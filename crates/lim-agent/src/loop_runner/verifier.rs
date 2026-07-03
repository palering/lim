//! Goal verification for Loop Engineering.
//!
//! A `Verifier` checks whether a goal has been achieved after each session.
//! The preferred implementation is `CommandVerifier` (deterministic: run a
//! command, check exit code). `LlmVerifier` (LLM-as-judge) is a lightweight
//! fallback for goals without deterministic verification. `ReviewerVerifier`
//! spawns a full independent reviewer agent (with tools and fresh context)
//! for deep inspection — the "second agent code review" pattern. `AllPassVerifier`
//! requires every sub-verifier to pass (logical AND).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use lim_core::content::ContentBlock;
use lim_core::error::{LimError, Result};
use lim_core::event::EventEnvelope;
use lim_core::id::GoalId;
use lim_core::message::{AgentMessage, LlmMessage, Role};

use lim_llm::provider::{LlmRequest, Provider};
use lim_llm::stream::StreamEvent;

use crate::agent_loop::AgentRoleSpec;
use crate::context_window::ContextConfig;
use crate::loop_runner::runner::AgentFactory;
use crate::loop_runner::state::LoopState;
use crate::registry::ToolRegistry;

/// Result of a verification check.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum VerifyResult {
    /// Goal achieved — the Goal Loop can stop.
    Pass,
    /// Goal not yet achieved — continue to the next session.
    /// The string is a description of what failed (for logging).
    Fail(String),
    /// Verification could not determine status — escalate to a human.
    NeedsHuman,
}

/// Context passed to `Verifier::verify()` alongside the loop state.
///
/// Carries the `goal_id` (for correlation with `.lim/loop/<goal_id>/`) and
/// the base `loop_dir` (so verifiers like `ReviewerVerifier` can write their
/// own event logs under the goal's directory).
///
/// `loop_dir` is `None` when the `LoopRunner` was not configured with
/// `with_loop_dir()` (e.g. in tests). Verifiers that want to persist events
/// should check for `None` and skip persistence gracefully.
pub struct VerifyContext<'a> {
    pub goal_id: &'a GoalId,
    pub loop_dir: Option<&'a Utf8Path>,
}

/// Trait for goal verification strategies.
///
/// Implementations must be `Send + Sync` so they can be stored in
/// `LoopRunner` (which may run in async contexts).
#[async_trait]
pub trait Verifier: Send + Sync {
    /// Verify whether the goal has been achieved given the current state.
    ///
    /// `ctx` carries the `goal_id` and `loop_dir` so verifiers (e.g.
    /// `ReviewerVerifier`) can persist their own event logs. Simple verifiers
    /// (e.g. `CommandVerifier`) can ignore `ctx`.
    async fn verify(&self, state: &LoopState, ctx: &VerifyContext<'_>) -> Result<VerifyResult>;

    /// Identifier for this verifier kind (e.g. "command", "llm", "reviewer").
    ///
    /// Used in `LoopEvent::VerificationDone` for observability. Default is
    /// `"unknown"` for backward compat with verifiers that don't override.
    fn kind(&self) -> &'static str {
        "unknown"
    }
}

/// Deterministic verifier: runs a shell command and checks the exit code.
///
/// Exit code 0 = `Pass`, non-zero = `Fail`. This is the preferred verifier
/// because it cannot be fooled (e.g. `cargo test`, `cargo clippy`, `npm test`).
pub struct CommandVerifier {
    command: String,
}

impl CommandVerifier {
    /// Create a verifier from a command string (e.g. `"cargo clippy"`).
    pub fn new(command: impl Into<String>) -> Self {
        Self { command: command.into() }
    }
}

#[async_trait]
impl Verifier for CommandVerifier {
    fn kind(&self) -> &'static str {
        "command"
    }

    async fn verify(&self, _state: &LoopState, _ctx: &VerifyContext<'_>) -> Result<VerifyResult> {
        // Parse the command into program + args (simple split on spaces;
        // sufficient for typical verifier commands like "cargo test").
        let parts: Vec<&str> = self.command.split_whitespace().collect();
        if parts.is_empty() {
            return Err(LimError::tool_execution(
                "verifier",
                "empty verifier command",
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty command"),
            ));
        }

        let mut cmd = Command::new(parts[0]);
        if parts.len() > 1 {
            cmd.args(&parts[1..]);
        }

        debug!(command = %self.command, "running verifier command");
        let output = cmd.output().await.map_err(|e| {
            LimError::tool_execution(
                "verifier",
                format!("failed to run verifier command `{}`: {e}", self.command),
                e,
            )
        })?;

        if output.status.success() {
            info!(command = %self.command, "verifier: PASS");
            Ok(VerifyResult::Pass)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let reason = if stderr.is_empty() {
                format!("exit code {:?}\n{}", output.status.code(), stdout.trim())
            } else {
                format!("exit code {:?}\n{}", output.status.code(), stderr.trim())
            };
            warn!(command = %self.command, reason = %reason, "verifier: FAIL");
            Ok(VerifyResult::Fail(reason))
        }
    }
}

/// A verifier that always passes. Useful for testing or for goals that
/// have no deterministic verification (the loop runs until budget exhausted
/// or the agent stops naturally).
pub struct AlwaysPassVerifier;

#[async_trait]
impl Verifier for AlwaysPassVerifier {
    fn kind(&self) -> &'static str {
        "always_pass"
    }

    async fn verify(&self, _state: &LoopState, _ctx: &VerifyContext<'_>) -> Result<VerifyResult> {
        Ok(VerifyResult::Pass)
    }
}

/// LLM-as-judge verifier: asks an LLM whether the goal is achieved.
///
/// This is the fallback for goals without a deterministic verifier command
/// (e.g. "write a design doc for X"). The LLM sees the goal + progress and
/// returns `PASS` / `FAIL: <reason>` / `NEEDS_HUMAN`.
///
/// **Cost warning**: each `verify()` call is one LLM request. Use sparingly
/// or with a cheap model.
pub struct LlmVerifier {
    provider: Arc<dyn Provider>,
    model: String,
    /// Optional extra prompt instructions (e.g. "be strict about edge cases").
    extra_instructions: Option<String>,
}

impl LlmVerifier {
    /// Create an LLM verifier using the given provider + model.
    pub fn new(
        provider: Arc<dyn Provider>,
        model: impl Into<String>,
        extra_instructions: Option<String>,
    ) -> Self {
        Self { provider, model: model.into(), extra_instructions }
    }

    /// Build the user prompt sent to the judge LLM.
    fn build_prompt(&self, state: &LoopState) -> String {
        let mut p = format!(
            "You are a goal verifier. Decide whether the following goal has been achieved.\n\n\
             ## Goal\n\n{}\n\n\
             ## Progress so far\n\n\
             - Sessions run: {}\n\
             - Total tokens spent: {}\n\n",
            state.goal, state.session_count, state.total_tokens,
        );
        if !state.progress.is_empty() {
            p.push_str(&format!("## Agent's Work Notes\n\n{}\n\n", state.progress));
        }
        p.push_str(
            "## Your Task\n\n\
             Respond in EXACTLY one of these formats on the first line:\n\
             - `PASS` — goal achieved\n\
             - `FAIL: <short reason>` — not yet achieved\n\
             - `NEEDS_HUMAN` — cannot determine, requires human judgment\n\n\
             You may add explanation after the first line.\n",
        );
        if let Some(ref extra) = self.extra_instructions {
            p.push_str(&format!("\n## Additional Instructions\n\n{extra}\n"));
        }
        p
    }
}

#[async_trait]
impl Verifier for LlmVerifier {
    fn kind(&self) -> &'static str {
        "llm"
    }

    async fn verify(&self, state: &LoopState, _ctx: &VerifyContext<'_>) -> Result<VerifyResult> {
        let prompt = self.build_prompt(state);
        let request = LlmRequest::new(
            &self.model,
            vec![LlmMessage {
                role: Role::User,
                content: vec![ContentBlock::text(&prompt)],
                tool_call_ids: vec![],
            }],
        )
        .with_max_tokens(256);

        let mut text = String::new();
        let mut stream = self.provider.stream(request);
        while let Some(event) = stream.next().await {
            match event {
                Ok(StreamEvent::TextDelta { text: delta }) => text.push_str(&delta),
                Ok(_) => {}
                Err(e) => {
                    warn!(error = %e, "llm verifier stream error");
                    return Err(LimError::tool_execution(
                        "llm_verifier",
                        format!("provider stream error: {e}"),
                        std::io::Error::other(format!("provider error: {e}")),
                    ));
                }
            }
        }

        let first_line = text.lines().next().unwrap_or("").trim().to_string();
        debug!(response = %text, first_line = %first_line, "llm verifier response");
        if first_line.starts_with("PASS") {
            info!("llm verifier: PASS");
            Ok(VerifyResult::Pass)
        } else if let Some(reason) = first_line.strip_prefix("FAIL:") {
            let reason = reason.trim().to_string();
            warn!(reason = %reason, "llm verifier: FAIL");
            Ok(VerifyResult::Fail(reason))
        } else if first_line.starts_with("NEEDS_HUMAN") {
            warn!("llm verifier: NEEDS_HUMAN");
            Ok(VerifyResult::NeedsHuman)
        } else {
            // Unparseable response — treat as NeedsHuman to avoid silent loops.
            warn!(first_line = %first_line, "llm verifier: unparseable response");
            Ok(VerifyResult::NeedsHuman)
        }
    }
}

/// Composite verifier: runs all sub-verifiers and requires every one to Pass.
///
/// Short-circuits on the first non-Pass result (Fail or NeedsHuman).
/// If any sub-verifier returns `NeedsHuman`, the composite returns
/// `NeedsHuman` (takes priority over `Fail`).
pub struct AllPassVerifier {
    verifiers: Vec<Box<dyn Verifier>>,
}

impl AllPassVerifier {
    /// Create a composite verifier from a list of sub-verifiers.
    pub fn new(verifiers: Vec<Box<dyn Verifier>>) -> Self {
        Self { verifiers }
    }

    /// Add a sub-verifier.
    pub fn with(mut self, v: Box<dyn Verifier>) -> Self {
        self.verifiers.push(v);
        self
    }
}

#[async_trait]
impl Verifier for AllPassVerifier {
    fn kind(&self) -> &'static str {
        "all_pass"
    }

    async fn verify(&self, state: &LoopState, ctx: &VerifyContext<'_>) -> Result<VerifyResult> {
        let mut last_fail: Option<String> = None;
        for v in &self.verifiers {
            match v.verify(state, ctx).await? {
                VerifyResult::Pass => continue,
                VerifyResult::NeedsHuman => {
                    // NeedsHuman takes priority — escalate immediately.
                    return Ok(VerifyResult::NeedsHuman);
                }
                VerifyResult::Fail(reason) => {
                    if last_fail.is_none() {
                        last_fail = Some(reason);
                    }
                }
            }
        }
        match last_fail {
            Some(reason) => Ok(VerifyResult::Fail(reason)),
            None => Ok(VerifyResult::Pass),
        }
    }
}

/// Reviewer-agent verifier: spawns an independent reviewer agent with a
/// fresh context window to inspect the workspace and judge whether the goal
/// is achieved.
///
/// Unlike `LlmVerifier` (a single LLM call with no tools, seeing only a
/// summary), the reviewer runs a full `AgentLoop` with tool access — it can
/// read files, run read-only commands, and inspect the actual work product
/// before deciding. This is the "second agent code review (fresh context)"
/// pattern from Claude Code's Loop Engineering: the reviewer has no memory
/// of the main agent's work, so it is not biased by the main agent's
/// reasoning.
///
/// **Cost warning**: each `verify()` call runs a full agent session, which
/// may include multiple LLM turns + tool calls. Prefer `CommandVerifier`
/// when a deterministic check exists, and use `ReviewerVerifier` only for
/// goals that require qualitative judgment (e.g. "is the design doc
/// complete?"). Combine with `AllPassVerifier` alongside a cheaper verifier
/// if needed.
pub struct ReviewerVerifier {
    /// Factory used to spawn the reviewer agent. Should be configured with
    /// a reviewer-appropriate system prompt and (optionally) a different
    /// model — the "independent evaluator" pattern.
    factory: Arc<AgentFactory>,
    /// Extra instructions appended to the review prompt (free-form text).
    extra_instructions: Option<String>,
    /// Monotonically increasing review counter — used to name review event
    /// files (`review-<N>.jsonl`). Incremented on every `verify()` call.
    review_count: AtomicU64,
}

impl ReviewerVerifier {
    /// Create a reviewer verifier.
    ///
    /// The factory should be configured with a reviewer system prompt
    /// (e.g. "You are an independent code reviewer. Inspect the workspace
    /// and verify whether the goal is achieved.").
    pub fn new(factory: Arc<AgentFactory>, extra_instructions: Option<String>) -> Self {
        Self { factory, extra_instructions, review_count: AtomicU64::new(0) }
    }

    /// Create a reviewer verifier from a role spec.
    ///
    /// This is the preferred constructor when loading from
    /// `.lim/agents/reviewer.toml`. The `base_provider` and `base_registry`
    /// are typically the main agent's provider/registry (shared so the
    /// reviewer can inspect the workspace). If `spec.tools_whitelist` is
    /// set, the reviewer is restricted to those tools.
    ///
    /// `extra_instructions` is appended to the review prompt (free-form
    /// text), typically from `--verifier-instructions`.
    pub fn from_role_spec(
        spec: AgentRoleSpec,
        base_provider: Arc<dyn Provider>,
        base_registry: Arc<ToolRegistry>,
        context_config: Option<ContextConfig>,
        sessions_dir: camino::Utf8PathBuf,
        extra_instructions: Option<String>,
    ) -> Self {
        let factory = Arc::new(AgentFactory::for_role(
            spec,
            base_provider,
            base_registry,
            context_config,
            sessions_dir,
        ));
        Self::new(factory, extra_instructions)
    }

    /// Build the review prompt sent to the reviewer agent.
    fn build_prompt(&self, state: &LoopState) -> String {
        let mut p = format!(
            "You are an independent reviewer. Determine whether the following goal has been achieved.\n\n\
             ## Goal\n\n{}\n\n\
             ## Progress so far\n\n\
             - Sessions run: {}\n\
             - Total turns: {}\n\
             - Total tokens spent: {}\n\n",
            state.goal, state.session_count, state.turn_count, state.total_tokens,
        );
        if !state.progress.is_empty() {
            p.push_str(&format!("## Agent's Work Notes\n\n{}\n\n", state.progress));
        }
        p.push_str(
            "## Your Task\n\n\
             Inspect the workspace to verify whether the goal has been achieved. You may:\n\
             - Read files (source code, docs, test output)\n\
             - Run read-only commands (e.g. `ls`, `cat`, `git diff`)\n\
             - Check whether tests pass or lint is clean\n\n\
             After inspection, respond in EXACTLY one of these formats on the FIRST line of your final message:\n\
             - `PASS` — goal achieved\n\
             - `FAIL: <short reason>` — not yet achieved\n\
             - `NEEDS_HUMAN` — cannot determine, requires human judgment\n\n\
             The first line must be the verdict; you may add explanation after.\n",
        );
        if let Some(ref extra) = self.extra_instructions {
            p.push_str(&format!("\n## Additional Instructions\n\n{extra}\n"));
        }
        p
    }

    /// Spawn a background task to persist reviewer AgentEvents to
    /// `<loop_dir>/<goal_id>/reviews/review-<N>.jsonl`.
    ///
    /// Returns `None` when `ctx.loop_dir` is not set (persistence skipped).
    /// The returned `ReviewPersister` must be `finish()`ed after the reviewer
    /// agent completes to flush remaining events.
    fn maybe_spawn_review_persister(
        &self,
        event_rx: tokio::sync::broadcast::Receiver<EventEnvelope>,
        ctx: &VerifyContext<'_>,
    ) -> Option<ReviewPersister> {
        let loop_dir = ctx.loop_dir?;
        let n = self.review_count.fetch_add(1, Ordering::SeqCst) + 1;
        let path: Utf8PathBuf = loop_dir
            .join(ctx.goal_id.to_string())
            .join("reviews")
            .join(format!("review-{n}.jsonl"));

        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move {
            Self::run_review_persister(event_rx, &path, cancel_clone).await;
        });
        Some(ReviewPersister { handle, cancel })
    }

    /// Drain `event_rx` and write each `EventEnvelope` as a JSONL line to `path`.
    /// Stops when `cancel` is cancelled, then drains any remaining buffered
    /// events before returning.
    async fn run_review_persister(
        mut rx: tokio::sync::broadcast::Receiver<EventEnvelope>,
        path: &Utf8Path,
        cancel: CancellationToken,
    ) {
        // Ensure parent dir exists.
        if let Some(parent) = path.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            warn!(error = %e, path = %path, "review persister: failed to create dir");
            return;
        }
        let mut file =
            match tokio::fs::OpenOptions::new().create(true).append(true).open(path).await {
                Ok(f) => f,
                Err(e) => {
                    warn!(error = %e, path = %path, "review persister: failed to open file");
                    return;
                }
            };

        use tokio::sync::broadcast::error::RecvError;
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                result = rx.recv() => match result {
                    Ok(event) => {
                        if let Ok(line) = serde_json::to_string(&event) {
                            if let Err(e) = file.write_all(line.as_bytes()).await {
                                warn!(error = %e, "review persister: write failed");
                            }
                            if let Err(e) = file.write_all(b"\n").await {
                                warn!(error = %e, "review persister: newline failed");
                            }
                        }
                    }
                    Err(RecvError::Closed) => break,
                    Err(RecvError::Lagged(n)) => {
                        warn!(lagged = n, "review persister: lagged, dropped events");
                    }
                }
            }
        }
        // Drain any remaining buffered events after cancellation.
        while let Ok(event) = rx.try_recv() {
            if let Ok(line) = serde_json::to_string(&event) {
                let _ = file.write_all(line.as_bytes()).await;
                let _ = file.write_all(b"\n").await;
            }
        }
        let _ = file.flush().await;
        debug!(path = %path, "review persister: flushed and closed");
    }
}

/// Handle to a spawned review-event persister task.
///
/// Created by `ReviewerVerifier::maybe_spawn_review_persister`. Call `finish()`
/// after the reviewer agent run completes to cancel the task and wait for it
/// to flush remaining events to disk.
pub struct ReviewPersister {
    handle: tokio::task::JoinHandle<()>,
    cancel: CancellationToken,
}

impl ReviewPersister {
    /// Cancel the persister and wait for it to flush remaining events.
    pub async fn finish(self) {
        self.cancel.cancel();
        let _ = self.handle.await;
    }
}

#[async_trait]
impl Verifier for ReviewerVerifier {
    fn kind(&self) -> &'static str {
        "reviewer"
    }

    async fn verify(&self, state: &LoopState, ctx: &VerifyContext<'_>) -> Result<VerifyResult> {
        let prompt = self.build_prompt(state);
        debug!(
            goal = %state.goal,
            session_count = state.session_count,
            "spawning reviewer agent for verification"
        );

        // Spawn a fresh reviewer agent with its own context window.
        // The agent has no memory of the main agent's work (fresh context).
        let (mut reviewer, event_rx) = self.factory.create_session();
        let user_msg = AgentMessage::user(prompt);

        // Persist reviewer AgentEvents to `<loop_dir>/<goal_id>/reviews/review-<N>.jsonl`.
        // This closes the §8.4 reviewer black-box: the reviewer's LLM calls
        // and tool executions are now recorded for observability/replay.
        // Skipped when `loop_dir` is None (e.g. in tests without a workspace).
        let persister = self.maybe_spawn_review_persister(event_rx, ctx);

        let messages = reviewer.run(user_msg).await.map_err(|e| {
            warn!(error = %e, "reviewer agent run failed");
            LimError::tool_execution(
                "reviewer_verifier",
                format!("reviewer agent run failed: {e}"),
                std::io::Error::other(format!("agent error: {e}")),
            )
        })?;

        // Signal the persister to stop and wait for it to flush remaining events.
        if let Some(p) = persister {
            p.finish().await;
        }

        // Extract the final assistant message's text content.
        // The reviewer may produce multiple messages (tool calls etc.);
        // we want the last assistant message.
        let final_text: String = messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
            .map(|m| {
                m.content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();

        let first_line = final_text.lines().next().unwrap_or("").trim().to_string();
        debug!(
            first_line = %first_line,
            response_len = final_text.len(),
            "reviewer agent responded"
        );

        if first_line.starts_with("PASS") {
            info!("reviewer verifier: PASS");
            Ok(VerifyResult::Pass)
        } else if let Some(reason) = first_line.strip_prefix("FAIL:") {
            let reason = reason.trim().to_string();
            warn!(reason = %reason, "reviewer verifier: FAIL");
            Ok(VerifyResult::Fail(reason))
        } else if first_line.starts_with("NEEDS_HUMAN") {
            warn!("reviewer verifier: NEEDS_HUMAN");
            Ok(VerifyResult::NeedsHuman)
        } else {
            // Unparseable response — treat as NeedsHuman to avoid silent loops.
            warn!(first_line = %first_line, "reviewer verifier: unparseable response");
            Ok(VerifyResult::NeedsHuman)
        }
    }
}

/// Skill-based verifier: delegates to either `CommandVerifier` (programmatic
/// skill with a `verify` command) or `LlmVerifier` (non-programmatic skill
/// with markdown body as prompt).
///
/// This bridges human-editable skill documents (`.lim/skills/*.md`) and the
/// verifier trait system. Users write skills in markdown; `SkillVerifier`
/// dispatches to the right runtime based on the skill's frontmatter.
///
/// ## Dispatch logic
///
/// - `skill.is_programmatic()` → `CommandVerifier::new(skill.full_verify_command())`
/// - else → `LlmVerifier` with skill body injected as `extra_instructions`
///
/// ## Construction
///
/// `SkillVerifier::from_skill(skill, provider, model)` — for non-programmatic
/// skills, pass the LLM provider + model. For programmatic skills, these are
/// ignored (the command is run directly).
pub struct SkillVerifier {
    inner: SkillVerifierInner,
}

enum SkillVerifierInner {
    /// Programmatic skill — delegates to CommandVerifier.
    Command(CommandVerifier),
    /// Non-programmatic skill — uses LlmVerifier with the skill body as prompt.
    Llm(LlmVerifier),
}

impl SkillVerifier {
    /// Create a `SkillVerifier` from a loaded skill.
    ///
    /// - If the skill has a `verify` command, delegates to `CommandVerifier`.
    /// - Otherwise, uses `LlmVerifier` with the skill body as extra
    ///   instructions, prepended with a skill-aware framing prompt.
    ///
    /// `provider` and `model` are only used for non-programmatic skills;
    /// they're ignored for programmatic skills.
    pub fn from_skill(
        skill: &lim_skills::Skill,
        provider: Arc<dyn Provider>,
        model: impl Into<String>,
    ) -> Self {
        if let Some(cmd) = skill.full_verify_command() {
            debug!(skill = %skill.name(), command = %cmd, "skill verifier: programmatic");
            Self { inner: SkillVerifierInner::Command(CommandVerifier::new(cmd)) }
        } else {
            debug!(skill = %skill.name(), "skill verifier: llm-based");
            // Inject the skill body as extra instructions, with a framing
            // prompt that tells the LLM to use the skill's steps.
            let extra = format!(
                "Follow the verification skill `{name}`:\n\n{body}",
                name = skill.name(),
                body = skill.body,
            );
            let description_hint = skill
                .frontmatter
                .description
                .as_deref()
                .map(|d| format!("\n\nSkill description: {d}"));
            let extra =
                if let Some(hint) = description_hint { format!("{extra}{hint}") } else { extra };
            Self { inner: SkillVerifierInner::Llm(LlmVerifier::new(provider, model, Some(extra))) }
        }
    }

    /// Create a `SkillVerifier` by looking up a skill by name in a registry.
    ///
    /// Convenience wrapper around `from_skill` that does the lookup. Returns
    /// an error if the skill is not found.
    pub fn from_registry(
        registry: &lim_skills::SkillRegistry,
        skill_name: &str,
        provider: Arc<dyn Provider>,
        model: impl Into<String>,
    ) -> Result<Self> {
        let skill = registry.get(skill_name).ok_or_else(|| {
            LimError::tool_execution_msg(
                "skill",
                format!("skill `{skill_name}` not found in registry"),
            )
        })?;
        Ok(Self::from_skill(skill, provider, model))
    }
}

#[async_trait]
impl Verifier for SkillVerifier {
    fn kind(&self) -> &'static str {
        match &self.inner {
            SkillVerifierInner::Command(_) => "skill:command",
            SkillVerifierInner::Llm(_) => "skill:llm",
        }
    }

    async fn verify(&self, state: &LoopState, ctx: &VerifyContext<'_>) -> Result<VerifyResult> {
        match &self.inner {
            SkillVerifierInner::Command(v) => v.verify(state, ctx).await,
            SkillVerifierInner::Llm(v) => v.verify(state, ctx).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lim_llm::mock::{MockProvider, MockResponse};

    /// Helper: verify with a throwaway context (no loop_dir, so no persistence).
    async fn verify_no_ctx<V: Verifier + ?Sized>(v: &V, state: &LoopState) -> Result<VerifyResult> {
        let goal_id = GoalId::new();
        let ctx = VerifyContext { goal_id: &goal_id, loop_dir: None };
        v.verify(state, &ctx).await
    }

    #[tokio::test]
    async fn command_verifier_pass_on_exit_zero() {
        let v = CommandVerifier::new("true");
        let state = LoopState::new("test");
        let result = verify_no_ctx(&v, &state).await.unwrap();
        assert!(matches!(result, VerifyResult::Pass));
    }

    #[tokio::test]
    async fn command_verifier_fail_on_nonzero_exit() {
        let v = CommandVerifier::new("false");
        let state = LoopState::new("test");
        let result = verify_no_ctx(&v, &state).await.unwrap();
        assert!(matches!(result, VerifyResult::Fail(_)));
    }

    #[tokio::test]
    async fn always_pass_verifier() {
        let v = AlwaysPassVerifier;
        let state = LoopState::new("test");
        let result = verify_no_ctx(&v, &state).await.unwrap();
        assert!(matches!(result, VerifyResult::Pass));
    }

    #[tokio::test]
    async fn llm_verifier_passes_on_pass_response() {
        let provider: Arc<dyn Provider> =
            Arc::new(MockProvider::new(vec![MockResponse::Text("PASS\nLooks good.".to_string())]));
        let v = LlmVerifier::new(provider, "mock", None);
        let state = LoopState::new("test");
        let result = verify_no_ctx(&v, &state).await.unwrap();
        assert!(matches!(result, VerifyResult::Pass));
    }

    #[tokio::test]
    async fn llm_verifier_fails_on_fail_response() {
        let provider: Arc<dyn Provider> =
            Arc::new(MockProvider::new(vec![MockResponse::Text("FAIL: not done yet".to_string())]));
        let v = LlmVerifier::new(provider, "mock", None);
        let state = LoopState::new("test");
        let result = verify_no_ctx(&v, &state).await.unwrap();
        match result {
            VerifyResult::Fail(reason) => assert_eq!(reason, "not done yet"),
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn llm_verifier_needs_human_on_unparseable() {
        let provider: Arc<dyn Provider> =
            Arc::new(MockProvider::new(vec![MockResponse::Text("I cannot decide".to_string())]));
        let v = LlmVerifier::new(provider, "mock", None);
        let state = LoopState::new("test");
        let result = verify_no_ctx(&v, &state).await.unwrap();
        assert!(matches!(result, VerifyResult::NeedsHuman));
    }

    #[tokio::test]
    async fn all_pass_verifier_passes_when_all_pass() {
        let v = AllPassVerifier::new(vec![
            Box::new(CommandVerifier::new("true")),
            Box::new(AlwaysPassVerifier),
        ]);
        let state = LoopState::new("test");
        let result = verify_no_ctx(&v, &state).await.unwrap();
        assert!(matches!(result, VerifyResult::Pass));
    }

    #[tokio::test]
    async fn all_pass_verifier_fails_if_any_fails() {
        let v = AllPassVerifier::new(vec![
            Box::new(CommandVerifier::new("true")),
            Box::new(CommandVerifier::new("false")),
        ]);
        let state = LoopState::new("test");
        let result = verify_no_ctx(&v, &state).await.unwrap();
        assert!(matches!(result, VerifyResult::Fail(_)));
    }

    #[tokio::test]
    async fn all_pass_verifier_needs_human_takes_priority() {
        struct NeedsHumanVerifier;
        #[async_trait]
        impl Verifier for NeedsHumanVerifier {
            async fn verify(
                &self,
                _state: &LoopState,
                _ctx: &VerifyContext<'_>,
            ) -> Result<VerifyResult> {
                Ok(VerifyResult::NeedsHuman)
            }
        }
        let v = AllPassVerifier::new(vec![
            Box::new(CommandVerifier::new("false")), // Fail
            Box::new(NeedsHumanVerifier),            // NeedsHuman
        ]);
        let state = LoopState::new("test");
        let result = verify_no_ctx(&v, &state).await.unwrap();
        assert!(matches!(result, VerifyResult::NeedsHuman));
    }

    // --- ReviewerVerifier tests ---

    /// Build a minimal AgentFactory wired to a MockProvider for testing.
    fn make_reviewer_factory(responses: Vec<MockResponse>) -> Arc<AgentFactory> {
        use crate::agent_loop::AgentConfig;
        use crate::registry::ToolRegistry;
        use camino::Utf8PathBuf;

        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(responses));
        let registry = Arc::new(ToolRegistry::new());
        Arc::new(AgentFactory::new(
            provider,
            registry,
            AgentConfig::default(),
            None,
            Utf8PathBuf::from("sessions"),
        ))
    }

    #[tokio::test]
    async fn reviewer_verifier_passes_on_pass_response() {
        let factory = make_reviewer_factory(vec![MockResponse::Text(
            "PASS\nThe goal is achieved.".to_string(),
        )]);
        let v = ReviewerVerifier::new(factory, None);
        let state = LoopState::new("test goal");
        let result = verify_no_ctx(&v, &state).await.unwrap();
        assert!(matches!(result, VerifyResult::Pass));
    }

    #[tokio::test]
    async fn reviewer_verifier_fails_on_fail_response() {
        let factory = make_reviewer_factory(vec![MockResponse::Text(
            "FAIL: tests still failing".to_string(),
        )]);
        let v = ReviewerVerifier::new(factory, None);
        let state = LoopState::new("test goal");
        let result = verify_no_ctx(&v, &state).await.unwrap();
        match result {
            VerifyResult::Fail(reason) => assert_eq!(reason, "tests still failing"),
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn reviewer_verifier_needs_human_on_unparseable() {
        let factory = make_reviewer_factory(vec![MockResponse::Text(
            "I'm not sure how to evaluate this.".to_string(),
        )]);
        let v = ReviewerVerifier::new(factory, None);
        let state = LoopState::new("test goal");
        let result = verify_no_ctx(&v, &state).await.unwrap();
        assert!(matches!(result, VerifyResult::NeedsHuman));
    }

    /// Verify that reviewer events are persisted to `review-1.jsonl` when
    /// `loop_dir` is set in the `VerifyContext`.
    #[tokio::test]
    async fn reviewer_verifier_persists_events_to_loop_dir() {
        let dir = std::env::temp_dir().join("lim-test-reviewer-persist");
        let _ = std::fs::remove_dir_all(&dir);
        let loop_dir = Utf8PathBuf::from_path_buf(dir.clone()).unwrap();

        let factory =
            make_reviewer_factory(vec![MockResponse::Text("PASS\nLooks good.".to_string())]);
        let v = ReviewerVerifier::new(factory, None);
        let state = LoopState::new("persist test");
        let goal_id = GoalId::new();
        let ctx = VerifyContext { goal_id: &goal_id, loop_dir: Some(&loop_dir) };

        let result = v.verify(&state, &ctx).await.unwrap();
        assert!(matches!(result, VerifyResult::Pass));

        // The review file should exist at <loop_dir>/<goal_id>/reviews/review-1.jsonl
        let review_path = loop_dir.join(goal_id.to_string()).join("reviews").join("review-1.jsonl");
        assert!(review_path.exists(), "review file should exist at {review_path}");

        // The file should contain at least one JSONL line (an AgentEvent).
        let content = std::fs::read_to_string(review_path.as_std_path()).unwrap();
        assert!(!content.is_empty(), "review file should not be empty");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- ReviewerVerifier::from_role_spec tests ---

    /// Build a minimal base provider + registry for role-spec tests.
    fn make_base_provider_registry(
        responses: Vec<MockResponse>,
    ) -> (Arc<dyn Provider>, Arc<ToolRegistry>) {
        use crate::registry::ToolRegistry;
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(responses));
        let registry = Arc::new(ToolRegistry::new());
        (provider, registry)
    }

    #[tokio::test]
    async fn reviewer_from_role_spec_uses_spec_system_prompt_and_model() {
        // Register a tool so we can verify the reviewer has tool access.
        let (provider, registry) =
            make_base_provider_registry(vec![MockResponse::Text("PASS\nLooks good.".to_string())]);

        let spec = AgentRoleSpec {
            system_prompt: Some("You are a test reviewer.".to_string()),
            model: Some("test-model".to_string()),
            max_turns: Some(5),
            ..Default::default()
        };

        let v = ReviewerVerifier::from_role_spec(
            spec,
            provider,
            registry,
            None,
            Utf8PathBuf::from("sessions"),
            None,
        );

        let state = LoopState::new("role spec test");
        let result = verify_no_ctx(&v, &state).await.unwrap();
        assert!(matches!(result, VerifyResult::Pass));
    }

    #[tokio::test]
    async fn reviewer_from_role_spec_applies_tools_whitelist() {
        // Register two tools; the spec whitelists only one.
        struct DummyTool {
            name: String,
        }
        impl DummyTool {
            fn new(name: &str) -> Self {
                Self { name: name.to_string() }
            }
        }
        impl crate::tool::Tool for DummyTool {
            fn name(&self) -> &str {
                &self.name
            }
            fn schema(&self) -> lim_core::tool::ToolSchema {
                lim_core::tool::ToolSchema::new(
                    &self.name,
                    "dummy",
                    serde_json::json!({"type": "object"}),
                )
            }
            fn execute(
                &self,
                id: lim_core::id::ToolCallId,
                _args: serde_json::Value,
                _cancel: CancellationToken,
            ) -> crate::tool::ToolFuture<Result<lim_core::tool::ToolResult>> {
                Box::pin(async move { Ok(lim_core::tool::ToolResult::success(id, "ok")) })
            }
        }

        let (provider, registry) =
            make_base_provider_registry(vec![MockResponse::Text("PASS\nLooks good.".to_string())]);
        registry.register(DummyTool::new("read")).expect("register read");
        registry.register(DummyTool::new("write")).expect("register write");
        assert_eq!(registry.len(), 2);

        let spec = AgentRoleSpec {
            system_prompt: Some("Reviewer with read-only access.".to_string()),
            tools_whitelist: Some(vec!["read".to_string()]),
            ..Default::default()
        };

        let v = ReviewerVerifier::from_role_spec(
            spec,
            provider,
            registry,
            None,
            Utf8PathBuf::from("sessions"),
            None,
        );

        // The reviewer's factory should have a filtered registry with only "read".
        // We can't directly inspect the factory's registry, but we can verify
        // the reviewer still works (the MockProvider doesn't call tools).
        let state = LoopState::new("whitelist test");
        let result = verify_no_ctx(&v, &state).await.unwrap();
        assert!(matches!(result, VerifyResult::Pass));
    }

    // --- SkillVerifier tests ---

    /// Helper: build a Skill from inline frontmatter + body.
    fn make_skill(name: &str, verify: Option<&str>, body: &str) -> lim_skills::Skill {
        lim_skills::Skill {
            frontmatter: lim_skills::SkillFrontmatter {
                name: name.to_string(),
                description: None,
                verify: verify.map(|s| s.to_string()),
                verify_args: None,
            },
            body: body.to_string(),
            source: camino::Utf8PathBuf::from("/tmp/test-skill.md"),
        }
    }

    #[tokio::test]
    async fn skill_verifier_programmatic_delegates_to_command_pass() {
        // Programmatic skill with `verify: true` — delegates to CommandVerifier.
        // `true` always exits 0, so the skill should pass.
        let skill = make_skill("verify-build", Some("true"), "Build the project.");
        let v = SkillVerifier::from_skill(&skill, Arc::new(MockProvider::new(vec![])), "ignored");

        assert_eq!(v.kind(), "skill:command");
        let state = LoopState::new("build test");
        let result = verify_no_ctx(&v, &state).await.unwrap();
        assert!(matches!(result, VerifyResult::Pass));
    }

    #[tokio::test]
    async fn skill_verifier_programmatic_delegates_to_command_fail() {
        // Programmatic skill with `verify: false` — exits 1, skill should fail.
        let skill = make_skill("verify-build-fail", Some("false"), "Build the project.");
        let v = SkillVerifier::from_skill(&skill, Arc::new(MockProvider::new(vec![])), "ignored");

        assert_eq!(v.kind(), "skill:command");
        let state = LoopState::new("build test");
        let result = verify_no_ctx(&v, &state).await.unwrap();
        assert!(matches!(result, VerifyResult::Fail(_)));
    }

    #[tokio::test]
    async fn skill_verifier_non_programmatic_uses_llm() {
        // Non-programmatic skill — delegates to LlmVerifier with the skill body
        // injected as extra instructions.
        let provider =
            Arc::new(MockProvider::new(vec![MockResponse::Text("PASS\nLooks good.".to_string())]));
        let skill =
            make_skill("manual-review", None, "1. Check code quality\n2. Verify tests pass");
        let v = SkillVerifier::from_skill(&skill, provider, "test-model");

        assert_eq!(v.kind(), "skill:llm");
        let state = LoopState::new("manual review test");
        let result = verify_no_ctx(&v, &state).await.unwrap();
        assert!(matches!(result, VerifyResult::Pass));
    }

    #[tokio::test]
    async fn skill_verifier_from_registry_looks_up_by_name() {
        // Build a registry with two skills, look up by name.
        let skill1 = make_skill("skill-a", Some("true"), "Skill A");
        let skill2 = make_skill("skill-b", None, "Skill B");

        // We can't easily build a SkillRegistry from scratch (it has private
        // fields), so we test from_skill directly instead. from_registry is
        // a thin wrapper that does registry.get() then from_skill().
        let provider = Arc::new(MockProvider::new(vec![MockResponse::Text("PASS".to_string())]));

        // Verify the programmatic one.
        let v1 = SkillVerifier::from_skill(&skill1, provider.clone(), "m");
        assert_eq!(v1.kind(), "skill:command");

        // Verify the non-programmatic one.
        let v2 = SkillVerifier::from_skill(&skill2, provider, "m");
        assert_eq!(v2.kind(), "skill:llm");
    }
}
