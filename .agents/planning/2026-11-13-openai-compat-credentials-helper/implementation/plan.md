# Implementation Plan: Credentials Helper for OpenAI-Compatible Providers

## Checklist

- [x] Step 1: Settings plumbing — `credentials_helper` field through
      `settings_content` and `language_models::settings`, with round-trip test.
- [x] Step 2: Helper execution + classification — `run_credentials_helper`,
      `is_authentication_rejection`, parse function, unit tests.
- [x] Step 3: Refresh orchestration — `refresh_api_key` single-flight logic.
- [x] Step 4: Request-path wiring — retry-once in `stream_chat_completion` and
      `stream_response`; compile + full crate test run.
- [ ] Step 5: Documentation — `docs/src/ai/use-api-access.md` section update.
- [ ] Step 6: Ponytail review round(s) (max 3) and fixes.

## Working agreements

  Proceed step by step:

- When completing a step, add an "Implementation notes" subsection to its section in the plan, summarizing what we did, focusing on deviations from the plan, why we made them, and implications for subsequent steps

Each step below builds on the previous ones and ends with working, tested code. Every
step is committed separately. Verification commands per step: `cargo test -p
<crate>` and `cargo clippy -p <crate>` (or `cargo check` for compile-only steps);
`cargo run -p docs_preprocessor` after the docs step.

---

## Step 1: Settings plumbing

Add `credentials_helper: Option<String>` to `OpenAiCompatibleSettingsContent`
(`crates/settings_content/src/language_model.rs`) and to
`OpenAiCompatibleSettings` (`crates/language_models/src/provider/open_ai_compatible.rs`),
map it in `from_settings` (`crates/language_models/src/settings.rs`). Add a test
asserting the field survives the settings-content → settings conversion (extend the
existing settings tests in `crates/language_models/src/settings.rs` if present,
otherwise a deserialization test in `settings_content`).

### Implementation notes

Done as planned. `credentials_helper: Option<String>` added to
`OpenAiCompatibleSettingsContent` and `OpenAiCompatibleSettings`, mapped in
`from_settings`. Two serde tests added in `settings_content` (explicit value and
default `None`); the content struct requires `available_models`, so the test
fixtures include an empty list. `cargo test -p settings_content --lib language_model`
and `cargo check -p language_models` pass. No deviations.

---

## Step 2: Helper execution and error classification

In `open_ai_compatible.rs`, add:

- `fn is_authentication_rejection(error: &LanguageModelCompletionError) -> bool` —
  matches `ProviderRejection { category: Authentication, .. }`.
- `fn parse_helper_output(output: &str) -> anyhow::Result<String>` — trims, rejects
  empty and multi-line output.
- `async fn run_credentials_helper(command: &str, executor: &BackgroundExecutor)
  -> anyhow::Result<String>` — `sh -c` / `cmd /C`, captures stdout/stderr,
  non-zero exit → error, 60s timeout via executor timer.

Unit tests: classification (401 vs 403 vs stream errors), parsing (trim/empty/
multi-line), and a real-process test of `run_credentials_helper` using `printf` /
`exit 1` shell commands.

### Implementation notes

Done with one deviation: `run_credentials_helper` takes no `BackgroundExecutor`.
`#[gpui::test]` runs under the deterministic test scheduler, which rejects real
async-io process activity ("Your test is not deterministic"), so the timeout uses
`smol::Timer::after` directly and the function is executor-independent. The three
helper tests are plain `#[test]`s using `smol::block_on`. Timeout enforcement relies
on `kill_on_drop(true)`: on timeout, `smol::future::or` drops the output future,
dropping the child and killing the process (review finding 1). Tests: classification
(401 vs 403/429/NoApiKey), parse (trim/empty/multi-line), and real-process runs
(success, non-zero exit, multi-line stdout). All 22 crate-local tests pass.

---

## Step 3: Refresh orchestration

Add `refresh_lock: Arc<smol::lock::Mutex<()>>` to the provider struct (created in
`new`). Add `async fn refresh_api_key(...)` implementing: single-flight lock;
skip if the key already changed since the failure; refuse env-var keys; run the
helper; `state.set_api_key(...)` and await; re-read and return the new key. All
failures log (never the key) and return `Err`.

### Implementation notes

Done as designed: `refresh_api_key` takes the single-flight `smol::lock::Mutex`
guard, refuses env-var keys, skips the helper when the stored key already differs
from the failed one, runs the helper, persists via `set_api_key`, and re-reads the
key. One deviation from the plan sketch: it takes `&mut AsyncApp` (gpui's
`Entity::update` requires a mutable context) and the credentials helper command is
passed in by the caller rather than re-read from settings. Not yet called; wired in
step 4.

---

## Step 4: Request-path wiring

Refactor `stream_chat_completion` and `stream_response` so the send is a reusable
`attempt(api_key)` future (same `RateLimiter`), then wrap: on authentication
rejection with a configured helper, call `refresh_api_key` and retry once; otherwise
surface the original error. Run `cargo test -p language_models` and
`cargo check -p zed` (or the broadest feasible compile check).

### Implementation notes

Done with structural deviations forced by gpui:
- `AsyncApp` is not `Send`, so the attempt/retry orchestration cannot live in the
  boxed (Send) future. It runs inside a `cx.spawn` task, and the returned future
  just awaits the `Task`.
- The spawned task requires `'static` captures, so the single-attempt senders
  (`send_chat_completion`, `send_response`) are free functions taking owned
  arguments, and the wire requests are rebuilt per attempt via a
  `SharedRequestBuilder` closure (`open_ai::Request` / `ResponseRequest` are not
  `Clone`; `LanguageModelRequest` is, so the closure clones it and re-runs
  `into_open_ai`/`into_open_ai_response`).
- `RateLimiter` is `Clone`, so each attempt re-enters the same limiter.
- The 60s helper timeout uses `smol::Timer` with a targeted, commented
  `#[allow(clippy::disallowed_methods)]`: the helper does real process I/O, which
  can never run under gpui's deterministic test scheduler, so
  `BackgroundExecutor::timer` is not usable here.
Verification: `cargo test -p language_models --lib` 144 passed; `cargo clippy -p
language_models` clean; `cargo fmt` applied. `cargo check -p settings_ui` fails in
this sandbox on a missing system glib (build script / pkg-config), unrelated to the
change.

---

## Step 5: Documentation

Update `docs/src/ai/use-api-access.md` (OpenAI-Compatible Endpoints section) with
`credentials_helper`: what it is, when it runs, output contract (exit 0, single
line on stdout), env-var limitation, and a settings example. Run
`cargo run -p docs_preprocessor` to validate docs examples.

### Implementation notes

(to be filled on completion)

---

## Step 6: Ponytail review rounds

Run the ponytail-review skill on the design (already done once before
implementation) and on the implemented code after each step; incorporate all
suggestions; at most 3 rounds total, stopping early on a "Ship" verdict.

### Implementation notes

(to be filled on completion)
