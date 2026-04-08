# Tasks — Canonical Error Migration

- [ ] `p1` - **ID**: `cpt-cf-errors-status-overall`

This document tracks the phased migration from the legacy error system (`Problem::new()` / `ErrDef` / `declare_errors!` / `ErrorCode`) to the canonical error architecture defined in [DESIGN.md](./DESIGN.md).

## Overview

This decomposition organizes the canonical error migration into sequential implementation phases (foundation, middleware, module migration, cleanup, CI gates, docs) plus orthogonal workstreams. Each entry is structured so teams can execute incrementally while preserving traceability to PRD/DESIGN constraints.

## Entries

### Phase 1 — Foundation

- [ ] `p1` - **ID**: `cpt-cf-errors-feature-foundation`

Build the `CanonicalError` enum, context types, `Problem` mapping, and `#[resource_error]` macro in `libs/modkit-errors`.

> Traces to: `cpt-cf-errors-component-canonical-error`, `cpt-cf-errors-component-context-types`, `cpt-cf-errors-component-rest-mapping`, `cpt-cf-errors-component-resource-error-macro`

#### 1.1 Core types

- [ ] 1.1.1 Define `CanonicalError` enum with 16 variants in `libs/modkit-errors/src/canonical.rs`
- [ ] 1.1.2 Define context types: `Validation`, `ResourceInfo`, `ErrorInfo`, `QuotaFailure`, `PreconditionFailure`, `DebugInfo`, `RetryInfo`, `RequestInfo` in `libs/modkit-errors/src/context.rs`
- [ ] 1.1.3 Implement `GtsSchema` for each context type via `#[struct_to_gts_schema]` macro
- [ ] 1.1.4 Implement `GtsSchema` for `CanonicalError` (oneOf schema with all 16 variants)
- [ ] 1.1.5 Implement ergonomic constructors (one per category) and builder methods (`with_message`, `with_resource_type`, `with_debug_info`)
- [ ] 1.1.6 Implement accessors: `message()`, `resource_type()`, `debug_info()`, `gts_type()`, `status_code()`, `title()`
- [ ] 1.1.7 Implement `Display` and `std::error::Error` for `CanonicalError`

#### 1.2 REST mapping

> Traces to: `cpt-cf-errors-interface-problem-wire-format`, `cpt-cf-errors-constraint-rfc9457`

- [ ] 1.2.1 Define `Problem` struct with RFC 9457 fields + `trace_id`, `context`, `debug` extensions
- [ ] 1.2.2 Implement `Problem::from_error()` (production — omits debug)
- [ ] 1.2.3 Implement `Problem::from_error_debug()` (debug — includes debug if present)
- [ ] 1.2.4 Implement `From<CanonicalError> for Problem` delegating to `from_error()`
- [ ] 1.2.5 Implement `TryFrom<Problem> for CanonicalError` for SDK round-trip

#### 1.3 Resource error macro

> Traces to: `cpt-cf-errors-component-resource-error-macro`, `cpt-cf-errors-constraint-macro-gts-construction`

- [ ] 1.3.1 Implement `#[resource_error("gts.cf.core.users.user.v1~")]` proc macro in `libs/modkit-errors-macro/`
- [ ] 1.3.2 Generate 15 associated functions per annotated struct (all categories except `service_unavailable`)
- [ ] 1.3.3 Validate GTS identifier at compile time — must be a valid GTS type ID registered in the Types Registry and must end with `~`

#### 1.4 Blanket `From` implementations

> Traces to: `cpt-cf-errors-fr-library-error-propagation`

- [ ] 1.4.1 `From<anyhow::Error> for CanonicalError` → `Internal`
- [ ] 1.4.2 `From<sea_orm::DbErr> for CanonicalError` → `Internal`
- [ ] 1.4.3 `From<sqlx::Error> for CanonicalError` → `Internal`
- [ ] 1.4.4 `From<serde_json::Error> for CanonicalError` → `InvalidArgument`
- [ ] 1.4.5 `From<std::io::Error> for CanonicalError` → `Internal`

#### 1.5 Dylint enforcement rules

> Traces to: PRD § 12 Risks — "LLM agents bypass compile checks"

- [ ] 1.5.1 Implement a dylint rule — reject any `Problem::new()`, `Problem { ... }` struct literals, or direct `IntoResponse` impls that bypass `CanonicalError`
- [ ] 1.5.2 Implement a dylint rule — reject module-specific error enums in handler return types (only `Result<T, CanonicalError>` allowed)
- [ ] 1.5.3 Implement a dylint rule — warn when constructing `ResourceInfo` manually in modules that have declared a `#[resource_error]` type for that GTS identifier
- [ ] 1.5.4 Add dylint CI gate to run on all module code

#### 1.6 Contract enforcement (Tier 2)

> Traces to: `cpt-cf-errors-fr-schema-drift-prevention`, `cpt-cf-errors-constraint-error-contract-stability`

- [ ] 1.6.1 Write showcase test for every category (16 tests, full `assert_eq!` on Problem JSON)
- [ ] 1.6.2 Write JSON Schema equality assertions for every context type
- [ ] 1.6.3 Evaluate `insta` snapshot testing as a replacement for inline `assert_eq!`
- [ ] 1.6.4 Add `jsonschema` cross-validation (serialize → validate against GTS schema)

---

### Phase 2 — Error Middleware

- [ ] `p1` - **ID**: `cpt-cf-errors-feature-error-middleware`

> Traces to: `cpt-cf-errors-component-error-middleware`, `cpt-cf-errors-fr-mandatory-trace-id`

- [ ] 2.1 Implement Axum error middleware that catches `CanonicalError` from handlers
- [ ] 2.2 Set `trace_id` from the current tracing span context or request headers (`x-trace-id`, `x-request-id`, `traceparent`)
- [ ] 2.3 Set `instance` from the request URI path
- [ ] 2.4 Select `from_error()` vs `from_error_debug()` based on application configuration
- [ ] 2.5 Catch panics and unhandled errors, wrap as `CanonicalError::internal(...)`
- [ ] 2.6 Log `debug_info` at WARN/ERROR with `trace_id` for correlation

---

### Phase 3 — Module Migration

- [ ] `p1` - **ID**: `cpt-cf-errors-feature-module-migration`

Migrate each module from legacy error types to `CanonicalError`. Can proceed in parallel per module once Phase 1 is merged.

#### 3.1 Migration per module

For each module:

- [ ] 3.x.1 Define `#[resource_error("gts.cf.{module}.{resource}.v1~")]` struct for each resource type in the module
- [ ] 3.x.2 Replace `Problem::new()` / `ErrDef` / `ErrorCode` calls with `CanonicalError` / `ResourceError` constructors
- [ ] 3.x.3 Replace `Result<T, Problem>` handler return types with `Result<T, CanonicalError>`
- [ ] 3.x.4 Remove module-specific error enums that are now covered by canonical categories
- [ ] 3.x.5 Update module tests to use canonical error matching

#### 3.2 Module list

- [ ] 3.2.1 OAGW (`modules/system/oagw/`)
- [ ] 3.2.2 Credstore (`modules/core/credstore/`)
- [ ] 3.2.3 Tenant Resolver (`modules/core/tenant-resolver/`)
- [ ] 3.2.4 AuthZ Resolver (`modules/core/authz-resolver/`)
- [ ] 3.2.5 AuthN Resolver (`modules/core/authn-resolver/`)
- [ ] 3.2.6 Simple Resource Registry (`modules/simple-resource-registry/`)
- [ ] 3.2.7 Simple User Settings (`modules/simple-user-settings/`)
- [ ] 3.2.8 API Gateway (`modules/system/api-gateway/`)
- [ ] 3.2.9 Nodes Registry (`modules/system/nodes-registry/`)
- [ ] 3.2.10 Types Registry (`modules/system/types-registry/`)
- [ ] 3.2.11 File Parser (`modules/file-parser/`)
- [ ] 3.2.12 Mini-Chat (`modules/mini-chat/`)

---

### Phase 4 — Legacy Cleanup

- [ ] `p1` - **ID**: `cpt-cf-errors-feature-legacy-cleanup`

All modules are now on `CanonicalError`. Remove legacy infrastructure and finalize the `Problem` struct.

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

- [ ] 4.2.1 Remove `ErrDef` struct from `libs/modkit-errors/src/catalog.rs`
- [ ] 4.2.2 Remove `declare_errors!` macro from `libs/modkit-errors-macro/`
- [ ] 4.2.3 Remove `ValidationViolation` struct (replaced by `FieldViolation` in `Validation` context)
- [ ] 4.2.4 Remove `ValidationError` and `ValidationErrorResponse` structs
- [ ] 4.2.5 Remove convenience constructors from `libs/modkit/src/api/problem.rs` (`bad_request`, `not_found`, `conflict`, `internal_error`)
- [ ] 4.2.6 Remove all `gts/errors.json` files from modules and examples
- [ ] 4.2.7 Remove any remaining `ErrorCode` enum references

#### 4.3 Final verification

- [ ] 4.3.1 `cargo build` — workspace compiles with no legacy error references
- [ ] 4.3.2 `cargo test` — all tests pass
- [ ] 4.3.3 Grep verification: zero occurrences of `Problem::new`, `ErrDef`, `declare_errors!`, `ErrorCode`, `with_code()`, `with_errors()` in module code
- [ ] 4.3.4 Grep verification: zero `gts/errors.json` files in repository

---

### Phase 5 — CI Gates & Semver

- [ ] `p1` - **ID**: `cpt-cf-errors-feature-ci-gates-semver`

> Traces to: `cpt-cf-errors-fr-schema-drift-prevention`

- [ ] 5.1 Add `cargo-semver-checks` to CI for `cf-modkit-errors` crate
- [ ] 5.2 Export GTS schemas to `schemas/*.json` and add CI step to diff against committed baselines
- [ ] 5.3 Add `cargo insta test --check` to CI to reject unapproved snapshot changes

---

### Phase 6 — Documentation Updates

- [ ] `p2` - **ID**: `cpt-cf-errors-feature-documentation-updates`

Update all documentation to reflect the canonical error architecture. Can run in parallel with Phases 2-4 once Phase 1 is merged.

#### 6.1 ModKit Unified System docs

- [ ] 6.1.1 Rewrite `docs/modkit_unified_system/05_errors_rfc9457.md` — replace legacy patterns with `CanonicalError` categories, `#[resource_error]` macro, typed context structs, and `Result<T, CanonicalError>` handler return types
- [ ] 6.1.2 Update `docs/modkit_unified_system/01_overview.md` — update error handling summary
- [ ] 6.1.3 Update `docs/modkit_unified_system/03_clienthub_and_plugins.md` — update error handling in SDK error boundaries
- [ ] 6.1.4 Update `docs/modkit_unified_system/04_rest_operation_builder.md` — update error registration examples
- [ ] 6.1.5 Update `docs/modkit_unified_system/06_authn_authz_secure_orm.md` — update error handling for auth flows
- [ ] 6.1.6 Update `docs/modkit_unified_system/07_odata_pagination_select_filter.md` — update OData validation to use `CanonicalError::invalid_argument` with `Validation` context
- [ ] 6.1.7 Update `docs/modkit_unified_system/10_checklists_and_templates.md` — update error handling checklist and templates

#### 6.2 Architecture-level docs

- [ ] 6.2.1 Update `docs/ARCHITECTURE_MANIFEST.md` — update error handling section
- [ ] 6.2.2 Update `docs/REPO_PLAYBOOK.md` — update error handling standards references
- [ ] 6.2.3 Update `docs/MODULES.md` — update error mapping component descriptions

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
