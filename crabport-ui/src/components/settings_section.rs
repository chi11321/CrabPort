//! Declarative settings-section builder.
//!
//! The Settings window renders repeated blocks of the same shape:
//!
//! ```text
//! ┌ section_header ─────────────┐
//! section_desc (optional)
//! ┌ field: label ─── control ──┐
//! │ field: label ─── control ──│
//! └────────────────────────────┘
//! ```
//!
//! [`Section`] lets the caller declare this structure with a fluent API,
//! removing the deeply-nested `div().child(div().child(div(...)))` chains
//! that were repeated for every section in `render_appearance_pane`.

use gpui::prelude::FluentBuilder;
use gpui::*;

use crate::color::*;

/// A single settings section: optional header + description + a vertical
/// stack of fields.
///
/// Built with a builder API:
///
/// ```ignore
/// Section::new()
///     .header(t!("..."))
///     .desc(t!("..."))
///     .field("Font family", div().w(px(240.)).child(dropdown))
///     .field("Font size", div().w(px(180.)).child(stepper))
/// ```
#[derive(IntoElement)]
pub struct Section {
    header: Option<SharedString>,
    desc: Option<SharedString>,
    fields: Vec<(Option<SharedString>, AnyElement)>,
    gap_section: gpui::DefiniteLength,
    gap_field: gpui::DefiniteLength,
}

impl Default for Section {
    fn default() -> Self {
        Self::new()
    }
}

impl Section {
    pub fn new() -> Self {
        Self {
            header: None,
            desc: None,
            fields: Vec::new(),
            gap_section: px(12.0).into(),
            gap_field: px(4.0).into(),
        }
    }

    /// Section title (bold, `text_sm`).
    pub fn header(mut self, text: impl Into<SharedString>) -> Self {
        self.header = Some(text.into());
        self
    }

    /// Muted description below the header.
    pub fn desc(mut self, text: impl Into<SharedString>) -> Self {
        self.desc = Some(text.into());
        self
    }

    /// Add a labelled field row. `label` is shown above `control` in a
    /// `text_xs` muted style.
    pub fn field(
        mut self,
        label: impl Into<SharedString>,
        control: impl IntoElement + 'static,
    ) -> Self {
        self.fields
            .push((Some(label.into()), control.into_any_element()));
        self
    }

    /// Add a bare control row (no label).
    pub fn bare(mut self, control: impl IntoElement + 'static) -> Self {
        self.fields.push((None, control.into_any_element()));
        self
    }
}

impl RenderOnce for Section {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(self.gap_section)
            .when_some(self.header, |el, header| {
                el.child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(text_primary()))
                        .child(header),
                )
            })
            .when_some(self.desc, |el, desc| {
                el.child(div().text_xs().text_color(rgb(text_muted())).child(desc))
            })
            .children(self.fields.into_iter().map(|(label, control)| {
                let mut row = div().flex().flex_col().gap(self.gap_field);
                if let Some(label) = label {
                    row = row.child(div().text_xs().text_color(rgb(text_muted())).child(label));
                }
                row.child(control)
            }))
    }
}
