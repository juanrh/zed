# Dependencies (subsystem-relevant)

- `gpui` — entity system, `Task`, `AsyncApp`, background executor (`cx.background_executor().spawn(...)`).
- `smol` — async runtime used by zed; `smol::process::Command` is the established way to
  spawn external processes asynchronously (see `crates/remote/src/transport.rs:745`).
  `std::process::Command` is used for synchronous one-shots (e.g. `system_specs`).
- `anyhow` — error type used throughout app crates.
- `serde` / `serde_json` / `schemars` (via `settings_content`) — settings serialization and JSON schema.
- `http_client` — `HttpClient` trait, `StatusCode`, `CustomHeaders`, `HeaderMap`.
- `keyring`-style persistence is behind `CredentialsProvider`; app crate wires the OS keychain.
- `convert_case` — used to derive env var names (`{ID}_API_KEY`).
