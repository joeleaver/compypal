//! A scripted ACP agent for testing the agent panel without a model. It
//! streams a reply, calls compypal's `get_harmony` tool through whichever
//! MCP server the panel gave it; if the prompt mentions "tempo", it asks
//! permission to set the tempo to 110 BPM (which the panel grants itself:
//! it's compypal's own tool), and if it mentions "shell", it asks to run a
//! command (which the user has to answer).
//!
//! cargo build -p compypal-mcp --example fake_agent
//! COMPYPAL_AGENT=target/debug/examples/fake_agent cargo run -p compypal
//!
//! With `FAKE_AGENT_NO_HTTP=1` it claims not to speak MCP over HTTP, so the
//! panel hands it `compypal --mcp-stdio` instead.

use std::io::{BufRead, BufReader, Write};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

/// How this agent reaches compypal's tools.
enum Mcp {
    Http(String),
    Stdio(ChildStdin, BufReader<ChildStdout>),
    None,
}

impl Mcp {
    fn call(&mut self, name: &str, args: Value) -> String {
        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}});
        let reply = match self {
            Mcp::Http(url) => compypal_mcp::agent::bridge::post(url, &req.to_string()).ok().flatten(),
            Mcp::Stdio(input, output) => {
                let _ = writeln!(input, "{req}");
                let _ = input.flush();
                let mut line = String::new();
                output.read_line(&mut line).ok().map(|_| line)
            }
            Mcp::None => None,
        };
        let v: Value = reply.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or_default();
        v["result"]["content"][0]["text"].as_str().unwrap_or("(no answer)").to_string()
    }
}

fn send(v: Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

fn update(session: &str, u: Value) {
    send(json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": session, "update": u}}));
}

fn say(session: &str, text: &str) {
    for word in text.split_inclusive(' ') {
        update(session, json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": word}}));
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
}

fn main() {
    let http = std::env::var_os("FAKE_AGENT_NO_HTTP").is_none();
    let mut mcp = Mcp::None;
    let mut lines = std::io::stdin().lock().lines().map_while(Result::ok);
    while let Some(line) = lines.next() {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let id = msg["id"].clone();
        match msg["method"].as_str().unwrap_or_default() {
            "initialize" => send(json!({"jsonrpc": "2.0", "id": id, "result": {
                "protocolVersion": 1,
                "agentCapabilities": {"mcpCapabilities": {"http": http}},
                "agentInfo": {"name": "fake-agent", "title": "Fake agent", "version": "0"},
                "authMethods": []
            }})),
            "session/new" => {
                let server = &msg["params"]["mcpServers"][0];
                mcp = match server["type"].as_str() {
                    Some("http") => Mcp::Http(server["url"].as_str().unwrap_or_default().to_string()),
                    _ => match server["command"].as_str() {
                        Some(cmd) => {
                            let args: Vec<String> = server["args"].as_array().into_iter().flatten().filter_map(|a| a.as_str().map(String::from)).collect();
                            // Lives as long as this agent; its stdin closing ends it.
                            #[allow(clippy::zombie_processes)]
                            let mut child = Command::new(cmd).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
                            Mcp::Stdio(child.stdin.take().unwrap(), BufReader::new(child.stdout.take().unwrap()))
                        }
                        None => Mcp::None,
                    },
                };
                send(json!({"jsonrpc": "2.0", "id": id, "result": {"sessionId": "fake-1"}}));
            }
            "session/prompt" => {
                let session = "fake-1";
                let text: String = msg["params"]["prompt"].as_array().into_iter().flatten().filter_map(|b| b["text"].as_str()).collect();
                say(session, "Let me look at the harmony. ");
                update(session, json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "compypal_get_harmony",
                                       "kind": "read", "status": "in_progress", "rawInput": {"from_bar": 1, "to_bar": 4}}));
                let harmony = mcp.call("get_harmony", json!({"from_bar": 1, "to_bar": 4}));
                update(session, json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed",
                                       "content": [{"type": "content", "content": {"type": "text", "text": harmony.clone()}}]}));
                let chords: Vec<&str> = harmony.lines().skip(1).filter_map(|l| l.split_whitespace().nth(2)).collect();
                say(session, &format!("The chords are {}. ", chords.join(" – ")));
                let mut stop = "end_turn";
                if text.contains("tempo") {
                    update(session, json!({"sessionUpdate": "tool_call", "toolCallId": "t2", "title": "compypal_set_project",
                                           "kind": "edit", "status": "pending", "rawInput": {"bpm": 110}}));
                    send(json!({"jsonrpc": "2.0", "id": 900, "method": "session/request_permission", "params": {
                        "sessionId": session,
                        "toolCall": {"toolCallId": "t2", "title": "Set the tempo to 110 BPM", "rawInput": {"bpm": 110}},
                        "options": [{"optionId": "yes", "name": "Allow", "kind": "allow_once"},
                                    {"optionId": "no", "name": "Reject", "kind": "reject_once"}]
                    }}));
                    // Wait for the answer, or a cancel.
                    let mut allowed = false;
                    for line in lines.by_ref() {
                        let Ok(m) = serde_json::from_str::<Value>(&line) else { continue };
                        if m["method"] == "session/cancel" {
                            stop = "cancelled";
                            continue;
                        }
                        if m["id"] == 900 {
                            allowed = m["result"]["outcome"]["optionId"] == "yes";
                            break;
                        }
                    }
                    if stop != "cancelled" && allowed {
                        let r = mcp.call("set_project", json!({"bpm": 110}));
                        update(session, json!({"sessionUpdate": "tool_call_update", "toolCallId": "t2", "status": "completed",
                                               "content": [{"type": "content", "content": {"type": "text", "text": r}}]}));
                        say(session, "Tempo is now 110 BPM.");
                    } else if stop != "cancelled" {
                        update(session, json!({"sessionUpdate": "tool_call_update", "toolCallId": "t2", "status": "failed",
                                               "content": [{"type": "content", "content": {"type": "text", "text": "Rejected"}}]}));
                        say(session, "Leaving the tempo alone.");
                    }
                }
                if stop != "cancelled" && text.contains("shell") {
                    update(session, json!({"sessionUpdate": "tool_call", "toolCallId": "t3", "title": "Run ls exports",
                                           "kind": "execute", "status": "pending", "rawInput": {"command": "ls exports"}}));
                    send(json!({"jsonrpc": "2.0", "id": 901, "method": "session/request_permission", "params": {
                        "sessionId": session,
                        "toolCall": {"toolCallId": "t3", "title": "Run ls exports", "rawInput": {"command": "ls exports"}},
                        "options": [{"optionId": "yes", "name": "Allow", "kind": "allow_once"},
                                    {"optionId": "no", "name": "Reject", "kind": "reject_once"}]
                    }}));
                    let mut allowed = false;
                    for line in lines.by_ref() {
                        let Ok(m) = serde_json::from_str::<Value>(&line) else { continue };
                        if m["method"] == "session/cancel" {
                            stop = "cancelled";
                            continue;
                        }
                        if m["id"] == 901 {
                            allowed = m["result"]["outcome"]["optionId"] == "yes";
                            break;
                        }
                    }
                    let status = if allowed { "completed" } else { "failed" };
                    update(session, json!({"sessionUpdate": "tool_call_update", "toolCallId": "t3", "status": status}));
                }
                send(json!({"jsonrpc": "2.0", "id": id, "result": {"stopReason": stop}}));
            }
            _ if !id.is_null() => send(json!({"jsonrpc": "2.0", "id": id, "result": {}})),
            _ => {}
        }
    }
}
