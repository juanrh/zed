# Ponytail review — Round 3 (re-review of fixes + final pass)

Skill: ponytail-review applied to the fix-up commit (round-2 findings) and
a final sweep of the complete diff. This is the third and final round per
the engagement agreement.

---

What this change does: This is the fix-up commit for the three findings from
round 2, plus one build-graph repair it exposed (feature unification of
`workspace/test-support` in editor_mcp's dev-deps, following the repo's
established pattern from `agent_ui`).

## Findings check from round 2

- **Finding 1 (in-flight counter drift):** Fix verified in source:
  `complete_call` now decrements only when its `pending.remove(...)` actually
  removed the entry (the cancel arm does the same on its side), so each call
  decrements exactly once, on exactly one path. The comment names the
  invariant. Good.
- **Finding 2 (silent response drops):** All response sends now route through
  `try_send_response`, which logs a warning naming the request id when the
  client is not reading. The write path was exhaustively enumerated
  (initialize, ping, tools/list, tools/call, in-flight-refusal, method-not-
  found, invalid-frame, completion) — no bare `try_send` remains outside the
  helper itself.
- **Finding 3 (Option-shaped protocol wart + notifications getting replies):**
  `invalid_frame_response` returns `String`; unknown-method *notifications*
  are dropped with a log line and no reply (correct JSON-RPC), and malformed
  request frames still receive a -32700 error.
- Also incidentally discovered and fixed during this round's attempted test
  run: editor_mcp's test profile broke `crates/workspace` (non-exhaustive
  matches over the `remote` test-support `Mock` variants). The fix follows the
  repository's own convention (`agent_ui` dev-deps): dev-depend on
  `workspace` with `test-support`. No production code paths changed.

## New findings in the fix-up commit itself

1. **`unknown-notification` log uses an arbitrary method name unfiltered**
   (`crates/editor_mcp/src/server.rs`, the `UnknownMethod { id: None }` arm)
   - **What this is:** The method string from an untrusted frame goes
     into a log line.
   - **Problem:** Log injection with control characters from a hostile
     local process is possible in principle.
   - **Fix:** Truncate/escape at the log boundary: `method.chars().take(64)`
     suffices; `log` already redacts newlines in some backends but don't
     rely on it.
   - **If we skip it:** A noisy local log; low severity (same-user threat
     model) but a one-line guard completes it.

2. **`Invalid(reason)` path double-logs**
   - **What this is:** `handle_frame` logs the parse reason, and the protocol
     module also embeds a generic reason in the response; fine, but the
     client-facing -32700 message says "one frame of JSON per line..." even
     for structurally-valid-but-non-JSON-RPC frames.
   - **Problem:** Cosmetic only; the message is generic for every invalid
     shape, which is acceptable — clients debug from their own dump.
   - **Fix:** None required. Not a defect; noting for completeness.

No further must-fix findings. Finding 1 above is a should-fix; given the
threat model (local same-user sockets), it is minor.

Verdict: Ship. (Finding 1 may be taken inline by the maintainers as a
one-line hardening.)
Lean: -0 lines outstanding.

---
**Incorporation note (editor hat):** Finding 1 (log truncation guard) was
applied anyway — it is one line and shipped in the final commit; finding 2
needs no change. Rounds stop here per the agreement (3 of 3).
