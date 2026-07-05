//! Tab lifecycle methods for `CrabportApp` (home/terminal switching, SSH
//! tabs, activation, closing, and the command-palette toggle).

use std::sync::Arc;

use gpui::*;
use rust_i18n::t;

use super::{CrabPortTab, CrabportApp, Tab, TabKind, ToggleCommand};
use crate::components::button::Button;
use crate::components::notification::{Notification, NotificationLevel};
use crate::views::terminal::TerminalView;
use crabport_ssh::backend::SshBackend;
use crabport_ssh::session::SshConnectionInfo;
use crabport_terminal::terminal::SftpTransferKind;

impl CrabportApp {
    // -----------------------------------------------------------------------
    // Tabs
    // -----------------------------------------------------------------------

    pub fn is_home_active(&self) -> bool {
        self.tabs
            .iter()
            .find(|t| t.id == self.active_tab_id)
            .map(|t| t.kind == TabKind::Home)
            .unwrap_or(false)
    }

    pub fn add_tab(&mut self, cx: &mut Context<Self>) -> u64 {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(Tab {
            id,
            title: format!("Terminal-{}", id),
            kind: TabKind::Terminal,
            is_remote: false,
        });

        let terminal_view = cx.new(|cx| TerminalView::new(id, cx));

        // When the local PTY child exits, automatically close the tab
        let app_handle = cx.entity().clone();
        terminal_view.update(cx, |view, _cx| {
            view.set_on_backend_closed(move |cx| {
                app_handle.update(cx, |app, cx| {
                    app.close_tab(id, cx);
                });
            });
        });

        // Re-render the app when SFTP transfer progress changes so the
        // toolbar (rendered in `render_content`) picks up the latest
        // snapshot. We use a dedicated callback rather than observing the
        // whole view to avoid re-rendering the app on every terminal frame
        // pump tick (~120Hz during output).
        let app_handle = cx.entity().downgrade();
        terminal_view.update(cx, |view, _cx| {
            view.set_on_sftp_progress_changed(move |cx| {
                let _ = app_handle.update(cx, |_, cx| cx.notify());
            });
        });

        // Surface a toast notification when an SFTP transfer finishes so the
        // user gets clear success/failure feedback even if the SFTP panel is
        // closed or scrolled out of view.
        let app_handle = cx.entity().downgrade();
        terminal_view.update(cx, |view, _cx| {
            view.set_on_sftp_transfer_finished(move |kind, success, message, cx| {
                let _ = app_handle.update(cx, |app, cx| {
                    let (title, message_notif, level, duration) = match (kind, success) {
                        (SftpTransferKind::Download, true) => (
                            t!("sftp.notif_download_done_title").to_string(),
                            t!("sftp.notif_download_done_msg", message = message.as_str())
                                .to_string(),
                            NotificationLevel::Success,
                            std::time::Duration::from_secs(3),
                        ),
                        (SftpTransferKind::Download, false) => (
                            t!("sftp.notif_download_failed_title").to_string(),
                            t!("sftp.notif_download_failed_msg", message = message.as_str())
                                .to_string(),
                            NotificationLevel::Danger,
                            std::time::Duration::from_secs(5),
                        ),
                        (SftpTransferKind::Upload, true) => (
                            t!("sftp.notif_upload_done_title").to_string(),
                            t!("sftp.notif_upload_done_msg", message = message.as_str())
                                .to_string(),
                            NotificationLevel::Success,
                            std::time::Duration::from_secs(3),
                        ),
                        (SftpTransferKind::Upload, false) => (
                            t!("sftp.notif_upload_failed_title").to_string(),
                            t!("sftp.notif_upload_failed_msg", message = message.as_str())
                                .to_string(),
                            NotificationLevel::Danger,
                            std::time::Duration::from_secs(5),
                        ),
                    };
                    app.app_ctx.notifications.update(cx, |c, cx| {
                        c.show(
                            Notification::new(title)
                                .level(level)
                                .message(message_notif)
                                .duration(duration),
                            cx,
                        );
                    });
                    cx.notify();
                });
            });
        });

        self.terminal_views.insert(id, terminal_view);

        self.active_tab_id = id;
        id
    }

    pub fn add_ssh_tab(
        &mut self,
        name: &str,
        host_id: Option<i64>,
        host: &str,
        port: u16,
        username: &str,
        password: &str,
        private_key: Option<&str>,
        passphrase: Option<&str>,
        proxy: Option<crabport_core::credential::ProxyConfig>,
        cx: &mut Context<Self>,
    ) -> u64 {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(Tab {
            id,
            title: name.to_string(),
            kind: TabKind::Terminal,
            is_remote: true,
        });

        let mut info = SshConnectionInfo::new(host, username, password).with_port(port);
        if let Some(pk) = private_key {
            info = info.with_private_key(pk, passphrase.map(|s| s.to_string()));
        }
        if let Some(p) = proxy {
            info = info.with_proxy(p);
        }
        let info_for_view = info.clone();
        let cols: usize = 80;
        let rows: usize = 24;

        // Create the overlay state early so the SSH backend callback can write to it
        let overlay: crate::views::terminal::connection_overlay::SharedOverlayState =
            std::sync::Arc::new(parking_lot::Mutex::new(
                crate::views::terminal::connection_overlay::ConnectionOverlayState::new(),
            ));
        let overlay_cb = overlay.clone();

        // Host-key verifier: pushes a confirmation prompt into the overlay
        // when the server presents an unknown key, and awaits the user's
        // decision (TOFU). See `make_host_key_verifier` for the repaint
        // mechanism.
        let verifier =
            crate::views::terminal::connection_overlay::make_host_key_verifier(overlay.clone());

        let backend = Arc::new(SshBackend::new(
            info,
            cols as u16,
            rows as u16,
            Arc::new(move |msg: String| {
                overlay_cb.lock().log(
                    crate::views::terminal::connection_overlay::ConnectionLogLevel::Info,
                    msg,
                );
            }),
            Some(verifier),
        ));
        // Clone the backend as a `CrabPortTunnel` source before it's moved
        // into the `TerminalView` (coerced to `Arc<dyn CrabPortTerminal>`).
        // `SshBackend` implements `CrabPortTunnel`, so the panel can reuse
        // this tab's SSH connection for borrowed tunnels.
        let tunnel_source: Arc<dyn crabport_ssh::CrabPortTunnel> = backend.clone();
        let terminal_view = cx.new(|cx| {
            TerminalView::with_backend_and_host_and_overlay(
                backend,
                cols,
                rows,
                format!("{}@{}", username, host),
                host_id,
                overlay,
                Some(info_for_view),
                id,
                cx,
            )
        });
        // When the SSH session closes, automatically close the tab
        let app_handle = cx.entity().clone();
        terminal_view.update(cx, |view, _cx| {
            view.set_on_backend_closed(move |cx| {
                app_handle.update(cx, |app, cx| {
                    app.close_tab(id, cx);
                });
            });
        });

        // Re-render the app when SFTP transfer progress changes so the
        // toolbar picks up the latest snapshot.
        let app_handle = cx.entity().downgrade();
        terminal_view.update(cx, |view, _cx| {
            view.set_on_sftp_progress_changed(move |cx| {
                let _ = app_handle.update(cx, |_, cx| cx.notify());
            });
        });

        // Surface a toast notification when an SFTP transfer finishes so the
        // user gets clear success/failure feedback even if the SFTP panel is
        // closed or scrolled out of view.
        let app_handle = cx.entity().downgrade();
        terminal_view.update(cx, |view, _cx| {
            view.set_on_sftp_transfer_finished(move |kind, success, message, cx| {
                let _ = app_handle.update(cx, |app, cx| {
                    let (title, message_notif, level, duration) = match (kind, success) {
                        (SftpTransferKind::Download, true) => (
                            t!("sftp.notif_download_done_title").to_string(),
                            t!("sftp.notif_download_done_msg", message = message.as_str())
                                .to_string(),
                            NotificationLevel::Success,
                            std::time::Duration::from_secs(3),
                        ),
                        (SftpTransferKind::Download, false) => (
                            t!("sftp.notif_download_failed_title").to_string(),
                            t!("sftp.notif_download_failed_msg", message = message.as_str())
                                .to_string(),
                            NotificationLevel::Danger,
                            std::time::Duration::from_secs(5),
                        ),
                        (SftpTransferKind::Upload, true) => (
                            t!("sftp.notif_upload_done_title").to_string(),
                            t!("sftp.notif_upload_done_msg", message = message.as_str())
                                .to_string(),
                            NotificationLevel::Success,
                            std::time::Duration::from_secs(3),
                        ),
                        (SftpTransferKind::Upload, false) => (
                            t!("sftp.notif_upload_failed_title").to_string(),
                            t!("sftp.notif_upload_failed_msg", message = message.as_str())
                                .to_string(),
                            NotificationLevel::Danger,
                            std::time::Duration::from_secs(5),
                        ),
                    };
                    app.app_ctx.notifications.update(cx, |c, cx| {
                        c.show(
                            Notification::new(title)
                                .level(level)
                                .message(message_notif)
                                .duration(duration),
                            cx,
                        );
                    });
                    cx.notify();
                });
            });
        });

        // Wire the `CrabPortTunnel` source captured above into the view so
        // the Tunnels panel can start borrowed tunnels reusing this tab's
        // SSH connection.
        terminal_view.update(cx, |view, _cx| {
            view.set_tunnel_source(tunnel_source);
        });

        self.terminal_views.insert(id, terminal_view);

        self.active_tab_id = id;
        id
    }

    pub fn activate_tab(&mut self, id: u64) {
        if self.tabs.iter().any(|t| t.id == id) {
            self.active_tab_id = id;
        }
    }

    pub fn close_tab(&mut self, id: u64, cx: &mut Context<Self>) {
        if id == 0 {
            return;
        }

        // Find the tab before removing it, to know if it had a close button
        let tab = self.tabs.iter().find(|t| t.id == id);
        let is_home_tab = tab.map(|t| t.kind == TabKind::Home).unwrap_or(true);

        // Clean up gpui-animation state
        let tab_btn_id = ElementId::Name(format!("tab-{}", id).into());
        let tab_wrapper_id = ElementId::Name(format!("tab-wrapper-{}", id).into());
        Button::cleanup_animation(&tab_btn_id, !is_home_tab);
        gpui_animation::reset_transition(&tab_wrapper_id);

        if let Some(view) = self.terminal_views.remove(&id) {
            view.update(cx, |v, _cx| {
                v.close();
            });
        }
        self.tabs.retain(|t| t.id != id);
        if self.active_tab_id == id {
            self.active_tab_id = 0;
        }
    }

    pub fn toggle_command(
        &mut self,
        _: &ToggleCommand,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cmd = self.app_ctx.command_palette.clone();
        let was_open = cmd.read(cx).open;
        cmd.update(cx, |cmd, cx| {
            if was_open {
                cmd.close(cx);
            } else {
                cmd.open(_window, cx);
            }
        });
        cx.notify();
    }
}
