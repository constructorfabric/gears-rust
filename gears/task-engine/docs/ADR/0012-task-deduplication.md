---
status: accepted
date: 2026-10-24
description: "Deduplicate task creation with a required caller key scoped to tenant and subject, or with an engine-computed identity declared by the task type, plus optional supersession by a newer order value."
---

# Deduplicate Task Creation with Caller Keys or Type-Declared Identities

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Optional caller key with the stored response returned on replay (Stripe style)](#optional-caller-key-with-the-stored-response-returned-on-replay-stripe-style)
  - [Content hash over a fixed time window (SQS FIFO style)](#content-hash-over-a-fixed-time-window-sqs-fifo-style)
  - [Required caller key, or a type-declared engine identity, with optional supersession](#required-caller-key-or-a-type-declared-engine-identity-with-optional-supersession)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-task-deduplication`

## Context and Problem Statement

Tasks are created by APIs, events, retries, and integrations. A retried request must not create the task twice, because that means the work runs twice. Some callers need more: several independent producers submit the same work, and they must land on one task. For work whose input changes over time (for example a new graph revision), the running task becomes useless and should be replaced by the newer one.

How does the engine detect duplicate creation, and how does it replace outdated work, without letting one caller see or cancel another caller's tasks?

## Decision Drivers

* A caller retry must never create a second task.
* Independent producers of the same work must be able to share one task.
* One subject or tenant must never collide with, see, or cancel another one's task through deduplication.
* An older submission must never replace a newer one.
* Uniqueness must hold across all stored tasks while their identity is held, not only within a cache.
* The identity store must not grow with every stored task.

## Considered Options

* Optional caller key with the stored response returned on replay (Stripe style)
* Content hash over a fixed time window (SQS FIFO style)
* Required caller key, or a type-declared engine identity, with optional supersession

## Decision Outcome

Chosen option: "Required caller key, or a type-declared engine identity, with optional supersession", because it covers both "is this a retry?" and "is this the same work?", and it puts the authorization scope inside the identity so deduplication can never cross access boundaries.

How it works:

* **Caller key (default)**: every create request carries an `Idempotency-Key`. The key is unique per tenant and creating subject. A replay returns `409` with the existing task ID; the same key with a different request returns `400`; a missing key returns `428`.
* **Derived identity**: a task type may declare `dedup` with key fields from the input, a scope (`subject`, `authz_scope`, or `tenant`), and a window (`active` or `active_or_succeeded`). The engine computes the identity from the validated input, tenant, scope, and type. The caller key is then ignored.
* **Supersession**: a type with `dedup` may add `supersede` with an integer order field. A newer order value cancels the in-flight task of the group in the same transaction; an equal value is a normal duplicate; a lower value is rejected as stale.
* All identities are stored in one non-partitioned table whose primary key enforces uniqueness.

### Consequences

* Clients must always send a key for types without `dedup`. The SDK sends one automatically.
* A replay returns `409` and the current task, not the original `201` body. This differs from Stripe and the IETF draft on purpose; the SDK hides it.
* A caller key is held while its task is unfinished and for at least `idempotency_key_window_days` after creation (default 7, at most the task's retention). After that, a replayed key of a finished task creates a new task. Holding keys for the whole retention would keep one identity row per stored task (500 million at the capacity profile); the window keeps about 35 million.
* `authz_scope` works only once ToolKit exposes a stable serialized form of the compiled access scope. Until then it is rejected.
* Supersession needs the `cancel` permission on the replaced task, and supersede-enabled types cannot accept non-cancellable tasks.
* The `dedup` and `supersede` traits must be validated against the input schema when the engine first resolves the type. An invalid policy makes the type non-creatable.

### Confirmation

* Tests check replay, payload mismatch, missing key, and subject isolation for caller keys.
* Tests check that a replay is compared with the original request (stored as a fingerprint), not with the task's current state: a retry sent after the task checkpointed, was moved, was dead-lettered, or finished still returns `409` with the task ID.
* Tests check that different scopes never share a derived identity.
* Tests check that newer, equal, and older order values give supersede, replay, and stale rejection, including after the newer task finished.

## Pros and Cons of the Options

### Optional caller key with the stored response returned on replay (Stripe style)

* Good, because clients that skip the key still work, and replays look like normal success.
* Bad, because callers that forget the key create duplicate work, which is the usual failure.
* Bad, because a task changes after creation, so the stored `201` body becomes stale.
* Bad, because it cannot merge equal work from different producers.

### Content hash over a fixed time window (SQS FIFO style)

* Good, because callers need no key.
* Bad, because a fixed window (for example 5 minutes) misses slow retries.
* Bad, because the hash has no authorization scope, so different callers could collide.
* Bad, because it cannot express "newer replaces older".

### Required caller key, or a type-declared engine identity, with optional supersession

* Good, because both retry safety and shared work are covered.
* Good, because the scope is part of the identity, so deduplication never crosses access boundaries.
* Good, because one ordering rule gives race-free replacement of outdated work.
* Bad, because the replay contract (`409`) differs from common practice.
* Bad, because the derived identity depends on correct type registration.

## More Information

Temporal offers business workflow IDs with a conflict policy, but its `TERMINATE_EXISTING` policy has no ordering, so a slow older submission can replace a newer run. The order value closes that gap.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-task-engine-fr-idempotent-create` — required caller key scoped to tenant and subject.
* `cpt-cf-task-engine-fr-derived-dedup` — type-declared identity with scope and window.
* `cpt-cf-task-engine-fr-task-supersede` — newer order wins, equal is a replay, older is stale.
* `cpt-cf-task-engine-fr-task-cancel` — supersession uses the normal cancel path.
* `cpt-cf-task-engine-fr-retention` — deduplication is bounded by retention.
* `cpt-cf-task-engine-nfr-security` — identities never cross tenants or access scopes.
