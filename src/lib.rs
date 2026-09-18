//! Lightweight AST code index and query service for local agent harnesses.
//!
//! One library backs three frontends: the `ast-index` CLI, the stdio MCP adapter
//! and the Unix socket service. Indexing extracts definitions, call sites and
//! imports per language with tree-sitter, stores them in SQLite and resolves
//! call edges by name with a precision-first scope ladder.

pub mod engine;
pub mod error;
pub mod index;
pub mod lang;
pub mod model;
pub mod parse;
pub mod report;
pub mod resolve;
pub mod rpc;
pub mod serve;
pub mod store;

pub use engine::{Engine, SymbolLookup};
pub use error::{Error, Result};
pub use index::{IndexOptions, IndexStats};
pub use rpc::RpcServer;
