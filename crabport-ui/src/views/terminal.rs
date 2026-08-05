use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU32, Ordering},
};

use alacritty_terminal::{
    grid::Dimensions,
    term::TermDamage,
    term::cell::Flags,
    vte::ansi::{Color, CursorShape, NamedColor},
};
use crabport_core::keybind::{self, KeyAction, TerminalAction};
use crabport_serial::backend::SerialBackend;
use crabport_serial::session::SerialConnectionInfo;
use crabport_ssh::CrabPortTunnel;
use crabport_ssh::backend::HostKeyInfo;
use crabport_ssh::session::SshConnectionInfo;
use crabport_telnet::backend::TelnetBackend;
use crabport_telnet::session::TelnetConnectionInfo;
use crabport_terminal::terminal::{
    CrabPortMonitor, RemoteStatus, SftpTransferBytes, SftpTransferKind, SftpTransferStage,
    TerminalSession,
};

use gpui::prelude::FluentBuilder;
use gpui::*;
use parking_lot::Mutex;
use rust_i18n::t;

use crate::app::{
    CrabPortTab, TerminalDecreaseFont, TerminalIncreaseFont, TerminalResetFont, TerminalShiftTab,
    TerminalTab,
};
use crate::color::{selection_bg, term_bg, term_cursor, term_fg};
use crate::views::terminal::color::*;
use crate::views::terminal::connection_overlay::*;
use crate::views::terminal::fonts::{TerminalMetrics, palette};
use crate::views::terminal::render_cache::{
    CellSnap, RenderCache, RowSnapshot, SharedRenderCache, hash_row,
};
use crate::views::terminal::runs::build_runs;
use crate::views::terminal::selection::*;

pub mod connection_overlay;
pub mod search;
pub mod split;
pub mod toolbar;

mod color;
mod fonts;
mod render_cache;
mod runs;
mod scrollbar_handle;
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
    /// Stable per-instance id, used only for debug logging to tell apart
    /// multiple terminal panels in the same window.
    id: u64,
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
    /// Cached (family, size) we last applied, so the render entry point can
    /// detect external config changes (e.g. from the Settings window) and
    /// recompute metrics without each tab needing an explicit notification.
    /// `None` until the first render finishes setup.
    applied_font_signature: Option<(String, f32)>,
    last_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
    /// Last grid size (cols, rows) we actually told the PTY about.
    /// Used to throttle / debounce resizes so a layout feedback loop
    /// (e.g. opencode inside tmux) cannot oscillate the terminal size.
    last_resized_grid: Arc<Mutex<Option<(usize, usize)>>>,
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
    /// Shared with [`TerminalScrollbarHandle`] so the scrollbar widget can
    /// size / position its thumb without reading the term lock on its own.
    history_size: Arc<std::sync::atomic::AtomicI32>,
    /// Latest visible row count, updated each prepaint. Shared with the
    /// scrollbar handle.
    visible_rows: Arc<std::sync::atomic::AtomicI32>,
    /// The scrollbar handle driving the terminal's vertical scrollbar.
    /// Adapts the pixel-based `gpui_component::Scrollbar` widget to the
    /// alacritty grid's row-based scroll model. Cloned into the render
    /// closure and passed to `Scrollbar::vertical`.
    scrollbar_handle: scrollbar_handle::TerminalScrollbarHandle,
    /// Current IME marked (preedit) text, if any. Set by the platform's IME
    /// system via [`EntityInputHandler::replace_and_mark_text_in_range`] and
    /// committed (written to the PTY) via `replace_text_in_range`. Rendered
    /// inline at the cursor so the user sees live composition feedback.
    marked_text: Arc<Mutex<Option<String>>>,
    /// Latest terminal cursor bounds in window coordinates, refreshed each
    /// paint. Used by [`EntityInputHandler::bounds_for_range`] to position the
    /// IME candidate window near the cursor.
    cursor_bounds: Arc<Mutex<Bounds<Pixels>>>,
    /// Whether this terminal pane currently has keyboard focus, tracked via
    /// `on_focus`/`on_blur` listeners registered on first render. Read by the
    /// paint callback to render a solid cursor when focused vs. a hollow
    /// outline when not focused (no blinking).
    is_focused: Arc<AtomicBool>,
    /// Lazily-registered focus/blur listeners (registered on first render,
    /// where a `&mut Window` is available). Held in an `Option` so the
    /// `Subscription`s stay alive for as long as the view.
    focus_sub: Option<gpui::Subscription>,
    overlay: SharedOverlayState,
    remote_host: String,
    /// Persisted host id for command-history storage and tunnel filtering.
    /// `None` for local terminals (their history is in-memory only, not
    /// persisted, and they have no host to filter tunnels by).
    host_id: Option<i64>,
    count: u64,
    ssh_info: Option<SshConnectionInfo>,
    telnet_info: Option<TelnetConnectionInfo>,
    serial_info: Option<SerialConnectionInfo>,
    on_backend_closed: Option<Rc<dyn Fn(&mut App)>>,
    /// Invoked exactly once per connection attempt when the attempt
    /// resolves: `(true, None)` on the first `Connected` status, or
    /// `(false, error)` when the backend errors/closes before ever
    /// connecting (`error` is `None` when the channel closed without an
    /// error message). The app uses it to record the attempt in the
    /// persistent connection history. Mirrors the `on_backend_closed`
    /// callback pattern. Reconnects fire it again (one event per attempt).
    on_connection_result: Option<Rc<dyn Fn(bool, Option<String>, &mut App)>>,
    /// Invoked when this pane receives keyboard focus, passing this pane's
    /// id. The app uses it to sync `split_trees[tab].active_pane` so splits
    /// and the toolbar follow keyboard focus, not just mouse clicks.
    on_focused: Option<Rc<dyn Fn(u64, &mut App)>>,
    /// Invoked when the user triggers a split via keyboard shortcut.
    /// Receives the split direction. The app calls `split_active_pane`.
    on_split_request: Option<Rc<dyn Fn(crate::views::terminal::split::SplitDir, &mut App)>>,
    /// Invoked when the user right-clicks inside the terminal pane.
    /// Receives the click position (window-relative pixels) and the pane
    /// id, so the app can show a context menu with Copy/Paste/etc. actions
    /// scoped to this pane. The app owns the `ContextMenuController` and
    /// decides which items to show — the terminal view itself doesn't know
    /// about the menu system.
    on_context_menu: Option<Rc<dyn Fn(u64, gpui::Point<gpui::Pixels>, &mut App)>>,
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
    /// Invoked when the shell's cwd changes (detected via OSC 7 escape
    /// sequences emitted by shell integration). The app uses this to
    /// update the tab title to reflect the live cwd. Mirrors the
    /// `on_backend_closed` callback pattern. The callback receives the
    /// pane's tab id (the `count` passed to `new`/`with_backend...`), the
    /// new cwd, and the foreground process name (e.g. `zsh`, `cargo`), so
    /// the app can find the right tab to update its title.
    on_cwd_changed: Option<Rc<dyn Fn(u64, std::path::PathBuf, String, &mut App)>>,
    /// A `CrabPortTunnel` view of the backend, when the backend is an SSH
    /// session. Used by the Tunnels panel to start "borrowed" tunnels that
    /// reuse this tab's SSH connection instead of opening a dedicated owned
    /// session. `None` for local PTY backends.
    tunnel_source: Option<Arc<dyn crabport_ssh::CrabPortTunnel>>,
    /// Reconnect attempt counter for exponential backoff. Reset to 0 on a
    /// successful `Connected` status. Incremented each time an auto-reconnect
    /// fires, so the delay grows (1 → 2 → 4 → … → 30s cap) across repeated
    /// failures.
    reconnect_attempts: Arc<AtomicU32>,
    /// Terminal search overlay state.
    search_state: Option<Entity<gpui_component::input::InputState>>,
    /// Whether the search overlay is currently visible.
    search_visible: bool,
    /// Current search query (kept in sync with the InputState).
    search_query: String,
    /// Cached list of matches for `search_query`. Recomputed when the query
    /// changes or the terminal grid is invalidated (resize, new output
    /// that changes line count, …).
    search_matches: Vec<crabport_terminal::terminal::SearchMatch>,
    /// Index into `search_matches` of the currently-active (highlighted)
    /// match, or `None` if none is active.
    search_active: Option<usize>,
}

/// Encode a key event into the kitty keyboard protocol's `CSI u` form, or one
/// of its fixed-function-key escape sequences when the key has no Unicode
/// scalar (arrows, F-keys, …). Returns `None` for keys we cannot encode (in
/// which case the caller should fall back to the legacy IME/byte path).
///
/// `CSI u` layout: `\e[<unicode>;<mods>u`, where `mods` is the kitty modifier
/// mask (shift=1, alt=2, ctrl=4, super/cmd=8) **plus 1** — the +1 is part of
/// the protocol so that "no modifiers" is reported as `1`, not `0`.
///
/// NOTE: when the kitty keyboard protocol is active we route *every* key
/// through this encoder and bypass the IME text path. That trades away CJK
/// input-method composition under kitty-enabled TUIs (e.g. opencode), which is
/// an acceptable trade-off: those TUIs are English-first and need unambiguous
/// key reporting far more than they need an IME.
fn encode_kitty_key(keystroke: &Keystroke) -> Option<Vec<u8>> {
    let modifiers = &keystroke.modifiers;
    let mut mods = 0u8;
    if modifiers.shift {
        mods |= 1;
    }
    if modifiers.alt {
        mods |= 2;
    }
    if modifiers.control {
        mods |= 4;
    }
    // On macOS the `platform` modifier is ⌘ (Command); kitty maps it to super.
    if modifiers.platform {
        mods |= 8;
    }
    let reported = mods + 1;

    // Function / non-character keys get dedicated escape sequences.
    // `keystroke.key` uses gpui's naming ("up", "down", "enter", "f1", …).
    let seq = match keystroke.key.as_str() {
        "up" => format!("\x1b[1;{}A", reported),
        "down" => format!("\x1b[1;{}B", reported),
        "right" => format!("\x1b[1;{}C", reported),
        "left" => format!("\x1b[1;{}D", reported),
        "home" => format!("\x1b[1;{}H", reported),
        "end" => format!("\x1b[1;{}F", reported),
        "pageup" => format!("\x1b[5;{}~", reported),
        "pagedown" => format!("\x1b[6;{}~", reported),
        "insert" => format!("\x1b[2;{}~", reported),
        "delete" => format!("\x1b[3;{}~", reported),
        "f1" => format!("\x1b[1;{}P", reported),
        "f2" => format!("\x1b[1;{}Q", reported),
        "f3" => format!("\x1b[1;{}R", reported),
        "f4" => format!("\x1b[1;{}S", reported),
        "f5" => format!("\x1b[15;{}~", reported),
        "f6" => format!("\x1b[17;{}~", reported),
        "f7" => format!("\x1b[18;{}~", reported),
        "f8" => format!("\x1b[19;{}~", reported),
        "f9" => format!("\x1b[20;{}~", reported),
        "f10" => format!("\x1b[21;{}~", reported),
        "f11" => format!("\x1b[23;{}~", reported),
        "f12" => format!("\x1b[24;{}~", reported),
        _ => {
            // Character keys and the "named" control keys (enter/tab/…).
            let cp: u32 = match keystroke.key.as_str() {
                "enter" => 13,
                "tab" => 9,
                "backspace" => 127,
                "escape" => 27,
                "space" => 32,
                // Regular character: take its first Unicode scalar value.
                s if !s.is_empty() => s.chars().next()? as u32,
                _ => return None,
            };
            format!("\x1b[{};{}u", cp, reported)
        }
    };
    Some(seq.into_bytes())
}

/// Resize the PTY grid to match the *current* canvas bounds, issuing the
/// resize only when the computed grid actually changed.
///
/// Shared by the prepaint and paint callbacks of the terminal canvas. The
/// frame pump drives repaints through `cx.notify()` without re-running
/// layout, so a bounds change that happens without a relayout would
/// otherwise leave the PTY grid stale — the prepaint-time resize alone is
/// not enough, hence the same check also runs on every painted frame.
///
/// Resizing eagerly (no debounce) is deliberate: during split-drag or panel
/// animations the bounds shrink transiently to an intermediate size, and a
/// debounce can lock the PTY grid onto that small size. Once locked, even
/// after the bounds grow back the grid stays small and text collapses to
/// the top-left corner while the surrounding (stale) frame buffer is never
/// repainted. Following bounds every frame keeps the grid in sync with what
/// is actually on screen.
///
/// Returns `true` when a resize was issued (callers use this to invalidate
/// derived caches).
fn sync_pty_grid(
    bounds: Bounds<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
    last_grid: &Mutex<Option<(usize, usize)>>,
    session: &TerminalSession,
    selection: &Mutex<Option<Selection>>,
    window: &mut Window,
    view_id: u64,
) -> bool {
    // `Pixels / Pixels` yields an f32 column/row count.
    let cols = (bounds.size.width / cell_width).floor().max(2.0) as usize;
    let rows = (bounds.size.height / line_height).floor().max(1.0) as usize;

    // Guard against degenerate sizes: a 0/very small grid would shrink the
    // PTY (and a multiplexer such as tmux) to almost nothing, which looks
    // like the terminal "collapsing". This can happen if the size is
    // computed from a not-yet-laid-out view or an overlay bounds.
    if cols < 10 || rows < 4 {
        tracing::debug!(
            "[grid-sync #{}] SKIP tiny bounds {:?} (cell {}x{}, prev {:?})",
            view_id, bounds.size, cell_width, line_height, *last_grid.lock()
        );
        return false;
    }

    let prev_grid = *last_grid.lock();
    let grid_changed = match prev_grid {
        Some((lc, lr)) => lc != cols || lr != rows,
        None => true,
    };
    if !grid_changed {
        return false;
    }

    session.resize(cols as u16, rows as u16);
    *selection.lock() = None;
    *last_grid.lock() = Some((cols, rows));

    // Force the whole window to repaint on the next frame. Without this,
    // when the canvas bounds shrink (e.g. opencode exits its TUI and the
    // grid collapses) GPUI only repaints the new, smaller dirty rectangle —
    // the pixels that used to sit in the now-vacated region are never
    // cleared and stay as stale residue until the user manually resizes the
    // window. Refreshing the window marks it fully dirty so the surrounding
    // area repaints and the ghost is gone.
    window.refresh();
    tracing::debug!(
        "[grid-sync #{}] {}x{} (bounds {:?}, cell {}x{}, prev {:?})",
        view_id, cols, rows, bounds.size, cell_width, line_height, prev_grid
    );
    true
}

impl TerminalView {
    pub fn new(count: u64, cx: &mut Context<Self>) -> Self {
        Self::new_with_cwd(count, None, cx)
    }

    /// Like [`new`](Self::new) but spawns the local PTY child shell in
    /// `cwd` when given. Used by the macOS "Open in CrabPort" entry
    /// point (Finder folder right-click / `crabport://` URL scheme) so
    /// the terminal tab opens already cd'd into the user-selected
    /// folder, instead of the GUI app's default cwd (`/` on macOS).
    /// `None` is equivalent to [`new`](Self::new).
    pub fn new_with_cwd(count: u64, cwd: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let cols: usize = 80;
        let rows: usize = 24;
        // Local PTY creation must stay off the UI thread: on Windows,
        // `PtyBackend::new_with_cwd` synchronously runs `CreatePseudoConsole`
        // + `CreateProcessW` (spawning `pwsh.exe` / `powershell.exe`), which
        // takes 200–500 ms and would freeze the whole window. We use
        // `PendingPtyBackend`, which returns immediately and constructs the
        // real `PtyBackend` on a worker thread. Construction failures are
        // surfaced by the worker itself (it broadcasts `Error` + `Closed` and
        // flips `status()` to `Disconnected`), so the UI's connection overlay
        // still shows the error without any synchronous fallback here.
        let backend: Arc<dyn crabport_terminal::terminal::CrabPortTerminal> = Arc::new(
            crabport_terminal::pty::PendingPtyBackend::new_with_cwd(cols as u16, rows as u16, cwd),
        );
        // Local terminal: empty host label, no persisted host id, a fresh
        // overlay (drives the PTY-startup spinner), and no connection info.
        Self::with_backend_and_host_and_overlay(
            backend,
            cols,
            rows,
            String::new(),
            None,
            Arc::new(Mutex::new(ConnectionOverlayState::new())),
            None,
            None,
            None,
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
        serial_info: Option<SerialConnectionInfo>,
        count: u64,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_backend_and_host_and_overlay_and_history(
            backend,
            cols,
            rows,
            host,
            host_id,
            overlay,
            ssh_info,
            telnet_info,
            serial_info,
            count,
            None,
            cx,
        )
    }

    /// Like [`with_backend_and_host_and_overlay`] but optionally shares the
    /// command-history buffer with another pane. Used when splitting a
    /// terminal so all panes of the same tab see the same history.
    ///
    /// `shared_history` = `None` creates a fresh history (first pane of a
    /// tab); `Some(arc)` reuses the source pane's history (split panes). The
    /// Store-backed persistence callback is wired in both cases so commands
    /// captured in any pane still land in the DB; pre-seeding from the Store
    /// is skipped when sharing (the source pane already did it).
    pub fn with_backend_and_host_and_overlay_and_history(
        backend: Arc<dyn crabport_terminal::terminal::CrabPortTerminal>,
        cols: usize,
        rows: usize,
        host: String,
        host_id: Option<i64>,
        overlay: SharedOverlayState,
        ssh_info: Option<SshConnectionInfo>,
        telnet_info: Option<TelnetConnectionInfo>,
        serial_info: Option<SerialConnectionInfo>,
        count: u64,
        shared_history: Option<Arc<parking_lot::Mutex<std::collections::VecDeque<String>>>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        // Font size, line height, and cell width are derived from the
        // persisted terminal font settings (`[appearance.terminal]`). The
        // cell width is measured from the configured font so the monospace
        // grid stays aligned across family/size changes. See
        // [`TerminalMetrics::from_config`].
        let metrics = TerminalMetrics::from_config(cx);
        let font_size = metrics.font_size;
        let line_height = metrics.line_height;
        let cell_width = metrics.cell_width;

        let is_shared = shared_history.is_some();
        let session = if let Some(hist) = shared_history {
            Arc::new(TerminalSession::new_with_shared_history(
                backend.clone(),
                cols,
                rows,
                hist,
            ))
        } else {
            Arc::new(TerminalSession::new(backend.clone(), cols, rows))
        };
        session.start();

        // NOTE: kitty keyboard protocol is *not* enabled unconditionally here.
        // Doing so makes plain shells (bash/zsh) mis-parse `CSI u` sequences as
        // garbage. Instead, `TerminalSession` negotiates on demand: when the
        // program (e.g. a TUI like opencode/Ink) sends a kitty keyboard request
        // (`\e[>u` / `\e[?u`), the session enables `CSI u` input encoding and
        // focus events for that session only.

        // Wire command-history persistence: when the session captures a new
        // command, persist it to the Store for this host (if any). Local
        // terminals (host_id = None) keep history in-memory only.
        //
        // When sharing history (split panes), skip the Store pre-seed — the
        // source pane already populated the shared buffer, and re-seeding
        // would clobber it with a stale snapshot.
        if let Some(hid) = host_id {
            let store = crate::app_state::AppState::store(cx);
            // Only pre-seed for the first pane (no shared history).
            if !is_shared {
                if let Ok(cmds) = store.lock().commands_for_host(hid) {
                    let mut history = std::collections::VecDeque::new();
                    for c in cmds {
                        history.push_back(c);
                    }
                    *session.command_history_deque() = history;
                }
            }
            // `store` is `Arc<Mutex<Store>>` — clone for the callback so
            // the original binding stays usable above.
            let store_for_cb = store.clone();
            session.set_on_command(Some(std::sync::Arc::new(move |cmd: &str| {
                let _ = store_for_cb.lock().add_command(hid, cmd);
            })));
        }

        let needs_repaint = Arc::new(AtomicBool::new(true));
        // `is_focused` is created here (rather than only in the struct
        // literal) so it can be captured by the paint closure. Read to render
        // a solid cursor when focused vs. a hollow outline when not focused.
        let is_focused = Arc::new(AtomicBool::new(false));

        // Per-attempt listener tasks (backend events, wakeup/status,
        // overlay fade-out) — one fresh set per connection attempt — plus
        // the constructor-only ~120 Hz frame pump, which survives
        // reconnects and is therefore spawned exactly once here.
        Self::spawn_backend_listeners(&session, &overlay, &needs_repaint, cx);
        Self::spawn_frame_pump(&overlay, &needs_repaint, cx);

        // Shared atomics for the terminal grid scroll state. These are kept
        // fresh by the prepaint loop (`display_offset_atomic`, …) and are also
        // handed to the scrollbar handle so the `gpui_component::Scrollbar`
        // widget can compute thumb size/position without taking the term lock.
        let display_offset = Arc::new(std::sync::atomic::AtomicI32::new(0));
        let history_size = Arc::new(std::sync::atomic::AtomicI32::new(0));
        let visible_rows = Arc::new(std::sync::atomic::AtomicI32::new(0));
        let scrollbar_handle = scrollbar_handle::TerminalScrollbarHandle::new_from_atomics(
            session.clone(),
            display_offset.clone(),
            history_size.clone(),
            visible_rows.clone(),
        );

        Self {
            id: count,
            session,
            backend,
            focus_handle,
            font_size,
            line_height,
            cell_width,
            applied_font_signature: None,
            last_bounds: Arc::new(Mutex::new(None)),
            last_resized_grid: Arc::new(Mutex::new(None)),
            selection: Arc::new(Mutex::new(None)),
            render_cache: Arc::new(Mutex::new(RenderCache::default())),
            needs_repaint,
            bindings: keybind::default_bindings(),
            pending_paste: false,
            pending_copy: false,
            scroll_accumulator: 0.0,
            display_offset,
            history_size,
            visible_rows,
            scrollbar_handle,
            marked_text: Arc::new(Mutex::new(None)),
            cursor_bounds: Arc::new(Mutex::new(Bounds::new(
                point(px(0.0), px(0.0)),
                size(px(0.0), px(0.0)),
            ))),
            is_focused,
            focus_sub: None,
            overlay,
            remote_host: host,
            host_id,
            count,
            ssh_info,
            telnet_info,
            serial_info,
            on_backend_closed: None,
            on_connection_result: None,
            on_focused: None,
            on_split_request: None,
            on_context_menu: None,
            sftp_progress: None,
            on_sftp_progress_changed: None,
            on_sftp_transfer_finished: None,
            on_cwd_changed: None,
            reconnect_attempts: Arc::new(AtomicU32::new(0)),
            tunnel_source: None,
            search_state: None,
            search_visible: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_active: None,
        }
    }

    /// Spawn the per-connection-attempt listener tasks for `session`: the
    /// backend error/close event loop, the wakeup/status loop, and the
    /// overlay fade-out watcher. Called by the constructor and by
    /// [`Self::reconnect`] — each connection attempt gets its own set of
    /// listeners (the previous attempt's loops exit once their session's
    /// channels close).
    ///
    /// Allocates a fresh `conn_recorded` once-flag per call so each attempt
    /// records exactly one connection-history event.
    ///
    /// The ~120 Hz frame pump is deliberately NOT spawned here: it polls
    /// the view's shared `overlay` / `needs_repaint` handles, which stay
    /// the same across reconnects, so the constructor spawns it exactly
    /// once via [`Self::spawn_frame_pump`] — re-spawning per reconnect
    /// would leak one timer task per attempt.
    fn spawn_backend_listeners(
        session: &Arc<TerminalSession>,
        overlay: &SharedOverlayState,
        needs_repaint: &Arc<AtomicBool>,
        cx: &mut Context<Self>,
    ) {
        // Once-flag for this connection attempt's history event: flipped by
        // whichever listener resolves the attempt first (wakeup loop on
        // `Connected`, backend-event loop on error/close before that).
        let conn_recorded = Arc::new(AtomicBool::new(false));

        // Backend error/close events.
        let mut event_rx = session.subscribe_backend();
        let overlay_c = overlay.clone();
        let entity = cx.entity().downgrade();
        let conn_recorded_ev = conn_recorded.clone();
        // Captured for the auto-reconnect stale-session guard inside the
        // `Closed` arm — this is the session this listener set belongs to, so
        // a later `Arc::ptr_eq` against `view.session` detects manual
        // reconnects (which swap in a fresh session) and aborts the pending
        // auto-reconnect.
        let session_for_reconnect = session.clone();
        cx.spawn(async move |_this, cx| {
            while let Ok(event) = event_rx.recv().await {
                match event {
                    crabport_terminal::terminal::BackendEvent::Error(err) => {
                        overlay_c.lock().log(ConnectionLogLevel::Error, err.clone());
                        let _ = entity.update(cx, |this, cx| {
                            // An error before the first `Connected` means the
                            // attempt failed — record it. Post-connect errors
                            // are no-ops here (the flag is already set).
                            this.fire_connection_result(
                                &conn_recorded_ev,
                                false,
                                Some(err.clone()),
                                cx,
                            );
                            cx.notify();
                        });
                    }
                    crabport_terminal::terminal::BackendEvent::Closed => {
                        let entity_for_reconnect = entity.clone();
                        let _ = entity.update(cx, |this, cx| {
                            // A close before the first `Connected` (and with
                            // no prior error) is a silent connect failure.
                            this.fire_connection_result(&conn_recorded_ev, false, None, cx);
                            if let Some(ref cb) = this.on_backend_closed {
                                let cb = cb.clone();
                                cx.defer(move |cx| cb(cx));
                            } else {
                                this.overlay
                                    .lock()
                                    .log(ConnectionLogLevel::Warning, "Connection closed");
                            }
                            cx.notify();

                            // Auto-reconnect on an unexpected mid-session
                            // drop (config-gated). We only fire when the
                            // overlay status was still `Connected` at close
                            // time — a close while `Connecting` is a connect
                            // failure (don't loop), and a user-initiated tab
                            // close is caught by the `WeakEntity` going stale
                            // during the backoff window. The `Arc::ptr_eq`
                            // guard inside the spawned task also aborts if the
                            // user manually reconnected (replacing
                            // `this.session`) while we were waiting.
                            if crabport_core::config::snapshot()
                                .appearance
                                .terminal
                                .auto_reconnect
                            {
                                let was_connected =
                                    this.overlay.lock().status == RemoteStatus::Connected;
                                if was_connected {
                                    let attempts = this.reconnect_attempts.clone();
                                    let overlay_for_reconnect = this.overlay.clone();
                                    let n = attempts.fetch_add(1, Ordering::SeqCst);
                                    // Exponential backoff: 1, 2, 4, 8, 16, 30,
                                    // 30, … (cap at 30s). `n.min(5)` avoids a
                                    // shift overflow panic on pathological
                                    // consecutive-failure counts.
                                    let delay_secs = (1u64 << n.min(5)).min(30);
                                    let session_ref = session_for_reconnect.clone();
                                    cx.spawn(async move |_this, cx| {
                                        overlay_for_reconnect.lock().log(
                                            ConnectionLogLevel::Info,
                                            format!("Reconnecting in {delay_secs}s..."),
                                        );
                                        smol::Timer::after(std::time::Duration::from_secs(
                                            delay_secs,
                                        ))
                                        .await;
                                        let _ = entity_for_reconnect.update(cx, |view, cx| {
                                            // Guard: don't reconnect if the
                                            // session was replaced (manual
                                            // reconnect) or the view is gone.
                                            if !Arc::ptr_eq(&session_ref, &view.session) {
                                                return;
                                            }
                                            view.reconnect(cx);
                                        });
                                    })
                                    .detach();
                                }
                            }
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
                            crabport_terminal::terminal::SftpTransferKind::Delete => "Delete",
                            crabport_terminal::terminal::SftpTransferKind::Mkdir => "Mkdir",
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
                    crabport_terminal::terminal::BackendEvent::HistoryLoaded(_) => {
                        // The session's `start` loop already merged the
                        // loaded commands into `command_history`. Repaint
                        // so the History panel picks up the new list.
                        let _ = entity.update(cx, |_, cx| cx.notify());
                    }
                    crabport_terminal::terminal::BackendEvent::ProcessChanged {
                        cwd,
                        process_name,
                    } => {
                        // Foreground process changed (cwd and/or name).
                        // Forward to the app so it can update the tab title.
                        let _ = entity.update(cx, |this, cx| {
                            let cb = this.on_cwd_changed.clone();
                            let tab_id = this.count;
                            if let Some(cb) = cb {
                                cx.defer(move |cx| cb(tab_id, cwd, process_name, cx));
                            }
                        });
                    }
                    crabport_terminal::terminal::BackendEvent::Ready => {
                        // The backend finished async construction
                        // (`PendingPtyBackend` installed the real
                        // `PtyBackend`). The wakeup listener below re-reads
                        // `monitor().status()` and flips the overlay out of
                        // the "Connecting" spinner, so nothing to do here
                        // but repaint so the fade-out kicks in immediately.
                        let _ = entity.update(cx, |_, cx| cx.notify());
                    }
                }
            }
        })
        .detach();

        // Wakeup listener: only mark dirty (+ reflect status into overlay).
        let mut wakeup_rx = session.subscribe_wakeup();
        let dirty_wk = needs_repaint.clone();
        let status_entity = cx.entity().downgrade();
        // Fires `refresh_history` once the backend reports Connected / Local
        // so the History panel is seeded from the TTY history file instead
        // of only from session-captured commands. One-shot — subsequent
        // refreshes are user-triggered via the History panel's refresh button.
        let history_refreshed = Arc::new(AtomicBool::new(false));
        let history_refreshed_wk = history_refreshed.clone();
        let session_for_refresh = session.clone();
        let conn_recorded_wk = conn_recorded.clone();
        cx.spawn(async move |_this, cx| {
            while let Ok(()) = wakeup_rx.recv().await {
                let _ = status_entity.update(cx, |this, cx| {
                    // A reconnect swaps `this.session` for a fresh one; this
                    // loop belongs to a single attempt. Once superseded, go
                    // inert (the loop exits for good when its own wakeup
                    // channel closes) so a stale attempt can't mirror status
                    // into the shared overlay, re-resolve the connection
                    // result, or refresh history on a dead session.
                    if !Arc::ptr_eq(&session_for_refresh, &this.session) {
                        return;
                    }
                    if let Some(m) = this.session.monitor() {
                        let new_status = m.status();
                        let mut ov = this.overlay.lock();
                        if new_status != ov.status {
                            ov.update_status(new_status, &this.remote_host);
                            // First `Connected` resolves the attempt as a
                            // success in the connection history.
                            if new_status == RemoteStatus::Connected {
                                this.reconnect_attempts.store(0, Ordering::SeqCst);
                                this.fire_connection_result(&conn_recorded_wk, true, None, cx);
                                // Force a window-size sync right after the PTY is
                                // up. The initial PTY was allocated at a default
                                // size (80x24) before the grid was measured, so a
                                // program like tmux that lays out its panes from
                                // the PTY size would otherwise render truncated
                                // and never refresh.
                                //
                                // IMPORTANT: do NOT snap the grid to
                                // `last_bounds` here. This callback runs on a
                                // spawned async task, and for an SSH connection
                                // the round-trip delay means the window may not
                                // have been laid out (or `last_bounds` may still
                                // hold a stale/transitional value) by the time we
                                // get here. Snapping to that value would lock the
                                // PTY grid at the wrong size, and since the layout
                                // would then never change again the terminal would
                                // stay shrunk in the top-left corner until the user
                                // manually resizes the window.
                                //
                                // Instead we just invalidate the recorded grid
                                // size. The per-frame resize sync in `paint` (and
                                // `prepaint`) will then re-measure the *current*
                                // real bounds on the next painted frame and resize
                                // to that — self-correcting once layout is stable.
                                *this.last_resized_grid.lock() = None;
                                tracing::debug!(
                                    "post-connect #{}: invalidated last_resized_grid, will re-sync to real bounds next frame (last_bounds was {:?})",
                                    this.id, *this.last_bounds.lock()
                                );
                            }
                            // Trigger an initial TTY-history read when the
                            // connection first reaches a ready state.
                            if !history_refreshed_wk.swap(true, Ordering::AcqRel)
                                && matches!(
                                    new_status,
                                    RemoteStatus::Connected | RemoteStatus::Local
                                )
                            {
                                session_for_refresh.refresh_history();
                            }
                        }
                    }
                });
                dirty_wk.store(true, Ordering::Release);
            }
        })
        .detach();

        // Spawn the fade-out watcher for both remote and local terminals.
        // For remote (SSH / Telnet) sessions it hides the overlay after the
        // connection establishes. For local terminals it hides the overlay
        // once `PendingPtyBackend` finishes constructing the real `PtyBackend`
        // and `update_status(Local, ...)` flips `fade_out_started`.
        {
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
    }

    /// Spawn the ~120 Hz frame pump: notifies the entity at most ~120
    /// times a second and only when the dirty flag is set, advances the
    /// connecting spinner's rotation, and keeps repainting while overlay
    /// log rows are mid-fade-in. Constructor-only — see
    /// [`Self::spawn_backend_listeners`] for why `reconnect` must not
    /// spawn another pump. The loop exits when the entity is dropped.
    fn spawn_frame_pump(
        overlay: &SharedOverlayState,
        needs_repaint: &Arc<AtomicBool>,
        cx: &mut Context<Self>,
    ) {
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

    /// The persisted host id this terminal is connected to, or `None` for
    /// local PTY tabs. Used by the Tunnels panel to filter the tunnel list
    /// to only those belonging to this terminal's host.
    pub fn host_id(&self) -> Option<i64> {
        self.host_id
    }

    pub fn sftp_entries(&self) -> Option<std::sync::Arc<Vec<crabport_sftp::FileEntry>>> {
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

    /// Upload multiple files in a single batch transfer. Falls back to
    /// per-file upload if the remote doesn't support tar. Completion is
    /// reported via the backend's event stream.
    pub fn sftp_upload_batch(&self, items: &[(String, String)]) {
        self.backend.sftp_upload_batch(items);
    }

    /// Snapshot of this session's command history, most-recent-first.
    /// Returns an empty vec for local terminals or sessions without a
    /// backend that tracks history.
    pub fn command_history(&self) -> Vec<String> {
        self.session.command_history()
    }

    /// Trigger a fresh read of the shell's TTY history file. The backend
    /// broadcasts a `BackendEvent::HistoryLoaded` once the data is ready,
    /// which the session forwards into `command_history` (and the next
    /// render picks up via [`Self::command_history`]).
    pub fn refresh_history(&self) {
        self.session.refresh_history();
    }

    /// Cloned handle to the shared command-history buffer. Used when
    /// splitting this pane so the new pane shares the same history.
    pub fn command_history_arc(
        &self,
    ) -> Arc<parking_lot::Mutex<std::collections::VecDeque<String>>> {
        self.session.command_history_arc()
    }

    /// Write raw bytes to the terminal **without** capturing them as a
    /// command. Used by the History panel's "paste" action so inserting a
    /// historical command into the input line doesn't re-record it.
    pub fn write_raw(&self, data: &[u8]) {
        self.session.write_raw(data);
    }

    /// Copy the current selection (or the whole visible grid if no
    /// selection) to the clipboard. Equivalent to the `TerminalAction::Copy`
    /// keyboard shortcut — sets the `pending_copy` flag, which the next
    /// render tick consumes. Used by the terminal right-click context menu.
    pub fn trigger_copy(&mut self, cx: &mut Context<Self>) {
        self.pending_copy = true;
        cx.notify();
    }

    /// Paste the current clipboard contents into the terminal. Equivalent
    /// to the `TerminalAction::Paste` keyboard shortcut — sets the
    /// `pending_paste` flag, which the next render tick consumes. Used by
    /// the terminal right-click context menu.
    pub fn trigger_paste(&mut self, cx: &mut Context<Self>) {
        self.pending_paste = true;
        cx.notify();
    }

    /// Whether the terminal currently has a non-empty text selection.
    /// Used by the right-click context menu to decide whether to enable
    /// the "Copy" item.
    pub fn has_selection(&self) -> bool {
        self.selection
            .lock()
            .as_ref()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    }

    /// Clear the current text selection. Used by the right-click context
    /// menu's "Clear Selection" item.
    pub fn clear_selection(&mut self) {
        *self.selection.lock() = None;
    }

    /// Clear the terminal screen by asking the shell to run `clear`.
    //
    // We send the `clear` command through the PTY (via `write_raw`, which
    // doesn't record it into the command history) rather than feeding
    // ANSI escape sequences directly into the term grid via `feed_escape`.
    // The reason: `feed_escape(b"\x1b[2J\x1b[H")` erases the visible grid
    // but the shell has no idea the screen was cleared, so it won't
    // redraw its prompt — the `[user@host] ...` prompt vanishes and the
    // user is left with a blank screen until they press Enter.
    //
    // By running `clear` as a shell command, the shell's own `clear`
    // implementation sends the right escape sequences AND redraws the
    // prompt via its precmd/preprompt hook, so the result matches what
    // the user expects from typing `clear` at the prompt.
    //
    // `write_raw` is used instead of `write` so the `clear` invocation
    // isn't captured into the command history (it's a UI action, not a
    // user command).
    pub fn clear_screen(&mut self) {
        self.session.write_raw(b"clear\n");
        self.session.scroll_to_bottom();
    }

    /// Reset the terminal by running `reset` in the shell. This performs
    // a full terminal reset (re-initializes the terminal state, clears
    // scrollback, redraws the prompt) — heavier than `clear_screen`.
    //
    // Like [`clear_screen`], we send `reset\n` through the PTY via
    // `write_raw` rather than feeding `ESC c` (RIS) directly into the
    // term grid. `ESC c` resets the terminal emulator's state but the
    // shell doesn't know it happened, so the prompt wouldn't redraw.
    // Running `reset` as a command lets the shell + `reset` binary
    // coordinate the full reset sequence and prompt redraw.
    //
    // Note: `reset` may not exist on all systems (e.g. minimal
    // embedded shells). On systems without it, the shell will print
    // "command not found" — a tolerable failure mode. If this becomes
    // a real issue we can fall back to `stty sane` + `clear`.
    pub fn reset_terminal(&mut self) {
        self.session.write_raw(b"reset\n");
        self.session.scroll_to_bottom();
    }

    /// Delete a remote file or directory. The backend stats the path to
    /// decide between `remove_file` and recursive `remove_dir`.
    pub fn sftp_delete(&self, remote_path: &str) {
        self.session.sftp_delete(remote_path);
    }

    /// Create a directory on the remote host (non-recursive). Completion is
    /// reported through the backend's event stream; on success the SFTP
    /// listing is auto-refreshed by the terminal's `SftpTransferFinished`
    /// handler.
    pub fn sftp_mkdir(&self, remote_path: &str) {
        self.backend.sftp_mkdir(remote_path);
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

    /// See [`Self::on_connection_result`]. `success` + optional error text.
    pub fn set_on_connection_result(
        &mut self,
        f: impl Fn(bool, Option<String>, &mut App) + 'static,
    ) {
        self.on_connection_result = Some(Rc::new(f));
    }

    /// Fire `on_connection_result` for the current attempt if it hasn't
    /// fired yet. `recorded` is the per-attempt once-flag shared by the
    /// backend-event and wakeup listeners of that attempt.
    fn fire_connection_result(
        &self,
        recorded: &Arc<AtomicBool>,
        success: bool,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if recorded.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(cb) = self.on_connection_result.clone() {
            cx.defer(move |cx| cb(success, error, cx));
        }
    }

    /// The stable pane id this view was created with (passed as `count` to
    /// the constructor). The app uses it to identify which pane in a split
    /// tree received keyboard focus, so `split_active_pane` operates on the
    /// focused pane rather than just the last-clicked one.
    pub fn pane_id(&self) -> u64 {
        self.count
    }

    /// Whether this pane currently has keyboard focus. Read by the app to
    /// determine which pane to split (the focused one) without relying on
    /// mouse-click bookkeeping.
    pub fn is_focused(&self) -> bool {
        self.is_focused.load(Ordering::Acquire)
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

    /// Sets the callback invoked when the foreground process changes (new
    /// cwd or new process name). The callback receives this pane's tab id
    /// (`count`), the new cwd, and the process name (e.g. `zsh`, `cargo`).
    pub fn set_on_cwd_changed(
        &mut self,
        f: impl Fn(u64, std::path::PathBuf, String, &mut App) + 'static,
    ) {
        self.on_cwd_changed = Some(Rc::new(f));
    }

    /// Set the callback invoked when this pane receives keyboard focus. The
    /// app uses it to mark this pane as the active pane of its tab, so that
    /// `split_active_pane` and the toolbar follow keyboard focus (not just
    /// mouse clicks). The callback receives this pane's id.
    pub fn set_on_focused(&mut self, f: impl Fn(u64, &mut App) + 'static) {
        self.on_focused = Some(Rc::new(f));
    }

    /// Set the callback invoked when the user triggers a split via keyboard
    /// shortcut. The app calls `split_active_pane` with the given direction.
    pub fn set_on_split_request(
        &mut self,
        f: impl Fn(crate::views::terminal::split::SplitDir, &mut App) + 'static,
    ) {
        self.on_split_request = Some(Rc::new(f));
    }

    /// Clone of the current `on_split_request` callback, if any. Exposed so
    /// the right-click context menu can invoke the same split path as the
    /// keyboard shortcut without the app having to re-resolve the target
    /// pane.
    pub fn on_split_request_cb(
        &self,
    ) -> Option<Rc<dyn Fn(crate::views::terminal::split::SplitDir, &mut App)>> {
        self.on_split_request.clone()
    }

    /// Set the callback invoked when the user right-clicks inside this
    /// terminal pane. The app uses it to show a context menu with
    /// Copy/Paste/Select-All actions scoped to this pane. The callback
    /// receives this pane's id and the click position (window-relative
    /// pixels).
    pub fn set_on_context_menu(
        &mut self,
        f: impl Fn(u64, gpui::Point<gpui::Pixels>, &mut App) + 'static,
    ) {
        self.on_context_menu = Some(Rc::new(f));
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

    // --- Split-pane support ---
    //
    // A split pane gets its own *independent* PTY/channel on the same
    // underlying connection (SSH: new session channel + PTY + shell on the
    // existing authenticated handle; local: new shell process; Telnet: new
    // connection since Telnet has no channel multiplexing). Each pane has
    // its own term grid, scrollback, and input/output — they are fully
    // independent, not mirrored.

    /// The remote host label (empty for local PTY).
    pub fn remote_host(&self) -> &str {
        &self.remote_host
    }

    /// SSH connection info, if this is an SSH tab.
    pub fn ssh_info(&self) -> Option<&SshConnectionInfo> {
        self.ssh_info.as_ref()
    }

    /// Telnet connection info, if this is a Telnet tab.
    pub fn telnet_info(&self) -> Option<&TelnetConnectionInfo> {
        self.telnet_info.as_ref()
    }

    /// Serial connection info, if this is a Serial tab.
    pub fn serial_info(&self) -> Option<&SerialConnectionInfo> {
        self.serial_info.as_ref()
    }

    /// The shared connection-overlay state (host-key prompt, etc.).
    pub fn overlay_state(&self) -> SharedOverlayState {
        self.overlay.clone()
    }

    /// The tunnel source Arc, if any (for SSH tabs).
    pub fn tunnel_source_arc(&self) -> Option<Arc<dyn CrabPortTunnel>> {
        self.tunnel_source.clone()
    }

    /// Set the tunnel source (optional builder, used by split-pane creation
    /// to share the SSH tunnel source with the new pane).
    pub fn with_tunnel_source_opt(mut self, source: Option<Arc<dyn CrabPortTunnel>>) -> Self {
        self.tunnel_source = source;
        self
    }

    /// Create a new [`TerminalView`] for a split pane. The new pane gets an
    /// independent PTY/channel via `backend.spawn_channel()`:
    /// - **SSH**: opens a new session channel on the existing authenticated
    ///   connection (no re-auth, no new TCP connect).
    /// - **Local PTY**: spawns a new shell process.
    /// - **Telnet**: `spawn_channel` returns `None`, so the caller falls
    ///   back to creating a new `TelnetBackend` (new TCP connection).
    ///
    /// Returns `None` if the backend can't spawn a channel.
    pub fn spawn_channel_backend(
        &self,
        _cols: u16,
        _rows: u16,
    ) -> Option<Arc<dyn crabport_terminal::terminal::CrabPortTerminal>> {
        self.backend.spawn_channel(80, 24)
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
        // Try SSH first, then Telnet, then Serial.
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
            } else if let Some(info) = self.serial_info.clone() {
                let overlay_cb = self.overlay.clone();
                Arc::new(SerialBackend::new(
                    info,
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

        // Carry the command-history buffer over to the fresh session. Split
        // panes share this `Arc` with their siblings, so the sharing must
        // survive a reconnect; a standalone pane simply keeps the history it
        // had before the drop.
        let history = self.session.command_history_arc();
        let session = Arc::new(TerminalSession::new_with_shared_history(
            backend.clone(),
            cols,
            rows,
            history,
        ));
        session.start();

        // Kitty keyboard protocol is negotiated on demand by `TerminalSession`
        // when the program requests it (see note in `new_with_cwd`).

        // Re-install command-history persistence: the callback lives on the
        // session, so without this the fresh session would capture commands
        // in memory but never write them to the Store again.
        if let Some(hid) = self.host_id {
            let store_for_cb = crate::app_state::AppState::store(cx);
            session.set_on_command(Some(std::sync::Arc::new(move |cmd: &str| {
                let _ = store_for_cb.lock().add_command(hid, cmd);
            })));
        }

        self.render_cache.lock().clear_all();

        // Fresh per-attempt listeners for the new session (fresh
        // `conn_recorded` once-flag included). The ~120 Hz frame pump
        // spawned by the constructor keeps polling the same shared
        // `overlay` / `needs_repaint` handles across reconnects, so it is
        // deliberately NOT re-spawned here — re-spawning would leak one
        // timer task per reconnect.
        let overlay = self.overlay.clone();
        let needs_repaint = self.needs_repaint.clone();
        Self::spawn_backend_listeners(&session, &overlay, &needs_repaint, cx);

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

    // -----------------------------------------------------------------
    // Font settings reload
    // -----------------------------------------------------------------

    /// Re-read the terminal font settings from `config.toml` and recompute
    /// the cached `font_size` / `line_height` / `cell_width`. Clears the
    /// shaped-glyph LRU so the next repaint reshapes every line with the new
    /// metrics, and forces a PTY resize so the new cell size maps to the
    /// right `cols`/`rows` for the current viewport.
    ///
    /// Call this after mutating `config.appearance.terminal` (e.g. from the
    /// Settings window or the in-terminal zoom shortcuts).
    pub fn reload_font_settings(&mut self, cx: &mut Context<Self>) {
        let metrics = TerminalMetrics::from_config(cx);
        let changed = self.font_size != metrics.font_size
            || self.line_height != metrics.line_height
            || self.cell_width != metrics.cell_width;
        self.font_size = metrics.font_size;
        self.line_height = metrics.line_height;
        self.cell_width = metrics.cell_width;
        // Record the config we just applied so the render-entry auto-check
        // doesn't redundantly re-derive metrics on the next frame.
        let family = crate::views::terminal::fonts::font_family();
        let size = crabport_core::config::snapshot()
            .appearance
            .terminal
            .effective_font_size();
        self.applied_font_signature = Some((family, size));
        if changed {
            // Drop cached shaped lines — they were laid out for the old
            // font/size and would render at the wrong width.
            self.render_cache.lock().shaped.clear();
            // Invalidate last_bounds so the prepaint step re-runs the
            // cols/rows resize with the new cell metrics.
            *self.last_bounds.lock() = None;
            *self.last_resized_grid.lock() = None;
            self.needs_repaint
                .store(true, std::sync::atomic::Ordering::Release);
        }
        cx.notify();
    }

    // -----------------------------------------------------------------
    // Search
    // -----------------------------------------------------------------

    /// Toggle the search overlay open/closed. When opening, lazily creates
    /// the `InputState` entity (bound to the current window) and focuses
    /// it so the user can start typing immediately.
    pub fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_visible {
            self.close_search(window, cx);
        } else {
            self.open_search(window, cx);
        }
    }

    fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_state.is_none() {
            let state = cx.new(|cx| {
                gpui_component::input::InputState::new(window, cx)
                    .placeholder(t!("terminal.search.placeholder"))
            });
            // Subscribe to query changes + Enter key so we recompute matches
            // and navigate on Enter / Shift+Enter.
            cx.subscribe(
                &state,
                |this, input, event: &gpui_component::input::InputEvent, cx| match event {
                    gpui_component::input::InputEvent::Change { .. } => {
                        let new_query = input.read(cx).value().to_string();
                        this.on_search_query_changed(&new_query, cx);
                    }
                    gpui_component::input::InputEvent::PressEnter { secondary } => {
                        let dir = if *secondary {
                            search::SearchDirection::Prev
                        } else {
                            search::SearchDirection::Next
                        };
                        this.advance_match(dir, cx);
                    }
                    _ => {}
                },
            )
            .detach();
            self.search_state = Some(state);
        }
        self.search_visible = true;
        // Clear any selection so the search highlights are the only overlay.
        *self.selection.lock() = None;
        if let Some(state) = &self.search_state {
            // Focus the search input so typing goes straight into the query.
            state.focus_handle(cx).focus(window);
        }
        cx.notify();
    }

    pub fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_visible = false;
        // Clear matches so highlights disappear.
        self.search_matches.clear();
        self.search_active = None;
        self.search_query.clear();
        // Return keyboard focus to the terminal pane so the user can
        // resume typing immediately after closing the search.
        self.focus_handle.focus(window);
        cx.notify();
    }

    /// Recompute matches for a new query, then select the first match
    /// closest to the current cursor position.
    fn on_search_query_changed(&mut self, query: &str, cx: &mut Context<Self>) {
        self.search_query = query.to_string();
        self.recompute_matches();
        self.search_active = self.compute_initial_active();
        if let Some(idx) = self.search_active {
            self.activate_match(idx, cx);
        } else {
            cx.notify();
        }
    }

    /// Re-run the search against the current terminal grid. Called when
    /// the query changes or when the grid is invalidated (resize, new
    /// output). Safe to call even when the search overlay is closed.
    fn recompute_matches(&mut self) {
        if self.search_query.is_empty() {
            self.search_matches.clear();
            self.search_active = None;
            return;
        }
        // Escape regex metacharacters so the query is treated as a literal
        // substring. We don't expose a regex toggle in the UI yet; this keeps
        // it simple and matches the common Ctrl+F expectation.
        let escaped = escape_regex(&self.search_query);
        self.search_matches = self.session.search_matches(&escaped);
        // Clamp the active index.
        if let Some(idx) = self.search_active {
            if idx >= self.search_matches.len() {
                self.search_active = if self.search_matches.is_empty() {
                    None
                } else {
                    Some(0)
                };
            }
        }
    }

    /// Pick the initial active match: the first match whose start line is at
    /// or after the cursor line (searching forward), or 0 if none qualify.
    fn compute_initial_active(&self) -> Option<usize> {
        if self.search_matches.is_empty() {
            return None;
        }
        let (cursor_line, _) = self.session.cursor_point();
        // Find the first match at or after the cursor.
        let idx = self
            .search_matches
            .iter()
            .position(|m| m.start_line >= cursor_line)
            .unwrap_or(0);
        Some(idx)
    }

    /// Advance the active match in the given direction, wrapping around.
    pub fn advance_match(&mut self, dir: search::SearchDirection, cx: &mut Context<Self>) {
        if self.search_matches.is_empty() {
            return;
        }
        let len = self.search_matches.len();
        let new_idx = match (self.search_active, dir) {
            (Some(i), search::SearchDirection::Next) => Some((i + 1) % len),
            (Some(i), search::SearchDirection::Prev) => Some((i + len - 1) % len),
            (None, _) => Some(0),
        };
        if let Some(idx) = new_idx {
            self.search_active = Some(idx);
            self.activate_match(idx, cx);
        }
    }

    /// Scroll the terminal to the match at `index` and request a repaint.
    fn activate_match(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(m) = self.search_matches.get(index) {
            self.session.scroll_to_match(m);
            self.needs_repaint
                .store(true, std::sync::atomic::Ordering::Release);
        }
        cx.notify();
    }

    // -----------------------------------------------------------------
    // Search accessors (read by `render_content` to render the search bar
    // as a layout-level row that pushes the toolbar down)
    // -----------------------------------------------------------------

    pub fn search_visible(&self) -> bool {
        self.search_visible
    }

    pub fn search_state(&self) -> Option<&Entity<gpui_component::input::InputState>> {
        self.search_state.as_ref()
    }

    pub fn search_active(&self) -> Option<usize> {
        self.search_active
    }

    pub fn search_match_count(&self) -> usize {
        self.search_matches.len()
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
            let sel = self.selection.lock().clone();
            let text = match sel {
                Some(ref sel) if !sel.is_empty() => Self::copy_selected_text(&self.session, sel),
                _ => self.session.with_term(|term| {
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
                }),
            };
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }

        // Auto-detect font config changes made elsewhere (e.g. the Settings
        // window). Comparing the cached signature against the live config is
        // cheap, and lets every terminal tab pick up new font settings
        // without requiring an explicit cross-window notification.
        {
            let cfg = crabport_core::config::snapshot().appearance.terminal;
            let family = cfg.effective_font_family().to_string();
            let size = cfg.effective_font_size();
            if self.applied_font_signature != Some((family.clone(), size)) {
                self.reload_font_settings(cx);
            }
        }

        // Lazily register focus/blur listeners (only once — a `&mut Window`
        // is required, which we only have inside `render`). These track
        // `is_focused` so the paint callback can render a solid cursor when
        // focused vs. a hollow outline when not focused. On focus we also
        // fire the `on_focused` callback so the app can sync
        // `split_trees[tab].active_pane` to the keyboard-focused pane.
        if self.focus_sub.is_none() {
            let is_focused = self.is_focused.clone();
            let focused_cb = self.on_focused.clone();
            let pane_id = self.count;
            let fh = self.focus_handle.clone();
            let session_focus = self.session.clone();
            let sub_f = cx.on_focus(&fh, _window, move |_this, _window, cx| {
                is_focused.store(true, Ordering::Release);
                // Kitty keyboard protocol: notify the program it gained focus.
                session_focus.report_focus(true);
                if let Some(cb) = &focused_cb {
                    let cb = cb.clone();
                    cx.defer(move |cx| cb(pane_id, cx));
                }
            });
            let is_focused_b = self.is_focused.clone();
            let session_blur = self.session.clone();
            let sub_b = cx.on_blur(&fh, _window, move |_this, _window, _cx| {
                is_focused_b.store(false, Ordering::Release);
                // Kitty keyboard protocol: notify the program it lost focus.
                session_blur.report_focus(false);
            });
            // Re-fetch focus state immediately so the first frame after a
            // focus change (e.g. switching tabs via the app's
            // `window.focus()`) is correct even before a listener fires.
            self.is_focused
                .store(fh.is_focused(_window), Ordering::Release);
            self.focus_sub = Some(gpui::Subscription::join(sub_f, sub_b));
        }

        let session_c = self.session.clone();
        let session = session_c.clone();
        let view_id = self.id;
        let font_size = self.font_size;
        let line_height = self.line_height;
        let cell_width = self.cell_width;
        let focus_handle = self.focus_handle.clone();
        let last_bounds_c = self.last_bounds.clone();
        let last_bounds = last_bounds_c.clone();
        let last_resized_grid = self.last_resized_grid.clone();
        let selection = self.selection.clone();
        let selection_prepaint = selection.clone();
        let selection_c = selection.clone();
        // Paint-closure clones: the resize sync below must run on *every*
        // painted frame (not just on layout), because the frame pump only
        // triggers repaints via `cx.notify()`, which does not re-run layout.
        // Without this, a bounds that shrank without a subsequent relayout
        // would leave the PTY grid locked at the small size forever (text
        // collapses to the top-left corner) until the user resizes the window.
        let session_paint = session_c.clone();
        let last_resized_grid_paint = self.last_resized_grid.clone();
        let selection_paint_resize = selection.clone();
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
        let scrollbar_handle = self.scrollbar_handle.clone();
        // Clones used by the paint closure to (a) register the IME input
        // handler so the platform can route composition events (Chinese /
        // Japanese / Korean input) to the terminal, and (b) refresh the
        // cached cursor bounds each frame so `bounds_for_range` can position
        // the IME candidate window.
        let marked_text_paint = self.marked_text.clone();
        let cursor_bounds_paint = self.cursor_bounds.clone();
        let entity_input = cx.entity();
        let focus_handle_input = self.focus_handle.clone();
        // Read by the paint callback to render a solid cursor when focused
        // vs. a hollow outline when not focused (no blinking).
        let is_focused_paint = self.is_focused.clone();
        // Cloned into the right-click handler so a right-click inside the
        // terminal pane surfaces a Copy/Paste context menu via the app's
        // global `ContextMenuController`.
        let on_context_menu = self.on_context_menu.clone();
        let pane_id_for_ctx = self.count;
        // Search matches snapshot for the paint callback. Cloned by value so
        // the paint closure has an immutable copy that doesn't fight the
        // render thread for the Vec.
        let search_matches_paint = self.search_matches.clone();
        let search_active_paint = self.search_active;

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
            .pt_2()
            .pl_2()
            .relative()
            .size_full()
            .overflow_hidden()
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
            .on_action(cx.listener(|this, _: &TerminalIncreaseFont, _window, cx| {
                // Bump the persisted font size by 1px (clamped inside the
                // config accessor) and re-derive cell metrics. We go through
                // `config::update` so the change survives restarts and is
                // shared by every terminal tab in the app.
                let _ = crabport_core::config::update(|cfg| {
                    cfg.appearance.terminal.font_size =
                        (cfg.appearance.terminal.font_size + 1.0).clamp(8.0, 32.0);
                });
                this.reload_font_settings(cx);
            }))
            .on_action(cx.listener(|this, _: &TerminalDecreaseFont, _window, cx| {
                let _ = crabport_core::config::update(|cfg| {
                    cfg.appearance.terminal.font_size =
                        (cfg.appearance.terminal.font_size - 1.0).clamp(8.0, 32.0);
                });
                this.reload_font_settings(cx);
            }))
            .on_action(cx.listener(|this, _: &TerminalResetFont, _window, cx| {
                let _ = crabport_core::config::update(|cfg| {
                    cfg.appearance.terminal.font_size = 13.0;
                });
                this.reload_font_settings(cx);
            }))
            .on_action(
                cx.listener(|this, _: &crate::app::SplitVertical, _window, cx| {
                    if let Some(cb) = &this.on_split_request {
                        cb(crate::views::terminal::split::SplitDir::Vertical, cx);
                    }
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::app::SplitHorizontal, _window, cx| {
                    if let Some(cb) = &this.on_split_request {
                        cb(crate::views::terminal::split::SplitDir::Horizontal, cx);
                    }
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::app::TerminalSearch, window, cx| {
                    this.toggle_search(window, cx);
                }),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                // Kitty keyboard protocol: when active, every key is encoded as
                // `CSI u` (or its function-key escape) and routed straight to the
                // PTY, bypassing the IME text path. This is what lets TUI tools
                // such as opencode bind unambiguous keys and react to focus
                // changes. Falls back to the legacy path when the protocol is off.
                if this.session.kitty_keyboard_enabled() {
                    if let Some(bytes) = encode_kitty_key(&event.keystroke) {
                        this.session.write(&bytes);
                        this.session.scroll_to_bottom();
                        cx.notify();
                        cx.stop_propagation();
                        return;
                    }
                }
                match Self::resolve_keystroke(&event.keystroke, &this.bindings) {
                    Some(KeyAction::Action(TerminalAction::Copy)) => {
                        this.pending_copy = true;
                        cx.notify();
                    }
                    Some(KeyAction::Action(TerminalAction::Paste)) => {
                        this.pending_paste = true;
                        cx.notify();
                    }
                    Some(KeyAction::Action(TerminalAction::Search)) => {
                        this.toggle_search(_window, cx);
                    }
                    Some(KeyAction::Bytes(bytes)) => {
                        // While the IME is actively composing (preedit text
                        // is non-empty), editing keys — Backspace / Delete —
                        // must reach the platform's input context so it can
                        // edit the in-progress composition (e.g. delete a
                        // pinyin letter in the candidate window). If we wrote
                        // the raw control byte to the PTY and called
                        // `stop_propagation`, the IME would never see the
                        // key and the Backspace would silently leak into the
                        // terminal as a DEL (#58). So: when preedit is active
                        // AND the matched byte is an editing key, fall through
                        // to the platform instead of writing.
                        let is_editing_key =
                            matches!(bytes.as_slice(), [0x08] | [0x7f] | [0x1b, b'[', b'3', b'~']);
                        let composing = this
                            .marked_text
                            .lock()
                            .as_ref()
                            .map(|s| !s.is_empty())
                            .unwrap_or(false);
                        if composing && is_editing_key {
                            return;
                        }
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
                    move |bounds, window, _cx| {
                        let mut last = last_bounds.lock();
                        // Keep the PTY grid in sync with the current bounds
                        // (see `sync_pty_grid` for why we resize eagerly).
                        let resized = sync_pty_grid(
                            bounds,
                            cell_width,
                            line_height,
                            &last_resized_grid,
                            &session,
                            &selection_prepaint,
                            window,
                            view_id,
                        );

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

                        // Keep the PTY grid in sync with the *current* paint
                        // bounds on every frame. The frame pump drives repaints
                        // through `cx.notify()` without re-running layout, so the
                        // prepaint-time resize alone would miss any size change
                        // that happened without a relayout — e.g. the canvas
                        // bounds spontaneously shrinking (or a detached/split
                        // layout) would lock the grid at the wrong size. By
                        // resizing here we self-correct every painted frame.
                        sync_pty_grid(
                            bounds,
                            cell_width,
                            line_height,
                            &last_resized_grid_paint,
                            &session_paint,
                            &selection_paint_resize,
                            window,
                            view_id,
                        );

                        let text_system = window.text_system().clone();

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
                            //
                            // `is_empty()` hides the highlight for a clicked-but-
                            // not-yet-dragged single cell so a plain click
                            // doesn't visually select anything.
                            let (sel_start, sel_end) =
                                if let Some(ref s) = sel.as_ref().filter(|s| !s.is_empty()) {
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

                                    let is_dim = cell.flags.contains(Flags::DIM);
                                    let bg_color: Option<Hsla> = if is_sel {
                                        Some(rgb(selection_bg()).into())
                                    } else if is_inv {
                                        Some(rgb(if is_dim { dim_color(cell.fg) } else { cell.fg }).into())
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

                            // Search match highlights. Drawn after the cell
                            // background layer but before the text so the text
                            // remains visible on top of the match highlight.
                            // Each match is converted from grid line to
                            // viewport row via `display_offset`, the same as
                            // selection highlighting.
                            //
                            // The active match gets a brighter highlight so
                            // the user can see which one they're on.
                            if !search_matches_paint.is_empty() {
                                let vp_row = row_idx as i32 - display_offset;
                                let mut search_rects: Vec<(usize, usize, Hsla)> = Vec::new();
                                for (mi, m) in search_matches_paint.iter().enumerate() {
                                    let is_active = Some(mi) == search_active_paint;
                                    if m.start_line == m.end_line {
                                        // Single-row match.
                                        if vp_row == m.start_line {
                                            let lo = m.start_col.min(num_cols);
                                            let hi = (m.end_col + 1).min(num_cols).max(lo + 1);
                                            let color = search_match_color(is_active);
                                            search_rects.push((lo, hi - lo, color));
                                        }
                                    } else {
                                        // Multi-row match.
                                        if vp_row == m.start_line {
                                            let lo = m.start_col.min(num_cols);
                                            let color = search_match_color(is_active);
                                            search_rects.push((lo, num_cols - lo, color));
                                        } else if vp_row > m.start_line && vp_row < m.end_line {
                                            let color = search_match_color(is_active);
                                            search_rects.push((0, num_cols, color));
                                        } else if vp_row == m.end_line {
                                            let hi = (m.end_col + 1).min(num_cols);
                                            let color = search_match_color(is_active);
                                            search_rects.push((0, hi, color));
                                        }
                                    }
                                }
                                for (col, n, color) in search_rects {
                                    if n == 0 {
                                        continue;
                                    }
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
                                // Focused → solid cursor; not focused → hollow
                                // outline (no blinking).
                                let focused = is_focused_paint.load(Ordering::Acquire);
                                paint_cursor(
                                    cursor_shape,
                                    cx_x,
                                    cx_y,
                                    cell_width,
                                    line_height,
                                    focused,
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
                        let cell_width = cell_width;
                        let last_bounds = last_bounds_c.clone();
                        let display_offset_mouse = display_offset_mouse.clone();
                        move |event, _window, cx| {
                            let delta = event.delta.pixel_delta(line_height);
                            let dy = delta.y / line_height;
                            if dy.abs() < 0.001 {
                                return;
                            }
                            // Shift bypasses alt-screen / mouse-mode
                            // forwarding so the user can reach the
                            // scrollback buffer even inside vim / less.
                            let shift = event.modifiers.shift;
                            // Compute the pointer's grid cell for
                            // mouse-mode wheel reports. Fall back to
                            // (0, 0) if the bounds aren't known yet or
                            // the pointer is outside the grid.
                            let cell = if shift {
                                (0, 0)
                            } else if let Some(bounds) = *last_bounds.lock() {
                                let offset = display_offset_mouse.load(Ordering::Relaxed);
                                mouse_to_grid(
                                    event.position,
                                    bounds,
                                    cell_width,
                                    line_height,
                                    offset,
                                )
                                .map(|(c, r)| (c, r.max(0) as usize))
                                .unwrap_or((0, 0))
                            } else {
                                (0, 0)
                            };
                            let _ = entity.update(cx, |this, _cx| {
                                this.scroll_accumulator += dy;
                                let lines = this.scroll_accumulator.trunc() as i32;
                                if lines != 0 {
                                    this.scroll_accumulator -= lines as f32;
                                    session.handle_wheel(lines, cell, shift);
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
                        let session_for_dblclick = session_c.clone();
                        let session_for_mouse = session_c.clone();
                        move |event, _window, _cx| {
                            // If the program is in mouse-reporting mode and
                            // Shift is NOT held, forward the click to the PTY
                            // instead of doing a local text selection. This is
                            // what lets TUI tools (e.g. opencode) handle clicks.
                            if !event.modifiers.shift {
                                if let Some(bounds) = *last_bounds.lock() {
                                    let offset = display_offset_mouse.load(Ordering::Relaxed);
                                    if let Some((col, row)) =
                                        mouse_to_grid(event.position, bounds, cell_width, line_height, offset)
                                    {
                                        if session_for_mouse
                                            .report_mouse_button(0, (col as usize, row as usize), true, false)
                                        {
                                            return;
                                        }
                                    }
                                }
                            }
                            if let Some(bounds) = *last_bounds.lock() {
                                // Skip if click is in the scrollbar region
                                // (rightmost WIDTH px). The `Scrollbar`
                                // widget overlays that strip and calls
                                // `cx.stop_propagation()` on its own click
                                // handler, but we also guard here in case the
                                // scrollbar is hidden (no history) — in which
                                // case the strip is empty and we want normal
                                // selection. Scrollbar::WIDTH is private, so we
                                // hardcode the gpui-component value
                                // (THUMB_ACTIVE_INSET*2 + THUMB_ACTIVE_WIDTH).
                                let in_scrollbar = event.position.x
                                    > bounds.origin.x + bounds.size.width - px(16.0);
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
                                    // Double-click selects the word at the click
                                    // position; triple-click selects the whole line.
                                    // Single click starts a normal drag selection.
                                    let new_sel = if event.click_count >= 3 {
                                        Some(select_line(
                                            session_for_dblclick
                                                .with_term(|term| term.grid().columns()),
                                            row,
                                        ))
                                    } else if event.click_count == 2 {
                                        session_for_dblclick.with_term(|term| {
                                            let grid = term.grid();
                                            select_word(grid, grid.columns(), col, row)
                                        })
                                    } else {
                                        Some(Selection::new(col, row))
                                    };
                                    if let Some(sel) = new_sel {
                                        *selection.lock() = Some(sel);
                                    }
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
                        let session_for_mouse_move = session_c.clone();
                        move |event, _window, _cx| {
                            if event.dragging() {
                                // Forward drag motions to the PTY in mouse mode
                                // (Shift bypasses to allow local selection).
                                if !event.modifiers.shift {
                                    if let Some(bounds) = *last_bounds.lock() {
                                        let offset =
                                            display_offset_mouse_move.load(Ordering::Relaxed);
                                        if let Some((col, row)) = mouse_to_grid(
                                            event.position,
                                            bounds,
                                            cell_width,
                                            line_height,
                                            offset,
                                        ) {
                                            if session_for_mouse_move
                                                .report_mouse_button(0, (col as usize, row as usize), true, true)
                                            {
                                                return;
                                            }
                                        }
                                    }
                                }
                                if let Some(bounds) = *last_bounds.lock() {
                                    let offset = display_offset_mouse_move.load(Ordering::Relaxed);
                                    if let Some((col, row)) = mouse_to_grid(
                                        event.position,
                                        bounds,
                                        cell_width,
                                        line_height,
                                        offset,
                                    ) {
                                        // Only extend an in-progress drag selection.
                                        // Word/line selections from double/triple click
                                        // have `active == false` and should not be
                                        // mutated by a subsequent drag — otherwise
                                        // dragging after a double-click would
                                        // collapse the word selection back to a
                                        // single cell.
                                        if let Some(ref mut sel) = *selection.lock() {
                                            if !sel.active {
                                                return;
                                            }
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
                        let session_for_mouse_up = session_c.clone();
                        move |event, _window, _cx| {
                            // If the press started a mouse-mode drag, just send
                            // the release to the PTY; skip local selection logic.
                            if !event.modifiers.shift {
                                if let Some(bounds) = *last_bounds.lock() {
                                    let offset = display_offset_mouse_up.load(Ordering::Relaxed);
                                    if let Some((up_col, up_row)) = mouse_to_grid(
                                        event.position,
                                        bounds,
                                        cell_width,
                                        line_height,
                                        offset,
                                    ) {
                                        if session_for_mouse_up
                                            .report_mouse_button(0, (up_col as usize, up_row as usize), false, false)
                                        {
                                            return;
                                        }
                                    }
                                }
                            }
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
                                    // Only apply the "click without drag → clear"
                                    // logic to in-progress drag selections
                                    // (`active == true`). Word/line selections
                                    // from double/triple click have
                                    // `active == false` and must survive the
                                    // mouse-up even when start == up (e.g.
                                    // double-clicking a single-character word).
                                    let clear = if let Some(ref sel) = *sel_guard {
                                        sel.active
                                            && sel.start_col == up_col
                                            && sel.start_row == up_row
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
                    })
                    .on_mouse_down(MouseButton::Right, {
                        let on_context_menu = on_context_menu.clone();
                        move |event, _window, cx| {
                            // Right-click inside the terminal pane: surface a
                            // Copy/Paste/Select-All context menu via the
                            // app-injected callback. The app owns the
                            // `ContextMenuController` and decides which items
                            // to show; the terminal view just reports the
                            // click position + pane id.
                            if let Some(cb) = &on_context_menu {
                                cb(pane_id_for_ctx, event.position, cx);
                            }
                        }
                    }),
            )
            // Scrollbar overlay: only visible when there is scrollback history.
            // Uses the same `gpui_component::Scrollbar` widget as the rest of
            // the app (History / SFTP / Snippets / Tunnels panels, About
            // window, …) so styling, hover/drag, and fade animations stay
            // consistent. The handle (`TerminalScrollbarHandle`) bridges the
            // pixel-based `ScrollbarHandle` API to the alacritty grid's
            // row-based scroll model.
            .when(history_size_sb.load(Ordering::Relaxed) > 0, |el| {
                // Keep the handle's line_height fresh so its `offset()` /
                // `content_size()` math agrees with the latest font metrics.
                scrollbar_handle.set_line_height(line_height);
                el.child(
                    div()
                        .id("terminal-scrollbar-overlay")
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .w(px(16.0))
                        .child(
                            gpui_component::scroll::Scrollbar::vertical(&scrollbar_handle)
                                .scrollbar_show(gpui_component::scroll::ScrollbarShow::Hover),
                        ),
                )
            })
            // Connection overlay — shown for remote sessions (SSH / Telnet)
            // and during the local-PTY startup window, while
            // `PendingPtyBackend` is constructing the real `PtyBackend` on a
            // background thread. Once the local PTY is ready the overlay
            // fades out via `update_status(RemoteStatus::Local, ...)`.
            //
            // Note: the host-key confirmation prompt is no longer rendered
            // here. It is surfaced via the global `AlertController` (held by
            // `CrabportApp`), which `render_content` triggers when it sees a
            // pending host key on the active terminal view. That way the
            // dialog overlays the whole window and is unaffected by the
            // terminal container's padding.
            .when(is_remote || overlay_visible, |el| {
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
        // `range` is in UTF-16 code units (the NSTextInputClient convention),
        // so we must slice the UTF-16 representation — never the raw `String`
        // bytes. Slicing a `String` by a UTF-16 index would land mid-character
        // for CJK text and panic (`byte index … is not a char boundary`),
        // which aborts the process under the panic=abort config. This is the
        // crash triggered when switching IMEs: IMK re-queries the current
        // character index with a stale range that doesn't align to byte
        // boundaries.
        let marked = self.marked_text.lock().clone().unwrap_or_default();
        let utf16: Vec<u16> = marked.encode_utf16().collect();
        let start = range.start.min(utf16.len());
        let end = range.end.min(utf16.len());
        if start >= end {
            Some(String::new())
        } else {
            String::from_utf16(&utf16[start..end]).ok()
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
///
/// `focused` controls the cursor style: when the pane has keyboard focus the
/// cursor is rendered solid; when not focused it's rendered as a hollow
/// outline so the user can still see where the cursor is without it competing
/// for attention with the focused pane's solid cursor. There is no blinking.
#[allow(clippy::too_many_arguments)]
fn paint_cursor(
    shape: CursorShape,
    cx_x: Pixels,
    cx_y: Pixels,
    cell_width: Pixels,
    line_height: Pixels,
    focused: bool,
    window: &mut Window,
) {
    match shape {
        CursorShape::Block => {
            if focused {
                // Solid filled block, semi-transparent so the character
                // beneath remains visible.
                let c: Hsla = rgb(term_cursor()).into();
                window.paint_quad(fill(
                    Bounds::new(point(cx_x, cx_y), size(cell_width, line_height)),
                    c.opacity(0.5),
                ));
            } else {
                // Hollow outline so the unfocused pane's cursor stays
                // visible but clearly secondary.
                window.paint_quad(outline(
                    Bounds::new(point(cx_x, cx_y), size(cell_width, line_height)),
                    rgb(term_cursor()),
                    BorderStyle::Solid,
                ));
            }
        }
        CursorShape::HollowBlock => {
            // The requested shape is already hollow; render the outline in
            // both states (it's inherently non-solid).
            window.paint_quad(outline(
                Bounds::new(point(cx_x, cx_y), size(cell_width, line_height)),
                rgb(term_cursor()),
                BorderStyle::Solid,
            ));
        }
        CursorShape::Underline => {
            // For underline/beam there's no meaningful hollow variant, so
            // render the shape at full opacity when focused and dimmed when
            // not focused to convey the secondary state.
            let opacity = if focused { 1.0 } else { 0.4 };
            let c: Hsla = rgb(term_cursor()).into();
            window.paint_quad(fill(
                Bounds::new(
                    point(cx_x, cx_y + line_height - px(2.0)),
                    size(cell_width, px(2.0)),
                ),
                c.opacity(opacity),
            ));
        }
        CursorShape::Beam => {
            let opacity = if focused { 1.0 } else { 0.4 };
            let c: Hsla = rgb(term_cursor()).into();
            window.paint_quad(fill(
                Bounds::new(point(cx_x, cx_y), size(px(1.5), line_height)),
                c.opacity(opacity),
            ));
        }
        CursorShape::Hidden => {}
    }
}

/// Escape regex metacharacters in `s` so the resulting string is treated as
/// a literal substring by [`alacritty_terminal::term::search::RegexSearch`].
/// Mirrors `regex::escape` without pulling the `regex` crate as a direct dep.
fn escape_regex(s: &str) -> String {
    const METACHARS: &[char] = &[
        '\\', '.', '+', '*', '?', '(', ')', '[', ']', '{', '}', '$', '^', '|',
    ];
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        if METACHARS.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Background color for a search-match highlight. The active match gets a
/// brighter accent-tinted color; non-active matches use a muted version.
fn search_match_color(active: bool) -> Hsla {
    // Reuse the accent-ish colors: active = a semi-transparent yellow/orange
    // that pops over the dark terminal bg; inactive = a dimmer variant.
    if active {
        let c: Hsla = rgb(0xFFD668FF).into();
        c.opacity(0.35)
    } else {
        let c: Hsla = rgb(0xFFD668FF).into();
        c.opacity(0.18)
    }
}
