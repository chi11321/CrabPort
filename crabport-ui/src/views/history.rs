//! Connection-history page — the full-page sidebar view listing past
//! connection events (one row per attempt, most-recent-first).
//!
//! Listed when the sidebar's "History" item is active. Mirrors
//! [`crate::views::snippets::SnippetsView`]: an `Entity` view held in
//! `AppCtx`, refreshed via `set_state` before each render, reading its
//! rows straight from the Store's `connection_history` table. The view's
//! own state is just the hovered row (driving the hover transition).
//!
//! The list is virtualized via [`uniform_list`] — only the visible rows
//! are built each frame. That requires every row to have the same height,
//! so the failure message renders inline on the second line (next to the
//! target) instead of adding a third line.
//!
//! ```text
//! ┌───────────────────────────────────────────────┐
//! │ History                             [Clear]   │
//! │ ───────────────────────────────────────────── │
//! │ ● name [SSH]                        Success   │  ← right-click: Delete
//! │   user@host:22            2026-07-26 12:00:00 │
//! │ ● name [Telnet]                       Failed  │
//! │   user@host:23 · refused  2026-07-26 11:58:00 │
//! └───────────────────────────────────────────────┘
//! ```

use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::TransitionExt;
use rust_i18n::t;

use crabport_core::credential::HostKind;
use crabport_core::store::{ConnectionEvent, ConnectionStatus};

use crate::app_state::AppState;
use crate::color::*;
use crate::components::button::Button;
use crate::components::context_menu::{ContextMenuController, ContextMenuItem, ContextMenuState};
use crate::components::dialog::{AlertController, AlertSeverity, AlertState};
use crate::motion::{EASE_STANDARD, RADIUS_MD, duration_fast};

/// Fixed height of one list row (incl. the 4 px gap below it). Required
/// by [`uniform_list`], which measures one row and assumes the rest are
/// identical.
const ROW_HEIGHT: f32 = 60.0;

/// Absolute local time for a unix-seconds timestamp. Uses `chrono` so the
/// user's timezone + DST are respected.
fn format_time(secs: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(t) => t.format("%Y-%m-%d %H:%M:%S").to_string(),
        _ => "—".to_string(),
    }
}

/// Short protocol label for the kind chip.
fn kind_label(kind: HostKind) -> &'static str {
    match kind {
        HostKind::Ssh => "SSH",
        HostKind::Telnet => "Telnet",
        HostKind::Serial => "Serial",
    }
}

/// The `user@host:port` target line. Serial events carry the device path
/// in `address` and have no port/username, so the address stands alone.
fn target_line(e: &ConnectionEvent) -> String {
    if e.kind == HostKind::Serial {
        return e.address.clone();
    }
    if e.username.is_empty() {
        format!("{}:{}", e.address, e.port)
    } else {
        format!("{}@{}:{}", e.username, e.address, e.port)
    }
}

/// Connection-history management view.
pub struct HistoryView {
    /// The event row currently being hovered, if any. Drives the row's
    /// hover-background transition.
    hovered_event_id: Option<i64>,
    /// The event row that triggered the currently-open context menu.
    /// Keeps that row highlighted while the menu is open, even though the
    /// pointer has moved onto the menu (mirrors `SnippetsView`).
    context_menu_event_id: Option<i64>,
    /// Global context menu host (right-click Delete).
    context_menu: Option<Entity<ContextMenuController>>,
    /// Global alert dialog host (clear-all confirmation).
    alert_controller: Option<Entity<AlertController>>,
}

impl HistoryView {
    pub fn new() -> Self {
        Self {
            hovered_event_id: None,
            context_menu_event_id: None,
            context_menu: None,
            alert_controller: None,
        }
    }

    /// Push the latest external state into the view before render
    /// (mirrors the other sidebar views — called from `render_content`).
    pub fn set_state(
        &mut self,
        context_menu: Entity<ContextMenuController>,
        alert_controller: Entity<AlertController>,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = Some(context_menu);
        self.alert_controller = Some(alert_controller);
        let _ = cx;
    }
}

impl Default for HistoryView {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for HistoryView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let events = AppState::store(cx)
            .lock()
            .connection_history()
            .unwrap_or_default();
        let count = events.len();

        // Clear stale hover if the event disappeared (deleted / evicted).
        if let Some(id) = self.hovered_event_id
            && !events.iter().any(|e| e.id == id)
        {
            self.hovered_event_id = None;
        }
        let hovered_event_id = self.hovered_event_id;

        // Clear the stale context-menu highlight once the menu closed.
        let menu_active = self
            .context_menu
            .as_ref()
            .map(|cm| cm.read_with(cx, |c, _| c.is_active()))
            .unwrap_or(false);
        if !menu_active {
            self.context_menu_event_id = None;
        }
        let context_menu_event_id = self.context_menu_event_id;

        // --- Clear-all button (confirmation alert, then wipe + repaint) ---
        let alert_controller = self.alert_controller.clone();
        let entity_for_clear = cx.entity().downgrade();
        let clear_btn = Button::new("connection-history-clear-btn")
            .ghost()
            .icon("icons/trash.svg")
            .w_auto()
            .px_2()
            .child(t!("connection_history.clear").to_string())
            .on_click(move |_e, _w, cx| {
                let Some(ref ac) = alert_controller else {
                    return;
                };
                let entity = entity_for_clear.clone();
                ac.update(cx, |c, cx| {
                    c.show(
                        AlertState {
                            severity: AlertSeverity::Danger,
                            title: t!("connection_history.clear_title").to_string().into(),
                            description: Some(
                                t!("connection_history.clear_prompt").to_string().into(),
                            ),
                            confirm_label: t!("connection_history.clear_confirm")
                                .to_string()
                                .into(),
                            cancel_label: t!("terminal.host_key_cancel").to_string().into(),
                            on_confirm: Some(Rc::new(move |_w, cx| {
                                let _ = AppState::store(cx).lock().clear_connection_history();
                                let _ = entity.update(cx, |_, cx| cx.notify());
                            })),
                            ..AlertState::default()
                        },
                        cx,
                    );
                });
            });

        let entity = cx.entity().downgrade();
        let context_menu = self.context_menu.clone();

        div()
            .size_full()
            .flex()
            .flex_col()
            // --- Header: title + Clear button ---
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .pt_4()
                    .pb_2()
                    .text_sm()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(text_primary()))
                            .child(t!("sidebar.history").to_string()),
                    )
                    .when(count > 0, |el| el.child(clear_btn)),
            )
            // --- Separator ---
            .child(div().h_px().bg(rgb(border())).mx_4())
            // --- Event list (virtualized) or empty state ---
            .child(div().flex_1().min_h_0().px_4().py_2().when_else(
                count == 0,
                |el| {
                    el.flex().items_center().justify_center().child(
                        div()
                            .text_color(rgb(text_muted()))
                            .text_sm()
                            .child(t!("connection_history.empty").to_string()),
                    )
                },
                |el| {
                    // Only the visible rows are constructed each frame; the
                    // scroll offset persists across renders via the element id.
                    el.child(
                        uniform_list(
                            "connection-history-list",
                            count,
                            move |range, _window, _cx| {
                                range
                                    .map(|i| {
                                        history_row(
                                            &events[i],
                                            hovered_event_id == Some(events[i].id),
                                            context_menu_event_id == Some(events[i].id),
                                            entity.clone(),
                                            context_menu.clone(),
                                        )
                                        .into_any_element()
                                    })
                                    .collect()
                            },
                        )
                        .size_full(),
                    )
                },
            ))
    }
}

/// One fixed-height connection-event row. Hover eases the background via
/// the shared transition machinery; right-click opens a Delete menu (the
/// row stays highlighted while its menu is open via `force_highlight`).
fn history_row(
    event: &ConnectionEvent,
    is_hovered: bool,
    force_highlight: bool,
    entity: WeakEntity<HistoryView>,
    context_menu: Option<Entity<ContextMenuController>>,
) -> impl IntoElement {
    let ok = event.status == ConnectionStatus::Success;
    let status_color = if ok { term_green() } else { term_red() };
    let status_label = if ok {
        t!("connection_history.status_success").to_string()
    } else {
        t!("connection_history.status_failed").to_string()
    };
    let event_id = event.id;
    let row_id = ElementId::Name(format!("connection-history-row-{event_id}").into());
    let is_highlighted = is_hovered || force_highlight;
    let entity_for_hover = entity.clone();

    // Outer wrapper owns the fixed row height + inter-row gap so the inner
    // card can simply fill it.
    div().h(px(ROW_HEIGHT)).pb_1().child(
        div()
            .id(row_id.clone())
            .size_full()
            .flex()
            .flex_col()
            .justify_center()
            .gap_0p5()
            .px_3()
            .rounded(RADIUS_MD)
            .bg(rgb(bg_base()))
            // Right-click: Delete this event. (Interactive handlers must be
            // attached before `with_transition` wraps the element.)
            .on_mouse_down(MouseButton::Right, move |mouse_event, _w, cx| {
                let Some(ref cm) = context_menu else {
                    return;
                };
                // Keep this row highlighted while its menu is open.
                let _ = entity.update(cx, |view, cx| {
                    view.context_menu_event_id = Some(event_id);
                    cx.notify();
                });
                let pos = mouse_event.position;
                let entity = entity.clone();
                cm.update(cx, |c, cx| {
                    c.show(
                        ContextMenuState {
                            position: pos,
                            items: vec![
                                ContextMenuItem::new(
                                    t!("connection_history.delete").to_string(),
                                    {
                                        move |_w, cx| {
                                            let _ = AppState::store(cx)
                                                .lock()
                                                .remove_connection_event(event_id);
                                            let _ = entity.update(cx, |_, cx| cx.notify());
                                        }
                                    },
                                )
                                .danger(true),
                            ],
                            ..ContextMenuState::default()
                        },
                        cx,
                    );
                });
            })
            // Track hover of the whole row; the transition below eases the
            // background between the base and highlighted colors (same
            // pattern as `snippet_row`).
            .with_transition(row_id)
            .on_hover(move |hovered, _w, cx| {
                let _ = entity_for_hover.update(cx, |view, cx| {
                    if *hovered {
                        view.hovered_event_id = Some(event_id);
                    } else if view.hovered_event_id == Some(event_id) {
                        view.hovered_event_id = None;
                    }
                    cx.notify();
                });
            })
            .transition_when_else(
                is_highlighted,
                duration_fast(),
                EASE_STANDARD,
                |el| el.bg(rgb(surface_active())),
                |el| el.bg(rgb(bg_base())),
            )
            // Line 1: status dot + name + kind chip … status label.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .size(px(8.0))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(rgb(status_color)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(text_primary()))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .min_w_0()
                            .child(event.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .px_1p5()
                            .rounded(px(4.0))
                            .bg(rgb(surface_active()))
                            .text_color(rgb(text_muted()))
                            .flex_shrink_0()
                            .child(kind_label(event.kind)),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(status_color))
                            .flex_shrink_0()
                            .child(status_label),
                    ),
            )
            // Line 2: target (+ inline error when failed) … timestamp.
            // Indented by dot width + gap so it aligns with the name.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .pl(px(16.0))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_muted()))
                            .whitespace_nowrap()
                            .flex_shrink_0()
                            .child(target_line(event)),
                    )
                    .when_some(event.error.clone(), |el, err| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(rgb(term_red()))
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .min_w_0()
                                .child(err),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(text_muted()))
                            .flex_shrink_0()
                            .child(format_time(event.created_at)),
                    ),
            ),
    )
}
