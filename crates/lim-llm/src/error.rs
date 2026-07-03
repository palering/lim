//! Provider error types.
//!
//! Structured errors that allow the agent loop to make retry/recovery decisions
//! based on the error category (rate limit, auth, context window, etc.).
//!
//! ## Source chain
//!
//! `ProviderError::Transport` preserves the underlying `reqwest::Error` via
//! `#[source]`, so `Error::source()` walks back to the root network cause.
//! This type is **not `Clone`** because `reqwest::Error` is not `Clone`;
//! callers that need to retain a copy should convert to `LimError::Provider`
//! (which flattens to `String`) at the appropriate boundary.

use thiserror::Error;

/// Errors returned by LLM providers.
#[derive(Debug, Error)]
pub enum ProviderError {
    /// Network or transport-level error (timeout, connection refused, etc.).
    ///
    /// `context` is a short label ("timeout", "connection", "transport");
    /// `source` preserves the original `reqwest::Error` for debugging.
    #[error("transport error: {context}")]
    Transport {
        context: String,
        #[source]
        source: reqwest::Error,
    },

    /// Authentication failed (401 / 403).
    #[error("auth error: {0}")]
    Auth(String),

    /// Rate limited (429). `retry_after_ms` is the server-suggested wait time.
    #[error("rate limited, retry after {retry_after_ms:?}ms")]
    RateLimited { retry_after_ms: Option<u64> },

    /// The request exceeded the model's context window.
    #[error("context window exceeded")]
    ContextWindowExceeded,

    /// API quota or billing limit reached.
    #[error("quota exceeded")]
    QuotaExceeded,

    /// Server-side overload (502 / 503), typically retryable.
    #[error("server overloaded, retryable")]
    ServerOverloaded,

    /// The request was malformed or rejected (400).
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// Error while parsing the SSE stream (malformed data, unexpected EOF).
    #[error("stream error: {0}")]
    Stream(String),

    /// Other API errors with an HTTP status code and message.
    #[error("api error: status {status}: {message}")]
    Api { status: u16, message: String },
}

impl ProviderError {
    /// Whether the agent loop should consider retrying this request.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ProviderError::RateLimited { .. }
                | ProviderError::ServerOverloaded
                | ProviderError::Transport { .. }
        )
    }
}

impl From<reqwest::Error> for ProviderError {
    fn from(err: reqwest::Error) -> Self {
        let context = if err.is_timeout() {
            "timeout"
        } else if err.is_connect() {
            "connection"
        } else {
            "transport"
        };
        ProviderError::Transport { context: context.to_string(), source: err }
    }
}

impl From<lim_core::error::LimError> for ProviderError {
    fn from(err: lim_core::error::LimError) -> Self {
        // Map LimError variants to the closest ProviderError category.
        // Most LimError variants are not provider-related; we default to
        // Stream so the original message is preserved via Display.
        match err {
            lim_core::error::LimError::Cancelled => {
                ProviderError::Stream("operation cancelled".into())
            }
            ref other => ProviderError::Stream(other.to_string()),
        }
    }
}

impl From<ProviderError> for lim_core::error::LimError {
    fn from(err: ProviderError) -> Self {
        // ProviderError is flattened to String here because lim-core cannot
        // depend on lim-llm (architectural layering). The Display impl of
        // ProviderError carries enough information for human-readable output.
        // Callers needing structured access (e.g. is_retryable) should
        // inspect ProviderError before it crosses this boundary.
        lim_core::error::LimError::Provider(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_classification() {
        assert!(ProviderError::RateLimited { retry_after_ms: None }.is_retryable());
        assert!(ProviderError::ServerOverloaded.is_retryable());
        // Transport requires a reqwest::Error; we verify is_retryable logic
        // via the variant pattern, not construction, in unit tests.
        assert!(!ProviderError::Auth("bad key".into()).is_retryable());
        assert!(!ProviderError::ContextWindowExceeded.is_retryable());
        assert!(!ProviderError::InvalidRequest("bad".into()).is_retryable());
    }

    #[test]
    fn cancelled_maps_to_stream() {
        let pe: ProviderError = lim_core::error::LimError::Cancelled.into();
        match pe {
            ProviderError::Stream(msg) => assert_eq!(msg, "operation cancelled"),
            _ => panic!("expected Stream variant"),
        }
    }

    #[test]
    fn provider_error_to_lim_error_preserves_display() {
        let pe = ProviderError::RateLimited { retry_after_ms: Some(5000) };
        let le: lim_core::error::LimError = pe.into();
        match le {
            lim_core::error::LimError::Provider(msg) => {
                assert!(msg.contains("rate limited"), "msg was: {msg}");
                assert!(msg.contains("5000"), "msg was: {msg}");
            }
            _ => panic!("expected Provider variant"),
        }
    }
}
