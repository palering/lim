//! lim-agent: the agent loop, hooks, and orchestration primitives.
//!
//! Contains [`AgentLoop`] (the core turn-by-turn driver), [`AgentHooks`] for
//! intercepting tool calls and editing context, [`ToolRegistry`] for
//! dispatching tools by name, [`ContextWindowManager`] for compaction, and
//! [`StreamAccumulator`] for assembling provider stream events into messages.

pub mod agent_loop;
pub mod compaction;
pub mod context_window;
pub mod hooks;
pub mod loop_runner;
pub mod multiagent;
pub mod prompt_builder;
pub mod registry;
pub mod stream_accumulator;
pub mod tool;

// Re-export key types.
pub use agent_loop::{AgentConfig, AgentLoop, AgentRoleSpec};
pub use compaction::{CompactRequest, CompactionStrategy, SummarizeStrategy};
pub use context_window::{CompactionResult, ContextConfig, ContextWindowManager};
pub use hooks::{AgentHooks, BeforeToolDecision, SessionEndReason, StopContext, StopDecision};
pub use loop_runner::{
    AgentFactory, AllPassVerifier, AlwaysPassVerifier, BackoffStrategy, Budget, BudgetRemaining,
    CommandVerifier, Controller, ControllerRegistry, CustomController,
    DEFAULT_NO_PROGRESS_THRESHOLD, DaemonCommand, DaemonHandle, DaemonLedger, DaemonResponse,
    DaemonServer, FixClippyController, FixedBackoff, GoalRecipe, GoalResult, GoalRunner,
    GoalStatus, IntervalTimer, IpcTransport, LedgerBackend, LlmVerifier, LoggingGoalRunner,
    LoopEvent, LoopRunner, LoopState, NoProgressDetector, NullTransport, ReviewerVerifier,
    StateLedger, Timer, TimerError, TimerLoop, Verifier, VerifyResult, WorkspaceBinding,
    WriteTestsController, into_ledger_arc, into_runner_arc, loop_event_channel,
};
pub use multiagent::{
    AgentHandle, AgentStatus, AgentTransport, CentralizedTopology, LocalChannelTransport,
    LocalTopology, NetworkTopology, RoutingHub, Scheduler, SpawnOptions, Workspace, WorkspaceId,
    WorkspaceRegistry, WorktreeHandle, WorktreeId, WorktreeSpec, create_worktree,
    into_topology_arc,
};
pub use prompt_builder::{PromptBuilder, build_system_reminder};
pub use registry::{ToolRegistry, validate_args};
pub use stream_accumulator::StreamAccumulator;
pub use tool::Tool;
