//! Claude Code over its own protocol, as the VS Code extension runs it:
//! `claude -p` speaks newline-delimited JSON on stdin/stdout
//! (`stream-json`), and asks the host for permission over the same pipe
//! (`--permission-prompt-tool stdio`). No adapter needed.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::{Ask, Backend, Choice, ChoiceKind, Event, Options, SERVER, content_text, first_line, short_tool, tool_detail};

/// Tools that are plumbing rather than work: loading tool schemas, keeping
/// a todo list.
const HIDDEN_TOOLS: [&str; 3] = ["ToolSearch", "TodoWrite", "TaskOutput"];

/// One line of Claude Code's output as events.
pub fn parse(line: &str) -> Vec<Event> {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return Vec::new() };
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    match v["type"].as_str().unwrap_or_default() {
        "system" if v["subtype"] == "init" => {
            vec![Event::Ready { session: s(&v["session_id"]), agent: format!("Claude Code · {}", s(&v["model"])) }]
        }
        "stream_event" => {
            let e = &v["event"];
            // Subagents stream too; only the main thread is shown.
            if e["type"] == "content_block_delta" && e["delta"]["type"] == "text_delta" && v["parent_tool_use_id"].is_null() {
                vec![Event::Delta(s(&e["delta"]["text"]))]
            } else {
                Vec::new()
            }
        }
        "assistant" if v["parent_tool_use_id"].is_null() => blocks(&v["message"]["content"])
            .filter_map(|b| match b["type"].as_str()? {
                "text" => Some(Event::Text(s(&b["text"]))).filter(|e| *e != Event::Text(String::new())),
                // Claude Code's own housekeeping, not something the user asked for.
                "tool_use" if HIDDEN_TOOLS.contains(&b["name"].as_str()?) => None,
                "tool_use" => Some(Event::Tool {
                    id: s(&b["id"]),
                    name: short_tool(b["name"].as_str()?),
                    detail: tool_detail(b["name"].as_str()?, &b["input"]),
                }),
                _ => None,
            })
            .collect(),
        "user" if v["parent_tool_use_id"].is_null() => blocks(&v["message"]["content"])
            .filter(|b| b["type"] == "tool_result")
            .map(|b| Event::ToolDone {
                id: s(&b["tool_use_id"]),
                ok: !b["is_error"].as_bool().unwrap_or(false),
                detail: first_line(&content_text(&b["content"])),
            })
            .collect(),
        "control_request" if v["request"]["subtype"] == "can_use_tool" => {
            let r = &v["request"];
            let tool = s(&r["tool_name"]);
            let always = r["permission_suggestions"].as_array().is_some_and(|a| !a.is_empty());
            let choice = |id: &str, label: &str, kind| Choice { id: id.into(), label: label.into(), kind };
            let mut options = vec![choice("allow", "Allow", ChoiceKind::AllowOnce)];
            if always {
                options.push(choice("always", "Always allow", ChoiceKind::AllowAlways));
            }
            options.push(choice("deny", "Deny", ChoiceKind::RejectOnce));
            vec![Event::Ask(Ask {
                request: s(&v["request_id"]),
                detail: match r["description"].as_str() {
                    Some(d) if !d.is_empty() => d.to_string(),
                    _ => tool_detail(&tool, &r["input"]),
                },
                tool: short_tool(&tool),
                options,
                extra: json!({"input": r["input"], "always": r["permission_suggestions"]}),
            })]
        }
        "result" => {
            let error = (v["is_error"].as_bool() == Some(true) || v["subtype"].as_str().is_some_and(|t| t.starts_with("error")))
                .then(|| match v["result"].as_str() {
                    Some(r) if !r.is_empty() => r.to_string(),
                    _ => s(&v["subtype"]).replace('_', " "),
                });
            vec![Event::Done { error, cost: v["total_cost_usd"].as_f64() }]
        }
        _ => Vec::new(),
    }
}

fn blocks(content: &Value) -> impl Iterator<Item = &Value> {
    content.as_array().into_iter().flatten()
}

/// A running Claude Code session.
pub struct ClaudeCode {
    stdin: Arc<Mutex<ChildStdin>>,
    child: Arc<Mutex<Child>>,
    next_request: Mutex<u64>,
}

impl ClaudeCode {
    pub fn spawn(opts: &Options, sink: impl Fn(Event) + Send + 'static) -> std::io::Result<Self> {
        let mcp = json!({"mcpServers": {SERVER: {"type": "http", "url": opts.mcp_url}}}).to_string();
        let mut cmd = Command::new(&opts.agent.program);
        cmd.args(&opts.agent.args)
            .args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose"])
            .args(["--include-partial-messages", "--permission-prompt-tool", "stdio"])
            // Only compypal's tools: the user's other servers don't belong in
            // a music app, and compypal's own `.mcp.json` would name it twice.
            .args(["--strict-mcp-config", "--mcp-config", &mcp])
            .args(["--allowedTools", &format!("mcp__{SERVER}")])
            .args(["--append-system-prompt", &opts.instructions]);
        if let Some(m) = &opts.model {
            cmd.args(["--model", m]);
        }
        let mut child = own_group(&mut cmd)
            .current_dir(&opts.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let stdin = Arc::new(Mutex::new(child.stdin.take().expect("piped")));
        let child = Arc::new(Mutex::new(child));

        let tail = last_line_of(stderr);
        {
            let child = child.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    for e in parse(&line) {
                        sink(e);
                    }
                }
                sink(Event::Exited(exit_reason(&child, &tail, "Claude Code")));
            });
        }
        let claude = Self { stdin, child, next_request: Mutex::new(0) };
        claude.control(json!({"subtype": "initialize"}))?;
        Ok(claude)
    }

    fn write(&self, v: Value) -> std::io::Result<()> {
        let mut w = self.stdin.lock().unwrap();
        writeln!(w, "{v}")?;
        w.flush()
    }

    fn control(&self, request: Value) -> std::io::Result<()> {
        let id = {
            let mut n = self.next_request.lock().unwrap();
            *n += 1;
            format!("compypal-{n}")
        };
        self.write(json!({"type": "control_request", "request_id": id, "request": request}))
    }
}

impl Backend for ClaudeCode {
    fn send(&self, text: &str, context: Option<&str>) -> std::io::Result<()> {
        let mut content = Vec::new();
        if let Some(c) = context {
            content.push(json!({"type": "text", "text": c}));
        }
        content.push(json!({"type": "text", "text": text}));
        self.write(json!({"type": "user", "message": {"role": "user", "content": content}}))
    }

    fn interrupt(&self) -> std::io::Result<()> {
        self.control(json!({"subtype": "interrupt"}))
    }

    fn answer(&self, ask: &Ask, choice: &str) -> std::io::Result<()> {
        let input = &ask.extra["input"];
        let response = match choice {
            "allow" => json!({"behavior": "allow", "updatedInput": input}),
            "always" => json!({"behavior": "allow", "updatedInput": input, "updatedPermissions": ask.extra["always"]}),
            _ => json!({"behavior": "deny", "message": "The user said no."}),
        };
        self.write(json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": ask.request, "response": response}
        }))
    }
}

impl Drop for ClaudeCode {
    fn drop(&mut self) {
        kill_group(&mut self.child.lock().unwrap());
    }
}

/// Starts the agent in a process group of its own, so that everything it
/// starts can be stopped with it: an agent's helper outliving its session
/// would go on editing the song with nobody watching.
pub(super) fn own_group(cmd: &mut Command) -> &mut Command {
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(cmd, 0);
    cmd
}

/// Stops the agent and anything it started.
pub(super) fn kill_group(child: &mut Child) {
    #[cfg(unix)]
    if let Ok(pid) = i32::try_from(child.id()) {
        // SAFETY: kill(2) with a negative pid signals that process group;
        // the group is the agent's own (see `own_group`).
        unsafe { libc::kill(-pid, libc::SIGTERM) };
    }
    let _ = child.kill();
    let _ = child.try_wait();
}

/// Keeps the last non-blank line of a stream: why a process died, usually.
pub(super) fn last_line_of(stream: impl std::io::Read + Send + 'static) -> Arc<Mutex<String>> {
    let tail = Arc::new(Mutex::new(String::new()));
    let keep = tail.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            if !line.trim().is_empty() {
                *keep.lock().unwrap() = line;
            }
        }
    });
    tail
}

/// Waits for a process that closed its output and says why it ended. A
/// session that has stopped talking is over, so whatever it left running
/// goes too.
pub(super) fn exit_reason(child: &Mutex<Child>, tail: &Mutex<String>, name: &str) -> String {
    // Poll rather than wait under the lock: dropping the session needs it.
    let started = std::time::Instant::now();
    let status = loop {
        {
            let mut c = child.lock().unwrap();
            match c.try_wait() {
                Ok(Some(s)) => {
                    kill_group(&mut c);
                    break Ok(s);
                }
                Err(e) => break Err(e),
                // Output closed but still running: it won't be heard from again.
                Ok(None) if started.elapsed() > std::time::Duration::from_secs(3) => {
                    kill_group(&mut c);
                    break c.wait();
                }
                Ok(None) => {}
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let why = tail.lock().unwrap().clone();
    match (status, why.is_empty()) {
        (_, false) => why,
        (Ok(s), true) if s.success() => format!("{name} ended"),
        (Ok(s), true) => format!("{name} exited ({s})"),
        (Err(e), true) => e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Chat, Item, Status, ToolState};

    fn feed(chat: &mut Chat, lines: &[Value]) {
        for l in lines {
            for e in parse(&l.to_string()) {
                chat.apply(e);
            }
        }
    }

    #[test]
    fn a_turn_with_a_tool() {
        let mut chat = Chat { status: Status::Working, ..Default::default() };
        feed(
            &mut chat,
            &[
                json!({"type": "system", "subtype": "init", "session_id": "s1", "model": "claude-x"}),
                json!({"type": "stream_event", "parent_tool_use_id": null,
                       "event": {"type": "content_block_delta", "delta": {"type": "text_delta", "text": "Look"}}}),
                json!({"type": "stream_event", "parent_tool_use_id": null,
                       "event": {"type": "content_block_delta", "delta": {"type": "text_delta", "text": "ing"}}}),
            ],
        );
        assert_eq!((chat.status.clone(), chat.agent.as_str()), (Status::Working, "Claude Code · claude-x"));
        assert_eq!(chat.streaming, "Looking");
        feed(
            &mut chat,
            &[
                json!({"type": "assistant", "parent_tool_use_id": null, "message": {"content": [
                    {"type": "text", "text": "Looking."},
                    {"type": "tool_use", "id": "t1", "name": "mcp__compypal__set_harmony",
                     "input": {"chords": ["vi", "IV"], "section": "Chorus"}}]}}),
                json!({"type": "user", "parent_tool_use_id": null, "message": {"content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": [{"type": "text", "text": "Re-voiced 3 tracks\nmore"}]}]}}),
                json!({"type": "result", "subtype": "success", "is_error": false, "total_cost_usd": 0.02}),
            ],
        );
        assert_eq!(chat.streaming, "");
        assert_eq!(chat.items.len(), 2);
        assert_eq!(chat.items[0].1, Item::Reply("Looking.".into()));
        match &chat.items[1].1 {
            Item::Tool { name, detail, state, result, .. } => {
                assert_eq!(name, "set_harmony");
                assert_eq!(detail, r#"chords ["vi","IV"] · section Chorus"#);
                assert_eq!(*state, ToolState::Ok);
                assert_eq!(result, "Re-voiced 3 tracks");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(chat.cost, 0.02);
        assert_eq!(chat.status, Status::Ready);
    }

    #[test]
    fn permission_requests_become_asks() {
        let line = json!({"type": "control_request", "request_id": "r9", "request": {
            "subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "ls exports"},
            "description": "List exports", "permission_suggestions": [{"type": "addRules"}]}});
        let mut chat = Chat::default();
        feed(&mut chat, &[line]);
        let ask = chat.pending_ask().cloned().unwrap();
        assert_eq!((ask.tool.as_str(), ask.detail.as_str()), ("Bash", "List exports"));
        let ids: Vec<&str> = ask.options.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, ["allow", "always", "deny"]);
        chat.answer("r9", "Deny");
        assert!(chat.pending_ask().is_none());
    }

    #[test]
    fn housekeeping_and_subagents_are_hidden() {
        let search = json!({"type": "assistant", "parent_tool_use_id": null, "message": {"content": [
            {"type": "tool_use", "id": "t", "name": "ToolSearch", "input": {"query": "select:x"}}]}});
        let inner = json!({"type": "assistant", "parent_tool_use_id": "t0",
                           "message": {"content": [{"type": "text", "text": "inner"}]}});
        assert!(parse(&search.to_string()).is_empty());
        assert!(parse(&inner.to_string()).is_empty());
    }
}
