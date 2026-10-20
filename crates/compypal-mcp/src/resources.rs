//! MCP resources: the song as ABC notation, whole or a track at a time,
//! for an agent (or any MCP client) to read without calling a tool.

use compypal_core::Project;
use compypal_io::abc::{self, AbcOptions};
use serde_json::{Value, json};

const MIME: &str = "text/vnd.abc";

/// What there is to read.
pub fn list(p: &Project) -> Vec<Value> {
    let mut out = vec![json!({
        "uri": "compypal://song.abc",
        "name": "song.abc",
        "title": format!("{} (ABC)", p.name),
        "description": "The whole song in ABC notation, a voice per track.",
        "mimeType": MIME,
    })];
    out.extend(p.tracks.iter().map(|t| {
        json!({
            "uri": format!("compypal://track/{}.abc", t.id),
            "name": format!("{}.abc", t.name),
            "title": format!("{} (ABC)", t.name),
            "description": format!("Track \"{}\" alone in ABC notation.", t.name),
            "mimeType": MIME,
        })
    }));
    out
}

/// One resource: its MIME type and text.
pub fn read(p: &Project, uri: &str) -> Result<(&'static str, String), String> {
    let opts = AbcOptions::default();
    if uri == "compypal://song.abc" {
        return Ok((MIME, abc::export(p, &opts)));
    }
    let id = uri
        .strip_prefix("compypal://track/")
        .and_then(|r| r.strip_suffix(".abc"))
        .and_then(|id| id.parse::<u64>().ok())
        .ok_or_else(|| format!("no resource {uri}"))?;
    let mut one = p.clone();
    one.tracks.retain(|t| t.id.0 == id);
    if one.tracks.is_empty() {
        return Err(format!("no track {id}"));
    }
    Ok((MIME, abc::export(&one, &opts)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Dispatch, handle};

    struct Demo;

    impl Dispatch for Demo {
        fn call_tool(&self, _: &str, _: Value) -> Result<String, String> {
            Err("no tools".into())
        }

        fn project(&self) -> Option<Project> {
            Some(compypal_core::demo::project())
        }
    }

    #[test]
    fn lists_and_reads_abc() {
        let r = handle(&json!({"jsonrpc": "2.0", "id": 1, "method": "resources/list"}), &Demo).unwrap();
        let list = r["result"]["resources"].as_array().unwrap().clone();
        assert_eq!(list[0]["uri"], "compypal://song.abc");
        assert!(list.len() > 1, "a resource per track too");

        let song = handle(&json!({"jsonrpc": "2.0", "id": 2, "method": "resources/read", "params": {"uri": "compypal://song.abc"}}), &Demo).unwrap();
        let text = song["result"]["contents"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("X:1") && text.contains("K:"), "{text}");

        let track = list[1]["uri"].as_str().unwrap();
        let one = handle(&json!({"jsonrpc": "2.0", "id": 3, "method": "resources/read", "params": {"uri": track}}), &Demo).unwrap();
        assert!(one["result"]["contents"][0]["text"].as_str().unwrap().starts_with("X:1"));

        let bad = handle(&json!({"jsonrpc": "2.0", "id": 4, "method": "resources/read", "params": {"uri": "compypal://nope"}}), &Demo).unwrap();
        assert!(bad["error"]["message"].as_str().unwrap().contains("no resource"));
    }
}
