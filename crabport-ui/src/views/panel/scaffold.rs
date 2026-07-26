//! Shared scaffolding for the right-hand list panels (snippets / tunnels /
//! command history): search + scroll + hover state, the panel body shell,
//! and the ghost icon-button used in rows and toolbars.
//!
//! Everything here is a behavior-preserving extraction from the three
//! sibling panels ([`super::snippets_panel`], [`super::tunnels_panel`],
//! [`super::history_command_panel`]). Element ids, transition ids, and the
//! builder-call order that matters (styles before `with_transition`;
//! handlers the `AnimatedWrapper` does not forward — e.g. `on_mouse_down` —
//! attached before it) are kept exactly as they were, since animation and
//! scroll state are keyed by those ids.

use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::{AnimatedWrapper, TransitionExt};
use gpui_component::VirtualListScrollHandle;
use gpui_component::input::InputState;
use gpui_component::scroll::{Scrollbar, ScrollbarShow};
use rust_i18n::t;

use crate::color::*;
use crate::components::input::StyledInput;
use crate::components::tooltip::TooltipController;
use crate::motion::{EASE_STANDARD, RADIUS_MD, duration_fast};

/// Common list-panel view state: the lazily-created search input, the
/// current query, the virtual-list scroll handle, and the hovered row.
///
/// Panels that keep per-tab view state (tunnels) leave `search_query` /
/// `hovered_row` at their defaults and store those per tab instead — the
/// [`PanelListState::ensure_search_input`] callback decides where a query
/// write lands.
pub struct PanelListState {
    /// Search input state (lazily initialized on the first `set_state`
    /// call, which is the first moment a `Window` is available).
    pub search_input: Option<Entity<InputState>>,
    /// Current search query (single-list panels only).
    pub search_query: String,
    /// Scroll handle for the virtual list + custom scrollbar.
    pub scroll_handle: VirtualListScrollHandle,
    /// Index (into the filtered list) of the hovered row, if any
    /// (single-list panels only).
    pub hovered_row: Option<usize>,
}

impl Default for PanelListState {
    fn default() -> Self {
        Self {
            search_input: None,
            search_query: String::new(),
            scroll_handle: VirtualListScrollHandle::new(),
            hovered_row: None,
        }
    }
}

impl PanelListState {
    /// Lazily create the search [`InputState`] on the first `set_state` call
    /// (the panel constructors have no `Window`), and subscribe to its
    /// `Change` events, re-filtering on every keystroke. `on_query` receives
    /// the new query text and decides where to store it (the panel's own
    /// [`PanelListState::search_query`], or per-tab state) and whether to
    /// `notify`.
    pub fn ensure_search_input<V: 'static>(
        &mut self,
        placeholder_key: &'static str,
        on_query: fn(&mut V, String, &mut Context<V>),
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        if self.search_input.is_none() {
            let entity = cx
                .new(|cx| InputState::new(window, cx).placeholder(t!(placeholder_key).to_string()));
            cx.subscribe(
                &entity,
                move |this, input, event: &gpui_component::input::InputEvent, cx| {
                    if let gpui_component::input::InputEvent::Change { .. } = event {
                        let query = input.read(cx).value().to_string();
                        on_query(this, query, cx);
                    }
                },
            )
            .detach();
            self.search_input = Some(entity);
        }
    }
}

/// Case-insensitive substring filter over `items`, returning indices into
/// the original list so rows can be cloned out cheaply. The query is
/// trimmed + lowercased once; an empty query selects everything. `matches`
/// receives the already-lowercased needle.
pub fn filter_indices<T>(
    items: &[T],
    query: &str,
    matches: impl Fn(&T, &str) -> bool,
) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return (0..items.len()).collect();
    }
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| matches(item, &q))
        .map(|(i, _)| i)
        .collect()
}

/// Per-render prep for the virtual list: clone the filtered rows out of the
/// source snapshot, pre-compute the uniform item sizes the virtual list
/// requires, and derive the empty flag.
pub fn prepare_list_items<T: Clone>(
    source: &[T],
    filtered_indices: &[usize],
    row_height: f32,
) -> (Arc<Vec<T>>, Rc<Vec<Size<Pixels>>>, bool) {
    let filtered: Vec<T> = filtered_indices
        .iter()
        .map(|&i| source[i].clone())
        .collect();
    let item_sizes = Rc::new(
        (0..filtered.len())
            .map(|_| Size {
                width: px(0.0),
                height: px(row_height),
            })
            .collect::<Vec<_>>(),
    );
    let filtered = Arc::new(filtered);
    let is_empty = filtered.is_empty();
    (filtered, item_sizes, is_empty)
}

/// How the panel shell hosts its search input row.
pub enum SearchRow {
    /// Bare input in an `mb_1` wrapper (snippets / tunnels).
    Bare,
    /// Input inside a flex row with a trailing slot. The row chrome is
    /// rendered even when the slot is `None` (history's refresh button is
    /// only present when the active backend supports a history reload).
    WithSuffix(Option<AnyElement>),
}

/// The shared panel body: full-size column with the search row on top and,
/// below it, either the empty-state placeholder or the bordered list
/// container with its hover-scrollbar overlay.
pub fn panel_shell(
    search_id: &'static str,
    search_input: Option<Entity<InputState>>,
    search_row: SearchRow,
    is_empty: bool,
    empty_text: String,
    list: impl IntoElement,
    scroll_handle: &VirtualListScrollHandle,
) -> Div {
    let scroll_handle = scroll_handle.clone();
    div()
        .h_full()
        .w_full()
        .min_h_0()
        .overflow_hidden()
        .flex()
        .flex_col()
        .pt_1()
        .px_1()
        // Search input
        .when_some(search_input, |el, input| {
            let field = StyledInput::new(search_id, input).xsmall().prefix(
                svg()
                    .path("icons/search.svg")
                    .size(px(12.0))
                    .text_color(rgb(text_muted())),
            );
            el.child(match search_row {
                SearchRow::Bare => div().mb_1().child(field),
                SearchRow::WithSuffix(suffix) => div()
                    .mb_1()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(div().flex_1().min_w_0().child(field))
                    .when_some(suffix, |el, suffix| el.child(suffix)),
            })
        })
        // List + scrollbar, or empty-state placeholder.
        .when_else(
            is_empty,
            |el| {
                el.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .text_color(rgb(text_muted()))
                                .text_sm()
                                .child(empty_text),
                        ),
                )
            },
            |el| {
                el.child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .border_1()
                        .border_color(rgb(border()))
                        .bg(rgb(bg_tab_bar()))
                        .rounded(RADIUS_MD)
                        .overflow_hidden()
                        .child(list)
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .right_0()
                                .bottom_0()
                                .w(px(16.0))
                                .child(
                                    Scrollbar::vertical(&scroll_handle)
                                        .scrollbar_show(ScrollbarShow::Hover),
                                ),
                        ),
                )
            },
        )
}

/// Accessor for a panel's hovered-row slot. Returning `None` skips the
/// write + repaint entirely (tunnels without an active tab).
pub type HoverSlot<V> = fn(&mut V) -> Option<&mut Option<usize>>;

/// The shared row skeleton: fixed-height, full-width flex row with the
/// standard gap / padding / rounding. Callers append panel-specific styles
/// (e.g. `.relative()`) and any pre-transition mouse handlers before
/// passing the result to [`row_hover_chrome`].
pub fn row_base(row_id: ElementId, row_height: f32) -> Stateful<Div> {
    div()
        .id(row_id)
        .h(px(row_height))
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap_1p5()
        .px_2()
        .rounded(px(4.0))
}

/// Wrap a row in its transition, wire hover tracking into the panel's
/// hovered-row slot, and ease the row background with the highlight state.
/// Hover drives the row background (and, where present, the hover-button
/// strip's opacity) via `transition_when_else` — not `transition_on_hover` —
/// so overlaid buttons can stay visible while the row is hovered,
/// independent of the mouse position over the buttons themselves.
///
/// Interactive handlers that the `AnimatedWrapper` does not forward (e.g.
/// `on_mouse_down`) must already be attached to `el`.
pub fn row_hover_chrome<V: 'static>(
    el: Stateful<Div>,
    transition_id: ElementId,
    is_highlighted: bool,
    entity: WeakEntity<V>,
    row_index: usize,
    hover_slot: HoverSlot<V>,
) -> AnimatedWrapper<Stateful<Div>> {
    el.with_transition(transition_id)
        .on_hover(move |hovered, _w, cx| {
            let _ = entity.update(cx, |view, cx| {
                if let Some(row) = hover_slot(view) {
                    if *hovered {
                        *row = Some(row_index);
                    } else if *row == Some(row_index) {
                        // Only clear if we still own the hover — another
                        // row may have already claimed it (prevents the
                        // bottom-to-top glitch where `false` fires after
                        // `true`).
                        *row = None;
                    }
                    cx.notify();
                }
            });
        })
        .transition_when_else(
            is_highlighted,
            duration_fast(),
            EASE_STANDARD,
            |el| el.bg(rgba((surface_hover() << 8) | 0x60)),
            |el| el.bg(rgba((surface_hover() << 8) | 0x00)),
        )
}

/// The absolutely-positioned button strip overlaid on a row's right edge,
/// layered above the row text with a transparent background so the buttons
/// don't displace the text when they fade in. The container is a
/// `Stateful<Div>` (has an id) so it supports `with_transition` +
/// `transition_when_else` for a smooth opacity ease on row hover.
pub fn hover_button_strip(
    id: String,
    is_hovered: bool,
    buttons: impl IntoIterator<Item = AnyElement>,
) -> impl IntoElement {
    div()
        .id(ElementId::Name(id.clone().into()))
        .absolute()
        .top_0()
        .right_0()
        .bottom_0()
        .flex()
        .flex_row()
        .items_center()
        .gap_0p5()
        .pr_2()
        .bg(rgba(0x00000000))
        .opacity(0.0)
        .with_transition(ElementId::Name(id.into()))
        .transition_when_else(
            is_hovered,
            duration_fast(),
            EASE_STANDARD,
            |el| el.opacity(1.0),
            |el| el.opacity(0.0),
        )
        .children(buttons)
}

/// The ghost icon button used in panel rows and toolbars: a transparent
/// square that eases to `surface_hover` on hover, with a tooltip and a
/// click action. `size` is the button square in px (20 for row buttons,
/// 24 for the history toolbar's refresh); the icon is always 13 px.
pub fn panel_icon_button(
    id: String,
    size: f32,
    flex_shrink0: bool,
    icon_path: &'static str,
    tooltip_key: &'static str,
    tooltip: Option<Entity<TooltipController>>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnimatedWrapper<Stateful<Div>> {
    div()
        .id(ElementId::Name(id.clone().into()))
        .flex()
        .items_center()
        .justify_center()
        .size(px(size))
        .when(flex_shrink0, |el| el.flex_shrink_0())
        .rounded(px(4.0))
        .bg(rgba(0x00000000))
        .with_transition(ElementId::Name(id.into()))
        .on_hover(move |hovered, w, cx| {
            if let Some(ref tc) = tooltip {
                if *hovered {
                    tc.update(cx, |t, cx| {
                        t.show(t!(tooltip_key).to_string(), w.mouse_position(), cx);
                    });
                } else {
                    tc.update(cx, |t, cx| {
                        t.hide(cx);
                    });
                }
            }
        })
        .transition_on_hover(duration_fast(), EASE_STANDARD, |hovered, el| {
            if *hovered {
                el.bg(rgb(surface_hover()))
            } else {
                el.bg(rgba(0x00000000))
            }
        })
        .on_click(on_click)
        .child(
            svg()
                .path(icon_path)
                .size(px(13.0))
                .text_color(rgb(text_muted())),
        )
}
