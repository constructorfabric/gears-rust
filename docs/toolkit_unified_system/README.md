<!-- Updated: 2026-04-07 by Constructor Tech -->

# ToolKit Architecture & Developer Guide

This folder contains the ToolKit developer documentation, split by topic for focused reading and LLM retrieval.

## How to use this folder

- **For humans**: Browse the file that matches your task (e.g., gear layout, DB access, OData, plugins).
- **For LLMs**: Start by reading this `README.md` only, then load the smallest set of sections/files needed for your task. Do not load unrelated docs.

## Task → Document routing

| Task / Goal | Primary file(s) to read | Related external docs |
|-------------|------------------------|----------------------|
| Onboarding: what a gear is, core concepts, golden path | `00_gear_overview.md`, `01_overview.md` | |
| Adding a new gear | `02_gear_layout_and_sdk_pattern.md` | |
| AuthZ scopes, secure ORM (`Scopable`, `.secure().scope_with()`), AccessScope | `06_authn_authz_secure_orm.md` | |
| DB execution (transactions, migrations, repos) | `11_database_patterns.md` | |
| REST endpoint wiring, OperationBuilder | `04_rest_operation_builder.md` | |
| OData, $select, pagination, filtering | `07_odata_pagination_select_filter.md` | |
| ClientHub, inter-gear clients | `03_clienthub_and_plugins.md` | |
| Plugins, scoped clients, GTS | `03_clienthub_and_plugins.md` § Plugins | |
| Errors (`CanonicalError`, RFC 9457 wire format) | `05_errors_rfc9457.md` | `docs/arch/errors/DESIGN.md` |
| Lifecycle, background tasks, cancellation | `08_lifecycle_stateful_tasks.md` | |
| Out-of-Process / gRPC / SDK pattern | `09_oop_grpc_sdk_pattern.md` | |
| Domain model macro, DDD enforcement | `02_gear_layout_and_sdk_pattern.md` (§ Domain types) | Architecture lint DE0309 (via `cargo gears lint`) |
| New-gear checklist, index of per-topic checklists, test security contexts | `10_checklists_and_templates.md` | |
| Unit & integration testing (philosophy, patterns, infrastructure) | `12_unit_testing.md` | |
| E2E testing (philosophy, patterns, infrastructure) | `13_e2e_testing.md` | |
| DB behavior testing & audit (rules with no exceptions, audit method; choices with a price in TRADEOFFS) | `14_db_behavior_testing.md` | `docs/arch/database/TRADEOFFS.md` |
| Test strategy, coverage thresholds, test commands, CI jobs, fuzzing | `15_testing_and_ci.md` | |
| HTTP client (TLS, retries, timeouts, concurrency, OTel tracing, auth hook) | | `docs/adrs/toolkit/0001-toolkit-hyper-tower-http-client.md` |
| Client circuit breaker (`toolkit-resilience`, generated contract clients) | | `docs/adrs/toolkit/0005-toolkit-client-circuit-breaker.md` |
| AuthN/AuthZ, PolicyEnforcer, PEP enforcement | `06_authn_authz_secure_orm.md` | `docs/arch/authorization/DESIGN.md` |
| Authentication (inbound JWT/OIDC policies, outbound OAuth2 client-credentials) | | `docs/adrs/toolkit/0002-toolkit-auth-oauth2-client.md` |
| Which control catches which class of defect (inverse lookup, before merge) | `16_defect_class_to_control_map.md` | |

## Core invariants (apply everywhere)

- **SDK pattern is the public API**: Use the `<gear>-sdk` crate for traits and models; trait methods of new SDKs return `Result<_, CanonicalError>` per [ADR 0005](../arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md); some existing SDKs (tenant-resolver-sdk `TenantResolverError`, authn-resolver-sdk `AuthNResolverError`, usage-collector-sdk `UsageCollectorError`, policy-engine-sdk `ManagementError`) still define their own error enums. Do not expose internals.
- **Gear declaration**: `#[toolkit::gear(name = "<kebab-case>", deps = [<crate_ident>, ...], ...)]`; depend on another gear's contract with `#[toolkit::consumes(contract = ..., from = "<gear>")]`.
- **Secure-by-default DB access**: Get a `DBProvider` from `ctx.db_required()`, then `.conn()` (`DbConn`) or `.transaction(|tx| ..)` (`DbTx`). Every query is scoped: `Entity::find().secure().scope_with(&scope)`, `secure_insert::<Entity>(..)`. Gears cannot access raw database connections.
- **Canonical errors everywhere**: Handlers return `ApiResult<T>` = `Result<T, CanonicalError>` from `toolkit::api::canonical_prelude`; one `impl From<DomainError> for CanonicalError` per gear. Responses are RFC 9457 `application/problem+json`.
- **Type-safe REST**: Use `OperationBuilder` with an auth call (`.authenticated()`), a license call (`.require_license_features(..)`), and declared error statuses (`.standard_errors()` or `.error_*`).
- **Config loading is explicit**: `ctx.config()` / `ctx.config_expanded()` require `gears.<name>.config`; use `ctx.config_or_default()` / `ctx.config_expanded_or_default()` only when missing config should fall back to `Default`.
- **OData macros are in `toolkit-odata-macros`**: Use `toolkit_odata_macros::ODataFilterable`.
- **ClientHub registration**: `ctx.client_hub().register::<dyn MyGearApi>(api)`; `ctx.client_hub().get::<dyn MyGearApi>()?`.
- **Cancellation**: Pass `CancellationToken` to background tasks for cooperative shutdown.
- **Domain model enforcement**: Every `struct`/`enum` in `domain/` that is not strictly module-private must have `#[domain_model]` (`toolkit_macros::domain_model`). CI lint DE0309 enforces this.
- **AuthZ via PolicyEnforcer**: Use `PolicyEnforcer` from `authz-resolver-sdk` for all authorization. Do not construct `AccessScope` manually — it must come from PDP constraints. The one exception is the `AccessScope::allow_all()` prefetch in the GET/UPDATE/DELETE flows described in `06_authn_authz_secure_orm.md`.
- **GTS schema**: Use `gts_schema_with_refs_as_string()` for faster, correct schema generation.

## File overview

- `00_gear_overview.md` – What the Gear is, core architecture, typical REST API request processing.
- `01_overview.md` – What ToolKit provides; condensed golden path from users-info.
- `02_gear_layout_and_sdk_pattern.md` – Gear directory layout, SDK crate, gear crate, re-exports.
- `03_clienthub_and_plugins.md` – Typed ClientHub, in-process vs remote clients, scoped clients, plugins (main gear, plugin gears, GTS publication, selection).
- `04_rest_operation_builder.md` – OperationBuilder usage, auth, error registration, SSE, content types.
- `05_errors_rfc9457.md` – `CanonicalError`, `#[resource_error]`, `From<DomainError>`, the error middleware and the `Problem` wire format.
- `06_authn_authz_secure_orm.md` – AuthN/AuthZ integration, PolicyEnforcer PEP pattern, pep_prop mapping, Scopable derive, secure query builders, CRUD authorization patterns.
- `07_odata_pagination_select_filter.md` – OData $filter/$orderby/$select, pagination, macro usage, field projection.
- `08_lifecycle_stateful_tasks.md` – WithLifecycle, cancellation tokens, stateful gear patterns.
- `09_oop_grpc_sdk_pattern.md` – Out-of-Process gears, gRPC, SDK pattern for OoP, client utilities.
- `10_checklists_and_templates.md` – New-gear checklist, index of per-topic checklists, test security contexts.
- `11_database_patterns.md` – `DBRunner`-generic repositories, transactions through `DBProvider`, CTEs, migrations.
- `12_unit_testing.md` – Philosophy, reliability principles, infrastructure, assert patterns, naming, priority matrix for unit/integration tests.
- `13_e2e_testing.md` – Philosophy, integration seams concept, pytest infrastructure, test patterns, anti-patterns for E2E tests.
- `14_db_behavior_testing.md` – DB-behavior testing as a third test layer: rules with no exceptions and the audit method; load- and guarantee-dependent choices are in `docs/arch/database/TRADEOFFS.md`.
- `15_testing_and_ci.md` – Test pyramid, coverage thresholds and where they are enforced, unit/integration/E2E/fuzz commands, CI jobs, architecture lints.
- `16_defect_class_to_control_map.md` – The inverse lookup: given a class of defect (tenant bypass, missing gate, global tables, wire/storage type mismatch, error classification, disclosure, in-process bypass, N+1, write-skew, API completeness, coverage that proves nothing), which control is supposed to catch it. Use it to find the class a defect belongs to, and to check that an applicable class has a control rather than an intention.

## Designs and ADRs

This folder explains how to write code against ToolKit. The architecture behind it lives in the designs below; the guides link to them and do not restate them.

| Area | Design | ADRs |
|------|--------|------|
| Authorization (AuthN/AuthZ, PDP/PEP, scopes) | [DESIGN](../arch/authorization/DESIGN.md), [usage scenarios](../arch/authorization/AUTHZ_USAGE_SCENARIOS.md) | [ADR](../arch/authorization/ADR/) |
| Errors (canonical categories, RFC 9457 wire format) | [DESIGN](../arch/errors/DESIGN.md), [categories](../arch/errors/categories/) | [ADR](../arch/errors/ADR/) |
| Out-of-process gears, deployment, discovery | [DESIGN](../arch/toolkit-oop/DESIGN.md) | [ADR](../arch/toolkit-oop/ADR/) |
| Contracts (REST/gRPC projection, generated clients) | [DESIGN](../arch/toolkit-contract-binding/DESIGN.md) | [ADR](../arch/toolkit-contract-binding/ADR/) |
| Secure ORM (CTEs, property graphs) | | [ADR](../arch/secure-orm/ADR/) |
| Database (namespacing, trade-offs) | [TRADEOFFS](../arch/database/TRADEOFFS.md) | [ADR](../arch/database/ADR/) |
| Throttling (api-gateway) | [DESIGN](../arch/throttling/DESIGN.md) | [ADR](../arch/throttling/ADR/) |
| ToolKit libraries (HTTP client, OAuth2, proxy, circuit breaker), test layout | | [toolkit](../adrs/toolkit/), [tests](../adrs/tests/) |
