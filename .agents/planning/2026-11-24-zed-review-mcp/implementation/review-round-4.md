# Ponytail review — round 4 (revision 2: bridge gateway, part 1 of 3)

Scope: the rev-2 delta — research/design v2 (`research/agent-tool-bridge.md`,
`design/detailed-design.md` Revision 2), `crates/zed_review_mcp/src/agent_bridge.rs`,
the three gateway tools, and the new protocol tests. Assumed load: one agent
conversation, a local Zed with the (future) app-side listener; tool calls sequential,
database-free.

## Must fix

8. **`GrepBridgeInput` must match the upstream `GrepToolInput` contract**
   (`src/agent_bridge.rs`): upstream rejects unknown fields so a mistyped scoping
   parameter (e.g. `path` instead of `include_pattern`) fails loudly instead of being
   silently dropped — which would widen the search to the whole project. The rev-2
   mirror lacked `deny_unknown_fields`, silently dropping such typos and forwarding a
   broader search than the caller asked for. Fix: mirror the upstream serde attribute.
   If skipped: an agent's scoping typo silently searches the entire project and
   wastes the tool call.

## Should fix

9. **`AgentBridgeClient::is_available` had no callers** (`src/agent_bridge.rs`): a
   capability probe nothing used — dead code. Fix: deleted. Lean.

## Verdict

Fixes applied (8, 9); re-verified: full suite green.
Verdict: fix finding 8 first — done; continue to round 5.
