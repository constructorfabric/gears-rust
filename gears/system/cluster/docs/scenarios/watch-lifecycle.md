# Watch Lifecycle Uniformity — Scenario Details (`SC-WLU-*`)

> Detail for §10 of the [scenario catalog](./README.md). Template: *Intent / Steps /
> Expected / Done-when*. These assert the *uniform* watch contract (ADR-003) across all
> three watches; per-primitive instances live in [cache.md](./cache.md) (SC-CACHE-016/017)
> and [discovery.md](./discovery.md) (SC-DISC-009). The recovery action differs per
> primitive but the *signal shape* and the *obligation to recover* are identical.

**SC-WLU-001 — uniform union shape** · L2 · ☐
- *Intent:* one mental model for watches — every watch event is `{value-variant, Lagged, Reset, Closed}`, infallible at the type level (no `Result`-returning `changed()`).
- *Steps:* type-level / structural check of `CacheWatchEvent`, `LeaderWatchEvent`, `ServiceWatchEvent`.
- *Expected:* each carries exactly `Event/Status/Change`, `Lagged { dropped }`, `Reset`, `Closed(ClusterError)`; all `#[non_exhaustive]`; none expose a fallible `changed()`.
- *Done-when:* asserts the three enums share the variant set (a compile-time/structural test, no backend needed).

**SC-WLU-002 — `Lagged` under backpressure (all three)** · L4 · ☐
- *Intent:* a slow subscriber on *any* watch is told it fell behind rather than blocking the producer.
- *Steps:* for each of cache / leader / SD, subscribe, then drive events faster than the subscriber drains against a bounded channel.
- *Expected:* each watch yields `Lagged { dropped }`; the producer is never blocked.
- *Done-when:* asserts a `Lagged` event on every watch type. *(Generalizes SC-CACHE-016; L4 — needs a throughput harness.)*

**SC-WLU-003 — `Reset` and recovery (all three)** · L4 · ☐
- *Intent:* after a re-established subscription, the consumer must re-sync — and the recovery recipe is the same regardless of primitive.
- *Steps:* for each watch, sever and restore the subscription (Toxiproxy / induced reconnect); follow the documented recovery.
- *Expected:* each yields `Reset`; recovery succeeds — cache re-reads via `get`, leader awaits the next `Status`, SD re-reads via `discover`.
- *Done-when:* asserts `Reset` on each watch and that the per-primitive recovery restores correct state. *(Generalizes SC-CACHE-017 / SC-DISC-009.)*

**SC-WLU-004 — terminal `Closed`; transients retried internally** · L2 · ☐
- *Intent:* terminal failure is explicit (`Closed(err)`) and final; transient backend errors (`ConnectionLost`/`Timeout`/`ResourceExhausted`) are retried by the watch's background task and never surface as events.
- *Steps:* for each watch, induce a terminal cause (e.g. via the scripted source) and, separately, a transient blip.
- *Expected:* terminal → exactly one `Closed(err)`, no further events; transient → no event emitted (silently retried).
- *Done-when:* asserts the single terminal `Closed` and the absence of any event for transients, on each watch type.
