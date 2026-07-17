//! Theme catalog — built-in + user-loadable TOML themes.
//!
//! Themes are discovered from two sources, merged at runtime into a single
//! catalog keyed by `id`:
//!
//! 1. **Built-in** themes — embedded into the binary at build time via
//!    `include_str!` from `assets/themes/*.toml`. These always exist and
//!    cannot be deleted by the user. They cover the three presets that used
//!    to be hardcoded in `crabport_core::config::ThemeConfig::preset`.
//! 2. **Custom** themes — `.toml` files the user drops into
//!    `{data_dir}/crabport/themes/`. Loaded on startup and on
//!    [`refresh_catalog`]. A custom theme with the same `id` as a built-in
//!    overrides the built-in (so users can tweak "modern-dark" without
//!    forking the binary).
//!
//! Each TOML file deserializes directly into a
//! [`crabport_core::config::ThemeConfig`]; the `name` field doubles as the
//! theme id and must be unique within the merged catalog.
//!
//! The Settings window's theme dropdown reads this catalog (see
//! `windows/settings.rs`), and `color::apply_theme(id)` resolves an id back
//! to a `ThemeConfig` to apply it.

use std::path::PathBuf;

use parking_lot::RwLock;
use std::sync::LazyLock;

use crabport_core::config::ThemeConfig;

// ---------------------------------------------------------------------------
// Built-in themes — embedded at build time
// ---------------------------------------------------------------------------

/// `(id, label, toml text)` triples for every theme under `assets/themes/`.
///
/// The TOML text is embedded via `include_str!` so the binary is
/// self-contained: a packaged `.app` / `.exe` / Linux binary doesn't need
/// the source tree's `assets/` directory at runtime.
///
/// `id` is the filename stem (e.g. `"modern-dark"`); `label` is the
/// human-readable name shown in the dropdown.
static BUILTIN_THEMES: &[(&str, &str, &str)] = &[
    (
        "modern-dark",
        "Modern Dark",
        include_str!("../assets/themes/modern-dark.toml"),
    ),
    (
        "mocha",
        "Catppuccin Mocha",
        include_str!("../assets/themes/mocha.toml"),
    ),
    (
        "tokyo-night",
        "Tokyo Night",
        include_str!("../assets/themes/tokyo-night.toml"),
    ),
];

// ---------------------------------------------------------------------------
// Catalog entry
// ---------------------------------------------------------------------------

/// Where a theme came from. Built-in themes are always available; custom
/// themes only exist when the user has a matching `.toml` file on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeSource {
    /// Embedded in the binary under `assets/themes/`.
    Builtin,
    /// Loaded from `{data_dir}/crabport/themes/*.toml`.
    Custom,
}

/// One entry in the merged theme catalog.
#[derive(Clone, Debug)]
pub struct ThemeEntry {
    /// Unique id (matches `ThemeConfig::name` and the TOML filename stem).
    pub id: String,
    /// Human-readable name for the dropdown.
    pub label: String,
    pub source: ThemeSource,
}

impl ThemeEntry {
    /// Drop-down label — `"Label (custom)"` for custom themes so the user
    /// can tell at a glance which entries are user-supplied. Built-in
    /// themes show the label alone (the "Modern Dark" / "Catppuccin Mocha"
    /// / "Tokyo Night" proper nouns are intentionally left untranslated,
    /// matching the old `ThemeConfig::preset_label` behavior).
    pub fn dropdown_label(&self) -> String {
        match self.source {
            ThemeSource::Builtin => self.label.clone(),
            ThemeSource::Custom => format!("{} (custom)", self.label),
        }
    }
}

// ---------------------------------------------------------------------------
// Catalog storage
// ---------------------------------------------------------------------------

/// One parsed `ThemeConfig` plus its catalog metadata.
#[derive(Clone, Debug)]
struct CatalogItem {
    entry: ThemeEntry,
    config: ThemeConfig,
}

/// Process-wide merged theme catalog. Built once on first access from
/// [`BUILTIN_THEMES`] + the `themes/` data dir, and refreshed on demand via
/// [`refresh_catalog`].
///
/// A `RwLock<Vec<CatalogItem>>` keeps reads (every Settings render) cheap
/// and lock-free-ish while allowing a concurrent `refresh_catalog` to swap
/// the whole list atomically.
static CATALOG: LazyLock<RwLock<Vec<CatalogItem>>> = LazyLock::new(|| RwLock::new(build_catalog()));

/// Resolve `{data_dir}/crabport/themes/`. Returns `None` when the platform
/// can't determine a data dir (sandboxed envs, etc.) — in that case only
/// built-in themes are available.
fn themes_dir() -> Option<PathBuf> {
    let base = dirs::data_dir()?;
    Some(base.join("crabport").join("themes"))
}

/// Build the merged catalog: built-in themes first (in the order they appear
/// in [`BUILTIN_THEMES`]), then custom themes (alphabetical by id). A custom
/// theme with the same `id` as a built-in replaces the built-in entry, so
/// users can override "modern-dark" without forking the binary.
fn build_catalog() -> Vec<CatalogItem> {
    let mut items: Vec<CatalogItem> = Vec::new();

    // Built-in themes.
    for (id, label, toml_text) in BUILTIN_THEMES {
        match toml::from_str::<ThemeConfig>(toml_text) {
            Ok(cfg) => items.push(CatalogItem {
                entry: ThemeEntry {
                    id: (*id).to_string(),
                    label: (*label).to_string(),
                    source: ThemeSource::Builtin,
                },
                config: cfg,
            }),
            Err(e) => {
                // A malformed built-in theme is a build-time bug — log it
                // loudly so it gets noticed, but don't panic (the app can
                // still run with the remaining themes).
                tracing::error!("theme: built-in theme {:?} failed to parse: {e}", id);
            }
        }
    }

    // Custom themes from the data dir.
    if let Some(dir) = themes_dir() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => {
                // Missing dir is the normal case for a fresh install —
                // silently fall back to built-ins only.
                return items;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // Only `.toml` files, and skip directories / hidden files.
            if !path.is_file() {
                continue;
            }
            let is_toml = path
                .extension()
                .map(|ext| ext.eq_ignore_ascii_case("toml"))
                .unwrap_or(false);
            if !is_toml {
                continue;
            }
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("theme: failed to read {}: {e}", path.display());
                    continue;
                }
            };
            let cfg: ThemeConfig = match toml::from_str(&text) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("theme: failed to parse {}: {e}", path.display());
                    continue;
                }
            };
            // Prefer the `name` field inside the TOML as the id when it's
            // non-empty; otherwise fall back to the filename stem. The `name`
            // field is what `config.toml`'s `appearance.theme.name` stores, so
            // keeping it authoritative means a saved selection round-trips
            // correctly even if the user renames the file.
            let id = if cfg.name.trim().is_empty() {
                stem
            } else {
                cfg.name.clone()
            };
            let label = id.clone();

            // Replace a built-in with the same id, if present (override).
            if let Some(existing) = items.iter_mut().find(|it| it.entry.id == id) {
                existing.entry.source = ThemeSource::Custom;
                existing.config = cfg;
                continue;
            }
            items.push(CatalogItem {
                entry: ThemeEntry {
                    id,
                    label,
                    source: ThemeSource::Custom,
                },
                config: cfg,
            });
        }
    }

    // Stable order: built-ins first (already in BUILTIN_THEMES order), then
    // customs alphabetical by id. The custom themes were appended above in
    // filesystem order, which isn't stable across platforms, so re-sort the
    // tail.
    if items.len() > BUILTIN_THEMES.len() {
        items[BUILTIN_THEMES.len()..].sort_by(|a, b| a.entry.id.cmp(&b.entry.id));
    }

    items
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Rebuild the merged catalog from built-in themes + the `themes/` data dir.
/// Call this after the user adds/removes a `.toml` file in
/// `{data_dir}/crabport/themes/` so the Settings dropdown reflects the
/// change without restarting the app.
pub fn refresh_catalog() {
    let fresh = build_catalog();
    *CATALOG.write() = fresh;
}

/// Snapshot of the catalog entries (id + label + source), in display order.
/// Cheap (clones a Vec of small structs) — call freely from render paths.
pub fn list() -> Vec<ThemeEntry> {
    CATALOG.read().iter().map(|it| it.entry.clone()).collect()
}

/// Resolve an id to its `ThemeConfig`. Falls back to the default
/// (`modern-dark`) when the id isn't in the catalog — so a stale
/// `config.toml` referencing a deleted custom theme can never brick the UI.
pub fn get(id: &str) -> ThemeConfig {
    let guard = CATALOG.read();
    for it in guard.iter() {
        if it.entry.id == id {
            return it.config.clone();
        }
    }
    // Fallback: the modern-dark built-in. If even that's missing (shouldn't
    // happen — it's embedded in the binary), fall back to the hardcoded
    // `ThemeConfig::modern_dark()` so we always return *something*.
    for it in guard.iter() {
        if it.entry.id == "modern-dark" {
            return it.config.clone();
        }
    }
    ThemeConfig::modern_dark()
}

/// Human-readable label for a theme id, or `"Modern Dark"` for an unknown id
/// (mirrors the old `ThemeConfig::preset_label` fallback). Convenience for
/// callers that only need the label, not the full catalog.
pub fn label_for(id: &str) -> String {
    let guard = CATALOG.read();
    for it in guard.iter() {
        if it.entry.id == id {
            return it.entry.dropdown_label();
        }
    }
    "Modern Dark".to_string()
}

/// `true` when `id` refers to a built-in theme (vs a user-loaded custom one).
/// Returns `false` for unknown ids.
pub fn is_builtin(id: &str) -> bool {
    CATALOG
        .read()
        .iter()
        .any(|it| it.entry.id == id && it.entry.source == ThemeSource::Builtin)
}
