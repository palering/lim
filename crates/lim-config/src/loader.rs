//! Configuration loader with layered merging.
//!
//! Merge priority (lowest to highest):
//! 1. Global config (`~/.config/lim/config.toml`)
//! 2. Project config (`<workspace>/.lim/config.toml`)
//! 3. CLI flags (applied by the caller via `apply_overrides`)

use std::collections::HashMap;

use camino::Utf8PathBuf;
use tracing::{debug, warn};

use crate::error::{ConfigError, ConfigResult};
use crate::paths::{ProjectPaths, XdgPaths};
use crate::schema::{AgentRoleConfig, ProviderEntry, WorkspaceConfig};

/// Loads and merges configuration from multiple layers.
#[derive(Debug, Clone)]
pub struct ConfigLoader {
    xdg: XdgPaths,
    project: ProjectPaths,
}

/// Loaded configuration — the fully merged result.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub workspace: WorkspaceConfig,
    pub xdg: XdgPaths,
    pub project: ProjectPaths,
    /// Agent role configs loaded from disk (project + global, merged per role).
    pub agent_roles: HashMap<String, AgentRoleConfig>,
}

impl ConfigLoader {
    /// Create a loader with detected XDG and project paths.
    pub fn detect() -> ConfigResult<Self> {
        let xdg = XdgPaths::detect()?;
        let project = ProjectPaths::detect()?;
        Ok(Self { xdg, project })
    }

    /// Create a loader with explicit paths (for testing).
    pub fn new(xdg: XdgPaths, project: ProjectPaths) -> Self {
        Self { xdg, project }
    }

    /// Load and merge configuration from all layers.
    ///
    /// Missing files are silently ignored (they just don't contribute to the
    /// merge). Parse errors are propagated.
    pub fn load(&self) -> ConfigResult<LoadedConfig> {
        let mut merged = WorkspaceConfig::default();

        // Layer 1: global config
        let global_path = self.xdg.global_config_file();
        if global_path.exists() {
            debug!(path = %global_path, "loading global config");
            match self.load_file(&global_path) {
                Ok(cfg) => {
                    merged = Self::merge(merged, cfg);
                }
                Err(e) => {
                    warn!(path = %global_path, error = %e, "failed to load global config, skipping");
                }
            }
        }

        // Layer 2: project config
        let project_path = self.project.project_config_file();
        if project_path.exists() {
            debug!(path = %project_path, "loading project config");
            match self.load_file(&project_path) {
                Ok(cfg) => {
                    merged = Self::merge(merged, cfg);
                }
                Err(e) => {
                    warn!(path = %project_path, error = %e, "failed to load project config, skipping");
                }
            }
        }

        // Load agent roles (project + global, project overrides global)
        let agent_roles = self.load_agent_roles(&merged.agents)?;

        Ok(LoadedConfig {
            workspace: merged,
            xdg: self.xdg.clone(),
            project: self.project.clone(),
            agent_roles,
        })
    }

    /// Load a single TOML config file.
    fn load_file(&self, path: &Utf8PathBuf) -> ConfigResult<WorkspaceConfig> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::Read { path: path.clone(), source: e })?;
        toml::from_str(&content).map_err(|e| ConfigError::Parse { path: path.clone(), source: e })
    }

    /// Merge two configs: `overlay` takes priority over `base`.
    fn merge(base: WorkspaceConfig, overlay: WorkspaceConfig) -> WorkspaceConfig {
        WorkspaceConfig {
            providers: Self::merge_providers(base.providers, overlay.providers),
            default_provider: overlay.default_provider.or(base.default_provider),
            workspace: if overlay.workspace != crate::schema::WorkspaceMode::default() {
                overlay.workspace
            } else {
                base.workspace
            },
            agents: if overlay.agents != crate::schema::AgentSource::default() {
                overlay.agents
            } else {
                base.agents
            },
        }
    }

    fn merge_providers(
        mut base: HashMap<String, ProviderEntry>,
        overlay: HashMap<String, ProviderEntry>,
    ) -> HashMap<String, ProviderEntry> {
        for (name, entry) in overlay {
            base.insert(name, entry);
        }
        base
    }

    /// Load agent role configs from the appropriate directories.
    ///
    /// Behavior depends on `agent_source`:
    /// - `Project`: only `.lim/agents/`
    /// - `Global`: only `~/.config/lim/agents/`
    /// - `Hybrid`: both, project overrides global (per role name)
    fn load_agent_roles(
        &self,
        agent_source: &crate::schema::AgentSource,
    ) -> ConfigResult<HashMap<String, AgentRoleConfig>> {
        use crate::schema::AgentSource;

        let mut roles: HashMap<String, AgentRoleConfig> = HashMap::new();

        let dirs_to_load: Vec<Utf8PathBuf> = match agent_source {
            AgentSource::Project => vec![self.project.project_agent_config_dir()],
            AgentSource::Global => vec![self.xdg.global_agent_config_dir()],
            AgentSource::Hybrid => {
                vec![self.xdg.global_agent_config_dir(), self.project.project_agent_config_dir()]
            }
        };

        for dir in dirs_to_load {
            if !dir.exists() {
                debug!(dir = %dir, "agent config dir does not exist, skipping");
                continue;
            }
            debug!(dir = %dir, "loading agent role configs");
            let entries = match std::fs::read_dir(dir.as_std_path()) {
                Ok(e) => e,
                Err(e) => {
                    warn!(dir = %dir, error = %e, "failed to read agent config dir");
                    continue;
                }
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                    continue;
                }
                let path = match Utf8PathBuf::from_path_buf(path) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                let content = match std::fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(e) => {
                        warn!(path = %path, error = %e, "failed to read agent config file");
                        continue;
                    }
                };
                match toml::from_str::<AgentRoleConfig>(&content) {
                    Ok(role) => {
                        debug!(role = %role.role, path = %path, "loaded agent role");
                        roles.insert(role.role.clone(), role);
                    }
                    Err(e) => {
                        warn!(path = %path, error = %e, "failed to parse agent config");
                    }
                }
            }
        }

        Ok(roles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{AgentSource, WorkspaceMode};

    #[test]
    fn merge_overlay_wins() {
        let base = WorkspaceConfig {
            default_provider: Some("base".into()),
            workspace: WorkspaceMode::Shared,
            agents: AgentSource::Project,
            providers: HashMap::new(),
        };
        let overlay = WorkspaceConfig {
            default_provider: Some("overlay".into()),
            workspace: WorkspaceMode::Worktree,
            agents: AgentSource::Hybrid,
            providers: HashMap::new(),
        };
        let merged = ConfigLoader::merge(base, overlay);
        assert_eq!(merged.default_provider.as_deref(), Some("overlay"));
        assert_eq!(merged.workspace, WorkspaceMode::Worktree);
        assert_eq!(merged.agents, AgentSource::Hybrid);
    }

    #[test]
    fn merge_keeps_base_when_overlay_is_default() {
        let base = WorkspaceConfig {
            default_provider: Some("base".into()),
            workspace: WorkspaceMode::Worktree,
            agents: AgentSource::Global,
            providers: HashMap::new(),
        };
        let overlay = WorkspaceConfig::default();
        let merged = ConfigLoader::merge(base, overlay);
        assert_eq!(merged.default_provider.as_deref(), Some("base"));
        assert_eq!(merged.workspace, WorkspaceMode::Worktree);
        assert_eq!(merged.agents, AgentSource::Global);
    }

    #[test]
    fn merge_providers_overlay_replaces() {
        let mut base = HashMap::new();
        base.insert(
            "openai".into(),
            ProviderEntry {
                kind: "openai-chat".into(),
                api_key: Some("base-key".into()),
                api_key_env: None,
                base_url: None,
                full_url: None,
                default_model: Some("gpt-4o".into()),
                models: vec![],
            },
        );
        let mut overlay = HashMap::new();
        overlay.insert(
            "openai".into(),
            ProviderEntry {
                kind: "openai-chat".into(),
                api_key: Some("overlay-key".into()),
                api_key_env: None,
                base_url: None,
                full_url: None,
                default_model: Some("gpt-4o-mini".into()),
                models: vec![],
            },
        );
        let merged = ConfigLoader::merge_providers(base, overlay);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged["openai"].api_key.as_deref(), Some("overlay-key"));
        assert_eq!(merged["openai"].default_model.as_deref(), Some("gpt-4o-mini"));
    }

    #[test]
    fn load_from_temp_dirs() {
        // Create temp XDG and project dirs
        let tmp = tempfile::tempdir().unwrap();
        let home = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();

        // Set up XDG paths manually
        let xdg = XdgPaths {
            config_dir: home.join(".config/lim"),
            data_dir: home.join(".local/share/lim"),
            cache_dir: home.join(".cache/lim"),
            runtime_dir: None,
        };
        std::fs::create_dir_all(&xdg.config_dir).unwrap();
        std::fs::write(
            xdg.global_config_file(),
            r#"
default_provider = "openai"
workspace = "shared"
"#,
        )
        .unwrap();

        // Set up project
        let project_root = home.join("myproject");
        std::fs::create_dir_all(project_root.join(".lim")).unwrap();
        std::fs::write(
            project_root.join(".lim/config.toml"),
            r#"
workspace = "worktree"
"#,
        )
        .unwrap();

        let project = ProjectPaths::new(project_root).unwrap();
        let loader = ConfigLoader::new(xdg.clone(), project);
        let loaded = loader.load().unwrap();

        // Project overrides global
        assert_eq!(loaded.workspace.default_provider.as_deref(), Some("openai"));
        assert_eq!(loaded.workspace.workspace, WorkspaceMode::Worktree);
    }

    #[test]
    fn load_agent_roles_hybrid() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();

        let xdg = XdgPaths {
            config_dir: home.join(".config/lim"),
            data_dir: home.join(".local/share/lim"),
            cache_dir: home.join(".cache/lim"),
            runtime_dir: None,
        };

        // Global agent
        let global_agents = xdg.global_agent_config_dir();
        std::fs::create_dir_all(&global_agents).unwrap();
        std::fs::write(
            global_agents.join("reviewer.toml"),
            r#"
role = "reviewer"
system_prompt = "Global reviewer"
model = "gpt-4o"
"#,
        )
        .unwrap();

        // Project agent overrides
        let project_root = home.join("project");
        std::fs::create_dir_all(project_root.join(".lim/agents")).unwrap();
        std::fs::write(
            project_root.join(".lim/agents/reviewer.toml"),
            r#"
role = "reviewer"
system_prompt = "Project reviewer"
model = "claude-sonnet"
"#,
        )
        .unwrap();
        // Project-only agent
        std::fs::write(
            project_root.join(".lim/agents/planner.toml"),
            r#"
role = "planner"
system_prompt = "Project planner"
"#,
        )
        .unwrap();

        let project = ProjectPaths::new(project_root).unwrap();
        let loader = ConfigLoader::new(xdg, project);

        let roles = loader.load_agent_roles(&AgentSource::Hybrid).unwrap();
        assert_eq!(roles.len(), 2);
        // Project overrides global for reviewer
        assert_eq!(roles["reviewer"].model.as_deref(), Some("claude-sonnet"));
        assert_eq!(roles["reviewer"].system_prompt.as_deref(), Some("Project reviewer"));
        // Planner only in project
        assert!(roles.contains_key("planner"));
    }
}
