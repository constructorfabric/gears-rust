# Scoping & Namespacing — Scenario Details (`SC-SCOP-*`)

> Detail for §6 of the [scenario catalog](./README.md). Template: *Intent / Steps /
> Expected / Done-when*. Exercises `*V1::scoped(prefix)` across primitives (DESIGN §3.8).

**SC-SCOP-001 — cache scoping prepends and strips** · L2 · ☐
- *Intent:* a consumer sees name-relative keys; the prefix is invisible inside its own code yet isolates it from other consumers on the same profile.
- *Steps:* `cache.scoped("eb")`; `put("k", v)`; `watch("k")`; trigger an event; inspect backend key and the event delivered to the consumer.
- *Expected:* the backend stores `eb/k`; the read-path event key the consumer sees is `k` (prefix stripped).
- *Done-when:* asserts the write-path prefixing and the read-path stripping round-trip.

**SC-SCOP-002 — scoping composes** · L2 · ☐
- *Intent:* a sharded gear nests a per-shard namespace inside its per-gear namespace.
- *Steps:* `cache.scoped("eb").scoped("shard-0")`; `put("k", v)`.
- *Expected:* effective backend key is `eb/shard-0/k`; reads strip both layers back to `k`.
- *Done-when:* asserts the composed prefix on write and full stripping on read.

**SC-SCOP-003 — invalid prefix rejected** · L2 · ☐
- *Intent:* a malformed scope must fail at construction, not corrupt keys silently.
- *Steps:* `scoped("bad prefix!")` (violates `[a-zA-Z0-9_/-]+`).
- *Expected:* `Err(InvalidName { name, reason })`.
- *Done-when:* asserts the `InvalidName` error for an invalid prefix and success for a valid one.

**SC-SCOP-004 — polyfill composes with scoping** · L2 · ☐ · gate `!prefix_watch`
- *Intent:* the polling polyfill emits full backend keys like a native prefix watch, so scoping must strip them on the read path.
- *Steps:* on a non-prefix-watch backend, `cache.scoped("eb")`, then a polyfilled `watch_prefix("")`; mutate keys under the scope.
- *Expected:* events arrive with scope-relative keys (e.g. `k`, not `eb/k`).
- *Done-when:* asserts the `ScopedCacheBackend` strips the prefix from polyfill-emitted keys.

**SC-SCOP-005 — election names scoped** · L2 · ☐
- *Intent:* two gears on the same profile must not collide on election names; scoping isolates them invisibly.
- *Steps:* `leader.scoped("eb")`; `elect("shard-leader")`.
- *Expected:* the backend election key is `eb/shard-leader`; the consumer used the bare name. (`LeaderWatch` carries no name, so there is no read-path strip — see §3.8.)
- *Done-when:* asserts the backend sees the prefixed election name.

**SC-SCOP-006 — lock names scoped** · L2 · ☐
- *Intent:* lock names are namespaced per consumer the same way.
- *Steps:* `lock.scoped("eb")`; `try_lock("budget", ttl)`.
- *Expected:* the backend lock key is `eb/budget`. (`LockGuard` is opaque — no read-path strip.)
- *Done-when:* asserts the backend sees the prefixed lock name.
