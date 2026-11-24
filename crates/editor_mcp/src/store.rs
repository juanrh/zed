//! Boots and tears down per-workspace MCP servers based on the
//! `agent.editor_mcp_server` settings, and keeps them in sync when the
//! settings change mid-session.

use crate::server::{self, EditorMcpServer};
use agent_settings::AgentSettings;
use collections::HashMap;
use gpui::{App, BorrowAppContext, Entity, EntityId, Global, WeakEntity};
use settings::{Settings, SettingsStore};
use workspace::Workspace;

struct WorkspaceEntry {
    workspace: WeakEntity<Workspace>,
    server: Option<Entity<EditorMcpServer>>,
}

#[derive(Default)]
pub struct EditorMcpStore {
    entries: HashMap<EntityId, WorkspaceEntry>,
}

impl Global for EditorMcpStore {}

pub fn init(cx: &mut App) {
    cx.set_global(EditorMcpStore::default());
    cx.observe_new::<Workspace>(|_, _window, cx| {
        let handle = cx.entity();
        let workspace_id = handle.entity_id();
        let weak_workspace = handle.downgrade();
        cx.observe_release(&handle, move |_this, _released, cx| {
            cx.update_global::<EditorMcpStore, _>(|store, cx| {
                store.remove_workspace(workspace_id, cx);
            });
        })
        .detach();
        cx.update_global::<EditorMcpStore, _>(|store, cx| {
            store.track(workspace_id, weak_workspace, cx);
        });
    })
    .detach();
    cx.observe_global::<SettingsStore>(move |cx| {
        cx.update_global::<EditorMcpStore, _>(|store, cx| store.sync_all(cx));
    })
    .detach();
}

impl EditorMcpStore {
    fn track(&mut self, workspace_id: EntityId, workspace: WeakEntity<Workspace>, cx: &mut App) {
        self.entries.insert(
            workspace_id,
            WorkspaceEntry {
                workspace,
                server: None,
            },
        );
        self.sync_entry(workspace_id, cx);
    }

    fn remove_workspace(&mut self, workspace_id: EntityId, cx: &mut App) {
        if let Some(entry) = self.entries.remove(&workspace_id) {
            if let Some(server) = entry.server {
                server.update(cx, |server, _| server.shutdown());
            }
        }
    }

    fn sync_all(&mut self, cx: &mut App) {
        let ids: Vec<EntityId> = self.entries.keys().copied().collect();
        for id in ids {
            self.sync_entry(id, cx);
        }
    }

    fn sync_entry(&mut self, workspace_id: EntityId, cx: &mut App) {
        let entry = match self.entries.get_mut(&workspace_id) {
            Some(entry) => entry,
            None => return,
        };
        // Forget workspaces that closed without a release notification.
        let Some(workspace) = entry.workspace.upgrade() else {
            if let Some(server) = entry.server.take() {
                server.update(cx, |server, _| server.shutdown());
            }
            self.entries.remove(&workspace_id);
            return;
        };

        let enabled = AgentSettings::get_global(cx)
            .editor_mcp_server
            .enabled
            && AgentSettings::get_global(cx).enabled(cx);

        if !enabled {
            if let Some(server) = entry.server.take() {
                log::info!("editor MCP server disabled; stopping {}", server.read(cx).socket_path().display());
                server.update(cx, |server, _| server.shutdown());
            }
            return;
        }

        if entry.server.is_some() {
            // Already serving this workspace; the allowlist is read live at
            // tools/list and tools/call time, so nothing to restart.
            return;
        }

        let (project, worktree_roots) = workspace.read_with(cx, |workspace, cx| {
            let roots = workspace
                .worktrees(cx)
                .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
                .collect::<Vec<_>>();
            (workspace.project().clone(), roots)
        });
        if worktree_roots.is_empty() {
            log::debug!("editor MCP server not started: workspace has no worktrees yet");
            return;
        }
        let socket_path = server::socket_path_for(&worktree_roots);
        match EditorMcpServer::start(project, socket_path, cx) {
            Some(server_entity) => {
                log::info!(
                    "editor MCP server serving this workspace on {}",
                    server_entity.read(cx).socket_path().display()
                );
                entry.server = Some(server_entity);
            }
            None => {
                // `EditorMcpServer::start` already logged why (another live
                // window owns the socket, or the path cannot be bound).
            }
        }
    }
}

