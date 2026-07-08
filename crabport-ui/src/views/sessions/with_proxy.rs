use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::input::InputState;
use rust_i18n::t;

use crate::components::input::StyledInput;
use crate::components::tabs::{TabPane, Tabs};

/// Which proxy mode the user picked in the proxy sub-tabs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProxyKind {
    None,
    System,
    Custom,
}

impl ProxyKind {
    pub fn as_index(&self) -> usize {
        match self {
            ProxyKind::None => 0,
            ProxyKind::System => 1,
            ProxyKind::Custom => 2,
        }
    }

    pub fn from_index(i: usize) -> Self {
        match i {
            1 => ProxyKind::System,
            2 => ProxyKind::Custom,
            _ => ProxyKind::None,
        }
    }
}

/// Proxy configuration form. A 3-tab strip (None / System / Custom) where
/// only the Custom tab has content (a proxy URL input). None and System are
/// empty — they just select a mode. The tabs component height-animates
/// between the empty tabs (~0 content) and the Custom tab (one input).
#[derive(IntoElement)]
pub struct WithProxyForm {
    pub proxy_url_input: Entity<InputState>,
    pub proxy_url_focused: bool,
    pub proxy_kind: ProxyKind,
    /// Per-field validation error for the proxy URL (only relevant when
    /// `proxy_kind == Custom`).
    pub proxy_url_error: Option<SharedString>,
    pub app: Entity<crate::app::CrabportApp>,
}

impl RenderOnce for WithProxyForm {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let app = self.app.clone();
        let proxy_url_error = self.proxy_url_error.clone();
        let has_error = proxy_url_error.is_some();

        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(crate::color::text_muted()))
                    .child(t!("connection_form.proxy").to_string()),
            )
            .child(
                Tabs::new("conn-proxy-tabs")
                    .active(self.proxy_kind.as_index())
                    .pane(
                        TabPane::new(t!("connection_form.proxy_none").to_string(), div())
                            .height(px(0.0)),
                    )
                    .pane(
                        TabPane::new(t!("connection_form.proxy_system").to_string(), div())
                            .height(px(0.0)),
                    )
                    .pane(
                        TabPane::new(
                            t!("connection_form.proxy_custom").to_string(),
                            div().flex().flex_col().gap_4().child(
                                StyledInput::new("proxy-url", self.proxy_url_input)
                                    .label(t!("connection_form.proxy_url").to_string())
                                    .focused(self.proxy_url_focused)
                                    .when_some(proxy_url_error, |el, e| el.error(e)),
                            ),
                        )
                        .height(px(if has_error { 80.0 } else { 57.0 })),
                    )
                    .on_change(move |index, w, cx| {
                        let kind = ProxyKind::from_index(index);
                        app.update(cx, |app, cx| {
                            if let Some(ref mut form) = app.connection_form {
                                form.proxy_kind = kind;
                                if kind == ProxyKind::Custom {
                                    form.proxy_url_input.update(cx, |state, cx| {
                                        state.focus(w, cx);
                                    });
                                }
                                cx.notify();
                            }
                        });
                    }),
            )
    }
}
