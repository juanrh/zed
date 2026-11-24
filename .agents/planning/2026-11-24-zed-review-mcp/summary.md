# Summary — zed-review-mcp

Built with the Prompt-Driven Development process, fully unattended per the task
instructions. This document summarizes the artifacts and the delivered feature.

## What was built

An MCP server (`crates/zed_review_mcp`, binary `zed-review-mcp`) that gives external
coding agents (Mistral Vibe CLI, Claude Code, opencode, …) Zed's asynchronous
change-review workflow, answering the flagship use case of
[zed-industries/zed discussion #46388](https://github.com/zed-industries/zed/discussions/46388):

1. `record_baseline(paths)` before editing (or `edit(...)`, whose first edit records the
   baseline automatically) — Aletheia-compatible protocol.
2. Agent edits on disk; linters/tests see real files; drift is always live.
3. `open_review()` opens **Zed itself** as the review GUI via the existing
   `zed --diff <review-baseline-view> <review-work-view>` CLI capability — the user gets
   Zed's rich multi-file diff, can edit the proposal, and can run CI before deciding.
4. `get_change_status()` live poll; `approve(paths)` / `reject(paths)` (omit paths =
   everything) record the user's decision; a file is under review exactly while it has
   a baseline. `open_paths(paths)` provides the first basic IDE tie-in.

Safety: trusted-root confinement (git worktree root of the startup directory) with
symlink resolution, lexical traversal rejection, UTF-8-only reviewable text files,
per-path error messages, loopback-only server (listens on nothing, spawns only the
local `zed` executable), atomic restores. Implemented on the official
[RMCP SDK](https://github.com/modelcontextprotocol/rust-sdk) (rmcp 3.11) as suggested.

## Artifacts (`.agents/planning/2026-11-24-zed-review-mcp/`)

- `rough-idea.md` — the request and its constraints
- `idea-honing.md` — scope/protocol/safety decisions with alternatives
- `research/codebase-analysis.md`, `research/prior-art.md` — codebase facts
  (`zed --diff` directory-pair expansion, MCP client, build constraints) and prior art
  (discussion #46388 comments, Aletheia, JetBrains/VS Code MCP servers)
- `design/detailed-design.md` — self-contained design (updated with the review outcomes)
- `implementation/plan.md` — checklist, working agreements, three test-first steps with
  implementation notes
- `implementation/review-round-1.md` / `-2.md` / `-3.md` — ponytail reviews
  (round 1: 3 must-fix bugs — trusted-root traversal, review-view flooding, creation
  marker data loss; round 2: 3 should-fix lean/safety issues; round 3: 1 polish,
  verdict **Ship**)

## Verification

- `cargo test -p zed_review_mcp` — 54 passed (43 unit, 6 in-process MCP-protocol,
  2 spawned-binary end-to-end, 3 CLI)
- `cargo clippy -p zed_review_mcp --all-targets` clean; `cargo fmt --check` clean
- `cargo check -p cli` — root manifest changes don't disturb existing crates
- Not verifiable here: an interactive run with a real desktop Zed (no display / system
  GUI libraries in this environment); argv-level fake-`zed` tests plus Zed's shipped
  `--diff` implementation cover the invocation contract.

## Next steps

1. Try it against a running Zed nightly/preview and tune the review view (e.g. per-file
   pairs instead of directory expansion if tab titles are noisy).
2. Future extensions from the design: an in-editor IPC bridge into Zed's agent-panel
   review UI (approve/reject buttons inside Zed), diagnostics/active-editor context
   tools, branch-diff review.
3. Consider publishing the binary for easy `uvx`-style usage by agent hosts.
