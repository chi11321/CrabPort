#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use crabport_ui::app::{
    TerminalDecreaseFont, TerminalIncreaseFont, TerminalResetFont, TerminalShiftTab, TerminalTab,
    open_main_window,
};
use crabport_ui::app_state::AppState;
use crabport_ui::assets::CrabportAssets;
use crabport_ui::menus::{Hide, Minimize, OpenAbout, OpenSettings, Quit, Zoom};
use crabport_ui::windows::AuxWindowKind;
use gpui::*;

/// Work around a crash on WSL2.
///
/// WSLg ships a Weston compositor that advertises `xdg_wm_base` at a
/// version below 2. gpui 0.2.2 binds that global with `globals.bind(&qh,
/// 2..=5, ()).unwrap()` (see `platform/linux/wayland/client.rs:151`), so the
/// `UnsupportedVersion` error turns into a panic before any window opens.
///
/// gpui's `guess_compositor` picks Wayland whenever `WAYLAND_DISPLAY` is set
/// and non-empty, with no fallback to X11 on bind failure. Dropping the
/// variable forces the X11 path, which works fine under WSLg.
#[cfg(target_os = "linux")]
fn workaround_wsl2_wayland_version() {
    let is_wsl2 = std::fs::read_to_string("/proc/version")
        .is_ok_and(|v| v.contains("microsoft-standard-WSL2"));
    if is_wsl2 {
        std::env::remove_var("WAYLAND_DISPLAY");
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    workaround_wsl2_wayland_version();

    #[cfg(debug_assertions)]
    crabport_core::log::init();

    Application::new()
        .with_assets(CrabportAssets::new())
        .run(|cx| {
            gpui_component::init(cx);

            // Force dark theme regardless of system appearance.
            gpui_component::theme::Theme::change(gpui_component::theme::ThemeMode::Dark, None, cx);

            // Set the active locale early so the menu bar (built below) and
            // every window picks up the right translations. Read from the
            // persisted config.toml so the user's language choice survives
            // app restarts.
            let locale = crabport_core::config::snapshot().appearance.locale;
            crabport_ui::set_locale(&locale);

            cx.bind_keys([
                KeyBinding::new("tab", TerminalTab, Some("CrabPortTerminal")),
                KeyBinding::new("shift-tab", TerminalShiftTab, Some("CrabPortTerminal")),
                // Terminal font zoom shortcuts. macOS uses cmd; other
                // platforms use ctrl. The actions clamp the persisted size
                // into [8, 32] and re-derive cell metrics.
                #[cfg(target_os = "macos")]
                KeyBinding::new("cmd-=", TerminalIncreaseFont, Some("CrabPortTerminal")),
                #[cfg(target_os = "macos")]
                KeyBinding::new("cmd--", TerminalDecreaseFont, Some("CrabPortTerminal")),
                #[cfg(target_os = "macos")]
                KeyBinding::new("cmd-0", TerminalResetFont, Some("CrabPortTerminal")),
                #[cfg(not(target_os = "macos"))]
                KeyBinding::new("ctrl-=", TerminalIncreaseFont, Some("CrabPortTerminal")),
                #[cfg(not(target_os = "macos"))]
                KeyBinding::new("ctrl--", TerminalDecreaseFont, Some("CrabPortTerminal")),
                #[cfg(not(target_os = "macos"))]
                KeyBinding::new("ctrl-0", TerminalResetFont, Some("CrabPortTerminal")),
                // macOS-standard shortcuts for the app menu items.
                KeyBinding::new("cmd-q", Quit, None),
                KeyBinding::new("cmd-h", Hide, None),
                KeyBinding::new("cmd-,", OpenSettings, None),
                KeyBinding::new("cmd-m", Minimize, None),
            ]);

            // Initialize process-wide shared state (store, window registry)
            // before opening any window. `CrabportApp::new` reads from this
            // global, so it must be ready first.
            AppState::init(cx);

            // Global actions for opening secondary windows. These are app-
            // level (no window context required) so they work from any
            // focused window.
            cx.on_action::<OpenSettings>(|_a, cx| {
                AppState::focus_or_open(AuxWindowKind::Settings, cx);
            });
            cx.on_action::<OpenAbout>(|_a, cx| {
                AppState::focus_or_open(AuxWindowKind::About, cx);
            });

            // Menu-bar actions backed by App-level platform calls.
            cx.on_action::<Quit>(|_a, cx| cx.quit());
            cx.on_action::<Hide>(|_a, cx| cx.hide());

            // Window menu: act on the currently-focused window. Menu actions
            // dispatch globally, so we resolve the active window handle here
            // and run the platform call inside its window context.
            cx.on_action::<Minimize>(|_a, cx| {
                if let Some(handle) = cx.active_window() {
                    let _ = handle.update(cx, |_, window, _cx| window.minimize_window());
                }
            });
            cx.on_action::<Zoom>(|_a, cx| {
                if let Some(handle) = cx.active_window() {
                    let _ = handle.update(cx, |_, window, _cx| window.zoom_window());
                }
            });

            // Install the macOS menu bar. On non-macOS platforms this is a
            // no-op / ignored, but the call is harmless.
            cx.set_menus(crabport_ui::menus::app_menus());

            open_main_window(cx);
        });
}
