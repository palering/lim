//! Daemon mode — detach from the terminal and run in the background.
//!
//! Unlike the traditional Unix `fork + setsid` approach, lim uses
//! **respawn**: the main process spawns a detached child process
//! (running `lim serve` without `--daemon`) and then exits. This
//! avoids the well-known fork-safety issues with tokio's multi-threaded
//! runtime (only one thread survives `fork`, corrupting the runtime
//! state).
//!
//! ## Flow
//!
//! 1. Main process (with `--daemon`) calls [`daemonize`].
//! 2. `daemonize` spawns a child: `lim serve <args>` with detached I/O.
//! 3. Main process writes the child's PID to `~/.lim/lim.pid`.
//! 4. Main process prints a confirmation and exits.
//! 5. Child process runs `serve` in the foreground (stdio redirected
//!    to log files), surviving the terminal close.
//!
//! ## Stopping
//!
//! [`stop_daemon`] reads the PID file and sends SIGTERM (Unix) or
//! TerminateProcess (Windows), then removes the PID file.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use lim_core::error::{LimError, Result};
use tracing::{info, warn};

use crate::pid_file::{PidFile, is_process_alive, terminate_process};

/// Default log directory: `~/.lim/logs/`.
pub fn default_log_dir() -> Result<PathBuf> {
    let home =
        std::env::var_os("HOME").ok_or_else(|| LimError::Config("HOME not set".to_string()))?;
    Ok(PathBuf::from(home).join(".lim").join("logs"))
}

/// Spawn a detached child process running `lim serve <args>` and write
/// its PID to the PID file.
///
/// `args` should be the serve arguments **without** `--daemon` (e.g.
/// `["--protocol", "mcp"]`). The child re-executes the current binary
/// with `serve` prepended.
///
/// Returns the child's PID. The caller (usually the main process) should
/// exit shortly after.
pub fn daemonize(args: &[String], log_dir: &Path) -> Result<u32> {
    let exe = std::env::current_exe().map_err(|e| LimError::Config(format!("current_exe: {e}")))?;

    let stdout_path = log_dir.join("lim.log");
    let stderr_path = log_dir.join("lim.err.log");
    std::fs::create_dir_all(log_dir)
        .map_err(|e| LimError::Config(format!("create log dir: {e}")))?;

    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&stdout_path)
        .map_err(|e| LimError::Config(format!("open stdout log: {e}")))?;
    let stderr = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&stderr_path)
        .map_err(|e| LimError::Config(format!("open stderr log: {e}")))?;

    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("serve");
    for a in args {
        cmd.arg(a);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::from(stdout)).stderr(Stdio::from(stderr));

    // Detach on Unix: setsid via pre_exec (creates a new session,
    // detaching from the controlling terminal).
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    // Detach on Windows: DETACHED_PROCESS + CREATE_NEW_PROCESS_GROUP.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x00000008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }

    let child = cmd.spawn().map_err(|e| LimError::Config(format!("spawn daemon child: {e}")))?;
    let pid = child.id();

    // Write the PID file.
    let pid_path = PidFile::default_path()?;
    let pid_file = PidFile::new(&pid_path);
    pid_file.write(pid)?;

    info!(pid, log = %stdout_path.display(), "daemon spawned");

    // Intentionally do NOT wait on the child — we want it to outlive us.
    // `std::mem::forget` prevents Drop from killing the child.
    std::mem::forget(child);

    Ok(pid)
}

/// Stop a running daemon: read the PID file, terminate the process,
/// and remove the PID file.
///
/// Returns `Ok(true)` if a daemon was stopped, `Ok(false)` if no daemon
/// was running.
pub fn stop_daemon() -> Result<bool> {
    let pid_path = PidFile::default_path()?;
    let pid_file = PidFile::new(&pid_path);

    let pid = match pid_file.read()? {
        Some(pid) => pid,
        None => {
            info!("no PID file, daemon not running");
            return Ok(false);
        }
    };

    if !is_process_alive(pid) {
        warn!(pid, "PID file exists but process is dead, cleaning up");
        pid_file.remove()?;
        return Ok(false);
    }

    terminate_process(pid)?;
    pid_file.remove()?;
    info!(pid, "daemon stopped");
    Ok(true)
}

/// Check if a daemon is currently running (PID file exists and process
/// is alive). Returns the PID if so.
pub fn daemon_pid() -> Result<Option<u32>> {
    let pid_path = PidFile::default_path()?;
    let pid_file = PidFile::new(&pid_path);
    match pid_file.read()? {
        Some(pid) if is_process_alive(pid) => Ok(Some(pid)),
        Some(_) => {
            // Stale PID file — clean it up.
            let _ = pid_file.remove();
            Ok(None)
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_pid_returns_none_when_no_file() {
        // Point HOME at a temp dir so ~/.lim/lim.pid doesn't exist.
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: test is single-threaded.
        unsafe {
            std::env::set_var("HOME", dir.path());
        }
        let pid = daemon_pid().unwrap();
        assert!(pid.is_none());
    }
}
