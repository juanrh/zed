# Architecture (LLM provider subsystem)

## Settings pipeline

```mermaid
graph LR
    U[settings.json language_models.openai_compatible] --> SC[settings_content crate
OpenAiCompatibleSettingsContent]
    SC --> LM[language_models crate settings.rs
OpenAiCompatibleSettings]
    LM --> PS[ApiCompatibleProviderState&lt;OpenAiCompatibleSettings&gt;
Entity per provider id]
    PS -->|observe_global SettingsStore| PS
```

- `settings_content` defines the user-facing JSON shape (serde + JsonSchema, `#[with_fallible_options]`).
- `language_models::settings.rs` converts content structs into resolved settings in
  `AllLanguageModelSettings::from_settings`.
- `ApiCompatibleProviderState::new` observes the global settings store and calls
  `update_settings` when the merged settings for the provider id change.

## Request lifecycle

```mermaid
sequenceDiagram
    participant Agent as Agent panel
    participant Prov as OpenAiCompatibleLanguageModelProvider
    participant OA as open_ai crate
    participant API as Provider HTTP API
    Agent->>Prov: LanguageModelClient::stream_completion
    Prov->>Prov: stream_chat_completion / stream_response
    Prov->>OA: stream_completion(http_client, api_url, api_key, ...)
    OA->>API: POST chat/completions or responses
    API-->>OA: error 401
    OA-->>Prov: LanguageModelCompletionError::ProviderRejection
    Prov-->>Agent: error surfaced in thread UI
```

- The api key is read from `state.api_key_state.key(api_url)` at request time
  (`crates/language_models/src/provider/open_ai_compatible.rs:139-159`).
- There is no retry or credential-refresh step today: a 401 fails the request and the
  user must reset the key in the settings UI.

## API key state machine

```mermaid
stateDiagram-v2
    [*] --> NotLoaded
    NotLoaded --> Loaded: load_if_needed (env var or keychain)
    NotLoaded --> NotPresent: no credentials
    Loaded --> NotPresent: store(None) / delete
    Loaded --> Loaded: store(Some(key)) writes keychain
    NotPresent --> Loaded: store(Some(key))
```

- Env-var keys cannot be overwritten from Zed (`ApiKeyState::store` returns an error).
- Keychain keys are namespaced by `api_url`.
