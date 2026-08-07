# PRD — Infrastructure Inventory

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 Typed Item Model on GTS](#51-typed-item-model-on-gts)
  - [5.2 Connector Contract & Sources](#52-connector-contract--sources)
  - [5.3 Projection, Provenance & Freshness](#53-projection-provenance--freshness)
  - [5.4 Read Surface](#54-read-surface)
  - [5.5 Change Signals](#55-change-signals)
  - [5.6 Tenancy Scoping & Access Gating](#56-tenancy-scoping--access-gating)
  - [5.7 Audit](#57-audit)
  - [5.8 Source Lifecycle Operations (p2)](#58-source-lifecycle-operations-p2)
  - [5.9 Item Relationships (p2)](#59-item-relationships-p2)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Gear-Specific NFRs](#61-gear-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
  - [Register a Source and consume its estate](#register-a-source-and-consume-its-estate)
  - [Staleness-aware consumption](#staleness-aware-consumption)
  - [Decommission with clean retirement](#decommission-with-clean-retirement)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
  - [10.1 Launch Prerequisites (blocking)](#101-launch-prerequisites-blocking)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

- **Gear**: `infrastructure-inventory`
- **Status**: DRAFT (for review)
- **Owner**: TBD
- **Reference example gears**: [`settings-service`](../../settings-service) (PRD conventions), [`simple-resource-registry`](../../simple-resource-registry) (boundary partner), [`types-registry`](../../system/types-registry) (GTS)

## 1. Overview

### 1.1 Purpose

Infrastructure Inventory is the platform's **consolidated, typed, read-optimized record of external infrastructure**: the single place where every other gear finds out *what infrastructure exists* — clusters, hosts, nodes, and further item kinds over time — regardless of which vendor's estate the information came from. Inventory is **fed exclusively by Connectors** (vendor-specific discovery modules registered against a stable contract) and **consumed by any gear** through a read SDK. It records; it never operates: no lifecycle action on external infrastructure ever originates here.

### 1.2 Background / Problem Statement

The platform has no shared record of external infrastructure. Every gear that needs to know "which clusters exist", "which hosts belong to them", or "is this record still current" must integrate against vendor estates itself — its own polling, its own data shapes, its own staleness rules. Without a central inventory:

**Key problems solved**:

- **Fragmented knowledge**: each consumer builds a private, divergent view of the same estate; two gears can disagree about what exists.
- **Untyped records**: infrastructure facts travel as ad-hoc JSON with no schema authority, so consumers cannot validate, reference, or evolve them safely.
- **No provenance or freshness semantics**: a consumer cannot tell where a record came from, when it was last observed, or whether it should still be trusted.
- **Repeated vendor coupling**: every gear that touches infrastructure re-learns each vendor's API; adding a vendor means touching N consumers instead of adding one Connector.

**Boundary note — inventory vs. a managed entity.** An inventory item is a *projected observation* of something whose lifecycle is owned elsewhere (by the external estate and whatever operates it). The moment the platform starts *managing* that thing — creating, resizing, deleting — the managing gear owns it; Inventory only reflects it. This boundary keeps Inventory a pure information plane.

**Comparable systems (survey).** AWS Config / Resource Explorer and Azure Resource Graph prove the value of a fast, queryable, cross-source inventory decoupled from resource management — but both are single-vendor by construction. NetBox is the open-source DCIM/IPAM reference for a source-of-truth inventory, but it is operator-authored rather than connector-projected and carries no runtime type system. Kubernetes' API machinery shows what a uniform typed record surface enables for an ecosystem of controllers. Infrastructure Inventory combines the three lessons — queryable consolidation, source-of-truth discipline, and typed records — behind a vendor-neutral connector seam, with the platform's own type system (GTS) as the schema authority.

### 1.3 Goals (Business Outcomes)

1. **One question, one answer**: any gear can ask "what infrastructure exists in scope X" and get a consistent, typed, provenance-carrying answer.
2. **Vendors become plugins**: supporting a new estate kind means shipping one Connector against a stable contract — zero changes to consumers or to this gear's core.
3. **Trustworthy records**: every item carries provenance (which Source, observed when) and explicit freshness state, so consumers can make staleness-aware decisions instead of guessing.
4. **Ecosystem referenceability**: every item is addressable by a GTS instance identifier, so events, policies, and audit records can reference inventory items uniformly across the platform.

### 1.4 Glossary

| Term | Definition |
|------|------------|
| **Inventory Item** | One typed record of a piece of external infrastructure. Identified by a GTS **instance** id `<kind-type>~<item-instance-id>`; payload validated against the kind's GTS schema; carries scope, provenance, and freshness state. |
| **Item Kind** | The GTS type an item conforms to. Kinds live in a curated, registered catalog (see §5.1); the baseline kinds are **Cluster**, **Host**, and **Node**. The catalog is extensible without core changes. |
| **Cluster** (baseline kind) | A group of compute/storage capacity managed as one unit by an external control plane. |
| **Host** (baseline kind) | A physical or virtual machine that provides workload capacity, addressable by the external estate (e.g., a hypervisor or bare-metal machine). Where an estate presents one machine in both a capacity role and a Cluster-member role, the Connector projects **one** item and expresses membership via a `member-of` relationship — never two items for one machine (§5.1). |
| **Node** (baseline kind) | A machine fulfilling a role in a Cluster's own topology (control or worker) **where the estate distinguishes it from capacity Hosts**. Where an estate carries only one machine concept, the Connector uses **Host**. Host and Node are disjoint for any single machine within one Source (§5.1). |
| **Connector** | A module implementing the Connector contract (§5.2): it discovers infrastructure in one class of external estate and projects it into Inventory. Vendor-specific Connectors (e.g., for VMware vCenter, OpenStack-family platforms, bare-metal Redfish/IPMI estates, public-cloud inventories) ship as separate modules; none is part of this gear. |
| **Source** | A registered Connector *instance* bound to one concrete external estate/endpoint, with its own credentials (held by reference in the platform credential store), declared sync interval, and liveness state. One Connector kind MAY have N Sources. |
| **Projection** | The read-only import of discovered items from a Source into Inventory — full (complete estate enumeration) or incremental (changes since the last projection, where the Connector declares that capability). Projection MUST NOT modify the external estate. |
| **Provenance** | The per-item record of origin: `source_id` plus `observed_at` (the instant the Connector last observed the item). |
| **Freshness / Stale** | An item is **fresh** while its Source projects within its declared sync interval (plus grace); otherwise the item is marked **stale** — retained and readable, flagged as no longer confirmed. |
| **Retired Item** | An item whose backing infrastructure disappeared from the Source's projection (or whose Source was decommissioned). Retired items drop out of default reads but remain queryable for a bounded retention window. |
| **Consumer** | Any gear reading Inventory through the read SDK or REST surface. Consumers never write items. |
| **Scope** | The tenant context an item belongs to: platform (shared estate) or a specific tenant, per the platform tenancy model. Governs visibility, never typing. |

## 2. Actors

### 2.1 Human Actors

- **Platform administrator** — registers and governs Sources (credentials, sync policy, pause/resume/decommission), browses the inventory across scopes, investigates staleness.
- **Tenant administrator** — browses the inventory items visible in their own scope. No mutation surface.

### 2.2 System Actors

- **Connector** (per vendor) — authenticates to one external estate, discovers infrastructure, projects items. The only writer of item records.
- **Consumer gear** — reads items and subscribes to change signals; reacts in its own domain (placement, adapters, billing, visualization…).
- **Types Registry (GTS)** — owns the Item Kind schemas; Inventory consumes them for validation and never defines types outside the registry.
- **Tenant resolver** — scope hierarchy authority for item visibility.
- **AuthN/AuthZ resolver + RBAC** — request authentication and fail-closed authorization decisions.
- **Credential Store** — holds Source credentials by reference; plaintext never rests in Inventory state.
- **Events Broker** — carries item change signals and Source liveness events.
- **Audit** — receives an immutable record of every administrative mutation.

## 3. Operational Concept & Environment

Infrastructure Inventory is delivered as a **gear** with the standard SDK/implementation split: a public SDK crate exposing the typed clients (reader + connector contract), and the gear implementation owning the REST surface, persistence, and domain core. It registers its clients in the platform's client hub for in-process consumption and exposes an equivalent REST surface for remote callers. Capabilities: `db`, `rest`. It consumes the Types Registry (GTS schemas), the tenant resolver (scope), the AuthZ/RBAC stack (access decisions), the Credential Store (Source credentials by reference), the Events Broker (signals), and the platform audit subsystem.

The write path belongs to Connectors alone; the read path is the product. The design center is **many reads, few writes**: consumers poll or react to signals at high frequency, while projections arrive on Source sync intervals.

## 4. Scope

### 4.1 In Scope

| **Feature** | **Priority** | **Notes** |
|-------------|--------------|-----------|
| **Typed Item Model on GTS** | `p1` | Items as GTS instances (`<kind-type>~<item-instance-id>`); curated, registered Item Kind catalog (baseline: Cluster, Host, Node); payload validation against the kind's schema; kinds extensible without core changes. |
| **Connector Contract & Sources** | `p1` | Stable contract for vendor Connectors: Source registration (endpoint + credential reference + sync interval), full projection, capability-declared incremental projection, liveness heartbeat. Projection is strictly read-only toward the estate. |
| **Projection, Provenance & Freshness** | `p1` | Upsert/retire semantics per projection; per-item `source_id` + `observed_at`; stale marking on missed sync (interval + grace); retire on disappearance with bounded retention. |
| **Read Surface (SDK + REST)** | `p1` | Get by id; list/filter by kind, scope, Source, freshness; bulk reads with per-item outcomes; consistent pagination. The consolidated space other gears read from. |
| **Change Signals** | `p1` | `inventory.item.registered / updated / retired / stale` and `inventory.source.*` events — **identifiers only** (GTS instance ids + scope), consumers re-read via the SDK; no payloads or secrets in the stream. |
| **Tenancy Scoping & Access Gating** | `p1` | Every item carries scope; reads filtered by tenant visibility (tenant resolver) and gated by AuthZ fail-closed; no cross-scope leakage through any read path, list, or signal. |
| **Audit of Administrative Mutations** | `p1` | Source register/update/pause/resume/decommission audited with actor, before/after, timestamp. |
| **Source Lifecycle Operations** | `p2` | Pause/resume projection; credential rotation via credential-store reference swap; decommission with explicit disposition of the Source's items (retire-all). |
| **Item Relationships** | `p2` | Typed edges between items (`member-of`, `runs-on`), projected by Connectors, readable through the same surface; no graph mutation API. |
| **Derived Aggregates** | `p3` | Cheap read-side counts (items per kind / per Source / per scope, freshness distribution) for consumer dashboards. |

*Sorting order: priority (`p1` → `p2` → `p3`).*

### 4.2 Out of Scope

- **Lifecycle operations on external infrastructure** — create/resize/start/stop/delete of anything in an external estate. Inventory observes; managing gears act.
- **Vendor Connectors themselves** — each ships as its own module against the §5.2 contract (candidates: VMware vCenter, OpenStack-family platforms, bare-metal Redfish/IPMI estates, public-cloud inventories). This PRD defines the contract, not any Connector.
- **Monitoring, metrics, and health of the inventoried infrastructure** — utilization, alerts, and telemetry are monitoring-domain concerns. Inventory's only "health" facts are Source liveness and item freshness.
- **The platform's own runtime node registry** — the execution environment of platform modules (CPU/memory/OS of the hosts running the platform itself) is a separate concern with a separate owner; Inventory covers *managed external* infrastructure only.
- **Generic application-object storage** — arbitrary typed CRUD storage for gear-owned objects is owned by [`simple-resource-registry`](../../simple-resource-registry). The boundary: Inventory items are **connector-projected observations with provenance and freshness semantics**; records that a gear authors and owns end-to-end belong in the resource registry, not here.
- **Capacity planning, placement, and scheduling** — consumers' domains, computed on top of inventory reads.
- **Inventory UI** — administrative and end-user screens are owned by consuming applications; this gear ships data surfaces only.
- **GTS type authoring and the Types Registry itself** — this PRD consumes GTS; the registry defines it.
- **API schemas, data models, and error taxonomies** — owned by the downstream DESIGN document; this PRD defines WHAT/WHY only.

## 5. Functional Requirements

### 5.1 Typed Item Model on GTS

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-inventory-fr-item-model-gts`

Every Inventory Item MUST be identified by a GTS **instance** id of the form `<kind-type>~<item-instance-id>`, where the kind type is a member of the curated Item Kind catalog registered in the platform Types Registry, and the item instance itself is an **unregistered instance** stored in the Inventory database (registered types, unregistered instances — the platform's established pattern for high-cardinality instance populations). Item payloads MUST validate against the kind's GTS schema (JSON Schema 2020-12 plus GTS traits) at projection time; a payload that fails validation MUST be rejected with a per-item outcome and MUST NOT corrupt the batch.

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-inventory-fr-kind-catalog`

The gear MUST ship the baseline Item Kind catalog — **Cluster**, **Host**, **Node** — registered in the Types Registry under the gear's curated namespace at startup, and MUST support additional kinds registered through the same mechanism without changes to the gear core. Kind schemas are versioned by the Types Registry's own versioning rules; an item always records the schema version it validated against.

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-inventory-fr-item-identity-stability`

An item's instance identifier MUST be stable across projections of the same underlying infrastructure and MUST be collision-safe across Sources: the Connector derives it deterministically from the registered `source_id` **plus** estate-native identity, so two Sources exposing equal native identifiers never collide. One machine observed by one Source MUST yield exactly **one** item (Host/Node disjointness — see Glossary); the same machine observed by two Sources yields two items, one per observation (cross-Source de-duplication is out of scope for v1 and recorded as a candidate follow-up). Consumers, events, policies, and audit records reference items durably by this identifier. The identifier **shape** is fixed here; the curated namespace string for kind types remains an Open Question for the Types Registry owners.

### 5.2 Connector Contract & Sources

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-inventory-fr-source-registration`

A platform administrator MUST be able to register a Source: the Connector kind, the external endpoint, a credential **reference** (resolved via the platform Credential Store — plaintext never stored in Inventory state), a declared sync interval, and the scope its items belong to. Registration MUST validate connectivity through the Connector before the Source becomes active.

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-inventory-fr-connector-contract`

The gear MUST expose a Connector contract with exactly three obligations: **full projection** (enumerate all items currently present in the estate), **incremental projection** (changes since the last projection — honored only where the Connector declares the capability per item kind; otherwise the gear falls back to full projection on every interval), and **liveness heartbeat**. The contract MUST be vendor-neutral: no estate-specific concept may leak into it.

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-inventory-fr-projection-read-only`

Projection MUST NOT modify, create, or delete anything in the external estate. The Connector contract offers no write channel toward the estate.

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-inventory-fr-projection-authorization`

Every `project_full`, `project_incremental`, and `heartbeat` call MUST be authorized against the registered Source: the authenticated caller identity MUST match the Source's registered Connector kind and the Source's scope, and cross-Source submissions MUST be rejected. The platform's trusted-caller posture for in-process modules does not waive this per-Source binding.

### 5.3 Projection, Provenance & Freshness

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-inventory-fr-projection-semantics`

Each projection MUST upsert items by instance id (new → registered; changed → updated). Retirement on absence MUST follow only a **complete** full projection: the contract carries an explicit completed-enumeration marker, and a partial or aborted enumeration MUST NOT retire anything. An item that fails validation is rejected per-item **and its previous valid record is retained** (never dropped by a bad batch). Incremental projection MUST be continuity-safe: where the gear cannot establish that the incremental chain since the last accepted projection is unbroken, it MUST fall back to a full projection on the next cycle (cursor/checkpoint mechanics are owned by DESIGN). Every item MUST carry provenance: the `source_id` it came from and `observed_at`, the instant of the Connector's last observation. Per-item outcomes MUST be reported for every projection batch — a bad item never fails the batch wholesale.

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-inventory-fr-freshness-staleness`

An item's `observed_at` and freshness MUST be updated **only by a successful projection that includes the item** — the heartbeat updates Source liveness alone and never item freshness. An item whose Source misses its declared sync interval (plus a configurable grace) MUST be marked **stale** — retained, readable, and flagged — regardless of heartbeat state (a live-but-not-projecting Source still goes stale; a projecting Source with failed heartbeats does not). A paused Source stops projecting, so its items follow the same staleness clock. Consumers MUST be able to filter by freshness; a stale item returns to fresh on the next successful projection that includes it. Staleness MUST be derived from Source behavior, never from consumer reads.

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-inventory-fr-retire-retention`

Retired items MUST drop out of default reads, remain queryable via an explicit filter for a bounded, configurable retention window, and be purged after it. Retirement MUST emit a change signal.

### 5.4 Read Surface

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-inventory-fr-read-surface`

The gear MUST expose a read surface — SDK trait for in-process consumers plus an equivalent REST surface — providing: get by instance id; list/filter by kind, scope, Source, and freshness; and bulk reads with **per-item outcomes** (a mixed batch never fails wholesale). Lists MUST paginate consistently. The read surface is the *only* consumer-facing surface: consumers cannot write items.

- [ ] `p3` - **ID**: `cpt-cf-infrastructure-inventory-fr-derived-aggregates`

The read surface SHOULD offer cheap aggregate counts (per kind, per Source, per scope, freshness distribution) computed on the read side, for consumer dashboards.

### 5.5 Change Signals

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-inventory-fr-change-signals`

The gear MUST publish change signals through the platform Events Broker on item registration, update, retirement, and staleness transitions, and on Source lifecycle transitions. Signals MUST carry **identifiers only** and never item payloads, credentials, or secrets: item signals (`inventory.item.*`) carry the GTS instance id and scope; Source signals (`inventory.source.*`) carry the `source_id`, its scope, and the new lifecycle state. A consumer needing the data re-reads through §5.4. A signal MUST be published only **after** the corresponding state change is durably committed and readable (no signal may precede its record), and a committed transition MUST NOT go silently unsignaled — publication is retried until the broker accepts it (durable-publication mechanics are owned by DESIGN). Delivery is at-least-once; consumers MUST treat re-reads as the source of truth (convergent, not event-sourced). Signals MUST be delivered only to subscribers authorized for the signal's scope — the enforcement point (tenant-aware broker routing or per-subscriber filtering before delivery) is owned by DESIGN and MUST fail closed when the authorization decision is unavailable, consistent with §5.6.

### 5.6 Tenancy Scoping & Access Gating

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-inventory-fr-tenancy-scoping`

Every item MUST carry its scope (platform or tenant, per the platform tenancy model). All reads — get, list, bulk, aggregates — and all signals MUST be filtered to the caller's visible scope as resolved by the tenant resolver. No item outside the caller's scope may be revealed through any path, including error messages and counts.

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-inventory-fr-authz-fail-closed`

Every request MUST be authenticated and authorized through the platform AuthN/AuthZ stack. Where the authorization decision point is unavailable, reads and administrative operations MUST fail closed.

### 5.7 Audit

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-inventory-fr-audit-mutations`

Every administrative mutation — Source register/update/pause/resume/decommission, retention configuration — MUST produce an immutable audit record (actor, action, before/after, timestamp) in the platform audit subsystem. Projections themselves are not per-item audited (volume); each projection run MUST leave one summarized audit record (source, counts, outcome).

### 5.8 Source Lifecycle Operations (`p2`)

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-inventory-fr-source-lifecycle`

An administrator MUST be able to: **pause** a Source (projection stops; its items begin the staleness countdown), **resume** it (next projection is full), **rotate credentials** (swap the credential-store reference after a successful validation — no projection downtime), and **decommission** it (explicit confirmation; all its items retire per §5.3).

### 5.9 Item Relationships (`p2`)

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-inventory-fr-item-relationships`

Connectors MAY project typed relationships between items (`member-of` — a Node to its Cluster; `runs-on` — a workload-bearing item to its Host), and the read surface MUST return them alongside items and support traversal one hop at a time. Relationship kinds live in the same curated GTS catalog. There is no consumer-facing graph mutation API.

## 6. Non-Functional Requirements

### 6.1 Gear-Specific NFRs

#### Performance: Read Path

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-inventory-nfr-read-performance`

Point reads (get by id) MUST complete within **20 ms at p95** at the SDK boundary; filtered list pages within **100 ms at p95** — sustained at ≥ 500 reads/second against the full declared dataset (§Scale). Exact validated thresholds are owned by DESIGN within or below these ceilings.

#### Scale

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-inventory-nfr-scale`

The gear MUST support at least **100 Sources**, **50,000 items per Source**, and **1,000,000 items per platform instance**, with projection batches up to a full Source enumeration. None of these bounds may degrade the read-path thresholds.

#### Reliability: Projection Isolation

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-inventory-nfr-projection-isolation`

A failing Source (unreachable estate, bad credential, malformed batch) MUST NOT affect the freshness, availability, or correctness of items from other Sources, and MUST NOT degrade the read path. Failure surfaces as Source liveness state plus staleness of its own items — nothing else.

#### Security Baseline

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-inventory-nfr-security-baseline`

Source credentials exist only as credential-store references; no plaintext in Inventory state, logs, API responses, or events. Authorization is fail-closed (§5.6). Change signals carry identifiers only (§5.5).

#### Availability

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-inventory-nfr-availability`

The read path MUST maintain **99.9 % monthly availability**, independent of Connector or estate availability: with every Source down, reads keep serving the last projected state (staleness-flagged).

### 6.2 NFR Exclusions

- **Safety**: not applicable — a pure information system with no physical or industrial interaction.
- **Device/Platform UX**: not applicable — no UI is shipped by this gear.

## 7. Public Library Interfaces

### 7.1 Public API Surface

- **`InventoryReaderClient`** (SDK trait, in-process via the client hub; equivalent REST): `get(id)`, `list(filter: kind|scope|source|freshness, page)`, `get_bulk(ids[]) → per-item outcomes`, `relationships(id)` (p2), `aggregates(filter)` (p3).
- **`InventoryConnectorClient`** (SDK trait for Connectors): `register_source / update_source` (admin-gated), `project_full(source, items[]) → per-item outcomes`, `project_incremental(source, changes[]) → per-item outcomes` (capability-gated), `heartbeat(source)`.
- **REST**: `/v1/sources` (admin CRUD + lifecycle actions), `/v1/items` (read-only browse with filters), `/v1/items/{id}` (+ `/relationships`, p2). Exact schemas, error taxonomy, and pagination mechanics are owned by DESIGN.

### 7.2 External Integration Contracts

Types Registry (kind catalog registration + schema retrieval), tenant resolver (scope resolution), AuthN/AuthZ resolvers (request gating), Credential Store (Source credential references), Events Broker (change signals), Audit (administrative records).

## 8. Use Cases

### Register a Source and consume its estate

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-inventory-usecase-register-consume`

**Actor**: platform administrator; consumer gear.

**Preconditions**: a vendor Connector module for the estate kind is deployed; credentials exist in the Credential Store.

**Main flow**: admin registers a Source (endpoint, credential reference, sync interval, scope) → Connector validates connectivity → Source activates → first full projection lands (items registered, typed, provenance-stamped) → change signals fan out → any consumer gear reads the new items through the SDK in its own scope.

**Postconditions**: the estate is queryable platform-wide; consumers reference items by GTS instance id.

### Staleness-aware consumption

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-inventory-usecase-staleness`

**Actor**: consumer gear.

**Main flow**: a Source's estate becomes unreachable → heartbeats stop → after interval + grace the Source's items are marked stale and `inventory.item.stale` signals fan out → the consumer filters stale items out of a placement decision, or renders them dimmed → the estate returns; the next projection restores freshness.

**Postconditions**: consumers never acted on silently-outdated records.

### Decommission with clean retirement

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-inventory-usecase-decommission`

**Actor**: platform administrator.

**Main flow**: admin decommissions a Source → explicit confirmation lists the item count to retire → items retire (signals fan out; default reads exclude them; retention window starts) → audit records the operation → after retention, records purge.

## 9. Acceptance Criteria

1. **Typed registration**: given a registered Source, when a projection lands, every accepted item is a GTS instance of a catalog kind, schema-validated, carrying `source_id`, `observed_at`, and scope; invalid items are rejected per-item without failing the batch.
2. **Identity stability**: given repeated projections of an unchanged estate, item instance ids are identical across runs; given two Sources exposing equal estate-native identifiers, the resulting items do not collide; given one machine in both a capacity and a Cluster-member role in one Source, exactly one item exists, with membership expressed as a `member-of` relationship.
3. **Read surface**: given items across kinds, scopes, and Sources, consumers can get-by-id, filter lists by kind/scope/source/freshness, and bulk-read with per-item outcomes, within the §6.1 thresholds.
4. **Identifiers-only signals**: given any item or Source transition, a signal is published carrying only identifiers and scope; no payload or secret appears in the stream.
5. **Staleness**: given a Source that misses its projection interval + grace, its items — and only its items — are marked stale and signal it, regardless of heartbeat state; heartbeats alone never refresh `observed_at`; the next successful projection that includes an item restores its freshness.
6. **Retirement**: given a **complete** full projection missing a previously present item, the item retires, leaves default reads, remains under the explicit filter through the retention window, and purges after it; given a partial or aborted enumeration, nothing retires; given an item rejected by validation, its previous valid record is retained.
7. **Scope isolation**: given items in two tenant scopes, no read, list, bulk, aggregate, signal, or error surface reveals an item — or its existence — outside the caller's visible scope; authorization failures deny fail-closed.
8. **Projection isolation**: given one failing Source among many, all other Sources' items stay fresh and the read path stays within thresholds.
9. **Read-only estates**: given any projection or Source operation, no request that would mutate the external estate is issued through the Connector contract.
10. **Audited administration**: given any Source lifecycle mutation, an immutable audit record exists with actor and before/after; each projection run leaves one summarized record.

## 10. Dependencies

| Dependency | What Inventory consumes | Failure posture |
|---|---|---|
| Types Registry (GTS) | Kind catalog registration, schema retrieval for validation | Fail-closed for new kind registration and projection validation; reads of already-validated items unaffected |
| Tenant resolver | Scope resolution for visibility filtering | Reads fail closed on unresolvable scope |
| AuthN/AuthZ resolvers | Request gating | Fail-closed |
| Credential Store | Source credential references | Source activation/rotation blocked; existing projections stop at next credential use; read path unaffected |
| Events Broker | Change signal fan-out | Signals buffered/retried per platform broker semantics; read path is the source of truth regardless |
| Audit | Administrative records | Administrative mutations fail closed if the audit write fails |

### 10.1 Launch Prerequisites (blocking)

1. Types Registry supports the curated kind-catalog registration flow this gear uses at startup.
2. A reference Connector exists and **passes the contract test suite** end-to-end: projection semantics (upsert / complete-enumeration retirement / per-item outcomes with retained-last-valid records), staleness and retirement transitions, read-only estate behavior, projection authorization, and identifiers-only signals — i.e., the reference Connector demonstrably satisfies the §9 acceptance criteria before release. The first vendor Connector is a separate deliverable.

## 11. Assumptions

- Connectors are trusted, in-platform components behind the deployment trust boundary (same posture as other SDK-consuming modules); a verified per-service identity model, when the platform provides one, will be adopted for the projection surface.
- The platform tenancy model provides a resolvable scope hierarchy; Inventory does not define tenancy semantics.
- Estate-native identifiers exist that allow Connectors to derive stable instance ids; where an estate cannot provide them, that Connector documents its identity-derivation strategy.

## 12. Risks

| Risk | Mitigation |
|---|---|
| **Scope creep toward a general data store** | The simple-resource-registry boundary (§4.2) is normative: connector-projected observations only. Any "let a gear write its own items" request is redirected there. |
| **Connector quality variance** breaks consumer trust | Per-item validation outcomes, projection isolation (§6.1), and the capability-declared incremental contract keep a weak Connector's blast radius inside its own Source. |
| **Kind catalog fragmentation** (every vendor wants bespoke kinds) | Kinds are curated: additions go through the registered catalog with review; vendor-specific detail belongs in the payload schema of an existing kind before a new kind is minted. |
| **Staleness semantics misread as monitoring** | Freshness is projection bookkeeping, not health. The Out-of-Scope boundary (§4.2) and the signal vocabulary keep "stale" distinct from any monitoring status. |

## 13. Open Questions

| Question | Notes |
|---|---|
| Curated GTS namespace for the kind catalog | Proposed: the gear registers its kinds under the platform toolkit namespace (mirroring the settings-service value-type catalog pattern); exact namespace string to be confirmed with the Types Registry owners. |
| Baseline kind granularity | Cluster / Host / Node is the v1 set. Whether storage estates warrant a fourth baseline kind (e.g., StoragePool) in v1 or arrive with the first storage Connector is open. |
| First vendor Connector timing | The reference Connector (with its contract test suite) satisfies launch per §10.1; whether the first *vendor* Connector ships in the same release window is a roadmap decision, not a contract question. |
| Relationship traversal depth | §5.9 fixes one-hop traversal; whether consumers need bounded multi-hop queries (and whether that pushes toward a graph read model) is deferred to DESIGN with real consumer input. |

## 14. Traceability

All requirements in this document carry `cpt-cf-infrastructure-inventory-*` identifiers for downstream DESIGN, implementation, and test traceability, following the platform's spec-driven conventions.
