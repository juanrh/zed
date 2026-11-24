# Ponytail review — Round 2 (implementation)

Skill: ponytail-review applied to the full implementation diff
(`git diff FETCH_HEAD` on branch `vibe/editor-agent-mcp-8dff25`, 25 files).
Reviewer hat on; findings below, then incorporation notes.

---

What this change does: This adds an MCP server inside Zed that lets an
external MCP client run the editor's real built-in agent tools (starting
with `edit_file`) on the real project, over a per-workspace Unix socket,
gated by a new setting (default off) and the user's existing tool-permission
rules, plus a stdio relay binary for standard MCP clients.

Assumed load: one local editor, a handful of MCP client connections, few
tool calls per minute, humans at the keyboard. Not a server farm.

## Must fix

1. **Cancel path lets the in-flight counter drift, breaking the one-call-at-a-time rule**
   (`crates/editor_mcp/src/server.rs`, `handle_frame` cancel arm +
   `complete_call`)
   - **What this is:** Each connection allows one in-flight `tools/call`,
     tracked by `connections[conn].pending_calls`. Cancel removes the pending
     entry and decrements; when the tool run later completes,
     `complete_call` removes the (now absent) entry and decrements *again*.
   - **Problem:** After cancel→completion, the counter is one lower than
     reality. Sequence: call A in flight (count 1) → cancel A (count 0) →
     client issues call B (count 1) → A's run completes (count 0 while B
     still runs) → call C accepted → two tools running simultaneously that
     both promise serialization. The counter keeps bleeding downward with
     every cancel.
   - **Fix:** Decrement in exactly one place: on the transition that removes
     the `pending` entry. In `complete_call`, only remove-if-present and
     decrement when the entry was still there; the cancel arm already does
     that. (Both already `remove()` conditionally — make the decrement
     conditional on the same `was_present` result, in both spots, and delete
     any second decrement.)
   - **If we skip it:** The R10 invariant slowly breaks; concurrent edits from
     one connection become possible after enough cancels.

## Should fix

2. **Response loss is silent when a client floods without reading**
   (`serve_connection`, response channel bounded at 16, `try_send` in
   `handle_request`/`complete_call`)
   - **What this is:** Responses go through a bounded per-connection channel
     with `try_send`; a full channel drops the frame silently.
   - **Problem:** A client that sends >16 requests without reading any
     responses gets silently-lost frames and hangs on a request id (it only
     harms itself, but the failure mode is mysterious). The design chose
     backpressure via the *inbound* channel; responses never apply pressure
     back.
   - **Fix:** No channel surgery needed for the realistic case; make the
     drop loud: change `try_send` results on failure to `log::warn!` naming
     the dropped frame class (request id where known). Optionally cap is
     fine — but a hung client should show up in Zed.log, not nothing.
   - **If we skip it:** Support tickets of the form "client hangs after
     connecting to Zed" with no log evidence.

3. **`invalid_frame_response` returning `Option<String>`** (`crates/editor_mcp/src/protocol.rs`)
   - **What this is:** The builder returns `Option<String>` and both call
     sites immediately `if let Some(...)` it.
   - **Problem:** The Option encodes nothing — it is always `Some`. Reader
     must check anyway; the awkward shape suggests a meaning that isn't
     there. Same pattern family: sending a null-id protocol error for
     *notification*-shaped unknown frames (no response is allowed for
     notifications).
   - **Fix:** Return `String`; simplify the call site. For
     `Message::UnknownMethod { id: None, .. }` drop the frame silently with
     a warn log (notifications get no reply per JSON-RPC), keep the
     invalid-frame error only when a malformed *request* (or unparseable
     frame with unknown shape) arrives.
   - **If we skip it:** Two lines of needless shape, one protocol wart.

## Nice to have

4. The `PendingCall` struct wraps a single field — inlining the oneshot
   into the map value would save a hop, but the named type documents intent;
   leave it.

5. `crates/editor_mcp/src/server.rs` retains `MAX_CHANNEL_DEPTH` used for
   both the signal and response channels; one constant for two purposes is
   fine at depth 16, no change needed.

---

Verdict: fix 1 first, then 2 and 3.
Lean: -8 lines possible (finding 3).

**Incorporation note (editor hat):** All of 1–3 accepted and fixed in the
follow-up commit; see round-3 re-review. Counted against: none declined.
