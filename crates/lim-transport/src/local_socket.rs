//! Unified local socket transport — abstracts over Unix socket and Named Pipe.
//!
//! `LocalSocketTransport` selects the platform-native IPC mechanism at compile
//! time: Unix domain sockets on Linux/macOS, Named Pipes on Windows. Callers
//! build a [`LocalSocketEndpoint`] from a URL string (`unix://<path>` or
//! `pipe://<name>`) and don't need `#[cfg]` gates in their own code.
//!
//! The underlying platform transports live in [`crate::unix_socket`] (Unix)
//! and [`crate::named_pipe`] (Windows); this module only delegates
//! bind/connect to the right implementation. Use the `as_unix_*` /
//! `as_named_pipe_*` accessors to reach the platform transport for I/O.
//!
//! See `docs/architecture/transport-layer.md` §四 for the design rationale.

use std::sync::Arc;

use camino::Utf8PathBuf;
use lim_core::error::{LimError, Result};

use crate::protocol::ProtocolAdapter;

/// Platform-specific local socket endpoint.
///
/// Construct with [`LocalSocketEndpoint::parse`] from a URL string.
#[derive(Debug, Clone)]
pub enum LocalSocketEndpoint {
    /// Unix domain socket path (Linux/macOS).
    Unix(Utf8PathBuf),
    /// Windows named pipe name (e.g. `\\.\pipe\lim-daemon`).
    NamedPipe(String),
}

impl LocalSocketEndpoint {
    /// Parse an endpoint URL string.
    ///
    /// Supported schemes:
    /// - `unix://<path>` — Unix domain socket
    /// - `pipe://<name>` — Windows named pipe
    ///
    /// Any other input (unknown scheme, bare path) is rejected.
    pub fn parse(s: &str) -> Result<Self> {
        if let Some(rest) = s.strip_prefix("unix://") {
            return Ok(Self::Unix(Utf8PathBuf::from(rest)));
        }
        if let Some(rest) = s.strip_prefix("pipe://") {
            return Ok(Self::NamedPipe(rest.to_string()));
        }
        Err(LimError::Config(format!(
            "invalid local socket endpoint '{s}': expected unix:// or pipe:// scheme"
        )))
    }
}

impl std::fmt::Display for LocalSocketEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unix(p) => write!(f, "unix://{}", p),
            Self::NamedPipe(n) => write!(f, "pipe://{}", n),
        }
    }
}

#[cfg(unix)]
enum UnixInner {
    Server(crate::unix_socket::UnixSocketServer),
    Client(crate::unix_socket::UnixStreamTransport),
}

#[cfg(windows)]
enum WindowsInner {
    Server(crate::named_pipe::NamedPipeServer),
    Client(crate::named_pipe::NamedPipeClientTransport),
}

/// Unified local socket transport wrapping the platform-native IPC mechanism.
///
/// Created via [`LocalSocketTransport::bind`] (server side) or
/// [`LocalSocketTransport::connect`] (client side). Inspect with
/// [`Self::is_server`] / [`Self::is_client`] and access the underlying
/// platform transport via the `as_*` accessors.
pub struct LocalSocketTransport {
    #[cfg(unix)]
    inner: UnixInner,
    #[cfg(windows)]
    inner: WindowsInner,
}

impl LocalSocketTransport {
    /// Bind a server to the given endpoint.
    ///
    /// On Unix this creates a `UnixSocketServer`; on Windows a `NamedPipeServer`.
    /// The endpoint variant must match the current platform — a `NamedPipe`
    /// endpoint on Unix (or a `Unix` endpoint on Windows) is rejected.
    pub async fn bind(
        endpoint: LocalSocketEndpoint,
        adapter: Arc<dyn ProtocolAdapter>,
    ) -> Result<Self> {
        #[cfg(unix)]
        {
            let path = match endpoint {
                LocalSocketEndpoint::Unix(p) => p.into_std_path_buf(),
                LocalSocketEndpoint::NamedPipe(_) => {
                    return Err(LimError::Config(format!(
                        "named pipe endpoint not supported on unix (use unix://): {endpoint}"
                    )));
                }
            };
            let server = crate::unix_socket::UnixSocketServer::bind(path, adapter).await?;
            Ok(Self { inner: UnixInner::Server(server) })
        }
        #[cfg(windows)]
        {
            let name = match endpoint {
                LocalSocketEndpoint::NamedPipe(n) => n,
                LocalSocketEndpoint::Unix(_) => {
                    return Err(LimError::Config(format!(
                        "unix endpoint not supported on windows (use pipe://): {endpoint}"
                    )));
                }
            };
            let server = crate::named_pipe::NamedPipeServer::create(&name, adapter)?;
            Ok(Self { inner: WindowsInner::Server(server) })
        }
    }

    /// Connect a client to the given endpoint.
    ///
    /// On Unix this connects a `UnixStreamTransport`; on Windows a
    /// `NamedPipeClientTransport`. The endpoint variant must match the
    /// current platform.
    pub async fn connect(
        endpoint: LocalSocketEndpoint,
        adapter: Arc<dyn ProtocolAdapter>,
    ) -> Result<Self> {
        #[cfg(unix)]
        {
            let path = match endpoint {
                LocalSocketEndpoint::Unix(p) => p.into_std_path_buf(),
                LocalSocketEndpoint::NamedPipe(_) => {
                    return Err(LimError::Config(format!(
                        "named pipe endpoint not supported on unix (use unix://): {endpoint}"
                    )));
                }
            };
            let client = crate::unix_socket::UnixStreamTransport::connect(path, adapter).await?;
            Ok(Self { inner: UnixInner::Client(client) })
        }
        #[cfg(windows)]
        {
            let name = match endpoint {
                LocalSocketEndpoint::NamedPipe(n) => n,
                LocalSocketEndpoint::Unix(_) => {
                    return Err(LimError::Config(format!(
                        "unix endpoint not supported on windows (use pipe://): {endpoint}"
                    )));
                }
            };
            let client =
                crate::named_pipe::NamedPipeClientTransport::connect(&name, adapter).await?;
            Ok(Self { inner: WindowsInner::Client(client) })
        }
    }

    /// The platform name this transport is using: `"unix"` or `"named_pipe"`.
    pub fn platform_name() -> &'static str {
        #[cfg(unix)]
        {
            "unix"
        }
        #[cfg(windows)]
        {
            "named_pipe"
        }
    }

    /// Whether this transport is a bound server.
    pub fn is_server(&self) -> bool {
        #[cfg(unix)]
        {
            matches!(self.inner, UnixInner::Server(_))
        }
        #[cfg(windows)]
        {
            matches!(self.inner, WindowsInner::Server(_))
        }
    }

    /// Whether this transport is a connected client.
    pub fn is_client(&self) -> bool {
        #[cfg(unix)]
        {
            matches!(self.inner, UnixInner::Client(_))
        }
        #[cfg(windows)]
        {
            matches!(self.inner, WindowsInner::Client(_))
        }
    }

    /// Borrow the underlying Unix socket server, if this is a bound Unix server.
    #[cfg(unix)]
    pub fn as_unix_server(&self) -> Option<&crate::unix_socket::UnixSocketServer> {
        match &self.inner {
            UnixInner::Server(s) => Some(s),
            _ => None,
        }
    }

    /// Borrow the underlying Unix stream client, if this is a connected Unix client.
    #[cfg(unix)]
    pub fn as_unix_client(&self) -> Option<&crate::unix_socket::UnixStreamTransport> {
        match &self.inner {
            UnixInner::Client(c) => Some(c),
            _ => None,
        }
    }

    /// Mutably borrow the underlying Unix stream client, for read/write I/O.
    #[cfg(unix)]
    pub fn as_unix_client_mut(&mut self) -> Option<&mut crate::unix_socket::UnixStreamTransport> {
        match &mut self.inner {
            UnixInner::Client(c) => Some(c),
            _ => None,
        }
    }

    /// Borrow the underlying named pipe server, if this is a bound pipe server.
    #[cfg(windows)]
    pub fn as_named_pipe_server(&self) -> Option<&crate::named_pipe::NamedPipeServer> {
        match &self.inner {
            WindowsInner::Server(s) => Some(s),
            _ => None,
        }
    }

    /// Borrow the underlying named pipe client, if this is a connected pipe client.
    #[cfg(windows)]
    pub fn as_named_pipe_client(&self) -> Option<&crate::named_pipe::NamedPipeClientTransport> {
        match &self.inner {
            WindowsInner::Client(c) => Some(c),
            _ => None,
        }
    }

    /// Mutably borrow the underlying named pipe client, for read/write I/O.
    #[cfg(windows)]
    pub fn as_named_pipe_client_mut(
        &mut self,
    ) -> Option<&mut crate::named_pipe::NamedPipeClientTransport> {
        match &mut self.inner {
            WindowsInner::Client(c) => Some(c),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::LimNativeAdapter;
    use lim_core::message::Role;

    #[test]
    fn parse_unix_endpoint() {
        let ep = LocalSocketEndpoint::parse("unix:///tmp/lim/test.sock").unwrap();
        assert!(matches!(ep, LocalSocketEndpoint::Unix(_)));
        if let LocalSocketEndpoint::Unix(p) = ep {
            assert_eq!(p.as_str(), "/tmp/lim/test.sock");
        }
    }

    #[test]
    fn parse_pipe_endpoint() {
        let ep = LocalSocketEndpoint::parse("pipe://lim-daemon").unwrap();
        assert!(matches!(ep, LocalSocketEndpoint::NamedPipe(_)));
        if let LocalSocketEndpoint::NamedPipe(n) = ep {
            assert_eq!(n, "lim-daemon");
        }
    }

    #[test]
    fn parse_invalid_endpoint_fails() {
        assert!(LocalSocketEndpoint::parse("foo://bar").is_err());
        // Bare paths and unknown schemes are rejected (no heuristic, unlike LocalEndpoint).
        assert!(LocalSocketEndpoint::parse("/tmp/lim/x.sock").is_err());
        assert!(LocalSocketEndpoint::parse("tcp://127.0.0.1:8080").is_err());
    }

    #[test]
    fn endpoint_display_roundtrips() {
        let ep = LocalSocketEndpoint::Unix(Utf8PathBuf::from("/tmp/lim/x.sock"));
        assert_eq!(ep.to_string(), "unix:///tmp/lim/x.sock");
        let ep = LocalSocketEndpoint::NamedPipe("lim-daemon".to_string());
        assert_eq!(ep.to_string(), "pipe://lim-daemon");
    }

    #[test]
    fn platform_name_matches_cfg() {
        #[cfg(unix)]
        assert_eq!(LocalSocketTransport::platform_name(), "unix");
        #[cfg(windows)]
        assert_eq!(LocalSocketTransport::platform_name(), "named_pipe");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_bind_connect_roundtrip() {
        let adapter: Arc<dyn ProtocolAdapter> = Arc::new(LimNativeAdapter::new());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.keep().join("local-socket.sock");
        let endpoint = LocalSocketEndpoint::Unix(
            Utf8PathBuf::from_path_buf(path).expect("temp path is utf-8"),
        );

        let server =
            LocalSocketTransport::bind(endpoint.clone(), Arc::clone(&adapter)).await.unwrap();
        assert!(server.is_server());
        assert!(server.as_unix_server().is_some());

        let mut client =
            LocalSocketTransport::connect(endpoint, Arc::clone(&adapter)).await.unwrap();
        assert!(client.is_client());

        // Accept the client connection on the server side.
        let mut server_conn = server.as_unix_server().unwrap().accept().await.unwrap();

        // Client -> Server
        let msg = lim_core::message::AgentMessage::user("hello via local socket");
        client.as_unix_client_mut().unwrap().write_message(&msg).await.unwrap();
        let received = server_conn.read_message().await.unwrap();
        assert_eq!(received.role, Role::User);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_bind_rejects_named_pipe_endpoint() {
        let adapter: Arc<dyn ProtocolAdapter> = Arc::new(LimNativeAdapter::new());
        let ep = LocalSocketEndpoint::NamedPipe("lim-daemon".to_string());
        let err = LocalSocketTransport::bind(ep, adapter).await;
        assert!(err.is_err());
    }
}
