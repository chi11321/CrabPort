//! Host management.
//!
//! Contains the methods that connect to, remove, and edit saved hosts.

use std::rc::Rc;

use gpui::*;
use rust_i18n::t;

use crate::app_state::AppState;
use crate::components::notification::{Notification, NotificationLevel};
use crate::views::hosts::{AuthKind, ConnectionFormState, ConnectionHost, ConnectionKind};
use crabport_core::credential::{
    CredentialEntry, CredentialKind as CoreCredentialKind, HostEntry, HostKind as CoreHostKind,
};

use super::CrabportApp;
use super::connection::upsert_proxy_for_host;

impl CrabportApp {
    /// Connect to a saved host by ID. Resolves the linked credential password.
    pub fn connect_to_host(&mut self, host_id: i64, cx: &mut Context<Self>) {
        let host = match self.hosts.iter().find(|h| h.id == host_id) {
            Some(h) => h.clone(),
            None => return,
        };

        // Update last_login timestamp
        let _ = AppState::store(cx).lock().touch_host_login(host_id);
        if let Ok(all) = AppState::store(cx).lock().hosts() {
            self.hosts = all
                .into_iter()
                .map(|h| ConnectionHost {
                    id: h.id,
                    name: h.name,
                    host: h.host,
                    port: h.port,
                    username: h.username,
                    kind: match h.kind {
                        CoreHostKind::Ssh => crate::views::hosts::ConnectionKind::SSH,
                        CoreHostKind::Telnet => crate::views::hosts::ConnectionKind::Telnet,
                        CoreHostKind::Serial => crate::views::hosts::ConnectionKind::Serial,
                    },
                    credential_id: h.credential_id,
                    last_login: h.last_login,
                    favorite: h.favorite,
                    proxy_id: h.proxy_id,
                })
                .collect();
        }

        // Try to resolve password and private key from linked credential
        let cred = host.credential_id.and_then(|cid| {
            AppState::store(cx)
                .lock()
                .find_credential(cid)
                .ok()
                .flatten()
        });

        // Resolve password / passphrase based on credential kind
        let (password, private_key, passphrase) = match cred.as_ref() {
            Some(c) if c.kind == CoreCredentialKind::Certificate => (
                String::new(),
                if c.private_key.is_empty() {
                    None
                } else {
                    Some(c.private_key.as_str())
                },
                if c.secret.is_empty() {
                    None
                } else {
                    Some(c.secret.as_str())
                },
            ),
            Some(c) => (c.secret.clone(), None, None),
            None => (String::new(), None, None),
        };

        // Resolve the saved proxy (if any) into a ready-to-use config.
        let proxy_config = host.proxy_id.and_then(|pid| {
            AppState::store(cx)
                .lock()
                .find_proxy_config(pid)
                .ok()
                .flatten()
        });

        self.add_ssh_tab(
            &host.name,
            Some(host_id),
            &host.host,
            host.port,
            &host.username,
            &password,
            private_key,
            passphrase,
            proxy_config,
            cx,
        );
    }

    // -----------------------------------------------------------------------
    // Host management
    // -----------------------------------------------------------------------

    pub fn remove_host(&mut self, host_id: i64, cx: &mut Context<Self>) {
        let host = self.hosts.iter().find(|h| h.id == host_id);
        if let Some(h) = host {
            if let Some(cred_id) = h.credential_id {
                let _ = AppState::store(cx).lock().remove_credential(cred_id);
            }
        }
        let _ = AppState::store(cx).lock().remove_host(host_id);
        self.hosts.retain(|h| h.id != host_id);
        cx.notify();
    }

    pub fn edit_host(&mut self, host_id: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.close_connection_form(cx);

        let host = self.hosts.iter().find(|h| h.id == host_id).cloned();
        if host.is_none() {
            return;
        }
        let h = host.unwrap();

        let cred = h.credential_id.and_then(|cid| {
            AppState::store(cx)
                .lock()
                .find_credential(cid)
                .ok()
                .flatten()
        });

        let mut form = ConnectionFormState::new(window, cx);

        form.name_input.update(cx, |state, cx| {
            state.set_value(&h.name, window, cx);
        });
        form.host_input.update(cx, |state, cx| {
            state.set_value(&h.host, window, cx);
        });
        form.port_input.update(cx, |state, cx| {
            state.set_value(&h.port.to_string(), window, cx);
        });
        form.user_input.update(cx, |state, cx| {
            state.set_value(&h.username, window, cx);
        });

        if let Some(c) = cred.as_ref() {
            match c.kind {
                CoreCredentialKind::Password => {
                    form.pass_input.update(cx, |state, cx| {
                        state.set_value(&c.secret, window, cx);
                    });
                }
                CoreCredentialKind::Certificate => {
                    form.auth_kind = AuthKind::Certificate;
                    form.passphrase_input.update(cx, |state, cx| {
                        state.set_value(&c.secret, window, cx);
                    });
                    form.private_key_input.update(cx, |state, cx| {
                        state.set_value(&c.private_key, window, cx);
                    });
                }
            }
        }

        // Load the saved proxy (if any) so the user can edit / clear it.
        // System proxies aren't persisted, so we only resolve a config for a
        // real `proxy_id`.
        let saved_proxy = h.proxy_id.and_then(|pid| {
            AppState::store(cx)
                .lock()
                .find_proxy_config(pid)
                .ok()
                .flatten()
                .map(|cfg| (pid, cfg))
        });
        #[cfg(debug_assertions)]
        tracing::info!(
            "edit_host: host_id={}, host.proxy_id={:?}, resolved_saved_proxy={}",
            h.id,
            h.proxy_id,
            saved_proxy.is_some()
        );
        match saved_proxy {
            Some((pid, cfg)) => form.load_proxy(Some(pid), Some(&cfg), window, cx),
            None => form.load_proxy(None, None, window, cx),
        }

        let app = cx.entity().clone();
        let editing_host_id = h.id;
        let editing_cred_id = h.credential_id;
        let editing_proxy_id = h.proxy_id;

        form.editing = true;

        form.on_connect = Some(Rc::new({
            let app = app.clone();
            move |_kind: ConnectionKind, _w: &mut Window, cx: &mut App| {
                #[cfg(debug_assertions)]
                tracing::info!(
                    "edit_host: on_connect fired — editing_proxy_id={:?}",
                    editing_proxy_id
                );
                app.update(cx, |app, cx| {
                    // Validate required fields before doing anything. If the
                    // form is invalid, per-field errors are shown and a toast
                    // is surfaced; the save flow is aborted.
                    if !app.validate_connection_form(cx) {
                        return;
                    }
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
                        #[cfg(debug_assertions)]
                        tracing::info!(
                            "edit_host: reading form — proxy_kind={:?}, form.proxy_id={:?}, proxy_url={:?}",
                            f.proxy_kind,
                            f.proxy_id,
                            f.proxy_url_text(cx)
                        );
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

                    if let Some(old_cred_id) = editing_cred_id {
                        let _ = AppState::store(cx).lock().remove_credential(old_cred_id);
                    }

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
                    let new_cred_id = AppState::store(cx)
                        .lock()
                        .add_credential(&cred)
                        .unwrap_or(0);

                    let entry = HostEntry {
                        id: editing_host_id,
                        name: name.clone(),
                        host: host.clone(),
                        port: port_num,
                        username: username.clone(),
                        credential_id: Some(new_cred_id),
                        kind: CoreHostKind::Ssh,
                        last_login: None,
                        favorite: false,
                        proxy_id: upsert_proxy_for_host(&proxy_config, editing_proxy_id, cx),
                    };
                    #[cfg(debug_assertions)]
                    tracing::info!(
                        "edit_host: on_connect — editing_proxy_id={:?}, resolved_entry.proxy_id={:?}",
                        editing_proxy_id,
                        entry.proxy_id
                    );
                    let save_result = AppState::store(cx).lock().update_host(&entry);
                    let host_name = name.clone();
                    app.app_ctx.notifications.update(cx, |c, cx| {
                        let (title, msg, level) = match &save_result {
                            Ok(()) => (
                                t!("hosts.notif_saved_title").to_string(),
                                t!("hosts.notif_saved_msg", name = host_name.as_str()).to_string(),
                                NotificationLevel::Success,
                            ),
                            Err(e) => {
                                tracing::error!("update_host failed: {e}");
                                (
                                    t!("hosts.notif_save_failed_title").to_string(),
                                    t!("hosts.notif_save_failed_msg", name = host_name.as_str())
                                        .to_string(),
                                    NotificationLevel::Danger,
                                )
                            }
                        };
                        c.show(
                            Notification::new(title)
                                .level(level)
                                .message(msg)
                                .duration(std::time::Duration::from_secs(
                                    if save_result.is_ok() { 3 } else { 5 },
                                )),
                            cx,
                        );
                    });
                    if save_result.is_err() {
                        return;
                    }

                    if let Some(h) = app.hosts.iter_mut().find(|h| h.id == editing_host_id) {
                        h.name = name.clone();
                        h.host = host.clone();
                        h.port = port_num;
                        h.username = username.clone();
                        h.credential_id = Some(new_cred_id);
                        h.proxy_id = entry.proxy_id;
                    }

                    cx.notify();
                });
            }
        }));

        form.on_close = Some(Rc::new({
            let app = app.clone();
            move |_w, cx| {
                app.update(cx, |app, cx| {
                    app.close_connection_form(cx);
                });
            }
        }));

        form.open(window, cx);
        self.connection_form = Some(form);
        cx.notify();
    }
}
