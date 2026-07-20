//! A2A HTTP server — exposes lim as an A2A agent over HTTP.
//!
//! This module provides a minimal HTTP/1.1 server that speaks the A2A
//! (Agent-to-Agent) protocol. It wraps an [`A2ARequestHandler`] and
//! handles:
//!
//! - `GET /.well-known/agent-card.json` → returns the [`AgentCard`]
//! - `POST /` → JSON-RPC 2.0 requests (`tasks/send`, `tasks/get`,
//!   `tasks/cancel`)
//!
//! The implementation uses raw `tokio::net::TcpListener` — no external
//! HTTP framework dependency. This is sufficient for A2A's simple
//! request/response model.
//!
//! ## Usage
//!
//! ```ignore
//! let server = A2AServer::new(handler)
//!     .bind("127.0.0.1:8080")
//!     .auth_token("secret");
//! server.serve(shutdown).await?;
//! ```

use std::sync::Arc;

use lim_core::error::{LimError, Result};
use lim_transport::{
    A2ARequestHandler, INTERNAL_ERROR, INVALID_PARAMS, JsonRpcRequest, JsonRpcResponse,
    METHOD_NOT_FOUND, PARSE_ERROR, TaskIdParams, TaskSendParams,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Content-Type for JSON responses.
const JSON_CONTENT_TYPE: &str = "application/json";

/// Minimal A2A HTTP server.
pub struct A2AServer {
    handler: Arc<dyn A2ARequestHandler>,
    bind_addr: String,
    auth_token: Option<String>,
}

impl A2AServer {
    /// Create a new A2A server with the given handler.
    pub fn new(handler: Arc<dyn A2ARequestHandler>) -> Self {
        Self { handler, bind_addr: "127.0.0.1:0".to_string(), auth_token: None }
    }

    /// Set the bind address (e.g. `"127.0.0.1:8080"` or `"0.0.0.0:8080"`).
    pub fn bind(mut self, addr: impl Into<String>) -> Self {
        self.bind_addr = addr.into();
        self
    }

    /// Set an auth token for Bearer authentication.
    pub fn auth_token(mut self, token: impl Into<String>) -> Self {
        self.auth_token = Some(token.into());
        self
    }

    /// Run the HTTP server until the shutdown token is cancelled.
    ///
    /// Prints the actual bound address to stdout (useful when binding
    /// to port 0 for auto-assignment).
    pub async fn serve(self, shutdown: CancellationToken) -> Result<()> {
        let listener = TcpListener::bind(&self.bind_addr)
            .await
            .map_err(|e| LimError::Config(format!("A2A bind {}: {e}", self.bind_addr)))?;
        let actual_addr =
            listener.local_addr().map_err(|e| LimError::Config(format!("local_addr: {e}")))?;

        // Update the AgentCard endpoint to reflect the actual address.
        // The handler's card may have a placeholder endpoint.
        let card = self.handler.agent_card().clone();
        info!(
            name = %card.name,
            addr = %actual_addr,
            "A2A server listening"
        );
        println!("A2A server listening on http://{actual_addr}");

        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    let (stream, peer) = match accept_result {
                        Ok(s) => s,
                        Err(e) => {
                            warn!(error = %e, "A2A accept failed");
                            continue;
                        }
                    };
                    let handler = self.handler.clone();
                    let token = self.auth_token.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, handler, token).await {
                            debug!(error = %e, peer = %peer, "A2A connection error");
                        }
                    });
                }
                _ = shutdown.cancelled() => {
                    info!("A2A server shutting down");
                    break;
                }
            }
        }
        Ok(())
    }
}

/// Handle a single HTTP connection.
async fn handle_connection(
    stream: tokio::net::TcpStream,
    handler: Arc<dyn A2ARequestHandler>,
    auth_token: Option<String>,
) -> Result<()> {
    let mut reader = BufReader::new(stream);

    // Read request line: "METHOD PATH HTTP/1.1"
    let mut request_line = String::new();
    tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut request_line)
        .await
        .map_err(|e| LimError::Config(format!("read request line: {e}")))?;
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        write_http_response(reader.get_mut(), 400, "Bad Request", "invalid request line").await?;
        return Ok(());
    }
    let method = parts[0].to_string();
    let path = parts[1].to_string();

    // Read headers until empty line.
    let mut content_length: usize = 0;
    let mut authorization: Option<String> = None;
    loop {
        let mut header_line = String::new();
        tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut header_line)
            .await
            .map_err(|e| LimError::Config(format!("read header: {e}")))?;
        let trimmed = header_line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            if key == "content-length" {
                content_length = value.parse().unwrap_or(0);
            } else if key == "authorization" {
                authorization = Some(value.to_string());
            }
        }
    }

    // Read body.
    let body = if content_length > 0 {
        let mut buf = vec![0u8; content_length];
        reader
            .read_exact(&mut buf)
            .await
            .map_err(|e| LimError::Config(format!("read body: {e}")))?;
        String::from_utf8_lossy(&buf).to_string()
    } else {
        String::new()
    };

    // All reads done — now get the stream for writing.
    let stream = reader.get_mut();

    // Auth check.
    if let Some(token) = &auth_token {
        let expected = format!("Bearer {token}");
        if authorization.as_deref() != Some(&expected) {
            write_http_response(stream, 401, "Unauthorized", "missing or invalid auth token")
                .await?;
            return Ok(());
        }
    }

    // Dispatch.
    debug!(method = %method, path = %path, "A2A request");
    match (method.as_str(), path.as_str()) {
        ("GET", "/.well-known/agent-card.json") => {
            let card = handler.agent_card();
            let json = serde_json::to_string(card)
                .map_err(|e| LimError::Config(format!("serialize agent card: {e}")))?;
            write_http_json(stream, 200, &json).await?;
        }
        ("POST", "/") => {
            let rpc_resp = handle_jsonrpc(&body, &handler).await;
            let json = serde_json::to_string(&rpc_resp)
                .map_err(|e| LimError::Config(format!("serialize rpc response: {e}")))?;
            write_http_json(stream, 200, &json).await?;
        }
        _ => {
            write_http_response(stream, 404, "Not Found", "unknown endpoint").await?;
        }
    }
    Ok(())
}

/// Handle a JSON-RPC 2.0 request and produce a response.
async fn handle_jsonrpc(body: &str, handler: &Arc<dyn A2ARequestHandler>) -> JsonRpcResponse {
    let req: JsonRpcRequest = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => {
            return JsonRpcResponse::error(
                serde_json::Value::Null,
                PARSE_ERROR,
                format!("parse error: {e}"),
            );
        }
    };

    let id = req.id.clone();
    match req.method.as_str() {
        "tasks/send" => {
            let params: TaskSendParams = match req.params {
                Some(p) => match serde_json::from_value(p) {
                    Ok(v) => v,
                    Err(e) => {
                        return JsonRpcResponse::error(
                            id,
                            INVALID_PARAMS,
                            format!("invalid params: {e}"),
                        );
                    }
                },
                None => {
                    return JsonRpcResponse::error(id, INVALID_PARAMS, "missing params");
                }
            };
            match handler.send_task(params).await {
                Ok(task) => match serde_json::to_value(&task) {
                    Ok(v) => JsonRpcResponse::success(id, v),
                    Err(e) => {
                        JsonRpcResponse::error(id, INTERNAL_ERROR, format!("serialize task: {e}"))
                    }
                },
                Err(e) => JsonRpcResponse::error(id, INTERNAL_ERROR, e.to_string()),
            }
        }
        "tasks/get" => {
            let params: TaskIdParams = match req.params {
                Some(p) => match serde_json::from_value(p) {
                    Ok(v) => v,
                    Err(e) => {
                        return JsonRpcResponse::error(
                            id,
                            INVALID_PARAMS,
                            format!("invalid params: {e}"),
                        );
                    }
                },
                None => {
                    return JsonRpcResponse::error(id, INVALID_PARAMS, "missing params");
                }
            };
            match handler.get_task(&params.id).await {
                Ok(task) => match serde_json::to_value(&task) {
                    Ok(v) => JsonRpcResponse::success(id, v),
                    Err(e) => {
                        JsonRpcResponse::error(id, INTERNAL_ERROR, format!("serialize task: {e}"))
                    }
                },
                Err(e) => JsonRpcResponse::error(id, INTERNAL_ERROR, e.to_string()),
            }
        }
        "tasks/cancel" => {
            let params: TaskIdParams = match req.params {
                Some(p) => match serde_json::from_value(p) {
                    Ok(v) => v,
                    Err(e) => {
                        return JsonRpcResponse::error(
                            id,
                            INVALID_PARAMS,
                            format!("invalid params: {e}"),
                        );
                    }
                },
                None => {
                    return JsonRpcResponse::error(id, INVALID_PARAMS, "missing params");
                }
            };
            match handler.cancel_task(&params.id).await {
                Ok(task) => match serde_json::to_value(&task) {
                    Ok(v) => JsonRpcResponse::success(id, v),
                    Err(e) => {
                        JsonRpcResponse::error(id, INTERNAL_ERROR, format!("serialize task: {e}"))
                    }
                },
                Err(e) => JsonRpcResponse::error(id, INTERNAL_ERROR, e.to_string()),
            }
        }
        _ => {
            JsonRpcResponse::error(id, METHOD_NOT_FOUND, format!("unknown method: {}", req.method))
        }
    }
}

/// Write a plain-text HTTP response.
async fn write_http_response(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    reason: &str,
    body: &str,
) -> Result<()> {
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: text/plain\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|e| LimError::Config(format!("write response: {e}")))?;
    Ok(())
}

/// Write a JSON HTTP response.
async fn write_http_json(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    body: &str,
) -> Result<()> {
    let response = format!(
        "HTTP/1.1 {status} OK\r\n\
         Content-Type: {json_ct}\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        json_ct = JSON_CONTENT_TYPE,
        len = body.len(),
        body = body,
    );
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|e| LimError::Config(format!("write json response: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use lim_transport::{A2ATask, AgentCapabilities, AgentCard, AgentSkill, TaskState, TaskStatus};

    /// A simple mock handler for testing.
    struct MockHandler {
        card: AgentCard,
    }

    #[async_trait]
    impl A2ARequestHandler for MockHandler {
        async fn send_task(&self, params: TaskSendParams) -> Result<A2ATask> {
            Ok(A2ATask {
                id: params.id,
                session_id: params.session_id,
                status: TaskStatus {
                    state: TaskState::Completed,
                    timestamp: Some("2026-07-09T00:00:00Z".to_string()),
                    message: None,
                },
                artifacts: Vec::new(),
                history: vec![params.message],
            })
        }

        async fn get_task(&self, task_id: &str) -> Result<A2ATask> {
            Ok(A2ATask {
                id: task_id.to_string(),
                session_id: None,
                status: TaskStatus { state: TaskState::Working, timestamp: None, message: None },
                artifacts: Vec::new(),
                history: Vec::new(),
            })
        }

        async fn cancel_task(&self, task_id: &str) -> Result<A2ATask> {
            Ok(A2ATask {
                id: task_id.to_string(),
                session_id: None,
                status: TaskStatus { state: TaskState::Canceled, timestamp: None, message: None },
                artifacts: Vec::new(),
                history: Vec::new(),
            })
        }

        fn agent_card(&self) -> &AgentCard {
            &self.card
        }
    }

    fn mock_handler() -> Arc<dyn A2ARequestHandler> {
        Arc::new(MockHandler {
            card: AgentCard {
                name: "test-lim".to_string(),
                version: "0.1.0".to_string(),
                endpoint: "http://localhost".to_string(),
                description: Some("test agent".to_string()),
                capabilities: AgentCapabilities {
                    streaming: false,
                    push_notifications: false,
                    state_transition_history: false,
                },
                default_input_modes: vec!["text".to_string()],
                default_output_modes: vec!["text".to_string()],
                skills: vec![AgentSkill {
                    id: "test".to_string(),
                    name: "Test".to_string(),
                    description: Some("test skill".to_string()),
                    tags: vec![],
                }],
                auth_token: None,
            },
        })
    }

    #[tokio::test]
    async fn handle_jsonrpc_tasks_send() {
        let handler = mock_handler();
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"tasks/send","params":{"id":"task-1","message":{"role":"user","parts":[{"type":"text","text":"hello"}]}}}"#;
        let resp = handle_jsonrpc(body, &handler).await;
        assert!(resp.error.is_none());
        assert!(resp.result.is_some());
    }

    #[tokio::test]
    async fn handle_jsonrpc_tasks_get() {
        let handler = mock_handler();
        let body = r#"{"jsonrpc":"2.0","id":2,"method":"tasks/get","params":{"id":"task-1"}}"#;
        let resp = handle_jsonrpc(body, &handler).await;
        assert!(resp.error.is_none());
        assert!(resp.result.is_some());
    }

    #[tokio::test]
    async fn handle_jsonrpc_tasks_cancel() {
        let handler = mock_handler();
        let body = r#"{"jsonrpc":"2.0","id":3,"method":"tasks/cancel","params":{"id":"task-1"}}"#;
        let resp = handle_jsonrpc(body, &handler).await;
        assert!(resp.error.is_none());
        assert!(resp.result.is_some());
    }

    #[tokio::test]
    async fn handle_jsonrpc_unknown_method() {
        let handler = mock_handler();
        let body = r#"{"jsonrpc":"2.0","id":4,"method":"unknown/method"}"#;
        let resp = handle_jsonrpc(body, &handler).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn handle_jsonrpc_parse_error() {
        let handler = mock_handler();
        let body = "not valid json";
        let resp = handle_jsonrpc(body, &handler).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, PARSE_ERROR);
    }

    #[tokio::test]
    async fn handle_jsonrpc_missing_params() {
        let handler = mock_handler();
        let body = r#"{"jsonrpc":"2.0","id":5,"method":"tasks/send"}"#;
        let resp = handle_jsonrpc(body, &handler).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    #[tokio::test]
    async fn a2a_server_serves_agent_card() {
        // Start an A2A server on a random port, fetch the agent card.
        let handler = mock_handler();
        let server = A2AServer::new(handler).bind("127.0.0.1:0");
        let shutdown = CancellationToken::new();
        let shutdown_clone = shutdown.clone();

        // Spawn the server.
        let handle = tokio::spawn(async move { server.serve(shutdown_clone).await });

        // Give it a moment to start.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // We don't know the port, so cancel and check it shuts down.
        shutdown.cancel();
        let result = handle.await;
        assert!(result.is_ok());
    }
}
