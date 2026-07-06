use std::sync::OnceLock;

use gpui::{Font, FontStyle, FontWeight, Pixels, font, px};
use parking_lot::Mutex;

use crabport_core::config;

/// Palette built once and reused for every cell of every frame.
pub(crate) fn palette() -> &'static alacritty_terminal::term::color::Colors {
    static P: OnceLock<alacritty_terminal::term::color::Colors> = OnceLock::new();
    P.get_or_init(alacritty_terminal::term::color::Colors::default)
}

/// Pre-built font variants for a given family, cloned cheaply per run.
/// We cache exactly one `Fonts` set at a time — the currently-configured
/// family — and rebuild it whenever the configured family changes. This
/// keeps `pick_font` (called once per run, every frame) off the hot path
/// while still adapting to Settings-window changes on the next render.
struct Fonts {
    family: String,
    regular: Font,
    bold: Font,
    italic: Font,
    bold_italic: Font,
}

static FONTS: OnceLock<Mutex<Fonts>> = OnceLock::new();

fn fonts_lock() -> &'static Mutex<Fonts> {
    FONTS.get_or_init(|| {
        Mutex::new(build_fonts(
            config::snapshot()
                .appearance
                .terminal
                .effective_font_family(),
        ))
    })
}

/// Build the four font variants (regular/bold/italic/bold_italic) for a
/// given family name.
fn build_fonts(family: &str) -> Fonts {
    let family_owned: gpui::SharedString = family.to_string().into();
    let base = font(family_owned);
    let mut italic = base.clone();
    italic.style = FontStyle::Italic;
    let mut bold_italic = base.clone();
    bold_italic.weight = FontWeight::BOLD;
    bold_italic.style = FontStyle::Italic;
    Fonts {
        family: family.to_string(),
        regular: base.clone(),
        bold: base.bold(),
        italic,
        bold_italic,
    }
}

/// Returns the configured monospace font family name for the terminal.
///
/// Reads the live config on every call so a Settings-window change is
/// picked up immediately on the next render. An empty configured value
/// means "use the platform-native default" (`Menlo` / `Consolas` / …).
pub(crate) fn font_family() -> String {
    config::snapshot()
        .appearance
        .terminal
        .effective_font_family()
        .to_string()
}

/// Return the cached `Fonts` for the current config, rebuilding the cache
/// when the configured family changes.
fn current_fonts() -> Fonts {
    let family = font_family();
    let lock = fonts_lock();
    {
        let guard = lock.lock();
        if guard.family == family {
            // Cheap clone: `Font` is a small struct of `SharedString` + enums.
            return Fonts {
                family: guard.family.clone(),
                regular: guard.regular.clone(),
                bold: guard.bold.clone(),
                italic: guard.italic.clone(),
                bold_italic: guard.bold_italic.clone(),
            };
        }
    }
    // Family changed — rebuild under the write lock.
    let mut guard = lock.lock();
    *guard = build_fonts(&family);
    Fonts {
        family: guard.family.clone(),
        regular: guard.regular.clone(),
        bold: guard.bold.clone(),
        italic: guard.italic.clone(),
        bold_italic: guard.bold_italic.clone(),
    }
}

pub(crate) fn pick_font(bold: bool, italic: bool) -> Font {
    let f = current_fonts();
    match (bold, italic) {
        (false, false) => f.regular,
        (true, false) => f.bold,
        (false, true) => f.italic,
        (true, true) => f.bold_italic,
    }
}

// ---------------------------------------------------------------------------
// Metrics: derive cell_width + line_height from font + size
// ---------------------------------------------------------------------------

/// Computed terminal cell metrics for a given font family + size.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TerminalMetrics {
    pub font_size: Pixels,
    pub line_height: Pixels,
    pub cell_width: Pixels,
}

impl TerminalMetrics {
    /// Compute metrics for the current config (family + size). Falls back to
    /// the legacy hardcoded values if the font can't be measured.
    ///
    /// `line_height` = `font_size * 1.5` (matches the original 13 → 20 ratio).
    /// `cell_width` is the advance width of the ASCII glyph `'M'` in the
    /// configured font at the configured size — measured via the text system
    /// so changing either the family or the size updates the grid cleanly.
    /// When measurement fails (e.g. the configured family isn't installed),
    /// we fall back to a platform-specific default ratio.
    pub fn from_config(cx: &gpui::App) -> Self {
        let family = font_family();
        let size = config::snapshot().appearance.terminal.effective_font_size();
        let font_size = px(size);
        let line_height = px((size * 1.5).round());

        let cell_width = measure_cell_width(cx, &family, font_size).unwrap_or_else(|| {
            // Fallback: a roughly 0.6× ratio matches Menlo/Consolas. Keeps
            // the grid usable even when font metrics can't be queried.
            px((size * 0.6).round().max(4.0))
        });

        TerminalMetrics {
            font_size,
            line_height,
            cell_width,
        }
    }
}

/// Measure the advance width of a single ASCII glyph in the given font +
/// size. Returns `None` if the text system can't shape the sample (e.g. the
/// family isn't installed and no fallback matched).
fn measure_cell_width(cx: &gpui::App, family: &str, font_size: Pixels) -> Option<Pixels> {
    let text_system = cx.text_system();
    // Use 'M' — a wide-ish ASCII glyph that matches the cell-grid intent.
    // `resolve_font` + `advance` live on `TextSystem` (reachable via
    // `cx.text_system()`, which returns `Arc<TextSystem>`), so this works
    // from any `&App` without needing a window handle — unlike
    // `shape_line`/`layout_line`, which are on `WindowTextSystem`.
    let family_owned: gpui::SharedString = family.to_string().into();
    let font = font(family_owned);
    let font_id = text_system.resolve_font(&font);
    let advance = text_system.advance(font_id, font_size, 'M').ok()?;
    let w = advance.width;
    if w > px(0.0) { Some(w) } else { None }
}
