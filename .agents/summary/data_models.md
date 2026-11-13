# Data Models

## Settings chain for OpenAI-compatible providers

1. `settings_content::OpenAiCompatibleSettingsContent`
   (`crates/settings_content/src/language_model.rs:424`):
   `api_url: String`, `available_models: Vec<OpenAiCompatibleAvailableModel>`,
   `custom_headers: Option<HashMap<String, String>>`.
2. `language_models::OpenAiCompatibleSettings`
   (`crates/language_models/src/provider/open_ai_compatible.rs:35`):
   `api_url`, `available_models`, `custom_headers: CustomHeaders`. Implements
   `ApiCompatibleProviderSettings` (`fn api_url()`).
3. Merged map: `AllLanguageModelSettings::openai_compatible: HashMap<Arc<str>, OpenAiCompatibleSettings>`
   keyed by user-chosen provider id.

## API key

- `ApiKeyState { url, env_var, load_status: LoadStatus, load_task }`.
- `LoadStatus`: `NotLoaded | NotPresent | Error(String) | Loaded(ApiKey)`.
- `ApiKey { source: EnvVar | SystemKeychain, key: Arc<str> }` — key never logged.

## Errors

- `LanguageModelCompletionError` (language_model_core): `NoApiKey`, `ProviderRejection`,
  `ModelUnavailable`, transport variants, `Other(anyhow::Error)`.
- `ProviderErrorCategory`: `Authentication` (401), `Permission` (403), `RateLimit` (429),
  `PromptTooLarge`, `Overloaded`, `Timeout`, `InvalidRequest`, `ContentPolicy`, `Other`, ...
