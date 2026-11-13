# Workflows

## Adding a per-provider setting (e.g. for openai_compatible)

1. Add the field to `OpenAiCompatibleSettingsContent` in
   `crates/settings_content/src/language_model.rs` (serde default, `Option<...>`).
2. Map it in `crates/language_models/src/settings.rs::from_settings` into
   `OpenAiCompatibleSettings`.
3. Consume it in `crates/language_models/src/provider/open_ai_compatible.rs` via
   `state.settings`.
4. Document it in `docs/src/ai/use-api-access.md` (OpenAI-Compatible section) and, if
   applicable, `docs/src/reference/all-settings.md`. Docs examples are validated by
   `docs_preprocessor`.

## API key entry today

1. User opens Agent Settings → LLM Providers → provider sub-page
   (`ApiCompatibleProviderConfigurationView`).
2. `authenticate()` loads the key from env var or keychain; if absent, an input field is
   shown; `save_api_key` stores it via `ApiKeyState::store` → `CredentialsProvider::write_credentials`.
3. `Reset API Key` button deletes the stored key.

## Auth failure today (no automatic recovery)

1. Request fails with HTTP 401 → `RequestError::HttpResponseError`.
2. Converted to `LanguageModelCompletionError::ProviderRejection { category: Authentication }`.
3. Agent UI shows the error; every subsequent request fails until the user manually
   resets the API key. This is the gap the credentials-helper feature addresses.
