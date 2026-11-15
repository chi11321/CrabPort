//! Known-hosts settings pane: inspect and forget trusted SSH host keys.
//!
//! The store itself lives in `crabport-ssh` (`crabport_ssh::known_hosts`);
//! this pane only renders it and routes destructive actions through the
//! settings window's own `AlertController`. Entries are re-read on every
//! render — the file is tiny, and a terminal tab can trust a new key while
//! this window is open, so a cached copy would go stale.

use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use rust_i18n::t;

use crabport_ssh::known_hosts::{KnownHost, KnownHosts};

use crate::color::*;
use crate::components::button::Button;
use crate::components::dialog::{AlertSeverity, AlertState};
use crate::components::input::StyledInput;
use crate::components::settings_section::Section;
use crate::motion::RADIUS_MD;

use super::{SettingsWindow, open_path};

/// Fixed row height for the single-line entry rows.
const ROW_HEIGHT: f32 = 40.0;

impl SettingsWindow {
    /// The Known Hosts pane: a searchable list of the TOFU trust store.
    pub(super) fn render_known_hosts_pane(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let handle = cx.entity().clone();

        // Read the store fresh on each render. `open` failure is reported in
        // the pane rather than hidden: it usually means the data directory
        // isn't writable, which the user can act on.
        let (path, entries, load_error) = match KnownHosts::open() {
            Ok(store) => {
                let path = store.path().display().to_string();
                match store.list() {
                    Ok(entries) => (path, entries, None),
                    Err(e) => (path, Vec::new(), Some(e.to_string())),
                }
            }
            Err(e) => (
                KnownHosts::default_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                Vec::new(),
                Some(e.to_string()),
            ),
        };
        let total = entries.len();

        let query = self.known_hosts_query.trim().to_lowercase();
        let rows: Vec<KnownHost> = entries
            .into_iter()
            .filter(|e| matches_query(e, &query))
            .collect();
        let is_empty = rows.is_empty();

        let list_rows: Vec<AnyElement> = rows
            .into_iter()
            .enumerate()
            .map(|(i, entry)| {
                let copy_value = entry.fingerprint.clone();
                let copy_btn = Button::new(ElementId::Name(format!("known-hosts-copy-{i}").into()))
                    .ghost()
                    .icon("icons/copy.svg")
                    .w_auto()
                    .h_7()
                    .px_2()
                    .on_click(move |_e, _w, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_value.clone()));
                    });

                let target = entry.clone();
                let alert = self.alert_controller.clone();
                let handle_for_confirm = handle.clone();
                let forget_btn =
                    Button::new(ElementId::Name(format!("known-hosts-forget-{i}").into()))
                        .ghost()
                        .icon("icons/trash.svg")
                        .w_auto()
                        .h_7()
                        .px_2()
                        .on_click(move |_e, _w, cx| {
                            let target = target.clone();
                            let handle_for_confirm = handle_for_confirm.clone();
                            alert.update(cx, |controller, cx| {
                                let target_label = format!("{}:{}", target.host, target.port);
                                controller.show(
                                    AlertState {
                                        severity: AlertSeverity::Danger,
                                        title: t!("window.settings.known_hosts.delete_title")
                                            .to_string()
                                            .into(),
                                        description: Some(
                                            t!(
                                                "window.settings.known_hosts.delete_desc",
                                                host = target_label.as_str()
                                            )
                                            .to_string()
                                            .into(),
                                        ),
                                        details: vec![
                                            (
                                                t!("window.settings.known_hosts.detail_host")
                                                    .to_string()
                                                    .into(),
                                                target_label.into(),
                                            ),
                                            (
                                                t!("window.settings.known_hosts.detail_type")
                                                    .to_string()
                                                    .into(),
                                                target.algo.clone().into(),
                                            ),
                                            (
                                                t!(
                                                    "window.settings.known_hosts.detail_fingerprint"
                                                )
                                                .to_string()
                                                .into(),
                                                target.fingerprint.clone().into(),
                                            ),
                                        ],
                                        confirm_label: t!(
                                            "window.settings.known_hosts.delete_confirm"
                                        )
                                        .to_string()
                                        .into(),
                                        cancel_label: t!("terminal.host_key_cancel")
                                            .to_string()
                                            .into(),
                                        on_confirm: Some(Rc::new(move |_w, cx| {
                                            let result =
                                                KnownHosts::open().and_then(|s| s.remove(&target));
                                            let error = result.err().map(|e| e.to_string());
                                            handle_for_confirm.update(cx, |view, cx| {
                                                view.known_hosts_error = error;
                                                cx.notify();
                                            });
                                        })),
                                        ..AlertState::default()
                                    },
                                    cx,
                                );
                            });
                        });

                known_host_row(i, &entry, copy_btn, forget_btn).into_any_element()
            })
            .collect();

        // ---- Forget-all button (confirmation alert, then wipe) ----
        let alert = self.alert_controller.clone();
        let handle_for_confirm = handle.clone();
        let clear_btn = Button::new("known-hosts-clear")
            .ghost()
            .icon("icons/trash.svg")
            .w_auto()
            .px_2()
            .child(t!("window.settings.known_hosts.forget_all").to_string())
            .on_click(move |_e, _w, cx| {
                let handle_for_confirm = handle_for_confirm.clone();
                alert.update(cx, |controller, cx| {
                    controller.show(
                        AlertState {
                            severity: AlertSeverity::Danger,
                            title: t!("window.settings.known_hosts.clear_title")
                                .to_string()
                                .into(),
                            description: Some(
                                t!("window.settings.known_hosts.clear_desc")
                                    .to_string()
                                    .into(),
                            ),
                            confirm_label: t!("window.settings.known_hosts.clear_confirm")
                                .to_string()
                                .into(),
                            cancel_label: t!("terminal.host_key_cancel").to_string().into(),
                            on_confirm: Some(Rc::new(move |_w, cx| {
                                let result = KnownHosts::open().and_then(|s| s.clear());
                                let error = result.err().map(|e| e.to_string());
                                handle_for_confirm.update(cx, |view, cx| {
                                    view.known_hosts_error = error;
                                    cx.notify();
                                });
                            })),
                            ..AlertState::default()
                        },
                        cx,
                    );
                });
            });

        let folder = std::path::Path::new(&path)
            .parent()
            .map(|p| p.to_path_buf());
        let reveal_btn = Button::action(
            "known-hosts-open-folder",
            t!("window.settings.known_hosts.open_folder").to_string(),
        )
        .on_click(move |_e, _w, cx| {
            if let Some(folder) = folder.as_ref() {
                let _ = open_path(folder, cx);
            }
        });

        let error = load_error.or_else(|| self.known_hosts_error.clone());

        div()
            .size_full()
            .flex()
            .flex_col()
            .p_6()
            .gap_4()
            .child(
                Section::new()
                    .header(t!("window.settings.known_hosts.section"))
                    .desc(t!("window.settings.known_hosts.desc"))
                    // A bare, full-width row so the path truncates instead of
                    // forcing the pane wider on a narrow window (the same
                    // reason the General pane's data-dir row is bare).
                    .bare(
                        div()
                            .w_full()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(rgb(text_muted()))
                                    .child(t!("window.settings.known_hosts.file").to_string()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(rgb(text_muted()))
                                    .child(path.clone()),
                            )
                            .child(reveal_btn),
                    ),
            )
            .child(
                // Search + count + forget all, on one line.
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div().flex_1().min_w_0().child(
                            StyledInput::new("known-hosts-search", self.known_hosts_search.clone())
                                .xsmall()
                                .prefix(
                                    svg()
                                        .path("icons/search.svg")
                                        .size(px(12.0))
                                        .text_color(rgb(text_muted())),
                                ),
                        ),
                    )
                    .when(total > 0, |el| {
                        el.child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(rgb(text_muted()))
                                .child(
                                    t!("window.settings.known_hosts.count", count = total)
                                        .to_string(),
                                ),
                        )
                    })
                    .when(total > 0, |el| el.child(clear_btn)),
            )
            .when_some(error, |el, error| {
                el.child(div().text_xs().text_color(rgb(term_red())).child(
                    t!("window.settings.known_hosts.error", error = error.as_str()).to_string(),
                ))
            })
            .child(
                div()
                    .id("known-hosts-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .rounded(RADIUS_MD)
                    .border_1()
                    .border_color(rgb(border()))
                    .bg(rgb(bg_base()))
                    .when_else(
                        is_empty,
                        |el| {
                            el.flex()
                                .flex_col()
                                .items_center()
                                .justify_center()
                                .gap_2()
                                .child(
                                    svg()
                                        .path("icons/key.svg")
                                        .size_4()
                                        .text_color(rgb(text_muted()))
                                        .opacity(0.5),
                                )
                                .child(div().text_sm().text_color(rgb(text_muted())).child(
                                    if total == 0 {
                                        t!("window.settings.known_hosts.empty").to_string()
                                    } else {
                                        t!("window.settings.known_hosts.no_matches").to_string()
                                    },
                                ))
                        },
                        |el| el.flex().flex_col().children(list_rows),
                    ),
            )
    }
}

/// Case-insensitive search over everything the row shows. `query` must
/// already be trimmed and lowercased by the caller; an empty query matches
/// every entry.
fn matches_query(entry: &KnownHost, query: &str) -> bool {
    query.is_empty()
        || entry.host.to_lowercase().contains(query)
        || entry.port.to_string().contains(query)
        || entry.algo.to_lowercase().contains(query)
        || entry.fingerprint.to_lowercase().contains(query)
}

/// One fixed-height entry row: key icon, `host:port`, then the copy /
/// forget actions. The key type and fingerprint stay out of the list (they
/// are shown in the forget confirmation) so long fingerprints don't
/// truncate into an unreadable `SHA256:…`.
fn known_host_row(
    index: usize,
    entry: &KnownHost,
    copy_btn: Button,
    forget_btn: Button,
) -> impl IntoElement {
    div()
        .id(ElementId::Name(format!("known-hosts-row-{index}").into()))
        .h(px(ROW_HEIGHT))
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .px_3()
        .when(index > 0, |el| {
            // Extra vertical breathing room between entries; the hairline
            // still marks where one record ends and the next begins.
            el.mt_2().border_t_1().border_color(rgb(border()))
        })
        .child(
            svg()
                .path("icons/key.svg")
                .size_4()
                .flex_shrink_0()
                .text_color(rgb(text_muted())),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .text_color(rgb(text_primary()))
                .child(format!("{}:{}", entry.host, entry.port)),
        )
        .child(copy_btn)
        .child(forget_btn)
}

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*`: this module's parent glob-imports
    // `gpui::*`, which brings in gpui's own `test` attribute macro and makes
    // the built-in `#[test]` expand recursively.
    use super::matches_query;
    use crabport_ssh::known_hosts::KnownHost;

    fn entry(host: &str, port: u16, algo: &str, fingerprint: &str) -> KnownHost {
        KnownHost {
            host: host.into(),
            port,
            algo: algo.into(),
            fingerprint: fingerprint.into(),
        }
    }

    #[test]
    fn search_matches_host_port_algo_and_fingerprint() {
        let e = entry("Example.COM", 2222, "ssh-ed25519", "SHA256:AbC123");

        assert!(matches_query(&e, ""));
        assert!(matches_query(&e, "example"));
        assert!(matches_query(&e, "2222"));
        assert!(matches_query(&e, "ed25519"));
        assert!(matches_query(&e, "abc123"));
        assert!(!matches_query(&e, "example.com:22"));
        assert!(!matches_query(&e, "missing"));
    }
}
