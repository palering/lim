//! Anthropic Messages API provider (`/v1/messages`).
//!
//! Implements Anthropic's streaming format, which differs from OpenAI in:
//! - System prompt goes in a top-level `system` field (not a system role message)
//! - `max_tokens` is required
//! - Tools use `{name, description, input_schema}` (not `function` wrapper)
//! - SSE events use `event:` + `data:` lines (eventsource-stream exposes `.event`)
//! - Tool call arguments arrive as `input_json_delta` on `content_block_delta`
//! - `thinking` content blocks carry extended reasoning
//!
//! Also compatible with Anthropic-format proxies.

use std::collections::HashMap;

use futures::stream::{BoxStream, StreamExt};
use reqwest::Client;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use lim_core::id::ToolCallId;
use lim_core::stop_reason::StopReason;
use lim_core::tool::ToolSchema;
use lim_core::usage::Usage;

use crate::error::ProviderError;
use crate::http::{sse_event_stream, try_extract_error};
use crate::model::{ModelId, ModelMetadata};
use crate::provider::{LlmRequest, Provider};
use crate::stream::StreamEvent;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

/// Anthropic Messages API provider.
///
/// Compatible with Anthropic's official API and any Anthropic-format proxy.
pub struct AnthropicProvider {
    client: Client,
    api_key: String,
    base_url: String,
    /// Complete endpoint URL that overrides `base_url + suffix` when set.
    /// Use for non-standard providers (private proxies with custom paths,
    /// endpoints with query params, etc.).
    full_url: Option<String>,
    anthropic_version: String,
    extra_headers: HashMap<String, String>,
}

impl AnthropicProvider {
    /// Create a provider with an explicit API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            full_url: None,
            anthropic_version: DEFAULT_ANTHROPIC_VERSION.to_string(),
            extra_headers: HashMap::new(),
        }
    }

    /// Override the base URL (for proxies).
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Override the full endpoint URL, bypassing `base_url + /v1/messages`
    /// concatenation. When set, `endpoint()` returns this URL verbatim.
    ///
    /// Use cases: private proxies with custom paths, endpoints with query
    /// params, non-standard endpoint layouts.
    pub fn with_full_url(mut self, url: impl Into<String>) -> Self {
        self.full_url = Some(url.into());
        self
    }

    /// Override the `anthropic-version` header.
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.anthropic_version = version.into();
        self
    }

    /// Add extra headers.
    pub fn with_extra_headers(mut self, headers: HashMap<String, String>) -> Self {
        self.extra_headers = headers;
        self
    }

    fn endpoint(&self) -> String {
        if let Some(ref full) = self.full_url {
            return full.clone();
        }
        let base = self.base_url.trim_end_matches('/');
        format!("{base}/v1/messages")
    }

    /// Build the JSON request body for `/v1/messages`.
    fn build_request_body(&self, request: &LlmRequest) -> serde_json::Value {
        // Anthropic separates system prompt from messages
        let system_text = request.system.clone().unwrap_or_default();

        let mut messages: Vec<serde_json::Value> = Vec::new();
        for msg in &request.messages {
            if let Some(converted) = convert_message(msg) {
                messages.push(converted);
            }
        }

        let mut body = serde_json::json!({
            "model": request.model,
            "messages": messages,
            "max_tokens": request.max_tokens.unwrap_or(4096),
            "stream": true,
        });

        if !system_text.is_empty() {
            body["system"] = serde_json::json!(system_text);
        }

        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(convert_tool_schema)
                .collect();
            body["tools"] = serde_json::Value::Array(tools);
        }

        body
    }
}

impl Provider for AnthropicProvider {
    fn stream<'a>(
        &'a self,
        request: LlmRequest,
    ) -> BoxStream<'a, Result<StreamEvent, ProviderError>> {
        let body = self.build_request_body(&request);
        let url = self.endpoint();
        let api_key = self.api_key.clone();
        let version = self.anthropic_version.clone();
        let client = self.client.clone();
        let extra_headers = self.extra_headers.clone();

        let (tx, rx) = mpsc::channel(64);

        tokio::spawn(async move {
            let mut req_builder = client
                .post(&url)
                .header("x-api-key", &api_key)
                .header("anthropic-version", &version)
                .header("Content-Type", "application/json")
                .json(&body);

            for (k, v) in &extra_headers {
                req_builder = req_builder.header(k, v);
            }

            let response = match req_builder.send().await {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Err(ProviderError::from(e))).await;
                    return;
                }
            };

            if !response.status().is_success() {
                let status = response.status().as_u16();
                let text = response.text().await.unwrap_or_default();
                let _ = tx.send(Err(classify_http_error(status, &text))).await;
                return;
            }

            let mut state = AnthropicStreamState::default();
            let mut stream = sse_event_stream(response);

            while let Some(result) = stream.next().await {
                match result {
                    Ok(event) => {
                        // Detect provider/proxy errors embedded as SSE
                        // events (HTTP 200 + error event). Without this,
                        // malformed streams would be silently dropped as
                        // empty responses.
                        if let Some(err) = try_extract_error(&event) {
                            let _ = tx.send(Err(err)).await;
                            return;
                        }
                        for ev in state.parse_event(&event) {
                            if tx.send(Ok(ev)).await.is_err() {
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                }
            }
        });

        ReceiverStream::new(rx).boxed()
    }

    fn model_metadata(&self, model: &str) -> ModelMetadata {
        let (ctx, max_out, thinking) = match model {
            m if m.starts_with("claude-opus-4") => (200_000, 32_000, true),
            m if m.starts_with("claude-sonnet-4") => (200_000, 16_000, true),
            m if m.starts_with("claude-3-7-sonnet") => (200_000, 16_000, true),
            m if m.starts_with("claude-3-5-sonnet") => (200_000, 8_192, false),
            m if m.starts_with("claude-3-opus") => (200_000, 4_096, false),
            m if m.starts_with("claude-3-haiku") => (200_000, 4_096, false),
            _ => (200_000, 4_096, false),
        };

        ModelMetadata {
            id: ModelId::new(model),
            display_name: format!("Anthropic {model}"),
            context_window: ctx,
            max_output_tokens: max_out,
            supports_tools: true,
            supports_thinking: thinking,
        }
    }
}

/// Stateful parser for Anthropic SSE stream.
///
/// Maintains `content_block index → (ToolCallId, block_type)` mapping to
/// correlate `content_block_delta` and `content_block_stop` events.
#[derive(Default)]
struct AnthropicStreamState {
    /// `content_block.index` → `(ToolCallId, block_type)`
    blocks: HashMap<usize, (ToolCallId, String)>,
}

impl AnthropicStreamState {
    fn parse_event(&mut self, event: &eventsource_stream::Event) -> Vec<StreamEvent> {
        let event_type = event.event.as_str();
        let data = &event.data;

        let payload: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return vec![],
        };

        match event_type {
            "message_start" => {
                // Extract usage.input_tokens if present
                if let Some(usage) = payload.get("message").and_then(|m| m.get("usage")) {
                    let input = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    return vec![StreamEvent::Usage(Usage::new(input, 0))];
                }
                vec![]
            }
            "content_block_start" => {
                let index = payload.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let block = payload.get("content_block").cloned().unwrap_or_default();
                let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");

                match block_type {
                    "tool_use" => {
                        let id_str = block.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        // Use Anthropic's id directly so that subsequent
                        // `tool_result` messages can reference the same id.
                        let id = if id_str.is_empty() {
                            ToolCallId::new()
                        } else {
                            ToolCallId::from_string(id_str)
                        };
                        let name = block.get("name").and_then(|v| v.as_str()).map(String::from);
                        self.blocks.insert(index, (id.clone(), block_type.to_string()));
                        vec![StreamEvent::ToolCallStart { id, name }]
                    }
                    _ => {
                        // text, thinking, etc. — deltas come in content_block_delta
                        self.blocks.insert(index, (ToolCallId::new(), block_type.to_string()));
                        vec![]
                    }
                }
            }
            "content_block_delta" => {
                let index = payload.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let delta = payload.get("delta").cloned().unwrap_or_default();
                let delta_type = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");

                match delta_type {
                    "text_delta" => {
                        let text = delta.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        vec![StreamEvent::TextDelta { text: text.to_string() }]
                    }
                    "thinking_delta" => {
                        let text = delta.get("thinking").and_then(|v| v.as_str()).unwrap_or("");
                        vec![StreamEvent::ThinkingDelta { text: text.to_string() }]
                    }
                    "input_json_delta" => {
                        // Tool call arguments fragment
                        if let Some((id, _)) = self.blocks.get(&index) {
                            let partial = delta.get("partial_json").and_then(|v| v.as_str()).unwrap_or("");
                            vec![StreamEvent::ToolCallDelta {
                                id: id.clone(),
                                arguments_delta: partial.to_string(),
                            }]
                        } else {
                            vec![]
                        }
                    }
                    _ => vec![],
                }
            }
            "content_block_stop" => {
                let index = payload.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                // Remove from tracking; no ToolCallEnd event (StreamEvent has no
                // such variant — tool calls are implicitly ended by Done).
                self.blocks.remove(&index);
                vec![]
            }
            "message_delta" => {
                // stop_reason + output_tokens usage
                let mut events = Vec::new();
                if let Some(delta) = payload.get("delta") {
                    if let Some(stop) = delta.get("stop_reason").and_then(|v| v.as_str()) {
                        let reason = match stop {
                            "tool_use" => StopReason::ToolCall,
                            "max_tokens" => StopReason::MaxTokens,
                            _ => StopReason::EndTurn,
                        };
                        events.push(StreamEvent::Done { stop_reason: reason });
                    }
                }
                if let Some(usage) = payload.get("usage") {
                    let output = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                    events.push(StreamEvent::Usage(Usage::new(0, output)));
                }
                events
            }
            "message_stop" => {
                // Stream is done; if no Done event was emitted yet, emit one
                vec![]
            }
            "error" => {
                let _msg = payload
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                vec![] // errors handled via HTTP status; this is mid-stream
            }
            _ => vec![],
        }
    }
}

// Note: Anthropic's `content_block_start` event carries the tool_use `id`
// (e.g. "toolu_01abc..."). We store it verbatim in `ToolCallId::from_string`
// so that:
//   1. Subsequent `content_block_delta` events can be correlated by index.
//   2. When we send `tool_result` back to Anthropic, the original id is
//      preserved in the message's `tool_call_ids` and echoed in the
//      `tool_use_id` field of the converted user message.
//
// This works because `ToolCallId` is a String-backed newtype (not Ulid).

// --- HTTP error classification ---

fn classify_http_error(status: u16, body: &str) -> ProviderError {
    match status {
        401 | 403 => ProviderError::Auth(body.to_string()),
        429 => ProviderError::RateLimited {
            retry_after_ms: None,
        },
        400 => {
            if body.contains("context") || body.contains("too long") {
                ProviderError::ContextWindowExceeded
            } else {
                ProviderError::InvalidRequest(body.to_string())
            }
        }
        402 => ProviderError::QuotaExceeded,
        502 | 503 | 529 => ProviderError::ServerOverloaded,
        _ => ProviderError::Api {
            status,
            message: body.to_string(),
        },
    }
}

// --- Message conversion ---

/// Convert our `LlmMessage` to Anthropic's message JSON.
///
/// Returns `None` for System role (system goes in the top-level `system` field).
fn convert_message(msg: &lim_core::message::LlmMessage) -> Option<serde_json::Value> {
    use lim_core::content::ContentBlock;
    use lim_core::message::Role;

    match msg.role {
        Role::System => {
            // System is handled separately in build_request_body
            None
        }
        Role::User => {
            let has_image = msg
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::Image { .. }));

            if has_image {
                let parts: Vec<serde_json::Value> = msg
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => {
                            Some(serde_json::json!({"type": "text", "text": text}))
                        }
                        ContentBlock::Image { data, mime_type } => {
                            use base64::{engine::general_purpose::STANDARD, Engine};
                            let b64 = STANDARD.encode(data);
                            Some(serde_json::json!({
                                "type": "image",
                                "source": {
                                    "type": "base64",
                                    "media_type": mime_type,
                                    "data": b64,
                                }
                            }))
                        }
                        _ => None,
                    })
                    .collect();
                Some(serde_json::json!({"role": "user", "content": parts}))
            } else {
                let text = extract_text(&msg.content);
                Some(serde_json::json!({"role": "user", "content": text}))
            }
        }
        Role::Assistant => {
            let mut parts: Vec<serde_json::Value> = Vec::new();

            // Thinking blocks
            for block in &msg.content {
                if let ContentBlock::Thinking { text } = block {
                    parts.push(serde_json::json!({"type": "thinking", "thinking": text}));
                }
            }

            // Text
            let text = msg
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                parts.push(serde_json::json!({"type": "text", "text": text}));
            }

            // Tool calls
            for block in &msg.content {
                if let ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                } = block
                {
                    parts.push(serde_json::json!({
                        "type": "tool_use",
                        "id": id.to_string(),
                        "name": name,
                        "input": arguments,
                    }));
                }
            }

            if parts.is_empty() {
                parts.push(serde_json::json!({"type": "text", "text": ""}));
            }

            Some(serde_json::json!({"role": "assistant", "content": parts}))
        }
        Role::Tool => {
            // Tool results in Anthropic are `role: "user"` with `tool_result` content blocks
            let text = extract_text(&msg.content);
            let tool_call_id = msg
                .tool_call_ids
                .first()
                .map(|id| id.to_string())
                .unwrap_or_default();

            Some(serde_json::json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": tool_call_id,
                    "content": text,
                }]
            }))
        }
    }
}

fn extract_text(content: &[lim_core::content::ContentBlock]) -> String {
    use lim_core::content::ContentBlock;
    content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Convert our `ToolSchema` to Anthropic's tool format.
fn convert_tool_schema(schema: &ToolSchema) -> serde_json::Value {
    serde_json::json!({
        "name": schema.name,
        "description": schema.description,
        "input_schema": schema.parameters,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(event_type: &str, data: &str) -> eventsource_stream::Event {
        eventsource_stream::Event {
            event: event_type.to_string(),
            data: data.to_string(),
            id: String::new(),
            retry: None,
        }
    }

    #[test]
    fn parse_text_delta() {
        let mut state = AnthropicStreamState::default();
        let ev = make_event(
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
        );
        let events = state.parse_event(&ev);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::TextDelta { text } => assert_eq!(text, "Hello"),
            _ => panic!("expected TextDelta"),
        }
    }

    #[test]
    fn parse_thinking_delta() {
        let mut state = AnthropicStreamState::default();
        let ev = make_event(
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#,
        );
        let events = state.parse_event(&ev);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ThinkingDelta { text } => assert_eq!(text, "hmm"),
            _ => panic!("expected ThinkingDelta"),
        }
    }

    #[test]
    fn parse_tool_call_stream() {
        let mut state = AnthropicStreamState::default();

        // content_block_start: tool_use
        let ev1 = make_event(
            "content_block_start",
            r#"{"index":1,"content_block":{"type":"tool_use","id":"toolu_abc","name":"bash"}}"#,
        );
        let e1 = state.parse_event(&ev1);
        assert_eq!(e1.len(), 1);
        let tool_id = match &e1[0] {
            StreamEvent::ToolCallStart { id, name } => {
                assert_eq!(name.as_deref(), Some("bash"));
                // The Anthropic-supplied id must be preserved verbatim so
                // that subsequent tool_result messages can reference it.
                assert_eq!(id.as_str(), "toolu_abc");
                id.clone()
            }
            _ => panic!("expected ToolCallStart"),
        };

        // content_block_delta: input_json_delta
        let ev2 = make_event(
            "content_block_delta",
            r#"{"index":1,"delta":{"type":"input_json_delta","partial_json":"{\"cmd\":"}}"#,
        );
        let e2 = state.parse_event(&ev2);
        assert_eq!(e2.len(), 1);
        match &e2[0] {
            StreamEvent::ToolCallDelta { id, arguments_delta } => {
                assert_eq!(*id, tool_id);
                assert_eq!(id.as_str(), "toolu_abc");
                assert_eq!(arguments_delta, "{\"cmd\":");
            }
            _ => panic!("expected ToolCallDelta"),
        }

        // content_block_stop (no event emitted, just tracking cleanup)
        let ev3 = make_event("content_block_stop", r#"{"index":1}"#);
        let e3 = state.parse_event(&ev3);
        assert_eq!(e3.len(), 0);

        // message_delta: stop_reason = tool_use
        let ev4 = make_event(
            "message_delta",
            r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":42}}"#,
        );
        let e4 = state.parse_event(&ev4);
        assert!(e4
            .iter()
            .any(|e| matches!(e, StreamEvent::Done { stop_reason: StopReason::ToolCall })));
    }

    #[test]
    fn convert_user_message() {
        let msg = lim_core::message::LlmMessage {
            role: lim_core::message::Role::User,
            content: vec![lim_core::content::ContentBlock::text("hi")],
            tool_call_ids: vec![],
        };
        let json = convert_message(&msg).unwrap();
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"], "hi");
    }

    #[test]
    fn convert_system_returns_none() {
        let msg = lim_core::message::LlmMessage {
            role: lim_core::message::Role::System,
            content: vec![lim_core::content::ContentBlock::text("system")],
            tool_call_ids: vec![],
        };
        assert!(convert_message(&msg).is_none());
    }

    #[test]
    fn convert_tool_result_uses_user_role() {
        // Use an Anthropic-style id to verify it round-trips verbatim.
        let tc_id = lim_core::id::ToolCallId::from_string("toolu_01XYZ");
        let msg = lim_core::message::LlmMessage {
            role: lim_core::message::Role::Tool,
            content: vec![lim_core::content::ContentBlock::text("result")],
            tool_call_ids: vec![tc_id.clone()],
        };
        let json = convert_message(&msg).unwrap();
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"][0]["type"], "tool_result");
        assert_eq!(json["content"][0]["tool_use_id"], "toolu_01XYZ");
        assert_eq!(json["content"][0]["tool_use_id"], tc_id.to_string());
    }

    #[test]
    fn convert_tool_schema_uses_input_schema() {
        let schema = ToolSchema {
            name: "read".to_string(),
            description: "Read a file".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        };
        let json = convert_tool_schema(&schema);
        assert_eq!(json["name"], "read");
        assert!(json.get("input_schema").is_some());
        assert!(json.get("function").is_none());
    }

    #[test]
    fn model_metadata_claude_sonnet_4() {
        let provider = AnthropicProvider::new("test-key");
        let meta = provider.model_metadata("claude-sonnet-4-20250514");
        assert_eq!(meta.context_window, 200_000);
        assert!(meta.supports_thinking);
    }

    // --- endpoint() resolution tests ---

    #[test]
    fn endpoint_uses_default_when_nothing_set() {
        let p = AnthropicProvider::new("key");
        assert_eq!(p.endpoint(), "https://api.anthropic.com/v1/messages");
    }

    #[test]
    fn endpoint_falls_back_to_base_url_suffix() {
        let p = AnthropicProvider::new("key")
            .with_base_url("https://anthropic-proxy.example.com");
        assert_eq!(
            p.endpoint(),
            "https://anthropic-proxy.example.com/v1/messages"
        );
    }

    #[test]
    fn endpoint_uses_full_url_when_set() {
        let p = AnthropicProvider::new("key")
            .with_full_url("https://proxy.example.com/custom/messages?key=1");
        assert_eq!(
            p.endpoint(),
            "https://proxy.example.com/custom/messages?key=1"
        );
    }

    #[test]
    fn full_url_overrides_base_url() {
        let p = AnthropicProvider::new("key")
            .with_base_url("https://should-be-ignored.com")
            .with_full_url("https://real-endpoint.com/api");
        assert_eq!(p.endpoint(), "https://real-endpoint.com/api");
    }
}
