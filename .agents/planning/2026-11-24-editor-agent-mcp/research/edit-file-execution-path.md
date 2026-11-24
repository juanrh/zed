# Research: the real edit_file execution path in this fork

All findings verified against the checked-out fork of `zed-industries/zed`.
File/line references are to the working tree (upstream paths identical).

## The tool

- `EditFileTool` — `crates/agent/src/tools/edit_file_tool.rs:75`.
  - `EditFileTool::new(project: Entity<Project>, thread: WeakEntity<Thread>, action_log: Entity<ActionLog>, language_registry: Arc<LanguageRegistry>)` (line 80).
  - Input contract lives in `EditFileToolInput { path, edits: Vec<Edit> }` (line 37);
    `Edit { old_text, new_text }` is in `crates/agent/src/tools/edit_session.rs:46`.
  - Output: `EditSessionOutput` (`edit_session.rs:68`) — `Success { input_path, new_text, old_text, diff }`
    / `Error { error, .. }`; `Display` renders `Edited <path> successfully` / the error text.
  - `EditFileTool::run` (edit_file_tool.rs:258) drives an `EditSessionContext` which:
    opens the buffer in the project (`project.open_buffer`), applies the replace
    edits, calls `set_agent_location` so the gutter/cursor agent indicator moves
    (`edit_session.rs:166-178`), enforces format-on-save and saves
    (`ensure_buffer_saved`, edit_session.rs:184-205), and records the edit in the
    `ActionLog` (`log.buffer_edited`), which is what the editor's edit-review and
    undo surfaces read.

## What the tool needs to run

1. A real `Thread` entity — *not* an LLC turn. `Thread::new` is public:
   `crates/agent/src/thread.rs:1415`
   `Thread::new(project, project_context: Entity<ProjectContext>, context_server_registry: Entity<ContextServerRegistry>, templates: Arc<Templates>, model: Option<LanguageModel>, cx)`.
   `NativeAgent::new_session` (agent.rs:878) is the production caller, and the
   in-crate test at `crates/agent/src/tools/edit_file_tool.rs:3201` shows a
   minimal recipe: `ProjectContext::default()`,
   `ContextServerRegistry::new(project.read(cx).context_server_store(), cx)`,
   `Templates::new()`, `None` model.
2. A `ToolCallEventStream` (`thread.rs:5937`), created private:
   `ToolCallEventStream::new(tool_use_id, tool_call_id, stream: ThreadEventStream, fs: Option<Arc<dyn Fs>>, cancellation_rx: watch::Receiver<bool>, sandbox_grants, thread: Option<WeakEntity<Thread>>)` (thread.rs:6017).
   - The event stream is also the permission surface: `authorize` / `authorize_always_prompt`
     / `authorize_third_party_tool` live on `ToolCallEventStream` (thread.rs:6124-6230).
   - `ToolCallEventStream::test()` constructors exist but are test-gated; a
     production entry point for "a tool run outside a model turn" does not exist
     yet — the fork must add one small constructor.
3. An event pump. The stream writes `ThreadEvent`s (thread.rs:914) into an
   unbounded mpsc; whoever owns the receiver decides prompts. In a normal agent
   turn, `Thread::prompt`-style methods return the receiver to the caller
   (`agent.rs` session code / `NativeAgentConnection`), which filters them into
   the ACP/UI pipeline. In practice the interesting variants for us:
   - `ToolCallAuthorization(ToolCallAuthorization)` — an interactive permission prompt.
   - `ToolCallAuthorizationResolved { tool_call_id, outcome }` — the answer, sent
     back via `ToolCallEventStream::resolve_authorization` (thread.rs:6082).
   - Everything else (diffs, status updates) can be ignored by a programmatic
     caller for v1.
4. Permission decisions **before** any prompt: `crates/agent/src/tools/tool_permissions.rs:627`
   `authorize_file_edit(tool_name, path, thread, event_stream, cx)` →
   `decide_permission_for_path(tool_name, path, settings)`:
   - `allow` rules → run.
   - `deny` → the tool returns its own deny message.
   - `confirm` → the event-stream authorize flow runs → emits
     `ToolCallAuthorization` → **someone must answer** or the call hangs.

## How one dispatches a tool without a model turn

- The type-erased interface is `AnyAgentTool` (thread.rs:5614):
  `run(self: Arc<Self>, input: ToolInput<serde_json::Value>, event_stream, cx) -> Task<Result<AgentToolOutput, AgentToolOutput>>`.
  With `Err` meaning *tool error output*, not protocol error — exactly MCP's
  `isError` semantic.
- `Thread::run_tool` (thread.rs:3899) is the reference dispatcher but is private
  and thread/prompt-coupled (`running_turn`, message indices). It shows the
  essential sequence: check `allow_in_restricted_mode`, build a
  `ToolCallEventStream`, `tool_event_stream.update_fields(status: InProgress)`,
  then `tool.run(...)`, then map `Ok/Err(output)` to a `LanguageModelToolResult`.
- Lookup: a `Thread` holds `tools: BTreeMap<SharedString, Arc<dyn AnyAgentTool>>`
  registered by `add_default_tools` (thread.rs:2233, called during
  `Thread::new` → `new_internal`). The current accessor `fn tool` (thread.rs:4510)
  reads `running_turn`, so the fork needs a small public accessor on the
  registered map.
- Input: `Thread` stores the raw JSON; `ToolInput::parsed`-style paths exist —
  the erased run accepts a `serde_json::Value` directly and validates it.
- Cancellation: a `watch::channel(bool)`; `ToolCallEventStream::cancelled_by_user`
  is what tools select on. MCP `notifications/cancelled` (with request id) is a
  natural mapping.

## Concurrency/threading model

- GPUI entities (Thread, Project...) may only be touched on the main thread;
  background work must marshal via `cx.update` / `cx.spawn` (AsyncApp).
- A socket accept loop must therefore live in background tasks (`smol`/`Async` IO
  or plain std threads) and forward each request to the entity via an
  application-scoped channel, running tool tasks via `cx.spawn`.
- Zed's monorepo avoids `tokio` in the editor; `smol` + `futures` (+ `Async`
  wrappers for std socket types) is the established pattern.
