# PRD — Location Manager

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
  - [1.5 Relationship to Existing Gears](#15-relationship-to-existing-gears)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 Topology & Administration](#51-topology--administration)
  - [5.2 Cluster Registration & Agent Lifecycle](#52-cluster-registration--agent-lifecycle)
  - [5.3 Synchronization & Autonomy](#53-synchronization--autonomy)
  - [5.4 Placement](#54-placement)
  - [5.5 Usage & Observability](#55-usage--observability)
  - [5.6 Artifact Syndication](#56-artifact-syndication)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
- [7. Public Library Interfaces](#7-public-library-interfaces)
- [8. Use Cases](#8-use-cases)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Quality Vectors Analysis](#10-quality-vectors-analysis)
- [11. Dependencies](#11-dependencies)
- [12. Assumptions](#12-assumptions)
- [13. Risks](#13-risks)
- [14. Open Questions](#14-open-questions)
- [15. Traceability](#15-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

**Location Manager** is a CF/Gears subsystem (gear) that manages a geographically distributed
estate of clusters from a single hub: it maintains the geographic topology registry
(geography → region → availability zone → cluster), registers and lifecycle-manages the fleet
agents that connect clusters to the hub, synchronizes desired state hub→spoke with
disconnected-autonomous operation over unreliable WAN links, resolves constraint-based
placement requests deterministically against an admission-time estate snapshot (with quota
admission and immutable, expiring placement decision records for asynchronous consumers), and
spools usage/observability records WAN-resiliently so metering survives outages.

This PRD consolidates and supersedes the earlier requirements intake for this gear
(`UPSTREAM_REQS.md`, contributed from a production multi-region infrastructure control plane):
the intake described what adopters need from the gear; this document describes what the gear
does. Every requirement remains self-contained — no external or non-public document is needed
to implement, review, or test it. The gear is tracked on the platform roadmap as
[#4324](https://github.com/constructorfabric/gears-rust/issues/4324).

### 1.2 Background / Problem Statement

Platforms that grow past a single deployment face the same structural problems:

1. **No estate-scope control plane** — each cluster is managed in isolation; topology
   (which cluster is in which region, under which compliance regime) lives in spreadsheets,
   and cross-cluster operations are per-cluster manual procedures.
2. **WAN links are not LAN links** — a stretched control plane fails when the WAN does.
   Remote clusters need to keep operating, keep metering, and reconverge deterministically
   when connectivity returns — not cascade a link outage into a data-plane incident.
3. **Placement by hand or by guesswork** — without constraint-based, deterministic, auditable
   placement, "put this workload in a compliant region with AZ spread" is tribal knowledge,
   and no two resolvers pick the same cluster for the same request.
4. **Commercial commitments race capacity** — order-based consumers accept commercially before
   they fulfill technically; without an immutable, expiring placement decision reference they
   either commit against stale capacity or silently relocate the customer.
5. **Metering gaps are unbillable revenue** — usage records emitted during a WAN outage are
   lost unless the spoke spools them durably and flushes them with identities that make
   downstream deduplication safe.
6. **Fleet agents rot** — hundreds of agents are only operable if upgrade, certificate
   rotation, rollback, and clean removal are hub-orchestrated invariants.

### 1.3 Goals (Business Outcomes)

- Provide one reusable hub-spoke estate control plane for CF/Gears platforms, so each adopting
  platform binds its own semantics (resource-graph node types, policy admission, tenant tiers,
  billing contracts) on top of a generic core instead of rebuilding it
- Make geographic topology a first-class, delegable registry with sovereignty-preserving
  administration (owner defines regions and compliance attributes; delegated tiers manage
  zones and clusters within them)
- Make WAN outage a non-event: automatic autonomous mode, durable local spooling, and
  deterministic, idempotent reconvergence on reconnection
- Make placement intent-based, deterministic, and auditable — same request, same snapshot,
  same winner, on any implementation — with quota admission that cannot double-consume a scope
- Give asynchronous (order/fulfillment) consumers a bindable, expiring placement decision
  record with explicit expiry/invalidation/revalidation semantics — never silent relocation
- Keep every boundary composable: quota counters stay with Quota Enforcement, usage
  aggregation with Usage Collector, event delivery with Event Broker, intra-deployment
  coordination with the Cluster gear

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Estate | The full set of clusters managed by one hub across all geographies |
| Geography | Optional top-level regulatory grouping of regions |
| Region | Sovereignty/compliance boundary carrying extensible compliance attributes (e.g., data-residency tags) |
| Availability Zone (AZ) | Failure domain within a region |
| Cluster | Leaf deployment unit of the topology, hosting exactly one fleet agent |
| Hub | The Location Manager control plane: topology registry, sync source of desired state, placement resolver, fleet lifecycle orchestrator |
| Spoke / Fleet Agent | The per-cluster agent that registers with the hub, pulls desired state, reports actual state and heartbeats, spools usage during disconnection, and manages local reconciliation |
| Registration Token | Time-limited, single-use token bound to a target region/AZ scope, exchanged at registration for a per-cluster mTLS identity bound to that scope |
| Drain-then-Revoke | The deregistration order: the cluster's certificate stays valid until its durable spool is acknowledged flushed (or a configurable drain timeout expires), then the cluster is removed and certificate material revoked |
| Autonomous Mode | The spoke state entered automatically on heartbeat loss: local operation continues, centrally-initiated provisioning is suspended, state and usage are durably spooled |
| Estate Snapshot | The hub's admission-time view of topology, capacity, and availability against which one placement resolution is evaluated; identified so decisions are reproducible |
| Constraint Set | A placement request expressed over the governed topology vocabulary (e.g., `{region, az_affinity, compliance}`) — never a named cluster |
| Placement Decision Record | The immutable, uniquely identified materialization of one successful resolution: snapshot identity, selected cluster, resolved constraints, per-rung scoring data, quota lease reference, availability state, validity period, reason codes |
| Revalidation | A fresh resolution against the current snapshot producing a new decision record — never a replay of a stored one |
| Usage Spool | The spoke's durable, capacity-bounded local store of usage/observability records emitted during disconnection, flushed at-least-once in emission order with immutable record IDs |
| Syndication Scope | The target set (all regions, selected regions, selected clusters) to which an owner-/tier-published artifact propagates via the sync channel |
| Compatibility Window (N−1) | The hub accepts its current agent major.minor release and exactly one previous major.minor release |

### 1.5 Relationship to Existing Gears

Location Manager operates at **estate scope** (many deployments across regions), not within a
single deployment. It composes with, and must not duplicate:

| Gear / artifact | Boundary |
|------|----------|
| `gears/system/cluster` | Intra-deployment coordination (cache, leader election, locks, discovery). Location Manager instances MAY use it internally; it does not manage remote estates. |
| `gears/system/nodes-registry` | Node inventory *within* one deployment. Location Manager registers whole clusters/deployments as leaf units of a geo hierarchy — one level above. |
| `gears/system/quota-enforcement` | Authoritative quota engine. Location Manager declares geo-scope subjects and calls it at placement admission; it does not implement its own counters. |
| `gears/system/usage-collector` | Central usage store. The spoke-side spool flushes into it; aggregation and retention stay there. |
| `gears/system/event-broker` | Delivery of fleet lifecycle/alert events emitted by Location Manager. |
| `gears/system/types-registry` | Candidate home for the governed constraint-vocabulary registry (`cpt-cf-location-manager-fr-constraint-vocabulary-registry`). |
| Infrastructure Resource Manager (`gears/infrastructure-resource-manager`) | IRM orchestrates resources *within* the platform's deployment scope — resource types, adapters, deployments, day-2 actions, and its own discovery/inventory. Location Manager owns the *estate* dimension: geographic topology, cluster registration, WAN-autonomous spokes, and cross-cluster placement. IRM's own scope decision keeps a later placement dimension open; the composition seam between IRM deployments and Location Manager placement decisions is an open question (§14). |

## 2. Actors

### 2.1 Human Actors

#### Estate Owner

**ID**: `cpt-cf-location-manager-actor-estate-owner`

- **Role**: Defines geographies, regions, and region-level compliance attributes; owns the
  constraint vocabulary governance and estate-wide agent configuration defaults; delegates
  scoped administration to lower tiers.
- **Needs**: Sovereignty-preserving delegation — lower tiers can operate zones and clusters
  without being able to mutate region compliance attributes or the governed vocabulary.

#### Delegated Scope Administrator

**ID**: `cpt-cf-location-manager-actor-scope-admin`

- **Role**: Within owner-granted scopes, defines availability zones, attaches/registers
  clusters, configures per-scope agent parameters, and runs scoped estate queries.
- **Needs**: Full operational capability inside the granted subtree; hard walls outside it —
  including in query results, which must not leak rows beyond the authorized subtree.

#### Estate Operator

**ID**: `cpt-cf-location-manager-actor-estate-operator`

- **Role**: Operates the fleet day to day: monitors fleet health and autonomous-mode alerts,
  runs rolling agent upgrades, resolves brownfield discovery conflicts, and owns residual-spool
  recovery (export/replay or explicit discard) after failed drains or forced revocations.
- **Needs**: Status roll-up from cluster to region, per-cluster drill-down, and alerts that
  carry enough identity (cluster, scope, counts, timestamps) to act without archaeology.

### 2.2 System Actors

#### Fleet Agent (Spoke)

**ID**: `cpt-cf-location-manager-actor-fleet-agent`

- **Role**: Registers with a scope-bound token; pulls desired state at the configured
  interval; reports heartbeat, actual state, and drift; enters/exits autonomous mode; spools
  and flushes usage records; performs discovery projection on brownfield attach; upgrades,
  rolls back, renews certificates, and uninstalls under hub orchestration.

#### Placement Consumer

**ID**: `cpt-cf-location-manager-actor-placement-consumer`

- **Role**: Submits constraint-set placement requests and consumes the resolution
  synchronously (deny reasons included). The embedding platform MAY wrap this behind its own
  policy engine for additional deny-overrides layers.

#### Asynchronous Order/Fulfillment Consumer

**ID**: `cpt-cf-location-manager-actor-async-consumer`

- **Role**: Accepts orders or commits subscriptions before fulfillment starts; binds the
  commitment to a placement decision record by ID; revalidates on expiry/invalidation.
  Product, tenant, commercial, and lifecycle eligibility stay with this consumer — the
  decision record asserts topology, capacity, and quota admission only.

#### Quota Enforcement

**ID**: `cpt-cf-location-manager-actor-quota-enforcement`

- **Role**: Authoritative quota engine. Receives geo-scope subject-type declarations; answers
  race-free, idempotent debit/lease admission calls keyed by the placement request.

#### Usage Collector

**ID**: `cpt-cf-location-manager-actor-usage-collector`

- **Role**: Flush target for spoke usage spools; owns aggregation, retention, and downstream
  deduplication keyed on immutable record IDs.

#### Event Broker

**ID**: `cpt-cf-location-manager-actor-event-broker`

- **Role**: Delivers fleet lifecycle/alert events (registration, autonomous-mode entry/exit
  and duration alerts, rollback events, decision invalidation signals, spool watermark alerts).

## 3. Operational Concept & Environment

> **Note**: Runtime, OS, architecture, lifecycle policy, and gear integration patterns are
> defined in this repository's foundational documents. This section captures only this gear's
> deviations.

- The hub runs as a platform gear; **fleet agents run on remote, possibly customer-owned,
  possibly edge-class clusters** connected over unreliable WAN links with NAT/firewall
  asymmetry — hence pull-based sync and agent-initiated connections.
- The agent is a separately shipped, hub-orchestrated component with an explicit
  hub↔agent compatibility window (N−1) and a documented resource footprint (§6).
- Disconnected operation is a designed-for state, not an error state: every spoke capability
  (local operation, spooling, reconvergence) assumes the WAN can be absent for extended
  periods.

## 4. Scope

### 4.1 In Scope

**Topology layer**:
- Four-level geographic topology registry with typed, addressable nodes
- Tier-scoped topology administration (sovereignty-preserving delegation)
- Governed constraint vocabulary with approval workflow (registry substrate composable)

**Fleet layer**:
- Scope-bound, single-use token registration; per-cluster mTLS identity
- Drain-then-revoke deregistration with residual-spool recovery contract
- Brownfield attach with idempotent discovery projection and conflict surfacing
- Hub-orchestrated agent lifecycle: rolling upgrades (N−1 window), certificate auto-renewal,
  automatic rollback on failed health checks, idempotent clean uninstall, handshake-enforced
  compatibility matrix with visible quarantine
- Agent configuration inheritance (cluster > region > estate, most-specific-wins)

**Sync & autonomy layer**:
- Pull-based declarative state synchronization with drift detection and reporting
- Automatic autonomous mode with deterministic, idempotent reconvergence and duration alerts

**Placement layer**:
- Constraint-set resolution with a fixed, normative, reproducible tie-break order against an
  identified admission-time estate snapshot; reason-coded denials
- Geo-scoped quota admission via Quota Enforcement (lease-based, race-free)
- Immutable, expiring placement decision records for asynchronous consumers, with explicit
  expiry/invalidation/revalidation semantics

**Usage & observability layer**:
- WAN-resilient usage spooling: durable, capacity-bounded, watermark-alerted, at-least-once
  ordered flush with immutable record IDs
- Fleet health data model with scope roll-up, autonomous-mode marking, and per-cluster
  drill-down; events via Event Broker
- Scope-filtered estate queries evaluated within the caller's authorized subtree

**Distribution layer**:
- Scope-targeted artifact syndication over the sync channel

### 4.2 Out of Scope

These belong to the adopting platform or to sibling gears, not to this gear:

- Resource-graph node-type bindings and scope-URN schemes
- Policy-engine admission semantics and policy authoring surfaces (adopters MAY layer
  deny-overrides policy on top of the resolver)
- Tenant-tier permission models and commercial hierarchies (the delegation model consumes the
  embedding platform's authorization; it does not define one)
- Product-specific cluster typing and brownfield tenant-to-billing mapping
- Hub high-availability deployment profiles (orchestrator topology, database HA, PITR bounds)
- Billing-system delivery contracts and regional pricing
- Geo-replicated object-storage semantics (conflict resolution, clock-skew bounds)
- Quota counters and usage aggregation (owned by Quota Enforcement / Usage Collector)
- Intra-deployment coordination and node inventory (owned by the Cluster gear /
  nodes-registry)
- All portal UX and mockups — the fleet health model is data, not dashboards

## 5. Functional Requirements

> Requirement IDs follow the gear PRD convention `cpt-cf-location-manager-fr-*`. Each
> requirement originated as an intake requirement (`…-upreq-*`); the 1:1 ID migration table in
> §15 preserves traceability for adopters that referenced the intake IDs.

### 5.1 Topology & Administration

#### Geographic Topology Registry

- [ ] `p1` - **ID**: `cpt-cf-location-manager-fr-geo-topology`

The system **MUST** maintain a four-level topology registry — `geography` (optional regulatory
grouping) → `region` (sovereignty/compliance boundary, carrying extensible compliance
attributes such as data-residency tags) → `availability zone` (failure domain) → `cluster`
(leaf deployment unit hosting exactly one fleet agent) — with each level addressable as a
typed node and usable as a scope for placement, quota, and configuration.

- **Rationale**: The hierarchy matches industry conventions (partition/region/AZ;
  geography/region/AZ; multi-region/region/zone) and is the substrate every other fleet
  capability scopes against.
- **Actors**: `cpt-cf-location-manager-actor-estate-owner`, `cpt-cf-location-manager-actor-scope-admin`

#### Tier-Scoped Topology Administration

- [ ] `p2` - **ID**: `cpt-cf-location-manager-fr-scoped-topology-admin`

The system **MUST** allow topology mutation rights to be assigned per hierarchy level to
different administrative tiers: the top-level estate owner defines regions and their
compliance attributes; delegated lower tiers MAY be granted rights to define availability
zones and attach clusters within owner-defined regions, but MUST NOT mutate region-level
compliance attributes.

- **Rationale**: Multi-tier operator/partner models require delegation without ceding
  sovereignty-relevant attributes; the embedding platform maps its own tenant tiers onto these
  rights.
- **Actors**: `cpt-cf-location-manager-actor-estate-owner`, `cpt-cf-location-manager-actor-scope-admin`

#### Constraint Vocabulary Registry

- [ ] `p2` - **ID**: `cpt-cf-location-manager-fr-constraint-vocabulary-registry`

The system **MUST** validate placement constraints against a governed vocabulary registry
owned by the estate owner: lower tiers MAY propose new keys/values via an approval workflow,
ad-hoc unregistered vocabulary MUST be rejected with a distinct reason code, and the
vocabulary MUST be runtime-extensible without code changes. (Candidate composition:
`gears/system/types-registry` as the registry substrate — see §14.)

- **Rationale**: An ungoverned tag vocabulary fragments the estate (the same compliance regime
  spelled three ways) and turns placement into guesswork; governance belongs at the platform,
  not in each caller.
- **Actors**: `cpt-cf-location-manager-actor-estate-owner`, `cpt-cf-location-manager-actor-scope-admin`

### 5.2 Cluster Registration & Agent Lifecycle

#### Registration & Drain-then-Revoke Deregistration

- [ ] `p1` - **ID**: `cpt-cf-location-manager-fr-agent-registration`

The system **MUST** register clusters via time-limited, single-use registration tokens bound
to a target region/AZ scope; a registered cluster receives a per-cluster mTLS identity bound
to that scope. Deregistration MUST drain first, then revoke: the hub MUST hold the cluster's
certificate valid until the agent's durable spool is acknowledged flushed (see
`cpt-cf-location-manager-fr-usage-spool`) or a configurable drain timeout expires, then remove
the cluster node and revoke certificate material; unused registration tokens MUST be revoked
immediately. If draining fails or times out — or an operator forces immediate revocation for a
compromised cluster — the residual spool MUST be surfaced as an operator alert with record
counts, never silently discarded. A residual spool left by a failed drain or forced revocation
MUST survive on the cluster in a documented, exportable on-disk format and MUST be retained
until the estate operator (the recovery owner) either completes an out-of-band export/replay
to the downstream consumer or explicitly discards it; replayed records keep their original
record IDs, so downstream deduplication makes replay safe. The alert MUST identify the
cluster, spool location, and record count. A residual spool MUST stay under the same access
protection as the live spool: readable, exportable, and discardable only under the recovery
owner's authority, with export and discard both audited operations; at-rest protection
mechanics (encryption and key custody) follow the embedding platform's data-at-rest
conventions and are settled in the gear's design. The parent region/AZ MUST remain in place.

- **Rationale**: Scope-bound single-use tokens prevent replay and cross-scope registration; a
  compromised agent must not be able to impersonate another cluster or cross region/AZ
  boundaries; drain-then-revoke prevents stranding billable records behind a revoked identity.
- **Actors**: `cpt-cf-location-manager-actor-scope-admin`, `cpt-cf-location-manager-actor-estate-operator`, `cpt-cf-location-manager-actor-fleet-agent`

#### Brownfield Attach

- [ ] `p2` - **ID**: `cpt-cf-location-manager-fr-brownfield-attach`

The system **MUST** support attaching a live, already-operating cluster in place — without
workload disruption or data migration — after which the agent discovers existing local
resources and projects them as typed nodes into the hub's resource model. Discovery projection
MUST be idempotent: each discovered resource is keyed by an immutable provider-native source
identifier, repeated discovery runs and renames update the same node rather than duplicating
it, and a resource already attached under another owner MUST surface as a reason-coded
conflict for operator resolution — never be silently re-parented. Mapping discovered resources
onto the embedding platform's tenancy/billing model is the embedding platform's
responsibility.

- **Rationale**: Estates are consolidated brownfield-first; requiring migration would block
  adoption. The generic part is non-disruptive attach + discovery projection; tenant mapping
  semantics differ per platform.
- **Actors**: `cpt-cf-location-manager-actor-estate-operator`, `cpt-cf-location-manager-actor-fleet-agent`

#### Agent Lifecycle Management

- [ ] `p1` - **ID**: `cpt-cf-location-manager-fr-agent-lifecycle`

The system **MUST** manage the full agent lifecycle from the hub: rolling upgrades (scoped
region→AZ→cluster) during which the hub MUST accept both its current agent release and exactly
one previous agent major.minor release (N−1, where N is the agent major.minor the hub
currently ships); mTLS certificate auto-renewal before expiry without re-registration or
connectivity interruption; automatic rollback to the prior version on failed post-upgrade
health checks (with an emitted rollback event); idempotent clean uninstall on deregistration
(retaining the durable spool until acknowledged flushed, per the drain-then-revoke sequence in
`cpt-cf-location-manager-fr-agent-registration`); and a hub↔agent compatibility matrix
enforced at handshake with reason-coded rejection, using this same version definition. A
rolling upgrade MUST complete or explicitly abandon the N−1 cohort before the hub advances
beyond N, and an agent that reconnects below the hub's compatibility window MUST be
quarantined visibly with a reason code and remain hub-upgradable in place — never silently
rejected into a stranded state.

- **Rationale**: A fleet of hundreds of agents is only operable if upgrade, rotation,
  rollback, and removal are hub-orchestrated invariants rather than per-cluster manual
  procedures.
- **Actors**: `cpt-cf-location-manager-actor-estate-operator`, `cpt-cf-location-manager-actor-fleet-agent`

#### Agent Configuration Inheritance

- [ ] `p1` - **ID**: `cpt-cf-location-manager-fr-agent-config-inheritance`

The system **MUST** make agent parameters (heartbeat interval, autonomous-mode trigger
timeout, autonomous-duration alert threshold, sync interval) operator-configurable with
most-specific-wins inheritance: per-cluster overrides per-region overrides estate-wide
defaults; changes propagate on the next successful sync cycle.

- **Rationale**: Heterogeneous links (metro fiber vs. satellite edge) need different
  tolerances; per-cluster hand-configuration alone does not scale to hundreds of clusters.
- **Actors**: `cpt-cf-location-manager-actor-estate-operator`

### 5.3 Synchronization & Autonomy

#### Declarative State Synchronization

- [ ] `p1` - **ID**: `cpt-cf-location-manager-fr-declarative-sync`

The system **MUST** synchronize desired state hub→spoke via asynchronous pull at a
configurable interval, and MUST detect and report drift between desired and actual state to
the hub.

- **Rationale**: Pull-based declarative sync scales the hub, survives NAT/firewall asymmetry,
  and makes reconciliation after outages a first-class operation rather than a special case.
- **Actors**: `cpt-cf-location-manager-actor-fleet-agent`

#### Autonomous (Disconnected) Mode

- [ ] `p1` - **ID**: `cpt-cf-location-manager-fr-autonomous-mode`

The system **MUST** transition a spoke to autonomous mode automatically when heartbeat misses
exceed a configured timeout: local operation continues, centrally-initiated provisioning is
suspended, and locally accumulated state/usage records are durably spooled. On reconnection
the spoke MUST automatically exit autonomous mode, submit its state delta, and the hub MUST
reconcile by merging spoke-reported actual state. Reconvergence MUST be deterministic and
idempotent: the hub stays authoritative for desired state, the spoke's delta is authoritative
for locally observed actual state, replaying the same delta MUST NOT change the outcome (state
deltas carry a monotonic per-spoke sequence; usage records already carry record IDs), and a
merge MUST NOT resurrect resources deleted on the hub during the disconnection — such
collisions surface as drift, not silent overwrites. Exceeding a configured autonomous-duration
threshold MUST raise an alert carrying cluster identity, parent scope, entry time, elapsed
duration, and last successful sync.

- **Rationale**: WAN outages must not cascade into data-plane disruption; disconnected
  operation with deterministic reconvergence is the defining property of a hub-spoke estate
  versus a stretched control plane.
- **Actors**: `cpt-cf-location-manager-actor-fleet-agent`, `cpt-cf-location-manager-actor-estate-operator`

### 5.4 Placement

#### Constraint-Set Placement Resolution

- [ ] `p1` - **ID**: `cpt-cf-location-manager-fr-constraint-placement`

The system **MUST** resolve placement requests expressed as constraint sets over the topology
vocabulary (e.g., `{region, az_affinity, compliance}`) to a target cluster as the intersection
of all hard constraints, without requiring the caller to name a cluster. Ties MUST be broken
in a fixed deterministic order, all rungs normative for all implementations and evaluated
against the same hub admission-time estate snapshot: **(1) soft affinity** —
`az_affinity: spread` prefers the candidate whose AZ hosts the fewest of the requesting
subject's already-placed resources, `pack` the most, `none` skips this rung; equal counts fall
through; **(2) lowest weighted utilization** — per-dimension utilization = allocated ÷ usable
capacity for cpu, ram, and storage from the same snapshot, each clamped to [0,1] — a dimension
whose usable capacity is zero, unknown, or absent from the snapshot evaluates to utilization
1.0, never a division; score = weighted sum using a single estate-scope weight configuration
(normative defaults 0.5 cpu / 0.3 ram / 0.2 storage), identical for all resolvers of the
estate and recorded in the audit record; scores compared in integer basis points
(round-half-up to 1/10000) to avoid float drift; equal scores fall through; **(3)
deterministic hash** — for each candidate cluster, compute the unkeyed (no seed) SHA-256
digest of the byte sequence
`uint32_be(byte-length of UTF-8(request_id)) ‖ UTF-8(request_id) ‖ uint32_be(byte-length of UTF-8(cluster_id)) ‖ UTF-8(cluster_id)`
— length-prefixed framing so distinct pairs never serialize identically — and the candidate
whose digest is lowest under lexicographic byte comparison wins. The decision audit record
MUST capture the resolved scope, the per-rung scoring inputs and computed scores, and the
selected tie-break rung so any implementation independently reproduces the same winner, and
unsatisfiable requests MUST be denied with a reason code identifying the unsatisfied
constraint(s). Hard residency constraints MUST deny placement outside the designated scope.

- **Rationale**: Intent-based placement is the estate's core user-facing contract; determinism
  and decision traceability are what make it auditable and debuggable. Embedding platforms MAY
  wrap this resolver behind their own policy engines for additional deny-overrides layers.
- **Actors**: `cpt-cf-location-manager-actor-placement-consumer`

#### Placement Decision Reference (Asynchronous Consumers)

- [ ] `p2` - **ID**: `cpt-cf-location-manager-fr-placement-decision-reference`

The system **MUST** materialize every successful placement resolution as an immutable,
uniquely identified **placement decision record**, retrievable by ID, containing: the
admission-time estate snapshot identity, the selected cluster, the resolved constraint set,
the per-rung scoring data (the audit record of
`cpt-cf-location-manager-fr-constraint-placement`), the quota admission/lease reference
(`cpt-cf-location-manager-fr-scoped-quota`), the selected cluster's availability state at
resolution, an explicit **validity period**, and reason codes. During the validity period the
associated quota lease MUST be held reserved, so an asynchronous consumer — one that accepts
an order or commits a subscription before fulfillment starts — can bind its commitment to the
decision ID without racing capacity changes. The binding is exclusive by reference: the system
MUST NOT substitute a different cluster under an existing decision ID; a changed outcome is
only ever produced by explicit **revalidation**, which is a fresh resolution against the
current snapshot producing a new decision record (never a replay of the stored snapshot), with
reason codes when the outcome differs from the original. On expiry without fulfillment the
lease MUST be released and the decision marked expired; presenting an expired or invalidated
decision MUST be denied with a reason code directing the consumer to revalidate. If the
selected cluster's availability state changes during validity, the decision MUST be marked
invalidated and a change signal emitted via the platform event mechanism, so the consumer
learns before fulfillment rather than at it. Product, tenant, commercial, and lifecycle
eligibility remain the consumer's responsibility; the decision record asserts topology,
capacity, and quota admission only.

- **Rationale**: Order-based consumers accept commercially before they fulfill technically;
  without an immutable, expiring decision reference they either commit against stale capacity
  or silently relocate the customer. Expiry-plus-revalidation keeps the placement contract's
  anti-stale discipline — a fresh resolution against current state, never a replay — while
  giving asynchronous consumers a bindable artifact.
- **Actors**: `cpt-cf-location-manager-actor-async-consumer`

#### Geo-Scoped Quota Admission

- [ ] `p2` - **ID**: `cpt-cf-location-manager-fr-scoped-quota`

The system **MUST** enforce per-scope quotas (per region, per AZ, optional geography
aggregates) at placement admission by declaring geo-scope subject types to
`gears/system/quota-enforcement` and consulting it before resolution completes; denials MUST
identify the exhausted scope and current usage. The admission call MUST be race-free: it uses
Quota Enforcement's idempotent debit/lease primitives keyed by the placement request,
committed when placement resolves and released when it fails; when the resolution produces a
placement decision reference (`cpt-cf-location-manager-fr-placement-decision-reference`), the
lease is held reserved for the decision's validity period and released on expiry,
invalidation, or explicit consumer release. Concurrent resolutions therefore cannot
double-consume a scope's remaining capacity. Location Manager MUST NOT implement its own
consumption counters.

- **Rationale**: Distributor→partner capacity delegation is per-scope; Quota Enforcement
  already owns deterministic quota decisions, so this gear's contribution is the scope subject
  model and the admission call, not a parallel engine.
- **Actors**: `cpt-cf-location-manager-actor-placement-consumer`, `cpt-cf-location-manager-actor-quota-enforcement`

### 5.5 Usage & Observability

#### WAN-Resilient Usage Spooling

- [ ] `p1` - **ID**: `cpt-cf-location-manager-fr-usage-spool`

The system's spoke **MUST** durably spool usage and observability records locally during
disconnection and flush them on reconnection in emission order per resource and per subject,
with at-least-once delivery (deduplication owned by the downstream consumer). Every record
MUST carry an immutable, globally unique record ID (idempotency key) assigned at emission and
stable across retries and re-flushes — scope attributes alone are not an identity — plus the
full structured scope (`geography`, `region`, `az`, `cluster_id`) as stable attributes;
downstream deduplication keys on the record ID. The local spool MUST be capacity-bounded with
configurable high and critical watermarks that raise alerts when crossed; at capacity the
spoke MUST apply the configured policy — backpressure on new emission or oldest-first drop —
and any dropped records MUST be counted per subject and reported to the hub on reconnection;
silent loss is forbidden. (Composition: flush target is `gears/system/usage-collector` or the
embedding platform's metering endpoint.)

- **Rationale**: Metering gaps during WAN outages are unbillable revenue for the embedding
  platform; ordered at-least-once flush with scope attribution is the generic contract that
  makes downstream billing possible.
- **Actors**: `cpt-cf-location-manager-actor-fleet-agent`, `cpt-cf-location-manager-actor-usage-collector`

#### Fleet Health & Observability Model

- [ ] `p2` - **ID**: `cpt-cf-location-manager-fr-fleet-observability`

The system **MUST** expose a fleet health data model with status roll-up from cluster to AZ to
region, distinct marking of clusters in autonomous mode (with elapsed duration), and
per-cluster drill-down facts: connectivity status, last successful sync, heartbeat history,
spool depth, and active alerts. Alerts and lifecycle events MUST be emitted via the platform
event mechanism (`gears/system/event-broker`). UI presentation stays with the embedding
platform.

- **Rationale**: The hub is only trustworthy if estate health is observable at every scope
  level; the data model is generic even though each platform renders its own map/dashboard.
- **Actors**: `cpt-cf-location-manager-actor-estate-operator`, `cpt-cf-location-manager-actor-event-broker`

#### Scope-Filtered Estate Queries

- [ ] `p3` - **ID**: `cpt-cf-location-manager-fr-scoped-query`

The system **MUST** answer estate queries filtered by any scope level (global, geography,
region, AZ, cluster) and combinations thereof, returning scope attributes per row, so
embedding platforms can build global/context-switched views without client-side aggregation.
Every query MUST be evaluated within the caller's authorized scope: the system intersects the
requested filters with the caller's permitted subtree (the delegation model of
`cpt-cf-location-manager-fr-scoped-topology-admin`), and rows outside that subtree MUST NOT be
returned or inferable; the authorization source itself is the embedding platform's.

- **Rationale**: Cross-region resource lists and context switchers are universal estate UX;
  the server-side filtered query is the reusable part.
- **Actors**: `cpt-cf-location-manager-actor-scope-admin`, `cpt-cf-location-manager-actor-placement-consumer`

### 5.6 Artifact Syndication

#### Scope-Targeted Artifact Syndication

- [ ] `p3` - **ID**: `cpt-cf-location-manager-fr-artifact-syndication`

The system **MUST** propagate owner- or tier-published artifacts (e.g., resource templates) to
clusters matching a syndication scope (all regions, selected regions, selected clusters) via
the agent sync channel, with updates propagated on subsequent sync cycles and visibility
filtered by scope.

- **Rationale**: Consistent catalogs across an estate are otherwise maintained by hand per
  cluster; syndication reuses the sync channel that already exists.
- **Actors**: `cpt-cf-location-manager-actor-estate-owner`, `cpt-cf-location-manager-actor-scope-admin`

## 6. Non-Functional Requirements

#### Efficiency: Agent Resource Footprint

- [ ] `p2` - **ID**: `cpt-cf-location-manager-nfr-agent-footprint`

The agent **MUST** operate within a small documented steady-state footprint suitable for edge
deployment (reference bounds from the requesting source: ≤ 200m CPU / ≤ 512 MiB RSS per
cluster-scoped agent; ≤ 100m CPU / ≤ 256 MiB RSS per single-node agent), with validated
thresholds documented and not relaxable without requirement amendment.

- **Threshold**: The documented bounds above, validated per release.
- **Rationale**: Spokes run on customer-owned, possibly edge-class hardware; an agent that
  competes with workloads for resources will be uninstalled.

#### Reliability: No Silent Loss, Deterministic Reconvergence

- [ ] `p1` - **ID**: `cpt-cf-location-manager-nfr-reliability`

No usage record, state delta, or residual spool may be silently lost, and reconvergence after
any disconnection MUST be deterministic and idempotent
(`cpt-cf-location-manager-fr-autonomous-mode`, `cpt-cf-location-manager-fr-usage-spool`,
`cpt-cf-location-manager-fr-agent-registration`).

- **Threshold**: Zero silent record loss under spool-capacity pressure, forced revocation, and
  repeated delta replay; identical reconvergence outcome on replay.
- **Rationale**: The estate contract is only as strong as its worst outage story.

#### Determinism: Reproducible Placement

- [ ] `p1` - **ID**: `cpt-cf-location-manager-nfr-placement-determinism`

Any two conforming implementations given the same placement request, estate snapshot, and
weight configuration MUST select the same cluster and produce equivalent audit records
(`cpt-cf-location-manager-fr-constraint-placement`).

- **Threshold**: 100% reproducibility across implementations and re-evaluations in
  conformance tests.
- **Rationale**: Determinism is what makes placement auditable, debuggable, and safely
  wrappable by adopter policy engines.

#### Scale

- [ ] `p2` - **ID**: `cpt-cf-location-manager-nfr-scale`

The hub **MUST** support estates of at least hundreds of clusters across tens of regions
without per-cluster manual procedures becoming necessary; agent parameters tune per scope
(`cpt-cf-location-manager-fr-agent-config-inheritance`), and rolling upgrades operate on
scoped cohorts. Exact sizing targets are settled in the design.

- **Threshold**: Reference target — ≥ 500 clusters, ≥ 20 regions per estate — without
  degrading sync or placement below design-set latency bounds.
- **Rationale**: The rationale for hub orchestration collapses if operations remain
  O(cluster) manual work.

#### Security Baseline

- [ ] `p1` - **ID**: `cpt-cf-location-manager-nfr-security`

Registration is scope-bound and single-use; each cluster's identity is per-cluster mTLS bound
to its scope; a compromised agent MUST NOT be able to impersonate another cluster or cross
region/AZ boundaries; residual-spool export and discard are audited, recovery-owner-only
operations; scoped queries MUST NOT leak rows outside the caller's authorized subtree.

- **Threshold**: Zero cross-scope identity reuse; zero unaudited residual-spool operations;
  zero query-scope leakage in isolation tests.
- **Rationale**: The estate spans trust boundaries (customer-owned sites, partner tiers,
  compliance regimes) by definition; scope containment is the core security property.

## 7. Public Library Interfaces

#### Estate Management API

- [ ] `p1` - **ID**: `cpt-cf-location-manager-interface-estate-api`

- **Type**: Service API consumed by embedding platforms (topology CRUD, registration tokens,
  agent lifecycle operations, fleet health, scoped queries)
- **Stability**: stable (business-level contract; technical contract owned by DESIGN)
- **Breaking Change Policy**: Major version bump for any incompatible change to resource
  shapes, scope semantics, or lifecycle state machines; additive changes do not require one.

#### Placement API

- [ ] `p1` - **ID**: `cpt-cf-location-manager-interface-placement-api`

- **Type**: Service API for constraint-set resolution and placement decision records
  (resolve, retrieve by ID, revalidate, release)
- **Stability**: stable; the tie-break algorithm of
  `cpt-cf-location-manager-fr-constraint-placement` is part of the contract — changing any
  rung is a breaking change
- **Breaking Change Policy**: Major version bump for any change to resolution semantics,
  decision-record fields, or expiry/invalidation behavior.

#### Agent Sync & Spool Contract

- [ ] `p1` - **ID**: `cpt-cf-location-manager-interface-agent-contract`

- **Type**: Hub↔agent protocol contract (pull sync, heartbeat, state delta with monotonic
  sequence, spool flush with record IDs, compatibility handshake)
- **Stability**: versioned with the N−1 compatibility window enforced at handshake
  (`cpt-cf-location-manager-fr-agent-lifecycle`)
- **Breaking Change Policy**: A protocol change that would exclude N−1 agents MUST ship behind
  a hub release that still accepts N−1, with the cohort completed or abandoned before
  advancing.

## 8. Use Cases

#### Register a Cluster into a Region

- [ ] `p1` - **ID**: `cpt-cf-location-manager-usecase-register-cluster`

**Actor**: `cpt-cf-location-manager-actor-scope-admin`, `cpt-cf-location-manager-actor-fleet-agent`

**Preconditions**: The target region/AZ exists; the administrator holds rights on that scope.

**Main Flow**:
1. Administrator requests a registration token for the target region/AZ scope
2. The token (time-limited, single-use) is delivered to the cluster; the agent presents it
3. The hub validates scope binding, issues the per-cluster mTLS identity, and attaches the
   cluster as a leaf of the topology
4. The agent begins pull-based sync and heartbeating; fleet events are emitted

**Postconditions**: The cluster is a typed topology node, scoped for placement, quota, and
configuration; the token is consumed.

**Alternative Flows**:
- **Token expired/reused**: registration is denied with a reason code; the token is revoked.
- **Brownfield attach**: after registration, discovery projects existing resources
  idempotently; ownership conflicts surface reason-coded for operator resolution
  (`cpt-cf-location-manager-fr-brownfield-attach`).

#### Survive a WAN Outage

- [ ] `p1` - **ID**: `cpt-cf-location-manager-usecase-wan-outage`

**Actor**: `cpt-cf-location-manager-actor-fleet-agent`, `cpt-cf-location-manager-actor-estate-operator`

**Preconditions**: A registered cluster loses hub connectivity beyond the configured
heartbeat timeout.

**Main Flow**:
1. The spoke enters autonomous mode automatically: local operation continues,
   centrally-initiated provisioning suspends, state and usage spool durably
2. The hub marks the cluster autonomous in the fleet health model; the duration alert fires
   when the configured threshold is exceeded
3. On reconnection the spoke exits autonomous mode, submits its sequenced state delta, and
   flushes the spool in emission order
4. The hub reconciles deterministically: hub-authoritative desired state, spoke-authoritative
   actual state; deletions during disconnection surface as drift, never resurrect

**Postconditions**: No lost usage records; reconverged state; an auditable autonomous episode.

**Alternative Flows**:
- **Spool watermark crossed**: alert raised; at capacity the configured policy applies
  (backpressure or oldest-first drop with per-subject counts reported on reconnection).
- **Repeated delta replay**: identical outcome (idempotent reconvergence).

#### Resolve a Constraint-Based Placement

- [ ] `p1` - **ID**: `cpt-cf-location-manager-usecase-placement`

**Actor**: `cpt-cf-location-manager-actor-placement-consumer`

**Preconditions**: Topology populated; the constraint vocabulary governs the request's keys.

**Main Flow**:
1. Consumer submits a constraint set (e.g., region + compliance + `az_affinity: spread`)
2. The resolver intersects hard constraints over the admission-time snapshot, applies the
   normative tie-break rungs, and consults Quota Enforcement race-free for the scope
3. The winner is returned with the full per-rung audit record

**Postconditions**: A reproducible, audited placement; the quota lease committed.

**Alternative Flows**:
- **Unsatisfiable**: denial with reason codes identifying the unsatisfied constraint(s).
- **Quota exhausted**: denial identifying the exhausted scope and current usage.
- **Unregistered vocabulary**: rejection with the distinct vocabulary reason code.

#### Bind an Order to a Placement Decision

- [ ] `p2` - **ID**: `cpt-cf-location-manager-usecase-async-binding`

**Actor**: `cpt-cf-location-manager-actor-async-consumer`

**Preconditions**: A successful resolution produced a placement decision record with a
validity period; the quota lease is held reserved.

**Main Flow**:
1. The consumer binds its order/subscription to the decision ID
2. Fulfillment starts within the validity period and presents the decision ID
3. The system honors exactly the recorded cluster — never a silent substitute

**Postconditions**: The commercial commitment and the technical placement reference the same
immutable record.

**Alternative Flows**:
- **Expiry before fulfillment**: lease released, decision marked expired; presenting it is
  denied with a revalidation reason code; revalidation produces a fresh decision (new record,
  current snapshot), reason-coded if the outcome differs.
- **Availability change during validity**: decision marked invalidated; change signal emitted
  so the consumer learns before fulfillment.

#### Roll a Fleet Upgrade

- [ ] `p2` - **ID**: `cpt-cf-location-manager-usecase-fleet-upgrade`

**Actor**: `cpt-cf-location-manager-actor-estate-operator`

**Preconditions**: A new agent release (N) is available; the fleet runs N−1.

**Main Flow**:
1. Operator starts a rolling upgrade scoped region→AZ→cluster
2. Each upgraded agent passes post-upgrade health checks; failures roll back automatically
   with an emitted rollback event
3. The cohort completes (or is explicitly abandoned) before the hub advances beyond N

**Postconditions**: Fleet on N; no agent stranded below the compatibility window.

**Alternative Flows**:
- **Stale agent reconnects below the window**: visibly quarantined with a reason code,
  hub-upgradable in place.

## 9. Acceptance Criteria

Each criterion validates the referenced requirement; the full normative statement lives with
that requirement.

- [ ] Four-level topology (geography → region → AZ → cluster) with typed, addressable nodes usable as placement/quota/configuration scopes (`cpt-cf-location-manager-fr-geo-topology`)
- [ ] Region compliance attributes mutable only by the estate owner; delegated tiers manage AZs/clusters within owner-defined regions (`cpt-cf-location-manager-fr-scoped-topology-admin`)
- [ ] Placement constraints validated against the governed vocabulary; unregistered vocabulary rejected with a distinct reason code; vocabulary runtime-extensible via approval workflow (`cpt-cf-location-manager-fr-constraint-vocabulary-registry`)
- [ ] Registration via scope-bound, time-limited, single-use tokens yielding scope-bound per-cluster mTLS identities; unused tokens revoked immediately (`cpt-cf-location-manager-fr-agent-registration`)
- [ ] Deregistration drains before revoking; residual spools survive in a documented exportable format, alerted with cluster/location/count, recoverable or explicitly discarded under audited recovery-owner authority — never silently discarded (`cpt-cf-location-manager-fr-agent-registration`)
- [ ] Brownfield attach without workload disruption; idempotent discovery keyed by immutable provider-native identifiers; ownership conflicts surface reason-coded, never silently re-parented (`cpt-cf-location-manager-fr-brownfield-attach`)
- [ ] Hub-orchestrated rolling upgrades with N−1 window, auto-rollback on failed health checks (event emitted), certificate auto-renewal without re-registration, idempotent clean uninstall, handshake-enforced compatibility with visible quarantine (`cpt-cf-location-manager-fr-agent-lifecycle`)
- [ ] Agent parameters inherit most-specific-wins (cluster > region > estate) and propagate on next sync (`cpt-cf-location-manager-fr-agent-config-inheritance`)
- [ ] Desired state syncs hub→spoke by pull at a configurable interval; drift detected and reported (`cpt-cf-location-manager-fr-declarative-sync`)
- [ ] Autonomous mode enters/exits automatically; reconvergence deterministic and idempotent (sequenced deltas, no resurrection of hub-deleted resources); over-duration alert carries cluster, scope, entry time, elapsed duration, last sync (`cpt-cf-location-manager-fr-autonomous-mode`)
- [ ] Placement resolves constraint sets with the three normative tie-break rungs against an identified snapshot; the audit record reproduces the winner independently; unsatisfiable and out-of-residency requests denied with reason codes (`cpt-cf-location-manager-fr-constraint-placement`)
- [ ] Every successful resolution materializes an immutable, retrievable decision record with validity period and reserved lease; no silent relocation; expiry/invalidation explicit with change signals; revalidation is a fresh resolution (`cpt-cf-location-manager-fr-placement-decision-reference`)
- [ ] Geo-scoped quota admission is race-free via Quota Enforcement lease primitives; denials identify the exhausted scope and usage; no local counters (`cpt-cf-location-manager-fr-scoped-quota`)
- [ ] Usage records spool durably with immutable record IDs and full scope attributes; flush at-least-once in emission order; watermark alerts; capacity policy applied with per-subject drop counts reported — silent loss forbidden (`cpt-cf-location-manager-fr-usage-spool`)
- [ ] Fleet health rolls up cluster→AZ→region with autonomous marking and per-cluster drill-down; events via Event Broker (`cpt-cf-location-manager-fr-fleet-observability`)
- [ ] Estate queries filter by any scope combination and never return or imply rows outside the caller's authorized subtree (`cpt-cf-location-manager-fr-scoped-query`)
- [ ] Artifacts syndicate to scope-matched clusters over the sync channel with scope-filtered visibility (`cpt-cf-location-manager-fr-artifact-syndication`)
- [ ] Agent footprint within documented bounds; no silent loss; reproducible placement; scale and security baselines held (`cpt-cf-location-manager-nfr-agent-footprint`, `cpt-cf-location-manager-nfr-reliability`, `cpt-cf-location-manager-nfr-placement-determinism`, `cpt-cf-location-manager-nfr-scale`, `cpt-cf-location-manager-nfr-security`)

## 10. Quality Vectors Analysis

A cross-cutting quality summary over five vectors; each show-stopper traces to the
requirements above.

| Quality Vector | Show-Stopper Requirements | Rationale |
|----------------|---------------------------|-----------|
| **Efficiency** | Pull-based sync (no hub fan-out), scoped agent-config inheritance instead of per-cluster hand-tuning, agent footprint bounds (`fr-declarative-sync`, `fr-agent-config-inheritance`, `nfr-agent-footprint`) | An estate control plane that taxes its spokes or requires O(cluster) manual work defeats its own purpose |
| **Reliability** | Autonomous mode with deterministic idempotent reconvergence; drain-then-revoke; spool watermarks with forbidden silent loss (`fr-autonomous-mode`, `fr-agent-registration`, `fr-usage-spool`, `nfr-reliability`) | WAN outage is the normal case to design for, not the exception |
| **Performance** | Race-free lease-based quota admission; snapshot-scoped resolution; scale reference targets (`fr-scoped-quota`, `fr-constraint-placement`, `nfr-scale`) | Placement sits on consumers' provisioning paths; contention and drift must not degrade it |
| **Security** | Scope-bound single-use tokens and mTLS identities; sovereignty-preserving delegation; audited residual-spool recovery; subtree-confined queries (`fr-agent-registration`, `fr-scoped-topology-admin`, `fr-scoped-query`, `nfr-security`) | The estate crosses trust boundaries by definition — customer sites, partner tiers, compliance regimes |
| **Versatility** | Governed, runtime-extensible constraint vocabulary; adopter-bound semantics kept out of the core; composition seams to sibling gears (`fr-constraint-vocabulary-registry`, §1.5, §4.2) | The gear is a commons: every adopting platform binds its own node types, policies, tiers, and billing on top |

*(Requirement IDs abbreviated; full form `cpt-cf-location-manager-…`.)*

## 11. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| `gears/system/quota-enforcement` | Geo-scope subject declarations; idempotent debit/lease admission calls | p1 |
| `gears/system/usage-collector` | Flush target for spoke usage spools; downstream deduplication by record ID | p1 |
| `gears/system/event-broker` | Fleet lifecycle/alert events; decision invalidation change signals | p1 |
| `gears/system/types-registry` | Candidate substrate for the governed constraint-vocabulary registry (§14) | p2 |
| `gears/system/cluster` | Optional internal coordination for hub instances | p3 |
| Certificate/PKI facility | Issuance, renewal, and revocation of per-cluster mTLS identities (design decides composition) | p1 |

## 12. Assumptions

- The embedding platform provides the authorization source that the delegation model
  (`cpt-cf-location-manager-fr-scoped-topology-admin`) and scoped queries consume; this gear
  enforces scope, it does not define identity.
- WAN links between hub and spokes are unreliable by assumption; agent-initiated pull
  connectivity is viable through NAT/firewall asymmetry.
- Downstream usage consumers deduplicate on record IDs (at-least-once delivery is sufficient).
- Quota Enforcement exposes idempotent debit/lease primitives suitable for placement-keyed
  admission.

## 13. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Estate snapshot staleness vs. placement validity | Decisions made against outdated capacity | Snapshot identity in every decision; explicit validity periods; availability-change invalidation signals; revalidation as fresh resolution |
| Spool growth during long outages | Capacity exhaustion at the spoke | Watermark alerts; configurable backpressure/oldest-first-drop with per-subject accounting; capacity sized for realistic outage windows (design) |
| N−1 cohort discipline erodes | Fleet fragments across incompatible versions | Handshake-enforced matrix; cohort completion/abandonment gate before the hub advances; visible quarantine, never silent rejection |
| Vocabulary governance bypassed by adopters | Estate fragmentation, placement guesswork | Ad-hoc vocabulary rejected with a distinct reason code; the approval workflow is the only extension path |
| Residual spool mishandling after forced revocation | Billable record loss or unaudited exfiltration | Documented exportable format; recovery-owner-only audited export/discard; same protection as the live spool |
| Boundary drift vs. IRM's deployment-scope orchestration | Two gears claiming placement/discovery surfaces | Boundary stated in §1.5; composition seam recorded as an open question (§14) to settle before designs harden |

## 14. Open Questions

- **Constraint-vocabulary registry substrate**: is `gears/system/types-registry` the home for
  the governed vocabulary (registered value sets, approval workflow), or does this gear carry
  its own registry? — owner: this gear's DESIGN together with the Types Registry owners —
  target: before DESIGN freeze.
- **IRM composition seam**: how does a Location Manager placement decision hand off to
  Infrastructure Resource Manager deployments (IRM's scope decision keeps a later placement
  dimension open)? — owner: both gears' owners — target: before either DESIGN hardens
  placement-adjacent surfaces.
- **PKI composition**: which platform facility issues and revokes per-cluster mTLS identities
  (a credstore-backed CA, an external PKI, or gear-local)? — owner: DESIGN — target: before
  implementation.
- **Decision-record retention**: how long are expired/invalidated placement decision records
  retained for audit, and where do they archive? — owner: DESIGN — target: before GA.
- **Hub HA profile ownership**: deployment profiles are out of scope here (§4.2); confirm
  which repository artifact owns them for reference deployments. — owner: platform
  architecture docs — target: before GA.

## 15. Traceability

- **Design**: [DESIGN.md](./DESIGN.md) — TBD, not yet authored for this gear
- **ADRs**: [ADR/](./ADR/) — TBD, not yet authored for this gear
- **Roadmap**: [#4324](https://github.com/constructorfabric/gears-rust/issues/4324)

### Intake ID migration

This PRD consolidates and supersedes the requirements intake (`UPSTREAM_REQS.md`, removed in
the same change). Requirement content was carried over 1:1; IDs migrated `upreq` → `fr`/`nfr`:

| Intake ID (`cpt-cf-location-manager-…`) | PRD ID (`cpt-cf-location-manager-…`) |
|---|---|
| `upreq-geo-topology` | `fr-geo-topology` |
| `upreq-scoped-topology-admin` | `fr-scoped-topology-admin` |
| `upreq-agent-registration` | `fr-agent-registration` |
| `upreq-brownfield-attach` | `fr-brownfield-attach` |
| `upreq-declarative-sync` | `fr-declarative-sync` |
| `upreq-autonomous-mode` | `fr-autonomous-mode` |
| `upreq-agent-config-inheritance` | `fr-agent-config-inheritance` |
| `upreq-agent-lifecycle` | `fr-agent-lifecycle` |
| `upreq-constraint-placement` | `fr-constraint-placement` |
| `upreq-placement-decision-reference` | `fr-placement-decision-reference` |
| `upreq-constraint-vocabulary-registry` | `fr-constraint-vocabulary-registry` |
| `upreq-scoped-quota` | `fr-scoped-quota` |
| `upreq-usage-spool` | `fr-usage-spool` |
| `upreq-artifact-syndication` | `fr-artifact-syndication` |
| `upreq-fleet-observability` | `fr-fleet-observability` |
| `upreq-scoped-query` | `fr-scoped-query` |
| `upreq-agent-footprint` | `nfr-agent-footprint` |

### Provenance

All §5–§6 requirements are generalised from a production multi-region infrastructure control
plane contributed by an adopting platform, plus one requirement
(`cpt-cf-location-manager-fr-placement-decision-reference`) contributed during review by an
order/fulfillment consumer. They are stated self-contained: no external or non-public document
is required to build, review, or test this gear. The contributing platform maintains the
mapping from its own specification to the identifiers above (via the intake-ID migration
table), so upstream changes remain traceable in both directions.
