# Implementation plan

## Checklist

- [x] Step 1: Settings plumbing for `agent.editor_mcp_server`
- [x] Step 2: `agent` crate out-of-turn tool-run API on `Thread`
- [x] Step 3: MCP protocol module for `editor_mcp` (pure, unit-tested)
- [ ] Step 4: `editor_mcp` dispatch core (tool registry view + pump)
- [ ] Step 5: `editor_mcp` socket server + proxy crate
- [ ] Step 6: workspace wiring + README
- [ ] Step 7: review rounds (ponytail code review, round 2 and, if needed, 3)

## Working agreements

Proceed step by step:

- After each step, ask me for confirmation before completing it
- When completing a step, add an "Implementation notes" subsection to its section in the plan, summarizing what we did, focusing on deviations from the plan, why we made them, and implications for subsequent steps

(Execution is autonomous per the user's directive; the confirmation gate is
self-applied: each step ends with the implementation-notes subsection and the
checklist ticked before the next step starts. Each step is its own commit, and
the full editor build is exercised at the end of every step that touches
workspace crates: `cargo build -p <crate>` for speed, plus `cargo test -p <crate>`
where tests exist.)

## Step 1: Settings plumbing for `agent.editor_mcp_server`

**Objective:** the `agent.editor_mcp_server` settings block exists and compiles
into a typed `EditorMcpServerSettings { enabled: bool, tools: Vec<String> }`
with defaults `false` / `["edit_file"]`, readable via `AgentSettings::get_global(cx).editor_mcp_server`.

**Guidance:** add `EditorMcpServerContent` in
`crates/settings_content/src/agent.rs` (all-`Option` fields, schemars docs),
field on `AgentSettingsContent`, compiled struct + mapping in
`crates/agent_settings/src/agent_settings.rs` (follow the `ToolPermissions`
compile pattern, including a `Default` impl). No settings-UI page.

**Tests:** `agent_settings` unit test: content with/without the block parses
and compiles to the documented defaults and overrides.

**Integration:** standalone; later steps read the compiled settings.

**Demo:** a test (and only a test) shows `{"agent": {"editor_mcp_server": {"enabled": true}}}`
producing `enabled == true`.

## Step 2: `agent` crate out-of-turn tool-run API on `Thread`

**Objective:** `Thread::registered_tools()`, `Thread::registered_tool(name)`, and
`Thread::run_external_tool(name, input_json, cx) -> Option<ExternalToolRun>`
exist and run a registered tool with its own `ToolCallEventStream`, mirroring
the private `run_tool` sequence (restricted-mode check, fresh mpsc + watch
cancellation, initial InProgress status, `AnyAgentTool::run`), with
`ExternalToolRun { tool, stream, events, task, cancel() }`.

**Guidance:** in `crates/agent/src/thread.rs`; construct the event stream here
(private `ToolCallEventStream::new`, `ThreadEventStream::new`) so callers never
touch private internals; `AgentToolOutput` must be exported for callers. Respect
R11: no store inserts, no notifications, no title generation.

**Tests:** `#[gpui::test]` in `agent` (recipe: `crates/agent/src/tools/edit_file_tool.rs:3201`
test setup): build a project+thread, run `edit_file` via `run_external_tool`
with a permissive tool-permission setting; assert file changed, `Edited ...`
output, `Thread::new` produces no observable store/UI changes (R11 check),
authorization events round-trip (`ToolCallAuthorization` observed, then
`resolve_authorization` denial resolves the authorize future with an error).

**Integration:** standalone API; Step 4 is its first consumer.

**Demo:** a test drives a real buffer edit through the new API and asserts
permissions authorizations can be answered out-of-turn.

## Step 3: MCP protocol module (pure)

**Objective:** `crates/editor_mcp/src/protocol.rs` parses and builds the JSON-RPC
subset: `initialize` (+`notifications/initialized`), `ping`, `tools/list`,
`tools/call`, `notifications/cancelled`; error shapes; a `route(request) ->
Action` pure API; exhaustive unit tests.

**Guidance:** newline-delimited frames; `serde_json`; no GPUI. The module must
not know about sockets or GPUI — dispatch callbacks come in as data.
`tools/list`/`tools/call` payloads carry tool descriptors/results as data too.

**Tests:** framing, every method, malformed frames, unknown method,
missing-params paths, cancellation routing. All pure unit tests.

**Integration:** Step 5's socket server consumes it; Step 4's dispatcher feeds
it callbacks.

**Demo:** tests show a full client session (initialize → tools/list →
tools/call → cancel) replayed against in-memory callbacks.

## Step 4: `editor_mcp` dispatch core

**Objective:** the `EditorMcpDispatcher` (GPUI-side, testable without sockets):
owns/creates the hidden `Thread` for a `Project` (eager), exposes
`tools/list` data from the real registered tool map filtered by the settings
allowlist, and executes `tools/call` by delegating to
`Thread::run_external_tool`, pumping events (auto-deny `ToolCallAuthorization`
with the R2 message), converting `AgentToolOutput` to MCP results, tracking
`pending` with cleanup on completion/cancel.

**Guidance:** `crates/editor_mcp/src/dispatch.rs`; construction needs
`Entity<Project>` (+ `ContextServerRegistry`, `Templates`, `ProjectContext` —
follow the test recipe). The dispatcher is deliberately socket-free so GPUI
tests can drive it directly.

**Tests:** GPUI integration tests per R8 layer 2: allow-rule edit (file +
`Edited ...`), deny-rule (isError), confirm-default (isError + pre-authorize
message), unknown tool, invalid args, cancellation, `pending` cleanup after
completion/cancel (R10), allowlist filtering on tools/list (R12: eager thread).

**Integration:** consumes Step 2 API and Step 1 settings; consumed by Step 5.

**Demo:** tests show a permission-respecting tool-driven edit with a real
buffer; tools/list reflects allowlist.

## Step 5: socket server + proxy

**Objective:** `crates/editor_mcp/src/server.rs`: background UDS accept loop
(smol `Async<UnixListener>`, `background_spawn`), bounded channels (R10), the
dual-window bind tiebreak/reclaim (R10), per-connection serial request
execution with at most one in-flight tool each, disconnect/cancel →
`PendingRun::cancel` (R7). Plus `crates/zed-editor-mcp/`: std-only proxy
binary (`--socket PATH`), relaying stdin↔socket line-by-line with a clear
failure message when Zed isn't serving.

**Guidance:** socket path `<data_dir>/editor_mcp/<sha256(worktree_roots joined)>.sock`
(sha2 already in tree), dir created on demand, socket removed on close. Transport
isolation behind a tiny trait is optional — keep the socket code in one module
instead of inventing an abstraction used once (lean).

**Tests:** socket end-to-end test (R8 layer 3): temp UDS, scripted client:
initialize → tools/list → tools/call (allow) → assert reply AND file state;
cancel mid-call returns promptly; disconnect mid-call cancels the run. Proxy
smoke test: spawn proxy against the test server, bytes flow, exit codes on
dead socket. Proxy builds standalone: `cargo build -p zed-editor-mcp`.

**Integration:** consumes Steps 1–4. Demo: the end-to-end test is the demo.

## Step 6: workspace wiring + README

**Objective:** enabled server auto-starts per workspace at open (R3), clean
teardown at close; `crates/zed-editor-mcp/README.md` documents the setting,
the socket path, the proxy usage for stdio MCP clients, the permission
pre-authorization requirement (R2), and limitations (confirm-auto-deny,
Windows).

**Guidance:** hook in the workspace initialization path (find the established
observer pattern in `crates/zed`/`workspace` and follow it); keep the entity
construction settings-gated; duplex handling of settings changes observed
(enabled mid-session → start; disabled → close socket, cancel in-flight runs).

**Tests:** unit test that the gate logic reacts to settings; end-to-end test
from Step 5 gains a "disabled → connection refused" case proxy-side.

**Integration:** closes the loop: settings → wiring → server → dispatcher →
real tool. Demo: with the setting on and an allow rule, a stdio MCP client
connects, lists, edits a real project file through the proxy.

## Step 7: review rounds

**Objective:** run the ponytail code review (round 2) over the full
implementation diff; fix or justify every finding; repeat once more (round 3)
only if round 2's verdict isn't "Ship" (the skill caps engagement at 3 rounds
with the design review included; design review was round 1).

**Guidance:** review findings and fixes are recorded verbatim under
`reviews/round-N-code.md` with an incorporation note, mirroring round 1's
format.

**Tests:** whatever the review flags as untested risky code becomes a test
before the round closes.

**Demo:** review verdict "Ship" (or 3 rounds exhausted with all findings
incorporated), recorded in `reviews/`.

## Implementation notes — Step 1

Settings plumbing done: `EditorMcpServerContent { enabled, tools }` in
`crates/settings_content/src/agent.rs` (needed `MergeFrom`/`PartialEq` derives
to satisfy the settings machinery — same as sibling content structs), field on
`AgentSettingsContent`, compiled `EditorMcpServerSettings` +
`compile_editor_mcp_server` in `crates/agent_settings` (defaults:
`enabled=false`, `tools=["edit_file"]`), and three unit tests
(`test_editor_mcp_server_{defaults,parsing,partial_content}`).
No deviations from the plan. `cargo test -p agent_settings --lib`: 47 passed.

## Implementation notes — Step 2

Done: `Thread::registered_tool(s)` + `Thread::run_external_tool` + `ExternalToolRun`
(`crates/agent/src/thread.rs`), mirroring the private `run_tool` sequence
(restricted-mode check with a ready-Err run, fresh event stream + watch
cancellation, InProgress status, `AnyAgentTool::run`). Deviations from plan:
- `ExternalToolRun::events` is a raw `UnboundedReceiver<ThreadEvent>` (the
  existing wrapper `ToolCallEventStreamReceiver` is test-gated; exposing it
  would have widened the fork surface).
- A restricted workspace returns `Some(run)` with a ready Err output (not
  `None`, which the plan left unspecified) so callers can distinguish
  "not a registered tool" from "tool refused to run".
- Discovered while testing: `Thread::new` registers NO tools by itself;
  `add_default_tools(environment, cx)` must be called (as `NativeAgent` does),
  and it needs a `ThreadEnvironment`. The `agent` test module introduces
  `FailingThreadEnvironment` (terminals/subagents unsupported); `editor_mcp`
  will ship its own minimal one in Step 4. In-project edits are allowed by
  default permission settings, so Step 4's confirm coverage uses a sensitive
  path (`.zed/settings.json`), which always prompts.
R11 check: the hidden-thread recipe here creates no ThreadStore/UI changes
(Project::test + plain `cx.new::<Thread>`; no observe subscriptions fire —
subscriptions live in `NativeAgent::register_session`, which we do not call).
Tests: 3 new `#[gpui::test]`s in `agent` (allow edit — file changed +
`Edited ...` text; deny rule — tool error, file untouched; sensitive-path
confirmation → deny → tool error; unknown tool → None). Full `agent` lib
suite: 821 passed, 0 failed.

## Implementation notes — Step 3

Done: `crates/editor_mcp` crate (workspace member; deps limited to
agent/agent_settings/gpui/project/prompt_store + std-level crates) and the
pure `protocol` module: `parse_message` producing `Message::{Request,
Notification, UnknownMethod, Invalid}`, builders for
initialize/ping/tools-list/tools-call/error responses, and a `ToolCallOutcome`
type mapping tool output to MCP `isError` semantics. One addition the plan
didn't name: `Action::CallToolError` for `tools/call` frames that miss the
tool name — keeps protocol handling total and answers with a tool-result
error, like the built-in agent answers bad input. All 8 protocol unit tests
pass. No deviations otherwise.
