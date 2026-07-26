//! Snippets panel — a side panel listing saved command snippets.
//!
//! Sibling of [`super::sftp::SftpPanel`] / [`super::history_command_panel`]:
//! renders inside the right-hand panel strip's "Snippets" tab (see
//! `crabport-ui/src/layouts/panel.rs`).
//!
//! Snippets are persisted globally (not scoped to a host) in the Store's
//! `snippets` table. New snippets are added from the History panel's "save"
//! button; this panel lists them with real-time search, and offers two
//! actions per row:
//!
//! - **Run** (`file-terminal.svg`) — writes `command + "\r"` into the active
//!   terminal, executing it immediately.
//! - **Paste** (`clipboard-paste.svg`) — writes the command text without a
//!   trailing Enter so the user can edit before running.
//!
//! Both buttons fade in on row hover (same transition as the History panel).
//! Deletion is intentionally not exposed here — it lives on the full-page
//! Snippets management view.

use std::rc::Rc;
use std::sync::Arc;

use gpui::*;
use gpui_component::label::Label;
use gpui_component::v_virtual_list;
use rust_i18n::t;

use super::scaffold::{self, PanelListState};
use crate::color::*;

/// A single saved snippet, mirroring the Store row.
#[derive(Clone, Debug)]
pub struct Snippet {
    pub id: i64,
    pub name: String,
    pub command: String,
}

/// Snippets panel view.
#[derive(Default)]
pub struct SnippetsPanel {
    snippets: Arc<Vec<Snippet>>,
    on_run: Option<Rc<dyn Fn(String, &mut App)>>,
    on_paste: Option<Rc<dyn Fn(String, &mut App)>>,
    /// Global tooltip host for button hover tooltips.
    tooltip: Option<Entity<crate::components::tooltip::TooltipController>>,
    /// Shared search / scroll / hover state.
    list: PanelListState,
}

impl SnippetsPanel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update the snippet list + callbacks from the active context.
    /// Called by the content layout each render. Snippets are re-read from
    /// the Store here so newly-saved snippets (e.g. via the History panel's
    /// "save" button) show up on the next repaint.
    #[allow(dead_code)]
    pub fn set_state(
        &mut self,
        on_run: Option<Rc<dyn Fn(String, &mut App)>>,
        on_paste: Option<Rc<dyn Fn(String, &mut App)>>,
        tooltip: Entity<crate::components::tooltip::TooltipController>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Lazily init the search InputState on the first call.
        self.list.ensure_search_input(
            "panel.search",
            |this, query, cx| {
                this.list.search_query = query;
                cx.notify();
            },
            window,
            cx,
        );

        // Re-read snippets from the Store. Cheap (small table) and keeps
        // the panel in sync with saves from anywhere in the app.
        let store = crate::app_state::AppState::store(cx);
        let new_snippets = if let Ok(rows) = store.lock().snippets() {
            Arc::new(
                rows.into_iter()
                    .map(|s| Snippet {
                        id: s.id,
                        name: s.name,
                        command: s.command,
                    })
                    .collect::<Vec<_>>(),
            )
        } else {
            Arc::new(Vec::new())
        };

        let changed = !Arc::ptr_eq(&self.snippets, &new_snippets);
        self.snippets = new_snippets;
        self.on_run = on_run;
        self.on_paste = on_paste;
        self.tooltip = Some(tooltip);
        if changed {
            cx.notify();
        }
    }

    /// The filtered view of `self.snippets` for the current search query.
    /// Case-insensitive substring match on both name and command.
    fn filtered(&self) -> Vec<usize> {
        scaffold::filter_indices(&self.snippets, &self.list.search_query, |s, q| {
            s.name.to_lowercase().contains(q) || s.command.to_lowercase().contains(q)
        })
    }
}

/// Fixed height of each snippet row. The virtual list requires uniform
/// item sizes.
const ROW_HEIGHT: f32 = 28.0;

impl Render for SnippetsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let search_input = self.list.search_input.clone();
        let on_run = self.on_run.clone();
        let on_paste = self.on_paste.clone();
        let tooltip = self.tooltip.clone();
        let scroll_handle = self.list.scroll_handle.clone();

        // Compute the filtered list + per-row data once per render.
        let (filtered_for_list, item_sizes, is_empty) =
            scaffold::prepare_list_items(&self.snippets, &self.filtered(), ROW_HEIGHT);
        let hovered_row = self.list.hovered_row;

        let list = v_virtual_list(
            cx.entity(),
            "snippet-list",
            item_sizes,
            move |_this, range, _window, cx| {
                let filtered = &filtered_for_list;
                let on_run = on_run.clone();
                let on_paste = on_paste.clone();
                let tooltip = tooltip.clone();
                let entity = cx.entity().downgrade();
                range
                    .map(|i| {
                        let s = &filtered[i];
                        let name = s.name.clone();
                        let cmd = s.command.clone();
                        let is_hovered = hovered_row == Some(i);
                        let row_id = ElementId::Name(format!("snippet-{i}").into());

                        // Run button: writes command + Enter into the active
                        // terminal, executing it immediately.
                        let cmd_for_run = cmd.clone();
                        let on_run_for_btn = on_run.clone();
                        let run_btn = scaffold::panel_icon_button(
                            format!("snippet-run-{i}"),
                            20.0,
                            false,
                            "icons/file-terminal.svg",
                            "panel.run_tooltip",
                            tooltip.clone(),
                            move |_e, _w, cx| {
                                if let Some(cb) = on_run_for_btn.as_ref() {
                                    cb(cmd_for_run.clone(), cx);
                                }
                            },
                        );

                        // Paste button: writes the command text (no Enter)
                        // so the user can edit before running. Uses
                        // `write_raw` to avoid re-capturing as history.
                        let cmd_for_paste = cmd.clone();
                        let on_paste_for_btn = on_paste.clone();
                        let paste_btn = scaffold::panel_icon_button(
                            format!("snippet-paste-{i}"),
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

                        scaffold::row_hover_chrome(
                            scaffold::row_base(row_id.clone(), ROW_HEIGHT).relative(),
                            row_id,
                            is_hovered,
                            entity.clone(),
                            i,
                            |view: &mut Self| Some(&mut view.list.hovered_row),
                        )
                        // Snippet name fills the full row width so long
                        // names don't shift when the hover buttons fade
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
                                .child(Label::new(if name.is_empty() {
                                    cmd.clone()
                                } else {
                                    name
                                })),
                        )
                        // Buttons: absolutely positioned over the right
                        // edge of the row, layered above the snippet name
                        // with a transparent background so they don't
                        // displace the text when they fade in.
                        .child(scaffold::hover_button_strip(
                            format!("snippet-btns-{i}"),
                            is_hovered,
                            [run_btn.into_any_element(), paste_btn.into_any_element()],
                        ))
                    })
                    .collect::<Vec<_>>()
            },
        )
        .track_scroll(&scroll_handle);

        scaffold::panel_shell(
            "snippet-search",
            search_input,
            scaffold::SearchRow::Bare,
            is_empty,
            t!("sidebar.snippets").to_string(),
            list,
            &scroll_handle,
        )
    }
}
