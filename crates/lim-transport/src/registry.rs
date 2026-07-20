//! Remote registry service discovery for T3 (cross-network).
//!
//! In a T3 topology, lim nodes find each other through a central directory
//! service (an HTTP registry) rather than LAN broadcast. `RegistryClient` is
//! the client for that directory: agents register themselves on startup and
//! look up peers by ID.
//!
//! This is currently an in-memory stub so the transport crate compiles
//! without a live registry backend. Real HTTP calls will plug in here.
//!
//! See `docs/architecture/transport-layer.md` §八 for the design rationale.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::SystemTime;

use lim_core::error::Result;

/// An agent entry stored in the remote registry.
#[derive(Debug, Clone)]
pub struct RegistryEntry {
    /// Unique agent identifier.
    pub agent_id: String,
    /// Reachable endpoint URL for the agent (e.g. `https://host:port`).
    pub endpoint: String,
    /// Advertised capability tags (e.g. `["research", "write"]`).
    pub capabilities: Vec<String>,
    /// When the agent last refreshed its registration.
    pub last_seen: SystemTime,
}

/// Client for a remote agent registry (T3 cross-network).
///
/// TODO: implement HTTP calls to real registry service (currently in-memory stub).
pub struct RegistryClient {
    registry_url: String,
    entries: Mutex<HashMap<String, RegistryEntry>>,
}

impl RegistryClient {
    /// Create a registry client pointed at the given registry URL.
    ///
    /// `registry_url` is the base URL of the directory service, e.g.
    /// `https://registry.example.com`.
    pub fn new(registry_url: String) -> Self {
        Self { registry_url, entries: Mutex::new(HashMap::new()) }
    }

    /// The base URL of the registry this client talks to.
    pub fn registry_url(&self) -> &str {
        &self.registry_url
    }

    /// Register (or refresh) an agent entry in the directory.
    ///
    /// TODO: implement HTTP calls to real registry service (currently in-memory stub).
    pub async fn register(&self, entry: RegistryEntry) -> Result<()> {
        let mut entries = self.entries.lock().expect("registry entries mutex poisoned");
        entries.insert(entry.agent_id.clone(), entry);
        Ok(())
    }

    /// Look up a single agent by ID.
    ///
    /// TODO: implement HTTP calls to real registry service (currently in-memory stub).
    pub async fn lookup(&self, agent_id: &str) -> Result<Option<RegistryEntry>> {
        let entries = self.entries.lock().expect("registry entries mutex poisoned");
        Ok(entries.get(agent_id).cloned())
    }

    /// List all agents currently registered in the directory.
    ///
    /// TODO: implement HTTP calls to real registry service (currently in-memory stub).
    pub async fn list(&self) -> Result<Vec<RegistryEntry>> {
        let entries = self.entries.lock().expect("registry entries mutex poisoned");
        Ok(entries.values().cloned().collect())
    }

    /// Deregister an agent from the directory (e.g. on shutdown).
    ///
    /// TODO: implement HTTP calls to real registry service (currently in-memory stub).
    pub async fn deregister(&self, agent_id: &str) -> Result<()> {
        let mut entries = self.entries.lock().expect("registry entries mutex poisoned");
        entries.remove(agent_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(id: &str, endpoint: &str) -> RegistryEntry {
        RegistryEntry {
            agent_id: id.to_string(),
            endpoint: endpoint.to_string(),
            capabilities: vec!["research".to_string(), "write".to_string()],
            last_seen: SystemTime::now(),
        }
    }

    #[tokio::test]
    async fn register_then_lookup() {
        let client = RegistryClient::new("https://registry.example.com".to_string());
        client.register(sample_entry("agent-1", "https://10.0.0.1:8443")).await.unwrap();

        let found = client.lookup("agent-1").await.unwrap();
        assert!(found.is_some());
        let entry = found.unwrap();
        assert_eq!(entry.agent_id, "agent-1");
        assert_eq!(entry.endpoint, "https://10.0.0.1:8443");
        assert_eq!(entry.capabilities, vec!["research", "write"]);

        // Unknown agent returns None.
        assert!(client.lookup("no-such-agent").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn list_returns_all_entries() {
        let client = RegistryClient::new("https://registry.example.com".to_string());
        client.register(sample_entry("agent-1", "https://10.0.0.1:8443")).await.unwrap();
        client.register(sample_entry("agent-2", "https://10.0.0.2:8443")).await.unwrap();

        let mut ids: Vec<String> =
            client.list().await.unwrap().into_iter().map(|e| e.agent_id).collect();
        ids.sort();
        assert_eq!(ids, vec!["agent-1".to_string(), "agent-2".to_string()]);
    }

    #[tokio::test]
    async fn deregister_removes_entry() {
        let client = RegistryClient::new("https://registry.example.com".to_string());
        client.register(sample_entry("agent-1", "https://10.0.0.1:8443")).await.unwrap();
        assert!(client.lookup("agent-1").await.unwrap().is_some());

        client.deregister("agent-1").await.unwrap();
        assert!(client.lookup("agent-1").await.unwrap().is_none());
        assert!(client.list().await.unwrap().is_empty());

        // Deregistering an unknown id is a no-op.
        client.deregister("never-registered").await.unwrap();
    }

    #[test]
    fn registry_url_accessor() {
        let client = RegistryClient::new("https://registry.example.com".to_string());
        assert_eq!(client.registry_url(), "https://registry.example.com");
    }
}
