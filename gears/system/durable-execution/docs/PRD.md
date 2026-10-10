# PRD: Durable execution

Requirements for the shared runner used by gears to submit and execute persisted background work.

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
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Traceability](#13-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

Durable execution is a system gear for background Rust activities and multi-step
workflows submitted by other gears. It owns the execution journal, migrations,
retries, cancellation and recovery. Consumer gears define the work and register
local handlers through the SDK.

### 1.2 Background / Problem Statement

Gears need to submit background tasks and run workflows reliably across process
restarts. Durable execution persists serialized inputs, execution state, and
checkpoints so consumer gears do not have to implement their own task journal,
retries, and recovery.

Contracts and execution data are serializable. Rust handlers remain local objects
and must be registered by the application after each restart.

### 1.3 Goals (Business Outcomes)

- Give consumer gears one runner for persisted background work.
- Preserve accepted work across worker loss and delivery failures.
- Restore execution from confirmed checkpoints without rerunning completed steps.
- Keep execution control subject to tenant, owner and resource authorization.

These are platform engineering outcomes. Market positioning, revenue targets and
end-user UI requirements do not apply to this internal system gear.

### 1.4 Glossary

| Term | Definition |
|---|---|
| Definition | Versioned contract with step IDs, timeouts, retry policies, stages and input sources. |
| Activity | One step implemented by a local Rust handler. |
| Run | Persisted execution of a definition with input, owner and progress. |
| Checkpoint | Activity result committed to the journal after successful execution. |
| Claim | Time-limited ownership of an activity, renewed by heartbeat and protected by fencing. |
| Epoch | Execution revision advanced by manual Retry/Resume. |
| Registration generation | Catalog generation used to fence runs after definition revocation. |

## 2. Actors

### 2.1 Human Actors

#### Gear developer

**ID**: `cpt-cf-durable-execution-actor-developer`

- **Role**: Defines workflow contracts and handlers, integrates the SDK and handles external-effect idempotency.
- **Needs**: Submit and inspect work without implementing persistence and recovery in each consumer.

#### Operator

**ID**: `cpt-cf-durable-execution-actor-operator`

- **Role**: Configures database access, service credentials, queues and host lifecycle.
- **Needs**: Recover workers and stop execution within the configured lifecycle budget.

### 2.2 System Actors

#### Consumer gear

**ID**: `cpt-cf-durable-execution-actor-consumer`

- **Role**: Registers definitions and submits, inspects or controls runs through the embedded SDK.

#### Host process

**ID**: `cpt-cf-durable-execution-actor-host`

- **Role**: Hosts the shared gear instance, binds local handlers and runs lifecycle-managed workers and reconciliation.

#### Authorization services

**ID**: `cpt-cf-durable-execution-actor-policy`

- **Role**: AuthN supplies service identity; PolicyEnforcer obtains PDP scopes for user and service operations.

## 3. Operational Concept & Environment

Platform conventions come from the [architecture manifest](../../../../docs/ARCHITECTURE_MANIFEST.md)
and [ToolKit guide](../../../../docs/toolkit_unified_system/README.md).
Gear-specific constraints:

- The full runtime requires PostgreSQL. SQLite is used for journal tests.
- Consumers linked into a host share one gear instance. Cooperating processes
  share the journal and queue names; each execution host binds its own handlers.
- Execution and delivery are disabled by default. `execute_activities=true`
  starts workers and the delivery runtime; `delivery_enabled=true` starts delivery
  without workers when execution is disabled. With both flags disabled, the host
  reconciles the catalog but cannot publish new runnable work.
- The journal uses fixed `durable_*` tables and `durable_delivery_outbox`.
  The official Apalis adapter owns the shared `apalis` schema and a separate pool.
- Enabled execution or delivery requires the configured service secret environment
  variable before startup. Configuration and permissions are listed in [configuration](configuration.md).

## 4. Scope

### 4.1 In Scope

Persisted execution, local activities, sequential and parallel steps, checkpoints,
retries, cancellation, epochs, coalescing, named queues, dynamic registration,
authorized inspection and lifecycle recovery.

### 4.2 Out of Scope

Remote handler bindings, serialization of Rust functions, user-facing REST/gRPC
execution endpoints, arbitrary-code sandboxing and consumer-specific business
logic. Moving existing consumers or production data is separate deployment work.

## 5. Functional Requirements

### Persisted admission

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-fr-admission`

The gear **MUST** persist an accepted run and its delivery intent atomically.
Admission **MUST** check the definition's active registration generation.
An idempotent replay **MUST** return the existing run; conflicting input under the
same request key **MUST** be rejected. Closed admission **MUST NOT** create a new
run or coalescing successor.

- **Rationale**: A committed request must survive loss of the submitting process.
- **Actors**: `cpt-cf-durable-execution-actor-consumer`

### Activity progress

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-fr-progress`

The gear **MUST** execute sequential activities and declared parallel groups,
commit successful results as checkpoints and retain confirmed checkpoints across
recovery. Duplicate delivery **MUST NOT** rerun a completed checkpoint.

- **Rationale**: Recovery must preserve completed work.
- **Actors**: `cpt-cf-durable-execution-actor-host`

### Retry and continuation

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-fr-continuation`

The gear **MUST** apply activity timeout and retry policies through the journal.
Authorized Retry/Resume **MUST** validate the expected epoch, preserve completed
checkpoints and reject revoked registration generations. Missing local handlers
**MUST** preserve runnable state, due time, attempts and checkpoints; late binding
**MUST** restore delivery without manual Resume. A contract fingerprint mismatch
**MUST** remain a blocking error.

- **Rationale**: Temporary worker unavailability must not consume business retries.
- **Actors**: `cpt-cf-durable-execution-actor-consumer`, `cpt-cf-durable-execution-actor-host`

### Cancellation

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-fr-cancellation`

The gear **MUST** support authorized cancellation, including epoch-checked commands,
and retain confirmed checkpoints. It **MUST** signal running handlers and retain
claims until safe release or lease expiry when handler termination is unconfirmed.
Aborting a Rust future does not establish that external work stopped.

- **Rationale**: Cancellation must not discard confirmed progress or imply external cleanup.
- **Actors**: `cpt-cf-durable-execution-actor-consumer`, `cpt-cf-durable-execution-actor-host`

### Definition registration

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-fr-registration`

The gear **MUST** allow asynchronous registration before and after host startup.
Matching contracts **MUST** be shareable across processes; incompatible contracts
under the same name **MUST** be rejected. Duplicate local bindings **MUST** preserve
the original handlers. Registering an existing version **MUST NOT** reactivate it.

`Retain` **MUST** close new admission while preserving existing execution and
Retry/Resume. For an unreleased registration, `CancelAndRelease` **MUST** revoke
its generation and return `Stopping`. A repeat for an already released registration
**MUST** preserve `Released`. Lifecycle reconciliation **MUST** complete cancellation
until runs finish and active claims end, then record `Released`. Reconciliation
**MUST** also run on hosts without activity workers.
Each host **MUST** release local bindings after observing release under authorization.

Control commands **MUST** validate the expected revision. `activate` **MUST** reopen
a retired generation or open a new generation after release; activation during
`Stopping` **MUST** fail. Catalog state does not report handler readiness on every host.

- **Rationale**: Consumers need to manage definitions while the host is serving.
- **Actors**: `cpt-cf-durable-execution-actor-consumer`, `cpt-cf-durable-execution-actor-host`

### Queue routing

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-fr-queues`

The gear **MUST** route by exact versioned definition name, use the default queue
for unmatched names and enforce each queue's configured concurrency independently.

- **Rationale**: One workload must not consume another queue's configured worker slots.
- **Actors**: `cpt-cf-durable-execution-actor-operator`, `cpt-cf-durable-execution-actor-host`

### Coalescing

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-fr-coalescing`

The gear **MUST** support keyed coalescing with at most one active run and one
parked successor per slot. Coalescing **MUST** remain within a registration
generation and reject incompatible input for a reused key.

- **Rationale**: Repeated requests during execution need a bounded pending successor.
- **Actors**: `cpt-cf-durable-execution-actor-consumer`

### Inspection

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-fr-inspection`

The SDK **MUST** return consistent progress and an event cursor from `get`.
Events notify clients to reread progress. Results and epoch/attempt history
**MUST** be explicit reads; progress **MUST** omit inputs, checkpoint payloads and
credentials. Administrative inspection/listing **MUST** use separate authorization
and **MUST NOT** grant result access. Filters **MUST** use typed statuses.
`Failing` **MUST** distinguish a failed step with continuing siblings from final
`Failed`. Unknown legacy timestamps **MUST NOT** be inferred from update times.
Retained checkpoints **MUST** preserve their origin epoch.

- **Rationale**: Consumers need to observe progress under the caller's access constraints.
- **Actors**: `cpt-cf-durable-execution-actor-consumer`

## 6. Non-Functional Requirements

Platform baselines are defined in the [ToolKit guide](../../../../docs/toolkit_unified_system/README.md)
and [security guidelines](../../../../guidelines/SECURITY.md).
The following requirements cover execution-specific guarantees.

### Ownership and recovery

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-nfr-fencing`

The gear **MUST** renew claims at the configured heartbeat cadence and reject
checkpoints from stale claims, epochs or revoked registration generations.
Lease expiry **MUST** permit recovery after worker loss. Activity timeout remains
independent of the renewable lease.

### Delivery semantics

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-nfr-delivery`

The gear **MUST** recover committed delivery intents after enqueue or acknowledgment
failure and tolerate duplicate delivery. External effects are at least once;
exactly-once effects are not guaranteed. Consumers **MUST** deduplicate effects
using `ActivityContext::idempotency_key` where replay would be unsafe.

### Authorization

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-nfr-authorization`

User operations **MUST** enforce PDP tenant, owner and resource constraints.
Service operations **MUST** obtain service identity through AuthN and fresh PDP
scopes between operations; service identity **MUST NOT** replace `run.owner`.
`CancelAndRelease` **MUST** require an unconstrained `cancel_definition` grant
before changing catalog state.

AuthN/PDP outages or denials **MUST** pause processing without recording a workflow
business failure. Authorization loss **MUST** stop handlers; unconfirmed claims
wait for lease expiry. Pending release **MUST** retain `Stopping` and bindings
until authorization recovers.

### Lifecycle

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-nfr-lifecycle`

Workers and reconciliation **MUST** run under host lifecycle cancellation and
readiness reporting. Shutdown **MUST** use one configured wait budget across
workers, with time reserved for handler cleanup, claim persistence and lifecycle
teardown. The host deadline may shorten this budget. Unconfirmed termination or
release **MUST** leave claims for recovery. Shutdown **MUST NOT** request business
cancellation or discard completed checkpoints. Readiness **MUST** clear when
serving stops or fails. Retryable control-plane
database failures **MUST** use bounded backoff and the configured consecutive
failure limit. Readiness does not establish AuthN/PDP availability.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### Execution SDK

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-interface-execution`

`cf-gears-durable-execution-sdk` exposes `DurableExecution` through
ClientHub for submission, progress, events, results, history, cancellation and
Retry/Resume. `DurableExecutionClient` provides typed methods above this object-safe
trait. Cancel, Retry and Resume require the expected epoch. `ExecutionInspector`
provides administrative progress and listing. Trait methods return
`toolkit_canonical_errors::CanonicalError`.

#### Registration SDK

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-interface-registration`

`WorkflowRegistry` exposes `register`, `register_contract`, `registration`,
`unregister` and `activate`. It is a trusted embedded management capability and
**MUST NOT** be exposed as a user-facing REST/gRPC API.

Both crates start at version `0.1.0`; these interfaces are experimental.
This PRD adds no compatibility policy beyond workspace release conventions.

### 7.2 External Integration Contracts

#### Activity implementation

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-contract-activity`

Consumers use async functions/closures or implement `Activity` with associated
serializable input/output types. `WorkflowBuilder` composes typed sequential stages
and parallel tuples or homogeneous lists. Branch outputs retain declaration order.
Earlier checkpoints require declared dependencies; type mismatches fail compilation
and invalid IDs, policies or references fail before registration. Contracts persist
versioned input sources, not Rust functions or type names. Consumers register
handlers on every execution host and use a new versioned definition name for
payload or handler semantics changes. Handlers **MUST** honor cooperative cancellation and
complete external-work cleanup before reporting termination.

## 8. Use Cases

### Submit background work

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-usecase-background-work`

**Actor**: `cpt-cf-durable-execution-actor-consumer`

With an active definition and authorized caller, submit input and an idempotency
key through `start`, then read progress by run ID. Repeating the same request
returns that run. A worker executes locally registered handlers and commits results.

### Recover after worker loss

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-usecase-worker-recovery`

**Actor**: `cpt-cf-durable-execution-actor-host`

After a worker exits, the application restores handlers on a replacement host.
Recovery reclaims expired work and continues from confirmed checkpoints.
An effect performed before checkpoint commit can repeat and needs deduplication.

### Retire or replace a definition

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-usecase-retire-definition`

**Actor**: `cpt-cf-durable-execution-actor-consumer`

Read the current revision and unregister with `Retain` to close admission while
existing work continues. The registration stays `Retired`; `activate` reopens the
same generation with its existing bindings.

With `CancelAndRelease`, wait for `Released`, prepare replacement bindings for the
same contract and explicitly activate the new generation. An incompatible contract
requires a new versioned definition name.

## 9. Acceptance Criteria

- [ ] A committed run survives failure before enqueue and after enqueue before Outbox acknowledgment.
- [ ] Duplicate delivery and worker termination preserve completed checkpoints; stale workers cannot commit results.
- [ ] Retry/Resume reject stale epochs and revoked generations without changing saved progress.
- [ ] Late handler registration restores delivery without consuming attempts or losing checkpoints.
- [ ] Named queues enforce independent concurrency and exact-name/default routing.
- [ ] Retain closes admission; CancelAndRelease survives controller restart and releases only after claims end.
- [ ] Foreign-tenant operations and partial-scope definition cancellation are rejected; policy outages do not become business failures.
- [ ] Lifecycle stop or runtime failure clears readiness and initiates handler cancellation.
- [ ] Typed chains and parallel joins reject incompatible types; declared checkpoint inputs survive recovery.
- [ ] Progress and cursor agree; paged events, history and explicit result reads preserve epochs and checkpoint provenance.
- [ ] Partial parallel failure remains Failing until eligible siblings settle; interrupted attempts are not reported as success.
- [ ] Legacy records remain readable and incompatible contracts cannot replace their saved checkpoints.

Verification uses the unit, PostgreSQL and subprocess suites described in
[testing](testing.md). Coverage follows the shared workspace pipeline and Codecov
policy, without a separate gear threshold.

## 10. Dependencies

| Dependency | Purpose | Criticality |
|---|---|---|
| PostgreSQL and toolkit-db | Scoped journal, catalog, migrations and transactional Outbox. | p1 |
| Apalis PostgreSQL adapter | Serialized delivery, worker coordination and orphaned-task recovery. | p1 |
| AuthN resolver and AuthZ resolver/PDP | Service credentials and operation scopes. | p1 |
| ToolKit host and ClientHub | Embedded SDK resolution, configuration and lifecycle. | p1 |
| Consumer handlers | Local activity implementations and external-effect cleanup. | p1 |

## 11. Assumptions

- PostgreSQL, the host lifecycle and authorized service credentials are available for processing to progress.
- Cooperating hosts use compatible contracts and coordinated queue names.
- Applications restore handlers after restart; the database cannot reconstruct Rust objects.
- Consumers own payload content, external-effect idempotency and cleanup of external work.

## 12. Risks

| Risk | Impact | Handling |
|---|---|---|
| Effect succeeds before checkpoint commit | Recovery repeats the effect. | Consumer deduplication with the activity idempotency key. |
| Handler ignores cancellation or leaves external work running | Cleanup is unconfirmed; claims wait for expiry. | Cooperative handlers and consumer-managed external cleanup. |
| AuthN/PDP unavailable or access revoked | Processing and definition release pause. | Retry authorization; retain journal state and pending release. |
| Missing local handlers or incompatible contract | Work waits for bindings or blocks on fingerprint mismatch. | Restore matching handlers; version incompatible definitions. |
| Adapter RC API changes | Dependency upgrades can break integration. | Pin versions and verify upgrades against recovery tests and workspace TLS policy. |

## 13. Traceability

- [Technical design](DESIGN.md).
- [Usage and configuration](../README.md).
- [Tests and shared coverage](testing.md).
- [ADR 0001: Apalis with transactional Outbox](adrs/0001-apalis-with-transactional-outbox.md): admission atomicity and delivery recovery.
- [ADR 0002: Task execution framework selection](adrs/0002-task-execution-framework-selection.md): runtime choice and integration constraints.
- [ADR 0003: Typed SDK and persisted journal](adrs/0003-typed-sdk-and-journal-boundary.md): authoring and observation boundaries.
