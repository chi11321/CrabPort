//! Client-side window control buttons (about / settings / minimize /
//! maximize / close).
//!
//! Rendered in the top-right corner on Windows and Linux where the system
//! title bar is disabled. On macOS the native traffic-light buttons are used
//! instead, so [`WindowControls`] renders nothing.
//!
//! On Windows/Linux two extra auxiliary buttons (About, Settings) are
//! prepended to the left of the standard minimize/maximize/close trio, since
//! macOS gets those via the app menu bar but Windows/Linux has no such
//! chrome. They dispatch the same `OpenAbout` / `OpenSettings` actions the
//! macOS menu items and keybindings use, so the App-level `on_action`
//! handlers open the singleton windows.

use gpui::prelude::FluentBuilder;
use gpui::*;

use crate::color::*;
use crate::components::button::Button;
use crate::menus::{OpenAbout, OpenSettings};

/// Whether the platform uses client-side window controls.
/// macOS uses native traffic-light buttons, so we never render our own.
pub const HAS_CLIENT_CONTROLS: bool = cfg!(not(target_os = "macos"));

/// A row of minimize / maximize / close buttons.
///
/// Renders nothing on macOS (where the native traffic lights are used).
/// On Windows/Linux each button reuses the project [`Button`] component so it
/// inherits the same hover color transition as every other button in the app.
/// The close button overrides `bg_hover` to red, following the Windows / GNOME
/// convention.
///
/// `prefix` is used to namespace the button element ids so that multiple
/// windows (main + settings + about) don't share animation state in the
/// global transition registry.
///
/// When `aux_buttons` is set (main window only), two extra buttons (About,
/// Settings) are prepended to the left of minimize/maximize/close. Secondary
/// windows don't render them — they already *are* the About/Settings windows,
/// and macOS gets those via the app menu bar.
#[derive(IntoElement)]
pub struct WindowControls {
    prefix: &'static str,
    aux_buttons: bool,
}

impl WindowControls {
    /// Create a new `WindowControls` with the given id prefix.
    ///
    /// The prefix should be unique per window (e.g. `"main"`, `"settings"`,
    /// `"about"`) so the generated button ids (`"{prefix}-win-minimize"`,
    /// etc.) don't collide with controls in other windows.
    ///
    /// Defaults to `aux_buttons = false`. Use [`.with_aux_buttons(true)`](
    /// WindowControls::with_aux_buttons) on the main window to render the
    /// About / Settings launcher buttons.
    pub fn new(prefix: &'static str) -> Self {
        Self {
            prefix,
            aux_buttons: false,
        }
    }

    /// Enable the auxiliary About / Settings launcher buttons on the left of
    /// the minimize button. Intended for the main window only.
    pub fn with_aux_buttons(mut self, enable: bool) -> Self {
        self.aux_buttons = enable;
        self
    }
}

impl RenderOnce for WindowControls {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        if !HAS_CLIENT_CONTROLS {
            return div().into_any_element();
        }

        let prefix = self.prefix;
        let aux = self.aux_buttons;
        div()
            .flex()
            .items_center()
            .gap_1()
            .when(aux, |el| {
                el.child(render_control_button(
                    ElementId::Name(format!("{prefix}-win-about").into()),
                    "icons/info.svg",
                    None,
                    |w, cx| {
                        w.dispatch_action(Box::new(OpenAbout), cx);
                    },
                ))
                .child(render_control_button(
                    ElementId::Name(format!("{prefix}-win-settings").into()),
                    "icons/settings.svg",
                    None,
                    |w, cx| {
                        w.dispatch_action(Box::new(OpenSettings), cx);
                    },
                ))
            })
            .child(render_control_button(
                ElementId::Name(format!("{prefix}-win-minimize").into()),
                "icons/minus.svg",
                None,
                |w, _cx| {
                    w.minimize_window();
                },
            ))
            .child(render_control_button(
                ElementId::Name(format!("{prefix}-win-maximize").into()),
                "icons/square.svg",
                None,
                |w, _cx| {
                    toggle_maximize(w);
                },
            ))
            .child(render_control_button(
                ElementId::Name(format!("{prefix}-win-close").into()),
                "icons/close.svg",
                Some(0xE0_42_42), // red, matches Windows close-button hover
                |w, _cx| {
                    w.remove_window();
                },
            ))
            .into_any_element()
    }
}

/// Toggle the window between maximized and its pre-maximize (restored) state.
///
/// On Windows, GPUI's `Window::zoom_window()` unconditionally sends
/// `SW_MAXIMIZE`, so clicking the maximize button a second time does nothing
/// — the window stays maximized forever. To get proper toggle behavior we
/// call `ShowWindowAsync(hwnd, SW_RESTORE)` directly when the window is
/// already maximized, and fall back to `zoom_window()` (which maximizes) when
/// it isn't.
///
/// On Linux, GPUI's `zoom()` already toggles correctly (Wayland checks
/// `state.maximized`, X11 sends `_NET_WM_STATE_MAXIMIZED_*` with
/// `_NET_WM_STATE_TOGGLE`), so we can just delegate.
///
/// macOS uses native traffic lights and never reaches here (see
/// [`HAS_CLIENT_CONTROLS`]).
fn toggle_maximize(window: &mut Window) {
    #[cfg(target_os = "windows")]
    {
        use raw_window_handle::{HasWindowHandle, Win32WindowHandle};
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{SW_RESTORE, ShowWindowAsync};

        if window.is_maximized() {
            // Restore to the pre-maximize bounds.
            let raw = match window.window_handle() {
                Ok(h) => h,
                Err(_) => {
                    window.zoom_window();
                    return;
                }
            };
            let win32: Win32WindowHandle = match raw.as_raw() {
                raw_window_handle::RawWindowHandle::Win32(h) => h,
                _ => {
                    window.zoom_window();
                    return;
                }
            };
            // `Win32WindowHandle::hwnd` is a `NonZeroIsize`; cast it to the
            // pointer-sized `HWND` type expected by Win32.
            let hwnd = HWND(win32.hwnd.get() as *mut core::ffi::c_void);
            unsafe {
                let _ = ShowWindowAsync(hwnd, SW_RESTORE);
            }
        } else {
            window.zoom_window();
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        window.zoom_window();
    }
}

/// A single window-control button built on the project [`Button`] component.
///
/// `hover_color` — when `Some`, overrides the hover background to this color
/// (used for the close button which gets a red hover). When `None`, the
/// default `tab_btn_bg_hover` is used.
fn render_control_button(
    id: impl Into<ElementId>,
    icon_path: &'static str,
    hover_color: Option<u32>,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let mut btn = Button::new(id)
        .tab()
        .centered(true)
        .bg(0x0100_0000) // fully transparent — no fill until hover
        .child(svg().path(icon_path).size_4().text_color(rgb(text_muted())))
        .h_9()
        .w_9()
        .border_0()
        .px_0()
        .text_sm()
        .on_click(move |_e, w, cx| {
            on_click(w, cx);
            cx.stop_propagation();
        });
    if let Some(c) = hover_color {
        btn = btn.bg_hover(c);
    }
    btn
}
