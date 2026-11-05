//! Real offline producer acceptance. Run in the pinned development shell;
//! an absent producer is a test failure, never an ignored passing result.
use ast_index::{Engine, producer, query::Query};
use std::fs;

#[path = "producer/dependencies.rs"]
mod dependencies;
#[path = "producer/typescript.rs"]
mod typescript;

#[test]
fn typescript_producer_uses_native_definitions_without_scripts_or_installation() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("tsconfig.json"),r#"{"compilerOptions":{"strict":true,"target":"ES2022","module":"ESNext"},"include":["main.ts"]}"#).unwrap();
    fs::write(
        root.join("unloaded.ts"),
        "export function skipped() {} export function offcaller() { skipped(); }",
    )
    .unwrap();
    fs::write(
        root.join("ops.ts"),
        "export function /* 😀 */ target():number {return 1;}\r\n",
    )
    .unwrap();
    fs::write(
        root.join("main.ts"),
        "import {target} from './ops'; export function caller() {const marker = '😀'; return target()+target();}\r\n",
    )
    .unwrap();
    fs::write(
        root.join("package.json"),
        r#"{"name":"synthetic-typescript","scripts":{"prepare":"touch /output/script-ran"}}"#,
    )
    .unwrap();
    fs::write(root.join("private.json"), "SYNTHETIC_PRIVATE_CANARY").unwrap();
    let state = directory.path().join("state");
    let stats = producer::typescript(&root, &state, &Default::default()).unwrap();
    assert_eq!(stats.documents, 2);
    assert_eq!(stats.occurrences, 3);
    assert!(fs::read_dir(&state).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("semantic-typescript-")
    }));
    let engine = Engine::open_state_dir(&root, &state).unwrap();
    let query = Query {
        action: "semantic".into(),
        query: Some("target".into()),
        relation: Some("definition".into()),
        ..Default::default()
    };
    let value = engine.execute(&query).unwrap();
    assert!(!value["items"].as_array().unwrap().is_empty());
    assert!(!value.to_string().contains("SYNTHETIC_PRIVATE_CANARY"));
    assert_eq!(
        value["items"][0]["range"],
        serde_json::json!([0, 27, 0, 33])
    );
    let references = engine
        .execute(&Query {
            action: "semantic".into(),
            query: Some("target".into()),
            relation: Some("reference".into()),
            ..Default::default()
        })
        .unwrap();
    assert_ne!(
        references["items"][0]["range"],
        references["items"][1]["range"]
    );
    fs::write(root.join("package.json"), "{}").unwrap();
    assert_eq!(
        engine.execute(&query).unwrap()["excluded"],
        "stale_semantic_artifact"
    );
    fs::write(
        root.join("main.ts"),
        "import {missing} from './unavailable_private_canary'; missing();",
    )
    .unwrap();
    let error = producer::typescript(&root, &state, &Default::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("offline TypeScript check failed"));
    assert!(!error.contains("unavailable_private_canary"));
    assert!(fs::read_dir(&state).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("semantic-typescript-")
    }));
}

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
    let result = producer::rust(&root, &state, &Default::default()).unwrap();
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

#[test]
fn real_rust_producer_abstains_for_dynamic_values_traits_and_unknown_attributes() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("Cargo.toml"),"[package]\nname=\"synthetic-negative\"\nversion=\"0.0.0\"\nedition=\"2021\"\n[workspace]\n").unwrap();
    fs::write(
        root.join("Cargo.lock"),
        "version=4\n[[package]]\nname=\"synthetic-negative\"\nversion=\"0.0.0\"\n",
    )
    .unwrap();
    fs::write(
        root.join("src/lib.rs"),
        r#"#![no_std]
pub fn target() {}
pub trait Dynamic { fn tick(&self) {} }
pub fn pointer(mut callback: fn(), choose: bool) {
    if choose { callback = target; }
    callback();
}
pub fn dynamic(value: &dyn Dynamic) { value.tick(); }
pub fn generic<T: Dynamic>(value: &T) { value.tick(); }
#[unknown_attribute]
pub fn transformed() {}
pub fn unknown_macro() { transformed(); }
#[cfg(any())]
pub fn inactive() {}
pub fn missing_config() { inactive(); }
#[unknown_attribute]
// Comments do not detach an attribute from its enclosing module.
pub mod transformed_module { pub fn nested_target() {} }
pub fn module_macro() { transformed_module::nested_target(); }
#[unknown_attribute]
pub fn transformed_caller() { target(); }
"#,
    )
    .unwrap();
    let state = directory.path().join("state");
    fs::write(
        root.join("src/unloaded.rs"),
        "pub fn skipped() {} pub fn offcaller() { skipped(); }",
    )
    .unwrap();
    producer::rust(&root, &state, &Default::default()).unwrap();
    let engine = Engine::open_state_dir(&root, &state).unwrap();
    for name in [
        "pointer",
        "dynamic",
        "generic",
        "unknown_macro",
        "missing_config",
        "offcaller",
        "module_macro",
        "transformed_caller",
    ] {
        let value = engine
            .execute(&Query {
                action: "callees".into(),
                query: Some(name.into()),
                evidence: Some("combined".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(
            value["items"].as_array().unwrap().is_empty(),
            "{name}: {value}"
        );
    }
}
