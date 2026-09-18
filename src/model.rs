//! Index data model: files, symbols, references and their resolution state.

use serde::{Deserialize, Serialize};

/// Kind of a definition. Values are stable strings because they cross the MCP
/// boundary and are stored in SQLite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Trait,
    Interface,
    Module,
    Const,
    Static,
    TypeAlias,
    Macro,
    Impl,
}

impl SymbolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Class => "class",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Interface => "interface",
            Self::Module => "module",
            Self::Const => "const",
            Self::Static => "static",
            Self::TypeAlias => "type_alias",
            Self::Macro => "macro",
            Self::Impl => "impl",
        }
    }

    /// Parse a capture suffix such as `function` from `@definition.function`.
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "function" => Self::Function,
            "method" => Self::Method,
            "class" => Self::Class,
            "struct" => Self::Struct,
            "enum" => Self::Enum,
            "trait" => Self::Trait,
            "interface" => Self::Interface,
            "module" => Self::Module,
            "const" => Self::Const,
            "static" => Self::Static,
            "type" | "type_alias" => Self::TypeAlias,
            "macro" => Self::Macro,
            "impl" => Self::Impl,
            _ => return None,
        })
    }

    /// Parse a stored kind string.
    pub fn from_stored(value: &str) -> Option<Self> {
        Some(match value {
            "function" => Self::Function,
            "method" => Self::Method,
            "class" => Self::Class,
            "struct" => Self::Struct,
            "enum" => Self::Enum,
            "trait" => Self::Trait,
            "interface" => Self::Interface,
            "module" => Self::Module,
            "const" => Self::Const,
            "static" => Self::Static,
            "type_alias" => Self::TypeAlias,
            "macro" => Self::Macro,
            "impl" => Self::Impl,
            _ => return None,
        })
    }
}

/// Kind of an outgoing reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    Call,
    Import,
}

impl RefKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Import => "import",
        }
    }

    pub fn from_stored(value: &str) -> Option<Self> {
        Some(match value {
            "call" => Self::Call,
            "import" => Self::Import,
            _ => return None,
        })
    }
}

/// How a reference was matched to a definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Single match in the same file.
    Exact,
    /// Single match in the same directory.
    High,
    /// Single match in the whole index.
    Low,
    /// Unresolved or ambiguous; kept as a plain reference.
    None,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::High => "high",
            Self::Low => "low",
            Self::None => "none",
        }
    }

    pub fn from_stored(value: &str) -> Option<Self> {
        Some(match value {
            "exact" => Self::Exact,
            "high" => Self::High,
            "low" => Self::Low,
            "none" => Self::None,
            _ => return None,
        })
    }
}

/// A stored definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub id: i64,
    /// Path relative to the indexed root.
    pub file: String,
    pub name: String,
    pub kind: SymbolKind,
    pub start_line: u32,
    pub end_line: u32,
    pub parent: Option<String>,
    /// `parent::name` when nested, otherwise `name`.
    pub qualified: String,
}

/// A call site or import recorded against a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reference {
    pub id: i64,
    pub file: String,
    pub name: String,
    pub kind: RefKind,
    pub line: u32,
    pub from_symbol: Option<String>,
    pub resolved: Option<i64>,
    pub confidence: Confidence,
    /// Full scoped path when the language provides one.
    pub path: Option<String>,
}

/// An outgoing edge from a symbol to a resolved callee.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: Option<String>,
    pub to: String,
    pub file: String,
    pub line: u32,
    pub confidence: Confidence,
}

/// Aggregate counts for `status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub root: String,
    pub indexed: bool,
    pub last_indexed_at: Option<i64>,
    pub files: i64,
    pub symbols: i64,
    pub references: i64,
    pub imports: i64,
    pub resolved_references: i64,
    pub languages: Vec<LanguageCount>,
}

/// One row of the language breakdown in `status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageCount {
    pub language: String,
    pub files: i64,
    pub symbols: i64,
}
