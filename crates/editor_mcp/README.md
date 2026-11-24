# Zed editor MCP server (`editor_mcp`)

An in-editor [Model Context Protocol](https://modelcontextprotocol.io) server
that exposes Zed's **built-in agent tools** — `edit_file` to start — to
external MCP clients. Every `tools/call` runs the *real* tool implementation
on the *real* project state: open buffers, format-on-save, undo history, the
action log, and — importantly — the user's existing tool-permission rules.

This is the difference between "mirroring Zed's tool over MCP" and "letting an
external agent drive the editor": the tool run goes down the exact code path
the built-in agent uses (`crates/agent/src/tools/edit_file_tool.rs` via
`agent::Thread::run_external_tool`), so a human at the editor keeps visibility
(cursor/agent-location updates, edit review surfaces) and veto rights.

## Enabling

```json
{
  "agent": {
    "editor_mcp_server": {
      "enabled": true,
      "tools": ["edit_file"]
    }
  }
}
```

- `enabled` (default **false**) starts the server per workspace.
- `tools` (default `["edit_file"]`) is the allowlist served over MCP; entries
  must be registered agent tool names (the intersection with the real tool
  map is served).

When enabled, each `Workspace` serves a Unix-domain socket at

```
<Zed data dir>/editor_mcp/<sha256 of the workspace's sorted worktree roots>.sock
```

Two Zed windows on the same project never fight over the socket: the second
window detects the live one, logs, and stands down (settings changes re-sync
both windows).

## Connecting an MCP client

Standard MCP clients spawn *stdio* servers, and an in-editor server can't be
spawned per client — so the repo ships a std-only relay binary:

```
zed-editor-mcp --socket <path from the log line above>
# or: ZED_EDITOR_MCP_SOCKET=<path> zed-editor-mcp
```

Point your client at that command (e.g. as a local MCP server). The relay
moves newline-delimited JSON-RPC frames verbatim; parse errors or a missing
socket produce a clear message and non-zero exit.

Protocol surface: `initialize`, `notifications/initialized`, `ping`,
`tools/list`, `tools/call`, `notifications/cancelled`. `tools/list` carries
the tools' *real* JSON schema and descriptions.

## Permissions (read this)

MCP-driven tool calls go through the tools' own permission machinery
(`agent.tool_permissions`), with one deliberate adaptation: a request that
would normally raise an **interactive confirmation prompt** is **auto-denied**
with instructions, because there is no human at the other end of a socket to
click "Allow". Pre-authorize with settings rules, e.g.:

```json
{
  "agent": {
    "tool_permissions": {
      "tools": { "edit_file": { "default": "allow" } }
    }
  }
}
```

`deny` rules reject calls with the tool's own message. In-project edits are
allowed by default Zed permission settings; sensitive paths (e.g.
`.zed/settings.json`) still prompt and therefore fail over MCP until an
explicit allow rule covers them.

## Manual QA (five minutes)

Full socket sessions can't run under `#[gpui::test]` (the deterministic test
scheduler forbids the transport threads' cross-thread wakes), so verify a
live editor by hand:

1. Open a project in Zed with `editor_mcp_server.enabled = true` (and an
   `edit_file` allow rule). Zed.log prints the socket path.
2. Build the relay: `cargo build -p zed-editor-mcp`.
3. Drive a session:
   ```sh
   printf '%s\n' \
     '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}}}' \
     '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
     '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"edit_file","arguments":{"path":"<root-relative path>","edits":[{"old_text":"a","new_text":"b"}]}}}' \
     | ./target/debug/zed-editor-mcp --socket <socket path>
   ```
4. Expect: an `initialize` result, then a `tools/call` result whose text
   reads `Edited … successfully`; the edited buffer changes in the editor,
   undo works, and the edit is on the review surface.

## Limits (v1)

- Linux/macOS only (Unix-domain sockets). A named-pipe transport for Windows
  is a deliberate future seam; nothing in dispatch assumes Unix.
- One in-flight `tools/call` per connection; open more connections for
  parallel calls.
- `confirm`-class permission requests auto-deny (see above); no interactive
  prompt UI for socket-originated calls yet.
- The hidden thread supports neither `terminal` nor subagents; allowlisting
  those tools is possible but their runs will fail honestly.

## Layout

- `crates/editor_mcp/src/protocol.rs` — pure MCP JSON-RPC subset (unit-tested).
- `crates/editor_mcp/src/dispatch.rs` — hidden `Thread` + real-tool dispatch,
  permission auto-deny pump (GPUI integration-tested).
- `crates/editor_mcp/src/server.rs` — UDS server, per-connection slots,
  cancellation, dual-window tiebreak.
- `crates/editor_mcp/src/store.rs` — settings-driven lifecycle per workspace.
- `crates/zed-editor-mcp/` — stdlib-only stdio↔socket relay (lib-tested + manually verified).
- `crates/agent/src/thread.rs` — the small fork addition enabling out-of-turn
  tool runs (`run_external_tool`, `ExternalToolRun`).
