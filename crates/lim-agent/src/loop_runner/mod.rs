//! Loop Engineering: Goal Loop execution across multiple sessions.
//!
//! This module implements the "Goal Loop" — a cross-session loop that drives
//! an agent toward a user-defined goal by running multiple `AgentLoop`
//! sessions, persisting state between them, and verifying goal completion.
//!
//! ## Architecture
//!
//! ```text
//! LoopRunner (Goal Loop)
//! ├── StateLedger      — cross-session state persistence (.lim/loop/)
//! ├── Verifier         — goal completion verification
//! ├── Budget           — resource limits (sessions / tokens / cost)
//! └── AgentFactory     — creates a fresh AgentLoop per session
//! ```
//!
//! See `docs/architecture/loop-engineering.md` for the full design.

pub mod backoff;
pub mod budget;
pub mod controller;
pub mod daemon;
pub mod events;
pub mod goal_runner;
pub mod ipc;
pub mod ledger;
pub mod progress;
pub mod runner;
pub mod state;
pub mod timer;
pub mod timer_loop;
pub mod verifier;

pub use backoff::{BackoffStrategy, FixedBackoff};
pub use budget::Budget;
pub use controller::{
    Controller, ControllerRegistry, CustomController, FixClippyController, GoalRecipe,
    WriteTestsController,
};
pub use daemon::{DaemonCommand, DaemonHandle, DaemonResponse, DaemonServer};
pub use events::{BudgetRemaining, LoopEvent, loop_event_channel};
pub use goal_runner::{GoalRunner, LoggingGoalRunner, into_runner_arc};
pub use ipc::{IpcTransport, NullTransport};
pub use ledger::{DaemonLedger, LedgerBackend, WorkspaceBinding, into_ledger_arc};
pub use progress::{DEFAULT_NO_PROGRESS_THRESHOLD, NoProgressDetector};
pub use runner::{AgentFactory, GoalResult, LoopRunner};
pub use state::{GoalStatus, LoopState, StateLedger};
pub use timer::{IntervalTimer, Timer, TimerError};
pub use timer_loop::TimerLoop;
pub use verifier::{
    AllPassVerifier, AlwaysPassVerifier, CommandVerifier, LlmVerifier, ReviewerVerifier,
    SkillVerifier, Verifier, VerifyResult,
};
