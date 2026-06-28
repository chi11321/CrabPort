#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use crabport_ui::CrabportApp;
use crabport_ui::app::{TerminalShiftTab, TerminalTab};
use crabport_ui::assets::CrabportAssets;
use gpui::*;

fn main() {
    #[cfg(debug_assertions)]
    crabport_core::log::init();

    Application::new()
        .with_assets(CrabportAssets::new())
        .run(|cx| {
            gpui_component::init(cx);

            cx.bind_keys([
                KeyBinding::new("tab", TerminalTab, Some("CrabPortTerminal")),
                KeyBinding::new("shift-tab", TerminalShiftTab, Some("CrabPortTerminal")),
            ]);

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1200.0), px(800.0)), cx)),
                #[cfg(target_os = "macos")]
                titlebar: Some(TitlebarOptions {
                    title: None,
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(12.0), px(14.0))),
                    ..Default::default()
                }),
                window_min_size: Some(Size {
                    width: px(480.0),
                    height: px(340.0),
                }),
                ..Default::default()
            };

            cx.open_window(options, |_window, cx| {
                cx.new(|cx| {
                    let app = cx.new(|cx| CrabportApp::new(_window, cx));
                    app.update(cx, |app, cx| app.wire(cx));
                    gpui_component::Root::new(app, _window, cx)
                })
            })
            .expect("Failed to open window");

            std::mem::forget(cx.on_window_closed(|_cx| {
                std::process::exit(0);
            }));
        });
}
