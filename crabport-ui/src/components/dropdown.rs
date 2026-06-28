use crate::color::*;
use gpui::{prelude::FluentBuilder, *};
use gpui_animation::{animation::TransitionExt, transition::general::EaseInOutQuad};
use gpui_component::scroll::ScrollableElement;
use std::f32::consts::PI;
use std::{rc::Rc, time::Duration};

// ---------------------------------------------------------------------------
// Dropdown
// ---------------------------------------------------------------------------

const ITEM_HEIGHT: Pixels = px(32.0);
const MAX_MENU_HEIGHT: Pixels = px(256.0);

/// Dropdown option item.
#[derive(Clone)]
pub struct DropdownItem {
    pub label: SharedString,
    /// Opaque value the caller can match on in `on_change`.
    pub value: SharedString,
}

impl DropdownItem {
    pub fn new(label: impl Into<SharedString>) -> Self {
        let label: SharedString = label.into();
        Self {
            value: label.clone(),
            label,
        }
    }

    pub fn value(mut self, value: impl Into<SharedString>) -> Self {
        self.value = value.into();
        self
    }
}

/// Usage example:
///
/// ```ignore
/// Dropdown::new("profile")
///     .placeholder("Select profile…")
///     .item("Production")
///     .item("Staging")
///     .item("Development")
///     .selected(self.selected_idx)
///     .is_open(self.dropdown_open)
///     .on_change(cx.listener(|this, idx, _w, cx| {
///         this.selected_idx = *idx;
///         this.dropdown_open = false;
///         cx.notify();
///     }))
///     .on_toggle(cx.listener(|this, _w, cx| {
///         this.dropdown_open = !this.dropdown_open;
///         cx.notify();
///     }))
/// ```
#[derive(IntoElement)]
pub struct Dropdown {
    id: ElementId,
    id_str: String,
    style: StyleRefinement,
    items: Vec<DropdownItem>,
    selected: Option<usize>,
    placeholder: SharedString,
    is_open: bool,
    on_change: Option<Rc<dyn Fn(usize, &mut Window, &mut App) + 'static>>,
    on_toggle: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
}

impl Styled for Dropdown {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl Dropdown {
    pub fn new(id: impl Into<ElementId>) -> Self {
        let id: ElementId = id.into();
        let id_str = format!("{:?}", id);
        Self {
            id,
            id_str,
            style: Default::default(),
            items: Vec::new(),
            selected: None,
            placeholder: "Select…".into(),
            is_open: false,
            on_change: None,
            on_toggle: None,
        }
    }

    pub fn item(mut self, label: impl Into<SharedString>) -> Self {
        self.items.push(DropdownItem::new(label));
        self
    }

    pub fn item_with_value(
        mut self,
        label: impl Into<SharedString>,
        value: impl Into<SharedString>,
    ) -> Self {
        self.items.push(DropdownItem::new(label).value(value));
        self
    }

    pub fn selected(mut self, index: usize) -> Self {
        self.selected = Some(index);
        self
    }

    pub fn placeholder(mut self, text: impl Into<SharedString>) -> Self {
        self.placeholder = text.into();
        self
    }

    pub fn is_open(mut self, open: bool) -> Self {
        self.is_open = open;
        self
    }

    pub fn on_change(mut self, f: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(f));
        self
    }

    pub fn on_toggle(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for Dropdown {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let Self {
            id,
            id_str,
            style,
            items,
            selected,
            placeholder,
            is_open,
            on_change,
            on_toggle,
        } = self;

        let item_count = items.len();
        let selected_label = selected
            .and_then(|i| items.get(i))
            .map(|it| it.label.clone())
            .unwrap_or(placeholder);

        // ------------------------------------------------------------------
        // Trigger
        // ------------------------------------------------------------------
        let trigger_id = ElementId::Name(format!("{id_str}-trigger").into());

        let chevron_anim_id = ElementId::Name(format!("{id_str}-chevron-{is_open}").into());
        let chevron = svg()
            .path("icons/chevron-down.svg")
            .size_4()
            .text_color(rgb(TEXT_MUTED))
            .with_animation(
                chevron_anim_id,
                Animation::new(Duration::from_millis(150)).with_easing(ease_in_out),
                move |svg, delta| {
                    let angle = if is_open {
                        PI * delta
                    } else {
                        PI * (1.0 - delta)
                    };
                    svg.with_transformation(Transformation::rotate(radians(angle)))
                },
            );

        let trigger = div()
            .id(trigger_id)
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .w_full()
            .h_9()
            .px_3()
            .rounded_md()
            .bg(rgb(BG_BASE))
            .border_1()
            .border_color(rgb(BORDER))
            .cursor_pointer()
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(TEXT_PRIMARY))
                    .child(selected_label),
            )
            .child(chevron)
            .when_some(on_toggle, |this, cb| {
                this.on_click(move |_e, w, cx| {
                    cb(w, cx);
                })
            });

        // ------------------------------------------------------------------
        // Menu
        // ------------------------------------------------------------------
        let menu_id = ElementId::Name(format!("{id_str}-menu").into());
        let natural_height = ITEM_HEIGHT * item_count as f32;
        let menu_h = if natural_height > MAX_MENU_HEIGHT {
            MAX_MENU_HEIGHT
        } else {
            natural_height
        };

        let item_els: Vec<AnyElement> = items
            .into_iter()
            .enumerate()
            .map(|(i, item)| {
                let is_selected = selected == Some(i);
                let cb = on_change.clone();
                let item_id = ElementId::Name(format!("{id_str}-item-{i}").into());

                div()
                    .id(item_id.clone())
                    .flex()
                    .items_center()
                    .h(ITEM_HEIGHT)
                    .px_3()
                    .w_full()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_sm()
                    .text_color(if is_selected {
                        rgb(TEXT_PRIMARY)
                    } else {
                        rgb(TEXT_MUTED)
                    })
                    .bg(rgb(BG_BASE))
                    .child(item.label)
                    .with_transition(item_id)
                    .transition_on_hover(
                        Duration::from_millis(150),
                        EaseInOutQuad,
                        move |hovered, state| {
                            if *hovered {
                                state.bg(rgb(SURFACE_ACTIVE))
                            } else {
                                state.bg(rgb(BG_BASE))
                            }
                        },
                    )
                    .on_click(move |_e, w, cx| {
                        if let Some(ref f) = cb {
                            f(i, w, cx);
                        }
                    })
                    .into_any_element()
            })
            .collect();

        let menu = div()
            .id(menu_id.clone())
            .absolute()
            .top_full()
            .left_0()
            .mt_1()
            .w_full()
            .overflow_hidden()
            .rounded_md()
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(BG_BASE))
            .opacity(0.)
            .h(px(0.))
            .with_transition(menu_id)
            .transition_when_else(
                is_open,
                Duration::from_millis(250),
                EaseInOutQuad,
                move |state| state.h(menu_h).opacity(1.),
                move |state| state.h(px(0.)).opacity(0.),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_1()
                    .h_full()
                    .overflow_y_scrollbar()
                    .children(item_els),
            );

        // ------------------------------------------------------------------
        // Root
        // ------------------------------------------------------------------
        let mut root = div()
            .id(id)
            .relative()
            .w_full()
            .cursor_default()
            .child(trigger)
            .child(menu);

        root.style().refine(&style);
        root
    }
}
