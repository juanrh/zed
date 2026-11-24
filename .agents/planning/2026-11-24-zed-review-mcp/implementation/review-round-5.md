# Ponytail review — round 5 (revision 2: bridge gateway, part 2 of 3)

## Must fix

None found.

## Should fix

10. **Fixed 120-second reply timeout breaks permission-gated tools**
    (`src/agent_bridge.rs`, `run_tool`): the app-side bridge (per the design) routes
    permission-gated tools through the same `ToolPermissionsAgent` the agent panel
    uses — the reply only arrives after the human confirms inside Zed, which can take
    minutes. A hard-coded 120s timeout would abort valid, simply slow confirmations.
    Fix: `response_timeout` field on the client, default 300s, overridable with
    `ZED_AGENT_BRIDGE_TIMEOUT_SECS`; the timeout error names both the waiting semantics
    and the env var so agents/users can act.

## Checked and accepted

- One connection per tool call: no shared-connection state to corrupt, sequential
  request/response per socket → trivially correct at conversation scale; the
  app-side listener accepting one request at a time matches.
- Error paths: unreachable socket → actionable guidance naming `ZED_AGENT_BRIDGE_SOCKET`
  (tested); bridge JSON-RPC error → `isError` text naming the tool (tested); empty/short
  read → explicit error; malformed JSON → context-wrapped parsing error.
- Schema parity: `edit_file` (path + edits[{old_text,new_text}]) and `diagnostics`
  (optional path) mirror upstream field names; forwarded `input` flattening matches
  `EditFileToolInput`'s shape (tested).
- Offline fallback tools unchanged and still fully covered by the rev-1 test battery.

## Verdict

Fix applied (10) and re-verified: `cargo test -p zed_review_mcp` — 60 passed;
`clippy --all-targets` clean; `fmt --check` clean. Verdict: proceed to round 6.
