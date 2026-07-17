use gpui::{prelude::FluentBuilder, *};
use gpui_animation::{animation::TransitionExt, transition::general::Linear};
use gpui_component::input::InputState;
use gpui_component::scroll::ScrollableElement as _;
use rust_i18n::t;
use std::rc::Rc;
use std::time::Duration;

use super::with_certificate::WithCertificateForm;
use super::with_proxy::{ProxyKind, WithProxyForm};
use crate::app::CrabportApp;
use crate::color::*;
use crate::components::button::Button;
use crate::components::dropdown::Dropdown;
use crate::components::input::{StyledInput, StyledPasswordInput};
use crate::components::overlay::render_overlay;
use crate::components::tabs::{TabPane, Tabs};
use crabport_core::credential::PrivateKeyKind;

// ---------------------------------------------------------------------------
// Connection type
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConnectionKind {
    SSH,
    Telnet,
    Serial,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AuthKind {
    Password,
    Certificate,
}

// ---------------------------------------------------------------------------
// ValidationErrors — per-field error strings shown via StyledInput.error()
// ---------------------------------------------------------------------------

/// Per-field validation errors for the connection form. A field is `Some`
/// when it has an error to display; `None` means it passed validation.
/// Cloning is cheap (just `SharedString`s).
#[derive(Clone, Default)]
pub struct ValidationErrors {
    pub host: Option<SharedString>,
    pub user: Option<SharedString>,
    pub pass: Option<SharedString>,
    pub private_key: Option<SharedString>,
    pub proxy_url: Option<SharedString>,
}

impl ValidationErrors {
    pub fn is_empty(&self) -> bool {
        self.host.is_none()
            && self.user.is_none()
            && self.pass.is_none()
            && self.private_key.is_none()
            && self.proxy_url.is_none()
    }
}

// ---------------------------------------------------------------------------
// ConnectionFormState — owned by CrabportApp
// ---------------------------------------------------------------------------

/// Holds all mutable state for the connection form overlay so that
/// `ConnectionFormView` can be a pure `RenderOnce` renderer.
#[derive(Clone)]
pub struct ConnectionFormState {
    pub active: bool,
    pub kind: ConnectionKind,
    pub auth_kind: AuthKind,
    // Basic fields
    pub name_input: Entity<InputState>,
    pub host_input: Entity<InputState>,
    pub port_input: Entity<InputState>,
    pub user_input: Entity<InputState>,
    pub pass_input: Entity<InputState>,
    // Certificate-mode: passphrase + private key
    pub passphrase_input: Entity<InputState>,
    pub private_key_input: Entity<InputState>,
    /// Read-only file path picked by the "Browse…" button. Either this or
    /// `private_key_input` (pasted key content) must be filled to pass
    /// certificate validation. The path is stored verbatim and resolved by
    /// `crabport_ssh::keys::decode_private_key` at connect time.
    pub private_key_path_input: Entity<InputState>,
    // Proxy mode + custom proxy URL
    pub proxy_kind: ProxyKind,
    pub proxy_url_input: Entity<InputState>,
    /// When editing an existing host, this is the row id of the proxy currently
    /// linked to it (so we can UPDATE instead of INSERT). `None` for new hosts.
    pub proxy_id: Option<i64>,
    // Startup command — sent to the remote shell once the session is ready.
    // Multi-line textarea: each line becomes one command.
    pub startup_command_input: Entity<InputState>,
    // Focus states
    pub name_focused: bool,
    pub host_focused: bool,
    pub port_focused: bool,
    pub user_focused: bool,
    pub pass_focused: bool,
    pub passphrase_focused: bool,
    pub private_key_focused: bool,
    pub private_key_path_focused: bool,
    pub proxy_url_focused: bool,
    pub startup_command_focused: bool,
    pub editing: bool,
    /// FK into the `groups` table. `None` = ungrouped. Edited via a group
    /// dropdown in the form.
    pub group_id: Option<i64>,
    /// Open state for the group dropdown.
    pub group_dropdown_open: bool,
    /// Search input for the group dropdown (filtering + create).
    pub group_search_input: Entity<InputState>,
    /// Per-field validation errors. Populated by `validate()` and rendered
    /// via `StyledInput.error(...)` on the relevant fields. Cleared on open.
    pub errors: ValidationErrors,
    pub on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    pub on_connect: Option<Rc<dyn Fn(ConnectionKind, &mut Window, &mut App) + 'static>>,
}

impl ConnectionFormState {
    pub fn new(window: &mut Window, cx: &mut App) -> Self {
        let name_input = cx.new(|cx| InputState::new(window, cx));
        let host_input = cx.new(|cx| InputState::new(window, cx));
        let port_input = cx.new(|cx| InputState::new(window, cx));
        let user_input = cx.new(|cx| InputState::new(window, cx));
        let pass_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_masked(true, window, cx);
            state
        });
        let passphrase_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_masked(true, window, cx);
            state
        });
        let private_key_input = cx.new(|cx| InputState::new(window, cx).multi_line(true).rows(5));
        // Read-only path field — never focused for typing, only filled via
        // the "Browse…" button. Kept as an `InputState` so the existing
        // `StyledInput` chrome (label / error / disabled styling) applies.
        let private_key_path_input = cx.new(|cx| InputState::new(window, cx));
        let proxy_url_input = cx.new(|cx| InputState::new(window, cx));
        let group_search_input = cx.new(|cx| InputState::new(window, cx));
        let startup_command_input =
            cx.new(|cx| InputState::new(window, cx).multi_line(true).rows(3));

        Self {
            active: false,
            kind: ConnectionKind::SSH,
            auth_kind: AuthKind::Password,
            name_input,
            host_input,
            port_input,
            user_input,
            pass_input,
            passphrase_input,
            private_key_input,
            private_key_path_input,
            proxy_kind: ProxyKind::None,
            proxy_url_input,
            proxy_id: None,
            startup_command_input,
            name_focused: false,
            host_focused: false,
            port_focused: false,
            user_focused: false,
            pass_focused: false,
            passphrase_focused: false,
            private_key_focused: false,
            private_key_path_focused: false,
            proxy_url_focused: false,
            startup_command_focused: false,
            editing: false,
            group_id: None,
            group_dropdown_open: false,
            group_search_input,
            errors: ValidationErrors::default(),
            on_close: None,
            on_connect: None,
        }
    }

    pub fn open(&mut self, window: &mut Window, cx: &mut App) {
        self.active = true;
        self.errors = ValidationErrors::default();
        self.group_dropdown_open = false;
        self.group_search_input
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.name_input.update(cx, |state, cx| {
            state.focus(window, cx);
        });
        // Only set default port for new connections, not when editing an
        // existing host (where the port was already loaded from the store).
        if !self.editing {
            let default_port = match self.kind {
                ConnectionKind::SSH => "22",
                ConnectionKind::Telnet => "23",
                ConnectionKind::Serial => "22",
            };
            self.port_input.update(cx, |state, cx| {
                state.set_value(default_port, window, cx);
            });
        }
    }

    pub fn close(&mut self) {
        self.active = false;
    }

    pub fn name_text(&self, cx: &App) -> String {
        self.name_input.read(cx).text().to_string()
    }

    pub fn host_text(&self, cx: &App) -> String {
        self.host_input.read(cx).text().to_string()
    }

    pub fn port_text(&self, cx: &App) -> String {
        self.port_input.read(cx).text().to_string()
    }

    pub fn user_text(&self, cx: &App) -> String {
        self.user_input.read(cx).text().to_string()
    }

    pub fn pass_text(&self, cx: &App) -> String {
        self.pass_input.read(cx).text().to_string()
    }

    pub fn passphrase_text(&self, cx: &App) -> String {
        self.passphrase_input.read(cx).text().to_string()
    }

    pub fn private_key_text(&self, cx: &App) -> String {
        self.private_key_input.read(cx).text().to_string()
    }

    /// The private-key value to persist into `CredentialEntry.private_key`,
    /// paired with the [`PrivateKeyKind`] that tells the store / SSH layer
    /// how to interpret it.
    ///
    /// Preference order: pasted content (`private_key_input`) first, then the
    /// file path picked via "Browse…". Either satisfies certificate auth —
    /// `crabport_ssh::keys::decode_private_key` resolves both PEM content and
    /// a filesystem path — but we record which one was used so the edit-host
    /// flow can restore the value into the correct field. Returns an empty
    /// string + `Content` when neither is set.
    pub fn private_key_value(&self, cx: &App) -> (String, PrivateKeyKind) {
        let pasted = self.private_key_text(cx);
        if !pasted.trim().is_empty() {
            return (pasted, PrivateKeyKind::Content);
        }
        let path = self.private_key_path_text(cx);
        if !path.trim().is_empty() {
            return (path, PrivateKeyKind::Path);
        }
        (String::new(), PrivateKeyKind::Content)
    }

    pub fn private_key_path_text(&self, cx: &App) -> String {
        self.private_key_path_input.read(cx).text().to_string()
    }

    pub fn proxy_url_text(&self, cx: &App) -> String {
        self.proxy_url_input.read(cx).text().to_string()
    }

    pub fn startup_command_text(&self, cx: &App) -> String {
        self.startup_command_input.read(cx).text().to_string()
    }

    /// Validate the form against the required-field rules. Populates
    /// `self.errors` and returns `true` if the form is valid (no errors).
    ///
    /// Rules:
    /// - SSH / Telnet: host and username are required.
    /// - SSH + Password auth: password is required.
    /// - Telnet: password is required (credentials are sent via the terminal
    ///   prompt in v1, but we still require one so saved hosts reconnect).
    /// - SSH + Certificate auth: a private key is required — either pasted
    ///   key content OR a key file path picked via "Browse…" (passphrase
    ///   is optional).
    /// - Proxy = Custom: proxy URL is required.
    /// - Name is optional in all modes.
    /// - Serial has no required fields yet (placeholder backend).
    pub fn validate(&mut self, cx: &App) -> bool {
        let mut errors = ValidationErrors::default();

        let needs_host_user = matches!(self.kind, ConnectionKind::SSH | ConnectionKind::Telnet);
        if needs_host_user {
            if self.host_text(cx).trim().is_empty() {
                errors.host = Some(t!("connection_form.error_host_required").into());
            }
            if self.user_text(cx).trim().is_empty() {
                errors.user = Some(t!("connection_form.error_user_required").into());
            }
        }

        if self.kind == ConnectionKind::SSH {
            match self.auth_kind {
                AuthKind::Password => {
                    if self.pass_text(cx).trim().is_empty() {
                        errors.pass = Some(t!("connection_form.error_password_required").into());
                    }
                }
                AuthKind::Certificate => {
                    // Either pasted key content or a picked file path satisfies
                    // the requirement; `decode_private_key` resolves both.
                    let (pk_value, _pk_kind) = self.private_key_value(cx);
                    if pk_value.trim().is_empty() {
                        errors.private_key =
                            Some(t!("connection_form.error_private_key_required").into());
                    }
                    // passphrase is optional — no check.
                }
            }
        }

        if self.kind == ConnectionKind::Telnet && self.pass_text(cx).trim().is_empty() {
            errors.pass = Some(t!("connection_form.error_password_required").into());
        }

        if self.proxy_kind == ProxyKind::Custom && self.proxy_url_text(cx).trim().is_empty() {
            errors.proxy_url = Some(t!("connection_form.error_proxy_url_required").into());
        }

        let ok = errors.is_empty();
        self.errors = errors;
        ok
    }

    /// Build a `ProxyConfig` from the current form state.
    ///
    /// - `None`    → no proxy (direct connection).
    /// - `System`  → resolved from `ALL_PROXY` / `HTTPS_PROXY` / `HTTP_PROXY`
    ///   env vars (returns `None` if none are set / parseable).
    /// - `Custom`  → parsed from the proxy URL field. Accepted formats:
    ///   `socks5://host:port`, `socks5://user:pass@host:port`,
    ///   `http://host:port`, `https://user:pass@host:port`.
    pub fn proxy_config(&self, cx: &App) -> Option<crabport_core::credential::ProxyConfig> {
        let cfg = match self.proxy_kind {
            ProxyKind::None => None,
            ProxyKind::System => crabport_core::credential::ProxyConfig::from_system(),
            ProxyKind::Custom => {
                let url = self.proxy_url_text(cx);
                crabport_core::credential::parse_proxy_url(&url)
            }
        };
        tracing::info!(
            "connection_form: proxy_config — kind={:?}, editing_proxy_id={:?}, resolved={:?}",
            self.proxy_kind,
            self.proxy_id,
            cfg.as_ref().map(|c| (c.kind, c.host.clone(), c.port))
        );
        cfg
    }

    /// Populate the proxy fields from a previously-saved `ProxyConfig`
    /// (loaded when editing a host). Selects the `Custom` tab and fills the
    /// URL input via `ProxyConfig::to_url`.
    pub fn load_proxy(
        &mut self,
        proxy_id: Option<i64>,
        config: Option<&crabport_core::credential::ProxyConfig>,
        window: &mut Window,
        cx: &mut App,
    ) {
        tracing::info!(
            "connection_form: load_proxy — proxy_id={:?}, has_config={}",
            proxy_id,
            config.is_some()
        );
        self.proxy_id = proxy_id;
        match config {
            Some(cfg) if cfg.is_enabled() => {
                self.proxy_kind = ProxyKind::Custom;
                let url = cfg.to_url();
                tracing::info!(
                    "connection_form: load_proxy — restoring Custom url={:?}",
                    url
                );
                self.proxy_url_input.update(cx, |state, cx| {
                    state.set_value(&url, window, cx);
                });
            }
            _ => {
                tracing::info!("connection_form: load_proxy — no proxy, selecting None");
                self.proxy_kind = ProxyKind::None;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ConnectionFormView — pure RenderOnce renderer
// ---------------------------------------------------------------------------

#[derive(IntoElement)]
pub struct ConnectionFormView {
    active: bool,
    kind: ConnectionKind,
    auth_kind: AuthKind,
    name_input: Entity<InputState>,
    host_input: Entity<InputState>,
    port_input: Entity<InputState>,
    user_input: Entity<InputState>,
    pass_input: Entity<InputState>,
    passphrase_input: Entity<InputState>,
    private_key_input: Entity<InputState>,
    private_key_path_input: Entity<InputState>,
    proxy_kind: ProxyKind,
    proxy_url_input: Entity<InputState>,
    startup_command_input: Entity<InputState>,
    name_focused: bool,
    host_focused: bool,
    port_focused: bool,
    user_focused: bool,
    pass_focused: bool,
    passphrase_focused: bool,
    private_key_focused: bool,
    private_key_path_focused: bool,
    proxy_url_focused: bool,
    startup_command_focused: bool,
    editing: bool,
    group_id: Option<i64>,
    group_dropdown_open: bool,
    group_search_input: Entity<InputState>,
    errors: ValidationErrors,
    app: Entity<CrabportApp>,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    on_connect: Option<Rc<dyn Fn(ConnectionKind, &mut Window, &mut App) + 'static>>,
}

impl ConnectionFormView {
    pub fn new(state: &ConnectionFormState, app: Entity<CrabportApp>) -> Self {
        Self {
            active: state.active,
            kind: state.kind,
            auth_kind: state.auth_kind,
            name_input: state.name_input.clone(),
            host_input: state.host_input.clone(),
            port_input: state.port_input.clone(),
            user_input: state.user_input.clone(),
            pass_input: state.pass_input.clone(),
            passphrase_input: state.passphrase_input.clone(),
            private_key_input: state.private_key_input.clone(),
            private_key_path_input: state.private_key_path_input.clone(),
            proxy_kind: state.proxy_kind,
            proxy_url_input: state.proxy_url_input.clone(),
            startup_command_input: state.startup_command_input.clone(),
            name_focused: state.name_focused,
            host_focused: state.host_focused,
            port_focused: state.port_focused,
            user_focused: state.user_focused,
            pass_focused: state.pass_focused,
            passphrase_focused: state.passphrase_focused,
            private_key_focused: state.private_key_focused,
            private_key_path_focused: state.private_key_path_focused,
            proxy_url_focused: state.proxy_url_focused,
            startup_command_focused: state.startup_command_focused,
            editing: state.editing,
            group_id: state.group_id,
            group_dropdown_open: state.group_dropdown_open,
            group_search_input: state.group_search_input.clone(),
            errors: state.errors.clone(),
            app,
            on_close: state.on_close.clone(),
            on_connect: state.on_connect.clone(),
        }
    }
}

impl RenderOnce for ConnectionFormView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let on_close_for_dialog = self.on_close.clone();

        render_overlay(
            ElementId::Name("conn-form-overlay".into()),
            self.active,
            self.on_close,
            render_dialog(
                self.active,
                self.editing,
                self.kind,
                self.auth_kind,
                self.name_input,
                self.host_input,
                self.port_input,
                self.user_input,
                self.pass_input,
                self.passphrase_input,
                self.private_key_input,
                self.private_key_path_input,
                self.proxy_kind,
                self.proxy_url_input,
                self.startup_command_input,
                self.name_focused,
                self.host_focused,
                self.port_focused,
                self.user_focused,
                self.pass_focused,
                self.passphrase_focused,
                self.private_key_focused,
                self.private_key_path_focused,
                self.proxy_url_focused,
                self.startup_command_focused,
                self.group_id,
                self.group_dropdown_open,
                self.group_search_input,
                self.errors,
                self.app,
                cx,
                on_close_for_dialog,
                self.on_connect,
            ),
        )
    }
}

// ---------------------------------------------------------------------------
// Render helpers
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn render_dialog(
    active: bool,
    editing: bool,
    kind: ConnectionKind,
    auth_kind: AuthKind,
    name_input: Entity<InputState>,
    host_input: Entity<InputState>,
    port_input: Entity<InputState>,
    user_input: Entity<InputState>,
    pass_input: Entity<InputState>,
    passphrase_input: Entity<InputState>,
    private_key_input: Entity<InputState>,
    private_key_path_input: Entity<InputState>,
    proxy_kind: ProxyKind,
    proxy_url_input: Entity<InputState>,
    startup_command_input: Entity<InputState>,
    name_focused: bool,
    host_focused: bool,
    port_focused: bool,
    user_focused: bool,
    pass_focused: bool,
    passphrase_focused: bool,
    private_key_focused: bool,
    private_key_path_focused: bool,
    proxy_url_focused: bool,
    startup_command_focused: bool,
    group_id: Option<i64>,
    group_dropdown_open: bool,
    group_search_input: Entity<InputState>,
    errors: ValidationErrors,
    app: Entity<CrabportApp>,
    cx: &App,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    on_connect: Option<Rc<dyn Fn(ConnectionKind, &mut Window, &mut App) + 'static>>,
) -> impl IntoElement {
    let dialog_id = ElementId::Name("conn-form-dialog".into());

    let auth_active_index = match auth_kind {
        AuthKind::Password => 0,
        AuthKind::Certificate => 1,
    };

    let active_type_index = match kind {
        ConnectionKind::SSH => 0,
        ConnectionKind::Telnet => 1,
        ConnectionKind::Serial => 2,
    };

    div()
        .id(dialog_id.clone())
        .w(px(420.0))
        .max_h(px(600.0))
        .bg(rgb(bg_base()))
        .border_1()
        .border_color(rgb(border()))
        .rounded_lg()
        .shadow_lg()
        .flex()
        .flex_col()
        .overflow_hidden()
        .gap_4()
        .opacity(0.0)
        .mt(px(-16.0))
        .when(active, |el| {
            el.on_click(|_, _, cx| {
                cx.stop_propagation();
            })
        })
        .with_transition(dialog_id)
        .transition_when_else(
            active,
            Duration::from_millis(150),
            Linear,
            |el| el.opacity(1.0).mt_0(),
            |el| el.opacity(0.0).mt(px(-16.0)),
        )
        // Fixed header: Title + Name + Group selector.
        .child(
            div()
                .p_6()
                .pb_0()
                .flex()
                .flex_col()
                .gap_4()
                // Title
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(text_primary()))
                        .child(t!("connection_form.title").to_string()),
                )
                // Name
                .child(
                    div().child(
                        StyledInput::new("name", name_input)
                            .label(t!("connection_form.name").to_string())
                            .focused(name_focused),
                    ),
                )
                // Group selector (searchable + creatable dropdown)
                .child(render_group_selector(
                    group_id,
                    group_dropdown_open,
                    group_search_input,
                    app.clone(),
                    cx,
                )),
        )
        // Scrollable area: connection-type tabs. The `flex_1` + `min_h_0`
        // lets this region shrink when the dialog hits `max_h`, and
        // `overflow_y_scrollbar` activates the scrollbar. The `.id()` is
        // required by the Scrollable wrapper.
        .child(
            div()
                .id(ElementId::Name("conn-form-scroll".into()))
                .px_6()
                .pb_4()
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .child(
                    Tabs::new("conn-type-tabs")
                        .active(active_type_index)
                        .pane(
                            TabPane::new(
                                t!("new_connection.ssh").to_string(),
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_4()
                                    // Username (shared across auth types)
                                    // Host + Port row
                                    .child(render_host_port_row(
                                        host_input.clone(),
                                        port_input.clone(),
                                        host_focused,
                                        port_focused,
                                        errors.host.clone(),
                                    ))
                                    // Username (shared across auth types)
                                    .child(
                                        div().child(
                                            StyledInput::new("username", user_input.clone())
                                                .label(t!("connection_form.username").to_string())
                                                .focused(user_focused)
                                                .when_some(errors.user.clone(), |el, e| {
                                                    el.error(e)
                                                }),
                                        ),
                                    )
                                    .child(
                                        Tabs::new("conn-auth-tabs")
                                            .active(auth_active_index)
                                            .pane(
                                                TabPane::new(
                                                    t!("connection_form.auth_password").to_string(),
                                                    div().flex().flex_col().gap_4().child(
                                                        StyledPasswordInput::new(
                                                            "password",
                                                            pass_input.clone(),
                                                        )
                                                        .label(
                                                            t!("connection_form.password")
                                                                .to_string(),
                                                        )
                                                        .focused(pass_focused)
                                                        .when_some(errors.pass.clone(), |el, e| {
                                                            el.error(e)
                                                        }),
                                                    ),
                                                )
                                                .height(px({
                                                    if errors.pass.is_some() { 80.0 } else { 57.0 }
                                                })),
                                            )
                                            .pane(
                                                TabPane::new(
                                                    t!("connection_form.auth_certificate")
                                                        .to_string(),
                                                    WithCertificateForm {
                                                        passphrase_input,
                                                        private_key_input,
                                                        private_key_path_input,
                                                        passphrase_focused,
                                                        private_key_focused,
                                                        private_key_path_focused,
                                                        private_key_error: errors
                                                            .private_key
                                                            .clone(),
                                                        app: app.clone(),
                                                    },
                                                )
                                                .height(px({
                                                    let has_err = errors.private_key.is_some();
                                                    let pass_h = if has_err { 80.0 } else { 57.0 };
                                                    let path_h = if has_err { 80.0 } else { 57.0 };
                                                    let pk_h = if has_err { 148.0 } else { 125.0 };
                                                    pass_h + 16.0 + path_h + 16.0 + pk_h
                                                })),
                                            )
                                            .on_change({
                                                let app = app.clone();
                                                move |index, _w, cx| {
                                                    app.update(cx, |app, cx| {
                                                        if let Some(ref mut form) =
                                                            app.connection_form
                                                        {
                                                            form.auth_kind = match index {
                                                                0 => AuthKind::Password,
                                                                _ => AuthKind::Certificate,
                                                            };
                                                            cx.notify();
                                                        }
                                                    });
                                                }
                                            }),
                                    )
                                    // Proxy tabs (None / System / Custom). Only
                                    // Custom has content (a proxy URL input).
                                    .child(WithProxyForm {
                                        proxy_url_input: proxy_url_input.clone(),
                                        proxy_url_focused,
                                        proxy_kind,
                                        proxy_url_error: errors.proxy_url.clone(),
                                        app: app.clone(),
                                    })
                                    // Startup command — sent to the remote shell
                                    // once the SSH session is ready.
                                    .child(
                                        StyledInput::new(
                                            "ssh-startup-command",
                                            startup_command_input.clone(),
                                        )
                                        .label(t!("connection_form.startup_command").to_string())
                                        .multi_line(true)
                                        .rows(3)
                                        .focused(startup_command_focused),
                                    ),
                            )
                            .height(px({
                                let field_h = |err: bool| if err { 80.0 } else { 57.0 };
                                let auth_pane = match auth_kind {
                                    AuthKind::Password => field_h(errors.pass.is_some()),
                                    AuthKind::Certificate => {
                                        let has_err = errors.private_key.is_some();
                                        let pass_h = field_h(has_err);
                                        let path_h = field_h(has_err);
                                        let pk_h = if has_err { 148.0 } else { 125.0 };
                                        pass_h + 16.0 + path_h + 16.0 + pk_h
                                    }
                                };
                                let auth_h = field_h(errors.host.is_some())
                                    + 16.0
                                    + field_h(errors.user.is_some())
                                    + 16.0
                                    + 35.0
                                    + 8.0
                                    + auth_pane;
                                let proxy_pane = if proxy_kind == ProxyKind::Custom {
                                    field_h(errors.proxy_url.is_some())
                                } else {
                                    0.0
                                };
                                let proxy_h = 16.0 + 21.0 + 4.0 + 35.0 + 8.0 + proxy_pane;
                                let startup_h = 16.0 + 85.0;
                                auth_h + proxy_h + startup_h
                            })),
                        )
                        .pane(
                            TabPane::new(
                                t!("new_connection.telnet").to_string(),
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_4()
                                    // Host + Port row
                                    .child(render_host_port_row(
                                        host_input.clone(),
                                        port_input.clone(),
                                        host_focused,
                                        port_focused,
                                        errors.host.clone(),
                                    ))
                                    // Username
                                    .child(
                                        div().child(
                                            StyledInput::new("telnet-username", user_input.clone())
                                                .label(t!("connection_form.username").to_string())
                                                .focused(user_focused)
                                                .when_some(errors.user.clone(), |el, e| {
                                                    el.error(e)
                                                }),
                                        ),
                                    )
                                    // Password
                                    .child(
                                        div().child(
                                            StyledPasswordInput::new(
                                                "telnet-password",
                                                pass_input.clone(),
                                            )
                                            .label(t!("connection_form.password").to_string())
                                            .focused(pass_focused)
                                            .when_some(errors.pass.clone(), |el, e| el.error(e)),
                                        ),
                                    )
                                    // Proxy tabs
                                    .child(WithProxyForm {
                                        proxy_url_input: proxy_url_input.clone(),
                                        proxy_url_focused,
                                        proxy_kind,
                                        proxy_url_error: errors.proxy_url.clone(),
                                        app: app.clone(),
                                    })
                                    // Startup command — sent after the telnet
                                    // connection is established.
                                    .child(
                                        StyledInput::new(
                                            "telnet-startup-command",
                                            startup_command_input.clone(),
                                        )
                                        .label(t!("connection_form.startup_command").to_string())
                                        .multi_line(true)
                                        .rows(3)
                                        .focused(startup_command_focused),
                                    ),
                            )
                            .height(px({
                                let field_h = |err: bool| if err { 80.0 } else { 57.0 };
                                let proxy_pane = if proxy_kind == ProxyKind::Custom {
                                    field_h(errors.proxy_url.is_some())
                                } else {
                                    0.0
                                };
                                field_h(errors.host.is_some())
                                    + 16.0
                                    + field_h(errors.user.is_some())
                                    + 16.0
                                    + field_h(errors.pass.is_some())
                                    + 16.0
                                    + 21.0
                                    + 4.0
                                    + 35.0
                                    + 8.0
                                    + proxy_pane
                                    + 16.0
                                    + 85.0
                            })),
                        )
                        .pane(
                            TabPane::new(
                                t!("new_connection.serial").to_string(),
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_sm()
                                    .text_color(rgb(text_muted()))
                                    .child(t!("connection_form.coming_soon").to_string()),
                            )
                            .height(px(80.0)),
                        )
                        .on_change({
                            let app = app.clone();
                            move |index, w, cx| {
                                app.update(cx, |app, cx| {
                                    if let Some(ref mut form) = app.connection_form {
                                        form.kind = match index {
                                            0 => ConnectionKind::SSH,
                                            1 => ConnectionKind::Telnet,
                                            _ => ConnectionKind::Serial,
                                        };
                                        let cur = form.port_text(cx);
                                        let new_port = match form.kind {
                                            ConnectionKind::SSH => "22",
                                            ConnectionKind::Telnet => "23",
                                            ConnectionKind::Serial => "22",
                                        };
                                        if cur == "22" || cur == "23" || cur.is_empty() {
                                            form.port_input.update(cx, |state, cx| {
                                                state.set_value(new_port, w, cx);
                                            });
                                        }
                                        cx.notify();
                                    }
                                });
                            }
                        }),
                ), // close scrollable div's .child
        ) // close scrollable div
        // Buttons (fixed at bottom — do not scroll)
        .child(
            div()
                .p_6()
                .pt_2()
                .child(render_buttons(editing, kind, on_close, on_connect)),
        )
}

/// Group selector dropdown for the connection form. Searchable + creatable
/// — mirrors the tunnel/snippet group selectors. The first item is always
/// "None" (ungrouped); existing groups follow.
fn render_group_selector(
    group_id: Option<i64>,
    dropdown_open: bool,
    group_search_input: Entity<InputState>,
    app: Entity<CrabportApp>,
    cx: &App,
) -> impl IntoElement {
    use crate::app_state::AppState;
    use crabport_core::credential::{GroupEntry, GroupKind};

    let groups: Vec<GroupEntry> = AppState::store(cx)
        .lock()
        .groups(GroupKind::Host)
        .unwrap_or_default();

    let label_div = div()
        .flex()
        .flex_col()
        .gap_1()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(text_muted()))
        .child(t!("connection_form.group").to_string());

    // Index 0 = "None" (ungrouped); groups start at index 1.
    let selected_idx =
        group_id.and_then(|id| groups.iter().position(|g| g.id == id).map(|i| i + 1));

    let mut dropdown = Dropdown::new("conn-form-group-dropdown")
        .placeholder(t!("connection_form.group_none").to_string())
        .is_open(dropdown_open)
        .searchable(group_search_input)
        .on_create({
            let app = app.clone();
            move |name, _w, cx| {
                app.update(cx, |app, cx| {
                    if let Ok(gid) =
                        AppState::store(cx)
                            .lock()
                            .add_group(&name, GroupKind::Host, None)
                    {
                        if let Some(ref mut form) = app.connection_form {
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
                    if let Some(ref mut form) = app.connection_form {
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
                    if let Some(ref mut form) = app.connection_form {
                        form.group_id = new_group;
                        form.group_dropdown_open = false;
                        cx.notify();
                    }
                });
            }
        });

    dropdown = dropdown.item_with_value(t!("connection_form.group_none").to_string(), "none");
    for g in &groups {
        dropdown = dropdown.item_with_value(g.name.clone(), g.id.to_string());
    }
    if let Some(idx) = selected_idx {
        dropdown = dropdown.selected(idx);
    }

    label_div.child(dropdown)
}

fn render_host_port_row(
    host_input: Entity<InputState>,
    port_input: Entity<InputState>,
    host_focused: bool,
    port_focused: bool,
    host_error: Option<SharedString>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_start()
        .gap_3()
        .child(
            div().flex_1().min_w_0().child(
                StyledInput::new("host", host_input)
                    .label(t!("connection_form.host").to_string())
                    .focused(host_focused)
                    .when_some(host_error, |el, e| el.error(e)),
            ),
        )
        .child(
            div().w(px(96.0)).flex_none().child(
                StyledInput::new("port", port_input)
                    .label(t!("connection_form.port").to_string())
                    .focused(port_focused),
            ),
        )
}

fn render_buttons(
    editing: bool,
    kind: ConnectionKind,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    on_connect: Option<Rc<dyn Fn(ConnectionKind, &mut Window, &mut App) + 'static>>,
) -> impl IntoElement {
    let overlay_id = ElementId::Name("conn-form-overlay".into());
    let dialog_id = ElementId::Name("conn-form-dialog".into());
    let confirm_label = if editing {
        t!("connection_form.save").to_string()
    } else {
        t!("connection_form.connect").to_string()
    };
    div()
        .flex()
        .flex_row()
        .gap_3()
        .justify_end()
        .child(
            Button::new("conn-cancel")
                .centered(true)
                .child(t!("connection_form.cancel").to_string())
                .on_click(move |_e, w, cx| {
                    if let Some(ref cb) = on_close {
                        cb(w, cx);
                    }
                }),
        )
        .child(
            Button::new("conn-connect")
                .primary()
                .centered(true)
                .child(confirm_label)
                .on_click(move |_e, w, cx| {
                    if !editing {
                        gpui_animation::reset_transition(&overlay_id);
                        gpui_animation::reset_transition(&dialog_id);
                    }
                    if let Some(ref cb) = on_connect {
                        cb(kind, w, cx);
                    }
                }),
        )
}
