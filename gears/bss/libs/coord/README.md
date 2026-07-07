# `coord`

> Shared BSS coordination primitives. Today it hosts **one** thing: a DB-backed
> distributed **lease** — a *single-active* guard for jobs and runs.

This document explains what the lease does and, more importantly, **when it is and
isn't the right tool**, so you can decide if it fits your case before adopting it.

---

## The problem it solves

You run several replicas of a service (or several ticks of a scheduled job), and
each one might try to do the same unit of work at the same time:

- one recognition run per `(tenant, period)`,
- one nightly close,
- a singleton reaper / ticker / outbox-drainer.

You want **at most one** of them to actually run a given unit at a time — without
standing up Redis, ZooKeeper, or a dedicated lock service. `coord` gives you that
using **only the database you already have** (Postgres in prod, SQLite in tests).

It was ported from the account-management `am_leases` pattern and generalized for
reuse by any BSS gear. It works **entirely within the existing toolkit SecureORM**
— the `coord_leases` row is unscoped process-coordination state accessed via
`AccessScope::allow_all()` — so adopting it needs **no toolkit / `gears-rust`
changes**.

---

## Mental model

A single table, `coord_leases`, with one row per coordination key:

| column         | meaning                                                            |
| -------------- | ------------------------------------------------------------------ |
| `key` (PK)     | the coordination domain, an arbitrary string you choose            |
| `locked_by`    | the current holder's UUID (`NULL` when free)                       |
| `locked_until` | TTL deadline — the lease is *live* while `locked_until > NOW()`    |
| `attempts`     | forensic steal counter (a flapping cluster shows a high-water value) |

- **Acquire** = INSERT a fresh row (free slot) **or** steal an expired one
  (`UPDATE … WHERE locked_until < NOW()`), all inside one `SERIALIZABLE` retry
  transaction. Exactly one worker wins; the loser gets `CoordError::LeaseHeld`.
- The lease is **time-bounded**. A holder that crashes without releasing is
  automatically reclaimable once `locked_until` passes — **no manual cleanup, no
  permanent deadlock.**
- Time anchors on the **DB clock** (steal / renew / fence filters all use the
  database's `NOW()`). So clock drift between workers can only ever cause a
  *conservative false negative* (you fail to acquire a slot that was actually
  free), **never an unsafe double-acquire.**

---

## API at a glance

```rust,ignore
let mgr = coord::LeaseManager::new(db.clone());

match mgr.acquire("recognition-run:{tenant}:{period}", ttl).await {
    Ok(guard) => {
        // ... do the single-active work ...
        guard.release().await?;            // success: free the slot + reset `attempts`
    }
    Err(coord::CoordError::LeaseHeld) => { /* a peer is already running — skip */ }
    Err(e) => return Err(e.into()),        // DB failure
}
```

| Item | Purpose |
| --- | --- |
| `LeaseManager::new(db)` / `.acquire(key, ttl)` | acquire-or-steal; returns a `LeaseGuard` or `LeaseHeld` |
| `LeaseGuard::release()` | success path: free the slot, reset `attempts` |
| `LeaseGuard::release_with_retry()` | recoverable-failure path: free the slot, **keep** the `attempts` streak |
| `LeaseGuard::renew(ttl)` | push `locked_until` forward; zero rows ⇒ `LeaseLost` |
| `LeaseGuard::with_ack_in_tx(extract_db_err, f)` | run `f` **and** a lease-validity fence SELECT in ONE serializable tx — a mid-flight steal ⇒ `AckError::LeaseLost` + rollback (your writes never commit under a lost lease) |
| `LeaseGuard::spawn_renewal(period)` → `RenewalHandle` | background heartbeat; emits `RenewalState::Lost` on a `watch` channel so a long holder can pre-empt itself |
| `CoordError { LeaseHeld, LeaseLost, Db }` | acquire / renew / release outcomes |
| `AckError<E> { LeaseLost, Work(E), Db }` | `with_ack_in_tx` outcome envelope |
| `coord::migration::Migration::in_schema("bss")` / `::unqualified()` | the `coord_leases` table migration (qualified for a named PG schema, or bare for SQLite / `search_path`) |

---

## Guarantees

- **At most one live holder per key** (subject to the TTL caveat below).
- **Crash safety via TTL**: a dead holder's slot auto-reclaims after `locked_until`
  — no orphaned locks, no manual recovery.
- **DB-clock anchored**: worker clock drift cannot cause an unsafe double-steal.
- **Atomic check-and-commit** (`with_ack_in_tx`): your DB work and the "do I still
  hold the lease?" check commit atomically. You cannot commit results under a lease
  a peer has already stolen — the check either aborts on serialization conflict (and
  retries) or sees zero rows and rolls back as `LeaseLost`. This is an in-transaction
  re-check, **not** a monotonic fencing token (`locked_by` is a fresh per-acquire
  UUID); for effects outside the DB transaction see Non-guarantees below.

## Non-guarantees — read these before relying on it

- **It is a lease, not a fencing token for *external* side effects.** Only DB
  writes folded into `with_ack_in_tx` are protected by the steal fence. Between
  "I hold the lease" and an external effect (an HTTP call, a publish outside the
  DB transaction) the lease can expire or be stolen. For non-DB effects you still
  need **idempotency**.
- **TTL must exceed the work, or you must renew.** If a job outruns its TTL without
  `spawn_renewal` / `renew`, a peer can steal mid-run and you get two concurrent
  holders. Choose `ttl > expected work`, or heartbeat with `period ≈ ttl / 3`
  (survives one missed tick before expiry).
- **No `Drop`-time release.** There is no async I/O in `Drop`, so always call
  `release()` / `release_with_retry()` explicitly; an abandoned guard relies on
  the TTL fallback.
- **Single database.** It coordinates workers sharing one DB. It does **not**
  coordinate across databases, clusters, or regions.
- **Not a queue.** No fairness, no ordering, no backlog. The loser simply gets
  `LeaseHeld` and decides for itself whether to skip or retry later.
- **Postgres or SQLite only.** MySQL is rejected at runtime (returns a `Db` error,
  not a panic).

---

## Use `coord` when…

- You need **single-active** semantics for a job/run across replicas, and you
  already have Postgres (or SQLite for tests).
- The protected critical section is **primarily DB work** (so `with_ack_in_tx`
  fences it), or any external effects can be made idempotent.
- You want **crash-safe auto-reclaim** (TTL) rather than manual lock cleanup.
- Typical fits: one recognition run per `(tenant, period)`; a singleton
  reaper/ticker; a nightly close that must not double-run.

## Don't use `coord` when… (reach for something else)

| If you need… | use instead |
| --- | --- |
| Hard mutual exclusion for **non-idempotent external side effects**, zero tolerance for a TTL-expiry overlap | a real fencing-token protocol, or idempotency at the effect itself |
| **Low-latency, high-churn** locking (thousands/sec) | Redis / a dedicated lock service — a DB-row lease is the wrong tool |
| **Fairness, ordering, or a backlog** of work | a real queue |
| Coordination **across databases / regions** | out of scope for this primitive |
| Mutual exclusion **within a single process** | `tokio::sync::Mutex` (no DB needed) |

---

## Adopting it

1. Add `coord` as a dependency and register the migration in your `Migrator`:
   `Box::new(coord::migration::Migration::in_schema("bss"))` (or `::unqualified()`
   for SQLite / a single-schema `search_path`).
2. Build `LeaseManager::new(db)` and bracket the job: `acquire` → run → `release`.
3. Map `CoordError::LeaseHeld` onto your gear's "already running" outcome; surface
   `Db` / `LeaseLost` as appropriate (the crate is deliberately free of any gear's
   domain error taxonomy — the mapping lives at the call site).
4. For **long** jobs, `spawn_renewal(ttl / 3)` and watch for `RenewalState::Lost`
   to self-pre-empt. For **DB-critical** sections, prefer `with_ack_in_tx` so the
   commit is fenced against a concurrent steal.

### Fenced-commit sketch

```rust,ignore
// Work + lease-validity check commit atomically; a peer steal mid-flight
// rolls the work back as AckError::LeaseLost (never committed under a lost lease).
let out = guard
    .with_ack_in_tx(
        |e: &MyErr| e.as_db_err(),          // classify retryable contention in your work error
        |tx| Box::pin(async move { do_db_work(tx).await }),
    )
    .await?;
```

---

## Design notes / provenance

- Ported from account-management `am_leases`, generalized for cross-gear reuse:
  no domain-error coupling (the consumer maps `CoordError`), a `Dialect` enum
  instead of the AM original's panicking `&str` engine match, and a
  schema-qualifiable migration.
- `attempts` is a forensic steal counter — alert on `attempts >= 3` to catch a
  flapping cluster. `release` resets it; `release_with_retry` preserves the streak.
- Single-table, single-DB, TTL-reclaimable by design: it is the *smallest* thing
  that delivers single-active coordination on the database you already run.
