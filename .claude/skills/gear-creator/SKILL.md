---
name: gear-creator
description: Interactive workflow for creating new ToolKit gears and editing existing ones. Use when adding a new gear to the platform, adding features to an existing gear (REST endpoints, DB entities, OData filtering, plugins, lifecycle/background tasks, SSE events, error types), refactoring gear layer structure, or creating SDK crates. Covers the full DDD-light stack — SDK pattern, API/domain/infra layers, OperationBuilder, SecureORM, ClientHub, error handling, and testing.
user-invocable: true
---

# ToolKit Gear Creator

Interactive workflow for creating and editing ToolKit gears.

## Canonical example

**`examples/toolkit/users-info/`** is the reference implementation. When unsure about patterns, read the corresponding file in users-info first.

- SDK crate: `examples/toolkit/users-info/users-info-sdk/src/`
- Gear crate: `examples/toolkit/users-info/users-info/src/`

## Document routing

Read the minimum set of docs for the task. The routing table is in `docs/toolkit_unified_system/README.md` (Task → Document routing); start there and do not load all docs at once.

## Workflow: Create a new gear

### Phase 1 — Requirements gathering

Ask the user (one message, not a wall of questions) or infer from the task or design documents:

1. **Gear name** (kebab-case, e.g. `file-storage`)
2. **Purpose** — what does this gear do?
3. **Capabilities needed** — which apply: `db`, `rest`, `stateful`?
4. **Dependencies** — does it depend on other gears? (e.g. `authz-resolver`, declared as `deps = [authz_resolver]` or `#[toolkit::consumes(...)]`)
5. **SDK needed?** — will other gears consume its API?

Use the answers to determine which docs to load next.

### Phase 2 — Design

1. Read `docs/toolkit_unified_system/02_gear_layout_and_sdk_pattern.md`
2. Read `docs/toolkit_unified_system/10_checklists_and_templates.md` — use the new-gear checklist
3. If capabilities include `rest` — also read `docs/toolkit_unified_system/04_rest_operation_builder.md`
4. If capabilities include `db` — also read `docs/toolkit_unified_system/06_authn_authz_secure_orm.md` and `docs/toolkit_unified_system/11_database_patterns.md`
5. Study corresponding files in `examples/toolkit/users-info/` for the layers being created

Present to the user:
- Proposed directory structure
- SDK trait definition (if SDK needed)
- Domain model outline
- REST endpoints list (if rest capability)
- DB entities list (if db capability)

Get approval before writing code.

### Phase 3 — Scaffold

Create files in this order (each layer builds on the previous):

1. **SDK crate** (if needed): `<gear>-sdk/` with `Cargo.toml`, `src/lib.rs` (re-exports `CanonicalError` under a gear alias), `src/client.rs`, `src/models.rs`; `src/gts.rs` and `src/odata/` when the gear has GTS types or OData queries
2. **Gear crate**: `<gear>/Cargo.toml`, `src/lib.rs`, `src/gear.rs`, `src/config.rs`, and `gear.gdl` next to `Cargo.toml` (`docs/GEARBOX.md`)
3. **Domain layer**: `src/domain/error.rs`, `src/domain/service/`, `src/domain/repos/`
4. **Infra layer** (if db): `src/infra/storage/entity/`, `src/infra/storage/mapper.rs`, `src/infra/storage/migrations/`
5. **API layer** (if rest): `src/api/rest/dto.rs`, `src/api/rest/handlers/`, `src/api/rest/routes/`, `src/api/rest/error.rs`
6. **Local client**: `src/domain/local_client/`

Tests go in sibling `*_tests.rs` files (`docs/toolkit_unified_system/12_unit_testing.md`).

### Phase 4 — Wire and register

1. Add the crate(s) to workspace `members` in the root `Cargo.toml`; if another gear lists this one in `deps`, add it to `[workspace.metadata.cargo-shear] ignored`
2. To run it in the example server: optional dependency + Cargo feature in `apps/cf-gears-example-server/Cargo.toml`, and `#[cfg(feature = "...")] use <crate> as _;` in `apps/cf-gears-example-server/src/registered_gears.rs`
3. Register the local client in `init()` via ClientHub
4. Add an entry to `docs/GEARS.md`

### Phase 5 — Verify

1. `make gear-ci GEAR=<gear>` — fmt, clippy (`-D warnings`) and tests for the gear, its SDK and the crates that depend on them
2. `make dylint` — architecture lints
3. `make e2e-local GEAR=<gear>` — if the gear has E2E suites

## Workflow: Edit an existing gear

### Step 1 — Understand scope

Ask the user what they want to change. Common tasks:

- Add a new REST endpoint
- Add a new DB entity
- Add OData filtering to an existing endpoint
- Add a new domain service method
- Add error variants
- Add lifecycle/background task
- Add plugin support

### Step 2 — Load relevant docs

Use the routing table in `docs/toolkit_unified_system/README.md`. Load only what's needed for the specific change.

### Step 3 — Study existing code

Read the gear's current implementation. Compare with `examples/toolkit/users-info/` for the same layer.

### Step 4 — Implement

Follow the DDD-light layer rules:

- **SDK models**: NO utoipa, NO HTTP types
- **API/REST DTOs**: MUST have `Serialize`, `Deserialize`, `ToSchema`; MUST be in `api/rest/`
- **Domain**: All non-module-private structs/enums (`pub`/`pub(crate)`/`pub(super)`) MUST have `#[domain_model]` (strictly module-private helpers are exempt)
- **Entities**: Use `#[derive(Scopable)]`; `#[secure(...)]` must decide all four dimensions (tenant, resource, owner, type), e.g. `#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]`, or `#[secure(unrestricted)]` for global tables
- **Endpoints**: MUST follow `/{service-name}/v{N}/{resource}` pattern
- **Errors**: handlers return `ApiResult<T>` (`CanonicalError`); implement `From<DomainError> for CanonicalError` in `api/rest/error.rs` with `#[resource_error]` types

### Step 5 — Verify

Same as Phase 5 of the create workflow.

## Key invariants

These rules apply to ALL gear work. Violating them will fail CI:

- SDK pattern is the public API — use `<gear>-sdk` crate
- `DBProvider` (`.conn()` / `.transaction(..)`) with `.secure().scope_with(&scope)` for all DB access — no raw connections
- `OperationBuilder` for all REST routes — with an auth call (`.authenticated()`), a license call (`.require_license_features(..)` or `.no_license_required()`) and declared errors (`.standard_errors()`)
- `#[domain_model]` on all non-module-private domain structs/enums (DE0309 lint; strictly private helpers exempt)
- No `unwrap()` / `expect()` — use proper Result types
- AuthZ via `PolicyEnforcer` PEP pattern — never construct `AccessScope` manually
