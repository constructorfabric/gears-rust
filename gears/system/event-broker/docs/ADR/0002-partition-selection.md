---
status: proposed
date: 2026-05-12
decision-makers: Event Broker Team
revision-history:
  - 2026-05-06 — initial draft (key-hash with explicit producer override + subject fallback)
  - 2026-05-12 — revised (drop explicit `partition` override; broker re-hashes and is authoritative; `partition_key` is the producer-facing input selector)
  - 2026-06-07 — default partition key changed from `subject` to `tenant` (PR #1978 review): a tenant's events are totally ordered by default; per-subject grouping is opt-in via explicit `partition_key`
---

# Partition Selection — Broker-Authoritative `partition_key` / `tenant_id` Input

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Default Algorithm](#default-algorithm)
  - [Partition-Key Source](#partition-key-source)
  - [Hash Location](#hash-location)
  - [Broker-Side Validation](#broker-side-validation)
  - [Encoding](#encoding)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Broker-Authoritative `partition_key` / `tenant_id` Input (chosen)](#broker-authoritative-partition_key--tenant_id-input-chosen)
  - [Earlier Draft — Add Explicit `partition` Producer Override (rejected on revision)](#earlier-draft--add-explicit-partition-producer-override-rejected-on-revision)
  - [Always Derive Partition From `event.subject`](#always-derive-partition-from-eventsubject)
  - [Round-Robin With No Key Affinity](#round-robin-with-no-key-affinity)
  - [Custom Pluggable Partitioner Trait in SDK (MVP)](#custom-pluggable-partitioner-trait-in-sdk-mvp)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-evbk-adr-partition-selection`

## Context and Problem Statement

A topic in the Gears event broker is divided into a fixed number of partitions declared at topic registration time (`Topic.partitions`, see DESIGN §3.1). Every `Event` is bound to a partition; sequence assignment, ordering, idempotent-producer state, and consumer cursors are all scoped to `(topic, partition)`. The broker therefore needs a contract for **how partition assignment is computed** before the event is enqueued in the producer outbox and ultimately landed in `backend.persist`.

The existing design imposes hard constraints on this decision:

- `Topic.partitions` is **fixed at topic creation** and cannot be grown or shrunk on a live topic. Re-partitioning would break per-key ordering for every key already published; the migration path is "create new topic, dual-write, cut consumers over." Partition selection MUST therefore behave deterministically across the topic's full lifetime.
- The broker assigns the consumer-visible `event.sequence` per `(topic, partition)`. Producers never set `sequence`; they only carry chain state for ingest-side dedup in `meta.previous` and `meta.sequence` (see [ADR-0003 Event Schema](0002-event-schema.md)).
- `Event` carries `subject` (a per-event entity identifier) and `subject_type`. A dedicated `partition_key` field exists as an optional producer-supplied grouping input.
- Idempotent-producer state is keyed by `(producer_id, topic, partition)` (`evbk_producer_state`). The same producer publishing "the same logical event" on a retry MUST land on the same partition, otherwise the chain check fires against a state row that does not contain the previous attempt and the duplicate is admitted.

The **initial 2026-05-06 draft** of this ADR allowed three paths to a partition value: `partition_key` (hashed), `subject` fallback (hashed), and an explicit `event.partition` producer override. The override path turned out to be a silent-ordering-bug surface — a producer refactor that switches a code-path from "set `partition_key`" to "set `partition` directly" quietly breaks per-key ordering on a live topic, and the SDK has no way to tell whether the producer *meant* to bypass the hash. Re-iteration during pre-implementation review removed the override path. This revision records the corrected contract.

## Decision Drivers

* Per-key order: events sharing a stable partition key MUST land on the same partition for the lifetime of the topic so consumers observe them in publish order
* Even distribution under unbiased keys: the chosen partition distribution SHOULD be approximately uniform across `[0, topic.partitions)`
* Idempotent retry determinism: a retry of the same logical publish MUST resolve to the same `partition`, otherwise idempotent-producer dedup degrades to "best effort"
* First-party SDK / broker parity: when an SDK computes a local partition hint for outbox routing, it must use the same input field as the broker and produce a value the broker can validate
* Operational simplicity: the broker stays a thin validator; the partition is computed deterministically from inputs already on the event
* No silent freedom: a producer should not be able to bypass the hash with an explicit partition number — this turned out to be a refactor-induced bug surface in the initial draft
* Schema extensibility: support legitimate use cases where the partition key differs from the event subject (per-tenant audit, system events with no business-domain subject, deliberate fan-out)

## Considered Options

* Broker-authoritative partition selection from `partition_key` by default, falling back to `tenant_id`; SDK-local partitioning is only a hint for first-party outbox routing (chosen — this revision)
* Earlier draft: key-hash with explicit `event.partition` producer override (rejected on revision)
* Always derive partition from `event.subject` (no `partition_key` field at all)
* Round-robin with no key affinity
* Custom pluggable partitioner trait exposed in the SDK at MVP

## Decision Outcome

Adopt **broker-authoritative partition selection from `partition_key` when present, otherwise `tenant_id`**. `partition_key` is the producer-facing way to choose a grouping input other than the default `tenant_id`. There is no explicit `partition` field on publish input. The broker computes the final topic partition; any producer SDK partition computation is an internal/local hint for outbox routing, not a native Kafka producer compatibility contract.

Defaulting to `tenant` gives **per-tenant total ordering** out of the box — every event a tenant emits to a topic lands on one partition and is observed in publish order (the property the audit pipeline needs). Producers that want finer-grained grouping (per-subject, per-region, fan-out) set `partition_key` explicitly.

### Default Algorithm

The partition is computed deterministically from a single input:

```text
partition_input = partition_key  if partition_key is present
                  tenant_id      otherwise
partition       = local_derivation(ascii_bytes(partition_input)) % partition_count
```

- Current first-party SDK/broker implementation: **MurmurHash3 (32-bit, x86 variant)** with a fixed seed of `0x00000000`, masked with `& 0x7FFFFFFF` before modulo. This pins first-party SDK hints to broker validation but is not a native Kafka producer compatibility promise.
- The mask `& 0x7FFFFFFF` strips the sign bit so the modulo operates on a non-negative `u31` value and avoids the negative-modulo edge case in languages with signed `%`.
- The bytes hashed MUST be the **ASCII byte representation** of the input. Per the platform convention recorded in [ADR-0003 Event Schema § Event Field Encoding](0002-event-schema.md#event-field-encoding-ascii-only), all event string fields (`partition_key`, `subject`, etc.) are ASCII; UTF-8 is permitted only inside `data`. This eliminates UTF-8-vs-ASCII determinism concerns in the hash path.
- Producers MUST NOT provide a top-level topic `partition`. First-party SDKs that send an internal `meta.partition_hint` MUST compute it with the broker-supported local derivation for that broker version.

### Partition-Key Source

The `Event` schema carries an **optional, body-level** field:

- **`partition_key: Option<String>`** — producer-supplied, opaque (ASCII, ≤ 1024 bytes), used only for partition selection. Not validated against any GTS schema. Preserved on the read projection so consumers can see which grouping key the producer chose.

Resolution rules:

1. If `event.partition_key` is `Some(s)`, hash `s`.
2. Else, hash `event.tenant_id` (which is always present per the event schema contract).

Since `tenant_id` is required on every event, there is no third case — no missing-input failure mode, no silent fallback. The default path is always defined.

`tenant_id`, `subject`, and `partition_key` are conceptually different:

- `tenant_id` identifies the tenant the event belongs to — the default co-location key, giving per-tenant ordering.
- `subject` identifies the entity the event is *about*.
- `partition_key` is the explicit grouping input controlling *co-location* when tenant-wide grouping is not the desired behavior.

The tenant default fits the common platform case (audit, notifications, per-tenant streams) where a tenant's events should be totally ordered. Producers needing per-subject ordering set `partition_key = subject`; system events with no natural tenant grouping or deliberate fan-out for non-causal high-volume events set `partition_key` explicitly (e.g., `partition_key = uuid_v4()`).

### Hash Location

Partition selection happens in **both** the producer SDK and the broker:

- **Producer SDK** computes the partition locally before calling `outbox.enqueue()`, so the `toolkit-db` outbox can route the row to the correct per-`(topic, partition)` outbox shard and preserve order.
- **Broker** re-computes the partition on ingest from `partition_key` / `tenant_id` in the received event. The broker's value is authoritative; if persisted at all, the SDK-computed value is treated as a hint only.

This is a deliberate change from the initial draft (which had the SDK as the single computer of `partition`). The trade-offs:

- Adds one Murmur3-32 hash to the ingest path (~ns-scale; negligible against the DB write that follows).
- Eliminates the producer-supplied partition surface (no explicit `partition` field that producers can stamp).
- Adds defense-in-depth against SDK bugs: if the SDK stamps an internal `meta.partition_hint` for outbox routing, the broker validates equality and returns `400 PartitionHashMismatch` on drift.

The SDK may fetch `topic.partitions` once at startup (or on first publish to a topic) via `GET /v1/topics/{id}` when it needs a local topic-partition hint. The count is immutable per `Topic.partitions` design, so that cache does not go stale. This does not require producer-local outbox partitions, broker topic partitions, and ingest-service shards to have the same count; each partition domain derives its own local partition from the agreed input field and its own partition count.

Example:

```text
partition_input = partition_key if present else tenant_id

producer local/outbox partition = local_derivation(partition_input) % producer_outbox_partitions
broker topic partition          = broker_derivation(partition_input) % topic.partitions
ingest service shard            = ingest_derivation(partition_input or topic/partition) % ingest_shard_count
```

These counts can legitimately differ, such as 16 producer outbox partitions, 64 broker topic partitions, and 8 ingest shards.

### Broker-Side Validation

On `POST /v1/events` and `POST /v1/events:batch`, the ingest service MUST:

- Compute `partition` from `partition_key` (if present) or `tenant_id` using the broker-supported derivation for this broker version.
- Use the computed value as the authoritative partition for sequence assignment, `evbk_producer_state` lookup, and storage routing.
- Reject any publish carrying a top-level `partition` field with `400 BadRequest` (RFC 9457 problem type `gts.cf.core.errors.err.v1~cf.core.partition.forbidden.v1`). The wire contract DOES NOT accept a producer-supplied partition.
- If the publish carries `meta.partition_hint` (an internal SDK-stamped optimization), validate it equals the broker-computed value; reject mismatches with `400 PartitionHashMismatch` (RFC 9457 problem type `gts.cf.core.errors.err.v1~cf.core.partition.hash.mismatch.v1`).

### Encoding

All inputs to the hash are ASCII per [ADR-0003 § Event Field Encoding](0002-event-schema.md#event-field-encoding-ascii-only). The broker rejects publishes with non-ASCII bytes in `partition_key` or `tenant_id` with `400 InvalidEventFieldEncoding` before partition computation is attempted. `partition_key` is additionally length-capped at 1024 bytes (`400 EventFieldTooLong` on overflow).

### Consequences

- Good, because per-`tenant` ordering holds by default, with zero producer configuration — a tenant's events on a topic are totally ordered, which is the common platform need (audit, notifications, per-tenant streams).
- Good, because producers needing a different grouping (per-subject, per-region, fan-out) opt in by setting `partition_key` explicitly — no SDK fork, no custom partitioner.
- Good, because idempotent retries are deterministic: the same `(producer_id, partition_key | tenant_id)` resolves to the same `partition` and therefore to the same `evbk_producer_state` row, so chain dedup works as designed.
- Good, because future first-party SDKs can validate their local hints against broker fixtures without making native Kafka producer compatibility part of the publish contract.
- Good, because the broker is the authority on partition assignment, removing the silent-ordering-bug class that a producer-set `partition` override created in the initial draft.
- Good, because the broker stays a thin validator: the partition computation is a single line of code (hash + mask + mod), not a policy engine.
- Bad / accepted limitation, because **re-partitioning is not supported**. Once a topic is created with N partitions, the only way to change is the dual-write migration path. Deliberate match to Kafka semantics; consumers depend on stable key-to-partition mapping.
- Bad / accepted limitation, because **no per-topic partitioner choice in MVP**. Every topic uses the same Murmur3 algorithm.
- Bad / accepted limitation, because **hash collisions are accepted**. Two distinct partition keys can map to the same partition; intrinsic to modulo-hash partitioning.
- Bad / accepted limitation, because **a large tenant hot-spots its partition** under the tenant default — all of one tenant's events route to a single partition, so a high-volume tenant gets no intra-tenant parallelism and can become a noisy neighbour. Accepted in exchange for per-tenant ordering; the escape hatch is an explicit `partition_key` (e.g., `partition_key = subject`) for topics where a tenant's volume outweighs its ordering need.
- Bad / accepted limitation, because **adversarial keys can hot-spot a partition**. Murmur3 is not cryptographic, so producers must use authenticated, normalized identifiers with producer-controlled canonical representations rather than raw attacker-controlled free-form values. Canonical user-derived identifiers remain supported. The broker's threat model treats producers as authenticated trusted modules; opening ingest to untrusted producers requires a separately versioned keyed partition algorithm and migration design.
- Bad / accepted cost, because **the broker now spends one Murmur3-32 hash per ingest** that the SDK already computed. Murmur3-32 over ≤ 1024 ASCII bytes is sub-microsecond; negligible against the DB write that follows. Accepted in exchange for removing the producer-supplied partition surface.
- Bad / accepted limitation, because **producers that use `partition_key` inconsistently** (sometimes set, sometimes rely on the `tenant_id` default) can split ordering intent across different grouping levels. Mitigation: producer-author guidance — set `partition_key` consistently per logical entity when tenant-wide ordering is not the desired grouping. Documented in `docs/features/0001-idempotent-producers.md`.

### Confirmation

The decision is verified by:

- **SDK unit tests** pinning the current first-party local derivation: known input → known partition for representative `partition_key` / `tenant_id` strings (ASCII printable, ASCII control bytes, empty `partition_key` falling back to `tenant_id`), with `topic.partitions` values 1, 2, 16, 64. The tests SHALL fail any future SDK change that drifts from the broker-supported derivation for that version.
- **Broker-side test** of the same fixture vector: the broker's re-hash matches the SDK's per-vector value bit-for-bit.
- **First-party SDK fixture vector** maintained in the SDK contract documentation: a list of `(input, partitions, expected_partition)` tuples (using `input = partition_key or tenant_id`) that any SDK sending `meta.partition_hint` MUST reproduce for the broker version it targets. Native Kafka producers writing directly to backend topics are outside this contract.
- **Broker rejection tests**:
  - Publish with top-level `partition` field → `400 BadRequest` (`...partition.forbidden.v1`).
  - Publish with `meta.partition_hint` that disagrees with broker's re-hash → `400 PartitionHashMismatch` (`...partition.hash.mismatch.v1`).
  - Publish with non-ASCII bytes in `partition_key` or `tenant_id` → `400 InvalidEventFieldEncoding`.
  - Publish with `partition_key` > 1024 bytes → `400 EventFieldTooLong`.
- **Idempotent-retry test**: a producer publishes with chained mode, retries the publish without the original network response, and the test asserts both attempts resolve to the same partition (so they hit the same `evbk_producer_state` row) and the second is rejected per the chain protocol (`412 SequenceViolation` for chain mismatch, `200 OK` for duplicate).

## Pros and Cons of the Options

### Broker-Authoritative `partition_key` / `tenant_id` Input (chosen)

* Good, because it cleanly separates the partitioning concern (`partition_key`) from the semantic concern (`subject`), allowing legitimate divergence
* Good, because the producer-facing contract is field-level (`partition_key` else `tenant_id`) and does not depend on native Kafka producer behavior
* Good, because the broker is authoritative — no producer-set `partition` to drift on refactor
* Good, because the SDK still owns hashing for outbox routing, so per-`(topic, partition)` outbox order is preserved synchronously
* Good, because adding `partition_key` to the schema is a low-cost extension point that producers opt into when needed
* Bad, because Murmur3 is not cryptographic — adversarial keys can collide on one partition (accepted; producer threat model is "trusted modules")
* Bad, because the broker now hashes once per ingest (accepted; sub-microsecond cost)

### Earlier Draft — Add Explicit `partition` Producer Override (rejected on revision)

**Description**: The initial 2026-05-06 draft kept all three paths: `partition_key` hashed, `subject` fallback hashed, and explicit `event.partition` producer override. The override was justified for "deterministic test fixtures, replaying events from another system that already picked partitions, and operator-driven traffic shaping experiments."

* Good, because the escape hatch covered niche use cases without bloating the default path
* Good, because producers replaying historical data could preserve the original partition numbers
* Bad / decisive against, because **a producer refactor that switches a code-path from "set `partition_key`" to "set `partition` directly" quietly breaks per-key ordering on a live topic**, and the broker has no way to tell whether the producer *meant* to bypass the hash. The producer override is a refactor-induced bug surface that's invisible in CI / staging and only manifests as a partition-ordering anomaly in production.
* Bad, because the original niche use cases re-decompose cleanly:
  - **Test fixtures**: use literal `partition_key` values; the hash is deterministic. Same partition guaranteed by Murmur3.
  - **Cross-system replay**: preserve the *source system's partition key*, not its partition number. Source N's partition layout is irrelevant once events land in our broker.
  - **Operator-driven traffic shaping**: this is an operator-side concern, handled by operational tooling (replay job that re-emits events with synthesized `partition_key` values), not by a producer-facing API.
* Bad, because every niche use case the override served can be served by `partition_key` alone, but the silent-ordering-bug class only exists with the override path
* Captured as the rejected alternative; the revision moves all override semantics into "Rejected Alternatives" for the historical record

### Always Derive Partition From `event.subject`

**Description**: Drop `partition_key` from the schema; `partition = murmur3(subject) % N` always.

* Good, because the schema is one field smaller
* Bad, because `subject` and partition key are not always the same — audit aggregation per tenant, system events with no domain subject, deliberate fan-out for non-causal events all need a different key
* Bad, because the alternative for producers needing a different grouping becomes "synthesize a different `subject`" — overloading the subject field, which is supposed to identify the entity the event is about
* Bad, because adding `partition_key` later is a one-way schema migration; not adding it now is a one-way lock-in

### Round-Robin With No Key Affinity

**Description**: SDK assigns `partition = next_counter % N` per call, ignoring keys.

* Good, because partition utilization is even by construction
* Bad, because it violates the design's central per-topic-ordering guarantee; two events about the same subject end up on different partitions
* Bad, because idempotent producer retry becomes non-deterministic
* Bad, because the only legitimate niche (high-volume non-causal events that want even spread) is already covered by `partition_key = uuid_v4()` per event in the chosen design

### Custom Pluggable Partitioner Trait in SDK (MVP)

**Description**: Expose a `Partitioner` trait in `cf-gears-event-broker-sdk`; the default impl is Murmur3-mod-N; users can register their own.

* Good, because it is maximally extensible
* Bad, because a pluggable partitioner that disagrees across producer instances on the same topic silently breaks per-key ordering — one Pod hashes with FNV, the other with Murmur3, and a fraction of keys land on different partitions
* Bad, because it expands the public SDK surface before any concrete second use case has been identified (YAGNI)
* Bad, because the broker is now authoritative on partition assignment — a custom SDK partitioner that disagrees with the broker's Murmur3 simply gets `400 PartitionHashMismatch` on every publish
* Captured as a post-MVP extension in [More Information](#more-information) if and when a real second use case appears

## More Information

- **Sticky-batch partitioning post-MVP**: Kafka 2.4+ offers a "sticky batch" partitioner that keeps consecutive keyless events on the same partition for batching efficiency, then rotates. Likely worth offering as an opt-in once the SDK gains true batch-publish performance work; deferred.
- **Pluggable Partitioner trait**: if a real second use case appears (e.g., a producer wanting weighted partition selection for hot-tenant isolation), the SDK could expose a `Partitioner` trait — but the broker would still be authoritative and reject mismatches, so any pluggable scheme would need an explicit broker-side contract. Decision deferred until a concrete request lands.
- **Hash function evolution**: Murmur3 has known weaknesses against adversarial inputs. The threat model treats producers as trusted, but if the broker ever opens to untrusted producers (e.g., a public ingest endpoint), it requires a separately versioned keyed partition algorithm and a migration plan that preserves existing topic assignments. Out of scope for MVP.
- **`meta.partition_hint`**: an internal SDK-stamped optimization the broker may accept to short-circuit re-hashing once cross-validated; not part of the public producer API. The SDK MAY omit it; the broker MUST handle its absence gracefully.

External references:

- MurmurHash3 reference (Austin Appleby): <https://github.com/aappleby/smhasher/wiki/MurmurHash3>
- CloudEvents `subject` attribute (semantic for "what the event is about"; reinforces why `subject` and partition-key may differ): <https://github.com/cloudevents/spec/blob/v1.0.2/cloudevents/spec.md#subject>
- RFC 2119 — keyword definitions used above (MUST, SHOULD, MAY): <https://www.rfc-editor.org/rfc/rfc2119>
- RFC 9457 — Problem Details, used for error response shapes: <https://www.rfc-editor.org/rfc/rfc9457>

## Traceability

- **PRD**: [PRD.md](../PRD.md)
  - `cpt-cf-evbk-fr-publish-single` — single-event publish; partition is broker-derived
  - `cpt-cf-evbk-fr-publish-batch` — batch publish requires same `(topic, partition)` for all events (broker derives partition; producer guarantees a batch shares one `partition_key` or one `tenant_id`-default bucket)
  - `cpt-cf-evbk-fr-producer-modes` — chained / monotonic dedup uses chain check on `evbk_producer_state(producer_id, topic, partition)`; partition determinism on retry is the dedup invariant
- **DESIGN**: [DESIGN.md](../DESIGN.md)
  - §1.1 Architectural Vision — per-topic ordering centrality
  - §2.1 Design Principles — Per-topic ordering, Immutable log
  - §3.1 Domain Model — `Topic.partitions`, "Partition Count" subsection
  - §3.2 Producer Modes — references [ADR-0004](0003-idempotent-producer-protocol.md)
  - §3.6 Two Sequences — producer chain in `meta` / server-assigned `sequence` (per [ADR-0003](0002-event-schema.md))
  - `evbk_producer_state` — keyed by `(producer_id, topic, partition)`
- **Related ADRs**:
  - [`0002-event-schema`](0002-event-schema.md) — canonical event shape; `partition_key` placement (body, optional); `partition` is `readOnly` (server-stamped on read)
  - [`0003-idempotent-producer-protocol`](0003-idempotent-producer-protocol.md) — chain dedup is keyed by `(producer_id, topic, partition)`; partition determinism is the chain-correctness invariant
