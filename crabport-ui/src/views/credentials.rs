use gpui::{prelude::FluentBuilder, *};
use gpui_animation::{animation::TransitionExt, transition::general::Linear};
use gpui_component::scroll::ScrollableElement as _;
use rust_i18n::t;
use std::time::Duration;

use crate::color::*;
use crate::components::button::Button;
use crate::layouts::credential_form::{CredentialFormState, CredentialFormView};
use crabport_core::credential::CredentialEntry;

/// Render the credentials sidebar view.
pub fn render_credentials_view(
    credentials: &[CredentialEntry],
    form_state: Option<&CredentialFormState>,
    on_new: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    // Hide anonymous credentials (auto-created from connection form)
    let visible: Vec<&CredentialEntry> = credentials.iter().filter(|c| !c.anonymous).collect();
    div()
        .size_full()
        .flex()
        .flex_col()
        .relative()
        // --- Header: title + New button ---
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .px_4()
                .pt_4()
                .pb_2()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(TEXT_PRIMARY))
                        .child(t!("sidebar.credentials").to_string()),
                )
                .child(
                    Button::new("creds-new-btn")
                        .primary()
                        .icon("icons/plus.svg")
                        .w_auto()
                        .px_2()
                        .child(t!("credentials.new_button").to_string())
                        .on_click(move |_e, w, cx| {
                            on_new(w, cx);
                        }),
                ),
        )
        // --- Separator ---
        .child(div().h_px().bg(rgb(BORDER)).mx_4())
        // --- Credentials list (or empty state) ---
        .child(
            div()
                .flex_1()
                .overflow_y_scrollbar()
                .px_4()
                .py_2()
                .when_else(
                    visible.is_empty(),
                    |el| {
                        el.flex().items_center().justify_center().child(
                            div()
                                .text_color(rgb(TEXT_MUTED))
                                .text_sm()
                                .child(t!("credentials.empty").to_string()),
                        )
                    },
                    |el| {
                        el.flex()
                            .flex_col()
                            .gap_1()
                            .children(visible.iter().map(|c| credential_row(c)))
                    },
                ),
        )
        // --- Credential form overlay ---
        .when_some(form_state, |el, state| {
            el.child(CredentialFormView::new(state))
        })
}

fn credential_row(cred: &CredentialEntry) -> impl IntoElement {
    let kind_label = match cred.kind {
        crabport_core::credential::CredentialKind::Password => {
            t!("credential_form.type_password").to_string()
        }
        crabport_core::credential::CredentialKind::Certificate => {
            t!("credential_form.type_certificate").to_string()
        }
    };

    div()
        .id(ElementId::Name(format!("cred-row-{}", cred.id).into()))
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(rgb(BG_BASE))
        .with_transition(ElementId::Name(format!("cred-row-{}", cred.id).into()))
        .transition_on_hover(Duration::from_millis(120), Linear, |hovered, s| {
            if *hovered {
                s.bg(rgb(SURFACE_ACTIVE))
            } else {
                s.bg(rgb(BG_BASE))
            }
        })
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(TEXT_PRIMARY))
                        .child(cred.name.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(TEXT_MUTED))
                        .child(kind_label),
                ),
        )
}
