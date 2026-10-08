//! AI provider types + provider resolution.
//!
//! `PROVIDER_TYPES` maps the persisted protocol id (`[[ai.providers]].type`)
//! to its display label, so the settings pane's "Type" dropdown and future
//! backends stay in one place. The default config ships the built-in
//! provider entries pre-seeded (see
//! [`crabport_core::config::BUILTIN_PROVIDER_IDS`]) — "Add Provider" is only
//! needed for *additional* endpoints.
//!
//! Storage split: non-secret settings live in `config.toml` under `[ai]`
//! (see `crabport_core::config::AiConfig` — a list of provider entries plus
//! an `active` pointer); each entry's API key lives AES-256-GCM encrypted
//! in the store (`ai_secrets` table, keyed by entry id).

use crabport_ai::OpenAiProvider;
use crabport_core::config;
use gpui::App;
use secrecy::SecretString;

use crate::app_state::AppState;

/// One selectable provider protocol type.
pub struct ProviderType {
    pub id: &'static str,
    /// i18n key under `window.settings.ai.type_*`.
    pub label_key: &'static str,
}

/// Display order = dropdown order. Only OpenAI-compatible chat completions
/// is implemented today; an Anthropic backend would be one entry here plus
/// one transport implementation — the storage format doesn't change.
pub const PROVIDER_TYPES: &[ProviderType] = &[ProviderType {
    id: "openai",
    label_key: "window.settings.ai.type_openai",
}];

pub fn provider_type_by_id(id: &str) -> Option<&'static ProviderType> {
    PROVIDER_TYPES.iter().find(|t| t.id == id)
}

/// API key stored for `entry`, read under
/// [`AiProviderConfig::effective_key_id`](crabport_core::config::AiProviderConfig::effective_key_id)
/// so entries that borrow another entry's key (OpenCode Zen / Go) resolve
/// the same secret.
fn stored_api_key(cx: &App, entry: &config::AiProviderConfig) -> Option<String> {
    AppState::store(cx)
        .lock()
        .ai_api_key(entry.effective_key_id())
        .ok()
        .flatten()
}

/// Whether the entry's gateway routes chat traffic by a per-conversation
/// session id. OpenCode's Zen and Go endpoints reject requests without
/// `x-opencode-session` (`MissingSessionID`); other OpenAI-compatible
/// endpoints never see that header.
fn wants_session_header(entry: &config::AiProviderConfig) -> bool {
    matches!(
        entry.id.as_str(),
        config::OPENCODE_ZEN_PROVIDER_ID | config::OPENCODE_GO_PROVIDER_ID
    )
}

/// Build a provider for a specific config entry when its endpoint and key
/// are usable. Model-independent — used to fetch model lists *before* a
/// model has been chosen, and per entry so every configured provider can
/// be listed in the panel's combined picker.
///
/// Returns `None` when the entry's base URL is unset or no API key is
/// stored for it.
pub fn resolve_entry(
    cx: &App,
    entry: &crabport_core::config::AiProviderConfig,
) -> Option<OpenAiProvider> {
    resolve_entry_session(cx, entry, None)
}

/// [`resolve_entry`] for a chat turn: the conversation's `session_id` is
/// attached for gateways that want one (see [`wants_session_header`]).
/// Model-list fetches pass `None` — they aren't part of a conversation.
fn resolve_entry_session(
    cx: &App,
    entry: &crabport_core::config::AiProviderConfig,
    session_id: Option<&str>,
) -> Option<OpenAiProvider> {
    if entry.base_url.trim().is_empty() {
        return None;
    }
    let key = stored_api_key(cx, entry)?;
    if key.trim().is_empty() {
        return None;
    }
    let base = entry.base_url.trim();
    let provider = match session_id {
        Some(session_id) if wants_session_header(entry) => {
            OpenAiProvider::with_session(base, SecretString::from(key), session_id)
        }
        _ => OpenAiProvider::new(base, SecretString::from(key)),
    };
    provider.ok()
}

/// Whether a chat turn could start right now: AI enabled, a model selected,
/// and the active endpoint holding a key. Unlike the `resolve_*` builders
/// this sets up no HTTP client, so it is cheap enough for `render`.
pub fn configured(cx: &App) -> bool {
    let ai = config::snapshot().ai;
    if !ai.enabled || ai.model.trim().is_empty() {
        return false;
    }
    let Some(entry) = ai.active_provider() else {
        return false;
    };
    !entry.base_url.trim().is_empty()
        && stored_api_key(cx, entry).is_some_and(|key| !key.trim().is_empty())
}

/// Resolve the active endpoint into a provider for a chat turn, tagged with
/// the conversation's `session_id` where the gateway needs one. Callers
/// surface the "not configured" state in their own UI (see [`configured`]).
pub fn resolve_provider_session(cx: &App, session_id: &str) -> Option<OpenAiProvider> {
    let ai = config::snapshot().ai;
    if !ai.enabled || ai.model.trim().is_empty() {
        return None;
    }
    let entry = ai.active_provider()?;
    resolve_entry_session(cx, entry, Some(session_id))
}
