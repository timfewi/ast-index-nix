//! Static Nix extraction and conservative lexical call targets. Never evaluate
//! expressions, follow imports, or infer attributes supplied by `with`.

use tree_sitter::Node;

use crate::model::SymbolKind;

fn text<'s>(node: Node<'_>, source: &'s str) -> Option<&'s str> {
    source.get(node.byte_range())
}

pub(super) fn static_name(node: Node<'_>, source: &str) -> Option<String> {
    match node.kind() {
        "identifier" => text(node, source).map(str::to_string),
        "string_expression" => {
            let mut cursor = node.walk();
            if node
                .named_children(&mut cursor)
                .any(|n| n.kind() == "interpolation")
            {
                None
            } else {
                serde_json::from_str(text(node, source)?).ok()
            }
        }
        "attrpath" => {
            let mut cursor = node.walk();
            node.children_by_field_name("attr", &mut cursor)
                .map(|attr| static_name(attr, source))
                .collect::<Option<Vec<_>>>()
                .map(|parts| parts.join("."))
        }
        _ => None,
    }
}

fn unparenthesize(mut node: Node<'_>) -> Node<'_> {
    while node.kind() == "parenthesized_expression" {
        let Some(inner) = node.child_by_field_name("expression") else {
            break;
        };
        node = inner;
    }
    node
}

pub(super) fn binding_kind(node: Node<'_>) -> SymbolKind {
    match node.child_by_field_name("expression").map(unparenthesize) {
        Some(value) if value.kind() == "function_expression" => SymbolKind::Function,
        Some(value)
            if matches!(
                value.kind(),
                "attrset_expression" | "rec_attrset_expression"
            ) =>
        {
            SymbolKind::Module
        }
        _ => SymbolKind::Const,
    }
}

pub(super) fn literal_import(node: Node<'_>, source: &str) -> Option<String> {
    match node.kind() {
        "string_expression" => static_name(node, source),
        "spath_expression" => text(node, source).map(str::to_string),
        "path_expression" => {
            let mut cursor = node.walk();
            if node
                .named_children(&mut cursor)
                .any(|n| n.kind() == "interpolation")
            {
                None
            } else {
                text(node, source).map(str::to_string)
            }
        }
        _ => None,
    }
}

/// Return a unique binding visible to a bare call. Parameters and inherited
/// values shadow outer bindings, but do not provide statically known targets.
/// Selected calls and aliases are deliberately left unresolved.
pub(super) fn local_binding<'t>(node: Node<'t>, name: &str, source: &str) -> Option<Node<'t>> {
    if !matches!(node.kind(), "identifier" | "variable_expression") {
        return None;
    }
    let mut ancestor = node.parent();
    while let Some(scope) = ancestor {
        if scope.kind() == "function_expression" {
            if scope
                .child_by_field_name("universal")
                .and_then(|n| text(n, source))
                == Some(name)
            {
                return None;
            }
            if let Some(formals) = scope.child_by_field_name("formals") {
                let mut cursor = formals.walk();
                if formals
                    .children_by_field_name("formal", &mut cursor)
                    .any(|formal| {
                        formal
                            .child_by_field_name("name")
                            .and_then(|n| text(n, source))
                            == Some(name)
                    })
                {
                    return None;
                }
            }
        }
        if scope.kind() == "with_expression" {
            return None;
        }
        if matches!(
            scope.kind(),
            "let_expression" | "rec_attrset_expression" | "let_attrset_expression"
        ) {
            let mut cursor = scope.walk();
            let bindings = scope
                .named_children(&mut cursor)
                .find(|n| n.kind() == "binding_set");
            if let Some(bindings) = bindings {
                let mut candidate = None;
                let mut cursor = bindings.walk();
                for binding in bindings.named_children(&mut cursor) {
                    if binding.kind() == "binding" {
                        let attrpath = binding.child_by_field_name("attrpath")?;
                        let binding_name = static_name(attrpath, source)?;
                        // A dotted binding also shadows its root attribute.
                        if binding_name.split('.').next() == Some(name) {
                            if binding_name != name || candidate.is_some() {
                                return None;
                            }
                            candidate = Some(binding);
                        }
                    } else if let Some(attrs) = binding.child_by_field_name("attrs") {
                        let mut cursor = attrs.walk();
                        for attr in attrs.named_children(&mut cursor) {
                            if static_name(attr, source)?.as_str() == name {
                                return None;
                            }
                        }
                    }
                }
                if candidate.is_some() {
                    return candidate;
                }
            }
        }
        ancestor = scope.parent();
    }
    None
}
