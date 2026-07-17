//! Build script for the main `CrabPort` binary.
//!
//! On Windows, embeds `app-icon.ico` into the final `.exe` so the OS shows
//! our icon in the taskbar, Alt-Tab switcher, and Explorer (instead of the
//! generic Rust exe icon). The icon is referenced from a tiny `.rc` resource
//! script compiled by `embed-resource`, which finds `rc.exe` from the Windows
//! SDK / MSVC toolchain automatically.
//!
//! On macOS and Linux this is a no-op — the platform bundles (`.app` /
//! `.AppImage`) carry the icon via `cargo-bundle` using
//! `[package.metadata.bundle].icon`, not via the binary itself.
//!
//! No version-sync check is needed here: `[workspace.package].version` is the
//! single source of truth, inherited by every member crate via
//! `version.workspace = true`. `cargo-bundle` reads the package version via
//! `cargo_metadata` (which resolves workspace inheritance), and since we
//! intentionally do NOT set `bundle.version`, it falls back to the package
//! version automatically — no duplication to drift out of sync.

#[cfg(target_os = "windows")]
fn main() {
    // Re-run if the icon or rc file changes.
    println!("cargo:rerun-if-changed=app-icon.ico");
    println!("cargo:rerun-if-changed=crabport.rc");
    embed_resource::compile("crabport.rc", embed_resource::NONE)
        .manifest_optional()
        .expect("failed to compile crabport.rc");
}

#[cfg(not(target_os = "windows"))]
fn main() {}
