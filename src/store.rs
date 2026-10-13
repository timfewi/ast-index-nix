//! SQLite storage. One database file per indexed root holds files, definitions,
//! references and their resolution state. WAL is requested but not assumed:
//! network filesystems fall back to the rollback journal.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::{Confidence, LanguageCount, RefKind, Status, Symbol, SymbolKind};

mod history;
mod query;

/// Schema version stored in `meta`.
pub const SCHEMA_VERSION: i64 = 6;

/// Hash state of one file as recorded during indexing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileState {
    pub size: i64,
    pub mtime_ns: i64,
    pub hash: String,
    pub lang: String,
    pub fingerprint: String,
    pub parse_errors: bool,
    pub project: String,
}

/// A stored reference row used by the resolver.
#[derive(Debug, Clone)]
pub struct RefRow {
    pub id: i64,
    pub language: String,
    pub project: String,
    pub file: String,
    pub directory: String,
    pub name: String,
    /// Full scoped path such as `Store::open`, when the language provides one.
    pub path: Option<String>,
    pub kind: RefKind,
    pub blocked: Option<String>,
}

/// A symbol row used by the resolver.
#[derive(Debug, Clone)]
pub struct SymbolRow {
    pub id: i64,
    pub language: String,
    pub project: String,
    pub file: String,
    pub directory: String,
    pub name: String,
    pub exported: bool,
    pub kind: SymbolKind,
    /// Case-sensitive qualified name such as `Store::open`.
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
    pub reason: String,
}

/// A symbol reachable from `impact`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpactRow {
    pub symbol: Symbol,
    pub depth: u32,
}

/// Handle to the index database.
pub struct Store {
    pub(crate) conn: Connection,
    path: PathBuf,
}

impl Store {
    /// Open (and create) the index at `path`.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
        let path = parent
            .canonicalize()
            .map_err(|e| Error::io(&path, e))?
            .join(
                path.file_name()
                    .ok_or_else(|| Error::Invalid("database path needs a filename".into()))?,
            );
        let conn = Connection::open(&path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let journal: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;")?;
        let path = path.canonicalize().map_err(|e| Error::io(&path, e))?;
        let store = Self { conn, path };
        let has_meta: bool = store.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='meta')",
            [],
            |r| r.get(0),
        )?;
        let version = if has_meta {
            store.meta("schema_version")?
        } else {
            None
        };
        if version
            .as_deref()
            .and_then(|v| v.parse::<i64>().ok())
            .is_some_and(|v| v > SCHEMA_VERSION)
        {
            return Err(Error::Invalid(
                "database schema is newer than this executable".into(),
            ));
        }
        if version.as_deref() != Some(&SCHEMA_VERSION.to_string()) {
            let _guard = store.write_lock()?;
            store.transaction(|| {
                store.init_schema()?;
                store.set_meta("journal_mode", &journal)
            })?;
        }
        Ok(store)
    }

    /// One writer per canonical database path. Readers do not hold this lock.
    pub fn write_lock(&self) -> Result<nix::fcntl::Flock<std::fs::File>> {
        use std::os::unix::fs::OpenOptionsExt;
        let lock = self.path.with_extension("writer.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(&lock)
            .map_err(|e| Error::io(&lock, e))?;
        nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock).map_err(
            |(_, e)| {
                Error::Invalid(format!(
                    "index writer busy ({e}); wait for the active indexing/import operation"
                ))
            },
        )
    }

    /// Path of the database file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Check again under writer ownership before constructing an update: an
    /// engine may have opened an empty database before another root indexed it.
    pub(crate) fn require_root(&self, root: &Path) -> Result<()> {
        if self
            .meta("root")?
            .is_some_and(|stored| Path::new(&stored) != root)
        {
            return Err(Error::Invalid("database belongs to a different indexed root; select that root or a separate database".into()));
        }
        Ok(())
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
        for (table, column, definition) in [
            ("files", "project", "TEXT NOT NULL DEFAULT ''"),
            ("files", "fingerprint", "TEXT NOT NULL DEFAULT ''"),
            ("files", "parse_errors", "INTEGER NOT NULL DEFAULT 0"),
            ("symbols", "start_column", "INTEGER NOT NULL DEFAULT 1"),
            ("symbols", "end_column", "INTEGER NOT NULL DEFAULT 1"),
            ("symbols", "signature", "TEXT NOT NULL DEFAULT ''"),
            ("symbols", "exported", "INTEGER NOT NULL DEFAULT 0"),
            ("symbols", "content_hash", "TEXT NOT NULL DEFAULT ''"),
            ("refs", "blocked", "TEXT"),
            ("refs", "import_target", "TEXT"),
            ("refs", "reason", "TEXT NOT NULL DEFAULT 'unknown'"),
        ] {
            let mut statement = self.conn.prepare(&format!("PRAGMA table_info({table})"))?;
            let names: Vec<String> = statement
                .query_map([], |r| r.get(1))?
                .collect::<rusqlite::Result<_>>()?;
            if !names.iter().any(|name| name == column) {
                self.conn.execute_batch(&format!(
                    "ALTER TABLE {table} ADD COLUMN {column} {definition}"
                ))?;
            }
        }
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_symbols_qualified ON symbols(qualified_lower);
            CREATE INDEX IF NOT EXISTS idx_symbols_prefix ON symbols(name_lower COLLATE NOCASE);
            CREATE INDEX IF NOT EXISTS idx_symbols_qualified_prefix ON symbols(qualified_lower COLLATE NOCASE);
            CREATE INDEX IF NOT EXISTS idx_symbols_exact_name ON symbols(name);
            CREATE INDEX IF NOT EXISTS idx_symbols_exact_qualified ON symbols(qualified);",
        )?;
        self.conn.execute_batch("CREATE TABLE IF NOT EXISTS snapshots(generation INTEGER PRIMARY KEY, complete INTEGER NOT NULL, note TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS snapshot_symbols(generation INTEGER NOT NULL, symbol_id INTEGER NOT NULL, identity TEXT NOT NULL, file TEXT NOT NULL,
                name TEXT NOT NULL, kind TEXT NOT NULL, start_line INTEGER NOT NULL, end_line INTEGER NOT NULL, parent TEXT, qualified TEXT NOT NULL, start_column INTEGER NOT NULL, end_column INTEGER NOT NULL, content_hash TEXT NOT NULL,
                PRIMARY KEY(generation,symbol_id));
            CREATE INDEX IF NOT EXISTS idx_snapshot_identity ON snapshot_symbols(generation,identity);
            CREATE TABLE IF NOT EXISTS snapshot_edges(generation INTEGER NOT NULL, caller INTEGER, callee INTEGER NOT NULL, file TEXT NOT NULL, line INTEGER NOT NULL, reason TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS idx_snapshot_callers ON snapshot_edges(generation,callee);")?;
        self.conn.execute_batch("CREATE TABLE IF NOT EXISTS semantic_documents(file TEXT PRIMARY KEY, hash TEXT NOT NULL, artifact TEXT NOT NULL, indexer TEXT NOT NULL, indexer_version TEXT NOT NULL, encoding INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS semantic_occurrences(id INTEGER PRIMARY KEY, file TEXT NOT NULL REFERENCES semantic_documents(file) ON DELETE CASCADE,
                identity TEXT NOT NULL, symbol TEXT NOT NULL, display_name TEXT NOT NULL, role INTEGER NOT NULL, range TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS idx_semantic_symbol ON semantic_occurrences(symbol);
            CREATE TABLE IF NOT EXISTS semantic_implementations(id INTEGER PRIMARY KEY, file TEXT NOT NULL REFERENCES semantic_documents(file) ON DELETE CASCADE,
                identity TEXT NOT NULL, symbol TEXT NOT NULL, target_identity TEXT NOT NULL, target_symbol TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS idx_semantic_implementations ON semantic_implementations(target_symbol);")?;
        for column in ["start_column", "end_column"] {
            let mut statement = self.conn.prepare("PRAGMA table_info(snapshot_symbols)")?;
            let names: Vec<String> = statement
                .query_map([], |r| r.get(1))?
                .collect::<rusqlite::Result<_>>()?;
            if !names.iter().any(|n| n == column) {
                self.conn.execute_batch(&format!("ALTER TABLE snapshot_symbols ADD COLUMN {column} INTEGER NOT NULL DEFAULT 1; UPDATE snapshots SET complete=0,note='snapshot predates position tracking';"))?;
            }
        }
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

    pub fn generation(&self) -> Result<i64> {
        Ok(self
            .meta("generation")?
            .and_then(|s| s.parse().ok())
            .unwrap_or(0))
    }

    pub(crate) fn bump_generation(&self) -> Result<()> {
        self.set_meta(
            "generation",
            &self
                .generation()?
                .checked_add(1)
                .ok_or_else(|| Error::Invalid("index generation exhausted".into()))?
                .to_string(),
        )
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

    /// Read a consistent snapshot with a bounded SQLite VM instruction budget.
    pub(crate) fn query_transaction<T>(&self, action: impl FnOnce() -> Result<T>) -> Result<T> {
        let mut ticks = 20_000usize;
        self.conn.progress_handler(
            1000,
            Some(move || {
                ticks = ticks.saturating_sub(1);
                ticks == 0
            }),
        )?;
        let result = self.transaction(action);
        self.conn.progress_handler(0, None::<fn() -> bool>)?;
        match result {
            Err(Error::Database(rusqlite::Error::SqliteFailure(e, _)))
                if e.code == rusqlite::ErrorCode::OperationInterrupted =>
            {
                Err(Error::Invalid(
                    "query computation budget exceeded; narrow the query or reduce depth".into(),
                ))
            }
            other => other,
        }
    }

    /// Recorded hash state of a file, if present.
    pub fn file_state(&self, path: &str) -> Result<Option<FileState>> {
        let state = self
            .conn
            .query_row(
                "SELECT size, mtime_ns, hash, lang, fingerprint, parse_errors, project FROM files WHERE path = ?1",
                [path],
                |row| {
                    Ok(FileState {
                        size: row.get(0)?,
                        mtime_ns: row.get(1)?,
                        hash: row.get(2)?,
                        lang: row.get(3)?,
                        fingerprint: row.get(4)?,
                        parse_errors: row.get(5)?,
                        project: row.get(6)?,
                    })
                },
            )
            .optional()?;
        Ok(state)
    }

    /// All indexed files as `(path, state)`.
    pub fn file_states(&self) -> Result<Vec<(String, FileState)>> {
        let mut statement = self.conn.prepare(
            "SELECT path, size, mtime_ns, hash, lang, fingerprint, parse_errors, project FROM files",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                FileState {
                    size: row.get(1)?,
                    mtime_ns: row.get(2)?,
                    hash: row.get(3)?,
                    lang: row.get(4)?,
                    fingerprint: row.get(5)?,
                    parse_errors: row.get(6)?,
                    project: row.get(7)?,
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
                start_line, end_line, parent, start_column, end_column
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
                symbol.start_column,
                symbol.end_column,
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
                resolved_symbol_id, confidence, blocked, reason
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9, COALESCE(?9, 'unknown'))",
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
                reference.blocked,
            ])?;
        }
        Ok(())
    }

    /// Load one symbol by id.
    pub fn symbol_by_id(&self, id: i64) -> Result<Option<Symbol>> {
        let symbol = self
            .conn
            .query_row(
                "SELECT s.id, f.path, s.name, s.kind, s.start_line, s.end_line, s.parent, s.qualified, s.start_column, s.end_column
                 FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.id = ?1",
                [id],
                symbol_from_row,
            )
            .optional()?;
        Ok(symbol)
    }

    /// Transitive callers of a symbol up to `depth` hops.
    pub(crate) fn impact_ids(&self, symbol_id: i64, depth: u32) -> Result<Vec<(i64, u32)>> {
        use std::collections::{HashSet, VecDeque};
        let mut seen = HashSet::from([symbol_id]);
        let mut queue = VecDeque::from([(symbol_id, 0)]);
        let mut result = Vec::new();
        let mut remaining = 20_000usize;
        let mut statement = self.conn.prepare(
            "SELECT DISTINCT from_symbol_id FROM refs
            WHERE resolved_symbol_id = ?1 AND kind = 'call' AND from_symbol_id IS NOT NULL
            ORDER BY from_symbol_id LIMIT ?2",
        )?;
        while let Some((id, distance)) = queue.pop_front() {
            if distance >= depth.min(10) {
                continue;
            }
            let callers: Vec<i64> = statement
                .query_map(params![id, (remaining + 1) as i64], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            if callers.len() > remaining {
                return Err(Error::Invalid(
                    "impact traversal budget exceeded; reduce depth or choose a narrower seed"
                        .into(),
                ));
            }
            remaining -= callers.len();
            for caller in callers {
                if seen.insert(caller) {
                    result.push((caller, distance + 1));
                    queue.push_back((caller, distance + 1));
                }
            }
        }

        Ok(result)
    }

    /// Candidate symbols for reference resolution.
    pub fn resolution_symbols(&self) -> Result<Vec<SymbolRow>> {
        let mut statement = self.conn.prepare(
            "SELECT s.id, f.path, s.name, s.qualified, f.lang, f.project, s.exported, s.kind FROM symbols s JOIN files f ON f.id = s.file_id",
        )?;
        let rows = statement.query_map([], |row| {
            let file: String = row.get(1)?;
            Ok(SymbolRow {
                id: row.get(0)?,
                language: row.get(4)?,
                exported: row.get(6)?,
                kind: SymbolKind::from_stored(&row.get::<_, String>(7)?)
                    .unwrap_or(SymbolKind::Function),
                project: row.get(5)?,
                directory: directory_of(&file),
                file,
                name: row.get(2)?,
                qualified: row.get(3)?,
            })
        })?;
        collect(rows)
    }

    /// Call and dependency references to resolve against indexed symbols.
    pub fn resolution_refs(&self) -> Result<Vec<RefRow>> {
        let mut statement = self.conn.prepare(
            "SELECT r.id, f.path, r.name, r.kind, r.path, f.lang, f.project, r.blocked
             FROM refs r JOIN files f ON f.id = r.file_id
             WHERE r.kind IN ('call', 'dependency')",
        )?;
        let rows = statement.query_map([], |row| {
            let file: String = row.get(1)?;
            Ok(RefRow {
                id: row.get(0)?,
                language: row.get(5)?,
                project: row.get(6)?,
                blocked: row.get(7)?,
                directory: directory_of(&file),
                file,
                name: row.get(2)?,
                kind: RefKind::from_stored(&row.get::<_, String>(3)?).unwrap_or(RefKind::Call),
                path: row.get(4)?,
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
        reason: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE refs SET resolved_symbol_id = ?2, confidence = ?3, reason = ?4 WHERE id = ?1",
            params![ref_id, symbol_id, confidence.as_str(), reason],
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
            "SELECT COUNT(*) FROM refs WHERE kind IN ('call', 'dependency') AND resolved_symbol_id IS NOT NULL",
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
        let health: Option<crate::index::IndexStats> = self
            .meta("health")?
            .map(|s| serde_json::from_str(&s))
            .transpose()?;
        let last_freshness_check: Option<crate::model::Freshness> = self
            .meta("last_freshness_check")?
            .map(|s| serde_json::from_str(&s))
            .transpose()?
            .flatten();
        let parser_versions: std::collections::BTreeMap<String, String> = self
            .meta("parser_versions")?
            .map(|s| serde_json::from_str(&s))
            .transpose()?
            .unwrap_or_default();
        let current_parser_versions = crate::lang::fingerprints();
        let resolution_version = self.meta("resolution_version")?;
        let current_resolution_version = crate::resolve::fingerprint();
        let reasons = self
            .conn
            .prepare("SELECT reason,COUNT(*) FROM refs GROUP BY reason ORDER BY reason")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let freshness = if !indexed {
            "not_indexed"
        } else if health
            .as_ref()
            .is_some_and(|h| !h.scan_complete || h.files_failed > 0 || h.parse_error_files > 0)
            || last_freshness_check.as_ref().is_some_and(|f| !f.complete)
        {
            "incomplete"
        } else if resolution_version.as_deref() != Some(&current_resolution_version)
            || parser_versions != current_parser_versions
            || last_freshness_check
                .as_ref()
                .is_some_and(|f| f.state == "stale")
        {
            "stale"
        } else {
            "unknown"
        }
        .to_string();
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
            generation: self.generation()?,
            freshness,
            last_freshness_check,
            health,
            parser_versions,
            current_parser_versions,
            resolution_reasons: reasons,
            resolution_version,
            current_resolution_version,
        })
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
    pub start_column: u32,
    pub end_column: u32,
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
    pub blocked: Option<String>,
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
        start_column: row.get(8)?,
        end_column: row.get(9)?,
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

pub(crate) fn directory_of(path: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_query_releases_snapshot_and_resets_the_budget() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("index.sqlite")).unwrap();
        let result: Result<i64> = store.query_transaction(|| {
            Ok(store.conn.query_row(
                "WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<100000000) SELECT sum(x) FROM n",
                [], |r| r.get(0),
            )?)
        });
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("computation budget")
        );
        assert!(store.conn.is_autocommit());
        assert_eq!(
            store
                .query_transaction(|| Ok(store
                    .conn
                    .query_row("SELECT 42", [], |r| r.get::<_, i64>(0))?))
                .unwrap(),
            42
        );
    }

    #[test]
    fn migration_preserves_legacy_rows_and_requires_new_extraction() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            INSERT INTO meta VALUES('schema_version','1');
            CREATE TABLE files(id INTEGER PRIMARY KEY,path TEXT NOT NULL UNIQUE,lang TEXT NOT NULL,size INTEGER NOT NULL,mtime_ns INTEGER NOT NULL,hash TEXT NOT NULL);
            CREATE TABLE symbols(id INTEGER PRIMARY KEY,file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,name TEXT NOT NULL,name_lower TEXT NOT NULL,qualified TEXT NOT NULL,qualified_lower TEXT NOT NULL,kind TEXT NOT NULL,start_line INTEGER NOT NULL,end_line INTEGER NOT NULL,parent TEXT);
            CREATE TABLE refs(id INTEGER PRIMARY KEY,file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,name TEXT NOT NULL,name_lower TEXT NOT NULL,path TEXT,kind TEXT NOT NULL,line INTEGER NOT NULL,from_symbol_id INTEGER REFERENCES symbols(id) ON DELETE SET NULL,resolved_symbol_id INTEGER REFERENCES symbols(id) ON DELETE SET NULL,confidence TEXT NOT NULL);
            INSERT INTO files VALUES(1,'lib.rs','rust',14,0,'legacy-hash');
            INSERT INTO symbols VALUES(1,1,'helper','helper','helper','helper','function',1,1,NULL);"
        ).unwrap();
        drop(connection);
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.meta("schema_version").unwrap(),
            Some(SCHEMA_VERSION.to_string())
        );
        let state = store.file_state("lib.rs").unwrap().unwrap();
        assert_eq!(state.hash, "legacy-hash");
        assert!(state.fingerprint.is_empty());
        let query = crate::query::Query {
            action: "outline".into(),
            file: Some("lib.rs".into()),
            ..Default::default()
        };
        let symbols = store
            .select_symbols(&query, &query.window(0).unwrap(), false)
            .unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "helper");
        assert_eq!(symbols[0].start_column, 1);
        drop(store);
        assert!(Store::open(path).is_ok());
    }
}
