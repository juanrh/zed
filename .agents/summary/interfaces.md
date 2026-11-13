# Interfaces

## `CredentialsProvider` (`crates/credentials_provider/src/credentials_provider.rs`)

```rust
pub trait CredentialsProvider: Send + Sync {
    fn read_credentials<'a>(&'a self, url: &'a str, cx: &'a AsyncApp)
        -> Pin<Box<dyn Future<Output = Result<Option<(String, Vec<u8>)>>> + 'a>>;
    fn write_credentials<'a>(&'a self, url: &'a str, username: &'a str,
        password: &'a [u8], cx: &'a AsyncApp) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>>;
    fn write_credentials_if_not_exists<'a>(...) -> Pin<Box<dyn Future<Output = Result<bool>> + 'a>>;
    fn delete_credentials<'a>(&'a self, url: &'a str, cx: &'a AsyncApp)
        -> Pin<Box<dyn Future<Output = Result<()>> + 'a>>;
}
```

## `ApiCompatibleProviderState<S>` (`crates/language_models/src/provider/api_compatible.rs`)

Generic per-provider entity shared by OpenAI-compatible and Anthropic-compatible providers:

- `new(id, credentials_provider, resolve_settings, cx)` — observes settings store.
- `is_authenticated()`, `set_api_key(Option<String>, cx) -> Task<Result<()>>`,
  `authenticate(cx)`, `update_settings(settings, cx)`.
- `api_key_state: ApiKeyState` and resolved `settings: S` are public fields.

## `LanguageModelProvider` / `LanguageModelProviderState` (language_model crate)

- `id()`, `name()`, `is_authenticated(cx)`, `authenticate(cx)`, `set_api_key(...)`,
  `settings_view(cx)`, `provided_models(cx)`.
- `LanguageModelClient::stream_completion(model, request, cx)` is the request entry point.

## Error surface

- `open_ai::RequestError::HttpResponseError { provider, status_code, body, headers }`.
- Converted (in `crates/open_ai/src/open_ai.rs` `From<RequestError> for LanguageModelCompletionError`)
  into `LanguageModelCompletionError::ProviderRejection { provider, status, code, message, retry_after, category }`.
- `ProviderErrorCategory::from_http_status`: `401 UNAUTHORIZED → Authentication`,
  `403 FORBIDDEN → Permission`, etc. (`crates/language_model_core/src/language_model_core.rs:169`).
