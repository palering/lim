use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::node::NodeOutput;
use crate::types::NodeId;

/// Shared context passed between workflow nodes.
///
/// Nodes read predecessor outputs from the context and write their
/// own outputs back. Thread-safe for concurrent access from parallel nodes.
pub struct WorkflowContext {
    /// Outputs keyed by node ID.
    outputs: Arc<RwLock<HashMap<NodeId, NodeOutput>>>,
    /// Arbitrary data entries (e.g. scene data, story bible).
    data: Arc<RwLock<HashMap<String, serde_json::Value>>>,
}

impl WorkflowContext {
    pub fn new() -> Self {
        Self {
            outputs: Arc::new(RwLock::new(HashMap::new())),
            data: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Store a node's output.
    pub fn set_output(&self, node_id: NodeId, output: NodeOutput) {
        self.outputs.write().unwrap().insert(node_id, output);
    }

    /// Get a node's output.
    pub fn get_output(&self, node_id: &str) -> Option<NodeOutput> {
        self.outputs.read().unwrap().get(node_id).cloned()
    }

    /// Get outputs from multiple predecessors.
    pub fn get_outputs(&self, node_ids: &[NodeId]) -> Vec<NodeOutput> {
        let outputs = self.outputs.read().unwrap();
        node_ids
            .iter()
            .filter_map(|id| outputs.get(id).cloned())
            .collect()
    }

    /// Store arbitrary data.
    pub fn set_data(&self, key: impl Into<String>, value: serde_json::Value) {
        self.data.write().unwrap().insert(key.into(), value);
    }

    /// Get arbitrary data.
    pub fn get_data(&self, key: &str) -> Option<serde_json::Value> {
        self.data.read().unwrap().get(key).cloned()
    }

    /// List all node IDs that have outputs.
    pub fn output_keys(&self) -> Vec<NodeId> {
        self.outputs.read().unwrap().keys().cloned().collect()
    }
}

impl Default for WorkflowContext {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for WorkflowContext {
    fn clone(&self) -> Self {
        Self {
            outputs: Arc::clone(&self.outputs),
            data: Arc::clone(&self.data),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_retrieve_output() {
        let ctx = WorkflowContext::new();
        ctx.set_output("a".to_string(), NodeOutput::Data(serde_json::json!(42)));

        let output = ctx.get_output("a").unwrap();
        match output {
            NodeOutput::Data(v) => assert_eq!(v, serde_json::json!(42)),
            _ => panic!("expected Data output"),
        }
    }

    #[test]
    fn missing_output_returns_none() {
        let ctx = WorkflowContext::new();
        assert!(ctx.get_output("nonexistent").is_none());
    }

    #[test]
    fn store_and_retrieve_data() {
        let ctx = WorkflowContext::new();
        ctx.set_data("bible", serde_json::json!({"title": "My Novel"}));

        let data = ctx.get_data("bible").unwrap();
        assert_eq!(data["title"], "My Novel");
    }

    #[test]
    fn clone_shares_state() {
        let ctx = WorkflowContext::new();
        let ctx2 = ctx.clone();

        ctx.set_output("a".to_string(), NodeOutput::Data(serde_json::json!(1)));
        assert!(ctx2.get_output("a").is_some());
    }
}
