# Idea Honing

This project is executed fully unattended (per the task instructions), so instead of an
interactive clarification the decisions below were made by the implementing agent using
the codebase research and the user's request, and are recorded here in question/answer
form so a reviewer can trace each choice.

## Q1: What exactly counts as "the API key is expired"?

**Answer:** Zed cannot know why a provider rejected a key, so we treat any request
rejection classified as an authentication failure — HTTP 401, mapped by
`ProviderErrorCategory::from_http_status` to `ProviderErrorCategory::Authentication` —
as "possibly expired" and trigger the refresh path. 403 (`Permission`) is not treated as
expiry.

## Q2: What does the credentials helper setting look like and where does it live?

**Answer:** A new optional string setting `credentials_helper` per provider under
`language_models.openai_compatible.<provider-id>`, holding an arbitrary shell command
line (like git's `credential.helper`). Example: `"pass show my-provider/api-key"`.
Scope is OpenAI-compatible providers only; Anthropic-compatible providers are not
touched.

## Q3: How is the helper command executed?

**Answer:** Asynchronously via `smol::process::Command` on the GPUI background
executor, run through the user's shell: `sh -c <command>` on macOS/Linux, `cmd /C
<command>` on Windows. The command must exit 0 and print exactly the new API key to
stdout. Zed trims surrounding whitespace. Empty output, non-zero exit, or output with
embedded newlines is an error.

## Q4: What happens after a successful helper run?

**Answer:** Zed stores the new key exactly as if the user had entered it in the
settings UI (`ApiCompatibleProviderState::set_api_key`, which persists to the system
keychain and updates the in-memory `ApiKeyState`, so the settings UI reflects it),
then retries the failed request once with the new key.

## Q5: How many times is a request retried?

**Answer:** At most once. If the retried request still fails with an authentication
error, that error is surfaced to the user as today.

## Q6: What if the key comes from an environment variable?

**Answer:** No refresh. Env-var keys cannot be overwritten in the keychain
(`ApiKeyState::store` refuses), and the helper cannot change Zed's environment. The
original authentication error is surfaced, with a log line explaining why the helper
was skipped.

## Q7: What if several concurrent requests fail with the same expired key?

**Answer:** Refreshes are single-flight per provider: a shared async mutex serializes
them, and each waiter re-reads the current key after acquiring the lock; if another
request already refreshed the key (the key differs from the one that failed), the
helper is not run again. This prevents stampedes of helper executions.

## Q8: What if the helper fails or hangs?

**Answer:** Helper failures (non-zero exit, empty/invalid output, spawn error) are
logged and the original authentication error is surfaced to the user. The helper run
is bounded by a 60-second timeout so a hanging helper cannot stall the request
forever.

## Q9: Is the helper output ever logged?

**Answer:** No. The API key is treated as a secret: it is never logged, echoed, or
included in error messages, consistent with the rest of the credentials code.

## Q10: Do we need UI changes?

**Answer:** No. The setting is a settings-file-only option, like `custom_headers`.
The provider settings UI continues to work unchanged (the stored key shown there is
simply the refreshed one). Documented in `docs/src/ai/use-api-access.md`.

## Q11: Success criteria?

**Answer:**
1. A provider configured with `credentials_helper` whose key expires gets a fresh key
   and the failed request succeeds, without user interaction.
2. Providers without the setting behave exactly as before (single 401 error).
3. Env-var keys behave exactly as before.
4. New unit tests cover helper output parsing (trim, empty, multi-line, non-zero
   exit) and the auth-error classification, and the settings round-trip.
