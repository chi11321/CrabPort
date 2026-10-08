use alacritty_terminal::vte::ansi::{Color, NamedColor};

use crate::color::theme;

/// Dim (a.k.a. faint) intensity multiplier.  A value of 0.6 approximates the
/// brightness reduction applied by most terminals for SGR 2.
const DIM_FACTOR: f32 = 0.6;

pub(crate) fn dim_color(rgb: u32) -> u32 {
    let r = ((rgb >> 16) & 0xFF) as f32;
    let g = ((rgb >> 8) & 0xFF) as f32;
    let b = (rgb & 0xFF) as f32;
    let dim = |c: f32| (c * DIM_FACTOR) as u32;
    (dim(r) << 16) | (dim(g) << 8) | dim(b)
}

pub(crate) fn ansi_color_to_rgb(
    color: &Color,
    term_colors: &alacritty_terminal::term::color::Colors,
) -> u32 {
    match color {
        Color::Named(named) => named_color_to_rgb(*named, term_colors),
        Color::Spec(rgb) => ((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | (rgb.b as u32),
        Color::Indexed(idx) => indexed_color_to_rgb(*idx, term_colors),
    }
}

pub(crate) fn named_color_to_rgb(
    named: NamedColor,
    _term_colors: &alacritty_terminal::term::color::Colors,
) -> u32 {
    // Snapshot the theme once per call so every branch sees the same palette
    // (a `refresh_theme()` mid-render can't tear a single color lookup).
    let t = theme();
    match named {
        NamedColor::Foreground => t.term_fg,
        NamedColor::Background => t.term_bg,
        NamedColor::Cursor => t.term_cursor,
        NamedColor::Black => t.term_black,
        NamedColor::Red => t.term_red,
        NamedColor::Green => t.term_green,
        NamedColor::Yellow => t.term_yellow,
        NamedColor::Blue => t.term_blue,
        NamedColor::Magenta => t.term_magenta,
        NamedColor::Cyan => t.term_cyan,
        NamedColor::White => t.term_white,
        NamedColor::BrightBlack => t.term_bright_black,
        NamedColor::BrightRed => t.term_bright_red,
        NamedColor::BrightGreen => t.term_bright_green,
        NamedColor::BrightYellow => t.term_bright_yellow,
        NamedColor::BrightBlue => t.term_bright_blue,
        NamedColor::BrightMagenta => t.term_bright_magenta,
        NamedColor::BrightCyan => t.term_bright_cyan,
        NamedColor::BrightWhite => t.term_bright_white,
        NamedColor::DimBlack => t.term_black,
        NamedColor::DimRed => t.term_red,
        NamedColor::DimGreen => t.term_green,
        NamedColor::DimYellow => t.term_yellow,
        NamedColor::DimBlue => t.term_blue,
        NamedColor::DimMagenta => t.term_magenta,
        NamedColor::DimCyan => t.term_cyan,
        NamedColor::DimWhite => t.term_white,
        NamedColor::BrightForeground => t.term_fg,
        NamedColor::DimForeground => t.term_fg,
    }
}

pub(crate) fn indexed_color_to_rgb(
    idx: u8,
    _term_colors: &alacritty_terminal::term::color::Colors,
) -> u32 {
    let t = theme();
    match idx {
        0 => t.term_black,
        1 => t.term_red,
        2 => t.term_green,
        3 => t.term_yellow,
        4 => t.term_blue,
        5 => t.term_magenta,
        6 => t.term_cyan,
        7 => t.term_white,
        8 => t.term_bright_black,
        9 => t.term_bright_red,
        10 => t.term_bright_green,
        11 => t.term_bright_yellow,
        12 => t.term_bright_blue,
        13 => t.term_bright_magenta,
        14 => t.term_bright_cyan,
        15 => t.term_bright_white,
        16..=231 => {
            let idx = idx - 16;
            let r = if idx / 36 > 0 {
                (idx / 36 - 1) * 40 + 55
            } else {
                0
            };
            let g = if (idx % 36) / 6 > 0 {
                ((idx % 36) / 6 - 1) * 40 + 55
            } else {
                0
            };
            let b = if idx % 6 > 0 {
                (idx % 6 - 1) * 40 + 55
            } else {
                0
            };
            (r as u32) << 16 | (g as u32) << 8 | (b as u32)
        }
        232..=255 => {
            let v = (idx - 232) as u32 * 10 + 8;
            v << 16 | v << 8 | v
        }
    }
}
