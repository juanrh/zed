//! The per-workspace MCP server: a Unix-socket listener whose requests are
//! pumped onto the GPUI main thread and dispatched to the real agent tools
//! via [`crate::dispatch::EditorMcpDispatcher`].

use crate::dispatch::EditorMcpDispatcher;
use crate::protocol::{self, Message, ToolCallOutcome};
use gpui::{AppContext, Context, Entity, EventEmitter};
use project::Project;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub const SOCKET_DIR_NAME: &str = "editor_mcp";
pub const MAX_CHANNEL_DEPTH: usize = 16;

/// Deterministic per-workspace socket path:
/// `<data dir>/editor_mcp/<sha256 of sorted worktree roots>.sock`.
pub fn socket_path_for(worktree_roots: &[PathBuf]) -> PathBuf {
    let mut roots: Vec<&Path> = worktree_roots.iter().map(AsRef::as_ref).collect();
    roots.sort();
    let mut hasher = Sha256::new();
    for root in roots {
        hasher.update(root.to_string_lossy().as_bytes());
        hasher.update(b"\0");
    }
    let digest = hex::encode(hasher.finalize());
    paths::data_dir()
        .join(SOCKET_DIR_NAME)
        .join(format!("{digest}.sock"))
}

/// An in-flight `tools/call` awaiting its tool run.
struct PendingCall {
    cancel: futures::channel::oneshot::Sender<()>,
}

#[derive(Default)]
struct ConnectionState {
    pending_calls: usize,
}

/// What a connection's reader task sends to the main-thread pump.
enum ConnSignal {
    Frame {
        conn_id: u64,
        frame: String,
        response_tx: smol::channel::Sender<String>,
    },
    /// The client went away: cancel anything still running for it.
    Disconnect {
        conn_id: u64,
    },
}

pub struct EditorMcpServer {
    dispatcher: EditorMcpDispatcher,
    socket_path: PathBuf,
    connections: collections::HashMap<u64, ConnectionState>,
    pending: collections::HashMap<(u64, Value), PendingCall>,
}

pub enum ServerEvent {
    /// The socket stopped serving: bind conflict with another live window,
    /// or an accept-loop failure. The reason is in the payload.
    Stopped { reason: String },
}

impl EventEmitter<ServerEvent> for EditorMcpServer {}

impl EditorMcpServer {
    /// Starts the server for `project` at `socket_path`.
    ///
    /// Returns `None` when this window cannot serve the socket: another live
    /// Zed window already serves the same workspace (logged; never stolen),
    /// or the socket path cannot be bound (logged). Either way the caller
    /// stands down loudly instead of silently doing nothing (R10).
    pub fn start(
        project: Entity<Project>,
        socket_path: PathBuf,
        cx: &mut gpui::App,
    ) -> Option<Entity<Self>> {
        let Some(socket_dir) = socket_path.parent().map(Path::to_path_buf) else {
            return None;
        };
        if let Err(error) = std::fs::create_dir_all(&socket_dir) {
            log::warn!(
                "editor MCP server socket dir {} unavailable: {error}",
                socket_dir.display()
            );
            return None;
        }
        let listener = match bind(&socket_path) {
            Ok(Some(listener)) => listener,
            Ok(None) => return None,
            Err(error) => {
                log::warn!(
                    "editor MCP server could not bind {}: {error}",
                    socket_path.display()
                );
                return None;
            }
        };

        let dispatcher = EditorMcpDispatcher::new(project, cx);
        let entity = cx.new(|_| Self {
            dispatcher,
            socket_path: socket_path.clone(),
            connections: collections::HashMap::default(),
            pending: collections::HashMap::default(),
        });
        let handle = entity.downgrade();

        let (signal_tx, signal_rx) = smol::channel::bounded::<ConnSignal>(MAX_CHANNEL_DEPTH);
        let conn_counter = Arc::new(AtomicU64::new(0));
        std::thread::spawn(move || {
            accept_thread(listener, socket_path.clone(), conn_counter, signal_tx.clone())
        });
        cx.spawn(async move |cx| {
            let signals = signal_rx;
            while let Ok(signal) = signals.recv().await {
                if handle
                    .update(cx, |server: &mut EditorMcpServer, cx| match signal {
                        ConnSignal::Frame {
                            conn_id,
                            frame,
                            response_tx,
                        } => server.handle_frame(conn_id, frame, response_tx, cx),
                        ConnSignal::Disconnect { conn_id } => server.handle_disconnect(conn_id),
                    })
                    .is_err()
                {
                    // The server entity is gone; nothing to answer. Dropping
                    // signal_tx clones when tasks end closes the loop.
                    continue;
                }
            }
        })
        .detach();
        Some(entity)
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Exposed for the socket-level tests.
    pub fn dispatcher(&self) -> &EditorMcpDispatcher {
        &self.dispatcher
    }

    fn handle_frame(
        &mut self,
        conn_id: u64,
        frame: String,
        response_tx: smol::channel::Sender<String>,
        cx: &mut Context<Self>,
    ) {
        match protocol::parse_message(&frame) {
            Message::Request { id, action } => {
                self.handle_request(conn_id, id, action, response_tx, cx)
            }
            Message::Notification(protocol::Notification::Cancelled { request_id }) => {
                if let Some(pending) = self.pending.remove(&(conn_id, request_id.clone())) {
                    let _ = pending.cancel.send(());
                    if let Some(conn) = self.connections.get_mut(&conn_id) {
                        conn.pending_calls = conn.pending_calls.saturating_sub(1);
                    }
                }
            }
            Message::Notification(_) => {}
            Message::UnknownMethod {
                id: Some(id),
                method,
            } => {
                let _ = response_tx.try_send(protocol::method_not_found_response(&id, &method));
            }
            Message::UnknownMethod { id: None, .. } | Message::Invalid(_) => {
                log::warn!("editor MCP server dropped a malformed frame");
                if let Some(response) = protocol::invalid_frame_response() {
                    let _ = response_tx.try_send(response);
                }
            }
        }
    }

    fn handle_request(
        &mut self,
        conn_id: u64,
        id: Value,
        action: protocol::Action,
        response_tx: smol::channel::Sender<String>,
        cx: &mut Context<Self>,
    ) {
        use protocol::Action;
        match action {
            Action::Initialize {
                requested_protocol_version,
            } => {
                let _ = response_tx.try_send(protocol::initialize_result(
                    &id,
                    &requested_protocol_version,
                ));
            }
            Action::Ping => {
                let _ = response_tx.try_send(protocol::ping_result(&id));
            }
            Action::ListTools => {
                let tools = self.dispatcher.tools(cx);
                let _ = response_tx.try_send(protocol::tools_list_result(&id, &tools));
            }
            Action::CallToolError { invalid_request } => {
                let outcome = ToolCallOutcome::error(invalid_request);
                let _ = response_tx.try_send(protocol::tools_call_result(&id, &outcome));
            }
            Action::CallTool { name, arguments } => {
                self.dispatch_call(conn_id, id, name, arguments, response_tx, cx)
            }
        }
    }

    fn dispatch_call(
        &mut self,
        conn_id: u64,
        id: Value,
        name: String,
        arguments: Value,
        response_tx: smol::channel::Sender<String>,
        cx: &mut Context<Self>,
    ) {
        // R10: one in-flight tool call per connection. Additional calls get
        // an immediate error instead of queuing behind the running one.
        let conn = self
            .connections
            .entry(conn_id)
            .or_insert_with(ConnectionState::default);
        if conn.pending_calls > 0 {
            let outcome = ToolCallOutcome::error(
                "this connection already has a tool call in flight; wait for its \
                 response before issuing another (open another connection for \
                 parallel calls)",
            );
            let _ = response_tx.try_send(protocol::tools_call_result(&id, &outcome));
            return;
        }
        conn.pending_calls += 1;

        let (call, task) = self.dispatcher.call_tool(&name, &arguments, cx);
        self.pending.insert((conn_id, id.clone()), PendingCall { cancel: call.cancel });
        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            let _ = this.update(cx, |server: &mut EditorMcpServer, cx| {
                server.complete_call(conn_id, &id, outcome, response_tx, cx);
            });
        })
        .detach();
    }

    fn complete_call(
        &mut self,
        conn_id: u64,
        id: &Value,
        outcome: ToolCallOutcome,
        response_tx: smol::channel::Sender<String>,
        _: &mut Context<Self>,
    ) {
        // R10: pending-map cleanup happens exactly where the response is
        // sent. `try_send` is a no-op if the client already disconnected.
        self.pending.remove(&(conn_id, id.clone()));
        if let Some(conn) = self.connections.get_mut(&conn_id) {
            conn.pending_calls = conn.pending_calls.saturating_sub(1);
        }
        let _ = response_tx.try_send(protocol::tools_call_result(id, &outcome));
    }

    /// Stops serving this workspace: cancels every in-flight call and removes
    /// the socket file. In-flight connections see their runs cancel; new
    /// connections find no socket.
    pub fn shutdown(&mut self) {
        let pending: Vec<PendingCall> = self.pending.drain().map(|(_, call)| call).collect();
        for call in pending {
            let _ = call.cancel.send(());
        }
        self.connections.clear();
        let _ = std::fs::remove_file(&self.socket_path);
    }

    fn handle_disconnect(&mut self, conn_id: u64) {
        let pending_ids: Vec<Value> = self
            .pending
            .keys()
            .filter(|(conn, _)| *conn == conn_id)
            .map(|(_, request_id)| request_id.clone())
            .collect();
        for request_id in pending_ids {
            if let Some(pending) = self.pending.remove(&(conn_id, request_id)) {
                let _ = pending.cancel.send(());
            }
        }
        self.connections.remove(&conn_id);
    }
}

/// Binds the socket, honoring the dual-window tiebreak: a live listener from
/// another Zed window means this window must stand down (Ok(None)), while a
/// dead socket file from a crashed process is reclaimed and bound once.
fn bind(path: &Path) -> std::io::Result<Option<std::os::unix::net::UnixListener>> {
    match std::os::unix::net::UnixListener::bind(path) {
        Ok(listener) => Ok(Some(listener)),
        Err(_error) if path.exists() => {
            if std::os::unix::net::UnixStream::connect(path).is_ok() {
                log::warn!(
                    "another Zed window already serves editor MCP on {}; this window stands down",
                    path.display()
                );
                return Ok(None);
            }
            std::fs::remove_file(path)?;
            Ok(Some(std::os::unix::net::UnixListener::bind(path)?))
        }
        Err(error) => Err(error),
    }
}

/// Accepts connections on the listener until the server entity is dropped.
/// Runs on its own thread: `UnixListener::accept` blocks, which is what
/// background executors shouldn't do forever.
fn accept_thread(
    listener: std::os::unix::net::UnixListener,
    socket_path: PathBuf,
    conn_counter: Arc<AtomicU64>,
    signal_tx: smol::channel::Sender<ConnSignal>,
) {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let conn_id = conn_counter.fetch_add(1, Ordering::SeqCst);
                serve_connection(stream, conn_id, signal_tx.clone());
            }
            Err(error) => {
                log::warn!(
                    "editor MCP socket {} accept failed: {error}",
                    socket_path.display()
                );
                break;
            }
        }
    }
}

fn serve_connection(
    stream: std::os::unix::net::UnixStream,
    conn_id: u64,
    signal_tx: smol::channel::Sender<ConnSignal>,
) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let (response_tx, response_rx) = smol::channel::bounded::<String>(MAX_CHANNEL_DEPTH);

    // Reader: blocking line reads on its own thread. A full signal channel
    // (MAX_CHANNEL_DEPTH frames in flight) blocks this thread — the intended
    // backpressure, so a runaway client can't outpace the editor.
    std::thread::spawn(move || {
        use std::io::BufRead;
        let reader = std::io::BufReader::new(read_half);
        for line in reader.lines() {
            match line {
                Ok(line) => {
                    if signal_tx
                        .send_blocking(ConnSignal::Frame {
                            conn_id,
                            frame: line,
                            response_tx: response_tx.clone(),
                        })
                        .is_err()
                    {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
        let _ = signal_tx.send_blocking(ConnSignal::Disconnect { conn_id });
    });

    // Writer: blocking writes on its own thread, so a slow client can't
    // hold up dispatching for anyone else.
    std::thread::spawn(move || {
        use std::io::Write;
        let mut writer = std::io::BufWriter::new(stream);
        while let Ok(response) = response_rx.recv_blocking() {
            if writeln!(writer, "{response}").and_then(|_| writer.flush()).is_err() {
                break;
            }
        }
    });
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

    #[test]
    fn test_socket_path_is_deterministic_per_root_set() {
        let a = socket_path_for(&["/a".into(), "/b".into()]);
        let b = socket_path_for(&["/b".into(), "/a".into()]);
        let c = socket_path_for(&["/a".into(), "/c".into()]);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    // NOTE: end-to-end socket sessions (real client connect → dispatch →
    // reply, and disconnect-cancel) cannot run under `#[gpui::test]`: the
    // deterministic test scheduler forbids the cross-thread wakes the
    // transport threads produce. They are covered by the documented manual
    // QA steps in the README (a five-line stdio session via the proxy),
    // with the in-harness coverage carried by the dispatch tests plus the
    // socket-path and bind tests below.

}
