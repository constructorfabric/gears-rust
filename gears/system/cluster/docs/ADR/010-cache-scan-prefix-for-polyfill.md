---
status: accepted
date: 2026-06-10
---

# ADR-010: Cache `scan_prefix` Enumeration for the Prefix-Watch Polyfill

**ID**: `cpt-cf-clst-adr-cache-scan-prefix-for-polyfill`

> Recorded during the scoping & prefix-watch polyfill feature (DECOMPOSITION §2.7), when implementing `PollingPrefixWatch` against the frozen cache contract surfaced a missing enumeration capability.

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [The added method](#the-added-method)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

DESIGN §3.12 specifies `PollingPrefixWatch::spawn(cache, prefix, interval) -> CacheWatch`, which "periodically lists keys under the prefix, diffs against the previous list, and emits `Changed`/`Deleted` … Cost: N `get` calls per interval." Synthesizing a prefix watch by polling therefore requires the polyfill to **enumerate** the keys currently under a prefix.

The cache contract frozen by the cache-primitive feature (`ClusterCacheBackend`, DECOMPOSITION §2.2) exposes only `get` / `put` / `delete` / `contains` / `put_if_absent` / `compare_and_swap` / `watch` / `watch_prefix`. None of these enumerates a keyspace: `watch_prefix` is the very capability the polyfill stands in for (and is unavailable on the backends that need the polyfill), and the cache-based service-discovery default avoids enumeration entirely by building its membership view from a **native** `watch_prefix` stream. As written, `PollingPrefixWatch` cannot be implemented against `Arc<dyn ClusterCacheBackend>` — there is no way to discover which keys exist under the prefix.

## Decision Drivers

- **The polyfill must work against the public trait object**, with no backend-specific downcast.
- **The cache contract is consumed by the four primitives and the SDK defaults**; any change must be additive so existing backends keep compiling.
- **Honest capability signalling** — a backend that cannot enumerate must say so, surfaced as the standard `Unsupported` error, not a panic or a silent empty result.
- **Match DESIGN §3.12's documented cost** (`N + 1` round-trips: one enumeration plus one `get` per key to read versions for change detection).

## Considered Options

1. **Add `scan_prefix` to `ClusterCacheBackend`** with a default that returns `Unsupported` (CHOSEN).
2. **Change `spawn` to take an explicit candidate key set** (`spawn(cache, keys, interval)`), staying within the frozen contract.
3. **Defer the polyfill** to a follow-up once enumeration is designed.

## Decision Outcome

Chosen option: **Option 1** — extend the trait with a defaulted `scan_prefix`.

### The added method

```rust
async fn scan_prefix(&self, prefix: &str) -> Result<Vec<String>, ClusterError> {
    let _ = prefix;
    Err(ClusterError::Unsupported { feature: "scan_prefix" })
}
```

The default body makes the extension **additive**: every existing `ClusterCacheBackend` implementation keeps compiling unchanged and reports `Unsupported` until it opts in by overriding the method. `PollingPrefixWatch` lists keys with `scan_prefix`, then issues one `get` per key to read its version (realizing the documented `N + 1` cost) and diffs versions to emit `Changed` for new/bumped keys and `Deleted` for vanished keys — the events carry the full backend key, exactly like a native `watch_prefix`. A backend whose `scan_prefix` errors closes the synthesized watch with a terminal `CacheWatchEvent::Closed`.

`ScopedCacheBackend` forwards `scan_prefix` like every other operation: it prepends the scope prefix to the argument and strips it from each returned key, so scoping composes with the polyfill.

### Consequences

- The cache contract gains one method. Because it is defaulted, the dyn-compatibility assertion (`assert_dyn_compatible!(ClusterCacheBackend)`) still holds and no existing plugin breaks.
- Backends that want polyfill support (or any prefix enumeration) override `scan_prefix`; those that cannot leave the default and the polyfill degrades to a clean `Unsupported`/`Closed`.
- The polyfill's cost is `O(N)` `get` calls per interval with no millisecond precision; its doc comment warns about this and steers high-scale consumers to a native-`prefix_watch` backend.
- A future native `watch_prefix` on a backend supersedes the polyfill for that backend; `scan_prefix` remains useful for any one-shot enumeration need.

### Confirmation

- Unit test: `PollingPrefixWatch` against an in-memory cache implementing `scan_prefix` emits `Changed` for initial keys and `Deleted` on removal.
- Unit test: a cache whose `scan_prefix` returns `Unsupported` closes the synthesized watch with `Closed(Unsupported { feature: "scan_prefix" })`.
- Unit test: dropping the returned `CacheWatch` stops the polling task (checked via a scan-call counter), even on a quiescent keyspace where no events are sent.
- Unit test: `ScopedCacheBackend::scan_prefix` prepends the scope prefix on the way in and strips it from returned keys.

## Pros and Cons of the Options

**Option 1 — defaulted `scan_prefix` (CHOSEN)**
- Good: the polyfill works against the public trait object; no downcast.
- Good: additive — existing backends compile unchanged; opt-in by override.
- Good: dishonest/absent support surfaces as the standard `Unsupported` error.
- Bad: widens the plugin contract by one method (mitigated by the default body).

**Option 2 — explicit key set on `spawn`**
- Good: zero contract change.
- Bad: diverges from the documented `spawn(cache, prefix, interval)` signature; cannot discover brand-new keys (the consumer would have to know them in advance), which defeats the purpose of a prefix watch.

**Option 3 — defer the polyfill**
- Good: keeps the contract frozen for this change.
- Bad: leaves DESIGN §3.12 unrealized and the scoping feature half-delivered; the enumeration decision must be made regardless.

## More Information

- DESIGN §3.12 — Polyfill (the `spawn` signature and `N get calls per interval` cost note).
- DESIGN §3.8 — Per-primitive scoping (`ScopedCacheBackend` forwards `scan_prefix`).
- ADR-005 — Facade + backend-trait pattern (the method is added to the backend trait; consumers reach it through `ClusterCacheV1`).
- ADR-003 — Watch-event lifecycle contract (`Closed` terminal signal used when enumeration errors).

## Traceability

- **PRD**: [PRD.md](../PRD.md) §5.8
- **DESIGN**: [DESIGN.md](../DESIGN.md) §3.12, §3.8

This decision directly addresses:

- `cpt-cf-clst-fr-namespacing-scoped` — scoping wrappers compose with the polyfill via `scan_prefix`.
- `cpt-cf-clst-dod-scoping-polyfill-polling` — `PollingPrefixWatch` enumeration mechanism.
- `cpt-cf-clst-component-sdk` (DESIGN §3.2) — the SDK hosts `PollingPrefixWatch` and the extended `ClusterCacheBackend`.

**Sibling ADRs:**

- ADR-003 — watch lifecycle (`Reset`/`Closed`) the synthesized watch emits.
- ADR-005 — facade/backend-trait pattern the added method follows.
