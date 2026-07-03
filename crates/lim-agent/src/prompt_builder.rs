//! Prompt builder — assembles cache-friendly [`PromptLayout`] for LLM requests.
//!
//! This is the single place where the prompt structure is decided. It pulls
//! static fragments (role, environment, shell guidance), project context,
//! and dynamic runtime info into a [`PromptLayout`] with correct cache tier
//! annotations. The provider layer consumes the layout to place cache
//! breakpoints; this module is provider-agnostic.
//!
//! ## Cache-friendly design (Claude Code铁律)
//!
//! 1. Static content first (role + env + shell guidance) — long cache lifetime
//! 2. Project context second (AGENTS.md, skills index) — medium cache lifetime
//! 3. Session history via messages — incremental cache
//! 4. Dynamic info last (timestamp, runtime flags) — never cached
//!
//! Dynamic info (timestamp, current cwd) is NOT placed in the system prompt.
//! It goes into a `<system-reminder>` block appended to the latest user
//! message, so the system prompt prefix stays stable for caching.
//!
//! ## What goes where
//!
//! | Source | Tier | Location | Cache |
//! |--------|------|----------|-------|
//! | Role definition | Static | system prompt | yes |
//! | Environment (OS/Arch/Shell) | Static | system prompt | yes |
//! | Shell guidance | Static | system prompt | yes |
//! | Custom static system prompt | Static | system prompt | yes |
//! | Project AGENTS.md (future) | Project | system prompt | yes |
//! | Skills index (future) | Project | system prompt | yes |
//! | Conversation history | Session | messages field | yes |
//! | Timestamp | Dynamic | system-reminder in message | no |
//! | Current cwd | Dynamic | system-reminder in message | no |
//! | Trust level | Dynamic | system-reminder in message | no |

use lim_core::inject::InjectInfo;
use lim_core::prompt::{PromptLayout, PromptLayoutBuilder};

/// Default role definition text for the lim agent.
///
/// This is the static foundation of the system prompt — it defines what
/// the agent *is* before any environment or project context is added.
/// Kept intentionally minimal; project-specific instructions go in
/// Project-tier segments.
const ROLE_DEFINITION: &str = "\
You are an interactive agent operating inside lim, a Rust-native agent runtime. \
You help users by reading files, executing commands, editing code, and writing new files. \
You have access to tools listed below; use them to accomplish tasks step by step.\n\n\
## Guidelines\n\
- Be concise and direct. Lead with the answer, not the reasoning.\n\
- When using tools, explain briefly what you're doing and why.\n\
- If a task is ambiguous, ask for clarification before proceeding.\n\
- If an approach fails, consider alternatives rather than retrying blindly.";

/// Assembles [`PromptLayout`] for LLM requests.
///
/// Constructed once at agent startup (or per-session) with the static
/// environment info. The [`Self::build`] method produces a fresh layout
/// for each turn, incorporating any dynamic info.
///
/// ## Example
///
/// ```ignore
/// let builder = PromptBuilder::new(inject_info)
///     .with_custom_static("Additional role instructions here");
/// let layout = builder.build();
/// let request = LlmRequest::new(model, messages)
///     .with_tools(tools)
///     .with_prompt_layout(layout);
/// ```
#[derive(Clone)]
pub struct PromptBuilder {
    /// Static environment info (OS, Arch, Shell, workspace root).
    inject_info: InjectInfo,
    /// Optional custom static system prompt (e.g. from AgentConfig).
    custom_static: Option<String>,
    /// Optional project-level context (e.g. AGENTS.md content).
    project_context: Option<String>,
}

impl PromptBuilder {
    /// Create a new prompt builder with the given environment info.
    pub fn new(inject_info: InjectInfo) -> Self {
        Self { inject_info, custom_static: None, project_context: None }
    }

    /// Add a custom static system prompt segment.
    ///
    /// This is appended to the role definition and environment info in the
    /// Static tier. Used by `AgentConfig::system_prompt` and sub-agent role
    /// prompts.
    pub fn with_custom_static(mut self, prompt: impl Into<String>) -> Self {
        self.custom_static = Some(prompt.into());
        self
    }

    /// Add project-level context (e.g. AGENTS.md, README summary).
    ///
    /// Placed in the Project tier — gets a cache breakpoint, but invalidated
    /// when the project config changes.
    pub fn with_project_context(mut self, context: impl Into<String>) -> Self {
        self.project_context = Some(context.into());
        self
    }

    /// Build the prompt layout for a single LLM request.
    ///
    /// The layout is sorted by cache tier (Static → Project → Dynamic).
    /// Session-tier (conversation history) is NOT included here — it goes
    /// into the `messages` field of the request, not the system prompt.
    ///
    /// Dynamic info (timestamp, cwd) is intentionally NOT included in the
    /// system prompt. It should be injected as a `<system-reminder>` block
    /// in the latest user message by the caller, to keep the system prompt
    /// prefix stable for caching.
    pub fn build(&self) -> PromptLayout {
        use lim_core::prompt::{CacheTier, PromptSegment};

        let mut builder = PromptLayoutBuilder::default();

        // --- Static tier (long cache lifetime) ---
        builder = builder
            .segment(PromptSegment::new(CacheTier::Static, ROLE_DEFINITION).with_label("role"));

        // Environment info (OS, Arch, Shell, workspace root, shell guidance).
        // This is static for the process lifetime.
        let env_fragment = self.inject_info.system_prompt_fragment();
        if !env_fragment.is_empty() {
            builder = builder.segment(
                PromptSegment::new(CacheTier::Static, env_fragment).with_label("environment"),
            );
        }

        // Custom static prompt (from AgentConfig or sub-agent role).
        if let Some(ref custom) = self.custom_static
            && !custom.is_empty()
        {
            builder =
                builder.segment(PromptSegment::new(CacheTier::Static, custom).with_label("custom"));
        }

        // --- Project tier (medium cache lifetime) ---
        if let Some(ref ctx) = self.project_context
            && !ctx.is_empty()
        {
            builder = builder
                .segment(PromptSegment::new(CacheTier::Project, ctx).with_label("project_context"));
        }

        builder.build()
    }
}

/// Build a `<system-reminder>` block for dynamic runtime info.
///
/// This is meant to be appended to the latest user message (not the system
/// prompt) so that the system prompt prefix stays stable for caching.
///
/// ## Fields
///
/// - `timestamp`: current ISO-8601 time (changes every turn → must not
///   be in system prompt)
/// - `cwd`: current working directory (may change between turns if the
///   agent `cd`s, but rarely)
/// - `trust_level`: current trust level (workspace/permissive)
///
/// ## Format
///
/// ```text
/// <system-reminder>
/// Current time: 2026-07-03T12:34:56Z
/// Working directory: /path/to/workspace
/// Trust level: workspace
/// </system-reminder>
/// ```
pub fn build_system_reminder(timestamp: &str, cwd: &str, trust_level: &str) -> String {
    format!(
        "<system-reminder>\n\
         Current time: {timestamp}\n\
         Working directory: {cwd}\n\
         Trust level: {trust_level}\n\
         </system-reminder>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use lim_core::env::{ArchKind, OsKind, PlatformInfo, ShellKind};

    fn make_platform() -> PlatformInfo {
        PlatformInfo {
            os: OsKind::Macos,
            arch: ArchKind::Aarch64,
            shell: ShellKind::PosixSh,
            workspace_roots: vec![Utf8PathBuf::from("/workspace")],
        }
    }

    #[test]
    fn builder_produces_static_segments() {
        let builder = PromptBuilder::new(InjectInfo::new(make_platform()));
        let layout = builder.build();
        assert!(!layout.segments.is_empty());
        // All segments should be Static or Project tier (no Session, no Dynamic)
        for seg in &layout.segments {
            assert!(
                matches!(
                    seg.tier,
                    lim_core::prompt::CacheTier::Static | lim_core::prompt::CacheTier::Project
                ),
                "unexpected tier {:?}",
                seg.tier
            );
        }
    }

    #[test]
    fn includes_role_and_environment() {
        let builder = PromptBuilder::new(InjectInfo::new(make_platform()));
        let layout = builder.build();
        let text = layout.system_prompt_text();
        assert!(text.contains("interactive agent operating inside lim"));
        assert!(text.contains("## Environment"));
        assert!(text.contains("OS: macos"));
    }

    #[test]
    fn custom_static_appended() {
        let builder = PromptBuilder::new(InjectInfo::new(make_platform()))
            .with_custom_static("You are a code reviewer.");
        let layout = builder.build();
        let text = layout.system_prompt_text();
        assert!(text.contains("code reviewer"));
    }

    #[test]
    fn project_context_in_project_tier() {
        let builder = PromptBuilder::new(InjectInfo::new(make_platform()))
            .with_project_context("# Project: lim\nA Rust agent runtime.");
        let layout = builder.build();
        assert!(layout.segments.iter().any(|s| {
            s.tier == lim_core::prompt::CacheTier::Project && s.content.contains("Project: lim")
        }));
    }

    #[test]
    fn no_dynamic_segments_in_layout() {
        // Dynamic info should NOT be in the layout — it goes into messages.
        let builder = PromptBuilder::new(InjectInfo::new(make_platform()));
        let layout = builder.build();
        assert!(layout.segments.iter().all(|s| s.tier != lim_core::prompt::CacheTier::Dynamic));
    }

    #[test]
    fn cache_breakpoints_present() {
        let builder = PromptBuilder::new(InjectInfo::new(make_platform()))
            .with_project_context("project context");
        let layout = builder.build();
        let bps = layout.cache_breakpoints();
        // At least one breakpoint (after Static tier)
        assert!(!bps.is_empty());
    }

    #[test]
    fn system_reminder_format() {
        let reminder = build_system_reminder("2026-07-03T12:00:00Z", "/workspace", "workspace");
        assert!(reminder.contains("<system-reminder>"));
        assert!(reminder.contains("Current time: 2026-07-03T12:00:00Z"));
        assert!(reminder.contains("Working directory: /workspace"));
        assert!(reminder.contains("Trust level: workspace"));
        assert!(reminder.contains("</system-reminder>"));
    }
}
