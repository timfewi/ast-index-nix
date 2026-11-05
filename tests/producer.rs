//! Real offline producer acceptance. Run in the pinned development shell;
//! an absent producer is a test failure, never an ignored passing result.
use ast_index::{Engine, producer, query::Query};
use std::fs;

#[test]
fn rust_producer_disables_build_scripts_and_discards_artifacts_after_import() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    fs::create_dir(&root).unwrap();
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("Cargo.toml"),
        "[package]\nname = \"synthetic-producer\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[workspace]\n").unwrap();
    fs::write(
        root.join("Cargo.lock"),
        "version = 4\n[[package]]\nname = \"synthetic-producer\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    fs::write(
        root.join("build.rs"),
        "fn main() { std::fs::write(\"/output/build-script-ran\", b\"forbidden\").unwrap(); }",
    )
    .unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "#![no_std]\npub fn target() {}\npub fn caller() { target(); }\n",
    )
    .unwrap();
    fs::write(root.join("private.json"), "SYNTHETIC_PRIVATE_CANARY").unwrap();
    let state = directory.path().join("state");
    let result = producer::rust(&root, &state).unwrap();
    assert!(result.documents >= 1 && result.occurrences >= 3);
    assert!(
        fs::read_dir(&state).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("semantic-rust-")),
        "raw snapshot and SCIP output must be removed after import"
    );
    let engine = Engine::open_state_dir(&root, &state).unwrap();
    let query = Query {
        action: "semantic".into(),
        relation: Some("definition".into()),
        ..Default::default()
    };
    let value = engine.execute(&query).unwrap();
    assert!(!value["items"].as_array().unwrap().is_empty());
    assert!(!value.to_string().contains("SYNTHETIC_PRIVATE_CANARY"));
    fs::write(root.join("Cargo.lock"), "changed lock").unwrap();
    assert_eq!(
        engine.execute(&query).unwrap()["excluded"],
        "stale_semantic_artifact"
    );
}
