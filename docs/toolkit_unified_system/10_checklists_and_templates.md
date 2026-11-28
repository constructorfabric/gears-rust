# Checklists and Templates

Each topic guide ends with its own checklist and keeps its code templates next to the explanation. This page holds the new-gear checklist and points to the rest.

## Adding a new gear

Reference implementation: `examples/toolkit/users-info`.

- [ ] Directory `gears/<name>/` (kebab-case; `make validate-gear-names`) with `<name>/` and `<name>-sdk/` crates. Package names under `gears/` are `cf-gears-<name>` and `cf-gears-<name>-sdk` (examples such as users-info use `<name>` / `<name>-sdk`). Layout: [02_gear_layout_and_sdk_pattern.md](02_gear_layout_and_sdk_pattern.md).
- [ ] SDK crate: client trait, models, GTS types, OData query types. New SDKs return `CanonicalError` from trait methods (re-exported under a gear alias), with an optional typed projection in `error.rs` per ADR 0005; some older SDKs (tenant-resolver, authn-resolver, usage-collector, policy-engine) still define their own error enums ([05_errors_rfc9457.md](05_errors_rfc9457.md)).
- [ ] Gear crate: `gear.rs`, `api/rest/` (DTOs, handlers, routes), `domain/`, `infra/storage/`.
- [ ] `#[toolkit::gear(name = "<name>", deps = [<crate_ident>, ...], capabilities = [...])]`. `deps` takes crate identifiers; prefer `#[toolkit::consumes(...)]` for contract dependencies ([03_clienthub_and_plugins.md](03_clienthub_and_plugins.md)).
- [ ] `#[domain_model]` on every `struct`/`enum` in `domain/` that is not strictly module-private (lint DE0309).
- [ ] `#[derive(Scopable)]` on SeaORM entities, with all four scope dimensions declared; queries go through `.secure().scope_with(&scope)` with a scope from `PolicyEnforcer` ([06_authn_authz_secure_orm.md](06_authn_authz_secure_orm.md)).
- [ ] Routes through `OperationBuilder` with an auth and a license decision ([04_rest_operation_builder.md](04_rest_operation_builder.md)).
- [ ] Register the local client in `init()`: `ctx.client_hub().register::<dyn MyGearApi>(api)`.
- [ ] Config: `ctx.config()` is strict, `ctx.config_or_default()` falls back to `Default`.
- [ ] Tests in sibling `*_tests.rs` files ([12_unit_testing.md](12_unit_testing.md)).
- [ ] Add both crates to the workspace `members` in the root `Cargo.toml`. If another gear lists yours in `#[toolkit::gear(deps = [...])]`, add the crate to `[workspace.metadata.cargo-shear] ignored`.
- [ ] To run it in the example server: an optional dependency plus a Cargo feature in `apps/cf-gears-example-server/Cargo.toml`, and a `#[cfg(feature = "...")] use <crate> as _;` line in `apps/cf-gears-example-server/src/registered_gears.rs`.
- [ ] Add a `gear.gdl` descriptor next to the gear crate ([GEARBOX.md](../GEARBOX.md)) and an entry in [GEARS.md](../GEARS.md).
- [ ] Iterate with `make gear-ci GEAR=<name>`; E2E with `make e2e-local GEAR=<name>`.

## Checklists by topic

| Task | Checklist |
|---|---|
| Gear layout, SDK crate | [02 § Quick checklist](02_gear_layout_and_sdk_pattern.md#quick-checklist) |
| ClientHub, plugins | [03 § Quick checklist](03_clienthub_and_plugins.md#quick-checklist), [ToolKit plugins](03_clienthub_and_plugins.md#plugins) |
| REST routes | [04 § Quick checklist](04_rest_operation_builder.md#quick-checklist) |
| Errors | [05 § Quick checklist](05_errors_rfc9457.md#quick-checklist) |
| AuthZ, secure ORM | [06 § Quick checklist](06_authn_authz_secure_orm.md#quick-checklist) |
| OData, pagination | [07 § Quick checklist](07_odata_pagination_select_filter.md#quick-checklist) |
| Lifecycle, background tasks | [08 § Quick checklist](08_lifecycle_stateful_tasks.md#quick-checklist) |
| Out-of-process gears | [09 § Quick checklist](09_oop_grpc_sdk_pattern.md#quick-checklist) |
| Repositories, transactions, migrations | [11 § Quick checklist](11_database_patterns.md#quick-checklist) |
| Unit and integration tests | [12_unit_testing.md](12_unit_testing.md) |
| E2E tests | [13_e2e_testing.md](13_e2e_testing.md) |

## Test security contexts

Service and repository tests need a `SecurityContext`. `users-info/src/test_support.rs` keeps the helpers:

```rust
pub fn ctx_for_subject(subject_id: Uuid, tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(subject_id)
        .subject_tenant_id(tenant_id)
        .build()
        .unwrap() // build() returns Result
}

pub fn ctx_deny_all() -> SecurityContext {
    SecurityContext::anonymous()
}
```

For tenant isolation, build one context per tenant and assert that a context for tenant A does not see rows seeded for tenant B. The same file has the in-memory DB, seeding, and mock PDP helpers (`inmem_db`, `seed_user`, `build_services_with_authz`); copy the pattern instead of writing new fixtures.
