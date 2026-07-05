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
use crate::views::hosts::{AuthKind, ConnectionFormState, ConnectionHost, ConnectionKind};
use crabport_core::credential::{
    CredentialEntry, CredentialKind as CoreCredentialKind, HostEntry, HostKind as CoreHostKind,
    ProxyConfig, ProxyEntry,
};

use super::CrabportApp;

impl CrabportApp {
    // -----------------------------------------------------------------------
    // Connection form (ephemeral entity — created on open, destroyed after close animation)
    // -----------------------------------------------------------------------

    /// Create a new ConnectionFormView entity, wire its callbacks, and open it.
    pub fn open_connection_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

        form.on_connect = Some(Rc::new({
            let a = app.clone();
            move |_kind: ConnectionKind, _w: &mut Window, cx: &mut App| {
                a.update(cx, |app, cx| {
                    // Validate required fields before doing anything. If the
                    // form is invalid, per-field errors are shown and a toast
                    // is surfaced; the save/connect flow is aborted.
                    if !app.validate_connection_form(cx) {
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
                        proxy_config,
                    ) = {
                        let f = app.connection_form.as_ref().unwrap();
                        let n = f.name_text(cx);
                        let h = f.host_text(cx);
                        let p: u16 = f.port_text(cx).parse().unwrap_or(22);
                        let u = f.user_text(cx);
                        let pw = f.pass_text(cx);
                        let pp = f.passphrase_text(cx);
                        let ak = f.auth_kind;
                        let pk = f.private_key_text(cx);
                        let pc = f.proxy_config(cx);
                        (n, h, p, u, pw, pp, ak, pk, pc)
                    };
                    app.close_connection_form(cx);

                    // Persist credential for this host
                    let (cred_kind, secret, pk) = match auth_kind {
                        AuthKind::Password => (
                            CoreCredentialKind::Password,
                            password.clone(),
                            String::new(),
                        ),
                        AuthKind::Certificate => (
                            CoreCredentialKind::Certificate,
                            passphrase.clone(),
                            private_key.clone(),
                        ),
                    };
                    let cred = CredentialEntry {
                        id: 0,
                        name: name.clone(),
                        kind: cred_kind,
                        anonymous: true,
                        secret,
                        private_key: pk,
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
                        kind: CoreHostKind::Ssh,
                        last_login: None,
                        favorite: false,
                        proxy_id,
                    };
                    let row_id = AppState::store(cx).lock().add_host(&entry).unwrap_or(0);

                    app.hosts.push(ConnectionHost {
                        id: row_id,
                        name: name.clone(),
                        host: host.to_string(),
                        port: port_num,
                        username: username.to_string(),
                        kind: crate::views::hosts::ConnectionKind::SSH,
                        credential_id: Some(cred_id),
                        last_login: None,
                        favorite: false,
                        proxy_id,
                    });
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
                    app.add_ssh_tab(
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
                        cx,
                    );
                    cx.notify();
                });
            }
        }));

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
    #[cfg(debug_assertions)]
    tracing::info!(
        "upsert_proxy_for_host: existing_id={:?}, has_config={}",
        existing_id,
        proxy_config.is_some()
    );
    let store = AppState::store(cx);
    let proxy_config = proxy_config.as_ref()?;
    // Only persist enabled proxies (kind != None and host non-empty).
    if !proxy_config.is_enabled() {
        #[cfg(debug_assertions)]
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

    #[cfg(debug_assertions)]
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
            #[cfg(debug_assertions)]
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
            #[cfg(debug_assertions)]
            tracing::info!(
                "upsert_proxy_for_host: add_proxy result={:?}",
                res.as_ref().err()
            );
            res.ok()
        }
    }
}
