---
description: "Requirements for the embeddable durable-execution library: persisted typed workflows with checkpoints, retries, runtime fan-out, saga compensation, control commands and payload-free progress."
---

# PRD: Durable execution

This document defines requirements for persisted background work run by an engine that a consumer gear embeds.
Use the [technical terms](DESIGN.md#23-terminology) with the meanings given there.
Requirement labels **MUST** and **MUST NOT** specify mandatory behavior.
Unchecked boxes stay open acceptance records until a test result confirms them.


<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [Persisted admission](#persisted-admission)
  - [Activity progress](#activity-progress)
  - [Retry and continuation](#retry-and-continuation)
  - [Cancellation](#cancellation)
  - [Definition registration](#definition-registration)
  - [Queue routing](#queue-routing)
  - [Coalescing](#coalescing)
  - [Inspection](#inspection)
  - [Typed result identity](#typed-result-identity)
  - [Runtime fan-out](#runtime-fan-out)
  - [Saga compensation](#saga-compensation)
  - [Stored progress projection](#stored-progress-projection)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [Ownership and recovery](#ownership-and-recovery)
  - [Delivery semantics](#delivery-semantics)
  - [Authorization](#authorization)
  - [Lifecycle](#lifecycle)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
  - [Submit background work](#submit-background-work)
  - [Recover after worker loss](#recover-after-worker-loss)
  - [Retire or replace a definition](#retire-or-replace-a-definition)
  - [Compensate an order](#compensate-an-order)
  - [Process a variable list](#process-a-variable-list)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Traceability](#13-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

Durable execution is an embeddable library for local Rust activities and multi-stage workflows.
The package is `cf-gears-toolkit-durable-execution` and the crate is `toolkit_durable_execution`.
A consumer gear embeds one `Engine` in its own database and lifecycle.
The engine owns the journal schema, retries, execution control, and recovery.
The consumer gear defines contracts, registers local handlers, and supplies authorization.
The stability tier is experimental.

### 1.2 Background / Problem Statement

Background work must survive worker loss and delivery failures.
Consumer gears must have confirmed checkpoints, ordered parallel results, and recoverable compensation.
They must also have authorized progress without access to stored payloads.
The library supplies these capabilities to each embedding gear.
Applications must restore local Rust handlers after restart.

### 1.3 Goals (Business Outcomes)

- Supply one runner for persisted background work.
- Preserve accepted work and confirmed checkpoints across restart.
- Support variable data-driven fan-out and recoverable saga compensation.
- Apply tenant, owner, and resource authorization to each operation.
- Keep ordinary authoring and result reads typed.

### 1.4 Glossary

The [technical terms](DESIGN.md#23-terminology) define activity template, activity instance, run, checkpoint, claim, epoch, and compensation.
The glossary also defines the document-specific technical verbs.

## 2. Actors

### 2.1 Human Actors

#### Gear developer

**ID**: `cpt-cf-durable-execution-actor-developer`

The developer defines contracts, handlers, compensators, and provider idempotency.
The developer embeds the engine, implements the `Authorizer`, and uses the library API to submit and read work.

#### Operator

**ID**: `cpt-cf-durable-execution-actor-operator`

The operator configures databases, queue DSNs, the service identity, queues, and lifecycle limits through the embedding gear.
The operator restores workers and controls authorized runs.

### 2.2 System Actors

#### Consumer gear

**ID**: `cpt-cf-durable-execution-actor-consumer`

The consumer embeds the engine and applies its migrations.
It registers definitions and submits, reads, or controls runs through the engine handles.

#### Host process

**ID**: `cpt-cf-durable-execution-actor-host`

The host runs the consumer gear: migrations, initialization, start, and stop.
The consumer gear builds the engine, binds local handlers, and serves the `Runner` in its lifecycle.

#### Authorization services

**ID**: `cpt-cf-durable-execution-actor-policy`

The consumer gear implements the `Authorizer` port.
It gets scopes for caller and service operations, usually from `PolicyEnforcer`.
It supplies its own cached service identity for service operations.

## 3. Operational Concept & Environment

PostgreSQL is necessary for the full runtime.
SQLite supports unit and storage tests.
Processes in the deployment share contracts, catalog, journal, and queue names.
Each execution host restores its own handlers and compensators.

Each process selects an `ExecutionMode` through `EngineBuilder::mode`.
`ControlPlane` is the default.
It observes runs, cancels runs, and reconciles definitions, but cannot publish runnable work.
Scheduling operations such as start, Retry, and Resume return `Unavailable` in this mode.
`Dispatcher(QueueConfig)` also admits runs and publishes due work.
`Worker(QueueConfig)` also executes activities.
`QueueConfig` supplies the queue DSN, queues, concurrency, and routes.
The library does not read environment variables or gear configuration.
An `Authorizer` with a working service identity is necessary for processing.
The engine tables use a fixed prefix, so one database or schema hosts one engine.
See [README execution modes](../README.md#execution-modes) and the [ToolKit guide](../../../docs/toolkit_unified_system/README.md).

## 4. Scope

### 4.1 In Scope

Persisted admission, local activities, sequential stages, static parallel stages, runtime fan-out, saga compensation, retries, pause, Abort, and recovery are included.
The library also supports epochs, coalescing, named queues, dynamic registration, progress, and authorized inspection.

### 4.2 Out of Scope

Remote handlers, function serialization, arbitrary DAGs, nested workflows, and user-facing REST or gRPC bindings are excluded.
Distributed transactions across external providers are excluded.
A separate design is necessary for existing production data migration.
There is no migration from the tables of the former durable-execution gear.
ClientHub registration, gear configuration parsing, environment variable reads, and credential exchange are excluded.
Several engines in one database or schema are excluded until a caller-supplied table prefix exists.

## 5. Functional Requirements

### Persisted admission

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-fr-admission`

The engine **MUST** commit an accepted run and publication intent in one transaction.
Admission **MUST** check the active registration generation.
An idempotent replay **MUST** return the existing run.
Conflicting input under the same request key **MUST** fail.
Closed admission **MUST NOT** create a new run or successor.

### Activity progress

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-fr-progress`

The engine **MUST** execute declared stages and store successful outputs as checkpoints.
Recovery **MUST** keep confirmed checkpoints.
Duplicate delivery **MUST NOT** repeat a completed activity instance.
Static parallel joins **MUST** keep declaration order.
Without saga compensation, failed branches **MUST NOT** stop eligible siblings prematurely.

### Retry and continuation

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-fr-continuation`

The journal **MUST** apply activity timeout and retry policies.
Retry and Resume **MUST** check the expected epoch and keep completed checkpoints.
Forward continuation **MUST** reject revoked registration generations.
Compensation Retry **MUST** accept cleanup in the same generation while the definition is `Stopping`.
Missing handlers **MUST** keep due time, attempts, and checkpoints.
Late binding **MUST** restore delivery without manual Resume.
Contract mismatch **MUST** block execution.
`Compensated` **MUST** reject forward Retry and Resume.

### Cancellation

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-fr-cancellation`

Cancel **MUST** pause forward execution and keep confirmed checkpoints.
Resume **MUST** continue from that progress.
Cancel and Abort **MUST** check the expected epoch and current authorization.
Abort **MUST** stop forward execution permanently for the run.
Saga Abort **MUST** start compensation.
Abort **MUST NOT** change a successful or completed run.
Cancel during compensation **MUST** fail.

The runtime **MUST** signal active handlers.
Unconfirmed termination **MUST** keep claims until safe release or lease expiry.
An aborted Rust future does not confirm that external work stopped.

### Definition registration

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-fr-registration`

Registration **MUST** work before and after host startup.
Processes **MUST** share matching contracts.
Incompatible contracts under the same name **MUST** fail registration.
Duplicate local binding **MUST** keep the original handlers.
Registration alone **MUST NOT** reactivate a definition.

`Retain` **MUST** close admission and keep existing execution.
`CancelAndRelease` **MUST** record `Stopping` and revoke forward execution for the generation.
It **MUST** request Abort for sagas, including paused and failed runs.
It **MUST** keep saga bindings until compensation and claim cleanup finish.
`CompensationFailed` **MUST** keep the definition in `Stopping`.
Restart **MUST** accept restored saga bindings with the same contract for this cleanup.
Other revoked runs **MUST** complete cancellation and claim cleanup before release.

Reconciliation **MUST** run on hosts without activity workers too.
Repeated release requests **MUST** keep an already `Released` state.
Hosts **MUST** free local bindings after authorized observation of release.
Control commands **MUST** check the expected catalog revision.
Activation during `Stopping` **MUST** fail.
After release, a separate activation command **MUST** open the prepared new generation.

### Queue routing

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-fr-queues`

The engine **MUST** route by exact versioned definition name from `QueueConfig::route`.
Unmatched names **MUST** use the default queue.
Each host's queue pool **MUST** enforce its configured concurrency independently.

### Coalescing

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-fr-coalescing`

A scoped slot **MUST** contain at most one active run and one pending successor.
Coalescing **MUST** stay within one registration generation.
Incompatible input for a reused key **MUST** fail.

### Inspection

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-fr-inspection`

Progress **MUST** omit inputs, result payloads, and credentials.
Results and history **MUST** use separate reads.
Inspection and listing **MUST** have separate authorization.
Inspection permission **MUST NOT** grant result access.
Status filters **MUST** use typed statuses.
For non-saga parallel work, `Failing` **MUST** distinguish an unfinished failed stage from final `Failed`.
Recorded times **MUST NOT** be inferred from update times.
Retained checkpoints **MUST** keep their origin epoch.

### Typed result identity

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-fr-typed-results`

Typed start **MUST** return `StartedRun<I, O>` with a `RunHandle<I, O>`.
The handle **MUST** keep the expected contract identity.
`WorkflowRef::attach` **MUST** restore a handle from a saved run ID.
Typed result reads **MUST** use the handle.
The internal untyped read path **MUST** use a supplied expected fingerprint and result selector.

Result reads **MUST** check fingerprint and checkpoint membership before payload reads.
Contract mismatch **MUST** return `FailedPrecondition`, even for compatible JSON.
Typed decode failure **MUST** return `Internal`.
Pre-build references **MUST** receive final identity only after successful build.
Checkpoint types **MUST** distinguish sequential, static parallel, and fan-out results.

### Runtime fan-out

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-fr-fanout`

`.for_each` **MUST** consume `Vec<Item>` and return `Vec<Output>`.
Expansion **MUST** commit item inputs, stable indexes, and publication intents together.
Recovery **MUST NOT** recreate committed branches.
An empty list **MUST** complete without handler calls.
Join **MUST** keep input order.
Each instance **MUST** have separate attempts, lease, fence, and checkpoint.
Retry **MUST** repeat only unfinished branches.
Branch reads **MUST** check existence and readiness.

Default limits are 1024 items per stage and stage concurrency 16.
A run **MUST** reject expansion above 4096 forward activity instances.
Expansion **MUST** check limits before branch execution.
Limit failure **MUST** produce a recorded stage error.
Stage concurrency **MUST** count persisted running claims across processes through CAS.
Stage and queue limits **MUST** both apply.

### Saga compensation

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-fr-compensation`

`.saga()` **MUST** enable compensation after final forward failure or Abort.
The runtime **MUST** stop new forward claims and signal active handlers.
Compensation **MUST** wait for confirmed cleanup or lease expiry.
Only confirmed successful instances with declared compensators **MUST** participate.
Compensation **MUST** run sequentially in reverse stage and reverse branch-index order.

The journal **MUST** keep original invocation inputs and successful outputs.
Compensation keys **MUST** be stable across retries and restart.
They **MUST** differ from forward keys.
Compensation **MUST** use a separate timeout and retry budget.
The original cause **MUST** stay separate from the compensation error.

Exhausted compensation retries **MUST** produce `CompensationFailed`.
Manual Retry **MUST** continue the failed and remaining compensators.
Successful compensation **MUST NOT** repeat.
Complete compensation **MUST** produce `Compensated`.
Shutdown **MUST** keep the phase for recovery.

### Stored progress projection

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-fr-projection`

The run row **MUST** store a private, versioned progress projection without payloads.
One transaction **MUST** update journal, projection, events, and publication intents.
Get and inspect **MUST** each use one projection query.
List **MUST** use one count query and one paginated projection query.
These queries **MUST NOT** select journal, input, result, or history payloads.
Query counts **MUST NOT** change with page size.
Correct projection reads **MUST** survive corruption in separate payload or history records.
The cursor **MUST** identify the last committed per-run event.
Event sequence numbers **MUST** increase without fixed revision blocks.

## 6. Non-Functional Requirements

### Ownership and recovery

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-nfr-fencing`

The runtime **MUST** renew claims at the configured heartbeat interval.
It **MUST** reject checkpoints from stale claims, epochs, or unauthorized registration generations.
Lease expiry **MUST** make recovery possible after worker loss.
Lease renewal **MUST NOT** change the activity timeout.

### Delivery semantics

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-nfr-delivery`

The runtime **MUST** recover committed intents after queue or acknowledgment failures.
It **MUST** tolerate duplicate delivery.
External effects can occur more than once.
Providers **MUST** support idempotency or reconciliation when repeated effects are unsafe.
Compensation covers only declared compensators and confirmed results.
It does not supply a distributed transaction.

### Authorization

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-nfr-authorization`

Caller operations **MUST** apply current PDP tenant, owner, and resource constraints.
Owned Abort **MUST** enforce `update` authorization.
Operator Abort **MUST** enforce `manage` authorization.
`CancelAndRelease` **MUST** enforce an unconstrained `cancel_definition` grant.
Service operations **MUST** get a current `Authorizer` service scope before each operation.
Caller operations **MUST** reject a nil subject or tenant before the `Authorizer` call.
Each `Authorizer` call **MUST** end within the authorization timeout.
A timeout **MUST** produce `Unavailable`.
Authorization logs **MUST** contain only the failure kind, action, and resource.
Compensation **MUST** keep the original execution owner.
Compensators **MUST** authorize access to consumer resources.
Cleanup permission for a draining generation **MUST NOT** authorize new forward effects.

Policy failure or denial **MUST NOT** become a workflow business failure.
Authorization loss **MUST** stop handlers.
Unconfirmed claims **MUST** wait for lease expiry.
Pending release **MUST** keep state and bindings until authorization recovers.

### Lifecycle

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-nfr-lifecycle`

Workers and reconciliation **MUST** follow `Runner::run` cancellation.
Shutdown **MUST** use one budget for handler cleanup, claim persistence, and lifecycle teardown.
The host deadline can shorten this budget.
Shutdown **MUST NOT** request business Cancel or discard checkpoints.
Readiness **MUST** be true only while `Runner::run` serves.
It **MUST** clear when the runner stops or fails.
Retryable control-plane failures **MUST** use bounded backoff and a consecutive failure limit.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### Execution API

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-interface-execution`

`Engine::builder(db, authorizer)` configures an engine; `build` validates settings without I/O.
`Engine::prepare` returns a `Runner`, and `Runner::run(cancel)` serves until cancellation.
The engine supplies concrete handles; there is no ClientHub registration.
`Engine::client` returns `ExecutionClient`.
Its operations are typed start, progress, wait, watch, events, results, history, Cancel, Abort, Retry, and Resume.
For control commands, supply the expected epoch.
`Engine::inspector` returns `ExecutionInspector` for administrative progress and listing.
Operations return the library `Error` with a non-exhaustive `ErrorKind` and a stable `code`.
`Error` keeps its cause through `source`; the embedding gear maps `ErrorKind` to its canonical errors at REST and gRPC boundaries.
The embedding gear implements the `Authorizer` port with stable action and resource names.

#### Registration API

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-interface-registration`

`Engine::registry` returns `WorkflowRegistry`.
It supplies `register_workflow`, `register_contract`, `registration`, `unregister`, and `activate`.
It is a trusted embedded capability under the service scope.
It **MUST NOT** have a user-facing REST or gRPC binding.
The API and storage format can change directly while the library is experimental.
Unsupported stored formats **MUST** fail reads.
Development and tests **MUST** use clean databases.
Automatic database deletion is excluded.

### 7.2 External Integration Contracts

#### Activity implementation

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-contract-activity`

Consumers use async functions, closures, or the `Activity` trait.
For `Step::new`, `Step::with_deps` and `Step::activity`, supply positive timeouts no greater than 24 hours.
For `Compensation::new`, supply a separate timeout and typed input/output handler.
Compensation returns `Result<(), ActivityError>`.
`.retry(policy)` sets its retry policy.

Rust **MUST** reject incompatible chains, fan-out, compensators, handles, and checkpoint reads.
Build **MUST** reject incorrect IDs, policies, and checkpoint dependencies.
`ActivityContext` **MUST** supply named accessors without `Deref` or public fields.
Public Debug and error formatting **MUST** omit handlers, payloads, and credentials.
Handlers **MUST** support cancellation and confirm cleanup before termination.
Credentials **MUST** stay in provider clients.
Use a new versioned definition name for changed payloads or business behavior.

## 8. Use Cases

### Submit background work

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-usecase-background-work`

Register an active definition.
Submit input and a request key through the typed client.
Save the run ID and expected workflow reference.
Read results with the returned handle.
Use `attach` to restore that handle later.
See [single activity](../examples/single_activity.rs).

### Recover after worker loss

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-usecase-worker-recovery`

Restore matching handlers and compensators on the replacement host.
Recovery waits for expired claims and continues the saved phase.
The provider deduplicates calls whose effects preceded a lost checkpoint.
See [contract and bindings](../examples/contract_and_bindings.rs) and [provider crash tests](../tests/integration/provider_stages_tests.rs).

### Retire or replace a definition

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-usecase-retire-definition`

Read the catalog revision.
Use `Retain` to close admission and keep current execution.
Use `activate` to reopen that generation.
Use `CancelAndRelease` to stop work and complete saga compensation.
If compensation fails, restore bindings and issue authorized Retry.
Wait for `Released` before you prepare and activate a replacement generation.
Use a new name for an incompatible contract.
See [two-tenant CRM](../examples/dynamic_registration.rs).

### Compensate an order

Enable `.saga()` and declare payment and stock compensators.
A final activation failure starts compensation.
Stock compensation runs before payment compensation.
If compensation retries are exhausted, use Retry to continue compensation.
See [order saga](../examples/sequential_workflow.rs).

### Process a variable list

Get an item list from run input or a checkpoint.
Use `.for_each` to create durable instances.
Read joined results in input order.
An empty list makes no provider call.
See [parallel and fan-out](../examples/parallel_workflow.rs).

## 9. Acceptance Criteria

These records stay unchecked here.
The [Makefile](../../../Makefile) defines checks run by the [CI workflow](../../../.github/workflows/ci.yml).
`make test-durable-execution-pg` runs the PostgreSQL suites behind the `integration` feature.
ADR Confirmation sections identify the validating test source.
A passing source test is necessary before a release review marks a criterion complete.

- [ ] Accepted work survives failures before enqueue and before acknowledgment.
- [ ] Duplicate delivery keeps checkpoints and stale workers cannot commit results.
- [ ] Retry and Resume reject stale epochs and incorrect forward generations.
- [ ] Late handler binding keeps attempts and restores delivery.
- [ ] Queue routes and separate pool concurrency behave as configured.
- [ ] Definition release survives restart and waits for claims and saga compensation.
- [ ] Foreign access fails and policy outages do not become business failures.
- [ ] Lifecycle failure clears readiness and signals active handlers.
- [ ] Compile-time tests reject incompatible authoring, handles, and checkpoints.
- [ ] Result reads reject a foreign fingerprint before payload hydration.
- [ ] Error and Debug output exclude secrets, handlers, and payloads.
- [ ] Fan-out handles zero, one, and many items with ordered results.
- [ ] Expansion limits reject work before handler calls.
- [ ] Concurrent workers keep stage limits, separate retries, and fences.
- [ ] Restart keeps committed expansion and publication intents.
- [ ] Saga failure and Abort compensate confirmed results in reverse order.
- [ ] Cancel and Resume keep forward progress without compensation.
- [ ] Compensation failure stops compensation and Retry keeps successful compensation checkpoints.
- [ ] Worker restart keeps provider deduplication during forward and compensation calls.
- [ ] Get and inspect use one projection query; list uses two queries for any page size.
- [ ] Separate payload/history corruption does not prevent correct progress reads.
- [ ] Progress cursor matches committed events, including batches above 256 events.
- [ ] Unsupported storage formats fail without compatibility fallback or database deletion.
- [ ] `ControlPlane` mode rejects scheduling operations with `FailedPrecondition` and code `DELIVERY_DISABLED`.
- [ ] An unconstrained scope for an action that requires constraints is rejected.
- [ ] Library migrations are recorded separately, so later outbox migrations reach existing databases.
- [ ] Callers with a nil subject or tenant fail before the `Authorizer` call.
- [ ] An `Authorizer` timeout returns `Unavailable` and logs no policy data.

Two checks still have no dedicated runtime evidence:

- [ ] Corrupt separate activity payload and epoch/attempt history records, then read the correct stored projection.
- [ ] Restore saga bindings while `Stopping` after compensation failure, then Retry and complete definition release.

The [query-recorder test](../tests/unit/audit_storage_tests.rs) currently corrupts only the journal header.
Existing [registration](../tests/integration/registry_tests.rs) and [process](../tests/integration/process_tests.rs) tests cover ordinary release.
They do not directly prove the saga release scenario above.

## 10. Dependencies

The library uses PostgreSQL, ToolKit DB, ToolKit Outbox, Apalis, and ToolKit security types.
It has no dependency on the platform error model; the embedding gear maps errors.
It has no dependency on AuthN, AuthZ, ClientHub, or gear lifecycle crates.
The embedding gear supplies the `Db`, migration host, `Authorizer`, queue DSN, settings, and lifecycle.
Consumer handlers supply business operations and external cleanup.
See [DESIGN](DESIGN.md#35-external-dependencies) for boundaries.

## 11. Assumptions

- PostgreSQL and an `Authorizer` with a working service identity are necessary for processing.
- One database or schema hosts one engine, and queue names are unique per database.
- Hosts use matching contracts and queue names.
- Applications restore local bindings after restart.
- Providers supply idempotency or reconciliation for repeated calls.
- Development and test databases use the current format.

## 12. Risks

An effect can succeed before checkpoint commit.
The provider must deduplicate or reconcile a repeated call.
A handler can leave external work active after task cancellation.
The runtime keeps claims when cleanup is unconfirmed.
Policy outages can pause processing and definition release.
After compensation failure, operator Retry can be necessary while the definition stays `Stopping`.
Missing compensators leave some confirmed business effects outside compensation.

## 13. Traceability

- [DESIGN](DESIGN.md): architecture and storage.
- [README](../README.md): library usage.
- [README execution modes](../README.md#execution-modes) and [README authorization](../README.md#authorization): modes and the `Authorizer` port.
- [Makefile](../../../Makefile) and [CI workflow](../../../.github/workflows/ci.yml): check commands.
- [ADR 0001](adrs/0001-apalis-with-transactional-outbox.md): atomic publication.
- [ADR 0002](adrs/0002-task-execution-framework-selection.md): transport choice.
- [ADR 0003](adrs/0003-typed-sdk-and-journal-boundary.md): public API boundary.
- [ADR 0004](adrs/0004-saga-fanout-and-progress.md): current contract and runtime decisions.
- [ADR 0005](adrs/0005-embeddable-library.md): embeddable library, authorization port, and execution modes.
