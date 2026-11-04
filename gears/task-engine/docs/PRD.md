---
description: "Product requirements for the Task Engine gear: durable, multi-tenant task lifecycle with queues, leases, retries, waits, dependencies, and lifecycle events."
---

# PRD — Task Engine

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
  - [3.1 Gear-Specific Environment Constraints](#31-gear-specific-environment-constraints)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
  - [4.3 Future Scope](#43-future-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 Task — Core Work Unit](#51-task--core-work-unit)
  - [5.2 Queue + Concurrency Control](#52-queue--concurrency-control)
  - [5.3 Scheduling + Timeouts](#53-scheduling--timeouts)
  - [5.4 Wait Conditions / Durable Suspension](#54-wait-conditions--durable-suspension)
  - [5.5 Task Entries](#55-task-entries)
  - [5.6 Assignment + Atomic Claim + Lease](#56-assignment--atomic-claim--lease)
  - [5.7 Progress Tracking](#57-progress-tracking)
  - [5.8 Retry / Failure Handling](#58-retry--failure-handling)
  - [5.9 Dependencies / Parent-Child Execution](#59-dependencies--parent-child-execution)
  - [5.10 Event History / Audit Trail](#510-event-history--audit-trail)
  - [5.11 Authentication and Authorization](#511-authentication-and-authorization)
  - [5.12 GTS Extension Points](#512-gts-extension-points)
  - [5.13 REST API](#513-rest-api)
  - [5.14 Data Retention and Cleanup](#514-data-retention-and-cleanup)
  - [5.15 Observability](#515-observability)
  - [5.16 SDK Crate](#516-sdk-crate)
  - [5.17 Search and Querying](#517-search-and-querying)
  - [5.18 Bulk Operations](#518-bulk-operations)
  - [5.19 Task Domain Routing](#519-task-domain-routing)
  - [5.20 Caching Layer](#520-caching-layer)
  - [5.21 Rate Limiting](#521-rate-limiting)
  - [5.22 Recurring / Cron Tasks](#522-recurring--cron-tasks)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Gear-Specific NFRs](#61-gear-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
  - [Create, Claim, and Execute a Task](#create-claim-and-execute-a-task)
  - [Task Waits for External Event](#task-waits-for-external-event)
  - [Retry with Backoff After Transient Failure](#retry-with-backoff-after-transient-failure)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

**Task Engine** is a CF/Gears gear that orchestrates units of work ("tasks") for many tenants across a cluster. API clients create tasks; workers claim them, run them, and report results. The engine owns the full task lifecycle: queues with priorities and concurrency limits, safe claiming with leases, timeouts, retries, durable waits, dependencies, progress, and an audit trail. Task types are extensible through the Global Type System (GTS).

The engine is a **control plane only**: it runs no task code and hosts no workers. Running a gear's own activities and workflows in-process, on a local worker, is the job of the [Durable Execution](../../../libs/toolkit-durable-execution/docs/PRD.md) gear.

This PRD states **what** the engine must do and **why**. **How** it is built is in [DESIGN.md](./DESIGN.md) and the [ADRs](./ADR/).

### 1.2 Background / Problem Statement

Teams that run background work across services and tenants keep hitting the same problems:

1. **No shared task lifecycle** — each team builds its own job queue, cron jobs, and status tables. States, timeouts, retries, and progress work differently everywhere.
2. **Weak tenant isolation** — most task frameworks have one namespace. A multi-tenant platform needs tasks that other tenants cannot see, queue access per role and tenant, and per-tenant limits and retention.
3. **No durable waiting** — tasks often wait for a time, an external event, or human input. Without built-in waits, teams write polling loops and fragile callbacks.
4. **Rigid task typing** — fixed enums and schemas force coordinated releases for every new task type.
5. **Stalled work** — without central timeout handling, tasks can hang forever.
6. **Fragile retries** — failures either lose the task or retry without limit and flood downstream systems.

### 1.3 Goals (Business Outcomes)

- One reusable task orchestration control plane for the whole CF/Gears platform, replacing per-gear job queues
- Strong tenant isolation with per-tenant limits, retention, and access policies
- Durable waits (timer, event, input, children) so tasks can pause without polling
- New task types, schemas, and entry types without engine releases or database migrations
- Reliable execution: retries with backoff, dead-letter handling, clear failure classification
- Fair, priority-based dispatch with concurrency limits and safe, lease-based claiming
- Parent/child tasks and dependencies for multi-step work
- A REST API and a Rust SDK for every lifecycle and admin operation
- A complete, immutable audit trail for every task

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Task | The unit of work: type, state, urgency, priority, input/output, metadata, owner, and lifecycle |
| Urgency | `normal` (default) or `critical`. `critical` runs before other tenants' work and needs the `expedite` action (§5.2) |
| Priority | 0 (highest) to 1000 (lowest). Orders only one tenant's own tasks in a queue (§5.2) |
| Task Entry | An immutable record attached to a task during execution: work done, an observation, an issue, a note, or a measurement |
| Queue | A named channel, registered as a GTS instance (`gts.cf.core.tasks.queue.v1~`), with concurrency limits, default priority, timeouts, retry policy, and an optional dead-letter queue. Tasks are put into queues; workers claim from queues |
| Worker | A process that claims tasks, runs them, reports progress, and completes them with a result |
| Worker Identity | The pair (authenticated subject, `worker_id`) that names one worker process (§5.6 Worker identity) |
| Wait Condition | A durable condition a task waits for before it continues. Four kinds: timer, event, input, children |
| Assignment | The link between a task and the worker that claimed it: worker identity, claim time, lease, heartbeat, attempt |
| Lease | A time-limited hold on a claimed task. If the worker stops renewing it, the task goes back to the queue |
| Lease Token | A random value issued with each lease. Every worker write must carry it and come from the lease holder; otherwise it is rejected (§5.6 Lease Fencing) |
| Lease Epoch | A per-task counter that grows with every new lease. Workers pass it to external systems to reject writes from old leases |
| Heartbeat | A periodic worker call that renews the lease and returns control signals such as a cancel request |
| Worker Session | The liveness record of one worker process in one task domain. A heartbeat renews the session, and so every lease it holds (§5.6) |
| Checkpoint | Saved intermediate state that lets a task resume after an interruption |
| Dependency | A predecessor/successor link: a task stays blocked until its predecessors finish successfully |
| Retry Policy | Per task, or inherited from the task type or queue (§5.12 Type defaults resolution): two budgets — `max_retries` for reported failures and `max_liveness_requeues` for lost workers — plus backoff, jitter, and error codes that are never retried |
| Dead Letter Queue | A parking place for tasks that ended in failure, kept for inspection and re-enqueue |
| Task Type | A GTS type that sets input/output schemas and optional defaults, limits, and dedup behavior |
| Concurrency Limit | A cap on how many tasks may run (`claimed` or `running`) at once, per queue or task type, globally or per tenant |
| Queue Depth | The number of `queued` tasks waiting for a worker. `scheduled`, `blocked`, and `waiting` tasks are **deferred** and are counted separately |
| Dedup Identity | A task identity the engine computes from input fields declared by the task type, used instead of a caller key (§5.1 Derived Dedup Identity) |
| Supersession | Cancelling an in-flight task because a newer submission of the same work (higher order value) was accepted (§5.1 Task Supersession) |
| Task Domain | A configured database that holds the tasks of matching queues. Each task belongs to exactly one domain, the domain of its queue (§5.19) |
| Cluster Gear | The platform coordination gear ([Cluster PRD](../../system/cluster/docs/PRD.md)). The engine uses only its distributed cache, only for an optional ready-task cache. Task correctness never depends on it |

## 2. Actors

### 2.1 Human Actors

#### Platform Administrator

**ID**: `cpt-cf-task-engine-actor-platform-admin`

- **Role**: Operates the Task Engine: configures queues and limits, sets retention, watches health, inspects and re-enqueues dead-lettered tasks.
- **Needs**: Admin APIs for queues, runtime configuration, DLQ handling, and monitoring.

#### API Consumer (Developer)

**ID**: `cpt-cf-task-engine-actor-api-consumer`

- **Role**: Creates tasks, follows their progress, provides input to waiting tasks, and reads results, entries, and history.
- **Needs**: Task create/read/search, entry read/list, input resolution, progress and result access.

### 2.2 System Actors

#### Worker (Task Consumer)

**ID**: `cpt-cf-task-engine-actor-worker`

- **Role**: Claims tasks, runs them, renews leases, reports progress, saves checkpoints, writes entries, waits when needed, and completes tasks with a result.
- **Needs**: Claim, heartbeat, progress, checkpoint, complete, release, wait, and entry APIs; a lease token on every call; fast notice when a lease is lost. Workers run idempotently (§5.6 Execution Semantics).

#### Event Broker

**ID**: `cpt-cf-task-engine-actor-event-broker`

- **Role**: The platform Event Broker gear. It receives task lifecycle events for other gears, and it delivers external events that can satisfy event wait conditions.
- **Needs**: Structured lifecycle events with task metadata, result codes, and resource references.

#### Storage Backend

**ID**: `cpt-cf-task-engine-actor-storage-backend`

- **Role**: One or more relational databases (PostgreSQL, MariaDB/MySQL, or SQLite) that hold all task data. Task domains can split task classes across databases.
- **Needs**: Transactions, tenant-scoped queries, schema migrations, and per-domain routing.

## 3. Operational Concept & Environment

> **Note**: Runtime, OS, architecture, lifecycle policy, and gear integration patterns are defined in this repository's foundational documents — the [architecture manifest](../../../docs/ARCHITECTURE_MANIFEST.md) and [guidelines/](../../../guidelines/). This section captures only this gear's deviations.

### 3.1 Gear-Specific Environment Constraints

- Runs on the ToolKit database layer and supports three backends: PostgreSQL (recommended for production), MariaDB/MySQL, and SQLite (development, tests, and small single-node use)
- Can use several databases at once through task domains (§5.19)
- Can use the Cluster gear's distributed cache for an optional ready-task cache; works fully without it
- Needs no message broker or external scheduler to run tasks

## 4. Scope

### 4.1 In Scope

**Definition**:
- Task types as GTS types with input/output schemas and optional defaults
- Four built-in wait condition kinds (timer, event, input, children)
- Queues and task entry types as GTS entities

**Execution**:
- Full lifecycle: create, queue, claim, start, heartbeat, progress, checkpoint, release, complete, cancel
- Priority-based dispatch with tenant fairness and concurrency limits
- Lease-based claiming with fencing of stale workers
- Retries with backoff and dead-letter handling
- Timeouts: queue, assignment, execution, heartbeat, lifetime, and SLA deadlines
- Durable waits, parent/child tasks, and dependencies
- Cancellation (requested, graceful, forced)
- Duplicate-safe creation (caller key or type-declared identity) and supersession
- Queue depth limits at admission
- Delayed start (`run_after`)

**Tracking**:
- Task entries, progress with optional progress events, resource references
- Structured results with error classification
- Immutable event history; lifecycle events published to the Event Broker
- Transparent compression of stored payloads

**Operations**:
- REST API and Rust SDK
- Tenant-scoped, policy-based access control through the platform authorization stack
- Search, bulk operations, runtime configuration, retention, metrics, and health probes
- Tags for filtering
- Task domains (several databases per deployment)

### 4.2 Out of Scope

- **Executing task code** — the engine never runs task handlers or hosts workers; workers pull work through the API (§5.6). In-gear execution on a local worker belongs to the Durable Execution gear (§1.1)
- **Workflow orchestration** — the engine supports dependencies, parent/child links, and waits, but it does not run user-defined control flow (branches, loops, sagas, arbitrary DAGs). Control flow belongs to the worker or caller
- **UI or dashboard** — consumers build them on the REST API
- **Message broker replacement** — the engine manages tasks, not pub/sub messaging
- **File or blob storage** — tasks carry JSON up to 64 KB; larger artifacts live in external storage (e.g., the File Storage gear) and are referenced by URL or ID
- **Comments and attachments** — use a collaboration gear or external storage referenced from task entries
- **Notifications** — use the Event Broker for routing
- **Own cluster mesh** — use the Cluster gear when cross-instance coordination is needed
- **Slow-query tracing** — belongs to a platform-wide observability component
- **Search inside payloads** — search covers standard task fields only (state, queue, type, tags, resource references, priority, assignee, age, dependencies)

### 4.3 Future Scope

The following may be considered later and are not part of this PRD:

- Per-tenant database sharding
- Cross-tenant administrative queries, analytics, and status views
- An approval wait kind — **not planned**. Approval gates use the `approval-service` gear: the task sets an **event** wait for its approval event (§5.4). The engine **MUST NOT** re-implement approval lifecycle, decisions, or approver policies. The **input** wait stays available for simple structured decisions
- An optional analytics, archive, and search layer (for example ClickHouse or OpenSearch) fed by lifecycle events ([ADR-0001](ADR/0001-pluggable-execution-backends.md)). If added, it **MUST NOT** hold the true task state, take part in claims, leases, or limits, or stop task execution when it is down
- Recurring / cron tasks (§5.22)
- Other client APIs, such as gRPC ([ADR-0013](ADR/0013-worker-transport.md))

## 5. Functional Requirements

> **Testing strategy**: All requirements are verified by automated tests (unit, integration, e2e). Coverage targets are in §6.1 Code Coverage.

### 5.1 Task — Core Work Unit

#### Task Creation

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-task-create`

The system **MUST** accept a task definition with: task type (GTS identifier), optional queue (§5.12 Type defaults resolution), optional urgency (§5.2), priority, input (JSON), optional context (JSON), optional owner, optional timeouts, optional SLA deadline, optional retry policy, optional `run_after`, a cancellable flag, tags, resource references, an optional parent task, optional `wait_for_children` / `propagate_cancel` flags, and optional predecessors (§5.9).

On creation the system **MUST**:

- assign a unique ID and record the creating subject;
- reject an unknown task type (`UNKNOWN_TASK_TYPE`) before anything is stored, and fail closed (`503`) if the GTS registry cannot be reached and the type is not known yet;
- validate the input against the type's input schema, when one is registered;
- authorize the parent and predecessors (§5.9 Link Authorization);
- check that parent and predecessors are in the same task domain (§5.19) and that dependencies form no cycle (§5.9);
- store the task, arm its timers, and record the `taskCreated` event as one atomic step, so a crash never leaves a task without its timers or its event;
- place the task in `blocked` if it has unmet predecessors, else in `scheduled` if `run_after` is in the future, else in `queued`. A blocked task with a future `run_after` goes to `scheduled` when it is unblocked.

**Payload size**: the combined size of a task's `input`, `context`, and `output` **MUST NOT** exceed a configurable limit (default 64 KB), measured on the uncompressed JSON. The limit applies to every write that can change the payload — create, checkpoint, set-wait with checkpoint, release with checkpoint, and completion. A write over the limit is rejected (`413`) and changes nothing. Two concurrent payload writes on one task **MUST NOT** together exceed the limit or overwrite each other; the losing write is rejected (`409`, `CONCURRENT_MODIFICATION`) and may be retried.

- **Rationale**: Creation is the entry point for all work. Early validation and atomic creation keep tasks consistent from the start; the size limit keeps the engine from becoming a blob store.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`, `cpt-cf-task-engine-actor-worker`

#### Structured Input/Output

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-structured-io`

Every task **MUST** carry a structured input (set at creation) and a structured result (set at completion). The result **MUST** contain a result code (§5.12), an output (JSON), an optional error, and optional warnings. An error has a domain, a code, a message, optional context, a `retryable` flag (§5.8), and a `fault`: `task` (default — bad input or a business error), `dependency` (a downstream service failed), or `worker` (the worker's own infrastructure failed). `fault` affects only worker blacklisting (§5.6). Input and output **MUST** be validated against the task type's schemas when they are registered.

- **Rationale**: Structured results allow automation, reporting, and correct failure handling; without them a task is just "status plus comments".
- **Actors**: `cpt-cf-task-engine-actor-worker`, `cpt-cf-task-engine-actor-api-consumer`

#### Idempotent Task Creation

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-idempotent-create`

A create request for a task type without a derived dedup identity (below) **MUST** carry an idempotency key (`Idempotency-Key` header). A request without a key is rejected (`428`). The key is unique per **tenant and creating subject** ([ADR-0012](ADR/0012-task-deduplication.md)):

- **Subject-scoped keys**: two subjects using the same key get two independent tasks. One subject can never see, collide with, or block another subject's task through a key.
- **Replay**: if a task with the same key already exists — in any state, terminal or not — no new task is created and the request returns `409 Conflict` (`already_exists`) with the existing task's ID. The caller treats this as success and reads the task.
- **Different request, same key**: if the request differs in type, queue, input, or context from the request the key was **first accepted with**, it is rejected (`400`, `invalid_argument` on the key). The comparison is with the original request, never with the task's current state, which checkpoints, moves, and dead-lettering change; so a retry sent after the task has progressed is still a replay.
- **Window**: a key is held while its task is unfinished, and in any case for `idempotency_key_window_days` after creation (default 7, configurable up to the task's retention, §5.14). After that, the same key creates a new task. A caller that may retry later than the window **MUST** look for the task first (for example by tag or resource reference). A short window keeps the key store small; a key held for the whole retention would grow with every stored task.

**Deliberate differences from common practice**: Stripe-style APIs and the IETF `Idempotency-Key` draft make the key optional, answer a replay with the original `201`, and answer a reused key with `422`. This engine differs on purpose, and clients **MUST NOT** assume the common behavior:

- The key is **required**, because a duplicate task means duplicate work, and forgotten keys are the usual cause.
- A replay returns **`409`** with the task ID, because a task changes after creation and a stored `201` body would be stale. The SDK hides this and returns the existing task.
- A reused key with a different request returns **`400`**, the default status of the platform's `invalid_argument` category.

- **Rationale**: Retries by APIs, events, and integrations must never create duplicate work. Scoping keys to the subject stops one producer in a tenant from squatting on, blocking, or discovering another producer's tasks.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`

#### Derived Dedup Identity

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-derived-dedup`

A caller key answers "is this a retry by the same caller?". Some work needs "is this the same work?", where independent producers must land on one task. A task type **MAY** declare a `dedup` policy (a GTS trait, §5.12) so the **engine** computes the identity:

- `key_fields` — input fields (JSON Pointers) whose values form the identity. Each **MUST** be a required field of the input schema; at most 16 fields, each value at most 4 KiB.
- `scope` — who may collide: `subject` (default, the creating subject), `authz_scope` (callers with an identical compiled access scope; if the scope cannot be put into a canonical form, the request **MUST** fail closed and never fall back to a weaker scope), or `tenant` (all callers of the tenant; only for types whose output does not depend on the caller, and never a default).
- `window` — how long the identity is held: `active` (default; released when the task ends in any way, so a failed task can be resubmitted) or `active_or_succeeded` (also held after a successful completion, until retention, so a good result is reused).

The identity always includes the task type (with its version), the tenant, the scope, and the key field values. It is computed from validated input, after authorization.

For a type with `dedup`:

- No idempotency key is needed. Platform SDKs and gateways may still send one, so a key that is sent anyway is ignored — not rejected, not stored, and not checked. Clients **MUST NOT** rely on it for these types.
- A duplicate returns `409 Conflict` with the existing task ID, as for caller keys. Fields outside `key_fields` are not compared; the first accepted request wins.
- The engine **MUST** validate the policy against the input schema when it first loads the type, and again when the type changes. A type with an invalid policy cannot be created (fail closed).

**Security**: identities never cross tenants, and different scopes never share an identity. When a policy change alters a caller's access scope, the identity changes too, so duplicates may briefly exist across the change — a missed deduplication, never a disclosure. A duplicate reveals the existing task's ID only if the caller may read it; otherwise the request fails with `403` and no ID. Under `tenant` scope, the registrant accepts that callers in the same tenant learn that an identical task exists. The identity is not secret and is never an authorization token.

- **Rationale**: Systems that submit equal work from several producers need one task per piece of work. Putting the access scope into the identity keeps deduplication from crossing authorization boundaries. Releasing the identity on failure avoids a "failed forever" trap.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`

#### Task Supersession

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-task-supersede`

When a task's input changes over time (for example a new graph revision), the running task becomes useless. A type with `dedup` **MAY** declare a `supersede` policy:

- `order_field` — a required, non-negative integer in the input (for example a revision number). It is not part of `key_fields`. Tasks with the same identity apart from this field form a **supersede group**.
- `mode` — how an outdated task is stopped: `graceful` (default; cancel request plus grace period, §5.1 Task Cancellation) or `forced`.

On creation, the new order value is compared with the latest value seen for the group, atomically with the insert:

1. **Higher** — the new task is created. The group's unfinished task, if any, is cancelled with result `superseded` and records the new task's ID; it ends as `superseded` even if a `graceful`-mode worker reports `cancelled`. Its capacity is released, cancellation reaches its children, and `taskCancelled` carries `superseded_by`. If that task finished at the same moment, there is nothing to replace and creation goes ahead.
2. **Equal** — normal deduplication: a replay while the identity is held, otherwise a new task.
3. **Lower** — the request is stale and is rejected (`400`, `STALE_ORDER`), even if the newer task already finished. An old revision can never replace a newer outcome. The group's latest order value is kept until the retention of its latest task ends.

Rules:

- A supersede-enabled type **MUST NOT** accept `cancellable=false` tasks.
- The caller **MUST** be allowed to `create` and also to `cancel` the task being replaced; otherwise the whole request is rejected (`403`) and nothing changes. Because the group shares the dedup `scope`, one subject can replace another subject's task only for types that declare `authz_scope` or `tenant`.
- A replaced task that holds a lease follows the normal lease rules: `forced` ends the lease at once; `graceful` tells the worker through its heartbeat. In `graceful` mode the old and new task may overlap for a short time; work that needs exclusivity **MUST** use `forced`.
- The replaced task's history records who replaced it and with which task.

- **Rationale**: One rule — newer wins, equal is a replay, older is stale — replaces outdated work without callers having to cancel and recreate it, and without a slow old request ever replacing a newer one.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`

#### Task Cancellation

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-task-cancel`

The system **MUST** support three cancellation modes:

- **requested** — the worker sees the request on its next heartbeat and ends the task as `cancelled`;
- **graceful** — like requested, but if the worker does not finish within a configurable grace period, the engine cancels the task by force;
- **forced** — the engine ends the task at once with result `forcibly_cancelled`, without waiting for the worker.

A task that holds no lease (not `claimed` or `running`) has no worker to ask, so `requested` and `graceful` cancel it at once with result `cancelled`. Cancellation reaches child tasks unless the parent opted out (§5.9). `taskCancelRequested` and `taskCancelled` events are published.

**`cancellable=false`**: `requested` and `graceful` **MUST** be rejected for such a task. `forced` **MUST** still work, because it is the emergency stop.

- **Rationale**: Operators need both cooperative shutdown and a guaranteed emergency stop. Letting only `forced` override the flag protects tasks from routine cancellation.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`, `cpt-cf-task-engine-actor-platform-admin`

#### Tags and Classification

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-tags`

The system **MUST** support key-value tags on tasks. Keys (up to 128 characters) and values (up to 256 characters) are free-form strings; there is no tag registry. Tags **MUST** be filterable in search and changeable after creation. Tags belong to the tenant: they are visible only within the caller's tenant scope (§5.11), and one tenant's tag keys are never visible to another. Common keys (such as `project` or `region`) are recommendations only.

- **Rationale**: Tags are cheap, tenant-local labels. A global, registered key space would add a privileged step and leak key names across tenants.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`

#### Resource References

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-resource-refs`

A task **MAY** carry a bounded list of resource references, each naming a platform resource by type (GTS identifier) and ID. References **MUST** be filterable in search (§5.17), included in task-scoped lifecycle events (§5.10), and tenant-scoped like all task data. The maximum number per task is configurable.

**Verified or not**: a reference is a claim made by the creator, not a proof of access. Each reference carries a `verified` flag. It is `true` only when a creator-authority check of the task type (§5.11 Creator authority) confirmed, at creation, that the creator may act on that resource; otherwise it is `false`. Search, events, and the claim response show the flag. Consumers and workers **MUST NOT** treat an unverified reference as proof that the creator may access the resource.

- **Rationale**: Consumers need "all tasks touching this resource" views. Payload search is out of scope, so a standard field is the only way to offer them. An explicit flag keeps a creator from using a reference to point privileged workers or downstream consumers at a resource the creator cannot reach.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`, `cpt-cf-task-engine-actor-event-broker`

#### Task Checkpoint

- [ ] `p3` - **ID**: `cpt-cf-task-engine-fr-task-checkpoint`

A worker **MUST** be able to save checkpoint data (JSON). It is merged into the task's context key by key. The merged payload is checked against the payload size limit (§5.1 Task Creation); a checkpoint over the limit is rejected (`413`) and changes nothing. Each checkpoint publishes a `taskCheckpointed` event.

- **Rationale**: Long tasks can resume from their last checkpoint instead of starting over.
- **Actors**: `cpt-cf-task-engine-actor-worker`

### 5.2 Queue + Concurrency Control

#### Priority-Ordered Dispatch

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-priority-dispatch`

The system **MUST** dispatch the tasks of a queue in this order:

1. **Urgency** — `critical` before `normal` (default). Setting `critical`, at creation or later (§5.18), needs the `expedite` action (§5.11); otherwise `403`.
2. **Tenant fairness** — the queue's fairness mode picks the next tenant (below).
3. **Priority** — 0 (highest) to 1000 (lowest), out of range `400`. It orders only the chosen tenant's tasks, so it needs no extra permission.
4. **Age, then task ID** — FIFO.

- **Rationale**: If priority ranked tenants, one tenant could send everything at `0` and starve the rest. Only urgency ranks tenants, and only trusted callers can set it.
- **Actors**: `cpt-cf-task-engine-actor-worker`, `cpt-cf-task-engine-actor-api-consumer`

#### Tenant Fairness

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-tenant-fairness`

Many tenants share one queue, so plain FIFO lets one tenant's burst delay everyone else. Each queue **MUST** have a tenant fairness mode, set in its registration (§5.12) and overridable in config:

- `round_robin` (default) — tenants take turns; each turn takes that tenant's best task (highest priority, then oldest). Turns may be tracked per node, so across nodes the rotation is approximate, but no active tenant is skipped for more than one cycle per node.
- `fifo` — all tenants' tasks in one order by priority, then age. Priority then ranks tenants, so use it only for single-tenant or trusted queues.

Fairness only orders eligible tasks. Per-tenant concurrency limits (below) decide how many tasks a tenant may run.

- **Rationale**: A shared queue needs a fair share per tenant, or one noisy tenant starves the rest (compare per-tenant fair queuing in Hatchet and Oban Pro).
- **Actors**: `cpt-cf-task-engine-actor-worker`, `cpt-cf-task-engine-actor-platform-admin`

#### Queue Concurrency Limits

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-queue-concurrency`

Each queue **MUST** declare, in its registration (§5.12):

- `global_concurrency_limit` — the most tasks from this queue that may run at once across all tenants;
- `per_tenant_concurrency_limit` — the most tasks from this queue that may run at once for one tenant.

A task counts against these limits while it is `claimed` or `running`. The system **MUST** assign a task only if every applicable limit (queue and task type, global and per tenant) has room, and **MUST** never exceed a limit, even under many concurrent claims. A full limit blocks only the tasks it covers: the claim moves on to other tenants and types (no head-of-line blocking).

A claim that assigns nothing returns `429` (`CONCURRENCY_LIMIT`, with `Retry-After`) only when an eligible task was skipped because a limit was full; when nothing was eligible it returns an empty result (`204`). A long-poll claim keeps waiting until its timeout before it returns either answer.

- **Rationale**: Global limits protect the platform; per-tenant limits stop noisy neighbors. Limits that are never exceeded let operators size downstream systems safely.
- **Actors**: `cpt-cf-task-engine-actor-worker`

#### Queue Depth Limits

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-queue-depth`

Concurrency limits bound running tasks; nothing else bounds waiting ones. A client that polls, times out, and resubmits can grow a queue forever. The system **MUST** therefore limit the number of `queued` tasks (queue depth) and, separately, the number of deferred tasks:

| Limit | Counts | Set by | Default |
|---|---|---|---|
| `queue_max_queue_depth` | `queued` tasks in one queue, all tenants | queue registration, config override | 1,000,000 |
| `tenant_max_queue_depth` | `queued` tasks in one queue, one tenant | config, per queue pattern | 100,000 |
| `global_max_queue_depth` | `queued` tasks in all queues of a task domain | config | 5,000,000 |
| `tenant_max_deferred_tasks` | `scheduled`, `blocked`, and `waiting` tasks of one tenant in a task domain | config | 100,000 |

`0` means unlimited. For a queue, a config override wins over the queue registration, which wins over the config default. `queue_max_queue_depth` and `global_max_queue_depth` cover all tenants, so an exact count would make every create and claim of a queue wait on one shared counter. They **MAY** therefore be checked against a periodically sampled count and passed by at most the admissions of one sampling interval. The per-tenant limits (`tenant_max_queue_depth`, `tenant_max_deferred_tasks`) **MUST** never be passed at admission.

Rules:

- **Admission** — creation (including child tasks), bulk move into a queue, and re-enqueue (§5.8) **MUST** be refused when a limit is full, with `429` (`QUEUE_DEPTH_LIMIT`) and `Retry-After`.
- **Deferring by choice is an admission** — parking in `waiting` (§5.4), release with a delay (§5.6), and adding an unmet predecessor (§5.9) **MUST** pass the deferred limit. If it is full: `429` (`QUEUE_DEPTH_LIMIT`, `deferred`), nothing changes, and a worker keeps its lease. Otherwise a caller could park tasks to free queue space, create more, and later flood the queue.
- **Never strand admitted work** — engine-driven moves (retry, lease-expiry requeue, release without delay, wait resume, unblock, `run_after` reached) **MUST NOT** be refused. A limit can be passed for a short time, by at most the work claimed within one maximum retry backoff.
- **Order** — authorization, validation, and duplicate checks run first, so a replay (`409`) never uses capacity and is never refused as full. Supersession (§5.1) frees the replaced task's capacity before the new task is admitted.
- **Security** — counts are per tenant, so one tenant cannot use another's capacity. A `429` names only the full limit (`queue`, `tenant`, `domain_global`, `deferred`) and never shows usage figures. Changing limits at runtime needs the queue `configure` action (§5.11) and emits `configUpdated`.
- **Subjects share tenant capacity** — within one tenant, all subjects share the depth limits; per-subject sub-limits are out of scope. Tenants that need subject isolation use separate queues or PDP queue constraints (§5.11).
- **Visibility** — current depth and effective limits are shown by Queue Introspection and as metrics.

- **Rationale**: Bounded queues are the normal overload contract of brokers (for example RabbitMQ `x-max-length` with `reject-publish`). Per-tenant limits stop one tenant's resubmission storm; the domain ceiling is the last line of defense. A clear `429` turns silent growth into back-pressure that clients can follow.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`, `cpt-cf-task-engine-actor-platform-admin`

#### Task Type Concurrency Limits

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-type-concurrency`

A task type **MAY** declare `global_concurrency_limit` and `per_tenant_concurrency_limit` in its registration (§5.12). A task **MUST** satisfy both queue and type limits, global and per tenant, before it is assigned.

- **Rationale**: Some limits follow the kind of work, not the queue — for example, at most 5 expensive AI inference tasks at once, wherever they are queued.
- **Actors**: `cpt-cf-task-engine-actor-worker`

#### Concurrency Limit Overrides

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-concurrency-overrides`

Operators **MUST** be able to override the concurrency limits of queues and task types in the Task Engine config, using GTS patterns with wildcards (for example `gts.cf.core.tasks.task.v1~cf.vendor.*`). The most specific pattern wins.

- **Rationale**: Registrations give sensible defaults; operators must be able to tune them per deployment without changing the registrations.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

#### Runtime Configuration Updates

- [ ] `p3` - **ID**: `cpt-cf-task-engine-fr-runtime-config`

Queue settings (concurrency limits, depth limits, default priority, timeouts, retry policy) and concurrency overrides **MUST** be changeable at runtime without a restart. A change needs the queue `configure` action (§5.11) and **MUST** be recorded as a `configUpdated` event with the actor, the time, and the changed keys.

- **Rationale**: Operators tune limits without a deployment; authorization and audit keep changes controlled and traceable.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

#### Queue Introspection

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-queue-introspection`

The system **MUST** show, per queue: queue depth, running count, concurrency limits, effective depth limits, and the number of tasks in each state.

- **Rationale**: Operators need these numbers for capacity planning, autoscaling, and alerts.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

### 5.3 Scheduling + Timeouts

#### Delayed / Scheduled Enqueue

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-delayed-enqueue`

A task **MAY** carry a `run_after` time. The task stays `scheduled` until that time and then becomes `queued`.

- **Rationale**: Delayed start is needed for backoff, maintenance windows, and time-triggered work.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`

#### Multi-Type Timeout Enforcement

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-timeouts`

The system **MUST** enforce these timeouts, and they **MUST** survive restarts:

| Timeout | Fires when | Outcome |
|---|---|---|
| Queue | a task waited in `queued` too long | ends the task with `timed_out` (terminal rule below) |
| Assignment | a claimed task was not started in time (heartbeats do not extend it) | **liveness failure**: back to the queue with backoff, using the `max_liveness_requeues` budget (§5.8); counts toward the worker blacklist (§5.6) |
| Execution | one run took too long, measured from its start (time in `waiting` is not counted; heartbeats do not extend it). **Every task has one**: if neither the request, the type, nor the queue sets it, the engine default applies (`default_execution_timeout_secs`), and every declared value is capped by the trust limits (§5.12) | ends the task with `timed_out`, never retried; not counted against the worker |
| Heartbeat | the lease expired because the worker stopped renewing it | **liveness failure**: as for assignment |
| Lifetime | the task existed too long in total | ends the task with `timed_out`, even if retries remain |
| SLA deadline | the task had not finished by its deadline | publishes `slaBreached`; the task keeps running |

**Terminal rule**: a timed-out task, like a task that used up its retries (§5.8), goes to `dead_lettered` if its queue has a dead-letter queue, otherwise to `failed`. The `timed_out` result is kept either way.

**Deadlines survive requeues**: the lifetime timeout and the SLA deadline are measured from creation and stay in force across retries, requeues, waits, and resumes. Only the deadlines that belong to the state being left (queue, assignment, execution, wait, scheduled start) are dropped on a transition.

All deadlines **MUST** fire within the timer-accuracy bound (§6.1) on a multi-instance deployment ([ADR-0003](ADR/0003-timer-architecture.md)).

- **Rationale**: Every lifecycle stage needs a limit, or tasks can hang forever. Separating liveness failures (the worker was lost) from task failures (the work is too slow) keeps healthy tasks and healthy workers from being punished for each other's problems.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

#### Priority Aging / Fairness

- [ ] `p3` - **ID**: `cpt-cf-task-engine-fr-priority-aging`

The system **SHOULD** raise the priority of tasks that have waited in a queue longer than a configurable threshold, so low-priority work is not starved forever by the same tenant's higher-priority work. Aging never changes urgency.

- **Rationale**: Tenant fairness prevents starvation across tenants; aging prevents it inside one tenant.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

### 5.4 Wait Conditions / Durable Suspension

#### Wait Condition Framework

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-wait-conditions`

A task **MUST** be able to wait durably for a condition. Wait conditions are fixed engine concepts, not GTS types, with four kinds: timer, event, input, and children ([ADR-0004](ADR/0004-wait-condition-model.md)). A worker or an API consumer sets the conditions; the task moves to `waiting` and goes back to `queued` when they are satisfied.

**Waiting parks the task**: while `waiting`, a task holds no worker, lease, or concurrency slot, however long it waits. Setting a wait ends the worker's lease (a later call with the old token fails `LEASE_LOST`), and resuming needs a new claim with a new lease. A wait set by an API consumer on a claimed task ends that lease too. The set-wait call **MAY** include a checkpoint, so the worker that resumes the task can continue from it; the resumed worker also receives the wait result (event header, provided input, child counts, or a timeout note). A `waiting` task counts against the deferred limit (§5.2), so parking **MAY** be refused (`429`); resuming never is.

**Groups**: all conditions of a wait are set in one call, with mode `ALL` (every condition must be satisfied) or `ANY` (the first satisfied one resumes the task). Conditions cannot be added later.

**Arm before acting (no missed events)**: a worker often starts an action and then waits for the event that reports its result. If the event comes before the wait exists, it would be lost. So a worker that holds the lease **MAY** set the group in two steps:

1. **Arm** — set the group without parking. The conditions start matching at once; the task keeps running and keeps its lease. The call returns only when the engine is ready to receive the referenced events; an event admitted shortly before it **MAY** also match. Matches while armed are recorded and do not change the task state.
2. **Park** — park the armed group (optionally with a checkpoint). If the group is already satisfied, the task is not parked: the call returns the result and the task keeps running under the same lease. Otherwise the task moves to `waiting` as usual.

The worker arms, then starts its action, then parks. An armed group is dropped when the task stops running without parking. Wait timeouts start at park time. A task has at most one wait group.

**Timeouts**: each condition **MAY** have a timeout. When it fires, a `waitTimedOut` event is published and the condition's `on_timeout` action applies: fail the task (with `timed_out`, terminal rule §5.3) or resume it with a timeout note. In an `ALL` group the first timeout decides the group at once and cancels the other conditions. In an `ANY` group a timed-out condition just drops out; when all have timed out, the group fails if any condition says `fail`, otherwise it resumes. A task never stays waiting on a group that can no longer be satisfied.

- **Rationale**: Built-in waits replace polling loops and fragile callbacks with something durable and observable. Parking frees workers and capacity during long waits. Arm-then-park closes the classic race between starting an action and waiting for its result.
- **Actors**: `cpt-cf-task-engine-actor-worker`, `cpt-cf-task-engine-actor-api-consumer`

#### Timer Wait Condition

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-wait-timer`

A **timer** condition has a `resume_at` time and is satisfied when that time is reached.

- **Rationale**: Timer waits give cool-downs and planned pauses without external scheduling.
- **Actors**: `cpt-cf-task-engine-actor-worker`

#### Event Wait Condition

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-wait-event`

An **event** condition names an `event_type` (GTS identifier), optionally a `subject_id`, and a `subject_type`. The `subject_type` **MAY** be left out only when the event type allows exactly one subject type, which is then used; otherwise it is required (`400`). It is satisfied by a matching event from the Event Broker. An event **MUST** only match conditions of tasks in the event's own tenant, so one tenant's events can never resume or inject data into another tenant's tasks. A repeated delivery of the same event **MUST NOT** satisfy a condition twice, even across task domains (§5.19).

- **Waitable topics**: only topics the operator lists as waitable are received. A condition on another topic's event type is rejected (`400`). Setting a wait **MUST NOT** change the engine's broker subscription.
- **Authorization**: a wait **MUST NOT** let the caller observe more than its own Event Broker subscription with the same filters would. Setting or arming an event condition **MUST** require the caller's own Event Broker `consume` permission, in the task's tenant, on the event's topic, on the event type, and on the condition's subject type: the same checks the broker makes when a subscriber joins. If the policy decision limits access with conditions, the engine **MUST** apply them to every event before it matches; if it cannot apply them completely, it **MUST** refuse the wait. A denial or an unenforceable decision returns `403`, an unreachable policy service returns `503`, and nothing is stored.
- **Bounded validity**: the engine holds no credential of the caller, so these checks cannot be repeated later. Every event condition **MUST** therefore have a timeout no longer than a configured maximum (default 7 days); a condition without one gets the maximum. A right removed after the wait was set stops counting no later than that.
- **Header only**: the engine **MUST** store and return only the matched event's header (ID, type, subject, subject type, time, broker position), never its data. A resumed worker that needs the data reads it from the source with its own permissions.
- **Bounded per tenant**: one consumer serves all tenants, so the matching work one tenant can cause **MUST** be bounded. The number of pending event conditions (armed or parked) **MUST** be limited per tenant, per tenant and event type, and — tighter — per tenant and event type for conditions without a `subject_id`, which every event of that type must be checked against. Defaults: 100,000, 10,000, and 100 per task domain; `0` means unlimited. Setting or arming a wait over a limit is refused (`429`, `EVENT_WAIT_LIMIT`, naming only the full limit); nothing changes and a worker keeps its lease. Resolving, timing out, or cancelling a condition frees its place.

- **Rationale**: Event waits let a task react to other systems without polling. The engine reads events with platform rights; the caller's subscription-equivalent checks, enforced conditions, bounded validity, and header-only storage keep waits (and their match expressions) from bypassing event authorization. Without per-tenant bounds one tenant could arm enough broad waits to slow event matching, and so wake-ups, for every other tenant.
- **Actors**: `cpt-cf-task-engine-actor-worker`, `cpt-cf-task-engine-actor-event-broker`

#### Event Wait Condition — Match Expression

- [ ] `p3` - **ID**: `cpt-cf-task-engine-fr-wait-event-expression`

An event condition **SHOULD** support an optional `match_expression` that is checked against the event (for example against payload fields) after the field match succeeds. The expression language and its safety limits are defined in [ADR-0007](ADR/0007-event-wait-matching.md). An invalid expression **MUST** be rejected when the wait is set, not when an event arrives.

**Bounded evaluation**: every evaluation **MUST** run under a fixed cost budget that the evaluator enforces while it runs (a wall-clock timeout cannot stop a synchronous evaluation). Outcomes:

- an expression that evaluates to anything but `true`, or fails at run time (for example a missing field), does not match that event;
- an evaluation that runs out of budget does not match either, and counts as an **evaluation failure** of the condition. After a configured number of consecutive failures (default 10) the condition is resolved like a timeout, with reason `eval_budget_exceeded`, and its `on_timeout` action applies, so a task never waits for ever on an expression that cannot finish.

- **Rationale**: Many waits depend on a value inside the event, not only on its type and subject. A cost budget enforced inside the evaluator bounds the work per event; the failure rule keeps a pathological expression from parking a task with no way out.
- **Actors**: `cpt-cf-task-engine-actor-worker`, `cpt-cf-task-engine-actor-event-broker`

#### Input Wait Condition

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-wait-input`

An **input** condition has an `input_schema` (JSON Schema or GTS type reference) and an optional `prompt`. It is satisfied by an API call that provides a JSON value valid against the schema. The value is stored and given to the worker on resume. Providing input needs the `update` action on the task (§5.11).

The schema comes from the caller, so it is untrusted input. It **MUST** be checked when the wait is set, and refused (`400`) if it is too large or too deep, refers to anything outside itself other than a GTS type the caller can see (no remote `$ref`), or uses a regular expression that cannot be checked in linear time. The provided value is subject to the payload size limit (§5.1).

- **Rationale**: Tasks can ask people or systems for data or decisions without being completed and recreated.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`

#### Children Wait Condition

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-wait-children`

A **children** condition has no fields. It is satisfied when the task has no unfinished child tasks (§5.9); if there are none when it is parked, it is satisfied at once. A waiting parent cannot create children, so the set cannot grow. The result **MUST** give the number of children in each terminal state (`completed`, `failed`, `cancelled`, `dead_lettered`). The parent **MUST** be resumed when its last child finishes, without polling, and within the timer-accuracy bound (§6.1) even if a check is lost.

- **Rationale**: Without it, a parent waiting for children must hold its lease and slot or poll. This is the usual fan-out / fan-in pattern (compare Celery chords and Temporal child workflows).
- **Actors**: `cpt-cf-task-engine-actor-worker`

> **Note on retry/back-off**: no separate "backoff" wait kind is needed. Retry delays come from the retry policy (§5.8), and the timer condition covers planned pauses.

### 5.5 Task Entries

#### Fine-Grained Execution Records

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-task-entries`

Workers and users **MUST** be able to attach **task entries** to a task: immutable, timestamped records of what happened during execution. Each entry has a GTS entry type, a time, a creator, and a JSON payload of at most 64 KB (larger is rejected, `413`). Entries **MUST** be listable and filterable by type, time range, and creator. These entry types **MUST** be built in:

```
gts.cf.core.tasks.entry.v1~cf.core.tasks.entry_work.v1~        — work performed
gts.cf.core.tasks.entry.v1~cf.core.tasks.entry_observation.v1~ — observation / check
gts.cf.core.tasks.entry.v1~cf.core.tasks.entry_issue.v1~       — issue encountered
gts.cf.core.tasks.entry.v1~cf.core.tasks.entry_note.v1~        — freeform note
gts.cf.core.tasks.entry.v1~cf.core.tasks.entry_measurement.v1~ — measurement / metric
```

**No client mutation**: entries are audit records, so clients can only create, read, and list them. Only retention (§5.14) removes them.

- **Rationale**: Entries record what happened during execution, separate from progress (how far) and result (what outcome). Making them immutable keeps them trustworthy.
- **Actors**: `cpt-cf-task-engine-actor-worker`, `cpt-cf-task-engine-actor-api-consumer`

### 5.6 Assignment + Atomic Claim + Lease

#### Atomic Claim with Lease

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-assignment`

A worker **MUST** be able to claim a task from one or more queues. The claim assigns the best eligible task across all named queues, gives the worker a lease (§5.6 Lease Fencing), counts the task against concurrency limits, and returns the task — all as one atomic step. Two workers **MUST NOT** claim the same task. A task that another claim is taking at that moment is skipped; the claim takes the best task that no other claim holds.

**Batch claim**: a claim **MAY** ask for up to `max_tasks` tasks (capped by config). Each task is chosen, leased, and counted as in a single claim, with its own lease token; the claim returns between 0 and `max_tasks` tasks. All named queues **MUST** be in one task domain (§5.19); the number of queues per claim **MAY** be capped. Requests that break these rules are rejected (`400`); a worker that consumes queues in several domains sends one claim per domain.

**Order across queues**: each queue offers its next task (§5.2); the claim takes the most urgent, then the oldest, then the lowest task ID. Priority is not compared across queues. The order in which queues are listed does not matter.

**Long poll**: if no task is ready, a claim **MAY** wait for one up to a requested timeout, capped by config (`claim_max_wait_secs`, default 50 seconds, below common 60-second proxy timeouts); a longer request is shortened, not rejected. Clients **SHOULD** use a timeout at or below the cap and send the next claim right after an empty response. A task that becomes ready on any engine instance **MUST** reach a waiting worker within a configurable poll interval, without the Cluster gear ([ADR-0013](ADR/0013-worker-transport.md)). The claim response **MUST** include `heartbeat_interval_secs` — one third of the lease, at least 1 second — so one late heartbeat never loses the lease.

**Attempt count**: `attempt` counts claims. It goes up by one on each claim, except when the claimed task was resumed from a wait or given back by a release (those continue the same attempt).

**Worker identity**: many worker processes often share one service principal. Every claim **MUST** carry a `worker_id`: 1–128 characters from `[A-Za-z0-9._:-]`, naming one worker process (for example pod name plus process ID), and kept across restarts where possible. A missing or invalid `worker_id` is rejected (`400`). The **worker identity** is the pair (caller's `subject_id`, `worker_id`). It is recorded on the assignment and used for the blacklist, events, and audit. Because it includes the authenticated subject, one principal cannot act as, or blacklist, another principal's workers. `worker_id` is not a credential ([ADR-0014](ADR/0014-worker-identity.md)).

The assignment **MUST** record the worker identity, claim time, worker session (whose record holds the lease expiry and last heartbeat), attempt, lease token, and lease epoch.

- **Rationale**: Atomic, lease-based claiming is the base of safe distributed execution: no double dispatch, and abandoned tasks are recovered automatically. A per-process `worker_id` lets the engine blacklist one faulty replica instead of a whole fleet.
- **Actors**: `cpt-cf-task-engine-actor-worker`

#### Heartbeat / Lease Renewal

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-heartbeat`

A heartbeat **MUST** renew the lease and return the cancel-requested flag, the task state, and `heartbeat_interval_secs`. Workers **MUST** heartbeat at least that often. A **batch heartbeat** **MUST** renew many leases of one caller in one request, with a result per lease (renewed, or `LEASE_LOST`). A heartbeat with a stale token returns `LEASE_LOST`, which tells the worker to stop.

**Worker sessions**: leases are kept alive per worker process, not per task ([ADR-0017](ADR/0017-worker-session-leases.md)). The first claim of a worker process in a task domain opens a **worker session**; every lease the process holds in that domain belongs to it. A heartbeat renews the session, and so all of its leases; the cost of a heartbeat **MUST NOT** grow with the number of tasks the process holds. As a result, a task stays leased while its worker process keeps heartbeating, even if the task's own handler is stuck; the execution timeout (§5.3), which every task has, bounds that case, and a worker that stops working on a task **MUST** release it. When a session expires, all of its leases expire together and count as one outage for the blacklist (below).

If a lease expires, it is a liveness failure (§5.3): the task goes back to the queue with backoff while the `max_liveness_requeues` budget remains (§5.8), otherwise it ends with `timed_out`. Lease expiry **MUST** use the retry policy's backoff, not an immediate requeue, and counts toward the worker blacklist (below).

- **Rationale**: Heartbeats detect lost workers and carry control signals back to them. Backoff on expiry stops a slow but alive worker from getting the same task back in a tight loop. Batch heartbeats keep request volume low for workers that run many tasks.
- **Actors**: `cpt-cf-task-engine-actor-worker`

#### Voluntary Release

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-release`

A worker **MUST** be able to give a `claimed` or `running` task back without failing it — for example when it shuts down or is over capacity. The release carries the lease token and **MAY** carry a checkpoint and a `delay_secs` value. As one atomic step it ends the lease, frees the task's concurrency slots, applies the checkpoint, and moves the task to `queued` (or to `scheduled` after the delay). A `taskQueued` event with reason `released` is published.

A release **MUST NOT** use either retry budget, **MUST NOT** count toward the worker blacklist, and does not start a new attempt. A release without a delay is never refused by queue depth limits; a release with a delay passes the deferred limit (§5.2) and may get `429`. A release with a stale token returns `LEASE_LOST`.

- **Rationale**: Without it, a worker that stops early must let its lease expire: the task is delayed by a full lease, a liveness requeue is spent, and a healthy worker gets a blacklist strike. Lease-based queues offer the same operation (SQS visibility timeout 0, Azure Service Bus abandon).
- **Actors**: `cpt-cf-task-engine-actor-worker`

#### Worker Blacklisting

- [ ] `p3` - **ID**: `cpt-cf-task-engine-fr-worker-blacklist`

The system **MUST** stop assigning tasks from a queue to a worker (worker identity, §5.6 Worker identity) that keeps failing on that queue:

- **What counts**: a reported failure with `fault=worker`, and a liveness failure (lease expiry or assignment timeout). Each counts as one failure.
- **Outages count once**: all liveness failures of one worker on one queue that are detected within one lease duration of the last counted one count as a single failure, because one connection loss drops every lease the worker holds. Reported `fault=worker` failures always count.
- **What does not count**: failures with `fault=task` or `fault=dependency`, and execution timeouts. They neither count nor reset the counter, so bad input or a broken downstream service never blacklists a healthy worker.
- **Reset**: the counter goes back to zero on the worker's next successful completion on that queue.
- **Blacklist**: after a configurable number of consecutive failures (default 10), the worker is skipped for that queue for a configurable time (default 60 minutes). Both values can be set per queue. Each blacklisting is recorded as a `workerBlacklisted` event.

- **Rationale**: Stop sending work to a worker that keeps failing, while letting it recover after a cool-down. Counting only worker-caused failures keeps worker health separate from task health.
- **Actors**: `cpt-cf-task-engine-actor-worker`, `cpt-cf-task-engine-actor-platform-admin`

#### Reassignment / Handoff

- [ ] `p3` - **ID**: `cpt-cf-task-engine-fr-reassignment`

An administrator **MUST** be able to move a claimed or running task to another worker identity (subject and `worker_id`) without losing its history. It needs the `admin` action (§5.11). In one atomic step it ends the current lease (the old holder gets `LEASE_LOST`), records the new assignee, increases the lease epoch, and records `taskReassigned`. State, attempt, slots, and deadlines do not change.

The administrator gets no lease token. The new holder calls **accept** with its own credentials (`claim` action; subject and `worker_id` **MUST** match the new assignee) and gets a new token; anyone else gets `404`. If nobody accepts within one lease duration, the task is requeued like a lost lease (§5.3), with no blacklist strike.

- **Rationale**: Supports hand-off without recreating the task. Giving the administrator a token would make the token a password.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

#### Lease Fencing

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-lease-fencing`

Every lease grant (claim, or accept after reassignment) **MUST** issue a **lease token** and the task's **lease epoch**, returned only to the new holder. The token is random (at least 122 bits from a secure generator, no timestamp part) and unique per lease. The epoch grows with every lease and reassignment and never decreases ([ADR-0011](ADR/0011-lease-fencing.md)).

**Holder binding**: a lease belongs to the subject that received it. Every worker write — start, heartbeat, progress, checkpoint, complete, release, set wait, and worker entries — **MUST** carry the token and come from that subject; otherwise it is rejected with `409` (`LEASE_LOST`) and changes nothing. A copied token is useless to anyone else. `LEASE_LOST` is final: the worker **MUST NOT** retry the call and **MUST** treat the task as lost. A progress update accepted under a lease that is lost before it is saved **MUST** be dropped. A lost token never becomes valid again. Non-worker operations (create, cancel, reassign, provide input, bulk, admin) carry no token and are governed by authorization only.

**Idempotent completion**: the lease holder repeating a completion with the same token and the same result **MUST** get the stored outcome (`200`), so a network retry is not mistaken for lease loss. A different result is rejected (`400`, task already ended).

- **Rationale**: A worker that stalls past its lease (GC pause, network split) must not be able to complete or overwrite a task already given to someone else. An identity check alone fails when the same worker claims the task again; a token alone acts as a password if it leaks. Together they need a current lease and the holder's credentials. The epoch lets workers protect external systems.
- **Actors**: `cpt-cf-task-engine-actor-worker`

#### Execution Semantics

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-execution-semantics`

Execution is **at-least-once**: a task **MAY** run more than once (after lease expiry, a crash, or a retry). Task state is fenced, so at most one completion is accepted per task. There is **no** exactly-once guarantee for side effects outside the engine. Therefore:

- Workers **MUST** make external side effects idempotent, keyed on a value that is the same for every attempt — the task ID (plus a step ID for multi-step tasks), **not** the attempt number or lease epoch.
- Workers **SHOULD** pass the lease epoch to external systems that support fencing.
- Workers **SHOULD** resume from the last checkpoint.
- Workers **SHOULD** stop as soon as the lease is lost — on `LEASE_LOST`, or when their own lease deadline passes without a successful renewal.

- **Rationale**: Every lease-based dispatcher (SQS, Kafka, Temporal) is at-least-once. Stating it clearly, next to what *is* guaranteed, tells worker authors exactly where idempotency is their job.
- **Actors**: `cpt-cf-task-engine-actor-worker`

### 5.7 Progress Tracking

#### Multi-Dimensional Progress

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-progress`

Workers **MUST** be able to report progress with: percent (0–100), and optionally a milestone, completed/total counters, a current stage, and an ETA. Progress writes **MUST** support batching to reduce database load, and the batching **MUST** be configurable: the flush interval is set globally and per task type or queue (resolved as for progress events, below). Progress read through the API is at most one flush interval old.

- **Rationale**: Useful monitoring needs more than one percentage.
- **Actors**: `cpt-cf-task-engine-actor-worker`

#### Progress Events

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-progress-events`

The system **MUST** support a `taskProgressed` event with the latest progress, controlled by a **progress event mode**:

- `none` — no progress events (**default**);
- `milestones` — an event only when the milestone or stage changed;
- `all` — an event when any progress value changed.

The mode and the flush interval can be set on the task type (GTS trait) and on the queue, and overridden in config by type or queue pattern. Order of precedence: config for the type, config for the queue, type trait, queue registration, global default.

A progress event **MUST** only be published together with the saved progress it describes, at most once per task per flush. Changes within one flush **MAY** be merged, so consumers **MUST NOT** expect an event for every intermediate step. Publishing also follows the event publishing policy (§5.10).

- **Rationale**: Dashboards otherwise have to poll every running task. Per-type settings let user-facing long tasks report milestones while high-volume system tasks stay quiet.
- **Actors**: `cpt-cf-task-engine-actor-worker`, `cpt-cf-task-engine-actor-api-consumer`, `cpt-cf-task-engine-actor-event-broker`, `cpt-cf-task-engine-actor-platform-admin`

### 5.8 Retry / Failure Handling

#### Configurable Retry Policies

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-retry`

A task **MUST** have a retry policy, set on the task or inherited from the task type or queue (§5.12 Type defaults resolution). A policy has: `max_retries`, `max_liveness_requeues`, a backoff strategy (`fixed`, `linear`, `exponential`), base and maximum intervals, a jitter factor, and optional `non_retryable_codes`. A reported error is retryable only if the worker marks it `retryable` and its code is not in `non_retryable_codes`.

**Two separate budgets**:

- `max_retries` limits retries after **reported** retryable failures.
- `max_liveness_requeues` limits requeues after **liveness** failures (lease expiry, assignment timeout). `0` means unlimited, bounded only by the lifetime timeout; the queue default is 20.

A liveness failure **MUST NOT** use `max_retries`, and a reported failure **MUST NOT** use `max_liveness_requeues`, so losing a worker never eats the retry budget of a healthy task. Each budget computes its backoff from its own count.

**Backoff**: the n-th retry or requeue of a budget (n starts at 1) waits `base` (`fixed`), `base × n` (`linear`), or `base × 2^(n−1)` (`exponential`), capped at the maximum interval. The calculation **MUST NOT** overflow. Jitter only **shortens** the wait, by a random part of up to `jitter_factor` of it, so the wait never exceeds the maximum interval (`jitter_factor = 1` gives "full jitter").

**Lifecycle**: a retryable failure with budget left moves the task to `scheduled` with a backoff delay; it **MUST NOT** pass through `failed`. A terminal failure, or a failure with no budget left, follows the terminal rule (§5.3): `dead_lettered` if the queue has a dead-letter queue, otherwise `failed`.

- **Rationale**: Retries with backoff are the standard answer to transient failures. Separate budgets keep worker loss and task failure from consuming each other's allowance.
- **Actors**: `cpt-cf-task-engine-actor-worker`

#### Dead Letter Queue

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-dead-letter`

A task that ends in failure on a queue with a dead-letter queue **MUST** be moved there instead of being discarded. The engine records the queue it came from. A dead-letter queue is a queue registered with `kind = dead_letter` (§5.12): it only holds `dead_lettered` tasks, it is never dispatched (a claim naming it is rejected, `400`), and it has no dispatch settings. The task state (`dead_lettered`) says the task ended and can be re-enqueued; the queue says where it is parked, which drives DLQ listing, metrics, and PDP queue constraints. A dead-letter queue is not bound to a task domain: a dead-lettered task stays in the database of the queue it came from (§5.19), so one dead-letter queue can serve queues in several domains. Dead-lettered tasks **MUST** be searchable, inspectable, and re-enqueueable; re-enqueue needs the `admin` action.

**Re-enqueue rules**: an administrator **MAY** re-enqueue a `dead_lettered` task (to the queue it came from) or a `failed` task (to its current queue), so failed work can be retried even without a dead-letter queue. `completed` and `cancelled` tasks cannot be re-enqueued. A re-enqueue **MUST**, as one atomic step:

- reset both retry budgets (`attempt` keeps counting);
- clear the previous result and keep the context, so the next run can resume from its checkpoint;
- restart the lifetime timeout from now, and keep the SLA deadline only if it is still in the future;
- for a type with a derived dedup identity, take the identity again, or fail with `409` naming the task that now holds it;
- for a type with supersession, fail with `400` (`STALE_ORDER`) if the group has a newer order value;
- pass queue depth admission (may be refused with `429`);
- move the task to `queued` and publish `taskQueued` with reason `reenqueued` and the actor.

Successors already cancelled with `dependency_failed` are not revived (§5.9).

**Inspection across domains**: listing dead-lettered tasks **MUST** cover all task domains (§5.19), unless it is filtered by a source queue, so operators never miss parked failures.

- **Rationale**: Failed work must never vanish silently. A clean re-enqueue gives the task a full fresh start without reusing stale state.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

### 5.9 Dependencies / Parent-Child Execution

#### Parent/Child Task Hierarchies

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-parent-child`

A task **MAY** create child tasks while it runs. The system **MUST** track each child's state. When the parent sets `wait_for_children`, its successful completion is rejected while any child is unfinished (`400`, `CHILDREN_PENDING`); a parent that must wait **SHOULD** park on a children condition (§5.4). Cancelling a parent cancels its unfinished children, unless the parent sets `propagate_cancel=false` (default `true`). A parent and its children **MUST** be in the same task domain (§5.19).

**Forced cancellation**: a force-cancelled parent ends at once and does not wait for its children. Every unfinished child **MUST** still be cancelled exactly once, even across crashes and restarts.

- **Rationale**: Large work can be split into independent sub-tasks; guaranteed cancellation leaves no orphans.
- **Actors**: `cpt-cf-task-engine-actor-worker`

#### Predecessor/Successor Dependencies

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-dependencies`

A task **MAY** name predecessors that must **succeed** — end as `completed` (result `success` or `warning`) — before it can be claimed. Modes: `ALL` (every predecessor must succeed) and `ANY` (the first success unblocks). A predecessor and its successor **MUST** be in the same task domain.

**Predecessor failure**: a predecessor that ends `failed`, `cancelled`, or `dead_lettered` counts as failed (a later re-enqueue does not revive its successors). When a successor can no longer be unblocked — any failure in `ALL` mode, all failed in `ANY` mode — it **MUST** be cancelled with `dependency_failed`, and this cascades to its own successors. Every affected successor **MUST** be cancelled exactly once, even for large fan-outs and across crashes. A creation or change that would be unsatisfiable at once (for example `ALL` on an already failed task) is rejected (`400`).

**Changes and cycles**: dependencies **MAY** be added or removed while the task is unfinished and not yet running. Adding an unmet predecessor moves a `queued` or `scheduled` task to `blocked` (counted against `tenant_max_deferred_tasks`); removing the last one unblocks it. Every change **MUST** be checked for cycles; a change that would create one is rejected (`400`, `CYCLE_DETECTED`). The check **MUST** also hold when several changes run at the same time: concurrent additions that would together form a cycle **MUST NOT** all succeed. The number of predecessors per task (default 64) and the size of the graph a cycle check may walk (default 10,000 tasks) are bounded; a change over either bound is rejected (`400`, `DEPENDENCY_GRAPH_TOO_LARGE`).

**Concurrent end of a predecessor**: a dependency added, or a task created with predecessors, at the same moment a predecessor ends **MUST** see that outcome — it is satisfied, cancelled with the successor rules above, or rejected as unsatisfiable. A successor **MUST NEVER** stay `blocked` on a predecessor that has already ended.

- **Rationale**: Dependencies give multi-step pipelines without an external orchestrator. Checking cycles on every change, not only at creation, keeps the graph valid; two checks that each look correct can still create a cycle together, so the guarantee must cover concurrency.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`

#### Link Authorization

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-link-authz`

A link affects the task it points at, so the system **MUST** authorize that task too:

- **Same tenant**: parent and children, and predecessor and successors, **MUST** have the same owner tenant.
- **Parent**: creating a child needs the parent's lease (the caller holds it and sends its token, §5.6) or `update` on the parent. The parent **MUST** be unfinished and not `waiting`. A child created at the same moment its parent ends is either rejected or reached by the parent's cancellation (§5.9 Parent/Child) — never left orphaned.
- **Predecessor**: naming one needs `read` on it. Adding or removing a dependency also needs `update` on the successor.
- **No existence oracle**: a missing, foreign, or forbidden linked task gets the same `404`, never `403`. State checks (unsatisfiable, `CYCLE_DETECTED`) run only after this.

- **Rationale**: Otherwise any creator could block another subject's parent with children, cancel work through a cascade, or probe tasks it cannot read.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`, `cpt-cf-task-engine-actor-worker`

### 5.10 Event History / Audit Trail

#### Immutable Event Log

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-event-history`

The system **MUST** keep an immutable, append-only history per task: state changes, assignments (including reassignment and blacklisting), waits set and resolved, retries, enabled progress events, checkpoints, cancel requests, timeouts, configuration changes, and user actions. Each entry has a unique, stable event ID, a time, the actor, the event type, and a structured payload.

The history **MUST** always be complete, whatever the publishing policy (below) says, and **MUST** be written atomically with the change it records, so it can never disagree with the task state.

- **Rationale**: A complete audit trail is needed for debugging, compliance, and operations. Keeping it independent of external publishing lets operators reduce event traffic without losing audit data.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`, `cpt-cf-task-engine-actor-api-consumer`

#### Event Publishing

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-event-publishing`

The system **MUST** publish lifecycle events to the platform Event Broker. Supported event types: `taskCreated`, `taskQueued`, `taskClaimed`, `taskStarted`, `taskCompleted`, `taskFailed`, `taskCancelled`, `taskCancelRequested`, `taskCheckpointed`, `taskProgressed`, `taskWaiting`, `taskResumed`, `taskRetrying`, `taskReassigned`, `taskDeadLettered`, `taskTimedOut`, `slaBreached`, `waitConditionSet`, `waitConditionSatisfied`, `waitTimedOut`, `workerBlacklisted`, `configUpdated`. `taskTimedOut` is published when a task ends because of a timeout (§5.3); `taskProgressed` only as enabled by the progress event mode (§5.7).

**Delivery contract**:

- Each event carries the stable event ID from the history.
- A state change and its events **MUST** be recorded atomically in the database of the task's domain (§5.19), even when the Event Broker is down; events are delivered later, in order per task.
- Delivery is **at-least-once**. Consumers, including the engine's own event-wait matcher, **MUST** deduplicate on the event ID.

**When the Event Broker is unavailable**: the task history stays readable through the API (`GET /tasks/{id}/events`). Undelivered events wait in the platform outbox and are delivered when the broker is back. Retention and back-pressure of undelivered events are governed by the platform outbox, not by the engine.

**Publishing identity**: events are published by the engine itself, never on behalf of the caller that caused them. The engine **MUST** publish as a dedicated root-tenant service subject (client credentials, tenant plane, §5.11), separate from the subject it uses to consume waitable topics. Each event names the task's owner tenant as its tenant (queue-scoped events: the tenant recorded in the history). That subject **MUST** hold Event Broker `produce` on the Task Engine topic, on the Task Engine event types, and on the task and queue subject types, for every tenant that can own tasks — including tenants behind a barrier — and nothing more. A publish the broker refuses for authorization is a deployment fault: it **MUST NOT** change task state, the event stays in the history, and it **MUST** raise an alert.

- **Rationale**: Other gears react to task changes without polling, and event waits get their input. Atomic recording means state and events never disagree and no event is lost. A fixed, narrowly granted publishing identity lets operators see and limit exactly what the engine may emit, and keeps a caller from emitting events into tenants it cannot reach.
- **Actors**: `cpt-cf-task-engine-actor-event-broker`

#### Event Publishing Configuration

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-event-publishing-config`

Operators **MUST** be able to choose which events are published, per task type pattern (with wildcards). Each policy names a pattern, the enabled event types (or `all` / `none`), and optionally the state transitions that trigger events (for example only `taskCompleted` and `taskFailed` for light system tasks). A `default` policy covers unmatched types. Without configuration, the default is the `lifecycle` preset: every event type except the frequent execution events `taskQueued`, `taskClaimed`, `taskStarted`, `taskCheckpointed`, and `taskProgressed`; operators opt into `all` explicitly. The local history always records everything.

- **Rationale**: High-volume system tasks need only key events, while user-facing tasks may want everything. A quiet default keeps event traffic and storage in check.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

### 5.11 Authentication and Authorization

#### Authentication via AuthN Resolver

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-authn`

Every protected request **MUST** be authenticated by the platform (API Gateway and AuthN Resolver), which gives the engine the caller's security context (subject, tenant, token scopes). The engine exposes no anonymous endpoint of its own: health and readiness are served by the platform health endpoints, and metrics are exported through the platform telemetry pipeline (§5.15).

- **Rationale**: The engine follows the platform model: the AuthN Resolver checks tokens and gears act as policy enforcement points ([docs/arch/authorization/DESIGN.md](../../../docs/arch/authorization/DESIGN.md)).
- **Actors**: All actors

#### Authorization via PolicyEnforcer (PEP)

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-authz`

Every access **MUST** be authorized by the platform policy decision point (AuthZ Resolver), and its constraints **MUST** limit which tasks a query can see or change. Authorization fails closed: a denial, an unreachable PDP, or missing constraints result in `403`. PDP internals are never shown to clients. **No existence oracle**: a request about one task that the caller may not read **MUST** get the same `404` as a task that does not exist; `403` is returned only when the caller may read the task but not perform the action.

Permissions are GTS permission instances (base type `gts.cf.toolkit.authz.permission.v1~`) in the `task_engine` namespace:

- **Task actions** (resource `gts.cf.core.tasks.task.v1~`): `create`, `read`, `update`, `delete`, `claim`, `cancel`, `admin`, `expedite`. `expedite` allows urgency `critical` (§5.2) and is meant for platform gears and operators. A permission on the base type covers every task type; policies can narrow it to one type or a type pattern (below).
- **Queue actions** (resource `gts.cf.core.tasks.queue.v1~`): `read` (list and inspect queues) and `configure` (runtime configuration, §5.2). Seeing or tuning queues never grants access to task data.

**Least privilege for workers**: every worker call that carries a lease token — claim, start, heartbeat, progress, checkpoint, complete, release, worker wait (arm and park), and worker entries — and accept **MUST** need only `claim`. A worker needs `claim` (and `read` for its own reads), never `update`; to create children of its own task it needs `create` plus the parent's lease (§5.9). `update` covers changes an API consumer makes to a task it does not hold: tags, dependencies, providing input, and setting a wait. Reassignment needs `admin` (§5.6 Reassignment).

- **Rationale**: Policy-based, query-level enforcement follows the platform authorization architecture ([docs/arch/authorization/DESIGN.md](../../../docs/arch/authorization/DESIGN.md)). Narrow worker permissions limit the damage a compromised worker can do.
- **Actors**: All actors

#### Tenant Scoping and Hierarchy

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-tenant-scoping`

Every task **MUST** have an owner tenant (the isolation boundary) and **MAY** have an owner subject (for "my tasks" views). Every query **MUST** be limited to the caller's accessible tenants, resolved through the Tenant Resolver, regardless of what the PDP returns, and every operation **MUST** check tenant ancestry.

- **Rationale**: Tenant scoping is the base of multi-tenant isolation; owner subjects give per-user views inside it.
- **Actors**: All actors

#### Type and Queue Access Policies

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-resource-path-access`

Policies **MUST** be able to limit, per subject, which task types and queues it may create in, claim from, read, or change. The engine **MUST** send each task's type and queue to the PDP and apply the returned constraints to the write or query ([ADR-0016](ADR/0016-authorization-model.md)). Token scopes **MUST** act as a ceiling on allowed operations.

- **Rationale**: A broad `create` grant must not let a tenant put a platform task type into a queue served by privileged workers. Teams in one tenant can also be kept apart by queue.
- **Actors**: `cpt-cf-task-engine-actor-worker`, `cpt-cf-task-engine-actor-api-consumer`

#### Worker Principals

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-worker-principals`

All task work happens inside a tenant, so every task operation **MUST** use a tenant security context, also for platform services. The platform (workload) context is used only for work without a tenant, such as registering the engine's own GTS entities ([ADR-0016](ADR/0016-authorization-model.md)).

- **Tenant workers** run as a subject of a tenant and see only that tenant's subtree. Tenant barriers apply.
- **Platform workers** run as a service subject of the root tenant (client credentials). They reach tenants behind a barrier only on queues with `operated_by: platform` (§5.12), and only if the PDP allows it.
- **Workers act as themselves.** The claim returns the task's owner tenant, creator, and owner. When a worker calls other gears for a task, it names the task's owner tenant as the context tenant, so the PDP limits it to that tenant. The engine **MUST NOT** store or forward the creator's token.
- **Creator authority.** The tenant limit does not stop a creator from naming, in the task, a resource of its own tenant that only the worker may reach. Each task type therefore declares an `authority` mode (§5.12):
  - `creator_verified` (default): the type lists **creator checks** — each names where a resource is referenced (a resource reference type pattern, or a required string field of the input) and the action the work will perform on it. At creation the engine **MUST** ask the PDP, with the creating caller's own security context and the task's owner tenant as context tenant, whether that action is allowed on every referenced resource. A denial, an unreachable PDP, or a decision the engine cannot fully evaluate (constraints on properties only the resource's owning gear knows) rejects the create (`403`, fail closed; `503` when the PDP is unreachable) and stores nothing. Checked references are stored as `verified`. A worker of such a type **MUST** act downstream only on resources that passed a check, and **MUST NOT** use its own wider rights to reach any other resource named in the task.
  - `worker`: the work uses only the worker's own authority. Allowed only for platform-owned types. It fits work that acts on no creator-chosen resource, and **gear-mediated creation**, where the gear that owns the resources authorizes the user with its own PEP and then creates the task as its own service subject.
  - Checks run once, at creation; the engine holds no creator credential to repeat them later. A right revoked after creation is therefore not seen by tasks already created. Types that need a short window **SHOULD** set a short lifetime timeout.
- **Loss of access stops work.** Every worker call checks `claim` again. On an explicit denial the engine returns the task to the queue as a voluntary release (no budget, attempt, or blacklist change) and answers `403`; the worker **MUST** stop. If the PDP cannot be reached, the call fails with `503` and nothing changes.

- **Rationale**: Shared workers with broad rights are the usual path to privilege escalation in a task engine. Following the platform's two-plane model and the Durable Execution rule ("worker identity does not replace the execution owner") keeps the task's tenant in force for every action. Creator checks close the remaining gap inside a tenant: the platform has no token exchange, so the only moment the engine can prove the creator's own rights is the create request, which carries the creator's context.
- **Actors**: `cpt-cf-task-engine-actor-worker`, `cpt-cf-task-engine-actor-platform-admin`

### 5.12 GTS Extension Points

#### Task Type as GTS Base Type

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-gts-task-type`

The system **MUST** define a GTS base type for task types:

```
gts.cf.core.tasks.task.v1~
```

Other gears register derived task types with input and output JSON Schemas ([ADR-0015](ADR/0015-gts-hybrid-model.md)). A task type **MAY** declare, as GTS traits: concurrency limits, `dedup` and `supersede` policies (§5.1), progress settings (§5.7), the `authority` mode and creator checks (§5.11 Creator authority), and **type defaults** — `default_queue`, `timeouts`, `retry_policy`, and `retention_days`. A new task type **MUST NOT** need a database migration or an engine release.

**Type defaults resolution**: for each setting the engine uses the first value found: (1) the create request, (2) the task type trait, (3) the queue's effective value (config override, then queue registration). The queue itself comes from the request, then the type's `default_queue`, then the `default` queue. Each timeout resolves on its own, so a request may set one timeout and inherit the rest; a retry policy is taken whole from the first level that has one. `retention_days` follows §5.14.

- **Rationale**: Gears add task types at runtime without migrations. Registering task types and queues is itself a privileged action, governed by the GTS registry's authorization. Type-level defaults let each kind of work carry sensible settings while operators keep the final word through config.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`

#### Queue as GTS Base Type

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-gts-queue`

The system **MUST** define a GTS base type for queues:

```
gts.cf.core.tasks.queue.v1~
```

Every queue **MUST** be a registered GTS instance of this type; task creation and claims **MUST** name registered queues. A queue has a `kind`: `dispatch` (default) or `dead_letter`.

A `dispatch` queue **MUST** declare concurrency limits, a default priority, default timeouts, and a default retry policy. It **MAY** declare default progress settings, `queue_max_queue_depth`, blacklist threshold and duration, a tenant fairness mode (default `round_robin`), and a dead-letter queue. A `dead_letter` queue declares only its ID and description; dispatch settings **MUST NOT** be set on it, and a dead-letter reference **MUST** point to a `dead_letter` queue. New queues **MUST NOT** need a database migration.

- **Rationale**: Queues as GTS instances are discoverable and self-describing, and their settings are defined once and can be overridden in config.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

#### Task Type and Queue Trust

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-type-trust`

Task types, queues, and entry types are registered by other gears and, where the Types Registry allows it, by tenants. The engine **MUST** treat their settings as untrusted input:

- **Who may register** is set by the Types Registry registration policy for each GTS region. Expected setup: task and entry types may be tenant-owned if the deployment allows it; queues are platform-owned only.
- **Visibility**: a create **MUST** resolve the type with the caller's security context. A type the caller cannot see is unknown (`UNKNOWN_TASK_TYPE`).
- **Limits**: every value a type or queue declares (lease, timeouts, retry budgets, retention, progress settings) **MUST** be inside engine limits set in config, with tighter limits for tenant-owned types. A type outside the limits cannot be created; a queue outside them refuses new tasks; a create request outside them is rejected (`400`). The error names the setting. The minimum lease is 10 seconds by default. The execution timeout has a maximum (default 24 hours for platform-owned types and queues, 1 hour for tenant-owned types), so no task can hold a lease and a concurrency slot without bound while its worker process keeps heartbeating.
- **Platform queues**: a tenant-owned type **MUST NOT** be placed in a queue with `operated_by: platform`. A queue **MAY** list `accepted_types`; other types are rejected (`400`) on create, move, and re-enqueue.
- **Authority mode**: a tenant-owned type **MUST NOT** declare `authority.mode = worker` (§5.11 Creator authority); such a type cannot be created. Creator checks are bounded in number (default 16 per type) and their pointers **MUST** name required string fields of the input schema.
- **Removed or unavailable type**: new tasks are refused as unknown; existing tasks run to the end. Output is checked against the type revision recorded at creation.

- **Rationale**: Extending the engine must not let a registrant switch off its protections, or let one tenant use another tenant's private type.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`, `cpt-cf-task-engine-actor-api-consumer`

#### Task State and Result Code Domain Enums

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-state-result-enums`

Task states, result codes, and urgency **MUST** be closed engine enums, not GTS entities ([ADR-0015](ADR/0015-gts-hybrid-model.md)):

- **States**: `scheduled`, `queued`, `blocked`, `claimed`, `running`, `waiting` (not terminal); `completed`, `failed`, `cancelled`, `dead_lettered` (terminal).
- **Result codes**: `success`, `warning`, `error`, `cancelled`, `timed_out`, `forcibly_cancelled`, `superseded`, `dependency_failed`.
- **Urgency**: `critical`, `normal` (default) (§5.2).

A terminal task **MUST NOT** change state again, except by re-enqueue (`dead_lettered` or `failed` → `queued`, §5.8 Re-enqueue rules).

**State and result code pairing**: a terminal task has exactly one result code, from the set allowed for its state. Unfinished tasks have none.

| Terminal state | Allowed result codes |
|---|---|
| `completed` | `success`, `warning` |
| `failed` | `error`, `timed_out` |
| `dead_lettered` | `error`, `timed_out` (the code of the failure that sent it there) |
| `cancelled` | `cancelled`, `forcibly_cancelled`, `superseded`, `dependency_failed` |

"The task succeeded" always means `completed` with `success` or `warning`. A worker may report only `success`, `warning`, `error`, or `cancelled` (the last only after a cancel request); other codes are rejected (`400`). `timed_out`, `forcibly_cancelled`, `superseded`, and `dependency_failed` are set only by the engine.

**Enum evolution** (applies to all engine enums: states, result codes, urgency, wait kinds, timer types, dependency modes, concurrency dimensions):

- Each value has a fixed numeric code that is never renumbered or reused.
- On the wire (REST/JSON) a value is its snake_case name (for example `"dead_lettered"`, `"timed_out"`).
- A client that gets a value it does not know **MUST** show it as `Unknown`, never guess or crash. A server that reads an unknown stored value **MUST** treat it as non-actionable.
- Adding a value is a minor change; renumbering, removing, or reusing one is breaking. This also holds for rolling upgrades and stored data.

- **Rationale**: States and result codes drive the state machine and must stay exhaustive and safe. Clear evolution rules let them grow across SDK versions and mixed-version clusters.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`

#### Task Entry Type as GTS Base Type

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-gts-entry-type`

The system **MUST** define a GTS base type for task entry types (`gts.cf.core.tasks.entry.v1~`) with built-in derived types for work, observation, issue, note, and measurement (§5.5).

- **Rationale**: Gears can define their own entry kinds for domain-specific records.
- **Actors**: `cpt-cf-task-engine-actor-worker`

### 5.13 REST API

#### REST API

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-rest-api`

The system **MUST** expose a REST API at `/api/task_engine/v1` covering:

- task create, read, claim, start, heartbeat (single and batch), progress, checkpoint, release, complete, cancel, reassign, and accept a reassignment;
- task entries: create, read, list (no update or delete, §5.5);
- wait conditions: set (arm and park), list, and provide input — approvals use an event wait on the `approval-service` gear (§4.3), not dedicated endpoints;
- queues: list, inspect, and configure;
- dependencies: add and remove;
- search with filters, sorting, detail levels, and cursor paging, plus a capped count query (§5.17);
- bulk operations (§5.18), DLQ inspection and re-enqueue (§5.8);
- task event history, paged — the complete history of a task;
- no health or metrics endpoints of its own: health and readiness go through the platform health endpoints, metrics through platform telemetry (§5.15).

Errors **MUST** use RFC 9457 Problem Details with the platform's canonical error types.

- **Rationale**: The REST API is the main integration surface for every actor.
- **Actors**: All actors

### 5.14 Data Retention and Cleanup

#### Scheduled Retention

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-retention`

The system **MUST** remove finished tasks, with their entries and history, once their retention has passed:

- Retention is counted from the moment the task **finished**, not from its creation.
- Unfinished tasks are never removed by retention. They are bounded by their lifetime timeout and by the hard retention limit below.
- One tenant's or type's policy **MUST NOT** delay cleanup of other data (for example, one tenant's legal hold must not block everyone else's cleanup).

Retention is set at three levels, highest first:

1. **Per tenant** — for compliance needs. It wins over type rules, because legal obligations belong to the tenant.
2. **Per task type** — by GTS pattern in config (most specific pattern wins), or by the type's `retention_days` trait (§5.12); a config pattern wins over the trait.
3. **Global default** — **100 days**.

Before cleanup, tasks still unfinished after the **hard retention limit** **MUST** be force-cancelled. Cleanup runs separately in each task domain.

- **Rationale**: Without retention the database grows forever. Per-type rules clean short-lived system tasks quickly; per-tenant rules meet compliance needs. Counting from completion keeps every finished task for its full retention.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

### 5.15 Observability

#### Instrumentation and Metrics

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-observability`

The system **MUST** emit metrics through the platform telemetry pipeline (OpenTelemetry) for: request counts and latency, queue depth and running counts, retries, DLQ sizes, wait conditions by kind, SLA breaches, timer and signal backlog, retention, and database pool usage. Metrics **MUST NOT** be readable without authentication and **MUST NOT** carry tenant-private values (tenant or subject IDs, tenant-owned task type IDs) or unbounded labels. Readiness **MUST** be reported through the platform healthcheck mechanism and reflect database connectivity, applied migrations, and completed startup recovery; liveness stays shallow.

- **Rationale**: Operators need these signals for capacity planning, alerts, and health checks.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

### 5.16 SDK Crate

#### Public SDK

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-sdk`

The system **MUST** provide a `task-engine-sdk` crate with: a client trait for all task operations; GTS type definitions for task and entry types; the engine enums (states, result codes, wait kinds, and others); request and response types; and error types that map to RFC 9457. The SDK **MUST** follow the ToolKit SDK-first pattern and **MUST** enforce the same authorization as the REST API.

- **Rationale**: Other gears integrate type-safely, without hand-written HTTP calls.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`

### 5.17 Search and Querying

#### Advanced Search

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-search`

The system **MUST** provide paged search over tasks with:

- **detail levels**: `tiny` (ID, state, type), `short` (adds priority, queue, assignee, times), `long` (adds input, output, context, progress, tags), `full` (adds a **bounded** event history, waits, entries, dependencies);
- filters on standard fields: state, assignee, queue, type, tags, resource references, age, urgency, priority, dependencies;
- sorting by creation time (default, newest first) or, for finished tasks only, by finish time, always with the task ID as tie-breaker so paging is stable. Other sort keys are rejected (`400`), because only these orders are served by an index at the capacity profile;
- cursor-based paging that follows the platform query conventions (ToolKit OData: `$filter`, `$orderby`, `$select`, `$top`, opaque cursor; no offset paging and no totals on a page), and a separate count-only query.

**Bounds**: search **MUST** stay bounded, whatever the number of stored tasks:

- **Time window**: a search that can return finished tasks covers at most `search_max_window_days` of creation time (default 31). Without a creation-time filter the last `search_default_window_days` (default 7) are searched. A wider range is rejected (`400`). A search for unfinished tasks only has no window.
- **Page size**: at most 100 items per page, 20 at `full`.
- **Count**: exact up to `search_max_count` (default 10,000). A larger result returns the cap and is marked as capped.

At `full` level each item embeds at most the most recent events (configurable, default 50). The complete history is read from the task's event history endpoint. Search inside payloads is out of scope (§4.2); large-scale analytics belongs to other gears fed by lifecycle events (§4.3).

- **Rationale**: Dashboards, debugging, and bulk management need rich search. Fixed sort orders, a time window, and capped counts keep every search on an index and its cost independent of the total number of stored tasks. Capping embedded history keeps response sizes predictable.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`, `cpt-cf-task-engine-actor-platform-admin`

### 5.18 Bulk Operations

#### Bulk Task Management

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-bulk-ops`

The system **MUST** support bulk cancel, bulk reprioritize, bulk retry (re-enqueue of `dead_lettered` or `failed` tasks, §5.8), and bulk move between queues, each on an explicit list of task IDs:

- **Per-task authorization**: each task is authorized on its own. The response lists an outcome per task — `succeeded`, `unauthorized`, `not_found`, or `invalid_state` — so allowed tasks proceed even if others are refused.
- **Size limit**: at most a configurable number of IDs per request (default 1,000); a larger request is rejected as a whole (`400`, `BATCH_TOO_LARGE`). Each task is applied independently, so one request never locks the whole batch.
- **Move**: only `queued`, `scheduled`, or `blocked` tasks can be moved; a task with an active lease returns `invalid_state`. A moved task takes the destination queue's settings at once. Source and destination **MUST** be in the same task domain.
- **Reprioritize**: changes priority and/or urgency; raising to `critical` also needs `expedite` (else `unauthorized`).

- **Rationale**: Operators fix many tasks at once. Per-task outcomes keep authorization strict; moving only unleased tasks keeps limits and leases consistent.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

### 5.19 Task Domain Routing

#### Multi-Database Storage Domains

- [ ] `p3` - **ID**: `cpt-cf-task-engine-fr-task-domains`

The system **MUST** support **task domains** that send different task classes to different databases ([ADR-0010](ADR/0010-task-domains.md)). A domain is configured with a name, a list of queue patterns (with wildcards), optional task type patterns, and a database. A `default` domain catches every other queue. Each domain has its own schema, migrations, connection pool, and retention.

**Routing by queue**: each task belongs to exactly one domain, the domain of its queue. Queue patterns are checked in config order and the first match wins; every operation (create, claim, search, retention, cache) uses the same rule. Dead-letter queues are not routed (§5.8).

**Type rules**: a domain **MAY** list task type patterns. A type that matches them (first matching domain wins) **MUST** only be placed in queues of that domain. Task types are registered at runtime, so this is checked on every create, bulk move, and re-enqueue, not at startup; a violation is rejected (`400`, `TASK_DOMAIN_MISMATCH`, naming the type, queue, and domains).

**Routing changes**: a change applies only to new tasks; existing tasks never move between databases. A queue change that would leave unfinished tasks in their old domain **MUST** stop startup with an error naming the queues. Moving a live queue to another domain needs draining its unfinished tasks first. A re-enqueue into a queue that now routes to another domain is rejected (`400`, `TASK_DOMAIN_MISMATCH`).

**One domain per atomic operation**: a claim, a parent with its children, and a predecessor with its successor **MUST** each stay in one domain; requests that cross domains are rejected (`400`).

**Events per domain**: each domain keeps its own outgoing events (§5.10). An incoming event that satisfies waits in several domains **MUST** be applied exactly once in each, without a cross-database transaction.

**Reads across domains**: a read filtered by queue, or by a type that has a type rule, uses that one domain. Any other read **SHOULD** cover all domains with consistent sorting and paging. DLQ inspection **MUST** cover all domains unless it is filtered by a source queue (§5.8).

- **Rationale**: Heavy system tasks can live apart from user-facing tasks, so they do not compete for I/O, and each class gets its own retention.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

### 5.20 Caching Layer

#### Cluster-Backed Ready-Task Cache

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-cluster-cache`

The system **MUST** support an optional ready-task cache on the Cluster gear's distributed cache, to speed up claims. The cache is only a hint: claims **MUST** stay correct, and **MUST** fall back to the database alone, when the cache is missing, stale, or unavailable.

- **Rationale**: High-throughput deployments can cut database reads per claim without a direct Redis dependency.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

### 5.21 Rate Limiting

#### Per-Tenant and Per-Queue Rate Limits

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-rate-limiting`

The system **SHOULD** support rate limits on task creation and claims, per tenant and per queue, enforced on each engine instance. Exceeded limits return `429` with `Retry-After`. The effective cluster-wide limit is therefore about the per-instance limit times the number of instances. A true cluster-wide limit is out of scope for the first releases, because it would put a shared counter on the hot path.

- **Rationale**: Rate limits stop noisy neighbors without tying the fast path to shared cluster state. They complement the engine's own load shedding (DESIGN §3.9).
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`

### 5.22 Recurring / Cron Tasks

#### Recurring Schedules

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-recurring`

**Status: deferred.** Recurring schedules are not part of the first releases (§4.3). This entry keeps the ID so the future work stays traceable.

The system **SHOULD** support recurring task definitions with cron-like schedules and configurable overlap policies (skip, enqueue, cancel previous). Before this is scheduled, it **MUST** be specified in full: schedule syntax and time zone, handling of runs missed while the engine was down, the exact meaning of each overlap policy, which subject creates the spawned tasks and how that is authorized, and where schedules are stored.

- **Rationale**: Built-in schedules remove the need for external cron jobs.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`

## 6. Non-Functional Requirements

### 6.1 Gear-Specific NFRs

> **Default guidelines**: Project-wide NFR baselines are defined in this repository's [architecture manifest](../../../docs/ARCHITECTURE_MANIFEST.md) and [guidelines/](../../../guidelines/). This section captures only gear-specific NFRs.

#### Claim Latency

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-latency`

A claim **MUST** complete within 50 ms (p50) and 200 ms (p99) under normal load, not counting long-poll waiting.

- **Threshold**: p50 ≤ 50 ms, p99 ≤ 200 ms over 5 minutes, with ≥ 100 concurrent workers claiming from ≥ 10 queues.

#### Throughput

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-throughput`

One task domain (one PostgreSQL database) **MUST** sustain at least 1,000 task creations and 500 claims per second under normal load. All engine nodes share the domain's database, so more nodes add availability and request capacity, not database write capacity; more write capacity comes from more task domains (§5.19).

- **Threshold**: Sustained for 60 seconds with ≤ 5% errors, with one domain database and both one and three engine nodes; the three-node run **MUST NOT** be slower than the one-node run. The claim figure **MUST** also hold with ≥ 100 workers claiming from the **same** queue.

#### Cache Degradation

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-availability`

If the Cluster cache is unavailable, all task operations **MUST** keep working on the database alone.

- **Threshold**: All lifecycle operations succeed within 2× normal latency while the cache is unreachable for 5 minutes.

#### Timer Accuracy

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-timer-accuracy`

Timeouts and deadlines (queue, assignment, execution, heartbeat, lifetime, SLA) **MUST** fire within 1 second under normal load.

- **Threshold**: 99% fire within 1 second of their due time at the capacity profile below (100,000 active tasks with their deadlines).

#### Tenant Isolation

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-tenant-isolation`

No API path **MUST** ever let one tenant see or change another tenant's tasks.

- **Threshold**: Zero cross-tenant leaks across all endpoints, verified by automated isolation tests with ≥ 3 tenants.

#### Request Payload Limits

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-payload-size`

A task's combined payload (input, context, output; uncompressed JSON) and each entry payload **MUST** be at most 64 KB (configurable). HTTP request bodies **MUST** be at most 1 MB (configurable), and bodies over 256 KB **MUST** be logged as warnings. A body whose declared `Content-Length` is over the limit **MUST** be rejected before it is read. The limit **MUST** also hold for bodies without a `Content-Length` (chunked or HTTP/2): the request is stopped as soon as the limit is passed, without reading the whole body. The engine is not a place for logs, blobs, or other large artifacts; clients **MUST** keep them in external storage and reference them by URL or ID.

- **Threshold**: Payloads over 64 KB are rejected with `413`. Bodies over 1 MB are rejected within 10 ms when `Content-Length` is present, or within one buffer read after the limit when it is not.

#### Storage Compression

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-compression`

Stored payloads (input, context, output, entry payloads, event payloads) larger than a configurable size (default 1 KB) **MUST** be compressed transparently ([ADR-0006](ADR/0006-blob-compression.md)).

- **Threshold**: ≥ 40% smaller on average for typical JSON over 1 KB, with < 1 ms per payload to compress and < 1 ms to decompress.

#### Search Latency

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-search-latency`

Search **MUST** stay fast at every detail level, including the cost of decompressing payloads at `long` and `full`.

- **Threshold**: Over 5 minutes at the capacity profile (500 million tasks, 100,000 active) on PostgreSQL, page size 50 (20 at `full`), within the search bounds (§5.17: supported sort keys, default time window, count cap), for a tenant with up to 1 million tasks in the window: `tiny`, `short`, and count p95 ≤ 100 ms; `long` p95 ≤ 300 ms; `full` p95 ≤ 500 ms.

#### Capacity Planning

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-capacity`

The system **MUST** meet all latency and throughput targets with this reference profile:

- up to **100,000 tenants**;
- up to **500 million stored tasks** (before retention);
- **100-day** default retention;
- up to **100,000 active tasks**;
- up to **5,000 parallel client connections**.

Index design, connection pooling, and retention cleanup **MUST** be validated against this profile.

- **Threshold**: Create, claim, heartbeat, complete, and search meet their targets with 500 million tasks, 100,000 active tasks, and 5,000 connections on PostgreSQL.

#### Graceful Shutdown

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-graceful-shutdown`

On shutdown the system **MUST** finish in-flight requests, save buffered progress, and close database connections, within a configurable deadline (default 30 seconds).

- **Threshold**: No dropped in-flight requests with ≤ 50 concurrent requests; shutdown finishes within 30 seconds.

#### Service Availability and Recovery

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-recovery`

The engine keeps no state outside its databases, so its availability and recovery follow the database. With a healthy database and at least two instances, losing one instance **MUST NOT** stop create, claim, heartbeat, or complete. After a restore from backup, the engine **MUST** start without manual repair and rebuild its timers and counters before it accepts claims.

- **Threshold**: Monthly availability ≥ 99.9% for create, claim, heartbeat, and complete while the database is available. Losing one of two or more instances loses no task and causes no claim gap longer than one lease. RPO equals the database backup RPO (target ≤ 5 minutes with point-in-time recovery); RTO ≤ 30 minutes after the database is restored.

#### Rolling Upgrade

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-rolling-upgrade`

The system **MUST** support rolling upgrades, with old and new versions running against the same database at the same time. Schema changes **MUST** stay backward-compatible for one release (add first, remove old columns only in a later release). Enum changes follow §5.12.

- **Threshold**: An upgrade from release N to N+1 under steady load has no failed request other than retried `503`s, no lost task, and no stuck lease.

#### Code Coverage

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-code-coverage`

The gear crate **MUST** reach at least 85% line coverage from unit tests, and at least 90% from unit and integration tests together.

- **Threshold**: Both figures measured by `cargo llvm-cov` in CI.

#### Security

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-security`

Every data access **MUST** go through the platform's tenant-scoped, PDP-authorized data layer; no code path may bypass it. Payloads **MUST** be treated as opaque data and never executed.

- **Threshold**: Zero raw database access outside migrations, and zero authorization bypasses, verified by static analysis and integration tests.

#### Data Protection and Privacy Controls

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-data-protection`

The engine does not process personal data of its own, but it stores caller identities, opaque payloads, and history. It **MUST** therefore provide these controls:

- **Access control**: all stored data is reachable only through tenant-scoped, authorized access (§5.11).
- **Encryption**: all API traffic uses TLS. Encryption at rest is provided by the database deployment and **MUST** be supported.
- **No payloads in logs or events**: payload and entry contents **MUST NOT** appear in logs, metrics, traces, error messages, or published events. Error `message` and `context` may echo payload data, so published events carry only the error's `domain`, `code`, `fault`, and `retryable`; the full error is read from the task.
- **Retention and deletion**: retention (§5.14) supports callers' erasure duties. Payloads are opaque, so a single field cannot be erased in place; erasure removes or rewrites the whole payload, and callers that need field-level erasure **MUST** keep such data outside payloads.
- **Caller responsibility for PII**: the engine does not know which payload fields are personal data. Callers identify and, where needed, encrypt them before submission.

- **Threshold**: All read paths are authenticated and tenant-scoped (isolation tests); zero payload contents found by log-scanning tests; TLS on all endpoints.

### 6.2 NFR Exclusions

- **Accessibility (WCAG)**: Not applicable — no user interface.
- **Internationalization**: Not applicable — English API and logs; payloads are opaque JSON.
- **Data Classification**: Payloads are caller-controlled opaque JSON that the engine does not read, index, or log. Callers are responsible for any personal data in them (§6.1 Data Protection).
- **Safety (ISO 25010 §4.2.9)**: Not applicable — information system with no physical interaction.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### Task Engine Client Trait (`task-engine-sdk`)

- [ ] `p1` - **ID**: `cpt-cf-task-engine-interface-client-trait`

- **Description**: Async trait with all task lifecycle operations, implemented in-process by the gear and by an HTTP client for remote use.
- **Breaking Change Policy**: Semver; trait changes are breaking.

#### GTS Type Definitions (`task-engine-sdk`)

- [ ] `p1` - **ID**: `cpt-cf-task-engine-interface-gts-types`

- **Description**: GTS base types for tasks and entries, and the engine enums, which follow the evolution rules in §5.12.
- **Breaking Change Policy**: Changing a GTS type ID is breaking. Renumbering, removing, or reusing an enum value is breaking; adding one is minor when clients handle unknown values. Adding schema fields is minor.

### 7.2 External Integration Contracts

#### Task Engine REST API Contract

- [ ] `p1` - **ID**: `cpt-cf-task-engine-contract-rest-api`

- **Direction**: provided by the gear
- **Protocol/Format**: HTTP/JSON REST API at `/api/task_engine/v1`
- **Compatibility**: Backward-compatible additive changes within a major version

## 8. Use Cases

### Create, Claim, and Execute a Task

- [ ] `p1` - **ID**: `cpt-cf-task-engine-usecase-create-execute`

**Actor**: `cpt-cf-task-engine-actor-api-consumer`, `cpt-cf-task-engine-actor-worker`

**Preconditions**:
- The Task Engine is running
- A worker is claiming from the target queue

**Main Flow**:
1. The API consumer creates a task with type, queue, priority, and input
2. The system validates and stores it and publishes `taskCreated`
3. A worker claims the task and gets a lease
4. The worker starts the task and sends heartbeats to keep the lease
5. The worker reports progress, may save checkpoints, and may write entries
6. The worker completes the task with a result
7. The system publishes `taskCompleted`

**Postconditions**: The task is `completed` with its result; its full history is recorded.

**Alternative Flows**:
- **Worker stops heartbeating**: the lease expires; the task is rescheduled with backoff (§5.6); the worker gets one blacklist strike
- **Worker stops early on purpose**: it releases the task (§5.6 Voluntary Release); no budget or strike is used
- **Cancellation requested**: the worker sees it on a heartbeat and ends the task as `cancelled`
- **Retries used up**: the task moves to the dead-letter queue

### Task Waits for External Event

- [ ] `p2` - **ID**: `cpt-cf-task-engine-usecase-event-wait`

**Actor**: `cpt-cf-task-engine-actor-worker`, `cpt-cf-task-engine-actor-event-broker`

**Preconditions**:
- The task is `running`

**Main Flow**:
1. The worker arms an event wait (event type, subject, optional match expression)
2. The worker starts the external action, then parks the wait with a checkpoint
3. The task moves to `waiting` and the worker is free
4. The external system publishes a matching event through the Event Broker
5. The condition is satisfied and the task goes back to `queued`
6. A worker claims it and continues from the checkpoint, with the event header (§5.4)

**Postconditions**: The task resumed with the event header; the wait is recorded in its history.

**Alternative Flows**:
- **Event arrives before parking**: park returns the event header at once and the task keeps running

### Retry with Backoff After Transient Failure

- [ ] `p1` - **ID**: `cpt-cf-task-engine-usecase-retry`

**Actor**: `cpt-cf-task-engine-actor-worker`

**Preconditions**:
- The task has a retry policy

**Main Flow**:
1. The worker completes the task with a retryable error
2. The system classifies the error as retryable and budget remains
3. The task moves to `scheduled` with a backoff delay
4. When the delay ends, the task becomes `queued`
5. A worker claims and runs it again

**Postconditions**: The task was retried after a delay; each attempt is in its history.

**Alternative Flows**:
- **Retries used up**: the task moves to the dead-letter queue and `taskDeadLettered` is published

## 9. Acceptance Criteria

- [ ] The lifecycle create → claim → start → heartbeat → progress → complete runs end to end with correct states and events
- [ ] Two workers can never claim the same task
- [ ] An expired lease returns the task within 1 second of the deadline
- [ ] Every timeout type fires within 1 second and applies the right outcome
- [ ] Lifetime and SLA deadlines still fire after retries, lease-expiry requeues, and waits; an execution timeout fires on time even while the worker heartbeats, and is terminal and not counted against the worker
- [ ] A tenant at its per-tenant limit does not stop other tenants' tasks from being claimed; a claim returns `429` (`CONCURRENCY_LIMIT`) only when nothing was assignable and a task was skipped for capacity
- [ ] On several instances, a task created on one instance is claimed by a worker waiting on another within the poll interval
- [ ] Retries use the right backoff; `attempt` grows only on claims
- [ ] The first retry waits `base`; no wait ever exceeds the maximum interval, also with jitter and with a very large retry count
- [ ] Tasks that run out of retries are dead-lettered and can be re-enqueued
- [ ] All four wait kinds suspend and resume tasks; a waiting task holds no slot, lease, or assignee, resumes through `queued` and a new claim, and that claim does not start a new attempt
- [ ] Ten `fault=task` failures in a row do not blacklist a worker; ten lease expiries or `fault=worker` failures do
- [ ] Event waits apply their match expressions
- [ ] A caller without `consume` on the topic, the event type, or the subject type of an event condition cannot set or arm it (`403`); a condition without a subject type is rejected (`400`) unless the event type allows exactly one; a policy decision whose conditions the engine cannot apply is refused (`403`), and one it can apply stops events outside it from matching; an event condition never waits longer than the configured maximum; a satisfied event condition stores and returns only the event header
- [ ] A wait on a non-waitable topic is rejected (`400`); waits in ≥ 1,000 tenants never change the broker subscription
- [ ] With two task domains, an event matching waits in both is applied exactly once in each, also after a crash and redelivery, and each domain's outgoing events commit with its state changes
- [ ] Concurrency limits (queue and type, global and per tenant) are never exceeded under concurrent claims
- [ ] Per-tenant queue depth limits are never exceeded at admission under concurrent creates, the queue-wide and domain-wide limits are exceeded by at most one sampling interval of admissions, and no tenant ever has more than `tenant_max_deferred_tasks` deferred tasks (including `waiting`); a full limit returns `429` (`QUEUE_DEPTH_LIMIT`) with `Retry-After` and shows no other tenant's usage; replays and requeues are never refused
- [ ] A type with a derived dedup identity accepts creates with or without a key (a sent key has no effect on the identity), answers a duplicate with `409`, and gives callers with different `authz_scope` values (or different subjects under `subject` scope) independent tasks
- [ ] Supersession: a newer order value cancels the in-flight task (result `superseded`, `superseded_by` set, capacity freed) atomically with the new insert; an equal value is a replay; a lower value is rejected `STALE_ORDER`, even after the newer task finished; a caller without `cancel` on the old task gets `403` and nothing changes
- [ ] Tenant A cannot see or change tenant B's tasks
- [ ] Every permission boundary is enforced through PDP decisions
- [ ] Bulk operations authorize each task and return per-task outcomes
- [ ] Bulk requests over the size limit are rejected (`BATCH_TOO_LARGE`)
- [ ] Creating a task with an unknown type is rejected (`UNKNOWN_TASK_TYPE`)
- [ ] Concurrent payload writes near the limit never store an oversized payload or lose an accepted write
- [ ] Input that violates the registered schema is rejected
- [ ] The combined payload limit (input + context + output, uncompressed JSON) applies on create, checkpoint, and completion
- [ ] Parent/child tasks track completion and cancel children, including on forced parent cancellation
- [ ] Blocked tasks cannot be claimed until their predecessors succeed; every dependency change is checked for cycles
- [ ] Without the parent's lease or `update`, a caller cannot add a child; without `read`, it cannot name a predecessor; both get the same `404` as a missing task
- [ ] Cross-domain claims, parent/child links, and dependencies are rejected (`400`)
- [ ] A type with a type rule is rejected (`400`, `TASK_DOMAIN_MISMATCH`) in a queue of another domain, also when the type was registered after startup; one dead-letter queue serves queues of several domains
- [ ] Cursor paging stays stable while tasks are being added
- [ ] A search with an unsupported sort key or a time range over the window is rejected (`400`); a search without a time range covers the default window; a count over the cap is returned as capped
- [ ] A duplicate key returns `409` with the existing task (in any state, including after it checkpointed, was moved or dead-lettered, or finished); the same key with a request different from the original returns `400`; a missing key returns `428`; after the key window, the key of a finished task creates a new task, while the key of an unfinished task is never released
- [ ] Two subjects in one tenant using the same key get two independent tasks; neither receives the other's task
- [ ] With mode `milestones`, a percent-only update publishes no `taskProgressed` and a stage change publishes exactly one at the next flush; with mode `none`, none are published
- [ ] Search by resource reference returns exactly the matching tasks in the caller's tenant scope
- [ ] An SLA breach publishes `slaBreached` and the task keeps running
- [ ] Lifetime timeout and used-up retries end the same way (dead-lettered if there is a DLQ, else failed)
- [ ] Lease expiries and assignment timeouts use only `max_liveness_requeues`: with `max_retries = 3`, a task survives more than 3 lease expiries and still gets 3 retries after reported failures
- [ ] A worker that loses N leases on one queue in one outage (all detected within one lease duration) gets exactly one blacklist strike
- [ ] A long-poll timeout over `claim_max_wait_secs` is shortened to the cap; claim and heartbeat responses carry `heartbeat_interval_secs` = one third of the lease (at least 1)
- [ ] Graceful shutdown finishes within 30 seconds
- [ ] After a lease expires and the task is claimed again, the old worker's complete, checkpoint, progress, and heartbeat calls return `409` (`LEASE_LOST`), and the new holder's result is kept
- [ ] After reassignment, the old holder's heartbeat returns `LEASE_LOST` and the epoch has increased; the response has no token, and only the named identity can accept
- [ ] A valid token sent by another subject returns `LEASE_LOST`
- [ ] A progress update buffered under a lease that is then lost is never saved
- [ ] Repeating a completion with the same token and result returns `200` with the stored outcome; a different result returns `400`
- [ ] A claim without a valid `worker_id` is rejected (`400`); two processes under one principal with different `worker_id`s are blacklisted independently
- [ ] A release returns the task to `queued` (or `scheduled` after a delay) at once, frees its slots, and changes neither retry budget, `attempt`, nor the worker's blacklist count
- [ ] An event published after an arm call returns satisfies the armed condition; the following park returns the data and the task keeps running under the same lease
- [ ] A parent parked on a children wait resumes exactly once, after its last child finishes, with the count of children per terminal state
- [ ] On a `round_robin` queue, with tenant A holding 1,000 queued tasks and tenant B holding 10, B's tasks are claimed between A's, not after all of them, even if A uses priority `0` and B `1000`
- [ ] A `critical` task is claimed before `normal` tasks of its queue; creating one without `expedite` returns `403`
- [ ] A subject allowed to create only type A gets `403` for type B; a worker allowed to claim only queue Q never receives a task from another queue
- [ ] A tenant-owned type is unknown to other tenants and is rejected on an `operated_by: platform` queue
- [ ] A type with a lease below the minimum cannot be created, and a queue with one refuses new tasks; the error names the setting
- [ ] A task whose request, type, and queue set no execution timeout gets the engine default; an execution timeout above the trust limit is rejected on the request, makes a type non-creatable, and makes a queue refuse new tasks; a task whose handler hangs in a heartbeating worker process ends `timed_out` at its execution deadline
- [ ] A platform worker gets no task of a tenant behind a barrier from a queue without `operated_by: platform`
- [ ] After `claim` is revoked, the worker's next call returns `403` and the task is back in the queue with no budget, attempt, or blacklist change
- [ ] Re-enqueuing a `dead_lettered` or `failed` task resets both retry budgets, clears the old result, restarts the lifetime timeout, and returns `409` if another task now holds its dedup identity
- [ ] For a `creator_verified` type, a creator who may not perform a checked action on a referenced resource gets `403` and no task is stored, even when the type's workers may perform it; a PDP decision the engine cannot fully evaluate is also refused; references not covered by a check are stored and published with `verified = false`
- [ ] A tenant-owned type declaring `authority.mode = worker` cannot be created
- [ ] With the per-tenant event-wait limits full, arming or setting another event wait returns `429` (`EVENT_WAIT_LIMIT`) and the worker keeps its lease; one tenant at its limits does not raise event-wait resume latency for another tenant above the timer-accuracy bound
- [ ] A match expression that exceeds its evaluation budget never matches; after the configured number of consecutive failures its condition resolves with reason `eval_budget_exceeded` and applies `on_timeout`
- [ ] Lifecycle events of a task in a tenant behind a barrier are published under the engine's publishing subject with that tenant; with the publishing grant removed, task operations keep working, the history stays complete, and an alert fires
- [ ] Concurrent dependency additions that would together form a cycle (including cycles longer than two tasks) never all succeed
- [ ] A dependency added, a task created with predecessors, or a child created while the linked task ends concurrently is never left `blocked` on an ended predecessor and never orphaned under an ended parent
- [ ] Counter reconciliation that runs during concurrent claims, releases, and admissions never lets a concurrency or queue-depth limit be exceeded
- [ ] Every terminal path (completion, failure, every cancel mode, supersession, each timeout, liveness exhaustion, wait failure, `dependency_failed`, hard-retention cancel) leaves no live-only rows, frees all counters and the dedup identity per its window, records `terminal_at`, and reaches children and successors

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| ToolKit framework | REST operations, security context, tenant-scoped data access, client hub, canonical errors | p1 |
| AuthN Resolver gear | Validates bearer tokens and provides the security context; client credentials give platform workers and the engine's broker subscription a root-tenant service identity | p1 |
| AuthZ Resolver gear | Authorization decisions and query constraints (through `authz-resolver-sdk`) | p1 |
| Tenant Resolver gear | Tenant hierarchy, subtree, and barrier rules | p1 |
| Resource Group Resolver gear | Resource-group hierarchy for group-scoped policies | p2 |
| GTS Registry | Registers and resolves task, queue, and entry types and instances; applies registration policy and per-tenant visibility (§5.12 Task Type and Queue Trust) | p1 |
| Event Broker gear | Receives lifecycle events and delivers inbound events for event waits. The engine publishes as its publishing subject, which needs `produce` grants on its topic, event types, and subject types for every tenant that can own tasks, including tenants behind a barrier (§5.10 Publishing identity). It consumes waitable topics as a separate platform service subject with `consume` grants on them. It keeps running without the broker: events wait in the outbox, but event waits cannot resolve until it is back | p1 |
| ToolKit DB layer | Database access, connection pools, migrations, and the event outbox | p1 |
| Cluster gear | Distributed cache for the optional ready-task cache ([Cluster PRD](../../system/cluster/docs/PRD.md)) | p3 |
| CEL evaluator | Evaluates match expressions for event waits under a cost budget enforced inside the evaluator | p2 |

## 11. Assumptions

- The ToolKit DB layer supports PostgreSQL, MariaDB/MySQL, and SQLite behind one API
- Authentication is done by the platform; the engine receives a ready security context
- The AuthZ Resolver (PDP) is available and configured
- The Tenant Resolver provides tenant hierarchies
- The GTS registry is available; unknown task types are rejected, and input is validated only when the type registers a schema
- The Types Registry registration policy keeps instances of `gts.cf.core.tasks.queue.v1~` platform-owned (§5.12 Task Type and Queue Trust)
- The Event Broker, when available, delivers events at least once
- The Cluster gear, when configured, provides a distributed cache; the engine does not use Redis directly
- ToolKit exposes the compiled access scope in a stable, serializable form so it can be hashed; until this is confirmed, the `authz_scope` dedup scope is unavailable and only `subject` and `tenant` work (§5.1 Derived Dedup Identity)

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Background timer processing falls behind under load | Tasks time out late | Timer work is split across instances; backlog metrics and alerts |
| Ready-task cache is stale after a Cluster outage | Wasted claim attempts | Cache is only a hint; rebuilt on reconnect; database fallback |
| High event volume slows event-wait matching | Waiting tasks resume late | Only waitable topics consumed; indexed probe (type, tenant, subject) before any expression; consumer lag alert |
| Event waits used to read events the caller may not see | Disclosure across authorization boundaries | Caller's `consume` checks on topic, event type, and subject type at set/arm, with policy conditions applied to every event; capped wait lifetime; only the event header is stored (§5.4) |
| Expensive match expressions | Slow event processing | Expressions checked when set; a cost budget enforced inside the evaluator; repeated budget failures resolve the condition (§5.4) |
| One tenant arms many broad event waits | Event matching, and so wake-ups, slow down for every tenant | Per-tenant, per-type, and subject-less condition limits (§5.4 Event Wait Condition) |
| A worker acts with more rights than the task's creator | Disclosure or change of a resource the creator cannot reach, inside the tenant | Creator checks at creation, `verified` references, `worker` mode only for platform-owned types (§5.11 Creator authority) |
| The publishing subject lacks a grant, or a barrier blocks it | Lifecycle events missing for some tenants | Dedicated publishing subject with documented grants; history unaffected; alert on refused publishes (§5.10) |
| Concurrent links race with each other or with a task ending | Dependency cycles, successors blocked for ever, orphaned children | Serialized dependency changes per tenant; links lock the linked task (DESIGN §4.6) |
| Dead-letter queues grow unnoticed | Unbounded storage | DLQ size metrics and alerts; retention applies to dead-lettered tasks |
| A tenant marks all its work most important | Other tenants starve | Priority is tenant-local; `critical` needs `expedite` (§5.2) |
| A lease token leaks | Another principal changes the task | Random token bound to the holder (§5.6) |
| A caller links to someone else's task | Blocked parent, cascaded cancel, probing | Link authorization, uniform `404` (§5.9) |
| A tenant puts a platform task type into a privileged queue | Privilege escalation through platform workers | Type and queue constraints from the PDP; `operated_by` and `accepted_types` (§5.11, §5.12) |
| A registrant declares unsafe settings | Endless retries, leases that always expire | Engine limits on every declared value (§5.12) |
| Many workers race for the head of one queue | Claim throughput collapses | ToolKit secure claim primitive (counter-first guarded updates, no task write for a full scope), batch claims, node-local claim coalescing, and sharded counter rows (DESIGN §4.6, ADR-0008, `proposed` until the ToolKit primitive exists and the benchmark passes); the throughput NFR measures this case |
| Heartbeat writes grow with running tasks | Write load and table bloat on the live table | Worker-session leases: one heartbeat write per worker process and domain (ADR-0017) |
| Stalled ("zombie") worker writes after its lease expired | Double execution, overwritten result | Lease fencing (§5.6); at-least-once contract with idempotent workers |
| Clients that poll, time out, and resubmit | Unbounded backlog, noisy tenants | Queue depth limits at admission with `429` and `Retry-After` (§5.2) |
| Deduplication or supersession crosses an access boundary | Disclosure or unauthorized cancellation | Scope inside the identity; explicit opt-in for `tenant` scope; `cancel` check on the replaced task (§5.1) |
| Buffered progress lost on crash | Latest progress missing | Configurable flush interval; accepted trade-off for lower write load |
| Oversized payloads | Rejected creates, memory pressure | 64 KB payload limit, 1 MB body limit, external storage for large data |

## 13. Open Questions

- Is one indexed probe per event (or batch) enough for the volume of the waitable topics, or is broker-side filtering needed?
- When will the Event Broker offer reads by position (post-MVP `GET /v1/events`), so a resumed worker can fetch a matched event's data with its own rights?
- What is the maximum depth of parent/child hierarchies?
- Should retried tasks prefer the same worker ("sticky" assignment)?
- Does ToolKit expose a stable serialized form of the compiled access scope for the `authz_scope` dedup identity, or must the engine build it from the raw PDP constraints?
- Will AuthN offer a way to act on behalf of a subject (token exchange)? Until then workers act as themselves, and creator rights are proven only at creation (§5.11 Worker Principals, Creator authority).
- Will the Event Broker let a root-tenant producer publish into tenants behind a barrier by policy alone, or does its tenant check need an explicit barrier option for platform producers (§5.10 Publishing identity)?
- May platform workers cross tenant barriers for task data on `operated_by: platform` queues, and what audit does AuthZ require?
- When will Types Registry validation hooks be available, so invalid types are rejected at registration instead of at first use?

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/)
