//! editor_mcp: an in-editor MCP server that exposes Zed's built-in agent tools
//! (starting with `edit_file`) to external MCP clients, by delegating every
//! tool call to the real tool implementation through `agent::Thread`.

pub mod dispatch;
pub mod protocol;
