//! AST extraction: run the per-language tag query and turn matches into
//! definitions, call sites and imports.

use std::collections::HashSet;

use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

use crate::error::{Error, Result};
use crate::lang::LangSpec;
use crate::model::{RefKind, SymbolKind};

/// A definition found in one file, with byte ranges used for nesting.
#[derive(Debug, Clone)]
pub struct ParsedSymbol {
    pub name: String,
    pub kind: SymbolKind,
    pub start_line: u32,
    pub end_line: u32,
    pub start_byte: usize,
    pub end_byte: usize,
    /// Name of the innermost enclosing definition, if any.
    pub parent: Option<String>,
    /// `parent::name` when nested, otherwise `name`.
    pub qualified: String,
}

/// An outgoing call site or import.
#[derive(Debug, Clone)]
pub struct ParsedRef {
    pub name: String,
    /// Full scoped path for calls like `Store::open`, used for a qualified-first
    /// resolution attempt before falling back to the bare name.
    pub path: Option<String>,
    pub kind: RefKind,
    pub line: u32,
    pub byte: usize,
    /// Name of the innermost enclosing definition, if any.
    pub from_symbol: Option<String>,
}

/// Everything extracted from one file.
#[derive(Debug, Default, Clone)]
pub struct ParsedFile {
    pub symbols: Vec<ParsedSymbol>,
    pub refs: Vec<ParsedRef>,
}

/// Parse `source` with the grammar and tag query of `spec`.
pub fn parse(source: &str, spec: LangSpec) -> Result<ParsedFile> {
    let language = spec.language();
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .map_err(|error| Error::Invalid(format!("cannot load grammar for {}: {error}", spec.id)))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| Error::Invalid(format!("parser returned no tree for {}", spec.id)))?;
    let query = Query::new(&language, spec.tags()).map_err(|error| {
        Error::Invalid(format!("tag query for {} is invalid: {error}", spec.id))
    })?;

    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut symbols: Vec<ParsedSymbol> = Vec::new();
    let mut refs: Vec<ParsedRef> = Vec::new();
    // Two patterns can match the same node; keep one definition per start byte.
    let mut seen_symbols: HashSet<usize> = HashSet::new();

    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    while let Some(query_match) = matches.next() {
        let mut definition: Option<(SymbolKind, Node<'_>)> = None;
        let mut reference: Option<(RefKind, Node<'_>)> = None;
        let mut name: Option<Node<'_>> = None;
        let mut path: Option<Node<'_>> = None;

        for capture in query_match.captures {
            let capture_name = capture_names[capture.index as usize];
            let node = capture.node;
            if let Some(kind) = capture_name.strip_prefix("definition.") {
                if let Some(kind) = SymbolKind::parse(kind) {
                    definition = Some((kind, node));
                }
            } else if let Some(kind) = capture_name.strip_prefix("reference.") {
                let kind = match kind {
                    "call" => Some(RefKind::Call),
                    "import" => Some(RefKind::Import),
                    _ => None,
                };
                if let Some(kind) = kind {
                    reference = Some((kind, node));
                }
            } else if capture_name == "name" {
                name = Some(node);
            } else if capture_name == "path" {
                path = Some(node);
            }
        }

        let Some(name_node) = name else { continue };
        let text = match source.get(name_node.byte_range()) {
            Some(text) => text,
            None => continue,
        };

        if let Some((kind, node)) = definition {
            let name = text.to_string();
            if name.is_empty() {
                continue;
            }
            let start_line = node.start_position().row as u32 + 1;
            let end_line = node.end_position().row as u32 + 1;
            if !seen_symbols.insert(node.start_byte()) {
                continue;
            }
            symbols.push(ParsedSymbol {
                name,
                kind,
                start_line,
                end_line,
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
                parent: None,
                qualified: String::new(),
            });
        } else if let Some((kind, _node)) = reference {
            let name = normalize_reference_name(kind, text);
            if name.is_empty() {
                continue;
            }
            let path = if kind == RefKind::Call {
                path.and_then(|node| source.get(node.byte_range()))
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            } else {
                None
            };
            refs.push(ParsedRef {
                name,
                path,
                kind,
                line: name_node.start_position().row as u32 + 1,
                byte: name_node.start_byte(),
                from_symbol: None,
            });
        }
    }

    assign_nesting(&mut symbols);
    assign_enclosing(&symbols, &mut refs);

    Ok(ParsedFile { symbols, refs })
}

/// Import strings arrive quoted (`"./mod"`); definitions are identifiers.
fn normalize_reference_name(kind: RefKind, text: &str) -> String {
    let trimmed = text.trim();
    if kind == RefKind::Import {
        trimmed.trim_matches(['"', '\'', '`']).to_string()
    } else {
        trimmed.to_string()
    }
}

/// Sort definitions and compute the innermost enclosing definition for each.
fn assign_nesting(symbols: &mut [ParsedSymbol]) {
    symbols.sort_by(|left, right| {
        left.start_byte
            .cmp(&right.start_byte)
            .then(right.end_byte.cmp(&left.end_byte))
    });

    // Stack of indexes with an open byte range.
    let mut stack: Vec<usize> = Vec::new();
    for index in 0..symbols.len() {
        let start = symbols[index].start_byte;
        while let Some(&top) = stack.last() {
            if symbols[top].end_byte <= start {
                stack.pop();
            } else {
                break;
            }
        }
        let parent_index = stack.last().copied();
        let parent = parent_index.map(|top| symbols[top].name.clone());
        let parent_kind = parent_index.map(|top| symbols[top].kind);
        if symbols[index].kind == SymbolKind::Function
            && parent_kind.map(is_container).unwrap_or(false)
        {
            symbols[index].kind = SymbolKind::Method;
        }
        symbols[index].parent = parent.clone();
        symbols[index].qualified = match parent {
            Some(parent) => format!("{parent}::{}", symbols[index].name),
            None => symbols[index].name.clone(),
        };
        stack.push(index);
    }
}

/// Definitions that make enclosed functions methods rather than functions.
fn is_container(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class
            | SymbolKind::Struct
            | SymbolKind::Trait
            | SymbolKind::Interface
            | SymbolKind::Impl
    )
}

/// Point each reference at the innermost definition that contains it.
///
/// AST node ranges are laminar (nested or disjoint), so a single sweep over
/// references sorted by byte position with a stack of open symbols is enough:
/// linear instead of a quadratic scan per reference.
fn assign_enclosing(symbols: &[ParsedSymbol], refs: &mut [ParsedRef]) {
    let mut order: Vec<usize> = (0..refs.len()).collect();
    order.sort_by_key(|&index| refs[index].byte);
    let mut stack: Vec<usize> = Vec::new();
    let mut cursor = 0usize;
    for index in order {
        let byte = refs[index].byte;
        while cursor < symbols.len() && symbols[cursor].start_byte <= byte {
            stack.push(cursor);
            cursor += 1;
        }
        while let Some(&top) = stack.last() {
            if symbols[top].end_byte <= byte {
                stack.pop();
            } else {
                break;
            }
        }
        refs[index].from_symbol = stack.last().map(|&top| symbols[top].name.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang;

    fn parse_lang(id: &str, source: &str) -> ParsedFile {
        let spec = lang::by_id(id).expect("known language");
        parse(source, spec).expect("parses")
    }

    #[test]
    fn every_tag_query_compiles_for_its_grammar() {
        for spec in lang::ALL {
            let language = spec.language();
            if let Err(error) = Query::new(&language, spec.tags()) {
                panic!("{} tag query is invalid: {error}", spec.id);
            }
        }
    }

    #[test]
    fn rust_extracts_definitions_calls_and_imports() {
        let source = r#"
use std::collections::HashMap;

pub struct Store {
    items: HashMap<String, u32>,
}

impl Store {
    pub fn insert(&mut self, key: &str) {
        self.record(key);
    }
    fn record(&mut self, key: &str) {}
}

pub fn build_store() -> Store {
    let mut store = Store { items: HashMap::new() };
    store.insert("a");
    build_store();
    Store
}
"#;
        let parsed = parse_lang("rust", source);
        let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Store"), "symbols: {names:?}");
        assert!(names.contains(&"insert"), "symbols: {names:?}");
        assert!(names.contains(&"record"), "symbols: {names:?}");
        assert!(names.contains(&"build_store"), "symbols: {names:?}");

        let insert = parsed
            .symbols
            .iter()
            .find(|symbol| symbol.name == "insert")
            .expect("insert symbol");
        assert_eq!(insert.kind, SymbolKind::Method);
        assert_eq!(insert.parent.as_deref(), Some("Store"));
        assert_eq!(insert.qualified, "Store::insert");

        let calls: Vec<&str> = parsed
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::Call)
            .map(|r| r.name.as_str())
            .collect();
        assert!(calls.contains(&"record"), "calls: {calls:?}");
        assert!(calls.contains(&"insert"), "calls: {calls:?}");
        assert!(calls.contains(&"new"), "calls: {calls:?}");

        let record_call = parsed
            .refs
            .iter()
            .find(|r| r.kind == RefKind::Call && r.name == "record")
            .expect("record call");
        assert_eq!(record_call.from_symbol.as_deref(), Some("insert"));

        let imports: Vec<&str> = parsed
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::Import)
            .map(|r| r.name.as_str())
            .collect();
        assert!(imports.contains(&"HashMap"), "imports: {imports:?}");
    }

    #[test]
    fn python_extracts_classes_methods_and_calls() {
        let source = r#"
class Greeter:
    def hello(self):
        return self.render("hi")

    def render(self, text):
        return text

def main():
    greeter = Greeter()
    greeter.hello()
"#;
        let parsed = parse_lang("python", source);
        let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Greeter"));
        assert!(names.contains(&"hello"));
        assert!(names.contains(&"render"));
        assert!(names.contains(&"main"));

        let hello = parsed
            .symbols
            .iter()
            .find(|symbol| symbol.name == "hello")
            .expect("hello");
        assert_eq!(hello.kind, SymbolKind::Method);
        assert_eq!(hello.parent.as_deref(), Some("Greeter"));
        assert_eq!(hello.qualified, "Greeter::hello");

        let render_call = parsed
            .refs
            .iter()
            .find(|r| r.kind == RefKind::Call && r.name == "render")
            .expect("render call");
        assert_eq!(render_call.from_symbol.as_deref(), Some("hello"));
    }

    #[test]
    fn typescript_extracts_interfaces_arrow_functions_and_calls() {
        let source = r#"
import { helper } from "./helper";

export interface Options {
    retries: number;
}

export const load = async (options: Options) => {
    await helper(options);
    return run(options);
};

export function run(options: Options) {
    return helper(options);
}
"#;
        let parsed = parse_lang("typescript", source);
        let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Options"), "symbols: {names:?}");
        assert!(names.contains(&"load"), "symbols: {names:?}");
        assert!(names.contains(&"run"), "symbols: {names:?}");

        let load = parsed
            .symbols
            .iter()
            .find(|symbol| symbol.name == "load")
            .expect("load");
        assert_eq!(load.kind, SymbolKind::Function);

        let helper_calls: Vec<_> = parsed
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::Call && r.name == "helper")
            .collect();
        assert_eq!(helper_calls.len(), 2, "refs: {:?}", parsed.refs);

        let imports: Vec<&str> = parsed
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::Import)
            .map(|r| r.name.as_str())
            .collect();
        assert!(imports.contains(&"./helper"), "imports: {imports:?}");
    }
}
