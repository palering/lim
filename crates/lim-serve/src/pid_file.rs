//! PID file management for `lim serve --daemon`.
//!
//! The PID file lives at `~/.lim/lim.pid` and records the daemon's
//! process ID. It's used by `lim serve --daemon stop` to find and
//! signal the running daemon.

use std::fs;
use std::path::{Path, PathBuf};

use lim_core::error::{LimError, Result};

/// Manages the PID file for a lim serve daemon.
pub struct PidFile {
    path: PathBuf,
}

impl PidFile {
    /// Create a PID file handle for the given path (typically
    /// `~/.lim/lim.pid`).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Default location: `~/.lim/lim.pid`.
    pub fn default_path() -> Result<PathBuf> {
        let home =
            std::env::var_os("HOME").ok_or_else(|| LimError::Config("HOME not set".to_string()))?;
        Ok(PathBuf::from(home).join(".lim").join("lim.pid"))
    }

    /// Path to the PID file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write the PID to the file, creating parent directories if needed.
    pub fn write(&self, pid: u32) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| LimError::Config(format!("create pid dir: {e}")))?;
        }
        fs::write(&self.path, pid.to_string())
            .map_err(|e| LimError::Config(format!("write pid file: {e}")))?;
        Ok(())
    }

    /// Read the PID from the file. Returns `None` if the file doesn't
    /// exist or is empty.
    pub fn read(&self) -> Result<Option<u32>> {
        let contents = match fs::read_to_string(&self.path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(LimError::Config(format!("read pid file: {e}"))),
        };
        let trimmed = contents.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        trimmed
            .parse::<u32>()
            .map(Some)
            .map_err(|e| LimError::Config(format!("parse pid file: {e}")))
    }

    /// Remove the PID file (e.g. on clean shutdown).
    pub fn remove(&self) -> Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(LimError::Config(format!("remove pid file: {e}"))),
        }
    }

    /// Check whether the process identified by the PID file is still
    /// running.
    pub fn is_running(&self) -> bool {
        match self.read() {
            Ok(Some(pid)) => is_process_alive(pid),
            _ => false,
        }
    }
}

/// Check if a process with the given PID is alive.
pub fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0) returns 0 if the process exists, -1 otherwise.
        // SAFETY: kill with signal 0 is a standard POSIX no-op check.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{BOOL, CloseHandle};
        use windows::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        // STILL_ACTIVE (259) is the exit code Windows reports for a
        // process that has not yet exited.
        const STILL_ACTIVE: u32 = 259;

        // SAFETY: OpenProcess queries a handle to the target PID; the
        // handle is released below via CloseHandle. BOOL(0) == false
        // means the handle is not inheritable.
        let handle = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, BOOL(0), pid) } {
            Ok(h) => h,
            Err(_) => return false,
        };

        let mut exit_code: u32 = 0;
        // SAFETY: exit_code is a valid out-pointer to a stack u32.
        let alive = unsafe { GetExitCodeProcess(handle, &mut exit_code).is_ok() }
            && exit_code == STILL_ACTIVE;

        // SAFETY: handle is a valid process handle from OpenProcess.
        // CloseHandle returns a Result we deliberately ignore here.
        drop(unsafe { CloseHandle(handle) });

        alive
    }
    #[cfg(not(any(unix, windows)))]
    {
        // No platform-specific liveness API available; conservatively
        // return true if the PID is non-zero.
        pid != 0
    }
}

/// Send a termination signal to a process.
///
/// On Unix this sends SIGTERM; on Windows it calls TerminateProcess.
pub fn terminate_process(pid: u32) -> Result<()> {
    #[cfg(unix)]
    {
        // SAFETY: kill with SIGTERM is standard POSIX.
        let rc = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        if rc != 0 {
            return Err(LimError::Config(format!(
                "kill(pid={pid}, SIGTERM) failed: errno {}",
                std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
            )));
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{BOOL, CloseHandle};
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};

        // SAFETY: OpenProcess requests a handle with PROCESS_TERMINATE;
        // the handle is released below via CloseHandle. BOOL(0) == false
        // means the handle is not inheritable.
        let handle = unsafe { OpenProcess(PROCESS_TERMINATE, BOOL(0), pid) }
            .map_err(|e| LimError::Config(format!("OpenProcess(pid={pid}): {e}")))?;

        // SAFETY: handle is a valid process handle with PROCESS_TERMINATE
        // access; exit code 1 is a conventional generic failure code.
        let term_result = unsafe { TerminateProcess(handle, 1) };

        // SAFETY: handle is a valid process handle from OpenProcess.
        // CloseHandle returns a Result we deliberately ignore during cleanup.
        drop(unsafe { CloseHandle(handle) });

        term_result.map_err(|e| LimError::Config(format!("TerminateProcess(pid={pid}): {e}")))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        Err(LimError::Config("terminate_process not implemented on this platform".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_file_write_read_remove() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = PidFile::new(dir.path().join("test.pid"));

        assert_eq!(pid_file.read().unwrap(), None);

        pid_file.write(12345).unwrap();
        assert_eq!(pid_file.read().unwrap(), Some(12345));

        pid_file.remove().unwrap();
        assert_eq!(pid_file.read().unwrap(), None);
    }

    #[test]
    fn pid_file_remove_missing_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = PidFile::new(dir.path().join("nonexistent.pid"));
        // Removing a missing file should not error.
        pid_file.remove().unwrap();
    }

    #[test]
    fn pid_file_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b").join("test.pid");
        let pid_file = PidFile::new(&nested);

        pid_file.write(99).unwrap();
        assert_eq!(pid_file.read().unwrap(), Some(99));
    }

    #[test]
    fn pid_file_handles_garbage_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("garbage.pid");
        std::fs::write(&path, "not a number").unwrap();
        let pid_file = PidFile::new(&path);
        assert!(pid_file.read().is_err());
    }
}
