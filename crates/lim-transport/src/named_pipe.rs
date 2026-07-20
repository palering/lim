//! Windows Named Pipe transport (T1 cross-process, Windows only).
//!
//! Uses `tokio::net::windows::named_pipe` for local IPC on Windows.
//! Same length-prefixed framing as the TCP and Unix socket transports.
//!
//! ## Pipe naming
//!
//! Pipe names follow the `\\.\pipe\<name>` convention, e.g.
//! `\\.\pipe\lim-daemon`.
//!
//! ## Notes
//!
//! - Named pipes don't have a "socket file" to clean up — the pipe
//!   instance is removed when the server handle is dropped.
//! - Unlike Unix sockets, the server must create a pipe instance before
//!   the client connects. `NamedPipeServer` is created in a "disconnected"
//!   state and must call `connect()` to wait for a client.

use std::sync::Arc;

use lim_core::error::{LimError, Result};
use lim_core::message::AgentMessage;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use tracing::debug;

use crate::protocol::ProtocolAdapter;

/// Maximum frame size (16 MiB).
const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// A framed Named Pipe client transport.
pub struct NamedPipeClientTransport {
    stream: NamedPipeClient,
    adapter: Arc<dyn ProtocolAdapter>,
}

impl NamedPipeClientTransport {
    /// Connect to a named pipe server.
    ///
    /// `pipe_name` should be the full path, e.g. `\\.\pipe\lim-daemon`.
    pub async fn connect(pipe_name: &str, adapter: Arc<dyn ProtocolAdapter>) -> Result<Self> {
        let stream = ClientOptions::new()
            .open(pipe_name)
            .map_err(|e| LimError::Config(format!("pipe connect {pipe_name}: {e}")))?;
        debug!(pipe = %pipe_name, "named pipe client connected");
        Ok(Self { stream, adapter })
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

/// A named pipe server that accepts framed connections.
pub struct NamedPipeServer {
    pipe: NamedPipeServer,
    adapter: Arc<dyn ProtocolAdapter>,
    pipe_name: String,
}

impl NamedPipeServer {
    /// Create a named pipe server instance.
    ///
    /// `pipe_name` should be the full path, e.g. `\\.\pipe\lim-daemon`.
    ///
    /// The pipe is created in a disconnected state — call `connect()` to
    /// wait for a client to connect.
    pub fn create(pipe_name: &str, adapter: Arc<dyn ProtocolAdapter>) -> Result<Self> {
        let pipe = ServerOptions::new()
            .first_pipe_instance(true)
            .create(pipe_name)
            .map_err(|e| LimError::Config(format!("pipe create {pipe_name}: {e}")))?;
        debug!(pipe = %pipe_name, "named pipe server created");
        Ok(Self { pipe, adapter, pipe_name: pipe_name.to_string() })
    }

    /// Wait for a client to connect to this pipe instance.
    ///
    /// Consumes the server and returns a connected transport.
    pub async fn connect(self) -> Result<NamedPipeClientTransport> {
        self.pipe
            .connect()
            .await
            .map_err(|e| LimError::Config(format!("pipe accept {}: {e}", self.pipe_name)))?;
        debug!(pipe = %self.pipe_name, "named pipe client connected");
        Ok(NamedPipeClientTransport { stream: self.pipe, adapter: self.adapter })
    }

    /// The pipe name this server is bound to.
    pub fn pipe_name(&self) -> &str {
        &self.pipe_name
    }
}

fn io_err(e: std::io::Error) -> LimError {
    LimError::Config(format!("named pipe io: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::LimNativeAdapter;
    use lim_core::content::ContentBlock;
    use lim_core::message::Role;

    fn unique_pipe_name() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        format!(r"\\.\pipe\lim-test-{}", id)
    }

    #[tokio::test]
    async fn pipe_roundtrip_single_message() {
        let adapter = Arc::new(LimNativeAdapter::new());
        let pipe_name = unique_pipe_name();

        let server = NamedPipeServer::create(&pipe_name, adapter.clone()).unwrap();

        // Spawn server accept task
        let server_task = tokio::spawn(async move {
            let mut conn = server.connect().await.unwrap();
            let received = conn.read_message().await.unwrap();
            // Echo back
            conn.write_message(&received).await.unwrap();
            conn
        });

        // Small delay to let server start waiting
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut client = NamedPipeClientTransport::connect(&pipe_name, adapter).await.unwrap();

        // Client → Server
        let msg = AgentMessage::user("hello over named pipe");
        client.write_message(&msg).await.unwrap();

        // Server echoes back
        let received = client.read_message().await.unwrap();
        assert_eq!(received.role, Role::User);
        match &received.content[0] {
            ContentBlock::Text { text } => {
                assert_eq!(text, "hello over named pipe");
            }
            other => panic!("expected Text, got {other:?}"),
        }

        // Wait for server task
        let _ = server_task.await;
    }
}
