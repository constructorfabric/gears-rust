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
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

**Task Engine** is a CF/Gears subsystem (gear) that provides a distributed, multi-tenant task execution engine — enabling API clients and workers to create, queue, claim, execute, suspend, resume, and complete structured units of work. The gear manages the full lifecycle of tasks across a cluster of nodes, with queue-based dispatch with concurrency control, atomic claiming with leases, durable wait conditions, structured input/output, retry policies, parent/child dependencies, and extensible task typing via the Global Type System (GTS).

### 1.2 Background / Problem Statement

Distributed systems that need to orchestrate real work execution across services, workers, and tenants face several structural problems:

1. **No unified task lifecycle** — Teams build ad-hoc job queues, cron schedulers, and status-tracking tables independently. Each implementation handles states, timeouts, retries, and progress reporting differently, creating inconsistent behavior and duplicated effort.

2. **Missing multi-tenant isolation** — Most task frameworks treat all work as belonging to a single namespace. In multi-tenant platforms, tasks belonging to one tenant must be invisible to another, queue access must be scoped by role and tenant, and rate limiting and retention policies may vary per tenant.

3. **No durable suspension model** — Tasks frequently need to pause and wait for external conditions: a timer, an event from another system, or user-provided input. Without first-class wait conditions, developers build polling loops, callback hacks, or split tasks into chains — all fragile and hard to monitor.

4. **Lack of extensible task typing** — Task payloads, input schemas, and result structures vary by domain. Hard-coded enums and rigid schemas force coordinated releases when new task types appear. A type-extensible model allows vendors and plugins to introduce new task variants without breaking existing consumers or requiring database migrations.

5. **Timeout and health monitoring gaps** — Without centralized timer management, tasks can stall indefinitely. Queue timeouts, assignment deadlines, execution limits, heartbeat monitoring, and SLA deadlines must be enforced consistently across a cluster.

6. **Retry fragility** — Most ad-hoc implementations lack structured retry policies. Transient failures either cause permanent task loss or trigger unbounded retries with no backoff, flooding downstream systems.

### 1.3 Goals (Business Outcomes)

- Provide a single, reusable distributed task execution engine for the entire CF/Gears platform, eliminating per-gear ad-hoc job-queue implementations
- Enable multi-tenant task isolation with configurable retention, queue access policies, and tenant-hierarchy-aware scoping
- Support durable task suspension via first-class wait conditions (timer, event, input) so tasks can pause and resume without polling or callback chains
- Deliver an extensible task type model via GTS so that new task types, input/output schemas, and entry types can be introduced by vendors and plugins without API or DB schema changes
- Ensure reliable execution through configurable retry policies with backoff, dead-letter handling, and structured failure classification
- Provide queue-based dispatch with concurrency/semaphore control, atomic claim/lease semantics, and heartbeat-based liveness detection
- Enable parent/child task hierarchies and predecessor/successor dependencies for multi-step execution
- Expose a REST API for full task lifecycle operations, queue introspection, progress tracking, and administrative management
- Offer a public SDK crate for programmatic integration by other gears
- Maintain an immutable event history and audit trail for all state transitions, assignments, waits, retries, and completions

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Task | The core work unit with type, status, priority, structured input/output, metadata, timestamps, owner, and full lifecycle |
| Task Entry | A fine-grained execution record within a task: work performed, checks, observations, measurements, notes, or issues encountered during execution |
| Queue | A GTS well-known instance (base type `gts.cf.core.te.queue.v1~`) defining a named channel with concurrency limits (global and per-tenant), default priority, timeout values, retry policy, and optional dead-letter queue. Tasks are enqueued into queues and workers claim work from them |
| Worker | A service or process that claims tasks from queues, executes work, reports progress, and completes tasks with structured results |
| Wait Condition | A durable suspension point — a first-class domain model element defining a condition that must be satisfied before a waiting task resumes. Three built-in kinds: timer, event, input |
| Assignment | The binding of a task to a worker, including atomic claim, lease expiry, heartbeat tracking, and attempt counter |
| Lease | A time-bounded lock on an assigned task; if the worker fails to heartbeat before the lease expires, the task is returned to the queue |
| Heartbeat | A periodic signal from a worker extending its lease and receiving control signals (cancel requests, resolved wait conditions) |
| Checkpoint | A durable snapshot of intermediate task state enabling resume after interruption |
| Dependency | A predecessor/successor relationship between tasks; a task with unmet dependencies remains blocked until prerequisites complete |
| Retry Policy | A per-task configuration defining max attempts, backoff strategy (fixed, linear, exponential), interval, jitter, and retryable vs terminal error classification |
| Dead Letter Queue | A destination for tasks that exhaust their retry budget, preserving them for inspection and manual re-enqueue |
| Task Type | A GTS-typed classification that determines input/output schemas, wait condition types, and processing behavior |
| Concurrency Limit | A configurable cap on how many tasks from a queue (or of a type, tenant, or resource) can be executing simultaneously |
| Task Domain | A named storage partition that maps GTS task type or queue patterns (with wildcard support) to a specific database. A task resolves to exactly one domain via a deterministic, config-order first-match rule applied identically across all operations (see §5.19). Enables physical separation of task data — e.g., system tasks vs user-visible tasks — with independent databases, retention policies, and query isolation |
| Cluster Gear | The platform-level coordination service ([Cluster PRD](../../system/cluster/docs/PRD.md)) providing distributed cache, leader election, distributed locks, and service discovery. The Task Engine uses the Cluster gear's distributed cache for optional ready-task caching instead of depending on Redis directly |

## 2. Actors

### 2.1 Human Actors

#### Platform Administrator

**ID**: `cpt-cf-task-engine-actor-platform-admin`

- **Role**: An operator managing the Task Engine gear in a multi-tenant environment — configuring queues and concurrency limits, managing tenant retention policies, monitoring cluster health, inspecting dead-letter queues, and resolving wait conditions (inputs).
- **Needs**: Administrative APIs for queue management, tenant configuration, cluster status, DLQ inspection, and operational monitoring.

#### API Consumer (Developer)

**ID**: `cpt-cf-task-engine-actor-api-consumer`

- **Role**: A developer or tool operator who creates tasks, monitors progress, resolves wait conditions (inputs), queries task state, and inspects task entries and event history via the REST API.
- **Needs**: Full CRUD API for tasks and task entries; wait condition resolution; progress tracking; structured result retrieval; search and filtering.

### 2.2 System Actors

#### Worker (Task Consumer)

**ID**: `cpt-cf-task-engine-actor-worker`

- **Role**: A service or process that claims tasks from queues via atomic claim/lease, executes work, reports heartbeat and progress, creates task entries, checkpoints intermediate state, sets wait conditions when external input is needed, and completes tasks with structured results.
- **Needs**: Claim API, heartbeat/lease-renewal API, progress/checkpoint/complete APIs, wait condition API, task entry API.

#### Event Broker

**ID**: `cpt-cf-task-engine-actor-event-broker`

- **Role**: The Event Broker gear that receives task lifecycle events for downstream processing by other gears. Also a source of inbound events that can satisfy event-type wait conditions on tasks.
- **Needs**: Structured event payloads with task metadata, result codes, and resource references. Inbound event delivery for wait condition resolution.

#### Storage Backend

**ID**: `cpt-cf-task-engine-actor-storage-backend`

- **Role**: One or more relational databases (PostgreSQL, MariaDB/MySQL, or SQLite — selected via ToolKit `DbManager` DSN configuration) storing tasks, task entries, wait conditions, timers, dependencies, retry state, event history, and tenant configuration. Multiple databases can be configured via task domains to isolate storage for different task type groups (e.g., system tasks vs user-visible tasks).
- **Needs**: Tenant-scoped queries; transactional writes; schema migrations via SeaORM migrations; per-domain database routing.

## 3. Operational Concept & Environment

> **Note**: Runtime, OS, architecture, lifecycle policy, and gear integration patterns are defined in this repository's foundational documents — the [architecture manifest](../../../docs/ARCHITECTURE_MANIFEST.md) and [guidelines/](../../../guidelines/). This section captures only this gear's deviations.

### 3.1 Gear-Specific Environment Constraints

- Requires an async Rust runtime (tokio) for concurrent request handling, timer processing, signal dispatch, and database operations
- All database access uses the ToolKit `DbManager` for DSN resolution, connection pooling, and per-gear database configuration; the `SecureConn` transaction runner enforces tenant scoping at the ORM level
- Uses the ToolKit DB layer (`cf-gears-toolkit-db` / SeaORM) with support for three database backends selected via DSN scheme: PostgreSQL, MariaDB/MySQL, SQLite
- Multiple databases can be configured via task domains — each domain maps a set of GTS task types to a dedicated database instance, enabling physical separation of system tasks from user-visible tasks (or any other classification)
- Optional distributed cache integration via the Cluster gear for a high-performance ready-task cache layer, with graceful degradation to database-only dispatch when the cache is unavailable

## 4. Scope

### 4.1 In Scope

**Definition layer**:
- Task types as GTS extension points with structured input/output schemas
- Wait condition kinds as first-class domain model elements (timer, event, input)
- Custom fields via GTS hybrid storage (base columns + JSONB extension)

**Execution layer**:
- Full task lifecycle: create, queue, claim, start, heartbeat, progress, checkpoint, complete, cancel
- Queue-based dispatch with concurrency/semaphore control
- Atomic assignment with lease/heartbeat semantics
- Configurable retry policies with backoff and dead-letter handling
- Multi-type timeout enforcement (queue, assignment, execution, heartbeat, lifetime, SLA)
- Durable wait conditions with first-class domain model resume criteria
- Parent/child task hierarchies and predecessor/successor dependencies
- Cancellation semantics (requested, graceful, forced)
- Idempotent task creation via caller-supplied idempotency key
- Delayed / scheduled task enqueue (`run_after`)

**Tracking layer**:
- Task entries (fine-grained immutable task execution records)
- Progress tracking (percent, milestones, counters, current stage, ETA)
- Structured result/output with typed error classification
- Immutable event history and audit trail for all state transitions
- SLA deadlines with breach detection
- Transparent JSON blob compression for storage efficiency (blobs > 1 KB)

**Operations layer**:
- REST API for all domain operations
- Multi-tenant access control via JWT-based RBAC with ToolKit authorization stack
- Tenant hierarchy resolution and scope narrowing
- Search and querying (by status, assignee, age, queue, metadata, dependencies)
- Bulk operations (cancel, reprioritize, retry, move queue)
- Configurable event publishing policies per GTS task type pattern
- Configurable data retention and scheduled cleanup
- Health probes (liveness, readiness) and Prometheus metrics
- SDK crate for programmatic integration
- Tags, labels, and classification for flexible filtering
- Task domain routing — multiple databases per instance, routed by GTS task type

### 4.2 Out of Scope

- Workflow orchestration engine (DAG orchestration, conditional branching, saga patterns) — the Task Engine provides dependencies and parent/child hierarchies, not workflow engines
- UI or dashboard — consumers build dashboards on the REST API
- Message broker replacement — the Task Engine manages task lifecycle, not pub/sub messaging
- File/blob storage for task artifacts — tasks carry JSON payloads; large artifacts are referenced by URI
- Comments / collaboration threads — use a separate collaboration gear
- Attachments / evidence — use external storage with URI references in task entries
- Notifications / subscriptions — use the Event Broker gear for downstream notification routing
- Cluster coordination — the Task Engine does not implement its own cluster mesh; use the Cluster gear for cross-instance coordination when needed
- Per-tenant database sharding — horizontal partitioning of task data across multiple database instances by tenant; single-database deployments are the target for initial releases
- Cross-tenant administrative queries — admin searches that span tenant boundaries (all queries are scoped to the caller's tenant hierarchy)
- Large payload storage — the Task Engine enforces a maximum payload size per task/entry (default 64 KB); clients must use external file storage (e.g., File Storage gear) and reference uploaded content by URL/ID
- Approval wait condition — human-in-the-loop approval gates (manager sign-off, security review, change control) are deferred; the input wait condition can serve basic approval-like flows by collecting a structured decision payload
- Slow-query tracing — generic slow-query tracing (logging, SQL normalization, rotation) should be provided by a platform-wide observability component rather than implemented per-gear; the Task Engine will integrate with such a component when available
- Custom payload search and indexing — searching or filtering by fields within task-type-specific JSON payloads (e.g., indexing arbitrary payload keys per GTS task type) is out of scope; search is limited to standard task metadata fields (status, queue, type, tags, priority, assignee, age, dependencies)

## 5. Functional Requirements

> **Testing strategy**: All requirements verified via automated tests (unit, integration, e2e) targeting 90%+ code coverage unless otherwise specified.

### 5.1 Task — Core Work Unit

#### Task Creation

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-task-create`

The system **MUST** accept a task definition containing: type (GTS type identifier), queue, priority, input payload (JSON), optional context (JSON), optional owner, timeout values (queue, assignment, execution, heartbeat, lifetime), optional SLA deadline, optional retry policy, optional `run_after` timestamp for delayed enqueue, cancellable flag, tags, optional parent task ID, and optional predecessor dependencies (§5.9). The system **MUST** generate a unique identifier, validate the task type and input against the GTS registry (if a schema is registered), validate cross-references (parent and dependencies must resolve to the same task domain, §5.19; the dependency graph must be acyclic, §5.9), insert the task, set applicable timers, publish a `taskCreated` event, and emit a `newTask` signal to wake waiting workers. If `run_after` is specified, the task **MUST** enter a `scheduled` state and transition to `queued` at the specified time. If the task is created with unmet dependencies it **MUST** enter a `blocked` state (§5.9).

**Payload size enforcement**: The total serialized size of a task's structured payload — `input + context + output` combined — **MUST NOT** exceed a configurable maximum (default 64 KB). The measured form **MUST** be the **uncompressed canonical JSON** encoding (measured before transparent storage compression, so the limit is stable and independent of compressibility). This aggregate limit **MUST** be enforced on **every write that can change the payload**, not only on the input at creation: task creation (input + any provided context), checkpoint / context update (§5.1 Task Checkpoint), and completion (when `output` is set). Any write whose resulting merged `input + context + output` exceeds the limit **MUST** be rejected with `413 Payload Too Large`, leaving the task's stored payload unchanged. Clients requiring larger data **MUST** store artifacts in external storage (e.g. file storage) and reference them by URL/ID in the task payload.

- **Rationale**: Task creation is the entry point for all work; validation, event publishing, and delayed scheduling ensure correctness and observability.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`, `cpt-cf-task-engine-actor-worker`

#### Structured Input/Output

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-structured-io`

Every task **MUST** carry a structured input payload (set at creation) and a structured result (set at completion). The result **MUST** include: result code (closed `TaskResultCode` enum), output payload (JSON), optional error (with domain, code, message, context), and optional warnings list. The input and output schemas **MUST** be defined per task type via GTS and validated at boundaries when a schema is registered.

- **Rationale**: Structured results enable downstream automation, reporting, and error classification; without them tasks become "status + comments".
- **Actors**: `cpt-cf-task-engine-actor-worker`, `cpt-cf-task-engine-actor-api-consumer`

#### Idempotent Task Creation

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-idempotent-create`

The system **MUST** support idempotent task creation via a mandatory caller-supplied idempotency key. Every task creation request **MUST** include an `idempotency_key` (string). The idempotency key **MUST** be indexed and unique within a tenant scope (`(owner_tenant_id, idempotency_key)`), and deduplication semantics are defined as follows:

- **Existing record (any state)**: If a task with an identical key already exists — **regardless of whether it is in a terminal or non-terminal state** — the system **MUST** return the existing task rather than creating a duplicate (HTTP `200 OK` referencing the existing task, not `201 Created`). The unique index makes a second `INSERT` with the same key impossible, so "return the existing task" is the defined behavior for the full lifetime of the record, terminal states included.
- **Different payload, same key**: If the incoming request carries the same key but a **different** task definition than the stored record, the system **MUST** reject it with `409 Conflict` (`error.conflict`) rather than silently returning the mismatched existing task. Payload equivalence is determined by comparing the canonical-JSON encoding of the request's defining fields (type, queue, input, context) against the stored task.
- **Idempotency window**: Deduplication is bounded by data retention — once the original task is removed by retention (§5.14), a replayed key creates a new task. Callers **MUST NOT** rely on deduplication beyond the retention horizon (see DESIGN §4.5 Idempotency window).

- **Rationale**: Idempotency is essential once tasks are created by APIs, events, retries, or integrations — it prevents duplicate work on caller retry. A mandatory caller-supplied key is simpler and more predictable than implicit checksumming, and gives the caller full control over deduplication semantics. Returning the existing record in every state (not only non-terminal) and rejecting mismatched payloads removes the previously unspecified terminal-record and key-reuse behavior.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`

#### Task Cancellation

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-task-cancel`

The system **MUST** support three cancellation modes: (1) **requested** — sets `cancel_requested` flag, worker detects via heartbeat and transitions the task to `cancelled` gracefully; (2) **graceful** — system attempts cooperative shutdown with a configurable grace period; (3) **forced** — directly transitions the task to `cancelled` with a `forciblyCancelled` result, bypassing worker acknowledgment. Cancellation **MUST** propagate to child tasks if configured (see §5.9 for forced-cancel/child interaction). The system **MUST** publish `taskCancelRequested` and/or `taskCancelled` events.

**Precedence over the `cancellable` flag**: The `requested` and `graceful` modes **MUST** respect a task created with `cancellable=false` and reject the cancellation request for such a task. The `forced` mode **MUST** override `cancellable=false` — it exists precisely for emergency/administrative termination and therefore takes precedence over the flag by design. This precedence **MUST** be explicit so implementations do not diverge.

- **Rationale**: Multi-mode cancellation covers the spectrum from cooperative shutdown to emergency termination. Making `forced` override `cancellable=false` (while cooperative modes honor it) gives operators a guaranteed emergency-stop without weakening the flag's protection against routine cancellation.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`, `cpt-cf-task-engine-actor-platform-admin`

#### Tags and Classification

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-tags`

The system **MUST** support key-value tags on tasks, where tag keys are GTS well-known instance identifiers (base type `gts.cf.core.te.tag_key.v1~`) and tag values are strings. Pre-defined tag keys **MUST** include common classification dimensions (e.g., `project`, `region`, `capability`, `customer`). Additional tag keys **MUST** be registerable via GTS without schema changes. Tags **MUST** be queryable via filter expressions, mutable after creation, and indexable for efficient filtering.

**Registry scope vs tenant isolation**: The tag-**key** GTS registry is **global** (platform-wide) — tag-key identifiers are shared, standardized instances, deliberately not tenant-private, which is what prevents ad-hoc per-tenant key proliferation. Tenant isolation applies at the tag **value** level: the tag *assignments* on tasks (key → value) are stored on tenant-owned rows (`owner_tenant_id`, `Scopable`) and are only ever visible/queryable within the caller's tenant scope (§5.11). There is no per-tenant "private key" registration; a tenant needing a custom classification dimension registers a globally-visible key (whose *identifier* is discoverable platform-wide) but whose *values on tasks* remain tenant-scoped. This keeps the registry consistent with §5.11 tenant isolation without introducing a tenant-scoped GTS registry.

- **Rationale**: GTS-typed tag keys ensure consistency, discoverability, and prevent ad-hoc key proliferation across tenants. Using well-known instances enables standardized filtering, reporting, and policy evaluation across the platform. Scoping isolation to tag values (not key identifiers) reconciles the global-registry benefit with per-tenant data isolation.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`

#### Task Checkpoint

- [ ] `p3` - **ID**: `cpt-cf-task-engine-fr-task-checkpoint`

The system **MUST** accept checkpoint data (JSON) and merge it into the task's context at the key level, enabling resume from the last checkpoint after interruption. After the merge, the task's **aggregate** payload (`input + context + output`, measured as uncompressed canonical JSON per §5.1 Task Creation) **MUST NOT** exceed the configurable maximum (default 64 KB). A checkpoint whose merged result would exceed the limit **MUST** be rejected with a `413 Payload Too Large` error, leaving the stored context unchanged. The system **MUST** publish a `taskCheckpointed` event.

- **Rationale**: Checkpointing enables long-running tasks to resume from intermediate state without re-executing completed work. Because a checkpoint grows the context, it is validated against the same aggregate limit as every other payload-changing write (§5.1), preventing a valid-input task from later exceeding the limit via accumulated context.
- **Actors**: `cpt-cf-task-engine-actor-worker`


### 5.2 Queue + Concurrency Control

#### Priority-Ordered Dispatch

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-priority-dispatch`

The system **MUST** dispatch tasks from queues in priority order (lowest numeric value = highest priority). When multiple tasks share the same priority, the system **MUST** use FIFO ordering (earliest enqueued first).

- **Rationale**: Priority ordering ensures critical tasks are processed first; FIFO tiebreaking prevents starvation within the same priority level.
- **Actors**: `cpt-cf-task-engine-actor-worker`

#### Queue Concurrency Limits

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-queue-concurrency`

The system **MUST** enforce concurrency limits defined in the queue's GTS well-known instance registration (see §5.12). Each queue **MUST** declare:

- `global_concurrency_limit` — maximum number of simultaneously executing tasks from this queue across all tenants
- `per_tenant_concurrency_limit` — maximum number of simultaneously executing tasks from this queue per tenant

The system **MUST** enforce both limits independently by refusing to assign new tasks when either executing count reaches its cap.

**Atomic slot reservation** (applies to all concurrency dimensions — queue and type, global and per-tenant): Concurrency enforcement **MUST** reserve the required slot(s) and transition the task to `claimed` in **one atomic operation** (single transaction, guarded conditional update `used < limit`), so two concurrent claims cannot both observe an available slot. A slot is **consumed** by tasks in the `claimed` and `running` states, and **released** when a task leaves those states for any other state (completion, cancellation, failure, dead-letter, retry re-enqueue to `scheduled`, or requeue to `queued`). A task **MUST** satisfy every applicable dimension before assignment; if any single dimension is at capacity the claim is refused (`429`, `error.concurrency_limit`) and any partially-reserved dimensions for that candidate are rolled back. See DESIGN §3.7 (`te_concurrency_slots`) and §4.6 (claim algorithm) — the slot table and single-statement guarded reservation are already specified there.

- **Rationale**: Separate global and per-tenant concurrency on queues prevents resource exhaustion at the platform level while also preventing noisy-neighbor problems within individual tenants. Making reservation atomic with the claim (rather than a separate check-then-assign) closes the race where concurrent claims exceed the limit.
- **Actors**: `cpt-cf-task-engine-actor-worker`

#### Task Type Concurrency Limits

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-type-concurrency`

In addition to queue concurrency limits, the system **MUST** enforce concurrency limits defined in the task type's GTS registration (see §5.12). Each task type **MAY** declare:

- `global_concurrency_limit` — maximum number of simultaneously executing tasks of this type across all tenants
- `per_tenant_concurrency_limit` — maximum number of simultaneously executing tasks of this type per tenant

A task **MUST** satisfy all applicable concurrency limits (both queue-level and type-level, both global and per-tenant) before it can be assigned.

- **Rationale**: Type-level concurrency limits enable capacity control orthogonal to queues — for example, limiting expensive AI inference tasks to 5 globally regardless of which queue they are in, or capping per-tenant backup operations independently.
- **Actors**: `cpt-cf-task-engine-actor-worker`

#### Concurrency Limit Overrides

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-concurrency-overrides`

The concurrency limits declared in GTS queue and task type registrations **MUST** be overridable in the Task Engine config file. Override entries **MUST** support GTS identifier patterns with wildcard matching (e.g., `gts.cf.core.te.queue.v1~cf.system.*` or `gts.cf.core.te.task.v1~cf.vendor.*`). Each override **MAY** specify a replacement `global_concurrency_limit` and/or `per_tenant_concurrency_limit`. The most specific matching pattern wins.

- **Rationale**: GTS registrations define sensible defaults, but operators need to tune concurrency limits for their deployment without modifying type registrations. Wildcard support enables bulk overrides (e.g., cap all vendor task types at 20 per tenant).
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

#### Runtime Configuration Updates

- [ ] `p3` - **ID**: `cpt-cf-task-engine-fr-runtime-config`

Queue parameters (concurrency limits, default priority, timeout values, retry policy) and concurrency limit overrides **MUST** be modifiable at runtime without restart. Runtime configuration changes **MUST** require the `admin` authorization action and **MUST** be recorded in the event history via a `configUpdated` event (§5.10) carrying the acting subject identity, timestamp, and the changed keys.

- **Rationale**: Runtime-configurable parameters enable operational tuning without deployment. Authorization and audit logging ensure changes are traceable and restricted to authorized operators.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

#### Queue Introspection

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-queue-introspection`

The system **MUST** expose queue depth, executing count, concurrency limit, and per-state task counts for each queue.

- **Rationale**: Queue introspection enables capacity planning, autoscaling decisions, and operational monitoring.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

### 5.3 Scheduling + Timeouts

#### Delayed / Scheduled Enqueue

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-delayed-enqueue`

The system **MUST** support deferred task enqueue: a task definition may include a `run_after` timestamp, and the system holds the task in a `scheduled` state until the specified time, at which point it transitions to `queued`.

- **Rationale**: Delayed execution is fundamental for retry backoff, scheduled maintenance, and time-triggered work.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`

#### Multi-Type Timeout Enforcement

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-timeouts`

The system **MUST** enforce the following timeout types, persisted in the database and rebuilt on restart:

- **Queue timeout** — task waited too long in queue without being claimed → force-complete with timeout result
- **Assignment timeout** — worker claimed but didn't start in time → return to queue, blacklist worker
- **Execution timeout** — task execution exceeded time limit → force-complete with timeout result, blacklist worker
- **Heartbeat timeout** — worker stopped sending heartbeats → return to queue or force-complete, blacklist worker
- **Lifetime timeout** — task exceeded maximum total lifetime → **terminal force-completion** with a `timedOut` result (see terminal precedence below)
- **SLA deadline** — task did not reach a terminal state by the deadline → publish `slaBreached` event (task continues executing; SLA is **non-terminal** and never changes task state)

**Terminal precedence for lifetime timeout**: Lifetime expiry is a **terminal** event and takes precedence over retry and lease-timeout handling — a task that reaches its lifetime limit **MUST NOT** be retried or requeued regardless of remaining attempts. The single terminal sequence is: force-complete the task with a `timedOut` result code, then route it based on DLQ configuration — if the task's queue has a dead-letter queue configured, the task transitions to `dead_lettered` (with the `timedOut` result preserved) and a `taskDeadLettered` event is published; otherwise it transitions to `failed`. This is the same terminal routing used for retry-budget exhaustion (§5.8), so lifetime expiry and retry exhaustion converge on one consistent DLQ-vs-`failed` rule.

Timers **MUST** be processed by a mechanism that supports multiple concurrent processor workers. The timer processing architecture is defined in [DESIGN.md](./DESIGN.md) §4.6 and [ADR/0002-timer-architecture.md](ADR/0002-timer-architecture.md).

- **Rationale**: Comprehensive timeout enforcement prevents tasks from stalling indefinitely at any lifecycle stage.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

#### Priority Aging / Fairness

- [ ] `p3` - **ID**: `cpt-cf-task-engine-fr-priority-aging`

The system **SHOULD** support automatic priority boosting for tasks that have waited in a queue beyond a configurable threshold, preventing indefinite starvation of low-priority tasks.

- **Rationale**: Priority aging is a standard scheduling technique that balances urgency with fairness.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

### 5.4 Wait Conditions / Durable Suspension

#### Wait Condition Framework

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-wait-conditions`

The system **MUST** support durable wait conditions that transition a task to a `waiting` state. Wait conditions are first-class elements of the Task Engine domain model (like task state), not GTS-typed entities. Each wait condition has a **kind** — one of the three built-in kinds defined below (timer, event, input).

When a worker or API consumer sets a wait condition on a task, the task transitions to `waiting`. The task resumes (transitions back to `queued` or `running`) when the condition is satisfied. Wait conditions **MUST** support an optional timeout — if the condition is not satisfied within the timeout, the system **MUST** publish a `waitTimedOut` event and either fail the task or resume it with a timeout indication, as configured.

Multiple wait conditions on a single task **MUST** support `ALL` (all must be satisfied) and `ANY` (first satisfied resumes the task) semantics.

- **Rationale**: Durable suspension replaces polling loops and callback chains with a first-class, observable domain model mechanism. Wait condition kinds are fixed domain concepts — unlike task types (which benefit from GTS extensibility for vendor/plugin diversity), the set of suspension semantics is small, well-defined, and tightly coupled to the Task Engine's state machine.
- **Actors**: `cpt-cf-task-engine-actor-worker`, `cpt-cf-task-engine-actor-api-consumer`

#### Timer Wait Condition

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-wait-timer`

The system **MUST** support a **timer** wait condition kind with a `resume_at` timestamp field. The system **MUST** automatically satisfy the condition and resume the task when the current time reaches `resume_at`.

- **Rationale**: Timer waits enable scheduled resumption, polling intervals, and cooldown periods without external orchestration.
- **Actors**: `cpt-cf-task-engine-actor-worker`

#### Event Wait Condition

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-wait-event`

The system **MUST** support an **event** wait condition kind with fields: `event_type` (GTS type identifier of the expected event), optional `subject_id` (expected event subject identifier), and optional `subject_type` (expected event subject type). The system **MUST** evaluate incoming events against active event wait conditions by matching on these fields and satisfy matching conditions.

- **Rationale**: Event waits enable reactive patterns — a task pauses until an external system publishes a specific event, eliminating polling.
- **Actors**: `cpt-cf-task-engine-actor-worker`, `cpt-cf-task-engine-actor-event-broker`

#### Event Wait Condition — Match Expression

- [ ] `p3` - **ID**: `cpt-cf-task-engine-fr-wait-event-expression`

In addition to field-based matching (§5.4 `cpt-cf-task-engine-fr-wait-event`), the system **SHOULD** support an optional `match_expression` field on event wait conditions — a structured expression evaluated against the incoming event to determine if it satisfies the condition (e.g., matching on event payload fields or composite criteria). The expression language is defined in DESIGN. When a `match_expression` is present, the system **MUST** evaluate it after field-based matching succeeds.

- **Rationale**: Expression-based matching enables complex event filtering beyond simple field equality — for example, matching on event payload attributes or combining multiple conditions — without requiring callers to create separate wait conditions for each criterion.
- **Actors**: `cpt-cf-task-engine-actor-worker`, `cpt-cf-task-engine-actor-event-broker`

#### Input Wait Condition

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-wait-input`

The system **MUST** support an **input** wait condition kind with fields: `input_schema` (a JSON Schema or GTS type reference defining the expected input shape) and optional `prompt` (human-readable description of what input is needed). Satisfaction requires an API call providing a JSON value that validates against the declared schema. The validated input **MUST** be stored on the task and accessible to the worker upon resumption.

- **Rationale**: Input waits enable tasks to request structured data from external actors — configuration values, user decisions, or computed parameters — without completing and re-creating the task.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`

> **Note on retry/back-off**: A separate "retry/back-off" wait condition type is **not** needed. Retry backoff delays are handled natively by the retry policy (§5.8), which re-enqueues tasks with a `run_after` timestamp corresponding to the backoff delay. The timer wait condition (§5.4) can serve the same purpose for manual in-execution pauses. This avoids concept overlap.

### 5.5 Task Entries

#### Fine-Grained Execution Records

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-task-entries`

The system **MUST** support creating task entries — immutable, timestamped records attached to a task that capture granular execution details. Each entry **MUST** have a GTS type (enabling extensible entry kinds), a timestamp, a creator (worker or user), and a payload (JSON). The serialized size of each task entry payload **MUST NOT** exceed a configurable maximum (default 64 KB). Entries exceeding this limit **MUST** be rejected with a `413 Payload Too Large` error. Pre-defined entry types **MUST** include:

```
gts.cf.core.te.entry.v1~cf.core.te.entry_work.v1~        — work performed
gts.cf.core.te.entry.v1~cf.core.te.entry_observation.v1~ — observation / check
gts.cf.core.te.entry.v1~cf.core.te.entry_issue.v1~       — issue encountered
gts.cf.core.te.entry.v1~cf.core.te.entry_note.v1~        — freeform note
gts.cf.core.te.entry.v1~cf.core.te.entry_measurement.v1~ — measurement / metric
```

Task entries **MUST** be queryable and filterable by type, time range, and creator.

**No client mutation** (resolves the immutability-vs-CRUD conflict): Because entries are immutable audit records, the client API exposes **create, read, and list only** — there is **no** `update` or `delete` on the normal client path (§5.13). The sole deletion path is scheduled **retention cleanup** (§5.14), an engine-internal administrative operation, not a client-facing CRUD verb.

- **Rationale**: Task entries provide an auditable, structured record of what happened during execution — distinct from progress (how far) and result (what outcome). Restricting the client surface to create/read/list preserves the immutability/audit guarantee; retention is the only deletion mechanism.
- **Actors**: `cpt-cf-task-engine-actor-worker`, `cpt-cf-task-engine-actor-api-consumer`

### 5.6 Assignment + Atomic Claim + Lease

#### Atomic Claim with Lease

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-assignment`

The system **MUST** support atomic task claiming: a worker requests a task from one or more queues, and the system atomically assigns the highest-priority eligible task, sets a lease expiry (`lease_until`), records `claimed_at`, increments the `attempt` counter, reserves the required concurrency slots (§5.2), and returns the task. Two workers **MUST NOT** be able to claim the same task. Slot reservation and the task-state transition to `claimed` **MUST** occur in the same atomic operation (see §5.2). The system **MUST** support long-poll claiming: if no tasks are immediately available and a timeout is specified, the worker waits on a signal channel and retries when a `newTask` signal arrives.

The `attempt` counter is the single authoritative count of execution attempts and **MUST** be incremented in exactly one place — on a successful claim. Retry re-enqueue (§5.8) and lease-expiry requeue (below) **MUST NOT** increment `attempt` themselves; the increment happens when the requeued task is next claimed.

**Single-domain claim** (§5.19): all queues named in a single claim request **MUST** resolve to the same task domain. A claim request whose queues span multiple task domains **MUST** be rejected with `422 Unprocessable Entity`. This preserves atomic, highest-priority-across-queues assignment within one database; a worker that wants to consume from queues in different domains issues one claim request per domain.

The assignment record **MUST** include: `assignee` (worker identity), `claimed_at`, `lease_until`, `heartbeat_at`, and `attempt`.

- **Rationale**: Atomic claiming with leases is the foundation of distributed task execution; it prevents double-dispatch and enables automatic recovery of abandoned tasks. Bundling slot reservation into the same atomic step prevents two concurrent claims from both observing an available slot. A single authoritative `attempt` increment avoids double-counting across claim and retry paths.
- **Actors**: `cpt-cf-task-engine-actor-worker`

#### Heartbeat / Lease Renewal

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-heartbeat`

The system **MUST** accept heartbeat signals from workers, extend the lease, update `heartbeat_at`, and return a control response containing: cancel-requested flag, resolved wait conditions, and current task state. If the lease expires without a heartbeat, the task **MUST** be returned to the queue (if retries remain) or force-completed with a timeout result.

**Lease-expiry requeue uses backoff**: When a lease expires and the task is returned to the queue, the system **MUST** apply the same backoff computation as an explicit worker-reported failure (§5.8) — it transitions the task to `scheduled` with a `run_after` derived from the retry policy and the current attempt number, rather than re-enqueueing immediately. This prevents a slow-but-alive worker from being re-assigned the same task in a tight retry storm and keeps the two failure paths (explicit failure vs lease expiry) consistent. A lease expiry also counts as one consecutive failure toward the worker-blacklist threshold (§5.6 Worker Blacklisting).

- **Rationale**: Heartbeats provide worker liveness detection and a bidirectional control channel. Reusing the retry-policy backoff on lease expiry avoids a divergence where unhealthy workers thrash a task with no delay.
- **Actors**: `cpt-cf-task-engine-actor-worker`

#### Worker Blacklisting

- [ ] `p3` - **ID**: `cpt-cf-task-engine-fr-worker-blacklist`

The system **MUST** maintain a per-queue temporary worker blacklist keyed by consecutive failures. A worker is blacklisted for a queue once it accumulates a configurable number of **consecutive failures** on that queue (default: 10). A "consecutive failure" **MUST** include both an explicit task failure and a lease expiry (missed heartbeat) — a single missed heartbeat therefore counts as **one** failure toward the same threshold, not an immediate blacklist. The counter **MUST** reset to zero on the worker's next successful task completion on that queue. Once the threshold is reached, the worker **MUST** be blacklisted for that queue for a configurable duration (default: 60 minutes). Blacklisted workers **MUST** be skipped during task assignment for the affected queue. Blacklist thresholds and durations **MUST** be configurable per queue. Each blacklisting **MUST** be recorded in the event history via a `workerBlacklisted` event (§5.10).

- **Rationale**: Blacklisting with testable thresholds prevents repeated assignment to failing workers while allowing recovery after the cooldown period. Treating a missed heartbeat as one failure (rather than an immediate blacklist) avoids ejecting workers on a single transient network blip, keeping heartbeat and task-failure handling symmetric.
- **Actors**: `cpt-cf-task-engine-actor-worker`, `cpt-cf-task-engine-actor-platform-admin`

#### Reassignment / Handoff

- [ ] `p3` - **ID**: `cpt-cf-task-engine-fr-reassignment`

The system **MUST** support explicit task reassignment from one worker (or worker pool) to another without losing history. The reassignment **MUST** be recorded in the event history via a `taskReassigned` event (§5.10) carrying the previous and new assignee.

- **Rationale**: Reassignment supports escalation, shift handoff, and skill-based routing without task cancellation and re-creation.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`, `cpt-cf-task-engine-actor-platform-admin`

### 5.7 Progress Tracking

#### Multi-Dimensional Progress

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-progress`

The system **MUST** accept progress updates for tasks with multiple dimensions: percentage (0–100), optional milestone name, optional counters (completed/total units), optional current stage name, and optional ETA timestamp. Progress updates **MUST** support write batching to reduce database pressure. The batching mechanism and flush interval **MUST** be configurable.

- **Rationale**: Multi-dimensional progress enables meaningful monitoring beyond a single percentage bar.
- **Actors**: `cpt-cf-task-engine-actor-worker`

### 5.8 Retry / Failure Handling

#### Configurable Retry Policies

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-retry`

The system **MUST** support configurable retry policies per task (or inherited from queue defaults): max retry count, backoff strategy (`fixed`, `linear`, `exponential`), base interval, maximum interval, and jitter factor. On task failure, the system **MUST** classify the error as retryable or terminal.

**Retry lifecycle** (aligns with the state machine and `attempt` counting in §5.6):

- A **retryable** failure with attempts remaining transitions the task `running → scheduled` with a `run_after` timestamp computed from the backoff strategy and the current attempt number. It **MUST NOT** transition to `failed`; `failed` is a terminal state and retryable failures never enter it. The `attempt` counter is **not** incremented here — it is incremented only when the requeued task is next claimed (§5.6).
- A **terminal** failure, or a retryable failure that has exhausted its retry budget, ends the lifecycle via the single terminal routing rule: if the queue has a dead-letter queue configured the task transitions to `dead_lettered` (with `taskDeadLettered` published), otherwise it transitions to `failed`. This is the same rule applied to lifetime-timeout expiry (§5.3).

- **Rationale**: Structured retry with backoff is the industry standard for transient failure recovery. Keeping retryable failures on the `running → scheduled` path (never `failed`) and incrementing `attempt` in a single place (on claim) removes the double-count and illegal-terminal-transition ambiguities.
- **Actors**: `cpt-cf-task-engine-actor-worker`

#### Dead Letter Queue

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-dead-letter`

Tasks that exhaust their retry budget or exceed their lifetime (§5.3) **MUST**, when a dead-letter queue is configured on their queue, be moved to that dead-letter queue rather than being silently discarded (tasks with no DLQ configured transition to `failed` instead — see the single terminal routing rule in §5.8 Retry and §5.3). DLQ tasks **MUST** be queryable, inspectable, and manually re-enqueueable.

**Task-domain scope of DLQ inspection** (§5.19): DLQ inspection is a read/inspection path. When the caller does not specify a task type (and therefore does not pin a single domain), DLQ inspection **MUST** fan out across all task domains and merge results with consistent sorting and pagination — the same cross-domain fan-out behavior defined for generic listing queries in §5.19. When a task type or domain is specified, inspection is scoped to that domain.

- **Rationale**: DLQs prevent silent data loss and are standard in enterprise task systems. Fanning out DLQ inspection across domains by default ensures operators see all parked failures rather than missing dead letters in domains they did not think to query.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

### 5.9 Dependencies / Parent-Child Execution

#### Parent/Child Task Hierarchies

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-parent-child`

The system **MUST** support parent/child task relationships. A parent task **MAY** create child tasks during execution. The system **MUST** track child completion status and optionally prevent the parent from completing until all children complete. Child task cancellation **MUST** be propagatable from the parent.

**Single-domain constraint** (§5.19): a parent and all of its children **MUST** reside in the same task domain (database). If a child's GTS type would route it to a different domain than its parent, child creation **MUST** be rejected with `422 Unprocessable Entity`. This keeps completion tracking and cancellation propagation within one database.

**Interaction with forced cancellation** (§5.1): when a parent configured to wait for children is **force-cancelled**, the parent **MUST** transition to `cancelled` immediately (forced cancellation is unconditional, see §5.1), and the system **MUST** propagate cancellation to all in-flight children as part of the same operation so no orphaned children continue running. Children are cancelled asynchronously (best-effort per child) while the parent's terminal transition is not blocked on their completion.

- **Rationale**: Parent/child hierarchies enable decomposition of complex work into independently executable sub-tasks. Constraining a hierarchy to one domain avoids cross-database tracking; propagating cancellation on forced parent cancel prevents orphaned children.
- **Actors**: `cpt-cf-task-engine-actor-worker`

#### Predecessor/Successor Dependencies

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-dependencies`

The system **MUST** support predecessor/successor dependency declarations: a task specifies prerequisite tasks that must complete successfully before it becomes eligible for assignment. Dependencies **MUST** support `ALL` (all predecessors must succeed) and `ANY` (first predecessor to succeed unblocks) modes.

**Mutability and cycle validation**: Dependencies **MAY** be declared at task creation (see §5.1) and **MAY** be added or removed after creation via the dependency-management API (§5.13) while the task is still non-terminal and not yet running. The dependency graph **MUST** be validated to prevent cycles on **every** mutation — both at creation time and on every post-creation dependency change — not only at creation. A mutation that would introduce a cycle **MUST** be rejected with `422 Unprocessable Entity` (`error.cycle_detected`).

**Single-domain constraint** (§5.19): a predecessor and its successor **MUST** reside in the same task domain (database). A dependency whose endpoints would route to different domains **MUST** be rejected with `422 Unprocessable Entity`. Cycle detection and `ALL`/`ANY` unblocking are therefore always evaluated within a single database.

- **Rationale**: Dependencies enable multi-step pipelines and coordinated execution without external orchestrators. Re-validating on every mutation closes the gap where a cycle is introduced by a later dependency change; the single-domain constraint keeps validation and unblocking within one database.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`

### 5.10 Event History / Audit Trail

#### Immutable Event Log

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-event-history`

The system **MUST** maintain an immutable, append-only event log for each task recording all state transitions, assignment changes (including reassignment and worker blacklisting), wait condition set/resolved, retry attempts, progress milestones, checkpoint saves, cancellation requests, timeout firings, runtime configuration changes, and user actions. Each event **MUST** include a **stable unique event ID**, a timestamp, actor identity, event type, and structured payload.

**Local log is always complete and independent of publishing policy**: The immutable local event log **MUST** always record the full set of applicable events (including `taskCreated`) regardless of the external event-publishing policy (§5.10 Event Publishing Configuration). The publishing policy (which may be `none` for some task types) controls only which events are **emitted externally** to the Event Broker; it **MUST NOT** suppress writes to the local audit log. The local event-log write **MUST** be atomic with the state change that produced it (same transaction), so the audit trail can never diverge from actual task state.

- **Rationale**: A complete audit trail is essential for debugging, compliance, and operational transparency. Separating the always-complete local log from policy-controlled external publishing resolves the apparent conflict between "`taskCreated` MUST be published" and "policy may be `none`". Stable event IDs enable downstream deduplication (§5.10 Event Publishing).
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`, `cpt-cf-task-engine-actor-api-consumer`

#### Event Publishing

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-event-publishing`

The system **MUST** publish structured notification events for task lifecycle state changes to the upcoming platform Event Broker gear. The full set of supported event types is: `taskCreated`, `taskQueued`, `taskClaimed`, `taskStarted`, `taskCompleted`, `taskFailed`, `taskCancelled`, `taskCancelRequested`, `taskCheckpointed`, `taskWaiting`, `taskResumed`, `taskRetrying`, `taskReassigned`, `taskDeadLettered`, `slaBreached`, `waitConditionSet`, `waitConditionSatisfied`, `waitTimedOut`, `workerBlacklisted`, `configUpdated`. (`taskReassigned`, `workerBlacklisted`, and `configUpdated` are included so the "MUST be recorded in the event history" requirements in §5.6 Reassignment, §5.6 Worker Blacklisting, and §5.2 Runtime Configuration Updates map to concrete event types rather than an undefined mechanism.)

**Delivery and atomicity contract**:

- Each event carries the **stable unique event ID** assigned in the local log (§5.10 Immutable Event Log).
- The state change and the durable emission of its event(s) **MUST** be **atomic** (written in the same transaction as the state change via the transactional outbox). This is not conditional on the Event Broker being reachable — the outbox row is written atomically and delivered asynchronously.
- Delivery to the Event Broker is **at-least-once**; consumers (including the Task Engine's own event-wait matcher, §5.4) **MUST** treat delivery as at-least-once and **MUST** deduplicate on the event ID. In particular, wait-condition resolution **MUST** be idempotent so that a duplicate event delivery cannot satisfy the same wait condition twice.

Until the Event Broker gear is available, the system **MUST** support a local event log that downstream consumers can poll.

- **Rationale**: Event publishing enables downstream consumers to react to task lifecycle changes without polling, and provides the source for event-type wait conditions. The transactional outbox ensures state and events never diverge and that no events are lost even if the Event Broker is temporarily unavailable; stable IDs + idempotent resolution make at-least-once delivery safe. The managed outbox producer is specified in DESIGN §3.2 Event Producer.
- **Actors**: `cpt-cf-task-engine-actor-event-broker`

#### Event Publishing Configuration

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-event-publishing-config`

Which events are actually emitted **MUST** be configurable in the Task Engine config file per GTS task type pattern (with wildcard support). Each event policy entry specifies:

- A GTS task type pattern (exact or wildcard, e.g., `gts.cf.core.te.task.v1~cf.system.*`)
- A list of enabled event types (or `all` / `none` shorthand)
- An optional list of state transitions that trigger events (e.g., only emit `taskCompleted` and `taskFailed` for lightweight system tasks, but emit all events for user-visible tasks)

A `default` policy **MUST** apply to task types not matched by any specific pattern. If no configuration is provided, the default **MUST** be to emit all event types for all task types.

- **Rationale**: Not all task types need the same event verbosity — high-volume system tasks may only need terminal-state events, while user-visible tasks benefit from full lifecycle visibility. Config-driven event policies avoid unnecessary event throughput and storage overhead.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

### 5.11 Authentication and Authorization

#### Authentication via AuthN Resolver

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-authn`

Every protected request **MUST** be authenticated via the API Gateway AuthN middleware, which validates the bearer token through the AuthN Resolver gear and injects a `SecurityContext` (containing `subject_id`, `subject_tenant_id`, `token_scopes`, and optional `subject_type`) into the request. The Task Engine **MUST** extract the `SecurityContext` for all protected endpoints using ToolKit's `.authenticated()` route policy. Public endpoints (health, metrics) **MUST** use `.public()` and receive an anonymous `SecurityContext`.

- **Rationale**: Authentication follows the Gears PDP/PEP model (per [docs/arch/authorization/DESIGN.md](../../../docs/arch/authorization/DESIGN.md)): AuthN Resolver validates tokens, domain gears act as PEPs.
- **Actors**: All actors

#### Authorization via PolicyEnforcer (PEP)

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-authz`

All authorization decisions **MUST** be obtained by calling the `PolicyEnforcer` from `authz-resolver-sdk`, which delegates to the AuthZ Resolver gear (PDP). The PDP returns access decisions with structured access constraints (predicates). The Task Engine **MUST** compile these constraints into an `AccessScope` and pass it to `SecureConn` for SQL-level enforcement. Fail-closed: denied decisions, unreachable PDP, and missing constraints **MUST** result in 403 Forbidden. PDP internals **MUST NOT** be exposed to clients.

Permissions **MUST** be modeled as GTS Permission types (base type `gts.cf.toolkit.authz.permission.v1~`) with actions scoped to the `task_engine` resource namespace. Core actions include: `create`, `read`, `update`, `delete`, `claim`, `cancel`, `admin`.

- **Rationale**: The PDP/PEP model ensures authorization is policy-driven, externalized, and enforceable at the query level via `SecureConn` WHERE clauses. This aligns with the platform authorization architecture (per [docs/arch/authorization/DESIGN.md](../../../docs/arch/authorization/DESIGN.md) and [docs/toolkit_unified_system/06_authn_authz_secure_orm.md](../../../docs/toolkit_unified_system/06_authn_authz_secure_orm.md)).
- **Actors**: All actors

#### Tenant Scoping and Hierarchy

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-tenant-scoping`

The system **MUST** resolve tenant hierarchies via the Tenant Resolver gear, scope all queries to the caller's accessible tenants via `SecureConn` tenant predicates, and validate tenant ancestry for all operations. Every task **MUST** have an `owner_tenant_id` (isolation boundary) and an optional `owner_id` (subject scoping for "my tasks" views). The `SecureConn` layer **MUST** always enforce a tenant predicate to prevent cross-tenant data leakage, regardless of PDP decisions. SeaORM entities **MUST** derive `Scopable` with the appropriate tenant and resource columns.

- **Rationale**: Tenant hierarchy enforcement is the foundation of multi-tenant data isolation. Subject scoping enables per-user task views without breaking tenant boundaries.
- **Actors**: All actors

#### Resource-Path Access Policies

- [ ] `p3` - **ID**: `cpt-cf-task-engine-fr-resource-path-access`

Creators and workers **MUST** be restrictable to specific queues via access constraint predicates returned by the PDP. Token scopes **MUST** act as a capability ceiling on permitted operations.

- **Rationale**: Fine-grained queue access control enables multi-team isolation within a single tenant.
- **Actors**: `cpt-cf-task-engine-actor-worker`, `cpt-cf-task-engine-actor-api-consumer`

### 5.12 GTS Extension Points

#### Task Type as GTS Base Type

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-gts-task-type`

The system **MUST** define a GTS base type for task types:

```
gts.cf.core.te.task.v1~
```

This base type **MUST** establish the stable contract: base fields in indexed columns, extension data (input, context, output) in JSONB. Derived task types **MUST** be registerable with JSON Schema definitions for input and output shapes, and **MAY** declare concurrency limits (`global_concurrency_limit`, `per_tenant_concurrency_limit`) as part of their GTS registration. The `type` column **MUST** store the deterministic UUID derived from the GTS type identifier. New task types **MUST NOT** require DDL changes.

- **Rationale**: GTS typing enables runtime type extension without schema migrations. Embedding concurrency limits in the type registration provides sensible defaults that operators can override in config.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`

#### Queue as GTS Base Type

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-gts-queue`

The system **MUST** define a GTS base type for queues:

```
gts.cf.core.te.queue.v1~
```

Each queue **MUST** be registered as a GTS well-known instance derived from this base type. The queue registration **MUST** include: `global_concurrency_limit`, `per_tenant_concurrency_limit`, default priority, default timeout values (queue, assignment, execution, heartbeat, lifetime), default retry policy, and optional dead-letter queue reference (another queue GTS instance). Queue names used in task creation and claiming **MUST** reference registered GTS queue instances. New queues **MUST** be registerable via GTS without DDL changes.

- **Rationale**: Modeling queues as GTS instances makes them discoverable, self-documenting, and consistent with the platform type system. Queue parameters (concurrency, timeouts, retry) are defined once in the registration and overridable in config.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

#### Task State and Result Code Domain Enums

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-state-result-enums`

Task states (`scheduled`, `queued`, `blocked`, `claimed`, `running`, `waiting`, `completed`, `failed`, `cancelled`, `dead_lettered`) and result codes (`success`, `warning`, `error`, `cancelled`, `timedOut`, `forciblyCancelled`) **MUST** be modeled as closed SDK/domain enums managed by Task Engine, not as GTS well-known instances.

The following states are **terminal**: `completed`, `failed`, `cancelled`, `dead_lettered`. Tasks in terminal states **MUST NOT** transition to any other state except via explicit administrative re-enqueue (which transitions `dead_lettered` → `queued`). The following states are **non-terminal**: `scheduled`, `queued`, `blocked`, `claimed`, `running`, `waiting`. Non-terminal states participate in timeout enforcement, retention hard-limit cleanup, and lifecycle event publishing. The `cancelled` state **MUST** be used when cancellation ends the lifecycle; the cancellation result code records the cancellation outcome or mode.

State and result-code persistence **MUST** use compact numeric enum codes (`u8` in SDK/domain types; `TINYINT` or the backend's smallest portable integer type in storage), not strings and not GTS UUIDs.

**Enum evolution and unknown-value behavior** (applies to all engine-owned closed enums — task states, result codes, wait condition kinds, timer types, timer statuses, dependency modes, concurrency dimensions):

- **Stable discriminants**: Each variant has a fixed numeric discriminant that **MUST NOT** be renumbered or reused once released. Stored values and SDK discriminants are the same numbers.
- **Wire encoding**: The REST/JSON representation **MUST** be the stable lowercase string name (e.g., `"queued"`, `"timed_out"`); the numeric discriminant is the persistence/SDK form. Both encodings map 1:1 to the same variant.
- **Unknown-value handling**: An SDK or REST client that receives a discriminant/name it does not recognize (e.g., an older client against a newer server) **MUST** surface it as an explicit `Unknown`/unrecognized value rather than silently coercing it to a known variant or panicking. Servers reading an unknown stored code **MUST** fail closed (treat as non-actionable) rather than mis-classify.
- **Compatibility rule**: **Adding** a new variant (with a new, never-before-used discriminant) is a **minor** (backward-compatible) change *provided* clients implement the unknown-value handling above. **Renumbering, removing, or repurposing** a discriminant is a **breaking** change. This applies to rolling upgrades (mixed old/new nodes) and to values already persisted in the database.

- **Rationale**: States and result codes are engine-owned control-plane values, not extension points. Closed enums keep transition logic exhaustive, make invalid values unrepresentable in SDK code, and keep hot database indexes compact. Explicit terminal/non-terminal classification ensures consistent behavior across timeout enforcement, retention cleanup, authz policies, and event publishing. Defining stable discriminants, wire encoding, and unknown-value handling lets the enums evolve safely across SDK versions, REST clients, rolling upgrades, and stored data.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`

#### Task Entry Type as GTS Base Type

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-gts-entry-type`

The system **MUST** define a GTS base type for task entry types (`gts.cf.core.te.entry.v1~`) with pre-built derived types for work, observation, issue, note, and measurement (see §5.5).

- **Rationale**: GTS typing for entries enables domain-specific execution record types.
- **Actors**: `cpt-cf-task-engine-actor-worker`

### 5.13 REST API

#### REST API

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-rest-api`

The system **MUST** expose a REST API at `/api/task_engine/v1` with endpoints for:

- Task CRUD, claim, start, heartbeat, progress, checkpoint, complete, cancel
- Task entry create, read, list (entries are immutable; no update/delete on the client path — see §5.5)
- Wait condition set, query, resolve (provide input). Approval gates are out of scope (§4.2); an approval-like decision is modeled as a structured value on the **input** wait condition, not as dedicated approve/reject endpoints.
- Queue configuration and introspection
- Dependency management (add/remove predecessors with cycle re-validation, §5.9)
- Search and filtering with cursor-based pagination, LOD projections, and multi-field sorting
- Bulk operations (cancel, reprioritize, retry, move queue) with per-task authorization and partial-success results (§5.18)
- Dead-letter queue inspection (task-domain fan-out, §5.8) and re-enqueue
- Event history query (paginated; the canonical source for a task's complete event log — see §5.17)
- Health probes (liveness, readiness)
- Prometheus metrics endpoint

All mutating endpoints **MUST** use ToolKit `OperationBuilder`, and all error responses **MUST** use RFC 9457 Problem Details with GTS type URIs.

- **Rationale**: The REST API is the primary integration surface.
- **Actors**: All actors

### 5.14 Data Retention and Cleanup

#### Scheduled Retention

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-retention`

The system **MUST** run scheduled cleanup performing batched deletion of completed/failed tasks, task entries, and event history older than retention thresholds. Retention **MUST** be configurable at three levels with an **explicit precedence order** (highest precedence first):

1. **Per tenant** — tenant-specific overrides for compliance requirements. A matching tenant policy takes precedence over any task-type policy and the global default, because compliance/legal-hold obligations are tenant-scoped and must not be overridden by a type rule.
2. **Per GTS task type** — configurable in the Task Engine config file using GTS type patterns with wildcard support (e.g., `gts.cf.core.te.task.v1~cf.vendor.*` matches all vendor task types). Each pattern specifies its own retention duration. When multiple type patterns match, the **most specific** (longest non-wildcard prefix) pattern wins.
3. **Global default** — fallback retention when no tenant or type policy matches. The global default retention is **100 days** (matching the capacity-planning profile in §6.1).

The effective retention for a task is therefore: tenant policy if one matches, else the most-specific matching type policy, else the 100-day global default.

Before cleanup, the system **MUST** force-cancel tasks stuck in non-terminal states beyond the hard retention threshold. Retention cleanup **MUST** run independently per task domain (storage database).

- **Rationale**: Without retention, the database grows unboundedly; per-type retention (with GTS wildcards) allows short-lived system tasks to be cleaned aggressively while preserving user-visible tasks longer; per-tenant retention supports varying compliance requirements.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

### 5.15 Observability

#### Instrumentation and Metrics

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-observability`

The system **MUST** expose Prometheus metrics for: request counters and latency histograms, queue depth and executing counts, retry rates, DLQ sizes, wait condition counts by type, SLA breach counts, timer/signal channel sizes, and database connection pool stats. Liveness and readiness probes **MUST** reflect database connectivity and overall health.

- **Rationale**: Observability is essential for capacity planning, alerting, and operational health.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

### 5.16 SDK Crate

#### Public SDK

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-sdk`

The system **MUST** provide a `task-engine-sdk` crate exposing:

- Client trait for all task operations
- GTS type definitions for task types and entry types
- Domain enum definitions for task states, result codes, wait condition kinds, timer types, and other engine-owned finite sets
- Request/response types for all API operations
- Error types with GTS identifiers for RFC 9457 mapping

The SDK **MUST** follow the ToolKit SDK-first pattern.

- **Rationale**: An SDK crate enables type-safe integration by other gears.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`

### 5.17 Search and Querying

#### Advanced Search

- [ ] `p1` - **ID**: `cpt-cf-task-engine-fr-search`

The system **MUST** provide paginated search for tasks with: **response detail levels** — callers select how much data each result item contains (`tiny` returns only IDs and status, `short` adds key fields like type/priority/queue, `long` includes input/output/context, `full` returns the complete task with a **bounded** embedded event history, `count` returns only the match count without task data), multi-field filtering on standard metadata fields (status, assignee, queue, type, tags, age, priority, dependencies), multi-field sorting with automatic unique-sort tiebreaker, and cursor-based pagination. Searching or filtering by fields within task-type-specific JSON payloads is out of scope (see §4.2).

**Bounded embedded event history at `full` level**: Because the event log is append-only with no length cap (§5.10) and `full`-level results apply to paginated *bulk* search, the embedded event history per result item **MUST** be capped at a configurable maximum (default: the most-recent **N=50** events) rather than the complete log. Callers needing the complete event history **MUST** use the dedicated, paginated event-history endpoint (`GET /tasks/{id}/events`, §5.13). This bounds `full`-level response size to `page_size × (fixed per-task base + capped history)` so response payloads scale predictably even for long-lived, heavily-retried tasks at the §6.1 capacity targets.

- **Rationale**: Rich search is essential for operational dashboards, debugging, and bulk management. Capping embedded history and directing full-log retrieval to the dedicated endpoint prevents unbounded per-item response growth in bulk `full`-level queries.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`, `cpt-cf-task-engine-actor-platform-admin`

### 5.18 Bulk Operations

#### Bulk Task Management

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-bulk-ops`

The system **MUST** support bulk operations in a single API call: batch cancel, batch reprioritize, batch retry (re-enqueue from DLQ), and batch move (transfer tasks between queues).

**Per-task authorization and partial success**: Authorization for bulk operations **MUST** be evaluated **per task** (not once for the whole batch), consistent with the fail-closed model in §5.11 applied at the `SecureConn`/`AccessScope` level. A bulk request **MUST** return a **partial-success** response that reports each task's outcome individually — `succeeded`, `unauthorized` (403-equivalent for that task), `not_found`, or `invalid_state` — so that authorized tasks proceed while unauthorized ones are reported without failing the whole request. The response shape is a per-task result list keyed by task ID.

**Batch move semantics** (`/tasks/bulk/move`): When a task is moved to a destination queue, it **MUST** immediately adopt the destination queue's retry policy and concurrency limits for all subsequent dispatch/accounting. In-flight leases **MUST** be reconciled by **restricting** which tasks are movable: a move **MUST** be rejected (per-task `invalid_state` outcome) for any task currently holding an active lease (`claimed` or `running`); only tasks in `queued`, `scheduled`, or `blocked` states may be moved. This avoids a worker executing against a queue it was never dispatched from and avoids concurrency-slot accounting bypass. Both source and destination queues **MUST** resolve to the same task domain (§5.19); a cross-domain move **MUST** be rejected.

- **Rationale**: Bulk operations reduce round-trip overhead for operational management. Per-task authorization with partial-success results keeps the fail-closed model consistent for multi-task requests, and constraining moves to non-leased tasks removes the lease/concurrency/retry-policy reconciliation ambiguity.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

### 5.19 Task Domain Routing

#### Multi-Database Storage Domains

- [ ] `p3` - **ID**: `cpt-cf-task-engine-fr-task-domains`

The system **MUST** support configurable **task domains** that route tasks to different databases. Each domain is defined in the Task Engine config file with:

- A domain name (e.g., `system`, `user_visible`, `analytics`)
- A list of GTS task queue or task type patterns with wildcard support (e.g., `gts.cf.core.te.task.v1~cf.system.*`)
- A database DSN (resolved via `DbManager`)

A `default` domain **MUST** catch all task types not matched by any other domain. Each domain **MUST** have its own independent schema, migrations, connection pool, and retention schedule.

**Deterministic routing rule**: A task **MUST** resolve to exactly one task domain. Domain patterns (whether they match on GTS task type or on queue) are evaluated **in configuration order**, and the **first domain whose pattern matches wins**; the `default` domain is the terminal fallback. This single, order-based resolution **MUST** be applied identically to every operation that selects a database — task creation, claiming, search, retention, and ready-task caching — so that a given task never maps to two different databases across operations. Because a queue's registered task types and a domain's patterns are both known at configuration time, queue-to-domain and type-to-domain resolution **MUST** agree; a configuration in which they disagree **MUST** be rejected at startup.

**Single-domain operation boundary**: Because each domain is a physically independent database, operations that require atomicity or graph traversal across rows **MUST** stay within one domain:

- **Atomic claim** (§5.6): all queues named in a single claim request **MUST** resolve to the same task domain; a claim request spanning multiple domains **MUST** be rejected with `422 Unprocessable Entity`.
- **Parent/child** (§5.9) and **predecessor/successor dependencies** (§5.9): a parent and its children, and a predecessor and its successor, **MUST** reside in the same task domain. A relationship whose endpoints would route to different domains **MUST** be rejected at creation with `422 Unprocessable Entity`. Cycle validation and `ALL`/`ANY` unblocking are therefore always evaluated within a single database.

**Cross-domain fan-out queries** (p3): When a *read* query does not specify a task type filter (e.g., listing tasks of any type, or dead-letter-queue inspection), the system **SHOULD** fan out to all domains and merge results with consistent sorting and pagination. Fan-out applies to non-atomic read/inspection paths only, never to claim, dependency, or parent/child operations.

This enables physical separation of storage — for example, system/infrastructure tasks can be stored separately from user-visible tasks, allowing UIs to query only the user-visible domain for responsive dashboards.

- **Rationale**: Task domains enable operational isolation between task classes with different storage, performance, and retention requirements. System tasks (health checks, internal bookkeeping) should not compete for I/O with user-visible tasks.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

### 5.20 Caching Layer

#### Cluster-Backed Ready-Task Cache

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-cluster-cache`

The system **MUST** support an optional ready-task cache via the Cluster gear's distributed cache primitive to accelerate claim dispatch. The cache **MUST** use priority-ordered entries with multi-dimensional routing keys (tenant, queue, priority). The system **MUST** degrade gracefully to database-only dispatch when the Cluster cache is unavailable.

- **Rationale**: Distributed caching via the Cluster gear eliminates full database scans on every claim for high-throughput deployments, without requiring a direct Redis dependency.
- **Actors**: `cpt-cf-task-engine-actor-platform-admin`

### 5.21 Rate Limiting

#### Per-Tenant and Per-Queue Rate Limits

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-rate-limiting`

The system **SHOULD** support configurable rate limiting on task creation and claim: per-tenant, per-queue, and global limits. Exceeded limits **MUST** return `429 Too Many Requests` with `Retry-After`.

- **Rationale**: Rate limiting prevents noisy-neighbor problems in multi-tenant deployments.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`

### 5.22 Recurring / Cron Tasks

#### Recurring Schedules

- [ ] `p2` - **ID**: `cpt-cf-task-engine-fr-recurring`

The system **SHOULD** support recurring task definitions with cron-like schedules. Overlap policies (skip, enqueue, cancel-previous) **SHOULD** be configurable.

- **Rationale**: Cron-like scheduling reduces the need for external schedulers.
- **Actors**: `cpt-cf-task-engine-actor-api-consumer`

## 6. Non-Functional Requirements

### 6.1 Gear-Specific NFRs

> **Default guidelines**: Project-wide NFR baselines are defined in this repository's [architecture manifest](../../../docs/ARCHITECTURE_MANIFEST.md) and [guidelines/](../../../guidelines/). This section captures only gear-specific NFRs.

#### Claim Latency

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-latency`

Task claim **MUST** complete within 50ms (p50) and 200ms (p99) under normal load, excluding long-poll wait time.

- **Threshold**: p50 ≤ 50ms, p99 ≤ 200ms measured over a 5-minute window with ≥ 100 concurrent workers claiming from ≥ 10 queues.

#### Throughput

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-throughput`

The system **MUST** sustain at least 1,000 task creations/second and 500 claims/second per node under normal load.

- **Threshold**: Sustained for 60 seconds with ≤ 5% error rate on a single node with PostgreSQL backend.

#### Cache Degradation

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-availability`

Cluster cache unavailability **MUST NOT** prevent task operations. The system **MUST** fall back to database-only dispatch transparently.

- **Threshold**: All task lifecycle operations succeed within 2× normal latency when the Cluster cache is unreachable for 5 minutes.

#### Timer Accuracy

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-timer-accuracy`

Timer expiry (queue timeout, assignment timeout, execution timeout, heartbeat timeout, lifetime timeout, SLA deadline) **MUST** be accurate within 1 second under normal load.

- **Threshold**: 99% of timers fire within 1 second of their scheduled expiry with 10,000 active timers.

#### Tenant Isolation

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-tenant-isolation`

Tasks belonging to one tenant **MUST NOT** be visible or accessible to another tenant under any API path. `SecureConn` tenant predicates **MUST** be enforced on every query.

- **Threshold**: Zero cross-tenant data leakage across all API endpoints verified by automated isolation tests with ≥ 3 tenants.

#### Request Payload Limits

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-payload-size`

The total serialized payload of a task (input + context + output combined, measured as uncompressed canonical JSON per §5.1) and the payload of each task entry **MUST** be limited to a configurable maximum (default 64 KB). The Task Engine is not a place to store logs, blobs, or other massive artifacts — clients **MUST** use external file storage and reference uploaded content by URL/ID. Request body size **MUST** be limited to a configurable maximum (default 1 MB). Bodies exceeding a warning threshold (default 256 KB) **MUST** generate warning-level log entries.

**Enforcement for unknown-length bodies**: The request-body limit **MUST NOT** rely solely on the `Content-Length` header. For requests where a usable `Content-Length` is present, the body **MUST** be rejected before reading when the declared length exceeds the limit. For requests with no usable declared length — including HTTP chunked transfer-encoding and HTTP/2 bodies — the server **MUST** count bytes while streaming and abort with `413 Payload Too Large` as soon as the accumulated size exceeds the configured maximum, without buffering the entire body.

- **Threshold**: Task payloads exceeding 64 KB are rejected with `413 Payload Too Large`. HTTP request bodies exceeding 1 MB are rejected within 10ms of reading the `Content-Length` header when present, or aborted mid-stream within one buffer read after the limit is crossed when no `Content-Length` is available.

#### Storage Compression

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-compression`

The system **MUST** apply transparent compression to JSON blob columns (input, context, output, task entry payload, event history payload) stored in the database. Blobs larger than a configurable threshold (default 1 KB) **MUST** be compressed before writing and decompressed on read.

- **Threshold**: Compression reduces average storage footprint by ≥ 40% for typical JSON payloads > 1 KB with < 1ms per-blob compression overhead **and < 1ms per-blob decompression overhead** on the read path.

#### Search Latency

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-search-latency`

Search **MUST** complete within bounded latency at all response-detail levels, explicitly accounting for the cost of decompressing blob columns (input, context, output) that `long` and `full` levels return (§5.17), and the bounded embedded event history at `full` level.

- **Threshold**: Over a 5-minute window at the §6.1 capacity profile (500M total tasks, 100K active) on a PostgreSQL backend, with a page size of 50: `tiny`/`short`/`count` searches p95 ≤ 100ms; `long` searches (with blob decompression) p95 ≤ 300ms; `full` searches (blob decompression + capped event history) p95 ≤ 500ms. Per-blob decompression overhead is bounded by the Storage Compression NFR (< 1ms per blob).

#### Capacity Planning

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-capacity`

The system **MUST** operate correctly and within latency/throughput NFR thresholds under the following reference deployment profile:

- Up to **100,000 tenants**
- Up to **500 million tasks** in the database (before retention cleanup)
- **100-day** default retention period for completed/failed tasks
- Up to **100,000 active (running) tasks** concurrently
- Up to **5,000 parallel client connections**

All queries, claim operations, and administrative endpoints **MUST** remain within stated latency thresholds at these volumes. Index design, connection pooling, and retention cleanup **MUST** be validated against this profile.

- **Threshold**: Full task lifecycle operations (create, claim, heartbeat, complete, search) meet latency NFRs with 500M total tasks, 100K active tasks, and 5K connections on a PostgreSQL backend.

#### Graceful Shutdown

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-graceful-shutdown`

The system **MUST** drain in-flight requests, flush the progress cache, and close database connections on shutdown. Shutdown **MUST** complete within a configurable deadline (default 30 seconds).

- **Threshold**: Zero dropped in-flight requests during shutdown with ≤ 50 concurrent requests. Shutdown completes within 30 seconds.

#### Code Coverage

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-code-coverage`

The gear crate **MUST** achieve at least 85% line coverage. Combined unit + integration test coverage **MUST** meet or exceed 90%.

- **Threshold**: 85% line coverage verified by `cargo llvm-cov`.

#### Security

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-security`

All database access **MUST** use `SecureConn` with `AccessScope` — no raw database connections in handler/service/repository code. All authorization decisions **MUST** be obtained via `PolicyEnforcer` (fail-closed). Task input/output payloads **MUST** be treated as opaque and never interpreted as executable content.

- **Threshold**: Zero instances of raw DB access outside migrations. Zero authorization bypasses verified by static analysis and integration tests.

#### Data Protection and Privacy Controls

- [ ] `p1` - **ID**: `cpt-cf-task-engine-nfr-data-protection`

Although the Task Engine treats task/entry payloads as opaque and does not process personal data of its own, it **stores** caller-supplied metadata (tenant, owner, subject, and actor identities) and opaque payloads and event history. Storing this data does not remove access, retention, or protection obligations, so the following controls **MUST** be provided (rather than excluded as "not applicable"):

- **Access control**: All stored task data (metadata and payloads) is reachable only through tenant-scoped, PDP-authorized access (§5.11); there is no unauthenticated or cross-tenant read path.
- **Encryption**: All API traffic **MUST** use TLS (encryption in transit). Encryption at rest is a deployment/database responsibility and **MUST** be supported (the gear stores no data outside the configured databases).
- **No payloads in logs**: Payload and entry contents **MUST NOT** be written to logs, metrics, traces, or error messages (DESIGN §4.4 enumerates what is and is not logged).
- **Retention and deletion**: Configurable retention with scheduled cleanup (§5.14) supports caller data-lifecycle/erasure obligations. **Limitation**: because payloads are stored compressed as opaque blobs, field-level redaction inside a payload is not possible via SQL — erasure is achieved by deleting the whole task (or rewriting the blob at the application layer); callers requiring field-level erasure **MUST** model erasable data outside opaque payloads (DESIGN §4.5).
- **Caller PII responsibility**: The Task Engine has no knowledge of which payload fields constitute PII; callers remain responsible for identifying and encrypting sensitive fields before submission.

- **Threshold**: All task-data read paths are authenticated + tenant-scoped (verified by isolation tests, §6.1 Tenant Isolation); zero payload/entry contents present in logs verified by log-scanning tests; TLS enforced on all endpoints.

### 6.2 NFR Exclusions

- **Accessibility (WCAG)**: Not applicable — no user-facing UI.
- **Internationalization**: Not applicable — English API/logs; task payloads are opaque JSON.
- **Data Classification**: Task payloads (input, context, output) and task entry payloads are caller-controlled opaque JSON. The Task Engine does not inspect, index, or log payload contents. Callers are responsible for ensuring that any PII placed in payloads complies with applicable regulations. When PII may be present, callers are responsible for encrypting sensitive fields before submission — the Task Engine treats all payloads as opaque and has no knowledge of which fields constitute PII.
- **Safety (ISO 25010 §4.2.9)**: Not applicable — information system with no physical interaction.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### Task Engine Client Trait (`task-engine-sdk`)

- [ ] `p1` - **ID**: `cpt-cf-task-engine-interface-client-trait`

- **Description**: Async trait defining all task lifecycle operations. Implemented by both the in-process gear and an HTTP client for remote access.
- **Breaking Change Policy**: Semver; trait changes are breaking.

#### GTS Type Definitions (`task-engine-sdk`)

- [ ] `p1` - **ID**: `cpt-cf-task-engine-interface-gts-types`

- **Description**: GTS base type definitions for task types and entry types, generated via `#[struct_to_gts_schema]`. Task states, result codes, wait condition kinds, timer types, and other engine-owned finite sets are closed SDK/domain enums governed by the enum-evolution rules in §5.12 (stable discriminants, string wire encoding, unknown-value handling).
- **Breaking Change Policy**: GTS type_id changes are breaking. Per §5.12: **renumbering, removing, or repurposing** an enum discriminant is breaking, while **adding** a new enum variant is a minor change provided clients honor the unknown-value handling contract. Additive schema fields are minor.

### 7.2 External Integration Contracts

#### Task Engine REST API Contract

- [ ] `p1` - **ID**: `cpt-cf-task-engine-contract-rest-api`

- **Direction**: provided by the gear
- **Protocol/Format**: HTTP/JSON REST API at `/api/task_engine/v1`
- **Compatibility**: Backward-compatible additive evolution within a major version

## 8. Use Cases

#### Create, Claim, and Execute a Task

- [ ] `p1` - **ID**: `cpt-cf-task-engine-usecase-create-execute`

**Actor**: `cpt-cf-task-engine-actor-api-consumer`, `cpt-cf-task-engine-actor-worker`

**Preconditions**:
- Task Engine is running and accessible
- Worker is polling the target queue

**Main Flow**:
1. API consumer creates a task with type, queue, priority, and input payload
2. System validates, assigns ID, persists, sets timers, publishes `taskCreated` event
3. Worker claims a task from the queue; system atomically assigns it with a lease
4. Worker starts execution, sends heartbeats periodically to renew the lease
5. Worker reports progress, optionally checkpoints intermediate state and creates task entries
6. Worker completes the task with a structured result
7. System publishes `taskCompleted` event, cleans up timers

**Postconditions**: Task is in `completed` state with structured result; full event history recorded.

**Alternative Flows**:
- **Worker fails to heartbeat**: Lease expires → task returned to queue, worker blacklisted, retry counter incremented
- **Cancellation requested**: Worker detects via heartbeat, transitions task to `cancelled` with `cancelled` result code
- **Max retries exhausted**: Task moved to dead-letter queue

#### Task Waits for External Event

- [ ] `p2` - **ID**: `cpt-cf-task-engine-usecase-event-wait`

**Actor**: `cpt-cf-task-engine-actor-worker`, `cpt-cf-task-engine-actor-event-broker`

**Preconditions**:
- Task is in `running` state

**Main Flow**:
1. Worker sets an event wait condition specifying event type, subject, and match expression
2. System transitions task to `waiting`
3. An external system publishes a matching event via the Event Broker
4. Task Engine evaluates the match expression against the event; condition satisfied
5. Task resumes execution with the event data available

**Postconditions**: Task resumed with event data; wait resolved in event history.

#### Retry with Backoff After Transient Failure

- [ ] `p1` - **ID**: `cpt-cf-task-engine-usecase-retry`

**Actor**: `cpt-cf-task-engine-actor-worker`

**Preconditions**:
- Task has a retry policy configured

**Main Flow**:
1. Worker completes a task with a retryable error result
2. System classifies the error as retryable, increments attempt counter
3. System computes the next `run_after` from the backoff strategy
4. Task transitions to `scheduled` with the computed `run_after`
5. At `run_after` time, task transitions to `queued` and becomes claimable
6. A worker claims and retries the task

**Postconditions**: Task retried with backoff; attempt history recorded.

**Alternative Flows**:
- **Max retries exhausted**: Task moved to dead-letter queue; `taskDeadLettered` event published

## 9. Acceptance Criteria

- [ ] Task lifecycle (create → claim → start → heartbeat → progress → complete) executes end-to-end with correct state transitions and events
- [ ] Atomic claim prevents two workers from claiming the same task
- [ ] Lease expiry returns abandoned tasks to the queue within 1 second of the deadline
- [ ] All timeout types fire within 1 second of expiry and apply correct actions
- [ ] Retry policy re-enqueues with correct backoff delay; `attempt` is incremented exactly once per lifecycle (on the subsequent claim, not on the retry re-enqueue)
- [ ] Dead-letter queue captures tasks that exhaust retries; DLQ tasks are re-enqueueable
- [ ] Wait conditions (timer, event, input) correctly suspend and resume tasks
- [ ] Event wait condition evaluates match expressions against incoming events
- [ ] Concurrency limits are enforced atomically on claim across every dimension — queue global, queue per-tenant, type global, and type per-tenant — with no more than the configured N tasks executing per dimension, verified under concurrent claims
- [ ] Multi-tenant isolation: tenant A cannot see or modify tenant B's tasks
- [ ] PolicyEnforcer authorization enforces all permission boundaries via PDP decisions
- [ ] Bulk operations evaluate authorization per task and return per-task partial-success outcomes
- [ ] GTS task type validation rejects input that violates the registered schema
- [ ] Aggregate payload limit (input + context + output, uncompressed canonical JSON) is enforced on create, checkpoint/context-update, and completion
- [ ] Parent/child task hierarchies track completion and propagate cancellation (including forced parent cancellation propagating to children)
- [ ] Dependencies prevent blocked tasks from becoming claimable until prerequisites complete; cycle validation runs on every dependency mutation
- [ ] Cross-domain claim, parent/child, and dependency relationships are rejected with 422
- [ ] Cursor-based pagination returns stable results across concurrent inserts
- [ ] Idempotent task creation returns the existing task on duplicate key (in any state) and rejects a same-key/different-payload request with 409
- [ ] SLA breach detection publishes `slaBreached` event when deadline passes (task continues executing)
- [ ] Lifetime timeout and retry-exhaustion converge on one terminal rule (dead-lettered if DLQ configured, else failed)
- [ ] Graceful shutdown completes within 30 seconds

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| ToolKit framework | OperationBuilder, SecureConn, SecurityContext, AccessScope, Scopable, pep_properties, ClientHub, RFC 9457 canonical errors | p1 |
| AuthN Resolver gear | Validates bearer tokens and produces `SecurityContext` | p1 |
| AuthZ Resolver gear | Provides authorization decisions and query constraints | p1 |
| Tenant Resolver gear | Provides tenant hierarchy, subtree, and barrier semantics | p1 |
| Resource Group Resolver gear | Provides resource-group hierarchy for group-scoped policies | p2 |
| authz-resolver-sdk `PolicyEnforcer` | Enforces access policies at the gear level | p1 |
| GTS Registry (gts-rust) | Registers and resolves task, queue, entry type, and tag-key schemas and well-known instances | p1 |
| Event Broker gear (upcoming) | Receives task lifecycle notification events; delivers inbound events for event wait conditions | p1 |
| ToolKit DB layer (`cf-gears-toolkit-db`) | SeaORM-based ORM with `DbManager`, `SecureConn`, `AccessScope`, `Scopable`, connection pooling, and migration support | p1 |
| Cluster gear | Distributed cache primitive for optional ready-task caching ([Cluster PRD](../../system/cluster/docs/PRD.md)) | p3 |
| CEL evaluator | Evaluates match expressions for event wait conditions | p2 |

## 11. Assumptions

- The ToolKit DB layer (`cf-gears-toolkit-db`) provides the ORM abstraction via SeaORM; backend selection is via DSN scheme (`postgresql://`, `mysql://`, `sqlite://`)
- Authentication is handled by the API Gateway AuthN middleware; the Task Engine receives a ready-to-use `SecurityContext`
- Authorization decisions are obtained via `PolicyEnforcer` from `authz-resolver-sdk`; the PDP (AuthZ Resolver) is available and configured
- All DB access uses `SecureConn` with `AccessScope` compiled from PDP constraints; SeaORM entities derive `Scopable`
- Tenant hierarchy is resolved by the Tenant Resolver gear
- GTS registry is available for type validation; tasks with unregistered types are accepted without input schema validation
- Event Broker gear (when available) delivers events for wait condition evaluation with at-least-once semantics
- The Cluster gear, when configured, provides a distributed cache primitive; the Task Engine does not depend on Redis directly

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Timer wheel stall under high load | Tasks timeout late, causing cascading delays | Multiple timer processor workers; timer channel size monitoring |
| ID generation contention under high concurrency | Task creation temporarily blocked | Sleep-and-retry; alert on exhaustion |
| Cluster cache desync | Stale tasks in ready-task cache after Cluster backend recovery | Cache rebuild on reconnect; graceful degradation to DB-only dispatch |
| Wait condition event matching overhead | High event throughput degrades wait condition evaluation | Index active wait conditions by event type; batch evaluation |
| CEL expression evaluation cost | Complex expressions slow event processing | Expression complexity limits; precompilation; timeout on evaluation |
| DLQ growth without operator attention | Dead-letter queue grows unboundedly | DLQ size metrics and alerting; configurable DLQ retention |
| Concurrency limit enforcement accuracy | Race conditions in concurrent claims | Database-level atomic counting; Cluster-backed semaphore for high throughput |
| Progress batch data loss on crash | Uncommitted progress updates lost | Configurable flush interval; trade-off accepted for write pressure reduction |
| Large task payloads exceeding limits | Task creation rejected; potential OOM | 64 KB per-task/entry payload limit; external file storage for large artifacts; HTTP body limit with warnings |

## 13. Open Questions

- How should event wait condition evaluation scale when thousands of tasks are waiting for different event types?
- What is the recommended approach for wait condition timeout — fail the task or resume with a timeout flag?
- What is the maximum supported depth for parent/child task hierarchies?
- Should the system support "sticky" assignment — preferring the same worker for retried tasks?

> **Resolved**: The default retention period is **100 days** (global default; see §5.14 Scheduled Retention and the §6.1 capacity profile). Retention precedence is tenant → most-specific task-type → global default.

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/)
- **Features**: [features/](./features/)

