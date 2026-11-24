# Idea honing

Task constraint: this feature is developed fully unattended ("use your best judgement when
you don't know something"). Therefore questions are recorded here together with the
decision taken, its rationale, and the alternatives considered. Each question was answered
with the explicit user preferences available in the task description and the discussion
threads referenced there.

## Q1: What is the MVP scope?

**Decision:** A new Rust crate shipped as a standalone MCP server (stdio) binary that
implements the change-review protocol of Aletheia with Zed as the review UI.

In scope for the first increment:

1. `record_baseline(paths)` — snapshot files before editing (files that do not exist yet
   are recorded as *marked for creation*).
2. `open_review()` — open Zed's rich multi-file diff view of baseline vs. working tree
   through `zed --diff <baseline-snapshot> <project-root>` (works against a running Zed).
3. `get_change_status()` — live poll: per-file kind (modified/added/removed/unchanged) and
   drift versus the baseline.
4. `approve(paths)` — adopt the working tree, remove baselines (per file or all).
5. `reject(paths)` — restore pre-change content on disk, remove baselines.
6. `edit(file_path, old_string, new_string)` — opt-in baseline-aware replacement tool, the
   drop-in replacement for the built-in CLI agent edit tool (first edit records the
   baseline automatically).
7. `open_paths(paths)` — basic IDE integration: open files in the running Zed.

Out of scope for this increment (documented as future extensions): diagnostics queries,
active-editor context, buffer state, `zed`-internal IPC beyond the existing CLI surface.

**Alternatives considered:** exposing the full discussion wish-list immediately
(buffer state, diagnostics, debugger state); rejected — too large for one increment and
most of it would be unverifiable in a headless environment. A generic "run any Zed
action" tool was rejected for the same reason and for safety.

**Rationale:** the rich-diff review use case is the one explicitly named by the requester
(see [juanrh's comment](https://github.com/zed-industries/zed/discussions/46388#discussioncomment-19614750)),
it is end-to-end useful on its own, and every part can be implemented headless and tested
with `cargo test -p <crate>` in CI.

## Q2: How does the MCP server show the review UI in Zed?

**Decision:** reuse the `zed` CLI. The server keeps on-disk baseline snapshots mirroring
the trusted root's file layout, and `open_review()` invokes
`zed --diff <baseline-snapshot-dir> <trusted-root>`; Zed's CLI expands directory pairs into
per-file diffs (creating empty stubs for added/removed files) and shows them in a single
multi-diff view in the running editor.

**Alternatives considered:**

- *In-editor bridge:* implement a new JSON-RPC listener inside the `zed` app crate over a
  Unix socket, translating tool calls into workspace/editor actions. Much deeper
  integration (could drive Zed's agent-panel review UI directly), but all touched crates
  (`zed`, `workspace`, `buffer_diff`) cannot be compiled or tested in this environment,
  risking breaking the fork's build with unverifiable code. Kept as the documented future
  extension path.
- *Custom web GUI (the Aletheia approach):* rebuilds a review UI the user explicitly
  wants to replace with Zed's own; duplicate product, larger surface, no Zed synergy.

**Rationale:** simplest correct mechanism that produces Zed's actual rich diff UI, zero
new code in GUI crates, cross-platform (macOS IPC and Linux datagram socket both handled by
the existing CLI), and testable with a fake `zed` binary on `PATH`.

## Q3: What is the review protocol semantics (baseline lifecycle)?

**Decision:** identical to Aletheia's proven model: a file is under review exactly while
it has a baseline entry; approve (adopt) and reject both remove the entry; a new change
round re-records baselines. Status never blocks the agent; all drift is computed live from
disk. Baselines are never re-anchored automatically by `record_baseline` (recording a file
that already has a baseline is an error naming the path, so a fresh round after a decision
is explicit).

**Rationale:** decisions shrink the baseline, which is what makes per-turn review rounds
composable; keeping semantics identical to Aletheia makes the tool a familiar drop-in and
gives us battle-tested rules to port (UTF-8-only reviewable text files, error messages
naming offending paths).

## Q4: How is safety handled?

**Decision:** `file_path` inputs must resolve (after symlink resolution) inside the trusted
root — the git repository root of the working directory at server start, falling back to
the working directory if it is not a git worktree. Only UTF-8 text files are reviewable;
binary files and directories produce clear tool errors. The `edit` tool requires a unique
`old_string` match unless `replace_all` is set, rejects `old_string == new_string`, and
must not register a baseline when the edit itself fails.

**Rationale:** mirrors the Vibe CLI filesystem-safety criteria already adopted by Aletheia.

## Q5: MCP protocol details?

**Decision:** MCP stdio with JSON-RPC 2.0, protocol version `2025-06-18`, server
capabilities: tools (listChanged=false). The server answers `initialize`,
`notifications/initialized`, `ping`, `tools/list`, `tools/call`, and replies with a
proper JSON-RPC error for unknown methods. Tool results use MCP content objects
(`text`). No new external dependencies for the protocol layer: protocol handling is
dependency-light (serde/serde_json already in workspace). Server support of `tools/list`
includes input schemas (JSON Schema) so agents render parameters correctly.

**Alternatives considered:** hand-writing the JSON-RPC framing — feasible in ~350 lines,
but after the user explicitly suggested the official
[RMCP Rust SDK](https://github.com/modelcontextprotocol/rust-sdk) and tokio was already a
pinned workspace dependency, we adopt `rmcp` 3.11 (features: `server`, `transport-io`,
`macros`): the official SDK gives protocol correctness (initialize handshake, ping,
notificationsProgress, logging), auto-derived JSON schemas for tool arguments, and less
code we have to own. Hand-written JSON-RPC remains documented fallback only if the
dependency turns out un-buildable in the Zed workspace.

## Q6: Where does review state live?

**Decision:** in a per-server-session snapshot directory on disk (needed anyway for the
`--diff` flow): default under the OS temp dir, named `zed-review-mcp-<pid>`; overridable
via `--session-dir` / env `ZED_REVIEW_MCP_SESSION_DIR` for tests and embedding. Baseline
entries are a `baselines/` mirror of the trusted root plus a session manifest. The
directory is cleaned up on normal shutdown (EOF on stdin); after a crash it is inert
(temp-dir) garbage.

**Rationale:** baselines must be real files for `zed --diff`; keeping the manifest on disk
rather than in memory makes the server stateless across restarts for free and lets other
processes (or a future Zed extension) inspect review state.

## Q7: Relation to the existing Aletheia project?

**Decision:** keep the tool names, argument shapes and reply payloads compatible with
Aletheia where it costs nothing (`record_baseline`, `get_change_status`, baseline removal,
the `edit` tool schema), with two renames for clarity: `remove_from_baseline` →
`approve_paths`, `rollback_to_baseline` → `reject_paths` (the Aletheia names describe the
mechanism, the review decision names describe the user intent; both reply payloads are
compatible).

**Rationale:** agents and users already understand the Aletheia flow; familiarity lowers
adoption cost while the GUI difference (Zed instead of a web page) is the point of this
work.
