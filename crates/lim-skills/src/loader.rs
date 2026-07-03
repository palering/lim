//! Skill loader — discovers and parses SKILL.md files.
//!
//! ## Search order
//!
//! Skills are loaded from multiple directories. Later directories override
//! earlier ones (by skill `name`):
//!
//! 1. Global skills (`~/.config/lim/skills/`)
//! 2. Project skills (`<project>/.lim/skills/`)
//!
//! Files must have a `.md` extension and contain YAML frontmatter delimited
//! by `---` lines. Files without frontmatter are skipped with a `warn!` log.
//!
//! ## Thread safety
//!
//! `SkillLoader` is stateless and can be shared across threads. The loaded
//! `SkillRegistry` is wrapped in `Arc` for cheap cloning.

use std::collections::HashMap;

use camino::{Utf8Path, Utf8PathBuf};
use tracing::{debug, warn};

use crate::error::{SkillError, SkillResult};
use crate::schema::{Skill, SkillFrontmatter};

/// Discovers and loads skills from disk.
#[derive(Debug, Clone)]
pub struct SkillLoader {
    /// Directories to search, in priority order (lowest first).
    /// Later directories override earlier ones.
    search_dirs: Vec<Utf8PathBuf>,
}

impl SkillLoader {
    /// Create a loader with the given search directories (lowest priority first).
    pub fn new(search_dirs: Vec<Utf8PathBuf>) -> Self {
        Self { search_dirs }
    }

    /// Create a loader for project-only skills (`.lim/skills/`).
    pub fn project_only(project_root: &Utf8Path) -> Self {
        Self::new(vec![project_root.join(".lim").join("skills")])
    }

    /// Create a loader with both global and project dirs (project overrides).
    pub fn with_global_and_project(global_dir: Utf8PathBuf, project_root: &Utf8Path) -> Self {
        Self::new(vec![global_dir, project_root.join(".lim").join("skills")])
    }

    /// Load all skills from the search directories.
    ///
    /// Missing directories are silently skipped. Parse errors are logged as
    /// warnings and the offending file is skipped (other files still load).
    pub fn load(&self) -> SkillResult<SkillRegistry> {
        let mut skills: HashMap<String, Skill> = HashMap::new();

        for dir in &self.search_dirs {
            if !dir.exists() {
                debug!(dir = %dir, "skills dir does not exist, skipping");
                continue;
            }
            debug!(dir = %dir, "loading skills");
            let entries = match std::fs::read_dir(dir.as_std_path()) {
                Ok(e) => e,
                Err(e) => {
                    warn!(dir = %dir, error = %e, "failed to read skills dir, skipping");
                    continue;
                }
            };
            for entry in entries.flatten() {
                let path = entry.path();
                // Only .md files are considered skills.
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                let path = match Utf8PathBuf::from_path_buf(path) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                match parse_skill_file(&path) {
                    Ok(skill) => {
                        debug!(name = %skill.name(), path = %path, "loaded skill");
                        skills.insert(skill.name().to_string(), skill);
                    }
                    Err(e) => {
                        warn!(path = %path, error = %e, "failed to parse skill file, skipping");
                    }
                }
            }
        }

        Ok(SkillRegistry { skills, search_dirs: self.search_dirs.clone() })
    }
}

/// Registry of loaded skills, indexed by name.
#[derive(Debug, Clone)]
pub struct SkillRegistry {
    skills: HashMap<String, Skill>,
    search_dirs: Vec<Utf8PathBuf>,
}

impl SkillRegistry {
    /// Look up a skill by name.
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    /// List all loaded skills (unordered).
    pub fn list(&self) -> Vec<&Skill> {
        self.skills.values().collect()
    }

    /// Number of loaded skills.
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    /// Is the registry empty?
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// The search directories used to load this registry.
    pub fn search_dirs(&self) -> &[Utf8PathBuf] {
        &self.search_dirs
    }
}

/// Parse a single SKILL.md file into a [`Skill`].
///
/// Expected format:
///
/// ```text
/// ---
/// name: skill-name
/// description: ...
/// verify: ...
/// ---
/// # Markdown body
/// ...
/// ```
///
/// The frontmatter is YAML between two `---` delimiter lines. The body is
/// everything after the closing `---`.
pub fn parse_skill_file(path: &Utf8Path) -> SkillResult<Skill> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| SkillError::Read { path: path.to_path_buf(), source: e })?;

    parse_skill_content(path, &content)
}

/// Parse skill content from a string (separated for testing).
fn parse_skill_content(path: &Utf8Path, content: &str) -> SkillResult<Skill> {
    let (frontmatter_str, body) = split_frontmatter(content)
        .ok_or_else(|| SkillError::NoFrontmatter { path: path.to_path_buf() })?;

    let frontmatter: SkillFrontmatter = serde_yaml::from_str(frontmatter_str)
        .map_err(|e| SkillError::FrontmatterParse { path: path.to_path_buf(), source: e })?;

    if frontmatter.name.trim().is_empty() {
        return Err(SkillError::EmptyName { path: path.to_path_buf() });
    }

    Ok(Skill {
        frontmatter,
        body: body.trim_start_matches('\n').to_string(),
        source: path.to_path_buf(),
    })
}

/// Split a markdown file into (frontmatter, body).
///
/// Returns `None` if the file doesn't start with a `---` frontmatter block.
fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let content = content.trim_start_matches('\u{feff}'); // strip BOM if present
    if !content.starts_with("---\n") && !content.starts_with("---\r\n") {
        return None;
    }
    // Skip the opening `---\n`.
    let after_opening = content.split_once('\n')?;
    // Find the closing `---` line.
    let (frontmatter, rest) = after_opening.1.split_once("\n---")?;
    // The rest should start right after `---` — skip the newline.
    let body = rest.trim_start_matches("---").trim_start_matches('\n');
    Some((frontmatter, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(dir: &std::path::Path, name: &str, content: &str) -> Utf8PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        Utf8PathBuf::from_path_buf(path).unwrap()
    }

    #[test]
    fn parse_skill_with_frontmatter_and_body() {
        let content = "---\nname: my-skill\ndescription: A test skill\nverify: npm test\n---\n# Steps\n1. Do thing\n2. Check result\n";
        let path = Utf8Path::new("/tmp/test.md");
        let skill = parse_skill_content(path, content).unwrap();
        assert_eq!(skill.name(), "my-skill");
        assert_eq!(skill.frontmatter.description.as_deref(), Some("A test skill"));
        assert_eq!(skill.verify_command(), Some("npm test"));
        assert!(skill.is_programmatic());
        assert!(skill.body.contains("# Steps"));
        assert!(skill.body.contains("Do thing"));
    }

    #[test]
    fn parse_skill_without_verify_is_non_programmatic() {
        let content =
            "---\nname: manual-review\ndescription: Manual review\n---\nReview the code by hand.\n";
        let path = Utf8Path::new("/tmp/test.md");
        let skill = parse_skill_content(path, content).unwrap();
        assert_eq!(skill.name(), "manual-review");
        assert!(!skill.is_programmatic());
        assert!(skill.verify_command().is_none());
        assert!(skill.body.contains("Review the code by hand"));
    }

    #[test]
    fn parse_skill_with_verify_args() {
        let content = "---\nname: test-args\nverify: cargo\nverify_args: [\"test\", \"--release\"]\n---\nbody\n";
        let path = Utf8Path::new("/tmp/test.md");
        let skill = parse_skill_content(path, content).unwrap();
        assert_eq!(skill.full_verify_command().unwrap(), "cargo test --release");
    }

    #[test]
    fn parse_skill_without_frontmatter_errors() {
        let content = "# Just markdown\nNo frontmatter here.\n";
        let path = Utf8Path::new("/tmp/test.md");
        let err = parse_skill_content(path, content).unwrap_err();
        assert!(matches!(err, SkillError::NoFrontmatter { .. }));
    }

    #[test]
    fn parse_skill_with_empty_name_errors() {
        let content = "---\nname: \"\"\n---\nbody\n";
        let path = Utf8Path::new("/tmp/test.md");
        let err = parse_skill_content(path, content).unwrap_err();
        assert!(matches!(err, SkillError::EmptyName { .. }));
    }

    #[test]
    fn parse_skill_with_invalid_yaml_errors() {
        let content = "---\nname: [unclosed\n---\nbody\n";
        let path = Utf8Path::new("/tmp/test.md");
        let err = parse_skill_content(path, content).unwrap_err();
        assert!(matches!(err, SkillError::FrontmatterParse { .. }));
    }

    #[test]
    fn loader_loads_from_project_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let project = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let skills_dir = project.join(".lim").join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        write_skill(
            skills_dir.as_std_path(),
            "verify-build.md",
            "---\nname: verify-build\nverify: cargo build\n---\nBuild the project.\n",
        );
        write_skill(
            skills_dir.as_std_path(),
            "manual-review.md",
            "---\nname: manual-review\n---\nReview by hand.\n",
        );

        let loader = SkillLoader::project_only(&project);
        let registry = loader.load().unwrap();
        assert_eq!(registry.len(), 2);
        assert!(registry.get("verify-build").is_some());
        assert!(registry.get("manual-review").is_some());
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn loader_project_overrides_global() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let global_dir = home.join(".config/lim/skills");
        let project_root = home.join("myproject");

        std::fs::create_dir_all(&global_dir).unwrap();
        write_skill(
            global_dir.as_std_path(),
            "shared.md",
            "---\nname: shared\nverify: echo global\n---\nGlobal version.\n",
        );
        write_skill(
            global_dir.as_std_path(),
            "global-only.md",
            "---\nname: global-only\n---\nOnly in global.\n",
        );

        let project_skills = project_root.join(".lim").join("skills");
        std::fs::create_dir_all(&project_skills).unwrap();
        write_skill(
            project_skills.as_std_path(),
            "shared.md",
            "---\nname: shared\nverify: echo project\n---\nProject version.\n",
        );

        let loader = SkillLoader::with_global_and_project(global_dir, &project_root);
        let registry = loader.load().unwrap();
        assert_eq!(registry.len(), 2);
        // Project version overrides global.
        let shared = registry.get("shared").unwrap();
        assert_eq!(shared.verify_command(), Some("echo project"));
        assert!(shared.body.contains("Project version"));
        // Global-only skill still loaded.
        assert!(registry.get("global-only").is_some());
    }

    #[test]
    fn loader_skips_non_md_files() {
        let tmp = tempfile::tempdir().unwrap();
        let project = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let skills_dir = project.join(".lim").join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        write_skill(skills_dir.as_std_path(), "valid.md", "---\nname: valid\n---\nbody\n");
        // Non-md files should be ignored.
        std::fs::write(skills_dir.join("README.txt"), "not a skill\n").unwrap();
        std::fs::write(skills_dir.join(".DS_Store"), "junk\n").unwrap();

        let loader = SkillLoader::project_only(&project);
        let registry = loader.load().unwrap();
        assert_eq!(registry.len(), 1);
        assert!(registry.get("valid").is_some());
    }

    #[test]
    fn loader_missing_dir_is_ok() {
        let loader = SkillLoader::new(vec![
            Utf8PathBuf::from("/nonexistent/path/1"),
            Utf8PathBuf::from("/nonexistent/path/2"),
        ]);
        let registry = loader.load().unwrap();
        assert!(registry.is_empty());
    }

    #[test]
    fn loader_skips_files_with_parse_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let project = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let skills_dir = project.join(".lim").join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        // Valid skill.
        write_skill(skills_dir.as_std_path(), "valid.md", "---\nname: valid\n---\nbody\n");
        // Invalid skill (no frontmatter).
        write_skill(skills_dir.as_std_path(), "invalid.md", "just markdown, no frontmatter\n");

        let loader = SkillLoader::project_only(&project);
        let registry = loader.load().unwrap();
        // Only the valid skill loaded.
        assert_eq!(registry.len(), 1);
        assert!(registry.get("valid").is_some());
    }
}
