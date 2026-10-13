//! End-to-end tests: index a synthetic project, query it through the CLI, the
//! stdio MCP adapter and the Unix socket service.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

/// Create a synthetic multi-language project.
fn fixture() -> TempDir {
    let directory = tempfile::tempdir().expect("tempdir");
    let root = directory.path();
    std::fs::create_dir_all(root.join("src")).expect("src");
    std::fs::create_dir_all(root.join("tests")).expect("tests");

    std::fs::write(
        root.join("src/lib.rs"),
        r#"
pub struct Store {
    items: Vec<u32>,
}

impl Store {
    pub fn insert(&mut self, value: u32) {
        self.record(value);
    }

    fn record(&mut self, value: u32) {
        self.items.push(value);
    }
}

pub fn helper() -> u32 {
    1
}

pub fn run() -> u32 {
    helper()
}
"#,
    )
    .expect("lib.rs");

    std::fs::write(
        root.join("src/other.rs"),
        r#"
pub fn entry() -> u32 {
    crate::run()
}
"#,
    )
    .expect("other.rs");

    std::fs::write(
        root.join("tests/main.py"),
        r#"
def py_helper():
    return 1


def py_main():
    value = py_helper()
    return value
"#,
    )
    .expect("main.py");

    directory
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ast-index"))
}

fn run_cli(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(binary())
        .arg("--root")
        .arg(root)
        .args(args)
        .output()
        .expect("run ast-index")
}

fn run_cli_json(root: &Path, args: &[&str]) -> Value {
    let output = run_cli(root, args);
    assert!(
        output.status.success(),
        "ast-index {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("ast-index {args:?} did not emit JSON: {error}"));
    // Existing behavior assertions inspect the first bounded page; dedicated
    // pagination tests below inspect the complete response contract.
    value.get("items").cloned().unwrap_or(value)
}

#[test]
fn same_named_nested_functions_keep_distinct_callers() {
    let directory = tempfile::tempdir().expect("tempdir");
    let root = directory.path();
    std::fs::write(
        root.join("lib.rs"),
        r#"
pub fn helper() {}
mod left { pub fn run() { crate::helper(); } }
mod right { pub fn run() { crate::helper(); } }
"#,
    )
    .expect("lib.rs");
    run_cli_json(root, &["index", "--json"]);
    let callers = run_cli_json(root, &["callers", "helper", "--json"]);
    let names: Vec<_> = callers
        .as_array()
        .expect("callers")
        .iter()
        .filter_map(|edge| edge["caller"].as_str())
        .collect();
    assert_eq!(names, ["left::run", "right::run"]);
}

#[test]
fn nix_ambiguous_qualified_targets_stay_unresolved() {
    let directory = tempfile::tempdir().expect("tempdir");
    let root = directory.path();
    std::fs::write(
        root.join("ambiguous.nix"),
        r#"
let helper = x: x;
in [ (let helper = x: x + 1; in helper 1) (helper 2) ]
"#,
    )
    .expect("ambiguous.nix");
    run_cli_json(root, &["index", "--json"]);
    let refs = run_cli_json(root, &["refs", "ambiguous.nix", "--json"]);
    let refs = refs.as_array().expect("refs");
    assert_eq!(refs.len(), 2);
    assert!(refs.iter().all(|r| r["resolved"].is_null()));
}

#[test]
fn nix_index_answers_queries_without_guessing_dynamic_or_cross_file_targets() {
    let directory = tempfile::tempdir().expect("tempdir");
    let root = directory.path();
    std::fs::write(
        root.join("flake.nix"),
        r#"
let
  nix_helper = x: y: x + y;
in {
  left = { run = x: nix_helper x 1; };
  right = { run = x: nix_helper x 2; };
  shadow = nix_helper: nix_helper 1;
  selected = x: lib.nix_helper x;
  external = x: remote x;
  imports = [ ./module.nix ];
}
"#,
    )
    .expect("flake.nix");
    std::fs::write(root.join("module.nix"), "{ remote = x: x; }").expect("module.nix");
    std::fs::write(root.join("lib.rs"), "pub fn rust_entry() { nix_helper(); }").expect("lib.rs");

    let stats = run_cli_json(root, &["index", "--json"]);
    assert_eq!(stats["files_indexed"], 3);
    assert_eq!(stats["files_failed"], 0);
    let status = run_cli_json(root, &["status", "--json"]);
    let nix = status["languages"]
        .as_array()
        .expect("languages")
        .iter()
        .find(|language| language["language"] == "nix")
        .expect("Nix coverage");
    assert_eq!(nix["files"], 2);
    let outline = run_cli_json(root, &["outline", "flake.nix", "--json"]);
    assert!(
        outline
            .as_array()
            .expect("outline")
            .iter()
            .any(|s| s["qualified"] == "right::run")
    );
    let callers = run_cli_json(root, &["callers", "nix_helper", "--json"]);
    let names: Vec<_> = callers
        .as_array()
        .expect("callers")
        .iter()
        .filter_map(|edge| edge["caller"].as_str())
        .collect();
    assert_eq!(names, ["left::run", "right::run"]);
    assert!(
        callers
            .as_array()
            .expect("callers")
            .iter()
            .all(|edge| edge["confidence"] == "same_file")
    );
    let callees = run_cli_json(root, &["callees", "right::run", "--json"]);
    assert_eq!(callees[0]["callee"], "nix_helper");
    let impact = run_cli_json(root, &["impact", "nix_helper", "--json"]);
    assert_eq!(impact.as_array().expect("impact").len(), 2);
    for name in ["shadow", "selected", "external", "rust_entry"] {
        let callees = run_cli_json(root, &["callees", name, "--json"]);
        assert!(
            callees.as_array().expect("callees").is_empty(),
            "guessed target for {name}"
        );
    }
    let references = run_cli_json(root, &["refs", "flake.nix", "--json"]);
    assert!(
        references
            .as_array()
            .expect("refs")
            .iter()
            .any(|r| r["kind"] == "import" && r["name"] == "./module.nix")
    );

    // Nix extraction uses the same stored outline through MCP.
    let mut child = Command::new(binary())
        .arg("--root")
        .arg(root)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mcp");
    send(
        &mut child,
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"code_explore","arguments":{"action":"outline","file":"flake.nix"}}}"#,
    );
    let mut reader = BufReader::new(child.stdout.take().expect("stdout"));
    let response: Value = serde_json::from_str(&read_line(&mut reader)).expect("MCP response");
    assert_eq!(response["result"]["isError"], false);
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .expect("outline text")
            .contains("right::run")
    );
    drop(child.stdin.take());
    assert!(child.wait().expect("MCP exit").success());

    // The new language participates in the existing incremental update/removal path.
    let unchanged = run_cli_json(root, &["index", "--json"]);
    assert_eq!(unchanged["files_unchanged"], 3);
    std::fs::write(root.join("module.nix"), "{ renamed = x: x; }").expect("change Nix source");
    let changed = run_cli_json(root, &["index", "--json"]);
    assert_eq!(changed["files_indexed"], 1);
    let found = run_cli_json(root, &["search", "renamed", "--json"]);
    assert_eq!(found.as_array().expect("search").len(), 1);
    std::fs::remove_file(root.join("module.nix")).expect("remove Nix source");
    let removed = run_cli_json(root, &["index", "--json"]);
    assert_eq!(removed["files_removed"], 1);
}

#[test]
fn indexes_and_answers_structural_queries() {
    let fixture = fixture();
    let root = fixture.path();

    // Index.
    let stats = run_cli_json(root, &["index", "--json"]);
    assert_eq!(stats["files_indexed"], Value::from(3));
    assert_eq!(stats["files_failed"], Value::from(0));
    assert!(stats["symbols"].as_u64().unwrap_or(0) >= 8);

    // Status.
    let status = run_cli_json(root, &["status", "--json"]);
    assert_eq!(status["indexed"], Value::from(true));
    assert_eq!(status["files"], Value::from(3));

    // Outline of a file.
    let outline = run_cli_json(root, &["outline", "src/lib.rs", "--json"]);
    let names: Vec<&str> = outline
        .as_array()
        .expect("outline array")
        .iter()
        .filter_map(|symbol| symbol["name"].as_str())
        .collect();
    assert!(names.contains(&"Store"), "outline: {names:?}");
    assert!(names.contains(&"insert"), "outline: {names:?}");
    assert!(names.contains(&"helper"), "outline: {names:?}");

    // A method carries its parent and qualified name.
    let insert = outline
        .as_array()
        .expect("array")
        .iter()
        .find(|symbol| symbol["name"] == "insert")
        .expect("insert");
    assert_eq!(insert["kind"], Value::from("method"));
    assert_eq!(insert["qualified"], Value::from("Store::insert"));

    // Search.
    let search = run_cli_json(root, &["search", "helper", "--json"]);
    let hits: Vec<&str> = search
        .as_array()
        .expect("search array")
        .iter()
        .filter_map(|symbol| symbol["qualified"].as_str())
        .collect();
    assert!(hits.contains(&"helper"), "search: {hits:?}");
    assert!(hits.len() >= 2, "helpers in Rust and Python: {hits:?}");

    // Callers of helper in Rust: run.
    let callers = run_cli_json(root, &["callers", "helper", "--json"]);
    let caller_names: Vec<&str> = callers
        .as_array()
        .expect("callers array")
        .iter()
        .filter_map(|edge| edge["caller"].as_str())
        .collect();
    assert!(caller_names.contains(&"run"), "callers: {caller_names:?}");

    // The Python helper resolves in its own file.
    let python_callers = run_cli_json(root, &["callers", "py_helper", "--json"]);
    let python_names: Vec<&str> = python_callers
        .as_array()
        .expect("callers array")
        .iter()
        .filter_map(|edge| edge["caller"].as_str())
        .collect();
    assert!(
        python_names.contains(&"py_main"),
        "python callers: {python_names:?}"
    );

    // Cross-file call run -> entry resolves with high confidence (same directory).
    let entry_callers = run_cli_json(root, &["callers", "run", "--json"]);
    let entry_edge = entry_callers
        .as_array()
        .expect("array")
        .iter()
        .find(|edge| edge["caller"] == "entry")
        .expect("entry calls run");
    assert_eq!(entry_edge["confidence"], Value::from("same_directory"));

    // Transitive impact: helper <- run <- entry.
    let impact = run_cli_json(root, &["impact", "helper", "--depth", "3", "--json"]);
    let impacted: Vec<&str> = impact
        .as_array()
        .expect("impact array")
        .iter()
        .filter_map(|row| row["symbol"]["qualified"].as_str())
        .collect();
    assert!(impacted.contains(&"run"), "impact: {impacted:?}");
    assert!(impacted.contains(&"entry"), "impact: {impacted:?}");
}

#[test]
fn reindexing_is_incremental_and_removes_deleted_files() {
    let fixture = fixture();
    let root = fixture.path();
    run_cli(root, &["index"]);

    // Second pass without changes parses nothing.
    let stats = run_cli_json(root, &["index", "--json"]);
    assert_eq!(stats["files_indexed"], Value::from(0));
    assert_eq!(stats["files_unchanged"], Value::from(3));

    // A new file is picked up.
    std::fs::write(root.join("src/extra.rs"), "pub fn extra() {}\n").expect("extra.rs");
    let stats = run_cli_json(root, &["index", "--json"]);
    assert_eq!(stats["files_indexed"], Value::from(1));

    // Deleting it removes it from the index.
    std::fs::remove_file(root.join("src/extra.rs")).expect("remove");
    let stats = run_cli_json(root, &["index", "--json"]);
    assert_eq!(stats["files_removed"], Value::from(1));
    let search = run_cli_json(root, &["search", "extra", "--json"]);
    assert_eq!(search.as_array().expect("array").len(), 0);

    // An mtime-only change with identical content is not re-parsed.
    let path = root.join("src/other.rs");
    let metadata = std::fs::metadata(&path).expect("metadata");
    let modified = metadata.modified().expect("mtime") + Duration::from_secs(5);
    let file = std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("open");
    file.set_modified(modified).expect("set mtime");
    drop(file);
    let stats = run_cli_json(root, &["index", "--json"]);
    assert_eq!(stats["files_indexed"], Value::from(0));
    assert_eq!(stats["files_unchanged"], Value::from(3));
}

#[test]
fn reindexing_detects_content_changes_with_unchanged_size_and_mtime() {
    let fixture = fixture();
    let root = fixture.path();
    let path = root.join("src/other.rs");
    run_cli(root, &["index"]);

    let original = std::fs::metadata(&path).expect("original metadata");
    let content = std::fs::read_to_string(&path).expect("original content");
    std::fs::write(&path, content.replace("entry", "begin")).expect("changed content");
    let file = std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("open changed file");
    file.set_modified(original.modified().expect("original mtime"))
        .expect("restore mtime");
    drop(file);
    let changed = std::fs::metadata(&path).expect("changed metadata");
    assert_eq!(changed.len(), original.len());
    assert_eq!(
        changed.modified().expect("changed mtime"),
        original.modified().expect("original mtime")
    );

    let stats = run_cli_json(root, &["index", "--json"]);
    assert_eq!(stats["files_indexed"], Value::from(1));
    assert!(
        run_cli_json(root, &["search", "entry", "--json"])
            .as_array()
            .expect("entries")
            .is_empty()
    );
    assert_eq!(
        run_cli_json(root, &["search", "begin", "--json"])
            .as_array()
            .expect("begins")
            .len(),
        1
    );
}

#[test]
fn ambiguous_names_stay_unresolved() {
    let fixture = fixture();
    let root = fixture.path();
    // Two definitions with the same name in one file: a reference cannot be
    // linked to exactly one target, so it must stay unresolved.
    std::fs::write(
        root.join("src/ambiguous.rs"),
        r#"
pub struct A;
pub struct B;

impl A {
    pub fn shared(&self) {}
}

impl B {
    pub fn shared(&self) {}
}

pub fn call() {
    let a = A;
    a.shared();
}
"#,
    )
    .expect("ambiguous.rs");
    run_cli(root, &["index"]);

    let callers = run_cli_json(root, &["callers", "A::shared", "--json"]);
    assert!(
        callers.as_array().expect("array").is_empty(),
        "ambiguous callers must stay unresolved: {callers}"
    );
    // A bare ambiguous name is rejected instead of guessed.
    let ambiguous = run_cli(root, &["callers", "shared"]);
    assert!(!ambiguous.status.success());
    assert!(String::from_utf8_lossy(&ambiguous.stderr).contains("ambiguous"));

    // The reference itself is still recorded, just unresolved.
    let references = run_cli_json(root, &["refs", "src/ambiguous.rs", "--json"]);
    let shared = references
        .as_array()
        .expect("array")
        .iter()
        .find(|reference| reference["name"] == "shared")
        .expect("shared reference");
    assert_eq!(shared["resolved"], Value::Null);
    assert_eq!(shared["confidence"], Value::from("none"));
}

#[test]
fn non_utf8_sources_are_indexed_lossily() {
    let fixture = fixture();
    let root = fixture.path();
    // A latin-1 byte in a comment must not drop the whole file.
    let bytes = b"// caf\xe9 comment\npub fn latin1_symbol() {}\n";
    std::fs::write(root.join("src/latin.rs"), bytes).expect("write");
    let stats = run_cli_json(root, &["index", "--json"]);
    assert_eq!(stats["files_failed"], Value::from(0));

    let search = run_cli_json(root, &["search", "latin1_symbol", "--json"]);
    assert_eq!(search.as_array().expect("array").len(), 1);
}

#[test]
fn exclude_patterns_skip_subtrees() {
    let fixture = fixture();
    let root = fixture.path();
    std::fs::create_dir_all(root.join("secrets")).expect("secrets");
    std::fs::write(
        root.join("secrets/private.rs"),
        "pub fn private_thing() {}\n",
    )
    .expect("write");

    let stats = run_cli_json(root, &["index", "--exclude", "secrets/", "--json"]);
    assert_eq!(stats["files_indexed"], Value::from(3));
    let search = run_cli_json(root, &["search", "private_thing", "--json"]);
    assert_eq!(search.as_array().expect("array").len(), 0);

    // Without the exclusion the file is part of the index.
    run_cli_json(root, &["index", "--force", "--json"]);
    let search = run_cli_json(root, &["search", "private_thing", "--json"]);
    assert_eq!(search.as_array().expect("array").len(), 1);
}

#[test]
fn reports_not_indexed_with_exit_code_two() {
    let fixture = fixture();
    let output = run_cli(fixture.path(), &["search", "helper"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ast-index index"), "stderr: {stderr}");
}

fn read_line(reader: &mut impl BufRead) -> String {
    let mut line = String::new();
    reader.read_line(&mut line).expect("read line");
    line
}

fn send(child: &mut Child, message: &str) {
    let stdin = child.stdin.as_mut().expect("stdin");
    writeln!(stdin, "{message}").expect("write");
    stdin.flush().expect("flush");
}

#[test]
fn mcp_stdio_adapter_serves_the_single_tool() {
    let fixture = fixture();
    let root = fixture.path();
    run_cli(root, &["index"]);

    let mut child = Command::new(binary())
        .arg("--root")
        .arg(root)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mcp");

    send(
        &mut child,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#,
    );
    send(
        &mut child,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
    );
    send(
        &mut child,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"code_explore","arguments":{"action":"describe","query":"run"}}}"#,
    );

    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);
    let initialize: Value = serde_json::from_str(&read_line(&mut reader)).expect("initialize");
    assert_eq!(
        initialize["result"]["protocolVersion"],
        Value::from("2025-11-25")
    );

    let tools: Value = serde_json::from_str(&read_line(&mut reader)).expect("tools");
    assert_eq!(tools["result"]["tools"].as_array().expect("tools").len(), 1);

    let call: Value = serde_json::from_str(&read_line(&mut reader)).expect("call");
    assert_eq!(call["result"]["isError"], Value::from(false));
    let text = call["result"]["content"][0]["text"].as_str().expect("text");
    assert!(text.contains("callers"), "text: {text}");
    assert!(text.contains("entry"), "text: {text}");

    drop(child.stdin.take());
    let _ = child.wait();
}

#[test]
fn socket_service_and_stdio_proxy_share_one_index() {
    let fixture = fixture();
    let root = fixture.path();
    run_cli(root, &["index"]);

    let socket = root.join("ast-index.sock");
    let mut service = Command::new(binary())
        .arg("--root")
        .arg(root)
        .arg("serve")
        .arg("--socket")
        .arg(&socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn serve");

    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(Instant::now() < deadline, "socket was never created");
        std::thread::sleep(Duration::from_millis(50));
    }

    // The proxy is the harness-facing process and speaks MCP on stdio.
    let mut child = Command::new(binary())
        .arg("--root")
        .arg(root)
        .arg("mcp")
        .arg("--socket")
        .arg(&socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn proxy");
    send(
        &mut child,
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"code_explore","arguments":{"action":"status"}}}"#,
    );
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);
    let response: Value = serde_json::from_str(&read_line(&mut reader)).expect("response");
    assert_eq!(response["result"]["isError"], Value::from(false));
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(text.contains("files: 3"), "status text: {text}");

    let input = child.stdin.as_mut().expect("proxy stdin");
    input
        .write_all(&vec![b'x'; 1_048_577])
        .expect("write oversized socket frame");
    input
        .write_all(b"\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n")
        .expect("write socket ping");
    input.flush().expect("flush proxy stdin");
    let rejected: Value = serde_json::from_str(&read_line(&mut reader)).expect("rejection");
    assert_eq!(rejected["error"]["code"], -32600);
    let ping: Value = serde_json::from_str(&read_line(&mut reader)).expect("ping");
    assert_eq!(ping["id"], 2);
    assert_eq!(ping["result"], serde_json::json!({}));

    drop(child.stdin.take());
    let _ = child.wait();
    let _ = service.kill();
    let _ = service.wait();
}

#[test]
fn socket_proxy_preserves_crlf_bytes() {
    let directory = tempfile::tempdir().expect("tempdir");
    let socket = directory.path().join("echo.sock");
    let listener = UnixListener::bind(&socket).expect("bind echo socket");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept proxy");
        let mut received = Vec::new();
        stream.read_to_end(&mut received).expect("read proxy bytes");
        stream.write_all(&received).expect("echo proxy bytes");
        received
    });

    let mut child = Command::new(binary())
        .arg("mcp")
        .arg("--socket")
        .arg(&socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn proxy");
    let payload = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\r\n";
    child
        .stdin
        .take()
        .expect("proxy stdin")
        .write_all(payload)
        .expect("write proxy bytes");
    let output = child.wait_with_output().expect("wait for proxy");
    assert!(output.status.success(), "proxy failed: {output:?}");
    assert_eq!(server.join().expect("echo server"), payload);
    assert_eq!(output.stdout, payload);
}

#[test]
fn mcp_stdio_rejects_oversized_frame_and_serves_the_next_request() {
    let directory = tempfile::tempdir().expect("tempdir");
    let mut child = Command::new(binary())
        .arg("--root")
        .arg(directory.path())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn stdio server");
    let mut input = child.stdin.take().expect("server stdin");
    input
        .write_all(&vec![b'x'; 1_048_577])
        .expect("write oversized frame");
    input
        .write_all(b"\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n")
        .expect("write next request");
    drop(input);

    let output = child.wait_with_output().expect("wait for server");
    assert!(output.status.success(), "server failed: {output:?}");
    let responses: Vec<Value> = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).expect("JSON-RPC response"))
        .collect();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["error"]["code"], -32600);
    assert_eq!(responses[0]["id"], Value::Null);
    assert_eq!(responses[1]["id"], 2);
    assert_eq!(responses[1]["result"], serde_json::json!({}));
}

#[test]
fn resolution_respects_language_projects_and_shadowing() {
    let directory = tempfile::tempdir().expect("tempdir");
    let root = directory.path();
    std::fs::write(root.join("main.rs"), "fn entry() { helper(); }").unwrap();
    std::fs::write(
        root.join("module.py"),
        "def helper():\n    pass\ndef shadow(helper):\n    helper()\n",
    )
    .unwrap();
    run_cli_json(root, &["index", "--json"]);
    for name in ["entry", "shadow"] {
        assert!(
            run_cli_json(root, &["callees", name, "--json"])
                .as_array()
                .unwrap()
                .is_empty(),
            "incorrect edge from {name}"
        );
    }
    for dir in ["one", "two"] {
        std::fs::create_dir(root.join(dir)).unwrap();
        std::fs::write(
            root.join(dir).join("Cargo.toml"),
            "[package]\nname = 'fixture'\n",
        )
        .unwrap();
    }
    std::fs::write(
        root.join("one/lib.rs"),
        "fn local_entry() { crate::foreign(); }",
    )
    .unwrap();
    std::fs::write(root.join("two/lib.rs"), "fn foreign() {}").unwrap();
    run_cli_json(root, &["index", "--json"]);
    assert!(
        run_cli_json(root, &["callees", "local_entry", "--json"])
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn impact_cycles_are_unique_and_exclude_the_seed() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(
        root.join("lib.rs"),
        "fn alpha() { beta(); }\nfn beta() { alpha(); }\nfn gamma() { alpha(); beta(); }",
    )
    .unwrap();
    run_cli_json(root, &["index", "--json"]);
    let rows = run_cli_json(root, &["impact", "alpha", "--depth", "4", "--json"]);
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows.iter()
            .all(|r| r["depth"] == 1 && r["symbol"]["name"] != "alpha")
    );
}

#[test]
fn adding_a_candidate_invalidates_existing_resolution() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(
        root.join("lib.rs"),
        "fn helper() {}\nfn entry() { helper(); }",
    )
    .unwrap();
    run_cli_json(root, &["index", "--json"]);
    assert_eq!(
        run_cli_json(root, &["callees", "entry", "--json"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    std::fs::write(
        root.join("lib.rs"),
        "fn helper() {}\nfn helper() {}\nfn entry() { helper(); }",
    )
    .unwrap();
    run_cli_json(root, &["index", "--json"]);
    assert!(
        run_cli_json(root, &["callees", "entry", "--json"])
            .as_array()
            .unwrap()
            .is_empty()
    );
}

fn raw_json(root: &Path, args: &[&str]) -> Value {
    let output = run_cli(root, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn mcp_json(root: &Path, arguments: Value) -> Value {
    let mut child = Command::new(binary())
        .arg("--root")
        .arg(root)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let message = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"code_explore","arguments":arguments}});
    writeln!(child.stdin.take().unwrap(), "{message}").unwrap();
    let output = child.wait_with_output().unwrap();
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["result"]["isError"], false, "{response}");
    serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[test]
fn selectors_pagination_and_frontend_contracts() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(
        root.join("one.rs"),
        "fn same() {}\nfn caller() { same(); }\nfn a_b() {}\nfn axb() {}",
    )
    .unwrap();
    std::fs::write(root.join("two.rs"), "fn same() {}\nfn same() {}").unwrap();
    run_cli_json(root, &["index", "--json"]);
    let ambiguous = run_cli(root, &["describe", "same", "--json"]);
    assert!(!ambiguous.status.success());
    let error: Value = serde_json::from_slice(&ambiguous.stdout).unwrap();
    assert_eq!(error["error"], "ambiguous_symbol");
    assert_eq!(error["candidates"].as_array().unwrap().len(), 3);
    for (file, line) in [("one.rs", "1"), ("two.rs", "2")] {
        let cli = raw_json(
            root,
            &["describe", "same", "--file", file, "--line", line, "--json"],
        );
        let mcp = mcp_json(
            root,
            serde_json::json!({"action":"describe","query":"same","file":file,"line":line.parse::<u32>().unwrap(),"format":"json"}),
        );
        assert_eq!(cli, mcp);
        assert_eq!(cli["symbol"]["file"], file);
        assert!(cli["callers"]["items"].is_array());
    }
    let page = raw_json(root, &["search", "same", "--limit", "1", "--json"]);
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["truncated"], true);
    let cursor = page["next_cursor"].as_str().unwrap();
    let next = raw_json(
        root,
        &[
            "search", "same", "--limit", "1", "--cursor", cursor, "--json",
        ],
    );
    assert_ne!(page["items"][0]["id"], next["items"][0]["id"]);
    assert!(
        !run_cli(
            root,
            &[
                "search", "caller", "--limit", "1", "--cursor", cursor, "--json"
            ]
        )
        .status
        .success()
    );
    let literal = raw_json(root, &["search", "a_b", "--json"]);
    assert_eq!(
        literal["items"].as_array().unwrap().len(),
        1,
        "underscore is literal"
    );
    run_cli_json(root, &["index", "--json"]);
    let stale = run_cli(
        root,
        &[
            "search", "same", "--limit", "1", "--cursor", cursor, "--json",
        ],
    );
    assert!(!stale.status.success());
    assert!(String::from_utf8_lossy(&stale.stderr).contains("stale cursor"));
    assert!(
        !run_cli(root, &["outline", "../outside.rs", "--json"])
            .status
            .success()
    );
}

#[test]
fn freshness_checks_bytes_and_reports_incomplete_coverage() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let file = root.join("lib.rs");
    std::fs::write(&file, "fn alpha() {}").unwrap();
    run_cli_json(root, &["index", "--json"]);
    assert_eq!(
        raw_json(root, &["status", "--json"])["freshness"],
        "unknown"
    );
    assert_eq!(
        raw_json(root, &["freshness", "--json"])["state"],
        "fresh_at_check"
    );
    assert_eq!(
        raw_json(root, &["status", "--json"])["freshness"],
        "unknown"
    );
    let before = std::fs::metadata(&file).unwrap();
    std::fs::write(&file, "fn bravo() {}").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(before.modified().unwrap())
        .unwrap();
    assert_eq!(raw_json(root, &["freshness", "--json"])["state"], "stale");
    assert_eq!(raw_json(root, &["status", "--json"])["freshness"], "stale");
    std::fs::write(&file, "fn broken( { ").unwrap();
    run_cli_json(root, &["index", "--json"]);
    let status = raw_json(root, &["status", "--json"]);
    assert_eq!(status["freshness"], "incomplete");
    assert_eq!(status["health"]["parse_error_files"], 1);
    let changes = raw_json(
        root,
        &[
            "changes",
            r#"[{"file":"lib.rs","start_line":1,"end_line":1}]"#,
            "--json",
        ],
    );
    assert_eq!(changes["current_coverage_complete"], false);
}

#[test]
fn writer_lock_blocks_indexing_but_allows_queries_and_canonical_aliases() {
    use nix::fcntl::{Flock, FlockArg};
    let fixture = fixture();
    let root = fixture.path();
    run_cli_json(root, &["index", "--json"]);
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(".ast-index/index.writer.lock"))
        .unwrap();
    let guard = Flock::lock(lock, FlockArg::LockExclusiveNonblock).unwrap();
    assert!(run_cli(root, &["status", "--json"]).status.success());
    let blocked = run_cli(root, &["index", "--json"]);
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("writer busy"));
    std::os::unix::fs::symlink(
        root.join(".ast-index/index.sqlite"),
        root.join("alias.sqlite"),
    )
    .unwrap();
    assert!(
        !run_cli(
            root,
            &[
                "--db",
                root.join("alias.sqlite").to_str().unwrap(),
                "index",
                "--json"
            ]
        )
        .status
        .success()
    );
    drop(guard);
    assert!(run_cli(root, &["index", "--json"]).status.success());
}

#[test]
fn incomplete_walk_retains_previously_indexed_files() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let sub = root.join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("lib.rs"), "fn retained() {}").unwrap();
    run_cli_json(root, &["index", "--json"]);
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o000)).unwrap();
    let result = run_cli(root, &["index", "--json"]);
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.status.success());
    let stats: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(
        stats["scan_complete"], false,
        "test requires an unprivileged user"
    );
    assert_eq!(
        raw_json(root, &["search", "retained", "--json"])["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn imports_are_static_bounded_and_do_not_cross_roots() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(root.join("main.nix"), "{ name }: { imports = [ ./module.nix ./missing.nix ../outside.nix ./modules/${name}.nix ]; }").unwrap();
    std::fs::write(root.join("module.nix"), "{ value = 1; }").unwrap();
    std::fs::write(
        root.join("helper.ts"),
        "export function helper() { return 1; }",
    )
    .unwrap();
    std::fs::write(
        root.join("main.js"),
        "import { helper as renamed } from './helper'; function entry() { renamed(); }",
    )
    .unwrap();
    run_cli_json(root, &["index", "--json"]);
    let imports = raw_json(root, &["imports", "main.nix", "--json"]);
    let rows = imports["items"].as_array().unwrap();
    assert_eq!(rows.len(), 4);
    assert!(rows.iter().any(|r| r["target"] == "module.nix"));
    for reason in ["unknown", "outside_root", "dynamic"] {
        assert!(rows.iter().any(|r| r["reason"] == reason), "{rows:?}");
    }
    assert_eq!(
        raw_json(root, &["importers", "module.nix", "--json"])["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let cli = raw_json(root, &["deps", "main.js", "--json"]);
    assert_eq!(
        cli,
        mcp_json(
            root,
            serde_json::json!({"action":"deps","file":"main.js","format":"json"})
        )
    );
    assert_eq!(cli["items"][0]["target"], "helper.ts");
    let calls = raw_json(root, &["callees", "entry", "--json"]);
    assert_eq!(calls["items"][0]["callee"], "helper");
    // Adding another plausible extension invalidates the old imported edge.
    std::fs::write(root.join("helper.js"), "export function helper() {}").unwrap();
    run_cli_json(root, &["index", "--json"]);
    assert_eq!(
        raw_json(root, &["imports", "main.js", "--json"])["items"][0]["reason"],
        "ambiguous"
    );
    assert!(
        raw_json(root, &["callees", "entry", "--json"])["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn maps_and_snapshots_report_actual_structural_changes() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(
        root.join("lib.rs"),
        "fn leaf() {}\nfn entry() { leaf(); removed(); }\nfn removed() {}\n",
    )
    .unwrap();
    run_cli_json(root, &["index", "--json"]);
    assert!(!run_cli(root, &["diff", "--json"]).status.success());
    let map = raw_json(root, &["map", "leaf", "--budget", "1024", "--json"]);
    assert!(serde_json::to_vec(&map).unwrap().len() <= 1024);
    assert!(
        run_cli(root, &["map", "leaf", "--budget", "1024"])
            .stdout
            .len()
            <= 1024
    );
    assert_eq!(
        map,
        raw_json(root, &["map", "leaf", "--budget", "1024", "--json"])
    );
    assert!(!map["items"][0]["signature"].as_str().unwrap().is_empty());
    assert_eq!(map["items"][0]["symbol"]["name"], "leaf");
    let first = raw_json(root, &["status", "--json"])["generation"]
        .as_i64()
        .unwrap();
    std::fs::write(
        root.join("lib.rs"),
        "fn leaf() { let x = 1; }\nfn entry() {}\nfn added() {}\n",
    )
    .unwrap();
    run_cli_json(root, &["index", "--json"]);
    let diff = raw_json(root, &["diff", "--json"]);
    assert_eq!(diff, raw_json(root, &["diff", "--subtree", ".", "--json"]));
    let rows = diff["items"].as_array().unwrap();
    for kind in ["added", "removed", "changed", "relationship_removed"] {
        assert!(
            rows.iter().any(|r| r["change"] == kind),
            "missing {kind}: {diff}"
        );
    }
    let changes = r#"[{"file":"lib.rs","start_line":3,"end_line":3}]"#;
    let impacted = raw_json(root, &["changes", changes, "--json"]);
    assert!(
        impacted["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["origin"] == "baseline" && r["symbol"]["name"] == "removed")
    );
    assert!(
        impacted["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["origin"] == "baseline"
                && r["symbol"]["name"] == "entry"
                && r["depth"] == 1)
    );
    std::fs::write(root.join("lib.rs"), "fn newest() {}\n").unwrap();
    run_cli_json(root, &["index", "--json"]);
    assert!(
        !run_cli(root, &["diff", "--baseline", &first.to_string(), "--json"])
            .status
            .success(),
        "only two structural snapshots retained"
    );
}

#[test]
fn cli_scip_import_and_mcp_semantic_queries_share_a_contract() {
    fn field(tag: u8, bytes: &[u8]) -> Vec<u8> {
        let mut result = vec![tag << 3 | 2];
        let mut size = bytes.len();
        while size >= 128 {
            result.push((size as u8 & 127) | 128);
            size >>= 7;
        }
        result.push(size as u8);
        result.extend(bytes);
        result
    }
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let source = "fn helper() {}";
    std::fs::write(root.join("lib.rs"), source).unwrap();
    run_cli_json(root, &["index", "--json"]);
    let mut metadata = field(3, b"file:///fixture");
    metadata.extend([32, 1]);
    let mut occurrence = field(1, &[0, 3, 9]);
    occurrence.extend(field(2, b"fixture helper()."));
    occurrence.extend([24, 1]);
    let mut document = field(1, b"lib.rs");
    document.extend(field(2, &occurrence));
    document.extend(field(4, b"rust"));
    document.extend(field(5, source.as_bytes()));
    document.extend([48, 1]);
    let mut bytes = field(1, &metadata);
    bytes.extend(field(2, &document));
    let artifact = root.join("fixture.scip");
    std::fs::write(&artifact, bytes).unwrap();
    let imported = raw_json(root, &["import-scip", artifact.to_str().unwrap(), "--json"]);
    assert_eq!(imported["documents"], 1);
    let cli = raw_json(root, &["semantic", "--relation", "definition", "--json"]);
    assert_eq!(
        cli,
        mcp_json(
            root,
            serde_json::json!({"action":"semantic","relation":"definition","format":"json"})
        )
    );
    assert_eq!(cli["items"][0]["range"], serde_json::json!([0, 3, 0, 9]));
    assert_eq!(cli["items"][0]["provenance"]["kind"], "scip");
}

#[test]
fn unchanged_scans_skip_resolution_but_project_boundaries_invalidate_it() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("lib.rs"), "fn helper() {}").unwrap();
    std::fs::write(root.join("src/other.rs"), "fn entry() { crate::helper(); }").unwrap();
    run_cli_json(root, &["index", "--json"]);
    assert_eq!(
        raw_json(root, &["index", "--json"])["resolution_recomputed"],
        false
    );
    std::fs::write(root.join("src/Cargo.toml"), "[package]\nname='other'\n").unwrap();
    assert_eq!(raw_json(root, &["freshness", "--json"])["state"], "stale");
    let changed = raw_json(root, &["index", "--json"]);
    assert_eq!(changed["files_indexed"], 0);
    assert_eq!(changed["resolution_recomputed"], true);
    assert!(
        raw_json(root, &["callees", "entry", "--json"])["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn source_controlled_symlinks_cannot_redirect_default_index_writes() {
    let directory = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(external.path(), directory.path().join(".ast-index")).unwrap();
    assert!(
        !run_cli(directory.path(), &["index", "--json"])
            .status
            .success()
    );
    assert!(!external.path().join("index.sqlite").exists());
}

#[test]
fn columns_disambiguate_definitions_on_the_same_line() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(
        root.join("same.js"),
        "function same() {} function same() {}",
    )
    .unwrap();
    run_cli_json(root, &["index", "--json"]);
    assert!(
        !run_cli(
            root,
            &[
                "describe", "same", "--file", "same.js", "--line", "1", "--json"
            ]
        )
        .status
        .success()
    );
    let selected = raw_json(
        root,
        &[
            "describe", "same", "--file", "same.js", "--line", "1", "--column", "20", "--json",
        ],
    );
    assert_eq!(selected["symbol"]["start_column"], 20);
}

#[test]
fn separate_socket_services_share_readers_and_database_writer_coordination() {
    struct Service(Child);
    impl Drop for Service {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let fixture = fixture();
    let root = fixture.path();
    run_cli_json(root, &["index", "--json"]);
    let mut services = Vec::new();
    for name in ["one.sock", "two.sock"] {
        let socket = root.join(name);
        services.push(Service(
            Command::new(binary())
                .arg("--root")
                .arg(root)
                .args(["serve", "--socket"])
                .arg(&socket)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        ));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let store = ast_index::store::Store::open(root.join(".ast-index/index.sqlite")).unwrap();
    let guard = store.write_lock().unwrap();
    assert!(!run_cli(root, &["index", "--json"]).status.success());
    for name in ["one.sock", "two.sock"] {
        let mut stream = std::os::unix::net::UnixStream::connect(root.join(name)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(stream,"{}",serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"code_explore","arguments":{"action":"status","format":"json"}}})).unwrap();
        let response: Value =
            serde_json::from_str(&read_line(&mut BufReader::new(stream))).unwrap();
        assert_eq!(response["result"]["isError"], false);
    }
    drop(guard);
    assert!(run_cli(root, &["index", "--json"]).status.success());
}

#[test]
fn explicit_database_basename_is_relative_to_working_directory() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("lib.rs"), "fn helper() {}").unwrap();
    let output = Command::new(binary())
        .current_dir(directory.path())
        .args(["--root", ".", "--db", "custom.sqlite", "index", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(directory.path().join("custom.sqlite").is_file());
}

#[test]
fn list_actions_page_consistently_and_references_explain_dynamic_imports() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(
        root.join("lib.rs"),
        "fn leaf() {} fn first() { leaf(); } fn second() { leaf(); first(); }",
    )
    .unwrap();
    std::fs::write(
        root.join("load.js"),
        "export async function load(name) { return import(name); }",
    )
    .unwrap();
    run_cli_json(root, &["index", "--json"]);
    for (cli, request) in [
        (
            vec!["outline", "lib.rs"],
            serde_json::json!({"action":"outline","file":"lib.rs"}),
        ),
        (
            vec!["refs", "--file", "lib.rs"],
            serde_json::json!({"action":"refs","file":"lib.rs"}),
        ),
        (
            vec!["callers", "leaf"],
            serde_json::json!({"action":"callers","query":"leaf"}),
        ),
        (
            vec!["callees", "second"],
            serde_json::json!({"action":"callees","query":"second"}),
        ),
        (
            vec!["impact", "leaf"],
            serde_json::json!({"action":"impact","query":"leaf"}),
        ),
    ] {
        let mut full_args = cli.clone();
        full_args.push("--json");
        let full = raw_json(root, &full_args);
        let mut args = cli.clone();
        args.extend(["--limit", "1", "--json"]);
        let first = raw_json(root, &args);
        let mut request = request;
        request["limit"] = serde_json::json!(1);
        request["format"] = serde_json::json!("json");
        assert_eq!(first, mcp_json(root, request.clone()));
        assert_eq!(first["truncated"], true);
        let mut items = first["items"].as_array().unwrap().clone();
        let mut page = first;
        while let Some(cursor) = page["next_cursor"].as_str() {
            request["cursor"] = serde_json::json!(cursor);
            let mut next_args = args.clone();
            next_args.extend(["--cursor", cursor]);
            let next = raw_json(root, &next_args);
            assert_eq!(next, mcp_json(root, request.clone()));
            items.extend(next["items"].as_array().unwrap().iter().cloned());
            page = next;
        }
        assert_eq!(items, *full["items"].as_array().unwrap());
    }
    let dynamic = raw_json(
        root,
        &[
            "refs",
            "--kind",
            "import",
            "--reason",
            "dynamic",
            "--language",
            "javascript",
            "--json",
        ],
    );
    assert_eq!(dynamic["items"].as_array().unwrap().len(), 1);
    assert_eq!(dynamic["items"][0]["file"], "load.js");
    assert_eq!(
        dynamic,
        mcp_json(
            root,
            serde_json::json!({"action":"refs","kind":"import","reason":"dynamic","language":"javascript","format":"json"})
        )
    );
    assert_eq!(
        raw_json(root, &["status", "--json"])["resolution_reasons"]["dynamic"],
        1
    );
}

#[test]
fn lexical_calls_skip_python_class_names_and_respect_comprehension_and_catch_bindings() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(root.join("scope.py"), "def helper():\n    pass\nclass Container:\n    def helper(self):\n        pass\n    def run(self):\n        helper()\ndef values(items):\n    return [helper() for helper in items]\n").unwrap();
    std::fs::write(
        root.join("scope.js"),
        "function helper() {} function run() { try { work(); } catch(helper) { helper(); } }",
    )
    .unwrap();
    run_cli_json(root, &["index", "--json"]);
    let calls = raw_json(
        root,
        &["callees", "Container::run", "--file", "scope.py", "--json"],
    );
    assert_eq!(calls["items"][0]["callee"], "helper");
    for (symbol, file) in [("values", "scope.py"), ("run", "scope.js")] {
        assert!(
            raw_json(root, &["callees", symbol, "--file", file, "--json"])["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn writer_rechecks_root_ownership_and_version_drift_invalidates_caches() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let db = first.path().join("shared.sqlite");
    let owner = ast_index::Engine::open(first.path(), Some(db.clone())).unwrap();
    let contender = ast_index::Engine::open(second.path(), Some(db.clone())).unwrap();
    std::fs::write(first.path().join("lib.rs"), "fn helper() {}").unwrap();
    owner.index(&Default::default()).unwrap();
    let generation = owner.store().generation().unwrap();
    assert!(
        contender
            .index(&Default::default())
            .unwrap_err()
            .to_string()
            .contains("different indexed root")
    );
    assert_eq!(owner.store().generation().unwrap(), generation);
    assert!(
        contender
            .execute(&ast_index::query::Query {
                action: "status".into(),
                ..Default::default()
            })
            .unwrap_err()
            .to_string()
            .contains("different indexed root")
    );
    owner
        .store()
        .set_meta("resolution_version", "old-rules")
        .unwrap();
    assert_eq!(owner.status().unwrap().freshness, "stale");
    let updated = owner.index(&Default::default()).unwrap();
    assert_eq!(updated.files_indexed, 0);
    assert!(updated.resolution_recomputed);
    rusqlite::Connection::open(db)
        .unwrap()
        .execute("UPDATE files SET fingerprint='old-extraction'", [])
        .unwrap();
    assert_eq!(owner.index(&Default::default()).unwrap().files_indexed, 1);
}

#[test]
fn hoisted_reassigned_and_aliased_values_never_resolve_to_outer_functions() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let sources = [
        (
            "var.js",
            "function helper() {} function entry() { if (true) { var helper = other; } helper(); }",
        ),
        (
            "assign.js",
            "function helper() {} function entry() { helper = other; helper(); }",
        ),
        (
            "capture.js",
            "function helper() {} function mutate() { helper = other; } function entry() { helper(); }",
        ),
        (
            "pattern.js",
            "function helper() {} function entry({ callback: helper }) { helper(); }",
        ),
        (
            "default.js",
            "function helper() {} function entry({ callback: helper = other }) { helper(); }",
        ),
        (
            "except.py",
            "def helper():\n    pass\ndef entry():\n    try:\n        work()\n    except Error as helper:\n        helper()\n",
        ),
        (
            "with.py",
            "def helper():\n    pass\ndef entry():\n    with context() as helper:\n        helper()\n",
        ),
    ];
    for (name, source) in sources {
        std::fs::write(root.join(name), source).unwrap();
    }
    let indexed = raw_json(root, &["index", "--json"]);
    assert_eq!(indexed["parse_error_files"], 0);
    for (name, _) in sources {
        let calls = raw_json(root, &["callees", "entry", "--file", name, "--json"]);
        assert!(
            calls["items"].as_array().unwrap().is_empty(),
            "false call from {name}: {calls}"
        );
    }
    let refs = raw_json(
        root,
        &["refs", "assign.js", "--reason", "reassigned", "--json"],
    );
    assert_eq!(refs["items"].as_array().unwrap().len(), 1);
}

#[test]
fn named_imports_require_named_top_level_exports() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for (name, source) in [
        ("default.ts", "export default function helper() {}"),
        ("class.ts", "export class Container { helper() {} }"),
        (
            "named.ts",
            "export function helper() {} export const arrow = () => {};",
        ),
        (
            "main.js",
            "import { helper as one } from './default'; import { helper as two } from './class'; import { helper as three, arrow } from './named'; function entry() { one(); two(); three(); arrow(); }",
        ),
    ] {
        std::fs::write(root.join(name), source).unwrap();
    }
    run_cli_json(root, &["index", "--json"]);
    let typed = raw_json(
        root,
        &[
            "callees",
            "entry",
            "--file",
            "main.js",
            "--language",
            "typescript",
            "--json",
        ],
    );
    assert_eq!(typed["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        typed["items"][0]["file"], "main.js",
        "edge locations remain call sites"
    );
    assert!(
        raw_json(
            root,
            &[
                "callees",
                "entry",
                "--file",
                "main.js",
                "--language",
                "javascript",
                "--json"
            ]
        )["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let refs = raw_json(root, &["refs", "main.js", "--kind", "call", "--json"]);
    let rows = refs["items"].as_array().unwrap();
    for name in ["one", "two"] {
        let row = rows.iter().find(|r| r["name"] == name).unwrap();
        assert!(row["resolved"].is_null(), "{row}");
    }
    for name in ["three", "arrow"] {
        let row = rows.iter().find(|r| r["name"] == name).unwrap();
        assert_eq!(row["reason"], "imported_symbol", "{row}");
        assert!(row["resolved"].is_number());
    }
}

#[test]
fn member_namespaces_and_rust_modules_do_not_leak_bare_bindings() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(
        root.join("class.js"),
        "function helper() {} class Container { helper() {} entry() { helper(); } }",
    )
    .unwrap();
    std::fs::write(root.join("lib.rs"), "fn helper() {} struct Container; impl Container { fn helper(&self) {} fn entry() { helper(); } fn receiver(&self) { self.helper(); } } mod nested { fn entry() { helper(); } fn explicit() { super::helper(); } }").unwrap();
    run_cli_json(root, &["index", "--json"]);
    for file in ["class.js", "lib.rs"] {
        let calls = raw_json(
            root,
            &["callees", "Container::entry", "--file", file, "--json"],
        );
        assert_eq!(calls["items"][0]["callee"], "helper");
    }
    assert!(
        raw_json(
            root,
            &["callees", "nested::entry", "--file", "lib.rs", "--json"]
        )["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        raw_json(
            root,
            &["callees", "nested::explicit", "--file", "lib.rs", "--json"]
        )["items"][0]["callee"],
        "helper"
    );
    assert_eq!(
        raw_json(
            root,
            &[
                "callees",
                "Container::receiver",
                "--file",
                "lib.rs",
                "--json"
            ]
        )["items"][0]["callee"],
        "Container::helper"
    );
}

#[test]
fn shell_shebangs_and_incremental_detection_are_bounded_and_static() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (name, header) in [
        ("direct", "#!/bin/bash"),
        ("env", "#!/usr/bin/env bash"),
        ("split", "#!/usr/bin/env -S bash -eu"),
        ("posix", "#!/bin/sh"),
    ] {
        std::fs::write(
            root.join(name),
            format!("{header}\nhelper() {{ echo ok; }}\nrun() {{ helper; }}\nrun\n"),
        )
        .unwrap();
    }
    std::fs::write(
        root.join("unsupported"),
        "#!/bin/zsh\nignored() { echo no; }",
    )
    .unwrap();
    std::fs::write(
        root.join("source.sh"),
        "source ./lib.sh\nsource \"$DYNAMIC\"\n",
    )
    .unwrap();
    std::fs::write(root.join("lib.sh"), "loaded() { :; }\n").unwrap();
    run_cli_json(root, &["index", "--json"]);
    let callers = run_cli_json(root, &["callers", "helper", "--file", "split", "--json"]);
    assert_eq!(callers[0]["caller"], "run");
    let imports = run_cli_json(root, &["imports", "source.sh", "--json"]);
    assert_eq!(imports[0]["name"], "./lib.sh");
    assert!(imports[0]["target"].is_null());
    assert_eq!(imports[0]["reason"], "runtime_working_directory");
    let symbols = run_cli_json(root, &["search", "ignored", "--json"]);
    assert!(symbols.as_array().unwrap().is_empty());
    assert_eq!(run_cli_json(root, &["index", "--json"])["files_indexed"], 0);
    std::fs::write(
        root.join("direct"),
        "#!/usr/bin/python3\ndef ignored(): pass\n",
    )
    .unwrap();
    assert_eq!(
        run_cli_json(root, &["freshness", "--json"])["state"],
        "stale"
    );
    assert_eq!(run_cli_json(root, &["index", "--json"])["files_removed"], 1);
}

#[test]
fn go_packages_imports_module_changes_and_shadowing_share_cli_mcp_contracts() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join("lib")).unwrap();
    std::fs::create_dir(root.join("nested")).unwrap();
    std::fs::write(root.join("go.mod"), "module example.test/app\n\ngo 1.24\n").unwrap();
    std::fs::write(root.join("a.go"), "package app\nfunc local() {}\n").unwrap();
    std::fs::write(root.join("b.go"), "package app\nimport \"example.test/app/lib\"\nfunc Run() { local(); tools.Help() }\nfunc Shadow(local func()) { local() }\n").unwrap();
    std::fs::write(root.join("lib/lib.go"), "package tools\nfunc Help() {}\n").unwrap();
    std::fs::write(root.join("nested/go.mod"), "module example.test/other\n").unwrap();
    std::fs::write(root.join("nested/a.go"), "package app\nfunc local() {}\n").unwrap();
    run_cli_json(root, &["index", "--json"]);
    let result = raw_json(root, &["callees", "app::Run", "--file", "b.go", "--json"]);
    let callees: Vec<_> = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["callee"].as_str().unwrap())
        .collect();
    assert_eq!(callees.len(), 2, "{result}");
    assert!(callees.contains(&"app::local") && callees.contains(&"tools::Help"));
    assert!(
        run_cli_json(
            root,
            &["callees", "app::Shadow", "--file", "b.go", "--json"]
        )
        .as_array()
        .unwrap()
        .is_empty()
    );
    assert_eq!(
        result,
        mcp_json(
            root,
            serde_json::json!({"action":"callees","query":"app::Run","file":"b.go","format":"json"})
        )
    );
    std::fs::write(root.join("go.mod"), "module example.test/new\n\ngo 1.24\n").unwrap();
    assert_eq!(
        run_cli_json(root, &["freshness", "--json"])["state"],
        "stale"
    );
    let stats = run_cli_json(root, &["index", "--json"]);
    assert_eq!(stats["files_indexed"], 0);
    assert_eq!(stats["resolution_recomputed"], true);
    let result = run_cli_json(root, &["callees", "app::Run", "--file", "b.go", "--json"]);
    assert_eq!(result.as_array().unwrap().len(), 1);
    assert_eq!(result[0]["callee"], "app::local");
}

#[test]
fn new_languages_filter_and_outline_identically_over_cli_and_mcp() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (file, source) in [
        ("script.bash", "helper() { :; }\nrun() { helper; }\n"),
        ("main.go", "package app\nfunc Run() {}\n"),
        (
            "main.lua",
            "local function helper() end\nlocal function run() helper() end\nrun()\n",
        ),
        (
            "schema.sql",
            "CREATE TABLE public.items (id bigint); CREATE VIEW public.visible AS SELECT i.id FROM public.items AS i;",
        ),
    ] {
        std::fs::write(root.join(file), source).unwrap();
    }
    let stats = run_cli_json(root, &["index", "--json"]);
    assert_eq!(stats["parse_error_files"], 0, "{stats}");
    for (file, language) in [
        ("script.bash", "bash"),
        ("main.go", "go"),
        ("main.lua", "lua"),
        ("schema.sql", "sql"),
    ] {
        let cli = raw_json(root, &["outline", file, "--language", language, "--json"]);
        assert!(
            !cli["items"].as_array().unwrap().is_empty(),
            "{language}: {cli}"
        );
        assert_eq!(
            cli,
            mcp_json(
                root,
                serde_json::json!({"action":"outline","file":file,"language":language,"format":"json"})
            )
        );
    }
    let lua = run_cli_json(root, &["callers", "helper", "--file", "main.lua", "--json"]);
    assert_eq!(lua[0]["caller"], "run");
}

#[test]
fn sql_dependencies_never_become_calls_or_index_internal_database_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join(".ast-index")).unwrap();
    std::fs::write(
        root.join(".ast-index/generated.sql"),
        "CREATE TABLE forbidden (id int);",
    )
    .unwrap();
    std::fs::write(
        root.join("other.sqlite"),
        "CREATE TABLE also_forbidden (id int);",
    )
    .unwrap();
    std::fs::write(root.join("schema.sql"), "CREATE TABLE public.items (id bigint); CREATE VIEW public.visible AS SELECT i.id FROM public.items AS i;").unwrap();
    for _ in 0..2 {
        let stats = run_cli_json(root, &["index", "--json"]);
        assert_eq!(stats["files_seen"], 1, "{stats}");
        let deps = raw_json(
            root,
            &[
                "refs",
                "--file",
                "schema.sql",
                "--kind",
                "dependency",
                "--json",
            ],
        );
        assert_eq!(deps["items"].as_array().unwrap().len(), 1, "{deps}");
        assert!(!deps["items"][0]["resolved"].is_null(), "{deps}");
        assert_eq!(deps["items"][0]["from_symbol"], "public.visible");
        let status = raw_json(root, &["status", "--json"]);
        assert_eq!(status["references"], 1, "{status}");
        assert_eq!(status["resolved_references"], 1, "{status}");
        assert_eq!(status["resolution_reasons"]["static_sql_object"], 1);
        if stats["resolution_recomputed"] == true {
            assert_eq!(
                status["resolved_references"],
                stats["resolution"]["resolved"]
            );
        }
        assert_eq!(
            deps,
            mcp_json(
                root,
                serde_json::json!({"action":"refs","file":"schema.sql","kind":"dependency","format":"json"})
            )
        );
        assert!(
            run_cli_json(
                root,
                &["callers", "public.items", "--file", "schema.sql", "--json"]
            )
            .as_array()
            .unwrap()
            .is_empty()
        );
        assert!(
            run_cli_json(root, &["search", "forbidden", "--json"])
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn sql_relation_and_function_namespaces_preserve_ambiguity_independently() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(
        root.join("schema.sql"),
        "CREATE TABLE app.same (id integer);\n\
         CREATE FUNCTION app.same(value integer) RETURNS integer LANGUAGE sql AS $$ SELECT 1 $$;\n\
         CREATE VIEW app.consumer AS SELECT app.same(1) FROM app.same;",
    )
    .unwrap();
    let stats = run_cli_json(root, &["index", "--json"]);
    assert_eq!(stats["parse_error_files"], 0, "{stats}");
    let symbols = run_cli_json(root, &["outline", "schema.sql", "--json"]);
    let table = symbols
        .as_array()
        .unwrap()
        .iter()
        .find(|symbol| symbol["qualified"] == "app.same" && symbol["kind"] == "table")
        .unwrap();
    let function = symbols
        .as_array()
        .unwrap()
        .iter()
        .find(|symbol| symbol["qualified"] == "app.same" && symbol["kind"] == "function")
        .unwrap();
    assert_ne!(table["id"], function["id"]);
    let refs = run_cli_json(root, &["refs", "schema.sql", "--json"]);
    let dependency = refs
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "dependency")
        .unwrap();
    let call = refs
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "call")
        .unwrap();
    assert_eq!(dependency["resolved"], table["id"]);
    assert_eq!(call["resolved"], function["id"]);
    assert_eq!(dependency["from_symbol"], "app.consumer");
    assert_eq!(call["from_symbol"], "app.consumer");
    let callees = run_cli_json(root, &["callees", "app.consumer", "--json"]);
    assert_eq!(callees.as_array().unwrap().len(), 1, "{callees}");
    assert_eq!(callees[0]["callee"], "app.same");

    std::fs::write(
        root.join("overload.sql"),
        "CREATE FUNCTION app.same(value text) RETURNS integer LANGUAGE sql AS $$ SELECT 2 $$;",
    )
    .unwrap();
    let stats = run_cli_json(root, &["index", "--json"]);
    assert_eq!(stats["files_indexed"], 1);
    assert_eq!(stats["parse_error_files"], 0, "{stats}");
    let refs = run_cli_json(root, &["refs", "schema.sql", "--json"]);
    let dependency = refs
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "dependency")
        .unwrap();
    let call = refs
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "call")
        .unwrap();
    assert_eq!(dependency["resolved"], table["id"]);
    assert!(call["resolved"].is_null());
    assert_eq!(call["reason"], "ambiguous");
    assert!(
        run_cli_json(root, &["callees", "app.consumer", "--json"])
            .as_array()
            .unwrap()
            .is_empty()
    );

    std::fs::write(
        root.join("duplicate.sql"),
        "CREATE TABLE app.same (id integer);",
    )
    .unwrap();
    run_cli_json(root, &["index", "--json"]);
    let refs = run_cli_json(
        root,
        &["refs", "schema.sql", "--kind", "dependency", "--json"],
    );
    assert_eq!(refs.as_array().unwrap().len(), 1);
    assert!(refs[0]["resolved"].is_null());
    assert_eq!(refs[0]["reason"], "ambiguous");
}

#[test]
fn sql_qualified_references_cross_files_but_not_project_or_search_path_boundaries() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::create_dir(root.join("nested")).unwrap();
    std::fs::write(root.join("nested/package.json"), "{}").unwrap();
    for (file, source) in [
        (
            "schema.sql",
            "CREATE TABLE public.users (id integer); CREATE TABLE public.root_only (id integer); CREATE TABLE local_only (id integer);",
        ),
        (
            "query.sql",
            "SELECT * FROM public.users; SELECT * FROM users; SELECT * FROM local_only;",
        ),
        (
            "nested/schema.sql",
            "CREATE TABLE public.users (id integer);",
        ),
        (
            "nested/query.sql",
            "SELECT * FROM public.users; SELECT * FROM public.root_only;",
        ),
    ] {
        std::fs::write(root.join(file), source).unwrap();
    }
    let stats = run_cli_json(root, &["index", "--json"]);
    assert_eq!(stats["parse_error_files"], 0, "{stats}");
    let root_symbols = run_cli_json(root, &["outline", "schema.sql", "--json"]);
    let root_users = root_symbols
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["qualified"] == "public.users")
        .unwrap();
    let nested_symbols = run_cli_json(root, &["outline", "nested/schema.sql", "--json"]);
    let nested_users = &nested_symbols[0];
    assert_ne!(root_users["id"], nested_users["id"]);
    let refs = run_cli_json(root, &["refs", "query.sql", "--json"]);
    assert_eq!(refs.as_array().unwrap().len(), 3);
    assert_eq!(refs[0]["resolved"], root_users["id"]);
    assert_eq!(refs[0]["reason"], "static_sql_object");
    assert_eq!(refs[0]["confidence"], "same_directory");
    for reference in [&refs[1], &refs[2]] {
        assert!(reference["resolved"].is_null(), "{reference}");
        assert_eq!(reference["reason"], "unknown_sql_object");
    }
    let refs = run_cli_json(root, &["refs", "nested/query.sql", "--json"]);
    assert_eq!(refs.as_array().unwrap().len(), 2);
    assert_eq!(refs[0]["resolved"], nested_users["id"]);
    assert!(refs[1]["resolved"].is_null());
    assert_eq!(refs[1]["reason"], "unknown_sql_object");
}

#[test]
fn sql_quoted_identifier_punctuation_keeps_distinct_target_identities() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(
        root.join("quoted.sql"),
        "CREATE TABLE app.\"x.y\" (id integer);\n\
         CREATE TABLE \"app.x\".y (id integer);\n\
         CREATE TABLE \"app.x.y\" (id integer);\n\
         CREATE TABLE APP.Users (id integer);\n\
         CREATE TABLE app.\"Users\" (id integer);\n\
         CREATE VIEW app.punctuation AS SELECT * FROM app.\"x.y\" a JOIN \"app.x\".y b ON true JOIN \"app.x.y\" c ON true;\n\
         CREATE VIEW app.cases AS SELECT * FROM \"app\".\"users\" a JOIN app.\"Users\" b ON true;",
    )
    .unwrap();
    let stats = run_cli_json(root, &["index", "--json"]);
    assert_eq!(stats["parse_error_files"], 0, "{stats}");
    let symbols = run_cli_json(root, &["outline", "quoted.sql", "--json"]);
    let refs = run_cli_json(
        root,
        &["refs", "quoted.sql", "--kind", "dependency", "--json"],
    );
    let expected = [
        "app.\"x.y\"",
        "\"app.x\".y",
        "\"app.x.y\"",
        "app.users",
        "app.\"Users\"",
    ];
    assert_eq!(refs.as_array().unwrap().len(), expected.len());
    let mut ids = std::collections::HashSet::new();
    for (reference, qualified) in refs.as_array().unwrap().iter().zip(expected) {
        let symbol = symbols
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["qualified"] == qualified && s["kind"] == "table")
            .unwrap();
        assert_eq!(
            reference["resolved"], symbol["id"],
            "{qualified}: {reference}"
        );
        assert_eq!(reference["reason"], "static_sql_object");
        assert!(ids.insert(symbol["id"].as_i64().unwrap()));
    }
    assert_eq!(refs[0]["from_symbol"], "app.punctuation");
    assert_eq!(refs[3]["from_symbol"], "app.cases");
}

#[test]
fn sql_cte_and_search_path_reasons_survive_cli_and_mcp_queries() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(
        root.join("scopes.sql"),
        "CREATE TABLE users (id integer);\n\
         CREATE TABLE app.users (id integer);\n\
         WITH users AS (SELECT * FROM app.users) SELECT * FROM users;\n\
         SELECT * FROM users;\n\
         SET search_path TO other;\n\
         SELECT * FROM users;\n\
         SELECT * FROM app.users;",
    )
    .unwrap();
    let stats = run_cli_json(root, &["index", "--json"]);
    assert_eq!(stats["parse_error_files"], 0, "{stats}");
    let symbols = run_cli_json(root, &["outline", "scopes.sql", "--json"]);
    let local = symbols
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["qualified"] == "users")
        .unwrap();
    let qualified = symbols
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["qualified"] == "app.users")
        .unwrap();
    let refs = raw_json(
        root,
        &["refs", "scopes.sql", "--kind", "dependency", "--json"],
    );
    let rows = refs["items"].as_array().unwrap();
    assert_eq!(rows.len(), 5, "{refs}");
    assert_eq!(rows[0]["resolved"], qualified["id"]);
    assert!(rows[1]["resolved"].is_null());
    assert_eq!(rows[1]["reason"], "sql_cte");
    assert_eq!(rows[2]["resolved"], local["id"]);
    assert!(rows[3]["resolved"].is_null());
    assert_eq!(rows[3]["reason"], "sql_dynamic_namespace");
    assert_eq!(rows[4]["resolved"], qualified["id"]);
    assert_eq!(
        refs,
        mcp_json(
            root,
            serde_json::json!({"action":"refs","file":"scopes.sql","kind":"dependency","format":"json"})
        )
    );
}

#[test]
fn lua_static_module_layouts_and_loader_mutations_remain_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join("pkg")).unwrap();
    std::fs::write(root.join("main.lua"), "local module = require('pkg')\nlocal function helper() end\nlocal function run() helper() end\nrun()\n").unwrap();
    std::fs::write(
        root.join("pkg/init.lua"),
        "local M = {}\nfunction M.work() end\nreturn M\n",
    )
    .unwrap();
    std::fs::write(
        root.join("dynamic.lua"),
        "package.path = custom\nrequire('pkg')\n",
    )
    .unwrap();
    std::fs::write(
        root.join("shadow.lua"),
        "local require = loader\nrequire('pkg')\n",
    )
    .unwrap();
    run_cli_json(root, &["index", "--json"]);
    let deps = run_cli_json(root, &["deps", "main.lua", "--json"]);
    assert_eq!(deps[0]["target"], "pkg/init.lua");
    for file in ["dynamic.lua", "shadow.lua"] {
        let imports = run_cli_json(root, &["imports", file, "--json"]);
        assert!(
            imports
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["target"].is_null()),
            "{imports}"
        );
    }
    std::fs::write(root.join("pkg.lua"), "return {}\n").unwrap();
    run_cli_json(root, &["index", "--json"]);
    let imports = run_cli_json(root, &["imports", "main.lua", "--json"]);
    assert!(imports[0]["target"].is_null());
    assert_eq!(imports[0]["reason"], "ambiguous");
}

#[test]
fn new_language_metadata_reads_share_scan_budget_and_reject_root_escape() {
    use ast_index::{Engine, IndexOptions};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let source = "#!/bin/bash\nwork() { :; }\n";
    std::fs::write(root.join("task"), source).unwrap();
    let engine = Engine::open(root, None).unwrap();
    let stats = engine
        .index(&IndexOptions {
            max_total_bytes: source.len() as u64,
            ..IndexOptions::default()
        })
        .unwrap();
    assert!(!stats.scan_complete);
    assert_eq!(stats.files_indexed, 0);
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(
        outside.path().join("go.mod"),
        "module example.test/outside\n",
    )
    .unwrap();
    std::fs::remove_file(root.join("task")).unwrap();
    std::fs::write(root.join("main.go"), "package main\nfunc main() {}\n").unwrap();
    std::os::unix::fs::symlink(outside.path().join("go.mod"), root.join("go.mod")).unwrap();
    let stats = engine.index(&IndexOptions::default()).unwrap();
    assert!(!stats.scan_complete);
    assert!(stats.files_failed > 0);
    assert_eq!(stats.files_indexed, 1);
}

#[test]
fn custom_sql_database_and_renamed_sqlite_are_excluded_from_source_indexing() {
    use ast_index::{Engine, IndexOptions};
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let database = root.join("cache.sql");
    let renamed = root.join("renamed.sql");
    {
        let binary = rusqlite::Connection::open(&renamed).unwrap();
        binary
            .execute_batch("CREATE TABLE must_not_leak (id INTEGER);")
            .unwrap();
    }
    std::fs::write(
        root.join("source.sql"),
        "CREATE TABLE public.visible (id integer);",
    )
    .unwrap();
    let engine = Engine::open(root, Some(database)).unwrap();
    for _ in 0..2 {
        let stats = engine.index(&IndexOptions::default()).unwrap();
        assert_eq!(stats.files_seen, 1, "{stats:?}");
        assert_eq!(engine.status().unwrap().files, 1);
        let symbols = engine
            .execute(&ast_index::query::Query {
                action: "outline".into(),
                file: Some("source.sql".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(symbols["items"].as_array().unwrap().len(), 1);
        assert_eq!(symbols["items"][0]["qualified"], "public.visible");
        assert_eq!(
            engine
                .execute(&ast_index::query::Query {
                    action: "freshness".into(),
                    ..Default::default()
                })
                .unwrap()["state"],
            "fresh_at_check"
        );
    }
}

#[test]
fn sql_reference_owners_use_ranges_when_table_and_function_names_match() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(
        root.join("schema.sql"),
        "CREATE FUNCTION app.helper() RETURNS integer LANGUAGE sql AS $$ SELECT 1 $$;\n\
         CREATE TABLE app.same (id integer);\n\
         CREATE FUNCTION app.same() RETURNS integer LANGUAGE sql AS $$ SELECT app.helper() $$;\n",
    )
    .unwrap();
    assert_eq!(
        run_cli_json(root, &["index", "--json"])["parse_error_files"],
        0
    );
    let function = run_cli_json(
        root,
        &[
            "callees",
            "app.same",
            "--file",
            "schema.sql",
            "--line",
            "3",
            "--json",
        ],
    );
    assert_eq!(function.as_array().unwrap().len(), 1, "{function}");
    assert_eq!(function[0]["callee"], "app.helper");
    let table = run_cli_json(
        root,
        &[
            "callees",
            "app.same",
            "--file",
            "schema.sql",
            "--line",
            "2",
            "--json",
        ],
    );
    assert!(table.as_array().unwrap().is_empty(), "{table}");
}
