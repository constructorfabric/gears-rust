---
status: proposed
date: 2026-10-12
decision-makers: "BSS Rating team"
---

Created:  2026-10-12 by Virtuozzo International GmbH
Updated:  2026-10-12 by Virtuozzo International GmbH

# ADR-0008: Coordinate Instances through the Cluster SDK, with Database Writes as the Fence

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Cluster facades with database conditional writes (chosen)](#cluster-facades-with-database-conditional-writes-chosen)
  - [Store and compare a lease fence in Rating tables](#store-and-compare-a-lease-fence-in-rating-tables)
  - [Use bss-coord](#use-bss-coord)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-rating-adr-coordination`

## Context and Problem Statement

Rating runs several instances. Some work must have one active worker: the reader of each usage feed
source, the window scheduler and wake scanner per shard, the recovery sweeps, the reconciler and a
few periodic jobs. The platform assigns cross-instance coordination to the Cluster Plane
([`docs/GEARS.md`](../../../../../docs/GEARS.md)). The Cluster SDK offers distributed locks
(`DistributedLockApi`, facade `DistributedLockV1`) and leader election (`LeaderElectionApi`, facade
`LeaderElectionV1`), resolved through a typed `ClusterProfile` marker.

Two Cluster ADRs constrain how a consumer may rely on them:

- [Cluster ADR-002](../../../../system/cluster/docs/ADR/002-async-boundary-no-remote-in-critical-section.md)
  exposes no fencing token and forbids non-cluster remote I/O (database writes, HTTP to other
  services) inside a lock's critical section. Locks are advisory; a conditional write is the
  correctness gate.
- [Cluster ADR-012](../../../../system/cluster/docs/ADR/012-store-owned-leases.md) keeps a fence in
  each store-owned lease record, but guarantees only that a fence value is not reused for one lease
  name within `fence_retention` of a lapse. It is not global monotonicity, and it is not exposed
  ([`lease.rs`](../../../../system/cluster/cluster-sdk/src/lease.rs)).

An earlier Rating design stored "the Cluster lease fence of the last writer" in
`bss_rating__source_checkpoint` and required `fence >= stored fence` on every page commit. BSS
documents also use `bss-coord` (`LeaseManager`). Whether the BSS deployment profile binds a Cluster
backend is not decided (R-27).

How does Rating coordinate its instances, and what makes its writes correct when coordination
fails?

## Decision Drivers

* Use the platform's coordination owner; do not fork it.
* Correctness must survive a paused holder, a partition and a handover, which no lease prevents.
* Follow Cluster ADR-002 and ADR-012 as written.
* Coordination names must pass the Cluster SDK's name rules in every profile.

## Considered Options

1. Cluster facades through a `ClusterProfile`, advisory, with database conditional writes as the
   fence (chosen).
2. Store the Cluster lease fence in Rating tables and compare it on every write.
3. Use `bss-coord` as Rating's coordination mechanism.

## Decision Outcome

Chosen option: **Cluster facades, advisory, with database conditional writes as the fence**
(DECISIONS R-27; DESIGN §3.5).

- **Profile.** Rating declares one `ClusterProfile` marker and resolves its facades through it. Every
  facade is scoped with `.scoped("bss-rating")`.
- **Leadership.** The `LeaderElectionV1` facade (`elect`, after `.scoped("bss-rating")`) elects the single active reader of each feed source and the
  leader of each scheduler and wake-scanner shard; the recovery sweeps and the reconciler and the replay
  verifier use it per shard too, and so do the periodic jobs (exception sweep, retention purge), because they write the
  database. No Rating job runs under a lock. The `DistributedLockV1` facade is used only where the critical section makes cluster calls alone;
  Rating has no such section at launch.
- **Advisory only.** A lease or a leadership saves duplicate work and nothing more. Correctness comes
  from database conditional writes: the checkpoint cursor CAS, the `applied_seq` CAS, the
  input-generation CAS, `SKIP LOCKED` claims, and unique keys such as `(tenant_id, usage_record_id)`,
  `child_id` and `(fact_id, result_revision)`. If two instances process the same page, one CAS wins
  and the other rolls back. No lease fence is stored or compared; the checkpoint has no `fence`
  column.
- **No remote I/O under a guard.** No non-cluster remote I/O runs inside a lock guard's critical
  section: no database transaction and no HTTP call to another gear (feed reads, Pricing resolves).
  Bounded cluster round trips are allowed (Cluster ADR-002 amendment). A feed page is fetched before
  its database transaction opens, and the transaction contains only database statements.
- **Names.** Leaf names match `[A-Za-z0-9_-]{1,255}`: `usage-feed-<uuid>` (the UUID of the canonical
  source, without the `usage:` prefix of `source_id`, since `:` is not allowed), `scheduler-<shard>`,
  `recovery-<shard>`, `reconcile-<shard>`, `verify-<shard>`, `exception-sweep`, `purge`. Composed names read
  `bss-rating/usage-feed-<uuid>`.
- **Losing a lease.** A worker that loses leadership stops after its current transaction; a zombie
  write fails its CAS.
- **Transitional adapter.** `bss-coord` is used only as a transitional adapter behind Rating's
  `LeaseProvider` port, and only if the Cluster profile is not deployed in time. It needs
  Platform/Cluster and BSS approval, an owner and a removal milestone (R-27), and it keeps the same
  advisory semantics.

### Consequences

* During a handover two instances may do the same work for up to one lease TTL; the database
  absorbs it.
* Rating's correctness can be tested without any Cluster backend.
* The first deployment needs a Cluster leader-election backend bound in the BSS profile (R-27).
* DESIGN rows that name a stored fence or a lock per feed source follow this ADR.

### Confirmation

* Two readers on one source with no coordination at all produce exactly one effect per entry and
  one checkpoint advance per page.
* A reader that loses leadership in the middle of a page rolls back on the CAS.
* Every coordination name passes the SDK's scoped-name validation (`validate_scoped_cluster_name`).
* Review and the Cluster lint (when re-scoped) find no database or HTTP call inside a lock guard.
* Schema review: no Rating table stores a lease fence.

## Pros and Cons of the Options

### Cluster facades with database conditional writes (chosen)

* Good: uses the platform owner; correct under any pause or partition; follows Cluster ADR-002 and
  ADR-012; profile-transparent.
* Bad: depends on the BSS profile binding a backend (R-27); duplicate work during handovers.

### Store and compare a lease fence in Rating tables

* Good: rejects some stale writers before their CAS.
* Bad: unsound and unavailable. The ADR-012 fence is unique per lease name only within
  `fence_retention` and may restart after it, so `fence >= stored fence` could reject a legitimate
  new holder or accept a stale one; it is also not exposed to consumers (ADR-002, ADR-012). The CAS
  already rejects every stale writer.

### Use bss-coord

* Good: exists and is used by other BSS documents; `with_ack_in_tx` couples a lease to a
  transaction.
* Bad: a second coordination mechanism beside the platform's Cluster Plane. Acceptable only as a
  transitional adapter with an owner and a removal milestone.

## More Information

- Cluster SDK contract traits:
  [`contract.rs`](../../../../system/cluster/cluster-sdk/src/contract.rs); profile and name rules:
  [`profile.rs`](../../../../system/cluster/cluster-sdk/src/profile.rs).
- Topology and concurrency: [DESIGN §3.8](../DESIGN.md#38-deployment-topology),
  [DESIGN §4.5](../DESIGN.md#45-concurrency).

## Traceability

- **Decisions**: [`../DECISIONS.md`](../DECISIONS.md) R-27 (this decision's open row).
- **Upstream**: [`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) §2.12 (A-4 `bss-coord`, A-7
  `cluster-sdk`, J-16).
- **Design**: [DESIGN §3.5](../DESIGN.md#35-external-dependencies),
  [DESIGN §3.8](../DESIGN.md#38-deployment-topology),
  [DESIGN §4.2](../DESIGN.md#42-transactions-idempotency-delivery-semantics),
  [DESIGN §4.5](../DESIGN.md#45-concurrency).
- **Cluster**: ADR-002 (no remote I/O in a critical section), ADR-012 (store-owned leases).
