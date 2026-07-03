//! XDG Base Directory paths for lim.
//!
//! Follows the [XDG Base Directory Specification](https://specifications.freedesktop.org/basedir-spec/).
//!
//! ## Paths
//!
//! | Variable | Default | lim usage |
//! |---|---|---|
//! | `XDG_CONFIG_HOME` | `~/.config` | `~/.config/lim/` — global config |
//! | `XDG_DATA_HOME` | `~/.local/share` | `~/.local/share/lim/` — global data (agents, sessions) |
//! | `XDG_CACHE_HOME` | `~/.cache` | `~/.cache/lim/` — cache (daemon socket, temp) |
//! | `XDG_RUNTIME_DIR` | (varies) | `$XDG_RUNTIME_DIR/lim/` — runtime (daemon.sock) |

use camino::Utf8PathBuf;

use crate::error::{ConfigError, ConfigResult};

/// Resolved XDG paths for lim.
///
/// All paths are absolute and the directories are guaranteed to be
/// creatable (but not guaranteed to exist yet — callers should `create_dir_all`
/// before writing).
#[derive(Debug, Clone)]
pub struct XdgPaths {
    /// `~/.config/lim/` — global config dir.
    pub config_dir: Utf8PathBuf,
    /// `~/.local/share/lim/` — global data dir.
    pub data_dir: Utf8PathBuf,
    /// `~/.cache/lim/` — cache dir.
    pub cache_dir: Utf8PathBuf,
    /// `$XDG_RUNTIME_DIR/lim/` — runtime dir (may be None on macOS).
    pub runtime_dir: Option<Utf8PathBuf>,
}

impl XdgPaths {
    /// Resolve XDG paths from environment variables, with sensible defaults.
    ///
    /// Uses the `dirs` crate which respects XDG env vars and falls back to
    /// platform defaults (e.g. `~/Library/Application Support` on macOS for
    /// config, but we override to XDG-style for consistency).
    pub fn detect() -> ConfigResult<Self> {
        // We use XDG-style paths even on macOS for consistency with Linux
        // deployments. The `dirs` crate gives us XDG env var support.
        let home: Utf8PathBuf = dirs::home_dir()
            .ok_or(ConfigError::HomeNotFound)
            .and_then(|p| Utf8PathBuf::from_path_buf(p).map_err(|_| ConfigError::HomeNotFound))?;

        let config_dir = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(v) if !v.is_empty() => Utf8PathBuf::from_path_buf(std::path::PathBuf::from(v))
                .map_err(|_| ConfigError::HomeNotFound)?,
            _ => home.join(".config"),
        };
        let data_dir = match std::env::var_os("XDG_DATA_HOME") {
            Some(v) if !v.is_empty() => Utf8PathBuf::from_path_buf(std::path::PathBuf::from(v))
                .map_err(|_| ConfigError::HomeNotFound)?,
            _ => home.join(".local/share"),
        };
        let cache_dir = match std::env::var_os("XDG_CACHE_HOME") {
            Some(v) if !v.is_empty() => Utf8PathBuf::from_path_buf(std::path::PathBuf::from(v))
                .map_err(|_| ConfigError::HomeNotFound)?,
            _ => home.join(".cache"),
        };
        let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|v| !v.is_empty())
            .and_then(|v| Utf8PathBuf::from_path_buf(std::path::PathBuf::from(v)).ok())
            .map(|p| p.join("lim"));

        Ok(Self {
            config_dir: config_dir.join("lim"),
            data_dir: data_dir.join("lim"),
            cache_dir: cache_dir.join("lim"),
            runtime_dir,
        })
    }

    /// `~/.config/lim/config.toml`
    pub fn global_config_file(&self) -> Utf8PathBuf {
        self.config_dir.join("config.toml")
    }

    /// `~/.config/lim/agents/<role>.toml`
    pub fn global_agent_config_dir(&self) -> Utf8PathBuf {
        self.config_dir.join("agents")
    }

    /// `~/.local/share/lim/agents/<agent_id>/` — global agent workspace dir.
    pub fn global_agent_data_dir(&self, agent_id: &str) -> Utf8PathBuf {
        self.data_dir.join("agents").join(agent_id)
    }

    /// `~/.local/share/lim/worktrees/<wt_id>/` — global worktree dir.
    pub fn global_worktree_dir(&self, wt_id: &str) -> Utf8PathBuf {
        self.data_dir.join("worktrees").join(wt_id)
    }

    /// `~/.local/share/lim/daemon/` — daemon state.
    pub fn daemon_data_dir(&self) -> Utf8PathBuf {
        self.data_dir.join("daemon")
    }

    /// `$XDG_RUNTIME_DIR/lim/daemon.sock` or `~/.cache/lim/daemon.sock` fallback.
    pub fn daemon_socket(&self) -> Utf8PathBuf {
        match &self.runtime_dir {
            Some(d) => d.join("daemon.sock"),
            None => self.cache_dir.join("daemon.sock"),
        }
    }

    /// `~/.cache/lim/sessions/` — session cache (for global sessions).
    pub fn session_cache_dir(&self) -> Utf8PathBuf {
        self.cache_dir.join("sessions")
    }
}

/// Project-local paths (inside the workspace directory).
///
/// All paths are relative to the workspace root and follow the `.lim/`
/// convention established in earlier phases.
#[derive(Debug, Clone)]
pub struct ProjectPaths {
    /// The workspace root (e.g. `/path/to/lim`).
    pub workspace_root: Utf8PathBuf,
}

impl ProjectPaths {
    /// Create a `ProjectPaths` rooted at the given workspace.
    ///
    /// Returns an error if the workspace root does not exist.
    pub fn new(workspace_root: Utf8PathBuf) -> ConfigResult<Self> {
        if !workspace_root.exists() {
            return Err(ConfigError::WorkspaceNotFound(workspace_root));
        }
        Ok(Self { workspace_root })
    }

    /// Detect the workspace root from the current directory.
    ///
    /// Walks up from `cwd` looking for the first directory containing
    /// `.lim/` or a `Cargo.toml` (for Rust projects). Falls back to `cwd`.
    pub fn detect() -> ConfigResult<Self> {
        let cwd = std::env::current_dir()
            .map_err(|e| ConfigError::Read { path: ".".into(), source: e })?;
        let cwd = Utf8PathBuf::from_path_buf(cwd).map_err(|_| ConfigError::HomeNotFound)?;

        let mut current = cwd.as_path();
        loop {
            if current.join(".lim").exists() {
                return Self::new(current.to_path_buf());
            }
            if current.join("Cargo.toml").exists() {
                return Self::new(current.to_path_buf());
            }
            match current.parent() {
                Some(p) => current = p,
                None => break,
            }
        }
        Self::new(cwd)
    }

    /// `<workspace>/.lim/`
    pub fn lim_dir(&self) -> Utf8PathBuf {
        self.workspace_root.join(".lim")
    }

    /// `<workspace>/.lim/config.toml`
    pub fn project_config_file(&self) -> Utf8PathBuf {
        self.lim_dir().join("config.toml")
    }

    /// `<workspace>/.lim/agents/<role>.toml`
    pub fn project_agent_config_dir(&self) -> Utf8PathBuf {
        self.lim_dir().join("agents")
    }

    /// `<workspace>/.lim/loop/<goal_id>/`
    pub fn loop_dir(&self, goal_id: &str) -> Utf8PathBuf {
        self.lim_dir().join("loop").join(goal_id)
    }

    /// `<workspace>/.lim/worktrees/<wt_id>/`
    pub fn worktree_dir(&self, wt_id: &str) -> Utf8PathBuf {
        self.lim_dir().join("worktrees").join(wt_id)
    }

    /// `<workspace>/.lim/daemon/` (daemon state synced to workspace).
    pub fn daemon_sync_dir(&self) -> Utf8PathBuf {
        self.lim_dir().join("daemon")
    }

    /// `<workspace>/sessions/`
    pub fn sessions_dir(&self) -> Utf8PathBuf {
        self.workspace_root.join("sessions")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_paths_detect_uses_env_vars() {
        // This test relies on env vars being set; just verify it doesn't panic.
        let paths = XdgPaths::detect();
        assert!(paths.is_ok());
        let paths = paths.unwrap();
        assert!(paths.config_dir.as_str().ends_with("lim"));
        assert!(paths.data_dir.as_str().ends_with("lim"));
        assert!(paths.cache_dir.as_str().ends_with("lim"));
    }

    #[test]
    fn project_paths_detect_finds_workspace() {
        // Running tests from the project root should detect the workspace.
        let paths = ProjectPaths::detect();
        assert!(paths.is_ok());
        let paths = paths.unwrap();
        // The workspace should contain Cargo.toml or .lim/
        assert!(paths.workspace_root.join("Cargo.toml").exists() || paths.lim_dir().exists());
    }

    #[test]
    fn project_paths_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let paths = ProjectPaths::new(root.clone()).unwrap();

        assert_eq!(paths.lim_dir(), root.join(".lim"));
        assert_eq!(paths.project_config_file(), root.join(".lim/config.toml"));
        assert_eq!(paths.project_agent_config_dir(), root.join(".lim/agents"));
        assert_eq!(paths.loop_dir("01JABC"), root.join(".lim/loop/01JABC"));
        assert_eq!(paths.worktree_dir("wt-123"), root.join(".lim/worktrees/wt-123"));
        assert_eq!(paths.sessions_dir(), root.join("sessions"));
    }

    #[test]
    fn xdg_global_paths_layout() {
        let paths = XdgPaths::detect().unwrap();
        assert!(paths.global_config_file().as_str().ends_with("config.toml"));
        assert!(paths.global_agent_config_dir().as_str().ends_with("lim/agents"));
        assert!(paths.daemon_socket().as_str().ends_with("daemon.sock"));
    }
}
