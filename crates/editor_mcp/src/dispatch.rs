//! The GPUI-side dispatch core: owns the hidden agent `Thread` for a project
//! and translates MCP tool calls into real tool runs on that thread.

use crate::protocol::{ToolCallOutcome, ToolDescriptor};
use agent_settings::AgentSettings;
use settings::Settings;
use anyhow::{Result, anyhow};
use futures::{FutureExt, StreamExt};
use gpui::{App, AppContext, Entity, Task};
use project::Project;
use prompt_store::ProjectContext;
use serde_json::Value;
use std::rc::Rc;

/// The message an auto-denied call returns to the MCP client.
pub const AUTO_DENIED_MESSAGE: &str = "denied: this tool call requires interactive \
     confirmation, which is not available for MCP-driven calls. Add an allow rule for \
     this tool (and, for path-scoped tools, its path pattern) in Zed's Tool \
     Permissions settings, then retry.";

/// An in-flight (or immediately failed) MCP tool call. Dropping the `cancel`
/// sender leaves the run alone; sending on it requests cancellation, and the
/// tool's own cancellation machinery finishes the run promptly.
pub struct CallToolCall {
    pub cancel: futures::channel::oneshot::Sender<()>,
}

/// Dispatches MCP tool calls to the real agent tools registered on a hidden
/// `Thread`, with the user's `agent.tool_permissions` rules applying verbatim
/// because the tools' own permission machinery runs untouched.
pub struct EditorMcpDispatcher {
    project: Entity<Project>,
    thread: Entity<agent::Thread>,
}

/// The `ThreadEnvironment` for the hidden MCP thread. It supports none of the
/// environment-gated tools: `terminal` and subagents stay outside the default
/// MCP tool allowlist (`edit_file`), and an honest error is better than a
/// surprise environment.
struct McpThreadEnvironment;

impl agent::ThreadEnvironment for McpThreadEnvironment {
    fn create_terminal(
        &self,
        _command: String,
        _extra_env: Vec<(String, String)>,
        _cwd: Option<std::path::PathBuf>,
        _output_byte_limit: Option<u64>,
        _sandbox_wrap: Option<acp_thread::SandboxWrap>,
        _cx: &mut gpui::AsyncApp,
    ) -> Task<Result<Rc<dyn agent::TerminalHandle>>> {
        Task::ready(Err(anyhow!(
            "the terminal tool is not served over the editor MCP server"
        )))
    }

    fn create_subagent(
        &self,
        _label: String,
        _model: Option<acp_thread::AgentModelId>,
        _cx: &mut App,
    ) -> Result<Rc<dyn agent::SubagentHandle>> {
        Err(anyhow!(
            "the spawn_agent tool is not served over the editor MCP server"
        ))
    }
}

impl EditorMcpDispatcher {
    /// Creates the hidden thread for `project`, eagerly. The thread is not
    /// registered with any thread store and produces no UI.
    pub fn new(project: Entity<Project>, cx: &mut App) -> Self {
        let context_server_registry = cx.new(|cx| {
            agent::ContextServerRegistry::new(project.read(cx).context_server_store(), cx)
        });
        let thread = cx.new(|cx| {
            let mut thread = agent::Thread::new(
                project.clone(),
                cx.new(|_cx| ProjectContext::default()),
                context_server_registry,
                agent::Templates::new(),
                None,
                cx,
            );
            thread.add_default_tools(Rc::new(McpThreadEnvironment), cx);
            thread
        });
        Self { project, thread }
    }

    pub fn project(&self) -> &Entity<Project> {
        &self.project
    }

    /// The tools advertised over MCP: the settings allowlist intersected with
    /// the hidden thread's real registered tools, carrying the tools' real
    /// descriptions and input schemas.
    pub fn tools(&self, cx: &App) -> Vec<ToolDescriptor> {
        let allowed = &AgentSettings::get_global(cx).editor_mcp_server.tools;
        self.thread
            .read(cx)
            .registered_tools()
            .into_iter()
            .filter(|tool| allowed.iter().any(|name| *name == tool.name()))
            .map(|tool| ToolDescriptor {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                input_schema: tool.input_schema(),
            })
            .collect()
    }

    /// Runs a tool call to completion, honoring the settings allowlist and
    /// the tool's own permission rules, and returns the tool's honest
    /// outcome. Never `None`: every failure arrives as an error outcome.
    pub fn call_tool(
        &self,
        name: &str,
        arguments: &Value,
        cx: &mut App,
    ) -> (CallToolCall, Task<ToolCallOutcome>) {
        let allowed_tools = AgentSettings::get_global(cx).editor_mcp_server.tools.clone();
        let is_allowed = allowed_tools.iter().any(|allowed| allowed == name);
        let run = if is_allowed {
            self.thread.update(cx, |thread, cx| {
                thread.run_external_tool(name, arguments.clone(), cx)
            })
        } else {
            None
        };
        let Some(run) = run else {
            let available = allowed_tools
                .iter()
                .filter(|allowed| self.thread.read(cx).registered_tool(allowed).is_some())
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let outcome = ToolCallOutcome::error(format!(
                "no tool named `{name}` is served; available tools: {available}"
            ));
            return (ready_call(), Task::ready(outcome));
        };
        let (task, events, mut cancel_sender) = run.into_parts();
        let task = cx.background_spawn(pump_tool_run(task, events));
        let (cancel_tx, cancel_rx) = futures::channel::oneshot::channel::<()>();
        cx.background_spawn(async move {
            if cancel_rx.await.is_ok() {
                cancel_sender.send(true).ok();
            }
        })
        .detach();
        (
            CallToolCall { cancel: cancel_tx },
            task,
        )
    }
}

/// Drains the tool run's event stream while awaiting its completion:
/// authorization requests are denied with [`AUTO_DENIED_MESSAGE`] (they are
/// interactive prompts this harness cannot answer), other events are dropped
/// (they are UI updates, not answers we owe anyone).
async fn pump_tool_run(
    task: Task<Result<agent::AgentToolOutput, agent::AgentToolOutput>>,
    events: futures::channel::mpsc::UnboundedReceiver<Result<agent::ThreadEvent>>,
) -> ToolCallOutcome {
    use agent::ThreadEvent;
    use futures::select;
    let mut task = task.fuse();
    let mut events = events.fuse();
    let mut denied = false;
    loop {
        select! {
            output = task => {
                return match output {
                    Ok(output) => ToolCallOutcome::ok(output_text(&output)),
                    Err(_) if denied => ToolCallOutcome::error(AUTO_DENIED_MESSAGE),
                    Err(output) => ToolCallOutcome::error(output_text(&output)),
                };
            }
            event = events.next() => {
                let Some(event) = event else { continue };
                match event {
                    Ok(ThreadEvent::ToolCallAuthorization(auth)) => {
                        deny_authorization(auth);
                        denied = true;
                    }
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
        }
    }
}

fn deny_authorization(auth: agent::ToolCallAuthorization) {
    use agent_client_protocol::v2::schema as acp_v2;
    let Some(reject) = auth
        .options
        .first_option_of_kind(acp_v2::PermissionOptionKind::RejectOnce)
        .or_else(|| {
            auth.options
                .first_option_of_kind(acp_v2::PermissionOptionKind::RejectAlways)
        })
    else {
        log::warn!(
            "MCP tool call asked for authorization with no reject option; dropping the request"
        );
        return;
    };
    let outcome =
        acp_thread::SelectedPermissionOutcome::new(reject.option_id.clone(), reject.kind.clone());
    if auth.response.send(outcome).is_err() {
        log::warn!("MCP tool call authorization request was already answered");
    } else {
        log::info!("auto-denied an MCP tool call that required interactive confirmation");
    }
}

fn output_text(output: &agent::AgentToolOutput) -> String {
    output
        .llm_output
        .iter()
        .map(|part| match part {
            language_model::LanguageModelToolResultContent::Text(text) => text.to_string(),
            language_model::LanguageModelToolResultContent::Image(_) => {
                "[tool produced an image; images are not forwarded over MCP]".to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

fn ready_call() -> CallToolCall {
    let (cancel, _) = futures::channel::oneshot::channel::<()>();
    CallToolCall { cancel }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn init_settings(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
        });
    }

    async fn dispatcher_with_tree(
        cx: &mut gpui::TestAppContext,
        tree: serde_json::Value,
    ) -> EditorMcpDispatcher {
        init_settings(cx);
        let fs = project::FakeFs::new(cx.executor());
        fs.insert_tree("/root", tree).await;
        let project =
            Project::test(fs.clone(), [std::path::Path::new("/root")], cx).await;
        cx.update(|cx| EditorMcpDispatcher::new(project, cx))
    }

    fn set_tool_permission(
        tool: &str,
        mode: Option<settings::ToolPermissionMode>,
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            let mut agent_settings = AgentSettings::get_global(cx).clone();
            agent_settings.tool_permissions.tools.insert(
                tool.into(),
                agent_settings::ToolRules {
                    default: mode,
                    ..Default::default()
                },
            );
            agent_settings
                .tool_permissions
                .tools
                .retain(|_, rules| rules.default.is_some() || tool.is_empty());
            AgentSettings::override_global(agent_settings, cx);
        });
    }

    #[gpui::test]
    async fn test_tools_reflect_allowlist_and_real_schema(cx: &mut gpui::TestAppContext) {
        let dispatcher = dispatcher_with_tree(cx, json!({ "f.txt": "x" })).await;
        let tools = cx.update(|cx| dispatcher.tools(cx));
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "edit_file");
        assert!(tools[0].description.contains("edit"));

        cx.update(|cx| {
            let mut settings = AgentSettings::get_global(cx).clone();
            settings.editor_mcp_server.tools = vec!["read_file".to_string()];
            AgentSettings::override_global(settings, cx);
        });
        let tools = cx.update(|cx| dispatcher.tools(cx));
        assert_eq!(tools[0].name, "read_file");
    }

    #[gpui::test]
    async fn test_call_tool_runs_real_edit_file(cx: &mut gpui::TestAppContext) {
        let dispatcher =
            dispatcher_with_tree(cx, json!({ "file.txt": "line 1\nline 2\n" })).await;
        set_tool_permission("edit_file", Some(settings::ToolPermissionMode::Allow), cx);

        let (_call, task) = cx.update(|cx| {
            dispatcher.call_tool(
                "edit_file",
                &json!({
                    "path": "root/file.txt",
                    "edits": [{ "old_text": "line 2", "new_text": "modified line 2" }]
                }),
                cx,
            )
        });
        let outcome = task.await;
        assert!(!outcome.is_error, "outcome: {}", outcome.text);
        assert!(outcome.text.contains("Edited"), "outcome: {}", outcome.text);
        let fs = cx.update(|cx| dispatcher.project().read(cx).fs().clone());
        let contents = fs.load(std::path::Path::new("/root/file.txt")).await.unwrap();
        assert_eq!(contents, "line 1\nmodified line 2\n");
    }

    #[gpui::test]
    async fn test_call_tool_denies_rules_and_confirmations(cx: &mut gpui::TestAppContext) {
        let dispatcher = dispatcher_with_tree(
            cx,
            json!({ "file.txt": "content\n", ".zed": { "settings.json": "{}\n" } }),
        )
        .await;

        // A deny rule: the tool's own permission path rejects the call.
        set_tool_permission("edit_file", Some(settings::ToolPermissionMode::Deny), cx);
        let (_call, task) = cx.update(|cx| {
            dispatcher.call_tool(
                "edit_file",
                &json!({
                    "path": "root/file.txt",
                    "edits": [{ "old_text": "content", "new_text": "changed" }]
                }),
                cx,
            )
        });
        let outcome = task.await;
        assert!(outcome.is_error, "outcome: {}", outcome.text);
        assert!(!outcome.text.contains(AUTO_DENIED_MESSAGE));
        let fs = cx.update(|cx| dispatcher.project().read(cx).fs().clone());
        let contents = fs.load(std::path::Path::new("/root/file.txt")).await.unwrap();
        assert_eq!(contents, "content\n");

        // Sensitive paths still prompt; the harness auto-denies with
        // instructions instead of hanging.
        cx.update(|cx| {
            let mut agent_settings = AgentSettings::get_global(cx).clone();
            agent_settings.tool_permissions.tools.clear();
            agent_settings.tool_permissions.default = settings::ToolPermissionMode::Confirm;
            AgentSettings::override_global(agent_settings, cx);
        });
        let (_call, task) = cx.update(|cx| {
            dispatcher.call_tool(
                "edit_file",
                &json!({
                    "path": "root/.zed/settings.json",
                    "edits": [{ "old_text": "{}", "new_text": "{}\n" }]
                }),
                cx,
            )
        });
        let outcome = task.await;
        assert!(outcome.is_error, "outcome: {}", outcome.text);
        assert_eq!(outcome.text, AUTO_DENIED_MESSAGE);
    }

    #[gpui::test]
    async fn test_call_tool_unknown_tool(cx: &mut gpui::TestAppContext) {
        let dispatcher = dispatcher_with_tree(cx, json!({ "file.txt": "x" })).await;

        let (_call, task) = cx.update(|cx| {
            dispatcher.call_tool("delete_path", &json!({ "path": "root/file.txt" }), cx)
        });
        let outcome = task.await;
        assert!(outcome.is_error);
        assert!(outcome.text.contains("no tool named `delete_path`"));
    }

    #[gpui::test]
    async fn test_call_tool_invalid_arguments(cx: &mut gpui::TestAppContext) {
        let dispatcher = dispatcher_with_tree(cx, json!({ "file.txt": "x" })).await;
        set_tool_permission("edit_file", Some(settings::ToolPermissionMode::Allow), cx);

        let (_call, task) = cx.update(|cx| {
            dispatcher.call_tool(
                "edit_file",
                &json!({ "path": "root/file.txt", "edits": 42 }),
                cx,
            )
        });
        let outcome = task.await;
        assert!(outcome.is_error, "outcome: {}", outcome.text);
        // The file is untouched for a call whose arguments never parsed.
        let fs = cx.update(|cx| dispatcher.project().read(cx).fs().clone());
        let contents = fs.load(std::path::Path::new("/root/file.txt")).await.unwrap();
        assert_eq!(contents, "x");
    }
}
