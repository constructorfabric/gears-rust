# UPSTREAM_REQS — Location Manager (Multi-Region)

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Requesting Sources](#12-requesting-sources)
  - [1.3 Relationship to Existing Gears](#13-relationship-to-existing-gears)
- [2. Requirements](#2-requirements)
  - [2.1 Multi-Region Control Plane](#21-multi-region-control-plane)
- [3. Priorities](#3-priorities)
- [4. Traceability](#4-traceability)
  - [Provenance](#provenance)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

A hub-spoke location-manager gear is needed to manage geographically distributed cluster estates from a single control plane: a topology registry (geography / region / availability-zone / cluster), agent-based cluster registration and lifecycle management, disconnected-autonomous operation over unreliable WAN links, constraint-based placement resolution, and WAN-resilient spooling of usage and observability records.

These requirements are contributed from a production multi-region infrastructure control plane whose hub-spoke core proved to be provider-agnostic: nothing in agent registration, declarative sync, autonomous mode, agent lifecycle, or constraint-set placement depends on any particular product's semantics. Capturing that core as a gear lets any Constructor Fabric-based platform manage a distributed estate, while each adopting platform binds its own semantics — resource-graph node types, policy admission, tenant-tier permissions, billing contracts, storage geo-replication — on top. Every requirement below is stated self-contained; no external document is needed to implement it.

This intake seeds the **Location Manager (Multi-Region)** gear tracked on the platform roadmap as [#4324](https://github.com/constructorfabric/gears-rust/issues/4324); the tentative scope there — "coordinates placements across regions", with location descriptors and region policies as extension points — is what §2 specifies.

### 1.2 Requesting Sources

| Source | Why it needs this gear |
|--------|-------------------------|
| A production multi-region infrastructure control plane | Needs a hub-spoke control plane over a distributed cluster estate: geographic topology, agent-based cluster registration and lifecycle, disconnected-autonomous operation over unreliable WAN links, constraint-based placement, and WAN-resilient usage spooling. The generic core is wanted as a reusable gear so that platform-specific bindings stay in the adopting platform. |

### 1.3 Relationship to Existing Gears

Location Manager operates at **estate scope** (many deployments across regions), not within a single deployment. It composes with, and must not duplicate:

| Gear | Boundary |
|------|----------|
| `gears/system/cluster` | Intra-deployment coordination (cache, leader election, locks, discovery). Location Manager instances MAY use it internally; it does not manage remote estates. |
| `gears/system/nodes-registry` | Node inventory *within* one deployment. Location Manager registers whole clusters/deployments as leaf units of a geo hierarchy — one level above. |
| `gears/system/quota-enforcement` | Authoritative quota engine. Location Manager declares geo-scope subjects (geography/region/az) via the Subject Type Registry and calls QE at placement admission; it does not implement its own counters. |
| `gears/system/usage-collector` | Central usage store. Location Manager's spoke-side spool flushes into it; aggregation and retention stay in Usage Collector. |
| `gears/system/event-broker` | Delivery of fleet lifecycle/alert events emitted by Location Manager. |
| `gears/system/types-registry` | Candidate home for the governed constraint-vocabulary registry (see `cpt-cf-location-manager-upreq-constraint-vocabulary-registry`). |

## 2. Requirements

### 2.1 Multi-Region Control Plane

#### Geographic Topology Registry

- [ ] `p1` - **ID**: `cpt-cf-location-manager-upreq-geo-topology`

The future gear **MUST** maintain a four-level topology registry — `geography` (optional regulatory grouping) → `region` (sovereignty/compliance boundary, carrying extensible compliance attributes such as data-residency tags) → `availability zone` (failure domain) → `cluster` (leaf deployment unit hosting exactly one fleet agent) — with each level addressable as a typed node and usable as a scope for placement, quota, and configuration.

- **Rationale**: The hierarchy matches industry conventions (AWS partition/region/AZ, Azure geography/region/AZ, GCP multi-region/region/zone) and is the substrate every other fleet capability scopes against.

#### Tier-Scoped Topology Administration

- [ ] `p2` - **ID**: `cpt-cf-location-manager-upreq-scoped-topology-admin`

The future gear **MUST** allow topology mutation rights to be assigned per hierarchy level to different administrative tiers: the top-level estate owner defines regions and their compliance attributes; delegated lower tiers MAY be granted rights to define availability zones and attach clusters within owner-defined regions, but MUST NOT mutate region-level compliance attributes.

- **Rationale**: Multi-tier operator/partner models require delegation without ceding sovereignty-relevant attributes; the embedding platform maps its own tenant tiers onto these rights.

#### Agent Registration & Deregistration

- [ ] `p1` - **ID**: `cpt-cf-location-manager-upreq-agent-registration`

The future gear **MUST** register clusters via time-limited, single-use registration tokens bound to a target region/AZ scope; a registered cluster receives a per-cluster mTLS identity bound to that scope. Deregistration MUST drain first, then revoke: the hub MUST hold the cluster's certificate valid until the agent's durable spool is acknowledged flushed (see `cpt-cf-location-manager-upreq-usage-spool`) or a configurable drain timeout expires, then remove the cluster node and revoke certificate material; unused registration tokens MUST be revoked immediately. If draining fails or times out — or an operator forces immediate revocation for a compromised cluster — the residual spool MUST be surfaced as an operator alert with record counts, never silently discarded. A residual spool left by a failed drain or forced revocation MUST survive on the cluster in a documented, exportable on-disk format and MUST be retained until the estate operator (the recovery owner) either completes an out-of-band export/replay to the downstream consumer or explicitly discards it; replayed records keep their original record IDs (see `cpt-cf-location-manager-upreq-usage-spool`), so downstream deduplication makes replay safe. The alert MUST identify the cluster, spool location, and record count. The parent region/AZ MUST remain in place.

- **Rationale**: Scope-bound single-use tokens prevent replay and cross-scope registration; a compromised agent must not be able to impersonate another cluster or cross region/AZ boundaries.

#### Brownfield Attach

- [ ] `p2` - **ID**: `cpt-cf-location-manager-upreq-brownfield-attach`

The future gear **MUST** support attaching a live, already-operating cluster in place — without workload disruption or data migration — after which the agent discovers existing local resources and projects them as typed nodes into the hub's resource model. Mapping discovered resources onto the embedding platform's tenancy/billing model is the embedding platform's responsibility.

- **Rationale**: Estates are consolidated brownfield-first; requiring migration would block adoption. The generic part is non-disruptive attach + discovery projection; tenant mapping semantics differ per platform.

#### Declarative State Synchronization

- [ ] `p1` - **ID**: `cpt-cf-location-manager-upreq-declarative-sync`

The future gear **MUST** synchronize desired state hub→spoke via asynchronous pull at a configurable interval, and MUST detect and report drift between desired and actual state to the hub.

- **Rationale**: Pull-based declarative sync scales the hub, survives NAT/firewall asymmetry, and makes reconciliation after outages a first-class operation rather than a special case.

#### Autonomous (Disconnected) Mode

- [ ] `p1` - **ID**: `cpt-cf-location-manager-upreq-autonomous-mode`

The future gear **MUST** transition a spoke to autonomous mode automatically when heartbeat misses exceed a configured timeout: local operation continues, centrally-initiated provisioning is suspended, and locally accumulated state/usage records are durably spooled. On reconnection the spoke MUST automatically exit autonomous mode, submit its state delta, and the hub MUST reconcile by merging spoke-reported actual state. Exceeding a configured autonomous-duration threshold MUST raise an alert carrying cluster identity, parent scope, entry time, elapsed duration, and last successful sync.

- **Rationale**: WAN outages must not cascade into data-plane disruption; disconnected operation with deterministic reconvergence is the defining property of a hub-spoke estate versus a stretched control plane.

#### Agent Configuration Inheritance

- [ ] `p1` - **ID**: `cpt-cf-location-manager-upreq-agent-config-inheritance`

The future gear **MUST** make agent parameters (heartbeat interval, autonomous-mode trigger timeout, autonomous-duration alert threshold, sync interval) operator-configurable with most-specific-wins inheritance: per-cluster overrides per-region overrides estate-wide defaults; changes propagate on the next successful sync cycle.

- **Rationale**: Heterogeneous links (metro fiber vs. satellite edge) need different tolerances; per-cluster hand-configuration alone does not scale to hundreds of clusters.

#### Agent Lifecycle Management

- [ ] `p1` - **ID**: `cpt-cf-location-manager-upreq-agent-lifecycle`

The future gear **MUST** manage the full agent lifecycle from the hub: rolling upgrades (scoped region→AZ→cluster) during which the hub MUST accept both its current agent release and exactly one previous agent major.minor release (N-1, where N is the agent major.minor the hub currently ships); mTLS certificate auto-renewal before expiry without re-registration or connectivity interruption; automatic rollback to the prior version on failed post-upgrade health checks (with an emitted rollback event); idempotent clean uninstall on deregistration (retaining the durable spool until acknowledged flushed, per the drain-then-revoke sequence in `cpt-cf-location-manager-upreq-agent-registration`); and a hub↔agent compatibility matrix enforced at handshake with reason-coded rejection, using this same version definition.

- **Rationale**: A fleet of hundreds of agents is only operable if upgrade, rotation, rollback, and removal are hub-orchestrated invariants rather than per-cluster manual procedures.

#### Constraint-Set Placement Resolution

- [ ] `p1` - **ID**: `cpt-cf-location-manager-upreq-constraint-placement`

The future gear **MUST** resolve placement requests expressed as constraint sets over the topology vocabulary (e.g., `{region, az_affinity, compliance}`) to a target cluster as the intersection of all hard constraints, without requiring the caller to name a cluster. Ties MUST be broken in a fixed deterministic order, all rungs normative for all implementations and evaluated against the same hub admission-time estate snapshot: **(1) soft affinity** — `az_affinity: spread` prefers the candidate whose AZ hosts the fewest of the requesting subject's already-placed resources, `pack` the most, `none` skips this rung; equal counts fall through; **(2) lowest weighted utilization** — per-dimension utilization = allocated ÷ usable capacity for cpu, ram, and storage from the same snapshot, each clamped to [0,1]; score = weighted sum using a single estate-scope weight configuration (normative defaults 0.5 cpu / 0.3 ram / 0.2 storage), identical for all resolvers of the estate and recorded in the audit record; scores compared in integer basis points (round-half-up to 1/10000) to avoid float drift; equal scores fall through; **(3) deterministic hash** — for each candidate cluster, compute the unkeyed (no seed) SHA-256 digest of the byte sequence `uint32_be(byte-length of UTF-8(request_id)) ‖ UTF-8(request_id) ‖ uint32_be(byte-length of UTF-8(cluster_id)) ‖ UTF-8(cluster_id)` — length-prefixed framing so distinct pairs never serialize identically — and the candidate whose digest is lowest under lexicographic byte comparison wins. The decision audit record MUST capture the resolved scope, the per-rung scoring inputs and computed scores, and the selected tie-break rung so any implementation independently reproduces the same winner, and unsatisfiable requests MUST be denied with a reason code identifying the unsatisfied constraint(s). Hard residency constraints MUST deny placement outside the designated scope.

- **Rationale**: Intent-based placement is the estate's core user-facing contract; determinism and decision traceability are what make it auditable and debuggable. Embedding platforms MAY wrap this resolver behind their own policy engines for additional deny-overrides policy layers.

#### Constraint Vocabulary Registry

- [ ] `p2` - **ID**: `cpt-cf-location-manager-upreq-constraint-vocabulary-registry`

The future gear **MUST** validate placement constraints against a governed vocabulary registry owned by the estate owner: lower tiers MAY propose new keys/values via an approval workflow, ad-hoc unregistered vocabulary MUST be rejected with a distinct reason code, and the vocabulary MUST be runtime-extensible without code changes. (Candidate composition: `gears/system/types-registry` as the registry substrate.)

- **Rationale**: An ungoverned tag vocabulary fragments the estate (same compliance regime spelled three ways) and turns placement into guesswork; governance belongs at the platform, not in each caller.

#### Geo-Scoped Quota Subjects

- [ ] `p2` - **ID**: `cpt-cf-location-manager-upreq-scoped-quota`

The future gear **MUST** enforce per-scope quotas (per region, per AZ, optional geography aggregates) at placement admission by declaring geo-scope subject types to `gears/system/quota-enforcement` and consulting it before resolution completes; denials MUST identify the exhausted scope and current usage. Location Manager MUST NOT implement its own consumption counters.

- **Rationale**: Distributor→partner capacity delegation is per-scope; QE already owns deterministic quota decisions, so Location Manager's contribution is the scope subject model and the admission call, not a parallel engine.

#### WAN-Resilient Usage Spooling

- [ ] `p1` - **ID**: `cpt-cf-location-manager-upreq-usage-spool`

The future gear's spoke **MUST** durably spool usage and observability records locally during disconnection and flush them on reconnection in emission order per resource and per subject, with at-least-once delivery (deduplication owned by the downstream consumer). Every record MUST carry an immutable, globally unique record ID (idempotency key) assigned at emission and stable across retries and re-flushes — scope attributes alone are not an identity — plus the full structured scope (`geography`, `region`, `az`, `cluster_id`) as stable attributes; downstream deduplication keys on the record ID. (Composition: flush target is `gears/system/usage-collector` or the embedding platform's metering endpoint.)

- **Rationale**: Metering gaps during WAN outages are unbillable revenue for the embedding platform; ordered at-least-once flush with scope attribution is the generic contract that makes downstream billing possible.

#### Scope-Targeted Artifact Syndication

- [ ] `p3` - **ID**: `cpt-cf-location-manager-upreq-artifact-syndication`

The future gear **MUST** propagate owner- or tier-published artifacts (e.g., resource templates) to clusters matching a syndication scope (all regions, selected regions, selected clusters) via the agent sync channel, with updates propagated on subsequent sync cycles and visibility filtered by scope.

- **Rationale**: Consistent catalogs across an estate are otherwise maintained by hand per cluster; syndication reuses the sync channel that already exists.

#### Fleet Health & Observability Model

- [ ] `p2` - **ID**: `cpt-cf-location-manager-upreq-fleet-observability`

The future gear **MUST** expose a fleet health data model with status roll-up from cluster to AZ to region, distinct marking of clusters in autonomous mode (with elapsed duration), and per-cluster drill-down facts: connectivity status, last successful sync, heartbeat history, spool depth, and active alerts. Alerts and lifecycle events MUST be emitted via the platform event mechanism (`gears/system/event-broker`). UI presentation stays with the embedding platform.

- **Rationale**: The hub is only trustworthy if estate health is observable at every scope level; the data model is generic even though each platform renders its own map/dashboard.

#### Scope-Filtered Estate Queries

- [ ] `p3` - **ID**: `cpt-cf-location-manager-upreq-scoped-query`

The future gear **MUST** answer estate queries filtered by any scope level (global, geography, region, AZ, cluster) and combinations thereof, returning scope attributes per row, so embedding platforms can build global/context-switched views without client-side aggregation.

- **Rationale**: Cross-region resource lists and context switchers are universal estate UX; the server-side filtered query is the reusable part.

#### Agent Resource Footprint

- [ ] `p2` - **ID**: `cpt-cf-location-manager-upreq-agent-footprint`

The future gear's agent **MUST** operate within a small documented steady-state footprint suitable for edge deployment (reference bounds from the requesting source: ≤ 200m CPU / ≤ 512 MiB RSS per cluster-scoped agent; ≤ 100m CPU / ≤ 256 MiB RSS per single-node agent), with validated thresholds documented and not relaxable without requirement amendment.

- **Rationale**: Spokes run on customer-owned, possibly edge-class hardware; an agent that competes with workloads for resources will be uninstalled.

## 3. Priorities

| Priority | Requirements |
|----------|-------------|
| p1 (critical) | `cpt-cf-location-manager-upreq-geo-topology`, `cpt-cf-location-manager-upreq-agent-registration`, `cpt-cf-location-manager-upreq-declarative-sync`, `cpt-cf-location-manager-upreq-autonomous-mode`, `cpt-cf-location-manager-upreq-agent-config-inheritance`, `cpt-cf-location-manager-upreq-agent-lifecycle`, `cpt-cf-location-manager-upreq-constraint-placement`, `cpt-cf-location-manager-upreq-usage-spool` |
| p2 (important) | `cpt-cf-location-manager-upreq-scoped-topology-admin`, `cpt-cf-location-manager-upreq-brownfield-attach`, `cpt-cf-location-manager-upreq-constraint-vocabulary-registry`, `cpt-cf-location-manager-upreq-scoped-quota`, `cpt-cf-location-manager-upreq-fleet-observability`, `cpt-cf-location-manager-upreq-agent-footprint` |
| p3 (nice-to-have) | `cpt-cf-location-manager-upreq-artifact-syndication`, `cpt-cf-location-manager-upreq-scoped-query` |

## 4. Traceability

- **Product requirements** (when created): [PRD.md](./PRD.md)
- **Design** (when created): DESIGN.md

### Provenance

All requirements in §2 are generalised from a production multi-region infrastructure control plane contributed by an adopting platform. They are
restated here as self-contained, implementation-ready requirements: no external or non-public document is required to build,
review, or test this gear. The contributing platform maintains the mapping from its own specification to the
`cpt-cf-location-manager-upreq-*` identifiers above, so upstream changes can be traced in both directions.

Deliberately **out of scope** for this gear — these belong to the adopting platform, not the commons:

- Resource-graph node-type bindings and scope-URN schemes
- Policy-engine admission semantics and policy authoring surfaces
- Tenant-tier permission models and commercial hierarchies
- Product-specific cluster typing and brownfield tenant-to-billing mapping
- Hub high-availability deployment profiles (Kubernetes topology, database HA, PITR bounds)
- Billing-system delivery contracts and regional pricing
- Geo-replicated object-storage semantics (conflict resolution, clock-skew bounds, conflict audit)
- All portal UX and mockups
