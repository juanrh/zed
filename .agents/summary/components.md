# Components

| Crate | Responsibility |
| --- | --- |
| `language_models` | Registry of LLM providers; per-provider settings resolution; `ApiCompatibleProviderState` (shared by `open_ai_compatible.rs` and `anthropic_compatible.rs`); settings UI wiring. |
| `language_model` | Provider-agnostic model types, `ApiKeyState` (env var / keychain), `LanguageModelClient` trait re-exports. |
| `language_model_core` | `LanguageModelCompletionError`, `ProviderErrorCategory`, request/stream types shared by all providers. |
| `open_ai` | OpenAI wire protocol: chat completions and Responses API; `RequestError`; error → `LanguageModelCompletionError` conversion. |
| `settings_content` | Serde/JsonSchema definitions of everything in `settings.json`. |
| `credentials_provider` | `CredentialsProvider` trait (read/write/delete credentials), backed by the system keychain in the app. |
| `settings_ui` | Settings pages, including the LLM providers page that renders `ApiCompatibleProviderConfigurationView`. |
| `http_client` | HTTP abstraction (`HttpClient`, `StatusCode`, `CustomHeaders`). |
| `gpui` | App framework: entities, contexts (`App`, `AsyncApp`, `Context<T>`), background executor, `Task`. |
