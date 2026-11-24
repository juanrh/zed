# Ponytail review — round 1 (design + steps 1–3)

Scope reviewed: `design/detailed-design.md`, the plan, and the full implementation
(`crates/zed_review_mcp`). Assumed load: one agent conversation per server process, one
local user in one Zed instance — small state, no concurrency across conversations.

## Must fix (all fixed in this round)

1. **Trusted-root escape via `..` in non-existent paths** (`src/trusted_root.rs`,
   `confine`): walking up to the nearest existing ancestor and re-joining the
   non-existing tail can climb back out of the root (`/root/missing/../../escape`
   with `missing` absent rejoins as `/root/missing/../../escape` after the ancestor
   check passes). Fix: reject any path whose components are not `Normal`/`RootDir`
   before resolution. Test: `test_confine_rejects_dotdot_components`.
   If skipped: an agent could `edit` outside the project root — the exact class of
   safety hole the trusted root exists to prevent.
2. **`open_review` would show the whole repository as changed**
   (`ReviewStore::materialize_review_view` + `open_review`): pairing the baseline view
   against the whole trusted root made Zed's CLI directory expansion stub the left side
   of *every* right-only file, i.e. every file in the project. Fix: materialize
   `review-baseline-view` **and** `review-work-view` session trees containing only the
   changed files, and open `--diff` between them; removed files are right-side stubs,
   created file pairs are empty-baseline vs content. If skipped: the flagship feature
   floods Zed's diff view with unrelated files; unusable for real repos.
3. **Creation-marker ambiguity can destroy user data** (`src/review_store.rs`):
   representing "marked for creation" as a zero-byte snapshot is indistinguishable from
   a recorded genuinely-empty file. After a user adds content to an originally empty
   recorded file, `reject` deleted it (instead of restoring the empty original).
   Fix: separate `creations/` marker tree; `baselines/` stays pure content. Test:
   `test_existing_empty_file_is_a_snapshot_not_a_creation_marker`. If skipped: silent
   loss of user-written content on a rejected round — the worst failure mode for a
   review tool.

## Should fix (checked, no change required)

- `status()` fails loudly (not silently) if a created path was turned into a directory —
  accepted: the review cannot be compared, and the error names the path.
- Concurrent tool calls are serialized through the workspace mutex and the filesystem;
  rmcp serves requests on one connection per process, so the expected load (one
  conversation) is safely within design.
- Errors never use `let _ =`: propagation, `context`, or logged-in-`error_result` only.

## Verdict

Round-1 fixes applied and re-verified: `cargo test -p zed_review_mcp` — 54 passed;
`cargo clippy -p zed_review_mcp --all-targets` clean; `cargo fmt --check` clean. Design
document updated for the new session layout (creations tree, review view pair).
Verdict: fix the three findings above first — done; proceed to round 2.
