---
status: accepted
date: 2026-09-10
decision-makers: BSS Orders team
---

# ADR-0006: Events Use the Platform Transactional Producer Outbox

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Platform DbProducer with toolkit outbox (chosen)](#platform-dbproducer-with-toolkit-outbox-chosen)
  - [Orders-owned transactional outbox](#orders-owned-transactional-outbox)
  - [Synchronous publish inside the transaction](#synchronous-publish-inside-the-transaction)
  - [Publish after commit, in the same request](#publish-after-commit-in-the-same-request)
  - [Change data capture off the write-ahead log](#change-data-capture-off-the-write-ahead-log)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-orders-lifecycle-adr-outbox-publication`

## Context and Problem Statement

Eleven order events are a published contract consumed by three sibling gears. Every event
originates in a transition that also writes Orders state, version, idempotency and audit data in
one database transaction. The repository already provides the supported producer path:
`event-broker-sdk::DbProducer` with its `outbox` feature, backed by `toolkit_db::outbox`.

PRD §7.1 sets transition latency at `p95 < 1 s` and describes it as **“durable write + event
publish”**. A transactional producer outbox deliberately separates durable enqueue from broker
publication. The decision must therefore address both atomicity and that explicit divergence,
without duplicating platform sequencing, leasing, retry, dead-letter and vacuum machinery inside
Orders.

## Decision Drivers

* An event-declaring state change and its durable producer message must commit atomically.
* No Event Broker call may occur while the aggregate row lock is held.
* Existing platform producer/outbox capabilities should be reused rather than forked.
* Delivery is at-least-once, so event identity and consumer de-duplication are mandatory.
* `orderId` must route one order's events to one broker partition.
* Permanent bad messages must not block unrelated orders indefinitely.
* Orders is authoritative state; its event stream is notification, not an event-sourced ledger.
* The Event Broker runtime is not yet available even though its SDK has landed, so readiness must
  expose that dependency honestly.

## Considered Options

* **Platform `DbProducer` with toolkit outbox** — enqueue a typed event in the transition
  transaction and let the SDK/toolkit workers publish it
* **Orders-owned transactional outbox** — own a table, shard leases, retry state, dead letters and
  re-drive logic in this gear
* **Synchronous publish inside the transaction** — call Event Broker before commit
* **Publish after commit, in the same request** — commit, then publish, with reconciliation for the
  gap
* **Change data capture off the write-ahead log** — derive events from database replication

## Decision Outcome

Chosen option: **`event-broker-sdk::DbProducer` with feature `outbox`, backed by
`toolkit_db::outbox`, in managed `ProducerMode::Chained`**. Orders constructs the typed event and
calls the bound `ProducerOutbox::enqueue` with the active transition runner. The platform owns the
producer registration, opaque producer envelope, local sequence, partition mapping, leases,
processing, retry classification, dead-letter lifecycle and vacuum.

The producer queue is `bss-orders-events`, configured with `Partitions::of(16)` and the toolkit
high-throughput profile. `orderId` is the GTS event partition key. Event Broker topic partition
count is explicit configuration and must match the deployed broker; the SDK's default of eight is
used only when the deployment uses eight. Eager schema preparation, managed producer registration,
queue registration and worker startup are readiness requirements.

**The PRD latency baseline remains governing and compliance requires verification.** PRD §7.1
and AC-17 require durable write plus event publish at p95 < 1 s. Asynchronous publication does
not inherently prevent meeting that target, but commit completion alone cannot prove it.
The former separate **30 s p95** target was borrowed from Orders Workflow and is an unapproved
proposal, reopened in `DECISIONS.md` D-41 / Q-16. Product and Architecture must confirm measurement
boundaries and review load-test evidence before approving any relaxation. `DESIGN.md` §4.1
defines request-to-commit, commit-to-broker acceptance and the full operation-to-broker path;
downstream processing is separate.

### Consequences

* **Atomic durable notification.** An event-declaring transition cannot commit without its toolkit
  producer message, and an aborted transition cannot publish one.
* **No custom Orders outbox.** There is no `orders_event_outbox`, Orders shard selector, lease
  protocol, retry bookkeeping, delivered-row purge or Orders-owned dead-letter schema. Platform
  migration families are not counted as Orders tables.
* **At-least-once delivery.** Consumers de-duplicate by the event envelope ID. Accepted,
  persisted and duplicate broker outcomes acknowledge the toolkit message.
  Broker idempotency is separate: managed Chained mode uses producer ID, predecessor and
  sequence within the topic/broker partition, not `event.id`. The SDK takes the sequence from
  `OutboxMessage.seq` and manages the predecessor cursor; Orders supplies neither a custom
  sequence nor an event-ID broker token. Foundation §4.4 requires lost-response/restart retry
  tests and separate consumer de-duplication tests; these remain pending implementation.
* **Availability-oriented ordering.** Events for one order route to the same broker partition and
  remain FIFO during normal processing and transient retry. The SDK maps `(topic, broker
  partition)` to one toolkit queue partition, so a transient retry blocks that whole toolkit
  partition. Transport and rate-limit faults return `Retry` without an Orders attempt cap.
* **Permanent rejection may create a gap.** Invalid envelopes/schema, unrecoverable producer
  identity and persistent chained-sequence divergence return `Reject`; toolkit-db writes an
  inspectable dead letter and advances the queue-partition cursor. Later events may proceed. A
  strict per-order barrier was rejected because the platform outbox does not provide one and
  recreating it would restore the custom implementation this decision removes.
* **Consumers reconcile with authority.** Consumers must use event ID for de-duplication and
  `orderVersion` plus resulting state with an authoritative Orders read to reject stale or
  inapplicable work. They must not reconstruct order state or assume every prior event was seen.
  This freshness read deliberately qualifies D-67's no-callback rationale; Product/Architecture
  reconciliation of PRD §9.2 remains open under Q-25. Consumer services need target-scoped
  `order × read` grants, not merely root-stream access. Foundation §4.4 requires durable
  pending work on unavailable validation and event/action-specific applicability rules;
  a different state or a failed read alone must not silently discard work.

* **No Orders re-drive endpoint.** Its removal depends on shared platform recovery:
  `cpt-cf-bss-orders-lifecycle-upreq-event-broker-dead-letter-recovery`
  ([`UPSTREAM_REQS.md §2.7`](../UPSTREAM_REQS.md#27-event-broker)). The SDK must safely republish
  the original event with unchanged event ID and business payload, handling producer identity and
  chained sequencing; authenticated operator tooling must authorize and audit recovery. No new
  Orders transition is required. Toolkit's claim operation alone is insufficient. Both SDK
  recovery and the operator interface are open production release prerequisites.
* **Payload bound.** The serialized producer envelope must fit toolkit-db's 64 KiB payload limit.
  Capacity tests cover worst-case `OrderSubmitted` and `OrderCompleted` at the 200-line cap.
* **Runtime gate.** `docs/GEARS.md` currently says “SDK landed — impl crate TODO”. Orders cannot be
  ready for event-producing traffic until `EventBrokerApi` has a runtime implementation and the
  producer integration gate passes.
* **Audit and events can disagree transiently.** Committed Orders state and audit remain true while
  a message waits, retries or is dead-lettered. Reads therefore use authoritative Orders state,
  never event replay.

### Confirmation

**Verifiable today:** the SDK's producer outbox enqueues with a caller-supplied database runner;
uses toolkit `OutboxMessage.seq`; recovers managed chained cursors from Event Broker; treats
accepted, persisted and duplicate as success; returns `Retry` for transport/rate-limit errors; and
returns `Reject` for permanent errors. Toolkit-db retains a partition cursor on `Retry` and writes a
dead letter then advances it on `Reject`. Toolkit outbox rejects payloads above 64 KiB.

**Planned with the Orders implementation:**

1. fault injection proving state, audit, idempotency and producer enqueue commit or roll back
   together;
2. duplicate-delivery tests proving consumers key on event ID;
3. transient-failure tests proving queue-partition FIFO and recovery;
4. permanent-rejection tests proving a dead letter is visible, order state is unchanged and later
   events may proceed;
5. stale/out-of-order contract tests proving Workflow reads authoritative Orders state/version;
6. largest-envelope tests at the 200-line cap;
7. readiness tests for absent Event Broker runtime, schema preparation failure, producer
   registration failure and broker-partition mismatch; and
8. correlated full-path latency measurement against the governing PRD baseline at expected load,
   with backlog/retries, visible incomplete deliveries and tested delayed-delivery/dead-letter
   alerts; Q-16 confirms boundaries, observation window and tail criteria before production.

## Pros and Cons of the Options

### Platform DbProducer with toolkit outbox (chosen)

* Good, because enqueue and business writes share one transaction.
* Good, because it removes duplicated schema and worker logic.
* Good, because typed validation, producer identity and chained cursor recovery are platform-owned.
* Bad, because a transient failure blocks a whole toolkit queue partition.
* Bad, because permanent rejection permits a notification gap and consumers must reconcile with
  authoritative state.
* Bad, because commit success alone cannot establish the PRD's write-plus-publish latency;
  asynchronous delivery requires correlated measurement and operational monitoring.

### Orders-owned transactional outbox

* Good, because it could implement strict per-order head-of-line suspension and a bespoke REST
  re-drive.
* Bad, because it duplicates platform tables, leases, sequencing, retry, DLQ and vacuum behavior.
* Bad, because Orders would own subtle distributed-delivery correctness outside its business
  boundary.

### Synchronous publish inside the transaction

* Good, because it matches the PRD's “durable write + event publish” wording literally.
* Bad, because broker latency and outages become order-transition latency and outages.
* Bad, because a database commit and remote publish still cannot be one atomic operation.

### Publish after commit, in the same request

* Good, because the aggregate lock is released before the network call.
* Bad, because the atomicity gap remains and still requires reconciliation.
* Bad, because callers pay broker latency without gaining atomicity.

### Change data capture off the write-ahead log

* Good, because it adds no application write.
* Bad, because curated GTS payload construction would move outside the gear boundary.
* Bad, because it couples consumers to Orders physical schema.

## More Information

Superseded by nothing. This revision replaces the earlier Orders-owned sharded drain with the
platform producer outbox now present in the repository. `DECISIONS.md` D-41 records the capacity
and delivery budgets; D-42 records synchronous port deadlines; D-87 records the revised ordering
posture.

## Traceability

- **PRD**: [`../PRD.md`](../PRD.md) — §7.1 transition latency and audit completeness, §12 AC-17
- **DESIGN**: [01 §3.6](../features/01-foundation.md#contract-01-3-6), §3.7
  *Platform-managed producer persistence*, §3.8, §4.4

This decision directly addresses:

* `cpt-cf-bss-orders-lifecycle-fr-order-events` — eleven typed notifications use the supported
  platform producer path with at-least-once delivery and event-ID de-duplication;
* `cpt-cf-bss-orders-lifecycle-nfr-order-transition-latency` — publication is outside the commit
  path; Q-16 requires boundary clarification and performance evidence, with a PRD amendment only
  if Product and Architecture approve a changed requirement;
* `cpt-cf-bss-orders-lifecycle-nfr-order-audit-completeness` — enqueue shares the transition
  transaction, so an event-declaring transition cannot silently omit its durable producer message;
* `cpt-cf-bss-orders-lifecycle-component-transition-engine` — the engine constructs and enqueues
  event semantics, while platform workers own delivery state.
- **Decisions register**: [`../DECISIONS.md`](../DECISIONS.md) — D-17, D-23, D-24, D-41, D-42,
  D-58, D-87, D-91, Q-16, Q-19, Q-26
