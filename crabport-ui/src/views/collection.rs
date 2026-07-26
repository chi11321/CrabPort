//! Shared render scaffold for the grouped full-page sidebar views
//! (Sessions / Snippets / Tunnels).
//!
//! All three views render the same page: a title + "New" header, a
//! separator, then a scrollable list of ungrouped rows, a virtual
//! "Favorites" group, and one collapsible section per real group (with the
//! same Rename / Delete group context menu), topped by an optional form
//! overlay. Only the row content, the callbacks, and a few constants (id
//! prefix, i18n keys, row height) differ — those come from the
//! [`GroupedListView`] trait; everything else lives in
//! [`render_grouped_list`] once.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::scroll::ScrollableElement as _;
use rust_i18n::t;

use crabport_core::credential::{GroupEntry, GroupKind};

use crate::app_state::AppState;
use crate::color::*;
use crate::components::button::Button;
use crate::components::context_menu::{
    ContextMenuController, ContextMenuItem, ContextMenuState, confirm_delete_item,
};
use crate::components::dialog::AlertController;
use crate::components::group_header::group_header;
use crate::components::list_row::collapsible_group_body;
use crate::views::group_rename::GroupRenameView;

/// Sentinel id used for the virtual "Favorites" group in collapse state.
/// Uses `i64::MAX` so it can never collide with a real group id.
pub const FAVORITES_GROUP_ID: i64 = i64::MAX;

/// Per-view configuration + hooks for [`render_grouped_list`].
///
/// The supertrait [`GroupRenameView`] supplies the inline group-rename
/// state and the owning `CrabportApp` entity (used for group rename /
/// delete and favorite toggles).
pub trait GroupedListView: GroupRenameView + Render + Sized + 'static {
    type Item: Clone;

    /// Element-id prefix ("host" / "snippet" / "tunnel") — keys row,
    /// group-header, and group-body element ids, so it must never change
    /// for an existing view (ids carry animation + hover state).
    const ID_PREFIX: &'static str;
    /// i18n namespace of the group context-menu keys
    /// (`{NS}.rename_group`, `{NS}.delete_group`,
    /// `{NS}.delete_group_prompt`, `{NS}.delete`).
    const MENU_NS: &'static str;
    const GROUP_KIND: GroupKind;
    /// Fixed row height used by the collapse animation.
    const ROW_HEIGHT: f32;
    /// Tunnels hides groups with no members; Sessions/Snippets render
    /// empty group headers.
    const SKIP_EMPTY_GROUPS: bool;
    /// Tunnels wraps each header+body pair in its own `flex_col` section
    /// div; Sessions/Snippets emit them as siblings of the outer list.
    const WRAP_SECTIONS: bool;

    const TITLE_KEY: &'static str;
    const NEW_BUTTON_ID: &'static str;
    const NEW_BUTTON_KEY: &'static str;
    const EMPTY_KEY: &'static str;

    fn items(&self) -> &[Self::Item];
    fn item_id(item: &Self::Item) -> i64;
    fn item_group_id(item: &Self::Item) -> Option<i64>;
    fn item_favorite(item: &Self::Item) -> bool;

    fn context_menu_handle(&self) -> Option<Entity<ContextMenuController>>;
    fn alert_handle(&self) -> Option<Entity<AlertController>>;
    fn on_new_cb(&self) -> Option<Rc<dyn Fn(&mut Window, &mut App)>>;

    fn hover_state(&self) -> Option<(i64, bool)>;
    fn menu_row_state(&mut self) -> &mut Option<(i64, bool)>;
    fn collapsed_groups(&mut self) -> &mut HashSet<i64>;

    /// Build one row. `is_favorite_copy` marks the duplicate rendered
    /// inside the virtual Favorites group (distinct element ids).
    fn render_row(
        &self,
        item: &Self::Item,
        is_favorite_copy: bool,
        is_hovered: bool,
        force_highlight: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement;

    /// Optional form overlay rendered on top of the list.
    fn render_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement>;
}

/// Build the rows for a slice of items, resolving per-row hover /
/// menu-highlight state from the copied snapshots.
fn rows_for<V: GroupedListView>(
    view: &V,
    cx: &mut Context<V>,
    slice: &[V::Item],
    hovered: Option<(i64, bool)>,
    menu_row: Option<(i64, bool)>,
    is_favorite_copy: bool,
) -> Vec<AnyElement> {
    slice
        .iter()
        .map(|item| {
            let id = V::item_id(item);
            view.render_row(
                item,
                is_favorite_copy,
                hovered == Some((id, is_favorite_copy)),
                menu_row == Some((id, is_favorite_copy)),
                cx,
            )
        })
        .collect()
}

/// The collapse-toggle closure shared by every group header.
fn toggle_collapse<V: GroupedListView>(
    entity: WeakEntity<V>,
    gid: i64,
) -> Rc<dyn Fn(&mut Window, &mut App)> {
    Rc::new(move |_w, cx| {
        let _ = entity.update(cx, |view, cx| {
            let set = view.collapsed_groups();
            if set.contains(&gid) {
                set.remove(&gid);
            } else {
                set.insert(gid);
            }
            cx.notify();
        });
    })
}

/// The right-click handler for a real group header: Rename Group +
/// Delete Group (with confirmation), i18n keys resolved from
/// `V::MENU_NS`.
fn group_menu_opener<V: GroupedListView>(
    group: &GroupEntry,
    entity: WeakEntity<V>,
    app: Entity<crate::app::CrabportApp>,
    context_menu: Option<Entity<ContextMenuController>>,
    alert: Option<Entity<AlertController>>,
) -> Rc<dyn Fn(&MouseDownEvent, &mut Window, &mut App)> {
    let group = group.clone();
    Rc::new(move |event, _w, cx| {
        let Some(ref cm) = context_menu else {
            return;
        };
        let pos = event.position;
        let alert = alert.clone();
        let app = app.clone();
        let group = group.clone();
        let entity = entity.clone();
        cm.update(cx, |c, cx| {
            let mut items: Vec<ContextMenuItem> = Vec::new();

            let rename_key = format!("{}.rename_group", V::MENU_NS);
            items.push(ContextMenuItem::new(t!(&rename_key).to_string(), {
                let group = group.clone();
                let entity = entity.clone();
                move |w, cx| {
                    let _ = entity.update(cx, |view, cx| {
                        view.group_rename()
                            .start(group.id, group.name.clone(), w, cx);
                    });
                }
            }));

            let delete_key = format!("{}.delete_group", V::MENU_NS);
            items.push(confirm_delete_item(
                t!(&delete_key).to_string(),
                alert.clone(),
                {
                    let name = group.name.clone();
                    move || {
                        let delete_key = format!("{}.delete_group", V::MENU_NS);
                        let prompt_key = format!("{}.delete_group_prompt", V::MENU_NS);
                        let confirm_key = format!("{}.delete", V::MENU_NS);
                        (
                            t!(&delete_key).to_string().into(),
                            Some(t!(&prompt_key, name = name.as_str()).to_string().into()),
                            t!(&confirm_key).to_string().into(),
                        )
                    }
                },
                Rc::new({
                    let app = app.clone();
                    let gid = group.id;
                    move |_w, cx| {
                        app.update(cx, |app, cx| {
                            app.remove_group(gid, V::GROUP_KIND, cx);
                        });
                    }
                }),
            ));

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
}

/// Render the whole grouped-list page for `view`. Called from each view's
/// `Render::render` impl.
pub fn render_grouped_list<V: GroupedListView>(
    view: &mut V,
    cx: &mut Context<V>,
) -> impl IntoElement + use<V> {
    // If the global context menu is no longer active, clear the
    // "menu-triggering row" highlight. Done in render (read-only on the
    // controller) because the menu's dismiss is async and there is no
    // direct hook into it.
    let menu_active = view
        .context_menu_handle()
        .map(|cm| cm.read_with(cx, |c, _| c.is_active()))
        .unwrap_or(false);
    if !menu_active {
        *view.menu_row_state() = None;
    }

    let items: Vec<V::Item> = view.items().to_vec();
    let hovered = view.hover_state();
    let menu_row = *view.menu_row_state();
    let collapsed = view.collapsed_groups().clone();
    let renaming_group_id = view.group_rename().renaming_group_id;
    let rename_input = view.group_rename().rename_input.clone();
    let context_menu = view.context_menu_handle();
    let alert = view.alert_handle();
    let on_new = view.on_new_cb();
    let entity = cx.entity().downgrade();

    // Load this view's groups once per render so newly-created groups
    // appear immediately (ordered by sort_order, id).
    let groups: Vec<GroupEntry> = AppState::store(cx)
        .lock()
        .groups(V::GROUP_KIND)
        .unwrap_or_default();

    // Partition items: ungrouped first, then one bucket per group. The
    // store's queries sort `favorite DESC, ...`, so favorites float to the
    // top of each bucket without further work here.
    let mut ungrouped: Vec<V::Item> = Vec::new();
    let mut grouped: HashMap<i64, Vec<V::Item>> = HashMap::new();
    for item in &items {
        match V::item_group_id(item) {
            Some(gid) => grouped.entry(gid).or_default().push(item.clone()),
            None => ungrouped.push(item.clone()),
        }
    }

    // Favorites bucket: every starred item, regardless of group. This is a
    // *virtual* group — the items still appear in their real groups below.
    let favorites: Vec<V::Item> = items
        .iter()
        .filter(|i| V::item_favorite(i))
        .cloned()
        .collect();
    let favorites_collapsed = collapsed.contains(&FAVORITES_GROUP_ID);

    // --- Ungrouped rows ---
    let ungrouped_rows = rows_for(view, cx, &ungrouped, hovered, menu_row, false);

    // --- Virtual Favorites section ---
    let favorites_section: Option<(AnyElement, AnyElement)> = (!favorites.is_empty()).then(|| {
        let header = group_header(
            V::ID_PREFIX,
            FAVORITES_GROUP_ID,
            t!("groups.favorites").to_string(),
            favorites.len(),
            favorites_collapsed,
            false,
            true,
            toggle_collapse(entity.clone(), FAVORITES_GROUP_ID),
            None,
            None,
            false,
            None,
        )
        .into_any_element();
        let rows = rows_for(view, cx, &favorites, hovered, menu_row, true);
        let body = collapsible_group_body(
            V::ID_PREFIX,
            FAVORITES_GROUP_ID,
            !favorites_collapsed,
            favorites.len(),
            V::ROW_HEIGHT,
            rows,
        );
        (header, body)
    });

    // --- One section per real group: header + animated body ---
    let mut group_sections: Vec<AnyElement> = Vec::new();
    for g in &groups {
        let members = grouped.remove(&g.id).unwrap_or_default();
        if V::SKIP_EMPTY_GROUPS && members.is_empty() {
            continue;
        }
        let is_collapsed = collapsed.contains(&g.id);
        let header = group_header(
            V::ID_PREFIX,
            g.id,
            g.name.clone(),
            members.len(),
            is_collapsed,
            g.favorite,
            false,
            toggle_collapse(entity.clone(), g.id),
            None,
            Some(group_menu_opener::<V>(
                g,
                entity.clone(),
                view.app_entity().clone(),
                context_menu.clone(),
                alert.clone(),
            )),
            renaming_group_id == Some(g.id),
            if renaming_group_id == Some(g.id) {
                rename_input.clone()
            } else {
                None
            },
        )
        .into_any_element();
        let rows = rows_for(view, cx, &members, hovered, menu_row, false);
        let body = collapsible_group_body(
            V::ID_PREFIX,
            g.id,
            !is_collapsed,
            members.len(),
            V::ROW_HEIGHT,
            rows,
        );
        if V::WRAP_SECTIONS {
            group_sections.push(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(header)
                    .child(body)
                    .into_any_element(),
            );
        } else {
            group_sections.push(header);
            group_sections.push(body);
        }
    }

    let overlay = view.render_overlay(cx);

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
                        .child(t!(V::TITLE_KEY).to_string()),
                )
                .child(
                    Button::new(V::NEW_BUTTON_ID)
                        .primary()
                        .icon("icons/plus.svg")
                        .w_auto()
                        .px_2()
                        .child(t!(V::NEW_BUTTON_KEY).to_string())
                        .on_click(move |_e, w, cx| {
                            if let Some(ref cb) = on_new {
                                cb(w, cx);
                            }
                        }),
                ),
        )
        // --- Separator ---
        .child(div().h_px().bg(rgb(border())).mx_4())
        // --- List (or empty state) ---
        .child(
            div()
                .flex_1()
                .overflow_y_scrollbar()
                .px_4()
                .py_2()
                .when_else(
                    items.is_empty(),
                    |el| {
                        el.flex().items_center().justify_center().child(
                            div()
                                .text_color(rgb(text_muted()))
                                .text_sm()
                                .child(t!(V::EMPTY_KEY).to_string()),
                        )
                    },
                    |el| {
                        el.flex()
                            .flex_col()
                            .gap_1()
                            .children(ungrouped_rows)
                            .when_some(favorites_section, |el, (header, body)| {
                                el.child(header).child(body)
                            })
                            .children(group_sections)
                    },
                ),
        )
        // --- Form overlay ---
        .when_some(overlay, |el, overlay| el.child(overlay))
}
