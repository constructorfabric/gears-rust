---
description: "Technical design of the embeddable durable-execution library: the typed API, the engine lifecycle, the staged execution runtime, journal storage with lease fencing, transactional outbox delivery through Apalis, and the authorization port."
---

# Technical Design: Durable execution

This document describes the implementation of the [PRD](PRD.md).
Use the [technical terms](#23-terminology) with the meanings given there.

- [ ] `p3` - **ID**: `cpt-cf-durable-execution-design-runner`


<!-- toc -->

- [1. Architecture Overview](#1-architecture-overview)
  - [1.1 Architectural Vision](#11-architectural-vision)
  - [1.2 Architecture Drivers](#12-architecture-drivers)
  - [1.3 Architecture Layers](#13-architecture-layers)
- [2. Principles & Constraints](#2-principles--constraints)
  - [2.1 Design Principles](#21-design-principles)
  - [2.2 Constraints](#22-constraints)
  - [2.3 Terminology](#23-terminology)
  - [2.4 Documentation Language](#24-documentation-language)
- [3. Technical Architecture](#3-technical-architecture)
  - [3.1 Domain Model](#31-domain-model)
  - [3.2 Component Model](#32-component-model)
  - [3.3 API Contracts](#33-api-contracts)
  - [3.4 Internal Dependencies](#34-internal-dependencies)
  - [3.5 External Dependencies](#35-external-dependencies)
  - [3.6 Interactions & Sequences](#36-interactions--sequences)
  - [3.7 Database schemas & tables](#37-database-schemas--tables)
  - [3.8 Deployment Topology](#38-deployment-topology)
- [4. Additional Context](#4-additional-context)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

Durable execution is the library `cf-gears-toolkit-durable-execution` (crate `toolkit_durable_execution`).
A consumer gear embeds one `Engine`, like the transactional outbox in `toolkit-db`.
The library owns the journal schema, execution control, and the engine lifecycle.
The embedding gear owns the database, the migration host, authorization, configuration, and its gear lifecycle.
Consumer code supplies local Rust handlers through the same crate.
The journal controls work eligibility, retries, checkpoints, fan-out, and compensation.
Apalis transports delivery identifiers and supplies workers.
ToolKit Outbox connects journal commits to queue delivery.

PostgreSQL stores the catalog and journal in the embedding gear's database.
Local registries store handler bindings.
Applications must restore these bindings after each restart.
The library has no ClientHub registration, no gear configuration, and no environment variable reads.
Its stability tier is experimental.
[ADR 0005](adrs/0005-embeddable-library.md) records this hosting model.

### 1.2 Architecture Drivers

#### Functional Drivers

| PRD requirement | Mechanism |
|---|---|
| `cpt-cf-durable-execution-fr-admission` | One transaction for the accepted run and publication intent. |
| `cpt-cf-durable-execution-fr-progress` | Confirmed checkpoints and separate activity instances. |
| `cpt-cf-durable-execution-fr-continuation` | Epoch checks, retry budgets, and late handler binding. |
| `cpt-cf-durable-execution-fr-cancellation` | Forward pause, cancellation tokens, and lease recovery. |
| `cpt-cf-durable-execution-fr-registration` | Catalog revisions, generations, and definition release. |
| `cpt-cf-durable-execution-fr-queues` | Exact-name routes and separate queue worker pools. |
| `cpt-cf-durable-execution-fr-coalescing` | One active run and one pending successor per scoped slot. |
| `cpt-cf-durable-execution-fr-inspection` | Authorized progress without payload access. |
| `cpt-cf-durable-execution-fr-typed-results` | Typed handles and fingerprint checks before payload reads. |
| `cpt-cf-durable-execution-fr-fanout` | Persisted expansion, stable indexes, and ordered results. |
| `cpt-cf-durable-execution-fr-compensation` | Persisted compensation with a separate retry budget. |
| `cpt-cf-durable-execution-fr-projection` | A committed progress projection and event cursor. |

#### Non-Functional Drivers

| PRD requirement | Mechanism | Test area |
|---|---|---|
| `cpt-cf-durable-execution-nfr-fencing` | Journal CAS, claim fences, catalog locks, and leases. | Stale claims and concurrent workers. |
| `cpt-cf-durable-execution-nfr-delivery` | Transactional Outbox and delivery recovery. | Queue failures and worker crashes. |
| `cpt-cf-durable-execution-nfr-authorization` | Fresh `Authorizer` scopes and SecureORM. | Tenant isolation and access revocation. |
| `cpt-cf-durable-execution-nfr-lifecycle` | `Runner` cancellation and one shutdown budget. | Start, stop, and runtime failure. |

#### Key ADRs

- [ADR 0001](adrs/0001-apalis-with-transactional-outbox.md): publication transactions.
- [ADR 0002](adrs/0002-task-execution-framework-selection.md): transport selection.
- [ADR 0003](adrs/0003-typed-sdk-and-journal-boundary.md): public API and internal journal.
- [ADR 0004](adrs/0004-saga-fanout-and-progress.md): run identity, fan-out, compensation, and progress storage.
- [ADR 0005](adrs/0005-embeddable-library.md): embeddable library, authorization port, and execution modes.

### 1.3 Architecture Layers

| Layer | Responsibility |
|---|---|
| Public API | Typed authoring, handles, contracts, observation models, errors, and the `Authorizer` port. |
| Domain | Journal transitions, stage execution, definition lifecycle, and registry rules. |
| Infrastructure | Authorized services, storage, migrations, Outbox, adapter, and executor. |
| Engine | `EngineBuilder` settings, execution mode, readiness, `prepare`, and `Runner::run`. |

## 2. Principles & Constraints

### 2.1 Design Principles

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-principle-journal-authority`

The journal is the authority for execution.
Queue delivery can repeat.
Each state change checks persisted ownership and uses a short transaction.
Transactions do not call handlers or external providers.
Repeated transaction attempts must not repeat an external call.

### 2.2 Constraints

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-constraint-runtime-boundary`

PostgreSQL is necessary for the full runtime.
SQLite supports journal and storage tests.
Journal and catalog access uses scoped ToolKit DB operations.
Direct SQLx access applies only to the official adapter's `apalis` schema.
The adapter uses workspace SQLx and TLS settings.

The library is experimental.
The current storage format rejects unsupported versions.
There are no legacy readers or fingerprint fallbacks.
There is no migration from the tables of the former durable-execution gear.
Use a clean development or test database.
The library does not delete existing databases.

Table names use the fixed `durable_execution__` prefix.
One database or schema therefore hosts one engine.
A caller-supplied prefix needs secure-ORM support in `toolkit-db`, because SeaORM table names are `&'static str`.
The library does not read environment variables or gear configuration.
The embedding gear supplies every setting through `EngineBuilder` and `QueueConfig`.

### 2.3 Terminology

#### Technical Nouns

| Term | Meaning |
|---|---|
| Gear | A ToolKit component with a managed lifecycle. |
| Embedding gear | The consumer gear that owns the database, `Authorizer`, settings, and lifecycle of one engine. |
| Engine | The configured durable-execution instance of one embedding gear. |
| Runner | A prepared engine that serves until its cancellation token fires. |
| Execution mode | The work that one process does: `ControlPlane`, `Dispatcher`, or `Worker`. |
| Authorizer | The port through which the embedding gear supplies access scopes. |
| Workflow | A sequence of declared execution stages. |
| Definition | A named, versioned workflow contract and its local bindings. |
| Contract | The serializable stages, data sources, policies, and limits for a definition. |
| Fingerprint | The hash of the serialized contract identity. |
| Handler | The local Rust function that performs one invocation. |
| Activity template | A step declared in a workflow definition. |
| Activity instance | One run-specific invocation identified by template, optional index, and phase. |
| Forward execution | Business work before compensation. |
| Run | One persisted execution with input, owner, and progress. |
| Stage | A sequential step, static parallel group, or runtime fan-out group. |
| Fan-out | Expansion of a run-specific item list into durable activity instances. |
| Branch | One activity instance in a parallel or fan-out stage. |
| Join | The ordered collection of successful branch outputs. |
| Saga | A workflow with automatic compensation enabled. |
| Compensator | A handler that reverses a confirmed successful activity's business effect. |
| Compensation | Execution of declared compensators after final failure or Abort. |
| Receipt | A stored output that identifies a confirmed provider operation. |
| Checkpoint | A successful activity output committed to the journal. |
| Confirmed success | A successful activity result stored in a committed checkpoint. |
| Successful process exit | A process exit with status zero after the example assertions complete. |
| Journal | Persisted execution state that controls work eligibility. |
| Projection | The stored progress view without input, result, or history payloads. |
| Payload | Serialized application input or output. |
| Claim | Time-limited ownership of one activity instance. |
| Lease | The time interval during which a claim supplies execution ownership. |
| Fence | A claim counter that rejects results from an old owner. |
| CAS | Compare-and-swap update that checks the expected run revision. |
| Epoch | Execution counter advanced by manual Retry or Resume. |
| Catalog | Shared definition contracts and registration lifecycle records. |
| Registration generation | Catalog counter that distinguishes released and reactivated bindings. |
| Binding | A local handler or compensator attached to a registered contract. |
| Delivery generation | Counter that invalidates older queue delivery messages. |
| Publication intent | A committed record that makes queue publication necessary. |
| Outbox | ToolKit transaction records and delivery processing for publication commands. |
| Cursor | The last committed event sequence number in a run's progress. |
| Idempotency key | A stable provider key used to prevent a repeated business effect. |
| Reconciliation | Recovery that compares persisted state with eligible work or provider effects. |
| PDP | Policy decision point that supplies current authorization constraints. |
| SecureORM | Scoped database operations that apply authorization constraints. |

#### Technical Verbs

Use these verbs only with the software meanings given here.

| Verb | Meaning |
|---|---|
| Authorize | Get and apply a current policy decision for an operation. |
| Check | Compare software state, identifiers, policies, or query output with an expected value. |
| Confirm | Record the successful result or completed cleanup of a software operation. |
| Declare | Add stage, dependency, or policy metadata to a workflow contract. |
| Enforce | Apply a software permission, constraint, or limit to an operation. |
| Bind | Attach a local handler to a registered contract. |
| Claim | Acquire persisted ownership before a handler call. |
| Commit | Complete a database transaction successfully. |
| Compensate | Run a declared compensator for a confirmed successful activity. |
| Decode | Convert a stored payload into its requested Rust type. |
| Deduplicate | Prevent another business effect from a repeated request. |
| Enqueue | Add a delivery message to a named queue. |
| Hydrate | Load normalized payload and history records into execution state. |
| Materialize | Store the run-specific instances and inputs for a stage. |
| Persist | Store data so it survives process restart. |
| Reconcile | Compare saved state and restore the necessary delivery or cleanup. |
| Register | Store a contract or attach its local bindings through the registry. |
| Serialize | Convert a value into its stored or transported representation. |
| Support | Make a documented software operation available to a caller. |

API command names such as Cancel, Abort, Retry, Resume, Retain, and CancelAndRelease keep their API meanings.
Do not use pause and Abort as synonyms.
Use compensation consistently for saga business cleanup.

### 2.4 Documentation Language

Use [ASD-STE100 Issue 9](https://www.asd-ste100.org/about_STE.html) as the English writing reference for these documents.
Use short sentences and active verbs.
Give one instruction in each procedural sentence.
Use a maximum of 20 words in a procedural sentence.
Use a maximum of 25 words in a descriptive sentence.
Put a necessary condition before its instruction.
Use one term consistently for each technical meaning.

Keep Rust identifiers, code, commands, URLs, schema names, and traceability IDs unchanged.
These are technical names, not ordinary English prose.
Use **MUST** and **MUST NOT** as requirement labels in the PRD.
They mark mandatory behavior.

## 3. Technical Architecture

### 3.1 Domain Model

See the [run state machine](../src/state/) and the [public modules](../src/lib.rs).

| Model | Responsibility |
|---|---|
| `Journal` | Run input, owner, fingerprint, execution epoch, revision, and delivery generation. |
| `ExecutionState` | The only execution engine: materialized stages, activity instances, selected inputs, receipts, and execution phase. |
| `Definition` | Contract, registration state, revision, generation, and revocation boundary. |
| `Registry` | Local contracts and generation-specific handler bindings. |

`StageSpec` distinguishes sequential, static parallel, and runtime fan-out stages.
Every contract carries a `DataFlowSpec`; low-level definitions use `DataFlowSpec::independent`.
Low-level definitions are built from `DefinitionParts`; `build` validates them once.
A built `ExecutionDefinition` is immutable and caches its contract and fingerprint.
A stage materializes its instances when the previous stage checkpoints.
A pause records stages that complete meanwhile but starts no new stage.
An activity template belongs to a definition.
An activity instance belongs to a run.
Its identity contains the template ID, optional fan-out index, and execution phase.
Each instance has its own attempts, fence, lease, due time, and checkpoint.
Queue delivery includes this identity and its encoded activity ID.
The runtime rejects inconsistent delivery identities.

The contract fingerprint includes stages, data sources, dependencies, policies, compensation, and fan-out limits.
Run-specific item counts are outside the fingerprint.
Rust functions and type names are outside the fingerprint.
Use a new versioned definition name for a changed payload format or changed business behavior.

Run revision controls aggregate CAS updates.
Claim fences reject stale owners.
Retry and Resume advance the execution epoch.
Release and reactivation change the registration generation.
Delivery generation rejects old queue messages.
These counters have separate checks.

#### Fan-out execution

When a stage becomes eligible, the runtime selects its exact input list.
One transaction stores item inputs, stable indexes, publication intents, events, and progress.
Recovery uses the saved expansion.
It does not create the same branches again.
An empty list completes with an empty result and no handler calls.
Join returns results in input order.
Retry repeats only unfinished branches.

The default stage limit is 1024 items.
The default stage concurrency is 16.
Configured item limits are 1 through 1024.
Configured stage concurrency is 1 through 128.
A run can have at most 4096 forward activity instances.
This limit includes sequential and static parallel instances.
An incorrect expansion fails before branch execution.
The journal records `fanout_limit_exceeded` as a stage error.

Persisted running claims enforce stage concurrency across processes through CAS.
The queue pool also enforces its configured concurrency on each host.
An expansion creates publication intents for all branches.
Subsequent delivery refresh uses available stage capacity.

#### Saga execution

`.saga()` enables automatic compensation after a permanent forward failure or exhausted retries.
Explicit Abort also stops forward work and starts compensation for a saga.
The runtime stops new forward claims and signals active handlers.
Compensation waits for confirmed cleanup or lease expiry.

The runtime compensates only confirmed successful activities with declared compensators.
It uses reverse stage order and reverse branch-index order.
One compensator runs at a time.
Each compensator receives the original invocation input and successful output.
Its stable idempotency key differs from the forward key.

The original failure or Abort reason stays separate from the compensation error.
Progress exposes `Compensating`, `CompensationFailed`, and `Compensated` as separate states.
After compensation retries are exhausted, the run becomes `CompensationFailed`.
Manual Retry repeats the failed compensator and continues the remaining compensation.
Successful compensation checkpoints stay complete.
`Compensated` rejects forward Retry and Resume.
A new run is necessary for another business attempt.

```mermaid
stateDiagram-v2
    [*] --> Forward
    Forward --> Succeeded: All forward stages complete
    Forward --> Paused: Cancel
    Paused --> Forward: Resume
    Forward --> Stopping: Final failure or Abort
    Paused --> Stopping: Abort
    Stopping --> Compensating: Cleanup confirmed or leases expired
    Compensating --> CompensationFailed: Retry budget exhausted
    CompensationFailed --> Compensating: Manual Retry
    Compensating --> Compensated: Compensation complete
    Succeeded --> [*]
    Compensated --> [*]
```

This diagram shows saga phases.
`Paused` is a forward execution condition.
`Stopping` reports `Cancelling` while forward claims stay unsettled.
Cancel during compensation is rejected.
Abort does not change a successful or completed run.
Host shutdown keeps the execution phase for recovery.
Abort is final for workflows without saga compensation too.

### 3.2 Component Model

```mermaid
flowchart LR
    Consumer[Embedding gear] --> Engine
    Consumer -. implements .-> Authorizer
    Engine --> Service[Execution service]
    Engine --> Registrar[Registrar]
    Engine --> Runner
    Registrar --> Registry[Local handlers and compensators]
    Service --> Store[JournalStore]
    Registrar --> Store
    Store --> Authorizer
    Store --> DB[(Journal, catalog, and progress)]
    Store --> Outbox[ToolKit Outbox]
    Outbox --> Apalis[Apalis queues]
    Apalis --> Executor
    Executor --> Registry
    Executor --> Store
    Runner --> Executor
    Runner --> Store
```

#### Execution services

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-component-services`

`Service` authorizes caller operations and constructs domain transitions.
`Registrar` authorizes contract registration and local handler binding.
Both get scopes from the embedding gear's `Authorizer`.
Both use `JournalStore` for persistence.
Binding requests a delivery refresh.
If refresh fails, registration stays correct and periodic reconciliation retries delivery.

#### Journal storage

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-component-storage`

`JournalStore` owns scoped CRUD, hydration, CAS, catalog locks, request keys, and coalescing.
Its transaction stores journal changes, normalized records, events, progress, and Outbox commands together.
Wake signals run only after commit.
Periodic scans recover a lost signal.
Shared catalog locks order execution against exclusive definition changes.

#### Execution and lifecycle

- [ ] `p1` - **ID**: `cpt-cf-durable-execution-component-runtime`

The engine lifecycle has three phases.
Each phase consumes the previous one.

1. `Engine::builder(db, authorizer)` and `EngineBuilder::build` validate settings without I/O.
   The built `Engine` supplies `client`, `registry`, `inspector`, `readiness`, and `shutdown_timeout`.
2. `Engine::prepare` checks handler bindings.
   With a queue, it connects Apalis to PostgreSQL and starts the delivery Outbox.
   It returns a `Runner`.
3. `Runner::run(cancel)` serves until cancellation and then drains within the shutdown budget.

`ExecutionMode` selects the work of one process.
`ControlPlane` is the default.
It supports observation, cancellation, and catalog reconciliation.
Scheduling operations such as start, Retry, and Resume return `Unavailable` in this mode.
`Dispatcher(QueueConfig)` also admits and schedules runs and publishes due work.
`Worker(QueueConfig)` also executes activities with the registered handlers.
`QueueConfig` holds the queue DSN as a `SecretString`, the default queue, other named queues, and exact-name routes.

`Runtime` owns named queues, Outbox delivery, and dispatch.
Without a queue, the runner reconciles the catalog only.
`Executor` claims an instance before it calls a local handler.
It checks authorization, registration generation, cancellation, timeout, and claim ownership during execution.
Heartbeats renew the lease.
Lease renewal does not change the activity timeout.
Heartbeat intervals use `Delay` to prevent catch-up bursts.

Shutdown uses one monotonic budget from `EngineBuilder::shutdown_timeout`.
Its default is 30 seconds.
For budget `T`, reserve `R = min(5 seconds, T / 6)`.
Handler cleanup ends by `T - 2R`.
Runtime draining ends by `T - R`.
The lifecycle wait ends by `T`.
Cleanup is also limited by the confirmed lease.
The embedding gear uses `Engine::shutdown_timeout` as its stop timeout.
A shorter host deadline can still shorten this budget.
A drain that exceeds the budget returns `LifecycleError` with kind `ShutdownTimedOut`.

Unconfirmed termination leaves the claim for lease recovery.
Shutdown does not request business Cancel or discard checkpoints.
`Readiness` is true only while `Runner::run` serves.
It clears when the runner stops or fails.
Readiness does not confirm `Authorizer` availability.
An aborted Rust task does not confirm termination of external work.
Dropping the `Runner::run` future aborts its workers.

### 3.3 API Contracts

The `Engine` supplies concrete [handles](../src/client.rs).
There is no ClientHub registration and no object-safe public trait.
The embedding gear keeps the handles and exposes its own API.

| Handle | Operations |
|---|---|
| `ExecutionClient` | `start`, `get`, `wait`, `wait_until`, `watch`, `events`, `result`, `step_result`, `history`, `cancel`, `abort`, `retry`, `resume`. |
| `WorkflowRegistry` | `register_workflow`, `register_contract`, `registration`, `unregister`, `activate`. |
| `ExecutionInspector` | `inspect`, `list`. |

All handles are `Send + Sync` and cheap to clone.
A handle is not a grant: each call gets a fresh scope.
`WorkflowRegistry` runs under the service scope and is a trusted capability.
`WorkflowRegistry::register_workflow` registers a built workflow and returns `RegisteredWorkflow` with its typed `WorkflowRef<I, O>`.
`WorkflowRef` computes the contract fingerprint once; typed calls reuse it.
`ExecutionClient::start` returns `StartedRun<I, O>`.
It exposes a typed handle, run ID, requested coalescing generation, and coalescing flag.
`ExecutionClient::wait` and `wait_until` poll progress until a predicate holds.
`ExecutionClient::watch` streams progress after each committed event until the run settles.
`RunHandle<I, O>` contains the run ID and expected contract identity.
`WorkflowRef::attach(run_id)` restores a handle from a saved ID.
Attach does not read storage or grant access.

Use a handle for typed `result` and `step_result` reads.
Internally, a result read supplies the definition name, expected fingerprint, and `ResultSelector`.
The service checks authorized run identity before payload hydration.
Checkpoint reads also check that the selector belongs to the expected contract.
A different contract returns `FailedPrecondition`, even if its JSON is compatible.
A typed payload decode failure returns `Internal`.

`WorkflowBuilder` supports `.then`, `.parallel`, `.for_each`, and `.saga()`.
Static tuple branches can have different output types.
Static list branches have one output type.
Static joins keep declaration order.
Runtime fan-out consumes `Vec<Item>` and produces `Vec<Output>`.
Sequential, static parallel, and fan-out checkpoints have different types.
Static `.branches()` returns references directly.
Fan-out `.branch(index)` creates a typed selector.
Reads check branch existence and readiness.

Supply the timeout argument to `Step::new`, `Step::with_deps`, and `Step::activity`.
Build rejects zero timeouts and timeouts above 24 hours.
Build also checks IDs, policies, and checkpoint dependencies.
`Step::with_deps` declares earlier checkpoints and passes their decoded values to the handler.
The dependency type comes from the reference, so an undeclared read cannot compile.
Build reports the first violation as a `DefinitionErrorKind` with the step ID.
Pre-build references share a private, write-once contract descriptor.
Only a successful build sets its final fingerprint.

Forward handlers and compensators receive `ActivityContext`.
It has named metadata and cancellation accessors, and no public fields.
The compensation phase has its own instance identity and idempotency key.
`ActivityError` carries an `ErrorCode`: 1..=64 bytes of `[a-z0-9_]`.
Invalid code text becomes `invalid_error_code`, so error details never become public codes.
`ActivityResultExt` maps provider errors with `or_retryable` and `or_permanent`.
Error formatting contains only the allowed error code.
Public authoring Debug output excludes handlers, payloads, and credentials.
The `test-util` feature adds `testing::TestRuntime` for in-memory workflow unit tests.
It applies the runtime's stage materialization, fan-out and instance limits, panic and timeout handling, and saga order.
Its checkpoint reads reject references of other or unbuilt workflows, and its `Debug` output omits payloads.

`get` and `inspect` select only the stored progress projection in one scoped query.
`list` uses one count query and one paginated projection query.
These reads do not hydrate journal, input, result, or history payloads.
Projection and event cursor belong to the same committed transition.
Event sequence numbers increase per run without fixed revision blocks.
Results and history use separate authorized reads.
Recorded timestamps and checkpoint origin epochs stay exact.

`register_contract` supports hosts without handlers.
Registration of an existing contract does not reactivate a definition.
During `Stopping`, exact saga bindings can be restored for compensation.
Activation stays prohibited until release.
After `Released`, binding prepares a new generation and `activate` opens it.
There is no user-facing REST or gRPC binding.

#### Errors

Handle operations return the library [`Error`](../src/error.rs).
`Error::kind` returns an `ErrorKind` for recovery.
The kinds are `InvalidArgument`, `NotFound`, `AlreadyExists`, `PermissionDenied`, `FailedPrecondition`, `Aborted`, `Unavailable`, and `Internal`.
`ErrorKind` is non-exhaustive.
`Error::code` returns a stable string, for example `RUN_NOT_FOUND` or `DEFINITION_MISMATCH`.
The [code list](../src/error/codes.rs) only grows.
`is_retryable` is true for `Aborted` and `Unavailable`.
`field`, `run_id`, `definition`, and `request_key` identify the failing input when one applies.
Display text is for humans and can change.

`Error::source` keeps the underlying storage, authorization or decoding failure; equality ignores it.
`Error::resource` tells whether a run or a definition is concerned.
The library does not depend on the platform error model.
The embedding gear maps `ErrorKind` and `resource` to its own canonical errors and resource types, and can pass `code` as the reason.
Scheduling on an engine without delivery returns `FailedPrecondition` with code `DELIVERY_DISABLED`; a `Dispatcher` or `Worker` engine returns retryable `Unavailable` until `Engine::prepare` starts delivery.
Internal errors are logged where they are created.
`EngineBuilder::build` returns `ConfigError` with `setting` and `reason`.
`Engine::prepare` and `Runner::run` return `LifecycleError`.
Its kinds are `IncompleteBindings`, `QueueUnavailable`, `Storage`, `ShutdownTimedOut`, and `Worker`.

### 3.4 Internal Dependencies

The embedding gear implements the [`Authorizer`](../src/authorization.rs) port.
`caller_scope` returns the scope for a `CallerAction` of a `SecurityContext`.
`service_scope` returns the scope for a `ServiceAction` under the gear's own service identity.
Each action has a stable PDP name from `as_str`, a `resource`, and a `ConstraintPolicy`.
Run actions use resource `durable_execution.run`.
Catalog actions use resource `durable_execution.definition`.
Caller actions and service run actions require row constraints.
The engine rejects an unconstrained scope for them as denied, as `PolicyEnforcer` does with `require_constraints`.
Catalog actions and `cancel_definition` accept an unconstrained scope.
Rejections are logged with action, resource, reason and, for callers, subject and tenant ids, never with authorizer error text.
A typical implementation uses `PolicyEnforcer` and a cached service identity.
The library does not exchange client credentials or read secrets.
SecureORM applies the returned scopes to ToolKit DB operations.

The engine keeps the checks that do not depend on policy.
It rejects callers with a nil subject or tenant.
It narrows owner operations to the calling subject and hides runs of other owners.
A control command first uses `update` on a run that the caller owns.
If `update` is denied or the run has another owner, it uses a `manage` grant and its row constraints.
Each `Authorizer` call has the limit `EngineBuilder::authorization_timeout`.
A timeout counts as unavailable authorization and returns `Unavailable`.
Logs contain only the failure kind, action, and resource.
They do not contain policy text or authorization data.

The embedding gear supplies the database, migration host, settings, and lifecycle control.

Worker identity does not replace the original execution owner.
Compensators must authorize access to consumer resources.
Cleanup under `Stopping` applies only to the same registration generation.
An unconstrained `cancel_definition` grant is necessary for `CancelAndRelease` in the shared catalog.

### 3.5 External Dependencies

The journal uses the embedding gear's `Db`.
The adapter connects with the `QueueConfig` DSN through a separate pool.
Deployments normally point that DSN at the journal database.
Named queues share the adapter pool.
The adapter tables live in the shared `apalis` schema.
Queue names must therefore be unique across all engines that use that database.
The current adapter pool has a maximum of four connections.
Queue delivery uses one transport attempt.
The journal controls business retries and redelivery.
Adapter due times round upward to whole seconds.
Adapter heartbeat and orphan recovery do not establish journal ownership.

### 3.6 Interactions & Sequences

#### Submit and execute

**ID**: `cpt-cf-durable-execution-seq-submit`

**Use case**: `cpt-cf-durable-execution-usecase-background-work`

```mermaid
sequenceDiagram
    participant C as Typed client
    participant S as Service and JournalStore
    participant D as PostgreSQL
    participant O as Outbox
    participant A as Apalis and Executor
    C->>S: Start with input and expected fingerprint
    S->>S: Authorize and check admission
    S->>D: Commit journal, expansion, events, progress, and intents
    S-->>C: StartedRun with typed handle
    O->>A: Enqueue instance identity and generation
    O->>D: Record delivery and acknowledge command
    A->>D: Authorize and acquire claim through CAS
    A->>A: Call local handler outside transaction
    A->>D: Commit fenced result, next stage, progress, and intents
```

Failure before enqueue leaves the Outbox command pending.
Failure after enqueue can cause duplicate delivery.
Generation and claim checks reject obsolete work.
An external effect before checkpoint commit can repeat.
The provider must support idempotency or reconciliation.

#### Recover a lost worker

**ID**: `cpt-cf-durable-execution-seq-recovery`

**Use case**: `cpt-cf-durable-execution-usecase-worker-recovery`

Recovery scans expired claims under current authorization.
It checks the registration generation and persisted phase.
It keeps expansions and confirmed forward and compensation checkpoints.
An exhausted forward retry budget starts saga compensation without an extra forward call.
Missing handlers defer execution without extra attempts.
A contract mismatch blocks execution.

#### Revoke and release

**ID**: `cpt-cf-durable-execution-seq-release`

**Use case**: `cpt-cf-durable-execution-usecase-retire-definition`

Unregister checks revision and cancellation permission under an exclusive catalog lock.
`Retain` closes admission, but existing work can continue.
`CancelAndRelease` records `Stopping` and revokes forward execution for that generation.
Reconciliation requests Abort for sagas, including paused and failed runs.
Other runs follow cancellation and claim cleanup.
Saga bindings stay available until compensation completes.
`CompensationFailed` keeps the definition in `Stopping`.
Authorized Retry can continue that compensation.

```mermaid
stateDiagram-v2
    Active --> Retired: Retain
    Retired --> Active: Activate same generation
    Active --> Stopping: CancelAndRelease
    Retired --> Stopping: CancelAndRelease
    Stopping --> Stopping: Cleanup pending or compensation failed
    Stopping --> Released: Runs and claims settled
    Released --> Active: Bind and activate new generation
```

Hosts release local bindings after authorized observation of release.
Restart restores matching saga bindings while `Stopping`.
Metadata-only hosts also reconcile definitions.
A remote process cannot free another host's local `Arc` objects.

### 3.7 Database schemas & tables

`toolkit_durable_execution::migrations()` returns the schema migrations.
The embedding gear appends them to its own `DatabaseCapability::migrations`.
The host records them in the embedding gear's migration history.
One squashed, forward-only [journal migration](../src/store/migrations/init.rs), `m0001_durable_execution__init`, defines all journal columns, constraints, and indexes.
`migrations()` returns it together with the ToolKit outbox migrations for the `durable_execution__delivery_outbox` prefix as separate entries.
Each is recorded under its own name in the embedding gear's history, so later journal or outbox migrations reach existing databases.
Table names follow the [object namespacing ADR](../../../docs/arch/database/ADR/0001-cpt-cf-database-adr-object-namespacing.md).

| Table | Contents |
|---|---|
| `durable_execution__runs` | Owner, definition, fingerprint, revisions, journal header, versioned progress, status filter, and scheduling fields. |
| `durable_execution__activities` | Instance identity, template, item index, phase, stage, checkpoint state, fence, lease, and due time. |
| `durable_execution__attempts` | Per-instance attempt records. Completed records stay immutable. |
| `durable_execution__epochs` | Archived execution state, including compensation cause and instance identity. |
| `durable_execution__events` | Per-run event sequence and event records. |
| `durable_execution__start_keys` | Scoped request-key hash, input hash, and original run reference. |
| `durable_execution__coalescing` | Scoped slot, revision, active run, and successor run. |
| `durable_execution__delivery_intents` | Run delivery-generation intent and delivery acknowledgment. |
| `durable_execution__definitions` | Shared contract, registration revision, generation, and lifecycle state. |
| `durable_execution__delivery_outbox*` | ToolKit publication commands and leased delivery records. |
| `apalis.*` | Official queue adapter tables. |

The prefix is fixed.
One database or schema therefore hosts one engine.
A caller-supplied prefix is a planned follow-up in [ADR 0005](adrs/0005-embeddable-library.md).

Activity constraints distinguish template, index, and phase.
Claim indexes support stage scheduling.
Normalized records store invocation inputs, receipts, attempts, and epoch history outside control metadata.
The run row stores a private, versioned progress projection.
Correct progress stays readable when separate payload or history data is corrupt.
Payload corruption still fails reads that use those records.
The format has no compatibility reader for older stored records.

### 3.8 Deployment Topology

Each process of the embedding gear selects an `ExecutionMode`.
A `ControlPlane` process observes runs, cancels runs, and reconciles the catalog.
A `Dispatcher` process can register contracts and submit work without handler bindings.
A `Worker` process restores local handlers and compensators and executes activities.
All processes of one engine share contracts, catalog, journal, and queue names.
Queue concurrency applies per process pool.
Fan-out stage concurrency applies across persisted claims on all processes.
Two gears that embed engines need separate databases or schemas and unique queue names.
See [README execution modes](../README.md#execution-modes) and [README authorization](../README.md#authorization).
The [engine source](../src/engine.rs) defines all settings, defaults, and validation.

## 4. Additional Context

Compensation is not a distributed transaction.
Only declared compensators and confirmed outputs participate in compensation.
Providers must reconcile an external effect that occurred before a lost checkpoint.
Keep credentials in local provider clients.
Store only necessary business input and receipts.

`testing::TestRuntime` runs workflows in memory behind the `test-util` feature.
The PostgreSQL suites run behind the `integration` feature with `make test-durable-execution-pg`.
Compile-time fixtures are in [tests/ui](../tests/ui/).
See the [examples](../examples/README.md), [Makefile](../../../Makefile), and [CI workflow](../../../.github/workflows/ci.yml).

## 5. Traceability

- [PRD](PRD.md): requirements, use cases, and acceptance evidence.
- [ADR 0001](adrs/0001-apalis-with-transactional-outbox.md): publication boundary.
- [ADR 0002](adrs/0002-task-execution-framework-selection.md): transport choice.
- [ADR 0003](adrs/0003-typed-sdk-and-journal-boundary.md): public and persisted models.
- [ADR 0004](adrs/0004-saga-fanout-and-progress.md): stage runtime and storage format.
- [ADR 0005](adrs/0005-embeddable-library.md): embeddable library and authorization port.
- [Engine](../src/engine.rs): settings, execution modes, and lifecycle.
- [Authorization port](../src/authorization.rs): actions, resources, and engine checks.
- [Errors](../src/error.rs): error kinds and codes.
- [Executor](../src/engine/executor.rs): claims and handlers.
- [Storage](../src/store/): transactions and queries.
