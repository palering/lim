//! lim-session: JSONL event log persistence, replay, and discovery.
//!
//! - [`SessionWriter`] appends [`EventEnvelope`]s atomically to a session
//!   file (truncate-on-open to prevent data pollution when reusing paths).
//! - [`SessionReader`] loads events back.
//! - [`replay_transcript`] extracts the message stream; [`replay_edits`]
//!   re-applies `ContextEdited` events to restore user edits on resume.
//! - [`SessionManager`] enumerates sessions in a directory and derives
//!   [`SessionMeta`] / [`SessionDetails`] from the JSONL files.

pub mod manager;
pub mod reader;
pub mod replay;
pub mod writer;

pub use manager::{SessionDetails, SessionManager, SessionMeta};
