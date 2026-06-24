use lim_core::id::ArtifactId;
use serde::{Deserialize, Serialize};

use crate::types::NodeId;

/// Events emitted during workflow execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WorkflowEvent {
    /// A workflow node started executing.
    NodeStart { node_id: NodeId },

    /// A workflow node finished executing.
    NodeEnd {
        node_id: NodeId,
        output_summary: String,
    },

    /// An artifact was created during workflow execution.
    ArtifactCreated {
        artifact_id: ArtifactId,
        kind: String,
        node_id: NodeId,
    },

    /// A review loop iteration completed.
    ReviewResult {
        node_id: NodeId,
        passed: bool,
        score: f64,
        attempt: usize,
    },

    /// The entire workflow started.
    WorkflowStart,

    /// The entire workflow completed.
    WorkflowEnd,
}
