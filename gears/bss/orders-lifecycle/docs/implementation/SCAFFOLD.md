# S2-01 — SDK/runtime scaffold execution ledger

Owner: Orders. Status: **implemented and locally verified as a scaffold**, 2026-10-05. No commit or PR was created. The original design reconciliation and unrelated working-tree changes remain local.

## Delivered paths and source coverage

| Paths | Delivery | Design/package source |
|---|---|---|
| [SDK crate](../../orders-lifecycle-sdk/Cargo.toml), [shared models](../../orders-lifecycle-sdk/src/models.rs), [reason/catalog generator](../../orders-lifecycle-sdk/generate_catalog.py) | Independent runtime-free SDK, checked version/revision/key types, canonical errors, sparse version/grant result golden, all planned operation and enum identities | S1-02; DESIGN API/boundary/GTS/error contracts |
| [Runtime crate](../../orders-lifecycle/Cargo.toml), [Gear](../../orders-lifecycle/src/gear.rs) | Explicit dependency/config checks, PostgreSQL requirement, last-step ClientHub provider registration, cooperative lifecycle, unhealthy business readiness | S2-01; Foundation lifecycle; D-184 prerequisites |
| [REST boundary](../../orders-lifecycle/src/api/rest/mod.rs) | Strong ETag/key parsing, independently required submit draft revision, HTTP 428 mapping; empty route and OpenAPI registration | D-112/147 and selected S1-02 bindings |
| [GTS declarations](../../orders-lifecycle/src/gts/mod.rs) and [permission catalog](../../orders-lifecycle/src/gts/permissions.rs) | 19 permissions, four resource labels, category/reason bootstrap, complete acknowledgement checking, distinct logical/registered action names | DESIGN permission matrix and category/error registry |
| [Migration hook](../../orders-lifecycle/src/infra/storage/mod.rs) | One owner and `bss_orders__` prefix; no placeholder DDL | S2-02 sole schema owner |
| Workspace/lockfile, example-server optional feature and registration, [gear metadata](../../gear.toml) | Packages named `cf-gears-bss-orders-lifecycle` and `cf-gears-bss-orders-lifecycle-sdk`; opt-in host integration | Existing Products/PriceBook workspace patterns |

Read the [crate overview and configuration](../../README.md) before enabling the feature. The selected scaffold API is deliberately incremental: its only local method is typed `submit`, always unavailable. It is not the completed nine-method Workflow SDK or the completed 25-route authoring/read surface. No caller can obtain a fabricated successful order, receipt or grant from it.

## Observed checks

- **11 Rust tests pass** (5 runtime, 2 REST-boundary, 4 SDK): configuration rejection, missing prerequisites, incomplete/refused registry response, real PostgreSQL initialization, bounded registry timeout, unavailable SDK result, empty routes/OpenAPI, unhealthy readiness and cooperative shutdown. PostgreSQL uses the repository's pinned `test_containers::postgres()` helper; PDP/registry are recording doubles.
- Rust boundary fixtures cover the independent S1-02 ETag/Problem goldens, missing/malformed draft revision, forbidden context fields, all 86 reason mappings, operation inventory, typed deserialization bounds, grant timestamp precision and unknown remote Problem preservation. These cover the scaffold's implemented subset; the 73-case Python oracle remains broader than this Rust adapter.
- Clippy, package formatting, generated catalog drift and targeted architecture lints pass. The optional example-server feature compiles with default features disabled. Documentation validation passes (4214 local links); the unchanged coverage inventory verifies 27 sources, 2557 source units and 44 upstream requirements. The S1-02 Python baseline still passes all 73 cases and eight golden envelopes. `git diff --check` passes.

Exact commands are in the [crate README](../../README.md#verification). This is boot/wiring evidence, not PostgreSQL row-level isolation, business transaction correctness, actual GTS schema-provider conformance, live PDP policy behavior or event delivery evidence.

## Next assignment and deferred work

S1-03 local capability proofs are now recorded in [CAPABILITIES](CAPABILITIES.md), including concrete adapter gaps and production-provider blockers. S1-04 local codec/contracts are recorded in [COMMERCIAL](COMMERCIAL.md); S1-05 local process/receiver contracts are recorded in [PROCESS](PROCESS.md). S1-06 local fixture/harness evidence is in [CONFORMANCE](CONFORMANCE.md). S1-07 local inventory is in [READINESS](READINESS.md). The next assignment is S5-01 receiver contract corrections. Test-only capability prototypes do not complete S2-03 runtime enforcement.

S1-04/05 record local commercial/receiver baselines with owner API/profile gaps; their provider packages still own full supported interfaces. S2-02 owns DDL; S2-03 owns enforcement; S2-04 the engine; S2-08 real event schemas/broker; S2-12 route mounting. The scaffold does not register permissive placeholder business handlers while those prerequisites remain absent. Continue to hold production readiness false until the supported milestone has real implementations and evidence.
