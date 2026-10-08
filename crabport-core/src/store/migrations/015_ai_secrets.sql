-- API keys for AI provider entries (the `id` of each `[[ai.providers]]`
-- table in config.toml), AES-256-GCM encrypted with the shared key file.
-- Managed exclusively through store/ai.rs — keys are never written to the
-- plain-text config.toml.
CREATE TABLE ai_secrets (
    provider_id TEXT PRIMARY KEY,
    api_key BLOB NOT NULL
);
