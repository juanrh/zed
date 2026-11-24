# Rough idea

Expose Zed's IDE capabilities to external coding agents through an MCP (Model Context
Protocol) server, so any CLI agent (Mistral Vibe CLI, Claude Code, opencode, ...) can make
use of them. See
[zed-industries/zed discussion #46388](https://github.com/zed-industries/zed/discussions/46388).

Primary use case (see
[juanrh's comment in that discussion](https://github.com/zed-industries/zed/discussions/46388#discussioncomment-19614750)
and the [Aletheia README](https://github.com/demiourgoi/epinoia/blob/main/aletheia/README.md)):
an asynchronous, Zed-native **change review workflow**:

1. The agent records a baseline of the files it is about to change.
2. The agent edits files on disk normally, so linters, formatters and tests can validate
   the changes at any time; the review diff is live.
3. The user reviews the changes in Zed's rich, multi-file diff UI (Zed's native review
   experience), can edit the proposed content, and approves or rejects per file or for
   the whole change set.
4. The agent polls the decision status without blocking the conversation.

Zed today only acts as an MCP *client*; external agents have no supported way to surface
their pending changes as rich diffs inside Zed, and no way to query IDE state.

Target repo: `juanrh/zed` (fork of `zed-industries/zed`). Constraints discovered during
research:

- Zed is a huge Rust workspace; the sandbox cannot build GUI crates (missing xkbcommon,
  wayland, alsa, glib, gtk, X11 system libraries), so the feature should be implemented
  as a headless, independently buildable and testable component.
- The `zed` CLI already supports `zed --diff <left> <right>`, including directory pairs,
  which opens a rich multi-file diff view in the running editor (or boots a new one).

The rough idea is to build an MCP server (stdio) that implements the Aletheia-style
baseline/approve/reject review protocol, but renders the review GUI through Zed itself via
`zed --diff <baseline-snapshot> <project>` instead of shipping a custom web UI.
