//! Snippet form dialog methods for `CrabportApp` (open/create/edit/close and
//! persistence via the credential store).

use gpui::*;
use rust_i18n::t;

use super::CrabportApp;
use crate::app_state::AppState;
use crate::components::notification::{Notification, NotificationLevel};

impl CrabportApp {
    // -----------------------------------------------------------------------
    // Snippets form
    // -----------------------------------------------------------------------

    /// Open the snippet form dialog in create mode (blank fields).
    pub fn open_snippet_form_for_create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Lazily create the form state on first use.
        if self.snippet_form.is_none() {
            let mut form = crate::views::snippets::SnippetFormState::new(window, cx);
            let app = cx.entity().clone();
            form.on_close = Some(std::rc::Rc::new(move |_w, cx| {
                app.update(cx, |app, cx| app.close_snippet_form(cx));
            }));
            let app = cx.entity().clone();
            form.on_save = Some(std::rc::Rc::new(move |out, w, cx| {
                app.update(cx, |app, cx| app.save_snippet(out, w, cx));
            }));
            self.snippet_form = Some(form);
        }
        if let Some(ref mut form) = self.snippet_form {
            form.open_for_create(window, cx);
        }
        cx.notify();
    }

    /// Open the snippet form dialog in edit mode, populated from a saved
    /// snippet.
    pub fn open_snippet_form_for_edit(
        &mut self,
        snippet_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = AppState::store(cx);
        let snippet = store
            .lock()
            .snippets()
            .ok()
            .into_iter()
            .flatten()
            .find(|s| s.id == snippet_id);
        let Some(snippet) = snippet else {
            tracing::warn!("snippet {snippet_id} not found");
            return;
        };
        if self.snippet_form.is_none() {
            let mut form = crate::views::snippets::SnippetFormState::new(window, cx);
            let app = cx.entity().clone();
            form.on_close = Some(std::rc::Rc::new(move |_w, cx| {
                app.update(cx, |app, cx| app.close_snippet_form(cx));
            }));
            let app = cx.entity().clone();
            form.on_save = Some(std::rc::Rc::new(move |out, w, cx| {
                app.update(cx, |app, cx| app.save_snippet(out, w, cx));
            }));
            self.snippet_form = Some(form);
        }
        if let Some(ref mut form) = self.snippet_form {
            form.open_for_edit(snippet.id, &snippet.name, &snippet.command, window, cx);
        }
        cx.notify();
    }

    /// Close the snippet form dialog. Mirrors `close_tunnel_form`.
    pub fn close_snippet_form(&mut self, cx: &mut Context<Self>) {
        if let Some(ref mut form) = self.snippet_form {
            form.close();
        }
        // Destroy after the exit animation.
        let app = cx.entity().clone();
        cx.spawn(async move |_this, cx| {
            smol::Timer::after(std::time::Duration::from_millis(200)).await;
            let _ = app.update(cx, |app, cx| {
                if app.snippet_form.is_some() {
                    app.snippet_form = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Persist a snippet (insert or update) from the form output.
    pub fn save_snippet(
        &mut self,
        out: crate::views::snippets::SnippetFormOutput,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = AppState::store(cx);
        let name = out.name.clone();
        match out.editing_id {
            Some(id) => {
                if let Err(e) = store.lock().update_snippet(id, &out.name, &out.command) {
                    tracing::error!("update_snippet failed: {e}");
                    self.app_ctx.notifications.update(cx, |c, cx| {
                        c.show(
                            Notification::new(t!("snippets.notif_save_failed_title").to_string())
                                .level(NotificationLevel::Danger)
                                .message(
                                    t!("snippets.notif_save_failed_msg", name = name.as_str())
                                        .to_string(),
                                )
                                .duration(std::time::Duration::from_secs(5)),
                            cx,
                        );
                    });
                    return;
                }
                self.app_ctx.notifications.update(cx, |c, cx| {
                    c.show(
                        Notification::new(t!("snippets.notif_updated_title").to_string())
                            .level(NotificationLevel::Success)
                            .message(
                                t!("snippets.notif_updated_msg", name = name.as_str()).to_string(),
                            )
                            .duration(std::time::Duration::from_secs(3)),
                        cx,
                    );
                });
            }
            None => {
                if let Err(e) = store.lock().add_snippet(&out.name, &out.command) {
                    tracing::error!("add_snippet failed: {e}");
                    self.app_ctx.notifications.update(cx, |c, cx| {
                        c.show(
                            Notification::new(t!("snippets.notif_save_failed_title").to_string())
                                .level(NotificationLevel::Danger)
                                .message(
                                    t!("snippets.notif_save_failed_msg", name = name.as_str())
                                        .to_string(),
                                )
                                .duration(std::time::Duration::from_secs(5)),
                            cx,
                        );
                    });
                    return;
                }
                self.app_ctx.notifications.update(cx, |c, cx| {
                    c.show(
                        Notification::new(t!("snippets.notif_created_title").to_string())
                            .level(NotificationLevel::Success)
                            .message(
                                t!("snippets.notif_created_msg", name = name.as_str()).to_string(),
                            )
                            .duration(std::time::Duration::from_secs(3)),
                        cx,
                    );
                });
            }
        }
        self.close_snippet_form(cx);
    }
}
