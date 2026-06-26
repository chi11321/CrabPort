use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use alacritty_terminal::{
    Term,
    event::{Event, EventListener},
    sync::FairMutex,
    term::{Config, test::TermSize},
    vte::ansi::{Processor, StdSyncHandler},
};
use async_broadcast::{
    InactiveReceiver, Receiver as BroadcastReceiver, Sender as BroadcastSender, broadcast,
};

#[derive(Debug, Clone)]
pub enum BackendEvent {
    Data(Vec<u8>),
    Closed,
    Error(String),
}

pub trait CrabPortTerminal: Send + Sync {
    fn write(&self, data: &[u8]);
    fn resize(&self, cols: u16, rows: u16);
    fn close(&self);
    fn subscribe(&self) -> BroadcastReceiver<BackendEvent>;

    /// Try to obtain a `CrabPortMonitor` implementation.
    /// Backends that also implement `CrabPortMonitor` should return `Some(self)`.
    fn as_monitor(&self) -> Option<&dyn CrabPortMonitor> {
        None
    }
}

// ---------------------------------------------------------------------------
// Remote performance monitoring
// ---------------------------------------------------------------------------

/// Connection state of a backend.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RemoteStatus {
    /// Local session (no network connection).
    Local,
    /// Actively connected and operational.
    Connected,
    /// Connection is being established.
    Connecting,
    /// Connection has been lost or closed.
    Disconnected,
}

/// Snapshot of network I/O counters (bytes).
#[derive(Clone, Copy, Debug, Default)]
pub struct NetworkStats {
    /// Total bytes sent since connection start.
    pub bytes_sent: u64,
    /// Total bytes received since connection start.
    pub bytes_recv: u64,
}

/// Snapshot of remote host memory usage.
#[derive(Clone, Copy, Debug, Default)]
pub struct MemoryStats {
    /// Total physical memory in bytes.
    pub total: u64,
    /// Used physical memory in bytes.
    pub used: u64,
}

/// A full performance snapshot from a remote backend.
#[derive(Clone, Copy, Debug, Default)]
pub struct RemoteMetrics {
    /// Round-trip latency in milliseconds.
    pub latency_ms: Option<u32>,
    /// Remote host memory usage.
    pub memory: Option<MemoryStats>,
    /// Network I/O counters.
    pub network: Option<NetworkStats>,
}

/// Trait for backends that can report performance metrics.
///
/// Implement this on `CrabPortTerminal` for any backend (local or remote)
/// to expose latency, memory, and network monitoring data to the UI.
/// Local backends should return `latency_ms = None` and `RemoteStatus::Local`.
pub trait CrabPortMonitor: Send + Sync {
    /// Current connection status.
    fn status(&self) -> RemoteStatus;

    /// Latest performance metrics snapshot.
    fn metrics(&self) -> RemoteMetrics;
}

#[derive(Clone)]
pub struct EventProxy {
    wakeup_tx: BroadcastSender<()>,
}

impl EventProxy {
    pub fn new(wakeup_tx: BroadcastSender<()>) -> Self {
        Self { wakeup_tx }
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::Wakeup => {
                #[cfg(debug_assertions)]
                tracing::debug!("EventProxy: Wakeup event received");
                let _ = self.wakeup_tx.try_broadcast(());
            }
            _ => {
                #[cfg(debug_assertions)]
                tracing::debug!("EventProxy: Other event {:?}", event);
                let _ = self.wakeup_tx.try_broadcast(());
            }
        }
    }
}

pub struct TerminalSession {
    backend: Arc<dyn CrabPortTerminal>,
    term: Arc<FairMutex<Term<EventProxy>>>,
    wakeup_tx: BroadcastSender<()>,
    started: AtomicBool,
    _wakeup_rx: InactiveReceiver<()>,
}

impl TerminalSession {
    pub fn new(backend: Arc<dyn CrabPortTerminal>, cols: usize, rows: usize) -> Self {
        let (wakeup_tx, wakeup_rx) = broadcast(256);
        let _wakeup_rx = wakeup_rx.deactivate();

        let term = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &TermSize::new(cols, rows),
            EventProxy::new(wakeup_tx.clone()),
        )));

        Self {
            backend,
            term,
            wakeup_tx,
            started: AtomicBool::new(false),
            _wakeup_rx,
        }
    }

    pub fn start(&self) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }

        let mut rx = self.backend.subscribe();
        let term = self.term.clone();
        let wakeup_tx = self.wakeup_tx.clone();

        smol::spawn(async move {
            let mut parser = Processor::<StdSyncHandler>::new();

            loop {
                match rx.recv().await {
                    Ok(event) => match event {
                        BackendEvent::Data(data) => {
                            #[cfg(debug_assertions)]
                            tracing::debug!("session: received {} bytes", data.len());
                            let mut terminal = term.lock();
                            parser.advance(&mut *terminal, &data);
                            // Force wakeup after processing data
                            let _ = wakeup_tx.try_broadcast(());
                        }
                        BackendEvent::Closed => {
                            #[cfg(debug_assertions)]
                            tracing::info!("session: backend closed");
                            let _ = wakeup_tx.try_broadcast(());
                            break;
                        }
                        BackendEvent::Error(err) => {
                            #[cfg(debug_assertions)]
                            tracing::error!("terminal backend error: {}", err);
                            let _ = wakeup_tx.try_broadcast(());
                        }
                    },
                    Err(e) => {
                        #[cfg(debug_assertions)]
                        tracing::warn!("session: recv error: {:?}", e);
                        let _ = wakeup_tx.try_broadcast(());
                        break;
                    }
                }
            }
        })
        .detach();
    }

    pub fn with_term<R>(&self, f: impl FnOnce(&Term<EventProxy>) -> R) -> R {
        let term = self.term.lock();
        f(&*term)
    }

    /// Feed an escape sequence directly into the terminal parser (bypasses PTY).
    pub fn feed_escape(&self, data: &[u8]) {
        let mut term = self.term.lock();
        let mut parser = Processor::<StdSyncHandler>::new();
        parser.advance(&mut *term, data);
    }

    pub fn write(&self, data: &[u8]) {
        self.backend.write(data);
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        {
            let mut term = self.term.lock();
            term.resize(TermSize::new(cols as usize, rows as usize));
        }
        self.backend.resize(cols, rows);
    }

    pub fn close(&self) {
        self.backend.close();
    }

    pub fn subscribe_wakeup(&self) -> BroadcastReceiver<()> {
        self.wakeup_tx.new_receiver()
    }

    /// Try to obtain a reference to the `CrabPortMonitor` implementation.
    /// Returns `None` if the backend doesn't implement `CrabPortMonitor`.
    pub fn monitor(&self) -> Option<&dyn CrabPortMonitor> {
        // Both CrabPortTerminal and CrabPortMonitor are object-safe,
        // but we can't downcast `dyn CrabPortTerminal` to `dyn CrabPortMonitor`
        // without a helper. Instead, we store both traits when available.
        //
        // For now, we use a simple approach: since the concrete types that
        // implement CrabPortTerminal also implement CrabPortMonitor, we ask
        // the backend for its monitor via a method on CrabPortTerminal.
        self.backend.as_monitor()
    }

    pub fn scroll(&self, delta: i32) {
        let mut term = self.term.lock();
        use alacritty_terminal::grid::Scroll;
        if delta > 0 {
            term.scroll_display(Scroll::Delta(delta));
        } else {
            term.scroll_display(Scroll::Delta(delta));
        }
        let _ = self.wakeup_tx.try_broadcast(());
    }

    pub fn scroll_to_bottom(&self) {
        let mut term = self.term.lock();
        use alacritty_terminal::grid::Scroll;
        term.scroll_display(Scroll::Bottom);
        let _ = self.wakeup_tx.try_broadcast(());
    }
}
