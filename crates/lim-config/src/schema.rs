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

    /// Project context file loading configuration.
    ///
    /// Controls which markdown files (AGENTS.md, CLAUDE.md, etc.) are loaded
    /// into the system prompt's Project cache tier, and how they are
    /// discovered (current dir, walk up, subdirectories).
    ///
    /// When `None`, no context files are loaded (current behavior).
    /// When `Some(ContextFileConfig { enabled: false, .. })`, explicitly disabled.
    /// When `Some(ContextFileConfig { enabled: true, files: [], .. })`, uses defaults.
    #[serde(default)]
    pub context: Option<ContextFileConfig>,
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

/// Configuration for loading project context files (AGENTS.md, etc.) into
/// the system prompt.
///
/// This makes the set of context files and their discovery behavior fully
/// configurable — no file name is hard-coded. Users can customize which
/// files to load, where to find them, and how deep to search.
///
/// ## TOML example
///
/// ```toml
/// [context]
/// enabled = true
///
/// [[context.files]]
/// name = "AGENTS.md"
/// search = "workspace_root"
///
/// [[context.files]]
/// name = "CONVENTIONS.md"
/// search = "subdirs"
/// max_depth = 2
///
/// [[context.files]]
/// path = "docs/conventions/basics.md"
/// search = "fixed"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextFileConfig {
    /// Master switch. When `false`, no context files are loaded even if
    /// listed in `files`. Defaults to `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// The context files to load. If empty and `enabled = true`, uses
    /// the built-in default (`AGENTS.md` at workspace root).
    #[serde(default)]
    pub files: Vec<ContextFileEntry>,
}

fn default_true() -> bool {
    true
}

/// A single context file to load, with its discovery behavior.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextFileEntry {
    /// File name (e.g. `"AGENTS.md"`) or relative path (e.g. `"docs/conventions.md"`).
    ///
    /// - If `search = "fixed"`, this is treated as a path relative to the
    ///   workspace root.
    /// - If `search = "workspace_root"` or `"walk_up"`, this is treated as
    ///   a file name to search for.
    /// - If `search = "subdirs"`, this is the file name to find in subdirectories.
    ///
    /// Either `name` or `path` must be set.
    #[serde(default)]
    pub name: Option<String>,

    /// Explicit path relative to workspace root (alternative to `name`).
    /// When set, `search` is forced to `"fixed"`.
    #[serde(default)]
    pub path: Option<String>,

    /// Discovery strategy.
    #[serde(default)]
    pub search: ContextSearch,

    /// Max depth for `"subdirs"` search. `0` = workspace root only,
    /// `1` = root + immediate children, etc. Defaults to `1`.
    /// Ignored for other search modes.
    #[serde(default = "default_subdir_depth")]
    pub max_depth: usize,

    /// Optional label for this context segment (used in PromptLayout).
    /// If not set, the file name is used.
    #[serde(default)]
    pub label: Option<String>,
}

fn default_subdir_depth() -> usize {
    1
}

/// How to discover a context file.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextSearch {
    /// Look for the file at the workspace root only (default).
    #[default]
    WorkspaceRoot,

    /// Walk up from the workspace root toward the filesystem root,
    /// merging all matches found (closest-first). Useful for monorepos
    /// where the workspace is nested inside a parent project.
    WalkUp,

    /// Search subdirectories of the workspace root up to `max_depth`,
    /// merging all matches. Useful for loading multiple `CONVENTIONS.md`
    /// files from subprojects.
    Subdirs,

    /// Use the exact `path` field — no searching. The path is resolved
    /// relative to the workspace root.
    Fixed,
}

impl ContextFileEntry {
    /// Resolve the effective file name to search for.
    /// Returns `path` if set (forced to Fixed), otherwise `name`.
    pub fn effective_name(&self) -> Option<&str> {
        if let Some(ref p) = self.path { Some(p.as_str()) } else { self.name.as_deref() }
    }

    /// Effective search mode: if `path` is set, forces `Fixed`.
    pub fn effective_search(&self) -> ContextSearch {
        if self.path.is_some() { ContextSearch::Fixed } else { self.search.clone() }
    }
}

impl ContextFileConfig {
    /// Get the effective file list, falling back to defaults if empty.
    ///
    /// The default is a single entry: `AGENTS.md` at workspace root.
    pub fn effective_files(&self) -> Vec<&ContextFileEntry> {
        if self.files.is_empty() {
            // Default: AGENTS.md at workspace root. This is the only
            // place where "AGENTS.md" is referenced — it's a default,
            // not a hard-coded requirement. Users can override via config.
            // We return an empty vec here; the loader handles the default
            // separately to avoid allocating a static entry.
            Vec::new()
        } else {
            self.files.iter().collect()
        }
    }
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

    #[test]
    fn parse_context_config_default() {
        // No [context] section → context is None
        let toml = r#"
[providers.openai]
type = "openai-chat"
"#;
        let config: WorkspaceConfig = toml::from_str(toml).unwrap();
        assert!(config.context.is_none());
    }

    #[test]
    fn parse_context_config_empty_files() {
        // [context] with enabled=true but no files → uses default (AGENTS.md)
        let toml = r#"
[context]
enabled = true
"#;
        let config: WorkspaceConfig = toml::from_str(toml).unwrap();
        let ctx = config.context.expect("context should be set");
        assert!(ctx.enabled);
        assert!(ctx.files.is_empty(), "empty files means use default");
    }

    #[test]
    fn parse_context_config_multiple_files() {
        let toml = r#"
[context]
enabled = true

[[context.files]]
name = "AGENTS.md"
search = "workspace_root"

[[context.files]]
name = "CONVENTIONS.md"
search = "subdirs"
max_depth = 2

[[context.files]]
path = "docs/conventions/basics.md"
label = "basics"
"#;
        let config: WorkspaceConfig = toml::from_str(toml).unwrap();
        let ctx = config.context.expect("context should be set");
        assert!(ctx.enabled);
        assert_eq!(ctx.files.len(), 3);

        // First: AGENTS.md at workspace root
        assert_eq!(ctx.files[0].name.as_deref(), Some("AGENTS.md"));
        assert_eq!(ctx.files[0].search, ContextSearch::WorkspaceRoot);

        // Second: CONVENTIONS.md in subdirs, depth 2
        assert_eq!(ctx.files[1].name.as_deref(), Some("CONVENTIONS.md"));
        assert_eq!(ctx.files[1].search, ContextSearch::Subdirs);
        assert_eq!(ctx.files[1].max_depth, 2);

        // Third: fixed path with label
        assert_eq!(ctx.files[2].path.as_deref(), Some("docs/conventions/basics.md"));
        assert_eq!(ctx.files[2].label.as_deref(), Some("basics"));
        // path set forces Fixed search
        assert_eq!(ctx.files[2].effective_search(), ContextSearch::Fixed);
        assert_eq!(ctx.files[2].effective_name(), Some("docs/conventions/basics.md"));
    }

    #[test]
    fn parse_context_config_disabled() {
        let toml = r#"
[context]
enabled = false

[[context.files]]
name = "AGENTS.md"
"#;
        let config: WorkspaceConfig = toml::from_str(toml).unwrap();
        let ctx = config.context.expect("context should be set");
        assert!(!ctx.enabled);
    }

    #[test]
    fn parse_context_search_modes() {
        let toml = r#"
[[context.files]]
name = "a.md"
search = "walk_up"

[[context.files]]
name = "b.md"
search = "fixed"

[[context.files]]
name = "c.md"
search = "workspace_root"

[[context.files]]
name = "d.md"
search = "subdirs"
"#;
        let config: WorkspaceConfig = toml::from_str(toml).unwrap();
        let ctx = config.context.expect("context should be set");
        assert_eq!(ctx.files.len(), 4);
        assert_eq!(ctx.files[0].search, ContextSearch::WalkUp);
        assert_eq!(ctx.files[1].search, ContextSearch::Fixed);
        assert_eq!(ctx.files[2].search, ContextSearch::WorkspaceRoot);
        assert_eq!(ctx.files[3].search, ContextSearch::Subdirs);
    }
}
