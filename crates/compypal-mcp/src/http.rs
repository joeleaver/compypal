//! MCP over HTTP ("streamable HTTP", JSON responses only) on localhost.
//!
//! Anything on this machine can reach a localhost port, including web
//! pages. Requests must name a local Host (no DNS rebinding), come from no
//! Origin or a local one, and carry a JSON content type, which a page can
//! only send after a CORS preflight that this server never approves.

use std::sync::Arc;

use serde_json::Value;
use tiny_http::{Header, Method, Response, Server};

use crate::protocol::{Dispatch, handle_body};

pub const DEFAULT_PORT: u16 = 7766;

/// Starts serving on `127.0.0.1:port` in a background thread.
pub fn serve(port: u16, dispatch: Arc<dyn Dispatch>) -> Result<(), String> {
    let server = Server::http(("127.0.0.1", port)).map_err(|e| format!("can't listen on 127.0.0.1:{port}: {e}"))?;
    std::thread::Builder::new()
        .name("compypal-mcp-http".into())
        .spawn(move || {
            for mut req in server.incoming_requests() {
                let header = |name: &str| {
                    req.headers()
                        .iter()
                        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
                        .map(|h| h.value.as_str().to_string())
                };
                let local = |v: &str| {
                    let host = v.trim_start_matches("http://").split(':').next().unwrap_or("");
                    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
                };
                let reply = |code: u16, body: &str| Response::from_string(body).with_status_code(code);
                if !header("host").is_some_and(|h| local(&h)) || header("origin").is_some_and(|o| !local(&o)) {
                    let _ = req.respond(reply(403, "forbidden"));
                    continue;
                }
                if !matches!(req.url(), "/mcp" | "/") {
                    let _ = req.respond(reply(404, "not found"));
                    continue;
                }
                match req.method() {
                    Method::Post => {
                        if !header("content-type").is_some_and(|c| c.contains("application/json")) {
                            let _ = req.respond(reply(415, "expected application/json"));
                            continue;
                        }
                        let mut body = String::new();
                        if req.as_reader().read_to_string(&mut body).is_err() {
                            let _ = req.respond(reply(400, "unreadable body"));
                            continue;
                        }
                        let parsed: Value = match serde_json::from_str(&body) {
                            Ok(v) => v,
                            Err(e) => {
                                let _ = req.respond(reply(400, &format!("bad JSON: {e}")));
                                continue;
                            }
                        };
                        match handle_body(&parsed, dispatch.as_ref()) {
                            Some(resp) => {
                                let json: Header = "Content-Type: application/json".parse().unwrap();
                                let _ = req.respond(Response::from_string(resp.to_string()).with_header(json));
                            }
                            None => {
                                let _ = req.respond(reply(202, ""));
                            }
                        }
                    }
                    // No server-initiated stream: tools/list never changes.
                    Method::Get => {
                        let allow: Header = "Allow: POST, DELETE".parse().unwrap();
                        let _ = req.respond(reply(405, "").with_header(allow));
                    }
                    Method::Delete => {
                        let _ = req.respond(reply(200, ""));
                    }
                    _ => {
                        let _ = req.respond(reply(405, ""));
                    }
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}


