//! Snippets management view — the full-page sidebar view for managing saved
//! command snippets.
//!
//! Listed when the sidebar's "Snippets" item is active. Reads/writes the
//! same `snippets` Store table as the panel-tab Snippets view
//! ([`crate::views::panel::snippets_panel`]); the two are intentionally
//! distinct — the panel is a quick-run overlay next to the terminal, this
//! is the management surface (edit / delete).
//!
//! Layout mirrors [`crate::views::sessions::SessionsView`]:
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

use std::collections::HashSet;
use std::rc::Rc;

use gpui::*;
use rust_i18n::t;

use crabport_core::credential::GroupKind;

use crate::app::CrabportApp;
use crate::color::*;
use crate::components::context_menu::{
    ContextMenuController, ContextMenuItem, ContextMenuState, confirm_delete_item,
};
use crate::components::dialog::AlertController;
use crate::components::list_row::{favorite_star, interactive_row};
use crate::views::collection::{GroupedListView, render_grouped_list};
use crate::views::group_rename::{GroupRenameState, GroupRenameView};

// ---------------------------------------------------------------------------
// Submodules & re-exports
// ---------------------------------------------------------------------------

pub mod form;
pub use form::{SnippetFormOutput, SnippetFormState, SnippetFormView};

/// A snippet row shown in the management list.
#[derive(Clone)]
pub struct SnippetRow {
    pub id: i64,
    pub name: String,
    pub command: String,
    /// Starred by the user to pin it above un-starred snippets.
    pub favorite: bool,
    /// FK into the `groups` table. `None` = ungrouped.
    pub group_id: Option<i64>,
}

/// Snippets management view.
pub struct SnippetsView {
    /// The snippet row currently being hovered, if any. Keyed by
    /// `(id, is_favorite_copy)` so the favorites copy of an item and its
    /// real-group copy don't share hover state (they'd otherwise
    /// cross-highlight because both match the same id).
    hovered_snippet_id: Option<(i64, bool)>,
    /// The snippet row that triggered the currently-open context menu.
    context_menu_snippet_id: Option<(i64, bool)>,
    /// Snippet list, most-recently-created first. Reloaded from the Store
    /// before each render via `set_state`.
    snippets: Vec<SnippetRow>,
    /// Owning `CrabportApp` entity. Used to construct `SnippetFormView`
    /// (which needs an `Entity<CrabportApp>` to drive the save callback).
    app: Entity<CrabportApp>,
    /// Global context menu host (right-click Edit / Delete).
    context_menu: Option<Entity<ContextMenuController>>,
    /// Global alert dialog host (delete confirmation).
    alert_controller: Option<Entity<AlertController>>,
    /// "New" button callback — routes to `CrabportApp::open_snippet_form_for_create`.
    on_new: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    /// "Edit" context-menu callback — routes to
    /// `CrabportApp::open_snippet_form_for_edit`. Receives the snippet id.
    on_edit: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
    /// Snippet form dialog state, pushed in before each render. When
    /// `Some`, `SnippetFormView` is rendered on top of the list.
    form_state: Option<SnippetFormState>,
    /// Per-group collapse state for the grouped list. A group id present in
    /// this set renders its header with a right-chevron and hides its rows.
    collapsed_groups: HashSet<i64>,
    /// Shared inline group-rename state (id + InputState).
    group_rename: GroupRenameState,
}

impl SnippetsView {
    pub fn new(app: Entity<CrabportApp>) -> Self {
        Self {
            hovered_snippet_id: None,
            context_menu_snippet_id: None,
            snippets: Vec::new(),
            app,
            context_menu: None,
            alert_controller: None,
            on_new: None,
            on_edit: None,
            form_state: None,
            collapsed_groups: HashSet::new(),
            group_rename: GroupRenameState::new(),
        }
    }

    /// Push the latest external state into the view before render.
    /// `snippets` is re-read from the Store by the caller (`render_content`).
    #[allow(clippy::too_many_arguments)]
    pub fn set_state(
        &mut self,
        snippets: Vec<SnippetRow>,
        context_menu: Entity<ContextMenuController>,
        alert_controller: Entity<AlertController>,
        on_new: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
        on_edit: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
        form_state: Option<SnippetFormState>,
        cx: &mut Context<Self>,
    ) {
        // Clear stale hover if the snippet disappeared.
        if let Some((id, _)) = self.hovered_snippet_id
            && !snippets.iter().any(|s| s.id == id)
        {
            self.hovered_snippet_id = None;
        }
        self.snippets = snippets;
        self.context_menu = Some(context_menu);
        self.alert_controller = Some(alert_controller);
        self.on_new = on_new;
        self.on_edit = on_edit;
        self.form_state = form_state;
        let _ = cx;
    }

    /// Delete a snippet by id (after confirmation).
    fn delete_snippet(&mut self, id: i64, cx: &mut Context<Self>) {
        let store = crate::app_state::AppState::store(cx);
        let _ = store.lock().remove_snippet(id);
        cx.notify();
    }
}

impl GroupRenameView for SnippetsView {
    fn group_rename(&mut self) -> &mut GroupRenameState {
        &mut self.group_rename
    }

    fn app_entity(&self) -> &Entity<CrabportApp> {
        &self.app
    }
}

impl GroupedListView for SnippetsView {
    type Item = SnippetRow;

    const ID_PREFIX: &'static str = "snippet";
    const MENU_NS: &'static str = "snippets";
    const GROUP_KIND: GroupKind = GroupKind::Snippet;
    const ROW_HEIGHT: f32 = 62.0;
    const SKIP_EMPTY_GROUPS: bool = false;
    const WRAP_SECTIONS: bool = false;
    const TITLE_KEY: &'static str = "sidebar.snippets";
    const NEW_BUTTON_ID: &'static str = "snippets-new-btn";
    const NEW_BUTTON_KEY: &'static str = "snippets.new_button";
    const EMPTY_KEY: &'static str = "snippets.empty";

    fn items(&self) -> &[SnippetRow] {
        &self.snippets
    }
    fn item_id(item: &SnippetRow) -> i64 {
        item.id
    }
    fn item_group_id(item: &SnippetRow) -> Option<i64> {
        item.group_id
    }
    fn item_favorite(item: &SnippetRow) -> bool {
        item.favorite
    }

    fn context_menu_handle(&self) -> Option<Entity<ContextMenuController>> {
        self.context_menu.clone()
    }
    fn alert_handle(&self) -> Option<Entity<AlertController>> {
        self.alert_controller.clone()
    }
    fn on_new_cb(&self) -> Option<Rc<dyn Fn(&mut Window, &mut App)>> {
        self.on_new.clone()
    }

    fn hover_state(&self) -> Option<(i64, bool)> {
        self.hovered_snippet_id
    }
    fn menu_row_state(&mut self) -> &mut Option<(i64, bool)> {
        &mut self.context_menu_snippet_id
    }
    fn collapsed_groups(&mut self) -> &mut HashSet<i64> {
        &mut self.collapsed_groups
    }

    fn render_row(
        &self,
        snippet: &SnippetRow,
        is_favorite_copy: bool,
        is_hovered: bool,
        force_highlight: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        snippet_row(
            snippet,
            is_favorite_copy,
            is_hovered,
            force_highlight,
            cx.entity().downgrade(),
            self.context_menu.clone(),
            self.alert_controller.clone(),
            self.on_edit.clone(),
            self.app.clone(),
        )
        .into_any_element()
    }

    fn render_overlay(&self, _cx: &mut Context<Self>) -> Option<AnyElement> {
        self.form_state
            .as_ref()
            .map(|state| SnippetFormView::new(state, self.app.clone()).into_any_element())
    }
}

impl Render for SnippetsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        render_grouped_list(self, cx)
    }
}

// ---------------------------------------------------------------------------
// Snippet row
// ---------------------------------------------------------------------------

fn snippet_row(
    snippet: &SnippetRow,
    is_favorite_copy: bool,
    is_hovered: bool,
    force_highlight: bool,
    entity: WeakEntity<SnippetsView>,
    context_menu: Option<Entity<ContextMenuController>>,
    alert_controller: Option<Entity<AlertController>>,
    on_edit: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
    app: Entity<CrabportApp>,
) -> impl IntoElement {
    // The favorites bucket renders the same snippet a second time (the
    // item also appears under its real group below). Each instance needs
    // a distinct transition id, otherwise hover state is shared across
    // both and they animate in lockstep. Suffix "-fav" disambiguates the
    // copy.
    let row_id = ElementId::Name(
        format!(
            "snippet-row-{}{}",
            snippet.id,
            if is_favorite_copy { "-fav" } else { "" }
        )
        .into(),
    );

    let snippet_id = snippet.id;
    let snippet_name = snippet.name.clone();
    let snippet_command = snippet.command.clone();
    let is_favorite = snippet.favorite;
    let is_highlighted = is_hovered || force_highlight;

    // Snippet info (name + command)
    let info =
        div()
            .flex()
            .flex_col()
            .min_w_0()
            .flex_1()
            .child(div().text_sm().text_color(rgb(text_primary())).child(
                if snippet.name.is_empty() {
                    snippet_command.clone()
                } else {
                    snippet.name.clone()
                },
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(text_muted()))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(snippet_command.clone()),
            )
            .into_any_element();

    let star = favorite_star(
        "snippet",
        snippet_id,
        is_favorite,
        is_highlighted || is_favorite,
        {
            let app = app.clone();
            move |cx| {
                app.update(cx, |app, cx| {
                    app.toggle_snippet_favorite(snippet_id, cx);
                });
            }
        },
    )
    .into_any_element();

    interactive_row(
        row_id,
        is_highlighted,
        entity.clone(),
        (snippet_id, is_favorite_copy),
        |view| &mut view.hovered_snippet_id,
        // Right-click context menu: Favorite / Edit / Delete.
        move |el| {
            el.on_mouse_down(MouseButton::Right, {
                move |event, _w, cx| {
                    let Some(ref cm) = context_menu else {
                        return;
                    };
                    let _ = entity.update(cx, |view, cx| {
                        view.context_menu_snippet_id = Some((snippet_id, is_favorite_copy));
                        cx.notify();
                    });
                    let pos = event.position;
                    let entity_for_delete = entity.clone();
                    let alert_controller = alert_controller.clone();
                    let snippet_name = snippet_name.clone();
                    let on_edit = on_edit.clone();
                    let app = app.clone();
                    cm.update(cx, |c, cx| {
                        c.show(
                            ContextMenuState {
                                position: pos,
                                items: build_snippet_context_menu(
                                    snippet_id,
                                    is_favorite,
                                    on_edit,
                                    entity_for_delete.clone(),
                                    alert_controller.clone(),
                                    snippet_name.clone(),
                                    app,
                                ),
                                ..ContextMenuState::default()
                            },
                            cx,
                        );
                    });
                }
            })
        },
        vec![info, star],
    )
}

/// Build the right-click context menu items for a snippet.
///
/// Order: Favorite toggle, Edit, Delete.
fn build_snippet_context_menu(
    snippet_id: i64,
    is_favorite: bool,
    on_edit: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
    entity: WeakEntity<SnippetsView>,
    alert_controller: Option<Entity<AlertController>>,
    snippet_name: String,
    app: Entity<CrabportApp>,
) -> Vec<ContextMenuItem> {
    let mut items: Vec<ContextMenuItem> = Vec::new();

    // Favorite / Unfavorite toggle.
    let app_for_fav = app.clone();
    items.push(
        ContextMenuItem::new(
            if is_favorite {
                t!("snippets.unfavorite").to_string()
            } else {
                t!("snippets.favorite").to_string()
            },
            move |_w, cx| {
                app_for_fav.update(cx, |app, cx| {
                    app.toggle_snippet_favorite(snippet_id, cx);
                });
            },
        )
        .divider_after(),
    );

    // Edit
    items.push(ContextMenuItem::new(t!("snippets.edit").to_string(), {
        let on_edit = on_edit.clone();
        move |w, cx| {
            if let Some(ref cb) = on_edit {
                cb(snippet_id, w, cx);
            }
        }
    }));

    // Delete (with confirmation)
    items.push(confirm_delete_item(
        t!("snippets.delete").to_string(),
        alert_controller,
        {
            let snippet_name = snippet_name.clone();
            move || {
                (
                    t!("snippets.delete_title").to_string().into(),
                    Some(
                        t!("snippets.delete_prompt", name = snippet_name.as_str())
                            .to_string()
                            .into(),
                    ),
                    t!("snippets.delete_confirm").to_string().into(),
                )
            }
        },
        Rc::new(move |_w, cx| {
            let _ = entity.update(cx, |view, cx| {
                view.delete_snippet(snippet_id, cx);
            });
        }),
    ));

    items
}
