//! Client-side window control buttons (minimize / maximize / close).
//!
//! Rendered in the top-right corner on Windows and Linux where the system
//! title bar is disabled. On macOS the native traffic-light buttons are used
//! instead, so [`WindowControls`] renders nothing.

use gpui::*;

use crate::color::*;
use crate::components::button::Button;

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
#[derive(IntoElement)]
pub struct WindowControls {
    prefix: &'static str,
}

impl WindowControls {
    /// Create a new `WindowControls` with the given id prefix.
    ///
    /// The prefix should be unique per window (e.g. `"main"`, `"settings"`,
    /// `"about"`) so the generated button ids (`"{prefix}-win-minimize"`,
    /// etc.) don't collide with controls in other windows.
    pub fn new(prefix: &'static str) -> Self {
        Self { prefix }
    }
}

impl RenderOnce for WindowControls {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        if !HAS_CLIENT_CONTROLS {
            return div().into_any_element();
        }

        let prefix = self.prefix;
        div()
            .flex()
            .items_center()
            .gap_1()
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
                    w.zoom_window();
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
