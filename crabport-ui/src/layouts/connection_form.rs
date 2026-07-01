pub mod with_certificate;
pub mod with_proxy;

use gpui::{prelude::FluentBuilder, *};
use gpui_animation::{animation::TransitionExt, transition::general::Linear};
use gpui_component::input::InputState;
use rust_i18n::t;
use std::rc::Rc;
use std::time::Duration;

use crate::app::CrabportApp;
use crate::color::*;
use crate::components::button::Button;
use crate::components::input::{StyledInput, StyledPasswordInput};
use crate::components::tabs::{TabPane, Tabs};
use with_certificate::WithCertificateForm;
use with_proxy::{ProxyKind, WithProxyForm};

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
    // Proxy mode + custom proxy URL
    pub proxy_kind: ProxyKind,
    pub proxy_url_input: Entity<InputState>,
    /// When editing an existing host, this is the row id of the proxy currently
    /// linked to it (so we can UPDATE instead of INSERT). `None` for new hosts.
    pub proxy_id: Option<i64>,
    // Focus states
    pub name_focused: bool,
    pub host_focused: bool,
    pub port_focused: bool,
    pub user_focused: bool,
    pub pass_focused: bool,
    pub passphrase_focused: bool,
    pub private_key_focused: bool,
    pub proxy_url_focused: bool,
    pub editing: bool,
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
        let proxy_url_input = cx.new(|cx| InputState::new(window, cx));

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
            proxy_kind: ProxyKind::None,
            proxy_url_input,
            proxy_id: None,
            name_focused: false,
            host_focused: false,
            port_focused: false,
            user_focused: false,
            pass_focused: false,
            passphrase_focused: false,
            private_key_focused: false,
            proxy_url_focused: false,
            editing: false,
            on_close: None,
            on_connect: None,
        }
    }

    pub fn open(&mut self, window: &mut Window, cx: &mut App) {
        self.active = true;
        self.name_input.update(cx, |state, cx| {
            state.focus(window, cx);
        });
        self.port_input.update(cx, |state, cx| {
            state.set_value("22", window, cx);
        });
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

    pub fn proxy_url_text(&self, cx: &App) -> String {
        self.proxy_url_input.read(cx).text().to_string()
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
            with_proxy::ProxyKind::None => None,
            with_proxy::ProxyKind::System => crabport_core::credential::ProxyConfig::from_system(),
            with_proxy::ProxyKind::Custom => {
                let url = self.proxy_url_text(cx);
                crabport_core::credential::parse_proxy_url(&url)
            }
        };
        #[cfg(debug_assertions)]
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
        #[cfg(debug_assertions)]
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
                #[cfg(debug_assertions)]
                tracing::info!(
                    "connection_form: load_proxy — restoring Custom url={:?}",
                    url
                );
                self.proxy_url_input.update(cx, |state, cx| {
                    state.set_value(&url, window, cx);
                });
            }
            _ => {
                #[cfg(debug_assertions)]
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
    proxy_kind: ProxyKind,
    proxy_url_input: Entity<InputState>,
    name_focused: bool,
    host_focused: bool,
    port_focused: bool,
    user_focused: bool,
    pass_focused: bool,
    passphrase_focused: bool,
    private_key_focused: bool,
    proxy_url_focused: bool,
    editing: bool,
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
            proxy_kind: state.proxy_kind,
            proxy_url_input: state.proxy_url_input.clone(),
            name_focused: state.name_focused,
            host_focused: state.host_focused,
            port_focused: state.port_focused,
            user_focused: state.user_focused,
            pass_focused: state.pass_focused,
            passphrase_focused: state.passphrase_focused,
            private_key_focused: state.private_key_focused,
            proxy_url_focused: state.proxy_url_focused,
            editing: state.editing,
            app,
            on_close: state.on_close.clone(),
            on_connect: state.on_connect.clone(),
        }
    }
}

impl RenderOnce for ConnectionFormView {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let on_close_for_dialog = self.on_close.clone();

        render_overlay(
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
                self.proxy_kind,
                self.proxy_url_input,
                self.name_focused,
                self.host_focused,
                self.port_focused,
                self.user_focused,
                self.pass_focused,
                self.passphrase_focused,
                self.private_key_focused,
                self.proxy_url_focused,
                self.app,
                on_close_for_dialog,
                self.on_connect,
            ),
        )
    }
}

// ---------------------------------------------------------------------------
// Render helpers
// ---------------------------------------------------------------------------

fn render_overlay(
    active: bool,
    on_close: Option<Rc<dyn Fn(&mut Window, &mut App) + 'static>>,
    child: impl IntoElement,
) -> impl IntoElement {
    let overlay_id = ElementId::Name("conn-form-overlay".into());

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
        .when(active, |el| {
            el.occlude().on_click(move |_e, w, cx| {
                if let Some(ref cb) = on_close {
                    cb(w, cx);
                }
            })
        })
        .with_transition(overlay_id)
        .transition_when_else(
            active,
            Duration::from_millis(150),
            Linear,
            |el| el.bg(rgba(0x00000080)),
            |el| el.bg(rgba(0x00000000)),
        )
        .child(child)
}

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
    proxy_kind: ProxyKind,
    proxy_url_input: Entity<InputState>,
    name_focused: bool,
    host_focused: bool,
    port_focused: bool,
    user_focused: bool,
    pass_focused: bool,
    passphrase_focused: bool,
    private_key_focused: bool,
    proxy_url_focused: bool,
    app: Entity<CrabportApp>,
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
        // Title
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(TEXT_PRIMARY))
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
        // Connection-type tabs: SSH has the full form, Telnet/Serial are
        // placeholders until their backends land. The tab's `on_change`
        // writes `form.kind` so the connect button / save flow know which
        // kind to create.
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
                            // Host + Port row
                            .child(render_host_port_row(
                                host_input.clone(),
                                port_input.clone(),
                                host_focused,
                                port_focused,
                            ))
                            // Username (shared across auth types)
                            .child(
                                div().child(
                                    StyledInput::new("username", user_input.clone())
                                        .label(t!("connection_form.username").to_string())
                                        .focused(user_focused),
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
                                                .label(t!("connection_form.password").to_string())
                                                .focused(pass_focused)
                                                .on_toggle(|_, _| {}),
                                            ),
                                        )
                                        .height(px(60.0)),
                                    )
                                    .pane(
                                        TabPane::new(
                                            t!("connection_form.auth_certificate").to_string(),
                                            WithCertificateForm {
                                                passphrase_input,
                                                private_key_input,
                                                passphrase_focused,
                                                private_key_focused,
                                            },
                                        )
                                        .height(px(196.0)),
                                    )
                                    .on_change({
                                        let app = app.clone();
                                        move |index, _w, cx| {
                                            app.update(cx, |app, cx| {
                                                if let Some(ref mut form) = app.connection_form {
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
                                app: app.clone(),
                            }),
                    )
                    .height(px({
                        let auth_pane = if auth_kind == AuthKind::Password {
                            60.0
                        } else {
                            196.0
                        };
                        let auth_h = 54.0 + 16.0 + 54.0 + 16.0 + 30.0 + 8.0 + auth_pane;
                        let proxy_pane = if proxy_kind == ProxyKind::Custom {
                            60.0
                        } else {
                            0.0
                        };
                        let proxy_h = 16.0 + 20.0 + 54.0 + 8.0 + proxy_pane;
                        auth_h + proxy_h
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
                            ))
                            // Username
                            .child(
                                div().child(
                                    StyledInput::new("telnet-username", user_input.clone())
                                        .label(t!("connection_form.username").to_string())
                                        .focused(user_focused),
                                ),
                            )
                            // Proxy tabs
                            .child(WithProxyForm {
                                proxy_url_input: proxy_url_input.clone(),
                                proxy_url_focused,
                                proxy_kind,
                                app: app.clone(),
                            }),
                    )
                    .height(px({
                        let proxy_pane = if proxy_kind == ProxyKind::Custom {
                            60.0
                        } else {
                            0.0
                        };
                        54.0 + 16.0 + 54.0 + 16.0 + 20.0 + 54.0 + 8.0 + proxy_pane
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
                            .text_color(rgb(TEXT_MUTED))
                            .child(t!("connection_form.coming_soon").to_string()),
                    )
                    .height(px(80.0)),
                )
                .on_change({
                    let app = app.clone();
                    move |index, _w, cx| {
                        app.update(cx, |app, cx| {
                            if let Some(ref mut form) = app.connection_form {
                                form.kind = match index {
                                    0 => ConnectionKind::SSH,
                                    1 => ConnectionKind::Telnet,
                                    _ => ConnectionKind::Serial,
                                };
                                cx.notify();
                            }
                        });
                    }
                }),
        )
        // Buttons
        .child(render_buttons(editing, kind, on_close, on_connect))
}

fn render_host_port_row(
    host_input: Entity<InputState>,
    port_input: Entity<InputState>,
    host_focused: bool,
    port_focused: bool,
) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .gap_3()
        .child(
            div().flex_1().child(
                StyledInput::new("host", host_input)
                    .label(t!("connection_form.host").to_string())
                    .focused(host_focused),
            ),
        )
        .child(
            div().w(px(96.0)).child(
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
