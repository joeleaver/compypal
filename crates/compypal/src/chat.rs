//! The agent panel: a coding agent (Claude Code, Gemini CLI, opencode, or
//! any other that speaks ACP) running inside compypal, using the same MCP
//! tools as any other session, with what you've selected sent along with
//! each message. `compypal_mcp::agent` does the protocols; this is the UI.

use std::cell::RefCell;
use std::rc::Rc;

use compypal_mcp::agent::{self, AgentSpec, Backend, Chat, Item, Status, ToolState};
use compypal_mcp::tools;
use rinch::prelude::*;

use crate::store::Store;

thread_local! {
    /// The running session. UI thread only, like the store.
    static AGENT: RefCell<Option<Box<dyn Backend>>> = const { RefCell::new(None) };
    /// The message box, to focus it and to know when someone's typing there.
    static COMPOSER: RefCell<Option<NodeHandle>> = const { RefCell::new(None) };
    /// The selection as last sent, so an unchanged one isn't repeated.
    static SENT_CONTEXT: RefCell<String> = const { RefCell::new(String::new()) };
}

const INSTRUCTIONS: &str = "\
You are running inside compypal's agent panel. compypal is a MIDI composer and \
arranger; the user is talking to you from inside the app, not from a terminal. \
Use the compypal MCP tools to read and change the song: your edits appear in \
the app as you make them, and the user can undo each one. A message may start \
with a <selection> block saying what the user has selected; \"this\" means that. \
Answer briefly and musically: positions as bar.beat, pitches as note names, \
chords as symbols. The panel is narrow: two or three short sentences unless \
the user asks for more, no headings or tables. Don't describe tool calls \
the user can already see.";

/// Whether keys are going into the message box: shortcuts keep out of the
/// way then, as they do for the popovers.
pub fn composer_focused() -> bool {
    COMPOSER.with(|c| {
        c.borrow().as_ref().is_some_and(|n| n.active_element().is_some_and(|a| a.node_id() == n.node_id()))
    })
}

fn focus_composer() {
    COMPOSER.with(|c| {
        if let Some(n) = c.borrow().as_ref() {
            n.focus();
        }
    });
}

/// Opens or closes the panel, starting the agent the first time.
pub fn toggle(store: Store) {
    let open = !store.chat_open.get();
    store.chat_open.set(open);
    store.agent_menu.set(false);
    if open {
        ensure_started(store);
        set_timeout(0, focus_composer);
    } else {
        COMPOSER.with(|c| c.borrow_mut().take());
    }
}

/// `COMPYPAL_AGENT` (an id like `gemini`, or a command line for any ACP
/// agent) wins; then the one picked in the panel; then the first installed.
fn env_agent() -> Option<AgentSpec> {
    let v = std::env::var("COMPYPAL_AGENT").ok()?;
    AgentSpec::from_id(&v).or_else(|| AgentSpec::custom(&v))
}

/// The agents the picker offers.
pub fn choices() -> Vec<AgentSpec> {
    let mut all = agent::installed();
    if let Some(a) = env_agent()
        && !all.contains(&a)
    {
        all.insert(0, a);
    }
    all
}

fn chosen(store: Store) -> Option<AgentSpec> {
    env_agent()
        .or_else(|| store.agent_id.get().and_then(|id| AgentSpec::from_id(&id)).filter(|a| agent::find_program(&a.program).is_some()))
        .or_else(|| agent::installed().into_iter().next())
}

/// The picked agent's name, for the panel header.
pub fn agent_name(store: Store) -> String {
    chosen(store).map_or_else(|| "No agent".into(), |a| a.name)
}

fn options(spec: AgentSpec) -> agent::Options {
    let mcp_url = format!("http://127.0.0.1:{}/mcp", crate::agent::mcp_port());
    agent::Options {
        agent: spec,
        cwd: std::env::current_dir().unwrap_or_else(|_| ".".into()),
        // This same binary forwards stdio MCP to the HTTP endpoint.
        mcp_stdio: std::env::current_exe()
            .ok()
            .map(|exe| (exe.display().to_string(), vec!["--mcp-stdio".into(), mcp_url.clone()])),
        mcp_url,
        instructions: INSTRUCTIONS.into(),
        model: std::env::var("COMPYPAL_AGENT_MODEL").ok(),
    }
}

/// Starts the agent unless it's already running.
fn ensure_started(store: Store) {
    if AGENT.with(|c| c.borrow().is_some()) && !matches!(store.chat.with(|c| c.status.clone()), Status::Gone(_)) {
        return;
    }
    SENT_CONTEXT.with(|s| s.borrow_mut().clear());
    let Some(spec) = chosen(store) else {
        store.chat.update(|c| {
            c.status = Status::Gone(
                "No coding agent found. Install one that speaks ACP (Gemini CLI, Qwen Code, opencode, goose…) \
                 or Claude Code, or set COMPYPAL_AGENT to the command that starts yours."
                    .into(),
            )
        });
        return;
    };
    let name = spec.name.clone();
    let chat = store.chat;
    match agent::start(&options(spec), move |e| chat.update_send(move |c| c.apply(e))) {
        Ok(backend) => {
            AGENT.with(|c| *c.borrow_mut() = Some(backend));
            store.chat.update(|c| {
                c.status = Status::Ready;
                c.agent = name;
            });
        }
        Err(e) => store.chat.update(|c| c.status = Status::Gone(format!("Couldn't start {name}: {e}"))),
    }
}

/// Starts over with a fresh session.
pub fn new_chat(store: Store) {
    AGENT.with(|c| c.borrow_mut().take());
    store.chat.set(Chat::default());
    ensure_started(store);
    focus_composer();
}

/// Switches to another agent, remembering the choice.
fn pick(store: Store, id: String) {
    store.agent_menu.set(false);
    crate::autosave::update_settings(|s| s.agent = Some(id.clone()));
    store.agent_id.set(Some(id));
    new_chat(store);
}

/// What the user has selected, as a `<selection>` block: in full the first
/// time it's sent, then just its name while it stays the same.
fn context(store: Store) -> (Option<String>, Option<String>) {
    let Some(sel) = store.selection.get() else { return (None, None) };
    let (about, full) = store.project.with(|p| (sel.describe(p), tools::selection_text(p, &sel)));
    let block = SENT_CONTEXT.with(|s| {
        let mut s = s.borrow_mut();
        if *s == full {
            format!("<selection>Still: {about}</selection>")
        } else {
            *s = full.clone();
            format!("<selection>\n{full}</selection>")
        }
    });
    (Some(block), Some(about))
}

/// Sends what's in the message box.
pub fn send(store: Store) {
    let text = store.chat_draft.get().trim().to_string();
    if text.is_empty() || store.chat.with(|c| c.busy() || c.pending_ask().is_some()) {
        return;
    }
    send_text(store, text);
    store.chat_draft.set(String::new());
}

fn send_text(store: Store, text: String) {
    ensure_started(store);
    let (block, about) = context(store);
    let sent = AGENT.with(|c| c.borrow().as_ref().map(|a| a.send(&text, block.as_deref())));
    store.chat.update(|c| {
        c.push(Item::User { text, about });
        match sent {
            Some(Ok(())) => c.status = Status::Working,
            Some(Err(e)) => c.status = Status::Gone(format!("Lost the agent: {e}")),
            None => {}
        }
    });
}

pub fn stop(store: Store) {
    store.chat.update(|c| c.stopping = true);
    AGENT.with(|c| {
        if let Some(a) = c.borrow().as_ref() {
            let _ = a.interrupt();
        }
    });
    store.status.set("Stopped the agent".into());
}

fn answer(store: Store, choice: String) {
    let Some(ask) = store.chat.with(|c| c.pending_ask().cloned()) else { return };
    let label = ask.options.iter().find(|o| o.id == choice).map_or(choice.clone(), |o| o.label.clone());
    AGENT.with(|c| {
        if let Some(a) = c.borrow().as_ref() {
            let _ = a.answer(&ask, &choice);
        }
    });
    store.chat.update(|c| c.answer(&ask.request, &label));
}

/// One conversation line, flattened for a keyed list: the key carries
/// everything that can change, so a changed line is rebuilt.
#[derive(Clone, Debug, PartialEq)]
struct Row {
    key: String,
    kind: RowKind,
    text: String,
    detail: String,
    extra: String,
    class: &'static str,
    mark: &'static str,
    /// A permission card's buttons: id, label, variant.
    choices: Vec<(String, String, &'static str)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum RowKind {
    User,
    Reply,
    ToolRunning,
    ToolOk,
    ToolFailed,
    Ask,
    Note,
    Problem,
}

fn rows(chat: &Chat) -> Vec<Row> {
    chat.items
        .iter()
        // Once answered, the tool's own line says what happened.
        .filter(|(_, item)| !matches!(item, Item::Ask { answer: Some(_), .. }))
        .map(|(id, item)| {
            let (kind, text, detail, extra) = match item {
                Item::User { text, about } => {
                    (RowKind::User, text.clone(), about.as_ref().map(|a| format!("about {a}")).unwrap_or_default(), String::new())
                }
                Item::Reply(t) => (RowKind::Reply, plain(t), String::new(), String::new()),
                Item::Tool { name, detail, state, result, .. } => {
                    let kind = match state {
                        ToolState::Running => RowKind::ToolRunning,
                        ToolState::Ok => RowKind::ToolOk,
                        ToolState::Failed => RowKind::ToolFailed,
                    };
                    (kind, name.clone(), detail.clone(), result.clone())
                }
                Item::Ask { ask, .. } => {
                    // Agents name either a tool ("Bash") or what it would do
                    // ("Set the tempo to 110 BPM"); both read as a question.
                    let q = if ask.tool.contains(' ') { format!("{}?", ask.tool.trim_end_matches('.')) } else { format!("Use {}?", ask.tool) };
                    (RowKind::Ask, q, ask.detail.clone(), String::new())
                }
                Item::Note(n) => (RowKind::Note, n.clone(), String::new(), String::new()),
                Item::Problem(p) => (RowKind::Problem, p.clone(), String::new(), String::new()),
            };
            // The first "allow" is the obvious answer; the rest are plain.
            let first_allow = match item {
                Item::Ask { ask, .. } => ask.options.iter().position(|o| o.kind.allows()),
                _ => None,
            };
            let choices = match item {
                Item::Ask { ask, .. } => ask
                    .options
                    .iter()
                    .enumerate()
                    .map(|(i, o)| (o.id.clone(), o.label.clone(), if Some(i) == first_allow { "filled" } else { "default" }))
                    .collect(),
                _ => Vec::new(),
            };
            let (class, mark) = match kind {
                RowKind::ToolRunning => ("chat-tool running", "◌"),
                RowKind::ToolFailed => ("chat-tool failed", "✕"),
                RowKind::User if detail.is_empty() => ("chat-about none", ""),
                RowKind::User => ("chat-about", ""),
                _ => ("chat-tool", "✓"),
            };
            Row { key: format!("{id}:{kind:?}:{}", extra.len()), kind, text, detail, extra, class, mark, choices }
        })
        .collect()
}

/// Replies come as Markdown; the panel shows text, so drop the marks that
/// would otherwise show as clutter.
fn plain(md: &str) -> String {
    md.lines()
        .filter(|l| !l.trim_start().starts_with("```"))
        // A table's rule row ("|---|:--|") carries nothing.
        .filter(|l| !(l.trim_start().starts_with('|') && l.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))))
        .map(|l| {
            // A table row reads as its cells in a line.
            if let Some(row) = l.trim().strip_prefix('|') {
                let cells: Vec<&str> = row.trim_end_matches('|').split('|').map(str::trim).collect();
                return cells.join("  ·  ").replace("**", "").replace('`', "");
            }
            let l = l.trim_start_matches('#').trim_start_matches(' ');
            let l = match l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")) {
                Some(rest) => format!("• {rest}"),
                None => l.to_string(),
            };
            l.replace("**", "").replace('`', "")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn status_label(chat: &Chat) -> (&'static str, String) {
    match &chat.status {
        Status::Off => ("idle", "not started".into()),
        Status::Ready if chat.pending_ask().is_some() => ("ask", "needs you".into()),
        Status::Ready => ("ready", "ready".into()),
        Status::Working if chat.pending_ask().is_some() => ("ask", "needs you".into()),
        Status::Working => ("busy", "working…".into()),
        Status::Gone(_) => ("gone", "stopped".into()),
    }
}

/// The transport button's dot: green when ready, violet while working,
/// amber when the agent is waiting on you.
pub fn dot_class(store: Store) -> String {
    format!("chat-dot {}", store.chat.with(|c| status_label(c).0))
}

const STARTERS: [&str; 4] = [
    "Clean up my last take",
    "What are the chords here?",
    "Write a bass line under this",
    "What did I play today?",
];

#[component]
pub fn AgentPanel() -> NodeHandle {
    let store = use_store::<Store>();
    let lines = Memo::new(move || store.chat.with(rows));
    let composer = rsx! {
        textarea {
            class: "chat-input",
            placeholder: "Ask about this, or say what to change…",
            value: {|| store.chat_draft.get()},
            oninput: move |v: String| store.chat_draft.set(v),
        }
    };
    COMPOSER.with(|c| *c.borrow_mut() = Some(composer.clone()));
    let log = rsx! {
        div { class: "chat-log",
            if store.chat.with(|c| c.items.is_empty()) {
                div { class: "chat-empty",
                    div { class: "chat-empty-title", "What should we do with this?" }
                    div { class: "chat-empty-body",
                        "The agent sees what you've selected and edits the song the way you do. \
                         Its changes show up as it makes them, and each one undoes like your own."
                    }
                    for s in STARTERS.iter().copied() {
                        div { key: s, class: "chat-starter",
                            onclick: move || {
                                send_text(store, String::from(s as &str));
                                focus_composer();
                            },
                            {s}
                        }
                    }
                }
            }
            for row in lines.get() {
                div { key: row.key.clone(),
                    match row.kind {
                        RowKind::User => div { class: "chat-user",
                            div { {row.text.clone()} }
                            div { class: {row.class}, {row.detail.clone()} }
                        },
                        RowKind::Reply => div { class: "chat-reply", {row.text.clone()} },
                        RowKind::ToolRunning | RowKind::ToolOk | RowKind::ToolFailed => div {
                            class: {row.class},
                            span { class: "chat-tool-mark", {row.mark} }
                            div { class: "chat-tool-body",
                                div { class: "chat-tool-call",
                                    span { class: "chat-tool-name", {row.text.clone()} }
                                    span { class: "chat-tool-detail", {row.detail.clone()} }
                                }
                                div { class: "chat-tool-result", {row.extra.clone()} }
                            }
                        },
                        RowKind::Ask => div { class: "chat-ask",
                            div { class: "chat-ask-label", "The agent asks" }
                            div { class: "chat-ask-title", {row.text.clone()} }
                            div { class: "chat-ask-detail", {row.detail.clone()} }
                            div { class: "chat-ask-actions",
                                for (id, label, variant) in row.choices.clone() {
                                    Button { key: id.clone(), size: "xs", variant: variant,
                                        onclick: move || answer(store, id.clone()),
                                        {label.clone()}
                                    }
                                }
                            }
                        },
                        RowKind::Note => div { class: "chat-note", {row.text.clone()} },
                        RowKind::Problem => div { class: "chat-problem", {row.text.clone()} },
                    }
                }
            }
            if !store.chat.with(|c| c.streaming.is_empty()) {
                div { class: "chat-reply streaming", {|| format!("{}▍", store.chat.with(|c| c.streaming.clone()))} }
            }
            if store.chat.with(|c| c.status == Status::Working && c.streaming.is_empty() && c.pending_ask().is_none()
                && !c.items.iter().any(|(_, i)| matches!(i, Item::Tool { state: ToolState::Running, .. })))
            {
                div { class: "chat-thinking", "Thinking…" }
            }
            if let Status::Gone(why) = store.chat.with(|c| c.status.clone()) {
                div { class: "chat-problem",
                    div { {why} }
                    div { class: "chat-problem-actions",
                        Button { size: "xs", variant: "light", onclick: move || new_chat(store), "Start again" }
                    }
                }
            }
        }
    };
    // New lines arrive at the bottom; keep them in view.
    {
        let log = log.clone();
        rinch::core::Effect::new(move || {
            let _ = store.chat.with(|c| (c.items.len(), c.streaming.len(), c.status.clone()));
            let log = log.clone();
            set_timeout(0, move || log.scroll_to_bottom());
        });
    }
    let root = rsx! {
        div { class: "chat-panel",
            div { class: "chat-head",
                span { class: "chat-title", "Agent" }
                div { class: "agent-pick-anchor",
                    span { class: "agent-pick",
                        onclick: move || store.agent_menu.update(|o| *o = !*o),
                        {|| {
                            let said = store.chat.with(|c| c.agent.clone());
                            format!("{} ▾", if said.is_empty() { agent_name(store) } else { said })
                        }}
                    }
                    if store.agent_menu.get() {
                        AgentMenu {}
                    }
                }
                span { class: {|| format!("chat-state {}", store.chat.with(|c| status_label(c).0))},
                    {|| store.chat.with(|c| status_label(c).1)}
                }
                div { class: "spacer" }
                Button { size: "xs", variant: "subtle",
                    disabled: {|| store.chat.with(|c| c.items.is_empty() && c.status != Status::Off && !matches!(c.status, Status::Gone(_)))},
                    onclick: move || new_chat(store),
                    "New chat"
                }
                Button { size: "xs", variant: "subtle", onclick: move || toggle(store), "✕" }
            }
            {log}
            div { class: "chat-compose",
                div { class: "chat-context",
                    {|| {
                        let sel = store.selection.get();
                        match sel {
                            Some(s) => store.project.with(|p| s.describe(p)),
                            None => "Nothing selected: the whole song".into(),
                        }
                    }}
                }
                {composer.clone()}
                div { class: "chat-compose-foot",
                    span { class: "chat-hint",
                        {|| if store.chat.with(|c| c.busy()) {
                            "Esc stops the agent".to_string()
                        } else {
                            "Enter sends · Shift+Enter new line".to_string()
                        }}
                    }
                    if store.chat.with(|c| c.busy()) {
                        Button { size: "xs", variant: "light", color: "red", onclick: move || stop(store), "■ Stop" }
                    } else {
                        Button { size: "xs", variant: "filled",
                            disabled: {|| store.chat_draft.with(|d| d.trim().is_empty())},
                            onclick: move || send(store),
                            "Send"
                        }
                    }
                }
            }
        }
    };
    overlay_dismiss::arm_keys_while_open(
        __scope,
        &composer,
        Rc::new(|| true),
        Rc::new(move |k: &rinch::core::KeyEventData| {
            if !k.is_down() {
                return false;
            }
            match k.key.as_str() {
                "Enter" if !k.shift => {
                    send(store);
                    true
                }
                _ => false,
            }
        }),
        Rc::new(|| {}),
    );
    root
}

#[derive(Clone, Debug, PartialEq)]
struct AgentRow {
    id: String,
    name: String,
    command: String,
    current: bool,
}

/// Picks which agent the panel runs.
#[component]
fn AgentMenu() -> NodeHandle {
    let store = use_store::<Store>();
    let current = chosen(store).map(|a| a.id);
    let rows: Vec<AgentRow> = choices()
        .into_iter()
        .map(|a| AgentRow {
            current: Some(&a.id) == current.as_ref(),
            command: std::iter::once(a.program.clone()).chain(a.args.iter().cloned()).collect::<Vec<_>>().join(" "),
            id: a.id,
            name: a.name,
        })
        .collect();
    let root = rsx! {
        div { class: "agent-menu",
            for row in rows.clone() {
                div { key: row.id.clone(),
                    class: {if row.current { "agent-row current" } else { "agent-row" }},
                    onclick: {
                        let id = row.id.clone();
                        move || pick(store, id.clone())
                    },
                    div { class: "track-name", {row.name.clone()} }
                    div { class: "track-detail", {row.command.clone()} }
                }
            }
            if rows.is_empty() {
                div { class: "empty", "No agents found on PATH." }
            }
            div { class: "agent-menu-hint",
                "Any agent that speaks ACP works: set COMPYPAL_AGENT to the command that starts it."
            }
        }
    };
    overlay_dismiss::arm_keys_while_open(
        __scope,
        &root,
        Rc::new(|| true),
        Rc::new(move |k: &rinch::core::KeyEventData| {
            if k.key == "Escape" {
                store.agent_menu.set(false);
                return true;
            }
            false
        }),
        Rc::new(move || store.agent_menu.set(false)),
    );
    root
}

#[cfg(test)]
mod tests {
    use super::plain;

    #[test]
    fn markdown_reads_as_text() {
        let md = "**Done.** Here:\n\n| Bars | Chord |\n|---|:--|\n| 5 | `C` |\n- one\n## Next";
        assert_eq!(plain(md), "Done. Here:\n\nBars  ·  Chord\n5  ·  C\n• one\nNext");
    }
}
