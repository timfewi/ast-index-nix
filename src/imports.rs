//! Static file dependencies. Resolution only consults the set of indexed paths;
//! it never opens, follows, evaluates or downloads an imported module.
use crate::{error::Result, store::Store};
use rusqlite::params;
use serde::Serialize;
use std::collections::HashMap;
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
    for (id, file, name, path, blocked) in rows {
        let (language, project) = &files[&file];
        let (target, reason) = if let Some(blocked) = blocked {
            (None, blocked)
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
}
