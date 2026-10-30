---
status: accepted
date: 2026-10-08
---

# Typed SDK and persisted journal boundary

**ID**: `cpt-cf-durable-execution-adr-typed-sdk-journal-boundary`

> **Superseded in part by [ADR 0005](0005-embeddable-library.md)**: the SDK and the gear are now one library crate, and ClientHub resolution is removed.
> `ExecutionClient` replaces `DurableExecutionClient`.
> `WorkflowRegistry` and `ExecutionInspector` are concrete handles from `Engine`; the object-safe `DurableExecution` trait and `WorkflowRegistryExt` are removed.
> Operations return the library `Error`; the embedding gear maps it to `CanonicalError` at its REST and gRPC boundaries.
> The typed-authoring and journal boundary stays accepted.


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Current SDK Boundary](#current-sdk-boundary)
  - [Superseded Decisions](#superseded-decisions)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Typed authoring with separate observation models](#typed-authoring-with-separate-observation-models)
  - [Public journal records with convenience builders](#public-journal-records-with-convenience-builders)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

The former SDK exposed journal records beside execution and registration models.
Callers constructed JSON definitions and interpreted separate status, error, and time fields.
Consumers must have typed authoring and separate observation without storage coupling.
They must also have checkpoint results and history under separate access rules.

## Decision Drivers

- Typed composition and declared checkpoint dependencies.
- Consistent progress and event cursor.
- Separate result and administrative access.
- Serializable contracts with local Rust bindings.
- Internal ownership of persisted records.

## Considered Options

1. Typed authoring with separate observation models.
2. Public journal records with convenience builders.

## Decision Outcome

Use typed authoring and separate observation models.
The SDK owns public types and capabilities.
The gear owns persisted journal records.
`DurableExecutionClient` supplies typed methods above object-safe `DurableExecution`.
`WorkflowRegistry` manages definitions.
`ExecutionInspector` supplies administrative progress.

### Current SDK Boundary

`WorkflowBuilder` produces declared stages, data sources, a serializable contract, and local bindings.
It supports sequential stages, static tuples/lists, and runtime fan-out.
Adjacent payload types must match.
Build checks IDs, policies, and dependencies before registration.
Supply timeouts to step constructors.
Contexts use named accessors.
Checkpoint types express different stage result shapes.

Typed start returns a handle with the expected contract identity.
Use that handle for typed result reads.
Supply a fingerprint and selector for object-safe result reads.
The service checks identity before payload hydration.
Checkpoint selectors must belong to the expected contract.
Contract mismatch returns `FailedPrecondition`.
Typed decode failure returns `Internal`.

Progress contains no input or result payload.
Results and history use separate reads.
Administrative access does not grant result access.
Events tell clients when to read progress again.
Stored projection and cursor describe the same committed transition.
Recorded timestamps and checkpoint origin epochs stay unchanged.

### Superseded Decisions

[ADR 0004](0004-saga-fanout-and-progress.md) supersedes the earlier legacy-compatibility assumptions.
The current format has no legacy journal reader or fingerprint fallback.
It also defines typed handles, finalized reference identity, runtime instance phases, and persisted progress queries.
This ADR's SDK-versus-storage boundary stays accepted.

### Consequences

The unreleased SDK and storage format change directly.
Applications use the public SDK instead of journal records.
They must version changed payload formats and changed handler behavior.
Rust functions and type names do not identify a persisted schema.
Implementation and test maintenance are necessary for adapters and typed references.
Fresh authorization, SecureORM, CAS, fencing, Outbox, and lifecycle control stay mandatory.

### Confirmation

- [Builder tests](../../tests/unit/workflow_tests.rs) check data sources and dependencies.
- [Authoring tests](../../tests/unit/authoring_contract_tests.rs) check finalized references and safe Debug output.
- [Compile-time fixtures](../../tests/ui/) reject incompatible chains, compensators, fan-out, handles, and checkpoints.
- [Service tests](../../tests/unit/service_tests.rs) check result access and identity before payload hydration.
- [Typed runtime tests](../../tests/integration/typed_runtime_tests.rs) check restart and late binding.
- [Projection tests](../../tests/unit/audit_storage_tests.rs) check query counts and payload isolation.

See the [Makefile](../../../../Makefile) and [CI workflow](../../../../.github/workflows/ci.yml) for check commands.
The current suite has no legacy compatibility requirement.

## Pros and Cons of the Options

### Typed authoring with separate observation models

Rust detects incompatible adjacent payload types.
Build detects incorrect dependencies.
Storage can change independently of public observation types.
Adapters and typed references are necessary for this design.

### Public journal records with convenience builders

With this option, existing callers have fewer changes.
Public records still expose storage details.
Observation changes affect persisted compatibility.

## More Information

Static tuples support two through eight branches.
Static lists support homogeneous branches known during definition construction.
Runtime fan-out supports item lists selected during a run.
Arbitrary DAGs, nested workflows, and remote handlers stay outside scope.

## Traceability

- [PRD](../PRD.md): typed results, authoring, inspection, and projection requirements.
- [DESIGN](../DESIGN.md): public and persisted models.
- [ADR 0004](0004-saga-fanout-and-progress.md): current contract and storage decisions.
- [Examples](../../examples/README.md): consumer scenarios.
- [Makefile](../../../../Makefile) and [CI workflow](../../../../.github/workflows/ci.yml): validation commands.
