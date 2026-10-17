---
status: accepted
date: 2026-09-10
decision-makers: BSS Orders team (Architecture)
---
# ADR-0008: Process Events Are Published Via The Platform Producer Outbox, Disjoint From Lifecycle's State Events


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Platform DbProducer with toolkit outbox (chosen)](#platform-dbproducer-with-toolkit-outbox-chosen)
  - [Workflow-owned transactional outbox (previous choice, rejected)](#workflow-owned-transactional-outbox-previous-choice-rejected)
  - [Synchronous publish inside the transition](#synchronous-publish-inside-the-transition)
  - [Publish after commit, in the same request](#publish-after-commit-in-the-same-request)
  - [Change data capture off the write-ahead log](#change-data-capture-off-the-write-ahead-log)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-orders-workflow-adr-outbox-process-events`
## Context and Problem Statement

Orders Workflow must publish six named process events (`OrderFulfillmentStarted`,
`OrderFulfillmentStepCompleted`, `OrderFulfillmentCompleted`, `OrderFulfillmentAborted`,
`OrderApprovalRequested`, `OrderApprovalEscalated`) with at-least-once delivery and consumer-side
de-duplication, and must not publish order-**state** events — that set is owned and enumerated
exclusively by Orders Lifecycle. Every process event originates in a step whose unit of work also
writes the step's audit entry and settles its idempotency record in one database transaction.

The repository already provides the supported producer path: `event-broker-sdk::DbProducer` with
its `outbox` feature, backed by `toolkit_db::outbox`. The sibling gear adopted it in
[Lifecycle ADR-0006](../../../orders-lifecycle/docs/ADR/0006-cpt-cf-bss-orders-lifecycle-adr-outbox-publication.md)
and recorded the consequences in its D-17, D-87 and D-95. The earlier revision of this ADR chose a
Workflow-owned outbox table (`owf_event_outbox`) with its own drain, lease, retry cap, dead-letter
column and purge; keeping it would fork the platform's sequencing, leasing, retry classification,
dead-letter and vacuum machinery inside this gear. The decision must fix the publication path,
keep the process-event set disjoint from Lifecycle's, and address the PRD's p95 < 30 s delivery
target honestly.

## Decision Drivers

* An event-declaring committed step and its durable producer messages — zero or more of the
  declared type, at most one per subject its contract names (`DECISIONS.md` D-179) — must commit
  atomically, in the same unit of work as the audit entry and the idempotency settlement.
* No Event Broker call may occur inside a durable-execution step transaction.
* Existing platform producer/outbox capabilities should be reused rather than forked; the sibling
  gear has already made this move, so deviating from it needs its own rationale.
* Delivery is at-least-once, so event identity and consumer de-duplication are mandatory.
* Every payload carries the process `correlationId` for consumer-side correlation; `orderId` must
  route one order's process events to one broker partition of this gear's topic.
* The six process events must remain strictly disjoint from Lifecycle's state-event set; no
  consumer may see dual publication of one semantic change.
* The PRD sets a p95 < 30 s delivery target for the six events
  (`cpt-cf-bss-orders-workflow-nfr-owf-event-latency`). Commit success alone cannot prove it;
  measurement is required.
* The Event Broker runtime is not yet available even though its SDK has landed, so readiness must
  expose that dependency honestly.

## Considered Options

* **Platform `DbProducer` with toolkit outbox** — enqueue a typed event in the step's transaction
  and let the SDK/toolkit workers publish it
* **Workflow-owned transactional outbox** — the previous choice: own `owf_event_outbox`, a drain,
  leases, a retry cap, `dead_lettered_at` and a purge in this gear
* **Synchronous publish inside the transition** — call Event Broker before commit
* **Publish after commit, in the same request** — commit, then publish, with a reconciliation
  sweep for the gap
* **Change data capture off the write-ahead log** — derive events from database replication

## Decision Outcome

Chosen option: **`event-broker-sdk::DbProducer` with feature `outbox`, backed by
`toolkit_db::outbox`, in managed `ProducerMode::Chained`**, exactly as Lifecycle ADR-0006 decides
for the sibling gear. Workflow constructs the typed event and calls the bound
`ProducerOutbox::enqueue` with the step's transaction runner. The platform owns producer
registration, the opaque producer envelope, local sequence, partition mapping, leases, processing,
retry classification, dead-letter lifecycle and vacuum.

The producer queue is `bss-orders-workflow-events`, configured with `Partitions::of(16)` and the
toolkit high-throughput profile. The six events publish to this gear's own topic,
`gts.cf.core.events.topic.v1~cf.bss._.orders_workflow.v1`, which the gear registers in
`types-registry` before readiness and names in the abstract event type's `x-gts-traits` with
`partition_key` `/subject` (`design/01-foundation.md` §4.7, `DECISIONS.md` D-177), as Lifecycle
does for its own topic. `orderId` is therefore the partition key within that topic, and every
event for one order keeps per-order ordering there; Lifecycle's state events are on Lifecycle's
topic, and nothing orders the two streams against each other, so a consumer correlates them by
`orderId` and `orderVersion`, never by partition. Event Broker topic partition count is explicit configuration and must match the
deployed broker. Envelope tenancy is platform-root per Lifecycle D-95; `resourceTenantId` and
`sellerTenantId` remain payload fields. Eager schema preparation, managed producer registration,
queue registration and worker startup are readiness requirements.

**The enqueue is sequenced with the local state mutation, never after a cross-gear call.** An
event's producer message is enqueued in the same transaction as the *local* process-state change
that makes the event true, and that transaction commits before any synchronous call to another
gear. `OrderFulfillmentAborted` is the case that forces the rule: it reports an order-level abort
that is also reflected onto Orders Lifecycle, and Lifecycle's acknowledgement is a network call
that cannot share a transaction with this gear's write. The correct order is: commit the local
abort transition **and** its enqueue together, then make the Lifecycle call as a separately
retried, idempotent step under the Lifecycle transition-call idempotency family (ADR-0006). The
event describes what this gear decided, a local fact; it does not describe what Lifecycle
acknowledged. The same rule binds all six events.

The process-event set stays **non-overlapping** with Lifecycle's order-state event set: Workflow
**MUST NOT** publish order-state events. Lifecycle's state event on the `fulfillment_failed`
transition is `OrderFulfillmentFailed`; Workflow's process event when order-level fulfillment
fails or the workflow is cancelled is `OrderFulfillmentAborted`. The two names refer to
related-but-distinct facts owned by different systems of record and are not interchangeable.

**The PRD delivery target remains governing and compliance requires verification.**
`cpt-cf-bss-orders-workflow-nfr-owf-event-latency` requires p95 < 30 s from internal state change
to event delivery. Asynchronous publication does not inherently prevent meeting that target, but
commit completion alone cannot prove it, exactly as Lifecycle ADR-0006 states for its 1 s target.
The measurement is producer-queue lag — enqueue instant to broker acceptance — observed at
expected load with backlog and retries present; `DECISIONS.md` Q-07 is resolved on that basis by
D-58, and any relaxation of the number is a Product decision, not a silent widening.

### Consequences

* **Atomic durable notification.** An event-declaring step cannot commit without the toolkit
  producer messages its contract requires (zero or more, D-179), and an aborted step cannot
  publish one.
* **No custom Workflow outbox.** There is no `owf_event_outbox`, no Workflow drain, lease, retry
  cap, delivered-row purge, `dead_lettered_at` column or Workflow-owned dead-letter schema for
  outbound events. Platform migration families are not counted as Workflow tables; the engine
  owned seven tables at this decision — nine once D-59 added the two audit checkpoint tables —
  and none of them is an outbox. `owf_dead_letter_record` was the *inbound* store of ADR-0009 and
  gained no outbound role; it is now retired, because inbound dead letters belong to the platform
  trigger path (D-72).
* **At-least-once delivery.** Consumers de-duplicate by the event envelope ID. Accepted,
  persisted and duplicate broker outcomes acknowledge the toolkit message. Broker idempotency is
  separate: managed Chained mode uses producer ID, predecessor and sequence within the
  topic/broker partition, not `event.id`. The SDK takes the sequence from `OutboxMessage.seq` and
  manages the predecessor cursor; Workflow supplies neither a custom sequence nor an event-ID
  broker token, and the former per-correlation `sequence` ordinal is withdrawn.
* **Availability-oriented ordering.** Events for one order route to one broker partition of this
  gear's topic and remain FIFO during normal processing and transient retry. The SDK maps `(topic, broker
  partition)` to one toolkit queue partition, so a transient retry blocks that whole toolkit
  partition. Transport and rate-limit faults return `Retry` without a Workflow attempt cap.
* **Permanent rejection may create a gap** (Lifecycle D-87). Invalid envelopes/schema,
  unrecoverable producer identity and persistent chained-sequence divergence return `Reject`;
  toolkit-db writes an inspectable dead letter and advances the queue-partition cursor. Later
  events may proceed. A strict per-correlation barrier is not rebuilt beside the platform. An
  unknown producer identity is the bulk case: the SDK registers a replacement for future enqueues
  and rejects every message still queued under the old id, so one rotation dead-letters every
  unpublished event, whose recovery is the shared dead-letter ask (`DECISIONS.md` D-178).
* **Consumers reconcile with authority.** Consumers of the six events must de-duplicate by event
  ID and verify `orderVersion` plus resulting state against an authoritative Lifecycle read before
  acting; a timeout or authorization failure on that read is not evidence of staleness. The same
  obligation binds this gear as a consumer of Lifecycle's stream
  ([`02 §2.1`](../design/02-triggers-and-start.md#21-design-principles)) and of Subscriptions
  confirmations ([`05 §2`](../design/05-provisioning-intents.md#2-principles--constraints)).
* **Payload bound.** Each serialized producer envelope must fit toolkit-db's 64 KiB payload limit.
  The largest payload is `OrderFulfillmentCompleted.lineOutcomes[]` at the 200-line cap; it is a
  capacity test, not an assumption.
* **No operator re-drive surface in this gear.** Dead-letter recovery uses the shared platform
  operator interface and SDK republication that Lifecycle requests as
  `cpt-cf-bss-orders-lifecycle-upreq-event-broker-dead-letter-recovery`; Workflow co-signs it in
  [`UPSTREAM_REQS.md §2.7`](../UPSTREAM_REQS.md#27-event-broker) and exposes no REST re-drive.
  Toolkit's claim operation alone is insufficient.
* **Runtime gate.** `docs/GEARS.md` currently says “SDK landed — impl crate TODO”. Workflow cannot
  be ready for event-producing traffic until `EventBrokerApi` has a runtime implementation
  (`cpt-cf-bss-orders-lifecycle-upreq-event-broker-runtime`) and the producer integration gate
  passes.
* **Audit and events can disagree transiently.** Committed process state and the audit entry
  remain true while a message waits, retries or is dead-lettered. Reads use the gear's own audit
  trail and Lifecycle's authoritative order state, never event replay.

### Confirmation

**Verifiable today:** the SDK's producer outbox enqueues with a caller-supplied database runner;
uses toolkit `OutboxMessage.seq`; recovers managed chained cursors from Event Broker; treats
accepted, persisted and duplicate as success; returns `Retry` for transport/rate-limit errors; and
returns `Reject` for permanent errors. Toolkit-db retains a partition cursor on `Retry` and writes a
dead letter then advances it on `Reject`. Toolkit outbox rejects payloads above 64 KiB.

**Planned with the Workflow implementation:**

1. fault injection proving audit entry, idempotency settlement and producer enqueue commit or
   roll back together for every event-declaring step;
2. a crash injected between the committed local abort transition and the Lifecycle
   acknowledgement, proving the `OrderFulfillmentAborted` message is present and that no code path
   enqueues after a cross-gear call returns;
3. a test asserting no Workflow code path publishes a Lifecycle state event, and a naming-registry
   check that `OrderFulfillmentAborted` and `OrderFulfillmentFailed` are never used interchangeably;
4. duplicate-delivery tests proving consumers key on event ID;
5. transient-failure tests proving queue-partition FIFO and recovery;
6. permanent-rejection tests proving a dead letter is visible, process state is unchanged, no
   Orders dead-letter row is written (that table is retired, D-72), and later events may proceed;
7. largest-envelope tests for `OrderFulfillmentCompleted` at the 200-line cap;
8. readiness tests for absent Event Broker runtime, an unregistered or undeclared topic, schema
   preparation failure, producer registration failure and broker-partition mismatch; a
   producer-rotation test proving every message queued under the old id is dead-lettered and
   process state is unchanged; and
9. producer-queue lag measured against the 30 s p95 target at expected load, with backlog and
   retries present and the delayed-delivery/dead-letter alerts of `DESIGN.md §4.4` exercised.

## Pros and Cons of the Options

### Platform DbProducer with toolkit outbox (chosen)

* Good, because enqueue and the step's writes share one transaction.
* Good, because it removes a duplicated table, drain, lease and dead-letter schema from this gear.
* Good, because typed validation, producer identity and chained cursor recovery are platform-owned,
  and the two Orders gears publish through one path.
* Bad, because a transient failure blocks a whole toolkit queue partition.
* Bad, because permanent rejection permits a notification gap and consumers must reconcile with
  authoritative state.
* Bad, because commit success alone cannot establish the 30 s delivery target; asynchronous
  delivery requires correlated measurement and monitoring.

### Workflow-owned transactional outbox (previous choice, rejected)

* Good, because it could implement a strict per-correlation ordinal and a bespoke retry cap.
* Bad, because it duplicates platform tables, leases, sequencing, retry, dead-letter and vacuum
  behaviour, and diverges from the sibling gear's now-adopted path.
* Bad, because Workflow would own subtle distributed-delivery correctness outside its business
  boundary, plus a second dead-letter surface beside ADR-0009's inbound store.

### Synchronous publish inside the transition

* Good, because it minimises the gap between state change and event visibility.
* Bad, because broker latency and outages become step latency and outages inside a
  durable-execution transaction.
* Bad, because a database commit and a remote publish still cannot be one atomic operation.

### Publish after commit, in the same request

* Good, because the step's row locks are released before the network call.
* Bad, because the atomicity gap remains and still requires a reconciliation sweep — an outbox
  with extra steps and less rigour.
* Bad, because callers pay broker latency without gaining atomicity.

### Change data capture off the write-ahead log

* Good, because it adds no application write.
* Bad, because curated GTS payload construction would move outside the gear boundary.
* Bad, because it couples consumers to Workflow's physical schema, which ADR-0003 forbids reading
  as order state.

## More Information

This revision replaces the earlier Workflow-owned outbox and drain with the platform producer
outbox, following
[Lifecycle ADR-0006](../../../orders-lifecycle/docs/ADR/0006-cpt-cf-bss-orders-lifecycle-adr-outbox-publication.md)
and its register entries D-17 (platform producer outbox, no re-drive API), D-87 (partition
ordering; permanent reject may create a gap) and D-95 (platform-root tenancy) in
[`orders-lifecycle/docs/DECISIONS.md`](../../../orders-lifecycle/docs/DECISIONS.md). This ADR
does not repeat Lifecycle's eleven-event enumeration. `DECISIONS.md` D-58 records this decision
and its propagation; Q-07 is resolved by it.

## Traceability

- **PRD**: [PRD.md](../PRD.md) — §7 process-event delivery target
- **DESIGN**: [DESIGN.md](../DESIGN.md) §3.4, §3.5, §3.7, §3.8, §4.1, §4.4, §4.5;
  [`design/01-foundation.md`](../design/01-foundation.md) §3.2 *Platform event producer adapter*,
  §3.6 *Process Producer Outbox Message*, §3.7 *Platform-managed producer persistence*, §3.8, §4.7
- **Decisions register**: [`DECISIONS.md`](../DECISIONS.md) — D-58, Q-07
- **Upstream asks**: [`UPSTREAM_REQS.md §2.7`](../UPSTREAM_REQS.md#27-event-broker)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-bss-orders-workflow-fr-owf-process-events` — the six named process events use the
  supported platform producer path with at-least-once delivery and event-ID de-duplication, and
  stay disjoint from Lifecycle's state-event set, including the
  `OrderFulfillmentAborted`/`OrderFulfillmentFailed` naming split
* `cpt-cf-bss-orders-workflow-nfr-owf-event-latency` — publication is outside the commit path;
  producer-queue lag is the instrument that measures the 30 s target
* `cpt-cf-bss-orders-workflow-nfr-owf-audit` — enqueue shares the step's transaction, so an
  event-declaring step cannot silently omit the durable producer messages its contract requires
  (zero or more, D-179)
* `cpt-cf-bss-orders-workflow-component-foundation` (**platform event producer adapter**, the
  component `design/01-foundation.md` §3.2 declares under its unchanged identifier) — constructs
  the typed events and enqueues through the bound producer outbox; owns no table, drain or re-drive
* `cpt-cf-bss-orders-workflow-component-foundation` (**step executor**) — declares the event
  alongside the local state mutation it commits, which is where the atomicity rule binds
* `cpt-cf-bss-orders-workflow-component-approval-execution` (**approval gate manager**) and
  (**escalation timer owner**) — declare `OrderApprovalRequested` and `OrderApprovalEscalated`
* `cpt-cf-bss-orders-workflow-adr-manual-task-dead-letter-separation` — the former inbound-only
  `owf_dead_letter_record` is retired (D-72); a platform dead letter is broker evidence, never a Workflow record
