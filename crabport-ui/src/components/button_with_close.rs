use crate::color::*;
use gpui::{prelude::FluentBuilder, *};
use gpui_animation::{animation::TransitionExt, transition::general::Linear};
use std::{rc::Rc, time::Duration};

#[derive(IntoElement)]
pub struct ButtonWithClose {
    id: ElementId,
    style: StyleRefinement,
    children: Vec<AnyElement>,
    on_click: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    on_hover: Option<Rc<dyn Fn(&bool, &mut Window, &mut App) + 'static>>,
    selected: Option<bool>,
    disabled: Option<bool>,
    centered: bool,
}

impl Styled for ButtonWithClose {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for ButtonWithClose {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl ButtonWithClose {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            style: Default::default(),
            children: Default::default(),
            on_click: None,
            on_close: None,
            on_hover: None,
            selected: None,
            disabled: None,
            centered: false,
        }
    }

    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(f));
        self
    }

    pub fn on_close(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(f));
        self
    }

    pub fn on_hover(mut self, f: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_hover = Some(Rc::new(f));
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = Some(selected);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = Some(disabled);
        self
    }

    pub fn centered(mut self, centered: bool) -> Self {
        self.centered = centered;
        self
    }
}

impl RenderOnce for ButtonWithClose {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let has_close = self.on_close.is_some();
        let close_bg_id = ElementId::Name(format!("{}-close-bg", self.id).into());
        let close_opacity_id = ElementId::Name(format!("{}-close-opacity", self.id).into());

        let mut root = div()
            .id(self.id.clone())
            .flex()
            .items_center()
            .when_else(
                self.centered,
                |this| this.justify_center(),
                |this| this.justify_start(),
            )
            .w_full()
            .border_1()
            .border_color(rgb(BTN_BORDER))
            .rounded_md()
            .h_8()
            .overflow_hidden()
            .bg(rgb(BTN_BG));
        root.style().refine(&self.style);

        // Label
        let label = div()
            .flex_1()
            .flex()
            .items_center()
            .min_w_0()
            .overflow_hidden()
            .text_ellipsis()
            .whitespace_nowrap()
            .children(self.children);

        // Close area — opacity driven by own hover state with transition
        let close_icon = if has_close {
            div()
                .id(close_opacity_id.clone())
                .h_full()
                .flex()
                .items_center()
                .justify_end()
                .mr_1()
                .opacity(0.)
                .child(
                    div()
                        .id(close_bg_id.clone())
                        .flex()
                        .items_center()
                        .justify_center()
                        .h_4()
                        .w_4()
                        .rounded_sm()
                        .cursor_pointer()
                        .child(
                            svg()
                                .path("icons/close.svg")
                                .size_3()
                                .text_color(rgb(TEXT_PRIMARY)),
                        )
                        .on_click({
                            let on_close = self.on_close.clone();
                            move |_e, w, cx| {
                                if let Some(ref cb) = on_close {
                                    cb(w, cx);
                                }
                            }
                        })
                        .bg(rgb(SURFACE_ACTIVE)),
                )
                .with_transition(close_opacity_id)
                .transition_on_hover(Duration::from_millis(100), Linear, |hovered, el| {
                    if *hovered {
                        el.opacity(1.)
                    } else {
                        el.opacity(0.)
                    }
                })
                .into_any_element()
        } else {
            div().into_any_element()
        };

        let content = div().flex_1().flex().items_center().min_w_0().child(label);

        root.child(content)
            .when(has_close, |el| el.child(close_icon))
            .with_transition(self.id.clone())
            .when_else(
                self.disabled.unwrap_or_default(),
                |this| {
                    this.bg(rgb(BTN_BG_DISABLED))
                        .text_color(rgb(BTN_TEXT_DISABLED))
                        .cursor_not_allowed()
                },
                |this| {
                    this.text_color(rgb(TEXT_PRIMARY))
                        .when_some(self.on_hover, |this, on_hover| {
                            this.on_hover(move |h, w, a| (on_hover)(h, w, a))
                        })
                        .when_some(self.on_click, |this, on_click| {
                            this.on_click(move |e, w, a| (on_click)(e, w, a))
                        })
                        .transition_when_else(
                            self.selected.unwrap_or_default(),
                            Duration::from_millis(250),
                            Linear,
                            |this| this.bg(rgb(BTN_BG_SELECTED)),
                            |this| this.bg(rgb(BTN_BG)),
                        )
                        .transition_on_hover(Duration::from_millis(250), Linear, |hovered, this| {
                            if *hovered {
                                this.bg(rgb(BTN_BG_HOVER))
                            } else {
                                this
                            }
                        })
                },
            )
    }
}
