//! lim-workflow: DAG-based workflow orchestration.
//!
//! Builds a directed acyclic graph of [`Node`]s, computes layered
//! topological order via Kahn's algorithm, and executes each layer
//! concurrently using [`WorkflowExecutor`]. [`WorkflowContext`] carries
//! outputs and arbitrary data between nodes; [`ArtifactStore`] persists
//! workflow artifacts.

pub mod artifact;
pub mod context;
pub mod events;
pub mod executor;
pub mod graph;
pub mod node;
pub mod types;

pub use artifact::{Artifact, ArtifactStore};
pub use context::WorkflowContext;
pub use events::WorkflowEvent;
pub use executor::WorkflowExecutor;
pub use graph::WorkflowGraph;
pub use node::{Node, NodeKind, NodeOutput};
pub use types::NodeId;
