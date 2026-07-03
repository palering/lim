//! Multi-agent primitives: transport, scheduler, agent identity, topology.
//!
//! This module provides the building blocks for Stage 2 (Multi/Sub Agent)
//! of the lim evolution roadmap. It is intentionally minimal — the goal is
//! to establish the trait boundaries and a working in-process transport,
//! not a full multi-agent runtime.
//!
//! ## Architecture
//!
//! ```text
//! Scheduler
//! ├── spawns AgentLoop instances as tokio tasks
//! ├── routes messages via AgentTransport (LocalChannelTransport)
//! └── tracks agent lifecycle (running / done / cancelled)
//!
//! NetworkTopology (P3)
//! ├── LocalTopology        — P2P, in-process (wraps RoutingHub)
//! └── CentralizedTopology  — hub-and-spoke stub
//!
//! WorkspaceRegistry (P3, daemon mode)
//! └── tracks multiple workspaces for cross-workspace goal execution
//! ```
//!
//! See `docs/architecture/evolution.md` Stage 2 / Stage 4 for the roadmap context.

pub mod scheduler;
pub mod topology;
pub mod transport;
pub mod workspace;
pub mod worktree;

pub use scheduler::{AgentHandle, AgentStatus, Scheduler, SpawnOptions};
pub use topology::{CentralizedTopology, LocalTopology, NetworkTopology, into_topology_arc};
pub use transport::{AgentId, AgentTransport, LocalChannelTransport, RoutingHub};
pub use workspace::{Workspace, WorkspaceId, WorkspaceRegistry};
pub use worktree::{WorktreeHandle, WorktreeId, WorktreeSpec, create_worktree};
