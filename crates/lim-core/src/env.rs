//! Platform information detected at agent startup.
//!
//! [`PlatformInfo`] is an immutable snapshot of the host environment, captured
//! once at startup and shared via `Arc<PlatformInfo>` for the duration of a
//! session. It is the **detection layer** (alongside `std::env` and
//! `which`-style lookups); transforming this info into prompt fragments is
//! the job of [`crate::inject::InjectInfo`].
//!
//! ## Shell detection priority
//!
//! - **Unix (macOS / Linux)**: always `PosixSh` (`/bin/sh`, POSIX maximum
//!   denominator). macOS/BSD vs. GNU differences are left for the LLM to
//!   adapt to via execution feedback — `sh -c` is portable enough.
//! - **Windows**: Git Bash > WSL > PowerShell.
//!   - **Git Bash** is preferred when present: ships with most Windows dev
//!     setups, supports Unix syntax natively, fast startup.
//!   - **WSL** is the next best: full Linux compat, but slow startup and
//!     requires explicit install.
//!   - **PowerShell** is the fallback: always available on modern Windows,
//!     but no Unix commands — the LLM must adapt its syntax.
//!   - `cmd.exe` is **never** used: no mainstream agent uses it, and Unix
//!     syntax won't work. Surfacing "no Unix shell available" is preferable
//!     to silently degrading to `cmd /C`.

use std::path::PathBuf;

use camino::Utf8PathBuf;

/// Immutable snapshot of the host environment.
#[derive(Debug, Clone)]
pub struct PlatformInfo {
    /// Operating system family.
    pub os: OsKind,
    /// CPU architecture.
    pub arch: ArchKind,
    /// Shell selected for tool execution.
    pub shell: ShellKind,
    /// Workspace roots for path validation (git root preferred, cwd fallback).
    pub workspace_roots: Vec<Utf8PathBuf>,
}

impl PlatformInfo {
    /// Detect platform info from the current process environment.
    ///
    /// Performs `git rev-parse --show-toplevel` to find the workspace root
    /// (falls back to current dir on failure). Shell detection runs `which`-style
    /// PATH lookups on Windows. All operations are synchronous and fast (ms).
    pub fn detect() -> Self {
        let os = OsKind::from_const(std::env::consts::OS);
        let arch = ArchKind::from_const(std::env::consts::ARCH);
        let shell = detect_shell(os);
        let workspace_roots = detect_workspace_roots();
        Self { os, arch, shell, workspace_roots }
    }

    /// Whether the host is a Unix-like system (macOS or Linux).
    pub fn is_unix(&self) -> bool {
        self.os.is_unix()
    }

    /// Whether the host is Windows.
    pub fn is_windows(&self) -> bool {
        self.os.is_windows()
    }
}

/// Operating system family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsKind {
    Macos,
    Linux,
    Windows,
    /// Any other OS (BSD, Solaris, etc.).
    Other,
}

impl OsKind {
    /// Map `std::env::consts::OS` to [`OsKind`].
    pub fn from_const(os: &'static str) -> Self {
        match os {
            "macos" => Self::Macos,
            "linux" => Self::Linux,
            "windows" => Self::Windows,
            _ => Self::Other,
        }
    }

    /// String representation (stable across serialization).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Macos => "macos",
            Self::Linux => "linux",
            Self::Windows => "windows",
            Self::Other => "other",
        }
    }

    /// Whether the OS is Unix-like (POSIX shells, Unix file paths, etc.).
    pub fn is_unix(&self) -> bool {
        matches!(self, Self::Macos | Self::Linux | Self::Other)
    }

    /// Whether the OS is Windows.
    pub fn is_windows(&self) -> bool {
        matches!(self, Self::Windows)
    }
}

/// CPU architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchKind {
    X86_64,
    Aarch64,
    /// Any other architecture (riscv64, powerpc64, etc.).
    Other,
}

impl ArchKind {
    /// Map `std::env::consts::ARCH` to [`ArchKind`].
    pub fn from_const(arch: &'static str) -> Self {
        match arch {
            "x86_64" => Self::X86_64,
            "aarch64" => Self::Aarch64,
            _ => Self::Other,
        }
    }

    /// String representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
            Self::Other => "other",
        }
    }
}

/// Shell selected for tool execution.
///
/// Determined once at startup by [`detect_shell`]; never changes during a
/// session. The `shell` tool uses whatever backend this points to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    /// POSIX `/bin/sh` — default on Unix.
    PosixSh,
    /// Git Bash on Windows (`bash.exe` from a Git for Windows install).
    GitBash,
    /// Windows Subsystem for Linux.
    Wsl,
    /// Windows PowerShell (`powershell.exe`).
    PowerShell,
}

impl ShellKind {
    /// String identifier suitable for prompts and logs.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PosixSh => "sh",
            Self::GitBash => "git-bash",
            Self::Wsl => "wsl",
            Self::PowerShell => "powershell",
        }
    }

    /// Human-readable description.
    pub fn description(&self) -> &'static str {
        match self {
            Self::PosixSh => "POSIX sh (Unix)",
            Self::GitBash => "Git Bash (Windows)",
            Self::Wsl => "WSL (Windows Subsystem for Linux)",
            Self::PowerShell => "PowerShell (Windows)",
        }
    }

    /// Whether this shell accepts POSIX/Unix syntax.
    pub fn is_posix(&self) -> bool {
        matches!(self, Self::PosixSh | Self::GitBash | Self::Wsl)
    }
}

/// Detect the appropriate shell for the current platform.
///
/// Unix defaults to `PosixSh`. Windows picks the first available of Git Bash,
/// WSL, or PowerShell.
pub fn detect_shell(os: OsKind) -> ShellKind {
    match os {
        OsKind::Macos | OsKind::Linux | OsKind::Other => ShellKind::PosixSh,
        OsKind::Windows => detect_windows_shell(),
    }
}

/// Windows shell detection: Git Bash > WSL > PowerShell.
///
/// Returns [`ShellKind::PowerShell`] as the final fallback because it is always
/// available on modern Windows. `cmd.exe` is intentionally never returned.
fn detect_windows_shell() -> ShellKind {
    if which("bash").is_some() {
        ShellKind::GitBash
    } else if which("wsl").is_some() {
        ShellKind::Wsl
    } else if which("powershell").is_some() {
        ShellKind::PowerShell
    } else {
        // Modern Windows always has powershell.exe, but if a stripped-down
        // Windows image lacks even that, we still surface PowerShell as the
        // "best available" rather than panic — the agent will fail at command
        // execution time with a clear error rather than at startup.
        ShellKind::PowerShell
    }
}

/// Detect workspace roots for path validation.
///
/// Priority: `git rev-parse --show-toplevel` > current dir.
pub fn detect_workspace_roots() -> Vec<Utf8PathBuf> {
    let output = std::process::Command::new("git").args(["rev-parse", "--show-toplevel"]).output();
    if let Ok(out) = output
        && out.status.success()
    {
        let s = String::from_utf8_lossy(&out.stdout);
        let trimmed = s.trim();
        if !trimmed.is_empty()
            && let Ok(p) = Utf8PathBuf::from_path_buf(std::path::PathBuf::from(trimmed))
        {
            return vec![p];
        }
    }
    vec![Utf8PathBuf::from(".")]
}

/// Locate an executable on `PATH` (minimal `which` implementation).
///
/// Returns the first matching file found. On Windows, automatically appends
/// `.exe` to the command name if the bare name is not found.
///
/// This is intentionally simple — no permission checks (PATH directories are
/// assumed to be executable on Unix by convention). Used only for shell
/// detection at startup; not a general-purpose `which`.
pub fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(cmd);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let candidate = dir.join(format!("{cmd}.exe"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_kind_round_trip() {
        assert_eq!(OsKind::from_const("macos"), OsKind::Macos);
        assert_eq!(OsKind::from_const("linux"), OsKind::Linux);
        assert_eq!(OsKind::from_const("windows"), OsKind::Windows);
        assert_eq!(OsKind::from_const("freebsd"), OsKind::Other);

        assert_eq!(OsKind::Macos.as_str(), "macos");
        assert_eq!(OsKind::Linux.as_str(), "linux");
        assert_eq!(OsKind::Windows.as_str(), "windows");
        assert_eq!(OsKind::Other.as_str(), "other");
    }

    #[test]
    fn os_kind_classification() {
        assert!(OsKind::Macos.is_unix());
        assert!(OsKind::Linux.is_unix());
        assert!(!OsKind::Windows.is_unix());
        assert!(OsKind::Other.is_unix()); // conservative: assume Unix-like

        assert!(OsKind::Windows.is_windows());
        assert!(!OsKind::Macos.is_windows());
    }

    #[test]
    fn arch_kind_round_trip() {
        assert_eq!(ArchKind::from_const("x86_64"), ArchKind::X86_64);
        assert_eq!(ArchKind::from_const("aarch64"), ArchKind::Aarch64);
        assert_eq!(ArchKind::from_const("riscv64"), ArchKind::Other);

        assert_eq!(ArchKind::X86_64.as_str(), "x86_64");
        assert_eq!(ArchKind::Aarch64.as_str(), "aarch64");
        assert_eq!(ArchKind::Other.as_str(), "other");
    }

    #[test]
    fn shell_kind_strings() {
        assert_eq!(ShellKind::PosixSh.as_str(), "sh");
        assert_eq!(ShellKind::GitBash.as_str(), "git-bash");
        assert_eq!(ShellKind::Wsl.as_str(), "wsl");
        assert_eq!(ShellKind::PowerShell.as_str(), "powershell");
    }

    #[test]
    fn shell_kind_posix_classification() {
        // PosixSh, GitBash, Wsl all accept POSIX syntax
        assert!(ShellKind::PosixSh.is_posix());
        assert!(ShellKind::GitBash.is_posix());
        assert!(ShellKind::Wsl.is_posix());
        // PowerShell does NOT accept POSIX syntax
        assert!(!ShellKind::PowerShell.is_posix());
    }

    #[test]
    fn detect_shell_unix_returns_posix() {
        assert_eq!(detect_shell(OsKind::Macos), ShellKind::PosixSh);
        assert_eq!(detect_shell(OsKind::Linux), ShellKind::PosixSh);
        assert_eq!(detect_shell(OsKind::Other), ShellKind::PosixSh);
    }

    #[test]
    fn detect_workspace_roots_returns_non_empty() {
        let roots = detect_workspace_roots();
        assert!(!roots.is_empty(), "workspace_roots must never be empty");
        // On a dev machine inside the lim repo, this should resolve to the
        // git root (not "."), but we don't assert that here because the test
        // might run from a tarball or non-git context.
    }

    #[test]
    fn platform_info_detect_does_not_panic() {
        // Smoke test: detect() should never panic regardless of host.
        let info = PlatformInfo::detect();
        assert!(!info.workspace_roots.is_empty());
        // Shell must be one of the known variants
        assert!(matches!(
            info.shell,
            ShellKind::PosixSh | ShellKind::GitBash | ShellKind::Wsl | ShellKind::PowerShell
        ));
    }

    #[test]
    fn which_returns_some_for_known_command() {
        // `sh` is guaranteed on Unix; `cmd` on Windows. We don't assert
        // the exact path (varies by OS), just that lookup works.
        #[cfg(unix)]
        {
            assert!(which("sh").is_some(), "sh must be on PATH on Unix");
            assert!(which("definitely-not-a-real-command-xyz").is_none());
        }
        #[cfg(windows)]
        {
            assert!(which("cmd").is_some(), "cmd must be on PATH on Windows");
            // `where` is the Windows equivalent of `which`
            assert!(which("where").is_some());
        }
    }
}
