---
status: accepted
date: 2026-09-10
decision-makers: BSS Orders team (Architecture)
---

# ADR-0001: Orchestrate Over Gear-Owned Durable State, Not Engine History


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [An external durable-execution engine as the substrate, with its history as the record](#an-external-durable-execution-engine-as-the-substrate-with-its-history-as-the-record)
  - [Orchestration over gear-owned durable state (chosen)](#orchestration-over-gear-owned-durable-state-chosen)
  - [A hybrid with dual-written engine and gear-owned history](#a-hybrid-with-dual-written-engine-and-gear-owned-history)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-orders-workflow-adr-durable-execution-substrate`

> **Amended 2026-09-24 by ADR-0011** (`cpt-cf-bss-orders-workflow-adr-flow-as-platform-definition`):
> this ADR is rewritten. The decision that gear-owned durable state is the audit and recovery
> record is unchanged. The substrate is now **selected** — the platform gear serverless-runtime's
> Temporal plugin executing a versioned platform workflow definition — so the earlier statements
> that this ADR "selects no engine product" and that Q-01 is open are withdrawn. Engine history is
> non-authoritative and, by ADR-0013 (`cpt-cf-bss-orders-workflow-adr-references-not-payloads`),
> reference-only. The fallback property is kept.

## Context and Problem Statement

Orders Workflow is a long-running process orchestrator: approval waits, provisioning intents,
retries and long timers must survive restarts with zero loss for committed steps. PRD §13 (line
1168) defers the durable-execution platform choice to an ADR and PRD §15 (line 1183) states the
criteria that choice must be evaluated on; PRD §6.1 (`cpt-cf-bss-orders-workflow-fr-owf-process-state-nonauth`,
lines 241–243) fixes a constraint ahead of that choice: this gear's process audit and saga log —
independent of whatever execution substrate is used — are the audit system of record (SoR) for
process-execution progress, never engine execution history.

The substrate is now known. The platform executes Serverless Workflow definitions on Temporal
through the serverless-runtime Temporal plugin
([serverless-runtime ADR-0004](../../../../serverless-runtime/docs/ADR/0004-cpt-cf-serverless-runtime-adr-temporal-workflow-engine.md)
line 90), the plugin owns the full invocation record, timeline and internal execution state
([serverless-runtime DESIGN.md §1.4.2](../../../../serverless-runtime/docs/DESIGN.md#142-plugin-model)
line 174), and Temporal Server keeps that history in a persistence backend the platform operates
(ADR-0004 line 100). ADR-0011 decides that the order process flow is such a definition and that
this gear provides step operations and the process record. Which record is the audit SoR and the
recovery record, and what is the engine's history allowed to be?

## Decision Drivers

* Gear-owned process audit and saga log must be the audit SoR (§6.1), independently of engine history, which is explicitly not the audit SoR.
* Zero data loss for committed steps (§7.1) must hold regardless of engine purge, retention or residency policy; Temporal history is retained on the platform's clock (DESIGN.md line 739, `TenantRuntimePolicy` retention), not this gear's.
* Commercial data — the resolved total in approval context, approver identities, tenant axes, the saga log — must not sit in a history this gear cannot isolate, retain or erase (§15).
* The BSS/OSS boundary keeps a BSS process gear from depending on a store it does not own for its compliance-grade record.
* A process instance must execute to completion under the definition version it started with (§6.1, §5.2); the record must carry that binding on this gear's side even though the platform also pins invocations (DESIGN.md line 614).
* The sibling gear `orders-lifecycle` made its `p1` guarantee structural rather than a discipline (its ADR-0001); the same shape applies here to process-execution durability.
* If the platform slips, the record and the operations must not change; only what sequences them may.

## Considered Options

* An external durable-execution engine as the substrate, with its execution history treated as the operational and audit record
* Orchestration over gear-owned durable state: this gear persists step progress, the saga/compensation log, the idempotency registry, the audit chain, approval-request tracking, the `correlationId` and the definition binding itself, in its own transactions, and treats the engine's history as a mechanical sequencing aid whose content is reference-only and whose retention is irrelevant to the record
* A hybrid where the engine's history is dual-written into a gear-owned audit table

## Decision Outcome

Chosen option: **orchestration over gear-owned durable state**, because it is the only option
under which "engine history is not the audit SoR" (§6.1) is a structural property rather than an
operational promise — and it remains the choice now that the engine is the serverless-runtime
Temporal plugin. Concretely:

* **The gear owns the record.** Step progress (`owf_step_log`, with the platform `attempt_id` on
  every row), the process instance and its recorded `phase`, the saga/compensation log, the
  idempotency registry, the hash-chained audit trail (D-59…D-61), approval-request tracking, the
  process `correlationId` and the definition binding (`owf_definition_binding`, ADR-0012) are
  written by this gear's step operations in this gear's transactions, with the producer-outbox
  enqueue of ADR-0008 in the same transaction where an event is declared.
* **The substrate is the platform.** Sequencing, waits, timers, retry scheduling, event listening
  and signals are executed by the serverless-runtime Temporal plugin from a versioned platform
  definition (ADR-0011). This gear owns no timer table, no retry-state table and no scheduling
  worker for the process path.
* **Engine history is non-authoritative and reference-only.** Task inputs and outputs carry
  identifiers and small enums only (ADR-0013). Nothing downstream — audit exports, replay,
  operator progress, the reconciliation sweep, manual-task views — reads the platform timeline or
  any Temporal history as a source; the record is complete without it by construction.
* **Recovery is defined against the record.** A crashed or restarted operation reconstructs its
  position from this gear's record alone; the platform's re-invocation of a task is absorbed by
  the idempotency registry (ADR-0006 as amended), and an instance whose invocation ends without
  `terminate-instance` is found by the sweep from the record, not from the engine.

### Consequences

* This gear implements and operates its own durable-state model — the record above — and does not delegate audit, recovery or idempotency to the platform's guarantees; it does not implement timers, retry curves or scheduling for the process path, which are the platform's (ADR-0011 responsibility split).
* Retention, isolation and erasure of process artifacts are this gear's, applied in one store; the residual identifiers that sit in engine history are bounded by ADR-0013 and covered by the residency and retention asks it names.
* **Fallback property.** If the platform readiness gate does not pass or the Q-01 evaluation fails on the platform asks, the record and the step operations are unchanged; only sequencing falls back to a code sequencer in this gear calling the same operations, binding the instance with `definition_source = code`. Because nothing in the record depends on which sequencer called an operation, the audit SoR never moves.
* The definition-version binding is recorded on this gear's side (`owf_definition_binding`) even though the platform pins invocations, so the pinning requirement of §6.1 is enforceable against a record this gear controls.
* The engine's history may be purged, relocated or reset without an audit migration, because the audit SoR never lived there.

### Confirmation

Confirmed by a structural test asserting no code path in this gear reads the platform timeline,
debug or trace endpoints or any engine-side history as a source for audit, replay, operator
progress or the reconciliation sweep; a restart test asserting an in-flight process reconstructs
step progress and its next expected operation solely from the gear-owned record after a forced
restart, with zero loss for already-committed steps; a test that a completed instance's record and
audit chain verify with the engine's history absent; and the same operation-level tests passing
under the code sequencer with `definition_source = code`.

## Pros and Cons of the Options

### An external durable-execution engine as the substrate, with its history as the record

The engine — here the serverless-runtime Temporal plugin — provides durability, retries and timers, and its execution history is inspected as the record.

* Good, because durability, retry and timer mechanics are provided rather than built.
* Good, because it matches a widely understood workflow-engine model and the platform's own timeline endpoints.
* Bad, because engine history is retained on the platform's clock and stored where the platform's persistence backend is, which conflicts directly with §6.1's independence requirement and §15's isolation and retention criteria.
* Bad, because it would put commercial data into that history unless ADR-0013's reference rule were adopted anyway — at which point the history cannot be the record, since it holds only references.

### Orchestration over gear-owned durable state (chosen)

The gear persists its own process-execution record; the platform plugin sequences the operations that write it.

* Good, because the audit SoR is gear-owned by construction, satisfying §6.1 without relying on operational discipline.
* Good, because retention, isolation and erasure of process artifacts are set by this gear's own policy in one store.
* Good, because the definition-version pinning requirement is enforceable against a record this gear controls.
* Good, because the fallback property holds: the sequencer can change without the record moving.
* Neutral, because timers, retries and waits are now the platform's rather than built here; the gear builds the record and the operations, not the engine.
* Bad, because two records exist — the gear's and the engine's — and the rule that only one is citable must be held by tests, not by the absence of the other.

### A hybrid with dual-written engine and gear-owned history

The engine's execution history is copied into a gear-owned audit table after the fact.

* Good, because it lets teams start from an engine's built-in durability while working toward gear ownership.
* Bad, because two writers of "process history" create a reconciliation problem exactly analogous to the dual-SoR drift §6.1 exists to prevent, only one level down.
* Bad, because a lag or failure in the copy step reintroduces the zero-loss risk this ADR exists to close, and because the engine's history is reference-only (ADR-0013) there is nothing in it worth copying.

## More Information

**Revision history.** First accepted 2026-09-10 with the substrate left open as `DECISIONS.md`
Q-01. Rewritten 2026-09-24 by ADR-0011: the substrate is the serverless-runtime Temporal plugin
executing a platform workflow definition; Q-01 is answered in two parts there (the choice is
made; the PRD §15 evaluation of engine-history isolation, retention and residency is pending on
the platform asks in `UPSTREAM_REQS.md`, serverless-runtime section). The statement in the first
revision that a document citing this ADR as selecting an engine misreads it no longer applies:
this ADR, together with ADR-0011, selects it. See the sibling `orders-lifecycle` gear's ADR-0001
(`transition-through-engine`) for the analogous pattern of making a `p1` guarantee structural.

## Traceability

- **PRD**: [PRD.md](../PRD.md) — §5.2, §6.1, §7.1, §13, §15 (line 1183), §16
- **DESIGN**: [DESIGN.md](../DESIGN.md) §1, §2.2, §4.2, §4.3, §4.9;
  [`design/01-foundation.md`](../design/01-foundation.md);
  [`design/10-process-definition.md`](../design/10-process-definition.md)
- **Decisions register**: [`DECISIONS.md`](../DECISIONS.md) — D-01, Q-01 (answered by ADR-0011)
- **Related ADRs**: ADR-0011 (flow as platform definition), ADR-0012 (versioning and protected
  steps), ADR-0013 (references, not payloads), ADR-0003 (process state non-authoritative)
- **Platform**: serverless-runtime
  [ADR-0004](../../../../serverless-runtime/docs/ADR/0004-cpt-cf-serverless-runtime-adr-temporal-workflow-engine.md),
  [DESIGN.md](../../../../serverless-runtime/docs/DESIGN.md) §1.4.2, §3.1

This decision directly addresses the following requirements or design elements:

* `cpt-cf-bss-orders-workflow-fr-owf-process-state-nonauth` — this decision is what makes gear-owned process audit the SoR independently of engine history, now with the engine named
* `cpt-cf-bss-orders-workflow-nfr-owf-durability` — zero-loss durability for committed process state is asserted against the gear-owned record this decision establishes
* `cpt-cf-bss-orders-workflow-nfr-owf-audit` — 100% process-state-transition audit coverage is achievable only if the audit path is gear-owned end to end
* `cpt-cf-bss-orders-workflow-nfr-owf-retention` — retention of process artifacts is set by this gear's own policy rather than an inherited engine window; the residual engine-side identifiers are covered by ADR-0013's asks
