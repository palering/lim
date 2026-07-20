//! Platform-aware local IPC endpoint abstraction (T1 cross-process).
//!
//! `LocalEndpoint` abstracts over the three local IPC mechanisms used by lim:
//!
//! | Variant | Platform | tokio support |
//! |---|---|---|
//! | `Unix(PathBuf)` | Linux / macOS | `tokio::net::UnixListener` |
//! | `NamedPipe(String)` | Windows | `tokio::net::windows::named_pipe` |
//! | `TcpLoopback(SocketAddr)` | All (fallback) | `tokio::net::TcpListener` |
//!
//! Each platform has a preferred variant (selected by `auto()`), with
//! TCP loopback as the universal fallback when the preferred mechanism
//! is unavailable or fails to bind.
//!
//! See `docs/architecture/transport-layer.md` §四 for the design rationale.

use std::net::SocketAddr;
use std::path::PathBuf;

/// Platform-aware local IPC endpoint.
///
/// Represents the address a T1 transport binds to or connects to.
/// The `auto()` constructor picks the best variant for the current platform.
#[derive(Debug, Clone)]
pub enum LocalEndpoint {
    /// Unix domain socket path (Linux/macOS preferred).
    ///
    /// Convention: `$XDG_RUNTIME_DIR/lim/<name>.sock` on Linux,
    /// `$TMPDIR/lim/<name>.sock` on macOS.
    Unix(PathBuf),

    /// Windows named pipe name (Windows preferred).
    ///
    /// Format: `\\.\pipe\<name>`.
    NamedPipe(String),

    /// TCP loopback address (cross-platform fallback).
    ///
    /// Binds to `127.0.0.1:<port>` (or `[::1]:<port>` for IPv6).
    TcpLoopback(SocketAddr),
}

impl LocalEndpoint {
    /// Auto-detect the best endpoint for the current platform.
    ///
    /// - Unix (Linux/macOS): Unix domain socket at the platform-specific path.
    /// - Windows: Named pipe at `\\.\pipe\<app_name>`.
    pub fn auto(app_name: &str) -> Self {
        #[cfg(unix)]
        {
            Self::Unix(default_unix_socket_path(app_name))
        }
        #[cfg(windows)]
        {
            Self::NamedPipe(format!(r"\\.\pipe\{}", app_name))
        }
    }

    /// Parse an endpoint URL string into a `LocalEndpoint`.
    ///
    /// Supported schemes:
    /// - `auto` — platform default (ignores the rest of the URL)
    /// - `unix://<path>` — Unix domain socket
    /// - `pipe://<name>` — Windows named pipe
    /// - `tcp://<addr>:<port>` — TCP loopback
    pub fn parse(s: &str) -> Result<Self, EndpointParseError> {
        if s == "auto" {
            return Ok(Self::auto("lim"));
        }
        if let Some(rest) = s.strip_prefix("unix://") {
            return Ok(Self::Unix(PathBuf::from(rest)));
        }
        if let Some(rest) = s.strip_prefix("pipe://") {
            return Ok(Self::NamedPipe(rest.to_string()));
        }
        if let Some(rest) = s.strip_prefix("tcp://") {
            let addr: SocketAddr = rest.parse().map_err(|e: std::net::AddrParseError| {
                EndpointParseError::InvalidTcp(rest.to_string(), e.to_string())
            })?;
            return Ok(Self::TcpLoopback(addr));
        }
        // Heuristic: if it ends with .sock, treat as Unix socket path
        if s.ends_with(".sock") {
            return Ok(Self::Unix(PathBuf::from(s)));
        }
        Err(EndpointParseError::UnknownScheme(s.to_string()))
    }

    /// Human-readable description for logging.
    pub fn describe(&self) -> String {
        match self {
            Self::Unix(p) => format!("unix:{}", p.display()),
            Self::NamedPipe(n) => format!("pipe:{}", n),
            Self::TcpLoopback(a) => format!("tcp:{}", a),
        }
    }
}

impl std::fmt::Display for LocalEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unix(p) => write!(f, "unix://{}", p.display()),
            Self::NamedPipe(n) => write!(f, "pipe://{}", n),
            Self::TcpLoopback(a) => write!(f, "tcp://{}", a),
        }
    }
}

/// Error from parsing an endpoint URL string.
#[derive(Debug, thiserror::Error)]
pub enum EndpointParseError {
    #[error("unknown endpoint scheme: {0} (expected auto/unix/pipe/tcp)")]
    UnknownScheme(String),
    #[error("invalid TCP address '{0}': {1}")]
    InvalidTcp(String, String),
}

/// Compute the default Unix domain socket path for an app.
///
/// - Linux: `$XDG_RUNTIME_DIR/lim/<app>.sock` (fallback `/tmp/lim-<user>/<app>.sock`)
/// - macOS: `$TMPDIR/lim/<app>.sock` (fallback `/tmp/lim-<user>/<app>.sock`)
#[cfg(unix)]
fn default_unix_socket_path(app_name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    // Determine base directory
    #[cfg(target_os = "linux")]
    let base = std::env::var("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|_| {
        // Fallback: /tmp/lim-<uid>
        let uid = unsafe { libc::getuid() };
        PathBuf::from(format!("/tmp/lim-{}", uid))
    });

    #[cfg(target_os = "macos")]
    let base = std::env::var("TMPDIR").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/tmp"));

    let dir = base.join("lim");
    // Ensure directory exists with secure permissions (0700)
    if !dir.exists() {
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(metadata) = std::fs::metadata(&dir) {
            let mut perms = metadata.permissions();
            perms.set_mode(0o700);
            let _ = std::fs::set_permissions(&dir, perms);
        }
    }

    dir.join(format!("{}.sock", app_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_auto() {
        let ep = LocalEndpoint::parse("auto").unwrap();
        // On unix it's Unix, on windows it's NamedPipe
        #[cfg(unix)]
        assert!(matches!(ep, LocalEndpoint::Unix(_)));
        #[cfg(windows)]
        assert!(matches!(ep, LocalEndpoint::NamedPipe(_)));
    }

    #[test]
    fn parse_unix_scheme() {
        let ep = LocalEndpoint::parse("unix:///tmp/lim/test.sock").unwrap();
        assert!(matches!(ep, LocalEndpoint::Unix(_)));
        if let LocalEndpoint::Unix(p) = ep {
            assert_eq!(p, PathBuf::from("/tmp/lim/test.sock"));
        }
    }

    #[test]
    fn parse_pipe_scheme() {
        let ep = LocalEndpoint::parse("pipe://lim-daemon").unwrap();
        assert!(matches!(ep, LocalEndpoint::NamedPipe(_)));
        if let LocalEndpoint::NamedPipe(n) = ep {
            assert_eq!(n, "lim-daemon");
        }
    }

    #[test]
    fn parse_tcp_scheme() {
        let ep = LocalEndpoint::parse("tcp://127.0.0.1:7878").unwrap();
        assert!(matches!(ep, LocalEndpoint::TcpLoopback(_)));
        if let LocalEndpoint::TcpLoopback(addr) = ep {
            assert_eq!(addr.port(), 7878);
        }
    }

    #[test]
    fn parse_sock_heuristic() {
        let ep = LocalEndpoint::parse("/tmp/lim/daemon.sock").unwrap();
        assert!(matches!(ep, LocalEndpoint::Unix(_)));
    }

    #[test]
    fn parse_unknown_scheme_fails() {
        assert!(LocalEndpoint::parse("foo://bar").is_err());
    }

    #[test]
    fn describe_and_display() {
        let ep = LocalEndpoint::TcpLoopback("127.0.0.1:7878".parse().unwrap());
        assert!(ep.describe().starts_with("tcp:"));
        assert_eq!(ep.to_string(), "tcp://127.0.0.1:7878");
    }

    #[cfg(unix)]
    #[test]
    fn auto_creates_unix_socket_path() {
        let ep = LocalEndpoint::auto("test-app");
        assert!(matches!(ep, LocalEndpoint::Unix(_)));
        if let LocalEndpoint::Unix(p) = ep {
            assert!(p.to_string_lossy().ends_with("test-app.sock"));
        }
    }

    #[cfg(windows)]
    #[test]
    fn auto_creates_named_pipe() {
        let ep = LocalEndpoint::auto("test-app");
        assert!(matches!(ep, LocalEndpoint::NamedPipe(_)));
        if let LocalEndpoint::NamedPipe(n) = ep {
            assert_eq!(n, r"\\.\pipe\test-app");
        }
    }
}
