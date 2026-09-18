//! Name-based reference resolution.
//!
//! Resolution is deliberately precision-first: a reference is only linked when
//! the narrowest scope (same file, then same directory, then whole index) holds
//! exactly one candidate. Ambiguous names stay unresolved and are reported as
//! plain references instead of guessed edges. This mirrors documented limits of
//! heuristic indexers and avoids inventing edges for dynamic dispatch, macros or
//! same-named methods.
//!
//! Scoped calls such as `Store::open` carry a full path. Those are tried against
//! qualified names first, which avoids linking `Store::open` to a same-file
//! `Engine::open`; the bare name is only a fallback.

use std::collections::HashMap;

use serde::Serialize;

use crate::error::Result;
use crate::model::Confidence;
use crate::store::{RefRow, Store, SymbolRow};

/// Outcome of one resolution pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ResolutionStats {
    pub resolved: usize,
    pub ambiguous: usize,
    pub unmatched: usize,
    pub exact: usize,
    pub high: usize,
    pub low: usize,
}

/// Scope ladders for bare names and qualified names.
#[derive(Default)]
struct ScopeMaps {
    name_by_file: HashMap<(String, String), Vec<i64>>,
    name_by_dir: HashMap<(String, String), Vec<i64>>,
    name_global: HashMap<String, Vec<i64>>,
    qualified_by_file: HashMap<(String, String), Vec<i64>>,
    qualified_by_dir: HashMap<(String, String), Vec<i64>>,
    qualified_global: HashMap<String, Vec<i64>>,
}

impl ScopeMaps {
    fn build(symbols: &[SymbolRow]) -> Self {
        let mut maps = Self::default();
        for symbol in symbols {
            maps.name_by_file
                .entry((symbol.file.clone(), symbol.name.clone()))
                .or_default()
                .push(symbol.id);
            maps.name_by_dir
                .entry((symbol.directory.clone(), symbol.name.clone()))
                .or_default()
                .push(symbol.id);
            maps.name_global
                .entry(symbol.name.clone())
                .or_default()
                .push(symbol.id);
            maps.qualified_by_file
                .entry((symbol.file.clone(), symbol.qualified.clone()))
                .or_default()
                .push(symbol.id);
            maps.qualified_by_dir
                .entry((symbol.directory.clone(), symbol.qualified.clone()))
                .or_default()
                .push(symbol.id);
            maps.qualified_global
                .entry(symbol.qualified.clone())
                .or_default()
                .push(symbol.id);
        }
        maps
    }

    /// Qualified-first for scoped references. The bare-name fallback is only
    /// applied for relative paths (`crate::`, `self::`, `super::`), so external
    /// paths such as `Connection::open` are never linked to a same-named local
    /// definition by accident.
    fn candidates(&self, reference: &RefRow) -> Option<(i64, Confidence)> {
        if let Some(path) = &reference.path {
            let qualified = pick(
                self.qualified_by_file
                    .get(&(reference.file.clone(), path.clone())),
                self.qualified_by_dir
                    .get(&(reference.directory.clone(), path.clone())),
                self.qualified_global.get(path),
            );
            if qualified.is_some() {
                return qualified;
            }
            let head = path.split("::").next().unwrap_or_default();
            if matches!(head, "crate" | "self" | "super") {
                return self.bare(reference);
            }
            return None;
        }
        self.bare(reference)
    }

    /// The same-name scope ladder.
    fn bare(&self, reference: &RefRow) -> Option<(i64, Confidence)> {
        pick(
            self.name_by_file
                .get(&(reference.file.clone(), reference.name.clone())),
            self.name_by_dir
                .get(&(reference.directory.clone(), reference.name.clone())),
            self.name_global.get(&reference.name),
        )
    }

    /// Whether any candidate existed at all, to separate ambiguous from unknown.
    fn known(&self, reference: &RefRow) -> bool {
        let qualified_known = reference
            .path
            .as_ref()
            .map(|path| {
                self.qualified_global.contains_key(path)
                    || self
                        .qualified_by_file
                        .contains_key(&(reference.file.clone(), path.clone()))
                    || self
                        .qualified_by_dir
                        .contains_key(&(reference.directory.clone(), path.clone()))
            })
            .unwrap_or(false);
        if qualified_known {
            return true;
        }
        if let Some(path) = &reference.path {
            let head = path.split("::").next().unwrap_or_default();
            if !matches!(head, "crate" | "self" | "super") {
                return false;
            }
        }
        self.name_global.contains_key(&reference.name)
            || self
                .name_by_file
                .contains_key(&(reference.file.clone(), reference.name.clone()))
            || self
                .name_by_dir
                .contains_key(&(reference.directory.clone(), reference.name.clone()))
    }
}

/// Resolve every unresolved call reference in the store.
pub fn resolve_all(store: &Store) -> Result<ResolutionStats> {
    let symbols = store.resolution_symbols()?;
    let references = store.resolution_refs()?;
    let maps = ScopeMaps::build(&symbols);

    let mut stats = ResolutionStats::default();
    for reference in &references {
        match maps.candidates(reference) {
            Some((symbol_id, confidence)) => {
                store.set_resolution(reference.id, Some(symbol_id), confidence)?;
                stats.resolved += 1;
                match confidence {
                    Confidence::Exact => stats.exact += 1,
                    Confidence::High => stats.high += 1,
                    Confidence::Low => stats.low += 1,
                    Confidence::None => {}
                }
            }
            None => {
                store.set_resolution(reference.id, None, Confidence::None)?;
                if maps.known(reference) {
                    stats.ambiguous += 1;
                } else {
                    stats.unmatched += 1;
                }
            }
        }
    }
    Ok(stats)
}

/// Choose a target from the narrowest scope that has any candidate.
pub fn pick(
    same_file: Option<&Vec<i64>>,
    same_directory: Option<&Vec<i64>>,
    global: Option<&Vec<i64>>,
) -> Option<(i64, Confidence)> {
    if let Some(candidates) = same_file {
        return single(candidates, Confidence::Exact);
    }
    if let Some(candidates) = same_directory {
        return single(candidates, Confidence::High);
    }
    if let Some(candidates) = global {
        return single(candidates, Confidence::Low);
    }
    None
}

fn single(candidates: &[i64], confidence: Confidence) -> Option<(i64, Confidence)> {
    match candidates {
        [only] => Some((*only, confidence)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_file_scope_over_directory_and_global() {
        let file = vec![1];
        let directory = vec![2];
        let global = vec![3];
        assert_eq!(
            pick(Some(&file), Some(&directory), Some(&global)),
            Some((1, Confidence::Exact))
        );
    }

    #[test]
    fn falls_back_when_narrower_scope_is_empty() {
        let directory = vec![2];
        let global = vec![3];
        assert_eq!(
            pick(None, Some(&directory), Some(&global)),
            Some((2, Confidence::High))
        );
        assert_eq!(pick(None, None, Some(&global)), Some((3, Confidence::Low)));
    }

    #[test]
    fn ambiguous_names_stay_unresolved() {
        let file = vec![1, 2];
        let global = vec![1, 2];
        assert_eq!(pick(Some(&file), None, Some(&global)), None);
    }

    #[test]
    fn unknown_names_stay_unresolved() {
        assert_eq!(pick(None, None, None), None);
    }

    #[test]
    fn scoped_references_prefer_qualified_names() {
        let maps = ScopeMaps::build(&[
            SymbolRow {
                id: 1,
                file: "src/engine.rs".to_string(),
                directory: "src".to_string(),
                name: "open".to_string(),
                qualified: "engine::open".to_string(),
            },
            SymbolRow {
                id: 2,
                file: "src/store.rs".to_string(),
                directory: "src".to_string(),
                name: "open".to_string(),
                qualified: "store::open".to_string(),
            },
        ]);
        let reference = RefRow {
            id: 9,
            file: "src/engine.rs".to_string(),
            directory: "src".to_string(),
            name: "open".to_string(),
            path: Some("store::open".to_string()),
            kind: crate::model::RefKind::Call,
        };
        assert_eq!(
            maps.candidates(&reference),
            Some((2, Confidence::High)),
            "the scoped path must win over the same-file bare name"
        );
    }

    #[test]
    fn scoped_references_fall_back_to_the_bare_name() {
        let maps = ScopeMaps::build(&[SymbolRow {
            id: 1,
            file: "src/lib.rs".to_string(),
            directory: "src".to_string(),
            name: "run".to_string(),
            qualified: "run".to_string(),
        }]);
        // `crate::run()` from another file has no qualified match.
        let reference = RefRow {
            id: 9,
            file: "src/other.rs".to_string(),
            directory: "src".to_string(),
            name: "run".to_string(),
            path: Some("crate::run".to_string()),
            kind: crate::model::RefKind::Call,
        };
        assert_eq!(maps.candidates(&reference), Some((1, Confidence::High)));
    }

    #[test]
    fn external_scoped_paths_do_not_fall_back_to_local_names() {
        let maps = ScopeMaps::build(&[SymbolRow {
            id: 1,
            file: "src/store.rs".to_string(),
            directory: "src".to_string(),
            name: "open".to_string(),
            qualified: "store::open".to_string(),
        }]);
        // `Connection::open` is an external type and must not link to `Store::open`.
        let reference = RefRow {
            id: 9,
            file: "src/store.rs".to_string(),
            directory: "src".to_string(),
            name: "open".to_string(),
            path: Some("connection::open".to_string()),
            kind: crate::model::RefKind::Call,
        };
        assert_eq!(maps.candidates(&reference), None);
        assert!(!maps.known(&reference));
    }
}
