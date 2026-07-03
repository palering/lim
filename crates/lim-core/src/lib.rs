//! lim-core: core types and primitives for the lim agent runtime.
//!
//! Provides IDs (newtype wrappers around [`Ulid`]), error types, message
//! abstractions, tool call/result shapes, context edit descriptors, and the
//! [`ToolGuard`] trait that decouples executors from the approval layer.
//!
//! [`Ulid`]: ulid::Ulid

pub mod content;
pub mod context_edit;
pub mod env;
pub mod error;
pub mod event;
pub mod guard;
pub mod id;
pub mod inject;
pub mod loop_end;
pub mod message;
pub mod prompt;
pub mod status;
pub mod steering;
pub mod stop_reason;
pub mod tool;
pub mod usage;

// Re-export key types at crate root for ergonomic use.
pub use content::ContentBlock;
pub use context_edit::{ContextEdit, apply_edit, compose_edits};
pub use env::{ArchKind, OsKind, PlatformInfo, ShellKind};
pub use error::{LimError, Result};
pub use event::{AgentEvent, EventEnvelope};
pub use guard::{PermissiveGuard, ToolGuard, TrustLevel, WorkspaceGuard};
pub use id::{ArtifactId, BranchId, GoalId, MessageId, SessionId, ToolCallId, TraceId, TurnId};
pub use inject::InjectInfo;
pub use loop_end::SessionEndReason;
pub use message::{AgentMessage, LlmMessage, Role, TurnResult, convert_to_llm};
pub use prompt::{CacheTier, PromptLayout, PromptLayoutBuilder, PromptSegment};
pub use status::{ComponentStatus, Condition, ConditionStatus, StatusProvider};
pub use steering::SteeringMessage;
pub use stop_reason::StopReason;
pub use tool::{ToolCall, ToolResult, ToolSchema};
pub use usage::Usage;

/// Current session JSONL schema version.
///
/// Bumped ONLY when a breaking schema change is introduced that
/// `#[serde(default)]` on event fields cannot handle (e.g. field
/// renames, semantic unit changes, envelope structure rewrites).
/// Routine field additions do NOT bump this — `#[serde(default)]`
/// makes them transparent to old readers.
///
/// - `0` = legacy files written before `SessionHeader` existed
/// - `1` = first version with `SessionHeader` event
///
/// See "Session schema versioning" in `docs/architecture/behavior-trace.md`.
pub const SCHEMA_VERSION: u32 = 1;

/// lim version that wrote the current binary (compile-time constant).
///
/// Recorded in `SessionHeader` events for debug/audit. Prefer the
/// build-time git description (`LIM_BUILD_VERSION`, set by
/// `build.rs` via `git describe --tags --always --dirty=-dirty`) so
/// that `lim --version` and session JSONL report the actual release
/// tag + commit, not just the static `CARGO_PKG_VERSION`. Falls back
/// to `CARGO_PKG_VERSION` when git is unavailable (e.g. tarball build
/// without `.git`).
///
/// Consumers:
///   - `SessionHeader.lim_version` in session JSONL
///   - clap `#[command(version = LIM_VERSION)]` in the CLI
pub const LIM_VERSION: &str = match option_env!("LIM_BUILD_VERSION") {
    Some(v) if !v.is_empty() => v,
    _ => env!("CARGO_PKG_VERSION"),
};

/// Compact platform identifier for `SessionHeader`, e.g. `"macos/aarch64"`.
///
/// Uses `std::env::consts` (compile-time target) rather than
/// [`PlatformInfo::detect`] because the header only needs OS + arch
/// for cross-platform replay diagnostics, not the full shell/workspace
/// detection that `PlatformInfo` performs at runtime.
pub fn platform_string() -> String {
    format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH)
}
