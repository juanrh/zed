mod common;

use anyhow::{Context as _, Result, bail};
use common::{FakeZed, TestProject};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

struct SpawnedServer {
    child: Child,
    stdin: Option<std::process::ChildStdin>,
    stdout: BufReader<std::process::ChildStdout>,
}

impl SpawnedServer {
    fn send(&mut self, payload: &Value) -> Result<()> {
        let mut stdin = self.stdin.as_ref().context("stdin already closed")?;
        writeln!(stdin, "{payload}")?;
        stdin.flush()?;
        Ok(())
    }

    fn read_response(&mut self, request_id: u64) -> Result<Value> {
        loop {
            let mut line = String::new();
            let bytes = self.stdout.read_line(&mut line)?;
            if bytes == 0 {
                bail!("server closed stdout before responding to request {request_id}");
            }
            let message: Value = serde_json::from_str(&line)
                .with_context(|| format!("parsing server message: {line}"))?;
            if message.get("id").and_then(Value::as_u64) == Some(request_id) {
                if let Some(error) = message.get("error") {
                    bail!("request {request_id} failed: {error}");
                }
                return Ok(message["result"].clone());
            }
        }
    }

    fn call_tool(
        &mut self,
        request_id: u64,
        tool_name: &str,
        arguments: Value,
    ) -> Result<(bool, String)> {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/call",
            "params": {"name": tool_name, "arguments": arguments},
        }))?;
        let result = self.read_response(request_id)?;
        let text = result["content"]
            .as_array()
            .map(|contents| {
                contents
                    .iter()
                    .filter_map(|content| content["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        Ok((result["isError"].as_bool().unwrap_or(false), text))
    }

    fn shutdown(&mut self) -> Result<()> {
        self.stdin.take();
        let status = self
            .child
            .wait()
            .context("waiting for the server to shut down after stdin closed")?;
        anyhow::ensure!(status.success(), "server exited with {status}");
        Ok(())
    }
}

#[test]
#[allow(clippy::disallowed_methods)]
fn test_binary_serves_full_review_round_trip_over_stdio() {
    let project = TestProject::new();
    let fake_zed = FakeZed::new();
    let session_directory = tempfile::TempDir::new().unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_zed-review-mcp"))
        .current_dir(&project.root)
        .env("ZED_PATH", &fake_zed.script_path)
        .env("ZED_REVIEW_MCP_SESSION_DIR", session_directory.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawning the zed-review-mcp binary");
    let stdin = child.stdin.take().unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let mut server = SpawnedServer {
        child,
        stdin: Some(stdin),
        stdout,
    };

    server
        .send(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "e2e-test", "version": "0.0.0"},
            },
        }))
        .unwrap();
    let initialize_result = server.read_response(1).unwrap();
    assert_eq!(initialize_result["serverInfo"]["name"], "zed-review-mcp");
    server
        .send(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {},
        }))
        .unwrap();

    let code_path = project.write_relative("src/main.py", "value = 1\n");
    let (is_error, reply) = server
        .call_tool(
            2,
            "record_baseline",
            json!({"paths": [code_path.to_string_lossy()]}),
        )
        .unwrap();
    assert!(!is_error, "{reply}");
    assert!(reply.contains("recorded"), "{reply}");

    let (is_error, reply) = server
        .call_tool(
            3,
            "edit",
            json!({
                "file_path": code_path.to_string_lossy(),
                "old_string": "value = 1",
                "new_string": "value = 2",
            }),
        )
        .unwrap();
    assert!(!is_error, "{reply}");
    assert_eq!(std::fs::read_to_string(&code_path).unwrap(), "value = 2\n");

    let unrecorded_path = project.write_relative("src/other.py", "flag = false\n");
    let (is_error, reply) = server
        .call_tool(
            4,
            "edit",
            json!({
                "file_path": unrecorded_path.to_string_lossy(),
                "old_string": "flag = false",
                "new_string": "flag = true",
            }),
        )
        .unwrap();
    assert!(!is_error, "{reply}");
    assert!(reply.contains("baseline registered"), "{reply}");

    let (is_error, reply) = server.call_tool(5, "get_change_status", json!({})).unwrap();
    assert!(!is_error, "{reply}");
    assert!(reply.contains("src/main.py: modified"), "{reply}");
    assert!(reply.contains("src/other.py: modified"), "{reply}");

    let (is_error, reply) = server.call_tool(6, "open_review", json!({})).unwrap();
    assert!(!is_error, "{reply}");
    fake_zed.wait_for_invocation();
    let invocations = fake_zed.recorded_invocations();
    assert_eq!(invocations.len(), 1, "{invocations:?}");
    assert!(
        invocations[0].starts_with("--diff ")
            && invocations[0].ends_with(project.root.to_string_lossy().as_ref()),
        "{invocations:?}"
    );

    let (is_error, reply) = server
        .call_tool(
            7,
            "reject",
            json!({"paths": [code_path.to_string_lossy(), unrecorded_path.to_string_lossy()]}),
        )
        .unwrap();
    assert!(!is_error, "{reply}");
    assert_eq!(
        std::fs::read_to_string(&code_path).unwrap(),
        "value = 1\n",
        "reject must restore the pre-change content"
    );
    assert_eq!(
        std::fs::read_to_string(&unrecorded_path).unwrap(),
        "flag = false\n",
        "reject must restore the auto-baseline content"
    );

    server.shutdown().unwrap();
    assert!(
        session_directory.path().join("trusted-root").exists(),
        "explicit session directories must survive shutdown"
    );
}

#[test]
#[allow(clippy::disallowed_methods)]
fn test_binary_rejects_unknown_arguments() {
    let output = Command::new(env!("CARGO_BIN_EXE_zed-review-mcp"))
        .arg("--nonsense")
        .output()
        .expect("spawning the binary with a bad flag");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unexpected argument"), "{stderr}");
}
