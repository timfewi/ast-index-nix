//! Opt-in ingestion of precomputed semantic evidence. Source paths and hashes
//! are validated; this module never runs indexers or treats references as calls.
use crate::{
    error::{Error, Result},
    query::{Query, Window},
    store::Store,
};
use prost::Message;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    path::Path,
};
mod wire;

const ARTIFACT_BYTES: u64 = 4 * 1024 * 1024;
const SOURCE_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    artifact_blake3: String,
    files: HashMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct ImportStats {
    pub documents: usize,
    pub occurrences: usize,
    pub implementations: usize,
    pub artifact_blake3: String,
    pub generation: i64,
}

fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}

fn bounded_file(path: &Path, limit: u64) -> Result<Vec<u8>> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| Error::io(path, e))?;
    if !file.metadata().map_err(|e| Error::io(path, e))?.is_file() {
        return Err(invalid("artifact must be a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::io(path, e))?;
    if bytes.len() as u64 > limit {
        return Err(invalid("artifact/manifest size budget exceeded"));
    }
    Ok(bytes)
}

fn symbol_key(file: &str, symbol: &str) -> Result<String> {
    if symbol.is_empty() || symbol.len() > 4096 || symbol.contains('\0') {
        return Err(invalid("invalid or oversized SCIP symbol"));
    }
    // SCIP local symbols belong to one document. Global symbols remain opaque
    // identifiers, not paths, executable names or AST call targets.
    Ok(serde_json::to_string(&(
        symbol.starts_with("local ").then_some(file),
        symbol,
    ))?)
}

fn source(root: &Path, path: &str, limit: u64) -> Result<Vec<u8>> {
    if path.is_empty() || crate::query::relative_path(path)? != path {
        return Err(invalid("SCIP document path must be canonical and relative"));
    }
    let mut joined = root.to_path_buf();
    for component in Path::new(path).components() {
        joined.push(component);
        if std::fs::symlink_metadata(&joined)
            .map_err(|e| Error::io(path, e))?
            .file_type()
            .is_symlink()
        {
            return Err(invalid("SCIP document paths cannot contain symbolic links"));
        }
    }
    crate::index::read_source(root, &joined, limit.min(2 * 1024 * 1024))
}

fn range(
    occurrence: &wire::Occurrence,
    lines: &[&str],
    encoding: i32,
    work_left: &mut u64,
) -> Result<[u32; 4]> {
    use wire::occurrence::TypedRange;
    let raw = match &occurrence.typed_range {
        Some(TypedRange::Single(r)) => [r.line, r.start_character, r.line, r.end_character],
        Some(TypedRange::Multi(r)) => {
            [r.start_line, r.start_character, r.end_line, r.end_character]
        }
        None => match occurrence.range.as_slice() {
            [line, start, end] => [*line, *start, *line, *end],
            [line, start, end_line, end] => [*line, *start, *end_line, *end],
            _ => return Err(invalid("SCIP range must have three or four coordinates")),
        },
    };
    if raw.iter().any(|&n| n < 0) || (raw[0], raw[1]) > (raw[2], raw[3]) {
        return Err(invalid("invalid SCIP source range"));
    }
    for (line, column) in [(raw[0], raw[1]), (raw[2], raw[3])] {
        let line = *lines
            .get(line as usize)
            .ok_or_else(|| invalid("SCIP range exceeds document lines"))?;
        let line = line.strip_suffix('\r').unwrap_or(line);
        // UTF-16/32 checks walk the line. Repeated occurrences on a long line
        // must not multiply a small artifact into unbounded validation work.
        let cost = if encoding == 1 {
            1
        } else {
            2 * line.len() as u64 + 1
        };
        *work_left = work_left
            .checked_sub(cost)
            .ok_or_else(|| invalid("SCIP range validation budget exceeded; reduce the artifact"))?;
        let units = match encoding {
            1 => line.len(),
            2 => line.encode_utf16().count(),
            3 => line.chars().count(),
            _ => {
                return Err(invalid(
                    "SCIP requires an explicit supported position encoding",
                ));
            }
        };
        if column as usize > units || (encoding == 1 && !line.is_char_boundary(column as usize)) {
            return Err(invalid(
                "SCIP column exceeds document or splits a UTF-8 character",
            ));
        }
        if encoding == 2 {
            let mut offset = 0;
            let mut boundary = column == 0;
            for ch in line.chars() {
                offset += ch.len_utf16();
                boundary |= offset == column as usize;
            }
            if !boundary {
                return Err(invalid("SCIP column splits a UTF-16 character"));
            }
        }
    }
    Ok(raw.map(|n| n as u32))
}

/// Explicit local artifact ingestion. Either embedded document text or an
/// artifact-bound producer manifest must establish the source snapshot.
pub fn import(
    root: &Path,
    store: &Store,
    artifact: &Path,
    manifest: Option<&Path>,
) -> Result<ImportStats> {
    let _writer = store.write_lock()?;
    store.require_root(root)?;
    let bytes = bounded_file(artifact, ARTIFACT_BYTES)?;
    preflight(&bytes)?;
    let digest = blake3::hash(&bytes).to_hex().to_string();
    let manifest: Option<Manifest> = manifest
        .map(|p| bounded_file(p, 1024 * 1024).and_then(|b| Ok(serde_json::from_slice(&b)?)))
        .transpose()?;
    if manifest
        .as_ref()
        .is_some_and(|m| m.artifact_blake3 != digest)
    {
        return Err(invalid("manifest does not match SCIP artifact digest"));
    }
    let index =
        wire::Index::decode(bytes.as_slice()).map_err(|_| invalid("malformed SCIP protobuf"))?;
    let metadata = index
        .metadata
        .ok_or_else(|| invalid("SCIP metadata missing"))?;
    if metadata.version != 0 || metadata.text_document_encoding != 1 {
        return Err(invalid(
            "supported SCIP subset requires protocol version 0 and UTF-8 document encoding",
        ));
    }
    if !metadata.project_root.starts_with("file:///")
        || metadata.project_root.len() > 4096
        || metadata.project_root.contains('\0')
    {
        return Err(invalid(
            "SCIP metadata requires a local file project-root URI; it is never followed",
        ));
    }
    let tool = metadata.tool_info.unwrap_or_default();
    if tool.name.len() > 512 || tool.version.len() > 128 {
        return Err(invalid("oversized SCIP producer metadata"));
    }
    let mut seen = HashSet::new();
    let mut checked = Vec::new();
    let mut total_bytes = 0u64;
    let mut range_work_left = SOURCE_BYTES;
    for document in index.documents {
        if !seen.insert(document.relative_path.clone()) {
            return Err(invalid("duplicate SCIP document"));
        }
        if !(1..=3).contains(&document.position_encoding) {
            return Err(invalid("unsupported or unspecified SCIP position encoding"));
        }
        let bytes = source(
            root,
            &document.relative_path,
            SOURCE_BYTES.saturating_sub(total_bytes),
        )?;
        total_bytes += bytes.len() as u64;
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| invalid("SCIP documents must be valid UTF-8"))?;
        let supplied_hash = manifest
            .as_ref()
            .and_then(|m| m.files.get(&document.relative_path));
        if document.text.is_empty() && supplied_hash.is_none() {
            return Err(invalid(
                "SCIP freshness cannot be established: provide embedded text or an artifact-bound source-hash manifest",
            ));
        }
        if (!document.text.is_empty() && document.text != text)
            || supplied_hash.is_some_and(|h| h != &hash)
        {
            return Err(invalid(
                "SCIP source snapshot does not match current source bytes",
            ));
        }
        let state = store
            .file_state(&document.relative_path)?
            .ok_or_else(|| invalid("SCIP source must already be indexed"))?;
        if document.language.to_lowercase() != state.lang
            && !(document.language == "tsx" && state.lang == "typescript")
        {
            return Err(invalid("SCIP language does not match indexed file"));
        }
        if state.hash != hash {
            return Err(invalid(
                "AST snapshot is stale; index current source before importing SCIP",
            ));
        }
        let lines: Vec<_> = text.split('\n').collect();
        for occurrence in &document.occurrences {
            if occurrence.symbol_roles < 0 {
                return Err(invalid("invalid SCIP role bitset"));
            }
            range(
                occurrence,
                &lines,
                document.position_encoding,
                &mut range_work_left,
            )?;
            if !occurrence.symbol.is_empty() {
                symbol_key(&document.relative_path, &occurrence.symbol)?;
            }
        }
        for symbol in &document.symbols {
            symbol_key(&document.relative_path, &symbol.symbol)?;
            if symbol.display_name.len() > 4096 {
                return Err(invalid("oversized SCIP display name"));
            }
            for relation in &symbol.relationships {
                symbol_key(&document.relative_path, &relation.symbol)?;
            }
        }
        checked.push((document, hash));
    }
    if checked.is_empty() {
        return Err(invalid("SCIP artifact has no documents"));
    }
    store.transaction(|| {
        store.conn.execute_batch("DELETE FROM semantic_implementations; DELETE FROM semantic_occurrences; DELETE FROM semantic_documents;")?;
        let mut stats=ImportStats { documents:checked.len(),occurrences:0,implementations:0,artifact_blake3:digest.clone(),generation:0 };
        for (document,hash) in &checked {
            let file=&document.relative_path;
            store.conn.execute("INSERT INTO semantic_documents(file,hash,artifact,indexer,indexer_version,encoding) VALUES(?1,?2,?3,?4,?5,?6)",params![file,hash,digest,tool.name,tool.version,document.position_encoding])?;
            let names: HashMap<_,_>=document.symbols.iter().map(|s|(s.symbol.as_str(),s.display_name.as_str())).collect();
            for occurrence in &document.occurrences {
                if occurrence.symbol.is_empty() { continue; }
                let coordinates=match &occurrence.typed_range {
                    Some(wire::occurrence::TypedRange::Single(r))=>vec![r.line,r.start_character,r.line,r.end_character],
                    Some(wire::occurrence::TypedRange::Multi(r))=>vec![r.start_line,r.start_character,r.end_line,r.end_character],
                    None=>match occurrence.range.as_slice(){[l,s,e]=>vec![*l,*s,*l,*e],r=>r.to_vec()},
                };
                store.conn.execute("INSERT INTO semantic_occurrences(file,identity,symbol,display_name,role,range) VALUES(?1,?2,?3,?4,?5,?6)",params![file,symbol_key(file,&occurrence.symbol)?,occurrence.symbol,names.get(occurrence.symbol.as_str()).copied().unwrap_or_default(),occurrence.symbol_roles,serde_json::to_string(&coordinates)?])?;
                stats.occurrences+=1;
            }
            for symbol in &document.symbols {
                for relation in symbol.relationships.iter().filter(|r|r.is_implementation) {
                    store.conn.execute("INSERT INTO semantic_implementations(file,identity,symbol,target_identity,target_symbol) VALUES(?1,?2,?3,?4,?5)",params![file,symbol_key(file,&symbol.symbol)?,symbol.symbol,symbol_key(file,&relation.symbol)?,relation.symbol])?;
                    stats.implementations+=1;
                }
            }
        }
        store.bump_generation()?;
        stats.generation=store.generation()?;
        Ok(stats)
    })
}

pub(crate) fn query(root: &Path, store: &Store, query: &Query, window: &Window) -> Result<Value> {
    let documents: Vec<(String, String)> = store
        .conn
        .prepare("SELECT file,hash FROM semantic_documents ORDER BY file LIMIT 2001")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if documents.is_empty() {
        return Err(invalid("no SCIP artifact imported; use import-scip"));
    }
    if documents.len() > 2000 {
        return Err(invalid("semantic document budget exceeded"));
    }
    let mut total_bytes = 0u64;
    let mut stale = Vec::new();
    for (file, hash) in documents {
        let matches = match source(root, &file, SOURCE_BYTES.saturating_sub(total_bytes)) {
            Ok(bytes) => {
                total_bytes += bytes.len() as u64;
                blake3::hash(&bytes).to_hex().as_str() == hash
            }
            Err(_) => false,
        };
        if !matches {
            stale.push(file);
        }
    }
    if !stale.is_empty() {
        return Ok(
            json!({"items":[],"generation":window.generation,"excluded":"stale_semantic_artifact","truncated":false,"next_cursor":null,"stale_count":stale.len(),"stale_files":stale.into_iter().take(16).collect::<Vec<_>>() }),
        );
    }
    let relation = query.relation.as_deref().unwrap_or("definition");
    let sql=match relation {
        "definition"|"reference"=>"SELECT o.file,o.identity,o.symbol,o.display_name,o.range,d.artifact,d.indexer,d.indexer_version,d.encoding
            FROM semantic_occurrences o JOIN semantic_documents d ON d.file=o.file
            WHERE (?1 IS NULL OR o.symbol=?1 OR o.display_name=?1) AND (?2 IS NULL OR o.file=?2)
            AND ((?3='definition' AND o.role & 1 != 0) OR (?3='reference' AND o.role & 1 = 0))
            ORDER BY o.file,o.id LIMIT ?4 OFFSET ?5",
        "implementation"=>"SELECT o.file,o.identity,o.symbol,o.target_symbol,'null',d.artifact,d.indexer,d.indexer_version,d.encoding
            FROM semantic_implementations o JOIN semantic_documents d ON d.file=o.file
            WHERE (?1 IS NULL OR o.target_symbol=?1) AND (?2 IS NULL OR o.file=?2) AND ?3='implementation'
            ORDER BY o.file,o.id LIMIT ?4 OFFSET ?5",
        _=>return Err(invalid("semantic relation must be definition, reference or implementation")),
    };
    let rows: Vec<Value>=store.conn.prepare(sql)?.query_map(params![query.query,query.file,relation,(window.limit+1) as i64,window.offset as i64],|r| Ok(json!({
        "file":r.get::<_,String>(0)?,"identity":r.get::<_,String>(1)?,"symbol":r.get::<_,String>(2)?,
        "display_or_target":r.get::<_,String>(3)?,"range":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(Value::Null),
        "provenance":{"kind":"scip","artifact_blake3":r.get::<_,String>(5)?,"indexer":r.get::<_,String>(6)?,"version":r.get::<_,String>(7)?},
        "position_encoding":r.get::<_,i32>(8)?,"relation":relation
    })))?.collect::<rusqlite::Result<_>>()?;
    let mut value = serde_json::to_value(window.page(rows)?)?;
    value["freshness"] = json!("all artifact documents matched current bytes at query time");
    value["range_convention"] =
        json!("zero-based, half-open; columns use each document's position encoding");
    Ok(value)
}

/// Reject repeated-message allocation bombs before protobuf decoding.
fn preflight(bytes: &[u8]) -> Result<()> {
    fn varint(bytes: &[u8], at: &mut usize) -> Result<u64> {
        let mut value = 0u64;
        for shift in (0..70).step_by(7) {
            let byte = *bytes
                .get(*at)
                .ok_or_else(|| invalid("truncated SCIP varint"))?;
            *at += 1;
            if shift == 63 && byte > 1 {
                return Err(invalid("oversized SCIP varint"));
            }
            value |= u64::from(byte & 127) << shift;
            if byte & 128 == 0 {
                return Ok(value);
            }
        }
        Err(invalid("invalid SCIP varint"))
    }
    fn walk(bytes: &[u8], kind: u8, counts: &mut [usize; 4]) -> Result<()> {
        let mut at = 0;
        while at < bytes.len() {
            let tag = varint(bytes, &mut at)?;
            if tag >> 3 == 0 {
                return Err(invalid("invalid SCIP field tag"));
            }
            match tag & 7 {
                0 => {
                    varint(bytes, &mut at)?;
                }
                1 => {
                    at = at
                        .checked_add(8)
                        .ok_or_else(|| invalid("SCIP field overflow"))?
                }
                5 => {
                    at = at
                        .checked_add(4)
                        .ok_or_else(|| invalid("SCIP field overflow"))?
                }
                2 => {
                    let length = usize::try_from(varint(bytes, &mut at)?)
                        .map_err(|_| invalid("SCIP length overflow"))?;
                    let end = at
                        .checked_add(length)
                        .filter(|&n| n <= bytes.len())
                        .ok_or_else(|| invalid("truncated SCIP field"))?;
                    let child = match (kind, tag >> 3) {
                        (0, 2) => Some((0, 1)),
                        (1, 2) => Some((1, 3)),
                        (1, 3) => Some((2, 2)),
                        (2, 4) => Some((3, 3)),
                        _ => None,
                    };
                    if let Some((counter, next)) = child {
                        counts[counter] += 1;
                        if counts[counter] > [2000, 100_000, 50_000, 100_000][counter] {
                            return Err(invalid("SCIP repeated-message budget exceeded"));
                        }
                        walk(&bytes[at..end], next, counts)?;
                    }
                    at = end;
                }
                _ => return Err(invalid("unsupported SCIP protobuf wire type")),
            }
            if at > bytes.len() {
                return Err(invalid("truncated SCIP fixed-width field"));
            }
        }
        Ok(())
    }
    walk(bytes, 0, &mut [0; 4])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Engine, index::IndexOptions};
    use tempfile::TempDir;

    fn fixture() -> (TempDir, Engine, wire::Index) {
        let dir = tempfile::tempdir().unwrap();
        let text = "fn helper() {}\nfn entry() { helper(); }\ntrait Base {}\nstruct Impl;\nimpl Base for Impl {}\n";
        std::fs::write(dir.path().join("lib.rs"), text).unwrap();
        let engine = Engine::open(dir.path(), None).unwrap();
        engine.index(&IndexOptions::default()).unwrap();
        let index = wire::Index {
            metadata: Some(wire::Metadata {
                version: 0,
                project_root: format!("file://{}", dir.path().display()),
                text_document_encoding: 1,
                tool_info: Some(wire::ToolInfo {
                    name: "fixture-indexer".into(),
                    version: "1".into(),
                }),
            }),
            documents: vec![wire::Document {
                relative_path: "lib.rs".into(),
                language: "rust".into(),
                text: text.into(),
                position_encoding: 1,
                occurrences: vec![
                    wire::Occurrence {
                        symbol: "fixture helper().".into(),
                        range: vec![0, 3, 9],
                        symbol_roles: 1,
                        ..Default::default()
                    },
                    wire::Occurrence {
                        symbol: "fixture helper().".into(),
                        range: vec![1, 13, 19],
                        ..Default::default()
                    },
                ],
                symbols: vec![
                    wire::SymbolInformation {
                        symbol: "fixture helper().".into(),
                        display_name: "helper".into(),
                        kind: 17,
                        ..Default::default()
                    },
                    wire::SymbolInformation {
                        symbol: "fixture Impl#".into(),
                        display_name: "Impl".into(),
                        kind: 49,
                        relationships: vec![wire::Relationship {
                            symbol: "fixture Base#".into(),
                            is_implementation: true,
                        }],
                    },
                ],
            }],
        };
        (dir, engine, index)
    }
    fn artifact(dir: &TempDir, index: &wire::Index) -> std::path::PathBuf {
        let path = dir.path().join("fixture.scip");
        std::fs::write(&path, index.encode_to_vec()).unwrap();
        path
    }
    fn request(relation: &str) -> Query {
        Query {
            action: "semantic".into(),
            relation: Some(relation.into()),
            ..Query::default()
        }
    }

    #[test]
    fn semantic_evidence_is_separate_and_excluded_when_source_changes() {
        let (dir, engine, index) = fixture();
        let stats = import(dir.path(), engine.store(), &artifact(&dir, &index), None).unwrap();
        assert_eq!(
            (stats.documents, stats.occurrences, stats.implementations),
            (1, 2, 1)
        );
        for relation in ["definition", "reference", "implementation"] {
            let value = engine.execute(&request(relation)).unwrap();
            assert_eq!(value["items"].as_array().unwrap().len(), 1, "{value}");
            assert_eq!(value["items"][0]["provenance"]["kind"], "scip");
        }
        let calls = engine
            .execute(&Query {
                action: "callees".into(),
                query: Some("entry".into()),
                ..Query::default()
            })
            .unwrap();
        assert_eq!(
            calls["items"].as_array().unwrap().len(),
            1,
            "SCIP must not add call edges"
        );
        let path = dir.path().join("lib.rs");
        let metadata = std::fs::metadata(&path).unwrap();
        std::fs::write(&path, index.documents[0].text.replace("helper", "worker")).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(metadata.modified().unwrap())
            .unwrap();
        let stale = engine.execute(&request("definition")).unwrap();
        assert_eq!(stale["excluded"], "stale_semantic_artifact");
        assert!(stale["items"].as_array().unwrap().is_empty());
    }

    #[test]
    fn manifest_must_bind_artifact_and_source_and_rejections_are_atomic() {
        let (dir, engine, mut index) = fixture();
        index.documents[0].text.clear();
        let path = artifact(&dir, &index);
        assert!(
            import(dir.path(), engine.store(), &path, None)
                .unwrap_err()
                .to_string()
                .contains("freshness")
        );
        let manifest = dir.path().join("manifest.json");
        let hashes = json!({"artifact_blake3":blake3::hash(&std::fs::read(&path).unwrap()).to_hex().to_string(),
            "files":{"lib.rs":blake3::hash(&std::fs::read(dir.path().join("lib.rs")).unwrap()).to_hex().to_string()}});
        std::fs::write(&manifest, hashes.to_string()).unwrap();
        import(dir.path(), engine.store(), &path, Some(&manifest)).unwrap();
        let generation = engine.store().generation().unwrap();
        index.documents[0].relative_path = "../escape.rs".into();
        assert!(import(dir.path(), engine.store(), &artifact(&dir, &index), None).is_err());
        assert_eq!(engine.store().generation().unwrap(), generation);
        assert_eq!(
            engine.execute(&request("definition")).unwrap()["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn malformed_versions_ranges_paths_and_allocation_bombs_are_rejected() {
        let (dir, engine, index) = fixture();
        let mut variants = Vec::new();
        let mut bad = index.clone();
        bad.metadata.as_mut().unwrap().version = 99;
        variants.push(bad);
        let mut bad = index.clone();
        bad.documents[0].occurrences[0].range = vec![0, 3, 999];
        variants.push(bad);
        let mut bad = index.clone();
        bad.documents[0].position_encoding = 0;
        variants.push(bad);
        let mut bad = index.clone();
        bad.documents[0].relative_path = "/outside.rs".into();
        variants.push(bad);
        let mut bad = index.clone();
        bad.documents.push(bad.documents[0].clone());
        variants.push(bad);
        std::os::unix::fs::symlink(dir.path().join("lib.rs"), dir.path().join("linked.rs"))
            .unwrap();
        let mut bad = index.clone();
        bad.documents[0].relative_path = "linked.rs".into();
        variants.push(bad);
        for bad in variants {
            assert!(import(dir.path(), engine.store(), &artifact(&dir, &bad), None).is_err());
        }
        assert!(preflight(&[0x12, 0xff]).is_err());
        let repeated = [0x12, 0x00].repeat(2001);
        assert!(
            preflight(&repeated)
                .unwrap_err()
                .to_string()
                .contains("budget")
        );
    }

    #[test]
    fn special_files_are_rejected_without_waiting_for_a_writer() {
        use nix::{sys::stat::Mode, unistd::mkfifo};
        let directory = tempfile::tempdir().unwrap();
        let fifo = directory.path().join("source.rs");
        mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        // Check on a thread with a deadline so a blocking-open
        // regression cannot hang the test harness indefinitely.
        let (send, receive) = std::sync::mpsc::channel();
        let root = directory.path().to_path_buf();
        std::thread::spawn(move || {
            let artifact = bounded_file(&fifo, 1024).unwrap_err().to_string();
            let source = crate::index::read_source(&root, &fifo, 1024)
                .unwrap_err()
                .to_string();
            send.send((artifact, source)).unwrap();
        });
        let (artifact, source) = receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(artifact.contains("regular file"));
        assert!(source.contains("regular file"));
    }

    #[test]
    fn typed_ranges_take_precedence_and_unicode_boundaries_are_validated() {
        let occurrence = wire::Occurrence {
            range: vec![-1],
            typed_range: Some(wire::occurrence::TypedRange::Single(
                wire::SingleLineRange {
                    line: 0,
                    start_character: 0,
                    end_character: 4,
                },
            )),
            ..Default::default()
        };
        assert_eq!(
            range(&occurrence, &["🚀"], 1, &mut 1024).unwrap(),
            [0, 0, 0, 4]
        );
        let mut bad = occurrence.clone();
        bad.typed_range = None;
        bad.range = vec![0, 0, 1];
        assert!(range(&bad, &["🚀"], 2, &mut 1024).is_err());
        bad.range = vec![0, 0, 2];
        assert!(range(&bad, &["🚀"], 2, &mut 1024).is_ok());
        let mut work_left = 18;
        assert!(range(&bad, &["🚀"], 2, &mut work_left).is_ok());
        assert!(
            range(&bad, &["🚀"], 2, &mut work_left)
                .unwrap_err()
                .to_string()
                .contains("validation budget")
        );
        assert_ne!(
            symbol_key("a.rs", "local 0").unwrap(),
            symbol_key("b.rs", "local 0").unwrap()
        );
    }
}
