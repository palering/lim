/// Identifier for a workflow node.
///
/// Phase 2 uses simple strings — no Ulid needed since workflow graphs
/// are constructed in code, not persisted.
pub type NodeId = String;
