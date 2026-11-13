# Codebase Info

- **Repository**: `zed-industries/zed` (fork: `juanrh/zed`), Rust workspace, ~200+ crates under `crates/`.
- **Product**: The Zed editor (GPUI-based) with a built-in AI agent panel.
- **Languages**: Rust (primary), TypeScript/WASM (extensions), MDX (docs in `docs/src`).
- **Tooling**: Cargo workspace (`Cargo.toml`), `rustfmt.toml`, `clippy.toml`, CI in `.github/workflows`.
- **Relevant subsystem for the current task** (credentials helper for OpenAI-compatible LLM providers):
  - `crates/language_models` — provider registry, per-provider settings, `ApiCompatibleProviderState` shared by OpenAI/Anthropic-compatible providers.
  - `crates/language_model` — `ApiKeyState` (keychain/env-var API key handling), `LanguageModelClient`.
  - `crates/language_model_core` — `LanguageModelCompletionError`, `ProviderErrorCategory` (401 → `Authentication`).
  - `crates/open_ai` — wire protocol client; `RequestError::HttpResponseError` → `LanguageModelCompletionError::ProviderRejection`.
  - `crates/settings_content` — user-facing settings JSON schemas (`OpenAiCompatibleSettingsContent`).
  - `crates/credentials_provider` — keychain abstraction trait.
  - `crates/settings_ui` — LLM providers settings page.
- **Key build/test commands** (repo-specific): `cargo build -p language_models`, `cargo test -p language_models`, docs lint via `cargo run -p docs_preprocessor` (checks docs examples).
