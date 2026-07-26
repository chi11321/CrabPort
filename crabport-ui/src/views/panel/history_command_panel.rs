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

use gpui::*;
use gpui_component::label::Label;
use gpui_component::v_virtual_list;
use rust_i18n::t;

use super::scaffold::{self, PanelListState};
use crate::color::*;
use crate::components::notification::{Notification, NotificationLevel};

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
#[derive(Default)]
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
    /// Shared search / scroll / hover state.
    list: PanelListState,
}

impl HistoryCommandPanel {
    pub fn new() -> Self {
        Self::default()
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
        self.list.ensure_search_input(
            "panel.search",
            |this, query, cx| {
                this.list.search_query = query;
                cx.notify();
            },
            window,
            cx,
        );

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

    /// The filtered view of `self.history` for the current search query.
    /// Case-insensitive substring match. Returns indices into the original
    /// list so we can clone the `HistoryCommand` cheaply.
    fn filtered(&self) -> Vec<usize> {
        scaffold::filter_indices(&self.history, &self.list.search_query, |h, q| {
            h.command.to_lowercase().contains(q)
        })
    }
}

/// Fixed height of each history row. The virtual list requires uniform
/// item sizes.
const ROW_HEIGHT: f32 = 28.0;

impl Render for HistoryCommandPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let search_input = self.list.search_input.clone();
        let on_paste = self.on_paste.clone();
        let on_refresh = self.on_refresh.clone();
        let notifications = self.notifications.clone();
        let tooltip = self.tooltip.clone();
        let scroll_handle = self.list.scroll_handle.clone();

        // Compute the filtered list + per-row data once per render.
        let (filtered_for_list, item_sizes, is_empty) =
            scaffold::prepare_list_items(&self.history, &self.filtered(), ROW_HEIGHT);
        let hovered_row = self.list.hovered_row;

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

                        // Save button: persists the command as a snippet
                        // into the global Store so it shows up in the
                        // Snippets panel and survives restarts.
                        let cmd_for_save = cmd.clone();
                        let notifications = notifications.clone();
                        let save_btn = scaffold::panel_icon_button(
                            format!("history-save-{i}"),
                            20.0,
                            false,
                            "icons/save.svg",
                            "panel.save_tooltip",
                            tooltip.clone(),
                            move |_e, _w, cx| {
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
                            },
                        );

                        // Paste button: writes the command into the active
                        // terminal's input line (no Enter — the user can
                        // edit before running).
                        let cmd_for_paste = cmd.clone();
                        let on_paste_for_btn = on_paste.clone();
                        let paste_btn = scaffold::panel_icon_button(
                            format!("history-paste-{i}"),
                            20.0,
                            false,
                            "icons/clipboard-copy.svg",
                            "panel.paste_tooltip",
                            tooltip.clone(),
                            move |_e, _w, cx| {
                                if let Some(cb) = on_paste_for_btn.as_ref() {
                                    cb(cmd_for_paste.clone(), cx);
                                }
                            },
                        );

                        // Hover drives the row background + the buttons'
                        // opacity transition (via `transition_when_else`,
                        // not `transition_on_hover`) so the buttons stay
                        // visible while the row is hovered, independent of
                        // mouse position over the buttons themselves.
                        scaffold::row_hover_chrome(
                            scaffold::row_base(row_id.clone(), ROW_HEIGHT).relative(),
                            row_id,
                            is_hovered,
                            entity.clone(),
                            i,
                            |view: &mut Self| Some(&mut view.list.hovered_row),
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
                        // displace the text when they fade in.
                        .child(scaffold::hover_button_strip(
                            format!("history-btns-{i}"),
                            is_hovered,
                            [save_btn.into_any_element(), paste_btn.into_any_element()],
                        ))
                    })
                    .collect::<Vec<_>>()
            },
        )
        .track_scroll(&scroll_handle);

        // Refresh button rendered beside the search input: asks the active
        // terminal backend to re-read the TTY history file.
        let refresh_btn = on_refresh.map(|cb| {
            scaffold::panel_icon_button(
                "history-refresh".to_string(),
                24.0,
                true,
                "icons/refresh-cw.svg",
                "panel.refresh_tooltip",
                tooltip_for_search,
                move |_e, _w, cx| cb(cx),
            )
            .into_any_element()
        });

        scaffold::panel_shell(
            "history-search",
            search_input,
            scaffold::SearchRow::WithSuffix(refresh_btn),
            is_empty,
            t!("sidebar.history").to_string(),
            list,
            &scroll_handle,
        )
    }
}
