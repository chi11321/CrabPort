//! "About CrabPort" window.
//!
//! Layout mirrors the main app: a narrow sidebar of tab buttons on the left
//! and a content pane on the right. Two tabs are exposed:
//!
//! - **Version** — application name + version, centered.
//! - **License** — the full Apache-2.0 LICENSE text (embedded at compile
//!   time from `LICENSE` via `include_str!`) followed by a scrollable list
//!   of third-party dependencies, generated at build time from
//!   `Cargo.lock` by `crabport-ui/build.rs` (`OUT_DIR/about_dependencies.rs`).
//!
//! Opened via [`AboutWindow::open`] or the global [`focus_or_open`] helper.

use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::label::Label;
use gpui_component::scroll::Scrollbar;
use gpui_component::{VirtualListScrollHandle, v_virtual_list};
use rust_i18n::t;

use crate::color::*;
use crate::components::window_controls::{HAS_CLIENT_CONTROLS, WindowControls};
use crate::components::window_layout::{
    SidebarTabEntry, render_sidebar_window, render_tab_sidebar,
};

// The project is licensed under Apache 2.0. We show the license name as
// plain text rather than embedding the full LICENSE text (200+ lines) —
// keeps the About window lean and avoids the rendering cost of a long
// text block. Anyone needing the full text can find it in the source tree.
const LICENSE_NAME: &str = "Apache License 2.0";

// Build-time-generated dependency table. See `crabport-ui/build.rs`.
include!(concat!(env!("OUT_DIR"), "/about_dependencies.rs"));

/// Which tab is currently selected in the About window's sidebar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AboutTab {
    Version,
    License,
}

impl AboutTab {
    const ALL: [AboutTab; 2] = [AboutTab::Version, AboutTab::License];

    fn label(self) -> SharedString {
        match self {
            AboutTab::Version => t!("window.about.tab.version").into(),
            AboutTab::License => t!("window.about.tab.license").into(),
        }
    }

    fn sidebar_entries() -> Vec<SidebarTabEntry> {
        Self::ALL
            .iter()
            .enumerate()
            .map(|(i, tab)| SidebarTabEntry {
                id: ElementId::Name(format!("about-tab-{i}").into()),
                label: tab.label(),
                icon: None,
            })
            .collect()
    }
}

/// Root view for the About window.
pub struct AboutWindow {
    /// Cached app version string. Read once at construction time so renders
    /// are pure and don't re-query cargo metadata.
    version: SharedString,
    /// Active sidebar tab. Defaults to `Version` so the window opens on the
    /// most commonly-needed pane.
    tab: AboutTab,
    /// Scroll handle for the dependency list virtual list.
    deps_scroll: VirtualListScrollHandle,
}

impl AboutWindow {
    /// Open the About window (or no-op if one already exists — callers
    /// should normally go through [`crate::windows::focus_or_open`] for the
    /// singleton check).
    pub fn open(cx: &mut App) -> WindowHandle<gpui_component::Root> {
        // Slightly taller than the original single-pane About so the License
        // tab has room for a scrollable text block + dependency list.
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(640.0), px(480.0)), cx)),
            #[cfg(target_os = "macos")]
            titlebar: Some(TitlebarOptions {
                title: Some(t!("window.about.title").to_string().into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(12.0), px(14.0))),
                ..Default::default()
            }),
            #[cfg(target_os = "macos")]
            window_background: WindowBackgroundAppearance::Blurred,
            #[cfg(target_os = "linux")]
            window_decorations: Some(WindowDecorations::Client),
            window_min_size: Some(Size {
                width: px(520.0),
                height: px(360.0),
            }),
            ..Default::default()
        };

        let version = env!("CARGO_PKG_VERSION").into();

        cx.open_window(options, |window, cx| {
            cx.new(|cx| {
                let view = cx.new(|_cx| AboutWindow {
                    version,
                    tab: AboutTab::Version,
                    deps_scroll: VirtualListScrollHandle::new(),
                });
                gpui_component::Root::new(view, window, cx)
            })
        })
        .expect("Failed to open About window")
    }

    fn render_version_pane(&self) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div()
                    .text_2xl()
                    .text_color(rgb(text_primary()))
                    .child("CrabPort"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(text_muted()))
                    .child(format!("v{}", self.version)),
            )
    }

    fn render_license_pane(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // License is shown as a single static line (the license name) —
        // no need to embed the full Apache-2.0 text. The dependency list
        // below uses `v_virtual_list` so only visible rows are laid out.

        // --- Dependency rows ---
        let deps_count = ABOUT_DEPENDENCIES.len();
        let deps_item_sizes = Rc::new(
            (0..deps_count)
                .map(|_| Size {
                    width: px(0.0),
                    height: px(24.0),
                })
                .collect::<Vec<_>>(),
        );
        let deps_scroll = self.deps_scroll.clone();
        let deps_list = v_virtual_list(
            cx.entity(),
            "about-deps-list",
            deps_item_sizes,
            move |_this, range, _window, _cx| {
                range
                    .map(|i| {
                        let (name, ver) = ABOUT_DEPENDENCIES[i];
                        div()
                            .id(ElementId::Name(format!("about-dep-{}", i).into()))
                            .h(px(24.0))
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .px_3()
                            .text_xs()
                            .text_color(rgb(text_primary()))
                            .child(Label::new(name.to_string()))
                            .child(
                                div()
                                    .text_color(rgb(text_muted()))
                                    .child(Label::new(ver.to_string())),
                            )
                    })
                    .collect::<Vec<_>>()
            },
        )
        .track_scroll(&deps_scroll);

        div()
            .size_full()
            .flex()
            .flex_col()
            .p_4()
            .gap_3()
            .overflow_hidden()
            // --- License block ---
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(text_primary()))
                            .child(t!("window.about.license").to_string()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(text_muted()))
                            .child(LICENSE_NAME),
                    ),
            )
            // --- Dependencies block ---
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(text_primary()))
                    .child(t!("window.about.dependencies").to_string()),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .border_1()
                    .border_color(rgb(border()))
                    .bg(rgb(bg_tab_bar()))
                    .rounded_md()
                    .overflow_hidden()
                    .child(deps_list)
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w(px(16.0))
                            .child(
                                Scrollbar::vertical(&deps_scroll)
                                    .scrollbar_show(gpui_component::scroll::ScrollbarShow::Hover),
                            ),
                    ),
            )
    }
}

impl Render for AboutWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let handle = cx.entity().clone();
        let selected_idx = AboutTab::ALL
            .iter()
            .position(|t| *t == self.tab)
            .unwrap_or(0);

        let content: AnyElement = match self.tab {
            AboutTab::Version => self.render_version_pane().into_any_element(),
            AboutTab::License => self.render_license_pane(cx).into_any_element(),
        };

        render_sidebar_window(
            render_tab_sidebar(
                AboutTab::sidebar_entries(),
                px(160.0),
                selected_idx,
                move |idx, _w, cx| {
                    handle.update(cx, |view, _| {
                        view.tab = AboutTab::ALL[idx];
                    });
                },
            ),
            content,
        )
        .when(HAS_CLIENT_CONTROLS, |el| {
            el.child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .h_11()
                    .flex()
                    .items_center()
                    .pr_2()
                    .child(WindowControls::new("about")),
            )
        })
    }
}
