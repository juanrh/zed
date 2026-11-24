mod common;

use anyhow::{Context as _, Result};
use common::{FakeZed, TestProject};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use zed_review_mcp::agent_bridge::AgentBridgeClient;
use zed_review_mcp::review_server::ReviewServer;

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

struct RawMcpClient {
    reader: BufReader<ReadHalf<DuplexStream>>,
    writer: WriteHalf<DuplexStream>,
    next_request_id: u64,
}

impl RawMcpClient {
    fn new(stream: DuplexStream) -> Self {
        let (reader, writer) = tokio::io::split(stream);
        Self {
            reader: BufReader::new(reader),
            writer,
            next_request_id: 1,
        }
    }

    async fn send(&mut self, payload: &Value) -> Result<()> {
        let line = format!("{payload}\n");
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<u64> {
        let request_id = self.next_request_id;
        self.next_request_id += 1;
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params,
        }))
        .await?;
        Ok(request_id)
    }

    async fn notification(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .await
    }

    async fn read_message(&mut self) -> Result<Value> {
        let mut line = String::new();
        let bytes = tokio::time::timeout(TIMEOUT, self.reader.read_line(&mut line))
            .await
            .context("timed out waiting for a server message")?
            .context("reading a message from the server")?;
        anyhow::ensure!(bytes > 0, "server closed the connection");
        Ok(serde_json::from_str(&line)?)
    }

    async fn wait_for_response(&mut self, request_id: u64) -> Result<Value> {
        loop {
            let message = self.read_message().await?;
            if message.get("id").and_then(Value::as_u64) == Some(request_id) {
                if let Some(error) = message.get("error") {
                    anyhow::bail!("request {request_id} failed: {error}");
                }
                return Ok(message["result"].clone());
            }
        }
    }

    async fn initialize(&mut self) -> Result<Value> {
        let request_id = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "test-client", "version": "0.0.0"},
                }),
            )
            .await?;
        let result = self.wait_for_response(request_id).await?;
        self.notification("notifications/initialized", json!({}))
            .await?;
        Ok(result)
    }

    async fn list_tools(&mut self) -> Result<Vec<Value>> {
        let request_id = self.request("tools/list", json!({})).await?;
        let result = self.wait_for_response(request_id).await?;
        Ok(result["tools"]
            .as_array()
            .context("tools/list result has no tools array")?
            .clone())
    }

    async fn call_tool(&mut self, tool_name: &str, arguments: Value) -> Result<ToolOutcome> {
        let request_id = self
            .request(
                "tools/call",
                json!({"name": tool_name, "arguments": arguments}),
            )
            .await?;
        let result = self.wait_for_response(request_id).await?;
        let text = result["content"]
            .as_array()
            .context("tools/call result has no content")?
            .iter()
            .filter_map(|content| content["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutcome {
            is_error: result["isError"].as_bool().unwrap_or(false),
            text,
        })
    }
}

struct ToolOutcome {
    is_error: bool,
    text: String,
}

async fn start_server(project: &TestProject, fake_zed: &FakeZed) -> Result<RawMcpClient> {
    start_server_with_bridge(project, fake_zed, None).await
}

async fn start_server_with_bridge(
    project: &TestProject,
    fake_zed: &FakeZed,
    bridge_socket_path: Option<std::path::PathBuf>,
) -> Result<RawMcpClient> {
    let review_store = Arc::new(project.store());
    let (server_stream, client_stream) = tokio::io::duplex(64 * 1024);
    let zed_opener = Arc::new(fake_zed.opener());
    let agent_bridge = bridge_socket_path
        .map(AgentBridgeClient::from_socket_path)
        .unwrap_or_else(|| {
            AgentBridgeClient::from_socket_path(std::path::PathBuf::from(
                "/nonexistent/zed-agent-tool-bridge.sock",
            ))
        });
    tokio::spawn(async move {
        let server = ReviewServer::with_agent_bridge(review_store, zed_opener, agent_bridge);
        use rmcp::ServiceExt;
        match server.serve(server_stream).await {
            Ok(service) => {
                let _ = service.waiting().await;
            }
            Err(error) => eprintln!("serve failed inside test: {error:?}"),
        }
    });
    let mut client = RawMcpClient::new(client_stream);
    client.initialize().await?;
    Ok(client)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_metadata_lists_review_tools_with_descriptions() {
    let project = TestProject::new();
    let fake_zed = FakeZed::new();
    let mut client = start_server(&project, &fake_zed).await.unwrap();
    let tools = client.list_tools().await.unwrap();
    let mut names = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        vec![
            "approve",
            "diagnostics",
            "edit",
            "edit_file",
            "get_change_status",
            "grep",
            "open_paths",
            "open_review",
            "record_baseline",
            "reject",
        ]
    );
    for tool in &tools {
        assert!(
            tool["description"].as_str().is_some_and(|d| !d.is_empty()),
            "every tool must document itself for the agent: {tool}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_review_round_trip_over_protocol() {
    let project = TestProject::new();
    let fake_zed = FakeZed::new();
    let mut client = start_server(&project, &fake_zed).await.unwrap();

    let code_path = project.write_relative("src/hello.py", "print('original')\n");
    let new_file_path = project.root.join("src/generated.py");

    let outcome = client
        .call_tool(
            "record_baseline",
            json!({"paths": [
                code_path.to_string_lossy(),
                new_file_path.to_string_lossy(),
            ]}),
        )
        .await
        .unwrap();
    assert!(
        !outcome.is_error,
        "record_baseline failed: {}",
        outcome.text
    );
    assert!(outcome.text.contains("recorded"), "{}", outcome.text);
    assert!(
        outcome.text.contains("marked for creation"),
        "{}",
        outcome.text
    );

    let outcome = client
        .call_tool("get_change_status", json!({}))
        .await
        .unwrap();
    assert!(
        !outcome.is_error,
        "get_change_status failed: {}",
        outcome.text
    );
    assert!(
        outcome.text.contains("src/hello.py: unchanged"),
        "{}",
        outcome.text
    );
    assert!(
        outcome.text.contains("src/generated.py: created"),
        "{}",
        outcome.text
    );

    project.write_relative("src/hello.py", "print('proposed')\n");

    let outcome = client
        .call_tool("get_change_status", json!({}))
        .await
        .unwrap();
    assert!(
        outcome.text.contains("src/hello.py: modified"),
        "{}",
        outcome.text
    );

    let outcome = client
        .call_tool("reject", json!({"paths": [code_path.to_string_lossy()]}))
        .await
        .unwrap();
    assert!(!outcome.is_error, "reject failed: {}", outcome.text);

    assert_eq!(
        std::fs::read_to_string(&code_path).unwrap(),
        "print('original')\n"
    );

    let outcome = client
        .call_tool(
            "approve",
            json!({"paths": [new_file_path.to_string_lossy()]}),
        )
        .await
        .unwrap();
    assert!(!outcome.is_error, "approve failed: {}", outcome.text);

    let outcome = client
        .call_tool("get_change_status", json!({}))
        .await
        .unwrap();
    assert!(
        outcome.text.contains("no files are under review"),
        "{}",
        outcome.text
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_edit_tool_registers_baseline_and_open_review_spawns_zed_diff() {
    let project = TestProject::new();
    let fake_zed = FakeZed::new();
    let mut client = start_server(&project, &fake_zed).await.unwrap();

    let code_path = project.write_relative("hello.py", "value = 1\n");

    let outcome = client
        .call_tool(
            "edit",
            json!({
                "file_path": code_path.to_string_lossy(),
                "old_string": "value = 1",
                "new_string": "value = 2",
            }),
        )
        .await
        .unwrap();
    assert!(!outcome.is_error, "edit failed: {}", outcome.text);
    assert!(
        outcome.text.contains("baseline registered"),
        "{}",
        outcome.text
    );
    assert_eq!(std::fs::read_to_string(&code_path).unwrap(), "value = 2\n");

    let outcome = client
        .call_tool(
            "edit",
            json!({
                "file_path": code_path.to_string_lossy(),
                "old_string": "value = 2",
                "new_string": "value = 3",
            }),
        )
        .await
        .unwrap();
    assert!(!outcome.is_error, "second edit failed: {}", outcome.text);
    assert!(
        !outcome.text.contains("baseline registered"),
        "the baseline must not be re-anchored: {}",
        outcome.text
    );
    assert_eq!(std::fs::read_to_string(&code_path).unwrap(), "value = 3\n");

    let outcome = client.call_tool("open_review", json!({})).await.unwrap();
    assert!(!outcome.is_error, "open_review failed: {}", outcome.text);
    assert!(
        outcome.text.contains("1 changed file(s)"),
        "{}",
        outcome.text
    );
    fake_zed.wait_for_invocation();
    let invocations = fake_zed.recorded_invocations();
    assert_eq!(invocations.len(), 1, "{invocations:?}");
    assert!(
        invocations[0].starts_with("--diff ")
            && invocations[0].contains("review-baseline-view")
            && invocations[0].contains("review-work-view"),
        "the diff must span the session review views, not the whole project: {invocations:?}"
    );

    let outcome = client
        .call_tool(
            "open_paths",
            json!({"paths": [code_path.to_string_lossy()]}),
        )
        .await
        .unwrap();
    assert!(!outcome.is_error, "open_paths failed: {}", outcome.text);
    fake_zed.wait_for_invocation_count(2);
    let invocations = fake_zed.recorded_invocations();
    assert_eq!(invocations.len(), 2, "{invocations:?}");
    assert_eq!(
        invocations[1],
        code_path.to_string_lossy().to_string(),
        "{invocations:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_edit_tool_error_cases_leave_no_baseline() {
    let project = TestProject::new();
    let fake_zed = FakeZed::new();
    let mut client = start_server(&project, &fake_zed).await.unwrap();

    let code_path = project.write_relative("poem.txt", "roses\n");

    let not_found = client
        .call_tool(
            "edit",
            json!({
                "file_path": code_path.to_string_lossy(),
                "old_string": "violets",
                "new_string": "daisies",
            }),
        )
        .await
        .unwrap();
    assert!(not_found.is_error, "{}", not_found.text);
    assert!(not_found.text.contains("not found"), "{}", not_found.text);

    let ambiguous_source = project.write_relative("repeated.txt", "da\nda\nda\n");
    let ambiguous = client
        .call_tool(
            "edit",
            json!({
                "file_path": ambiguous_source.to_string_lossy(),
                "old_string": "da",
                "new_string": "di",
            }),
        )
        .await
        .unwrap();
    assert!(ambiguous.is_error, "{}", ambiguous.text);
    assert!(ambiguous.text.contains("3 times"), "{}", ambiguous.text);

    let identical = client
        .call_tool(
            "edit",
            json!({
                "file_path": code_path.to_string_lossy(),
                "old_string": "roses",
                "new_string": "roses",
            }),
        )
        .await
        .unwrap();
    assert!(identical.is_error, "{}", identical.text);

    let outcome = client
        .call_tool("get_change_status", json!({}))
        .await
        .unwrap();
    assert!(
        outcome.text.contains("no files are under review"),
        "failed edits must leave no baseline side effects: {}",
        outcome.text
    );

    let outcome = client
        .call_tool(
            "edit",
            json!({
                "file_path": code_path.to_string_lossy(),
                "old_string": "roses",
                "new_string": "violets",
                "replace_all": true,
            }),
        )
        .await
        .unwrap();
    assert!(!outcome.is_error, "{}", outcome.text);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_decide_all_over_protocol() {
    let project = TestProject::new();
    let fake_zed = FakeZed::new();
    let mut client = start_server(&project, &fake_zed).await.unwrap();
    let first_path = project.write_relative("one.txt", "1\n");
    let second_path = project.write_relative("two.txt", "2\n");
    client
        .call_tool(
            "record_baseline",
            json!({"paths": [
                first_path.to_string_lossy(),
                second_path.to_string_lossy(),
            ]}),
        )
        .await
        .unwrap();

    project.write_relative("two.txt", "changed\n");

    let outcome = client.call_tool("approve", json!({})).await.unwrap();
    assert!(!outcome.is_error, "approve failed: {}", outcome.text);
    assert!(
        outcome.text.contains("all files under review"),
        "{}",
        outcome.text
    );
    assert_eq!(std::fs::read_to_string(&second_path).unwrap(), "changed\n");

    let outcome = client
        .call_tool("get_change_status", json!({}))
        .await
        .unwrap();
    assert!(
        outcome.text.contains("no files are under review"),
        "{}",
        outcome.text
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_tool_errors_report_is_error_with_offending_path() {
    let project = TestProject::new();
    let fake_zed = FakeZed::new();
    let mut client = start_server(&project, &fake_zed).await.unwrap();
    let code_path = project.write_relative("hello.py", "print('hi')\n");

    let outcome = client
        .call_tool(
            "record_baseline",
            json!({"paths": [code_path.to_string_lossy()]}),
        )
        .await
        .unwrap();
    assert!(!outcome.is_error, "{}", outcome.text);

    let repeated = client
        .call_tool(
            "record_baseline",
            json!({"paths": [code_path.to_string_lossy()]}),
        )
        .await
        .unwrap();
    assert!(
        repeated.is_error,
        "expected an error result: {}",
        repeated.text
    );
    assert!(repeated.text.contains("hello.py"), "{}", repeated.text);
    assert!(
        repeated.text.contains("already under review"),
        "{}",
        repeated.text
    );

    let outside = tempfile::TempDir::new().unwrap();
    let outside_path = outside.path().join("outside.py");
    std::fs::write(&outside_path, "x").unwrap();
    let outcome = client
        .call_tool(
            "record_baseline",
            json!({"paths": [outside_path.to_string_lossy()]}),
        )
        .await
        .unwrap();
    assert!(outcome.is_error, "{}", outcome.text);
    assert!(outcome.text.contains("trusted root"), "{}", outcome.text);

    let outcome = client
        .call_tool("approve", json!({"paths": []}))
        .await
        .unwrap();
    assert!(outcome.is_error, "{}", outcome.text);
    assert!(outcome.text.contains("at least one"), "{}", outcome.text);

    let outcome = client
        .call_tool(
            "reject",
            json!({"paths": [project.root.join("missing.py").to_string_lossy()]}),
        )
        .await
        .unwrap();
    assert!(outcome.is_error, "{}", outcome.text);
    assert!(
        outcome.text.contains("missing.py") && outcome.text.contains("no baseline"),
        "{}",
        outcome.text
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bridge_tools_forward_to_agent_tool_bridge() {
    let project = TestProject::new();
    let fake_zed = FakeZed::new();

    let bridge_dir = tempfile::TempDir::new().unwrap();
    let socket_path = bridge_dir.path().join("bridge.sock");
    let (request_sender, mut request_receiver) = tokio::sync::mpsc::channel::<serde_json::Value>(8);
    let listener_socket_path = socket_path.clone();
    tokio::spawn(async move {
        let listener = tokio::net::UnixListener::bind(&listener_socket_path).unwrap();
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let request_sender = request_sender.clone();
            tokio::spawn(async move {
                let (reader, mut writer) = stream.into_split();
                let mut reader = tokio::io::BufReader::new(reader);
                let mut request_line = String::new();
                use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
                loop {
                    request_line.clear();
                    if reader.read_line(&mut request_line).await.unwrap() == 0 {
                        return;
                    }
                    let request: serde_json::Value =
                        serde_json::from_str(request_line.trim()).unwrap();
                    let _ = request_sender.send(request["params"].clone()).await;
                    let response = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": {
                            "content": [{
                                "type": "text",
                                "text": format!(
                                    "ran in zed: {}",
                                    request["params"]["tool"].as_str().unwrap()
                                ),
                            }],
                        },
                    });
                    writer
                        .write_all(format!("{response}\n").as_bytes())
                        .await
                        .unwrap();
                }
            });
        }
    });

    let mut client = start_server_with_bridge(&project, &fake_zed, Some(socket_path.clone()))
        .await
        .unwrap();

    let outcome = client
        .call_tool(
            "edit_file",
            json!({
                "path": "backend/src/main.rs",
                "edits": [{"old_text": "a", "new_text": "b"}],
            }),
        )
        .await
        .unwrap();
    assert!(!outcome.is_error, "{}", outcome.text);
    assert!(
        outcome.text.contains("edit_file executed in Zed"),
        "{}",
        outcome.text
    );
    assert!(
        outcome.text.contains("ran in zed: edit_file"),
        "{}",
        outcome.text
    );
    let forwarded = request_receiver.recv().await.unwrap();
    assert_eq!(forwarded["tool"], "edit_file");
    assert_eq!(forwarded["path"], "backend/src/main.rs");
    assert_eq!(forwarded["edits"][0]["old_text"], "a");

    let outcome = client
        .call_tool("grep", json!({"regex": "match", "case_sensitive": false}))
        .await
        .unwrap();
    assert!(
        outcome.text.contains("ran in zed: grep"),
        "{}",
        outcome.text
    );
    let forwarded = request_receiver.recv().await.unwrap();
    assert_eq!(forwarded["tool"], "grep");
    assert_eq!(forwarded["regex"], "match");

    let outcome = client
        .call_tool("diagnostics", json!({"path": "backend/main.rs"}))
        .await
        .unwrap();
    assert!(
        outcome.text.contains("ran in zed: diagnostics"),
        "{}",
        outcome.text
    );
    let forwarded = request_receiver.recv().await.unwrap();
    assert_eq!(forwarded["tool"], "diagnostics");
    assert_eq!(forwarded["path"], "backend/main.rs");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bridge_tools_report_actionable_errors_when_bridge_is_down() {
    let project = TestProject::new();
    let fake_zed = FakeZed::new();
    let mut client = start_server(&project, &fake_zed).await.unwrap();
    let outcome = client
        .call_tool("grep", json!({"regex": "anything"}))
        .await
        .unwrap();
    assert!(outcome.is_error, "{}", outcome.text);
    assert!(outcome.text.contains("grep"), "{}", outcome.text);
    assert!(
        outcome.text.contains("ZED_AGENT_BRIDGE_SOCKET"),
        "{}",
        outcome.text
    );
}
