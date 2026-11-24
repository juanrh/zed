//! Minimal MCP protocol layer: newline-delimited JSON-RPC 2.0 frames for the
//! subset of MCP this server implements (`initialize`, `notifications/initialized`,
//! `ping`, `tools/list`, `tools/call`, `notifications/cancelled`).
//!
//! This module is pure: it parses and builds frames, and knows nothing about
//! sockets or the editor. Callers feed frames in and get actions to perform;
//! they execute the actions and send back the built response frames.

use serde_json::{Value, json};

pub const PROTOCOL_VERSION: &str = "2024-11-05";
pub const SERVER_NAME: &str = "zed-editor-mcp";

/// The MCP method subset this server understands.
pub const SUPPORTED_PROTOCOL_VERSION: &str = "2024-11-05";

/// A tool advertised by `tools/list`.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// The outcome of a `tools/call`, as reported to the MCP client.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCallOutcome {
    pub text: String,
    pub is_error: bool,
}

impl ToolCallOutcome {
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }
}

/// One decoded frame off the wire.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    /// A request that must be answered with exactly one response frame.
    Request {
        id: Value,
        action: Action,
    },
    /// A notification: no response frame is expected.
    Notification(Notification),
    /// A frame that is valid JSON-RPC but targets an unsupported method.
    ///
    /// Callers answer these with a protocol error; `UnknownMethod` keeps the
    /// original method name for the message.
    UnknownMethod { id: Option<Value>, method: String },
    /// Not valid JSON, or not a JSON-RPC message at all.
    Invalid(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Initialize { requested_protocol_version: String },
    Ping,
    ListTools,
    CallTool { name: String, arguments: Value },
    /// A tools/call that couldn't even name a tool: the dispatcher answers it
    /// with a `tools/call` result error, like the built-in agent answers bad
    /// tool calls.
    CallToolError { invalid_request: String },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Notification {
    Initialized,
    Cancelled { request_id: Value },
}

pub fn parse_message(frame: &str) -> Message {
    let trimmed = frame.trim();
    if trimmed.is_empty() {
        return Message::Invalid("empty frame".to_string());
    }
    let value: Value = match serde_json::from_str(trimmed) {
        Ok(value) => value,
        Err(error) => return Message::Invalid(format!("malformed JSON frame: {error}")),
    };
    let method = value["method"].as_str().map(str::to_string);
    let Some(method) = method else {
        // Responses from clients (unexpected here) still need a reply so
        // clients don't block waiting for one on a matching id.
        return match value.get("id") {
            Some(id) => Message::UnknownMethod {
                id: Some(id.clone()),
                method: "<no method>".to_string(),
            },
            None => Message::Invalid("frame has no method".to_string()),
        };
    };
    let id = value.get("id").cloned();
    let notification = id.is_none();
    let params = &value["params"];
    match method.as_str() {
        "initialize" => match id {
            Some(id) => Message::Request {
                id,
                action: Action::Initialize {
                    requested_protocol_version: params["protocolVersion"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                },
            },
            None => Message::Invalid("initialize is not a notification".to_string()),
        },
        "ping" => match id {
            Some(id) => Message::Request {
                id,
                action: Action::Ping,
            },
            None => Message::Invalid("ping is not a notification".to_string()),
        },
        "tools/list" => match id {
            Some(id) => Message::Request {
                id,
                action: Action::ListTools,
            },
            None => Message::Invalid("tools/list is not a notification".to_string()),
        },
        "tools/call" => match id {
            Some(id) => {
                let name = params["name"].as_str().unwrap_or_default().to_string();
                if name.is_empty() {
                    return Message::Request {
                        id,
                        action: Action::CallToolError {
                            invalid_request: "tools/call is missing the tool name".to_string(),
                        },
                    };
                }
                Message::Request {
                    id,
                    action: Action::CallTool {
                        name,
                        arguments: params["arguments"].clone(),
                    },
                }
            }
            None => Message::Invalid("tools/call is not a notification".to_string()),
        },
        "notifications/initialized" => {
            if notification {
                Message::Notification(Notification::Initialized)
            } else {
                Message::Invalid("notifications/initialized must not carry an id".to_string())
            }
        }
        "notifications/cancelled" => {
            if notification {
                Message::Notification(Notification::Cancelled {
                    request_id: params["requestId"].clone(),
                })
            } else {
                Message::Invalid("notifications/cancelled must not carry an id".to_string())
            }
        }
        _ => Message::UnknownMethod {
            id,
            method,
        },
    }
}

impl Action {
    /// Only these actions can be answered without consulting the editor,
    /// so a static `InvalidCall` keeps the protocol layer total.
    /// (See `Message::Request` construction for the one error shape.)
    pub fn describe(&self) -> &'static str {
        match self {
            Self::Initialize { .. } => "initialize",
            Self::Ping => "ping",
            Self::ListTools => "tools/list",
            Self::CallTool { .. } => "tools/call",
            Self::CallToolError { .. } => "tools/call",
        }
    }
}

pub fn initialize_result(id: &Value, requested_protocol_version: &str) -> String {
    // Answer with the client's protocol version when we support it, else
    // with our own; MCP clients tolerate a server-side downgrade.
    let protocol_version = if requested_protocol_version == SUPPORTED_PROTOCOL_VERSION {
        requested_protocol_version
    } else {
        SUPPORTED_PROTOCOL_VERSION
    };
    result_response(
        id,
        &json!({
            "protocolVersion": protocol_version,
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": SERVER_NAME,
                "version": env!("CARGO_PKG_VERSION"),
            }
        }),
    )
}

pub fn ping_result(id: &Value) -> String {
    result_response(id, &json!({}))
}

pub fn tools_list_result(id: &Value, tools: &[ToolDescriptor]) -> String {
    result_response(
        id,
        &json!({
            "tools": tools.iter().map(|tool| json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": tool.input_schema,
            })).collect::<Vec<_>>()
        }),
    )
}

pub fn tools_call_result(id: &Value, outcome: &ToolCallOutcome) -> String {
    let mut result = json!({
        "content": [{ "type": "text", "text": outcome.text }],
    });
    if outcome.is_error {
        result["isError"] = json!(true);
    }
    result_response(id, &result)
}

pub fn method_not_found_response(id: &Value, method: &str) -> String {
    error_response(id, -32601, &format!("method not found: {method}"))
}

pub fn invalid_frame_response() -> Option<String> {
    Some(error_response(
        &Value::Null,
        -32700,
        "malformed JSON-RPC frame; one frame of JSON per line, ending in a newline, is expected",
    ))
}

fn result_response(id: &Value, result: &Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

fn error_response(id: &Value, code: i64, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": if id.is_null() { Value::Null } else { id.clone() },
        "error": { "code": code, "message": message }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(json: Value) -> Message {
        parse_message(&json.to_string())
    }

    #[test]
    fn parses_initialize() {
        let message = parse(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": { "name": "t" } }
        }));
        match message {
            Message::Request { id, action } => {
                assert_eq!(id, json!(1));
                assert_eq!(
                    action,
                    Action::Initialize {
                        requested_protocol_version: "2025-03-26".to_string()
                    }
                );
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn initialize_result_prefers_client_protocol_when_supported() {
        let response = initialize_result(&json!(7), SUPPORTED_PROTOCOL_VERSION);
        let parsed: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed["id"], json!(7));
        assert_eq!(parsed["result"]["protocolVersion"], SUPPORTED_PROTOCOL_VERSION);
        assert_eq!(parsed["result"]["capabilities"]["tools"], json!({}));

        let response = initialize_result(&json!(7), "1999-99-99");
        let parsed: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed["result"]["protocolVersion"], SUPPORTED_PROTOCOL_VERSION);
    }

    #[test]
    fn parses_tools_call_with_arguments() {
        let message = parse(json!({
            "jsonrpc": "2.0",
            "id": "abc",
            "method": "tools/call",
            "params": {
                "name": "edit_file",
                "arguments": { "path": "root/file.txt", "edits": [] }
            }
        }));
        match message {
            Message::Request { id, action } => {
                assert_eq!(id, json!("abc"));
                match action {
                    Action::CallTool { name, arguments } => {
                        assert_eq!(name, "edit_file");
                        assert_eq!(arguments["path"], "root/file.txt");
                    }
                    other => panic!("unexpected action: {other:?}"),
                }
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn tools_call_result_marks_errors() {
        let ok = tools_call_result(&json!(2), &ToolCallOutcome::ok("Edited x"));
        assert!(ok.contains("\"text\":\"Edited x\""));
        assert!(!ok.contains("\"isError\""));

        let err = tools_call_result(&json!(2), &ToolCallOutcome::error("no match"));
        assert!(err.contains("\"isError\":true"));
    }

    #[test]
    fn parses_notifications() {
        assert_eq!(
            parse(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })),
            Message::Notification(Notification::Initialized)
        );
        assert_eq!(
            parse(json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": { "requestId": 42 }
            })),
            Message::Notification(Notification::Cancelled {
                request_id: json!(42)
            })
        );
    }

    #[test]
    fn unknown_method_keeps_id_and_name() {
        assert_eq!(
            parse(json!({ "jsonrpc": "2.0", "id": 3, "method": "resources/list" })),
            Message::UnknownMethod {
                id: Some(json!(3)),
                method: "resources/list".to_string(),
            }
        );
        let response = method_not_found_response(&json!(3), "resources/list");
        assert!(response.contains("-32601"));
        assert!(response.contains("resources/list"));
    }

    #[test]
    fn invalid_frames_report_why() {
        assert!(matches!(
            parse_message("this is not json"),
            Message::Invalid(_)
        ));
        assert!(matches!(parse_message(""), Message::Invalid(_)));
        // A non-string method can't be routed; the frame still carries an
        // id, so the server answers it with a protocol error instead of
        // leaving the client waiting.
        assert_eq!(
            parse_message("{\"jsonrpc\": \"2.0\", \"id\": 1, \"method\": 42}"),
            Message::UnknownMethod {
                id: Some(json!(1)),
                method: "<no method>".to_string(),
            }
        );
        assert!(matches!(
            parse(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {} })),
            Message::Request { action: Action::CallToolError { .. }, .. }
        ));
    }

    #[test]
    fn tools_list_result_serializes_descriptors() {
        let response = tools_list_result(
            &json!(1),
            &[ToolDescriptor {
                name: "edit_file".to_string(),
                description: "Edits files".to_string(),
                input_schema: json!({"type": "object"}),
            }],
        );
        assert!(response.contains("\"name\":\"edit_file\""));
        assert!(response.contains("\"inputSchema\""));
        assert!(response.contains("\"description\":\"Edits files\""));
    }
}
