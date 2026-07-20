//! MCP Client mode — call external MCP tool servers from lim.
//!
//! `McpClient` connects to an external MCP server (over stdio), performs
//! the initialize handshake, lists available tools, and calls them.
//!
//! `McpTool` wraps a single MCP tool as lim's `Tool` trait, so the
//! `AgentLoop` can invoke MCP tools exactly like built-in tools (read,
//! write, shell, etc.).
//!
//! ## Architecture
//!
//! ```text
//! AgentLoop ──▶ ToolRegistry ──▶ McpTool (impl Tool)
//!                                    │
//!                                    ▼
//!                                 McpClient (JSON-RPC over stdio)
//!                                    │
//!                                    ▼
//!                              External MCP Server process
//! ```

use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lim_agent::tool::{Tool, ToolFuture};
use lim_core::error::{LimError, Result};
use lim_core::id::ToolCallId;
use lim_core::tool::{ToolCategory, ToolResult, ToolSchema};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::mcp_protocol::*;

/// Connection to an external MCP server.
///
/// Manages the subprocess lifecycle and JSON-RPC communication over
/// stdio. All methods take `&mut self` — wrap in `Arc<tokio::sync::Mutex<_>>`
/// for shared access (as `McpTool` does).
pub struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: AtomicU64,
    server_info: Option<McpServerInfo>,
}

impl McpClient {
    /// Spawn an MCP server process and return a client connected to it.
    ///
    /// The server should communicate via stdin/stdout using newline-delimited
    /// JSON-RPC 2.0 (the standard MCP stdio transport).
    pub async fn spawn(command: &str, args: &[&str]) -> Result<Self> {
        let mut child = tokio::process::Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| LimError::Config(format!("spawn MCP server '{command}': {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| LimError::Config("MCP server stdin not available".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| LimError::Config("MCP server stdout not available".to_string()))?;

        debug!(command, "MCP server spawned");

        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: AtomicU64::new(1),
            server_info: None,
        })
    }

    /// Create a client from explicit streams (for testing).
    #[cfg(test)]
    pub fn from_streams(stdin: ChildStdin, stdout: ChildStdout) -> Self {
        Self {
            child: unsafe { std::mem::zeroed() }, // placeholder, not used in tests
            stdin,
            stdout: BufReader::new(stdout),
            next_id: AtomicU64::new(1),
            server_info: None,
        }
    }

    /// Perform the MCP initialize handshake.
    ///
    /// Sends `initialize` request, then `notifications/initialized`.
    pub async fn initialize(&mut self) -> Result<McpServerInfo> {
        let id = self.next_id();
        let params = McpInitializeParams {
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
            capabilities: serde_json::json!({}),
            client_info: McpClientInfo {
                name: "lim".to_string(),
                version: lim_core::LIM_VERSION.to_string(),
            },
        };

        let req = JsonRpcRequest::new(id.clone(), "initialize")
            .with_params(serde_json::to_value(&params).unwrap());

        let response = self.send_request(req).await?;

        let init_result: McpInitializeResult =
            serde_json::from_value(response.result.ok_or_else(|| {
                LimError::Config("initialize response missing result".to_string())
            })?)
            .map_err(|e| LimError::Config(format!("parse initialize result: {e}")))?;

        // Send initialized notification
        let notif = JsonRpcNotification::new("notifications/initialized");
        self.send_notification(notif).await?;

        self.server_info = Some(init_result.server_info.clone());
        debug!(name = %init_result.server_info.name, "MCP server initialized");
        Ok(init_result.server_info)
    }

    /// List available tools from the MCP server.
    pub async fn list_tools(&mut self) -> Result<Vec<McpToolDef>> {
        let id = self.next_id();
        let req = JsonRpcRequest::new(id, "tools/list");
        let response = self.send_request(req).await?;

        let result: McpToolListResult =
            serde_json::from_value(response.result.ok_or_else(|| {
                LimError::Config("tools/list response missing result".to_string())
            })?)
            .map_err(|e| LimError::Config(format!("parse tools/list result: {e}")))?;

        debug!(count = result.tools.len(), "MCP tools listed");
        Ok(result.tools)
    }

    /// Call a tool on the MCP server.
    pub async fn call_tool(
        &mut self,
        name: &str,
        arguments: &serde_json::Value,
    ) -> Result<McpCallToolResult> {
        let id = self.next_id();
        let params =
            McpCallToolParams { name: name.to_string(), arguments: Some(arguments.clone()) };
        let req = JsonRpcRequest::new(id, "tools/call")
            .with_params(serde_json::to_value(&params).unwrap());

        let response = self.send_request(req).await?;

        if let Some(err) = response.error {
            return Err(LimError::Config(format!(
                "MCP tools/call error [{}]: {}",
                err.code, err.message
            )));
        }

        let result: McpCallToolResult =
            serde_json::from_value(response.result.ok_or_else(|| {
                LimError::Config("tools/call response missing result".to_string())
            })?)
            .map_err(|e| LimError::Config(format!("parse tools/call result: {e}")))?;

        debug!(tool = name, is_error = result.is_error, "MCP tool called");
        Ok(result)
    }

    /// Shut down the MCP server process.
    pub async fn shutdown(&mut self) -> Result<()> {
        // Try graceful shutdown via JSON-RPC, then kill
        let _ = self.send_request(JsonRpcRequest::new(self.next_id(), "shutdown")).await;
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        debug!("MCP server shut down");
        Ok(())
    }

    /// Server info from the initialize handshake (if completed).
    pub fn server_info(&self) -> Option<&McpServerInfo> {
        self.server_info.as_ref()
    }

    // ── Internal JSON-RPC plumbing ──────────────────────────────────

    fn next_id(&self) -> serde_json::Value {
        serde_json::Value::from(self.next_id.fetch_add(1, Ordering::SeqCst))
    }

    async fn send_request(&mut self, req: JsonRpcRequest) -> Result<JsonRpcResponse> {
        let json = serde_json::to_string(&req)
            .map_err(|e| LimError::Config(format!("serialize request: {e}")))?;
        self.stdin
            .write_all(json.as_bytes())
            .await
            .map_err(|e| LimError::Config(format!("write to MCP stdin: {e}")))?;
        self.stdin
            .write_all(b"\n")
            .await
            .map_err(|e| LimError::Config(format!("write newline: {e}")))?;
        self.stdin.flush().await.map_err(|e| LimError::Config(format!("flush MCP stdin: {e}")))?;

        self.read_response().await
    }

    async fn send_notification(&mut self, notif: JsonRpcNotification) -> Result<()> {
        let json = serde_json::to_string(&notif)
            .map_err(|e| LimError::Config(format!("serialize notification: {e}")))?;
        self.stdin
            .write_all(json.as_bytes())
            .await
            .map_err(|e| LimError::Config(format!("write notification: {e}")))?;
        self.stdin
            .write_all(b"\n")
            .await
            .map_err(|e| LimError::Config(format!("write newline: {e}")))?;
        self.stdin.flush().await.map_err(|e| LimError::Config(format!("flush: {e}")))?;
        Ok(())
    }

    async fn read_response(&mut self) -> Result<JsonRpcResponse> {
        let mut line = String::new();
        loop {
            line.clear();
            let n = self
                .stdout
                .read_line(&mut line)
                .await
                .map_err(|e| LimError::Config(format!("read from MCP stdout: {e}")))?;
            if n == 0 {
                return Err(LimError::Config("MCP server closed stdout".to_string()));
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Try to parse as response (skip notifications)
            match serde_json::from_str::<JsonRpcMessage>(trimmed) {
                Ok(JsonRpcMessage::Response(resp)) => return Ok(resp),
                Ok(JsonRpcMessage::Notification(notif)) => {
                    debug!(method = %notif.method, "MCP notification received (ignored)");
                    continue;
                }
                Ok(JsonRpcMessage::Request(_)) => {
                    // Server-to-client request (not supported yet)
                    continue;
                }
                Err(e) => {
                    warn!(line = trimmed, error = %e, "failed to parse MCP message");
                    continue;
                }
            }
        }
    }
}

/// A single MCP tool wrapped as lim's `Tool` trait.
///
/// Each `McpTool` shares an `Arc<tokio::sync::Mutex<McpClient>>` with
/// other tools from the same MCP server. Calls are serialized through
/// the mutex — this is acceptable because most MCP servers are
/// single-threaded.
pub struct McpTool {
    client: Arc<tokio::sync::Mutex<McpClient>>,
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

impl McpTool {
    pub fn new(client: Arc<tokio::sync::Mutex<McpClient>>, def: &McpToolDef) -> Self {
        Self {
            client,
            name: def.name.clone(),
            description: def.description.clone().unwrap_or_default(),
            input_schema: def.input_schema.clone().unwrap_or(serde_json::json!({"type": "object"})),
        }
    }
}

impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(&self.name, &self.description, self.input_schema.clone())
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Remote
    }

    fn execute(
        &self,
        tool_call_id: ToolCallId,
        args: serde_json::Value,
        _cancel: CancellationToken,
    ) -> ToolFuture<Result<ToolResult>> {
        let client = Arc::clone(&self.client);
        let name = self.name.clone();
        Box::pin(async move {
            let mut client = client.lock().await;
            let result = client.call_tool(&name, &args).await?;

            let output = result.text_output();
            if result.is_error {
                Ok(ToolResult::error(tool_call_id, output))
            } else {
                Ok(ToolResult::success(tool_call_id, output))
            }
        })
    }
}

/// Manages an MCP server connection and its registered tools.
///
/// Spawns the server, performs handshake, lists tools, and provides
/// the tools for registration into a `ToolRegistry`.
pub struct McpToolProxy {
    client: Arc<tokio::sync::Mutex<McpClient>>,
    tools: Vec<McpToolDef>,
    server_info: McpServerInfo,
}

impl McpToolProxy {
    /// Connect to an MCP server, perform handshake, and list tools.
    pub async fn connect(command: &str, args: &[&str]) -> Result<Self> {
        let mut client = McpClient::spawn(command, args).await?;
        let server_info = client.initialize().await?;
        let tools = client.list_tools().await?;

        Ok(Self { client: Arc::new(tokio::sync::Mutex::new(client)), tools, server_info })
    }

    /// List the tool definitions from this MCP server.
    pub fn tools(&self) -> &[McpToolDef] {
        &self.tools
    }

    /// Server identity.
    pub fn server_info(&self) -> &McpServerInfo {
        &self.server_info
    }

    /// Create `McpTool` wrappers for all tools from this server.
    ///
    /// Each tool shares the same `Arc<Mutex<McpClient>>`.
    pub fn create_tools(&self) -> Vec<McpTool> {
        self.tools.iter().map(|def| McpTool::new(Arc::clone(&self.client), def)).collect()
    }

    /// Register all tools from this server into a `ToolRegistry`.
    pub fn register_into(&self, registry: &lim_agent::ToolRegistry) -> Result<()> {
        for tool in self.create_tools() {
            registry.register(tool)?;
        }
        Ok(())
    }

    /// Shut down the MCP server.
    pub async fn shutdown(&self) -> Result<()> {
        let mut client = self.client.lock().await;
        client.shutdown().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test using a shell script that simulates an MCP server.
    /// Echoes back tool calls as results.
    #[cfg(unix)]
    #[tokio::test]
    async fn mcp_client_full_flow() {
        // A minimal MCP server shell script
        let script = r#"#!/bin/bash
# Read initialize request
read -r line
echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"test-server","version":"1.0"}}}'
# Read initialized notification
read -r line
# Read tools/list request
read -r line
echo '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echo back","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}}]}}'
# Read tools/call request
read -r line
echo '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"echoed: hello"}],"isError":false}}'
"#;

        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("mcp_server.sh");
        std::fs::write(&script_path, script).unwrap();
        std::fs::set_permissions(&script_path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        let mut client = McpClient::spawn("bash", &[script_path.to_str().unwrap()]).await.unwrap();

        let server_info = client.initialize().await.unwrap();
        assert_eq!(server_info.name, "test-server");

        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");

        let result = client.call_tool("echo", &serde_json::json!({"text": "hello"})).await.unwrap();
        assert!(!result.is_error);
        assert_eq!(result.text_output(), "echoed: hello");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn mcp_tool_proxy_register() {
        let script = r#"#!/bin/bash
read -r line
echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"proxy-test","version":"1.0"}}}'
read -r line
read -r line
echo '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"calc","description":"Calculator","inputSchema":{"type":"object"}}]}}'
"#;

        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("mcp_proxy.sh");
        std::fs::write(&script_path, script).unwrap();
        std::fs::set_permissions(&script_path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        let proxy = McpToolProxy::connect("bash", &[script_path.to_str().unwrap()]).await.unwrap();

        assert_eq!(proxy.server_info().name, "proxy-test");
        assert_eq!(proxy.tools().len(), 1);

        let tools = proxy.create_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "calc");
        assert_eq!(tools[0].category(), ToolCategory::Remote);
    }
}
