//! Secondary window types and the registry that manages them.
//!
//! The main terminal window is constructed directly in `main.rs` (it owns the
//! heavy `CrabportApp` state). Auxiliary windows — Settings, About — are
//! lighter views opened on demand from anywhere in the app via
//! [`focus_or_open`].
//!
//! ## Singleton policy
//!
//! Each `AuxWindowKind` is treated as a singleton: calling `focus_or_open`
//! when a window of that kind already exists brings it to the front rather
//! than spawning a duplicate. The registry tracks open windows by kind in
//! `WindowRegistry` (stored as a GPUI global).

pub mod about;
pub mod registry;
pub mod settings;
pub mod theme_editor;

pub use about::AboutWindow;
pub use registry::{AuxWindowKind, WindowRegistry, focus_or_open};
pub use settings::SettingsWindow;
pub use theme_editor::ThemeEditorWindow;
