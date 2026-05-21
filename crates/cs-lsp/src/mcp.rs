//! MCP server core (Phase 6).
//!
//! The Model Context Protocol lets coding harnesses — Claude Code/Desktop
//! and any MCP client — call CrabScheme code intelligence as *tools*.
//! This module is the protocol brain: a pure [`handle`] over JSON-RPC 2.0
//! [`Value`]s with no I/O of its own (the `crabscheme-mcp` binary owns the
//! newline-delimited stdio loop). Every tool is a thin wrapper over
//! [`crate::harness`], so MCP, the CLI, and the LSP all report identical
//! analysis.
//!
//! It implements the MCP lifecycle (`initialize` →
//! `notifications/initialized` → operation), `tools/list`, `tools/call`,
//! and `ping`. The protocol version is negotiated by echoing a client
//! version we recognize, otherwise advertising our latest.

use serde_json::{json, Value};

use crate::harness::{self, Pos};

/// MCP protocol versions this server understands. We echo the client's
/// requested version when it's one of these, else advertise the latest.
const SUPPORTED_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
const LATEST_VERSION: &str = "2025-06-18";
const SERVER_NAME: &str = "crabscheme-mcp";

/// A tool failure: either the tool name is unknown (a protocol-level
/// JSON-RPC error) or the tool ran but failed (reported in-band as an
/// `isError` tool result, per the MCP spec).
enum ToolError {
    Unknown,
    Exec(String),
}

/// Handle one JSON-RPC message. Returns `Some(response)` for requests and
/// `None` for notifications (which never get a reply).
pub fn handle(request: &Value) -> Option<Value> {
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(Value::Null);

    // No id ⇒ a notification (e.g. notifications/initialized): no response.
    // A JSON-RPC batch array also lands here (no top-level "id") and is
    // silently ignored — intentional, since MCP 2025-06-18 removed batching.
    let id = request.get("id").cloned()?;

    let result = match method {
        "initialize" => Ok(initialize(&params)),
        "tools/list" => Ok(json!({ "tools": tool_defs() })),
        "tools/call" => tools_call(&params),
        "ping" => Ok(json!({})),
        other => Err(error_obj(-32601, &format!("method not found: {other}"))),
    };

    Some(match result {
        Ok(value) => json!({ "jsonrpc": "2.0", "id": id, "result": value }),
        Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    })
}

/// A full JSON-RPC parse-error response (the binary emits this when a
/// line off stdin isn't valid JSON).
pub fn parse_error() -> Value {
    json!({ "jsonrpc": "2.0", "id": null, "error": error_obj(-32700, "parse error") })
}

fn initialize(params: &Value) -> Value {
    let version = match params.get("protocolVersion").and_then(Value::as_str) {
        Some(v) if SUPPORTED_VERSIONS.contains(&v) => v,
        _ => LATEST_VERSION,
    };
    json!({
        "protocolVersion": version,
        // The tool list is static, so we never emit tools/list_changed.
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
        "instructions": "CrabScheme code intelligence. cs_diagnostics checks a .scm file \
            for parse/expand errors; cs_symbols returns its outline; \
            cs_definition/cs_references/cs_hover act on the identifier at a 1-based \
            line/col; cs_format reindents; cs_workspace_symbols searches defines across a \
            directory. Each source tool takes a file 'path' or inline 'text'."
    })
}

/// JSON Schema shared by the source-accepting tools.
fn source_props() -> Value {
    json!({
        "path": { "type": "string", "description": "Path to a .scm file to analyze." },
        "text": { "type": "string", "description": "Inline source to analyze instead of a file." }
    })
}

/// `path`/`text` plus a required 1-based `line`/`col`.
fn pos_props() -> Value {
    let mut props = source_props();
    let obj = props.as_object_mut().unwrap();
    obj.insert(
        "line".into(),
        json!({ "type": "integer", "description": "1-based line." }),
    );
    obj.insert(
        "col".into(),
        json!({ "type": "integer", "description": "1-based column (UTF-16 units)." }),
    );
    props
}

fn tool(name: &str, description: &str, properties: Value, required: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required }
    })
}

fn tool_defs() -> Value {
    json!([
        tool(
            "cs_diagnostics",
            "Parse + expand a CrabScheme source and return any errors (JSON: severity, message, 1-based range). Empty when well-formed.",
            source_props(),
            json!([]),
        ),
        tool(
            "cs_symbols",
            "Return the document outline — every top-level and nested define — as JSON (name, kind, 1-based range).",
            source_props(),
            json!([]),
        ),
        tool(
            "cs_definition",
            "Return the definition site (1-based range) of the identifier at line/col, if it is defined in this source.",
            pos_props(),
            json!(["line", "col"]),
        ),
        tool(
            "cs_references",
            "Return every reference (definition included) to the identifier at line/col, as 1-based ranges.",
            pos_props(),
            json!(["line", "col"]),
        ),
        tool(
            "cs_hover",
            "Return hover documentation (builtin signature, or 'defined at line N') for the identifier at line/col.",
            pos_props(),
            json!(["line", "col"]),
        ),
        tool(
            "cs_format",
            "Reformat a CrabScheme source with canonical indentation; returns the formatted source text.",
            source_props(),
            json!([]),
        ),
        tool(
            "cs_workspace_symbols",
            "Search every .scm file under a directory for defines whose name contains the query (case-insensitive; empty matches all).",
            json!({
                "root": { "type": "string", "description": "Directory to scan." },
                "query": { "type": "string", "description": "Substring to match (default: all)." }
            }),
            json!(["root"]),
        ),
    ])
}

fn tools_call(params: &Value) -> Result<Value, Value> {
    use std::panic::{catch_unwind, AssertUnwindSafe};

    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match catch_unwind(AssertUnwindSafe(|| dispatch_tool(&name, &args))) {
        Ok(Ok(text)) => Ok(tool_text(text, false)),
        Ok(Err(ToolError::Exec(msg))) => Ok(tool_text(msg, true)),
        Ok(Err(ToolError::Unknown)) => Err(error_obj(-32602, &format!("unknown tool: {name}"))),
        Err(_) => Ok(tool_text(
            "internal error analyzing input".to_string(),
            true,
        )),
    }
}

fn dispatch_tool(name: &str, args: &Value) -> Result<String, ToolError> {
    match name {
        "cs_diagnostics" => {
            let (n, t) = resolve_source(args)?;
            Ok(to_json(&harness::check(&n, &t)))
        }
        "cs_symbols" => {
            let (n, t) = resolve_source(args)?;
            Ok(to_json(&harness::symbols(&n, &t)))
        }
        "cs_definition" => {
            let (n, t) = resolve_source(args)?;
            Ok(to_json(&harness::definition(&n, &t, pos(args)?)))
        }
        "cs_references" => {
            let (n, t) = resolve_source(args)?;
            Ok(to_json(&harness::references(&n, &t, pos(args)?)))
        }
        "cs_hover" => {
            let (n, t) = resolve_source(args)?;
            Ok(to_json(&harness::hover(&n, &t, pos(args)?)))
        }
        "cs_format" => {
            let (_, t) = resolve_source(args)?;
            Ok(harness::format(&t))
        }
        "cs_workspace_symbols" => {
            let root = args
                .get("root")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::Exec("'root' is required".to_string()))?;
            let query = args.get("query").and_then(Value::as_str).unwrap_or("");
            Ok(to_json(&harness::workspace_symbols(
                std::path::Path::new(root),
                query,
            )))
        }
        _ => Err(ToolError::Unknown),
    }
}

/// Resolve a tool's source: inline `text` wins, else read `path`.
fn resolve_source(args: &Value) -> Result<(String, String), ToolError> {
    if let Some(text) = args.get("text").and_then(Value::as_str) {
        let name = args
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("<input>")
            .to_string();
        return Ok((name, text.to_string()));
    }
    if let Some(path) = args.get("path").and_then(Value::as_str) {
        return std::fs::read_to_string(path)
            .map(|t| (path.to_string(), t))
            .map_err(|e| ToolError::Exec(format!("cannot read {path}: {e}")));
    }
    Err(ToolError::Exec("provide 'path' or 'text'".to_string()))
}

fn pos(args: &Value) -> Result<Pos, ToolError> {
    match (
        args.get("line").and_then(Value::as_u64),
        args.get("col").and_then(Value::as_u64),
    ) {
        (Some(line), Some(col)) => Ok(Pos::new(line as u32, col as u32)),
        _ => Err(ToolError::Exec(
            "line and col are required (1-based integers)".to_string(),
        )),
    }
}

fn to_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".to_string())
}

fn tool_text(text: String, is_error: bool) -> Value {
    json!({ "content": [ { "type": "text", "text": text } ], "isError": is_error })
}

fn error_obj(code: i64, message: &str) -> Value {
    json!({ "code": code, "message": message })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(id: i64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    #[test]
    fn initialize_negotiates_and_advertises_tools() {
        let r = handle(&req(
            1,
            "initialize",
            json!({ "protocolVersion": "2025-03-26" }),
        ))
        .expect("response");
        assert_eq!(
            r["result"]["protocolVersion"], "2025-03-26",
            "should echo known version"
        );
        assert_eq!(r["result"]["serverInfo"]["name"], "crabscheme-mcp");
        assert!(r["result"]["capabilities"]["tools"].is_object());

        // Unknown version → fall back to latest.
        let r = handle(&req(
            1,
            "initialize",
            json!({ "protocolVersion": "1999-01-01" }),
        ))
        .unwrap();
        assert_eq!(r["result"]["protocolVersion"], LATEST_VERSION);
    }

    #[test]
    fn notification_gets_no_response() {
        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(handle(&note).is_none());
    }

    #[test]
    fn tools_list_has_all_seven() {
        let r = handle(&req(2, "tools/list", json!({}))).unwrap();
        let tools = r["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 7, "{tools:?}");
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for expected in [
            "cs_diagnostics",
            "cs_symbols",
            "cs_definition",
            "cs_references",
            "cs_hover",
            "cs_format",
            "cs_workspace_symbols",
        ] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
        // Every tool advertises an object inputSchema.
        for t in tools {
            assert_eq!(t["inputSchema"]["type"], "object", "{t:?}");
        }
    }

    #[test]
    fn call_diagnostics_on_inline_text() {
        let r = handle(&req(
            3,
            "tools/call",
            json!({ "name": "cs_diagnostics", "arguments": { "text": "(+ 1 2" } }),
        ))
        .unwrap();
        assert_eq!(r["result"]["isError"], false);
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("\"severity\": \"error\""), "{text}");
        assert!(text.contains("\"line\": 1"), "1-based: {text}");
    }

    #[test]
    fn call_format_returns_source() {
        let r = handle(&req(
            4,
            "tools/call",
            json!({ "name": "cs_format", "arguments": { "text": "(a\n(b))" } }),
        ))
        .unwrap();
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert_eq!(text, "(a\n  (b))");
    }

    #[test]
    fn call_definition_round_trips() {
        let r = handle(&req(
            5,
            "tools/call",
            json!({
                "name": "cs_definition",
                "arguments": { "text": "(define (f x) x)\n(f 1)", "line": 2, "col": 2 }
            }),
        ))
        .unwrap();
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("\"line\": 1"), "definition on line 1: {text}");
    }

    #[test]
    fn missing_source_is_in_band_error() {
        let r = handle(&req(
            6,
            "tools/call",
            json!({ "name": "cs_symbols", "arguments": {} }),
        ))
        .unwrap();
        assert_eq!(r["result"]["isError"], true);
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("path") || text.contains("text"), "{text}");
    }

    #[test]
    fn unknown_tool_is_jsonrpc_error() {
        let r = handle(&req(
            7,
            "tools/call",
            json!({ "name": "nope", "arguments": {} }),
        ))
        .unwrap();
        assert_eq!(r["error"]["code"], -32602, "{r}");
    }

    #[test]
    fn unknown_method_is_jsonrpc_error() {
        let r = handle(&req(8, "frobnicate", json!({}))).unwrap();
        assert_eq!(r["error"]["code"], -32601, "{r}");
    }

    #[test]
    fn missing_position_args_report_in_band() {
        let r = handle(&req(
            9,
            "tools/call",
            json!({ "name": "cs_hover", "arguments": { "text": "(cons 1 2)" } }),
        ))
        .unwrap();
        assert_eq!(r["result"]["isError"], true);
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("line and col"), "{text}");
    }
}
