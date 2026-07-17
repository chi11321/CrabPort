use alacritty_terminal::term::cell::Flags;
use gpui::{TextRun, UnderlineStyle, px, rgb};

use crate::color::theme;
use crate::views::terminal::fonts::pick_font;
use crate::views::terminal::render_cache::CellSnap;

pub(crate) fn make_run(
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

pub(crate) fn build_runs(cells: &[CellSnap], num_cols: usize) -> (String, Vec<TextRun>) {
    let mut line_text = String::new();
    let mut runs: Vec<TextRun> = Vec::new();
    let mut run_start = 0usize;
    // Snapshot the theme once per line so the whole run list is consistent
    // even if `refresh_theme()` fires between cell lookups.
    let t = theme();
    let mut cur_fg = t.term_fg;
    let mut cur_inv_bg = t.term_bg;
    let mut cur_bold = false;
    let mut cur_italic = false;
    let mut cur_underline = false;
    let mut cur_inverse = false;

    for cell in cells.iter() {
        // Note: we intentionally do NOT skip `WIDE_CHAR_SPACER` /
        // `LEADING_WIDE_CHAR_SPACER` cells here. Those cells hold a space
        // and exist in the grid to mark the second column occupied by a
        // wide (CJK) glyph. Skipping them would collapse the shaped text
        // by one character per wide char, breaking the 1-glyph-per-cell
        // invariant that `force_width` (passed in the paint closure in
        // terminal.rs) relies on to place each glyph at
        // `index * cell_width`. With the spacer included, a wide char
        // (1 glyph at column N) plus its spacer (1 space glyph at column
        // N+1) maps cleanly: the wide glyph renders at its natural width
        // (~1.7-1.8x cell_width) starting at column N, fitting inside the
        // 2-cell slot, and the invisible space at column N+1 reserves
        // the slot so the following glyph lands at column N+2.
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
                runs.push(make_run(
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
            // Alacritty's `put_tab` stores a `\t` in the cell where the
            // tab started and advances the cursor to the next tab stop,
            // leaving the in-between cells as `' '` (their default). We
            // must NOT expand `\t` into `ns` spaces here — that would
            // double-count the in-between cells (which we'll visit in
            // subsequent iterations and push as spaces themselves),
            // inflating the line text and pushing later glyphs past their
            // grid slots under `force_width`.
            //
            // The visible "tab width" is just the count of blank cells
            // between the `\t` and the next non-blank cell, which falls
            // out naturally when each cell contributes exactly one char.
            // Pushing a single space (matching the cell's visual width of
            // one column) keeps `line_text.len() == num_cells_visited`,
            // so `force_width` places glyph N at `N * cell_width` exactly.
            line_text.push(' ');
        } else {
            line_text.push(cell.c);
        }
    }

    let rl = line_text.len() - run_start;
    if rl > 0 {
        runs.push(make_run(
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
        line_text.extend(std::iter::repeat_n(' ', pad));
        runs.push(TextRun {
            len: pad,
            font: pick_font(false, false),
            color: rgb(t.term_fg).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }

    (line_text, runs)
}
