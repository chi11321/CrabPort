//! Tunnels panel — a side panel listing saved tunnel configs.
//!
//! Sibling of [`super::sftp::SftpPanel`] / [`super::snippets_panel`]:
//! renders inside the right-hand panel strip's "Tunnels" tab (see
//! `crabport-ui/src/layouts/panel.rs`).
//!
//! Unlike the full-page [`crate::views::tunnels::TunnelsView`], which starts
//! tunnels via a dedicated owned SSH connection ([`CrabportApp::start_tunnel_owned`]),
//! this panel starts **borrowed** tunnels that reuse the active terminal
//! tab's SSH connection ([`CrabportApp::start_tunnel_borrowed`]). The two
//! paths are mutually exclusive: a tunnel can run from only one source at a
//! time, enforced by [`crate::views::tunnels::TunnelRegistry::is_running`].
//!
//! Interactions:
//! - **Double-click** a row → toggle the tunnel: start (borrowed from the
//!   active tab) if stopped, stop if running.
//! - **Right-click** a row → context menu with Stop (when running) or Start.
//!
//! The panel is only useful for SSH tabs (local PTY backends expose no tunnel
//! source), so `render_content` only wires callbacks when the active tab is
//! remote.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::{animation::TransitionExt, transition::general::Linear};
use gpui_component::input::InputState;
use gpui_component::label::Label;
use gpui_component::scroll::Scrollbar;
use gpui_component::scroll::ScrollbarShow;
use gpui_component::{VirtualListScrollHandle, v_virtual_list};
use rust_i18n::t;

use crate::app::CrabportApp;
use crate::color::*;
use crate::components::context_menu::{ContextMenuController, ContextMenuItem, ContextMenuState};
use crate::components::input::StyledInput;
use crate::views::tunnels::TunnelView;

/// Color accents for the kind badge (mirrors the Tunnels page). Read live
/// from the theme so a preset switch recolors the badges too.
fn kind_local_color() -> u32 {
    term_blue()
}
fn kind_remote_color() -> u32 {
    term_magenta()
}
fn kind_dynamic_color() -> u32 {
    term_yellow()
}
fn status_running_color() -> u32 {
    term_green()
}
fn status_stopped_color() -> u32 {
    text_muted()
}

/// Per-tab state held by the tunnels panel so each terminal connection
/// keeps its own search query, hover, and context-menu-triggering row.
/// The panel swaps this state in/out when the active tab changes.
struct TabPanelState {
    search_query: String,
    hovered_row: Option<usize>,
    /// The tunnel row that triggered the currently-open context menu, if
    /// any. While set, that row stays highlighted even though the mouse
    /// has moved to the overlay.
    context_menu_row: Option<usize>,
}

impl Default for TabPanelState {
    fn default() -> Self {
        Self {
            search_query: String::new(),
            hovered_row: None,
            context_menu_row: None,
        }
    }
}

/// Tunnels panel view.
pub struct TunnelsPanel {
    /// Current tunnel list snapshot. Reloaded from the registry on each
    /// `set_state` call.
    tunnels: Arc<Vec<TunnelView>>,
    /// App entity handle, used to drive favorite toggles from the panel
    /// (the panel's `set_state` callbacks are wired from `content.rs` and
    /// can't grow new params without touching that file, so the panel
    /// reaches the app directly via this handle for the star toggle).
    app: Option<Entity<CrabportApp>>,
    /// Start callback — invoked with `(tunnel_id, tab_id)` on double-click /
    /// context-menu "Start". Routes to `CrabportApp::start_tunnel_borrowed`.
    on_start: Option<Rc<dyn Fn(i64, &mut App)>>,
    /// Stop callback — invoked with `tunnel_id` on context-menu "Stop".
    /// Routes to `CrabportApp::stop_tunnel`.
    on_stop: Option<Rc<dyn Fn(i64, &mut App)>>,
    /// Search input state (lazily initialized on the first `set_state`).
    /// Shared across tabs — its visible text is resynced to the active
    /// tab's `search_query` on tab switch via `InputState::set_value`.
    search_input: Option<Entity<InputState>>,
    /// Scroll handle for the virtual list + custom scrollbar.
    scroll_handle: VirtualListScrollHandle,
    /// Global context menu host. Held so the panel can open a right-click
    /// menu on rows (Start / Stop).
    context_menu: Option<Entity<ContextMenuController>>,
    /// The tab whose state is currently surfaced in the panel. `None`
    /// before the first `set_state` (e.g. on the Home tab).
    active_tab_id: Option<u64>,
    /// Per-tab panel state. Each terminal connection keeps its own search
    /// query + hover/context-menu row so switching tabs doesn't bleed one
    /// connection's view state into another.
    tab_states: HashMap<u64, TabPanelState>,
}

impl TunnelsPanel {
    pub fn new() -> Self {
        Self {
            tunnels: Arc::new(Vec::new()),
            app: None,
            on_start: None,
            on_stop: None,
            search_input: None,
            scroll_handle: VirtualListScrollHandle::new(),
            context_menu: None,
            active_tab_id: None,
            tab_states: HashMap::new(),
        }
    }

    /// Update the tunnel list + callbacks from the active context.
    /// Called by the content layout each render. `tunnels` is the live
    /// snapshot from `TunnelRegistry::list()` so running state is current.
    ///
    /// `tab_id` identifies the active terminal tab so the panel can keep
    /// per-tab search query / hover / context-menu-row state independent
    /// across terminal connections.
    #[allow(clippy::too_many_arguments)]
    pub fn set_state(
        &mut self,
        tunnels: Vec<TunnelView>,
        on_start: Option<Rc<dyn Fn(i64, &mut App)>>,
        on_stop: Option<Rc<dyn Fn(i64, &mut App)>>,
        context_menu: Entity<ContextMenuController>,
        tab_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Lazily init the search InputState on the first call.
        if self.search_input.is_none() {
            let entity = cx
                .new(|cx| InputState::new(window, cx).placeholder(t!("panel.search").to_string()));
            cx.subscribe(
                &entity,
                |this, input, event: &gpui_component::input::InputEvent, cx| {
                    if let gpui_component::input::InputEvent::Change { .. } = event {
                        // Write into the active tab's state so each
                        // connection keeps its own query.
                        if let Some(id) = this.active_tab_id {
                            let q = input.read(cx).value().to_string();
                            this.tab_states.entry(id).or_default().search_query = q;
                            cx.notify();
                        }
                    }
                },
            )
            .detach();
            self.search_input = Some(entity);
        }

        // Switch the active tab's state. When the tab changes, resync the
        // shared search input to the new tab's query so the field shows the
        // right text. `set_value` does not emit `InputEvent::Change`, so the
        // subscription above won't double-write.
        let tab_changed = self.active_tab_id != Some(tab_id);
        if tab_changed {
            self.active_tab_id = Some(tab_id);
            let new_query = self
                .tab_states
                .entry(tab_id)
                .or_default()
                .search_query
                .clone();
            if let Some(ref input) = self.search_input {
                input.update(cx, |state, cx| {
                    state.set_value(new_query, window, cx);
                });
            }
            // Clear stale hover/context-menu highlight from the previously
            // active tab so it doesn't leak into the new tab's render.
            cx.notify();
        }

        let new_tunnels = Arc::new(tunnels);
        let changed = !Arc::ptr_eq(&self.tunnels, &new_tunnels);
        self.tunnels = new_tunnels;
        self.on_start = on_start;
        self.on_stop = on_stop;
        self.context_menu = Some(context_menu);
        if changed {
            cx.notify();
        }
    }

    /// Drop the per-tab state for a closing tab so the `tab_states` map
    /// doesn't leak entries for closed tabs. Called from `close_tab`.
    pub fn forget_tab(&mut self, tab_id: u64) {
        self.tab_states.remove(&tab_id);
        if self.active_tab_id == Some(tab_id) {
            self.active_tab_id = None;
        }
    }

    /// The filtered view of `self.tunnels` for the active tab's search query.
    /// Case-insensitive substring match on name + bind address.
    fn filtered(&self) -> Vec<usize> {
        let q = self
            .active_tab_id
            .and_then(|id| self.tab_states.get(&id))
            .map(|s| s.search_query.trim().to_lowercase())
            .unwrap_or_default();
        if q.is_empty() {
            return (0..self.tunnels.len()).collect();
        }
        self.tunnels
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.name.to_lowercase().contains(&q)
                    || t.bind_addr.to_lowercase().contains(&q)
                    || t.bind_port.to_string().contains(&q)
            })
            .map(|(i, _)| i)
            .collect()
    }
}

impl Default for TunnelsPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl TunnelsPanel {
    /// Set the app entity handle (called once from `CrabportApp::wire` so the
    /// panel can drive favorite toggles without a new `set_state` param).
    pub fn set_app(&mut self, app: Entity<CrabportApp>) {
        self.app = Some(app);
    }
}

/// Fixed height of each tunnel row. The virtual list requires uniform
/// item sizes.
const ROW_HEIGHT: f32 = 36.0;

impl Render for TunnelsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let search_input = self.search_input.clone();
        let on_start = self.on_start.clone();
        let on_stop = self.on_stop.clone();
        let scroll_handle = self.scroll_handle.clone();
        let context_menu = self.context_menu.clone();

        // Compute the filtered list + per-row data once per render.
        let filtered_indices = self.filtered();
        let filtered: Vec<TunnelView> = filtered_indices
            .iter()
            .map(|&i| self.tunnels[i].clone())
            .collect();
        let hovered_row = self
            .active_tab_id
            .and_then(|id| self.tab_states.get(&id))
            .and_then(|s| s.hovered_row);
        let context_menu_row = self
            .active_tab_id
            .and_then(|id| self.tab_states.get(&id))
            .and_then(|s| s.context_menu_row);

        // If the global context menu is no longer active, clear the
        // "menu-triggering row" highlight for the active tab.
        let menu_active = self
            .context_menu
            .as_ref()
            .map(|cm| cm.read_with(cx, |c, _| c.is_active()))
            .unwrap_or(false);
        if !menu_active {
            if let Some(id) = self.active_tab_id {
                if let Some(state) = self.tab_states.get_mut(&id) {
                    state.context_menu_row = None;
                }
            }
        }

        // Pre-compute item sizes for the virtual list.
        let item_sizes = Rc::new(
            (0..filtered.len())
                .map(|_| Size {
                    width: px(0.0),
                    height: px(ROW_HEIGHT),
                })
                .collect::<Vec<_>>(),
        );
        let filtered_for_list = Arc::new(filtered);
        let is_empty = filtered_for_list.is_empty();

        let app_for_list = self.app.clone();

        let list = v_virtual_list(
            cx.entity(),
            "tunnel-panel-list",
            item_sizes,
            move |_this, range, _window, cx| {
                let filtered = &filtered_for_list;
                let on_start = on_start.clone();
                let on_stop = on_stop.clone();
                let context_menu = context_menu.clone();
                let app = app_for_list.clone();
                let entity = cx.entity().downgrade();
                range
                    .map(|i| {
                        let t = &filtered[i];
                        let tunnel = t.clone();
                        let is_hovered = hovered_row == Some(i);
                        let force_highlight = context_menu_row == Some(i);
                        let is_highlighted = is_hovered || force_highlight;
                        let row_id = ElementId::Name(format!("tunnel-panel-{i}").into());
                        let row_id_for_transition = row_id.clone();

                        // Kind badge accent + letter.
                        let (kind_letter, kind_color) = match t.kind {
                            crabport_core::credential::TunnelKind::Local => {
                                ("L", kind_local_color())
                            }
                            crabport_core::credential::TunnelKind::Remote => {
                                ("R", kind_remote_color())
                            }
                            crabport_core::credential::TunnelKind::Dynamic => {
                                ("D", kind_dynamic_color())
                            }
                        };

                        // Address line (compact for the narrow panel).
                        let bind_display = if t.bind_addr.is_empty() {
                            format!("*:{}", t.bind_port)
                        } else {
                            format!("{}:{}", t.bind_addr, t.bind_port)
                        };
                        let address_line = match t.kind {
                            crabport_core::credential::TunnelKind::Local
                            | crabport_core::credential::TunnelKind::Remote => {
                                format!("{} → {}:{}", bind_display, t.target_host, t.target_port)
                            }
                            crabport_core::credential::TunnelKind::Dynamic => {
                                format!("{} (SOCKS5)", bind_display)
                            }
                        };

                        // Status dot color.
                        let status_dot = if t.running {
                            status_running_color()
                        } else {
                            status_stopped_color()
                        };

                        let on_start_for_row = on_start.clone();
                        let on_stop_for_row = on_stop.clone();
                        let context_menu_for_row = context_menu.clone();
                        let entity_for_menu = entity.clone();
                        let tunnel_for_menu = tunnel.clone();

                        // Capture the running flag for the double-click
                        // toggle below — `tunnel` is moved into the
                        // right-click closure after this.
                        let running_for_dblclick = tunnel.running;
                        let tunnel_id_for_dblclick = tunnel.id;
                        let favorite_for_star = t.favorite;
                        let tunnel_id_for_star = tunnel.id;
                        let app_for_star = app.clone();

                        div()
                            .id(row_id.clone())
                            .h(px(ROW_HEIGHT))
                            .w_full()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1p5()
                            .px_2()
                            .rounded(px(4.0))
                            // Double-click toggles start/stop based on the
                            // current running state — mirrors the full-page
                            // TunnelsView. Must be on the pre-transition div;
                            // the AnimatedWrapper from `with_transition`
                            // doesn't expose `on_mouse_down`.
                            .on_mouse_down(MouseButton::Left, {
                                let on_start = on_start_for_row.clone();
                                let on_stop = on_stop_for_row.clone();
                                move |event, _w, cx| {
                                    if event.click_count >= 2 {
                                        if running_for_dblclick {
                                            if let Some(ref cb) = on_stop {
                                                cb(tunnel_id_for_dblclick, cx);
                                            }
                                        } else if let Some(ref cb) = on_start {
                                            cb(tunnel_id_for_dblclick, cx);
                                        }
                                    }
                                }
                            })
                            // Right-click context menu: Start / Stop
                            // (contextual on running state).
                            .on_mouse_down(MouseButton::Right, {
                                let on_start = on_start_for_row.clone();
                                let on_stop = on_stop_for_row.clone();
                                let cm = context_menu_for_row.clone();
                                let entity = entity_for_menu.clone();
                                let tunnel = tunnel_for_menu.clone();
                                move |event, _w, cx| {
                                    let Some(ref cm) = cm else {
                                        return;
                                    };
                                    let _ = entity.update(cx, |view, cx| {
                                        if let Some(id) = view.active_tab_id {
                                            view.tab_states
                                                .entry(id)
                                                .or_default()
                                                .context_menu_row = Some(i);
                                        }
                                        cx.notify();
                                    });
                                    let pos = event.position;
                                    let running = tunnel.running;
                                    let on_start = on_start.clone();
                                    let on_stop = on_stop.clone();
                                    let tunnel_id = tunnel.id;
                                    cm.update(cx, |c, cx| {
                                        let toggle_item = if running {
                                            ContextMenuItem::new(t!("tunnels.stop").to_string(), {
                                                let on_stop = on_stop.clone();
                                                move |_w, cx| {
                                                    if let Some(ref cb) = on_stop {
                                                        cb(tunnel_id, cx);
                                                    }
                                                }
                                            })
                                        } else {
                                            ContextMenuItem::new(t!("tunnels.start").to_string(), {
                                                let on_start = on_start.clone();
                                                move |_w, cx| {
                                                    if let Some(ref cb) = on_start {
                                                        cb(tunnel_id, cx);
                                                    }
                                                }
                                            })
                                        };
                                        c.show(
                                            ContextMenuState {
                                                position: pos,
                                                items: vec![toggle_item],
                                                ..ContextMenuState::default()
                                            },
                                            cx,
                                        );
                                    });
                                }
                            })
                            .with_transition(row_id_for_transition)
                            .on_hover({
                                let entity = entity.clone();
                                move |hovered, _w, cx| {
                                    let _ = entity.update(cx, |view, cx| {
                                        if let Some(id) = view.active_tab_id {
                                            let state = view.tab_states.entry(id).or_default();
                                            if *hovered {
                                                state.hovered_row = Some(i);
                                            } else if state.hovered_row == Some(i) {
                                                state.hovered_row = None;
                                            }
                                            cx.notify();
                                        }
                                    });
                                }
                            })
                            .transition_when_else(
                                is_highlighted,
                                std::time::Duration::from_millis(120),
                                Linear,
                                |el| el.bg(rgba((surface_hover() << 8) | 0x60)),
                                |el| el.bg(rgba((surface_hover() << 8) | 0x00)),
                            )
                            // Kind badge (single letter, color-coded).
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size_4()
                                    .rounded(px(3.0))
                                    .bg(rgba((kind_color << 8) | 0x22))
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(kind_color))
                                    .child(kind_letter.to_string()),
                            )
                            // Name + address (two-line, flex-1).
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(text_primary()))
                                            .whitespace_nowrap()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .child(Label::new(tunnel.name.clone())),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(text_muted()))
                                            .whitespace_nowrap()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .child(address_line),
                                    ),
                            )
                            // Status dot.
                            .child(div().size_2().rounded_full().bg(rgb(status_dot)))
                            // Favorite star toggle (far right, compact). Fades
                            // in on hover; stays visible (yellow) when already
                            // favorited so the user can see + unstar.
                            .child({
                                let app = app_for_star.clone();
                                let star_id =
                                    ElementId::Name(format!("tunnel-panel-star-{i}").into());
                                let star_visible = is_highlighted || favorite_for_star;
                                div()
                                    .id(star_id.clone())
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(svg().path("icons/star.svg").size(px(14.0)).text_color(
                                        rgb(if favorite_for_star {
                                            term_yellow()
                                        } else {
                                            text_muted()
                                        }),
                                    ))
                                    .with_transition(star_id)
                                    .transition_when_else(
                                        star_visible,
                                        std::time::Duration::from_millis(120),
                                        Linear,
                                        |el| el.opacity(1.0),
                                        |el| el.opacity(0.0),
                                    )
                                    .on_click({
                                        let app = app.clone();
                                        move |_e, _w, cx| {
                                            if let Some(app) = app.clone() {
                                                app.update(cx, |app, cx| {
                                                    app.toggle_tunnel_favorite(
                                                        tunnel_id_for_star,
                                                        cx,
                                                    );
                                                });
                                            }
                                        }
                                    })
                            })
                    })
                    .collect::<Vec<_>>()
            },
        )
        .track_scroll(&scroll_handle);

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
                el.child(
                    div().mb_1().child(
                        StyledInput::new("tunnel-panel-search", input)
                            .xsmall()
                            .prefix(
                                svg()
                                    .path("icons/search.svg")
                                    .size(px(12.0))
                                    .text_color(rgb(text_muted())),
                            ),
                    ),
                )
            })
            // List + scrollbar, or empty-state placeholder.
            .when(is_empty, |el| {
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
                                .child(t!("tunnels.panel_empty").to_string()),
                        ),
                )
            })
            .when(!is_empty, |el| {
                el.child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .border_1()
                        .border_color(rgb(border()))
                        .bg(rgb(bg_tab_bar()))
                        .rounded_md()
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
            })
    }
}
