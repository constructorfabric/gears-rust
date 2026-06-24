# Lifecycle & Shutdown — Scenario Details (`SC-LIFE-*`)

> Detail for §8 of the [scenario catalog](./README.md). Template: *Intent / Steps /
> Expected / Done-when*. **Requires the wiring crate** (`cf-gears-cluster`) — these are
> L3 integration tests over a built `ClusterHandle`, not L2 conformance. Source of
> truth: DESIGN §3.13 shutdown sequence.

**SC-LIFE-001 — leader revocation order** · L3 · ☐
- *Intent:* a leader must lose confidence *before* it sees shutdown, so it never acts stale (`fr-shutdown-revoke`).
- *Steps:* build the handle, become leader on a watch, call `handle.stop()`.
- *Expected:* the active `LeaderWatch` receives `Status(Lost)` and *then* `Closed(Shutdown)` — two distinct events, in that order.
- *Done-when:* asserts both events and their ordering on the leader's watch.

**SC-LIFE-002 — in-flight `lock()` returns `Shutdown`** · L3 · ☐
- *Intent:* a blocked acquirer must distinguish "cluster going down" from "I timed out".
- *Steps:* start a blocking `lock("m", ttl, timeout)` that cannot acquire; call `stop()` while it waits.
- *Expected:* the waiter returns `Err(Shutdown)` — never `LockTimeout`.
- *Done-when:* asserts the `Shutdown` variant on the in-flight waiter.

**SC-LIFE-003 — cache & SD watches close with `Shutdown`** · L3 · ☐
- *Intent:* every active watch ends terminally with a clear cause on shutdown.
- *Steps:* open a cache `watch` and a service-discovery `watch`; call `stop()`.
- *Expected:* both receive `Closed(Shutdown)` as their terminal event.
- *Done-when:* asserts the `Closed(Shutdown)` on both watch types.

**SC-LIFE-004 — no remote release; TTL cleanup** · L3 · ☐
- *Intent:* shutdown makes no best-effort remote cleanup calls (`fr-shutdown-ttl-cleanup`); resources lapse via TTL.
- *Steps:* hold a lock / leader claim / registration; `stop()`; observe the backend state immediately after.
- *Expected:* no delete/release/deregister calls are issued; entries remain until their TTL lapses.
- *Done-when:* asserts the absence of remote-cleanup calls (e.g. via a recording backend) and TTL-bounded disappearance.

**SC-LIFE-005 — post-stop resolution fails** · L3 · ☐
- *Intent:* after teardown, backends are deregistered, so a late resolve fails cleanly.
- *Steps:* `stop()`, then `*V1::resolver(hub).profile(P).resolve()`.
- *Expected:* `Err(ProfileNotBound)`.
- *Done-when:* asserts resolution returns `ProfileNotBound` after stop.

**SC-LIFE-006 — omit-primitive auto-wrap; reject native non-cache** · L3 · ☐
- *Intent:* a single-backend profile is one line (omit non-cache primitives → SDK defaults over the cache); binding a native non-cache backend is rejected until the routing follow-up lands.
- *Steps:* build from YAML that (a) binds only `cache` and omits the rest, then (b) binds an explicit native `leader_election` provider.
- *Expected:* (a) the three omitted primitives resolve as `CasBased*`/`CacheBased*` over the cache; (b) startup fails with `InvalidConfig` naming the primitive.
- *Done-when:* asserts the auto-wrap resolves working defaults and the explicit native binding is rejected with `InvalidConfig`.
