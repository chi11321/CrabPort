//! Reusable drop-zone overlay with eased fade-in/out.
//!
//! Designed to be rendered as a child of any container that accepts
//! external file drops (the SFTP panel, a future standalone SFTP tab, …).
//! When `active` is true the overlay covers the parent with a translucent
//! tint and a centered icon + label, signaling that a drop will upload.
//!
//! The overlay uses `transition_when_else` on both opacity and a subtle
//! scale so the appearance/disappearance is eased, matching the rest of
//! the app's animation language.
//!
//! ## Usage
//!
//! ```ignore
//! div()
//!     .on_drop::<ExternalPaths>(...)
//!     .on_drag_move::<ExternalPaths>(...)
//!     .child(DropZoneOverlay::new(view.drag_over)
//!         .hint(t!("sftp.drop_upload_hint").to_string()))
//! ```

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::TransitionExt;

use crate::color::*;
use crate::motion::{EASE_STANDARD, RADIUS_MD, duration_base};

/// A translucent overlay shown over a drop target while external files
/// are being dragged over it. Fades in/out with a 150ms ease.
#[derive(IntoElement)]
pub struct DropZoneOverlay {
    /// Whether the overlay is currently visible (files are being dragged
    /// over the parent container).
    active: bool,
    /// The hint text shown in the center (e.g. "Drop files to upload").
    hint: SharedString,
    /// Optional icon path (e.g. "icons/upload.svg"). Defaults to a cloud
    /// upload icon.
    icon: SharedString,
    /// Unique id for the transition. Callers that mount multiple overlays
    /// simultaneously should pass distinct ids to avoid animation state
    /// collisions.
    id: ElementId,
}

impl DropZoneOverlay {
    pub fn new(active: bool) -> Self {
        Self {
            active,
            hint: "".into(),
            icon: "icons/upload.svg".into(),
            id: ElementId::Name("drop-zone-overlay".into()),
        }
    }

    /// Set the hint text shown in the center of the overlay.
    pub fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.hint = hint.into();
        self
    }

    /// Override the icon path.
    pub fn icon(mut self, icon: impl Into<SharedString>) -> Self {
        self.icon = icon.into();
        self
    }

    /// Override the transition id (use when multiple overlays coexist).
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = id.into();
        self
    }
}

impl RenderOnce for DropZoneOverlay {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let active = self.active;
        let hint = self.hint.clone();
        let icon = self.icon.clone();

        div()
            .id(self.id.clone())
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            // Pointer-events: none — let the drop event pass through to
            // the parent's on_drop handler. The overlay is purely visual.
            .bg(rgba(0x00000000))
            .opacity(0.0)
            .when(active, |el| {
                el.border_2()
                    .border_color(rgb(btn_primary_bg()))
                    .rounded(RADIUS_MD)
            })
            .with_transition(self.id)
            .transition_when_else(
                active,
                duration_base(),
                EASE_STANDARD,
                |el| el.opacity(1.0).bg(rgba((btn_primary_bg() << 8) | 0x20)),
                |el| el.opacity(0.0).bg(rgba(0x00000000)),
            )
            .when(active, |el| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_3()
                        .child(
                            svg()
                                .path(icon)
                                .size(px(32.0))
                                .text_color(rgb(btn_primary_bg())),
                        )
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(text_primary()))
                                .child(hint),
                        ),
                )
            })
    }
}
