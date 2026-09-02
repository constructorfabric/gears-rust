---
status: accepted
date: 2026-08-12
---

# ADR-012: Store-Owned Leases and Handle Lifetime Across the Boundary

**ID**: `cpt-cf-clst-adr-store-owned-leases`

> Recorded with item `L2` (the Postgres liveness-beacon removal), which is the first change that could not be made without settling this. The model itself landed in `L1`; the decision predates both, in [DESIGN.md](../DESIGN.md) §3.19.

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [The model](#the-model)
  - [The fence, and exactly what it guarantees](#the-fence-and-exactly-what-it-guarantees)
  - [Release deletes the record: settling a contradiction in the design](#release-deletes-the-record-settling-a-contradiction-in-the-design)
  - [Why the fence is not exposed](#why-the-fence-is-not-exposed)
  - [Two costs, stated plainly](#two-costs-stated-plainly)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Session-owned leases — the broker vouches for the lease](#option-1-session-owned-leases--the-broker-vouches-for-the-lease)
  - [Option 2: Keep the liveness beacon for in-process acquisitions only](#option-2-keep-the-liveness-beacon-for-in-process-acquisitions-only)
  - [Option 3: Store-owned leases, uniformly (CHOSEN)](#option-3-store-owned-leases-uniformly-chosen)
- [More Information](#more-information)

<!-- /toc -->

## Context and Problem Statement

The cluster gear becomes deployable out-of-process, so a lock or a leader claim may be acquired by a consumer through one cluster replica and renewed through another — or through the *same* replica after it has been restarted, upgraded or rescheduled.

Every mechanism cluster shipped before this assumed the opposite. The cache-backed default backends held a holder marker whose *physical* cache TTL was the lease, so the entry vanished at expiry. The native Postgres lock went further: it carried a **liveness beacon**, one per-incarnation advisory lock on a dedicated connection, stamped onto every `cluster_lock` row that instance wrote. The acquire predicate joined it against `pg_locks`, so a row whose beacon was no longer granted became stealable the instant Postgres noticed the holder's connection had died — sub-TTL reclaim of a crashed holder's lock, for free, with nothing having to notice on the dead holder's behalf.

That is sound precisely when **the process holding the beacon is the process using the lock**. Brokered, it inverts: the cluster gear's beacon would vouch for locks held by other, live consumers, so the gear's own restart would revoke the fleet's locks. A rolling upgrade of the cluster deployment would be a fleet-wide coordination event.

So the question this ADR settles is: where does a lease live, and what — if anything — vouches for it?

## Decision Drivers

- **A cluster replica must be replaceable** without revoking the fleet's locks (invariant I7). This is the requirement that rules out session ownership outright, not a preference.
- **Coordination must scale past one process.** Holding lease state in the replica that issued it makes every second replica a correctness hazard, so `replicaCount > 1` would be permanently unavailable.
- **Profile transparency (Goal 2, invariant I1).** One consumer source file must behave identically in Profile 1 (embedded) and Profile 3 (deployed). Lease expiry is not a defensible place to make an exception.
- **The plugin-facing `*Backend` traits stay stable** (invariant I11). Whatever this costs, it cannot be a breaking change to every plugin.
- **Renewal must remain the consumer-liveness proxy** (invariant I8). A wedged holder must still lose its claim.

## Considered Options

1. **Session-owned leases** — the serving replica vouches for the lease, as the beacon did for the acquiring process.
2. **Keep the liveness beacon for in-process acquisitions only**, and use store-owned leases for brokered ones.
3. **Store-owned leases, uniformly** — a lease is a fenced record in the backing store, and nothing vouches for it.

## Decision Outcome

**Chosen: option 3, store-owned leases, uniformly.** A lease is a record in the backing store, fenced by a token the client presents; no server-side session is required to interpret it, and no process's death — holder's or broker's — ends another's lease.

This is the shape the industry converged on: etcd holds a lease ID in the raft log and any member serves a renew; Consul holds a session ID in its own state and any server serves it; Kubernetes holds a `Lease` object with `holderIdentity` + `renewTime` and any apiserver replica serves it.

### The model

Every lease-bearing operation becomes a **conditional write predicated on state the store already holds**, so the replica handling the request needs no memory of the one that issued it.

| Element | Definition |
|---|---|
| **Lease record** | `owner` (the caller's `ClientId`), `deadline` (absolute, server clock), `fence` (per lease name, monotonic within the retention window below) |
| **Lease token** | What the client holds and presents: `(name, owner, fence)`. **It is the whole of the authority** — there is no lookup table behind it |
| `renew(token, ttl)` | One conditional write on `(name, owner, fence, deadline > now)`. Zero rows ⇒ `LockExpired`, and that answer is identical on every replica because it is a property of the record |
| `release(token)` | The same predicate, deleting. Absence ⇒ `Ok` (idempotent by absence) |
| `acquire(name, ttl)` | Insert-or-steal-if-lapsed, bumping `fence` |
| **Liveness** | The stored `deadline`, and nothing else. A holder that stops renewing lapses; nothing has to notice it is gone |

The token is **token-only**: `renew(&token, ttl)`, `release(&token)`. No caller identity is threaded alongside it, following §3.19.1's normative table rather than §4.4's sketch. Cross-checking that the *transport* caller is `token.owner` is the serving gear's authorization decision (§3.17.5), not the backend's predicate — the backend will not do it.

Two implementations, one algebra. The cache-backed defaults encode the record into an opaque cache value and CAS it on `CacheEntry::version` (`cluster/src/defaults/lease.rs`); the native Postgres lock holds the same three fields in columns and lets a guarded upsert be the CAS (`postgres-cluster-plugin` DESIGN §5.1). The four lease methods are **defaulted and dyn-safe** on the plugin-facing traits, so a backend that has not implemented them compiles and reports `Unsupported { feature: "store-owned-leases" }` — invariant I11 kept.

### The fence, and exactly what it guarantees

The fence is what makes "steal on expiry" safe rather than merely detectable: the previous holder's operations fail their predicate instead of silently succeeding against a lease someone else now owns. It must therefore not restart while any stale token bearing the old value could still be presented.

It does not come for free. `CacheEntry::version` is monotonic per key *while the key exists*, and a TTL reap deletes the key — the standalone plugin then writes `version: 1` on the next insert. A lease that lapsed, was reaped, and was re-acquired by the **same** owner would hand the old token a matching predicate again. So:

- **The fence lives in the value (or the column), not in the store's version.** `version`/`xmin` still drives the CAS; authority is the `fence` field.
- **Acquisition preserves and increments.** Taking a lapsed lease reads the existing record and writes `fence + 1`; the record is CAS'd, not deleted-then-inserted, so the counter survives the change of owner. In Postgres this is `fence = cluster_lock.fence + 1` inside the acquire's own `ON CONFLICT DO UPDATE`, read off the row rather than bound by the acquirer, so two racing stealers cannot land on the same fence.
- **The record outlives the lease.** Its physical expiry is `deadline + fence_retention`, not `deadline`.

**The guarantee, stated exactly: a fence value is never reused for a given lease name within `fence_retention` of its lease lapsing.** Not global monotonicity. Beyond that window the counter may restart, and reuse then *additionally* requires the same `ClientId` to re-acquire while holding a token that has been stale for longer than the retention window — a holder whose renew cadence is a fraction of its TTL learns it lost the lease orders of magnitude sooner.

`fence_retention` defaults to an hour (`FENCE_RETENTION_DEFAULT`) and is operator-configurable in two places, deliberately: `gears.cluster.config.fence_retention` governs the cache-backed defaults, whose fence lives in a cache value the wiring crate writes, and the Postgres lock binding's own `fence_retention_ms` governs the fence that lives in its columns. The alternative — injecting one gear-level key into every provider's option map — would add a key to the plugin config contract that any provider's `deny_unknown_fields` would then reject. The two cannot disagree in a way that matters, because a lease name lives in exactly one backend and the guarantee is stated per lease name.

**Where the guarantee holds, as of item `L3`:**

| | Mechanism | Cost when raised |
|---|---|---|
| Cache-backed defaults | The record's physical TTL is `deadline + fence_retention`, so **no reaper needed teaching**: both plugins sweep on the expiry they were given, which makes "skip a record inside the window" structurally true rather than a rule a reaper could get wrong | One cache entry per lease *name* for the window |
| Native Postgres lock | The reaper's predicate is `expires_at <= now() - fence_retention` while acquire's is still `expires_at <= now()`, so a lapsed row is stolen **in place** at `fence + 1` and only deleted a window later. No schema change: `now() - interval` is a runtime constant, so the sweep is the same indexed range scan | One row per lock *name* for the window |

**What `L3` does not close, and the exit criterion it could not meet as written.**

- **`release` still deletes**, in both implementations, so an explicit release drops the fence with the record and the next acquisition of that name starts at 1. The window covers a lease that **lapsed**, which is the case a stale holder can actually be in — a holder that released knows it did. This is not a gap `L3` left; it is the ruling the next section takes, now implemented on both paths.
- **"Reject a `fence_retention` shorter than the longest configured lease TTL" is unimplementable as stated**: there is no configured lease TTL anywhere in the tree to compare against. A TTL is a per-call argument to `lock(name, ttl)` and to an election claim, so the longest one in use is not knowable until it is used. The check is split instead — **zero is rejected at startup** in both config shapes (a zero window is not a short retention, it is the absence of one), and a lease taken with `ttl >= retention` **warns once per backend**, naming both durations. That is the real form of the rule, checked against a TTL that exists rather than one that was configured.

### Release deletes the record: settling a contradiction in the design

§3.19.1 states the guarantee in terms of a lease **ending**, while its own normative table specifies `release(token)` as a `DELETE` — which drops the fence immediately, inside the retention window. The two sentences are in tension and both are load-bearing, so this ADR picks one.

**Decision: `release` deletes, and the guarantee narrows to lapsing.** The alternative — making release a mark-expired CAS so the record survives to its retention deadline — was rejected:

- **The hazard it would close is not a mutual-exclusion break.** A voluntary release only drops the fence inside the retention window when the same owner re-acquires while its earlier guard task is still alive, in which case both tokens name that owner's own live lease (the analysis above).
- **It would make release non-idempotent-by-absence, or force a second concept.** "Absence ⇒ `Ok`" is the contract (§3.20.8); a mark-expired release has to distinguish "no record" from "a record I marked" to stay idempotent, and every reader then has to treat a marked record as vacant — a second liveness notion beside the deadline, which is exactly what this ADR removes elsewhere.
- **It would leave every released lease name resident for an hour.** A release is the *common* path, so the retained-record cost stops being "one small record per lease name" and becomes one per release, which is the sizing argument §3.19.1 rests on.

So the guarantee is scoped to lapsing, deliberately, and §3.19.1's prose should be corrected to say so rather than the table changed.

### Why the fence is not exposed

Exposing it as `LockGuard::fence()` for external fencing — passing a monotonic token to a third-party resource so it can reject stale writers — would promise **global** monotonicity for the lifetime of the protected resource, which no backend here can honour across the retention window. The fence is scoped to making cluster's own lease predicates safe.

`LockGuard` cannot carry one anyway: its fields are private and its only constructor is `LockGuard::channel`, so the token lives in the guard task's closure. That is *why* the trait needs a token-returning half at all — a remote caller, or any caller that must renew from somewhere other than the acquiring task, needs the token itself.

External fencing, if a consumer ever needs it, is an additive `LockGuard` method backed by a source that can actually promise it (a Postgres sequence, say), and it needs its own ADR.

### Two costs, stated plainly

**1. A crashed holder's lock lingers until its TTL, in every profile.** This is the price of retiring the Postgres liveness beacon, and it is a real capability removed from code that shipped three weeks before this ADR (PR #4411). Where the beacon returned a dead holder's locks in milliseconds — bounded by how fast Postgres noticed the connection was gone — reclamation is now bounded only by the lease TTL the holder chose.

The honest statement of crash recovery changes from *"immediate on clean disconnect, keepalive-bounded otherwise, TTL-bounded in the worst case"* to simply **TTL-bounded, always**. There is no knob: the TTL is a per-acquisition parameter on the trait, so recovery promptness is under caller control rather than operator control. **Keep lock TTLs tight.** A consumer whose critical section is short should not ask for a long lease.

Three mechanisms went with the beacon, and each cost is recorded where it lands rather than aggregated here: the sub-TTL reclaim above; the **shutdown drain**, whose removal is the point rather than a consequence (deleting a stopping instance's rows *is* the revocation this ADR exists to prevent); and the **incarnation-keyed orphan sweep**, so a row left by an acquisition cancelled after its INSERT committed is now reclaimed at its TTL like any other lapsed lease instead of at the next reaper wake.

What is bought for it: a consumer behaves identically wherever it runs, a replica is replaceable, and any replica serves any lease operation. The failure mode removed is also worth naming — one beacon per instance meant one blast radius, so a single connection blip made *every* lock that instance held stealable at once, and a ping overrunning its statement timeout was read as a loss, which made runtime starvation a way to lose every lock on an instance without the database having done anything wrong. Nothing can now invalidate a lease for a reason local to the holding process.

**2. A lapsing lease writes nothing, so no watch event announces it.** Expiry used to be *physical*: the entry vanished at the deadline and a waiter learned of it from the watch's `Expired` event. Expiry is now **logical** — the stored deadline is the authority and the record outlives it — so nothing happens in the store when a lease lapses.

Every waiter must therefore schedule its own wake-up at the incumbent's observed deadline. Both cache-backed defaults do (a blocking `lock()` caps each wait by it; a follower caps its reclaim tick by it), and the Postgres lock's pre-existing 250 ms release-NOTIFY heartbeat already bounds it. **Any new waiter needs the same discipline or it sleeps past a lease it could have taken** — this applies to the server's blocking `Lock` RPC (`S1`) and the remote client's watch pump (`K2`).

### Consequences

- **Good**: a cluster restart, upgrade or reschedule revokes nothing. Consumers observe a dropped subscription and one `RestartingWatch` cycle, nothing more.
- **Good**: `ClusterIP` round-robining across replicas is correct rather than dangerous, so nothing is affine and the constraint that previously ruled out a headless Service disappears with it. `replicaCount: 1` becomes a shipped default awaiting the `T5` failover suite, not a correctness constraint.
- **Good**: renewal keeps doing double duty as a consumer-liveness signal, since the token the holder presents is the authority (invariant I8).
- **Bad**: the two costs above.
- **Neutral**: watch subscriptions and long-poll cursors stay replica-affine. A replica going away closes those streams, the client sees `Closed(Provider{ConnectionLost})`, and `RestartingWatch` re-subscribes — possibly against a different replica, which is fine because a subscription carries no lease authority. **Failover costs a re-subscribe, never a lost lock.**
- **Neutral**: the session registry becomes an index, not an owner. It carries diagnostics, quota accounting and watch bookkeeping; nothing in the lease path reads it, so a lost or stale entry costs a reporting artefact rather than a lease.

### Confirmation

- `renew` with a non-matching `(name, owner, fence)` or a lapsed deadline returns `LockExpired`, **asserted by renewing through a different backend handle than the one that acquired** — the property that makes any replica able to serve any lease operation. (`PG-LOCK-021` does this across a full handle stop/start; the SDK defaults assert it against a second handle over one cache.)
- Acquisition of a lapsed lease **strictly increases** `fence`; the superseded token's `renew` fails and its `release` is a no-op `Ok` that leaves the successor's lease untouched (`PG-LOCK-024`).
- `release` on an absent record returns `Ok` (`PG-LOCK-025`).
- **The fence survives a lapse and its sweeps**, and the same owner re-acquiring is fenced against its own stale token — asserted as a *pair* on both paths, so the window is what the pair measures rather than a reaper that happened not to run: `PG-LOCK-026` (default window, row retained, fence climbs) against `PG-LOCK-027` (200 ms window, row swept, counter restarts), and `the_fence_climbs_across_a_lapse_a_sweep_and_the_same_owner` against `a_window_shorter_than_the_lapse_lets_the_fence_reset` over the cache-backed defaults.
- A zero window is refused at startup by name, in the gear config and in both plugin config shapes (`a_zero_window_fails_startup_by_name`, `a_zero_fence_retention_is_rejected_by_both_config_shapes`), and a lease TTL at or over the window warns once (`a_ttl_at_or_over_the_window_warns_once`).
- **Uniform expiry**: a killed holder's lock is reclaimed at its TTL and **not before**, sampled repeatedly across the remainder of the lease rather than once (`PG-LOCK-023`). This is the assertion that the beacon removal was complete.
- The acquire path issues no `pg_locks` scan on any path, checked against a real `EXPLAIN ANALYZE` plan (`PG-SPEC-012`), and `rg holder_beacon` over the plugin returns nothing.
- A cluster-gear restart under held locks and leadership produces no `LockExpired`, no `Status(Lost)` and no re-acquire; only watch subscribers see `Closed`. Cross-replica renew/release is the gate on `replicaCount > 1` (`T3`, `T5`).

## Pros and Cons of the Options

### Option 1: Session-owned leases — the broker vouches for the lease

The replica that issued a lease holds the state that interprets it, as the Postgres beacon held state for the acquiring process.

- Good, because it is the smallest change: the beacon already worked this way, and no schema or wire change is needed.
- Good, because sub-TTL reclaim of a crashed holder's lock is retained for free.
- **Bad, because every cluster deploy becomes a revocation event.** The broker's beacon vouches for locks held by other, live consumers, so rolling the pod revokes the fleet's locks.
- **Bad, because every second replica is a correctness hazard**, so `replicaCount > 1` is permanently unavailable and a `ClusterIP` Service is dangerous rather than merely unhelpful.
- Bad, because it violates invariant I7 directly, which makes it a design change rather than an option.

### Option 2: Keep the liveness beacon for in-process acquisitions only

Store-owned leases for brokered acquisitions; the beacon predicate retained when the acquiring process is the holding process.

- Good, because it keeps sub-TTL reclaim exactly where it is safe, and gives up nothing in Profile 1.
- Good, because it needs no change to already-shipped Profile 1 behaviour.
- **Bad, because it breaks profile transparency (Goal 2, invariant I1).** The same consumer source, the same config, two reclaim timings: one deployment reclaims a dead holder's lock in milliseconds, another waits out the TTL.
- **Bad, because it produces a class of bug that reproduces in only one profile.** A consumer whose correctness quietly depends on fast reclaim passes every test embedded and fails deployed — the most expensive shape of defect this design can produce.
- Bad, because the acquire predicate must then branch on how the caller arrived, which the row cannot know: the beacon column would have to be nullable and the predicate two-branched forever.
- Bad, because it keeps the beacon's own failure modes (one blast radius per instance; starvation-induced total loss) in the codebase for a benefit available in only one profile.

### Option 3: Store-owned leases, uniformly (CHOSEN)

- Good, because a cluster replica becomes replaceable and coordination scales past one process — the two requirements that drove this.
- Good, because the answer to every lease predicate is a property of stored state, so it is identical on every replica and cheap to reason about.
- Good, because it collapses two liveness mechanisms into one, and removes the acquire path's only unindexed scan as a side effect (`pg_locks` is a function scan with no index, so a contended acquire was `O(advisory locks on the server)`).
- Good, because it is the shape etcd, Consul and Kubernetes all converged on, so the failure modes are well understood.
- Bad, because of the two costs above: a crashed holder's lock lingers to its TTL in every profile, and a lapsing lease produces no watch event, so every waiter needs a deadline-driven wake-up.
- Bad, because the fence needs a retention window to be correct across a lapse, which is extra machinery (`L3`) and a configuration an operator can get wrong.

## More Information

- [DESIGN.md](../DESIGN.md) §3.19.1 (the model), §3.19.2 (restarts and upgrades revoke nothing), §3.19.3 (replica count)
- [ADR-002](./002-async-boundary-no-remote-in-critical-section.md) — the TTL safety net this replaces fencing tokens with; the reason the fence stays internal
- [ADR-009](./009-leader-election-backend-safety.md) — the linearizability the CAS steal depends on
- `postgres-cluster-plugin` DESIGN §2.1 (the lease row), §5.1 (what the beacon was and why it is gone), §5.2 (what garbage collection no longer does), §10 (shutdown revokes nothing)
- PR [#4411](https://github.com/constructorfabric/gears-rust/pull/4411) — the beacon this retires, and the design it retires with it

## Traceability

| Requirement | Relationship |
|---|---|
| `cpt-cf-clst-fr-lock-release` | Refines — the TTL safety net becomes the *only* liveness authority, in every profile |
| `cpt-cf-clst-fr-shutdown-revoke` | Amends — a cluster restart is not a lease loss; shutdown closes subscriptions only |
| `cpt-cf-clst-fr-shutdown-ttl-cleanup` | Confirms — held claims and locks lapse via their TTL once a holder stops renewing |
| `cpt-cf-clst-nfr-plugin-stability` | Confirms — the four lease methods are defaulted and dyn-safe, so no shipped plugin breaks |
| `cpt-cf-nfr-oop-latency` | Supports — one conditional write per lease operation, and one fewer unindexed scan on the Postgres acquire path |
