//! Lua bindings are lexical and mutable. Only direct, unmodified bindings and
//! members of a locally constructed table provide call targets.

use std::collections::HashMap;

use tree_sitter::Node;

use super::{ParsedFile, ParsedRef, ParsedSymbol, assign_enclosing, assign_nesting};
use crate::error::{Error, Result};
use crate::model::{RefKind, SymbolKind};

#[derive(Default)]
struct Scope {
    parent: Option<usize>,
    locals: HashMap<String, Vec<usize>>,
}

struct Binding {
    scope: usize,
    available: usize,
    target: Option<usize>,
    table: bool,
    mutated: bool,
    members: HashMap<String, Member>,
}

struct Member {
    target: Option<usize>,
    available: usize,
    mutated: bool,
}

struct Call<'t> {
    node: Node<'t>,
    scope: usize,
}

struct Extraction<'s, 't> {
    source: &'s str,
    symbols: Vec<ParsedSymbol>,
    scopes: Vec<Scope>,
    globals: HashMap<String, usize>,
    bindings: Vec<Binding>,
    calls: Vec<Call<'t>>,
    visited: usize,
    members: usize,
    dynamic_environment: bool,
    changed_loader: bool,
    dynamic_tables: bool,
}

pub(super) fn extract(source: &str, root: Node<'_>) -> Result<ParsedFile> {
    let mut extraction = Extraction {
        source,
        symbols: Vec::new(),
        scopes: vec![Scope::default()],
        globals: HashMap::new(),
        bindings: Vec::new(),
        calls: Vec::new(),
        visited: 0,
        members: 0,
        dynamic_environment: false,
        changed_loader: false,
        dynamic_tables: false,
    };
    extraction.walk(root, 0, 0)?;
    // Bindings use insertion indexes; retain their byte identities through the
    // nesting helper's source-order sort.
    let identities: Vec<_> = extraction.symbols.iter().map(|s| s.start_byte).collect();
    assign_nesting(&mut extraction.symbols)?;
    let qualified: HashMap<_, _> = extraction
        .symbols
        .iter()
        .map(|s| (s.start_byte, s.qualified.clone()))
        .collect();
    let mut refs = Vec::new();
    for call in &extraction.calls {
        extraction.reference(call, &identities, &qualified, &mut refs)?;
        if extraction.symbols.len() + refs.len() > 100_000 {
            return Err(Error::Invalid("file extraction budget exceeded".into()));
        }
    }
    assign_enclosing(&extraction.symbols, &mut refs);
    Ok(ParsedFile {
        symbols: extraction.symbols,
        refs,
        has_errors: root.has_error(),
    })
}

impl<'s, 't> Extraction<'s, 't> {
    fn text(&self, node: Node<'_>) -> &'s str {
        &self.source[node.byte_range()]
    }

    fn lookup(&self, mut scope: usize, name: &str, byte: usize) -> Option<usize> {
        loop {
            if let Some(bindings) = self.scopes[scope].locals.get(name) {
                let visible = bindings.partition_point(|&id| self.bindings[id].available <= byte);
                if visible > 0 {
                    return Some(bindings[visible - 1]);
                }
            }
            match self.scopes[scope].parent {
                Some(parent) => scope = parent,
                None => return self.globals.get(name).copied(),
            }
        }
    }

    fn bind(
        &mut self,
        scope: usize,
        name: &str,
        available: usize,
        local: bool,
        target: Option<usize>,
        table: bool,
    ) -> Result<usize> {
        if name.len() > 4096 {
            return Err(Error::Invalid(
                "symbol/reference name exceeds 4096 bytes".into(),
            ));
        }
        if matches!(name, "_ENV" | "_G") {
            self.dynamic_environment = true;
        }
        if name == "package" {
            self.changed_loader = true;
        }
        if !local && let Some(id) = self.lookup(scope, name, available) {
            self.bindings[id].mutated = true;
            return Ok(id);
        }
        let id = self.bindings.len();
        if id + self.members >= 100_000 {
            return Err(Error::Invalid("file binding budget exceeded".into()));
        }
        self.bindings.push(Binding {
            scope,
            available,
            target,
            table,
            // A global assignment inside a function or conditional block does
            // not establish an unconditional binding in the chunk.
            mutated: !local && scope != 0,
            members: HashMap::new(),
        });
        if local {
            self.scopes[scope]
                .locals
                .entry(name.into())
                .or_default()
                .push(id);
        } else {
            self.globals.insert(name.into(), id);
        }
        Ok(id)
    }

    fn symbol(&mut self, node: Node<'_>, name: String, method: bool) -> Result<usize> {
        if name.len() > 4096 {
            return Err(Error::Invalid(
                "symbol/reference name exceeds 4096 bytes".into(),
            ));
        }
        let index = self.symbols.len();
        if index + self.calls.len() >= 100_000 {
            return Err(Error::Invalid("file extraction budget exceeded".into()));
        }
        self.symbols.push(ParsedSymbol {
            name,
            kind: if method {
                SymbolKind::Method
            } else {
                SymbolKind::Function
            },
            start_line: node.start_position().row as u32 + 1,
            end_line: node.end_position().row as u32 + 1,
            start_column: node.start_position().column as u32 + 1,
            end_column: node.end_position().column as u32 + 1,
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            parent: None,
            qualified: String::new(),
            signature: self.source[node.start_byte()
                ..node
                    .child_by_field_name("body")
                    .map_or(node.end_byte(), |body| body.start_byte())]
                .chars()
                .take(512)
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            // Lua modules can return arbitrary values; global visibility is
            // not evidence that a symbol is exported by require.
            exported: false,
        });
        Ok(index)
    }

    fn member(
        &mut self,
        binding: usize,
        name: String,
        target: Option<usize>,
        available: usize,
        scope: usize,
    ) -> Result<()> {
        if name.len() > 4096 {
            return Err(Error::Invalid(
                "symbol/reference name exceeds 4096 bytes".into(),
            ));
        }
        if self.members + self.bindings.len() >= 100_000 {
            return Err(Error::Invalid("file binding budget exceeded".into()));
        }
        let conditional = self.bindings[binding].scope != scope;
        let members = &mut self.bindings[binding].members;
        if let Some(member) = members.get_mut(&name) {
            member.mutated = true;
        } else {
            members.insert(
                name,
                Member {
                    target,
                    available,
                    mutated: conditional,
                },
            );
            self.members += 1;
        }
        Ok(())
    }

    fn write(
        &mut self,
        name: Node<'_>,
        value: Option<Node<'_>>,
        scope: usize,
        local: bool,
        available: usize,
    ) -> Result<()> {
        let path = static_path(name, self.source);
        let target = value
            .filter(|v| matches!(v.kind(), "function_definition" | "function_declaration"))
            .zip(path.as_ref())
            .map(|(function, path)| {
                self.symbol(function, path.clone(), name.kind() != "identifier")
            })
            .transpose()?;
        if name.kind() == "identifier" {
            let table = value.is_some_and(|v| v.kind() == "table_constructor");
            let id = self.bind(scope, self.text(name), available, local, target, table)?;
            if let Some(value) = value.filter(|_| table) {
                let mut cursor = value.walk();
                for field in value
                    .named_children(&mut cursor)
                    .filter(|n| n.kind() == "field")
                {
                    let field_name = field.child_by_field_name("name").and_then(|name| {
                        if self.text(field).starts_with('[') {
                            literal_string(name, self.source)
                        } else {
                            literal_field(name, self.source)
                        }
                    });
                    let Some(field_name) = field_name else {
                        self.bindings[id].mutated = true;
                        continue;
                    };
                    let function = field
                        .child_by_field_name("value")
                        .filter(|v| v.kind() == "function_definition");
                    let member_target = function
                        .map(|function| {
                            self.symbol(
                                function,
                                format!("{}.{}", self.text(name), field_name),
                                true,
                            )
                        })
                        .transpose()?;
                    self.member(id, field_name, member_target, available, scope)?;
                }
            }
        } else if let Some(base) = name.child_by_field_name("table") {
            let mut root = base;
            while let Some(table) = root.child_by_field_name("table") {
                root = table;
            }
            if self.text(root) == "package" {
                self.changed_loader = true;
            }
            if matches!(self.text(root), "_G" | "_ENV") {
                self.dynamic_environment = true;
            }
            if base.kind() == "identifier"
                && let Some(id) = self.lookup(scope, self.text(base), name.start_byte())
            {
                if let Some((_, field)) = path.as_deref().and_then(|path| path.split_once('.')) {
                    self.member(id, field.to_string(), target, available, scope)?;
                } else {
                    self.bindings[id].mutated = true;
                }
            }
        }
        Ok(())
    }

    fn value_can_escape(&self, mut node: Node<'_>) -> bool {
        let mut wrapped = false;
        while let Some(parent) = node.parent() {
            match parent.kind() {
                "parenthesized_expression" | "binary_expression" | "unary_expression" => {
                    // Parentheses and composed values do not keep a table from
                    // escaping into an alias, call or computed receiver.
                    wrapped = true;
                    node = parent;
                }
                "expression_list" | "arguments" => return true,
                "field" => {
                    return parent.child_by_field_name("value") == Some(node)
                        || (self.text(parent).starts_with('[')
                            && parent.child_by_field_name("name") == Some(node));
                }
                "dot_index_expression" | "method_index_expression" | "bracket_index_expression" => {
                    return (wrapped && parent.child_by_field_name("table") == Some(node))
                        || (parent.kind() == "bracket_index_expression"
                            && parent.child_by_field_name("field") == Some(node));
                }
                _ => return false,
            }
        }
        false
    }

    fn walk(&mut self, node: Node<'t>, mut scope: usize, depth: usize) -> Result<()> {
        self.visited += 1;
        if depth > 256 || self.visited > 1_000_000 {
            return Err(Error::Invalid("Lua traversal budget exceeded".into()));
        }
        match node.kind() {
            "function_declaration" => {
                if let Some(name) = node.child_by_field_name("name") {
                    let local = self.text(node).starts_with("local");
                    self.write(name, Some(node), scope, local, node.start_byte())?;
                }
            }
            "assignment_statement" => {
                let local = node.parent().is_some_and(|p| {
                    p.kind() == "variable_declaration" && self.text(p).starts_with("local")
                });
                let mut cursor = node.walk();
                let variables = node
                    .named_children(&mut cursor)
                    .find(|n| n.kind() == "variable_list");
                let mut cursor = node.walk();
                let values = node
                    .named_children(&mut cursor)
                    .find(|n| n.kind() == "expression_list");
                if let Some(variables) = variables {
                    let mut cursor = variables.walk();
                    for (index, variable) in variables
                        .children_by_field_name("name", &mut cursor)
                        .enumerate()
                    {
                        let value = values.and_then(|list| list.named_child(index));
                        self.write(variable, value, scope, local, node.end_byte())?;
                    }
                }
            }
            "variable_declaration" => {
                let mut cursor = node.walk();
                if let Some(variables) = node
                    .named_children(&mut cursor)
                    .find(|n| n.kind() == "variable_list")
                {
                    let mut cursor = variables.walk();
                    for name in variables.children_by_field_name("name", &mut cursor) {
                        self.bind(
                            scope,
                            self.text(name),
                            node.end_byte(),
                            self.text(node).starts_with("local"),
                            None,
                            false,
                        )?;
                    }
                }
            }
            "identifier" => {
                // Passing a table as a value can create an alias or let an
                // unknown callee mutate it; neither establishes stable fields.
                if self.value_can_escape(node) {
                    if self.text(node) == "package" {
                        self.changed_loader = true;
                    }
                    if matches!(self.text(node), "_ENV" | "_G") {
                        self.dynamic_environment = true;
                    }
                    if let Some(id) = self.lookup(scope, self.text(node), node.start_byte())
                        && self.bindings[id].table
                    {
                        self.bindings[id].mutated = true;
                    }
                }
            }
            "function_call" => {
                if self.symbols.len() + self.calls.len() >= 100_000 {
                    return Err(Error::Invalid("file extraction budget exceeded".into()));
                }
                if let Some(name) = node.child_by_field_name("name") {
                    if matches!(self.text(name), "setmetatable" | "rawset") {
                        self.dynamic_tables = true;
                        if node
                            .child_by_field_name("arguments")
                            .and_then(|args| args.named_child(0))
                            .is_some_and(|arg| matches!(self.text(arg), "_G" | "_ENV"))
                        {
                            self.dynamic_environment = true;
                        }
                    }
                    if matches!(
                        self.text(name),
                        "load" | "loadstring" | "dofile" | "setfenv" | "module"
                    ) {
                        self.dynamic_environment = true;
                        self.changed_loader = true;
                    }
                }
                self.calls.push(Call { node, scope });
            }
            _ => {}
        }
        if matches!(
            node.kind(),
            "block"
                | "function_declaration"
                | "function_definition"
                | "for_statement"
                | "repeat_statement"
        ) && !(node.kind() == "block"
            && node
                .parent()
                .is_some_and(|p| p.kind() == "repeat_statement"))
        {
            let id = self.scopes.len();
            self.scopes.push(Scope {
                parent: Some(scope),
                ..Scope::default()
            });
            scope = id;
            if let Some(parameters) = node.child_by_field_name("parameters") {
                let mut cursor = parameters.walk();
                for name in parameters.children_by_field_name("name", &mut cursor) {
                    self.bind(scope, self.text(name), node.start_byte(), true, None, false)?;
                }
                if node
                    .child_by_field_name("name")
                    .is_some_and(|n| n.kind() == "method_index_expression")
                {
                    self.bind(scope, "self", node.start_byte(), true, None, false)?;
                }
            }
            if let Some(clause) = node.child_by_field_name("clause") {
                let available = node
                    .child_by_field_name("body")
                    .map_or(node.end_byte(), |body| body.start_byte());
                if let Some(name) = clause.child_by_field_name("name") {
                    self.bind(scope, self.text(name), available, true, None, false)?;
                } else {
                    let mut cursor = clause.walk();
                    if let Some(variables) = clause
                        .named_children(&mut cursor)
                        .find(|n| n.kind() == "variable_list")
                    {
                        let mut cursor = variables.walk();
                        for name in variables.children_by_field_name("name", &mut cursor) {
                            self.bind(scope, self.text(name), available, true, None, false)?;
                        }
                    }
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.walk(child, scope, depth + 1)?;
        }
        Ok(())
    }

    fn reference(
        &self,
        call: &Call<'_>,
        identities: &[usize],
        qualified: &HashMap<usize, String>,
        refs: &mut Vec<ParsedRef>,
    ) -> Result<()> {
        let Some(name_node) = call.node.child_by_field_name("name") else {
            return Ok(());
        };
        if name_node.byte_range().len() > 4096 {
            return Err(Error::Invalid(
                "symbol/reference name exceeds 4096 bytes".into(),
            ));
        }
        let path = static_path(name_node, self.source);
        let name = path.as_deref().unwrap_or("<dynamic>");
        let mut reason = "unbound";
        let mut target = None;
        if self.dynamic_environment {
            reason = "dynamic_environment";
        } else if name_node.kind() == "identifier" {
            if let Some(id) = self.lookup(call.scope, name, name_node.start_byte()) {
                let binding = &self.bindings[id];
                if binding.mutated {
                    reason = "reassigned";
                } else if binding.available > name_node.start_byte() {
                    reason = "definition_order";
                } else {
                    target = binding.target;
                    reason = "shadowed_or_dynamic_binding";
                }
            }
        } else if let Some(table) = name_node
            .child_by_field_name("table")
            .filter(|n| n.kind() == "identifier")
        {
            reason = "dynamic_receiver";
            if let Some(id) = self.lookup(call.scope, self.text(table), name_node.start_byte()) {
                let binding = &self.bindings[id];
                if self.dynamic_tables || binding.mutated {
                    reason = "dynamic_table";
                } else if binding.table
                    && let Some(member) = path
                        .as_deref()
                        .and_then(|p| p.split_once('.'))
                        .and_then(|(_, field)| binding.members.get(field))
                {
                    if member.mutated {
                        reason = "reassigned";
                    } else if member.available <= name_node.start_byte() {
                        target = member.target;
                    }
                }
            }
        } else {
            reason = "dynamic_receiver";
        }
        let target = target
            .and_then(|id| qualified.get(&identities[id]))
            .cloned();
        refs.push(ParsedRef {
            name: name.to_string(),
            path: target.clone(),
            kind: RefKind::Call,
            line: name_node.start_position().row as u32 + 1,
            byte: name_node.start_byte(),
            from_symbol: None,
            blocked: target.is_none().then(|| reason.into()),
        });
        if name == "require" {
            let arguments = call.node.child_by_field_name("arguments");
            let argument = arguments.and_then(|args| args.named_child(0));
            let module = argument
                .filter(|_| arguments.is_some_and(|args| args.named_child_count() == 1))
                .and_then(|n| literal_string(n, self.source));
            if module.as_ref().is_some_and(|m| m.len() > 4096) {
                return Err(Error::Invalid(
                    "symbol/reference name exceeds 4096 bytes".into(),
                ));
            }
            let blocked = if self.dynamic_environment {
                Some("dynamic_environment")
            } else if self.changed_loader {
                Some("modified_lua_loader")
            } else if self
                .lookup(call.scope, "require", name_node.start_byte())
                .is_some()
            {
                Some("shadowed_require")
            } else if module.is_none() {
                Some("dynamic")
            } else {
                None
            };
            refs.push(ParsedRef {
                name: module.unwrap_or_else(|| "<dynamic>".into()),
                path: None,
                kind: RefKind::Import,
                line: name_node.start_position().row as u32 + 1,
                byte: name_node.start_byte(),
                from_symbol: None,
                blocked: blocked.map(str::to_string),
            });
        }
        Ok(())
    }
}

fn literal_string(node: Node<'_>, source: &str) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    let content = node
        .child_by_field_name("content")
        .map(|n| &source[n.byte_range()])
        .unwrap_or_default();
    // Escaped loader names are retained as unresolved instead of applying
    // Rust/JSON escaping rules to a Lua string.
    (!content.contains('\\')).then(|| content.to_string())
}

fn literal_field(node: Node<'_>, source: &str) -> Option<String> {
    if node.kind() == "identifier" {
        Some(source[node.byte_range()].to_string())
    } else {
        literal_string(node, source)
    }
}

fn static_path(node: Node<'_>, source: &str) -> Option<String> {
    match node.kind() {
        "identifier" => Some(source[node.byte_range()].to_string()),
        "dot_index_expression" | "method_index_expression" | "bracket_index_expression" => {
            let table = node.child_by_field_name("table")?;
            // Nested/dynamic receivers need value inference that this parser
            // deliberately does not supply.
            if table.kind() != "identifier" {
                return None;
            }
            let field = node
                .child_by_field_name("field")
                .or_else(|| node.child_by_field_name("method"))?;
            let field = if node.kind() == "bracket_index_expression" {
                literal_string(field, source)?
            } else {
                literal_field(field, source)?
            };
            Some(format!("{}.{}", &source[table.byte_range()], field))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::model::RefKind;
    use crate::{lang, parse};

    fn lua(source: &str) -> super::ParsedFile {
        let parsed = parse::parse(source, lang::by_id("lua").unwrap()).unwrap();
        assert!(!parsed.has_errors, "invalid Lua fixture");
        parsed
    }

    #[test]
    fn definitions_lexical_calls_and_static_table_methods() {
        let parsed = lua(
            "local function helper() end\nfunction run() helper() end\nlocal M = {}\nfunction M:method() helper() end\nM:method()\nrun()\nlocal inline = function() helper() end\ninline()\nlocal N = { f = function() helper() end }\nN.f()\n",
        );
        for name in ["helper", "run", "M.method", "inline", "N.f"] {
            assert!(
                parsed.symbols.iter().any(|s| s.name == name),
                "missing {name}"
            );
        }
        assert!(
            parsed
                .refs
                .iter()
                .filter(|r| r.kind == RefKind::Call)
                .all(|r| r.path.is_some()),
            "{:?}",
            parsed.refs
        );
        assert!(
            parsed
                .refs
                .iter()
                .any(|r| r.name == "helper" && r.from_symbol.as_deref() == Some("run"))
        );
    }

    #[test]
    fn parameters_shadowing_and_reassignment_block_targets() {
        let parsed = lua(
            "local function f() end\nlocal function shadow(f) f() end\ndo local f = 1; f() end\nf()\nf = replacement\nf()\n",
        );
        assert!(
            parsed
                .refs
                .iter()
                .filter(|r| r.name == "f")
                .all(|r| r.path.is_none())
        );
        let parsed = lua(
            "local function f() end\ndo local f = 1; f() end\nf()\nlocal function recursive() recursive() end\n",
        );
        let calls: Vec<_> = parsed.refs.iter().filter(|r| r.name == "f").collect();
        assert!(calls[0].path.is_none());
        assert_eq!(calls[1].path.as_deref(), Some("f"));
        assert_eq!(
            parsed
                .refs
                .iter()
                .find(|r| r.name == "recursive")
                .unwrap()
                .path
                .as_deref(),
            Some("recursive")
        );
    }

    #[test]
    fn dynamic_tables_receivers_and_metatables_are_unresolved() {
        for mutation in [
            "M[key] = replacement",
            "M.f = replacement",
            "M = other",
            "setmetatable(M, other)",
            "local alias = M; alias.f = replacement",
            "external(M)",
            "if unknown then function M.f() end end",
        ] {
            let parsed = lua(&format!(
                "local M = {{}}\nfunction M.f() end\n{mutation}\nM.f()\nM[key]()\n"
            ));
            assert!(parsed.refs.iter().all(|r| r.path.is_none()), "{mutation}");
        }
        let parsed = lua("function object.method() end\nobject.method()\n");
        assert!(parsed.refs.iter().all(|r| r.path.is_none()));
        let parsed = lua("local M = {}\nif unknown then function M.f() end end\nM.f()\n");
        assert!(parsed.refs.iter().all(|r| r.path.is_none()));
    }

    #[test]
    fn literal_requires_are_separate_and_loader_changes_block_imports() {
        let parsed = lua("local a = require('pkg.util')\nrequire \"second\"\nrequire(dynamic)\n");
        let imports: Vec<_> = parsed
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::Import)
            .collect();
        assert_eq!(imports[0].name, "pkg.util");
        assert!(imports[0].blocked.is_none());
        assert_eq!(imports[1].name, "second");
        assert_eq!(imports[2].blocked.as_deref(), Some("dynamic"));
        for prefix in [
            "package.path = custom",
            "package.cpath = custom",
            "package.searchers = custom",
            "package.searchers[1] = custom",
            "package[key] = custom",
            "local p = package; p.path = custom",
            "local require = replacement",
            "require = replacement",
        ] {
            let parsed = lua(&format!("{prefix}\nrequire('pkg.util')"));
            assert!(
                parsed
                    .refs
                    .iter()
                    .find(|r| r.kind == RefKind::Import)
                    .unwrap()
                    .blocked
                    .is_some(),
                "{prefix}"
            );
        }
    }

    #[test]
    fn wrapped_and_composed_table_values_do_not_keep_stable_members() {
        for mutation in [
            "local alias = (M); alias.f = replacement",
            "local alias = true and M; alias.f = replacement",
            "external((M))",
            "local holder = { value = (M) }; holder.value.f = replacement",
            "local holder = { [M] = true }; local alias = next(holder); alias.f = replacement",
            "local holder = {}; holder[M] = true; local alias = next(holder); alias.f = replacement",
            "(M).f = replacement",
            "(true and M).f = replacement",
        ] {
            let parsed = lua(&format!(
                "local M = {{}}\nfunction M.f() end\n{mutation}\nM.f()\n"
            ));
            let call = parsed.refs.iter().find(|r| r.name == "M.f").unwrap();
            assert!(call.path.is_none(), "{mutation}");
            assert_eq!(call.blocked.as_deref(), Some("dynamic_table"), "{mutation}");
        }
    }

    #[test]
    fn wrapped_environment_and_loader_values_block_resolution() {
        for mutation in [
            "local p = (package); p.path = custom",
            "local p = true and package; p.path = custom",
            "external((package))",
            "(package).path = custom",
        ] {
            let parsed = lua(&format!("{mutation}\nrequire('pkg.util')\n"));
            let import = parsed
                .refs
                .iter()
                .find(|r| r.kind == RefKind::Import)
                .unwrap();
            assert_eq!(
                import.blocked.as_deref(),
                Some("modified_lua_loader"),
                "{mutation}"
            );
        }
        for mutation in [
            "local g = (_G); g.f = replacement",
            "local g = true and _ENV; g.f = replacement",
            "(_G).f = replacement",
        ] {
            let parsed = lua(&format!("function f() end\n{mutation}\nf()\n"));
            let call = parsed.refs.iter().find(|r| r.name == "f").unwrap();
            assert!(call.path.is_none(), "{mutation}");
            assert_eq!(
                call.blocked.as_deref(),
                Some("dynamic_environment"),
                "{mutation}"
            );
        }
    }

    #[test]
    fn loop_initializers_and_repeat_conditions_use_lua_scopes() {
        let parsed = lua(
            "local function f() end\nfor f = f(), 2 do f() end\nrepeat local f = other until f()\n",
        );
        let calls: Vec<_> = parsed.refs.iter().filter(|r| r.name == "f").collect();
        assert_eq!(calls[0].path.as_deref(), Some("f"));
        assert!(calls[1].path.is_none());
        assert!(calls[2].path.is_none());
    }

    #[test]
    fn later_definitions_and_initializer_scope_never_guess() {
        let parsed = lua(
            "later()\nfunction later() end\nlocal f = function() f() end\nlocal function loop() end\nfor loop = 1, 2 do loop() end\n",
        );
        assert!(
            parsed.refs.iter().all(|r| r.path.is_none()),
            "{:?}",
            parsed.refs
        );
    }
}
