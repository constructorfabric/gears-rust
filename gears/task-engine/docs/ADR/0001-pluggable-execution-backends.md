---
status: accepted
date: 2026-10-07
description: "Build the Task Engine as one simple operational core in its own database. Send search, analytics, and history to other gears."
---

# Build the Task Engine as a Single Operational Core

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Wrap an Existing Engine](#option-1-wrap-an-existing-engine)
  - [Option 2: Single Core with Infrastructure Adapters](#option-2-single-core-with-infrastructure-adapters)
  - [Option 3: Tiny Gateway with Pluggable Execution Backends](#option-3-tiny-gateway-with-pluggable-execution-backends)
  - [Option 4: Single Operational Core Without Plugins](#option-4-single-operational-core-without-plugins)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-implementation-architecture`

## Context and Problem Statement

The Task Engine must run distributed tasks in a safe way. It must also provide many Gears capabilities: GTS typing, multi-tenant authorization, queue admission, concurrency limits, leases and fencing, durable waits, retries, dependencies, deduplication, supersession, progress, search, retention, and task-domain routing.

We must decide where task execution lives. There are four choices:

1. Use an existing task engine and wrap it.
2. Build one core and add storage adapters for different databases.
3. Build a tiny gateway and let each deployment plug in its own execution backend.
4. Build one simple core in our own database and send extra jobs (search, analytics, history) to other gears.

## Decision Drivers

* Keep one source of truth for task state.
* Keep task admission, claim, leases, and the event outbox in one atomic transaction.
* Keep the default deployment small. Do not require an extra server to run tasks.
* Support PostgreSQL, MariaDB/MySQL, and SQLite through the ToolKit database layer.
* Support the target size: up to 500 million stored tasks and 100,000 active tasks.
* Keep the design as simple as possible for now.

## Considered Options

1. **Wrap an existing engine** — put Temporal, Hatchet, or Conductor OSS behind a thin API.
2. **Single core with infrastructure adapters** — one core, with custom storage adapters for different infrastructures (SQL, NoSQL).
3. **Tiny gateway with pluggable execution backends** — a small gateway, and each deployment uses its own executor (SQL, NoSQL).
4. **Single operational core without plugins** — our own execution in one database; the simplest design; scale with sharding and send search, analytics, and history to other gears.

## Decision Outcome

Chosen option: **Option 4 — Single operational core without plugins.**

We choose this option because it is the simplest design and it keeps one source of truth. The Task Engine runs all task logic itself in one database. We do not add a plugin layer so far and we do not add a second system that also holds task state.

The Task Engine runs these jobs directly in its own core and its own database:

* task lifecycle and state changes;
* GTS task, queue, entry, and tag identifiers, and payload validation;
* authentication context, tenant resolution, PDP authorization, and `AccessScope`;
* task admission and queue-depth limits;
* queue, tenant, type, and global concurrency counting;
* atomic claim and assignment;
* execution attempts;
* leases, lease tokens, and lease epochs;
* heartbeat and worker liveness;
* timeout processing;
* retry and dead-letter behavior;
* timer, event, and input wait conditions;
* parent/child and predecessor dependencies;
* cancellation;
* idempotency, derived deduplication, and supersession;
* progress and checkpoints;
* task entries;
* lifecycle event history and Event Broker integration;
* resource references and tags;
* recurring schedules, once added (deferred, PRD §5.22);
* retention policy;
* task-domain routing;
* REST API and Rust SDK.

The Task Engine does **not** add internal plugins for storage. If a deployment needs heavy search, large-scale analytics, or long-term history, it uses **other gears** for those jobs. For example, it can send lifecycle events to an analytics or history gear through the Event Broker. These other gears never hold the true task state and never take part in claim, lease, or concurrency decisions.

To scale, the Task Engine uses **sharding** inside its own database model (hot-row sharding for counters, shard keys for background loops). It does not scale by adding plugins.

### Consequences

* The Task Engine does not depend on Temporal, Hatchet, Conductor OSS, or any other outside execution engine.
* The relational database is the one true source for all task state.
* Claim, queue order, concurrency counting, leases, retries, waits, dependencies, and timers all run as direct database transactions.
* There is no second copy of running work in another system. There is no need for reconciliation or orphan-job repair.
* PostgreSQL, MariaDB/MySQL, and SQLite can all serve as the operational database when they meet the transaction rules.
* The operational database holds everything the engine needs to make a decision. The engine never reads an eventually consistent system to decide a claim, a lease, or a limit.
* All task payloads (input, context, output, entries, events) stay inside the operational database, within the PRD size limit (64 KB per payload). The Task Engine does not become a file store.
* Search and analytics can be offloaded to other gears. These gears receive data through published lifecycle events, not by sharing the Task Engine database.
* If a search or analytics gear is slow or down, task creation, claim, heartbeat, retries, waits, and completion still work.
* The default deployment needs only the Task Engine and one supported database.
* There is no runtime plugin ABI and no plugin version to manage.
* History and archiving are out of scope for now. Later, a deployment MAY add an optional history/archiving plugin that stores **only completed tasks** and is read back transparently through the existing task read APIs. The current release ships no such plugin, and adding it would need a new ADR.
* Moving task execution to an outside engine in the future needs a new ADR.

### Confirmation

We confirm this decision through architecture review, code review, integration tests, failure-mode tests, and capacity tests.

Architecture review MUST check that:

* there is exactly one true store for task state;
* claim and execution never depend on an outside or eventually consistent system;
* no outside execution engine is needed for normal work;
* search, analytics, and history offload happens through events to other gears, not through a shared database.

Operational-store tests MUST check:

* atomic task admission;
* atomic queue-depth counting;
* atomic concurrency reservation;
* no double claim;
* lease fencing;
* retry and timeout recovery;
* dependency correctness;
* deduplication and supersession correctness;
* task recovery after a restart.

Capacity tests MUST check the design against the PRD target size (500 million stored tasks, 100,000 active tasks), including sharded counters and background loops.

## Pros and Cons of the Options

### Option 1: Wrap an Existing Engine

Use Temporal, Hatchet, or Conductor OSS as the main engine. The Task Engine is a thin API in front of it and adds the missing Gears rules.

* Good, because the engine already has a mature scheduler, retries, timers, and crash recovery.
* Good, because there is less generic execution code to write.
* Bad, because our rules still need our own database in front — task entries, queue depth, dedup and suspension records, tenant-scoped queries and joins, lease epoch, and outbox events.
* Bad, because we would still run an extra server, and there is no edge support.
* Bad, because there are two sources of truth, so we must reconcile them.

**Rejected.**

### Option 2: Single Core with Infrastructure Adapters

Build one core and add custom storage adapters for different infrastructures (SQL, NoSQL). The adapters also provide large payloads, analytics, search, and history.

* Good, because any logic can be customized through adapters.
* Good, because large payloads, analytics, search, and history go through the infrastructure adapters.
* Bad, because the implementation becomes complex and the test coverage is hard to keep.
* Bad, because we still build and run the scheduler ourselves, so the adapter layer adds cost without removing our main work.

**Rejected.**

### Option 3: Tiny Gateway with Pluggable Execution Backends

Build a small gateway. Each deployment plugs in its own executor (SQL, NoSQL).

* Good, because it looks flexible and powerful on paper.
* Bad, because the plugin API must be a lowest-common-denominator, so it loses power.
* Bad, because the state machine is duplicated per backend.
* Bad, because the plugin ABI is complicated.
* Bad, because we still build and run the scheduler ourselves, now in different storages.

**Rejected.**

### Option 4: Single Operational Core Without Plugins

Build one core engine in one database. Keep execution native. Scale with sharding. Send search, analytics, and history to other gears.

* Good, because there is a single core engine with atomic transactions for admission, claim, leases, and outbox.
* Good, because it is the simplest possible implementation.
* Good, because there is one source of truth and no reconciliation.
* Good, because the default deployment is small: only the engine and one database.
* Neutral, because we offload heavy search, analytics, and archiving to other gears when a deployment needs them.
* Bad, because task-engine clients will need to interact with other gears for search, analytics, and history.
* Bad, because we still build and run the scheduler ourselves (true for every option except Option 1).

**Chosen.**

## More Information

"Execution in the core" means execution *state*; task code never runs in the Task Engine. Workers are separate processes, and in-gear execution on a local worker belongs to [Durable Execution](../../../../libs/toolkit-durable-execution/docs/PRD.md) (DESIGN `cpt-cf-task-engine-design-positioning`).

The design keeps execution and the true state in one place:

```text
Worker
   |
   v
Task Engine Core
   |
   v
Operational Database (PostgreSQL / MariaDB/MySQL / SQLite)
```

Heavy, non-critical work is sent to other gears through events. These gears do not hold task state:

```text
Task Engine Core
   |
   | lifecycle events (Event Broker)
   v
Other gears (optional):
   - search gear      (text / rich search over task data)
   - analytics gear   (reports, metrics, aggregates)
   - history gear     (long-term archive of old tasks)
```

The Task Engine scales inside its own model:

```text
- hot-row sharding for concurrency and queue-depth counters
- shard keys for timer and lease background loops
- task domains to route task types to separate databases
```

For the target size (up to 500 million stored tasks), sharding and task domains give the needed scale while keeping the design simple and keeping one source of truth.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision supports these requirements and design elements:

* `cpt-cf-task-engine-fr-task-create` — creation, validation, timers, admission, and event writes stay inside Task Engine transactions.
* `cpt-cf-task-engine-fr-structured-io` — task input and output stay inside the operational database within the size limit.
* `cpt-cf-task-engine-fr-task-checkpoint` — checkpoint and context stay as execution state in the operational database.
* `cpt-cf-task-engine-fr-task-entries` — entries stay inside Task Engine control and storage.
* `cpt-cf-task-engine-fr-queue-concurrency` — concurrency stays authoritative in the operational database.
* `cpt-cf-task-engine-fr-queue-depth` — admission stays atomic with operational state.
* `cpt-cf-task-engine-fr-wait-conditions` — wait state stays native Task Engine state.
* `cpt-cf-task-engine-fr-assignment` — assignment and attempt logic stay native.
* `cpt-cf-task-engine-fr-lease-fencing` — lease correctness does not depend on any other system.
* `cpt-cf-task-engine-fr-retry` — retry state stays in the operational database.
* `cpt-cf-task-engine-fr-dependencies` — dependency updates stay transactional.
* `cpt-cf-task-engine-fr-task-domains` — each task domain keeps its own operational database; sharding and routing give scale.
* `cpt-cf-task-engine-nfr-throughput` — native claim and execution avoid extra network hops.
* `cpt-cf-task-engine-nfr-search-latency` — a separate search gear can serve large-scale search when needed.
* `cpt-cf-task-engine-nfr-capacity` — sharding, task domains, and offload to other gears keep the operational database within the target size.
* `cpt-cf-task-engine-nfr-tenant-isolation` — all task data stays behind Task Engine tenant-scoped access.
* `cpt-cf-task-engine-nfr-security` — authorization stays inside the Task Engine core.
* `cpt-cf-task-engine-usecase-create-execute` — execution stays inside the Task Engine gear.
