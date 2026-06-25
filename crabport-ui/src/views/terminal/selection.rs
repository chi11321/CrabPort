use gpui::{Bounds, Pixels, Point, px};

#[derive(Clone, Debug)]
pub(crate) struct Selection {
    pub(crate) active: bool,
    pub(crate) start_col: usize,
    pub(crate) start_row: usize,
    pub(crate) end_col: usize,
    pub(crate) end_row: usize,
}

impl Selection {
    pub(crate) fn new(col: usize, row: usize) -> Self {
        Self {
            active: true,
            start_col: col,
            start_row: row,
            end_col: col,
            end_row: row,
        }
    }

    pub(crate) fn range(&self) -> (usize, usize, usize, usize) {
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

pub(crate) fn mouse_to_grid(
    pos: Point<Pixels>,
    bounds: Bounds<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
) -> Option<(usize, usize)> {
    let local_x = pos.x - bounds.origin.x;
    let local_y = pos.y - bounds.origin.y;
    if local_x < px(0.0) || local_y < px(0.0) {
        return None;
    }
    let col = ((local_x / cell_width) as f32).floor() as usize;
    let row = ((local_y / line_height) as f32).floor() as usize;
    Some((col.min(999), row.min(999)))
}
