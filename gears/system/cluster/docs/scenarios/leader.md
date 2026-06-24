# Leader Election — Scenario Details (`SC-LEAD-*`)

> Detail for §2 of the [scenario catalog](./README.md). Template: *Intent / Steps /
> Expected / Done-when*. Most backends obtain leader election via
> `CasBasedLeaderElectionBackend` over their cache; native backends (K8s Lease, etcd)
> override it. Timing scenarios use `tokio::time` pause/advance.

**SC-LEAD-001 — single candidate becomes leader** · L2 · ☐
- *Intent:* with no contention, the sole participant must win — the baseline of the primitive.
- *Steps:* `elect("e")` on one node; await the first `Status` event.
- *Expected:* `LeaderStatus::Leader`; `is_leader()` returns `true`.
- *Done-when:* asserts the watch reaches `Leader` and the cached snapshot agrees.

**SC-LEAD-002 — at most one leader under contention** · L2 · ☐ · gate `linearizable`
- *Intent:* the core safety property — no split-brain when many candidates race.
- *Steps:* spawn N candidates against one backend; sample `status()` across all over a window.
- *Expected:* at no observed instant do two candidates report `Leader`.
- *Done-when:* asserts a single `Leader` at every sample. *Asserted only when `features().linearizable == true`; weak backends assert the documented advisory bound instead.*

**SC-LEAD-003 — automatic renewal** · L2 · ☐
- *Intent:* the leader keeps its claim with no consumer renewal code; a quiet leader does not lose leadership.
- *Steps:* `elect`, become leader, advance time across several renewal intervals without other activity.
- *Expected:* `status()` stays `Leader`; no `Lost` event fires.
- *Done-when:* asserts leadership persists across ≥3 renewal intervals.

**SC-LEAD-004 — graceful step-down** · L2 · ☐
- *Intent:* explicit resignation hands off within a round-trip rather than waiting for TTL.
- *Steps:* leader A `resign()`; a waiting candidate B awaits its next `Status`.
- *Expected:* A's `resign()` returns `Ok`; B observes `Leader` well within one TTL.
- *Done-when:* asserts B is promoted promptly after A resigns.

**SC-LEAD-005 — synchronous status snapshot** · L2 · ☐
- *Intent:* timer-driven workers need a non-async gate (`is_leader()`/`status()`) usable inside select arms.
- *Steps:* drive a transition, then call `status()`/`is_leader()` without awaiting the watch.
- *Expected:* the cached snapshot reflects the most recently observed transition; no I/O.
- *Done-when:* asserts the snapshot matches the last `Status` event.

**SC-LEAD-006 — `Status(Lost)` is transient** · L2 · ☐
- *Intent:* loss is not terminal — the watch auto-reenrolls without consumer re-`elect()` boilerplate.
- *Steps:* force a lost claim (e.g. TTL lapse under simulated stall), keep consuming the watch.
- *Expected:* a `Status(Lost)` followed by a later `Status(Leader|Follower)` on the *same* watch.
- *Done-when:* asserts re-enrollment without the consumer calling `elect()` again.

**SC-LEAD-007 — `ElectionConfig` validation** · L2 · ☐
- *Intent:* misconfigured timing must fail loudly at construction, not silently at runtime.
- *Steps:* `ElectionConfig::new(0, _)` and `ElectionConfig::new(_, 0)`.
- *Expected:* both return `Err(InvalidConfig)`; valid values derive `renewal_interval = ttl / (max_missed_renewals + 1)`.
- *Done-when:* asserts rejection of zero values and the derived interval for a valid pair.

**SC-LEAD-008 — weak-consistency guard** · L2 · ☐
- *Intent:* per ADR-009, the default constructor must refuse an `EventuallyConsistent` cache; the opt-in must warn.
- *Steps:* `CasBasedLeaderElectionBackend::new(eventually_consistent_cache)`; then `new_allow_weak_consistency(...)`.
- *Expected:* `new` → `Err(InvalidConfig)`; `new_allow_weak_consistency` → `Ok`, emitting a warning log (the `weak_consistency` field / split-brain message).
- *Done-when:* asserts the rejection and the warning (capture via `tracing-test`).

**SC-LEAD-009 — partition surfaces `Lost` within TTL** · L4 · ☐ · gate `linearizable`
- *Intent:* a partitioned leader must observe loss within the configured TTL (DESIGN §3.3 staleness bound).
- *Steps:* under `turmoil`, partition the leader from the backend; advance the simulated clock.
- *Expected:* the leader observes `Status(Lost)` within `TTL + observation_lag`.
- *Done-when:* asserts the `Lost` transition lands inside the bound. *(Deterministic simulation — L4.)*

**SC-LEAD-010 — no split-brain under partition** · L4 · ☐ · gate `linearizable`
- *Intent:* the headline guarantee `nfr-leader-guarantee` — zero split-brain under adversarial partition.
- *Steps:* under `turmoil`, model 10+ candidates across 3+ simulated nodes sharing one backend; inject partitions and message reorder/drop; sample every candidate's `status()` across the run.
- *Expected:* at no simulated instant do two candidates report `Leader`.
- *Done-when:* asserts zero concurrent-`Leader` samples across the run, replayable from a fixed seed. *(Deterministic simulation — L4, per linearizable backend.)*
