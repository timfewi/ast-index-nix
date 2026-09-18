//! High-level operations shared by the CLI, the stdio MCP adapter and the
//! Unix socket service.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::index::{self, IndexOptions, IndexStats};
use crate::model::{Status, Symbol};
use crate::store::{CallEdge, CallersAndCallees, ImpactRow, Store};

/// Directory (relative to the indexed root) that holds the database.
pub const INDEX_DIR: &str = ".ast-index";

/// Default database file name.
pub const DB_FILE: &str = "index.sqlite";

/// Result of resolving a user-supplied symbol name.
#[derive(Debug, Clone)]
pub enum SymbolLookup {
    Found(Symbol),
    Ambiguous(Vec<Symbol>),
    Missing,
}

/// Engine bound to one indexed root.
pub struct Engine {
    store: Store,
    root: PathBuf,
}

impl Engine {
    /// Open the engine for `root`, using `db` or `<root>/.ast-index/index.sqlite`.
    pub fn open(root: impl Into<PathBuf>, db: Option<PathBuf>) -> Result<Self> {
        let root = root.into();
        let db = db.unwrap_or_else(|| default_db_path(&root));
        let store = Store::open(db)?;
        Ok(Self { store, root })
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

    /// Definitions in one file, ordered by position.
    pub fn outline(&self, file: &str) -> Result<Vec<Symbol>> {
        self.require_index()?;
        let relative = self.relative(file);
        self.store.symbols_in_file(&relative)
    }

    /// Search definitions by name or qualified name.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Symbol>> {
        self.require_index()?;
        self.store.find_symbols(query, limit)
    }

    /// Resolve a symbol reference to exactly one definition.
    pub fn symbol(&self, query: &str) -> Result<Symbol> {
        self.require_index()?;
        match self.lookup(query)? {
            SymbolLookup::Found(symbol) => Ok(symbol),
            SymbolLookup::Ambiguous(candidates) => Err(Error::Invalid(format!(
                "`{query}` is ambiguous; candidates: {}",
                render_candidates(&candidates)
            ))),
            SymbolLookup::Missing => Err(Error::Invalid(format!("no symbol named `{query}`"))),
        }
    }

    /// Look up a symbol without failing on ambiguity.
    pub fn lookup(&self, query: &str) -> Result<SymbolLookup> {
        self.require_index()?;
        let mut matches = self.store.find_symbols(query, 25)?;
        if matches.is_empty() {
            return Ok(SymbolLookup::Missing);
        }
        // Exact name or qualified matches win over substring matches.
        let lower = query.to_lowercase();
        matches.sort_by_key(|symbol| {
            let exact =
                symbol.name.to_lowercase() == lower || symbol.qualified.to_lowercase() == lower;
            if exact { 0 } else { 1 }
        });
        let exact: Vec<Symbol> = matches
            .iter()
            .filter(|symbol| {
                symbol.name.to_lowercase() == lower || symbol.qualified.to_lowercase() == lower
            })
            .cloned()
            .collect();
        if exact.len() == 1 {
            return Ok(SymbolLookup::Found(exact[0].clone()));
        }
        if exact.len() > 1 {
            return Ok(SymbolLookup::Ambiguous(exact));
        }
        if matches.len() == 1 {
            return Ok(SymbolLookup::Found(matches.remove(0)));
        }
        Ok(SymbolLookup::Ambiguous(matches))
    }

    /// Incoming call edges.
    pub fn callers(&self, query: &str) -> Result<(Symbol, Vec<CallEdge>)> {
        let symbol = self.symbol(query)?;
        let edges = self.store.callers(symbol.id)?;
        Ok((symbol, edges))
    }

    /// Outgoing call edges.
    pub fn callees(&self, query: &str) -> Result<(Symbol, Vec<CallEdge>)> {
        let symbol = self.symbol(query)?;
        let edges = self.store.callees(symbol.id)?;
        Ok((symbol, edges))
    }

    /// Transitive callers up to `depth` hops.
    pub fn impact(&self, query: &str, depth: u32) -> Result<(Symbol, Vec<ImpactRow>)> {
        let symbol = self.symbol(query)?;
        let rows = self.store.impact(symbol.id, depth)?;
        Ok((symbol, rows))
    }

    /// Both directions at once, for the single-tool MCP surface.
    pub fn relations(&self, query: &str) -> Result<(Symbol, CallersAndCallees)> {
        let symbol = self.symbol(query)?;
        let callers = self.store.callers(symbol.id)?;
        let callees = self.store.callees(symbol.id)?;
        Ok((symbol, CallersAndCallees { callers, callees }))
    }

    fn require_index(&self) -> Result<()> {
        if self.store.is_indexed()? {
            return Ok(());
        }
        Err(Error::NotIndexed(format!(
            "no index at {}: run `ast-index index {}` first",
            self.root.display(),
            self.root.display()
        )))
    }

    /// Store paths are relative to the indexed root.
    pub fn relative(&self, path: &str) -> String {
        let candidate = Path::new(path);
        if candidate.is_absolute()
            && let Ok(stripped) = candidate.strip_prefix(&self.root)
        {
            return stripped.to_string_lossy().replace('\\', "/");
        }
        path.trim_start_matches("./").replace('\\', "/")
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

fn render_candidates(candidates: &[Symbol]) -> String {
    candidates
        .iter()
        .take(8)
        .map(|symbol| {
            format!(
                "{} ({}:{})",
                symbol.qualified, symbol.file, symbol.start_line
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}
