pub mod app;
pub mod app_state;
pub mod assets;
pub mod color;
pub mod components;
pub mod keybinds;
pub mod layouts;
pub mod menus;
pub mod theme;
pub mod views;
pub mod windows;

pub use app::CrabportApp;

rust_i18n::i18n!("i18n", fallback = "en");

/// Set the active locale for translations. Wraps `rust_i18n::set_locale` so
/// the binary crate doesn't need to depend on `rust_i18n` directly.
pub fn set_locale(locale: &str) {
    rust_i18n::set_locale(locale);
}

/// Reload the cached color theme from `config.toml` and force every open
/// window to repaint. Call this after mutating
/// `config.appearance.theme` so the new palette is visible immediately
/// across the main window and any auxiliary windows (Settings, About, …).
pub fn refresh_theme_with(cx: &mut gpui::App) {
    crate::color::refresh_theme();
    // `refresh_windows` queues a full repaint of every live window — each
    // repaint re-reads the `color::*()` accessors and picks up the new
    // palette without us having to enumerate window handles here.
    cx.refresh_windows();
}
