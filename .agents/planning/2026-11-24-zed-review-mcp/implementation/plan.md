# zed-review-mcp implementation plan

Converts the design into test-first, demoable increments. The design document
(`design/detailed-design.md`) and all PDD context files are available during execution.

## Checklist

- [ ] Step 1: crate scaffold + baseline review store (record/status/approve/reject) with tests
- [ ] Step 2: MCP server on rmcp with the review tools wired to the store
- [ ] Step 3: ZedOpener (`open_review`, `open_paths`) + edit tool + binary end-to-end

## Working agreements

Proceed step by step:

- After each step, ask me for confirmation before completing it
- When completing a step, add an "Implementation notes" subsection to its section in the plan, summarizing what we did, focusing on deviations from the plan, why we made them, and implications for subsequent steps
- Build and test with `cargo test -p zed_review_mcp`; lint with `cargo clippy -p zed_review_mcp --all-targets` and `cargo fmt --check`
- Commit after each step
- (Unattended run: confirmations are implied by the task instructions; each step only completes when its tests pass.)

## Step 1: Crate scaffold + ReviewStore

Create `crates/zed_review_mcp` with `[lib] path = "src/zed_review_mcp.rs"` (repo naming
rule), `anyhow` and workspace `util`-style deps only where needed; add the crate to the
workspace `members`. Implement in `src/review_store.rs`:
`session/trusted-root/baselines` layout, `ReviewStore::start`, `record_baseline`
(Recorded / MarkedForCreation, re-record error, UTF-8-only, confinement via
symlink-proof ancestor canonicalization), `status` (byte-equality FileKind), `approve`,
`reject` (atomic restore, creation-delete, dir pruning). Also `src/trusted_root.rs`
(git worktree root via `Command`, fallback to cwd) and the shared path-safety helper.
Tests cover both files of `review_store.rs` and `trusted_root.rs` (temp dirs, real temp
git repo for root resolution).

**Demo:** a `ReviewStore` created over a temp project records `src/main.py`, reports
`unchanged` → edit → `modified` → `reject` restores content byte-for-byte and the file
leaves review.

## Step 2: MCP server with review tools

Add `rmcp` dependency (features `server`, `transport-io`, `macros`; no default features)
and a `ReviewServer: ServerHandler` in the lib: initialize metadata, tool router with
`record_baseline`, `get_change_status`, `approve`, `reject` wired to the store
(review GUI tools land in Step 3). Tool errors become MCP `isError` text naming paths.
Integration test: drive the server in-memory through rmcp with a duplexed stream client:
initialize → `tools/list` → call each tool on a temp project → verify replies and disk
state; pins the protocol surface.

**Demo:** an MCP `tools/list` returns the four tools with schemas; `tools/call
record_baseline` + `approve` works against a temp project.

## Step 3: ZedOpener + `edit` + `open_paths` + binary end-to-end

Implement `src/zed_opener.rs` (`ZED_PATH` → `PATH` resolution, `open_diff`, `open_paths`,
record-args fake `zed` tests), the baseline-aware `edit` tool (all Vibe-CLI semantics,
first-edit auto-baseline with no side effects on failure), and `open_paths`/`open_review`
tools. Write `main.rs`: flag/env handling (`--session-dir`, `ZED_REVIEW_MCP_SESSION_DIR`),
trusted-root + store start, stdio serve, EOF cleanup. Add crate `README.md` (user docs,
agent guidance incl. Vibe `disabled_tools`) and end-to-end test spawning the binary:
handshake → record → edit → status → reject (against a fake `zed`) over real pipes.

**Demo:** run the released binary: `record_baseline` then `open_review` invokes the real
`zed --diff <snapshot> <root>` (fake in tests); the agent round completes, working tree
restored on reject.
