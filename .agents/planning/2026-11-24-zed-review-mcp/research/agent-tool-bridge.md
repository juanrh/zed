# Research: exposing the Zed agent's own tools (edit_file, grep, diagnostics, …)

Triggered by the reviewer's question: rather than re-implementing an Aletheia-style
baseline protocol inside zed, expose the tools the Zed agent already has
(https://zed.dev/docs/ai/tools) so external agents reuse the exact same code paths,
including the agent's pending-edit review diff. This file records what the codebase says
about that idea, per tool, and what it would take to cover the whole documented tool list.

## Where the agent's tools live

- Implementations: `crates/agent/src/tools/*.rs` — one file per tool
  (`edit_file_tool.rs`, `grep_tool.rs`, `diagnostics_tool.rs`, `read_file_tool.rs`,
  `write_file_tool.rs`, `terminal_tool.rs`, `find_path_tool.rs`, `list_directory_tool.rs`,
  `copy_path_tool.rs`, `move_path_tool.rs`, `delete_path_tool.rs`, `create_directory_tool.rs`,
  `fetch_tool.rs`, `web_search_tool.rs`, `skill_tool.rs`, `spawn_agent_tool.rs`,
  `ask_user_tool.rs`, `list_agents_and_models_tool.rs`, `create_thread_tool.rs`,
  plus LSP tools: `go_to_definition_tool.rs`, `find_references_tool.rs`, `rename_tool.rs`,
  `get_code_actions_tool.rs`, `apply_code_action_tool.rs`).
- Trait: `AgentTool` (`crates/agent/src/thread.rs:5511`):

  ```rust
  pub trait AgentTool: 'static + Sized {
      type Input: Deserialize + Serialize + JsonSchema;
      type Output: Deserialize + Serialize + Into<LanguageModelToolResultContent>;
      const NAME: &'static str;
      fn run(self: Arc<Self>, input: ToolInput<Self::Input>,
             event_stream: ToolCallEventStream, cx: &mut App)
          -> Task<Result<Self::Output, Self::Output>>;
      fn input_schema() -> Schema; // schemars-derived from Input
  }
  ```

  Every tool's constructor takes live app entities — `Project`, `Thread`, `Workspace`,
  `ActionLog`, permission agents (`GrepTool::new(project: Entity<Project>)`,
  `EditFileTool { project, thread, workspace, permission_agent }`,
  `DiagnosticsTool::new(project)`) — and `run` needs `&mut App`.

## Key findings

1. **"Just a library extraction" is not available.** The tools share the live entity
   graph: grep executes through `project::project_search` over buffer snapshots;
   diagnostics pulls LSP state through `Project`; `edit_file` records its diff into the
   thread's `EditSession` (created pending-edits reviewed in the agent panel UI).
   These crates (`agent`, `project`, `language`) all depend on `gpui`, which binds to the
   windowing system (xkbcommon/wayland/X11/alsa…) — they cannot even be *compiled* in
   headless CI like this sandbox (verified: no GUI system libraries present).
   Extracting the three tools into a `gpui`-free library shared with the MCP server means
   extracting `Project`, buffers, syntaxes and LSP too — a re-architecture of Zed's core,
   not a refactor of a tool file.

2. **But the tools are perfect for in-process reuse.** `AgentTool` is a clean,
   schemars-described interface (`NAME`, `Input`, `Output`, `run(input, cx)`), and the
   agent constructs concrete tools per thread in `crates/agent/src/thread.rs`'s tool
   registry (`tools.rs` `ALL_TOOL_NAMES` macro). The right architecture is therefore a
   **local in-app bridge**: the running Zed listens on a loopback socket, receives
   `tool/run` requests, constructs the *agent's own tool objects* for the active
   workspace project (`GrepTool::new(project)`, `EditFileTool::new(project, thread, …)`),
   runs them through `AgentTool::run`, serializes the outputs back. External agents then
   get byte-identical behavior — including `edit_file`'s review flow: pending edits
   land in the agent panel with Zed's diff review, exactly as for the built-in agent,
   because it is literally the same code.

3. **No out-of-process surface exists to hook today.** `NativeAgentServer`
   (`crates/agent/src/native_agent_server.rs`) is an in-process `AgentServer` behind
   `acp_thread::AgentConnection`; Zed is an ACP *client* for external agents. The app's
   only existing loopback IPC is the CLI open-path channel
   (`crates/zed/src/zed/open_listener.rs:417`, a `UnixDatagram` at
   `zed-<channel>.sock` + per-OS IPC handshake) which carries `CliRequest::Open`
   URLs — not a general request/response protocol. A new, tiny listener in the `zed`
   app crate (pattern: `listen_for_cli_connections`) is needed for tool calls. It is
   ~300–500 lines of app-crate code following an existing pattern, but like all
   `gpui`-dependent crates it **cannot be compiled or tested in this environment**
   (missing system GUI libraries — same constraint documented in PR #1), so it must be
   landed from a machine that can build the app; everything else can land first.

4. **The bridge protocol side can be fully built and tested headless.** The MCP server
   gains gateway tools that forward to the socket; schema parity is free by copying the
   tools' `Input` structs (they are plain serde+schemars types, no gpui in the input
   types). This is implemented in this PR (`crates/zed_review_mcp/src/agent_bridge.rs`).

## Tool-by-tool coverage sketch (https://zed.dev/docs/ai/tools + LSP tools in this fork)

Legend — mechanism: **B** = in-app bridge (construct agent tool, run in app);
feasibility when the app-side listener lands: trivial (tool has no extra session input);
easy (needs thread/worktree context the bridge already has); medium (needs per-tool
plumbing, e.g. permissions or interactive UI).

| Tool (`AgentTool::NAME`) | Mechanism | Notes / effort beyond the listener |
| --- | --- | --- |
| `grep` | B | `GrepTool::new(project)`; input: regex, include_pattern, offset, case_sensitive, max_ancestor_lines, context_lines — trivial |
| `diagnostics` | B | `DiagnosticsTool::new(project)`; refreshes LSP diagnostics in-app — trivial |
| `edit_file` | B | needs a `Thread` + permission agent; edits surface as pending-edit **diff review in the agent panel** (the reviewer's whole point) — easy |
| `read_file`, `write_file`, `list_directory`, `find_path` | B | trivial |
| `create_directory`, `copy_path`, `move_path`, `delete_path` | B | trivial (permission agent wiring only) |
| `fetch` | B | trivial; inherits Zed's tool-permission config for the bridge session |
| `terminal` | B | must run through the bridge's permission agent — easy |
| `search_web` | B (Zed Pro) | provider/billing check `supports_provider` repeats in-app — easy |
| `skill`, `list_agents_and_models`, `spawn_agent`, `create_thread`, `ask_user` | B, per-thread session semantics | medium: `spawn_agent`/`create_thread` spawn nested threads; `ask_user` requires the human over MCP — map to an MCP *elicitation**/*output-only* error in v1 |
| `go_to_definition`, `find_references`, `rename`, `get_code_actions`, `apply_code_action` | B | LSP tools gated by feature flags (`LspToolFeatureFlag`); behavior-flagged per-profile today — easy |

Phasing: (1) bridge + `grep`/`diagnostics` (no session state), (2) `edit_file` +
read/write/path tools (pending-edit review = the requested review flow), (3) terminal +
LSP tools (+ permission-agent policy decision for external callers), (4) conversation
tools (`spawn_agent`, `ask_user`, `skill`) once bridge session semantics are defined.

## Answer to the reviewer's question ("why the Aletheia-style mode at all?")

Because until the app-side listener exists, disk-editing is the only thing an external
agent can do that Zed can review at all — and the baseline tools produce that review with
zero app-code risk (they shipped fully tested here). With the bridge in place, the
Aletheia-style tools become an offline/degraded fallback for sessions where Zed is not
running; the design now states this explicitly instead of presenting both as equals.
If the fork decides bridged mode is the only supported mode once it lands, deleting the
fallback tools is a one-commit follow-up (their isolation in this crate makes that cheap).
