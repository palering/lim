//! Daemon IPC transport abstraction.
//!
//! This module defines the [`IpcTransport`] trait — the transport-agnostic
//! interface between a daemon client (CLI, TUI, remote caller) and the
//! daemon server. Concrete implementations live in sibling files:
//!
//! - `daemon.rs` — `DaemonHandle` (in-process mpsc channel, the simple impl)
//! - future: `ipc_unix.rs` — `UnixSocketTransport` (Unix domain socket)
//! - future: `ipc_tcp.rs` — `TcpTransport` (TCP for remote callers)
//!
//! ## Why a trait?
//!
//! The in-process mpsc implementation is sufficient for tests and for
//! single-process TUI/CLI integration. But the daemon's real value is
//! running as a background server that multiple clients connect to —
//! which requires a real socket transport.
//!
//! By abstracting the transport, the CLI/TUI layer depends on
//! `dyn IpcTransport` rather than `DaemonHandle`, so we can swap
//! implementations without touching client code.
//!
//! ## Design
//!
//! ```text
//! Client (CLI/TUI)                IpcTransport (trait)             DaemonServer
//!      │                               │                               │
//!      │  send(DaemonCommand)          │                               │
//!      ├──────────────────────────────▶│──────────────────────────────▶│
//!      │                               │                               │ process
//!      │  DaemonResponse               │                               │
//!      │◀──────────────────────────────│◀──────────────────────────────┤
//! ```
//!
//! ## Known limitations (P4.9 scope)
//!
//! - Only `DaemonHandle` (mpsc) implements the trait. Unix socket transport
//!   is a future enhancement (pending crate choice: `tokio::net::UnixListener`
//!   vs `interprocess` crate).
//! - No authentication or encryption — fine for local Unix socket, not for TCP.

use async_trait::async_trait;

use lim_core::error::Result;

use super::daemon::{DaemonCommand, DaemonResponse};

/// Transport-agnostic interface for sending commands to the daemon server.
///
/// Implementations are responsible for:
/// - Serializing the command (if needed for wire transport)
/// - Sending it to the server
/// - Waiting for and returning the response
/// - Handling transport-level errors (connection refused, timeout, etc.)
///
/// The trait is `Clone`-friendly (implementations are typically `Clone`
/// handles wrapping a sender) so multiple clients can share a transport.
#[async_trait]
pub trait IpcTransport: Send + Sync {
    /// Send a command to the daemon and wait for the response.
    ///
    /// Returns `Err` on transport-level failures (connection lost, timeout).
    /// A successful `Ok(response)` does NOT mean the command succeeded —
    /// check `response.is_error()` for command-level errors.
    async fn send(&self, command: DaemonCommand) -> Result<DaemonResponse>;

    /// Human-readable description of the transport (e.g. "mpsc" / "unix:/path").
    ///
    /// Used for logging and the `lim daemon status` command.
    fn describe(&self) -> String;
}

/// No-op transport for testing or when daemon is disabled.
///
/// Always returns `DaemonResponse::Error` — useful as a placeholder when
/// the daemon feature is compiled out or for unit tests that don't want
/// to spawn a real server.
pub struct NullTransport;

impl NullTransport {
    pub fn new() -> Self {
        Self
    }
}

impl Default for NullTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IpcTransport for NullTransport {
    async fn send(&self, _command: DaemonCommand) -> Result<DaemonResponse> {
        Ok(DaemonResponse::Error {
            message: "no daemon transport configured (NullTransport)".to_string(),
        })
    }

    fn describe(&self) -> String {
        "null".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn null_transport_returns_error_response() {
        let transport = NullTransport::new();
        let resp = transport.send(DaemonCommand::List).await.unwrap();
        assert!(resp.is_error());
    }

    #[tokio::test]
    async fn null_transport_describes_itself() {
        let transport = NullTransport::new();
        assert_eq!(transport.describe(), "null");
    }
}
