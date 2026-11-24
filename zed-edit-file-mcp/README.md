# zed-edit-file-mcp

A standalone [Model Context Protocol](https://modelcontextprotocol.io) (MCP) server that
exposes [Zed](https://zed.dev)'s agent `edit_file` tool as an MCP tool, usable from any
MCP client — including Zed's own Agent Panel, added natively via
**Settings → AI → MCP Servers → Add Local Server**.

This avoids the deprecated "MCP server extension" mechanism
([zed.dev/docs/extensions/mcp-extensions](https://zed.dev/docs/extensions/mcp-extensions)):
Zed plans to deprecate it in favor of the official MCP registry and native local servers.

## Tool: `edit_file`

The tool mirrors the input/output contract of the built-in Zed agent `edit_file` tool
(`crates/agent/src/tools/edit_file_tool.rs`, `crates/agent/src/tools/edit_session.rs`):

```json
{
  "path": "/abs/path/to/file.rs",
  "edits": [{ "old_text": "run();", "new_text": "run2();" }]
}
```

Semantics:

- Edits are applied sequentially; later edits match the result of earlier ones.
- `old_text` must identify exactly one location in the current file content —
  ambiguous edits fail without touching the file, and the error lists the matching lines.
- If no exact match exists, matching falls back to a whitespace-insensitive line match
  in the spirit of Zed's fuzzy edit matching.
- On success, the tool replies `Edited <path> successfully` followed by a numbered
  excerpt of the changed region (6-wide right-aligned line numbers and a tab, the same
  format `read_file` produces).
- On failure, the reply includes the requested excerpt and surrounding context so the
  calling agent can retry precisely.

## Building

```sh
cd zed-edit-file-mcp
cargo build --release
# binary: target/release/zed-edit-file-mcp
```

The crate has a detached `[workspace]` table so it does not join the main Zed workspace.

## Registering in Zed

1. Build the binary (see above) and note its absolute path.
2. In Zed: **Settings → AI → MCP Servers → Add Local Server**, or add to `settings.json`:

```json
{
  "context_servers": {
    "zed-edit-file": {
      "command": {
        "path": "/abs/path/to/zed-edit-file-mcp",
        "args": [],
        "env": {}
      }
    }
  }
}
```

3. Ask the agent to edit files through `edit_file`. Edits apply directly to files on
   disk; if a file is open in Zed, Zed reloads it (prompting to keep or discard local
   changes if the buffer was dirty).

Paths are best passed absolute. Relative paths are resolved against the server's
working directory, which is the directory Zed spawned the server in.

## Other MCP clients

The server speaks MCP over stdio with a `tools` capability, so it works with any MCP
client (Claude Desktop, Codex CLI, ...) as a `stdio` command.

## Relation to upstream Zed

Zed's built-in `edit_file` runs against open editor buffers, with permission prompts,
diff review, and undo support. That machinery lives inside the editor process and can't
be invoked from an out-of-process MCP server. This crate mirrors the tool's contract and
matching semantics on files on disk instead, which compose well with Zed's file
watching.

## Tests

```sh
cargo test
```
