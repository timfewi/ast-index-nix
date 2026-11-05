//! Authored dependency trees exercise real producers without a registry,
//! installer, personal cache or external project. Checksums are synthetic.
use ast_index::{
    Engine,
    producer::{self, Options},
    query::Query,
};
use std::{fs, path::Path};

fn write(root: &Path, file: &str, content: &str) {
    let path = root.join(file);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn callees() -> Query {
    Query {
        action: "callees".into(),
        query: Some("caller".into()),
        evidence: Some("combined".into()),
        ..Default::default()
    }
}

#[test]
fn rust_uses_only_explicit_vendored_inputs_without_running_dependency_build_scripts() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    write(
        &root,
        "Cargo.toml",
        "[package]\nname=\"synthetic-vendor-host\"\nversion=\"0.0.0\"\nedition=\"2021\"\n[workspace]\n[dependencies]\nsynthetic-dependency=\"=0.0.0\"\n",
    );
    write(
        &root,
        "Cargo.lock",
        "version=4\n[[package]]\nname=\"synthetic-vendor-host\"\nversion=\"0.0.0\"\ndependencies=[\"synthetic-dependency\"]\n[[package]]\nname=\"synthetic-dependency\"\nversion=\"0.0.0\"\nsource=\"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum=\"0000000000000000000000000000000000000000000000000000000000000000\"\n",
    );
    write(
        &root,
        "src/lib.rs",
        "#![no_std]\nuse synthetic_dependency::External;\npub fn helper(_: External) {}\npub fn caller() {helper(External);}\n",
    );
    write(
        &root,
        "vendor/synthetic-dependency/Cargo.toml",
        "[package]\nname=\"synthetic-dependency\"\nversion=\"0.0.0\"\nedition=\"2021\"\n",
    );
    write(
        &root,
        "vendor/synthetic-dependency/src/lib.rs",
        "#![no_std]\npub struct External;\n",
    );
    write(
        &root,
        "vendor/synthetic-dependency/build.rs",
        "fn main() {std::fs::write(\"/output/dependency-script-ran\", b\"forbidden\").unwrap();}\n",
    );
    write(
        &root,
        "vendor/synthetic-dependency/.cargo-checksum.json",
        r#"{"files":{"Cargo.toml":"fdb75ca9435d27bed8e8d26bc0b85ca08d4d395a573470d0670623a90b3dd50b","src/lib.rs":"ed20bddb15728b84c56dc4d508ea10b650f39a686f78d175a306fd4f46e1bc10","build.rs":"b14692038e9ae11fbbf38416d6bc77438f4ffe3ce7d4b0d1cb77c74fa415d367"},"package":"0000000000000000000000000000000000000000000000000000000000000000"}"#,
    );
    write(&root, ".gitignore", "vendor/\n");
    fs::create_dir(root.join(".git")).unwrap();
    let state = directory.path().join("state");
    let error = producer::rust(&root, &state, &Options::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("offline Cargo metadata failed"), "{error}");
    let enabled = Options { dependencies: true };
    producer::rust(&root, &state, &enabled).unwrap();
    assert!(
        !root.join(".cargo").exists(),
        "only the disposable snapshot receives runner configuration"
    );
    let engine = Engine::open_state_dir(&root, &state).unwrap();
    let result = engine.execute(&callees()).unwrap();
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert_eq!(result["items"][0]["callee"], "helper");
    let generation = engine.store().generation().unwrap();
    assert!(producer::rust(&root, &state, &Options::default()).is_err());
    assert_eq!(engine.store().generation().unwrap(), generation);
    assert!(
        engine.execute(&callees()).is_ok(),
        "a failed refresh preserves existing valid evidence"
    );
    write(
        &root,
        "vendor/synthetic-dependency/src/lib.rs",
        "#![no_std]\npub struct Changed;\n",
    );
    assert!(
        engine.execute(&callees()).is_err(),
        "dependency content is a freshness input"
    );
    assert!(fs::read_dir(&state).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("semantic-rust-")
    }));
}

#[test]
fn typescript_loads_explicit_dependency_types_and_invalidates_changed_or_added_inputs() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project");
    write(
        &root,
        "tsconfig.json",
        r#"{"compilerOptions":{"strict":true,"module":"ESNext","moduleResolution":"Bundler","target":"ES2022"},"include":["main.ts"]}"#,
    );
    write(
        &root,
        "main.ts",
        "import {external} from 'synthetic-dependency';\nexport function helper() {return 1;}\nexport function caller() {external(); return helper();}\n",
    );
    write(
        &root,
        "node_modules/synthetic-dependency/package.json",
        r#"{"name":"synthetic-dependency","version":"0.0.0","types":"index.d.ts","scripts":{"postinstall":"touch /output/dependency-script-ran"}}"#,
    );
    write(
        &root,
        "node_modules/synthetic-dependency/index.d.ts",
        "export declare function external(): void;\n",
    );
    write(
        &root,
        "node_modules/synthetic-dependency/index.js",
        "throw new Error('SYNTHETIC_RUNTIME_MUST_NOT_EXECUTE');\n",
    );
    let state = directory.path().join("state");
    let error = producer::typescript(&root, &state, &Options::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("offline TypeScript check failed"), "{error}");
    let enabled = Options { dependencies: true };
    let stats = producer::typescript(&root, &state, &enabled).unwrap();
    assert_eq!(stats.documents, 1, "dependency definitions remain external");
    let engine = Engine::open_state_dir(&root, &state).unwrap();
    let result = engine.execute(&callees()).unwrap();
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert_eq!(result["items"][0]["callee"], "helper");
    let refs = engine
        .execute(&Query {
            action: "refs".into(),
            kind: Some("call".into()),
            evidence: Some("combined".into()),
            ..Default::default()
        })
        .unwrap();
    let external = refs["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "external")
        .unwrap();
    assert!(external["resolved"].is_null());
    let generation = engine.store().generation().unwrap();
    assert!(producer::typescript(&root, &state, &Options::default()).is_err());
    assert_eq!(engine.store().generation().unwrap(), generation);
    assert!(engine.execute(&callees()).is_ok());
    write(
        &root,
        "node_modules/synthetic-dependency/index.d.ts",
        "export declare function external(): number;\n",
    );
    assert!(engine.execute(&callees()).is_err());
    producer::typescript(&root, &state, &enabled).unwrap();
    assert!(engine.execute(&callees()).is_ok());
    write(
        &root,
        "node_modules/synthetic-dependency/added.d.ts",
        "export interface Added {}\n",
    );
    assert!(
        engine.execute(&callees()).is_err(),
        "new dependency inputs invalidate the snapshot"
    );
    assert!(fs::read_dir(&state).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("semantic-typescript-")
    }));
}
