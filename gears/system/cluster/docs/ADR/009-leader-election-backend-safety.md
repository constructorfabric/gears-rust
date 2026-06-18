---
status: accepted
date: 2026-04-27
---

# ADR-009: Leader Election Backend Safety Under Failure

**ID**: `cpt-cf-clst-adr-leader-election-backend-safety`

> Initial analysis recorded during the cluster design review on 2026-03-15. Promoted to ADR on 2026-04-27, capturing the constructor-pair evolution from the original "no opt-out" stance.

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Per-backend safety table](#per-backend-safety-table)
  - [Constructor pair: default-safe + explicit opt-in](#constructor-pair-default-safe--explicit-opt-in)
  - [Why opt-in exists at all (evolution from the original "no bypass" stance)](#why-opt-in-exists-at-all-evolution-from-the-original-no-bypass-stance)
  - [Recommended deployments](#recommended-deployments)
  - [Lock backend follows the same rule](#lock-backend-follows-the-same-rule)
  - [Service-discovery backend does NOT follow the same rule](#service-discovery-backend-does-not-follow-the-same-rule)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Hard requirement, no opt-out](#option-1-hard-requirement-no-opt-out)
  - [Option 2: Best-effort — declare expected guarantees in docs, let operator pick](#option-2-best-effort--declare-expected-guarantees-in-docs-let-operator-pick)
  - [Option 3: Constructor pair — default-safe + explicit opt-in with warning (CHOSEN)](#option-3-constructor-pair--default-safe--explicit-opt-in-with-warning-chosen)
- [More Information](#more-information)
  - [Why each unsafe backend is unsafe (summary)](#why-each-unsafe-backend-is-unsafe-summary)
  - [References](#references)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

The SDK-default `CasBasedLeaderElectionBackend` (per ADR-001) is built on `ClusterCacheBackend::put_if_absent` + `compare_and_swap` + `watch` + TTL renewal. The correctness of this algorithm — specifically, the at-most-one-leader guarantee — depends entirely on the underlying cache backend providing **linearizable** `put_if_absent` and `compare_and_swap` under all failure modes the system tolerates (process crash, network partition, replica failover, quorum loss).

Linearizability is the wrong angle for performance discussion (which is why ADR-001 frames its per-backend pros/cons around throughput, latency, watch overhead, and connection cost). It is the *right* angle for correctness-under-failure discussion. The two angles produce non-overlapping conclusions:

- ADR-001 says "Redis is great for cache because 100k+ ops/sec single-node." That's a perf claim and it's correct.
- This ADR says "Redis Sentinel default config can produce two leaders on every failover." That's a correctness claim and it's also correct. The same backend is great for cache and unsafe for leader election.

Without a versioned decision-of-record on per-backend safety, two failure modes recur:

1. **Operators pick a backend on perf grounds and inherit a correctness bug they didn't know existed.** A team running Redis Sentinel for cache (perfectly fine) extends it to leader election (silently broken on every failover). The first incident is months away when the failover finally trims the wrong replica.

2. **The SDK silently accepts misconfigured backends.** Without enforcement, a `CasBasedLeaderElectionBackend` instantiated against an `EventuallyConsistent` cache would compile, start, and run — until partition, when it would produce dual leaders without diagnostic.

The earlier (pre-this-ADR) shape recorded in r2 was a hard requirement: `new(cache)` returns `Err(RequiresLinearizableCache)` if the cache is `EventuallyConsistent`, with no bypass. The current shape — captured here — is a constructor pair: default-safe `new(cache)` plus an explicit `new_allow_weak_consistency(cache)` that requires the caller to opt in by name and emits a warning at instantiation. This ADR records both the per-backend safety analysis (the substance) and the evolution from "no bypass" to "explicit opt-in with friction" (the API shape).

The per-backend analysis was originally captured during the cluster design review and is now consolidated in this ADR.

## Decision Drivers

- **Correctness under failure must be a startup-time check, not a runtime surprise.** Misconfigured leader election should fail loudly at gear boot.
- **Performance and correctness are independent axes.** ADR-001 picks backends on perf grounds; this ADR validates them on correctness grounds. Both must agree for a backend to ship.
- **The hybrid (mixed-backend) profile is the platform's existing answer to "Redis cache + correct leader election".** Operators who want Redis for cache route leader election separately to a linearizable backend. Capability validation enforces this at startup (per ADR-007).
- **Opt-out must be explicit and high-friction.** A consumer who genuinely accepts the risk (testing, transient deployments, dev environments) should be able to proceed, but the API shape must make accidental misuse impossible.
- **Don't litigate the same analysis at every consumer site.** The decision per backend lives once, in this ADR; consumers and operators read it; nobody re-derives it.

## Considered Options

1. **Hard requirement, no opt-out** — `new(cache)` rejects `EventuallyConsistent`; no second constructor exists. (Original r2 stance.)
2. **Best-effort — declare expected guarantees in docs, let operator pick** — `new(cache)` always succeeds; documentation warns about unsafe combinations.
3. **Constructor pair — default-safe + explicit opt-in with warning** (CHOSEN) — `new(cache)` rejects `EventuallyConsistent`; `new_allow_weak_consistency(cache)` succeeds, requires caller to name-opt-in, and emits a warning log at instantiation.

## Decision Outcome

Chosen option: **Option 3** — constructor pair.

Two pieces:

- **Per-backend safety classification** (table below). Each cache backend's `consistency()` declaration aligns with whether it is safe for leader election under failure.
- **Constructor pair on `CasBasedLeaderElectionBackend`** (and `CasBasedDistributedLockBackend` — see below):
  - `new(cache: Arc<dyn ClusterCacheBackend>) -> Result<Self, ClusterError>` — returns `Err(ClusterError::InvalidConfig)` if `cache.consistency() == EventuallyConsistent`. This is the default path consumers reach for first.
  - `new_allow_weak_consistency(cache: Arc<dyn ClusterCacheBackend>) -> Self` — always succeeds. The caller acknowledges the safety implications by selecting the constructor explicitly. Construction emits a warning log identifying the bound cache backend's type name and the consistency class.

The wiring crate's omit-primitive auto-wrap (per ADR-001) uses `new()`, so single-backend profiles built on `EventuallyConsistent` cache fail at startup with a clear diagnostic. Operators with a weak-consistency cache route leader election explicitly to a linearizable backend (the per-primitive routing is exactly what enables this).

Capability validation (ADR-007) closes the loop: a consumer that requires `LeaderElectionCapability::Linearizable` and is bound to a backend whose `features().linearizable == false` fails resolution at startup.

### Per-backend safety table

The relevant axis is **linearizability under failure**: does a write that returned success remain durable across crash, replication failover, and partition? For `put_if_absent` + CAS to enforce at-most-one leader, the answer must be yes.

| Backend | Default config safe for `CasBasedLeaderElection`? | Safe-with-config | Failure mode if unsafe |
|---|---|---|---|
| **Redis single-node**, `appendfsync everysec` (default) | **No** | Yes: `appendfsync always` | Crash between ack and fsync → up to 1s rollback window |
| Redis single-node, `appendfsync always` | Yes | — | Single-node only; no replica failover concern |
| **Redis Sentinel** (async replication) | **No** | Partial: `WAIT 1` + `min-replicas-to-write 1` reduces window but does not linearize | Every failover: promoted replica may lack accepted writes |
| **Redis Cluster** | **No** | No known safe config for this use case | Same async replication + hash-slot migration edge cases |
| **NATS KV R=1** | **No** | N/A — must use R≥3 | Crash between WAL write and fsync |
| **NATS KV R=3** | **Yes** (Raft quorum commit) | — | Only on quorum loss (≥2/3 servers down) |
| **Postgres**, `synchronous_commit=on`, single node | **Yes** | — | Locally durable; no replica concern |
| Postgres, `synchronous_commit=on`, **synchronous** streaming replication | **Yes** | — | Only if quorum of sync standbys fails simultaneously |
| **Postgres**, `synchronous_commit=off` | **No** | Yes: `synchronous_commit=on` | Crash within `wal_writer_delay` (default 200 ms) → rollback |
| Postgres, `synchronous_commit=on`, **async** streaming replication (common default) | **No (failover unsafe)** | Yes: enable sync standby (`synchronous_standby_names`) | Primary crash before replica catches up |
| **K8s Lease API** (etcd-backed) | **Yes** | — | Only on etcd quorum loss |
| **etcd native** | **Yes** | — | Only on quorum loss |
| **Hazelcast CP Subsystem** | **Yes** | CP group must be configured (`cp-member-count ≥ 3`) | AP mode (default before 5.x); CP group loss |
| **Standalone (in-process)** | **Yes** (single instance, trivially serializable) | — | Multi-process: no coordination — not the SDK's responsibility |

Plugins that bind to one of the **No** rows MUST declare `consistency() == EventuallyConsistent`. Plugins on the **Yes** rows MUST declare `Linearizable`. Honest declaration is a contract obligation; capability validation rejects the combination at startup.

The safety classification holds only when each backend's runtime behavior matches its declared characteristics. Some Postgres GUCs that affect this classification (notably `synchronous_commit`) are `USERSET` scope and can be mutated per-session at runtime; startup capability validation cannot detect later mutations. The Postgres plugin enforces `synchronous_commit = on` on every connection in its dedicated pool. Per-plugin DESIGNs document the enforcement mechanism and the full GUC checklist relevant to this ADR's safety classification.

### Constructor pair: default-safe + explicit opt-in

```rust
impl CasBasedLeaderElectionBackend {
    /// Default constructor. Refuses `EventuallyConsistent` caches.
    pub fn new(cache: Arc<dyn ClusterCacheBackend>) -> Result<Self, ClusterError> {
        if cache.consistency() == CacheConsistency::EventuallyConsistent {
            return Err(ClusterError::InvalidConfig {
                reason: format!(
                    "CasBasedLeaderElectionBackend requires Linearizable cache; \
                     bound cache {} declares EventuallyConsistent. \
                     Use new_allow_weak_consistency(cache) to opt in explicitly, \
                     or route leader_election to a linearizable backend.",
                    std::any::type_name_of_val(&*cache),
                ),
            });
        }
        Ok(Self { cache })
    }

    /// Opt-in constructor. Caller acknowledges that split-brain is possible
    /// under cache-backend failover. Emits a warning log at instantiation.
    pub fn new_allow_weak_consistency(cache: Arc<dyn ClusterCacheBackend>) -> Self {
        if cache.consistency() == CacheConsistency::EventuallyConsistent {
            tracing::warn!(
                provider = %std::any::type_name_of_val(&*cache),
                "CasBasedLeaderElectionBackend instantiated against EventuallyConsistent cache; \
                 split-brain is possible under failover"
            );
        }
        Self { cache }
    }
}
```

> **Implementation note.** The snippets in this ADR are illustrative of the *decision* — the `new` / `new_allow_weak_consistency` constructor pair (reject `EventuallyConsistent` by default; warn on explicit opt-in). The shipped code factors this shared logic into `cluster-sdk/src/defaults/guard.rs` (`reject_weak_consistency` / `warn_weak_consistency`), reused by both the leader-election and lock defaults. The diagnostic names the **backend** (via its `Self::NAME`) and reports the cache's *consistency class* — it does **not** call `std::any::type_name_of_val(&*cache)`, which on a `&dyn ClusterCacheBackend` would yield only the trait-object name, never the concrete cache type.

The warning log fires on instantiation regardless of whether the backend is actually misused — the log is the audit trail. Operators monitoring logs see the warning and can confirm whether the opt-in was intentional.

### Why opt-in exists at all (evolution from the original "no bypass" stance)

The original r2 stance was Option 1: `new(cache)` rejects `EventuallyConsistent` and there is no second constructor. The reasoning was sound for production: an opt-out hatch becomes a habit under deadline pressure, audit burden grows, and the at-most-one-leader guarantee should be unambiguous.

The shape evolved to Option 3 because three legitimate use cases for opt-in emerged:

1. **Test environments where the consumer specifically wants to verify split-brain handling.** A test that expects `Lost` events under partition needs to construct the backend against a deliberately weak cache.
2. **Single-replica development deployments** (Redis without Sentinel, single-node NATS) where the operator knows the deployment cannot fail over and the cache's "weak" classification is technically true but operationally moot.
3. **Consumer-controlled fallback paths** — a consumer that has its own application-level idempotency layer (CAS on its own state) may genuinely not need linearizable leader election; the leader-election watch is informational.

Option 3 keeps the protection (default-safe construction) and the audit trail (warning log) while allowing these three to proceed without forking the cluster gear. The friction of typing `new_allow_weak_consistency` instead of `new` is sufficient to prevent accidental use.

### Recommended deployments

For **Redis-heavy stacks**: use the per-primitive routing to bind leader election to K8s Lease (if on K8s) or a Postgres advisory-lock backend (if Postgres is present, follow-up plugin work). Keep Redis for cache and rate-limiting where eventual consistency is acceptable and throughput matters.

For **Postgres-only stacks**: ensure `synchronous_commit=on` (the default) and either run a single primary or configure synchronous streaming replication (`synchronous_standby_names`) for HA.

For **NATS-backed stacks**: use R≥3 for any bucket carrying election or lock state.

For **K8s-native stacks**: use the K8s Lease API directly via the K8s plugin — it is the reference implementation of linearizable `put_if_absent` + CAS + TTL renewal.

For **dev / testing**: standalone is trivially correct (single instance); for multi-process tests use Postgres with `synchronous_commit=on` against the test container.

### Lock backend follows the same rule

`CasBasedDistributedLockBackend` has identical safety requirements: split-brain on a lock has the same correctness consequences as split-brain on a leader (two holders, neither knowing about the other). The same constructor pair applies — `new(cache)` rejects `EventuallyConsistent`; `new_allow_weak_consistency(cache)` opts in with a warning. `LockCapability::Linearizable` is the consumer-side requirement.

### Service-discovery backend does NOT follow the same rule

`CacheBasedServiceDiscoveryBackend` has weaker correctness requirements. Stale instance entries during cache failover are recoverable by clients retrying — a discoverer that gets a stale instance set re-discovers when the watch reset arrives, and consumers tolerate transient routing errors. The SDK default for service discovery does NOT require linearizable cache; the constructor is single-form (`new(cache)`) and accepts `EventuallyConsistent` without opt-in.

This asymmetry is intentional: leader election and locks have at-most-one semantics where dual occupancy is a correctness bug; service discovery has set-membership semantics where transient inaccuracy degrades into retry, not corruption. Documenting the distinction explicitly prevents the (incorrect) instinct to treat all three SDK defaults the same way.

### Consequences

- **Single-backend profiles built on `EventuallyConsistent` cache fail at startup.** The wiring crate's omit-primitive auto-wrap calls `new()`; mismatch surfaces as `ClusterError::InvalidConfig` with a diagnostic identifying the cache backend and suggesting `new_allow_weak_consistency` or per-primitive routing.
- **Honest backend declaration is a contract obligation.** A plugin that lies about its consistency (declares `Linearizable` for a Redis Sentinel async-replication setup) defeats the validation. Reviewers of plugin PRs verify the declaration against this ADR's per-backend table.
- **The hybrid (mixed-backend) profile is the platform's intended pattern for "Redis-everywhere except where it's unsafe".** Operators don't have to choose between "all Redis" (broken leader election) or "all Postgres" (slower cache); per-primitive routing lets them pick correctly per primitive.
- **The constructor-pair pattern is reusable.** When a future SDK-default backend ships with similar correctness-vs-permissiveness trade-offs, the same `new` + `new_allow_weak_consistency` pair is the established pattern.
- **Audit trail via logs.** Every opt-in instantiation produces a warning log; security review can grep production logs for these and verify each is intentional.
- **No "linearizable-ish" middle ground.** The `CacheConsistency` enum is binary (`Linearizable` / `EventuallyConsistent`). Backends like Redis Sentinel + `WAIT 1` that reduce but don't eliminate the failover window declare `EventuallyConsistent`. Honest two-class declaration beats fuzzy three-class declaration.

### Confirmation

- Unit test: instantiate `CasBasedLeaderElectionBackend::new(weak_cache)` against a stub cache returning `EventuallyConsistent`; assert `Err(InvalidConfig)` with provider name in the message.
- Unit test: instantiate `CasBasedLeaderElectionBackend::new_allow_weak_consistency(weak_cache)`; assert success and verify the warning log fires (via `tracing-test` or equivalent capture).
- Integration test (per plugin, in plugin follow-up changes): each plugin's cache backend declares `consistency()` matching this ADR's per-backend table. Test fails if a plugin's declaration drifts from the table.
- Plugin-level GUC test (Postgres plugin integration suite, follow-up): the plugin's declared safety classification holds across mid-session GUC mutations; per-connection enforcement restores the GUC before the next cluster transaction executes.
- Capability validation test: a consumer requiring `LeaderElectionCapability::Linearizable` against a backend whose `features().linearizable == false` produces `Err(CapabilityNotMet)` at resolver time.
- Wiring-layer test: a profile binding `cache: redis-sentinel` (async replication) and omitting `leader_election` fails startup with `InvalidConfig` from the auto-wrap call to `new()`.

## Pros and Cons of the Options

### Option 1: Hard requirement, no opt-out

```rust
impl CasBasedLeaderElectionBackend {
    pub fn new(cache: Arc<dyn ClusterCacheBackend>) -> Result<Self, ClusterError> {
        if cache.consistency() == CacheConsistency::EventuallyConsistent {
            return Err(ClusterError::InvalidConfig { /* ... */ });
        }
        Ok(Self { cache })
    }
    // No second constructor.
}
```

- Good, because the at-most-one-leader guarantee is unambiguous in code: there is literally no way to instantiate this backend against a weak cache.
- Good, because audit burden is zero: there's nothing to opt into.
- Bad, because legitimate edge cases (test environments deliberately exercising split-brain handling, single-replica dev deployments, consumer-managed idempotency) require forking or re-implementing the algorithm.
- Bad, because the strictness can become its own reliability hazard: a team that encounters the rejection on a weak cache may copy-paste the algorithm into their own crate without the protection, and now the platform has divergent implementations.
- Neutral, because most consumers should never need the opt-in. The strict shape is correct for production; it's the developer-experience tail that suffers.

### Option 2: Best-effort — declare expected guarantees in docs, let operator pick

```rust
impl CasBasedLeaderElectionBackend {
    pub fn new(cache: Arc<dyn ClusterCacheBackend>) -> Self {
        // Always succeeds. Caller is responsible for picking a safe backend.
        Self { cache }
    }
}
```

- Good, because zero enforcement infrastructure.
- Good, because flexible for advanced consumers.
- Bad, because misconfiguration is silent. The team running Redis Sentinel for leader election produces dual leaders on the next failover with zero diagnostic; the failure is months away from the configuration choice.
- Bad, because the "documented expectation" is unenforced — every consumer team reads the docs once and forgets; the platform-wide correctness guarantee is contingent on continuous human attention.
- Bad, because no audit trail. Production logs contain no record of which deployments are actually using a weak combination.

### Option 3: Constructor pair — default-safe + explicit opt-in with warning (CHOSEN)

- Good, because default path is safe — accidental misuse is impossible.
- Good, because legitimate edge cases (test, dev, app-managed idempotency) have a documented path.
- Good, because the warning log is an audit trail. Production logs reveal which deployments are using the opt-in; security review can verify each.
- Good, because the friction of typing `new_allow_weak_consistency` instead of `new` is sufficient to prevent thoughtless use; the call site is greppable.
- Good, because the pattern composes with capability validation (ADR-007): even if the backend is constructed with `new_allow_weak_consistency`, a consumer requiring `LeaderElectionCapability::Linearizable` still rejects the resolved facade at startup.
- Bad, because there are now two constructors instead of one. Documentation must explain when to use which. Mitigated by clear method names and warning-log behavior.
- Bad, because plugin authors must remember the pattern when shipping new SDK-default-like backends in the future. Mitigated by this ADR being the reusable reference.
- Neutral, because the original r2 stance (Option 1) was correct for its scope (production-only); evolution to Option 3 reflects broader consumer needs without weakening production safety.

## More Information

### Why each unsafe backend is unsafe (summary)

- **Redis single-node, `appendfsync everysec`**: between client ack and the next fsync (up to 1s), the write exists only in kernel page cache. Process crash in that window loses the write; on restart, the key does not exist, and a different caller's `SET NX` succeeds — two leaders.
- **Redis Sentinel (async replication)**: primary acknowledges `SET NX` before replicating to the replica. If primary crashes before replication, Sentinel promotes a replica that has never seen the write; a different caller's `SET NX` against the new primary succeeds — two leaders.
- **Redis Cluster**: same async replication issue plus hash-slot migration edge cases.
- **NATS KV R=1**: same WAL-loss window as Redis AOF.
- **Postgres `synchronous_commit=off`**: COMMIT returns after WAL is written to memory but before fsync; up to `wal_writer_delay` (default 200 ms) of acknowledged transactions can be lost on crash.
- **Postgres async streaming replication**: same issue as Redis Sentinel — primary commits before replica receives; failover promotes a replica missing recent commits.

### References

- ADR-001 — backend compatibility and the cache-CAS-universal model. Performance pros/cons per backend; this ADR is the correctness counterpart.
- ADR-002 — async boundary, no remote I/O in critical section. The "no fencing tokens" argument relies on the no-remote-in-critical-section rule, which composes with this ADR's at-most-one guarantee.
- ADR-005 — facade + backend trait pattern. The constructor pair lives on `CasBasedLeaderElectionBackend` (a `LeaderElectionBackend` impl); consumers hold the facade and never see the constructor distinction.
- ADR-007 — capability typing and typed profile resolution. `LeaderElectionCapability::Linearizable` enforcement at the consumer's resolver call site is the defense-in-depth check that catches plugin lies (a plugin that declares `Linearizable` against this ADR's table is caught when a consumer requires the capability and the backend's `features().linearizable == true` declaration is verified at startup; the integration test per plugin verifies the declaration matches).
- DESIGN.md §3.11 (SDK Default Backends — constructor-pair definition).
- Martin Kleppmann, "How to do distributed locking" — the canonical analysis of distributed-lock failure modes. <https://martin.kleppmann.com/2016/02/08/how-to-do-distributed-locking.html>
- Jepsen consistency model hierarchy. <https://jepsen.io/consistency>
- etcd API guarantees (linearizability). <https://etcd.io/docs/current/learning/api_guarantees/>
- K8s Lease API. <https://kubernetes.io/docs/concepts/architecture/leases/>
- Postgres `synchronous_commit`. <https://www.postgresql.org/docs/current/runtime-config-wal.html#GUC-SYNCHRONOUS-COMMIT>
- NATS JetStream replication. <https://docs.nats.io/nats-concepts/jetstream/streams#replication-factor>

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements and design elements:

- `cpt-cf-clst-fr-leader-elect` — At-most-one leader per election name when bound to a Linearizable cache.
- `cpt-cf-clst-fr-lock-acquire` — Same correctness rule applies to `CasBasedDistributedLockBackend`.
- `cpt-cf-clst-nfr-leader-guarantee` — Per-backend safety classification + constructor pair enforcement.
- DESIGN §3.11 SDK Default Backends — Constructor pair (`new` rejects `EventuallyConsistent`; `new_allow_weak_consistency` opts in with warning).
- DESIGN §4.1 Backend Feature Compatibility — Per-backend matrix consistent with this ADR's safety table.
- `cpt-cf-clst-component-sdk` (DESIGN §3.2) — SDK hosts `CasBasedLeaderElectionBackend` and `CasBasedDistributedLockBackend`.

**Sibling ADRs:**

- ADR-001 — Performance counterpart; per-backend pros/cons.
- ADR-002 — No fencing tokens — this ADR's at-most-one guarantee composes with ADR-002's no-remote-in-critical-section rule.
- ADR-005 — Facade pattern hosts the constructor pair on the backend type.
- ADR-007 — Capability validation enforces backend declaration honesty (catches plugins that mis-declare characteristics).
