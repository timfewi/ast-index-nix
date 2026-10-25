//! High-level operations shared by the CLI, the stdio MCP adapter and the
//! Unix socket service.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::index::{self, IndexOptions, IndexStats};
use crate::model::Status;
use crate::store::Store;

/// Directory (relative to the indexed root) that holds the database.
pub const INDEX_DIR: &str = ".ast-index";

/// Default database file name.
pub const DB_FILE: &str = "index.sqlite";

/// Engine bound to one indexed root.
pub struct Engine {
    store: Store,
    root: PathBuf,
}

impl Engine {
    /// Open the engine for `root`, using `db` or `<root>/.ast-index/index.sqlite`.
    pub fn open(root: impl Into<PathBuf>, db: Option<PathBuf>) -> Result<Self> {
        let root = root.into();
        let root = root.canonicalize().map_err(|e| Error::io(&root, e))?;
        if db.is_none() {
            for path in [root.join(INDEX_DIR), default_db_path(&root)] {
                if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
                    return Err(Error::Invalid("default index directory/database cannot be a symbolic link; select an explicit database path".into()));
                }
            }
        }
        let db = db.unwrap_or_else(|| default_db_path(&root));
        let store = Store::open(db)?;
        store.require_root(&root)?;
        Ok(Self { store, root })
    }

    /// Open independent derived state for `root` in a dedicated directory.
    pub fn open_state_dir(root: impl Into<PathBuf>, state_dir: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let root = root.canonicalize().map_err(|e| Error::io(&root, e))?;
        let store = Store::open_state_dir(state_dir)?;
        store.require_root(&root)?;
        Ok(Self { store, root })
    }

    /// Execute one bounded query against a consistent database snapshot.
    pub fn execute(&self, query: &crate::query::Query) -> Result<serde_json::Value> {
        query.validate()?;
        let mut normalized = query.clone();
        normalized.file = query
            .file
            .as_deref()
            .map(crate::query::relative_path)
            .transpose()?;
        normalized.subtree = query
            .subtree
            .as_deref()
            .map(crate::query::relative_path)
            .transpose()?
            .filter(|path| !path.is_empty());
        let query = &normalized;
        if query.action == "freshness" {
            self.require_index()?;
            return Ok(serde_json::to_value(index::freshness(
                &self.root,
                &self.store,
            )?)?);
        }
        self.store.query_transaction(|| {
            self.store.require_root(&self.root)?;
            if query.action == "status" { return Ok(serde_json::to_value(self.status()?)?); }
            self.require_index()?;
            let window = query.window(self.store.generation()?)?;
            let value = match query.action.as_str() {
                "search" | "outline" => {
                    if query.action == "outline" && query.file.is_none() { return Err(Error::Invalid("outline requires file".into())); }
                    if query.action == "search" { query.name()?; }
                    serde_json::to_value(window.page(self.store.select_symbols(query, &window, false)?)?)?
                }
                "semantic" => crate::scip::query(&self.root, &self.store, query, &window)?,
                "diff" => self.store.snapshot_diff(query, &window)?,
                "changes" => self.store.changed_impact(query, &window)?,
                "map" => self.store.repository_map(query, &window)?,
                "imports" | "importers" | "deps" => serde_json::to_value(window.page(self.store.select_imports(query, &window)?)?)?,
                "refs" => serde_json::to_value(window.page(self.store.select_refs(query, &window)?)?)?,
                "describe" | "callers" | "callees" | "impact" => {
                    query.name()?;
                    let symbol = self.store.select_one(query)?;
                    match query.action.as_str() {
                        "describe" => serde_json::json!({ "symbol": symbol,
                            "callers": window.page(self.store.select_edges(query, &window, symbol.id, true)?)?,
                            "callees": window.page(self.store.select_edges(query, &window, symbol.id, false)?)? }),
                        "callers" | "callees" => serde_json::to_value(window.page(self.store.select_edges(query, &window, symbol.id, query.action == "callers")?)?)?,
                        _ => {
                            let rows = self.store.select_impact(query, &window, symbol.id)?;
                            serde_json::to_value(window.page(rows)?)?
                        }
                    }
                }
                _ => return Err(Error::Invalid(format!("unknown action `{}`", query.action))),
            };
            if serde_json::to_vec(&value)?.len() > 60 * 1024 { return Err(Error::Invalid("result exceeds output budget; reduce limit".into())); }
            Ok(value)
        })
    }

    /// The default database path for a root.
    pub fn default_db(&self) -> PathBuf {
        default_db_path(&self.root)
    }

    /// Indexed root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Run an indexing pass.
    pub fn index(&self, options: &IndexOptions) -> Result<IndexStats> {
        index::index(&self.root, &self.store, options)
    }

    /// Aggregate index status.
    pub fn status(&self) -> Result<Status> {
        self.store.status()
    }

    fn require_index(&self) -> Result<()> {
        if self.store.is_indexed()? {
            return Ok(());
        }
        Err(Error::NotIndexed(format!(
            "no index at {}: run `ast-index index` with the same --root and --state-dir/--db selection first",
            self.store.path().display()
        )))
    }

    /// Access the underlying store (tests and future operations).
    pub fn store(&self) -> &Store {
        &self.store
    }
}

/// Default database path for a root directory.
pub fn default_db_path(root: &Path) -> PathBuf {
    root.join(INDEX_DIR).join(DB_FILE)
}
