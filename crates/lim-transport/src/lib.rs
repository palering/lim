//! lim-transport — T1+ transport layer, protocol adapters, and service discovery.
//!
//! This crate provides cross-process / cross-node / cross-network transport
//! implementations for lim's multi-agent communication. The in-process (T0)
//! transport stays in `lim-agent` (it's tightly coupled with the `Scheduler`);
//! this crate handles everything above T0.
//!
//! ## Layering
//!
//! ```text
//! lim-core (AgentMessage, AgentId)
//!    ↑
//! lim-transport (T1+ transport + protocol adapters + discovery)  ← this crate
//!    ↑
//! lim-agent (AgentTransport trait, T0 LocalChannelTransport, Scheduler)
//!    ↑
//! lim-serve (assembles transport + agent + loop_runner into `lim serve`)
//! ```
//!
//! `lim-agent` does NOT depend on `lim-transport` — the app layer injects
//! T1+ transport implementations into `lim-agent`'s `AgentTransport` trait
//! object. This keeps L1/L2 integration users free of network dependencies.
//!
//! ## Key types
//!
//! - [`LocalEndpoint`] — platform-aware IPC address (Unix socket / Named Pipe / TCP)
//! - [`ProtocolAdapter`] — bridges `AgentMessage` and wire format
//! - [`ServiceDiscovery`] — register/lookup agent addresses
//! - [`TopologySelector`] — auto-infer transport tier from config
//!
//! See `docs/architecture/transport-layer.md` for the full design.

pub mod a2a;
pub mod discovery;
pub mod local_endpoint;
pub mod mcp_client;
pub mod mcp_protocol;
pub mod mcp_server;
pub mod mdns;
pub mod protocol;
pub mod registry;
pub mod selector;

// Platform-specific T1 transport implementations
#[cfg(windows)]
pub mod named_pipe;
pub mod tcp;
#[cfg(unix)]
pub mod unix_socket;

// Unified local socket abstraction over the platform transports above.
pub mod local_socket;

pub use a2a::{
    A2AAdapter, A2AHttpTransport, A2AMessage, A2APart, A2ARequestHandler, A2ATask,
    AgentCapabilities, AgentCard, AgentSkill, Artifact, TaskIdParams, TaskSendParams, TaskState,
    TaskStatus, a2a_to_agent_message, agent_message_to_a2a,
};
pub use discovery::{AgentAddr, LocalRegistry, ServiceDiscovery, TransportTier};
pub use local_endpoint::{EndpointParseError, LocalEndpoint};
pub use local_socket::{LocalSocketEndpoint, LocalSocketTransport};
pub use mcp_client::{McpClient, McpTool, McpToolProxy};
pub use mcp_protocol::{
    INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, JsonRpcError, JsonRpcRequest, JsonRpcResponse,
    MCP_PROTOCOL_VERSION, METHOD_NOT_FOUND, McpCallToolParams, McpCallToolResult, McpClientInfo,
    McpContent, McpInitializeParams, McpInitializeResult, McpResourceRef, McpServerInfo,
    McpToolDef, McpToolListResult, PARSE_ERROR,
};
pub use mcp_server::{McpServer, McpToolHandler, StaticToolHandler};
pub use mdns::{MdnsDiscovery, MdnsServiceRecord};
pub use protocol::{LimNativeAdapter, ProtocolAdapter};
pub use registry::{RegistryClient, RegistryEntry};
pub use selector::{
    DiscoveryMethod, Protocol, ResolvedTopology, TopologyConfig, TopologyError, TopologySelector,
};
