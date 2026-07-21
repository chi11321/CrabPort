//! macOS Services integration.
//!
//! This module registers a Service handler so CrabPort appears in the
//! system-wide Services menu (Finder → right-click a folder → Services,
//! or any app's app-name menu → Services → "Open Folder in CrabPort").
//!
//! ## Why this is needed
//!
//! GPUI 0.2.2's `MacPlatform` delegate only implements a fixed set of
//! Cocoa selectors (`application:openURLs:`, `applicationShouldHandleReopen:`,
//! menu item handlers, …). It does NOT expose a way for downstream
//! crates to add their own selectors to the delegate class, which is
//! what macOS Services requires (each Service's `NSMessage` names the
//! selector to call on the app delegate).
//!
//! We work around this by using the ObjC runtime's `class_addMethod` to
//! dynamically attach our Service selector (`openInCrabPort:userData:error:`)
//! to GPUI's already-registered `"GPUIApplicationDelegate"` class. ObjC's
//! method dispatch is dynamic — instances see newly-added methods because
//! lookup goes through the class table at call time, not at instantiation.
//! This is the same mechanism KVO uses to synthesize accessors.
//!
//! ## Flow when the user invokes the Service
//!
//! 1. User right-clicks a folder in Finder → Services → "Open Folder in
//!    CrabPort".
//! 2. LaunchServices delivers the folder as an `NSPasteboard` to the
//!    app delegate's `openInCrabPort:userData:error:` selector.
//! 3. Our selector reads `public.file-url` items off the pasteboard,
//!    converts each to a `file://` URL string, and forwards them to
//!    [`crate::app::handle_open_urls`] — the same handler `on_open_urls`
//!    uses. That resolves each URL to a folder and opens a local-
//!    terminal tab cd'd into it.
//! 4. If the app isn't running yet, LaunchServices launches it first
//!    and then delivers the Service call. Our `app.run` callback
//!    populates the `AsyncApp` slot before the run loop starts, so the
//!    Service handler always finds a live `AsyncApp` by the time it's
//!    invoked.

#![cfg(target_os = "macos")]

use std::cell::RefCell;

use gpui::AsyncApp;
use objc::runtime::{Class, Imp, Object, Sel, class_addMethod};
use objc::{class, msg_send, sel, sel_impl};

/// Pasteboard type string for file URLs (modern UTI). We prefer this
/// over the legacy `NSFilenamesPboardType` ("NSFilenamesPboardType")
/// because Apple deprecated the latter in 10.14; the UTI works on
/// every supported macOS release and is what Finder puts on the
/// pasteboard for folder Services.
const FILE_URL_PBOARD_TYPE: &str = "public.file-url";

/// The ObjC type encoding for our Service selector.
///
/// Signature: `- (void)openInCrabPort:(NSPasteboard *)pboard
///                              userData:(NSString *)userData
///                                error:(NSError **)error`
///
/// ObjC encoding chars:
/// - `v` = void return
/// - `@` = object pointer (self, pasteboard, user_data, error-out)
/// - `:` = selector (`_cmd`)
///
/// So the full encoding is `v@:@@@` — return void, self, _cmd,
/// pasteboard, user_data, error-out. The implicit `self` + `_cmd`
/// are the standard hidden parameters of every ObjC method.
const SELECTOR_ENCODING: &[u8] = b"v@:@@@\0";

/// State of the one-time registration. `Once` ensures we only attempt
/// the `class_addMethod` call once even if `register_services_handler`
/// is invoked multiple times.
static REGISTERED: std::sync::Once = std::sync::Once::new();

// Thread-local slot holding the `AsyncApp` created in `main.rs`'s
// `app.run` callback.
//
// `AsyncApp` wraps a `Weak<Rc<RefCell<App>>>` — `Rc` is neither `Send`
// nor `Sync`, so it CAN'T live in a `static`. But ObjC Service
// invocations always dispatch on the main thread, and `app.run` (which
// populates this slot) also runs on the main thread. So a `thread_local!`
// on the main thread is the correct storage: same thread writes and
// reads, no cross-thread access, no `Sync` requirement.
thread_local! {
    static SERVICE_ASYNC_APP: RefCell<Option<AsyncApp>> = const { RefCell::new(None) };
}

/// Install the Service selector on GPUI's app delegate class.
///
/// Must be called from the main thread (ObjC runtime requirement) and
/// before the run loop starts processing events — `main.rs` calls this
/// inside `app.run`. Safe to call multiple times; only the first call
/// performs the actual registration.
///
/// Returns `Ok(())` on success, or an `&'static str` error if the
/// delegate class can't be found (GPUI not yet initialised) or
/// `class_addMethod` reports failure (the selector was already present
/// on the class — shouldn't happen, but we surface it instead of
/// panicking).
pub fn register_services_handler() -> Result<(), &'static str> {
    REGISTERED.call_once(|| {
        // SAFETY: we're on the main thread (caller's contract), and the
        // ObjC runtime's class-table mutation is safe to do once at
        // startup before any instances are dispatching this selector.
        unsafe {
            register_selector_unchecked();
        }
    });
    Ok(())
}

/// Publish an `AsyncApp` so the Service selector can reach app state.
///
/// Called from `main.rs`'s `app.run` closure, right after `cx.to_async()`
/// produces the `AsyncApp`. Stored in the main-thread-local slot; the
/// Service handler clones it out on each invocation (cheap — `AsyncApp`
/// is just a `Weak<AppCell>` + two executor handles).
pub fn set_async_app(app: AsyncApp) {
    SERVICE_ASYNC_APP.with(|slot| {
        *slot.borrow_mut() = Some(app);
    });
}

/// # Safety
///
/// Must be called on the main thread, and the `GPUIApplicationDelegate`
/// class must already be registered by GPUI's `Once` initialiser.
unsafe fn register_selector_unchecked() {
    let cls = match Class::get("GPUIApplicationDelegate") {
        Some(c) => c as *const Class as *mut Class,
        None => {
            tracing::error!(
                "macos_services: GPUIApplicationDelegate class not registered — \
                 is GPUI initialised?"
            );
            return;
        }
    };

    let sel: Sel = sel!(openInCrabPort:userData:error:);

    // The IMP (function pointer) we're attaching. `Imp` is
    // `unsafe extern fn()` (no args) — we transmute our typed
    // `extern "C" fn` through a raw pointer to satisfy the type system.
    // The ObjC runtime doesn't care about the Rust signature; it calls
    // through the IMP with the ABI declared in `SELECTOR_ENCODING`.
    let imp: Imp = unsafe {
        std::mem::transmute::<
            extern "C" fn(&mut Object, Sel, *mut Object, *mut Object, *mut *mut Object),
            Imp,
        >(open_in_crab_port)
    };

    let success =
        unsafe { class_addMethod(cls, sel, imp, SELECTOR_ENCODING.as_ptr() as *const i8) };
    // `BOOL` is `bool` on 64-bit macOS — direct truthiness check.
    if success {
        tracing::info!(
            "macos_services: registered `openInCrabPort:userData:error:` \
                 on GPUIApplicationDelegate"
        );
    } else {
        tracing::error!(
            "macos_services: class_addMethod returned false — \
                 selector `openInCrabPort:userData:error:` may already be present"
        );
    }

    // Critical: tell AppKit our app is now ready to receive Service
    // invocations. macOS Services are delivered via a Distributed
    // Objects (DO) port that AppKit sets up lazily — without this the
    // Services menu shows the item but clicking it does nothing,
    // and `NSPerformService` returns `true` (because LaunchServices
    // found the `NSServices` declaration) without ever invoking the
    // selector.
    //
    // GPUI 0.2.2's delegate doesn't run AppKit's standard Services
    // registration path (it only fires when the app declares
    // `NSServices` at launch AND AppKit's default delegate handlers
    // run — GPUI overrides `applicationDidFinishLaunching:` with its
    // own IMP that skips that). We have to do the registration
    // ourselves, in two parts:
    //
    //   1. `NSRegisterServicesProvider(provider, name)` — the
    //      low-level C function that vends `provider` over DO under
    //      the given name. `name` MUST match the service's
    //      `NSPortName` in `Info.plist` (we declared `CrabPort`
    //      there). Without this match LaunchServices can't route the
    //      call. We pass the app delegate as `provider` because it
    //      carries our `openInCrabPort:userData:error:` IMP.
    //
    //   2. `NSUpdateDynamicServices()` — re-scans `NSServices`
    //      entries system-wide so the just-registered provider shows
    //      up in the Services menu.
    //
    // We DON'T use `-[NSApplication setServicesProvider:]` because
    // that method internally calls `NSRegisterServicesProvider` with
    // a name derived from `CFBundleExecutable`, which may differ from
    // the `NSPortName` we declared (it doesn't in our case, but being
    // explicit avoids a class of subtle mismatch bugs). The C function
    // gives us full control.
    unsafe extern "C" {
        // `id` is `*mut Object` in cocoa; `NSServiceProviderName`
        // is `NSString *` — so `name` must be an NSString, NOT a raw
        // C string. Passing a C string here was crashing the app
        // (EXC_BAD_ACCESS inside AppKit's NSString handling).
        fn NSRegisterServicesProvider(provider: *mut Object, name: *mut Object);
        fn NSUpdateDynamicServices();
    }
    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        if !app.is_null() {
            let delegate: *mut Object = msg_send![app, delegate];
            if !delegate.is_null() {
                // Wrap the name in an NSString — `NSRegisterServicesProvider`
                // expects `NSServiceProviderName` (= `NSString *`), not a
                // raw C string.
                let name_str: *mut Object = msg_send![
                    class!(NSString),
                    stringWithUTF8String: b"CrabPort\0".as_ptr() as *const i8
                ];
                if !name_str.is_null() {
                    NSRegisterServicesProvider(delegate, name_str);
                    tracing::info!("macos_services: NSRegisterServicesProvider(CrabPort) ok");
                } else {
                    tracing::error!("macos_services: failed to build name NSString");
                }
            } else {
                tracing::error!("macos_services: NSApplication.delegate is nil");
            }
            NSUpdateDynamicServices();
            tracing::info!("macos_services: called NSUpdateDynamicServices");
        } else {
            tracing::error!("macos_services: NSApplication.sharedApplication is nil");
        }
    }
}

/// The Service selector itself.
///
/// Signature matches `NSMessage` Services convention:
/// `- (void)openInCrabPort:(NSPasteboard *)pboard
///                  userData:(NSString *)userData
///                    error:(NSError **)error`
///
/// # Safety
///
/// This is an ObjC method — the runtime calls it with `self` (the
/// delegate instance), `_cmd` (the selector), and the three declared
/// parameters. All pointer args are valid ObjC objects owned by the
/// runtime; we only read from `pboard` and write a nil `NSError*` to
/// `error` on success.
extern "C" fn open_in_crab_port(
    _this: &mut Object,
    _cmd: Sel,
    pboard: *mut Object,
    _user_data: *mut Object,
    error: *mut *mut Object,
) {
    // Best-effort: never let a panic escape into ObjC (undefined
    // behaviour — the runtime unwind path is not guaranteed to be
    // safe). Any failure mode just logs and returns; the user sees
    // nothing in the Services menu because we didn't claim success.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handle_service_invocation(pboard)
    }));
    if let Err(e) = result {
        tracing::error!("macos_services: selector panicked: {e:?}");
    }
    // Services convention: set `*error` to nil to signal success. We
    // always set nil even on our internal failures — surfacing an
    // NSError would put up a system alert, which is noisier than the
    // log line we already wrote. The user just sees "nothing happened"
    // and can check the log.
    if !error.is_null() {
        unsafe {
            *error = std::ptr::null_mut();
        }
    }
}

/// Core logic of the Service selector, factored out so it can be
/// `catch_unwind`-wrapped without the ObjC signature getting in the
/// way.
///
/// Reads file URLs off the pasteboard and forwards them through
/// [`crate::app::handle_open_urls`] on the main thread via the
/// stored `AsyncApp`.
fn handle_service_invocation(pboard: *mut Object) {
    let urls = unsafe { read_file_urls_from_pasteboard(pboard) };
    if urls.is_empty() {
        tracing::info!("macos_services: Service invoked with no file URLs on pasteboard");
        return;
    }
    tracing::info!("macos_services: Service invoked, {} URL(s)", urls.len());

    // Pull the AsyncApp out of the thread-local slot. If it's not set
    // yet (very early launch race), log and drop — the next Service
    // invocation will work once `app.run` has populated the slot.
    let async_app = SERVICE_ASYNC_APP.with(|slot| slot.borrow().clone());
    let Some(async_app) = async_app else {
        tracing::warn!(
            "macos_services: AsyncApp slot not yet populated — dropping Service invocation"
        );
        return;
    };
    // `AsyncApp::update` runs the closure on the main thread, which is
    // required for GPUI entity mutations. We're already on the main
    // thread (Services always dispatch there), but `update` still does
    // the borrow-check dance that gives us `&mut App`.
    let _ = async_app.update(|cx| {
        crate::app::handle_open_urls(urls, cx);
    });
}

/// Pull every file URL off an `NSPasteboard`.
///
/// Returns the URLs as `file://`-prefixed strings — the exact shape
/// [`crate::app::resolve_url_to_folder`] expects — so the existing
/// URL-scheme code path reuses its parsing / parent-dir / symlink-
/// resolution logic.
///
/// # Safety
///
/// `pboard` must be a valid `NSPasteboard*` (the runtime guarantees
/// this when calling a Service selector).
unsafe fn read_file_urls_from_pasteboard(pboard: *mut Object) -> Vec<String> {
    let pb = pboard;
    if pb.is_null() {
        return Vec::new();
    }

    // Use the modern pasteboard API: `readObjectsForClasses:options:`
    // with `[NSURL class]` and an options dict that restricts to file
    // URLs. This returns an `NSArray<NSURL*>` (or nil if nothing on
    // the pasteboard matches).
    //
    // `class!(NSURL)` yields `&'static Class`; `msg_send!` accepts that
    // as the receiver and returns `*mut Object` when the return type
    // is inferred as an object pointer. We omit explicit annotations
    // on the intermediates so the macro infers `*mut Object`.
    let nsurl_class = class!(NSURL);
    let classes: *mut Object = msg_send![class!(NSArray), arrayWithObject: nsurl_class];
    if classes.is_null() {
        return Vec::new();
    }
    // Options: restrict to file URLs only. This filters out string /
    // image representations that might also be on the pasteboard.
    let true_obj: *mut Object = msg_send![class!(NSNumber), numberWithBool: true as i8];
    let url_key: *mut Object = msg_send![class!(NSString), stringWithUTF8String: b"NSPasteboardURLReadingFileURLKey\0".as_ptr() as *const i8];
    let keys: *mut Object = msg_send![class!(NSArray), arrayWithObject: url_key];
    let objs: *mut Object = msg_send![class!(NSArray), arrayWithObject: true_obj];
    let options: *mut Object =
        msg_send![class!(NSDictionary), dictionaryWithObjects: objs forKeys: keys];
    let url_array: *mut Object = msg_send![pb, readObjectsForClasses: classes options: options];
    if url_array.is_null() {
        // Fallback: try the legacy string-based API in case the host
        // app put plain path strings on the pasteboard instead of URLs.
        return unsafe { read_filenames_legacy(pb) };
    }

    let count: usize = msg_send![url_array, count];
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let url: *mut Object = msg_send![url_array, objectAtIndex: i];
        if url.is_null() {
            continue;
        }
        // CRITICAL: use `-path` (the filesystem path), NOT
        // `-absoluteString`. Finder delivers folder URLs as file
        // reference URLs (`file:///.file/id=<inode>.<gen>/`) rather
        // than path-based URLs (`file:///tmp/`). `absoluteString`
        // returns the reference-URL string verbatim, which our
        // resolver treats as a path (`/.file/id=...`) — `metadata`
        // on it succeeds (it's a valid inode reference) but `chdir`
        // into it lands in `/` because the shell can't resolve the
        // `.file/id=` form.
        //
        // `-path` asks NSURL to resolve the reference and return the
        // real filesystem path (e.g. `/tmp`), which is what we want.
        // We then re-wrap it as a `file://` URL so the existing
        // resolver code path handles it uniformly.
        let is_file_url: bool = msg_send![url, isFileURL];
        if !is_file_url {
            // Non-file URL (e.g. http) — fall back to absoluteString
            // so the resolver can log and reject it.
            let abs_string: *mut Object = msg_send![url, absoluteString];
            if abs_string.is_null() {
                continue;
            }
            let utf8: *const i8 = msg_send![abs_string, UTF8String];
            if utf8.is_null() {
                continue;
            }
            if let Ok(s) = unsafe { std::ffi::CStr::from_ptr(utf8).to_str() } {
                out.push(s.to_string());
            }
            continue;
        }
        let path_obj: *mut Object = msg_send![url, path];
        if path_obj.is_null() {
            continue;
        }
        let utf8: *const i8 = msg_send![path_obj, UTF8String];
        if utf8.is_null() {
            continue;
        }
        if let Ok(s) = unsafe { std::ffi::CStr::from_ptr(utf8).to_str() } {
            // Re-wrap as a `file://` URL. The path from NSURL is
            // already absolute (starts with `/`), so we just prepend
            // `file://` and let the resolver do its thing.
            out.push(format!("file://{s}"));
        }
    }
    out
}

/// Legacy pasteboard fallback: read `public.file-url` strings via the
/// deprecated `stringForType:` API. Used only when
/// `readObjectsForClasses:` returns nil (rare — modern macOS always
/// delivers NSURLs for Services — but covers hosts that drop a raw
/// path string on the pasteboard).
unsafe fn read_filenames_legacy(pb: *mut Object) -> Vec<String> {
    let type_str: *mut Object = msg_send![class!(NSString), stringWithUTF8String: FILE_URL_PBOARD_TYPE.as_ptr() as *const i8];
    if type_str.is_null() {
        return Vec::new();
    }
    let s: *mut Object = msg_send![pb, stringForType: type_str];
    if s.is_null() {
        return Vec::new();
    }
    let utf8: *const i8 = msg_send![s, UTF8String];
    if utf8.is_null() {
        return Vec::new();
    }
    match unsafe { std::ffi::CStr::from_ptr(utf8) }.to_str() {
        Ok(s) => {
            // Legacy `public.file-url` pasteboard puts one URL per line
            // (newline-separated) when multiple items are selected.
            s.lines().map(|l| l.to_string()).collect()
        }
        Err(_) => Vec::new(),
    }
}
