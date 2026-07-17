//! Process-wide tracing initialization.
//!
//! - Debug build: logs to **stderr** (human-readable, ANSI colors) **and**
//!   to `{data_dir}/crabport/latest.log`.
//! - Release build: logs to `{data_dir}/crabport/latest.log` only.
//!
//! The file is opened in append mode, so consecutive app runs accumulate in
//! the same `latest.log` until the user clears it. Truncate-on-start could be
//! added later if the file grows too large.
//!
//! `init()` is called unconditionally from `main` (no `debug_assertions`
//! gate) — release builds produce logs too, which is essential for
//! diagnosing field-reported issues. The tracing macros elsewhere
//! (`info!` / `warn!` / etc.) become no-ops only if no subscriber is
//! installed, which never happens after this `init`.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::Local;
use tracing_subscriber::fmt::{format::Writer, time::FormatTime};

struct LocalTime;

impl FormatTime for LocalTime {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", Local::now().format("%Y-%m-%d %H:%M:%S%.6f"))
    }
}

/// Path to the `latest.log` file inside the CrabPort data directory.
///
/// Sibling of `config.toml` and `crabport.db` so all user-facing app state
/// lives in one place (`dirs::data_dir()/crabport/`). Returns `None` if the
/// data directory can't be resolved (e.g. sandboxed environments without a
/// home dir) — in that case we fall back to stderr-only logging.
fn log_path() -> Option<PathBuf> {
    let base = dirs::data_dir()?;
    Some(base.join("crabport").join("latest.log"))
}

/// Open (or create) `latest.log` for append, creating the parent directory
/// if needed. Returns `None` if the file can't be opened (logged to stderr
/// as a last resort).
fn open_log_file() -> Option<Arc<Mutex<File>>> {
    let path = log_path()?;
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return None;
        }
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map(|f| Arc::new(Mutex::new(f)))
        .map_err(|e| {
            // Last-resort: print to stderr so the failure is at least visible
            // somewhere (file logging is broken, but the app should still run).
            eprintln!("crabport-core::log: failed to open {}: {e}", path.display());
        })
        .ok()
}

/// A `MakeWriter` that writes into a shared `Mutex<File>`.
///
/// We need `Mutex` (not `&File`) because tracing may emit log lines from
/// multiple threads concurrently, and `io::Write` on a raw `File` is not
/// synchronized — concurrent writes would interleave/corrupt lines.
///
/// Each `make_new()` call returns a guard that holds the lock for the
/// duration of one `write!` batch, so a single log line stays contiguous.
struct FileMakeWriter {
    file: Arc<Mutex<File>>,
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for FileMakeWriter {
    type Writer = FileWriteGuard<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        // Lock for the lifetime of the returned guard. tracing-subscriber
        // drops the writer after flushing each record, so the lock is held
        // only across one log line — not a contention concern.
        FileWriteGuard {
            lock: self.file.lock().unwrap_or_else(|e| e.into_inner()),
        }
    }
}

/// RAII guard holding a `MutexGuard<File>` and forwarding `io::Write` calls
/// to the underlying file. On `Err` during `write` (e.g. disk full) we mask
/// the error — tracing shouldn't panic the app because logging failed.
struct FileWriteGuard<'a> {
    lock: std::sync::MutexGuard<'a, File>,
}

impl<'a> Write for FileWriteGuard<'a> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.lock.write(buf).or_else(|_e| {
            // If the write fails, try once more (e.g. after a flush) and
            // otherwise mask: returning Ok(buf.len()) makes tracing happy
            // and we don't propagate log failures into the caller.
            self.lock.write(buf).or(Ok(buf.len()))
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        self.lock.flush().or(Ok(()))
    }
}

pub fn init() {
    let file = open_log_file();

    // Build a layered subscriber:
    // - File layer (always, if the file opened) — no ANSI colors in the log
    //   file (plain text is greppable and survives any pager/editor).
    // - Stderr layer (debug builds only) — with ANSI colors for readability
    //   during local dev.
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};

    // Default to INFO verbosity unless RUST_LOG overrides.
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let registry = tracing_subscriber::registry().with(env_filter);

    // `LocalTime` is a unit struct (zero-sized), so we can construct it at
    // each use site rather than threading a binding through the cfg branches.
    #[cfg(debug_assertions)]
    let registry = registry.with(fmt::layer().with_timer(LocalTime).with_writer(io::stderr));

    if let Some(file) = file {
        let make = FileMakeWriter { file };
        let file_layer = fmt::layer()
            .with_timer(LocalTime)
            .with_ansi(false)
            .with_writer(make);
        registry.with(file_layer).init();
    } else {
        // File open failed — fall back to stderr even in release so logs
        // aren't completely lost.
        registry
            .with(fmt::layer().with_timer(LocalTime).with_writer(io::stderr))
            .init();
    }
}
