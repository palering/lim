//! Unix domain socket transport (T1 cross-process, Linux/macOS).
//!
//! Uses `tokio::net::UnixListener` / `UnixStream` for high-performance
//! local IPC. Same length-prefixed framing as the TCP transport.
//!
//! ## Socket file lifecycle
//!
//! - `bind()` creates the socket file at the given path.
//! - On drop / `shutdown()`, the server removes the socket file.
//! - If the file already exists (stale socket from a crashed process),
//!   `bind()` unlinks it first.
//!
//! ## Permissions
//!
//! The socket file is created with mode `0600` (owner read/write only)
//! to prevent other users from connecting.

use std::path::PathBuf;
use std::sync::Arc;

use lim_core::error::{LimError, Result};
use lim_core::message::AgentMessage;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tracing::debug;

use crate::protocol::ProtocolAdapter;

/// Maximum frame size (16 MiB).
const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// A framed Unix socket stream transport — client side.
pub struct UnixStreamTransport {
    stream: UnixStream,
    adapter: Arc<dyn ProtocolAdapter>,
}

impl UnixStreamTransport {
    /// Connect to a Unix socket server at the given path.
    pub async fn connect(path: PathBuf, adapter: Arc<dyn ProtocolAdapter>) -> Result<Self> {
        let stream = UnixStream::connect(&path)
            .await
            .map_err(|e| LimError::Config(format!("unix connect {}: {e}", path.display())))?;
        debug!(path = %path.display(), "unix transport connected");
        Ok(Self { stream, adapter })
    }

    /// Wrap an existing `UnixStream`.
    pub fn from_stream(stream: UnixStream, adapter: Arc<dyn ProtocolAdapter>) -> Self {
        Self { stream, adapter }
    }

    /// Write a single framed message.
    pub async fn write_message(&mut self, msg: &AgentMessage) -> Result<()> {
        let payload = self.adapter.encode(msg).await?;
        let len = payload.len();
        if len > MAX_FRAME_SIZE {
            return Err(LimError::Config(format!("frame too large: {len} > {MAX_FRAME_SIZE}")));
        }
        let len_bytes = (len as u32).to_be_bytes();
        self.stream.write_all(&len_bytes).await.map_err(io_err)?;
        self.stream.write_all(&payload).await.map_err(io_err)?;
        self.stream.flush().await.map_err(io_err)?;
        Ok(())
    }

    /// Read a single framed message.
    pub async fn read_message(&mut self) -> Result<AgentMessage> {
        let mut len_buf = [0u8; 4];
        self.stream.read_exact(&mut len_buf).await.map_err(io_err)?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > MAX_FRAME_SIZE {
            return Err(LimError::Config(format!("frame too large: {len} > {MAX_FRAME_SIZE}")));
        }
        if len == 0 {
            return Err(LimError::Config("empty frame (len=0)".to_string()));
        }
        let mut payload = vec![0u8; len];
        self.stream.read_exact(&mut payload).await.map_err(io_err)?;
        self.adapter.decode(&payload).await
    }
}

/// A Unix socket server that accepts framed connections.
///
/// On drop, removes the socket file (if still present).
pub struct UnixSocketServer {
    listener: UnixListener,
    adapter: Arc<dyn ProtocolAdapter>,
    path: PathBuf,
}

impl UnixSocketServer {
    /// Bind to a Unix socket path.
    ///
    /// If a stale socket file exists at the path (from a crashed process),
    /// it is unlinked first.
    pub async fn bind(path: PathBuf, adapter: Arc<dyn ProtocolAdapter>) -> Result<Self> {
        // Remove stale socket file
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let listener = UnixListener::bind(&path)
            .map_err(|e| LimError::Config(format!("unix bind {}: {e}", path.display())))?;

        // Set socket file permissions to 0600 (owner only)
        set_socket_permissions(&path, 0o600)?;

        debug!(path = %path.display(), "unix server bound");
        Ok(Self { listener, adapter, path })
    }

    /// Accept the next incoming connection.
    pub async fn accept(&self) -> Result<UnixStreamTransport> {
        let (stream, _peer) = self.listener.accept().await.map_err(io_err)?;
        debug!(path = %self.path.display(), "unix server accepted connection");
        Ok(UnixStreamTransport::from_stream(stream, Arc::clone(&self.adapter)))
    }

    /// The socket path this server is bound to.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Drop for UnixSocketServer {
    fn drop(&mut self) {
        // Clean up socket file on drop
        let _ = std::fs::remove_file(&self.path);
        debug!(path = %self.path.display(), "unix server socket file removed");
    }
}

/// Set file permissions on the socket path (Unix only).
fn set_socket_permissions(path: &std::path::Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(path, perms)
        .map_err(|e| LimError::Config(format!("set permissions {}: {e}", path.display())))?;
    Ok(())
}

fn io_err(e: std::io::Error) -> LimError {
    LimError::Config(format!("unix io: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::LimNativeAdapter;
    use lim_core::content::ContentBlock;
    use lim_core::message::Role;

    fn temp_socket_path() -> PathBuf {
        let dir = tempfile::tempdir().unwrap();
        dir.keep().join("test.sock")
    }

    #[tokio::test]
    async fn unix_roundtrip_single_message() {
        let adapter = Arc::new(LimNativeAdapter::new());
        let path = temp_socket_path();
        let server = UnixSocketServer::bind(path.clone(), adapter.clone()).await.unwrap();

        let mut client = UnixStreamTransport::connect(path.clone(), adapter.clone()).await.unwrap();
        let mut server_conn = server.accept().await.unwrap();

        // Client → Server
        let msg = AgentMessage::user("hello over unix socket");
        client.write_message(&msg).await.unwrap();
        let received = server_conn.read_message().await.unwrap();
        assert_eq!(received.role, Role::User);

        // Server → Client
        let reply =
            AgentMessage::assistant(vec![ContentBlock::Text { text: "got it".to_string() }]);
        server_conn.write_message(&reply).await.unwrap();
        let received = client.read_message().await.unwrap();
        assert_eq!(received.role, Role::Assistant);
    }

    #[tokio::test]
    async fn unix_roundtrip_multiple_messages() {
        let adapter = Arc::new(LimNativeAdapter::new());
        let path = temp_socket_path();
        let server = UnixSocketServer::bind(path.clone(), adapter.clone()).await.unwrap();

        let mut client = UnixStreamTransport::connect(path, adapter.clone()).await.unwrap();
        let mut server_conn = server.accept().await.unwrap();

        for i in 0..5 {
            let msg = AgentMessage::user(format!("message {i}"));
            client.write_message(&msg).await.unwrap();
            let received = server_conn.read_message().await.unwrap();
            match &received.content[0] {
                ContentBlock::Text { text } => {
                    assert_eq!(text, &format!("message {i}"));
                }
                other => panic!("expected Text, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn unix_stale_socket_replaced() {
        let adapter = Arc::new(LimNativeAdapter::new());
        let path = temp_socket_path();

        // Create a dummy file at the socket path (simulating stale socket)
        std::fs::write(&path, b"stale").unwrap();

        // bind() should succeed by removing the stale file
        let server = UnixSocketServer::bind(path.clone(), adapter.clone()).await.unwrap();
        let client = UnixStreamTransport::connect(path, adapter).await.unwrap();
        drop(client);
        drop(server);
    }

    #[tokio::test]
    async fn unix_server_drop_cleans_socket() {
        let adapter = Arc::new(LimNativeAdapter::new());
        let path = temp_socket_path();

        let server = UnixSocketServer::bind(path.clone(), adapter).await.unwrap();
        assert!(path.exists());

        drop(server);
        assert!(!path.exists());
    }
}
