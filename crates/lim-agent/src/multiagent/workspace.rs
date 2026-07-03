//! Multi-workspace management: registry of workspaces for daemon mode.
//!
//! A daemon process can manage goals across multiple workspaces. Each
//! workspace has its own root path, trust level, and local `StateLedger`.
//! The `WorkspaceRegistry` tracks all known workspaces and provides lookup
//! by ID or path.
//!
//! ## Design
//!
//! This is the extension point for Stage 4 (Daemon/Server) multi-workspace
//! support. In P3 we provide the data structures and registry; actual
//! cross-workspace goal execution is a future enhancement.
//!
//! ```text
//! WorkspaceRegistry
//! ├── WorkspaceId  — opaque identifier
//! ├── Workspace    — root path + trust level + display name
//! └── lookup by ID / path
//! ```

use std::collections::HashMap;
use std::sync::Mutex;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use lim_core::TrustLevel;

/// Opaque identifier for a registered workspace.
///
/// Wraps a `String` so it can carry human-readable names (e.g. `"lim"`,
/// `"novel-pack"`) or generated IDs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkspaceId(String);

impl WorkspaceId {
    /// Create a workspace ID from a string.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Get the inner string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for WorkspaceId {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_string()))
    }
}

/// A registered workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    /// Unique identifier for this workspace.
    pub id: WorkspaceId,
    /// Absolute path to the workspace root.
    pub root: Utf8PathBuf,
    /// Trust level for operations in this workspace.
    pub trust_level: TrustLevel,
    /// Human-readable display name (defaults to the directory name).
    pub display_name: String,
    /// When this workspace was registered (Unix seconds).
    pub registered_at: u64,
}

impl Workspace {
    /// Create a new workspace entry.
    pub fn new(
        id: impl Into<String>,
        root: impl Into<Utf8PathBuf>,
        trust_level: TrustLevel,
    ) -> Self {
        let root = root.into();
        let display_name =
            root.file_name().map(|s| s.to_string()).unwrap_or_else(|| root.to_string());
        Self {
            id: WorkspaceId::new(id),
            root,
            trust_level,
            display_name,
            registered_at: unix_now(),
        }
    }
}

/// Registry of workspaces known to the daemon.
///
/// Thread-safe via a `Mutex`. Lookups are O(1) by ID. Path-based lookup
/// normalizes the input path before comparison.
pub struct WorkspaceRegistry {
    inner: Mutex<HashMap<WorkspaceId, Workspace>>,
}

impl WorkspaceRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self { inner: Mutex::new(HashMap::new()) }
    }

    /// Register a workspace. If a workspace with the same ID already exists,
    /// it is replaced.
    pub fn register(&self, workspace: Workspace) {
        self.inner
            .lock()
            .expect("WorkspaceRegistry mutex not poisoned")
            .insert(workspace.id.clone(), workspace);
    }

    /// Look up a workspace by ID.
    pub fn get(&self, id: &WorkspaceId) -> Option<Workspace> {
        self.inner.lock().expect("WorkspaceRegistry mutex not poisoned").get(id).cloned()
    }

    /// Look up a workspace by its root path (exact match after normalization).
    pub fn get_by_path(&self, path: &camino::Utf8Path) -> Option<Workspace> {
        let normalized = normalize_path(path);
        self.inner
            .lock()
            .expect("WorkspaceRegistry mutex not poisoned")
            .values()
            .find(|ws| normalize_path(&ws.root) == normalized)
            .cloned()
    }

    /// List all registered workspaces.
    pub fn list(&self) -> Vec<Workspace> {
        self.inner.lock().expect("WorkspaceRegistry mutex not poisoned").values().cloned().collect()
    }

    /// Number of registered workspaces.
    pub fn len(&self) -> usize {
        self.inner.lock().expect("WorkspaceRegistry mutex not poisoned").len()
    }

    /// Is the registry empty?
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Remove a workspace by ID.
    pub fn unregister(&self, id: &WorkspaceId) -> Option<Workspace> {
        self.inner.lock().expect("WorkspaceRegistry mutex not poisoned").remove(id)
    }
}

impl Default for WorkspaceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Normalize a path for comparison: strip trailing `/`, resolve `.` segments.
/// Does NOT resolve `..` or symlinks (those require filesystem access).
fn normalize_path(path: &camino::Utf8Path) -> Utf8PathBuf {
    let mut buf = Utf8PathBuf::new();
    for component in path.components() {
        match component {
            camino::Utf8Component::CurDir => { /* skip `.` */ }
            other => buf.push(other.as_str()),
        }
    }
    if buf.as_str().is_empty() { Utf8PathBuf::from(".") } else { buf }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_get_by_id() {
        let registry = WorkspaceRegistry::new();
        let ws = Workspace::new("lim", "/home/user/lim", TrustLevel::Workspace);
        registry.register(ws);

        let found = registry.get(&WorkspaceId::new("lim")).unwrap();
        assert_eq!(found.root, "/home/user/lim");
        assert_eq!(found.trust_level, TrustLevel::Workspace);
        assert_eq!(found.display_name, "lim");
    }

    #[test]
    fn get_by_path_finds_workspace() {
        let registry = WorkspaceRegistry::new();
        registry.register(Workspace::new("project", "/home/user/project", TrustLevel::Workspace));

        let found = registry.get_by_path(camino::Utf8Path::new("/home/user/project")).unwrap();
        assert_eq!(found.id, WorkspaceId::new("project"));
    }

    #[test]
    fn get_by_path_normalizes_dot_segments() {
        let registry = WorkspaceRegistry::new();
        registry.register(Workspace::new("lim", "/home/user/lim", TrustLevel::Workspace));

        // Path with `.` segment should still match
        let found = registry.get_by_path(camino::Utf8Path::new("/home/user/./lim"));
        assert!(found.is_some());
    }

    #[test]
    fn list_returns_all_workspaces() {
        let registry = WorkspaceRegistry::new();
        registry.register(Workspace::new("a", "/path/a", TrustLevel::Workspace));
        registry.register(Workspace::new("b", "/path/b", TrustLevel::Permissive));

        let list = registry.list();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn unregister_removes_workspace() {
        let registry = WorkspaceRegistry::new();
        registry.register(Workspace::new("temp", "/tmp/temp", TrustLevel::Workspace));
        assert_eq!(registry.len(), 1);

        let removed = registry.unregister(&WorkspaceId::new("temp")).unwrap();
        assert_eq!(removed.root, "/tmp/temp");
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let registry = WorkspaceRegistry::new();
        assert!(registry.get(&WorkspaceId::new("nope")).is_none());
    }

    #[test]
    fn display_name_defaults_to_dirname() {
        let ws = Workspace::new("custom-id", "/very/deep/path/my-project", TrustLevel::Workspace);
        assert_eq!(ws.display_name, "my-project");
    }

    #[test]
    fn workspace_id_roundtrips_through_string() {
        let id = WorkspaceId::new("test-id");
        let s = id.to_string();
        let parsed: WorkspaceId = s.parse().unwrap();
        assert_eq!(id, parsed);
    }
}
