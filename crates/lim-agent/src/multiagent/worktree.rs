//! Git worktree management for multi-agent workspace isolation.
//!
//! Each worktree is a temporary checkout of the project repository at a
//! specific branch, allowing multiple agents to work in parallel without
//! clobbering each other's files. Worktrees are stored under
//! `.lim/worktrees/<wt-id>/` (project-local) or
//! `~/.local/share/lim/worktrees/<wt-id>/` (global).
//!
//! ## Design
//!
//! - `WorktreeSpec` — declarative inputs (role, branch, base branch)
//! - `WorktreeHandle` — RAII handle: `create_worktree()` returns one,
//!   `remove()` cleans up. `Drop` auto-removes if `auto_cleanup` is set.
//! - `create_worktree()` / `remove_worktree()` — free functions wrapping
//!   `git worktree add` / `git worktree remove`
//!
//! ## Path layout
//!
//! ```text
//! <project_root>/.lim/worktrees/<wt-id>/
//!   └── ... (full checkout at the specified branch)
//! ```
//!
//! The `<wt-id>` is a short ULID, generated per worktree.

use std::sync::Mutex;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};
use ulid::Ulid;

use lim_core::error::{LimError, Result};

/// Declarative spec for creating a worktree.
///
/// Callers (e.g. the Scheduler, or CLI `--worktree`) build this spec, then
/// pass it to `create_worktree()` to materialize the worktree on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeSpec {
    /// Agent role this worktree is for (e.g. "reviewer", "coder").
    /// Used for naming/logging only — does not affect the worktree itself.
    pub agent_role: String,
    /// Branch name to create/check out in the worktree.
    pub branch: String,
    /// Base branch to create the new branch from (defaults to HEAD).
    /// If None, uses the current HEAD of the repository.
    pub base_branch: Option<String>,
    /// Whether to auto-remove the worktree when the handle is dropped.
    /// Default: `true` — worktrees are ephemeral by design.
    #[serde(default = "default_auto_cleanup")]
    pub auto_cleanup: bool,
}

fn default_auto_cleanup() -> bool {
    true
}

impl WorktreeSpec {
    /// Create a spec with the given role + branch, auto_cleanup = true.
    pub fn new(agent_role: impl Into<String>, branch: impl Into<String>) -> Self {
        Self {
            agent_role: agent_role.into(),
            branch: branch.into(),
            base_branch: None,
            auto_cleanup: true,
        }
    }

    /// Set the base branch (default: HEAD).
    pub fn with_base_branch(mut self, base: impl Into<String>) -> Self {
        self.base_branch = Some(base.into());
        self
    }

    /// Disable auto-cleanup (worktree persists after handle is dropped).
    pub fn persistent(mut self) -> Self {
        self.auto_cleanup = false;
        self
    }
}

/// Opaque worktree ID (wraps a ULID string).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorktreeId(String);

impl WorktreeId {
    /// Generate a new unique worktree ID.
    pub fn new() -> Self {
        Self(Ulid::new().to_string().to_lowercase())
    }

    /// Get the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WorktreeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Default for WorktreeId {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII handle to a created worktree.
///
/// Holds the worktree's path and metadata. When dropped, if `auto_cleanup`
/// is true, the worktree is removed via `git worktree remove`.
///
/// To explicitly remove the worktree (without relying on Drop), call
/// `remove()`. This consumes the handle and returns the result of the
/// `git worktree remove` command.
///
/// **Thread safety**: The handle uses an internal `Mutex` to protect the
/// `removed` flag, ensuring Drop doesn't double-remove even if the handle
/// is shared (though typically it isn't).
#[derive(Debug)]
pub struct WorktreeHandle {
    id: WorktreeId,
    path: Utf8PathBuf,
    branch: String,
    auto_cleanup: bool,
    /// Project root (where `.git` lives) — needed for `git worktree remove`.
    project_root: Utf8PathBuf,
    /// Track whether the worktree has already been removed (by `remove()`
    /// or a previous Drop). Guarded by a Mutex to allow Drop to safely
    /// check-and-set.
    removed: Mutex<bool>,
}

impl WorktreeHandle {
    /// The worktree ID (ULID string).
    pub fn id(&self) -> &WorktreeId {
        &self.id
    }

    /// The filesystem path of the worktree (e.g. `.lim/worktrees/01j.../`).
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }

    /// The branch checked out in this worktree.
    pub fn branch(&self) -> &str {
        &self.branch
    }

    /// Explicitly remove the worktree via `git worktree remove`.
    ///
    /// This is the preferred way to clean up — it returns the result of
    /// the git command so the caller can handle errors. Drop will silently
    /// log errors on failure.
    ///
    /// After `remove()` returns, the handle is consumed and cannot be used
    /// again. Drop will be a no-op.
    pub fn remove(self) -> Result<()> {
        let mut removed = self.removed.lock().expect("WorktreeHandle mutex not poisoned");
        if *removed {
            return Ok(());
        }
        *removed = true;
        drop(removed);
        remove_worktree_inner(&self.project_root, &self.path)
    }
}

impl Drop for WorktreeHandle {
    fn drop(&mut self) {
        let mut removed = self.removed.lock().expect("WorktreeHandle mutex not poisoned");
        if *removed {
            return;
        }
        if !self.auto_cleanup {
            // Persistent worktree — leave it on disk.
            debug!(id = %self.id, path = %self.path, "worktree handle dropped (persistent, not removing)");
            return;
        }
        *removed = true;
        // Drop the guard before calling git (which may take time) — we've
        // already claimed responsibility for removal.
        drop(removed);
        if let Err(e) = remove_worktree_inner(&self.project_root, &self.path) {
            warn!(id = %self.id, path = %self.path, error = %e, "failed to auto-remove worktree on Drop");
        } else {
            debug!(id = %self.id, path = %self.path, "worktree auto-removed on Drop");
        }
    }
}

/// Create a new git worktree under `<project_root>/.lim/worktrees/<wt-id>/`.
///
/// This runs `git -C <project_root> worktree add <path> -b <branch> [<base>]`.
///
/// The worktree's parent directory (`.lim/worktrees/`) is created if it
/// doesn't exist. The worktree path itself is created by `git worktree add`.
///
/// Returns a `WorktreeHandle` that will auto-remove the worktree on Drop
/// (unless `spec.auto_cleanup` is false).
pub async fn create_worktree(
    project_root: &Utf8Path,
    spec: WorktreeSpec,
) -> Result<WorktreeHandle> {
    let id = WorktreeId::new();
    let worktrees_dir = project_root.join(".lim").join("worktrees");
    let wt_path = worktrees_dir.join(id.as_str());

    // Ensure `.lim/worktrees/` exists. Git will create the leaf directory.
    tokio::fs::create_dir_all(&worktrees_dir).await.map_err(|e| {
        LimError::tool_execution(
            "worktree",
            format!("failed to create worktrees dir `{worktrees_dir}`: {e}"),
            e,
        )
    })?;

    // Build the git command: git -C <root> worktree add <path> -b <branch> [<base>]
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C").arg(project_root.as_str());
    cmd.arg("worktree").arg("add");
    cmd.arg(wt_path.as_str());
    cmd.arg("-b").arg(&spec.branch);
    if let Some(ref base) = spec.base_branch {
        cmd.arg(base);
    }

    debug!(
        id = %id,
        role = %spec.agent_role,
        branch = %spec.branch,
        base = ?spec.base_branch,
        path = %wt_path,
        "creating worktree"
    );

    let output = cmd.output().await.map_err(|e| {
        LimError::tool_execution("worktree", format!("failed to spawn git worktree add: {e}"), e)
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        // Clean up the empty leaf directory if git created it but failed.
        let _ = tokio::fs::remove_dir(&wt_path).await;
        return Err(LimError::tool_execution(
            "worktree",
            format!(
                "git worktree add failed (exit {:?}):\n{}",
                output.status.code(),
                if stderr.is_empty() { stdout.trim() } else { stderr.trim() }
            ),
            std::io::Error::other("git worktree add failed"),
        ));
    }

    info!(
        id = %id,
        role = %spec.agent_role,
        branch = %spec.branch,
        path = %wt_path,
        "worktree created"
    );

    Ok(WorktreeHandle {
        id,
        path: wt_path,
        branch: spec.branch,
        auto_cleanup: spec.auto_cleanup,
        project_root: project_root.to_path_buf(),
        removed: Mutex::new(false),
    })
}

/// Remove a worktree via `git worktree remove`.
///
/// Called by `WorktreeHandle::remove()` and `Drop`. Not intended to be
/// called directly — use the handle's methods instead.
fn remove_worktree_inner(project_root: &Utf8Path, wt_path: &Utf8Path) -> Result<()> {
    debug!(path = %wt_path, "removing worktree");
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(project_root.as_str())
        .arg("worktree")
        .arg("remove")
        .arg("--force")
        .arg(wt_path.as_str())
        .output()
        .map_err(|e| {
            LimError::tool_execution(
                "worktree",
                format!("failed to spawn git worktree remove: {e}"),
                e,
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Best-effort: if git complains the worktree is already gone, treat
        // it as success. Common when the directory was deleted manually.
        if stderr.contains("not a working tree") || stderr.contains("already") {
            debug!(path = %wt_path, stderr = %stderr.trim(), "worktree already removed");
            return Ok(());
        }
        return Err(LimError::tool_execution(
            "worktree",
            format!(
                "git worktree remove failed (exit {:?}):\n{}",
                output.status.code(),
                stderr.trim()
            ),
            std::io::Error::other("git worktree remove failed"),
        ));
    }

    // Best-effort cleanup of the now-empty leaf directory (git removes the
    // worktree contents but may leave the parent `.lim/worktrees/<id>/`).
    let _ = std::fs::remove_dir(wt_path.as_std_path());

    info!(path = %wt_path, "worktree removed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_spec_new_defaults() {
        let spec = WorktreeSpec::new("reviewer", "review-001");
        assert_eq!(spec.agent_role, "reviewer");
        assert_eq!(spec.branch, "review-001");
        assert_eq!(spec.base_branch, None);
        assert!(spec.auto_cleanup, "auto_cleanup should default to true");
    }

    #[test]
    fn worktree_spec_with_base_branch() {
        let spec = WorktreeSpec::new("coder", "feature-x").with_base_branch("main");
        assert_eq!(spec.base_branch.as_deref(), Some("main"));
    }

    #[test]
    fn worktree_spec_persistent_disables_auto_cleanup() {
        let spec = WorktreeSpec::new("coder", "feature-x").persistent();
        assert!(!spec.auto_cleanup);
    }

    #[test]
    fn worktree_spec_serde_roundtrip() {
        let spec = WorktreeSpec::new("reviewer", "rev-1").with_base_branch("main");
        let json = serde_json::to_string(&spec).unwrap();
        let parsed: WorktreeSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.agent_role, spec.agent_role);
        assert_eq!(parsed.branch, spec.branch);
        assert_eq!(parsed.base_branch, spec.base_branch);
        assert_eq!(parsed.auto_cleanup, spec.auto_cleanup);
    }

    #[test]
    fn worktree_id_is_unique() {
        let a = WorktreeId::new();
        let b = WorktreeId::new();
        assert_ne!(a, b, "worktree IDs should be unique");
    }

    #[test]
    fn worktree_id_display() {
        let id = WorktreeId::new();
        let s = id.to_string();
        assert!(!s.is_empty());
        assert_eq!(s, id.as_str());
    }

    /// Integration test: create + remove a real worktree in a temp git repo.
    ///
    /// Skipped if `git` is not available or if we're not in an environment
    /// that can create temp git repos.
    #[tokio::test]
    async fn create_and_remove_worktree_in_temp_repo() {
        // Skip if git is not available.
        if std::process::Command::new("git").arg("--version").output().is_err() {
            eprintln!("skipping worktree integration test: git not available");
            return;
        }

        // Create a temp git repo with at least one commit (worktree needs HEAD).
        let tmp = tempfile::tempdir().unwrap();
        let repo_path = tmp.path();
        let repo = Utf8PathBuf::from_path_buf(repo_path.to_path_buf()).unwrap();

        std::process::Command::new("git").arg("init").current_dir(repo_path).output().unwrap();
        // Configure git user (required for commit).
        std::process::Command::new("git")
            .args(["config", "user.email", "test@lim.dev"])
            .current_dir(repo_path)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Lim Test"])
            .current_dir(repo_path)
            .output()
            .unwrap();
        // Create a file + commit (worktree needs at least one commit on HEAD).
        std::fs::write(repo_path.join("README.md"), "# test repo\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "README.md"])
            .current_dir(repo_path)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(repo_path)
            .output()
            .unwrap();

        // Create a worktree.
        let spec = WorktreeSpec::new("reviewer", "review-test");
        let handle = create_worktree(&repo, spec).await.expect("create_worktree");

        // The worktree path should exist.
        assert!(handle.path().exists(), "worktree path should exist");
        assert!(handle.path().join("README.md").exists(), "worktree should have the file");

        // Capture path before remove (remove consumes the handle).
        let wt_path = handle.path().to_path_buf();
        handle.remove().expect("remove_worktree");

        // The worktree path should be gone.
        assert!(!wt_path.exists(), "worktree path should be removed");
    }

    /// Verify that Drop auto-removes a worktree when auto_cleanup is true.
    #[tokio::test]
    async fn drop_auto_removes_worktree() {
        if std::process::Command::new("git").arg("--version").output().is_err() {
            eprintln!("skipping worktree integration test: git not available");
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let repo_path = tmp.path();
        let repo = Utf8PathBuf::from_path_buf(repo_path.to_path_buf()).unwrap();

        std::process::Command::new("git").arg("init").current_dir(repo_path).output().unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "test@lim.dev"])
            .current_dir(repo_path)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Lim Test"])
            .current_dir(repo_path)
            .output()
            .unwrap();
        std::fs::write(repo_path.join("file.txt"), "content\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "file.txt"])
            .current_dir(repo_path)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(repo_path)
            .output()
            .unwrap();

        let path;
        {
            let spec = WorktreeSpec::new("coder", "feature-auto-drop");
            let handle = create_worktree(&repo, spec).await.expect("create_worktree");
            path = handle.path().to_path_buf();
            assert!(path.exists(), "worktree should exist while handle is alive");
            // handle drops here — auto_cleanup should trigger.
        }

        // After the handle is dropped, the worktree should be removed.
        assert!(!path.exists(), "worktree should be auto-removed on Drop");
    }

    /// Verify that persistent worktrees survive Drop.
    #[tokio::test]
    async fn persistent_worktree_survives_drop() {
        if std::process::Command::new("git").arg("--version").output().is_err() {
            eprintln!("skipping worktree integration test: git not available");
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let repo_path = tmp.path();
        let repo = Utf8PathBuf::from_path_buf(repo_path.to_path_buf()).unwrap();

        std::process::Command::new("git").arg("init").current_dir(repo_path).output().unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "test@lim.dev"])
            .current_dir(repo_path)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Lim Test"])
            .current_dir(repo_path)
            .output()
            .unwrap();
        std::fs::write(repo_path.join("file.txt"), "content\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "file.txt"])
            .current_dir(repo_path)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(repo_path)
            .output()
            .unwrap();

        let path;
        {
            let spec = WorktreeSpec::new("coder", "persistent-branch").persistent();
            let handle = create_worktree(&repo, spec).await.expect("create_worktree");
            path = handle.path().to_path_buf();
            // handle drops here — auto_cleanup is false, should NOT remove.
        }

        // The worktree should still exist.
        assert!(path.exists(), "persistent worktree should survive Drop");

        // Clean up manually (so the temp dir can be removed).
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_path)
            .args(["worktree", "remove", "--force"])
            .arg(path.as_str())
            .output();
    }
}
