//! Conservative dispatch validation over verified source and complete binding
//! uses from the pinned adapter. Type annotations alone never prove a receiver.
use super::{Definition, coordinates};
use crate::{error::Result, scip::invalid};
use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::{Node, Point, Tree};

type Location = (String, [u32; 4]);

pub(super) struct File {
    pub text: String,
    pub tree: Tree,
}

impl File {
    fn token(&self, range: [u32; 4]) -> Option<Node<'_>> {
        let node = self.tree.root_node().named_descendant_for_point_range(
            Point::new(range[0] as usize, range[1] as usize),
            Point::new(range[2] as usize, range[3] as usize),
        )?;
        (coordinates(node) == range).then_some(node)
    }
    fn text(&self, node: Node<'_>) -> &str {
        self.text.get(node.byte_range()).unwrap_or("")
    }
}

pub(super) fn declaration(root: Node<'_>, range: [u32; 4]) -> Option<Node<'_>> {
    let token = root.named_descendant_for_point_range(
        Point::new(range[0] as usize, range[1] as usize),
        Point::new(range[2] as usize, range[3] as usize),
    )?;
    let parent = token.parent()?;
    (coordinates(token) == range
        && parent.child_by_field_name("name") == Some(token)
        && matches!(
            parent.kind(),
            "function_declaration"
                | "class_declaration"
                | "method_definition"
                | "variable_declarator"
        ))
    .then_some(parent)
}

fn nodes<'a>(root: Node<'a>, budget: &mut usize) -> Result<Vec<Node<'a>>> {
    let mut pending = vec![root];
    let mut result = Vec::new();
    while let Some(node) = pending.pop() {
        if *budget == 0 {
            return Err(invalid("TypeScript dispatch validation budget exceeded"));
        }
        *budget -= 1;
        result.push(node);
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    Ok(result)
}

fn keyword(node: Node<'_>, kind: &str) -> bool {
    (0..node.child_count()).any(|i| node.child(i).is_some_and(|n| n.kind() == kind))
}

fn decorated(node: Node<'_>) -> bool {
    std::iter::successors(Some(node), |n| n.parent()).any(|n| {
        matches!(
            n.kind(),
            "class_declaration" | "method_definition" | "export_statement" | "function_declaration"
        ) && {
            let mut cursor = n.walk();
            n.named_children(&mut cursor)
                .any(|child| child.kind() == "decorator")
        }
    })
}

fn owner(node: Node<'_>) -> Option<Node<'_>> {
    std::iter::successors(node.parent(), |n| n.parent()).find(|n| {
        matches!(
            n.kind(),
            "function_declaration" | "function_expression" | "arrow_function" | "method_definition"
        )
    })
}

fn terminal(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "identifier" | "type_identifier" => Some(node),
        "member_expression" => node.child_by_field_name("property"),
        _ => None,
    }
}

fn unwrap(mut node: Node<'_>) -> Node<'_> {
    while node.kind() == "parenthesized_expression" {
        let mut cursor = node.walk();
        let Some(inner) = node
            .named_children(&mut cursor)
            .find(|n| n.kind() != "comment")
        else {
            break;
        };
        node = inner;
    }
    node
}

fn member_call(node: Node<'_>) -> Option<Node<'_>> {
    let member = node.parent()?;
    let call = member.parent()?;
    (member.kind() == "member_expression"
        && member.child_by_field_name("object") == Some(node)
        && call.kind() == "call_expression"
        && call.child_by_field_name("function") == Some(member))
    .then_some(call)
}

/// Simple classes have no replacement decorators, inheritance, accessors,
/// executable initializers or escaping `this`. Data-field reads/writes are fine.
fn class_syntax(file: &File, class: Node<'_>, budget: &mut usize) -> Result<bool> {
    if decorated(class) {
        return Ok(false);
    }
    let mut cursor = class.walk();
    if class.named_children(&mut cursor).any(|n| {
        matches!(
            n.kind(),
            "class_heritage" | "extends_clause" | "implements_clause"
        )
    }) {
        return Ok(false);
    }
    let Some(body) = class.child_by_field_name("body") else {
        return Ok(false);
    };
    let mut fields = BTreeSet::new();
    let mut methods = Vec::new();
    let mut cursor = body.walk();
    for member in body.named_children(&mut cursor) {
        if member.kind() == "comment" {
            continue;
        }
        let mut children = member.walk();
        if member
            .named_children(&mut children)
            .any(|n| n.kind() == "decorator")
        {
            return Ok(false);
        }
        let Some(name) = member.child_by_field_name("name") else {
            return Ok(false);
        };
        if !matches!(
            name.kind(),
            "property_identifier" | "private_property_identifier" | "identifier"
        ) {
            return Ok(false);
        }
        match member.kind() {
            "public_field_definition" => {
                if member.child_by_field_name("value").is_some_and(|n| {
                    !matches!(n.kind(), "number" | "string" | "true" | "false" | "null")
                }) {
                    return Ok(false);
                }
                fields.insert(file.text(name));
            }
            "method_definition" => {
                if keyword(member, "get") || keyword(member, "set") || decorated(member) {
                    return Ok(false);
                }
                methods.push((member, file.text(name) == "constructor"));
            }
            _ => return Ok(false),
        }
    }
    for (method, constructor) in methods {
        if method.child_by_field_name("body").is_none() {
            return Ok(false);
        }
        // Default parameter expressions execute too, including before the
        // constructor body. They must not leak or replace the fresh receiver.
        for node in nodes(method, budget)? {
            if matches!(
                node.kind(),
                "call_expression"
                    | "new_expression"
                    | "await_expression"
                    | "yield_expression"
                    | "function_expression"
                    | "arrow_function"
                    | "function_declaration"
                    | "class_declaration"
                    | "subscript_expression"
                    | "super"
            ) {
                return Ok(false);
            }
            if constructor && node.kind() == "return_statement" && node.named_child_count() > 0 {
                return Ok(false);
            }
            if node.kind() == "this" {
                let Some(member) = node.parent().filter(|n| {
                    n.kind() == "member_expression" && n.child_by_field_name("object") == Some(node)
                }) else {
                    return Ok(false);
                };
                if !member.child_by_field_name("property").is_some_and(|n| {
                    n.kind() == "property_identifier" && fields.contains(file.text(n))
                }) {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

pub(super) struct Validator {
    files: BTreeMap<String, File>,
    meanings: BTreeMap<Location, Location>,
    uses: BTreeMap<Location, BTreeSet<Location>>,
    namespaces: BTreeMap<String, BTreeSet<String>>,
    writes: BTreeSet<String>,
    dynamic: bool,
    classes: BTreeMap<Location, bool>,
    instances: BTreeMap<Location, Location>,
    call_blocks: BTreeMap<Location, Option<String>>,
}

impl Validator {
    pub(super) fn new(
        files: BTreeMap<String, File>,
        definitions: &BTreeMap<String, Vec<Definition>>,
        sites: &BTreeMap<Location, BTreeSet<String>>,
    ) -> Result<Self> {
        let mut result = Self {
            files,
            meanings: BTreeMap::new(),
            uses: BTreeMap::new(),
            namespaces: BTreeMap::new(),
            writes: BTreeSet::new(),
            dynamic: false,
            classes: BTreeMap::new(),
            instances: BTreeMap::new(),
            call_blocks: BTreeMap::new(),
        };
        for (site, identities) in sites {
            for identity in identities {
                if let Some(defs) = definitions.get(identity).filter(|defs| defs.len() == 1) {
                    let definition = &defs[0];
                    let target = (definition.file.clone(), definition.range);
                    result
                        .uses
                        .entry(target.clone())
                        .or_default()
                        .insert(site.clone());
                    if identities.len() == 1 {
                        result.meanings.insert(site.clone(), target);
                    }
                }
            }
        }
        let mut budget = 1_000_000;
        for (path, file) in &result.files {
            let spec = crate::lang::detect(std::path::Path::new(path))
                .ok_or_else(|| invalid("unsupported TypeScript dispatch source"))?;
            for reference in crate::parse::parse(&file.text, spec)?
                .refs
                .into_iter()
                .filter(|r| r.kind == crate::model::RefKind::Call)
            {
                result
                    .call_blocks
                    .insert((path.clone(), reference.range), reference.blocked);
            }
            result.dynamic |= file.tree.root_node().has_error();
            for node in nodes(file.tree.root_node(), &mut budget)? {
                if node.kind() == "namespace_import" {
                    let mut cursor = node.walk();
                    if let Some(name) = node
                        .named_children(&mut cursor)
                        .find(|n| n.kind() == "identifier")
                    {
                        result
                            .namespaces
                            .entry(path.clone())
                            .or_default()
                            .insert(file.text(name).to_owned());
                    }
                }
                if node.kind() == "class_declaration"
                    && let Some(name) = node.child_by_field_name("name")
                {
                    result.classes.insert(
                        (path.clone(), coordinates(name)),
                        class_syntax(file, node, &mut budget)?,
                    );
                }
                if matches!(node.kind(), "identifier" | "property_identifier")
                    && matches!(file.text(node), "eval" | "Function")
                {
                    result.dynamic = true;
                }
                let left = match node.kind() {
                    "assignment_expression"
                    | "augmented_assignment_expression"
                    | "for_in_statement" => node.child_by_field_name("left"),
                    "update_expression" => node.child_by_field_name("argument"),
                    "unary_expression" if keyword(node, "delete") => {
                        node.child_by_field_name("argument")
                    }
                    _ => None,
                };
                if let Some(left) = left {
                    for name in nodes(left, &mut budget)? {
                        if name.kind() == "subscript_expression" {
                            result.dynamic = true;
                        }
                        if matches!(
                            name.kind(),
                            "identifier"
                                | "property_identifier"
                                | "shorthand_property_identifier_pattern"
                        ) {
                            result.writes.insert(file.text(name).to_owned());
                        }
                    }
                }
                if node.kind() == "call_expression"
                    && node
                        .child_by_field_name("function")
                        .is_some_and(|n| n.kind() == "subscript_expression")
                {
                    result.dynamic = true;
                }
            }
        }
        let classes: Vec<_> = result.classes.keys().cloned().collect();
        for class in classes {
            let safe = result.classes[&class] && result.class_uses_safe(&class);
            result.classes.insert(class, safe);
        }
        for defs in definitions.values().filter(|defs| defs.len() == 1) {
            let location = (defs[0].file.clone(), defs[0].range);
            if let Some(class) = result.fresh_binding(&location) {
                result.instances.insert(location, class);
            }
        }
        Ok(result)
    }

    fn meaning(&self, file: &str, node: Node<'_>) -> Option<&Location> {
        self.meanings.get(&(file.to_owned(), coordinates(node)))
    }

    fn namespace(&self, file: &str, node: Node<'_>, blocked: Option<&str>) -> bool {
        if matches!(blocked, Some("shadowed" | "reassigned")) {
            return false;
        }
        let source = &self.files[file];
        if node.kind() == "identifier" {
            let imported = self
                .namespaces
                .get(file)
                .is_some_and(|names| names.contains(source.text(node)));
            let module = self.meaning(file, node).is_some_and(|target| {
                self.files[&target.0]
                    .token(target.1)
                    .is_some_and(|token| matches!(token.kind(), "program" | "source_file"))
            });
            return imported && module;
        }
        if node.kind() == "member_expression" {
            let Some(object) = node.child_by_field_name("object") else {
                return false;
            };
            let Some(target) = node
                .child_by_field_name("property")
                .and_then(|n| self.meaning(file, n))
            else {
                return false;
            };
            let Some(token) = self.files[&target.0].token(target.1) else {
                return false;
            };
            return matches!(token.kind(), "program" | "source_file")
                && self.namespace(file, object, blocked);
        }
        false
    }

    fn static_access(&self, file: &str, node: Node<'_>, blocked: Option<&str>) -> bool {
        if blocked == Some("reassigned") {
            return false;
        }
        match node.kind() {
            "identifier" => true,
            "member_expression" => node
                .child_by_field_name("object")
                .is_some_and(|n| self.namespace(file, n, blocked)),
            _ => false,
        }
    }

    fn class_uses_safe(&self, class: &Location) -> bool {
        let Some(name) = self.files[&class.0].token(class.1) else {
            return false;
        };
        if self.writes.contains(self.files[&class.0].text(name)) {
            return false;
        }
        self.uses.get(class).is_none_or(|uses| {
            uses.iter().all(|(file, range)| {
                let Some(token) = self.files[file].token(*range) else {
                    return false;
                };
                if std::iter::successors(token.parent(), |n| n.parent())
                    .any(|n| n.kind() == "class_heritage")
                {
                    return false;
                }
                if token.kind() == "type_identifier" {
                    return true;
                }
                if token.parent().is_some_and(|p| {
                    matches!(
                        p.kind(),
                        "import_specifier"
                            | "export_specifier"
                            | "type_query"
                            | "class_declaration"
                    )
                }) {
                    return true;
                }
                let expression = if token.parent().is_some_and(|p| {
                    p.kind() == "member_expression"
                        && p.child_by_field_name("property") == Some(token)
                }) {
                    token.parent().unwrap()
                } else {
                    token
                };
                if expression.parent().is_some_and(|p| {
                    p.kind() == "new_expression"
                        && p.child_by_field_name("constructor") == Some(expression)
                }) {
                    return true;
                }
                member_call(expression).is_some()
            })
        })
    }

    fn new_class(&self, file: &str, node: Node<'_>) -> Option<Location> {
        let node = unwrap(node);
        if node.kind() != "new_expression" {
            return None;
        }
        let constructor = node.child_by_field_name("constructor")?;
        let blocked = self
            .call_blocks
            .get(&(file.to_owned(), coordinates(terminal(constructor)?)))?
            .as_deref();
        if !self.static_access(file, constructor, blocked) {
            return None;
        }
        let class = self.meaning(file, terminal(constructor)?)?;
        self.classes
            .get(class)
            .copied()
            .unwrap_or(false)
            .then(|| class.clone())
    }

    fn fresh_binding(&self, location: &Location) -> Option<Location> {
        let file = &self.files[&location.0];
        let declaration = declaration(file.tree.root_node(), location.1)?;
        if declaration.kind() != "variable_declarator"
            || !declaration.parent().is_some_and(|p| keyword(p, "const"))
        {
            return None;
        }
        let binding_owner = owner(declaration)?;
        let class = self.new_class(&location.0, declaration.child_by_field_name("value")?)?;
        let safe = self.uses.get(location).is_some_and(|uses| {
            uses.iter().all(|(path, range)| {
                if path != &location.0 {
                    return false;
                }
                let Some(token) = file.token(*range) else {
                    return false;
                };
                token.start_byte() > declaration.end_byte()
                    && owner(token) == Some(binding_owner)
                    && member_call(token).is_some()
            })
        });
        safe.then_some(class)
    }

    pub(super) fn admits(
        &self,
        file: &str,
        range: [u32; 4],
        target: &Definition,
        blocked: Option<&str>,
    ) -> bool {
        if self.dynamic || blocked == Some("reassigned") {
            return false;
        }
        let target_file = &self.files[&target.file];
        let Some(definition) = declaration(target_file.tree.root_node(), target.range) else {
            return false;
        };
        let name = target_file.text(definition.child_by_field_name("name").unwrap());
        if self.writes.contains(name) || decorated(definition) {
            return false;
        }
        let Some(token) = self.files[file].token(range) else {
            return false;
        };
        let Some(call) = std::iter::successors(token.parent(), |n| n.parent())
            .find(|n| matches!(n.kind(), "call_expression" | "new_expression"))
        else {
            return false;
        };
        let Some(callee) = call
            .child_by_field_name("function")
            .or_else(|| call.child_by_field_name("constructor"))
        else {
            return false;
        };
        match definition.kind() {
            "function_declaration" => {
                definition.child_by_field_name("body").is_some()
                    && self.static_access(file, callee, blocked)
            }
            "variable_declarator" => {
                definition.parent().is_some_and(|p| keyword(p, "const"))
                    && definition.child_by_field_name("value").is_some_and(|n| {
                        matches!(n.kind(), "arrow_function" | "function_expression")
                    })
                    && self.static_access(file, callee, blocked)
            }
            "class_declaration" => {
                call.kind() == "new_expression"
                    && self
                        .classes
                        .get(&(target.file.clone(), target.range))
                        .copied()
                        .unwrap_or(false)
                    && self.static_access(file, callee, blocked)
            }
            "method_definition" => {
                let Some(class) = std::iter::successors(definition.parent(), |n| n.parent())
                    .find(|n| n.kind() == "class_declaration")
                else {
                    return false;
                };
                let class = (
                    target.file.clone(),
                    coordinates(class.child_by_field_name("name").unwrap()),
                );
                if !self.classes.get(&class).copied().unwrap_or(false)
                    || callee.kind() != "member_expression"
                {
                    return false;
                }
                let Some(receiver) = callee.child_by_field_name("object") else {
                    return false;
                };
                if keyword(definition, "static") {
                    self.static_access(file, receiver, blocked)
                        && terminal(receiver).and_then(|n| self.meaning(file, n)) == Some(&class)
                } else {
                    let receiver = unwrap(receiver);
                    let concrete = if receiver.kind() == "identifier" {
                        self.meaning(file, receiver)
                            .and_then(|binding| self.instances.get(binding))
                            .cloned()
                    } else {
                        self.new_class(file, receiver)
                    };
                    concrete.as_ref() == Some(&class)
                }
            }
            _ => false,
        }
    }
}
