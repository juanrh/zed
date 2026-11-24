use edit::{EditFileOutcome, EDIT_FILE_TOOL_NAME};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};

mod edit;

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "zed-edit-file-mcp";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in BufReader::new(stdin.lock()).lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let method = request["method"].as_str().unwrap_or_default().to_string();
        let id = request.get("id").cloned();
        let result = match method.as_str() {
            "initialize" => Some(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": SERVER_NAME,
                    "version": SERVER_VERSION
                }
            })),
            "ping" => Some(json!({})),
            "tools/list" => Some(json!({
                "tools": [tool_descriptor()]
            })),
            "tools/call" => Some(handle_tools_call(&request)),
            _ => None,
        };
        let Some(id) = id else {
            continue;
        };
        let Some(result) = result else {
            let error = json!({
                "code": -32601,
                "message": format!("method not found: {method}"),
            });
            if write_response(&mut stdout, &json!({ "jsonrpc": "2.0", "id": id, "error": error })).is_err() {
                break;
            }
            continue;
        };
        if write_response(&mut stdout, &json!({ "jsonrpc": "2.0", "id": id, "result": result }))
            .is_err()
        {
            break;
        }
    }
}

fn tool_descriptor() -> Value {
    json!({
        "name": EDIT_FILE_TOOL_NAME,
        "description": "Edits files by replacing specific text with new content, using the \
                        same input/output contract as the built-in Zed agent edit_file tool. \
                        Each edit finds old_text in the file and replaces it with new_text. \
                        If no exact unique match is found, matches are attempted \
                        whitespace-insensitively. Fails when the text to replace is missing \
                        or matches more than one location.",
        "inputSchema": edit::tool_json_schema(),
    })
}

fn handle_tools_call(request: &Value) -> Value {
    let params = &request["params"];
    let Some(tool_name) = params["name"].as_str() else {
        return error_result("missing tool name");
    };
    if tool_name != EDIT_FILE_TOOL_NAME {
        return error_result(&format!("unknown tool: {tool_name}"));
    };
    let input: edit::EditFileToolInput = match serde_json::from_value(params["arguments"].clone())
    {
        Ok(input) => input,
        Err(error) => return error_result(&format!("invalid edit_file arguments: {error}")),
    };
    match edit::edit_file(&input.path, &input.edits) {
        EditFileOutcome::Success { summary } => json!({
            "content": [{ "type": "text", "text": summary }],
        }),
        EditFileOutcome::NoEdits => json!({
            "content": [{ "type": "text", "text": "No edits were made." }],
        }),
        EditFileOutcome::Failure { error } => error_result(&error),
    }
}

fn error_result(message: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true,
    })
}

fn write_response(mut stdout: impl Write, response: &Value) -> std::io::Result<()> {
    stdout
        .write_all(serde_json::to_string(response).unwrap().as_bytes())
        .and_then(|_| stdout.write_all(b"\n"))
        .and_then(|_| stdout.flush())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_descriptor_lists_edit_file() {
        let descriptor = tool_descriptor();
        assert_eq!(descriptor["name"], "edit_file");
        assert_eq!(descriptor["inputSchema"]["type"], "object");
    }

    #[test]
    fn tools_call_returns_success_text() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("file.txt");
        std::fs::write(&path, "hello\n").unwrap();
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "edit_file",
                "arguments": {
                    "path": path.display().to_string(),
                    "edits": [{ "old_text": "hello", "new_text": "goodbye" }]
                }
            }
        });
        let result = handle_tools_call(&request);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Edited"), "{text}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "goodbye\n");
    }

    #[test]
    fn tools_call_rejects_unknown_tools() {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "other_tool", "arguments": {} }
        });
        let result = handle_tools_call(&request);
        assert_eq!(result["isError"], json!(true));
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("unknown tool"));
    }

    #[test]
    fn tools_call_reports_invalid_arguments() {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "edit_file", "arguments": { "path": 42 } }
        });
        let result = handle_tools_call(&request);
        assert_eq!(result["isError"], json!(true));
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("invalid edit_file arguments"));
    }
}
