//! OpenSSH user-config import lifecycle for `CrabportApp`.

use gpui::*;
use rust_i18n::t;

use crabport_core::ssh_import::scan_default_ssh_config;

use super::CrabportApp;
use crate::app_state::AppState;
use crate::components::notification::{Notification, NotificationLevel};
use crate::views::sessions::ConnectionHost;
use crate::views::sessions::import::SshImportState;

impl CrabportApp {
    /// Scan the default OpenSSH config and open a validated import preview.
    pub fn open_ssh_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        tracing::info!("ssh import: opening system OpenSSH configuration preview");
        let scan = match scan_default_ssh_config() {
            Ok(scan) => scan,
            Err(error) => {
                tracing::warn!("ssh import: scan failed: {error}");
                self.show_ssh_import_notification(
                    t!("ssh_import.scan_failed_title").to_string(),
                    t!(
                        "ssh_import.scan_failed_message",
                        error = error.to_string().as_str()
                    )
                    .to_string(),
                    NotificationLevel::Danger,
                    cx,
                );
                return;
            }
        };
        if scan.candidates.is_empty() {
            tracing::info!("ssh import: no concrete host aliases were found");
            self.show_ssh_import_notification(
                t!("ssh_import.empty_title").to_string(),
                t!("ssh_import.empty_message").to_string(),
                NotificationLevel::Info,
                cx,
            );
            return;
        }

        let existing_hosts = match AppState::store(cx).lock().hosts() {
            Ok(hosts) => hosts,
            Err(error) => {
                tracing::error!("ssh import: failed to load saved hosts: {error}");
                self.show_ssh_import_notification(
                    t!("ssh_import.scan_failed_title").to_string(),
                    t!("ssh_import.store_read_failed").to_string(),
                    NotificationLevel::Danger,
                    cx,
                );
                return;
            }
        };
        // The click callback already owns the active window. Re-entering it via
        // `WindowHandle::update` fails and previously left the preview unopened.
        let state = SshImportState::new(scan, existing_hosts, window, cx);
        tracing::info!(
            "ssh import: preview ready with {} candidates",
            state.rows.len()
        );
        self.ssh_import = Some(state);
        cx.notify();
    }

    /// Close the import preview and drop its masked input state after animation.
    pub fn close_ssh_import(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.ssh_import.as_mut() {
            state.open = false;
        }
        let app = cx.entity().clone();
        cx.spawn(async move |_this, cx| {
            smol::Timer::after(std::time::Duration::from_millis(200)).await;
            let _ = app.update(cx, |app, cx| {
                if app.ssh_import.is_some() {
                    app.ssh_import = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Validate password phrases and persist every selected host atomically.
    pub fn perform_ssh_import(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(output) = self.ssh_import.as_mut().and_then(|state| state.output(cx)) else {
            tracing::info!("ssh import: preview validation prevented submission");
            cx.notify();
            return;
        };

        tracing::info!(
            "ssh import: submitting {} validated records",
            output.records.len()
        );
        let result = AppState::store(cx).lock().import_ssh_hosts(&output.records);
        match result {
            Ok(summary) => {
                // Refresh from SQLite so ordering and all preserved fields match
                // the committed transaction exactly; importing never connects.
                if let Ok(hosts) = AppState::store(cx).lock().hosts() {
                    self.hosts = hosts.into_iter().map(ConnectionHost::from).collect();
                }
                self.show_ssh_import_notification(
                    t!("ssh_import.success_title").to_string(),
                    t!(
                        "ssh_import.success_message",
                        created = summary.created,
                        updated = summary.updated,
                        skipped = output.skipped,
                        warnings = output.warnings
                    )
                    .to_string(),
                    NotificationLevel::Success,
                    cx,
                );
                self.close_ssh_import(cx);
            }
            Err(error) => {
                tracing::error!("ssh import: transaction rolled back: {error}");
                self.show_ssh_import_notification(
                    t!("ssh_import.failed_title").to_string(),
                    t!("ssh_import.failed_message").to_string(),
                    NotificationLevel::Danger,
                    cx,
                );
            }
        }
        cx.notify();
    }

    /// Show a consistently timed notification for the SSH import workflow.
    fn show_ssh_import_notification(
        &self,
        title: String,
        message: String,
        level: NotificationLevel,
        cx: &mut Context<Self>,
    ) {
        self.app_ctx.notifications.update(cx, |controller, cx| {
            controller.show(
                Notification::new(title)
                    .level(level)
                    .message(message)
                    .duration(std::time::Duration::from_secs(
                        if level == NotificationLevel::Danger {
                            6
                        } else {
                            4
                        },
                    )),
                cx,
            );
        });
    }
}
