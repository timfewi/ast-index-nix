//! Synthetic resolution quality oracle. Producer integration is added alongside
//! the semantic adapters; these tests verify that its scorer cannot hide gaps.
#[path = "quality/acceptance.rs"]
mod acceptance;
#[path = "quality/evaluator.rs"]
mod evaluator;

const RUST_CORPUS: &[(&str, &str)] = &[
    (
        "src/lib.rs",
        include_str!("quality/fixtures/rust/src/lib.rs"),
    ),
    (
        "src/ops.rs",
        include_str!("quality/fixtures/rust/src/ops.rs"),
    ),
];

const TYPESCRIPT_CORPUS: &[(&str, &str)] = &[
    (
        "src/main.ts",
        include_str!("quality/fixtures/typescript/src/main.ts"),
    ),
    (
        "src/ops.ts",
        include_str!("quality/fixtures/typescript/src/ops.ts"),
    ),
    (
        "src/facade.ts",
        include_str!("quality/fixtures/typescript/src/facade.ts"),
    ),
];

#[test]
fn rust_gold_labels_cover_distinct_calls_and_negative_cases() {
    validate_corpus(RUST_CORPUS, "rust", 101, 2);
}

#[test]
fn typescript_gold_labels_cover_distinct_calls_and_negative_cases() {
    validate_corpus(TYPESCRIPT_CORPUS, "typescript", 100, 3);
}

fn validate_corpus(files: &[(&str, &str)], language: &str, eligible: usize, denied: usize) {
    use std::collections::{BTreeMap, BTreeSet};
    let cases = evaluator::parse(files).unwrap();
    assert_eq!(
        cases.iter().filter(|c| c.expected.is_some()).count(),
        eligible
    );
    assert_eq!(
        cases.iter().filter(|c| c.expected.is_none()).count(),
        denied
    );
    let mut families = BTreeMap::<_, usize>::new();
    for case in &cases {
        *families.entry(&case.family).or_default() += 1;
    }
    assert_eq!(families.len(), 11);
    assert_eq!(families.get(&"heldout".to_string()), Some(&10));
    let labelled: BTreeSet<_> = cases.iter().map(|c| c.site.clone()).collect();
    let mut extracted = BTreeSet::new();
    for (file, source) in files {
        let parsed =
            ast_index::parse::parse(source, ast_index::lang::by_id(language).unwrap()).unwrap();
        assert!(!parsed.has_errors, "{file}");
        for reference in parsed
            .refs
            .into_iter()
            .filter(|r| r.kind == ast_index::model::RefKind::Call)
        {
            extracted.insert(evaluator::Location {
                file: (*file).into(),
                range: reference.range,
            });
        }
    }
    assert!(
        labelled == extracted,
        "missing labels: {:?}; missing extraction: {:?}",
        extracted.difference(&labelled).collect::<Vec<_>>(),
        cases
            .iter()
            .filter(|c| !extracted.contains(&c.site))
            .map(|c| &c.id)
            .collect::<Vec<_>>()
    );
    assert!(!evaluator::score(&cases, &evaluator::Observations::new()).passes());
}
