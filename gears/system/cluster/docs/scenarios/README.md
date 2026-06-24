# Cluster Test Scenarios — Ordered Catalog

> **Status: DRAFT — for iteration.** This is the master, ordered list of behaviors the
> cluster gear must be tested against. Each scenario has a stable ID, the test layer it
> belongs to (see [TESTING-STRATEGY.md](../TESTING-STRATEGY.md) §2), and the
> requirement it traces to. The `cluster-conformance` crate implements the **L2**
> scenarios as a parametrized suite run against every backend; **L4** scenarios are
> implemented per-backend with fault injection / simulation.

## Legend

- **Layer** — `L2` shared conformance suite · `L4` fault-injection / deterministic
  simulation.
- **Status** — ☐ not started · ◑ scaffolded (stub exists) · ☑ implemented · ⊘ intentionally
  not tested directly at this layer (covered elsewhere — see the row's notes).
- **Capability gate** — assertion applies only when the backend declares the listed
  feature; otherwise the scenario asserts the documented fallback (e.g. `Unsupported`).

Each `SC-*` table below is an **index**. Full per-scenario detail lives in a
per-primitive file (linked under each section heading) using a fixed template:

- *Intent* — the behavior this guards and why it matters.
- *Steps* — the stimulus applied to a fresh backend.
- *Expected* — the observable outcome the suite asserts.
- *Done-when* — the acceptance bar for marking the row ☑.

---

## 1. Cache (`SC-CACHE-*`)

| ID | Layer | Status | Scenario | Capability gate | Traces to |
|----|-------|--------|----------|-----------------|-----------|
| SC-CACHE-001 | L2 | ☑ | `get` on an absent key returns `Ok(None)`, never an error | — | `fr-cache-storage` |
| SC-CACHE-002 | L2 | ☑ | `put` then `get` returns the stored value at version 1 | — | `fr-cache-storage` |
| SC-CACHE-003 | L2 | ☑ | each overwrite strictly increments the version; version 0 never observed | — | `principle-version-based-cas` |
| SC-CACHE-004 | L2 | ☑ | `put_if_absent` returns `Some(entry)` on create, `None` when present (atomic) | — | `fr-cache-atomic` |
| SC-CACHE-005 | L2 | ☑ | `compare_and_swap` succeeds iff `expected_version == current` | — | `fr-cache-atomic` |
| SC-CACHE-006 | L2 | ☑ | CAS on a stale version returns `CasConflict { key, current }` carrying the current entry | — | `fr-cache-atomic` |
| SC-CACHE-007 | L2 | ☑ | `delete` removes the entry and reports prior existence | — | `fr-cache-storage` |
| SC-CACHE-008 | L2 | ☑ | `compare_and_delete` removes only when the owner token matches; mismatch/absent → `Ok(false)` | — | DESIGN §3.3 backend note |
| SC-CACHE-009 | L2 | ☑ | a key deleted and re-created resets to version 1, and a value/owner guard still distinguishes the successor | — | [DESIGN.md §3.3](../DESIGN.md#33-api-contracts) |
| SC-CACHE-010 | L2 | ☑ | TTL expiry removes the entry and emits `CacheEvent::Expired` to watchers | — | `fr-cache-ttl` |
| SC-CACHE-011 | L2 | ☑ | indefinite (no-TTL) entries persist until explicit delete; in-memory backends document the constraint | — | `fr-cache-ttl` |
| SC-CACHE-012 | L2 | ☑ | exact `watch` yields `Changed`/`Deleted` for the key, preserving per-key order | — | `fr-cache-watch`, `nfr-watch-delivery` |
| SC-CACHE-013 | L2 | ☑ | `watch_prefix` yields events for matching keys; unsupported backends return `Unsupported` | `prefix_watch` | `fr-cache-watch` |
| SC-CACHE-014 | L2 | ☑ | `PollingPrefixWatch` polyfill synthesizes prefix diffs (Changed/Deleted) on a non-native backend | `!prefix_watch` | §3.12 polyfill |
| SC-CACHE-015 | L2 | ☑ | watch delivery is at-most-once per subscriber per key | — | `nfr-watch-delivery` |
| SC-CACHE-016 | L4 | ☐ | a slow subscriber receives `Lagged { dropped }` rather than blocking writers | — | `fr-watch-lifecycle-signals` |
| SC-CACHE-017 | L4 | ☐ | connection loss surfaces `Reset` on resubscribe; consumer re-reads | — | `fr-watch-lifecycle-signals` |

**Details:** [cache.md](./cache.md)

## 2. Leader Election (`SC-LEAD-*`)

| ID | Layer | Status | Scenario | Capability gate | Traces to |
|----|-------|--------|----------|-----------------|-----------|
| SC-LEAD-001 | L2 | ☐ | a single candidate becomes `Leader` | — | `fr-leader-elect` |
| SC-LEAD-002 | L2 | ☐ | with N candidates, at most one observes `Leader` at any time | `linearizable` | `nfr-leader-guarantee` |
| SC-LEAD-003 | L2 | ☐ | the leader's claim auto-renews without consumer renewal code | — | `fr-leader-elect` |
| SC-LEAD-004 | L2 | ☐ | `resign()` releases the claim; a successor is elected within a round-trip | — | `fr-leader-resign` |
| SC-LEAD-005 | L2 | ☐ | `is_leader()`/`status()` reflect the cached snapshot synchronously | — | `fr-leader-observability` |
| SC-LEAD-006 | L2 | ☐ | `Status(Lost)` is transient — the watch auto-reenrolls to `Leader`/`Follower` | — | `fr-leader-observability` |
| SC-LEAD-007 | L2 | ☐ | `ElectionConfig::new` rejects zero `ttl`/`max_missed_renewals` | — | `fr-leader-config` |
| SC-LEAD-008 | L2 | ☐ | default-constructor leader election rejects an `EventuallyConsistent` cache; `new_allow_weak_consistency` warns | — | ADR-009 |
| SC-LEAD-009 | L4 | ☐ | under partition, the leader observes `Status(Lost)` within the configured TTL | `linearizable` | DESIGN §3.3 staleness bound |
| SC-LEAD-010 | L4 | ☐ | 10+ candidates across 3+ nodes under partition: zero split-brain | `linearizable` | `nfr-leader-guarantee` |

**Details:** [leader.md](./leader.md)

## 3. Distributed Lock (`SC-LOCK-*`)

| ID | Layer | Status | Scenario | Capability gate | Traces to |
|----|-------|--------|----------|-----------------|-----------|
| SC-LOCK-001 | L2 | ☐ | `try_lock` succeeds when free, returns `LockContended` when held | — | `fr-lock-acquire` |
| SC-LOCK-002 | L2 | ☐ | `lock` blocks up to `timeout`, then returns `LockTimeout { name, waited }` | — | `fr-lock-acquire` |
| SC-LOCK-003 | L2 | ☐ | a held lock is acquirable by another holder after its TTL lapses (crashed-holder recovery) | — | `fr-lock-release` |
| SC-LOCK-004 | L2 | ☐ | explicit `release()` frees the lock immediately; a waiter acquires it | — | `fr-lock-release` |
| SC-LOCK-005 | L2 | ☐ | `renew` extends an active lease; renewing an expired lock returns `LockExpired` | — | `fr-lock-release` |
| SC-LOCK-006 | L2 | ⊘ | a foreign holder cannot release another's lock (owner-token guard) — not exercised directly (no owner-token seam on `DistributedLockBackend`); covered indirectly by [SC-CACHE-008/009](./cache.md) | — | §3.11 defaults |
| SC-LOCK-007 | L2 | ☐ | `LockGuard::drop` performs no I/O (TTL is the only safety net) | — | ADR-002 |
| SC-LOCK-008 | L4 | ☐ | a blocked `lock()` waiter is woken promptly on release notification | — | §3.11 defaults |

**Details:** [lock.md](./lock.md)

## 4. Service Discovery (`SC-DISC-*`)

| ID | Layer | Status | Scenario | Capability gate | Traces to |
|----|-------|--------|----------|-----------------|-----------|
| SC-DISC-001 | L2 | ☐ | `register` assigns an `instance_id` when omitted; new registrations default to `Enabled` | — | `fr-sd-register` |
| SC-DISC-002 | L2 | ☐ | default `DiscoveryFilter` returns only `Enabled` instances | — | `fr-sd-discover` |
| SC-DISC-003 | L2 | ☐ | metadata predicates AND-combine; `Equals` and `OneOf` match correctly | `metadata_pushdown` (else client-side) | `fr-sd-discover` |
| SC-DISC-004 | L2 | ☐ | result-set order is treated as unspecified (suite sorts before asserting) | — | `fr-sd-discover` |
| SC-DISC-005 | L2 | ☐ | `set_state(Disabled)` drains an instance; `deregister` removes it (watchers see `Left`) | — | `fr-sd-state` |
| SC-DISC-006 | L2 | ☐ | a registration disappears after its heartbeat/TTL stops (liveness ≠ intent) | — | `fr-sd-state`, ADR-008 |
| SC-DISC-007 | L2 | ☐ | `watch` yields `Joined`/`Left`/`Updated`; filtering is client-side | — | `fr-sd-watch` |
| SC-DISC-008 | L2 | ☐ | metadata keys are NOT scoped; service `name` IS scoped | — | `fr-namespacing-sd-metadata-unscoped` |
| SC-DISC-009 | L4 | ☐ | after `Lagged`/`Reset`, re-reading membership via `discover` recovers state | — | `fr-sd-watch` |

**Details:** [discovery.md](./discovery.md)

## 5. Resolution & Capability Validation (`SC-RESV-*`)

| ID | Layer | Status | Scenario | Capability gate | Traces to |
|----|-------|--------|----------|-----------------|-----------|
| SC-RESV-001 | L2 | ☑ | resolution succeeds for a bound backend meeting all declared capabilities | — | `fr-validation-typed-profile` |
| SC-RESV-002 | L2 | ☑ | a declared capability unmet by the backend fails with `CapabilityNotMet` naming primitive/capability/provider | — | `fr-validation-startup-fail` |
| SC-RESV-003 | L2 | ☑ | resolving an unbound profile returns `ProfileNotBound` | — | §3.6 resolution |
| SC-RESV-004 | L2 | ☐ | a backend that under-declares a feature still fails the capability gate (honest declaration) | — | `fr-validation-honest-declaration` |

**Details:** [resolution.md](./resolution.md)

## 6. Scoping & Namespacing (`SC-SCOP-*`)

| ID | Layer | Status | Scenario | Capability gate | Traces to |
|----|-------|--------|----------|-----------------|-----------|
| SC-SCOP-001 | L2 | ☐ | cache `scoped(p)` prepends `p/` on write and strips it on read-path events | — | `fr-namespacing-scoped` |
| SC-SCOP-002 | L2 | ☐ | scoping composes: `scoped("a").scoped("b")` → effective prefix `a/b/` | — | §3.8 |
| SC-SCOP-003 | L2 | ☐ | an invalid prefix fails with `InvalidName` | — | §3.8 |
| SC-SCOP-004 | L2 | ☐ | the polyfill composes with scoping (full keys stripped on read) | `!prefix_watch` | §3.12 |
| SC-SCOP-005 | L2 | ☐ | leader-election names are scoped by `scoped(p)` | — | §3.8 |
| SC-SCOP-006 | L2 | ☐ | lock names are scoped by `scoped(p)` | — | §3.8 |

(Service-discovery name scoping — and metadata staying unscoped — is [SC-DISC-008](./discovery.md).)

**Details:** [scoping.md](./scoping.md)

## 7. Watch Auto-Restart Combinator (`SC-REST-*`)

| ID | Layer | Status | Scenario | Capability gate | Traces to |
|----|-------|--------|----------|-----------------|-----------|
| SC-REST-001 | L2 | ☐ | retryable `Closed` (`ConnectionLost`/`Timeout`/`ResourceExhausted`) reconnects and emits `Reset` | — | `fr-watch-auto-restart` |
| SC-REST-002 | L2 | ☐ | non-retryable `Closed` (`AuthFailure`/`Shutdown`/`CapabilityNotMet`) propagates unchanged | — | `fr-watch-auto-restart` |
| SC-REST-003 | L2 | ☐ | backoff honors `RetryPolicy` (initial/max/jitter); exhausting `max_retries` propagates the last `Closed` | — | `fr-watch-auto-restart` |
| SC-REST-004 | L2 | ☐ | the combinator is available for all three watch types via one `RetryPolicy` | — | `fr-watch-auto-restart` |

**Details:** [restart.md](./restart.md)

## 8. Lifecycle & Shutdown (`SC-LIFE-*`) — requires the wiring crate

| ID | Layer | Status | Scenario | Capability gate | Traces to |
|----|-------|--------|----------|-----------------|-----------|
| SC-LIFE-001 | L3 | ☐ | `stop()` delivers `Status(Lost)` then `Closed(Shutdown)` to active leaders, in that order | — | `fr-shutdown-revoke` |
| SC-LIFE-002 | L3 | ☐ | an in-flight blocking `lock()` waiter returns `Err(Shutdown)`, distinct from `LockTimeout` | — | `fr-shutdown-revoke` |
| SC-LIFE-003 | L3 | ☐ | active cache and SD watches receive `Closed(Shutdown)` | — | `fr-shutdown-revoke` |
| SC-LIFE-004 | L3 | ☐ | `stop()` performs no remote release — held claims/locks/registrations lapse via TTL only | — | `fr-shutdown-ttl-cleanup` |
| SC-LIFE-005 | L3 | ☐ | after `stop()`, `resolver().resolve()` returns `ProfileNotBound` | — | §3.13 |
| SC-LIFE-006 | L3 | ☐ | omitting non-cache primitives auto-wraps SDK defaults over the cache; binding a native non-cache backend is rejected with `InvalidConfig` | — | `fr-routing-omit-default`, `fr-routing-per-primitive` |

**Details:** [lifecycle.md](./lifecycle.md)

## 9. Static Analysis (`SC-LINT-*`)

| ID | Layer | Status | Scenario | Capability gate | Traces to |
|----|-------|--------|----------|-----------------|-----------|
| SC-LINT-001 | L1 | ☐ | the dylint rule fires on a remote call inside a lock critical section (positive fixture) | — | `nfr-bounded-critical-section`, `constraint-no-remote-in-critical-section` |
| SC-LINT-002 | L1 | ☐ | the dylint rule does NOT fire on compliant code (negative fixture) | — | `nfr-bounded-critical-section`, `constraint-no-remote-in-critical-section` |

**Details:** [static-analysis.md](./static-analysis.md)

## 10. Watch Lifecycle Uniformity (`SC-WLU-*`)

Cross-cutting: the three watches (cache, leader, service-discovery) must expose the same
union shape and the same recovery model. Per-primitive instances of these signals also
appear above (e.g. [SC-CACHE-016/017](./cache.md), [SC-DISC-009](./discovery.md)); this
section asserts the *uniformity* itself.

| ID | Layer | Status | Scenario | Capability gate | Traces to |
|----|-------|--------|----------|-----------------|-----------|
| SC-WLU-001 | L2 | ☐ | all three `*WatchEvent` enums share the union shape `{value-variant, Lagged, Reset, Closed}` | — | `principle-watch-union-shape`, ADR-003 |
| SC-WLU-002 | L4 | ☐ | each watch surfaces `Lagged { dropped }` under backpressure (parametrized over all three) | — | `fr-watch-lifecycle-signals` |
| SC-WLU-003 | L4 | ☐ | each watch surfaces `Reset` on resubscribe and the consumer recovers per its primitive | — | `fr-watch-lifecycle-signals` |
| SC-WLU-004 | L2 | ☐ | each watch ends terminally via `Closed(err)`; transient backend errors are retried internally, not surfaced | — | `fr-watch-lifecycle-signals`, `nfr-watch-delivery` |

**Details:** [watch-lifecycle.md](./watch-lifecycle.md)

## 11. Naming & Validation (`SC-NAME-*`)

The cluster name rule (`[a-zA-Z0-9_/-]+`-style) is uniform across all coordination names
so consumers reuse one convention; invalid names are rejected with `InvalidName`.

| ID | Layer | Status | Scenario | Capability gate | Traces to |
|----|-------|--------|----------|-----------------|-----------|
| SC-NAME-001 | L2 | ☐ | an invalid cache key is rejected with `InvalidName { name, reason }` | — | `fr-cache-storage` |
| SC-NAME-002 | L2 | ☐ | an invalid election name is rejected with `InvalidName` | — | `fr-cache-storage` |
| SC-NAME-003 | L2 | ☐ | an invalid lock name is rejected with `InvalidName` | — | `fr-cache-storage` |
| SC-NAME-004 | L2 | ☐ | an invalid service name is rejected with `InvalidName` | — | `fr-sd-register` |
| SC-NAME-005 | L2 | ☐ | the rule is uniform — a name valid (or invalid) for one primitive is so for all four | — | `fr-cache-storage` |

**Details:** [naming.md](./naming.md)

## 12. Routing & SDK Defaults (`SC-ROUTE-*`)

| ID | Layer | Status | Scenario | Capability gate | Traces to |
|----|-------|--------|----------|-----------------|-----------|
| SC-ROUTE-001 | L2 | ☑ (in `cluster`, not this crate) | a cache-only backend yields working leader/lock/SD via the SDK defaults | — | `fr-routing-cache-only-plugin` |
| SC-ROUTE-002 | L2 | ☑ (in `cluster`, not this crate) | SDK-default features derive from the cache: `LeaderElection/LockFeatures.linearizable == (cache.consistency() == Linearizable)` | — | §3.11 |

(Operator-config auto-wrap of omitted primitives, and rejection of a native non-cache
binding, are wiring-level: [SC-LIFE-006](./lifecycle.md).)

**Details:** [routing.md](./routing.md)

## 13. Observability Contract (`SC-OBS-*`)

Per `cpt-cf-clst-nfr-observability` / ADR-004, signal *names* are a contract every plugin
must honor. Authoritative catalog: [OBSERVABILITY.md](../OBSERVABILITY.md).

| ID | Layer | Status | Scenario | Capability gate | Traces to |
|----|-------|--------|----------|-----------------|-----------|
| SC-OBS-001 | L2 | ☐ | each facade operation emits its catalogued OTel span (`cluster.<primitive>.<op>`) with the specified attributes | — | `nfr-observability` |
| SC-OBS-002 | L2 | ☐ | operations emit the catalogued Prometheus metrics (`cluster_<primitive>_<subject>_<unit>`) | — | `nfr-observability` |
| SC-OBS-003 | L2 | ☐ | cardinality rule: keys/lock/election/instance names never appear as metric labels (only the bounded allowlist does) | — | `nfr-observability`, ADR-004 |
| SC-OBS-004 | L2 | ☐ | structured log events use the catalogued names (`cluster.<primitive>.<event>`) | — | `nfr-observability` |
| SC-OBS-005 | L3 | ☐ | every plugin emits every signal in the observability reference (cross-plugin completeness) | — | `nfr-observability` |

**Details:** [observability.md](./observability.md)

---

## Applicability per backend

**A backend does *not* run every scenario.** The `cluster-conformance` suite is shared,
but which scenarios a given backend exercises is filtered along three axes — so a
cache-only plugin author is *not* on the hook to implement or test all four primitives.

### Axis 1 — SDK-level scenarios run once, not per-backend

These exercise SDK code that sits *above* the backend trait, so no backend changes their
outcome. Most run once in the `cluster-conformance` crate itself; the exception is
SDK-default derivation, which runs once in the `cluster` gear's own test suite instead
(see the ownership note in [routing.md](./routing.md) — `cluster-conformance` never
depends on `cluster`, so it can't be the one to prove this):

| Area | Scenarios | Runs once in |
|---|---|---|
| Resolution & capability validation | [SC-RESV-*](./resolution.md) | `cluster-conformance` |
| Scoping wrappers | [SC-SCOP-*](./scoping.md) | `cluster-conformance` |
| Watch auto-restart combinator | [SC-REST-*](./restart.md) | `cluster-conformance` |
| Watch union shape (type-level) | [SC-WLU-001](./watch-lifecycle.md) | `cluster-conformance` |
| Name validation | [SC-NAME-*](./naming.md) | `cluster-conformance` |
| Dylint rule | [SC-LINT-*](./static-analysis.md) | `cluster-conformance` |
| SDK-default derivation | [SC-ROUTE-*](./routing.md) | `cluster` (the wiring gear) |

### Axis 2 — per-backend, only for primitives implemented *natively*

Every backend implements cache, so **every backend runs the cache suite**
([SC-CACHE-*](./cache.md)) against its real store. Leader/lock/SD obtained from the SDK
defaults are proven **once**, in the `cluster` gear's own test suite (SC-ROUTE-001); a
backend re-runs the `cluster-conformance` leader/lock/discovery suites **only when it
ships a native override**. From DESIGN §4.1:

| Backend | Runs natively (own conformance run) | Derived — proven once via SDK defaults |
|---|---|---|
| **NATS** | cache | leader, lock, service-discovery |
| **Postgres** | cache, lock (`pg_advisory_lock`) | leader, service-discovery |
| **Redis** | cache, lock (`SET NX EX`) | leader, service-discovery |
| **etcd** | cache, leader, lock (native) | service-discovery |
| **K8s** | cache, leader, lock, service-discovery (Lease/CRD) | — |
| **Standalone** | cache, leader, lock, service-discovery (in-process) | — |

So NATS runs ≈ one suite; K8s runs four. Neither runs the full catalog.

### Axis 3 — capability gates and layers filter within a run

- A scenario asserts its strict branch only when the backend *declares* the feature
  (`features()` / `consistency()`); otherwise it asserts the documented fallback. E.g.
  `linearizable` gates [SC-LEAD-002](./leader.md); `prefix_watch` selects
  [SC-CACHE-013](./cache.md) (native) vs. [SC-CACHE-014](./cache.md) (polyfill);
  `metadata_pushdown` selects server-side vs. client-side [SC-DISC-003](./discovery.md).
- **L4** scenarios run selectively — typically only for backends claiming strong
  guarantees (e.g. split-brain testing a `linearizable` backend), not every backend.
- **Observability** ([SC-OBS-*](./observability.md)): the contract assertions (001–004)
  ride the SDK's instrumented facade (largely once); per-plugin completeness (SC-OBS-005)
  is the per-backend obligation, run at L3.

## Ownership

- **L2** scenarios live in the shared `cluster-conformance` crate; each backend runs the
  subset that applies to it (see *Applicability per backend* above) — not all of them.
- **L3** scenarios live with the wiring crate (`cf-gears-cluster`) integration tests.
- **L4** scenarios are implemented per-plugin (Toxiproxy / `turmoil`) and
  documented in each plugin's own testing design ([TESTING-STRATEGY.md](../TESTING-STRATEGY.md) §5).

See [TESTING-STRATEGY.md](../TESTING-STRATEGY.md) for the layer definitions (§2), the
`cluster-conformance` crate layout (§4.1), tooling choices (§6–§7), and the still-open
lifecycle/contract gaps (§8) referenced by the rows above.
