//! End-to-end quality uses real producer output and independently authored gold.
use super::{
    RUST_CORPUS, TYPESCRIPT_CORPUS,
    evaluator::{self, Case, Location, Observations},
};
use ast_index::{
    Engine, IndexOptions,
    model::{Reference, Symbol},
    query::Query,
};
use std::{collections::BTreeSet, fs};

fn references(engine: &Engine, evidence: &str) -> Vec<Reference> {
    let mut query = Query {
        action: "refs".into(),
        kind: Some("call".into()),
        limit: Some(100),
        evidence: Some(evidence.into()),
        ..Default::default()
    };
    let mut rows = Vec::new();
    loop {
        let value = engine.execute(&query).unwrap();
        rows.extend(serde_json::from_value::<Vec<Reference>>(value["items"].clone()).unwrap());
        query.cursor = value["next_cursor"].as_str().map(str::to_owned);
        if query.cursor.is_none() {
            break;
        }
    }
    rows
}

/// Translate actual AST symbol positions to their name tokens without consulting
/// expected labels. An unrecognized declaration remains an observable wrong
/// location rather than disappearing from the precision denominator.
fn target_location(symbol: Symbol, sources: &[(&str, &str)], language: &str) -> Location {
    let mut location = Location {
        file: symbol.file.clone(),
        range: [
            symbol.start_line - 1,
            symbol.start_column - 1,
            symbol.end_line - 1,
            symbol.end_column - 1,
        ],
    };
    let source = sources
        .iter()
        .find(|(file, _)| *file == symbol.file)
        .unwrap()
        .1;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ast_index::lang::by_id(language).unwrap().language())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    let mut nodes = vec![tree.root_node()];
    while let Some(node) = nodes.pop() {
        let start = node.start_position();
        let end = node.end_position();
        if [
            start.row as u32,
            start.column as u32,
            end.row as u32,
            end.column as u32,
        ] == location.range
            && let Some(name) = node.child_by_field_name("name")
        {
            let a = name.start_position();
            let b = name.end_position();
            location.range = [a.row as u32, a.column as u32, b.row as u32, b.column as u32];
            break;
        }
        let mut cursor = node.walk();
        nodes.extend(node.named_children(&mut cursor));
    }
    location
}

fn observations(
    engine: &Engine,
    rows: &[Reference],
    sources: &[(&str, &str)],
    language: &str,
) -> Observations {
    let mut result = Observations::new();
    for row in rows {
        if let Some(target) = row.resolved {
            let symbol = engine.store().symbol_by_id(target).unwrap().unwrap();
            result
                .entry(Location {
                    file: row.file.clone(),
                    range: row.range.unwrap(),
                })
                .or_default()
                .insert(target_location(symbol, sources, language));
        }
    }
    result
}

fn report(cases: &[Case], observed: &Observations, profile: &str) {
    eprintln!("{profile}: {:?}", evaluator::score(cases, observed));
    for family in cases.iter().map(|c| &c.family).collect::<BTreeSet<_>>() {
        let selected: Vec<_> = cases
            .iter()
            .filter(|c| &c.family == family)
            .cloned()
            .collect();
        let sites: BTreeSet<_> = selected.iter().map(|c| &c.site).collect();
        let actual = observed
            .iter()
            .filter(|(site, _)| sites.contains(site))
            .map(|(s, t)| (s.clone(), t.clone()))
            .collect();
        eprintln!(
            "{profile}/{family}: {:?}",
            evaluator::score(&selected, &actual)
        );
    }
    for case in cases {
        if let Some(expected) = &case.expected
            && !observed
                .get(&case.site)
                .is_some_and(|targets| targets.contains(expected))
        {
            eprintln!("{profile} missed {}", case.id);
        }
    }
}

#[test]
fn rust_combined_profile_meets_independent_recall_precision_and_safety_gate() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    fs::create_dir_all(root.join("src")).unwrap();
    for (file, source) in RUST_CORPUS {
        fs::write(root.join(file), source).unwrap();
    }
    fs::write(
        root.join("Cargo.toml"),
        include_str!("fixtures/rust/Cargo.toml"),
    )
    .unwrap();
    fs::write(
        root.join("Cargo.lock"),
        include_str!("fixtures/rust/Cargo.lock"),
    )
    .unwrap();
    let state = directory.path().join("state");
    // The explicit runner creates private state before native indexing.
    ast_index::producer::rust(&root, &state).unwrap();
    let engine = Engine::open_state_dir(&root, &state).unwrap();
    let cases = evaluator::parse(RUST_CORPUS).unwrap();
    let baseline_rows = references(&engine, "ast");
    let baseline = observations(&engine, &baseline_rows, RUST_CORPUS, "rust");
    let combined_rows = references(&engine, "combined");
    let combined = observations(&engine, &combined_rows, RUST_CORPUS, "rust");
    report(&cases, &baseline, "rust/ast");
    report(&cases, &combined, "rust/combined");
    eprintln!(
        "all-reference links remain separate: {:?}",
        engine.status().unwrap().reference_coverage
    );
    assert!(evaluator::score(&cases, &combined).passes());
    assert_eq!(
        serde_json::to_value(references(&engine, "ast")).unwrap(),
        serde_json::to_value(&baseline_rows).unwrap(),
        "combined queries must not overwrite AST evidence"
    );
    assert!(
        combined_rows
            .iter()
            .filter(|r| r.reason == "semantic_static")
            .all(|r| r.evidence.as_ref().unwrap()["indexer"] == "rust-analyzer")
    );
    // A reindexed AST invalidates generation-bound semantic call IDs even when
    // the source did not change. An explicit refresh is required.
    engine.index(&IndexOptions::default()).unwrap();
    assert!(
        engine
            .execute(&Query {
                action: "refs".into(),
                evidence: Some("combined".into()),
                ..Default::default()
            })
            .is_err()
    );
}

/// This verifies definition navigation only. Overridable receivers can still
/// navigate to a declaration, so these observations must not become call edges.
#[test]
fn typescript_producer_definitions_match_the_frozen_navigation_labels() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    fs::create_dir_all(root.join("src")).unwrap();
    for (file, source) in TYPESCRIPT_CORPUS {
        fs::write(root.join(file), source).unwrap();
    }
    fs::write(
        root.join("tsconfig.json"),
        include_str!("fixtures/typescript/tsconfig.json"),
    )
    .unwrap();
    let state = directory.path().join("state");
    ast_index::producer::typescript(&root, &state).unwrap();
    let engine = Engine::open_state_dir(&root, &state).unwrap();
    let cases = evaluator::parse(TYPESCRIPT_CORPUS).unwrap();
    let mut observed = Observations::new();
    let semantic = |relation: &str| {
        let mut query = Query {
            action: "semantic".into(),
            relation: Some(relation.into()),
            limit: Some(100),
            ..Default::default()
        };
        let mut rows = Vec::new();
        loop {
            let result = engine.execute(&query).unwrap();
            rows.extend(result["items"].as_array().unwrap().iter().cloned());
            query.cursor = result["next_cursor"].as_str().map(str::to_owned);
            if query.cursor.is_none() {
                break;
            }
        }
        rows
    };
    let definitions = semantic("definition");
    for reference in semantic("reference") {
        for definition in definitions
            .iter()
            .filter(|d| d["identity"] == reference["identity"])
        {
            observed
                .entry(Location {
                    file: reference["file"].as_str().unwrap().into(),
                    range: serde_json::from_value(reference["range"].clone()).unwrap(),
                })
                .or_default()
                .insert(Location {
                    file: definition["file"].as_str().unwrap().into(),
                    range: serde_json::from_value(definition["range"].clone()).unwrap(),
                });
        }
    }
    let mut correct = 0;
    for case in cases.iter().filter(|c| c.expected.is_some()) {
        if observed
            .get(&case.site)
            .is_some_and(|actual| actual.contains(case.expected.as_ref().unwrap()))
        {
            correct += 1;
        } else {
            eprintln!(
                "typescript/definition missed {}: {:?}",
                case.id,
                observed.get(&case.site)
            );
        }
    }
    eprintln!(
        "typescript definition navigation: {correct}/100; static-dispatch acceptance is separate"
    );
    assert_eq!(correct, 100);
    assert!(
        engine
            .execute(&Query {
                action: "refs".into(),
                evidence: Some("combined".into()),
                ..Default::default()
            })
            .is_err(),
        "raw definition evidence is not yet a supported TypeScript call profile"
    );
}
