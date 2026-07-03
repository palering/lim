//! lim-executor: tool execution and permission policy.
//!
//! Provides the [`Executor`] trait (sync-ish IO + path-aware command running),
//! [`LocalExecutor`] (the default in-process executor), and [`DefaultPolicy`]
//! for denylisting commands and validating paths against workspace roots.
//! Implements [`ToolGuard`] so the approval layer can read the trust level.

pub mod executor;
pub mod local;
pub mod policy;

// Re-export key types.
pub use executor::{
    CommandOutput, CommandSpec, EditReport, Executor, ExecutorCapabilities, FileEdit,
};
pub use local::LocalExecutor;
pub use policy::{DefaultPolicy, PermissionPolicy};
