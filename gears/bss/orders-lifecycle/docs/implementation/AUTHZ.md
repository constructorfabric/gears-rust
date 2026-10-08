# S2-03 shared PEP and bounded internal authority

Status: **implemented, independently gap-fix reviewed and locally verified**. Scope is the shared Orders authorization adapter, authorization-bound storage adapters and the D-184 maintenance capability types. Of the public business operations only draft authoring and the draft reads are exposed (S2-09, early S6-01/S6-04, S2-12) through this PEP; submit, amendments and fulfillment stay unavailable, and nothing here marks production authorization ready (`upreq-pdp-policy-integration`).

## Delivered

| Area | Implementation |
|---|---|
| Permission catalog | [permissions.rs](../../orders-lifecycle/src/gts/permissions.rs): four registered labels, named properties, shared `ResourceType` descriptors (three axes plus standard `id`; `audit-unresolved` uses `subject_tenant_id`), 19 `AuthzPermissionV1` pairs |
| Shared PEP | [authz.rs](../../orders-lifecycle/src/authz.rs): closed 19-action census mapped from all 25 catalog operations (PATCH by field class), startup census, trusted `Caller` (authenticated context plus supplied proof reference), untargeted/targeted/proposed/replay decisions, D-114/D-141/D-68 hidden-missing call pattern, 403/404/503/integration mapping, finite order-ID shape check for Workflow seam actions and every service principal, user-only break-glass |
| Caller propagation | [REST adapter](../../orders-lifecycle/src/api/rest/mod.rs) extracts `x-delegation-proof-ref` once; SDK `CallMeta.delegation_proof_ref` reaches the PEP identically |
| Scoped storage | [scoped.rs](../../orders-lifecycle/src/infra/storage/scoped.rs): approved point prefetch (facts only), constrained re-read, lock-and-compare distinguishing stale facts (`authorization-context-changed`/`version-conflict`) from lost access (non-disclosing), complete-arrangement insert/update, bounded private persistence scopes |
| Maintenance capability | [scope.rs](../../orders-lifecycle/src/infra/maintenance/scope.rs): private read-only `DiscoveryScope`, task-gated bounded discovery in `TxConfig::read_only`, `TargetScope` only from discovered rows; retention, registry cleanup and checkpoint repositories accept only `&TargetScope`; [worker entry](../../orders-lifecycle/src/infra/maintenance/mod.rs) relocks and rechecks discovered facts; configured service actor and allowlisted tasks in `OrdersConfig.maintenance` |
| D-184 gates | crate-local `clippy.toml` (workspace file plus `AccessScope::allow_all`) with `#![deny(clippy::disallowed_methods)]` in both Orders crates; three `trybuild` fixtures; [source-scan backstop](../../orders-lifecycle/tests/d184_lint_gate.rs) |
| Platform provider | [rules-authz-plugin](../../../../system/authz-resolver/plugins/rules-authz-plugin/README.md): configurable fail-closed rules PDP (no Orders types), selectable by the example server's `rules-authz` feature. Exact `unconditional_grants` (one subject ID + tenant, one resource type, one action) cover property-less checks such as Event Broker's `event_type` `produce`; they apply only when the PEP declares no constraint properties and requires none (user decision 2026-10-06) |

## Interim decisions (coordinator, 2026-10-06)

- The supplied delegation proof reference travels as the reserved, never-compiled resource property `delegation_proof_ref`; Orders deny-code constants `delegation_proof_required`/`delegation_proof_invalid` are **pending platform agreement** (`UPSTREAM_REQS` §2.9 items 1–3). The resolver SDK is unchanged. Evidence rows record the reference as supplied, never verified.
- REST carrier `X-Delegation-Proof-Ref` is **confirmed by D-202** ([08 §4.4](../DESIGN.md#contract-08-4-4)) and recorded in the S1-02 boundary checker/fixtures (B44–B52).
- The rules plugin (policy revision recorded per run, e.g. `s2-03-matrix-1` in the PostgreSQL matrix) is a real resolver provider for integration and live E2E. `cpt-cf-bss-orders-lifecycle-upreq-pdp-policy-integration` stays open for the production provider, policy provisioning and delegation-proof verification.

## Independent gap-fix review (2026-10-06)

- Every PDP allow is enforced on the known facts with SecureORM's own predicate evaluation (`scoped::scope_admits`): an allow whose constraints exclude the prefetched target, the proposed arrangement or (on the follow-up read) the current facts is a denial with the same follow-up and 403/404 mapping, so a constraint-only provider can neither skip the D-68 follow-up nor turn a hidden target into a disclosing 403.
- A proposed arrangement never changes the seller (D-119; integration failure before any proposed-side call); only the list read obtains a collection scope.
- Audit-namespace discovery returns distinct namespaces with a keyset cursor, so a busy namespace cannot starve the others.
- Added a real concurrent axis-change race (blocked lock, then `authorization-context-changed`, no mutation or registry change) and constraint-only provider cases on PostgreSQL.

Findings, fixes and evidence are in the [gap review](../../../../../artifacts/orders-lifecycle-s2-20261006/S2-03-gap-review.md).

Evidence, exact commands and the requirement-to-test mapping are in the [implementation report](../../../../../artifacts/orders-lifecycle-s2-20261006/S2-03-implementation.md).
