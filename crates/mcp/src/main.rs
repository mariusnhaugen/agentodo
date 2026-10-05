//! An MCP server exposing agentodo over stdio.
//!
//! This is a small hand-rolled implementation of the MCP stdio transport:
//! newline-delimited JSON-RPC 2.0 messages on stdin/stdout. Every tool is a thin
//! adapter over `agentodo_core` (see `tools.rs`).

mod tools;

use std::io::{self, BufRead, Write};

use serde_json::{json, Value};

const PROTOCOL_VERSION: &str = "2024-11-05";

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                // Parse error, and we have no id to attribute it to.
                write_message(
                    &mut stdout,
                    &error_response(Value::Null, -32700, "parse error"),
                )?;
                continue;
            }
        };

        // Requests carry an `id`; notifications do not and get no response.
        let id = request.get("id").cloned();
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        match (method, id) {
            ("initialize", Some(id)) => {
                if let Some(name) = params.pointer("/clientInfo/name").and_then(Value::as_str) {
                    tools::set_client_name(name);
                }
                write_message(&mut stdout, &success(id, initialize_result()))?;
            }
            ("tools/list", Some(id)) => {
                write_message(
                    &mut stdout,
                    &success(id, json!({ "tools": tools::definitions() })),
                )?;
            }
            ("tools/call", Some(id)) => {
                write_message(&mut stdout, &handle_tool_call(id, &params))?;
            }
            ("ping", Some(id)) => {
                write_message(&mut stdout, &success(id, json!({})))?;
            }
            // Notifications (e.g. notifications/initialized): acknowledge silently.
            (_, None) => {}
            // Unknown method with an id: report method-not-found.
            (_, Some(id)) => {
                write_message(&mut stdout, &error_response(id, -32601, "method not found"))?;
            }
        }
    }
    Ok(())
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "agentodo", "version": env!("CARGO_PKG_VERSION") }
    })
}

fn handle_tool_call(id: Value, params: &Value) -> Value {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let result = tools::call(name, &args);
    success(
        id,
        json!({
            "content": [ { "type": "text", "text": result.text } ],
            "isError": result.is_error
        }),
    )
}

fn success(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn write_message(out: &mut impl Write, message: &Value) -> io::Result<()> {
    // One JSON object per line; messages must not contain embedded newlines.
    writeln!(out, "{}", serde_json::to_string(message)?)?;
    out.flush()
}
