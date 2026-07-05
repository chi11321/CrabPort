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

/// A single saved snippet, mirroring the Store row.
#[derive(Clone, Debug)]
pub struct Snippet {
    pub id: i64,
    pub name: String,
    pub command: String,
}

/// Snippets panel view.
pub struct SnippetsPanel {
    /// Current snippet list, most-recently-created first. Reloaded from
    /// the Store on each `set_state` call.
    snippets: Arc<Vec<Snippet>>,
    /// Run callback — invoked with `command + "\r"` when the user clicks
    /// the "run" button. Writes the command into the active terminal and
    /// executes it immediately.
    on_run: Option<Rc<dyn Fn(String, &mut App)>>,
    /// Paste callback — invoked with the command text (no Enter) so the
    /// user can edit before running. Uses `write_raw` to avoid
    /// re-capturing the text as history.
    on_paste: Option<Rc<dyn Fn(String, &mut App)>>,
    /// Search input state (lazily initialized on the first `set_state`).
    search_input: Option<Entity<InputState>>,
    /// Current search query. Updated via `InputEvent::Change` subscription.
    search_query: String,
    /// Scroll handle for the virtual list + custom scrollbar.
    scroll_handle: VirtualListScrollHandle,
    /// Per-row hover state, keyed by snippet index in the filtered list.
    hovered_row: Option<usize>,
}

impl SnippetsPanel {
    pub fn new() -> Self {
        Self {
            snippets: Arc::new(Vec::new()),
            on_run: None,
            on_paste: None,
            search_input: None,
            search_query: String::new(),
            scroll_handle: VirtualListScrollHandle::new(),
            hovered_row: None,
        }
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
                        this.search_query = input.read(cx).value().to_string();
                        cx.notify();
                    }
                },
            )
            .detach();
            self.search_input = Some(entity);
        }

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
        if changed {
            cx.notify();
        }
    }

    /// The filtered view of `self.snippets` for the current `search_query`.
    /// Case-insensitive substring match on both name and command.
    fn filtered(&self) -> Vec<usize> {
        let q = self.search_query.trim().to_lowercase();
        if q.is_empty() {
            return (0..self.snippets.len()).collect();
        }
        self.snippets
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.name.to_lowercase().contains(&q) || s.command.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect()
    }
}

impl Default for SnippetsPanel {
    fn default() -> Self {
        Self::new()
    }
}

/// Fixed height of each snippet row. The virtual list requires uniform
/// item sizes.
const ROW_HEIGHT: f32 = 28.0;

impl Render for SnippetsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let search_input = self.search_input.clone();
        let on_run = self.on_run.clone();
        let on_paste = self.on_paste.clone();
        let scroll_handle = self.scroll_handle.clone();

        // Compute the filtered list + per-row data once per render.
        let filtered_indices = self.filtered();
        let filtered: Vec<Snippet> = filtered_indices
            .iter()
            .map(|&i| self.snippets[i].clone())
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
            "snippet-list",
            item_sizes,
            move |_this, range, _window, cx| {
                let filtered = &filtered_for_list;
                let on_run = on_run.clone();
                let on_paste = on_paste.clone();
                let entity = cx.entity().downgrade();
                range
                    .map(|i| {
                        let s = &filtered[i];
                        let name = s.name.clone();
                        let cmd = s.command.clone();
                        let is_hovered = hovered_row == Some(i);
                        let row_id = ElementId::Name(format!("snippet-{i}").into());
                        let row_id_for_transition = row_id.clone();

                        // Run button: writes command + Enter into the active
                        // terminal, executing it immediately.
                        let cmd_for_run = cmd.clone();
                        let on_run_for_btn = on_run.clone();
                        let run_btn = div()
                            .id(ElementId::Name(format!("snippet-run-{i}").into()))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(20.0))
                            .rounded(px(4.0))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(SURFACE_HOVER)))
                            .on_click(move |_e, _w, cx| {
                                if let Some(cb) = on_run_for_btn.as_ref() {
                                    cb(cmd_for_run.clone(), cx);
                                }
                            })
                            .child(
                                svg()
                                    .path("icons/file-terminal.svg")
                                    .size(px(13.0))
                                    .text_color(rgb(TEXT_MUTED)),
                            );

                        // Paste button: writes the command text (no Enter)
                        // so the user can edit before running. Uses
                        // `write_raw` to avoid re-capturing as history.
                        let cmd_for_paste = cmd.clone();
                        let on_paste_for_btn = on_paste.clone();
                        let paste_btn = div()
                            .id(ElementId::Name(format!("snippet-paste-{i}").into()))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(20.0))
                            .rounded(px(4.0))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(SURFACE_HOVER)))
                            .on_click(move |_e, _w, cx| {
                                if let Some(cb) = on_paste_for_btn.as_ref() {
                                    cb(cmd_for_paste.clone(), cx);
                                }
                            })
                            .child(
                                svg()
                                    .path("icons/clipboard-copy.svg")
                                    .size(px(13.0))
                                    .text_color(rgb(TEXT_MUTED)),
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
                                |el| el.bg(rgba((SURFACE_HOVER << 8) | 0x60)),
                                |el| el.bg(rgba((SURFACE_HOVER << 8) | 0x00)),
                            )
                            // Snippet name (flex-1 so buttons sit on the right).
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(rgb(TEXT_PRIMARY))
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(Label::new(if name.is_empty() {
                                        cmd.clone()
                                    } else {
                                        name
                                    })),
                            )
                            // Buttons: fade in on row hover.
                            .child(
                                div()
                                    .id(ElementId::Name(format!("snippet-btns-{i}").into()))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_0p5()
                                    .opacity(0.0)
                                    .with_transition(ElementId::Name(
                                        format!("snippet-btns-{i}").into(),
                                    ))
                                    .transition_when_else(
                                        is_hovered,
                                        std::time::Duration::from_millis(120),
                                        Linear,
                                        |el| el.opacity(1.0),
                                        |el| el.opacity(0.0),
                                    )
                                    .child(run_btn)
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
                        StyledInput::new("snippet-search", input).xsmall().prefix(
                            svg()
                                .path("icons/search.svg")
                                .size(px(12.0))
                                .text_color(rgb(TEXT_MUTED)),
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
                                .text_color(rgb(TEXT_MUTED))
                                .text_sm()
                                .child(t!("sidebar.snippets").to_string()),
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
                        .border_color(rgb(BORDER))
                        .bg(rgb(BG_TAB_BAR))
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
