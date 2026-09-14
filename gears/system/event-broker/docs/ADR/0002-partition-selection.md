---
status: proposed
date: 2026-09-01
decision-makers: Event Broker Team
revision-history:
  - 2026-05-06 — initial draft (key-hash with explicit producer override + subject fallback)
  - 2026-05-12 — revised (drop explicit `partition` override; broker re-hashes and is authoritative)
  - 2026-06-07 — default partition key changed from `subject` to `tenant`: a tenant's events are totally ordered by default
  - 2026-09-01 — the partition key is a JSON Pointer declared by the event type, validated at registration; no publish-time key
---

# Partition Selection — A JSON Pointer Declared by the Event Type

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Default Algorithm](#default-algorithm)
  - [The Pointer](#the-pointer)
  - [Registration-Time Validation](#registration-time-validation)
  - [Hash Location](#hash-location)
  - [Encoding](#encoding)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [A JSON Pointer Declared by the Event Type (chosen)](#a-json-pointer-declared-by-the-event-type-chosen)
  - [A Producer-Supplied `partition_key` Field on the Event](#a-producer-supplied-partition_key-field-on-the-event)
  - [An Explicit `partition` Producer Override](#an-explicit-partition-producer-override)
  - [Always Derive Partition From `event.subject`](#always-derive-partition-from-eventsubject)
  - [A Pointer on the Topic Rather Than the Event Type](#a-pointer-on-the-topic-rather-than-the-event-type)
  - [Round-Robin With No Key Affinity](#round-robin-with-no-key-affinity)
  - [Custom Pluggable Partitioner Trait in SDK (MVP)](#custom-pluggable-partitioner-trait-in-sdk-mvp)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-evbk-adr-partition-selection`

## Context and Problem Statement

A topic in the Gears event broker is divided into partitions, whose count is broker configuration (see DESIGN §3.1). Every `Event` is bound to a partition; sequence assignment, ordering, idempotent-producer state, and consumer cursors are all scoped to `(topic, partition)`. The broker therefore needs a contract for **how partition assignment is computed** before the event is enqueued in the producer outbox and ultimately landed in `backend.persist`.

The existing design imposes hard constraints on this decision:

- The partition count cannot be grown or shrunk on a live topic. Re-partitioning would break per-key ordering for every key already published; the migration path is "create new topic, dual-write, cut consumers over." Partition selection MUST therefore behave deterministically across the topic's full lifetime.
- The broker assigns the consumer-visible `event.sequence` per `(topic, partition)`. Producers never set `sequence`; they only carry chain state for ingest-side dedup in `meta.previous` and `meta.sequence` (see [ADR-0003 Event Schema](0003-event-schema.md)).
- Idempotent-producer state is keyed by `(producer_id, topic, partition)` (`evbk_producer_state`). The same producer publishing "the same logical event" on a retry MUST land on the same partition, otherwise the chain check fires against a state row that does not contain the previous attempt and the duplicate is admitted.
- Every consumer of a topic depends on the *same* key-to-partition mapping. Ordering is a property the whole set of consumers observes, so the choice of key cannot be one that individual publishers make independently.

## Decision Drivers

* Per-key order: events sharing a stable partition key MUST land on the same partition for the lifetime of the topic so consumers observe them in publish order
* One decision per type, not per message: the routing contract MUST be the same for every publisher of an event type, and visible to every consumer of it
* Even distribution under unbiased keys: the chosen partition distribution SHOULD be approximately uniform across the partition count
* Idempotent retry determinism: a retry of the same logical publish MUST resolve to the same `partition`, otherwise idempotent-producer dedup degrades to "best effort"
* First-party SDK / broker parity: when an SDK computes a local partition hint for outbox routing, it must use the same input as the broker
* Fail early: a mis-declared routing contract SHOULD be caught once, when the type is registered, rather than on every publish of it
* Reach: the interesting keys frequently live inside the payload, so the contract MUST be able to name a member of `data`
* Schema extensibility: support legitimate cases where the partition key differs from the event subject (per-tenant audit, system events with no business-domain subject, deliberate fan-out)

## Considered Options

* A JSON Pointer declared as an event-type trait, defaulting to the event's tenant (chosen)
* A producer-supplied `partition_key` field on the event, falling back to `tenant_id`
* An explicit `partition` producer override
* Always derive partition from `event.subject`
* A pointer on the topic rather than the event type
* Round-robin with no key affinity
* Custom pluggable partitioner trait exposed in the SDK at MVP

## Decision Outcome

Adopt a **JSON Pointer (RFC 6901) declared by the event type**, naming the member of an event whose value is hashed. The pointer is an `x-gts-traits` value on the event type's GTS schema, merged along the derivation chain like every other trait, and the base event type defaults it to `/tenant_id`.

An event carries no partition key. There is no publish-time way to choose one, and no explicit `partition` field. The broker computes the final topic partition; any producer SDK partition computation is an internal/local hint for outbox routing.

The `/tenant_id` default gives **per-tenant total ordering** out of the box - every event a tenant emits to a topic lands on one partition and is observed in publish order, the property the audit pipeline needs. A type that wants finer-grained grouping declares a pointer at the member it wants to group by.

### Default Algorithm

The partition is computed deterministically from a single input:

```text
pointer         = event type's `partition_key` trait, resolved along its chain
partition_input = the value `pointer` resolves to within the event
partition       = local_derivation(ascii_bytes(partition_input)) % partition_count
```

- Current first-party SDK/broker implementation: **MurmurHash3 (32-bit, x86 variant)** with a fixed seed of `0x00000000`, masked with `& 0x7FFFFFFF` before modulo. This pins first-party SDK hints to broker validation but is not a native Kafka producer compatibility promise.
- The mask `& 0x7FFFFFFF` strips the sign bit so the modulo operates on a non-negative `u31` value and avoids the negative-modulo edge case in languages with signed `%`.
- The bytes hashed MUST be the **ASCII byte representation** of the resolved value. Per the platform convention recorded in [ADR-0003 Event Schema § Event Field Encoding](0003-event-schema.md#event-field-encoding-ascii-only), all event string fields are ASCII; UTF-8 is permitted only inside `data`. A pointer into `data` therefore carries a caller obligation to name an ASCII member.
- A pointer resolving to a JSON string hashes its contents; one resolving to a number or boolean hashes its JSON form, so a numeric identifier is usable without a producer stringifying it. A pointer resolving to an object, an array, or null is an error rather than a silent fallback.
- Producers MUST NOT provide a top-level topic `partition`. First-party SDKs that send an internal `meta.partition_hint` MUST compute it with the broker-supported local derivation for that broker version.

### The Pointer

The base event type declares:

```jsonc
"x-gts-traits-schema": {
  "properties": {
    "partition_key": {
      "type": "string",
      "format": "json-pointer",
      "default": "/tenant_id"
    }
  }
}
```

A derived event type overrides it by fixing its own value in `x-gts-traits`, for example `"/subject"` to group by the entity an event is about, or `"/data/order_id"` to group by a payload member. A pointer into `data` is the case a bare field name could not express, and is why the contract is a pointer rather than a member name.

`tenant_id`, `subject`, and the partition key are conceptually different:

- `tenant_id` identifies the tenant the event belongs to - the default co-location key, giving per-tenant ordering.
- `subject` identifies the entity the event is *about*.
- the partition key names *which of the event's members* controls co-location, and is a property of the type rather than of any one event.

The tenant default fits the common platform case (audit, notifications, per-tenant streams) where a tenant's events should be totally ordered. A type needing per-subject ordering points at `/subject`; a type wanting deliberate fan-out for non-causal high-volume events points at a member the producer varies per event.

### Registration-Time Validation

The broker verifies at event-type provisioning that the pointer names a member the type's own resolved schema declares - its own narrowings and everything it inherits. A type whose pointer names no such member is rejected at registration.

This is the right moment for the check because:

- A pointer naming nothing would fail identically on *every* publish of the type. Catching it once at admission turns an open-ended runtime failure into a closed-ended registration failure.
- The resolved schema the check needs is already in hand: the registry resolved the chain in order to admit the type at all.
- The registering gear is the party that can fix it, and it is present at registration. The publisher, who would see the runtime failure, cannot.

The failure is a validation error naming the pointer and the member it failed to find, so the registering gear can correct the declaration without reading broker code.

### Hash Location

Partition selection happens in **both** the producer SDK and the broker:

- **Producer SDK** resolves the pointer from the prepared event type and computes the partition locally before calling `outbox.enqueue()`, so the `toolkit-db` outbox can route the row to the correct per-`(topic, partition)` outbox shard and preserve order.
- **Broker** re-resolves the pointer on ingest from the registered event type and re-computes the partition. The broker's value is authoritative; if persisted at all, the SDK-computed value is treated as a hint only.

The trade-offs:

- Adds one Murmur3-32 hash plus one pointer resolution to the ingest path (~ns-scale; negligible against the DB write that follows).
- Adds defense-in-depth against SDK bugs: if the SDK stamps an internal `meta.partition_hint` for outbox routing, the broker validates equality and returns `400 PartitionHashMismatch` on drift.

Each partition domain derives its own local partition from the same resolved input and its own partition count, so the counts need not agree:

```text
producer local/outbox partition = local_derivation(partition_input) % producer_outbox_partitions
broker topic partition          = broker_derivation(partition_input) % broker topic partitions
ingest service shard            = ingest_derivation(partition_input or topic/partition) % ingest_shard_count
```

These counts can legitimately differ, such as 16 producer outbox partitions, 64 broker topic partitions, and 8 ingest shards. A topic reports no partition count, so a producer computing a local hint declares the count its broker is configured with.

### Encoding

All inputs to the hash are ASCII per [ADR-0003 § Event Field Encoding](0003-event-schema.md#event-field-encoding-ascii-only). The broker rejects publishes with non-ASCII bytes in the resolved value with `400 InvalidEventFieldEncoding` before partition computation is attempted.

### Consequences

- Good, because the routing contract is **one decision per event type**, shared by every producer and visible to every consumer of it. No publisher can change the ordering another publisher's consumers depend on.
- Good, because per-`tenant` ordering holds by default, with zero declaration - a tenant's events on a topic are totally ordered, which is the common platform need.
- Good, because a mis-declared key is rejected at registration rather than failing on every publish, and the party that can fix it is the party that sees the error.
- Good, because the pointer reaches inside `data`, so grouping by a payload identifier needs no synthesized envelope field.
- Good, because idempotent retries are deterministic: the same event resolves through the same pointer to the same `partition` and therefore to the same `evbk_producer_state` row, so chain dedup works as designed.
- Good, because the event schema is one field smaller and carries no member whose only purpose is routing.
- Bad / accepted limitation, because **re-partitioning is not supported**. The only way to change the count is the dual-write migration path. Deliberate match to Kafka semantics; consumers depend on stable key-to-partition mapping.
- Bad / accepted limitation, because **changing an event type's pointer re-routes its future events**. Events already published keep their partition, so per-key ordering spans the change only if the pointer resolves to the same value. A type that needs a different key is a new type.
- Bad / accepted limitation, because **a pointer may name an optional member**. The registration check proves the member is *declared*, not that every event carries it; an event omitting it is rejected at publish. A type whose grouping must always resolve declares the member required.
- Bad / accepted limitation, because **no per-topic partitioner choice in MVP**. Every topic uses the same Murmur3 algorithm.
- Bad / accepted limitation, because **hash collisions are accepted**. Two distinct values can map to the same partition; intrinsic to modulo-hash partitioning.
- Bad / accepted limitation, because **a large tenant hot-spots its partition** under the default - all of one tenant's events route to a single partition, so a high-volume tenant gets no intra-tenant parallelism and can become a noisy neighbour. Accepted in exchange for per-tenant ordering; the escape hatch is a type-level pointer at a finer-grained member.
- Bad / accepted limitation, because **adversarial values can hot-spot a partition**. Murmur3 is not cryptographic, so a type should point at an authenticated, normalized identifier rather than a raw attacker-controlled free-form member. The broker's threat model treats producers as authenticated trusted modules; opening ingest to untrusted producers requires a separately versioned keyed partition algorithm and migration design.
- Bad / accepted cost, because **the broker spends one Murmur3-32 hash per ingest** that the SDK already computed. Sub-microsecond; negligible against the DB write that follows.

### Confirmation

The decision is verified by:

- **Registration tests**: a pointer into `data` is admitted; a pointer naming a member the type inherits from the base is admitted; a pointer naming no declared member is rejected with a message naming the pointer; a value that is not a JSON Pointer is rejected.
- **SDK unit tests** pinning the current first-party local derivation: known input → known partition, with partition counts 1, 2, 16, 64. The tests SHALL fail any future SDK change that drifts from the broker-supported derivation for that version.
- **Broker-side test** of the same fixture vector: the broker's re-hash matches the SDK's per-vector value bit-for-bit.
- **Routing tests**: a type declaring no pointer partitions by tenant, so two events of one tenant share a partition; a type pointing at a member two *different* tenants share routes both to one partition, which is the property that proves the key is the type's choice rather than the tenant's.
- **Broker rejection tests**:
  - Publish with top-level `partition` field → `400 BadRequest` (`...partition.forbidden.v1`).
  - Publish with `meta.partition_hint` that disagrees with broker's re-hash → `400 PartitionHashMismatch` (`...partition.hash.mismatch.v1`).
  - Publish with non-ASCII bytes in the resolved value → `400 InvalidEventFieldEncoding`.
- **Idempotent-retry test**: a producer publishes with chained mode, retries the publish without the original network response, and the test asserts both attempts resolve to the same partition (so they hit the same `evbk_producer_state` row) and the second is rejected per the chain protocol (`412 SequenceViolation` for chain mismatch, `200 OK` for duplicate).

## Pros and Cons of the Options

### A JSON Pointer Declared by the Event Type (chosen)

* Good, because the routing contract lives with the rest of the type's governing metadata, next to the topic it publishes to and the subject types it admits
* Good, because it is one decision per type: every publisher of the type routes identically, and no refactor in one publisher can re-route another's events
* Good, because a consumer reading the type's schema can see how its events are ordered, which a per-message field never showed
* Good, because a pointer reaches into `data`, covering the common case where the grouping identifier is a payload member
* Good, because the declaration is checkable at registration, so the failure mode is closed-ended
* Bad, because a type author must think about ordering at registration time rather than deferring it to publish sites - which is the point, but it does move the decision earlier
* Bad, because Murmur3 is not cryptographic - adversarial values can collide on one partition (accepted; producer threat model is "trusted modules")
* Bad, because the broker resolves a pointer once per ingest (accepted; sub-microsecond cost)

### A Producer-Supplied `partition_key` Field on the Event

**Description**: An optional body-level `partition_key: Option<String>` on the event, hashed when present and falling back to `tenant_id` otherwise.

* Good, because a publisher can choose grouping per message with no type change
* Good, because the fallback makes the default path always defined - `tenant_id` is required on every event
* Bad / decisive against, because **one producer can break per-key ordering for every consumer of the topic**. Ordering is a property the whole consumer set observes, but the field lets a single publish site decide it, and nothing detects a publisher that sets it inconsistently.
* Bad, because the routing contract is invisible to anyone reading the event type - the only place the rest of the governing metadata lives
* Bad, because "sometimes set, sometimes defaulted" splits one type's events across two grouping levels, and the broker cannot tell the difference from a deliberate choice
* Bad, because it cannot reach a payload member without the producer copying it into the envelope, duplicating a value that must then be kept in step
* Bad, because a wrong key is only ever visible as a production ordering anomaly; there is no moment at which it can be rejected

### An Explicit `partition` Producer Override

**Description**: Let a producer stamp the partition number directly, for deterministic test fixtures, replaying events from another system that already picked partitions, and operator-driven traffic shaping.

* Good, because the escape hatch covers niche use cases without bloating the default path
* Good, because producers replaying historical data could preserve the original partition numbers
* Bad / decisive against, because **a refactor that switches a code path from declaring a key to setting `partition` directly quietly breaks per-key ordering on a live topic**, and the broker has no way to tell whether the producer *meant* to bypass the hash. It is invisible in CI and staging and manifests only as a production ordering anomaly.
* Bad, because the niche use cases re-decompose cleanly:
  - **Test fixtures**: point the fixture's event type at a member the test varies; the hash is deterministic.
  - **Cross-system replay**: preserve the source system's *key*, not its partition number. Source N's partition layout is irrelevant once events land in our broker.
  - **Operator-driven traffic shaping**: an operator-side concern for replay tooling, not a producer-facing API.

### Always Derive Partition From `event.subject`

**Description**: No declaration at all; `partition = murmur3(subject) % N` always.

* Good, because there is nothing to declare and nothing to get wrong
* Bad, because `subject` and the grouping key are not always the same - audit aggregation per tenant, system events with no domain subject, and deliberate fan-out for non-causal events all need a different key
* Bad, because the only way for a type to group differently is to synthesize a different `subject`, overloading a field that is supposed to identify the entity the event is about
* Bad, because it cannot reach a payload member

### A Pointer on the Topic Rather Than the Event Type

**Description**: Declare the pointer once per topic, as upstream `gts-spec` models the same idea.

* Good, because a topic is the partition domain, so the declaration sits with the thing being partitioned
* Good, because every event type on the topic is then guaranteed to route consistently
* Bad / decisive against, because **a topic carries several event types with different payload shapes**, so one pointer cannot address them all. `/data/order_id` is meaningless for a shipment event on the same topic.
* Bad, because the check that the pointer names a declared member could not be made at topic registration: the event types that must satisfy it do not exist yet
* Bad, because it would make adding an event type to a topic a potentially breaking change for the topic

### Round-Robin With No Key Affinity

**Description**: Assign `partition = next_counter % N` per call, ignoring keys.

* Good, because partition utilization is even by construction
* Bad, because it violates the design's central per-topic-ordering guarantee; two events about the same subject end up on different partitions
* Bad, because idempotent producer retry becomes non-deterministic
* Bad, because the only legitimate niche (high-volume non-causal events wanting even spread) is covered by pointing a type at a member that varies per event

### Custom Pluggable Partitioner Trait in SDK (MVP)

**Description**: Expose a `Partitioner` trait in `cf-gears-event-broker-sdk`; the default impl is Murmur3-mod-N; users can register their own.

* Good, because it is maximally extensible
* Bad, because a pluggable partitioner that disagrees across producer instances on the same topic silently breaks per-key ordering - one Pod hashes with FNV, the other with Murmur3, and a fraction of keys land on different partitions
* Bad, because it expands the public SDK surface before any concrete second use case has been identified (YAGNI)
* Bad, because the broker is authoritative on partition assignment - a custom SDK partitioner that disagrees with the broker's Murmur3 simply gets `400 PartitionHashMismatch` on every publish
* Captured as a post-MVP extension in [More Information](#more-information) if and when a real second use case appears

## More Information

- **Sticky-batch partitioning post-MVP**: Kafka 2.4+ offers a "sticky batch" partitioner that keeps consecutive keyless events on the same partition for batching efficiency, then rotates. Likely worth offering as an opt-in once the SDK gains true batch-publish performance work; deferred.
- **Pluggable Partitioner trait**: if a real second use case appears (e.g., a producer wanting weighted partition selection for hot-tenant isolation), the SDK could expose a `Partitioner` trait - but the broker would still be authoritative and reject mismatches, so any pluggable scheme would need an explicit broker-side contract. Decision deferred until a concrete request lands.
- **Requiring the pointed-at member**: the registration check proves the member is declared, not that it is required. Tightening it to reject a pointer at an optional member, or at a `readOnly` one that can never be present on publish, is a plausible next step and is not decided here.
- **Hash function evolution**: Murmur3 has known weaknesses against adversarial inputs. The threat model treats producers as trusted, but if the broker ever opens to untrusted producers (e.g., a public ingest endpoint), it requires a separately versioned keyed partition algorithm and a migration plan that preserves existing topic assignments. Out of scope for MVP.
- **`meta.partition_hint`**: an internal SDK-stamped optimization the broker may accept to short-circuit re-hashing once cross-validated; not part of the public producer API. The SDK MAY omit it; the broker MUST handle its absence gracefully.

External references:

- MurmurHash3 reference (Austin Appleby): <https://github.com/aappleby/smhasher/wiki/MurmurHash3>
- RFC 6901 — JavaScript Object Notation (JSON) Pointer: <https://www.rfc-editor.org/rfc/rfc6901>
- CloudEvents `subject` attribute (semantic for "what the event is about"; reinforces why `subject` and the partition key may differ): <https://github.com/cloudevents/spec/blob/v1.0.2/cloudevents/spec.md#subject>
- RFC 2119 — keyword definitions used above (MUST, SHOULD, MAY): <https://www.rfc-editor.org/rfc/rfc2119>
- RFC 9457 — Problem Details, used for error response shapes: <https://www.rfc-editor.org/rfc/rfc9457>

## Traceability

- **PRD**: [PRD.md](../PRD.md)
  - `cpt-cf-evbk-fr-publish-single` — single-event publish; partition is broker-derived
  - `cpt-cf-evbk-fr-publish-batch` — batch publish requires same `(topic, partition)` for all events (broker derives partition; a batch's events must resolve through their types' pointers to one partition)
  - `cpt-cf-evbk-fr-producer-modes` — chained / monotonic dedup uses chain check on `evbk_producer_state(producer_id, topic, partition)`; partition determinism on retry is the dedup invariant
- **DESIGN**: [DESIGN.md](../DESIGN.md)
  - §1.1 Architectural Vision — per-topic ordering centrality
  - §2.1 Design Principles — Per-topic ordering, Immutable log
  - §3.1 Domain Model — "Partition count is broker configuration" subsection
  - §3.2 Producer Modes — references [ADR-0004](0004-idempotent-producer-protocol.md)
  - §3.6 Two Sequences — producer chain in `meta` / server-assigned `sequence` (per [ADR-0003](0003-event-schema.md))
  - `evbk_producer_state` — keyed by `(producer_id, topic, partition)`
- **Related ADRs**:
  - [`0003-event-schema`](0003-event-schema.md) — canonical event shape; `partition` is `readOnly` (server-stamped on read)
  - [`0004-idempotent-producer-protocol`](0004-idempotent-producer-protocol.md) — chain dedup is keyed by `(producer_id, topic, partition)`; partition determinism is the chain-correctness invariant
