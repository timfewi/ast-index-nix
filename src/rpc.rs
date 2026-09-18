//! Minimal Model Context Protocol server.
//!
//! The transport is newline-delimited JSON-RPC 2.0, which the MCP specification
//! defines for stdio and recommends reusing for Unix domain sockets. The server
//! exposes exactly one tool (`code_explore`) by design: a small, stable tool
//! surface steers agents better than a menu and keeps the prompt cache stable.
//!
//! The dispatcher is revision-tolerant: it answers `initialize` for the
//! 2025-06-18 and 2025-11-25 lifecycle and also answers requests that already
//! carry their protocol version in `params._meta` (the stateless 2026-07-28
//! revision, which removes the handshake).

use serde_json::{Value, json};

use crate::engine::Engine;
use crate::error::Error;
use crate::model::Symbol;
use crate::report;

/// Server name reported to clients.
pub const SERVER_NAME: &str = "ast-index";
/// Fallback protocol revision.
pub const LATEST_PROTOCOL: &str = "2026-07-28";
/// Protocol revisions this server can answer.
pub const SUPPORTED_PROTOCOLS: &[&str] = &["2025-06-18", "2025-11-25", "2026-07-28"];
/// Tool name exposed to agents.
pub const EXPLORE_TOOL: &str = "code_explore";

/// Guidance returned to clients during `initialize`. Kept short because clients
/// truncate server instructions (Claude Code at 2 KB) and Codex only guarantees
/// the first 512 characters.
pub const INSTRUCTIONS: &str = "ast-index answers structural questions about the indexed repository. \
Call code_explore first for symbol lookup, outlines, callers/callees and impact. \
Edges are name-resolved and precision-first: ambiguous names are reported as unresolved, and dynamic \
dispatch, macros, reflection and generated code are not resolved. If no index exists the tool returns \
setup guidance instead of an error. Treat every result as a hint and read the file before editing.";

/// Parsed tool arguments.
#[derive(Debug, Default)]
struct ExploreArgs {
    action: String,
    query: Option<String>,
    file: Option<String>,
    depth: Option<u32>,
    limit: Option<usize>,
    format_json: bool,
}

/// One MCP server bound to an indexed root.
pub struct RpcServer {
    engine: Engine,
}

impl RpcServer {
    /// Create a server for `engine`.
    pub fn new(engine: Engine) -> Self {
        Self { engine }
    }

    /// Access the engine (used by the socket service).
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Handle one newline-delimited message. Returns `None` for notifications.
    pub fn handle_line(&self, line: &str) -> Option<String> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let request: Value = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(error) => {
                return Some(
                    json!({
                        "jsonrpc": "2.0",
                        "id": Value::Null,
                        "error": { "code": -32700, "message": format!("parse error: {error}") }
                    })
                    .to_string(),
                );
            }
        };
        if request.get("id").is_none() || request.get("id") == Some(&Value::Null) {
            // Notification: MCP expects no response.
            return None;
        }
        let response = self.handle(&request);
        Some(response.to_string())
    }

    /// Dispatch one request to a response value.
    pub fn handle(&self, request: &Value) -> Value {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = match request.get("method").and_then(Value::as_str) {
            Some(method) => method,
            None => return error(id, -32600, "invalid request: missing method"),
        };
        let params = request.get("params").cloned().unwrap_or(json!({}));
        match method {
            "initialize" => {
                let requested = params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or(LATEST_PROTOCOL);
                let version = if SUPPORTED_PROTOCOLS.contains(&requested) {
                    requested
                } else {
                    LATEST_PROTOCOL
                };
                result(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": { "tools": { "listChanged": false } },
                        "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
                        "instructions": INSTRUCTIONS
                    }),
                )
            }
            "ping" => result(id, json!({})),
            "tools/list" => result(id, json!({ "tools": [tool_definition()] })),
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if name != EXPLORE_TOOL {
                    return error(id, -32602, &format!("unknown tool: {name}"));
                }
                match self.call_tool(&params) {
                    Ok(call_result) => result(id, call_result),
                    Err(protocol_error) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": protocol_error
                    }),
                }
            }
            "resources/list" => result(id, json!({ "resources": [] })),
            "resources/templates/list" => result(id, json!({ "resourceTemplates": [] })),
            "prompts/list" => result(id, json!({ "prompts": [] })),
            "logging/setLevel" => result(id, json!({})),
            other => error(id, -32601, &format!("method not found: {other}")),
        }
    }

    fn call_tool(&self, params: &Value) -> std::result::Result<Value, Value> {
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
        let args = match parse_args(&arguments) {
            Ok(args) => args,
            Err(message) => return Err(error_value(-32602, &message)),
        };
        match self.run(&args) {
            Ok(text) => Ok(json!({
                "content": [ { "type": "text", "text": text } ],
                "isError": false
            })),
            // Not-indexed is deliberately not an error: an `isError` reply early
            // in a session teaches the agent that the tool is broken.
            Err(Error::NotIndexed(message)) => Ok(json!({
                "content": [ { "type": "text", "text": message } ],
                "isError": false
            })),
            Err(Error::Invalid(message)) => Ok(json!({
                "content": [ { "type": "text", "text": message } ],
                "isError": true
            })),
            Err(other) => Ok(json!({
                "content": [ { "type": "text", "text": format!(
                    "Tool execution failed: {other}. Retry the call once; if it persists, continue without ast-index."
                ) } ],
                "isError": true
            })),
        }
    }

    fn run(&self, args: &ExploreArgs) -> crate::error::Result<String> {
        let limit = args.limit.unwrap_or(25).clamp(1, 100);
        let pretty = |value: Value| {
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
        };
        match args.action.as_str() {
            "status" => {
                let status = self.engine.status()?;
                if args.format_json {
                    Ok(pretty(serde_json::to_value(&status)?))
                } else {
                    Ok(report::status_text(&status))
                }
            }
            "outline" => {
                let file = required(&args.file, "file")?;
                let symbols = self.engine.outline(file)?;
                if args.format_json {
                    Ok(pretty(serde_json::to_value(&symbols)?))
                } else {
                    Ok(report::symbols_text(&symbols))
                }
            }
            "search" => {
                let query = required(&args.query, "query")?;
                let symbols = self.engine.search(query, limit)?;
                if args.format_json {
                    Ok(pretty(serde_json::to_value(&symbols)?))
                } else {
                    Ok(report::symbols_text(&symbols))
                }
            }
            "describe" => {
                let query = required(&args.query, "query")?;
                let symbol = self.engine.symbol(query)?;
                let (_, relations) = self.engine.relations(query)?;
                if args.format_json {
                    Ok(pretty(serde_json::to_value(&symbol)?))
                } else {
                    let mut output = format!(
                        "{}  {}  {}:{}\n",
                        symbol.qualified,
                        symbol.kind.as_str(),
                        symbol.file,
                        symbol.start_line
                    );
                    output.push_str(&report::edges_text("callers", &relations.callers));
                    output.push('\n');
                    output.push_str(&report::edges_text("callees", &relations.callees));
                    Ok(output)
                }
            }
            "callers" => {
                let query = required(&args.query, "query")?;
                let (_, edges) = self.engine.callers(query)?;
                if args.format_json {
                    Ok(pretty(serde_json::to_value(&edges)?))
                } else {
                    Ok(report::edges_text("callers", &edges))
                }
            }
            "callees" => {
                let query = required(&args.query, "query")?;
                let (_, edges) = self.engine.callees(query)?;
                if args.format_json {
                    Ok(pretty(serde_json::to_value(&edges)?))
                } else {
                    Ok(report::edges_text("callees", &edges))
                }
            }
            "impact" => {
                let query = required(&args.query, "query")?;
                let depth = args.depth.unwrap_or(3).clamp(1, 10);
                let (symbol, rows) = self.engine.impact(query, depth)?;
                if args.format_json {
                    Ok(pretty(serde_json::to_value(&rows)?))
                } else {
                    Ok(report::impact_text(&symbol, &rows))
                }
            }
            other => Err(Error::Invalid(format!(
                "unknown action `{other}`; expected one of status, outline, search, describe, callers, callees, impact"
            ))),
        }
    }
}

/// The single tool definition.
pub fn tool_definition() -> Value {
    json!({
        "name": EXPLORE_TOOL,
        "title": "Code explore",
        "description": "Structural code intelligence for the indexed repository. \
    Use `search` to find symbols, `outline` for one file, `describe` for a symbol with its callers and callees, \
    `callers`/`callees` for one direction, `impact` for transitive callers, and `status` for index coverage. \
    Edges are name-resolved; ambiguous or dynamic calls are reported as unresolved. \
    Prefer this over grep/read loops, then read the file before editing.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["status", "outline", "search", "describe", "callers", "callees", "impact"],
                    "description": "Which query to run."
                },
                "query": {
                    "type": "string",
                    "description": "Symbol name or qualified name (for search, describe, callers, callees, impact)."
                },
                "file": {
                    "type": "string",
                    "description": "File path relative to the indexed root (for outline)."
                },
                "depth": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 10,
                    "description": "Maximum hops for impact (default 3)."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "Maximum results for search (default 25)."
                },
                "format": {
                    "type": "string",
                    "enum": ["text", "json"],
                    "description": "Output format; text is token-efficient and the default."
                }
            },
            "required": ["action"]
        },
        "annotations": {
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false
        }
    })
}

fn parse_args(arguments: &Value) -> std::result::Result<ExploreArgs, String> {
    let action = arguments
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing required argument `action`".to_string())?;
    let format_json = arguments.get("format").and_then(Value::as_str) == Some("json");
    Ok(ExploreArgs {
        action: action.to_string(),
        query: string_arg(arguments, "query"),
        file: string_arg(arguments, "file"),
        depth: arguments
            .get("depth")
            .and_then(Value::as_u64)
            .map(|value| value as u32),
        limit: arguments
            .get("limit")
            .and_then(Value::as_u64)
            .map(|value| value as usize),
        format_json,
    })
}

fn string_arg(arguments: &Value, key: &str) -> Option<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn required<'a>(value: &'a Option<String>, name: &str) -> std::result::Result<&'a str, Error> {
    value
        .as_deref()
        .ok_or_else(|| Error::Invalid(format!("missing required argument `{name}`")))
}

fn result(id: Value, value: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": value })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

fn error_value(code: i64, message: &str) -> Value {
    json!({ "code": code, "message": message })
}

/// Render an ambiguous lookup for callers that only have symbols.
pub fn describe_candidates(candidates: &[Symbol]) -> String {
    candidates
        .iter()
        .map(|symbol| symbol.qualified.clone())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unindexed_server() -> RpcServer {
        let directory = tempfile::tempdir().expect("tempdir");
        let engine = Engine::open(directory.path(), None).expect("engine");
        RpcServer::new(engine)
    }

    #[test]
    fn initialize_negotiates_known_and_unknown_versions() {
        let server = unindexed_server();
        let response = server.handle(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2025-11-25" }
        }));
        assert_eq!(response["result"]["protocolVersion"], json!("2025-11-25"));
        assert_eq!(response["result"]["serverInfo"]["name"], json!(SERVER_NAME));
        assert!(
            response["result"]["instructions"].as_str().unwrap().len() < 2048,
            "instructions must stay under the 2 KB client cap"
        );

        let response = server.handle(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "initialize",
            "params": { "protocolVersion": "1999-01-01" }
        }));
        assert_eq!(
            response["result"]["protocolVersion"],
            json!(LATEST_PROTOCOL)
        );
    }

    #[test]
    fn notifications_get_no_response() {
        let server = unindexed_server();
        assert!(
            server
                .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .is_none()
        );
        assert!(server.handle_line("").is_none());
    }

    #[test]
    fn tools_list_exposes_exactly_one_read_only_tool() {
        let server = unindexed_server();
        let response = server.handle(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": {}
        }));
        let tools = response["result"]["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], json!(EXPLORE_TOOL));
        assert_eq!(tools[0]["annotations"]["readOnlyHint"], json!(true));
    }

    #[test]
    fn unindexed_status_is_guidance_not_an_error() {
        let server = unindexed_server();
        let response = server.handle(&json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": EXPLORE_TOOL, "arguments": { "action": "search", "query": "main" } }
        }));
        assert_eq!(response["result"]["isError"], json!(false));
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .expect("text");
        assert!(text.contains("ast-index index"), "text: {text}");
    }

    #[test]
    fn unknown_tool_is_invalid_params() {
        let server = unindexed_server();
        let response = server.handle(&json!({
            "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": { "name": "nope", "arguments": {} }
        }));
        assert_eq!(response["error"]["code"], json!(-32602));
    }

    #[test]
    fn parse_errors_are_reported_with_null_id() {
        let server = unindexed_server();
        let response = server.handle_line("{not json").expect("response");
        let value: Value = serde_json::from_str(&response).expect("json");
        assert_eq!(value["error"]["code"], json!(-32700));
        assert_eq!(value["id"], Value::Null);
    }
}
