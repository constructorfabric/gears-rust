---
status: accepted
date: 2026-04-02
---

# ADR-002: Async Boundary and No Remote I/O in Critical Sections

**ID**: `cpt-cf-clst-adr-async-boundary-no-remote-critical`

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Sync Drop with block_on](#option-1-sync-drop-with-block_on)
  - [Option 2: Detached task in Drop](#option-2-detached-task-in-drop)
  - [Option 3: Reaper channel](#option-3-reaper-channel)
  - [Option 4: No-op Drop + explicit async release (CHOSEN)](#option-4-no-op-drop--explicit-async-release-chosen)
  - [Option 5: Keep fencing tokens](#option-5-keep-fencing-tokens)
  - [Option 6: Remove fencing tokens, enforce no-remote-in-critical-section (CHOSEN)](#option-6-remove-fencing-tokens-enforce-no-remote-in-critical-section-chosen)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

The cluster gears the Gears middleware's abstraction for cluster coordination: distributed cache, leader election, distributed locks, and service discovery. Every meaningful cluster operation is a remote call over the network — there is no "in-process" version of a distributed lock, and the "standalone" provider is a testing/development fixture, not the production target.

Two interrelated design questions emerged during review:

1. **How should resource handles (`LockGuard`, `ServiceHandle`, `LeaderWatch`) clean up on scope exit?** The Rust idiom is RAII via `Drop`. But these handles represent remote resources whose release requires a network call. `Drop` is a synchronous function; Gears use Tokio and does not maintain dedicated OS threads for blocking I/O. Sync `Drop` cannot perform async network I/O reliably: `block_on` panics in a Tokio runtime, spawning a detached task is cancelled on shutdown and fails silently, and routing to a background reaper pushes the ordering problem elsewhere.

2. **Do distributed locks need fencing tokens?** The classic Kleppmann argument says yes: a lock holder might pause and then send stale writes to the guarded resource. Pause causes that can exceed a TTL include — non-exhaustively — GC stop-the-world, OS swap-in stalls under memory pressure, partial network partition, VM suspend / hypervisor freeze (common under node overcommit), CPU overcommit / kernel scheduler stalls, container CPU throttling (cgroup CFS), NUMA / memory-pressure stalls, and host hibernation / live migration. A monotonic fencing token lets the guarded resource reject stale writes. But the scenario in which a fencing token actually helps requires *both* an unbounded pause of the holder *and* a critical section that contains remote I/O (otherwise there is no stale writer to guard against). The first half holds in any deployment; the second half is what we eliminate at the architectural level.

Both questions have the same root: **Gears middleware is an async-only, cooperatively-scheduled system where every meaningful cluster operation is remote**. The design must acknowledge this consistently instead of borrowing idioms from sync/blocking systems.

## Decision Drivers

- Rust's `Drop` trait is synchronous and cannot perform async I/O without unsafe tricks that compromise correctness.
- Tokio does not allow `block_on` inside a runtime context (panics at runtime).
- Detached tasks (`tokio::spawn` of a release future) are cancelled during runtime shutdown, causing silent leaks in exactly the scenarios release matters most.
- TTL on the backend is an unavoidable safety net for every distributed resource; it handles process crash, panic, and forgotten release identically.
- The Kleppmann fencing-token argument requires an "unbounded pause of the lock holder while still able to reach the guarded resource". Async + timeouts cannot bound *every* pause source — VM suspend or kernel-scheduler stalls freeze the entire runtime, including any timeout futures. The argument that eliminates the stale-writer scenario is therefore the no-remote-in-critical-section rule: it removes the *guarded resource access* from the critical section, so even an unbounded pause of arbitrary cause cannot produce a stale writer.
- Gears already enforces architectural constraints via `cargo gears lint` (layer rules, no-serde-in-contracts); adding one more rule is cheap.

## Considered Options

1. **Sync `Drop` with `block_on`**: Perform the release synchronously inside `Drop`.
2. **Detached-task `Drop`**: Spawn a Tokio task from `Drop` that performs the async release.
3. **Reaper channel**: `Drop` sends a release request to a background reaper task via a channel.
4. **No-op `Drop` + explicit async release**: `Drop` does nothing. Consumers call `async fn release(self)` explicitly. TTL is the safety net.
5. **Keep fencing tokens in the public API**: Provide monotonic tokens on `LockGuard` for consumers to pass to guarded resources.
6. **Remove fencing tokens; enforce no-remote-in-critical-section**: Eliminate the scenario fencing protects against at the architectural level.

## Decision Outcome

Chosen options: **Option 4** (no-op `Drop` + explicit async release) combined with **Option 6** (remove fencing tokens; enforce no-remote-in-critical-section via `cargo gears lint`).

The `LockGuard`, `ServiceHandle`, and `LeaderWatch` types have no-op `Drop` implementations. Remote cleanup is exposed as explicit async methods:

- `LockGuard::release(self) -> Result<(), ClusterError>`
- `ServiceHandle::deregister(self) -> Result<(), ClusterError>`
- `LeaderWatch::resign(self) -> Result<(), ClusterError>`

Consumers that forget to call these rely on the backend TTL for eventual cleanup. The TTL is bounded (seconds, not hours) and identical in behavior to the process-crash case.

The `LockGuard` does not expose a fencing token. Instead, Gears enforces two architectural principles via a architecture lint rule:

1. All cluster operations are `async fn` invoked within Tokio. Consumers SHOULD wrap them with `tokio::time::timeout` to bound blocking on network calls.
2. Code protected by a `LockGuard` (or inside a database transaction) MUST NOT make additional remote I/O calls. Remote effects MUST occur before `try_lock` or after `release`, never between them.

Together, these principles eliminate the scenario Kleppmann's fencing tokens protect against. The two principles play different roles, and only the second is cause-agnostic:

- **Async + timeouts** bound the *normal-operation* case: a task waiting on a slow or hung backend call is timeout-bounded within the runtime's own time, so consumer code does not block indefinitely on a single backend round-trip. This does NOT bound process-wide pauses (VM suspend, OS swap-in, container CPU throttling, hypervisor live migration) — when the entire runtime is frozen, timeouts are frozen with it.
- **No-remote-in-critical-section** is the cause-agnostic safety property. Regardless of why the holder paused — GC, VM suspend, kernel stall, network partition, host hibernation — the critical section contains no remote effects, so no in-flight write to the guarded resource exists to "go stale". Writes against the resource happen via `ClusterCacheV1::compare_and_swap` *after* the lock is released; the CAS is the actual correctness gate, and a `CasConflict` cleanly catches the case where the holder's pre-pause assumptions about the resource state have been invalidated by a successor.

The TTL safety net composes with both: any pause of duration ≥ TTL produces an automatic backend-side lock release, so a successor can acquire and proceed without coordination from the paused holder. When the paused holder eventually resumes, its `release().await` sees the lock no longer held (release is idempotent / a no-op against a foreign holder via delete-if-still-holder CAS), its `renew().await` returns `LockExpired`, and any write it attempts via cache CAS fails with `CasConflict` if the resource state has moved.

This coverage is intentional and complete: bounded pauses (GC) and unbounded pauses (VM suspend) are handled by the same mechanism, because the mechanism doesn't depend on the pause being bounded.

The architecture lint rule that enforces "no remote I/O in critical sections" is initially scoped to the four cluster backend traits within `try_lock` / `release` scopes. Database-transaction enforcement (treating an open `sqlx::Transaction` as a critical section) is deferred to a follow-up rule extension once the wiring crate and consumer migrations land — the lint surface for cluster locks alone is the high-value target and worth shipping first.

### Consequences

- Consumers MUST explicitly call `release().await` (or `deregister().await`, `resign().await`) for timely handoff. Forgetting the call leaks the resource until TTL — a bounded but undesirable delay. Linters and code review catch most forgotten calls.
- The `LockGuard` API is simpler (no `fencing_token()` method). Provider implementations are significantly simpler: no Lua `INCR` (Redis), no sequence table (Postgres), no annotation CAS (K8s), no `mod_revision` coupling (etcd).
- Consumers whose critical section contained remote I/O must restructure: compute locally, release the lock, then apply remote effects. The architecture lint rule flags violations at compile time.
- The TTL becomes the operationally-visible bound on forgotten cleanup. Monitoring SHOULD alert on unusually long TTL-expiry rates (indicator of a bug where release is consistently missed).
- If a future consumer has a genuine fencing need for a resource with its own concurrency control (e.g., an external storage layer with fencing support), they can generate a monotonic sequence via `ClusterCache::compare_and_swap` at the application level. The primitive does not need to be in the lock API.

### Confirmation

- Unit tests verify that `Drop` on `LockGuard`, `ServiceHandle`, and `LeaderWatch` performs no I/O (no panics under Tokio; no detached tasks spawned).
- Integration tests verify that forgotten release results in TTL-bounded cleanup (lock becomes available within TTL+epsilon).
- Architecture lint rule `no-remote-in-critical-section` flags violations in unit tests with known-bad inputs; passes on known-good inputs.
- Cluster provider implementations are reviewed to confirm no fencing-token generation exists in production code paths.
- Unbounded-pause coverage test (per-backend integration suite): simulate a holder pause longer than TTL by suspending the holder process or pausing its async runtime via `pause_runtime_for(ttl + epsilon)`. Assert: (a) backend releases the lock at TTL, (b) a successor acquires within `epsilon`, (c) on resume, the original holder's `release().await` is a benign no-op against the foreign holder, (d) the original holder's subsequent CAS write attempt against the guarded resource returns `CasConflict` when the successor has changed it, and `Ok` only when the resource state matches the holder's pre-pause expected_version.

## Pros and Cons of the Options

### Option 1: Sync Drop with block_on

`Drop for LockGuard { fn drop(&mut self) { tokio::runtime::Handle::current().block_on(self.release_remote()) } }`

- Good, because the consumer sees conventional Rust RAII semantics — drop just works.
- Bad, because `block_on` inside a Tokio runtime panics at runtime (`"Cannot start a runtime from within a runtime"`). This is not a hypothetical; it fires the first time a `LockGuard` is dropped inside an async function.
- Bad, because even if we used a dedicated blocking thread pool, Gears don't have one.
- Bad, because the call is invisible in async stack traces; debugging a deadlock inside `Drop` is painful.
- Bad, because the `Drop` signature has no way to propagate errors.

### Option 2: Detached task in Drop

`Drop for LockGuard { fn drop(&mut self) { tokio::spawn(async move { ... release ... }); } }`

- Good, because the consumer still sees RAII semantics from their perspective.
- Bad, because detached tasks are cancelled when the Tokio runtime shuts down. In a graceful shutdown sequence, the release task may be cancelled before the release completes — the exact scenario where reliable release matters most.
- Bad, because errors from the release are silently swallowed. If the provider connection is gone, the release fails and the consumer never knows.
- Bad, because the spawned task outlives the original scope, consuming the Tokio executor's resources until it completes (or is cancelled). A rapid churn of locks produces a flood of detached release tasks.
- Bad, because it hides a concurrency bug: after `Drop`, the consumer believes the lock is released, but the release task may still be queued. Another acquire attempt from the same consumer might race with its own pending release.

### Option 3: Reaper channel

`Drop for LockGuard { fn drop(&mut self) { self.reaper_tx.send(release_request).ok(); } }`

- Good, because `Drop` is fast and non-blocking.
- Good, because the reaper task can batch releases for efficiency.
- Bad, because it adds an unspecified component (the reaper task) to every provider. Its lifecycle, shutdown semantics, and backpressure behavior become part of the contract.
- Bad, because if the reaper channel is full or closed, releases are dropped silently.
- Bad, because the reaper's shutdown ordering with respect to the cluster's own `shutdown()` is yet another problem to specify — the very problem we're trying to avoid.
- Bad, because it centralizes a failure point: a hung reaper blocks every release.

### Option 4: No-op Drop + explicit async release (CHOSEN)

`impl Drop for LockGuard { fn drop(&mut self) { /* no-op */ } }` + `async fn release(self) -> Result<()>`

- Good, because `Drop` does no I/O — it cannot panic, leak tasks, or hide errors.
- Good, because the contract is explicit: `release().await` is the release path, and errors are first-class.
- Good, because the consumer can handle release failures (log, retry, escalate) rather than have them silently swallowed.
- Good, because the implementation is trivial for every provider — no `block_on`, no detached task, no reaper.
- Good, because the TTL safety net handles panic, crash, and forgotten release uniformly — no special case.
- Bad, because consumers can forget to call `release().await`. Linter and code review catch most cases; TTL bounds the worst case.
- Bad, because it deviates from Rust's RAII norm. Mitigated by documentation and the observation that remote resources are fundamentally different from local ones — the norm doesn't apply cleanly.
- Neutral, because the consumer-visible verbosity is one line per release site (`guard.release().await?`). Net cost is low.

### Option 5: Keep fencing tokens

- Good, because it preserves the option for consumers who want Kleppmann-style fencing on guarded resources.
- Bad, because zero current Gears consumers use fencing tokens. The feature is implemented for hypothetical future need.
- Bad, because every provider pays the implementation cost: Redis Lua `INCR` counter for tokens, Postgres sequence or dedicated token table, K8s annotation CAS, NATS revision coupling. These are non-trivial.
- Bad, because exposing a raw `u64` invites misuse — consumers may compare tokens or use them for purposes other than fencing.
- Bad, because it implies a guarantee (fencing is safe here) that is only meaningful if the guarded resource supports fencing. Off-the-shelf databases do not.

### Option 6: Remove fencing tokens, enforce no-remote-in-critical-section (CHOSEN)

- Good, because it eliminates the scenario fencing protects against at the architectural level, not at the API level.
- Good, because the principle (no remote I/O inside critical sections) is a good architectural rule independent of fencing — it prevents deadlocks, bounds critical section duration, and simplifies reasoning about partial-failure scenarios.
- Good, because compile-time enforcement (`cargo gears lint`) catches violations early. Existing workspace architecture lint rules establish the pattern.
- Good, because it removes significant provider implementation complexity.
- Good, because if a future consumer needs fencing for a specific external resource, they can implement it at the application level via `ClusterCache::compare_and_swap`.
- Bad, because it imposes a restriction on consumers: their critical sections cannot contain remote I/O. Consumers whose existing patterns violate this must refactor.
- Bad, because the architecture lint rule has to distinguish "remote" traits from "local" traits. Requires a maintained registry of remote-trait signatures.
- Neutral, because "no remote I/O inside critical sections" is an architectural best practice regardless; formalizing it strengthens the system.

## More Information

**Good pattern — using a cluster lock with no remote I/O inside the critical section:**

```rust
async fn update_tenant_rate_limit(
    lock: DistributedLockV1,
    tenant_id: &str,
) -> Result<()> {
    // 1. Acquire the lock with a bounded timeout
    let lock_name = format!("oagw/rate-limit/{}", tenant_id);
    let guard = timeout(
        Duration::from_secs(2),
        cluster.distributed_lock().try_lock(&lock_name, Duration::from_secs(1)),
    ).await??;

    // 2. Read current state (cache.get is a remote call — forbidden inside critical section)
    // To respect the no-remote-in-critical-section rule, move the read OUTSIDE the lock scope.
    //
    // But wait: how do we read state transactionally with the lock? Use CAS on the cache:
    //   - The cache CAS is ONE operation, atomic on the backend.
    //   - We don't hold a lock around it; we retry on conflict.
    //
    // So actually, for "increment a counter atomically," we don't need a lock at all.
    // We just use cache::compare_and_swap in a retry loop.
    // Release the lock — we don't need it for the compute path.
    guard.release().await?;

    // 3. Apply remote effect via CAS (lock-free)
    loop {
        let current = cluster.cache().get(&format!("oagw/counter/{}", tenant_id)).await?;
        let (current_val, expected_ver) = match current {
            Some(entry) => (entry.value, entry.version),
            None => (vec![0; 8], 0),
        };
        let new_val = increment(&current_val);
        match cluster.cache().compare_and_swap(
            &format!("oagw/counter/{}", tenant_id),
            expected_ver,
            &new_val,
            None,
        ).await {
            Ok(_) => return Ok(()),
            Err(ClusterError::CasConflict { .. }) => continue,
            Err(e) => return Err(e.into()),
        }
    }
}
```

**Bad pattern — remote I/O inside the critical section (architecture lint rule rejects this):**

```rust
async fn update_tenant_rate_limit_BAD(
    lock: DistributedLockV1,
    tenant_id: &str,
) -> Result<()> {
    let lock_name = format!("oagw/rate-limit/{}", tenant_id);
    let guard = cluster.distributed_lock().try_lock(&lock_name, Duration::from_secs(1)).await?;

    // WRONG: cache.get is a remote call inside the critical section.
    // The lock holder is now subject to unbounded wait on the cache backend.
    // If the cache is slow or partitioned, the lock TTL expires while the
    // holder is still trying to read, creating the classic stale-writer scenario.
    let current = cluster.cache().get(&format!("oagw/counter/{}", tenant_id)).await?;
    //           ^^^^^^^^^^^^^^^ `cargo gears lint`: E0001 `no-remote-in-critical-section`

    let new_val = increment(&current.unwrap().value);

    // WRONG: another remote call inside the critical section.
    cluster.cache().put(&format!("oagw/counter/{}", tenant_id), &new_val, None).await?;
    //              ^^^ `cargo gears lint`: E0001 `no-remote-in-critical-section`

    guard.release().await?;
    Ok(())
}
```

**Good pattern — explicit release and handling of release failure:**

```rust
async fn do_exclusive_work(lock: DistributedLockV1) -> Result<()> {
    let guard = cluster.distributed_lock()
        .lock("singleton/migration", Duration::from_secs(30), Duration::from_secs(5))
        .await?;

    // Local work only
    let decision = compute_migration_plan();

    // Release before any remote effects. If release fails, log — the TTL
    // will eventually release the lock. We don't retry release indefinitely.
    if let Err(e) = guard.release().await {
        tracing::warn!(error = %e, "lock release failed; TTL will handle cleanup");
    }

    // Now apply remote effects. These can fail without holding the lock.
    apply_migration_plan(decision).await?;
    Ok(())
}
```

**Bad pattern — relying on Drop for release:**

```rust
async fn do_work_BAD(lock: DistributedLockV1) -> Result<()> {
    let _guard = cluster.distributed_lock()
        .try_lock("resource", Duration::from_secs(1))
        .await?;
    // Work
    compute();
    // `_guard` drops here — but Drop is a no-op (this ADR's decision).
    // The lock remains held until the TTL expires (1 second).
    // If the caller expects the lock to be released on scope exit, they are wrong.
    // The linter should flag missing `.release().await`.
    Ok(())
}
```

**References:**

- [Martin Kleppmann — How to do distributed locking](https://martin.kleppmann.com/2016/02/08/how-to-do-distributed-locking.html) — The canonical fencing-token argument, which this ADR addresses.
- [Tokio documentation — block_on inside an async context](https://docs.rs/tokio/latest/tokio/runtime/struct.Handle.html#method.block_on) — Explains why `block_on` panics inside a Tokio runtime.
- [async-drop RFC (Rust)](https://github.com/rust-lang/rfcs/pull/3417) — Discussion of why async Drop is not yet part of the language.
- ADR-001 (this change) — Backend compatibility and the cache-CAS-universal model, including which backends can support fencing tokens natively.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements and design elements:

- `cpt-cf-clst-fr-lock-acquire` — `try_lock` / `lock` with TTL and explicit async release.
- `cpt-cf-clst-fr-lock-release` — `LockGuard::release(self) -> Result<(), ClusterError>` with no-op `Drop`.
- `cpt-cf-clst-fr-leader-resign` — `LeaderWatch::resign(self)` as explicit step-down.
- `cpt-cf-clst-fr-sd-register` — `ServiceHandle::deregister(self)` as explicit teardown.
- `cpt-cf-clst-nfr-bounded-critical-section` — Async + timeouts + no-remote-in-critical-section structurally bounds critical sections.
- `cpt-cf-clst-constraint-no-remote-in-critical-section` (DESIGN §2.2) — Architectural rule enforced via `cargo gears lint`.
- DESIGN §3.3 lock contract — Method signatures and `Drop` semantics realize this ADR.
- DESIGN §3.7 Lifecycle Pattern (Builder/Handle) — Post-shutdown best-effort `Ok` semantics for `release` / `deregister` / `resign` derive from this ADR's release model.

**Sibling ADRs:**

- ADR-006 (builder/handle lifecycle) — `Drop` panic guard inherits this ADR's no-I/O-in-Drop rule.
