//! Executor abstraction — the trait that tools depend on for file/command access.
//!
//! Tools never touch `std::process::Command` or the host filesystem directly.
//! Instead they call methods on the `Executor` trait, which can be backed by
//! local, container, or remote implementations.
