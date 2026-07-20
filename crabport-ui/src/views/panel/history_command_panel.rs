//! History-command panel — a side panel listing commands previously run in
//! the active terminal session.
//!
//! Sibling of [`super::sftp::SftpPanel`] / [`super::snippets_panel::SnippetsPanel`]:
//! renders inside the right-hand panel strip's "History" tab (see
//! `crabport-ui/src/layouts/panel.rs`).
//!
//! Layout:
//!
//! ```text
//! ┌─────────────────────────────┐
//! │ [search input]              │
//! ├─────────────────────────────┤
//! │ command_1          [⧉][↧]   │  ← buttons fade in on row hover
//! │ command_2          [⧉][↧]   │
//! │ ...                         │
//! └─────────────────────────────┘
//! ```
//!
//! Commands are captured by [`crabport_terminal::terminal::TerminalSession`]
//! (most-recent-first, deduped, capped at 1000) and pushed in via
//! `set_state` each render. The search field filters the list in real time.

use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::TransitionExt;
use gpui_component::input::InputState;
use gpui_component::label::Label;
use gpui_component::scroll::Scrollbar;
use gpui_component::scroll::ScrollbarShow;
use gpui_component::{VirtualListScrollHandle, v_virtual_list};
use rust_i18n::t;

use crate::color::*;
use crate::components::input::StyledInput;
use crate::components::notification::{Notification, NotificationLevel};
use crate::motion::{EASE_STANDARD, RADIUS_MD, duration_fast};

/// A single previously-run terminal command entry.
///
/// `command` is the literal text that was executed. `timestamp` is an
/// optional display string (e.g. "2 min ago") rendered muted under the
/// command — kept as a pre-formatted string so this view doesn't need to
/// know about time formatting.
#[derive(Clone, Debug)]
pub struct HistoryCommand {
    pub command: String,
    pub timestamp: Option<String>,
}

/// History-command panel view.
pub struct HistoryCommandPanel {
    history: Arc<Vec<HistoryCommand>>,
    on_paste: Option<Rc<dyn Fn(String, &mut App)>>,
    /// Triggered by the toolbar refresh button — asks the active terminal's
    /// backend to re-read the TTY history file and broadcast a
    /// `HistoryLoaded` event.
    on_refresh: Option<Rc<dyn Fn(&mut App)>>,
    notifications: Option<Entity<crate::components::notification::NotificationController>>,
    /// Global tooltip host for button hover tooltips.
    tooltip: Option<Entity<crate::components::tooltip::TooltipController>>,
    search_input: Option<Entity<InputState>>,
    search_query: String,
    scroll_handle: VirtualListScrollHandle,
    hovered_row: Option<usize>,
}

impl HistoryCommandPanel {
    pub fn new() -> Self {
        Self {
            history: Arc::new(Vec::new()),
            on_paste: None,
            on_refresh: None,
            notifications: None,
            tooltip: None,
            search_input: None,
            search_query: String::new(),
            scroll_handle: VirtualListScrollHandle::new(),
            hovered_row: None,
        }
    }

    /// Update the history list + paste callback from the active context.
    /// Called by the content layout each render (same pattern as
    /// `SftpPanel::set_state`).
    #[allow(dead_code)]
    pub fn set_state(
        &mut self,
        history: Arc<Vec<HistoryCommand>>,
        on_paste: Option<Rc<dyn Fn(String, &mut App)>>,
        on_refresh: Option<Rc<dyn Fn(&mut App)>>,
        notifications: Entity<crate::components::notification::NotificationController>,
        tooltip: Entity<crate::components::tooltip::TooltipController>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Lazily init the search InputState on the first call (needs a
        // Window). Subsequent calls just refresh the history + callback.
        let history_changed = !Arc::ptr_eq(&self.history, &history);
        if self.search_input.is_none() {
            let entity = cx
                .new(|cx| InputState::new(window, cx).placeholder(t!("panel.search").to_string()));
            // Re-filter on every keystroke.
            cx.subscribe(
                &entity,
                |this, input, event: &gpui_component::input::InputEvent, cx| {
                    if let gpui_component::input::InputEvent::Change { .. } = event {
                        this.search_query = input.read(cx).value().to_string();
                        cx.notify();
                    }
                },
            )
            .detach();
            self.search_input = Some(entity);
        }

        self.history = history;
        self.on_paste = on_paste;
        self.on_refresh = on_refresh;
        self.notifications = Some(notifications);
        self.tooltip = Some(tooltip);
        if history_changed {
            // History changed (e.g. user ran a new command) — the filtered
            // list may grow, so a repaint is needed.
            cx.notify();
        }
    }

    /// The filtered view of `self.history` for the current `search_query`.
    /// Case-insensitive substring match. Returns indices into the original
    /// list so we can clone the `HistoryCommand` cheaply.
    fn filtered(&self) -> Vec<usize> {
        let q = self.search_query.trim().to_lowercase();
        if q.is_empty() {
            return (0..self.history.len()).collect();
        }
        self.history
            .iter()
            .enumerate()
            .filter(|(_, h)| h.command.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect()
    }
}

impl Default for HistoryCommandPanel {
    fn default() -> Self {
        Self::new()
    }
}

/// Fixed height of each history row. The virtual list requires uniform
/// item sizes.
const ROW_HEIGHT: f32 = 28.0;

impl Render for HistoryCommandPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let search_input = self.search_input.clone();
        let on_paste = self.on_paste.clone();
        let on_refresh = self.on_refresh.clone();
        let notifications = self.notifications.clone();
        let tooltip = self.tooltip.clone();
        let scroll_handle = self.scroll_handle.clone();

        // Compute the filtered list + per-row data once per render.
        let filtered_indices = self.filtered();
        let filtered: Vec<HistoryCommand> = filtered_indices
            .iter()
            .map(|&i| self.history[i].clone())
            .collect();
        let hovered_row = self.hovered_row;

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

        // Clone for the search-bar refresh button (the list closure below
        // moves the outer `tooltip`).
        let tooltip_for_search = tooltip.clone();

        let list = v_virtual_list(
            cx.entity(),
            "history-cmd-list",
            item_sizes,
            move |_this, range, _window, cx| {
                let filtered = &filtered_for_list;
                let on_paste = on_paste.clone();
                let entity = cx.entity().downgrade();
                let notifications = notifications.clone();
                let tooltip = tooltip.clone();
                range
                    .map(|i| {
                        let h = &filtered[i];
                        let cmd = h.command.clone();
                        let is_hovered = hovered_row == Some(i);
                        let row_id = ElementId::Name(format!("history-cmd-{i}").into());
                        let row_id_for_transition = row_id.clone();

                        // Save button: persists the command as a snippet
                        // into the global Store so it shows up in the
                        // Snippets panel and survives restarts.
                        let cmd_for_save = cmd.clone();
                        let notifications = notifications.clone();
                        let tooltip_save = tooltip.clone();
                        let save_btn = div()
                            .id(ElementId::Name(format!("history-save-{i}").into()))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(20.0))
                            .rounded(px(4.0))
                            .bg(rgba(0x00000000))
                            .with_transition(ElementId::Name(format!("history-save-{i}").into()))
                            .on_hover(move |hovered, w, cx| {
                                if let Some(ref tc) = tooltip_save {
                                    if *hovered {
                                        tc.update(cx, |t, cx| {
                                            t.show(
                                                t!("panel.save_tooltip").to_string(),
                                                w.mouse_position(),
                                                cx,
                                            );
                                        });
                                    } else {
                                        tc.update(cx, |t, cx| {
                                            t.hide(cx);
                                        });
                                    }
                                }
                            })
                            .transition_on_hover(
                                duration_fast(),
                                EASE_STANDARD,
                                move |hovered, el| {
                                    if *hovered {
                                        el.bg(rgb(surface_hover()))
                                    } else {
                                        el.bg(rgba(0x00000000))
                                    }
                                },
                            )
                            .on_click(move |_e, _w, cx| {
                                let store = crate::app_state::AppState::store(cx);
                                let result =
                                    store.lock().add_snippet("", &cmd_for_save, false, None);
                                if let Some(ref nc) = notifications {
                                    let notif = match result {
                                        Ok(_) => {
                                            Notification::new(t!("history.saved_title").to_string())
                                                .level(NotificationLevel::Success)
                                                .message(
                                                    t!(
                                                        "history.saved_msg",
                                                        command = cmd_for_save.as_str()
                                                    )
                                                    .to_string(),
                                                )
                                        }
                                        Err(_) => Notification::new(
                                            t!("history.save_failed_title").to_string(),
                                        )
                                        .level(NotificationLevel::Danger),
                                    };
                                    nc.update(cx, |c, cx| c.show(notif, cx));
                                }
                            })
                            .child(
                                svg()
                                    .path("icons/save.svg")
                                    .size(px(13.0))
                                    .text_color(rgb(text_muted())),
                            );

                        // Paste button: writes the command into the active
                        // terminal's input line (no Enter — the user can
                        // edit before running).
                        let cmd_for_paste = cmd.clone();
                        let on_paste_for_btn = on_paste.clone();
                        let tooltip_paste = tooltip.clone();
                        let paste_btn = div()
                            .id(ElementId::Name(format!("history-paste-{i}").into()))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(20.0))
                            .rounded(px(4.0))
                            .bg(rgba(0x00000000))
                            .with_transition(ElementId::Name(format!("history-paste-{i}").into()))
                            .on_hover(move |hovered, w, cx| {
                                if let Some(ref tc) = tooltip_paste {
                                    if *hovered {
                                        tc.update(cx, |t, cx| {
                                            t.show(
                                                t!("panel.paste_tooltip").to_string(),
                                                w.mouse_position(),
                                                cx,
                                            );
                                        });
                                    } else {
                                        tc.update(cx, |t, cx| {
                                            t.hide(cx);
                                        });
                                    }
                                }
                            })
                            .transition_on_hover(
                                duration_fast(),
                                EASE_STANDARD,
                                move |hovered, el| {
                                    if *hovered {
                                        el.bg(rgb(surface_hover()))
                                    } else {
                                        el.bg(rgba(0x00000000))
                                    }
                                },
                            )
                            .on_click(move |_e, _w, cx| {
                                if let Some(cb) = on_paste_for_btn.as_ref() {
                                    cb(cmd_for_paste.clone(), cx);
                                }
                            })
                            .child(
                                svg()
                                    .path("icons/clipboard-copy.svg")
                                    .size(px(13.0))
                                    .text_color(rgb(text_muted())),
                            );

                        div()
                            .id(row_id.clone())
                            .h(px(ROW_HEIGHT))
                            .w_full()
                            .relative()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1p5()
                            .px_2()
                            .rounded(px(4.0))
                            // Hover drives the row background + the
                            // buttons' opacity transition. We use
                            // `transition_when_else` (not `transition_on_hover`)
                            // so the buttons can also stay visible while
                            // the row is hovered, independent of mouse
                            // position over the buttons themselves.
                            .with_transition(row_id_for_transition)
                            .on_hover({
                                let entity = entity.clone();
                                move |hovered, _w, cx| {
                                    let _ = entity.update(cx, |view, cx| {
                                        if *hovered {
                                            view.hovered_row = Some(i);
                                        } else if view.hovered_row == Some(i) {
                                            // Only clear if we still own the
                                            // hover — another row may have
                                            // already claimed it (prevents
                                            // the bottom-to-top glitch where
                                            // `false` fires after `true`).
                                            view.hovered_row = None;
                                        }
                                        cx.notify();
                                    });
                                }
                            })
                            .transition_when_else(
                                is_hovered,
                                duration_fast(),
                                EASE_STANDARD,
                                |el| el.bg(rgba((surface_hover() << 8) | 0x60)),
                                |el| el.bg(rgba((surface_hover() << 8) | 0x00)),
                            )
                            // Command text fills the full row width so long
                            // commands don't shift when the hover buttons fade
                            // in — the buttons overlay on top (below).
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(rgb(text_primary()))
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(Label::new(cmd)),
                            )
                            // Buttons: absolutely positioned over the right
                            // edge of the row, layered above the command text
                            // with a transparent background so they don't
                            // displace the text when they fade in. The container
                            // is a `Stateful<Div>` (has an id) so it supports
                            // `with_transition` + `transition_when_else` for
                            // a smooth opacity ease.
                            .child(
                                div()
                                    .id(ElementId::Name(format!("history-btns-{i}").into()))
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
                                    .with_transition(ElementId::Name(
                                        format!("history-btns-{i}").into(),
                                    ))
                                    .transition_when_else(
                                        is_hovered,
                                        duration_fast(),
                                        EASE_STANDARD,
                                        |el| el.opacity(1.0),
                                        |el| el.opacity(0.0),
                                    )
                                    .child(save_btn)
                                    .child(paste_btn),
                            )
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
            // Search input + refresh button
            .when_some(search_input, |el, input| {
                el.child(
                    div()
                        .mb_1()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .child(
                            div().flex_1().min_w_0().child(
                                StyledInput::new("history-search", input).xsmall().prefix(
                                    svg()
                                        .path("icons/search.svg")
                                        .size(px(12.0))
                                        .text_color(rgb(text_muted())),
                                ),
                            ),
                        )
                        .when_some(on_refresh, |el, cb| {
                            el.child(
                                div()
                                    .id("history-refresh")
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(24.0))
                                    .flex_shrink_0()
                                    .rounded(px(4.0))
                                    .bg(rgba(0x00000000))
                                    .with_transition(ElementId::Name(
                                        "history-refresh".to_string().into(),
                                    ))
                                    .on_hover({
                                        let tooltip = tooltip_for_search.clone();
                                        move |hovered, w, cx| {
                                            if let Some(ref tc) = tooltip {
                                                if *hovered {
                                                    tc.update(cx, |t, cx| {
                                                        t.show(
                                                            t!("panel.refresh_tooltip").to_string(),
                                                            w.mouse_position(),
                                                            cx,
                                                        );
                                                    });
                                                } else {
                                                    tc.update(cx, |t, cx| {
                                                        t.hide(cx);
                                                    });
                                                }
                                            }
                                        }
                                    })
                                    .transition_on_hover(
                                        duration_fast(),
                                        EASE_STANDARD,
                                        |hovered, el| {
                                            if *hovered {
                                                el.bg(rgb(surface_hover()))
                                            } else {
                                                el.bg(rgba(0x00000000))
                                            }
                                        },
                                    )
                                    .on_click(move |_e, _w, cx| cb(cx))
                                    .child(
                                        svg()
                                            .path("icons/refresh-cw.svg")
                                            .size(px(13.0))
                                            .text_color(rgb(text_muted())),
                                    ),
                            )
                        }),
                )
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
                                    .child(t!("sidebar.history").to_string()),
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
}
