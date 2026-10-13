//! Static Go package lookup, constrained to indexed files and import evidence.

use std::collections::{HashMap, HashSet};

use crate::error::Result;
use crate::model::{Confidence, SymbolKind};
use crate::store::{RefRow, Store, SymbolRow, directory_of};

type PackageKey = (String, String, String);

/// Go excludes these filenames regardless of platform or build-tag selection.
pub(crate) fn ignored_file(file: &str) -> bool {
    file.rsplit('/')
        .next()
        .is_some_and(|name| name.starts_with(['_', '.']))
}

struct Target {
    id: i64,
    file: String,
    kind: SymbolKind,
    exported: bool,
}

struct Import {
    alias: Option<String>,
    target: Option<PackageKey>,
}

#[derive(Default)]
pub(super) struct GoResolver {
    symbols: HashMap<PackageKey, HashMap<String, Vec<Target>>>,
    imports: HashMap<String, Vec<Import>>,
    dot_imports: HashSet<String>,
}

impl GoResolver {
    pub(super) fn new(store: &Store, symbols: &[SymbolRow]) -> Result<Self> {
        let mut resolver = Self::default();
        let mut packages = HashMap::<String, HashSet<String>>::new();
        let mut directory_packages = HashMap::<(String, String), HashSet<String>>::new();
        let mut projects = HashMap::new();
        for symbol in symbols
            .iter()
            .filter(|symbol| symbol.language == "go" && !ignored_file(&symbol.file))
        {
            projects.insert(symbol.file.clone(), symbol.project.clone());
            if symbol.kind == SymbolKind::Module && !symbol.qualified.contains("::") {
                packages
                    .entry(symbol.file.clone())
                    .or_default()
                    .insert(symbol.name.clone());
                if !symbol.file.ends_with("_test.go") {
                    directory_packages
                        .entry((symbol.project.clone(), symbol.directory.clone()))
                        .or_default()
                        .insert(symbol.name.clone());
                }
            }
            let Some((package, name)) = symbol.qualified.split_once("::") else {
                continue;
            };
            if name.contains("::") {
                continue;
            }
            resolver
                .symbols
                .entry((
                    symbol.project.clone(),
                    symbol.directory.clone(),
                    package.to_owned(),
                ))
                .or_default()
                .entry(name.to_owned())
                .or_default()
                .push(Target {
                    id: symbol.id,
                    file: symbol.file.clone(),
                    kind: symbol.kind,
                    exported: symbol.exported,
                });
        }
        let mut statement = store.conn.prepare(
            "SELECT f.path, r.path, r.import_target FROM refs r JOIN files f ON f.id = r.file_id WHERE f.lang = 'go' AND r.kind = 'import'"
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;
        for row in rows {
            let (file, alias, target_file) = row?;
            if ignored_file(&file) {
                continue;
            }
            if alias.as_deref() == Some(".") {
                resolver.dot_imports.insert(file);
                continue;
            }
            if alias.as_deref() == Some("_") {
                continue;
            }
            let target = target_file.and_then(|target_file| {
                let package = packages.get(&target_file)?;
                if package.len() != 1 || target_file.ends_with("_test.go") {
                    return None;
                }
                let project = projects.get(&target_file)?;
                if projects.get(&file) != Some(project) {
                    return None;
                }
                if directory_packages
                    .get(&(project.clone(), directory_of(&target_file)))?
                    .len()
                    != 1
                {
                    return None;
                }
                Some((
                    project.clone(),
                    directory_of(&target_file),
                    package.iter().next()?.clone(),
                ))
            });
            let alias = alias.or_else(|| target.as_ref().map(|(_, _, package)| package.clone()));
            resolver
                .imports
                .entry(file)
                .or_default()
                .push(Import { alias, target });
        }
        Ok(resolver)
    }

    pub(super) fn resolve<'a>(&self, reference: &'a RefRow) -> (Option<i64>, Confidence, &'a str) {
        let unresolved = |reason| (None, Confidence::None, reason);
        if ignored_file(&reference.file) {
            return unresolved("ignored_go_file");
        }
        if let Some(reason) = reference.blocked.as_deref() {
            return unresolved(reason);
        }
        let Some(path) = reference.path.as_deref() else {
            return unresolved("unsupported");
        };
        let (key, name, imported) = if let Some(path) = path.strip_prefix("@go:") {
            let Ok((package, name)) = serde_json::from_str::<(String, String)>(path) else {
                return unresolved("unsupported");
            };
            if self.dot_imports.contains(&reference.file) {
                return unresolved("unsupported_dot_import");
            }
            if self.imports.get(&reference.file).is_some_and(|imports| {
                imports
                    .iter()
                    .any(|import| import.alias.as_deref() == Some(&name))
            }) {
                return unresolved("imported_package");
            }
            (
                (
                    reference.project.clone(),
                    reference.directory.clone(),
                    package,
                ),
                name,
                false,
            )
        } else if let Some(path) = path.strip_prefix("@go_selector:") {
            let Ok((_package, receiver, name)) =
                serde_json::from_str::<(String, String, String)>(path)
            else {
                return unresolved("unsupported");
            };
            let Some(imports) = self.imports.get(&reference.file) else {
                return unresolved("dynamic_receiver");
            };
            let mut matching = imports
                .iter()
                .filter(|import| import.alias.as_deref() == Some(&receiver));
            let Some(import) = matching.next() else {
                return unresolved("dynamic_receiver");
            };
            if matching.next().is_some() {
                return unresolved("ambiguous");
            }
            let Some(target) = import.target.as_ref() else {
                return unresolved("unsupported_import");
            };
            (target.clone(), name, true)
        } else {
            return unresolved("unsupported");
        };
        let Some(symbols) = self
            .symbols
            .get(&key)
            .and_then(|symbols| symbols.get(&name))
        else {
            return unresolved("unknown");
        };
        let test_context = !imported && reference.file.ends_with("_test.go");
        let mut candidates = symbols
            .iter()
            .filter(|target| test_context || !target.file.ends_with("_test.go"));
        let Some(target) = candidates.next() else {
            return unresolved("unknown");
        };
        if candidates.next().is_some() {
            return unresolved("ambiguous");
        }
        if target.kind != SymbolKind::Function {
            return unresolved("non_function_binding");
        }
        if imported && !target.exported {
            return unresolved("unexported_symbol");
        }
        let confidence = if target.file == reference.file {
            Confidence::Exact
        } else if directory_of(&target.file) == reference.directory {
            Confidence::High
        } else {
            Confidence::Low
        };
        (
            Some(target.id),
            confidence,
            if imported {
                "go_imported_package"
            } else {
                "go_package"
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RefKind;

    fn target(id: i64, file: &str, kind: SymbolKind) -> Target {
        Target {
            id,
            file: file.into(),
            kind,
            exported: true,
        }
    }

    fn reference(file: &str, package: &str, name: &str) -> RefRow {
        RefRow {
            id: 1,
            file: file.into(),
            directory: directory_of(file),
            language: "go".into(),
            project: "app".into(),
            name: name.into(),
            path: Some(format!(
                "@go:{}",
                serde_json::to_string(&(package, name)).unwrap()
            )),
            kind: RefKind::Call,
            blocked: None,
        }
    }

    #[test]
    fn package_candidates_require_unique_functions_and_respect_test_files() {
        let mut resolver = GoResolver::default();
        resolver.symbols.insert(
            ("app".into(), "app/pkg".into(), "pkg".into()),
            HashMap::from([
                (
                    "Work".into(),
                    vec![target(1, "app/pkg/worker.go", SymbolKind::Function)],
                ),
                (
                    "Callback".into(),
                    vec![target(2, "app/pkg/worker.go", SymbolKind::Static)],
                ),
                (
                    "Duplicate".into(),
                    vec![
                        target(3, "app/pkg/first.go", SymbolKind::Function),
                        target(4, "app/pkg/second.go", SymbolKind::Function),
                    ],
                ),
                (
                    "TestHelper".into(),
                    vec![target(5, "app/pkg/worker_test.go", SymbolKind::Function)],
                ),
            ]),
        );
        assert_eq!(
            resolver.resolve(&reference("app/pkg/main.go", "pkg", "Work")),
            (Some(1), Confidence::High, "go_package")
        );
        assert_eq!(
            resolver
                .resolve(&reference("app/pkg/main.go", "pkg", "Callback"))
                .2,
            "non_function_binding"
        );
        assert_eq!(
            resolver
                .resolve(&reference("app/pkg/main.go", "pkg", "Duplicate"))
                .2,
            "ambiguous"
        );
        assert_eq!(
            resolver
                .resolve(&reference("app/pkg/main.go", "pkg", "TestHelper"))
                .0,
            None
        );
        assert_eq!(
            resolver
                .resolve(&reference("app/pkg/main_test.go", "pkg", "TestHelper"))
                .0,
            Some(5)
        );
        assert_eq!(
            resolver
                .resolve(&reference("app/pkg/main_test.go", "pkg_test", "Work"))
                .0,
            None
        );
        assert_eq!(
            resolver
                .resolve(&reference("app/other/main.go", "pkg", "Work"))
                .0,
            None
        );
    }

    #[test]
    fn imported_selectors_require_alias_export_and_a_static_package_target() {
        let mut resolver = GoResolver::default();
        let key = ("app".into(), "app/library".into(), "different_name".into());
        let mut private = target(2, "app/library/private.go", SymbolKind::Function);
        private.exported = false;
        resolver.symbols.insert(
            key.clone(),
            HashMap::from([
                (
                    "Work".into(),
                    vec![target(1, "app/library/worker.go", SymbolKind::Function)],
                ),
                ("hidden".into(), vec![private]),
            ]),
        );
        resolver.imports.insert(
            "app/main.go".into(),
            vec![Import {
                alias: Some("lib".into()),
                target: Some(key),
            }],
        );
        let mut reference = reference("app/main.go", "main", "Work");
        reference.path = Some("@go_selector:[\"main\",\"lib\",\"Work\"]".into());
        assert_eq!(
            resolver.resolve(&reference),
            (Some(1), Confidence::Low, "go_imported_package")
        );
        reference.path = Some("@go_selector:[\"main\",\"lib\",\"hidden\"]".into());
        assert_eq!(resolver.resolve(&reference).2, "unexported_symbol");
        reference.path = Some("@go_selector:[\"main\",\"receiver\",\"Work\"]".into());
        assert_eq!(resolver.resolve(&reference).2, "dynamic_receiver");
        reference.path = Some("@go:[\"main\",\"Work\"]".into());
        resolver.dot_imports.insert("app/main.go".into());
        assert_eq!(resolver.resolve(&reference).2, "unsupported_dot_import");
    }

    #[test]
    fn ignored_go_files_never_supply_package_call_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("index.sqlite")).unwrap();
        let symbols: Vec<_> = [
            (1, "app/_ignored.go", "println"),
            (2, "app/.hidden.go", "hidden"),
            (3, "app/main.go", "actual"),
        ]
        .into_iter()
        .map(|(id, file, name)| SymbolRow {
            id,
            file: file.into(),
            language: "go".into(),
            project: "app".into(),
            directory: "app".into(),
            name: name.into(),
            qualified: format!("app::{name}"),
            kind: SymbolKind::Function,
            exported: false,
        })
        .collect();
        let resolver = GoResolver::new(&store, &symbols).unwrap();
        for name in ["println", "hidden"] {
            assert_eq!(
                resolver.resolve(&reference("app/main.go", "app", name)).0,
                None,
                "{name}"
            );
        }
        for file in ["app/_ignored.go", "app/.hidden.go"] {
            assert_eq!(
                resolver.resolve(&reference(file, "app", "actual")),
                (None, Confidence::None, "ignored_go_file")
            );
        }
        assert_eq!(
            resolver
                .resolve(&reference("app/main.go", "app", "actual"))
                .0,
            Some(3)
        );
    }
}
