//! SQLite storage. One database file per indexed root holds files, definitions,
//! references and their resolution state. WAL is requested but not assumed:
//! network filesystems fall back to the rollback journal.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::{Confidence, LanguageCount, RefKind, Reference, Status, Symbol, SymbolKind};

/// Schema version stored in `meta`.
pub const SCHEMA_VERSION: i64 = 1;

/// Hash state of one file as recorded during indexing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileState {
    pub size: i64,
    pub mtime_ns: i64,
    pub hash: String,
    pub lang: String,
}

/// A stored reference row used by the resolver.
#[derive(Debug, Clone)]
pub struct RefRow {
    pub id: i64,
    pub file: String,
    pub directory: String,
    pub name: String,
    /// Full scoped path such as `Store::open`, when the language provides one.
    pub path: Option<String>,
    pub kind: RefKind,
}

/// A symbol row used by the resolver.
#[derive(Debug, Clone)]
pub struct SymbolRow {
    pub id: i64,
    pub file: String,
    pub directory: String,
    pub name: String,
    /// Lowercased qualified name such as `store::open`.
    pub qualified: String,
}

/// One incoming or outgoing call edge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallEdge {
    pub caller: Option<String>,
    pub callee: String,
    pub file: String,
    pub line: u32,
    pub confidence: Confidence,
}

/// Both edge directions for one symbol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallersAndCallees {
    pub callers: Vec<CallEdge>,
    pub callees: Vec<CallEdge>,
}

/// A symbol reachable from `impact`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpactRow {
    pub symbol: Symbol,
    pub depth: u32,
}

/// Handle to the index database.
pub struct Store {
    conn: Connection,
    path: PathBuf,
}

impl Store {
    /// Open (and create) the index at `path`.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
        }
        let conn = Connection::open(&path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let journal: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;")?;
        let store = Self { conn, path };
        store.init_schema()?;
        store.set_meta("journal_mode", &journal)?;
        Ok(store)
    }

    /// Path of the database file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS files (
                id INTEGER PRIMARY KEY,
                path TEXT NOT NULL UNIQUE,
                lang TEXT NOT NULL,
                size INTEGER NOT NULL,
                mtime_ns INTEGER NOT NULL,
                hash TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS symbols (
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                name_lower TEXT NOT NULL,
                qualified TEXT NOT NULL,
                qualified_lower TEXT NOT NULL,
                kind TEXT NOT NULL,
                start_line INTEGER NOT NULL,
                end_line INTEGER NOT NULL,
                parent TEXT
            );
            CREATE TABLE IF NOT EXISTS refs (
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                name_lower TEXT NOT NULL,
                path TEXT,
                kind TEXT NOT NULL,
                line INTEGER NOT NULL,
                from_symbol_id INTEGER REFERENCES symbols(id) ON DELETE SET NULL,
                resolved_symbol_id INTEGER REFERENCES symbols(id) ON DELETE SET NULL,
                confidence TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name_lower);
            CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file_id);
            CREATE INDEX IF NOT EXISTS idx_refs_name ON refs(name_lower);
            CREATE INDEX IF NOT EXISTS idx_refs_from ON refs(from_symbol_id);
            CREATE INDEX IF NOT EXISTS idx_refs_resolved ON refs(resolved_symbol_id);
            "#,
        )?;
        self.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
        Ok(())
    }

    /// Read a metadata value.
    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        let value = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get::<_, String>(0)
            })
            .optional()?;
        Ok(value)
    }

    /// Write a metadata value.
    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Whether an index has been written for this database.
    pub fn is_indexed(&self) -> Result<bool> {
        Ok(self.meta("root")?.is_some())
    }

    /// Run `action` inside one write transaction.
    pub fn transaction<T>(&self, action: impl FnOnce() -> Result<T>) -> Result<T> {
        let tx = self.conn.unchecked_transaction()?;
        let value = action()?;
        tx.commit()?;
        Ok(value)
    }

    /// Recorded hash state of a file, if present.
    pub fn file_state(&self, path: &str) -> Result<Option<FileState>> {
        let state = self
            .conn
            .query_row(
                "SELECT size, mtime_ns, hash, lang FROM files WHERE path = ?1",
                [path],
                |row| {
                    Ok(FileState {
                        size: row.get(0)?,
                        mtime_ns: row.get(1)?,
                        hash: row.get(2)?,
                        lang: row.get(3)?,
                    })
                },
            )
            .optional()?;
        Ok(state)
    }

    /// All indexed files as `(path, state)`.
    pub fn file_states(&self) -> Result<Vec<(String, FileState)>> {
        let mut statement = self
            .conn
            .prepare("SELECT path, size, mtime_ns, hash, lang FROM files")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                FileState {
                    size: row.get(1)?,
                    mtime_ns: row.get(2)?,
                    hash: row.get(3)?,
                    lang: row.get(4)?,
                },
            ))
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Update the hash state of an unchanged file (mtime-only drift).
    pub fn touch_file(&self, path: &str, size: i64, mtime_ns: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET size = ?2, mtime_ns = ?3 WHERE path = ?1",
            params![path, size, mtime_ns],
        )?;
        Ok(())
    }

    /// Insert or update a file row and return its id.
    pub fn upsert_file(
        &self,
        path: &str,
        lang: &str,
        size: i64,
        mtime_ns: i64,
        hash: &str,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO files(path, lang, size, mtime_ns, hash) VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path) DO UPDATE SET
                lang = excluded.lang,
                size = excluded.size,
                mtime_ns = excluded.mtime_ns,
                hash = excluded.hash",
            params![path, lang, size, mtime_ns, hash],
        )?;
        let id: i64 =
            self.conn
                .query_row("SELECT id FROM files WHERE path = ?1", [path], |row| {
                    row.get(0)
                })?;
        Ok(id)
    }

    /// Remove a file and, by cascade, its symbols and references.
    pub fn remove_file(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM files WHERE path = ?1", [path])?;
        Ok(())
    }

    /// Clear all symbols and references for a file (before re-inserting).
    pub fn clear_file_contents(&self, file_id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM symbols WHERE file_id = ?1", [file_id])?;
        self.conn
            .execute("DELETE FROM refs WHERE file_id = ?1", [file_id])?;
        Ok(())
    }

    /// Insert definitions for a file; returns ids in insertion order.
    pub fn insert_symbols(&self, file_id: i64, symbols: &[SymbolInsert]) -> Result<Vec<i64>> {
        let mut statement = self.conn.prepare(
            "INSERT INTO symbols(
                file_id, name, name_lower, qualified, qualified_lower, kind,
                start_line, end_line, parent
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        let mut ids = Vec::with_capacity(symbols.len());
        for symbol in symbols {
            statement.execute(params![
                file_id,
                symbol.name,
                symbol.name.to_lowercase(),
                symbol.qualified,
                symbol.qualified.to_lowercase(),
                symbol.kind.as_str(),
                symbol.start_line,
                symbol.end_line,
                symbol.parent,
            ])?;
            ids.push(self.conn.last_insert_rowid());
        }
        Ok(ids)
    }

    /// Insert references for a file.
    pub fn insert_refs(&self, file_id: i64, refs: &[RefInsert]) -> Result<()> {
        let mut statement = self.conn.prepare(
            "INSERT INTO refs(
                file_id, name, name_lower, path, kind, line, from_symbol_id,
                resolved_symbol_id, confidence
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8)",
        )?;
        for reference in refs {
            statement.execute(params![
                file_id,
                reference.name,
                reference.name.to_lowercase(),
                reference.path,
                reference.kind.as_str(),
                reference.line,
                reference.from_symbol_id,
                reference.confidence.as_str(),
            ])?;
        }
        Ok(())
    }

    /// Symbols of a file, ordered by position.
    pub fn symbols_in_file(&self, path: &str) -> Result<Vec<Symbol>> {
        let mut statement = self.conn.prepare(
            "SELECT s.id, f.path, s.name, s.kind, s.start_line, s.end_line, s.parent, s.qualified
             FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE f.path = ?1
             ORDER BY s.start_line, s.id",
        )?;
        let rows = statement.query_map([path], symbol_from_row)?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Search definitions by name or qualified name.
    pub fn find_symbols(&self, query: &str, limit: usize) -> Result<Vec<Symbol>> {
        let lower = query.to_lowercase();
        let prefix = format!("{lower}%");
        let contains = format!("%{lower}%");
        let mut statement = self.conn.prepare(
            "SELECT s.id, f.path, s.name, s.kind, s.start_line, s.end_line, s.parent, s.qualified
             FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE s.name_lower = ?1 OR s.qualified_lower = ?1
                OR s.name_lower LIKE ?2 OR s.qualified_lower LIKE ?3
             ORDER BY
                CASE
                    WHEN s.name_lower = ?1 THEN 0
                    WHEN s.qualified_lower = ?1 THEN 1
                    WHEN s.name_lower LIKE ?2 THEN 2
                    ELSE 3
                END,
                s.name,
                f.path,
                s.start_line
             LIMIT ?4",
        )?;
        let rows = statement.query_map(
            params![lower, prefix, contains, limit as i64],
            symbol_from_row,
        )?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    /// Load one symbol by id.
    pub fn symbol_by_id(&self, id: i64) -> Result<Option<Symbol>> {
        let symbol = self
            .conn
            .query_row(
                "SELECT s.id, f.path, s.name, s.kind, s.start_line, s.end_line, s.parent, s.qualified
                 FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.id = ?1",
                [id],
                symbol_from_row,
            )
            .optional()?;
        Ok(symbol)
    }

    /// Incoming call edges for a symbol.
    pub fn callers(&self, symbol_id: i64) -> Result<Vec<CallEdge>> {
        let target = match self.symbol_by_id(symbol_id)? {
            Some(symbol) => symbol,
            None => return Ok(Vec::new()),
        };
        let mut statement = self.conn.prepare(
            "SELECT caller.qualified, f.path, r.line, r.confidence
             FROM refs r
             JOIN files f ON f.id = r.file_id
             LEFT JOIN symbols caller ON caller.id = r.from_symbol_id
             WHERE r.resolved_symbol_id = ?1 AND r.kind = 'call'
             ORDER BY f.path, r.line",
        )?;
        let rows = statement.query_map([symbol_id], |row| {
            Ok(CallEdge {
                caller: row.get::<_, Option<String>>(0)?,
                callee: target.qualified.clone(),
                file: row.get(1)?,
                line: row.get::<_, i64>(2)? as u32,
                confidence: parse_confidence(row.get::<_, String>(3)?),
            })
        })?;
        collect(rows)
    }

    /// Outgoing call edges from a symbol.
    pub fn callees(&self, symbol_id: i64) -> Result<Vec<CallEdge>> {
        let source = match self.symbol_by_id(symbol_id)? {
            Some(symbol) => symbol,
            None => return Ok(Vec::new()),
        };
        let mut statement = self.conn.prepare(
            "SELECT callee.qualified, f.path, r.line, r.confidence
             FROM refs r
             JOIN files f ON f.id = r.file_id
             JOIN symbols callee ON callee.id = r.resolved_symbol_id
             WHERE r.from_symbol_id = ?1 AND r.kind = 'call'
             ORDER BY f.path, r.line",
        )?;
        let rows = statement.query_map([symbol_id], |row| {
            Ok(CallEdge {
                caller: Some(source.qualified.clone()),
                callee: row.get(0)?,
                file: row.get(1)?,
                line: row.get::<_, i64>(2)? as u32,
                confidence: parse_confidence(row.get::<_, String>(3)?),
            })
        })?;
        collect(rows)
    }

    /// Transitive callers of a symbol up to `depth` hops.
    pub fn impact(&self, symbol_id: i64, depth: u32) -> Result<Vec<ImpactRow>> {
        let mut statement = self.conn.prepare(
            "WITH RECURSIVE callers(id, depth) AS (
                SELECT ?1, 0
                UNION
                SELECT r.from_symbol_id, callers.depth + 1
                FROM refs r JOIN callers ON r.resolved_symbol_id = callers.id
                WHERE r.from_symbol_id IS NOT NULL AND callers.depth < ?2
             )
             SELECT s.id, f.path, s.name, s.kind, s.start_line, s.end_line, s.parent,
                    s.qualified, callers.depth
             FROM callers
             JOIN symbols s ON s.id = callers.id
             JOIN files f ON f.id = s.file_id
             WHERE callers.depth > 0
             ORDER BY callers.depth, f.path, s.start_line",
        )?;
        let rows = statement.query_map(params![symbol_id, depth as i64], |row| {
            Ok(ImpactRow {
                symbol: symbol_from_row(row)?,
                depth: row.get::<_, i64>(8)? as u32,
            })
        })?;
        collect(rows)
    }

    /// Candidate symbols for reference resolution.
    pub fn resolution_symbols(&self) -> Result<Vec<SymbolRow>> {
        let mut statement = self.conn.prepare(
            "SELECT s.id, f.path, s.name, s.qualified FROM symbols s JOIN files f ON f.id = s.file_id",
        )?;
        let rows = statement.query_map([], |row| {
            let file: String = row.get(1)?;
            Ok(SymbolRow {
                id: row.get(0)?,
                directory: directory_of(&file),
                file,
                name: row.get::<_, String>(2)?.to_lowercase(),
                qualified: row.get::<_, String>(3)?.to_lowercase(),
            })
        })?;
        collect(rows)
    }

    /// Call and import references that still need resolution.
    pub fn resolution_refs(&self) -> Result<Vec<RefRow>> {
        let mut statement = self.conn.prepare(
            "SELECT r.id, f.path, r.name, r.kind, r.path
             FROM refs r JOIN files f ON f.id = r.file_id
             WHERE r.kind = 'call' AND r.resolved_symbol_id IS NULL",
        )?;
        let rows = statement.query_map([], |row| {
            let file: String = row.get(1)?;
            Ok(RefRow {
                id: row.get(0)?,
                directory: directory_of(&file),
                file,
                name: row.get::<_, String>(2)?.to_lowercase(),
                kind: RefKind::from_stored(&row.get::<_, String>(3)?).unwrap_or(RefKind::Call),
                path: row
                    .get::<_, Option<String>>(4)?
                    .map(|value| value.to_lowercase()),
            })
        })?;
        collect(rows)
    }

    /// Store a resolution result for one reference.
    pub fn set_resolution(
        &self,
        ref_id: i64,
        symbol_id: Option<i64>,
        confidence: Confidence,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE refs SET resolved_symbol_id = ?2, confidence = ?3 WHERE id = ?1",
            params![ref_id, symbol_id, confidence.as_str()],
        )?;
        Ok(())
    }

    /// Aggregate index status.
    pub fn status(&self) -> Result<Status> {
        let indexed = self.is_indexed()?;
        let files: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))?;
        let symbols: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))?;
        let references: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM refs", [], |row| row.get(0))?;
        let imports: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM refs WHERE kind = 'import'",
            [],
            |row| row.get(0),
        )?;
        let resolved_references: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM refs WHERE kind = 'call' AND resolved_symbol_id IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        let mut statement = self.conn.prepare(
            "SELECT f.lang, COUNT(DISTINCT f.id), COUNT(s.id)
             FROM files f LEFT JOIN symbols s ON s.file_id = f.id
             GROUP BY f.lang ORDER BY f.lang",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(LanguageCount {
                language: row.get(0)?,
                files: row.get(1)?,
                symbols: row.get(2)?,
            })
        })?;
        Ok(Status {
            root: self.meta("root")?.unwrap_or_default(),
            indexed,
            last_indexed_at: self
                .meta("last_indexed_at")?
                .and_then(|value| value.parse::<i64>().ok()),
            files,
            symbols,
            references,
            imports,
            resolved_references,
            languages: collect(rows)?,
        })
    }

    /// Stored references of a file, for debugging and JSON output.
    pub fn references_in_file(&self, path: &str) -> Result<Vec<Reference>> {
        let mut statement = self.conn.prepare(
            "SELECT r.id, f.path, r.name, r.kind, r.line, s.name, r.resolved_symbol_id, r.confidence, r.path
             FROM refs r
             JOIN files f ON f.id = r.file_id
             LEFT JOIN symbols s ON s.id = r.from_symbol_id
             WHERE f.path = ?1
             ORDER BY r.line, r.id",
        )?;
        let rows = statement.query_map([path], |row| {
            Ok(Reference {
                id: row.get(0)?,
                file: row.get(1)?,
                name: row.get(2)?,
                kind: RefKind::from_stored(&row.get::<_, String>(3)?).unwrap_or(RefKind::Call),
                line: row.get::<_, i64>(4)? as u32,
                from_symbol: row.get(5)?,
                resolved: row.get(6)?,
                confidence: parse_confidence(row.get::<_, String>(7)?),
                path: row.get(8)?,
            })
        })?;
        collect(rows)
    }

    /// Dump the raw schema version (used by tests and `status --json`).
    pub fn schema_version(&self) -> Result<Option<i64>> {
        Ok(self
            .meta("schema_version")?
            .and_then(|value| value.parse::<i64>().ok()))
    }
}

/// A definition about to be inserted.
#[derive(Debug, Clone)]
pub struct SymbolInsert {
    pub name: String,
    pub kind: SymbolKind,
    pub start_line: u32,
    pub end_line: u32,
    pub parent: Option<String>,
    pub qualified: String,
}

/// A reference about to be inserted.
#[derive(Debug, Clone)]
pub struct RefInsert {
    pub name: String,
    pub path: Option<String>,
    pub kind: RefKind,
    pub line: u32,
    pub from_symbol_id: Option<i64>,
    pub confidence: Confidence,
}

fn symbol_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Symbol> {
    let kind: String = row.get(3)?;
    Ok(Symbol {
        id: row.get(0)?,
        file: row.get(1)?,
        name: row.get(2)?,
        kind: SymbolKind::from_stored(&kind).unwrap_or(SymbolKind::Function),
        start_line: row.get::<_, i64>(4)? as u32,
        end_line: row.get::<_, i64>(5)? as u32,
        parent: row.get(6)?,
        qualified: row.get(7)?,
    })
}

fn parse_confidence(value: String) -> Confidence {
    Confidence::from_stored(&value).unwrap_or(Confidence::None)
}

fn collect<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>> {
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

fn directory_of(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((directory, _)) => directory.to_string(),
        None => String::new(),
    }
}

/// Current wall-clock time in seconds since the Unix epoch.
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}
