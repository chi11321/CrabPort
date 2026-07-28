//! Connection form management.
//!
//! Contains the methods that open, close, and validate the ephemeral
//! connection form entity, plus the `upsert_proxy_for_host` helper used by
//! both the connection and host flows to persist proxy rows.

use std::rc::Rc;

use gpui::*;
use rust_i18n::t;

use crate::app_state::AppState;
use crate::components::notification::{Notification, NotificationLevel};
use crate::views::sessions::{AuthKind, ConnectionFormState, ConnectionHost, ConnectionKind};
use crabport_core::credential::{
    CredentialEntry, CredentialKind as CoreCredentialKind, HostEntry, HostKind as CoreHostKind,
    PrivateKeyKind, ProxyConfig, ProxyEntry,
};

use super::CrabportApp;

impl CrabportApp {
    // -----------------------------------------------------------------------
    // Connection form (ephemeral entity — created on open, destroyed after close animation)
    // -----------------------------------------------------------------------

    /// Create a new ConnectionFormView entity, wire its callbacks, and open it.
    pub fn open_connection_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_connection_form_with_kind(None, window, cx);
    }

    /// Like [`open_connection_form`] but pre-selects a connection type tab.
    /// `kind = None` defaults to SSH (the form's built-in default).
    pub fn open_connection_form_with_kind(
        &mut self,
        kind: Option<crate::views::sessions::ConnectionKind>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // If one is already open, just bring it to front
        if let Some(ref mut form) = self.connection_form {
            form.open(window, cx);
            cx.notify();
            return;
        }

        let mut form = ConnectionFormState::new(window, cx);
        let app = cx.entity().clone();

        form.on_close = Some(Rc::new({
            let a = app.clone();
            move |_: &mut Window, cx: &mut App| {
                a.update(cx, |app, cx| {
                    app.close_connection_form(cx);
                });
            }
        }));

        // Save (persist without connecting). The New / Clone flow offers
        // this as a secondary button so users can stockpile host configs
        // without immediately opening a terminal.
        form.on_save = Some(Rc::new({
            let a = app.clone();
            move |kind: ConnectionKind, _w: &mut Window, cx: &mut App| {
                a.update(cx, |app, cx| {
                    app.save_new_host_from_form(kind, false, cx);
                });
            }
        }));

        form.on_connect = Some(Rc::new({
            let a = app.clone();
            move |kind: ConnectionKind, _w: &mut Window, cx: &mut App| {
                a.update(cx, |app, cx| {
                    app.save_new_host_from_form(kind, true, cx);
                });
            }
        }));

        // Pre-select the connection type tab if the caller requested one.
        // Must happen before `form.open()` so the default port is set for
        // the right kind.
        if let Some(k) = kind {
            form.kind = k;
        }

        form.open(window, cx);
        self.connection_form = Some(form);
        cx.notify();
    }

    /// Close the connection form. The state stays alive for the exit animation,
    /// then is destroyed by a timer.
    pub fn close_connection_form(&mut self, cx: &mut Context<Self>) {
        if let Some(ref mut form) = self.connection_form {
            form.close();
        } else {
            return;
        }
        // After animation finishes, destroy the state and clean up animations
        let app = cx.entity().clone();
        cx.spawn(async move |_this, cx| {
            smol::Timer::after(std::time::Duration::from_millis(200)).await;
            let _ = app.update(cx, |app, cx| {
                if app.connection_form.is_some() {
                    // Clean up Tabs animation state (conn-auth-tabs has 2 panes)
                    let tabs_id = ElementId::Name("conn-auth-tabs".into());
                    crate::components::tabs::Tabs::cleanup_animation(&tabs_id, 2);
                    app.connection_form = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Validate the connection form before submitting. Populates per-field
    /// error state (rendered via `StyledInput.error(...)`) and, when invalid,
    /// shows a toast notification summarizing the missing fields. Returns
    /// `true` when the form is valid and the caller may proceed with the
    /// save / connect flow.
    pub fn validate_connection_form(&mut self, cx: &mut Context<Self>) -> bool {
        let valid = self
            .connection_form
            .as_mut()
            .map(|form| form.validate(cx))
            .unwrap_or(true);
        if !valid {
            // Surface a summary toast so the user knows something is wrong
            // even if the offending field is scrolled out of view.
            self.app_ctx.notifications.update(cx, |c, cx| {
                c.show(
                    Notification::new(t!("connection_form.validation_title").to_string())
                        .level(NotificationLevel::Warning)
                        .message(t!("connection_form.validation_message").to_string())
                        .duration(std::time::Duration::from_secs(4)),
                    cx,
                );
            });
            cx.notify();
        }
        valid
    }

    /// Persist a brand-new host from the current connection form, optionally
    /// connecting to it. Shared by the form's Save and Connect buttons (and
    /// by the Clone flow, which reuses the same form in create mode).
    ///
    /// `connect = true` dispatches to the matching backend (SSH / Telnet /
    /// Serial) and opens a terminal tab; `connect = false` just persists
    /// the host + credential + proxy rows and shows a success toast.
    pub fn save_new_host_from_form(
        &mut self,
        kind: ConnectionKind,
        connect: bool,
        cx: &mut Context<Self>,
    ) {
        // Validate required fields before doing anything. If the form is
        // invalid, per-field errors are shown and a toast is surfaced; the
        // save/connect flow is aborted.
        if !self.validate_connection_form(cx) {
            return;
        }
        // Read form values directly from state
        let (
            name,
            host,
            port_num,
            username,
            password,
            passphrase,
            auth_kind,
            private_key,
            private_key_kind,
            proxy_config,
            startup_command,
            jump_host_id,
        ) = {
            let f = self.connection_form.as_ref().unwrap();
            let n = f.name_text(cx);
            let h = f.host_text(cx);
            let p: u16 = f.port_text(cx).parse().unwrap_or(22);
            let u = f.user_text(cx);
            let pw = f.pass_text(cx);
            let pp = f.passphrase_text(cx);
            let ak = f.auth_kind;
            let (pk, pk_kind) = f.private_key_value(cx);
            let pc = f.proxy_config(cx);
            let sc = f.startup_command_text(cx);
            // Jump hosts only apply to SSH connections.
            let jh = if kind == ConnectionKind::SSH {
                f.jump_host_id
            } else {
                None
            };
            (n, h, p, u, pw, pp, ak, pk, pk_kind, pc, sc, jh)
        };
        self.close_connection_form(cx);

        // Persist credential for this host
        let (cred_kind, secret, pk, pk_kind) = match auth_kind {
            AuthKind::Password => (
                CoreCredentialKind::Password,
                password.clone(),
                String::new(),
                PrivateKeyKind::Content,
            ),
            AuthKind::Certificate => (
                CoreCredentialKind::Certificate,
                passphrase.clone(),
                private_key.clone(),
                private_key_kind,
            ),
        };
        let cred = CredentialEntry {
            id: 0,
            name: name.clone(),
            kind: cred_kind,
            anonymous: true,
            secret,
            private_key: pk,
            private_key_kind: pk_kind,
            public_key: String::new(),
            certificate: String::new(),
        };
        let cred_id = AppState::store(cx)
            .lock()
            .add_credential(&cred)
            .unwrap_or(0);

        // Persist host with linked credential
        let proxy_id = upsert_proxy_for_host(&proxy_config, None, cx);
        let entry = HostEntry {
            id: 0,
            name: name.clone(),
            host: host.clone(),
            port: port_num,
            username: username.clone(),
            credential_id: Some(cred_id),
            kind: kind.into(),
            last_login: None,
            favorite: false,
            proxy_id,
            group_id: self.connection_form.as_ref().and_then(|f| f.group_id),
            startup_command: startup_command.clone(),
            serial_baud_rate: self
                .connection_form
                .as_ref()
                .and_then(|f| f.serial_baud_rate(cx)),
            serial_data_bits: self
                .connection_form
                .as_ref()
                .and_then(|f| f.serial_data_bits(cx)),
            serial_parity: self
                .connection_form
                .as_ref()
                .and_then(|f| f.serial_parity(cx)),
            serial_stop_bits: self
                .connection_form
                .as_ref()
                .and_then(|f| f.serial_stop_bits(cx)),
            serial_flow_control: self
                .connection_form
                .as_ref()
                .and_then(|f| f.serial_flow_control(cx)),
            jump_host_id,
        };
        let row_id = AppState::store(cx).lock().add_host(&entry).unwrap_or(0);

        self.hosts.push(ConnectionHost {
            id: row_id,
            name: name.clone(),
            host: host.to_string(),
            port: port_num,
            username: username.to_string(),
            kind,
            credential_id: Some(cred_id),
            last_login: None,
            favorite: false,
            proxy_id,
            group_id: self.connection_form.as_ref().and_then(|f| f.group_id),
        });

        // Save-only path: surface a toast and stop here.
        if !connect {
            self.app_ctx.notifications.update(cx, |c, cx| {
                c.show(
                    Notification::new(t!("hosts.notif_saved_title").to_string())
                        .level(NotificationLevel::Success)
                        .message(t!("hosts.notif_saved_msg", name = name.as_str()).to_string())
                        .duration(std::time::Duration::from_secs(3)),
                    cx,
                );
            });
            cx.notify();
            return;
        }

        let (private_key_arg, passphrase_arg) = match auth_kind {
            AuthKind::Password => (None, None),
            AuthKind::Certificate => (
                if private_key.is_empty() {
                    None
                } else {
                    Some(private_key.as_str())
                },
                if passphrase.is_empty() {
                    None
                } else {
                    Some(passphrase.as_str())
                },
            ),
        };
        // Dispatch to the matching backend by connection kind.
        // Telnet uses password-only auth; SSH keeps its full
        // password / private-key / passphrase flow.
        match kind {
            ConnectionKind::Telnet => {
                self.add_telnet_tab(
                    &name,
                    Some(row_id),
                    &host,
                    port_num,
                    &username,
                    &password,
                    proxy_config,
                    Some(&startup_command),
                    cx,
                );
            }
            ConnectionKind::Serial => {
                let f = self.connection_form.as_ref().unwrap();
                // Device path is entered in the host field.
                let device = f.host_text(cx);
                let baud = f.serial_baud_rate(cx).unwrap_or(115200);
                let data_bits = f.serial_data_bits(cx).unwrap_or(8);
                let parity = f.serial_parity(cx).unwrap_or_else(|| "none".to_string());
                let stop_bits = f.serial_stop_bits(cx).unwrap_or(1);
                let flow_control = f
                    .serial_flow_control(cx)
                    .unwrap_or_else(|| "none".to_string());
                self.add_serial_tab(
                    &name,
                    Some(row_id),
                    &device,
                    baud,
                    data_bits,
                    &parity,
                    stop_bits,
                    &flow_control,
                    Some(&startup_command),
                    cx,
                );
            }
            _ => {
                // Resolve the jump-host (bastion) chain, if one
                // was selected in the form.
                let jump_hosts = resolve_jump_chain(cx, jump_host_id);
                self.add_ssh_tab(
                    &name,
                    Some(row_id),
                    &host,
                    port_num,
                    &username,
                    match auth_kind {
                        AuthKind::Password => &password,
                        AuthKind::Certificate => "",
                    },
                    private_key_arg,
                    passphrase_arg,
                    proxy_config,
                    Some(&startup_command),
                    jump_hosts,
                    cx,
                );
            }
        }
        cx.notify();
    }
}

/// Resolve a saved host's jump-host chain into connect-ready hop infos.
///
/// Follows `jump_host_id` links starting at `first_jump_id`, resolving each
/// hop's credential + proxy, and returns the chain in CONNECTION order:
/// index 0 is the outermost bastion (the hop we TCP-connect to first), the
/// last entry is the hop that opens the tunnel to the actual target.
///
/// Guards: non-SSH hosts terminate the chain (jump hosts must be SSH), and
/// a visited-set stops cycles (A→B→A) — the chain simply ends where the
/// cycle would close.
pub fn resolve_jump_chain(
    cx: &App,
    first_jump_id: Option<i64>,
) -> Vec<crabport_ssh::session::JumpHostInfo> {
    use crabport_ssh::session::JumpHostInfo;

    let store = AppState::store(cx);
    let mut chain: Vec<JumpHostInfo> = Vec::new();
    let mut visited: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut next = first_jump_id;

    while let Some(id) = next {
        if !visited.insert(id) {
            tracing::warn!("resolve_jump_chain: cycle detected at host {id} — truncating chain");
            break;
        }
        let Some(host) = store.lock().find_host(id).ok().flatten() else {
            tracing::warn!("resolve_jump_chain: jump host {id} not found — truncating chain");
            break;
        };
        if host.kind != CoreHostKind::Ssh {
            tracing::warn!(
                "resolve_jump_chain: jump host {id} is not an SSH host — truncating chain"
            );
            break;
        }

        let cred = host
            .credential_id
            .and_then(|cid| store.lock().find_credential(cid).ok().flatten());
        let (password, private_key, passphrase) = match cred {
            Some(c) if c.kind == CoreCredentialKind::Certificate => (
                String::new(),
                (!c.private_key.is_empty()).then(|| c.private_key.clone()),
                (!c.secret.is_empty()).then(|| c.secret.clone()),
            ),
            Some(c) => (c.secret.clone(), None, None),
            None => (String::new(), None, None),
        };
        let proxy = host
            .proxy_id
            .and_then(|pid| store.lock().find_proxy_config(pid).ok().flatten());

        chain.push(JumpHostInfo {
            host: host.host.clone(),
            port: host.port,
            username: host.username.clone(),
            password,
            private_key,
            passphrase,
            proxy,
        });
        next = host.jump_host_id;
    }

    // The walk goes from the target's own jump outward (innermost hop
    // first); the connection is established outermost hop first — reverse.
    chain.reverse();
    chain
}

/// Returns true if setting `candidate_jump_id` as `host_id`'s jump host
/// would create a cycle — i.e. the candidate's own jump chain (including
/// the candidate itself) already passes through `host_id`.
///
/// The dropdown filters such candidates out up front
/// (`render_jump_host_selector`); this is the save-time backstop for stale
/// form state (e.g. another edit re-linked hosts while the form was open).
pub fn jump_would_cycle(cx: &App, host_id: i64, candidate_jump_id: i64) -> bool {
    let store = AppState::store(cx);
    let mut visited: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut cur = Some(candidate_jump_id);
    while let Some(id) = cur {
        if id == host_id {
            return true;
        }
        if !visited.insert(id) {
            // Pre-existing cycle that doesn't involve `host_id` — adding
            // this link doesn't make things worse; connect-time resolution
            // truncates it anyway.
            return false;
        }
        cur = store
            .lock()
            .find_host(id)
            .ok()
            .flatten()
            .and_then(|h| h.jump_host_id);
    }
    false
}

/// Persist (or update, or remove) the proxy row linked to a host.
///
/// - `proxy_config = None` → if `existing_id` was set, delete that proxy row
///   and return `None` (host becomes direct).
/// - `proxy_config = Some(cfg)` → if `existing_id` is set, update that row;
///   otherwise insert a new one. Returns the row id to store on the host.
///
/// The proxy is stored as an anonymous row (name = `"<host>"`) so it
/// doesn't clutter a future proxies-management UI.
pub(super) fn upsert_proxy_for_host(
    proxy_config: &Option<ProxyConfig>,
    existing_id: Option<i64>,
    cx: &mut App,
) -> Option<i64> {
    tracing::info!(
        "upsert_proxy_for_host: existing_id={:?}, has_config={}",
        existing_id,
        proxy_config.is_some()
    );
    let store = AppState::store(cx);
    let proxy_config = proxy_config.as_ref()?;
    // Only persist enabled proxies (kind != None and host non-empty).
    if !proxy_config.is_enabled() {
        tracing::info!(
            "upsert_proxy_for_host: config not enabled (kind={:?}, host={:?}) — removing if set",
            proxy_config.kind,
            proxy_config.host
        );
        if let Some(id) = existing_id {
            let _ = store.lock().remove_proxy(id);
        }
        return None;
    }

    tracing::info!(
        "upsert_proxy_for_host: persisting kind={:?} {}:{} has_user={} has_pass={}",
        proxy_config.kind,
        proxy_config.host,
        proxy_config.port,
        proxy_config.username.is_some(),
        proxy_config.password.is_some()
    );

    let password_bytes = proxy_config
        .password
        .as_ref()
        .map(|p| p.as_bytes().to_vec());

    let entry = ProxyEntry {
        id: existing_id.unwrap_or(0),
        name: String::new(), // anonymous — tied to the host
        kind: proxy_config.kind,
        host: proxy_config.host.clone(),
        port: proxy_config.port,
        username: proxy_config.username.clone(),
        password: password_bytes,
        created_at: 0,
    };

    let store = store.lock();
    match existing_id {
        Some(id) => {
            let res = store.update_proxy(&entry);
            tracing::info!(
                "upsert_proxy_for_host: update_proxy({}) result={:?}",
                id,
                res.as_ref().err()
            );
            let _ = res;
            Some(id)
        }
        None => {
            let res = store.add_proxy(&entry);
            tracing::info!(
                "upsert_proxy_for_host: add_proxy result={:?}",
                res.as_ref().err()
            );
            res.ok()
        }
    }
}
