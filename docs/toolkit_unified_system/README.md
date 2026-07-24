<!-- Updated: 2026-04-07 by Constructor Tech -->

# ToolKit Architecture & Developer Guide

This folder contains the ToolKit developer documentation, split by topic for focused reading and LLM retrieval.

## How to use this folder

- **For humans**: Browse the file that matches your task (e.g., gear layout, DB access, OData, plugins).
- **For LLMs**: Start by reading this `README.md` only, then load the smallest set of sections/files needed for your task. Do not load unrelated docs.

## Task → Document routing

| Task / Goal | Primary file(s) to read | Related external docs |
|-------------|------------------------|----------------------|
| Adding a new gear | `02_gear_layout_and_sdk_pattern.md` | |
| Authentication, Authorization, SecureConn, AccessScope | `06_authn_authz_secure_orm.md` | |
| DB execution (transactions, migrations, repos) | `11_database_patterns.md` | |
| REST endpoint wiring, OperationBuilder | `04_rest_operation_builder.md` | |
| OData, $select, pagination, filtering | `07_odata_pagination_select_filter.md` | |
| ClientHub, inter-gear clients | `03_clienthub_and_plugins.md` | |
| Plugins, scoped clients, GTS | `03_clienthub_and_plugins.md` | `docs/TOOLKIT_PLUGINS.md` |
| Errors, RFC-9457 Problem | `05_errors_rfc9457.md` | |
| Lifecycle, background tasks, cancellation | `08_lifecycle_stateful_tasks.md` | |
| Out-of-Process / gRPC / SDK pattern | `09_oop_grpc_sdk_pattern.md` | |
| Domain model macro, DDD enforcement | `02_gear_layout_and_sdk_pattern.md` (§ Domain types) | Architecture lint DE0309 (via `cargo gears lint`) |
| Quick checklists, templates | `10_checklists_and_templates.md` | |
| Unit & integration testing (philosophy, patterns, infrastructure) | `12_unit_testing.md` | |
| E2E testing (philosophy, patterns, infrastructure) | `13_e2e_testing.md` | |
| HTTP client (TLS, retries, timeouts, concurrency, OTel tracing, auth hook) | | `docs/adrs/toolkit/0001-toolkit-hyper-tower-http-client.md` |
| AuthN/AuthZ, PolicyEnforcer, PEP enforcement | `06_authn_authz_secure_orm.md` | `docs/arch/authorization/DESIGN.md` |
| Authentication (inbound JWT/OIDC policies, outbound OAuth2 client-credentials) | | `docs/adrs/toolkit/0002-toolkit-auth-oauth2-client.md` |

## Core invariants (apply everywhere)

- **SDK pattern is the public API**: Use `<gear>-sdk` crate for traits, models, errors. Do not expose internals.
- **Secure-by-default DB access**: Use `SecureConn` + `AccessScope`. Gears cannot access raw database connections.
- **RFC-9457 errors everywhere**: Use `Problem` (implements `IntoResponse`). Do not use `ProblemResponse`.
- **Type-safe REST**: Use `OperationBuilder` with `.authenticated()` and `.standard_errors()`.
- **Config loading is explicit**: `ctx.config()` / `ctx.config_expanded()` require `gears.<name>.config`; use `ctx.config_or_default()` / `ctx.config_expanded_or_default()` only when missing config should fall back to `Default`.
- **OData macros are in `toolkit-odata-macros`**: Use `toolkit_odata_macros::ODataFilterable`.
- **ClientHub registration**: `ctx.client_hub().register::<dyn MyGearApi>(api)`; `ctx.client_hub().get::<dyn MyGearApi>()?`.
- **Cancellation**: Pass `CancellationToken` to background tasks for cooperative shutdown.
- **Domain model enforcement**: All `struct`/`enum` in `domain/` must have `#[domain_model]` (`toolkit_macros::domain_model`). CI lint DE0309 enforces this.
- **AuthZ via PolicyEnforcer**: Use `PolicyEnforcer` from `authz-resolver-sdk` for all authorization. Do not construct `AccessScope` manually — it must come from PDP constraints.
- **GTS schema**: Use `gts_schema_with_refs_as_string()` for faster, correct schema generation.

## File overview

- `00_gear_overview.md` – What the Gear is, core architecture, typical REST API request processing.
- `01_overview.md` – What ToolKit provides, core concepts, golden path.
- `02_gear_layout_and_sdk_pattern.md` – Gear directory layout, SDK crate, gear crate, re-exports.
- `03_clienthub_and_plugins.md` – Typed ClientHub, in-process vs remote clients, scoped clients, GTS-based plugin discovery.
- `04_rest_operation_builder.md` – OperationBuilder usage, auth, error registration, SSE, content types.
- `05_errors_rfc9457.md` – Problem error type, From impls, handler patterns, OpenAPI error registration.
- `06_authn_authz_secure_orm.md` – AuthN/AuthZ integration, PolicyEnforcer PEP pattern, pep_prop mapping, SecureConn, Scopable derive, CRUD authorization patterns.
- `07_odata_pagination_select_filter.md` – OData $filter/$orderby/$select, pagination, macro usage, field projection.
- `08_lifecycle_stateful_tasks.md` – WithLifecycle, cancellation tokens, stateful gear patterns.
- `09_oop_grpc_sdk_pattern.md` – Out-of-Process gears, gRPC, SDK pattern for OoP, client utilities.
- `10_checklists_and_templates.md` – Quick checklists per task, minimal code templates.
- `11_database_patterns.md` – DBRunner/SecureTx executors, transactions, repository pattern, database migrations.
- `12_unit_testing.md` – Philosophy, reliability principles, infrastructure, assert patterns, naming, priority matrix for unit/integration tests.
- `13_e2e_testing.md` – Philosophy, integration seams concept, pytest infrastructure, test patterns, anti-patterns for E2E tests.

### Related ADRs

- `docs/adrs/toolkit/0001-toolkit-hyper-tower-http-client.md` – **toolkit-http**: Hyper+Tower HTTP client with TLS, retries, timeouts, concurrency limiting, decompression, OTel tracing, and extensible auth layer hook.
- `docs/adrs/toolkit/0002-toolkit-auth-oauth2-client.md` – **toolkit-auth**: Inbound JWT/OIDC route-level policies and outbound OAuth2 client-credentials flow with automatic token refresh and `Authorization: Bearer` injection.
