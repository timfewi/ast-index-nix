//! Conservative resolution from lexical bindings, literal module paths and
//! supported named imports. There is no directory/global bare-name fallback.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::model::Confidence;
use crate::store::{RefRow, Store, SymbolRow};
pub(crate) mod go;

/// Invalidate edges independently of extraction when resolution rules change.
pub fn fingerprint() -> String {
    static HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HASH.get_or_init(|| {
        let mut hash = blake3::Hasher::new();
        hash.update(include_bytes!("resolve.rs"));
        hash.update(include_bytes!("resolve/go.rs"));
        hash.update(include_bytes!("imports.rs"));
        hash.update(include_bytes!("index.rs"));
        hash.finalize().to_hex().to_string()
    })
    .clone()
}

/// Outcome of one resolution pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionStats {
    pub resolved: usize,
    pub ambiguous: usize,
    pub unmatched: usize,
    pub same_file: usize,
    pub same_directory: usize,
    pub same_project: usize,
}

/// Candidate maps are partitioned by indexed file or explicit project/module.
#[derive(Default)]
struct ScopeMaps {
    qualified_by_file: HashMap<(String, String), Vec<i64>>,
    rust_modules: HashMap<String, Vec<i64>>,
    exported: HashMap<(String, String), Vec<i64>>,
    imports: HashMap<(String, String), String>,
    files: HashMap<i64, String>,
    sql: HashMap<(String, String, String), Vec<i64>>,
}

impl ScopeMaps {
    fn build(symbols: &[SymbolRow]) -> Self {
        let mut maps = Self::default();
        for symbol in symbols {
            maps.files.insert(symbol.id, symbol.file.clone());
            if symbol.language == "sql" {
                let namespace = match symbol.kind {
                    crate::model::SymbolKind::Table | crate::model::SymbolKind::View => {
                        "dependency"
                    }
                    crate::model::SymbolKind::Function => "call",
                    _ => continue,
                };
                for scope in [
                    format!("file:{}", symbol.file),
                    format!("project:{}", symbol.project),
                ] {
                    maps.sql
                        .entry((scope, namespace.into(), symbol.qualified.clone()))
                        .or_default()
                        .push(symbol.id);
                }
            }
            maps.qualified_by_file
                .entry((symbol.file.clone(), symbol.qualified.clone()))
                .or_default()
                .push(symbol.id);
            if symbol.exported {
                maps.exported
                    .entry((symbol.file.clone(), symbol.name.clone()))
                    .or_default()
                    .push(symbol.id);
            }
            if symbol.language == "rust" {
                let module = rust_module(&symbol.file, &symbol.project);
                let qualified = if module.is_empty() {
                    symbol.qualified.clone()
                } else {
                    format!("{module}::{}", symbol.qualified)
                };
                maps.rust_modules
                    .entry(scoped_key("rust", &symbol.project, &qualified))
                    .or_default()
                    .push(symbol.id);
            }
        }
        maps
    }

    fn ids(&self, reference: &RefRow) -> Option<&Vec<i64>> {
        if reference.blocked.is_some() {
            return None;
        }
        if reference.language == "sql" {
            let path = reference.path.as_deref()?;
            let (scope, name) = if let Some(name) = path.strip_prefix("@sql-qualified:") {
                (format!("project:{}", reference.project), name)
            } else {
                (
                    format!("file:{}", reference.file),
                    path.strip_prefix("@sql-local:")?,
                )
            };
            return self
                .sql
                .get(&(scope, reference.kind.as_str().into(), name.into()));
        }
        if let Some((module, name)) = imported_name(reference) {
            let target = self.imports.get(&(reference.file.clone(), module))?;
            return self.exported.get(&(target.clone(), name));
        }
        if let Some(path) = rust_relative(reference) {
            return self
                .rust_modules
                .get(&scoped_key("rust", &reference.project, &path));
        }
        let path = reference.path.as_ref()?;
        self.qualified_by_file
            .get(&(reference.file.clone(), path.clone()))
    }

    fn candidates(&self, reference: &RefRow) -> Option<(i64, Confidence)> {
        let [id] = self.ids(reference)?.as_slice() else {
            return None;
        };
        let target = &self.files[id];
        let confidence = if target == &reference.file {
            Confidence::Exact
        } else if crate::store::directory_of(target) == reference.directory {
            Confidence::High
        } else {
            Confidence::Low
        };
        Some((*id, confidence))
    }

    fn reason<'a>(&self, reference: &'a RefRow) -> &'a str {
        if let Some(reason) = &reference.blocked {
            return reason;
        }
        if let Some(ids) = self.ids(reference) {
            if ids.len() > 1 {
                return "ambiguous";
            }
            if reference.language == "sql" {
                return "static_sql_object";
            }
            if imported_name(reference).is_some() {
                return "imported_symbol";
            }
            if rust_relative(reference).is_some() {
                return "static_module_path";
            }
            return "lexical_same_file";
        }
        if reference.language == "sql" {
            "unknown_sql_object"
        } else if imported_name(reference).is_some() {
            "unsupported_import"
        } else if rust_relative(reference).is_some() {
            "unknown"
        } else if reference.path.is_some() && reference.language != "nix" {
            "external"
        } else {
            "unsupported"
        }
    }
}

fn imported_name(reference: &RefRow) -> Option<(String, String)> {
    serde_json::from_str(reference.path.as_deref()?.strip_prefix("@import:")?).ok()
}

fn rust_module(file: &str, project: &str) -> String {
    let relative = if project.is_empty() {
        file
    } else {
        file.strip_prefix(project)
            .unwrap_or(file)
            .trim_start_matches('/')
    };
    let relative = relative.strip_prefix("src/").unwrap_or(relative);
    let path = std::path::Path::new(relative);
    let mut parts: Vec<_> = path
        .parent()
        .into_iter()
        .flat_map(|p| p.iter())
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    if let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        && !matches!(stem, "lib" | "main" | "mod")
    {
        parts.push(stem.into());
    }
    parts.join("::")
}

fn rust_relative(reference: &RefRow) -> Option<String> {
    if reference.language != "rust" {
        return None;
    }
    let path = reference.path.as_deref()?;
    if let Some(rest) = path.strip_prefix("crate::") {
        return Some(rest.into());
    }
    let (head, rest) = path.split_once("::")?;
    if !matches!(head, "self" | "super" | "local_module") {
        return None;
    }
    let mut module = rust_module(&reference.file, &reference.project);
    if head == "super" {
        module = module
            .rsplit_once("::")
            .map(|(p, _)| p.to_string())
            .unwrap_or_default();
    }
    Some(if module.is_empty() {
        rest.into()
    } else {
        format!("{module}::{rest}")
    })
}

fn scoped_key(language: &str, project: &str, name: &str) -> String {
    let family = if matches!(language, "javascript" | "typescript") {
        "ecmascript"
    } else {
        language
    };
    format!("{family}\0{project}\0{name}")
}

/// Resolve every call reference in the store.
pub fn resolve_all(store: &Store) -> Result<ResolutionStats> {
    let symbols = store.resolution_symbols()?;
    let references = store.resolution_refs()?;
    let go = go::GoResolver::new(store, &symbols)?;
    let mut maps = ScopeMaps::build(&symbols);
    maps.imports = store.conn.prepare("SELECT f.path, r.name, r.import_target FROM refs r JOIN files f ON f.id=r.file_id WHERE r.kind='import' AND r.import_target IS NOT NULL")?
        .query_map([], |r| Ok(((r.get(0)?,r.get(1)?),r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;

    let mut stats = ResolutionStats::default();
    for reference in &references {
        let (target, confidence, reason) = if reference.language == "go" {
            go.resolve(reference)
        } else {
            let (target, confidence) = maps
                .candidates(reference)
                .map_or((None, Confidence::None), |(id, confidence)| {
                    (Some(id), confidence)
                });
            (target, confidence, maps.reason(reference))
        };
        store.set_resolution(reference.id, target, confidence, reason)?;
        match target {
            Some(_) => {
                stats.resolved += 1;
                match confidence {
                    Confidence::Exact => stats.same_file += 1,
                    Confidence::High => stats.same_directory += 1,
                    Confidence::Low => stats.same_project += 1,
                    Confidence::None => {}
                }
            }
            None => {
                if reason == "ambiguous" {
                    stats.ambiguous += 1;
                } else {
                    stats.unmatched += 1;
                }
            }
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn symbol(id: i64, file: &str, qualified: &str) -> SymbolRow {
        SymbolRow {
            id,
            language: "rust".into(),
            project: String::new(),
            exported: false,
            kind: crate::model::SymbolKind::Function,
            file: file.into(),
            directory: crate::store::directory_of(file),
            name: qualified.rsplit("::").next().unwrap().into(),
            qualified: qualified.into(),
        }
    }
    fn reference(file: &str, path: &str) -> RefRow {
        RefRow {
            id: 9,
            language: "rust".into(),
            project: String::new(),
            file: file.into(),
            directory: crate::store::directory_of(file),
            name: path.rsplit("::").next().unwrap().into(),
            path: Some(path.into()),
            kind: crate::model::RefKind::Call,
            blocked: None,
        }
    }
    #[test]
    fn scoped_references_follow_literal_module_paths() {
        let maps = ScopeMaps::build(&[
            symbol(1, "src/engine.rs", "open"),
            symbol(2, "src/store.rs", "open"),
        ]);
        assert_eq!(
            maps.candidates(&reference("src/engine.rs", "crate::store::open")),
            Some((2, Confidence::High))
        );
        assert_eq!(
            maps.candidates(&reference("src/engine.rs", "Store::open")),
            None,
            "an external type is not a local module"
        );
    }
    #[test]
    fn crate_paths_never_fall_back_to_a_same_named_nested_function() {
        let maps = ScopeMaps::build(&[symbol(1, "src/lib.rs", "nested::helper")]);
        assert_eq!(
            maps.candidates(&reference("src/lib.rs", "crate::helper")),
            None
        );
        assert_eq!(
            maps.candidates(&reference("src/lib.rs", "crate::nested::helper")),
            Some((1, Confidence::Exact))
        );
    }
    #[test]
    fn ambiguous_names_stay_unresolved() {
        let maps =
            ScopeMaps::build(&[symbol(1, "lib.rs", "helper"), symbol(2, "lib.rs", "helper")]);
        let call = reference("lib.rs", "helper");
        assert_eq!(maps.candidates(&call), None);
        assert_eq!(maps.reason(&call), "ambiguous");
    }
    #[test]
    fn external_scoped_paths_do_not_fall_back_to_local_names() {
        let maps = ScopeMaps::build(&[symbol(1, "lib.rs", "Store::open")]);
        let call = reference("lib.rs", "Connection::open");
        assert_eq!(maps.candidates(&call), None);
        assert_eq!(maps.reason(&call), "external");
    }
}
