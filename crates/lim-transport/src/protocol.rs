//! Protocol adapter layer — decouples wire format from transport.
//!
//! Protocols and transports are orthogonal:
//! - A2A protocol can run over HTTP (T3) or Unix socket (T1)
//! - MCP protocol can run over stdio (T1) or SSE (T3)
//! - lim-native can run over mpsc (T0) / Unix socket (T1) / TCP (T2)
//!
//! `ProtocolAdapter` bridges between `AgentMessage` (lim's in-memory type)
//! and the wire format. Each transport pairs with an adapter to form a
//! complete communication channel.
//!
//! See `docs/architecture/transport-layer.md` §五 for the design rationale.

use async_trait::async_trait;
use lim_core::error::{LimError, Result};
use lim_core::message::AgentMessage;

/// Bridges between lim's in-memory `AgentMessage` and a wire format.
///
/// Each transport implementation pairs with a `ProtocolAdapter` to handle
/// serialization. This separation lets you combine any transport with any
/// protocol (e.g. `UnixSocketTransport + A2AAdapter` for same-machine
/// A2A communication).
#[async_trait]
pub trait ProtocolAdapter: Send + Sync {
    /// Encode an `AgentMessage` into wire-format bytes.
    async fn encode(&self, msg: &AgentMessage) -> Result<Vec<u8>>;

    /// Decode wire-format bytes back into an `AgentMessage`.
    async fn decode(&self, data: &[u8]) -> Result<AgentMessage>;

    /// Human-readable protocol name (e.g. `"lim_native"`, `"a2a"`).
    fn name(&self) -> &str;
}

/// lim's native protocol: `serde_json` serialization of `AgentMessage`.
///
/// This is the simplest adapter — messages are serialized as JSON lines
/// (one JSON object per message, newline-delimited on the wire). Suitable
/// for T0/T1/T2 where both ends are lim.
pub struct LimNativeAdapter;

impl LimNativeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LimNativeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProtocolAdapter for LimNativeAdapter {
    async fn encode(&self, msg: &AgentMessage) -> Result<Vec<u8>> {
        serde_json::to_vec(msg).map_err(|e| LimError::Config(format!("lim_native encode: {e}")))
    }

    async fn decode(&self, data: &[u8]) -> Result<AgentMessage> {
        serde_json::from_slice(data)
            .map_err(|e| LimError::Config(format!("lim_native decode: {e}")))
    }

    fn name(&self) -> &str {
        "lim_native"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lim_core::message::Role;

    #[tokio::test]
    async fn lim_native_roundtrip() {
        let adapter = LimNativeAdapter::new();
        let msg = AgentMessage::user("hello world");

        let encoded = adapter.encode(&msg).await.unwrap();
        assert!(!encoded.is_empty());

        let decoded = adapter.decode(&encoded).await.unwrap();
        assert_eq!(decoded.role, Role::User);
        match &decoded.content[0] {
            lim_core::content::ContentBlock::Text { text } => {
                assert_eq!(text, "hello world");
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn lim_native_decode_invalid_json_fails() {
        let adapter = LimNativeAdapter::new();
        let result = adapter.decode(b"not json").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn lim_native_name() {
        let adapter = LimNativeAdapter::new();
        assert_eq!(adapter.name(), "lim_native");
    }
}
