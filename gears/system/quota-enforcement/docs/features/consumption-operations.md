<!-- Created: 2026-08-26 by Constructor Tech -->

# Feature: Consumption Operations & Idempotency

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-featstatus-consumption-operations-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-quota-enforcement-feature-consumption-operations`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Debit](#debit)
  - [Credit](#credit)
  - [Return](#return)
  - [Rollback](#rollback)
  - [Evaluate Preview](#evaluate-preview)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Evaluation Orchestrator Pipeline](#evaluation-orchestrator-pipeline)
  - [Unwinding](#unwinding)
  - [Idempotency Lookup and Replay](#idempotency-lookup-and-replay)
  - [Period Rollover and Settlement](#period-rollover-and-settlement)
  - [Retention Sweep](#retention-sweep)
- [4. States (CDSL)](#4-states-cdsl)
  - [Consumption Period Counter State Machine](#consumption-period-counter-state-machine)
  - [Idempotency Record State Machine](#idempotency-record-state-machine)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Consumption Operation Endpoints](#consumption-operation-endpoints)
  - [Evaluation Orchestrator](#evaluation-orchestrator)
  - [Idempotency Guarantee](#idempotency-guarantee)
  - [Counter Shapes and Period Semantics](#counter-shapes-and-period-semantics)
  - [Charge Ledger and Return](#charge-ledger-and-return)
  - [Retention Sweeper](#retention-sweeper)
  - [Hot-Path NFR Verification](#hot-path-nfr-verification)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Additional Context (optional)](#7-additional-context-optional)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Establishes the `EvaluationOrchestrator` and its canonical pipeline, and implements the single-shot counter
operations: debit, credit, return, rollback, and read-only preview. Covers the allocation and consumption counter
shapes with lazy period-row materialization, calendar-aligned UTC periods with deterministic rollover and settlement
semantics, the **charge ledger** (one record per allowed debit or lease commit, so part of it can be returned later),
and the typed idempotency scope `(tenant_id, operation_type, idem_key)` that makes every write replay-safe. The
**replay window** is the time QE keeps idempotency records, 24 hours by default. The **return window** of a charge is
the replay window, counted from the charge's creation.

### 1.2 Purpose

This is the hot path: every guarded operation in a consuming service turns into a debit, and every retry of that debit
must land exactly once. This feature joins the Quota records of the quota-lifecycle feature with the Decision contract
of the resolution-policy-engine feature into one atomic transaction, so a caller never observes a partially applied
Debit Plan, a double-counted retry, or a counter that changed on a denial. It also owns the period machinery that
resets consumption counters at calendar boundaries, and the retention sweep that keeps idempotency and operation-log
storage bounded. Behavior is fail-closed: when the pipeline cannot produce a Decision, it surfaces a canonical error
and mutates nothing.

**Scope**: S2S `QuotaEnforcementService` entry points for debit, credit, return, rollback, and preview;
the canonical orchestrator pipeline (resolution, idempotency lookup, locked read, Policy lookup, Engine, invariant
check, mutation, charge write, idempotency persist, outbox enqueue, commit); allocation and consumption counter shapes
with lazy period-row materialization and the threshold-marker reset (I13); debit applying Engine Debit Plans
atomically; credit against a named Quota; the charge ledger and unwinding; return of part of a charge and rollback as
the return of its remainder; preview with no persisted state; the `INVALID_AMOUNT`
fail-fast ordering and `IDEMPOTENCY_PAYLOAD_MISMATCH`; period rollover event ordering and the calendar-keyed credit
closure; idempotency, operation-log, and charge retention via `RetentionSweeper`.

**Out of scope**: two-phase holds (the lease-operations feature), multi-item envelopes (the batch-debit feature),
subject resolution itself (defined by the projection-contracts feature as
`cpt-cf-quota-enforcement-algo-subject-resolution` and consumed here), Policy selection, Engine invocation, and the
Debit-Plan invariant boundary (defined by the resolution-policy-engine feature as
`cpt-cf-quota-enforcement-algo-engine-boundary` and invoked here), and notification dispatch (the notifications
feature; this feature only enqueues events in the same transaction as its counter mutation, invariant I11).

**Requirements**: `cpt-cf-quota-enforcement-fr-debit`, `cpt-cf-quota-enforcement-fr-credit`,
`cpt-cf-quota-enforcement-fr-return`, `cpt-cf-quota-enforcement-fr-rollback`,
`cpt-cf-quota-enforcement-fr-evaluate-preview`, `cpt-cf-quota-enforcement-fr-idempotency`,
`cpt-cf-quota-enforcement-fr-period-semantics`, `cpt-cf-quota-enforcement-fr-period-rollover`,
`cpt-cf-quota-enforcement-fr-quota-type-allocation`, `cpt-cf-quota-enforcement-fr-quota-type-consumption`,
`cpt-cf-quota-enforcement-nfr-evaluation-latency`, `cpt-cf-quota-enforcement-nfr-throughput`,
`cpt-cf-quota-enforcement-nfr-subject-scale`, `cpt-cf-quota-enforcement-nfr-quota-density`,
`cpt-cf-quota-enforcement-nfr-availability`, `cpt-cf-quota-enforcement-nfr-fault-tolerance`,
`cpt-cf-quota-enforcement-nfr-idempotency-guarantee`

**Principles**: none of its own; the feature executes the strict-engine-boundary and fail-closed principles owned by
earlier features (per DECOMPOSITION §2.5).

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-quota-enforcement-actor-quota-consumer` | Sends debit with a client-supplied idempotency key, rollback of a charge, and preview |
| `cpt-cf-quota-enforcement-actor-quota-resolver` | Sends debit and return through the resolver plugin |
| `cpt-cf-quota-enforcement-actor-quota-manager` | Uses the same S2S credit and preview surface for authorized tenant-management workflows |
| `cpt-cf-quota-enforcement-actor-quota-reader` | Uses preview as a read-only "what would happen" affordance |
| `cpt-cf-quota-enforcement-actor-storage-backend` | Serializes concurrent counter mutations and provides the durable-commit guarantee behind RPO = 0 |
| `cpt-cf-quota-enforcement-actor-monitoring-system` | Scrapes `denial_total` and the pipeline stage traces |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) (§5.3 quota types, §5.4 period and reset semantics, §5.5 debit/credit/rollback/preview,
  §5.8 idempotency, §3.4 Decision contract, §6.1 gear NFRs)
- **Design**: [DESIGN.md](../DESIGN.md) (`QuotaEnforcementService`, `EvaluationOrchestrator`, `IdempotencyCache`,
  `RetentionSweeper`, storage-plugin counter-mutation group, §3.3 error model, §3.6 sequences, §4.1 telemetry, §4.2
  performance verification)
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md) (§2.5)
- **ADR**: [ADR-0002 Acquisition ordering](../ADR/0002-cpt-cf-quota-enforcement-adr-acquisition-ordering.md),
  [ADR-0003 Metadata snapshot timing](../ADR/0003-cpt-cf-quota-enforcement-adr-metadata-snapshot-timing.md),
  [ADR-0004 Settlement window emit](../ADR/0004-cpt-cf-quota-enforcement-adr-settlement-window-emit.md),
  [ADR-0008 Quota resolver subjects](../ADR/0008-cpt-cf-quota-enforcement-adr-quota-resolver-subjects.md)
  (the subject set this pipeline consumes),
  [ADR-0010 Debit plan of chains](../ADR/0010-cpt-cf-quota-enforcement-adr-debit-plan-chains.md) (unwinding),
  [ADR-0011 Charge ledger](../ADR/0011-cpt-cf-quota-enforcement-adr-charge-ledger.md) (return, rollback, retention),
  [ADR-0012 Metric catalogue](../ADR/0012-cpt-cf-quota-enforcement-adr-contract-catalogue.md) (operations on an
  unavailable metric),
  [ADR-0013 Metric types](../ADR/0013-cpt-cf-quota-enforcement-adr-metric-types.md) (the metric of a request)
- **Dependencies**: `cpt-cf-quota-enforcement-feature-quota-lifecycle` (Quota records and the allocation-counter
  materialization), `cpt-cf-quota-enforcement-feature-resolution-policy-engine` (Policy selection, Engine invocation,
  and the Debit-Plan invariant boundary), plus transitively
  `cpt-cf-quota-enforcement-feature-projection-contracts` (Metric Types & Subject Admission: ingress validation and
  subject admission; this document calls it the projection-contracts feature, after its file name)
  and `cpt-cf-quota-enforcement-feature-foundation` (storage plugin, admission, coordination, telemetry conventions)

## 2. Actor Flows (CDSL)

**Use cases**: `cpt-cf-quota-enforcement-usecase-debit` (the debit body; admission and ingress prefixes are owned by
the foundation and projection-contracts features), `cpt-cf-quota-enforcement-usecase-cascade-via-cel` and
`cpt-cf-quota-enforcement-usecase-region-gated-via-metadata` (the debit that exercises them; the Policy authoring side
is owned by the resolution-policy-engine feature)

### Debit

- [x] `p1` - **ID**: `cpt-cf-quota-enforcement-flow-debit`

Realises `cpt-cf-quota-enforcement-seq-debit`.

**Actor**: `cpt-cf-quota-enforcement-actor-quota-consumer`

**Success Scenarios**:
- An `Allowed` Decision whose Debit Plan is applied atomically across every named Quota, with the idempotency record,
  operation-log entry, and outbox events committed in the same transaction
- A `Denied` Decision returned as an HTTP 200 verdict with every counter unchanged
- A replay of the same idempotency key returning the original Decision verbatim with no second counter effect

**Error Scenarios**:
- `amount <= 0`: `INVALID_AMOUNT` before any pipeline step; nothing is persisted
- Same key, different payload (other subjects included): `IDEMPOTENCY_PAYLOAD_MISMATCH` (409); the original record is
  untouched
- Target metric classified `Direct`: `METRIC_NOT_QUOTA_GATED`; no counter mutation, no idempotency, operation-log, or
  lease record
- Engine timeout, invariant violation, or mid-flight storage failure: platform-canonical error, fail-closed, no
  counter mutation

**Steps**:
1. [x] - `p1` - Caller sends `POST /quota-enforcement/v1/operations/debit` with a `DebitRequest` carrying `tenant_id`, `{type, id}` subjects, the metric object `metric: {type, id?, metadata}`, positive integer `amount`, and the client-supplied idempotency key; in the metric object, `type` is the **metric type** (the GTS type that the metric's owner registers, ADR-0013), `id` is optional and names the one thing the amount is counted for, and `metadata` holds the attributes of the operation that policies read; foundation admission (`cpt-cf-quota-enforcement-flow-authorized-admission`) and projection-contracts ingress validation (`cpt-cf-quota-enforcement-flow-ingress-validation`) have already run - `inst-deb-request`
2. [x] - `p1` - **IF** `amount <= 0` - `inst-deb-amount-if`
   1. [x] - `p1` - **RETURN** `INVALID_AMOUNT` (`DomainError::InvalidAmount`, canonical `InvalidArgument`) before idempotency lookup, subject resolution, or any other pipeline step; no idempotency record, operation-log entry, or counter mutation occurs (`cpt-cf-quota-enforcement-fr-debit`) - `inst-deb-amount`
3. [x] - `p1` - Run the pipeline `cpt-cf-quota-enforcement-algo-evaluation-pipeline`; on `Allowed`, the storage plugin applies the Engine's Debit Plan atomically: for every entry, an allocation Quota increments its in-flight counter and a consumption Quota increases the current-period consumed amount by `entry.amount`, and writes the charge; Quotas not named in the plan are never mutated - `inst-deb-pipeline`
4. [x] - `p1` - Decision-shaped request fields are silently ignored per the PRD §3.4 trust boundary; the Decision is server-derived and response-only - `inst-deb-trust`
5. [x] - `p1` - **IF** the Decision is `Denied` - `inst-deb-denied-if`
   1. [x] - `p1` - **RETURN** the Decision as an HTTP 200 verdict with an empty `debit_plan` and no counter mutation; increment `denial_total` by the closed `reason` kind - `inst-deb-denied`
6. [x] - `p1` - **RETURN** the `Decision` body (`result`, `debit_plan`, `diagnostics`); the SDK path is `QuotaEnforcementApi::debit(req)` returning `Decision`; a Decision and a `Problem` are mutually exclusive outcomes - `inst-deb-return`

### Credit

- [x] `p1` - **ID**: `cpt-cf-quota-enforcement-flow-credit`

Realises `cpt-cf-quota-enforcement-seq-credit`.

**Actor**: `cpt-cf-quota-enforcement-actor-quota-manager`

**Success Scenarios**:
- The named Quota's remaining capacity increases by the credited amount, with the `quota-counter-adjusted` event and
  the operation-log entry committed in the same transaction
- A replay of the same idempotency key returns the original outcome without a second counter effect
- A credit to a Quota whose metric has an unavailable catalogue entry runs as any other; a **catalogue entry** is
  what QE keeps of a metric type it has read, and it is served or unavailable (ADR-0012)

**Error Scenarios**:
- `amount <= 0`: `INVALID_AMOUNT` before idempotency lookup or the row-locked read
- Unknown `quota_id`, or a Quota outside the caller's `AccessScope`: canonical `NotFound` (404, `UNKNOWN_QUOTA`); the two
  are indistinguishable
- Quota visible in the caller's scope but owned by a tenant other than the request's: `PdpDenied` (403, storage
  defense-in-depth)
- Deactivated Quota: `QUOTA_DEACTIVATED` (400)
- Consumption Quota whose calendar window has elapsed: `PERIOD_CLOSED` (400)

**Steps**:
1. [x] - `p1` - Quota Manager sends `POST /quota-enforcement/v1/operations/credit` with a `CreditRequest` naming an explicit `quota_id`, a positive integer `amount`, and an idempotency key; credit invokes no subject resolution and no Engine (`cpt-cf-quota-enforcement-fr-credit`) - `inst-cre-request`
2. [x] - `p1` - **IF** `amount <= 0` - `inst-cre-amount-if`
   1. [x] - `p1` - **RETURN** `INVALID_AMOUNT` before idempotency lookup, the row-locked Quota read, or any other pipeline step; nothing is persisted - `inst-cre-amount`
3. [x] - `p1` - DB: read the Quota row by `quota_id` and the caller's `AccessScope` in one predicate, under a row lock, so the four rejection arms and the mutation share atomic semantics; a row that is absent, outside the scope, or another tenant's is rejected here, before the replay lookup - `inst-cre-lock`
4. [x] - `p1` - DB: under the key lock, which the lock order takes after the Quota row, `lookup_idempotency`; on an exact replay **RETURN** the stored outcome verbatim per `cpt-cf-quota-enforcement-algo-idempotency-replay`, before the guards that apply only to a fresh credit - `inst-cre-idem`
5. [x] - `p1` - **IF** the row is absent or outside the scope, in scope but another tenant's, deactivated, or a consumption Quota whose calendar window has elapsed (`time >= period_end` at the moment the transaction is evaluated) - `inst-cre-guard-if`
   1. [x] - `p1` - **RETURN** the matching rejection before any mutation: `StorageError::QuotaNotFound` lifts to `NotFound { kind: "quota" }` (404), `StorageError::SubjectOutOfScope` lifts to `PdpDenied` (403), `StorageError::QuotaDeactivated` lifts to `QUOTA_DEACTIVATED` (400), and `StorageError::PeriodClosed` lifts to `PERIOD_CLOSED` (400); credit closure is calendar-keyed, so the rejection fires immediately at the boundary even while the settlement window is still draining cross-period lease commits - `inst-cre-guard`
6. [x] - `p1` - DB: `apply_credit(quota_id, amount, idem_key, events)` in the same transaction: an allocation Quota decrements its in-flight counter and a consumption Quota decreases the current-period consumed amount, both with a floor of zero; the idempotency record under `(tenant, credit, key)` and the operation-log entry are persisted, and the `quota-counter-adjusted` event carrying the credited amount, the target `quota_id`, and the actor that admission returned is enqueued (I11); commit - `inst-cre-apply`
7. [x] - `p1` - **RETURN** the outcome; the SDK path is `QuotaManagerApi::credit(req)` returning `Decision` - `inst-cre-return`

### Return

- [x] `p1` - **ID**: `cpt-cf-quota-enforcement-flow-return`

**Actor**: `cpt-cf-quota-enforcement-actor-quota-resolver`

**Success Scenarios**:
- Part of a charge is given back to the quotas the charge reached, with the `quota-rollback-applied` event committed
  in the same transaction
- A repeated return, with the same key, charge, and amount, changes nothing and gets the first result

**Error Scenarios**:
- `amount <= 0`: `INVALID_AMOUNT` before any other step
- No charge under `(tenant, operation, key)`, including a deleted charge: `UNKNOWN_OPERATION` (canonical `NotFound`)
- The return names a metric type other than the charge's: `METRIC_TYPE_MISMATCH` (canonical `InvalidArgument`)
- Amount above what remains of the charge: canonical `FailedPrecondition` (`RETURN_EXCEEDS_CHARGE`)
- No entry can take a part of the amount (every entry that the amount reaches is on a rate Quota, or on a consumption
  Quota whose period is settled or whose charge is past its return window): `RETURN_NOT_APPLICABLE` (canonical
  `FailedPrecondition`); a counter
  already at zero does not cause this, and an allocation entry always takes a return
- A return key already recorded in the tenant, with another charge or amount: `IDEMPOTENCY_PAYLOAD_MISMATCH`
- A charge whose metric is now classified `Direct`, or whose catalogue entry is unavailable, does not stop a return:
  a return makes no `METRIC_NOT_QUOTA_GATED` check (ADR-0012)
- Every failure changes nothing

**Steps**:
1. [x] - `p1` - The resolver plugin calls return with the tenant, the charge's operation (debit or lease commit) and idempotency key, the id of the metric type, the amount, and the return's own idempotency key; the call is admitted in trusted mode (`cpt-cf-quota-enforcement-flow-authorized-admission`) and is not part of QE's REST API (`cpt-cf-quota-enforcement-fr-return`) - `inst-rtn-request`
2. [x] - `p1` - **IF** `amount <= 0` **RETURN** `INVALID_AMOUNT`; nothing is persisted - `inst-rtn-amount`
3. [x] - `p1` - DB: read the charge by `(tenant, operation, key)` to learn the Quotas of its entries, then take the locks in the lock rank of DESIGN (Rollback and Return): those Quota rows in ascending `quota_id` order (ADR-0002), the key lock of the charge and the key lock of the return key `(tenant, return, key)`, then the counter rows of the entries; read the charge again under the locks; a return with the same key that is running ends first, so its return record is then found; whether a period is settled is read under the counter lock in this transaction - `inst-rtn-lock`
4. [x] - `p1` - **IF** a return record under `(tenant, return key)` exists: with another charge or amount **RETURN** `IDEMPOTENCY_PAYLOAD_MISMATCH`; with the same charge and amount and a metric type that is not the metric type of the charge **RETURN** `METRIC_TYPE_MISMATCH`, as a first call does; otherwise **RETURN** the first result and change nothing; the return record is deleted with its charge and is the return's only idempotency record - `inst-rtn-idem`
5. [x] - `p1` - **IF** the charge is absent or deleted (`cpt-cf-quota-enforcement-algo-retention-sweep`), the metric type of the return is not the metric type of the charge, the amount exceeds what remains of it, or no entry can take a part of the unwound amount by the rules of step 7 (the current value of a counter does not decide this) - `inst-rtn-reject-if`
   1. [x] - `p1` - **RETURN** the matching error from the error scenarios; the return is all or nothing - `inst-rtn-reject`
6. [x] - `p1` - Unwind the amount over the charge's entries (`cpt-cf-quota-enforcement-algo-unwind`) - `inst-rtn-unwind`
7. [x] - `p1` - DB: add each unwound amount to its entry's returned amount and give it to the entry's Quota by type: an allocation Quota's held amount decreases; a consumption Quota's counter of the charge's period decreases inside the return window and while that period is not settled, otherwise it does not change; a rate Quota's counter does not change; a deactivated Quota takes the amount as an active one does, and a counter decreases by the entry's amount or to zero, never below; write the return record with the key, the charge, and the amount, unique per `(tenant, return key)`, append the operation-log entry, and enqueue `quota-rollback-applied` with the charge's operation and key, the returned amount, what remains of the charge, the affected Quotas, and the actor (I11); commit - `inst-rtn-apply`
8. [x] - `p1` - **RETURN** no result - `inst-rtn-return`

### Rollback

- [x] `p1` - **ID**: `cpt-cf-quota-enforcement-flow-rollback`

Realises `cpt-cf-quota-enforcement-seq-rollback`.

**Actor**: `cpt-cf-quota-enforcement-actor-quota-consumer`

**Success Scenarios**:
- Everything that remains of the charge is returned, as by `cpt-cf-quota-enforcement-flow-return`
- A rollback of a charge with nothing left succeeds and changes nothing
- A rollback of a charge whose metric has an unavailable catalogue entry runs as any other (ADR-0012)

**Error Scenarios**:
- No charge under the tenant, operation, and key, a deleted charge, or a charge outside the caller's access:
  `UNKNOWN_OPERATION` (canonical `NotFound`); the three are indistinguishable
- An amount remains but no entry can take a part of it, as for a return: `RETURN_NOT_APPLICABLE` (canonical
  `FailedPrecondition`); no event
- Rollback targeting a credit: `UNKNOWN_OPERATION`; a credit writes no charge

**Steps**:
1. [x] - `p1` - Caller sends `POST /quota-enforcement/v1/operations/rollback` with a `RollbackRequest` that names the charge by the tenant, the charge's operation (debit or lease commit), and the charge's idempotency key, and carries the attribution of the charged operation: the subjects and the metric with its type and optional id; it carries no amount and no idempotency key of its own - `inst-rlb-request`
2. [x] - `p1` - The attribution passes the same PDP admission as a debit's, for the action `rollback`, before any storage access, and the PDP returns an access scope; QE computes the attribution digest of the admitted attribution (the tenant, the subjects, and the metric with its type and optional id), the digest that every charge keeps of its own attribution (ADR-0011) - `inst-rlb-authz`
3. [x] - `p1` - DB: in the transaction, take the locks as in step 3 of `cpt-cf-quota-enforcement-flow-return`, without a return key, and apply the access scope to the charge; a lease-commit charge is named by the commit call's idempotency key exactly like a debit (`cpt-cf-quota-enforcement-fr-rollback`) - `inst-rlb-lookup`
4. [x] - `p1` - **IF** no charge exists, it is deleted, it is outside the caller's access, or its attribution digest differs from the digest of step 2 - `inst-rlb-unknown-if`
   1. [x] - `p1` - **RETURN** `UNKNOWN_OPERATION` (canonical `NotFound`) - `inst-rlb-unknown`
5. [x] - `p1` - **IF** nothing remains of the charge **RETURN** success with no change and no event; this makes a repeated rollback safe without a key - `inst-rlb-idem`
6. [x] - `p1` - **IF** no entry can take a part of the remainder (for example, its only entries are on a settled consumption period) - `inst-rlb-settled-if`
   1. [x] - `p1` - **RETURN** `RETURN_NOT_APPLICABLE` (canonical `FailedPrecondition`) before any mutation, with no event - `inst-rlb-settled`
7. [x] - `p1` - Return the remainder of the charge as in steps 6–7 of `cpt-cf-quota-enforcement-flow-return`, with no return key recorded; the `quota-rollback-applied` event has the same payload as for a return (I11); commit - `inst-rlb-apply`
8. [x] - `p1` - **RETURN** success; the SDK path is `QuotaEnforcementApi::rollback(req)` - `inst-rlb-return`

### Evaluate Preview

- [x] `p1` - **ID**: `cpt-cf-quota-enforcement-flow-evaluate-preview`

Realises `cpt-cf-quota-enforcement-seq-evaluate-preview`.

**Actor**: `cpt-cf-quota-enforcement-actor-quota-consumer` (also
`cpt-cf-quota-enforcement-actor-quota-manager` and `cpt-cf-quota-enforcement-actor-quota-reader`)

**Success Scenarios**:
- The full evaluation pipeline runs read-only and returns the would-be Decision with an explicit `preview: true`
  marker and Policy attribution in diagnostics

**Error Scenarios**:
- PDP denial or unreachability: canonical error, identical to the corresponding write (fail-closed)
- Ingress-validation failures: canonical `InvalidArgument` per the projection-contracts feature
- Debit-Plan invariant violation: canonical `Internal` with the `INVARIANT_VIOLATION` sub-token, identical to the
  corresponding write (fail-closed)

**Steps**:
1. [x] - `p1` - Caller sends `POST /quota-enforcement/v1/operations/preview` with the same tenant, subjects, metric object, and amount as the corresponding write but no idempotency key; preview requires the same PDP authorization as the corresponding write (`cpt-cf-quota-enforcement-fr-evaluate-preview`) - `inst-prv-request`
2. [x] - `p1` - Consume the admitted subject set (`cpt-cf-quota-enforcement-algo-subject-resolution`), then DB: `read_quota_snapshot` as a transactional snapshot read of current counter state with no read-modify hold and no contention against concurrent debits (I3), keeping only the Quotas whose conditions hold on the request's metadata (I15) - `inst-prv-read`
3. [x] - `p1` - DB: `read_policy(scope)`; invoke the Engine boundary (`cpt-cf-quota-enforcement-algo-engine-boundary`) unchanged: an invariant violation on the returned Decision surfaces the same canonical `Internal` error as the corresponding write (fail-closed) and increments `debit_plan_invariant_violations_total` per the engine boundary - `inst-prv-engine`
4. [x] - `p1` - Persist nothing: no counter mutation, no held capacity, no idempotency record, no operation-log entry, and no outbox event, and no period row even across a boundary; only ephemeral telemetry counters move - `inst-prv-nopersist`
5. [x] - `p1` - **RETURN** `DecisionPreview` carrying the full Decision shape plus `preview: true`, with `policy_id` and `policy_version` in diagnostics; the SDK path is `QuotaEnforcementApi::evaluate_preview(req)`; the verdict can be invalidated by concurrent debits, and callers that need held capacity use the lease flow instead (lease-operations feature) - `inst-prv-return`

## 3. Processes / Business Logic (CDSL)

### Evaluation Orchestrator Pipeline

- [x] `p1` - **ID**: `cpt-cf-quota-enforcement-algo-evaluation-pipeline`

**Input**: a validated write request (admitted tenant/subjects, metric object, amount, idempotency key), and the
actor and the `AccessScope` returned by admission (DESIGN, SecurityContext propagation)

**Output**: a committed Decision with all side effects applied atomically, or a platform-canonical error with no
counter mutation

**Steps**:
1. [x] - `p1` - Consume the complete admitted subject set from `cpt-cf-quota-enforcement-algo-subject-resolution`; `EvaluationOrchestrator` never re-admits it - `inst-pipe-resolve`
2. [x] - `p1` - **IF** the operation targets a Quota whose metric is classified `Direct` - `inst-pipe-gated-if`
   1. [x] - `p1` - **RETURN** `METRIC_NOT_QUOTA_GATED` (canonical `FailedPrecondition`; the service decides it from the `enforcement` trait in the metric's `MetricCatalog` entry, the storage plugin does not decide the mode) with no counter mutation and no idempotency, operation-log, or lease record; this admission-time rejection applies to every entry point that takes or checks capacity (debit, reserve, batch debit, preview) and to no other: credit, rollback, return, commit, and release never make this check (PRD §3.2); this check and the catalogue check of admission run before the idempotency lookup, so a repeated request for a metric whose type was revised to `direct`, or became unavailable, after its charge gets this error or `FailedPrecondition` and not the stored result - `inst-pipe-gated`
3. [x] - `p1` - Construct the typed `IdempotencyScope { tenant_id, operation_type, idem_key }`; the admitted subject set is part of the request fingerprint, not of the scope, so neither the order of the subjects nor naming the tenant changes it; then DB: `lookup_idempotency(scope)`; **IF** a record exists, short-circuit to `cpt-cf-quota-enforcement-algo-idempotency-replay` without opening the evaluation transaction - `inst-pipe-idem`
4. [x] - `p1` - DB: begin the single backend transaction and run the applicable-Quotas locked read (`read_quota_snapshot` with lock), keeping only the Quotas whose conditions hold on the request's metadata, judged on the locked rows (I15); `EvaluationContext` metadata is captured at this locked-read step per ADR-0003, and lazy period-row materialization runs here when a boundary was crossed (`cpt-cf-quota-enforcement-algo-period-rollover`) - `inst-pipe-lockread`
5. [x] - `p1` - DB: `read_policy(scope = metric, else global)`, then invoke the Engine boundary (`cpt-cf-quota-enforcement-algo-engine-boundary`, resolution-policy-engine feature): Policy selection, the synchronous no-I/O `evaluate` call, the per-Policy timeout, `EngineError` lifting, and the closed Debit-Plan invariant set are all specified there and invoked here unchanged - `inst-pipe-engine`
6. [x] - `p1` - **IF** the boundary surfaces a canonical error (timeout, cost cap, invariant violation, internal) - `inst-pipe-fail-if`
   1. [x] - `p1` - **RETURN** the canonical error and roll the transaction back; fail-closed, no counter mutation, no idempotency record - `inst-pipe-fail`
7. [x] - `p1` - DB: `apply_debit_plan(applicable, plan, idem_scope, events)`: mutate every plan entry's counter, write the charge for an `Allowed` Decision (tenant, operation, idempotency key, request fingerprint, attribution digest, first result, metric, amount, and per entry its chain, position, quota, period, charged amount, and returned amount; unique per `(tenant, operation, key)`), which is the idempotency record of that operation for as long as it lives, append the operation-log entry, and enqueue the outbox events, all inside the same transaction (I1, I2, I11); a `Denied` Decision changes no counter, writes no charge, and persists an idempotency record with an empty plan so an exact replay returns the original verdict (PRD §5.8), while the operation log records only successful mutating operations per the DESIGN `OperationLog` definition; a `NO_APPLICABLE_QUOTA` denial persists no idempotency record and no operation-log entry per the PRD §5.8 exception for that denial; concurrent multi-Quota mutations follow the deterministic lexicographic `quota_id` acquisition ordering of ADR-0002 - `inst-pipe-apply`
8. [x] - `p1` - Forward the `AccessScope` unmodified into every storage read and write, where `SecureConn` compiles it into query filters (phase-2 defense-in-depth per `cpt-cf-quota-enforcement-algo-pdp-constraint-composition`); the orchestrator never calls the PDP, holds no `types-registry` client, and performs no schema resolution, so registry latency cannot enter the transaction - `inst-pipe-scope`
9. [x] - `p1` - Emit the `quota_enforcement.operations.debit` span with the admission phase and the evaluation as sibling spans and the storage call under the evaluation, per DESIGN §4.1 - `inst-pipe-spans`
10. [x] - `p1` - **RETURN** the committed Decision; orchestrator instances are not singletons, join no cluster election, and delegate all synchronization between concurrent instances to the storage plugin's serialization of row mutations (I9) - `inst-pipe-return`

### Unwinding

- [x] `p1` - **ID**: `cpt-cf-quota-enforcement-algo-unwind`

**Input**: an amount X and the entries of a charge or of a lease's holds, each with its chain, position, and the
amount it still holds

**Output**: the amount to take back from each entry

**Steps**:
1. [x] - `p1` - **FOR EACH** chain: take X from its entries in reverse order, last entry first, each entry at most what it still holds; each chain gives back exactly X (`cpt-cf-quota-enforcement-adr-debit-plan-chains`) - `inst-unw-chain`
2. [x] - `p1` - **RETURN** the per-entry amounts; return and rollback apply them to a charge, and a lease commit below the held amount applies them to the lease's holds (lease-operations feature) - `inst-unw-return`

### Idempotency Lookup and Replay

- [x] `p1` - **ID**: `cpt-cf-quota-enforcement-algo-idempotency-replay`

Realises `cpt-cf-quota-enforcement-seq-idempotency-replay`.

**Input**: a write operation's typed `IdempotencyScope`, the operation payload, the actor returned by
admission

**Output**: the stored outcome verbatim, an `IDEMPOTENCY_PAYLOAD_MISMATCH` rejection, or a cache miss that proceeds to
full evaluation

**Steps**:
1. [x] - `p1` - Construct the typed `IdempotencyScope { tenant_id, operation_type, idem_key }`; the key of every operation is unique per `(tenant, operation, key)`, and the subjects are part of the request fingerprint (the payload hash), not of the scope (`cpt-cf-quota-enforcement-fr-idempotency`) - `inst-idem-scope`
2. [x] - `p1` - Different tenants or operation types using the same key string create independent records; the same key with another payload, other subjects included, is a payload mismatch - `inst-idem-independent`
3. [x] - `p1` - Consult the `IdempotencyCache` in-process LRU of recent records (operator-tunable TTL, P1 reference default 5 s) for the most contended keys, then DB: `lookup_idempotency(scope)`; an allowed debit or lease commit is found by its charge, which stores the request fingerprint - `inst-idem-lookup`
4. [x] - `p1` - **IF** a record exists and the canonical SHA-256 hash of the sorted-JSON payload matches the stored `payload_hash` - `inst-idem-replay-if`
   1. [x] - `p1` - **RETURN** the stored `decision_blob` verbatim, including `result`, `debit_plan`, `diagnostics`, and the original identifiers and amounts; the Engine is never re-invoked, counters are never touched, and the non-deterministic `EvaluationContext` fields (notably `time`) are never re-bound, so a replay at a different wall-clock time still produces the original verdict; replay diagnostics surface the recorded `engine_id`, `policy_id`, and `policy_version` verbatim - `inst-idem-replay`
5. [x] - `p1` - **IF** a record exists and the payload hash diverges - `inst-idem-mismatch-if`
   1. [x] - `p1` - **RETURN** `IDEMPOTENCY_PAYLOAD_MISMATCH` (`DomainError::IdempotencyPayloadMismatch`, canonical `AlreadyExists`, 409; not `Aborted`, because a retry with the same key never passes) without touching the original record - `inst-idem-mismatch`
6. [x] - `p1` - On a miss, proceed with the full pipeline; the record is persisted implicitly by the mutating storage primitive in the same transaction as the mutation (I1, I2), capturing the full Decision blob plus the `engine_id`, `policy_id`, and `policy_version` under which it was produced; the `decision_blob` is JSON-typed and schema-versioned (top-level `__version`) so additive P2/P3 shape changes need no migration; concurrent misses on one scope cannot both apply: the transaction takes the key lock of the scope after its Quota rows (lock rank, DESIGN) and re-reads the record before evaluating, and an insert that loses the primary-key race on the scope rolls back and returns the winner's stored outcome (or `IDEMPOTENCY_PAYLOAD_MISMATCH`), so counters move once; a loser whose contention budget expires first gets the retryable contention error and replays on retry - `inst-idem-persist`
7. [x] - `p1` - **RETURN** a replay after the replay window is a new operation, re-evaluated against current state (`cpt-cf-quota-enforcement-algo-retention-sweep` owns the reclamation), except an allowed debit or lease commit whose charge still lives: the same fingerprint gets the first result and another fingerprint is a payload mismatch - `inst-idem-window`

### Period Rollover and Settlement

- [x] `p1` - **ID**: `cpt-cf-quota-enforcement-algo-period-rollover`

Realises `cpt-cf-quota-enforcement-seq-period-rollover`.

**Input**: a consumption Quota's counter row with its persisted period boundary timestamp, the transaction commit time

**Output**: deterministic period attribution, the new period row, and the `period-rollover` event for the closing
period

**Steps**:
1. [x] - `p1` - Periods are drawn from the five GTS instances under `gts.cf.core.qe.period_type.v1~` (`day`, `week`, `month`, `year`, `one_time`), all UTC and calendar-aligned by default; the current period boundary timestamp is persisted with each consumption counter for deterministic detection (`cpt-cf-quota-enforcement-fr-period-semantics`) - `inst-per-spec`
2. [x] - `p1` - Attribute every single-shot mutation to the period whose half-open interval `[start, end)` contains the mutation transaction's commit timestamp; a debit committing at exactly `boundary_at` is accounted to period `P+1`; leases are attributed to their acquisition period, per the lease-operations feature and I5 - `inst-per-attr`
3. [x] - `p1` - **IF** any mutating operation observes `now() >= period_end` for a consumption Quota (lazy detection) - `inst-per-lazy-if`
   1. [x] - `p1` - DB: atomically materialize the new `quota_consumption_counters` row with `consumed = 0` and `highest_crossed_threshold_pct = NULL` (I13), so `threshold-crossed` notifications can fire again in the new period per the notifications feature's emission rule - `inst-per-lazy`
4. [x] - `p1` - Mark the closing-period row settled only after every active lease with the closing acquisition period has resolved, and enqueue the `period-rollover` event carrying the closing-period consumed amount, the closing-period cap, and the new period boundary; the event is emitted strictly after the last commit attributed to the closing period, so it signals settlement completion, not the calendar transition, and may lag the boundary by up to `max_lease_ttl` - `inst-per-settle`
5. [x] - `p1` - During the settlement window, cross-period lease commits and releases mutate the closing-period counter and emit no `quota-counter-adjusted` or `threshold-crossed` events (`cpt-cf-quota-enforcement-adr-settlement-window-emit`) - `inst-per-window`
6. [x] - `p1` - Unused capacity is forfeited at every boundary; QE models no carry-over, and downstream consumers may derive unused-capacity figures from the `period-rollover` payload - `inst-per-forfeit`
7. [x] - `p1` - **RETURN** the current-period row; a `one_time` period never auto-resets and its Quota is deactivated explicitly when exhausted or expired; for Quotas with no operations in the new period the closing event fires only on the next operation, a known P1 limitation with a P2 active-scheduler hook (DESIGN §4.3) - `inst-per-return`

### Retention Sweep

- [x] `p1` - **ID**: `cpt-cf-quota-enforcement-algo-retention-sweep`

**Input**: the `idempotency_retention_config` table (default 24 h, per-`(tenant, metric)` overrides), the
operation-log retention setting (default 30 days), the coordination adapter's `retention-sweeper` election

**Output**: physically reclaimed expired idempotency records, operation-log rows, and deleted charges

**Steps**:
1. [x] - `p1` - API: `RetentionSweeper` hands its reclamation body to the coordination adapter for `SingletonScope::RetentionSweeper`; the body runs only while this replica is elected, on a child `CancellationToken` that leadership loss cancels; the resolved cluster backend renews the claim and re-election is automatic (foundation `cpt-cf-quota-enforcement-dod-coordination-adapter` owns the election semantics) - `inst-ret-elect`
2. [x] - `p1` - DB: `reclaim_expired_idempotency` in operator-configured batches, honoring the per-`(tenant, metric)` retention window from `idempotency_retention_config`; the window bounds the replay guarantee of `cpt-cf-quota-enforcement-fr-idempotency` - `inst-ret-idem`
3. [x] - `p1` - DB: `reclaim_operation_log` in operator-configured batches under the 30-day default (PRD §6.2); consumption-counter partition retention (default 13 months) is handled by the storage plugin's partition reclamation, not by this sweeper - `inst-ret-oplog`
4. [x] - `p1` - DB: remove deleted charges in operator-configured batches; a charge is deleted when the replay window has passed since its creation and since the last return or rollback applied to it, and no allocation entry of it holds an amount on an active Quota; from that moment it is unknown to every operation, whether or not this sweep has removed it, and a debit or commit under its key replaces it (`cpt-cf-quota-enforcement-adr-charge-ledger`) - `inst-ret-charge`
5. [x] - `p1` - DB: `read_held_allocation_charge_stats`; publish the gauges `allocation_charges_past_replay_window` and `allocation_charge_max_age_seconds` by canonical registered `metric`: the allocation charges that hold an amount on an active Quota and are older than the replay window, and the age of the oldest one; a follower publishes nothing - `inst-ret-charge-gauge`
6. [x] - `p1` - **RETURN** after each cycle; sweeper outage never affects counter correctness, only storage growth, and a killed holder's lock becomes acquirable by a survivor within one TTL - `inst-ret-return`

## 4. States (CDSL)

### Consumption Period Counter State Machine

- [x] `p1` - **ID**: `cpt-cf-quota-enforcement-state-consumption-period`

**States**: Absent, Current, SettlementWindow, Settled

**Initial State**: Absent (consumption counter rows are created lazily by the first mutating operation)

**Transitions**:
1. [x] - `p1` - **FROM** Absent **TO** Current **WHEN** the first mutating operation touching the Quota in the period materializes the row with `consumed = 0` and `highest_crossed_threshold_pct = NULL` (I13) - `inst-perst-create`
2. [x] - `p1` - **FROM** Current **TO** SettlementWindow **WHEN** the calendar boundary passes (`now() >= period_end`); new single-shot mutations attribute to the successor period, credits against this period are rejected with `PERIOD_CLOSED` (calendar-keyed), while cross-period lease commits, returns, and rollbacks still land against this period's counter - `inst-perst-boundary`
3. [x] - `p1` - **FROM** SettlementWindow **TO** Settled **WHEN** every active lease acquired in the period has resolved and the `period-rollover` event is enqueued; the closing consumed amount is final: a return or rollback does not change this period's counter - `inst-perst-settle`

Settled is terminal: rows are retained for the operator-configured historical window (default 13 months) and reclaimed
via partition drop by the storage plugin. Allocation counters have no period dimension and no state machine here; their
single in-flight counter persists across calendar boundaries until explicitly modified.

### Idempotency Record State Machine

- [x] `p1` - **ID**: `cpt-cf-quota-enforcement-state-idempotency-record`

**States**: Absent, Active, Reclaimed

**Initial State**: Absent

**Transitions**:
1. [x] - `p1` - **FROM** Absent **TO** Active **WHEN** a mutating storage primitive persists the record in the same transaction as its operation's commit (the counter mutation for an applied plan; no counter change for a persisted `Denied` verdict) (I1, I2), capturing the payload hash, the full Decision blob, and the Engine/Policy attribution - `inst-idemst-persist`
2. [x] - `p1` - **FROM** Active **TO** Reclaimed **WHEN** the record's retention window elapses and `cpt-cf-quota-enforcement-algo-retention-sweep` physically reclaims it; a later submission of the same key is a new operation - `inst-idemst-reclaim`

While Active, an exact replay returns the stored outcome verbatim and a divergent payload returns
`IDEMPOTENCY_PAYLOAD_MISMATCH`; no transition mutates or re-issues an Active record. For an allowed debit or lease
commit the charge is the idempotency record, for as long as the charge lives
(`cpt-cf-quota-enforcement-dod-charge-ledger`).

## 5. Definitions of Done

### Consumption Operation Endpoints

- [x] `p1` - **ID**: `cpt-cf-quota-enforcement-dod-consumption-endpoints`

The system **MUST** deliver `QuotaEnforcementService`
(`cpt-cf-quota-enforcement-component-quota-enforcement-service`) as the S2S entry point for debit, credit, return,
rollback, and preview. Debit, credit, rollback, and preview are the four REST endpoints; debit, rollback and preview
are `QuotaEnforcementApi` methods, credit is `QuotaManagerApi::credit` only, and return is reached only through the
resolver plugin. Debit and credit requests with
`amount <= 0` **MUST** be rejected with `INVALID_AMOUNT` before idempotency lookup (and, for debit, subject resolution)
or any other pipeline step, persisting nothing. A debit or a preview targeting a Quota on a `Direct`-classified metric **MUST** be rejected with
`METRIC_NOT_QUOTA_GATED` with no counter mutation and no idempotency, operation-log, or lease record; a credit, a
rollback, and a return **MUST NOT** be rejected for this reason. Every evaluation
operation returns either a `Decision` (HTTP 200, including `Denied` verdicts) or a `Problem`, never both;
Decision-shaped request fields are silently ignored; `denial_total` increments by closed `reason` kind on every
denial.

**Implements**:
- `cpt-cf-quota-enforcement-flow-debit`
- `cpt-cf-quota-enforcement-flow-credit`
- `cpt-cf-quota-enforcement-flow-return`
- `cpt-cf-quota-enforcement-flow-rollback`
- `cpt-cf-quota-enforcement-flow-evaluate-preview`

**Constraints**: `cpt-cf-quota-enforcement-constraint-toolkit`,
`cpt-cf-quota-enforcement-constraint-security-context`

**Touches**:
- API: `POST /quota-enforcement/v1/operations/debit`, `POST /quota-enforcement/v1/operations/credit`,
  `POST /quota-enforcement/v1/operations/rollback`, `POST /quota-enforcement/v1/operations/preview`
- DB: `cpt-cf-quota-enforcement-db-schema`
- Entities: `DebitRequest`, `CreditRequest`, `RollbackRequest`, `PreviewRequest`, `Decision`, `DecisionPreview`

### Evaluation Orchestrator

- [x] `p1` - **ID**: `cpt-cf-quota-enforcement-dod-evaluation-orchestrator`

The system **MUST** establish `EvaluationOrchestrator` (`cpt-cf-quota-enforcement-component-evaluation-orchestrator`)
implementing the canonical pipeline order (authorized subject-set intake, idempotency lookup, applicable-Quotas locked
read, Policy lookup, Engine evaluate, Debit-Plan invariant check, mutation, charge write, idempotency persist, outbox
enqueue, commit) with everything after the idempotency short-circuit inside one backend transaction. It **MUST**
consume the subject admission of the projection-contracts feature and invoke the Engine boundary of the
resolution-policy-engine feature without re-specifying either; it **MUST** capture `EvaluationContext` metadata at the
locked-read step (ADR-0003), apply Debit Plans through `apply_debit_plan` under the ADR-0002 lexicographic acquisition
ordering, forward the `AccessScope` unmodified into every storage call for `SecureConn` compilation, and emit the
DESIGN §4.1 stage spans. The orchestrator **MUST NOT** call the PDP, hold a `types-registry` client, or join a cluster
election; synchronization between concurrent instances is delegated to the storage plugin (I9).

**Implements**:
- `cpt-cf-quota-enforcement-algo-evaluation-pipeline`

**Constraints**: `cpt-cf-quota-enforcement-constraint-no-business-logic`,
`cpt-cf-quota-enforcement-constraint-security-context`

**Touches**:
- API: no new endpoint (in-process pipeline behind the operation handlers)
- DB: `cpt-cf-quota-enforcement-db-schema`
- Entities: `EvaluationContext`, `Decision`, `QuotaDebitPlan`, `MutationResult`

### Idempotency Guarantee

- [x] `p1` - **ID**: `cpt-cf-quota-enforcement-dod-idempotency`

The system **MUST** enforce typed `IdempotencyScope { tenant_id, operation_type, idem_key }` on every write operation
that carries a key: a key is unique per `(tenant, operation, key)`, and the subjects are part of the request
fingerprint. Exact replays **MUST** return the stored `decision_blob` verbatim without re-invoking the Engine or
re-binding `time`; divergent payloads, other subjects included, **MUST** return `IDEMPOTENCY_PAYLOAD_MISMATCH` (409)
before any change, leaving the original record untouched. The `payload_hash` is the canonical SHA-256 of the
sorted-JSON payload stored as fixed-width binary. The record captures the full Decision blob plus `engine_id`,
`policy_id`, and `policy_version`; `IdempotencyCache` (`cpt-cf-quota-enforcement-component-idempotency-cache`)
provides the in-process LRU (default TTL 5 s) while persistence stays implicit in the mutating storage primitives (I1,
I2). Records live for the replay window (default 24 h, per-`(tenant, metric)`); a later replay is a new operation. The
charge of an allowed debit or lease commit is its idempotency record for as long as the charge lives, also after the
replay window. Rollback carries no key, and a return's key is kept in its return record
(`cpt-cf-quota-enforcement-dod-charge-ledger`). This mechanism is established here and consumed unchanged by the
lease-operations and batch-debit features for their operation types.

**Implements**:
- `cpt-cf-quota-enforcement-algo-idempotency-replay`
- `cpt-cf-quota-enforcement-state-idempotency-record`

**Constraints**: `cpt-cf-quota-enforcement-constraint-security-context`

**Touches**:
- API: no new endpoint (rides every write path)
- DB: `cpt-cf-quota-enforcement-db-schema` (`idempotency_records`, `idempotency_retention_config`)
- Entities: `IdempotencyRecord`, `IdempotencyScope`

### Counter Shapes and Period Semantics

- [x] `p1` - **ID**: `cpt-cf-quota-enforcement-dod-counter-period`

The system **MUST** implement the two P1 counter shapes: allocation Quotas keep a single in-flight counter with no
period (incremented by debit, decremented by credit with a floor of zero, and by return and rollback), and consumption
Quotas keep a per-period consumed counter (increased by debit, decreased by credit with a floor of zero, and by return
and rollback against the charge's period) that resets to zero at every period
boundary. Periods **MUST** be the five GTS instances under `gts.cf.core.qe.period_type.v1~`, UTC and
calendar-aligned, with the current boundary timestamp persisted per counter; consumption rows are materialized lazily
by the first mutating operation; rollover **MUST** be atomic with respect to in-flight operations, attribute
mutations by the half-open `[start, end)` commit-time rule, set `highest_crossed_threshold_pct = NULL` on every new
row (I13), emit the `period-rollover` event strictly after the last commit attributed to the closing period, and suppress
`quota-counter-adjusted` and `threshold-crossed` emission for settlement-window mutations (ADR-0004). All events are
enqueued in the same transaction as the counter mutation (I11); emission rules for `threshold-crossed` and all
dispatch belong to the notifications feature.

**Implements**:
- `cpt-cf-quota-enforcement-algo-period-rollover`
- `cpt-cf-quota-enforcement-state-consumption-period`

**Constraints**: `cpt-cf-quota-enforcement-constraint-toolkit`

**Touches**:
- API: no new endpoint (rides the evaluation paths)
- DB: `cpt-cf-quota-enforcement-db-schema` (`quota_allocation_counters`, `quota_consumption_counters`,
  `operation_log`, `notification_outbox`)
- Entities: `Counter` (allocation), `Counter` (consumption), `OperationLog`

### Charge Ledger and Return

- [x] `p1` - **ID**: `cpt-cf-quota-enforcement-dod-charge-ledger`

The system **MUST** write a charge in the transaction of every allowed debit and lease commit, unique per `(tenant,
operation, key)`, recording the request fingerprint and, per plan entry, its chain, position, quota, period, charged
amount, and returned amount, with the attribution digest (a digest of the tenant, the subjects, and the metric with
its type and optional id) and no subject id or metric `id`. A return **MUST** take its locks in the lock rank of
DESIGN (the
Quota rows of the charge's entries, the key locks of the charge and of the return key, then the counters in ascending
`quota_id` order, ADR-0002), unwind its amount over the charge (`cpt-cf-quota-enforcement-algo-unwind`), and apply
each entry by quota type, all or nothing: allocation always; consumption inside the return window and while the period
is not settled, read under the counter lock; rate never; a deactivated Quota as an active one; no counter below zero.
It **MUST** fail and change nothing when the charge is unknown, the amount exceeds what remains, or no entry can take
a part of the amount; the current value of a counter does not decide that, and an allocation entry always takes a
return. Every applied return **MUST** write one return record with its key, charge, and amount, unique per `(tenant,
return key)` and deleted with the charge: a repeated key with the same charge and amount gets the first result
(`METRIC_TYPE_MISMATCH` when its metric type is not the charge's), with
another charge or amount a payload mismatch, also when the two returns run at the same time. A charge **MUST** take
its return
window, and the time it is kept after a return, from the configuration in force when each is written. Rollback
**MUST** return everything that remains of the charge it names, and succeed without a change when nothing remains. On
QE's API a rollback **MUST** carry the attribution of the charged operation, the PDP **MUST** admit that attribution
for the action rollback as it admits a debit's, and a charge outside the caller's access or with another attribution
digest **MUST** get the answer of an unknown charge. A charge **MUST** be deleted, and unknown to every operation, when the
replay window has passed since its creation and since the last return or rollback applied to it, and no allocation
entry of it holds an amount on an active Quota.

**Implements**:
- `cpt-cf-quota-enforcement-flow-return`
- `cpt-cf-quota-enforcement-flow-rollback`
- `cpt-cf-quota-enforcement-algo-unwind`

**Constraints**: `cpt-cf-quota-enforcement-constraint-security-context`

**Touches**:
- API: return through the resolver plugin; `POST /quota-enforcement/v1/operations/rollback`
- DB: `cpt-cf-quota-enforcement-db-schema` (charge ledger)
- Entities: `Charge`, `OperationLog`

### Retention Sweeper

- [x] `p1` - **ID**: `cpt-cf-quota-enforcement-dod-retention-sweeper`

The system **MUST** deliver `RetentionSweeper` (`cpt-cf-quota-enforcement-component-retention-sweeper`) as a
single-leader background task under the `retention-sweeper` cluster election (`SingletonScope::RetentionSweeper`)
through the foundation coordination adapter, invoking `reclaim_expired_idempotency` per the
`idempotency_retention_config` windows (default 24 h) and `reclaim_operation_log` under the 30-day default, and
removing deleted charges (ADR-0011), with operator-configurable batch size and frequency.
Sweeper liveness **MUST NOT** affect counter correctness. The sweeper **MUST** run as a lifecycle-managed background
task per the ToolKit lifecycle model: it receives a child `CancellationToken`, its reclamation loop is
cancellation-aware, and on graceful shutdown it stops starting new batches while the adapter resigns the election
(ADR-0006). Counter-partition retention stays with the storage plugin's partition reclamation, and policy-version
reclamation is not delivered here (see section 7).

**Implements**:
- `cpt-cf-quota-enforcement-algo-retention-sweep`

**Constraints**: `cpt-cf-quota-enforcement-constraint-toolkit`

**Touches**:
- API: `SingletonCoordinator` port (`SingletonScope::RetentionSweeper`)
- DB: `cpt-cf-quota-enforcement-db-schema` (`idempotency_records`, `operation_log`, `idempotency_retention_config`,
  charge ledger)
- Entities: `IdempotencyRecord`, `OperationLog`, `Charge`

### Hot-Path NFR Verification

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-dod-nfr-verification`

The system **MUST** verify the seven NFRs this feature owns through the following hooks, adding no promise beyond the
PRD targets. Evaluation latency (`cpt-cf-quota-enforcement-nfr-evaluation-latency`) and throughput
(`cpt-cf-quota-enforcement-nfr-throughput`): the Criterion benchmark suite in `quota-enforcement/benches/` covering
single-Quota debit at 10 000 ops/s, with CI gating p95 <= 100 ms before merge. Subject scale
(`cpt-cf-quota-enforcement-nfr-subject-scale`): the 100 M-subject load benchmark holding the latency threshold. Quota
density (`cpt-cf-quota-enforcement-nfr-quota-density`): the 10-Quota cascade benchmark at 5 000 ops/s holding the
latency threshold. Availability (`cpt-cf-quota-enforcement-nfr-availability`): the end-to-end 99.95% monthly
evaluation-endpoint criterion, verified by the chaos failover drill whose gateway-failover input the foundation
feature provides. Fault tolerance (`cpt-cf-quota-enforcement-nfr-fault-tolerance`): the end-to-end RPO = 0 drill
(storage-backend restart after `Allowed`/`Denied` was returned loses no committed mutation), consuming the foundation
durable-commit input. Idempotency guarantee (`cpt-cf-quota-enforcement-nfr-idempotency-guarantee`): the simulated
retry storm at 10x normal RPS with a 5% retry rate showing zero double-count events.

**Implements**:
- `cpt-cf-quota-enforcement-algo-evaluation-pipeline`
- `cpt-cf-quota-enforcement-algo-idempotency-replay`

**Constraints**: `cpt-cf-quota-enforcement-constraint-bounded-cardinality`

**Touches**:
- API: `quota-enforcement/benches/` Criterion suite; chaos and durability drills
- Entities: benchmark and drill fixtures (no new domain entities)

## 6. Acceptance Criteria

- [ ] An `Allowed` debit applies exactly the Engine's Debit Plan: every named Quota moves by its `entry.amount`,
  unnamed Quotas never move, and the caller never observes a partially applied plan; `Denied` and every canonical
  error leave all counters unchanged (concurrency integration test)
- [ ] A debit or credit with `amount <= 0` fails with `INVALID_AMOUNT` before idempotency lookup (and, for debit,
  subject resolution): no idempotency record, no operation-log entry, no counter change
- [ ] An exact replay of a committed debit returns the original Decision verbatim (result, `debit_plan`,
  `diagnostics`, original identifiers and amounts) without re-invoking the Engine and without a second counter
  effect; a replay under a time-gated `cel` Policy at a different wall-clock time still returns the original verdict
- [ ] A replay with a divergent payload under the same scope fails with `IDEMPOTENCY_PAYLOAD_MISMATCH` (409) and
  leaves the original record untouched; the same key string used by a different tenant or operation type creates an
  independent record; the same key with other subjects fails with `IDEMPOTENCY_PAYLOAD_MISMATCH`
- [ ] A replay after the replay window (default 24 h, honoring a per-`(tenant, metric)` override) is re-evaluated
  as a new operation, except a replay of an allowed debit whose charge still lives, which gets the first result
- [ ] Credit rejection arms fire before any mutation, in-tx with the row lock: unknown `quota_id` gives 404
  `UNKNOWN_QUOTA`, a Quota outside the caller's scope gives the same 404, a Quota in scope but of another tenant gives 403, a deactivated Quota gives 400 `QUOTA_DEACTIVATED`, and a
  consumption Quota at `time >= period_end` gives 400 `PERIOD_CLOSED` even while the settlement window is still
  draining; a successful credit floors the counter at zero and enqueues `quota-counter-adjusted` with the credited
  amount, `quota_id`, and authenticated service principal in the same transaction
- [ ] Rollback returns the remainder of the charge it names; an unknown or deleted charge, a credit, a charge
  outside the caller's access, and a charge made under another attribution give `UNKNOWN_OPERATION`; a rollback whose
  attribution the PDP does not allow is rejected before any storage access; a rollback after a partial return returns only the remainder;
  a second rollback succeeds and changes nothing; a lease-commit charge rolls back via the commit call's idempotency
  key; a successful rollback that returns an amount enqueues `quota-rollback-applied` same-tx
- [ ] Partial and repeated returns unwind each chain last entry first; a repeated return key with the same charge
  and amount changes nothing, also after the replay window while the charge lives, and with another charge or amount fails with
  `IDEMPOTENCY_PAYLOAD_MISMATCH`; a
  return on a deactivated allocation or consumption Quota succeeds; a return after a credit takes a counter to zero,
  not below; a return on a charge with an allocation and a rate Quota
  decreases only the held amount; a return above what remains, of an unknown or deleted charge, or one that would
  change no counter (rate Quotas only, a settled consumption period, a consumption charge past its return window)
  fails and changes nothing
- [ ] A charge is unknown to every operation once the replay window has passed since its creation and since the last
  return or rollback applied to it, and no allocation entry of it holds an amount on an active Quota, before and after
  the sweeper removes it; a debit or commit under its key then replaces it; an allocation charge that holds an amount
  on an active Quota stays returnable, and one whose allocation Quotas are all deactivated does not
- [ ] Preview runs the full pipeline against current counter state and persists nothing: no counter mutation, no held
  capacity, no idempotency record, no operation-log entry, no outbox event; the response carries the full Decision
  shape plus `preview: true` and Policy attribution in diagnostics; preview under a denied or unreachable PDP fails
  exactly like the corresponding write
- [ ] Decision-shaped fields injected into any request body are silently ignored on both fresh and replay paths
  (adversarial integration test of the PRD §3.4 trust boundary)
- [ ] The first mutating operation after a period boundary materializes the new consumption row with `consumed = 0` and
  `highest_crossed_threshold_pct = NULL`; a debit committing at exactly `boundary_at` lands in the new period; the
  `period-rollover` event carries the closing consumed amount, closing cap, and new boundary, and is observed
  strictly after the last commit attributed to the closing period
- [ ] A debit or a preview against a Quota on a `Direct`-classified metric fails with `METRIC_NOT_QUOTA_GATED` and
  persists nothing; a credit, a rollback, and a return against such a metric run
- [ ] One `RetentionSweeper` leader runs at a time in steady state; killing the leader makes a survivor the leader
  of `SingletonScope::RetentionSweeper` within one election TTL plus observation lag; expired idempotency and
  operation-log rows are physically reclaimed per their configured windows, and sweeper downtime changes no counter
  outcome
- [ ] Two retention sweep bodies forced to run at once (advisory overlap) complete without error, and each expired
  row is deleted exactly once
- [ ] The Criterion CI gate holds p95 <= 100 ms for single-Quota debit at 10 000 ops/s sustained; the 100 M-subject
  and 10-Quota-density benchmarks hold the same latency threshold
- [ ] The retry storm at 10x normal RPS with 5% retry rate produces zero double-count events; the RPO = 0 drill loses
  no mutation acknowledged with `Allowed` or `Denied` across a storage-backend restart; the chaos failover drill
  sustains the 99.95% evaluation-endpoint availability criterion with requests failing over to surviving replicas
- [ ] Metrics scrape shows `denial_total` labeled only by the closed `reason` enum, with no `tenant_id`,
  `subject_id`, `quota_id`, `idempotency_key`, metric, subject-type, or caller label on any instrument this
  feature touches

## 7. Additional Context (optional)

- **ADR dependencies**: ADR-0002 (acquisition ordering), ADR-0003 (metadata snapshot timing), ADR-0004
  (settlement window emit), ADR-0008 (subjects), ADR-0010 (chains), ADR-0011 (charge ledger), ADR-0012 (metric
  catalogue), and ADR-0013 (metric types) are load-bearing for this pipeline. The projection-contracts feature owns the ingress and subject admission this pipeline consumes.
- **Charge storage**: the charge ledger holds about one replay window of charges, the same order as idempotency
  records, plus the allocation charges that still hold an amount; DESIGN §4 states its capacity.
- **Boundary with resolution-policy-engine**: Policy selection, the Engine contract, the per-Policy timeout, and the
  Debit-Plan invariant boundary are defined there
  (`cpt-cf-quota-enforcement-algo-engine-boundary`); this feature owns the pipeline that invokes them and the atomic
  application of the resulting plan. The evaluation-latency and throughput budgets that motivated the cached
  `ValidatedConfig` and the sync no-I/O Engine contract are verified by this feature's benchmarks.
- **Upstream alignment items (tracked upstream prerequisites)**: DESIGN's `RetentionSweeper` declares no policy-version reclamation primitive although PRD
  §5.9 assigns that sweep to storage retention; the gap is already tracked by the resolution-policy-engine feature
  and no such primitive is promised here. The hot-path source of the `Direct`/`QuotaGated` classification is the
  `enforcement` trait in the metric's entry in `MetricCatalog`, the catalogue of the projection-contracts feature
  (ADR-0012); the service raises `METRIC_NOT_QUOTA_GATED` from that entry, and the storage plugin does not decide the
  mode.
- **Known P1 limitation**: silent Quotas emit their closing `period-rollover` only on the next operation; the P2
  active rollover scheduler (DESIGN §4.3) covers them without changing this feature's contract.
- **Rust contract notes**: the pipeline is async over the Tokio-based storage plugin while the Engine call is sync
  and I/O-free; the orchestrator holds no in-process lock across an await point and delegates cross-instance
  synchronization to storage serialization (I9), so instances are freely replicated. `IdempotencyRecord` and the
  cached Decision blobs cross task boundaries and are `Send + Sync` compatible plain data.
- **Rollout / rollback**: all components here are stateless above the storage plugin except the sweeper leadership,
  which resigns its election on shutdown and hands over within one round trip; rollout is a rolling update under the
  same schema major version. Counter rows, idempotency records, and the operation log are forward-compatible via the
  schema-versioned `decision_blob` and additive table evolution.
- **Test layering**: pipeline ordering, `INVALID_AMOUNT` fail-fast, counter floors, period attribution, and the
  state machines get unit tests; replay, payload mismatch, credit/rollback rejection arms, and the trust boundary
  get integration and adversarial tests against the storage plugin; atomicity, the retry storm, the RPO drill, and
  the availability chaos test are the drills named in section 6, not unit tests.
- **Cap guard consumption arm (tracked here)**: the reference plugin's `update_quota` evaluates the I6 cap guard on the
  merged row under the Quota's row lock; the allocation arm reads `qe_quota_allocation_counters.in_flight` today, and
  the consumption arm — the active period's consumed amount from the `quota_consumption_counters` row this feature
  materialises — lands here through the single seam the plugin leaves for it. Until then a consumption Quota's cap
  reduction is guarded by the gear's pre-check only.
- **Non-applicable review domains**: UX/accessibility is not applicable; there is no user-facing surface. Data
  protection inherits the Platform Operational Data rules from PRD §6.2; idempotency records and the operation log
  follow their configured retention windows with no additional feature-specific requirement.
