//! Secondary window types and the registry that manages them.
//!
//! The main terminal window is constructed directly in `main.rs` (it owns the
//! heavy `CrabportApp` state). Auxiliary windows — Settings, About — are
//! lighter views opened on demand from anywhere in the app via
//! [`focus_or_open`].
//!
//! ## Singleton policy
//!
//! Each `AuxWindowKind` is treated as a singleton: calling `focus_or_open`
//! when a window of that kind already exists brings it to the front rather
//! than spawning a duplicate. The registry tracks open windows by kind in
//! `WindowRegistry` (stored as a GPUI global).

pub mod about;
pub mod registry;
pub mod settings;
pub mod theme_editor;

pub use about::AboutWindow;
pub use registry::{AuxWindowKind, WindowRegistry, focus_or_open};
pub use settings::SettingsWindow;
pub use theme_editor::ThemeEditorWindow;

use gpui::*;

/// Shared [`WindowOptions`] for the auxiliary singleton windows (Settings /
/// Theme Editor / About): centered at `bounds_size`, transparent titlebar
/// (required on Windows as well as macOS to actually strip the system title
/// bar; the `title` is kept everywhere so the taskbar / window switcher /
/// Exposé show a real name), blurred background + traffic-light offset on
/// macOS, client decorations on Linux. See `app::open_main_window` for the
/// per-platform titlebar rationale.
pub(crate) fn aux_window_options(
    title: SharedString,
    bounds_size: Size<Pixels>,
    min_size: Size<Pixels>,
    cx: &mut App,
) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::centered(bounds_size, cx)),
        titlebar: Some(TitlebarOptions {
            title: Some(title),
            appears_transparent: true,
            #[cfg(target_os = "macos")]
            traffic_light_position: Some(point(px(12.0), px(14.0))),
            ..Default::default()
        }),
        #[cfg(target_os = "macos")]
        window_background: WindowBackgroundAppearance::Blurred,
        #[cfg(target_os = "linux")]
        window_decorations: Some(WindowDecorations::Client),
        window_min_size: Some(min_size),
        ..Default::default()
    }
}
