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
                    None,
                    |w, cx| {
                        w.dispatch_action(Box::new(OpenAbout), cx);
                    },
                ))
                .child(render_control_button(
                    ElementId::Name(format!("{prefix}-win-settings").into()),
                    "icons/settings.svg",
                    None,
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
                Some(WindowControlArea::Min),
                |w, _cx| {
                    w.minimize_window();
                },
            ))
            .child(render_control_button(
                ElementId::Name(format!("{prefix}-win-maximize").into()),
                "icons/square.svg",
                None,
                Some(WindowControlArea::Max),
                |w, _cx| {
                    toggle_maximize(w);
                },
            ))
            .child(render_control_button(
                ElementId::Name(format!("{prefix}-win-close").into()),
                "icons/close.svg",
                Some(0xE0_42_42), // red, matches Windows close-button hover
                Some(WindowControlArea::Close),
                |w, _cx| {
                    w.remove_window();
                },
            ))
            .into_any_element()
    }
}

/// Toggle the window between maximized and its pre-maximize (restored) state.
///
/// This is the single cross-platform entry point used both by the maximize
/// window-control button (see [`render_control_button`]) and by the
/// double-click handler on drag regions (see
/// [`crate::layouts::tabbar::render_tab_bar`] and
/// [`crate::components::window_layout::render_sidebar_window`]).
///
/// Platform behavior:
/// - **macOS**: delegates to [`Window::titlebar_double_click`], which
///   respects the user's `AppleActionOnDoubleClick` system preference
///   (maximize or minimize by default). Native traffic lights are still
///   used for the actual maximize button, but this path covers the
///   double-click-on-title-bar interaction.
/// - **Windows**: GPUI's `Window::zoom_window()` unconditionally sends
///   `SW_MAXIMIZE`, so clicking the maximize button a second time does
///   nothing — the window stays maximized forever. To get proper toggle
///   behavior we call `ShowWindowAsync(hwnd, SW_RESTORE)` directly when the
///   window is already maximized, and fall back to `zoom_window()` (which
///   maximizes) when it isn't.
/// - **Linux**: GPUI's `zoom()` already toggles correctly (Wayland checks
///   `state.maximized`, X11 sends `_NET_WM_STATE_MAXIMIZED_*` with
///   `_NET_WM_STATE_TOGGLE`), so we just delegate.
pub(crate) fn toggle_maximize(window: &mut Window) {
    #[cfg(target_os = "macos")]
    {
        window.titlebar_double_click();
    }

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

    #[cfg(target_os = "linux")]
    {
        window.zoom_window();
    }
}

/// Begin a window drag from a pointer-down on a client-side drag region.
///
/// This is the single cross-platform entry point used by drag regions
/// (see [`crate::layouts::tabbar::render_tab_bar`] and
/// [`crate::components::window_layout::render_sidebar_window`]) on the
/// mouse-down event.
///
/// Platform behavior:
/// - **macOS**: no-op. macOS already provides window drag via the
///   transparent system title bar + `traffic_light_position`; client-side
///   drag regions are not needed there (and `Window::start_window_move`
///   is a no-op on macOS anyway).
/// - **Windows**: no-op at the app level. `window_control_area(Drag)` makes
///   `WM_NCHITTEST` return `HTCAPTION`, so Windows itself drives the drag
///   via `WM_NCLBUTTONDOWN` — calling `start_window_move` here would be
///   redundant (and GPUI's `start_window_move` is a no-op on Windows
///   anyway).
/// - **Linux (X11/Wayland)**: GPUI's `on_hit_test_window_control` is a
///   no-op, so `window_control_area(Drag)` alone does nothing. We must
///   explicitly call [`Window::start_window_move`], which fires
///   `_NET_WM_MOVERESIZE` (X11) / `xdg_toplevel._move` (Wayland) to let
///   the compositor take over the drag.
pub(crate) fn start_window_move(window: &mut Window) {
    #[cfg(target_os = "linux")]
    {
        window.start_window_move();
    }

    // macOS and Windows handle drag natively (see doc comment); no app-side
    // call needed.
    #[cfg(not(target_os = "linux"))]
    {
        let _ = window;
    }
}

/// A single window-control button built on the project [`Button`] component.
///
/// `hover_color` — when `Some`, overrides the hover background to this color
/// (used for the close button which gets a red hover). When `None`, the
/// default `tab_btn_bg_hover` is used.
///
/// `control_area` — when `Some`, registers the button's hitbox as a
/// platform window-control area (`Min` / `Max` / `Close`) so Windows'
/// `WM_NCHITTEST` returns the matching `HT*BUTTON` code and the OS handles
/// the NC click (auto-minimize/maximize-toggle/close, correct cursor).
/// On Linux this is currently a no-op (GPUI's `on_hit_test_window_control`
/// is empty for X11/Wayland), so the `on_click` handler is the *only* path
/// that actually runs the action there — both must be kept in sync.
/// The auxiliary About/Settings buttons pass `None` — they aren't native
/// window controls, they just dispatch an app action via `on_click`.
///
/// All buttons also call [`Button::occlude_mouse`] so their hitbox blocks
/// the drag-region hitbox beneath them — without this, the drag region
/// registered on the surrounding tab/title bar would shadow the buttons
/// in GPUI's `on_hit_test_window_control` callback (the drag hitbox is
/// pushed first because the parent paints before its children, so it would
/// otherwise win the hit-test and the buttons' clicks would never fire on
/// Windows).
fn render_control_button(
    id: impl Into<ElementId>,
    icon_path: &'static str,
    hover_color: Option<u32>,
    control_area: Option<WindowControlArea>,
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
        // Block the drag-region hitbox beneath this button so the
        // drag region doesn't win `WM_NCHITTEST` over the buttons.
        // Without this, clicking a control button on Windows would
        // start a window drag instead of firing the button's on_click.
        .occlude_mouse()
        .on_click(move |_e, w, cx| {
            on_click(w, cx);
            cx.stop_propagation();
        });
    if let Some(c) = hover_color {
        btn = btn.bg_hover(c);
    }
    if let Some(area) = control_area {
        btn = btn.window_control_area(area);
    }
    btn
}
