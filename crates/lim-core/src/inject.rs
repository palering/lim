//! Information injection — transform [`PlatformInfo`] into context fragments.
//!
//! This is the **application layer** (prompt-engineering-like), decoupled from
//! [`crate::env::PlatformInfo`] which is the detection layer. The split exists
//! because:
//!
//! - Detection (env-like) should be a pure, side-effect-free query of the host.
//! - Injection (prompt-engineering-like) decides *how* to present that info to
//!   the LLM: which fields to surface, what guidance to add per shell, what
//!   path conventions to recommend, etc.
//!
//! Keeping them separate lets us change the prompt wording without touching
//! detection logic, and vice versa.
//!
//! ## Current scope
//!
//! For Phase 3c, only system-prompt injection is implemented. Future work:
//!
//! - Tool description augmentation (per-tool hints based on platform)
//! - Session metadata (record platform in JSONL header for replay parity)
//! - Runtime re-injection on platform change (rare; mostly CI matrices)

use crate::env::{PlatformInfo, ShellKind};

/// Transform platform info into context fragments for the LLM.
///
/// Construct once at startup from a [`PlatformInfo`] snapshot and used to
/// render fragments for various injection points.
pub struct InjectInfo {
    platform: PlatformInfo,
}

impl InjectInfo {
    /// Construct from a detected platform snapshot.
    pub fn new(platform: PlatformInfo) -> Self {
        Self { platform }
    }

    /// Borrow the underlying platform info.
    pub fn platform(&self) -> &PlatformInfo {
        &self.platform
    }

    /// Render a system-prompt fragment describing the host environment.
    ///
    /// Designed to be appended to (or prepended to) a user-supplied system
    /// prompt. Always returns a non-empty string when called on a detected
    /// [`PlatformInfo`]; callers may freely concatenate it.
    ///
    /// The fragment is intentionally compact (~10 lines) to avoid wasting
    /// tokens. Sensitive info (full workspace paths beyond the root name,
    /// env var values, hostnames) is deliberately NOT included — only what
    /// the LLM needs to write platform-correct commands.
    pub fn system_prompt_fragment(&self) -> String {
        let mut s = String::with_capacity(512);
        s.push_str("## Environment\n");
        s.push_str(&format!("- OS: {}\n", self.platform.os.as_str()));
        s.push_str(&format!("- Arch: {}\n", self.platform.arch.as_str()));
        s.push_str(&format!(
            "- Shell: {} ({})\n",
            self.platform.shell.as_str(),
            self.platform.shell.description()
        ));

        if let Some(root) = self.platform.workspace_roots.first() {
            s.push_str(&format!("- Workspace root: {root}\n"));
        }

        s.push('\n');
        s.push_str(self.shell_guidance());
        s
    }

    /// Shell-specific guidance for the LLM.
    ///
    /// The LLM uses this to choose correct syntax: POSIX vs. PowerShell,
    /// Unix paths vs. Windows paths, available utilities, etc.
    fn shell_guidance(&self) -> &'static str {
        match self.platform.shell {
            ShellKind::PosixSh => {
                "## Shell guidance\n\
                 You are running under POSIX `sh` (not bash). Use POSIX syntax:\n\
                 - Prefer portable constructs: avoid bash-specific features like `[[ ]]`, \
                   `<<<`, process substitution `<(...)`, or `bash` arrays.\n\
                 - macOS uses BSD utilities (e.g. `sed -i ''`), Linux uses GNU \
                   (e.g. `sed -i`). When in doubt, write portable constructs or \
                   test both forms.\n\
                 - Standard utilities (`ls`, `cat`, `grep`, `awk`, `sed`, `find`) \
                   are available everywhere.\n"
            }
            ShellKind::GitBash => {
                "## Shell guidance\n\
                 You are running under Git Bash on Windows (POSIX-compatible).\n\
                 - Use POSIX shell syntax. `bash` 4+ features are supported.\n\
                 - Paths: use Unix-style `/c/Users/...` (Git Bash mounts drives \
                   under `/c/`, `/d/`, etc.) or quote Windows paths with single \
                   quotes if needed.\n\
                 - Standard Unix utilities are bundled (ls, grep, awk, sed, etc.).\n\
                 - Windows-native commands (`cmd`, `powershell`) can be called \
                   from inside Git Bash if needed.\n"
            }
            ShellKind::Wsl => {
                "## Shell guidance\n\
                 You are running under WSL (Windows Subsystem for Linux).\n\
                 - Use POSIX shell syntax. This is a real Linux environment.\n\
                 - The Linux filesystem is at `/`; Windows files are mounted \
                   under `/mnt/c/`, `/mnt/d/`, etc.\n\
                 - To invoke a Windows binary from WSL, use its `.exe` name \
                   (e.g. `code.exe`, `notepad.exe`).\n\
                 - Standard Linux utilities are available.\n"
            }
            ShellKind::PowerShell => {
                "## Shell guidance\n\
                 You are running under Windows PowerShell.\n\
                 - Use PowerShell cmdlets and syntax (e.g. `Get-ChildItem`, \
                   `Get-Content`, `Invoke-WebRequest`), NOT Unix commands.\n\
                 - Some Unix aliases exist (`ls`, `cat`, `cp`) but flag syntax \
                   differs — prefer native PowerShell cmdlets.\n\
                 - Paths use backslashes: `C:\\Users\\...` (forward slashes also \
                   work in most cmdlets).\n\
                 - Pipe output with `|`; filter with `Where-Object` / `Select-Object`.\n"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{ArchKind, OsKind, ShellKind};
    use camino::Utf8PathBuf;

    fn make_platform(shell: ShellKind) -> PlatformInfo {
        PlatformInfo {
            os: match shell {
                ShellKind::PosixSh => OsKind::Linux,
                ShellKind::GitBash | ShellKind::Wsl | ShellKind::PowerShell => OsKind::Windows,
            },
            arch: ArchKind::X86_64,
            shell,
            workspace_roots: vec![Utf8PathBuf::from("/workspace")],
        }
    }

    #[test]
    fn system_prompt_fragment_includes_environment_section() {
        let info = InjectInfo::new(make_platform(ShellKind::PosixSh));
        let frag = info.system_prompt_fragment();
        assert!(frag.contains("## Environment"), "missing Environment section");
        assert!(frag.contains("OS: linux"), "missing OS line");
        assert!(frag.contains("Arch: x86_64"), "missing Arch line");
        assert!(frag.contains("Shell: sh"), "missing Shell line");
        assert!(frag.contains("Workspace root: /workspace"), "missing workspace line");
    }

    #[test]
    fn system_prompt_fragment_includes_shell_guidance() {
        let info = InjectInfo::new(make_platform(ShellKind::PosixSh));
        let frag = info.system_prompt_fragment();
        assert!(frag.contains("## Shell guidance"), "missing shell guidance section");
        assert!(frag.contains("POSIX"), "POSIX shell guidance should mention POSIX");
    }

    #[test]
    fn posix_shell_guidance_mentions_bsd_vs_gnu() {
        let info = InjectInfo::new(make_platform(ShellKind::PosixSh));
        let frag = info.system_prompt_fragment();
        assert!(frag.contains("BSD"), "POSIX guidance should warn about BSD/GNU differences");
        assert!(frag.contains("GNU"));
    }

    #[test]
    fn git_bash_guidance_mentions_drive_mounts() {
        let info = InjectInfo::new(make_platform(ShellKind::GitBash));
        let frag = info.system_prompt_fragment();
        assert!(frag.contains("/c/"), "Git Bash guidance should explain drive mounts");
    }

    #[test]
    fn wsl_guidance_mentions_mnt() {
        let info = InjectInfo::new(make_platform(ShellKind::Wsl));
        let frag = info.system_prompt_fragment();
        assert!(frag.contains("/mnt/"), "WSL guidance should explain /mnt/ mounts");
    }

    #[test]
    fn powershell_guidance_mentions_cmdlets() {
        let info = InjectInfo::new(make_platform(ShellKind::PowerShell));
        let frag = info.system_prompt_fragment();
        assert!(frag.contains("Get-ChildItem"), "PowerShell guidance should mention cmdlets");
        assert!(frag.contains("backslashes"), "PowerShell guidance should mention path style");
    }

    #[test]
    fn platform_borrow_works() {
        let info = InjectInfo::new(make_platform(ShellKind::PosixSh));
        assert_eq!(info.platform().shell, ShellKind::PosixSh);
    }

    #[test]
    fn fragment_is_non_empty_for_all_shell_kinds() {
        for &shell in
            &[ShellKind::PosixSh, ShellKind::GitBash, ShellKind::Wsl, ShellKind::PowerShell]
        {
            let info = InjectInfo::new(make_platform(shell));
            let frag = info.system_prompt_fragment();
            assert!(!frag.is_empty(), "fragment should be non-empty for {shell:?}");
            assert!(frag.len() > 100, "fragment should be substantial for {shell:?}");
        }
    }
}
