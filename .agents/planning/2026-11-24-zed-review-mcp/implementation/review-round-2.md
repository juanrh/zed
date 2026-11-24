# Ponytail review — round 2 (post round-1 fixes)

Assumed load: one agent conversation per server process; one local user reviewing in one
Zed instance. Reviewed the final code (`review_store.rs`, `review_server.rs`,
`trusted_root.rs`, `zed_opener.rs`, `main.rs`, both test suites) against the design.

## Must fix

None found in this round.

## Should fix (all fixed in this round)

4. **Duplicate paths split an approve/reject and report failure**
   (`src/review_server.rs`, `acceptance_paths`): calling `approve` with `["a", "a"]`
   removed the baseline for the first entry and then failed the whole tool call on the
   second with a confusing "no baseline" error, leaving the decision half-applied and
   the agent misled. Fix: order-preserving deduplication of decision paths.
   Test: `test_acceptance_paths_defaults_to_all_and_dedupes`.
5. **`apply_edit` returned the edited contents nobody used**
   (`src/review_server.rs`): the returned `String` was discarded by its only
   caller — dead data, extra allocation per edit. Fix: return only the
   `baseline_registered` flag. Lean: −4 lines.
6. **`ReviewServer::review_store()` getter had no callers** — deleted. Lean.

## Checked and accepted (no change)

- `materialize_review_views` calls `status()` which takes the workspace lock;
  callers do not hold it — no self-deadlock; the lock only guards per-process
  read-modify-write sequences, and rmcp serves this single-connection server
  sequentially enough for the expected load (one conversation).
- Unknown *tool* names surface as JSON-RPC errors from rmcp's router (behavior
  consistent with the framework; tool *execution* failures are `isError` per design R13).
- `open_paths`/`open_diff` validate paths exist before spawning; spawn errors carry
  the action description and executable path.
- Duplicate-paths in `record_baseline` intentionally error ("already under review"),
  which names the offending path — a visible agent mistake, matching Aletheia rules.
- Default-session cleanup removes exactly the directory the server created;
  explicit session dirs survive shutdown; e2e asserts both.
- `Cargo.lock` updated in-repo; `cargo test/clippy/fmt` for the crate all clean.

## Verdict

Round-2 fixes applied and re-verified: `cargo test -p zed_review_mcp` — 54 passed;
`cargo clippy --all-targets` clean; `cargo fmt --check` clean.
Verdict: proceed to the final round 3.
