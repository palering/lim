//! Configuration schema for lim.
//!
//! The schema is intentionally minimal — it only contains fields that need
//! cross-layer merging. Provider configurations stay in `lim-llm::providers`
//! (which has its own `LimConfig` with providers map); this module wraps and
//! extends it with workspace/agent/worktree settings.

use std::collections::HashMap;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

/// Workspace-level configuration, mergeable across global/project/CLI layers.
///
/// Fields are all `Option<>` to support layered merging: a lower-priority
/// layer provides defaults, a higher-priority layer overrides them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    /// Provider configurations (merged from `[providers.*]` sections).
    ///
    /// This is the same shape as `lim_llm::providers::LimConfig::providers`,
    /// but we re-define it here to avoid a hard dependency on lim-llm
    /// (lim-config should be a low-level crate).
    /// The CLI layer translates this into `LimConfig` when constructing providers.
    #[serde(default)]
    pub providers: HashMap<String, ProviderEntry>,

    /// Name of the default provider.
    #[serde(default)]
    pub default_provider: Option<String>,

    /// Workspace isolation mode for multi-agent.
    #[serde(default)]
    pub workspace: WorkspaceMode,

    /// Agent configuration source (project vs global vs hybrid).
    #[serde(default)]
    pub agents: AgentSource,
}

/// Workspace isolation mode for multi-agent execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceMode {
    /// All agents share the current workspace (default, current behavior).
    #[default]
    Shared,

    /// Each agent gets a git worktree under `.lim/worktrees/`.
    Worktree,

    /// Each agent has an independent workspace directory (OpenClaw mode).
    Isolated,
}

/// Agent configuration source.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentSource {
    /// Load agent configs from `.lim/agents/` (default).
    #[default]
    Project,

    /// Load agent configs from `~/.config/lim/agents/`.
    Global,

    /// Project configs override global (like skills priority).
    Hybrid,
}

/// Minimal provider entry — the real provider construction happens in lim-llm.
///
/// We keep this here so `lim-config` doesn't depend on `lim-llm`. The CLI
/// layer converts this to `lim_llm::providers::ProviderInstanceConfig`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderEntry {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub full_url: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub models: Vec<String>,
}

/// Agent role configuration — defines a sub-agent's persona and capabilities.
///
/// Loaded from `<role>.toml` files in `.lim/agents/` or `~/.config/lim/agents/`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentRoleConfig {
    /// Role name (e.g. "reviewer", "planner", "coder").
    pub role: String,

    /// System prompt for this role.
    #[serde(default)]
    pub system_prompt: Option<String>,

    /// Model override (if None, uses the workspace default).
    #[serde(default)]
    pub model: Option<String>,

    /// Provider override (if None, uses the workspace default).
    #[serde(default)]
    pub provider: Option<String>,

    /// Tool whitelist (if empty, all tools available).
    #[serde(default)]
    pub tools_whitelist: Vec<String>,

    /// Max turns for this role's sessions.
    #[serde(default)]
    pub max_turns: Option<usize>,

    /// Workspace path override (for Isolated mode).
    ///
    /// If set, this role runs in a specific workspace directory instead of
    /// the current project. Used for OpenClaw-style global agents.
    #[serde(default)]
    pub workspace_path: Option<Utf8PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_config() {
        let toml = r#"
[providers.openai]
type = "openai-chat"
api_key = "sk-test"
default_model = "gpt-4o"
"#;
        let config: WorkspaceConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.providers.len(), 1);
        assert!(config.default_provider.is_none());
        assert_eq!(config.workspace, WorkspaceMode::Shared);
        assert_eq!(config.agents, AgentSource::Project);
    }

    #[test]
    fn parse_full_config() {
        let toml = r#"
default_provider = "openai"
workspace = "worktree"
agents = "hybrid"

[providers.openai]
type = "openai-chat"
api_key_env = "OPENAI_API_KEY"
default_model = "gpt-4o"
"#;
        let config: WorkspaceConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.default_provider.as_deref(), Some("openai"));
        assert_eq!(config.workspace, WorkspaceMode::Worktree);
        assert_eq!(config.agents, AgentSource::Hybrid);
    }

    #[test]
    fn parse_agent_role_config() {
        let toml = r#"
role = "reviewer"
system_prompt = "You are an independent code reviewer."
model = "claude-sonnet"
tools_whitelist = ["read", "shell"]
max_turns = 10
"#;
        let role: AgentRoleConfig = toml::from_str(toml).unwrap();
        assert_eq!(role.role, "reviewer");
        assert!(role.system_prompt.as_deref().unwrap().contains("reviewer"));
        assert_eq!(role.model.as_deref(), Some("claude-sonnet"));
        assert_eq!(role.tools_whitelist, vec!["read", "shell"]);
        assert_eq!(role.max_turns, Some(10));
    }
}
