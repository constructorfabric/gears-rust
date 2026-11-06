# Task Engine SDK

The public API contract of the `task-engine` gear, derived from
[`../docs/openapi.yaml`](../docs/openapi.yaml).

- `TaskEngineApi` — transport-agnostic `#[toolkit::contract]`, one method per
  `operationId` (34). Resolve it from `ClientHub` as `Arc<dyn TaskEngineApi>`.
- `TaskEngineApiRest` — REST projection under `/task_engine/v1`.
  Features: `rest-client` (generated `TaskEngineApiRestClient` /
  `TaskEngineApiRestResolvingClient`), `rest-server` (generated route
  registration), `otel`.
- `models` — DTOs, one per document schema; enums the engine sends are open
  (`Unknown` variant).
- `UnimplementedTaskEngine` — placeholder impl; every method fails
  `unimplemented`.

Known gaps against the document (see `src/rest.rs`):

- `Idempotency-Key` travels in the `NewTask` body until the contract macro can
  bind request headers.
- Operations needing a non-`200` status or response headers are
  `#[server_manual]`: `create_task`, `cancel_task`, `claim_tasks`,
  `report_progress`, `create_entry`, `resolve_wait_condition`,
  `save_runtime_config`.
