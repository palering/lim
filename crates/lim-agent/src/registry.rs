use std::collections::HashMap;

use lim_core::error::{LimError, Result};
use lim_core::tool::{ToolCategory, ToolSchema};

use crate::tool::Tool;

/// Registry of available tools, indexed by name.
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool. Panics if a tool with the same name already exists.
    pub fn register(&mut self, tool: impl Tool + 'static) {
        let name = tool.name().to_string();
        assert!(
            !self.tools.contains_key(&name),
            "tool `{name}` already registered"
        );
        self.tools.insert(name, Box::new(tool));
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    /// Look up a tool's category by name.
    ///
    /// Returns `None` if the tool is not registered. Used by `ApprovalHooks`
    /// to decide whether a tool call requires user approval.
    pub fn category_of(&self, name: &str) -> Option<ToolCategory> {
        self.tools.get(name).map(|t| t.category())
    }

    /// Get schemas for all registered tools (sent to the LLM).
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools.values().map(|t| t.schema()).collect()
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
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
}
