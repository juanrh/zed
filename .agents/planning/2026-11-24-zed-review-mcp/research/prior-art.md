# Research: prior art and references

## Zed discussion #46388 — MCP Server Mode for External AI Client Integration

[zd: discussion](https://github.com/zed-industries/zed/discussions/46388) requests MCP
*server* mode in Zed (today Zed is only a client). Requests named there, in priority
order for this work:

- [juanrh's comment](https://github.com/zed-industries/zed/discussions/46388#discussioncomment-19614750):
  use a CLI agent and still get Zed's pending-edit review diff — edit the diff and run CI
  tools **before approving**. This is the concrete flagship use case; the Aletheia
  prototype exists because Zed cannot do it yet.
- simontong's comment: broader wish-list (cross-project open buffers, live debugger
  state, buffer state, LSP-backed lookups, run registered actions). Future work.
- chenjiangui's comment: read-only editor-context bridge (active editor, selection,
  open buffers, dirty buffer contents) as an on-demand query tool. Future work; the
  MVP keeps only `open_paths` from this family.
- sitole's comment: `git diff` / branch-diff review for agent/human PR tandem review.
  Our `--diff` based review covers the working-tree half; branch diffs are a later tool.

## Aletheia (demiourgoi/epinoia) — protocol reference implementation

[Aletheia README](https://github.com/demiourgoi/epinoia/blob/main/aletheia/README.md) is a
Python stdio MCP server giving terminal agents a Zed-style asynchronous change review:

1. `record_baseline(paths)` before editing; snapshots content; files that will be created
   are recorded as such. Starts a web review GUI, returns a constant URL.
2. Agent edits on disk; linters/tests see real files; the diff vs baseline is live.
3. Web GUI: side-by-side diff per file; approve (adopt working dir), edit+approve, or
   reject (restore baseline on disk) — per file or whole change.
4. Decisions shrink the baseline: file is under review iff it has a baseline entry; new
   change rounds re-record baselines (record of a file that already has a baseline is an
   error naming the path).
5. `get_change_status()` polls; nothing blocks the conversation.
6. Optional baseline-aware `edit(file_path, old_string, new_string, replace_all)` tool:
   same schema/semantics as the Vibe CLI built-in edit (absolute path, exact unique match
   unless `replace_all`, `old_string != new_string`), first edit records the baseline,
   reply carries the review URL.
7. Safety rules worth adopting: UTF-8-only reviewable text (binary/directories rejected
   with clear errors), trusted-root confinement with symlink resolution (git root of the
   startup working dir, falling back to it), errors naming the offending path.
8. Guidance for agents: "forbid the built-in edit tool and force edits through the
   baseline-aware tool" — we should mention the equivalent in our docs (Mistral Vibe:
   `disabled_tools = ["edit"]`).

**Adopted**, with the one deliberate divergence: the review GUI is **Zed itself** via
`zed --diff <baseline-snapshot> <project-root>` instead of a bundled web page — that is
the whole point of doing this in the zed repo (the user wants Zed's diff UI: editable,
syntax-highlighted, multi-buffer).

## JetBrains MCP proxy & VS Code MCP servers

- JetBrains ships a bundled MCP server plugin (IntelliJ 2025.2+, on by default) exposing
  `get_open_file`, editor context, `execute_action`, plus community extensions
  (maximehamm/mcp-intellij-all) with a GUI confirmation for edits.
- VS Code family exposes similar tools through extensions; ACP Patchbay (VS Code) adds a
  built-in local MCP server with current file, selection, open editors, diagnostics,
  workspace layout.

These are full in-IDE JSON bridges requiring deep app integration; our fork cannot
compile the app crates in this environment, so the MVP ships the standalone-crate review
workflow (which answers the top use case) and leaves the in-app bridge documented as the
extension path.

## MCP protocol / SDK

- Model Context Protocol: JSON-RPC 2.0 over stdio (spec rev 2025-06-18): `initialize`
  handshake, `notifications/initialized`, `tools/list`, `tools/call`, `ping`, tool result
  content arrays (`text` objects), `isError` flag on tool execution failures.
- [RMCP Rust SDK](https://github.com/modelcontextprotocol/rust-sdk) (official,
  `rmcp` 3.11.0): `#[tool_router]` / `#[tool]` macros, `schemars`-derived input JSON
  schemas, `tokio`-based `serve_server(stdio())`. Chosen per user suggestion; keeps us
  spec-correct without hand-rolled JSON-RPC.
