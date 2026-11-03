---
status: accepted
date: 2026-10-07
---

# Retain Apalis as the task execution transport

**ID**: `cpt-cf-durable-execution-adr-task-execution-framework-selection`

> **Superseded in part by [ADR 0005](0005-embeddable-library.md)**: durable execution is now a library that a consumer gear embeds, not a shared embedded gear.
> Read "the gear" as the engine, and "gear-owned migrations" as `migrations()` that the embedding gear applies.
> Queue settings come from `ExecutionMode` and `QueueConfig`, not gear configuration keys.
> The transport decision stays accepted.


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Apalis with transactional Outbox](#apalis-with-transactional-outbox)
  - [Extend ToolKit Outbox](#extend-toolkit-outbox)
  - [Managed executor over Outbox](#managed-executor-over-outbox)
  - [Underway](#underway)
  - [Fang](#fang)
  - [Rusty Celery](#rusty-celery)
- [More Information](#more-information)
  - [Historical Evidence](#historical-evidence)
  - [Conditions for Reconsideration](#conditions-for-reconsideration)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

The original extraction moved durable execution into a shared embedded gear.
The transport decision had to keep journal integration and worker recovery.
The alternatives included Apalis, ToolKit Outbox extensions, a custom executor, and other task frameworks.
This ADR records that historical selection.
[ADR 0004](0004-saga-fanout-and-progress.md) extends the gear's workflow runtime without a change to the transport choice.

## Decision Drivers

- PostgreSQL transport without another broker.
- Delayed delivery of serialized work identifiers.
- Long-running handlers and separate worker pools.
- Worker-loss recovery and bounded shutdown.
- Scoped ToolKit DB access and current authorization.
- Gear-owned migrations and workspace TLS compatibility.
- Integration effort and maintenance cost.

## Considered Options

1. Apalis with transactional Outbox.
2. Extend ToolKit Outbox for long-task execution.
3. Build a managed executor over Outbox.
4. Underway.
5. Fang.
6. Rusty Celery.

## Decision Outcome

Retain Apalis with transactional ToolKit Outbox.
Apalis already supplies the integrated PostgreSQL transport and worker coordination.
Outbox keeps publication atomicity across the separate pools.
[ADR 0001](0001-apalis-with-transactional-outbox.md) defines that boundary.

### Consequences

Apalis owns queue transport, worker heartbeat, and orphaned-task recovery.
The gear owns workflow stages, data sources, expansion, compensation, retries, epochs, claims, and checkpoints.
Apalis does not decide saga order or fan-out joins.
Its worker heartbeat does not replace journal leases or fences.

The gear migrates its journal and Outbox tables.
It initializes the official adapter schema.
Named queues share an adapter pool and have separate host worker limits.
Persisted claims enforce the fan-out stage limit across hosts.
Handlers and compensators stay local Rust bindings.
Queue messages contain identifiers without payloads or credentials.

Pin adapter versions.
Do recovery tests and a workspace SQLx/TLS configuration check for each upgrade.
A replacement must keep scope enforcement and current workflow behavior.
SQLx transaction support alone does not establish ToolKit DB integration.

### Confirmation

Use the existing [Makefile](../../../../Makefile) targets run by the [CI workflow](../../../../.github/workflows/ci.yml).
[Queue tests](../../tests/integration/queue_runtime_tests.rs) cover routes and concurrency.
[Outbox tests](../../tests/integration/outbox_tests.rs) cover publication failures.
[Provider crash tests](../../tests/integration/provider_stages_tests.rs) cover forward and compensation recovery.
Test queue delays, routing, separate pools, and publication failure windows.
Test worker crashes, duplicate delivery, stale claims, and kept checkpoints.
Test fan-out concurrency, expansion recovery, compensation order, and compensation Retry.
Test service authorization, tenant isolation, and host shutdown.
Run package-scoped fmt, Clippy, and architectural Dylint checks.

This selection makes no performance superiority claim.
For an alternative, do the same operational tests and a representative workload comparison.

## Pros and Cons of the Options

### Apalis with transactional Outbox

The official adapter supplies PostgreSQL workers, heartbeat, and orphan recovery.
The existing journal integration reduces transport migration work.
A separate pool, adapter schema, and Outbox delivery step are necessary for this design.
Do upgrade checks for the pinned release-candidate APIs.

### Extend ToolKit Outbox

This option keeps native transaction and delivery integration.
The assessed leased strategy has a fixed handler lease budget.
It processes each partition sequentially.
Renewable ownership and more lifecycle support are necessary for long activities.

### Managed executor over Outbox

This option keeps ToolKit transaction ownership.
With this option, the project owns concurrency, backpressure, heartbeat, recovery, cancellation, and shutdown.
Immediate acknowledgment after task spawn loses durable execution ownership.
A separate managed task lifecycle is necessary.

### Underway

The assessed 0.2.0 API supplies PostgreSQL tasks, multi-step jobs, scheduling, retries, and SQLx transactions.
Its transaction API could support a redesigned storage boundary.
Integration with ToolKit scopes and existing journal semantics is still necessary.
Do workload tests of long-task recovery and shutdown before adoption.

### Fang

The assessed API supplies serialized tasks, SQL backends, scheduling, retries, and worker pools.
Adoption replaces task registration and backend integration.
Do integration tests of long-task recovery, TLS, and external-work cleanup before adoption.

### Rusty Celery

The assessed API supplies Celery tasks, worker execution, and task retries.
Its documented AMQP/Redis brokers add deployment and credential requirements.
Do worker-loss and shutdown tests for the selected broker before adoption.

## More Information

### Historical Evidence

The original comparison used source inspection and primary documentation.
It did not use an equivalent-load benchmark.
These snapshots define the historical assessment:

- Apalis/core `1.0.0-rc.10` and PostgreSQL adapter `1.0.0-rc.9`: [adapter documentation](https://docs.rs/apalis-postgres/1.0.0-rc.9/apalis_postgres/).
- ToolKit DB `0.16.2`: local Outbox source at workspace base `fa9334a34`.
- Underway `0.2.0`: [published documentation](https://docs.rs/underway/0.2.0/underway/).
- Fang: [source snapshot](https://github.com/ayrat555/fang/tree/c7cc3679e845d3a6b03f33836cb6bc097caa5b56), with README API example `0.11.0`.
- Rusty Celery: [source snapshot](https://github.com/rusty-celery/rusty-celery/tree/1a0168f4b4b521c891fcfc853c21fc30de12c2f2).

Unverified behavior is not evidence of missing functionality.
Current framework versions can differ from these snapshots.

### Conditions for Reconsideration

Reconsider if Outbox supplies suitable long-task execution or Apalis maintenance becomes unacceptable.
Reconsider if a pinned alternative shows a measured operational benefit.
A replacement must pass publication, crash, fencing, queue, authorization, fan-out, and compensation tests.

## Traceability

- [PRD](../PRD.md): runtime and recovery requirements.
- [DESIGN](../DESIGN.md): transport and workflow ownership.
- [ADR 0001](0001-apalis-with-transactional-outbox.md): atomic publication.
- [ADR 0004](0004-saga-fanout-and-progress.md): workflow runtime extension.
- [Runtime](../../src/engine/runtime.rs) and [adapter](../../src/delivery/apalis.rs): transport integration.
- [Queue configuration](../../src/config.rs): routes and worker limits.
- [Execution state](../../src/state/execution.rs): gear-owned workflow semantics.
- [Makefile](../../../../Makefile) and [CI workflow](../../../../.github/workflows/ci.yml): validation commands.
