# Idea honing: requirements clarification

This project was executed in **autonomous mode** (explicit instruction: "complete
this task fully unattended, no additional user input; use your best judgement").
The PDD skill's interactive clarification gates were therefore self-answered.
Each question below records the decision taken, the rationale, and the
alternatives considered, so a later reader can audit or overturn any decision.

Prior conversation with the user already settled the largest decisions and is
treated as answered input: the built-in `edit_file` execution path must be
reused, not mirrored (the standalone mirroring approach was delivered earlier on
branch `vibe/zed-edit-file-mcp-8dff25` / PR #3, and the reactive socket-bridge
approach of PR #2 was rejected as the "wrong approach"); editor coupling is
desired ("this MCP is about giving an agent control over the editor, just like
the Zed agent has"); and Option A dispatch (real `Thread`/real tool run, not
freeform conversation turns) was explicitly chosen.

## Q1. What exactly does the MCP server expose — only `edit_file`, or every agent tool?

**Decision:** A configurable allowlist of tool names, defaulting to exactly
`["edit_file"]`, matched against a real `Thread`'s registered tool map.
`tools/list` reflects the allowlist (intersected with actually-registered tools).

**Rationale:** `edit_file` is the requested capability. Exposing `terminal`,
`delete_path`, or `fetch` over a socket by default would be a large, risky
attack surface. An allowlist keeps the door open for other tools later with
zero extra machinery — the dispatch path is name-generic.

**Alternatives considered:** expose all default tools (rejected: unsafe
default); hardcode `edit_file` only (rejected: less extensible with no safety
benefit — the allowlist is the same mechanism and defaults to the safe set).

## Q2. How does permission gating work for tool calls that arrive over the socket?

**Decision:** Reuse the tools' own permission machinery untouched. Every tool
run goes through its real `authorize` flow, which consults the user's
`agent.tool_permissions` settings first. `allow` rules run the tool; `deny` rules
reject it. A `confirm` decision — which would normally raise an interactive
prompt in the Agent Panel — is **auto-denied** by the server with an actionable
error message ("add an allow rule for this tool/path in Tool Permissions to
enable MCP-driven calls").

**Rationale:** Correctness and safety demand the real permission path, not a
bypass. Surfacing interactive confirms for socket-driven calls would require new
prompt UI wired from a non-thread event pump; that is a large, risky surface for
the first iteration, and the failure mode of shipping it half-right (a prompt
nobody renders, leaving calls hanging) is worse than a clean denial. The denial
keeps behavior explicit and auditable: a user must deliberately pre-authorize
MCP-driven edits, which matches the trust model of "an external agent is
driving my editor".

**Alternatives considered:** route confirmations to the Agent Panel UI
(rejected for v1, recorded as future work — a large, risky surface); auto-allow
everything (rejected: unsafe silent bypass); server-level kill switch separate
from tool permissions (rejected: reinvents the permission layer that already
exists).

## Q3. Where does the tool run — a dedicated `Thread` per what scope?

**Decision:** One hidden, **workspace-scoped** `Thread` entity is created
lazily per `Workspace` (at first MCP use, or at server start if enabled), via
the same public `Thread::new` used by `NativeAgent::new_session`. The socket
lives under a per-workspace path derived from the workspace root directories, so
`path` semantics match a single, well-defined project.

**Rationale:** `EditFileTool` (and its `EditSessionContext`) requires a real
`Entity<Thread>` — it drives `project.set_agent_location`, buffer opens, saves,
format-on-save, and the `ActionLog`. A hidden thread gives exactly what the
tool needs without creating Agent Panel UI or fake model turns. Workspace
scoping keeps any MCP edit unambiguous about which multi-root project it hits.

**Alternatives considered:** reuse `NativeAgent`'s sessions (rejected: sessions
are model-turn-oriented; a prompt would put an LLM in the loop and the model,
not the MCP caller, would decide to call the tool — not the requested semantic);
create a thread per MCP connection (rejected: action-log/diff continuity is
better with one per workspace, and `Thread` construction is nontrivial;
connections still get fresh event streams per call).

## Q4. What transport and framing does the server speak?

**Decision:** MCP JSON-RPC with newline-delimited frames over a **Unix domain
socket** owned by the editor, one socket per workspace. A small standalone
`zed-editor-mcp` proxy binary (separate zero-dependency crate) is shipped so
standard stdio MCP clients can `spawn` the proxy, which relays stdin/stdout to
the socket. This avoids any TCP listener at all.

**Rationale:** MCP stdio clients spawn commands; an in-editor server must be
reachable without a network listener. A UDS keeps everything local-machine and
permissions-checked by the filesystem. The proxy is ~100 lines with no
editor dependencies, so it builds independently of the monorepo.

**Alternatives considered:** SSE/streamable-HTTP listener in the editor
(rejected: heavyweight async HTTP stack in-process plus a real network
listener — more surface, no added capability over a UDS for the local-machine
use case); making Zed itself a stdio MCP server (impossible: stdio servers are
spawned by clients; Zed is not spawned per client).

## Q5. Which MCP method subset is implemented, and how are concurrent calls handled?

**Decision:** `initialize`, `notifications/initialized`, `ping`, `tools/list`,
`tools/call`, and `notifications/cancelled` on a minimal JSON-RPC 2.0 loop.
Each connection handles requests serially per client, but multiple
connections may run concurrently; concurrent tool runs are serialized onto the
GPUI main thread by design (entity updates), which is the natural editor
behavior. Per-call cancellation maps to the tool event stream's cancellation
watch channel.

**Rationale:** This is the minimum a real MCP client needs to get a working
`edit_file`. Serial-per-connection keeps the protocol logic simple and
predictable; GPUI serialization needs no extra locking and matches how all
editor state changes already work.

**Alternatives considered:** full `rmcp` server crate dependency (rejected:
heavy dependency to add to the editor monorepo for a small protocol subset we
can implement and test precisely; the fork's existing MCP client usage does not
need a server library); per-connection concurrent request execution (rejected
as v1 complexity with no demonstrated need).

## Q6. What does the server return for a tool run, and how are failures reported?

**Decision:** The tool's true `AgentToolOutput` is returned: `llm_output`
content parts mapped to MCP text content; an `Err` becomes an `isError: true`
result carrying the tool's own error text (for `edit_file`, the familiar
"Failed to edit ... no match found / not unique" messages). Malformed
arguments return a JSON-RPC/`tools/call` error naming the parse failure.
Unknown tool names return an error enumerating available tools.

**Rationale:** The MCP caller must see the real outcome of the real tool,
including its error excerpt format that LLMs already know how to react to.
Inventing a second error format would break that contract.

**Alternatives considered:** wrapping errors as protocol-level exceptions
(rejected: hides tool semantics from the calling agent; mixed states are what
`isError` is for).

## Q7. Scope guards: what is explicitly *not* in v1?

**Decision:** Not in v1: interactive confirm prompts routed to the editor UI
(auto-deny instead, Q2), exposing subagent/context-server tools, ACP session
exposure, remote/TCP transport, per-tool per-connection session state,
and Agent Panel UI for MCP activity. The hidden thread is not opened in the
panel in v1; the editor's existing surfaces (agent location, edit review via
action log, undo) already reflect the effects of the tools.

**Rationale:** Each excluded item roughly doubles the integration surface
without changing the core delegation contract. The v1 scope already delivers a
working, reviewable, testable agent-driven-editor mechanism; the excluded items
are recorded as future work with their seams identified.

## Q8. Testing strategy without a GUI?

**Decision:** Three layers: (1) unit tests for the JSON-RPC framing and
dispatch logic (pure functions); (2) GPUI `#[gpui::test]` integration tests
that construct a real `Thread` the same way the `edit_file` unit tests in
`crates/agent` do, drive the dispatcher end-to-end, and assert on files and
returned text (including a permission-deny case); (3) a socket-level test
over the real UDS server with a scripted client. The proxy binary is smoke
tested against the socket server.

**Rationale:** The real test barrier for a UI-coupled feature is the GPUI app
context; the `agent` crate already has the recipe (`Project::test`, thread
setup with a real worktree), which the fork gets to reuse rather than invent.

## Q9. Success criteria?

**Decision:**
1. With `agent.editor_mcp_server.enabled = true` and a tool-permission `allow`
   rule for `edit_file`, an external stdio MCP client can connect (via the
   proxy), list `edit_file`, and apply real edits: the buffer/file changes,
   the edit is undoable in the editor, and a `set_agent_location` update lands
   (cursor/gutter agent indicator moves) — plus format-on-save runs.
2. The same call structure denies cleanly with an actionable message when the
   permission decision is `deny`, and prompts-with-auto-deny when `confirm`.
3. With the setting off, no socket, no thread, no overhead beyond a check on
   workspace open.
4. `cargo test -p agent -p agent_settings -p editor_mcp` green; `cargo build`
   of proxy independent of the editor.
5. No interactive errors, no hangs: every `tools/call` returns or errors in
   bounded time; closing the socket mid-call cancels the tool run.
