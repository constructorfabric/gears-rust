---
status: accepted
date: 2026-10-07
---

# Apalis with a transactional Outbox

**ID**: `cpt-cf-durable-execution-adr-apalis-transactional-outbox`

> **Superseded in part by [ADR 0005](0005-embeddable-library.md)**: durable execution is now a library that a consumer gear embeds.
> Read "the gear" as the engine, and "gear-owned migrations" as `migrations()` that the embedding gear applies.
> Service operations use the embedding gear's `Authorizer`, not AuthN credential exchange.
> The tables are now `durable_execution__*`, with `durable_execution__delivery_intents` and the `durable_execution__delivery_outbox` prefix.
> The publication decision stays accepted.


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Apalis with transactional ToolKit Outbox](#apalis-with-transactional-toolkit-outbox)
  - [Separate journal and adapter transactions](#separate-journal-and-adapter-transactions)
  - [Shared SQLx transaction integration](#shared-sqlx-transaction-integration)
- [More Information](#more-information)
  - [Failure Windows](#failure-windows)
  - [Historical Versions](#historical-versions)
  - [Conditions for Reconsideration](#conditions-for-reconsideration)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

A journal change and its publication intent must commit together.
The journal uses scoped ToolKit DB transactions.
The official Apalis adapter uses a separate SQLx pool.
Two separate transactions cannot supply this atomicity.

## Decision Drivers

- Atomic journal changes and publication intents.
- Recovery after commit, enqueue, or acknowledgment failure.
- SecureORM scopes and current service authorization.
- Gear-owned migrations and workspace SQLx/TLS settings.
- Existing PostgreSQL worker integration.

## Considered Options

1. Apalis with transactional ToolKit Outbox.
2. Separate journal and adapter transactions without Outbox.
3. A new shared SQLx transaction integration.

## Decision Outcome

Use Apalis with transactional ToolKit Outbox.
The journal transaction records state and publication commands.
The Outbox handler enqueues work after commit.
Apalis supplies transport and workers.
The gear supplies workflow execution semantics.

### Consequences

One transaction commits journal changes, normalized records, events, progress, delivery intents, and ToolKit Outbox commands.
For fan-out, it also stores exact item inputs and stable indexes.
It records publication intents for all expanded branches.
For compensation, it stores the phase and the next eligible compensation instance.
Repeated transaction attempts make no external provider calls.
Wake signals run only after successful commit.
Periodic reconciliation recovers lost signals.

The Outbox handler checks current generation, instance identity, and eligibility before enqueue.
It acknowledges the command only after enqueue and intent acknowledgment succeed.
Delivery contains run ID, encoded activity ID, structured instance identity, and delivery generation.
It contains no workflow input, checkpoint payload, or credential.
The instance identity includes template ID, optional item index, and phase.
The runtime rejects inconsistent identities.

Workers acquire claims and store checkpoints through journal CAS and fences.
Each instance has an separate renewable lease.
The journal controls business retries and recovery.
Apalis uses one transport attempt.
Reconciliation recovers expired claims and overdue deliveries without claims.
Fresh AuthN/PDP decisions authorize service operations.
Policy failure pauses processing without a business failure.

Direct SQLx access stays restricted to the adapter's `apalis` schema.
All named queues share its pool.
The gear owns fixed `durable_*` tables and the `durable_delivery_outbox` prefix.
Use a clean development or test database for the current format.
There is no legacy storage conversion or automatic database deletion.

### Confirmation

Run `make test-durable-execution-pg` against a disposable PostgreSQL database.
See the [Makefile](../../../../Makefile) and [CI workflow](../../../../.github/workflows/ci.yml).
Use these tests as evidence:

- [Outbox tests](../../tests/integration/outbox_tests.rs) check commit, enqueue, acknowledgment, and duplicate delivery.
- [Stage runtime tests](../../tests/unit/staged_runtime_tests.rs) check expansion, compensation Retry, and large event batches.
- [Typed runtime tests](../../tests/integration/typed_runtime_tests.rs) check concurrent fan-out and ordered join through real queues.
- [Provider crash tests](../../tests/integration/provider_stages_tests.rs) check deduplication across forward and compensation worker crashes.
- [Storage query tests](../../tests/unit/audit_storage_tests.rs) check projection query counts and payload isolation.

An unavailable PostgreSQL runtime or Dylint toolchain is incomplete validation.

## Pros and Cons of the Options

### Apalis with transactional ToolKit Outbox

The existing scoped transaction supplies atomic publication.
Persisted commands survive queue failure.
The design adds a delivery step, a pool, and an adapter schema.
Queue delivery and external effects can repeat.

### Separate journal and adapter transactions

This option removes the Outbox delivery step.
A crash after journal commit can lose queue publication.
Enqueue before commit can deliver work for a rolled-back run.
The same database does not combine separate transactions.

### Shared SQLx transaction integration

A shared transaction could store journal changes and queue jobs together.
Integration with scope enforcement and ToolKit transaction ownership is necessary for this option.
That change is outside this decision.

## More Information

### Failure Windows

| Failure point | Recovery |
|---|---|
| Before journal commit | New state and publication records roll back together. |
| After commit, before enqueue | The persisted Outbox command stays pending. |
| After enqueue, before acknowledgment | Delivery can repeat. Generation, CAS, and fences protect state changes. |
| After acknowledgment, before claim | Reconciliation republishes overdue work. |
| After effect, before checkpoint | The provider deduplicates or reconciles repeated calls. |
| After checkpoint | Recovery keeps the successful instance. |

These rules apply to forward and compensation instances.
Fencing protects journal changes.
It does not create an external distributed transaction.

### Historical Versions

The initial assessment used ToolKit DB `0.16.2` and Apalis/core `1.0.0-rc.10`.
It used Apalis PostgreSQL `1.0.0-rc.9` and SQLx `0.9.0`.
The workspace base was `fa9334a34`.
These are historical assessment versions.
Before an upgrade, do a check of the current manifests.
See the [adapter API](https://docs.rs/apalis-postgres/1.0.0-rc.9/apalis_postgres/).

### Conditions for Reconsideration

Reconsider this decision if ToolKit DB can safely share an adapter transaction.
A replacement must keep authorization, atomic publication, instance ownership, and crash recovery.

## Traceability

- [PRD](../PRD.md): admission, fan-out, compensation, projection, and delivery requirements.
- [DESIGN](../DESIGN.md): transaction and runtime boundaries.
- [ADR 0002](0002-task-execution-framework-selection.md): transport choice.
- [ADR 0004](0004-saga-fanout-and-progress.md): current instance and storage model.
- [Repository](../../src/store/repository.rs) and [starts](../../src/store/start.rs): publication transactions.
- [Outbox handler](../../src/delivery/outbox.rs) and [adapter](../../src/delivery/apalis.rs): queue delivery.
