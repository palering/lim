//! Skill schema — the parsed representation of a SKILL.md file.
//!
//! A skill is a human-editable document that encodes "how to verify a goal
//! is achieved". It has two parts:
//!
//! 1. **Frontmatter** (YAML): metadata + optional `verify` command
//! 2. **Body** (Markdown): human-readable verification steps
//!
//! ## Frontmatter fields
//!
//! | Field | Required | Description |
//! |---|---|---|
//! | `name` | yes | Unique skill identifier (e.g. `verify-frontend-change`) |
//! | `description` | no | One-line summary shown in listings |
//! | `verify` | no | Shell command to run for verification (e.g. `npm test`) |
//! | `verify_args` | no | Optional args appended to `verify` command |
//!
//! If `verify` is present, `SkillVerifier` delegates to `CommandVerifier`
//! (programmatic, deterministic). If absent, the markdown body is injected
//! into the LLM prompt for `LlmVerifier` (human-readable, fuzzy).
//!
//! ## Example
//!
//! ```markdown
//! ---
//! name: verify-frontend-change
//! description: Verify any UI change end-to-end before declaring it done.
//! verify: npm test
//! ---
//! # 前端改动验证流程
//! 1. 启动 dev server，打开改动页面
//! 2. 点击按钮、填表单，确认预期行为
//! 3. 检查浏览器 console：不能有新的 error 或 warning
//! ```
//!
//! ## Loading priority
//!
//! Skills are loaded from multiple directories, with later directories
//! overriding earlier ones (by skill `name`):
//!
//! 1. Bundled skills (shipped with lim, future)
//! 2. Global skills (`~/.config/lim/skills/`)
//! 3. Project skills (`.lim/skills/`)

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

/// Parsed frontmatter of a SKILL.md file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillFrontmatter {
    /// Unique skill identifier.
    pub name: String,
    /// One-line description.
    #[serde(default)]
    pub description: Option<String>,
    /// Shell command to run for verification.
    ///
    /// If present, `SkillVerifier` delegates to `CommandVerifier`.
    /// If absent, the markdown body is used as an LLM verification prompt.
    #[serde(default)]
    pub verify: Option<String>,
    /// Optional args appended to the `verify` command.
    #[serde(default)]
    pub verify_args: Option<Vec<String>>,
}

/// A fully parsed skill — frontmatter + markdown body + source path.
#[derive(Debug, Clone)]
pub struct Skill {
    /// Parsed frontmatter.
    pub frontmatter: SkillFrontmatter,
    /// The markdown body (everything after the frontmatter).
    pub body: String,
    /// Absolute path to the source file (for error messages / debugging).
    pub source: Utf8PathBuf,
}

impl Skill {
    /// The skill's unique name.
    pub fn name(&self) -> &str {
        &self.frontmatter.name
    }

    /// The verification command (if any).
    pub fn verify_command(&self) -> Option<&str> {
        self.frontmatter.verify.as_deref()
    }

    /// Whether this skill is programmatic (has a `verify` command).
    ///
    /// Programmatic skills delegate to `CommandVerifier`; non-programmatic
    /// skills use the markdown body as an LLM verification prompt.
    pub fn is_programmatic(&self) -> bool {
        self.frontmatter.verify.is_some()
    }

    /// The full verification command (verify + verify_args joined).
    ///
    /// Returns None if `verify` is not set.
    pub fn full_verify_command(&self) -> Option<String> {
        let verify = self.frontmatter.verify.as_ref()?;
        let mut cmd = verify.clone();
        if let Some(args) = &self.frontmatter.verify_args {
            for arg in args {
                cmd.push(' ');
                cmd.push_str(arg);
            }
        }
        Some(cmd)
    }
}
