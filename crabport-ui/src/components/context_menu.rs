//! # ContextMenu
//!
//! A global, reusable context (right-click) menu. Like [`AlertController`],
//! it's an `Entity` held by the app root and rendered as a top-level child.
//! Trigger it from anywhere via:
//!
//! ```ignore
//! context_menu.update(cx, |c, cx| {
//!     c.show(ContextMenuState {
//!         position: point(px(x), px(y)),
//!         items: vec![
//!             ContextMenuItem::new("Copy", |w, cx| { /* ... */ }),
//!             ContextMenuItem::new("Paste", |w, cx| { /* ... */ }),
//!         ],
//!         ..ContextMenuState::default()
//!     }, cx);
//! });
//! ```
//!
//! The menu animates in (opacity + scale) and out. Clicking an item or the
//! backdrop dismisses it with the same easing.

use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::TransitionExt;

use crate::color::*;
use crate::motion::{DURATION_BASE, DURATION_INSTANT, EASE_STANDARD, RADIUS_MD, RADIUS_SM};

// ---------------------------------------------------------------------------
// ContextMenuItem
// ---------------------------------------------------------------------------

/// A single entry in a context menu.
#[derive(Clone)]
pub struct ContextMenuItem {
    pub label: SharedString,
    /// Optional icon path (e.g. `"icons/copy.svg"`).
    pub icon: Option<SharedString>,
    /// Invoked when the user clicks the item. Receives `(&mut Window, &mut App)`.
    pub on_click: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    /// Render the item in a muted / disabled style and skip the click handler.
    pub disabled: bool,
    /// Render the label in red — use for destructive actions ("Delete", etc.).
    pub danger: bool,
    /// Render a divider line after this item. Set on the item that should
    /// be visually followed by a separator.
    pub divider_after: bool,
}

impl ContextMenuItem {
    pub fn new(
        label: impl Into<SharedString>,
        on_click: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            icon: None,
            on_click: Some(Rc::new(on_click)),
            disabled: false,
            danger: false,
            divider_after: false,
        }
    }

    pub fn with_icon(mut self, icon: impl Into<SharedString>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn danger(mut self, danger: bool) -> Self {
        self.danger = danger;
        self
    }

    /// Render a divider line below this item.
    pub fn divider_after(mut self) -> Self {
        self.divider_after = true;
        self
    }
}

// ---------------------------------------------------------------------------
// ContextMenuState
// ---------------------------------------------------------------------------

/// Describes one context menu invocation. Cloning is cheap (callbacks are `Rc`).
#[derive(Clone, Default)]
pub struct ContextMenuState {
    /// Screen-space position (top-left of the menu card) in window pixels.
    pub position: Point<Pixels>,
    pub items: Vec<ContextMenuItem>,
    /// Optional title shown at the top of the menu in a muted style.
    pub header: Option<SharedString>,
    /// Whether the menu is currently shown. Drives the in/out transition.
    pub open: bool,
    /// When `true`, clicking an item does NOT auto-dismiss the menu — the
    /// menu stays open so the user can toggle multiple items in a row
    /// (used by the toolbar's slot-visibility menu). The menu is closed
    /// by clicking outside it (backdrop) or pressing Escape, same as the
    /// non-sticky variant.
    ///
    /// Default `false` — the conventional behavior where a click both
    /// invokes the item's handler and dismisses the menu.
    pub sticky: bool,
}

impl ContextMenuState {
    pub fn new(position: Point<Pixels>) -> Self {
        Self {
            position,
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// ContextMenuController — global host
// ---------------------------------------------------------------------------

/// How long the dismiss animation runs before the state is dropped. Matches
/// the `transition_when_else` duration used in `render_menu`.
const CONTEXT_MENU_DISMISS_MS: u64 = 150;

pub struct ContextMenuController {
    /// `None` when no menu is showing or dismissing.
    state: Option<ContextMenuState>,
    /// Monotonic counter incremented on every `show`. The dismiss spawn
    /// task captures the generation at scheduling time and bails out if
    /// it has changed by the time it fires — this prevents a stale
    /// dismiss from clearing a freshly-shown menu.
    generation: u64,
}

impl ContextMenuController {
    pub fn new() -> Self {
        Self {
            state: None,
            generation: 0,
        }
    }

    /// Show a context menu at `state.position`. Any currently-showing menu
    /// is replaced (its item callbacks are dropped without being invoked).
    pub fn show(&mut self, mut state: ContextMenuState, cx: &mut Context<Self>) {
        let entity = cx.entity().downgrade();
        let sticky = state.sticky;

        // Wrap each item's click handler so that after invoking it we
        // dismiss the menu (which plays the out animation + clears state).
        //
        // Exception: `sticky` menus keep the menu open after an item click
        // so the user can toggle multiple items in a row — the toolbar's
        // slot-visibility menu uses this. Sticky menus still dismiss on
        // backdrop click (handled in `render_context_menu` via the overlay
        // backdrop's `on_click`).
        if !sticky {
            for item in &mut state.items {
                if item.disabled {
                    continue;
                }
                let user_cb = item.on_click.take();
                let entity = entity.clone();
                item.on_click = Some(Rc::new(move |w, cx| {
                    if let Some(cb) = user_cb.as_ref() {
                        cb(w, cx);
                    }
                    let _ = entity.update(cx, |this, cx| this.begin_dismiss(cx));
                }));
            }
        }

        // Bump generation so any in-flight dismiss task becomes stale and
        // won't clobber this new menu.
        self.generation = self.generation.wrapping_add(1);
        state.open = true;
        self.state = Some(state);
        // Reset the gpui-animation transition state for the overlay + menu
        // card so the new invocation animates in from scratch. Without
        // this, a second right-click after a dismiss leaves the transition
        // state stuck at the dismiss endpoint (opacity 0), and the menu
        // either doesn't animate in or renders at the wrong position.
        gpui_animation::reset_transition(&ElementId::Name("context-menu-overlay".into()));
        gpui_animation::reset_transition(&ElementId::Name("context-menu".into()));
        cx.notify();
    }

    /// Begin the dismiss animation. Called from the wrapped item click
    /// handlers and from the backdrop click handler.
    pub fn begin_dismiss(&mut self, cx: &mut Context<Self>) {
        if let Some(s) = self.state.as_mut() {
            if s.open {
                s.open = false;
                cx.notify();
            }
        }

        let entity = cx.entity().downgrade();
        let dismiss_gen = self.generation;
        cx.spawn(async move |_this, cx| {
            smol::Timer::after(std::time::Duration::from_millis(CONTEXT_MENU_DISMISS_MS)).await;
            let _ = entity.update(cx, |this, cx| {
                // Only clear if no new menu has been shown in the meantime.
                if this.generation == dismiss_gen {
                    this.state = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Returns `true` when a menu is currently visible (showing or dismissing).
    pub fn is_active(&self) -> bool {
        self.state.is_some()
    }

    /// Replace the current menu's items in-place, without replaying the
    /// open/close animation. Used by **sticky** menus (e.g. the toolbar's
    /// slot-visibility menu) to update checkmarks after a click without
    /// the flicker of a full re-show.
    ///
    /// No-op if no menu is currently showing. Also re-wraps the new items'
    /// click handlers with the dismiss-after-click behavior — unless the
    /// current state is `sticky`, in which case the items are kept as-is
    /// (sticky menus stay open after a click).
    pub fn replace_items(&mut self, mut new_items: Vec<ContextMenuItem>, cx: &mut Context<Self>) {
        let Some(s) = self.state.as_mut() else {
            return;
        };
        let sticky = s.sticky;
        let entity = cx.entity().downgrade();
        // Apply the same click-handler wrapping as `show` does — sticky
        // menus skip the dismiss-after-click wrapper.
        if !sticky {
            for item in &mut new_items {
                if item.disabled {
                    continue;
                }
                let user_cb = item.on_click.take();
                let entity = entity.clone();
                item.on_click = Some(Rc::new(move |w, cx| {
                    if let Some(cb) = user_cb.as_ref() {
                        cb(w, cx);
                    }
                    let _ = entity.update(cx, |this, cx| this.begin_dismiss(cx));
                }));
            }
        }
        s.items = new_items;
        cx.notify();
    }
}

impl Render for ContextMenuController {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(state) = self.state.clone() else {
            return div().into_any_element();
        };
        // Build a backdrop-dismiss closure that bounces back into the
        // controller via a weak entity handle. This is the cleanest way
        // to dismiss on backdrop click without threading a controller
        // reference through every render helper.
        let weak = cx.entity().downgrade();
        let on_backdrop_click = Rc::new(move |_e: &ClickEvent, _w: &mut Window, cx: &mut App| {
            let _ = weak.update(cx, |this, cx| this.begin_dismiss(cx));
        });
        // Clamp the menu position so the card stays inside the window:
        // if the click point is too close to the bottom/right edge we
        // flip the menu to open above/to-the-left of the cursor. We
        // don't know the menu's exact rendered size here (it depends on
        // item count + label length), so we estimate using a fixed width
        // (200px, matching `.w(px(200.0))` in `render_context_menu`) and
        // a per-item height of ~24px plus header/padding. The estimate
        // only needs to be good enough to keep the card on screen —
        // overflow is clipped anyway, so off-by-a-few-pixels is fine.
        let viewport = window.viewport_size();
        let win_w = f32::from(viewport.width);
        let win_h = f32::from(viewport.height);
        const MENU_WIDTH: f32 = 200.0;
        const MENU_ITEM_H: f32 = 24.0;
        const MENU_HEADER_H: f32 = 26.0;
        const MENU_PADDING: f32 = 8.0; // p_1 top + bottom
        let estimated_h = MENU_PADDING
            + if state.header.is_some() {
                MENU_HEADER_H + 4.0
            } else {
                0.0
            }
            + state.items.len() as f32 * MENU_ITEM_H;
        let pos = state.position;
        let mut x = f32::from(pos.x);
        let mut y = f32::from(pos.y);
        // Flip left if the card would overflow the right edge.
        if x + MENU_WIDTH > win_w {
            x = (win_w - MENU_WIDTH).max(0.0);
        }
        // Flip up if the card would overflow the bottom edge.
        if y + estimated_h > win_h {
            y = (y - estimated_h).max(0.0);
        }
        let clamped_pos = point(px(x), px(y));
        render_context_menu(state, clamped_pos, Some(on_backdrop_click)).into_any_element()
    }
}

// ---------------------------------------------------------------------------
// Render helpers
// ---------------------------------------------------------------------------

fn render_context_menu(
    state: ContextMenuState,
    position: Point<Pixels>,
    on_backdrop_click: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
) -> impl IntoElement {
    let open = state.open;
    let header = state.header.clone();
    let items = state.items.clone();

    let overlay_id = ElementId::Name("context-menu-overlay".into());
    let menu_id = ElementId::Name("context-menu".into());

    div()
        .id(overlay_id.clone())
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        // Only capture clicks while open so the menu doesn't block the app
        // while it's animating out / hidden.
        .when(open, |el| {
            el.occlude().when_some(on_backdrop_click, |el, cb| {
                el.on_click(move |e, w, cx| {
                    cb(e, w, cx);
                })
            })
        })
        .with_transition(overlay_id)
        .transition_when_else(
            open,
            DURATION_BASE,
            EASE_STANDARD,
            |el| el.bg(rgba(0x00000000)),
            |el| el.bg(rgba(0x00000000)),
        )
        .child(
            div()
                .id(menu_id.clone())
                .absolute()
                .top(position.y)
                .left(position.x)
                // Constrain width so long labels wrap nicely.
                .w(px(200.0))
                .bg(rgb(bg_base()))
                .border_1()
                .border_color(rgb(border()))
                .rounded(RADIUS_MD)
                .shadow_lg()
                .flex()
                .flex_col()
                .p_1()
                .gap_0p5()
                .overflow_hidden()
                // Initial hidden state; the transition animates these to
                // visible. A subtle scale (via opacity + translate) gives
                // the menu a "pop in" feel.
                .opacity(0.0)
                .mt(px(-4.0))
                .with_transition(menu_id)
                .transition_when_else(
                    open,
                    DURATION_BASE,
                    EASE_STANDARD,
                    |el| el.opacity(1.0).mt_0(),
                    |el| el.opacity(0.0).mt(px(-4.0)),
                )
                // Stop clicks on the menu card from bubbling up to the
                // backdrop (which would dismiss the menu). Item clicks still
                // fire normally because they sit inside this card.
                .when(open, |el| {
                    el.on_click(|_e, _w, cx| {
                        cx.stop_propagation();
                    })
                })
                .when_some(header, |el, h| {
                    el.child(
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(rgb(text_muted()))
                            .child(h.to_string()),
                    )
                    .child(div().mx_1().my_0p5().h(px(1.0)).bg(rgb(border())))
                })
                .children(
                    items
                        .into_iter()
                        .enumerate()
                        .map(|(idx, item)| render_menu_item(idx, item)),
                ),
        )
}

fn render_menu_item(idx: usize, item: ContextMenuItem) -> impl IntoElement {
    let label = item.label.clone();
    let icon = item.icon.clone();
    let disabled = item.disabled;
    let danger = item.danger;
    let divider_after = item.divider_after;
    let on_click = item.on_click.clone();

    // Resolve the label color for the *current* state. NOTE: this value is
    // also driven through `transition_when_else` below (not just set
    // statically via `.text_color(...)`). `with_transition(row_id)` caches
    // the element's style state on first render and replays `state.cur` on
    // every subsequent render, overwriting any statically-set color — so a
    // menu item that was disabled the first time it rendered (e.g. Edit on
    // a running tunnel) would stay grey even after the menu is re-shown for
    // a stopped tunnel, because the cache holds the old `text_muted`
    // value. Driving it through `transition_when_else` makes the library
    // aware of both branches and re-evaluates them on each render.
    let primary_color = text_primary();
    let muted_color = text_muted();
    let danger_color = term_red();
    let label_color = if disabled {
        muted_color
    } else if danger {
        danger_color
    } else {
        primary_color
    };
    let hover_bg = rgba((surface_hover() << 8) | 0xFF);
    let rest_bg = rgba((surface_hover() << 8) | 0x00);

    let row_id = ElementId::Name(format!("ctx-item-{}", idx).into());
    let row = div()
        .id(row_id.clone())
        .flex()
        .flex_row()
        .items_center()
        .gap_1p5()
        .px_2()
        .py_0p5()
        .rounded(RADIUS_SM)
        .text_xs()
        .bg(rest_bg)
        .when(!disabled, |el| {
            el.when_some(on_click, |el, cb| {
                el.on_click(move |_e, w, cx| {
                    cb(w, cx);
                })
            })
        })
        .when(disabled, |el| el.cursor_not_allowed())
        .when_some(icon, |el, path| {
            el.child(
                svg()
                    .path(path)
                    .size(px(12.0))
                    .flex_shrink_0()
                    .text_color(rgb(label_color)),
            )
        })
        .child(div().flex_1().min_w_0().child(label.to_string()))
        .with_transition(row_id)
        // Hover background fade — uses `transition_on_hover` so the bg
        // eases in/out on mouse enter/leave. `DURATION_BASE` (150ms) is
        // deliberately slightly slower than the old `DURATION_FAST`
        // (100ms) so the hover feels perceptible rather than snapping.
        .transition_on_hover(DURATION_BASE, EASE_STANDARD, move |hovered, el| {
            if *hovered {
                el.bg(hover_bg)
            } else {
                el.bg(rest_bg)
            }
        })
        // Drive text_color through the transition system so the cached
        // style state updates when `disabled` / `danger` change between
        // successive menu shows for different rows (see note above).
        // `transition_when_else` with identical durations on both branches
        // means no visible animation — the color just snaps to the right
        // value each render.
        .transition_when_else(
            disabled,
            DURATION_INSTANT,
            EASE_STANDARD,
            move |state| state.text_color(rgb(muted_color)),
            move |state| state.text_color(rgb(label_color)),
        );

    if divider_after {
        div()
            .child(row)
            .child(div().mx_1().my_0p5().h(px(1.0)).bg(rgb(border())))
            .into_any_element()
    } else {
        row.into_any_element()
    }
}
