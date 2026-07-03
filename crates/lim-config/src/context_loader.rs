//! Context file loader — discovers and reads project context files
//! (AGENTS.md, CONVENTIONS.md, etc.) based on [`ContextFileConfig`].
//!
//! This module is the bridge between configuration and prompt assembly:
//! it takes a [`ContextFileConfig`] and a workspace root, then produces
//! a list of `(label, content)` pairs ready to be fed into
//! `PromptBuilder::with_project_context`.
//!
//! ## Design principles
//!
//! - **No hard-coded file names.** The default (`AGENTS.md`) is only used
//!   when `files` is empty AND `enabled = true`. Users can override this
//!   completely via config.
//! - **Non-fatal.** Missing files or permission errors are logged at
//!   `warn!` and skipped — one unreadable file should not crash the agent.
//! - **Order-preserving.** Files are returned in the order they appear in
//!   the config, so users can control the prompt segment ordering.
//! - **Cache-friendly.** Each file becomes a separate segment with its own
//!   label, so the `PromptBuilder` can place cache breakpoints at tier
//!   boundaries (Project tier).

use camino::Utf8PathBuf;
use tracing::{debug, warn};

use crate::paths::ProjectPaths;
use crate::schema::{ContextFileConfig, ContextFileEntry, ContextSearch};

/// A loaded context segment, ready to be injected into the prompt.
#[derive(Debug, Clone)]
pub struct LoadedContext {
    /// Label for this segment (file name or configured label).
    pub label: String,
    /// Content of the file.
    pub content: String,
    /// Source path (for debugging / observability).
    pub source: Utf8PathBuf,
}

/// Default file name when `files` is empty.
///
/// This is the **only** place where "AGENTS.md" appears as a literal in
/// the codebase. Users can override this entirely via `[context.files]`.
const DEFAULT_CONTEXT_FILE: &str = "AGENTS.md";

/// Load context files according to the given configuration.
///
/// Returns a list of `LoadedContext` segments in the order they should
/// appear in the prompt. Missing/unreadable files are skipped with a
/// `warn!` log.
///
/// When `config` is `None` or `enabled = false`, returns an empty vec.
/// When `files` is empty and `enabled = true`, uses the default
/// (`AGENTS.md` at workspace root).
pub fn load_context_files(
    config: Option<&ContextFileConfig>,
    paths: &ProjectPaths,
) -> Vec<LoadedContext> {
    let Some(config) = config else {
        debug!("no context config, skipping context file loading");
        return Vec::new();
    };

    if !config.enabled {
        debug!("context loading disabled via config");
        return Vec::new();
    }

    let workspace_root = &paths.workspace_root;

    if config.files.is_empty() {
        // Default: AGENTS.md at workspace root
        let entry = ContextFileEntry {
            name: Some(DEFAULT_CONTEXT_FILE.to_string()),
            search: ContextSearch::WorkspaceRoot,
            ..Default::default()
        };
        return load_single_entry(&entry, workspace_root);
    }

    let mut loaded = Vec::new();
    for entry in &config.files {
        let segments = load_single_entry(entry, workspace_root);
        loaded.extend(segments);
    }
    loaded
}

/// Load a single context file entry, applying its search strategy.
fn load_single_entry(entry: &ContextFileEntry, workspace_root: &Utf8PathBuf) -> Vec<LoadedContext> {
    let name = match entry.effective_name() {
        Some(n) => n,
        None => {
            warn!("context file entry has neither name nor path, skipping");
            return Vec::new();
        }
    };

    let search = entry.effective_search();
    let label =
        entry.label.clone().unwrap_or_else(|| name.rsplit('/').next().unwrap_or(name).to_string());

    let found_paths = match search {
        ContextSearch::Fixed => {
            // path relative to workspace root
            let p = workspace_root.join(name);
            if p.exists() { vec![p] } else { vec![] }
        }
        ContextSearch::WorkspaceRoot => {
            let p = workspace_root.join(name);
            if p.exists() { vec![p] } else { vec![] }
        }
        ContextSearch::WalkUp => find_walk_up(workspace_root, name),
        ContextSearch::Subdirs => find_in_subdirs(workspace_root, name, entry.max_depth),
    };

    if found_paths.is_empty() {
        debug!(name = name, search = ?search, "context file not found, skipping");
        return Vec::new();
    }

    let mut segments = Vec::new();
    for path in found_paths {
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                debug!(path = %path, label = %label, chars = content.len(), "loaded context file");
                segments.push(LoadedContext { label: label.clone(), content, source: path });
            }
            Err(e) => {
                warn!(path = %path, error = %e, "failed to read context file, skipping");
            }
        }
    }
    segments
}

/// Walk up from `start` toward the filesystem root, collecting all
/// directories that contain a file named `file_name`.
///
/// Returns matches **closest-first** (start dir, then parent, etc.).
/// Stops at the filesystem root.
fn find_walk_up(start: &Utf8PathBuf, file_name: &str) -> Vec<Utf8PathBuf> {
    use camino::Utf8Path;
    let mut found = Vec::new();
    let mut current: Option<&Utf8Path> = Some(start.as_path());
    while let Some(dir) = current {
        let candidate = dir.join(file_name);
        if candidate.exists() {
            found.push(candidate);
        }
        current = dir.parent();
    }
    found
}

/// Search subdirectories of `root` up to `max_depth` for files named
/// `file_name`.
///
/// `max_depth = 0` means root only. `max_depth = 1` means root + immediate
/// children. Files are returned in directory-traversal order (root first,
/// then children alphabetically).
fn find_in_subdirs(root: &Utf8PathBuf, file_name: &str, max_depth: usize) -> Vec<Utf8PathBuf> {
    let mut found = Vec::new();

    // Root first
    let root_candidate = root.join(file_name);
    if root_candidate.exists() {
        found.push(root_candidate);
    }

    if max_depth == 0 {
        return found;
    }

    // Walk subdirectories
    walk_dir(root, file_name, 1, max_depth, &mut found);
    found
}

/// Recursive directory walker for subdirs search.
fn walk_dir(
    dir: &Utf8PathBuf,
    file_name: &str,
    depth: usize,
    max_depth: usize,
    found: &mut Vec<Utf8PathBuf>,
) {
    let entries = match std::fs::read_dir(dir.as_std_path()) {
        Ok(e) => e,
        Err(_) => return,
    };

    // Collect subdirectories (sorted for deterministic order)
    let mut subdirs: Vec<Utf8PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir()
            && let Ok(utf8_path) = Utf8PathBuf::from_path_buf(path)
        {
            subdirs.push(utf8_path);
        }
    }
    subdirs.sort();

    for subdir in subdirs {
        // Check for the file in this subdirectory
        let candidate = subdir.join(file_name);
        if candidate.exists() {
            found.push(candidate);
        }

        // Recurse if we haven't hit max depth
        if depth < max_depth {
            walk_dir(&subdir, file_name, depth + 1, max_depth, found);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::ProjectPaths;
    use crate::schema::{ContextFileConfig, ContextFileEntry, ContextSearch};
    use tempfile::tempdir;

    fn make_project(tmp: &tempfile::TempDir) -> ProjectPaths {
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        std::fs::create_dir_all(root.join(".lim")).unwrap();
        ProjectPaths::new(root).unwrap()
    }

    #[test]
    fn no_config_returns_empty() {
        let tmp = tempdir().unwrap();
        let paths = make_project(&tmp);
        let loaded = load_context_files(None, &paths);
        assert!(loaded.is_empty());
    }

    #[test]
    fn disabled_returns_empty() {
        let tmp = tempdir().unwrap();
        let paths = make_project(&tmp);
        let config = ContextFileConfig { enabled: false, files: vec![] };
        let loaded = load_context_files(Some(&config), &paths);
        assert!(loaded.is_empty());
    }

    #[test]
    fn default_loads_agents_md_at_root() {
        let tmp = tempdir().unwrap();
        let paths = make_project(&tmp);
        std::fs::write(paths.workspace_root.join("AGENTS.md"), "# Project rules").unwrap();

        let config = ContextFileConfig { enabled: true, files: vec![] };
        let loaded = load_context_files(Some(&config), &paths);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].label, "AGENTS.md");
        assert!(loaded[0].content.contains("Project rules"));
    }

    #[test]
    fn workspace_root_search() {
        let tmp = tempdir().unwrap();
        let paths = make_project(&tmp);
        std::fs::write(paths.workspace_root.join("CONVENTIONS.md"), "# Conventions").unwrap();

        let config = ContextFileConfig {
            enabled: true,
            files: vec![ContextFileEntry {
                name: Some("CONVENTIONS.md".into()),
                search: ContextSearch::WorkspaceRoot,
                ..Default::default()
            }],
        };
        let loaded = load_context_files(Some(&config), &paths);
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].content.contains("Conventions"));
    }

    #[test]
    fn fixed_path_search() {
        let tmp = tempdir().unwrap();
        let paths = make_project(&tmp);
        std::fs::create_dir_all(paths.workspace_root.join("docs/conventions")).unwrap();
        std::fs::write(paths.workspace_root.join("docs/conventions/basics.md"), "# Basics content")
            .unwrap();

        let config = ContextFileConfig {
            enabled: true,
            files: vec![ContextFileEntry {
                path: Some("docs/conventions/basics.md".into()),
                label: Some("basics".into()),
                ..Default::default()
            }],
        };
        let loaded = load_context_files(Some(&config), &paths);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].label, "basics");
        assert!(loaded[0].content.contains("Basics content"));
    }

    #[test]
    fn walk_up_finds_files_in_parent_dirs() {
        let tmp = tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        // Simulate nested workspace: tmp/parent/AGENTS.md, tmp/parent/child/.lim/
        std::fs::create_dir_all(root.join("parent/child/.lim")).unwrap();
        std::fs::write(root.join("parent/AGENTS.md"), "# Parent rules").unwrap();
        std::fs::write(root.join("parent/child/AGENTS.md"), "# Child rules").unwrap();

        let paths = ProjectPaths::new(root.join("parent/child")).unwrap();
        let config = ContextFileConfig {
            enabled: true,
            files: vec![ContextFileEntry {
                name: Some("AGENTS.md".into()),
                search: ContextSearch::WalkUp,
                ..Default::default()
            }],
        };
        let loaded = load_context_files(Some(&config), &paths);
        // Both files found, closest first
        assert_eq!(loaded.len(), 2);
        assert!(loaded[0].content.contains("Child rules"));
        assert!(loaded[1].content.contains("Parent rules"));
    }

    #[test]
    fn subdirs_search_finds_files_in_children() {
        let tmp = tempdir().unwrap();
        let paths = make_project(&tmp);
        let root = &paths.workspace_root;
        // Root + 2 subdirs each with CONVENTIONS.md
        std::fs::write(root.join("CONVENTIONS.md"), "# Root conventions").unwrap();
        std::fs::create_dir_all(root.join("crates/alpha")).unwrap();
        std::fs::write(root.join("crates/alpha/CONVENTIONS.md"), "# Alpha conventions").unwrap();
        std::fs::create_dir_all(root.join("crates/beta")).unwrap();
        std::fs::write(root.join("crates/beta/CONVENTIONS.md"), "# Beta conventions").unwrap();

        let config = ContextFileConfig {
            enabled: true,
            files: vec![ContextFileEntry {
                name: Some("CONVENTIONS.md".into()),
                search: ContextSearch::Subdirs,
                max_depth: 2,
                ..Default::default()
            }],
        };
        let loaded = load_context_files(Some(&config), &paths);
        // Root + 2 subdirs = 3 files
        assert_eq!(loaded.len(), 3);
        // Root first
        assert!(loaded[0].content.contains("Root conventions"));
        // Then subdirs alphabetically (alpha < beta < crates)
        assert!(loaded.iter().any(|c| c.content.contains("Alpha conventions")));
        assert!(loaded.iter().any(|c| c.content.contains("Beta conventions")));
    }

    #[test]
    fn subdirs_depth_zero_only_root() {
        let tmp = tempdir().unwrap();
        let paths = make_project(&tmp);
        let root = &paths.workspace_root;
        std::fs::write(root.join("CONVENTIONS.md"), "# Root").unwrap();
        std::fs::create_dir_all(root.join("subdir")).unwrap();
        std::fs::write(root.join("subdir/CONVENTIONS.md"), "# Subdir").unwrap();

        let config = ContextFileConfig {
            enabled: true,
            files: vec![ContextFileEntry {
                name: Some("CONVENTIONS.md".into()),
                search: ContextSearch::Subdirs,
                max_depth: 0,
                ..Default::default()
            }],
        };
        let loaded = load_context_files(Some(&config), &paths);
        assert_eq!(loaded.len(), 1, "depth 0 should only find root file");
        assert!(loaded[0].content.contains("Root"));
    }

    #[test]
    fn missing_file_skipped_silently() {
        let tmp = tempdir().unwrap();
        let paths = make_project(&tmp);

        let config = ContextFileConfig {
            enabled: true,
            files: vec![ContextFileEntry {
                name: Some("NONEXISTENT.md".into()),
                search: ContextSearch::WorkspaceRoot,
                ..Default::default()
            }],
        };
        let loaded = load_context_files(Some(&config), &paths);
        assert!(loaded.is_empty(), "missing file should produce no segments");
    }

    #[test]
    fn multiple_files_preserve_order() {
        let tmp = tempdir().unwrap();
        let paths = make_project(&tmp);
        let root = &paths.workspace_root;
        std::fs::write(root.join("AGENTS.md"), "# Agents").unwrap();
        std::fs::write(root.join("CONVENTIONS.md"), "# Conventions").unwrap();

        let config = ContextFileConfig {
            enabled: true,
            files: vec![
                ContextFileEntry { name: Some("CONVENTIONS.md".into()), ..Default::default() },
                ContextFileEntry { name: Some("AGENTS.md".into()), ..Default::default() },
            ],
        };
        let loaded = load_context_files(Some(&config), &paths);
        assert_eq!(loaded.len(), 2);
        // Order preserved: CONVENTIONS.md first, AGENTS.md second
        assert_eq!(loaded[0].label, "CONVENTIONS.md");
        assert_eq!(loaded[1].label, "AGENTS.md");
    }

    #[test]
    fn custom_label_used_when_set() {
        let tmp = tempdir().unwrap();
        let paths = make_project(&tmp);
        std::fs::write(paths.workspace_root.join("AGENTS.md"), "# content").unwrap();

        let config = ContextFileConfig {
            enabled: true,
            files: vec![ContextFileEntry {
                name: Some("AGENTS.md".into()),
                label: Some("project_rules".into()),
                ..Default::default()
            }],
        };
        let loaded = load_context_files(Some(&config), &paths);
        assert_eq!(loaded[0].label, "project_rules");
    }
}
