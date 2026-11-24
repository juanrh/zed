# PDD summary: in-editor MCP server exposing Zed's agent tools

## Project

`.agents/planning/2026-11-24-editor-agent-mcp/` — an MCP server that runs
inside the Zed editor and exposes the **real** built-in agent tools
(`edit_file` first) to external MCP clients, delegating every call to the
actual tool run on a real `Thread` so buffers, format-on-save, undo, the
action log, and the user's tool-permission rules all behave exactly as for
the built-in agent.

## Artifacts

- `rough-idea.md` — the starting idea.
- `idea-honing.md` — requirements decisions (autonomous mode; each entry
  records decision, rationale, and alternatives).
- `research/edit-file-execution-path.md` — how the tool really executes,
  its dependencies, and the seams (paths + line numbers).
- `research/settings-and-transport.md` — settings plumbing, socket
  transport options, MCP protocol minimum, fork-surface inventory.
- `design/detailed-design.md` — self-contained design (requirements R1–R12,
  architecture with mermaid diagrams, components, data models, error
  handling, testing strategy, appendices). Revised once after review.
- `implementation/plan.md` — checklist, working agreements, and 7 steps,
  each with an "Implementation notes" subsection (deviations and
  implications recorded as executed).
- `reviews/round-1-design.md` — ponytail review of the design (5 findings,
  all incorporated; R10/R11/R12 added to the design).
- `reviews/round-2-code.md` — ponytail review of the implementation
  (3 findings: an in-flight counting drift bug, silent response drops, a
  protocol wart; all incorporated).
- `reviews/round-3-code.md` — final re-review: **verdict "Ship."** (one
  one-line hardening applied anyway; engagement stopped at 3 rounds.)

## Delivered code

- `crates/agent/src/thread.rs` — the minimal fork seam:
  `Thread::registered_tool(s)`, `Thread::run_external_tool`,
  `ExternalToolRun` (+`into_parts`, `cancel`), mirroring the private
  `run_tool` sequence for out-of-turn runs.
- `crates/editor_mcp/` — new crate: `protocol.rs` (pure JSON-RPC subset),
  `dispatch.rs` (hidden `Thread`, allowlist enforcement, permission
  auto-deny pump with real output conversion), `server.rs` (UDS server,
  one-in-flight-per-connection, cancel-on-disconnect, dual-window socket
  tiebreak), `store.rs` (settings-driven per-workspace lifecycle),
  `README.md` (setup, permissions, manual QA script, limits).
- `crates/zed-editor-mcp/` — std-only stdio↔socket relay binary
  (`--socket PATH` / `ZED_EDITOR_MCP_SOCKET`).
- `crates/settings_content`, `crates/agent_settings` — the
  `agent.editor_mcp_server { enabled, tools }` settings block with tests.
- `crates/zed` — two-line boot wiring (`editor_mcp::store::init`).

## How to use it

Enable `agent.editor_mcp_server.enabled = true`, start or adjust
`agent.tool_permissions` (`edit_file: allow`), pick up the socket path from
Zed.log, and register `zed-editor-mcp --socket <path>` as a local MCP server
in any stdio MCP client. Full details and a five-minute manual QA script in
`crates/editor_mcp/README.md`.

## Verification (in-sandbox)

- `cargo test -p agent --lib` — 821 passed (3 new `run_external_tool` tests:
  real edit through the seam, permission deny rules, confirm auto-deny
  round-trip, unknown tool).
- `cargo test -p agent_settings --lib` — 47 passed (3 new settings tests).
- `cargo test -p editor_mcp` — 14 passed (protocol subset, real-buffer edit
  over the dispatcher, allow/ deny/ confirm permission paths, unknown tool,
  invalid arguments, allowlist filtering, socket path determinism).
- `cargo test -p zed-editor-mcp` — 2 passed; plus a manually verified live
  session (initialize + tools/call relayed through the binary against a
  fake editor socket; exit codes for dead-socket/no-args).
- Known sandbox limits, honestly recorded: the `zed` crate itself cannot
  compile here (missing X11 headers, no root): its change is two lines and
  signature-checked against the adjacent `repl::init` call; live-socket
  end-to-end sessions are documented as manual QA
  (`#[gpui::test]`'s deterministic scheduler forbids the transport
  threads' cross-thread wakes).

## Possible next steps

- Interactive confirmation routing for MCP calls into the Agent Panel UI
  (the `ExternalToolRun` API keeps this seam open).
- Named-pipe transport for Windows.
- Expanding the default allowlist (`read_file`, `list_directory`, ...) once
  the permission UX for socket-driven confirms is decided.
