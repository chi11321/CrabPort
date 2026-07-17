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
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::{animation::TransitionExt, transition::general::EaseInOutCubic};

use crate::color::*;

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
const CONTEXT_MENU_DISMISS_MS: u64 = 120;

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

        // Wrap each item's click handler so that after invoking it we
        // dismiss the menu (which plays the out animation + clears state).
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
}

impl Render for ContextMenuController {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        render_context_menu(state, Some(on_backdrop_click)).into_any_element()
    }
}

// ---------------------------------------------------------------------------
// Render helpers
// ---------------------------------------------------------------------------

fn render_context_menu(
    state: ContextMenuState,
    on_backdrop_click: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
) -> impl IntoElement {
    let open = state.open;
    let position = state.position;
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
            Duration::from_millis(120),
            EaseInOutCubic,
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
                .rounded_md()
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
                    Duration::from_millis(120),
                    EaseInOutCubic,
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

    let label_color = if disabled {
        text_muted()
    } else if danger {
        term_red()
    } else {
        text_primary()
    };

    let row_id = ElementId::Name(format!("ctx-item-{}", idx).into());
    let row = div()
        .id(row_id.clone())
        .flex()
        .flex_row()
        .items_center()
        .gap_1p5()
        .px_2()
        .py_0p5()
        .rounded(px(3.0))
        .text_xs()
        .text_color(rgb(label_color))
        .bg(rgba(0x00000000))
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
        .transition_on_hover(
            Duration::from_millis(100),
            EaseInOutCubic,
            move |hovered, el| {
                if *hovered {
                    el.bg(rgb(surface_hover()))
                } else {
                    el.bg(rgba(0x00000000))
                }
            },
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
