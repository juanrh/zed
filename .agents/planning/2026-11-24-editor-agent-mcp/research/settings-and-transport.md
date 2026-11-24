# Research: settings plumbing, socket transport, MCP protocol minimum

## Settings plumbing (verified in fork)

- Compiled settings: `AgentSettings` in `crates/agent_settings/src/agent_settings.rs:211`,
  built from `SettingsContent` via `impl Settings for AgentSettings::from_settings`
  (line 770) — `content.agent` is an `AgentSettingsContent`.
- User-facing settings schema: `AgentSettingsContent` in
  `crates/settings_content/src/agent.rs:219` — plain `Option<T>` fields with
  schemars docs; adding a nested settings block means:
  1. new `*Content` struct (all `Option`, derive `Serialize/Deserialize/JsonSchema`),
  2. new field on `AgentSettingsContent`,
  3. new compiled struct(s) in `agent_settings.rs`,
  4. mapping in `from_settings`,
  5. registration of keys is automatic through the existing settings store
     (`RegisterSetting` on `AgentSettings` already covers the `agent` key; no
     settings-UI page work required for v1).
- Precedent for tool permission rules governing edit_file:
  `crates/agent/src/tool_permissions.rs` (`decide_permission_from_settings`,
  `decide_permission_for_path`), configured via `agent.tool_permissions`
  (`ToolPermissions { default, tools: map of tool id -> { default } }`).
  The MCP server reuses these rules *unchanged* — we add no parallel permission
  system. Note: third-party/`mcp:*` tool ids exist in settings for MCP-served
  tools; our server presents *built-in* tool names, so the user writes allow rules
  against the same tool ids (`edit_file`) as the built-in agent.

## Local socket transport options

- **Unix domain socket (chosen).** `std::os::unix::net::UnixListener`, addressed
  by a filesystem path; permissions enforced by the OS on the socket file.
  Zed places per-user runtime data under `paths::data_dir(...)`/`paths::support`
  (see `crates/paths`); we derive a per-workspace subdirectory from the
  workspace's primary(?) — actually from *all* worktree roots — hashed for a
  stable, filesystem-safe name.
  - Accepted in the fork's precedent: the rejected PR #2 used a UDS at
    `paths::data_dir()/zed-agent-tool-bridge.sock`; our design keeps the
    transport but replaces the *deployment shape* (server in-editor, no
    patched-in external protocol surface beyond MCP itself).
  - Cross-platform limitation: no UDS on Windows platforms in the std lib.
    Windows support is **out of scope** for v1 (documented); the design
    isolates the transport behind a small trait so a named-pipe transport can
    be added without touching dispatch.
- **TCP loopback.** Rejected for v1: anything listening on TCP — even 127.0.0.1 —
  is a network service with port-scanning exposure; a UDS is invisible off-host
  and inherits filesystem permissions.
- **Streamable HTTP/SSE.** Rejected: brings an HTTP stack into the editor
  process for no added capability over a UDS for local clients; see Q4 in
  idea-honing.md.

## Proxy for stdio MCP clients

Standard MCP clients (Claude Desktop, Codex, Zed itself) launch `stdio` servers
by spawning a command. An in-editor server cannot be spawned per client, so the
repository ships a tiny proxy: `spawn zed-editor-mcp-proxy --socket <path>` →
the proxy relays stdin→socket and socket→stdout verbatim, frame-by-frame
(newline-delimited JSON), exiting when either side closes. Zero crate
dependencies (std only) so it compiles alone in seconds and adds no supply-chain
surface. Stale sockets are handled by the proxy attempting a connection and
reporting "Zed is not running / MCP server not enabled for this workspace"
on failure.

## MCP protocol minimum (JSON-RPC 2.0, newline-delimited frames)

- `initialize` request → result `{ protocolVersion, capabilities: { tools: {} },
  serverInfo }`. Client then sends `notifications/initialized`.
  - protocolVersion: respond with the client's requested version if recognized,
    else a known one (`2024-11-05`); MCP clients tolerate this.
- `ping` → `{}`.
- `tools/list` → `{ tools: [ { name, description, inputSchema } ] }`.
  `inputSchema` comes straight from the real `AnyAgentTool::input_schema()` — the
  *actual* `edit_file` JSON schema the built-in agent uses.
- `tools/call { name, arguments }` →
  `{ content: [ { type: "text", text } ], isError? }`.
  - Unknown tool → result with `isError: true` + message (MCP prefers a tool
    result error over a protocol error for domain-level failures).
  - Invalid arguments → same, with parse error detail (the real tool's
    `ToolInput` validation produces the exact message the built-in agent would
    emit — including "provide `path` as a string..." style messages).
- `notifications/cancelled { requestId }` → triggers the tool event stream's
  cancellation watch; the in-flight `tool.run` task observes cancel and
  finishes promptly (tools select on `ToolCallEventStream::cancelled_by_user`).
- Serial-per-connection request handling is legitimate MCP; concurrent
  *connections* are supported by the server (each with its own pump).

## Zed-side seams the research identified (fork surface additions)

1. `agent` crate: a public constructor for an out-of-turn
   `ToolCallEventStream` (+ receiver + cancellation sender), defaulting
   sandbox grants; a public accessor for a `Thread`'s registered tool map;
   a public `Thread::run_external_tool(name, input_json, ...) -> Task<...>`
   that closes over the private `run_tool`-style sequence. This is the *entire*
   agent-crate API addition; everything else lives in the new `editor_mcp` crate.
2. `agent_settings`/`settings_content`: the `editor_mcp_server` settings block.
3. `workspace`/`zed` boot: on workspace open, if enabled, construct the
   `EditorMcpServer` entity for that workspace; drop entity+socket on close.
