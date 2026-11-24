use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use zed_review_mcp::ReviewStore;
use zed_review_mcp::review_server::ReviewServer;

const TIMEOUT: Duration = Duration::from_secs(20);

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

struct TestProject {
    _project_dir: tempfile::TempDir,
    _session_dir: tempfile::TempDir,
    root: PathBuf,
}

impl TestProject {
    fn new() -> Self {
        let project_dir = tempfile::TempDir::new().unwrap();
        let session_dir = tempfile::TempDir::new().unwrap();
        Self {
            root: project_dir.path().canonicalize().unwrap(),
            _project_dir: project_dir,
            _session_dir: session_dir,
        }
    }

    fn store(&self) -> ReviewStore {
        ReviewStore::start(&self.root, self._session_dir.path()).unwrap()
    }

    fn write_relative(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
        path
    }
}

async fn start_server(project: &TestProject) -> Result<RawMcpClient> {
    let review_store = Arc::new(project.store());
    let (server_stream, client_stream) = tokio::io::duplex(64 * 1024);
    let server_task = tokio::spawn(async move {
        let server = ReviewServer::new(review_store);
        use rmcp::ServiceExt;
        match server.serve(server_stream).await {
            Ok(service) => {
                let _ = service.waiting().await;
            }
            Err(error) => eprintln!("serve error in test: {error:?}"),
        }
    });
    tokio::spawn(async move {
        let _ = server_task.await;
    });
    let mut client = RawMcpClient::new(client_stream);
    client.initialize().await?;
    Ok(client)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_metadata_lists_review_tools_with_schemas() {
    let project = TestProject::new();
    let mut client = start_server(&project).await.unwrap();
    let tools = client.list_tools().await.unwrap();
    let mut names = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        vec!["approve", "get_change_status", "record_baseline", "reject"]
    );
    for tool in &tools {
        assert!(
            tool["inputSchema"]["properties"]
                .as_object()
                .or(tool["inputSchema"].as_object())
                .is_some()
                || tool["description"].as_str().is_some(),
            "tool must carry a schema or description: {tool}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_review_round_trip_over_protocol() {
    let project = TestProject::new();
    let mut client = start_server(&project).await.unwrap();

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
async fn test_decide_all_over_protocol() {
    let project = TestProject::new();
    let mut client = start_server(&project).await.unwrap();
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
    let mut client = start_server(&project).await.unwrap();
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
