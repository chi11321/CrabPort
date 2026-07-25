//! Telnet terminal backend for CrabPort.
//!
//! Connects to a remote host over raw TCP (optionally tunnelled through a
//! proxy) and implements just enough of the Telnet protocol (RFC 854) to keep
//! the session alive: incoming IAC negotiations are answered by refusing
//! options we don't support, and IAC bytes are stripped from the visible
//! output. Authentication is left to the user — the server's `login:` /
//! `Password:` prompts pass through to the terminal like any other telnet
//! client.

pub mod backend;
pub mod session;

pub use backend::TelnetBackend;
// Re-export the shared tokio runtime so existing callers using
// `crabport_telnet::TOKIO` keep working. The runtime itself lives in
// `crabport-terminal` so it's shared with the SSH and serial backends.
pub use crabport_terminal::runtime::TOKIO;
pub use session::TelnetConnectionInfo;
