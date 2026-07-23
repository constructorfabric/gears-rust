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

## Relationship to `cf-chat-engine-sdk`

`cf-chat-engine-sdk` is the stable contract plugin authors compile against (traits, shared models, error types). This crate is the runtime that consumes that contract: it owns persistence, HTTP, streaming, and multi-tenancy, and calls into plugins for anything that requires backend-specific logic.

## License

Same as the parent workspace.
