//! Explicit project/profile refresh and read-only selection of published state.
//! A private candidate database replaces the previous snapshot only on success.
use crate::{
    Engine,
    producer::{self, Inventory, Options, Profile},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub project: String,
    pub profile: Profile,
    #[serde(default)]
    pub dependencies: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    version: u8,
    projects: Vec<Selection>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Current,
    Updated,
    Failed,
    BudgetExceeded,
}

#[derive(Debug, Serialize)]
pub struct RefreshResult {
    entry: usize,
    profile: Profile,
    pub status: Outcome,
}

pub(crate) fn project_root(workspace: &Path, relative: &str) -> Result<PathBuf> {
    let normalized = crate::query::relative_path(relative)?;
    ensure!(
        !relative.is_empty() && !relative.chars().any(char::is_control),
        "invalid project selection"
    );
    let root = workspace.join(normalized);
    ensure!(
        root.is_dir() && root.canonicalize()? == root,
        "project must be a regular directory within the workspace, without symbolic links"
    );
    Ok(root)
}

/// The digest is private identity metadata, not anonymization or a credential.
fn state_path(root: &Path, base: &Path, profile: Profile) -> PathBuf {
    base.join(
        blake3::hash(root.as_os_str().as_encoded_bytes())
            .to_hex()
            .as_str(),
    )
    .join(profile.name())
}

fn regular_private(path: &Path, directory: bool) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        path.canonicalize()? == path,
        "managed state cannot contain symbolic links"
    );
    ensure!(
        if directory {
            metadata.is_dir()
        } else {
            metadata.is_file()
        },
        "managed state must be a regular file/directory"
    );
    ensure!(
        metadata.permissions().mode() & 0o077 == 0,
        "managed state must be private"
    );
    Ok(())
}

fn published_state(root: &Path, base: &Path, profile: Profile) -> Result<PathBuf> {
    regular_private(base, true)?;
    ensure!(
        !base.starts_with(root) && !root.starts_with(base),
        "managed state and project must be separate"
    );
    let state = state_path(root, base, profile);
    regular_private(state.parent().context("missing identity directory")?, true)?;
    regular_private(&state, true)?;
    let database = state.join(crate::engine::DB_FILE);
    regular_private(&database, false)?;
    // Published databases use rollback journaling and have no active writer.
    for suffix in ["-wal", "-shm", "-journal"] {
        ensure!(
            !state
                .join(format!("{}{suffix}", crate::engine::DB_FILE))
                .exists(),
            "managed database has unexpected sidecars; explicit repair required"
        );
    }
    Ok(state)
}

fn existing(root: &Path, base: &Path, profile: Profile) -> Result<Engine> {
    let state = published_state(root, base, profile)?;
    Ok(Engine::open_existing_state_dir(root.to_owned(), &state)?)
}

pub(crate) fn query(
    workspace: &Path,
    base: &Path,
    exclude: &[String],
    request: &crate::query::Query,
) -> crate::Result<serde_json::Value> {
    let operation = || -> Result<Engine> {
        let root = project_root(
            workspace,
            request.project.as_deref().context("project is required")?,
        )?;
        let profile = request.profile.context("profile is required")?;
        let base = if base.is_absolute() {
            base.to_owned()
        } else {
            std::env::current_dir()?.join(base)
        };
        let prefix = crate::query::relative_path(request.project.as_deref().unwrap_or(""))?;
        crate::index::exclusions(&root, exclude, &prefix)?;
        let engine = existing(&root, &base, profile)?;
        let inventory: Option<Inventory> = engine
            .store()
            .meta("semantic_input_inventory")?
            .map(|s| serde_json::from_str(&s))
            .transpose()?
            .flatten();
        ensure!(
            inventory.is_some_and(|i| i.matches_scope(profile, &prefix, exclude)),
            "project/profile exclusion policy changed; refresh semantic evidence"
        );
        Ok(engine)
    };
    let engine = operation().map_err(|e| {
        crate::Error::Invalid(format!(
            "managed semantic profile unavailable: {e}; run an explicit semantic refresh"
        ))
    })?;
    if request.action == "freshness" {
        return engine.store().query_transaction(|| {
            let mut value = serde_json::to_value(crate::index::check_freshness(
                engine.root(),
                engine.store(),
            )?)?;
            let semantic_current =
                crate::scip::calls::require_current(engine.root(), engine.store()).is_ok();
            if !semantic_current && value["state"] == "fresh_at_check" {
                value["state"] = "stale".into();
            }
            value["semantic_current"] = semantic_current.into();
            Ok(value)
        });
    }
    // Preserve normal query errors and project/profile in cursor fingerprints.
    engine.execute_local(request)
}

fn current(engine: &Engine, profile: Profile, options: &Options) -> Result<bool> {
    let inventory: Option<Inventory> = engine
        .store()
        .meta("semantic_input_inventory")?
        .map(|s| serde_json::from_str(&s))
        .transpose()?
        .flatten();
    Ok(
        inventory.as_ref() == Some(&Inventory::new(profile, options))
            && crate::scip::calls::require_current(engine.root(), engine.store()).is_ok(),
    )
}

/// Refresh one explicitly selected project. Unchanged validated inputs skip the
/// producer. The global base lock bounds concurrent compiler work for this base.
pub fn refresh(
    workspace: &Path,
    base: &Path,
    selection: &Selection,
    exclude: &[String],
) -> Result<Outcome> {
    let workspace = workspace.canonicalize()?;
    let root = project_root(&workspace, &selection.project)?;
    let prefix = crate::query::relative_path(&selection.project)?;
    crate::index::exclusions(&root, exclude, &prefix)?;
    producer::private_directory(base)?;
    let base = base.canonicalize()?;
    ensure!(
        !base.starts_with(&root) && !root.starts_with(&base),
        "managed state and project must be separate"
    );
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(base.join("refresh.lock"))?;
    ensure!(
        lock.metadata()?.is_file(),
        "refresh lock must be a regular file"
    );
    let _guard = nix::fcntl::Flock::lock(lock, nix::fcntl::FlockArg::LockExclusiveNonblock)
        .map_err(|(_, _)| anyhow::anyhow!("semantic refresh busy; wait for the active runner"))?;
    let options = Options {
        dependencies: selection.dependencies,
        exclude: exclude.to_owned(),
        exclude_prefix: prefix,
    };
    let state = state_path(&root, &base, selection.profile);
    let database = state.join(crate::engine::DB_FILE);
    let previous = if database.try_exists()? {
        published_state(&root, &base, selection.profile)?;
        // A previous schema is migrated only in the disposable candidate.
        if let Ok(engine) = existing(&root, &base, selection.profile)
            && current(&engine, selection.profile, &options)?
        {
            return Ok(Outcome::Current);
        }
        true
    } else {
        false
    };
    producer::private_directory(state.parent().context("missing identity directory")?)?;
    producer::private_directory(&state)?;
    let candidate = tempfile::Builder::new()
        .prefix("candidate-")
        .tempdir_in(&state)?;
    fs::set_permissions(candidate.path(), fs::Permissions::from_mode(0o700))?;
    let candidate_db = candidate.path().join(crate::engine::DB_FILE);
    if previous {
        // Read-only published state is stable under our refresh lock. Copying
        // retains generations/cursors and the previous structural snapshot.
        fs::copy(&database, &candidate_db)?;
    }
    match selection.profile {
        Profile::Rust => producer::rust(&root, candidate.path(), &options)?,
        Profile::TypeScript => producer::typescript(&root, candidate.path(), &options)?,
    };
    {
        let engine = Engine::open_state_dir(&root, candidate.path())?;
        ensure!(
            current(&engine, selection.profile, &options)?,
            "candidate became stale before publication"
        );
        let journal: String =
            engine
                .store()
                .conn
                .query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))?;
        ensure!(
            journal == "delete",
            "candidate journal could not be finalized"
        );
    }
    fs::set_permissions(&candidate_db, fs::Permissions::from_mode(0o600))?;
    fs::File::open(&candidate_db)?.sync_all()?;
    fs::rename(&candidate_db, &database)?;
    fs::File::open(&state)?.sync_all()?;
    Ok(Outcome::Updated)
}

/// Runtime-only allowlist: no discovery, installation or source-derived grants.
/// Validate the whole plan before starting any compiler. Reports omit paths,
/// source diagnostics, hashes and symbols so service journals contain no corpus.
pub fn refresh_plan(
    workspace: &Path,
    base: &Path,
    plan: &Path,
    exclude: &[String],
) -> Result<Vec<RefreshResult>> {
    let workspace = workspace.canonicalize()?;
    let plan_path = if plan.is_absolute() {
        plan.to_owned()
    } else {
        std::env::current_dir()?.join(plan)
    };
    regular_private(&plan_path, false)?;
    ensure!(
        !plan_path.starts_with("/nix/store"),
        "project selections belong in private runtime state, not the Nix store"
    );
    let plan: Plan = serde_json::from_slice(&producer::read(&plan_path, 64 * 1024)?)
        .map_err(|_| anyhow::anyhow!("invalid semantic refresh plan"))?;
    ensure!(
        plan.version == 1 && plan.projects.len() <= 8,
        "refresh plan requires version 1 and at most 8 project/profile selections"
    );
    let mut identities = std::collections::HashSet::new();
    let resolved_base = if base.exists() {
        base.canonicalize()?
    } else {
        let absolute = if base.is_absolute() {
            base.to_owned()
        } else {
            std::env::current_dir()?.join(base)
        };
        absolute
            .parent()
            .context("state base needs a parent")?
            .canonicalize()?
            .join(absolute.file_name().context("state base needs a name")?)
    };
    for selection in &plan.projects {
        let root = project_root(&workspace, &selection.project)?;
        crate::index::exclusions(
            &root,
            exclude,
            &crate::query::relative_path(&selection.project)?,
        )?;
        ensure!(
            !resolved_base.starts_with(&root) && !root.starts_with(&resolved_base),
            "managed state and project must be separate"
        );
        ensure!(
            identities.insert((root, selection.profile.name())),
            "duplicate project/profile selection"
        );
    }
    let started = std::time::Instant::now();
    let mut results = Vec::new();
    for (entry, selection) in plan.projects.iter().enumerate() {
        let status = if started.elapsed().as_secs() >= 300 {
            Outcome::BudgetExceeded
        } else {
            refresh(&workspace, base, selection, exclude).unwrap_or(Outcome::Failed)
        };
        results.push(RefreshResult {
            entry,
            profile: selection.profile,
            status,
        });
    }
    Ok(results)
}
