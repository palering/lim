pub mod bash;
pub mod edit;
pub mod read;
pub mod write;

use std::sync::Arc;

use lim_agent::registry::ToolRegistry;
use lim_executor::executor::Executor;

/// Register all built-in tools into a registry.
pub fn register_builtins(registry: &mut ToolRegistry, executor: Arc<dyn Executor>) {
    registry.register(read::ReadFileTool::new(Arc::clone(&executor)));
    registry.register(write::WriteFileTool::new(Arc::clone(&executor)));
    registry.register(edit::EditFileTool::new(Arc::clone(&executor)));
    registry.register(bash::BashTool::new(executor));
}
