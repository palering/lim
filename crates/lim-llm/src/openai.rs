//! OpenAI chat completions provider with SSE streaming.

use futures::stream::{BoxStream, StreamExt};
use reqwest::Client;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use lim_core::error::{LimError, Result};
use lim_core::id::ToolCallId;
use lim_core::stop_reason::StopReason;
use lim_core::tool::ToolSchema;
use lim_core::usage::Usage;

use crate::model::{ModelId, ModelMetadata};
use crate::provider::{LlmRequest, Provider};
use crate::stream::StreamEvent;

const OPENAI_API_URL: &str = "https://api.openai.com/v1/chat/completions";

/// OpenAI chat completions provider.
///
/// Streams responses from OpenAI's API (GPT-4o, etc.) as SSE events.
/// Requires `OPENAI_API_KEY` environment variable or explicit key in constructor.
pub struct OpenAiProvider {
    client: Client,
    api_key: String,
    model_id: ModelId,
    base_url: String,
}

impl OpenAiProvider {
    /// Create a provider reading the API key from `OPENAI_API_KEY` env var.
    pub fn from_env(model: impl Into<String>) -> Result<Self> {
        let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| {
            LimError::Provider("OPENAI_API_KEY environment variable not set".to_string())
        })?;
        Ok(Self::new(model, api_key))
    }

    /// Create a provider with an explicit API key.
    pub fn new(model: impl Into<String>, api_key: impl Into<String>) -> Self {
        let model_str = model.into();
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            model_id: ModelId::new(&model_str),
            base_url: OPENAI_API_URL.to_string(),
        }
    }

    /// Override the base URL (for Azure OpenAI, local proxies, etc.).
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Convert our LlmRequest to OpenAI's JSON body.
    fn build_request_body(&self, request: &LlmRequest) -> serde_json::Value {
        let messages: Vec<serde_json::Value> = request
            .messages
            .iter()
            .map(convert_message)
            .collect();

        let tools: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(convert_tool_schema)
            .collect();

        let mut body = serde_json::json!({
            "model": self.model_id.as_str(),
            "messages": messages,
            "stream": true,
            "stream_options": {"include_usage": true},
        });

        if !tools.is_empty() {
            body["tools"] = serde_json::Value::Array(tools);
        }

        if let Some(ref system) = request.system {
            // Prepend system message
            let sys_msg = serde_json::json!({"role": "system", "content": system});
            if let Some(arr) = body["messages"].as_array_mut() {
                arr.insert(0, sys_msg);
            }
        }

        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }

        body
    }

    /// Parse a single SSE data line into stream events.
    fn parse_chunk(data: &str) -> Vec<StreamEvent> {
        if data == "[DONE]" {
            return vec![StreamEvent::Done {
                stop_reason: StopReason::EndTurn,
            }];
        }

        let chunk: OpenAiChunk = match serde_json::from_str(data) {
            Ok(c) => c,
            Err(_) => return vec![],
        };

        let mut events = Vec::new();

        // Extract usage if present (arrives in final chunk with stream_options)
        if let Some(usage) = chunk.usage {
            events.push(StreamEvent::Usage(Usage::new(
                usage.prompt_tokens,
                usage.completion_tokens,
            )));
        }

        if let Some(choice) = chunk.choices.first() {
            let delta = &choice.delta;

            // Text content
            if let Some(ref content) = delta.content
                && !content.is_empty()
            {
                events.push(StreamEvent::TextDelta {
                    text: content.clone(),
                });
            }

            // Tool calls
            if let Some(ref tool_calls) = delta.tool_calls {
                for tc in tool_calls {
                    if let Some(ref _id) = tc.id {
                        // New tool call starting
                        let func_name = tc
                            .function
                            .as_ref()
                            .and_then(|f| f.name.as_ref())
                            .cloned()
                            .unwrap_or_default();
                        events.push(StreamEvent::ToolCallStart {
                            id: ToolCallId::new(),
                            name: func_name,
                        });
                    }
                    if let Some(ref func) = tc.function
                        && let Some(ref args) = func.arguments
                    {
                        // Use the tool call index as a proxy ID
                        // (OpenAI sends index-based deltas)
                        let tc_id = ToolCallId::new();
                        events.push(StreamEvent::ToolCallDelta {
                            id: tc_id,
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
                // Only emit Done if we haven't already from usage
                if events.is_empty()
                    || !events
                        .iter()
                        .any(|e| matches!(e, StreamEvent::Done { .. }))
                {
                    events.push(StreamEvent::Done {
                        stop_reason: stop,
                    });
                }
            }
        }

        events
    }
}

impl Provider for OpenAiProvider {
    fn stream<'a>(&'a self, request: LlmRequest) -> BoxStream<'a, Result<StreamEvent>> {
        let body = self.build_request_body(&request);
        let url = self.base_url.clone();
        let api_key = self.api_key.clone();
        let client = self.client.clone();

        let (tx, rx) = mpsc::channel(64);

        tokio::spawn(async move {
            let response = match client
                .post(&url)
                .header("Authorization", format!("Bearer {api_key}"))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx
                        .send(Err(LimError::Provider(format!(
                            "OpenAI request failed: {e}"
                        ))))
                        .await;
                    return;
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                let _ = tx
                    .send(Err(LimError::Provider(format!(
                        "OpenAI API error {status}: {text}"
                    ))))
                    .await;
                return;
            }

            let mut byte_stream = response.bytes_stream();
            let mut buffer = String::new();

            use futures::StreamExt;
            while let Some(chunk_result) = byte_stream.next().await {
                let bytes = match chunk_result {
                    Ok(b) => b,
                    Err(e) => {
                        let _ = tx
                            .send(Err(LimError::Provider(format!(
                                "stream read error: {e}"
                            ))))
                            .await;
                        return;
                    }
                };

                buffer.push_str(&String::from_utf8_lossy(&bytes));

                // Process complete SSE lines
                while let Some(newline_pos) = buffer.find('\n') {
                    let line = buffer[..newline_pos].trim().to_string();
                    buffer = buffer[newline_pos + 1..].to_string();

                    if let Some(data) = line.strip_prefix("data: ") {
                        let events = OpenAiProvider::parse_chunk(data.trim());
                        for event in events {
                            if tx.send(Ok(event)).await.is_err() {
                                return; // consumer dropped
                            }
                        }
                    }
                }
            }
        });

        ReceiverStream::new(rx).boxed()
    }

    fn model_metadata(&self) -> ModelMetadata {
        // Defaults for GPT-4o family; can be refined per model.
        let (ctx, max_out, thinking) = match self.model_id.as_str() {
            m if m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") => {
                (200_000, 100_000, true)
            }
            m if m.starts_with("gpt-4o") => (128_000, 16_384, false),
            m if m.starts_with("gpt-4.1") => (1_047_576, 32_768, false),
            _ => (128_000, 8_192, false),
        };

        ModelMetadata {
            id: self.model_id.clone(),
            display_name: format!("OpenAI {}", self.model_id),
            context_window: ctx,
            max_output_tokens: max_out,
            supports_tools: true,
            supports_thinking: thinking,
        }
    }
}

// --- OpenAI API response types ---

#[derive(Debug, Deserialize)]
struct OpenAiChunk {
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    delta: OpenAiDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiDelta {
    content: Option<String>,
    tool_calls: Option<Vec<OpenAiToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
struct OpenAiToolCallDelta {
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
}

// --- Conversion helpers ---

/// Convert our LlmMessage to OpenAI's message JSON format.
fn convert_message(msg: &lim_core::message::LlmMessage) -> serde_json::Value {
    use lim_core::content::ContentBlock;
    use lim_core::message::Role;

    match msg.role {
        Role::System => {
            let text = msg
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            serde_json::json!({"role": "system", "content": text})
        }
        Role::User => {
            // Check if we have images — if so, use multi-part content
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
                                "type": "image_url",
                                "image_url": {"url": format!("data:{mime_type};base64,{b64}")}
                            }))
                        }
                        _ => None,
                    })
                    .collect();
                serde_json::json!({"role": "user", "content": parts})
            } else {
                let text = msg
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                serde_json::json!({"role": "user", "content": text})
            }
        }
        Role::Assistant => {
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
                    ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } => Some(serde_json::json!({
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
            let text = msg
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");

            let tool_call_id = msg
                .tool_call_ids
                .first()
                .map(|id| id.to_string())
                .unwrap_or_default();

            serde_json::json!({
                "role": "tool",
                "content": text,
                "tool_call_id": tool_call_id,
            })
        }
    }
}

/// Convert our ToolSchema to OpenAI's function tool format.
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

    #[test]
    fn parse_text_chunk() {
        let data = r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#;
        let events = OpenAiProvider::parse_chunk(data);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::TextDelta { text } => assert_eq!(text, "Hello"),
            _ => panic!("expected TextDelta"),
        }
    }

    #[test]
    fn parse_done_chunk() {
        let events = OpenAiProvider::parse_chunk("[DONE]");
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            StreamEvent::Done {
                stop_reason: StopReason::EndTurn
            }
        ));
    }

    #[test]
    fn parse_usage_chunk() {
        let data = r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":20}}"#;
        let events = OpenAiProvider::parse_chunk(data);
        // Should have Usage + Done
        assert!(events.len() >= 2);
        assert!(events.iter().any(|e| matches!(e, StreamEvent::Usage(_))));
    }

    #[test]
    fn parse_tool_call_chunk() {
        let data = r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_abc","type":"function","function":{"name":"bash","arguments":"{\"cm"}}]},"finish_reason":null}]}"#;
        let events = OpenAiProvider::parse_chunk(data);
        assert!(!events.is_empty());
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolCallStart { .. })));
    }

    #[test]
    fn convert_user_message() {
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
    fn convert_assistant_with_tool_call() {
        let tc_id = lim_core::id::ToolCallId::new();
        let msg = LlmMessage {
            role: Role::Assistant,
            content: vec![
                ContentBlock::text("Let me check"),
                ContentBlock::tool_call(tc_id, "bash", serde_json::json!({"cmd": "ls"})),
            ],
            tool_call_ids: vec![],
        };
        let json = convert_message(&msg);
        assert_eq!(json["role"], "assistant");
        assert!(json["tool_calls"].is_array());
        assert_eq!(json["tool_calls"][0]["function"]["name"], "bash");
    }

    #[test]
    fn convert_tool_result_message() {
        let tc_id = lim_core::id::ToolCallId::new();
        let msg = LlmMessage {
            role: Role::Tool,
            content: vec![ContentBlock::text("file contents here")],
            tool_call_ids: vec![tc_id],
        };
        let json = convert_message(&msg);
        assert_eq!(json["role"], "tool");
        assert_eq!(json["content"], "file contents here");
        assert_eq!(json["tool_call_id"], tc_id.to_string());
    }

    #[test]
    fn convert_tool_schema() {
        let schema = ToolSchema {
            name: "read".to_string(),
            description: "Read a file".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
        };
        let json = super::convert_tool_schema(&schema);
        assert_eq!(json["type"], "function");
        assert_eq!(json["function"]["name"], "read");
    }

    #[test]
    fn model_metadata_defaults() {
        let provider = OpenAiProvider::new("gpt-4o", "test-key");
        let meta = provider.model_metadata();
        assert_eq!(meta.context_window, 128_000);
        assert!(meta.supports_tools);
        assert!(!meta.supports_thinking);
    }
}
