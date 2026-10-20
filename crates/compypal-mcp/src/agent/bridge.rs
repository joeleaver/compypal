//! MCP over stdio, forwarded to compypal's HTTP endpoint: for agents that
//! can only start MCP servers as subprocesses. The app runs this when
//! started as `compypal --mcp-stdio <url>`.

use std::io::{BufRead, Read, Write};
use std::net::TcpStream;

/// Forwards each JSON-RPC line on stdin to `url` and writes each answer to
/// stdout, until stdin closes.
pub fn run(url: &str) -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = post(url, &line)? {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

/// One POST to a plain-HTTP localhost URL; the body, unless it's empty (a
/// notification's 202).
pub fn post(url: &str, body: &str) -> std::io::Result<Option<String>> {
    let rest = url.strip_prefix("http://").ok_or_else(|| bad(format!("not an http URL: {url}")))?;
    let (host, path) = rest.split_once('/').map_or((rest, "/".to_string()), |(h, p)| (h, format!("/{p}")));
    let mut stream = TcpStream::connect(host)?;
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let (head, reply) = response.split_once("\r\n\r\n").ok_or_else(|| bad("malformed HTTP response".into()))?;
    let status = head.split_whitespace().nth(1).unwrap_or_default();
    match status {
        "200" => Ok(Some(reply.trim().to_string()).filter(|r| !r.is_empty())),
        "202" => Ok(None),
        other => Err(bad(format!("compypal answered {other}: {}", reply.trim()))),
    }
}

fn bad(msg: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::Value;

    use crate::Dispatch;

    struct Echo;

    impl Dispatch for Echo {
        fn call_tool(&self, name: &str, _args: Value) -> Result<String, String> {
            Ok(format!("called {name}"))
        }
    }

    #[test]
    fn forwards_requests_and_skips_notifications() {
        let port = 17766 + (std::process::id() % 1000) as u16;
        crate::http::serve(port, Arc::new(Echo)).unwrap();
        let url = format!("http://127.0.0.1:{port}/mcp");
        let reply = super::post(&url, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap().unwrap();
        let v: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["id"], 1);
        assert!(v["result"]["tools"].is_array());
        assert_eq!(super::post(&url, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).unwrap(), None);
    }
}
