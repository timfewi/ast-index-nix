//! Incremental indexing: walk a root, parse changed files, resolve references.
//!
//! The scan uses `size` + `mtime_ns` as a cheap prefilter and a content hash as
//! the correctness guarantee, so a file whose mtime moved but whose bytes did
//! not is not re-parsed. Watchers are intentionally not part of the core:
//! correctness must not depend on inotify, which is non-recursive, loses events
//! on overflow and does not see network filesystems.

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::time::{Instant, UNIX_EPOCH};

use ignore::WalkBuilder;
use serde::Serialize;

use crate::error::{Error, Result};
use crate::lang;
use crate::model::Confidence;
use crate::parse;
use crate::resolve::{self, ResolutionStats};
use crate::store::{RefInsert, Store, SymbolInsert, now_unix};

/// Options for one indexing pass.
#[derive(Debug, Clone)]
pub struct IndexOptions {
    /// Re-parse every file even when size and mtime match.
    pub force: bool,
    /// Files larger than this are skipped.
    pub max_file_bytes: u64,
    /// Hard cap on recorded failure messages.
    pub max_failures: usize,
}

impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            force: false,
            max_file_bytes: 2 * 1024 * 1024,
            max_failures: 16,
        }
    }
}

/// Summary of one indexing pass.
#[derive(Debug, Clone, Serialize)]
pub struct IndexStats {
    pub files_seen: usize,
    pub files_indexed: usize,
    pub files_unchanged: usize,
    pub files_removed: usize,
    pub files_skipped: usize,
    pub files_failed: usize,
    pub symbols: usize,
    pub references: usize,
    pub resolution: ResolutionStats,
    pub duration_ms: u128,
    pub failures: Vec<String>,
}

struct Candidate {
    relative: String,
    absolute: std::path::PathBuf,
    lang: lang::LangSpec,
    size: i64,
    mtime_ns: i64,
}

/// Index `root` into `store`. Paths are stored relative to `root`.
pub fn index(root: &Path, store: &Store, options: &IndexOptions) -> Result<IndexStats> {
    let started = Instant::now();
    let root = root
        .canonicalize()
        .map_err(|error| Error::io(root.to_path_buf(), error))?;
    let root_text = root.to_string_lossy().to_string();

    let mut stats = IndexStats {
        files_seen: 0,
        files_indexed: 0,
        files_unchanged: 0,
        files_removed: 0,
        files_skipped: 0,
        files_failed: 0,
        symbols: 0,
        references: 0,
        resolution: ResolutionStats::default(),
        duration_ms: 0,
        failures: Vec::new(),
    };

    let candidates = collect_candidates(&root, options, &mut stats);
    let existing: HashMap<String, _> = store.file_states()?.into_iter().collect();

    let mut present: HashSet<String> = HashSet::with_capacity(candidates.len());
    for candidate in &candidates {
        present.insert(candidate.relative.clone());
    }

    store.transaction(|| {
        store.set_meta("root", &root_text)?;
        store.set_meta("schema_version", &crate::store::SCHEMA_VERSION.to_string())?;

        for path in existing.keys() {
            if !present.contains(path) {
                store.remove_file(path)?;
                stats.files_removed += 1;
            }
        }

        for candidate in &candidates {
            let previous = existing.get(&candidate.relative);
            let unchanged = !options.force
                && previous
                    .map(|state| {
                        state.size == candidate.size && state.mtime_ns == candidate.mtime_ns
                    })
                    .unwrap_or(false);
            if unchanged {
                stats.files_unchanged += 1;
                continue;
            }

            let bytes = match std::fs::read(&candidate.absolute) {
                Ok(bytes) => bytes,
                Err(error) => {
                    stats.files_failed += 1;
                    record_failure(
                        &mut stats,
                        options,
                        format!("{}: read failed: {error}", candidate.relative),
                    );
                    continue;
                }
            };
            // Hash the raw bytes; only the parser sees a lossy UTF-8 view, so a
            // stray non-UTF-8 byte in a comment does not drop the whole file.
            let hash = blake3::hash(&bytes).to_hex().to_string();
            let source = String::from_utf8_lossy(&bytes);

            if let Some(previous) = previous
                && previous.hash == hash
            {
                store.touch_file(&candidate.relative, candidate.size, candidate.mtime_ns)?;
                stats.files_unchanged += 1;
                continue;
            }

            match index_one(store, candidate, &source, &hash) {
                Ok((symbol_count, reference_count)) => {
                    stats.files_indexed += 1;
                    stats.symbols += symbol_count;
                    stats.references += reference_count;
                }
                Err(error) => {
                    stats.files_failed += 1;
                    record_failure(
                        &mut stats,
                        options,
                        format!("{}: {error}", candidate.relative),
                    );
                }
            }
        }

        stats.resolution = resolve::resolve_all(store)?;
        store.set_meta("last_indexed_at", &now_unix().to_string())?;
        Ok(())
    })?;

    stats.duration_ms = started.elapsed().as_millis();
    Ok(stats)
}

fn record_failure(stats: &mut IndexStats, options: &IndexOptions, message: String) {
    if stats.failures.len() < options.max_failures {
        stats.failures.push(message);
    }
}

fn collect_candidates(
    root: &Path,
    options: &IndexOptions,
    stats: &mut IndexStats,
) -> Vec<Candidate> {
    let walker = WalkBuilder::new(root)
        .hidden(true)
        .parents(false)
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != ".ast-index")
        .build();

    let mut candidates = Vec::new();
    for entry in walker.flatten() {
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let path = entry.path();
        let Some(spec) = lang::detect(path) else {
            continue;
        };
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let size = metadata.len();
        if size > options.max_file_bytes {
            stats.files_skipped += 1;
            continue;
        }
        let mtime_ns = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos() as i64)
            .unwrap_or(0);
        let relative = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        stats.files_seen += 1;
        candidates.push(Candidate {
            relative,
            absolute: path.to_path_buf(),
            lang: spec,
            size: size as i64,
            mtime_ns,
        });
    }
    candidates
}

fn index_one(
    store: &Store,
    candidate: &Candidate,
    source: &str,
    hash: &str,
) -> Result<(usize, usize)> {
    let parsed = parse::parse(source, candidate.lang)?;
    let file_id = store.upsert_file(
        &candidate.relative,
        candidate.lang.family(),
        candidate.size,
        candidate.mtime_ns,
        hash,
    )?;
    store.clear_file_contents(file_id)?;

    let inserts: Vec<SymbolInsert> = parsed
        .symbols
        .iter()
        .map(|symbol| SymbolInsert {
            name: symbol.name.clone(),
            kind: symbol.kind,
            start_line: symbol.start_line,
            end_line: symbol.end_line,
            parent: symbol.parent.clone(),
            qualified: symbol.qualified.clone(),
        })
        .collect();
    let ids = store.insert_symbols(file_id, &inserts)?;

    let mut by_name: HashMap<&str, i64> = HashMap::new();
    for (symbol, id) in parsed.symbols.iter().zip(ids.iter()) {
        by_name.entry(symbol.name.as_str()).or_insert(*id);
    }

    let refs: Vec<RefInsert> = parsed
        .refs
        .iter()
        .map(|reference| RefInsert {
            name: reference.name.clone(),
            path: reference.path.clone(),
            kind: reference.kind,
            line: reference.line,
            from_symbol_id: reference
                .from_symbol
                .as_deref()
                .and_then(|name| by_name.get(name).copied()),
            confidence: Confidence::None,
        })
        .collect();
    store.insert_refs(file_id, &refs)?;

    Ok((parsed.symbols.len(), parsed.refs.len()))
}
