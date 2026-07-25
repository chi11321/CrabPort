//! Shared tokio runtime for the whole CrabPort process.
//!
//! All async terminal backends (SSH via `russh`, telnet, serial) need a
//! tokio runtime to drive their async I/O. Rather than each backend crate
//! lazily creating its own `Runtime` (which would spawn multiple worker
//! thread pools — one per crate, each sized to the CPU count), we expose a
//! single shared runtime here and have every backend reference it via
//! `crabport_terminal::runtime::TOKIO`.
//!
//! Features required by the backends are accumulated here:
//! - `rt` + `rt-multi-thread` — runtime itself.
//! - `net` — `TcpStream` (proxy, telnet).
//! - `io-util` — `AsyncReadExt` / `AsyncWriteExt`.
//! - `macros` — `tokio::spawn` etc.
//! - `sync` — `tokio::sync::Mutex` (SSH handle).
//! - `fs` — `tokio::fs::File` (SFTP transfers).
//! - `time` — `tokio::time::sleep` (serial broadcaster).

use std::sync::LazyLock;

use tokio::runtime::Runtime;

/// Process-wide tokio runtime shared by all terminal backends (SSH, telnet,
/// serial) and by SFTP transfer tasks spawned from them. Lazily created on
/// first use; one worker pool for the whole process.
pub static TOKIO: LazyLock<Runtime> = LazyLock::new(|| {
    Runtime::new().expect("failed to create shared tokio runtime for CrabPort backends")
});
