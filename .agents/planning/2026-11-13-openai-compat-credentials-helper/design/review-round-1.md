# Ponytail Review — Round 1 (Design)

What this change does: adds an optional `credentials_helper` shell command per
OpenAI-compatible provider. On an authentication-classified request failure (401),
Zed runs the helper, takes a single line from its stdout as the new API key, stores it
through the existing keychain path, and retries the request once.

Findings:

1. **Timeout must kill the helper process** (design, `run_credentials_helper`)
   - **What this is:** The design says a 60-second timeout bounds the helper run.
   - **Problem:** If the timeout is implemented by racing the output future with a
     timer, the spawned helper process keeps running after Zed gives up on it.
     Repeated expiries could pile up orphan processes.
   - **Fix:** Spawn the child explicitly, race its `output()` against the timer, and
     call `child.kill()` when the timer wins.
   - **If we skip it:** A hung helper leaks a process on every expiry.

2. **The refresh trigger must not fire when no key was sent** (design, request wiring)
   - **What this is:** `NoApiKey` (nothing stored) and a 401 rejection are different
     errors.
   - **Problem:** Both end the request with an error; only the 401 means "expired".
   - **Fix:** The design already limits the trigger to `ProviderRejection {
     category: Authentication }`; keep the wiring explicit so `NoApiKey` never runs
     the helper.
   - **If we skip it:** A provider with no key would run an arbitrary command on
     every request.

3. **Log the failure reason, never the output** (design, error handling)
   - **What this is:** Helper diagnostics go to the log.
   - **Problem:** The helper's stdout is the API key; its stderr may echo secrets.
   - **Fix:** Log exit status and a short stderr excerpt only on failure, never
     stdout. The design says this; it is repeated here because it is the one place
     a refactor could leak a key.
   - **If we skip it:** API keys in log files.

Everything else checked: single-flight via mutex plus key comparison is sound (a
second waiter sees the changed key and skips the helper); reusing `set_api_key`
keeps keychain, UI, and auth state consistent; refusing env-var keys matches the
existing `ApiKeyState::store` behavior; scope stays inside `open_ai_compatible.rs`.

Verdict: Ship. (Findings 1 and 2 are incorporated into the design/implementation
before coding starts; finding 3 is a coding constraint, not a design change.)
