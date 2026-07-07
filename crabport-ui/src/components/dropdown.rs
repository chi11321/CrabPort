use crate::color::*;
use gpui::{prelude::FluentBuilder, *};
use gpui_animation::{animation::TransitionExt, transition::general::EaseInOutQuad};
use gpui_component::scroll::ScrollableElement;
use rust_i18n::t;
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
///
/// ## Searchable + creatable
///
/// Pass a search `InputState` entity via `.searchable(...)` to render a
/// search box at the top of the menu. Items are filtered (case-insensitive
/// substring) against the query. Pass `.on_create(...)` to render a
/// "Create \"<query>\"" button at the bottom that fires the callback with
/// the query text — only shown when the query doesn't exactly match an
/// existing item.
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
    /// When `Some`, the menu renders a search box backed by this `InputState`
    /// and filters items by the query.
    search_input: Option<Entity<gpui_component::input::InputState>>,
    /// When `Some`, the menu renders a "Create \"<query>\"" button at the
    /// bottom when the search query doesn't match an existing item label.
    on_create: Option<Rc<dyn Fn(String, &mut Window, &mut App) + 'static>>,
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
            search_input: None,
            on_create: None,
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

    /// Enable a search box at the top of the menu, backed by the given
    /// `InputState` entity. The caller owns the entity so the query survives
    /// re-renders. Items are filtered (case-insensitive substring) against
    /// the query text.
    pub fn searchable(mut self, search_input: Entity<gpui_component::input::InputState>) -> Self {
        self.search_input = Some(search_input);
        self
    }

    /// Render a "Create \"<query>\"" button at the bottom of the menu when
    /// the search query doesn't exactly match an existing item's label.
    /// Fires the callback with the query text on click.
    pub fn on_create(mut self, f: impl Fn(String, &mut Window, &mut App) + 'static) -> Self {
        self.on_create = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for Dropdown {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
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
            search_input,
            on_create,
        } = self;

        // A disabled dropdown never shows its menu, regardless of `is_open`.
        let is_open = is_open && !disabled;

        let trigger_bounds: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::new(Cell::new(None));

        let selected_label = selected
            .and_then(|i| items.get(i))
            .map(|it| it.label.clone())
            .unwrap_or(placeholder);

        // ------------------------------------------------------------------
        // Trigger
        // ------------------------------------------------------------------
        let trigger_id = ElementId::Name(format!("{id_str}-trigger").into());
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

        // Compute the search query (if searchable) and filter items.
        let query: String = search_input
            .as_ref()
            .map(|s| s.read(_cx).value().to_lowercase())
            .unwrap_or_default();
        let has_search = search_input.is_some() && !query.is_empty();

        // Filtered items: (original_index, item) pairs.
        let filtered: Vec<(usize, &DropdownItem)> = items
            .iter()
            .enumerate()
            .filter(|(_, it)| {
                if has_search {
                    it.label.to_lowercase().contains(&query)
                } else {
                    true
                }
            })
            .collect();

        // Determine whether the create button should show: only when the
        // query is non-empty and doesn't exactly match an existing label.
        let can_create = on_create.is_some()
            && has_search
            && !items.iter().any(|it| it.label.to_lowercase() == query);

        // Whether the empty-state hint shows: searchable + has query + no
        // matches + no create button.
        let _has_empty = search_input.is_some() && has_search && filtered.is_empty() && !can_create;

        // Menu height for the open/close transition.
        //
        // We animate `h()` between 0 (closed) and a capped max (open).
        // Rather than estimating the exact content height (which is fragile
        // due to sub-pixel rounding of gaps/padding/border), we set the
        // open height to `MAX_MENU_HEIGHT` and let `overflow_hidden` on
        // the outer div clip any excess. The inner content div has
        // `max_h(MAX_MENU_HEIGHT)` + `overflow_y_scrollbar` so it scrolls
        // when content exceeds the cap. When content is shorter than the
        // cap, the outer div still renders at `MAX_MENU_HEIGHT` but the
        // inner content only fills what it needs — the extra space is
        // just empty background, which is fine since the menu has a
        // solid `bg(bg_base())`.
        //
        // To avoid the empty space when content is short, we DO compute
        // the natural height — but only use it if it's less than the cap.
        // The computation accounts for: ITEM_HEIGHT per child, gap_1 (4px)
        // between children, p_1 (8px) padding on the inner div, and
        // border_1 (2px) on the outer div.
        let content_item_count = filtered.len();
        let has_search_el = search_input.is_some();
        let has_create_el = can_create;
        let has_empty_el = _has_empty;
        let child_count = content_item_count
            + has_search_el as usize
            + has_empty_el as usize
            + has_create_el as usize;
        // Each child is ITEM_HEIGHT. Gaps between adjacent children = 4px
        // each (gap_1 = rems(0.25) = 4px). Inner padding p_1 = 8px total.
        // Outer border_1 = 2px total (included in h()).
        let gap_total = child_count.saturating_sub(1) as f32 * 4.0;
        let natural_height = f32::from(ITEM_HEIGHT) * child_count as f32
            + gap_total
            + 8.0  // p_1 padding (top + bottom)
            + 2.0; // border_1 (top + bottom, included in h())
        let menu_h = if natural_height > f32::from(MAX_MENU_HEIGHT) {
            MAX_MENU_HEIGHT
        } else {
            px(natural_height)
        };

        // Build the item elements from the filtered list.
        let item_els: Vec<AnyElement> = filtered
            .into_iter()
            .map(|(orig_i, item)| {
                let is_selected = selected == Some(orig_i);
                let cb = on_change.clone();
                let item_id = ElementId::Name(format!("{id_str}-item-{orig_i}").into());

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
                    .child(item.label.clone())
                    .on_click(move |_e, w, cx| {
                        if let Some(ref f) = cb {
                            f(orig_i, w, cx);
                        }
                    })
                    .into_any_element()
            })
            .collect();

        // Search box (if searchable). Same height as an item, only a
        // bottom border to visually separate it from the item list.
        let search_el = search_input.clone().map(|s| {
            div()
                .flex()
                .items_center()
                .gap_1()
                .h(ITEM_HEIGHT)
                .px_3()
                .w_full()
                .border_b_1()
                .border_color(rgb(border()))
                .child(
                    svg()
                        .path("icons/search.svg")
                        .size(px(12.0))
                        .text_color(rgb(text_muted())),
                )
                .child(
                    gpui_component::input::Input::new(&s)
                        .appearance(false)
                        .bordered(false),
                )
        });

        // Create button (if creatable + query doesn't match).
        let create_el = if can_create {
            let on_create = on_create.unwrap();
            let query_str = search_input
                .as_ref()
                .map(|s| s.read(_cx).value().to_string())
                .unwrap_or_default();
            let on_toggle_close = on_toggle.clone();
            Some(
                div()
                    .id(ElementId::Name(format!("{id_str}-create").into()))
                    .flex()
                    .items_center()
                    .h(ITEM_HEIGHT)
                    .px_3()
                    .w_full()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_sm()
                    .text_color(rgb(term_blue()))
                    .hover(|s| s.bg(rgb(surface_active())))
                    .child(t!("groups.create", name = query_str.as_str()).to_string())
                    .on_click(move |_e, w, cx| {
                        on_create(query_str.clone(), w, cx);
                        // Close the menu after creating.
                        if let Some(ref cb) = on_toggle_close {
                            cb(w, cx);
                        }
                    })
                    .into_any_element(),
            )
        } else {
            None
        };

        // Empty-state hint when searchable + has query + no matches.
        let empty_el =
            (search_input.is_some() && has_search && item_els.is_empty() && create_el.is_none())
                .then(|| {
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .h(ITEM_HEIGHT)
                        .text_sm()
                        .text_color(rgb(text_muted()))
                        .child(t!("groups.no_results").to_string())
                        .into_any_element()
                });

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
            .when(is_open, |el| {
                el.when_some(on_toggle, |el, cb| {
                    let trigger_bounds = trigger_bounds.clone();
                    el.on_mouse_down_out(move |e, w, cx| {
                        if trigger_bounds
                            .get()
                            .is_some_and(|b| b.contains(&e.position))
                        {
                            return;
                        }
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
                    .p_1()
                    .gap_1()
                    .h_full()
                    .overflow_y_scrollbar()
                    .when_some(search_el, |el, s| el.child(s))
                    .children(item_els)
                    .when_some(empty_el, |el, e| el.child(e))
                    .when_some(create_el, |el, c| el.child(c)),
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
            .child(deferred(menu));

        root.style().refine(&style);
        root
    }
}
