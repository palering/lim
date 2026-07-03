//! lim-skills — human-editable verification skills for lim agents.
//!
//! A skill is a markdown file (`SKILL.md` or `<name>.md`) with YAML
//! frontmatter that encodes "how to verify a goal is achieved". Skills
//! bridge the gap between programmatic verifiers (like `CommandVerifier`,
//! which developers write) and the fuzzy, human-readable verification steps
//! that users want to express.
//!
//! ## Architecture
//!
//! This crate is **loading-only**: it discovers, parses, and indexes skill
//! files. It does not implement the `Verifier` trait — that lives in
//! `lim-agent` (which depends on this crate) as `SkillVerifier`.
//!
//! ```text
//! .lim/skills/*.md  ─┐
//!                    ├─▶ SkillLoader ─▶ SkillRegistry ─▶ SkillVerifier (in lim-agent)
//! ~/.config/lim/skills/*.md ─┘
//! ```
//!
//! ## Skill file format
//!
//! ```markdown
//! ---
//! name: verify-frontend-change
//! description: Verify any UI change end-to-end.
//! verify: npm test
//! ---
//! # Verification steps
//! 1. Start dev server
//! 2. Click through the UI
//! 3. Check browser console
//! ```
//!
//! See [`schema::SkillFrontmatter`] for all frontmatter fields.
//!
//! ## Layering
//!
//! - `lim-skills` (this crate): schema + loader, no dependency on lim-agent
//! - `lim-agent`: `SkillVerifier` impl, depends on this crate
//!
//! This keeps the skill format decoupled from the verifier runtime, so
//! future tools (e.g. a TUI skill browser) can use this crate directly.

pub mod error;
pub mod loader;
pub mod schema;

pub use error::{SkillError, SkillResult};
pub use loader::{SkillLoader, SkillRegistry, parse_skill_file};
pub use schema::{Skill, SkillFrontmatter};
