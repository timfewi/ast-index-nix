//! The same bounded input policy governs snapshots and query-time freshness.
//! Dependency access is an explicit runner option, never a project setting.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path, time::Instant};

// Project sources keep the document budget. An explicitly selected dependency
// tree has its own share: real vendored registries exceed the project bound.
const PROJECT_FILES: usize = 2000;
const PROJECT_BYTES: u64 = 128 * 1024 * 1024;
const DEPENDENCY_FILES: usize = 14_000;
const DEPENDENCY_BYTES: u64 = 384 * 1024 * 1024;
const PROJECT_FILE: u64 = 2 * 1024 * 1024;
// Published crates ship generated tables and bindings above the project bound.
const DEPENDENCY_FILE: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Options {
    /// Include the existing project-local vendor/ or node_modules/ tree.
    /// No installation, global cache discovery or symlink traversal occurs.
    pub dependencies: bool,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub exclude_prefix: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    Rust,
    TypeScript,
}

impl Profile {
    pub fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
        }
    }
    fn manifest(self) -> &'static str {
        match self {
            Self::Rust => "Cargo.toml",
            Self::TypeScript => "tsconfig.json",
        }
    }
    fn dependency_directory(self) -> &'static str {
        match self {
            Self::Rust => "vendor",
            Self::TypeScript => "node_modules",
        }
    }
    fn selects(self, path: &Path, dependency: bool) -> bool {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        match self {
            Self::Rust => {
                path.extension().is_some_and(|e| e == "rs")
                    || matches!(name, "Cargo.toml" | "Cargo.lock")
                    || (dependency && name == ".cargo-checksum.json")
            }
            Self::TypeScript => {
                crate::lang::detect(path).is_some_and(|s| matches!(s.id, "typescript" | "tsx"))
                    || (name.starts_with("tsconfig") && name.ends_with(".json"))
                    || matches!(
                        name,
                        "package.json"
                            | "package-lock.json"
                            | "npm-shrinkwrap.json"
                            | "pnpm-lock.yaml"
                            | "yarn.lock"
                            | "bun.lock"
                    )
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Inventory {
    version: u8,
    pub(super) profile: Profile,
    pub(super) options: Options,
}

impl Inventory {
    pub(crate) fn new(profile: Profile, options: &Options) -> Self {
        Self {
            version: 1,
            profile,
            options: options.clone(),
        }
    }

    pub(crate) fn matches_scope(&self, profile: Profile, prefix: &str, exclude: &[String]) -> bool {
        self.version == 1
            && self.profile == profile
            && self.options.exclude_prefix == prefix
            && self.options.exclude == exclude
    }

    /// Upper bound of files this inventory may select.
    pub(crate) fn file_budget(&self) -> usize {
        PROJECT_FILES
            + if self.options.dependencies {
                DEPENDENCY_FILES
            } else {
                0
            }
    }

    pub(super) fn is_dependency(&self, file: &str) -> bool {
        Path::new(file).starts_with(self.profile.dependency_directory())
    }

    pub(crate) fn hashes(&self, root: &Path, excluded: &Path) -> Result<BTreeMap<String, String>> {
        Ok(self
            .capture(root, excluded)?
            .into_iter()
            .map(|(file, bytes)| (file, blake3::hash(&bytes).to_hex().to_string()))
            .collect())
    }

    pub(super) fn capture(
        &self,
        root: &Path,
        excluded: &Path,
    ) -> Result<BTreeMap<String, Vec<u8>>> {
        ensure!(
            self.version == 1,
            "unsupported semantic input inventory version"
        );
        ensure!(
            self.options.exclude.len() <= 64
                && self.options.exclude.iter().map(String::len).sum::<usize>() <= 16 * 1024,
            "semantic exclusion policy exceeds its bounds"
        );
        ensure!(
            !root.starts_with(excluded),
            "runtime state must not contain the project"
        );
        ensure!(
            root.join(self.profile.manifest()).is_file(),
            "{} profile needs {}",
            self.profile.name(),
            self.profile.manifest()
        );
        let dependency_root = root.join(self.profile.dependency_directory());
        let mut roots = vec![(root, false)];
        if self.options.dependencies {
            let metadata = std::fs::symlink_metadata(&dependency_root)
                .context("explicit dependency directory is unavailable")?;
            ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "dependencies must be a regular project-local directory"
            );
            ensure!(
                !dependency_root.starts_with(excluded) && !excluded.starts_with(&dependency_root),
                "dependency tree and runtime state must be separate"
            );
            roots.push((&dependency_root, true));
        }
        let started = Instant::now();
        let mut visited = 0;
        let mut files = BTreeMap::new();
        for (walk_root, dependency) in roots {
            let (max_files, max_bytes, max_file) = if dependency {
                (DEPENDENCY_FILES, DEPENDENCY_BYTES, DEPENDENCY_FILE)
            } else {
                (PROJECT_FILES, PROJECT_BYTES, PROJECT_FILE)
            };
            let mut count = 0;
            let mut total = 0;
            let exclusions = crate::index::exclusions(
                root,
                &self.options.exclude,
                &self.options.exclude_prefix,
            )?;
            // Explicit walk roots bypass the walker's ignore matching. Apply
            // the workspace policy before entering a separate dependency tree.
            if dependency && exclusions.matched(walk_root, true).is_ignore() {
                continue;
            }
            // Explicit dependency input is commonly gitignored. The option
            // selects only the named directory, with the same shared budgets.
            let walk = ignore::WalkBuilder::new(walk_root)
                .overrides(exclusions)
                .hidden(!dependency)
                .git_ignore(!dependency)
                .git_exclude(!dependency)
                .ignore(!dependency)
                .git_global(false)
                .follow_links(false)
                .filter_entry({
                    let excluded = excluded.to_owned();
                    let dependency_root = dependency_root.clone();
                    move |e| {
                        e.path() != excluded
                            && (dependency || e.path() != dependency_root)
                            && !matches!(
                                e.file_name().to_str(),
                                Some("target" | ".git" | ".ast-index")
                            )
                            && (dependency || e.file_name() != "node_modules")
                    }
                })
                .build();
            for entry in walk {
                ensure!(
                    visited < 50_000 && started.elapsed().as_secs() < 30,
                    "snapshot traversal budget exceeded"
                );
                visited += 1;
                let entry = entry?;
                ensure!(
                    !dependency || !entry.file_type().is_some_and(|t| t.is_symlink()),
                    "dependency inputs cannot contain symbolic links"
                );
                if entry.file_type().is_some_and(|t| t.is_dir()) {
                    ensure!(
                        !matches!(self.profile, Profile::Rust)
                            || (!entry.path().join(".cargo/config").exists()
                                && !entry.path().join(".cargo/config.toml").exists()),
                        "custom Cargo configuration is not supported by the offline Rust profile"
                    );
                    continue;
                }
                let path = entry.path();
                if !self.profile.selects(path, dependency) {
                    continue;
                }
                ensure!(count < max_files, "snapshot file budget exceeded");
                count += 1;
                let relative = path
                    .strip_prefix(root)?
                    .to_str()
                    .context("non UTF-8 source path")?;
                ensure!(relative.len() <= 4096, "snapshot path budget exceeded");
                ensure!(
                    !matches!(self.profile, Profile::TypeScript)
                        || !relative.chars().any(char::is_control),
                    "TypeScript snapshot paths must not contain control characters"
                );
                ensure!(
                    path.canonicalize()? == path,
                    "snapshot input path contains a symbolic link"
                );
                let bytes = crate::index::read_source(root, path, max_file.min(max_bytes - total))?;
                total += bytes.len() as u64;
                files.insert(relative.to_owned(), bytes);
            }
        }
        ensure!(
            files.contains_key(self.profile.manifest())
                && (!matches!(self.profile, Profile::Rust) || files.contains_key("Cargo.lock")),
            "offline profile requires its included root manifest and Rust Cargo.lock"
        );
        Ok(files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn dependency_selection_is_explicit_bounded_and_cannot_follow_links() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::write(root.join("tsconfig.json"), "{}").unwrap();
        fs::write(root.join("main.ts"), "export {};").unwrap();
        fs::create_dir_all(root.join("node_modules/package")).unwrap();
        fs::write(root.join("node_modules/package/index.d.ts"), "export {};").unwrap();
        fs::write(root.join("node_modules/package/package.json"), "{}").unwrap();
        fs::write(
            root.join("node_modules/package/private.json"),
            "SYNTHETIC_PRIVATE_CANARY",
        )
        .unwrap();
        fs::write(root.join("node_modules/package/index.js"), "throw 1;").unwrap();
        let disabled = Inventory::new(Profile::TypeScript, &Options::default());
        let enabled = Inventory::new(
            Profile::TypeScript,
            &Options {
                dependencies: true,
                ..Default::default()
            },
        );
        let excluded = root.join("state");
        assert_eq!(disabled.capture(root, &excluded).unwrap().len(), 2);
        let files = enabled.capture(root, &excluded).unwrap();
        assert_eq!(files.len(), 4);
        assert!(files.contains_key("node_modules/package/index.d.ts"));
        assert!(!files.contains_key("node_modules/package/private.json"));
        assert!(!files.contains_key("node_modules/package/index.js"));
        std::os::unix::fs::symlink("index.d.ts", root.join("node_modules/package/link.d.ts"))
            .unwrap();
        assert!(
            enabled
                .capture(root, &excluded)
                .unwrap_err()
                .to_string()
                .contains("symbolic links")
        );
        assert_eq!(disabled.capture(root, &excluded).unwrap().len(), 2);
        assert!(
            enabled
                .capture(root, &root.join("node_modules/state"))
                .is_err()
        );
        assert!(serde_json::from_str::<Inventory>(r#"{"version":1,"profile":"typescript","options":{"dependencies":true},"directory":"/outside"}"#).is_err());
        let unsupported: Inventory = serde_json::from_str(
            r#"{"version":2,"profile":"typescript","options":{"dependencies":false}}"#,
        )
        .unwrap();
        assert!(unsupported.capture(root, &excluded).is_err());
    }

    #[test]
    fn dependency_tree_has_its_own_budget_beside_the_project_bound() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::write(root.join("Cargo.toml"), "[workspace]").unwrap();
        fs::write(root.join("Cargo.lock"), "version=4").unwrap();
        fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        let fill = |directory: &Path, count: usize| {
            fs::create_dir_all(directory).unwrap();
            for index in 0..count {
                fs::write(directory.join(format!("file{index}.rs")), "// synthetic\n").unwrap();
            }
        };
        let vendor = root.join("vendor");
        // More dependency files than the whole project bound are accepted,
        // as is a generated dependency source above the project file bound.
        fill(&vendor.join("first"), PROJECT_FILES);
        let generated = vec![b' '; PROJECT_FILE as usize + 1];
        fs::write(vendor.join("first/generated.rs"), &generated).unwrap();
        let excluded = root.join("state");
        let disabled = Inventory::new(Profile::Rust, &Options::default());
        assert_eq!(disabled.capture(root, &excluded).unwrap().len(), 3);
        assert_eq!(disabled.file_budget(), PROJECT_FILES);
        let enabled = Inventory::new(
            Profile::Rust,
            &Options {
                dependencies: true,
                ..Default::default()
            },
        );
        assert_eq!(enabled.file_budget(), PROJECT_FILES + DEPENDENCY_FILES);
        assert_eq!(
            enabled.capture(root, &excluded).unwrap().len(),
            3 + PROJECT_FILES + 1
        );
        let exceeded = |inventory: &Inventory| {
            inventory
                .capture(root, &excluded)
                .unwrap_err()
                .to_string()
                .contains("snapshot file budget exceeded")
        };
        // The dependency share is bounded on its own.
        fill(&vendor.join("second"), DEPENDENCY_FILES - PROJECT_FILES);
        assert!(exceeded(&enabled));
        fs::remove_dir_all(vendor.join("second")).unwrap();
        // Dependencies do not widen the project share.
        fs::write(root.join("generated.rs"), &generated).unwrap();
        assert!(
            enabled
                .capture(root, &excluded)
                .unwrap_err()
                .to_string()
                .contains("source read budget exceeded")
        );
        fs::remove_file(root.join("generated.rs")).unwrap();
        fill(&root.join("src"), PROJECT_FILES);
        assert!(exceeded(&enabled) && exceeded(&disabled));
    }

    #[test]
    fn workspace_exclusions_also_filter_the_separate_dependency_walk() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("example");
        fs::create_dir_all(root.join("node_modules/package")).unwrap();
        fs::write(root.join("tsconfig.json"), "{}").unwrap();
        fs::write(root.join("main.ts"), "export {};").unwrap();
        fs::write(root.join("node_modules/package/index.d.ts"), "export {};").unwrap();
        let inventory = Inventory::new(
            Profile::TypeScript,
            &Options {
                dependencies: true,
                exclude: vec!["/example/node_modules/".into()],
                exclude_prefix: "example".into(),
            },
        );
        let files = inventory
            .capture(&root, &directory.path().join("state"))
            .unwrap();
        assert_eq!(files.len(), 2);
        assert!(!files.contains_key("node_modules/package/index.d.ts"));
    }
}
