//! PostgreSQL schema structure; no database connection or SQL evaluation.

use std::collections::HashMap;

use tree_sitter::Node;

use super::{ParsedFile, ParsedRef, ParsedSymbol, assign_enclosing};
use crate::error::{Error, Result};
use crate::model::{RefKind, SymbolKind};

struct ObjectName {
    name: String,
    qualified: String,
    schema: bool,
    database: bool,
}

#[derive(Default)]
struct Scope {
    parent: Option<usize>,
    /// Nonrecursive CTEs are visible after their own query has finished.
    ctes: HashMap<String, usize>,
    recursive: bool,
}

pub(super) fn extract(source: &str, root: Node<'_>) -> Result<ParsedFile> {
    let mut parsed = ParsedFile {
        has_errors: root.has_error(),
        ..ParsedFile::default()
    };
    let mut scopes = vec![Scope::default()];
    let mut stack = vec![(root, 0usize, false)];
    let mut hazards = Vec::new();
    let mut work = 0usize;
    while let Some((node, mut scope, in_function)) = stack.pop() {
        spend(&mut work)?;
        if matches!(node.kind(), "literal" | "comment") {
            continue;
        }
        if matches!(
            node.kind(),
            "statement" | "create_query" | "subquery" | "insert"
        ) {
            let mut nested = Scope {
                parent: Some(scope),
                ..Scope::default()
            };
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "keyword_recursive" {
                    nested.recursive = true;
                }
                if child.kind() == "cte"
                    && let Some(name) = child_kind(child, "identifier")
                    && let Some(name) = identifier(name, source)?
                {
                    nested.ctes.entry(name).or_insert(child.end_byte());
                }
            }
            if !nested.ctes.is_empty() {
                scope = scopes.len();
                scopes.push(nested);
            }
        }
        if let Some(kind) = definition_kind(node.kind())
            && let Some(object) = child_kind(node, "object_reference")
            && let Some(name) = object_name(object, source)?
            && !name.database
        {
            budget(&parsed)?;
            let body_start = ["function_body", "create_query", "column_definitions"]
                .into_iter()
                .filter_map(|kind| child_kind(node, kind).map(|child| child.start_byte()))
                .min()
                .unwrap_or(node.end_byte());
            parsed.symbols.push(ParsedSymbol {
                name: name.name,
                qualified: name.qualified,
                kind,
                start_line: node.start_position().row as u32 + 1,
                end_line: node.end_position().row as u32 + 1,
                start_column: node.start_position().column as u32 + 1,
                end_column: node.end_position().column as u32 + 1,
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
                parent: None,
                signature: source
                    .get(node.start_byte()..body_start)
                    .unwrap_or_default()
                    .chars()
                    .take(512)
                    .collect::<String>()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                exported: true,
            });
        }

        let target = match node.kind() {
            "relation" | "insert" | "alter_table" | "from" => {
                child_kind(node, "object_reference").map(|n| (n, RefKind::Dependency))
            }
            "column_definition" | "constraint" => {
                foreign_target(node).map(|n| (n, RefKind::Dependency))
            }
            "invocation" => child_kind(node, "object_reference").map(|n| (n, RefKind::Call)),
            _ => None,
        };
        if let Some((target, kind)) = target
            && let Some(name) = object_name(target, source)?
        {
            budget(&parsed)?;
            let blocked = if name.database {
                Some("unsupported_sql_database")
            } else if kind == RefKind::Dependency
                && node.kind() == "relation"
                && node.parent().is_none_or(|parent| parent.kind() != "update")
                && !name.schema
                && cte_visible(&scopes, scope, &name.name, target.start_byte(), &mut work)?
            {
                Some("sql_cte")
            } else if in_function && !name.schema {
                Some("sql_runtime_search_path")
            } else {
                None
            };
            let path = if name.schema {
                format!("@sql-qualified:{}", name.qualified)
            } else {
                format!("@sql-local:{}", name.name)
            };
            parsed.refs.push(ParsedRef {
                name: name.name,
                path: Some(path),
                kind,
                line: target.start_position().row as u32 + 1,
                byte: target.start_byte(),
                from_symbol: None,
                blocked: blocked.map(str::to_string),
            });
        }

        if matches!(node.kind(), "set_statement" | "reset_statement") {
            // Session authorization and role can also change the implicit
            // search path; conservatively treat every SET/RESET as a barrier.
            hazards.push(node.start_byte());
        }
        if node.kind() == "invocation"
            && let Some(object) = child_kind(node, "object_reference")
            && let Some(name) = object_name(object, source)?
            && matches!(name.name.as_str(), "set_config" | "dblink_exec")
        {
            hazards.push(node.start_byte());
        }
        if node.kind() == "ERROR" {
            // Unparsed SQL may contain dynamic execution or a namespace change.
            // Keep valid surrounding structure but do not infer later local names.
            hazards.push(node.start_byte());
        }

        let sql_function = if node.kind() == "create_function" {
            child_kind(node, "function_language")
                .and_then(|language| child_kind(language, "identifier"))
                .map(|language| identifier(language, source))
                .transpose()?
                .flatten()
                .is_some_and(|language| language == "sql")
        } else {
            false
        };
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "function_body" {
                if !sql_function || child.has_error() || child_kind(child, "literal").is_some() {
                    budget(&parsed)?;
                    parsed.refs.push(ParsedRef {
                        name: "<dynamic>".into(),
                        path: None,
                        kind: RefKind::Dependency,
                        line: child.start_position().row as u32 + 1,
                        byte: child.start_byte(),
                        from_symbol: None,
                        blocked: Some("unsupported_sql_function_body".into()),
                    });
                    continue;
                }
                stack.push((child, scope, true));
            } else {
                stack.push((child, scope, in_function));
            }
        }
    }
    if let Some(first_hazard) = hazards.into_iter().min() {
        for reference in &mut parsed.refs {
            if reference.byte > first_hazard
                && reference.blocked.is_none()
                && reference
                    .path
                    .as_deref()
                    .is_some_and(|p| p.starts_with("@sql-local:"))
            {
                reference.blocked = Some("sql_dynamic_namespace".into());
            }
        }
    }
    parsed.symbols.sort_by_key(|symbol| symbol.start_byte);
    parsed.refs.sort_by_key(|reference| reference.byte);
    assign_enclosing(&parsed.symbols, &mut parsed.refs);
    Ok(parsed)
}

fn definition_kind(kind: &str) -> Option<SymbolKind> {
    match kind {
        "create_table" => Some(SymbolKind::Table),
        "create_view" | "create_materialized_view" => Some(SymbolKind::View),
        "create_function" => Some(SymbolKind::Function),
        _ => None,
    }
}

fn child_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() == kind)
}

fn foreign_target(node: Node<'_>) -> Option<Node<'_>> {
    let mut after_references = false;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "keyword_references" {
            after_references = true;
        } else if after_references && child.kind() == "object_reference" {
            return Some(child);
        }
    }
    None
}

fn cte_visible(
    scopes: &[Scope],
    mut scope: usize,
    name: &str,
    byte: usize,
    work: &mut usize,
) -> Result<bool> {
    loop {
        spend(work)?;
        let current = &scopes[scope];
        if current
            .ctes
            .get(name)
            .is_some_and(|end| current.recursive || *end <= byte)
        {
            return Ok(true);
        }
        let Some(parent) = current.parent else {
            return Ok(false);
        };
        scope = parent;
    }
}

fn object_name(node: Node<'_>, source: &str) -> Result<Option<ObjectName>> {
    if node.byte_range().len() > 4096 {
        return Err(Error::Invalid(
            "symbol/reference name exceeds 4096 bytes".into(),
        ));
    }
    let Some(name) = node.child_by_field_name("name") else {
        return Ok(None);
    };
    let Some(name) = identifier(name, source)? else {
        return Ok(None);
    };
    let mut parts = Vec::new();
    for field in ["database", "schema"] {
        if let Some(part) = node.child_by_field_name(field) {
            let Some(part) = identifier(part, source)? else {
                return Ok(None);
            };
            parts.push(part);
        }
    }
    parts.push(name.clone());
    let qualified = parts.join(".");
    if qualified.len() > 4096 {
        return Err(Error::Invalid("qualified name budget exceeded".into()));
    }
    Ok(Some(ObjectName {
        name,
        qualified,
        schema: node.child_by_field_name("schema").is_some(),
        database: node.child_by_field_name("database").is_some(),
    }))
}

fn identifier(node: Node<'_>, source: &str) -> Result<Option<String>> {
    if node.byte_range().len() > 4096 {
        return Err(Error::Invalid(
            "symbol/reference name exceeds 4096 bytes".into(),
        ));
    }
    let text = source.get(node.byte_range()).unwrap_or_default();
    if let Some(quoted) = text.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        let unescaped = quoted.replace("\"\"", "\"");
        return Ok(Some(
            if plain_identifier(&unescaped) && unescaped == unescaped.to_ascii_lowercase() {
                unescaped
            } else {
                format!("\"{}\"", unescaped.replace('"', "\"\""))
            },
        ));
    }
    Ok(plain_identifier(text).then(|| text.to_ascii_lowercase()))
}

fn plain_identifier(text: &str) -> bool {
    let mut characters = text.chars();
    characters
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_alphabetic())
        && characters.all(|ch| ch == '_' || ch == '$' || ch.is_alphanumeric())
}

fn budget(parsed: &ParsedFile) -> Result<()> {
    if parsed.symbols.len() + parsed.refs.len() >= 100_000 {
        Err(Error::Invalid("file extraction budget exceeded".into()))
    } else {
        Ok(())
    }
}

fn spend(work: &mut usize) -> Result<()> {
    *work += 1;
    if *work > 1_000_000 {
        Err(Error::Invalid("SQL extraction work budget exceeded".into()))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> ParsedFile {
        super::super::parse(source, crate::lang::by_id("sql").expect("SQL grammar"))
            .expect("SQL extraction")
    }

    #[test]
    fn postgres_extracts_tables_views_foreign_keys_and_separate_calls() {
        let parsed = parse(
            "CREATE TABLE app.users (id integer PRIMARY KEY);\n\
             CREATE TABLE app.posts (id integer, owner integer REFERENCES app.users(id));\n\
             CREATE VIEW app.summary AS SELECT app.decorate(u.id) FROM app.users AS u JOIN app.posts p ON p.owner = u.id;\n\
             CREATE MATERIALIZED VIEW app.cached AS SELECT * FROM app.summary;",
        );
        assert!(!parsed.has_errors);
        assert_eq!(parsed.symbols.len(), 4);
        assert_eq!(parsed.symbols[0].qualified, "app.users");
        assert_eq!(parsed.symbols[0].kind, SymbolKind::Table);
        assert_eq!(parsed.symbols[2].kind, SymbolKind::View);
        assert_eq!(parsed.symbols[3].kind, SymbolKind::View);
        assert!(parsed.refs.iter().any(|r| r.name == "users"
            && r.kind == RefKind::Dependency
            && r.from_symbol.as_deref() == Some("app.posts")
            && r.path.as_deref() == Some("@sql-qualified:app.users")));
        assert!(
            parsed
                .refs
                .iter()
                .any(|r| r.name == "decorate" && r.kind == RefKind::Call)
        );
        assert!(
            !parsed
                .refs
                .iter()
                .any(|r| matches!(r.name.as_str(), "u" | "p" | "id" | "owner"))
        );
    }

    #[test]
    fn postgres_identifiers_follow_quoted_case_and_schema_rules() {
        let parsed = parse(
            "CREATE TABLE APP.Users (id integer);\n\
             CREATE TABLE app.\"Users\" (id integer);\n\
             CREATE VIEW app.v AS SELECT * FROM \"app\".\"users\" JOIN app.\"Users\" ON true;",
        );
        assert!(!parsed.has_errors);
        assert_eq!(parsed.symbols[0].qualified, "app.users");
        assert_eq!(parsed.symbols[1].qualified, "app.\"Users\"");
        assert_eq!(
            parsed.refs[0].path.as_deref(),
            Some("@sql-qualified:app.users")
        );
        assert_eq!(
            parsed.refs[1].path.as_deref(),
            Some("@sql-qualified:app.\"Users\"")
        );
    }

    #[test]
    fn postgres_ctes_shadow_relations_only_in_their_visible_scope() {
        let parsed = parse(
            "CREATE TABLE users (id integer);\n\
             WITH users AS (SELECT * FROM users), recent AS (SELECT * FROM users) SELECT * FROM recent;\n\
             SELECT * FROM users;\n\
             WITH RECURSIVE users AS (SELECT * FROM users) SELECT * FROM users;",
        );
        assert!(!parsed.has_errors);
        let refs: Vec<_> = parsed
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::Dependency)
            .collect();
        assert_eq!(refs.len(), 6);
        assert_eq!(
            refs[0].blocked, None,
            "nonrecursive CTE body sees the real table"
        );
        assert_eq!(refs[1].blocked.as_deref(), Some("sql_cte"));
        assert_eq!(refs[2].blocked.as_deref(), Some("sql_cte"));
        assert_eq!(refs[3].blocked, None, "CTE scope ends at its statement");
        assert_eq!(refs[4].blocked.as_deref(), Some("sql_cte"));
        assert_eq!(refs[5].blocked.as_deref(), Some("sql_cte"));
    }

    #[test]
    fn postgres_ctes_do_not_shadow_insert_update_or_delete_targets() {
        let parsed = parse(
            "CREATE TABLE users (id integer);\n\
             WITH users AS (SELECT 1 AS id) INSERT INTO users SELECT * FROM users;\n\
             WITH users AS (SELECT 1 AS id) UPDATE users SET id = 2;\n\
             WITH users AS (SELECT 1 AS id) DELETE FROM users;",
        );
        assert!(!parsed.has_errors);
        let refs: Vec<_> = parsed
            .refs
            .iter()
            .filter(|reference| reference.kind == RefKind::Dependency)
            .collect();
        assert_eq!(refs.len(), 4);
        for target in [&refs[0], &refs[2], &refs[3]] {
            assert_eq!(target.name, "users");
            assert_eq!(target.path.as_deref(), Some("@sql-local:users"));
            assert_eq!(target.blocked, None);
        }
        assert_eq!(
            refs[1].blocked.as_deref(),
            Some("sql_cte"),
            "INSERT SELECT reads the CTE"
        );
    }

    #[test]
    fn postgres_functions_keep_static_sql_and_mark_opaque_bodies() {
        let parsed = parse(
            "CREATE FUNCTION app.count_users() RETURNS integer LANGUAGE sql AS $$ SELECT count(*) FROM app.users $$;\n\
             CREATE FUNCTION app.opaque() RETURNS integer LANGUAGE plpgsql AS 'BEGIN RETURN 1; END';\n\
             SELECT app.count_users();",
        );
        assert!(!parsed.has_errors);
        assert_eq!(parsed.symbols.len(), 2);
        assert!(
            parsed
                .symbols
                .iter()
                .all(|s| s.kind == SymbolKind::Function)
        );
        assert!(parsed.refs.iter().any(|r| r.name == "users"
            && r.from_symbol.as_deref() == Some("app.count_users")
            && r.blocked.is_none()));
        assert!(parsed.refs.iter().any(|r| r.blocked.as_deref()
            == Some("unsupported_sql_function_body")
            && r.from_symbol.as_deref() == Some("app.opaque")));
        assert!(
            parsed
                .refs
                .iter()
                .any(|r| r.name == "count_users" && r.kind == RefKind::Call)
        );
    }

    #[test]
    fn postgres_search_path_changes_and_dynamic_sql_do_not_guess() {
        let parsed = parse(
            "CREATE TABLE users (id integer);\n\
             SELECT * FROM users;\n\
             SET search_path TO other;\n\
             SELECT * FROM users;\n\
             SELECT * FROM app.users;",
        );
        assert!(!parsed.has_errors);
        assert_eq!(parsed.refs[0].blocked, None);
        assert_eq!(
            parsed.refs[1].blocked.as_deref(),
            Some("sql_dynamic_namespace")
        );
        assert_eq!(parsed.refs[2].blocked, None);

        let parsed = parse(
            "DO $$ BEGIN EXECUTE 'CREATE TABLE ghost(id integer)'; END $$; SELECT * FROM users;",
        );
        assert!(!parsed.symbols.iter().any(|s| s.name == "ghost"));
        assert!(
            parsed
                .refs
                .iter()
                .filter(|r| r.name == "users")
                .all(|r| r.blocked.is_some())
        );
    }

    #[test]
    fn postgres_strings_and_comments_are_not_schema_references() {
        let parsed = parse(
            "-- CREATE TABLE ghost(id integer);\n\
             SELECT 'SELECT * FROM hidden', $$ CREATE VIEW secret AS SELECT * FROM hidden $$;\n\
             CREATE TABLE visible (id integer);",
        );
        assert!(!parsed.has_errors);
        assert_eq!(parsed.symbols.len(), 1);
        assert_eq!(parsed.symbols[0].name, "visible");
        assert!(parsed.refs.is_empty());
    }
}
