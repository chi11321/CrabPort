//! Snippets management view — the full-page sidebar view for managing saved
//! command snippets.
//!
//! Listed when the sidebar's "Snippets" item is active. Reads/writes the
//! same `snippets` Store table as the panel-tab Snippets view
//! ([`crate::views::panel::snippets_panel`]); the two are intentionally
//! distinct — the panel is a quick-run overlay next to the terminal, this
//! is the management surface (edit / delete).
//!
//! Layout mirrors [`crate::views::hosts::HostsView`]:
//!
//! ```text
//! ┌─────────────────────────────────┐
//! │ Snippets              [+ New]   │
//! │ ─────────────────────────────── │
//! │ snippet_name                    │  ← right-click: Edit / Delete
//! │   command text (muted)          │
//! │ ...                             │
//! └─────────────────────────────────┘
//! ```

use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::{animation::TransitionExt, transition::general::Linear};
use gpui_component::input::InputState;
use gpui_component::scroll::ScrollableElement as _;
use rust_i18n::t;
use std::time::Duration;

use crate::color::*;
use crate::components::button::Button;
use crate::components::context_menu::{ContextMenuController, ContextMenuItem, ContextMenuState};
use crate::components::dialog::{AlertController, AlertSeverity, AlertState};
use crate::components::input::StyledInput;
use crate::components::notification::{Notification, NotificationController, NotificationLevel};

/// A snippet row shown in the management list.
#[derive(Clone)]
pub struct SnippetRow {
    pub id: i64,
    pub name: String,
    pub command: String,
}

/// In-flight edit state for the snippet editor overlay.
pub struct SnippetEditState {
    pub id: i64,
    /// `true` while the overlay is animating in or fully open. Set to
    /// `false` on close so the dismiss animation plays before `editing`
    /// is cleared by a deferred task.
    pub active: bool,
    pub name_input: Entity<InputState>,
    pub command_input: Entity<InputState>,
    pub name_focused: bool,
    pub command_focused: bool,
    pub name_error: Option<SharedString>,
    pub command_error: Option<SharedString>,
}

/// Snippets management view.
pub struct SnippetsView {
    /// The snippet row currently being hovered, if any.
    hovered_snippet_id: Option<i64>,
    /// The snippet row that triggered the currently-open context menu.
    context_menu_snippet_id: Option<i64>,
    /// Snippet list, most-recently-created first. Reloaded from the Store
    /// before each render via `set_state`.
    snippets: Vec<SnippetRow>,
    /// Active edit overlay, if any. When `Some`, a modal dialog is shown
    /// with name + command inputs.
    editing: Option<SnippetEditState>,
    /// Global context menu host (right-click Edit / Delete).
    context_menu: Option<Entity<ContextMenuController>>,
    /// Global alert dialog host (delete confirmation).
    alert_controller: Option<Entity<AlertController>>,
    /// Global toast notification host (create/save success + failure).
    notification_controller: Option<Entity<NotificationController>>,
}

impl SnippetsView {
    pub fn new() -> Self {
        Self {
            hovered_snippet_id: None,
            context_menu_snippet_id: None,
            snippets: Vec::new(),
            editing: None,
            context_menu: None,
            alert_controller: None,
            notification_controller: None,
        }
    }

    /// Push the latest external state into the view before render.
    /// `snippets` is re-read from the Store by the caller (`render_content`).
    pub fn set_state(
        &mut self,
        snippets: Vec<SnippetRow>,
        context_menu: Entity<ContextMenuController>,
        alert_controller: Entity<AlertController>,
        notification_controller: Entity<NotificationController>,
        cx: &mut Context<Self>,
    ) {
        // Clear stale hover if the snippet disappeared.
        if let Some(id) = self.hovered_snippet_id
            && !snippets.iter().any(|s| s.id == id)
        {
            self.hovered_snippet_id = None;
        }
        self.snippets = snippets;
        self.context_menu = Some(context_menu);
        self.alert_controller = Some(alert_controller);
        self.notification_controller = Some(notification_controller);
        let _ = cx;
    }

    /// Open the edit overlay for an existing snippet. Pre-fills the
    /// inputs with the snippet's current name + command.
    fn begin_edit(&mut self, snippet: &SnippetRow, window: &mut Window, cx: &mut Context<Self>) {
        let name_input =
            cx.new(|cx| InputState::new(window, cx).default_value(snippet.name.clone()));
        let command_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .default_value(snippet.command.clone())
        });
        self.editing = Some(SnippetEditState {
            id: snippet.id,
            active: true,
            name_input,
            command_input,
            name_focused: true,
            command_focused: false,
            name_error: None,
            command_error: None,
        });
        cx.notify();
    }

    /// Open the edit overlay in "create" mode — empty inputs, `id = 0`
    /// signals that Save should insert a new snippet rather than update.
    fn begin_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name_input = cx.new(|cx| InputState::new(window, cx));
        let command_input = cx.new(|cx| InputState::new(window, cx).multi_line(true));
        self.editing = Some(SnippetEditState {
            id: 0,
            active: true,
            name_input,
            command_input,
            name_focused: true,
            command_focused: false,
            name_error: None,
            command_error: None,
        });
        cx.notify();
    }

    /// Close the edit overlay without saving. Sets `active = false` so the
    /// dismiss animation plays, then clears `editing` after a short delay.
    fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        if let Some(ref mut e) = self.editing {
            e.active = false;
        }
        let entity = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            smol::Timer::after(std::time::Duration::from_millis(160)).await;
            let _ = entity.update(cx, |view, cx| {
                view.editing = None;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Save the edit overlay's name + command back to the Store, then close.
    /// `id = 0` means create a new snippet; otherwise update the existing one.
    fn save_edit(&mut self, cx: &mut Context<Self>) {
        let Some(ref edit) = self.editing else {
            return;
        };
        let id = edit.id;
        let name = edit.name_input.read(cx).value().to_string();
        let command = edit.command_input.read(cx).value().to_string();

        // Validate: both name and command are required.
        let name_error = if name.trim().is_empty() {
            Some(t!("snippets.error_name_required").into())
        } else {
            None
        };
        let command_error = if command.trim().is_empty() {
            Some(t!("snippets.error_command_required").into())
        } else {
            None
        };
        if name_error.is_some() || command_error.is_some() {
            if let Some(ref mut edit) = self.editing {
                edit.name_error = name_error;
                edit.command_error = command_error;
            }
            cx.notify();
            return;
        }

        let store = crate::app_state::AppState::store(cx);
        let name_for_notif = name.clone();
        if id == 0 {
            match store.lock().add_snippet(&name, &command) {
                Ok(_) => {
                    self.show_notification(
                        NotificationLevel::Success,
                        t!("snippets.notif_created_title").to_string(),
                        t!("snippets.notif_created_msg", name = name_for_notif.as_str())
                            .to_string(),
                        Duration::from_secs(3),
                        cx,
                    );
                    self.cancel_edit(cx);
                }
                Err(e) => {
                    tracing::error!("add_snippet failed: {e}");
                    self.show_notification(
                        NotificationLevel::Danger,
                        t!("snippets.notif_save_failed_title").to_string(),
                        t!(
                            "snippets.notif_save_failed_msg",
                            name = name_for_notif.as_str()
                        )
                        .to_string(),
                        Duration::from_secs(5),
                        cx,
                    );
                    cx.notify();
                }
            }
        } else {
            match store.lock().update_snippet(id, &name, &command) {
                Ok(_) => {
                    self.show_notification(
                        NotificationLevel::Success,
                        t!("snippets.notif_updated_title").to_string(),
                        t!("snippets.notif_updated_msg", name = name_for_notif.as_str())
                            .to_string(),
                        Duration::from_secs(3),
                        cx,
                    );
                    self.cancel_edit(cx);
                }
                Err(e) => {
                    tracing::error!("update_snippet failed: {e}");
                    self.show_notification(
                        NotificationLevel::Danger,
                        t!("snippets.notif_save_failed_title").to_string(),
                        t!(
                            "snippets.notif_save_failed_msg",
                            name = name_for_notif.as_str()
                        )
                        .to_string(),
                        Duration::from_secs(5),
                        cx,
                    );
                    cx.notify();
                }
            }
        }
    }

    fn show_notification(
        &self,
        level: NotificationLevel,
        title: String,
        message: String,
        duration: Duration,
        cx: &mut Context<Self>,
    ) {
        if let Some(ref nc) = self.notification_controller {
            nc.update(cx, |c, cx| {
                c.show(
                    Notification::new(title)
                        .level(level)
                        .message(message)
                        .duration(duration),
                    cx,
                );
            });
        }
    }

    /// Delete a snippet by id (after confirmation).
    fn delete_snippet(&mut self, id: i64, cx: &mut Context<Self>) {
        let store = crate::app_state::AppState::store(cx);
        let _ = store.lock().remove_snippet(id);
        cx.notify();
    }
}

impl Default for SnippetsView {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for SnippetsView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let snippets = self.snippets.clone();
        let context_menu = self.context_menu.clone();
        let alert_controller = self.alert_controller.clone();
        let hovered_snippet_id = self.hovered_snippet_id;

        // Clear stale context-menu highlight if the menu closed.
        let menu_active = self
            .context_menu
            .as_ref()
            .map(|cm| cm.read_with(_cx, |c, _| c.is_active()))
            .unwrap_or(false);
        if !menu_active {
            self.context_menu_snippet_id = None;
        }
        let context_menu_snippet_id = self.context_menu_snippet_id;

        // Snapshot edit state for the overlay render.
        let editing = self.editing.is_some();
        let edit_active = self.editing.as_ref().map(|e| e.active).unwrap_or(false);
        let edit_is_new = self.editing.as_ref().map(|e| e.id == 0).unwrap_or(false);
        let edit_name_input = self.editing.as_ref().map(|e| e.name_input.clone());
        let edit_command_input = self.editing.as_ref().map(|e| e.command_input.clone());
        let edit_name_focused = self
            .editing
            .as_ref()
            .map(|e| e.name_focused)
            .unwrap_or(false);
        let edit_command_focused = self
            .editing
            .as_ref()
            .map(|e| e.command_focused)
            .unwrap_or(false);
        let edit_name_error = self.editing.as_ref().and_then(|e| e.name_error.clone());
        let edit_command_error = self.editing.as_ref().and_then(|e| e.command_error.clone());

        div()
            .size_full()
            .flex()
            .flex_col()
            .relative()
            // --- Header: title + New button ---
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .pt_4()
                    .pb_2()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(TEXT_PRIMARY))
                            .child(t!("sidebar.snippets").to_string()),
                    )
                    .child(
                        Button::new("snippets-new-btn")
                            .primary()
                            .icon("icons/plus.svg")
                            .w_auto()
                            .px_2()
                            .child(t!("snippets.new_button").to_string())
                            .on_click({
                                let entity = _cx.entity().downgrade();
                                move |_e, w, cx| {
                                    let _ = entity.update(cx, |view, cx| {
                                        view.begin_new(w, cx);
                                    });
                                }
                            }),
                    ),
            )
            // --- Separator ---
            .child(div().h_px().bg(rgb(BORDER)).mx_4())
            // --- Snippets list (or empty state) ---
            .child(
                div()
                    .flex_1()
                    .overflow_y_scrollbar()
                    .px_4()
                    .py_2()
                    .when_else(
                        snippets.is_empty(),
                        |el| {
                            el.flex().items_center().justify_center().child(
                                div()
                                    .text_color(rgb(TEXT_MUTED))
                                    .text_sm()
                                    .child(t!("snippets.empty").to_string()),
                            )
                        },
                        |el| {
                            el.flex()
                                .flex_col()
                                .gap_1()
                                .children(snippets.iter().map(|s| {
                                    let snippet = s.clone();
                                    let context_menu = context_menu.clone();
                                    let alert_controller = alert_controller.clone();
                                    let is_hovered = hovered_snippet_id == Some(s.id);
                                    let force_highlight = context_menu_snippet_id == Some(s.id);
                                    let entity = _cx.entity().downgrade();

                                    snippet_row(
                                        &snippet,
                                        is_hovered,
                                        force_highlight,
                                        entity,
                                        context_menu,
                                        alert_controller,
                                    )
                                    .into_any_element()
                                }))
                        },
                    ),
            )
            // --- Edit overlay (with connection_form-style easing) ---
            .when(editing, |el| {
                let overlay_id = ElementId::Name("snippet-edit-overlay".into());
                let dialog_id = ElementId::Name("snippet-edit-dialog".into());
                let title = if edit_is_new {
                    t!("snippets.new_button").to_string()
                } else {
                    t!("snippets.edit_title").to_string()
                };
                el.child(
                    div()
                        .id(overlay_id.clone())
                        .absolute()
                        .size_full()
                        .top_0()
                        .left_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(rgba(0x00000000))
                        .when(edit_active, |el| {
                            el.occlude().on_click({
                                let entity = _cx.entity().downgrade();
                                move |_, _, cx| {
                                    let _ = entity.update(cx, |view, cx| {
                                        view.cancel_edit(cx);
                                    });
                                }
                            })
                        })
                        .with_transition(overlay_id)
                        .transition_when_else(
                            edit_active,
                            Duration::from_millis(150),
                            Linear,
                            |el| el.bg(rgba(0x00000080)),
                            |el| el.bg(rgba(0x00000000)),
                        )
                        .child(
                            div()
                                .id(dialog_id.clone())
                                .w(px(420.0))
                                .bg(rgb(BG_BASE))
                                .border_1()
                                .border_color(rgb(BORDER))
                                .rounded_lg()
                                .shadow_lg()
                                .flex()
                                .flex_col()
                                .p_6()
                                .gap_4()
                                .opacity(0.0)
                                .mt(px(-16.0))
                                .when(edit_active, |el| {
                                    el.on_click(|_, _, cx| {
                                        cx.stop_propagation();
                                    })
                                })
                                .with_transition(dialog_id)
                                .transition_when_else(
                                    edit_active,
                                    Duration::from_millis(150),
                                    Linear,
                                    |el| el.opacity(1.0).mt_0(),
                                    |el| el.opacity(0.0).mt(px(-16.0)),
                                )
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb(TEXT_PRIMARY))
                                        .child(title),
                                )
                                .when_some(edit_name_input, |el, input| {
                                    el.child(
                                        div().child(
                                            StyledInput::new("snippet-edit-name", input)
                                                .label(t!("snippets.name").to_string())
                                                .focused(edit_name_focused)
                                                .when_some(edit_name_error, |el, e| el.error(e)),
                                        ),
                                    )
                                })
                                .when_some(edit_command_input, |el, input| {
                                    el.child(
                                        div().child(
                                            StyledInput::new("snippet-edit-command", input)
                                                .label(t!("snippets.command").to_string())
                                                .multi_line(true)
                                                .rows(5)
                                                .focused(edit_command_focused)
                                                .when_some(edit_command_error, |el, e| el.error(e)),
                                        ),
                                    )
                                })
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .gap_3()
                                        .justify_end()
                                        .child(
                                            Button::new("snippet-edit-cancel")
                                                .centered(true)
                                                .child(t!("snippets.cancel").to_string())
                                                .on_click({
                                                    let entity = _cx.entity().downgrade();
                                                    move |_, _, cx| {
                                                        let _ = entity.update(cx, |view, cx| {
                                                            view.cancel_edit(cx);
                                                        });
                                                    }
                                                }),
                                        )
                                        .child(
                                            Button::new("snippet-edit-save")
                                                .primary()
                                                .centered(true)
                                                .child(t!("snippets.save").to_string())
                                                .on_click({
                                                    let entity = _cx.entity().downgrade();
                                                    move |_, _, cx| {
                                                        let _ = entity.update(cx, |view, cx| {
                                                            view.save_edit(cx);
                                                        });
                                                    }
                                                }),
                                        ),
                                ),
                        ),
                )
            })
    }
}

// ---------------------------------------------------------------------------
// Snippet row
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn snippet_row(
    snippet: &SnippetRow,
    is_hovered: bool,
    force_highlight: bool,
    entity: WeakEntity<SnippetsView>,
    context_menu: Option<Entity<ContextMenuController>>,
    alert_controller: Option<Entity<AlertController>>,
) -> impl IntoElement {
    let row_id = ElementId::Name(format!("snippet-row-{}", snippet.id).into());

    let snippet_id = snippet.id;
    let snippet_name = snippet.name.clone();
    let snippet_command = snippet.command.clone();
    let is_highlighted = is_hovered || force_highlight;

    div()
        .id(row_id.clone())
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(rgb(BG_BASE))
        // Right-click context menu: "Edit" + "Delete".
        .on_mouse_down(MouseButton::Right, {
            let entity = entity.clone();
            let snippet_for_edit = snippet.clone();
            move |event, _w, cx| {
                let Some(ref cm) = context_menu else {
                    return;
                };
                let _ = entity.update(cx, |view, cx| {
                    view.context_menu_snippet_id = Some(snippet_id);
                    cx.notify();
                });
                let pos = event.position;
                let entity_for_edit = entity.clone();
                let entity_for_delete = entity.clone();
                let alert_controller = alert_controller.clone();
                let snippet_name = snippet_name.clone();
                let snippet_for_edit = snippet_for_edit.clone();
                cm.update(cx, |c, cx| {
                    c.show(
                        ContextMenuState {
                            position: pos,
                            items: vec![
                                ContextMenuItem::new(t!("snippets.edit").to_string(), {
                                    let entity = entity_for_edit.clone();
                                    let snippet = snippet_for_edit.clone();
                                    move |w, cx| {
                                        let _ = entity.update(cx, |view, cx| {
                                            view.begin_edit(&snippet, w, cx);
                                        });
                                    }
                                }),
                                ContextMenuItem::new(t!("snippets.delete").to_string(), {
                                    let entity = entity_for_delete.clone();
                                    let alert_controller = alert_controller.clone();
                                    let snippet_name = snippet_name.clone();
                                    move |_w, cx| {
                                        let Some(ref ac) = alert_controller else {
                                            return;
                                        };
                                        let entity = entity.clone();
                                        ac.update(cx, |c, cx| {
                                            c.show(
                                                AlertState {
                                                    severity: AlertSeverity::Danger,
                                                    title: t!("snippets.delete_title")
                                                        .to_string()
                                                        .into(),
                                                    description: Some(
                                                        t!(
                                                            "snippets.delete_prompt",
                                                            name = snippet_name.as_str()
                                                        )
                                                        .to_string()
                                                        .into(),
                                                    ),
                                                    confirm_label: t!("snippets.delete_confirm")
                                                        .to_string()
                                                        .into(),
                                                    cancel_label: t!("terminal.host_key_cancel")
                                                        .to_string()
                                                        .into(),
                                                    on_confirm: Some(Rc::new(move |_w, cx| {
                                                        let _ = entity.update(cx, |view, cx| {
                                                            view.delete_snippet(snippet_id, cx);
                                                        });
                                                    })),
                                                    ..AlertState::default()
                                                },
                                                cx,
                                            );
                                        });
                                    }
                                })
                                .danger(true),
                            ],
                            ..ContextMenuState::default()
                        },
                        cx,
                    );
                });
            }
        })
        // Track hover of the whole row.
        .with_transition(row_id)
        .on_hover(move |hovered, _w, cx| {
            let _ = entity.update(cx, |view, cx| {
                if *hovered {
                    view.hovered_snippet_id = Some(snippet_id);
                } else if view.hovered_snippet_id == Some(snippet_id) {
                    view.hovered_snippet_id = None;
                }
                cx.notify();
            });
        })
        .transition_when_else(
            is_highlighted,
            Duration::from_millis(120),
            Linear,
            |el| el.bg(rgb(SURFACE_ACTIVE)),
            |el| el.bg(rgb(BG_BASE)),
        )
        // Snippet info (name + command)
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .flex_1()
                .child(div().text_sm().text_color(rgb(TEXT_PRIMARY)).child(
                    if snippet.name.is_empty() {
                        snippet_command.clone()
                    } else {
                        snippet.name.clone()
                    },
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(TEXT_MUTED))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(snippet_command.clone()),
                ),
        )
}
