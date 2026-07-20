//! System service management (`lim serve --system`).
//!
//! Provides cross-platform install/uninstall/start/stop/status for lim
//! as a system service:
//!
//! | Platform | Backend | Scope |
//! |---|---|---|
//! | Linux | systemd user unit | `~/.config/systemd/user/lim.service` |
//! | macOS | launchd LaunchAgent | `~/Library/LaunchAgents/ai.lim.serve.plist` |
//! | Windows | Windows Service (SCM) | ✅ |
//!
//! User-level scope is preferred — lim is a developer tool and doesn't
//! need root/admin privileges to install.

use std::path::PathBuf;
use std::process::Command;

use lim_core::error::{LimError, Result};
use tracing::info;

use crate::options::{ServiceConfig, ServiceStatus};

/// Label / identifier used for the system service.
const SERVICE_LABEL: &str = "ai.lim.serve";

/// Trait abstracting platform-specific service lifecycle.
///
/// Implementations:
/// - [`SystemdServiceManager`] (Linux)
/// - [`LaunchdServiceManager`] (macOS)
/// - [`WindowsServiceManager`] (Windows)
pub trait ServiceManager: Send + Sync {
    /// Install the service (write unit file + enable).
    fn install(&self, config: &ServiceConfig) -> Result<()>;

    /// Uninstall the service (disable + remove unit file).
    fn uninstall(&self) -> Result<()>;

    /// Start the service.
    fn start(&self) -> Result<()>;

    /// Stop the service.
    fn stop(&self) -> Result<()>;

    /// Query the service status.
    fn status(&self) -> Result<ServiceStatus>;

    /// Restart the service (default: stop + start).
    fn restart(&self) -> Result<()> {
        self.stop()?;
        self.start()
    }
}

/// Factory: return the platform-appropriate service manager.
pub fn service_manager() -> Result<Box<dyn ServiceManager>> {
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(systemd::SystemdServiceManager::new()?))
    }
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(launchd::LaunchdServiceManager::new()?))
    }
    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(windows::WindowsServiceManager::new()?))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Err(LimError::Config("system service not supported on this platform".to_string()))
    }
}

// ── Linux systemd ──────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod systemd {
    use super::*;

    pub struct SystemdServiceManager {
        unit_dir: PathBuf,
    }

    impl SystemdServiceManager {
        pub fn new() -> Result<Self> {
            let home = std::env::var_os("HOME")
                .ok_or_else(|| LimError::Config("HOME not set".to_string()))?;
            let unit_dir = PathBuf::from(home).join(".config").join("systemd").join("user");
            Ok(Self { unit_dir })
        }

        fn unit_path(&self) -> PathBuf {
            self.unit_dir.join("lim.service")
        }

        fn systemctl(&self, args: &[&str]) -> Result<()> {
            let status = Command::new("systemctl")
                .args(["--user"])
                .args(args)
                .status()
                .map_err(|e| LimError::Config(format!("systemctl: {e}")))?;
            if !status.success() {
                return Err(LimError::Config(format!(
                    "systemctl --user {} failed (exit {:?})",
                    args.join(" "),
                    status.code()
                )));
            }
            Ok(())
        }

        pub(super) fn generate_unit(config: &ServiceConfig) -> String {
            let exec_start = std::iter::once(config.exec_path.to_string_lossy().as_ref())
                .chain(config.args.iter().map(|s| s.as_str()))
                .collect::<Vec<_>>()
                .join(" ");
            let restart = if config.auto_restart { "on-failure" } else { "no" };
            format!(
                "[Unit]\n\
                 Description={desc}\n\
                 After=network.target\n\
                 \n\
                 [Service]\n\
                 Type=simple\n\
                 ExecStart={exec_start}\n\
                 Restart={restart}\n\
                 RestartSec=5\n\
                 StandardOutput=append:%h/.lim/logs/lim.log\n\
                 StandardError=append:%h/.lim/logs/lim.err.log\n\
                 \n\
                 [Install]\n\
                 WantedBy=default.target\n",
                desc = config.description,
                exec_start = exec_start,
                restart = restart,
            )
        }
    }

    impl ServiceManager for SystemdServiceManager {
        fn install(&self, config: &ServiceConfig) -> Result<()> {
            std::fs::create_dir_all(&self.unit_dir)
                .map_err(|e| LimError::Config(format!("create unit dir: {e}")))?;
            let unit = Self::generate_unit(config);
            std::fs::write(self.unit_path(), unit)
                .map_err(|e| LimError::Config(format!("write unit file: {e}")))?;
            info!(path = %self.unit_path().display(), "systemd unit written");
            self.systemctl(&["daemon-reload"])?;
            self.systemctl(&["enable", "lim"])?;
            Ok(())
        }

        fn uninstall(&self) -> Result<()> {
            self.systemctl(&["disable", "lim"])?;
            let path = self.unit_path();
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| LimError::Config(format!("remove unit file: {e}")))?;
            }
            self.systemctl(&["daemon-reload"])?;
            info!("systemd unit removed");
            Ok(())
        }

        fn start(&self) -> Result<()> {
            self.systemctl(&["start", "lim"])
        }

        fn stop(&self) -> Result<()> {
            self.systemctl(&["stop", "lim"])
        }

        fn status(&self) -> Result<ServiceStatus> {
            let output = Command::new("systemctl")
                .args(["--user", "is-active", "lim"])
                .output()
                .map_err(|e| LimError::Config(format!("systemctl is-active: {e}")))?;
            let active = String::from_utf8_lossy(&output.stdout).trim() == "active";
            if active {
                // Try to get the PID.
                let pid = self.get_pid().ok().flatten();
                Ok(ServiceStatus::Running { pid })
            } else if String::from_utf8_lossy(&output.stderr).contains("inactive") {
                Ok(ServiceStatus::Stopped)
            } else {
                Ok(ServiceStatus::Unknown)
            }
        }
    }

    impl SystemdServiceManager {
        fn get_pid(&self) -> Result<Option<u32>> {
            let output = Command::new("systemctl")
                .args(["--user", "show", "lim", "--property=MainPID", "--value"])
                .output()
                .map_err(|e| LimError::Config(format!("systemctl show: {e}")))?;
            let pid_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if pid_str.is_empty() || pid_str == "0" {
                Ok(None)
            } else {
                pid_str
                    .parse::<u32>()
                    .map(Some)
                    .map_err(|e| LimError::Config(format!("parse PID: {e}")))
            }
        }
    }
}

// ── macOS launchd ──────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod launchd {
    use super::*;

    pub struct LaunchdServiceManager {
        agents_dir: PathBuf,
    }

    impl LaunchdServiceManager {
        pub fn new() -> Result<Self> {
            let home = std::env::var_os("HOME")
                .ok_or_else(|| LimError::Config("HOME not set".to_string()))?;
            let agents_dir = PathBuf::from(home).join("Library").join("LaunchAgents");
            Ok(Self { agents_dir })
        }

        fn plist_path(&self) -> PathBuf {
            self.agents_dir.join("ai.lim.serve.plist")
        }

        fn launchctl(&self, args: &[&str]) -> Result<()> {
            let status = Command::new("launchctl")
                .args(args)
                .status()
                .map_err(|e| LimError::Config(format!("launchctl: {e}")))?;
            if !status.success() {
                return Err(LimError::Config(format!(
                    "launchctl {} failed (exit {:?})",
                    args.join(" "),
                    status.code()
                )));
            }
            Ok(())
        }

        pub(super) fn generate_plist(config: &ServiceConfig) -> String {
            let program_args: Vec<String> =
                std::iter::once(config.exec_path.to_string_lossy().into_owned())
                    .chain(config.args.iter().cloned())
                    .collect();
            let args_xml = program_args
                .iter()
                .map(|a| format!("    <string>{}</string>", escape_xml(a)))
                .collect::<Vec<_>>()
                .join("\n");
            let keep_alive = if config.auto_restart { "<true/>" } else { "<false/>" };
            let log_dir = config.log_dir.display();

            let mut lines = Vec::new();
            lines.push(r#"<?xml version="1.0" encoding="UTF-8"?>"#.to_string());
            lines.push(r#"<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN""#.to_string());
            lines.push(r#"  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">"#.to_string());
            lines.push(r#"<plist version="1.0">"#.to_string());
            lines.push("<dict>".to_string());
            lines.push("  <key>Label</key>".to_string());
            lines.push(format!("  <string>{}</string>", SERVICE_LABEL));
            lines.push("  <key>ProgramArguments</key>".to_string());
            lines.push("  <array>".to_string());
            lines.push(args_xml);
            lines.push("  </array>".to_string());
            lines.push("  <key>RunAtLoad</key>".to_string());
            lines.push("  <true/>".to_string());
            lines.push("  <key>KeepAlive</key>".to_string());
            lines.push(format!("  {}", keep_alive));
            lines.push("  <key>StandardOutPath</key>".to_string());
            lines.push(format!("  <string>{}/lim.log</string>", log_dir));
            lines.push("  <key>StandardErrorPath</key>".to_string());
            lines.push(format!("  <string>{}/lim.err.log</string>", log_dir));
            lines.push("</dict>".to_string());
            lines.push("</plist>".to_string());
            lines.join("\n") + "\n"
        }
    }

    impl ServiceManager for LaunchdServiceManager {
        fn install(&self, config: &ServiceConfig) -> Result<()> {
            std::fs::create_dir_all(&self.agents_dir)
                .map_err(|e| LimError::Config(format!("create LaunchAgents dir: {e}")))?;
            let plist = Self::generate_plist(config);
            std::fs::write(self.plist_path(), plist)
                .map_err(|e| LimError::Config(format!("write plist: {e}")))?;
            info!(path = %self.plist_path().display(), "launchd plist written");
            self.launchctl(&["load", &self.plist_path().to_string_lossy()])?;
            Ok(())
        }

        fn uninstall(&self) -> Result<()> {
            let path = self.plist_path();
            if path.exists() {
                self.launchctl(&["unload", &path.to_string_lossy()])?;
                std::fs::remove_file(&path)
                    .map_err(|e| LimError::Config(format!("remove plist: {e}")))?;
            }
            info!("launchd plist removed");
            Ok(())
        }

        fn start(&self) -> Result<()> {
            self.launchctl(&["start", SERVICE_LABEL])
        }

        fn stop(&self) -> Result<()> {
            self.launchctl(&["stop", SERVICE_LABEL])
        }

        fn status(&self) -> Result<ServiceStatus> {
            // `launchctl list <label>` prints info and exits 0 if loaded.
            let output = Command::new("launchctl")
                .args(["list", SERVICE_LABEL])
                .output()
                .map_err(|e| LimError::Config(format!("launchctl list: {e}")))?;
            if !output.status.success() {
                // Not loaded — check if plist exists.
                if self.plist_path().exists() {
                    return Ok(ServiceStatus::Stopped);
                }
                return Ok(ServiceStatus::NotInstalled);
            }
            // Parse the PID from the output. `launchctl list <label>`
            // prints a table with PID in the first column, Status in
            // the second, Label in the third. A PID of "-" means not
            // running.
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 3 && parts[2] == SERVICE_LABEL {
                    let pid_str = parts[0];
                    if pid_str == "-" {
                        return Ok(ServiceStatus::Stopped);
                    }
                    if let Ok(pid) = pid_str.parse::<u32>() {
                        return Ok(ServiceStatus::Running { pid: Some(pid) });
                    }
                }
            }
            Ok(ServiceStatus::Unknown)
        }
    }
}

// ── Windows Service (SCM) ──────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod windows {
    use super::*;

    /// Service name used with `sc.exe`.
    const SERVICE_NAME: &str = "lim";

    /// Manages lim as a Windows service via the Service Control Manager.
    ///
    /// All operations shell out to `sc.exe` rather than calling the SCM
    /// API directly, keeping the implementation free of `unsafe` Win32
    /// code. `sc.exe` requires a space after each `param=`, e.g.
    /// `sc create lim binPath= "..." start= auto`.
    pub struct WindowsServiceManager;

    impl WindowsServiceManager {
        /// Create a new manager. No state is required — the service name
        /// is fixed and `sc.exe` is invoked on demand.
        pub fn new() -> Result<Self> {
            Ok(Self)
        }

        /// Run `sc.exe` with the given args, returning an error on
        /// non-zero exit.
        fn sc(&self, args: &[&str]) -> Result<()> {
            let status = Command::new("sc")
                .args(args)
                .status()
                .map_err(|e| LimError::Config(format!("sc.exe: {e}")))?;
            if !status.success() {
                return Err(LimError::Config(format!(
                    "sc {} failed (exit {:?})",
                    args.join(" "),
                    status.code()
                )));
            }
            Ok(())
        }

        /// Build the `binPath=` value: the executable path followed by the
        /// serve args, each quoted only if it contains spaces.
        fn bin_path(config: &ServiceConfig) -> String {
            let mut s = quote_if_needed(&config.exec_path.to_string_lossy());
            for arg in &config.args {
                s.push(' ');
                s.push_str(&quote_if_needed(arg));
            }
            s
        }
    }

    impl ServiceManager for WindowsServiceManager {
        fn install(&self, config: &ServiceConfig) -> Result<()> {
            let bin_path = Self::bin_path(config);
            let start = if config.auto_restart { "auto" } else { "demand" };
            // `binPath=` is its own token so sc treats the following
            // (auto-quoted by std::Command) token as the full image path
            // plus serve args.
            self.sc(&["create", SERVICE_NAME, "binPath=", &bin_path, "start=", start])?;
            info!(name = SERVICE_NAME, "windows service created");
            Ok(())
        }

        fn uninstall(&self) -> Result<()> {
            // Best-effort stop; ignore errors (service may already be stopped).
            let _ = self.sc(&["stop", SERVICE_NAME]);
            self.sc(&["delete", SERVICE_NAME])?;
            info!("windows service removed");
            Ok(())
        }

        fn start(&self) -> Result<()> {
            self.sc(&["start", SERVICE_NAME])
        }

        fn stop(&self) -> Result<()> {
            self.sc(&["stop", SERVICE_NAME])
        }

        fn status(&self) -> Result<ServiceStatus> {
            let output = Command::new("sc")
                .args(["query", SERVICE_NAME])
                .output()
                .map_err(|e| LimError::Config(format!("sc query: {e}")))?;
            // `sc query` exits non-zero when the service is not installed.
            if !output.status.success() {
                return Ok(ServiceStatus::NotInstalled);
            }
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let line = line.trim();
                // Lines look like `STATE              : 4  RUNNING`.
                if let Some(rest) = line.strip_prefix("STATE") {
                    let rest = rest.trim_start().strip_prefix(':').unwrap_or(rest).trim();
                    if rest.contains("RUNNING") {
                        return Ok(ServiceStatus::Running { pid: None });
                    }
                    return Ok(ServiceStatus::Stopped);
                }
            }
            Ok(ServiceStatus::Unknown)
        }
    }

    /// Quote a path/argument with double quotes if it contains spaces,
    /// matching `sc.exe`'s `binPath=` parsing rules.
    fn quote_if_needed(s: &str) -> String {
        if s.contains(' ') { format!("\"{s}\"") } else { s.to_string() }
    }
}

/// Escape special XML characters in a string for plist generation.
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::ServeOptions;
    use std::path::PathBuf;

    #[test]
    #[cfg(target_os = "macos")]
    fn launchd_plist_generation() {
        let config = ServiceConfig {
            name: "lim".into(),
            description: "lim agent server".into(),
            exec_path: PathBuf::from("/usr/local/bin/lim"),
            args: vec!["serve".into(), "--protocol".into(), "mcp".into()],
            working_dir: None,
            log_dir: PathBuf::from("/tmp/lim-logs"),
            auto_restart: true,
        };
        let plist = launchd::LaunchdServiceManager::generate_plist(&config);
        assert!(plist.contains("<string>ai.lim.serve</string>"));
        assert!(plist.contains("<string>/usr/local/bin/lim</string>"));
        assert!(plist.contains("<string>serve</string>"));
        assert!(plist.contains("<string>--protocol</string>"));
        assert!(plist.contains("<string>mcp</string>"));
        assert!(plist.contains("<true/>")); // KeepAlive
        assert!(plist.contains("/tmp/lim-logs/lim.log"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn systemd_unit_generation() {
        let config = ServiceConfig {
            name: "lim".into(),
            description: "lim agent server".into(),
            exec_path: PathBuf::from("/usr/local/bin/lim"),
            args: vec!["serve".into(), "--protocol".into(), "mcp".into()],
            working_dir: None,
            log_dir: PathBuf::from("/tmp/lim-logs"),
            auto_restart: true,
        };
        let unit = systemd::SystemdServiceManager::generate_unit(&config);
        assert!(unit.contains("Description=lim agent server"));
        assert!(unit.contains("ExecStart=/usr/local/bin/lim serve --protocol mcp"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn escape_xml_handles_special_chars() {
        assert_eq!(escape_xml("a&b"), "a&amp;b");
        assert_eq!(escape_xml("a<b>c"), "a&lt;b&gt;c");
        assert_eq!(escape_xml("\"quoted\""), "&quot;quoted&quot;");
    }

    #[test]
    fn service_config_from_options() {
        let opts = ServeOptions::default();
        let config = ServiceConfig::from_current_exe(&opts, PathBuf::from("/tmp/logs"));
        assert!(config.is_ok());
        let config = config.unwrap();
        assert_eq!(config.name, "lim");
        assert!(config.args.contains(&"serve".to_string()));
        // All is default, no --protocol flag emitted.
        assert!(!config.args.contains(&"--goal-daemon".to_string()));
    }
}
