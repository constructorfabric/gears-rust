---
status: accepted
date: 2026-10-24
description: "Route each task type or queue to one of several configured databases (task domains) with a fixed first-match rule, and keep every atomic operation inside one domain."
---

# Route Tasks to Separate Databases with Task Domains

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [One database for all tasks](#one-database-for-all-tasks)
  - [Per-tenant sharding](#per-tenant-sharding)
  - [Task domains: route by task type or queue pattern, first match wins](#task-domains-route-by-task-type-or-queue-pattern-first-match-wins)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-task-domains`

## Context and Problem Statement

All task data lives in a relational database (ADR-0001). Some task classes behave very differently: high-volume system tasks (cleanup, sync) create a lot of rows and churn, while user-visible tasks need fast search and longer retention. When both share one database, the system tasks slow down the user-facing queries and make retention harder to tune.

How can a deployment give different task classes their own storage, without adding a second source of truth or losing atomic claims and dependencies?

## Decision Drivers

* Keep one source of truth for each task (ADR-0001).
* Isolate the load, size, and retention of different task classes.
* Keep claims, dependencies, and parent/child links atomic.
* Make routing predictable: a task must always map to the same database.
* Work without new infrastructure: only more database connections.

## Considered Options

* One database for all tasks
* Per-tenant sharding
* Task domains: route by task type or queue pattern, first match wins

## Decision Outcome

Chosen option: "Task domains: route by task type or queue pattern, first match wins", because it isolates noisy task classes with plain configuration, keeps every task in exactly one database, and keeps all atomic operations local to that database.

How it works:

* A domain is a name, a list of GTS task-type or queue patterns, and a database DSN. A `default` domain catches everything else.
* Patterns are checked in config order. The first domain that matches wins. The same resolver is used for every operation: create, claim, search, retention, and cache.
* Operations that need atomicity stay inside one domain: a claim may name only queues of one domain, and a parent and its children, or a predecessor and its successor, must be in the same domain. Cross-domain requests are rejected.
* Read-only lists without a type filter, and DLQ inspection, fan out to all domains and merge the results.
* Each domain has its own migrations, connection pool, counters, retention loop, event outbox, and event inbox. Outgoing events commit with the task's state; an incoming event is applied once per domain via its inbox, and the consumer offset is committed last.

### Consequences

* Startup must check the routing rules: a queue whose task types map to different domains, or a change that would move a type or queue that still has live tasks in its old domain, stops startup with a clear error.
* Changing the domain of a live type or queue means draining its live tasks first. Rows are never moved between databases.
* Fan-out reads need a merged cursor across domains (DESIGN §4.6 Multi-Shard Search).
* Runtime configuration changes reach each domain through its own maintenance loop, so they apply per domain, not at one instant.
* A deployment that configures no domains behaves exactly like a single-database deployment.

### Confirmation

* Tests check that create, claim, search, and retention resolve the same domain for the same task.
* Tests check that cross-domain claims, dependencies, and parent/child links are rejected.
* Tests check that startup fails on conflicting routes and on routes that would strand live tasks.
* Tests check that an event matching waits in two domains is applied exactly once in each, also after a crash and redelivery.

## Pros and Cons of the Options

### One database for all tasks

* Good, because it is the simplest setup.
* Good, because every query and relationship works without limits.
* Bad, because heavy system tasks compete with user-visible tasks for I/O and index space.
* Bad, because retention and sizing cannot be tuned per task class.

### Per-tenant sharding

Split tasks across databases by tenant.

* Good, because it scales with the number of tenants.
* Bad, because a shared queue spans many tenants, so a claim would have to look in many databases.
* Bad, because it needs a shard map and rebalancing, which is a large new subsystem.
* Bad, because it does not separate system tasks from user tasks.

### Task domains: route by task type or queue pattern, first match wins

* Good, because isolation is plain configuration, with no new service.
* Good, because each task has one home and atomic operations stay local.
* Good, because a deployment can start with one domain and add more later.
* Bad, because cross-domain relationships are not allowed.
* Bad, because moving a live type or queue to another domain needs a drain.

## More Information

Per-tenant sharding stays in PRD §4.3 Future Scope. If it is added later, it needs a new ADR.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-task-engine-fr-task-domains` — domains, first-match routing, and the single-domain rule.
* `cpt-cf-task-engine-fr-assignment` — a claim names queues of one domain only.
* `cpt-cf-task-engine-fr-dependencies` — predecessor and successor live in the same domain.
* `cpt-cf-task-engine-fr-parent-child` — parent and children live in the same domain.
* `cpt-cf-task-engine-fr-dead-letter` — DLQ inspection fans out across domains.
* `cpt-cf-task-engine-nfr-capacity` — heavy task classes can be moved to their own database.
* `cpt-cf-task-engine-design-deployment` — the domain router and per-domain loops.
