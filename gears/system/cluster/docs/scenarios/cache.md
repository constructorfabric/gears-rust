# Cache — Scenario Details (`SC-CACHE-*`)

> Detail for §1 of the [scenario catalog](./README.md). Template: *Intent / Steps /
> Expected / Done-when*. Implemented by `cluster-conformance` (`cache.rs`).

**SC-CACHE-001 — `get` on an absent key** · L2 · ☑
- *Intent:* a miss is a normal result, not an error — consumers branch on `Option`, never on a thrown error.
- *Steps:* on a fresh backend, `get("absent")`.
- *Expected:* `Ok(None)`.
- *Done-when:* asserts `None`; confirms no error variant is returned for a missing key.

**SC-CACHE-002 — `put` then `get`** · L2 · ☑
- *Intent:* the basic store/load round-trip preserves bytes and assigns the first version.
- *Steps:* `put("k", b"v", None)`, then `get("k")`.
- *Expected:* `Some(CacheEntry { value: b"v", version: 1 })`.
- *Done-when:* asserts value equality and `version == 1` (versions start at 1, not 0).

**SC-CACHE-003 — version monotonicity** · L2 · ☑
- *Intent:* version is the basis for all optimistic concurrency; it must strictly increase and never be the 0 sentinel.
- *Steps:* `put` the same key N times, reading the version each time.
- *Expected:* each version strictly greater than the last; never 0.
- *Done-when:* asserts strict increase across ≥4 writes and `version != 0`.

**SC-CACHE-004 — `put_if_absent` atomicity** · L2 · ☑
- *Intent:* insert-if-absent is the create primitive for locks/leases/idempotent init — it must not overwrite.
- *Steps:* `put_if_absent("k", b"first")` then `put_if_absent("k", b"second")`.
- *Expected:* first → `Some(entry)`; second → `None`; final value is `b"first"`.
- *Done-when:* asserts the `Some`/`None` pair and that the original value survives.

**SC-CACHE-005 — CAS success** · L2 · ☑
- *Intent:* version-based compare-and-swap is the universal coordination operation; a matching version must commit.
- *Steps:* `put` a key, read its version `v`, `compare_and_swap(key, v, new)`.
- *Expected:* `Ok(entry)` with the new value and `version > v`.
- *Done-when:* asserts the new value and an advanced version.

**SC-CACHE-006 — CAS conflict surfaces current** · L2 · ☑
- *Intent:* a losing CAS must hand back the live entry so the consumer can re-read-and-retry without an extra round-trip.
- *Steps:* read version `v`, advance the key with another `put`, then CAS with the stale `v`.
- *Expected:* `Err(CasConflict { key, current: Some(live_entry) })`.
- *Done-when:* asserts the `CasConflict` variant, the key, and that `current` carries the live value.

**SC-CACHE-007 — `delete` reports existence** · L2 · ☑
- *Intent:* delete is idempotent and tells the caller whether it removed something.
- *Steps:* `put` then `delete` twice.
- *Expected:* first delete → `Ok(true)`, second → `Ok(false)`; subsequent `get` → `None`.
- *Note:* a backend that cannot determine prior existence MAY return `true` unconditionally (per the contract); the suite relaxes this assertion for such backends.

**SC-CACHE-008 — `compare_and_delete` owner guard** · L2 · ☑
- *Intent:* a holder releases only its *own* claim — the value-guarded delete underpins safe lock/lease release.
- *Steps:* store an owner token, attempt delete with a foreign token, then with the matching token, then against an absent key.
- *Expected:* foreign → `Ok(false)`; matching → `Ok(true)`; absent → `Ok(false)` (never an error).
- *Done-when:* asserts all three outcomes.

**SC-CACHE-009 — version reset on delete+recreate** · L2 · ☑
- *Intent:* regression guard for the version-reset caveat documented in [DESIGN.md §3.3](../DESIGN.md#33-api-contracts) — version resets to 1 on recreate, so a *version* guard would alias a successor's fresh claim; an *owner-token* guard must not.
- *Steps:* write+overwrite a key (version > 1), delete it, recreate via `put_if_absent`, then `compare_and_delete` with the *old* owner token.
- *Expected:* the recreated entry is `version == 1`; the stale-owner `compare_and_delete` returns `Ok(false)`.
- *Done-when:* asserts the reset to version 1 and that the predecessor cannot delete the successor.

**SC-CACHE-010 — TTL expiry emits `Expired`** · L2 · ☑
- *Intent:* TTL is the safety net for every cluster resource; expiry must remove the entry *and* notify watchers so a watch-driven waiter wakes.
- *Steps:* `watch(key)`, `put(key, v, Some(ttl))`, advance time past the TTL (`tokio::time::advance`).
- *Expected:* `get` returns `None`; the watcher receives `CacheEvent::Expired { key }`.
- *Done-when:* both the read-side eviction and the `Expired` emission are observed. Backed by `MemCache`'s background TTL sweeper (`fixture.rs`), which emits `Expired` on its own sweep tick rather than only on next access.

**SC-CACHE-011 — indefinite entries persist** · L2 · ☑
- *Intent:* a no-TTL value lives until explicitly deleted; backends that cannot persist indefinitely must declare it.
- *Steps:* `put(key, v, None)`, advance time well beyond any default TTL, `get`.
- *Expected:* the entry is still present. Backends without indefinite persistence (e.g. in-memory) document the constraint and the suite skips the persistence assertion for them.
- *Done-when:* asserts persistence on durable backends; records the documented exception otherwise.

**SC-CACHE-012 — exact watch ordering** · L2 · ☑
- *Intent:* per-key event order must match write order so consumers never apply updates out of sequence.
- *Steps:* `watch(key)`, then a known sequence of `put`/`delete` on that key.
- *Expected:* `Changed`/`Deleted` events arrive in exactly the write order.
- *Done-when:* asserts the event sequence equals the stimulus sequence.

**SC-CACHE-013 — native `watch_prefix`** · L2 · ☑ · gate `prefix_watch`
- *Intent:* prefix subscriptions enable reactive shard/topology observation without polling.
- *Steps:* `watch_prefix(p)`, then mutate several keys under and outside `p`.
- *Expected:* events for matching keys only. If `features().prefix_watch == false`, `watch_prefix` returns `Err(Unsupported { feature: "prefix_watch" })`.
- *Done-when:* asserts matching-key events when supported, or the `Unsupported` error otherwise.

**SC-CACHE-014 — polyfill prefix watch** · L2 · ☑ · gate `!prefix_watch`
- *Intent:* a backend without native prefix watch still gets prefix semantics via `PollingPrefixWatch`.
- *Steps:* spawn `PollingPrefixWatch` over a non-native backend, mutate keys under the prefix, advance time past the poll interval.
- *Expected:* synthesized `CacheEvent::Changed`/`Deleted` diffs for the changed keys.
- *Done-when:* asserts the diffed events match the mutations after one poll cycle.

**SC-CACHE-015 — at-most-once delivery** · L2 · ☑
- *Intent:* the contract is at-most-once (not exactly-once); no event is delivered twice to a subscriber for a single mutation.
- *Steps:* `watch(key)`, perform a single mutation, drain the stream.
- *Expected:* exactly one event for that mutation — never a duplicate.
- *Done-when:* asserts no duplicate events per key in normal (non-lagged) operation.

**SC-CACHE-016 — slow subscriber lags, not blocks** · L4 · ☐
- *Intent:* a subscriber falling behind must surface `Lagged` rather than apply backpressure to writers.
- *Steps:* `watch(key)`, flood writes faster than the subscriber drains, with the channel bounded.
- *Expected:* writers never block; the subscriber receives `CacheWatchEvent::Lagged { dropped }`.
- *Done-when:* asserts a `Lagged` event and that write throughput is unaffected. *(Needs a fault/throughput harness — hence L4.)*

**SC-CACHE-017 — reconnect surfaces `Reset`** · L4 · ☐
- *Intent:* after a dropped subscription is re-established, prior assumptions are invalid; the consumer must be told to re-read.
- *Steps:* establish a watch, sever the backend connection (Toxiproxy), restore it.
- *Expected:* `CacheWatchEvent::Reset` on resubscribe; re-reading via `get` recovers current state.
- *Done-when:* asserts a `Reset` event after reconnect against a real backend. *(Requires fault injection — L4, per-plugin.)*
