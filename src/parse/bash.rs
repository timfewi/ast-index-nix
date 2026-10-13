//! Shell structure without evaluating commands, expansions or sourced files.

use std::collections::HashMap;

use tree_sitter::Node;

use super::{ParsedFile, ParsedRef, ParsedSymbol, assign_enclosing, assign_nesting};
use crate::error::{Error, Result};
use crate::model::{RefKind, SymbolKind};

struct Definition {
    byte: usize,
    end: usize,
    unconditional: bool,
}

struct Command {
    reference: usize,
    /// Function bodies run later, so their source position is not execution order.
    outer_function: Option<usize>,
}

pub(super) fn extract(source: &str, root: Node<'_>) -> Result<ParsedFile> {
    let mut parsed = ParsedFile {
        has_errors: root.has_error(),
        ..ParsedFile::default()
    };
    let mut definitions: HashMap<String, Vec<Definition>> = HashMap::new();
    let mut commands = Vec::new();
    let mut sources = Vec::new();
    let mut hazards = Vec::new();
    let mut stack = vec![(root, None)];
    while let Some((node, mut outer_function)) = stack.pop() {
        if node.kind() == "function_definition"
            && let Some(name_node) = node.child_by_field_name("name")
        {
            let name = bounded_text(name_node, source)?;
            budget(&parsed)?;
            definitions
                .entry(name.to_string())
                .or_default()
                .push(Definition {
                    byte: node.start_byte(),
                    end: node.end_byte(),
                    unconditional: node.parent().is_some_and(|p| p.kind() == "program")
                        && node.next_sibling().is_none_or(|next| next.kind() != "&"),
                });
            parsed.symbols.push(ParsedSymbol {
                name: name.to_string(),
                kind: SymbolKind::Function,
                start_line: node.start_position().row as u32 + 1,
                end_line: node.end_position().row as u32 + 1,
                start_column: node.start_position().column as u32 + 1,
                end_column: node.end_position().column as u32 + 1,
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
                parent: None,
                qualified: String::new(),
                signature: source
                    .get(
                        node.start_byte()
                            ..node
                                .child_by_field_name("body")
                                .map_or(node.end_byte(), |body| body.start_byte()),
                    )
                    .unwrap_or_default()
                    .chars()
                    .take(512)
                    .collect::<String>()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                exported: false,
            });
            outer_function = outer_function.or(Some(node.start_byte()));
        }
        if let Some(name_node) = command_name(node) {
            let literal_name = literal(bounded_text(name_node, source)?);
            let name = literal_name.clone().unwrap_or_else(|| "<dynamic>".into());
            budget(&parsed)?;
            let reference = parsed.refs.len();
            parsed.refs.push(ParsedRef {
                name: name.clone(),
                path: None,
                kind: RefKind::Call,
                line: name_node.start_position().row as u32 + 1,
                byte: name_node.start_byte(),
                from_symbol: None,
                blocked: literal_name
                    .is_none()
                    .then(|| "dynamic_shell_command".into()),
            });
            commands.push(Command {
                reference,
                outer_function,
            });

            if matches!(name.as_str(), "source" | ".") {
                let argument = source_argument(node, source);
                let path = argument
                    .map(|arg| bounded_text(arg, source).map(literal))
                    .transpose()?
                    .flatten();
                budget(&parsed)?;
                sources.push((reference, parsed.refs.len()));
                parsed.refs.push(ParsedRef {
                    name: path.clone().unwrap_or_else(|| "<dynamic>".into()),
                    path: None,
                    kind: RefKind::Import,
                    line: argument.unwrap_or(name_node).start_position().row as u32 + 1,
                    byte: argument.unwrap_or(name_node).start_byte(),
                    from_symbol: None,
                    blocked: path.is_none().then(|| "dynamic_shell_source".into()),
                });
            }
            // These operations can change the shell's function/alias namespace.
            // A delayed function may run before or after any top-level command,
            // so hazards in either context also invalidate delayed call edges.
            if literal_name.is_none()
                || mutates_namespace(&name)
                || wrapped_mutation(node, &name, source)
            {
                hazards.push(commands.len() - 1);
            }
        }
        let mut cursor = node.walk();
        stack.extend(
            node.named_children(&mut cursor)
                .map(|child| (child, outer_function)),
        );
    }

    assign_nesting(&mut parsed.symbols)?;
    assign_enclosing(&parsed.symbols, &mut parsed.refs);
    let qualified: HashMap<_, _> = parsed
        .symbols
        .iter()
        .map(|symbol| (symbol.start_byte, symbol.qualified.clone()))
        .collect();
    let first_hazard = hazards.into_iter().filter_map(|index| {
        let command = &commands[index];
        let reference = &parsed.refs[command.reference];
        // A locally defined function can shadow even eval/source/unset. Its
        // body is analyzed separately; its name alone is not a builtin effect.
        let known_function = definitions.get(&reference.name).is_some_and(|definitions| {
            matches!(definitions.as_slice(), [definition] if available(definition, command, reference.byte))
        });
        (!known_function).then_some(command.outer_function.unwrap_or(reference.byte))
    }).min();
    for command in commands {
        let reference = &mut parsed.refs[command.reference];
        if reference.blocked.is_some() {
            continue;
        }
        let candidates = definitions.get(&reference.name);
        let reason = if candidates.is_some_and(|items| items.len() > 1) {
            "ambiguous"
        } else if first_hazard
            .is_some_and(|byte| command.outer_function.is_some() || byte < reference.byte)
        {
            "dynamic_shell_environment"
        } else if let Some([definition]) = candidates.map(Vec::as_slice) {
            if !definition.unconditional {
                "conditional_shell_definition"
            } else if available(definition, &command, reference.byte) {
                reference.path = qualified.get(&definition.byte).cloned();
                continue;
            } else {
                "shell_definition_not_yet_available"
            }
        } else if is_builtin(&reference.name) {
            "shell_builtin"
        } else {
            "external_shell_command"
        };
        reference.blocked = Some(reason.into());
    }
    for (call, import) in sources {
        if definitions.contains_key(&parsed.refs[call].name) {
            parsed.refs[import].blocked = Some("shadowed_shell_builtin".into());
        }
    }
    parsed.refs.sort_by_key(|reference| reference.byte);
    Ok(parsed)
}

fn available(definition: &Definition, command: &Command, byte: usize) -> bool {
    definition.unconditional
        && (definition.end <= command.outer_function.unwrap_or(byte)
            || command.outer_function == Some(definition.byte))
}

fn budget(parsed: &ParsedFile) -> Result<()> {
    if parsed.symbols.len() + parsed.refs.len() >= 100_000 {
        Err(Error::Invalid("file extraction budget exceeded".into()))
    } else {
        Ok(())
    }
}

fn bounded_text<'a>(node: Node<'_>, source: &'a str) -> Result<&'a str> {
    if node.byte_range().len() > 4096 {
        return Err(Error::Invalid(
            "symbol/reference name exceeds 4096 bytes".into(),
        ));
    }
    Ok(source.get(node.byte_range()).unwrap_or_default())
}

fn command_name(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "command" => node.child_by_field_name("name"),
        "declaration_command" | "unset_command" => node.child(0),
        _ => None,
    }
}

fn source_argument<'a>(node: Node<'a>, source: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    let mut arguments = node.children_by_field_name("argument", &mut cursor);
    let first = arguments.next()?;
    if source.get(first.byte_range()) == Some("--") {
        arguments.next()
    } else {
        Some(first)
    }
}

/// Decode only visibly literal shell words; interpolation and shell expansion
/// never become paths or command names. No shell is started for this operation.
fn literal(text: &str) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    if let Some(text) = text.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        return (!text.contains('\'')).then(|| text.to_string());
    }
    if let Some(text) = text.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return (!text.contains(['$', '`', '\\', '"'])).then(|| text.to_string());
    }
    (!text.chars().any(|ch| {
        ch.is_whitespace()
            || matches!(
                ch,
                '$' | '`' | '\\' | '\'' | '"' | '*' | '?' | '[' | ']' | '~' | '{' | '}' | '(' | ')'
            )
    }))
    .then(|| text.to_string())
}

fn mutates_namespace(name: &str) -> bool {
    matches!(
        name,
        "eval" | "source" | "." | "unset" | "alias" | "unalias" | "enable" | "trap" | "set"
    )
}

fn wrapped_mutation(node: Node<'_>, name: &str, source: &str) -> bool {
    if !matches!(name, "command" | "builtin") {
        return false;
    }
    let mut cursor = node.walk();
    node.children_by_field_name("argument", &mut cursor)
        .any(|argument| {
            source
                .get(argument.byte_range())
                .filter(|text| text.len() <= 4096)
                .and_then(literal)
                .is_none_or(|name| mutates_namespace(&name))
        })
}

fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        ":" | "."
            | "["
            | "alias"
            | "bg"
            | "bind"
            | "break"
            | "builtin"
            | "caller"
            | "cd"
            | "command"
            | "compgen"
            | "complete"
            | "compopt"
            | "continue"
            | "declare"
            | "dirs"
            | "disown"
            | "echo"
            | "enable"
            | "eval"
            | "exec"
            | "exit"
            | "export"
            | "false"
            | "fc"
            | "fg"
            | "getopts"
            | "hash"
            | "help"
            | "history"
            | "jobs"
            | "kill"
            | "let"
            | "local"
            | "logout"
            | "mapfile"
            | "popd"
            | "printf"
            | "pushd"
            | "pwd"
            | "read"
            | "readarray"
            | "readonly"
            | "return"
            | "set"
            | "shift"
            | "shopt"
            | "source"
            | "suspend"
            | "test"
            | "times"
            | "trap"
            | "true"
            | "type"
            | "typeset"
            | "ulimit"
            | "umask"
            | "unalias"
            | "unset"
            | "wait"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> ParsedFile {
        super::super::parse(source, crate::lang::by_id("bash").expect("Bash grammar"))
            .expect("Bash extraction")
    }

    #[test]
    fn bash_functions_calls_positions_and_command_categories() {
        let parsed = parse(
            "#!/usr/bin/env bash\nhelper() { printf '%s' hi; }\nfunction main { helper; /usr/bin/id; }\nmain\n",
        );
        assert!(!parsed.has_errors);
        assert_eq!(parsed.symbols.len(), 2);
        let helper = &parsed.symbols[0];
        assert_eq!(helper.name, "helper");
        assert_eq!(helper.qualified, "helper");
        assert_eq!((helper.start_line, helper.start_column), (2, 1));
        assert_eq!(helper.signature, "helper()");
        let call = parsed.refs.iter().find(|r| r.name == "helper").unwrap();
        assert_eq!(call.path.as_deref(), Some("helper"));
        assert_eq!(call.from_symbol.as_deref(), Some("main"));
        assert_eq!(call.line, 3);
        let call = parsed.refs.iter().find(|r| r.name == "main").unwrap();
        assert_eq!(call.path.as_deref(), Some("main"));
        assert_eq!(call.from_symbol, None);
        let builtin = parsed.refs.iter().find(|r| r.name == "printf").unwrap();
        assert_eq!(builtin.blocked.as_deref(), Some("shell_builtin"));
        let external = parsed
            .refs
            .iter()
            .find(|r| r.name == "/usr/bin/id")
            .unwrap();
        assert_eq!(external.blocked.as_deref(), Some("external_shell_command"));
    }

    #[test]
    fn bash_source_paths_are_literal_and_dynamic_paths_are_retained() {
        let parsed = parse("source './lib.sh'\n. \"./other.sh\"\nsource \"$dir/lib.sh\"\n");
        let imports: Vec<_> = parsed
            .refs
            .iter()
            .filter(|r| r.kind == RefKind::Import)
            .collect();
        assert_eq!(imports.len(), 3);
        assert!(
            imports
                .iter()
                .any(|r| r.name == "./lib.sh" && r.blocked.is_none())
        );
        assert!(
            imports
                .iter()
                .any(|r| r.name == "./other.sh" && r.blocked.is_none())
        );
        assert!(imports.iter().any(|r| {
            r.name == "<dynamic>" && r.blocked.as_deref() == Some("dynamic_shell_source")
        }));
    }

    #[test]
    fn bash_respects_definition_order_duplicates_and_conditional_definitions() {
        let parsed = parse(
            "later\nlater() { :; }\nlater\nif true; then maybe() { :; }; fi\nmaybe\ndup() { :; }\ndup() { :; }\ndup\n",
        );
        let calls: Vec<_> = parsed.refs.iter().filter(|r| r.name == "later").collect();
        let before = calls.iter().find(|r| r.line == 1).unwrap();
        assert_eq!(before.path, None);
        assert_eq!(
            before.blocked.as_deref(),
            Some("shell_definition_not_yet_available")
        );
        let after = calls.iter().find(|r| r.line == 3).unwrap();
        assert_eq!(after.path.as_deref(), Some("later"));
        assert_eq!(
            parsed
                .refs
                .iter()
                .find(|r| r.name == "maybe")
                .unwrap()
                .blocked
                .as_deref(),
            Some("conditional_shell_definition")
        );
        assert_eq!(
            parsed
                .refs
                .iter()
                .find(|r| r.name == "dup")
                .unwrap()
                .blocked
                .as_deref(),
            Some("ambiguous")
        );
    }

    #[test]
    fn bash_dynamic_execution_and_namespace_mutation_do_not_guess_targets() {
        for mutation in [
            "eval \"$code\"",
            "source ./library.sh",
            "unset -f target",
            "set -o posix",
            "alias target=other",
            "trap 'unset -f target' DEBUG",
            "\"$command\"",
            "builtin eval \"$code\"",
            "command source ./library.sh",
        ] {
            let parsed = parse(&format!("target() {{ :; }}\ntarget\n{mutation}\ntarget\n"));
            let before = parsed
                .refs
                .iter()
                .find(|r| r.name == "target" && r.line == 2)
                .unwrap();
            assert_eq!(before.path.as_deref(), Some("target"), "{mutation}");
            let after = parsed
                .refs
                .iter()
                .find(|r| r.name == "target" && r.line == 4)
                .unwrap();
            assert_eq!(after.path, None, "{mutation}");
            assert_eq!(
                after.blocked.as_deref(),
                Some("dynamic_shell_environment"),
                "{mutation}"
            );
        }
    }

    #[test]
    fn bash_delayed_bodies_and_nested_definitions_are_conservative() {
        let parsed =
            parse("first() { later; }\nlater() { later; }\nouter() { inner() { :; }; inner; }\n");
        let first = parsed
            .refs
            .iter()
            .find(|r| r.name == "later" && r.line == 1)
            .unwrap();
        assert_eq!(first.path, None);
        let recursive = parsed
            .refs
            .iter()
            .find(|r| r.name == "later" && r.line == 2)
            .unwrap();
        assert_eq!(recursive.path.as_deref(), Some("later"));
        assert!(parsed.symbols.iter().any(|s| s.qualified == "outer::inner"));
        assert_eq!(
            parsed.refs.iter().find(|r| r.name == "inner").unwrap().path,
            None
        );

        let parsed = parse("helper() { :; }\nmain() { helper; }\nunset -f helper\nmain\n");
        assert_eq!(
            parsed
                .refs
                .iter()
                .find(|r| r.name == "helper")
                .unwrap()
                .path,
            None
        );
    }

    #[test]
    fn bash_quoted_commands_are_literal_but_expansions_are_not() {
        let parsed = parse("target() { :; }\n\"target\"\n'$command'\n\"$command\"\n");
        assert_eq!(
            parsed
                .refs
                .iter()
                .find(|r| r.name == "target")
                .unwrap()
                .path
                .as_deref(),
            Some("target")
        );
        assert!(parsed.refs.iter().any(
            |r| r.name == "$command" && r.blocked.as_deref() == Some("external_shell_command")
        ));
        assert!(parsed.refs.iter().any(
            |r| r.name == "<dynamic>" && r.blocked.as_deref() == Some("dynamic_shell_command")
        ));
    }

    #[test]
    fn bash_async_definitions_do_not_escape_the_child_shell() {
        for source in [
            "target() { :; } &\ntarget\n",
            "target() { :; } \\\n & # child definition\ntarget\n",
        ] {
            let parsed = parse(source);
            assert!(!parsed.has_errors);
            assert!(parsed.symbols.iter().any(|symbol| symbol.name == "target"));
            let call = parsed
                .refs
                .iter()
                .find(|reference| reference.name == "target")
                .unwrap();
            assert_eq!(call.path, None);
            assert_eq!(
                call.blocked.as_deref(),
                Some("conditional_shell_definition")
            );
        }
    }

    #[test]
    fn bash_shadowed_source_is_a_function_call_not_a_builtin_import() {
        let parsed = parse(
            "source() { printf ok; }\nsource ./not-sourced.sh\nmain() { source ./also-not-sourced.sh; }\nmain\n",
        );
        assert!(!parsed.has_errors);
        let call = parsed
            .refs
            .iter()
            .find(|reference| reference.kind == RefKind::Call && reference.name == "source")
            .unwrap();
        assert_eq!(call.path.as_deref(), Some("source"));
        let import = parsed
            .refs
            .iter()
            .find(|reference| reference.kind == RefKind::Import)
            .unwrap();
        assert_eq!(import.name, "./not-sourced.sh");
        assert_eq!(import.blocked.as_deref(), Some("shadowed_shell_builtin"));
        assert!(
            parsed
                .refs
                .iter()
                .filter(|reference| reference.kind == RefKind::Call && reference.name == "source")
                .all(|reference| reference.path.as_deref() == Some("source"))
        );
        assert!(
            parsed
                .refs
                .iter()
                .filter(|reference| reference.kind == RefKind::Import)
                .all(|reference| reference.blocked.as_deref() == Some("shadowed_shell_builtin"))
        );
    }
}
