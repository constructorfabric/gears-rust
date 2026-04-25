---
status: accepted
date: 2026-05-07
---

# Coordination plugin — separate `CoordinationPluginV1` contract

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Default deployment shape](#default-deployment-shape)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Separate `CoordinationPluginV1` trait](#separate-coordinationpluginv1-trait)
  - [Bundle coordination methods into `QuotaEnforcementStoragePluginV1`](#bundle-coordination-methods-into-quotaenforcementstoragepluginv1)
  - [No in-process abstraction — external orchestration only](#no-in-process-abstraction--external-orchestration-only)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-quota-enforcement-adr-coordination-plugin`

## Context and Problem Statement

Three QE background tasks — `LeaseSweeper`, `RetentionSweeper`, `NotificationDispatcher` — must run as cluster-wide
singletons: only one replica may sweep expired leases / reclaim retention rows / drain the notification outbox at a
time, otherwise duplicate work and at-least-once-becomes-at-least-twice semantics emerge. The singleton property is not
just a performance hint: it underpins `cpt-cf-quota-enforcement-nfr-recovery` (RTO ≤ 15 min — a dead leader's slot must
be re-acquirable within at most one TTL by any survivor).

The natural realisation is a TTL-bounded distributed lock: every replica attempts to acquire a per-scope lock; the
winner runs the task, renews the lock periodically, and either releases it on graceful shutdown or lets it auto-expire
on crash. The mechanism behind that lock — Postgres advisory locks, MariaDB `GET_LOCK`, a lease-row +
`SELECT … FOR UPDATE`, etcd / Consul, Redis Redlock, Kubernetes Lease objects, ZooKeeper, custom in-memory for tests —
is operationally diverse and only weakly tied to the storage backend choice.

Question: where should this primitive live in the QE-core contract surface?

## Decision Drivers

- **Sweeper / dispatcher singleton coordination** for `LeaseSweeper`, `RetentionSweeper`, `NotificationDispatcher`.
- **NFR recovery (RTO ≤ 15 min)** — a crashed leader's lock MUST become acquirable within at most one TTL
  (`cpt-cf-quota-enforcement-nfr-recovery`).
- **Storage plugin contract minimality** — the storage trait already carries 13 invariants and ~20 methods; conflating
  coordination semantics with persistence semantics inflates an already large contract.
- **Independent operational evolution** — coordination backends evolve along a different axis than storage backends;
  operators deploying Postgres for storage may want Redis / etcd / k8s for coordination as the cluster grows.
- **Default-deployment ergonomics** — small / single-tenant deployments should not require a second ops dependency just
  to run sweepers.

## Considered Options

- **Separate `CoordinationPluginV1` trait** (chosen) — distinct plugin contract for TTL-bounded distributed locks,
  decoupled from `QuotaEnforcementStoragePluginV1`.
- **Bundle coordination methods into `QuotaEnforcementStoragePluginV1`** — add `acquire_lock` / `renew` / `release`
  directly on the storage trait, alongside data methods.
- **No in-process abstraction — external orchestration only** — defer singleton enforcement to an external orchestrator
  (Kubernetes Lease object, StatefulSet-with-replica=1, Nomad job constraint), with no QE-core trait at all.

## Decision Outcome

Chosen option: **Separate `CoordinationPluginV1` trait**, because it (a) keeps both plugin contracts narrow and focused,
(b) lets the coordination backend evolve independently of the storage backend, (c) makes coordination-backend choice an
operator-deployment decision rather than a QE-core or storage-plugin code change, and (d) does not force an external
orchestration dependency on small deployments.

The trait surface is intentionally minimal: three methods (`try_lock`, `renew`, `release`), a closed `LockScope` enum
(`LeaseSweeper` / `RetentionSweeper` / `NotificationDispatcher`), an opaque `Lock` holder token, and a closed
`CoordinationError` enum. TTL-bounded auto-release is an inviolable contract property — a lock MUST NOT outlive its TTL
even if the holder process crashes silently. Bootstrap-time reachability is validated via a `try_lock` + `release` probe
on each `LockScope::*` value (DESIGN §3.7); the contract has no separate health-check method. Full surface in DESIGN
§3.3 "Coordination Plugin Trait".

### Default deployment shape

The P1 default `quota-enforcement-coordination-plugin` impl piggybacks on the storage backend's own locking primitives
(Postgres advisory locks, MariaDB `GET_LOCK`, a `SELECT … FOR UPDATE` lease-row pattern, etc.), so default deployments
incur no additional ops dependency. Operators may swap to an independent coordination backend (etcd, Consul, Redis
Redlock, Kubernetes Lease, ZooKeeper) by replacing the plugin crate; QE-core and the storage plugin stay unchanged.

The contract is deliberately silent on transport, quorum semantics, or internal storage of lease state — those are
plugin-internal. QE-core requires only the outcome: TTL-bounded acquisition, renew-or-fail semantics, and crash-safe
auto-release.

### Consequences

- `quota-enforcement-coordination-plugin` ships as its own crate alongside `quota-enforcement-storage-plugin`. Both are
  consumed by sweepers / dispatcher through ClientHub.
- The trait travels in `quota-enforcement-sdk` so plugin authors implement against a single dependency (parallel to
  `QuotaEnforcementStoragePluginV1`, `QuotaResolutionEngineV1`, `QuotaNotificationSinkV1`).
- Bootstrap MUST run a `try_lock` + `release` reachability probe for each `LockScope::*` value before the module joins
  the platform readiness signal; failure aborts bootstrap fail-fast (DESIGN §3.7 bootstrap step).
- Holders MUST `renew` on or before TTL/3 of cycle elapsed; missing the renew window surfaces `LockExpired` and forces
  follower-mode fallback.
- Plugin contract is versioned with the module's major version per PRD §7.2 — the trait carries the matching `V<major>`
  suffix (`CoordinationPluginV1`).
- The default impl shares the storage backend's failure domain: if the storage database is unreachable, the default
  coordination plugin also fails the `try_lock` + `release` bootstrap probe. Operators who need decorrelated failure
  domains (e.g., coordination available while storage is degraded) deploy an independent coordination backend.

### Confirmation

Confirmed by: trait surface review against DESIGN §3.3 / §3.2 component model, bootstrap-time `try_lock` + `release`
probe coverage on each `LockScope::*` value (DESIGN §3.7), and a chaos drill that kills the active leader and verifies
the survivor acquires the lock within ≤ 1 TTL (RTO ≤ 15 min per `cpt-cf-quota-enforcement-nfr-recovery`).

## Pros and Cons of the Options

### Separate `CoordinationPluginV1` trait

- Good, because both plugin contracts (`StoragePluginV1`, `CoordinationPluginV1`) stay narrow and individually testable;
  ~4 coordination methods vs. ~20 storage methods.
- Good, because operators can choose a coordination backend independent of storage (Postgres storage + Redis
  coordination, MariaDB storage + k8s Lease coordination, …).
- Good, because the default impl can piggyback on the storage backend's native locks, so simple deployments incur zero
  additional ops dependency.
- Good, because failure-mode reasoning is local: a coordination backend outage silences sweepers (acceptable per
  `cpt-cf-quota-enforcement-fr-lease-timeout` lazy semantic release, I4) without affecting the gateway hot path.
- Bad, because two plugin crates instead of one increase the project surface (deployment manifest, dependency graph,
  bootstrap probes).
- Bad, because operators using the default impl must understand the failure-domain coupling between storage and
  coordination (same backend = correlated failure).

### Bundle coordination methods into `QuotaEnforcementStoragePluginV1`

- Good, because a single plugin crate is conceptually simpler for first-time readers.
- Good, because the default impl (storage-piggyback) is already coupled to the storage backend, so the bundling matches
  that reality directly.
- Bad, because operators who want a distinct coordination backend (Redis / etcd / k8s) would have to fork the storage
  plugin or layer adapters — coordination becomes hostage to storage choice.
- Bad, because the storage trait already encodes 13 invariants and ~20 methods; adding lock semantics inflates an
  already heavy contract and complicates plugin authoring.
- Bad, because versioning becomes coupled — a coordination-only protocol change forces a storage-plugin-V`N+1` bump (and
  vice versa).

### No in-process abstraction — external orchestration only

- Good, because zero QE-side code for coordination; rely on Kubernetes Lease, StatefulSet-with-replica=1, Nomad job
  constraints, or similar.
- Bad, because it imposes an external-orchestrator dependency on every deployment, including small / dev / edge ones;
  SQLite-backed single-process deployments would need a synthetic orchestrator.
- Bad, because in-process awareness of leader / follower state (used for observability — per-`LockScope` sweeper state
  surfacing in `module-status` / module readiness signal) becomes harder to plumb when there is no in-process
  abstraction.
- Bad, because the bootstrap probe (`try_lock` + `release` per `LockScope::*`, per DESIGN §3.7) cannot validate
  external-orchestrator readiness uniformly across deployment shapes.

## More Information

- DESIGN §3.2 "Component model" — `CoordinationPlugin` component definition.
- DESIGN §3.3 "Coordination Plugin Trait" — full trait surface, domain types, and semantic guarantees.
- DESIGN §3.6 "Sequences" — sweeper / dispatcher acquire / renew / fallback flows.
- Sibling ADR `cpt-cf-quota-enforcement-adr-storage-backend` — defines the pluggable storage contract; the default
  coordination impl piggybacks on the deployed storage plugin's locking primitives (whichever backend the plugin uses).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

- `cpt-cf-quota-enforcement-fr-pluggable-storage` — keeps coordination orthogonal to the storage-pluggable contract;
  backends evolve independently.
- `cpt-cf-quota-enforcement-nfr-recovery` — TTL-bounded auto-release lets a survivor acquire a crashed leader's lock
  within at most one TTL.
- `cpt-cf-quota-enforcement-fr-lease-timeout` — `LeaseSweeper` consumes the coordination contract for singleton
  execution.
- `cpt-cf-quota-enforcement-fr-notification-plugin` — `NotificationDispatcher` consumes the coordination contract for
  singleton outbox draining.
- Sibling ADR `cpt-cf-quota-enforcement-adr-storage-backend` — the default coordination impl piggybacks on the storage
  backend's locking primitives.
