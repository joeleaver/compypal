//! The Agent Client Protocol: JSON-RPC 2.0, one message per line, over the
//! agent's stdin/stdout. compypal is the client: it starts the agent, opens
//! a session with compypal's MCP server attached, sends prompts, and
//! answers permission requests. It offers no file system or terminal of
//! its own; the agent uses its own tools for those.
//!
//! <https://agentclientprotocol.com>

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::claude::{exit_reason, kill_group, last_line_of, own_group};
use super::{Ask, Backend, Choice, ChoiceKind, Event, Options, SERVER, content_text, first_line, short_tool, tool_detail};

pub const PROTOCOL_VERSION: u64 = 1;

/// What a request of ours was, so its response can be read.
#[derive(Clone, Copy, Debug)]
enum Call {
    Initialize,
    NewSession,
    Prompt,
}

#[derive(Default)]
struct State {
    next: u64,
    pending: HashMap<u64, Call>,
    session: Option<String>,
    /// Prompts sent before the session was up.
    queued: Vec<Vec<Value>>,
    /// Permission requests waiting on the user: our key → the request id.
    asks: HashMap<String, Value>,
    /// The instructions go with the first prompt; ACP has no system prompt.
    briefed: bool,
    /// Tool calls that are compypal's own tools, by id.
    ours: std::collections::HashSet<String>,
    agent: String,
}

struct Conn {
    out: Mutex<ChildStdin>,
    state: Mutex<State>,
    opts: Options,
}

/// With `COMPYPAL_AGENT_LOG=<file>`, every message both ways is appended
/// there: the way to see what an agent actually says.
fn log(dir: &str, line: &str) {
    if let Some(path) = std::env::var_os("COMPYPAL_AGENT_LOG")
        && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path)
    {
        let _ = writeln!(f, "{dir} {line}");
    }
}

impl Conn {
    fn write(&self, v: Value) -> std::io::Result<()> {
        log(">", &v.to_string());
        let mut w = self.out.lock().unwrap();
        writeln!(w, "{v}")?;
        w.flush()
    }

    fn request(&self, method: &str, params: Value, call: Call) -> std::io::Result<()> {
        let id = {
            let mut s = self.state.lock().unwrap();
            s.next += 1;
            let id = s.next;
            s.pending.insert(id, call);
            id
        };
        self.write(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
    }

    fn notify(&self, method: &str, params: Value) -> std::io::Result<()> {
        self.write(json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    fn respond(&self, id: &Value, result: Value) -> std::io::Result<()> {
        self.write(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    fn new_session(&self, http: bool) -> std::io::Result<()> {
        let o = &self.opts;
        let servers = match (&o.mcp_stdio, http) {
            (_, true) => json!([{"type": "http", "name": SERVER, "url": o.mcp_url, "headers": []}]),
            // Every ACP agent must take a stdio server.
            (Some((command, args)), false) => json!([{"name": SERVER, "command": command, "args": args, "env": []}]),
            (None, false) => json!([]),
        };
        self.request("session/new", json!({"cwd": o.cwd, "mcpServers": servers}), Call::NewSession)
    }

    fn prompt(&self, session: &str, blocks: Vec<Value>) -> std::io::Result<()> {
        self.request("session/prompt", json!({"sessionId": session, "prompt": blocks}), Call::Prompt)
    }

    /// One message from the agent.
    fn handle(&self, msg: Value, sink: &dyn Fn(Event)) {
        let method = msg["method"].as_str();
        match (method, msg.get("id")) {
            (Some("session/update"), None) => {
                let u = &msg["params"]["update"];
                if u["sessionUpdate"] == "tool_call" && [&u["name"], &u["title"]].iter().any(|n| n.as_str().is_some_and(super::is_ours)) {
                    self.state.lock().unwrap().ours.insert(u["toolCallId"].as_str().unwrap_or_default().to_string());
                }
                for e in update_events(u) {
                    sink(e);
                }
            }
            (Some("session/request_permission"), Some(id)) => {
                let p = &msg["params"];
                let call = &p["toolCall"];
                let ours = self.state.lock().unwrap().ours.contains(call["toolCallId"].as_str().unwrap_or_default())
                    || [&call["name"], &call["title"]].iter().any(|n| n.as_str().is_some_and(super::is_ours));
                let ask = permission_ask(id, p);
                // compypal's own tools only do what the user could do in the
                // editor, and undo: say yes without asking.
                let allow = ask.options.iter().find(|o| o.kind == ChoiceKind::AllowOnce).or(ask.options.iter().find(|o| o.kind.allows()));
                match (ours, allow) {
                    (true, Some(o)) => {
                        let _ = self.respond(id, json!({"outcome": {"outcome": "selected", "optionId": o.id}}));
                    }
                    _ => {
                        self.state.lock().unwrap().asks.insert(ask.request.clone(), id.clone());
                        sink(Event::Ask(ask));
                    }
                }
            }
            // We offer no file system, terminal or anything else.
            (Some(m), Some(id)) => {
                let error = json!({"code": -32601, "message": format!("compypal doesn't offer {m}")});
                let _ = self.write(json!({"jsonrpc": "2.0", "id": id, "error": error}));
            }
            (Some(_), None) => {}
            (None, Some(id)) => {
                let call = id.as_u64().and_then(|n| self.state.lock().unwrap().pending.remove(&n));
                if let Some(call) = call {
                    self.answered(call, &msg, sink);
                }
            }
            (None, None) => {}
        }
    }

    /// A response to one of our requests.
    fn answered(&self, call: Call, msg: &Value, sink: &dyn Fn(Event)) {
        let error = msg.get("error").map(|e| {
            let what = e["message"].as_str().unwrap_or("error");
            // "Internal error" alone says nothing; the details usually do.
            match e["data"]["details"].as_str().or(e["data"].as_str()) {
                Some(d) if !d.is_empty() => format!("{what}: {d}"),
                _ => what.to_string(),
            }
        });
        let r = &msg["result"];
        match (call, error) {
            (Call::Initialize, None) => {
                let info = &r["agentInfo"];
                let name = info["title"].as_str().or(info["name"].as_str()).unwrap_or(&self.opts.agent.name).to_string();
                self.state.lock().unwrap().agent = name;
                let http = r["agentCapabilities"]["mcpCapabilities"]["http"].as_bool() == Some(true);
                if let Err(e) = self.new_session(http) {
                    sink(Event::Exited(e.to_string()));
                }
            }
            (Call::NewSession, None) => {
                let session = r["sessionId"].as_str().unwrap_or_default().to_string();
                let (agent, queued) = {
                    let mut s = self.state.lock().unwrap();
                    s.session = Some(session.clone());
                    (s.agent.clone(), std::mem::take(&mut s.queued))
                };
                sink(Event::Ready { session: session.clone(), agent });
                for blocks in queued {
                    if let Err(e) = self.prompt(&session, blocks) {
                        sink(Event::Exited(e.to_string()));
                    }
                }
            }
            (Call::Prompt, None) => sink(Event::Done { error: stop_problem(r["stopReason"].as_str()), cost: None }),
            (Call::Prompt, Some(e)) => sink(Event::Done { error: Some(e), cost: None }),
            (Call::Initialize | Call::NewSession, Some(e)) => {
                let name = &self.opts.agent.name;
                let auth = msg["error"]["code"] == -32000 || e.to_lowercase().contains("auth");
                sink(Event::Exited(if auth {
                    format!("{name} needs you to sign in first: run `{}` in a terminal and log in, then start again.", self.opts.agent.program)
                } else {
                    format!("{name} couldn't start a session: {e}")
                }));
            }
        }
    }
}

/// What a turn's stop reason says to the user, if anything.
pub fn stop_problem(reason: Option<&str>) -> Option<String> {
    match reason {
        Some("max_tokens") => Some("The reply hit the agent's length limit.".into()),
        Some("max_turn_requests") => Some("The agent hit its limit on steps for one turn.".into()),
        Some("refusal") => Some("The agent declined to continue.".into()),
        _ => None,
    }
}

/// A `session/update` as events.
pub fn update_events(u: &Value) -> Vec<Event> {
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    match u["sessionUpdate"].as_str().unwrap_or_default() {
        "agent_message_chunk" if u["content"]["type"] == "text" => vec![Event::Delta(s(&u["content"]["text"]))],
        "tool_call" => {
            let id = s(&u["toolCallId"]);
            let (name, detail) = describe_call(u);
            let mut out = vec![Event::Tool { id: id.clone(), name, detail }];
            out.extend(finished(&id, u));
            out
        }
        "tool_call_update" => finished(&s(&u["toolCallId"]), u).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// A tool call's name and one line about it, from whatever the agent gave.
fn describe_call(u: &Value) -> (String, String) {
    let title = u["title"].as_str().unwrap_or_default();
    let raw = &u["rawInput"];
    match u["name"].as_str() {
        Some(n) => {
            let detail = if title.is_empty() || title == n { tool_detail(n, raw) } else { first_line(title) };
            (short_tool(n), detail)
        }
        None => {
            // A title like "Run ls exports" already says what the input would.
            let said = raw.as_object().is_some_and(|m| m.values().all(|v| v.as_str().is_some_and(|v| title.contains(v))));
            (first_line(&short_tool(title)), if said { String::new() } else { tool_detail("", raw) })
        }
    }
}

fn finished(id: &str, u: &Value) -> Option<Event> {
    let ok = match u["status"].as_str() {
        Some("completed") => true,
        Some("failed") => false,
        _ => return None,
    };
    let text = match content_text(&u["content"]) {
        t if !t.trim().is_empty() => t,
        _ => match &u["rawOutput"] {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => content_text(&other["content"]),
        },
    };
    Some(Event::ToolDone { id: id.into(), ok, detail: first_line(&text) })
}

/// A `session/request_permission` as a card.
pub fn permission_ask(id: &Value, params: &Value) -> Ask {
    let call = &params["toolCall"];
    let (tool, detail) = describe_call(call);
    let options = params["options"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|o| Choice {
            id: o["optionId"].as_str().unwrap_or_default().into(),
            label: o["name"].as_str().unwrap_or_default().into(),
            kind: ChoiceKind::parse(o["kind"].as_str().unwrap_or_default()),
        })
        .collect();
    Ask { request: id.to_string(), tool, detail, options, extra: id.clone() }
}

/// A running ACP agent.
pub struct Acp {
    conn: Arc<Conn>,
    child: Arc<Mutex<Child>>,
}

impl Acp {
    pub fn spawn(opts: &Options, sink: impl Fn(Event) + Send + 'static) -> std::io::Result<Self> {
        let mut child = own_group(&mut Command::new(&opts.agent.program))
            .args(&opts.agent.args)
            .current_dir(&opts.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().expect("piped");
        let tail = last_line_of(child.stderr.take().expect("piped"));
        let conn = Arc::new(Conn {
            out: Mutex::new(child.stdin.take().expect("piped")),
            state: Mutex::new(State::default()),
            opts: opts.clone(),
        });
        let child = Arc::new(Mutex::new(child));
        {
            let (conn, child, name) = (conn.clone(), child.clone(), opts.agent.name.clone());
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    log("<", &line);
                    // Agents sometimes log to stdout; only JSON is protocol.
                    if let Ok(msg) = serde_json::from_str::<Value>(&line) {
                        conn.handle(msg, &sink);
                    }
                }
                let why = exit_reason(&child, &tail, &name);
                log("!", &why);
                sink(Event::Exited(why));
            });
        }
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
            "clientInfo": {"name": "compypal", "title": "compypal", "version": env!("CARGO_PKG_VERSION")}
        });
        conn.request("initialize", params, Call::Initialize)?;
        Ok(Self { conn, child })
    }
}

impl Backend for Acp {
    fn send(&self, text: &str, context: Option<&str>) -> std::io::Result<()> {
        let mut blocks = Vec::new();
        let session = {
            let mut s = self.conn.state.lock().unwrap();
            if !std::mem::replace(&mut s.briefed, true) {
                blocks.push(json!({"type": "text", "text": format!("<instructions>\n{}\n</instructions>", self.conn.opts.instructions)}));
            }
            if let Some(c) = context {
                blocks.push(json!({"type": "text", "text": c}));
            }
            blocks.push(json!({"type": "text", "text": text}));
            match s.session.clone() {
                Some(id) => id,
                None => {
                    s.queued.push(blocks);
                    return Ok(());
                }
            }
        };
        self.conn.prompt(&session, blocks)
    }

    fn interrupt(&self) -> std::io::Result<()> {
        let (session, asks) = {
            let mut s = self.conn.state.lock().unwrap();
            (s.session.clone(), std::mem::take(&mut s.asks))
        };
        // Questions still open are answered "cancelled", as the protocol asks.
        for id in asks.values() {
            self.conn.respond(id, json!({"outcome": {"outcome": "cancelled"}}))?;
        }
        match session {
            Some(id) => self.conn.notify("session/cancel", json!({"sessionId": id})),
            None => Ok(()),
        }
    }

    fn answer(&self, ask: &Ask, choice: &str) -> std::io::Result<()> {
        let id = self.conn.state.lock().unwrap().asks.remove(&ask.request);
        match id {
            Some(id) => self.conn.respond(&id, json!({"outcome": {"outcome": "selected", "optionId": choice}})),
            None => Ok(()),
        }
    }
}

impl Drop for Acp {
    fn drop(&mut self) {
        kill_group(&mut self.child.lock().unwrap());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Chat, Item, Status, ToolState};

    #[test]
    fn a_turn_from_session_updates() {
        let mut chat = Chat { status: Status::Working, ..Default::default() };
        let updates = [
            json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Checking "}}),
            json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "the chords."}}),
            json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "hmm"}}),
            json!({"sessionUpdate": "tool_call", "toolCallId": "c1", "title": "compypal_get_harmony",
                   "kind": "other", "status": "pending", "rawInput": {"bars": "1-4"}}),
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "in_progress"}),
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed",
                   "content": [{"type": "content", "content": {"type": "text", "text": "C | F | G | C"}}]}),
            json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "I IV V I."}}),
        ];
        for u in &updates {
            for e in update_events(u) {
                chat.apply(e);
            }
        }
        chat.apply(Event::Done { error: stop_problem(Some("end_turn")), cost: None });
        let items: Vec<&Item> = chat.items.iter().map(|(_, i)| i).collect();
        assert_eq!(items[0], &Item::Reply("Checking the chords.".into()));
        match items[1] {
            Item::Tool { name, detail, state, result, .. } => {
                assert_eq!((name.as_str(), detail.as_str()), ("get_harmony", "bars 1-4"));
                assert_eq!((*state, result.as_str()), (ToolState::Ok, "C | F | G | C"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(items[2], &Item::Reply("I IV V I.".into()));
        assert_eq!(chat.status, Status::Ready);
    }

    #[test]
    fn named_tools_keep_their_title() {
        let u = json!({"sessionUpdate": "tool_call", "toolCallId": "c", "title": "Read song.abc",
                       "name": "read_file", "status": "failed", "rawOutput": "no such file"});
        let events = update_events(&u);
        assert_eq!(events[0], Event::Tool { id: "c".into(), name: "read_file".into(), detail: "Read song.abc".into() });
        assert_eq!(events[1], Event::ToolDone { id: "c".into(), ok: false, detail: "no such file".into() });
    }

    #[test]
    fn permission_requests_carry_the_agents_options() {
        let params = json!({"sessionId": "s", "toolCall": {"toolCallId": "c", "title": "Run ls", "rawInput": {}},
            "options": [{"optionId": "y", "name": "Allow", "kind": "allow_once"},
                        {"optionId": "a", "name": "Always", "kind": "allow_always"},
                        {"optionId": "n", "name": "Reject", "kind": "reject_once"}]});
        let ask = permission_ask(&json!(7), &params);
        assert_eq!((ask.request.as_str(), ask.tool.as_str()), ("7", "Run ls"));
        let kinds: Vec<ChoiceKind> = ask.options.iter().map(|o| o.kind).collect();
        assert_eq!(kinds, [ChoiceKind::AllowOnce, ChoiceKind::AllowAlways, ChoiceKind::RejectOnce]);
        assert_eq!(ask.extra, json!(7));
    }

    #[test]
    fn titles_that_say_it_all_need_no_detail() {
        let u = json!({"sessionUpdate": "tool_call", "toolCallId": "c", "title": "Run ls exports", "rawInput": {"command": "ls exports"}});
        assert_eq!(update_events(&u)[0], Event::Tool { id: "c".into(), name: "Run ls exports".into(), detail: String::new() });
    }

    #[test]
    fn stop_reasons() {
        assert_eq!(stop_problem(Some("end_turn")), None);
        assert_eq!(stop_problem(Some("cancelled")), None);
        assert!(stop_problem(Some("refusal")).is_some());
    }
}
