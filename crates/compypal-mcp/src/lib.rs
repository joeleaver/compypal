//! compypal's agent interface: MCP tools over the project, served over
//! HTTP for the tools and over Claude Code's IDE WebSocket for context.

pub mod http;
pub mod ide;
pub mod protocol;
pub mod tools;

pub use protocol::Dispatch;
pub use tools::{App, MemoryApp, Selection};
