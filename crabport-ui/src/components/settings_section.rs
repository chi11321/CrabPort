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

/// A single settings section: optional header + description + a stack of
/// rows, each one a title (optional) on the left and its control on the
/// right.
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
    /// Gap between the header, the description and the row list. Rows inside
    /// the list carry their own padding and hairline border instead.
    gap_section: gpui::DefiniteLength,
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

    /// Add a labelled field row. `label` is the row's title, shown on the
    /// left of `control`; the control sits against the section's right edge.
    pub fn field(
        mut self,
        label: impl Into<SharedString>,
        control: impl IntoElement + 'static,
    ) -> Self {
        self.fields
            .push((Some(label.into()), control.into_any_element()));
        self
    }

    /// Add a bare control row (no title). The control still sits against the
    /// section's right edge (a full-width control fills the row).
    pub fn bare(mut self, control: impl IntoElement + 'static) -> Self {
        self.fields.push((None, control.into_any_element()));
        self
    }
}

impl RenderOnce for Section {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        // Every item is one row: its title on the left, its control pushed to
        // the right edge, and a hairline border between rows (matching the
        // settings look Zed uses). Rows without a title are just the control,
        // still right-aligned.
        let row_count = self.fields.len();
        let rows: Vec<AnyElement> = self
            .fields
            .into_iter()
            .enumerate()
            .map(|(ix, (label, control))| {
                let has_label = label.is_some();
                let mut row = div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_4()
                    .py_2p5()
                    // Labelled rows split into title | control; an unlabelled
                    // row is just its control, against the same right edge
                    // (a full-width control simply fills the row).
                    .when(has_label, |el| el.justify_between())
                    .when(!has_label, |el| el.justify_end());
                match label {
                    Some(label) => {
                        row = row
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .text_color(rgb(text_primary()))
                                    .child(label),
                            )
                            // The control keeps its own width; the title side
                            // absorbs the slack (and truncates rather than
                            // pushing the control off the row).
                            .child(div().flex_shrink_0().child(control));
                    }
                    None => row = row.child(control),
                }
                let mut item = div().w_full().flex().flex_col().child(row);
                if ix + 1 < row_count {
                    item = item.child(div().w_full().h(px(1.)).flex_shrink_0().bg(rgb(border())));
                }
                item.into_any_element()
            })
            .collect();

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
            .when(!rows.is_empty(), |el| {
                el.child(div().w_full().flex().flex_col().children(rows))
            })
    }
}
