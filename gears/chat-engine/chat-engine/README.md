# cf-chat-engine

The **Chat Engine** gear: multi-tenant conversational infrastructure with a plugin-driven backend.

Chat Engine owns session state, the immutable message tree, streaming, and routing — but **zero business logic**. All message processing (response generation, summarization, etc.) is delegated to backend plugins that implement the `ChatEngineBackendPlugin` trait from [`cf-chat-engine-sdk`](../chat-engine-sdk).

## What this crate provides

- `ChatEngineModule` — the `#[toolkit::gear(...)]`-annotated entrypoint registered with the platform (capabilities: `db`, `rest`, `stateful`). It wires config, SeaORM repositories, the plugin `ClientHub`, domain services, and the REST router, then runs the retention-cleanup background task for the lifetime of the gear.
- Two first-party reference plugins, registered by default under `ChatEngineBackendPlugin`:
  - `infra::llm_gateway::LlmGatewayPlugin` — integrates with the internal LLM Gateway service.
  - `infra::webhook_compat::WebhookCompatPlugin` — forwards events to legacy HTTP webhook backends.

## Module layout

- `api` — REST surface (routes, request/response DTOs) mounted onto the gear's `Router`.
- `config` — gear configuration, validated on load.
- `domain` — sessions, messages, reactions, variants, retention policy, and the domain services that implement the business rules Chat Engine itself owns (as opposed to plugin-owned response generation).
- `infra` — SeaORM repositories, the leader-election gate for the retention sweep, and the first-party plugin implementations above.

`api`, `config`, `domain`, and `infra` are `pub` (integration tests in `tests/` reach into them) but marked `#[doc(hidden)]` so they don't pollute the public docs surface — `chat_engine_sdk`'s re-exported types plus `ChatEngineModule` are the intended public API.

## API reference

The gear serves its own API reference — no separate docs deployment:

| Route | What |
|---|---|
| `GET {prefix}/chat-engine/v1/docs` | Interactive reference page (Stoplight Elements) |
| `GET {prefix}/chat-engine/v1/openapi` | OpenAPI 3.1 document, 23 operations |

Both are anonymous; `{prefix}` is the gateway's `prefix_path` (`/cf` in `config/quickstart.yaml`, empty by default). The gateway's own `{prefix}/docs` still covers every gear in the process — these two narrow it to Chat Engine.

The document is assembled at route-registration time by [`api::rest::docs`](src/api/rest/docs.rs): a `TeeRegistry` mirrors every operation and schema registered by [`api::rest::routes`](src/api/rest/routes/mod.rs) into a private `OpenApiRegistryImpl`, so the served document is by construction what the gear actually mounted. `servers` is filled from the request path, so "try it" targets the real base URL.

[`../docs/openapi.json`](../docs/openapi.json) is the same document checked in for offline use (review, client codegen). Regenerate it whenever the REST surface changes:

```bash
make openapi-chat-engine
```

The target boots the example server with `config/chat-engine-openapi.yaml`, fetches `/chat-engine/v1/openapi`, and sorts the result for a stable diff. That config mounts at the root (so no deployment-specific `servers` entry is baked in) and sets `enable_search: true` so the two search endpoints are documented even though the runtime default leaves them unmounted.

## Relationship to `cf-chat-engine-sdk`

`cf-chat-engine-sdk` is the stable contract plugin authors compile against (traits, shared models, error types). This crate is the runtime that consumes that contract: it owns persistence, HTTP, streaming, and multi-tenancy, and calls into plugins for anything that requires backend-specific logic.

## License

Same as the parent workspace.
