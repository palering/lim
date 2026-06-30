use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use lim_core::error::{LimError, Result};
use lim_core::guard::{ToolGuard, TrustLevel};

use crate::executor::{
    CommandOutput, CommandSpec, EditReport, Executor, ExecutorCapabilities, ExecutorFuture, FileEdit,
};
use crate::policy::{DefaultPolicy, PermissionPolicy};

/// Local executor — runs file operations and commands on the host machine.
///
/// Phase 1 implementation with L0 permission policy. For production use,
/// prefer `LocalExecutor::workspace(roots)`; `permissive()` is deprecated
/// and should only be used in tests.
pub struct LocalExecutor {
    policy: Arc<DefaultPolicy>,
}

impl LocalExecutor {
    /// Create a local executor with the given permission policy.
    pub fn new(policy: DefaultPolicy) -> Self {
        Self {
            policy: Arc::new(policy),
        }
    }

    /// Create a workspace-scoped local executor.
    ///
    /// This is the recommended production constructor: paths are restricted
    /// to `workspace_roots`, dangerous commands are blocked, and
    /// cross-workspace `git -C` is rejected.
    ///
    /// See docs/draft/trust-and-approval.md for the full trust model.
    pub fn workspace(workspace_roots: Vec<Utf8PathBuf>) -> Self {
        Self::new(DefaultPolicy::new(workspace_roots))
    }

    /// Create a permissive local executor (allows everything).
    ///
    /// Deprecated for production use — prefer `LocalExecutor::workspace(vec![cwd])`.
    #[deprecated(note = "permissive executor allows everything; use `workspace(vec![root])` for production")]
    #[allow(deprecated)]
    pub fn permissive() -> Self {
        Self::new(DefaultPolicy::permissive())
    }
}

impl ToolGuard for LocalExecutor {
    fn trust_level(&self) -> TrustLevel {
        self.policy.trust_level()
    }
}

impl Executor for LocalExecutor {
    fn read_file(&self, path: &Utf8Path) -> ExecutorFuture<Result<Vec<u8>>> {
        let policy = Arc::clone(&self.policy);
        let path = path.to_owned();
        Box::pin(async move {
            policy.check_path(&path)?;
            debug!(%path, "reading file");
            tokio::fs::read(path.as_std_path())
                .await
                .map_err(|e| LimError::ToolExecution {
                    name: "read".to_string(),
                    message: format!("failed to read `{path}`: {e}"),
                })
        })
    }

    fn write_file_atomic(&self, path: &Utf8Path, bytes: &[u8]) -> ExecutorFuture<Result<()>> {
        let policy = Arc::clone(&self.policy);
        let path = path.to_owned();
        let bytes = bytes.to_vec();
        Box::pin(async move {
            policy.check_path(&path)?;
            debug!(%path, len = bytes.len(), "writing file atomically");

            // Ensure parent directory exists
            if let Some(parent) = path.parent()
                && !parent.as_std_path().exists()
            {
                tokio::fs::create_dir_all(parent.as_std_path())
                    .await
                    .map_err(|e| LimError::ToolExecution {
                        name: "write".to_string(),
                        message: format!("failed to create parent dirs for `{path}`: {e}"),
                    })?;
            }

            // Write to a temporary file in the same directory, then rename
            let parent = path.parent().unwrap_or(Utf8Path::new("."));
            let temp_path = parent.join(format!(".lim-tmp-{}", ulid::Ulid::new()));

            tokio::fs::write(temp_path.as_std_path(), &bytes)
                .await
                .map_err(|e| LimError::ToolExecution {
                    name: "write".to_string(),
                    message: format!("failed to write temp file: {e}"),
                })?;

            // fsync the temp file
            let file = tokio::fs::File::open(temp_path.as_std_path())
                .await
                .map_err(|e| LimError::ToolExecution {
                    name: "write".to_string(),
                    message: format!("failed to open temp file for fsync: {e}"),
                })?;
            file.sync_all().await.map_err(|e| LimError::ToolExecution {
                name: "write".to_string(),
                message: format!("fsync failed: {e}"),
            })?;
            drop(file);

            // Atomic rename
            tokio::fs::rename(temp_path.as_std_path(), path.as_std_path())
                .await
                .map_err(|e| LimError::ToolExecution {
                    name: "write".to_string(),
                    message: format!("atomic rename failed: {e}"),
                })?;

            Ok(())
        })
    }

    fn apply_edit(&self, edit: FileEdit) -> ExecutorFuture<Result<EditReport>> {
        let policy = Arc::clone(&self.policy);
        Box::pin(async move {
            policy.check_path(&edit.path)?;
            debug!(path = %edit.path, "applying edit");

            let content = tokio::fs::read_to_string(edit.path.as_std_path())
                .await
                .map_err(|e| LimError::ToolExecution {
                    name: "edit".to_string(),
                    message: format!("failed to read `{}`: {e}", edit.path),
                })?;

            if !content.contains(&edit.old_string) {
                return Ok(EditReport {
                    applied: false,
                    replacements: 0,
                    message: format!("old_string not found in `{}`", edit.path),
                });
            }

            let count = content.matches(&edit.old_string).count();
            if count > 1 {
                return Ok(EditReport {
                    applied: false,
                    replacements: 0,
                    message: format!(
                        "old_string found {count} times in `{}` — must be unique",
                        edit.path
                    ),
                });
            }

            let new_content = content.replacen(&edit.old_string, &edit.new_string, 1);

            // Atomic write (inlined to avoid self-borrow in future)
            policy.check_path(&edit.path)?;
            if let Some(parent) = edit.path.parent()
                && !parent.as_std_path().exists()
            {
                tokio::fs::create_dir_all(parent.as_std_path())
                    .await
                    .map_err(|e| LimError::ToolExecution {
                        name: "write".to_string(),
                        message: format!("failed to create parent dirs: {e}"),
                    })?;
            }
            let parent = edit.path.parent().unwrap_or(Utf8Path::new("."));
            let temp_path = parent.join(format!(".lim-tmp-{}", ulid::Ulid::new()));
            tokio::fs::write(temp_path.as_std_path(), new_content.as_bytes())
                .await
                .map_err(|e| LimError::ToolExecution {
                    name: "write".to_string(),
                    message: format!("failed to write temp file: {e}"),
                })?;
            let file = tokio::fs::File::open(temp_path.as_std_path())
                .await
                .map_err(|e| LimError::ToolExecution {
                    name: "write".to_string(),
                    message: format!("failed to open temp file for fsync: {e}"),
                })?;
            file.sync_all().await.map_err(|e| LimError::ToolExecution {
                name: "write".to_string(),
                message: format!("fsync failed: {e}"),
            })?;
            drop(file);
            tokio::fs::rename(temp_path.as_std_path(), edit.path.as_std_path())
                .await
                .map_err(|e| LimError::ToolExecution {
                    name: "write".to_string(),
                    message: format!("atomic rename failed: {e}"),
                })?;

            Ok(EditReport {
                applied: true,
                replacements: 1,
                message: format!("applied 1 replacement to `{}`", edit.path),
            })
        })
    }

    fn run_command(
        &self,
        command: CommandSpec,
        cancel: CancellationToken,
    ) -> ExecutorFuture<Result<CommandOutput>> {
        let policy = Arc::clone(&self.policy);
        Box::pin(async move {
            policy.check_command(&command.command, &command.args)?;
            debug!(command = %command.command, args = ?command.args, "running command");

            let mut cmd = tokio::process::Command::new(&command.command);
            cmd.args(&command.args);

            if let Some(ref cwd) = command.cwd {
                cmd.current_dir(cwd.as_std_path());
            }

            for (key, value) in &command.env {
                cmd.env(key, value);
            }

            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());

            // Create a new process group so we can kill the whole group on cancel
            #[cfg(unix)]
            {
                #[allow(unused_imports)]
                use std::os::unix::process::CommandExt as _;
                unsafe {
                    cmd.pre_exec(|| {
                        libc::setpgid(0, 0);
                        Ok(())
                    });
                }
            }

            let mut child = cmd.spawn().map_err(|e| LimError::ToolExecution {
                name: "bash".to_string(),
                message: format!("failed to spawn `{}`: {e}", command.command),
            })?;

            let child_id = child.id().unwrap_or(0);

            // Take stdout/stderr pipes for concurrent reading
            let mut stdout_pipe = child.stdout.take();
            let mut stderr_pipe = child.stderr.take();

            let stdout_handle = tokio::spawn(async move {
                let mut buf = Vec::new();
                if let Some(ref mut pipe) = stdout_pipe {
                    use tokio::io::AsyncReadExt;
                    let _ = pipe.read_to_end(&mut buf).await;
                }
                buf
            });

            let stderr_handle = tokio::spawn(async move {
                let mut buf = Vec::new();
                if let Some(ref mut pipe) = stderr_pipe {
                    use tokio::io::AsyncReadExt;
                    let _ = pipe.read_to_end(&mut buf).await;
                }
                buf
            });

            // Wait for completion or cancellation
            let was_cancelled = tokio::select! {
                _ = child.wait() => false,
                _ = cancel.cancelled() => true,
            };

            if was_cancelled {
                warn!(child_id, "cancelling command");
                #[cfg(unix)]
                unsafe {
                    libc::killpg(child_id as i32, libc::SIGTERM);
                }
                // Give it a moment to clean up, then force kill
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let _ = child.kill().await;
                let _ = child.wait().await;
            }

            let stdout = stdout_handle.await.unwrap_or_default();
            let stderr = stderr_handle.await.unwrap_or_default();
            let stdout = String::from_utf8_lossy(&stdout).to_string();
            let stderr = String::from_utf8_lossy(&stderr).to_string();

            if was_cancelled {
                let mut err_msg = stderr;
                if !err_msg.is_empty() {
                    err_msg.push('\n');
                }
                err_msg.push_str("[Command interrupted by user]");
                Ok(CommandOutput {
                    stdout,
                    stderr: err_msg,
                    exit_code: -1,
                })
            } else {
                let exit_code = child
                    .try_wait()
                    .ok()
                    .flatten()
                    .and_then(|s| s.code())
                    .unwrap_or(-1);
                Ok(CommandOutput {
                    stdout,
                    stderr,
                    exit_code,
                })
            }
        })
    }

    fn capabilities(&self) -> ExecutorCapabilities {
        ExecutorCapabilities {
            supports_files: true,
            supports_commands: true,
            is_sandboxed: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use tokio_util::sync::CancellationToken;

    fn test_executor() -> LocalExecutor {
        #[allow(deprecated)]
        LocalExecutor::permissive()
    }

    #[tokio::test]
    async fn write_and_read_file() {
        let exec = test_executor();
        let dir = std::env::temp_dir().join("lim-test-write-read");
        let _ = std::fs::create_dir_all(&dir);
        let path = Utf8PathBuf::try_from(dir.join("test.txt")).unwrap();

        exec.write_file_atomic(&path, b"hello world").await.unwrap();
        let content = exec.read_file(&path).await.unwrap();
        assert_eq!(content, b"hello world");

        // Cleanup
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn apply_edit_replaces_text() {
        let exec = test_executor();
        let dir = std::env::temp_dir().join("lim-test-edit");
        let _ = std::fs::create_dir_all(&dir);
        let path = Utf8PathBuf::try_from(dir.join("edit-test.txt")).unwrap();

        exec.write_file_atomic(&path, b"hello world")
            .await
            .unwrap();

        let edit = FileEdit {
            path: path.clone(),
            old_string: "world".to_string(),
            new_string: "lim".to_string(),
        };
        let report = exec.apply_edit(edit).await.unwrap();
        assert!(report.applied);
        assert_eq!(report.replacements, 1);

        let content = exec.read_file(&path).await.unwrap();
        assert_eq!(content, b"hello lim");

        // Cleanup
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn run_simple_command() {
        let exec = test_executor();
        let cancel = CancellationToken::new();
        let spec = CommandSpec {
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
            cwd: None,
            env: Default::default(),
            timeout_secs: 5,
        };
        let output = exec.run_command(spec, cancel).await.unwrap();
        assert!(output.is_success());
        assert_eq!(output.stdout.trim(), "hello");
    }

    #[tokio::test]
    async fn run_command_with_failure() {
        let exec = test_executor();
        let cancel = CancellationToken::new();
        let spec = CommandSpec {
            command: "false".to_string(),
            args: vec![],
            cwd: None,
            env: Default::default(),
            timeout_secs: 5,
        };
        let output = exec.run_command(spec, cancel).await.unwrap();
        assert!(!output.is_success());
    }
}
