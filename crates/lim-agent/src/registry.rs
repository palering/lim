use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use lim_core::error::{LimError, Result};
use lim_core::tool::{ToolCategory, ToolSchema};

use crate::tool::Tool;

/// Registry of available tools, indexed by name.
///
/// ## Interior mutability
///
/// `ToolRegistry` uses an `RwLock<HashMap<String, Arc<dyn Tool>>>` internally
/// so that tools can be registered at runtime even when the registry is
/// shared via `Arc`. This is essential for MCP / external tool servers that
/// connect after the agent loop has started.
///
/// Read operations (`get`, `schemas`, `category_of`) take a read lock only
/// for the duration of the lookup; `get` returns an `Arc<dyn Tool>` clone,
/// so the lock is **not** held across `await` points during tool execution.
/// This means a long-running `bash` tool call (seconds) does not block a
/// concurrent `register` from an MCP server.
///
/// ## Why `Arc<dyn Tool>` instead of `Box<dyn Tool>`
///
/// `Box` would require the lock guard to be held for as long as the `&dyn Tool`
/// reference is used — including across `.await` in `execute`. `Arc` clone is
/// a single atomic operation, and the clone is `'static` so it can be held
/// across `.await` without holding the lock.
pub struct ToolRegistry {
    tools: RwLock<HashMap<String, Arc<dyn Tool>>>,
}

impl ToolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self { tools: RwLock::new(HashMap::new()) }
    }

    /// Register a tool at runtime.
    ///
    /// Returns `Err(LimError::ToolAlreadyRegistered)` if a tool with the same
    /// name already exists. The registry is append-only within a session;
    /// to replace a tool, call [`unregister`](Self::unregister) first.
    ///
    /// This method takes `&self` (not `&mut self`) so it works through
    /// `Arc<ToolRegistry>`, enabling MCP / external tool servers to register
    /// tools after the agent loop has started.
    pub fn register(&self, tool: impl Tool + 'static) -> Result<()> {
        let name = tool.name().to_string();
        let mut guard = self
            .tools
            .write()
            .expect("ToolRegistry mutex cannot poison (no panic path in critical section)");
        if guard.contains_key(&name) {
            return Err(LimError::ToolAlreadyRegistered(name));
        }
        guard.insert(name, Arc::new(tool));
        Ok(())
    }

    /// Unregister a tool by name.
    ///
    /// Returns `Ok(())` whether or not the tool was present (idempotent).
    /// Used by MCP / external tool servers when they disconnect, to clean up
    /// their tool registrations.
    ///
    /// Note: in-flight tool executions holding an `Arc` clone are unaffected;
    /// the tool's memory is freed only after the last `Arc` is dropped.
    pub fn unregister(&self, name: &str) -> Result<()> {
        let mut guard = self
            .tools
            .write()
            .expect("ToolRegistry mutex cannot poison (no panic path in critical section)");
        guard.remove(name);
        Ok(())
    }

    /// Look up a tool by name.
    ///
    /// Returns an `Arc<dyn Tool>` clone so the caller can use the tool without
    /// holding the registry's read lock. This is critical for long-running
    /// tool executions (e.g., `bash` commands) that would otherwise block
    /// concurrent registrations.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        let guard = self
            .tools
            .read()
            .expect("ToolRegistry mutex cannot poison (no panic path in critical section)");
        guard.get(name).map(Arc::clone)
    }

    /// Look up a tool's category by name.
    ///
    /// Returns `None` if the tool is not registered. Used by `ApprovalHooks`
    /// to decide whether a tool call requires user approval.
    pub fn category_of(&self, name: &str) -> Option<ToolCategory> {
        let guard = self
            .tools
            .read()
            .expect("ToolRegistry mutex cannot poison (no panic path in critical section)");
        guard.get(name).map(|t| t.category())
    }

    /// Get schemas for all registered tools (sent to the LLM).
    ///
    /// Takes a read lock and clones all schemas. Called once per turn;
    /// the cost scales with the number of registered tools.
    ///
    /// **Order is stable**: schemas are sorted by `name` lexicographically
    /// so the tool list presented to the LLM doesn't fluctuate between
    /// turns. HashMap iteration order is non-deterministic, and unstable
    /// ordering can subtly bias the LLM's tool selection.
    pub fn schemas(&self) -> Vec<ToolSchema> {
        let guard = self
            .tools
            .read()
            .expect("ToolRegistry mutex cannot poison (no panic path in critical section)");
        let mut schemas: Vec<ToolSchema> = guard.values().map(|t| t.schema()).collect();
        schemas.sort_by(|a, b| a.name.cmp(&b.name));
        schemas
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        let guard = self
            .tools
            .read()
            .expect("ToolRegistry mutex cannot poison (no panic path in critical section)");
        guard.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Return a new registry containing only the tools named in `whitelist`.
    ///
    /// Tools not in the whitelist are silently dropped. Names in the
    /// whitelist that don't match any registered tool are silently ignored.
    /// Returns an empty registry if `whitelist` is empty (NOT all tools —
    /// use `Arc::clone` of the original if you want to share the full set).
    ///
    /// Used by `AgentFactory::for_role` to enforce a role's `tools_whitelist`
    /// (e.g. a reviewer role that should only have read access).
    pub fn filtered_clone(&self, whitelist: &[String]) -> Arc<ToolRegistry> {
        let new = Arc::new(ToolRegistry::new());
        let guard = self
            .tools
            .read()
            .expect("ToolRegistry mutex cannot poison (no panic path in critical section)");
        for name in whitelist {
            if let Some(tool) = guard.get(name) {
                new.tools
                    .write()
                    .expect("ToolRegistry mutex cannot poison (no panic path in critical section)")
                    .insert(name.clone(), Arc::clone(tool));
            }
        }
        new
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Validate tool arguments against a JSON Schema.
///
/// Returns `Ok(())` if valid, or an error describing what's wrong.
pub fn validate_args(schema: &ToolSchema, args: &serde_json::Value) -> Result<()> {
    if let Err(e) = jsonschema::validate(&schema.parameters, args) {
        return Err(LimError::ToolSchemaValidation {
            tool_name: schema.name.clone(),
            reason: e.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lim_core::id::ToolCallId;
    use tokio_util::sync::CancellationToken;

    /// A minimal tool for testing registry operations.
    struct EchoTool {
        name: String,
    }

    impl EchoTool {
        fn new(name: &str) -> Self {
            Self { name: name.to_string() }
        }
    }

    impl Tool for EchoTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema::new(&self.name, "echo tool", serde_json::json!({"type": "object"}))
        }
        fn execute(
            &self,
            id: ToolCallId,
            _args: serde_json::Value,
            _cancel: CancellationToken,
        ) -> crate::tool::ToolFuture<Result<lim_core::tool::ToolResult>> {
            Box::pin(async move { Ok(lim_core::tool::ToolResult::success(id, "ok")) })
        }
    }

    #[test]
    fn register_and_get() {
        let reg = ToolRegistry::new();
        assert!(reg.is_empty());

        reg.register(EchoTool::new("echo")).expect("register");
        assert_eq!(reg.len(), 1);
        assert!(!reg.is_empty());

        let tool = reg.get("echo").expect("tool should exist");
        assert_eq!(tool.name(), "echo");
    }

    #[test]
    fn register_duplicate_returns_error() {
        let reg = ToolRegistry::new();
        reg.register(EchoTool::new("echo")).expect("first register");
        let err = reg.register(EchoTool::new("echo")).unwrap_err();
        assert!(matches!(err, LimError::ToolAlreadyRegistered(_)));
    }

    #[test]
    fn register_duplicate_does_not_replace() {
        let reg = ToolRegistry::new();
        reg.register(EchoTool::new("echo")).expect("first register");
        assert!(reg.register(EchoTool::new("echo")).is_err());
        // Original still present
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn unregister_removes_tool() {
        let reg = ToolRegistry::new();
        reg.register(EchoTool::new("echo")).expect("register");
        assert_eq!(reg.len(), 1);

        reg.unregister("echo").expect("unregister");
        assert!(reg.is_empty());
        assert!(reg.get("echo").is_none());
    }

    #[test]
    fn unregister_missing_is_ok() {
        let reg = ToolRegistry::new();
        // Unregistering a non-existent tool is idempotent
        reg.unregister("nonexistent").expect("unregister");
    }

    #[test]
    fn register_after_unregister_succeeds() {
        let reg = ToolRegistry::new();
        reg.register(EchoTool::new("echo")).expect("first register");
        reg.unregister("echo").expect("unregister");
        // Can re-register after unregister
        reg.register(EchoTool::new("echo")).expect("re-register");
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn schemas_returns_all() {
        let reg = ToolRegistry::new();
        reg.register(EchoTool::new("a")).expect("register a");
        reg.register(EchoTool::new("b")).expect("register b");

        let schemas = reg.schemas();
        assert_eq!(schemas.len(), 2);
    }

    #[test]
    fn schemas_sorted_by_name() {
        // Register in non-sorted order; schemas() must return sorted by name.
        let reg = ToolRegistry::new();
        reg.register(EchoTool::new("zebra")).expect("register zebra");
        reg.register(EchoTool::new("alpha")).expect("register alpha");
        reg.register(EchoTool::new("mike")).expect("register mike");

        let schemas = reg.schemas();
        let names: Vec<&str> = schemas.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mike", "zebra"]);
    }

    #[test]
    fn schemas_order_stable_across_calls() {
        // Two calls to schemas() must return the same order.
        let reg = ToolRegistry::new();
        for name in ["zebra", "alpha", "mike", "bravo", "tango"] {
            reg.register(EchoTool::new(name)).expect("register");
        }
        let first: Vec<String> = reg.schemas().into_iter().map(|s| s.name).collect();
        let second: Vec<String> = reg.schemas().into_iter().map(|s| s.name).collect();
        assert_eq!(first, second);
    }

    #[test]
    fn category_of_returns_category() {
        let reg = ToolRegistry::new();
        reg.register(EchoTool::new("echo")).expect("register");
        assert_eq!(reg.category_of("echo"), Some(ToolCategory::ReadOnly));
        assert_eq!(reg.category_of("missing"), None);
    }

    #[test]
    fn get_returns_arc_that_outlives_registry() {
        // Verify the returned Arc<dyn Tool> is usable without holding the lock.
        let reg = ToolRegistry::new();
        reg.register(EchoTool::new("echo")).expect("register");
        let tool = reg.get("echo").expect("tool exists");
        // Drop the registry — tool should still be valid (Arc keeps it alive)
        drop(reg);
        assert_eq!(tool.name(), "echo");
    }

    #[test]
    fn runtime_register_through_arc() {
        // Simulate MCP server registering a tool after the agent loop starts:
        // the registry is shared via Arc, register takes &self.
        let reg = Arc::new(ToolRegistry::new());
        reg.register(EchoTool::new("builtin")).expect("register builtin");

        // Later, a new tool is registered through the Arc reference
        reg.register(EchoTool::new("mcp_tool")).expect("register mcp");

        assert_eq!(reg.len(), 2);
        assert!(reg.get("mcp_tool").is_some());
    }

    #[test]
    fn validate_args_passes() {
        let schema = ToolSchema::new(
            "test",
            "test tool",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" }
                },
                "required": ["name"]
            }),
        );
        let args = serde_json::json!({"name": "hello"});
        assert!(validate_args(&schema, &args).is_ok());
    }

    #[test]
    fn validate_args_fails_missing_required() {
        let schema = ToolSchema::new(
            "test",
            "test tool",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" }
                },
                "required": ["name"]
            }),
        );
        let args = serde_json::json!({});
        let result = validate_args(&schema, &args);
        assert!(result.is_err());
    }

    #[test]
    fn filtered_clone_returns_only_whitelisted() {
        let reg = ToolRegistry::new();
        reg.register(EchoTool::new("read")).expect("register read");
        reg.register(EchoTool::new("write")).expect("register write");
        reg.register(EchoTool::new("shell")).expect("register shell");
        assert_eq!(reg.len(), 3);

        let whitelist = vec!["read".to_string(), "ls".to_string()]; // "ls" not registered
        let filtered = reg.filtered_clone(&whitelist);
        assert_eq!(filtered.len(), 1, "only 'read' should be in filtered registry");
        assert!(filtered.get("read").is_some());
        assert!(filtered.get("write").is_none());
        assert!(filtered.get("shell").is_none());
    }

    #[test]
    fn filtered_clone_empty_whitelist_returns_empty() {
        let reg = ToolRegistry::new();
        reg.register(EchoTool::new("read")).expect("register read");
        reg.register(EchoTool::new("write")).expect("register write");

        let filtered = reg.filtered_clone(&[]);
        assert!(filtered.is_empty());
    }

    #[test]
    fn filtered_clone_shares_same_tool_instances() {
        // Verify that filtered_clone shares the same Arc<dyn Tool> instances
        // (not clones of the tool itself) — this is important for tools that
        // hold shared state (e.g. an executor handle).
        let reg = ToolRegistry::new();
        reg.register(EchoTool::new("read")).expect("register read");

        let filtered = reg.filtered_clone(&["read".to_string()]);
        let tool_from_original = reg.get("read").expect("from original");
        let tool_from_filtered = filtered.get("read").expect("from filtered");
        // Arc::ptr_eq verifies they point to the same allocation
        assert!(Arc::ptr_eq(&tool_from_original, &tool_from_filtered));
    }
}
