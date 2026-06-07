---
status: superseded
superseded_by: cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference
date: 2026-05-26
decision-makers: Usage Collector module owners
---

> Superseded by [0012](./0012-unified-plugin-catalog-and-gts-id-reference.md).

# Gateway-local metric catalog for Usage Collector

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Gateway-local catalog DB with no SPI referential check](#gateway-local-catalog-db-with-no-spi-referential-check)
  - [Keep catalog on the plugin side](#keep-catalog-on-the-plugin-side)
- [More Information](#more-information)
  - [Domain applicability](#domain-applicability)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-usage-collector-adr-gateway-local-metric-catalog`

## Context and Problem Statement

The Usage Collector currently routes Metric catalog ownership through the storage plugin via four Plugin SPI methods (`write_metric`, `read_metric`, `list_metrics`, `delete_metric_if_unreferenced`), exposes catalog operations on the REST surface only, and depends on a `catalog-projection-refresh` algorithm to keep an in-process projection consistent with the plugin's `metrics_catalog` table. That projection-refresh algorithm has no defined call site: the catalog is only mutated by configuration at boot and by REST requests at runtime, so there is nothing to "refresh from" between those events. The module also diverges from the platform's system-module convention (e.g. `account-management`), where local DB + Policy Decision Point (PDP) + SDK trait sit together inside the gateway, and where REST handlers and SDK trait impls share a single domain service.

## Decision Drivers

- Alignment with the platform pattern (`account-management`): local DB + PDP + SDK trait inside the gateway-side system module.
- Removal of unnecessary Plugin SPI (Service Provider Interface) surface and projection machinery whose call sites do not exist.
- A single in-process catalog surface for REST handlers and SDK consumers, with `SecurityContext`-based authorization performed inline via the platform's PDP.
- A delete path that avoids any plugin coupling: the operator is the platform-trusted role for catalog mutation, and referencing-records validation is intentionally not performed.

## Considered Options

- Gateway-local catalog DB with no SPI referential check — the catalog lives in the gateway module's local database behind a SeaORM `MetricCatalogRepo`; delete is a local-DB operation only; REST and SDK trait share a single domain service.
- Keep catalog on the plugin side — preserve the four catalog SPI methods, the projection-refresh algorithm, and the REST-only surface unchanged. `ADR-0002 Pluggable Storage` continues to cover the metric catalog as well as usage records.

## Decision Outcome

Chosen option: "Gateway-local catalog DB with no SPI referential check", because it matches the platform's system-module convention, removes five Plugin SPI methods (`write_metric`, `read_metric`, `list_metrics`, `delete_metric_if_unreferenced`, `check_metric_referenced`) and the `catalog-projection-refresh` algorithm whose call site never materialized, unifies the REST and SDK surfaces on a single domain service over a SeaORM `MetricCatalogRepo`, and avoids any plugin coupling on the delete path because deletion is a local-DB operation and referencing-records validation is intentionally not performed (the operator is the platform-trusted role for catalog mutation). This ADR narrows `ADR-0002 Pluggable Storage` to usage-record ingestion, query, and deactivation only; the Metric catalog is explicitly out of scope for the storage plugin going forward. The two-state Metric lifecycle (`active → deleted`) is unchanged — no `deleting` intermediate state and no new transitions are introduced.

### Consequences

- The Plugin SPI removes five catalog methods: `write_metric`, `read_metric`, `list_metrics`, `delete_metric_if_unreferenced`, and `check_metric_referenced`. The associated error variants (`DuplicateMetric`, `MetricDeletionOutcome::{Deleted,Referenced,NotFound}`) and the `metrics_catalog` clause in the plugin-owned "logical tables" section are removed from the SPI surface. The `check_metric_referenced` method is dropped together with the referencing-records deletion constraint, which is no longer enforced.
- The gateway module owns a new `MetricCatalogRepo` over `modkit-db::DBProvider<UsageCollectorError>` with SeaORM entities, a `metric_catalog` table, embedded migrations shipped in the module crate, and a unique constraint on `gts_id`. `Module::init` wires the repo via `ctx.db_required()?`, matching the `account-management` template.
- The SDK trait gains four catalog methods — `register_metric`, `delete_metric`, `list_metrics`, `get_metric` — each taking `ctx: &SecurityContext` as the first argument. REST handlers and the SDK trait impl share the same domain service (`MetricCatalogService`) over the local repo, so authorization, validation, and persistence run once per request regardless of surface.
- The previously documented in-process projection becomes a write-through in-memory kind/shape index, populated at boot from the local DB and updated synchronously on every register / delete. There is no TTL, no reconciliation tick, and no cold-projection cold-start branch on the list path.
- The `usage_collector.metrics_catalog.age` observability signal is removed (no async refresh, therefore no staleness). `usage_collector.metrics_catalog.size` is retained but reflects the local DB row count.
- The `metrics_catalog` logical table moves from the plugin-owned set to the module-owned set; its backup and disaster-recovery posture flips from "plugin-owned, see plugin docs" to "module-owned, under the platform DB's backup regime". `usage_records` backup remains plugin-owned.
- The delete path is local-only: PDP authorize; reject declared-catalog `gts_id` as `declared_metric_immutable`; `Repo.find(gts_id)` → 404 if absent; otherwise `Repo.delete(gts_id)` and synchronous in-memory index update. No SPI call, no tombstone, no state column, no second transaction.
- This ADR narrows `ADR-0002 Pluggable Storage` to usage-record ingestion, query, and deactivation. ADR-0002 is not superseded; its scope is reduced, and the carve-out is recorded explicitly in `cpt-cf-usage-collector-fr-pluggable-storage`.

### Confirmation

Compliance is confirmed through (a) design review against this ADR confirming that no catalog SPI methods remain on the plugin surface, that `MetricCatalogRepo` is the sole catalog persistence seam, and that REST handlers and the SDK trait impl share the `MetricCatalogService`; (b) `cfs validate` on this ADR file and on every back-referencing artifact; (c) the downstream artifact updates that back-reference this ADR — `DESIGN.md`, `PRD.md`, `DECOMPOSITION.md`, `plugin-spi.md`, `sdk-trait.md`, `features/foundation.md`, `features/metric-lifecycle.md`, `domain-model.md`, and `usage-collector-v1.yaml`.

## Pros and Cons of the Options

### Gateway-local catalog DB with no SPI referential check

The catalog table lives in the gateway module's local database behind a SeaORM `MetricCatalogRepo`. Delete is a local-DB operation only — no plugin round-trip. REST and SDK trait share one domain service.

- Good, because catalog ownership matches every other gateway-side system module (`account-management` is the canonical template), so engineers and operators meet a familiar shape rather than a one-off plugin-owned pattern.
- Good, because five Plugin SPI methods and the entire `catalog-projection-refresh` algorithm disappear, shrinking both the SPI surface and the module's algorithmic complexity.
- Good, because REST and SDK consumers reach catalog ops through a single in-process surface with one authorization site, one validation site, and one persistence site.
- Good, because the delete path has no plugin coupling and no cross-component coordination, keeping the deletion operation entirely local to the gateway module.
- Bad, because referencing usage records are not validated at delete time; the operator is trusted as the platform-trusted role for catalog mutation, and any historical references in `usage_records` remain queryable with caller-supplied attribution intact (per ADR-0003).
- Bad, because the module now declares a database capability and ships its own migrations, adding a small operational surface that the prior plugin-owned design avoided.

### Keep catalog on the plugin side

Preserve the four catalog SPI methods, the projection-refresh algorithm, and the REST-only catalog surface unchanged. `ADR-0002 Pluggable Storage` continues to cover catalog and usage records together.

- Good, because no change is required and `ADR-0002` covers the catalog unchanged.
- Good, because the gateway carries no DB capability for the catalog and inherits no migration responsibility for catalog rows.
- Bad, because the `catalog-projection-refresh` algorithm has no defined call site once it is needed; the catalog is mutated only by configuration at boot and by REST at runtime, leaving the algorithm without a trigger.
- Bad, because the SDK cannot expose catalog operations symmetrically with REST without re-introducing per-record Plugin SPI round-trips on the SDK path or duplicating logic between REST and SDK.
- Bad, because the module continues to diverge from the platform's system-module convention, which complicates onboarding and inflates the SPI surface plugin authors must implement.

## More Information

Related decisions:

- `cpt-cf-usage-collector-adr-pdp-centric-authorization` (ADR-0001) — every catalog operation authorizes via the PDP using `SecurityContext`, including the SDK trait surface introduced here.
- `cpt-cf-usage-collector-adr-pluggable-storage` (ADR-0002) — remains in force for usage-record ingestion, query, and deactivation. ADR-0007 narrows ADR-0002's scope so the pluggable storage seam no longer covers the Metric catalog.
- `cpt-cf-usage-collector-adr-caller-supplied-attribution` (ADR-0003) — historical `usage_records` rows that referenced a now-deleted `gts_id` remain queryable with caller-supplied, self-describing attribution intact; a re-registered `gts_id` with the matching kind reattaches semantically.

### Domain applicability

For each major checklist category, this ADR's applicability is recorded below:

- ARCH — Addressed: the decision restructures component ownership of the catalog (`Metrics Catalog` gains repo ownership; `Plugin Host` loses catalog write capabilities).
- PERF — Addressed: the in-memory kind/shape index is retained as a write-through cache so the latency-critical ingestion path is unchanged; the `metrics_catalog.age` cold-projection signal is removed.
- SEC — Addressed: every catalog operation (REST and SDK) authorizes via the PDP through the per-component `authz_scope` helper (per ADR-0001); no new credentials, no new secrets.
- REL — Addressed: PDP availability remains a hard dependency on the catalog operations path; gateway DB availability is a new dependency for the catalog surface, aligned with every other system module's readiness model.
- DATA — Addressed: a new module-owned `metric_catalog` table is introduced; migration ownership moves to the module crate; the table joins the platform DB's backup regime.
- INT — Addressed: five Plugin SPI methods are removed and no new SPI methods are added; the SDK trait gains four catalog methods. REST API paths are unchanged; only the Problem `reason` enum may grow.
- OPS — Addressed: backup posture for the catalog flips to module-owned; `metrics_catalog.age` observability is removed; `metrics_catalog.size` is retained.
- MAINT — Addressed: the module shrinks (five SPI methods and one algorithm removed) and aligns with the platform convention, lowering long-term maintenance cost.
- TEST — Not applicable to this ADR; conformance and integration tests are listed under Confirmation but their implementation is out of scope for an ADR per `TEST-ADR-NO-001`.
- COMPL — Not applicable: no regulated-industry obligations are altered; the change is internal-architectural.
- UX — Not applicable: no end-user-visible behavior or surface contract changes; REST paths and SDK call shapes are described here but UX impact is null.
- BIZ — Not applicable: no product-requirements impact; PRD updates that back-reference this ADR are content-relocation, not new requirements.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

- `cpt-cf-usage-collector-fr-metric-existence-and-kind` — Metric existence and kind lookups are now backed by the module's local `metric_catalog` table and an in-memory write-through kind/shape index, not by a plugin-side projection.
- `cpt-cf-usage-collector-fr-metric-registration` — Metric registration is reachable via the SDK trait (`UsageCollectorClient::register_metric`) and via the REST endpoint; both paths converge on the single domain service.
- `cpt-cf-usage-collector-fr-metric-deletion` — Metric deletion is a local-DB operation gated by PDP authorization and declared-catalog immutability; no plugin-side referential check is performed.
- `cpt-cf-usage-collector-fr-pluggable-storage` — explicit carve-out: the pluggable-storage seam applies to usage-record ingestion, query, and deactivation only; the Metric catalog is out of scope.
- `cpt-cf-usage-collector-component-metric-catalog` — owns the local `MetricCatalogRepo`, the in-memory kind/shape index, and the `MetricCatalogService` shared by REST and SDK surfaces.
