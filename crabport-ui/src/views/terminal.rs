use std::num::NonZeroUsize;
use std::rc::Rc;
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
};

use alacritty_terminal::{
    grid::Dimensions,
    term::TermDamage,
    term::cell::Flags,
    vte::ansi::{Color, CursorShape, NamedColor},
};
use crabport_core::keybind::{self, KeyAction, TerminalAction};
use crabport_ssh::session::SshConnectionInfo;
use crabport_terminal::pty::PtyBackend;
use crabport_terminal::terminal::{CrabPortMonitor, RemoteStatus, TerminalSession};

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::{animation::TransitionExt, transition::general::EaseInOutCubic};
use lru::LruCache;
use parking_lot::Mutex;
use rust_i18n::t;

use crate::app::{CrabPortTab, TerminalShiftTab, TerminalTab};
use crate::components::button::Button;

pub mod connection_overlay;

mod color;
mod selection;

use color::*;
use connection_overlay::*;
use selection::*;

// ---------------------------------------------------------------------------
// Shared, build-once resources.
// ---------------------------------------------------------------------------

/// Palette built once and reused for every cell of every frame.
fn palette() -> &'static alacritty_terminal::term::color::Colors {
    static P: OnceLock<alacritty_terminal::term::color::Colors> = OnceLock::new();
    P.get_or_init(alacritty_terminal::term::color::Colors::default)
}

/// Pre-built font variants, cloned cheaply per run.
struct Fonts {
    regular: Font,
    bold: Font,
    italic: Font,
    bold_italic: Font,
}

fn fonts() -> &'static Fonts {
    static F: OnceLock<Fonts> = OnceLock::new();
    F.get_or_init(|| {
        let base = font("Menlo");
        let mut italic = base.clone();
        italic.style = FontStyle::Italic;
        let mut bold_italic = base.clone().bold();
        bold_italic.style = FontStyle::Italic;
        Fonts {
            regular: base.clone(),
            bold: base.bold(),
            italic,
            bold_italic,
        }
    })
}

fn pick_font(bold: bool, italic: bool) -> Font {
    let f = fonts();
    match (bold, italic) {
        (false, false) => f.regular.clone(),
        (true, false) => f.bold.clone(),
        (false, true) => f.italic.clone(),
        (true, true) => f.bold_italic.clone(),
    }
}

// ---------------------------------------------------------------------------
// Render snapshot / shaped-line cache.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct CellSnap {
    c: char,
    fg: u32,
    bg: u32,
    flags: Flags,
    custom_bg: bool,
}

#[derive(Clone)]
struct RowSnapshot {
    cells: Vec<CellSnap>,
    hash: u64,
    /// True if any cell needs a background quad (custom bg or inverse).
    has_bg: bool,
}

impl Default for RowSnapshot {
    fn default() -> Self {
        Self {
            cells: Vec::new(),
            hash: u64::MAX,
            has_bg: false,
        }
    }
}

struct RenderCache {
    rows: Vec<RowSnapshot>,
    /// Keyed by row content hash so scrolled-back lines reuse their shaping.
    shaped: LruCache<u64, ShapedLine>,
    cols: usize,
    rows_count: usize,
}

impl Default for RenderCache {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            shaped: LruCache::new(NonZeroUsize::new(1024).unwrap()),
            cols: 0,
            rows_count: 0,
        }
    }
}

impl RenderCache {
    fn resize(&mut self, cols: usize, rows: usize) {
        self.cols = cols;
        self.rows_count = rows;
        self.rows = vec![RowSnapshot::default(); rows];
        // Keep the shaped LRU — identical lines after resize still hit.
    }

    fn clear_all(&mut self) {
        self.rows.clear();
        self.shaped.clear();
        self.cols = 0;
        self.rows_count = 0;
    }
}

fn hash_row(cells: &[CellSnap]) -> u64 {
    use std::hash::Hasher;
    let mut h = rustc_hash::FxHasher::default();
    for c in cells {
        h.write_u32(c.c as u32);
        h.write_u32(c.fg);
        h.write_u32(c.bg);
        h.write_u16(c.flags.bits() as u16);
    }
    h.finish()
}

type SharedRenderCache = Arc<Mutex<RenderCache>>;

// ---- TerminalView ----

pub struct TerminalView {
    session: Arc<TerminalSession>,
    focus_handle: FocusHandle,
    font_size: Pixels,
    line_height: Pixels,
    cell_width: Pixels,
    last_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
    selection: Arc<Mutex<Option<Selection>>>,
    render_cache: SharedRenderCache,
    /// Set by data/blink/status; consumed by the ~120Hz frame pump.
    needs_repaint: Arc<AtomicBool>,
    bindings: Vec<keybind::Binding>,
    pending_paste: bool,
    pending_copy: bool,
    cursor_visible: bool,
    scroll_accumulator: f32,
    overlay: SharedOverlayState,
    remote_host: String,
    count: u64,
    ssh_info: Option<SshConnectionInfo>,
    on_backend_closed: Option<Rc<dyn Fn(&mut App)>>,
}

impl TerminalView {
    pub fn new(count: u64, cx: &mut Context<Self>) -> Self {
        let cols: usize = 80;
        let rows: usize = 24;
        let backend = Arc::new(
            PtyBackend::new(cols as u16, rows as u16).expect("failed to create pty backend"),
        );
        Self::with_backend(backend, cols, rows, None, count, cx)
    }

    pub fn with_backend(
        backend: Arc<dyn crabport_terminal::terminal::CrabPortTerminal>,
        cols: usize,
        rows: usize,
        ssh_info: Option<SshConnectionInfo>,
        count: u64,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_backend_and_host(backend, cols, rows, String::new(), ssh_info, count, cx)
    }

    pub fn with_backend_and_host(
        backend: Arc<dyn crabport_terminal::terminal::CrabPortTerminal>,
        cols: usize,
        rows: usize,
        host: String,
        ssh_info: Option<SshConnectionInfo>,
        count: u64,
        cx: &mut Context<Self>,
    ) -> Self {
        let overlay = Arc::new(Mutex::new(ConnectionOverlayState::new()));
        Self::with_backend_and_host_and_overlay(
            backend, cols, rows, host, overlay, ssh_info, count, cx,
        )
    }

    pub fn with_backend_and_host_and_overlay(
        backend: Arc<dyn crabport_terminal::terminal::CrabPortTerminal>,
        cols: usize,
        rows: usize,
        host: String,
        overlay: SharedOverlayState,
        ssh_info: Option<SshConnectionInfo>,
        count: u64,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let font_size = px(13.0);
        let line_height = px(20.0);
        let cell_width = px(7.8);

        let session = Arc::new(TerminalSession::new(backend, cols, rows));
        session.start();

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
        let pump_entity = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            loop {
                smol::Timer::after(std::time::Duration::from_micros(8333)).await;
                if dirty_pump.swap(false, Ordering::AcqRel) {
                    if pump_entity.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }
        })
        .detach();

        // Cursor blink: flips state then marks dirty (no direct notify).
        let dirty_blink = needs_repaint.clone();
        let blink_entity = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            loop {
                smol::Timer::after(std::time::Duration::from_millis(530)).await;
                let ok = blink_entity
                    .update(cx, |this, _cx| {
                        this.cursor_visible = !this.cursor_visible;
                    })
                    .is_ok();
                if !ok {
                    break;
                }
                dirty_blink.store(true, Ordering::Release);
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
            cursor_visible: true,
            scroll_accumulator: 0.0,
            overlay,
            remote_host: host,
            count,
            ssh_info,
            on_backend_closed: None,
        }
    }

    pub fn monitor(&self) -> Option<&dyn CrabPortMonitor> {
        self.session.monitor()
    }

    pub fn set_on_backend_closed(&mut self, f: impl Fn(&mut App) + 'static) {
        self.on_backend_closed = Some(Rc::new(f));
    }

    pub fn reconnect(&mut self, cx: &mut Context<Self>) {
        let info = match self.ssh_info.clone() {
            Some(i) => i,
            None => return,
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

        let overlay_cb = self.overlay.clone();
        let backend = Arc::new(crabport_ssh::backend::SshBackend::new(
            info,
            cols as u16,
            rows as u16,
            Arc::new(move |msg: String| {
                overlay_cb.lock().log(ConnectionLogLevel::Info, msg);
            }),
        ));

        let session = Arc::new(TerminalSession::new(backend, cols, rows));
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
        cx.notify();
    }

    fn resolve_keystroke(
        keystroke: &Keystroke,
        bindings: &[keybind::Binding],
    ) -> Option<KeyAction> {
        if let Some(action) = keybind::resolve(keystroke, bindings) {
            return Some(action.clone());
        }
        let m = &keystroke.modifiers;
        if !m.control && !m.platform && !m.alt {
            if let Some(key_char) = &keystroke.key_char {
                if !key_char.is_empty() {
                    return Some(KeyAction::Bytes(key_char.as_bytes().to_vec()));
                }
            }
        }
        None
    }

    fn copy_selected_text(session: &Arc<TerminalSession>, sel: &Selection) -> String {
        session.with_term(|term| {
            let grid = term.grid();
            let display_offset = grid.display_offset();
            let num_cols = grid.columns();
            let num_lines = grid.screen_lines();
            let (sr, er, sc, ec) = sel.range();
            let mut result = String::new();
            for row in sr..=er.min(num_lines.saturating_sub(1)) {
                if row > sr {
                    result.push('\n');
                }
                let li = alacritty_terminal::index::Line(row as i32 - display_offset as i32);
                let (cs, ce) = if sel.start_row <= sel.end_row {
                    let cs = if row == sr { sc } else { 0 };
                    let ce = if row == er { ec + 1 } else { num_cols };
                    (cs, ce)
                } else {
                    let cs = if row == sr { ec } else { 0 };
                    let ce = if row == er { sc + 1 } else { num_cols };
                    (cs, ce)
                };
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

    fn make_run(
        len: usize,
        bold: bool,
        italic: bool,
        fg: u32,
        inverse: bool,
        inverse_bg: u32,
        underline: bool,
    ) -> TextRun {
        let run_font = pick_font(bold, italic);
        let fg_color = if inverse { rgb(inverse_bg) } else { rgb(fg) };
        TextRun {
            len,
            font: run_font,
            color: fg_color.into(),
            background_color: None,
            underline: if underline {
                Some(UnderlineStyle {
                    color: Some(fg_color.into()),
                    thickness: px(1.0),
                    wavy: false,
                })
            } else {
                None
            },
            strikethrough: None,
        }
    }

    fn build_runs(cells: &[CellSnap], num_cols: usize) -> (String, Vec<TextRun>) {
        let mut line_text = String::new();
        let mut runs: Vec<TextRun> = Vec::new();
        let mut run_start = 0usize;
        let mut cur_fg = TERM_FG;
        let mut cur_inv_bg = TERM_BG;
        let mut cur_bold = false;
        let mut cur_italic = false;
        let mut cur_underline = false;
        let mut cur_inverse = false;

        for (ci, cell) in cells.iter().enumerate() {
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let ef = cell.fg;
            let eb = cell.bg;
            let is_b = cell.flags.contains(Flags::BOLD);
            let is_i = cell.flags.contains(Flags::ITALIC);
            let is_u = cell.flags.contains(Flags::UNDERLINE);
            let is_inv = cell.flags.contains(Flags::INVERSE);

            let new_run = ef != cur_fg
                || eb != cur_inv_bg
                || is_b != cur_bold
                || is_i != cur_italic
                || is_u != cur_underline
                || is_inv != cur_inverse;

            if new_run {
                let rl = line_text.len() - run_start;
                if rl > 0 {
                    runs.push(Self::make_run(
                        rl,
                        cur_bold,
                        cur_italic,
                        cur_fg,
                        cur_inverse,
                        cur_inv_bg,
                        cur_underline,
                    ));
                }
                run_start = line_text.len();
                cur_fg = ef;
                cur_inv_bg = eb;
                cur_bold = is_b;
                cur_italic = is_i;
                cur_underline = is_u;
                cur_inverse = is_inv;
            }

            if cell.c == '\t' {
                let ns = ((ci / 8) + 1) * 8 - ci;
                for _ in 0..ns {
                    line_text.push(' ');
                }
            } else {
                line_text.push(cell.c);
            }
        }

        let rl = line_text.len() - run_start;
        if rl > 0 {
            runs.push(Self::make_run(
                rl,
                cur_bold,
                cur_italic,
                cur_fg,
                cur_inverse,
                cur_inv_bg,
                cur_underline,
            ));
        }

        if line_text.len() < num_cols {
            let pad = num_cols - line_text.len();
            line_text.extend(std::iter::repeat(' ').take(pad));
            runs.push(TextRun {
                len: pad,
                font: pick_font(false, false),
                color: rgb(TERM_FG).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }

        (line_text, runs)
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
        let cursor_visible = self.cursor_visible;

        let ov = self.overlay.lock();
        let overlay_visible = ov.is_visible();
        let is_fading_out = ov.is_fading_out();
        let log_entries: Vec<ConnectionLogEntry> = ov.logs.clone();
        let current_status = ov.status;
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
            .bg(rgb(TERM_BG))
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
                    }
                    None => {}
                }
            }))
            .child(
                canvas(
                    // ---- prepaint: resize + try_lock incremental snapshot ----
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

                            let cursor = term.renderable_content().cursor;
                            (Some(cursor), grid_cols, grid_lines)
                        });

                        match got {
                            Some(v) => v,
                            None => {
                                let cache = render_cache.lock();
                                (None, cache.cols, cache.rows_count)
                            }
                        }
                    },
                    // ---- paint: hash-keyed LRU shaped lines ----
                    move |bounds, lines, window, cx| {
                        let (cursor, num_cols, _num_lines) = lines;
                        let text_system = window.text_system().clone();

                        let sel_guard = selection.lock();
                        let sel: Option<Selection> = sel_guard.clone();
                        drop(sel_guard);

                        let mut cache = render_cache_paint.lock();

                        // Single viewport-wide background fill.
                        window.paint_quad(fill(
                            Bounds::new(bounds.origin, bounds.size),
                            rgb(TERM_BG),
                        ));

                        let row_count = cache.rows_count;
                        for row_idx in 0..row_count {
                            let y = bounds.origin.y + line_height * row_idx as f32;

                            let (sel_start, sel_end) = if let Some(ref s) = sel {
                                let (sr, er, sc, ec) = s.range();
                                if row_idx < sr || row_idx > er {
                                    (None, None)
                                } else if sr == er {
                                    let lo = sc.min(num_cols);
                                    let hi = (ec + 1).min(num_cols).max(lo + 1);
                                    (Some(lo), Some(hi))
                                } else if row_idx == sr {
                                    let col = if s.start_row <= s.end_row {
                                        s.start_col
                                    } else {
                                        s.end_col
                                    };
                                    (Some(col.min(num_cols)), Some(num_cols))
                                } else if row_idx == er {
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
                                        Some(rgb(SELECTION_BG).into())
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
                            let hash = row.hash;
                            if cache.shaped.peek(&hash).is_none() {
                                let (line_text, runs) =
                                    Self::build_runs(&cache.rows[row_idx].cells, num_cols);
                                if !line_text.is_empty() && !runs.is_empty() {
                                    let shaped = text_system.shape_line(
                                        line_text.into(),
                                        font_size,
                                        &runs,
                                        None,
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
                        if let Some(cursor) = cursor
                            && cursor.shape != CursorShape::Hidden
                            && cursor.point.line.0 >= 0
                            && cursor.point.line.0 < row_count as i32
                        {
                            let cx_x = bounds.origin.x + cursor.point.column.0 as f32 * cell_width;
                            let cx_y = bounds.origin.y + cursor.point.line.0 as f32 * line_height;
                            match cursor.shape {
                                CursorShape::Block => {
                                    let c: Hsla = rgb(TERM_CURSOR).into();
                                    window.paint_quad(fill(
                                        Bounds::new(
                                            point(cx_x, cx_y),
                                            size(cell_width, line_height),
                                        ),
                                        c.opacity(0.5),
                                    ));
                                }
                                CursorShape::HollowBlock => {
                                    window.paint_quad(outline(
                                        Bounds::new(
                                            point(cx_x, cx_y),
                                            size(cell_width, line_height),
                                        ),
                                        rgb(TERM_CURSOR),
                                        BorderStyle::Solid,
                                    ));
                                }
                                CursorShape::Underline => {
                                    window.paint_quad(fill(
                                        Bounds::new(
                                            point(cx_x, cx_y + line_height - px(2.0)),
                                            size(cell_width, px(2.0)),
                                        ),
                                        rgb(TERM_CURSOR),
                                    ));
                                }
                                CursorShape::Beam => {
                                    if cursor_visible {
                                        window.paint_quad(fill(
                                            Bounds::new(
                                                point(cx_x, cx_y),
                                                size(px(1.5), line_height),
                                            ),
                                            rgb(TERM_CURSOR),
                                        ));
                                    }
                                }
                                CursorShape::Hidden => {}
                            }
                        }
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
                            // Scroll marks dirty; the frame pump coalesces repaints.
                            needs_repaint.store(true, Ordering::Release);
                        }
                    })
                    .on_mouse_down(MouseButton::Left, {
                        let selection = selection_c.clone();
                        let last_bounds = last_bounds_c.clone();
                        let needs_repaint = needs_repaint.clone();
                        let cell_width = cell_width;
                        let line_height = line_height;
                        move |event, _window, _cx| {
                            if let Some(bounds) = *last_bounds.lock() {
                                if let Some((col, row)) =
                                    mouse_to_grid(event.position, bounds, cell_width, line_height)
                                {
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
                        move |event, _window, _cx| {
                            if event.dragging() {
                                if let Some(bounds) = *last_bounds.lock() {
                                    if let Some((col, row)) = mouse_to_grid(
                                        event.position,
                                        bounds,
                                        cell_width,
                                        line_height,
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
                        move |event, _window, _cx| {
                            let clear = if let Some(bounds) = *last_bounds.lock() {
                                if let Some((up_col, up_row)) =
                                    mouse_to_grid(event.position, bounds, cell_width, line_height)
                                {
                                    let sel_guard = selection.lock();
                                    if let Some(ref sel) = *sel_guard {
                                        sel.start_col == up_col && sel.start_row == up_row
                                    } else {
                                        false
                                    }
                                } else {
                                    false
                                }
                            } else {
                                false
                            };

                            if clear {
                                *selection.lock() = None;
                            } else if let Some(ref mut sel) = *selection.lock() {
                                sel.active = false;
                            }
                            needs_repaint.store(true, Ordering::Release);
                        }
                    }),
            )
            // Connection overlay (remote sessions only).
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
                    Some(on_reconnect),
                ))
            })
    }
}

// ---- Connection Overlay Rendering ----

fn render_connection_overlay(
    overlay_visible: bool,
    is_fading_out: bool,
    status: RemoteStatus,
    logs: &[ConnectionLogEntry],
    count: u64,
    on_reconnect: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
) -> AnyElement {
    if !overlay_visible {
        return div().into_any_element();
    }

    div()
        .id(ElementId::Name(
            format!("connection-overlay-{}", count).into(),
        ))
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .cursor_default()
        .items_center()
        .justify_center()
        .bg(rgb(TERM_BG))
        .opacity(1.0)
        .with_transition(("connection-overlay-opacity", count))
        .transition_when(
            is_fading_out,
            std::time::Duration::from_millis(500),
            EaseInOutCubic,
            |el| el.opacity(0.0),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_6()
                .max_w(px(400.0))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_3()
                        .child(match status {
                            RemoteStatus::Connecting => render_spinner(),
                            RemoteStatus::Disconnected => {
                                div().size(px(12.0)).rounded_full().bg(rgb(0xf38ba8))
                            }
                            _ => div().size(px(12.0)).rounded_full().bg(rgb(0xa6e3a1)),
                        })
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(TERM_FG))
                                .child(match status {
                                    RemoteStatus::Connecting => "Connecting…",
                                    RemoteStatus::Disconnected => "Connection failed",
                                    _ => "Connected",
                                }),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .w_full()
                        .children(logs.iter().map(|entry| {
                            let prefix = entry.level.prefix();
                            let color = entry.level.color();
                            let text = format!("{}{}", prefix, entry.message);
                            div()
                                .flex()
                                .flex_row()
                                .items_start()
                                .text_sm()
                                .text_color(rgb(color))
                                .child(text)
                        })),
                )
                .when(status == RemoteStatus::Disconnected, |el| {
                    let mut btn =
                        Button::new(ElementId::Name(format!("reconnect-btn-{}", count).into()))
                            .centered(true)
                            .child(t!("terminal.reconnect").to_string());
                    if let Some(cb) = on_reconnect {
                        btn = btn.on_click(move |e, w, a| cb(e, w, a));
                    }
                    el.child(btn)
                }),
        )
        .into_any_element()
}

fn render_spinner() -> Div {
    div()
        .size(px(12.0))
        .rounded_full()
        .border_2()
        .border_color(rgb(TERM_FG))
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
