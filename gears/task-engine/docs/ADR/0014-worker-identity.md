---
status: accepted
date: 2026-10-24
description: "Identify a worker by the pair of the caller's authenticated subject and a client-supplied worker_id, used for assignment, blacklisting, and audit but never as a credential."
---

# Identify Workers by Subject and Worker ID

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [The authenticated subject only](#the-authenticated-subject-only)
  - [A registered worker with an engine-issued ID](#a-registered-worker-with-an-engine-issued-id)
  - [The pair (authenticated subject, client-supplied `worker_id`)](#the-pair-authenticated-subject-client-supplied-worker_id)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-worker-identity`

## Context and Problem Statement

The engine needs to know which worker holds a task: for the `assignee` field, for the per-queue worker blacklist, for reassignment, and for audit. Workers usually run as many replicas of one service under one service principal. If the principal alone is the identity, one faulty replica gets the whole fleet blacklisted, and the audit cannot tell replicas apart.

What identifies a worker?

## Decision Drivers

* Tell apart worker processes that share one principal.
* One principal must not be able to act as, or blacklist, another principal's workers.
* Do not add a worker registry or extra service.
* Keep the identity stable across restarts where possible, so blacklisting stays meaningful.
* Identity must not weaken authorization or lease fencing.

## Considered Options

* The authenticated subject only
* A registered worker with an engine-issued ID
* The pair (authenticated subject, client-supplied `worker_id`)

## Decision Outcome

Chosen option: "The pair (authenticated subject, client-supplied `worker_id`)", because it separates replicas without a registry, and binding it to the authenticated subject keeps one principal from affecting another principal's workers.

How it works:

* Every claim carries a `worker_id`: 1–128 characters from `[A-Za-z0-9._:-]`, for example pod name plus process ID. A missing or invalid value is rejected with `400`.
* The worker identity is (`subject_id`, `worker_id`). The engine stores both on the assignment and uses the pair for the blacklist, events, and audit.
* Reassignment names the new holder as such a pair; only a caller authenticated as that subject and sending that `worker_id` can accept the task (ADR-0011).
* Workers should keep the same `worker_id` across restarts.
* `worker_id` is not a credential. Access is decided by the PDP, and worker writes are fenced by the lease token together with the authenticated half of the pair, `subject_id` (ADR-0011). `worker_id` alone never grants anything.
* Which subject a worker runs as (tenant subject or root-tenant service subject) and how it acts for a task is decided in ADR-0016.

### Consequences

* Worker SDKs must set a `worker_id` on every claim. The SDK can build a default from host and process data.
* A worker that changes its `worker_id` on every restart resets its blacklist history. This is accepted: the blacklist is a guard against repeated failures, not a security control.
* The assignment stores both parts (`assignee_subject` and `assignee`), and blacklist rows are keyed by both.
* No worker registration, discovery, or cleanup of dead workers is needed.

### Confirmation

* Tests check that a claim without a valid `worker_id` is rejected.
* Tests check that two processes under one principal with different `worker_id`s are blacklisted independently.
* Tests check that a `worker_id` used by another principal does not share its blacklist state.

## Pros and Cons of the Options

### The authenticated subject only

* Good, because the client sends nothing extra.
* Bad, because all replicas of a service look like one worker, so one faulty replica blacklists them all.
* Bad, because audit cannot tell which process did the work.

### A registered worker with an engine-issued ID

Workers register first, get an ID, and must renew the registration.

* Good, because the engine knows every live worker.
* Bad, because it adds a registry, registration API, liveness tracking, and cleanup.
* Bad, because leases already track liveness per task, so the registry duplicates it.

### The pair (authenticated subject, client-supplied `worker_id`)

* Good, because replicas are separated with one request field.
* Good, because the subject part prevents cross-principal impersonation.
* Good, because no new component is needed.
* Bad, because the engine trusts the worker to choose a sensible, stable ID.

## More Information

A worker registry may be added later if features such as worker capability routing need it. That would be a new ADR.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-task-engine-fr-assignment` — every claim carries a `worker_id`.
* `cpt-cf-task-engine-fr-worker-blacklist` — the blacklist is keyed by the worker identity.
* `cpt-cf-task-engine-fr-reassignment` — the new holder is named by the worker identity.
* `cpt-cf-task-engine-fr-event-history` — events and audit record the worker identity.
* `cpt-cf-task-engine-nfr-security` — the identity is not a credential.
