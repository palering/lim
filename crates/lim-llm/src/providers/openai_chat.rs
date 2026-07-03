//! OpenAI Chat Completions provider (`/v1/chat/completions`).
//!
//! This is the most widely-compatible API format: OpenAI official, Azure OpenAI,
//! and virtually all OpenAI-compatible proxies (OpenRouter, DeepSeek, Together,
//! Groq, local Ollama/LM Studio, etc.) support it.
//!
//! ## Tool call streaming
//!
//! OpenAI's Chat Completions sends tool call arguments as fragmented deltas
//! keyed by `index` (not `id`). The first delta for a tool call carries `id`
//! and `function.name`; subsequent deltas for the same tool call carry only
//! `index` and `function.arguments` fragments. We maintain an `index → ToolCallId`
//! map to correlate deltas and emit consistent `StreamEvent`s.

use std::collections::HashMap;

use futures::stream::{BoxStream, StreamExt};
use reqwest::Client;
use serde::Deserialize;
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

/// OpenAI Chat Completions provider.
///
/// Compatible with any endpoint that implements OpenAI's `/chat/completions`
/// API (OpenAI, Azure, OpenRouter, DeepSeek, local proxies, etc.).
pub struct OpenAiChatProvider {
    client: Client,
    api_key: String,
    base_url: String,
    /// Complete endpoint URL that overrides `base_url + suffix` when set.
    /// Use for non-standard providers (Azure, private proxies with custom
    /// paths, endpoints with query params, etc.).
    full_url: Option<String>,
    extra_headers: HashMap<String, String>,
}

impl OpenAiChatProvider {
    /// Create a provider with an explicit API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            full_url: None,
            extra_headers: HashMap::new(),
        }
    }

    /// Override the base URL (for proxies, Azure, OpenRouter, etc.).
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Override the full endpoint URL, bypassing `base_url + /chat/completions`
    /// concatenation. When set, `endpoint()` returns this URL verbatim.
    ///
    /// Use cases: Azure OpenAI (URL contains deployment name + query params),
    /// private proxies with custom paths, non-standard endpoint layouts.
    pub fn with_full_url(mut self, url: impl Into<String>) -> Self {
        self.full_url = Some(url.into());
        self
    }

    /// Add extra headers (e.g. `HTTP-Referer` for OpenRouter).
    pub fn with_extra_headers(mut self, headers: HashMap<String, String>) -> Self {
        self.extra_headers = headers;
        self
    }

    /// Build the JSON request body for `/chat/completions`.
    fn build_request_body(&self, request: &LlmRequest) -> serde_json::Value {
        let mut messages: Vec<serde_json::Value> = Vec::new();

        // System prompt as first message if present
        if let Some(ref system) = request.system {
            messages.push(serde_json::json!({
                "role": "system",
                "content": system,
            }));
        }

        // Convert conversation messages
        for msg in &request.messages {
            messages.push(convert_message(msg));
        }

        let mut body = serde_json::json!({
            "model": request.model,
            "messages": messages,
            "stream": true,
            "stream_options": {"include_usage": true},
        });

        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> =
                request.tools.iter().map(convert_tool_schema).collect();
            body["tools"] = serde_json::Value::Array(tools);
        }

        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }

        body
    }

    /// Endpoint URL for chat completions.
    ///
    /// If `full_url` is set, it is returned verbatim (bypasses concatenation).
    /// Otherwise, `{base_url}/v1/chat/completions`.
    ///
    /// `base_url` should be the host only (e.g. `https://api.openai.com`),
    /// without a trailing `/v1` — the version segment is part of the
    /// endpoint suffix. If you configure a `base_url` ending in `/v1`,
    /// a startup warning will remind you to drop it.
    fn endpoint(&self) -> String {
        if let Some(ref full) = self.full_url {
            return full.clone();
        }
        let base = self.base_url.trim_end_matches('/');
        format!("{base}/v1/chat/completions")
    }
}

impl Provider for OpenAiChatProvider {
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
            let model = body.get("model").and_then(|v| v.as_str()).unwrap_or("?").to_string();
            let msg_count =
                body.get("messages").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
            tracing::debug!(
                url = %url,
                model = %model,
                message_count = msg_count,
                "openai chat: sending request"
            );
            tracing::trace!(body = ?body, "openai chat request body");

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

            // Check HTTP status before streaming
            if !response.status().is_success() {
                let status = response.status().as_u16();
                let text = response.text().await.unwrap_or_default();
                tracing::debug!(status, body = %text, "openai chat: HTTP error response");
                let _ = tx.send(Err(classify_http_error(status, &text))).await;
                return;
            }

            tracing::debug!(status = response.status().as_u16(), "openai chat: stream started");
            let mut state = ChatStreamState::default();
            let mut stream = sse_event_stream(response);

            while let Some(result) = stream.next().await {
                match result {
                    Ok(event) => {
                        tracing::trace!(
                            event_type = %event.event,
                            data_len = event.data.len(),
                            "openai chat SSE event"
                        );
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
                                return; // consumer dropped
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
        // OpenAI o-series + OpenAI-compatible thinking models all stream
        // reasoning via `delta.reasoning_content`. Metadata here only flags
        // capability; actual parsing in `parse_event` is unconditional, so
        // unknown thinking models still work — they just report
        // `supports_thinking: false` until added here.
        let (ctx, max_out, thinking) = match model {
            // OpenAI o-series
            m if m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") => {
                (200_000, 100_000, true)
            }
            // DeepSeek-reasoner (R1 / V3 thinking)
            m if m.starts_with("deepseek-reasoner") => (64_000, 32_768, true),
            // Qwen3 thinking variants (qwq, qwen3-reasoning, etc.)
            m if m.starts_with("qwq") || m.contains("-thinking") => (131_072, 32_768, true),
            // GLM-4.5+ thinking variants
            m if m.starts_with("glm-4.5") || m.starts_with("glm-4.6") => (128_000, 32_768, true),
            // Kimi K2 thinking
            m if m.starts_with("kimi-k2") => (128_000, 32_768, true),
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

/// Stateful parser for OpenAI Chat Completions SSE stream.
///
/// Maintains the `index → ToolCallId` mapping across chunks so that
/// `ToolCallDelta` events carry the same ID as their corresponding
/// `ToolCallStart`.
#[derive(Default)]
struct ChatStreamState {
    /// OpenAI `tool_calls[].index` → lim `ToolCallId`
    index_to_id: HashMap<usize, ToolCallId>,
}

impl ChatStreamState {
    /// Parse one SSE event into zero or more `StreamEvent`s.
    fn parse_event(&mut self, event: &eventsource_stream::Event) -> Vec<StreamEvent> {
        let data = &event.data;

        if data == "[DONE]" {
            return vec![StreamEvent::Done { stop_reason: StopReason::EndTurn }];
        }

        let chunk: OpenAiChunk = match serde_json::from_str(data) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    data_len = data.len(),
                    error = %e,
                    "openai chat SSE: failed to parse JSON chunk, dropping event"
                );
                return vec![];
            }
        };

        let mut events = Vec::new();

        // Usage (arrives in final chunk when stream_options.include_usage = true)
        if let Some(usage) = chunk.usage {
            let cached = usage.prompt_tokens_details.as_ref().map(|d| d.cached_tokens).unwrap_or(0);
            // OpenAI semantics: prompt_tokens INCLUDES cached_tokens (subset,
            // not additive). We keep input_tokens = prompt_tokens to preserve
            // the provider's original accounting; consumers can read
            // `cached_tokens` separately. See Usage docs.
            events.push(StreamEvent::Usage(Usage::with_cache(
                usage.prompt_tokens,
                usage.completion_tokens,
                cached,
            )));
        }

        if let Some(choice) = chunk.choices.first() {
            let delta = &choice.delta;

            // Text content
            if let Some(ref content) = delta.content
                && !content.is_empty()
            {
                events.push(StreamEvent::TextDelta { text: content.clone() });
            }

            // Reasoning content (o-series + OpenAI-compatible thinking models:
            // DeepSeek-reasoner, Qwen3, GLM-4.5, Kimi, etc.). Streamed in
            // parallel to `content` and surfaced as ThinkingDelta so downstream
            // can render it separately from the final answer.
            if let Some(ref reasoning) = delta.reasoning_content
                && !reasoning.is_empty()
            {
                events.push(StreamEvent::ThinkingDelta { text: reasoning.clone() });
            }

            // Tool calls — keyed by `index`, not `id`
            if let Some(ref tool_calls) = delta.tool_calls {
                for tc in tool_calls {
                    let index = tc.index.unwrap_or(0) as usize;

                    // First time seeing this index: use the OpenAI-supplied id
                    // if present (e.g. "call_abc") so that subsequent tool_result
                    // messages can echo it back via `tool_call_id`. Fall back to
                    // a generated Ulid only if the provider omits the id.
                    let id = self
                        .index_to_id
                        .entry(index)
                        .or_insert_with(|| {
                            tc.id
                                .as_deref()
                                .map(ToolCallId::from_string)
                                .unwrap_or_else(ToolCallId::new)
                        })
                        .clone();

                    // If this delta has an `id` or `function.name`, it's the
                    // first chunk for this tool call → emit ToolCallStart
                    let has_id = tc.id.is_some();
                    let name = tc.function.as_ref().and_then(|f| f.name.clone());

                    if has_id || name.is_some() {
                        events.push(StreamEvent::ToolCallStart { id: id.clone(), name });
                    }

                    // Arguments delta (fragment of JSON string)
                    if let Some(ref func) = tc.function
                        && let Some(ref args) = func.arguments
                        && !args.is_empty()
                    {
                        events.push(StreamEvent::ToolCallDelta {
                            id: id.clone(),
                            arguments_delta: args.clone(),
                        });
                    }
                }
            }

            // Finish reason
            if let Some(ref finish) = choice.finish_reason {
                let stop = match finish.as_str() {
                    "tool_calls" => StopReason::ToolCall,
                    "length" => StopReason::MaxTokens,
                    _ => StopReason::EndTurn,
                };
                events.push(StreamEvent::Done { stop_reason: stop });
            }
        }

        events
    }
}

// --- OpenAI API response types ---

#[derive(Debug, Deserialize)]
struct OpenAiChunk {
    #[serde(default)]
    choices: Vec<OpenAiChoice>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    delta: OpenAiDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenAiDelta {
    #[serde(default)]
    content: Option<String>,
    /// Reasoning / thinking content emitted by o-series and OpenAI-compatible
    /// thinking models (DeepSeek-reasoner, Qwen3, GLM-4.5, Kimi, etc.).
    /// Streamed via `delta.reasoning_content`, parallel to `delta.content`.
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAiToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
struct OpenAiToolCallDelta {
    index: Option<u64>,
    id: Option<String>,
    function: Option<OpenAiFunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct OpenAiFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    /// OpenAI reports cached prompt tokens as a sub-object when prompt
    /// caching is active. The cached portion is a subset of
    /// `prompt_tokens` (not additive).
    #[serde(default)]
    prompt_tokens_details: Option<OpenAiPromptTokensDetails>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenAiPromptTokensDetails {
    #[serde(default)]
    cached_tokens: u64,
}

// --- Helpers ---

/// Convert an HTTP error status + body into a structured `ProviderError`.
fn classify_http_error(status: u16, body: &str) -> ProviderError {
    match status {
        401 | 403 => ProviderError::Auth(body.to_string()),
        429 => ProviderError::RateLimited { retry_after_ms: None },
        400 => {
            if body.contains("context_length") || body.contains("context window") {
                ProviderError::ContextWindowExceeded
            } else {
                ProviderError::InvalidRequest(body.to_string())
            }
        }
        402 => ProviderError::QuotaExceeded,
        502 | 503 => ProviderError::ServerOverloaded,
        _ => ProviderError::Api { status, message: body.to_string() },
    }
}

/// Convert our `LlmMessage` to OpenAI's message JSON format.
fn convert_message(msg: &lim_core::message::LlmMessage) -> serde_json::Value {
    use lim_core::content::ContentBlock;
    use lim_core::message::Role;

    match msg.role {
        Role::System => {
            let text = extract_text(&msg.content);
            serde_json::json!({"role": "system", "content": text})
        }
        Role::User => {
            let has_image = msg.content.iter().any(|b| matches!(b, ContentBlock::Image { .. }));

            if has_image {
                let parts: Vec<serde_json::Value> = msg
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => {
                            Some(serde_json::json!({"type": "text", "text": text}))
                        }
                        ContentBlock::Image { data, mime_type } => {
                            use base64::{Engine, engine::general_purpose::STANDARD};
                            let b64 = STANDARD.encode(data);
                            Some(serde_json::json!({
                                "type": "image_url",
                                "image_url": {"url": format!("data:{mime_type};base64,{b64}")}
                            }))
                        }
                        _ => None,
                    })
                    .collect();
                serde_json::json!({"role": "user", "content": parts})
            } else {
                let text = extract_text(&msg.content);
                serde_json::json!({"role": "user", "content": text})
            }
        }
        Role::Assistant => {
            // Note: `ContentBlock::Thinking` is intentionally NOT serialized
            // back to the API. All thinking models (o-series, DeepSeek-reasoner,
            // Qwen3, GLM, Kimi) treat `reasoning_content` as ephemeral — it is
            // regenerated each turn and must not be echoed back. Dropping it
            // here matches every compatible provider's documented contract.
            let text_parts: Vec<&str> = msg
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();

            let tool_calls: Vec<serde_json::Value> = msg
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolCall { id, name, arguments } => Some(serde_json::json!({
                        "id": id.to_string(),
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": arguments.to_string(),
                        }
                    })),
                    _ => None,
                })
                .collect();

            let mut obj = serde_json::json!({
                "role": "assistant",
                "content": text_parts.join("\n"),
            });

            if !tool_calls.is_empty() {
                obj["tool_calls"] = serde_json::Value::Array(tool_calls);
            }

            obj
        }
        Role::Tool => {
            let text = extract_text(&msg.content);
            let tool_call_id =
                msg.tool_call_ids.first().map(|id| id.to_string()).unwrap_or_default();

            serde_json::json!({
                "role": "tool",
                "content": text,
                "tool_call_id": tool_call_id,
            })
        }
    }
}

/// Extract all text blocks joined by newline.
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

/// Convert our `ToolSchema` to OpenAI's function tool format.
fn convert_tool_schema(schema: &ToolSchema) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": schema.name,
            "description": schema.description,
            "parameters": schema.parameters,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lim_core::content::ContentBlock;
    use lim_core::message::{LlmMessage, Role};

    /// Helper: feed a raw SSE `data:` payload through the stateful parser.
    fn parse(data: &str, state: &mut ChatStreamState) -> Vec<StreamEvent> {
        let event = eventsource_stream::Event {
            event: "message".to_string(),
            data: data.to_string(),
            id: String::new(),
            retry: None,
        };
        state.parse_event(&event)
    }

    #[test]
    fn parse_text_delta() {
        let data = r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#;
        let mut state = ChatStreamState::default();
        let events = parse(data, &mut state);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::TextDelta { text } => assert_eq!(text, "Hello"),
            _ => panic!("expected TextDelta"),
        }
    }

    #[test]
    fn parse_reasoning_content_delta() {
        // OpenAI-compatible thinking models (o-series, DeepSeek-reasoner,
        // Qwen3, GLM-4.5, Kimi, etc.) stream thinking via `reasoning_content`,
        // parallel to `content`. We emit it as ThinkingDelta so the UI can
        // render it separately from the final answer.
        let data = r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"reasoning_content":"Let me think..."},"finish_reason":null}]}"#;
        let mut state = ChatStreamState::default();
        let events = parse(data, &mut state);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ThinkingDelta { text } => assert_eq!(text, "Let me think..."),
            _ => panic!("expected ThinkingDelta"),
        }
    }

    #[test]
    fn parse_reasoning_and_text_in_same_chunk() {
        // Some providers emit reasoning_content and content in the same chunk
        // (e.g. when content starts immediately after thinking ends). Both
        // events should be emitted — order in the events vector follows the
        // parser's check order (content first, then reasoning_content).
        let data = r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"reasoning_content":"thinking...","content":"answer"},"finish_reason":null}]}"#;
        let mut state = ChatStreamState::default();
        let events = parse(data, &mut state);
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], StreamEvent::TextDelta { .. }));
        assert!(matches!(events[1], StreamEvent::ThinkingDelta { .. }));
    }

    #[test]
    fn parse_done() {
        let mut state = ChatStreamState::default();
        let events = parse("[DONE]", &mut state);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], StreamEvent::Done { stop_reason: StopReason::EndTurn }));
    }

    #[test]
    fn parse_usage_and_finish() {
        let data = r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":20}}"#;
        let mut state = ChatStreamState::default();
        let events = parse(data, &mut state);
        // Usage + Done
        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|e| matches!(e, StreamEvent::Usage(_))));
        assert!(events.iter().any(|e| matches!(e, StreamEvent::Done { .. })));
    }

    #[test]
    fn parse_usage_with_cache_details() {
        // OpenAI prompt caching: prompt_tokens_details.cached_tokens is a
        // SUBSET of prompt_tokens (not additive). Here 200 input includes
        // 100 cached.
        let data = r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":200,"completion_tokens":50,"prompt_tokens_details":{"cached_tokens":100}}}"#;
        let mut state = ChatStreamState::default();
        let events = parse(data, &mut state);
        let usage_ev = events.iter().find_map(|e| match e {
            StreamEvent::Usage(u) => Some(u),
            _ => None,
        });
        let u = usage_ev.expect("usage event should be present");
        assert_eq!(u.input_tokens, 200, "prompt_tokens → input_tokens (includes cached)");
        assert_eq!(u.output_tokens, 50);
        assert_eq!(u.cached_tokens, 100, "cached subset extracted");
        assert_eq!(u.cache_creation_tokens, 0, "OpenAI has no explicit cache creation");
    }

    /// Reproduces the original P0 bug: tool call deltas must carry the same
    /// ToolCallId as the initial ToolCallStart.
    #[test]
    fn tool_call_delta_uses_consistent_id() {
        let mut state = ChatStreamState::default();

        // First chunk: tool call start (index=0, id="call_abc", name="bash")
        let chunk1 = r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_abc","type":"function","function":{"name":"bash","arguments":""}}]},"finish_reason":null}]}"#;
        let events1 = parse(chunk1, &mut state);
        assert_eq!(events1.len(), 1);
        let start_id = match &events1[0] {
            StreamEvent::ToolCallStart { id, name } => {
                assert_eq!(name.as_deref(), Some("bash"));
                // The provider-supplied id must be preserved verbatim.
                assert_eq!(id.as_str(), "call_abc", "must preserve OpenAI's id");
                id.clone()
            }
            _ => panic!("expected ToolCallStart"),
        };

        // Second chunk: arguments delta (index=0, no id, partial arguments)
        let chunk2 = r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"cmd\":"}}]},"finish_reason":null}]}"#;
        let events2 = parse(chunk2, &mut state);
        assert_eq!(events2.len(), 1);
        match &events2[0] {
            StreamEvent::ToolCallDelta { id, arguments_delta } => {
                assert_eq!(*id, start_id, "delta must use the same id as start");
                assert_eq!(arguments_delta, "{\"cmd\":");
            }
            _ => panic!("expected ToolCallDelta"),
        }

        // Third chunk: more arguments
        let chunk3 = r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"ls\"}"}}]},"finish_reason":null}]}"#;
        let events3 = parse(chunk3, &mut state);
        assert_eq!(events3.len(), 1);
        match &events3[0] {
            StreamEvent::ToolCallDelta { id, .. } => {
                assert_eq!(*id, start_id);
            }
            _ => panic!("expected ToolCallDelta"),
        }

        // Fourth chunk: finish_reason = "tool_calls"
        let chunk4 = r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#;
        let events4 = parse(chunk4, &mut state);
        assert!(
            events4
                .iter()
                .any(|e| matches!(e, StreamEvent::Done { stop_reason: StopReason::ToolCall }))
        );
    }

    /// When the provider omits the `id` field (some proxies do), we fall
    /// back to a generated Ulid-based id. The important property is that
    /// ToolCallStart and ToolCallDelta still share the same id.
    #[test]
    fn tool_call_falls_back_to_generated_id_when_missing() {
        let mut state = ChatStreamState::default();

        // No "id" field in the tool call delta
        let chunk1 = r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"read","arguments":""}}]},"finish_reason":null}]}"#;
        let events1 = parse(chunk1, &mut state);
        assert_eq!(events1.len(), 1);
        let start_id = match &events1[0] {
            StreamEvent::ToolCallStart { id, .. } => id.clone(),
            _ => panic!("expected ToolCallStart"),
        };
        // Should be a generated Ulid, not empty
        assert!(!start_id.as_str().is_empty());

        // Subsequent delta should reuse the same id
        let chunk2 = r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"p\":1}"}}]},"finish_reason":null}]}"#;
        let events2 = parse(chunk2, &mut state);
        match &events2[0] {
            StreamEvent::ToolCallDelta { id, .. } => assert_eq!(*id, start_id),
            _ => panic!("expected ToolCallDelta"),
        }
    }

    /// Multiple tool calls in parallel: each gets its own id, deltas routed correctly.
    #[test]
    fn multiple_parallel_tool_calls() {
        let mut state = ChatStreamState::default();

        // Start tool call 0
        let c1 = r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"read"}}]},"finish_reason":null}]}"#;
        let e1 = parse(c1, &mut state);
        let id_a = match &e1[0] {
            StreamEvent::ToolCallStart { id, .. } => id.clone(),
            _ => panic!(),
        };

        // Start tool call 1
        let c2 = r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"write"}}]},"finish_reason":null}]}"#;
        let e2 = parse(c2, &mut state);
        let id_b = match &e2[0] {
            StreamEvent::ToolCallStart { id, .. } => id.clone(),
            _ => panic!(),
        };
        assert_ne!(id_a, id_b);

        // Delta for tool call 0
        let c3 = r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"p\":1}"}}]},"finish_reason":null}]}"#;
        let e3 = parse(c3, &mut state);
        match &e3[0] {
            StreamEvent::ToolCallDelta { id, .. } => assert_eq!(*id, id_a),
            _ => panic!(),
        }

        // Delta for tool call 1
        let c4 = r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"p\":2}"}}]},"finish_reason":null}]}"#;
        let e4 = parse(c4, &mut state);
        match &e4[0] {
            StreamEvent::ToolCallDelta { id, .. } => assert_eq!(*id, id_b),
            _ => panic!(),
        }
    }

    #[test]
    fn convert_user_message_basic() {
        let msg = LlmMessage {
            role: Role::User,
            content: vec![ContentBlock::text("Hello")],
            tool_call_ids: vec![],
        };
        let json = convert_message(&msg);
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"], "Hello");
    }

    #[test]
    fn convert_tool_schema_format() {
        let schema = ToolSchema {
            name: "read".to_string(),
            description: "Read a file".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
        };
        let json = convert_tool_schema(&schema);
        assert_eq!(json["type"], "function");
        assert_eq!(json["function"]["name"], "read");
    }

    #[test]
    fn http_error_classification() {
        assert!(matches!(classify_http_error(401, "bad key"), ProviderError::Auth(_)));
        assert!(matches!(classify_http_error(429, "slow down"), ProviderError::RateLimited { .. }));
        assert!(matches!(
            classify_http_error(400, "context_length exceeded"),
            ProviderError::ContextWindowExceeded
        ));
        assert!(matches!(classify_http_error(503, "overloaded"), ProviderError::ServerOverloaded));
    }

    #[test]
    fn model_metadata_for_gpt4o() {
        let provider = OpenAiChatProvider::new("test-key");
        let meta = provider.model_metadata("gpt-4o");
        assert_eq!(meta.context_window, 128_000);
        assert!(meta.supports_tools);
        assert!(!meta.supports_thinking);
    }

    #[test]
    fn model_metadata_for_o3() {
        let provider = OpenAiChatProvider::new("test-key");
        let meta = provider.model_metadata("o3-mini");
        assert_eq!(meta.context_window, 200_000);
        assert!(meta.supports_thinking);
    }

    #[test]
    fn model_metadata_for_compatible_thinking_models() {
        let provider = OpenAiChatProvider::new("test-key");
        // DeepSeek-reasoner
        let meta = provider.model_metadata("deepseek-reasoner");
        assert!(meta.supports_thinking);
        // Qwen3 qwq
        let meta = provider.model_metadata("qwq-32b");
        assert!(meta.supports_thinking);
        // GLM-4.5
        let meta = provider.model_metadata("glm-4.5");
        assert!(meta.supports_thinking);
        // Kimi K2
        let meta = provider.model_metadata("kimi-k2");
        assert!(meta.supports_thinking);
        // Non-thinking model stays false
        let meta = provider.model_metadata("deepseek-chat");
        assert!(!meta.supports_thinking);
    }

    // --- endpoint() resolution tests ---

    #[test]
    fn endpoint_uses_default_when_nothing_set() {
        let p = OpenAiChatProvider::new("key");
        assert_eq!(p.endpoint(), "https://api.openai.com/v1/chat/completions");
    }

    #[test]
    fn endpoint_falls_back_to_base_url_suffix() {
        let p = OpenAiChatProvider::new("key").with_base_url("https://api.deepseek.com");
        assert_eq!(p.endpoint(), "https://api.deepseek.com/v1/chat/completions");
    }

    #[test]
    fn endpoint_uses_full_url_when_set() {
        let p = OpenAiChatProvider::new("key")
            .with_full_url("https://proxy.example.com/custom/path?key=1");
        assert_eq!(p.endpoint(), "https://proxy.example.com/custom/path?key=1");
    }

    #[test]
    fn full_url_overrides_base_url() {
        // When both are set, full_url wins (base_url is ignored entirely)
        let p = OpenAiChatProvider::new("key")
            .with_base_url("https://should-be-ignored.com")
            .with_full_url("https://real-endpoint.com/api");
        assert_eq!(p.endpoint(), "https://real-endpoint.com/api");
    }

    #[test]
    fn endpoint_trims_trailing_slash_from_base_url() {
        let p = OpenAiChatProvider::new("key").with_base_url("https://api.deepseek.com/");
        assert_eq!(p.endpoint(), "https://api.deepseek.com/v1/chat/completions");
    }
}
