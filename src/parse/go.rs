//! Go declarations and lexical call evidence; package resolution happens later.

use std::collections::HashMap;

use tree_sitter::Node;

use super::{ParsedFile, ParsedRef, ParsedSymbol};
use crate::error::{Error, Result};
use crate::model::{RefKind, SymbolKind};

#[derive(Default)]
struct Scope {
    parent: Option<usize>,
    depth: usize,
    bindings: HashMap<String, usize>,
}

/// Read one literal module directive without executing Go or following metadata.
pub(crate) fn module_path(source: &str) -> Option<String> {
    if source.len() > 64 * 1024 {
        return None;
    }
    let bytes = source.as_bytes();
    let mut lines = vec![Vec::<&str>::new()];
    let mut position = 0;
    while position < bytes.len() {
        match bytes[position] {
            b'\n' => {
                lines.push(Vec::new());
                position += 1;
            }
            byte if byte.is_ascii_whitespace() => position += 1,
            b'/' if bytes.get(position + 1) == Some(&b'/') => {
                while position < bytes.len() && bytes[position] != b'\n' {
                    position += 1;
                }
            }
            b'/' if bytes.get(position + 1) == Some(&b'*') => {
                position += 2;
                while position + 1 < bytes.len() && &bytes[position..position + 2] != b"*/" {
                    if bytes[position] == b'\n' {
                        lines.push(Vec::new());
                    }
                    position += 1;
                }
                if position + 1 >= bytes.len() {
                    return None;
                }
                position += 2;
            }
            quote @ (b'"' | b'`') => {
                let start = position;
                position += 1;
                while position < bytes.len() && bytes[position] != quote {
                    // Multiline and escaped paths are outside the supported subset.
                    if matches!(bytes[position], b'\n' | b'\r') {
                        return None;
                    }
                    if quote == b'"' && bytes[position] == b'\\' {
                        position += 1;
                    }
                    position += 1;
                }
                if position >= bytes.len() {
                    return None;
                }
                position += 1;
                lines.last_mut()?.push(&source[start..position]);
            }
            b'(' | b')' => {
                lines.last_mut()?.push(&source[position..position + 1]);
                position += 1;
            }
            _ => {
                let start = position;
                while position < bytes.len()
                    && !bytes[position].is_ascii_whitespace()
                    && !matches!(bytes[position], b'(' | b')' | b'"' | b'`')
                    && !(bytes[position] == b'/'
                        && matches!(bytes.get(position + 1), Some(b'/' | b'*')))
                {
                    position += 1;
                }
                lines.last_mut()?.push(&source[start..position]);
            }
        }
    }
    let mut depth = 0usize;
    let mut module = None;
    for line in lines {
        if depth == 0 && line.first() == Some(&"module") {
            let [_, path] = line.as_slice() else {
                return None;
            };
            if module.is_some() {
                return None;
            }
            let path = path
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .or_else(|| path.strip_prefix('`').and_then(|s| s.strip_suffix('`')))
                .unwrap_or(path);
            if path.is_empty()
                || path.len() > 4096
                || path.starts_with(['/', '.'])
                || !path
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"/._~-".contains(&byte))
                || path.split('/').any(|part| matches!(part, "" | "." | ".."))
            {
                return None;
            }
            module = Some(path.to_owned());
        }
        for token in line {
            match token {
                "(" => {
                    depth += 1;
                    if depth > 256 {
                        return None;
                    }
                }
                ")" => depth = depth.checked_sub(1)?,
                _ => {}
            }
        }
    }
    (depth == 0).then_some(module).flatten()
}

pub(super) fn extract(source: &str, root: Node<'_>) -> Result<ParsedFile> {
    let mut result = ParsedFile {
        has_errors: root.has_error(),
        ..ParsedFile::default()
    };
    let mut cursor = root.walk();
    let package = root
        .named_children(&mut cursor)
        .find(|node| node.kind() == "package_clause")
        .and_then(|node| node.named_child(0))
        .map(|node| text(node, source))
        .unwrap_or_default();
    check_name(package)?;
    let mut scopes = vec![Scope::default()];
    let mut call_scopes = Vec::new();
    let mut stack = vec![(root, 0, None::<String>)];
    let mut visited = 0usize;
    while let Some((node, mut scope, mut owner)) = stack.pop() {
        visited += 1;
        if visited > 1_000_000 || result.symbols.len() + result.refs.len() >= 100_000 {
            return Err(Error::Invalid("file extraction budget exceeded".into()));
        }
        let enclosing_scope = scope;
        if is_scope(node.kind()) {
            let depth = scopes[enclosing_scope].depth + 1;
            if depth > 256 {
                return Err(Error::Invalid("file nesting budget exceeded".into()));
            }
            scope = scopes.len();
            scopes.push(Scope {
                parent: Some(enclosing_scope),
                depth,
                ..Scope::default()
            });
        }
        match node.kind() {
            "package_clause" => {
                if !package.is_empty() {
                    result.symbols.push(symbol(
                        node,
                        package,
                        SymbolKind::Module,
                        None,
                        package.to_owned(),
                        source,
                    )?);
                }
            }
            "function_declaration" | "method_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = text(name_node, source);
                    let receiver = node
                        .child_by_field_name("receiver")
                        .and_then(|list| list.named_child(0))
                        .and_then(|parameter| parameter.child_by_field_name("type"))
                        .and_then(receiver_base_type);
                    // Receiver type arguments declare fresh method type parameters.
                    if let Some(arguments) =
                        receiver.and_then(|receiver| receiver.child_by_field_name("type_arguments"))
                    {
                        let mut cursor = arguments.walk();
                        for argument in arguments.named_children(&mut cursor) {
                            if argument.named_child_count() == 1
                                && let Some(name) = argument.named_child(0)
                                && name.kind() == "type_identifier"
                            {
                                bind(&mut scopes, scope, text(name, source), 0)?;
                            }
                        }
                    }
                    let receiver = receiver.and_then(|receiver| {
                        if receiver.kind() == "generic_type" {
                            receiver
                                .child_by_field_name("type")
                                .filter(|node| node.kind() == "type_identifier")
                        } else {
                            Some(receiver)
                        }
                    });
                    let parent = receiver
                        .map(|receiver| format!("{package}::{}", text(receiver, source)))
                        .unwrap_or_else(|| package.to_owned());
                    let qualified = format!("{parent}::{name}");
                    let kind = if node.kind() == "method_declaration" {
                        SymbolKind::Method
                    } else {
                        SymbolKind::Function
                    };
                    result.symbols.push(symbol(
                        node,
                        name,
                        kind,
                        Some(parent),
                        qualified.clone(),
                        source,
                    )?);
                    owner = Some(qualified);
                }
            }
            "type_spec" | "type_alias" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = text(name_node, source);
                    if enclosing_scope != 0 {
                        bind(&mut scopes, enclosing_scope, name, name_node.end_byte())?;
                    }
                    let kind = match node.child_by_field_name("type").map(|node| node.kind()) {
                        Some("struct_type") if node.kind() == "type_spec" => SymbolKind::Struct,
                        Some("interface_type") if node.kind() == "type_spec" => {
                            SymbolKind::Interface
                        }
                        _ => SymbolKind::TypeAlias,
                    };
                    let parent = owner.as_deref().unwrap_or(package).to_owned();
                    let qualified = format!("{parent}::{name}");
                    result.symbols.push(symbol(
                        node,
                        name,
                        kind,
                        Some(parent),
                        qualified.clone(),
                        source,
                    )?);
                    owner = Some(qualified);
                }
            }
            "method_elem" => {
                if let (Some(name_node), Some(parent)) =
                    (node.child_by_field_name("name"), owner.as_ref())
                {
                    let name = text(name_node, source);
                    result.symbols.push(symbol(
                        node,
                        name,
                        SymbolKind::Method,
                        Some(parent.clone()),
                        format!("{parent}::{name}"),
                        source,
                    )?);
                }
            }
            "var_spec" | "const_spec" => {
                let mut cursor = node.walk();
                for name_node in node.children_by_field_name("name", &mut cursor) {
                    let name = text(name_node, source);
                    if scope == 0 {
                        result.symbols.push(symbol(
                            node,
                            name,
                            if node.kind() == "var_spec" {
                                SymbolKind::Static
                            } else {
                                SymbolKind::Const
                            },
                            Some(package.to_owned()),
                            format!("{package}::{name}"),
                            source,
                        )?);
                    } else {
                        bind(&mut scopes, scope, name, node.end_byte())?;
                    }
                }
            }
            "parameter_declaration"
            | "variadic_parameter_declaration"
            | "type_parameter_declaration" => {
                // Function-type parameter names do not bind outside that type.
                let mut parent = node.parent();
                let mut declaration_scope = false;
                while let Some(ancestor) = parent {
                    if ancestor.kind() == "function_type" || ancestor.kind() == "method_elem" {
                        break;
                    }
                    if is_scope(ancestor.kind()) {
                        declaration_scope = true;
                        break;
                    }
                    parent = ancestor.parent();
                }
                if declaration_scope {
                    let mut cursor = node.walk();
                    for name_node in node.children_by_field_name("name", &mut cursor) {
                        bind(&mut scopes, scope, text(name_node, source), 0)?;
                    }
                }
            }
            "short_var_declaration" | "range_clause" | "receive_statement" => {
                if (node.kind() == "short_var_declaration" || text(node, source).contains(":="))
                    && let Some(left) = node.child_by_field_name("left")
                {
                    let mut cursor = left.walk();
                    for name in left.named_children(&mut cursor) {
                        if name.kind() == "identifier" {
                            bind(&mut scopes, scope, text(name, source), node.end_byte())?;
                        }
                    }
                }
            }
            "type_switch_statement" => {
                if let Some(alias) = node.child_by_field_name("alias") {
                    let mut cursor = alias.walk();
                    let start = node
                        .child_by_field_name("value")
                        .map_or(alias.end_byte(), |value| value.end_byte());
                    for name in alias.named_children(&mut cursor) {
                        if name.kind() == "identifier" {
                            bind(&mut scopes, scope, text(name, source), start)?;
                        }
                    }
                }
            }
            "import_spec" => {
                if let Some(path) = node.child_by_field_name("path") {
                    let literal = text(path, source);
                    let name = literal
                        .strip_prefix('"')
                        .and_then(|value| value.strip_suffix('"'))
                        .or_else(|| literal.strip_prefix('`').and_then(|v| v.strip_suffix('`')))
                        .filter(|value| !value.contains(['\\', '\n', '\r', '\0']));
                    check_name(name.unwrap_or("<dynamic>"))?;
                    let alias = node.child_by_field_name("name").map(|n| text(n, source));
                    if let Some(alias) = alias {
                        check_name(alias)?;
                    }
                    result.refs.push(ParsedRef {
                        name: name.unwrap_or("<dynamic>").to_owned(),
                        path: alias.map(str::to_owned),
                        kind: RefKind::Import,
                        line: path.start_position().row as u32 + 1,
                        byte: path.start_byte(),
                        from_symbol: None,
                        blocked: name.is_none().then(|| "unsupported_import_literal".into()),
                    });
                }
            }
            "call_expression" => {
                if let Some(function) = node.child_by_field_name("function") {
                    let (name, receiver) = match function.kind() {
                        "identifier" => (text(function, source), None),
                        "selector_expression" => {
                            let name = function
                                .child_by_field_name("field")
                                .map(|node| text(node, source))
                                .unwrap_or("<dynamic>");
                            let receiver = function
                                .child_by_field_name("operand")
                                .filter(|node| node.kind() == "identifier")
                                .map(|node| text(node, source));
                            (name, Some(receiver))
                        }
                        _ => ("<dynamic>", Some(None)),
                    };
                    check_name(name)?;
                    if let Some(Some(receiver)) = receiver {
                        check_name(receiver)?;
                    }
                    let path = match receiver {
                        None => Some(format!("@go:{}", serde_json::to_string(&(package, name))?)),
                        Some(Some(receiver)) => Some(format!(
                            "@go_selector:{}",
                            serde_json::to_string(&(package, receiver, name))?
                        )),
                        Some(None) => None,
                    };
                    call_scopes.push((
                        result.refs.len(),
                        scope,
                        receiver.flatten().unwrap_or(name).to_owned(),
                    ));
                    result.refs.push(ParsedRef {
                        name: name.to_owned(),
                        path,
                        kind: RefKind::Call,
                        line: function.start_position().row as u32 + 1,
                        byte: function.start_byte(),
                        from_symbol: owner.clone(),
                        blocked: (receiver == Some(None)).then(|| "dynamic_receiver".into()),
                    });
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push((child, scope, owner.clone()));
        }
    }
    for (index, mut scope, name) in call_scopes {
        let reference = &mut result.refs[index];
        if reference.blocked.is_some() {
            continue;
        }
        if package.is_empty() {
            reference.blocked = Some("missing_package".into());
            continue;
        }
        loop {
            if scopes[scope]
                .bindings
                .get(&name)
                .is_some_and(|start| *start <= reference.byte)
            {
                reference.blocked = Some("shadowed_binding".into());
                break;
            }
            let Some(parent) = scopes[scope].parent else {
                break;
            };
            scope = parent;
        }
    }
    if result.symbols.len() + result.refs.len() > 100_000 {
        return Err(Error::Invalid("file extraction budget exceeded".into()));
    }
    Ok(result)
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    source.get(node.byte_range()).unwrap_or_default()
}

fn check_name(name: &str) -> Result<()> {
    if name.len() > 4096 {
        Err(Error::Invalid(
            "symbol/reference name exceeds 4096 bytes".into(),
        ))
    } else {
        Ok(())
    }
}

fn bind(scopes: &mut [Scope], scope: usize, name: &str, start: usize) -> Result<()> {
    check_name(name)?;
    if name != "_" {
        scopes[scope]
            .bindings
            .entry(name.to_owned())
            .and_modify(|previous| *previous = (*previous).min(start))
            .or_insert(start);
    }
    Ok(())
}

fn is_scope(kind: &str) -> bool {
    matches!(
        kind,
        "function_declaration"
            | "method_declaration"
            | "func_literal"
            | "block"
            | "if_statement"
            | "for_statement"
            | "expression_switch_statement"
            | "type_switch_statement"
            | "select_statement"
            | "expression_case"
            | "type_case"
            | "communication_case"
            | "default_case"
            | "type_spec"
            | "type_alias"
    )
}

fn receiver_base_type(mut node: Node<'_>) -> Option<Node<'_>> {
    for _ in 0..32 {
        match node.kind() {
            "type_identifier" | "generic_type" => return Some(node),
            "pointer_type" | "parenthesized_type" => node = node.named_child(0)?,
            _ => return None,
        }
    }
    None
}

fn symbol(
    node: Node<'_>,
    name: &str,
    kind: SymbolKind,
    parent: Option<String>,
    qualified: String,
    source: &str,
) -> Result<ParsedSymbol> {
    check_name(name)?;
    check_name(&qualified)?;
    let end = node
        .child_by_field_name("body")
        .map_or(node.end_byte(), |body| body.start_byte());
    Ok(ParsedSymbol {
        name: name.to_owned(),
        kind,
        start_line: node.start_position().row as u32 + 1,
        end_line: node.end_position().row as u32 + 1,
        start_column: node.start_position().column as u32 + 1,
        end_column: node.end_position().column as u32 + 1,
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        parent,
        qualified,
        signature: source
            .get(node.start_byte()..end)
            .unwrap_or_default()
            .chars()
            .take(512)
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
        exported: name.chars().next().is_some_and(char::is_uppercase),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> ParsedFile {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        extract(source, tree.root_node()).unwrap()
    }

    #[test]
    fn module_metadata_uses_only_a_unique_top_level_literal() {
        assert_eq!(
            module_path(
                "/*\nmodule ignored.test/wrong\n*/\nmodule \"example.test/app\" // tail\nrequire (\n module fake\n)\n"
            ),
            Some("example.test/app".into())
        );
        assert_eq!(
            module_path("module `example.test/app`\n"),
            Some("example.test/app".into())
        );
        assert_eq!(
            module_path("module/* comment */example.test/app\n"),
            Some("example.test/app".into())
        );
        for source in [
            "module example.test/first\nmodule example.test/second\n",
            "module example.test/app\n/* unclosed\n",
            "module \"example.test/app\n",
            "module \"example.test/\\x61pp\"\n",
            "module example.test/../outside\n",
            "module ../outside\n",
            "module example.test/app extra\n",
            "module\nexample.test/app\n",
            "require (\nmodule example.test/not-top-level\n)\n",
            "module example.test/app\nrequire (\n",
        ] {
            assert_eq!(module_path(source), None, "{source}");
        }
        assert_eq!(module_path(&" ".repeat(64 * 1024 + 1)), None);
    }

    #[test]
    fn declarations_imports_and_call_owners() {
        let parsed = parse(
            r#"package worker
import (
    "example.test/app/queue"
    alias "example.test/app/util"
)
type Item struct { Value int }
type Runner interface { Run() }
type Count int
type Alias = Item
var Callback func()
const Limit = 1
func Start() { helper(); queue.Work(); alias.Do() }
func helper() {}
func (item *Item) Run() { helper(); item.Run() }
"#,
        );
        assert!(!parsed.has_errors);
        for (name, kind) in [
            ("worker", SymbolKind::Module),
            ("worker::Start", SymbolKind::Function),
            ("worker::Item", SymbolKind::Struct),
            ("worker::Runner", SymbolKind::Interface),
            ("worker::Runner::Run", SymbolKind::Method),
            ("worker::Item::Run", SymbolKind::Method),
            ("worker::Count", SymbolKind::TypeAlias),
            ("worker::Alias", SymbolKind::TypeAlias),
            ("worker::Callback", SymbolKind::Static),
            ("worker::Limit", SymbolKind::Const),
        ] {
            assert!(
                parsed
                    .symbols
                    .iter()
                    .any(|s| s.qualified == name && s.kind == kind),
                "{name}"
            );
        }
        assert!(parsed.refs.iter().any(|r| r.kind == RefKind::Import
            && r.name.ends_with("/util")
            && r.path.as_deref() == Some("alias")));
        assert!(parsed.refs.iter().any(|r| r.name == "helper"
            && r.from_symbol.as_deref() == Some("worker::Item::Run")
            && r.blocked.is_none()));
        assert!(
            parsed
                .refs
                .iter()
                .any(|r| r.name == "Run" && r.blocked.as_deref() == Some("shadowed_binding"))
        );
    }

    #[test]
    fn locals_parameters_and_dynamic_receivers_block_package_lookup() {
        let parsed = parse(
            r#"package app
func Work() {}
func run(Work func(), receiver interface { Work() }) {
    Work(); receiver.Work(); factory().Work()
}
func other() {
    Work()
    { Work := func() {}; Work() }
    Work()
    if Work := func() {}; true { Work() }
    for Work := range []func(){} { Work() }
    var Work = func() {}; Work()
}
"#,
        );
        assert!(!parsed.has_errors);
        let work: Vec<_> = parsed.refs.iter().filter(|r| r.name == "Work").collect();
        assert_eq!(work.iter().filter(|r| r.blocked.is_none()).count(), 2);
        assert_eq!(
            work.iter()
                .filter(|r| r.blocked.as_deref() == Some("shadowed_binding"))
                .count(),
            6
        );
        assert_eq!(
            work.iter()
                .filter(|r| r.blocked.as_deref() == Some("dynamic_receiver"))
                .count(),
            1
        );
    }

    #[test]
    fn declaration_rhs_precedes_new_binding_and_function_types_do_not_leak() {
        let parsed = parse(
            r#"package app
func Work() func() { return nil }
var unused func(Work func())
func run() {
    Work := Work()
    Work()
}
func generic[Work any]() { Work() }
"#,
        );
        let work: Vec<_> = parsed.refs.iter().filter(|r| r.name == "Work").collect();
        assert_eq!(work.len(), 3);
        assert!(work[0].blocked.is_none());
        assert_eq!(work[1].blocked.as_deref(), Some("shadowed_binding"));
        assert_eq!(work[2].blocked.as_deref(), Some("shadowed_binding"));
    }

    #[test]
    fn generic_receiver_type_parameters_shadow_package_functions() {
        let parsed = parse(
            r#"package app
func Work(value int) int { return value }
func Other(value int) int { return value }
type Box[T any] struct { value T }
type Pair[A, B any] struct { first A; second B }
func (box Box[Work]) Value(value Work) Work { return Work(value) }
func (pair *Pair[Work, Other]) Values(first Work, second Other) {
    _ = Work(first)
    _ = Other(second)
}
func (Box[Work]) Unnamed(value Work) Work { return Work(value) }
func ordinary(box Box[int]) { Work(1); Other(2) }
"#,
        );
        assert!(!parsed.has_errors);
        let calls: Vec<_> = parsed
            .refs
            .iter()
            .filter(|reference| matches!(reference.name.as_str(), "Work" | "Other"))
            .collect();
        assert_eq!(calls.len(), 6);
        for reference in &calls[..4] {
            assert_eq!(reference.blocked.as_deref(), Some("shadowed_binding"));
        }
        for reference in &calls[4..] {
            assert!(reference.blocked.is_none());
        }
    }

    #[test]
    fn switch_select_and_closure_bindings_stay_in_their_scopes() {
        let parsed = parse(
            r#"package app
func Work() {}
func run(value any, channel chan func()) {
    switch Work := value.(type) { case func(): Work() }
    select { case Work := <-channel: Work(); default: Work() }
    closure := func(Work func()) { Work() }
    _ = closure
    Work()
}
"#,
        );
        assert!(!parsed.has_errors);
        let work: Vec<_> = parsed.refs.iter().filter(|r| r.name == "Work").collect();
        assert_eq!(work.len(), 5);
        assert_eq!(work.iter().filter(|r| r.blocked.is_none()).count(), 2);
        assert_eq!(
            work.iter()
                .filter(|r| r.blocked.as_deref() == Some("shadowed_binding"))
                .count(),
            3
        );
    }

    #[test]
    fn literals_and_identifier_limits_are_conservative() {
        let parsed = parse("package app\nimport alias \"example.test/\\x61\"\nfunc Run() {}\n");
        assert!(
            parsed
                .refs
                .iter()
                .any(|r| r.blocked.as_deref() == Some("unsupported_import_literal"))
        );
        let source = format!("package app\nfunc {}() {{}}\n", "x".repeat(4097));
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(&source, None).unwrap();
        assert!(extract(&source, tree.root_node()).is_err());
    }
}
