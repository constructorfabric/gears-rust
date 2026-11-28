# Tasks — Canonical Error Migration

- [ ] `p1` - **ID**: `cpt-cf-errors-status-overall`

This document tracks the phased migration from the legacy error system (`Problem::new()` / `ErrDef` / `declare_errors!` / `ErrorCode`) to the canonical error architecture defined in [DESIGN.md](./DESIGN.md).

## Overview

This decomposition organizes the canonical error migration into sequential implementation phases (foundation, middleware, gear migration, cleanup, CI gates, docs) plus orthogonal workstreams. Each entry is structured so teams can execute incrementally while preserving traceability to PRD/DESIGN constraints.

Status summary: phases 1-4 are done except the GTS schema work (1.1.3, 1.1.4), the architecture lint rules (1.5.1-1.5.3), the JSON Schema tests (1.6.2, 1.6.4), the `insta` evaluation (1.6.3), the remaining SDK error enums (3.x.4), the last `gts/errors.json` file (4.2.6) and the grep check that waits on it (4.3.4). OAGW is ticked in phase 3 with an exception noted there. Phase 5 is partly done and phase 6 is open.

The implementation lives in `libs/toolkit-canonical-errors` (package `cf-gears-toolkit-canonical-errors`) and `libs/toolkit-canonical-errors-macro`.

## Entries

### Phase 1 — Foundation

- [ ] `p1` - **ID**: `cpt-cf-errors-feature-foundation`

Build the `CanonicalError` enum, context types, `Problem` mapping, and `#[resource_error]` macro in `libs/toolkit-canonical-errors`.

> Traces to: `cpt-cf-errors-component-canonical-error`, `cpt-cf-errors-component-context-types`, `cpt-cf-errors-component-rest-mapping`, `cpt-cf-errors-component-resource-error-macro`

#### 1.1 Core types

- [x] 1.1.1 Define `CanonicalError` enum with 16 variants (`libs/toolkit-canonical-errors/src/error.rs`)
- [x] 1.1.2 Define 16 per-category context types and the shared item types `FieldViolationV1`, `QuotaViolationV1`, `PreconditionViolationV1` (`src/context.rs`), with versioned naming (`XxxV1`) and unversioned aliases (`pub type NotFound = NotFoundV1;`). The types carry no `gts_type` field and no `extra` field (see DESIGN §3.8)
- [ ] 1.1.3 Implement `GtsSchema` for each of the 16 per-category context types and 3 shared item types via `#[struct_to_gts_schema]` macro
- [ ] 1.1.4 Implement `GtsSchema` for `CanonicalError` (oneOf schema with all 16 variants)
- [x] 1.1.5 Builder-returning constructors: 13 via `#[resource_error]`, plus `CanonicalError::internal`, `service_unavailable`, `unauthenticated` (`src/builder.rs`). Internal mutators `with_detail`, `with_resource_type`, `with_resource` are `pub(crate)`
- [x] 1.1.6 Accessors: `detail()`, `resource_type()`, `resource_name()`, `retry_after_seconds()`, `diagnostic()`, `http_status_override()`, `gts_type()`, `status_code()`, `title()`
- [x] 1.1.7 Implement `Display` and `std::error::Error` for `CanonicalError`

#### 1.2 REST mapping

> Traces to: `cpt-cf-errors-interface-problem-wire-format`, `cpt-cf-errors-constraint-rfc9457`

- [x] 1.2.1 Define `Problem` struct with RFC 9457 fields + `trace_id`, `context`, `error_code`, `error_domain` extensions (`src/problem.rs`)
- [x] 1.2.2 Implement `Problem::from_error()`
- [x] 1.2.3 Implement `From<CanonicalError> for Problem` delegating to `from_error()`
- [x] 1.2.5 (`p2`) Implement `TryFrom<Problem> for CanonicalError` for SDK round-trip (traces to `cpt-cf-errors-interface-problem-roundtrip`)

#### 1.3 Resource error macro

> Traces to: `cpt-cf-errors-component-resource-error-macro`, `cpt-cf-errors-constraint-macro-gts-construction`

- [x] 1.3.1 Implement `#[resource_error]` attribute macro in `libs/toolkit-canonical-errors-macro/`
- [x] 1.3.2 Generate 13 associated functions per annotated struct (all categories except `internal`, `service_unavailable`, `unauthenticated`), each returning a typestate `ResourceErrorBuilder`. `not_found`, `already_exists` and `data_loss` require `.with_resource(name)`
- [x] 1.3.3 Validate the GTS identifier at compile time — must parse as a GTS type ID ending in `~`. The macro does not check that the type is registered in the Types Registry

#### 1.4 Blanket `From` implementations

> Traces to: `cpt-cf-errors-fr-library-error-propagation`

- [x] 1.4.2 `From<sea_orm::DbErr> for CanonicalError` → `Internal` (feature `sea-orm`)
- [x] 1.4.4 `From<serde_json::Error> for CanonicalError` → `InvalidArgument`
- [x] 1.4.5 `From<std::io::Error> for CanonicalError` → `Internal`

`anyhow::Error` and `sqlx::Error` impls (former 1.4.1, 1.4.3) were dropped.

#### 1.5 `cargo gears lint` enforcement rules

> Traces to: `cpt-cf-errors-component-architecture-lint-rules`, PRD § 12 Risks — "LLM agents bypass compile checks"

- [ ] 1.5.1 Implement an architecture lint rule — **No direct `Problem` construction**: reject `Problem { ... }` struct literals and direct `IntoResponse` impls that bypass `CanonicalError`; all `Problem` instances must originate from `CanonicalError` via the `From` impl
- [ ] 1.5.2 Implement an architecture lint rule — **No legacy error patterns**: reject usage of `Problem::new()`, `ErrDef`, `declare_errors!`, or `ErrorCode`. The APIs are gone from the workspace (phase 4), so this only guards against their return
- [ ] 1.5.3 Implement an architecture lint rule — **No raw status-code error responses**: handlers must return `Result<T, CanonicalError>`, not ad-hoc HTTP error responses or gear-specific error enums
- [x] 1.5.4 `cargo gears lint` CI gate on all gear code (job `lint` in `.github/workflows/ci.yml`)

#### 1.6 Contract enforcement (Tier 2)

> Traces to: `cpt-cf-errors-fr-schema-drift-prevention`, `cpt-cf-errors-constraint-error-contract-stability`

- [x] 1.6.1 Showcase test for every category with full `assert_eq!` on Problem JSON (`libs/toolkit-canonical-errors/tests/showcase.rs`)
- [ ] 1.6.2 Write JSON Schema equality assertions for every context type
- [ ] 1.6.3 Evaluate `insta` snapshot testing as a replacement for inline `assert_eq!`
- [ ] 1.6.4 Add `jsonschema` cross-validation (serialize → validate against GTS schema); depends on 1.1.3

---

### Phase 2 — Error Middleware

- [x] `p2` - **ID**: `cpt-cf-errors-feature-error-middleware`

> Traces to: `cpt-cf-errors-component-error-middleware`, `cpt-cf-errors-fr-mandatory-trace-id`

Implemented as `toolkit::api::canonical_error_middleware` (`libs/toolkit/src/api/canonical_error_layer.rs`).

- [x] 2.1 Axum middleware for `CanonicalError` responses from handlers
- [x] 2.2 Set `trace_id` from the live OpenTelemetry span context, falling back to the W3C `traceparent` (never from `x-request-id` or a span handle); leave it absent when neither is in scope
- [x] 2.3 Set `instance` from the request URI path
- [x] 2.4 Catch-all: error responses never typed as `CanonicalError` are wrapped into a `Problem` (ADR 0006). Panics are handled in two ways. In OoP serving, `drain_guard_middleware` (`libs/toolkit/src/runtime/oop_serve.rs`) catches the panic and returns a `CanonicalError::internal(..)` response directly, so it is already a canonical Problem. In api-gateway, `CatchPanicLayer` produces a plain 500, which the middleware wraps as `internal` through the foreign-5xx path
- [x] 2.5 Log error details server-side at WARN/ERROR; the log-correlation formatter splices the live trace id onto the record for correlation

---

### Phase 3 — Gear Migration

- [ ] `p1` - **ID**: `cpt-cf-errors-feature-gear-migration`

Migrate each gear from legacy error types to `CanonicalError`. Every gear below declares `#[resource_error]` types. How errors reach the wire differs:

- Credstore, Simple User Settings, Nodes Registry, Types Registry, File Parser and Mini-Chat: REST handlers return `toolkit::api::ApiResult<T>` (`Result<T, CanonicalError>`).
- OAGW is the exception. Its management handlers (`gears/system/oagw/oagw/src/api/rest/handlers/upstream.rs`, `route.rs`) return `Result<impl IntoResponse, Box<Problem>>`, where the `Problem` is built from `CanonicalError` by `domain_error_to_problem` (`api/rest/error.rs`). The proxy handler (`handlers/proxy.rs`, `proxy_handler`) returns a plain `Response` rendered from `DomainError` by `error_response`. Both go through `CanonicalError`, but the handler signatures are not `ApiResult<T>`.
- Tenant Resolver, AuthZ Resolver, AuthN Resolver and API Gateway have no REST resource handlers of their own; their migration covers `#[resource_error]` types and SDK and plugin error mapping.

#### 3.1 Migration per gear

For each gear:

- [x] 3.x.1 Define `#[resource_error(...)] struct ResourceError;` for each resource type in the gear
- [x] 3.x.2 Replace `Problem::new()` / `ErrDef` / `ErrorCode` calls with `CanonicalError` / `ResourceError` constructors
- [x] 3.x.3 Replace `Result<T, Problem>` handler return types with `Result<T, CanonicalError>` — except OAGW, whose handlers still return `Box<Problem>` or `Response` built from `CanonicalError` (see above)
- [ ] 3.x.4 Remove gear-specific error enums that are now covered by canonical categories — SDK enums such as `ServiceGatewayError` (oagw-sdk), `CredStoreError` (credstore-sdk), `TenantResolverError`, `AuthZResolverError`, `AuthNResolverError` remain; decide per [ADR 0005](./ADR/0005-cpt-cf-adr-sdk-canonical-projection.md) which are opt-in projections and which should go
- [x] 3.x.5 Update gear tests to use canonical error matching

#### 3.2 Gear list

- [x] 3.2.1 OAGW (`gears/system/oagw/`) — handler signatures are not `ApiResult<T>`; see the exception above
- [x] 3.2.2 Credstore (`gears/credstore/`)
- [x] 3.2.3 Tenant Resolver (`gears/system/tenant-resolver/`)
- [x] 3.2.4 AuthZ Resolver (`gears/system/authz-resolver/`)
- [x] 3.2.5 AuthN Resolver (`gears/system/authn-resolver/`)
- [x] 3.2.6 Simple Resource Registry (`gears/simple-resource-registry/`) — docs only, no Rust code to migrate
- [x] 3.2.7 Simple User Settings (`gears/simple-user-settings/`)
- [x] 3.2.8 API Gateway (`gears/system/api-gateway/`)
- [x] 3.2.9 Nodes Registry (`gears/system/nodes-registry/`)
- [x] 3.2.10 Types Registry (`gears/system/types-registry/`)
- [x] 3.2.11 File Parser (`gears/file-parser/`)
- [x] 3.2.12 Mini-Chat (`gears/mini-chat/`)

---

### Phase 4 — Legacy Cleanup

- [ ] `p1` - **ID**: `cpt-cf-errors-feature-legacy-cleanup`

All gears are now on `CanonicalError`. Remove legacy infrastructure and finalize the `Problem` struct.

> Traces to: `cpt-cf-errors-constraint-error-contract-stability`

#### 4.1 Clean up Problem struct

- [x] 4.1.1 Remove `code: String` field from `Problem`
- [x] 4.1.2 Remove `errors: Option<Vec<ValidationViolation>>` field from `Problem`
- [x] 4.1.3 `context` is a non-optional `serde_json::Value` (deserializes to `{}` when absent)
- [x] 4.1.4 Remove `with_code()` builder method
- [x] 4.1.5 Remove `with_errors()` builder method
- [x] 4.1.6 Remove `Problem::new()` — all construction goes through `CanonicalError`
- [x] 4.1.7 Snapshot tests reflect the final Problem shape (`tests/showcase.rs`)

#### 4.2 Remove legacy error infrastructure

- [x] 4.2.1 Remove `ErrDef` (the legacy `libs/toolkit-errors` crate no longer exists)
- [x] 4.2.2 Remove the `declare_errors!` macro
- [x] 4.2.3 Remove `ValidationViolation` struct (replaced by `FieldViolation` in `InvalidArgument` and `OutOfRange` context types)
- [x] 4.2.4 Remove the legacy `ValidationError` and `ValidationErrorResponse` structs
- [x] 4.2.5 Remove `libs/toolkit/src/api/problem.rs` and its convenience constructors (`bad_request`, `not_found`, `conflict`, `internal_error`)
- [ ] 4.2.6 Remove all `gts/errors.json` files from gears and examples — one left: `gears/simple-user-settings/simple-user-settings/gts/errors.json` (no code reads it)
- [x] 4.2.7 Remove any remaining legacy `ErrorCode` references (the `ErrorCode` types left in `toolkit-durable-execution` and `event-broker` are unrelated domain types)

#### 4.3 Final verification

- [x] 4.3.1 `cargo build` — workspace compiles with no legacy error references. CI builds every workspace crate through `make clippy` (`cargo clippy --workspace --all-targets --all-features`, `.github/workflows/ci.yml`)
- [x] 4.3.2 `cargo test` — all tests pass. CI runs `make test-no-macros` (nextest `--workspace` minus the macro crates) in `.github/workflows/test-suite.yml`, called from `ci.yml`, and `make test-macros` for the macro crates
- [x] 4.3.3 Grep verification: zero occurrences of `Problem::new`, `ErrDef`, `declare_errors!`, legacy `ErrorCode`, `Problem::with_code()`, `with_errors()` in gear code
- [ ] 4.3.4 Grep verification: zero `gts/errors.json` files in repository (blocked on 4.2.6)

---

### Phase 5 — CI Gates & Semver

- [ ] `p1` - **ID**: `cpt-cf-errors-feature-ci-gates-semver`

> Traces to: `cpt-cf-errors-fr-schema-drift-prevention`

- [x] 5.1 `cargo-semver-checks` for `cf-gears-toolkit-canonical-errors` — runs in the release-plz workflow (`.github/workflows/release-plz.yml`) on published crates, not as a PR gate
- [ ] 5.2 Export GTS schemas to `schemas/*.json` and add CI step to diff against committed baselines (depends on 1.1.3)
- [ ] 5.3 Add `cargo insta test --check` to CI to reject unapproved snapshot changes (depends on 1.6.3)

---

### Phase 6 — Documentation Updates

- [ ] `p2` - **ID**: `cpt-cf-errors-feature-documentation-updates`

Update all documentation to reflect the canonical error architecture.

#### 6.1 ToolKit Unified System docs

- [ ] 6.1.1 Rewrite `docs/toolkit_unified_system/05_errors_rfc9457.md` — replace legacy patterns with `CanonicalError` categories, `#[resource_error]` macro, typed context structs, and `Result<T, CanonicalError>` handler return types
- [ ] 6.1.2 Update `docs/toolkit_unified_system/01_overview.md` — update error handling summary
- [ ] 6.1.3 Update `docs/toolkit_unified_system/03_clienthub_and_plugins.md` — update error handling in SDK error boundaries
- [ ] 6.1.4 Update `docs/toolkit_unified_system/04_rest_operation_builder.md` — update error registration examples
- [ ] 6.1.5 Update `docs/toolkit_unified_system/06_authn_authz_secure_orm.md` — update error handling for auth flows
- [ ] 6.1.6 Update `docs/toolkit_unified_system/07_odata_pagination_select_filter.md` — update OData validation to use `CanonicalError::invalid_argument` with `InvalidArgument` context
- [ ] 6.1.7 Update `docs/toolkit_unified_system/10_checklists_and_templates.md` — update error handling checklist and templates

#### 6.2 Architecture-level docs

- [ ] 6.2.1 Update `docs/ARCHITECTURE_MANIFEST.md` — update error handling section
- [ ] 6.2.2 Update `docs/REPO_PLAYBOOK.md` — update error handling standards references
- [ ] 6.2.3 Update `docs/GEARS.md` — update error mapping component descriptions

#### 6.3 Checklists

- [ ] 6.3.1 Update `studio-kit-gears/artifacts/DESIGN/checklist.md` — update error handling architecture checklist
- [ ] 6.3.2 Update `studio-kit-gears/artifacts/FEATURE/checklist.md` — update security error handling and error handling completeness
- [ ] 6.3.3 Update `studio-kit-gears/codebase/checklist.md` — update explicit error handling standards

---

### Separate Workstreams (Orthogonal)

The following items are independent of the canonical error migration and are tracked separately:

- **W3C trace ID extraction** — Done. `extract_trace_id` (`toolkit_trace_context`) resolves the live `opentelemetry` span context, falling back to the W3C `traceparent`, and no longer uses `tracing::Span::current().id()` (u64 span ID) or `x-request-id`.
- **gRPC transport mapping** — Done as an envelope, not as `From<CanonicalError> for tonic::Status`: the `Problem` travels in the `x-toolkit-problem-bin` trailer (`toolkit_transport_grpc::attach_problem` / `extract_problem`); clients recover `CanonicalError` via `toolkit_contract::grpc::map_tonic_status` and `From<TransportError> for CanonicalError` (`libs/toolkit-contract/src/runtime/canonical.rs`). A shared server-side category → status-code mapping does not exist.
- **SSE error event format** — Client side done: `libs/toolkit-contract/src/runtime/sse.rs` parses `event: error` frames as `Problem`. No shared server-side emitter.
