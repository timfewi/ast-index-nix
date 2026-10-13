//! Shared query contracts for CLI and MCP. Cursors identify both the query and
//! the committed index generation; they never contain executable instructions.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

pub const ACTIONS: &[&str] = &[
    "status",
    "freshness",
    "search",
    "outline",
    "describe",
    "callers",
    "callees",
    "impact",
    "refs",
    "imports",
    "importers",
    "deps",
    "map",
    "changes",
    "diff",
    "semantic",
];

pub const MAX_PAGE: usize = 100;
pub const PAGE_BYTES: usize = 48 * 1024;
pub const MAX_OFFSET: usize = 20_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Query {
    pub action: String,
    pub query: Option<String>,
    pub file: Option<String>,
    pub subtree: Option<String>,
    pub language: Option<String>,
    pub kind: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub depth: Option<u32>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
    pub match_mode: Option<String>,
    pub reason: Option<String>,
    pub budget: Option<usize>,
    pub changes: Vec<Change>,
    pub baseline: Option<i64>,
    pub relation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub generation: i64,
    pub truncated: bool,
    pub next_cursor: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    generation: i64,
    offset: usize,
    query: String,
}

pub struct Window {
    pub generation: i64,
    pub offset: usize,
    pub limit: usize,
    fingerprint: String,
}

impl Query {
    pub fn validate(&self) -> Result<()> {
        if !ACTIONS.contains(&self.action.as_str()) {
            return Err(Error::Invalid("unsupported action".into()));
        }
        for value in [
            &self.query,
            &self.file,
            &self.subtree,
            &self.language,
            &self.kind,
            &self.reason,
            &self.cursor,
            &self.relation,
        ]
        .into_iter()
        .flatten()
        {
            if value.len() > 4096 {
                return Err(Error::Invalid("query argument exceeds 4096 bytes".into()));
            }
        }
        for path in [&self.file, &self.subtree].into_iter().flatten() {
            relative_path(path)?;
        }
        if self.baseline.is_some_and(|n| n < 1) {
            return Err(Error::Invalid(
                "baseline must be a positive generation".into(),
            ));
        }
        if self.column == Some(0) || (self.column.is_some() && self.line.is_none()) {
            return Err(Error::Invalid(
                "column requires a positive line and uses one-based UTF-8 byte offsets".into(),
            ));
        }
        if self.line == Some(0)
            || self.limit.is_some_and(|n| n == 0 || n > MAX_PAGE)
            || self.depth.is_some_and(|n| n == 0 || n > 10)
        {
            return Err(Error::Invalid(
                "line must be positive, limit 1..100, depth 1..10".into(),
            ));
        }
        if self
            .match_mode
            .as_deref()
            .is_some_and(|m| !matches!(m, "exact" | "prefix" | "contains"))
        {
            return Err(Error::Invalid(
                "match_mode must be exact, prefix or contains; all treat input literally".into(),
            ));
        }
        if self
            .language
            .as_deref()
            .is_some_and(|l| crate::lang::by_id(l).is_none())
        {
            return Err(Error::Invalid("unsupported language filter".into()));
        }
        if self.kind.as_deref().is_some_and(|k| {
            crate::model::SymbolKind::from_stored(k).is_none() && !matches!(k, "call" | "import")
        }) {
            return Err(Error::Invalid("unsupported kind filter".into()));
        }
        if self.action == "refs"
            && self
                .kind
                .as_deref()
                .is_some_and(|k| !matches!(k, "call" | "import"))
        {
            return Err(Error::Invalid("refs kind must be call or import".into()));
        }
        if matches!(self.action.as_str(), "semantic" | "diff" | "changes")
            && (self.language.is_some() || self.kind.is_some() || self.reason.is_some())
        {
            return Err(Error::Invalid(
                "language, kind and reason filters do not apply to this action".into(),
            ));
        }
        if self.action == "semantic" && self.subtree.is_some() {
            return Err(Error::Invalid(
                "semantic supports an exact file filter; subtree is unsupported".into(),
            ));
        }
        if self.action == "changes"
            && (self.file.is_some() || self.subtree.is_some() || self.query.is_some())
        {
            return Err(Error::Invalid(
                "changes uses explicit change ranges, not symbol/file filters".into(),
            ));
        }
        if self.changes.len() > 100 {
            return Err(Error::Invalid(
                "at most 100 change ranges are allowed".into(),
            ));
        }
        for change in &self.changes {
            relative_path(&change.file)?;
            if change.start_line == 0 || change.end_line < change.start_line {
                return Err(Error::Invalid("invalid changed line range".into()));
            }
        }
        Ok(())
    }

    pub fn window(&self, generation: i64) -> Result<Window> {
        self.validate()?;
        let mut canonical = self.clone();
        canonical.cursor = None;
        let fingerprint = blake3::hash(&serde_json::to_vec(&canonical)?)
            .to_hex()
            .to_string();
        let offset = if let Some(cursor) = &self.cursor {
            let cursor: Cursor = serde_json::from_str(cursor)
                .map_err(|_| Error::Invalid("malformed continuation cursor".into()))?;
            if cursor.generation != generation {
                return Err(Error::Invalid(
                    "stale cursor: index changed; restart the query".into(),
                ));
            }
            if cursor.query != fingerprint {
                return Err(Error::Invalid("cursor belongs to a different query".into()));
            }
            if cursor.offset > MAX_OFFSET {
                return Err(Error::Invalid(
                    "pagination work limit reached; narrow the query".into(),
                ));
            }
            cursor.offset
        } else {
            0
        };
        Ok(Window {
            generation,
            offset,
            limit: self.limit.unwrap_or(25),
            fingerprint,
        })
    }

    pub fn name(&self) -> Result<&str> {
        self.query
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::Invalid("query is required".into()))
    }
}

impl Window {
    pub fn page<T: Serialize>(&self, mut items: Vec<T>) -> Result<Page<T>> {
        let mut truncated = items.len() > self.limit;
        items.truncate(self.limit);
        while serde_json::to_vec(&items)?.len() > PAGE_BYTES {
            items.pop();
            truncated = true;
        }
        if truncated && items.is_empty() {
            return Err(Error::Invalid(
                "one result exceeds the output budget".into(),
            ));
        }
        let next_cursor = if truncated {
            Some(serde_json::to_string(&Cursor {
                generation: self.generation,
                offset: self.offset + items.len(),
                query: self.fingerprint.clone(),
            })?)
        } else {
            None
        };
        Ok(Page {
            items,
            generation: self.generation,
            truncated,
            next_cursor,
        })
    }
}

/// Normalize only harmless `.` components; reject absolute and parent paths.
pub fn relative_path(value: &str) -> Result<String> {
    use std::path::{Component, Path};
    if value.len() > 4096 || value.contains('\\') || value.contains('\0') {
        return Err(Error::Invalid("invalid relative path".into()));
    }
    let mut parts = Vec::new();
    for part in Path::new(value).components() {
        match part {
            Component::CurDir => {}
            Component::Normal(p) => parts.push(p.to_string_lossy()),
            _ => {
                return Err(Error::Invalid(
                    "path must stay relative to the indexed root".into(),
                ));
            }
        }
    }
    Ok(parts.join("/"))
}
