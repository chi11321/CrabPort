//! Shared inline group-rename state extracted from SessionsView /
//! SnippetsView / TunnelsView.
//!
//! All three collection views support inline rename of a group via an
//! `InputState`-backed `StyledInput`. The logic — lazy `InputState`
//! creation, Enter-to-commit / blur-to-cancel wiring, and persisting via
//! `CrabportApp::rename_group` — is identical across all three, so it
//! lives here once.

use gpui::*;
use gpui_component::input::InputState;

use crate::app::CrabportApp;

/// Trait implemented by views that embed a [`GroupRenameState`]. Provides
/// the `commit` / `cancel` entry points called from the `subscribe` /
/// `on_blur` callbacks wired in [`GroupRenameState::start`].
pub trait GroupRenameView: Sized {
    /// Borrow the shared rename state.
    fn group_rename(&mut self) -> &mut GroupRenameState;
    /// Borrow the app entity (for persisting the rename).
    fn app_entity(&self) -> &Entity<CrabportApp>;
}

/// Shared inline-rename state. Embedded in each collection view to avoid
/// duplicating the same three fields + three methods.
pub struct GroupRenameState {
    pub renaming_group_id: Option<i64>,
    pub rename_input: Option<Entity<InputState>>,
}

impl GroupRenameState {
    pub fn new() -> Self {
        Self {
            renaming_group_id: None,
            rename_input: None,
        }
    }

    /// Begin renaming a group inline: stash the id, (re)seed the rename
    /// `InputState` with the current name, and focus it.
    pub fn start<T: GroupRenameView + 'static>(
        &mut self,
        group_id: i64,
        current_name: String,
        window: &mut Window,
        cx: &mut Context<T>,
    ) {
        self.renaming_group_id = Some(group_id);
        if self.rename_input.is_none() {
            let entity = cx.new(|cx| {
                let state = InputState::new(window, cx).placeholder("new name");
                state.focus(window, cx);
                state
            });
            cx.subscribe(
                &entity,
                |this, _input, event: &gpui_component::input::InputEvent, cx| {
                    if let gpui_component::input::InputEvent::PressEnter { .. } = event {
                        let app = this.app_entity().clone();
                        this.group_rename().commit(&app, cx);
                    }
                },
            )
            .detach();
            let blur_handle = entity.read(cx).focus_handle(cx);
            cx.on_blur(&blur_handle, window, |this, _window, cx| {
                if this.group_rename().renaming_group_id.is_some() {
                    this.group_rename().cancel(cx);
                }
            })
            .detach();
            self.rename_input = Some(entity);
        }
        if let Some(ref input) = self.rename_input {
            input.update(cx, |state, cx| {
                state.set_value(current_name, window, cx);
                state.focus(window, cx);
            });
        }
        cx.notify();
    }

    /// Commit the inline rename: read the new name from `rename_input`,
    /// persist via `CrabportApp::rename_group`, and close the editor.
    pub fn commit<T: 'static>(&mut self, app: &Entity<CrabportApp>, cx: &mut Context<T>) {
        let group_id = match self.renaming_group_id.take() {
            Some(id) => id,
            None => return,
        };
        let new_name = self.rename_input.as_ref().and_then(|input| {
            let v = input.read(cx).value().to_string();
            if v.trim().is_empty() { None } else { Some(v) }
        });
        let Some(new_name) = new_name else {
            cx.notify();
            return;
        };
        app.update(cx, |app, cx| {
            app.rename_group(group_id, &new_name, cx);
        });
        cx.notify();
    }

    /// Abort the inline rename without persisting.
    pub fn cancel<T: 'static>(&mut self, cx: &mut Context<T>) {
        self.renaming_group_id = None;
        cx.notify();
    }
}

impl Default for GroupRenameState {
    fn default() -> Self {
        Self::new()
    }
}
