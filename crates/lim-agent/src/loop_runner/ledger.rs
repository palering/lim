//! Ledger backend trait + daemon ledger.
//!
//! `LedgerBackend` abstracts the storage layer for Loop Engineering state.
//! `StateLedger` (workspace-local, `.lim/loop/`) and `DaemonLedger`
//! (independent storage, optionally synced to workspace) both implement it.
//!
//! This abstraction is the key to Loop Engineering's three maturity levels
//! (CLI / TUI / daemon): `LoopRunner` is agnostic to the storage backend,
//! so the same loop code works whether state lives in the workspace or in a
//! daemon-managed independent store.
//!
//! ## Design
//!
//! ```text
//! LedgerBackend (trait)
//! ├── StateLedger    — workspace-local (.lim/loop/), used by CLI + TUI
//! └── DaemonLedger   — independent storage, used by daemon process
//!     └── sync_to_workspace() — mirror state back to a workspace
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::{debug, warn};

use lim_core::error::{LimError, Result};
use lim_core::id::GoalId;

use super::state::LoopState;

/// Storage backend for Loop Engineering state.
///
/// All methods are async because backends may use file I/O (StateLedger) or
/// network/distributed storage (future DaemonLedger variants).
#[async_trait]
pub trait LedgerBackend: Send + Sync {
    /// Create a new goal and return its ID. Writes the initial `state.json`
    /// and `goal.md` to the backing store.
    async fn create_goal(&self, goal: &str) -> Result<GoalId>;

    /// Load the state for a goal. Returns `Err` if the goal doesn't exist.
    async fn load(&self, goal_id: &GoalId) -> Result<LoopState>;

    /// Save (overwrite) the state for a goal.
    async fn save(&self, goal_id: &GoalId, state: &LoopState) -> Result<()>;

    /// List all goals, sorted by `updated_at` descending (most recent first).
    async fn list_goals(&self) -> Result<Vec<(GoalId, LoopState)>>;
}

// ---------------------------------------------------------------------------
// DaemonLedger — independent storage with workspace sync
// ---------------------------------------------------------------------------

/// Independent-storage ledger for daemon-managed goals.
///
/// Unlike `StateLedger` (which writes to `<workspace>/.lim/loop/`),
/// `DaemonLedger` writes to a configurable `storage_root` that is independent
/// of any workspace. This lets a daemon process manage goals across multiple
/// workspaces without polluting any single workspace's `.lim/` directory.
///
/// Use [`sync_to_workspace`] to mirror a goal's state into a workspace's
/// `.lim/loop/` directory so that workspace-local tools (TUI `/loop status`)
/// can see it.
///
/// ## Layout
///
/// ```text
/// <storage_root>/
/// └── <goal_id>/
///     ├── goal.md
///     ├── state.json
///     └── workspace.json   — metadata about which workspace(s) this goal belongs to
/// ```
pub struct DaemonLedger {
    storage_root: Utf8PathBuf,
}

/// Workspace metadata recorded for each daemon-managed goal.
///
/// A goal may be associated with one or more workspaces (e.g. a cross-cutting
/// refactor touching multiple repos). This metadata drives `sync_to_workspace`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceBinding {
    /// Absolute path to the workspace root.
    pub workspace_root: Utf8PathBuf,
    /// When this binding was created (Unix seconds).
    pub bound_at: u64,
    /// Whether this workspace is the primary one (for display purposes).
    pub primary: bool,
}

impl WorkspaceBinding {
    fn new(workspace_root: impl Into<Utf8PathBuf>, primary: bool) -> Self {
        Self { workspace_root: workspace_root.into(), bound_at: unix_now(), primary }
    }
}

impl DaemonLedger {
    /// Create a daemon ledger rooted at `storage_root`.
    ///
    /// The directory is created on first use (lazy); callers don't need to
    /// pre-create it.
    pub fn new(storage_root: impl Into<Utf8PathBuf>) -> Self {
        Self { storage_root: storage_root.into() }
    }

    /// Directory for a specific goal.
    fn goal_dir(&self, goal_id: &GoalId) -> Utf8PathBuf {
        self.storage_root.join(goal_id.to_string())
    }

    fn state_path(&self, goal_id: &GoalId) -> Utf8PathBuf {
        self.goal_dir(goal_id).join("state.json")
    }

    fn goal_md_path(&self, goal_id: &GoalId) -> Utf8PathBuf {
        self.goal_dir(goal_id).join("goal.md")
    }

    fn workspace_binding_path(&self, goal_id: &GoalId) -> Utf8PathBuf {
        self.goal_dir(goal_id).join("workspace.json")
    }

    async fn ensure_storage_root(&self) -> Result<()> {
        fs::create_dir_all(self.storage_root.as_std_path()).await.map_err(|e| {
            LimError::tool_execution(
                "daemon_ledger",
                format!("failed to create storage root `{}`: {e}", self.storage_root),
                e,
            )
        })?;
        Ok(())
    }

    /// Bind a goal to a workspace. Called when a goal is submitted via the
    /// daemon with a `--workspace` argument. The binding is recorded in
    /// `workspace.json` inside the goal's directory.
    pub async fn bind_workspace(
        &self,
        goal_id: &GoalId,
        workspace_root: impl Into<Utf8PathBuf>,
        primary: bool,
    ) -> Result<()> {
        let binding = WorkspaceBinding::new(workspace_root, primary);
        let path = self.workspace_binding_path(goal_id);
        let json = serde_json::to_string_pretty(&binding).map_err(|e| {
            LimError::tool_execution(
                "daemon_ledger",
                format!("failed to serialize workspace binding: {e}"),
                e,
            )
        })?;
        fs::write(path.as_std_path(), json).await.map_err(|e| {
            LimError::tool_execution("daemon_ledger", "failed to write workspace.json", e)
        })?;
        Ok(())
    }

    /// Load the workspace binding for a goal (if any).
    pub async fn workspace_binding(&self, goal_id: &GoalId) -> Result<Option<WorkspaceBinding>> {
        let path = self.workspace_binding_path(goal_id);
        match fs::read_to_string(path.as_std_path()).await {
            Ok(content) => {
                let binding: WorkspaceBinding = serde_json::from_str(&content).map_err(|e| {
                    LimError::tool_execution(
                        "daemon_ledger",
                        format!("failed to parse workspace.json: {e}"),
                        e,
                    )
                })?;
                Ok(Some(binding))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => {
                Err(LimError::tool_execution("daemon_ledger", "failed to read workspace.json", e))
            }
        }
    }

    /// Sync a goal's state to a workspace's `.lim/loop/` directory.
    ///
    /// This mirrors `state.json` and `goal.md` from the daemon's independent
    /// storage into `<workspace>/.lim/loop/<goal_id>/`, so workspace-local
    /// tools (TUI `/loop status`, CLI `lim loop --status`) can see the goal.
    ///
    /// Returns the path where the state was written in the workspace.
    pub async fn sync_to_workspace(
        &self,
        goal_id: &GoalId,
        workspace_root: &Utf8Path,
    ) -> Result<Utf8PathBuf> {
        let state = self.load(goal_id).await?;
        let goal_md =
            fs::read_to_string(self.goal_md_path(goal_id).as_std_path()).await.map_err(|e| {
                LimError::tool_execution(
                    "daemon_ledger",
                    format!("failed to read goal.md for sync: {e}"),
                    e,
                )
            })?;

        let ws_goal_dir = workspace_root.join(".lim").join("loop").join(goal_id.to_string());
        fs::create_dir_all(ws_goal_dir.as_std_path()).await.map_err(|e| {
            LimError::tool_execution(
                "daemon_ledger",
                format!("failed to create workspace goal dir `{ws_goal_dir}`: {e}"),
                e,
            )
        })?;

        let ws_state_path = ws_goal_dir.join("state.json");
        let ws_goal_md_path = ws_goal_dir.join("goal.md");

        let state_json = serde_json::to_string_pretty(&state).map_err(|e| {
            LimError::tool_execution(
                "daemon_ledger",
                format!("failed to serialize state for sync: {e}"),
                e,
            )
        })?;

        fs::write(ws_state_path.as_std_path(), &state_json).await.map_err(|e| {
            LimError::tool_execution("daemon_ledger", "failed to write workspace state.json", e)
        })?;
        fs::write(ws_goal_md_path.as_std_path(), &goal_md).await.map_err(|e| {
            LimError::tool_execution("daemon_ledger", "failed to write workspace goal.md", e)
        })?;

        debug!(
            goal_id = %goal_id,
            workspace = %workspace_root,
            "synced goal state to workspace"
        );
        Ok(ws_goal_dir)
    }

    /// Access the storage root path.
    pub fn storage_root(&self) -> &Utf8Path {
        &self.storage_root
    }
}

#[async_trait]
impl LedgerBackend for DaemonLedger {
    async fn create_goal(&self, goal: &str) -> Result<GoalId> {
        self.ensure_storage_root().await?;
        let goal_id = GoalId::new();
        let dir = self.goal_dir(&goal_id);
        fs::create_dir_all(dir.as_std_path()).await.map_err(|e| {
            LimError::tool_execution(
                "daemon_ledger",
                format!("failed to create goal dir `{dir}`: {e}"),
                e,
            )
        })?;

        // goal.md
        let goal_md = format!("# Goal\n\n{goal}\n");
        fs::write(self.goal_md_path(&goal_id).as_std_path(), goal_md)
            .await
            .map_err(|e| LimError::tool_execution("daemon_ledger", "failed to write goal.md", e))?;

        // state.json
        let state = LoopState::new(goal);
        let json = serde_json::to_string_pretty(&state).map_err(|e| {
            LimError::tool_execution("daemon_ledger", format!("failed to serialize state: {e}"), e)
        })?;
        fs::write(self.state_path(&goal_id).as_std_path(), json).await.map_err(|e| {
            LimError::tool_execution("daemon_ledger", "failed to write state.json", e)
        })?;

        debug!(goal_id = %goal_id, "daemon ledger: created new goal");
        Ok(goal_id)
    }

    async fn load(&self, goal_id: &GoalId) -> Result<LoopState> {
        let path = self.state_path(goal_id);
        let content = fs::read_to_string(path.as_std_path()).await.map_err(|e| {
            LimError::tool_execution(
                "daemon_ledger",
                format!("failed to read state for goal {goal_id}: {e}"),
                e,
            )
        })?;
        let state: LoopState = serde_json::from_str(&content).map_err(|e| {
            LimError::tool_execution(
                "daemon_ledger",
                format!("failed to parse state for goal {goal_id}: {e}"),
                e,
            )
        })?;
        Ok(state)
    }

    async fn save(&self, goal_id: &GoalId, state: &LoopState) -> Result<()> {
        let path = self.state_path(goal_id);
        let json = serde_json::to_string_pretty(state).map_err(|e| {
            LimError::tool_execution("daemon_ledger", format!("failed to serialize state: {e}"), e)
        })?;
        fs::write(path.as_std_path(), json).await.map_err(|e| {
            LimError::tool_execution(
                "daemon_ledger",
                format!("failed to write state for goal {goal_id}: {e}"),
                e,
            )
        })?;
        debug!(goal_id = %goal_id, status = ?state.status, "daemon ledger: state saved");
        Ok(())
    }

    async fn list_goals(&self) -> Result<Vec<(GoalId, LoopState)>> {
        if !self.storage_root.exists() {
            return Ok(Vec::new());
        }

        let mut entries = fs::read_dir(self.storage_root.as_std_path()).await.map_err(|e| {
            LimError::tool_execution(
                "daemon_ledger",
                format!("failed to read storage root `{}`: {e}", self.storage_root),
                e,
            )
        })?;

        let mut goals = Vec::new();
        while let Some(entry) = entries.next_entry().await.map_err(|e| {
            LimError::tool_execution("daemon_ledger", format!("failed to read dir entry: {e}"), e)
        })? {
            let name = match entry.file_name().to_str() {
                Some(s) => s.to_string(),
                None => continue,
            };
            let goal_id: GoalId = match name.parse() {
                Ok(id) => id,
                Err(_) => continue,
            };
            match self.load(&goal_id).await {
                Ok(state) => goals.push((goal_id, state)),
                Err(e) => {
                    warn!(goal_id = %goal_id, error = %e, "daemon ledger: skipping goal: failed to load state");
                }
            }
        }

        goals.sort_by_key(|(_, s)| std::cmp::Reverse(s.updated_at));
        Ok(goals)
    }
}

// ---------------------------------------------------------------------------
// LedgerBackend impl for StateLedger (delegates to inherent methods)
// ---------------------------------------------------------------------------

#[async_trait]
impl LedgerBackend for super::state::StateLedger {
    async fn create_goal(&self, goal: &str) -> Result<GoalId> {
        // Delegates to the inherent method — `self` is `&StateLedger` so
        // Rust resolves to the inherent impl, not this trait method.
        self.create_goal(goal).await
    }

    async fn load(&self, goal_id: &GoalId) -> Result<LoopState> {
        self.load(goal_id).await
    }

    async fn save(&self, goal_id: &GoalId, state: &LoopState) -> Result<()> {
        self.save(goal_id, state).await
    }

    async fn list_goals(&self) -> Result<Vec<(GoalId, LoopState)>> {
        self.list_goals().await
    }
}

/// Helper: construct an `Arc<dyn LedgerBackend>` from any ledger.
pub fn into_ledger_arc<L: LedgerBackend + 'static>(ledger: L) -> Arc<dyn LedgerBackend> {
    Arc::new(ledger)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::super::state::GoalStatus;
    use super::*;

    async fn setup_daemon_ledger(name: &str) -> DaemonLedger {
        let dir = std::env::temp_dir().join(format!("lim-test-daemon-ledger-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        DaemonLedger::new(Utf8PathBuf::from_path_buf(dir).unwrap())
    }

    #[tokio::test]
    async fn daemon_ledger_create_and_load() {
        let ledger = setup_daemon_ledger("create-load").await;
        let goal_id = ledger.create_goal("daemon test goal").await.unwrap();
        let state = ledger.load(&goal_id).await.unwrap();
        assert_eq!(state.goal, "daemon test goal");
        assert_eq!(state.session_count, 0);
        assert_eq!(state.status, GoalStatus::Running);
    }

    #[tokio::test]
    async fn daemon_ledger_save_and_reload() {
        let ledger = setup_daemon_ledger("save-reload").await;
        let goal_id = ledger.create_goal("save test").await.unwrap();
        let mut state = ledger.load(&goal_id).await.unwrap();
        state.session_count = 5;
        state.status = GoalStatus::Success;
        ledger.save(&goal_id, &state).await.unwrap();

        let reloaded = ledger.load(&goal_id).await.unwrap();
        assert_eq!(reloaded.session_count, 5);
        assert_eq!(reloaded.status, GoalStatus::Success);
    }

    #[tokio::test]
    async fn daemon_ledger_list_goals() {
        let ledger = setup_daemon_ledger("list").await;
        let id1 = ledger.create_goal("goal 1").await.unwrap();
        let id2 = ledger.create_goal("goal 2").await.unwrap();
        let goals = ledger.list_goals().await.unwrap();
        assert_eq!(goals.len(), 2);
        // Both should be present
        let ids: Vec<_> = goals.iter().map(|(id, _)| *id).collect();
        assert!(ids.contains(&id1));
        assert!(ids.contains(&id2));
    }

    #[tokio::test]
    async fn daemon_ledger_list_empty_when_no_dir() {
        let dir = std::env::temp_dir().join("lim-test-daemon-ledger-nonexistent");
        let _ = std::fs::remove_dir_all(&dir);
        let ledger = DaemonLedger::new(Utf8PathBuf::from_path_buf(dir).unwrap());
        let goals = ledger.list_goals().await.unwrap();
        assert!(goals.is_empty());
    }

    #[tokio::test]
    async fn daemon_ledger_workspace_binding() {
        let ledger = setup_daemon_ledger("binding").await;
        let goal_id = ledger.create_goal("binding test").await.unwrap();

        // No binding initially
        assert!(ledger.workspace_binding(&goal_id).await.unwrap().is_none());

        // Bind to a workspace
        ledger.bind_workspace(&goal_id, "/some/workspace", true).await.unwrap();
        let binding = ledger.workspace_binding(&goal_id).await.unwrap().unwrap();
        assert_eq!(binding.workspace_root, "/some/workspace");
        assert!(binding.primary);
    }

    #[tokio::test]
    async fn daemon_ledger_sync_to_workspace() {
        let daemon_ledger = setup_daemon_ledger("sync-daemon").await;
        let ws_dir = std::env::temp_dir().join("lim-test-daemon-ledger-sync-ws");
        let _ = std::fs::remove_dir_all(&ws_dir);
        let ws_root = Utf8PathBuf::from_path_buf(ws_dir.clone()).unwrap();

        let goal_id = daemon_ledger.create_goal("sync test goal").await.unwrap();

        // Sync to workspace
        let ws_goal_dir = daemon_ledger.sync_to_workspace(&goal_id, &ws_root).await.unwrap();

        // Verify state.json exists in workspace
        let ws_state_path = ws_goal_dir.join("state.json");
        assert!(ws_state_path.exists());

        // Verify StateLedger can read it
        let ws_ledger = super::super::state::StateLedger::new(ws_root);
        let state = ws_ledger.load(&goal_id).await.unwrap();
        assert_eq!(state.goal, "sync test goal");

        let _ = std::fs::remove_dir_all(&ws_dir);
    }

    #[tokio::test]
    async fn state_ledger_implements_ledger_backend() {
        // Verify StateLedger works through the trait.
        let dir = std::env::temp_dir().join("lim-test-trait-state");
        let _ = std::fs::remove_dir_all(&dir);
        let ledger =
            super::super::state::StateLedger::new(Utf8PathBuf::from_path_buf(dir.clone()).unwrap());

        let ledger: Arc<dyn LedgerBackend> = Arc::new(ledger);
        let goal_id = ledger.create_goal("trait test").await.unwrap();
        let state = ledger.load(&goal_id).await.unwrap();
        assert_eq!(state.goal, "trait test");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
