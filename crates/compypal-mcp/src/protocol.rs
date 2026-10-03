//! MCP over JSON-RPC 2.0: just the parts a tool server needs.

use serde_json::{Value, json};

/// Runs a tool call somewhere that can touch the app, and waits for it.
pub trait Dispatch: Send + Sync + 'static {
    fn call_tool(&self, name: &str, args: Value) -> Result<String, String>;
}

const INSTRUCTIONS: &str = "compypal is a MIDI composer. The user plays ideas into it from a \
controller; each take is kept raw as a session and laid down as a clip. Your job is usually to \
clean takes up without losing what the player meant, and to help compose and arrange. Read \
before writing: get_project, then get_session or get_figures. Positions are bar.beat.tick \
(one-based, 960 ticks per beat); pitches are names like C4 (= MIDI 60). Every edit is undoable, \
and play/audition let the user hear changes. Prefer musical units (figures, chords) over \
note-by-note edits when you can.";

/// Handles one JSON-RPC message. Returns the response, or `None` for a
/// notification.
pub fn handle(msg: &Value, dispatch: &dyn Dispatch) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let id = id?; // notifications (initialized, cancelled, ide_connected) need no answer
    let result = match method {
        "initialize" => {
            let version = msg
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2025-06-18");
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "compypal", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": crate::tools::list()})),
        "tools/call" => {
            let name = msg.pointer("/params/name").and_then(Value::as_str).unwrap_or("");
            let args = msg.pointer("/params/arguments").cloned().unwrap_or(json!({}));
            let (text, is_error) = match dispatch.call_tool(name, args) {
                Ok(t) => (t, false),
                Err(e) => (e, true),
            };
            Ok(json!({"content": [{"type": "text", "text": text}], "isError": is_error}))
        }
        "resources/list" => Ok(json!({"resources": []})),
        "prompts/list" => Ok(json!({"prompts": []})),
        other => Err(json!({"code": -32601, "message": format!("method not found: {other}")})),
    };
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
    })
}

/// Handles a single message or a batch.
pub fn handle_body(body: &Value, dispatch: &dyn Dispatch) -> Option<Value> {
    match body {
        Value::Array(batch) => {
            let out: Vec<Value> = batch.iter().filter_map(|m| handle(m, dispatch)).collect();
            (!out.is_empty()).then_some(Value::Array(out))
        }
        one => handle(one, dispatch),
    }
}
