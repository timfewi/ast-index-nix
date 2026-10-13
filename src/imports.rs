//! Static file dependencies. Resolution only consults the set of indexed paths;
//! it never opens, follows, evaluates or downloads an imported module.
use crate::{error::Result, store::Store};
use rusqlite::params;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path};

#[derive(Debug, Serialize)]
pub struct Import {
    pub file: String,
    pub line: u32,
    pub name: String,
    pub target: Option<String>,
    pub reason: String,
}

pub(crate) fn resolve(store: &Store) -> Result<()> {
    let files: HashMap<String, (String, String)> = store
        .conn
        .prepare("SELECT path, lang, project FROM files")?
        .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
        .collect::<rusqlite::Result<_>>()?;
    type ImportRow = (i64, String, String, Option<String>, Option<String>);
    let rows: Vec<ImportRow> = store.conn.prepare(
        "SELECT r.id, f.path, r.name, r.path, r.blocked FROM refs r JOIN files f ON f.id = r.file_id WHERE r.kind = 'import'")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<rusqlite::Result<_>>()?;
    let go_modules: HashMap<String, (Option<String>, String)> =
        serde_json::from_str(&store.meta("go_modules")?.unwrap_or_else(|| "{}".into()))?;
    let go_packages = go_packages(store)?;
    for (id, file, name, path, blocked) in rows {
        let (language, project) = &files[&file];
        let (target, reason) = if language == "go" && crate::resolve::go::ignored_file(&file) {
            (None, "ignored_go_file".into())
        } else if let Some(blocked) = blocked {
            (None, blocked)
        } else if language == "go" {
            resolve_go(&go_packages, project, &name, &go_modules)
        } else {
            resolve_path(
                &files,
                &file,
                language,
                project,
                path.as_deref().unwrap_or(&name),
            )
        };
        store.conn.execute(
            "UPDATE refs SET import_target = ?2, reason = ?3 WHERE id = ?1",
            params![id, target, reason],
        )?;
    }
    Ok(())
}

#[derive(Default)]
struct GoPackage {
    representative: String,
    names: HashSet<String>,
    incomplete: bool,
}

fn go_packages(store: &Store) -> Result<HashMap<(String, String), GoPackage>> {
    let mut packages = HashMap::<(String, String), GoPackage>::new();
    let mut statement = store.conn.prepare(
        "SELECT f.path, f.project, f.parse_errors, s.name FROM files f LEFT JOIN symbols s ON s.file_id = f.id AND s.kind = 'module' AND instr(s.qualified, '::') = 0 WHERE f.lang = 'go'"
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, bool>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    for row in rows {
        let (file, project, errors, name) = row?;
        if file.ends_with("_test.go") || crate::resolve::go::ignored_file(&file) {
            continue;
        }
        let package = packages
            .entry((project, crate::store::directory_of(&file)))
            .or_default();
        if package.representative.is_empty() || file < package.representative {
            package.representative = file;
        }
        package.incomplete |= errors || name.is_none();
        if let Some(name) = name {
            package.names.insert(name);
        }
    }
    Ok(packages)
}

fn resolve_go(
    packages: &HashMap<(String, String), GoPackage>,
    project: &str,
    specifier: &str,
    modules: &HashMap<String, (Option<String>, String)>,
) -> (Option<String>, String) {
    let Some((Some(module), _)) = modules.get(project) else {
        return (None, "unknown_module".into());
    };
    let relative = if specifier == module {
        ""
    } else if let Some(tail) = specifier.strip_prefix(&format!("{module}/")) {
        tail
    } else {
        return (None, "external".into());
    };
    if relative.split('/').any(|p| p == "." || p == "..") || relative.contains(['\\', '\0']) {
        return (None, "outside_root".into());
    }
    let directory = [project, relative]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    // Go imports packages, not individual files. The stable representative only
    // identifies that directory; the Go resolver checks the entire package.
    let Some(package) = packages.get(&(project.to_owned(), directory)) else {
        return (None, "unknown".into());
    };
    if package.names.len() > 1 {
        return (None, "ambiguous".into());
    }
    if package.incomplete || package.names.is_empty() {
        return (None, "incomplete_package".into());
    }
    if package.names.contains("main") {
        return (None, "non_importable_package".into());
    }
    (
        Some(package.representative.clone()),
        "indexed_go_package".into(),
    )
}

fn normalize(base: &str, specifier: &str) -> Option<String> {
    if specifier.contains('\\') || specifier.contains('\0') {
        return None;
    }
    let mut parts: Vec<String> = base
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    for part in Path::new(specifier).components() {
        match part {
            Component::Normal(p) => parts.push(p.to_str()?.into()),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

fn resolve_path(
    files: &HashMap<String, (String, String)>,
    file: &str,
    language: &str,
    project: &str,
    specifier: &str,
) -> (Option<String>, String) {
    let parent = file.rsplit_once('/').map(|(p, _)| p).unwrap_or_default();
    let mut candidates = Vec::new();
    match language {
        "bash" => return (None, "runtime_working_directory".into()),
        "lua" => {
            if specifier.is_empty()
                || !specifier.split('.').all(|part| {
                    !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                })
            {
                return (None, "unsupported_module_path".into());
            }
            let module = specifier.replace('.', "/");
            let base = if project.is_empty() {
                module
            } else {
                format!("{project}/{module}")
            };
            candidates.extend([format!("{base}.lua"), format!("{base}/init.lua")]);
        }
        "nix" | "javascript" | "typescript" => {
            if !specifier.starts_with('.') {
                return (None, "external".into());
            }
            let Some(base) = normalize(parent, specifier) else {
                return (None, "outside_root".into());
            };
            if language == "nix" {
                candidates.extend([base.clone(), format!("{base}/default.nix")]);
            } else {
                candidates.push(base.clone());
                if Path::new(&base).extension().is_none() {
                    for extension in ["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"] {
                        candidates.extend([
                            format!("{base}.{extension}"),
                            format!("{base}/index.{extension}"),
                        ]);
                    }
                }
            }
        }
        "python" => {
            let dots = specifier.chars().take_while(|&c| c == '.').count();
            let tail = specifier[dots..].replace('.', "/");
            let base = if dots > 0 {
                let path = format!("{}{tail}", "../".repeat(dots - 1));
                let Some(path) = normalize(parent, &path) else {
                    return (None, "outside_root".into());
                };
                path
            } else {
                // Absolute Python imports can be external; only the project root
                // layout is supported, without PYTHONPATH or executing setup.py.
                if project.is_empty() {
                    tail
                } else {
                    format!("{project}/{tail}")
                }
            };
            candidates.extend([
                format!("{base}.py"),
                format!("{base}.pyi"),
                format!("{base}/__init__.py"),
            ]);
        }
        "rust" => {
            let Some(rest) = specifier.strip_prefix("crate::") else {
                return (None, "external".into());
            };
            if rest.contains(['{', '*', ' ']) {
                return (None, "unsupported".into());
            }
            let parts: Vec<&str> = rest.split("::").collect();
            let prefix = if project.is_empty() {
                String::new()
            } else {
                format!("{project}/")
            };
            // A use path may name either a module or a symbol in that module.
            for count in [parts.len(), parts.len().saturating_sub(1)] {
                if count == 0 {
                    continue;
                }
                let module = parts[..count].join("/");
                for source in ["src/", ""] {
                    candidates.extend([
                        format!("{prefix}{source}{module}.rs"),
                        format!("{prefix}{source}{module}/mod.rs"),
                    ]);
                }
            }
        }
        _ => return (None, "unsupported".into()),
    }
    candidates.retain(|path| {
        files.get(path).is_some_and(|(lang, owner)| {
            owner == project
                && (lang == language
                    || matches!(
                        (lang.as_str(), language),
                        ("javascript", "typescript") | ("typescript", "javascript")
                    ))
        })
    });
    candidates.sort();
    candidates.dedup();
    match candidates.as_slice() {
        [one] => (Some(one.clone()), "literal_indexed_file".into()),
        [] => (None, "unknown".into()),
        _ => (None, "ambiguous".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalization_cannot_escape_root() {
        assert_eq!(normalize("src", "../mod.nix"), Some("mod.nix".into()));
        for path in ["../../outside", "/absolute", "../..", "bad\\path"] {
            assert_eq!(normalize("src", path), None);
        }
    }

    #[test]
    fn go_packages_reject_ambiguity_and_preserve_module_boundaries() {
        let modules = HashMap::from([(
            "app".into(),
            (Some("example.test/app".into()), String::new()),
        )]);
        let key = ("app".into(), "app/library".into());
        let mut packages = HashMap::from([(
            key.clone(),
            GoPackage {
                representative: "app/library/a.go".into(),
                names: HashSet::from(["lib".into()]),
                incomplete: false,
            },
        )]);
        assert_eq!(
            resolve_go(&packages, "app", "example.test/app/library", &modules),
            (Some("app/library/a.go".into()), "indexed_go_package".into())
        );
        for specifier in ["example.test/app/../escape", "example.test/app/lib\\name"] {
            assert_eq!(
                resolve_go(&packages, "app", specifier, &modules).1,
                "outside_root"
            );
        }
        assert_eq!(
            resolve_go(
                &packages,
                "app",
                "example.test/application/library",
                &modules
            )
            .1,
            "external"
        );
        assert_eq!(
            resolve_go(
                &packages,
                "app",
                "example.test/app/nested/library",
                &modules
            )
            .1,
            "unknown"
        );
        packages.get_mut(&key).unwrap().names.insert("other".into());
        assert_eq!(
            resolve_go(&packages, "app", "example.test/app/library", &modules).1,
            "ambiguous"
        );
        packages.get_mut(&key).unwrap().names.remove("other");
        packages.get_mut(&key).unwrap().incomplete = true;
        assert_eq!(
            resolve_go(&packages, "app", "example.test/app/library", &modules).1,
            "incomplete_package"
        );
    }

    #[test]
    fn ignored_go_files_never_supply_import_representatives() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("index.sqlite")).unwrap();
        for (file, package) in [
            ("pkg/_ignored.go", "wrong"),
            ("pkg/.hidden.go", "wrong"),
            ("pkg/z.go", "real"),
        ] {
            let id = store.upsert_file(file, "go", 0, 0, "fixture").unwrap();
            store
                .insert_symbols(
                    id,
                    &[crate::store::SymbolInsert {
                        name: package.into(),
                        kind: crate::model::SymbolKind::Module,
                        start_line: 1,
                        end_line: 1,
                        start_column: 1,
                        end_column: 10,
                        parent: None,
                        qualified: package.into(),
                    }],
                )
                .unwrap();
            if file == "pkg/_ignored.go" {
                store
                    .insert_refs(
                        id,
                        &[crate::store::RefInsert {
                            name: "example.test/pkg".into(),
                            path: None,
                            kind: crate::model::RefKind::Import,
                            line: 1,
                            from_symbol_id: None,
                            confidence: crate::model::Confidence::None,
                            blocked: None,
                        }],
                    )
                    .unwrap();
            }
        }
        let packages = go_packages(&store).unwrap();
        let package = &packages[&(String::new(), "pkg".into())];
        assert_eq!(package.representative, "pkg/z.go");
        assert_eq!(package.names, HashSet::from(["real".into()]));
        assert!(!package.incomplete);
        resolve(&store).unwrap();
        let (reason, target): (String, Option<String>) = store
            .conn
            .query_row("SELECT reason, import_target FROM refs", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(reason, "ignored_go_file");
        assert_eq!(target, None);
    }
}
