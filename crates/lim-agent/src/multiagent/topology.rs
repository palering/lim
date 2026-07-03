//! Network topology: abstract agent interconnection pattern.
//!
//! `NetworkTopology` describes how agents are connected and how messages
//! flow between them. The default `LocalTopology` is P2P — every agent can
//! talk to every other agent directly via a shared `RoutingHub`. The
//! `CentralizedTopology` stub reserves a hub-and-spoke pattern where a
//! central coordinator brokers all communication.
//!
//! ## Design
//!
//! This trait is the extension point for Stage 4 (Daemon/Server) of the lim
//! evolution roadmap. In P3 we define the trait and a minimal local
//! implementation; actual network transports (P2P over TCP, centralized
//! scheduler over IPC) are future enhancements.
//!
//! ```text
//! NetworkTopology (trait)
//! ├── LocalTopology       — P2P, in-process (wraps RoutingHub)
//! └── CentralizedTopology — hub-and-spoke, coordinator-brokered (stub)
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use tracing::debug;

use lim_core::error::Result;

use super::transport::{AgentId, AgentTransport, LocalChannelTransport, RoutingHub};

/// Describes how agents are interconnected and how messages are routed.
///
/// Implementations produce an `AgentTransport` for each agent that joins the
/// topology. The transport is what the agent uses to send/receive messages;
/// the topology decides whether messages go directly (P2P) or through a
/// coordinator (centralized).
#[async_trait]
pub trait NetworkTopology: Send + Sync {
    /// Human-readable name (e.g. `"local-p2p"`, `"centralized"`).
    fn name(&self) -> &str;

    /// Join the topology with a new agent. Returns the agent's transport
    /// (which encapsulates both send and recv capabilities).
    ///
    /// The topology implementation is responsible for registering the agent
    /// in its routing table.
    async fn join(&self, id: AgentId) -> Result<Box<dyn AgentTransport>>;

    /// Remove an agent from the topology.
    async fn leave(&self, id: &AgentId) -> Result<()>;

    /// Number of agents currently in the topology.
    fn agent_count(&self) -> usize;

    /// Whether this topology is decentralized (P2P) or has a central
    /// coordinator. Used for display/diagnostic purposes.
    fn is_decentralized(&self) -> bool;
}

// ---------------------------------------------------------------------------
// LocalTopology — P2P in-process (wraps RoutingHub)
// ---------------------------------------------------------------------------

/// P2P topology for in-process agents.
///
/// All agents share a single `RoutingHub`. Messages are delivered directly
/// from sender to receiver via in-process mpsc channels — no central
/// coordinator, no network. This is the default for Stage 2 (Multi/Sub Agent)
/// and works for any number of agents in the same process.
pub struct LocalTopology {
    hub: Arc<RoutingHub>,
    /// Track how many agents have joined (for `agent_count`). The hub itself
    /// doesn't expose a count, so we maintain it separately with a mutex.
    count: std::sync::Mutex<usize>,
}

impl LocalTopology {
    /// Create a new local P2P topology with a fresh routing hub.
    pub fn new() -> Self {
        Self { hub: Arc::new(RoutingHub::new()), count: std::sync::Mutex::new(0) }
    }

    /// Create from an existing routing hub (e.g. when sharing a hub across
    /// multiple topology instances — rare, but useful for testing).
    pub fn with_hub(hub: Arc<RoutingHub>) -> Self {
        Self { hub, count: std::sync::Mutex::new(0) }
    }

    /// Access the underlying routing hub (e.g. for diagnostics).
    pub fn hub(&self) -> &Arc<RoutingHub> {
        &self.hub
    }
}

impl Default for LocalTopology {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NetworkTopology for LocalTopology {
    fn name(&self) -> &str {
        "local-p2p"
    }

    async fn join(&self, id: AgentId) -> Result<Box<dyn AgentTransport>> {
        let (_tx, rx) = self.hub.register(id.clone(), 64);
        let transport = LocalChannelTransport::new(Arc::clone(&self.hub), rx);
        *self.count.lock().expect("LocalTopology count mutex not poisoned") += 1;
        debug!(agent_id = %id, "local topology: agent joined");
        Ok(Box::new(transport))
    }

    async fn leave(&self, id: &AgentId) -> Result<()> {
        self.hub.deregister(id);
        let mut count = self.count.lock().expect("LocalTopology count mutex not poisoned");
        *count = count.saturating_sub(1);
        debug!(agent_id = %id, "local topology: agent left");
        Ok(())
    }

    fn agent_count(&self) -> usize {
        *self.count.lock().expect("LocalTopology count mutex not poisoned")
    }

    fn is_decentralized(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// CentralizedTopology — hub-and-spoke stub
// ---------------------------------------------------------------------------

/// Centralized topology: a coordinator brokers all messages.
///
/// In this topology, agents do not send messages directly to each other.
/// Instead, all messages go through a central coordinator, which can apply
/// policies (filtering, logging, rate limiting, priority) before forwarding.
///
/// ## Status (P3)
///
/// This is a **stub** — the coordinator is a simple pass-through that
/// forwards messages without any policy. It exists to:
/// 1. Reserve the trait extension point for centralized scheduling.
/// 2. Let us validate the `NetworkTopology` abstraction against both P2P
///    and centralized patterns.
///
/// Future enhancements: policy-based routing, priority queues, persistent
/// message log, coordinator failover.
pub struct CentralizedTopology {
    hub: Arc<RoutingHub>,
    count: std::sync::Mutex<usize>,
}

impl CentralizedTopology {
    /// Create a new centralized topology.
    ///
    /// Despite using a `RoutingHub` internally (same as `LocalTopology`), the
    /// intent is different: future versions will replace the hub with a
    /// policy-enforcing coordinator. The current implementation is a
    /// pass-through that behaves identically to `LocalTopology` but
    /// advertises `is_decentralized() == false`.
    pub fn new() -> Self {
        Self { hub: Arc::new(RoutingHub::new()), count: std::sync::Mutex::new(0) }
    }
}

impl Default for CentralizedTopology {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NetworkTopology for CentralizedTopology {
    fn name(&self) -> &str {
        "centralized"
    }

    async fn join(&self, id: AgentId) -> Result<Box<dyn AgentTransport>> {
        let (_tx, rx) = self.hub.register(id.clone(), 64);
        let transport = LocalChannelTransport::new(Arc::clone(&self.hub), rx);
        *self.count.lock().expect("CentralizedTopology count mutex not poisoned") += 1;
        debug!(agent_id = %id, "centralized topology: agent joined");
        Ok(Box::new(transport))
    }

    async fn leave(&self, id: &AgentId) -> Result<()> {
        self.hub.deregister(id);
        let mut count = self.count.lock().expect("CentralizedTopology count mutex not poisoned");
        *count = count.saturating_sub(1);
        debug!(agent_id = %id, "centralized topology: agent left");
        Ok(())
    }

    fn agent_count(&self) -> usize {
        *self.count.lock().expect("CentralizedTopology count mutex not poisoned")
    }

    fn is_decentralized(&self) -> bool {
        false
    }
}

/// Helper: wrap a topology in an `Arc<dyn NetworkTopology>` for sharing
/// across tasks.
pub fn into_topology_arc<T: NetworkTopology + 'static>(topology: T) -> Arc<dyn NetworkTopology> {
    Arc::new(topology)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_topology_join_and_leave() {
        let topo = LocalTopology::new();
        assert_eq!(topo.agent_count(), 0);

        let id = AgentId::new("agent-1");
        let _transport = topo.join(id.clone()).await.unwrap();
        assert_eq!(topo.agent_count(), 1);

        topo.leave(&id).await.unwrap();
        assert_eq!(topo.agent_count(), 0);
    }

    #[tokio::test]
    async fn centralized_topology_join_and_leave() {
        let topo = CentralizedTopology::new();
        assert_eq!(topo.agent_count(), 0);

        let id = AgentId::new("coordinator-agent");
        let _transport = topo.join(id.clone()).await.unwrap();
        assert_eq!(topo.agent_count(), 1);

        topo.leave(&id).await.unwrap();
        assert_eq!(topo.agent_count(), 0);
    }

    #[test]
    fn local_topology_is_decentralized() {
        let topo = LocalTopology::new();
        assert!(topo.is_decentralized());
        assert_eq!(topo.name(), "local-p2p");
    }

    #[test]
    fn centralized_topology_is_not_decentralized() {
        let topo = CentralizedTopology::new();
        assert!(!topo.is_decentralized());
        assert_eq!(topo.name(), "centralized");
    }

    #[tokio::test]
    async fn multiple_agents_can_join_local_topology() {
        let topo = LocalTopology::new();
        let ids = vec![AgentId::new("a1"), AgentId::new("a2"), AgentId::new("a3")];

        for id in &ids {
            topo.join(id.clone()).await.unwrap();
        }
        assert_eq!(topo.agent_count(), 3);

        // Leave one
        topo.leave(&ids[1]).await.unwrap();
        assert_eq!(topo.agent_count(), 2);
    }
}
