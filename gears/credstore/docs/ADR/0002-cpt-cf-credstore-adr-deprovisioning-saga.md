---
status: superseded
date: 2026-07-04
---

Created:  2026-07-07 by Virtuozzo International GmbH
Updated:  2026-10-03 by Constructor Tech

# ADR-0002: Status-Driven Deprovisioning Saga with Name Retention

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)

<!-- /toc -->

**ID**: `cpt-cf-credstore-adr-deprovisioning-saga`

**Superseded by [ADR-0006](0006-cpt-cf-credstore-adr-immutable-value-versions.md).**

## Context and Problem Statement

With a stateful gear ([ADR-0001](0001-cpt-cf-credstore-adr-stateful-gear.md)) a delete spans two stores, the metadata row and the backend value, and a failure between the two steps left no owner for the leftover.

## Decision Drivers

- **D1** — a delete that fails between the row and the backend value must not leave a value with no owner.
- **D2** — a re-created name must not be clobbered by a lagging backend delete of its predecessor.

## Considered Options

- **Backend-first delete** — delete the value, then the row.
- **Status-driven saga with name retention** — chosen at the time.

## Decision Outcome

Delete was a saga: the row moved to a `deprovisioning` status that held the reference until the backend value was removed, resumed by a `DELETE` retry or a periodic reaper.

### Consequences

The saga and its name retention existed because a successor's value shared the deleted value's backend key. With immutable value versions that race cannot occur: deleting a record is one row transaction that records a purge of the record's key, executed by the request; a failed purge stays recorded until a possible external cleanup job. There is no `deprovisioning` status, no reaper and no name retention. See ADR-0006.

### Confirmation

Superseded: nothing of this decision ships any more; ADR-0006 carries the confirmation of what replaced it.

## Pros and Cons of the Options

- **Backend-first delete** — Good: simple. Bad: a failure between the steps leaves a value or a row with no owner (D1).
- **Saga with name retention** — Good: a retry or the reaper finishes the delete. Bad: a `deprovisioning` row holds the name hostage, and the reaper is a background process.
