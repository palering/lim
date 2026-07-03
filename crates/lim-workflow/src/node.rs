#![allow(clippy::type_complexity)]

use std::sync::Arc;

use lim_core::id::ArtifactId;
use lim_core::message::AgentMessage;

use crate::context::WorkflowContext;
use crate::types::NodeId;

/// A node in a workflow DAG.
pub struct Node {
    /// Unique identifier within the graph.
    pub id: NodeId,
    /// What this node does.
    pub kind: NodeKind,
}

impl Node {
    /// Create a new workflow [`Node`] with the given ID and kind.
    pub fn new(id: impl Into<NodeId>, kind: NodeKind) -> Self {
        Self { id: id.into(), kind }
    }
}

/// The kind of computation a node performs.
#[derive(Clone)]
pub enum NodeKind {
    /// Run an AgentLoop with a system prompt and dynamically composed initial message.
    AgentRun {
        system_prompt: String,
        initial_message_fn: Arc<dyn Fn(&WorkflowContext) -> AgentMessage + Send + Sync>,
    },

    /// Split a predecessor's output into multiple parallel branches.
    ///
    /// The splitter function receives the context and input, returning
    /// a list of (branch_label, data) pairs. Each branch will be available
    /// as a separate entry in the context.
    ParallelFanOut {
        splitter: Arc<
            dyn Fn(&WorkflowContext, &NodeOutput) -> Vec<(String, serde_json::Value)> + Send + Sync,
        >,
    },

    /// Collect all predecessor outputs and combine them.
    FanIn { combiner: Arc<dyn Fn(&WorkflowContext, &[NodeOutput]) -> NodeOutput + Send + Sync> },

    /// A review loop: writer → reviewer → pass/fail → rewrite, up to max_retries.
    ///
    /// Internally manages agent loop creation and retry logic.
    ReviewLoop {
        /// Produces writer prompt from scene data (and optional review feedback).
        writer_prompt_fn: Arc<dyn Fn(&serde_json::Value, Option<&str>) -> String + Send + Sync>,
        /// Produces reviewer prompt from the writer's draft.
        reviewer_prompt_fn: Arc<dyn Fn(&serde_json::Value) -> String + Send + Sync>,
        /// Minimum score (0.0–1.0) to pass review.
        pass_threshold: f64,
        /// Maximum number of rewrite attempts before accepting the draft as-is.
        max_retries: usize,
    },

    /// A pure data transformation node (no agent involved).
    Transform { transform: Arc<dyn Fn(&WorkflowContext, &NodeOutput) -> NodeOutput + Send + Sync> },
}

/// Output produced by a workflow node.
#[derive(Debug, Clone)]
pub enum NodeOutput {
    /// Messages from an agent run.
    AgentMessages(Vec<AgentMessage>),
    /// Artifact IDs created by this node.
    Artifacts(Vec<ArtifactId>),
    /// Arbitrary JSON data.
    Data(serde_json::Value),
    /// No output.
    Empty,
}

impl NodeOutput {
    /// Try to extract the text content from agent messages.
    pub fn extract_text(&self) -> Option<String> {
        match self {
            NodeOutput::AgentMessages(messages) => {
                let texts: Vec<String> = messages
                    .iter()
                    .filter(|m| m.role == lim_core::message::Role::Assistant)
                    .flat_map(|m| {
                        m.content.iter().filter_map(|block| {
                            if let lim_core::content::ContentBlock::Text { text } = block {
                                Some(text.clone())
                            } else {
                                None
                            }
                        })
                    })
                    .collect();
                if texts.is_empty() { None } else { Some(texts.join("\n\n")) }
            }
            NodeOutput::Data(value) => value.as_str().map(|s| s.to_string()),
            _ => None,
        }
    }
}
