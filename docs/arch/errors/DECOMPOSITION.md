# Tasks — Canonical Error Migration

- [ ] `p1` - **ID**: `cpt-cf-errors-status-overall`

This document tracks the phased migration from the legacy error system (`Problem::new()` / `ErrDef` / `declare_errors!` / `ErrorCode`) to the canonical error architecture defined in [DESIGN.md](./DESIGN.md).

## Overview

This decomposition organizes the canonical error migration into sequential implementation phases (foundation, middleware, gear migration, cleanup, CI gates, docs) plus orthogonal workstreams. Each entry is structured so teams can execute incrementally while preserving traceability to PRD/DESIGN constraints.

## Entries

### Phase 1 — Foundation

- [ ] `p1` - **ID**: `cpt-cf-errors-feature-foundation`

Build the `CanonicalError` enum, context types, `Problem` mapping, and `#[resource_error]` macro in `libs/toolkit-canonical-errors`.

> Traces to: `cpt-cf-errors-component-canonical-error`, `cpt-cf-errors-component-context-types`, `cpt-cf-errors-component-rest-mapping`, `cpt-cf-errors-component-resource-error-macro`

#### 1.1 Core types

- [ ] 1.1.1 Define `CanonicalError` enum with 16 variants in `libs/toolkit-errors/src/canonical.rs`
- [ ] 1.1.2 Define 16 per-category context types in `libs/toolkit-errors/src/`: `Cancelled`, `Unknown`, `InvalidArgument`, `DeadlineExceeded`, `NotFound`, `AlreadyExists`, `PermissionDenied`, `ResourceExhausted`, `FailedPrecondition`, `Aborted`, `OutOfRange`, `Unimplemented`, `Internal`, `ServiceUnavailable`, `DataLoss`, `Unauthenticated`; plus sub-types `FieldViolation`, `QuotaViolation`, `PreconditionViolation`. Use versioned naming (`XxxV1`) with unversioned type aliases (e.g., `pub type NotFound = NotFoundV1;`). Each type carries an internal `gts_type: GtsSchemaId` field (skipped during serialization) and a reserved `extra: Option<serde_json::Value>` field (always absent in p1, see DESIGN §3.8)
- [ ] 1.1.3 Implement `GtsSchema` for each of the 16 per-category context types and 3 sub-types via `#[struct_to_gts_schema]` macro
- [ ] 1.1.4 Implement `GtsSchema` for `CanonicalError` (oneOf schema with all 16 variants)
- [ ] 1.1.5 Implement ergonomic constructors (one per category: `CanonicalError::category(ctx)`) and builder methods (`with_message()`, `with_resource_type()`)
- [ ] 1.1.6 Implement accessors: `message()`, `resource_type()`, `gts_type()`, `status_code()`, `title()`
- [ ] 1.1.7 Implement `Display` and `std::error::Error` for `CanonicalError`

#### 1.2 REST mapping

> Traces to: `cpt-cf-errors-interface-problem-wire-format`, `cpt-cf-errors-constraint-rfc9457`

- [ ] 1.2.1 Define `Problem` struct with RFC 9457 fields + `trace_id`, `context` extensions
- [ ] 1.2.2 Implement `Problem::from_error()`
- [ ] 1.2.3 Implement `From<CanonicalError> for Problem` delegating to `from_error()`
- [ ] 1.2.5 (`p2`) Implement `TryFrom<Problem> for CanonicalError` for SDK round-trip — match GTS type URI against 16 known identifiers to dispatch to correct variant (traces to `cpt-cf-errors-interface-problem-roundtrip`)

#### 1.3 Resource error macro

> Traces to: `cpt-cf-errors-component-resource-error-macro`, `cpt-cf-errors-constraint-macro-gts-construction`

- [ ] 1.3.1 Implement `#[resource_error]` attribute macro in `libs/toolkit-canonical-errors-macro/`
- [ ] 1.3.2 Generate 15 associated functions per annotated struct (all categories except `service_unavailable`). For `not_found`, `already_exists`, and `data_loss`: take a single `impl Into<String>` (resource name) and construct the context type internally; other categories take the category-specific context type
- [ ] 1.3.3 Validate GTS identifier at compile time — must be a valid GTS type ID registered in the Types Registry and must end with `~`

#### 1.4 Blanket `From` implementations

> Traces to: `cpt-cf-errors-fr-library-error-propagation`

- [ ] 1.4.1 `From<anyhow::Error> for CanonicalError` → `Internal`
- [ ] 1.4.2 `From<sea_orm::DbErr> for CanonicalError` → `Internal`
- [ ] 1.4.3 `From<sqlx::Error> for CanonicalError` → `Internal`
- [ ] 1.4.4 `From<serde_json::Error> for CanonicalError` → `InvalidArgument`
- [ ] 1.4.5 `From<std::io::Error> for CanonicalError` → `Internal`

#### 1.5 `cargo gears lint` enforcement rules

> Traces to: `cpt-cf-errors-component-`cargo gears lint`-rules`, PRD § 12 Risks — "LLM agents bypass compile checks"

- [ ] 1.5.1 Implement a architecture lint rule — **No direct `Problem` construction**: reject `Problem { ... }` struct literals and direct `IntoResponse` impls that bypass `CanonicalError`; all `Problem` instances must originate from `CanonicalError` via the `From` impl
- [ ] 1.5.2 Implement a architecture lint rule — **No legacy error patterns**: reject usage of `Problem::new()`, `ErrDef`, `declare_errors!`, or `ErrorCode`
- [ ] 1.5.3 Implement a architecture lint rule — **No raw status-code error responses**: handlers must return `Result<T, CanonicalError>`, not ad-hoc HTTP error responses or gear-specific error enums
- [ ] 1.5.4 Add `cargo gears lint` CI gate to run on all gear code

#### 1.6 Contract enforcement (Tier 2)

> Traces to: `cpt-cf-errors-fr-schema-drift-prevention`, `cpt-cf-errors-constraint-error-contract-stability`

- [ ] 1.6.1 Write showcase test for every category (16 tests, full `assert_eq!` on Problem JSON)
- [ ] 1.6.2 Write JSON Schema equality assertions for every context type
- [ ] 1.6.3 Evaluate `insta` snapshot testing as a replacement for inline `assert_eq!`
- [ ] 1.6.4 Add `jsonschema` cross-validation (serialize → validate against GTS schema)

---

### Phase 2 — Error Middleware

- [ ] `p2` - **ID**: `cpt-cf-errors-feature-error-middleware`

> Traces to: `cpt-cf-errors-component-error-middleware`, `cpt-cf-errors-fr-mandatory-trace-id`

- [ ] 2.1 Implement Axum error middleware that catches `CanonicalError` from handlers
- [ ] 2.2 Set `trace_id` from the current tracing span context or request headers (`x-trace-id`, `x-request-id`, `traceparent`)
- [ ] 2.3 Set `instance` from the request URI path
- [ ] 2.4 Catch panics and unhandled errors, wrap as `CanonicalError::internal(...)` (catch-all behavior is out of scope for p1; deferred per PRD §4.2 and DESIGN §3.2 Error Middleware)
- [ ] 2.5 Log error details server-side at WARN/ERROR with `trace_id` for correlation

---

### Phase 3 — Gear Migration

- [ ] `p1` - **ID**: `cpt-cf-errors-feature-gear-migration`

Migrate each gear from legacy error types to `CanonicalError`. Can proceed in parallel per gear once Phase 1 is merged.

#### 3.1 Migration per gear

For each gear:

- [ ] 3.x.1 Define `#[resource_error("gts.cf.{gear}.{resource}.v1~")] struct ResourceError;` for each resource type in the gear
- [ ] 3.x.2 Replace `Problem::new()` / `ErrDef` / `ErrorCode` calls with `CanonicalError` / `ResourceError` constructors
- [ ] 3.x.3 Replace `Result<T, Problem>` handler return types with `Result<T, CanonicalError>`
- [ ] 3.x.4 Remove gear-specific error enums that are now covered by canonical categories
- [ ] 3.x.5 Update gear tests to use canonical error matching

#### 3.2 Gear list

- [ ] 3.2.1 OAGW (`gears/system/oagw/`)
- [ ] 3.2.2 Credstore (`gears/core/credstore/`)
- [ ] 3.2.3 Tenant Resolver (`gears/core/tenant-resolver/`)
- [ ] 3.2.4 AuthZ Resolver (`gears/core/authz-resolver/`)
- [ ] 3.2.5 AuthN Resolver (`gears/core/authn-resolver/`)
- [ ] 3.2.6 Simple Resource Registry (`gears/simple-resource-registry/`)
- [ ] 3.2.7 Simple User Settings (`gears/simple-user-settings/`)
- [ ] 3.2.8 API Gateway (`gears/system/api-gateway/`)
- [ ] 3.2.9 Nodes Registry (`gears/system/nodes-registry/`)
- [ ] 3.2.10 Types Registry (`gears/system/types-registry/`)
- [ ] 3.2.11 File Parser (`gears/file-parser/`)
- [ ] 3.2.12 Mini-Chat (`gears/mini-chat/`)

---

### Phase 4 — Legacy Cleanup

- [ ] `p1` - **ID**: `cpt-cf-errors-feature-legacy-cleanup`

All gears are now on `CanonicalError`. Remove legacy infrastructure and finalize the `Problem` struct.

> Traces to: `cpt-cf-errors-constraint-error-contract-stability`

#### 4.1 Clean up Problem struct

- [ ] 4.1.1 Remove `code: String` field from `Problem`
- [ ] 4.1.2 Remove `errors: Option<Vec<ValidationViolation>>` field from `Problem`
- [ ] 4.1.3 Verify `context` field is `serde_json::Value` (non-optional, already correct in canonical design)
- [ ] 4.1.4 Remove `with_code()` builder method
- [ ] 4.1.5 Remove `with_errors()` builder method
- [ ] 4.1.6 Remove or deprecate `Problem::new()` — all construction goes through `CanonicalError`
- [ ] 4.1.7 Update snapshot tests to reflect final Problem shape

#### 4.2 Remove legacy error infrastructure

- [ ] 4.2.1 Remove `ErrDef` struct from `libs/toolkit-errors/src/catalog.rs`
- [ ] 4.2.2 Remove `declare_errors!` macro from `libs/toolkit-errors-macro/`
- [ ] 4.2.3 Remove `ValidationViolation` struct (replaced by `FieldViolation` in `InvalidArgument` and `OutOfRange` context types)
- [ ] 4.2.4 Remove `ValidationError` and `ValidationErrorResponse` structs
- [ ] 4.2.5 Remove convenience constructors from `libs/toolkit/src/api/problem.rs` (`bad_request`, `not_found`, `conflict`, `internal_error`)
- [ ] 4.2.6 Remove all `gts/errors.json` files from gears and examples
- [ ] 4.2.7 Remove any remaining `ErrorCode` enum references

#### 4.3 Final verification

- [ ] 4.3.1 `cargo build` — workspace compiles with no legacy error references
- [ ] 4.3.2 `cargo test` — all tests pass
- [ ] 4.3.3 Grep verification: zero occurrences of `Problem::new`, `ErrDef`, `declare_errors!`, `ErrorCode`, `with_code()`, `with_errors()` in gear code
- [ ] 4.3.4 Grep verification: zero `gts/errors.json` files in repository

---

### Phase 5 — CI Gates & Semver

- [ ] `p1` - **ID**: `cpt-cf-errors-feature-ci-gates-semver`

> Traces to: `cpt-cf-errors-fr-schema-drift-prevention`

- [ ] 5.1 Add `cargo-semver-checks` to CI for `cf-gears-toolkit-errors` crate
- [ ] 5.2 Export GTS schemas to `schemas/*.json` and add CI step to diff against committed baselines
- [ ] 5.3 Add `cargo insta test --check` to CI to reject unapproved snapshot changes

---

### Phase 6 — Documentation Updates

- [ ] `p2` - **ID**: `cpt-cf-errors-feature-documentation-updates`

Update all documentation to reflect the canonical error architecture. Can run in parallel with Phases 2-4 once Phase 1 is merged.

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
- [ ] 6.2.3 Update `docs/GEARS.md ` — update error mapping component descriptions

#### 6.3 Checklists

- [ ] 6.3.1 Update `docs/checklists/DESIGN.md` — update error handling architecture checklist
- [ ] 6.3.2 Update `docs/checklists/FEATURE.md` — update security error handling and error handling completeness
- [ ] 6.3.3 Update `docs/checklists/CODING.md` — update explicit error handling standards

---

### Separate Workstreams (Orthogonal)

The following items are independent of the canonical error migration and will be tracked separately:

- **W3C trace ID extraction** — Replace `tracing::Span::current().id()` (u64 span ID) with a proper W3C trace ID from `opentelemetry` span context. Orthogonal; can be done before, during, or after migration.
- **gRPC transport mapping** — `From<CanonicalError> for tonic::Status`. Depends on Phase 1 completion.
- **SSE error event format** — Define error event structure for SSE streams. Depends on Phase 1 completion.
