---
status: proposed
date: 2026-09-01
decision-makers: Event Broker Team
revision-history:
  - 2026-05-06 — initial draft (key-hash with explicit producer override + subject fallback)
  - 2026-05-12 — revised (drop explicit `partition` override; broker re-hashes and is authoritative)
  - 2026-06-07 — default partition key changed from `subject` to `tenant`: a tenant's events are totally ordered by default
  - 2026-09-01 — the partition key is a JSON Pointer declared by the event type, validated at registration; no publish-time key
  - 2026-10-02 - the storage backend maps the key to a partition with a function of its own choice; determinism is the only contract
---

# Partition Selection — A JSON Pointer Declared by the Event Type

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Key to Partition](#key-to-partition)
  - [The Pointer](#the-pointer)
  - [Registration-Time Validation](#registration-time-validation)
  - [Partition Location](#partition-location)
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

A topic in the Gears event broker is divided into partitions, whose count is broker configuration until the topic is provisioned, and the count its backend reports from then on (feature 0006 §2.2) (see DESIGN §3.1). Every stored `Event` is bound to a partition; sequence assignment, ordering, and consumer cursors are all scoped to `(topic, partition)`. The broker therefore needs a contract for **which value decides an event's partition, and what is promised about how that value maps to one**.

The existing design imposes hard constraints on this decision:

- The partition count cannot be grown or shrunk on a live topic. Re-partitioning would break per-key ordering for every key already published; the migration path is "create new topic, dual-write, cut consumers over." Partition selection MUST therefore behave deterministically across the topic's full lifetime.
- A topic's log lives in a storage backend (feature 0007), and the backend assigns the consumer-visible `event.sequence` per `(topic, partition)`. Producers never set `sequence`; they only carry chain state for ingest-side dedup in `meta.previous` and `meta.sequence` (see [ADR-0003 Event Schema](0003-event-schema.md)).
- Producer chain state is keyed by `(producer_id, topic)` and checked at ingest, before any partition exists (see [ADR-0004](0004-idempotent-producer-protocol.md)), so deduplication does not depend on where an event lands.
- Storage media place keys differently: a Kafka cluster has its own partitioner, a database plugin its own hash. A contract that mandates one function binds every medium to it.
- Every consumer of a topic depends on the *same* key-to-partition mapping. Ordering is a property the whole set of consumers observes, so the choice of key cannot be one that individual publishers make independently.

## Decision Drivers

* Per-key order: events sharing a stable partition key MUST land on the same partition for the lifetime of the topic on its backend, so consumers observe them in publish order
* One decision per type, not per message: the routing contract MUST be the same for every publisher of an event type, and visible to every consumer of it
* Even distribution under unbiased keys: the chosen partition distribution SHOULD be approximately uniform across the partition count
* Backend freedom: each storage medium places keys the way it does natively; the contract asks only for what per-key order needs
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

Adopt a **JSON Pointer (RFC 6901) declared by the event type**, naming the member of an event whose value is the event's partition key. The pointer is an `x-gts-traits` value on the event type's GTS schema, merged along the derivation chain like every other trait, and the base event type defaults it to `/tenant_id`.

An event carries no partition key. There is no publish-time way to choose one, and no explicit `partition` field. Ingest resolves the pointer and hands the key to the topic's storage backend with the event; the backend maps the key to a partition with a function of its own choice.

The `/tenant_id` default gives **per-tenant total ordering** out of the box - every event a tenant emits to a topic lands on one partition and is observed in publish order, the property the audit pipeline needs. A type that wants finer-grained grouping declares a pointer at the member it wants to group by.

### Key to Partition

The partition follows from a single input:

```text
pointer   = event type's `partition_key` trait, resolved along its chain
key       = the value `pointer` resolves to within the event, as ASCII text
partition = backend_function(topic, key)    # the storage backend's own choice
```

- **The only contract is determinism**: for a topic on a backend, a given key always maps to the same partition, on every instance. Nothing else about the function is promised - not which partition, not agreement between backends.
- Any deterministic function qualifies. One example is MurmurHash3 (32-bit, x86 variant) with a fixed seed of `0x00000000`, masked with `& 0x7FFFFFFF` and taken modulo the topic's partition count; a Kafka plugin may use Kafka's own partitioner instead.
- A changed function, re-partitioning, or a move between backends with different functions changes where keys land. Per-key ordering across such a change is the end user's to handle; the broker does not carry it across.
- The key MUST be the **ASCII byte representation** of the resolved value. Per the platform convention recorded in [ADR-0003 Event Schema § Event Field Encoding](0003-event-schema.md#event-field-encoding-ascii-only), all event string fields are ASCII; UTF-8 is permitted only inside `data`. A pointer into `data` therefore carries a caller obligation to name an ASCII member.
- A pointer resolving to a JSON string yields its contents as the key; one resolving to a number or boolean yields its JSON form, so a numeric identifier is usable without a producer stringifying it. A pointer resolving to an object, an array, or null is an error rather than a silent fallback.
- Producers MUST NOT provide a top-level topic `partition`. An event has no partition until its backend stores it.

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

### Partition Location

Partition selection happens in the **storage backend**, and only there:

- **Ingest** resolves the pointer from the registered event type and passes the key with the event to `append` (feature 0007 §2.2). It computes no topic partition and stamps none on the event.
- **The backend** maps the key to one of the topic's partitions and assigns the sequence.
- **Producers** compute no topic partition and send none.

A producer's own partitions (for example its outbox partitions), ingest's outbox slots, and the backend's partitions of the topic log are distinct concepts, and none is derived from another. Only the backend's partition is the topic partition that sequences, ordering and cursors are scoped to.

### Encoding

The partition key is ASCII per [ADR-0003 § Event Field Encoding](0003-event-schema.md#event-field-encoding-ascii-only). The broker rejects publishes with non-ASCII bytes in the resolved value with `400 InvalidEventFieldEncoding` before the event is enqueued.

### Consequences

- Good, because the routing contract is **one decision per event type**, shared by every producer and visible to every consumer of it. No publisher can change the ordering another publisher's consumers depend on.
- Good, because per-`tenant` ordering holds by default, with zero declaration - a tenant's events on a topic are totally ordered, which is the common platform need.
- Good, because a mis-declared key is rejected at registration rather than failing on every publish, and the party that can fix it is the party that sees the error.
- Good, because the pointer reaches inside `data`, so grouping by a payload identifier needs no synthesized envelope field.
- Good, because each backend places keys the way its medium does natively - a Kafka plugin can use Kafka's partitioner - and the broker carries no partition function of its own.
- Good, because the event schema is one field smaller and carries no member whose only purpose is routing.
- Bad / accepted limitation, because **re-partitioning is not supported**. The only way to change the count is the dual-write migration path. Deliberate match to Kafka semantics; consumers depend on stable key-to-partition mapping.
- Bad / accepted limitation, because **per-key ordering holds only while the backend's function holds**. A changed function, re-partitioning, or a move between backends with different functions re-places keys, and ordering across that change is the end user's to handle.
- Bad / accepted limitation, because **changing an event type's pointer re-routes its future events**. Events already published keep their partition, so per-key ordering spans the change only if the pointer resolves to the same value. A type that needs a different key is a new type.
- Bad / accepted limitation, because **a pointer may name an optional member**. The registration check proves the member is *declared*, not that every event carries it; an event omitting it is rejected at publish. A type whose grouping must always resolve declares the member required.
- Bad / accepted limitation, because **key placement is not portable across backends**. Two backends may place one key on different partitions, so a key's partition cannot be predicted from the key alone.
- Bad / accepted limitation, because **collisions are accepted**. Two distinct values can map to the same partition; intrinsic to mapping keys onto a fixed partition count.
- Bad / accepted limitation, because **a large tenant hot-spots its partition** under the default - all of one tenant's events route to a single partition, so a high-volume tenant gets no intra-tenant parallelism and can become a noisy neighbour. Accepted in exchange for per-tenant ordering; the escape hatch is a type-level pointer at a finer-grained member.
- Bad / accepted limitation, because **adversarial values can hot-spot a partition**. A backend's function is typically not cryptographic, so a type should point at an authenticated, normalized identifier rather than a raw attacker-controlled free-form member. The broker's threat model treats producers as authenticated trusted modules; opening ingest to untrusted producers requires keyed placement in the backends and a migration design.
- Bad / accepted cost, because **ingest resolves one pointer per event**. Sub-microsecond; negligible against the write that follows.

### Confirmation

The decision is verified by:

- **Registration tests**: a pointer into `data` is admitted; a pointer naming a member the type inherits from the base is admitted; a pointer naming no declared member is rejected with a message naming the pointer; a value that is not a JSON Pointer is rejected.
- **Backend determinism tests** (per plugin): one key maps to the same partition across calls, across plugin instances built from the same settings, and across a restart, with partition counts 1, 2, 16, 64.
- **Ingest test**: the key handed to `append` is the value the pointer resolves to, and ingest stamps no partition on the event.
- **Routing tests**: a type declaring no pointer partitions by tenant, so two events of one tenant share a partition; a type pointing at a member two *different* tenants share routes both to one partition, which is the property that proves the key is the type's choice rather than the tenant's.
- **Broker rejection tests**:
  - Publish with top-level `partition` field → `400 BadRequest` (`...partition.forbidden.v1`).
  - Publish with non-ASCII bytes in the resolved value → `400 InvalidEventFieldEncoding`.

## Pros and Cons of the Options

### A JSON Pointer Declared by the Event Type (chosen)

* Good, because the routing contract lives with the rest of the type's governing metadata, next to the topic it publishes to and the subject types it admits
* Good, because it is one decision per type: every publisher of the type routes identically, and no refactor in one publisher can re-route another's events
* Good, because a consumer reading the type's schema can see how its events are ordered, which a per-message field never showed
* Good, because a pointer reaches into `data`, covering the common case where the grouping identifier is a payload member
* Good, because the declaration is checkable at registration, so the failure mode is closed-ended
* Bad, because a type author must think about ordering at registration time rather than deferring it to publish sites - which is the point, but it does move the decision earlier
* Bad, because a backend's function is typically not cryptographic - adversarial values can collide on one partition (accepted; producer threat model is "trusted modules")
* Bad, because the broker resolves a pointer once per ingest (accepted; sub-microsecond cost)

### A Producer-Supplied `partition_key` Field on the Event

**Description**: An optional body-level `partition_key: Option<String>` on the event, used as the key when present and falling back to `tenant_id` otherwise.

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
* Bad / decisive against, because **a refactor that switches a code path from declaring a key to setting `partition` directly quietly breaks per-key ordering on a live topic**, and the broker has no way to tell whether the producer *meant* to bypass the key. It is invisible in CI and staging and manifests only as a production ordering anomaly.
* Bad, because the niche use cases re-decompose cleanly:
  - **Test fixtures**: point the fixture's event type at a member the test varies; the backend's mapping is deterministic.
  - **Cross-system replay**: preserve the source system's *key*, not its partition number. Source N's partition layout is irrelevant once events land in our broker.
  - **Operator-driven traffic shaping**: an operator-side concern for replay tooling, not a producer-facing API.

### Always Derive Partition From `event.subject`

**Description**: No declaration at all; the partition key is always `event.subject`.

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
* Bad, because the only legitimate niche (high-volume non-causal events wanting even spread) is covered by pointing a type at a member that varies per event

### Custom Pluggable Partitioner Trait in SDK (MVP)

**Description**: Expose a `Partitioner` trait in `cf-gears-event-broker-sdk`; the default impl is Murmur3-mod-N; users can register their own.

* Good, because it is maximally extensible
* Bad, because a pluggable partitioner that disagrees across producer instances on the same topic silently breaks per-key ordering - one Pod hashes with FNV, the other with Murmur3, and a fraction of keys land on different partitions
* Bad, because it expands the public SDK surface before any concrete second use case has been identified (YAGNI)
* Bad, because the SDK places nothing: the storage backend maps keys to partitions, so an SDK partitioner has no decision to make
* Captured as a post-MVP extension in [More Information](#more-information) if and when a real second use case appears

## More Information

- **Sticky-batch partitioning post-MVP**: Kafka 2.4+ offers a "sticky batch" partitioner that keeps consecutive keyless events on the same partition for batching efficiency, then rotates. Likely worth offering as an opt-in once the SDK gains true batch-publish performance work; deferred.
- **Pluggable Partitioner trait**: if a real second use case appears (e.g., weighted partition selection for hot-tenant isolation), it belongs in the backend's function, as that backend's configuration, not in the SDK. Decision deferred until a concrete request lands.
- **Requiring the pointed-at member**: the registration check proves the member is declared, not that it is required. Tightening it to reject a pointer at an optional member, or at a `readOnly` one that can never be present on publish, is a plausible next step and is not decided here.
- **Function evolution**: common deterministic functions (MurmurHash3 among them) have known weaknesses against adversarial inputs. The threat model treats producers as trusted, but if the broker ever opens to untrusted producers (e.g., a public ingest endpoint), it requires keyed placement in the backends and a migration plan that preserves existing topic assignments. Out of scope for MVP.

External references:

- MurmurHash3 reference (Austin Appleby): <https://github.com/aappleby/smhasher/wiki/MurmurHash3>
- RFC 6901 — JavaScript Object Notation (JSON) Pointer: <https://www.rfc-editor.org/rfc/rfc6901>
- CloudEvents `subject` attribute (semantic for "what the event is about"; reinforces why `subject` and the partition key may differ): <https://github.com/cloudevents/spec/blob/v1.0.2/cloudevents/spec.md#subject>
- RFC 2119 — keyword definitions used above (MUST, SHOULD, MAY): <https://www.rfc-editor.org/rfc/rfc2119>
- RFC 9457 — Problem Details, used for error response shapes: <https://www.rfc-editor.org/rfc/rfc9457>

## Traceability

- **PRD**: [PRD.md](../PRD.md)
  - `cpt-cf-evbk-fr-publish-single` - single-event publish; the storage backend assigns the partition
  - `cpt-cf-evbk-fr-publish-batch` - batch publish; ingest resolves each event's key and the backend places each event
  - `cpt-cf-evbk-fr-producer-modes` - chained / monotonic dedup checks chain state per `(producer_id, topic)`, independent of the partition
- **DESIGN**: [DESIGN.md](../DESIGN.md)
  - §1.1 Architectural Vision — per-topic ordering centrality
  - §2.1 Design Principles — Per-topic ordering, Immutable log
  - §3.1 Domain Model — "Partition count is broker configuration" subsection
  - §3.2 Producer Modes — references [ADR-0004](0004-idempotent-producer-protocol.md)
  - §3.6 Two Sequences — producer chain in `meta` / server-assigned `sequence` (per [ADR-0003](0003-event-schema.md))
  - producer chain state - keyed by `(producer_id, topic)`
- **Features**:
  - [0007 Storage Backend API](../features/0007-storage-backend-api.md) - `append` takes the key; the backend maps it to a partition
- **Related ADRs**:
  - [`0003-event-schema`](0003-event-schema.md) — canonical event shape; `partition` is `readOnly` (server-stamped on read)
  - [`0004-idempotent-producer-protocol`](0004-idempotent-producer-protocol.md) - chain dedup is keyed by `(producer_id, topic)`, independent of the partition
