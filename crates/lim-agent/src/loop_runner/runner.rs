//! LoopRunner: the Goal Loop executor.
//!
//! `LoopRunner` drives the cross-session "Goal Loop": it runs an
//! [`AgentLoop`] session, persists state, verifies the goal, and decides
//! whether to start another session or stop.
//!
//! Each session gets a fresh `AgentLoop` (new context window) via
//! [`AgentFactory`]. State is persisted via [`StateLedger`] so that an
//! interrupted goal can be resumed with [`LoopRunner::resume_goal`].

use std::sync::{Arc, Mutex};

use camino::{Utf8Path, Utf8PathBuf};
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use lim_core::error::Result;
use lim_core::event::EventEnvelope;
use lim_core::id::GoalId;
use lim_core::message::AgentMessage;

use crate::agent_loop::{AgentConfig, AgentLoop, AgentRoleSpec};
use crate::context_window::{ContextConfig, ContextWindowManager};
use crate::hooks::AgentHooks;
use crate::registry::ToolRegistry;

use super::backoff::BackoffStrategy;
use super::budget::Budget;
use super::events::{BudgetRemaining, LoopEvent};
use super::ledger::LedgerBackend;
use super::progress::NoProgressDetector;
use super::state::{GoalStatus, LoopState};
use super::verifier::{Verifier, VerifyContext, VerifyResult};

/// Factory that creates a fresh `AgentLoop` for each session.
///
/// Each call returns a new `AgentLoop` with a new context window but
/// sharing the same `Provider`, `ToolRegistry`, and `Executor`.
///
/// Stores `ContextConfig` (not `ContextWindowManager`) because the manager
/// holds a `Box<dyn CompactionStrategy>` which is not `Clone`. Each session
/// gets a fresh `ContextWindowManager` constructed from the config — this is
/// the correct behavior anyway, since compaction state should not leak
/// between sessions.
pub struct AgentFactory {
    provider: Arc<dyn lim_llm::Provider>,
    registry: Arc<ToolRegistry>,
    config: AgentConfig,
    context_config: Option<ContextConfig>,
    /// Optional hooks (e.g. approval, no-progress) injected into every
    /// session created by this factory.
    hooks: Option<Arc<dyn AgentHooks>>,
    #[allow(dead_code)]
    sessions_dir: Utf8PathBuf,
}

impl AgentFactory {
    /// Create a factory that will produce AgentLoops with the given
    /// provider, tools, and config.
    pub fn new(
        provider: Arc<dyn lim_llm::Provider>,
        registry: Arc<ToolRegistry>,
        config: AgentConfig,
        context_config: Option<ContextConfig>,
        sessions_dir: Utf8PathBuf,
    ) -> Self {
        Self { provider, registry, config, context_config, hooks: None, sessions_dir }
    }

    /// Attach hooks (e.g. approval, NoProgress at turn level) to every
    /// session produced by this factory.
    pub fn with_hooks(mut self, hooks: Arc<dyn AgentHooks>) -> Self {
        self.hooks = Some(hooks);
        self
    }

    /// Build a factory for a sub-agent role, overriding the base config
    /// with role-specific fields from `spec`.
    ///
    /// `base_provider` is the provider resolved by the caller (the CLI
    /// layer handles the string→Provider translation from `spec.provider`).
    /// `base_registry` is the full tool registry; if `spec.tools_whitelist`
    /// is set, a filtered clone is created so the role can only access
    /// those tools (e.g. a reviewer restricted to read-only tools).
    ///
    /// This is the preferred constructor for sub-agents loaded from
    /// `.lim/agents/<role>.toml` (via `AgentRoleConfig` → `AgentRoleSpec`
    /// conversion at the CLI layer).
    pub fn for_role(
        spec: AgentRoleSpec,
        base_provider: Arc<dyn lim_llm::Provider>,
        base_registry: Arc<ToolRegistry>,
        context_config: Option<ContextConfig>,
        sessions_dir: Utf8PathBuf,
    ) -> Self {
        let registry = match &spec.tools_whitelist {
            Some(wl) if !wl.is_empty() => base_registry.filtered_clone(wl),
            _ => base_registry,
        };
        let config = AgentConfig {
            max_turns: spec.max_turns.unwrap_or(20),
            system_prompt: spec.system_prompt,
            model: spec.model.unwrap_or_default(),
        };
        Self::new(base_provider, registry, config, context_config, sessions_dir)
    }

    /// Create a fresh AgentLoop for a new session, with a new broadcast
    /// channel and cancellation token. The caller is responsible for
    /// subscribing to the channel (e.g. to write session JSONL).
    pub fn create_session(&self) -> (AgentLoop, broadcast::Receiver<EventEnvelope>) {
        let (event_tx, event_rx) = broadcast::channel(256);
        let cancel = CancellationToken::new();
        let mut agent = AgentLoop::new(
            Arc::clone(&self.provider),
            Arc::clone(&self.registry),
            event_tx,
            cancel,
        )
        .with_config(self.config.clone());

        if let Some(ref config) = self.context_config {
            agent = agent.with_context_window(ContextWindowManager::new(config.clone()));
        }

        if let Some(ref hooks) = self.hooks {
            agent = agent.with_hooks(Arc::clone(hooks));
        }

        (agent, event_rx)
    }
}

/// Final result of a Goal Loop run.
#[derive(Debug, Clone)]
pub enum GoalResult {
    /// Goal achieved — the verifier reported success.
    Success,
    /// Budget exhausted before the goal was achieved.
    BudgetExhausted,
    /// The goal needs human intervention (e.g. stuck, verifier uncertain).
    NeedsHuman(String),
    /// The goal failed and no more attempts are possible.
    Failed(String),
    /// The goal was cancelled by the user.
    Cancelled,
}

/// The Goal Loop executor.
///
/// Runs multiple `AgentLoop` sessions in sequence, persisting state between
/// them via a [`LedgerBackend`] and verifying goal completion via `Verifier`.
///
/// The ledger is abstracted via `Arc<dyn LedgerBackend>` so the same runner
/// works with workspace-local storage (`StateLedger`) or daemon-managed
/// independent storage (`DaemonLedger`).
pub struct LoopRunner {
    agent_factory: AgentFactory,
    ledger: Arc<dyn LedgerBackend>,
    verifier: Box<dyn Verifier>,
    budget: Budget,
    /// No-progress detector — tracks consecutive identical verifier failures
    /// across sessions and escalates to NeedsHuman when the threshold is hit.
    no_progress: NoProgressDetector,
    /// Optional backoff strategy — when set, the runner waits `next_delay()`
    /// between failed sessions. When the strategy is exhausted (`None`),
    /// the runner escalates to `NeedsHuman`.
    ///
    /// Wrapped in `Mutex` for interior mutability (`run_goal_loop` takes
    /// `&self`). The lock is held only for the synchronous `next_delay()`
    /// call, never across `.await` points.
    backoff: Option<Mutex<Box<dyn BackoffStrategy>>>,
    /// Optional broadcast sender for `LoopEvent`s (Goal Loop observability).
    /// Subscribers receive events for goal start/end, session boundaries,
    /// verification results, budget checks, and no-progress escalation.
    event_tx: Option<broadcast::Sender<LoopEvent>>,
    /// Base loop directory (e.g. `.lim/loop/`). When set, LoopEvents are
    /// persisted to `<loop_dir>/<goal_id>/events.jsonl` and the verifier
    /// receives `loop_dir` via `VerifyContext` (so `ReviewerVerifier` can
    /// write `review-<N>.jsonl`).
    loop_dir: Option<Utf8PathBuf>,
}

impl LoopRunner {
    /// Create a new LoopRunner with the default no-progress threshold (3).
    ///
    /// Accepts any `LedgerBackend` implementation (`StateLedger`,
    /// `DaemonLedger`, or a custom backend).
    pub fn new(
        agent_factory: AgentFactory,
        ledger: impl LedgerBackend + 'static,
        verifier: Box<dyn Verifier>,
        budget: Budget,
    ) -> Self {
        Self {
            agent_factory,
            ledger: Arc::new(ledger),
            verifier,
            budget,
            no_progress: NoProgressDetector::new(),
            backoff: None,
            event_tx: None,
            loop_dir: None,
        }
    }

    /// Create a LoopRunner with a custom no-progress threshold.
    pub fn with_no_progress_threshold(
        agent_factory: AgentFactory,
        ledger: impl LedgerBackend + 'static,
        verifier: Box<dyn Verifier>,
        budget: Budget,
        threshold: usize,
    ) -> Self {
        Self {
            agent_factory,
            ledger: Arc::new(ledger),
            verifier,
            budget,
            no_progress: NoProgressDetector::with_threshold(threshold),
            backoff: None,
            event_tx: None,
            loop_dir: None,
        }
    }

    /// Attach a broadcast sender to receive `LoopEvent`s for observability.
    ///
    /// When set, the runner emits events at: goal start/resume, session
    /// start/end, verification done, budget checked, no-progress escalation,
    /// and goal finished. Subscribers can log them, write to a separate
    /// `.lim/loop/<goal_id>/events.jsonl`, or feed into a TUI dashboard.
    pub fn with_event_sender(mut self, event_tx: broadcast::Sender<LoopEvent>) -> Self {
        self.event_tx = Some(event_tx);
        self
    }

    /// Attach a backoff strategy for cost control.
    ///
    /// When set, the runner applies `next_delay()` after each failed
    /// session (verifier returns `Fail` but below no-progress threshold).
    /// If the strategy returns `None` (exhausted), the runner escalates
    /// to `NeedsHuman` instead of retrying.
    ///
    /// Without a backoff strategy, the runner retries immediately after
    /// failures (subject to budget and no-progress limits).
    pub fn with_backoff(mut self, strategy: Box<dyn BackoffStrategy>) -> Self {
        self.backoff = Some(Mutex::new(strategy));
        self
    }

    /// Configure the base loop directory (e.g. `.lim/loop/`).
    ///
    /// When set:
    /// - LoopEvents are persisted to `<loop_dir>/<goal_id>/events.jsonl`
    ///   (a background persister task is spawned per goal run).
    /// - The verifier receives `loop_dir` via `VerifyContext`, enabling
    ///   `ReviewerVerifier` to write `review-<N>.jsonl`.
    /// - An internal `event_tx` is auto-created if not already set (so the
    ///   persister has a channel to subscribe to).
    pub fn with_loop_dir(mut self, loop_dir: Utf8PathBuf) -> Self {
        self.loop_dir = Some(loop_dir);
        if self.event_tx.is_none() {
            let (tx, _rx) = broadcast::channel(64);
            self.event_tx = Some(tx);
        }
        self
    }

    /// Access the underlying ledger (e.g. to read final state after
    /// `start_goal` / `resume_goal` returns).
    pub fn ledger(&self) -> &dyn LedgerBackend {
        self.ledger.as_ref()
    }

    /// Emit a `LoopEvent` if an event sender is attached. Errors (e.g. no
    /// subscribers) are silently ignored — observability must never block the
    /// loop. The `tracing` macro always logs regardless.
    fn emit(&self, event: LoopEvent) {
        if let Some(tx) = &self.event_tx {
            // send fails if there are no active receivers; that's fine.
            let _ = tx.send(event.clone());
        }
    }

    /// Apply backoff before the next session.
    ///
    /// If no backoff strategy is set, returns `None` immediately (retry now).
    /// If the strategy returns `Some(delay)`, sleeps for that duration and
    /// returns `None` (retry after delay).
    /// If the strategy returns `None` (exhausted), returns `Some(reason)`
    /// so the caller can escalate to `NeedsHuman`.
    ///
    /// The Mutex lock is held only for the synchronous `next_delay()` call;
    /// `tokio::time::sleep` is called after the lock is dropped.
    async fn apply_backoff(&self) -> Option<String> {
        let backoff = match &self.backoff {
            Some(b) => b,
            None => return None, // No backoff configured — retry immediately.
        };

        let delay = {
            let mut guard = backoff.lock().expect("backoff mutex not poisoned");
            guard.next_delay()
        };

        match delay {
            Some(d) => {
                debug!(delay = ?d, "backoff: sleeping before next session");
                tokio::time::sleep(d).await;
                None
            }
            None => {
                let reason = "backoff strategy exhausted (max attempts reached)".to_string();
                Some(reason)
            }
        }
    }

    /// Reset the backoff strategy to initial state (e.g. after a success).
    fn reset_backoff(&self) {
        if let Some(b) = &self.backoff {
            let mut guard = b.lock().expect("backoff mutex not poisoned");
            guard.reset();
        }
    }

    /// Spawn a background task to persist `LoopEvent`s to
    /// `<loop_dir>/<goal_id>/events.jsonl`.
    ///
    /// Returns `None` when `loop_dir` or `event_tx` is not set (persistence
    /// skipped). The returned `LoopEventPersister` must be `finish()`ed after
    /// the goal loop completes to flush remaining events.
    fn spawn_loop_event_persister(&self, goal_id: &GoalId) -> Option<LoopEventPersister> {
        let loop_dir = self.loop_dir.as_deref()?;
        let tx = self.event_tx.as_ref()?;
        let path: Utf8PathBuf = loop_dir.join(goal_id.to_string()).join("events.jsonl");
        let rx = tx.subscribe();
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move {
            Self::run_loop_event_persister(rx, &path, cancel_clone).await;
        });
        Some(LoopEventPersister { handle, cancel })
    }

    /// Drain `rx` and write each `LoopEvent` as a JSONL line to `path`.
    /// Stops when `cancel` is cancelled, then drains remaining buffered events.
    async fn run_loop_event_persister(
        mut rx: broadcast::Receiver<LoopEvent>,
        path: &Utf8Path,
        cancel: CancellationToken,
    ) {
        if let Some(parent) = path.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            warn!(error = %e, path = %path, "loop event persister: failed to create dir");
            return;
        }
        let mut file =
            match tokio::fs::OpenOptions::new().create(true).append(true).open(path).await {
                Ok(f) => f,
                Err(e) => {
                    warn!(error = %e, path = %path, "loop event persister: failed to open file");
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
                                warn!(error = %e, "loop event persister: write failed");
                            }
                            if let Err(e) = file.write_all(b"\n").await {
                                warn!(error = %e, "loop event persister: newline failed");
                            }
                        }
                    }
                    Err(RecvError::Closed) => break,
                    Err(RecvError::Lagged(n)) => {
                        warn!(lagged = n, "loop event persister: lagged, dropped events");
                    }
                }
            }
        }
        // Drain remaining buffered events after cancellation.
        while let Ok(event) = rx.try_recv() {
            if let Ok(line) = serde_json::to_string(&event) {
                let _ = file.write_all(line.as_bytes()).await;
                let _ = file.write_all(b"\n").await;
            }
        }
        let _ = file.flush().await;
        debug!(path = %path, "loop event persister: flushed and closed");
    }

    /// Start a new goal. Creates the state directory and runs the Goal Loop
    /// until the goal is achieved, the budget is exhausted, or an error occurs.
    pub async fn start_goal(&self, goal: &str) -> Result<(GoalId, GoalResult)> {
        let goal_id = self.ledger.create_goal(goal).await?;
        info!(goal_id = %goal_id, goal = goal, "starting new goal");
        let persister = self.spawn_loop_event_persister(&goal_id);
        self.emit(LoopEvent::GoalStarted { goal_id, goal: goal.to_string() });
        let result = self.run_goal_loop(&goal_id).await?;
        if let Some(p) = persister {
            p.finish().await;
        }
        Ok((goal_id, result))
    }

    /// Resume an existing goal that was interrupted or paused.
    ///
    /// Resets the no-progress detector — a human-initiated resume is treated
    /// as an intervention that should give the loop a fresh chance.
    pub async fn resume_goal(&self, goal_id: &GoalId) -> Result<GoalResult> {
        let state = self.ledger.load(goal_id).await?;
        info!(goal_id = %goal_id, goal = %state.goal, sessions_done = state.session_count, "resuming goal");
        let persister = self.spawn_loop_event_persister(goal_id);
        self.emit(LoopEvent::GoalResumed {
            goal_id: *goal_id,
            prior_sessions: state.session_count,
        });
        self.no_progress.reset();
        self.reset_backoff();
        let result = self.run_goal_loop(goal_id).await?;
        if let Some(p) = persister {
            p.finish().await;
        }
        Ok(result)
    }

    /// The core Goal Loop: run sessions until done.
    async fn run_goal_loop(&self, goal_id: &GoalId) -> Result<GoalResult> {
        loop {
            let mut state = self.ledger.load(goal_id).await?;

            // 1. Budget check
            if let Err(reason) = self.budget.check(state.session_count, state.total_tokens) {
                warn!(goal_id = %goal_id, reason = reason, "budget exhausted");
                state.status = GoalStatus::Failed(format!("budget exhausted: {reason}"));
                self.ledger.save(goal_id, &state).await?;
                self.emit(LoopEvent::GoalFinished {
                    goal_id: *goal_id,
                    final_status: state.status.clone(),
                });
                return Ok(GoalResult::BudgetExhausted);
            }
            self.emit(LoopEvent::BudgetChecked {
                goal_id: *goal_id,
                sessions_used: state.session_count,
                tokens_used: state.total_tokens,
                remaining: BudgetRemaining::from_budget(
                    &self.budget,
                    state.session_count,
                    state.total_tokens,
                ),
            });

            // 2. Construct session prompt from state
            let session_prompt = self.build_session_prompt(goal_id, &state);
            let session_number = state.session_count + 1;
            debug!(goal_id = %goal_id, session = session_number, "starting session");
            self.emit(LoopEvent::SessionStarted { goal_id: *goal_id, session_number });

            // 3. Create a fresh AgentLoop and run one session
            let (mut agent, _event_rx) = self.agent_factory.create_session();
            let user_msg = AgentMessage::user(session_prompt);

            match agent.run(user_msg).await {
                Ok(_messages) => {
                    info!(goal_id = %goal_id, session = session_number, "session completed");
                }
                Err(e) => {
                    error!(goal_id = %goal_id, error = %e, "session failed");
                    state.status = GoalStatus::Failed(format!("session error: {e}"));
                    self.ledger.save(goal_id, &state).await?;
                    self.emit(LoopEvent::Error {
                        goal_id: *goal_id,
                        message: format!("session error: {e}"),
                    });
                    self.emit(LoopEvent::GoalFinished {
                        goal_id: *goal_id,
                        final_status: state.status.clone(),
                    });
                    return Ok(GoalResult::Failed(format!("session error: {e}")));
                }
            }

            // 4. Update state
            state.session_count += 1;
            state.last_session_id = Some(agent.session_id());
            state.updated_at = unix_now();
            self.ledger.save(goal_id, &state).await?;
            self.emit(LoopEvent::SessionEnded {
                goal_id: *goal_id,
                session_number,
                turn_count: state.turn_count,
                tokens_used: state.total_tokens,
            });

            // 5. Verify goal
            debug!(goal_id = %goal_id, "verifying goal");
            let ctx = VerifyContext { goal_id, loop_dir: self.loop_dir.as_deref() };
            let verify_result = self.verifier.verify(&state, &ctx).await?;
            self.emit(LoopEvent::VerificationDone {
                goal_id: *goal_id,
                result: verify_result.clone(),
                verifier_kind: Some(self.verifier.kind().to_string()),
                verifier_session_id: None,
                verifier_tokens_used: None,
            });
            match verify_result {
                VerifyResult::Pass => {
                    info!(goal_id = %goal_id, "goal achieved!");
                    state.status = GoalStatus::Success;
                    self.ledger.save(goal_id, &state).await?;
                    self.no_progress.reset();
                    self.reset_backoff();
                    self.emit(LoopEvent::GoalFinished {
                        goal_id: *goal_id,
                        final_status: state.status.clone(),
                    });
                    return Ok(GoalResult::Success);
                }
                VerifyResult::Fail(ref reason) => {
                    warn!(goal_id = %goal_id, reason = %reason, "verification failed, continuing");
                    // 6. No-progress detection — escalate if stuck
                    if self.no_progress.record(&verify_result) {
                        let msg = format!(
                            "no progress after {} consecutive identical failures (last reason: {reason})",
                            self.no_progress.consecutive_count(),
                        );
                        warn!(goal_id = %goal_id, msg = %msg, "escalating to NeedsHuman");
                        state.status = GoalStatus::NeedsHuman(msg.clone());
                        self.ledger.save(goal_id, &state).await?;
                        self.emit(LoopEvent::NoProgressDetected {
                            goal_id: *goal_id,
                            consecutive_failures: self.no_progress.consecutive_count(),
                            last_reason: reason.clone(),
                        });
                        self.emit(LoopEvent::GoalFinished {
                            goal_id: *goal_id,
                            final_status: state.status.clone(),
                        });
                        return Ok(GoalResult::NeedsHuman(msg));
                    }

                    // 7. Backoff — wait before next session if a strategy is set.
                    if let Some(reason) = self.apply_backoff().await {
                        // Backoff exhausted — escalate to NeedsHuman.
                        warn!(goal_id = %goal_id, reason = %reason, "backoff exhausted, escalating to NeedsHuman");
                        state.status = GoalStatus::NeedsHuman(reason.clone());
                        self.ledger.save(goal_id, &state).await?;
                        self.emit(LoopEvent::GoalFinished {
                            goal_id: *goal_id,
                            final_status: state.status.clone(),
                        });
                        return Ok(GoalResult::NeedsHuman(reason));
                    }
                    // Continue to next session
                }
                VerifyResult::NeedsHuman => {
                    warn!(goal_id = %goal_id, "verifier needs human");
                    state.status = GoalStatus::NeedsHuman("verifier needs human".into());
                    self.ledger.save(goal_id, &state).await?;
                    self.no_progress.reset();
                    self.emit(LoopEvent::GoalFinished {
                        goal_id: *goal_id,
                        final_status: state.status.clone(),
                    });
                    return Ok(GoalResult::NeedsHuman("verifier needs human".into()));
                }
            }
        }
    }

    /// Build the session prompt from the current state.
    ///
    /// The prompt tells the agent: what the goal is, what's been done so
    /// far, and what to do next. This is the key mechanism for cross-session
    /// continuity — each session starts with this context.
    fn build_session_prompt(&self, goal_id: &GoalId, state: &LoopState) -> String {
        let mut prompt = format!("# Goal\n\n{}\n\n", state.goal);

        if state.session_count > 0 {
            prompt.push_str(&format!(
                "# Progress\n\nYou have completed {} session(s) so far, using approximately {} tokens.\n\n",
                state.session_count, state.total_tokens,
            ));
        }

        if !state.progress.is_empty() {
            prompt.push_str(&format!("# Previous Work Notes\n\n{}\n\n", state.progress));
        }

        prompt.push_str(&format!(
            "# Instructions\n\n\
             Continue working toward the goal above. Use the available tools to make progress. \
             When you finish a meaningful chunk of work, write a summary of what you did and \
             what's left to do to `.lim/loop/{goal_id}/progress.md` so the next session can \
             pick up where you left off.\n\n\
             Stop when the goal is achieved.",
        ));

        prompt
    }
}

/// Handle to a spawned loop-event persister task.
///
/// Created by `LoopRunner::spawn_loop_event_persister`. Call `finish()`
/// after the goal loop completes to cancel the task and wait for it to
/// flush remaining events to disk.
pub struct LoopEventPersister {
    handle: tokio::task::JoinHandle<()>,
    cancel: CancellationToken,
}

impl LoopEventPersister {
    /// Cancel the persister and wait for it to flush remaining events.
    pub async fn finish(self) {
        self.cancel.cancel();
        let _ = self.handle.await;
    }
}

/// Current Unix time in seconds.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_runner::state::StateLedger;
    use crate::loop_runner::verifier::AlwaysPassVerifier;
    use camino::Utf8PathBuf;
    use lim_core::message::Role;
    use lim_llm::mock::{MockProvider, MockResponse};

    #[tokio::test]
    async fn start_goal_succeeds_with_passing_verifier() {
        let dir = std::env::temp_dir().join("lim-test-loop-runner-success");
        let _ = std::fs::remove_dir_all(&dir);

        let provider: Arc<dyn lim_llm::Provider> =
            Arc::new(MockProvider::new(vec![MockResponse::Text("Done!".to_string())]));
        let registry = Arc::new(ToolRegistry::new());

        let factory = AgentFactory::new(
            provider,
            registry,
            AgentConfig::default(),
            None,
            Utf8PathBuf::try_from(dir.join("sessions")).unwrap(),
        );
        let ledger = StateLedger::new(Utf8PathBuf::try_from(dir.clone()).unwrap());

        let runner =
            LoopRunner::new(factory, ledger, Box::new(AlwaysPassVerifier), Budget::unlimited());

        let (goal_id, result) = runner.start_goal("test goal").await.unwrap();
        assert!(matches!(result, GoalResult::Success));

        // Verify state was persisted
        let state = runner.ledger.load(&goal_id).await.unwrap();
        assert_eq!(state.status, GoalStatus::Success);
        assert_eq!(state.session_count, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn budget_exhaustion_stops_loop() {
        let dir = std::env::temp_dir().join("lim-test-loop-runner-budget");
        let _ = std::fs::remove_dir_all(&dir);

        // Provide 2 mock responses so the loop can run 2 sessions if budget allows
        let provider: Arc<dyn lim_llm::Provider> = Arc::new(MockProvider::new(vec![
            MockResponse::Text("session 1".to_string()),
            MockResponse::Text("session 2".to_string()),
        ]));
        let registry = Arc::new(ToolRegistry::new());

        let factory = AgentFactory::new(
            provider,
            registry,
            AgentConfig::default(),
            None,
            Utf8PathBuf::try_from(dir.join("sessions")).unwrap(),
        );
        let ledger = StateLedger::new(Utf8PathBuf::try_from(dir.clone()).unwrap());

        // Budget: max 1 session. AlwaysPassVerifier means we'd succeed on
        // the first session, so we need a Fail verifier to force the loop
        // to try a second session and hit the budget.
        struct AlwaysFailVerifier;
        #[async_trait::async_trait]
        impl Verifier for AlwaysFailVerifier {
            async fn verify(
                &self,
                _state: &LoopState,
                _ctx: &VerifyContext<'_>,
            ) -> Result<VerifyResult> {
                Ok(VerifyResult::Fail("not done".into()))
            }
        }

        let runner = LoopRunner::new(
            factory,
            ledger,
            Box::new(AlwaysFailVerifier),
            Budget::with_max_sessions(1),
        );

        let (_goal_id, result) = runner.start_goal("test goal").await.unwrap();
        assert!(matches!(result, GoalResult::BudgetExhausted));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn resume_goal_continues_from_saved_state() {
        let dir = std::env::temp_dir().join("lim-test-loop-runner-resume");
        let _ = std::fs::remove_dir_all(&dir);

        let provider: Arc<dyn lim_llm::Provider> =
            Arc::new(MockProvider::new(vec![MockResponse::Text("resumed work".to_string())]));
        let registry = Arc::new(ToolRegistry::new());

        let factory = AgentFactory::new(
            provider,
            registry,
            AgentConfig::default(),
            None,
            Utf8PathBuf::try_from(dir.join("sessions")).unwrap(),
        );
        let ledger = StateLedger::new(Utf8PathBuf::try_from(dir.clone()).unwrap());

        // Manually create a goal with 2 prior sessions
        let goal_id = ledger.create_goal("resume test").await.unwrap();
        let mut state = ledger.load(&goal_id).await.unwrap();
        state.session_count = 2;
        state.total_tokens = 5000;
        ledger.save(&goal_id, &state).await.unwrap();

        let runner =
            LoopRunner::new(factory, ledger, Box::new(AlwaysPassVerifier), Budget::unlimited());

        let result = runner.resume_goal(&goal_id).await.unwrap();
        assert!(matches!(result, GoalResult::Success));

        // Session count should be 3 (2 prior + 1 new)
        let state = runner.ledger.load(&goal_id).await.unwrap();
        assert_eq!(state.session_count, 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_session_prompt_includes_goal() {
        let dir = std::env::temp_dir().join("lim-test-loop-runner-prompt");
        let provider: Arc<dyn lim_llm::Provider> = Arc::new(MockProvider::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let factory = AgentFactory::new(
            provider,
            registry,
            AgentConfig::default(),
            None,
            Utf8PathBuf::try_from(dir.join("sessions")).unwrap(),
        );
        let ledger = StateLedger::new(Utf8PathBuf::try_from(dir.clone()).unwrap());
        let runner =
            LoopRunner::new(factory, ledger, Box::new(AlwaysPassVerifier), Budget::unlimited());

        let goal_id = GoalId::new();
        let state = LoopState::new("fix all the bugs");
        let prompt = runner.build_session_prompt(&goal_id, &state);
        assert!(prompt.contains("fix all the bugs"));
        assert!(prompt.contains("# Goal"));
        // The prompt must include the actual goal_id so the agent knows where
        // to write its progress file.
        assert!(prompt.contains(&goal_id.to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Verify that `AgentFactory::create_session` produces a working
    /// AgentLoop with a fresh broadcast channel.
    #[tokio::test]
    async fn agent_factory_creates_working_session() {
        let provider: Arc<dyn lim_llm::Provider> =
            Arc::new(MockProvider::new(vec![MockResponse::Text("hello".to_string())]));
        let registry = Arc::new(ToolRegistry::new());

        let factory = AgentFactory::new(
            provider,
            registry,
            AgentConfig::default(),
            None,
            Utf8PathBuf::from("/tmp/lim-test-sessions"),
        );

        let (mut agent, _rx) = factory.create_session();

        let messages = agent.run(AgentMessage::user("hi")).await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[1].role, Role::Assistant);
    }

    /// Verify that `with_event_sender` emits `LoopEvent`s for the major
    /// lifecycle points: goal start, session start/end, verification,
    /// goal finished.
    #[tokio::test]
    async fn event_sender_emits_lifecycle_events() {
        let dir = std::env::temp_dir().join("lim-test-loop-runner-events");
        let _ = std::fs::remove_dir_all(&dir);

        let provider: Arc<dyn lim_llm::Provider> =
            Arc::new(MockProvider::new(vec![MockResponse::Text("done".to_string())]));
        let registry = Arc::new(ToolRegistry::new());
        let factory = AgentFactory::new(
            provider,
            registry,
            AgentConfig::default(),
            None,
            Utf8PathBuf::try_from(dir.join("sessions")).unwrap(),
        );
        let ledger = StateLedger::new(Utf8PathBuf::try_from(dir.clone()).unwrap());

        let (event_tx, mut event_rx) = broadcast::channel::<LoopEvent>(64);
        let runner =
            LoopRunner::new(factory, ledger, Box::new(AlwaysPassVerifier), Budget::unlimited())
                .with_event_sender(event_tx);

        let (_goal_id, result) = runner.start_goal("eventful goal").await.unwrap();
        assert!(matches!(result, GoalResult::Success));

        // Collect all events
        let mut events = Vec::new();
        while let Ok(ev) = event_rx.try_recv() {
            events.push(ev);
        }

        // We expect at minimum: GoalStarted, BudgetChecked, SessionStarted,
        // SessionEnded, VerificationDone, GoalFinished.
        assert!(
            events.iter().any(|e| matches!(e, LoopEvent::GoalStarted { .. })),
            "missing GoalStarted: {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(e, LoopEvent::SessionStarted { .. })),
            "missing SessionStarted: {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(e, LoopEvent::SessionEnded { .. })),
            "missing SessionEnded: {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(e, LoopEvent::VerificationDone { .. })),
            "missing VerificationDone: {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(e, LoopEvent::GoalFinished { .. })),
            "missing GoalFinished: {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(e, LoopEvent::BudgetChecked { .. })),
            "missing BudgetChecked: {events:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Verify that backoff exhaustion escalates to NeedsHuman.
    ///
    /// With `FixedBackoff::limited(1ms, 2)` and no_progress threshold=10:
    /// - Session 1 fails → backoff call 1 → sleep 1ms
    /// - Session 2 fails → backoff call 2 → sleep 1ms
    /// - Session 3 fails → backoff call 3 → None (exhausted) → NeedsHuman
    #[tokio::test]
    async fn backoff_exhaustion_escalates_to_needs_human() {
        let dir = std::env::temp_dir().join("lim-test-loop-runner-backoff");
        let _ = std::fs::remove_dir_all(&dir);

        // 3 mock responses for 3 sessions before backoff exhausts.
        let provider: Arc<dyn lim_llm::Provider> = Arc::new(MockProvider::new(vec![
            MockResponse::Text("s1".to_string()),
            MockResponse::Text("s2".to_string()),
            MockResponse::Text("s3".to_string()),
        ]));
        let registry = Arc::new(ToolRegistry::new());
        let factory = AgentFactory::new(
            provider,
            registry,
            AgentConfig::default(),
            None,
            Utf8PathBuf::try_from(dir.join("sessions")).unwrap(),
        );
        let ledger = StateLedger::new(Utf8PathBuf::try_from(dir.clone()).unwrap());

        struct AlwaysFailVerifier;
        #[async_trait::async_trait]
        impl Verifier for AlwaysFailVerifier {
            async fn verify(
                &self,
                _state: &LoopState,
                _ctx: &VerifyContext<'_>,
            ) -> Result<VerifyResult> {
                Ok(VerifyResult::Fail("not done".into()))
            }
        }

        let runner = LoopRunner::with_no_progress_threshold(
            factory,
            ledger,
            Box::new(AlwaysFailVerifier),
            Budget::with_max_sessions(10),
            10, // high threshold so backoff exhausts first
        )
        .with_backoff(Box::new(super::super::backoff::FixedBackoff::limited(
            std::time::Duration::from_millis(1),
            2,
        )));

        let (_goal_id, result) = runner.start_goal("backoff test").await.unwrap();
        assert!(
            matches!(result, GoalResult::NeedsHuman(ref msg) if msg.contains("backoff")),
            "expected NeedsHuman with backoff message, got {result:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
