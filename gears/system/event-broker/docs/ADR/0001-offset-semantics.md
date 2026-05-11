# ADR-0001: Offset Semantics — Sequences Start at 1

## Status

Accepted

## Context

The event broker assigns consumer-visible **sequences** (also called offsets) to events via the storage backend. The design describes sequences as monotonically increasing within a `(topic, partition)` but did not originally specify the floor — whether they start at 0 or 1. This gap has consequences for every component that reasons about cursors, valid SEEK ranges, or offset types.

The **cursor** model used throughout the broker is **last-processed-offset**: a consumer stores the sequence of the last event it successfully processed, and the broker delivers the next event from `cursor + 1`. For a consumer that has never processed any event, a special "nothing processed yet" cursor value is needed. That value is `cursor = RF - 1`, where RF is the retention floor (the sequence of the oldest available event).

Two choices were considered:

| Sequences start at | RF on fresh topic | "nothing yet" cursor | cursor space | Minimum type |
|---|---|---|---|---|
| 0 | 0 | `0 - 1 = -1` | `{-1, 0, 1, ...}` | signed (i64) |
| 1 | 1 | `1 - 1 = 0`  | `{0, 1, 2, ...}`  | unsigned (u64) |

Starting at 0 requires a signed integer type everywhere offsets appear (wire, DB, SDK) and co-opts -1 as a sentinel with no semantic business meaning. Starting at 1 eliminates negative values entirely.

## Decision

**Storage backends MUST assign sequences starting from 1. Sequence 0 is never assigned to any event.**

This is a hard conformance requirement for every backend implementation (built-in and third-party), not a convention or default.

## Consequences

**Cursor space is non-negative.**

With sequences starting at 1, the retention floor RF ≥ 1 always. Therefore:

```
cursor ∈ {0, 1, 2, ...}

cursor = 0  →  "nothing processed yet; broker emits from RF"
cursor = N  →  "last processed event had sequence N; broker emits from N + 1"
```

**Valid SEEK range is always non-negative.**

```
valid range: [RF - 1, HWM]
           = [≥ 0,    HWM]   (since RF ≥ 1)
```

No negative value is reachable on the wire, in the database, or in SDK types.

**Cursor type may be u64.**

Implementations MAY represent cursors as `u64` (unsigned 64-bit integer). Implementations that already use `i64` for cursors MUST enforce a `≥ 0` invariant. A future SDK design change may tighten this to `u64` across all layers.

**Backend conformance contract.**

Every storage backend that implements the `StorageBackend` trait MUST satisfy:

- The first `persist` call to a fresh `(topic, partition)` assigns `sequence = 1` to the first event.
- Sequence 0 is never assigned, not even as an internal or transitional value.
- On idempotent retry, previously persisted sequences are returned as-is; no sequence is assigned or re-assigned to 0.

A backend that assigns sequence 0 violates this ADR and breaks the cursor non-negativity guarantee for all consumers of that partition.

## Alternatives Considered

**Start at 0**: requires signed integer types; cursor = -1 as a "nothing yet" sentinel has no positive semantic meaning and creates edge cases in arithmetic (e.g., `cursor + 1 = 0` looks like "first sequence" but is actually "start of stream"). Rejected.

**Leave unspecified**: implicit assumptions about the floor diverge across backend implementations and SDK components. The gap was discovered during scenario review when `"earliest"` on a fresh topic was described as resolving to cursor = -1. Leaving it unspecified delays the conflict until runtime. Rejected.

## References

- DESIGN.md §3.1 "Offset Semantics" — normative vocabulary section derived from this decision
- ADR-0002 (partition-selection) — references `(topic, partition)` sequences without specifying their floor; this ADR is its prerequisite
- ADR-0006 (offset-authority) — establishes that consumers own their offset tracking; this ADR establishes what those offsets ARE
