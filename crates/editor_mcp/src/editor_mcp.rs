//! editor_mcp: an in-editor MCP server that exposes Zed's built-in agent tools
//! (starting with `edit_file`) to external MCP clients, by delegating every
//! tool call to the real tool implementation through `agent::Thread`.
//!
//! Boot the per-workspace servers with [`store::init`], gated by the
//! `agent.editor_mcp_server` settings block.

pub mod dispatch;
pub mod protocol;
pub mod store;
pub mod server;
