use std::sync::Arc;

use alacritty_terminal::{
    grid::Dimensions,
    term::cell::Flags,
    vte::ansi::{Color, CursorShape, NamedColor},
};
use crabport_core::keybind::{self, KeyAction, TerminalAction};
use crabport_terminal::pty::PtyBackend;
use crabport_terminal::terminal::CrabPortMonitor;
use crabport_terminal::terminal::TerminalSession;

use gpui::*;
use parking_lot::Mutex;

use crate::app::{CrabPortTab, TerminalShiftTab, TerminalTab};

mod color;
mod selection;

use color::*;
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
}

impl TerminalView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let cols: usize = 80;
        let rows: usize = 24;
        let backend = Arc::new(
            PtyBackend::new(cols as u16, rows as u16).expect("failed to create pty backend"),
        );
        Self::with_backend(backend, cols, rows, cx)
    }

    /// Create a TerminalView with a pre-built backend (e.g. SSH).
    pub fn with_backend(
        backend: Arc<dyn crabport_terminal::terminal::CrabPortTerminal>,
        cols: usize,
        rows: usize,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let font_size = px(13.0);
        let line_height = px(20.0);
        let cell_width = px(7.8);

        let session = Arc::new(TerminalSession::new(backend, cols, rows));
        session.start();

        // Set initial cursor to steady beam (|) via terminal parser
        session.feed_escape(b"\x1b[6 q");

        let mut wakeup_rx = session.subscribe_wakeup();
        let entity = cx.entity().downgrade();
        let blink_entity = entity.clone();
        cx.spawn(async move |_this, cx| {
            #[cfg(debug_assertions)]
            tracing::info!("wakeup listener started");
            while let Ok(()) = wakeup_rx.recv().await {
                let _ = entity.update(cx, |_, cx| cx.notify());
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
        }
    }

    /// Access the backend's `CrabPortMonitor` implementation, if available.
    pub fn monitor(&self) -> Option<&dyn CrabPortMonitor> {
        self.session.monitor()
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
        bg: Option<u32>,
        inverse: bool,
        underline: bool,
        selected: bool,
    ) -> TextRun {
        let mut run_font = font("Menlo");
        if bold {
            run_font = run_font.bold();
        }
        if italic {
            run_font.style = FontStyle::Italic;
        }
        let fg_color = if inverse {
            rgb(bg.unwrap_or(TERM_BG))
        } else {
            rgb(fg)
        };
        let bg_color = if inverse {
            Some(rgb(fg))
        } else if selected {
            Some(rgb(SELECTION_BG))
        } else {
            bg.map(rgb)
        };
        TextRun {
            len,
            font: run_font,
            color: fg_color.into(),
            background_color: bg_color.map(|c| c.into()),
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
        // Clipboard requests from keybinds
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

        // Shared state
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

        div()
            .id("terminal-view")
            .relative()
            .size_full()
            .overflow_hidden()
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
            // Keyboard
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
            // Canvas (bottom layer)
            .child(
                canvas(
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
                    move |bounds, lines, window, cx| {
                        let (grid_data, cursor, num_cols, _num_lines) = lines;
                        let text_system = window.text_system().clone();

                        let sel_guard = selection.lock();
                        let sel: Option<Selection> = sel_guard.clone();
                        drop(sel_guard);

                        for (row_idx, row) in grid_data.iter().enumerate() {
                            let y = bounds.origin.y + line_height * row_idx as f32;

                            // Selection range for this row
                            let (sel_start, sel_end) = if let Some(ref s) = sel {
                                let (sr, er, sc, ec) = s.range();
                                if row_idx < sr || row_idx > er {
                                    (None, None)
                                } else if sr == er {
                                    let lo = sc.min(num_cols);
                                    let hi = (ec + 1).min(num_cols).max(lo + 1);
                                    (Some(lo), Some(hi))
                                } else if row_idx == sr {
                                    if s.start_row <= s.end_row {
                                        (Some(s.start_col.min(num_cols)), Some(num_cols))
                                    } else {
                                        (Some(s.end_col.min(num_cols)), Some(num_cols))
                                    }
                                } else if row_idx == er {
                                    if s.start_row <= s.end_row {
                                        (Some(0), Some(s.end_col.saturating_add(1).min(num_cols)))
                                    } else {
                                        (Some(0), Some(s.start_col.saturating_add(1).min(num_cols)))
                                    }
                                } else {
                                    (Some(0), Some(num_cols))
                                }
                            } else {
                                (None, None)
                            };

                            // Build runs
                            let mut line_text = String::new();
                            let mut runs: Vec<TextRun> = Vec::new();
                            let mut run_start = 0usize;
                            let mut cur_fg = TERM_FG;
                            let mut cur_bg: Option<u32> = None;
                            let mut cur_bold = false;
                            let mut cur_italic = false;
                            let mut cur_underline = false;
                            let mut cur_inverse = false;
                            let mut cur_sel = false;

                            for (ci, (c, fg, bg, flags)) in row.iter().enumerate() {
                                if flags.contains(Flags::WIDE_CHAR_SPACER) {
                                    continue;
                                }
                                let is_sel = sel_start.is_some()
                                    && ci >= sel_start.unwrap()
                                    && ci < sel_end.unwrap();

                                let ef = ansi_color_to_rgb(
                                    fg,
                                    &alacritty_terminal::term::color::Colors::default(),
                                );
                                let eb = if *bg == Color::Named(NamedColor::Background) {
                                    if is_sel { Some(SELECTION_BG) } else { None }
                                } else {
                                    let raw = ansi_color_to_rgb(
                                        bg,
                                        &alacritty_terminal::term::color::Colors::default(),
                                    );
                                    if is_sel {
                                        Some(SELECTION_BG)
                                    } else {
                                        Some(raw)
                                    }
                                };
                                let is_b = flags.contains(Flags::BOLD);
                                let is_i = flags.contains(Flags::ITALIC);
                                let is_u = flags.contains(Flags::UNDERLINE);
                                let is_inv = flags.contains(Flags::INVERSE);

                                let new_run = ef != cur_fg
                                    || eb != cur_bg
                                    || is_b != cur_bold
                                    || is_i != cur_italic
                                    || is_u != cur_underline
                                    || is_inv != cur_inverse
                                    || is_sel != cur_sel;

                                if new_run {
                                    if !line_text.is_empty() {
                                        let rl = line_text.len() - run_start;
                                        if rl > 0 {
                                            runs.push(Self::make_run(
                                                rl,
                                                cur_bold,
                                                cur_italic,
                                                cur_fg,
                                                cur_bg,
                                                cur_inverse,
                                                cur_underline,
                                                cur_sel,
                                            ));
                                        }
                                        run_start = line_text.len();
                                    }
                                    cur_fg = ef;
                                    cur_bg = eb;
                                    cur_bold = is_b;
                                    cur_italic = is_i;
                                    cur_underline = is_u;
                                    cur_inverse = is_inv;
                                    cur_sel = is_sel;
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
                            if !line_text.is_empty() {
                                let rl = line_text.len() - run_start;
                                if rl > 0 {
                                    runs.push(Self::make_run(
                                        rl,
                                        cur_bold,
                                        cur_italic,
                                        cur_fg,
                                        cur_bg,
                                        cur_inverse,
                                        cur_underline,
                                        cur_sel,
                                    ));
                                }
                            }
                            if line_text.len() < num_cols {
                                let pad = num_cols - line_text.len();
                                line_text.extend(std::iter::repeat(' ').take(pad));
                                let pad_sel = sel_start.is_some()
                                    && line_text.len() - pad >= sel_start.unwrap()
                                    && line_text.len() - 1 < sel_end.unwrap_or(0);
                                runs.push(TextRun {
                                    len: pad,
                                    font: font("Menlo"),
                                    color: rgb(TERM_FG).into(),
                                    background_color: if pad_sel {
                                        Some(rgb(SELECTION_BG).into())
                                    } else {
                                        None
                                    },
                                    underline: None,
                                    strikethrough: None,
                                });
                            }
                            let whole = sel_start == Some(0) && sel_end == Some(num_cols);
                            window.paint_quad(fill(
                                Bounds::new(
                                    point(bounds.origin.x, y),
                                    size(bounds.size.width, line_height),
                                ),
                                if whole {
                                    rgb(SELECTION_BG)
                                } else {
                                    rgb(TERM_BG)
                                },
                            ));
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
                        // Cursor — only visible within the viewport
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
            // Transparent overlay for mouse events (selection + scroll)
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
                        let entity = entity.clone();
                        move |_event, _window, cx| {
                            #[cfg(debug_assertions)]
                            tracing::debug!("mouse-up");
                            if let Some(ref mut sel) = *selection.lock() {
                                sel.active = false;
                                let _ = entity.update(cx, |_, cx| cx.notify());
                            }
                        }
                    }),
            )
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
