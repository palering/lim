//! Agent transport: message routing between agents.
//!
//! `AgentTransport` is the abstraction for inter-agent communication. The
//! default implementation `LocalChannelTransport` uses in-process tokio
//! channels (Stage 2). Future implementations (P4) can provide P2P network
//! or centralized scheduler transports without changing the trait.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::debug;

use lim_core::error::{LimError, Result};
use lim_core::message::AgentMessage;

/// Unique identifier for an agent in a multi-agent system.
///
/// Wraps a `String` so it can carry human-readable names (e.g. `"researcher"`)
/// or generated IDs. This is distinct from `SessionId` (which identifies a
/// conversation session) — an agent may run multiple sessions over its
/// lifetime.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AgentId(String);

impl AgentId {
    /// Create an agent ID from a string (e.g. `"researcher"`, `"impl-1"`).
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Get the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for AgentId {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Self(s.to_string()))
    }
}

/// Trait for routing messages between agents.
///
/// Implementations must be `Send + Sync` so they can be shared across
/// async tasks. Each agent has its own transport instance — the transport's
/// `recv()` is the agent's inbox, and `send()` delivers to another agent's
/// inbox via the transport's routing backend.
///
/// ## Implementations
///
/// - `LocalChannelTransport` — in-process tokio channels (Stage 2)
/// - Future: `P2PNetworkTransport` — P2P for distributed agents (P4)
/// - Future: `CentralizedTransport` — scheduler-mediated routing (P4)
#[async_trait]
pub trait AgentTransport: Send + Sync {
    /// Send a message to a target agent.
    async fn send(&self, target: &AgentId, msg: AgentMessage) -> Result<()>;

    /// Receive the next inbound message (blocks until one arrives).
    async fn recv(&self) -> Result<AgentMessage>;
}

/// Routing backend shared by all `LocalChannelTransport` instances.
///
/// Each agent's transport holds a clone of the `RoutingHub` and an `mpsc::Receiver`
/// for its inbox. `send()` looks up the target's sender in the hub and dispatches.
///
/// This is intentionally simple — no buffering beyond the channel's capacity,
/// no retries, no persistence. Suitable for in-process multi-agent systems.
pub struct RoutingHub {
    inner: Arc<std::sync::Mutex<std::collections::HashMap<AgentId, mpsc::Sender<AgentMessage>>>>,
}

impl RoutingHub {
    /// Create a new empty routing hub.
    pub fn new() -> Self {
        Self { inner: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())) }
    }

    /// Register a new agent inbox. Returns the sender (for the hub) and
    /// receiver (for the agent's transport).
    ///
    /// The channel capacity is configurable; 64 is a reasonable default for
    /// most use cases.
    pub fn register(
        &self,
        id: AgentId,
        buffer: usize,
    ) -> (mpsc::Sender<AgentMessage>, mpsc::Receiver<AgentMessage>) {
        let (tx, rx) = mpsc::channel(buffer);
        self.inner.lock().expect("RoutingHub mutex not poisoned").insert(id, tx.clone());
        (tx, rx)
    }

    /// Look up the sender for a target agent.
    pub fn lookup(&self, id: &AgentId) -> Option<mpsc::Sender<AgentMessage>> {
        self.inner.lock().expect("RoutingHub mutex not poisoned").get(id).cloned()
    }

    /// Deregister an agent (e.g. after it exits).
    pub fn deregister(&self, id: &AgentId) {
        self.inner.lock().expect("RoutingHub mutex not poisoned").remove(id);
    }
}

impl Default for RoutingHub {
    fn default() -> Self {
        Self::new()
    }
}

/// In-process transport backed by tokio mpsc channels.
///
/// Each instance has its own inbox (`mpsc::Receiver`) and shares a
/// `RoutingHub` with all other agents for outbound delivery.
///
/// ## Example
///
/// ```ignore
/// let hub = Arc::new(RoutingHub::new());
/// let alice_tx = Arc::new(hub.register(AgentId::new("alice"), 64));
/// // ... create LocalChannelTransport for alice ...
/// ```
pub struct LocalChannelTransport {
    hub: Arc<RoutingHub>,
    inbox: tokio::sync::Mutex<mpsc::Receiver<AgentMessage>>,
}

impl LocalChannelTransport {
    /// Create a transport for a newly-registered agent.
    ///
    /// Caller must have already called `RoutingHub::register` and obtained
    /// the receiver. This constructor takes ownership of the receiver.
    pub fn new(hub: Arc<RoutingHub>, inbox: mpsc::Receiver<AgentMessage>) -> Self {
        Self { hub, inbox: tokio::sync::Mutex::new(inbox) }
    }
}

#[async_trait]
impl AgentTransport for LocalChannelTransport {
    async fn send(&self, target: &AgentId, msg: AgentMessage) -> Result<()> {
        match self.hub.lookup(target) {
            Some(tx) => {
                tx.send(msg)
                    .await
                    .map_err(|_| LimError::Config(format!("agent `{target}` inbox closed")))?;
                debug!(target = %target, "message routed");
                Ok(())
            }
            None => Err(LimError::Config(format!("agent `{target}` not registered"))),
        }
    }

    async fn recv(&self) -> Result<AgentMessage> {
        let mut inbox = self.inbox.lock().await;
        inbox.recv().await.ok_or_else(|| LimError::Config("inbox closed".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lim_core::message::Role;

    #[tokio::test]
    async fn local_transport_routes_between_agents() {
        let hub = Arc::new(RoutingHub::new());
        let (_alice_tx, alice_rx) = hub.register(AgentId::new("alice"), 8);
        let (_bob_tx, bob_rx) = hub.register(AgentId::new("bob"), 8);

        let alice = LocalChannelTransport::new(Arc::clone(&hub), alice_rx);
        let bob = LocalChannelTransport::new(Arc::clone(&hub), bob_rx);

        // Alice sends to Bob
        let msg = AgentMessage::user("hello from alice");
        alice.send(&AgentId::new("bob"), msg.clone()).await.unwrap();

        // Bob receives
        let received = bob.recv().await.unwrap();
        assert_eq!(received.role, Role::User);
        match &received.content[0] {
            lim_core::content::ContentBlock::Text { text } => {
                assert_eq!(text, "hello from alice");
            }
            other => panic!("expected Text block, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn send_to_unregistered_agent_fails() {
        let hub = Arc::new(RoutingHub::new());
        let (_tx, rx) = hub.register(AgentId::new("alice"), 8);
        let alice = LocalChannelTransport::new(hub, rx);

        let result = alice.send(&AgentId::new("nobody"), AgentMessage::user("hi")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn deregister_removes_agent() {
        let hub = Arc::new(RoutingHub::new());
        let (tx, _rx) = hub.register(AgentId::new("alice"), 8);
        assert!(hub.lookup(&AgentId::new("alice")).is_some());

        hub.deregister(&AgentId::new("alice"));
        assert!(hub.lookup(&AgentId::new("alice")).is_none());

        // The sender obtained before deregister is now the only reference;
        // dropping it closes the channel.
        drop(tx);
    }

    #[test]
    fn agent_id_display_and_parse() {
        let id = AgentId::new("researcher-1");
        assert_eq!(id.as_str(), "researcher-1");
        assert_eq!(id.to_string(), "researcher-1");
        let parsed: AgentId = "researcher-1".parse().unwrap();
        assert_eq!(id, parsed);
    }
}
