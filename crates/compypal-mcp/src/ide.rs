//! The editor-style link to Claude Code: a WebSocket MCP server advertised
//! by a lockfile in `~/.claude/ide/`, which `claude` finds through `/ide`
//! (or connects to on its own when started with `CLAUDE_CODE_SSE_PORT`).
//!
//! Claude Code hides an IDE server's tools from the model, so this link is
//! for presence and context: what the user has selected arrives in the
//! conversation through `selection_changed` notifications. The tools live
//! on the HTTP server ([`crate::http`]); the same handler answers here too.

use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::{Message, WebSocket};

use crate::protocol::{Dispatch, handle};

pub struct Ide {
    port: u16,
    lockfile: Option<PathBuf>,
    clients: Arc<Mutex<Vec<Sender<String>>>>,
    connected: Arc<AtomicUsize>,
}

/// `$CLAUDE_CONFIG_DIR/ide`, else `~/.claude/ide`.
fn lock_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(d).join("ide"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude").join("ide"))
}

fn token() -> String {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).expect("system randomness");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Ide {
    /// Listens on a free localhost port and writes the lockfile that
    /// advertises it for `workspace` folders.
    pub fn start(dispatch: Arc<dyn Dispatch>, workspace: Vec<String>) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let token = token();
        let clients: Arc<Mutex<Vec<Sender<String>>>> = Default::default();
        let connected = Arc::new(AtomicUsize::new(0));

        let lockfile = lock_dir().map(|dir| dir.join(format!("{port}.lock")));
        if let Some(path) = &lockfile {
            std::fs::create_dir_all(path.parent().unwrap())?;
            sweep_stale(path.parent().unwrap());
            let body = json!({
                "pid": std::process::id(),
                "workspaceFolders": workspace,
                "ideName": "compypal",
                "transport": "ws",
                "runningInWindows": false,
                "authToken": token,
            });
            write_private(path, body.to_string().as_bytes())?;
        }

        let (clients_accept, connected_accept) = (clients.clone(), connected.clone());
        std::thread::Builder::new().name("compypal-ide".into()).spawn(move || {
            for stream in listener.incoming().flatten() {
                let (dispatch, token) = (dispatch.clone(), token.clone());
                let (clients, connected) = (clients_accept.clone(), connected_accept.clone());
                std::thread::spawn(move || {
                    let Some(ws) = accept(stream, &token) else { return };
                    let (tx, rx) = channel();
                    clients.lock().unwrap().push(tx);
                    connected.fetch_add(1, Ordering::Relaxed);
                    serve(ws, rx, dispatch.as_ref());
                    connected.fetch_sub(1, Ordering::Relaxed);
                });
            }
        })?;

        Ok(Self { port, lockfile, clients, connected })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// How many Claude Code sessions are attached.
    pub fn connected(&self) -> usize {
        self.connected.load(Ordering::Relaxed)
    }

    /// Sends a notification to every attached session.
    pub fn notify(&self, method: &str, params: Value) {
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string();
        self.clients.lock().unwrap().retain(|c| c.send(msg.clone()).is_ok());
    }
}

impl Ide {
    /// Withdraws the lockfile, so Claude Code stops offering this app.
    /// Call on the way out; the server itself lives as long as the process.
    pub fn close(&self) {
        if let Some(p) = &self.lockfile {
            let _ = std::fs::remove_file(p);
        }
    }
}

impl Drop for Ide {
    fn drop(&mut self) {
        self.close();
    }
}

/// Removes lockfiles a crashed or killed compypal left behind. Claude Code
/// prunes dead ones too, but only when it goes looking.
fn sweep_stale(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Ok(info) = serde_json::from_str::<Value>(&text) else { continue };
        if info.get("ideName").and_then(Value::as_str) != Some("compypal") {
            continue;
        }
        let alive = info
            .get("pid")
            .and_then(Value::as_u64)
            .is_some_and(|pid| std::path::Path::new(&format!("/proc/{pid}")).exists());
        if !alive && cfg!(target_os = "linux") {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The lockfile holds the auth token, so only its owner may read it.
fn write_private(path: &PathBuf, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    opts.open(path)?.write_all(bytes)
}

/// Completes the WebSocket handshake if the token matches, agreeing to the
/// `mcp` subprotocol.
fn accept(stream: TcpStream, token: &str) -> Option<WebSocket<TcpStream>> {
    // The callback's shape is tungstenite's; its error type is large by design.
    #[allow(clippy::result_large_err)]
    let check = |req: &Request, mut resp: Response| -> Result<Response, ErrorResponse> {
        let ok = req
            .headers()
            .get("x-claude-code-ide-authorization")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == token);
        if !ok {
            let mut err = ErrorResponse::new(Some("unauthorized".into()));
            *err.status_mut() = tungstenite::http::StatusCode::UNAUTHORIZED;
            return Err(err);
        }
        let wants_mcp = req
            .headers()
            .get("sec-websocket-protocol")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(',').any(|p| p.trim() == "mcp"));
        if wants_mcp {
            resp.headers_mut().insert("sec-websocket-protocol", "mcp".parse().unwrap());
        }
        Ok(resp)
    };
    tungstenite::accept_hdr(stream, check).ok()
}

/// Answers requests and forwards notifications until the socket closes.
fn serve(mut ws: WebSocket<TcpStream>, outbox: Receiver<String>, dispatch: &dyn Dispatch) {
    // Short reads, so queued notifications go out promptly.
    let _ = ws.get_ref().set_read_timeout(Some(Duration::from_millis(50)));
    loop {
        match ws.read() {
            Ok(Message::Text(text)) => {
                let Ok(msg) = serde_json::from_str::<Value>(text.as_str()) else { continue };
                if let Some(reply) = handle(&msg, dispatch)
                    && ws.send(Message::text(reply.to_string())).is_err()
                {
                    return;
                }
            }
            Ok(Message::Close(_)) => return,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => return,
        }
        while let Ok(n) = outbox.try_recv() {
            if ws.send(Message::text(n)).is_err() {
                return;
            }
        }
    }
}
