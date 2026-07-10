use alacritty_terminal::{
    grid::Dimensions,
    index::{Column, Line},
    term::cell::{Cell, Flags},
};
use gpui::{Bounds, Pixels, Point, px};

/// Terminal selection.
///
/// Rows are stored as **alacritty grid absolute line indices** (not viewport
/// rows). This means the selection stays anchored to the text content as the
/// viewport scrolls, instead of sliding with the viewport.
///
/// Conversion: viewport_row = grid_line + display_offset
#[derive(Clone, Debug)]
pub(crate) struct Selection {
    pub(crate) active: bool,
    pub(crate) start_col: usize,
    pub(crate) start_row: i32,
    pub(crate) end_col: usize,
    pub(crate) end_row: i32,
}

impl Selection {
    pub(crate) fn new(col: usize, row: i32) -> Self {
        Self {
            active: true,
            start_col: col,
            start_row: row,
            end_col: col,
            end_row: row,
        }
    }

    /// Whether the selection is visually a no-op (nothing to highlight).
    ///
    /// For an in-progress drag selection (`active == true`), a single-cell
    /// span means the user clicked but hasn't dragged yet — we hide the
    /// highlight so a plain click doesn't visually select a cell.
    ///
    /// Word/line selections from double/triple click are `active == false`
    /// and always count as non-empty (they may legitimately span one cell).
    pub(crate) fn is_empty(&self) -> bool {
        self.active && self.start_row == self.end_row && self.start_col == self.end_col
    }

    /// Returns (start_row, end_row, start_col, end_col) in grid coordinates,
    /// normalized so start <= end.
    pub(crate) fn range(&self) -> (i32, i32, usize, usize) {
        if self.start_row < self.end_row {
            (self.start_row, self.end_row, self.start_col, self.end_col)
        } else if self.start_row > self.end_row {
            (self.end_row, self.start_row, self.end_col, self.start_col)
        } else {
            let (lo, hi) = if self.start_col <= self.end_col {
                (self.start_col, self.end_col)
            } else {
                (self.end_col, self.start_col)
            };
            (self.start_row, self.end_row, lo, hi)
        }
    }
}

/// Semantic classification of a single grid cell for word-boundary
/// detection. This mirrors the classic terminal (and Alacritty) approach:
/// characters are grouped into classes so that a "word" consists of adjacent
/// characters of the same class, and whitespace acts as a separator.
#[derive(PartialEq, Eq, Clone, Copy)]
enum CharClass {
    Whitespace,
    Word,  // alphanumeric, underscore
    Punct, // other printable, non-word characters
}

/// Determine the [`CharClass`] of a cell.
///
/// `WRAPLINE`/`WIDE_CHAR_SPACER`/`LEADING_WIDE_CHAR_SPACER` cells are treated
/// as whitespace-like separators so word selection does not cross line wraps
/// or wide-char spacers.
fn classify_cell(c: char, flags: Flags) -> CharClass {
    if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
        return CharClass::Whitespace;
    }
    if c == ' ' || c == '\t' || c == '\u{00a0}' {
        return CharClass::Whitespace;
    }
    if c.is_alphanumeric() || c == '_' {
        return CharClass::Word;
    }
    CharClass::Punct
}

/// Build a selection spanning the word at `(col, row)` in the given grid.
///
/// "Word" here means a maximal run of cells with the same [`CharClass`] as
/// the cell at `(col, row)`, *excluding* leading/trailing whitespace. If the
/// clicked cell is whitespace, the word is just that single cell (you can't
/// drag-select starting from a space).
///
/// Returns `None` if the grid lookup fails (out of bounds).
pub(crate) fn select_word(
    grid: &alacritty_terminal::grid::Grid<Cell>,
    num_cols: usize,
    col: usize,
    row: i32,
) -> Option<Selection> {
    let li = Line(row);
    if row < grid.topmost_line().0 || row > grid.bottommost_line().0 {
        return None;
    }
    let col = col.min(num_cols.saturating_sub(1));
    let cell = &grid[li][Column(col)];
    let class = classify_cell(cell.c, cell.flags);

    // Find the left boundary: scan backwards while the cell shares the same
    // class, stopping at column 0 or a class change.
    let start_col = {
        let mut c = col;
        while c > 0 {
            let prev = &grid[li][Column(c - 1)];
            if classify_cell(prev.c, prev.flags) != class {
                break;
            }
            c -= 1;
        }
        c
    };

    // Find the right boundary: scan forwards while the cell shares the same
    // class, stopping at the last column or a class change.
    let end_col = {
        let mut c = col;
        while c + 1 < num_cols {
            let next = &grid[li][Column(c + 1)];
            if classify_cell(next.c, next.flags) != class {
                break;
            }
            c += 1;
        }
        c
    };

    Some(Selection {
        active: false,
        start_col,
        start_row: row,
        end_col,
        end_row: row,
    })
}

/// Build a selection spanning the entire line at `row`.
///
/// The selection covers columns 0..num_cols-1 (inclusive). Used for triple-
/// click line selection.
pub(crate) fn select_line(num_cols: usize, row: i32) -> Selection {
    let last = num_cols.saturating_sub(1);
    Selection {
        active: false,
        start_col: 0,
        start_row: row,
        end_col: last,
        end_row: row,
    }
}

/// Convert a mouse position to a **grid absolute line** + viewport column.
///
/// `viewport_row` is the visible row (0 = top of viewport).
/// The grid line is `viewport_row - display_offset` (matching alacritty's
/// `Line(row as i32 - offset as i32)` indexing used in prepaint).
pub(crate) fn mouse_to_grid(
    pos: Point<Pixels>,
    bounds: Bounds<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
    display_offset: i32,
) -> Option<(usize, i32)> {
    let local_x = pos.x - bounds.origin.x;
    let local_y = pos.y - bounds.origin.y;
    if local_x < px(0.0) || local_y < px(0.0) {
        return None;
    }
    let col = ((local_x / cell_width) as f32).floor() as usize;
    let viewport_row = ((local_y / line_height) as f32).floor() as i32;
    // Convert viewport row to grid line.
    let grid_line = viewport_row - display_offset;
    Some((col.min(999), grid_line))
}
