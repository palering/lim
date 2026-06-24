pub mod content;
pub mod error;
pub mod event;
pub mod id;
pub mod message;
pub mod stop_reason;
pub mod tool;
pub mod usage;

// Re-export key types at crate root for ergonomic use.
pub use content::ContentBlock;
pub use error::{LimError, Result};
pub use event::{AgentEvent, EventEnvelope};
pub use id::{
    ArtifactId, BranchId, MessageId, SessionId, ToolCallId, TraceId, TurnId,
};
pub use message::{AgentMessage, LlmMessage, Role, TurnResult, convert_to_llm};
pub use stop_reason::StopReason;
pub use tool::{ToolCall, ToolResult, ToolSchema};
pub use usage::Usage;
