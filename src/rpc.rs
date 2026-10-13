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
use crate::report;

/// Server name reported to clients.
pub const SERVER_NAME: &str = "ast-index";
/// Fallback protocol revision.
pub const LATEST_PROTOCOL: &str = "2026-07-28";
/// Protocol revisions this server can answer.
pub const SUPPORTED_PROTOCOLS: &[&str] = &["2025-06-18", "2025-11-25", "2026-07-28"];
/// Tool name exposed to agents.
pub const EXPLORE_TOOL: &str = "code_explore";
/// Maximum agent-facing text in one tool result, regardless of output format.
pub const MAX_TOOL_TEXT_BYTES: usize = 64 * 1024;

/// Guidance returned to clients during `initialize`. Kept short because clients
/// truncate server instructions (Claude Code at 2 KB) and Codex only guarantees
/// the first 512 characters.
pub const INSTRUCTIONS: &str = "ast-index answers structural questions about the indexed repository. \
Call code_explore first for symbol lookup, outlines, callers/callees and impact. \
Edges are name-resolved and precision-first: ambiguous names are reported as unresolved, and dynamic \
dispatch, macros, reflection and generated code are not resolved. If no index exists the tool returns \
setup guidance instead of an error. Treat every result as a hint and read the file before editing.";

struct ExploreArgs {
    query: crate::query::Query,
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
        if !id.is_null() && !id.is_number() && !id.as_str().is_some_and(|s| s.len() <= 256) {
            return error(Value::Null, -32600, "invalid or oversized request id");
        }
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
        let (mut text, mut is_error) = match self.run(&args) {
            Ok(text) => (text, false),
            // Not-indexed is deliberately not an error: an `isError` reply early
            // in a session teaches the agent that the tool is broken.
            Err(Error::NotIndexed(message)) => (message, false),
            Err(Error::Invalid(message)) => (message, true),
            Err(error @ Error::Ambiguous(_)) => (error.payload().to_string(), true),
            Err(other) => (
                format!(
                    "Tool execution failed: {other}. Retry the call once; if it persists, continue without ast-index."
                ),
                true,
            ),
        };
        if text.len() > MAX_TOOL_TEXT_BYTES {
            text = format!(
                "Tool result exceeds {MAX_TOOL_TEXT_BYTES} bytes; narrow the query or choose a smaller file."
            );
            is_error = true;
        }
        Ok(json!({
            "content": [ { "type": "text", "text": text } ],
            "isError": is_error
        }))
    }

    fn run(&self, args: &ExploreArgs) -> crate::error::Result<String> {
        let value = self.engine.execute(&args.query)?;
        if args.format_json {
            Ok(serde_json::to_string(&value)?)
        } else {
            Ok(report::query_text(&value))
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
                    "enum": crate::query::ACTIONS,
                    "description": "Which query to run."
                },
                "query": {
                    "type": "string",
                    "description": "Symbol name or qualified name (for search, describe, callers, callees, impact)."
                },
                "file": {
                    "type": "string",
                    "description": "Relative file path; also disambiguates symbols for graph queries."
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
                    "description": "Maximum results per page (default 25)."
                },
                "subtree": { "type": "string", "description": "Restrict results to a relative subtree." },
                "language": { "type": "string" },
                "kind": { "type": "string" },
                "line": { "type": "integer", "minimum": 1 },
                "column": { "type": "integer", "minimum": 1, "description":"One-based UTF-8 byte column; requires line." },
                "cursor": { "type": "string", "description": "Opaque continuation returned by this query; invalid after reindexing." },
                "match_mode": { "enum": ["exact", "prefix", "contains"], "description": "Literal name matching; no user wildcards." },
                "reason": { "type": "string", "description": "Filter references by resolution reason." },
                "budget": { "type": "integer", "minimum": 1024, "maximum": 32768, "description": "Repository map byte budget." },
                "baseline": { "type": "integer", "minimum": 1, "description": "Retained structural snapshot generation." },
                "changes": { "type": "array", "maxItems": 100, "items": { "type": "object", "properties": {
                    "file": {"type":"string"}, "start_line":{"type":"integer","minimum":1}, "end_line":{"type":"integer","minimum":1}
                }, "required":["file","start_line","end_line"], "additionalProperties":false } },
                "relation": {"enum":["definition","reference","implementation"],"description":"SCIP semantic relation, default definition. Never treated as a call edge."},
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
    let mut arguments = arguments.clone();
    let object = arguments
        .as_object_mut()
        .ok_or("arguments must be an object")?;
    let format_json = match object.remove("format") {
        None => false,
        Some(Value::String(s)) if s == "text" => false,
        Some(Value::String(s)) if s == "json" => true,
        _ => return Err("format must be text or json".into()),
    };
    if !object.contains_key("action") {
        return Err("missing required argument `action`".into());
    }
    let query: crate::query::Query =
        serde_json::from_value(arguments).map_err(|e| e.to_string())?;
    query.validate().map_err(|e| e.to_string())?;
    Ok(ExploreArgs { query, format_json })
}

fn result(id: Value, value: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": value })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": error_value(code,message)
    })
}

fn error_value(code: i64, message: &str) -> Value {
    json!({ "code": code, "message": message.chars().take(1024).collect::<String>() })
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

    #[test]
    fn tool_text_is_bounded_even_when_arguments_are_large() {
        let server = unindexed_server();
        let response = server.handle(&json!({
            "jsonrpc": "2.0", "id": 6, "method": "tools/call",
            "params": {
                "name": EXPLORE_TOOL,
                "arguments": { "action": "x".repeat(70_000) }
            }
        }));
        let text = serde_json::to_string(&response).expect("response");
        assert!(text.len() <= 65_536, "response was {} bytes", text.len());
        assert_eq!(response["error"]["code"], json!(-32602));
    }
}
