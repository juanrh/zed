# Knowledge Base Index

Primary context file for AI assistants working in this repository. Each document below is
scoped to the LLM provider / credentials subsystem, which is the area this summary
analyzed in depth.

| File | Purpose | When to consult |
| --- | --- | --- |
| [codebase_info.md](codebase_info.md) | Repo identity, languages, tooling, relevant crates. | First orientation. |
| [architecture.md](architecture.md) | How settings flow from `settings.json` to provider requests; error flow back. | Understanding data flow for provider changes. |
| [components.md](components.md) | Responsibilities of each crate in the subsystem. | Locating where a change belongs. |
| [interfaces.md](interfaces.md) | Key traits and public APIs (`CredentialsProvider`, `LanguageModelProvider`, `ApiKeyState`). | Extending or calling these APIs. |
| [data_models.md](data_models.md) | Settings structs, error enums, api key state machine. | Adding settings fields or error categories. |
| [workflows.md](workflows.md) | Request lifecycle, api key load/store, auth failure surfacing. | Tracing runtime behavior. |
| [dependencies.md](dependencies.md) | External crates used by the subsystem. | Choosing utilities (async process spawn, HTTP). |
| [review_notes.md](review_notes.md) | Gaps and inconsistencies found while summarizing. | Before deep-diving into undocumented areas. |

Example queries this index supports:
- "Where do I add a new per-provider setting?" → `architecture.md` (settings pipeline), `data_models.md`.
- "How does Zed know an API key expired?" → `workflows.md` (auth failure flow).
- "How do I run an external command asynchronously?" → `dependencies.md` (`smol::process`).
