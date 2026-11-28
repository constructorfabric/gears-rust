# CODE Rules (FEATURE-led)

**Artifact**: CODE
**Kit**: gears

**Dependencies**:
- the source FEATURE artifact — the implementation contract
- `{codebase_checklist}` — code review criteria, review-only

---

## Prerequisites

### Load Dependencies
- [ ] Load the source FEATURE and the DESIGN, ADR, PRD, and UPSTREAM_REQS IDs it links; they are the implementation contract
- [ ] Resolve the source FEATURE location from `@cpt-*` markers or the request; stop and ask when it cannot be resolved
- [ ] Determine the traceability mode from the FEATURE or `artifacts.toml` (FULL or DOCS-ONLY)
- [ ] Leave `{codebase_checklist}` to the review stage; authoring does not load it
- [ ] Run `{ensure_dna_script}` before relying on `{dna_root}/languages/RUST.md`/`{dna_root}/REST/` content

---

## Requirements

### Structural
- [ ] Follow the gear layout in `{toolkit_unified_system}/02_gear_layout_and_sdk_pattern.md`: SDK crate for public contracts, gear crate for domain, API, and infrastructure. When `{project_paths}`'s `reference_gear_path` is non-empty, study it for the layer being touched (its `<name>-sdk/src/` and `<name>/src/` crates) as the canonical reference implementation; when it's empty, this project has no single canonical example gear — follow the layout chapter on its own
- [ ] Route to the matching `{toolkit_unified_system}` chapter for the surface being touched: `00_gear_overview.md`/`01_overview.md` when unfamiliar with a gear's DDD-light layering or the golden path, `03_clienthub_and_plugins.md` for ClientHub/inter-gear clients/plugins, `04_rest_operation_builder.md` for REST endpoints/OperationBuilder/SSE, `05_errors_rfc9457.md` for error types/Problem/`From` impls, `06_authn_authz_secure_orm.md` for DB entities/SecureORM/AuthZ, `07_odata_pagination_select_filter.md` for OData/pagination, `08_lifecycle_stateful_tasks.md` for lifecycle/background tasks, `09_oop_grpc_sdk_pattern.md` for out-of-process gears/gRPC, `10_checklists_and_templates.md` when scaffolding a brand-new gear, `11_database_patterns.md` for repositories/transactions/migrations/CTEs (distinct from `06`'s SecureORM/AuthZ scope), `12_unit_testing.md`/`13_e2e_testing.md` for test strategy and TDD anchoring, `14_db_behavior_testing.md` for DB-execution concerns (transactions, retries, migrations, pagination order, concurrent locking) on any slice touching the database
- [ ] For the specific `{repo_arch}` subsystem precedent behind the chapter just routed to (deeper than that chapter's own linked docs): `errors/{DESIGN.md,PRD.md,categories/}` to choose among the canonical wire error categories rather than inventing an ad hoc one, `authorization/{TENANT_MODEL.md,RESOURCE_GROUP_MODEL.md,PERMISSION_GTS_TYPE.md}` for tenant/resource-group/permission modeling beyond `06`'s own linked `DESIGN.md`/`AUTHZ_USAGE_SCENARIOS.md`, `toolkit-oop/DESIGN.md` and `toolkit-contract-binding/DESIGN.md` (each directory's `PRD.md`/`ADR/*` for deeper rationale) for out-of-process/gRPC/cross-gear contract work, `database/TRADEOFFS.md` for schema/namespacing decisions on a new gear, and `throttling/DESIGN.md` when the slice needs rate limiting (no `{toolkit_unified_system}` chapter covers throttling today)
- [ ] Follow cross-gear dependency rules, `SecurityContext` propagation, and middleware boundaries from `{gears_md}`
- [ ] Follow `{guidelines_security}`, `{gears_security}`, and `{guidelines_dependencies}`
- [ ] Follow repo Rust conventions from `{dna_root}/languages/RUST.md`
- [ ] If the gear exposes HTTP/REST or SSE endpoints, follow `{dna_root}/REST/` for status codes, versioning, batching, and querying conventions
- [ ] Follow `{testing_policy}` for test strategy, coverage requirements, and which checks must be green locally before a PR
- [ ] Add `@cpt-begin` and `@cpt-end` markers for implemented CDSL IDs when traceability mode is FULL
- [ ] Generate marker IDs only from existing FEATURE CDSL IDs; never invent implementation-only CPT IDs
- [ ] Keep markers minimal, correctly nested, and attached to the code that realizes the referenced behavior

### Semantic
- [ ] Implement every in-scope flow step, algorithm requirement, state transition, and definition-of-done item
- [ ] Preserve PRD coverage outcomes and DESIGN principles, constraints, components, sequences, data contracts, and security requirements in code behavior
- [ ] Preserve SDK-first public contracts (the `<gear>-sdk` crate is the public API), domain/API/infrastructure separation, `SecureConn`+`AccessScope` for all DB access (no raw connections), AuthZ via the `PolicyEnforcer` PEP pattern (never construct `AccessScope` manually), `OperationBuilder` for all REST routes with `.authenticated()` and `.standard_errors()`, `#[domain_model]` on every non-module-private domain struct/enum (DE0309 lint; strictly module-private helpers are exempt), and safe wire errors unless the source FEATURE or DESIGN documents an approved deviation — these are `{toolkit_unified_system}/00_gear_overview.md`'s and `01_overview.md`'s core invariants, not kit-invented rules; consult them directly for the full rationale on any one
- [ ] Never use `unwrap()`/`expect()` in gear code outside tests; use proper `Result` types
- [ ] Record review evidence for work touching registry, autodetect, or ignore matching, privilege boundaries, Secure ORM, SecurityContext, secrets, FIPS behavior (see `{fips_requirements}`), or other security-boundary logic: the touched guardrail or deviation, rationale, owner, and validation performed
- [ ] Add compile-fail tests when the code exposes compile-time guarantees (macro diagnostics, generated code contracts, type-state APIs, security or type-system invariants) and a compile-fail harness exists; otherwise state why the gate does not apply
- [ ] Never broaden scope beyond the source FEATURE without an explicit upstream artifact change

---

## Tasks

### Content Creation
- [ ] Split the FEATURE into implementation slices from its flow, algorithm, state, and definition-of-done IDs; each slice names the IDs it implements
- [ ] Order slices by dependency and user-observable behavior, each independently testable
- [ ] Implement one slice at a time with TDD, anchored the way `{toolkit_unified_system}/12_unit_testing.md` and `13_e2e_testing.md` define the split: a failing E2E-shaped check first for the slice's user-observable behavior (one call per API method across the whole gear suite — see `13_e2e_testing.md`'s Coverage Goal; reuse an existing call instead of adding a new one when the method is already exercised — except when this slice's own behavior is a distinct observable outcome an existing call can't exercise, such as an error or authorization denial that needs its own inputs; that's a rare exception for a genuine integration seam unit tests can't reach, per the Coverage Goal's own carve-out, not routine per-handler/per-edge-case coverage), then unit tests underneath for the domain logic, prioritized P1 (critical business invariants) first per `12_unit_testing.md`'s Priority Matrix — not exhaustive per-handler/per-edge-case unit coverage. See `{e2e_guide}` for how to actually run the E2E suite (`make e2e-local SUITE=<name>`/`GEAR=<name>`)
- [ ] Before adding a new test-support/fixture module, check the gear's existing `tests/common/mod.rs` (or equivalent shared helpers module) for one that already covers the same setup (DB, SecurityContext, PolicyEnforcer, etc.) and extend it instead of creating a parallel one — this is a real, observed failure mode, not a hypothetical (gears-rust#5322: a prior `cf-gears-implement` run produced 6 separate ~1000-line fixture modules where a hand-written reference used one ~1460-line shared module)
- [ ] When implementing the gear's first slice and no shared test-support module exists yet, deliberately create one gear-wide and reusable (e.g. `tests/common/mod.rs`) rather than a slice-scoped helper — scaffold the shared seam up front instead of only catching the duplication reactively once a second slice needs the same setup (gears-rust#5322's suggested fix also names this as the preferred prevention, not just the reactive check above)
- [ ] Organize test files by production module/concern (`<entity>_service_test.rs`, `api_rest_test.rs`, one `tests/common/mod.rs`), per `{toolkit_unified_system}/12_unit_testing.md`'s Test File Organization — never one file per handler or per edge-case cluster (gears-rust#5323: observed ~18 separate `routes_*_tests.rs` files for one `routes.rs` module, versus idiomatic colocated/per-module tests in hand-written gears in this workspace)
- [ ] Preserve existing behavior outside the current slice and the requested FEATURE scope

### IDs and Structure
- [ ] Mark the FEATURE checkboxes of the CDSL IDs a slice implements as `[x]` in the author stage, once the slice's tests pass; `cfs validate` reports an implemented but unchecked ID as an error
- [ ] Leave inspection-only definition-of-done items unchecked until the close stage confirms them
- [ ] Preserve existing stable IDs and markers; move markers only with the code they describe
- [ ] Never introduce orphan, duplicate, stale, or speculative `@cpt-*` markers

---

## Validation

### Structural
- [ ] `make gear-ci GEAR=<gear>` (fmt, clippy with the workspace lint set, tests) and `make dylint` pass
- [ ] `cfs validate` reports zero errors for the gear's code traceability

### Semantic
- [ ] No test, lint, build, or traceability failure is left unresolved when the command is available

---

## Error Handling

### Recovery Options
- [ ] If traceability validation fails, fix the marker placement or ID against the FEATURE rather than editing the FEATURE to match the code
- [ ] If a slice cannot be finished within the FEATURE scope, stop and propose an upstream FEATURE change

---

## Next Steps

### Options
- [ ] Commit the slice through `cf-git-commit`
- [ ] Prepare the pull request review through `cf-gears-pr-review`
