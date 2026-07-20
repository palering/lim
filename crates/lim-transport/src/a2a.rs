//! A2A (Agent-to-Agent) protocol — HTTP-based agent interop.
//!
//! A2A is a JSON-RPC 2.0 over HTTP protocol for agent-to-agent
//! communication. It defines three roles:
//!
//! - **Client** — initiates tasks (editors, CLIs, other agents)
//! - **Agent** — processes tasks (lim in server mode)
//! - **Agent ↔ Agent** — peer-to-peer interop
//!
//! This module provides the **client side**: types for AgentCard / Task
//! discovery and an [`A2AHttpTransport`] that calls remote A2A agents.
//! The server side (receiving A2A requests) lives in `lim-serve`, which
//! wires an HTTP server to an [`A2ARequestHandler`].
//!
//! ## Protocol summary
//!
//! 1. Client fetches `/.well-known/agent-card.json` to discover the
//!    agent's endpoint, capabilities, and skills.
//! 2. Client POSTs a JSON-RPC `tasks/send` request with a user message.
//! 3. Agent processes the task and responds with a `Task` object
//!    containing status, history, and artifacts.
//! 4. For long-running tasks, the agent may return `working` status;
//!    the client polls `tasks/get` or subscribes via SSE
//!    (`tasks/sendSubscribe`).
//!
//! See `docs/architecture/transport-layer.md` §6 for the design.

use std::sync::Arc;

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use lim_core::content::ContentBlock;
use lim_core::error::{LimError, Result};
use lim_core::message::{AgentMessage, Role};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::mcp_protocol::{JsonRpcRequest, JsonRpcResponse};
use crate::protocol::ProtocolAdapter;

// ── AgentCard ───────────────────────────────────────────────────────

/// Agent discovery card, served at `/.well-known/agent-card.json`.
///
/// Describes an agent's endpoint, capabilities, and skills so clients
/// can decide whether and how to invoke it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCard {
    pub name: String,
    pub version: String,
    /// Base URL for JSON-RPC requests (e.g. `https://lim.example.com`).
    /// The actual endpoint is `{url}/` (POST JSON-RPC).
    #[serde(rename = "url")]
    pub endpoint: String,
    #[serde(default)]
    pub description: Option<String>,
    pub capabilities: AgentCapabilities,
    #[serde(rename = "defaultInputModes", default)]
    pub default_input_modes: Vec<String>,
    #[serde(rename = "defaultOutputModes", default)]
    pub default_output_modes: Vec<String>,
    #[serde(default)]
    pub skills: Vec<AgentSkill>,
    /// Bearer token expected in `Authorization` header (optional).
    /// Production deployments should use mTLS or OAuth instead.
    #[serde(rename = "authToken", default, skip_serializing)]
    pub auth_token: Option<String>,
}

/// Capabilities advertised by an A2A agent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentCapabilities {
    #[serde(default)]
    pub streaming: bool,
    #[serde(rename = "pushNotifications", default)]
    pub push_notifications: bool,
    #[serde(rename = "stateTransitionHistory", default)]
    pub state_transition_history: bool,
}

/// A skill advertised in an AgentCard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSkill {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

// ── Task types ──────────────────────────────────────────────────────

/// A2A task state machine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    Submitted,
    Working,
    InputRequired,
    Completed,
    Canceled,
    Failed,
}

/// Status of a task at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStatus {
    pub state: TaskState,
    #[serde(rename = "timestamp", default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// Human-readable status message (e.g. error detail).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// A2A Message (distinct from lim's `AgentMessage`).
///
/// A2A messages have a role (`user` or `agent`) and a list of parts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2AMessage {
    pub role: String,
    pub parts: Vec<A2APart>,
    #[serde(rename = "messageId", default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

/// One part of an A2A message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum A2APart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "file")]
    File {
        #[serde(rename = "mimeType")]
        mime_type: String,
        /// Base64-encoded bytes (when inline) or a URI reference.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uri: Option<String>,
    },
    #[serde(rename = "data")]
    Data { data: serde_json::Value },
}

/// An artifact produced by a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    #[serde(rename = "artifactId")]
    pub artifact_id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub parts: Vec<A2APart>,
}

/// A2A task — the central unit of work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2ATask {
    pub id: String,
    #[serde(rename = "sessionId", default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub status: TaskStatus,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub history: Vec<A2AMessage>,
}

/// Parameters for `tasks/send`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSendParams {
    pub id: String,
    pub message: A2AMessage,
    #[serde(rename = "sessionId", default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Parameters for `tasks/get` / `tasks/cancel`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskIdParams {
    pub id: String,
}

// ── A2AAdapter ──────────────────────────────────────────────────────

/// Protocol adapter that converts between lim's `AgentMessage` and
/// A2A's `Message` JSON format.
///
/// Used by [`A2AHttpTransport`] to map lim messages onto A2A tasks.
/// Task-level wrapping (TaskSendParams, A2ATask) is handled by the
/// transport, not the adapter — the adapter only converts messages.
pub struct A2AAdapter;

impl A2AAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for A2AAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProtocolAdapter for A2AAdapter {
    async fn encode(&self, msg: &AgentMessage) -> Result<Vec<u8>> {
        let a2a_msg = agent_message_to_a2a(msg);
        serde_json::to_vec(&a2a_msg).map_err(|e| LimError::Config(format!("a2a encode: {e}")))
    }

    async fn decode(&self, data: &[u8]) -> Result<AgentMessage> {
        let a2a_msg: A2AMessage = serde_json::from_slice(data)
            .map_err(|e| LimError::Config(format!("a2a decode: {e}")))?;
        a2a_to_agent_message(&a2a_msg)
    }

    fn name(&self) -> &str {
        "a2a"
    }
}

/// Convert a lim `AgentMessage` to an A2A `Message`.
///
/// - `Role::User` → `role: "user"`
/// - `Role::Assistant` → `role: "agent"`
/// - `Role::System` → `role: "user"` (A2A has no system role)
/// - `Role::Tool` → skipped (tool results stay internal to lim)
///
/// Text blocks become `TextPart`; other block types are serialized as
/// `DataPart` (preserving their JSON form for round-tripping).
pub fn agent_message_to_a2a(msg: &AgentMessage) -> A2AMessage {
    let role = match msg.role {
        Role::User | Role::System => "user",
        Role::Assistant => "agent",
        Role::Tool => "user", // tool results fed back as user context
    };
    let parts: Vec<A2APart> = msg
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(A2APart::Text { text: text.clone() }),
            ContentBlock::Thinking { .. } => None, // internal, not sent
            ContentBlock::ToolCall { name, arguments, .. } => Some(A2APart::Data {
                data: serde_json::json!({
                    "type": "tool_call",
                    "name": name,
                    "arguments": arguments,
                }),
            }),
            ContentBlock::Image { data, mime_type } => {
                let encoded = STANDARD.encode(data);
                Some(A2APart::File { mime_type: mime_type.clone(), data: Some(encoded), uri: None })
            }
            ContentBlock::ArtifactRef { artifact_id, label } => Some(A2APart::Data {
                data: serde_json::json!({
                    "type": "artifact_ref",
                    "artifact_id": artifact_id.to_string(),
                    "label": label,
                }),
            }),
        })
        .collect();

    A2AMessage { role: role.to_string(), parts, message_id: Some(msg.id.to_string()) }
}

/// Convert an A2A `Message` back to a lim `AgentMessage`.
pub fn a2a_to_agent_message(a2a: &A2AMessage) -> Result<AgentMessage> {
    let role = match a2a.role.as_str() {
        "user" => Role::User,
        "agent" => Role::Assistant,
        other => {
            warn!(role = other, "unknown A2A role, defaulting to User");
            Role::User
        }
    };

    let mut content = Vec::new();
    for part in &a2a.parts {
        match part {
            A2APart::Text { text } => content.push(ContentBlock::Text { text: text.clone() }),
            A2APart::File { mime_type, data, uri } => {
                if let Some(b64) = data {
                    let bytes = STANDARD
                        .decode(b64)
                        .map_err(|e| LimError::Config(format!("a2a file decode: {e}")))?;
                    content.push(ContentBlock::Image { data: bytes, mime_type: mime_type.clone() });
                } else if let Some(_uri) = uri {
                    // URI references not supported yet — skip with a warning
                    warn!(uri = %_uri, "A2A file URI references not supported, skipping");
                }
            }
            A2APart::Data { data } => {
                // Reconstruct as a Text block containing the JSON, so the
                // message isn't lost. Future: map back to ToolCall/etc.
                let json = serde_json::to_string(data).unwrap_or_else(|_| "{}".to_string());
                content.push(ContentBlock::Text { text: format!("[data: {json}]") });
            }
        }
    }

    if content.is_empty() {
        content.push(ContentBlock::Text { text: String::new() });
    }

    // Construct with the mapped role. AgentMessage's helper constructors
    // are role-specific (user/assistant/system), so we use the struct
    // literal to preserve the role from the A2A message.
    Ok(AgentMessage {
        id: lim_core::id::MessageId::new(),
        role,
        content,
        tool_call_ids: Vec::new(),
        is_error: false,
        parent_id: None,
    })
}

// ── A2AHttpTransport (client) ───────────────────────────────────────

/// HTTP client for calling remote A2A agents.
///
/// Holds a `reqwest::Client` and an agent endpoint URL. Each call
/// POSTs a JSON-RPC 2.0 request and parses the response.
///
/// ## Auth
///
/// If `auth_token` is set, it's sent as `Authorization: Bearer <token>`.
pub struct A2AHttpTransport {
    http: reqwest::Client,
    endpoint: String,
    auth_token: Option<String>,
    adapter: Arc<A2AAdapter>,
    /// Inbound message queue for `AgentTransport::recv`.
    /// Populated by `send` (which synchronously waits for the task
    /// response and enqueues the agent's reply).
    inbox_tx: mpsc::Sender<AgentMessage>,
    inbox_rx: tokio::sync::Mutex<mpsc::Receiver<AgentMessage>>,
}

impl A2AHttpTransport {
    /// Connect to an A2A agent at the given endpoint URL.
    ///
    /// `endpoint` should be the base URL (e.g. `https://lim.example.com`);
    /// JSON-RPC requests are POSTed to `{endpoint}/`.
    pub async fn connect(endpoint: &str, auth_token: Option<&str>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| LimError::Config(format!("reqwest build: {e}")))?;

        let (inbox_tx, inbox_rx) = mpsc::channel(32);

        Ok(Self {
            http,
            endpoint: endpoint.trim_end_matches('/').to_string(),
            auth_token: auth_token.map(|s| s.to_string()),
            adapter: Arc::new(A2AAdapter::new()),
            inbox_tx,
            inbox_rx: tokio::sync::Mutex::new(inbox_rx),
        })
    }

    /// Fetch the agent's `AgentCard` from `/.well-known/agent-card.json`.
    pub async fn fetch_agent_card(&self) -> Result<AgentCard> {
        let url = format!("{}/.well-known/agent-card.json", self.endpoint);
        debug!(url = %url, "fetching agent card");
        let mut req = self.http.get(&url);
        if let Some(token) = &self.auth_token {
            req = req.bearer_auth(token);
        }
        let resp =
            req.send().await.map_err(|e| LimError::Config(format!("agent card fetch: {e}")))?;
        if !resp.status().is_success() {
            return Err(LimError::Config(format!(
                "agent card fetch failed: HTTP {}",
                resp.status()
            )));
        }
        resp.json::<AgentCard>()
            .await
            .map_err(|e| LimError::Config(format!("agent card parse: {e}")))
    }

    /// Send a task (user message) to the agent and wait for the response.
    ///
    /// Returns the resulting `A2ATask` (which may be completed or still
    /// working, depending on the agent).
    pub async fn send_task(&self, params: TaskSendParams) -> Result<A2ATask> {
        let req = JsonRpcRequest::new(serde_json::Value::from(1), "tasks/send")
            .with_params(serde_json::to_value(&params).unwrap());
        self.rpc_call(&req).await.and_then(|v| {
            serde_json::from_value::<A2ATask>(v)
                .map_err(|e| LimError::Config(format!("parse task: {e}")))
        })
    }

    /// Get the current state of a task.
    pub async fn get_task(&self, task_id: &str) -> Result<A2ATask> {
        let params = TaskIdParams { id: task_id.to_string() };
        let req = JsonRpcRequest::new(serde_json::Value::from(1), "tasks/get")
            .with_params(serde_json::to_value(&params).unwrap());
        self.rpc_call(&req).await.and_then(|v| {
            serde_json::from_value::<A2ATask>(v)
                .map_err(|e| LimError::Config(format!("parse task: {e}")))
        })
    }

    /// Cancel a task.
    pub async fn cancel_task(&self, task_id: &str) -> Result<A2ATask> {
        let params = TaskIdParams { id: task_id.to_string() };
        let req = JsonRpcRequest::new(serde_json::Value::from(1), "tasks/cancel")
            .with_params(serde_json::to_value(&params).unwrap());
        self.rpc_call(&req).await.and_then(|v| {
            serde_json::from_value::<A2ATask>(v)
                .map_err(|e| LimError::Config(format!("parse task: {e}")))
        })
    }

    /// Send a lim `AgentMessage` as an A2A task and enqueue the agent's
    /// reply for `recv`.
    ///
    /// This is the bridge between lim's `AgentTransport` trait and A2A's
    /// task model. The `target` agent ID is ignored (the endpoint is
    /// fixed at construction time); multi-agent routing is handled by
    /// the server side.
    pub async fn send_message(&self, msg: AgentMessage) -> Result<()> {
        let a2a_msg = agent_message_to_a2a(&msg);
        let task_id = format!("task-{}", msg.id);
        let params = TaskSendParams { id: task_id, message: a2a_msg, session_id: None };
        let task = self.send_task(params).await?;

        // Enqueue the agent's last reply (if any) for recv.
        if let Some(reply) = task.history.last()
            && reply.role == "agent"
        {
            let agent_msg = a2a_to_agent_message(reply)?;
            // If the channel is full or closed, log but don't fail —
            // the message is best-effort for recv.
            if let Err(e) = self.inbox_tx.try_send(agent_msg) {
                warn!(error = %e, "A2A inbox full or closed, dropping agent reply");
            }
        }
        Ok(())
    }

    /// Receive the next agent reply (blocks until one arrives).
    pub async fn recv_message(&self) -> Result<AgentMessage> {
        let mut rx = self.inbox_rx.lock().await;
        rx.recv().await.ok_or_else(|| LimError::Config("a2a inbox closed".to_string()))
    }

    /// Protocol adapter in use (for diagnostics).
    pub fn adapter(&self) -> &A2AAdapter {
        &self.adapter
    }

    // ── Internal ───────────────────────────────────────────────────

    async fn rpc_call(&self, req: &JsonRpcRequest) -> Result<serde_json::Value> {
        let url = format!("{}/", self.endpoint);
        debug!(url = %url, method = %req.method, "A2A RPC call");
        let mut http_req = self.http.post(&url).json(req);
        if let Some(token) = &self.auth_token {
            http_req = http_req.bearer_auth(token);
        }
        let resp = http_req.send().await.map_err(|e| LimError::Config(format!("a2a http: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(LimError::Config(format!("a2a http error: {status} — {body}")));
        }
        let rpc_resp: JsonRpcResponse =
            resp.json().await.map_err(|e| LimError::Config(format!("a2a response parse: {e}")))?;
        if let Some(err) = rpc_resp.error {
            return Err(LimError::Config(format!("a2a rpc error [{}]: {}", err.code, err.message)));
        }
        rpc_resp.result.ok_or_else(|| LimError::Config("a2a rpc missing result".to_string()))
    }
}

/// Handler trait for the A2A server side (implemented in lim-serve).
///
/// lim-transport defines the trait so that test code and examples can
/// inject a mock handler without depending on lim-serve.
#[async_trait]
pub trait A2ARequestHandler: Send + Sync {
    /// Handle an incoming `tasks/send` request.
    async fn send_task(&self, params: TaskSendParams) -> Result<A2ATask>;

    /// Handle `tasks/get`.
    async fn get_task(&self, task_id: &str) -> Result<A2ATask>;

    /// Handle `tasks/cancel`.
    async fn cancel_task(&self, task_id: &str) -> Result<A2ATask>;

    /// Return this server's AgentCard.
    fn agent_card(&self) -> &AgentCard;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_card_serde_roundtrip() {
        let card = AgentCard {
            name: "lim".to_string(),
            version: "0.1.0".to_string(),
            endpoint: "https://lim.example.com".to_string(),
            description: Some("lim agent".to_string()),
            capabilities: AgentCapabilities {
                streaming: true,
                push_notifications: false,
                state_transition_history: true,
            },
            default_input_modes: vec!["text".to_string()],
            default_output_modes: vec!["text".to_string()],
            skills: vec![AgentSkill {
                id: "write".to_string(),
                name: "Writer".to_string(),
                description: Some("writes prose".to_string()),
                tags: vec!["novel".to_string()],
            }],
            auth_token: None,
        };
        let json = serde_json::to_string(&card).unwrap();
        assert!(json.contains("\"name\":\"lim\""));
        assert!(json.contains("\"streaming\":true"));
        let parsed: AgentCard = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "lim");
        assert!(parsed.capabilities.streaming);
    }

    #[test]
    fn task_state_serde_lowercase() {
        let json = "\"completed\"";
        let state: TaskState = serde_json::from_str(json).unwrap();
        assert_eq!(state, TaskState::Completed);
        assert_eq!(serde_json::to_string(&TaskState::Working).unwrap(), "\"working\"");
    }

    #[tokio::test]
    async fn a2a_adapter_roundtrip_text() {
        let adapter = A2AAdapter::new();
        let msg = AgentMessage::user("hello a2a");
        let encoded = adapter.encode(&msg).await.unwrap();
        let decoded = adapter.decode(&encoded).await.unwrap();
        assert_eq!(decoded.role, Role::User);
        match &decoded.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "hello a2a"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_adapter_assistant_maps_to_agent_role() {
        let adapter = A2AAdapter::new();
        let msg =
            AgentMessage::assistant(vec![ContentBlock::Text { text: "I'm the agent".to_string() }]);
        let encoded = adapter.encode(&msg).await.unwrap();
        let a2a: A2AMessage = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(a2a.role, "agent");
    }

    #[tokio::test]
    async fn a2a_adapter_thinking_blocks_skipped() {
        let adapter = A2AAdapter::new();
        let msg = AgentMessage::assistant(vec![
            ContentBlock::Thinking { text: "secret".to_string() },
            ContentBlock::Text { text: "visible".to_string() },
        ]);
        let encoded = adapter.encode(&msg).await.unwrap();
        let a2a: A2AMessage = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(a2a.parts.len(), 1, "thinking should be skipped");
        match &a2a.parts[0] {
            A2APart::Text { text } => assert_eq!(text, "visible"),
            other => panic!("expected Text part, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a2a_adapter_preserves_message_id() {
        let adapter = A2AAdapter::new();
        let msg = AgentMessage::user("test");
        let original_id = msg.id.to_string();
        let encoded = adapter.encode(&msg).await.unwrap();
        let a2a: A2AMessage = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(a2a.message_id.as_deref(), Some(original_id.as_str()));
    }

    #[test]
    fn a2a_message_with_data_part_roundtrip() {
        let a2a = A2AMessage {
            role: "agent".to_string(),
            parts: vec![
                A2APart::Text { text: "result".to_string() },
                A2APart::Data { data: serde_json::json!({"key": "value"}) },
            ],
            message_id: Some("msg-1".to_string()),
        };
        let json = serde_json::to_string(&a2a).unwrap();
        let parsed: A2AMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.parts.len(), 2);
        assert!(matches!(parsed.parts[0], A2APart::Text { .. }));
        assert!(matches!(parsed.parts[1], A2APart::Data { .. }));
    }

    /// Minimal HTTP mock using tokio TcpListener.
    ///
    /// Returns the AgentCard JSON for `GET /.well-known/agent-card.json`
    /// and a canned task response for `POST /`.
    async fn spawn_mock_a2a_server(agent_card_json: &str, task_response_json: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let card = agent_card_json.to_string();
        let task = task_response_json.to_string();

        tokio::spawn(async move {
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(c) => c,
                    Err(_) => break,
                };
                let card = card.clone();
                let task = task.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]);

                    let response = if request.starts_with("GET /") {
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                            card.len(),
                            card
                        )
                    } else if request.starts_with("POST /") {
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                            task.len(),
                            task
                        )
                    } else {
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_string()
                    };
                    let _ = sock.write_all(response.as_bytes()).await;
                    let _ = sock.flush().await;
                });
            }
        });

        format!("http://{}", addr)
    }

    #[tokio::test]
    async fn http_transport_fetches_agent_card() {
        let card_json = serde_json::json!({
            "name": "mock-agent",
            "version": "1.0",
            "url": "http://placeholder",
            "capabilities": { "streaming": false },
            "skills": []
        })
        .to_string();
        let endpoint = spawn_mock_a2a_server(&card_json, "{}").await;

        let transport = A2AHttpTransport::connect(&endpoint, None).await.unwrap();
        let card = transport.fetch_agent_card().await.unwrap();
        assert_eq!(card.name, "mock-agent");
        assert_eq!(card.version, "1.0");
    }

    #[tokio::test]
    async fn http_transport_send_task() {
        let task_json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "id": "task-1",
                "status": { "state": "completed" },
                "artifacts": [],
                "history": [
                    { "role": "user", "parts": [{"type": "text", "text": "hi"}] },
                    { "role": "agent", "parts": [{"type": "text", "text": "hello back"}] }
                ]
            }
        })
        .to_string();
        let endpoint = spawn_mock_a2a_server("{}", &task_json).await;

        let transport = A2AHttpTransport::connect(&endpoint, None).await.unwrap();
        let params = TaskSendParams {
            id: "task-1".to_string(),
            message: A2AMessage {
                role: "user".to_string(),
                parts: vec![A2APart::Text { text: "hi".to_string() }],
                message_id: None,
            },
            session_id: None,
        };
        let task = transport.send_task(params).await.unwrap();
        assert_eq!(task.id, "task-1");
        assert_eq!(task.status.state, TaskState::Completed);
        assert_eq!(task.history.len(), 2);
        assert_eq!(task.history[1].role, "agent");
    }

    #[tokio::test]
    async fn http_transport_send_message_enqueues_reply() {
        let task_json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "id": "task-x",
                "status": { "state": "completed" },
                "artifacts": [],
                "history": [
                    { "role": "agent", "parts": [{"type": "text", "text": "reply"}] }
                ]
            }
        })
        .to_string();
        let endpoint = spawn_mock_a2a_server("{}", &task_json).await;

        let transport = A2AHttpTransport::connect(&endpoint, None).await.unwrap();
        let msg = AgentMessage::user("ping");
        transport.send_message(msg).await.unwrap();

        let reply = transport.recv_message().await.unwrap();
        assert_eq!(reply.role, Role::Assistant);
        match &reply.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "reply"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn agent_message_to_a2a_empty_content() {
        let msg = AgentMessage::assistant(vec![]);
        let a2a = agent_message_to_a2a(&msg);
        assert_eq!(a2a.role, "agent");
        assert!(a2a.parts.is_empty());
    }
}
