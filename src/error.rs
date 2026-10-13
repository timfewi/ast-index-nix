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
    #[error("ambiguous symbol; select file, query and optionally line")]
    Ambiguous(Vec<crate::model::Symbol>),
    #[error("invalid request: {0}")]
    Invalid(String),
}

impl Error {
    /// Bounded structured error shared by both frontends.
    pub fn payload(&self) -> serde_json::Value {
        match self {
            Self::Ambiguous(candidates) => {
                let mut items = candidates.clone();
                let mut truncated = items.len() > 25;
                items.truncate(25);
                while serde_json::to_vec(&items).is_ok_and(|v| v.len() > 48 * 1024) {
                    items.pop();
                    truncated = true;
                }
                serde_json::json!({"error":"ambiguous_symbol","candidates":items,"truncated":truncated,"hint":"select file, query and optionally line/column"})
            }
            _ => {
                serde_json::json!({"error":self.to_string().chars().take(1024).collect::<String>()})
            }
        }
    }

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
