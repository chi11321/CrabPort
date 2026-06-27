use std::rc::Rc;
use std::sync::Arc;

use alacritty_terminal::{
    grid::Dimensions,
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

// ---- TerminalView ----

pub struct TerminalView {
    session: Arc<TerminalSession>,
    focus_handle: FocusHandle,
    font_size: Pixels,
    line_height: Pixels,
    cell_width: Pixels,
    last_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
    selection: Arc<Mutex<Option<Selection>>>,
    bindings: Vec<keybind::Binding>,
    pending_paste: bool,
    pending_copy: bool,
    cursor_visible: bool,
    scroll_accumulator: f32,
    /// Connection overlay state (only meaningful for remote sessions).
    overlay: SharedOverlayState,
    /// Host label for the remote session (empty for local).
    remote_host: String,
    /// Unique count for this terminal, used to generate unique element ids.
    count: u64,
    /// SSH connection info, saved for reconnect (only for remote sessions).
    ssh_info: Option<SshConnectionInfo>,
    /// Callback invoked when the backend closes (e.g. local PTY child exits).
    /// For remote sessions this is `None` — they show a disconnect overlay instead.
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

        // Set initial cursor to steady beam (|) via terminal escape
        session.feed_escape(b"\x1b[6 q");

        let is_remote = !host.is_empty();

        // Subscribe to backend events for error/close notifications
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
                            // For local PTY: invoke on_backend_closed to remove the tab.
                            // For remote sessions: just update overlay, the user can reconnect.
                            if let Some(ref cb) = this.on_backend_closed {
                                // Defer the close to avoid mutating the entity
                                // graph during an active update.
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

        // Poll monitor status and repaint on wakeup signals
        let mut wakeup_rx = session.subscribe_wakeup();
        let entity = cx.entity().downgrade();
        let blink_entity = entity.clone();
        cx.spawn(async move |_this, cx| {
            #[cfg(debug_assertions)]
            tracing::info!("wakeup listener started");
            while let Ok(()) = wakeup_rx.recv().await {
                let _ = entity.update(cx, |this, cx| {
                    if let Some(m) = this.session.monitor() {
                        let new_status = m.status();
                        let mut ov = this.overlay.lock();
                        if new_status != ov.status {
                            ov.update_status(new_status, &this.remote_host);
                        }
                    }
                    cx.notify();
                });
            }
            #[cfg(debug_assertions)]
            tracing::warn!("wakeup listener ended");
        })
        .detach();

        // Cursor blink timer
        cx.spawn(async move |_this, cx| {
            loop {
                smol::Timer::after(std::time::Duration::from_millis(530)).await;
                let _ = blink_entity.update(cx, |this, cx| {
                    this.cursor_visible = !this.cursor_visible;
                    cx.notify();
                });
            }
        })
        .detach();

        // Once the overlay starts fading out, wait for the animation to finish
        // then mark it as fully hidden
        if is_remote {
            let overlay_fade = overlay.clone();
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

    /// Set a callback to be invoked when the backend closes.
    /// Used by the app to remove the tab when a local PTY child exits.
    pub fn set_on_backend_closed(&mut self, f: impl Fn(&mut App) + 'static) {
        self.on_backend_closed = Some(Rc::new(f));
    }

    /// Reconnect a disconnected SSH session by creating a new backend and session.
    pub fn reconnect(&mut self, cx: &mut Context<Self>) {
        let info = match self.ssh_info.clone() {
            Some(i) => i,
            None => return,
        };

        // Close the old session
        self.session.close();

        // Reset transition state so the overlay starts fresh
        gpui_animation::reset_transition(&ElementId::Name(
            format!("connection-overlay-{}", self.count).into(),
        ));

        // Reset overlay state for the new connection attempt
        {
            let mut ov = self.overlay.lock();
            ov.update_status(RemoteStatus::Connecting, &self.remote_host);
        }

        let cols: usize = 80;
        let rows: usize = 24;

        let overlay_cb = self.overlay.clone();
        let backend = Arc::new(crabport_ssh::SshBackend::new(
            info,
            cols as u16,
            rows as u16,
            Arc::new(move |msg: String| {
                overlay_cb.lock().log(ConnectionLogLevel::Info, msg);
            }),
        ));

        let session = Arc::new(TerminalSession::new(backend, cols, rows));
        session.start();
        session.feed_escape(b"\x1b[6 q");

        // Re-subscribe to backend events
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

        // Re-subscribe to wakeup signals
        let mut wakeup_rx = session.subscribe_wakeup();
        let entity = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            while let Ok(()) = wakeup_rx.recv().await {
                let _ = entity.update(cx, |this, cx| {
                    if let Some(m) = this.session.monitor() {
                        let new_status = m.status();
                        let mut ov = this.overlay.lock();
                        if new_status != ov.status {
                            ov.update_status(new_status, &this.remote_host);
                        }
                    }
                    cx.notify();
                });
            }
        })
        .detach();

        // Restart fade-out watcher for the new connection
        let overlay_fade = self.overlay.clone();
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

    // background_color is always None — backgrounds are painted separately via paint_quad
    fn make_run(
        len: usize,
        bold: bool,
        italic: bool,
        fg: u32,
        inverse: bool,
        inverse_bg: u32,
        underline: bool,
    ) -> TextRun {
        let mut run_font = font("Menlo");
        if bold {
            run_font = run_font.bold();
        }
        if italic {
            run_font.style = FontStyle::Italic;
        }
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
}

// ---- GPUI Render ----

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Handle deferred clipboard operations from keybinds
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
        let selection_c = selection.clone();
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
                this.session.write(b"\t");
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
            // Canvas layer: grid snapshot in prepaint, rendering in paint
            .child(
                canvas(
                    // prepaint: resize terminal and snapshot the grid
                    move |bounds, _window, _cx| {
                        let mut last = last_bounds.lock();
                        let (cols, rows) = {
                            let c = (bounds.size.width / cell_width).floor() as usize;
                            let r = (bounds.size.height / line_height).floor() as usize;
                            (c.max(2), r.max(1))
                        };
                        if let Some(ref lb) = *last {
                            let (lc, lr) = {
                                let c = (lb.size.width / cell_width).floor() as usize;
                                let r = (lb.size.height / line_height).floor() as usize;
                                (c.max(2), r.max(1))
                            };
                            if lc != cols || lr != rows {
                                session.resize(cols as u16, rows as u16);
                            }
                        } else {
                            session.resize(cols as u16, rows as u16);
                        }
                        *last = Some(bounds);

                        session.with_term(|term| {
                            let grid = term.grid();
                            let display_offset = grid.display_offset();
                            let num_cols = grid.columns();
                            let num_lines = grid.screen_lines();
                            let mut data = Vec::with_capacity(num_lines);
                            for row in 0..num_lines {
                                let li = alacritty_terminal::index::Line(
                                    row as i32 - display_offset as i32,
                                );
                                let mut cells = Vec::with_capacity(num_cols);
                                for col in 0..num_cols {
                                    let cell = &grid[li][alacritty_terminal::index::Column(col)];
                                    cells.push((cell.c, cell.fg, cell.bg, cell.flags));
                                }
                                data.push(cells);
                            }
                            let cursor = term.renderable_content().cursor;
                            (data, cursor, num_cols, num_lines)
                        })
                    },
                    // paint: three layers per row — base bg, cell backgrounds, text
                    move |bounds, lines, window, cx| {
                        let (grid_data, cursor, num_cols, _num_lines) = lines;
                        let text_system = window.text_system().clone();

                        let sel_guard = selection.lock();
                        let sel: Option<Selection> = sel_guard.clone();
                        drop(sel_guard);

                        for (row_idx, row) in grid_data.iter().enumerate() {
                            let y = bounds.origin.y + line_height * row_idx as f32;

                            // Compute selection column range for this row
                            let (sel_start, sel_end) = if let Some(ref s) = sel {
                                let (sr, er, sc, ec) = s.range();
                                if row_idx < sr || row_idx > er {
                                    (None, None)
                                } else if sr == er {
                                    // Single-row selection: precise column range
                                    let lo = sc.min(num_cols);
                                    let hi = (ec + 1).min(num_cols).max(lo + 1);
                                    (Some(lo), Some(hi))
                                } else if row_idx == sr {
                                    // First row of multi-row selection: start_col to end of line
                                    let col = if s.start_row <= s.end_row {
                                        s.start_col
                                    } else {
                                        s.end_col
                                    };
                                    (Some(col.min(num_cols)), Some(num_cols))
                                } else if row_idx == er {
                                    // Last row of multi-row selection: start of line to end_col
                                    let col = if s.start_row <= s.end_row {
                                        s.end_col
                                    } else {
                                        s.start_col
                                    };
                                    (Some(0), Some(col.saturating_add(1).min(num_cols)))
                                } else {
                                    // Middle rows: entire line selected
                                    (Some(0), Some(num_cols))
                                }
                            } else {
                                (None, None)
                            };

                            // Layer 1: solid row background
                            window.paint_quad(fill(
                                Bounds::new(
                                    point(bounds.origin.x, y),
                                    size(bounds.size.width, line_height),
                                ),
                                rgb(TERM_BG),
                            ));

                            // Layer 2: batched cell backgrounds (selection, custom bg, inverse)
                            // Collect rects then merge adjacent same-color ones to minimise paint_quad calls.
                            {
                                let mut rects: Vec<(usize, usize, Hsla)> = Vec::new(); // (col, cells, color)
                                for (ci, (_c, fg, bg, flags)) in row.iter().enumerate() {
                                    if flags.contains(Flags::WIDE_CHAR_SPACER) {
                                        continue;
                                    }

                                    let is_sel = sel_start
                                        .is_some_and(|ss| ci >= ss && ci < sel_end.unwrap_or(0));
                                    let has_custom_bg = *bg != Color::Named(NamedColor::Background);
                                    let is_inv = flags.contains(Flags::INVERSE);
                                    let wide = flags.contains(Flags::WIDE_CHAR);

                                    let bg_color: Option<Hsla> = if is_sel {
                                        Some(rgb(SELECTION_BG).into())
                                    } else if is_inv {
                                        let fg_raw = ansi_color_to_rgb(
                                            fg,
                                            &alacritty_terminal::term::color::Colors::default(),
                                        );
                                        Some(rgb(fg_raw).into())
                                    } else if has_custom_bg {
                                        let bg_raw = ansi_color_to_rgb(
                                            bg,
                                            &alacritty_terminal::term::color::Colors::default(),
                                        );
                                        Some(rgb(bg_raw).into())
                                    } else {
                                        None
                                    };

                                    if let Some(color) = bg_color {
                                        let cells = if wide { 2 } else { 1 };
                                        // Try to merge with the previous rect
                                        if let Some(last) = rects.last_mut() {
                                            if last.0 + last.1 == ci && last.2 == color {
                                                last.1 += cells;
                                                continue;
                                            }
                                        }
                                        rects.push((ci, cells, color));
                                    }
                                }

                                for (col, cells, color) in rects {
                                    let cell_x = bounds.origin.x + col as f32 * cell_width;
                                    window.paint_quad(fill(
                                        Bounds::new(
                                            point(cell_x, y),
                                            size(cell_width * cells as f32, line_height),
                                        ),
                                        color,
                                    ));
                                }
                            }

                            // Layer 3: text runs — background_color always None,
                            // runs split only by fg/style changes (not selection state)
                            let mut line_text = String::new();
                            let mut runs: Vec<TextRun> = Vec::new();
                            let mut run_start = 0usize;
                            let mut cur_fg = TERM_FG;
                            let mut cur_inv_bg = TERM_BG;
                            let mut cur_bold = false;
                            let mut cur_italic = false;
                            let mut cur_underline = false;
                            let mut cur_inverse = false;

                            for (ci, (c, fg, bg, flags)) in row.iter().enumerate() {
                                if flags.contains(Flags::WIDE_CHAR_SPACER) {
                                    continue;
                                }

                                let ef = ansi_color_to_rgb(
                                    fg,
                                    &alacritty_terminal::term::color::Colors::default(),
                                );
                                let eb = ansi_color_to_rgb(
                                    bg,
                                    &alacritty_terminal::term::color::Colors::default(),
                                );
                                let is_b = flags.contains(Flags::BOLD);
                                let is_i = flags.contains(Flags::ITALIC);
                                let is_u = flags.contains(Flags::UNDERLINE);
                                let is_inv = flags.contains(Flags::INVERSE);

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

                                if *c == '\t' {
                                    let ns = ((ci / 8) + 1) * 8 - ci;
                                    for _ in 0..ns {
                                        line_text.push(' ');
                                    }
                                } else {
                                    line_text.push(*c);
                                }
                            }

                            // Flush the last run
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

                            // Pad to full column width so shape_line covers the whole row
                            if line_text.len() < num_cols {
                                let pad = num_cols - line_text.len();
                                line_text.extend(std::iter::repeat(' ').take(pad));
                                runs.push(TextRun {
                                    len: pad,
                                    font: font("Menlo"),
                                    color: rgb(TERM_FG).into(),
                                    background_color: None,
                                    underline: None,
                                    strikethrough: None,
                                });
                            }

                            if !line_text.is_empty() && !runs.is_empty() {
                                let shaped = text_system.shape_line(
                                    line_text.into(),
                                    font_size,
                                    &runs,
                                    None,
                                );
                                let _ = shaped.paint(
                                    point(bounds.origin.x, y),
                                    line_height,
                                    window,
                                    cx,
                                );
                            }
                        }

                        // Cursor rendering
                        if cursor.shape != CursorShape::Hidden
                            && cursor.point.line.0 >= 0
                            && cursor.point.line.0 < _num_lines as i32
                        {
                            let cx_x = bounds.origin.x + cursor.point.column.0 as f32 * cell_width;
                            let cx_y = bounds.origin.y + cursor.point.line.0 as f32 * line_height;
                            match cursor.shape {
                                CursorShape::Block | CursorShape::HollowBlock => {
                                    let c: Hsla = rgb(TERM_CURSOR).into();
                                    window.paint_quad(fill(
                                        Bounds::new(
                                            point(cx_x, cx_y),
                                            size(cell_width, line_height),
                                        ),
                                        c.opacity(0.5),
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
            // Transparent overlay div for mouse events (selection + scroll)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .on_scroll_wheel({
                        let session = session_c.clone();
                        let entity = entity.clone();
                        let line_height = line_height;
                        move |event, _window, cx| {
                            let delta = event.delta.pixel_delta(line_height);
                            let dy = delta.y / line_height;
                            if dy.abs() < 0.001 {
                                return;
                            }
                            let _ = entity.update(cx, |this, cx| {
                                this.scroll_accumulator += dy;
                                let lines = this.scroll_accumulator.trunc() as i32;
                                if lines != 0 {
                                    this.scroll_accumulator -= lines as f32;
                                    session.scroll(lines);
                                    cx.notify();
                                }
                            });
                        }
                    })
                    .on_mouse_down(MouseButton::Left, {
                        let selection = selection_c.clone();
                        let last_bounds = last_bounds_c.clone();
                        let cell_width = cell_width;
                        let line_height = line_height;
                        let entity = entity.clone();
                        move |event, _window, cx| {
                            #[cfg(debug_assertions)]
                            tracing::debug!("mouse-down at {:?}", event.position);
                            if let Some(bounds) = *last_bounds.lock() {
                                if let Some((col, row)) =
                                    mouse_to_grid(event.position, bounds, cell_width, line_height)
                                {
                                    #[cfg(debug_assertions)]
                                    tracing::debug!("sel start col={} row={}", col, row);
                                    // Start a new selection; drag will extend it.
                                    // If the mouse is released without moving we
                                    // clear it in on_mouse_up.
                                    *selection.lock() = Some(Selection::new(col, row));
                                    let _ = entity.update(cx, |_, cx| cx.notify());
                                }
                            }
                        }
                    })
                    .on_mouse_move({
                        let selection = selection_c.clone();
                        let last_bounds = last_bounds_c.clone();
                        let cell_width = cell_width;
                        let line_height = line_height;
                        let entity = entity.clone();
                        move |event, _window, cx| {
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
                                            let _ = entity.update(cx, |_, cx| cx.notify());
                                        }
                                    }
                                }
                            }
                        }
                    })
                    .on_mouse_up(MouseButton::Left, {
                        let selection = selection_c.clone();
                        let last_bounds = last_bounds_c.clone();
                        let cell_width = cell_width;
                        let line_height = line_height;
                        let entity = entity.clone();
                        move |event, _window, cx| {
                            #[cfg(debug_assertions)]
                            tracing::debug!("mouse-up");
                            let clear = if let Some(bounds) = *last_bounds.lock() {
                                // Determine whether the mouse moved at least one cell
                                // from where the selection started
                                if let Some((up_col, up_row)) =
                                    mouse_to_grid(event.position, bounds, cell_width, line_height)
                                {
                                    let sel_guard = selection.lock();
                                    if let Some(ref sel) = *sel_guard {
                                        // A pure click: start == end position
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
                                // Single click with no drag — clear the selection
                                *selection.lock() = None;
                            } else {
                                // Drag release — keep the selection but mark it inactive
                                if let Some(ref mut sel) = *selection.lock() {
                                    sel.active = false;
                                }
                            }
                            let _ = entity.update(cx, |_, cx| cx.notify());
                        }
                    }),
            )
            // Connection overlay (remote sessions only)
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
