# zed-review-mcp

> [!IMPORTANT]
> Remove this line to confirm you've reviewed this PR before submitting.

`zed-review-mcp` is an MCP server (stdio) that gives external coding agents — Mistral Vibe
CLI, Claude Code, opencode, anything that speaks MCP — Zed's asynchronous change-review
workflow: the agent edits files on disk, and the human reviews the pending changes in
**Zed's own rich, multi-file diff UI** (edit the proposal, run linters and tests,
approve or reject per file) before any decision lands. It is the Rust, Zed-native
counterpart of [Zed discussion #46388](https://github.com/zed-industries/zed/discussions/46388)
and implements the review protocol pioneered by
[Aletheia](https://github.com/demiourgoi/epinoia/blob/main/aletheia/README.md), replacing
its bundled web UI with `zed --diff`.

## How the review works

1. The agent calls **`record_baseline(paths)`** before editing (missing files are marked
   for creation), or just uses **`edit(file_path, old_string, new_string)`** — the first
   edit of each file records its baseline automatically.
2. The agent edits files normally; linters and tests see real files. The repository is
   the working state, the baseline is the pre-change snapshot.
3. The agent calls **`open_review()`** — the server runs `zed --diff <baseline-view>
   <project-root>`, which opens (or focuses) the running Zed with a single multi-file
   diff view of everything that drifted.
4. The human reviews in Zed and decides; the agent polls **`get_change_status()`** and
   resolves the round with **`approve(paths)`** (keep the working tree) or
   **`reject(paths)`** (restore the baseline contents on disk). Omit `paths` to decide
   every file under review. A file is under review exactly while it has a baseline.

## Tools

| Tool | Purpose |
| --- | --- |
| `record_baseline(paths)` | snapshot files before editing; files that do not exist are marked for creation |
| `edit(file_path, old_string, new_string, replace_all?)` | exact-string replacement; unique match unless `replace_all`; first edit records the baseline (Vibe-CLI-compatible schema) |
| `open_review()` | open Zed's multi-file diff of baseline vs. working tree |
| `get_change_status()` | live per-file drift report (unchanged / modified / created / removed) |
| `approve(paths?)/reject(paths?)` | record the user's decision; `reject` restores pre-change content |
| `open_paths(paths)` | open files in the running Zed (basic IDE integration) |

Every path must be absolute and resolve inside the trusted root (the git worktree root of
the server's working directory, falling back to that directory). Only UTF-8 text files
are reviewable. Errors name the offending path.

## Running it

The binary is part of the Zed workspace; from a checkout:

```sh
cargo install --path crates/zed_review_mcp
```

It requires the `zed` executable (`--diff` support ships with Zed). Point `ZED_PATH` at
it when it is not on your `PATH`.

Client configuration example (Mistral Vibe CLI, `.vibe/config.toml`):

```toml
[[mcp_servers]]
name = "zed-review"
transport = "stdio"
command = "zed-review-mcp"

# optional, to force edits through the baseline-aware tool instead of the built-in edit:
disabled_tools = ["edit"]
```

Any MCP host with stdio support works the same way (command `zed-review-mcp`). The
server is loopback-only: it listens on nothing and spawns only the local `zed`
executable.

### Session directories

Baselines live in a per-session directory (default: `$TMPDIR/zed-review-mcp-<pid>`,
overridable with `--session-dir` or `ZED_REVIEW_MCP_SESSION_DIR`; the default is removed
on shutdown, explicit ones are kept). Each conversation gets its own MCP server process,
so review state never crosses conversations.

## Development

```sh
cargo test -p zed_review_mcp      # unit + protocol + binary end-to-end tests
cargo clippy -p zed_review_mcp --all-targets
cargo fmt --check
```

The crate is intentionally headless (no `gpui`): it builds and tests everywhere, and the
Zed integration is exactly the already-shipped CLI surface. Future extensions (see the
design document in `.agents/planning/2026-11-24-zed-review-mcp/`): an in-editor IPC
bridge into Zed's agent-panel review UI, diagnostics queries and active-editor context
tools.
