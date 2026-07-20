//! Topology auto-selection — infer transport tier from endpoint.
//!
//! The goal is **configuration simplification**: 90% of users should only
//! need zero config (T0) or one line (`endpoint = "auto"`). The selector
//! infers the transport tier, protocol, and discovery method from the
//! endpoint string, with optional explicit overrides.
//!
//! See `docs/architecture/transport-layer.md` §九–§十 for the design.

use crate::discovery::TransportTier;
use crate::local_endpoint::LocalEndpoint;

/// Wire protocol used between transport endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// lim's native serde_json format (T0/T1/T2).
    LimNative,
    /// A2A JSON-RPC protocol (T3, also usable on T1).
    A2a,
    /// MCP protocol (tool-level, T1/T3).
    Mcp,
}

impl Protocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LimNative => "lim_native",
            Self::A2a => "a2a",
            Self::Mcp => "mcp",
        }
    }
}

/// Service discovery mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryMethod {
    /// In-process `RoutingHub` (T0).
    RoutingHub,
    /// Daemon-side in-memory registry (T1).
    DaemonRegistry,
    /// mDNS broadcast on LAN (T2).
    Mdns,
    /// Remote A2A registry (T3).
    A2aRegistry,
    /// Manually configured peer list.
    ConfiguredPeers,
}

/// Resolved topology: the result of auto-selection.
#[derive(Debug, Clone)]
pub struct ResolvedTopology {
    /// Which transport tier was selected.
    pub tier: TransportTier,
    /// Which wire protocol to use.
    pub protocol: Protocol,
    /// Which discovery method to use.
    pub discovery: DiscoveryMethod,
    /// The resolved local endpoint (for T1+), if applicable.
    pub endpoint: Option<LocalEndpoint>,
    /// Optional auth token (for T2/T3).
    pub auth_token: Option<String>,
}

impl ResolvedTopology {
    /// The default T0 topology: in-process, lim_native, RoutingHub.
    pub fn t0_default() -> Self {
        Self {
            tier: TransportTier::T0InProcess,
            protocol: Protocol::LimNative,
            discovery: DiscoveryMethod::RoutingHub,
            endpoint: None,
            auth_token: None,
        }
    }
}

/// Topology configuration — what the user provides.
///
/// All fields are optional. When `tier` is `None`, the selector infers
/// it from `endpoint`. When `endpoint` is also `None`, defaults to T0.
#[derive(Debug, Clone, Default)]
pub struct TopologyConfig {
    /// Explicit tier override. If set, skips endpoint-based inference.
    pub tier: Option<TransportTier>,
    /// Endpoint string (e.g. `"auto"`, `"unix:///path"`, `"tcp://ip:port"`,
    /// `"https://host"`).
    pub endpoint: Option<String>,
    /// Protocol override.
    pub protocol: Option<Protocol>,
    /// Auth token for T2/T3.
    pub auth_token: Option<String>,
}

impl TopologyConfig {
    /// Zero config — T0 in-process.
    pub fn zero() -> Self {
        Self::default()
    }

    /// One-line config: just an endpoint.
    pub fn from_endpoint(endpoint: impl Into<String>) -> Self {
        Self { endpoint: Some(endpoint.into()), ..Default::default() }
    }
}

/// Selects the topology based on config, with auto-inference.
pub struct TopologySelector;

impl TopologySelector {
    /// Resolve a `TopologyConfig` into a `ResolvedTopology`.
    ///
    /// Inference rules (when `tier` is not explicitly set):
    /// - No endpoint → T0 in-process
    /// - `.sock` suffix or `unix://` / `pipe://` → T1 cross-process
    /// - `tcp://` → T2 cross-node
    /// - `http://` / `https://` / `a2a://` → T3 cross-network
    /// - `mcp://` → T1 or T3 depending on host
    pub fn select(config: &TopologyConfig) -> Result<ResolvedTopology, TopologyError> {
        // Explicit tier overrides inference
        if let Some(tier) = config.tier {
            return Ok(Self::resolve_explicit(tier, config));
        }

        // No endpoint → T0
        let endpoint_str = match &config.endpoint {
            None => return Ok(ResolvedTopology::t0_default()),
            Some(s) => s.as_str(),
        };

        // Infer from endpoint
        let (tier, protocol, discovery, endpoint) = Self::infer_from_endpoint(endpoint_str)?;

        // Protocol override
        let protocol = config.protocol.unwrap_or(protocol);

        Ok(ResolvedTopology {
            tier,
            protocol,
            discovery,
            endpoint: Some(endpoint),
            auth_token: config.auth_token.clone(),
        })
    }

    /// Infer tier/protocol/discovery from an endpoint string.
    fn infer_from_endpoint(
        s: &str,
    ) -> Result<(TransportTier, Protocol, DiscoveryMethod, LocalEndpoint), TopologyError> {
        if s == "auto" {
            let ep = LocalEndpoint::auto("lim");
            return Ok((
                TransportTier::T1CrossProcess,
                Protocol::LimNative,
                DiscoveryMethod::DaemonRegistry,
                ep,
            ));
        }

        if let Some(rest) = s.strip_prefix("unix://") {
            return Ok((
                TransportTier::T1CrossProcess,
                Protocol::LimNative,
                DiscoveryMethod::DaemonRegistry,
                LocalEndpoint::Unix(std::path::PathBuf::from(rest)),
            ));
        }

        if let Some(rest) = s.strip_prefix("pipe://") {
            return Ok((
                TransportTier::T1CrossProcess,
                Protocol::LimNative,
                DiscoveryMethod::DaemonRegistry,
                LocalEndpoint::NamedPipe(rest.to_string()),
            ));
        }

        if let Some(rest) = s.strip_prefix("tcp://") {
            let addr: std::net::SocketAddr = rest
                .parse()
                .map_err(|e| TopologyError::InvalidEndpoint(format!("invalid TCP address: {e}")))?;
            let is_loopback = addr.ip().is_loopback();
            let tier = if is_loopback {
                TransportTier::T1CrossProcess
            } else {
                TransportTier::T2CrossNode
            };
            let discovery =
                if is_loopback { DiscoveryMethod::DaemonRegistry } else { DiscoveryMethod::Mdns };
            return Ok((tier, Protocol::LimNative, discovery, LocalEndpoint::TcpLoopback(addr)));
        }

        if s.starts_with("http://") || s.starts_with("https://") || s.starts_with("a2a://") {
            return Ok((
                TransportTier::T3CrossNetwork,
                Protocol::A2a,
                DiscoveryMethod::A2aRegistry,
                LocalEndpoint::TcpLoopback("127.0.0.1:0".parse().unwrap()),
            ));
        }

        if s.starts_with("mcp://") {
            // MCP defaults to T1 (stdio/local) unless the host looks remote
            return Ok((
                TransportTier::T1CrossProcess,
                Protocol::Mcp,
                DiscoveryMethod::ConfiguredPeers,
                LocalEndpoint::auto("lim-mcp"),
            ));
        }

        // Heuristic: .sock suffix → T1 Unix socket
        if s.ends_with(".sock") {
            return Ok((
                TransportTier::T1CrossProcess,
                Protocol::LimNative,
                DiscoveryMethod::DaemonRegistry,
                LocalEndpoint::Unix(std::path::PathBuf::from(s)),
            ));
        }

        Err(TopologyError::InvalidEndpoint(format!("cannot infer topology from '{s}'")))
    }

    /// Resolve when tier is explicitly set.
    fn resolve_explicit(tier: TransportTier, config: &TopologyConfig) -> ResolvedTopology {
        let protocol = config.protocol.unwrap_or(match tier {
            TransportTier::T0InProcess
            | TransportTier::T1CrossProcess
            | TransportTier::T2CrossNode => Protocol::LimNative,
            TransportTier::T3CrossNetwork => Protocol::A2a,
        });

        let discovery = match tier {
            TransportTier::T0InProcess => DiscoveryMethod::RoutingHub,
            TransportTier::T1CrossProcess => DiscoveryMethod::DaemonRegistry,
            TransportTier::T2CrossNode => DiscoveryMethod::Mdns,
            TransportTier::T3CrossNetwork => DiscoveryMethod::A2aRegistry,
        };

        let endpoint = config.endpoint.as_ref().and_then(|s| LocalEndpoint::parse(s).ok());

        ResolvedTopology {
            tier,
            protocol,
            discovery,
            endpoint,
            auth_token: config.auth_token.clone(),
        }
    }
}

/// Error from topology selection.
#[derive(Debug, thiserror::Error)]
pub enum TopologyError {
    #[error("invalid endpoint: {0}")]
    InvalidEndpoint(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_config_defaults_to_t0() {
        let config = TopologyConfig::zero();
        let topo = TopologySelector::select(&config).unwrap();
        assert_eq!(topo.tier, TransportTier::T0InProcess);
        assert_eq!(topo.protocol, Protocol::LimNative);
        assert_eq!(topo.discovery, DiscoveryMethod::RoutingHub);
        assert!(topo.endpoint.is_none());
    }

    #[test]
    fn auto_endpoint_infers_t1() {
        let config = TopologyConfig::from_endpoint("auto");
        let topo = TopologySelector::select(&config).unwrap();
        assert_eq!(topo.tier, TransportTier::T1CrossProcess);
        assert_eq!(topo.discovery, DiscoveryMethod::DaemonRegistry);
        assert!(topo.endpoint.is_some());
    }

    #[test]
    fn unix_endpoint_infers_t1() {
        let config = TopologyConfig::from_endpoint("unix:///tmp/lim/test.sock");
        let topo = TopologySelector::select(&config).unwrap();
        assert_eq!(topo.tier, TransportTier::T1CrossProcess);
        assert_eq!(topo.protocol, Protocol::LimNative);
    }

    #[test]
    fn tcp_loopback_infers_t1() {
        let config = TopologyConfig::from_endpoint("tcp://127.0.0.1:7878");
        let topo = TopologySelector::select(&config).unwrap();
        assert_eq!(topo.tier, TransportTier::T1CrossProcess);
    }

    #[test]
    fn tcp_remote_infers_t2() {
        let config = TopologyConfig::from_endpoint("tcp://192.168.1.10:7878");
        let topo = TopologySelector::select(&config).unwrap();
        assert_eq!(topo.tier, TransportTier::T2CrossNode);
        assert_eq!(topo.discovery, DiscoveryMethod::Mdns);
    }

    #[test]
    fn http_endpoint_infers_t3() {
        let config = TopologyConfig::from_endpoint("https://lim-cluster.example.com");
        let topo = TopologySelector::select(&config).unwrap();
        assert_eq!(topo.tier, TransportTier::T3CrossNetwork);
        assert_eq!(topo.protocol, Protocol::A2a);
        assert_eq!(topo.discovery, DiscoveryMethod::A2aRegistry);
    }

    #[test]
    fn mcp_endpoint_infers_t1_mcp() {
        let config = TopologyConfig::from_endpoint("mcp://local");
        let topo = TopologySelector::select(&config).unwrap();
        assert_eq!(topo.tier, TransportTier::T1CrossProcess);
        assert_eq!(topo.protocol, Protocol::Mcp);
    }

    #[test]
    fn explicit_tier_overrides_inference() {
        let config = TopologyConfig {
            tier: Some(TransportTier::T3CrossNetwork),
            endpoint: Some("unix:///tmp/test.sock".to_string()),
            ..Default::default()
        };
        let topo = TopologySelector::select(&config).unwrap();
        // Explicit T3 wins over endpoint-based T1 inference
        assert_eq!(topo.tier, TransportTier::T3CrossNetwork);
        assert_eq!(topo.protocol, Protocol::A2a); // T3 defaults to A2A
    }

    #[test]
    fn protocol_override_works() {
        let config = TopologyConfig {
            endpoint: Some("unix:///tmp/test.sock".to_string()),
            protocol: Some(Protocol::A2a),
            ..Default::default()
        };
        let topo = TopologySelector::select(&config).unwrap();
        assert_eq!(topo.tier, TransportTier::T1CrossProcess);
        assert_eq!(topo.protocol, Protocol::A2a); // override applied
    }

    #[test]
    fn sock_heuristic_infers_t1() {
        let config = TopologyConfig::from_endpoint("/tmp/lim/daemon.sock");
        let topo = TopologySelector::select(&config).unwrap();
        assert_eq!(topo.tier, TransportTier::T1CrossProcess);
    }

    #[test]
    fn invalid_endpoint_fails() {
        let config = TopologyConfig::from_endpoint("foo://bar");
        assert!(TopologySelector::select(&config).is_err());
    }

    #[test]
    fn auth_token_propagated() {
        let config = TopologyConfig {
            endpoint: Some("https://lim.example.com".to_string()),
            auth_token: Some("secret123".to_string()),
            ..Default::default()
        };
        let topo = TopologySelector::select(&config).unwrap();
        assert_eq!(topo.auth_token.as_deref(), Some("secret123"));
    }
}
