use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_animation::animation::TransitionExt;

use crate::app::{CrabportApp, Tab, TabKind};
use crate::color::*;
use crate::components::button::Button;
use crate::components::window_controls::{HAS_CLIENT_CONTROLS, WindowControls};
use crate::motion::{DURATION_BASE, EASE_LINEAR};

pub fn render_tab_bar(
    handle: &Entity<CrabportApp>,
    tabs: &[Tab],
    active_tab_id: u64,
    is_home: bool,
    on_close: Rc<dyn Fn(u64, &mut Window, &mut App) + 'static>,
) -> impl IntoElement {
    let h = handle.clone();

    // On macOS the transparent titlebar reveals the native traffic-light
    // buttons at the top-left; we reserve left padding so the tabs don't sit
    // under them. The reserved space animates between a narrow (home view) and
    // wide (terminal view) value so tabs slide smoothly when switching contexts.
    // On Windows/Linux there are no native traffic lights overlapping the tab
    // bar, so no left padding reservation is needed — tabs start flush at the
    // left edge, and the terminal view needs no x-axis space reservation.
    let pad_narrow = if cfg!(target_os = "macos") {
        px(4.)
    } else {
        px(0.)
    };
    let pad_wide = if cfg!(target_os = "macos") {
        px(80.)
    } else {
        px(0.)
    };

    // Width reserved on the right edge for the `+` button (and, on non-macOS,
    // the window controls). The scrollable tabs region insets its right side
    // by this amount so its content never slides under the pinned controls.
    let right_reserve = if HAS_CLIENT_CONTROLS {
        // 3 control buttons (w_9 = 36px each) + gaps + right padding
        px(36.0 * 3.0 + 4.0 * 3.0 + 8.0 + 36.0 + 4.0)
    } else {
        // + button (w_9 = 36px) + gap + right padding
        px(36.0 + 4.0 + 8.0)
    };

    div()
        .id("tabbar")
        .w_full()
        .h_11()
        .bg(rgb(bg_tab_bar()))
        .border_b_1()
        .border_color(rgb(border()))
        .relative()
        .on_mouse_down(MouseButton::Left, move |_: &MouseDownEvent, window, _cx| {
            // Cross-platform: on Linux this fires `_NET_WM_MOVERESIZE` /
            // `xdg_toplevel._move`; on macOS/Windows it's a no-op (those
            // platforms handle drag natively — see the doc comment in
            // `window_controls::start_window_move`).
            crate::components::window_controls::start_window_move(window);
        })
        .on_mouse_up(MouseButton::Left, |event: &MouseUpEvent, window, _| {
            if event.click_count == 2 {
                // Cross-platform: macOS delegates to `titlebar_double_click`
                // (respects `AppleActionOnDoubleClick`), Windows/Linux use
                // our explicit maximize/restore toggle. See the doc comment
                // in `window_controls::toggle_maximize`.
                crate::components::window_controls::toggle_maximize(window);
            }
        })
        // Register the whole tab bar as a window-drag region on
        // Windows/Linux so the user can drag the window by grabbing any
        // empty area of the bar (left gutter, gaps between tabs, gap
        // before the `+` button, gap before the window controls) —
        // mirroring how a native title bar behaves. Buttons (tabs,
        // `+`, window controls) call [`InteractiveElement::occlude_mouse`]
        // so their hitboxes block this drag-region hitbox beneath them,
        // keeping their clicks working under Windows' `WM_NCHITTEST`.
        //
        // macOS is excluded because `window_control_area` is a no-op
        // there (GPUI's `on_hit_test_window_control` is empty on
        // macOS) — macOS already provides drag via the transparent
        // system title bar + `traffic_light_position`.
        //
        // `occlude_mouse` is what makes the drag region *itself* a
        // `BlockMouse` hitbox. This stops GPUI's `hit_test` from also
        // collecting ancestor hitboxes (notably the `app-root` div with
        // `.track_focus(...)`). Without it, that ancestor's hitbox would
        // be in `mouse_hit_test.ids`, its `track_focus` mouse-down handler
        // would call `window.prevent_default()`, and gpui's
        // `WM_NCLBUTTONDOWN` handler would see `default_prevented = true`,
        // return `Some(0)`, and cancel the native `HTCAPTION` drag (and
        // the double-click maximize). Drag-region buttons painted above
        // this bar still call `occlude_mouse` themselves, so they win the
        // hit-test via the reverse iteration and their clicks fire as
        // before.
        .when(cfg!(not(target_os = "macos")), |el| {
            el.window_control_area(WindowControlArea::Drag).occlude()
        })
        // --- Scrollable tabs layer -----------------------------------------
        // Occupies the full tab-bar width but is inset on the right by
        // `right_reserve` so its scrollable content never slides under the
        // pinned `+` / window-control buttons. On macOS it is also inset on
        // the left (animated) to clear the traffic lights.
        //
        // `overflow_x_scroll` enables horizontal scrolling; GPUI's default
        // `restrict_scroll_to_axis: false` means a vertical mouse-wheel delta
        // is automatically translated into horizontal scroll when only the x
        // axis is scrollable, so the user gets horizontal scrolling without
        // holding Shift. `overflow_y_hidden` clips any vertical overflow
        // (e.g. tab close-button hit areas) without enabling y-axis scroll.
        .child(
            div()
                .id("tabbar-scroll")
                .absolute()
                .top_0()
                .bottom_0()
                .right(right_reserve)
                .flex()
                .items_center()
                .gap_1()
                .py_1()
                .pl_1()
                .pr_2()
                .overflow_x_scroll()
                .overflow_y_hidden()
                // Animate the left inset (not padding) between a narrow
                // (home view) and wide (terminal view) value so tabs clear the
                // macOS traffic lights. Using `left` (inset) rather than `pl`
                // (padding) keeps the reserved space *outside* the scroll
                // viewport, so scrolling the tabs to the right never reveals
                // content bleeding through the left gutter. On non-macOS
                // both values are 0, so no left inset is reserved.
                .with_transition("tabbar-scroll")
                .transition_when_else(
                    is_home,
                    DURATION_BASE,
                    EASE_LINEAR,
                    move |el| el.left(pad_narrow),
                    move |el| el.left(pad_wide),
                )
                .children(tabs.iter().map(|tab| {
                    let is_active = tab.id == active_tab_id;
                    let is_home_tab = tab.kind == TabKind::Home;
                    let h2 = handle.clone();
                    let tab_id = tab.id;
                    let wrapper_id = ElementId::Name(format!("tab-wrapper-{}", tab.id).into());
                    let on_close = on_close.clone();
                    div()
                        .id(wrapper_id.clone())
                        .flex()
                        .items_center()
                        .flex_shrink_0()
                        .with_transition(wrapper_id)
                        .transition_when_else(
                            is_active,
                            DURATION_BASE,
                            EASE_LINEAR,
                            |el| el.w_48(),
                            |el| el.w_24(),
                        )
                        .child({
                            let mut btn =
                                Button::new(ElementId::Name(format!("tab-{}", tab.id).into()))
                                    .tab()
                                    .selected(is_active)
                                    .child(tab.title.clone())
                                    .h_9()
                                    .w_full()
                                    .border_0()
                                    .px_3()
                                    .text_sm()
                                    // Block the tab-bar drag region beneath
                                    // the tab so clicking the tab activates it
                                    // instead of starting a window drag on
                                    // Windows/Linux.
                                    .occlude_mouse()
                                    .on_click(move |_e, _w, cx| {
                                        h2.update(cx, |app, _| {
                                            app.activate_tab(tab_id);
                                        });
                                    });
                            if !is_home_tab && tab.kind != TabKind::Sftp {
                                let tab_id = tab.id;
                                let on_close = on_close.clone();
                                btn = btn.on_close(move |w, cx| {
                                    on_close(tab_id, w, cx);
                                });
                            }
                            btn
                        })
                })),
        )
        // --- Pinned right layer --------------------------------------------
        // `+` button and (on Windows/Linux) window controls, absolutely
        // positioned at the right edge on a higher paint layer than the
        // scrollable tabs, so they are always visible regardless of scroll
        // position.
        .child(
            div()
                .absolute()
                .top_0()
                .right_0()
                .bottom_0()
                .flex()
                .items_center()
                .gap_1()
                .pr_1()
                .py_1()
                .bg(rgb(bg_tab_bar()))
                .child(
                    Button::new("tab-add")
                        .tab()
                        .centered(true)
                        .child(
                            svg()
                                .path("icons/plus.svg")
                                .size_4()
                                .text_color(rgb(text_muted())),
                        )
                        .h_9()
                        .w_9()
                        .border_0()
                        .px_0()
                        .text_sm()
                        // Block the tab-bar drag region beneath the `+`
                        // button so clicking it opens the command palette
                        // instead of starting a window drag on Windows/Linux.
                        .occlude_mouse()
                        .on_click(move |_e, w, cx| {
                            h.update(cx, |app, cx| {
                                let cmd = app.app_ctx.command_palette.clone();
                                cmd.update(cx, |cmd, cx| {
                                    cmd.open(w, cx);
                                });
                                cx.notify();
                            });
                        }),
                )
                .when(HAS_CLIENT_CONTROLS, |el| {
                    el.child(WindowControls::new("main").with_aux_buttons(true))
                }),
        )
}
