//! Provider error types.
//!
//! Structured errors that allow the agent loop to make retry/recovery decisions
//! based on the error category (rate limit, auth, context window, etc.).

use thiserror::Error;

/// Errors returned by LLM providers.
#[derive(Debug, Clone, Error)]
pub enum ProviderError {
    /// Network or transport-level error (timeout, connection refused, etc.).
    #[error("transport error: {0}")]
    Transport(String),

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
                | ProviderError::Transport(_)
        )
    }
}

impl From<reqwest::Error> for ProviderError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            ProviderError::Transport(format!("timeout: {err}"))
        } else if err.is_connect() {
            ProviderError::Transport(format!("connection: {err}"))
        } else {
            ProviderError::Transport(err.to_string())
        }
    }
}

impl From<lim_core::error::LimError> for ProviderError {
    fn from(err: lim_core::error::LimError) -> Self {
        ProviderError::Stream(err.to_string())
    }
}

impl From<ProviderError> for lim_core::error::LimError {
    fn from(err: ProviderError) -> Self {
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
        assert!(ProviderError::Transport("timeout".into()).is_retryable());
        assert!(!ProviderError::Auth("bad key".into()).is_retryable());
        assert!(!ProviderError::ContextWindowExceeded.is_retryable());
        assert!(!ProviderError::InvalidRequest("bad".into()).is_retryable());
    }
}
