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

    fn as_monitor(&self) -> Option<&dyn CrabPortMonitor> {
        None
    }
}

// ---------------------------------------------------------------------------
// Remote performance monitoring
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RemoteStatus {
    Local,
    Connected,
    Connecting,
    Disconnected,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NetworkStats {
    pub bytes_sent: u64,
    pub bytes_recv: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MemoryStats {
    pub total: u64,
    pub used: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RemoteMetrics {
    pub latency_ms: Option<u32>,
    pub memory: Option<MemoryStats>,
    pub network: Option<NetworkStats>,
}

pub trait CrabPortMonitor: Send + Sync {
    fn status(&self) -> RemoteStatus;
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
                            // Batch-drain: hold the term lock once and advance all
                            // currently-queued chunks. Cuts lock churn and wakeup
                            // storms when the PTY floods (cat / top / build logs).
                            let mut terminal = term.lock();
                            parser.advance(&mut *terminal, &data);
                            loop {
                                match rx.try_recv() {
                                    Ok(BackendEvent::Data(more)) => {
                                        parser.advance(&mut *terminal, &more);
                                    }
                                    Ok(BackendEvent::Closed) => {
                                        drop(terminal);
                                        let _ = wakeup_tx.try_broadcast(());
                                        return;
                                    }
                                    Ok(BackendEvent::Error(err)) => {
                                        tracing::error!("terminal backend error: {}", err);
                                    }
                                    Err(_) => break, // queue drained
                                }
                            }
                            drop(terminal);
                            let _ = wakeup_tx.try_broadcast(());
                        }
                        BackendEvent::Closed => {
                            #[cfg(debug_assertions)]
                            tracing::info!("session: backend closed");
                            let _ = wakeup_tx.try_broadcast(());
                            break;
                        }
                        BackendEvent::Error(err) => {
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

    /// Mutable access — needed to read & reset alacritty damage.
    pub fn with_term_mut<R>(&self, f: impl FnOnce(&mut Term<EventProxy>) -> R) -> R {
        let mut term = self.term.lock();
        f(&mut *term)
    }

    /// Non-blocking mutable access. Returns `None` if the reader thread currently
    /// holds the lock — the caller should reuse the previous frame's snapshot
    /// instead of stalling the render thread.
    pub fn try_with_term_mut<R>(&self, f: impl FnOnce(&mut Term<EventProxy>) -> R) -> Option<R> {
        self.term.try_lock_unfair().map(|mut t| f(&mut *t))
    }

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

    pub fn subscribe_backend(&self) -> BroadcastReceiver<BackendEvent> {
        self.backend.subscribe()
    }

    pub fn monitor(&self) -> Option<&dyn CrabPortMonitor> {
        self.backend.as_monitor()
    }

    pub fn scroll(&self, delta: i32) {
        let mut term = self.term.lock();
        use alacritty_terminal::grid::Scroll;
        term.scroll_display(Scroll::Delta(delta));
        let _ = self.wakeup_tx.try_broadcast(());
    }

    pub fn scroll_to_bottom(&self) {
        let mut term = self.term.lock();
        use alacritty_terminal::grid::Scroll;
        term.scroll_display(Scroll::Bottom);
        let _ = self.wakeup_tx.try_broadcast(());
    }
}
