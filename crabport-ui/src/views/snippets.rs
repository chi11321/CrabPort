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

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::TransitionExt;
use gpui_component::scroll::ScrollableElement as _;
use rust_i18n::t;

use crabport_core::credential::{GroupEntry, GroupKind};

use crate::app::CrabportApp;
use crate::app_state::AppState;
use crate::color::*;
use crate::components::button::Button;
use crate::components::context_menu::{ContextMenuController, ContextMenuItem, ContextMenuState};
use crate::components::dialog::{AlertController, AlertSeverity, AlertState};
use crate::components::group_header::group_header;
use crate::motion::{duration_fast, duration_moderate, EASE_STANDARD, RADIUS_MD};
use crate::views::group_rename::{GroupRenameState, GroupRenameView};

// ---------------------------------------------------------------------------
// Submodules & re-exports
// ---------------------------------------------------------------------------

/// Sentinel id used for the virtual "Favorites" group in collapse state.
const FAVORITES_GROUP_ID: i64 = i64::MAX;

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

    // -------------------------------------------------------------------
    // Inline group rename (delegates to GroupRenameState)
    // -------------------------------------------------------------------

    fn start_group_rename(
        &mut self,
        group_id: i64,
        current_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.group_rename.start(group_id, current_name, window, cx);
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

impl Render for SnippetsView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let snippets = self.snippets.clone();
        let context_menu = self.context_menu.clone();
        let alert_controller = self.alert_controller.clone();
        let hovered_snippet_id = self.hovered_snippet_id;

        // Load snippet groups from the store on each render so newly-created
        // groups appear immediately (mirrors how the hosts view loads hosts).
        let groups: Vec<GroupEntry> = AppState::store(_cx)
            .lock()
            .groups(GroupKind::Snippet)
            .unwrap_or_default();

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
        let renaming_group_id = self.group_rename.renaming_group_id;
        let rename_input = self.group_rename.rename_input.clone();

        let on_new = self.on_new.clone();
        let on_edit = self.on_edit.clone();
        let form_state = self.form_state.clone();
        let app = self.app.clone();

        // Partition snippets: ungrouped (group_id is None) first, then one
        // bucket per group. The store returns snippets sorted by
        // `favorite DESC, id DESC`, so favorites float to the top of each
        // bucket without further work here.
        let mut ungrouped: Vec<&SnippetRow> = Vec::new();
        let mut grouped: std::collections::HashMap<i64, Vec<&SnippetRow>> =
            std::collections::HashMap::new();
        for s in &self.snippets {
            match s.group_id {
                Some(gid) => grouped.entry(gid).or_default().push(s),
                None => ungrouped.push(s),
            }
        }

        // Favorites bucket: every snippet with `favorite == true`, regardless
        // of which group it belongs to. This is a *virtual* group — the
        // items still appear in their real groups below.
        let favorites: Vec<&SnippetRow> = self.snippets.iter().filter(|s| s.favorite).collect();

        let collapsed_groups = self.collapsed_groups.clone();
        let favorites_collapsed = self.collapsed_groups.contains(&FAVORITES_GROUP_ID);

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
                            .text_color(rgb(text_primary()))
                            .child(t!("sidebar.snippets").to_string()),
                    )
                    .child(
                        Button::new("snippets-new-btn")
                            .primary()
                            .icon("icons/plus.svg")
                            .w_auto()
                            .px_2()
                            .child(t!("snippets.new_button").to_string())
                            .on_click(move |_e, w, cx| {
                                if let Some(ref cb) = on_new {
                                    cb(w, cx);
                                }
                            }),
                    ),
            )
            // --- Separator ---
            .child(div().h_px().bg(rgb(border())).mx_4())
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
                                    .text_color(rgb(text_muted()))
                                    .text_sm()
                                    .child(t!("snippets.empty").to_string()),
                            )
                        },
                        |el| {
                            el.flex()
                                .flex_col()
                                .gap_1()
                                // Ungrouped snippets (favorites float to top).
                                .children(ungrouped.iter().map(|s| {
                                    let snippet = (*s).clone();
                                    let context_menu = context_menu.clone();
                                    let alert_controller = alert_controller.clone();
                                    let is_hovered = hovered_snippet_id == Some((s.id, false));
                                    let force_highlight = context_menu_snippet_id == Some((s.id, false));
                                    let entity = _cx.entity().downgrade();
                                    let on_edit = on_edit.clone();
                                    let app = app.clone();
                                    let groups_for_menu = groups.clone();
                                    snippet_row(
                                        &snippet,
                                        false,
                                        is_hovered,
                                        force_highlight,
                                        entity,
                                        context_menu,
                                        alert_controller,
                                        on_edit,
                                        app,
                                        groups_for_menu,
                                    )
                                    .into_any_element()
                                }))
                                // --- Virtual Favorites group (all starred items) ---
                                .when(!favorites.is_empty(), |el| {
                                    let fav_count = favorites.len();
                                    let entity = _cx.entity().downgrade();
                                    let header = group_header(
                                        "snippet",
                                        FAVORITES_GROUP_ID,
                                        t!("groups.favorites").to_string(),
                                        favorites.len(),
                                        favorites_collapsed,
                                        false,
                                        true,
                                        {
                                            let entity = entity.clone();
                                            Rc::new(move |_w, cx| {
                                                let _ = entity.update(cx, |view, cx| {
                                                    if view
                                                        .collapsed_groups
                                                        .contains(&FAVORITES_GROUP_ID)
                                                    {
                                                        view.collapsed_groups
                                                            .remove(&FAVORITES_GROUP_ID);
                                                    } else {
                                                        view.collapsed_groups
                                                            .insert(FAVORITES_GROUP_ID);
                                                    }
                                                    cx.notify();
                                                });
                                            })
                                        },
                                        None,
                                        None,
                                        false,
                                        None,
                                    )
                                    .into_any_element();

                                    // Build rows for the favorites bucket.
                                    let fav_rows: Vec<AnyElement> = favorites
                                        .iter()
                                        .map(|s| {
                                            let snippet = (*s).clone();
                                            let context_menu = context_menu.clone();
                                            let alert_controller = alert_controller.clone();
                                            let is_hovered = hovered_snippet_id == Some((s.id, true));
                                            let force_highlight =
                                                context_menu_snippet_id == Some((s.id, true));
                                            let entity = _cx.entity().downgrade();
                                            let on_edit = on_edit.clone();
                                            let app = app.clone();
                                            let groups_for_menu = groups.clone();
                                            snippet_row(
                                                &snippet,
                                                true,
                                                is_hovered,
                                                force_highlight,
                                                entity,
                                                context_menu,
                                                alert_controller,
                                                on_edit,
                                                app,
                                                groups_for_menu,
                                            )
                                            .into_any_element()
                                        })
                                        .collect();

                                    let body_id = ElementId::Name(
                                        format!("snippet-group-body-{}", FAVORITES_GROUP_ID).into(),
                                    );
                                    let body = div()
                                        .id(body_id.clone())
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .overflow_hidden()
                                        .with_transition(body_id)
                                        .transition_when_else(
                                            !favorites_collapsed,
                                            duration_moderate(),
                                            EASE_STANDARD,
                                            move |el| {
                                                el.h(px(fav_count as f32 * 62.0 - 4.0)).opacity(1.0)
                                            },
                                            |el| el.h(px(0.0)).opacity(0.0),
                                        )
                                        .children(fav_rows)
                                        .into_any_element();

                                    el.child(header).child(body)
                                })
                                // One section per group: header immediately
                                // followed by its rows (wrapped in an
                                // animated container so collapse/expand
                                // eases height + opacity).
                                .children(groups.iter().flat_map(|g| {
                                    let gid = g.id;
                                    let group = g.clone();
                                    let members = grouped.get(&gid).cloned().unwrap_or_default();
                                    let member_count = members.len();
                                    let is_collapsed = collapsed_groups.contains(&gid);
                                    let entity = _cx.entity().downgrade();

                                    // Header first.
                                    let header = group_header(
                                        "snippet",
                                        gid,
                                        group.name.clone(),
                                        members.len(),
                                        is_collapsed,
                                        group.favorite,
                                        false,
                                        {
                                            let entity = entity.clone();
                                            Rc::new(move |_w, cx| {
                                                let _ = entity.update(cx, |view, cx| {
                                                    if view.collapsed_groups.contains(&gid) {
                                                        view.collapsed_groups.remove(&gid);
                                                    } else {
                                                        view.collapsed_groups.insert(gid);
                                                    }
                                                    cx.notify();
                                                });
                                            })
                                        },
                                        None,
                                        Some({
                                            let context_menu = context_menu.clone();
                                            let alert_controller = alert_controller.clone();
                                            let app = app.clone();
                                            let group_for_menu = group.clone();
                                            Rc::new(move |event, _w, cx| {
                                                let Some(ref cm) = context_menu else {
                                                    return;
                                                };
                                                let pos = event.position;
                                                let alert_controller = alert_controller.clone();
                                                let app = app.clone();
                                                let group = group_for_menu.clone();
                                                cm.update(cx, |c, cx| {
                                                    let mut items: Vec<ContextMenuItem> = Vec::new();

                                                    // Rename Group
                                                    items.push(ContextMenuItem::new(
                                                        t!("snippets.rename_group").to_string(),
                                                        {
                                                            let group = group.clone();
                                                            let entity = entity.clone();
                                                            move |w, cx| {
                                                                let _ = entity.update(cx, |view, cx| {
                                                                    view.start_group_rename(group.id, group.name.clone(), w, cx);
                                                                });
                                                            }
                                                        },
                                                    ));

                                                    // Delete Group (with confirmation)
                                                    items.push(
                                                        ContextMenuItem::new(
                                                            t!("snippets.delete_group").to_string(),
                                                            {
                                                                let alert_controller =
                                                                    alert_controller.clone();
                                                                let app = app.clone();
                                                                let group = group.clone();
                                                                move |_w, cx| {
                                                                    let Some(ref ac) =
                                                                        alert_controller
                                                                    else {
                                                                        return;
                                                                    };
                                                                    let app = app.clone();
                                                                    let gid = group.id;
                                                                    let name =
                                                                        group.name.clone();
                                                                    ac.update(cx, |c, cx| {
                                                                        c.show(
                                                                            AlertState {
                                                                                severity:
                                                                                    AlertSeverity::Danger,
                                                                                title: t!(
                                                                                    "snippets.delete_group"
                                                                                )
                                                                                .to_string()
                                                                                .into(),
                                                                                description: Some(
                                                                                    t!(
                                                                                        "snippets.delete_group_prompt",
                                                                                        name = name.as_str()
                                                                                    )
                                                                                    .to_string()
                                                                                    .into(),
                                                                                ),
                                                                                confirm_label: t!(
                                                                                    "snippets.delete"
                                                                                )
                                                                                .to_string()
                                                                                .into(),
                                                                                cancel_label: t!(
                                                                                    "terminal.host_key_cancel"
                                                                                )
                                                                                .to_string()
                                                                                .into(),
                                                                                on_confirm: Some(
                                                                                    Rc::new(move |_w, cx| {
                                                                                        app.update(cx, |app, cx| {
                                                                                            app.remove_group(gid, crabport_core::credential::GroupKind::Snippet, cx);
                                                                                        });
                                                                                    }),
                                                                                ),
                                                                                ..AlertState::default()
                                                                            },
                                                                            cx,
                                                                        );
                                                                    });
                                                                }
                                                            },
                                                        )
                                                        .danger(true),
                                                    );

                                                    c.show(
                                                        ContextMenuState {
                                                            position: pos,
                                                            items,
                                                            ..ContextMenuState::default()
                                                        },
                                                        cx,
                                                    );
                                                });
                                            })
                                        }),
                                        renaming_group_id == Some(gid),
                                        if renaming_group_id == Some(gid) {
                                            rename_input.clone()
                                        } else {
                                            None
                                        },
                                    )
                                    .into_any_element();

                                    // Build rows always (even when collapsed) so
                                    // the expand animation has content to reveal.
                                    let rows: Vec<AnyElement> = members
                                        .iter()
                                        .map(|s| {
                                            let snippet = (*s).clone();
                                            let context_menu = context_menu.clone();
                                            let alert_controller = alert_controller.clone();
                                            let is_hovered = hovered_snippet_id == Some((s.id, false));
                                            let force_highlight =
                                                context_menu_snippet_id == Some((s.id, false));
                                            let entity = _cx.entity().downgrade();
                                            let on_edit = on_edit.clone();
                                            let app = app.clone();
                                            let groups_for_menu = groups.clone();
                                            snippet_row(
                                                &snippet,
                                                false,
                                                is_hovered,
                                                force_highlight,
                                                entity,
                                                context_menu,
                                                alert_controller,
                                                on_edit,
                                                app,
                                                groups_for_menu,
                                            )
                                            .into_any_element()
                                        })
                                        .collect();

                                    // Animated body: collapses to h_0 +
                                    // opacity_0 when collapsed.
                                    let body_id = ElementId::Name(
                                        format!("snippet-group-body-{}", gid).into(),
                                    );
                                    let body = div()
                                        .id(body_id.clone())
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .overflow_hidden()
                                        .with_transition(body_id)
                                        .transition_when_else(
                                            !is_collapsed,
                                            duration_moderate(),
                                            EASE_STANDARD,
                                            move |el| {
                                                el.h(px(member_count as f32 * 62.0 - 4.0))
                                                    .opacity(1.0)
                                            },
                                            |el| el.h(px(0.0)).opacity(0.0),
                                        )
                                        .children(rows)
                                        .into_any_element();

                                    std::iter::once(header)
                                        .chain(std::iter::once(body))
                                        .collect::<Vec<_>>()
                                }))
                        },
                    ),
            )
            // --- Snippet form overlay (create/edit) ---
            .when_some(form_state, move |el, state| {
                el.child(SnippetFormView::new(&state, app))
            })
    }
}

// ---------------------------------------------------------------------------
// Snippet row
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
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
    groups: Vec<GroupEntry>,
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

    div()
        .id(row_id.clone())
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .px_3()
        .py_2()
        .rounded(RADIUS_MD)
        .bg(rgb(bg_base()))
        // Right-click context menu: Favorite / Move-to-Group / Edit / Delete.
        .on_mouse_down(MouseButton::Right, {
            let entity = entity.clone();
            let app = app.clone();
            let groups = groups.clone();
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
                let groups = groups.clone();
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
                                groups,
                            ),
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
                    view.hovered_snippet_id = Some((snippet_id, is_favorite_copy));
                } else if view.hovered_snippet_id == Some((snippet_id, is_favorite_copy)) {
                    view.hovered_snippet_id = None;
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
        // Snippet info (name + command)
        .child(
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
                ),
        )
        // Favorite star toggle (far right). Fades in on hover; stays visible
        // (yellow) when already favorited so the user can see + unstar.
        .child({
            let app = app.clone();
            let star_id = ElementId::Name(format!("snippet-star-{}", snippet.id).into());
            let star_visible = is_highlighted || is_favorite;
            div()
                .id(star_id.clone())
                .flex()
                .items_center()
                .justify_center()
                .child(
                    svg()
                        .path("icons/star.svg")
                        .size_4()
                        .text_color(rgb(if is_favorite {
                            term_yellow()
                        } else {
                            text_muted()
                        })),
                )
                .with_transition(star_id)
                .transition_when_else(
                    star_visible,
                    duration_fast(),
                    EASE_STANDARD,
                    |el| el.opacity(1.0),
                    |el| el.opacity(0.0),
                )
                .on_click(move |_e, _w, cx| {
                    app.update(cx, |app, cx| {
                        app.toggle_snippet_favorite(snippet_id, cx);
                    });
                })
        })
}

/// Build the right-click context menu items for a snippet.
///
/// Order: Favorite toggle, Move-to-Group (ungrouped + one per group + New
/// Group…), Edit, Delete.
#[allow(clippy::too_many_arguments)]
fn build_snippet_context_menu(
    snippet_id: i64,
    is_favorite: bool,
    on_edit: Option<Rc<dyn Fn(i64, &mut Window, &mut App)>>,
    entity: WeakEntity<SnippetsView>,
    alert_controller: Option<Entity<AlertController>>,
    snippet_name: String,
    app: Entity<CrabportApp>,
    _groups: Vec<GroupEntry>,
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
    items.push(
        ContextMenuItem::new(t!("snippets.delete").to_string(), {
            let entity = entity.clone();
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
                            title: t!("snippets.delete_title").to_string().into(),
                            description: Some(
                                t!("snippets.delete_prompt", name = snippet_name.as_str())
                                    .to_string()
                                    .into(),
                            ),
                            confirm_label: t!("snippets.delete_confirm").to_string().into(),
                            cancel_label: t!("terminal.host_key_cancel").to_string().into(),
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
    );

    items
}
