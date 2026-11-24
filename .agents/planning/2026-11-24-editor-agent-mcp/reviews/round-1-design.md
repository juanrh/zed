# Ponytail review — Round 1 (design)

Skill: ponytail-review (demiourgoi/epinoia skills/ponytail-review), applied to
`.agents/planning/2026-11-24-editor-agent-mcp/design/detailed-design.md`.
Reviewer hat on; findings use the skill's plain-English format. Autonomous mode,
so both review and the resulting fixes are reported here.

---

What this change does: This design adds an MCP server *inside* the Zed editor
process so an external MCP client can drive the built-in `edit_file` tool for
real, against a hidden per-workspace Thread, gated by a new setting (default
off) and the user's existing tool-permission rules. A std-only proxy bridges
stdio clients to a per-workspace Unix socket.

## Must fix

1. **Stale `pending` map only grows** (`EditorMcpServer` design)
   - **What this is:** `pending: HashMap<RequestId, PendingRun>` tracks
     in-flight runs keyed by MCP request id, for cancellation.
   - **Problem:** nothing in the design says an entry is removed when a run
     finishes. A long editor session with many tool calls leaks one entry
     (tool name + completed task handle) per call. With a chatty agent client
     that's thousands of dead entries.
   - **Fix:** delete the entry when the pump task completes (in the same
     place the response frame is sent), plus on cancel. One line of design
     wording; the plan already implies it but must state it.
   - **If we skip it:** memory grows with traffic; a long session slows down.

2. **A dead socket path never recovers inside one run of Zed** (transport
   design: per-workspace `<hash>.sock`)
   - **What this is:** Zed binds the UDS at startup when enabled. If the file
     already exists (previous Zed crashed), the research says "connect-probe
     to avoid stealing a live socket" then `remove_file`.
   - **Problem:** the design puts the stale-reclaim logic at *startup* only,
     worded as best-effort. If removal fails (permissions, socket dir shared
     with another running workspace of the same roots — two Zed windows on the
     same project), the second window's server silently fails to start and
     the user gets no feedback at all.
   - **Fix:** make failure loud and state a tiebreak rule: on bind failure,
   probe-connect; if a live server answers, log "another Zed window already
     serves MCP for this workspace" and disable serving in *this* window
     (never steal), else remove the dead file and retry once. Surface both
     outcomes in Zed.log at warn level.
   - **If we skip it:** enabling the setting can silently do nothing for a
     whole session, and the user files a bug.

## Should fix

3. **Hidden thread identity leaks into other surfaces** (design decision, Q3)
   - **What this is:** an ordinary `Entity<Thread>` is created per workspace and
     never handed to the Agent Panel.
   - **Problem:** Threads are observable entities; anything observing "new
     thread" (thread counts for auto-open sidebars, thread watchers, `Agent
     Panel` presence heuristics) may act on a thread the user never made. The
     design says it is not registered with `ThreadStore`, but "not registered"
     does not mean "not constructed": `Thread::new` may itself emit or register
     things.
   - **Fix:** verify during implementation that the constructed Thread emits
     nothing observable (no notifications, no store inserts); if `Thread::new`
     is quiet, keep it. If not, the implementation plan's first tool step must
     state the mitigation (e.g., a builder flag or construction-time
     suppression) — a factual check, not new machinery. Record the answer in
     the implementation notes.
   - **If we skip it:** the MCP server may leave ghost threads in the UI.

4. **No backpressure between MCP clients and the editor main thread**
   (`incoming: smol::channel::Receiver<McpRequest>`)
   - **What this is:** background accept loop queues raw frames onto a channel
     drained on the main thread.
   - **Problem:** the depth isn't stated. A fast client (or a hostile local
     process) can queue frames faster than the editor dispatches them, since
     each dispatch waits on a real tool run.
   - **Fix:** bounded channel (e.g. 16) and a documented cap of one
     concurrently-dispatched tool per connection (already chosen per R5's
     serial-per-connection rule; make it explicit as the backpressure
     mechanism). Caller blocks → their own problem, transport-level.
   - **If we skip it:** memory growth again, plus dispatch lag for the human's
     own UI work.

5. **`tools/list` before startup race isn't pinned down** (R4/R5 boundary)
   - **What this is:** whether the hidden Thread exists at `tools/list` time.
   - **Problem:** the plan implies tools/list reads the allowlist ∩ registered
     tools, but registered tools only exist after the (lazy) Thread build. The
     design doesn't say which happens on `tools/list` when the Thread isn't
     built yet.
   - **Fix:** state it: `tools/list` uses `AnyAgentTool::input_schema` only
     from the actually-registered map — so the Thread is created eagerly at
     server start (not lazily). Given the server already starts only when the
     user opted in, laziness buys nothing and causes this race. This removes
     the "lazy thread" wording everywhere (Q3 said lazily — overturn it).
   - **If we skip it:** tools/list could advertise tools that later fail at
     call time, or miss tools that exist.

## Nice to have

6. `sha256(worktree_roots)` for the socket file name is heavier than needed;
   a short hex of a stable hash is fine, but keep sha256 — it's already in the
   tree (sha2 crate used elsewhere). No change; just don't invent a new hash
   dependency.

7. The design has both `R8` test layer 2 (GPUI integration tests) and layer 3
   (socket end-to-end). That's good, not duplicated work — socket wiring and
   dispatch are different failure planes. Keep both, don't merge.

---

Verdict: fix 1 and 2 first, then 3, 4, 5.
Lean: ~0 lines (design phase).

**Incorporation note (editor hat):** all five substantive findings are
accepted and folded into the design (see Design Revision 1 in
detailed-design.md): pending map cleanup is specified (F1); dual-window
tiebreak and loud failure are specified (F2); Thread quietness verification
becomes an explicit implementation-checklist item (F3, revised plan wording);
bounded incoming channel + one-run-per-connection caps are specified (F4);
Thread creation switched from lazy to eager (F5). Findings 6 and 7 require no
change.
