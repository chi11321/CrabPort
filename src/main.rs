#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use crabport_ui::app::{open_main_window, reopen_main_window_if_closed};
use crabport_ui::app_state::AppState;
use crabport_ui::assets::CrabportAssets;
use crabport_ui::keybinds;
use gpui::*;

/// Work around a crash on WSL2.
///
/// WSLg ships a Weston compositor that advertises `xdg_wm_base` at a
/// version below 2. gpui 0.2.2 binds that global with `globals.bind(&qh,
/// 2..=5, ()).unwrap()` (see `platform/linux/wayland/client.rs:151`), so the
/// `UnsupportedVersion` error turns into a panic before any window opens.
///
/// gpui's `guess_compositor` picks Wayland whenever `WAYLAND_DISPLAY` is set
/// and non-empty, with no fallback to X11 on bind failure. Dropping the
/// variable forces the X11 path, which works fine under WSLg.
#[cfg(target_os = "linux")]
fn workaround_wsl2_wayland_version() {
    let is_wsl2 = std::fs::read_to_string("/proc/version")
        .is_ok_and(|v| v.contains("microsoft-standard-WSL2"));
    if is_wsl2 {
        unsafe {
            std::env::remove_var("WAYLAND_DISPLAY");
        }
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    workaround_wsl2_wayland_version();

    // Initialize process-wide tracing. Runs in BOTH debug and release builds
    // (no `debug_assertions` gate) so field-reported issues leave a trail in
    // `{data_dir}/crabport/latest.log`. Debug builds ALSO log to stderr.
    crabport_core::log::init();

    // Acquire the process-wide single-instance lock BEFORE constructing the
    // `Application` (which would claim shared resources like the SQLite store
    // again). On failure, this calls `process::exit(0)` — so the line below
    // only runs when this process is the sole instance. The guard `File` is
    // held for the entire process lifetime (until `main` returns / the OS
    // reaps the process); `fs2` releases the lock on drop, so even an abrupt
    // `terminate:` / SIGKILL frees it via the OS.
    #[cfg(not(debug_assertions))]
    let _single_instance_guard = AppState::acquire_single_instance_lock();

    let app = Application::new().with_assets(CrabportAssets::new());
    // macOS: re-open the main window when the user clicks the Dock icon
    // while no windows are visible. Maps to the AppKit delegate method
    // `applicationShouldHandleReopen:hasVisibleWindows:` — GPUI only
    // invokes this callback when `hasVisibleWindows == NO`, so the
    // `reopen_main_window_if_closed` path is safe (no risk of stacking
    // a second `CrabportApp` on top of the existing window).
    //
    // On non-macOS this is a silent no-op (GPUI's `on_reopen` is a
    // trait method on every platform but only Cocoa dispatches it), so
    // the Windows / Linux launch flow is unchanged.
    app.on_reopen(|cx| {
        // Logged to stderr via `eprintln` because the binary crate doesn't
        // directly depend on `tracing` (the subscriber is initialized
        // in `crabport_core::log::init` above, but the macro itself needs
        // the `tracing` crate to be a direct dep). Either way, this path
        // only fires on macOS Dock clicks with no visible windows.
        eprintln!("crabport: reopen requested — re-opening main window");
        reopen_main_window_if_closed(cx);
    });
    // macOS: receive `application:openURLs:` events. This is the "app
    // already running" half of the "Open in CrabPort" entry point —
    // when the user invokes a Service / drag-drop on a running app,
    // LaunchServices routes the URLs here instead of spawning a new
    // process. We resolve each URL to a folder path and open a local-
    // terminal tab cd'd into it.
    //
    // On Windows/Linux this is also wired (GPUI has implementations for
    // all three platforms), but the single-instance lock means a second
    // launch `exit(0)`s before the URLs arrive — so there it only fires
    // if the app is the sole instance, same as the argv path. The macOS
    // case is the primary target because Finder's "Open in CrabPort"
    // Service delivers to a running app via this callback.
    //
    // GPUI's `on_open_urls` callback signature is `FnMut(Vec<String>)`
    // with no `&mut App` — unlike `on_reopen`. To still reach the app
    // state from the callback we hand it an `AsyncApp` via a shared
    // slot that `app.run` fills in once the app is constructed (this
    // is the same pattern Zed uses for its URL handler). URLs only
    // arrive after `applicationDidFinishLaunching:`, so the slot is
    // always populated by the time the callback fires.
    let async_app_slot = std::rc::Rc::new(std::cell::RefCell::new(None::<gpui::AsyncApp>));
    let slot_for_cb = async_app_slot.clone();
    app.on_open_urls(move |urls| {
        let Some(async_app) = slot_for_cb.borrow().clone() else {
            eprintln!("crabport: open-urls arrived before app launched, ignoring: {urls:?}");
            return;
        };
        // `handle_open_urls` needs `&mut App`. `AsyncApp::update` gives
        // us that on the main thread; errors only happen if the app
        // was dropped (process tear-down), in which case we log + drop.
        let _ = async_app.update(|cx| {
            crabport_ui::app::handle_open_urls(urls, cx);
        });
    });
    app.run(move |cx| {
        // Fill the AsyncApp slot that `on_open_urls` uses to reach the app
        // state. `cx.to_async()` returns an `AsyncApp` holding a `Weak<…>`
        // — cheap to clone, safe to keep across the launch boundary.
        let async_app = cx.to_async();
        *async_app_slot.borrow_mut() = Some(async_app.clone());

        // macOS Services: register our `openInCrabPort:userData:error:`
        // selector on GPUI's app delegate class so CrabPort appears in
        // the system Services menu (Finder → right-click folder →
        // Services → "Open Folder in CrabPort"). Must run before the
        // run loop starts dispatching events, and on the main thread —
        // both are satisfied here. The `set_async_app` call hands the
        // selector the same `AsyncApp` we just stored in the local slot,
        // so Services and `on_open_urls` share one entry point.
        #[cfg(target_os = "macos")]
        {
            crabport_ui::macos_services::set_async_app(async_app);
            if let Err(e) = crabport_ui::macos_services::register_services_handler() {
                eprintln!("crabport: failed to register macOS Service handler: {e}");
            }
        }
        // Register all keybinds from the catalog (reads config.toml
        // overrides, falls back to platform defaults).
        //
        // This MUST run before `gpui_component::init` because
        // `apply_bindings` calls `cx.clear_key_bindings()`, which
        // wipes the entire keymap — including any bindings that
        // gpui-component registered earlier (notably the `Input`
        // widget's `backspace` / `delete` / `enter` / `escape`
        // bindings under the `Input` context). Registering our
        // bindings first and then calling `gpui_component::init`
        // leaves the input bindings intact, so text fields work
        // correctly (without this, Backspace does nothing in
        // StyledInput on Windows / Linux).
        keybinds::apply_bindings(cx);

        gpui_component::init(cx);

        // Force dark theme regardless of system appearance.
        gpui_component::theme::Theme::change(gpui_component::theme::ThemeMode::Dark, None, cx);

        // macOS: enable sidebar vibrancy (毛玻璃). Patches the gpui-component
        // theme background to transparent so `gpui_component::Root` doesn't
        // paint an opaque layer over the system vibrancy view. Main / Settings
        // / About windows are opened with `WindowBackgroundAppearance::Blurred`.
        #[cfg(target_os = "macos")]
        crabport_ui::color::enable_vibrancy(cx);

        // Set the active locale early so the menu bar (built below) and
        // every window picks up the right translations. Read from the
        // persisted config.toml so the user's language choice survives
        // app restarts.
        let locale = crabport_core::config::snapshot().appearance.locale;
        crabport_ui::set_locale(&locale);

        // Apply the persisted animation speed tier so transitions start at
        // the user's chosen multiplier from the very first frame (no
        // first-frame flash at 1.0× before Settings opens). The multiplier
        // lives in `motion.rs` as an `AtomicU32`; Settings updates it live
        // via the same call.
        let speed = crabport_core::config::snapshot().appearance.animation_speed;
        crabport_ui::motion::set_speed_multiplier(speed.multiplier());

        // Initialize process-wide shared state (store, window registry)
        // before opening any window. `CrabportApp::new` reads from this
        // global, so it must be ready first.
        AppState::init(cx);

        // Global actions for opening secondary windows. These are app-
        // level (no window context required) so they work from any
        // focused window.
        cx.on_action::<crabport_ui::menus::OpenSettings>(|_a, cx| {
            AppState::focus_or_open(crabport_ui::windows::AuxWindowKind::Settings, cx);
        });
        cx.on_action::<crabport_ui::menus::OpenAbout>(|_a, cx| {
            AppState::focus_or_open(crabport_ui::windows::AuxWindowKind::About, cx);
        });

        // Menu-bar actions backed by App-level platform calls.
        cx.on_action::<crabport_ui::menus::Quit>(|_a, cx| cx.quit());
        cx.on_action::<crabport_ui::menus::Hide>(|_a, cx| cx.hide());

        // Window menu: act on the currently-focused window. Menu actions
        // dispatch globally, so we resolve the active window handle here
        // and run the platform call inside its window context.
        cx.on_action::<crabport_ui::menus::Minimize>(|_a, cx| {
            if let Some(handle) = cx.active_window() {
                let _ = handle.update(cx, |_, window, _cx| window.minimize_window());
            }
        });
        cx.on_action::<crabport_ui::menus::Zoom>(|_a, cx| {
            if let Some(handle) = cx.active_window() {
                let _ = handle.update(cx, |_, window, _cx| window.zoom_window());
            }
        });

        // Install the macOS menu bar. On non-macOS platforms this is a
        // no-op / ignored, but the call is harmless.
        cx.set_menus(crabport_ui::menus::app_menus());

        // Inspect argv for a path argument. This is the first-launch half
        // of the "Open in CrabPort" macOS entry point: when the user
        // invokes the Service / Automator action (or runs
        // `crabport /some/folder` from the shell) the OS spawns the
        // binary with the path as argv[1]. We resolve it to a folder
        // (files → parent dir) so the very first thing the user sees is
        // a local-terminal tab cd'd into the right place.
        //
        // The "app already running" half is handled by `on_open_urls`
        // above — macOS LaunchServices routes those URLs to the existing
        // instance instead of spawning a new process, so the two paths
        // are mutually exclusive: argv only fires on first launch,
        // `on_open_urls` only fires when already running.
        let initial_path = std::env::args().nth(1).and_then(|a| {
            // Accept both bare paths and `file://` / `crabport://` URLs.
            // If the arg looks like a URL, route it through the same
            // resolver `on_open_urls` uses; otherwise treat it as a
            // raw filesystem path.
            if a.starts_with("file://") || a.starts_with("crabport://") {
                match crabport_ui::app::resolve_url_to_folder_pub(&a) {
                    Ok(p) => Some(p),
                    Err(reason) => {
                        eprintln!("crabport: ignoring argv URL {:?}: {}", a, reason);
                        None
                    }
                }
            } else if a.starts_with('/') || a.starts_with("./") || a.starts_with("../") {
                Some(std::path::PathBuf::from(a))
            } else {
                // Relative-looking arg without prefix — resolve
                // against the process cwd so `crabport sub/dir`
                // works from the shell.
                let abs = std::env::current_dir()
                    .ok()
                    .map(|d| d.join(&a))
                    .filter(|p| p.exists());
                abs
            }
        });
        if initial_path.is_some() {
            crabport_ui::app::open_main_window_with_path(initial_path, cx);
        } else {
            open_main_window(cx);
        }
    });
}
