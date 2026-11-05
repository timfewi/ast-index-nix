//! Opt-in call evidence: exact AST call token + SCIP identity + unique, locally
//! validated static definition. Raw references/implementations are never calls.
use super::{SOURCE_BYTES, invalid, source, symbol_key, wire};
use crate::{error::Result, store::Store};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
use tree_sitter::{Node, Point};

#[derive(Clone, Debug)]
struct Definition {
    file: String,
    range: [u32; 4],
    symbol: Option<i64>,
    project: String,
    static_dispatch: bool,
}

pub(super) struct Call {
    reference: i64,
    target: Option<i64>,
    reason: &'static str,
    confidence: &'static str,
    evidence: Value,
}

pub(crate) fn fingerprint() -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(include_bytes!("calls.rs"));
    hash.update(include_bytes!("../producer.rs"));
    hash.update(crate::lang::by_id("rust").unwrap().fingerprint().as_bytes());
    hash.finalize().to_hex().to_string()
}

fn coordinates(node: Node<'_>) -> [u32; 4] {
    let a = node.start_position();
    let b = node.end_position();
    [a.row as u32, a.column as u32, b.row as u32, b.column as u32]
}

fn rust_attributed(node: Node<'_>) -> bool {
    std::iter::successors(Some(node), |node| node.parent()).any(|node| {
        let mut previous = node.prev_named_sibling();
        while let Some(sibling) = previous {
            match sibling.kind() {
                "attribute_item" => return true,
                "line_comment" | "block_comment" => previous = sibling.prev_named_sibling(),
                _ => break,
            }
        }
        false
    })
}

/// Only a declaration's actual name token can identify its callable body.
/// Trait/default methods and trait implementations deliberately remain possible
/// targets. Inherent Rust methods cannot be overridden by a runtime subtype.
fn rust_definition(root: Node<'_>, range: [u32; 4]) -> Option<(Node<'_>, bool)> {
    let token = root.named_descendant_for_point_range(
        Point::new(range[0] as usize, range[1] as usize),
        Point::new(range[2] as usize, range[3] as usize),
    )?;
    if coordinates(token) != range {
        return None;
    }
    let declaration = token.parent()?;
    if declaration.child_by_field_name("name") != Some(token) {
        return None;
    }
    let callable = match declaration.kind() {
        "function_item" => true,
        "struct_item" => declaration
            .child_by_field_name("body")
            .is_some_and(|body| body.kind() == "ordered_field_declaration_list"),
        _ => false,
    };
    let dynamic = std::iter::successors(declaration.parent(), |node| node.parent()).any(|node| {
        node.kind() == "trait_item"
            || (node.kind() == "impl_item" && node.child_by_field_name("trait").is_some())
    });
    // An unknown attribute macro can replace the declaration. This profile
    // does not expand procedural macros, so it cannot establish that target.
    let attributed = rust_attributed(declaration);
    Some((declaration, callable && !dynamic && !attributed))
}

/// Canonical UTF-8 coordinates; the importer has validated bounds and scalar
/// boundaries first. Keep the conversion bounded even for long Unicode lines.
fn utf8_range(
    occurrence: &wire::Occurrence,
    lines: &[&str],
    encoding: i32,
    budget: &mut u64,
) -> Result<[u32; 4]> {
    let mut range = super::range(occurrence, lines, encoding, budget)?;
    if encoding != 1 {
        for (row, column) in [(0, 1), (2, 3)] {
            let line = lines[range[row] as usize]
                .strip_suffix('\r')
                .unwrap_or(lines[range[row] as usize]);
            let mut units = 0;
            let mut bytes = 0;
            for ch in line.chars() {
                if units == range[column] {
                    break;
                }
                units += if encoding == 2 {
                    ch.len_utf16() as u32
                } else {
                    1
                };
                bytes += ch.len_utf8() as u32;
            }
            range[column] = bytes;
        }
    }
    Ok(range)
}

pub(super) fn build(
    root: &Path,
    store: &Store,
    documents: &[(wire::Document, String)],
    tool: &wire::ToolInfo,
    digest: &str,
    inputs: Option<&std::collections::HashMap<String, String>>,
) -> Result<Vec<Call>> {
    // A producer/version outside an exercised profile remains navigable as raw
    // semantic evidence but cannot change the call graph.
    if tool.name != "rust-analyzer" || tool.version != "2026-09-21" {
        return Ok(Vec::new());
    }
    let mut definitions = BTreeMap::<String, Vec<Definition>>::new();
    let mut sites = BTreeMap::<(String, [u32; 4]), BTreeSet<String>>::new();
    let mut malformed = BTreeSet::new();
    let mut profile_files = BTreeSet::new();
    if let Some(inputs) = inputs {
        for file in inputs.keys() {
            if store
                .file_state(file)?
                .is_some_and(|state| state.lang == "rust")
            {
                profile_files.insert(file.clone());
            }
        }
    }
    let mut budget = SOURCE_BYTES;
    let mut total = 0;
    for (document, hash) in documents
        .iter()
        .filter(|(d, _)| d.language.to_lowercase() == "rust")
    {
        let bytes = source(
            root,
            &document.relative_path,
            SOURCE_BYTES.saturating_sub(total),
        )?;
        total += bytes.len() as u64;
        if blake3::hash(&bytes).to_hex().as_str() != hash {
            return Err(invalid("source changed while validating semantic calls"));
        }
        let text =
            std::str::from_utf8(&bytes).map_err(|_| invalid("semantic source must be UTF-8"))?;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::lang::by_id("rust").unwrap().language())
            .map_err(|_| invalid("cannot initialize semantic call parser"))?;
        let tree = parser
            .parse(text, None)
            .ok_or_else(|| invalid("semantic call parse cancelled"))?;
        let lines: Vec<_> = text.split('\n').collect();
        let file = &document.relative_path;
        profile_files.insert(file.clone());
        let state = store
            .file_state(file)?
            .ok_or_else(|| invalid("semantic document must be indexed"))?;
        if state.fingerprint != crate::lang::by_id("rust").unwrap().fingerprint() {
            return Err(invalid(
                "AST extraction profile changed; reindex before importing semantic calls",
            ));
        }
        if tree.root_node().has_error() {
            malformed.insert(file.clone());
        }
        let mut positions = BTreeMap::<(u32, u32), Option<i64>>::new();
        let mut statement = store.conn.prepare("SELECT s.id,s.start_line,s.start_column FROM symbols s JOIN files f ON f.id=s.file_id WHERE f.path=?1 LIMIT 100001")?;
        for (count, row) in statement
            .query_map([file], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, u32>(1)?,
                    r.get::<_, u32>(2)?,
                ))
            })?
            .enumerate()
        {
            if count >= 100_000 {
                return Err(invalid("semantic definition budget exceeded"));
            }
            let (id, line, column) = row?;
            positions
                .entry((line, column))
                .and_modify(|id| *id = None)
                .or_insert(Some(id));
        }
        for occurrence in &document.occurrences {
            if occurrence.symbol.is_empty() {
                continue;
            }
            let range = utf8_range(occurrence, &lines, document.position_encoding, &mut budget)?;
            let identity = symbol_key(file, &occurrence.symbol)?;
            if occurrence.symbol_roles & 1 != 0 {
                let (declaration, static_dispatch) = rust_definition(tree.root_node(), range)
                    .map_or((None, false), |(node, proven)| {
                        (Some(node), proven && !tree.root_node().has_error())
                    });
                let symbol = if let Some(node) = declaration {
                    let start = node.start_position();
                    positions
                        .get(&(start.row as u32 + 1, start.column as u32 + 1))
                        .copied()
                        .flatten()
                } else {
                    None
                };
                definitions.entry(identity).or_default().push(Definition {
                    file: file.clone(),
                    range,
                    symbol,
                    project: state.project.clone(),
                    static_dispatch,
                });
            } else if !tree
                .root_node()
                .named_descendant_for_point_range(
                    Point::new(range[0] as usize, range[1] as usize),
                    Point::new(range[2] as usize, range[3] as usize),
                )
                .is_some_and(rust_attributed)
            {
                sites
                    .entry((file.clone(), range))
                    .or_default()
                    .insert(identity);
            }
        }
    }
    let mut calls = Vec::new();
    let mut reference = store.conn.prepare("SELECT r.id,r.resolved_symbol_id,f.project FROM refs r JOIN files f ON f.id=r.file_id WHERE f.path=?1 AND r.kind='call' AND r.source_range=?2")?;
    for ((file, range), identities) in sites {
        let refs: Vec<(i64, Option<i64>, String)> = reference
            .query_map(params![file, serde_json::to_string(&range)?], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        if refs.len() != 1 {
            continue;
        }
        let (id, ast, project) = &refs[0];
        let target_count: usize = identities
            .iter()
            .map(|identity| definitions.get(identity).map_or(0, Vec::len))
            .sum();
        // An artifact may repeat one identity at many definitions and call
        // sites. Never expand that into a quadratic candidate cross product.
        let targets: Vec<_> = identities
            .iter()
            .flat_map(|identity| definitions.get(identity).into_iter().flatten())
            .take(8)
            .collect();
        let unique = if identities.len() == 1 && target_count == 1 {
            Some(targets[0])
        } else {
            None
        };
        let admitted = unique
            .filter(|d| d.static_dispatch && d.project == *project && !malformed.contains(&file))
            .and_then(|d| d.symbol);
        let conflict = admitted.is_some() && ast.is_some() && admitted != *ast;
        let reason = if conflict {
            "semantic_conflict"
        } else if admitted.is_some() {
            "semantic_static"
        } else if identities.len() > 1 || target_count > 1 {
            "semantic_ambiguous"
        } else {
            "semantic_possible"
        };
        let target = if conflict { None } else { admitted };
        let confidence = if target.is_none() {
            "none"
        } else if unique.is_some_and(|d| d.file == file) {
            "same_file"
        } else {
            "same_project"
        };
        let possible: Vec<_> = targets
            .iter()
            .take(8)
            .map(|d| json!({"file":d.file,"range":d.range,"symbol":d.symbol}))
            .collect();
        calls.push(Call {reference:*id, target, reason, confidence, evidence:json!({
            "kind":"scip_static_call", "state":reason, "indexer":tool.name,"version":tool.version,
            "artifact_blake3":digest,"ast_target":ast,"possible_targets":possible,
            "possible_target_count":target_count, "identity_count":identities.len(),
            "position_encoding":"utf8", "target":target.and(unique).map(|d| json!({"file":d.file,"range":d.range,"symbol":d.symbol}))
        })});
    }
    // Absence of a semantic occurrence does not validate an AST guess. In
    // particular cfg-disabled declarations must not reappear through fallback.
    // Other languages and files outside this artifact's declared input profile
    // retain their AST view; those edges have no semantic provenance.
    let matched: BTreeSet<_> = calls.iter().map(|c| c.reference).collect();
    let mut statement = store.conn.prepare("SELECT r.id,r.resolved_symbol_id FROM refs r JOIN files f ON f.id=r.file_id WHERE f.path=?1 AND r.kind='call' LIMIT 100001")?;
    for file in profile_files {
        for row in statement.query_map([file], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
        })? {
            let (reference, ast) = row?;
            if matched.contains(&reference) {
                continue;
            }
            if calls.len() >= 100_000 {
                return Err(invalid("semantic call overlay budget exceeded"));
            }
            calls.push(Call {reference,target:None,reason:"semantic_unverified",confidence:"none",evidence:json!({
                "kind":"scip_static_call", "state":"semantic_unverified", "indexer":tool.name,"version":tool.version,
                "artifact_blake3":digest,"ast_target":ast,"possible_targets":[],"possible_target_count":0,
                "identity_count":0,"position_encoding":"utf8","target":null
            })});
        }
    }
    Ok(calls)
}

pub(super) fn persist(store: &Store, calls: &[Call], supported: bool) -> Result<()> {
    store.conn.execute("DELETE FROM semantic_calls", [])?;
    for call in calls {
        store.conn.execute("INSERT INTO semantic_calls(ref_id,target_id,reason,confidence,evidence) VALUES(?1,?2,?3,?4,?5)", params![call.reference,call.target,call.reason,call.confidence,serde_json::to_string(&call.evidence)?])?;
    }
    store.set_meta("semantic_call_generation", &store.generation()?.to_string())?;
    store.set_meta(
        "semantic_call_profile",
        if supported {
            "rust-analyzer-2026-09-21"
        } else {
            "unsupported"
        },
    )?;
    store.set_meta("semantic_call_fingerprint", &fingerprint())?;
    Ok(())
}

pub(crate) fn require_current(root: &Path, store: &Store) -> Result<Value> {
    if store.meta("semantic_call_profile")?.as_deref() != Some("rust-analyzer-2026-09-21") {
        return Err(invalid(
            "no supported semantic call profile; refresh with the pinned Rust runner",
        ));
    }
    if store.meta("semantic_call_generation")?.as_deref() != Some(&store.generation()?.to_string())
        || store.meta("semantic_call_fingerprint")?.as_deref() != Some(&fingerprint())
    {
        return Err(invalid(
            "semantic call overlay no longer matches the AST generation/profile; refresh semantic evidence",
        ));
    }
    if !super::stale_files(root, store)?.is_empty() {
        return Err(invalid(
            "stale semantic call overlay; source/configuration changed, refresh semantic evidence",
        ));
    }
    let generation = store.generation()?;
    let provenance: Option<Value> = store.conn.query_row("SELECT artifact,indexer,indexer_version FROM semantic_documents ORDER BY file LIMIT 1", [], |r| Ok(json!({
        "mode":"combined", "artifact_blake3":r.get::<_,String>(0)?, "indexer":r.get::<_,String>(1)?, "version":r.get::<_,String>(2)?,
        "generation":generation, "freshness":"all artifact and manifest inputs matched current bytes"
    }))).optional()?;
    provenance.ok_or_else(|| invalid("semantic call profile has no artifact"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Engine, IndexOptions, query::Query};
    use prost::Message;

    fn fixture(files: &[(&str, &str)]) -> (tempfile::TempDir, Engine) {
        let dir = tempfile::tempdir().unwrap();
        for (file, text) in files {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let engine = Engine::open(dir.path(), None).unwrap();
        engine.index(&IndexOptions::default()).unwrap();
        (dir, engine)
    }

    fn occurrence(
        text: &str,
        needle: &str,
        nth: usize,
        symbol: &str,
        definition: bool,
        encoding: i32,
    ) -> wire::Occurrence {
        let byte = text.match_indices(needle).nth(nth).unwrap().0;
        let prefix = &text[..byte];
        let row = prefix.bytes().filter(|&b| b == b'\n').count() as i32;
        let line = prefix.rsplit('\n').next().unwrap();
        let units = |s: &str| match encoding {
            1 => s.len(),
            2 => s.encode_utf16().count(),
            _ => s.chars().count(),
        } as i32;
        let start = units(line);
        wire::Occurrence {
            range: vec![row, start, start + units(needle)],
            symbol: symbol.into(),
            symbol_roles: i32::from(definition),
            ..Default::default()
        }
    }

    fn document(
        file: &str,
        text: &str,
        encoding: i32,
        occurrences: Vec<wire::Occurrence>,
    ) -> wire::Document {
        wire::Document {
            relative_path: file.into(),
            language: "rust".into(),
            text: text.into(),
            position_encoding: encoding,
            occurrences,
            ..Default::default()
        }
    }

    fn import(engine: &Engine, documents: Vec<wire::Document>, version: &str) {
        let index = wire::Index {
            metadata: Some(wire::Metadata {
                version: 0,
                project_root: "file:///synthetic".into(),
                text_document_encoding: 1,
                tool_info: Some(wire::ToolInfo {
                    name: "rust-analyzer".into(),
                    version: version.into(),
                }),
            }),
            documents,
        };
        let artifact = engine.root().join("fixture.scip");
        std::fs::write(&artifact, index.encode_to_vec()).unwrap();
        super::super::import(engine.root(), engine.store(), &artifact, None).unwrap();
    }

    fn request(action: &str, name: Option<&str>) -> Query {
        Query {
            action: action.into(),
            query: name.map(str::to_owned),
            evidence: Some("combined".into()),
            ..Default::default()
        }
    }

    #[test]
    fn static_dispatch_requires_an_exact_inherent_or_free_definition() {
        let source = "fn free() {}\nstruct Item(u8);\nimpl Item { fn method(&self) {} }\ntrait T { fn dynamic(&self) {} }\nimpl T for Item { fn overridden(&self) {} }\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::lang::by_id("rust").unwrap().language())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        for (line, name, expected) in [
            (0, "free", true),
            (1, "Item", true),
            (2, "method", true),
            (3, "dynamic", false),
            (4, "overridden", false),
        ] {
            let text = source.lines().nth(line).unwrap();
            let col = text.find(name).unwrap();
            let range = [
                line as u32,
                col as u32,
                line as u32,
                (col + name.len()) as u32,
            ];
            assert_eq!(
                rust_definition(tree.root_node(), range).unwrap().1,
                expected,
                "{name}"
            );
            let mut wrong = range;
            wrong[3] += 1;
            assert!(rust_definition(tree.root_node(), wrong).is_none());
        }
    }

    #[test]
    fn conflicts_and_multiple_semantic_targets_never_choose_a_winner() {
        let text = "fn alpha() {}\nfn beta() {}\nfn caller() { alpha(); }\n";
        let (_dir, engine) = fixture(&[("lib.rs", text)]);
        let defs = vec![
            occurrence(text, "alpha", 0, "alpha", true, 1),
            occurrence(text, "beta", 0, "beta", true, 1),
        ];
        let mut occurrences = defs.clone();
        occurrences.push(occurrence(text, "alpha", 1, "beta", false, 1));
        import(
            &engine,
            vec![document("lib.rs", text, 1, occurrences.clone())],
            "2026-09-21",
        );
        let refs = engine.execute(&request("refs", None)).unwrap();
        assert_eq!(refs["items"][0]["reason"], "semantic_conflict");
        assert!(refs["items"][0]["resolved"].is_null());
        assert!(refs["items"][0]["evidence"]["ast_target"].is_number());
        assert_eq!(refs["items"][0]["evidence"]["possible_target_count"], 1);
        assert!(
            engine.execute(&request("callees", Some("caller"))).unwrap()["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            engine.execute(&request("impact", Some("alpha"))).unwrap()["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let ast = engine
            .execute(&Query {
                action: "callees".into(),
                query: Some("caller".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(ast["items"][0]["callee"], "alpha");
        occurrences.push(occurrence(text, "alpha", 1, "alpha", false, 1));
        import(
            &engine,
            vec![document("lib.rs", text, 1, occurrences)],
            "2026-09-21",
        );
        let refs = engine.execute(&request("refs", None)).unwrap();
        assert_eq!(refs["items"][0]["reason"], "semantic_ambiguous");
        assert_eq!(refs["items"][0]["evidence"]["identity_count"], 2);
        assert!(refs["items"][0]["resolved"].is_null());
        let mut repeated = vec![defs[1].clone(); 20];
        repeated.push(occurrence(text, "alpha", 1, "beta", false, 1));
        import(
            &engine,
            vec![document("lib.rs", text, 1, repeated)],
            "2026-09-21",
        );
        let refs = engine.execute(&request("refs", None)).unwrap();
        assert_eq!(refs["items"][0]["reason"], "semantic_ambiguous");
        assert_eq!(refs["items"][0]["evidence"]["possible_target_count"], 20);
        assert_eq!(
            refs["items"][0]["evidence"]["possible_targets"]
                .as_array()
                .unwrap()
                .len(),
            8
        );
    }

    #[test]
    fn utf16_crlf_and_same_line_calls_join_exactly_and_drive_both_graph_directions() {
        let text = "struct Counter;\r\nimpl Counter { /* 😀 */ fn get(&self) {} }\r\nfn caller(value: &Counter) { let _ = \"😀\"; value.get(); value.get(); }\r\n";
        let (_dir, engine) = fixture(&[("lib.rs", text)]);
        let occurrences = (0..3)
            .map(|n| occurrence(text, "get", n, "getter", n == 0, 2))
            .collect();
        import(
            &engine,
            vec![document("lib.rs", text, 2, occurrences)],
            "2026-09-21",
        );
        let refs = engine.execute(&request("refs", None)).unwrap();
        let items = refs["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_ne!(items[0]["range"], items[1]["range"]);
        assert!(items.iter().all(|r| r["reason"] == "semantic_static"));
        let expected = occurrence(text, "get", 0, "getter", true, 1).range;
        assert_eq!(
            items[0]["evidence"]["target"]["range"],
            json!([expected[0], expected[1], expected[0], expected[2]])
        );
        let callers = engine
            .execute(&request("callers", Some("Counter::get")))
            .unwrap();
        let callees = engine.execute(&request("callees", Some("caller"))).unwrap();
        assert_eq!(callers["items"], callees["items"]);
        assert_eq!(callers["items"].as_array().unwrap().len(), 2);
        let impact = engine
            .execute(&request("impact", Some("Counter::get")))
            .unwrap();
        assert_eq!(impact["items"].as_array().unwrap().len(), 1);
        assert_eq!(impact["items"][0]["symbol"]["name"], "caller");
        assert_eq!(impact["evidence"]["indexer"], "rust-analyzer");
        std::fs::write(
            engine.root().join("lib.rs"),
            text.replace("Counter", "Changed"),
        )
        .unwrap();
        assert!(
            engine
                .execute(&request("refs", None))
                .unwrap_err()
                .to_string()
                .contains("stale")
        );
    }

    #[test]
    fn nonexact_occurrences_unknown_profiles_and_malformed_sources_cannot_promote_calls() {
        let text =
            "struct Counter; impl Counter { fn get(&self) {} } fn caller(c: &Counter) { c.get(); }";
        let (_dir, engine) = fixture(&[("lib.rs", text)]);
        let definition = occurrence(text, "get", 0, "getter", true, 1);
        let reference = occurrence(text, "get", 1, "getter", false, 1);
        let mut wrong = reference.clone();
        wrong.range[2] += 1;
        import(
            &engine,
            vec![document("lib.rs", text, 1, vec![definition.clone(), wrong])],
            "2026-09-21",
        );
        assert!(engine.execute(&request("refs", None)).unwrap()["items"][0]["resolved"].is_null());
        let doc = document(
            "lib.rs",
            text,
            1,
            vec![definition.clone(), reference.clone()],
        );
        import(&engine, vec![doc.clone()], "unknown");
        assert!(engine.execute(&request("refs", None)).is_err());
        assert!(
            !engine
                .execute(&Query {
                    action: "semantic".into(),
                    ..Default::default()
                })
                .unwrap()["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let malformed = format!("{text}\nfn broken(");
        std::fs::write(engine.root().join("lib.rs"), &malformed).unwrap();
        engine.index(&IndexOptions::default()).unwrap();
        import(
            &engine,
            vec![document(
                "lib.rs",
                &malformed,
                1,
                vec![definition, reference],
            )],
            "2026-09-21",
        );
        assert!(engine.execute(&request("refs", None)).unwrap()["items"][0]["resolved"].is_null());
    }

    #[test]
    fn project_boundaries_and_attribute_macros_block_static_admission() {
        let target = "#[unknown_attribute]\nfn target() {}\nfn caller() { target(); }";
        let (_dir, engine) = fixture(&[("lib.rs", target)]);
        import(
            &engine,
            vec![document(
                "lib.rs",
                target,
                1,
                vec![
                    occurrence(target, "target", 0, "target", true, 1),
                    occurrence(target, "target", 1, "target", false, 1),
                ],
            )],
            "2026-09-21",
        );
        assert_eq!(
            engine.execute(&request("refs", None)).unwrap()["items"][0]["reason"],
            "semantic_possible"
        );
        let a = "fn caller() { target(); }";
        let b = "fn target() {}";
        let (_dir, engine) = fixture(&[
            ("a/lib.rs", a),
            ("b/lib.rs", b),
            ("a/Cargo.toml", "[package]\nname=\"a\""),
            ("b/Cargo.toml", "[package]\nname=\"b\""),
        ]);
        import(
            &engine,
            vec![
                document(
                    "a/lib.rs",
                    a,
                    1,
                    vec![occurrence(a, "target", 0, "target", false, 1)],
                ),
                document(
                    "b/lib.rs",
                    b,
                    1,
                    vec![occurrence(b, "target", 0, "target", true, 1)],
                ),
            ],
            "2026-09-21",
        );
        let value = engine.execute(&request("refs", None)).unwrap();
        assert_eq!(value["items"][0]["reason"], "semantic_possible");
        assert!(value["items"][0]["resolved"].is_null());
    }
}
