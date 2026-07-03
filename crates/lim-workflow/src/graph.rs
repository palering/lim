use std::collections::{HashMap, HashSet, VecDeque};

use lim_core::error::{LimError, Result};

use crate::node::Node;
use crate::types::NodeId;

/// A directed acyclic graph of workflow nodes.
pub struct WorkflowGraph {
    nodes: HashMap<NodeId, Node>,
    edges: Vec<(NodeId, NodeId)>,
}

impl WorkflowGraph {
    /// Create a new empty [`WorkflowGraph`].
    pub fn new() -> Self {
        Self { nodes: HashMap::new(), edges: Vec::new() }
    }

    /// Add a node to the graph.
    pub fn add_node(&mut self, node: Node) {
        self.nodes.insert(node.id.clone(), node);
    }

    /// Add a directed edge from one node to another.
    pub fn add_edge(&mut self, from: impl Into<NodeId>, to: impl Into<NodeId>) {
        self.edges.push((from.into(), to.into()));
    }

    /// Get a node by ID.
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// Get all predecessors of a node.
    pub fn predecessors(&self, node_id: &str) -> Vec<NodeId> {
        self.edges.iter().filter(|(_, to)| to == node_id).map(|(from, _)| from.clone()).collect()
    }

    /// Get all successors of a node.
    pub fn successors(&self, node_id: &str) -> Vec<NodeId> {
        self.edges.iter().filter(|(from, _)| from == node_id).map(|(_, to)| to.clone()).collect()
    }

    /// Return all node IDs.
    pub fn node_ids(&self) -> Vec<&NodeId> {
        self.nodes.keys().collect()
    }

    /// Compute a layered topological ordering.
    ///
    /// Returns a list of "levels" where nodes in the same level can be
    /// executed in parallel. Levels are executed sequentially.
    ///
    /// Uses Kahn's algorithm (BFS-based topological sort).
    pub fn topological_order(&self) -> Result<Vec<Vec<NodeId>>> {
        // Compute in-degree for each node
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        for id in self.nodes.keys() {
            in_degree.insert(id.as_str(), 0);
        }
        for (from, to) in &self.edges {
            if !self.nodes.contains_key(from) || !self.nodes.contains_key(to) {
                continue;
            }
            *in_degree.entry(to.as_str()).or_insert(0) += 1;
        }

        // Start with nodes that have no incoming edges
        let mut queue: VecDeque<NodeId> = VecDeque::new();
        for (id, &degree) in &in_degree {
            if degree == 0 {
                queue.push_back(id.to_string());
            }
        }

        let mut levels: Vec<Vec<NodeId>> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();

        while !queue.is_empty() {
            let level: Vec<NodeId> = queue.drain(..).collect();
            let mut next_queue: Vec<NodeId> = Vec::new();

            for node_id in &level {
                visited.insert(node_id.clone());
                for succ in self.successors(node_id) {
                    if visited.contains(&succ) {
                        continue;
                    }
                    let deg = in_degree
                        .get_mut(succ.as_str())
                        .expect("successor must be in in_degree map");
                    *deg -= 1;
                    if *deg == 0 {
                        next_queue.push(succ);
                    }
                }
            }

            // Sort for deterministic ordering within a level
            let mut sorted_level = level;
            sorted_level.sort();
            levels.push(sorted_level);

            for node in next_queue {
                queue.push_back(node);
            }
        }

        // Check for cycles
        if visited.len() != self.nodes.len() {
            return Err(LimError::Config("workflow graph contains a cycle".to_string()));
        }

        Ok(levels)
    }
}

impl Default for WorkflowGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{Node, NodeKind, NodeOutput};
    use std::sync::Arc;

    fn transform_node(id: &str) -> Node {
        Node::new(id, NodeKind::Transform { transform: Arc::new(|_ctx, input| input.clone()) })
    }

    #[test]
    fn empty_graph() {
        let graph = WorkflowGraph::new();
        let levels = graph.topological_order().unwrap();
        assert!(levels.is_empty());
    }

    #[test]
    fn single_node() {
        let mut graph = WorkflowGraph::new();
        graph.add_node(transform_node("a"));
        let levels = graph.topological_order().unwrap();
        assert_eq!(levels.len(), 1);
        assert_eq!(levels[0], vec!["a"]);
    }

    #[test]
    fn linear_chain() {
        let mut graph = WorkflowGraph::new();
        graph.add_node(transform_node("a"));
        graph.add_node(transform_node("b"));
        graph.add_node(transform_node("c"));
        graph.add_edge("a", "b");
        graph.add_edge("b", "c");

        let levels = graph.topological_order().unwrap();
        assert_eq!(levels.len(), 3);
        assert_eq!(levels[0], vec!["a"]);
        assert_eq!(levels[1], vec!["b"]);
        assert_eq!(levels[2], vec!["c"]);
    }

    #[test]
    fn fan_out_fan_in() {
        // a -> b, a -> c, b -> d, c -> d
        let mut graph = WorkflowGraph::new();
        graph.add_node(transform_node("a"));
        graph.add_node(transform_node("b"));
        graph.add_node(transform_node("c"));
        graph.add_node(transform_node("d"));
        graph.add_edge("a", "b");
        graph.add_edge("a", "c");
        graph.add_edge("b", "d");
        graph.add_edge("c", "d");

        let levels = graph.topological_order().unwrap();
        assert_eq!(levels.len(), 3);
        assert_eq!(levels[0], vec!["a"]);
        // b and c are in the same level (parallel)
        assert_eq!(levels[1].len(), 2);
        assert!(levels[1].contains(&"b".to_string()));
        assert!(levels[1].contains(&"c".to_string()));
        assert_eq!(levels[2], vec!["d"]);
    }

    #[test]
    fn diamond_graph() {
        //    a
        //   / \
        //  b   c
        //   \ /
        //    d
        let mut graph = WorkflowGraph::new();
        for id in &["a", "b", "c", "d"] {
            graph.add_node(transform_node(id));
        }
        graph.add_edge("a", "b");
        graph.add_edge("a", "c");
        graph.add_edge("b", "d");
        graph.add_edge("c", "d");

        let levels = graph.topological_order().unwrap();
        assert_eq!(levels.len(), 3);
    }

    #[test]
    fn predecessors_and_successors() {
        let mut graph = WorkflowGraph::new();
        graph.add_node(transform_node("a"));
        graph.add_node(transform_node("b"));
        graph.add_node(transform_node("c"));
        graph.add_edge("a", "b");
        graph.add_edge("a", "c");

        assert_eq!(graph.successors("a"), vec!["b", "c"]);
        assert_eq!(graph.predecessors("b"), vec!["a"]);
        assert!(graph.predecessors("a").is_empty());
        assert!(graph.successors("b").is_empty());
    }

    #[test]
    fn node_output_extract_text() {
        use lim_core::content::ContentBlock;
        use lim_core::message::AgentMessage;

        let messages = vec![
            AgentMessage::user("ignored"),
            AgentMessage::assistant(vec![ContentBlock::text("Hello world")]),
        ];
        let output = NodeOutput::AgentMessages(messages);
        assert_eq!(output.extract_text(), Some("Hello world".to_string()));

        let empty = NodeOutput::Empty;
        assert_eq!(empty.extract_text(), None);

        let data = NodeOutput::Data(serde_json::json!("some text"));
        assert_eq!(data.extract_text(), Some("some text".to_string()));
    }
}
