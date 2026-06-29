//! OpenAI Responses API provider (`/v1/responses`).
//!
//! The Responses API is OpenAI's newer endpoint (replacing Chat Completions for
//! new features). Key differences from Chat Completions:
//!
//! - `instructions` field replaces the system message
//! - `input` is an array of items (messages, function calls, function outputs)
//! - Standard `function_call` items arrive complete in `response.output_item.done`
//!   (no index-based delta accumulation needed — simpler than Chat Completions)
//! - SSE events are typed: `response.created`, `response.output_text.delta`,
//!   `response.output_item.added`, `response.output_item.done`, `response.completed`
//!
//! Compatibility: OpenAI official only. Most proxies do not yet support Responses.

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

const DEFAULT_BASE_URL: &str = "https://api.openai.com";

/// OpenAI Responses API provider.
pub struct OpenAiResponsesProvider {
    client: Client,
    api_key: String,
    base_url: String,
    /// Complete endpoint URL that overrides `base_url + suffix` when set.
    /// Use for non-standard providers (Azure, private proxies with custom
    /// paths, endpoints with query params, etc.).
    full_url: Option<String>,
    extra_headers: HashMap<String, String>,
}

impl OpenAiResponsesProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            full_url: None,
            extra_headers: HashMap::new(),
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Override the full endpoint URL, bypassing `base_url + /responses`
    /// concatenation. When set, `endpoint()` returns this URL verbatim.
    ///
    /// Use cases: Azure OpenAI (URL contains deployment name + query params),
    /// private proxies with custom paths, non-standard endpoint layouts.
    pub fn with_full_url(mut self, url: impl Into<String>) -> Self {
        self.full_url = Some(url.into());
        self
    }

    pub fn with_extra_headers(mut self, headers: HashMap<String, String>) -> Self {
        self.extra_headers = headers;
        self
    }

    fn endpoint(&self) -> String {
        if let Some(ref full) = self.full_url {
            return full.clone();
        }
        let base = self.base_url.trim_end_matches('/');
        format!("{base}/v1/responses")
    }

    fn build_request_body(&self, request: &LlmRequest) -> serde_json::Value {
        // Convert messages to ResponseItem input array
        let input: Vec<serde_json::Value> = request
            .messages
            .iter()
            .filter_map(convert_message_to_input_item)
            .collect();

        let mut body = serde_json::json!({
            "model": request.model,
            "input": input,
            "stream": true,
        });

        if let Some(ref system) = request.system {
            body["instructions"] = serde_json::json!(system);
        }

        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(convert_tool_schema)
                .collect();
            body["tools"] = serde_json::Value::Array(tools);
        }

        if let Some(max_tokens) = request.max_tokens {
            body["max_output_tokens"] = serde_json::json!(max_tokens);
        }

        body
    }
}

impl Provider for OpenAiResponsesProvider {
    fn stream<'a>(
        &'a self,
        request: LlmRequest,
    ) -> BoxStream<'a, Result<StreamEvent, ProviderError>> {
        let body = self.build_request_body(&request);
        let url = self.endpoint();
        let api_key = self.api_key.clone();
        let client = self.client.clone();
        let extra_headers = self.extra_headers.clone();

        let (tx, rx) = mpsc::channel(64);

        tokio::spawn(async move {
            let mut req_builder = client
                .post(&url)
                .header("Authorization", format!("Bearer {api_key}"))
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

            let mut state = ResponsesStreamState::default();
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
            m if m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") => {
                (200_000, 100_000, true)
            }
            m if m.starts_with("gpt-4o") => (128_000, 16_384, false),
            m if m.starts_with("gpt-4.1") => (1_047_576, 32_768, false),
            _ => (128_000, 8_192, false),
        };

        ModelMetadata {
            id: ModelId::new(model),
            display_name: format!("OpenAI {model}"),
            context_window: ctx,
            max_output_tokens: max_out,
            supports_tools: true,
            supports_thinking: thinking,
        }
    }
}

/// Stateful parser for Responses API SSE stream.
///
/// Unlike Chat Completions, standard `function_call` items arrive complete in
/// `response.output_item.done` — no delta accumulation needed. We still track
/// item ids for `added` → `done` correlation.
#[derive(Default)]
struct ResponsesStreamState {
    /// item id → ToolCallId (for correlation)
    items: HashMap<String, ToolCallId>,
}

impl ResponsesStreamState {
    fn parse_event(&mut self, event: &eventsource_stream::Event) -> Vec<StreamEvent> {
        let data = &event.data;
        if data == "[DONE]" {
            return vec![];
        }

        let payload: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return vec![],
        };

        let event_type = payload
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match event_type {
            "response.created" => {
                // Nothing to emit; MessageStart is implicit
                vec![]
            }
            "response.output_text.delta" => {
                let delta = payload.get("delta").and_then(|v| v.as_str()).unwrap_or("");
                vec![StreamEvent::TextDelta {
                    text: delta.to_string(),
                }]
            }
            "response.reasoning_summary_text.delta" => {
                let delta = payload.get("delta").and_then(|v| v.as_str()).unwrap_or("");
                vec![StreamEvent::ThinkingDelta {
                    text: delta.to_string(),
                }]
            }
            "response.reasoning_text.delta" => {
                let delta = payload.get("delta").and_then(|v| v.as_str()).unwrap_or("");
                vec![StreamEvent::ThinkingDelta {
                    text: delta.to_string(),
                }]
            }
            "response.output_item.added" => {
                // A new output item is starting. If it's a function_call, emit ToolCallStart.
                if let Some(item) = payload.get("item") {
                    if item.get("type").and_then(|v| v.as_str()) == Some("function_call") {
                        let call_id = item
                            .get("call_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .map(String::from);
                        // Preserve the provider-supplied call_id (e.g.
                        // "call_123") so that subsequent tool_result messages
                        // can echo it back verbatim. Fall back to a generated
                        // Ulid only if the provider omits the call_id.
                        let id = if call_id.is_empty() {
                            ToolCallId::new()
                        } else {
                            ToolCallId::from_string(&call_id)
                        };
                        self.items.insert(call_id, id.clone());
                        vec![StreamEvent::ToolCallStart { id, name }]
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                }
            }
            "response.output_item.done" => {
                // Item is complete. If it's a function_call, emit the full arguments
                // as a single ToolCallDelta (no accumulation needed).
                if let Some(item) = payload.get("item") {
                    if item.get("type").and_then(|v| v.as_str()) == Some("function_call") {
                        let call_id = item
                            .get("call_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let arguments = item
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();

                        if let Some(id) = self.items.remove(&call_id) {
                            vec![StreamEvent::ToolCallDelta {
                                id,
                                arguments_delta: arguments,
                            }]
                        } else {
                            // Item was not tracked (no added event); create and emit
                            let id = ToolCallId::new();
                            vec![StreamEvent::ToolCallDelta {
                                id,
                                arguments_delta: arguments,
                            }]
                        }
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                }
            }
            "response.completed" => {
                let mut events = Vec::new();

                // Extract usage
                if let Some(usage) = payload.get("response").and_then(|r| r.get("usage")) {
                    let input = usage
                        .get("input_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let output = usage
                        .get("output_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    events.push(StreamEvent::Usage(Usage::new(input, output)));
                }

                // Determine stop reason
                let stop_reason = payload
                    .get("response")
                    .and_then(|r| r.get("status"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("completed");
                let reason = match stop_reason {
                    "incomplete" => StopReason::MaxTokens,
                    _ => StopReason::EndTurn,
                };
                events.push(StreamEvent::Done {
                    stop_reason: reason,
                });

                events
            }
            _ => vec![],
        }
    }
}

fn classify_http_error(status: u16, body: &str) -> ProviderError {
    match status {
        401 | 403 => ProviderError::Auth(body.to_string()),
        429 => ProviderError::RateLimited {
            retry_after_ms: None,
        },
        400 => {
            if body.contains("context_length") || body.contains("context window") {
                ProviderError::ContextWindowExceeded
            } else {
                ProviderError::InvalidRequest(body.to_string())
            }
        }
        402 => ProviderError::QuotaExceeded,
        502 | 503 => ProviderError::ServerOverloaded,
        _ => ProviderError::Api {
            status,
            message: body.to_string(),
        },
    }
}

/// Convert our `LlmMessage` to a Responses API input item.
fn convert_message_to_input_item(msg: &lim_core::message::LlmMessage) -> Option<serde_json::Value> {
    use lim_core::content::ContentBlock;
    use lim_core::message::Role;

    match msg.role {
        Role::System => {
            // System goes in `instructions`, not in `input`
            None
        }
        Role::User => {
            let text = extract_text(&msg.content);
            Some(serde_json::json!({
                "role": "user",
                "content": text,
            }))
        }
        Role::Assistant => {
            let text = extract_text(&msg.content);

            // Extract tool calls
            let tool_calls: Vec<serde_json::Value> = msg
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } => Some(serde_json::json!({
                        "type": "function_call",
                        "call_id": id.to_string(),
                        "name": name,
                        "arguments": arguments.to_string(),
                    })),
                    _ => None,
                })
                .collect();

            if !tool_calls.is_empty() {
                // Return multiple items: assistant message + function calls
                // For simplicity, return the first function call as a separate item.
                // The Responses API expects function_call items at the top level.
                Some(tool_calls[0].clone())
            } else {
                Some(serde_json::json!({
                    "role": "assistant",
                    "content": text,
                }))
            }
        }
        Role::Tool => {
            let text = extract_text(&msg.content);
            let call_id = msg
                .tool_call_ids
                .first()
                .map(|id| id.to_string())
                .unwrap_or_default();

            Some(serde_json::json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": text,
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

fn convert_tool_schema(schema: &ToolSchema) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "name": schema.name,
        "description": schema.description,
        "parameters": schema.parameters,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(data: &str) -> eventsource_stream::Event {
        eventsource_stream::Event {
            event: "message".to_string(),
            data: data.to_string(),
            id: String::new(),
            retry: None,
        }
    }

    #[test]
    fn parse_text_delta() {
        let mut state = ResponsesStreamState::default();
        let ev = make_event(
            r#"{"type":"response.output_text.delta","delta":"Hello"}"#,
        );
        let events = state.parse_event(&ev);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::TextDelta { text } => assert_eq!(text, "Hello"),
            _ => panic!("expected TextDelta"),
        }
    }

    #[test]
    fn parse_reasoning_delta() {
        let mut state = ResponsesStreamState::default();
        let ev = make_event(
            r#"{"type":"response.reasoning_summary_text.delta","delta":"hmm"}"#,
        );
        let events = state.parse_event(&ev);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ThinkingDelta { text } => assert_eq!(text, "hmm"),
            _ => panic!("expected ThinkingDelta"),
        }
    }

    /// Responses API: function_call arrives complete in output_item.done.
    /// No delta accumulation needed.
    #[test]
    fn parse_function_call_complete() {
        let mut state = ResponsesStreamState::default();

        // output_item.added
        let ev1 = make_event(
            r#"{"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_123","name":"bash"}}"#,
        );
        let e1 = state.parse_event(&ev1);
        assert_eq!(e1.len(), 1);
        let tool_id = match &e1[0] {
            StreamEvent::ToolCallStart { id, name } => {
                assert_eq!(name.as_deref(), Some("bash"));
                // The provider-supplied call_id must be preserved verbatim.
                assert_eq!(id.as_str(), "call_123", "must preserve OpenAI's call_id");
                id.clone()
            }
            _ => panic!("expected ToolCallStart"),
        };

        // output_item.done — full arguments
        let ev2 = make_event(
            r#"{"type":"response.output_item.done","item":{"type":"function_call","call_id":"call_123","name":"bash","arguments":"{\"cmd\":\"ls\"}"}}"#,
        );
        let e2 = state.parse_event(&ev2);
        assert_eq!(e2.len(), 1);
        match &e2[0] {
            StreamEvent::ToolCallDelta { id, arguments_delta } => {
                assert_eq!(*id, tool_id);
                assert_eq!(arguments_delta, "{\"cmd\":\"ls\"}");
            }
            _ => panic!("expected ToolCallDelta with full arguments"),
        }
    }

    /// When the provider omits `call_id` (uncommon but possible), we fall
    /// back to a generated Ulid-based id. The important property is that
    /// the added/done correlation still works.
    #[test]
    fn function_call_falls_back_when_call_id_missing() {
        let mut state = ResponsesStreamState::default();

        // No call_id field
        let ev1 = make_event(
            r#"{"type":"response.output_item.added","item":{"type":"function_call","name":"read"}}"#,
        );
        let e1 = state.parse_event(&ev1);
        assert_eq!(e1.len(), 1);
        let start_id = match &e1[0] {
            StreamEvent::ToolCallStart { id, .. } => id.clone(),
            _ => panic!("expected ToolCallStart"),
        };
        assert!(!start_id.as_str().is_empty());

        // done event — also missing call_id, won't match the items map.
        // Should still emit a ToolCallDelta with a fresh id (graceful degradation).
        let ev2 = make_event(
            r#"{"type":"response.output_item.done","item":{"type":"function_call","name":"read","arguments":"{}"}}"#,
        );
        let e2 = state.parse_event(&ev2);
        assert_eq!(e2.len(), 1);
        match &e2[0] {
            StreamEvent::ToolCallDelta { arguments_delta, .. } => {
                assert_eq!(arguments_delta, "{}");
            }
            _ => panic!("expected ToolCallDelta"),
        }
    }

    #[test]
    fn parse_completed_with_usage() {
        let mut state = ResponsesStreamState::default();
        let ev = make_event(
            r#"{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":100,"output_tokens":50}}}"#,
        );
        let events = state.parse_event(&ev);
        assert_eq!(events.len(), 2);
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Usage(usage) if usage.input_tokens == 100 && usage.output_tokens == 50)));
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Done { stop_reason: StopReason::EndTurn })));
    }

    // --- endpoint() resolution tests ---

    #[test]
    fn endpoint_uses_default_when_nothing_set() {
        let p = OpenAiResponsesProvider::new("key");
        assert_eq!(p.endpoint(), "https://api.openai.com/v1/responses");
    }

    #[test]
    fn endpoint_falls_back_to_base_url_suffix() {
        let p = OpenAiResponsesProvider::new("key")
            .with_base_url("https://custom.openai-proxy.com");
        assert_eq!(
            p.endpoint(),
            "https://custom.openai-proxy.com/v1/responses"
        );
    }

    #[test]
    fn endpoint_uses_full_url_when_set() {
        let p = OpenAiResponsesProvider::new("key")
            .with_full_url("https://proxy.example.com/responses/v2?api-version=2024");
        assert_eq!(
            p.endpoint(),
            "https://proxy.example.com/responses/v2?api-version=2024"
        );
    }

    #[test]
    fn full_url_overrides_base_url() {
        let p = OpenAiResponsesProvider::new("key")
            .with_base_url("https://should-be-ignored.com/v1")
            .with_full_url("https://real-endpoint.com/api");
        assert_eq!(p.endpoint(), "https://real-endpoint.com/api");
    }
}
