//! Error type shared across the AI layer.
//!
//! Hand-rolled `Display`/`Error` impls instead of `thiserror` — matches
//! the rest of the workspace (`crabport_core::StoreError`) and keeps the
//! dependency tree flat.

use std::fmt;

/// Errors surfaced by [`crate::provider::ChatProvider`] implementations.
#[derive(Debug)]
pub enum AiError {
    /// Connection / DNS / TLS level failure or a connect timeout.
    Network(String),
    /// The remote API answered with a non-success HTTP status. `message`
    /// carries the server-provided error text when available.
    Api { status: u16, message: String },
    /// The server answered 200 but produced unparseable output (bad JSON,
    /// broken SSE framing, oversized lines, …).
    Protocol(String),
    /// Local I/O failure while reading the response body.
    Io(std::io::Error),
    /// The consumer dropped/closed the event channel; the worker stopped
    /// early. Not a fault.
    Cancelled,
}

impl fmt::Display for AiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AiError::Network(m) => write!(f, "network error: {m}"),
            AiError::Api { status, message } => write!(f, "api error ({status}): {message}"),
            AiError::Protocol(m) => write!(f, "protocol error: {m}"),
            AiError::Io(e) => write!(f, "io error: {e}"),
            AiError::Cancelled => write!(f, "stream cancelled"),
        }
    }
}

impl std::error::Error for AiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AiError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for AiError {
    fn from(e: std::io::Error) -> Self {
        AiError::Io(e)
    }
}
