pub mod executor;
pub mod local;
pub mod policy;

// Re-export key types.
pub use executor::{
    CommandOutput, CommandSpec, EditReport, Executor, ExecutorCapabilities, FileEdit,
};
pub use local::LocalExecutor;
pub use policy::{DefaultPolicy, PermissionPolicy};
