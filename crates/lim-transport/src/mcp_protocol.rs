//! MCP (Model Context Protocol) message types.
//!
//! MCP uses JSON-RPC 2.0 over stdio (newline-delimited JSON) or SSE.
//! This module defines the protocol types shared by both the MCP Server
//! mode (lim as server) and MCP Client mode (lim calling external tools).
//!
//! See https://spec.modelcontextprotocol.io/ for the spec.

use serde::{Deserialize, Serialize};

// ── JSON-RPC 2.0 envelope ───────────────────────────────────────────

/// JSON-RPC 2.0 request (has `id`, expects response).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// JSON-RPC 2.0 response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// JSON-RPC 2.0 notification (no `id`, no response expected).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// Any JSON-RPC message (request, response, or notification).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcMessage {
    Request(JsonRpcRequest),
    Response(JsonRpcResponse),
    Notification(JsonRpcNotification),
}

impl JsonRpcRequest {
    pub fn new(id: impl Into<serde_json::Value>, method: impl Into<String>) -> Self {
        Self { jsonrpc: "2.0".to_string(), id: id.into(), method: method.into(), params: None }
    }

    pub fn with_params(mut self, params: serde_json::Value) -> Self {
        self.params = Some(params);
        self
    }
}

impl JsonRpcResponse {
    pub fn success(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self { jsonrpc: "2.0".to_string(), id, result: Some(result), error: None }
    }

    pub fn error(id: serde_json::Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError { code, message: message.into(), data: None }),
        }
    }
}

impl JsonRpcNotification {
    pub fn new(method: impl Into<String>) -> Self {
        Self { jsonrpc: "2.0".to_string(), method: method.into(), params: None }
    }
}

// ── Standard JSON-RPC error codes ───────────────────────────────────

pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;

// ── MCP-specific types ──────────────────────────────────────────────

/// MCP tool definition (from `tools/list` response).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "inputSchema", skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
}

/// MCP `tools/list` response params.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolListResult {
    pub tools: Vec<McpToolDef>,
}

/// MCP `tools/call` request params.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpCallToolParams {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<serde_json::Value>,
}

/// MCP `tools/call` response result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpCallToolResult {
    pub content: Vec<McpContent>,
    #[serde(rename = "isError", default)]
    pub is_error: bool,
}

/// MCP content block (text, image, or resource reference).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum McpContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    #[serde(rename = "resource")]
    Resource { resource: McpResourceRef },
}

/// MCP resource reference (inside content blocks).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResourceRef {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl McpCallToolResult {
    /// Extract all text content concatenated into a single string.
    pub fn text_output(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| match c {
                McpContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ── MCP initialize handshake ────────────────────────────────────────

/// MCP `initialize` request params.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpInitializeParams {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: serde_json::Value,
    #[serde(rename = "clientInfo")]
    pub client_info: McpClientInfo,
}

/// MCP `initialize` response result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpInitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: serde_json::Value,
    #[serde(rename = "serverInfo")]
    pub server_info: McpServerInfo,
}

/// Client identity (sent during initialize).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpClientInfo {
    pub name: String,
    pub version: String,
}

/// Server identity (returned during initialize).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    pub version: String,
}

/// MCP protocol version supported by lim.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_request() {
        let req = JsonRpcRequest::new(1, "tools/list");
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"method\":\"tools/list\""));
        assert!(json.contains("\"id\":1"));
    }

    #[test]
    fn deserialize_response() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, serde_json::Value::from(1));
        assert!(resp.result.is_some());
    }

    #[test]
    fn mcp_tool_list_result_parse() {
        let json = r#"{"tools":[{"name":"read","description":"Read a file","inputSchema":{"type":"object"}}]}"#;
        let result: McpToolListResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.tools.len(), 1);
        assert_eq!(result.tools[0].name, "read");
    }

    #[test]
    fn mcp_call_result_text_extraction() {
        let result = McpCallToolResult {
            content: vec![
                McpContent::Text { text: "line1".to_string() },
                McpContent::Text { text: "line2".to_string() },
            ],
            is_error: false,
        };
        assert_eq!(result.text_output(), "line1\nline2");
    }

    #[test]
    fn mcp_call_result_with_error() {
        let result = McpCallToolResult {
            content: vec![McpContent::Text { text: "Tool failed".to_string() }],
            is_error: true,
        };
        assert!(result.is_error);
        assert_eq!(result.text_output(), "Tool failed");
    }

    #[test]
    fn jsonrpc_message_untagged() {
        let req_json = r#"{"jsonrpc":"2.0","id":1,"method":"test","params":{}}"#;
        let msg: JsonRpcMessage = serde_json::from_str(req_json).unwrap();
        assert!(matches!(msg, JsonRpcMessage::Request(_)));

        let notif_json = r#"{"jsonrpc":"2.0","method":"notify","params":{}}"#;
        let msg: JsonRpcMessage = serde_json::from_str(notif_json).unwrap();
        assert!(matches!(msg, JsonRpcMessage::Notification(_)));
    }

    #[test]
    fn error_response_construction() {
        let resp =
            JsonRpcResponse::error(serde_json::Value::from(1), METHOD_NOT_FOUND, "no such method");
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("-32601"));
    }
}
