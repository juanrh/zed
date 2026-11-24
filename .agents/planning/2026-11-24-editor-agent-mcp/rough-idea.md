# Rough idea: MCP server exposing Zed's agent tools (edit_file) from inside the editor

Give an external MCP client (any agent, automation, or CLI) control over the *real*
Zed editor agent tooling — starting with `edit_file` — by running an MCP **server
inside the Zed editor process** that delegates each `tools/call` to the actual
built-in agent tool implementation.

Key intent:

- This is *not* a reimplementation of `edit_file` semantics on files on disk
  (a standalone mirror already exists in `zed-edit-file-mcp/` on another branch).
  The value here is genuine reuse: the tool run must hit the editor's real
  code path — open buffers, format-on-save, undo history, action log, and the
  user's existing tool-permission rules.
- Editor coupling is a feature, not a bug: the human at the editor keeps
  visibility (cursor agent location updates, edit review surfaces) and control
  (tool permissions from `agent.tool_permissions` settings apply as usual).
- A setting should toggle whether this MCP server starts (default off);
  the served tool set should be configurable, defaulting to just `edit_file`.
- Transport: something reachable by standard MCP clients (stdio clients via a
  small proxy that bridges to an editor-owned local socket; no public network
  exposure — loopback/local socket only).
- Follow the PDD process; design and each implementation step must be reviewed
  with the ponytail-review skill (max 3 review rounds, stop on "Ship" verdict).
