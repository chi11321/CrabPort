use gpui::{prelude::FluentBuilder, *};
use gpui_component::input::InputState;
use gpui_component::scroll::ScrollableElement as _;
use rust_i18n::t;
use std::rc::Rc;

use super::with_certificate::WithCertificateForm;
use super::with_proxy::{ProxyKind, WithProxyForm};
use crate::app::CrabportApp;
use crate::color::*;
use crate::components::dropdown::Dropdown;
use crate::components::form::{
    form_dialog, form_footer, form_title, group_dropdown, label_column, password_field, text_field,
};
use crate::components::overlay::render_overlay;
use crate::components::tabs::{TabPane, Tabs};
use crabport_core::credential::PrivateKeyKind;
use crabport_serial::available_ports as available_serial_ports;

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
    // Jump host (bastion) — FK into the `hosts` table. `None` = direct
    // connection. Only meaningful for SSH connections; the selected host
    // must itself be an SSH host (the dropdown only lists those).
    pub jump_host_id: Option<i64>,
    /// Open state for the jump-host dropdown in the SSH tab.
    pub jump_host_dropdown_open: bool,
    /// When editing an existing host, its row id — used to exclude the host
    /// from its own jump-host dropdown (a host can't jump through itself).
    /// `None` for new connections.
    pub editing_host_id: Option<i64>,
    // Startup command — sent to the remote shell once the session is ready.
    // Multi-line textarea: each line becomes one command.
    pub startup_command_input: Entity<InputState>,
    // Serial config (only used when kind == Serial). The device path is
    // entered in the existing `host_input` (repurposed as "Device" when
    // Serial is selected).
    pub serial_baud_rate_input: Entity<InputState>,
    pub serial_data_bits: u8,
    pub serial_parity: String,
    pub serial_stop_bits: u8,
    pub serial_flow_control: String,
    /// Open state for the data-bits dropdown in the Serial tab.
    pub serial_data_bits_open: bool,
    /// Open state for the parity dropdown in the Serial tab.
    pub serial_parity_open: bool,
    /// Open state for the stop-bits dropdown in the Serial tab.
    pub serial_stop_bits_open: bool,
    /// Open state for the flow-control dropdown in the Serial tab.
    pub serial_flow_control_open: bool,
    /// Open state for the serial-port selector dropdown in the Serial tab.
    pub serial_port_open: bool,
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
        let serial_baud_rate_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value("115200", window, cx);
            state
        });

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
            jump_host_id: None,
            jump_host_dropdown_open: false,
            editing_host_id: None,
            startup_command_input,
            serial_baud_rate_input,
            serial_data_bits: 8,
            serial_parity: "none".to_string(),
            serial_stop_bits: 1,
            serial_flow_control: "none".to_string(),
            serial_data_bits_open: false,
            serial_parity_open: false,
            serial_stop_bits_open: false,
            serial_flow_control_open: false,
            serial_port_open: false,
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
                ConnectionKind::Serial => "",
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

    pub fn serial_baud_rate(&self, cx: &App) -> Option<u32> {
        let text = self.serial_baud_rate_input.read(cx).text().to_string();
        text.parse::<u32>().ok()
    }

    pub fn serial_data_bits(&self, _cx: &App) -> Option<u8> {
        Some(self.serial_data_bits)
    }

    pub fn serial_parity(&self, _cx: &App) -> Option<String> {
        Some(self.serial_parity.clone())
    }

    pub fn serial_stop_bits(&self, _cx: &App) -> Option<u8> {
        Some(self.serial_stop_bits)
    }

    pub fn serial_flow_control(&self, _cx: &App) -> Option<String> {
        Some(self.serial_flow_control.clone())
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
    /// - Serial: the device path (host field) is required.
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

        // Serial requires a device path (entered in the host field). Baud
        // rate parsing is best-effort: an unparseable value falls back to
        // the default at connect time, so it isn't a hard validation error.
        if self.kind == ConnectionKind::Serial {
            if self.host_text(cx).trim().is_empty() {
                errors.host = Some(t!("connection_form.error_device_required").into());
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
    jump_host_id: Option<i64>,
    jump_host_dropdown_open: bool,
    editing_host_id: Option<i64>,
    startup_command_input: Entity<InputState>,
    serial_baud_rate_input: Entity<InputState>,
    serial_data_bits: u8,
    serial_parity: String,
    serial_stop_bits: u8,
    serial_flow_control: String,
    serial_data_bits_open: bool,
    serial_parity_open: bool,
    serial_stop_bits_open: bool,
    serial_flow_control_open: bool,
    serial_port_open: bool,
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
            jump_host_id: state.jump_host_id,
            jump_host_dropdown_open: state.jump_host_dropdown_open,
            editing_host_id: state.editing_host_id,
            startup_command_input: state.startup_command_input.clone(),
            serial_baud_rate_input: state.serial_baud_rate_input.clone(),
            serial_data_bits: state.serial_data_bits,
            serial_parity: state.serial_parity.clone(),
            serial_stop_bits: state.serial_stop_bits,
            serial_flow_control: state.serial_flow_control.clone(),
            serial_data_bits_open: state.serial_data_bits_open,
            serial_parity_open: state.serial_parity_open,
            serial_stop_bits_open: state.serial_stop_bits_open,
            serial_flow_control_open: state.serial_flow_control_open,
            serial_port_open: state.serial_port_open,
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
        render_overlay(
            ElementId::Name("conn-form-overlay".into()),
            self.active,
            self.on_close.clone(),
            render_dialog(self, cx),
        )
    }
}

// ---------------------------------------------------------------------------
// Render helpers
// ---------------------------------------------------------------------------

fn render_dialog(view: ConnectionFormView, cx: &App) -> impl IntoElement {
    let ConnectionFormView {
        active,
        kind,
        auth_kind,
        name_input,
        host_input,
        port_input,
        user_input,
        pass_input,
        passphrase_input,
        private_key_input,
        private_key_path_input,
        proxy_kind,
        proxy_url_input,
        jump_host_id,
        jump_host_dropdown_open,
        editing_host_id,
        startup_command_input,
        serial_baud_rate_input,
        serial_data_bits,
        serial_parity,
        serial_stop_bits,
        serial_flow_control,
        serial_data_bits_open,
        serial_parity_open,
        serial_stop_bits_open,
        serial_flow_control_open,
        serial_port_open,
        editing,
        group_id,
        group_dropdown_open,
        group_search_input,
        errors,
        app,
        on_close,
        on_connect,
    } = view;

    let auth_active_index = match auth_kind {
        AuthKind::Password => 0,
        AuthKind::Certificate => 1,
    };

    let active_type_index = match kind {
        ConnectionKind::SSH => 0,
        ConnectionKind::Telnet => 1,
        ConnectionKind::Serial => 2,
    };

    form_dialog("conn-form-dialog", active, 420.0, |d| {
        d.max_h(px(600.0)).overflow_hidden()
    })
    // Fixed header: Title + Name + Group selector.
    .child(
        div()
            .p_6()
            .pb_0()
            .flex()
            .flex_col()
            .gap_4()
            // Title
            .child(form_title(t!("connection_form.title").to_string()))
            // Name
            .child(div().child(text_field(
                "name",
                name_input,
                t!("connection_form.name").to_string(),
                None,
            )))
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
    // `overflow_y_scrollbar` activates the scrollbar.
    //
    // IMPORTANT: the horizontal/bottom padding (px_6 / pb_4) lives on an
    // INNER wrapper, NOT on this scroll element. The `Scrollable` wrapper
    // (gpui-component) strips style off its direct element and reapplies
    // it to the outer viewport div — if padding were set here, the
    // scrollbar layer (which is absolutely positioned to fill the outer
    // viewport) would span the padding region too, making its draggable
    // track taller than the actual scroll area. That mismatch causes the
    // thumb to overshoot: dragging to the bottom leaves a gap while wheel
    // scrolling reaches the real bottom. Keeping padding on the inner
    // child keeps the scrollbar track == scroll-area exactly.
    .child(
        div()
            .id(ElementId::Name("conn-form-scroll".into()))
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(
                div().px_6().pb_4().child(
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
                                        errors.host.clone(),
                                    ))
                                    // Username (shared across auth types)
                                    .child(div().child(text_field(
                                        "username",
                                        user_input.clone(),
                                        t!("connection_form.username").to_string(),
                                        errors.user.clone(),
                                    )))
                                    .child(
                                        Tabs::new("conn-auth-tabs")
                                            .active(auth_active_index)
                                            .pane(
                                                TabPane::new(
                                                    t!("connection_form.auth_password").to_string(),
                                                    div().flex().flex_col().gap_4().child(
                                                        password_field(
                                                            "password",
                                                            pass_input.clone(),
                                                            t!("connection_form.password")
                                                                .to_string(),
                                                            errors.pass.clone(),
                                                        ),
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
                                        proxy_kind,
                                        proxy_url_error: errors.proxy_url.clone(),
                                        app: app.clone(),
                                    })
                                    // Jump host (bastion) selector — lists
                                    // saved SSH hosts to route this
                                    // connection through (ProxyJump-style).
                                    .child(render_jump_host_selector(
                                        jump_host_id,
                                        jump_host_dropdown_open,
                                        editing_host_id,
                                        app.clone(),
                                        cx,
                                    ))
                                    // Startup command — sent to the remote shell
                                    // once the SSH session is ready.
                                    .child(
                                        text_field(
                                            "ssh-startup-command",
                                            startup_command_input.clone(),
                                            t!("connection_form.startup_command").to_string(),
                                            None,
                                        )
                                        .multi_line(true)
                                        .rows(3),
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
                                // Jump-host selector: gap + label + gap +
                                // dropdown (mirrors the group selector).
                                let jump_h = 16.0 + 21.0 + 4.0 + 35.0;
                                let startup_h = 16.0 + 85.0;
                                auth_h + proxy_h + jump_h + startup_h
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
                                        errors.host.clone(),
                                    ))
                                    // Username
                                    .child(div().child(text_field(
                                        "telnet-username",
                                        user_input.clone(),
                                        t!("connection_form.username").to_string(),
                                        errors.user.clone(),
                                    )))
                                    // Password
                                    .child(div().child(password_field(
                                        "telnet-password",
                                        pass_input.clone(),
                                        t!("connection_form.password").to_string(),
                                        errors.pass.clone(),
                                    )))
                                    // Proxy tabs
                                    .child(WithProxyForm {
                                        proxy_url_input: proxy_url_input.clone(),
                                        proxy_kind,
                                        proxy_url_error: errors.proxy_url.clone(),
                                        app: app.clone(),
                                    })
                                    // Startup command — sent after the telnet
                                    // connection is established.
                                    .child(
                                        text_field(
                                            "telnet-startup-command",
                                            startup_command_input.clone(),
                                            t!("connection_form.startup_command").to_string(),
                                            None,
                                        )
                                        .multi_line(true)
                                        .rows(3),
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
                                    .flex_col()
                                    .gap_4()
                                    // Serial port selector — enumerates
                                    // available serial ports on the
                                    // system via `serialport`. The
                                    // selected device path is stored in
                                    // the `host_input` field (which the
                                    // backend reads as the device path).
                                    .child(render_serial_port_selector(
                                        serial_port_open,
                                        host_input.clone(),
                                        errors.host.clone(),
                                        app.clone(),
                                        cx,
                                    ))
                                    // Baud rate (text input)
                                    .child(div().child(text_field(
                                        "serial-baud",
                                        serial_baud_rate_input.clone(),
                                        t!("connection_form.baud_rate").to_string(),
                                        None,
                                    )))
                                    // Data bits dropdown (8 / 7 / 6 / 5)
                                    .child(render_serial_dropdown(
                                        "serial-data-bits".to_string(),
                                        t!("connection_form.data_bits").to_string(),
                                        serial_data_bits_open,
                                        vec![
                                            ("8".to_string(), "8".to_string()),
                                            ("7".to_string(), "7".to_string()),
                                            ("6".to_string(), "6".to_string()),
                                            ("5".to_string(), "5".to_string()),
                                        ],
                                        match serial_data_bits {
                                            8 => 0,
                                            7 => 1,
                                            6 => 2,
                                            _ => 3,
                                        },
                                        app.clone(),
                                        SerialField::DataBits,
                                    ))
                                    // Parity dropdown (none / odd / even)
                                    .child(render_serial_dropdown(
                                        "serial-parity".to_string(),
                                        t!("connection_form.parity").to_string(),
                                        serial_parity_open,
                                        vec![
                                            (
                                                t!("connection_form.parity_none").to_string(),
                                                "none".to_string(),
                                            ),
                                            (
                                                t!("connection_form.parity_odd").to_string(),
                                                "odd".to_string(),
                                            ),
                                            (
                                                t!("connection_form.parity_even").to_string(),
                                                "even".to_string(),
                                            ),
                                        ],
                                        match serial_parity.as_str() {
                                            "odd" => 1,
                                            "even" => 2,
                                            _ => 0,
                                        },
                                        app.clone(),
                                        SerialField::Parity,
                                    ))
                                    // Stop bits dropdown (1 / 2)
                                    .child(render_serial_dropdown(
                                        "serial-stop-bits".to_string(),
                                        t!("connection_form.stop_bits").to_string(),
                                        serial_stop_bits_open,
                                        vec![
                                            ("1".to_string(), "1".to_string()),
                                            ("2".to_string(), "2".to_string()),
                                        ],
                                        if serial_stop_bits == 2 { 1 } else { 0 },
                                        app.clone(),
                                        SerialField::StopBits,
                                    ))
                                    // Flow control dropdown (none / software / hardware)
                                    .child(render_serial_dropdown(
                                        "serial-flow-control".to_string(),
                                        t!("connection_form.flow_control").to_string(),
                                        serial_flow_control_open,
                                        vec![
                                            (
                                                t!("connection_form.flow_none").to_string(),
                                                "none".to_string(),
                                            ),
                                            (
                                                t!("connection_form.flow_software").to_string(),
                                                "software".to_string(),
                                            ),
                                            (
                                                t!("connection_form.flow_hardware").to_string(),
                                                "hardware".to_string(),
                                            ),
                                        ],
                                        match serial_flow_control.as_str() {
                                            "software" => 1,
                                            "hardware" => 2,
                                            _ => 0,
                                        },
                                        app.clone(),
                                        SerialField::FlowControl,
                                    ))
                                    // Startup command — sent to the serial
                                    // device once the connection is ready.
                                    .child(
                                        text_field(
                                            "serial-startup-command",
                                            startup_command_input.clone(),
                                            t!("connection_form.startup_command").to_string(),
                                            None,
                                        )
                                        .multi_line(true)
                                        .rows(3),
                                    ),
                            )
                            .height(px({
                                let field_h = 57.0;
                                // device + baud + 4 dropdowns + startup
                                // + 16px bottom padding so the startup
                                // command isn't flush against the pane edge.
                                field_h
                                    + 16.0
                                    + field_h
                                    + 16.0
                                    + field_h
                                    + 16.0
                                    + field_h
                                    + 16.0
                                    + field_h
                                    + 16.0
                                    + field_h
                                    + 16.0
                                    + 85.0
                                    + 16.0
                            })),
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
                                            ConnectionKind::Serial => "",
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
                ), // close inner padding div's .child(Tabs)
            ), // close scrollable div's .child (inner padding div)
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

    label_column(t!("connection_form.group").to_string()).child(group_dropdown(
        "conn-form-group-dropdown",
        t!("connection_form.group_none").to_string(),
        t!("connection_form.group_none").to_string(),
        "none",
        GroupKind::Host,
        group_id,
        dropdown_open,
        group_search_input,
        groups,
        app,
        |app| {
            app.connection_form
                .as_mut()
                .map(|f| (&mut f.group_id, &mut f.group_dropdown_open))
        },
    ))
}

/// Jump-host (bastion) selector for the SSH pane. Lists every saved SSH
/// host except those that would create a cycle with the host being edited:
/// the host itself, and any host whose own jump chain already passes
/// through it (if A jumps via B, then B must not offer A — nor may C offer
/// A when A→B→C). The first item is always "None" (direct connection).
/// Only the FK is stored on the host row — credentials/proxy of the jump
/// host are resolved from its own entry at connect time.
fn render_jump_host_selector(
    jump_host_id: Option<i64>,
    dropdown_open: bool,
    editing_host_id: Option<i64>,
    app: Entity<CrabportApp>,
    cx: &App,
) -> impl IntoElement {
    use crate::app_state::AppState;
    use crabport_core::credential::HostKind as CoreHostKind;

    let all_hosts = AppState::store(cx).lock().hosts().unwrap_or_default();

    // id → jump_host_id links for chain walking.
    let jump_links: std::collections::HashMap<i64, Option<i64>> =
        all_hosts.iter().map(|h| (h.id, h.jump_host_id)).collect();

    // Returns true if following `start`'s jump chain (including `start`
    // itself) reaches `target`. A visited set guards against walking a
    // pre-existing cycle in the data forever.
    let chain_reaches = |start: i64, target: i64| -> bool {
        let mut visited = std::collections::HashSet::new();
        let mut cur = Some(start);
        while let Some(id) = cur {
            if id == target {
                return true;
            }
            if !visited.insert(id) {
                return false;
            }
            cur = jump_links.get(&id).copied().flatten();
        }
        false
    };

    // Candidate jump hosts: saved SSH hosts whose selection would NOT
    // create a cycle. New connections (`editing_host_id == None`) can't be
    // referenced by anything yet, so everything qualifies.
    let hosts: Vec<(i64, String)> = all_hosts
        .into_iter()
        .filter(|h| h.kind == CoreHostKind::Ssh)
        .filter(|h| editing_host_id.is_none_or(|eid| !chain_reaches(h.id, eid)))
        .map(|h| {
            let label = if h.name.trim().is_empty() {
                format!("{}@{}:{}", h.username, h.host, h.port)
            } else {
                h.name.clone()
            };
            (h.id, label)
        })
        .collect();

    let label_div = label_column(t!("connection_form.jump_host").to_string());

    // Index 0 = "None" (direct); hosts start at index 1.
    let selected_idx =
        jump_host_id.and_then(|id| hosts.iter().position(|(hid, _)| *hid == id).map(|i| i + 1));

    let mut dropdown = Dropdown::new("conn-form-jump-host-dropdown")
        .placeholder(t!("connection_form.jump_host_none").to_string())
        .is_open(dropdown_open)
        .on_toggle({
            let app = app.clone();
            move |_w, cx| {
                app.update(cx, |app, cx| {
                    if let Some(ref mut form) = app.connection_form {
                        form.jump_host_dropdown_open = !form.jump_host_dropdown_open;
                        cx.notify();
                    }
                });
            }
        })
        .on_change({
            let app = app.clone();
            let hosts = hosts.clone();
            move |index, _w, cx| {
                let new_jump = if index == 0 {
                    None
                } else {
                    hosts.get(index - 1).map(|(id, _)| *id)
                };
                app.update(cx, |app, cx| {
                    if let Some(ref mut form) = app.connection_form {
                        form.jump_host_id = new_jump;
                        form.jump_host_dropdown_open = false;
                        cx.notify();
                    }
                });
            }
        });

    dropdown = dropdown.item_with_value(t!("connection_form.jump_host_none").to_string(), "none");
    for (id, label) in &hosts {
        dropdown = dropdown.item_with_value(label.clone(), id.to_string());
    }
    if let Some(idx) = selected_idx {
        dropdown = dropdown.selected(idx);
    }

    label_div.child(dropdown)
}

fn render_host_port_row(
    host_input: Entity<InputState>,
    port_input: Entity<InputState>,
    host_error: Option<SharedString>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_start()
        .gap_3()
        .child(div().flex_1().min_w_0().child(text_field(
            "host",
            host_input,
            t!("connection_form.host").to_string(),
            host_error,
        )))
        .child(div().w(px(96.0)).flex_none().child(text_field(
            "port",
            port_input,
            t!("connection_form.port").to_string(),
            None,
        )))
}

/// Which serial-config field a [`render_serial_dropdown`] dropdown controls.
/// Used by the `on_change` / `on_toggle` callbacks to update the right field
/// on `ConnectionFormState`.
#[derive(Clone, Copy)]
enum SerialField {
    DataBits,
    Parity,
    StopBits,
    FlowControl,
}

/// Render a serial-port selector dropdown. Enumerates available serial
/// ports on the system at render time via `serialport::available_ports()`.
/// The selected device path is written into the `host_input` field (which
/// the backend reads as the serial device path). If no ports are found,
/// the dropdown shows a placeholder and remains disabled.
fn render_serial_port_selector(
    is_open: bool,
    host_input: Entity<InputState>,
    host_error: Option<SharedString>,
    app: Entity<CrabportApp>,
    cx: &App,
) -> impl IntoElement {
    let ports = available_serial_ports();
    let current_device = host_input.read(cx).text().to_string();

    let label_div = label_column(t!("connection_form.serial_port").to_string()).when_some(
        host_error,
        |el, e| {
            el.child(
                div()
                    .text_color(rgb(input_border_error()))
                    .text_xs()
                    .child(e),
            )
        },
    );

    let selected_idx = ports.iter().position(|(name, _)| *name == current_device);

    let no_ports = ports.is_empty();

    let mut dropdown = Dropdown::new("serial-port-selector")
        .placeholder(if no_ports {
            t!("connection_form.serial_no_ports").to_string()
        } else {
            t!("connection_form.serial_port_none").to_string()
        })
        .is_open(is_open)
        .disabled(no_ports);

    for (name, desc) in &ports {
        dropdown = dropdown.item_with_value(desc.clone(), name.clone());
    }
    if let Some(idx) = selected_idx {
        dropdown = dropdown.selected(idx);
    }

    dropdown = dropdown.on_toggle({
        let app = app.clone();
        move |_w, cx| {
            app.update(cx, |app, cx| {
                if let Some(ref mut form) = app.connection_form {
                    form.serial_port_open = !form.serial_port_open;
                    cx.notify();
                }
            });
        }
    });

    dropdown = dropdown.on_change({
        let app = app.clone();
        let ports = ports.clone();
        move |idx, w, cx| {
            let device = ports
                .get(idx)
                .map(|(name, _)| name.clone())
                .unwrap_or_default();
            if !device.is_empty() {
                app.update(cx, |app, cx| {
                    if let Some(ref mut form) = app.connection_form {
                        form.host_input.update(cx, |state, cx| {
                            state.set_value(&device, w, cx);
                        });
                        form.serial_port_open = false;
                        cx.notify();
                    }
                });
            }
        }
    });

    label_div.child(dropdown)
}
/// parity, stop bits, or flow control). The dropdown is fully controlled —
/// open state lives on `ConnectionFormState` and is toggled via `on_toggle`;
/// the selected value is written back via `on_change`.
#[allow(clippy::too_many_arguments)]
fn render_serial_dropdown(
    id: String,
    label: String,
    is_open: bool,
    items: Vec<(String, String)>,
    selected: usize,
    app: Entity<CrabportApp>,
    field: SerialField,
) -> impl IntoElement {
    let label_div = label_column(label);

    let mut dropdown = Dropdown::new(gpui::ElementId::Name(id.into()))
        .is_open(is_open)
        .selected(selected);
    for (lbl, _val) in &items {
        dropdown = dropdown.item(lbl.clone());
    }
    dropdown = dropdown.on_toggle({
        let app = app.clone();
        move |_w, cx| {
            app.update(cx, |app, cx| {
                if let Some(ref mut form) = app.connection_form {
                    match field {
                        SerialField::DataBits => {
                            form.serial_data_bits_open = !form.serial_data_bits_open
                        }
                        SerialField::Parity => form.serial_parity_open = !form.serial_parity_open,
                        SerialField::StopBits => {
                            form.serial_stop_bits_open = !form.serial_stop_bits_open
                        }
                        SerialField::FlowControl => {
                            form.serial_flow_control_open = !form.serial_flow_control_open
                        }
                    }
                    cx.notify();
                }
            });
        }
    });
    dropdown = dropdown.on_change({
        let app = app.clone();
        let items = items.clone();
        move |idx, _w, cx| {
            let value = items.get(idx).map(|(_, v)| v.clone()).unwrap_or_default();
            app.update(cx, |app, cx| {
                if let Some(ref mut form) = app.connection_form {
                    match field {
                        SerialField::DataBits => {
                            if let Ok(b) = value.parse::<u8>() {
                                form.serial_data_bits = b;
                            }
                            form.serial_data_bits_open = false;
                        }
                        SerialField::Parity => {
                            form.serial_parity = value;
                            form.serial_parity_open = false;
                        }
                        SerialField::StopBits => {
                            if let Ok(b) = value.parse::<u8>() {
                                form.serial_stop_bits = b;
                            }
                            form.serial_stop_bits_open = false;
                        }
                        SerialField::FlowControl => {
                            form.serial_flow_control = value;
                            form.serial_flow_control_open = false;
                        }
                    }
                    cx.notify();
                }
            });
        }
    });
    label_div.child(dropdown)
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
    form_footer(
        "conn-cancel",
        t!("connection_form.cancel").to_string(),
        on_close,
        "conn-connect",
        confirm_label,
        move |_e, w, cx| {
            if !editing {
                gpui_animation::reset_transition(&overlay_id);
                gpui_animation::reset_transition(&dialog_id);
            }
            if let Some(ref cb) = on_connect {
                cb(kind, w, cx);
            }
        },
    )
}
