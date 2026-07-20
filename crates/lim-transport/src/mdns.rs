//! mDNS-based service discovery for T2 (LAN cross-node).
//!
//! In a T2 topology, lim nodes on the same LAN discover each other via
//! multicast DNS (mDNS / DNS-SD). `MdnsDiscovery` is the abstraction: each
//! node registers the services it hosts and probes for peers' services.
//!
//! This is currently an in-memory stub so the transport crate compiles
//! without pulling a native mDNS dependency. Real mDNS support will plug in
//! here once a crate (e.g. `mdns-sd`) is adopted.
//!
//! See `docs/architecture/transport-layer.md` §八 for the design rationale.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use lim_core::error::Result;

/// A service record advertised or discovered via mDNS.
#[derive(Debug, Clone)]
pub struct MdnsServiceRecord {
    /// Human-readable service instance name (must be unique on the LAN).
    pub name: String,
    /// Hostname or IP address of the serving node.
    pub host: String,
    /// TCP port the service listens on.
    pub port: u16,
    /// Free-form key/value metadata (e.g. protocol, agent capabilities).
    pub metadata: HashMap<String, String>,
}

/// mDNS service discovery for T2 LAN topologies.
///
/// TODO: integrate mdns-sd crate for real mDNS (currently in-memory stub).
pub struct MdnsDiscovery {
    service_type: String,
    services: Mutex<Vec<MdnsServiceRecord>>,
}

impl MdnsDiscovery {
    /// Create a discovery handle for the given service type.
    ///
    /// `service_type` follows DNS-SD conventions, e.g. `"_lim._tcp."`.
    pub fn new(service_type: &str) -> Self {
        Self { service_type: service_type.to_string(), services: Mutex::new(Vec::new()) }
    }

    /// The DNS-SD service type this discovery operates on.
    pub fn service_type(&self) -> &str {
        &self.service_type
    }

    /// Register (advertise) a service on the LAN.
    ///
    /// Re-registering a record with an existing `name` replaces the prior
    /// entry.
    ///
    /// TODO: integrate mdns-sd crate for real mDNS (currently in-memory stub).
    pub async fn register(&self, record: MdnsServiceRecord) -> Result<()> {
        let mut services = self.services.lock().expect("mdns services mutex poisoned");
        services.retain(|r| r.name != record.name);
        services.push(record);
        Ok(())
    }

    /// Discover lim services on the LAN within the given timeout.
    ///
    /// TODO: integrate mdns-sd crate for real mDNS (currently in-memory stub).
    pub async fn discover(&self, _timeout: Duration) -> Result<Vec<MdnsServiceRecord>> {
        let services = self.services.lock().expect("mdns services mutex poisoned");
        Ok(services.clone())
    }

    /// Unregister (stop advertising) a service by name.
    ///
    /// TODO: integrate mdns-sd crate for real mDNS (currently in-memory stub).
    pub async fn unregister(&self, name: &str) -> Result<()> {
        let mut services = self.services.lock().expect("mdns services mutex poisoned");
        services.retain(|r| r.name != name);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(name: &str, port: u16) -> MdnsServiceRecord {
        let mut metadata = HashMap::new();
        metadata.insert("protocol".to_string(), "lim_native".to_string());
        MdnsServiceRecord {
            name: name.to_string(),
            host: "192.168.1.10".to_string(),
            port,
            metadata,
        }
    }

    #[tokio::test]
    async fn register_then_discover() {
        let discovery = MdnsDiscovery::new("_lim._tcp.");
        discovery.register(sample_record("lim-node-1", 7878)).await.unwrap();

        let found = discovery.discover(Duration::from_millis(10)).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "lim-node-1");
        assert_eq!(found[0].port, 7878);
        assert_eq!(found[0].metadata.get("protocol").unwrap(), "lim_native");
    }

    #[tokio::test]
    async fn unregister_removes_record() {
        let discovery = MdnsDiscovery::new("_lim._tcp.");
        discovery.register(sample_record("lim-node-1", 7878)).await.unwrap();
        assert_eq!(discovery.discover(Duration::from_millis(10)).await.unwrap().len(), 1);

        discovery.unregister("lim-node-1").await.unwrap();
        assert!(discovery.discover(Duration::from_millis(10)).await.unwrap().is_empty());

        // Unregistering a name that was never registered is a no-op.
        discovery.unregister("does-not-exist").await.unwrap();
    }

    #[tokio::test]
    async fn discover_empty_when_nothing_registered() {
        let discovery = MdnsDiscovery::new("_lim._tcp.");
        let found = discovery.discover(Duration::from_millis(10)).await.unwrap();
        assert!(found.is_empty());
    }

    #[tokio::test]
    async fn register_replaces_existing_name() {
        let discovery = MdnsDiscovery::new("_lim._tcp.");
        discovery.register(sample_record("lim-node-1", 7878)).await.unwrap();
        // Re-register same name with a different port.
        discovery.register(sample_record("lim-node-1", 9999)).await.unwrap();

        let found = discovery.discover(Duration::from_millis(10)).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].port, 9999);
    }
}
