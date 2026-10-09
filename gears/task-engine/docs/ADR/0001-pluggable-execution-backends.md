---
status: accepted
date: 2026-10-07
description: "Choose native Task Engine execution with modular, integrated storage infrastructure."
---

# Implement Task Engine as a Modular Gear with Integrated Storage Infrastructure


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Reuse an Existing Execution Service Behind a Thin Task Engine Wrapper](#reuse-an-existing-execution-service-behind-a-thin-task-engine-wrapper)
  - [Implement Task Engine as a Fixed DB-only Monolithic Gear](#implement-task-engine-as-a-fixed-db-only-monolithic-gear)
  - [Implement Task Engine Core with Pluggable Execution Backends](#implement-task-engine-core-with-pluggable-execution-backends)
  - [Modular Gear with Integrated Storage Infrastructure](#modular-gear-with-integrated-storage-infrastructure)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-implementation-architecture`
## Context and Problem Statement

The Task Engine requires durable distributed task execution together with Gears-specific semantics including GTS typing, multi-tenant authorization, queue admission, concurrency control, leases and fencing, durable waits, retries, dependencies, deduplication, supersession, progress, search, retention, and task-domain routing.

The architectural decision is whether to delegate execution to an existing task/workflow service, implement everything as one fixed monolithic gear, introduce pluggable execution backends, or keep execution native while allowing the gear itself to integrate different storage technologies through narrow infrastructure abstractions.

## Decision Drivers

* Preserve the Task Engine PRD semantics without adapting them to the execution model of a third-party workflow or task engine.
* Maintain one authoritative representation of task execution state.
* Preserve atomic transactions for task admission, concurrency reservation, assignment, leases, dependencies, deduplication, supersession, timers, and event outbox writes where required.
* Keep the default deployment lightweight and avoid requiring a separate execution cluster.
* Support PostgreSQL, MariaDB/MySQL, and SQLite through the existing ToolKit database abstraction where their capabilities satisfy Task Engine requirements.
* Support the reference capacity profile, including up to 500 million retained tasks and 100,000 active tasks.
* Allow large payloads, historical task data, analytics data, and search indexes to be moved out of the operational database.
* Allow to scale the volume being processed with ClickHouse, Elasticsearch/OpenSearch, object storage, distributed cache, or future infrastructure technologies without changing Task Engine domain semantics.
* Avoid runtime plugin infrastructure where normal compile-time Rust modules and dependency injection are sufficient.
* Allow storage layout and infrastructure topology to evolve independently from the public Task Engine API.
* Minimize runtime dependencies, network hops, reconciliation logic, and failure modes on the execution hot path.

## Considered Options

1. Reuse an existing execution service behind a thin Task Engine wrapper
2. Implement Task Engine as a fixed monolithic DB-only gear
3. Implement Task Engine core with pluggable execution backends
4. Implement Task Engine as a monolith gear with multiple integrated storage infrastructures

## Decision Outcome

Chosen option: "4. Implement Task Engine as a monolith gear with multiple integrated storage infrastructures", because Task Engine itself must own the correctness-critical execution semantics, while storage responsibilities have clear and useful boundaries that can be implemented by different technologies without duplicating the Task Engine state machine.

The Task Engine SHALL implement scheduling, claiming, retries, waits, leases, dependencies, concurrency, deduplication, supersession, and worker coordination directly.

The gear SHALL remain one deployed Task Engine subsystem rather than delegating execution to interchangeable task-engine plugins.

Internally, the implementation SHALL separate domain/application logic from infrastructure implementations.

Task Engine Core owns:

* task lifecycle and state transitions;
* GTS task, queue, entry and tag identifiers and payload validation;
* authentication context, tenant resolution, PDP authorization, and `AccessScope`;
* task admission and queue-depth limits;
* queue-, tenant-, type-, and global concurrency accounting;
* atomic claim and assignment;
* logical execution attempts;
* leases, lease tokens, and lease epochs;
* heartbeat and worker liveness;
* timeout processing;
* retry and dead-letter behavior;
* timer, event, and input wait conditions;
* parent/child relationships and predecessor dependencies;
* cancellation;
* caller idempotency, derived deduplication, and supersession;
* progress and checkpoints;
* task entries;
* lifecycle event history and Event Broker integration;
* resource references and tags;
* recurring schedules;
* retention policy;
* task-domain routing;
* REST API and Rust SDK.

Infrastructure responsibilities are separated by role:

* **Operational store** — correctness-critical state used by execution and transactions.
* **Payload store** — large or opaque content not required directly for transactional execution.
* **Search store** — optional derived index optimized for querying or full-text search.
* **Analytics store** — optional sink optimized for aggregations and large analytical scans.
* **Historical archive** — optional storage for cold terminal task data and long retention.
* **Cache / notification infrastructure** — optional acceleration mechanisms that do not own canonical task state.

These are internal infrastructure abstractions, not runtime execution plugins.

### Consequences

* Task execution does not depend on Temporal, Hatchet, Conductor OSS, or another external workflow/task execution engine.
* The Task Engine relational operational database remains authoritative for all correctness-critical task state.
* Scheduling, claiming, queue ordering, concurrency accounting, leases, retries, waits, dependencies, and timers operate directly against Task Engine-controlled state.
* No duplicate execution representation is required in another task execution service.
* No external execution-engine outbox, submission reconciliation, orphan-job recovery, or backend-state synchronization is required for normal execution.
* PostgreSQL, MariaDB/MySQL, and SQLite MAY implement the operational-store role when they satisfy the required transactional semantics.
* The operational store MUST contain all state needed to make task execution decisions without consulting eventually consistent secondary systems.
* Status, queue, priority, attempts, leases, concurrency counters, dependencies, active waits, timers, deduplication state, supersession state, and other execution-critical metadata MUST remain in the operational store.
* Large or opaque content MAY be stored through a narrow `PayloadStore` infrastructure contract.
* Candidate externally stored content includes task input, task output, immutable task-entry payloads, event payloads, historical checkpoints, and other large opaque JSON content.
* Small or frequently accessed payloads MAY remain inline in the operational database.
* The implementation SHOULD support configurable inline-vs-external storage based on payload size or content category.
* A committed operational record MUST NOT reference external payload content that failed to persist.
* External immutable payloads SHOULD therefore be written before the transaction committing their reference.
* Failed operational transactions MAY leave unreferenced payloads, which MUST be safe to garbage-collect later.
* External payload references SHOULD carry enough integrity metadata to validate retrieved content, such as size, content type, compression method, and checksum.
* Current mutable context/checkpoint state SHOULD remain operational unless DESIGN explicitly adopts immutable versioned external payloads with atomic reference replacement.
* ClickHouse MAY be used for append-heavy analytics, historical lifecycle records, archived terminal tasks, or large immutable content where its access characteristics are appropriate.
* ClickHouse MUST NOT participate in atomic task claiming, lease ownership, concurrency reservation, or similar correctness-critical operations.
* Elasticsearch/OpenSearch MAY be used as a derived search index.
* Elasticsearch/OpenSearch MUST NOT be authoritative for task state.
* Elasticsearch/OpenSearch SHOULD only store large payload content when that content must actually be indexed or searched; it is not the default generic payload store.
* Object storage or another blob-oriented implementation MAY implement `PayloadStore` for large opaque content.
* Historical terminal records MAY eventually be moved from the operational database into a `TaskHistoryArchive` implementation to reduce the operational working set.
* The Task Engine query layer MAY combine operational and archived storage when a request spans both hot and historical data.
* Secondary search, analytics, and archive systems MAY be eventually consistent unless a specific requirement states otherwise.
* Failure of optional analytics or search infrastructure MUST NOT stop task creation, claiming, heartbeat, retries, waits, or completion.
* Failure of a configured external payload store MAY reject operations requiring new external payload persistence but MUST NOT corrupt already committed Task Engine state.
* Infrastructure implementations SHOULD be compiled Rust modules or crates selected by configuration and dependency injection rather than runtime-loaded plugins.
* A stable binary plugin ABI is not introduced by this decision.
* The default deployment SHOULD require only the Task Engine and a supported operational database.
* Optional infrastructure MUST be introduced only when it provides a measurable capacity, performance, search, retention, or cost benefit.
* Internal module boundaries MUST allow infrastructure implementations to change without changing Task Engine domain behavior or the public API.
* Moving execution semantics to an external task/workflow engine in the future requires a separate ADR.

### Confirmation

Compliance with this ADR is confirmed through architecture review, code review, integration tests, failure-mode tests, and storage-specific conformance tests.

Architecture review MUST verify that:

* exactly one authoritative operational representation exists for task execution state;
* scheduling and execution correctness do not depend on ClickHouse, Elasticsearch/OpenSearch, object storage, or another secondary store;
* domain/application logic does not directly depend on infrastructure-specific APIs;
* infrastructure interfaces represent narrow capabilities rather than generic database operations;
* optional infrastructure failure cannot corrupt authoritative Task Engine state;
* no external execution service is required for normal operation.

Operational-store tests MUST verify:

* atomic task admission;
* atomic queue-depth accounting;
* atomic concurrency reservation;
* duplicate claim prevention;
* lease fencing;
* retry and timeout recovery;
* dependency correctness;
* deduplication and supersession correctness;
* task recovery after process restart.

Payload-store tests MUST verify:

* payload persistence and retrieval;
* payload integrity validation;
* correct reconstruction of inline and external payloads;
* failure before operational reference commit when payload persistence fails;
* safe garbage collection of orphaned objects;
* tenant isolation and authorization of externalized content.

Search and analytics tests MUST verify:

* secondary-store outage does not stop execution;
* derived data can be rebuilt from authoritative state or events where required;
* eventual consistency does not alter execution semantics.

Capacity testing MUST validate the selected storage topology against the PRD reference profile.

## Pros and Cons of the Options

### Reuse an Existing Execution Service Behind a Thin Task Engine Wrapper

Use Temporal, Hatchet, Conductor OSS, or a comparable system as the primary execution engine, while Task Engine exposes Gears-facing APIs and adds missing platform semantics.

* Good, because existing engines provide mature worker dispatch, retries, scheduling, timers, crash recovery, and operational tooling.
* Good, because initial implementation of generic execution mechanics may be smaller.
* Good, because mature external systems have significant production experience.
* Neutral, because Hatchet and Conductor are closer to the Task Engine's task model than Temporal, while Temporal provides broader durable-workflow semantics.
* Bad, because no evaluated engine directly implements the required GTS, authorization, tenant hierarchy, queue admission, in-transaction events, concurrency limits, lease epoch, deduplication, supersession, task entries, and Task Engine query model.
* Bad, because capabilities above turns the supposedly thin wrapper into a substantial stateful Task Engine control plane with its own canonical database and state.
* Bad, because runnable work is represented both in Task Engine and in the external engine.
* Bad, because there is no single ACID transaction covering the Task Engine database and an external execution service.
* Bad, because reliable integration therefore requires outbox processing, idempotent submission, reconciliation, orphan detection, and repair.
* Bad, because deployment availability depends on more services and network paths.
* Bad, because delegating enough behavior to gain substantial performance or capacity benefits would require changing ownership of retries, waits, timers, leases, or concurrency and therefore changing the PRD semantics.

### Implement Task Engine as a Fixed DB-only Monolithic Gear

Implement all domain semantics, execution logic, persistence, search, payload storage, analytics, and historical retention using one fixed integrated storage architecture.

* Good, because the implementation is conceptually simple and has minimal abstraction overhead.
* Good, because all functionality can be optimized for one known persistence technology.
* Good, because there are few configuration permutations and a small integration-test matrix.
* Good, because execution correctness can use direct database transactions.
* Bad, because operational state, large payloads, historical data, search indexes, and analytics have very different storage characteristics and volume.
* Bad, because one storage technology must serve transactional, analytical, search, and blob-like workloads simultaneously.
* Bad, because hundreds of millions of retained historical tasks and large payloads can increase operational database size, index size, backup volume, replication load, and working-set pressure.
* Bad, because introducing ClickHouse, Elasticsearch/OpenSearch, or object storage later would require invasive changes if infrastructure boundaries were not designed from the beginning.
* Bad, because large-content offload and historical archival provide meaningful capacity benefits even though execution itself remains native.
* Bad, because fixing infrastructure choices in the domain/application implementation creates unnecessary coupling.

### Implement Task Engine Core with Pluggable Execution Backends

Keep Task Engine semantics in a common core and define an execution-backend API implemented by native PostgreSQL, Hatchet, Conductor OSS, Temporal, or other engines.

* Good, because execution infrastructure could theoretically be replaced without changing public Task Engine APIs.
* Good, because deployments could choose between a lightweight native implementation and an external execution system.
* Good, because external task engines could provide mature worker-dispatch functionality.
* Neutral, because Task Engine Core would still implement most PRD-defined behavior regardless of backend.
* Bad, because very little useful responsibility remains in the backend if Task Engine Core owns lifecycle, attempts, retries, waits, leases, concurrency, dependencies, deduplication, and search.
* Bad, because the native relational backend would mostly wrap state already stored by Task Engine.
* Bad, because external backends create a second durable representation of queued/running work.
* Bad, because this requires synchronization and reconciliation between Task Engine and backend state.
* Bad, because backend semantics differ materially in retries, priority, concurrency, worker polling, cancellation, waits, and task identity.
* Bad, because a sufficiently narrow backend interface provides too little value to justify the additional abstraction and runtime system.
* Bad, because a wider backend interface duplicates significant portions of Task Engine semantics and creates backend-dependent behavior.
* Bad, because the architecture optimizes for hypothetical execution-engine replacement rather than demonstrated Task Engine scaling concerns.

### Modular Gear with Integrated Storage Infrastructure

Implement task execution natively inside the Task Engine gear while defining narrow internal infrastructure interfaces for operational persistence, payload storage, search, analytics, caching, and historical archive.

* Good, because there remains one authoritative Task Engine state machine and one execution model.
* Good, because execution-critical operations retain local transactional semantics.
* Good, because no external execution-engine synchronization is required.
* Good, because large content can be moved out of the operational database without changing task lifecycle semantics.
* Good, because ClickHouse can be used for append-heavy historical or analytical workloads where it provides meaningful capacity and query benefits.
* Good, because Elasticsearch/OpenSearch can be introduced specifically for search use cases rather than being forced into transactional storage.
* Good, because object storage can be introduced for large opaque payloads without affecting claim, lease, retry, or concurrency logic.
* Good, because a narrow `PayloadStore` contract can externalize a large percentage of physical bytes while leaving correctness-critical state in the relational store.
* Good, because the operational database working set can remain focused on active and recent tasks.
* Good, because historical terminal data can later be archived independently from active task execution.
* Good, because storage choices can evolve without introducing an execution plugin ABI.
* Good, because compile-time Rust modules and dependency injection are sufficient for infrastructure variation.
* Good, because optional secondary stores can fail independently without stopping core execution.
* Good, because each infrastructure abstraction can expose semantics appropriate to its role instead of forcing all databases behind a generic CRUD interface.
* Good, because this architecture directly addresses the most likely scaling pressure in the reference profile: very large retained task and payload volume with a much smaller active working set.
* Neutral, because multiple storage technologies increase deployment and operational complexity when optional stores are enabled.
* Neutral, because secondary data projections may be eventually consistent.
* Bad, because the gear must implement synchronization or archival pipelines for optional search, analytics, or historical stores.
* Bad, because externalized payloads introduce additional reads for tasks whose content is not inline.
* Bad, because multi-store deployments require explicit failure-handling, observability, retention, and backup strategies for each enabled infrastructure component.
* Bad, because infrastructure abstractions and conformance tests add some implementation complexity compared with a fixed single-database implementation.

## More Information

This decision separates **execution architecture** from **storage architecture**.

Execution remains native:

```text
Worker
   |
   v
Task Engine
   |
   v
Operational Store
```

Storage may be specialized:

```text
                         Task Engine

            +-------------------------------+
            | Domain / Application          |
            |                               |
            | lifecycle / claim / retries   |
            | waits / leases / concurrency  |
            | dependencies / GTS / AuthZ    |
            +---------------+---------------+
                            |
                    Infrastructure ports
                            |
       +--------------------+----------------------+
       |                    |                      |
       v                    v                      v
 Operational Store      Payload Store        Search / Analytics
 PostgreSQL             Object Storage       Elasticsearch
 MariaDB/MySQL          ClickHouse           OpenSearch
 SQLite                 Inline DB            ClickHouse
```

Infrastructure abstractions SHOULD be capability-specific.

Examples:

```text
TaskRepository
TaskClaimStore
PayloadStore
TaskSearch
TaskAnalyticsSink
TaskHistoryArchive
WorkerWakeup
```

A generic interface such as:

```text
Database::insert()
Database::update()
Database::query()
Database::lock()
```

SHOULD NOT be introduced as the abstraction between fundamentally different database technologies.

The likely storage model is:

```text
Operational database:
    100% correctness-critical state
    100% indexed operational metadata
    small/hot payloads

PayloadStore:
    large input/output
    immutable entries/events
    other opaque large content

ClickHouse:
    analytics
    lifecycle history
    archived terminal tasks
    optionally large immutable structured content

Elasticsearch/OpenSearch:
    derived searchable representation
    payload contents only when full-text/content search is required
```

A hybrid inline/external payload policy is expected:

```text
small payload
    -> operational database

large payload
    -> PayloadStore
    -> operational row stores PayloadRef
```

This allows a narrow infrastructure contract to move a substantial portion of physical data volume away from the operational database while keeping all execution semantics local and transactional.

For the reference scale of up to 500 million retained tasks, reducing the amount of payload and cold historical content stored in the operational database is expected to provide a more direct scalability benefit than delegating task dispatch to another execution engine.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-task-engine-fr-task-create` — creation, validation, timers, admission, and event persistence remain Task Engine-controlled transactional operations.
* `cpt-cf-task-engine-fr-structured-io` — structured input and output may be stored inline or through `PayloadStore` while preserving the same Task Engine contract.
* `cpt-cf-task-engine-fr-task-checkpoint` — current checkpoint/context remains execution state; storage representation may vary only where atomic Task Engine semantics are retained.
* `cpt-cf-task-engine-fr-task-entries` — immutable entry payloads may be externalized while entry metadata and authorization remain Task Engine-controlled.
* `cpt-cf-task-engine-fr-queue-concurrency` — concurrency remains authoritative in the operational store.
* `cpt-cf-task-engine-fr-queue-depth` — admission remains atomic with operational Task Engine state.
* `cpt-cf-task-engine-fr-wait-conditions` — wait state remains native Task Engine state.
* `cpt-cf-task-engine-fr-assignment` — assignment and attempt semantics remain native Task Engine behavior.
* `cpt-cf-task-engine-fr-lease-fencing` — lease correctness does not depend on a secondary store.
* `cpt-cf-task-engine-fr-retry` — retry lifecycle remains represented in the authoritative operational database.
* `cpt-cf-task-engine-fr-dependencies` — dependency updates and unblocking remain transactional Task Engine operations.
* `cpt-cf-task-engine-fr-task-domains` — each operational task domain retains its authoritative relational storage while secondary infrastructure may specialize payload, search, analytics, or archive responsibilities.
* `cpt-cf-task-engine-nfr-throughput` — native claim and execution avoid external execution-engine hops.
* `cpt-cf-task-engine-nfr-search-latency` — optional specialized search infrastructure may be used where required to satisfy large-scale query targets.
* `cpt-cf-task-engine-nfr-capacity` — payload offload and historical archival reduce the size and working set of the operational store at the reference scale.
* `cpt-cf-task-engine-nfr-compression` — compression remains applicable to inline and externalized payload representations.
* `cpt-cf-task-engine-nfr-tenant-isolation` — all storage roles remain reachable only through Task Engine-controlled tenant-scoped access.
* `cpt-cf-task-engine-nfr-security` — authorization remains independent of which infrastructure implementation stores payloads or derived data.
* `cpt-cf-task-engine-usecase-create-execute` — execution stays inside the Task Engine gear while content storage is independently configurable.
* `cpt-cf-task-engine-design-infrastructure-ports` — DESIGN must define role-specific infrastructure ports and their consistency, failure, retention, and reconstruction semantics.
