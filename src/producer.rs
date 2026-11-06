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

mod inputs;
mod typescript;
pub(crate) use inputs::Inventory;
pub use inputs::{Options, Profile};
pub use typescript::typescript;

pub(crate) fn private_directory(path: &Path) -> Result<()> {
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

pub(crate) fn read(path: &Path, limit: u64) -> Result<Vec<u8>> {
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

fn configured(name: &str) -> Result<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .with_context(|| format!("missing {name}; use the packaged semantic runner"))
}

/// A private stable snapshot and its disposable output, shared by the two
/// explicit producers. The snapshot never grants access to the original root.
struct Snapshot {
    root: PathBuf,
    state: PathBuf,
    inventory: Inventory,
    files: BTreeMap<String, Vec<u8>>,
    _temporary: tempfile::TempDir,
    project: PathBuf,
    output: PathBuf,
}

impl Snapshot {
    fn new(root: &Path, state: &Path, profile: Profile, options: &Options) -> Result<Self> {
        let root = root.canonicalize()?;
        private_directory(state)?;
        let state = state.canonicalize()?;
        ensure!(
            state != root && !root.starts_with(&state),
            "runtime state must not contain the project"
        );
        let inventory = Inventory::new(profile, options);
        let files = inventory.capture(&root, &state)?;
        let temporary = tempfile::Builder::new()
            .prefix(&format!("semantic-{}-", profile.name()))
            .tempdir_in(&state)?;
        let project = temporary.path().join("project");
        let output = temporary.path().join("output");
        private_directory(&project)?;
        private_directory(&output)?;
        for (file, bytes) in &files {
            let path = project.join(file);
            if let Some(parent) = path.parent() {
                fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(parent)?;
            }
            write(&path, bytes)?;
        }
        if options.dependencies && matches!(profile, Profile::Rust) {
            let configuration = project.join(".cargo");
            private_directory(&configuration)?;
            // Fixed runner-owned configuration; project Cargo configuration is
            // still rejected. Nothing is written to the original checkout.
            write(&configuration.join("config.toml"), b"[source.crates-io]\nreplace-with = 'ast-index-vendor'\n[source.ast-index-vendor]\ndirectory = '/project/vendor'\n")?;
        }
        Ok(Self {
            root,
            state,
            inventory,
            files,
            _temporary: temporary,
            project,
            output,
        })
    }

    fn command(&self, closure: &Path, environment: &[(&str, String)]) -> Result<Command> {
        let sandbox = configured("AST_INDEX_SEMANTIC_BWRAP")?;
        let timeout = configured("AST_INDEX_SEMANTIC_TIMEOUT")?;
        let paths = String::from_utf8(read(closure, 1024 * 1024)?)?;
        let mut command = Command::new(timeout);
        command
            .args(["--signal=TERM", "--kill-after=5s", "120s"])
            .arg(sandbox);
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
        command
            .arg("--ro-bind")
            .arg(&self.project)
            .arg("/project")
            .arg("--bind")
            .arg(&self.output)
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
                "PATH",
                "/nonexistent",
            ]);
        for (name, value) in environment {
            command.args(["--setenv", name, value]);
        }
        command.args(["--chdir", "/project", "--"]);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Ok(command)
    }

    fn import(self) -> Result<crate::scip::ImportStats> {
        let outputs: Vec<_> = fs::read_dir(&self.output)?.collect::<std::io::Result<_>>()?;
        ensure!(
            outputs.len() == 1 && outputs[0].file_name() == "index.scip",
            "producer wrote unexpected output; artifact discarded"
        );
        if self.inventory.capture(&self.root, &self.state)? != self.files {
            bail!("project inputs changed during semantic generation; artifact discarded");
        }
        let artifact = self.output.join("index.scip");
        let bytes = read(&artifact, 4 * 1024 * 1024)?;
        let manifest = self.output.join("manifest.json");
        let hashes: BTreeMap<_, _> = self
            .files
            .iter()
            .map(|(path, bytes)| (path, blake3::hash(bytes).to_hex().to_string()))
            .collect();
        write(
            &manifest,
            &serde_json::to_vec(&serde_json::json!({
                "artifact_blake3":blake3::hash(&bytes).to_hex().to_string(), "files":hashes, "inventory":self.inventory
            }))?,
        )?;
        let engine = crate::Engine::open_state_dir(&self.root, &self.state)?;
        engine.index(&crate::IndexOptions {
            exclude: self.inventory.options.exclude.clone(),
            exclude_prefix: self.inventory.options.exclude_prefix.clone(),
            ..Default::default()
        })?;
        Ok(crate::scip::import(
            &self.root,
            engine.store(),
            &artifact,
            Some(&manifest),
        )?)
    }
}

/// Run a fresh snapshot; producer stdout/stderr is deliberately not retained.
/// Missing offline dependencies fail explicitly. There is no network fallback.
pub fn rust(root: &Path, state: &Path, options: &Options) -> Result<crate::scip::ImportStats> {
    let analyzer = configured("AST_INDEX_SEMANTIC_RUST_ANALYZER")?;
    let cargo = configured("AST_INDEX_SEMANTIC_CARGO")?;
    let rustc = configured("AST_INDEX_SEMANTIC_RUSTC")?;
    let closure = configured("AST_INDEX_SEMANTIC_CLOSURE")?;
    let snapshot = Snapshot::new(root, state, Profile::Rust, options)?;
    let environment = [
        ("CARGO_HOME", "/tmp/cargo".to_owned()),
        ("CARGO_NET_OFFLINE", "true".to_owned()),
        ("CARGO_TARGET_DIR", "/tmp/target".to_owned()),
        (
            "PATH",
            format!(
                "{}:{}",
                cargo.parent().context("cargo directory")?.display(),
                rustc.parent().context("rustc directory")?.display()
            ),
        ),
    ];
    let metadata = snapshot
        .command(&closure, &environment)?
        .arg(&cargo)
        .args(["metadata", "--offline", "--locked", "--format-version", "1"])
        .status()?;
    ensure!(
        metadata.success(),
        "offline Cargo metadata failed; dependencies/configuration unavailable (no project diagnostics retained)"
    );
    let produced = snapshot
        .command(&closure, &environment)?
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
    snapshot.import()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typescript_inputs_include_config_and_locks_but_exclude_runtime_and_scripts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for file in [
            "tsconfig.json",
            "tsconfig.base.json",
            "package.json",
            "pnpm-lock.yaml",
            "source.ts",
            "private.json",
            "script.js",
        ] {
            fs::write(root.join(file), "{}").unwrap();
        }
        let files = Inventory::new(Profile::TypeScript, &Options::default())
            .capture(root, &root.join("state"))
            .unwrap();
        assert_eq!(files.len(), 5);
        assert!(!files.contains_key("private.json") && !files.contains_key("script.js"));
        std::os::unix::fs::symlink("private.json", root.join("linked.ts")).unwrap();
        assert!(
            Inventory::new(Profile::TypeScript, &Options::default())
                .capture(root, &root.join("state"))
                .is_err()
        );
        fs::remove_file(root.join("linked.ts")).unwrap();
        fs::write(root.join("line\nbreak.ts"), "{}").unwrap();
        assert!(
            Inventory::new(Profile::TypeScript, &Options::default())
                .capture(root, &root.join("state"))
                .is_err()
        );
    }
    #[test]
    fn snapshot_rejects_config_symlinks_and_public_state() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("Cargo.toml"), "[workspace]").unwrap();
        fs::write(root.join("Cargo.lock"), "version = 4").unwrap();
        fs::write(root.join("private.json"), "PRIVATE_CANARY").unwrap();
        fs::write(root.join("lib.rs"), "fn f() {}").unwrap();
        let files = Inventory::new(Profile::Rust, &Options::default())
            .capture(root, &root.join("state"))
            .unwrap();
        assert_eq!(files.len(), 3);
        assert!(!files.contains_key("private.json"));
        std::os::unix::fs::symlink("private.json", root.join("linked.rs")).unwrap();
        assert!(
            Inventory::new(Profile::Rust, &Options::default())
                .capture(root, &root.join("state"))
                .is_err()
        );
        fs::remove_file(root.join("linked.rs")).unwrap();
        fs::create_dir(root.join(".cargo")).unwrap();
        fs::write(root.join(".cargo/config.toml"), "[build]").unwrap();
        assert!(
            Inventory::new(Profile::Rust, &Options::default())
                .capture(root, &root.join("state"))
                .is_err()
        );
        let state = root.join("state");
        private_directory(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_directory(&state).is_err());
    }
}
