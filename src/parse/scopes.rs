//! Conservative lexical bindings for the non-Nix grammars. Unknown values never
//! become call targets. This is syntactic scope analysis, not type inference.

use std::collections::HashMap;
use tree_sitter::Node;

use super::{ParsedRef, ParsedSymbol};
use crate::model::RefKind;

#[derive(Default)]
struct Scope {
    parent: Option<usize>,
    python_class: bool,
    function_boundary: bool,
    member_namespace: bool,
    bindings: HashMap<String, Binding>,
}

struct Binding {
    target: Option<usize>,
    reason: &'static str,
    import: Option<(String, String)>,
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    source.get(node.byte_range()).unwrap_or_default()
}

fn names(node: Node<'_>, source: &str) -> Vec<String> {
    let mut stack = vec![node];
    let mut result = Vec::new();
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier" || node.kind() == "shorthand_property_identifier_pattern" {
            result.push(text(node, source).to_string());
        } else {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                // Types/default values are not binding patterns.
                let default_value = matches!(
                    node.kind(),
                    "default_parameter"
                        | "typed_default_parameter"
                        | "optional_parameter"
                        | "required_parameter"
                ) && Some(child) == node.child_by_field_name("value");
                let default_right = node.kind() == "assignment_pattern"
                    && Some(child) == node.child_by_field_name("right");
                if Some(child) != node.child_by_field_name("type")
                    && !default_value
                    && !default_right
                {
                    stack.push(child);
                }
            }
        }
    }
    result
}

fn bind(scope: &mut Scope, name: String, target: Option<usize>, reason: &'static str) {
    scope
        .bindings
        .entry(name)
        .and_modify(|binding| {
            if binding.target != target || binding.import.is_some() {
                binding.target = None;
                binding.import = None;
                binding.reason = if reason == "shadowed" {
                    "shadowed"
                } else {
                    "ambiguous"
                };
            }
        })
        .or_insert(Binding {
            target,
            reason,
            import: None,
        });
}

pub(super) fn apply(
    root: Node<'_>,
    language: &str,
    source: &str,
    symbols: &[ParsedSymbol],
    refs: &mut [ParsedRef],
) {
    let definitions: HashMap<usize, &ParsedSymbol> =
        symbols.iter().map(|s| (s.start_byte, s)).collect();
    let mut scopes = vec![Scope::default()];
    let mut locations = HashMap::new();
    let mut assignments = Vec::new();
    let mut stack = vec![(root, 0usize)];
    while let Some((node, mut scope)) = stack.pop() {
        let kind = node.kind();
        // `var` declarations belong to the function, even when encountered in
        // a nested block. Lexical let/const declarations retain block ownership.
        let mut binding_scope = scope;
        if kind == "variable_declarator"
            && node
                .parent()
                .is_some_and(|p| p.kind() == "variable_declaration")
        {
            while !scopes[binding_scope].function_boundary {
                let Some(parent) = scopes[binding_scope].parent else {
                    break;
                };
                binding_scope = parent;
            }
        }
        if let Some(symbol) = definitions.get(&node.start_byte())
            && matches!(
                kind,
                "function_item"
                    | "function_definition"
                    | "function_declaration"
                    | "class_definition"
                    | "class_declaration"
                    | "variable_declarator"
            )
        {
            bind(
                &mut scopes[binding_scope],
                symbol.name.clone(),
                Some(symbol.start_byte),
                "ambiguous",
            );
        }
        let new_scope = matches!(
            kind,
            "function_item"
                | "function_definition"
                | "function_declaration"
                | "function_expression"
                | "arrow_function"
                | "lambda"
                | "closure_expression"
                | "class_definition"
                | "class_declaration"
                | "impl_item"
                | "trait_item"
                | "mod_item"
                | "method_definition"
                | "catch_clause"
                | "list_comprehension"
                | "set_comprehension"
                | "dictionary_comprehension"
                | "generator_expression"
        ) || (language != "python" && matches!(kind, "block" | "statement_block"));
        if new_scope {
            // Python class namespaces are not enclosing lexical scopes of
            // methods, lambdas or comprehensions. Class-body expressions still
            // see class-local names.
            let mut parent = scope;
            if language == "python" {
                while scopes[parent].python_class {
                    parent = scopes[parent].parent.unwrap_or(0);
                }
            }
            scopes.push(Scope {
                parent: (language != "rust" || kind != "mod_item").then_some(parent),
                python_class: language == "python" && kind == "class_definition",
                member_namespace: (language == "rust"
                    && matches!(kind, "impl_item" | "trait_item"))
                    || (matches!(language, "javascript" | "typescript" | "tsx")
                        && kind == "class_declaration"),
                function_boundary: matches!(
                    kind,
                    "function_item"
                        | "function_definition"
                        | "function_declaration"
                        | "function_expression"
                        | "arrow_function"
                        | "lambda"
                        | "closure_expression"
                        | "method_definition"
                ),
                ..Scope::default()
            });
            scope = scopes.len() - 1;
            if let Some(parameters) = node
                .child_by_field_name("parameters")
                .or_else(|| node.child_by_field_name("parameter"))
            {
                for name in names(parameters, source) {
                    bind(&mut scopes[scope], name, None, "shadowed");
                }
            }
        }
        let binding = match kind {
            "assignment"
            | "augmented_assignment"
            | "for_statement"
            | "for_in_statement"
            | "for_in_clause" => node.child_by_field_name("left"),
            "let_declaration" => node.child_by_field_name("pattern"),
            "as_pattern" if language == "python" => node.child_by_field_name("alias"),
            "variable_declarator" if !definitions.contains_key(&node.start_byte()) => {
                node.child_by_field_name("name")
            }
            "global_statement" | "nonlocal_statement" => Some(node),
            _ => None,
        };
        if let Some(binding) = binding {
            for name in names(binding, source) {
                bind(&mut scopes[binding_scope], name, None, "shadowed");
            }
        }
        if matches!(language, "javascript" | "typescript" | "tsx")
            && matches!(
                kind,
                "assignment_expression" | "augmented_assignment_expression"
            )
            && let Some(left) = node.child_by_field_name("left")
            && matches!(
                left.kind(),
                "identifier" | "array_pattern" | "object_pattern"
            )
        {
            assignments.extend(names(left, source).into_iter().map(|name| (scope, name)));
        }
        if matches!(
            kind,
            "import_statement" | "import_from_statement" | "use_declaration"
        ) {
            let imports = named_imports(node, source, language);
            let locals: std::collections::HashSet<_> =
                imports.iter().map(|(local, _, _)| local.as_str()).collect();
            for name in names(node, source) {
                if !locals.contains(name.as_str()) {
                    bind(&mut scopes[scope], name, None, "external");
                }
            }
            for (local, module, exported) in imports {
                match scopes[scope].bindings.entry(local) {
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(Binding {
                            target: None,
                            reason: "imported",
                            import: Some((module, exported)),
                        });
                    }
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        let b = entry.get_mut();
                        b.target = None;
                        b.import = None;
                        b.reason = "shadowed";
                    }
                }
            }
        }
        if matches!(kind, "call" | "call_expression") {
            locations.insert(node.start_byte(), scope);
        }
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        stack.extend(children.into_iter().rev().map(|child| (child, scope)));
    }

    // Reassignments can target bindings declared later or in enclosing scopes.
    // Invalidate those bindings after collection, without guessing execution order.
    for (mut scope, name) in assignments {
        loop {
            if let Some(binding) = scopes[scope].bindings.get_mut(&name) {
                binding.target = None;
                binding.import = None;
                binding.reason = "reassigned";
                break;
            }
            let Some(parent) = scopes[scope].parent else {
                break;
            };
            scope = parent;
        }
    }

    for reference in refs.iter_mut().filter(|r| r.kind == RefKind::Call) {
        let Some(mut node) = root
            .named_descendant_for_byte_range(reference.byte, reference.byte + reference.name.len())
        else {
            continue;
        };
        while !matches!(node.kind(), "call" | "call_expression") {
            let Some(parent) = node.parent() else { break };
            node = parent;
        }
        let Some(&scope) = locations.get(&node.start_byte()) else {
            continue;
        };
        let function = node.child_by_field_name("function");
        if function.is_some_and(|n| {
            matches!(
                n.kind(),
                "attribute" | "member_expression" | "field_expression"
            )
        }) {
            // Only Rust's explicit `self` receiver inside an impl is structural.
            if language == "rust"
                && std::iter::successors(node.parent(), |parent| parent.parent())
                    .any(|ancestor| ancestor.kind() == "impl_item")
                && function.is_some_and(|n| {
                    n.child_by_field_name("value")
                        .is_some_and(|v| text(v, source) == "self")
                })
            {
                let owner = reference
                    .from_symbol
                    .as_deref()
                    .and_then(|q| q.rsplit_once("::").map(|(parent, _)| parent));
                if let Some(owner) = owner {
                    reference.path = Some(format!("{owner}::{}", reference.name));
                    continue;
                }
            }
            reference.blocked = Some("dynamic".into());
            continue;
        }
        if let Some(path) = &reference.path {
            if language == "rust" && (path.starts_with("self::") || path.starts_with("super::")) {
                let mut modules = Vec::new();
                let mut parent = node.parent();
                while let Some(ancestor) = parent {
                    if ancestor.kind() == "mod_item"
                        && let Some(name) = ancestor.child_by_field_name("name")
                    {
                        modules.push(text(name, source));
                    }
                    parent = ancestor.parent();
                }
                modules.reverse();
                if !modules.is_empty() {
                    let (head, rest) = path.split_once("::").expect("scoped path");
                    if head == "super" {
                        modules.pop();
                    }
                    let prefix = if modules.is_empty() {
                        String::new()
                    } else {
                        format!("{}::", modules.join("::"))
                    };
                    reference.path = Some(format!("local_module::{prefix}{rest}"));
                }
            }
            continue;
        }
        let mut current = Some(scope);
        let mut found = false;
        while let Some(id) = current {
            // Rust associated items and JS/TS methods are not bare lexical
            // bindings. Explicit receivers/paths are handled separately above.
            if scopes[id].member_namespace {
                current = scopes[id].parent;
                continue;
            }
            if let Some(binding) = scopes[id].bindings.get(&reference.name) {
                found = true;
                if let Some((module, name)) = &binding.import {
                    reference.path = Some(format!("@import:{}", serde_json::json!([module, name])));
                } else if let Some(symbol) = binding.target.and_then(|byte| definitions.get(&byte))
                {
                    reference.path = Some(symbol.qualified.clone());
                } else {
                    reference.blocked = Some(binding.reason.into());
                }
                break;
            }
            current = scopes[id].parent;
        }
        if !found {
            // Bare names absent from lexical scopes must not match unrelated files.
            reference.blocked = Some("unknown".into());
        }
    }
}

fn named_imports(node: Node<'_>, source: &str, language: &str) -> Vec<(String, String, String)> {
    let mut result = Vec::new();
    if matches!(language, "javascript" | "typescript" | "tsx") {
        if text(node, source).trim_start().starts_with("import type") {
            return result;
        }
        let Some(module) = node.child_by_field_name("source") else {
            return result;
        };
        let module = text(module, source).trim_matches(['\'', '"']).to_string();
        let mut stack = vec![node];
        while let Some(part) = stack.pop() {
            if part.kind() == "import_specifier"
                && !text(part, source).trim_start().starts_with("type ")
            {
                if let Some(name) = part.child_by_field_name("name") {
                    let local = part.child_by_field_name("alias").unwrap_or(name);
                    result.push((
                        text(local, source).into(),
                        module.clone(),
                        text(name, source).into(),
                    ));
                }
            } else {
                let mut cursor = part.walk();
                stack.extend(part.named_children(&mut cursor));
            }
        }
    } else if language == "python" && node.kind() == "import_from_statement" {
        let Some(module) = node.child_by_field_name("module_name") else {
            return result;
        };
        let mut cursor = node.walk();
        for part in node.children_by_field_name("name", &mut cursor) {
            let name = part.child_by_field_name("name").unwrap_or(part);
            let local = part.child_by_field_name("alias").unwrap_or(name);
            if !text(name, source).contains('.') {
                result.push((
                    text(local, source).into(),
                    text(module, source).into(),
                    text(name, source).into(),
                ));
            }
        }
    }
    result
}
