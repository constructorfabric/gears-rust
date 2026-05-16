---
cpt:
  version: "1.11"
  changelog:
    - version: "1.11"
      date: "2026-05-16"
      changes:
        - "Re-align with the updated `cpt-cf-usage-collector-feature-sdk-and-ingest-core` emitter and error semantics after rebase: (a) `ClientHub` registration key is `dyn UsageEmitterRuntimeV1` (the layer-1 runtime) — `UsageEmitterFactoryV1` is not a trait that exists, the factory is the cloneable layer-2 `UsageEmitterFactory` obtained via `runtime.factory(module_name)`. Out-of-process sources now drive the three-layer chain `runtime.factory(MODULE_NAME).with_*().authorize(ctx, resource_id, resource_type)?` then `usage_record_builder()?.build()? → enqueue(record)` / `enqueue_in(db, record)`. Updated §1.1, §1.2, §2 Module Init Flow output, §3 `inst-init-6` (`UsageEmitter` → `UsageEmitterRuntime::build(...)`) and `inst-init-7` (registration key), §5 DoD body for `UsageCollectorRestClientModule`, and §6 AC. (b) Module Config Retrieval Flow (REST) — the factory's `.authorize()` step propagates non-`NotFound` variants from `get_module_config()` unchanged per Feature 1 `inst-authz-5a`; the emitter no longer collapses them to `UsageEmitterError::Internal`. §2 Error Scenarios rewritten to list `ServiceUnavailable` (transport / 401 / 5xx), `DeadlineExceeded` (timeout), `ResourceExhausted` (429), and `PermissionDenied` (403) propagation paths; `inst-cfg-rem-6a` now enumerates the explicit HTTP-status → canonical-variant mappings (`401`→`ServiceUnavailable`, `403`→`ModuleConfigError::permission_denied()`, `429`→`ModuleConfigError::resource_exhausted()`, `5xx`→`ServiceUnavailable`, timeout→`ModuleConfigError::deadline_exceeded()`, transport / residual `HttpError` variants→`ServiceUnavailable`, any other unexpected status→`Internal`) and corrects the stale `inst-authz-5b` cross-reference to `inst-authz-5a`; §5 DoD body and §6 AC for `get_module_config()` extended with the same explicit mapping list. (c) `UsageRecord` JSON shape — the legacy `subject_id` / `subject_type` field pair has been replaced by a single nested `subject: Option<Subject>` object (Feature 1 `inst-enq-8`); `inst-rem-5` updated to describe the nested serialization (absent when `None`, `\"subject\": { \"id\": \"...\", \"type\": \"...\" }` when `Some`, with the inner `type` itself absent when `Subject.r#type` is `None`)."
    - version: "1.10"
      date: "2026-05-10"
      changes:
        - "§5 DoD `BearerTokenAuthLayer` bullet: widen the AuthN-resolver-originated outcome collapse so the narrative matches `bearer_token_auth_layer.rs`. All AuthN-resolver-originated outcomes — transient resolver failures, permanent credential rejection (`Unauthorized` / `NoPluginAvailable`), and the post-exchange missing-token guard when the returned `SecurityContext` carries no bearer token — collapse to `HttpError::Transport(Box<AuthNResolverError>)`. In addition, an invalid token byte sequence yields `HttpError::InvalidHeaderValue`, which the REST client maps to `ServiceUnavailable` via the residual non-Transport, non-Timeout arm (already covered by the `create_usage_record()` mapping list — this is now stated explicitly in the layer description). No code change."
    - version: "1.9"
      date: "2026-05-10"
      changes:
        - "Document the bearer-token Tower middleware architecture (RESEARCH-diverge issue G) and the flat `infra/` crate layout (RESEARCH-diverge issue J). Token acquisition + `Authorization` header injection are now a per-request `tower::Layer` (`BearerTokenAuthLayer`), wired into the shared `HttpClient` via `HttpClientBuilder::with_auth_layer(...)`; the REST client's `create_usage_record()` / `get_module_config()` issue plain `.post(...).send()` / `.get(...).send()` calls. The layer collapses transient AuthN failures and permanent credential rejection alike to `HttpError::Transport`, which `rest_client.rs` maps to `ServiceUnavailable` (preserves the previous `inst-rem-3a` / `inst-rem-4a` mapping). Updates: §2 Remote Usage Emission Flow steps `inst-rem-2` / `inst-rem-3` / `inst-rem-4` and Module Config Retrieval Flow step `inst-cfg-rem-2`; §3 REST Client Module Initialization step `inst-init-5` (HTTP client built with `BearerTokenAuthLayer` injected via `with_auth_layer`); §5 DoD body for `UsageCollectorRestClient` (token acquisition is performed by the `BearerTokenAuthLayer`; the REST client method itself only issues the HTTP request). Crate layout: §5 DoD body now reflects the flat `infra/` module (`rest_client.rs`, `bearer_token_auth_layer.rs`, plus tests + `test_support.rs`); the previous `api/rest/dto.rs` DTO layer has been removed — the client serializes `usage_collector_sdk::models::UsageRecord` directly. Public surface: `pub use config::UsageCollectorRestClientConfig; pub use infra::UsageCollectorRestClient;`."
    - version: "1.8"
      date: "2026-05-10"
      changes:
        - "Align emitter trait name with the implemented crate surface (RESEARCH-diverge issue C): `UsageEmitterV1` → `UsageEmitterFactoryV1` across §1 Overview, §2 Module Init Flow output, §3 inst-init-7, §5 DoD body for `UsageCollectorRestClientModule`, and §6 AC for `ClientHub` availability. The factory trait is the `ClientHub` registration key shared with Feature 1."
    - version: "1.7"
      date: "2026-05-10"
      changes:
        - "Complete v1.6 canonical-taxonomy alignment by fixing two missed sites that still referenced the nonexistent `ModuleNotFound` variant: §5 DoD body for the `usage-collector-rest-client` crate (`maps 404 to ModuleNotFound` → `maps 404 to NotFound (built via ModuleConfigError::not_found(module_name))`) and §6 AC for `get_module_config()` (`returns ModuleNotFound on 404` → `returns NotFound (built via ModuleConfigError::not_found(module_name)) on 404`). v1.6 updated §2 and `inst-cfg-rem-5a`/`-6a` but missed the DoD body and AC."
    - version: "1.6"
      date: "2026-05-10"
      changes:
        - "Align Module Config Retrieval Flow (REST) with the canonical taxonomy (`UsageEmitterError = UsageCollectorError = CanonicalError`; resource-scoped builder `ModuleConfigError`): §2 Error Scenarios — `UsageEmitterError::ModuleNotConfigured` → `UsageEmitterError::NotFound` (built via `ModuleConfigError::not_found()`); `inst-cfg-rem-5a` — `UsageCollectorError::ModuleNotFound(module_name)` (nonexistent variant) → `UsageCollectorError::NotFound` (built via `ModuleConfigError::not_found(module_name)`); `inst-cfg-rem-6a` — clarify that infrastructure failures are surfaced as `UsageCollectorError::Internal` / `ServiceUnavailable` / `DeadlineExceeded` per the canonical taxonomy."
    - version: "1.5"
      date: "2026-05-08"
      changes:
        - "Replace legacy `UsageCollectorError` variant names with their `CanonicalError` equivalents across §2 Error Scenarios, §2 Steps (`inst-rem-3a / 4a / 6a / 7a / 9a / 10a`), §5 DoD mapping list, §5 Permanent credential rejection, and §6 AC: `Unavailable` → `ServiceUnavailable`; `PluginTimeout` (HTTP timeout) → `DeadlineExceeded`; `PluginTimeout` (HTTP 429) → `ResourceExhausted`; `PluginTimeout` (HTTP 5xx) → `ServiceUnavailable`; `AuthorizationFailed` (HTTP 401) → `ServiceUnavailable`. `UsageCollectorError = CanonicalError` no longer exposes `PluginTimeout` / `AuthorizationFailed` builders. `HandlerResult::Retry` vs `Reject` routing is preserved by `delivery_handler.rs` (`DeadlineExceeded` / `ResourceExhausted` / `ServiceUnavailable` → Retry; everything else → Reject)."
    - version: "1.4"
      date: "2026-05-08"
      changes:
        - "Ratify AuthN permanent-rejection retry semantics in `inst-rem-4 / 4a`: both transient AuthN failures and permanent credential rejection (`Unauthorized` / `NoPluginAvailable`) map to `Unavailable` and Retry; permanent rejection is NOT auto-dead-lettered — operator recovery via token-acquisition `WARN` logs and retry-rate metrics; §2 Error Scenarios and §5 DoD updated"
        - "Split 401 (Retry / fresh token) from 403 (Reject / `PermissionDenied`) across DoD mapping list, §2 Error Scenarios, and `inst-rem-11a` — 403 now dead-letters as `PermissionDenied`; `inst-rem-9` (401 retry) unchanged in identity"
        - "Remove `request_timeout` from §5 Configuration table and from `inst-init-5` — `UsageCollectorRestClientConfig` does not declare it; HTTP client built with `HttpClientConfig::default()`"
        - "Rename `base_url` → `collector_url` (typed `URL`, required, no default) across §2 / §5 / TLS sub-section / AC; trim-trailing-slash language replaced with explicit path-replacement semantics in `inst-init-5`"
        - "Flatten `client_id` / `client_secret` / `scopes` rows replaced with nested `oauth.{client_id,client_secret,scopes}` block in §5 Configuration; `inst-init-1` and Data Protection paragraph updated to `oauth.*` field names"
    - version: "1.3"
      date: "2026-04-29"
      changes:
        - "Document TLS/HTTPS requirements for base_url: add Security sub-section to §5 and AC item for startup behaviour on http:// scheme with non-localhost host (SEC-FDESIGN-004)"
    - version: "1.2"
      date: "2026-04-29"
      changes:
        - "Document at-least-once delivery idempotency semantics: add duplicate-delivery note to DoD §5 and flow error scenario, add AC item for idempotent upsert on idempotency_key (REL-FDESIGN-003)"
    - version: "1.1"
      date: "2026-04-29"
      changes:
        - "Add module_name URL percent-encoding requirement to inst-cfg-rem-3 (SEC-FDESIGN-003)"
    - version: "1.0"
      date: "2026-04-28"
      changes:
        - "Initial feature specification"
---

# Feature: REST Client & Remote Ingest Delivery

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Remote Usage Emission Flow](#remote-usage-emission-flow)
  - [Module Config Retrieval Flow (REST)](#module-config-retrieval-flow-rest)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [REST Client Module Initialization](#rest-client-module-initialization)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [`usage-collector-rest-client` Crate](#usage-collector-rest-client-crate)
  - [TLS/HTTPS Configuration](#tlshttps-configuration)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Applicability Notes](#7-non-applicability-notes)

<!-- /toc -->

- [x] `p1` - **ID**: `cpt-cf-usage-collector-featstatus-rest-ingest`
<!-- STATUS: IMPLEMENTED — all p1 DoD items and all CDSL blocks are [x]. -->

<!-- reference to DECOMPOSITION entry -->
- [x] `p1` - `cpt-cf-usage-collector-feature-rest-ingest`

## 1. Feature Context

### 1.1 Overview

Enables out-of-process usage sources to emit records using the same `UsageEmitterRuntimeV1` API as in-process sources by providing the `usage-collector-rest-client` crate, which delivers records to the collector gateway over HTTP with service-to-service bearer token authentication.

### 1.2 Purpose

Implements the remote delivery counterpart to Feature 1's in-process delivery path. Out-of-process sources retrieve `dyn UsageEmitterRuntimeV1` from `ClientHub` and drive the same three-layer Runtime / Factory / Emitter chain as in-process sources — `runtime.factory(MODULE_NAME).with_*().authorize(ctx, resource_id, resource_type)?` followed by `usage_record_builder()?.build()?` and `enqueue(record)` / `enqueue_in(db, record)`; only the outbox delivery hop differs — the background pipeline HTTP-POSTs each record to the gateway ingest endpoint instead of calling it in-process. The `usage-collector-rest-client` crate registers `dyn UsageEmitterRuntimeV1` in `ClientHub`, backed by `UsageCollectorRestClient` implementing `UsageCollectorClientV1`.

**Requirements**: `cpt-cf-usage-collector-fr-rest-ingestion`

**NFR targets**: See PRD §NFRs; `cpt-cf-usage-collector-nfr-recovery` constrains `outbox_backoff_max` to below 15 minutes.

**Principles**: `cpt-cf-usage-collector-principle-fail-closed`, `cpt-cf-usage-collector-principle-tenant-from-ctx`

**Constraints**: `cpt-cf-usage-collector-constraint-security-context`, `cpt-cf-usage-collector-constraint-modkit`, `cpt-cf-usage-collector-constraint-outbox-infra`

### 1.3 Actors

**Actors** (defined in PRD.md):

- `cpt-cf-usage-collector-actor-usage-source` — out-of-process usage source emitting records via the REST client

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md)
- **Dependencies**: `cpt-cf-usage-collector-feature-sdk-and-ingest-core`

## 2. Actor Flows (CDSL)

**Sequences**: `cpt-cf-usage-collector-seq-emit-remote`

### Remote Usage Emission Flow

- [x] `p1` - **ID**: `cpt-cf-usage-collector-flow-rest-ingest-remote-emit`

**Actor**: `cpt-cf-usage-collector-actor-usage-source` (out-of-process)

**Success Scenarios**:
- Outbox delivers record to gateway; gateway returns 204 No Content; outbox advances partition cursor

**Error Scenarios**:
- AuthN resolver unavailable → transient; outbox retries with exponential backoff
- Client credentials permanently rejected by AuthN resolver (`Unauthorized` / `NoPluginAvailable`) → mapped to `UsageCollectorError::ServiceUnavailable` (same as the transient case); outbox retries indefinitely. The message is **not** auto-dead-lettered — operators detect this via token-acquisition `WARN` logs and retry-rate metrics, then rotate or repair credentials to clear the retry loop
- Gateway returns 401 (token expired or invalid) → mapped to `UsageCollectorError::ServiceUnavailable`; transient — next attempt acquires a fresh bearer token
- Gateway returns 429 → mapped to `UsageCollectorError::ResourceExhausted`; transient — outbox retries with exponential backoff
- Gateway returns 5xx → mapped to `UsageCollectorError::ServiceUnavailable`; transient — outbox retries with exponential backoff
- Gateway returns 403 Forbidden (gateway PDP denied the forwarder's service identity) → permanent; message moved to dead-letter store as `PermissionDenied`
- Gateway returns any other 4xx (excluding 401, 403, and 429) → permanent; message moved to dead-letter store as `Internal`
- Gateway 204 on duplicate delivery (same `idempotency_key`) → idempotent; storage layer performs no-op upsert; outbox advances cursor normally

**Steps**:
1. [x] - `p1` - Outbox background pipeline calls REST client `UsageCollectorClientV1::create_usage_record(record)` for each ready outbox message - `inst-rem-1`
2. [x] - `p1` - For each outgoing HTTP request, the shared `BearerTokenAuthLayer` (a `tower::Layer` wired into the REST client's `HttpClient` via `HttpClientBuilder::with_auth_layer(...)`) acquires a bearer token from the platform AuthN resolver via client credentials flow and injects an `Authorization: Bearer <token>` header before the request reaches the network — token acquisition is **not** performed inline in `create_usage_record()`; the REST client method itself only issues `.post(url).json(&record).send()` - `inst-rem-2`
3. [x] - `p1` - **IF** the layer's call to `AuthNResolverClient::exchange_client_credentials` returns a transient error (service temporarily unreachable, network failure, or any error other than credential rejection) - `inst-rem-3`
   1. [x] - `p1` - The layer collapses the failure to `HttpError::Transport`; `rest_client.rs` maps the transport error to `UsageCollectorError::ServiceUnavailable`; outbox library applies exponential backoff retry. The layer also emits a `WARN` `tracing` event with the underlying error for operator monitoring - `inst-rem-3a`
4. [x] - `p1` - **IF** the layer's call to `AuthNResolverClient::exchange_client_credentials` rejects credentials as permanently invalid (`Unauthorized`) OR no AuthN plugin is registered (`NoPluginAvailable`) - `inst-rem-4`
   1. [x] - `p1` - The layer collapses the failure to `HttpError::Transport` — identical to the transient case in `inst-rem-3a` — and `rest_client.rs` returns `UsageCollectorError::ServiceUnavailable`; outbox library applies exponential backoff retry. Permanent rejection is **NOT** auto-dead-lettered: recovery is operator-driven via the layer's token-acquisition `WARN` log events and retry-rate metrics; operators MUST rotate or repair the configured credentials to clear the retry loop - `inst-rem-4a`
5. [x] - `p1` - HTTP POST `POST /usage-collector/v1/records` with the serialized `UsageRecord` JSON body — the `Authorization: Bearer <token>` header is injected by the `BearerTokenAuthLayer` per `inst-rem-2`, not by the REST client method; `subject` (`Option<Subject>`) and `metadata` serialize as absent JSON fields when `None` (not as `null`), and when `subject` is `Some` it is emitted as a nested object `"subject": { "id": "...", "type": "..." }` with the inner `type` itself absent when `Subject.r#type` is `None` - `inst-rem-5`
6. [x] - `p1` - **IF** HTTP request times out - `inst-rem-6`
   1. [x] - `p1` - **RETURN** `UsageCollectorError::DeadlineExceeded`; outbox retries with exponential backoff; `outbox_backoff_max` MUST be configured below 15 minutes to satisfy `cpt-cf-usage-collector-nfr-recovery` - `inst-rem-6a`
7. [x] - `p1` - **IF** HTTP transport error (connection refused, DNS failure, TLS error, or other non-timeout network error) - `inst-rem-7`
   1. [x] - `p1` - **RETURN** `UsageCollectorError::ServiceUnavailable`; outbox retries with exponential backoff - `inst-rem-7a`
8. [x] - `p1` - **IF** gateway returns 204 No Content - `inst-rem-8`
   1. [x] - `p1` - **RETURN** `Ok(())`; outbox advances partition cursor — record is confirmed delivered - `inst-rem-8a`
9. [x] - `p1` - **IF** gateway returns 401 Unauthenticated (token invalid or expired) - `inst-rem-9`
   1. [x] - `p1` - **RETURN** `UsageCollectorError::ServiceUnavailable`; outbox retries — next attempt acquires a fresh bearer token; no record is lost - `inst-rem-9a`
10. [x] - `p1` - **IF** gateway returns 429 Too Many Requests or any 5xx - `inst-rem-10`
    1. [x] - `p1` - **RETURN** `UsageCollectorError::ResourceExhausted` for 429 OR `UsageCollectorError::ServiceUnavailable` for 5xx; outbox retries with exponential backoff in both cases - `inst-rem-10a`
11. [x] - `p1` - **IF** gateway returns any other 4xx (excluding 401 and 429) - `inst-rem-11`
    1. [x] - `p1` - **RETURN** `UsageCollectorError::PermissionDenied` for 403 Forbidden (gateway PDP denied the forwarder's service identity) **OR** `UsageCollectorError::Internal` for any remaining 4xx; in both cases outbox moves message to dead-letter store and surfaces via monitoring - `inst-rem-11a`

### Module Config Retrieval Flow (REST)

- [x] `p2` - **ID**: `cpt-cf-usage-collector-flow-rest-ingest-fetch-module-config`

**Actor**: `cpt-cf-usage-collector-actor-usage-source`

**Success Scenarios**:
- Gateway returns `ModuleConfig` with the static `allowed_metrics` list for the requesting module

**Error Scenarios**:
- Module not registered in gateway static config → gateway returns 404; REST client returns `UsageCollectorError::NotFound` (built via `ModuleConfigError::not_found()`); the factory's `.authorize()` step surfaces `UsageEmitterError::NotFound`
- AuthN resolver temporarily unavailable, transport failure, gateway 401, or gateway 5xx → REST client returns `UsageCollectorError::ServiceUnavailable`; the factory's `.authorize()` step propagates it unchanged per `inst-authz-5a`
- Request deadline exceeded → REST client returns `UsageCollectorError::DeadlineExceeded` (built via `ModuleConfigError::deadline_exceeded()`); propagated unchanged per `inst-authz-5a`
- Gateway rate-limits the lookup (HTTP 429) → REST client returns `UsageCollectorError::ResourceExhausted` (built via `ModuleConfigError::resource_exhausted()`); propagated unchanged per `inst-authz-5a`
- Caller is rejected by the gateway (HTTP 403) → REST client returns `UsageCollectorError::PermissionDenied` (built via `ModuleConfigError::permission_denied()`); propagated unchanged per `inst-authz-5a`

**Steps**:
1. [x] - `p2` - During the factory's `.authorize()` step (phase 1), the emitter calls `UsageCollectorClientV1::get_module_config(module_name)` on the REST client - `inst-cfg-rem-1`
2. [x] - `p2` - The shared `BearerTokenAuthLayer` (the same Tower layer used by the remote-emit flow) acquires a bearer token from the platform AuthN resolver via client credentials flow and injects an `Authorization: Bearer <token>` header before the request reaches the network — `get_module_config()` itself only issues a plain `.get(url).send()` - `inst-cfg-rem-2`
3. [x] - `p2` - HTTP GET `GET /usage-collector/v1/modules/{module_name}/config`;
   the `Authorization: Bearer <token>` header is injected by the
   `BearerTokenAuthLayer` per `inst-cfg-rem-2`, not by the REST client method;
   `module_name` MUST be percent-encoded (URL percent-encoding) when
   interpolated into the URL path - `inst-cfg-rem-3`
4. [x] - `p2` - **IF** gateway returns 200 OK - `inst-cfg-rem-4`
   1. [x] - `p2` - Deserialize response body into `ModuleConfig`; **RETURN** `Ok(ModuleConfig { module_name, allowed_metrics })` - `inst-cfg-rem-4a`
5. [x] - `p2` - **IF** gateway returns 404 Not Found - `inst-cfg-rem-5`
   1. [x] - `p2` - **RETURN** `UsageCollectorError::NotFound` (built via `ModuleConfigError::not_found(module_name)`); emitter surfaces `UsageEmitterError::NotFound` - `inst-cfg-rem-5a`
6. [x] - `p2` - **IF** any other error (transport failure, 4xx other than 404, 5xx, timeout) - `inst-cfg-rem-6`
   1. [x] - `p2` - **RETURN** the matching canonical variant: 401 → `UsageCollectorError::ServiceUnavailable`; 403 → `UsageCollectorError::PermissionDenied` (built via `ModuleConfigError::permission_denied()`); 429 → `UsageCollectorError::ResourceExhausted` (built via `ModuleConfigError::resource_exhausted()`); 5xx → `UsageCollectorError::ServiceUnavailable`; `HttpError::Timeout` / `HttpError::DeadlineExceeded` → `UsageCollectorError::DeadlineExceeded` (built via `ModuleConfigError::deadline_exceeded()`); `HttpError::Transport(_)` (carrying both genuine transport failures and any AuthN-resolver-originated outcomes collapsed by the `BearerTokenAuthLayer`) and any residual non-Transport, non-Timeout `HttpError` variants → `UsageCollectorError::ServiceUnavailable`; any other unexpected HTTP status → `UsageCollectorError::Internal`. The factory's `.authorize()` step propagates the variant unchanged per `inst-authz-5a` so the source module can map a transient gateway outage to HTTP 503 / 504 (or rate-limit to HTTP 429) instead of an opaque 500 - `inst-cfg-rem-6a`

## 3. Processes / Business Logic (CDSL)

### REST Client Module Initialization

- [x] `p1` - **ID**: `cpt-cf-usage-collector-algo-rest-ingest-module-init`

**Input**: `ModuleCtx` (config, `ClientHub`, DB connection)

**Output**: `dyn UsageEmitterRuntimeV1` registered in `ClientHub`; outbox schema migrations registered via `DatabaseCapability`

**Steps**:
1. [x] - `p1` - Load and validate `UsageCollectorRestClientConfig` from `ModuleCtx`; required fields `oauth.client_id` and `oauth.client_secret` must be non-empty; fail module startup with descriptive error on missing or invalid config - `inst-init-1`
2. [x] - `p1` - Acquire DB connection from `ModuleCtx`; fail module startup if DB is not available - `inst-init-2`
3. [x] - `p1` - Retrieve `AuthNResolverClient` from `ClientHub`; fail module startup if not registered - `inst-init-3`
4. [x] - `p1` - Retrieve `AuthZResolverClient` from `ClientHub`; fail module startup if not registered - `inst-init-4`
5. [x] - `p1` - Construct `UsageCollectorRestClient` from config and `AuthNResolverClient`: build a `BearerTokenAuthLayer` from the resolver client and the configured `oauth.*` client credentials, then build `HttpClient` with default `HttpClientConfig` and inject the layer via `HttpClientBuilder::with_auth_layer(|svc| ServiceBuilder::new().layer(layer).service(svc).boxed_clone())` so every outbound request acquires a fresh bearer token and carries an `Authorization` header before reaching the network. Any path component of `collector_url` is replaced by the fixed REST routes (`/usage-collector/v1/records`, `/usage-collector/v1/modules/{module_name}/config`) — only the scheme + host[:port] are honored - `inst-init-5`
6. [x] - `p1` - Build `UsageEmitterRuntime` (layer 1) via `UsageEmitterRuntime::build(cfg.emitter, db, AuthZResolverClient, UsageCollectorRestClient)` — the runtime owns the source's outbox worker (`OutboxHandle`); the outbox background pipeline will call `create_usage_record()` on the REST client for each delivery attempt - `inst-init-6`
7. [x] - `p1` - Register `dyn UsageEmitterRuntimeV1` in `ClientHub` — out-of-process sources retrieve the runtime at initialization and drive the three-layer Runtime / Factory / Emitter chain via `runtime.factory(MODULE_NAME).with_*().authorize(ctx, resource_id, resource_type)?` then `usage_record_builder()?.build()? → enqueue(record)` / `enqueue_in(db, record)` - `inst-init-7`

**DatabaseCapability**: `migrations()` returns `modkit_db::outbox::outbox_migrations()` — this module owns the source's local outbox queue; the same schema migration set as the in-process path applies.

## 4. States (CDSL)

Not applicable for this feature. No new entity state machines are introduced by the REST client. `UsageRecord.status` transitions (`active` → `inactive`) remain owned by Feature 8. The outbox message lifecycle is managed by the `modkit-db` outbox library and is not a domain state machine defined here.

## 5. Definitions of Done

### `usage-collector-rest-client` Crate

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-rest-ingest-rest-client-crate`

The system **MUST** implement the `usage-collector-rest-client` crate providing:

- A flat module layout under `src/`: `config.rs` (public `UsageCollectorRestClientConfig`), `module.rs` (the `UsageCollectorRestClientModule`), and `infra/` containing `rest_client.rs` (the `UsageCollectorClientV1` implementation), `bearer_token_auth_layer.rs` (the Tower auth layer described below), plus tests and `test_support.rs`. The crate's public surface is exactly `pub use config::UsageCollectorRestClientConfig;` and `pub use infra::UsageCollectorRestClient;` — no separate `api/rest/` layer or hand-written request/response DTOs are exposed; the client serializes `usage_collector_sdk::models::UsageRecord` directly.
- `UsageCollectorRestClientModule` implementing `Module::init()`: loads `UsageCollectorRestClientConfig`, acquires `AuthNResolverClient` and `AuthZResolverClient` from `ClientHub`, constructs `UsageCollectorRestClient` (which builds the `BearerTokenAuthLayer` and the configured `HttpClient` internally), builds the layer-1 `UsageEmitterRuntime` via `UsageEmitterRuntime::build(cfg.emitter, db, authorization, collector)` backed by the REST client, and registers `dyn UsageEmitterRuntimeV1` in `ClientHub`. The factory and emitter layers (`UsageEmitterFactory`, `UsageEmitter`) are obtained per-call-site by source modules via `runtime.factory(MODULE_NAME).with_*().authorize(ctx, resource_id, resource_type)?`.
- `BearerTokenAuthLayer` (a `tower::Layer` defined in `src/infra/bearer_token_auth_layer.rs`) is the single place that performs token acquisition for outbound requests: for each call it invokes `AuthNResolverClient::exchange_client_credentials(&credentials)` with the configured `oauth.*` client credentials, reads the bearer token from the returned `SecurityContext`, and injects a sensitive `Authorization: Bearer <token>` header before the request reaches the network. All AuthN-resolver-originated outcomes — transient resolver failures, permanent credential rejection (`Unauthorized` / `NoPluginAvailable`), and the post-exchange missing-token guard when the returned `SecurityContext` carries no bearer token — are collapsed to `HttpError::Transport(Box<AuthNResolverError>)`; the layer also emits a `WARN` `tracing::warn!` event with the underlying error so operators can detect permanent rejection. In addition, an invalid token byte sequence (token text that cannot form a valid header value) yields `HttpError::InvalidHeaderValue`, which `UsageCollectorRestClient::create_usage_record()` maps to `ServiceUnavailable` via the residual non-Transport, non-Timeout arm. The layer is wired into the shared `HttpClient` via `HttpClientBuilder::with_auth_layer(|svc| ServiceBuilder::new().layer(layer).service(svc).boxed_clone())` in `UsageCollectorRestClient::new(...)`.
- `UsageCollectorRestClient` implementing `UsageCollectorClientV1::create_usage_record()`: HTTP-POSTs the serialized `UsageRecord` to `POST /usage-collector/v1/records` via the shared `HttpClient` — the `Authorization` header is injected by the `BearerTokenAuthLayer`, so the method body issues a plain `.post(url).json(&record)?.send().await`. Maps `204` to `Ok(())`; maps `401` to `ServiceUnavailable` (triggers Retry — next attempt acquires a fresh bearer token via the layer); maps `403` to `PermissionDenied` (triggers Reject — gateway PDP denied the forwarder's service identity); maps `429` to `ResourceExhausted` (triggers Retry); maps `5xx` to `ServiceUnavailable` (triggers Retry); maps other `4xx` to `Internal` (triggers Reject); maps `HttpError::Timeout` / `HttpError::DeadlineExceeded` to `DeadlineExceeded` (triggers Retry); maps `HttpError::Transport(_)` — which carries **both** genuine transport failures **and** all AuthN resolver errors (transient failures and permanent credential rejection alike, collapsed by the layer) — and any residual non-Transport, non-Timeout `HttpError` variants (`InvalidHeaderValue`, `BodyTooLarge`, `Tls`, …) to `ServiceUnavailable` (triggers Retry). Retry vs Reject routing is performed by `delivery_handler.rs`, which dispatches `DeadlineExceeded` / `ResourceExhausted` / `ServiceUnavailable` to `HandlerResult::Retry` and everything else to `HandlerResult::Reject`.
- `UsageCollectorRestClient` implementing `UsageCollectorClientV1::get_module_config()`: HTTP-GETs `GET /usage-collector/v1/modules/{module_name}/config` via the shared `HttpClient` — the `Authorization` header is injected by the `BearerTokenAuthLayer`, so the method body issues a plain `.get(url).send().await`; deserializes `200` into `ModuleConfig`; maps `404` to `NotFound` (built via `ModuleConfigError::not_found(module_name)`); maps `401` to `ServiceUnavailable`; maps `403` to `PermissionDenied` (built via `ModuleConfigError::permission_denied()`); maps `429` to `ResourceExhausted` (built via `ModuleConfigError::resource_exhausted()`); maps `5xx` to `ServiceUnavailable`; maps `HttpError::Timeout` / `HttpError::DeadlineExceeded` to `DeadlineExceeded` (built via `ModuleConfigError::deadline_exceeded()`); maps `HttpError::Transport(_)` and any residual non-Transport, non-Timeout `HttpError` variants to `ServiceUnavailable`; maps any other unexpected HTTP status to `Internal`. The factory's `.authorize()` step propagates these variants unchanged per Feature 1 `inst-authz-5a` so the source module can map a transient gateway outage to HTTP 503 / 504 (or rate-limit to HTTP 429) instead of an opaque 500.
- `DatabaseCapability::migrations()` returning `modkit_db::outbox::outbox_migrations()` — owns the source's local outbox schema.

**Implements**:
- `cpt-cf-usage-collector-flow-rest-ingest-remote-emit`
- `cpt-cf-usage-collector-flow-rest-ingest-fetch-module-config`
- `cpt-cf-usage-collector-algo-rest-ingest-module-init`
- `cpt-cf-usage-collector-component-rest-client`

**Constraints**: `cpt-cf-usage-collector-constraint-security-context`, `cpt-cf-usage-collector-constraint-modkit`, `cpt-cf-usage-collector-constraint-outbox-infra`

**Touches**:
- API: `POST /usage-collector/v1/records` (client-side HTTP delivery), `GET /usage-collector/v1/modules/{module_name}/config` (client-side config fetch)
- DB: outbox queue in source's local DB — same schema as in-process path (`cpt-cf-usage-collector-dbtable-outbox`)
- Entities: `UsageRecord`, `ModuleConfig`

**Configuration**:

| Parameter | Type | Default | Required | Notes |
|-----------|------|---------|----------|-------|
| `collector_url` | URL | — | Yes | Hierarchical URL (scheme + host[:port]); any path component is replaced by the REST client routes |
| `oauth` | `OAuthConfig` | — | Yes | Nested OAuth2 client-credentials block; see sub-rows below |
| `oauth.client_id` | string | — | Yes | OAuth2 client identifier |
| `oauth.client_secret` | secret string | — | Yes | Env-expanded; never logged |
| `oauth.scopes` | list\<string\> | `[]` | No | OAuth2 scopes; IdP defaults when empty |
| `emitter` | `UsageEmitterConfig` | defaults | No | Outbox/authorization tuning; `outbox_backoff_max` MUST be below 15 minutes |

**Delivery guarantees**: At-least-once delivery via the source's transactional outbox — identical to the in-process path. A bearer token is acquired on each delivery attempt, so expired tokens trigger retry with a fresh token and do not cause permanent record loss. All records durably committed to the source's local outbox are guaranteed to eventually reach the gateway. At-least-once delivery via the transactional outbox may produce duplicate `create_usage_record()` calls on retry after a network timeout or transient failure. The gateway **MUST** deduplicate repeated deliveries of the same record via idempotent upsert on `idempotency_key`; a second delivery of the same record **MUST** be a no-op at the storage layer.

**Observability**: Structured log events MUST be emitted for: token acquisition failure (`WARN`), delivery retry (`INFO`), dead-letter routing (`ERROR`). Outbox queue depth and delivery attempt count are surfaced via the same metrics as the in-process path. Bearer tokens and client secrets MUST NOT appear in log output.

**Permanent credential rejection**: When the AuthN resolver rejects the configured client credentials as permanently invalid (`Unauthorized`) or no AuthN plugin is registered (`NoPluginAvailable`), the REST client maps the failure to `UsageCollectorError::ServiceUnavailable` — the same mapping as the transient case — and the outbox retries indefinitely. Permanent rejection is **NOT** auto-dead-lettered; recovery is operator-driven. Operators MUST monitor token-acquisition `WARN` log events and retry-rate metrics, then rotate or repair the configured `oauth.client_id` / `oauth.client_secret` to clear the retry loop.

**Data Protection**: Bearer tokens are short-lived credentials managed by the AuthN resolver; this crate holds them only for the duration of a single delivery attempt and does not persist or cache them. `oauth.client_secret` is stored as `SecretString` and is not exposed in logs or error messages.

### TLS/HTTPS Configuration

- [x] `p1` - **ID**: `cpt-cf-dod-rest-ingest-tls-config`

The system **MUST** enforce the following transport security requirements for the `collector_url` configuration field:

- In production environments, `collector_url` **MUST** use the `https://` scheme. An `http://` scheme is permitted only when the host resolves to a localhost or loopback address (`127.0.0.1`, `::1`, or `localhost`) — exclusively for development and testing environments.
- TLS certificate validation is enforced by the HTTP client by default. Disabling certificate validation or trusting a self-signed certificate **MUST** require an explicit operator opt-in (for example, via a dedicated configuration flag); silent trust-all behaviour is prohibited.
- The module **MUST** emit a startup warning (`WARN` level) when `collector_url` uses the `http://` scheme with a non-localhost host, or **MUST** refuse to start with a descriptive configuration validation error, consistent with the platform security baseline.

**Implements**:
- `cpt-cf-usage-collector-dod-rest-ingest-rest-client-crate`

**Constraints**: `cpt-cf-usage-collector-constraint-security-context`

## 6. Acceptance Criteria

- [ ] Out-of-process source retrieves `dyn UsageEmitterRuntimeV1` from `ClientHub` after `usage-collector-rest-client` `init()` completes; the three-layer `runtime.factory(MODULE_NAME).with_*().authorize(ctx, resource_id, resource_type)?` chain and the subsequent `usage_record_builder()?.build()? → enqueue(record)` / `enqueue_in(db, record)` calls behave identically to the in-process path
- [ ] `BearerTokenAuthLayer` (a `tower::Layer` defined in `src/infra/bearer_token_auth_layer.rs`) is wired into the REST client's `HttpClient` via `HttpClientBuilder::with_auth_layer(...)`; every outbound request to the gateway carries an `Authorization: Bearer <token>` header obtained by the layer's call to `AuthNResolverClient::exchange_client_credentials(&credentials)` — neither `create_usage_record()` nor `get_module_config()` perform token acquisition or header injection inline
- [ ] The crate's public surface is exactly `pub use config::UsageCollectorRestClientConfig;` and `pub use infra::UsageCollectorRestClient;` — there is no separate `api/rest/` module, no hand-written request/response DTOs, and the client serializes `usage_collector_sdk::models::UsageRecord` directly
- [ ] Gateway 204 No Content causes `HandlerResult::Success`; outbox partition cursor advances and the outbox row is deleted
- [ ] Gateway 401 causes retry — next delivery attempt acquires a fresh bearer token; a temporarily expired token does not cause permanent record loss
- [ ] Gateway 429 causes `HandlerResult::Retry` via `ResourceExhausted`; gateway 5xx causes `HandlerResult::Retry` via `ServiceUnavailable`; outbox applies exponential backoff
- [ ] Gateway 403 Forbidden causes `HandlerResult::Reject` via `PermissionDenied`; message is moved to dead-letter store
- [ ] Gateway 4xx (excluding 401, 403, and 429) causes `HandlerResult::Reject` via `Internal`; message is moved to dead-letter store
- [ ] AuthN resolver transient failure (network error, service restart) causes `HandlerResult::Retry` via `ServiceUnavailable`; records in the source outbox are not lost
- [ ] AuthN resolver permanent credential rejection (`Unauthorized` / `NoPluginAvailable`) causes `HandlerResult::Retry` via `ServiceUnavailable` — the same mapping as the transient case — and the message is **not** auto-dead-lettered; recovery requires operator intervention via token-acquisition `WARN` logs and retry-rate metrics
- [ ] HTTP timeout causes `HandlerResult::Retry` via `DeadlineExceeded`; network transport errors cause `HandlerResult::Retry` via `ServiceUnavailable`
- [ ] `get_module_config()` returns `ModuleConfig` on gateway 200 OK; returns `NotFound` (built via `ModuleConfigError::not_found(module_name)`) on 404; returns `PermissionDenied` (built via `ModuleConfigError::permission_denied()`) on 403; returns `ResourceExhausted` (built via `ModuleConfigError::resource_exhausted()`) on 429; returns `ServiceUnavailable` on 401 and 5xx; returns `DeadlineExceeded` (built via `ModuleConfigError::deadline_exceeded()`) on HTTP timeout; the factory's `.authorize()` step propagates each variant unchanged per Feature 1 `inst-authz-5a`
- [ ] `DatabaseCapability::migrations()` registers outbox schema migrations; source's local outbox is created on module startup
- [ ] Missing or invalid `oauth.client_id` / `oauth.client_secret` configuration causes `init()` to fail with a descriptive error; the process does not start
- [ ] Bearer token and `oauth.client_secret` do not appear in log output or error messages
- [ ] `create_usage_record()` called twice with the same `idempotency_key` produces gateway 204 on both attempts; the second delivery is a no-op at the storage layer (idempotent upsert on `idempotency_key`)
- [ ] A `collector_url` configured with an `http://` scheme pointing to a non-localhost host either causes `init()` to fail with a descriptive configuration validation error or emits a `WARN`-level startup warning; this behaviour is documented in the crate README and is consistent with the platform security baseline (SEC-FDESIGN-004)

**Test data requirements**:
(1) AuthN resolver stub must support transient (`Unavailable`) and permanent (`Unauthorized`, `NoPluginAvailable`) error simulation.
(2) Gateway HTTP stub must be configurable to return 204, 401, 403, 429, 500, and 404 for `POST /records` and `GET /modules/{name}/config`.
(3) Integration tests verify the full outbox-to-gateway delivery path with the REST client transport using a gateway stub.

## 7. Non-Applicability Notes

**COMPL (Regulatory & Privacy Compliance)**: Not applicable. The REST client transmits the same opaque UUIDs and numeric values as the in-process path. Bearer tokens are short-lived and not stored. No regulated personal data is introduced by this crate.

**UX (User Experience & Accessibility)**: Not applicable. This feature provides a Rust library crate and a machine-to-machine HTTP client. There is no user-facing UI, no end-user interaction, and no accessibility requirements.

**State Management**: Not applicable. No new entity lifecycle or state machines are introduced. `UsageRecord.status` transitions remain owned by Feature 8 (Operator Operations).
