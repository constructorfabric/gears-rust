---
name: gear-creator
description: Interactive workflow for creating new ToolKit gears and editing existing ones. Use when adding a new gear to the platform, adding features to an existing gear (REST endpoints, DB entities, OData filtering, plugins, lifecycle/background tasks, SSE events, error types), refactoring gear layer structure, or creating SDK crates. Covers the full DDD-light stack — SDK pattern, contract/API/domain/infra layers, OperationBuilder, SecureORM, ClientHub, error handling, and testing.
user-invocable: true
---

# ToolKit Gear Creator

Interactive workflow for creating and editing ToolKit gears.

## Canonical example

**`examples/toolkit/users-info/`** is the reference implementation. When unsure about patterns, read the corresponding file in users-info first.

- SDK crate: `examples/toolkit/users-info/users-info-sdk/src/`
- Gear crate: `examples/toolkit/users-info/users-info/src/`

## Document routing

Read the minimum set of docs needed for the task. Start with the routing table below; do NOT load all docs at once.

| When you need to... | Read this |
|---|---|
| Understand ToolKit concepts, golden path | `docs/toolkit_unified_system/01_overview.md` |
| Create gear structure, SDK crate, naming, layers | `docs/toolkit_unified_system/02_gear_layout_and_sdk_pattern.md` |
| Wire ClientHub, inter-gear clients, plugins | `docs/toolkit_unified_system/03_clienthub_and_plugins.md` |
| Add REST endpoints, OperationBuilder, SSE, auth | `docs/toolkit_unified_system/04_rest_operation_builder.md` |
| Implement errors, RFC-9457 Problem, From impls | `docs/toolkit_unified_system/05_errors_rfc9457.md` |
| Add DB entities, SecureORM, AuthZ PEP | `docs/toolkit_unified_system/06_authn_authz_secure_orm.md` |
| Add repositories, migrations, transactions | `docs/toolkit_unified_system/11_database_patterns.md` |
| Add OData filtering, pagination, $select, $orderby | `docs/toolkit_unified_system/07_odata_pagination_select_filter.md` |
| Configure lifecycle, background tasks, cancellation | `docs/toolkit_unified_system/08_lifecycle_stateful_tasks.md` |
| Create out-of-process gear, gRPC, OoP SDK | `docs/toolkit_unified_system/09_oop_grpc_sdk_pattern.md` |
| Get checklists, code templates, test patterns | `docs/toolkit_unified_system/10_checklists_and_templates.md` |
| Write unit tests, test file layout, mocks, fixtures | `docs/toolkit_unified_system/12_unit_testing.md` |
| Write E2E tests, cross-gear integration tests | `docs/toolkit_unified_system/13_e2e_testing.md` |

## Workflow: Create a new gear

### Phase 1 — Requirements gathering

Ask the user (one message, not a wall of questions) or infer from the task or design documents:

1. **Gear name** (kebab-case, e.g. `file-storage`)
2. **Purpose** — what does this gear do?
3. **Capabilities needed** — which apply: `db`, `rest`, `stateful`?
4. **Dependencies** — does it depend on other gears? (e.g. `authz-resolver`)
5. **SDK needed?** — will other gears consume its API?

Use the answers to determine which docs to load next.

### Phase 2 — Design

1. Read `docs/toolkit_unified_system/02_gear_layout_and_sdk_pattern.md`
2. Read `docs/toolkit_unified_system/10_checklists_and_templates.md` — use the "Adding a New Gear" checklist
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

1. **SDK crate** (if needed): `<gear>-sdk/` with `Cargo.toml`, `src/lib.rs`, `src/client.rs`, `src/models.rs`, `src/errors.rs`
2. **Gear crate**: `<gear>/Cargo.toml`, `src/lib.rs`, `src/gear.rs`, `src/config.rs`
3. **Contract layer**: `src/contract/` — re-export from SDK or define inline
4. **Domain layer**: `src/domain/error.rs`, `src/domain/service/`, `src/domain/repos/`
5. **Infra layer** (if db): `src/infra/storage/entity/`, `src/infra/storage/mapper.rs`, `src/infra/storage/migrations/`
6. **API layer** (if rest): `src/api/rest/dto.rs`, `src/api/rest/handlers/`, `src/api/rest/routes/`, `src/api/rest/error.rs`
7. **Local client**: `src/domain/local_client/`

### Phase 4 — Wire and register

1. Add gear crate(s) to workspace `Cargo.toml`
2. Register gear in `apps/cf-gears-example-server/src/main.rs`
3. Register client in `init()` via ClientHub
4. Add feature flag if needed (see existing `static-authn` / `static-authz` pattern)

### Phase 5 — Verify

1. `cargo build -p <gear-crate>` — must compile
2. `cargo clippy -p <gear-crate>` — no warnings
3. `make dylint` — architecture lints pass
4. `cargo test -p <gear-crate>` — tests pass

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

Use the document routing table above. Load only what's needed for the specific change.

### Step 3 — Study existing code

Read the gear's current implementation. Compare with `examples/toolkit/users-info/` for the same layer.

### Step 4 — Implement

Follow the DDD-light layer rules:

- **Contract**: NO serde, NO utoipa, NO HTTP types
- **API/REST DTOs**: MUST have `Serialize`, `Deserialize`, `ToSchema`; MUST be in `api/rest/`
- **Domain**: All non-module-private structs/enums (`pub`/`pub(crate)`/`pub(super)`) MUST have `#[domain_model]` (strictly module-private helpers are exempt)
- **Entities**: Use `#[derive(Scopable)]` with `#[secure(tenant_col = "...")]`
- **Endpoints**: MUST follow `/{service-name}/v{N}/{resource}` pattern
- **Errors**: Use `Problem` (RFC-9457), implement `From<DomainError> for Problem`

### Step 5 — Verify

Same as Phase 5 of the create workflow.

## Key invariants

These rules apply to ALL gear work. Violating them will fail CI:

- SDK pattern is the public API — use `<gear>-sdk` crate
- `SecureConn` + `AccessScope` for all DB access — no raw connections
- `OperationBuilder` for all REST routes — with `.authenticated()` and `.standard_errors()`
- `#[domain_model]` on all non-module-private domain structs/enums (DE0309 lint; strictly private helpers exempt)
- No `unwrap()` / `expect()` — use proper Result types
- AuthZ via `PolicyEnforcer` PEP pattern — never construct `AccessScope` manually
