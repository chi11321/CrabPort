use alacritty_terminal::vte::ansi::{Color, NamedColor};

pub(crate) const TERM_FG: u32 = 0xcdd6f4;
pub(crate) const TERM_BG: u32 = 0x1e1e2e;
pub(crate) const TERM_CURSOR: u32 = 0xf5e0dc;
pub(crate) const TERM_BLACK: u32 = 0x45475a;
pub(crate) const TERM_RED: u32 = 0xf38ba8;
pub(crate) const TERM_GREEN: u32 = 0xa6e3a1;
pub(crate) const TERM_YELLOW: u32 = 0xf9e2af;
pub(crate) const TERM_BLUE: u32 = 0x89b4fa;
pub(crate) const TERM_MAGENTA: u32 = 0xf5c2e7;
pub(crate) const TERM_CYAN: u32 = 0x94e2d5;
pub(crate) const TERM_WHITE: u32 = 0xbac2de;
pub(crate) const TERM_BRIGHT_BLACK: u32 = 0x585b70;
pub(crate) const TERM_BRIGHT_RED: u32 = 0xf38ba8;
pub(crate) const TERM_BRIGHT_GREEN: u32 = 0xa6e3a1;
pub(crate) const TERM_BRIGHT_YELLOW: u32 = 0xf9e2af;
pub(crate) const TERM_BRIGHT_BLUE: u32 = 0x89b4fa;
pub(crate) const TERM_BRIGHT_MAGENTA: u32 = 0xf5c2e7;
pub(crate) const TERM_BRIGHT_CYAN: u32 = 0x94e2d5;
pub(crate) const TERM_BRIGHT_WHITE: u32 = 0xa6adc8;

pub(crate) const SELECTION_BG: u32 = 0x585b70;

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
    match named {
        NamedColor::Foreground => TERM_FG,
        NamedColor::Background => TERM_BG,
        NamedColor::Cursor => TERM_CURSOR,
        NamedColor::Black => TERM_BLACK,
        NamedColor::Red => TERM_RED,
        NamedColor::Green => TERM_GREEN,
        NamedColor::Yellow => TERM_YELLOW,
        NamedColor::Blue => TERM_BLUE,
        NamedColor::Magenta => TERM_MAGENTA,
        NamedColor::Cyan => TERM_CYAN,
        NamedColor::White => TERM_WHITE,
        NamedColor::BrightBlack => TERM_BRIGHT_BLACK,
        NamedColor::BrightRed => TERM_BRIGHT_RED,
        NamedColor::BrightGreen => TERM_BRIGHT_GREEN,
        NamedColor::BrightYellow => TERM_BRIGHT_YELLOW,
        NamedColor::BrightBlue => TERM_BRIGHT_BLUE,
        NamedColor::BrightMagenta => TERM_BRIGHT_MAGENTA,
        NamedColor::BrightCyan => TERM_BRIGHT_CYAN,
        NamedColor::BrightWhite => TERM_BRIGHT_WHITE,
        NamedColor::DimBlack => TERM_BLACK,
        NamedColor::DimRed => TERM_RED,
        NamedColor::DimGreen => TERM_GREEN,
        NamedColor::DimYellow => TERM_YELLOW,
        NamedColor::DimBlue => TERM_BLUE,
        NamedColor::DimMagenta => TERM_MAGENTA,
        NamedColor::DimCyan => TERM_CYAN,
        NamedColor::DimWhite => TERM_WHITE,
        NamedColor::BrightForeground => TERM_FG,
        NamedColor::DimForeground => TERM_FG,
    }
}

pub(crate) fn indexed_color_to_rgb(
    idx: u8,
    _term_colors: &alacritty_terminal::term::color::Colors,
) -> u32 {
    match idx {
        0 => TERM_BLACK,
        1 => TERM_RED,
        2 => TERM_GREEN,
        3 => TERM_YELLOW,
        4 => TERM_BLUE,
        5 => TERM_MAGENTA,
        6 => TERM_CYAN,
        7 => TERM_WHITE,
        8 => TERM_BRIGHT_BLACK,
        9 => TERM_BRIGHT_RED,
        10 => TERM_BRIGHT_GREEN,
        11 => TERM_BRIGHT_YELLOW,
        12 => TERM_BRIGHT_BLUE,
        13 => TERM_BRIGHT_MAGENTA,
        14 => TERM_BRIGHT_CYAN,
        15 => TERM_BRIGHT_WHITE,
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
