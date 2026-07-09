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
use gpui_animation::{animation::TransitionExt, transition::general::Linear};
use gpui_component::input::InputState;
use gpui_component::label::Label;
use gpui_component::scroll::Scrollbar;
use gpui_component::scroll::ScrollbarShow;
use gpui_component::{VirtualListScrollHandle, v_virtual_list};
use rust_i18n::t;

use crate::color::*;
use crate::components::input::StyledInput;

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
    /// Current history list, most-recent-first. Pushed in via `set_state`.
    history: Arc<Vec<HistoryCommand>>,
    /// Paste callback — invoked with the command text when the user clicks
    /// the "paste" button. Writes the command into the active terminal's
    /// input line **without** re-capturing it as history (see
    /// [`crate::views::terminal::TerminalView::write_raw`]).
    on_paste: Option<Rc<dyn Fn(String, &mut App)>>,
    /// Search input state (lazily initialized on the first `set_state`).
    search_input: Option<Entity<InputState>>,
    /// Current search query. Updated via `InputEvent::Change` subscription.
    search_query: String,
    /// Scroll handle for the virtual list + custom scrollbar.
    scroll_handle: VirtualListScrollHandle,
    /// Per-row hover state. Keyed by command index (the position in the
    /// *filtered* list for the current render). Used to drive the
    /// copy/paste buttons' fade-in transition.
    hovered_row: Option<usize>,
}

impl HistoryCommandPanel {
    pub fn new() -> Self {
        Self {
            history: Arc::new(Vec::new()),
            on_paste: None,
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

        let list = v_virtual_list(
            cx.entity(),
            "history-cmd-list",
            item_sizes,
            move |_this, range, _window, cx| {
                let filtered = &filtered_for_list;
                let on_paste = on_paste.clone();
                let entity = cx.entity().downgrade();
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
                        let save_btn = div()
                            .id(ElementId::Name(format!("history-save-{i}").into()))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(20.0))
                            .rounded(px(4.0))
                            .hover(|s| s.bg(rgb(surface_hover())))
                            .on_click(move |_e, _w, cx| {
                                let store = crate::app_state::AppState::store(cx);
                                let _ = store.lock().add_snippet("", &cmd_for_save, false, None);
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
                        let paste_btn = div()
                            .id(ElementId::Name(format!("history-paste-{i}").into()))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(20.0))
                            .rounded(px(4.0))
                            .hover(|s| s.bg(rgb(surface_hover())))
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
                                std::time::Duration::from_millis(120),
                                Linear,
                                |el| el.bg(rgba((surface_hover() << 8) | 0x60)),
                                |el| el.bg(rgba((surface_hover() << 8) | 0x00)),
                            )
                            // Command text (flex-1 so buttons sit on the right).
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
                            // Buttons: fade in on row hover. The container
                            // is a `Stateful<Div>` (has an id) so it supports
                            // `with_transition` + `transition_when_else` for
                            // a smooth opacity ease.
                            .child(
                                div()
                                    .id(ElementId::Name(format!("history-btns-{i}").into()))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_0p5()
                                    .opacity(0.0)
                                    .with_transition(ElementId::Name(
                                        format!("history-btns-{i}").into(),
                                    ))
                                    .transition_when_else(
                                        is_hovered,
                                        std::time::Duration::from_millis(120),
                                        Linear,
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
        .track_scroll(&scroll_handle)
        .pr(px(12.0));

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
                        StyledInput::new("history-search", input).xsmall().prefix(
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
                                .child(t!("sidebar.history").to_string()),
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
                                .w(px(12.0))
                                .child(
                                    Scrollbar::vertical(&scroll_handle)
                                        .scrollbar_show(ScrollbarShow::Hover),
                                ),
                        ),
                )
            })
    }
}
