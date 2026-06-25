//! SSH backend that implements [`CrabPortTerminal`] using `russh`.
//!
//! Spawns an async task that connects to the remote host, opens a PTY session,
//! and bridges data between the terminal parser and the SSH channel.

mod backend;
pub mod session;

pub use backend::SshBackend;
