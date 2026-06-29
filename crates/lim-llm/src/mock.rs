use futures::stream::{self, BoxStream, StreamExt};

use lim_core::id::ToolCallId;
use lim_core::stop_reason::StopReason;
use lim_core::usage::Usage;

use crate::error::ProviderError;
use crate::model::{ModelId, ModelMetadata};
use crate::provider::{LlmRequest, Provider};
use crate::stream::StreamEvent;

/// A scripted response that the [`MockProvider`] can return.
///
/// Used for deterministic testing of the agent loop without a real LLM.
#[derive(Debug, Clone)]
pub enum MockResponse {
    /// Simple text response (no tool calls).
    Text(String),

    /// Response with tool calls.
    ToolCalls {
        /// Optional text before the tool calls.
        text: Option<String>,
        /// Tool calls to emit.
        calls: Vec<MockToolCall>,
    },
}

/// A tool call in a mock response.
#[derive(Debug, Clone)]
pub struct MockToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

impl MockToolCall {
    pub fn new(name: impl Into<String>, arguments: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            arguments,
        }
    }
}

/// A mock LLM provider for testing.
///
/// Returns scripted responses from a queue. Each call to `stream()` pops
/// the next response. If the queue is empty, returns an error.
pub struct MockProvider {
    responses: std::sync::Mutex<Vec<MockResponse>>,
}

impl MockProvider {
    /// Create a mock provider with a sequence of scripted responses.
    pub fn new(responses: Vec<MockResponse>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses),
        }
    }

    /// Create an empty mock provider (useful for manual test setup).
    pub fn empty() -> Self {
        Self::new(Vec::new())
    }
}

impl Provider for MockProvider {
    fn stream<'a>(
        &'a self,
        _request: LlmRequest,
    ) -> BoxStream<'a, Result<StreamEvent, ProviderError>> {
        let response = {
            let mut queue = self.responses.lock().unwrap();
            if queue.is_empty() {
                None
            } else {
                Some(queue.remove(0))
            }
        };

        match response {
            None => {
                let err = ProviderError::Stream(
                    "MockProvider: no more scripted responses".to_string(),
                );
                stream::once(async move { Err(err) }).boxed()
            }
            Some(MockResponse::Text(text)) => {
                let events = vec![
                    Ok(StreamEvent::TextDelta { text }),
                    Ok(StreamEvent::Usage(Usage::new(10, 20))),
                    Ok(StreamEvent::Done {
                        stop_reason: StopReason::EndTurn,
                    }),
                ];
                stream::iter(events).boxed()
            }
            Some(MockResponse::ToolCalls { text, calls }) => {
                let mut events: Vec<Result<StreamEvent, ProviderError>> = Vec::new();

                if let Some(text) = text {
                    events.push(Ok(StreamEvent::TextDelta { text }));
                }

                for call in calls {
                    let tc_id = ToolCallId::new();
                    events.push(Ok(StreamEvent::ToolCallStart {
                        id: tc_id.clone(),
                        name: Some(call.name),
                    }));
                    events.push(Ok(StreamEvent::ToolCallDelta {
                        id: tc_id,
                        arguments_delta: call.arguments.to_string(),
                    }));
                }

                events.push(Ok(StreamEvent::Usage(Usage::new(15, 25))));
                events.push(Ok(StreamEvent::Done {
                    stop_reason: StopReason::ToolCall,
                }));

                stream::iter(events).boxed()
            }
        }
    }

    fn model_metadata(&self, _model: &str) -> ModelMetadata {
        ModelMetadata {
            id: ModelId::new("mock-test"),
            display_name: "Mock Provider".to_string(),
            context_window: 128_000,
            max_output_tokens: 8_192,
            supports_tools: true,
            supports_thinking: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lim_core::content::ContentBlock;
    use lim_core::message::LlmMessage;

    #[tokio::test]
    async fn mock_text_response() {
        let provider = MockProvider::new(vec![MockResponse::Text("Hello!".to_string())]);
        let request = LlmRequest::new(
            "mock",
            vec![LlmMessage {
                role: lim_core::message::Role::User,
                content: vec![ContentBlock::text("hi")],
                tool_call_ids: vec![],
            }],
        );

        let events: Vec<_> = provider.stream(request).collect().await;
        assert_eq!(events.len(), 3);
        assert!(events[0].is_ok());
        assert!(events[2].is_ok());

        match &events[2] {
            Ok(StreamEvent::Done { stop_reason }) => {
                assert_eq!(*stop_reason, StopReason::EndTurn);
            }
            _ => panic!("expected Done event"),
        }
    }

    #[tokio::test]
    async fn mock_tool_call_response() {
        let provider = MockProvider::new(vec![MockResponse::ToolCalls {
            text: Some("Let me check".to_string()),
            calls: vec![MockToolCall::new("bash", serde_json::json!({"cmd": "ls"}))],
        }]);

        let request = LlmRequest::new("mock", vec![]);
        let events: Vec<_> = provider.stream(request).collect().await;

        // text + tool_call_start + tool_call_delta + usage + done = 5
        assert_eq!(events.len(), 5);

        // Verify tool call start
        match &events[1] {
            Ok(StreamEvent::ToolCallStart { name, .. }) => {
                assert_eq!(name.as_deref(), Some("bash"));
            }
            _ => panic!("expected ToolCallStart"),
        }
    }

    #[tokio::test]
    async fn mock_empty_provider_errors() {
        let provider = MockProvider::empty();
        let request = LlmRequest::new("mock", vec![]);
        let events: Vec<_> = provider.stream(request).collect().await;
        assert_eq!(events.len(), 1);
        assert!(events[0].is_err());
    }
}
