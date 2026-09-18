//! End-to-end tests: index a synthetic project, query it through the CLI, the
//! stdio MCP adapter and the Unix socket service.

use std::io::{BufRead, BufReader, Write};
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
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "ast-index {args:?} did not emit JSON: {error}\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
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
    assert_eq!(entry_edge["confidence"], Value::from("high"));

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

    drop(child.stdin.take());
    let _ = child.wait();
    let _ = service.kill();
    let _ = service.wait();
}
