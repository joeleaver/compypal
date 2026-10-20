//! Coding agents as subprocesses, for the in-app agent panel: the way an
//! editor hosts one. Two protocols, one conversation model:
//!
//! - [`acp`]: the Agent Client Protocol, JSON-RPC over stdio, which Gemini
//!   CLI, Qwen Code, opencode, goose and others speak (Codex and Claude
//!   Code through adapters).
//! - [`claude`]: Claude Code's own `stream-json` protocol, so it works with
//!   no adapter installed.
//!
//! Either way the agent gets compypal's MCP tools and nothing else from
//! us; the backend turns its output into [`Event`]s and [`Chat`] folds them
//! into what the panel shows. Neither does I/O, so both are tested here.

pub mod acp;
pub mod bridge;
pub mod claude;

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The MCP server's name as agents see it; tool names carry it.
pub const SERVER: &str = "compypal";

/// What the panel cares about in an agent's output.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The session is up: its id, and the agent's own name for itself.
    Ready { session: String, agent: String },
    /// A piece of the reply as it streams.
    Delta(String),
    /// A finished block of reply text, replacing what streamed.
    Text(String),
    /// The agent called a tool.
    Tool { id: String, name: String, detail: String },
    /// A tool's answer.
    ToolDone { id: String, ok: bool, detail: String },
    /// The agent wants to use a tool that isn't pre-approved.
    Ask(Ask),
    /// The turn is over.
    Done { error: Option<String>, cost: Option<f64> },
    /// The process ended, never started, or can't go on.
    Exited(String),
}

/// A permission request, as the panel's card shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    /// The backend's handle for answering.
    pub request: String,
    pub tool: String,
    pub detail: String,
    pub options: Vec<Choice>,
    /// Whatever else the backend needs to answer.
    pub extra: Value,
}

/// One button on a permission card.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub id: String,
    pub label: String,
    pub kind: ChoiceKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChoiceKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

impl ChoiceKind {
    pub fn parse(s: &str) -> Self {
        match s {
            "allow_always" => Self::AllowAlways,
            "reject_once" => Self::RejectOnce,
            "reject_always" => Self::RejectAlways,
            _ => Self::AllowOnce,
        }
    }

    pub fn allows(self) -> bool {
        matches!(self, Self::AllowOnce | Self::AllowAlways)
    }
}

/// A running agent session.
pub trait Backend {
    /// Starts a turn. `context` goes ahead of the message, in its own block.
    fn send(&self, text: &str, context: Option<&str>) -> std::io::Result<()>;
    /// Stops the running turn; the session stays.
    fn interrupt(&self) -> std::io::Result<()>;
    fn answer(&self, ask: &Ask, choice: &str) -> std::io::Result<()>;
}

/// How to talk to an agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Acp,
    ClaudeCode,
}

/// An agent the panel can start.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentSpec {
    /// Stable id, for settings.
    pub id: String,
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    pub protocol: Protocol,
}

/// Agents we know how to start, if they're installed.
const KNOWN: [(&str, &str, &str, &[&str], Protocol); 7] = [
    ("claude", "Claude Code", "claude", &[], Protocol::ClaudeCode),
    ("gemini", "Gemini CLI", "gemini", &["--experimental-acp"], Protocol::Acp),
    ("qwen", "Qwen Code", "qwen", &["--acp"], Protocol::Acp),
    ("opencode", "opencode", "opencode", &["acp"], Protocol::Acp),
    ("goose", "goose", "goose", &["acp"], Protocol::Acp),
    ("codex", "Codex", "codex-acp", &[], Protocol::Acp),
    ("claude-acp", "Claude Code (ACP)", "claude-code-acp", &[], Protocol::Acp),
];

impl AgentSpec {
    /// A command line for any other ACP agent, e.g. `my-agent --acp`.
    pub fn custom(command: &str) -> Option<Self> {
        let mut words = command.split_whitespace().map(str::to_string);
        let program = words.next()?;
        let name = Path::new(&program).file_name()?.to_string_lossy().to_string();
        Some(Self { id: format!("custom:{command}"), name, program, args: words.collect(), protocol: Protocol::Acp })
    }

    pub fn known(id: &str) -> Option<Self> {
        KNOWN.iter().find(|k| k.0 == id).map(|&(id, name, program, args, protocol)| Self {
            id: id.into(),
            name: name.into(),
            program: program.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            protocol,
        })
    }

    /// Settings name an agent by id, or `custom:<command>`.
    pub fn from_id(id: &str) -> Option<Self> {
        match id.strip_prefix("custom:") {
            Some(cmd) => Self::custom(cmd),
            None => Self::known(id),
        }
    }
}

/// The known agents that are on this machine's PATH, in order of preference.
pub fn installed() -> Vec<AgentSpec> {
    KNOWN.iter().filter_map(|k| AgentSpec::known(k.0)).filter(|a| find_program(&a.program).is_some()).collect()
}

/// Where a program is, by searching PATH (or as given, if it's a path).
pub fn find_program(program: &str) -> Option<PathBuf> {
    let p = Path::new(program);
    if p.components().count() > 1 {
        return p.is_file().then(|| p.to_path_buf());
    }
    std::env::split_paths(&std::env::var_os("PATH")?).map(|dir| dir.join(program)).find(|c| c.is_file())
}

/// With `COMPYPAL_AGENT_LOG=<file>`, every protocol message both ways is appended
/// there: the way to see what an agent actually says.
pub(crate) fn log(dir: &str, line: &str) {
    if let Some(path) = std::env::var_os("COMPYPAL_AGENT_LOG")
        && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path)
    {
        let _ = writeln!(f, "{dir} {line}");
    }
}

/// What a session needs from the app.
#[derive(Clone, Debug)]
pub struct Options {
    pub agent: AgentSpec,
    pub cwd: PathBuf,
    /// compypal's MCP endpoint.
    pub mcp_url: String,
    /// A command that serves the same tools over stdio, for agents that
    /// can't speak MCP over HTTP: `(program, args)`.
    pub mcp_stdio: Option<(String, Vec<String>)>,
    /// Who the agent is talking to and how to behave.
    pub instructions: String,
    pub model: Option<String>,
}

/// Starts an agent, handing each event to `sink` (from a reader thread).
pub fn start(opts: &Options, sink: impl Fn(Event) + Send + 'static) -> std::io::Result<Box<dyn Backend>> {
    Ok(match opts.agent.protocol {
        Protocol::ClaudeCode => Box::new(claude::ClaudeCode::spawn(opts, sink)?),
        Protocol::Acp => Box::new(acp::Acp::spawn(opts, sink)?),
    })
}

/// "mcp__compypal__set_harmony", "compypal_set_harmony" and friends all
/// read as "set_harmony"; other servers' tools keep their server's name.
pub fn short_tool(name: &str) -> String {
    let name = name.strip_prefix("mcp__").unwrap_or(name);
    if let Some(rest) = name.strip_prefix(SERVER) {
        let tool = rest.trim_start_matches(['_', '.', '/', ':']);
        if !tool.is_empty() && tool.len() < rest.len() {
            return tool.to_string();
        }
    }
    match name.split_once("__") {
        Some((server, tool)) if name.len() != tool.len() => format!("{server}: {tool}"),
        _ => name.to_string(),
    }
}

/// Whether a tool (by name, or an agent's title for the call) is one of
/// compypal's own. Those only do what the user could do in the editor, and
/// every one undoes, so nobody is asked before the agent uses them.
pub fn is_ours(name: &str) -> bool {
    let short = short_tool(name);
    let word = short.split([' ', '(', ':']).next().unwrap_or_default();
    crate::tools::list()
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["name"].as_str())
        .any(|t| t == word)
}

/// One short line saying what a tool call is about.
pub fn tool_detail(name: &str, input: &Value) -> String {
    let pick = |k: &str| input[k].as_str().map(str::to_string);
    let line = match name {
        "Bash" => pick("command"),
        "Read" | "Write" | "Edit" | "NotebookEdit" => pick("file_path"),
        "Glob" | "Grep" => pick("pattern"),
        "WebFetch" => pick("url"),
        "WebSearch" => pick("query"),
        "Task" | "Agent" => pick("description"),
        _ => None,
    };
    let line = line.unwrap_or_else(|| {
        // Tools of ours: the arguments, compactly, as `key value`.
        let Some(map) = input.as_object() else { return String::new() };
        map.iter()
            .map(|(k, v)| match v {
                Value::String(s) => format!("{k} {s}"),
                other => format!("{k} {other}"),
            })
            .collect::<Vec<_>>()
            .join(" · ")
    });
    clip(&first_line(&line), 90)
}

/// The text of an MCP-style content list (or a plain string).
pub fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str().or_else(|| p["content"]["text"].as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

pub fn first_line(s: &str) -> String {
    clip(s.trim().lines().next().unwrap_or_default(), 90)
}

fn clip(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// Where the conversation stands.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Status {
    /// Not started: the panel starts the agent when it first opens.
    #[default]
    Off,
    /// The process is up. Agents may say nothing until the first message,
    /// so this is as ready as it gets.
    Ready,
    /// A turn is running.
    Working,
    /// The process is gone; the reason.
    Gone(String),
}

/// A line in the conversation.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// What the user said, and what they had selected.
    User { text: String, about: Option<String> },
    Reply(String),
    Tool { id: String, name: String, detail: String, state: ToolState, result: String },
    /// A permission card, and the label of the choice made.
    Ask { ask: Ask, answer: Option<String> },
    /// A quiet remark from the app, like "Stopped".
    Note(String),
    /// Something went wrong with the turn or the process.
    Problem(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ToolState {
    Running,
    Ok,
    Failed,
}

/// The conversation as the panel shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chat {
    /// Each item with a number that never changes, for keyed lists.
    pub items: Vec<(u64, Item)>,
    /// Reply text still streaming in.
    pub streaming: String,
    pub status: Status,
    /// The agent's name for itself, once it has said.
    pub agent: String,
    pub session: String,
    /// What the conversation has cost so far, in dollars, if the agent says.
    pub cost: f64,
    /// The user asked to stop; the turn's end isn't an error then.
    pub stopping: bool,
    next: u64,
}

impl Chat {
    pub fn push(&mut self, item: Item) {
        self.next += 1;
        self.items.push((self.next, item));
    }

    /// A turn is running: the message box waits.
    pub fn busy(&self) -> bool {
        self.status == Status::Working
    }

    /// The permission request waiting for an answer, if any.
    pub fn pending_ask(&self) -> Option<&Ask> {
        self.items.iter().rev().find_map(|(_, i)| match i {
            Item::Ask { ask, answer: None } => Some(ask),
            _ => None,
        })
    }

    pub fn answer(&mut self, request: &str, label: &str) {
        for (_, item) in &mut self.items {
            if let Item::Ask { ask, answer: answer @ None } = item
                && ask.request == request
            {
                *answer = Some(label.to_string());
            }
        }
    }

    fn flush(&mut self) {
        if !self.streaming.trim().is_empty() {
            let t = std::mem::take(&mut self.streaming);
            self.push(Item::Reply(t.trim().to_string()));
        }
        self.streaming.clear();
    }

    pub fn apply(&mut self, e: Event) {
        match e {
            Event::Ready { session, agent } => {
                self.session = session;
                self.agent = agent;
            }
            Event::Delta(t) => self.streaming.push_str(&t),
            Event::Text(t) => {
                self.streaming.clear();
                self.push(Item::Reply(t));
            }
            Event::Tool { id, name, detail } => {
                // Text that streamed before a tool call is a reply of its own.
                self.flush();
                self.push(Item::Tool { id, name, detail, state: ToolState::Running, result: String::new() });
            }
            Event::ToolDone { id: done, ok, detail } => {
                for (_, item) in &mut self.items {
                    if let Item::Tool { id, state, result, .. } = item
                        && *id == done
                    {
                        *state = if ok { ToolState::Ok } else { ToolState::Failed };
                        *result = detail.clone();
                    }
                }
            }
            Event::Ask(ask) => self.push(Item::Ask { ask, answer: None }),
            Event::Done { error, cost } => {
                self.flush();
                let said_nothing = !self
                    .items
                    .iter()
                    .rev()
                    .take_while(|(_, i)| !matches!(i, Item::User { .. }))
                    .any(|(_, i)| matches!(i, Item::Reply(_) | Item::Tool { .. } | Item::Ask { .. }));
                if std::mem::take(&mut self.stopping) {
                    self.push(Item::Note("Stopped.".into()));
                } else if let Some(e) = error {
                    self.push(Item::Problem(e));
                } else if said_nothing {
                    self.push(Item::Problem(
                        "The agent ended its turn without a word. It may need setting up: \
                         a model, an API key or a login (try running it in a terminal)."
                            .into(),
                    ));
                }
                self.cost = cost.unwrap_or(self.cost);
                self.settle();
                if self.status == Status::Working {
                    self.status = Status::Ready;
                }
            }
            Event::Exited(why) => {
                self.flush();
                self.settle();
                self.status = Status::Gone(why);
            }
        }
    }

    /// A turn that ended leaves no tool running and no question open.
    fn settle(&mut self) {
        for (_, item) in &mut self.items {
            match item {
                Item::Tool { state: s @ ToolState::Running, .. } => *s = ToolState::Failed,
                Item::Ask { answer: a @ None, .. } => *a = Some("cancelled".into()),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn streamed_text_before_a_tool_is_its_own_reply() {
        let mut chat = Chat { status: Status::Working, ..Default::default() };
        chat.apply(Event::Delta("Let me look. ".into()));
        chat.apply(Event::Tool { id: "1".into(), name: "get_harmony".into(), detail: String::new() });
        chat.apply(Event::ToolDone { id: "1".into(), ok: true, detail: "C F G".into() });
        chat.apply(Event::Delta("It's I IV V.".into()));
        chat.apply(Event::Done { error: None, cost: None });
        let replies: Vec<&Item> = chat.items.iter().map(|(_, i)| i).collect();
        assert_eq!(replies[0], &Item::Reply("Let me look.".into()));
        assert!(matches!(replies[1], Item::Tool { state: ToolState::Ok, .. }));
        assert_eq!(replies[2], &Item::Reply("It's I IV V.".into()));
        assert_eq!(chat.status, Status::Ready);
    }

    #[test]
    fn errors_and_exits_settle_the_turn() {
        let mut chat = Chat { status: Status::Working, ..Default::default() };
        chat.apply(Event::Tool { id: "t".into(), name: "x".into(), detail: String::new() });
        chat.apply(Event::Done { error: Some("API overloaded".into()), cost: None });
        assert!(matches!(chat.items[0].1, Item::Tool { state: ToolState::Failed, .. }));
        assert_eq!(chat.items[1].1, Item::Problem("API overloaded".into()));
        assert_eq!(chat.status, Status::Ready);
        chat.apply(Event::Exited("bye".into()));
        assert_eq!(chat.status, Status::Gone("bye".into()));
    }

    #[test]
    fn a_silent_turn_says_so() {
        let mut chat = Chat { status: Status::Working, ..Default::default() };
        chat.push(Item::User { text: "hi".into(), about: None });
        chat.apply(Event::Done { error: None, cost: None });
        assert!(matches!(chat.items.last().unwrap().1, Item::Problem(_)));
    }

    #[test]
    fn a_stop_is_not_an_error() {
        let mut chat = Chat { status: Status::Working, stopping: true, ..Default::default() };
        let ask = Ask { request: "r".into(), tool: "Bash".into(), detail: String::new(), options: vec![], extra: json!(null) };
        chat.apply(Event::Ask(ask));
        chat.apply(Event::Done { error: Some("error during execution".into()), cost: None });
        assert!(chat.pending_ask().is_none());
        assert_eq!(chat.items.last().unwrap().1, Item::Note("Stopped.".into()));
        assert!(!chat.stopping);
    }

    #[test]
    fn tool_names_and_details() {
        assert_eq!(short_tool("mcp__compypal__get_harmony"), "get_harmony");
        assert_eq!(short_tool("compypal_get_harmony"), "get_harmony");
        assert_eq!(short_tool("compypal.get_harmony"), "get_harmony");
        assert_eq!(short_tool("mcp__other__thing"), "other: thing");
        assert_eq!(short_tool("Bash"), "Bash");
        assert_eq!(tool_detail("Read", &json!({"file_path": "/a/b.abc"})), "/a/b.abc");
        assert_eq!(tool_detail("get_selection", &json!({})), "");
        let long = "x".repeat(200);
        assert_eq!(tool_detail("Bash", &json!({"command": long})).chars().count(), 91);
    }

    #[test]
    fn our_tools_are_recognised() {
        assert!(is_ours("mcp__compypal__set_notes"));
        assert!(is_ours("compypal_get_harmony"));
        assert!(is_ours("set_harmony (compypal MCP Server)"));
        assert!(!is_ours("Bash"));
        assert!(!is_ours("Set the tempo to 110 BPM"));
    }

    #[test]
    fn agents_by_id() {
        let q = AgentSpec::from_id("qwen").unwrap();
        assert_eq!((q.program.as_str(), q.args.clone(), q.protocol), ("qwen", vec!["--acp".to_string()], Protocol::Acp));
        let c = AgentSpec::from_id("custom:/opt/bin/my-agent --acp --fast").unwrap();
        assert_eq!((c.name.as_str(), c.args.len(), c.protocol), ("my-agent", 2, Protocol::Acp));
        assert!(AgentSpec::from_id("nope").is_none());
    }
}
