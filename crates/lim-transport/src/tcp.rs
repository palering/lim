//! TCP transport (T2 cross-node, also T1 fallback).
//!
//! Provides length-prefixed framing over `tokio::net::TcpStream` with
//! pluggable `ProtocolAdapter`. This is the cross-platform fallback
//! for T1 (when Unix socket / Named Pipe is unavailable) and the
//! primary transport for T2 (cross-node on LAN).
//!
//! ## Framing
//!
//! ```text
//! ┌────────────┬─────────────────────────┐
//! │ 4 bytes    │ payload_length bytes    │
//! │ len (BE)   │ encoded AgentMessage    │
//! └────────────┴─────────────────────────┘
//! ```
//!
//! ## Usage
//!
//! ```ignore
//! let adapter = Arc::new(LimNativeAdapter::new());
//! let server = TcpTransportServer::bind("127.0.0.1:0", adapter.clone()).await?;
//! let client = TcpStreamTransport::connect(server.local_addr(), adapter).await?;
//! ```

use std::net::SocketAddr;
use std::sync::Arc;

use lim_core::error::{LimError, Result};
use lim_core::message::AgentMessage;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::debug;

use crate::protocol::ProtocolAdapter;

/// Maximum frame size (16 MiB). Prevents malicious/malformed large frames
/// from exhausting memory.
const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// A framed TCP stream transport — client side.
///
/// Wraps a `TcpStream` and a `ProtocolAdapter`. Each call to
/// [`write_message`] writes one framed message; [`read_message`] reads one.
pub struct TcpStreamTransport {
    stream: TcpStream,
    adapter: Arc<dyn ProtocolAdapter>,
}

impl TcpStreamTransport {
    /// Connect to a TCP server and wrap the stream with the given adapter.
    pub async fn connect(addr: SocketAddr, adapter: Arc<dyn ProtocolAdapter>) -> Result<Self> {
        let stream = TcpStream::connect(addr)
            .await
            .map_err(|e| LimError::Config(format!("tcp connect {addr}: {e}")))?;
        debug!(addr = %addr, "tcp transport connected");
        Ok(Self { stream, adapter })
    }

    /// Wrap an existing `TcpStream` (e.g. from `TcpTransportServer::accept`).
    pub fn from_stream(stream: TcpStream, adapter: Arc<dyn ProtocolAdapter>) -> Self {
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

    /// Get the local address of this connection.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.stream.local_addr().map_err(io_err)
    }

    /// Get the remote address of this connection.
    pub fn peer_addr(&self) -> Result<SocketAddr> {
        self.stream.peer_addr().map_err(io_err)
    }
}

/// A TCP server that accepts framed connections.
pub struct TcpTransportServer {
    listener: TcpListener,
    adapter: Arc<dyn ProtocolAdapter>,
}

impl TcpTransportServer {
    /// Bind to an address. Use port `0` to let the OS pick a free port.
    pub async fn bind(addr: SocketAddr, adapter: Arc<dyn ProtocolAdapter>) -> Result<Self> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| LimError::Config(format!("tcp bind {addr}: {e}")))?;
        debug!(addr = %addr, "tcp server bound");
        Ok(Self { listener, adapter })
    }

    /// The actual bound address (useful when port `0` was requested).
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.listener.local_addr().map_err(io_err)
    }

    /// Accept the next incoming connection.
    pub async fn accept(&self) -> Result<TcpStreamTransport> {
        let (stream, peer) = self.listener.accept().await.map_err(io_err)?;
        debug!(peer = %peer, "tcp server accepted connection");
        Ok(TcpStreamTransport::from_stream(stream, Arc::clone(&self.adapter)))
    }
}

fn io_err(e: std::io::Error) -> LimError {
    LimError::Config(format!("tcp io: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::LimNativeAdapter;
    use lim_core::content::ContentBlock;
    use lim_core::message::Role;

    #[tokio::test]
    async fn tcp_roundtrip_single_message() {
        let adapter = Arc::new(LimNativeAdapter::new());
        let server = TcpTransportServer::bind("127.0.0.1:0".parse().unwrap(), adapter.clone())
            .await
            .unwrap();
        let addr = server.local_addr().unwrap();

        let client = TcpStreamTransport::connect(addr, adapter.clone()).await.unwrap();

        // Accept on server side
        let mut server_conn = server.accept().await.unwrap();
        let mut client = client;

        // Client → Server
        let msg = AgentMessage::user("hello over tcp");
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
    async fn tcp_roundtrip_multiple_messages() {
        let adapter = Arc::new(LimNativeAdapter::new());
        let server = TcpTransportServer::bind("127.0.0.1:0".parse().unwrap(), adapter.clone())
            .await
            .unwrap();
        let addr = server.local_addr().unwrap();

        let mut client = TcpStreamTransport::connect(addr, adapter.clone()).await.unwrap();
        let mut server_conn = server.accept().await.unwrap();

        for i in 0..5 {
            let msg = AgentMessage::user(format!("message {i}"));
            client.write_message(&msg).await.unwrap();
            let received = server_conn.read_message().await.unwrap();
            match &received.content[0] {
                lim_core::content::ContentBlock::Text { text } => {
                    assert_eq!(text, &format!("message {i}"));
                }
                other => panic!("expected Text, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn tcp_large_message() {
        let adapter = Arc::new(LimNativeAdapter::new());
        let server = TcpTransportServer::bind("127.0.0.1:0".parse().unwrap(), adapter.clone())
            .await
            .unwrap();
        let addr = server.local_addr().unwrap();

        let mut client = TcpStreamTransport::connect(addr, adapter.clone()).await.unwrap();
        let mut server_conn = server.accept().await.unwrap();

        let big_text = "x".repeat(100_000);
        let msg = AgentMessage::user(&big_text);
        client.write_message(&msg).await.unwrap();
        let received = server_conn.read_message().await.unwrap();
        match &received.content[0] {
            lim_core::content::ContentBlock::Text { text } => {
                assert_eq!(text.len(), 100_000);
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }
}
