//! compypal's agent interface: MCP tools over the project, served over
//! HTTP for the tools and over Claude Code's IDE WebSocket for context.

// The tool list is one large json! literal.
#![recursion_limit = "512"]

pub mod http;
pub mod ide;
pub mod protocol;
pub mod tools;

pub use protocol::Dispatch;
pub use tools::{App, MemoryApp, Selection};
