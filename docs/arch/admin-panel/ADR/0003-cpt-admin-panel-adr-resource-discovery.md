---
status: accepted
date: 2026-06-25
decision-makers: gears-rust admin-panel working group
---

# Discover admin resources OpenAPI-first, with a hardcoded v0 registry


> **Revision (2026-06-30) — discovery direction promoted after review.**
> Reviewer feedback on [#4145](https://github.com/constructorfabric/gears-rust/pull/4145#discussion_r3495062634) (see [ADR-0001 revision](0001-cpt-admin-panel-adr-placement-and-delivery.md)) requires the SPA to carry **zero per-project TypeScript**, which makes the hardcoded in-app registry a blocker rather than an acceptable v0 coupling. Two parts of this ADR that were "Still deferred" are promoted to the **target/v1**:
> 1. **Runtime parsing of `/openapi.json`** to derive fields/types/`required`/`readOnly`, CRUD verb mapping, and read-only detection — shrinking the curated registry by ~70%.
> 2. The **gear-contributed metadata mechanism** (`cpt-admin-panel-fr-gear-contributed-metadata`) for what OpenAPI cannot express (custom actions, safety levels, tenant-scope strategy, layout/labels, irregular list paths). This effectively moves the chosen approach from Option A toward **Option C**, but realized incrementally rather than as an upfront cross-gear blocker.
>
> **Open (pending reviewer):** the metadata transport — a config file, `x-cf-admin-*` OpenAPI vendor extensions emitted per-gear via `OperationBuilder` (leaning; keeps metadata next to the API), or a dedicated descriptor endpoint. The descriptor shape defined for the v0 registry is already forward-compatible with all three. No-regret work (runtime OpenAPI discovery + registry shrink) proceeds now regardless of the chosen transport.

> **Revision (2026-07-02) — transport resolved by splitting metadata by concern.**
> Re-examining what the aggregated `/cf/openapi.json` actually expresses closed the "open" question above without a new backend mechanism. Discovery is split by the *nature* of each fact:
> 1. **API-intrinsic facts → derived from OpenAPI.** Field names/types/`required`/`readOnly` (component schemas), CRUD verb mapping, **custom actions** (e.g. `POST …/suspend`, `…/unsuspend` are first-class operations), and **tenant-scope** (the `{tenant_id}` path parameter marks a tenant-scoped route) are all present in the spec and read at runtime.
> 2. **Presentation-only facts → a small panel-side config.** List columns, labels, grouping/ordering, confirm/safety level, action button labels, and irregular-list hints (e.g. tenants list via `/children`, conversion state via `PATCH {status}`) are *UX concerns, not API contract*. Encoding them in OpenAPI vendor extensions would leak presentation into the API layer, so they live in a panel-side config instead — the equivalent of Django's `admin.py` registration, **not** copied panel code.
>
> **Consequence:** the SPA becomes fully generic — **no per-project TypeScript and no framework changes.** The hardcoded in-app `registry.ts` is dropped: its API-intrinsic content comes from the spec, its presentation content moves to config. The `x-cf-admin-*` vendor-extension idea (Option-B transport) is **not required** and is retained only as a later optimization should a genuinely API-intrinsic fact appear that the spec cannot yet carry. This supersedes the "leaning toward vendor extensions" note above. Distribution (pre-built artifact + thin Rust loader, later extracted to a dedicated `constructorfabric/` repo) is tracked separately in [ADR-0001](0001-cpt-admin-panel-adr-placement-and-delivery.md).

> **Revision (2026-07-13) — implemented; the sections below are the original decision record and are superseded by this note.**
> The concern-split above is now the shipped design. In the codebase: `apps/admin-panel/src/resources/admin.config.json` holds declarative per-resource registration (list columns, labels, `schema` name, `basePath`, verb suppression, custom actions, option sources); `openapiOps.ts` derives CRUD routes and custom actions structurally from `/cf/openapi.json`; `openapi.ts` derives fields from component schemas. There is **no hardcoded in-app resource registry** — the "New v0 resources are added by editing the in-app registry" and "gear-contributed descriptor mechanism ... migration from the hardcoded registry" consequences below are obsolete. Adding an admin object is a JSON edit; a second project ships the same pre-built panel and edits only its own config. The gear-contributed-descriptor mechanism remains an optional future enhancement, not a v0 dependency.

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option A: OpenAPI-first + hardcoded v0 registry](#option-a-openapi-first--hardcoded-v0-registry)
  - [Option B: Pure OpenAPI introspection only](#option-b-pure-openapi-introspection-only)
  - [Option C: Build gear-contributed descriptors now](#option-c-build-gear-contributed-descriptors-now)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-admin-panel-adr-resource-discovery`

## Context and Problem Statement

The admin panel must know which resources exist, their fields, operations, filters, ordering, and custom actions, and must render generated screens for them. The PRD requires OpenAPI-first generation with manual overrides, gear-contributed admin metadata, and additive registration so new objects appear without editing the core admin app. The platform already serves an aggregated OpenAPI document (with `x-odata-*` extensions) through the API Gateway, but there is no mechanism today for a gear to ship admin resource descriptors. How should the panel discover and describe admin resources for v0, without blocking on a new backend mechanism?

## Decision Drivers

- **Reuse existing surface** — the aggregated OpenAPI document already describes operations, schemas, filters, and ordering.
- **Additive extensibility** — long-term, gears should contribute their own admin descriptors without core edits.
- **Avoid blocking v0** — the gear-contributed descriptor mechanism is new backend work; v0 must not wait on it.
- **Generated-but-overridable** — default screens from metadata, with per-resource overrides for custom workflows and actions.
- **Custom actions & partial CRUD** — discovery must capture non-CRUD actions and resources missing some CRUD operations.
- **Single source of truth drift** — minimize divergence between the API and what the panel shows.

## Considered Options

- **Option A**: OpenAPI-first discovery plus a small, hardcoded resource registry (descriptors + overrides) maintained in the admin app for v0; design the gear-contributed descriptor mechanism as later work.
- **Option B**: Pure OpenAPI introspection only — derive everything from `/openapi.json` with no curated descriptors.
- **Option C**: Build the gear-contributed admin-metadata mechanism now and require every gear to ship descriptors before v0.

## Decision Outcome

Chosen option: **Option A — OpenAPI-first discovery plus a hardcoded v0 registry**, because it reuses the existing OpenAPI surface for fields, operations, filters, and pagination, while a small curated registry supplies the things OpenAPI cannot express well (resource grouping, tenant-scope strategy, safety levels, custom-action wiring, layout/label overrides). It delivers v0 without waiting on a new backend mechanism, and the registry is shaped to be replaced later by gear-contributed descriptors (recorded as deferred FR `cpt-admin-panel-fr-gear-contributed-metadata`).

Direction:

- The data provider reads `/openapi.json` (gateway-aggregated) to derive default list/detail/create/update fields, operations, OData filter/order capabilities (`x-odata-*`), and cursor pagination.
- A curated, in-app **resource registry** declares the v0 resources (tenants, tenant metadata, conversions, resource groups, types/GTS, gateway routes, gear status) with: resource key, owning gear, source operation IDs, tenant-scope strategy, safety level, custom actions, required capabilities, and per-resource layout/label/widget overrides.
- Where OpenAPI and the registry overlap (fields, operations), OpenAPI provides defaults and the registry overrides.
- The registry's descriptor shape is designed to match a future gear-contributed descriptor schema, so the v0 hardcoded registry can later be populated from gear-shipped metadata without reworking the frontend.

#### Implementation status (v0)

- **Done**: the curated descriptor registry (`apps/admin-panel/src/resources`) and a fully descriptor-driven data provider and List/Show/Create/Edit screens. Descriptors carry paths, fields with per-view visibility and create-time immutability, capabilities, safety level, tenant scope, and custom actions; new resources are added by appending a descriptor. Covers tenants (full CRUD + suspend/unsuspend/soft-delete), resource-groups (CRUD), conversions (read + approve/reject/cancel), and read-only types/gears.
- **Still deferred**: runtime parsing of `/openapi.json` to *derive* field/filter/order defaults (the v0 fields are hand-curated from the served OpenAPI rather than read at runtime), and the gear-contributed descriptor mechanism (`cpt-admin-panel-fr-gear-contributed-metadata`). The descriptor shape is forward-compatible with both.

### Consequences

- A descriptor schema (resource + field + action shapes) must be defined in the admin app and documented; it is the contract the future gear-contributed mechanism will emit.
- The data provider must map OpenAPI operations and `x-odata-*` extensions onto the descriptor's list/read/create/update/delete/custom-action operations and onto filter/order/pagination behavior.
- New v0 resources are added by editing the in-app registry; this is an accepted, temporary coupling until gear-contributed descriptors land.
- Discovery must tolerate resources missing `getOne`/`update`/`delete` and expose the gaps rather than render broken controls.
- A later ADR/feature will specify the gear-contributed descriptor mechanism (e.g. inventory-based registration alongside route registration) and the migration from the hardcoded registry.

### Confirmation

Confirmed by design review of DESIGN.md (descriptor schema and OpenAPI mapping), by the panel rendering generated screens for the v0 resources from OpenAPI + registry, and by e2e tests covering list/detail/create/edit, filtering/ordering, pagination, and custom actions.

## Pros and Cons of the Options

### Option A: OpenAPI-first + hardcoded v0 registry

Derive defaults from the spec; curate a small in-app registry for what the spec cannot express; defer gear-contributed descriptors.

- Good, because it reuses the existing aggregated OpenAPI surface and `x-odata-*` extensions.
- Good, because the curated registry expresses grouping, tenant-scope strategy, safety levels, custom actions, and overrides that OpenAPI cannot.
- Good, because it ships v0 without blocking on a new backend descriptor mechanism.
- Good, because the registry shape is forward-compatible with gear-contributed descriptors.
- Neutral, because the registry is a temporary in-app coupling.
- Bad, because adding a v0 resource means editing the admin app until the gear-contributed mechanism exists.

### Option B: Pure OpenAPI introspection only

Generate the entire panel from `/openapi.json` with no curated descriptors.

- Good, because zero curation and fully automatic.
- Bad, because OpenAPI cannot express resource grouping, tenant-scope strategy, safety levels, custom-action semantics, or layout overrides.
- Bad, because custom actions (suspend, approve, convert) and confirmation/destructive semantics would be guessed, risking unsafe UI.
- Bad, because navigation and capability gating would lack the metadata the PRD requires.

### Option C: Build gear-contributed descriptors now

Require every gear to ship admin descriptors before v0 can render them.

- Good, because it directly realizes the long-term extensibility goal.
- Bad, because it is new backend work across many gears and blocks v0 delivery.
- Bad, because it couples the first UI milestone to a cross-cutting backend mechanism whose design is not yet settled.

## More Information

- Issue: constructorfabric/gears-rust#4144
- The API Gateway serves the aggregated OpenAPI document with vendor extensions `x-odata-filter`, `x-odata-orderby`, `x-rate-limit-rps`.
- Cursor pagination is provided via the platform's `Page<T>`/`PageInfo` envelope.
- Deferred mechanism is tracked by `cpt-admin-panel-fr-gear-contributed-metadata` (priority p2).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

- `cpt-admin-panel-fr-openapi-discovery` — defines OpenAPI as the primary discovery source.
- `cpt-admin-panel-fr-resource-descriptor` — the curated registry implements the descriptor model for v0.
- `cpt-admin-panel-fr-gear-contributed-metadata` — deferred; the registry shape is forward-compatible with it.
- `cpt-admin-panel-fr-generated-screens` — generated screens from OpenAPI + registry with overrides.
- `cpt-admin-panel-fr-partial-crud` — discovery tolerates missing CRUD operations.
- `cpt-admin-panel-fr-pagination-filtering` — `x-odata-*` and cursor pagination mapped from the spec.
