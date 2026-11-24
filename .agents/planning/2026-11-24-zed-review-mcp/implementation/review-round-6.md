# Ponytail review — round 6 (revision 2: final)

Final pass over the complete rev-2 change: research + design-v2 documents, the bridge
client, the three gateway tools, the updated README, and all test suites.

## Must fix / Should fix

None found.

## Nice to have (checked, not applied)

- The bridge protocol could grow a `tools/list` method so the gateway advertises only
  the tools the connected Zed actually supports (feature-flagged LSP tools vary by
  profile). Deferred: with three static tools the complexity is not worth it; the design
  notes it for the app-side phase when the tool surface grows.

## Checked

- `cargo test -p zed_review_mcp` — 60 passed (47 unit, 8 protocol incl. fake-bridge
  forwarding and bridge-down guidance, 2 binary end-to-end, 3 CLI arg tests).
- `cargo clippy -p zed_review_mcp --all-targets` clean; `cargo fmt --check` clean.
- Design/research claims spot-checked against the fork's code: `AgentTool` signature
  (`crates/agent/src/thread.rs:5511`), tool registry (`crates/agent/src/tools.rs`
  `ALL_TOOL_NAMES`), `NativeAgentServer` being in-process-only, `--diff` CLI expansion
  in `crates/cli/src/main.rs` — the documents cite files and lines that exist.
- The offline fallback and bridge tools have disjoint names (`edit` vs `edit_file`,
  `open_*` vs `grep`/`diagnostics`), so agents see no ambiguity in `tools/list`.
- README covers both modes, the env surface
  (`ZED_AGENT_BRIDGE_SOCKET`, `ZED_AGENT_BRIDGE_TIMEOUT_SECS`, `ZED_PATH`,
  `ZED_REVIEW_MCP_SESSION_DIR`) and phasing for the remaining documented tools.

## Not checked

- Live behavior against a real Zed build (the app-side listener does not exist yet and
  app crates cannot compile in this environment); the wire protocol and schema-parity
  are pinned by the fake-bridge tests, and the app-side piece is specified to
  file/line level in the design.

## Verdict

**Ship.**
Lean: −11 lines possible by dropping the `edit` fallback tool once bridged `edit_file`
is the only supported mode; deferred to the fork's decision (documented in the design
Revision 2), since deleting working, tested fallback code ahead of the app-side phase
would remove today's only complete flow.
