//! Pinned TypeScript native LSP to local SCIP evidence. Definition navigation
//! is not dispatch proof; the separate combined-call validator decides that.
use super::{Profile, Snapshot, configured, write};
use crate::{model::RefKind, scip::wire};
use anyhow::{Context, Result, bail, ensure};
use prost::Message;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

const VERSION: &str = "7.0.2";

struct Client {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    sequence: u64,
    remaining: usize,
}

impl Drop for Client {
    fn drop(&mut self) {
        // Killing the owned timeout parent also triggers Bubblewrap's parent
        // death handling. Reap it on every protocol or validation error.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Client {
    fn start(mut command: Command) -> Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let input = child.stdin.take().context("producer stdin missing")?;
        let output = BufReader::new(child.stdout.take().context("producer stdout missing")?);
        Ok(Self {
            child,
            input: Some(input),
            output,
            sequence: 0,
            remaining: 32 * 1024 * 1024,
        })
    }

    fn send(&mut self, value: Value) -> Result<()> {
        let bytes = serde_json::to_vec(&value)?;
        let input = self.input.as_mut().context("producer input is closed")?;
        write!(input, "Content-Length: {}\r\n\r\n", bytes.len())?;
        input.write_all(&bytes)?;
        input.flush()?;
        Ok(())
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.sequence += 1;
        let id = self.sequence;
        let mut message = json!({"jsonrpc":"2.0","id":id,"method":method});
        if !params.is_null() {
            message["params"] = params;
        }
        self.send(message)?;
        for _ in 0..1000 {
            let response = receive(&mut self.output, &mut self.remaining)?;
            if response.get("method").is_some() {
                if let Some(id) = response.get("id") {
                    self.send(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"client request unsupported"}}))?;
                }
                continue;
            }
            ensure!(response["id"] == id, "unexpected producer response id");
            ensure!(
                response["error"].is_null(),
                "TypeScript {method} failed (protocol code {:?}; no diagnostics retained)",
                response["error"]["code"].as_i64()
            );
            return response
                .get("result")
                .cloned()
                .context("producer response missing result");
        }
        bail!("producer notification budget exceeded")
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let mut message = json!({"jsonrpc":"2.0","method":method});
        if !params.is_null() {
            message["params"] = params;
        }
        self.send(message)
    }
}

fn receive(output: &mut impl BufRead, remaining: &mut usize) -> Result<Value> {
    let mut length = None;
    let mut header_bytes = 0;
    loop {
        let mut line = String::new();
        let size = (&mut *output).take(4097).read_line(&mut line)?;
        header_bytes += size;
        ensure!(
            size > 0 && header_bytes <= 4096,
            "producer closed or exceeded protocol header budget"
        );
        if line == "\r\n" {
            break;
        }
        if let Some(n) = line.strip_prefix("Content-Length: ") {
            ensure!(length.is_none(), "duplicate producer content length");
            length = Some(
                n.trim()
                    .parse::<usize>()
                    .context("invalid producer content length")?,
            );
        }
    }
    let length = length.context("missing producer content length")?;
    ensure!(
        length <= 1024 * 1024 && length <= *remaining,
        "producer response budget exceeded"
    );
    *remaining -= length;
    let mut bytes = vec![0; length];
    output.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid producer JSON"))
}

fn uri(file: &str) -> String {
    let mut result = "file:///project/".to_owned();
    for byte in file.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-_.~".contains(&byte) {
            result.push(byte as char);
        } else {
            result.push_str(&format!("%{byte:02X}"));
        }
    }
    result
}

fn relative(uri: &str) -> Option<String> {
    let bytes = uri.strip_prefix("file:///project/")?.as_bytes();
    let mut decoded = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let pair = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            decoded.push(u8::from_str_radix(pair, 16).ok()?);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn coordinates(value: &Value) -> Result<[u32; 4]> {
    let mut range = [0; 4];
    for (index, (endpoint, unit)) in [
        ("start", "line"),
        ("start", "character"),
        ("end", "line"),
        ("end", "character"),
    ]
    .into_iter()
    .enumerate()
    {
        range[index] = value[endpoint][unit]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .context("invalid producer range")?;
    }
    Ok(range)
}

fn occurrence(range: [u32; 4], symbol: &str, definition: bool) -> wire::Occurrence {
    wire::Occurrence {
        range: range.into_iter().map(|n| n as i32).collect(),
        symbol: symbol.into(),
        symbol_roles: i32::from(definition),
        ..Default::default()
    }
}

pub fn typescript(root: &Path, state: &Path) -> Result<crate::scip::ImportStats> {
    let compiler = configured("AST_INDEX_SEMANTIC_TYPESCRIPT")?;
    let closure = configured("AST_INDEX_SEMANTIC_TYPESCRIPT_CLOSURE")?;
    let snapshot = Snapshot::new(root, state, Profile::TypeScript)?;
    // No emit, build mode, plugins, package scripts or installer. Missing
    // dependencies and invalid configuration fail before producing evidence.
    let mut check = snapshot
        .command(&closure, &[])?
        .arg(&compiler)
        .args([
            "--project",
            "/project/tsconfig.json",
            "--noEmit",
            "--incremental",
            "false",
            "--listFiles",
        ])
        .stdout(Stdio::piped())
        .spawn()?;
    let mut listing = Vec::new();
    let captured = check
        .stdout
        .take()
        .context("compiler stdout missing")?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut listing);
    if captured.is_err() || listing.len() > 16 * 1024 * 1024 {
        let _ = check.kill();
        let _ = check.wait();
        bail!("TypeScript file-list output failed or exceeded its budget");
    }
    let checked = check.wait()?;
    ensure!(
        checked.success(),
        "offline TypeScript check failed; source/dependencies/configuration unavailable (no diagnostics retained)"
    );
    let listing = std::str::from_utf8(&listing).context("invalid TypeScript file list")?;
    let included: BTreeSet<_> = listing
        .lines()
        .filter_map(|line| line.strip_prefix("/project/"))
        .filter(|file| snapshot.files.contains_key(*file))
        .collect();
    ensure!(
        !included.is_empty(),
        "TypeScript configuration selected no snapshot sources"
    );
    let mut command = snapshot.command(&closure, &[])?;
    command.arg(&compiler).args(["--lsp", "--stdio"]);
    let mut client = Client::start(command)?;
    let initialized = client.request("initialize",json!({
        "processId":null,"rootUri":"file:///project","capabilities":{"general":{"positionEncodings":["utf-8"]}},
        "initializationOptions":{"enableTelemetry":false,"disablePushDiagnostics":true,"userPreferences":{"tsserver":{"automaticTypeAcquisition":{"enabled":false}}}}
    }))?;
    ensure!(
        initialized["serverInfo"]["version"] == VERSION
            && initialized["capabilities"]["positionEncoding"] == "utf-8",
        "unsupported TypeScript producer version/position encoding"
    );
    client.notify("initialized", json!({}))?;
    let mut documents = BTreeMap::new();
    let mut line_offsets = BTreeMap::new();
    let mut sites = Vec::new();
    for (file, bytes) in &snapshot.files {
        if !included.contains(file.as_str()) {
            continue;
        }
        let Some(spec) =
            crate::lang::detect(Path::new(file)).filter(|s| matches!(s.id, "typescript" | "tsx"))
        else {
            continue;
        };
        let text = std::str::from_utf8(bytes).context("TypeScript source must be UTF-8")?;
        let offsets: Vec<u32> = std::iter::once(0)
            .chain(
                text.bytes()
                    .enumerate()
                    .filter(|(_, byte)| *byte == b'\n')
                    .map(|(index, _)| index as u32 + 1),
            )
            .collect();
        line_offsets.insert(file.clone(), offsets);
        let parsed = crate::parse::parse(text, spec)?;
        ensure!(!parsed.has_errors, "malformed TypeScript source");
        client.notify("textDocument/didOpen",json!({"textDocument":{"uri":uri(file),"languageId":"typescript","version":1,"text":text}}))?;
        for reference in parsed.refs.into_iter().filter(|r| r.kind == RefKind::Call) {
            ensure!(sites.len() < 10_000, "TypeScript call-site budget exceeded");
            sites.push((file.clone(), reference.range));
        }
        documents.insert(
            file.clone(),
            wire::Document {
                relative_path: file.clone(),
                language: spec.id.into(),
                position_encoding: 1,
                ..Default::default()
            },
        );
    }
    let mut definitions = BTreeSet::new();
    let mut names = BTreeMap::new();
    for (file, range) in sites {
        let result = client.request("textDocument/definition",json!({"textDocument":{"uri":uri(&file)},"position":{"line":range[0],"character":range[1]}}))?;
        let targets = match result {
            Value::Null => Vec::new(),
            Value::Array(items) => items,
            Value::Object(_) => vec![result],
            _ => bail!("invalid TypeScript definition response"),
        };
        ensure!(
            targets.len() <= 16,
            "TypeScript definition candidate budget exceeded"
        );
        for target in targets {
            let link = target.get("targetUri").is_some();
            let Some(target_file) = target[if link { "targetUri" } else { "uri" }]
                .as_str()
                .and_then(relative)
            else {
                continue;
            };
            if !documents.contains_key(&target_file) {
                continue;
            }
            let target_range = coordinates(
                &target[if link {
                    "targetSelectionRange"
                } else {
                    "range"
                }],
            )?;
            let identity = format!(
                "typescript-lsp {}",
                blake3::hash(serde_json::to_string(&(&target_file, target_range))?.as_bytes())
                    .to_hex()
            );
            if definitions.insert((target_file.clone(), target_range)) {
                // Names make the existing semantic name selector useful. They
                // are bounded source tokens, not snippets or producer hover text.
                if target_range[0] == target_range[2] {
                    let offsets = &line_offsets[&target_file];
                    let text = std::str::from_utf8(&snapshot.files[&target_file])?;
                    if let Some(start) = offsets.get(target_range[0] as usize) {
                        let end = offsets
                            .get(target_range[0] as usize + 1)
                            .map_or(text.len(), |n| *n as usize - 1);
                        if let Some(name) = text
                            .get(*start as usize..end)
                            .and_then(|line| {
                                line.get(target_range[1] as usize..target_range[3] as usize)
                            })
                            .filter(|name| {
                                !name.is_empty()
                                    && name.len() <= 512
                                    && name
                                        .chars()
                                        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '$'))
                            })
                        {
                            names.insert(identity.clone(), name.to_owned());
                        }
                    }
                }
                documents
                    .get_mut(&target_file)
                    .unwrap()
                    .occurrences
                    .push(occurrence(target_range, &identity, true));
            }
            documents
                .get_mut(&file)
                .unwrap()
                .occurrences
                .push(occurrence(range, &identity, false));
        }
    }
    for document in documents.values_mut() {
        for identity in document
            .occurrences
            .iter()
            .map(|o| &o.symbol)
            .collect::<BTreeSet<_>>()
        {
            if let Some(name) = names.get(identity) {
                document.symbols.push(wire::SymbolInformation {
                    symbol: identity.clone(),
                    display_name: name.clone(),
                    ..Default::default()
                });
            }
        }
    }
    client.request("shutdown", Value::Null)?;
    // TypeScript 7.0.2's exit notification cancels its dispatch context and
    // returns status 1. After acknowledged shutdown, EOF takes its successful
    // read-loop exit path. Keep requiring a successful, reaped process.
    drop(client.input.take());
    let status = client.child.wait()?;
    ensure!(
        status.success(),
        "TypeScript producer did not shut down successfully: {status}"
    );
    let artifact = wire::Index {
        metadata: Some(wire::Metadata {
            version: 0,
            text_document_encoding: 1,
            project_root: "file:///project".into(),
            tool_info: Some(wire::ToolInfo {
                name: "typescript-lsp".into(),
                version: VERSION.into(),
            }),
        }),
        documents: documents.into_values().collect(),
    }
    .encode_to_vec();
    ensure!(
        artifact.len() <= 4 * 1024 * 1024,
        "TypeScript artifact budget exceeded"
    );
    write(&snapshot.output.join("index.scip"), &artifact)?;
    snapshot.import()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol_frames_reject_allocation_bombs_truncation_and_duplicate_lengths() {
        for input in [
            b"Content-Length: 1048577\r\n\r\n".as_slice(),
            b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}",
            b"Content-Length: 5\r\n\r\n{}",
            b"Content-Length: 2\r\n\r\n!!",
            b"\r\n",
        ] {
            assert!(receive(&mut std::io::Cursor::new(input), &mut (4 * 1024 * 1024)).is_err());
        }
        assert!(receive(&mut std::io::Cursor::new(vec![b'x'; 4097]), &mut 4096).is_err());
        let mut input =
            std::io::Cursor::new(b"Content-Length: 2\r\n\r\n{}Content-Length: 2\r\n\r\n{}");
        let mut remaining = 3;
        assert_eq!(receive(&mut input, &mut remaining).unwrap(), json!({}));
        assert!(receive(&mut input, &mut remaining).is_err());
    }
    #[test]
    fn local_uris_round_trip_unicode_without_following_foreign_roots() {
        let file = "src/café #%.ts";
        assert_eq!(relative(&uri(file)).as_deref(), Some(file));
        assert!(relative("file:///elsewhere/code.ts").is_none());
        assert!(relative("file:///project/%ZZ").is_none());
        assert!(
            coordinates(&json!({"start":{"line":-1,"character":0},"end":{"line":1,"character":0}}))
                .is_err()
        );
    }
}
