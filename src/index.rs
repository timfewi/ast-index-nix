//! Incremental indexing: walk a root, parse changed files, resolve references.
//!
//! The scan hashes every candidate to detect changes even when size and mtime
//! are unchanged. Files whose bytes did not change are not re-parsed. Watchers
//! are intentionally not part of the core:
//! correctness must not depend on inotify, which is non-recursive, loses events
//! on overflow and does not see network filesystems.

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::lang;
use crate::model::Confidence;
use crate::parse;
use crate::resolve::{self, ResolutionStats};
use crate::store::{RefInsert, Store, SymbolInsert, now_unix};

/// Options for one indexing pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexOptions {
    /// Re-parse every file even when its content hash matches.
    pub force: bool,
    /// Files larger than this are skipped.
    pub max_file_bytes: u64,
    /// Hard cap on recorded failure messages.
    pub max_failures: usize,
    /// gitignore-style patterns excluded from the walk, relative to the root.
    /// A pattern without a leading slash matches at any depth.
    pub exclude: Vec<String>,
    pub max_files: usize,
    pub max_total_bytes: u64,
}

impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            force: false,
            max_file_bytes: 2 * 1024 * 1024,
            max_failures: 16,
            exclude: Vec::new(),
            max_files: 100_000,
            max_total_bytes: 256 * 1024 * 1024,
        }
    }
}

/// Summary of one indexing pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub scan_complete: bool,
    pub parse_error_files: usize,
    #[serde(default)]
    pub resolution_recomputed: bool,
}

pub(crate) struct Candidate {
    relative: String,
    absolute: std::path::PathBuf,
    lang: lang::LangSpec,
    size: i64,
    mtime_ns: i64,
}

/// Index `root` into `store`. Paths are stored relative to `root`.
pub fn index(root: &Path, store: &Store, options: &IndexOptions) -> Result<IndexStats> {
    let _writer = store.write_lock()?;
    let started = Instant::now();
    let root = root
        .canonicalize()
        .map_err(|error| Error::io(root.to_path_buf(), error))?;
    let root_text = root.to_string_lossy().to_string();
    store.require_root(&root)?;

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
        scan_complete: true,
        parse_error_files: 0,
        resolution_recomputed: false,
    };

    stats.resolution_recomputed =
        store.meta("resolution_version")?.as_deref() != Some(&resolve::fingerprint());
    let (candidates, mut metadata_bytes) = collect_candidates(&root, store, options, &mut stats)?;
    let go_modules = go_modules(&root, &candidates, options, &mut stats, &mut metadata_bytes)?;
    stats.resolution_recomputed |= store.meta("go_modules")?.as_deref() != Some(&go_modules);
    let existing: HashMap<String, _> = store.file_states()?.into_iter().collect();

    let mut present: HashSet<String> = HashSet::with_capacity(candidates.len());
    for candidate in &candidates {
        present.insert(candidate.relative.clone());
    }

    let mut parsers = parse::ParserCache::default();
    store.transaction(|| {
        store.set_meta("root", &root_text)?;
        store.set_meta("schema_version", &crate::store::SCHEMA_VERSION.to_string())?;
        store.set_meta("go_modules", &go_modules)?;

        for path in existing.keys() {
            if stats.scan_complete && !present.contains(path) {
                store.remove_file(path)?;
                stats.files_removed += 1;
            }
        }

        let mut bytes_read = metadata_bytes;
        for candidate in &candidates {
            let previous = existing.get(&candidate.relative);
            let bytes = match read_budgeted(
                &root, &candidate.absolute, options.max_file_bytes,
                options.max_total_bytes, &mut bytes_read,
            ) {
                Ok(bytes) => bytes,
                Err(error) => {
                    stats.files_failed += 1;
                    stats.scan_complete = false;
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

            if let Some(previous) = previous
                && !options.force
                && previous.hash == hash
                && previous.fingerprint == candidate.lang.fingerprint()
            {
                if previous.size != candidate.size || previous.mtime_ns != candidate.mtime_ns {
                    store.touch_file(&candidate.relative, candidate.size, candidate.mtime_ns)?;
                }
                let project=project_boundary(&root, &candidate.absolute);
                if previous.project!=project {
                    store.conn.execute("UPDATE files SET project=?2 WHERE path=?1",rusqlite::params![candidate.relative,project])?;
                    stats.resolution_recomputed=true;
                }
                stats.parse_error_files += usize::from(previous.parse_errors);
                stats.files_unchanged += 1;
                continue;
            }

            let source = String::from_utf8_lossy(&bytes);
            match index_one(store, candidate, &source, &hash, &root, &mut parsers) {
                Ok((symbol_count, reference_count, parse_errors)) => {
                    stats.files_indexed += 1;
                    stats.parse_error_files += usize::from(parse_errors);
                    stats.symbols += symbol_count;
                    stats.references += reference_count;
                }
                Err(Error::Database(error)) => return Err(Error::Database(error)),
                Err(error) => {
                    stats.files_failed += 1;
                    stats.scan_complete = false;
                    record_failure(
                        &mut stats,
                        options,
                        format!("{}: {error}", candidate.relative),
                    );
                }
            }
        }

        let (symbols, refs): (i64,i64)=store.conn.query_row("SELECT (SELECT COUNT(*) FROM symbols),(SELECT COUNT(*) FROM refs)",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if symbols>200_000 || refs>500_000 { return Err(Error::Invalid("index size budget exceeded (200000 symbols / 500000 references); narrow the root or exclusions".into())); }
        if stats.files_indexed > 0 || stats.files_removed > 0 || stats.resolution_recomputed {
            stats.resolution_recomputed=true;
            crate::imports::resolve(store)?;
            stats.resolution = resolve::resolve_all(store)?;
        }
        store.set_meta("resolution_version", &resolve::fingerprint())?;
        store.set_meta("last_indexed_at", &now_unix().to_string())?;
        store.set_meta("parser_versions", &serde_json::to_string(&lang::fingerprints())?)?;
        store.set_meta("index_options", &serde_json::to_string(options)?)?;
        stats.duration_ms=started.elapsed().as_millis();
        store.set_meta("health", &serde_json::to_string(&stats)?)?;
        store.set_meta("last_freshness_check", "null")?;
        store.bump_generation()?;
        if stats.resolution_recomputed || store.generation()? == 1 {
            store.save_snapshot(stats.scan_complete && stats.files_failed == 0 && stats.parse_error_files == 0)?;
        }
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
    store: &Store,
    options: &IndexOptions,
    stats: &mut IndexStats,
) -> Result<(Vec<Candidate>, u64)> {
    // Exclusions are gitignore-style ignore patterns; everything else is walked.
    let mut overrides = OverrideBuilder::new(root);
    for pattern in &options.exclude {
        let trimmed = pattern.trim();
        if trimmed.is_empty() {
            continue;
        }
        overrides.add(&format!("!{trimmed}")).map_err(|error| {
            Error::Invalid(format!("invalid exclude pattern `{trimmed}`: {error}"))
        })?;
    }
    let overrides = overrides
        .build()
        .map_err(|error| Error::Invalid(format!("invalid exclude patterns: {error}")))?;

    let state_dir = store.state_dir().map(Path::to_path_buf);
    let walker = WalkBuilder::new(root)
        .hidden(true)
        .parents(false)
        .require_git(false)
        .overrides(overrides)
        .filter_entry(move |entry| {
            entry.file_name() != ".ast-index"
                && state_dir
                    .as_deref()
                    .is_none_or(|state_dir| !entry.path().starts_with(state_dir))
        })
        .build();

    let database = store.path();
    let mut internal_paths = HashSet::from([
        database.to_path_buf(),
        database.with_extension("writer.lock"),
    ]);
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = database.as_os_str().to_os_string();
        sidecar.push(suffix);
        internal_paths.insert(PathBuf::from(sidecar));
    }
    let mut candidates = Vec::new();
    let mut total_bytes = 0u64;
    let mut probe_bytes = 0u64;
    for (walked, entry) in walker.enumerate() {
        if walked >= options.max_files.saturating_mul(4) {
            stats.scan_complete = false;
            record_failure(stats, options, "directory traversal budget exceeded".into());
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                stats.files_failed += 1;
                stats.scan_complete = false;
                record_failure(
                    stats,
                    options,
                    "directory traversal failed; absent indexed paths retained".into(),
                );
                continue;
            }
        };
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let path = entry.path();
        if internal_paths.contains(path) {
            continue;
        }
        let extension_spec = lang::detect(path);
        if extension_spec.is_none() && path.extension().is_some() {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => {
                stats.files_failed += 1;
                stats.scan_complete = false;
                record_failure(
                    stats,
                    options,
                    "file metadata unavailable; absent indexed paths retained".into(),
                );
                continue;
            }
        };
        let size = metadata.len();
        let prefix = if extension_spec.is_none() || extension_spec.is_some_and(|s| s.id == "sql") {
            use std::io::Read;
            let remaining = options.max_total_bytes.saturating_sub(probe_bytes);
            if remaining == 0 {
                stats.scan_complete = false;
                record_failure(stats, options, "source header probe budget exceeded".into());
                break;
            }
            let mut prefix = Vec::new();
            let reservation = 512.min(remaining);
            probe_bytes += reservation;
            match open_source(root, path).and_then(|file| {
                file.take(reservation)
                    .read_to_end(&mut prefix)
                    .map_err(|e| Error::io(path, e))
            }) {
                Ok(_) => probe_bytes -= reservation - prefix.len() as u64,
                Err(_) => {
                    stats.files_failed += 1;
                    stats.scan_complete = false;
                    record_failure(stats, options, "source header probe failed".into());
                    continue;
                }
            }
            prefix
        } else {
            Vec::new()
        };
        if prefix.starts_with(b"SQLite format 3\0") {
            continue;
        }
        let Some(spec) = extension_spec.or_else(|| lang::detect_shebang(&prefix)) else {
            continue;
        };
        if size > options.max_file_bytes {
            stats.files_skipped += 1;
            stats.scan_complete = false;
            record_failure(
                stats,
                options,
                format!(
                    "{}: file size limit exceeded",
                    path.strip_prefix(root).unwrap_or(path).display()
                ),
            );
            continue;
        }
        total_bytes = total_bytes.saturating_add(size);
        if candidates.len() >= options.max_files
            || total_bytes.saturating_add(probe_bytes) > options.max_total_bytes
        {
            stats.scan_complete = false;
            record_failure(
                stats,
                options,
                "index scan budget exceeded; absent indexed paths retained".into(),
            );
            break;
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
    Ok((candidates, probe_bytes))
}

fn index_one(
    store: &Store,
    candidate: &Candidate,
    source: &str,
    hash: &str,
    root: &Path,
    parsers: &mut parse::ParserCache,
) -> Result<(usize, usize, bool)> {
    let parsed = parsers.parse(source, candidate.lang)?;
    let file_id = store.upsert_file(
        &candidate.relative,
        candidate.lang.family(),
        candidate.size,
        candidate.mtime_ns,
        hash,
    )?;
    store.conn.execute(
        "UPDATE files SET project = ?2, fingerprint = ?3, parse_errors = ?4 WHERE id = ?1",
        rusqlite::params![
            file_id,
            project_boundary(root, &candidate.absolute),
            candidate.lang.fingerprint(),
            parsed.has_errors
        ],
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
            start_column: symbol.start_column,
            end_column: symbol.end_column,
            parent: symbol.parent.clone(),
            qualified: symbol.qualified.clone(),
        })
        .collect();
    let ids = store.insert_symbols(file_id, &inserts)?;
    for (symbol, id) in parsed.symbols.iter().zip(&ids) {
        store.conn.execute(
            "UPDATE symbols SET signature = ?2, content_hash = ?3, exported = ?4 WHERE id = ?1",
            rusqlite::params![
                id,
                symbol.signature,
                blake3::hash(&source.as_bytes()[symbol.start_byte..symbol.end_byte])
                    .to_hex()
                    .to_string(),
                symbol.exported
            ],
        )?;
    }

    let mut by_qualified: HashMap<&str, Vec<(usize, usize, i64)>> = HashMap::new();
    for (symbol, id) in parsed.symbols.iter().zip(ids.iter()) {
        by_qualified
            .entry(symbol.qualified.as_str())
            .or_default()
            .push((symbol.start_byte, symbol.end_byte, *id));
    }
    for entries in by_qualified.values_mut() {
        entries.sort_unstable();
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
                .and_then(|name| by_qualified.get(name))
                .and_then(|entries| {
                    let index = entries
                        .partition_point(|(start, _, _)| *start <= reference.byte)
                        .checked_sub(1)?;
                    let (start, end, id) = entries[index];
                    (reference.byte < end && (index == 0 || entries[index - 1].0 != start))
                        .then_some(id)
                }),
            confidence: Confidence::None,
            blocked: reference.blocked.clone(),
        })
        .collect();
    store.insert_refs(file_id, &refs)?;

    Ok((parsed.symbols.len(), parsed.refs.len(), parsed.has_errors))
}

fn project_boundary(root: &Path, file: &Path) -> String {
    for parent in file.ancestors().skip(1) {
        if parent == root {
            break;
        }
        if [
            "Cargo.toml",
            "package.json",
            "pyproject.toml",
            "setup.py",
            "go.mod",
            ".git",
        ]
        .iter()
        .any(|marker| parent.join(marker).exists())
        {
            return parent
                .strip_prefix(root)
                .unwrap_or(parent)
                .to_string_lossy()
                .replace('\\', "/");
        }
    }
    String::new()
}

/// Only bounded module declarations are read, never Go commands or dependencies.
fn go_modules(
    root: &Path,
    candidates: &[Candidate],
    options: &IndexOptions,
    stats: &mut IndexStats,
    bytes_read: &mut u64,
) -> Result<String> {
    let projects: std::collections::BTreeSet<_> = candidates
        .iter()
        .filter(|c| c.lang.id == "go")
        .map(|c| project_boundary(root, &c.absolute))
        .collect();
    let mut modules = std::collections::BTreeMap::new();
    for project in projects {
        let path = root.join(&project).join("go.mod");
        if !path.exists() {
            continue;
        }
        let bytes = match read_budgeted(root, &path, 64_000, options.max_total_bytes, bytes_read) {
            Ok(bytes) => bytes,
            Err(_) => {
                stats.files_failed += 1;
                stats.scan_complete = false;
                record_failure(
                    stats,
                    options,
                    "Go module metadata could not be read within budget".into(),
                );
                continue;
            }
        };
        let module = crate::parse::go::module_path(std::str::from_utf8(&bytes).unwrap_or_default());
        modules.insert(project, (module, blake3::hash(&bytes).to_hex().to_string()));
    }
    Ok(serde_json::to_string(&modules)?)
}

/// Reserve the read plus its overflow sentinel before opening the file. Failed
/// reads retain the reservation, so repeated failures cannot reset the budget.
fn read_budgeted(
    root: &Path,
    path: &Path,
    file_limit: u64,
    total_limit: u64,
    used: &mut u64,
) -> Result<Vec<u8>> {
    let reservation = total_limit
        .saturating_sub(*used)
        .min(file_limit.saturating_add(1));
    if reservation == 0 {
        return Err(Error::Invalid("source read budget exceeded".into()));
    }
    *used += reservation;
    let result = read_source(root, path, reservation - 1);
    if let Ok(bytes) = &result {
        *used -= reservation - bytes.len() as u64;
    }
    result
}

/// Bounded, non-symlink file read. Source content cannot grant additional paths.
pub(crate) fn read_source(root: &Path, path: &Path, limit: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let file = open_source(root, path)?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| Error::io(path, e))?;
    if bytes.len() as u64 > limit {
        return Err(Error::Invalid("source read budget exceeded".into()));
    }
    Ok(bytes)
}

fn open_source(root: &Path, path: &Path) -> Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let canonical = path.canonicalize().map_err(|e| Error::io(path, e))?;
    if !canonical.starts_with(root) {
        return Err(Error::Invalid("source path escapes indexed root".into()));
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(&canonical)
        .map_err(|e| Error::io(path, e))?;
    if !file.metadata().map_err(|e| Error::io(path, e))?.is_file() {
        return Err(Error::Invalid("source is not a regular file".into()));
    }
    Ok(file)
}

/// Check current bytes without changing indexed content or trusting timestamps.
pub fn freshness(root: &Path, store: &Store) -> Result<crate::model::Freshness> {
    let _writer = store.write_lock()?;
    let root = root.canonicalize().map_err(|e| Error::io(root, e))?;
    store.require_root(&root)?;
    let options: IndexOptions = store
        .meta("index_options")?
        .map(|s| serde_json::from_str(&s))
        .transpose()?
        .unwrap_or_default();
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
        scan_complete: true,
        parse_error_files: 0,
        resolution_recomputed: false,
    };
    let (candidates, mut metadata_bytes) = collect_candidates(&root, store, &options, &mut stats)?;
    let current_go_modules = go_modules(
        &root,
        &candidates,
        &options,
        &mut stats,
        &mut metadata_bytes,
    )?;
    let existing: HashMap<_, _> = store.file_states()?.into_iter().collect();
    let present: HashSet<_> = candidates.iter().map(|c| c.relative.as_str()).collect();
    let mut changed = 0usize;
    let mut examples = Vec::new();
    let mut bytes_read = metadata_bytes;
    for candidate in &candidates {
        let matches = match read_budgeted(
            &root,
            &candidate.absolute,
            options.max_file_bytes,
            options.max_total_bytes,
            &mut bytes_read,
        ) {
            Ok(bytes) => existing.get(&candidate.relative).is_some_and(|state| {
                state.hash == blake3::hash(&bytes).to_hex().as_str()
                    && state.fingerprint == candidate.lang.fingerprint()
                    && state.project == project_boundary(&root, &candidate.absolute)
            }),
            Err(_) => {
                stats.scan_complete = false;
                false
            }
        };
        if !matches {
            changed += 1;
            if examples.len() < 16 {
                examples.push(candidate.relative.clone());
            }
        }
    }
    if stats.scan_complete {
        for path in existing.keys().filter(|p| !present.contains(p.as_str())) {
            changed += 1;
            if examples.len() < 16 {
                examples.push(path.clone());
            }
        }
    }
    let health: Option<IndexStats> = store
        .meta("health")?
        .map(|s| serde_json::from_str(&s))
        .transpose()?;
    stats.scan_complete &= health
        .as_ref()
        .is_some_and(|h| h.scan_complete && h.files_failed == 0 && h.parse_error_files == 0);
    examples.sort();
    let version_changed = store.meta("resolution_version")?.as_deref()
        != Some(&resolve::fingerprint())
        || store.meta("go_modules")?.as_deref() != Some(&current_go_modules);
    let result = crate::model::Freshness {
        state: if changed > 0 || version_changed {
            "stale"
        } else if !stats.scan_complete {
            "incomplete"
        } else {
            "fresh_at_check"
        }
        .into(),
        checked_at: now_unix(),
        changed_files: changed,
        version_changed,
        examples,
        complete: stats.scan_complete,
    };
    store.set_meta("last_freshness_check", &serde_json::to_string(&result)?)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_reads_consume_the_shared_budget() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let path = root.join("large");
        std::fs::write(&path, vec![b'x'; 100]).unwrap();
        let mut used = 0;
        assert!(read_budgeted(&root, &path, 30, 40, &mut used).is_err());
        assert_eq!(used, 31);
        assert!(read_budgeted(&root, &path, 30, 40, &mut used).is_err());
        assert_eq!(used, 40);
        assert!(read_budgeted(&root, &path, 30, 40, &mut used).is_err());
        assert_eq!(used, 40);
    }
}
