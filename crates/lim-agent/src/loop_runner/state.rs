//! Cross-session state persistence for Loop Engineering.
//!
//! `StateLedger` stores the "working memory" of a Goal Loop in
//! `.lim/loop/<goal_id>/` — separate from the immutable session JSONL
//! event log. The agent reads this state at the start of each session to
//! resume work, and writes it back at the end.
//!
//! Layout:
//! ```text
//! .lim/loop/<goal_id>/
//! ├── goal.md         — goal description + verification criteria
//! ├── progress.md     — agent's free-form work notes (markdown)
//! ├── state.json      — LoopRunner metadata (counts, status, timestamps)
//! ```

use std::time::SystemTime;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::{debug, warn};

use lim_core::error::{LimError, Result};
use lim_core::id::GoalId;
use lim_core::id::SessionId;
use lim_core::status::Condition;

/// Persistent working memory for a single Goal Loop.
///
/// Unlike session JSONL (which is an immutable event log), `LoopState` is
/// mutable: the LoopRunner updates it after every session and the agent
/// itself may read/write the `progress.md` file during a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopState {
    /// The original goal description, as provided by the user.
    pub goal: String,
    /// Agent's free-form work notes (mirrored to `progress.md`).
    ///
    /// Empty until the agent writes something. The LoopRunner does NOT
    /// auto-populate this — the agent is expected to use a `write_file`
    /// tool to update `progress.md` during its session.
    pub progress: String,
    /// How many sessions have run so far for this goal.
    pub session_count: usize,
    /// Cumulative turn count across all sessions.
    pub turn_count: usize,
    /// Cumulative token usage across all sessions.
    pub total_tokens: u64,
    /// The last session's ID (for cross-referencing with session JSONL).
    pub last_session_id: Option<SessionId>,
    /// Current goal status.
    pub status: GoalStatus,
    /// Fine-grained conditions (like k8s Status.conditions).
    ///
    /// Allows expressing composite state, e.g. "Running but BudgetExceeded=True
    /// and NoProgress=True". `#[serde(default)]` keeps backward compat with
    /// old state.json files written before conditions existed.
    #[serde(default)]
    pub conditions: Vec<Condition>,
    /// When the goal was created (Unix seconds).
    pub created_at: u64,
    /// When the state was last updated (Unix seconds).
    pub updated_at: u64,
}

impl LoopState {
    /// Create a fresh state for a new goal.
    pub fn new(goal: impl Into<String>) -> Self {
        let now = unix_now();
        Self {
            goal: goal.into(),
            progress: String::new(),
            session_count: 0,
            turn_count: 0,
            total_tokens: 0,
            last_session_id: None,
            status: GoalStatus::Running,
            conditions: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Set or update a condition by type name.
    ///
    /// If a condition with the same type exists, it is replaced; otherwise
    /// a new one is appended. `last_transition_time` is only updated when
    /// the status actually changes (matching k8s semantics).
    pub fn set_condition(&mut self, new_condition: Condition) {
        let now = unix_now();
        if let Some(existing) =
            self.conditions.iter_mut().find(|c| c.r#type == new_condition.r#type)
        {
            if existing.status != new_condition.status {
                existing.last_transition_time = now;
            }
            existing.status = new_condition.status;
            existing.reason = new_condition.reason;
            existing.message = new_condition.message;
        } else {
            self.conditions.push(new_condition);
        }
        self.updated_at = now;
    }

    /// Find a condition by type name.
    pub fn condition(&self, type_name: &str) -> Option<&Condition> {
        self.conditions.iter().find(|c| c.r#type == type_name)
    }
}

/// Lifecycle status of a goal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    /// Goal Loop is actively running sessions.
    Running,
    /// Paused by the user (resume with `lim loop --resume`).
    Paused,
    /// Verifier reported success.
    Success,
    /// Verifier failed and no more retries available.
    Failed(String),
    /// Stuck — needs human intervention.
    NeedsHuman(String),
}

/// File-based state persistence for Goal Loops.
///
/// Each goal gets its own directory under `.lim/loop/<goal_id>/`. The
/// `state.json` file holds the structured `LoopState`, while `goal.md`
/// and `progress.md` are human/agent-readable markdown files.
///
/// `StateLedger` is `Clone` so that loop-control tools can share an instance
/// with the `LoopRunner` (the runner moves one clone into its `Arc<dyn
/// LedgerBackend>`, the tools hold another).
#[derive(Clone)]
pub struct StateLedger {
    project_root: Utf8PathBuf,
}

impl StateLedger {
    /// Create a ledger rooted at the given project workspace.
    pub fn new(project_root: impl Into<Utf8PathBuf>) -> Self {
        Self { project_root: project_root.into() }
    }

    /// Directory for a specific goal: `<project_root>/.lim/loop/<goal_id>/`.
    fn goal_dir(&self, goal_id: &GoalId) -> Utf8PathBuf {
        self.project_root.join(".lim").join("loop").join(goal_id.to_string())
    }

    /// Path to the `state.json` file for a goal.
    fn state_path(&self, goal_id: &GoalId) -> Utf8PathBuf {
        self.goal_dir(goal_id).join("state.json")
    }

    /// Path to the `goal.md` file for a goal.
    fn goal_md_path(&self, goal_id: &GoalId) -> Utf8PathBuf {
        self.goal_dir(goal_id).join("goal.md")
    }

    /// Ensure the `.lim/loop/` base directory exists.
    async fn ensure_base_dir(&self) -> Result<()> {
        let base = self.project_root.join(".lim").join("loop");
        fs::create_dir_all(base.as_std_path()).await.map_err(|e| {
            LimError::tool_execution("state_ledger", format!("failed to create `{base}`: {e}"), e)
        })?;
        Ok(())
    }

    /// Initialize a new goal: create the directory, write `goal.md` and
    /// an initial `state.json`. Returns the goal ID.
    pub async fn create_goal(&self, goal: &str) -> Result<GoalId> {
        self.ensure_base_dir().await?;
        let goal_id = GoalId::new();
        let dir = self.goal_dir(&goal_id);
        fs::create_dir_all(dir.as_std_path()).await.map_err(|e| {
            LimError::tool_execution("state_ledger", format!("failed to create `{dir}`: {e}"), e)
        })?;

        // Write goal.md
        let goal_md = format!("# Goal\n\n{goal}\n");
        fs::write(self.goal_md_path(&goal_id).as_std_path(), goal_md)
            .await
            .map_err(|e| LimError::tool_execution("state_ledger", "failed to write goal.md", e))?;

        // Write initial state.json
        let state = LoopState::new(goal);
        self.write_state(&goal_id, &state).await?;

        debug!(goal_id = %goal_id, "created new goal");
        Ok(goal_id)
    }

    /// Load the state for a goal. Returns `Err` if the goal doesn't exist.
    pub async fn load(&self, goal_id: &GoalId) -> Result<LoopState> {
        let path = self.state_path(goal_id);
        let content = fs::read_to_string(path.as_std_path()).await.map_err(|e| {
            LimError::tool_execution(
                "state_ledger",
                format!("failed to read state for goal {goal_id}: {e}"),
                e,
            )
        })?;
        let state: LoopState = serde_json::from_str(&content).map_err(|e| {
            LimError::tool_execution(
                "state_ledger",
                format!("failed to parse state for goal {goal_id}: {e}"),
                e,
            )
        })?;
        Ok(state)
    }

    /// Write the state for a goal (overwrites existing).
    pub async fn save(&self, goal_id: &GoalId, state: &LoopState) -> Result<()> {
        self.write_state(goal_id, state).await
    }

    async fn write_state(&self, goal_id: &GoalId, state: &LoopState) -> Result<()> {
        let path = self.state_path(goal_id);
        let json = serde_json::to_string_pretty(state).map_err(|e| {
            LimError::tool_execution("state_ledger", format!("failed to serialize state: {e}"), e)
        })?;
        fs::write(path.as_std_path(), json).await.map_err(|e| {
            LimError::tool_execution(
                "state_ledger",
                format!("failed to write state for goal {goal_id}: {e}"),
                e,
            )
        })?;
        debug!(goal_id = %goal_id, status = ?state.status, "state saved");
        Ok(())
    }

    /// List all goal IDs in the workspace. Returns IDs sorted by `updated_at`
    /// descending (most recent first).
    pub async fn list_goals(&self) -> Result<Vec<(GoalId, LoopState)>> {
        let base = self.project_root.join(".lim").join("loop");
        if !base.exists() {
            return Ok(Vec::new());
        }

        let mut entries = fs::read_dir(base.as_std_path()).await.map_err(|e| {
            LimError::tool_execution("state_ledger", format!("failed to read `{base}`: {e}"), e)
        })?;

        let mut goals = Vec::new();
        while let Some(entry) = entries.next_entry().await.map_err(|e| {
            LimError::tool_execution("state_ledger", format!("failed to read dir entry: {e}"), e)
        })? {
            let name = match entry.file_name().to_str() {
                Some(s) => s.to_string(),
                None => continue,
            };
            let goal_id: GoalId = match name.parse() {
                Ok(id) => id,
                Err(_) => continue, // skip non-goal directories
            };
            match self.load(&goal_id).await {
                Ok(state) => goals.push((goal_id, state)),
                Err(e) => {
                    warn!(goal_id = %goal_id, error = %e, "skipping goal: failed to load state");
                }
            }
        }

        goals.sort_by_key(|(_, s)| std::cmp::Reverse(s.updated_at));
        Ok(goals)
    }

    /// Get the project root.
    pub fn project_root(&self) -> &Utf8Path {
        &self.project_root
    }
}

/// Current Unix time in seconds.
fn unix_now() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_and_load_goal() {
        let dir = std::env::temp_dir().join("lim-test-state-ledger-create");
        let _ = std::fs::remove_dir_all(&dir);
        let ledger = StateLedger::new(Utf8PathBuf::try_from(dir.clone()).unwrap());

        let goal_id = ledger.create_goal("fix all clippy warnings").await.unwrap();
        let state = ledger.load(&goal_id).await.unwrap();

        assert_eq!(state.goal, "fix all clippy warnings");
        assert_eq!(state.session_count, 0);
        assert_eq!(state.status, GoalStatus::Running);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn save_updates_state() {
        let dir = std::env::temp_dir().join("lim-test-state-ledger-save");
        let _ = std::fs::remove_dir_all(&dir);
        let ledger = StateLedger::new(Utf8PathBuf::try_from(dir.clone()).unwrap());

        let goal_id = ledger.create_goal("test goal").await.unwrap();
        let mut state = ledger.load(&goal_id).await.unwrap();
        state.session_count = 3;
        state.total_tokens = 5000;
        state.status = GoalStatus::Success;
        ledger.save(&goal_id, &state).await.unwrap();

        let reloaded = ledger.load(&goal_id).await.unwrap();
        assert_eq!(reloaded.session_count, 3);
        assert_eq!(reloaded.total_tokens, 5000);
        assert_eq!(reloaded.status, GoalStatus::Success);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn list_goals_returns_sorted_by_updated_desc() {
        let dir = std::env::temp_dir().join("lim-test-state-ledger-list");
        let _ = std::fs::remove_dir_all(&dir);
        let ledger = StateLedger::new(Utf8PathBuf::try_from(dir.clone()).unwrap());

        let id1 = ledger.create_goal("goal 1").await.unwrap();
        // Small delay to ensure updated_at differs
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let id2 = ledger.create_goal("goal 2").await.unwrap();

        let goals = ledger.list_goals().await.unwrap();
        assert_eq!(goals.len(), 2);
        // Most recent first
        assert_eq!(goals[0].0, id2);
        assert_eq!(goals[1].0, id1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn list_goals_empty_when_no_dir() {
        let dir = std::env::temp_dir().join("lim-test-state-ledger-empty");
        let _ = std::fs::remove_dir_all(&dir);
        let ledger = StateLedger::new(Utf8PathBuf::try_from(dir.clone()).unwrap());

        let goals = ledger.list_goals().await.unwrap();
        assert!(goals.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loop_state_new_initializes_fields() {
        let state = LoopState::new("test");
        assert_eq!(state.goal, "test");
        assert_eq!(state.session_count, 0);
        assert_eq!(state.turn_count, 0);
        assert_eq!(state.total_tokens, 0);
        assert_eq!(state.status, GoalStatus::Running);
        assert!(state.last_session_id.is_none());
    }
}
