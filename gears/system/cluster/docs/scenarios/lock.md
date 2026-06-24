# Distributed Lock — Scenario Details (`SC-LOCK-*`)

> Detail for §3 of the [scenario catalog](./README.md). Template: *Intent / Steps /
> Expected / Done-when*. Most backends obtain locking via
> `CasBasedDistributedLockBackend` over their cache; Redis/Postgres/etcd may override
> with a native lock. Timeout/TTL scenarios use `tokio::time` pause/advance.

**SC-LOCK-001 — `try_lock` contention** · L2 · ☐
- *Intent:* non-blocking acquisition fails fast so callers can shed load.
- *Steps:* holder A `try_lock("m", ttl)`; B `try_lock("m", ttl)` while A holds.
- *Expected:* A → `Ok(LockGuard)`; B → `Err(LockContended { name: "m" })`.
- *Done-when:* asserts A succeeds and B is contended.

**SC-LOCK-002 — `lock` blocks then times out** · L2 · ☐
- *Intent:* blocking acquisition waits up to `timeout`, then reports a timeout (distinct from contention).
- *Steps:* A holds "m"; B `lock("m", ttl, timeout)`; advance time past `timeout` without A releasing.
- *Expected:* B → `Err(LockTimeout { name: "m", waited })` with `waited ≈ timeout`.
- *Done-when:* asserts the `LockTimeout` variant and a plausible `waited`.

**SC-LOCK-003 — crashed-holder TTL recovery** · L2 · ☐
- *Intent:* a holder that crashes without releasing must not block others past its TTL.
- *Steps:* A acquires "m" then drops its guard without `release()`; advance time past the TTL; B `try_lock("m")`.
- *Expected:* B acquires once the TTL lapses.
- *Done-when:* asserts B acquires after TTL expiry with no explicit release by A.

**SC-LOCK-004 — explicit release wakes a waiter** · L2 · ☐
- *Intent:* `release()` frees the lock immediately so a blocked waiter proceeds without waiting for TTL.
- *Steps:* A holds "m"; B begins `lock("m", ttl, timeout)`; A `release()`.
- *Expected:* B acquires shortly after A releases, well within `timeout`.
- *Done-when:* asserts B acquires promptly post-release.

**SC-LOCK-005 — `renew` extends; expired `renew` fails** · L2 · ☐
- *Intent:* long operations extend the lease; renewing a lapsed lease must tell the holder it lost the lock.
- *Steps:* acquire with a short TTL, `renew(new_ttl)` before expiry (succeeds); separately, let a lock expire then `renew`.
- *Expected:* timely `renew` → `Ok`; renewing after expiry → `Err(LockExpired { name })`.
- *Done-when:* asserts both the successful extension and the `LockExpired` on a lapsed lease.

**SC-LOCK-006 — foreign-holder release guard** · L2 · ⊘ (covered indirectly)
- *Intent:* only the owner may release — a non-holder must not free someone else's lock (owner-token / CAS guard).
- *Steps:* not exercised directly — `DistributedLockBackend` does not expose the owner token, so there is no seam to drive a foreign-release attempt at this layer.
- *Expected/Done-when:* covered indirectly by [SC-CACHE-008/009](./cache.md), which exercise the same value/owner-token guard the SDK-default lock backend (`CasBasedDistributedLockBackend`) is built on. A backend with a native (non-cache-derived) lock implementation is responsible for its own equivalent guard test in its L3 per-plugin design (TESTING-STRATEGY.md §5).

**SC-LOCK-007 — `Drop` performs no I/O** · L2 · ☐
- *Intent:* per ADR-002, dropping a `LockGuard` must not make remote calls — TTL is the only safety net.
- *Steps:* acquire then drop the guard; observe that the lock is *not* released before its TTL.
- *Expected:* the lock remains held (from peers' view) until the TTL lapses; no release I/O on drop.
- *Done-when:* asserts a dropped guard does not eagerly free the lock (contrast with SC-LOCK-004's explicit release).

**SC-LOCK-008 — blocked waiter woken on release notification** · L4 · ☐
- *Intent:* the CAS-based default uses a watch to wake waiters promptly rather than busy-polling.
- *Steps:* B blocks on `lock("m", ...)`; A releases; measure B's wake latency under induced watch jitter.
- *Expected:* B wakes on the release notification within a small bound.
- *Done-when:* asserts prompt wake under fault injection. *(L4 — needs a timing/fault harness.)*
