# Research: Zed codebase — what exists that this feature can build on

All findings are from reading `juanrh/zed` at commit 8af404a6b0 (fork of
`zed-industries/zed`, Nov 2026 state).

## Zed CLI: `--diff` opens rich diffs in the running editor

- `crates/cli/src/main.rs` defines a `--diff <left> <right>` CLI argument; arguments are
  consumed in chunks of two as pairs (line 604ff).
- If **both** sides of a pair are directories, `expand_directory_diff_pairs`
  (`crates/cli/src/main.rs:169`) walks both trees, unions their relative file lists, and
  produces one file pair per relative path; files missing on one side are paired with an
  **empty stub file** in a kept temp dir. So `zed --diff <dirA> <dirB>` shows every
  changed, added and removed file — exactly a review diff.
- The CLI hands a `CliRequest::Open { paths, urls, diff_paths, diff_all, .. }`
  to the running Zed (IPC handshake; on Linux the app exposes a `UnixDatagram`
  at `~/.local/share/zed/zed-<channel>.sock`, see
  `crates/zed/src/zed/open_listener.rs:417` `listen_for_cli_connections`). If Zed is not
  running, the CLI boots it with the same request. Cross-platform, already maintained.
- `diff_all: true` is set when any pair side is a directory: Zed shows a single
  multi-buffer diff view (multi-diff) instead of one pane per pair.
- Restrictions to honor when we generate the invocation: every path passed to `--diff`
  must **exist** (`--diff path does not exist`), so we must empty-stub `marked_for_creation`
  files ourselves if we rely on single-file pairs; directory mode handles that for us.

## MCP in Zed today

- Zed is an MCP **client**: `crates/context_server` implements client-side MCP over stdio
  with its own serde protocol types (`src/protocol.rs`), spawned via
  `src/transport/stdio_transport.rs`. There is no server-side MCP anything; there is no
  `rmcp` in `Cargo.lock`.
- Workspace pins `tokio 1.54.1`, `tokio-util 0.7`, `futures-util 0.3`, `serde`,
  `serde_json`, `schemars 0.9`, `similar`, `anyhow` — everything the official
  [RMCP SDK](https://github.com/modelcontextprotocol/rust-sdk) (3.11.0, rust 1.88+,
  edition 2024, features `server`, `transport-io`, `macros`) needs is already at compatible
  versions in-tree except `rmcp` itself and a few of its transitive deps
  (`rmcp-macros`, `pastey`, `base64`, `uuid`, `sse-stream`, ...).

## Review/revert machinery that already exists

- Zed's **buffer_diff** crate renders base-vs-current diffs in the editor (git diff
  gutter, `Multibuffer` diff views). `git_store` applies reverts. These are exactly the
  primitives behind `zed --diff` rendering; we do not need to touch them to get a rich UI.
- The `agent` panel shows per-edit review for **ACP agents**; that flow is coupled to
  Zed-internal agent threads and is not reachable from outside the app in this fork.
  Reaching it would require new in-app IPC — documented as the future path, not the MVP.

## Build constraints in the development environment (see also PR #1 precedent)

- The sandbox has none of the Linux desktop libraries Zed's GUI crates require
  (xkbcommon, wayland, alsa/vint, pulse, glib, gtk, X11, xcb): **no crate that depends on
  `gpui` can be compiled or tested here.** Affected: `zed`, `workspace`, `editor`,
  `project`, `buffer_diff`, `agent_ui`, `settings_ui`, ...
- Headless crates (`cli`, `streaming_diff`, `paths`, `util`, `fs`, `git`, ...) do
  build: `cargo check -p streaming_diff` completes in ~3.5m after dependency download.
- Consequence: the feature must live in a new headless crate whose only ties to Zed are
  (a) the `zed` executable invocation and (b) workspace conventions. This gives
  testability and makes the PR reviewable by CI (`cargo test -p <crate>`).
- Repo rules (`.rules`): no `unwrap()`, no organization comments, prefer few files, no
  `mod.rs` (use `src/foo.rs` + `[lib] path = "src/foo.rs"`), full-word identifiers,
  `> [!IMPORTANT]` notice prepended to `README.md` when modifying sources,
  errors must not be silently discarded.

## Git root detection

- `crates/git` (`git::binary_path`, `git::worktree_root_for_dir`-style helpers) exists but
  drags gpui? — `git` crate is headless (used by `cli`-adjacent tooling); however for one
  `git rev-parse --show-toplevel` call, shelling out with `std::process::Command` is
  simpler and dependency-free; tests fake git or use a real temp worktree. Decide in
  design: shell out (no new workspace dependency, testable).

## Zed executable resolution

- `ZED_PATH` env var override → fallback to `zed` on `PATH` → clear error naming the
  env var. `open_paths`/`open_review` spawn it detached (`stdin` null, output captured to
  stderr on failure). No network, no listeners: the public-sandbox safety posture is
  preserved (we only spawn a local GUI process, as the user's own CLI would).
