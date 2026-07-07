//! Tunnel form dialog (create / edit).
//!
//! Mirrors the overlay-dialog pattern used by `ConnectionFormState` /
//! `ConnectionFormView` in `crabport-ui/src/layouts/connection_form.rs`:
//! - `TunnelFormState` is owned by `CrabportApp` and holds `Entity<InputState>`
//!   fields plus open/close animation state.
//! - `TunnelFormView` is a pure `RenderOnce` renderer that reads a snapshot of
//!   the state and emits an absolute overlay + centered dialog.
//!
//! The view does NOT persist anything itself — it reads its inputs, packages
//! them into a `TunnelFormOutput`, and invokes the `on_save` callback. The
//! caller (`CrabportApp`) is responsible for store CRUD + registry mutation.

use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::{animation::TransitionExt, transition::general::Linear};
use gpui_component::input::InputState;
use rust_i18n::t;

use crabport_core::credential::{GroupEntry, GroupKind, TunnelEntry, TunnelKind};

use crate::app::CrabportApp;
use crate::app_state::AppState;
use crate::color::*;
use crate::components::button::Button;
use crate::components::dropdown::Dropdown;
use crate::components::input::StyledInput;
use crate::components::tabs::{TabPane, Tabs};
use crate::views::hosts::ConnectionHost;

// ---------------------------------------------------------------------------
// TunnelKind helpers (Local / Remote / Dynamic ↔ tab index)
// ---------------------------------------------------------------------------

/// Map a `TunnelKind` to the index of its tab in the kind selector.
fn kind_as_tab_index(kind: TunnelKind) -> usize {
    match kind {
        TunnelKind::Local => 0,
        TunnelKind::Remote => 1,
        TunnelKind::Dynamic => 2,
    }
}

/// Map a tab index back to a `TunnelKind`.
fn kind_from_tab_index(i: usize) -> TunnelKind {
    match i {
        1 => TunnelKind::Remote,
        2 => TunnelKind::Dynamic,
        _ => TunnelKind::Local,
    }
}

// ---------------------------------------------------------------------------
// Output passed to the save callback
// ---------------------------------------------------------------------------

/// Parsed form values handed to the `on_save` callback. The caller resolves
/// `editing_id` against the store (UPDATE if `Some`, INSERT if `None`).
#[derive(Clone, Debug)]
pub struct TunnelFormOutput {
    pub editing_id: Option<i64>,
    pub name: String,
    pub host_id: i64,
    pub kind: TunnelKind,
    pub bind_addr: String,
    pub bind_port: u16,
    pub target_host: String,
    pub target_port: u16,
    /// Starred by the user to pin it above un-starred tunnels.
    pub favorite: bool,
    /// FK into the `groups` table. `None` = ungrouped.
    pub group_id: Option<i64>,
}

// ---------------------------------------------------------------------------
// TunnelValidationErrors — per-field error strings shown via StyledInput.error()
// ---------------------------------------------------------------------------

/// Per-field validation errors for the tunnel form. A field is `Some` when it
/// has an error to display; `None` means it passed validation.
#[derive(Clone, Default)]
pub struct TunnelValidationErrors {
    pub host: Option<SharedString>,
    pub bind_addr: Option<SharedString>,
    pub bind_port: Option<SharedString>,
    pub target_host: Option<SharedString>,
    pub target_port: Option<SharedString>,
}

impl TunnelValidationErrors {
    pub fn is_empty(&self) -> bool {
        self.host.is_none()
            && self.bind_addr.is_none()
            && self.bind_port.is_none()
            && self.target_host.is_none()
            && self.target_port.is_none()
    }
}

// ---------------------------------------------------------------------------
// TunnelFormState — owned by CrabportApp
// ---------------------------------------------------------------------------

/// Holds all mutable state for the tunnel form overlay so that
/// `TunnelFormView` can be a pure `RenderOnce` renderer.
#[derive(Clone)]
pub struct TunnelFormState {
    /// `Some(id)` when editing an existing tunnel; `None` when creating.
    pub editing_id: Option<i64>,
    /// Local / Remote / Dynamic — selected via the kind tabs.
    pub tunnel_kind: TunnelKind,
    pub name_input: Entity<InputState>,
    /// FK into `hosts.id` of the selected host. `None` until the user picks
    /// one from the dropdown. The form cannot be saved until this is `Some`.
    pub host_id: Option<i64>,
    pub bind_addr_input: Entity<InputState>,
    pub bind_port_input: Entity<InputState>,
    pub target_host_input: Entity<InputState>,
    pub target_port_input: Entity<InputState>,
    // Focus states (mirrors ConnectionFormState)
    pub name_focused: bool,
    pub bind_addr_focused: bool,
    pub bind_port_focused: bool,
    pub target_host_focused: bool,
    pub target_port_focused: bool,
    // Host dropdown open state — owned here so the renderer is a pure
    // function of the state.
    pub host_dropdown_open: bool,
    /// Group dropdown open state (mirrors `host_dropdown_open` for the
    /// group selector).
    pub group_dropdown_open: bool,
    /// Search input for the group dropdown (filtering + create). Owned here
    /// so the query survives re-renders.
    pub group_search_input: Entity<InputState>,
    /// Open/close animation state. `true` while the overlay is visible
    /// (drives the backdrop fade + dialog slide-in transition).
    pub open: bool,
    /// Per-field validation errors. Populated by `validate()` and rendered
    /// via `StyledInput.error(...)` on the relevant fields. Cleared on open.
    pub errors: TunnelValidationErrors,
    /// Starred flag, edited via a star toggle in the form.
    pub favorite: bool,
    /// FK into the `groups` table. `None` = ungrouped. Edited via a group
    /// dropdown in the form.
    pub group_id: Option<i64>,
    pub on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    pub on_save: Option<Rc<dyn Fn(TunnelFormOutput, &mut Window, &mut App) + 'static>>,
}

impl TunnelFormState {
    pub fn new(window: &mut Window, cx: &mut App) -> Self {
        let name_input = cx.new(|cx| InputState::new(window, cx));
        let bind_addr_input = cx.new(|cx| InputState::new(window, cx));
        let bind_port_input = cx.new(|cx| InputState::new(window, cx));
        let target_host_input = cx.new(|cx| InputState::new(window, cx));
        let target_port_input = cx.new(|cx| InputState::new(window, cx));
        let group_search_input = cx.new(|cx| InputState::new(window, cx));

        Self {
            editing_id: None,
            tunnel_kind: TunnelKind::Local,
            name_input,
            host_id: None,
            bind_addr_input,
            bind_port_input,
            target_host_input,
            target_port_input,
            name_focused: false,
            bind_addr_focused: false,
            bind_port_focused: false,
            target_host_focused: false,
            target_port_focused: false,
            host_dropdown_open: false,
            group_dropdown_open: false,
            group_search_input,
            open: false,
            errors: TunnelValidationErrors::default(),
            favorite: false,
            group_id: None,
            on_close: None,
            on_save: None,
        }
    }

    pub fn open(&mut self, window: &mut Window, cx: &mut App) {
        self.open = true;
        self.errors = TunnelValidationErrors::default();
        self.name_input.update(cx, |state, cx| {
            state.focus(window, cx);
        });
    }

    pub fn close(&mut self) {
        self.open = false;
        self.host_dropdown_open = false;
        self.group_dropdown_open = false;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Reset all fields to blank defaults and open the dialog in create mode.
    pub fn open_for_create(&mut self, window: &mut Window, cx: &mut App) {
        self.editing_id = None;
        self.tunnel_kind = TunnelKind::Local;
        self.host_id = None;
        self.host_dropdown_open = false;
        self.group_dropdown_open = false;
        self.group_search_input
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.favorite = false;
        self.group_id = None;
        for input in [
            &self.name_input,
            &self.bind_addr_input,
            &self.bind_port_input,
            &self.target_host_input,
            &self.target_port_input,
        ] {
            input.update(cx, |state, cx| {
                state.set_value("", window, cx);
            });
        }
        // Default bind address + port matches `ssh -L 127.0.0.1:2333:...`.
        self.bind_addr_input.update(cx, |state, cx| {
            state.set_value("127.0.0.1", window, cx);
        });
        self.bind_port_input.update(cx, |state, cx| {
            state.set_value("2333", window, cx);
        });
        self.open(window, cx);
    }

    /// Populate the fields from an existing `TunnelEntry`, set `editing_id`,
    /// and open the dialog in edit mode.
    pub fn open_for_edit(&mut self, entry: &TunnelEntry, window: &mut Window, cx: &mut App) {
        self.editing_id = Some(entry.id);
        self.tunnel_kind = entry.kind;
        self.host_id = Some(entry.host_id);
        self.host_dropdown_open = false;
        self.group_dropdown_open = false;
        self.group_search_input
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.favorite = entry.favorite;
        self.group_id = entry.group_id;
        self.name_input
            .update(cx, |state, cx| state.set_value(&entry.name, window, cx));
        self.bind_addr_input.update(cx, |state, cx| {
            state.set_value(&entry.bind_addr, window, cx)
        });
        self.bind_port_input.update(cx, |state, cx| {
            state.set_value(&entry.bind_port.to_string(), window, cx);
        });
        self.target_host_input.update(cx, |state, cx| {
            state.set_value(&entry.target_host, window, cx)
        });
        self.target_port_input.update(cx, |state, cx| {
            state.set_value(&entry.target_port.to_string(), window, cx);
        });
        self.open(window, cx);
    }

    pub fn name_text(&self, cx: &App) -> String {
        self.name_input.read(cx).text().to_string()
    }

    pub fn bind_addr_text(&self, cx: &App) -> String {
        self.bind_addr_input.read(cx).text().to_string()
    }

    pub fn bind_port_text(&self, cx: &App) -> String {
        self.bind_port_input.read(cx).text().to_string()
    }

    pub fn target_host_text(&self, cx: &App) -> String {
        self.target_host_input.read(cx).text().to_string()
    }

    pub fn target_port_text(&self, cx: &App) -> String {
        self.target_port_input.read(cx).text().to_string()
    }

    pub fn set_host_id(&mut self, id: i64) {
        self.host_id = Some(id);
    }

    pub fn set_kind(&mut self, kind: TunnelKind) {
        self.tunnel_kind = kind;
    }

    /// Build a `TunnelFormOutput` from the current form state. Parses the port
    /// inputs to `u16` (defaulting to `0` on parse failure — the caller should
    /// validate before persisting).
    pub fn output(&self, cx: &App) -> TunnelFormOutput {
        TunnelFormOutput {
            editing_id: self.editing_id,
            name: self.name_text(cx),
            host_id: self.host_id.unwrap_or(0),
            kind: self.tunnel_kind,
            bind_addr: self.bind_addr_text(cx),
            bind_port: self.bind_port_text(cx).parse().unwrap_or(0),
            target_host: self.target_host_text(cx),
            target_port: self.target_port_text(cx).parse().unwrap_or(0),
            favorite: self.favorite,
            group_id: self.group_id,
        }
    }

    /// Validate the form against the required-field rules. Populates
    /// `self.errors` and returns `true` if the form is valid (no errors).
    ///
    /// Rules:
    /// - A host must be selected (`host_id` is `Some`).
    /// - Bind address is required (all kinds).
    /// - Bind port is required and must parse as `u16` (all kinds).
    /// - For Local / Remote: target host and target port are required.
    /// - For Dynamic: target host / target port are not used (no check).
    /// - Name is optional.
    pub fn validate(&mut self, cx: &App) -> bool {
        let mut errors = TunnelValidationErrors::default();

        if self.host_id.is_none() {
            errors.host = Some(t!("tunnel_form.error_host_required").into());
        }

        if self.bind_addr_text(cx).trim().is_empty() {
            errors.bind_addr = Some(t!("tunnel_form.error_bind_addr_required").into());
        }

        let bp_text = self.bind_port_text(cx);
        if bp_text.trim().is_empty() {
            errors.bind_port = Some(t!("tunnel_form.error_bind_port_required").into());
        } else if bp_text.trim().parse::<u16>().is_err() {
            errors.bind_port = Some(t!("tunnel_form.error_port_invalid").into());
        }

        // Target host / port are only required for Local / Remote.
        if matches!(self.tunnel_kind, TunnelKind::Local | TunnelKind::Remote) {
            if self.target_host_text(cx).trim().is_empty() {
                errors.target_host = Some(t!("tunnel_form.error_target_host_required").into());
            }
            let tp_text = self.target_port_text(cx);
            if tp_text.trim().is_empty() {
                errors.target_port = Some(t!("tunnel_form.error_target_port_required").into());
            } else if tp_text.trim().parse::<u16>().is_err() {
                errors.target_port = Some(t!("tunnel_form.error_port_invalid").into());
            }
        }

        let ok = errors.is_empty();
        self.errors = errors;
        ok
    }
}

// ---------------------------------------------------------------------------
// TunnelFormView — pure RenderOnce renderer
// ---------------------------------------------------------------------------

#[derive(IntoElement)]
pub struct TunnelFormView {
    open: bool,
    editing: bool,
    tunnel_kind: TunnelKind,
    name_input: Entity<InputState>,
    bind_addr_input: Entity<InputState>,
    bind_port_input: Entity<InputState>,
    target_host_input: Entity<InputState>,
    target_port_input: Entity<InputState>,
    host_id: Option<i64>,
    host_dropdown_open: bool,
    group_dropdown_open: bool,
    group_search_input: Entity<InputState>,
    hosts: Vec<ConnectionHost>,
    name_focused: bool,
    bind_addr_focused: bool,
    bind_port_focused: bool,
    target_host_focused: bool,
    target_port_focused: bool,
    errors: TunnelValidationErrors,
    app: Entity<CrabportApp>,
    /// FK into the `groups` table. `None` = ungrouped. Snapshot from
    // `TunnelFormState::group_id`.
    group_id: Option<i64>,
    /// Tunnel groups loaded from the store, for the group dropdown.
    groups: Vec<GroupEntry>,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    on_save: Option<Rc<dyn Fn(TunnelFormOutput, &mut Window, &mut App) + 'static>>,
}

impl TunnelFormView {
    pub fn new(
        state: &TunnelFormState,
        app: Entity<CrabportApp>,
        hosts: Vec<ConnectionHost>,
        cx: &mut App,
    ) -> Self {
        // Load tunnel groups from the store for the group dropdown.
        let groups = AppState::store(cx)
            .lock()
            .groups(GroupKind::Tunnel)
            .unwrap_or_default();
        Self {
            open: state.open,
            editing: state.editing_id.is_some(),
            tunnel_kind: state.tunnel_kind,
            name_input: state.name_input.clone(),
            bind_addr_input: state.bind_addr_input.clone(),
            bind_port_input: state.bind_port_input.clone(),
            target_host_input: state.target_host_input.clone(),
            target_port_input: state.target_port_input.clone(),
            host_id: state.host_id,
            host_dropdown_open: state.host_dropdown_open,
            group_dropdown_open: state.group_dropdown_open,
            group_search_input: state.group_search_input.clone(),
            hosts,
            name_focused: state.name_focused,
            bind_addr_focused: state.bind_addr_focused,
            bind_port_focused: state.bind_port_focused,
            target_host_focused: state.target_host_focused,
            target_port_focused: state.target_port_focused,
            errors: state.errors.clone(),
            app,
            group_id: state.group_id,
            groups,
            on_close: state.on_close.clone(),
            on_save: state.on_save.clone(),
        }
    }
}

impl RenderOnce for TunnelFormView {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let on_close_for_dialog = self.on_close.clone();

        render_overlay(
            self.open,
            self.on_close,
            render_dialog(
                self.open,
                self.editing,
                self.tunnel_kind,
                self.name_input,
                self.bind_addr_input,
                self.bind_port_input,
                self.target_host_input,
                self.target_port_input,
                self.host_id,
                self.host_dropdown_open,
                self.group_dropdown_open,
                self.group_search_input,
                self.hosts,
                self.name_focused,
                self.bind_addr_focused,
                self.bind_port_focused,
                self.target_host_focused,
                self.target_port_focused,
                self.errors,
                self.group_id,
                self.groups,
                self.app,
                on_close_for_dialog,
                self.on_save,
            ),
        )
    }
}

// ---------------------------------------------------------------------------
// Render helpers
// ---------------------------------------------------------------------------

fn render_overlay(
    open: bool,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    child: impl IntoElement,
) -> impl IntoElement {
    let overlay_id = ElementId::Name("tunnel-form-overlay".into());

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
        .when(open, |el| {
            el.occlude().on_click(move |_e, w, cx| {
                if let Some(ref cb) = on_close {
                    cb(w, cx);
                }
            })
        })
        .with_transition(overlay_id)
        .transition_when_else(
            open,
            Duration::from_millis(150),
            Linear,
            |el| el.bg(rgba(0x00000080)),
            |el| el.bg(rgba(0x00000000)),
        )
        .child(child)
}

#[allow(clippy::too_many_arguments)]
fn render_dialog(
    open: bool,
    editing: bool,
    tunnel_kind: TunnelKind,
    name_input: Entity<InputState>,
    bind_addr_input: Entity<InputState>,
    bind_port_input: Entity<InputState>,
    target_host_input: Entity<InputState>,
    target_port_input: Entity<InputState>,
    host_id: Option<i64>,
    host_dropdown_open: bool,
    group_dropdown_open: bool,
    group_search_input: Entity<InputState>,
    hosts: Vec<ConnectionHost>,
    name_focused: bool,
    bind_addr_focused: bool,
    bind_port_focused: bool,
    target_host_focused: bool,
    target_port_focused: bool,
    errors: TunnelValidationErrors,
    group_id: Option<i64>,
    groups: Vec<GroupEntry>,
    app: Entity<CrabportApp>,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    on_save: Option<Rc<dyn Fn(TunnelFormOutput, &mut Window, &mut App) + 'static>>,
) -> impl IntoElement {
    let dialog_id = ElementId::Name("tunnel-form-dialog".into());

    let title = if editing {
        t!("tunnel_form.title_edit").to_string()
    } else {
        t!("tunnel_form.title_new").to_string()
    };

    let active_kind_index = kind_as_tab_index(tunnel_kind);

    // Hint text under the kind selector, varies by kind.
    let hint = match tunnel_kind {
        TunnelKind::Local => t!("tunnel_form.hint_local").to_string(),
        TunnelKind::Remote => t!("tunnel_form.hint_remote").to_string(),
        TunnelKind::Dynamic => t!("tunnel_form.hint_dynamic").to_string(),
    };

    // Per-kind pane content height (used so the Tabs component can animate
    // Per-kind pane content height (used so the Tabs component can animate
    // max_height between kinds). Local/Remote show 4 fields; Dynamic hides
    // target_host/target_port.
    // StyledInput single-line: label(19) + gap_1(4) + shell(32) = 55px per
    // field (text_xs line_height = 12*1.618 = 19px). Plus gap_4 (16) between
    // fields. Bind row uses a 2-col layout so it occupies one field height.
    // +1px per field for font metric rounding.
    let kind_pane_height: f32 = match tunnel_kind {
        TunnelKind::Local | TunnelKind::Remote => {
            // bind row + gap_4 + target_host + gap_4 + target_port
            56.0 + 16.0 + 56.0 + 16.0 + 56.0
        }
        TunnelKind::Dynamic => {
            // bind row only
            56.0
        }
    };

    div()
        .id(dialog_id.clone())
        .w(px(440.0))
        .bg(rgb(bg_base()))
        .border_1()
        .border_color(rgb(border()))
        .rounded_lg()
        .shadow_lg()
        .flex()
        .flex_col()
        .p_6()
        .gap_4()
        .opacity(0.0)
        .mt(px(-16.0))
        .when(open, |el| {
            el.on_click(|_, _, cx| {
                cx.stop_propagation();
            })
        })
        .with_transition(dialog_id)
        .transition_when_else(
            open,
            Duration::from_millis(150),
            Linear,
            |el| el.opacity(1.0).mt_0(),
            |el| el.opacity(0.0).mt(px(-16.0)),
        )
        // Title
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(text_primary()))
                .child(title),
        )
        // Name
        .child(
            StyledInput::new("tunnel-name", name_input)
                .label(t!("tunnel_form.name").to_string())
                .focused(name_focused),
        )
        // Group selector (dropdown of tunnel groups, or "None" = ungrouped).
        .child(render_group_selector(
            group_id,
            group_dropdown_open,
            groups,
            group_search_input,
            app.clone(),
        ))
        // Host selector (dropdown of saved hosts, or a hint if none).
        .child(render_host_selector(
            host_id,
            host_dropdown_open,
            hosts,
            errors.host.clone(),
            app.clone(),
        ))
        // Kind tabs (Local / Remote / Dynamic). Each pane shows the
        // bind/target fields appropriate for that kind.
        .child(
            Tabs::new("tunnel-kind-tabs")
                .active(active_kind_index)
                .pane(render_kind_pane(
                    TunnelKind::Local,
                    bind_addr_input.clone(),
                    bind_port_input.clone(),
                    target_host_input.clone(),
                    target_port_input.clone(),
                    bind_addr_focused,
                    bind_port_focused,
                    target_host_focused,
                    target_port_focused,
                    errors.bind_addr.clone(),
                    errors.bind_port.clone(),
                    errors.target_host.clone(),
                    errors.target_port.clone(),
                ))
                .pane(render_kind_pane(
                    TunnelKind::Remote,
                    bind_addr_input.clone(),
                    bind_port_input.clone(),
                    target_host_input.clone(),
                    target_port_input.clone(),
                    bind_addr_focused,
                    bind_port_focused,
                    target_host_focused,
                    target_port_focused,
                    errors.bind_addr.clone(),
                    errors.bind_port.clone(),
                    errors.target_host.clone(),
                    errors.target_port.clone(),
                ))
                .pane(render_kind_pane(
                    TunnelKind::Dynamic,
                    bind_addr_input.clone(),
                    bind_port_input.clone(),
                    target_host_input.clone(),
                    target_port_input.clone(),
                    bind_addr_focused,
                    bind_port_focused,
                    target_host_focused,
                    target_port_focused,
                    errors.bind_addr.clone(),
                    errors.bind_port.clone(),
                    errors.target_host.clone(),
                    errors.target_port.clone(),
                ))
                .on_change({
                    let app = app.clone();
                    move |index, _w, cx| {
                        let kind = kind_from_tab_index(index);
                        app.update(cx, |app, cx| {
                            if let Some(ref mut form) = app.tunnel_form {
                                form.tunnel_kind = kind;
                                cx.notify();
                            }
                        });
                    }
                }),
        )
        // Kind-specific hint
        .child(div().text_xs().text_color(rgb(text_muted())).child(hint))
        // Buttons
        .child(render_buttons(
            editing,
            tunnel_kind,
            kind_pane_height,
            app,
            on_close,
            on_save,
        ))
}

/// Build a `TabPane` for one TunnelKind. The pane content adapts to the kind:
/// Local/Remote show bind + target fields; Dynamic shows only bind fields.
/// The pane's `.height(...)` drives the Tabs component's animated max-height
/// so switching kinds eases the dialog taller/shorter.
fn render_kind_pane(
    kind: TunnelKind,
    bind_addr_input: Entity<InputState>,
    bind_port_input: Entity<InputState>,
    target_host_input: Entity<InputState>,
    target_port_input: Entity<InputState>,
    bind_addr_focused: bool,
    bind_port_focused: bool,
    target_host_focused: bool,
    target_port_focused: bool,
    bind_addr_error: Option<SharedString>,
    bind_port_error: Option<SharedString>,
    target_host_error: Option<SharedString>,
    target_port_error: Option<SharedString>,
) -> TabPane {
    let label = match kind {
        TunnelKind::Local => t!("tunnel_form.kind_local").to_string(),
        TunnelKind::Remote => t!("tunnel_form.kind_remote").to_string(),
        TunnelKind::Dynamic => t!("tunnel_form.kind_dynamic").to_string(),
    };

    // Each error row adds ~24px below its input (gap_1 4px + error text
    // line_height 19px + 1px rounding). Add that to the pane height so the
    // layout doesn't clip the error message during the height anim.
    let err_extra = |e: &Option<SharedString>| if e.is_some() { 24.0_f32 } else { 0.0_f32 };
    let bind_row_h = 56.0 + err_extra(&bind_addr_error).max(err_extra(&bind_port_error));

    // Bind addr + bind port share a row (mirrors the host:port row in the
    // connection form). Both columns are `flex_none` so the error rows
    // (rendered inside `StyledInput`) can't push the column wider than its
    // allotted width and shrink the input shell. The addr column takes the
    // remaining space via `flex_1`; the port column is a fixed width.
    let bind_row = div()
        .flex()
        .flex_row()
        .items_start()
        .gap_3()
        .child(
            div().flex_1().min_w_0().child(
                StyledInput::new("tunnel-bind-addr", bind_addr_input)
                    .label(t!("tunnel_form.bind_addr").to_string())
                    .focused(bind_addr_focused)
                    .when_some(bind_addr_error, |el, e| el.error(e)),
            ),
        )
        .child(
            div().w(px(112.0)).flex_none().child(
                StyledInput::new("tunnel-bind-port", bind_port_input)
                    .label(t!("tunnel_form.bind_port").to_string())
                    .focused(bind_port_focused)
                    .when_some(bind_port_error, |el, e| el.error(e)),
            ),
        );

    let (content, height) = match kind {
        TunnelKind::Local | TunnelKind::Remote => {
            let target_host_h = 56.0 + err_extra(&target_host_error);
            let target_port_h = 56.0 + err_extra(&target_port_error);
            (
                div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(bind_row)
                    .child(
                        div().child(
                            StyledInput::new("tunnel-target-host", target_host_input)
                                .label(t!("tunnel_form.target_host").to_string())
                                .focused(target_host_focused)
                                .when_some(target_host_error.clone(), |el, e| el.error(e)),
                        ),
                    )
                    .child(
                        div().child(
                            StyledInput::new("tunnel-target-port", target_port_input)
                                .label(t!("tunnel_form.target_port").to_string())
                                .focused(target_port_focused)
                                .when_some(target_port_error.clone(), |el, e| el.error(e)),
                        ),
                    ),
                // bind row + gap + target_host + gap + target_port
                px(bind_row_h + 16.0 + target_host_h + 16.0 + target_port_h),
            )
        }
        TunnelKind::Dynamic => (
            div().flex().flex_col().gap_4().child(bind_row),
            // bind row only
            px(bind_row_h),
        ),
    };

    TabPane::new(label, content).height(height)
}

/// Render the host dropdown. If `hosts` is empty, show a hint instead.
fn render_host_selector(
    host_id: Option<i64>,
    dropdown_open: bool,
    hosts: Vec<ConnectionHost>,
    host_error: Option<SharedString>,
    app: Entity<CrabportApp>,
) -> impl IntoElement {
    let label_div = div()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(text_muted()))
        .child(t!("tunnel_form.host").to_string());

    // Error row shown below the dropdown when no host is selected. Mirrors
    // the StyledInput error presentation (small icon + text).
    let error_row = |msg: SharedString| {
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                svg()
                    .path("icons/circle-alert.svg")
                    .size_3()
                    .text_color(rgb(crate::color::input_border_error())),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(crate::color::input_border_error()))
                    .child(msg),
            )
    };

    if hosts.is_empty() {
        return label_div
            .child(
                div()
                    .flex()
                    .items_center()
                    .h_9()
                    .px_3()
                    .rounded_md()
                    .bg(rgb(bg_base()))
                    .border_1()
                    .border_color(rgb(border()))
                    .text_sm()
                    .text_color(rgb(text_muted()))
                    .child(t!("tunnel_form.no_hosts").to_string()),
            )
            .when_some(host_error, |el, e| el.child(error_row(e)))
            .into_any_element();
    }

    // Find the index of the currently-selected host (if any).
    let selected_idx = host_id.and_then(|id| hosts.iter().position(|h| h.id == id));

    // Build the dropdown items: "name (host:port)".
    let mut dropdown = Dropdown::new("tunnel-host-dropdown")
        .placeholder(t!("tunnel_form.host").to_string())
        .is_open(dropdown_open)
        .on_toggle({
            let app = app.clone();
            move |_w, cx| {
                app.update(cx, |app, cx| {
                    if let Some(ref mut form) = app.tunnel_form {
                        form.host_dropdown_open = !form.host_dropdown_open;
                        cx.notify();
                    }
                });
            }
        })
        .on_change({
            let app = app.clone();
            let hosts = hosts.clone();
            move |index, _w, cx| {
                if let Some(h) = hosts.get(index) {
                    app.update(cx, |app, cx| {
                        if let Some(ref mut form) = app.tunnel_form {
                            form.host_id = Some(h.id);
                            form.host_dropdown_open = false;
                            cx.notify();
                        }
                    });
                }
            }
        });

    for h in &hosts {
        let label = format!("{} ({}:{})", h.name, h.host, h.port);
        dropdown = dropdown.item_with_value(label, h.id.to_string());
    }
    if let Some(idx) = selected_idx {
        dropdown = dropdown.selected(idx);
    }

    label_div
        .child(dropdown)
        .when_some(host_error, |el, e| el.child(error_row(e)))
        .into_any_element()
}

/// Render the group selector dropdown for the tunnel form. Mirrors
/// [`render_host_selector`] but for `GroupEntry` of `GroupKind::Tunnel`.
/// The first item is always `tunnel_form.group_none` ("None" = ungrouped);
/// existing groups follow.
fn render_group_selector(
    group_id: Option<i64>,
    dropdown_open: bool,
    groups: Vec<GroupEntry>,
    group_search_input: Entity<InputState>,
    app: Entity<CrabportApp>,
) -> impl IntoElement {
    let label_div = div()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(text_muted()))
        .child(t!("tunnel_form.group").to_string());

    // Index 0 = "None" (ungrouped); groups start at index 1.
    let selected_idx =
        group_id.and_then(|id| groups.iter().position(|g| g.id == id).map(|i| i + 1));

    let mut dropdown = Dropdown::new("tunnel-group-dropdown")
        .placeholder(t!("tunnel_form.group_none").to_string())
        .is_open(dropdown_open)
        .searchable(group_search_input)
        .on_create({
            let app = app.clone();
            move |name, _w, cx| {
                app.update(cx, |app, cx| {
                    // Create the group, then immediately select it.
                    let kind = crabport_core::credential::GroupKind::Tunnel;
                    if let Ok(gid) = crate::app_state::AppState::store(cx)
                        .lock()
                        .add_group(&name, kind, None)
                    {
                        if let Some(ref mut form) = app.tunnel_form {
                            form.group_id = Some(gid);
                            form.group_dropdown_open = false;
                            cx.notify();
                        }
                    }
                });
            }
        })
        .on_toggle({
            let app = app.clone();
            move |_w, cx| {
                app.update(cx, |app, cx| {
                    if let Some(ref mut form) = app.tunnel_form {
                        form.group_dropdown_open = !form.group_dropdown_open;
                        cx.notify();
                    }
                });
            }
        })
        .on_change({
            let app = app.clone();
            let groups = groups.clone();
            move |index, _w, cx| {
                let new_group = if index == 0 {
                    None
                } else {
                    groups.get(index - 1).map(|g| g.id)
                };
                app.update(cx, |app, cx| {
                    if let Some(ref mut form) = app.tunnel_form {
                        form.group_id = new_group;
                        form.group_dropdown_open = false;
                        cx.notify();
                    }
                });
            }
        });

    // Item 0: "None" (ungrouped).
    dropdown =
        dropdown.item_with_value(t!("tunnel_form.group_none").to_string(), "none".to_string());
    for g in &groups {
        dropdown = dropdown.item_with_value(g.name.clone(), g.id.to_string());
    }
    if let Some(idx) = selected_idx {
        dropdown = dropdown.selected(idx);
    }

    label_div.child(dropdown).into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_buttons(
    editing: bool,
    _tunnel_kind: TunnelKind,
    _kind_pane_height: f32,
    app: Entity<CrabportApp>,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    on_save: Option<Rc<dyn Fn(TunnelFormOutput, &mut Window, &mut App) + 'static>>,
) -> impl IntoElement {
    let overlay_id = ElementId::Name("tunnel-form-overlay".into());
    let dialog_id = ElementId::Name("tunnel-form-dialog".into());
    // Save label is the same for create/edit (unlike the connection form which
    // has Connect vs Save).
    let _ = editing;

    div()
        .flex()
        .flex_row()
        .gap_3()
        .justify_end()
        .child(
            Button::new("tunnel-cancel")
                .centered(true)
                .child(t!("tunnel_form.cancel").to_string())
                .on_click(move |_e, w, cx| {
                    if let Some(ref cb) = on_close {
                        cb(w, cx);
                    }
                }),
        )
        .child(
            Button::new("tunnel-save")
                .primary()
                .centered(true)
                .child(t!("tunnel_form.save").to_string())
                .on_click(move |_e, w, cx| {
                    // Reset the overlay/dialog transitions so the next open
                    // starts fresh (mirrors connection_form's connect button).
                    gpui_animation::reset_transition(&overlay_id);
                    gpui_animation::reset_transition(&dialog_id);
                    // Validate required fields before building the output. If
                    // invalid, per-field errors are shown and a toast is
                    // surfaced; the save flow is aborted.
                    let output: Option<TunnelFormOutput> = app.update(cx, |app, cx| {
                        let valid = app
                            .tunnel_form
                            .as_mut()
                            .map(|form| form.validate(cx))
                            .unwrap_or(true);
                        if !valid {
                            app.app_ctx.notifications.update(cx, |c, cx| {
                                c.show(
                                    crate::components::notification::Notification::new(
                                        t!("tunnel_form.validation_title").to_string(),
                                    )
                                    .level(
                                        crate::components::notification::NotificationLevel::Warning,
                                    )
                                    .message(t!("tunnel_form.validation_message").to_string())
                                    .duration(std::time::Duration::from_secs(4)),
                                    cx,
                                );
                            });
                            cx.notify();
                            return None;
                        }
                        app.tunnel_form.as_ref().map(|form| form.output(cx))
                    });
                    if let Some(out) = output {
                        if let Some(ref cb) = on_save {
                            cb(out, w, cx);
                        }
                    }
                }),
        )
}
