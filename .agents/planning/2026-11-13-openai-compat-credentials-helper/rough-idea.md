# Rough Idea

For OpenAI-compatible LLM providers in Zed, users must supply long-lived API keys.
When such a key expires, Zed currently fails every request with an authentication
error (HTTP 401) and the user has to manually reset the API key in the settings UI.

We want support for configuring an arbitrary **credentials helper command** per
OpenAI-compatible provider. When Zed detects that the current API key is expired
(request rejected as unauthenticated), Zed will:

1. Run the configured credentials helper command.
2. The helper prints a new API key to stdout (only that, nothing more).
3. Zed automatically updates the stored API key to the helper's output and retries
   the failed request with the new key.

Scope: OpenAI-compatible providers only (`language_models.openai_compatible.*` in
`settings.json`).

Source request: https://github.com/zed-industries/zed/ issue work, to be delivered
as a PR from the fork `juanrh/zed`.
