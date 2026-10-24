---
status: accepted
date: 2026-10-24
description: "Use GTS for the open sets (task types, queues, entry types) and closed engine enums for control values (states, result codes, wait kinds), storing base fields in indexed columns and type-specific payloads in opaque compressed columns."
---

# Use GTS for Open Sets and Engine Enums for Control Values

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Everything as GTS entities (types, states, result codes)](#everything-as-gts-entities-types-states-result-codes)
  - [Everything as closed engine enums, with a table per task type](#everything-as-closed-engine-enums-with-a-table-per-task-type)
  - [Hybrid: GTS for open sets, closed enums for control values](#hybrid-gts-for-open-sets-closed-enums-for-control-values)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-task-engine-adr-gts-hybrid-model`

## Context and Problem Statement

Many gears will define their own task types, each with its own input and output shape. New task types must not need an engine release or a database migration. At the same time the engine's own control values, such as task states and result codes, drive the state machine and hot indexes and must never be changed by a third party.

Which values are extensible through GTS, which are fixed by the engine, and how is task data stored so that new types need no schema change?

## Decision Drivers

* New task types, queues, and entry types without engine releases or DDL.
* The state machine must stay exhaustive and safe: nobody outside the engine may add a state.
* Hot indexes (dispatch, lease scan) must stay small and fast.
* Type-specific payloads must be validated against their schema.
* Values must evolve safely across SDK versions and rolling upgrades.

## Considered Options

* Everything as GTS entities (types, states, result codes)
* Everything as closed engine enums, with a table per task type
* Hybrid: GTS for open sets, closed enums for control values

## Decision Outcome

Chosen option: "Hybrid: GTS for open sets, closed enums for control values", because it gives open extension exactly where other gears need it, and keeps the values that drive the state machine closed and compact.

How it works:

* **GTS entities (open)**: task types derive from `gts.cf.core.tasks.task.v1~`, queues are instances of `gts.cf.core.tasks.queue.v1~`, and entry types derive from `gts.cf.core.tasks.entry.v1~`. A task type declares input and output schemas and optional traits (concurrency limits, dedup and supersession, progress settings, type defaults).
* **Engine enums (closed)**: task states, result codes, urgency, wait-condition kinds, timer types, dependency modes, and concurrency dimensions are SDK enums with fixed numeric codes. They are stored as small integers and sent on the wire as snake_case names.
* **Not GTS**: tags are free-form, tenant-scoped strings.
* **Storage**: base fields (state, urgency, priority, queue, owner, assignment, timestamps) are indexed columns. The task type is stored as its Registry Reference from the Types Registry SDK, with the schema revision used at creation. Input, context, output, and other payloads are opaque compressed columns (ADR-0006), validated against the type's schema at the API boundary.
* **Enum evolution**: numeric codes are never reused or renumbered. Clients map unknown values to `Unknown`. Adding a value is a minor change.
* **Trust**: registered values are untrusted. The Types Registry registration policy decides who may register; queues are platform-owned only. Types are resolved with the caller's visibility, and every declared value must be inside engine `trust_limits` (PRD §5.12 Task Type and Queue Trust).

### Consequences

* A new task type needs only GTS registration. No migration and no engine release.
* The engine must resolve types from the GTS registry and cache them. If the registry is down and a type is not cached, creation fails closed.
* Payload search is not possible: the database cannot read inside opaque payloads. Search uses base fields, tags, and resource references only.
* Adding a state or result code is an engine change, follows the enum evolution rules, and may need an ADR.
* Type traits such as `dedup` must be validated when the engine first resolves the type, because consuming gears register types directly. Types Registry validation hooks, when available, move this check to registration.
* A removed or unavailable type refuses new tasks; existing tasks run to the end.

### Confirmation

* Tests check that a new task type registered at runtime can be created and claimed without a migration.
* Tests check that input that violates the registered schema is rejected.
* Tests check that an unknown enum value is surfaced as `Unknown` by the SDK, and that a stored unknown code is treated as non-actionable.

## Pros and Cons of the Options

### Everything as GTS entities (types, states, result codes)

* Good, because one model covers every value.
* Bad, because a third party could add a state the state machine does not handle.
* Bad, because GTS UUIDs in hot indexes are larger and slower than small integers.

### Everything as closed engine enums, with a table per task type

* Good, because the database knows every field and can index payloads.
* Bad, because every new task type needs an engine release and a migration.
* Bad, because it does not fit a platform where many gears add their own types.

### Hybrid: GTS for open sets, closed enums for control values

* Good, because extension is open where it is needed and closed where safety needs it.
* Good, because hot indexes stay compact.
* Good, because new task types need no migration.
* Bad, because payload fields cannot be searched or indexed.

## More Information

The enum evolution rules are listed in PRD §5.12 Task State and Result Code Domain Enums. The storage form of payload columns is in DESIGN §3.7 Portable JSON Storage Contract.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-task-engine-fr-gts-task-type` — task type as a GTS base type with traits.
* `cpt-cf-task-engine-fr-gts-queue` — queues as GTS instances.
* `cpt-cf-task-engine-fr-gts-entry-type` — entry types as GTS types.
* `cpt-cf-task-engine-fr-state-result-enums` — closed enums for control values.
* `cpt-cf-task-engine-fr-tags` — tags are not GTS entities.
* `cpt-cf-task-engine-fr-structured-io` — payloads validated against the type's schema.
* `cpt-cf-task-engine-design-gts-types` — the GTS entities the engine registers.
* `cpt-cf-task-engine-fr-type-trust` — registered values treated as untrusted.
