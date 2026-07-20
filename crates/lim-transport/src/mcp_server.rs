//! MCP Server mode — expose lim's capabilities as MCP tools.
//!
//! `McpServer` speaks the MCP protocol over stdio (newline-delimited
//! JSON-RPC 2.0) on the *server* side. An external MCP client (e.g.
//! Claude Code, Cursor) connects to lim by spawning it as a subprocess
//! and sending initialize / tools/list / tools/call requests.
//!
//! The actual tool implementations are provided by an [`McpToolHandler`],
//! which lives in the app layer (e.g. lim-serve's `DaemonMcpHandler`
//! routes calls to `IpcTransport::send(DaemonCommand)`). lim-transport
//! itself stays free of DaemonCommand semantics.
//!
//! ## Architecture
//!
//! ```text
//! External MCP Client (Claude Code / Cursor)
//!     │
//!     │ JSON-RPC over stdio
//!     ▼
//! McpServer (this module) ──▶ McpToolHandler (trait)
//!                                  │
//!                                  ▼
//!                          app-layer impl
//!                          (e.g. DaemonMcpHandler → IpcTransport → DaemonCommand)
//! ```
//!
//! See `docs/architecture/transport-layer.md` §7.1.

use std::sync::Arc;

use async_trait::async_trait;
use lim_core::error::{LimError, Result};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tracing::{debug, info, warn};

use crate::mcp_protocol::*;

/// Handler that provides tool definitions and dispatches tool calls.
///
/// Implementations live in the app layer. lim-transport provides a
/// [`StaticToolHandler`] for testing; lim-serve provides a
/// `DaemonMcpHandler` that routes calls to `IpcTransport::send`.
#[async_trait]
pub trait McpToolHandler: Send + Sync {
    /// Return the tools this server exposes.
    fn list_tools(&self) -> Vec<McpToolDef>;

    /// Invoke a tool by name. `args` is the JSON arguments object
    /// (or `Value::Null` if the client omitted arguments).
    ///
    /// On error, implementations should return an `McpCallToolResult`
    /// with `is_error: true` rather than an `Err`, reserving `Err` for
    /// transport-level failures. `McpServer` will still convert `Err`
    /// into an `is_error` result as a fallback.
    async fn call_tool(&self, name: &str, args: serde_json::Value) -> Result<McpCallToolResult>;
}

/// Blanket impl so `Arc<dyn McpToolHandler>` can be used as `H` in
/// [`McpServer<H>`]. This enables type-erased handler injection from
/// the app layer (lim-serve) without knowing the concrete type.
#[async_trait]
impl<T: McpToolHandler + ?Sized> McpToolHandler for Arc<T> {
    fn list_tools(&self) -> Vec<McpToolDef> {
        (**self).list_tools()
    }

    async fn call_tool(&self, name: &str, args: serde_json::Value) -> Result<McpCallToolResult> {
        (**self).call_tool(name, args).await
    }
}

/// MCP server speaking JSON-RPC 2.0 over stdio.
///
/// Generic over the handler `H` so callers can inject their own
/// implementation without dynamic dispatch. For a type-erased variant
/// use `McpServer<Arc<dyn McpToolHandler>>` (the trait is object-safe).
///
/// ## Example
///
/// ```ignore
/// let handler = DaemonMcpHandler::new(ipc_transport);
/// McpServer::new(handler).serve().await?;
/// ```
pub struct McpServer<H: McpToolHandler + 'static> {
    handler: Arc<H>,
    server_info: McpServerInfo,
    protocol_version: String,
}

impl<H: McpToolHandler + 'static> McpServer<H> {
    /// Create a server with the given handler and default identity
    /// (`name = "lim"`, version from `CARGO_PKG_VERSION`).
    pub fn new(handler: H) -> Self {
        Self::with_server_info(handler, default_server_info())
    }

    /// Create a server with a custom identity.
    pub fn with_server_info(handler: H, server_info: McpServerInfo) -> Self {
        Self {
            handler: Arc::new(handler),
            server_info,
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
        }
    }

    /// Run the MCP server loop on tokio's stdin/stdout until stdin
    /// closes, a shutdown request is handled, or an `exit` notification
    /// is received.
    pub async fn serve(self) -> Result<()> {
        self.serve_with_io(tokio::io::stdin(), tokio::io::stdout()).await
    }

    /// Run the server loop on explicit I/O streams (for testing or
    /// non-stdio transports like SSE / socket).
    pub async fn serve_with_io<R, W>(self, stdin: R, stdout: W) -> Result<()>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        info!(server = %self.server_info.name, version = %self.server_info.version, "MCP server starting");
        let mut reader = BufReader::new(stdin);
        let mut writer = stdout;
        let mut line = String::new();

        loop {
            line.clear();
            let n = reader
                .read_line(&mut line)
                .await
                .map_err(|e| LimError::Config(format!("mcp stdin read: {e}")))?;
            if n == 0 {
                debug!("stdin closed, exiting MCP server");
                break;
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let msg: JsonRpcMessage = match serde_json::from_str(trimmed) {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, line = trimmed, "unparseable JSON-RPC line, ignoring");
                    continue;
                }
            };

            let (response, should_stop) = self.handle_message(msg).await;
            if let Some(resp) = response {
                let json = serde_json::to_string(&resp)
                    .map_err(|e| LimError::Config(format!("mcp response serialize: {e}")))?;
                writer
                    .write_all(json.as_bytes())
                    .await
                    .map_err(|e| LimError::Config(format!("mcp stdout write: {e}")))?;
                writer
                    .write_all(b"\n")
                    .await
                    .map_err(|e| LimError::Config(format!("mcp stdout write: {e}")))?;
                writer
                    .flush()
                    .await
                    .map_err(|e| LimError::Config(format!("mcp stdout flush: {e}")))?;
            }

            if should_stop {
                debug!("shutdown requested, exiting MCP server");
                break;
            }
        }

        info!("MCP server stopped");
        Ok(())
    }

    /// Dispatch one JSON-RPC message.
    ///
    /// Returns `(optional response, should_stop)`. Notifications produce
    /// no response; the `exit` notification and the `shutdown` request
    /// set `should_stop = true`.
    async fn handle_message(&self, msg: JsonRpcMessage) -> (Option<JsonRpcResponse>, bool) {
        match msg {
            JsonRpcMessage::Request(req) => {
                let id = req.id.clone();
                let (resp, stop) = self.handle_request(req).await;
                (
                    Some(
                        resp.unwrap_or_else(|| {
                            JsonRpcResponse::success(id, serde_json::Value::Null)
                        }),
                    ),
                    stop,
                )
            }
            JsonRpcMessage::Notification(notif) => {
                if notif.method == "exit" {
                    debug!("exit notification received");
                    return (None, true);
                }
                // notifications/initialized, notifications/cancelled, etc.
                debug!(method = %notif.method, "notification received (no response)");
                (None, false)
            }
            JsonRpcMessage::Response(_) => {
                warn!("unexpected JSON-RPC response on server stdin, ignoring");
                (None, false)
            }
        }
    }

    /// Handle a JSON-RPC request. Returns `(optional response, should_stop)`.
    ///
    /// A `None` response means "no response written" (used internally
    /// for notifications, but requests always get a response — callers
    /// wrap `None` in a default success).
    async fn handle_request(&self, req: JsonRpcRequest) -> (Option<JsonRpcResponse>, bool) {
        let id = req.id.clone();
        match req.method.as_str() {
            "initialize" => (Some(self.handle_initialize(id).await), false),
            "ping" => (Some(JsonRpcResponse::success(id, serde_json::json!({}))), false),
            "tools/list" => (Some(self.handle_tools_list(id).await), false),
            "tools/call" => (Some(self.handle_tools_call(id, req.params).await), false),
            "shutdown" => {
                debug!("shutdown request received");
                // Per MCP spec: respond with empty result, then exit on
                // the subsequent `exit` notification. We also stop here
                // to support clients that don't send `exit`. An empty
                // object is used (rather than `Value::Null`) because
                // serde deserializes `"result":null` into `None`, which
                // would make the response indistinguishable from a
                // missing result field.
                (Some(JsonRpcResponse::success(id, serde_json::json!({}))), true)
            }
            other => {
                warn!(method = other, "method not found");
                (
                    Some(JsonRpcResponse::error(
                        id,
                        METHOD_NOT_FOUND,
                        format!("method not found: {other}"),
                    )),
                    false,
                )
            }
        }
    }

    async fn handle_initialize(&self, id: serde_json::Value) -> JsonRpcResponse {
        let result = McpInitializeResult {
            protocol_version: self.protocol_version.clone(),
            capabilities: serde_json::json!({
                "tools": { "listChanged": false }
            }),
            server_info: self.server_info.clone(),
        };
        let value = serde_json::to_value(&result)
            .unwrap_or_else(|_| serde_json::json!({"protocolVersion": self.protocol_version}));
        JsonRpcResponse::success(id, value)
    }

    async fn handle_tools_list(&self, id: serde_json::Value) -> JsonRpcResponse {
        let tools = self.handler.list_tools();
        let result = McpToolListResult { tools };
        let value =
            serde_json::to_value(&result).unwrap_or_else(|_| serde_json::json!({"tools": []}));
        JsonRpcResponse::success(id, value)
    }

    async fn handle_tools_call(
        &self,
        id: serde_json::Value,
        params: Option<serde_json::Value>,
    ) -> JsonRpcResponse {
        let params: McpCallToolParams = match params {
            Some(p) => match serde_json::from_value(p) {
                Ok(parsed) => parsed,
                Err(e) => {
                    return JsonRpcResponse::error(
                        id,
                        INVALID_PARAMS,
                        format!("invalid tools/call params: {e}"),
                    );
                }
            },
            None => {
                return JsonRpcResponse::error(id, INVALID_PARAMS, "tools/call requires params");
            }
        };

        let args = params.arguments.unwrap_or(serde_json::Value::Null);
        match self.handler.call_tool(&params.name, args).await {
            Ok(result) => {
                let value = serde_json::to_value(&result)
                    .unwrap_or_else(|_| serde_json::json!({"content": [], "isError": true}));
                JsonRpcResponse::success(id, value)
            }
            Err(e) => {
                // Transport-level failure — surface as an MCP error result
                // (is_error: true) per spec, not a JSON-RPC error.
                warn!(tool = %params.name, error = %e, "tool call failed");
                let err_result = McpCallToolResult {
                    content: vec![McpContent::Text {
                        text: format!("tool `{}` failed: {e}", params.name),
                    }],
                    is_error: true,
                };
                let value = serde_json::to_value(&err_result).unwrap_or_else(|_| {
                    serde_json::json!({"content":[{"type":"text","text":"tool failed"}],"isError":true})
                });
                JsonRpcResponse::success(id, value)
            }
        }
    }
}

fn default_server_info() -> McpServerInfo {
    McpServerInfo { name: "lim".to_string(), version: env!("CARGO_PKG_VERSION").to_string() }
}

/// A simple handler with a static tool list and a closure-based dispatcher.
///
/// Primarily for tests and demos. Real deployments use their own
/// `McpToolHandler` implementation.
pub struct StaticToolHandler {
    tools: Vec<McpToolDef>,
    dispatcher: ToolDispatcher,
}

/// Closure-based tool dispatcher used by [`StaticToolHandler`].
type ToolDispatcher =
    Box<dyn Fn(&str, serde_json::Value) -> Result<McpCallToolResult> + Send + Sync>;

impl StaticToolHandler {
    /// Create an empty handler that exposes no tools.
    pub fn empty() -> Self {
        Self {
            tools: Vec::new(),
            dispatcher: Box::new(|_name, _args| {
                Err(LimError::Config("no tools registered".to_string()))
            }),
        }
    }

    /// Create a handler with the given tools and a dispatcher closure.
    ///
    /// The dispatcher receives `(tool_name, arguments)` and returns
    /// either a `McpCallToolResult` or an error.
    pub fn with_dispatcher<F>(tools: Vec<McpToolDef>, dispatcher: F) -> Self
    where
        F: Fn(&str, serde_json::Value) -> Result<McpCallToolResult> + Send + Sync + 'static,
    {
        Self { tools, dispatcher: Box::new(dispatcher) }
    }
}

#[async_trait]
impl McpToolHandler for StaticToolHandler {
    fn list_tools(&self) -> Vec<McpToolDef> {
        self.tools.clone()
    }

    async fn call_tool(&self, name: &str, args: serde_json::Value) -> Result<McpCallToolResult> {
        (self.dispatcher)(name, args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo_handler() -> StaticToolHandler {
        let tools = vec![McpToolDef {
            name: "echo".to_string(),
            description: Some("Echo back the input text".to_string()),
            input_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                },
                "required": ["text"]
            })),
        }];
        StaticToolHandler::with_dispatcher(tools, |name, args| {
            if name != "echo" {
                return Ok(McpCallToolResult {
                    content: vec![McpContent::Text { text: format!("unknown tool: {name}") }],
                    is_error: true,
                });
            }
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("<empty>");
            Ok(McpCallToolResult {
                content: vec![McpContent::Text { text: format!("echoed: {text}") }],
                is_error: false,
            })
        })
    }

    /// Drive the server with in-memory pipes: write requests to the
    /// input, read responses from the output.
    ///
    /// Uses two separate duplex streams (one for client→server input,
    /// one for server→client output) to avoid the single-duplex deadlock
    /// where writing all input up front fills the buffer before the
    /// server reads any.
    async fn run_session(handler: StaticToolHandler, input: &str) -> String {
        use tokio::io::{AsyncReadExt, duplex};

        let (mut client_write, server_read) = duplex(8 * 1024);
        let (server_write, mut client_read) = duplex(8 * 1024);

        // Write the full input up front (lines separated by \n).
        client_write.write_all(input.as_bytes()).await.unwrap();
        client_write.flush().await.unwrap();
        // Close the input side so the server sees EOF after consuming
        // all lines.
        drop(client_write);

        let server = McpServer::new(handler);
        let handle =
            tokio::spawn(async move { server.serve_with_io(server_read, server_write).await });

        let mut output = String::new();
        client_read.read_to_string(&mut output).await.ok();
        handle.await.unwrap().unwrap();
        output
    }

    #[tokio::test]
    async fn initialize_handshake() {
        let input = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}
"#;
        let output = run_session(echo_handler(), input).await;
        let resp: JsonRpcResponse = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(resp.id, serde_json::Value::from(1));
        let result: McpInitializeResult = serde_json::from_value(resp.result.unwrap()).unwrap();
        assert_eq!(result.server_info.name, "lim");
        assert_eq!(result.protocol_version, MCP_PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn tools_list_returns_handler_tools() {
        let input = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}
"#;
        let output = run_session(echo_handler(), input).await;
        let resp: JsonRpcResponse = serde_json::from_str(output.trim()).unwrap();
        let result: McpToolListResult = serde_json::from_value(resp.result.unwrap()).unwrap();
        assert_eq!(result.tools.len(), 1);
        assert_eq!(result.tools[0].name, "echo");
    }

    #[tokio::test]
    async fn tools_call_dispatches_to_handler() {
        let input = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"echo","arguments":{"text":"hello"}}}
"#;
        let output = run_session(echo_handler(), input).await;
        let resp: JsonRpcResponse = serde_json::from_str(output.trim()).unwrap();
        let result: McpCallToolResult = serde_json::from_value(resp.result.unwrap()).unwrap();
        assert!(!result.is_error);
        assert_eq!(result.text_output(), "echoed: hello");
    }

    #[tokio::test]
    async fn unknown_method_returns_error() {
        let input = r#"{"jsonrpc":"2.0","id":1,"method":"nope"}
"#;
        let output = run_session(echo_handler(), input).await;
        let resp: JsonRpcResponse = serde_json::from_str(output.trim()).unwrap();
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn shutdown_request_stops_server() {
        let input = r#"{"jsonrpc":"2.0","id":1,"method":"shutdown"}
"#;
        let output = run_session(StaticToolHandler::empty(), input).await;
        let resp: JsonRpcResponse = serde_json::from_str(output.trim()).unwrap();
        // shutdown responds with an empty object result and stops the server.
        assert_eq!(resp.result, Some(serde_json::json!({})));
    }

    #[tokio::test]
    async fn exit_notification_stops_server_without_response() {
        // Send an exit notification — should produce no output and stop.
        let input = r#"{"jsonrpc":"2.0","method":"exit"}
"#;
        let output = run_session(StaticToolHandler::empty(), input).await;
        assert!(output.trim().is_empty(), "exit should not produce a response, got: {output}");
    }

    #[tokio::test]
    async fn multiple_requests_in_sequence() {
        let input = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"echo","arguments":{"text":"a"}}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"echo","arguments":{"text":"b"}}}
"#;
        let output = run_session(echo_handler(), input).await;
        let lines: Vec<&str> = output.trim().lines().collect();
        assert_eq!(lines.len(), 3);
        let r1: JsonRpcResponse = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(r1.id, serde_json::Value::from(1));
        let r3: JsonRpcResponse = serde_json::from_str(lines[2]).unwrap();
        let res: McpCallToolResult = serde_json::from_value(r3.result.unwrap()).unwrap();
        assert_eq!(res.text_output(), "echoed: b");
    }

    #[tokio::test]
    async fn ping_responds_with_empty_object() {
        let input = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}
"#;
        let output = run_session(StaticToolHandler::empty(), input).await;
        let resp: JsonRpcResponse = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(resp.result, Some(serde_json::json!({})));
    }
}
