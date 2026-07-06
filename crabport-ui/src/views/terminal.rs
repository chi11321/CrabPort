use std::ops::Range;
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use alacritty_terminal::{
    grid::Dimensions,
    term::TermDamage,
    term::cell::Flags,
    vte::ansi::{Color, CursorShape, NamedColor},
};
use crabport_core::keybind::{self, KeyAction, TerminalAction};
use crabport_ssh::CrabPortTunnel;
use crabport_ssh::backend::HostKeyInfo;
use crabport_ssh::session::SshConnectionInfo;
use crabport_telnet::backend::TelnetBackend;
use crabport_telnet::session::TelnetConnectionInfo;
use crabport_terminal::pty::PtyBackend;
use crabport_terminal::terminal::{
    CrabPortMonitor, RemoteStatus, SftpTransferBytes, SftpTransferKind, SftpTransferStage,
    TerminalSession,
};

use gpui::prelude::FluentBuilder;
use gpui::*;
use parking_lot::Mutex;

use crate::app::{CrabPortTab, TerminalShiftTab, TerminalTab};
use crate::color::{selection_bg, term_bg, term_cursor, term_fg};
use crate::views::terminal::color::*;
use crate::views::terminal::connection_overlay::*;
use crate::views::terminal::fonts::palette;
use crate::views::terminal::render_cache::{
    CellSnap, RenderCache, RowSnapshot, SharedRenderCache, hash_row,
};
use crate::views::terminal::runs::build_runs;
use crate::views::terminal::selection::*;

pub mod connection_overlay;

mod color;
mod fonts;
mod render_cache;
mod runs;
mod selection;

// ---- TerminalView ----

/// Snapshot of an in-flight SFTP transfer, surfaced to the toolbar so the
/// user can see which stage (compress / transfer / decompress / cleanup)
/// is currently running and which path it's working on.
///
/// `None` on `TerminalView` means no transfer is active (either none was
/// started, or the most recent one already finished and the result has
/// been shown long enough — see [`TerminalView::clear_sftp_progress`]).
#[derive(Clone, Debug)]
pub struct SftpProgress {
    pub kind: SftpTransferKind,
    pub stage: SftpTransferStage,
    /// Short detail string emitted by the backend — typically the path of
    /// the file currently being processed.
    pub message: String,
    /// Byte-level progress for the current stage, when available. `None`
    /// for stages that don't have a meaningful byte count (e.g. remote
    /// `gzip` which runs as an opaque exec).
    pub bytes: Option<SftpTransferBytes>,
}

pub struct TerminalView {
    session: Arc<TerminalSession>,
    /// Cloned `Arc` to the underlying backend, kept so this view can call
    /// trait methods (`sftp_rename`, `sftp_open_in_editor`, …) that
    /// `TerminalSession` doesn't yet forward. `TerminalSession` owns the
    /// backend privately; this clone is cheap and stays in sync because the
    /// only mutation point is `reconnect`, which reassigns both.
    backend: Arc<dyn crabport_terminal::terminal::CrabPortTerminal>,
    focus_handle: FocusHandle,
    font_size: Pixels,
    line_height: Pixels,
    cell_width: Pixels,
    last_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
    selection: Arc<Mutex<Option<Selection>>>,
    render_cache: SharedRenderCache,
    /// Set by data/status; consumed by the ~120Hz frame pump.
    needs_repaint: Arc<AtomicBool>,
    bindings: Vec<keybind::Binding>,
    pending_paste: bool,
    pending_copy: bool,
    scroll_accumulator: f32,
    /// Latest display_offset from the alacritty grid, updated each prepaint.
    /// Used by mouse handlers to convert viewport rows to grid lines.
    display_offset: Arc<std::sync::atomic::AtomicI32>,
    /// Latest history_size from the alacritty grid, updated each prepaint.
    /// Used by the scrollbar overlay to compute thumb position/size.
    history_size: Arc<std::sync::atomic::AtomicI32>,
    /// Latest visible row count, updated each prepaint.
    visible_rows: Arc<std::sync::atomic::AtomicI32>,
    /// Whether the scrollbar thumb is currently being dragged.
    scrollbar_dragging: Arc<std::sync::atomic::AtomicBool>,
    /// Y offset (in px) from the thumb top to the mouse cursor at drag start.
    scrollbar_drag_offset: Arc<Mutex<f32>>,
    /// Current IME marked (preedit) text, if any. Set by the platform's IME
    /// system via [`EntityInputHandler::replace_and_mark_text_in_range`] and
    /// committed (written to the PTY) via `replace_text_in_range`. Rendered
    /// inline at the cursor so the user sees live composition feedback.
    marked_text: Arc<Mutex<Option<String>>>,
    /// Latest terminal cursor bounds in window coordinates, refreshed each
    /// paint. Used by [`EntityInputHandler::bounds_for_range`] to position the
    /// IME candidate window near the cursor.
    cursor_bounds: Arc<Mutex<Bounds<Pixels>>>,
    overlay: SharedOverlayState,
    remote_host: String,
    /// Persisted host id for command-history storage. `None` for local
    /// terminals (their history is in-memory only, not persisted).
    #[allow(dead_code)]
    host_id: Option<i64>,
    count: u64,
    ssh_info: Option<SshConnectionInfo>,
    telnet_info: Option<TelnetConnectionInfo>,
    on_backend_closed: Option<Rc<dyn Fn(&mut App)>>,
    /// Latest SFTP transfer progress pushed by the backend, or `None` when
    /// no transfer is in flight. Updated by the backend-event subscriber;
    /// read by the toolbar via [`Self::sftp_progress`].
    sftp_progress: Option<SftpProgress>,
    /// Invoked whenever `sftp_progress` changes, so the app (which renders
    /// the toolbar) can re-render without observing every terminal repaint.
    /// Mirrors the `on_backend_closed` callback pattern.
    on_sftp_progress_changed: Option<Rc<dyn Fn(&mut App)>>,
    /// Invoked when an SFTP transfer finishes (success or failure), so the
    /// app can surface a toast notification. Mirrors the
    /// `on_sftp_progress_changed` / `on_backend_closed` callback pattern.
    on_sftp_transfer_finished: Option<Rc<dyn Fn(SftpTransferKind, bool, String, &mut App)>>,
    /// A `CrabPortTunnel` view of the backend, when the backend is an SSH
    /// session. Used by the Tunnels panel to start "borrowed" tunnels that
    /// reuse this tab's SSH connection instead of opening a dedicated owned
    /// session. `None` for local PTY backends.
    tunnel_source: Option<Arc<dyn crabport_ssh::CrabPortTunnel>>,
}

impl TerminalView {
    pub fn new(count: u64, cx: &mut Context<Self>) -> Self {
        let cols: usize = 80;
        let rows: usize = 24;
        let backend = Arc::new(
            PtyBackend::new(cols as u16, rows as u16)
                .expect("failed to create pty backend (local PTY is not supported on Windows)"),
        );
        Self::with_backend(backend, cols, rows, None, None, count, cx)
    }

    pub fn with_backend(
        backend: Arc<dyn crabport_terminal::terminal::CrabPortTerminal>,
        cols: usize,
        rows: usize,
        ssh_info: Option<SshConnectionInfo>,
        telnet_info: Option<TelnetConnectionInfo>,
        count: u64,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_backend_and_host(
            backend,
            cols,
            rows,
            String::new(),
            ssh_info,
            telnet_info,
            count,
            cx,
        )
    }

    pub fn with_backend_and_host(
        backend: Arc<dyn crabport_terminal::terminal::CrabPortTerminal>,
        cols: usize,
        rows: usize,
        host: String,
        ssh_info: Option<SshConnectionInfo>,
        telnet_info: Option<TelnetConnectionInfo>,
        count: u64,
        cx: &mut Context<Self>,
    ) -> Self {
        let overlay = Arc::new(Mutex::new(ConnectionOverlayState::new()));
        Self::with_backend_and_host_and_overlay(
            backend,
            cols,
            rows,
            host,
            None,
            overlay,
            ssh_info,
            telnet_info,
            count,
            cx,
        )
    }

    pub fn with_backend_and_host_and_overlay(
        backend: Arc<dyn crabport_terminal::terminal::CrabPortTerminal>,
        cols: usize,
        rows: usize,
        host: String,
        host_id: Option<i64>,
        overlay: SharedOverlayState,
        ssh_info: Option<SshConnectionInfo>,
        telnet_info: Option<TelnetConnectionInfo>,
        count: u64,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let font_size = px(13.0);
        let line_height = px(20.0);
        // Consolas (Windows) has slightly narrower glyphs than Menlo (macOS);
        // a mismatch here causes character gaps and clipping, so pick the
        // advance width that matches the platform's monospace font.
        let cell_width = if cfg!(target_os = "windows") {
            px(7.2)
        } else {
            px(7.8)
        };

        let session = Arc::new(TerminalSession::new(backend.clone(), cols, rows));
        session.start();

        // Wire command-history persistence: when the session captures a new
        // command, persist it to the Store for this host (if any). Local
        // terminals (host_id = None) keep history in-memory only.
        if let Some(hid) = host_id {
            let store = crate::app_state::AppState::store(cx);
            // Pre-load persisted history into the in-memory buffer so the
            // panel has data before the user types anything new.
            if let Ok(cmds) = store.lock().commands_for_host(hid) {
                let mut history = std::collections::VecDeque::new();
                for c in cmds {
                    history.push_back(c);
                }
                *session.command_history_deque() = history;
            }
            // `store` is `Arc<Mutex<Store>>` — clone for the callback so
            // the original binding stays usable above.
            let store_for_cb = store.clone();
            session.set_on_command(Some(std::sync::Arc::new(move |cmd: &str| {
                let _ = store_for_cb.lock().add_command(hid, cmd);
            })));
        }

        let needs_repaint = Arc::new(AtomicBool::new(true));
        let is_remote = !host.is_empty();

        // Backend error/close events.
        let mut event_rx = session.subscribe_backend();
        let overlay_c = overlay.clone();
        let entity = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            while let Ok(event) = event_rx.recv().await {
                match event {
                    crabport_terminal::terminal::BackendEvent::Error(err) => {
                        overlay_c.lock().log(ConnectionLogLevel::Error, err);
                        let _ = entity.update(cx, |_, cx| cx.notify());
                    }
                    crabport_terminal::terminal::BackendEvent::Closed => {
                        let _ = entity.update(cx, |this, cx| {
                            if let Some(ref cb) = this.on_backend_closed {
                                let cb = cb.clone();
                                cx.defer(move |cx| cb(cx));
                            } else {
                                this.overlay
                                    .lock()
                                    .log(ConnectionLogLevel::Warning, "Connection closed");
                            }
                            cx.notify();
                        });
                    }
                    crabport_terminal::terminal::BackendEvent::SftpTransferFinished {
                        kind,
                        success,
                        message,
                    } => {
                        // Surface transfer results in the connection overlay
                        // so the user gets feedback. A richer toast / status
                        // bar can be added later without changing the backend.
                        let level = if success {
                            ConnectionLogLevel::Info
                        } else {
                            ConnectionLogLevel::Error
                        };
                        let prefix = match kind {
                            crabport_terminal::terminal::SftpTransferKind::Download => "Download",
                            crabport_terminal::terminal::SftpTransferKind::Upload => "Upload",
                            crabport_terminal::terminal::SftpTransferKind::Rename => "Rename",
                            crabport_terminal::terminal::SftpTransferKind::Edit => "Edit",
                        };
                        overlay_c.lock().log(level, format!("{prefix}: {message}"));
                        // Clear the live progress indicator — the transfer
                        // is done (success or failure). The toolbar will
                        // re-render without the progress chip on the next
                        // frame.
                        let _ = entity.update(cx, |this, cx| {
                            this.sftp_progress = None;
                            // Auto-refresh the SFTP listing on success so
                            // uploads/deletes are reflected immediately
                            // without the user clicking the refresh button.
                            // Downloads don't change the remote dir, but
                            // re-navigating is cheap and harmless.
                            if success {
                                if let Some(cwd) = this
                                    .session
                                    .sftp_cwd()
                                    .as_ref()
                                    .map(|c| c.as_str().to_string())
                                {
                                    this.session.sftp_navigate(&cwd);
                                }
                            }
                            let cb = this.on_sftp_progress_changed.clone();
                            let cb_kind = kind;
                            let cb_success = success;
                            let cb_message = message.clone();
                            let finished_cb = this.on_sftp_transfer_finished.clone();
                            cx.notify();
                            if let Some(cb) = cb {
                                cx.defer(move |cx| cb(cx));
                            }
                            if let Some(cb) = finished_cb {
                                cx.defer(move |cx| cb(cb_kind, cb_success, cb_message, cx));
                            }
                        });
                    }
                    crabport_terminal::terminal::BackendEvent::SftpTransferProgress {
                        kind,
                        stage,
                        message,
                        bytes,
                    } => {
                        // Update the live progress snapshot read by the
                        // toolbar. We don't log to the connection overlay
                        // here — the toolbar is the dedicated surface for
                        // in-flight progress, and double-logging would
                        // spam the overlay with one entry per stage.
                        let _ = entity.update(cx, |this, cx| {
                            this.sftp_progress = Some(SftpProgress {
                                kind,
                                stage,
                                message,
                                bytes,
                            });
                            let cb = this.on_sftp_progress_changed.clone();
                            cx.notify();
                            if let Some(cb) = cb {
                                cx.defer(move |cx| cb(cx));
                            }
                        });
                    }
                    crabport_terminal::terminal::BackendEvent::Data(_) => {}
                }
            }
        })
        .detach();

        // Wakeup listener: only mark dirty (+ reflect status into overlay).
        let mut wakeup_rx = session.subscribe_wakeup();
        let dirty_wk = needs_repaint.clone();
        let status_entity = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            while let Ok(()) = wakeup_rx.recv().await {
                let _ = status_entity.update(cx, |this, _cx| {
                    if let Some(m) = this.session.monitor() {
                        let new_status = m.status();
                        let mut ov = this.overlay.lock();
                        if new_status != ov.status {
                            ov.update_status(new_status, &this.remote_host);
                        }
                    }
                });
                dirty_wk.store(true, Ordering::Release);
            }
        })
        .detach();

        // Frame pump: at most ~120Hz, notify only when dirty.
        let dirty_pump = needs_repaint.clone();
        let overlay_dirty_pump = overlay.clone();
        let pump_entity = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            // One full revolution per ~900ms feels close to typical web
            // loaders. At 120Hz that's ~2π/108 rad per tick, encoded as
            // milliradians to keep the atomic integer-friendly.
            const TWO_PI_MRAD: u32 = (std::f32::consts::TAU * 1000.0) as u32;
            const TICKS_PER_REV: u32 = 108;
            const STEP_MRAD: u32 = TWO_PI_MRAD / TICKS_PER_REV;
            // Log row fade-in duration. Must match the value used in
            // `connection_overlay::render_connection_overlay` so the repaint
            // loop keeps ticking for exactly as long as the transition runs.
            const LOG_FADE_MS: u128 = 320;
            loop {
                smol::Timer::after(std::time::Duration::from_micros(8333)).await;
                let ov = overlay_dirty_pump.lock();
                // Fold the overlay-side dirty flag (set from non-gpui threads,
                // e.g. the SSH backend pushing a host-key prompt) into the
                // view's own needs_repaint flag.
                if ov.dirty.swap(false, Ordering::AcqRel) {
                    dirty_pump.store(true, Ordering::Release);
                }
                // While the connecting spinner is on screen, advance its
                // rotation and keep the view dirty so it repaints every
                // tick for a smooth spin.
                let spin = !ov.hidden
                    && ov.status == RemoteStatus::Connecting
                    && ov.pending_host_key.is_none();
                // Also keep repainting while any log row is still
                // mid-fade-in, so each entry's gpui-animation transition
                // actually plays out (without this, only the last row of a
                // batch gets visible animation because earlier rows' redraws
                // stop before their transition finishes).
                let now = std::time::Instant::now();
                let logs_animating = ov
                    .logs
                    .iter()
                    .any(|e| now.duration_since(e.added_at).as_millis() < LOG_FADE_MS);
                let spinner_rotation = ov.spinner_rotation.clone();
                drop(ov);
                if spin {
                    let prev = spinner_rotation.load(Ordering::Relaxed);
                    let next = prev.wrapping_add(STEP_MRAD) % TWO_PI_MRAD;
                    spinner_rotation.store(next, Ordering::Relaxed);
                    dirty_pump.store(true, Ordering::Release);
                }
                if logs_animating {
                    dirty_pump.store(true, Ordering::Release);
                }
                if dirty_pump.swap(false, Ordering::AcqRel) {
                    if pump_entity.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }
        })
        .detach();

        if is_remote {
            let overlay_fade = overlay.clone();
            let dirty_fade = needs_repaint.clone();
            let fade_entity = cx.entity().downgrade();
            cx.spawn(async move |_this, cx| {
                loop {
                    smol::Timer::after(std::time::Duration::from_millis(50)).await;
                    if overlay_fade.lock().fade_out_started {
                        break;
                    }
                }
                smol::Timer::after(std::time::Duration::from_millis(600)).await;
                overlay_fade.lock().mark_hidden();
                dirty_fade.store(true, Ordering::Release);
                let _ = fade_entity.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }

        Self {
            session,
            backend,
            focus_handle,
            font_size,
            line_height,
            cell_width,
            last_bounds: Arc::new(Mutex::new(None)),
            selection: Arc::new(Mutex::new(None)),
            render_cache: Arc::new(Mutex::new(RenderCache::default())),
            needs_repaint,
            bindings: keybind::default_bindings(),
            pending_paste: false,
            pending_copy: false,
            scroll_accumulator: 0.0,
            display_offset: Arc::new(std::sync::atomic::AtomicI32::new(0)),
            history_size: Arc::new(std::sync::atomic::AtomicI32::new(0)),
            visible_rows: Arc::new(std::sync::atomic::AtomicI32::new(0)),
            scrollbar_dragging: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            scrollbar_drag_offset: Arc::new(Mutex::new(0.0)),
            marked_text: Arc::new(Mutex::new(None)),
            cursor_bounds: Arc::new(Mutex::new(Bounds::new(
                point(px(0.0), px(0.0)),
                size(px(0.0), px(0.0)),
            ))),
            overlay,
            remote_host: host,
            host_id,
            count,
            ssh_info,
            telnet_info,
            on_backend_closed: None,
            sftp_progress: None,
            on_sftp_progress_changed: None,
            on_sftp_transfer_finished: None,
            tunnel_source: None,
        }
    }

    pub fn monitor(&self) -> Option<&dyn CrabPortMonitor> {
        self.session.monitor()
    }

    pub fn allow_sftp(&self) -> bool {
        self.session.allow_sftp()
    }

    pub fn allow_history(&self) -> bool {
        self.session.allow_history()
    }

    pub fn allow_snippets(&self) -> bool {
        self.session.allow_snippets()
    }

    pub fn allow_tunnels(&self) -> bool {
        self.session.allow_tunnels()
    }

    pub fn sftp_entries(&self) -> Option<std::sync::Arc<Vec<(String, bool)>>> {
        self.session.sftp_entries()
    }

    pub fn sftp_cwd(&self) -> Option<std::sync::Arc<String>> {
        self.session.sftp_cwd()
    }

    pub fn sftp_navigate(&self, path: &str) {
        self.session.sftp_navigate(path)
    }

    pub fn sftp_download(&self, remote_path: &str, local_path: &str) {
        self.session.sftp_download(remote_path, local_path);
    }

    pub fn sftp_upload(&self, local_path: &str, remote_path: &str) {
        self.session.sftp_upload(local_path, remote_path);
    }

    /// Snapshot of this session's command history, most-recent-first.
    /// Returns an empty vec for local terminals or sessions without a
    /// backend that tracks history.
    pub fn command_history(&self) -> Vec<String> {
        self.session.command_history()
    }

    /// Write raw bytes to the terminal **without** capturing them as a
    /// command. Used by the History panel's "paste" action so inserting a
    /// historical command into the input line doesn't re-record it.
    pub fn write_raw(&self, data: &[u8]) {
        self.session.write_raw(data);
    }

    /// Delete a remote file or directory. The backend stats the path to
    /// decide between `remove_file` and recursive `remove_dir`.
    pub fn sftp_delete(&self, remote_path: &str) {
        self.session.sftp_delete(remote_path);
    }

    /// Rename a remote file or directory. Forwards directly to the backend
    /// (`TerminalSession` doesn't expose a wrapper yet) via the cloned
    /// `backend` `Arc`. Completion is reported through the backend's event
    /// stream as `BackendEvent::SftpTransferFinished`.
    pub fn sftp_rename(&self, old_path: &str, new_path: &str) {
        self.backend.sftp_rename(old_path, new_path);
    }

    /// Download a remote file to a local temp path and open it in the OS
    /// default editor. Forwards directly to the backend. Completion is
    /// reported through the backend's event stream.
    pub fn sftp_open_in_editor(&self, remote_path: &str) {
        self.backend.sftp_open_in_editor(remote_path);
    }

    /// Latest SFTP transfer progress, or `None` if no transfer is in flight.
    /// Read by the terminal toolbar to render a stage-aware progress log.
    pub fn sftp_progress(&self) -> Option<&SftpProgress> {
        self.sftp_progress.as_ref()
    }

    pub fn set_on_backend_closed(&mut self, f: impl Fn(&mut App) + 'static) {
        self.on_backend_closed = Some(Rc::new(f));
    }

    /// Set the callback invoked whenever `sftp_progress` changes. The app
    /// uses this to trigger a re-render of the toolbar (which reads the
    /// progress snapshot) without observing every terminal repaint.
    pub fn set_on_sftp_progress_changed(&mut self, f: impl Fn(&mut App) + 'static) {
        self.on_sftp_progress_changed = Some(Rc::new(f));
    }

    /// Sets the callback invoked when an SFTP transfer finishes. The app uses
    /// this to show a success/failure toast notification.
    pub fn set_on_sftp_transfer_finished(
        &mut self,
        f: impl Fn(SftpTransferKind, bool, String, &mut App) + 'static,
    ) {
        self.on_sftp_transfer_finished = Some(Rc::new(f));
    }

    /// Attach a `CrabPortTunnel` view of this tab's backend, so the Tunnels
    /// panel can start "borrowed" tunnels reusing this SSH connection.
    /// Only set for SSH tabs (local PTY backends have no tunnel source).
    pub fn set_tunnel_source(&mut self, source: Arc<dyn CrabPortTunnel>) {
        self.tunnel_source = Some(source);
    }

    /// The tunnel source backing this tab, if it's an SSH session. Used by
    /// the Tunnels panel to start borrowed tunnels.
    pub fn tunnel_source(&self) -> Option<&Arc<dyn CrabPortTunnel>> {
        self.tunnel_source.as_ref()
    }

    /// Returns the host-key info for a currently-pending host-key prompt,
    /// if any. The prompt stays pending in the overlay until resolved via
    /// [`resolve_pending_host_key`]. Used by the global alert controller
    /// flow: `render_content` reads this to decide whether to show the
    /// alert, and the alert's confirm/cancel callbacks call
    /// [`resolve_pending_host_key`] to unblock the SSH backend.
    pub fn pending_host_key_info(&self) -> Option<HostKeyInfo> {
        self.overlay
            .lock()
            .pending_host_key
            .as_ref()
            .map(|p| p.info.clone())
    }

    /// Resolve a pending host-key prompt: `accept = true` continues the
    /// connection, `false` aborts it. No-op if no prompt is pending.
    pub fn resolve_pending_host_key(&self, accept: bool) {
        let mut ov = self.overlay.lock();
        if let Some(mut p) = ov.pending_host_key.take() {
            p.resolve(accept);
            if accept {
                ov.log(ConnectionLogLevel::Info, "Host key accepted — continuing…");
            } else {
                ov.log(
                    ConnectionLogLevel::Error,
                    "Host key rejected — connection aborted",
                );
            }
        }
    }

    pub fn reconnect(&mut self, cx: &mut Context<Self>) {
        // Try SSH first, then telnet.
        let backend: Arc<dyn crabport_terminal::terminal::CrabPortTerminal> =
            if let Some(info) = self.ssh_info.clone() {
                let verifier = crate::views::terminal::connection_overlay::make_host_key_verifier(
                    self.overlay.clone(),
                );
                let overlay_cb = self.overlay.clone();
                Arc::new(crabport_ssh::backend::SshBackend::new(
                    info,
                    80,
                    24,
                    Arc::new(move |msg: String| {
                        overlay_cb.lock().log(ConnectionLogLevel::Info, msg);
                    }),
                    Some(verifier),
                ))
            } else if let Some(info) = self.telnet_info.clone() {
                let overlay_cb = self.overlay.clone();
                Arc::new(TelnetBackend::new(
                    info,
                    80,
                    24,
                    Arc::new(move |msg: String| {
                        overlay_cb.lock().log(ConnectionLogLevel::Info, msg);
                    }),
                ))
            } else {
                return;
            };

        self.session.close();

        gpui_animation::reset_transition(&ElementId::Name(
            format!("connection-overlay-{}", self.count).into(),
        ));

        {
            let mut ov = self.overlay.lock();
            ov.update_status(RemoteStatus::Connecting, &self.remote_host);
        }

        let cols: usize = 80;
        let rows: usize = 24;

        let session = Arc::new(TerminalSession::new(backend.clone(), cols, rows));
        session.start();

        self.render_cache.lock().clear_all();

        // Backend events.
        let mut event_rx = session.subscribe_backend();
        let overlay_c = self.overlay.clone();
        let entity = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            while let Ok(event) = event_rx.recv().await {
                match event {
                    crabport_terminal::terminal::BackendEvent::Error(err) => {
                        overlay_c.lock().log(ConnectionLogLevel::Error, err);
                        let _ = entity.update(cx, |_, cx| cx.notify());
                    }
                    crabport_terminal::terminal::BackendEvent::Closed => {
                        let _ = entity.update(cx, |this, cx| {
                            if let Some(ref cb) = this.on_backend_closed {
                                let cb = cb.clone();
                                cx.defer(move |cx| cb(cx));
                            } else {
                                this.overlay
                                    .lock()
                                    .log(ConnectionLogLevel::Warning, "Connection closed");
                            }
                            cx.notify();
                        });
                    }
                    crabport_terminal::terminal::BackendEvent::SftpTransferFinished {
                        kind,
                        success,
                        message,
                    } => {
                        let level = if success {
                            ConnectionLogLevel::Info
                        } else {
                            ConnectionLogLevel::Error
                        };
                        let prefix = match kind {
                            crabport_terminal::terminal::SftpTransferKind::Download => "Download",
                            crabport_terminal::terminal::SftpTransferKind::Upload => "Upload",
                            crabport_terminal::terminal::SftpTransferKind::Rename => "Rename",
                            crabport_terminal::terminal::SftpTransferKind::Edit => "Edit",
                        };
                        overlay_c.lock().log(level, format!("{prefix}: {message}"));
                        let _ = entity.update(cx, |this, cx| {
                            this.sftp_progress = None;
                            if success {
                                if let Some(cwd) = this
                                    .session
                                    .sftp_cwd()
                                    .as_ref()
                                    .map(|c| c.as_str().to_string())
                                {
                                    this.session.sftp_navigate(&cwd);
                                }
                            }
                            let cb = this.on_sftp_progress_changed.clone();
                            let cb_kind = kind;
                            let cb_success = success;
                            let cb_message = message.clone();
                            let finished_cb = this.on_sftp_transfer_finished.clone();
                            cx.notify();
                            if let Some(cb) = cb {
                                cx.defer(move |cx| cb(cx));
                            }
                            if let Some(cb) = finished_cb {
                                cx.defer(move |cx| cb(cb_kind, cb_success, cb_message, cx));
                            }
                        });
                    }
                    crabport_terminal::terminal::BackendEvent::SftpTransferProgress {
                        kind,
                        stage,
                        message,
                        bytes,
                    } => {
                        let _ = entity.update(cx, |this, cx| {
                            this.sftp_progress = Some(SftpProgress {
                                kind,
                                stage,
                                message,
                                bytes,
                            });
                            let cb = this.on_sftp_progress_changed.clone();
                            cx.notify();
                            if let Some(cb) = cb {
                                cx.defer(move |cx| cb(cx));
                            }
                        });
                    }
                    crabport_terminal::terminal::BackendEvent::Data(_) => {}
                }
            }
        })
        .detach();

        // Wakeup → dirty.
        let mut wakeup_rx = session.subscribe_wakeup();
        let dirty_wk = self.needs_repaint.clone();
        let status_entity = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            while let Ok(()) = wakeup_rx.recv().await {
                let _ = status_entity.update(cx, |this, _cx| {
                    if let Some(m) = this.session.monitor() {
                        let new_status = m.status();
                        let mut ov = this.overlay.lock();
                        if new_status != ov.status {
                            ov.update_status(new_status, &this.remote_host);
                        }
                    }
                });
                dirty_wk.store(true, Ordering::Release);
            }
        })
        .detach();

        // Fade watcher.
        let overlay_fade = self.overlay.clone();
        let dirty_fade = self.needs_repaint.clone();
        let fade_entity = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            loop {
                smol::Timer::after(std::time::Duration::from_millis(50)).await;
                if overlay_fade.lock().fade_out_started {
                    break;
                }
            }
            smol::Timer::after(std::time::Duration::from_millis(600)).await;
            overlay_fade.lock().mark_hidden();
            dirty_fade.store(true, Ordering::Release);
            let _ = fade_entity.update(cx, |_, cx| cx.notify());
        })
        .detach();

        self.session = session;
        self.backend = backend;
        cx.notify();
    }

    fn resolve_keystroke(
        keystroke: &Keystroke,
        bindings: &[keybind::Binding],
    ) -> Option<KeyAction> {
        if let Some(action) = keybind::resolve(keystroke, bindings) {
            return Some(action.clone());
        }
        // NOTE: plain printable characters (no modifiers) are intentionally
        // NOT converted to `KeyAction::Bytes` here. They are delivered to the
        // PTY via the IME input handler (`EntityInputHandler::replace_text_in_range`)
        // instead, which is the only way to make CJK IME composition work on
        // macOS: the platform intercepts the keydown and routes it through
        // `NSTextInputClient`, calling `setMarkedText:` while composing and
        // `insertText:` on commit. If we wrote the raw `key_char` here we
        // would (a) double-write on plain English key-repeat and (b) break
        // IME composition entirely because the key would never reach the
        // input context.
        None
    }

    fn copy_selected_text(session: &Arc<TerminalSession>, sel: &Selection) -> String {
        session.with_term(|term| {
            let grid = term.grid();
            let num_cols = grid.columns();
            // Selection rows are stored as absolute grid lines, so they stay
            // anchored to the text regardless of the current display_offset.
            // We must NOT clamp them to the visible viewport: a selection that
            // spans into scrollback (or that the user scrolled away from after
            // selecting) still refers to valid grid rows and must be copied in
            // full. The grid's ring-buffer storage covers the entire scrollback,
            // so indexing by absolute line is always valid.
            let (sr, er, sc, ec) = sel.range();
            let mut result = String::new();
            for row in sr..=er {
                if row > sr {
                    result.push('\n');
                }
                let li = alacritty_terminal::index::Line(row);
                // `range()` normalizes sr<=er and returns (sc, ec) such that
                // the first grid row (sr) starts at column `sc` and the last
                // grid row (er) ends at column `ec` (inclusive), matching the
                // visual highlight painted in the render loop. This holds for
                // both top-down and bottom-up selections, so the column
                // trimming logic is the same in both cases.
                let cs = if row == sr { sc } else { 0 };
                let ce = if row == er { ec + 1 } else { num_cols };
                let mut line_text = String::new();
                for col in cs..ce.min(num_cols) {
                    let cell = &grid[li][alacritty_terminal::index::Column(col)];
                    line_text.push(cell.c);
                }
                result.push_str(line_text.trim_end());
            }
            result
        })
    }
}
// ---- GPUI Render ----

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_paste {
            self.pending_paste = false;
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.session.write(text.as_bytes());
            }
        }
        if self.pending_copy {
            self.pending_copy = false;
            let text = if let Some(ref sel) = *self.selection.lock() {
                Self::copy_selected_text(&self.session, sel)
            } else {
                self.session.with_term(|term| {
                    let grid = term.grid();
                    let display_offset = grid.display_offset();
                    let num_cols = grid.columns();
                    let num_lines = grid.screen_lines();
                    let mut result = String::new();
                    for row in 0..num_lines {
                        let li =
                            alacritty_terminal::index::Line(row as i32 - display_offset as i32);
                        let mut line_text = String::new();
                        for col in 0..num_cols {
                            let cell = &grid[li][alacritty_terminal::index::Column(col)];
                            line_text.push(cell.c);
                        }
                        let trimmed = line_text.trim_end();
                        if !trimmed.is_empty() || row + 1 < num_lines {
                            result.push_str(trimmed);
                            if row + 1 < num_lines {
                                result.push('\n');
                            }
                        }
                    }
                    result
                })
            };
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }

        let session_c = self.session.clone();
        let session = session_c.clone();
        let font_size = self.font_size;
        let line_height = self.line_height;
        let cell_width = self.cell_width;
        let focus_handle = self.focus_handle.clone();
        let last_bounds_c = self.last_bounds.clone();
        let last_bounds = last_bounds_c.clone();
        let selection = self.selection.clone();
        let selection_prepaint = selection.clone();
        let selection_c = selection.clone();
        let render_cache = self.render_cache.clone();
        let render_cache_paint = render_cache.clone();
        let needs_repaint = self.needs_repaint.clone();
        let entity = cx.entity().downgrade();
        let display_offset_atomic = self.display_offset.clone();
        let display_offset_mouse = self.display_offset.clone();
        let display_offset_mouse_move = self.display_offset.clone();
        let display_offset_mouse_up = self.display_offset.clone();
        let history_size_atomic = self.history_size.clone();
        let visible_rows_atomic = self.visible_rows.clone();
        let history_size_sb = self.history_size.clone();
        let visible_rows_sb = self.visible_rows.clone();
        let display_offset_sb = self.display_offset.clone();
        let scrollbar_dragging = self.scrollbar_dragging.clone();
        let scrollbar_drag_offset = self.scrollbar_drag_offset.clone();
        // Clones used by the paint closure to register global mouse capture
        // while the scrollbar thumb is being dragged. Registering via
        // `window.on_mouse_event` (instead of a child overlay div) means the
        // drag keeps receiving move/up events even when the cursor leaves the
        // terminal bounds — so dragging the scrollbar and moving the mouse
        // over the tab bar or SFTP panel no longer aborts the drag or loses
        // focus.
        let scrollbar_dragging_paint = self.scrollbar_dragging.clone();
        let scrollbar_drag_offset_paint = self.scrollbar_drag_offset.clone();
        let last_bounds_paint = self.last_bounds.clone();
        let session_paint = self.session.clone();
        let needs_repaint_paint = self.needs_repaint.clone();
        let display_offset_paint = self.display_offset.clone();
        let history_size_paint = self.history_size.clone();
        let visible_rows_paint = self.visible_rows.clone();
        // Clones used by the paint closure to (a) register the IME input
        // handler so the platform can route composition events (Chinese /
        // Japanese / Korean input) to the terminal, and (b) refresh the
        // cached cursor bounds each frame so `bounds_for_range` can position
        // the IME candidate window.
        let marked_text_paint = self.marked_text.clone();
        let cursor_bounds_paint = self.cursor_bounds.clone();
        let entity_input = cx.entity();
        let focus_handle_input = self.focus_handle.clone();

        let ov = self.overlay.lock();
        let overlay_visible = ov.is_visible();
        let is_fading_out = ov.is_fading_out();
        let log_entries: Vec<ConnectionLogEntry> = ov.logs.clone();
        let current_status = ov.status;
        let spinner_rotation_mrad = ov.spinner_rotation.load(Ordering::Relaxed);
        drop(ov);

        let is_remote = !self.remote_host.is_empty();

        div()
            .id(ElementId::Name(
                format!("terminal-view-{}", self.count).into(),
            ))
            .relative()
            .size_full()
            .overflow_hidden()
            .cursor_text()
            .bg(rgb(term_bg()))
            .track_focus(&focus_handle)
            .key_context("CrabPortTerminal")
            .on_action(cx.listener(|this, _: &TerminalTab, _window, cx| {
                this.session.write(b"	");
                this.session.scroll_to_bottom();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &TerminalShiftTab, _window, cx| {
                this.session.write(b"\x1b[Z");
                this.session.scroll_to_bottom();
                cx.notify();
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                match Self::resolve_keystroke(&event.keystroke, &this.bindings) {
                    Some(KeyAction::Action(TerminalAction::Copy)) => {
                        this.pending_copy = true;
                        cx.notify();
                    }
                    Some(KeyAction::Action(TerminalAction::Paste)) => {
                        this.pending_paste = true;
                        cx.notify();
                    }
                    Some(KeyAction::Bytes(bytes)) => {
                        this.session.write(&bytes);
                        this.session.scroll_to_bottom();
                        cx.notify();
                        // Stop propagation so the platform's IME input context
                        // doesn't also receive this keydown. These Bytes come
                        // from keybind matches (Enter, Tab, Backspace, arrows,
                        // Ctrl-sequences) — i.e. control sequences, not text —
                        // and we don't want the IME to swallow or echo them.
                        cx.stop_propagation();
                    }
                    None => {}
                }
            }))
            .child(
                canvas(
                    // ---- prepaint: resize + try-lock incremental snapshot ----
                    move |bounds, _window, _cx| {
                        let mut last = last_bounds.lock();
                        let (cols, rows) = {
                            let c = (bounds.size.width / cell_width).floor() as usize;
                            let r = (bounds.size.height / line_height).floor() as usize;
                            (c.max(2), r.max(1))
                        };

                        let mut resized = false;
                        if let Some(ref lb) = *last {
                            let (lc, lr) = {
                                let c = (lb.size.width / cell_width).floor() as usize;
                                let r = (lb.size.height / line_height).floor() as usize;
                                (c.max(2), r.max(1))
                            };
                            if lc != cols || lr != rows {
                                session.resize(cols as u16, rows as u16);
                                resized = true;
                                *selection_prepaint.lock() = None;
                            }
                        } else {
                            session.resize(cols as u16, rows as u16);
                            resized = true;
                            *selection_prepaint.lock() = None;
                        }
                        *last = Some(bounds);

                        let pal = palette();

                        // Try to update the snapshot without stalling. If the
                        // reader holds the lock, reuse last frame's snapshot.
                        let got = session.try_with_term_mut(|term| {
                            let mut cache = render_cache.lock();

                            let grid_cols = term.grid().columns();
                            let grid_lines = term.grid().screen_lines();
                            let offset = term.grid().display_offset();

                            if resized || cache.cols != grid_cols || cache.rows_count != grid_lines
                            {
                                cache.resize(grid_cols, grid_lines);
                            }

                            // Collect dirty rows from alacritty damage.
                            let mut full = false;
                            let mut dirty_rows: Vec<usize> = Vec::new();
                            match term.damage() {
                                TermDamage::Full => full = true,
                                TermDamage::Partial(iter) => {
                                    for ld in iter {
                                        if ld.line < grid_lines {
                                            dirty_rows.push(ld.line);
                                        }
                                    }
                                }
                            }
                            term.reset_damage();

                            let grid = term.grid();
                            let update_row = |row: usize, cache: &mut RenderCache| {
                                let li =
                                    alacritty_terminal::index::Line(row as i32 - offset as i32);
                                let mut cells = Vec::with_capacity(grid_cols);
                                let mut has_bg = false;
                                for col in 0..grid_cols {
                                    let cell = &grid[li][alacritty_terminal::index::Column(col)];
                                    let custom_bg = cell.bg != Color::Named(NamedColor::Background);
                                    if custom_bg || cell.flags.contains(Flags::INVERSE) {
                                        has_bg = true;
                                    }
                                    cells.push(CellSnap {
                                        c: cell.c,
                                        fg: ansi_color_to_rgb(&cell.fg, pal),
                                        bg: ansi_color_to_rgb(&cell.bg, pal),
                                        flags: cell.flags,
                                        custom_bg,
                                    });
                                }
                                let h = hash_row(&cells);
                                cache.rows[row] = RowSnapshot {
                                    cells,
                                    hash: h,
                                    has_bg,
                                };
                            };

                            if full {
                                for row in 0..grid_lines {
                                    update_row(row, &mut cache);
                                }
                            } else {
                                for &row in &dirty_rows {
                                    update_row(row, &mut cache);
                                }
                            }

                            // Skip the expensive renderable_content() call; we
                            // only need cursor point + shape for rendering.
                            let cursor_point = term.grid().cursor.point;
                            let cursor_shape = term.cursor_style().shape;
                            let history_size = grid.history_size() as i32;
                            // Persist offset for mouse handlers.
                            display_offset_atomic.store(offset as i32, Ordering::Relaxed);
                            history_size_atomic.store(history_size, Ordering::Relaxed);
                            visible_rows_atomic.store(grid_lines as i32, Ordering::Relaxed);
                            (
                                Some((cursor_point, cursor_shape)),
                                grid_cols,
                                grid_lines,
                                offset as i32,
                                history_size,
                            )
                        });

                        match got {
                            Some(v) => v,
                            None => {
                                let cache = render_cache.lock();
                                (None, cache.cols, cache.rows_count, 0, 0)
                            }
                        }
                    },
                    // ---- paint: hash-keyed LRU shaped lines ----
                    move |bounds, lines, window, cx| {
                        let (cursor, num_cols, _num_lines, display_offset, _history_size) = lines;
                        // cursor is Option<(Point, CursorShape)>
                        let text_system = window.text_system().clone();

                        // While the scrollbar thumb is being dragged, register
                        // global (window-level) capture-phase listeners for
                        // mouse-move and mouse-up. These fire regardless of
                        // which element is currently under the cursor, so the
                        // drag continues smoothly even if the mouse leaves the
                        // terminal area. The listeners are registered per-frame
                        // and only while dragging, so there is zero overhead in
                        // the common (non-dragging) case.
                        if scrollbar_dragging_paint.load(Ordering::Acquire)
                            && history_size_paint.load(Ordering::Relaxed) > 0
                        {
                            window.on_mouse_event({
                                let dragging = scrollbar_dragging_paint.clone();
                                let drag_offset = scrollbar_drag_offset_paint.clone();
                                let last_bounds = last_bounds_paint.clone();
                                let session = session_paint.clone();
                                let needs_repaint = needs_repaint_paint.clone();
                                let display_offset_atomic = display_offset_paint.clone();
                                let history = history_size_paint.clone();
                                let visible = visible_rows_paint.clone();
                                move |event: &MouseMoveEvent, phase, _window, _cx| {
                                    if phase != DispatchPhase::Capture
                                        || !dragging.load(Ordering::Acquire)
                                    {
                                        return;
                                    }
                                    let Some(bounds) = *last_bounds.lock() else {
                                        return;
                                    };
                                    let history = history.load(Ordering::Relaxed) as f32;
                                    let visible = visible.load(Ordering::Relaxed) as f32;
                                    if history <= 0.0 {
                                        return;
                                    }
                                    let drag_offset = *drag_offset.lock();
                                    let track_h = (f32::from(bounds.size.height) - 4.0).max(0.0);
                                    let thumb_h = track_h * (visible / (history + visible));
                                    let new_thumb_top = ((f32::from(event.position.y)
                                        - f32::from(bounds.origin.y))
                                        - drag_offset)
                                        .clamp(0.0, (track_h - thumb_h).max(0.0));
                                    let new_y_frac = if track_h > 0.0 {
                                        new_thumb_top / track_h
                                    } else {
                                        0.0
                                    };
                                    let total = history + visible;
                                    let new_offset = (history - new_y_frac * total).round() as i32;
                                    let cur_offset = display_offset_atomic.load(Ordering::Relaxed);
                                    let delta = new_offset - cur_offset;
                                    if delta != 0 {
                                        // Immediately update the atomic so the
                                        // next move event sees the new offset,
                                        // preventing repeated scrolls with a
                                        // stale cur_offset.
                                        display_offset_atomic.store(new_offset, Ordering::Relaxed);
                                        session.scroll(delta);
                                        needs_repaint.store(true, Ordering::Release);
                                    }
                                }
                            });
                            window.on_mouse_event({
                                let dragging = scrollbar_dragging_paint.clone();
                                move |_event: &MouseUpEvent, phase, _window, _cx| {
                                    if phase == DispatchPhase::Capture {
                                        dragging.store(false, Ordering::Release);
                                    }
                                }
                            });
                        }

                        let sel_guard = selection.lock();
                        let sel: Option<Selection> = sel_guard.clone();
                        drop(sel_guard);

                        let mut cache = render_cache_paint.lock();

                        // Single viewport-wide background fill.
                        window.paint_quad(fill(
                            Bounds::new(bounds.origin, bounds.size),
                            rgb(term_bg()),
                        ));

                        let row_count = cache.rows_count;
                        for row_idx in 0..row_count {
                            let y = bounds.origin.y + line_height * row_idx as f32;

                            // Convert selection grid lines to viewport rows.
                            // viewport_row = grid_line + display_offset
                            let (sel_start, sel_end) = if let Some(ref s) = sel {
                                let (sr, er, sc, ec) = s.range();
                                let vp_sr = sr + display_offset;
                                let vp_er = er + display_offset;
                                let ri = row_idx as i32;
                                if ri < vp_sr || ri > vp_er {
                                    (None, None)
                                } else if vp_sr == vp_er {
                                    let lo = sc.min(num_cols);
                                    let hi = (ec + 1).min(num_cols).max(lo + 1);
                                    (Some(lo), Some(hi))
                                } else if ri == vp_sr {
                                    let col = if s.start_row <= s.end_row {
                                        s.start_col
                                    } else {
                                        s.end_col
                                    };
                                    (Some(col.min(num_cols)), Some(num_cols))
                                } else if ri == vp_er {
                                    let col = if s.start_row <= s.end_row {
                                        s.end_col
                                    } else {
                                        s.start_col
                                    };
                                    (Some(0), Some(col.saturating_add(1).min(num_cols)))
                                } else {
                                    (Some(0), Some(num_cols))
                                }
                            } else {
                                (None, None)
                            };

                            let row_selected = sel_start.is_some();
                            let row = &cache.rows[row_idx];

                            // Background layer: only if the row needs it.
                            if row.has_bg || row_selected {
                                let mut rects: Vec<(usize, usize, Hsla)> = Vec::new();
                                for (ci, cell) in row.cells.iter().enumerate() {
                                    if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                                        continue;
                                    }
                                    let is_sel = sel_start
                                        .is_some_and(|ss| ci >= ss && ci < sel_end.unwrap_or(0));
                                    let is_inv = cell.flags.contains(Flags::INVERSE);
                                    let wide = cell.flags.contains(Flags::WIDE_CHAR);

                                    let bg_color: Option<Hsla> = if is_sel {
                                        Some(rgb(selection_bg()).into())
                                    } else if is_inv {
                                        Some(rgb(cell.fg).into())
                                    } else if cell.custom_bg {
                                        Some(rgb(cell.bg).into())
                                    } else {
                                        None
                                    };

                                    if let Some(color) = bg_color {
                                        let n = if wide { 2 } else { 1 };
                                        if let Some(last) = rects.last_mut() {
                                            if last.0 + last.1 == ci && last.2 == color {
                                                last.1 += n;
                                                continue;
                                            }
                                        }
                                        rects.push((ci, n, color));
                                    }
                                }
                                for (col, n, color) in rects {
                                    let cell_x = bounds.origin.x + col as f32 * cell_width;
                                    window.paint_quad(fill(
                                        Bounds::new(
                                            point(cell_x, y),
                                            size(cell_width * n as f32, line_height),
                                        ),
                                        color,
                                    ));
                                }
                            }

                            // Text layer: hash-keyed LRU; reshape only on miss.
                            //
                            // We pass `force_width = Some(cell_width)` so every
                            // glyph is laid out on a strict monospace grid
                            // where glyph N sits at x = N * cell_width. This is
                            // essential for CJK characters: alacritty gives a
                            // wide char two grid columns + a WIDE_CHAR_SPACER
                            // (so the cell grid treats it as 2 cells wide),
                            // but the underlying font's natural advance for
                            // that glyph is typically ~1.7-1.8x the ASCII
                            // advance rather than exactly 2x. Without forcing,
                            // the shaped line's natural width drifts relative
                            // to the cell grid, so backgrounds, the cursor,
                            // selection rects, and following glyphs all end up
                            // misaligned by a growing offset. `force_width`
                            // repositions each glyph to its grid slot based on
                            // its index in the shaped run, so a wide CJK char
                            // (1 glyph spanning 2 cells) lands exactly at
                            // 2 * cell_width. ASCII chars (1 glyph = 1 cell)
                            // are unaffected since their natural advance is
                            // already cell_width.
                            let hash = row.hash;
                            if cache.shaped.peek(&hash).is_none() {
                                let (line_text, runs) =
                                    build_runs(&cache.rows[row_idx].cells, num_cols);
                                if !line_text.is_empty() && !runs.is_empty() {
                                    let shaped = text_system.shape_line(
                                        line_text.into(),
                                        font_size,
                                        &runs,
                                        Some(cell_width),
                                    );
                                    cache.shaped.put(hash, shaped);
                                }
                            }
                            if let Some(shaped) = cache.shaped.get(&hash) {
                                let _ = shaped.paint(
                                    point(bounds.origin.x, y),
                                    line_height,
                                    window,
                                    cx,
                                );
                            }
                        }

                        drop(cache);

                        // Cursor (no reshape involved).
                        // cursor.point.line is a grid line; convert to viewport row.
                        if let Some((cursor_point, cursor_shape)) = cursor
                            && cursor_shape != CursorShape::Hidden
                        {
                            let cursor_vp_row = cursor_point.line.0 + display_offset;
                            if cursor_vp_row >= 0 && cursor_vp_row < row_count as i32 {
                                let cx_x =
                                    bounds.origin.x + cursor_point.column.0 as f32 * cell_width;
                                let cx_y = bounds.origin.y + cursor_vp_row as f32 * line_height;
                                // Cache the cursor's window-space bounds so the
                                // IME input handler can position the candidate
                                // window there via `bounds_for_range`.
                                *cursor_bounds_paint.lock() =
                                    Bounds::new(point(cx_x, cx_y), size(cell_width, line_height));
                                paint_cursor(
                                    cursor_shape,
                                    cx_x,
                                    cx_y,
                                    cell_width,
                                    line_height,
                                    window,
                                );
                            }
                        }

                        // Render IME preedit (marked) text inline at the cursor
                        // so the user sees live composition feedback while typing
                        // Chinese/Japanese/Korean. The text is drawn at the
                        // terminal cursor position with a subtle underline so it
                        // reads as in-progress input rather than committed text.
                        let marked = marked_text_paint.lock().clone();
                        if let Some(text) = marked
                            && !text.is_empty()
                        {
                            let cb = *cursor_bounds_paint.lock();
                            let preedit_run = crate::views::terminal::runs::make_run(
                                text.len(),
                                false,
                                false,
                                term_fg(),
                                false,
                                0,
                                true,
                            );
                            let runs = vec![preedit_run];
                            let shaped =
                                text_system.shape_line(text.clone().into(), font_size, &runs, None);
                            let _ = shaped.paint(
                                point(cb.origin.x, cb.origin.y),
                                line_height,
                                window,
                                cx,
                            );
                        }

                        // Register the IME input handler so the platform routes
                        // composition events (Chinese/Japanese/Korean input) to
                        // this terminal view. `handle_input` is a no-op when the
                        // view is not focused, so this is safe to call every frame.
                        window.handle_input(
                            &focus_handle_input,
                            gpui::ElementInputHandler::new(bounds, entity_input.clone()),
                            cx,
                        );

                        // Scrollbar is rendered as an interactive overlay div outside
                        // the canvas; nothing to paint here.
                    },
                )
                .size_full(),
            )
            // Transparent overlay div for mouse events (selection + scroll).
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .on_scroll_wheel({
                        let session = session_c.clone();
                        let needs_repaint = needs_repaint.clone();
                        let entity = entity.clone();
                        let line_height = line_height;
                        move |event, _window, cx| {
                            let delta = event.delta.pixel_delta(line_height);
                            let dy = delta.y / line_height;
                            if dy.abs() < 0.001 {
                                return;
                            }
                            let _ = entity.update(cx, |this, _cx| {
                                this.scroll_accumulator += dy;
                                let lines = this.scroll_accumulator.trunc() as i32;
                                if lines != 0 {
                                    this.scroll_accumulator -= lines as f32;
                                    session.scroll(lines);
                                }
                            });
                            // Notify immediately for low-latency scroll feedback;
                            // the frame pump coalesces subsequent PTY-driven repaints.
                            needs_repaint.store(true, Ordering::Release);
                            let _ = entity.update(cx, |_, cx| cx.notify());
                        }
                    })
                    .on_mouse_down(MouseButton::Left, {
                        let selection = selection_c.clone();
                        let last_bounds = last_bounds_c.clone();
                        let needs_repaint = needs_repaint.clone();
                        let cell_width = cell_width;
                        let line_height = line_height;
                        let display_offset_mouse = display_offset_mouse.clone();
                        let scrollbar_dragging_down = scrollbar_dragging.clone();
                        move |event, _window, _cx| {
                            // If the click landed on the scrollbar area, skip selection.
                            if scrollbar_dragging_down.load(Ordering::Acquire) {
                                return;
                            }
                            if let Some(bounds) = *last_bounds.lock() {
                                // Skip if click is in the scrollbar region (right 10px).
                                let in_scrollbar = event.position.x
                                    > bounds.origin.x + bounds.size.width - px(10.0);
                                if in_scrollbar {
                                    return;
                                }
                                let offset = display_offset_mouse.load(Ordering::Relaxed);
                                if let Some((col, row)) = mouse_to_grid(
                                    event.position,
                                    bounds,
                                    cell_width,
                                    line_height,
                                    offset,
                                ) {
                                    *selection.lock() = Some(Selection::new(col, row));
                                    needs_repaint.store(true, Ordering::Release);
                                }
                            }
                        }
                    })
                    .on_mouse_move({
                        let selection = selection_c.clone();
                        let last_bounds = last_bounds_c.clone();
                        let needs_repaint = needs_repaint.clone();
                        let cell_width = cell_width;
                        let line_height = line_height;
                        let display_offset_mouse_move = display_offset_mouse_move.clone();
                        let scrollbar_dragging_move = scrollbar_dragging.clone();
                        move |event, _window, _cx| {
                            if scrollbar_dragging_move.load(Ordering::Acquire) {
                                return;
                            }
                            if event.dragging() {
                                if let Some(bounds) = *last_bounds.lock() {
                                    let offset = display_offset_mouse_move.load(Ordering::Relaxed);
                                    if let Some((col, row)) = mouse_to_grid(
                                        event.position,
                                        bounds,
                                        cell_width,
                                        line_height,
                                        offset,
                                    ) {
                                        if let Some(ref mut sel) = *selection.lock() {
                                            sel.end_col = col;
                                            sel.end_row = row;
                                            needs_repaint.store(true, Ordering::Release);
                                        }
                                    }
                                }
                            }
                        }
                    })
                    .on_mouse_up(MouseButton::Left, {
                        let selection = selection_c.clone();
                        let last_bounds = last_bounds_c.clone();
                        let needs_repaint = needs_repaint.clone();
                        let cell_width = cell_width;
                        let line_height = line_height;
                        let display_offset_mouse_up = display_offset_mouse_up.clone();
                        move |event, _window, _cx| {
                            if let Some(bounds) = *last_bounds.lock() {
                                let offset = display_offset_mouse_up.load(Ordering::Relaxed);
                                if let Some((up_col, up_row)) = mouse_to_grid(
                                    event.position,
                                    bounds,
                                    cell_width,
                                    line_height,
                                    offset,
                                ) {
                                    let sel_guard = selection.lock();
                                    let clear = if let Some(ref sel) = *sel_guard {
                                        sel.start_col == up_col && sel.start_row == up_row
                                    } else {
                                        false
                                    };
                                    drop(sel_guard);
                                    if clear {
                                        *selection.lock() = None;
                                    } else if let Some(ref mut sel) = *selection.lock() {
                                        sel.active = false;
                                    }
                                }
                            }
                            needs_repaint.store(true, Ordering::Release);
                        }
                    }),
            )
            // Scrollbar overlay: only visible when there is scrollback history.
            // The thumb is draggable to scroll.
            .when(history_size_sb.load(Ordering::Relaxed) > 0, |el| {
                let history = history_size_sb.load(Ordering::Relaxed) as f32;
                let visible = visible_rows_sb.load(Ordering::Relaxed) as f32;
                let offset = display_offset_sb.load(Ordering::Relaxed) as f32;
                let total = history + visible;
                let thumb_h_frac = (visible / total).clamp(0.04, 1.0);
                // display_offset=0 → viewport at bottom (newest) → thumb at bottom.
                // display_offset=history → viewport at top → thumb at top.
                // thumb_y_frac: 0=top, 1=bottom. So thumb_y_frac = 1 - offset/history ... but
                // we also account for thumb height. Position the thumb so its top represents
                // the fraction of content scrolled past at the top.
                //
                // content above viewport top = history - offset
                // fraction of total content above viewport top = (history - offset) / total
                // thumb_top_frac = (history - offset) / total, clamped.
                let thumb_y_frac = ((history - offset) / total).clamp(0.0, 1.0 - thumb_h_frac);

                let scrollbar_dragging_c = scrollbar_dragging.clone();
                let scrollbar_drag_offset_c = scrollbar_drag_offset.clone();
                let last_bounds_sb = last_bounds_c.clone();

                el.child(
                    div()
                        .id("terminal-scrollbar")
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .w(px(10.0))
                        .child(
                            div()
                                .id("terminal-scrollbar-thumb")
                                .absolute()
                                .top(DefiniteLength::Fraction(thumb_y_frac))
                                .right_1()
                                .h(DefiniteLength::Fraction(thumb_h_frac))
                                .w(px(6.0))
                                .rounded_full()
                                .bg(rgb(0x4d4e50))
                                .cursor_pointer()
                                .on_mouse_down(MouseButton::Left, {
                                    let scrollbar_drag_offset_c = scrollbar_drag_offset_c.clone();
                                    let scrollbar_dragging_c = scrollbar_dragging_c.clone();
                                    let last_bounds_sb = last_bounds_sb.clone();
                                    move |event, _window, _cx| {
                                        scrollbar_dragging_c.store(true, Ordering::Release);
                                        if let Some(bounds) = *last_bounds_sb.lock() {
                                            let thumb_top =
                                                bounds.origin.y + bounds.size.height * thumb_y_frac;
                                            *scrollbar_drag_offset_c.lock() =
                                                (event.position.y - thumb_top) / px(1.0);
                                        }
                                    }
                                }),
                        ),
                )
            })
            // Connection overlay (remote sessions only).
            //
            // Note: the host-key confirmation prompt is no longer rendered
            // here. It is surfaced via the global `AlertController` (held by
            // `CrabportApp`), which `render_content` triggers when it sees a
            // pending host key on the active terminal view. That way the
            // dialog overlays the whole window and is unaffected by the
            // terminal container's padding.
            .when(is_remote, |el| {
                let on_reconnect: Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)> =
                    Rc::new(cx.listener(|this, _event: &ClickEvent, _window, cx| {
                        this.reconnect(cx);
                    }));
                el.child(render_connection_overlay(
                    overlay_visible,
                    is_fading_out,
                    current_status,
                    &log_entries,
                    self.count,
                    spinner_rotation_mrad,
                    Some(on_reconnect),
                ))
            })
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl CrabPortTab for TerminalView {
    fn close(&mut self) {
        self.session.close();
    }
}

/// IME / text input integration.
///
/// The terminal is modeled as a virtual document whose only content is the
/// current IME preedit (marked) text, with the cursor at the end. Committed
/// text is written straight to the PTY — there is no editable buffer to
/// update, so the range arguments from the platform are effectively ignored.
/// This is the same approach terminal emulators like Alacritty use for IME.
impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        _adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        // The only "document" text we expose is the current preedit text.
        let marked = self.marked_text.lock().clone().unwrap_or_default();
        let end = range.end.min(marked.len());
        if range.start >= end {
            Some(String::new())
        } else {
            Some(marked[range.start..end].to_string())
        }
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        // Report a zero-length selection at the end of the (virtual) document
        // so the platform positions IME composition at the cursor. Length is
        // the current preedit text length in UTF-16 units.
        let marked_len = self
            .marked_text
            .lock()
            .as_ref()
            .map(|s| s.encode_utf16().count())
            .unwrap_or(0);
        Some(UTF16Selection {
            range: marked_len..marked_len,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        // Marked range covers the entire preedit text, expressed in UTF-16
        // units (the platform convention for NSTextInputClient ranges).
        self.marked_text.lock().as_ref().map(|s| {
            let len = s.encode_utf16().count();
            0..len
        })
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let changed = self.marked_text.lock().take().is_some();
        if changed {
            self.needs_repaint.store(true, Ordering::Release);
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Commit: write the final text to the PTY and clear any preedit.
        // The range is ignored — the terminal has no editable buffer, so we
        // always insert at the cursor regardless of what the platform passes.
        if !text.is_empty() {
            self.session.write(text.as_bytes());
            self.session.scroll_to_bottom();
        }
        let had_marked = self.marked_text.lock().take().is_some();
        if had_marked || !text.is_empty() {
            self.needs_repaint.store(true, Ordering::Release);
            cx.notify();
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Update the live preedit text. We do NOT write to the PTY here —
        // the text is only sent once the IME commits via `replace_text_in_range`.
        // An empty `new_text` cancels the composition (clears preedit).
        let new = if new_text.is_empty() {
            None
        } else {
            Some(new_text.to_string())
        };
        let changed = *self.marked_text.lock() != new;
        if changed {
            *self.marked_text.lock() = new;
            self.needs_repaint.store(true, Ordering::Release);
            cx.notify();
        }
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // Position the IME candidate window just past the end of any in-flight
        // preedit text so it doesn't overlap the composition itself; if there
        // is no preedit, anchor at the terminal cursor. We approximate the
        // preedit width as (char count * cell_width) which is accurate for
        // ASCII pinyin and CJK wide chars (each 2 cells) — close enough for
        // popup anchoring, the platform reflows the candidate window anyway.
        let cb = *self.cursor_bounds.lock();
        let mut origin_x = cb.origin.x;
        if let Some(marked) = self.marked_text.lock().as_ref() {
            // Count display columns: wide CJK chars take 2 cells, others 1.
            let cols: usize = marked
                .chars()
                .map(|c| if c.is_ascii() { 1 } else { 2 })
                .sum();
            origin_x = cb.origin.x + self.cell_width * cols as f32;
        }
        Some(Bounds::new(
            point(origin_x, cb.origin.y),
            size(self.cell_width, self.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        // The terminal has no hit-testable character grid from the IME's
        // perspective; returning the end of the document is a safe default.
        self.marked_text
            .lock()
            .as_ref()
            .map(|s| s.encode_utf16().count())
    }
}

/// Paint the terminal cursor as one or more quads.
/// Paint the terminal cursor as one or more quads.
#[allow(clippy::too_many_arguments)]
fn paint_cursor(
    shape: CursorShape,
    cx_x: Pixels,
    cx_y: Pixels,
    cell_width: Pixels,
    line_height: Pixels,
    window: &mut Window,
) {
    match shape {
        CursorShape::Block => {
            let c: Hsla = rgb(term_cursor()).into();
            window.paint_quad(fill(
                Bounds::new(point(cx_x, cx_y), size(cell_width, line_height)),
                c.opacity(0.5),
            ));
        }
        CursorShape::HollowBlock => {
            window.paint_quad(outline(
                Bounds::new(point(cx_x, cx_y), size(cell_width, line_height)),
                rgb(term_cursor()),
                BorderStyle::Solid,
            ));
        }
        CursorShape::Underline => {
            window.paint_quad(fill(
                Bounds::new(
                    point(cx_x, cx_y + line_height - px(2.0)),
                    size(cell_width, px(2.0)),
                ),
                rgb(term_cursor()),
            ));
        }
        CursorShape::Beam => {
            window.paint_quad(fill(
                Bounds::new(point(cx_x, cx_y), size(px(1.5), line_height)),
                rgb(term_cursor()),
            ));
        }
        CursorShape::Hidden => {}
    }
}
