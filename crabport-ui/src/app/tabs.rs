//! Tab lifecycle methods for `CrabportApp` (home/terminal switching, SSH
//! tabs, activation, closing, and the command-palette toggle).

use std::sync::Arc;

use gpui::*;
use rust_i18n::t;

use super::{CrabPortTab, CrabportApp, Tab, TabKind, ToggleCommand};
use crate::components::button::Button;
use crate::components::notification::{Notification, NotificationLevel};
use crate::views::terminal::TerminalView;
use crate::views::terminal::split::{SplitDir, SplitTree};
use crabport_ssh::backend::SshBackend;
use crabport_ssh::session::SshConnectionInfo;
use crabport_telnet::backend::TelnetBackend;
use crabport_telnet::session::TelnetConnectionInfo;
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
                        // Rename: success is silent (no notification), only
                        // surface a toast on failure.
                        (SftpTransferKind::Rename, true) => return,
                        (SftpTransferKind::Rename, false) => (
                            t!("sftp.notif_rename_failed_title").to_string(),
                            t!("sftp.notif_rename_failed_msg", message = message.as_str())
                                .to_string(),
                            NotificationLevel::Danger,
                            std::time::Duration::from_secs(5),
                        ),
                        // Edit: success is silent, only upload/save
                        // failures surface a toast.
                        (SftpTransferKind::Edit, true) => return,
                        (SftpTransferKind::Edit, false) => (
                            t!("sftp.notif_edit_save_failed_title").to_string(),
                            t!(
                                "sftp.notif_edit_save_failed_msg",
                                message = message.as_str()
                            )
                            .to_string(),
                            NotificationLevel::Danger,
                            std::time::Duration::from_secs(5),
                        ),
                        (SftpTransferKind::Delete, true) => (
                            t!("sftp.notif_delete_done_title").to_string(),
                            t!("sftp.notif_delete_done_msg", message = message.as_str())
                                .to_string(),
                            NotificationLevel::Success,
                            std::time::Duration::from_secs(3),
                        ),
                        (SftpTransferKind::Delete, false) => (
                            t!("sftp.notif_delete_failed_title").to_string(),
                            t!("sftp.notif_delete_failed_msg", message = message.as_str())
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

        // Sync the split tree's active pane when this pane receives keyboard
        // focus (e.g. via Tab cycling), so `split_active_pane` and the
        // toolbar follow keyboard focus, not just mouse clicks.
        self.register_pane_focus_callback(&terminal_view, cx);

        self.terminal_views.insert(id, terminal_view.clone());
        self.init_split_for_tab(id, terminal_view.clone());

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
                None, // no TelnetConnectionInfo for SSH
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
                        // Rename: success is silent (no notification), only
                        // surface a toast on failure.
                        (SftpTransferKind::Rename, true) => return,
                        (SftpTransferKind::Rename, false) => (
                            t!("sftp.notif_rename_failed_title").to_string(),
                            t!("sftp.notif_rename_failed_msg", message = message.as_str())
                                .to_string(),
                            NotificationLevel::Danger,
                            std::time::Duration::from_secs(5),
                        ),
                        // Edit: success is silent, only upload/save
                        // failures surface a toast.
                        (SftpTransferKind::Edit, true) => return,
                        (SftpTransferKind::Edit, false) => (
                            t!("sftp.notif_edit_save_failed_title").to_string(),
                            t!(
                                "sftp.notif_edit_save_failed_msg",
                                message = message.as_str()
                            )
                            .to_string(),
                            NotificationLevel::Danger,
                            std::time::Duration::from_secs(5),
                        ),
                        (SftpTransferKind::Delete, true) => (
                            t!("sftp.notif_delete_done_title").to_string(),
                            t!("sftp.notif_delete_done_msg", message = message.as_str())
                                .to_string(),
                            NotificationLevel::Success,
                            std::time::Duration::from_secs(3),
                        ),
                        (SftpTransferKind::Delete, false) => (
                            t!("sftp.notif_delete_failed_title").to_string(),
                            t!("sftp.notif_delete_failed_msg", message = message.as_str())
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

        // Sync the split tree's active pane when this pane receives keyboard
        // focus (e.g. via Tab cycling), so `split_active_pane` and the
        // toolbar follow keyboard focus, not just mouse clicks.
        self.register_pane_focus_callback(&terminal_view, cx);

        self.terminal_views.insert(id, terminal_view.clone());
        self.init_split_for_tab(id, terminal_view.clone());

        self.active_tab_id = id;
        id
    }

    /// Open a Telnet terminal tab. Mirrors `add_ssh_tab` but uses the
    /// `TelnetBackend` (raw TCP + IAC negotiation, no SFTP / tunnels).
    /// Credentials are not auto-sent in v1 — the server's `login:` /
    /// `Password:` prompts pass through to the terminal so the user types
    /// them. The password field is still persisted so saved hosts reconnect
    /// without re-entry (a future auto-login flow can consume it).
    pub fn add_telnet_tab(
        &mut self,
        name: &str,
        host_id: Option<i64>,
        host: &str,
        port: u16,
        username: &str,
        password: &str,
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

        let mut info = TelnetConnectionInfo::new(host, username, password).with_port(port);
        if let Some(p) = proxy {
            info = info.with_proxy(p);
        }
        let info_for_view = info.clone();

        let cols: usize = 80;
        let rows: usize = 24;

        // Telnet has no host-key verification, but the connection overlay is
        // still used for status logging ("Connecting to …", "TCP connection
        // established").
        let overlay: crate::views::terminal::connection_overlay::SharedOverlayState =
            std::sync::Arc::new(parking_lot::Mutex::new(
                crate::views::terminal::connection_overlay::ConnectionOverlayState::new(),
            ));
        let overlay_cb = overlay.clone();

        let backend = Arc::new(TelnetBackend::new(
            info,
            cols as u16,
            rows as u16,
            Arc::new(move |msg: String| {
                overlay_cb.lock().log(
                    crate::views::terminal::connection_overlay::ConnectionLogLevel::Info,
                    msg,
                );
            }),
        ));
        let terminal_view = cx.new(|cx| {
            TerminalView::with_backend_and_host_and_overlay(
                backend,
                cols,
                rows,
                format!("{}@{}", username, host),
                host_id,
                overlay,
                None,                // no SshConnectionInfo for telnet
                Some(info_for_view), // TelnetConnectionInfo for reconnect
                id,
                cx,
            )
        });
        // Auto-close the tab when the telnet session ends.
        let app_handle = cx.entity().clone();
        terminal_view.update(cx, |view, _cx| {
            view.set_on_backend_closed(move |cx| {
                app_handle.update(cx, |app, cx| {
                    app.close_tab(id, cx);
                });
            });
        });

        // Sync the split tree's active pane when this pane receives keyboard
        // focus (e.g. via Tab cycling), so `split_active_pane` and the
        // toolbar follow keyboard focus, not just mouse clicks.
        self.register_pane_focus_callback(&terminal_view, cx);

        self.terminal_views.insert(id, terminal_view.clone());
        self.init_split_for_tab(id, terminal_view.clone());

        self.active_tab_id = id;
        id
    }

    pub fn activate_tab(&mut self, id: u64) {
        if self.tabs.iter().any(|t| t.id == id) {
            self.active_tab_id = id;
        }
    }

    pub fn close_tab(&mut self, id: u64, cx: &mut Context<Self>) {
        if id == 0 || id == 1 {
            // Home and SFTP tabs are permanent — never closeable.
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
        // Tear down every pane belonging to this tab: close each pane's
        // backend and drop its view from the pane registry.
        if let Some(tree) = self.split_trees.remove(&id) {
            for pane_id in tree.pane_ids() {
                if let Some(view) = self.pane_views.remove(&pane_id) {
                    view.update(cx, |v, _cx| v.close());
                }
                // Clear last-focused record if it pointed at one of these.
                if self.last_focused_pane == Some(pane_id) {
                    self.last_focused_pane = None;
                }
            }
        }
        // Drop this tab's per-tab panel state so the HashMaps don't leak
        // entries for closed tabs.
        self.panel_active_tab.remove(&id);
        self.app_ctx.tunnels_panel.update(cx, |panel, cx| {
            panel.forget_tab(id);
            cx.notify();
        });
        self.tabs.retain(|t| t.id != id);
        if self.active_tab_id == id {
            self.active_tab_id = 0;
        }
    }

    // -----------------------------------------------------------------------
    // Split panes
    // -----------------------------------------------------------------------

    /// Allocate a fresh, unique pane id.
    fn alloc_pane_id(&mut self) -> u64 {
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        id
    }

    /// Register a freshly-created terminal tab's single pane in the split
    /// registry. Called from `add_tab` / `add_ssh_tab` / `add_telnet_tab`
    /// after the [`TerminalView`] is created and inserted into
    /// `terminal_views`. Initializes a single-pane [`SplitTree`] and the
    /// pane-view registry.
    pub fn init_split_for_tab(&mut self, tab_id: u64, view: Entity<TerminalView>) {
        let pane_id = self.alloc_pane_id();
        self.pane_views.insert(pane_id, view);
        self.split_trees.insert(tab_id, SplitTree::single(pane_id));
    }

    /// Close the currently-focused pane of `tab_id`. If the tab has only one
    /// pane left, the whole tab is closed (mirrors `close_tab`). This is the
    /// entry point for the tab-bar close button: instead of killing all
    /// panes at once, it closes just the focused one.
    pub fn close_active_pane_or_tab(&mut self, tab_id: u64, cx: &mut Context<Self>) {
        let active_pane = self.split_trees.get(&tab_id).map(|t| t.active_pane);
        match active_pane {
            Some(pane_id) => {
                // close_pane closes just that pane, and closes the tab if it
                // was the last one.
                self.close_pane(tab_id, pane_id, cx);
            }
            None => {
                // No split tree (e.g. Home tab) → close the whole tab.
                self.close_tab(tab_id, cx);
            }
        }
    }

    /// Split the active pane of the active tab in `dir`. Creates a new local
    /// PTY [`TerminalView`] for the new pane, registers it, and splices it
    /// into the split tree. The new pane becomes active. No-op if the active
    /// tab isn't a terminal tab.
    ///
    /// The pane being split is the **keyboard-focused** pane (not just the
    /// last-clicked one). We find it by scanning the active tab's panes for
    /// the one whose `TerminalView::is_focused()` returns true, falling back
    /// to the tree's `active_pane` if none reports focus (e.g. focus is
    /// elsewhere but the user invokes split via the toolbar button).
    pub fn split_active_pane(&mut self, dir: SplitDir, cx: &mut Context<Self>) {
        let tab_id = self.active_tab_id;
        let Some(tree) = self.split_trees.get(&tab_id).cloned() else {
            return;
        };
        // The pane being split is the last one to have had keyboard focus
        // (`last_focused_pane`), tracked via each pane's `on_focused`
        // callback and via `focus_pane` on click. This is **not** cleared
        // when focus moves to a non-terminal element (e.g. the split toolbar
        // button), so splitting always targets the pane the user was last
        // typing in — e.g. split-down splits the focused left pane, not the
        // right one. Falls back to the tree's `active_pane` if we have no
        // record (e.g. focus was never on a pane this session).
        let active_pane = self
            .last_focused_pane
            .filter(|&p| tree.root.find_pane(p))
            .unwrap_or(tree.active_pane);
        let new_pane_id = self.alloc_pane_id();

        // Create an independent new PTY/channel for the split pane.
        // `spawn_channel` asks the backend to open a new channel on the
        // existing connection (SSH: new session channel; local: new shell).
        // For Telnet (no channel multiplexing), it returns `None` and we
        // fall back to creating a new TelnetBackend (new TCP connection).
        let view = if let Some(src) = self.pane_views.get(&active_pane).cloned() {
            let count = new_pane_id;
            // Extract the backend + metadata from the source view. `spawn_channel`
            // is synchronous (SSH uses `TOKIO.block_on` internally) so we can
            // call it here without a TerminalView context.
            let spawned_backend = src.read_with(cx, |v, _| v.spawn_channel_backend(80, 24));
            let host = src.read_with(cx, |v, _| v.remote_host().to_string());
            let host_id = src.read_with(cx, |v, _| v.host_id());
            let overlay = src.read_with(cx, |v, _| v.overlay_state());
            let ssh_info = src.read_with(cx, |v, _| v.ssh_info().cloned());
            let telnet_info = src.read_with(cx, |v, _| v.telnet_info().cloned());
            let tunnel_source = src.read_with(cx, |v, _| v.tunnel_source_arc());
            // Share the source pane's command history so all split panes of
            // this tab see the same list in the History panel.
            let shared_history = src.read_with(cx, |v, _| v.command_history_arc());

            if let Some(backend) = spawned_backend {
                // SSH channel / local PTY: build the view with the spawned backend.
                cx.new(|cx| {
                    TerminalView::with_backend_and_host_and_overlay_and_history(
                        backend,
                        80,
                        24,
                        host,
                        host_id,
                        overlay,
                        ssh_info,
                        telnet_info,
                        count,
                        Some(shared_history),
                        cx,
                    )
                    .with_tunnel_source_opt(tunnel_source)
                })
            } else if let Some(info) = telnet_info {
                // Telnet fallback: create a new connection.
                let overlay_cb = overlay.clone();
                let backend: Arc<dyn crabport_terminal::terminal::CrabPortTerminal> = Arc::new(
                    TelnetBackend::new(
                        info.clone(),
                        80,
                        24,
                        Arc::new(move |msg: String| {
                            overlay_cb.lock().log(
                                crate::views::terminal::connection_overlay::ConnectionLogLevel::Info,
                                msg,
                            );
                        }),
                    ),
                );
                cx.new(|cx| {
                    TerminalView::with_backend_and_host_and_overlay_and_history(
                        backend,
                        80,
                        24,
                        host,
                        host_id,
                        overlay,
                        None,
                        Some(info),
                        count,
                        Some(shared_history),
                        cx,
                    )
                })
            } else {
                // Ultimate fallback: fresh local PTY.
                cx.new(|cx| TerminalView::new(count, cx))
            }
        } else {
            cx.new(|cx| TerminalView::new(new_pane_id, cx))
        };

        // Each pane has an independent connection now, so only the pane
        // whose backend closed is affected.
        let app_handle = cx.entity().clone();
        view.update(cx, |v, _cx| {
            v.set_on_backend_closed(move |cx| {
                app_handle.update(cx, |app, cx| {
                    app.close_pane(tab_id, new_pane_id, cx);
                });
            });
        });
        // Sync the split tree's active pane when this pane receives keyboard
        // focus, so subsequent splits target the focused pane.
        self.register_pane_focus_callback(&view, cx);
        self.pane_views.insert(new_pane_id, view);
        if let Some(tree) = self.split_trees.get_mut(&tab_id) {
            tree.split_active(dir, new_pane_id);
            // Sync terminal_views[tab_id] → the now-active pane's view so the
            // toolbar / panel logic keeps reading the focused pane.
            if let Some(active_view) = self.pane_views.get(&tree.active_pane).cloned() {
                self.terminal_views.insert(tab_id, active_view);
            }
        }
        // Move keyboard focus to the newly-created pane so the user can
        // immediately type into it, and its cursor renders solid. Done on
        // the next render (where a `&mut Window` is available) rather than
        // here because `split_active_pane` only has a `&mut Context<Self>`.
        self.pending_focus_pane = Some(new_pane_id);
        self.last_focused_pane = Some(new_pane_id);
        cx.notify();
    }

    /// Close a single pane by pane id. If it was the last pane in the tab,
    /// the whole tab is closed (mirrors `close_tab`). Otherwise the split
    /// tree collapses and the active pane is updated.
    pub fn close_pane(&mut self, tab_id: u64, pane_id: u64, cx: &mut Context<Self>) {
        // Remove the pane's view from the registry + close its backend.
        if let Some(view) = self.pane_views.remove(&pane_id) {
            view.update(cx, |v, _cx| v.close());
        }
        // If the closed pane was the last focused one, drop the record so
        // `split_active_pane` falls back to the tree's active pane.
        if self.last_focused_pane == Some(pane_id) {
            self.last_focused_pane = None;
        }
        let Some(tree) = self.split_trees.remove(&tab_id) else {
            return;
        };
        match tree.remove_pane(pane_id) {
            // Tab is now empty → close it entirely.
            None => {
                self.close_tab(tab_id, cx);
            }
            Some(new_tree) => {
                self.split_trees.insert(tab_id, new_tree.clone());
                // Sync terminal_views[tab_id] → active pane.
                if let Some(active_view) = self.pane_views.get(&new_tree.active_pane).cloned() {
                    self.terminal_views.insert(tab_id, active_view);
                }
                // Move keyboard focus to the remaining active pane so the
                // user can keep typing without clicking another pane first.
                // Mirrors the split-pane path: set `pending_focus_pane` so
                // the next render (where `&mut Window` is available) grabs
                // focus, and track it as the last-focused pane.
                self.pending_focus_pane = Some(new_tree.active_pane);
                self.last_focused_pane = Some(new_tree.active_pane);
                cx.notify();
            }
        }
    }

    /// Focus a specific pane within a tab (called when the user clicks a
    /// pane). Updates the split tree's active pane and syncs
    /// `terminal_views[tab_id]` so the toolbar / right-hand panel follows the
    /// focused pane. Keyboard focus is grabbed separately by the caller
    /// (which has a `&mut Window`).
    pub fn focus_pane(&mut self, tab_id: u64, pane_id: u64, cx: &mut Context<Self>) {
        if let Some(tree) = self.split_trees.get_mut(&tab_id) {
            if tree.root.find_pane(pane_id) {
                tree.active_pane = pane_id;
            }
        }
        // Record this as the last pane to have keyboard focus so
        // `split_active_pane` targets it even after focus moves to a
        // non-terminal element (e.g. the split toolbar button).
        self.last_focused_pane = Some(pane_id);
        if let Some(view) = self.pane_views.get(&pane_id).cloned() {
            self.terminal_views.insert(tab_id, view);
        }
        cx.notify();
    }

    /// Update a split's divider ratio while dragging. `pane_id` identifies
    /// which side of the divider the drag is controlling (the leaf pane
    /// nearest the cursor).
    pub fn set_split_ratio(
        &mut self,
        tab_id: u64,
        pane_id: u64,
        ratio: f32,
        cx: &mut Context<Self>,
    ) {
        if let Some(tree) = self.split_trees.get_mut(&tab_id) {
            tree.set_ratio_for_child(pane_id, ratio);
        }
        cx.notify();
    }

    /// Mark `pane_id` as the active pane of whichever tab owns it, syncing
    /// `terminal_views[tab]` so the toolbar / right-hand panel follow keyboard
    /// focus. Called from a pane's `on_focused` callback when it receives
    /// keyboard focus (e.g. via Tab cycling or clicking), so that
    /// `split_active_pane` operates on the focused pane rather than just the
    /// last-clicked one.
    ///
    /// This is a no-op if no split tree contains `pane_id` (e.g. the pane was
    /// closed between focus being grabbed and this callback firing).
    pub fn sync_active_pane_from_focus(&mut self, pane_id: u64, cx: &mut Context<Self>) {
        let tab_id = self
            .split_trees
            .iter()
            .find(|(_, tree)| tree.root.find_pane(pane_id))
            .map(|(t, _)| *t);
        let Some(tab_id) = tab_id else {
            return;
        };
        // Record this as the last pane to have keyboard focus so
        // `split_active_pane` can target it even after focus moves to a
        // non-terminal element (e.g. the split toolbar button).
        self.last_focused_pane = Some(pane_id);
        // Avoid redundant work / notify if the pane is already active.
        let already_active = self
            .split_trees
            .get(&tab_id)
            .is_some_and(|t| t.active_pane == pane_id);
        if already_active {
            return;
        }
        if let Some(tree) = self.split_trees.get_mut(&tab_id) {
            tree.active_pane = pane_id;
        }
        if let Some(view) = self.pane_views.get(&pane_id).cloned() {
            self.terminal_views.insert(tab_id, view);
        }
        cx.notify();
    }

    /// Wire a freshly-created pane's `on_focused` callback so that when it
    /// receives keyboard focus the app marks it as the active pane of its
    /// tab. Used by `add_tab` / `add_ssh_tab` / `add_telnet_tab` /
    /// `split_active_pane`.
    fn register_pane_focus_callback(
        &mut self,
        view: &Entity<TerminalView>,
        cx: &mut Context<Self>,
    ) {
        let app_handle = cx.entity().downgrade();
        view.update(cx, |v, _cx| {
            v.set_on_focused(move |pane_id, cx| {
                let _ = app_handle.update(cx, |app, cx| {
                    app.sync_active_pane_from_focus(pane_id, cx);
                });
            });
        });
    }

    /// Begin a divider drag for the split whose first child is `pane_id`.
    /// `bounds` is the split container's pixel rect, captured at drag start
    /// so subsequent mouse moves can convert cursor → ratio.
    pub fn begin_split_drag(
        &mut self,
        tab_id: u64,
        pane_id: u64,
        dir: crate::views::terminal::split::SplitDir,
        bounds: gpui::Bounds<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let (origin, extent) = match dir {
            crate::views::terminal::split::SplitDir::Vertical => {
                (f32::from(bounds.origin.x), f32::from(bounds.size.width))
            }
            crate::views::terminal::split::SplitDir::Horizontal => {
                (f32::from(bounds.origin.y), f32::from(bounds.size.height))
            }
        };
        if extent <= 0.0 {
            return;
        }
        self.split_drag = Some(crate::views::terminal::split::SplitDrag {
            tab_id,
            pane_id,
            dir,
            origin,
            extent,
        });
        cx.notify();
    }

    /// Update the active drag's ratio from a window-space cursor position.
    pub fn update_split_drag(&mut self, cursor: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = self.split_drag.clone() else {
            return;
        };
        let pos = match drag.dir {
            crate::views::terminal::split::SplitDir::Vertical => f32::from(cursor.x),
            crate::views::terminal::split::SplitDir::Horizontal => f32::from(cursor.y),
        };
        let ratio = ((pos - drag.origin) / drag.extent).clamp(0.05, 0.95);
        self.set_split_ratio(drag.tab_id, drag.pane_id, ratio, cx);
    }

    /// End the active drag (mouse up).
    pub fn end_split_drag(&mut self, cx: &mut Context<Self>) {
        if self.split_drag.take().is_some() {
            cx.notify();
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
