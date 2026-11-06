use ast_index::{
    Engine,
    producer::Profile,
    profiles::{self, Outcome, Selection},
    query::Query,
};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

fn write(root: &Path, file: &str, value: &str) {
    let path = root.join(file);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, value).unwrap();
}

fn selection(project: &str, profile: Profile) -> Selection {
    Selection {
        project: project.into(),
        profile,
        dependencies: false,
    }
}

fn request(project: &str, profile: Profile) -> Query {
    Query {
        action: "callees".into(),
        query: Some("caller".into()),
        project: Some(project.into()),
        profile: Some(profile),
        language: Some(profile.name().into()),
        file: Some(
            if profile == Profile::Rust {
                "src/lib.rs"
            } else {
                "main.ts"
            }
            .into(),
        ),
        evidence: Some("combined".into()),
        ..Default::default()
    }
}

#[test]
fn managed_refresh_separates_projects_profiles_and_publishes_only_successful_snapshots() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let base = temporary.path().join("private-state");
    let first = workspace.join("first");
    write(
        &first,
        "Cargo.toml",
        "[package]\nname=\"synthetic-managed\"\nversion=\"0.0.0\"\nedition=\"2021\"\n[workspace]\n",
    );
    write(
        &first,
        "Cargo.lock",
        "version=4\n[[package]]\nname=\"synthetic-managed\"\nversion=\"0.0.0\"\n",
    );
    write(
        &first,
        "src/lib.rs",
        "#![no_std]\npub fn rust_target() {}\npub fn caller() {rust_target();}\n",
    );
    for (project, target) in [("first", "first_target"), ("second", "second_target")] {
        write(
            &workspace.join(project),
            "tsconfig.json",
            r#"{"compilerOptions":{"strict":true},"include":["main.ts"]}"#,
        );
        write(
            &workspace.join(project),
            "main.ts",
            &format!("export function {target}() {{}}\nexport function caller() {{{target}();}}\n"),
        );
    }
    for (project, profile) in [
        ("first", Profile::Rust),
        ("first", Profile::TypeScript),
        ("second", Profile::TypeScript),
    ] {
        assert_eq!(
            profiles::refresh(&workspace, &base, &selection(project, profile), &[]).unwrap(),
            Outcome::Updated
        );
    }
    let engine = Engine::open(&workspace, None)
        .unwrap()
        .with_semantic_state_dir(base.clone(), Vec::new());
    for (project, profile, target) in [
        ("first", Profile::Rust, "rust_target"),
        ("first", Profile::TypeScript, "first_target"),
        ("second", Profile::TypeScript, "second_target"),
    ] {
        let query = request(project, profile);
        let before = engine.execute(&query).unwrap();
        assert_eq!(before["items"][0]["callee"], target);
        assert_eq!(
            profiles::refresh(&workspace, &base, &selection(project, profile), &[]).unwrap(),
            Outcome::Current
        );
        assert_eq!(engine.execute(&query).unwrap(), before);
    }
    // The CLI must skip unchanged profiles even without any producer executable.
    let plan = temporary.path().join("projects.json");
    fs::write(
        &plan,
        r#"{"version":1,"projects":[{"project":"first","profile":"typescript"}]}"#,
    )
    .unwrap();
    fs::set_permissions(&plan, fs::Permissions::from_mode(0o600)).unwrap();
    let cli = Command::new(env!("CARGO_BIN_EXE_ast-index-semantic"))
        .args(["refresh", "--root"])
        .arg(&workspace)
        .arg("--state-dir")
        .arg(&base)
        .arg("--projects-file")
        .arg(&plan)
        .env_remove("AST_INDEX_SEMANTIC_TYPESCRIPT")
        .env_remove("AST_INDEX_SEMANTIC_TYPESCRIPT_CLOSURE")
        .output()
        .unwrap();
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&cli.stdout).unwrap()[0]["status"],
        "current"
    );

    let query = request("first", Profile::TypeScript);
    let before = engine.execute(&query).unwrap();
    let mut unavailable = selection("first", Profile::TypeScript);
    unavailable.dependencies = true;
    assert!(profiles::refresh(&workspace, &base, &unavailable, &[]).is_err());
    assert_eq!(
        engine.execute(&query).unwrap(),
        before,
        "failed settings changes leave the published snapshot intact"
    );
    write(
        &first,
        "main.ts",
        "export function replacement() {}\nexport function caller() {replacement();}\n",
    );
    assert!(
        engine.execute(&query).is_err(),
        "old evidence cannot answer changed source"
    );
    assert_eq!(
        profiles::refresh(
            &workspace,
            &base,
            &selection("first", Profile::TypeScript),
            &[]
        )
        .unwrap(),
        Outcome::Updated
    );
    let after = engine.execute(&query).unwrap();
    assert_eq!(after["items"][0]["callee"], "replacement");
    assert!(after["generation"].as_i64().unwrap() > before["generation"].as_i64().unwrap());
    assert_eq!(
        engine
            .execute(&request("second", Profile::TypeScript))
            .unwrap()["items"][0]["callee"],
        "second_target"
    );
    for entry in walkdir::WalkDir::new(&base) {
        let entry = entry.unwrap();
        assert!(
            !entry
                .file_name()
                .to_string_lossy()
                .starts_with("candidate-")
        );
        assert_eq!(entry.metadata().unwrap().permissions().mode() & 0o077, 0);
        assert!(!entry.file_name().to_string_lossy().ends_with("-wal"));
    }
}

#[test]
fn managed_selection_rejects_unapproved_paths_and_queries_never_create_state() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let base = temporary.path().join("missing-state");
    let engine = Engine::open(&workspace, None)
        .unwrap()
        .with_semantic_state_dir(base.clone(), Vec::new());
    for project in [".", "../outside", "/absolute", "missing", "", "line\nbreak"] {
        assert!(engine.execute(&request(project, Profile::Rust)).is_err());
    }
    assert!(
        !base.exists(),
        "read-only selection must not create a database or run a producer"
    );
    let outside = temporary.path().join("outside");
    fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, workspace.join("linked")).unwrap();
    assert!(
        profiles::refresh(&workspace, &base, &selection("linked", Profile::Rust), &[]).is_err()
    );
    assert!(!base.exists());
    let plan = temporary.path().join("projects.json");
    for contents in [
        r#"{"version":1,"projects":[{"project":".","profile":"rust"},{"project":"../outside","profile":"rust"}]}"#,
        r#"{"version":1,"projects":[{"project":".","profile":"rust"},{"project":"./","profile":"rust"}]}"#,
        r#"{"version":2,"projects":[]}"#,
        r#"{"version":1,"projects":[{"project":".","profile":"python"}]}"#,
        r#"{"version":1,"projects":[{"project":".","profile":"rust","command":"forbidden"}]}"#,
    ] {
        fs::write(&plan, contents).unwrap();
        fs::set_permissions(&plan, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(profiles::refresh_plan(&workspace, &base, &plan, &[]).is_err());
        assert!(
            !base.exists(),
            "the whole plan must be validated before any refresh"
        );
    }
    fs::write(&plan, r#"{"version":1,"projects":[]}"#).unwrap();
    fs::set_permissions(&plan, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(profiles::refresh_plan(&workspace, &base, &plan, &[]).is_err());
    fs::set_permissions(&plan, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        profiles::refresh_plan(&workspace, &base, &plan, &[])
            .unwrap()
            .is_empty()
    );
    let too_many = serde_json::json!({"version":1,"projects":(0..9).map(|_|serde_json::json!({"project":".","profile":"rust"})).collect::<Vec<_>>()});
    fs::write(&plan, serde_json::to_vec(&too_many).unwrap()).unwrap();
    assert!(
        profiles::refresh_plan(&workspace, &base, &plan, &[])
            .unwrap_err()
            .to_string()
            .contains("at most 8")
    );
    fs::write(
        &plan,
        r#"{"version":1,"projects":[{"project":".","profile":"rust"}]}"#,
    )
    .unwrap();
    let overlapping = workspace.join("private-state");
    assert!(profiles::refresh_plan(&workspace, &overlapping, &plan, &[]).is_err());
    assert!(!overlapping.exists());
}

#[test]
fn workspace_exclusions_apply_to_snapshot_ast_and_query_policy_before_refresh() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let root = workspace.join("example");
    let base = temporary.path().join("state");
    write(
        &root,
        "tsconfig.json",
        r#"{"compilerOptions":{"strict":true},"include":["**/*.ts"]}"#,
    );
    write(
        &root,
        "main.ts",
        "export function target() {}\nexport function caller() {target();}\n",
    );
    write(
        &root,
        "private/private.ts",
        "export function SYNTHETIC_PRIVATE_TARGET() {}\nconst wrong: number = 'excluded input';\n",
    );
    write(
        &root,
        "private/private.py",
        "def SYNTHETIC_PRIVATE_TARGET(): pass\n",
    );
    write(
        &root,
        "nested/secrets/private.ts",
        "const wrong: number = 'excluded input';\n",
    );
    let exclude = vec![
        "/example/private/".into(),
        "secrets/".into(),
        "/blocked/".into(),
    ];
    let selected = selection("example", Profile::TypeScript);
    assert_eq!(
        profiles::refresh(&workspace, &base, &selected, &exclude).unwrap(),
        Outcome::Updated
    );
    let engine = Engine::open(&workspace, None)
        .unwrap()
        .with_semantic_state_dir(base.clone(), exclude.clone());
    assert_eq!(
        engine
            .execute(&request("example", Profile::TypeScript))
            .unwrap()["items"][0]["callee"],
        "target"
    );
    let query = Query {
        action: "search".into(),
        query: Some("SYNTHETIC_PRIVATE_TARGET".into()),
        project: Some("example".into()),
        profile: Some(Profile::TypeScript),
        ..Default::default()
    };
    assert!(
        engine.execute(&query).unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty(),
        "excluded files cannot enter the AST half either"
    );
    let changed = Engine::open(&workspace, None)
        .unwrap()
        .with_semantic_state_dir(base.clone(), Vec::new());
    assert!(
        changed
            .execute(&query)
            .unwrap_err()
            .to_string()
            .contains("exclusion policy changed")
    );
    assert!(
        profiles::refresh(&workspace, &base, &selected, &[]).is_err(),
        "the excluded TypeScript type errors expose a missing filter"
    );
    assert!(
        engine
            .execute(&request("example", Profile::TypeScript))
            .is_ok(),
        "failed policy refresh does not replace published state"
    );
    write(&workspace.join("blocked/deep"), "tsconfig.json", "{}");
    let missing = temporary.path().join("must-not-exist");
    assert!(
        profiles::refresh(
            &workspace,
            &missing,
            &selection("blocked/deep", Profile::TypeScript),
            &exclude
        )
        .is_err()
    );
    assert!(!missing.exists());
    let plan = temporary.path().join("projects.json");
    fs::write(&plan, r#"{"version":1,"projects":[{"project":"example","profile":"typescript"},{"project":"blocked/deep","profile":"typescript"}]}"#).unwrap();
    fs::set_permissions(&plan, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(profiles::refresh_plan(&workspace, &missing, &plan, &exclude).is_err());
    assert!(
        !missing.exists(),
        "an excluded later selection rejects the whole batch before work"
    );
}
