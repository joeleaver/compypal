//! The agent's side door: the MCP tools run against the live store on the
//! UI thread, and what the user selects is pushed to Claude Code.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::Duration;

use compypal_core::text::{bar_of, format_notes};
use compypal_core::{Id, Note, Project, Tick};
use compypal_mcp::{App, Dispatch, Selection, ide::Ide, tools};
use rinch::prelude::*;
use serde_json::{Value, json};

use crate::store::Store;

/// The IDE link, kept so its lockfile can be withdrawn at exit.
static IDE: std::sync::OnceLock<&'static Ide> = std::sync::OnceLock::new();

/// Withdraws the IDE lockfile. Call when the app is closing.
pub fn shutdown() {
    if let Some(ide) = IDE.get() {
        ide.close();
    }
}

thread_local! {
    /// The store, for tool calls that arrive on server threads and are
    /// re-run here. Set once in `start`; the UI thread is the only reader.
    static STORE: Cell<Option<Store>> = const { Cell::new(None) };
}

/// Tools see the app through the store, exactly as the UI does: edits are
/// undoable and appear on screen immediately.
struct StoreApp(Store);

impl App for StoreApp {
    fn project(&self) -> Project {
        self.0.project.get()
    }

    fn edit(&mut self, label: &str, f: &mut dyn FnMut(&mut Project) -> Result<(), String>) -> Result<(), String> {
        let mut next = self.0.project.get();
        f(&mut next)?;
        self.0.edit(&format!("agent: {label}"), |p| *p = next);
        self.0.status.set(format!("Claude: {label}"));
        Ok(())
    }

    fn undo(&mut self) -> Option<String> {
        self.0.undo()
    }

    fn selection(&self) -> Option<Selection> {
        self.0.selection.get()
    }

    fn play(&mut self, from: Option<Tick>) -> Result<(), String> {
        if self.0.engine.is_none() {
            return Err("no audio output".into());
        }
        if self.0.is_recording() {
            return Err("the user is recording".into());
        }
        if self.0.playhead.get().is_some() {
            self.0.toggle_play();
        }
        self.0.cursor.set(from.unwrap_or(0));
        self.0.toggle_play();
        Ok(())
    }

    fn stop(&mut self) {
        if self.0.playhead.get().is_some() && !self.0.is_recording() {
            self.0.toggle_play();
        }
    }

    fn audition(&mut self, track: Id, notes: &[Note]) -> Result<(), String> {
        self.0.audition(track, notes);
        Ok(())
    }

    fn export(&mut self, format: &str) -> Result<String, String> {
        let path = match format {
            "abc" => self.0.export_abc(),
            _ => self.0.export_midi(),
        }?;
        Ok(std::fs::canonicalize(&path).unwrap_or(path).display().to_string())
    }
}

/// Hops each tool call onto the UI thread and waits for its answer.
struct UiDispatch;

impl Dispatch for UiDispatch {
    fn call_tool(&self, name: &str, args: Value) -> Result<String, String> {
        let (tx, rx) = channel();
        let name = name.to_string();
        run_on_main_thread(move || {
            let result = match STORE.with(Cell::get) {
                Some(store) => tools::call(&mut StoreApp(store), &name, &args),
                None => Err("compypal is still starting".into()),
            };
            let _ = tx.send(result);
        });
        rx.recv_timeout(Duration::from_secs(30)).unwrap_or_else(|_| Err("compypal didn't answer in time".into()))
    }
}

pub fn status_text(agents: usize) -> String {
    match agents {
        0 => "Claude: not attached (run claude here, then /ide)".into(),
        1 => "Claude attached".into(),
        n => format!("{n} Claude sessions attached"),
    }
}

/// Starts the MCP server for tools, and the IDE link for presence and
/// selection, then keeps the agent count and selection flowing.
pub fn start(store: Store) {
    STORE.with(|s| s.set(Some(store)));
    let dispatch: Arc<dyn Dispatch> = Arc::new(UiDispatch);

    let port = std::env::var("COMPYPAL_MCP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(compypal_mcp::http::DEFAULT_PORT);
    match compypal_mcp::http::serve(port, dispatch.clone()) {
        Ok(()) => eprintln!("MCP tools: http://127.0.0.1:{port}/mcp"),
        Err(e) => eprintln!("MCP tools unavailable: {e}"),
    }

    let workspace = std::env::current_dir().map(|d| vec![d.display().to_string()]).unwrap_or_default();
    let ide: &'static Ide = match Ide::start(dispatch, workspace.clone()) {
        Ok(ide) => IDE.get_or_init(|| Box::leak(Box::new(ide))),
        Err(e) => {
            eprintln!("IDE link unavailable: {e}");
            return;
        }
    };
    eprintln!(
        "IDE link on port {} for {}: run `claude` there and use /ide (or CLAUDE_CODE_SSE_PORT={} claude)",
        ide.port(),
        workspace.first().map_or("?", String::as_str),
        ide.port()
    );

    let agents = store.agents;
    std::thread::spawn(move || {
        let mut last = usize::MAX;
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let n = ide.connected();
            if n != last {
                agents.send(n);
                last = n;
            }
        }
    });

    // Tell attached sessions what the user is looking at.
    rinch::core::Effect::new(move || {
        let Some(sel) = store.selection.get() else { return };
        let _ = store.agents.get(); // resend when someone attaches
        let params = store.project.with(|p| selection_params(p, &sel));
        if let Some(params) = params {
            ide.notify("selection_changed", params);
        }
    });
}

/// A selection as Claude Code reads one: a "file" (project/track), a line
/// range (bars), and the text (what's there, readably).
fn selection_params(p: &Project, sel: &Selection) -> Option<Value> {
    let t = p.track(sel.track)?;
    let notes: Vec<Note> =
        t.absolute_notes().into_iter().filter(|n| n.start >= sel.start && n.start < sel.end.max(sel.start + 1)).collect();
    let end = sel.end.min(notes.iter().map(|n| n.end()).max().unwrap_or(sel.start + 1));
    let (first, last) = (bar_of(p, sel.start), bar_of(p, end.saturating_sub(1).max(sel.start)));
    let what = match sel.figure {
        Some(i) => {
            let figs = compypal_core::figure::analyze(&t.absolute_notes(), &p.meter_at(0), &Default::default());
            let f = figs.get(i)?;
            format!(
                "figure {i} of track {:?}: {} ({}) {}",
                t.name,
                f.chord.name(compypal_core::theory::Spelling::for_key(p.key)),
                f.chord.roman(p.key),
                f.kind.label()
            )
        }
        None => format!("track {:?}", t.name),
    };
    let text = format!("{what}, bars {first}-{last}, {} notes:\n{}", notes.len(), format_notes(p, &notes));
    Some(json!({
        "text": text,
        "filePath": format!("compypal/{}/{}", p.name, t.name),
        "selection": {
            "start": {"line": first - 1, "character": 0},
            "end": {"line": last - 1, "character": 1},
            "isEmpty": false
        }
    }))
}
