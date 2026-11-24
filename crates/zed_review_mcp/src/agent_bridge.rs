use anyhow::{Context as _, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub const BRIDGE_METHOD: &str = "agent_tool/run";
pub const DEFAULT_SOCKET_FILE: &str = "zed-agent-tool-bridge.sock";

const DEFAULT_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

#[derive(Clone)]
pub struct AgentBridgeClient {
    socket_path: PathBuf,
    response_timeout: std::time::Duration,
}

#[derive(Serialize)]
struct ToolRunRequest<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: ToolRunParams<'a>,
}

#[derive(Serialize)]
struct ToolRunParams<'a> {
    tool: &'a str,
    #[serde(flatten)]
    input: &'a Value,
}

#[derive(Deserialize)]
struct ToolRunResponse {
    result: Option<Value>,
    error: Option<BridgeError>,
}

#[derive(Deserialize)]
struct BridgeError {
    message: String,
}

impl AgentBridgeClient {
    pub fn from_environment() -> Self {
        Self::from_socket_path(socket_path_from_environment())
    }

    pub fn from_socket_path(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            response_timeout: response_timeout_from_environment(),
        }
    }

    pub fn socket_path(&self) -> &std::path::Path {
        &self.socket_path
    }

    pub fn response_timeout(&self) -> std::time::Duration {
        self.response_timeout
    }

    pub async fn run_tool(&self, tool_name: &str, input: &Value) -> Result<String> {
        let stream = self.connect().await.with_context(|| {
            format!(
                "the `{tool_name}` tool runs inside the running Zed editor, but the \
                 agent tool bridge socket {} is not reachable; open Zed (this fork with \
                 the agent tool bridge) for this project, or set \
                 ZED_AGENT_BRIDGE_SOCKET to the right socket path",
                self.socket_path.display()
            )
        })?;
        let (reader, mut writer) = stream.into_split();
        let request = ToolRunRequest {
            jsonrpc: "2.0",
            id: 1,
            method: BRIDGE_METHOD,
            params: ToolRunParams {
                tool: tool_name,
                input,
            },
        };
        let mut request_line =
            serde_json::to_string(&request).context("serializing the agent tool bridge request")?;
        request_line.push('\n');
        writer
            .write_all(request_line.as_bytes())
            .await
            .with_context(|| {
                format!("sending the `{tool_name}` request to the agent tool bridge")
            })?;
        writer.flush().await.with_context(|| {
            format!("flushing the `{tool_name}` request to the agent tool bridge")
        })?;

        let mut reader = BufReader::new(reader);
        let mut response_line = String::new();
        let read_bytes =
            tokio::time::timeout(self.response_timeout, reader.read_line(&mut response_line))
                .await
                .map_err(|_| {
                    anyhow!(
                        "the `{tool_name}` call did not get a reply from the agent tool bridge \
                         within {} seconds; permission-gated tools wait for the user to \
                         confirm inside Zed, so raise ZED_AGENT_BRIDGE_TIMEOUT_SECS if needed",
                        self.response_timeout.as_secs()
                    )
                })?
                .with_context(|| {
                    format!("reading the `{tool_name}` reply from the agent tool bridge")
                })?;
        if read_bytes == 0 {
            bail!("the agent tool bridge closed the connection without a reply for `{tool_name}`");
        }
        let response: ToolRunResponse =
            serde_json::from_str(&response_line).with_context(|| {
                format!("parsing the `{tool_name}` reply from the agent tool bridge")
            })?;
        if let Some(error) = response.error {
            bail!("`{tool_name}` failed in Zed: {}", error.message);
        }
        let result = response
            .result
            .ok_or_else(|| anyhow!("`{tool_name}` returned no result"))?;
        match result["content"][0]["text"].as_str() {
            Some(text) => Ok(text.to_string()),
            None => bail!(
                "the `{tool_name}` reply from Zed did not contain readable text content: {result}"
            ),
        }
    }

    async fn connect(&self) -> Result<UnixStream> {
        let stream = UnixStream::connect(&self.socket_path)
            .await
            .with_context(|| {
                format!(
                    "connecting to the agent tool bridge socket {}",
                    self.socket_path.display()
                )
            })?;
        Ok(stream)
    }
}

fn socket_path_from_environment() -> PathBuf {
    if let Some(socket_path) = std::env::var_os("ZED_AGENT_BRIDGE_SOCKET") {
        if !socket_path.is_empty() {
            return PathBuf::from(socket_path);
        }
    }
    paths::data_dir().join(DEFAULT_SOCKET_FILE)
}

fn response_timeout_from_environment() -> std::time::Duration {
    std::env::var("ZED_AGENT_BRIDGE_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(std::time::Duration::from_secs)
        .unwrap_or(DEFAULT_RESPONSE_TIMEOUT)
}

#[derive(serde::Deserialize, serde::Serialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct EditFileBridgeInput {
    /// The full path of the file to edit in the project.
    ///
    /// Start the path with one of the project's root directories (the same rule as the
    /// built-in Zed agent's `edit_file` tool).
    pub path: String,
    /// List of edit operations to apply sequentially.
    /// Each edit finds `old_text` in the file and replaces it with `new_text`.
    pub edits: Vec<EditBridgeInput>,
}

#[derive(serde::Deserialize, serde::Serialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct EditBridgeInput {
    /// The exact text to find in the file. Be minimal: include only the unique lines,
    /// or enough context to identify non-unique lines.
    pub old_text: String,
    /// The text to replace it with.
    pub new_text: String,
}

#[derive(serde::Deserialize, serde::Serialize, rmcp::schemars::JsonSchema)]
// Reject unknown fields so that a mistyped scoping parameter (e.g. `path`) fails
// loudly instead of being silently dropped, which would widen the search. This
// matches the upstream `GrepToolInput` contract.
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct GrepBridgeInput {
    /// A regex pattern to search for in the project (parsed by the Rust `regex` crate).
    pub regex: String,
    /// A glob pattern for the paths of files to include in the search
    /// (e.g. "**/*.rs"). If omitted, all files are searched.
    pub include_pattern: Option<String>,
    /// Optional starting position for paginated results (0-based).
    #[serde(default)]
    pub offset: u32,
    /// Whether the regex is case-sensitive. Defaults to false.
    #[serde(default)]
    pub case_sensitive: bool,
}

#[derive(serde::Deserialize, serde::Serialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct DiagnosticsBridgeInput {
    /// The path to get diagnostics for. If not provided, returns a project-wide summary.
    /// The first component of the path must be a root directory of the project.
    pub path: Option<String>,
}

pub fn edit_file_forward_input(input: &EditFileBridgeInput) -> Value {
    let edits: Vec<Value> = input
        .edits
        .iter()
        .map(|edit| {
            json!({
                "old_text": edit.old_text,
                "new_text": edit.new_text,
            })
        })
        .collect();
    json!({
        "path": input.path,
        "edits": edits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::io::AsyncReadExt;

    async fn spawn_fake_bridge(
        socket_dir: &std::path::Path,
        reply_text: &'static str,
        error_message: Option<&'static str>,
    ) -> tokio::sync::mpsc::Receiver<Value> {
        let socket_path = socket_dir.join("bridge.sock");
        let (request_sender, request_receiver) = tokio::sync::mpsc::channel(8);
        let _listener_guard = Arc::new(socket_path.clone());
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    let read = stream.read(&mut chunk).await.unwrap();
                    if read == 0 || chunk[..read].contains(&b'\n') {
                        buffer.extend_from_slice(&chunk[..read]);
                        if read == 0 {
                            break;
                        }
                        let line = String::from_utf8_lossy(&buffer).trim().to_string();
                        let _ = request_sender
                            .send(serde_json::from_str(&line).unwrap())
                            .await;
                        buffer.clear();
                        let response = match error_message {
                            Some(message) => json!({
                                "jsonrpc": "2.0",
                                "id": 1,
                                "error": {"code": -32000, "message": message},
                            }),
                            None => json!({
                                "jsonrpc": "2.0",
                                "id": 1,
                                "result": {
                                    "content": [{"type": "text", "text": reply_text}],
                                },
                            }),
                        };
                        use tokio::io::AsyncWriteExt;
                        stream
                            .write_all(format!("{response}\n").as_bytes())
                            .await
                            .unwrap();
                    } else {
                        buffer.extend_from_slice(&chunk[..read]);
                    }
                }
            }
        });
        request_receiver
    }

    #[tokio::test]
    async fn test_run_tool_forwards_request_and_returns_text() {
        let temp_dir = TempDir::new().unwrap();
        let mut requests = spawn_fake_bridge(temp_dir.path(), "src/lib.rs:42: match", None).await;
        let client = AgentBridgeClient::from_socket_path(temp_dir.path().join("bridge.sock"));

        let reply = client
            .run_tool(
                "grep",
                &json!({
                    "regex": "match",
                    "include_pattern": "**/*.rs",
                    "offset": 0,
                    "case_sensitive": false,
                }),
            )
            .await
            .unwrap();
        assert_eq!(reply, "src/lib.rs:42: match");

        let request = requests.recv().await.unwrap();
        assert_eq!(request["method"], "agent_tool/run");
        assert_eq!(request["params"]["tool"], "grep");
        assert_eq!(request["params"]["regex"], "match");
        assert_eq!(request["params"]["include_pattern"], "**/*.rs");
    }

    #[tokio::test]
    async fn test_run_tool_surfaces_bridge_errors_naming_the_tool() {
        let temp_dir = TempDir::new().unwrap();
        let _requests =
            spawn_fake_bridge(temp_dir.path(), "", Some("file not found: backend/main.rs")).await;
        let client = AgentBridgeClient::from_socket_path(temp_dir.path().join("bridge.sock"));

        let error = client
            .run_tool("diagnostics", &json!({"path": "backend/main.rs"}))
            .await
            .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("diagnostics"), "{message}");
        assert!(message.contains("failed in Zed"), "{message}");
        assert!(message.contains("file not found"), "{message}");
    }

    #[tokio::test]
    async fn test_run_tool_reports_unreachable_bridge_with_guidance() {
        let temp_dir = TempDir::new().unwrap();
        let client = AgentBridgeClient::from_socket_path(temp_dir.path().join("missing.sock"));
        let error = client
            .run_tool("edit_file", &json!({"path": "x", "edits": []}))
            .await
            .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("edit_file"), "{message}");
        assert!(message.contains("ZED_AGENT_BRIDGE_SOCKET"), "{message}");
    }

    #[test]
    fn test_edit_file_forward_input_matches_agent_schema_shape() {
        let input = EditFileBridgeInput {
            path: "backend/src/main.rs".to_string(),
            edits: vec![EditBridgeInput {
                old_text: "a".to_string(),
                new_text: "b".to_string(),
            }],
        };
        let forwarded = edit_file_forward_input(&input);
        assert_eq!(forwarded["path"], "backend/src/main.rs");
        assert_eq!(forwarded["edits"][0]["old_text"], "a");
        assert_eq!(forwarded["edits"][0]["new_text"], "b");
    }
}
