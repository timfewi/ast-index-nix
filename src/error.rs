//! Error type shared by indexing, storage, queries and the MCP surface.

use std::path::PathBuf;

use thiserror::Error;

/// Errors surfaced by the library. The MCP layer converts these into
/// service-authored, stable messages; source content never becomes a code path.
#[derive(Debug, Error)]
pub enum Error {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("no index found: {0}")]
    NotIndexed(String),
    #[error("invalid request: {0}")]
    Invalid(String),
}

impl Error {
    /// Attach a path to an io error.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// Convenience result alias.
pub type Result<T> = std::result::Result<T, Error>;
