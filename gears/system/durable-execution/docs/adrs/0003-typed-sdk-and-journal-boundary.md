---
status: accepted
date: 2026-10-08
---

# Typed SDK and persisted journal boundary

**ID**: `cpt-cf-durable-execution-adr-typed-sdk-journal-boundary`

## Table of Contents

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Typed authoring with separate observation models](#typed-authoring-with-separate-observation-models)
  - [Public journal DTOs with convenience builders](#public-journal-dtos-with-convenience-builders)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

The former SDK exposed journal records alongside registration, execution and UI
projections. Normal callers had to construct JSON definitions and interpret
independent status, error and timestamp fields. Consumers also need detailed
progress, epoch/attempt history and checkpoints for recovery and UI projections.
Simplifying submission must preserve those capabilities and existing records.

## Decision Drivers

- Typed sequential and parallel composition with explicit checkpoint dependencies.
- Small execution API with consistent progress/cursor and explicit result reads.
- Separate authorization for administrative observation and owned results.
- Stable persisted records, legacy fingerprints and recovery semantics.
- Local Rust bindings and a separately serializable contract.

## Considered Options

- Typed authoring with separate observation models and an internal persisted journal.
- Keep public journal DTOs and add convenience builders over them.

## Decision Outcome

Use typed authoring and separate public observation models. `WorkflowBuilder`
produces sequential stages, parallel tuples/lists, a serializable contract and
local bindings. `DurableExecutionClient` adds typed methods above object-safe
`DurableExecution`; `WorkflowRegistry` manages definitions and `ExecutionInspector`
provides administrative progress. Persisted records belong to the gear.

### Consequences

- The SDK change is breaking. Consumers migrate to explicit modules and a small
  prelude; old snapshot and duplicate cancellation methods are removed.
- Versioned input sources and dependencies enter new contract fingerprints.
  Legacy contracts without this metadata retain their original fingerprint.
  Rust functions and type names never identify a persisted schema.
- Neighboring step types are checked by Rust. Build validates IDs, policies and
  checkpoint references before registration. Applications version payload and
  handler semantics; serialization alone does not establish compatibility.
- `get` returns progress and cursor from one journal revision. Results and
  history are separate. Events notify clients to reread progress.
- State enums express errors, cancellation reasons and confirmed completion
  metadata. `Failing` preserves continuing parallel work; interrupted attempts
  are distinct from business cancellation. Legacy missing times stay unknown.
- Authoring adapters and projections add code and tests. They preserve existing
  journal transitions, authorization, CAS/fencing and shutdown budgets.

### Confirmation

Builder unit tests verify checkpoint serialization, dependency validation and
ordered tuple/list joins. Compile-fail tests reject incompatible inputs.
`typed_parallel_failure_progress_retry_and_cursor_are_consistent` verifies the
full runtime path and UI projection. `typed_checkpoint_survives_runtime_restart_and_late_binding`
verifies retained inputs across runtime restoration. Legacy journal/fingerprint,
authorization and forced subprocess recovery tests remain in the suite.

## Pros and Cons of the Options

### Typed authoring with separate observation models

- Detects adjacent payload mismatches at compile time and invalid dependencies at build.
- Keeps persisted formats independent from public state enums.
- Requires adapters, typed references and a consumer migration.

### Public journal DTOs with convenience builders

- Needs fewer changes for current callers.
- Retains overlapping methods and exposes storage details to ordinary consumers.
- Couples persisted compatibility to future observation/API changes.

## More Information

Arbitrary DAGs, nested workflows, remote handlers and REST bindings remain outside
scope. Tuple composition supports two through eight branches; dynamic lists cover
homogeneous branch sets. External effects remain at least once. Reconsider the
composition limits if a concrete consumer requires a graph that cannot be expressed
as sequential stages and parallel joins.

## Traceability

- [PRD](../PRD.md): progress, continuation, inspection and activity contracts.
- [DESIGN](../DESIGN.md): API and persistence boundaries.
- [Examples](../../durable-execution/examples/README.md): consumer scenarios.
- [Testing](../testing.md): unit, compile-fail, PostgreSQL and subprocess validation.
