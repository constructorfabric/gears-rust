---
status: accepted
date: 2026-10-07
---

# Retain Apalis as the task execution transport

**ID**: `cpt-cf-durable-execution-adr-task-execution-framework-selection`

## Table of Contents

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Apalis with transactional Outbox](#apalis-with-transactional-outbox)
  - [Extend toolkit Outbox with long-task execution](#extend-toolkit-outbox-with-long-task-execution)
  - [Build a managed executor over toolkit Outbox](#build-a-managed-executor-over-toolkit-outbox)
  - [Underway](#underway)
  - [Fang](#fang)
  - [Rusty Celery](#rusty-celery)
- [More Information](#more-information)
  - [Evidence and versions](#evidence-and-versions)
  - [Comparison against common criteria](#comparison-against-common-criteria)
  - [Conditions for reconsideration](#conditions-for-reconsideration)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

Durable execution is moving into a shared embedded system gear. The extraction
must preserve its existing journal and integrated worker runtime. We can retain
Apalis, extend toolkit Outbox, build an executor over Outbox or adopt another
serialized-task framework. The choice depends on long-task execution, recovery,
toolkit DB/AuthZ integration, migration effort and maintenance cost.

## Decision Drivers

- PostgreSQL-backed execution without another broker.
- Serialized work identifiers and delayed delivery/scheduling.
- Long-running handlers, independent concurrency pools, worker liveness and recovery.
- Bounded cooperative shutdown and cancellation of external work.
- Preservation of checkpoints, parallel groups, retries, execution epochs and fencing.
- toolkit-db scope enforcement, AuthN/PDP authorization and gear-owned migrations.
- Workspace TLS compatibility, API stability and maintenance cost.
- An extraction that preserves verified journal semantics without also migrating
  the transport and workflow model.

## Considered Options

- Apalis with transactional Outbox.
- Extend toolkit Outbox with long-task execution.
- Build a managed executor over toolkit Outbox.
- Underway.
- Fang.
- Rusty Celery.

## Decision Outcome

Chosen option: **Apalis with transactional Outbox**. Apalis already integrates
with the journal and provides a PostgreSQL worker runtime with heartbeat and
orphaned-task recovery. Keeping it avoids rewriting transport and workflow
semantics during extraction. [ADR 0001](0001-apalis-with-transactional-outbox.md)
defines the transaction boundary.

### Consequences

- Apalis owns transport and worker coordination. The journal remains
  authoritative for checkpoints, business scheduling/retries, parallel groups,
  cancellation, epochs and fencing.
- The gear must operate and migrate the journal and Outbox, and initialize the
  official adapter's `apalis` schema. Named queues share a separate adapter pool
  but have independent concurrency.
- Rust handlers register in process; transport carries serialized delivery IDs.
  Version 0.1.0 provides no remote execution or registration bindings.
- Pin adapter versions. Check upgrades against workspace SQLx/TLS policy,
  journal integration and recovery tests.
- A replacement must map existing journal semantics and preserve toolkit scope
  enforcement and service authorization. SQLx transaction support alone does
  not establish toolkit-db integration.

### Confirmation

Review the runtime and adapter boundaries, then verify:

- PostgreSQL delayed delivery, independent named-queue concurrency, pool
  replacement and failures before enqueue and after enqueue but before acknowledgement.
- Worker loss, checkpoint preservation and effect replay through subprocess tests.
- Duplicate delivery, stale fencing, Retry/Resume, parallel groups, coalescing
  and repeatable migrations through journal, storage and SDK tests.
- Allowed tenants, foreign-data rejection, unavailable endpoints and access
  revocation during execution through AuthN/PDP tests.
- Targeted Clippy, format and release checks, and the example-server opt-in feature build.
  Architecture lints require `cargo-gears`, which was initially unavailable
  in the extraction environment.

See [validation commands](../testing.md). An alternative must pass
the same operational tests and a workload-specific integration evaluation.
This decision establishes no benchmark superiority for Apalis.

## Pros and Cons of the Options

### Apalis with transactional Outbox

- Good, because the official PostgreSQL adapter provides worker heartbeat and
  orphaned-task recovery. The journal already integrates claims, attempts,
  cancellation and recovery with that runtime.
- Good, because it meets the PostgreSQL-only transport requirement and preserves
  workflow semantics during extraction.
- Bad, because it adds a second pool, service schema, delivery hop, dependencies
  and RC API upgrade risk while retaining both journal and Outbox.
- Neutral, because no production maturity advantage over alternatives has been
  established.

### Extend toolkit Outbox with long-task execution

- Good, because it retains native toolkit DB integration, serialized payloads
  and delivery machinery without another framework's service schema.
- Bad, because the current leased handler has a hard lease budget and processes
  each partition sequentially, constraining arbitrary long-running activities.
- Bad, because long activities need lease renewal, recovery/fencing and
  cancellation, or a separate managed executor. Either expands the shared
  framework's API and maintenance scope.

### Build a managed executor over toolkit Outbox

- Good, because it can retain toolkit transaction ownership and implement
  execution directly against the current journal.
- Bad, because we must own concurrency/backpressure, renewable ownership,
  heartbeat/loss detection, recovery, cancellation, shutdown and tests.
- Bad, because spawning a task and immediately acknowledging Outbox loses durable
  ownership. Task lifetime must be managed independently.

### Underway

- Good, because published 0.2.0 provides typed tasks and multi-step jobs over
  PostgreSQL, automatic retries, scheduling and SQLx transactional enqueue/task
  execution. It is a PostgreSQL alternative.
- Good, because transaction support could suit a redesigned DB boundary.
- Bad, because SQLx transactions do not automatically bridge scoped toolkit-db
  transactions. Replacing the journal with Underway's job model requires an
  explicit semantics migration.
- Neutral, because long-task lease/timeout, recovery and shutdown behavior still
  need workload-specific validation. The assessment covers the published API;
  newer APIs may differ.

### Fang

- Good, because it provides serializable tasks, SQL backends including PostgreSQL,
  worker pools, scheduling/cron and retries.
- Bad, because adopting its task registration, runtime and backend integration
  replaces an integrated transport without demonstrated operational benefit.
- Neutral, because long-task recovery, shutdown and selected backend/TLS behavior
  still need integration validation.

### Rusty Celery

- Good, because it provides serialized Celery tasks, worker execution and
  documented task retries/scheduling with AMQP/Redis brokers.
- Bad, because brokers add deployment, monitoring, credentials and TLS integration
  beyond the PostgreSQL-only requirement.
- Neutral, because worker-loss, acknowledgement and shutdown behavior need
  validation for the selected broker and runtime.

## More Information

### Evidence and versions

The assessment uses implementation source and primary documentation, not an
equivalent-load benchmark. Unverified behavior is not treated as missing
functionality. Evaluated snapshots:

- Apalis/apalis-core **1.0.0-rc.10**, apalis-postgres **1.0.0-rc.9**: pinned
  workspace dependencies and registry source;
  [PostgreSQL documentation](https://docs.rs/apalis-postgres/latest/apalis_postgres/).
- toolkit-db **0.16.2**, workspace base commit **fa9334a34**: local Outbox code,
  particularly the leased strategy, processor, core and Wake contract.
- Underway **0.2.0**: [published documentation](https://docs.rs/underway/0.2.0/underway/).
  This covers the published API, not potentially different GitHub main APIs.
- Fang README API example **0.11.0**, repository HEAD
  **c7cc3679e845d3a6b03f33836cb6bc097caa5b56**:
  [pinned source](https://github.com/ayrat555/fang/tree/c7cc3679e845d3a6b03f33836cb6bc097caa5b56).
- Rusty Celery repository HEAD **1a0168f4b4b521c891fcfc853c21fc30de12c2f2**:
  [pinned source](https://github.com/rusty-celery/rusty-celery/tree/1a0168f4b4b521c891fcfc853c21fc30de12c2f2).
  No Rusty Celery release is selected for integration.

### Comparison against common criteria

| Criterion | Apalis + Outbox | Extend toolkit Outbox | Own executor over Outbox | Underway 0.2.0 | Fang snapshot | Rusty Celery snapshot |
| --- | --- | --- | --- | --- | --- | --- |
| PostgreSQL without another broker | Yes, official adapter | Yes | Yes | Yes | PostgreSQL supported alongside other SQL backends | Documented brokers are AMQP/Redis; adds infrastructure |
| Serialized payloads | Delivery IDs via serde | Existing byte payloads/type tags | Existing payloads plus local handler registry | Typed serializable task/job inputs | Serializable task types | Serialized Celery task messages |
| Scheduling/retries | Adapter delayed enqueue; business schedule/retries in journal | Delivery retries exist; workflow policy still separate | Build workflow scheduling/reconciliation | Documented scheduling and automatic retries | Documented scheduling, cron and retry policies | Documented task retries/scheduling; broker semantics need integration tests |
| Long tasks | Journal renewable lease; adapter worker heartbeat | Current leased handler has a hard lease budget | Build/own renewable task execution outside short handler | Full worker/task/job model; lease/timeout behavior needs workload validation | Task framework; long-task timeout/recovery behavior needs validation | Long-task recovery/ack behavior needs validation for selected broker |
| Concurrency | Named queues, independent worker concurrency | Sequential processing inside each partition | Build managed concurrency and backpressure | Worker/queue abstractions; validate limits | Worker pools documented | Worker runtime; validate configured limits |
| Liveness/recovery | PG adapter heartbeat and orphaned-task recovery plus journal reconciliation | Partition/message leases; not a complete long-task runtime | Build heartbeats, loss detection, recovery and fencing | Dedicated workers and transactional tasks; test recovery contract | Dedicated workers; test loss/recovery contract | Broker/runtime-specific; test worker-loss contract |
| Shutdown | Gear cancellation; cooperative join within lifecycle budget | Handler deadline constrains shutdown | Own cancellation/join/escalation | Worker lifecycle exists; validate cooperative external-work cleanup | Validate lifecycle/external-work cleanup | Validate broker/runtime shutdown/external-work cleanup |
| toolkit-db/AuthZ | Already-integrated scoped journal; reauthorize service operations | Native DB integration; worker service authorization still needed | Native DB integration; author/maintain all authorization boundaries | SQLx transactional enqueue/task context is a real alternative; adapt scoped toolkit integration | Adapt SQL backend access and toolkit policy enforcement | Adapt policy enforcement plus broker credentials/operations |
| Migrations | Gear journal/Outbox migrations; official adapter schema setup | Existing migrations; additions for renewed leases/runtime | Gear owns additional executor schema/state | Own PostgreSQL schema and migrations | Framework SQL migrations | Broker infrastructure; journal migrations remain ours |
| TLS | Workspace SQLx Rustls/AWS-LC settings | Existing toolkit DB policy | Existing toolkit DB policy | Audit selected SQLx/TLS features against workspace | Audit selected backend/TLS features | Audit broker TLS, credential handling and features |
| API stability/maintenance | Pinned RC APIs; existing integration limits migration effort | We maintain expanded shared framework API | We own the complete executor | Additional task/job semantics and DB adaptation; published 0.2 API | Different task registration/runtime; adaptation required | Additional broker operations and Celery protocol compatibility |

### Conditions for reconsideration

Revisit if Outbox supports long-task execution with renewable ownership and
shutdown, Apalis compatibility or maintenance becomes problematic, or a pinned
alternative demonstrates a material advantage in tests. Before replacing
production execution, it must pass commit/enqueue/ack failure, duplicate
delivery, worker termination, stale fencing, Retry/Resume/checkpoint, queue
isolation and AuthN/PDP outage/revocation tests.

## Traceability

- **PRD**: [Durable execution](../PRD.md).
- **DESIGN**: [Durable execution](../DESIGN.md).
- **Related decision**:
  [Apalis with a transactional Outbox](0001-apalis-with-transactional-outbox.md)
  (`cpt-cf-durable-execution-adr-apalis-transactional-outbox`).

The decision constrains:

- [Runtime](../../durable-execution/src/infra/runtime.rs) and
  [Apalis adapter](../../durable-execution/src/infra/apalis.rs): worker lifecycle,
  scheduling and delivery transport.
- [Queue configuration](../../durable-execution/src/config.rs): independent
  concurrency and routing by exact versioned definition name.
- [SDK](../../durable-execution-sdk/src/api.rs) and
  [registry](../../durable-execution/src/domain/registry.rs): local contracts and
  handler registration.
- [Journal domain](../../durable-execution/src/domain/journal.rs),
  [migrations](../../durable-execution/src/infra/storage/migrations/mod.rs) and
  [authorization tests](../../durable-execution/src/infra/authorization_tests.rs):
  semantics and integration that a replacement must preserve.
