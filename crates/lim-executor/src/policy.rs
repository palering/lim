use camino::{Utf8Path, Utf8PathBuf};

use lim_core::error::{LimError, Result};

/// L0 permission policy — reduces accidental misuse but is NOT a sandbox.
///
/// Provides path prefix checks and dangerous command pattern matching.
pub trait PermissionPolicy: Send + Sync {
    /// Check if a file path is allowed for read/write operations.
    fn check_path(&self, path: &Utf8Path) -> Result<()>;

    /// Check if a command is allowed to execute.
    fn check_command(&self, command: &str, args: &[String]) -> Result<()>;
}

/// Default policy implementation.
///
/// - Path check: path must be under one of the allowed workspace roots.
/// - Command check: command must not match dangerous patterns.
pub struct DefaultPolicy {
    /// Allowed workspace root directories.
    workspace_roots: Vec<Utf8PathBuf>,
    /// Command patterns that are always rejected.
    dangerous_commands: Vec<String>,
}

impl DefaultPolicy {
    /// Create a policy with the given workspace roots.
    pub fn new(workspace_roots: Vec<Utf8PathBuf>) -> Self {
        Self {
            workspace_roots,
            dangerous_commands: vec![
                "rm -rf /".to_string(),
                "rm -rf /*".to_string(),
                "mkfs".to_string(),
                "dd if=/dev/zero".to_string(),
                ":(){:|:&};:".to_string(), // fork bomb
            ],
        }
    }

    /// Create a permissive policy (allows everything). Use only for testing.
    pub fn permissive() -> Self {
        Self {
            workspace_roots: Vec::new(),
            dangerous_commands: Vec::new(),
        }
    }

    /// Resolve a path to its canonical form, following symlinks.
    fn canonicalize(path: &Utf8Path) -> Result<Utf8PathBuf> {
        let std_path = path.as_std_path();
        // If the path doesn't exist yet (for write), canonicalize the parent
        if !std_path.exists()
            && let Some(parent) = std_path.parent()
        {
            let canon = std::fs::canonicalize(parent).map_err(|e| {
                LimError::ToolExecution {
                    name: "policy".to_string(),
                    message: format!("cannot resolve parent path: {e}"),
                }
            })?;
            let file_name = path
                .file_name()
                .unwrap_or_default();
            return Utf8PathBuf::from_path_buf(canon.join(file_name))
                .map_err(|_| {
                    LimError::ToolExecution {
                        name: "policy".to_string(),
                        message: "path contains non-UTF-8 characters".to_string(),
                    }
                });
        }
        let canon = std::fs::canonicalize(std_path).map_err(|e| {
            LimError::ToolExecution {
                name: "policy".to_string(),
                message: format!("cannot resolve path: {e}"),
            }
        })?;
        Utf8PathBuf::from_path_buf(canon).map_err(|_| LimError::ToolExecution {
            name: "policy".to_string(),
            message: "path contains non-UTF-8 characters".to_string(),
        })
    }
}

impl PermissionPolicy for DefaultPolicy {
    fn check_path(&self, path: &Utf8Path) -> Result<()> {
        // Permissive mode: no roots configured = allow all
        if self.workspace_roots.is_empty() {
            return Ok(());
        }

        let canonical = Self::canonicalize(path)?;

        let allowed = self.workspace_roots.iter().any(|root| {
            canonical.as_str().starts_with(root.as_str())
        });

        if allowed {
            Ok(())
        } else {
            Err(LimError::ToolExecution {
                name: "policy".to_string(),
                message: format!(
                    "path `{canonical}` is outside allowed workspace roots: {:?}",
                    self.workspace_roots
                ),
            })
        }
    }

    fn check_command(&self, command: &str, args: &[String]) -> Result<()> {
        if self.dangerous_commands.is_empty() {
            return Ok(());
        }

        let full_cmd = format!(
            "{} {}",
            command,
            args.join(" ")
        );

        for pattern in &self.dangerous_commands {
            if full_cmd.contains(pattern) {
                return Err(LimError::ToolExecution {
                    name: "policy".to_string(),
                    message: format!("command blocked by policy: matches `{pattern}`"),
                });
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissive_policy_allows_all() {
        let policy = DefaultPolicy::permissive();
        assert!(policy.check_path(Utf8Path::new("/etc/passwd")).is_ok());
        assert!(policy.check_command("rm", &["-rf".to_string(), "/".to_string()]).is_ok());
    }

    #[test]
    fn dangerous_command_blocked() {
        let policy = DefaultPolicy::new(vec![]);
        let result = policy.check_command("rm", &["-rf".to_string(), "/".to_string()]);
        assert!(result.is_err());
    }
}
