//! Prompt segment abstraction — structured, cache-friendly prompt composition.
//!
//! The prompt sent to an LLM is not a flat string. It has natural layers
//! (static role definition, tool schemas, project context, session history,
//! dynamic runtime info) with very different cache lifetimes. Prompt caching
//! is prefix-match: any change to a prefix invalidates all caches downstream.
//! Therefore the order of segments and the placement of cache breakpoints
//! directly determines cost.
//!
//! This module provides the data shapes only — [`PromptSegment`] describes
//! a single segment, and [`PromptLayout`] describes the ordered list with
//! cache breakpoint annotations. The actual *content* of each segment is
//! produced by higher layers (InjectInfo, ResourceLoader, ContextWindowManager,
//! etc.); this module is content-agnostic.
//!
//! ## Cache breakpoint semantics
//!
//! A cache breakpoint marks "the prefix up to this point may be cached".
//! Providers that support explicit breakpoints (Anthropic `cache_control`)
//! will place a marker at the end of each segment where `cache_breakpoint`
//! is true. Providers that rely on implicit prefix caching (OpenAI) simply
//! benefit from the stable ordering — no explicit marker needed.
//!
//! The golden rule (from Claude Code's design):
//! - **Static content first** (long cache lifetime)
//! - **Dynamic content last** (no cache, or very short cache)
//! - **Never modify a prefix** — any change invalidates everything downstream
//!
//! ## Layering convention
//!
//! lim uses a 4-tier cache lifetime convention:
//!
//! | Tier | Lifetime | Example | Cache breakpoint |
//! |------|----------|---------|------------------|
//! | Static | process-wide, never changes | role, tools, global AGENTS.md | yes |
//! | Project | per-project, changes rarely | project context, skills index | yes |
//! | Session | per-session, grows incrementally | conversation history | yes |
//! | Dynamic | per-turn, never cached | timestamp, current cwd, trust level | no |
//!
//! Higher layers should respect this convention when building segments.

use serde::{Deserialize, Serialize};

/// Cache lifetime tier for a prompt segment.
///
/// Determines ordering (static first, dynamic last) and whether a cache
/// breakpoint is placed at the end of the segment. See module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheTier {
    /// Process-wide static content — never changes within a process lifetime.
    /// Examples: role definition, tool schemas, global AGENTS.md.
    /// Always gets a cache breakpoint.
    Static,
    /// Project-level content — changes rarely, only when project config changes.
    /// Examples: project AGENTS.md, README summary, skills index.
    /// Gets a cache breakpoint.
    Project,
    /// Session-level content — grows incrementally within a session.
    /// Example: conversation history (AgentMessage list).
    /// Gets a cache breakpoint at the end of the history prefix.
    Session,
    /// Per-turn dynamic content — never cached.
    /// Examples: timestamp, current cwd, trust level, runtime flags.
    /// No cache breakpoint.
    Dynamic,
}

impl CacheTier {
    /// Whether a cache breakpoint should be placed at the end of a segment
    /// with this tier.
    pub fn has_breakpoint(self) -> bool {
        !matches!(self, CacheTier::Dynamic)
    }
}

/// A single segment of the prompt sent to the LLM.
///
/// Segments are ordered by [`CacheTier`] (static → project → session → dynamic)
/// to maximize prefix cache hits. Each segment carries its content as a string
/// and an optional cache breakpoint annotation.
///
/// The content is always a string; structured data (tool schemas, message
/// history) is converted to its provider-agnostic representation by the
/// higher layer before being placed in a segment. The provider layer is
/// responsible for the final provider-specific encoding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptSegment {
    /// Cache lifetime tier — determines ordering and breakpoint placement.
    pub tier: CacheTier,
    /// The content of this segment (system prompt text, context fragment, etc.).
    pub content: String,
    /// Optional label for debugging/observability (e.g. "role", "tools",
    /// "project_context", "skills_index"). Not sent to the LLM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl PromptSegment {
    /// Create a new segment with the given tier and content.
    pub fn new(tier: CacheTier, content: impl Into<String>) -> Self {
        Self { tier, content: content.into(), label: None }
    }

    /// Attach a debug label.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Whether a cache breakpoint should be placed after this segment.
    pub fn has_breakpoint(&self) -> bool {
        self.tier.has_breakpoint()
    }
}

/// Ordered layout of prompt segments for a single LLM request.
///
/// Segments are always sorted by tier (static → project → session → dynamic)
/// at construction time. The provider layer consumes this to build the final
/// request body, placing cache breakpoints at segment boundaries marked by
/// `has_breakpoint()`.
///
/// ## Construction
///
/// Use [`PromptLayout::builder()`] to construct a layout. Segments can be
/// added in any order; they will be sorted by tier when finalized.
///
/// ## System prompt assembly
///
/// Segments with tier Static/Project/Dynamic that have string content are
/// concatenated (in tier order) to form the system prompt. Session-tier
/// segments are NOT part of the system prompt — they represent the
/// conversation history, which goes into the `messages` field of the request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PromptLayout {
    /// Ordered segments (sorted by tier at construction time).
    pub segments: Vec<PromptSegment>,
}

impl PromptLayout {
    /// Create an empty layout.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get a builder for constructing a layout.
    pub fn builder() -> PromptLayoutBuilder {
        PromptLayoutBuilder::default()
    }

    /// Returns the concatenated system prompt text (Static + Project + Dynamic
    /// tier segments, in that order).
    ///
    /// Session-tier segments are excluded — they represent conversation
    /// history, not system prompt content.
    pub fn system_prompt_text(&self) -> String {
        let mut s = String::new();
        for seg in &self.segments {
            if !matches!(seg.tier, CacheTier::Session) {
                if !s.is_empty() {
                    s.push_str("\n\n");
                }
                s.push_str(&seg.content);
            }
        }
        s
    }

    /// Returns the cache breakpoints as the indices of segments after which
    /// a cache breakpoint should be placed.
    ///
    /// A breakpoint is placed after the last segment of each consecutive
    /// tier group that `has_breakpoint()`. For example, if there are 3 Static
    /// segments followed by 2 Project segments, breakpoints are placed after
    /// index 2 (last Static) and index 4 (last Project).
    pub fn cache_breakpoints(&self) -> Vec<usize> {
        let mut breakpoints = Vec::new();
        let mut prev_breakable = false;
        for (i, seg) in self.segments.iter().enumerate() {
            let breakable = seg.has_breakpoint();
            // Place a breakpoint at the end of a tier group: when transitioning
            // from a breakable tier to a different tier, or at the last breakable
            // segment overall.
            let is_last = i == self.segments.len() - 1;
            let next_different = if is_last { true } else { self.segments[i + 1].tier != seg.tier };
            if breakable && next_different {
                breakpoints.push(i);
            }
            prev_breakable = breakable;
        }
        let _ = prev_breakable; // suppress unused warning
        breakpoints
    }
}

/// Builder for [`PromptLayout`].
///
/// Segments can be added in any order; they are sorted by tier (and insertion
/// order within a tier) when [`build()`](Self::build) is called.
#[derive(Debug, Default)]
pub struct PromptLayoutBuilder {
    segments: Vec<PromptSegment>,
}

impl PromptLayoutBuilder {
    /// Add a static-tier segment (role, tools, global config).
    pub fn static_segment(mut self, content: impl Into<String>) -> Self {
        self.segments.push(PromptSegment::new(CacheTier::Static, content));
        self
    }

    /// Add a project-tier segment (project context, skills index).
    pub fn project_segment(mut self, content: impl Into<String>) -> Self {
        self.segments.push(PromptSegment::new(CacheTier::Project, content));
        self
    }

    /// Add a session-tier segment (conversation history placeholder).
    pub fn session_segment(mut self, content: impl Into<String>) -> Self {
        self.segments.push(PromptSegment::new(CacheTier::Session, content));
        self
    }

    /// Add a dynamic-tier segment (timestamp, runtime flags).
    pub fn dynamic_segment(mut self, content: impl Into<String>) -> Self {
        self.segments.push(PromptSegment::new(CacheTier::Dynamic, content));
        self
    }

    /// Add a labeled segment.
    pub fn segment(mut self, seg: PromptSegment) -> Self {
        self.segments.push(seg);
        self
    }

    /// Build the final layout, sorting segments by tier.
    ///
    /// Within the same tier, segments preserve insertion order.
    pub fn build(mut self) -> PromptLayout {
        // Stable sort by tier rank — preserves insertion order within a tier.
        self.segments.sort_by_key(|s| tier_rank(s.tier));
        PromptLayout { segments: self.segments }
    }
}

/// Sort order for tiers: Static < Project < Session < Dynamic.
fn tier_rank(tier: CacheTier) -> u8 {
    match tier {
        CacheTier::Static => 0,
        CacheTier::Project => 1,
        CacheTier::Session => 2,
        CacheTier::Dynamic => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_tier_breakpoint_semantics() {
        assert!(CacheTier::Static.has_breakpoint());
        assert!(CacheTier::Project.has_breakpoint());
        assert!(CacheTier::Session.has_breakpoint());
        assert!(!CacheTier::Dynamic.has_breakpoint());
    }

    #[test]
    fn builder_sorts_by_tier() {
        let layout = PromptLayout::builder()
            .dynamic_segment("dynamic")
            .static_segment("static1")
            .project_segment("project")
            .static_segment("static2")
            .build();
        assert_eq!(layout.segments.len(), 4);
        assert_eq!(layout.segments[0].tier, CacheTier::Static);
        assert_eq!(layout.segments[0].content, "static1"); // insertion order preserved
        assert_eq!(layout.segments[1].tier, CacheTier::Static);
        assert_eq!(layout.segments[1].content, "static2");
        assert_eq!(layout.segments[2].tier, CacheTier::Project);
        assert_eq!(layout.segments[3].tier, CacheTier::Dynamic);
    }

    #[test]
    fn system_prompt_text_excludes_session_tier() {
        let layout = PromptLayout::builder()
            .static_segment("role")
            .project_segment("context")
            .session_segment("history")
            .dynamic_segment("timestamp")
            .build();
        let text = layout.system_prompt_text();
        assert!(text.contains("role"));
        assert!(text.contains("context"));
        assert!(text.contains("timestamp"));
        assert!(!text.contains("history"));
    }

    #[test]
    fn cache_breakpoints_at_tier_boundaries() {
        let layout = PromptLayout::builder()
            .static_segment("s1")
            .static_segment("s2")
            .project_segment("p1")
            .session_segment("h1")
            .dynamic_segment("d1")
            .build();
        // Breakpoints after: s2 (idx 1), p1 (idx 2), h1 (idx 3)
        // No breakpoint after d1 (Dynamic tier)
        let bps = layout.cache_breakpoints();
        assert_eq!(bps, vec![1, 2, 3]);
    }

    #[test]
    fn cache_breakpoints_skip_dynamic() {
        let layout = PromptLayout::builder()
            .static_segment("s1")
            .dynamic_segment("d1")
            .dynamic_segment("d2")
            .build();
        // Only one breakpoint after s1 (idx 0)
        let bps = layout.cache_breakpoints();
        assert_eq!(bps, vec![0]);
    }

    #[test]
    fn empty_layout() {
        let layout = PromptLayout::new();
        assert!(layout.segments.is_empty());
        assert!(layout.system_prompt_text().is_empty());
        assert!(layout.cache_breakpoints().is_empty());
    }

    #[test]
    fn segment_with_label() {
        let seg = PromptSegment::new(CacheTier::Static, "content").with_label("role");
        assert_eq!(seg.label.as_deref(), Some("role"));
        assert!(seg.has_breakpoint());
    }
}
