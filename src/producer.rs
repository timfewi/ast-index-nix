//! Explicit, bounded offline producer. The core index/query paths never call it.
//! Only source/configuration snapshots enter the sandbox, never the user's home.
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const MAX_FILES: usize = 2000;
const MAX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_FILE: u64 = 2 * 1024 * 1024;

fn private_directory(path: &Path) -> Result<()> {
    // Do not chmod an existing directory or follow an output symlink.
    for ancestor in path.ancestors() {
        if let Ok(meta) = fs::symlink_metadata(ancestor) {
            ensure!(
                !meta.file_type().is_symlink(),
                "runtime path contains a symbolic link"
            );
        }
    }
    if !path.exists() {
        fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir() && meta.permissions().mode() & 0o077 == 0,
        "runtime directory must be private (no group/other access)"
    );
    Ok(())
}

fn read(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "snapshot input must be a regular file"
    );
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "snapshot size budget exceeded");
    Ok(bytes)
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

/// Inputs are bounded and selected by language/configuration, not by arbitrary
/// files in the project. A second full inventory detects additions/deletions.
fn inputs(root: &Path, excluded: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    ensure!(
        root.join("Cargo.toml").is_file(),
        "Rust profile needs Cargo.toml"
    );
    let mut files = BTreeMap::new();
    let mut total = 0;
    let started = std::time::Instant::now();
    let walk = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_global(false)
        .follow_links(false)
        .filter_entry({
            let excluded = excluded.to_path_buf();
            move |e| {
                e.path() != excluded
                    && !matches!(
                        e.file_name().to_str(),
                        Some("target" | "node_modules" | ".git" | ".ast-index")
                    )
            }
        })
        .build();
    for (visited, entry) in walk.enumerate() {
        ensure!(
            visited < 50_000 && started.elapsed().as_secs() < 30,
            "snapshot traversal budget exceeded"
        );
        let entry = entry?;
        if entry.file_type().is_some_and(|t| t.is_dir()) {
            // Cargo configuration can replace compiler executables. This first
            // profile rejects it rather than silently ignoring its semantics.
            ensure!(
                !entry.path().join(".cargo/config").exists()
                    && !entry.path().join(".cargo/config.toml").exists(),
                "custom Cargo configuration is not supported by the offline Rust profile"
            );
            continue;
        }
        let path = entry.path();
        let selected = path.extension().is_some_and(|e| e == "rs")
            || matches!(
                path.file_name().and_then(|n| n.to_str()),
                Some("Cargo.toml" | "Cargo.lock")
            );
        if !selected {
            continue;
        }
        ensure!(files.len() < MAX_FILES, "snapshot file budget exceeded");
        let relative = path
            .strip_prefix(root)?
            .to_str()
            .context("non UTF-8 source path")?;
        ensure!(relative.len() <= 4096, "snapshot path budget exceeded");
        ensure!(
            path.canonicalize()? == path,
            "snapshot input path contains a symbolic link"
        );
        let bytes = crate::index::read_source(root, path, MAX_FILE.min(MAX_BYTES - total))?;
        total += bytes.len() as u64;
        files.insert(relative.to_owned(), bytes);
    }
    ensure!(
        files.contains_key("Cargo.toml") && files.contains_key("Cargo.lock"),
        "offline Rust profile requires an included Cargo.toml and existing Cargo.lock"
    );
    Ok(files)
}

fn configured(name: &str) -> Result<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .with_context(|| format!("missing {name}; use the packaged semantic runner"))
}

/// Run a fresh snapshot; producer stdout/stderr is deliberately not retained.
/// Missing offline dependencies fail explicitly. There is no network fallback.
pub fn rust(root: &Path, state: &Path) -> Result<crate::scip::ImportStats> {
    let sandbox = configured("AST_INDEX_SEMANTIC_BWRAP")?;
    let timeout = configured("AST_INDEX_SEMANTIC_TIMEOUT")?;
    let analyzer = configured("AST_INDEX_SEMANTIC_RUST_ANALYZER")?;
    let cargo = configured("AST_INDEX_SEMANTIC_CARGO")?;
    let rustc = configured("AST_INDEX_SEMANTIC_RUSTC")?;
    let closure = configured("AST_INDEX_SEMANTIC_CLOSURE")?;
    let root = root.canonicalize()?;
    private_directory(state)?;
    let state = state.canonicalize()?;
    ensure!(
        state != root && !root.starts_with(&state),
        "runtime state must not contain the project"
    );
    let files = inputs(&root, &state)?;
    let temporary = tempfile::Builder::new()
        .prefix("semantic-rust-")
        .tempdir_in(&state)?;
    let snapshot = temporary.path().join("project");
    let output = temporary.path().join("output");
    private_directory(&snapshot)?;
    private_directory(&output)?;
    for (file, bytes) in &files {
        let path = snapshot.join(file);
        if let Some(parent) = path.parent() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
        write(&path, bytes)?;
    }
    let paths = String::from_utf8(read(&closure, 1024 * 1024)?)?;
    let sandbox_command = || -> Result<Command> {
        let mut command = Command::new(&timeout);
        command
            .args(["--signal=TERM", "--kill-after=5s", "120s"])
            .arg(&sandbox);
        command.args([
            "--unshare-all",
            "--die-with-parent",
            "--new-session",
            "--clearenv",
        ]);
        for path in paths.lines() {
            ensure!(
                path.starts_with("/nix/store/") && Path::new(path).components().count() == 4,
                "invalid packaged tool closure path"
            );
            command.args(["--ro-bind", path, path]);
        }
        let path = format!(
            "{}:{}",
            cargo.parent().context("cargo directory")?.display(),
            rustc.parent().context("rustc directory")?.display()
        );
        command
            .args(["--ro-bind"])
            .arg(&snapshot)
            .arg("/project")
            .arg("--bind")
            .arg(&output)
            .arg("/output")
            .args([
                "--proc",
                "/proc",
                "--dev",
                "/dev",
                "--size",
                "536870912",
                "--tmpfs",
                "/tmp",
                "--dir",
                "/tmp/home",
                "--setenv",
                "HOME",
                "/tmp/home",
                "--setenv",
                "CARGO_HOME",
                "/tmp/cargo",
                "--setenv",
                "CARGO_NET_OFFLINE",
                "true",
                "--setenv",
                "CARGO_TARGET_DIR",
                "/tmp/target",
                "--setenv",
                "PATH",
                &path,
                "--chdir",
                "/project",
                "--",
            ]);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Ok(command)
    };
    let metadata = sandbox_command()?
        .arg(&cargo)
        .args(["metadata", "--offline", "--locked", "--format-version", "1"])
        .status()?;
    ensure!(
        metadata.success(),
        "offline Cargo metadata failed; dependencies/configuration unavailable (no project diagnostics retained)"
    );
    let produced = sandbox_command()?
        .arg(&analyzer)
        .args([
            "scip",
            "/project",
            "--output",
            "/output/index.scip",
            "--exclude-vendored-libraries",
            "--num-threads",
            "2",
        ])
        .status()?;
    ensure!(
        produced.success(),
        "sandboxed Rust producer failed or timed out (no project diagnostics retained)"
    );
    let outputs: Vec<_> = fs::read_dir(&output)?.collect::<std::io::Result<_>>()?;
    ensure!(
        outputs.len() == 1 && outputs[0].file_name() == "index.scip",
        "producer wrote unexpected output; artifact discarded"
    );
    if inputs(&root, &state)? != files {
        bail!("project inputs changed during semantic generation; artifact discarded");
    }
    let artifact = output.join("index.scip");
    let bytes = read(&artifact, 4 * 1024 * 1024)?;
    let manifest = output.join("manifest.json");
    let hashes: BTreeMap<_, _> = files
        .iter()
        .map(|(path, bytes)| (path, blake3::hash(bytes).to_hex().to_string()))
        .collect();
    write(
        &manifest,
        &serde_json::to_vec(&serde_json::json!({
            "artifact_blake3":blake3::hash(&bytes).to_hex().to_string(), "files":hashes
        }))?,
    )?;
    let engine = crate::Engine::open_state_dir(&root, &state)?;
    engine.index(&crate::IndexOptions::default())?;
    Ok(crate::scip::import(
        &root,
        engine.store(),
        &artifact,
        Some(&manifest),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_rejects_config_symlinks_and_public_state() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("Cargo.toml"), "[workspace]").unwrap();
        fs::write(root.join("Cargo.lock"), "version = 4").unwrap();
        fs::write(root.join("private.json"), "PRIVATE_CANARY").unwrap();
        fs::write(root.join("lib.rs"), "fn f() {}").unwrap();
        let files = inputs(root, &root.join("state")).unwrap();
        assert_eq!(files.len(), 3);
        assert!(!files.contains_key("private.json"));
        std::os::unix::fs::symlink("private.json", root.join("linked.rs")).unwrap();
        assert!(inputs(root, &root.join("state")).is_err());
        fs::remove_file(root.join("linked.rs")).unwrap();
        fs::create_dir(root.join(".cargo")).unwrap();
        fs::write(root.join(".cargo/config.toml"), "[build]").unwrap();
        assert!(inputs(root, &root.join("state")).is_err());
        let state = root.join("state");
        private_directory(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_directory(&state).is_err());
    }
}
