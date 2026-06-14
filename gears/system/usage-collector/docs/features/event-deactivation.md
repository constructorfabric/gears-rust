# Feature: Event Deactivation

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
  - [1.5 Explicit Non-Applicability](#15-explicit-non-applicability)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Deactivate Record](#deactivate-record)
  - [Depth-1 Cascade on Usage-Row Deactivation](#depth-1-cascade-on-usage-row-deactivation)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Operator PDP Authorization](#operator-pdp-authorization)
  - [Monotonic Transition Dispatch](#monotonic-transition-dispatch)
  - [Atomic Transition Outcome Mapping](#atomic-transition-outcome-mapping)
  - [Atomic Cascade Flip](#atomic-cascade-flip)
  - [Cascade-vs-Compensation Concurrency Guard](#cascade-vs-compensation-concurrency-guard)
- [4. States (CDSL)](#4-states-cdsl)
  - [Usage Record Deactivation Lifecycle State Machine](#usage-record-deactivation-lifecycle-state-machine)
- [5. Definitions of Done](#5-definitions-of-done)
  - [FR: Event Deactivation](#fr-event-deactivation)
  - [FR: Usage Compensation (Cascade Cross-Link)](#fr-usage-compensation-cascade-cross-link)
  - [NFR: Availability](#nfr-availability)
  - [Principle: Monotonic Deactivation](#principle-monotonic-deactivation)
  - [Principle: Fail Closed](#principle-fail-closed)
  - [ADR: Monotonic Deactivation](#adr-monotonic-deactivation)
  - [ADR: Usage Compensation (Cascade Companion)](#adr-usage-compensation-cascade-companion)
  - [Constraint: No Business Logic](#constraint-no-business-logic)
  - [Component: Deactivation Handler](#component-deactivation-handler)
  - [Sequence: Deactivate Usage Event](#sequence-deactivate-usage-event)
  - [Entity: Usage Record](#entity-usage-record)
  - [Entity: Deactivation Status](#entity-deactivation-status)
  - [Entity: Security Context](#entity-security-context)
  - [API: POST /usage-collector/v1/records/{id}/deactivate](#api-post-usage-collectorv1recordsiddeactivate)
  - [§2.5-item → DoD-ID Coverage Matrix](#25-item--dod-id-coverage-matrix)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-featstatus-event-deactivation`

<!-- reference to DECOMPOSITION entry -->

- [ ] `p2` - `cpt-cf-usage-collector-feature-event-deactivation`

## 1. Feature Context

### 1.1 Overview

Provides the PDP-authorized **error retraction** path that **voids any erroneous `UsageRecord` row regardless of whether its `corrects_id` is `IS NULL` (an ordinary usage row) or `IS NOT NULL` (a counter compensation row)** — by atomically flipping the targeted row's `status` column from `active` to `inactive` without mutating any other property. This realizes immutability-via-deactivation rather than in-place edits or hard deletion.

When the targeted row has `corrects_id IS NULL`, the same atomic transition cascades **depth-1**: every active row whose `corrects_id` equals the targeted row's id is flipped to `inactive` in the same one-shot step, so `SUM` returns to the state it held before either the usage row or its referencing compensations were accepted.

The `cpt-cf-usage-collector-component-deactivation-handler` accepts the operator's `SecurityContext` (resolved upstream by the ToolKit gateway on REST or supplied verbatim by the in-process caller on the SDK trait surface) and authorizes the deactivation through the per-component `authz_scope` helper that wraps `cpt-cf-usage-collector-contract-authz-resolver` fail-closed. It then issues a status-only atomic transition (with depth-1 cascade when applicable) through the Plugin SPI's `deactivate_usage_record` capability so the plugin enforces monotonicity and cascade atomicity at the storage layer.

Inactive records remain queryable through the §2.4 Query Gateway, preserving auditable history for downstream consumers while the substrate stays free of mutable-record patterns.

**Atomicity scope (plugin-transaction invariant, NOT a cross-path guarantee).** The depth-1 cascade documented above commits as one **plugin backend transaction**: the primary row and every active referencing compensation row are flipped together inside a single backend transaction with no cross-replica protocol. That atomicity is the invariant `cpt-cf-usage-collector-adr-monotonic-deactivation` and `cpt-cf-usage-collector-adr-usage-compensation` bind on the Plugin SPI's `deactivate_usage_record` capability.

It is **NOT** a promise that a subsequent Query SPI read against any read pool observes the post-cascade state — visibility through `cpt-cf-usage-collector-feature-usage-query` is governed separately by `cpt-cf-usage-collector-nfr-query-freshness` and `cpt-cf-usage-collector-adr-consistency-contract` (ADR-0011): eventually consistent with no upper bound at the gear floor, plugin-bound by the active plugin's published ceiling.

The set of cascade-flipped compensation ids is not part of the deactivation return shape (the REST surface answers HTTP 204 No Content on success; the SDK trait returns `Ok(())`); operators that need to enumerate the cascade-flipped ids issue a follow-up `list_usage_records` query against the `status` and `corrects_id` columns. Full contract: DESIGN [§3.10](../DESIGN.md#310-consistency-contract).

### 1.2 Purpose

This feature exists so that **error retraction** of previously accepted records — uniformly across rows whose `corrects_id IS NULL` (ordinary usage rows) and rows whose `corrects_id IS NOT NULL` (counter compensation rows) — is expressed as a one-way `active → inactive` status transition rather than as in-place mutation, hard deletion, or reactivation, keeping the metering substrate free of mutable-record semantics that would break audit guarantees, retroactive query reproducibility, and idempotency-keyed re-emission. The single-row, status-only, atomic transition (with depth-1 cascade from a deactivated record with `corrects_id IS NULL` to its active referencing compensations) is the only path that can mutate the persisted record's `status` after acceptance. Deactivation is the **only** correction primitive for `gauge` records and for the `COUNT`/`MIN`/`MAX`/`AVG` aggregations on any kind; counter value-reversal that nets inside `SUM` is owned by the complementary compensation primitive (`cpt-cf-usage-collector-fr-usage-compensation`) on the unified ingestion path documented inline in `usage-emission.md`, not by this feature.

**Requirements**: `cpt-cf-usage-collector-fr-event-deactivation`, `cpt-cf-usage-collector-fr-usage-compensation`, `cpt-cf-usage-collector-nfr-availability`

**Principles**: `cpt-cf-usage-collector-principle-monotonic-deactivation`, `cpt-cf-usage-collector-principle-fail-closed`

### 1.3 Actors

| Actor                                            | Role in Feature                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| ------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `cpt-cf-usage-collector-actor-platform-operator` | Authenticated platform operator who issues the deactivation request against a single previously emitted `UsageRecord` by supplying the target record `id` (path parameter) through `POST /usage-collector/v1/records/{id}/deactivate` or through the in-process SDK `deactivate_usage_record` operation; the operator's authority to deactivate the targeted record is verified by `cpt-cf-usage-collector-flow-foundation-pdp-authorize` against the resolved `SecurityContext` and PRD §8 `cpt-cf-usage-collector-usecase-deactivate-event`. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) -- Individual Event Deactivation §5.6 (`cpt-cf-usage-collector-fr-event-deactivation`), Availability §6.1 (`cpt-cf-usage-collector-nfr-availability`), Deactivate a Usage Event use case §8 (`cpt-cf-usage-collector-usecase-deactivate-event`), Actor catalog §2 (Platform Operator)
- **Design**: [DESIGN.md](../DESIGN.md) -- Deactivation Handler component (§3.5 `cpt-cf-usage-collector-component-deactivation-handler`), Monotonic Deactivation principle (§2.1 `cpt-cf-usage-collector-principle-monotonic-deactivation`), Fail-closed principle (§2.1 `cpt-cf-usage-collector-principle-fail-closed`), Deactivate Usage Event sequence `cpt-cf-usage-collector-seq-deactivate-event` (§3.6), status-only mutation contract (`plugin-spi.md` §"Cross-entity invariants honored by the Plugin SPI"), Domain Model entities `UsageRecord` / `UsageRecordStatus` / `SecurityContext` (§3.1), Endpoints Overview row for `POST /usage-collector/v1/records/{id}/deactivate` (§3.3), PRD→DESIGN realization rows for `fr-event-deactivation`, `nfr-authorization`, `nfr-availability` (§5.3)
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md) -- §2.5 Event Deactivation
- **Foundation feature**: [foundation.md](./foundation.md) -- SecurityContext acceptance at the surface boundaries (REST `Extension<SecurityContext>` from ToolKit gateway middleware via `OperationBuilder::authenticated()`; SDK trait methods accepting `ctx: &SecurityContext` as the first parameter), PDP enforcement via the per-component `authz_scope` helper (`cpt-cf-usage-collector-flow-foundation-pdp-authorize`), plugin host binding, tenant isolation, fail-closed posture (reused, not re-defined)
- **Usage Emission feature**: [usage-emission.md](./usage-emission.md) -- sole writer of every persisted-record attribution field other than `status` (the `status` field is owned by Event Deactivation per `plugin-spi.md` §"Cross-entity invariants honored by the Plugin SPI"); also hosts the **inlined compensation flow** (counter value-reversal: a `UsageRecord` with `corrects_id` set to the corrected usage row's id and a strictly-negative `value`) — deactivation targets exactly one row that the emission feature previously accepted, and cascades depth-1 to active compensations emitted through that inlined flow (reused, not re-defined)
- **Plugin SPI reference**: [plugin-spi.md](../plugin-spi.md) -- Method 5 (`deactivate_usage_record`) atomic monotonic transition capability with depth-1 set-flip semantics; returns `Ok(())` on success, surfaces `UsageRecordAlreadyInactive` and `UsageRecordNotFound` as error variants
- **SDK trait reference**: [sdk-trait.md](../sdk-trait.md) -- Method 5 (`deactivate_usage_record`) in-process operation returning `Result<(), UsageCollectorError>`, and the `Authorization` / `UsageRecordNotFound` / `AlreadyInactive` / `PluginUnavailable` / `TypesRegistryUnavailable` / `ServiceUnavailable` / `Internal` error variants (plugin `BackendError` lifts to `Internal` until a retryable-kind taxonomy is defined; plugin-side `Transient` lifts to `ServiceUnavailable`)
- **REST contract**: [usage-collector-v1.yaml](../usage-collector-v1.yaml) -- `POST /usage-collector/v1/records/{id}/deactivate` path (no request body), HTTP 204 No Content on successful transition, `context.reason="ALREADY_INACTIVE"` discriminator for the already-inactive `Problem` envelope, canonical `NotFound` for an unknown id, `context.reason="PLUGIN_READINESS"` for Plugin SPI transport / readiness / persistence faults
- **Domain model**: [domain-model.md](../domain-model.md) -- §2 `UsageRecord` (`corrects_id` field as the structural discriminator between usage rows and compensation rows), `UsageRecordStatus` entity-state invariants (`active -> inactive` monotonicity on the lifecycle column, atomic transition, depth-1 cascade to active referencing compensations)
- **ADR cross-references**: `cpt-cf-usage-collector-adr-monotonic-deactivation` (uniform error-retraction primitive that covers every row regardless of `corrects_id` presence, with a depth-1 cascade) and `cpt-cf-usage-collector-adr-usage-compensation` (the complementary counter value-reversal primitive that compensations cascade-deactivate alongside); `cpt-cf-usage-collector-adr-consistency-contract` (ADR-0011) — clarifies that the cascade atomicity recorded in §1.1 above is a plugin-transaction invariant, NOT a cross-path guarantee against subsequent Query SPI reads (see DESIGN [§3.10](../DESIGN.md#310-consistency-contract))
- **Dependencies**: `cpt-cf-usage-collector-feature-foundation`, `cpt-cf-usage-collector-feature-usage-emission` (hosts the inlined compensation flow whose active rows are the cascade targets)

### 1.5 Explicit Non-Applicability

- **UX** (`UX-FDESIGN-001` user journey, `UX-FDESIGN-002` accessibility): Not applicable because the event-deactivation feature is a backend operator surface (`POST /usage-collector/v1/records/{id}/deactivate` plus the in-process SDK `deactivate_usage_record` operation routed through the same `cpt-cf-usage-collector-component-deactivation-handler`); there is no human-facing UI in this gear, the only direct caller is the authenticated platform operator (`cpt-cf-usage-collector-actor-platform-operator`), and any operator-facing tooling that surfaces deactivation lives outside this feature's scope. Operator developer experience is encoded through the deterministic `Problem` error envelopes published by `usage-collector-v1.yaml` (`already_inactive`, canonical `NotFound`, canonical `Unauthenticated`, canonical `PermissionDenied`, `plugin_readiness` for SPI faults).
- **Counter value-reversal (refunds, credits, credit-notes, partial releases)**: Not applicable to this feature. Deactivation is **error retraction**, not value-reversal — it voids a whole row from every aggregation. Caller-driven counter value-reversal (an append-only signed-negative entry that reduces `SUM` without retracting the original event) is owned by the **compensation primitive**, whose flow is **inlined into `features/usage-emission.md`** (no separate FEATURE file exists; compensation rides the same unified ingestion path as ordinary emission). See PRD FR `cpt-cf-usage-collector-fr-usage-compensation` and ADR `cpt-cf-usage-collector-adr-usage-compensation` for the contract; computing refunds, credits, credit-notes, or quota balances remains a downstream-consumer responsibility per the un-policed-net stance in `cpt-cf-usage-collector-adr-usage-compensation`.
- **Bulk-by-query deactivation**: Not applicable per DECOMPOSITION §2.5 Out of scope — every deactivation targets exactly one record by `id`; multi-record selection by filter is explicitly out of scope and any such request shape is rejected by the OpenAPI contract before handler dispatch. (The depth-1 cascade flips multiple rows in a single atomic step, but the request still targets exactly one explicit `id`; cascaded compensation rows are selected by `corrects_id` referential identity, not by an arbitrary query filter.)
- **Compensating a compensation**: Not applicable non-goals — the L1 referential check rejects a `corrects_id` whose target itself has `corrects_id IS NOT NULL` (`corrects_id_targets_compensation`), so a compensation-references-compensation row is structurally impossible; deactivating a row with `corrects_id IS NOT NULL` is therefore a **single-row, no-cascade** operation by construction.
- **Reactivation (`inactive → active`)**: Not applicable — the Usage Collector does not provide a reactivation operation, and the SPI capability surface deliberately exposes only the one-way `deactivate_usage_record` per `plugin-spi.md` Method 5. The latch applies uniformly to rows with `corrects_id IS NULL` and rows with `corrects_id IS NOT NULL`, and to any rows flipped by the depth-1 cascade.
- **Field edits**: Not applicable — no value, timestamp, metadata, tenant, resource, subject, UsageType, idempotency-key, `corrects_id`, or any attribution field other than `status` is mutable after acceptance per `plugin-spi.md` §"Cross-entity invariants honored by the Plugin SPI" ("Deactivation is a status-only update; no other column of `usage_records` may be mutated by the SPI").
- **Negative-net detection / enforcement**: Not applicable. The Usage Collector does NOT validate non-negative `SUM` at write time and does NOT emit a negative-net signal when a depth-1 cascade leaves `SUM` at a non-negative value or when a future compensation drives `SUM` negative — see the un-policed-net stance in `cpt-cf-usage-collector-adr-usage-compensation`. Downstream consumers own any "net can't be negative" policy.
- **Gear-local audit event emission for the deactivate operation**: Not applicable per DESIGN §3.9.5 and the §4 forward-looking note — authoritative audit is delegated to the platform gateway access log and PDP decision logs.

## 2. Actor Flows (CDSL)

### Deactivate Record

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-flow-event-deactivation-deactivate-record`

**Actor**: `cpt-cf-usage-collector-actor-platform-operator`

**Success Scenarios**:

- An authenticated platform operator submits a deactivation request for a previously emitted `UsageRecord` by `id` via `POST /usage-collector/v1/records/{id}/deactivate` or via the SDK `deactivate_usage_record(ctx, ...)` operation. The target record MAY have either `corrects_id IS NULL` (ordinary usage row) or `corrects_id IS NOT NULL` (counter compensation row) — the surface is identical and the operator does not pre-declare the row's role. On the REST surface the handler receives `Extension<SecurityContext>` populated upstream by ToolKit gateway middleware (`OperationBuilder::authenticated()`) and delegates to the `UsageCollectorClientV1` SDK trait; on the in-process SDK surface the caller passes `ctx: &SecurityContext` as the first argument directly. Both entry points converge on `cpt-cf-usage-collector-component-deactivation-handler`. `cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization` invokes the per-component `authz_scope` helper (`PolicyEnforcer::access_scope_with(ctx, ...)` against `cpt-cf-usage-collector-contract-authz-resolver`) to authorize the deactivation, and `cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch` invokes the Plugin SPI Method 5 `deactivate_usage_record` capability against the target `id`; the capability runs the depth-1 cascade atomically and returns `Ok(())`. The handler surfaces HTTP `204 No Content` per `usage-collector-v1.yaml`. The `status` column of the targeted row AND every cascade-target row is now `inactive`; every other column on every affected row is byte-identical to its pre-call value. When the target row has `corrects_id IS NOT NULL`, the cascade is empty by construction (no row can reference a row with `corrects_id IS NOT NULL`); the set of cascade-flipped compensation ids is not part of the response and a follow-up `list_usage_records` query against the `status` / `corrects_id` columns enumerates them when needed.

**Error Scenarios**:

- Request arrives without a resolved `SecurityContext` (REST handler never invoked by the gateway middleware because authentication failed upstream, or SDK trait called without a `ctx` argument) — whole-request rejection via the canonical `Unauthenticated` `Problem` envelope per `usage-collector-v1.yaml` default response; the collector never synthesizes identity and no SPI dispatch occurs.
- PDP denies the operator's deactivation request — surfaced as the canonical `PermissionDenied` `Problem` envelope per `usage-collector-v1.yaml` default response; no SPI dispatch occurs and no state change.
- The plugin surfaces the `UsageRecordAlreadyInactive` error variant — translated to the actionable `Problem` envelope with `context.reason="ALREADY_INACTIVE"` per `usage-collector-v1.yaml` and the SDK `AlreadyInactive` variant per `sdk-trait.md` Method 5; no state change.
- The plugin surfaces the `UsageRecordNotFound` error variant — translated to the canonical `NotFound` `Problem` envelope per `usage-collector-v1.yaml` and the SDK `UsageRecordNotFound` variant; no state change.
- Plugin SPI transport / readiness / persistence error (`PluginUnavailable`, `Timeout`, `BackendError`) — surfaced as the canonical `Problem` envelope with `context.reason="PLUGIN_READINESS"`; no state change.

**Steps**:

1. [ ] - `p1` - Operator submits a deactivation request — on REST through `POST /usage-collector/v1/records/{id}/deactivate` (with the target `UsageRecord.uuid` as the path parameter); the REST handler receives `Extension<SecurityContext>` populated by ToolKit gateway middleware (`OperationBuilder::authenticated()`) and W3C audit-correlation headers — or on the SDK through `UsageCollectorClientV1::deactivate_usage_record(ctx, ...)` with `ctx: &SecurityContext` as the first parameter per `sdk-trait.md` Method 5 - `inst-deactivate-record-submit`
2. [ ] - `p1` - **IF** the REST handler receives no `Extension<SecurityContext>` (gateway middleware rejected the call upstream) or the SDK trait is invoked without a `ctx` argument **RETURN** the canonical `Unauthenticated` `Problem` envelope per `usage-collector-v1.yaml` default response; the collector never synthesizes identity - `inst-deactivate-record-missing-ctx`
3. [ ] - `p1` - Pre-fetch the target `UsageRecord` via Plugin SPI Method 10 `get_usage_record(id)` so PDP can authorize over the row's full attribution tuple (`tenant_id`, `resource_ref`, optional `subject_ref`). The host has only `id` at the boundary; this fetch is the sole path that resolves the loaded attribution. Existence-leak trade-off: a `UsageRecordNotFound` from this pre-PDP fetch is observable to any caller who can hit the endpoint; the trade-off is accepted because the operator surface is already strongly authenticated by the ToolKit gateway, and the alternative (PDP policy over `id` alone) would break the resource-attribute reasoning model the rest of the gear uses - `inst-deactivate-record-prefetch`
4. [ ] - `p1` - **IF** the prefetch returns `Err(UsageRecordNotFound { id })` **RETURN** the canonical `NotFound` `Problem` envelope (no PDP call, no Method 5 dispatch); on `Err(plugin-readiness)` propagate the canonical `PLUGIN_READINESS` envelope through the From-impl chain identical to the Method 5 catch path - `inst-deactivate-record-prefetch-not-found`
5. [ ] - `p1` - Invoke `cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization` to authorize the deactivation through `cpt-cf-usage-collector-flow-foundation-pdp-authorize` (per-component `authorize_usage_record` helper wrapping `cpt-cf-usage-collector-contract-authz-resolver`) against the inbound `SecurityContext` and the deactivation attribution tuple (operator identity from `SecurityContext` + the fetched record's `tenant_id`, `resource_ref`, optional `subject_ref`) under the `deactivate` action verb - `inst-deactivate-record-pdp`
6. [ ] - `p1` - **IF** the operator-PDP-authorization algorithm returns `deny` **RETURN** the canonical `PermissionDenied` `Problem` envelope per `usage-collector-v1.yaml` default response without any further dispatch — no SPI Method 5 dispatch occurs - `inst-deactivate-record-pdp-deny`
7. [ ] - `p1` - **TRY** dispatch the validated request via `cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch`, which invokes the Plugin SPI Method 5 `deactivate_usage_record` capability against the target `id`; the capability runs the depth-1 cascade atomically and returns `Ok(())` on a successful transition, or surfaces `UsageRecordAlreadyInactive { id }` / `UsageRecordNotFound { id }` as error variants per `plugin-spi.md` Method 5 - `inst-deactivate-record-spi-dispatch`
8. [ ] - `p1` - **CATCH** Plugin SPI transport / readiness / persistence error (`PluginUnavailable`, plugin-side `Transient`, plugin-side `Internal` / `BackendError` — including host-contract breaches lifted as `Internal { … }` / `BackendError { kind: "contract_violation", … }`) — realised by the `From<UsageCollectorPluginError> for DomainError` impl arms that map plugin-side `Transient` → `ServiceUnavailable` (host-side per-call deadline expirations also lift here; the SPI does not carve a separate `Timeout` variant) and plugin-side `Internal` / `BackendError` → `Internal` (the SDK does not yet expose a retryable-kind taxonomy, so backend errors collapse to the unclassified `Internal` envelope until plugins ship with one); `PluginUnavailable` and `TypesRegistryUnavailable` originate from the plugin host resolution path (same envelope family) - `inst-deactivate-record-spi-catch`
   1. [ ] - `p1` - **RETURN** the canonical `Problem` envelope per `usage-collector-v1.yaml` — the deactivate handler routes the `UsageCollectorError` through `usage_collector_error_to_problem`, whose `per_record_wire_reason` post-injects `context.reason="PLUGIN_READINESS"` on `PluginUnavailable` / `TypesRegistryUnavailable` / `ServiceUnavailable` (HTTP 503); plugin `BackendError` surfaces as the unclassified `Internal` envelope (HTTP 500) with no `PLUGIN_READINESS` reason until a retryable-kind taxonomy is defined; no state change occurs - `inst-deactivate-record-spi-fail`
9. [ ] - `p1` - The returned SPI result is mapped to the response branch through the `cpt-cf-usage-collector-algo-event-deactivation-atomic-outcome-mapping` From-impl chain (`From<UsageCollectorPluginError> for DomainError`, the consumer-boundary lift to `UsageCollectorError`, and `From<UsageCollectorError> for CanonicalError`); no separate dispatch helper runs at this step - `inst-deactivate-record-outcome-map`
10. [ ] - `p1` - **IF** the outcome-mapping algorithm returns `transitioned` **RETURN** HTTP `204 No Content` per `usage-collector-v1.yaml` — the explicitly-deactivated row id is `id` (the path parameter on REST), every active row whose `corrects_id` equalled `id` was flipped to `inactive` in the same atomic step (empty when the target row itself has `corrects_id IS NOT NULL`, or when no active rows referenced it); the response body is empty and operators that need to enumerate the cascade-flipped ids issue a follow-up `list_usage_records` query against the `status` / `corrects_id` columns - `inst-deactivate-record-success`
11. [ ] - `p1` - **ELSE IF** the outcome-mapping algorithm returns `already-inactive` **RETURN** the `Problem` envelope with `context.reason="ALREADY_INACTIVE"` per `usage-collector-v1.yaml` and the SDK `AlreadyInactive` variant per `sdk-trait.md` Method 5; no state change occurs - `inst-deactivate-record-already-inactive`
12. [ ] - `p1` - **ELSE** the outcome-mapping algorithm returns `not-found` (rare: the prefetch saw the row but a concurrent deactivation / purge removed it before Method 5 dispatched, or the plugin's per-transaction visibility scope differs from the prefetch's); **RETURN** the canonical `NotFound` `Problem` envelope per `usage-collector-v1.yaml` and the SDK `UsageRecordNotFound` variant per `sdk-trait.md` Method 5; no state change occurs - `inst-deactivate-record-not-found`

### Depth-1 Cascade on Usage-Row Deactivation

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-flow-event-deactivation-cascade`

**Actor**: `cpt-cf-usage-collector-actor-platform-operator`

**Success Scenarios**:

- An authenticated platform operator deactivates a row R with `corrects_id IS NULL` (an ordinary usage row) that has one or more active rows whose `corrects_id` equals `R.id`. The Plugin SPI Method 5 `deactivate_usage_record(R.id)` capability executes `cpt-cf-usage-collector-algo-event-deactivation-atomic-cascade-flip`: in a **single atomic transition** at the storage layer, R is flipped from `active` to `inactive` AND every active referencing row C with `C.corrects_id = R.id ∧ C.corrects_id IS NOT NULL ∧ C.status = active ∧ same (tenant_id, gts_id)` is flipped from `active` to `inactive`. The handler surfaces HTTP `204 No Content` per `usage-collector-v1.yaml`. Post-cascade `SUM(value)` over `(tenant_id, gts_id)` returns to the state it held before either R or its referencing compensations were accepted; `COUNT`/`MIN`/`MAX`/`AVG` (which operate over active rows WHERE `corrects_id IS NULL`) also no longer include R. Operators that need to enumerate the cascade-flipped ids issue a follow-up `list_usage_records` query against the `status` / `corrects_id` columns.
- The same operator surface, applied to a row C with `corrects_id IS NOT NULL` (a counter compensation row): the capability flips C only — **single-row, no cascade** — and surfaces HTTP `204 No Content`. The depth-1 bound is structural: the L1 referential check rejects any `corrects_id` whose target itself has `corrects_id IS NOT NULL` (`corrects_id_targets_compensation`), so no row can reference a compensation row, and there is no second hop.
- The same operator surface, applied to a row with `corrects_id IS NULL` and no active rows referencing it: the capability flips only that row and surfaces HTTP `204 No Content`.

**Error Scenarios**:

- The cascade transition fails partway in the storage layer (a single compensation flip rejected by an underlying constraint or a transient transport fault mid-step). The Plugin SPI Method 5 capability MUST surface this as `PluginUnavailable` / `BackendError` per `plugin-spi.md` Method 5 atomicity obligation; the entire set-flip is reverted (or never committed), no row's `status` changes, and the handler returns `context.reason="PLUGIN_READINESS"` per `usage-collector-v1.yaml`. Partial cascades are structurally impossible because the cascade is one transaction.
- Concurrent compensation submission referencing R arriving while R is mid-deactivation: rejected by the L1 "referenced record must be active" check; the cascade itself observes only the set of compensations that were committed-active at transaction-start. See §3 Concurrency Guard.

**CDSL outcome shape** (logical; surface-specific spellings owned by sdk-trait.md / plugin-spi.md / usage-collector-v1.yaml per DESIGN §3.3):

```text
deactivate(<id>) -> Ok(())     # on success: the primary row PLUS every active row whose
                               # corrects_id equals <id> (single atomic depth-1 cascade
                               # when primary has corrects_id IS NULL; single-row, no cascade
                               # when primary has corrects_id IS NOT NULL) have flipped
                               # active -> inactive. The set of cascade-flipped ids is NOT
                               # part of the return shape; a follow-up list_usage_records
                               # query against status / corrects_id columns enumerates it.
                               # REST surface: HTTP 204 No Content. SDK surface: Result<(), Error>.
```

**Steps**:

1. [ ] - `p1` - Receive the explicitly-deactivated `id` from `cpt-cf-usage-collector-flow-event-deactivation-deactivate-record` after PDP `allow` - `inst-cascade-receive-id`
2. [ ] - `p1` - Invoke the Plugin SPI Method 5 capability `deactivate_usage_record(id)` exactly once; the capability is the atomic boundary that scopes the cascade per `plugin-spi.md` Method 5 - `inst-cascade-spi-call`
3. [ ] - `p1` - **PLUGIN-SIDE CONTRACT**. **IF** `primary.corrects_id IS NULL` (the primary is an ordinary usage row) — the capability MUST atomically flip the primary row AND every active row C with `C.corrects_id = primary.id ∧ C.corrects_id IS NOT NULL ∧ C.status = active ∧ same (tenant_id, gts_id)` from `active` to `inactive` in the same transition. The contract is documented at `plugin_api.rs::UsageCollectorPluginV1::deactivate_usage_record` Method 5; the noop plugin (`noop-usage-collector-plugin`) does NOT persist records and therefore does NOT exercise this branch — it short-circuits to `UsageRecordNotFound`. Production plugins with real storage MUST implement this set-flip atomically; the host carries no implementation and no marker for this step until such a plugin lands - `inst-cascade-usage-set-flip`
   1. [ ] - `p1` - **PLUGIN-SIDE CONTRACT**. **RETURN** `Ok(())` — the set of cascade-flipped compensation ids is committed atomically but is NOT part of the return shape; a follow-up `list_usage_records` query against the `status` / `corrects_id` columns enumerates it when needed - `inst-cascade-usage-return`
4. [ ] - `p1` - **PLUGIN-SIDE CONTRACT**. **ELSE IF** `primary.corrects_id IS NOT NULL` (the primary is itself a counter compensation row) — the capability flips ONLY the primary row; no cascade target search is performed because the L1 referential check rejects any `corrects_id` whose target itself has `corrects_id IS NOT NULL` (`corrects_id_targets_compensation`). Same realisation status as step 3: contract only, no production plugin in this repo - `inst-cascade-compensation-single`
   1. [ ] - `p1` - **PLUGIN-SIDE CONTRACT**. **RETURN** `Ok(())` — single-row transition, no cascade - `inst-cascade-compensation-return`
5. [ ] - `p1` - **CATCH** any storage-layer failure during the set-flip — partial cascade is structurally impossible because the transition is one transaction; the same `From<UsageCollectorPluginError> for DomainError` arms that catch the deactivate-record SPI-catch are the catch site for cascade-fail too - `inst-cascade-fail`
   1. [ ] - `p1` - Propagate `PluginUnavailable` | `Timeout` | `BackendError` per `plugin-spi.md` Method 5; the deactivate handler routes the propagated `UsageCollectorError` through `usage_collector_error_to_problem`, whose `per_record_wire_reason` post-injects `context.reason="PLUGIN_READINESS"`; no row's `status` changes - `inst-cascade-fail-propagate`

## 3. Processes / Business Logic (CDSL)

### Operator PDP Authorization

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization`

**Input**: the inbound `SecurityContext` (already present at the handler boundary per Flow A step 2 — the algorithm itself does NOT re-verify presence) AND the loaded `UsageRecord` returned by the Flow A step 3 prefetch (Plugin SPI Method 10 `get_usage_record(id)`).

**Output**: `Ok(())` when the PDP permits the operator's deactivation request, or `DomainError::AuthorizationDenied` / `DomainError::AuthorizationUnavailable` (lifted to `UsageCollectorError::Authorization` / `ServiceUnavailable` and ultimately the canonical `PermissionDenied` / `PLUGIN_READINESS` `Problem` envelopes). The algorithm MUST NOT re-implement PDP logic — it invokes the shared per-resource `authorize_usage_record` helper (`PolicyEnforcer::access_scope_with(ctx, &usage_record::RESOURCE, actions::DEACTIVATE, None, &request)` against `cpt-cf-usage-collector-contract-authz-resolver`), which is also the create-side authorizer for the emission feature (the `action` parameter selects the verb). Authentication is owned by the ToolKit gateway upstream of the REST handler and by the in-process caller on the SDK trait surface; the collector NEVER synthesizes identity and NEVER consults an authentication contract.

**Pre-PDP existence-leak trade-off**: the prefetch (Flow A step 3) returns a `UsageRecordNotFound` to any operator who can hit the endpoint. This is accepted because the operator surface is already strongly authenticated by the ToolKit gateway; the alternative (PDP over a uuid alone) would lose the resource-attribute reasoning model the rest of the gear uses. The trade-off is documented at the `Service::deactivate_usage_record` call site.

**Steps**:

1. [ ] - `p1` - Receive the inbound `SecurityContext` at the `cpt-cf-usage-collector-component-deactivation-handler` boundary — on REST as `Extension<SecurityContext>` from the gateway middleware, on SDK as the `ctx: &SecurityContext` first argument — along with the prefetched record - `inst-algo-pdp-receive-ctx`
2. [ ] - `p1` - **IF** no `SecurityContext` is present at the boundary **RETURN** `unauthenticated`; the collector never synthesizes identity and never forwards an unauthenticated request to the PDP. (Realised at the framework layer: `OperationBuilder::authenticated()` on the route plus the axum `Extension<SecurityContext>` extractor on the handler — the algorithm body is not entered without ctx.) - `inst-algo-pdp-no-ctx`
3. [ ] - `p1` - Compose the deactivation attribution tuple from the inbound `SecurityContext` (operator principal and operator's tenant scope) and the prefetched record's PEP attributes: `OWNER_TENANT_ID = record.tenant_id`, `PROP_RESOURCE_TYPE = record.resource_ref.resource_type`, `PROP_RESOURCE_ID = record.resource_ref.resource_id`, and (when `record.subject_ref` is set) `OWNER_ID = subject.subject_id` plus optional `PROP_SUBJECT_TYPE = subject.subject_type`. The verb is `actions::DEACTIVATE`. The UsageType `gts_id` field of the standard attribution tuple is not applicable to operator deactivation and is omitted - `inst-algo-pdp-compose-tuple`
4. [ ] - `p1` - Invoke `cpt-cf-usage-collector-flow-foundation-pdp-authorize` via the shared per-resource `authorize_usage_record` helper (`PolicyEnforcer::access_scope_with(ctx, &usage_record::RESOURCE, actions::DEACTIVATE, None, &request)` against `cpt-cf-usage-collector-contract-authz-resolver`) to obtain the `PdpDecision` (`permit` or `deny`) - `inst-algo-pdp-call`
5. [ ] - `p1` - **IF** the PDP helper returns `unreachable` (PDP transport failure) **RETURN** `deny`; no cached decision is consulted and no permissive fallback is applied (`EnforcerError::EvaluationFailed` maps to `DomainError::AuthorizationUnavailable`) - `inst-algo-pdp-fail-closed`
6. [ ] - `p1` - **IF** the PDP decision is `deny` **RETURN** `deny` carrying the propagated platform-authorization envelope (canonical `PermissionDenied`; `EnforcerError::Denied` / `CompileFailed` map to `DomainError::AuthorizationDenied`) - `inst-algo-pdp-deny`
7. [ ] - `p1` - **RETURN** `Ok(())` — the surrounding flow proceeds to Method 5 dispatch - `inst-algo-pdp-allow`

### Monotonic Transition Dispatch

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch`

**Input**: the validated target `UsageRecord.uuid`; the algorithm runs only after `cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization` returned `Ok(())`.

**Output**: Either `Ok(())` from the Plugin SPI Method 5 capability (successful atomic transition) forwarded for outcome mapping, or a Plugin SPI error variant — `UsageRecordAlreadyInactive { id }` / `UsageRecordNotFound { id }` for the deterministic rejection cases (forwarded for outcome mapping), or `PluginUnavailable` / `Timeout` / `BackendError` propagated to the surrounding `CATCH` branch for `context.reason="PLUGIN_READINESS"` rejection — per `plugin-spi.md` Method 5. The depth-1 cascade (primary row with `corrects_id IS NULL` plus all active rows whose `corrects_id` equals the primary's id, flipped together) is owned by `cpt-cf-usage-collector-algo-event-deactivation-atomic-cascade-flip` inside the SPI capability — this dispatch algorithm does not iterate, does not query for cascade targets, and does not split the call across multiple SPI invocations. The algorithm MUST NOT perform any local state cache, MUST NOT re-query the row for a status pre-check between the Flow A step-3 prefetch and the SPI Method 5 dispatch (the SPI capability is the atomic boundary; the prefetch is solely to supply attribution attributes to PDP).

**Steps**:

1. [ ] - `p1` - Resolve the ClientHub-scoped Plugin SPI client through `cpt-cf-usage-collector-component-plugin-host` for the configured GTS instance binding owned by `cpt-cf-usage-collector-feature-foundation` (the same client previously used for the prefetch is reused; both calls go through `Service::get_plugin()`) - `inst-algo-dispatch-resolve-plugin`
2. [ ] - `p1` - Invoke the Plugin SPI Method 5 capability `deactivate_usage_record(id)` exactly once with the target `id`; trace context is propagated via the ambient `tracing::Span` / OpenTelemetry context (no explicit `TraceContext` parameter) per `plugin-spi.md` Method 5 - `inst-algo-dispatch-spi-call`
3. [ ] - `p1` - **TRY** await the single `Result<(), UsageCollectorPluginError>` from the plugin per `plugin-spi.md` Method 5 - `inst-algo-dispatch-await`
4. [ ] - `p1` - **CATCH** Plugin SPI infrastructure error variant `PluginUnavailable` | `Timeout` | `BackendError` per `plugin-spi.md` Method 5 (the SPI exposes no `Unready` variant; structural unavailability surfaces as `PluginUnavailable`) - `inst-algo-dispatch-catch`
   1. [ ] - `p1` - Propagate the error variant up to the surrounding `CATCH` in `cpt-cf-usage-collector-flow-event-deactivation-deactivate-record` so the handler maps it to `context.reason="PLUGIN_READINESS"` per `usage-collector-v1.yaml` while preserving the audit-correlation context — the propagation chain is `UsageCollectorPluginError` → `DomainError` → `UsageCollectorError` → `usage_collector_error_to_problem` (which post-injects `PLUGIN_READINESS`) - `inst-algo-dispatch-propagate-error`
5. [ ] - `p1` - **RETURN** the result verbatim (`Ok(())` on a successful transition, or one of the deterministic rejection error variants `UsageRecordAlreadyInactive { id }` / `UsageRecordNotFound { id }`) to the calling flow for outcome mapping; the atomic depth-1 cascade has already been committed by the storage-layer set-flip when the result is `Ok(())` - `inst-algo-dispatch-return-outcome`

### Atomic Transition Outcome Mapping

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-algo-event-deactivation-atomic-outcome-mapping`

**Realisation**: this mapping is the host crate's compile-time `From`-impl error chain at the dispatch/handler boundary, NOT a discrete named function. Rust's `?` operator threads the SPI `Result<(), UsageCollectorPluginError>` through two deterministic conversions — `From<UsageCollectorPluginError> for DomainError` and `From<UsageCollectorError> for CanonicalError` (with the boundary lift `From<DomainError> for UsageCollectorError` between them) — so the response branch is selected purely by the variant the plugin returned. No runtime dispatch helper exists or is needed; each rule below corresponds to a single match arm in the From-impl chain.

**Input**: a single `Result<(), UsageCollectorPluginError>` returned by the Plugin SPI Method 5 capability — `Ok(())` on a successful transition, or one of the deterministic rejection error variants `UsageRecordAlreadyInactive { id }` / `UsageRecordNotFound { id }` per `plugin-spi.md` Method 5 — plus the target `UsageRecord.uuid` carried from the original request.

**Output**: a deterministic response branch — HTTP `204 No Content` on `Ok(())`; HTTP 409 `Aborted` `Problem` envelope with `context.reason="ALREADY_INACTIVE"` per `usage-collector-v1.yaml` and the SDK `AlreadyInactive` error variant per `sdk-trait.md` Method 5 on `UsageRecordAlreadyInactive`; canonical `NotFound` `Problem` envelope and the SDK `UsageRecordNotFound` error variant on `UsageRecordNotFound`. The mapping MUST be 1:1 with the SPI result taxonomy — no other outcomes are recognized, and any unexpected error variant is treated as a host-contract breach (`BackendError { kind: "contract_violation", … }`) at the dispatch stage rather than mapped here. The set of cascade-flipped compensation ids is not part of the SPI return shape and is not threaded through this mapping; operators that need it issue a follow-up `list_usage_records` query.

**Mapping rules** (one match arm per rule):

1. [ ] - `p1` - **WHEN** the SPI result is `Ok(())` the `?` operator propagates the unit value upward through the dispatch function and the calling flow surfaces HTTP `204 No Content` per `usage-collector-v1.yaml`; this is the only path that may report a successful `active → inactive` transition - `inst-algo-outcome-transitioned`
2. [ ] - `p1` - **WHEN** the SPI error is `UsageRecordAlreadyInactive { id }` the `From<UsageCollectorPluginError> for DomainError` impl produces `DomainError::UsageRecordAlreadyInactive(id)`, the boundary lift produces `UsageCollectorError::AlreadyInactive(id)`, and `From<UsageCollectorError> for CanonicalError` produces an `Aborted` (HTTP 409) envelope with `context.reason="ALREADY_INACTIVE"` per `usage-collector-v1.yaml` and the SDK `AlreadyInactive` error variant per `sdk-trait.md` Method 5, preserving the no-reactivation invariant - `inst-algo-outcome-already-inactive`
3. [ ] - `p1` - **WHEN** the SPI error is `UsageRecordNotFound { id }` the same chain produces `DomainError::UsageRecordNotFound(id)` → `UsageCollectorError::UsageRecordNotFound { id }` → canonical `NotFound` `Problem` envelope per `usage-collector-v1.yaml` and the SDK `UsageRecordNotFound` error variant per `sdk-trait.md` Method 5 - `inst-algo-outcome-not-found`

### Atomic Cascade Flip

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-algo-event-deactivation-atomic-cascade-flip`

**Realisation status**: PLUGIN-SIDE CONTRACT. This algorithm specifies the **internal** behavior of Plugin SPI Method 5 (`UsageCollectorPluginV1::deactivate_usage_record`) — it is NOT implemented by the host crate. The host's responsibility ends at "call Method 5 exactly once with `id`" (see `cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch`). The noop plugin in this repo (`noop-usage-collector-plugin`) does NOT persist usage records and short-circuits Method 5 to `UsageRecordNotFound`; the cascade obligation is fulfilled only by production plugins with real storage. All step-level checkboxes below stay `[ ]` until such a plugin lands.

**Input**: the explicitly-deactivated record id forwarded by `cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch` to the Plugin SPI Method 5 capability. The primary row's `corrects_id` (set vs. null) is read inside the capability's atomic transition; the operator does not pre-declare it.

**Output**: a single `Result<(), UsageCollectorPluginError>` per `plugin-spi.md` Method 5:

- `Ok(())` — the primary row was flipped from `active` to `inactive` in this transition; every active row whose `corrects_id IS NOT NULL` and whose `corrects_id` equalled the primary's id was flipped from `active` to `inactive` in the **same** transition (empty cascade when the primary itself has `corrects_id IS NOT NULL`, single-row no-cascade by construction; or when no active rows referenced the primary). The set of cascade-flipped ids is not part of the return shape; operators that need it issue a follow-up `list_usage_records` query against the `status` / `corrects_id` columns.
- `UsageRecordAlreadyInactive { id }` — the primary row was already `status = inactive` at transaction-start; no row's `status` changes; no cascade evaluation is performed.
- `UsageRecordNotFound { id }` — no row with the given id exists in `(tenant_id, gts_id)` scope visible to this transaction; no row's `status` changes.

The algorithm is a single atomic set-flip; no row's `status` may change without all of them changing together. Partial cascade is structurally impossible.

**Steps**:

1. [ ] - `p1` - **TRY** the following inside a single storage-layer atomic transition (the SPI Method 5 capability is the atomic boundary; no row's `status` may change in isolation) - `inst-algo-cascade-tx-begin`
   1. [ ] - `p1` - Read the primary row by `id`; **IF** absent **RETURN** the `UsageRecordNotFound { id }` error variant - `inst-algo-cascade-read-primary`
   2. [ ] - `p1` - **IF** primary.status = `inactive` **RETURN** the `UsageRecordAlreadyInactive { id }` error variant (no state change; no cascade evaluation; preserves the one-way `active → inactive` latch) - `inst-algo-cascade-already-inactive`
   3. [ ] - `p1` - Flip primary.status from `active` to `inactive` - `inst-algo-cascade-flip-primary`
   4. [ ] - `p1` - **IF** `primary.corrects_id IS NULL` (ordinary usage row) — select every row C such that `C.corrects_id = primary.id ∧ C.corrects_id IS NOT NULL ∧ C.status = active ∧ C.tenant_id = primary.tenant_id ∧ C.gts_id = primary.gts_id`; flip each selected row's `status` from `active` to `inactive` **in the same transition** - `inst-algo-cascade-flip-companions`
   5. [ ] - `p1` - **ELSE** (`primary.corrects_id IS NOT NULL`, the primary is itself a counter compensation row) — no companion lookup is performed because the L1 referential check rejects any `corrects_id` whose target itself has `corrects_id IS NOT NULL` (`corrects_id_targets_compensation`); the transition is single-row - `inst-algo-cascade-compensation-no-companions`
2. [ ] - `p1` - **CATCH** any storage-layer fault during the transaction — abort the entire transaction; no row's `status` is committed - `inst-algo-cascade-fail`
   1. [ ] - `p1` - **RETURN** the corresponding Plugin SPI error variant (`PluginUnavailable` | `Timeout` | `BackendError`) per `plugin-spi.md` Method 5; the dispatch algorithm propagates this to the surrounding flow's `CATCH` branch - `inst-algo-cascade-fail-propagate`
3. [ ] - `p1` - **RETURN** `Ok(())` — the transition committed atomically; every cascade target observed `status = active` at transaction-start, and every cascade target's `status` is `inactive` at transaction-commit - `inst-algo-cascade-return`

### Cascade-vs-Compensation Concurrency Guard

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-algo-event-deactivation-concurrency-guard`

**Realisation status**: CROSS-FEATURE OBLIGATION. The guard is documented from the deactivation feature's vantage point but the L1 "referenced record must be active" check is enforced inside the ingestion path inlined in `usage-emission.md`. The atomicity half of the guard depends on `cpt-cf-usage-collector-algo-event-deactivation-atomic-cascade-flip`, which is itself a PLUGIN-SIDE CONTRACT (see preceding algorithm). The deactivation feature owns neither implementation site, so the step-level checkboxes below stay `[ ]` until both the usage-emission L1 check and a production storage plugin with cascade atomicity land.

**Input**: any compensation ingestion request (a `UsageRecord` with `corrects_id` set) submitted concurrently with the deactivation of the usage row R it references (R has `corrects_id IS NULL`). The guard is documented from the deactivation feature's vantage point but the L1 check is enforced inside the ingestion path inlined in `usage-emission.md` (the request never reaches this feature's handler).

**Output**: the verbatim guarantee that no compensation submission can be admitted after R leaves `active`, even under concurrent submission and deactivation.

**Concurrency rule (verbatim, from the locked decision in plan.toml)**:

> A compensation submission referencing R that arrives while R is being deactivated is rejected by the L1 "referenced record must be active" check; state ordering and atomicity of the cascade transition guarantee that no compensation can be admitted after R leaves `active`.

**Steps**:

1. [ ] - `p1` - The L1 `corrects_id` referential check on the ingestion path inlined in `usage-emission.md` reads the referenced row's `(corrects_id, status, tenant_id, gts_id)` and admits the compensation only when `exists ∧ corrects_id IS NULL ∧ same (tenant_id, gts_id) ∧ status = active`. A row mid-deactivation either still reports `status = active` (the deactivation transaction has not yet committed) or already reports `status = inactive` (the deactivation transaction has committed). The L1 check observes one of these two states; there is no observable intermediate state - `inst-algo-concurrency-l1`
2. [ ] - `p1` - **IF** the L1 check observes `status = inactive` (deactivation already committed) the compensation is rejected; no row mutation occurs - `inst-algo-concurrency-reject-inactive`
3. [ ] - `p1` - **IF** the L1 check observes `status = active` but the deactivation transaction is still in flight — the storage layer's transactional ordering ensures one of two terminal outcomes: either (a) the compensation insert serialises **before** the deactivation transaction commits and the deactivation's cascade query observes that compensation as `active` and flips it together with the primary in the same atomic cascade transition, or (b) the compensation insert serialises **after** the deactivation commit and the L1 re-read (or the storage-layer concurrency control) sees `status = inactive` and rejects the compensation. There is no third option: no compensation can be admitted referencing a row that has already left `active` - `inst-algo-concurrency-serialise`
4. [ ] - `p1` - **RETURN** the locked invariant: state ordering and atomicity of the cascade transition guarantee that no compensation can be admitted after R leaves `active`. This guard adds no new lock or coordinator — it depends only on the L1 check and the atomicity of `cpt-cf-usage-collector-algo-event-deactivation-atomic-cascade-flip` - `inst-algo-concurrency-return`

## 4. States (CDSL)

### Usage Record Deactivation Lifecycle State Machine

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-state-event-deactivation-record-lifecycle`

**States**: `Active`, `Inactive`

**Initial State**: `Active` (every accepted `UsageRecord` — regardless of whether its `corrects_id IS NULL` or `corrects_id IS NOT NULL` — enters `Active` on ingestion per the unified ingestion path inlined in `features/usage-emission.md`; the emission feature is the only writer that creates new usage records through the Plugin SPI).

**Transition table** (cascade-aware; a single atomic SPI transition may flip multiple rows together):

| Source rows                                                                                | Trigger                                                  | Atomic effect (one transition)                                                                                                                                                                                  | Returned result                            |
| ------------------------------------------------------------------------------------------ | -------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------ |
| Primary row P (any `corrects_id` value); P.status = `Active`                               | `deactivate(P.id)` — Plugin SPI Method 5 returns `Ok(())` | P.status flips `Active → Inactive`; if `P.corrects_id IS NULL`, every active row C (`C.corrects_id = P.id`, `C.corrects_id IS NOT NULL`) ALSO flips `Active → Inactive` in the **same** transition              | `Ok(())`                                    |
| Primary row P; P.status = `Active`; `P.corrects_id IS NULL`; no active rows reference P    | `deactivate(P.id)`                                       | P.status flips `Active → Inactive` only; no cascade selection yields rows                                                                                                                                       | `Ok(())`                                    |
| Primary row P; P.status = `Active`; `P.corrects_id IS NOT NULL` (counter compensation row) | `deactivate(P.id)`                                       | P.status flips `Active → Inactive`; no cascade evaluation (the L1 referential check rejects any row that would target a row with `corrects_id IS NOT NULL`) | `Ok(())`                                    |
| Primary row P; P.status = `Inactive`                                                       | `deactivate(P.id)`                                       | No row's `status` changes (one-way latch); no cascade evaluation                                                                                                                                                | `Err(UsageRecordAlreadyInactive { id })`    |
| No row with given id in the operator's tenant scope                                        | `deactivate(<id>)`                                       | No row's `status` changes                                                                                                                                                                                       | `Err(UsageRecordNotFound { id })`           |

**Transitions** (CDSL):

1. [ ] - `p1` - **FROM** `Active` **TO** `Inactive` **WHEN** the Plugin SPI Method 5 `deactivate_usage_record` capability returns `Ok(())` for the target primary `id`; the transition is atomic at the storage layer per `plugin-spi.md` Method 5 atomicity obligation and the depth-1 cascade defined by `cpt-cf-usage-collector-algo-event-deactivation-atomic-cascade-flip`, and no other attribution field of the affected records is mutated per `plugin-spi.md` §"Cross-entity invariants honored by the Plugin SPI" ("Deactivation is a status-only update; no other column of `usage_records` may be mutated by the SPI") — host-side dispatch site marked at `Service::deactivate_usage_record`; the storage-layer atomicity guarantee is owned by the plugin contract per `inst-cascade-usage-set-flip` - `inst-state-active-to-inactive`
2. [ ] - `p1` - **PLUGIN-SIDE CONTRACT**. **FROM** `Active` **TO** `Inactive` (**CASCADE COMPANIONS**, same atomic transition as the primary flip) **WHEN** the primary has `corrects_id IS NULL` and the storage-layer set-flip selects companion rows by `C.corrects_id = primary.id ∧ C.corrects_id IS NOT NULL ∧ C.status = active ∧ same (tenant_id, gts_id)`; every selected companion's `status` flips `Active → Inactive` in the **same** atomic transition as the primary. The set of cascade-flipped companion ids is not part of the SPI return shape; a follow-up `list_usage_records` query against the `status` / `corrects_id` columns enumerates them when needed. Partial cascade is structurally impossible — the entire set-flip commits together or not at all. Same realisation gap as `inst-cascade-usage-set-flip`: no production storage plugin in this repo, noop short-circuits to `UsageRecordNotFound` - `inst-state-cascade-companions`
3. [ ] - `p1` - **FROM** `Inactive` **TO** `Inactive` **WHEN** a subsequent deactivation request targets the same `id` — the Plugin SPI Method 5 capability MUST surface the `UsageRecordAlreadyInactive { id }` error variant (no state change; no cascade re-evaluation) per `plugin-spi.md` Method 5, and the handler surfaces `context.reason="ALREADY_INACTIVE"` per `usage-collector-v1.yaml`; this is the no-op self-edge that realizes monotonicity at the SPI boundary and applies uniformly to rows with `corrects_id IS NULL` and rows with `corrects_id IS NOT NULL`. Host-side observable behavior is the canonical `Aborted` envelope with `context.reason="ALREADY_INACTIVE"`; the test `deactivate_usage_record_plugin_already_inactive_lifts_to_sdk_already_inactive` pins the mapping - `inst-state-inactive-self-loop`
4. [ ] - `p1` - **NO TRANSITION FROM** `Inactive` **TO** `Active` exists for any row — the Usage Collector does not provide a reactivation operation, the Plugin SPI Method 5 capability surface deliberately exposes only the one-way `deactivate_usage_record` per `plugin-spi.md` Method 5, the one-way latch applies to primary rows AND to cascade-flipped rows alike (regardless of `corrects_id` presence), and any caller-side attempt to re-introduce the inverse path is structurally impossible on the contract surface published by `usage-collector-v1.yaml` and `sdk-trait.md`. **Realisation by structural absence**: the SDK trait `UsageCollectorClientV1` and the Plugin SPI trait `UsageCollectorPluginV1` declare NO `reactivate_usage_record` method; the marker on `deactivate_usage_record` in `usage-collector-sdk/src/api.rs` is the load-bearing anchor for this invariant (no companion method, no inverse path) - `inst-state-no-reactivation`

## 5. Definitions of Done

### FR: Event Deactivation

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-event-deactivation-fr-event-deactivation`

The system **MUST** support deactivating an individual `UsageRecord` by `id` — regardless of whether the row has `corrects_id IS NULL` (ordinary usage row) or `corrects_id IS NOT NULL` (counter compensation row) — through `POST /usage-collector/v1/records/{id}/deactivate` (REST, returning HTTP 204 No Content on success) and the SDK `deactivate_usage_record` operation (in-process, returning `Result<(), UsageCollectorError>`) — both routed through `cpt-cf-usage-collector-component-deactivation-handler` — by transitioning the target row's `status` column from `active` to `inactive` while leaving every other column byte-identical to its pre-call value. When the target row has `corrects_id IS NULL`, the same atomic transition cascades depth-1 to every active row whose `corrects_id` equals the target id (every such row has `corrects_id IS NOT NULL`), flipping every selected row's `status` from `active` to `inactive` in the **same** atomic step; the set of cascade-flipped ids is not part of the return shape and operators that need to enumerate them issue a follow-up `list_usage_records` query against the `status` / `corrects_id` columns. Deactivation MUST be one-way (no reactivation operation exists for any row) and a second deactivation against an already-inactive record MUST be rejected with `context.reason="ALREADY_INACTIVE"` per `usage-collector-v1.yaml`.

**Implements**:

- `cpt-cf-usage-collector-flow-event-deactivation-deactivate-record`
- `cpt-cf-usage-collector-flow-event-deactivation-cascade`
- `cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization`
- `cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch`
- `cpt-cf-usage-collector-algo-event-deactivation-atomic-outcome-mapping`
- `cpt-cf-usage-collector-algo-event-deactivation-atomic-cascade-flip`

**Constraints**: `cpt-cf-usage-collector-fr-event-deactivation`

**Touches**:

- API: `POST /usage-collector/v1/records/{id}/deactivate`
- Entities: `UsageRecord`, `UsageRecordStatus`, `SecurityContext`

### FR: Usage Compensation (Cascade Cross-Link)

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-event-deactivation-fr-usage-compensation`

The system **MUST** honor the cascade obligation that `cpt-cf-usage-collector-fr-usage-compensation` imposes on the deactivation feature: when an operator deactivates a row R with `corrects_id IS NULL` that has one or more active rows referencing it via `corrects_id` (each such row has `corrects_id IS NOT NULL` by construction), the Plugin SPI Method 5 capability MUST flip R **and** every such active referencing row from `active` to `inactive` in the **same** atomic transition. The set of cascade-flipped row ids is not part of the return shape (the REST response is HTTP 204 No Content; the SDK trait returns `Ok(())`); callers that need to reconcile their downstream ledgers issue a follow-up `list_usage_records` query against the `status` / `corrects_id` columns. The compensation primitive itself (counter value-reversal: caller-driven, append-only, signed-negative `value` on the unified ingestion path) is **not implemented by this feature** — its flow is inlined into `features/usage-emission.md` per the `feature_doc_shape = inline-in-emission` decision; this DoD only realises the cascade leg that deactivation owes to compensation rows. Compensating a compensation is a non-goal, so deactivating a row with `corrects_id IS NOT NULL` is structurally single-row (no cascade).

**Implements**:

- `cpt-cf-usage-collector-flow-event-deactivation-cascade`
- `cpt-cf-usage-collector-algo-event-deactivation-atomic-cascade-flip`
- `cpt-cf-usage-collector-algo-event-deactivation-concurrency-guard`
- `cpt-cf-usage-collector-state-event-deactivation-record-lifecycle`

**Constraints**: `cpt-cf-usage-collector-fr-usage-compensation`

**Touches**:

- API: `POST /usage-collector/v1/records/{id}/deactivate`
- Entities: `UsageRecord`, `UsageRecordStatus`

### NFR: Availability

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-event-deactivation-nfr-availability`

The system **MUST** keep the deactivation endpoint available within the PRD-declared availability budget (99.95% monthly) by running `cpt-cf-usage-collector-component-deactivation-handler` inside the same stateless `cpt-cf-usage-collector-topology-gear-runtime` instances that serve ingestion and query, by reaching durable state exclusively through the ClientHub-bound plugin via `cpt-cf-usage-collector-component-plugin-host`, and by surfacing every Plugin SPI transport / readiness / persistence error as a deterministic `Problem` envelope with `context.reason="PLUGIN_READINESS"` so callers can retry idempotently — the same `id` re-submitted after a transient SPI fault is structurally idempotent because the Plugin SPI Method 5 capability surfaces the `UsageRecordAlreadyInactive` error variant (not `Ok(())`) on the retry that follows a successful prior transition. The handler MUST NOT serve a parallel cache and MUST NOT invent a binding when the plugin host is unreachable.

**Implements**:

- `cpt-cf-usage-collector-flow-event-deactivation-deactivate-record`
- `cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch`

**Constraints**: `cpt-cf-usage-collector-nfr-availability`

**Touches**:

- API: `POST /usage-collector/v1/records/{id}/deactivate`

### Principle: Monotonic Deactivation

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-event-deactivation-principle-monotonic-deactivation`

The system **MUST** realize `cpt-cf-usage-collector-principle-monotonic-deactivation` end-to-end on the deactivation path — `cpt-cf-usage-collector-component-deactivation-handler` MUST issue exactly the one-way `Active → Inactive` `status` transition through the Plugin SPI Method 5 capability, MUST NOT mutate any other attribution field on the affected records, MUST NOT expose any reactivation operation in either the REST surface (`usage-collector-v1.yaml`) or the SDK trait surface (`sdk-trait.md`), and MUST reject second deactivation against an already-inactive record with `context.reason="ALREADY_INACTIVE"` per `usage-collector-v1.yaml` — preserving the substrate's freedom from mutable-record semantics so storage plugins, query consumers, and aggregation pipelines can reason about active/inactive as a first-class monotonic lifecycle event.

**Implements**:

- `cpt-cf-usage-collector-flow-event-deactivation-deactivate-record`
- `cpt-cf-usage-collector-state-event-deactivation-record-lifecycle`
- `cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch`
- `cpt-cf-usage-collector-algo-event-deactivation-atomic-outcome-mapping`

**Constraints**: `cpt-cf-usage-collector-principle-monotonic-deactivation`

**Touches**:

- API: `POST /usage-collector/v1/records/{id}/deactivate`
- Entities: `UsageRecord`, `UsageRecordStatus`

### Principle: Fail Closed

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-dod-event-deactivation-principle-fail-closed`

The system **MUST** realize `cpt-cf-usage-collector-principle-fail-closed` on the deactivation path — `cpt-cf-usage-collector-component-deactivation-handler` MUST treat the absence of an inbound `SecurityContext` as `unauthenticated` (returning the canonical `Unauthenticated` `Problem` envelope; on REST this occurs when the ToolKit gateway middleware did not populate `Extension<SecurityContext>`, on SDK it occurs when the trait method was invoked without a `ctx` argument), MUST treat `cpt-cf-usage-collector-contract-authz-resolver` unavailability as `deny` (returning the canonical `PermissionDenied` `Problem` envelope) without consulting any cached decision and without applying any permissive fallback, MUST treat Plugin SPI unavailability (`PluginUnavailable`, `Timeout`, `BackendError`) as `context.reason="PLUGIN_READINESS"` rejection without inferring a successful transition, and MUST NEVER synthesize an operator identity, invent a plugin binding, or fabricate a successful deactivation result when any downstream collaborator is unreachable per DECOMPOSITION §2.5 "Fail-closed posture".

**Implements**:

- `cpt-cf-usage-collector-flow-event-deactivation-deactivate-record`
- `cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization`
- `cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch`

**Constraints**: `cpt-cf-usage-collector-principle-fail-closed`

**Touches**:

- API: `POST /usage-collector/v1/records/{id}/deactivate`
- Component: `cpt-cf-usage-collector-component-deactivation-handler`, `cpt-cf-usage-collector-component-plugin-host`
- Entities: `SecurityContext`, `PdpDecision`

### ADR: Monotonic Deactivation

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-event-deactivation-adr-monotonic-deactivation`

The system **MUST** honor `cpt-cf-usage-collector-adr-monotonic-deactivation` by exposing exactly one lifecycle transition (`active → inactive`) through exactly one capability surface (`POST /usage-collector/v1/records/{id}/deactivate` plus the SDK `deactivate_usage_record` operation) routed through exactly one component (`cpt-cf-usage-collector-component-deactivation-handler`) backed by exactly one Plugin SPI capability (`deactivate_usage_record` per `plugin-spi.md` Method 5); the system MUST NOT introduce a reactivation operation, MUST NOT introduce a bulk-by-query deactivation operation, MUST NOT introduce a field-edit operation that mutates any attribution field other than `status`, and MUST NOT introduce a hard-delete operation for persisted usage records — the storage plugin owns physical retention / archival / purge, and corrections beyond the monotonic deactivation pattern are expressed as a deactivation plus a fresh idempotency-keyed re-emission per DESIGN §3.9.5 correction posture.

**Implements**:

- `cpt-cf-usage-collector-flow-event-deactivation-deactivate-record`
- `cpt-cf-usage-collector-state-event-deactivation-record-lifecycle`

**Constraints**: `cpt-cf-usage-collector-adr-monotonic-deactivation`

**Touches**:

- API: `POST /usage-collector/v1/records/{id}/deactivate`
- Entities: `UsageRecord`, `UsageRecordStatus`

### ADR: Usage Compensation (Cascade Companion)

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-event-deactivation-adr-usage-compensation`

The system **MUST** honor `cpt-cf-usage-collector-adr-usage-compensation` on the deactivation path by recognising that compensations are independent first-class rows (a `UsageRecord` with `corrects_id` set to an active row whose `corrects_id IS NULL` in the same `(tenant_id, gts_id)`, with a strictly-negative `value`) ingested through the unified path inlined in `features/usage-emission.md` — and by flipping every active referencing compensation alongside a deactivated usage row (`corrects_id IS NULL`) in the depth-1 cascade. The feature MUST NOT introduce a dedicated compensate REST path, SDK method, or Plugin SPI call (the unified ingestion path is the sole compensation surface), MUST NOT validate or enforce non-negative `SUM` at deactivation time (the un-policed-net posture per `cpt-cf-usage-collector-adr-usage-compensation` is preserved), and MUST NOT permit a row whose `corrects_id` references a row that itself has `corrects_id IS NOT NULL` (deactivating a row with `corrects_id IS NOT NULL` is single-row, no cascade, per the ADR's compensating-a-compensation non-goal).

**Implements**:

- `cpt-cf-usage-collector-flow-event-deactivation-cascade`
- `cpt-cf-usage-collector-algo-event-deactivation-atomic-cascade-flip`
- `cpt-cf-usage-collector-algo-event-deactivation-concurrency-guard`
- `cpt-cf-usage-collector-state-event-deactivation-record-lifecycle`

**Constraints**: `cpt-cf-usage-collector-adr-usage-compensation`

**Touches**:

- API: `POST /usage-collector/v1/records/{id}/deactivate`
- Entities: `UsageRecord`, `UsageRecordStatus`

### Constraint: No Business Logic

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-dod-event-deactivation-constraint-no-business-logic`

The system **MUST** keep the deactivation path free of billing, pricing, quota enforcement, per-UsageType accounting transforms, and per-tenant business-rule interpretation; `cpt-cf-usage-collector-component-deactivation-handler` MUST NOT consult any per-UsageType or per-tenant pricing table, MUST NOT trigger a counter rollback or gauge recomputation as a side-effect of deactivation (downstream consumers MUST recompute aggregates by excluding `inactive` rows themselves), and MUST NOT mutate the `value` column or any other column other than `status` on the targeted row or on any cascade-flipped compensation row. Business logic — billing reversal, quota credit, customer-facing notifications — is owned by callers and downstream consumers, never by the metering substrate.

**Recording-not-computing (symmetric with `+value` recording, cross-reference to the compensation primitive)**: deactivation **records** a caller-supplied retraction action (an operator-initiated `Active → Inactive` flip plus the depth-1 cascade derived deterministically from `corrects_id` referential identity); it does **not** compute the financial consequence of that retraction. The same recording-not-computing posture governs the complementary compensation primitive on the unified ingestion path: a caller-supplied row with `corrects_id` set and a strictly-negative `value` is **recorded** verbatim (symmetric with a `+value` row whose `corrects_id IS NULL`) and the collector does NOT validate non-negative net at write time and does NOT emit a negative-net detection signal. See `cpt-cf-usage-collector-fr-usage-compensation`, `cpt-cf-usage-collector-adr-usage-compensation` (un-policed-net stance). The compensation flow is **inlined into `features/usage-emission.md`** — no separate FEATURE file exists.

**Implements**:

- `cpt-cf-usage-collector-flow-event-deactivation-deactivate-record`
- `cpt-cf-usage-collector-flow-event-deactivation-cascade`
- `cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch`
- `cpt-cf-usage-collector-algo-event-deactivation-atomic-cascade-flip`

**Constraints**: `cpt-cf-usage-collector-constraint-no-business-logic`

**Touches**:

- API: `POST /usage-collector/v1/records/{id}/deactivate`

### Component: Deactivation Handler

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-event-deactivation-component-deactivation-handler`

The system **MUST** realize `cpt-cf-usage-collector-component-deactivation-handler` as the sole synchronous entry point for status-only deactivation of `UsageRecord` rows (REST and SDK), owning the deactivation contract end-to-end — SecurityContext acceptance at both entry points (REST handler with `Extension<SecurityContext>` from ToolKit gateway middleware via `OperationBuilder::authenticated()`; SDK trait `deactivate_usage_record(ctx, ...)` with `ctx: &SecurityContext` as the first parameter), Plugin SPI Method 10 `get_usage_record` prefetch for attribution-tuple resolution, resource-attribute PDP enforcement via the shared `authorize_usage_record(ctx, &record, DEACTIVATE)` helper against `cpt-cf-usage-collector-contract-authz-resolver`, Plugin SPI Method 5 dispatch, atomic-outcome mapping into the HTTP 204 No Content success response or the actionable error envelopes — while delegating persistence to `cpt-cf-usage-collector-component-plugin-host`, with no field-edit capabilities, no reactivation path, no record deletion, no PDP-decision caching, no synthesized identities, and no invented plugin bindings per DESIGN §3.5 Deactivation Handler component description.

**Implements**:

- `cpt-cf-usage-collector-flow-event-deactivation-deactivate-record`
- `cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization`
- `cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch`
- `cpt-cf-usage-collector-algo-event-deactivation-atomic-outcome-mapping`

**Constraints**: `cpt-cf-usage-collector-component-deactivation-handler`

**Touches**:

- API: `POST /usage-collector/v1/records/{id}/deactivate`
- Entities: `UsageRecord`, `UsageRecordStatus`

### Sequence: Deactivate Usage Event

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-event-deactivation-seq-deactivate-event`

The system **MUST** implement the `cpt-cf-usage-collector-seq-deactivate-event` sequence end-to-end per DESIGN §3.6: operator surface (REST handler receiving `Extension<SecurityContext>` from ToolKit gateway middleware, or SDK trait `deactivate_usage_record(ctx, ...)` with `ctx: &SecurityContext` first) → Deactivation Handler PDP authorization via the per-component `authz_scope` helper against `cpt-cf-usage-collector-contract-authz-resolver` → Deactivation Handler dispatch → Plugin Host → storage plugin `deactivate_usage_record` against the target `id` → atomic result (`Ok(())` on a successful transition, or `UsageRecordAlreadyInactive` / `UsageRecordNotFound` as deterministic rejection error variants) → deterministic operator response (HTTP 204 No Content on success, or a deterministic `Problem` envelope); PDP denial, already-inactive target, not-found target, and SPI errors all reject before any column other than `status` is touched, and inactive records remain queryable through the §2.4 Query Gateway as required by the sequence description.

**Implements**:

- `cpt-cf-usage-collector-flow-event-deactivation-deactivate-record`
- `cpt-cf-usage-collector-state-event-deactivation-record-lifecycle`

**Constraints**: `cpt-cf-usage-collector-seq-deactivate-event`

**Touches**:

- API: `POST /usage-collector/v1/records/{id}/deactivate`


### Entity: Usage Record

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-event-deactivation-entity-usage-record`

The system **MUST** treat `UsageRecord` on the deactivation path as an append-only-after-acceptance entity whose only mutable surface is the `status` column governed by `UsageRecordStatus` per DESIGN §3.1; `cpt-cf-usage-collector-component-deactivation-handler` MUST NOT instantiate, re-validate, or rewrite any other field of the targeted entity, MUST NOT generate a new `id` (the SPI capability accepts the existing `id` as input), and MUST forward exactly `id` through the Plugin SPI Method 5 capability per `plugin-spi.md` Method 5 ("Structural inputs: the target `UsageRecord.uuid`"). The persisted post-call row carries the same `tenant_id`, `resource_id`, `resource_type`, `subject_id`, `subject_type`, `gts_id`, `value`, `created_at`, `idempotency_key`, and `metadata` it carried before the call.

**Implements**:

- `cpt-cf-usage-collector-flow-event-deactivation-deactivate-record`
- `cpt-cf-usage-collector-algo-event-deactivation-monotonic-transition-dispatch`

**Constraints**: `UsageRecord`

**Touches**:

- Entities: `UsageRecord`

### Entity: Deactivation Status

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-event-deactivation-entity-deactivation-status`

The system **MUST** treat `UsageRecordStatus` per DESIGN §3.1 and `domain-model.md` §2.9 as a closed two-valued lifecycle marker (`active`, `inactive`) on the `usage_records.status` column bound to a `UsageRecord` whose only permitted transition is `active → inactive`; `cpt-cf-usage-collector-component-deactivation-handler` MUST set the column value via the Plugin SPI Method 5 atomic capability (no client-side write, no read-modify-write loop), MUST surface a successful `Active → Inactive` transition as HTTP 204 No Content on the REST surface (no body) and `Ok(())` on the SDK surface per `usage-collector-v1.yaml` / `sdk-trait.md`, MUST surface already-inactive rejections as the actionable `context.reason="ALREADY_INACTIVE"` error envelope translated from the `UsageRecordAlreadyInactive` plugin error variant (preserving the no-reactivation invariant), and MUST NEVER leave the row in the `active` state as the post-call state of a successful transition.

**Implements**:

- `cpt-cf-usage-collector-flow-event-deactivation-deactivate-record`
- `cpt-cf-usage-collector-state-event-deactivation-record-lifecycle`
- `cpt-cf-usage-collector-algo-event-deactivation-atomic-outcome-mapping`

**Constraints**: `UsageRecordStatus`

**Touches**:

- API: `POST /usage-collector/v1/records/{id}/deactivate`
- Entities: `UsageRecordStatus`

### Entity: Security Context

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-event-deactivation-entity-security-context`

The system **MUST** consume `SecurityContext` (see `domain-model.md` §2.7) as the platform-resolved caller-identity envelope (operator principal, operator's tenant scope, auxiliary claims) — never owned, synthesized, or cached by `cpt-cf-usage-collector-component-deactivation-handler`. The handler MUST accept the `SecurityContext` exclusively at one of the two convention-bound entry points — on REST as `Extension<SecurityContext>` populated by ToolKit gateway middleware (`OperationBuilder::authenticated()`) and on the SDK trait as `ctx: &SecurityContext` passed as the first parameter to `deactivate_usage_record(ctx, ...)` — and pass it verbatim to `cpt-cf-usage-collector-flow-foundation-pdp-authorize` (shared `authorize_usage_record` helper invoking `PolicyEnforcer::access_scope_with(ctx, &usage_record::RESOURCE, actions::DEACTIVATE, None, &request)` against `cpt-cf-usage-collector-contract-authz-resolver`) so PDP authorizes the operator's identity against the deactivation attribution tuple (operator identity + the pre-fetched record's `tenant_id`, `resource_ref`, optional `subject_ref`), and fail closed on missing `SecurityContext` or PDP unavailability. The `SecurityContext` is the subject of PDP authorization for the deactivation request — no operator role table is held gear-local per DESIGN §3.9.4 ABAC-anchored authorization.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-pdp-authorize`
- `cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization`

**Constraints**: `SecurityContext`

**Touches**:

- Component: `cpt-cf-usage-collector-component-deactivation-handler`
- Entities: `SecurityContext`

### API: POST /usage-collector/v1/records/{id}/deactivate

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-event-deactivation-api-post-records-id-deactivate`

The system **MUST** expose `POST /usage-collector/v1/records/{id}/deactivate` as the sole REST entry point for individual usage-record deactivation per `usage-collector-v1.yaml`, with the REST handler receiving `Extension<SecurityContext>` populated by ToolKit gateway middleware (`OperationBuilder::authenticated()`) and delegating to `UsageCollectorClientV1::deactivate_usage_record(ctx, ...)` (`ctx: &SecurityContext` as first parameter per `sdk-trait.md` Method 5), accepting no request body (the target `id` is the path parameter), returning HTTP `204 No Content` (empty body) on successful transition, and surfacing deterministic `Problem` envelopes through the yaml's `default` response for every failure case — canonical `Unauthenticated` (no `SecurityContext` present at the handler boundary), canonical `PermissionDenied` (PDP `deny` from `cpt-cf-usage-collector-contract-authz-resolver`), `context.reason="ALREADY_INACTIVE"` (Plugin SPI Method 5 surfaced `UsageRecordAlreadyInactive`), canonical `NotFound` (Plugin SPI Method 10 prefetch returned `Err(UsageRecordNotFound { id })` OR Plugin SPI Method 5 surfaced `UsageRecordNotFound` in the rare race after a successful prefetch), and `context.reason="PLUGIN_READINESS"` (Plugin SPI transport / readiness / persistence error). The handler MUST NOT widen the contract beyond what is declared in the yaml and MUST NOT introduce alternative status-mutation routes outside this single endpoint. The runtime-emitted OpenAPI document produced by `OpenApiRegistryImpl` MUST remain drift-free against the yaml per DESIGN §3.3 D1.

**Implements**:

- `cpt-cf-usage-collector-flow-event-deactivation-deactivate-record`

**Constraints**: `cpt-cf-usage-collector-fr-event-deactivation`

**Touches**:

- API: `POST /usage-collector/v1/records/{id}/deactivate`
- Entities: `UsageRecord`, `UsageRecordStatus`

### §2.5-item → DoD-ID Coverage Matrix

Coverage of every DECOMPOSITION §2.5 catalog item:

| §2.5 Item                                                           | Kind              | DoD ID                                                                           |
| ------------------------------------------------------------------- | ----------------- | -------------------------------------------------------------------------------- |
| `cpt-cf-usage-collector-fr-event-deactivation`                      | FR                | `cpt-cf-usage-collector-dod-event-deactivation-fr-event-deactivation`            |
| `cpt-cf-usage-collector-fr-usage-compensation` (cascade leg)        | FR                | `cpt-cf-usage-collector-dod-event-deactivation-fr-usage-compensation`            |
| `cpt-cf-usage-collector-nfr-availability`                           | NFR               | `cpt-cf-usage-collector-dod-event-deactivation-nfr-availability`                 |
| `cpt-cf-usage-collector-principle-monotonic-deactivation`           | Principle         | `cpt-cf-usage-collector-dod-event-deactivation-principle-monotonic-deactivation` |
| `cpt-cf-usage-collector-principle-fail-closed`                      | Principle         | `cpt-cf-usage-collector-dod-event-deactivation-principle-fail-closed`            |
| `cpt-cf-usage-collector-adr-monotonic-deactivation`                 | ADR               | `cpt-cf-usage-collector-dod-event-deactivation-adr-monotonic-deactivation`       |
| `cpt-cf-usage-collector-adr-usage-compensation` (cascade companion) | ADR               | `cpt-cf-usage-collector-dod-event-deactivation-adr-usage-compensation`           |
| `cpt-cf-usage-collector-constraint-no-business-logic`               | Design constraint | `cpt-cf-usage-collector-dod-event-deactivation-constraint-no-business-logic`     |
| `cpt-cf-usage-collector-component-deactivation-handler`             | Design component  | `cpt-cf-usage-collector-dod-event-deactivation-component-deactivation-handler`   |
| `cpt-cf-usage-collector-seq-deactivate-event`                       | Sequence          | `cpt-cf-usage-collector-dod-event-deactivation-seq-deactivate-event`             |
| `UsageRecord` (status only)          | Entity            | `cpt-cf-usage-collector-dod-event-deactivation-entity-usage-record`              |
| `UsageRecordStatus`                 | Entity            | `cpt-cf-usage-collector-dod-event-deactivation-entity-deactivation-status`       |
| `SecurityContext`                    | Entity            | `cpt-cf-usage-collector-dod-event-deactivation-entity-security-context`          |
| `POST /usage-collector/v1/records/{id}/deactivate`                  | API               | `cpt-cf-usage-collector-dod-event-deactivation-api-post-records-id-deactivate`   |

## 6. Acceptance Criteria

- [ ] `p1` - A well-formed deactivation request by an authorized platform operator through `POST /usage-collector/v1/records/{id}/deactivate` or through the SDK `deactivate_usage_record` operation transitions the targeted record's `status` from `active` to `inactive` via a single Plugin SPI Method 5 `deactivate_usage_record` capability invocation that returns `Ok(())`; the post-call record's `tenant_id`, `resource_id`, `resource_type`, `subject_id`, `subject_type`, `gts_id`, `value`, `created_at`, `idempotency_key`, `corrects_id`, and `metadata` attribution is byte-identical to the pre-call values, and the REST response is HTTP `204 No Content` (empty body) per `usage-collector-v1.yaml` (status-only transition). The acceptance criterion applies uniformly when the target row has `corrects_id IS NULL` (cascade may flip companions) AND when the target row has `corrects_id IS NOT NULL` (single-row, no cascade).
- [ ] `p1` - Deactivating a row R with `corrects_id IS NULL` that has N (N ≥ 1) active rows whose `corrects_id = R.id ∧ same (tenant_id, gts_id)` (every such row has `corrects_id IS NOT NULL` by construction) flips R AND all N referencing compensations from `active` to `inactive` in a **single atomic** Plugin SPI Method 5 transition; the set of N cascade-flipped compensation ids is not part of the return shape (the REST response is HTTP 204 No Content) and a follow-up `list_usage_records` query against the `status` / `corrects_id` columns enumerates them. A follow-up `SUM(value)` over `(tenant_id, gts_id)` returns to the pre-acceptance baseline (depth-1 cascade).
- [ ] `p1` - Deactivating a row C with `corrects_id IS NOT NULL` flips ONLY C — no cascade target lookup is performed — and the REST response is HTTP 204 No Content; this is structural per the compensating-a-compensation non-goal in `cpt-cf-usage-collector-adr-usage-compensation` (single-row deactivation).
- [ ] `p1` - A compensation ingestion submission referencing R that arrives while R is being deactivated is rejected by the L1 "referenced record must be active" check enforced on the ingestion path inlined in `features/usage-emission.md`; either the compensation serialises before the deactivation commit and is included in the atomic cascade flip, or it serialises after the commit and is rejected — no compensation can be admitted referencing an `inactive` row, and no row's `status` changes outside the atomic cascade transition (concurrency safety without distributed coordination).
- [ ] `p1` - A second deactivation request targeting an already-inactive record (the Plugin SPI Method 5 capability surfaced the `UsageRecordAlreadyInactive` error variant) is surfaced as the `Problem` envelope with `context.reason="ALREADY_INACTIVE"` per `usage-collector-v1.yaml` and the SDK `AlreadyInactive` error variant per `sdk-trait.md` Method 5; the row's `status` column remains `inactive` and no other column is mutated (monotonicity). **Test**: `deactivate_usage_record_plugin_already_inactive_lifts_to_sdk_already_inactive`.
- [ ] `p1` - A deactivation request targeting a non-existent `id` is surfaced as the canonical `NotFound` `Problem` envelope per `usage-collector-v1.yaml` and the SDK `UsageRecordNotFound` error variant per `sdk-trait.md` Method 5; no state change occurs (not-found handling). The miss can be detected by Plugin SPI Method 10 `get_usage_record(id)` surfacing `Err(UsageRecordNotFound { id })` during the host-side prefetch (common case) OR by Plugin SPI Method 5 surfacing `UsageRecordNotFound { id }` after a successful prefetch (race: the row was deactivated/purged concurrently between prefetch and Method 5 dispatch). **Tests**: `deactivate_usage_record_prefetch_not_found_skips_pdp_and_spi`, `deactivate_usage_record_plugin_not_found_lifts_to_sdk_not_found`.
- [ ] `p1` - Every deactivation request accepts a resolved `SecurityContext` at the handler boundary — on REST as `Extension<SecurityContext>` populated by ToolKit gateway middleware (`OperationBuilder::authenticated()`), on the SDK trait as `ctx: &SecurityContext` first parameter — and dispatches PDP authorization through `cpt-cf-usage-collector-flow-foundation-pdp-authorize` (shared `authorize_usage_record` helper against `cpt-cf-usage-collector-contract-authz-resolver`) against the deactivation attribution tuple (operator identity + the pre-fetched record's `tenant_id`, `resource_ref`, optional `subject_ref`) before any Plugin SPI Method 5 dispatch; absence of `SecurityContext` at the boundary surfaces the canonical `Unauthenticated` `Problem` envelope per the yaml `default` response (framework-enforced by `OperationBuilder::authenticated()` + axum `Extension<SecurityContext>` extractor), a PDP `deny` surfaces the canonical `PermissionDenied` `Problem` envelope per the yaml `default` response, and no row is mutated in either case (PDP-gated authorization). **Tests**: `deactivate_usage_record_pdp_deny_short_circuits_before_plugin`, `deactivate_usage_record_pdp_unreachable_fails_closed_before_plugin`.
- [ ] `p1` - A Plugin SPI transport / readiness / persistence error (`PluginUnavailable`, `Timeout`, `BackendError`) from the Method 5 capability surfaces as the canonical `Problem` envelope with `context.reason="PLUGIN_READINESS"` per `usage-collector-v1.yaml`; the row's `status` column is unchanged, the operator can retry idempotently with the same `id`, and a retry after a successful prior transition is structurally idempotent because the SPI capability surfaces the `UsageRecordAlreadyInactive` error variant (not `Ok(())`) on the retry (fail-closed plus idempotent retry). The same envelope shape (`PLUGIN_READINESS`) applies when the Plugin SPI Method 10 prefetch surfaces a transport / readiness fault. **Tests**: `deactivate_with_unreachable_pdp_carries_plugin_readiness_reason`, `deactivate_usage_record_plugin_timeout_lifts_to_plugin_timeout_envelope`, `deactivate_usage_record_prefetch_timeout_propagates`.
- [ ] `p1` - A successfully deactivated record remains visible to the §2.4 Query Gateway with `status="inactive"` — both the raw query path (`GET /usage-collector/v1/records`) and the aggregated query path (`POST /usage-collector/v1/records/aggregate`) return the row within the PDP-authorized scope and DECOMPOSITION §2.4 "Active-and-inactive record visibility"; the row is NEVER physically deleted by the deactivation handler — physical retention, archival, and purge are owned by the active storage plugin's deployment profile (queryability preservation). **Cross-feature**: the visibility half is owned by the (not-yet-implemented) `usage-query` feature; the deactivation feature itself MUST NOT delete the row, which is structurally guaranteed by Method 5's status-only contract — but end-to-end queryability cannot be marked `[ ]` until `usage-query` lands.
- [ ] `p1` - No reactivation path exists in either the REST surface (`usage-collector-v1.yaml` has no `inactive → active` endpoint) or the SDK trait surface (`sdk-trait.md` has no reactivation method); the one-way `active → inactive` latch applies uniformly to primary rows AND to rows flipped by the depth-1 cascade (regardless of `corrects_id` presence) — any caller-side attempt to construct such a request is structurally impossible on the published contract surface, and any subsequent deactivation against the same `id` (whether primary or previously-cascaded compensation) returns `context.reason="ALREADY_INACTIVE"` rather than re-entering the `Active` state (no-reactivation invariant). **Anchor**: marker `inst-state-no-reactivation` on the SDK trait `deactivate_usage_record` signature in `usage-collector-sdk/src/api.rs`; the SDK trait surface enumerates the only mutation method.
