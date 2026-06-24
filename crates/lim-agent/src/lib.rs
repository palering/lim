pub mod agent_loop;
pub mod context_window;
pub mod hooks;
pub mod registry;
pub mod stream_accumulator;
pub mod tool;

// Re-export key types.
pub use agent_loop::{AgentConfig, AgentLoop};
pub use context_window::{CompactionResult, ContextConfig, ContextWindowManager};
pub use hooks::{AgentHooks, BeforeToolDecision};
pub use registry::{ToolRegistry, validate_args};
pub use stream_accumulator::StreamAccumulator;
pub use tool::Tool;
