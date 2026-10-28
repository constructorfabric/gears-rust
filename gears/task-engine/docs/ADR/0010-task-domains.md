---
status: accepted
date: 2026-10-25
description: "Route each queue to one of several configured databases (task domains) with a fixed first-match rule; a task lives in its queue's domain; optional type rules are checked at create time; dead-letter queues are not routed; every atomic operation stays inside one domain."
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
  - [Route by task type or queue pattern, first match wins](#route-by-task-type-or-queue-pattern-first-match-wins)
  - [Route by queue, with type rules checked at create time](#route-by-queue-with-type-rules-checked-at-create-time)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-task-domains`

## Context and Problem Statement

All task data lives in a relational database (ADR-0001). Some task classes behave very differently: high-volume system tasks (cleanup, sync) create a lot of rows and churn, while user-visible tasks need fast search and longer retention. When both share one database, the system tasks slow down the user-facing queries and make retention harder to tune.

How can a deployment give different task classes their own storage, without adding a second source of truth or losing atomic claims and dependencies?

Two facts shape the answer. Queues are platform-owned and known from config, but task types are registered by other gears **at runtime**, so a rule that depends on the set of types cannot be checked at startup. And claims, counters, and queue-depth limits are per queue, so one queue must never have tasks in two databases.

## Decision Drivers

* Keep one source of truth for each task (ADR-0001).
* Isolate the load, size, and retention of different task classes.
* Keep claims, dependencies, and parent/child links atomic.
* Make routing predictable: a task must always map to the same database.
* Every routing rule must be checkable when it matters, including for types registered after startup.
* Work without new infrastructure: only more database connections.

## Considered Options

* One database for all tasks
* Per-tenant sharding
* Route by task type or queue pattern, first match wins
* Route by queue, with type rules checked at create time

## Decision Outcome

Chosen option: "Route by queue, with type rules checked at create time", because a queue's domain is fully known from config, so a queue can never split across databases, and the only rule that depends on runtime types is checked where the type is known: at create.

How it works:

* A domain is a name, a list of **queue** patterns, optional **type** patterns, and a database DSN. A `default` domain catches every other queue.
* **Routing by queue**: queue patterns are checked in config order; the first matching domain wins. A task lives in its queue's domain. The same resolver is used for every operation: create, claim, search, retention, and cache.
* **Type rules**: a type that matches a domain's type patterns (first matching domain wins) may only be placed in queues of that domain. This is checked on every create, bulk move, and re-enqueue; a violation is rejected `400` `TASK_DOMAIN_MISMATCH`.
* **Dead-letter queues are not routed**: dead-lettering only sets the task's queue to the DLQ; the row stays in its source queue's database. One DLQ can serve queues in several domains.
* Operations that need atomicity stay inside one domain: a claim may name only queues of one domain, and a parent and its children, or a predecessor and its successor, must be in the same domain. Cross-domain requests are rejected.
* Reads filtered by queue, or by a type with a type rule, use one domain. Other reads, and DLQ inspection, fan out to all domains and merge the results.
* Each domain has its own migrations, connection pool, counters, retention loop, event outbox, and event inbox. Outgoing events commit with the task's state; an incoming event is applied once per domain via its inbox, and the consumer offset is committed last.

### Consequences

* Startup checks only queue routing: a change that would move a queue that still has live tasks in its old domain stops startup with a clear error.
* Changing the domain of a live queue means draining its live tasks first. Rows are never moved between databases. Type rules can change at any time; they affect only new tasks.
* A re-enqueue into a queue that now routes to another domain is rejected.
* The shipped queues work with any routing: `default` and `system` can live in different domains and still share the `dead_letter` queue.
* Fan-out reads need a merged cursor across domains (DESIGN §4.6 Multi-Shard Search).
* Runtime configuration changes reach each domain through its own maintenance loop, so they apply per domain, not at one instant.
* A deployment that configures no domains behaves exactly like a single-database deployment.

### Confirmation

* Tests check that create, claim, search, and retention resolve the same domain for the same task.
* Tests check that cross-domain claims, dependencies, and parent/child links are rejected.
* Tests check that a type with a type rule is rejected in a queue of another domain, also when the type is registered after startup.
* Tests check that queues in two domains share one dead-letter queue, that their dead-lettered tasks stay in their own databases, and that DLQ inspection lists both.
* Tests check that startup fails on a queue routing change that would strand live tasks.
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

### Route by task type or queue pattern, first match wins

The earlier design: one pattern list that mixes types and queues.

* Good, because a deployment can route by either key.
* Bad, because tasks of one queue can land in different databases when their types match different patterns, which breaks per-queue claims and counters.
* Bad, because the rule that prevents this ("a queue's types map to one domain") needs the set of types, which is only known at runtime, so it cannot be checked at startup.
* Bad, because a queue and its DLQ must map to the same domain, which breaks the shipped `default` and `system` queues sharing one `dead_letter` queue.

### Route by queue, with type rules checked at create time

* Good, because a queue's domain is decided by config alone, so a queue never splits.
* Good, because the type check runs where the type is known, so runtime-registered types are covered.
* Good, because each task has one home and atomic operations stay local.
* Good, because a deployment can start with one domain and add more later.
* Bad, because cross-domain relationships are not allowed.
* Bad, because moving a live queue to another domain needs a drain.

## More Information

Per-tenant sharding stays in PRD §4.3 Future Scope. If it is added later, it needs a new ADR. See DESIGN §3.8 Deterministic routing.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-task-engine-fr-task-domains` — domains, routing by queue, type rules, and the single-domain rule.
* `cpt-cf-task-engine-fr-assignment` — a claim names queues of one domain only.
* `cpt-cf-task-engine-fr-dependencies` — predecessor and successor live in the same domain.
* `cpt-cf-task-engine-fr-parent-child` — parent and children live in the same domain.
* `cpt-cf-task-engine-fr-dead-letter` — dead-letter queues are not routed; DLQ inspection fans out across domains.
* `cpt-cf-task-engine-nfr-capacity` — heavy task classes can be moved to their own database.
* `cpt-cf-task-engine-design-deployment` — the domain router and per-domain loops.
