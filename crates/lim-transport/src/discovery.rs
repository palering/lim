//! Service discovery — how agents find each other.
//!
//! `ServiceDiscovery` is the abstraction for registering and looking up
//! agent addresses. Implementations vary by topology tier:
//!
//! | Tier | Implementation | Mechanism |
//! |---|---|---|
//! | T0 | `RoutingHub` (in lim-agent) | In-process HashMap |
//! | T1 | `LocalRegistry` | Daemon-side HashMap (in-memory) |
//! | T2 | `MdnsDiscovery` | mDNS broadcast on LAN |
//! | T3 | `A2ARegistry` | A2A registry endpoint |
//!
//! See `docs/architecture/transport-layer.md` §八 for the design rationale.

use std::net::SocketAddr;
use std::path::PathBuf;

use async_trait::async_trait;
use lim_agent::multiagent::AgentId;
use lim_core::error::Result;

/// Network address of an agent, by transport tier.
#[derive(Debug, Clone)]
pub enum AgentAddr {
    /// In-process (T0) — no network address, routed via `RoutingHub`.
    InProcess,

    /// Unix domain socket path (T1, Unix only).
    UnixSocket(PathBuf),

    /// Windows named pipe name (T1, Windows only).
    NamedPipe(String),

    /// TCP address (T2 cross-node).
    Tcp(SocketAddr),

    /// HTTP URL (T3 cross-network, A2A protocol).
    Http(String),
}

impl AgentAddr {
    /// Human-readable description for logging.
    pub fn describe(&self) -> String {
        match self {
            Self::InProcess => "in-process".to_string(),
            Self::UnixSocket(p) => format!("unix:{}", p.display()),
            Self::NamedPipe(n) => format!("pipe:{}", n),
            Self::Tcp(a) => format!("tcp:{}", a),
            Self::Http(u) => format!("http:{}", u),
        }
    }

    /// Which transport tier this address belongs to.
    pub fn tier(&self) -> TransportTier {
        match self {
            Self::InProcess => TransportTier::T0InProcess,
            Self::UnixSocket(_) | Self::NamedPipe(_) => TransportTier::T1CrossProcess,
            Self::Tcp(_) => TransportTier::T2CrossNode,
            Self::Http(_) => TransportTier::T3CrossNetwork,
        }
    }
}

impl std::fmt::Display for AgentAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.describe())
    }
}

/// The four-layer transport spectrum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportTier {
    /// In-process (mpsc channels).
    T0InProcess,
    /// Cross-process same machine (Unix socket / Named Pipe).
    T1CrossProcess,
    /// Cross-node same LAN (TCP + mDNS).
    T2CrossNode,
    /// Cross-network (A2A over HTTP).
    T3CrossNetwork,
}

impl TransportTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::T0InProcess => "t0_in_process",
            Self::T1CrossProcess => "t1_cross_process",
            Self::T2CrossNode => "t2_cross_node",
            Self::T3CrossNetwork => "t3_cross_network",
        }
    }
}

/// Trait for registering and looking up agent addresses.
///
/// Implementations are responsible for maintaining the registry
/// (in-memory, file-backed, mDNS, or remote A2A registry) and
/// handling concurrent access.
#[async_trait]
pub trait ServiceDiscovery: Send + Sync {
    /// Register an agent's address.
    async fn register(&self, id: &AgentId, addr: &AgentAddr) -> Result<()>;

    /// Look up an agent's address by ID.
    async fn lookup(&self, id: &AgentId) -> Result<Option<AgentAddr>>;

    /// List all registered agents and their addresses.
    async fn list(&self) -> Result<Vec<(AgentId, AgentAddr)>>;

    /// Deregister an agent (e.g. when it shuts down).
    async fn deregister(&self, id: &AgentId) -> Result<()>;
}

/// Simple in-memory service discovery for T1 (daemon-side registry).
///
/// All agents on the same machine register here when they start,
/// and look up peers by ID. Suitable for single-daemon T1 topologies.
pub struct LocalRegistry {
    inner: tokio::sync::RwLock<std::collections::HashMap<AgentId, AgentAddr>>,
}

impl LocalRegistry {
    pub fn new() -> Self {
        Self { inner: tokio::sync::RwLock::new(std::collections::HashMap::new()) }
    }
}

impl Default for LocalRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ServiceDiscovery for LocalRegistry {
    async fn register(&self, id: &AgentId, addr: &AgentAddr) -> Result<()> {
        self.inner.write().await.insert(id.clone(), addr.clone());
        Ok(())
    }

    async fn lookup(&self, id: &AgentId) -> Result<Option<AgentAddr>> {
        Ok(self.inner.read().await.get(id).cloned())
    }

    async fn list(&self) -> Result<Vec<(AgentId, AgentAddr)>> {
        Ok(self.inner.read().await.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
    }

    async fn deregister(&self, id: &AgentId) -> Result<()> {
        self.inner.write().await.remove(id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_registry_register_and_lookup() {
        let registry = LocalRegistry::new();
        let id = AgentId::new("researcher");
        let addr = AgentAddr::InProcess;

        registry.register(&id, &addr).await.unwrap();

        let found = registry.lookup(&id).await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().tier(), TransportTier::T0InProcess);
    }

    #[tokio::test]
    async fn local_registry_list() {
        let registry = LocalRegistry::new();
        registry.register(&AgentId::new("a"), &AgentAddr::InProcess).await.unwrap();
        registry
            .register(&AgentId::new("b"), &AgentAddr::Tcp("127.0.0.1:8080".parse().unwrap()))
            .await
            .unwrap();

        let list = registry.list().await.unwrap();
        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn local_registry_deregister() {
        let registry = LocalRegistry::new();
        let id = AgentId::new("temp");
        registry.register(&id, &AgentAddr::InProcess).await.unwrap();
        assert!(registry.lookup(&id).await.unwrap().is_some());

        registry.deregister(&id).await.unwrap();
        assert!(registry.lookup(&id).await.unwrap().is_none());
    }

    #[test]
    fn agent_addr_tier() {
        assert_eq!(AgentAddr::InProcess.tier(), TransportTier::T0InProcess);
        assert_eq!(
            AgentAddr::UnixSocket(PathBuf::from("/tmp/x.sock")).tier(),
            TransportTier::T1CrossProcess
        );
        assert_eq!(
            AgentAddr::NamedPipe(r"\\.\pipe\lim".to_string()).tier(),
            TransportTier::T1CrossProcess
        );
        assert_eq!(
            AgentAddr::Tcp("127.0.0.1:8080".parse().unwrap()).tier(),
            TransportTier::T2CrossNode
        );
        assert_eq!(
            AgentAddr::Http("https://lim.example.com".to_string()).tier(),
            TransportTier::T3CrossNetwork
        );
    }

    #[test]
    fn tier_as_str() {
        assert_eq!(TransportTier::T0InProcess.as_str(), "t0_in_process");
        assert_eq!(TransportTier::T3CrossNetwork.as_str(), "t3_cross_network");
    }
}
