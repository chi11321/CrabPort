use crate::color::*;
use gpui::{prelude::FluentBuilder, *};
use gpui_animation::{animation::TransitionExt, transition::general::EaseInOutQuad};
use gpui_component::scroll::ScrollableElement;
use std::cell::Cell;
use std::f32::consts::PI;
use std::{rc::Rc, time::Duration};

/// Rotation (in radians) of the trigger chevron when the menu is open.
/// PI = 180°, so the down-chevron points up when open.
const CHEVRON_OPEN_ROTATION: f32 = PI;

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
    disabled: bool,
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
            disabled: false,
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

    /// Disable interaction and visually mute the dropdown. A disabled
    /// dropdown never opens its menu, even if `is_open` is left `true`.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
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
            disabled,
            on_change,
            on_toggle,
        } = self;

        // A disabled dropdown never shows its menu, regardless of `is_open`.
        let is_open = is_open && !disabled;

        // Shared slot for the trigger's window-space bounds, captured each
        // frame by the `canvas` child inside the trigger (see below) and read
        // by the menu's `on_mouse_down_out` handler. Using `Cell<Option<...>>`
        // lets the prepaint callback (which runs before paint) hand the bounds
        // to the mouse-down listener registered during paint, all within a
        // single frame and without needing an Entity to hold state.
        let trigger_bounds: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::new(Cell::new(None));

        let item_count = items.len();
        let selected_label = selected
            .and_then(|i| items.get(i))
            .map(|it| it.label.clone())
            .unwrap_or(placeholder);

        // ------------------------------------------------------------------
        // Trigger
        // ------------------------------------------------------------------
        let trigger_id = ElementId::Name(format!("{id_str}-trigger").into());

        // Chevron: rotate 180° when open. We animate via GPUI's built-in
        // `with_animation` rather than `gpui-animation`'s `transition_when_else`,
        // because the latter only interpolates `StyleRefinement` fields (bg,
        // opacity, size…) and SVG `Transformation` is not part of the style.
        //
        // The animation ID encodes `is_open` so that flipping the toggle
        // creates a fresh `AnimationState` (start = `Instant::now()`) and the
        // rotation re-runs from the opposite end. Without this, the cached
        // state would report `delta > 1` (animation already finished) and the
        // chevron would snap instead of rotating.
        //
        // For the close animation we must animate *back* from PI to 0, so the
        // rotation is `(1 - delta) * PI` (start = PI, end = 0). Computing it as
        // `delta * 0` would leave the chevron at 0 the whole time — no visible
        // reverse motion.
        let chevron_anim_id = ElementId::Name(format!("{id_str}-chevron-{}", is_open).into());

        let chevron = svg()
            .path("icons/chevron-down.svg")
            .size_4()
            .text_color(rgb(text_muted()))
            .with_animation(
                chevron_anim_id,
                Animation::new(Duration::from_millis(200)).with_easing(ease_in_out),
                move |this, delta| {
                    let angle = if is_open {
                        delta * CHEVRON_OPEN_ROTATION
                    } else {
                        (1.0 - delta) * CHEVRON_OPEN_ROTATION
                    };
                    this.with_transformation(Transformation::rotate(radians(angle)))
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
            .bg(rgb(if disabled {
                input_bg_disabled()
            } else {
                bg_base()
            }))
            .border_1()
            .border_color(rgb(border()))
            .when_else(
                disabled,
                |el| el.cursor_not_allowed().opacity(0.5),
                |el| el.cursor_pointer(),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(if disabled {
                        input_text_disabled()
                    } else {
                        text_primary()
                    }))
                    .child(selected_label),
            )
            .child(chevron)
            // Capture the trigger's bounds each frame so the menu's
            // `on_mouse_down_out` handler can tell a click *on the trigger*
            // (which the trigger's own `on_click` will toggle) apart from a
            // click *outside* the dropdown (which should dismiss the menu).
            // The canvas is absolute + size_full so it overlays the trigger
            // exactly without participating in the flex layout, and it has no
            // hitbox so it never intercepts pointer events.
            .child(
                canvas(
                    {
                        let trigger_bounds = trigger_bounds.clone();
                        move |bounds, _window, _cx| {
                            trigger_bounds.set(Some(bounds));
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .when_some(on_toggle.clone(), |this, cb| {
                this.when(!disabled, |this| {
                    this.on_click(move |_e, w, cx| {
                        cb(w, cx);
                    })
                })
            });

        // ------------------------------------------------------------------
        // Menu
        // ------------------------------------------------------------------
        let menu_id = ElementId::Name(format!("{id_str}-menu").into());
        // Menu height = items + gap_1 between them. The inner p_1 padding
        // is accounted for by NOT adding it — empirically the rendered
        // padding is smaller than the theoretical 8px.
        let gap_total = if item_count > 1 {
            (item_count - 1) as f32 * 4.0
        } else {
            0.0
        };
        let natural_height = f32::from(ITEM_HEIGHT) * item_count as f32 + gap_total;
        let menu_h = if natural_height > f32::from(MAX_MENU_HEIGHT) {
            MAX_MENU_HEIGHT
        } else {
            px(natural_height)
        };

        let item_els: Vec<AnyElement> = items
            .into_iter()
            .enumerate()
            .map(|(i, item)| {
                let is_selected = selected == Some(i);
                let cb = on_change.clone();
                let item_id = ElementId::Name(format!("{id_str}-item-{i}").into());

                // Use GPUI's native `hover()` style instead of gpui-animation's
                // `transition_on_hover`. The animation variant caches the
                // initial element state (including text_color) and fails to
                // pick up `selected` changes on re-render, so the highlight
                // never follows the new selection. Native hover applies the
                // bg purely from the current render's style, with no cached
                // state, so text_color updates take effect immediately.
                div()
                    .id(item_id)
                    .flex()
                    .items_center()
                    .h(ITEM_HEIGHT)
                    .px_3()
                    .w_full()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_sm()
                    .text_color(rgb(if is_selected {
                        text_primary()
                    } else {
                        text_muted()
                    }))
                    .bg(rgb(bg_base()))
                    .hover(|s| s.bg(rgb(surface_active())))
                    .child(item.label)
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
            .border_color(rgb(border()))
            .bg(rgb(bg_base()))
            .opacity(0.)
            .h(px(0.))
            .when(is_open, |el| el.occlude())
            // Dismiss the menu when the user mouse-downs outside of it. This
            // mirrors the context_menu's backdrop-click behavior but without a
            // full-screen overlay: the menu's own hitbox defines "inside", so
            // any mouse-down that lands outside (on another field, empty space,
            // etc.) fires the handler during the capture phase.
            //
            // We deliberately skip dismissal when the click lands on the
            // trigger: that case is handled by the trigger's `on_click` which
            // toggles `is_open`. Without this guard, clicking the trigger to
            // close would dismiss here *and* toggle, netting to no change (or
            // re-opening, depending on order). We also guard with `is_open` so
            // a collapsed (height-0) menu — whose hitbox contains no point —
            // doesn't fire the handler for every click in the window and flip
            // the dropdown open.
            .when(is_open, |el| {
                el.when_some(on_toggle, |el, cb| {
                    let trigger_bounds = trigger_bounds.clone();
                    el.on_mouse_down_out(move |e, w, cx| {
                        // If the click is on the trigger, let the trigger's
                        // own click handler toggle the state.
                        if trigger_bounds
                            .get()
                            .is_some_and(|b| b.contains(&e.position))
                        {
                            return;
                        }
                        // Otherwise dismiss by toggling (caller's `on_toggle`
                        // flips `dropdown_open` from true -> false).
                        cb(w, cx);
                    })
                })
            })
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
            // `deferred` delays the menu's paint until after all ancestors
            // and siblings, so the open menu renders on top of form elements
            // that follow the dropdown in the layout. `occlude` (applied on
            // the menu above when open) ensures it also captures clicks.
            .child(deferred(menu));

        root.style().refine(&style);
        root
    }
}
