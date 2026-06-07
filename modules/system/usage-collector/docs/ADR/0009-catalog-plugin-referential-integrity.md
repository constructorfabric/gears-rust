---
status: superseded
superseded_by: cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference
date: 2026-05-30
supersedes: cpt-cf-usage-collector-adr-gateway-local-metric-catalog
---

> Superseded by [0012](./0012-unified-plugin-catalog-and-gts-id-reference.md).

# Catalog returns to plugin + referential integrity

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Catalog co-located with usage rows on the plugin](#catalog-co-located-with-usage-rows-on-the-plugin)
  - [Catalog gateway-local without FK (the current ADR-0007 status quo)](#catalog-gateway-local-without-fk-the-current-adr-0007-status-quo)
  - [Pure types-registry-as-System-of-Record (SoR)](#pure-types-registry-as-system-of-record-sor)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-usage-collector-adr-catalog-plugin-referential-integrity`

## Context and Problem Statement

PRD §1.3 commits the Usage Collector (UC) to server-side aggregation: downstream
consumers must obtain aggregated views directly from UC without running their
own aggregation layer. Sustaining that promise requires a billing-grade substrate
where the metric catalog and the `usage_records` it governs cannot be torn apart
by operator action or by horizontally-scaled gateway replicas racing on the same
mutation. ADR-0007 (`cpt-cf-usage-collector-adr-gateway-local-metric-catalog`)
moved the catalog into the gateway's local database and removed the five Plugin
SPI (Service Provider Interface) methods that previously carried catalog state
across the seam (`write_metric`, `read_metric`, `list_metrics`,
`delete_metric_if_unreferenced`, `check_metric_referenced`); deletion became
unconditional and the referencing-records check was dropped. With that split,
catalog deletes and concurrent usage writes can race across the gateway-plugin
boundary (RESEARCH-metadata.md §6.3): an emit-then-delete interleaving can
produce an orphaned usage row in the plugin's store even though the gateway's
local catalog has just rejected or removed the metric, and no in-database foreign
key (FK) can span the two stores to close the window atomically. The question is
where the metric catalog should physically live and how referential integrity
between catalog rows and usage rows should be enforced.

## Decision Drivers

- Referential integrity for a billing-grade substrate — no orphaned usage rows
  produceable by operator action; deletion of a referenced metric must be
  rejected atomically with the delete attempt.
- Persistence durability — catalog state must survive gateway restarts with the
  same guarantees as `usage_records`, including for metrics registered at
  runtime by external apps over Representational State Transfer (REST).
- Signal hook for plugin index preparation — the plugin must observe catalog
  changes so it can pre-create indexes, partitions, or other structures needed
  by the typed-dimension surface that ADR-0010 introduces.
- FK atomicity — delete validation and rejection happen in the same transaction
  as the delete attempt, with no distributed protocol across gateway replicas.
- Alignment with the platform's hybrid-storage pattern documented in GTS.md §5
  (Generic Type System), where the consuming module locally persists its own
  `gts_type`-shaped table next to the rows it governs.

## Considered Options

- Catalog co-located with usage rows on the plugin — the metric catalog table
  lives in the plugin's backend database alongside `usage_records`; the gateway
  owns registration, Policy Decision Point (PDP) authorization, validation, and
  schema lifecycle through the Plugin SPI, and a real `ON DELETE RESTRICT` FK
  protects against orphan creation.
- Catalog gateway-local without FK (the current ADR-0007 status quo) — the
  catalog table lives in the gateway module's local database behind a SeaORM
  repo; delete is a local-DB operation only; referencing-records validation is
  not performed; the Plugin SPI carries no catalog methods.
- Pure types-registry-as-System-of-Record (SoR) — metric definitions are
  declared upstream in the `types-registry` module as GTS type schemas; the
  plugin holds no catalog state; UC reads its catalog projection from
  types-registry at boot and on cache refresh.

## Decision Outcome

Chosen option: "Catalog co-located with usage rows on the plugin", because it is
the only option that satisfies referential integrity atomically through a real
in-database FK, preserves durable persistence for runtime-registered metrics
(including those registered by external apps over REST), and keeps the catalog
mutation chokepoint inside UC so that plugin index preparation can hook into
the same lifecycle events that flow through gateway PDP and validation. The
five SPI methods that ADR-0007 removed return to the Plugin SPI surface and are
now load-bearing — the gateway no longer owns catalog state; specific method
names and signatures are deferred to the plugin-spi.md cascade phase. Owner and
physical location are deliberately split (RESEARCH-metadata.md §6.2): UC
remains the authoritative owner of the catalog API, PDP authorization,
validation, and schema lifecycle, while the rows physically live in the
plugin's database alongside `usage_records`. Publication of the catalog to
`types-registry` for cross-service discovery is OPTIONAL and explicitly
DEFERRED — it is not a dependency for this decision and not a dependency for
ADR-0010 (`cpt-cf-usage-collector-adr-gts-typed-metric-metadata`, the planned
follow-on ADR that builds GTS-typed metadata and declared dimensions on top of
this catalog substrate).

### Consequences

- The five SPI methods that ADR-0007 removed return to the Plugin SPI surface
  and are now load-bearing; their exact names, signatures, and error variants
  are deferred to the plugin-spi.md cascade phase of this rework.
- `cpt-cf-usage-collector-adr-pluggable-storage` (ADR-0002) re-expands to cover
  the metric catalog alongside `usage_records`; the appended Consequences
  paragraph on ADR-0002 makes the restored scope explicit without altering its
  original "Plugin SPI binding via Plugin Host + GTS Registry" decision or
  rationale.
- Referential delete is implemented by a real `ON DELETE RESTRICT` FK in the
  plugin's backend database; attempting to delete a metric with extant
  `usage_records` rows is rejected by the plugin engine inside the same
  transaction as the delete attempt, with no cross-replica coordination.
- `cpt-cf-usage-collector-adr-caller-supplied-attribution` (ADR-0003)'s
  caller-supplied attribution invariant is preserved unchanged; orphaned
  attribution from operator deletion no longer arises because referential
  delete rejects unsafe deletions atomically, but the orphan-friendly query
  semantics for any historical orphan rows remain in force.
- The gateway preserves an in-process Level-1 (L1) catalog cache for the hot
  validation path that ADR-0010 will introduce; the cache is read-through
  against the plugin as the System of Record (SoR) and is refreshed
  synchronously on register and delete because both flow through the gateway.
- Plugin authors carry more SPI surface area than under ADR-0007, and the
  gateway gives up a single in-memory source of truth for the catalog; this is
  the honest cost of regaining FK-enforced referential integrity for a
  billing-grade substrate.
- Migration of catalog ownership from the module-local SeaORM `MetricCatalogRepo`
  introduced by ADR-0007 back to the Plugin SPI is a downstream cascade
  concern; this ADR does not specify the migration mechanics, which are owned
  by the DECOMPOSITION and feature cascades.

### Confirmation

Compliance is confirmed through (a) a plugin contract conformance test
verifying the FK constraint — attempting to delete a metric with extant
`usage_records` rows MUST be rejected by the plugin and surfaced to the gateway
as a structured "metric referenced" error; (b) `cfs validate` PASS for the
usage-collector bundle covering this ADR and every artifact that
back-references it (ADR-0002, ADR-0007 superseded marker, downstream cascade
artifacts); (c) PR review by usage-collector maintainers confirming the
ownership-vs-location split and the absence of any cross-replica delete
coordination protocol.

## Pros and Cons of the Options

### Catalog co-located with usage rows on the plugin

The metric catalog table lives in the plugin's backend database alongside
`usage_records`; the gateway owns registration, PDP authorization, validation,
and schema lifecycle through the Plugin SPI; a real `ON DELETE RESTRICT` FK
between `usage_records` and the catalog enforces referential integrity natively.

- Good, because referential integrity is enforced atomically by the database
  engine — the FK rejects deletion of a referenced metric inside the same
  transaction as the delete attempt, with no distributed protocol across
  horizontally-scaled gateway replicas.
- Good, because runtime-registered metrics (including those registered by
  external apps over REST) persist with the same durability guarantees as
  `usage_records`; there is no in-memory-only catalog state that can be lost
  on restart.
- Good, because the plugin observes catalog changes on the same path as usage
  writes, so it can pre-create indexes or partitions for the typed-dimension
  surface that ADR-0010 introduces.
- Good, because the structural shape matches the platform's hybrid-storage
  pattern documented in GTS.md §5 — the consuming module owns a local
  `gts_type`-shaped table next to the rows it governs.
- Neutral, because catalog reads on the hot validation path go through a
  gateway-local L1 cache; the plugin remains the SoR but is not consulted on
  every ingest.
- Bad, because the Plugin SPI surface area grows by the five catalog methods
  ADR-0007 removed; plugin authors must re-implement them and the contract
  conformance suite expands accordingly.
- Bad, because the plugin's backend database must support FK with
  `ON DELETE RESTRICT` semantics; this is a real constraint on candidate
  backends and is acceptable for the named backends in PRD §2.2 but rules out
  any future backend that cannot honor referential constraints atomically.

### Catalog gateway-local without FK (the current ADR-0007 status quo)

The catalog table lives in the gateway module's local database behind a SeaORM
repo; delete is a local-DB operation only; the Plugin SPI carries no catalog
methods; referencing-records validation is not performed.

- Good, because the Plugin SPI surface is smaller and the gateway aligns with
  the platform's system-module convention (e.g. `account-management`).
- Good, because the catalog mutation path has no plugin round-trip and no
  cross-component coordination.
- Bad, because operators can orphan live billing data by deleting a metric
  referenced by `usage_records`, and no FK can span the gateway and plugin
  stores to prevent it.
- Bad, because the emit-then-delete race described in RESEARCH-metadata.md §6.3
  is structurally unavoidable: the gateway cannot make the "any rows for M?"
  check and the catalog delete atomic with a concurrent usage write on a
  different replica.
- Bad, because the plugin has no signal that a new metric exists, which forces
  the typed-dimension surface that ADR-0010 introduces to depend on the
  gateway pre-computing every plugin-side structure the dimensions need.

### Pure types-registry-as-System-of-Record (SoR)

Metric definitions are declared upstream in the `types-registry` module as GTS
type schemas; the plugin holds no catalog state; UC reads its catalog
projection from types-registry at boot and on cache refresh.

- Good, because metric definitions become discoverable across services through
  a single platform-wide registry, and downstream consumers can introspect
  metric metadata without a UC round-trip.
- Good, because schema lifecycle reuses the platform's existing
  `register_type_schemas` SDK trait surface verified in
  RESEARCH-metadata.md §5.1.
- Bad, because the types-registry module holds `GtsOps` in-memory only; on
  restart, runtime-registered entries are lost. In-process modules
  re-register at boot via the `#[gts_type_schema]` link-time inventory, but
  external apps registering metrics over REST would lose their definitions,
  which is unacceptable for a billing-grade substrate
  (RESEARCH-metadata.md §6.1).
- Bad, because UC loses the registration chokepoint and cannot reliably hook
  plugin index preparation on metric lifecycle events; types-registry has no
  event or notification surface today.
- Bad, because types-registry has no delete semantics today, so referential
  deletion would require either standing up new semantics in types-registry or
  re-implementing them in UC — re-introducing the very split this option
  intended to eliminate.

## More Information

- `RESEARCH-metadata.md` (§1 Trigger, §2 Problem Framing, §4 Design Space —
  Metric Deletion, §6 Catalog Ownership and Physical Location, §10 Decisions
  Summary) is the source of the option framings and the
  emit-then-delete race analysis reused above.
- ADR-0007 (`cpt-cf-usage-collector-adr-gateway-local-metric-catalog`) is the
  superseded predecessor; its frontmatter is flipped to `superseded` with
  `superseded_by: cpt-cf-usage-collector-adr-catalog-plugin-referential-integrity`
  in the same change set as this ADR.
- ADR-0010 (`cpt-cf-usage-collector-adr-gts-typed-metric-metadata`) is the
  follow-on ADR in this bundle and the downstream consumer of this catalog
  substrate; it builds GTS-typed metric metadata and declared dimensions on
  top of the plugin-resident catalog and the L1 gateway cache.
- GTS.md §5.2 (resource groups) documents the canonical hybrid-storage pattern
  that this decision mirrors structurally.
- Domain applicability:
  - **ARCH** — addressed (this IS the architectural decision: catalog co-located
    with `usage_records` on the active storage plugin; five catalog SPI methods
    restored on the Plugin SPI surface; in-database FK referential integrity).
  - **DATA** — addressed (FK `usage_records.metric_type_uuid →
metric_catalog(type_uuid) ON DELETE RESTRICT` enforced natively inside the
    plugin's backend transaction; full table DDL deferred to DESIGN per
    ARCH-ADR-NO-001).
  - **PERF** — addressed (gateway L1 read-through cache pattern over the plugin
    SoR with synchronous invalidation on register / delete; no per-request
    `types-registry` round-trip on the warm path).
  - **REL** — addressed via Confirmation (delete and referential check are
    atomic inside a single backend transaction; no cross-store coordination
    window; emit-then-delete race closed by the in-database FK).
  - **INT** — addressed (five catalog SPI methods restored on the Plugin SPI:
    `register_metric_type`, `read_metric_type`, `list_metric_types`,
    `delete_metric_type`, `read_metric_chain`; no new external module
    integration introduced).
  - **SEC** — Not applicable: catalog placement does not change the auth
    surface; PDP authorization (ADR-0001) and `SecurityContext` propagation
    remain unchanged; no secrets, no PII, no new attack surface introduced.
  - **OPS** — addressed (operator-facing register / delete flow gains a
    deterministic `MetricReferenced { type_uuid, sample_ref_count }` error on
    referencing-rows rejection; the ADR-0007 unconditional-delete stance is
    superseded; declared-catalog immutability semantics are unchanged).
  - **MAINT** — addressed (single source of truth for catalog rows lives with
    the active storage plugin alongside usage rows; the module-owned migration
    ratchet introduced by ADR-0007 is removed; plugin authors own one backend
    transaction shape for both tables).
  - **TEST** — Not applicable in the ADR body: test design belongs in
    `features/*` (Phase 9) per TEST-ADR-NO-001.
  - **COMPL** — Not applicable: no regulated-data implications; the decision
    affects internal substrate placement only.
  - **UX** — Not applicable: catalog placement is invisible to end users; the
    REST / SDK API surface naming and validation behavior remain owned by
    ADR-0010 and DESIGN.
  - **BIZ** — Not applicable in the ADR body: requirements belong in PRD per
    BIZ-ADR-NO-001.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses or relates to the following requirements,
decisions, or design elements:

- supersedes `cpt-cf-usage-collector-adr-gateway-local-metric-catalog` —
  reverses the gateway-local catalog placement and restores the catalog SPI
  surface.
- re-expands the scope of `cpt-cf-usage-collector-adr-pluggable-storage` to
  cover the metric catalog alongside `usage_records`; the original Plugin SPI
  binding decision and rationale are unchanged.
- preserves `cpt-cf-usage-collector-adr-caller-supplied-attribution` —
  caller-supplied attribution invariant is unchanged; referential delete
  removes the operator-driven orphan vector but the orphan-friendly query
  semantics for historical rows remain in force.
- `cpt-cf-usage-collector-fr-metric-registration` — metric registration crosses
  the Plugin SPI again; the gateway authorizes via PDP and validates before
  dispatching to the plugin.
- `cpt-cf-usage-collector-fr-metric-deletion` — metric deletion is enforced by
  a real `ON DELETE RESTRICT` FK in the plugin database; the gateway surfaces
  the engine-level rejection as a structured "metric referenced" error.
- `cpt-cf-usage-collector-fr-pluggable-storage` — the pluggable-storage seam
  applies to the metric catalog again alongside usage-record ingestion, query,
  and deactivation.
- to be consumed by the planned `cpt-cf-usage-collector-adr-gts-typed-metric-metadata`
  once Phase 2 of this rework lands (forward reference is informational only).
