//! Language registry: which tree-sitter grammar and tag query to use per file.

use std::path::Path;

use tree_sitter::Language;

/// A supported language. The tree-sitter language object is created on demand;
/// `tree_sitter::Language` is a cheap handle and not `Sync`, so it is not stored
/// in a global static.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LangSpec {
    pub id: &'static str,
    pub extensions: &'static [&'static str],
}

/// All supported languages.
pub const ALL: &[LangSpec] = &[
    LangSpec {
        id: "rust",
        extensions: &["rs"],
    },
    LangSpec {
        id: "python",
        extensions: &["py", "pyi"],
    },
    LangSpec {
        id: "typescript",
        extensions: &["ts", "mts", "cts"],
    },
    LangSpec {
        id: "tsx",
        extensions: &["tsx"],
    },
    LangSpec {
        id: "javascript",
        extensions: &["js", "mjs", "cjs", "jsx"],
    },
    LangSpec {
        id: "nix",
        extensions: &["nix"],
    },
];

impl LangSpec {
    /// Build the tree-sitter language handle.
    pub fn language(&self) -> Language {
        match self.id {
            "rust" => tree_sitter_rust::LANGUAGE.into(),
            "python" => tree_sitter_python::LANGUAGE.into(),
            "typescript" | "tsx" => {
                // The TypeScript crate ships two grammars; TSX needs its own.
                if self.id == "tsx" {
                    tree_sitter_typescript::LANGUAGE_TSX.into()
                } else {
                    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
                }
            }
            "javascript" => tree_sitter_javascript::LANGUAGE.into(),
            "nix" => tree_sitter_nix::LANGUAGE.into(),
            other => unreachable!("unsupported language id {other}"),
        }
    }

    /// The tag query for this language.
    pub fn tags(&self) -> &'static str {
        match self.id {
            "rust" => RUST_TAGS,
            "python" => PYTHON_TAGS,
            "typescript" | "tsx" => TYPESCRIPT_TAGS,
            "javascript" => JAVASCRIPT_TAGS,
            "nix" => NIX_TAGS,
            other => unreachable!("unsupported language id {other}"),
        }
    }

    /// Language id for a grammar-independent grouping (tsx -> typescript).
    pub fn family(&self) -> &'static str {
        if self.id == "tsx" {
            "typescript"
        } else {
            self.id
        }
    }
}

/// Detect the language of a file from its extension.
pub fn detect(path: &Path) -> Option<LangSpec> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    ALL.iter()
        .copied()
        .find(|spec| spec.extensions.contains(&extension.as_str()))
}

/// Look up a language by id.
pub fn by_id(id: &str) -> Option<LangSpec> {
    ALL.iter().copied().find(|spec| spec.id == id)
}

// Tag queries follow the tree-sitter `tags.scm` convention: definitions carry a
// `@definition.<kind>` capture plus a `@name` capture; outgoing references use
// `@reference.call` or `@reference.import`. Resolution is name-based and
// deliberately precision-first (see `resolve.rs`).

const RUST_TAGS: &str = r#"
(function_item name: (identifier) @name) @definition.function
(struct_item name: (type_identifier) @name) @definition.struct
(enum_item name: (type_identifier) @name) @definition.enum
(trait_item name: (type_identifier) @name) @definition.trait
(impl_item type: (type_identifier) @name) @definition.impl
(union_item name: (type_identifier) @name) @definition.struct
(mod_item name: (identifier) @name) @definition.module
(const_item name: (identifier) @name) @definition.const
(static_item name: (identifier) @name) @definition.static
(type_item name: (type_identifier) @name) @definition.type
(macro_definition name: (identifier) @name) @definition.macro
(call_expression function: (identifier) @name) @reference.call
(call_expression function: (field_expression field: (field_identifier) @name)) @reference.call
(call_expression function: (scoped_identifier name: (identifier) @name) @path) @reference.call
(use_declaration argument: (identifier) @name) @reference.import
(use_declaration argument: (scoped_identifier name: (identifier) @name)) @reference.import
"#;

const PYTHON_TAGS: &str = r#"
(function_definition name: (identifier) @name) @definition.function
(class_definition name: (identifier) @name) @definition.class
(call function: (identifier) @name) @reference.call
(call function: (attribute attribute: (identifier) @name)) @reference.call
(import_statement name: (dotted_name) @name) @reference.import
(import_from_statement module_name: (dotted_name) @name) @reference.import
"#;

const TYPESCRIPT_TAGS: &str = r#"
(function_declaration name: (identifier) @name) @definition.function
(class_declaration name: (type_identifier) @name) @definition.class
(interface_declaration name: (type_identifier) @name) @definition.interface
(enum_declaration name: (identifier) @name) @definition.enum
(type_alias_declaration name: (type_identifier) @name) @definition.type
(method_definition name: (property_identifier) @name) @definition.method
(variable_declarator name: (identifier) @name value: (arrow_function)) @definition.function
(variable_declarator name: (identifier) @name value: (function_expression)) @definition.function
(call_expression function: (identifier) @name) @reference.call
(call_expression function: (member_expression property: (property_identifier) @name)) @reference.call
(import_statement source: (string) @name) @reference.import
"#;

const JAVASCRIPT_TAGS: &str = r#"
(function_declaration name: (identifier) @name) @definition.function
(class_declaration name: (identifier) @name) @definition.class
(method_definition name: (property_identifier) @name) @definition.method
(variable_declarator name: (identifier) @name value: (arrow_function)) @definition.function
(variable_declarator name: (identifier) @name value: (function_expression)) @definition.function
(call_expression function: (identifier) @name) @reference.call
(call_expression function: (member_expression property: (property_identifier) @name)) @reference.call
(import_statement source: (string) @name) @reference.import
"#;

// Nix bindings are classified from their value by the parser. Only statically
// named bindings and literal imports are retained; no Nix code is evaluated.
const NIX_TAGS: &str = r#"
(binding attrpath: (attrpath) @name) @definition.const
(inherited_attrs attr: [(identifier) (string_expression)] @name @definition.const)
(apply_expression function: (variable_expression name: (identifier) @name)) @reference.call
(apply_expression function: (select_expression) @name) @reference.call
(apply_expression function: (parenthesized_expression expression: [(variable_expression) (select_expression)] @name)) @reference.call
((apply_expression
  function: (variable_expression name: (identifier) @_import)
  argument: [(path_expression) (spath_expression) (string_expression)] @name) @reference.import
 (#eq? @_import "import"))
((apply_expression
  function: (select_expression
    expression: (variable_expression name: (identifier) @_builtins)
    attrpath: (attrpath . (identifier) @_import .))
  argument: [(path_expression) (spath_expression) (string_expression)] @name) @reference.import
 (#eq? @_builtins "builtins")
 (#eq? @_import "import"))
((binding
  attrpath: (attrpath . (identifier) @_imports .)
  expression: (list_expression element: [(path_expression) (spath_expression) (string_expression)] @name)) @reference.import
 (#eq? @_imports "imports"))
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_nix_files_case_insensitively() {
        for path in ["flake.nix", "modules/service.nix", "MODULE.NIX"] {
            assert_eq!(detect(Path::new(path)), by_id("nix"));
        }
        assert_eq!(by_id("nix").expect("Nix language").family(), "nix");
    }
}
