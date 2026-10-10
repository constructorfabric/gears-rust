---
status: accepted
date: 2026-10-07
---

# Apalis with a transactional Outbox

**ID**: `cpt-cf-durable-execution-adr-apalis-transactional-outbox`

## Table of Contents

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Apalis with a transactional toolkit Outbox](#apalis-with-a-transactional-toolkit-outbox)
  - [Pure Apalis with separate journal and adapter transactions](#pure-apalis-with-separate-journal-and-adapter-transactions)
  - [Pure Apalis with a redesigned shared SQLx transaction integration](#pure-apalis-with-a-redesigned-shared-sqlx-transaction-integration)
- [More Information](#more-information)
  - [Crash windows and guarantees](#crash-windows-and-guarantees)
  - [Versions and sources](#versions-and-sources)
  - [Conditions for reconsideration](#conditions-for-reconsideration)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

The durable-execution journal must persist each state transition and its delivery
intent in one transaction; otherwise a crash can strand a workflow. Journal
writes use scoped toolkit-db transactions, but the official Apalis PostgreSQL
adapter uses SQLx transactions on a separate pool. The integration must preserve
atomicity without bypassing toolkit-db scope enforcement.

## Decision Drivers

- Atomic journal changes and delivery intents.
- Recovery across commit, enqueue and acknowledgement failures.
- Preservation of checkpoints, epochs, cancellation, coalescing and fencing.
- Scoped domain CRUD through toolkit-db and service authorization through AuthN/PDP.
- Reuse of the existing PostgreSQL worker integration without changing DB ownership.
- Gear-owned migrations and workspace SQLx/TLS policy.

## Considered Options

- Apalis with a transactional toolkit Outbox.
- Pure Apalis with separate journal and adapter transactions.
- Pure Apalis with a redesigned shared SQLx transaction integration.

## Decision Outcome

Chosen option: **Apalis with a transactional toolkit Outbox**. The existing
scoped toolkit-db transaction records journal changes and delivery intents
atomically, while Apalis retains its role as the worker runtime.

### Consequences

- Commit journal changes, normalized state, event history and toolkit Outbox
  records in one transaction. Bind Outbox Wake handles to that transaction;
  fire them only after a successful commit. Rollback or cancellation discards them. The cold reconciler
  must recover lost post-commit wakes.
- The leased Outbox handler must check journal generation, activity and status
  before enqueueing in Apalis. Acknowledge the Outbox only after both enqueue
  and the legacy intent acknowledgement succeed.
- Delivery payloads contain only run ID, activity ID and delivery generation.
  Rust handlers register in process. Credentials, workflow inputs and checkpoint
  results must stay out of transport payloads.
- Workers must claim and checkpoint through journal CAS, renewable leases and fencing.
  The journal owns business retries and Retry/Resume; Apalis gets one transport
  attempt. Reconciliation must recover expired claims and overdue acknowledged
  deliveries that were never claimed.
- Service operations must reauthorize through AuthN/PDP. Authorization failures pause
  delivery or execution without recording a workflow business failure.
- The official adapter's SQLx pool and setup code are an exception restricted to
  the service schema `apalis`. Journal and domain CRUD remain scoped through
  toolkit-db. All named queues share one adapter pool and inherit workspace
  SQLx/TLS features.
- The singleton gear owns fixed `durable_*` tables, toolkit Outbox prefix
  `durable_delivery_outbox` and their migrations. Cooperating processes share
  these tables and the adapter's fixed `apalis` schema. Configurable table-prefix
  families are outside this version's scope.

### Confirmation

Review transaction boundaries and restricted SQLx usage, then run these checks
against a disposable PostgreSQL database:

- `outbox_restart_recovers_each_handoff_window_and_duplicates_do_not_repeat_checkpoints`
  covers both handoff failure windows and duplicate delivery without repeating attempts.
- `framework_delivery_is_atomic_with_checkpoint_and_has_no_input` and
  `unavailable_publisher_rolls_back_run_and_intent` check atomic publication
  and rollback without exposing workflow input.
- `killed_worker_recovers_effects_and_preserves_successful_checkpoint` checks
  worker termination, effect replay and preservation of successful checkpoints.
- Journal and storage tests cover stale CAS/fencing, lost-delivery reconciliation,
  Retry/Resume, parallel groups and repeatable migrations with legacy IDs.
- Authorization tests cover tenant isolation, AuthN/PDP outages and access
  revocation during execution without recording a business cancellation.

Run with Docker available (the workspace pins the PostgreSQL image). `DURABLE_TEST_PG_URL` optionally selects an existing test database:

```text
cargo test -p cf-gears-durable-execution --features integration --lib -- --skip infra::storage::process_tests::worker_child --test-threads=1
```

SQLite journal tests complement runtime tests. Architecture lints require
`cargo-gears`, which was initially unavailable in the extraction environment.

## Pros and Cons of the Options

### Apalis with a transactional toolkit Outbox

- Good, because the existing toolkit-db transaction commits journal changes and
  delivery intents atomically without exposing raw journal DB access.
- Good, because persistent Outbox records recover failures before enqueue, and
  journal CAS/fencing handles duplicate transport deliveries.
- Bad, because it adds a second pool, service schema, delivery hop, Outbox latency
  and dependencies, including the selected RC APIs.
- Neutral, because delivery remains at-least-once. External effects need
  idempotency even when journal transitions are fenced.

### Pure Apalis with separate journal and adapter transactions

- Good, because it removes the Outbox hop and its operational machinery.
- Bad, because a journal-first commit can lose delivery after a crash. Enqueueing
  first can deliver work before the journal commits or after it rolls back.
- Bad, because connecting separate transactions to the same PostgreSQL database
  does not make them atomic.

### Pure Apalis with a redesigned shared SQLx transaction integration

- Good, because a shared SQLx transaction could atomically persist journal changes
  and Apalis jobs without the Outbox hop.
- Bad, because it requires a DB integration that preserves scope enforcement,
  transaction ownership and gear migrations.
- Bad, because that redesign expands the extraction beyond moving the existing
  journal-integrated executor.

## More Information

### Crash windows and guarantees

| Failure point | Durable state and recovery |
| --- | --- |
| Before journal transaction commit | Journal changes and new delivery records roll back together. |
| After commit, before enqueue | The Outbox retains the intent; retries eventually enqueue it. |
| After Apalis enqueue, before Outbox acknowledgement | The Outbox can enqueue a duplicate; current generation, CAS and fencing reject stale ownership. |
| After acknowledgement, before claim | Reconciliation republishes overdue unclaimed work. |
| After external effect, before checkpoint | The effect can repeat. Handlers must use the stable idempotency key. |
| After checkpoint | Recovery skips persisted successful activities. |

Delivery is at-least-once. CAS/fencing protects accepted journal transitions,
not arbitrary external side effects. External systems must honor idempotency
and, where needed, the execution fence.

### Versions and sources

Scope: embedded durable-execution system gear 0.1.0. Integration verified on
2026-10-07 with toolkit-db 0.16.2, apalis/apalis-core 1.0.0-rc.10,
apalis-postgres 1.0.0-rc.9 and SQLx 0.9.0.

- [Official PostgreSQL adapter](https://docs.rs/apalis-postgres/latest/apalis_postgres/).
  The pinned rc.9 crate source was inspected for schema setup, enqueue,
  worker heartbeat and orphan recovery. The latest docs do not pin a version.
- Local toolkit-db `src/outbox/core.rs`, `wake.rs` and leased strategy;
  workspace base commit `fa9334a34`.
- [ADR 0002](0002-task-execution-framework-selection.md).

### Conditions for reconsideration

Revisit if toolkit-db can safely share an adapter transaction or Outbox gains
a managed executor suitable for long-running work. Any change must preserve
the atomicity, authorization and recovery guarantees above.

## Traceability

- **PRD**: [Durable execution](../PRD.md).
- **DESIGN**: [Durable execution](../DESIGN.md).
- **Related decision**:
  [Task execution framework selection](0002-task-execution-framework-selection.md)
  (`cpt-cf-durable-execution-adr-task-execution-framework-selection`).

The decision constrains:

- [Journal publication](../../durable-execution/src/infra/storage/repository.rs)
  and [atomic starts](../../durable-execution/src/infra/storage/start.rs):
  transaction ownership and post-commit Wake handling.
- [Outbox handoff](../../durable-execution/src/infra/outbox.rs) and
  [Apalis adapter](../../durable-execution/src/infra/apalis.rs): delivery and
  restricted adapter DB access.
- [Executor](../../durable-execution/src/infra/executor.rs) and
  [service authorization](../../durable-execution/src/infra/authorization.rs):
  authorized claims, heartbeats, checkpoints and recovery.
- [Handoff fault tests](../../durable-execution/tests/integration/outbox_tests.rs)
  and [subprocess recovery tests](../../durable-execution/tests/integration/process_tests.rs):
  verification of delivery and checkpoint guarantees.
