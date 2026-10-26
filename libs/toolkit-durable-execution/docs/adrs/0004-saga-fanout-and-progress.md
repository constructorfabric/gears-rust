---
status: accepted
date: 2026-10-12
---

# Typed run identity, durable fan-out, saga compensation, and progress projection

**ID**: `cpt-cf-durable-execution-adr-saga-fanout-progress`


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
  - [Extend the old format with compatibility readers](#extend-the-old-format-with-compatibility-readers)
  - [Use ordinary handler loops and in-memory compensation](#use-ordinary-handler-loops-and-in-memory-compensation)
  - [Explicit stages, typed identity, and persisted compensation](#explicit-stages-typed-identity-and-persisted-compensation)
- [Decision Outcome](#decision-outcome)
  - [Typed Identity](#typed-identity)
  - [Stage and Instance Model](#stage-and-instance-model)
  - [Compensation](#compensation)
  - [Progress and Events](#progress-and-events)
- [Consequences](#consequences)
- [Confirmation](#confirmation)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

The unreleased runner must have typed result identity, variable work lists, and recoverable business compensation.
Fixed activity IDs cannot identify every fan-out branch or its compensation.
Full journal hydration for progress causes extra queries and unnecessary payload reads.
The earlier format also has fixed event blocks that cannot hold a large expansion.
Compatibility is not necessary for existing consumers.

## Decision Drivers

- Reject foreign contracts before payload access.
- Preserve expansion, inputs, and branch order across restart.
- Enforce stage concurrency across workers.
- Recover compensation independently of forward execution.
- Read progress without payload hydration.
- Preserve ToolKit security and transaction boundaries.

## Considered Options

### Extend the old format with compatibility readers

This option keeps old stored records.
Dual fingerprint rules, old readers, and migration tests are necessary for this option.
No consumers justify this cost.

### Use ordinary handler loops and in-memory compensation

This option keeps the runtime smaller.
A worker crash loses branch progress or compensation order.
Retry can repeat completed branch effects.
It does not meet durable recovery requirements.

### Explicit stages, typed identity, and persisted compensation

This option records each instance and phase.
It supports ordered joins, separate retries, and crash recovery.
More state, constraints, and tests are necessary for this option.
This is the selected option.

## Decision Outcome

Change the unreleased SDK and storage format directly.
Reject unsupported stored versions.
Remove legacy readers and fallback fingerprints.
Use clean development and test databases.
Do not delete existing databases automatically.

### Typed Identity

Typed start returns `StartedRun<I, O>` with a `RunHandle<I, O>`.
The handle records expected contract identity.
`WorkflowRef::attach` restores it from a saved ID.
For result reads, supply the expected fingerprint and a workflow or checkpoint selector.
Authorization and identity checks occur before payload hydration.
Checkpoint selection must belong to the expected contract.
Compatible JSON does not override a fingerprint mismatch.

Pre-build references share a private, write-once descriptor.
Successful build sets the final identity.
Sequential, static parallel, and fan-out checkpoints have separate types.
Mandatory timeouts and named context accessors make execution control visible.

### Stage and Instance Model

The contract records declared stages, data sources, forward policies, compensation policies, and fan-out limits.
These fields enter the fingerprint.
Run-specific item counts do not enter it.
An instance identity contains template ID, optional item index, and forward or compensation phase.
Storage, queue delivery, fencing, and provider keys use this identity.

Fan-out expansion commits exact item inputs, stable indexes, and all publication intents atomically.
Recovery keeps the expansion.
Join keeps input order.
An empty list completes without handler calls.
Each branch has separate retries and ownership.
Persisted claims enforce stage concurrency through journal CAS across hosts.
Each host's queue pool enforces its own concurrency too.
Defaults are 1024 items and stage concurrency 16.
A run can have at most 4096 forward instances.

### Compensation

Final forward failure or Abort stops new forward claims.
The runtime signals active handlers and waits for cleanup or lease expiry.
It compensates only confirmed successful instances with declared compensators.
It uses reverse stage and reverse branch-index order, one instance at a time.
The journal keeps original invocation inputs and successful outputs.
Compensation keys stay stable across retries and restart and differ from forward keys.

Compensation has separate timeout and retry policies.
Exhaustion stops compensation at `CompensationFailed`.
Manual Retry continues the failed and remaining compensators.
It keeps successful compensation checkpoints.
The original failure or Abort reason stays separate from compensation errors.
`Compensated` prohibits forward Retry and Resume.
Cancel and Resume stay forward pause operations.
Cancel during compensation is rejected.
Shutdown keeps the phase.

Saga `CancelAndRelease` starts Abort and keeps bindings until compensation completes.
Failed compensation keeps the definition `Stopping`.
Restart accepts restored saga bindings with the same contract for cleanup in the same generation.
Activation during `Stopping` stays prohibited.

### Progress and Events

The run row contains a private, versioned projection without payloads.
The transaction commits it with journal changes, events, and publication intents.
Get and inspect each select only projection in one query.
List performs one count query and one paginated projection query.
Invocation inputs, outputs, and history stay outside the projection and control metadata.
Event sequence numbers increase per run without revision-sized blocks.
The projection keeps exact recorded times, origin epochs, compensation states, and committed cursor.

## Consequences

Additional storage and recovery tests are necessary for the new model.
Clean development databases replace compatibility testing for this unreleased format.
A separate decision is necessary for future production migration.

External effects can repeat.
Compensation does not provide a distributed transaction.
Missing compensators and unconfirmed results stay outside compensation.
Providers must deduplicate or reconcile effects before a lost checkpoint.
Credentials stay in local provider clients.

Fresh authorization, SecureORM, CAS, fences, transactional Outbox, and managed lifecycle stay mandatory.
Compensation uses original owner metadata, but that metadata is not an authorization grant.

## Confirmation

- [Authoring tests](../../tests/unit/authoring_contract_tests.rs): finalized identity and safe formatting.
- [Compile-time fixtures](../../tests/ui/): correct and incorrect typed authoring.
- [Service tests](../../tests/unit/service_tests.rs): fingerprint guard before corrupt payload hydration.
- [Stage state tests](../../tests/unit/stage_state_tests.rs): limits, local retries, reverse order, and saved compensation.
- [Stage runtime tests](../../tests/unit/staged_runtime_tests.rs): competing claims, expansion, compensation Retry, and large event batches.
- [Storage query tests](../../tests/unit/audit_storage_tests.rs): constant query counts and corrupt payload/history isolation.
- [PostgreSQL runtime tests](../../tests/integration/typed_runtime_tests.rs): real queue fan-out and restart.
- [Provider crash tests](../../tests/integration/provider_stages_tests.rs): separate provider state across worker crashes.
- [Definition tests](../../tests/integration/registry_tests.rs): ordinary registration and release.

The listed tests do not directly prove release after failed saga compensation and restoration of its draining bindings.
That acceptance check stays open.

Use the [Makefile](../../../../Makefile) targets run by the [CI workflow](../../../../.github/workflows/ci.yml).
The [PRD acceptance criteria](../PRD.md#9-acceptance-criteria) keep the unproved runtime checks open.
Do not treat an unavailable runtime check as passed.

## Traceability

This ADR supersedes the legacy-compatibility assumptions in [ADR 0003](0003-typed-sdk-and-journal-boundary.md).
The SDK-versus-storage boundary in ADR 0003 stays accepted.
[ADR 0001](0001-apalis-with-transactional-outbox.md) still defines atomic publication.
[ADR 0002](0002-task-execution-framework-selection.md) still defines transport selection.

- [PRD](../PRD.md): `fr-typed-results`, `fr-fanout`, `fr-compensation`, and `fr-projection` under the `cpt-cf-durable-execution-` prefix.
- [DESIGN](../DESIGN.md): stage model, saga states, publication sequences, and storage.
- [Execution state](../../src/state/execution.rs): runtime transitions.
- [Projection storage](../../src/store/progress.rs): progress queries.
- [SDK handles](../../src/handles.rs): typed run identity.
