//! Shared HTTP/SSE utilities for LLM providers.
//!
//! All providers use [`sse_event_stream`] to convert a `reqwest::Response`
//! into a stream of SSE events (with `data:` prefix and `[DONE]` handling
//! already done by `eventsource-stream`).

use eventsource_stream::Eventsource;
use futures::stream::{BoxStream, StreamExt};
use eventsource_stream::Event;

use crate::error::ProviderError;

/// Convert an HTTP response body into a stream of SSE events.
///
/// Each yielded `Event` has `.data` (the payload after `data: `) and
/// optionally `.event` (the event type, used by Anthropic's SSE format).
///
/// The stream ends when the server closes the connection. Errors during
/// reading or parsing are converted to [`ProviderError::Stream`].
pub fn sse_event_stream(
    response: reqwest::Response,
) -> BoxStream<'static, Result<Event, ProviderError>> {
    response
        .bytes_stream()
        .eventsource()
        .map(|result| {
            result.map_err(|e| ProviderError::Stream(format!("SSE parse error: {e}")))
        })
        .boxed()
}

/// Detect provider- or proxy-emitted error events embedded in an SSE stream.
///
/// Most LLM endpoints return an HTTP error status code when a request fails
/// before streaming begins (handled by each provider's HTTP status check).
/// However, some proxies and gateways (e.g. OpenAI-compatible proxies
/// routing to private backends) return HTTP 200 with the error embedded
/// as an SSE event. Without this helper, such errors are silently dropped
/// by the parsers' `_ => vec![]` fallback arms, making failures look like
/// empty responses.
///
/// Detected shapes:
///
/// - **Anthropic** (`event: error`): `{"type":"error","error":{"type":"...","message":"..."}}`
/// - **OpenAI Responses** (`event: error`): `{"type":"error","code":"...","message":"..."}`
/// - **Generic proxy** (no `event:` field): `{"error":{"message":"...","type":"RuntimeError"}}`
///
/// Returns `Some(ProviderError::Api { status: 0, message })` when an error
/// shape is detected, `None` for normal events that the caller should
/// hand to its parser. `status: 0` signals "status unknown; error arrived
/// mid-stream".
pub fn try_extract_error(event: &Event) -> Option<ProviderError> {
    // Shape 1: explicit `event: error` SSE event type (Anthropic / OpenAI Responses)
    if event.event == "error" {
        let payload: serde_json::Value = serde_json::from_str(&event.data).ok()?;
        let msg = payload
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|v| v.as_str())
            .or_else(|| payload.get("message").and_then(|v| v.as_str()))
            .unwrap_or("unknown streaming error");
        return Some(ProviderError::Api {
            status: 0,
            message: msg.to_string(),
        });
    }

    // Shape 2: no `event:` field, but the JSON payload has a top-level
    // `error` object with `message` / `type` / `code`. This is how some
    // proxies surface backend failures while pretending the stream is
    // healthy. We require the `error` value to look like an error object
    // (have at least one of those fields) to avoid false positives on
    // unrelated chunks.
    let payload: serde_json::Value = match serde_json::from_str(&event.data) {
        Ok(v) => v,
        Err(_) => return None,
    };
    if let Some(error) = payload.get("error") {
        let looks_like_error = error.is_object()
            && (error.get("message").is_some()
                || error.get("type").is_some()
                || error.get("code").is_some());
        if looks_like_error {
            let msg = error
                .get("message")
                .and_then(|v| v.as_str())
                .or_else(|| error.get("type").and_then(|v| v.as_str()))
                .or_else(|| error.get("code").and_then(|v| v.as_str()))
                .unwrap_or("unknown streaming error");
            return Some(ProviderError::Api {
                status: 0,
                message: msg.to_string(),
            });
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(event_type: &str, data: &str) -> Event {
        Event {
            event: event_type.to_string(),
            data: data.to_string(),
            id: String::new(),
            retry: None,
        }
    }

    #[test]
    fn detects_anthropic_error_event() {
        let ev = make_event(
            "error",
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        );
        let err = try_extract_error(&ev).expect("must detect");
        match err {
            ProviderError::Api { status, message } => {
                assert_eq!(status, 0);
                assert!(message.contains("Overloaded"));
            }
            other => panic!("expected Api, got {other:?}"),
        }
    }

    #[test]
    fn detects_responses_error_event_with_message_at_root() {
        // Some proxies put `message` at root instead of inside `error`.
        let ev = make_event("error", r#"{"type":"error","message":"backend failed"}"#);
        let err = try_extract_error(&ev).expect("must detect");
        match err {
            ProviderError::Api { status, message } => {
                assert_eq!(status, 0);
                assert_eq!(message, "backend failed");
            }
            other => panic!("expected Api, got {other:?}"),
        }
    }

    #[test]
    fn detects_generic_proxy_error_in_data() {
        // x666.me-style: no `event:` field, error embedded in data
        let ev = make_event(
            "message",
            r#"{"error":{"message":"/backend-anon/conversation failed: status=403, body=","type":"RuntimeError"}}"#,
        );
        let err = try_extract_error(&ev).expect("must detect");
        match err {
            ProviderError::Api { status, message } => {
                assert_eq!(status, 0);
                assert!(message.contains("backend-anon"));
            }
            other => panic!("expected Api, got {other:?}"),
        }
    }

    #[test]
    fn does_not_flag_normal_anthropic_message_delta() {
        let ev = make_event(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
        );
        assert!(try_extract_error(&ev).is_none());
    }

    #[test]
    fn does_not_flag_normal_responses_output_text_delta() {
        let ev = make_event(
            "message",
            r#"{"type":"response.output_text.delta","delta":"hello"}"#,
        );
        assert!(try_extract_error(&ev).is_none());
    }

    #[test]
    fn does_not_flag_chat_completions_chunk() {
        let ev = make_event(
            "message",
            r#"{"choices":[{"delta":{"content":"hi"},"index":0}]}"#,
        );
        assert!(try_extract_error(&ev).is_none());
    }

    #[test]
    fn does_not_flag_done_marker() {
        let ev = make_event("message", "[DONE]");
        assert!(try_extract_error(&ev).is_none());
    }

    #[test]
    fn does_not_flag_invalid_json() {
        let ev = make_event("message", "not json at all");
        assert!(try_extract_error(&ev).is_none());
    }
}
