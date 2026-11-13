# Research: Existing Code

## Settings pipeline for OpenAI-compatible providers

- User JSON: `language_models.openai_compatible.<id>` →
  `OpenAiCompatibleSettingsContent` (`crates/settings_content/src/language_model.rs:424`)
  with `api_url`, `available_models`, `custom_headers`.
- Converted in `crates/language_models/src/settings.rs:176-190` into
  `OpenAiCompatibleSettings` (`crates/language_models/src/provider/open_ai_compatible.rs:35`).
- The provider entity `ApiCompatibleProviderState<OpenAiCompatibleSettings>`
  (`crates/language_models/src/provider/api_compatible.rs`) observes the settings store
  and updates `state.settings` on change.

## API key handling

- `ApiKeyState` (`crates/language_model/src/api_key.rs`) manages one key from env var
  or system keychain; `store()` refuses to overwrite env-var keys; keys are namespaced
  by `api_url`.
- `ApiCompatibleProviderState::set_api_key(Some(key), cx)` persists to the keychain
  and updates `load_status` so `is_authenticated()` immediately reflects the new key.
- `read_credentials` is gated (`set_credentials_read_gate`) for tests/privacy mode.

## Request path and error flow

- `OpenAiCompatibleLanguageModelProvider::stream_chat_completion` /
  `stream_response` (`crates/language_models/src/provider/open_ai_compatible.rs:126-197`)
  read the key from `state.api_key_state.key(api_url)`, then send via the `open_ai`
  crate inside a `request_limiter.stream(...)` future. Both return
  `Result<BoxStream<...>, LanguageModelCompletionError>`; the key error paths are
  `NoApiKey` (no key) and `ProviderRejection` (HTTP failure).
- HTTP 401 from the provider becomes `open_ai::RequestError::HttpResponseError`
  (`crates/open_ai/src/open_ai.rs:926`), converted by
  `impl From<RequestError> for LanguageModelCompletionError`
  (`crates/open_ai/src/open_ai.rs:~1340`) into
  `LanguageModelCompletionError::ProviderRejection { category: ProviderErrorCategory::Authentication }`
  (mapping at `crates/language_model_core/src/language_model_core.rs:186`).
- Classification helper: `ProviderErrorCategory::from_http_status`, and
  `LanguageModelCompletionError::from_http_status`.

## Spawning external processes asynchronously

- Pattern in repo: `smol::process::Command` (e.g. `crates/remote/src/transport.rs:745`
  runs `/bin/sh -c ...`). `askpass` documents smol process usage. GPUI background
  executor (`cx.background_executor().spawn(...)`) is used for blocking work.
- `smol::lock::Mutex` exists for async single-flight serialization; `smol` is already
  a workspace dependency.

## Settings UI

- `ApiCompatibleProviderConfigurationView` (`crates/language_models/src/provider/api_compatible.rs`)
  renders the key entry/reset UI for both compatible provider families; no changes
  needed for a settings-file-only option.

## Docs

- OpenAI-compatible provider docs: `docs/src/ai/use-api-access.md` (section
  "OpenAI-Compatible Endpoints"); settings reference: `docs/src/reference/all-settings.md`.
  Docs code blocks are validated by the `docs_preprocessor` crate.

## Tests

- `crates/language_models/src/provider/open_ai_compatible.rs` has plain `#[test]`
  unit tests for request construction helpers (no gpui test context needed for pure
  functions).
- `crates/open_ai/src/chat_completion_transport_tests.rs` shows HTTP-level fake
  transport tests; heavier than needed for this feature.

## Conclusions for the design

1. The refresh trigger is cleanly detectable: match on
   `LanguageModelCompletionError::ProviderRejection { category: Authentication, .. }`.
2. The refresh+retry belongs in `open_ai_compatible.rs` around the two request
   futures; the shared `ApiCompatibleProviderState` and anthropic-compatible path
   stay untouched.
3. Persistence reuses `set_api_key`, so the keychain, settings UI, and
   `is_authenticated` all stay consistent for free.
4. A single-flight async mutex + key-comparison avoids helper stampedes without
   adding entities or fields to shared types.
