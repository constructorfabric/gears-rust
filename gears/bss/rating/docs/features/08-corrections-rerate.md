# Feature: Corrections, Replay and Administrative Re-rate

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-featstatus-corrections-rerate-implemented`

- [ ] `p1` - `cpt-cf-bss-rating-feature-corrections-rerate`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Request an administrative re-rate](#21-request-an-administrative-re-rate)
  - [2.2 Inspect or cancel a re-rate run](#22-inspect-or-cancel-a-re-rate-run)
  - [2.3 Correct a delivered result](#23-correct-a-delivered-result)
  - [Migrated namespace flows](#migrated-namespace-flows)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Propagate an input change](#31-propagate-an-input-change)
  - [3.2 Enumerate and drive a re-rate run](#32-enumerate-and-drive-a-re-rate-run)
  - [3.3 Replay a recorded revision](#33-replay-a-recorded-revision)
- [4. States (CDSL)](#4-states-cdsl)
  - [4.1 Re-rate run](#41-re-rate-run)
  - [4.2 Re-rate target row](#42-re-rate-target-row)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Corrections reprice with the binding and engine of record](#51-corrections-reprice-with-the-binding-and-engine-of-record)
  - [5.2 Durable administrative re-rate](#52-durable-administrative-re-rate)
  - [5.3 Deterministic replay](#53-deterministic-replay)
- [6. Acceptance Criteria](#6-acceptance-criteria)
  - [Corrections: Acceptance Vectors](#corrections-acceptance-vectors)
- [7. Detailed Behavior Contracts](#7-detailed-behavior-contracts)
  - [Corrections: Interactions and Sequences](#corrections-interactions-and-sequences)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Turn every input change into a re-evaluation with the prices and engine a window was finalized
with, replay any recorded revision to prove determinism, and run administrative re-rates — the only
sanctioned way to change already-final amounts — as durable runs with a frozen target per run and
per child.

### 1.2 Purpose

A late or withdrawn record, a re-attribution, a new fact version or a plan change must reprice
exactly what changed, and must never silently pick up a newer binding or engine. Approved prices are
immutable and cannot start in the past (pricing `WINDOW_START_IN_PAST`), so a "corrective publish"
no longer exists: a price change reaches a subscription only through its pins and a new fact version
(SUB-D-29). A re-bind with the fact's current pins or a new engine reaches final amounts only through
an approved re-rate whose target cannot drift, be lost by queue coalescing or crash, or be reverted
by a later correction (T-D-21, T-D-24, T-D-42, T-D-63, T-D-73). Rating emits complete new revisions and never a delta; Billing derives the
monetary difference (T-D-50).

**Requirements** (primary, per DECOMPOSITION): `cpt-cf-bss-rating-fr-idempotency`, `cpt-cf-bss-rating-fr-separation`, `cpt-cf-bss-rating-fr-posted-period-protection`, `cpt-cf-bss-rating-fr-late-arriving-usage-reresolve`, `cpt-cf-bss-rating-fr-usage-corrections`. **Supporting**: `cpt-cf-bss-rating-fr-single-outcome-determinism`.

**Principles**: `cpt-cf-bss-rating-principle-absolute-results`, `cpt-cf-bss-rating-principle-pinned-replay-rtr`, `cpt-cf-bss-rating-principle-same-math-rtr`, `cpt-cf-bss-rating-principle-delta-only-rtr`, `cpt-cf-bss-rating-principle-append-only-money`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-bss-rating-actor-platform-operator` | Requests, inspects and cancels administrative re-rates (`rerate × execute` / `read`, audited) |
| `cpt-cf-bss-rating-actor-rating` | Propagates input changes, enumerates runs and re-evaluates children under its service identity |
| `cpt-cf-bss-rating-actor-subscriptions` | Produces the fact versions and plan changes (`changeEffectiveAt`, `changeMode`) whose effect is re-evaluated |
| `cpt-cf-bss-rating-actor-billing` | Receives each new complete revision and alone derives credit/debit notes (PROPOSED; no Billing gear) |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §6.10 (posted-period protection, late usage, corrections), §15.
- **Architecture**: [DESIGN.md](../DESIGN.md) §3.6 Flow B (usage invalidation / correction), Flow D (pricing change: re-bind through pins, T-D-73), Flow E (plan change), Flow F (commitment cascade, dormant), administrative re-rate — [§3.6](../DESIGN.md#36-interactions--sequences); [§3.3](../DESIGN.md#33-api-contracts) (`RatingRunControlV1`, REST `reratings.*`); [§3.7](../DESIGN.md#37-database-schemas--tables) (`bss_rating__rerate_run`, `bss_rating__rerate_target`, `bss_rating__operation`); [§4.1](../DESIGN.md#41-versioning-and-historical-correctness) (pin and engine rule table); [§4.2](../DESIGN.md#42-transactions-idempotency-delivery-semantics); [§4.8](../DESIGN.md#48-multi-tenancy-and-authorization). Namespace [08](../DESIGN.md#contract-08) — [replay and binding of record](../DESIGN.md#contract-08-4-1), [correction keys](../DESIGN.md#contract-08-4-2), [posted periods](../DESIGN.md#contract-08-4-3), [reversal math](../DESIGN.md#contract-08-4-4). Complete runtime procedures are in [§7](#7-detailed-behavior-contracts).
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.8.
- **Dependencies**: [Child Evaluation and Finalization](07-child-evaluation.md) (the rater honours the requested target and closes met target rows); [Parent Roll-up and Billing Delivery](09-rollup-delivery.md) (a correction is accepted when it produces a new parent revision).
- **Consumers**: [Operations](11-operations.md) runs the replay determinism sample and observes run metrics.
- **Upstream**: none beyond those of its dependencies (DECOMPOSITION §3.1). The commitment cascade waits on `cpt-cf-bss-rating-upreq-contracts-inputs` (R-11).

**UI applicability**: none. The re-rate routes are API only and follow the REST table in [DESIGN §3.3](../DESIGN.md#33-api-contracts).

## 2. Actor Flows (CDSL)

**Use cases**: none in the PRD; re-rate is an operator flow, corrections are system flows.

### 2.1 Request an administrative re-rate

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-rerate-request`

**Actor**: `cpt-cf-bss-rating-actor-platform-operator`

**Success Scenarios**: `202 Accepted` with `run_id` and the frozen target; a retry with the same key returns the original receipt.

**Error Scenarios**: `403` (no `rerate × execute`); selector outside the caller's scope; `409 AlreadyExists` (same key, different body); `429` when the tenant already has `max_active_rerate_runs` runs active.

**Steps**:
1. [ ] - `p1` - Receive `POST /bss-rating/v1/reratings` with a selector, a reason and a required `Idempotency-Key` (or `RatingRunControlV1::request_rerate`) - `inst-rr-receive`
2. [ ] - `p1` - Authorize `rerate × execute`; the selector must lie inside the PDP-granted scope; audit the request - `inst-rr-authorize`
3. [ ] - `p1` - **IF** the key is known for `(tenant_id, actor)`, return the stored receipt; a different body is `409` - `inst-rr-replay`
4. [ ] - `p1` - In one transaction, record the operation key and insert the run with its target frozen at acceptance: `target_engine_generation`, `rebind` (re-resolve with each fact's current pins) and `rebind_as_of` - `inst-rr-accept`
5. [ ] - `p1` - **RETURN** `202` with `Location: /bss-rating/v1/reratings/{runId}`; enumeration proceeds asynchronously (§3.2) - `inst-rr-return`

### 2.2 Inspect or cancel a re-rate run

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-rerate-inspect-cancel`

**Actor**: `cpt-cf-bss-rating-actor-platform-operator`

**Success Scenarios**: the run's state, frozen target and counts per target state are returned; a cancel stops enumeration and enqueueing.

**Error Scenarios**: `404` for a run outside the caller's scope; `409 FailedPrecondition` when cancelling a terminal run.

**Steps**:
1. [ ] - `p1` - Receive `GET /reratings/{runId}` (`rerate × read`) or `POST /reratings/{runId}:cancel` (`rerate × execute`, audited) - `inst-ri-receive`
2. [ ] - `p1` - **IF** cancel, stop enumeration and enqueueing; requested targets already written on children stay and are applied - `inst-ri-cancel`
3. [ ] - `p1` - **RETURN** `RerateRunView` - `inst-ri-return`

### 2.3 Correct a delivered result

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-rerate-correction`

**Actor**: `cpt-cf-bss-rating-actor-rating` (on an input change produced by intake, attribution, fact intake or evidence)

**Success Scenarios**: the affected child gets window revision `n + 1` at its binding and engine of record; its parent gets result revision `m + 1` naming `m`; Billing receives a complete replacement.

**Error Scenarios**: a sibling child is not current (no parent revision until it is); evaluation error (child `failed`).

**Steps**:
1. [ ] - `p1` - The transaction that changes an input bumps `input_generation` of every affected child and enqueues them (§3.1) - `inst-cr-bump`
2. [ ] - `p1` - The rater re-evaluates each child with its binding and engine of record — no new `resolve` — and the latest fact version (DESIGN §4.1); a new fact version whose pins changed is bound again at fact intake and reaches a final child only as that fact's new binding of the period (T-D-73) - `inst-cr-evaluate`
3. [ ] - `p1` - Roll-up writes the next parent revision once every expected child is current - `inst-cr-rollup`
4. [ ] - `p1` - **RETURN** without any delta; Rating's behaviour is identical whether Billing has posted or not - `inst-cr-return`

<a id="register-flows"></a>

### Migrated namespace flows

The flows of the former slice documents that this feature implements are defined here and specified in [§7](#7-detailed-behavior-contracts); each entry links to its contract.

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-open-period-correction-rtr`
  — Corrections — Interactions and Sequences ([contract](#contract-08-3-6))

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-posted-period-correction-rtr`
  — Corrections — Interactions and Sequences ([contract](#contract-08-3-6))

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-negative-usage-reversal-rtr`
  — Corrections — Interactions and Sequences ([contract](#contract-08-3-6))

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-reattribution-rtr`
  — Corrections — Interactions and Sequences ([contract](#contract-08-3-6))

## 3. Processes / Business Logic (CDSL)

### 3.1 Propagate an input change

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-rerate-propagate`

**Input**: a committed input change — counter change, invalidation, re-attribution, segment, scope, coverage, source-loss change, fact version, plan change.

**Output**: affected children re-enqueued with a raised `input_generation`.

1. [ ] - `p1` - Determine the affected children: the child of the changed counter and its window-group members; every child of a fact whose version changed; both old and new children of a re-attributed interval - `inst-pp-affected`
2. [ ] - `p1` - Under each child's row lock, increment `input_generation`; a `final` child becomes `final_stale` - `inst-pp-bump`
3. [ ] - `p1` - Enqueue the children on `rating.child_work` in the same transaction - `inst-pp-enqueue`
4. [ ] - `p1` - **IF** a plan or usage-policy change is effective inside a `CalendarHour` window and not on an hour boundary, the child fails closed `intra_window_policy_change`; Subscriptions schedules such changes at the next UTC hour (pricing D-510 E4; DESIGN Flow E) - `inst-pp-intra-hour`

### 3.2 Enumerate and drive a re-rate run

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-algo-rerate-enumerate`

**Input**: an accepted run with its frozen target and selector.

**Output**: one target row per selected child, each child's requested target raised, work enqueued.

1. [ ] - `p1` - In keyset batches resumable from `enumerated_through`, insert `bss_rating__rerate_target (run_id, child_id, state = pending)` and set the child's `requested_rebind` (true if any run on it asks for a re-bind) and raise `requested_engine` to the maximum with the run's target, in the same transaction - `inst-en-batch`
2. [ ] - `p1` - Only after a batch commits, enqueue its children as notifications at `rerate_enqueue_per_second` - `inst-en-enqueue`
3. [ ] - `p1` - Mark the run `completed` when no target row is `pending`; `failed` rows are counted, never skipped silently - `inst-en-complete`
4. [ ] - `p1` - **IF** the service restarts, re-enqueue every `pending` target row; never skip a child because it was rated after the run started - `inst-en-resume`

### 3.3 Replay a recorded revision

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-algo-rerate-replay`

**Input**: `(child_id, window_revision)`.

**Output**: equality with the stored lines and `input_digest`, or a determinism defect.

1. [ ] - `p1` - Rebuild `EvaluationInput` from the revision's stored manifest — its `bss_rating__pricing_binding` rows (optionally verified against `PricingReadV1::price(price_id).money_digest`, served forever for an approved price, pricing D-422) and its derived declaration — and evaluate under its recorded `(engine_generation, engine_digest)`; write nothing - `inst-rp-rebuild`
2. [ ] - `p1` - **IF** the generation is not compiled in, fail `engine_unavailable`; never use the current engine - `inst-rp-engine`
3. [ ] - `p1` - **IF** the result differs from the stored lines or digest, page as a determinism defect - `inst-rp-defect`

Canonical rule: [08 §4.1](../DESIGN.md#contract-08-4-1).

## 4. States (CDSL)

### 4.1 Re-rate run

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-state-rerate-run`

**States**: `accepted`, `enumerating`, `running`, `completed`, `failed`, `cancelled`.

**Initial State**: `accepted`.

**Transitions**:
1. [ ] - `p1` - **FROM** `accepted` **TO** `enumerating` **WHEN** the first enumeration batch starts - `inst-rs-enumerate`
2. [ ] - `p1` - **FROM** `enumerating` **TO** `running` **WHEN** every selected child has a target row - `inst-rs-running`
3. [ ] - `p1` - **FROM** `running` **TO** `completed` **WHEN** no target row is `pending` - `inst-rs-completed`
4. [ ] - `p1` - **FROM** `enumerating` or `running` **TO** `failed` **WHEN** enumeration cannot proceed - `inst-rs-failed`
5. [ ] - `p1` - **FROM** `accepted`, `enumerating` or `running` **TO** `cancelled` **WHEN** an operator cancels; written requested targets are still applied - `inst-rs-cancelled`

### 4.2 Re-rate target row

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-state-rerate-target`

**States**: `pending`, `done`, `failed`.

**Initial State**: `pending`.

**Transitions**:
1. [ ] - `p1` - **FROM** `pending` **TO** `done` **WHEN** a final revision meeting the target commits for the child — the engine generation reached and, for a re-bind, a binding resolved at or after `rebind_as_of` (closed by the rater) - `inst-ts-done`
2. [ ] - `p1` - **FROM** `pending` **TO** `failed` **WHEN** the child fails at the target, including a re-bind whose `currency_scale` differs from the fact's pinned scale (`currency_scale_changed`; the child keeps its binding and engine of record; T-D-80) - `inst-ts-failed`

## 5. Definitions of Done

### 5.1 Corrections reprice with the binding and engine of record

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-rerate-corrections`

The system **MUST** turn every input change into a raised `input_generation` and a re-evaluation of the affected children in the same transaction as the change, reprice a final child only with its binding and engine of record (no new `resolve`), publish each correction as a new complete revision naming its predecessor, and compute no monetary delta.

**Implements**: `cpt-cf-bss-rating-flow-rerate-correction`, `cpt-cf-bss-rating-algo-rerate-propagate`, `cpt-cf-bss-rating-flow-open-period-correction-rtr`, `cpt-cf-bss-rating-flow-posted-period-correction-rtr`, `cpt-cf-bss-rating-flow-negative-usage-reversal-rtr`, `cpt-cf-bss-rating-flow-reattribution-rtr`.

**Constraints**: `cpt-cf-bss-rating-constraint-posted-immutability`, `cpt-cf-bss-rating-constraint-posted-immutability-rtr`, `cpt-cf-bss-rating-constraint-delta-dedup-owner-rtr`.

**Touches**: `cpt-cf-bss-rating-dbtable-child-window`, `cpt-cf-bss-rating-dbtable-rated-version`, `cpt-cf-bss-rating-dbtable-fact-result`.

### 5.2 Durable administrative re-rate

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-dod-rerate-runs`

The system **MUST** accept a re-rate idempotently on `(tenant_id, actor, Idempotency-Key)`, freeze its target at acceptance, record a target row and raise the child's requested target in one transaction before enqueueing, close a target only by a final revision that meets it, resume from stored state after a crash, and apply already-written targets after a cancel.

**Implements**: `cpt-cf-bss-rating-flow-rerate-request`, `cpt-cf-bss-rating-flow-rerate-inspect-cancel`, `cpt-cf-bss-rating-algo-rerate-enumerate`, `cpt-cf-bss-rating-state-rerate-run`, `cpt-cf-bss-rating-state-rerate-target`.

**Constraints**: `cpt-cf-bss-rating-constraint-posted-immutability`.

**Touches**: API: `POST /bss-rating/v1/reratings`, `GET /bss-rating/v1/reratings/{runId}`, `POST /bss-rating/v1/reratings/{runId}:cancel`; SDK `RatingRunControlV1`; `cpt-cf-bss-rating-dbtable-rerate-run` (`bss_rating__rerate_run`, `bss_rating__rerate_target`, `bss_rating__operation`).

### 5.3 Deterministic replay

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-dod-rerate-replay`

The system **MUST** re-execute any retained revision from its stored manifest under its recorded engine generation without writing, and report any difference as a determinism defect.

**Implements**: `cpt-cf-bss-rating-algo-rerate-replay`.

**Constraints**: `cpt-cf-bss-rating-constraint-stateless-hot-path`.

**Touches**: `cpt-cf-bss-rating-dbtable-rated-version`, `cpt-cf-bss-rating-dbtable-engine-generation`.

The pricing-change flow (DESIGN Flow D) is re-cut by T-D-73: a price change reaches a subscription through its pins and a new fact version; a final child keeps its binding of record and re-binds only on a re-rate with `rebind` or on a new fact version whose pricing query (revision, period or pins) changed; any other correction calls no `resolve`. The commitment cascade (Flow F) stays dormant until a Contracts source exists (R-11).

Observability: this feature emits `rating_rerate_children_total{run, state}`, `rating_correction_queue_depth`, `rating_correction_cascade_size{trigger}` and `rating_post_final_revisions_total` (DESIGN §4.7).

## 6. Acceptance Criteria

- [ ] An invalidation of a record in a final window produces window revision `n + 1` at the child's binding of record (no `resolve` call) and a parent revision `m + 1` with `previous_result_revision = m`; Rating emits no delta.
- [ ] Two runs on one child combine into one requested target (re-bind if either asks, the higher engine generation); one final revision meeting it closes both runs' target rows.
- [ ] After a re-rate re-binds a child, a later input correction of the same child uses the new binding and engine of record, never the obsolete ones.
- [ ] A cancelled run enqueues nothing more; children whose requested target was already written are still evaluated at it.
- [ ] A selector outside the caller's scope is refused; a run of another tenant answers `404`.
- [ ] A plan change inside a `CalendarHour` window that is not on an hour boundary fails the child `intra_window_policy_change`.
- [ ] A replay of a retained revision writes nothing and equals the stored lines and `input_digest`.
- [ ] A correction or replay uses the `currency` and `currency_scale` of the stored binding of record, never a value resolved later (T-D-80).

Acceptance vectors owned by this feature (DESIGN §4.13 index):

| # | Scenario | Expected state |
|---|---|---|
| V08 | An ordinary correction is queued, then a re-rate covers the same child | the child finalizes at the re-rate target; the target row is `done` |
| V10 | Re-rate resumes after a crash; a child was rated after the run started with its old binding | the child is re-enqueued, not skipped |
| V11 | Retried `POST /reratings` with the same key | same `run_id`; one run |
| V44 | `PerUnit{unit_amount: "0.002"}` EUR × 3, then corrected to × 2.5 after Billing posted | **Rating**: revision 1 exactly `3/500` (0.006), revision 2 exactly `1/200` (0.005), absolute, no delta. Billing golden (R-32): posted 0.01; corrected rounded total 0.00; credit −0.01, never `round(0.005 − 0.01) = 0` |
| V45 | V44 continued: corrected to × 4, then × 4.5 | **Rating**: revisions 3 and 4 exactly `1/125` (0.008) and `9/1000` (0.009). Billing golden: rounded 0.01 − cumulative 0.00 → debit +0.01; rounded 0.01 − cumulative 0.01 → no note, head advances |
| V46 | A fact pinned at scale 2; a later `resolve` would answer scale 3 | **Rating**: correction and replay use the stored scale (same `input_digest`, identical delivery); a provisional child, a new fact version and a re-rate with `rebind` resolving at scale 3 fail `currency_scale_changed` for that fact, which keeps its last delivered result; the next period's fact pins scale 3 under a different `invoice_line_key` |

<a id="contract-08-4-5"></a>

<!-- contract:08-retroactivity-corrections:4.5 -->
### Corrections: Acceptance Vectors

| Fixture | Required result |
|---|---|
| F14 | v1 0.470 → v2 0.423 → v3 0.376; Billing 0.47 → 0.42 → 0.38; obligations −0.05, −0.04 |
| F15 | v3 before v2 and duplicate v3: v3 accepted, v2 and the duplicate ignored; 0.421 → 0.424 advances the revision with no rounded adjustment |
| F21 | an older-input evaluation finishing late is superseded |
| F26 | whole-hour re-tiering on correction; replay produces no second credit |
| F32 | parent revisions follow child vectors (exact 0.34 → 0.33 → 0.351, posted 0.35); manifest digest |

This namespace's table also cited the state-transition vectors V08 ([08-corrections-rerate](#6-acceptance-criteria)), V09 ([07-child-evaluation](07-child-evaluation.md#6-acceptance-criteria)), V10 ([08-corrections-rerate](#6-acceptance-criteria)), V11 ([08-corrections-rerate](#6-acceptance-criteria)), V22 ([02-evaluation-core](02-evaluation-core.md#6-acceptance-criteria)); each is defined once, in the acceptance criteria of the feature that owns it.

<!-- /contract -->

## 7. Detailed Behavior Contracts

**Contract namespace 08.** Section numbers inside the detailed contracts below resolve through the [contract address index](../DECOMPOSITION.md#4-contract-address-index), not the overview section numbers above. A bare section reference names a section of the same namespace; "DESIGN §n" names §1–§5 of [DESIGN.md](../DESIGN.md).

The following sections keep the runtime procedures of these namespaces — their interactions and sequences and the procedural parts of their §4. The flows, processes and acceptance criteria above summarize them; the architecture, schemas and evaluation semantics they rely on are defined once in [DESIGN.md](../DESIGN.md) §6.

<a id="contract-08-3-6"></a>

<!-- contract:08-retroactivity-corrections:3.6 -->
### Corrections: Interactions and Sequences

**Contract**: `cpt-cf-bss-rating-flow-open-period-correction-rtr` (`p2`), defined in [§2 migrated flows](#register-flows).

**Late record before the parent is delivered**: the record increments its counter; the child is
re-evaluated provisionally (or, if already final, gets a new window revision with its binding of record);
the parent has not been delivered yet, so its first delivery already contains the record.

**Contract**: `cpt-cf-bss-rating-flow-posted-period-correction-rtr` (`p1`), defined in [§2 migrated flows](#register-flows).

**Correction after delivery** (Billing may have frozen or posted the group): the child gets window
revision `n + 1`; the parent roll-up produces `result_revision m + 1` once all expected children are
final; Billing rounds the corrected line total at its stored `currency_scale` and subtracts the
cumulative posted amount (original posting and every prior note, pending ones included), and posts
that difference unrounded as a credit/debit note (money ADR rule 8). Rating's behaviour is
identical whether or not Billing has posted, and it never reads a posted amount.

**Contract**: `cpt-cf-bss-rating-flow-negative-usage-reversal-rtr` (`p2`), defined in [§2 migrated flows](#register-flows).

**Invalidation / negative quantity** — DESIGN Flow B. Worked example (Atlas F-A3/F14, VM hours at
0.047 EUR (`PerUnit{unit_amount: "0.047"}`), `BillingCycle` window, amounts in EUR, stored as exact
major-unit fractions at `currency_scale = 2`; Billing's column applies money ADR rule 8):

```text
Nov 03  October final: 10 VM·h → child r1 0.470 → parent rev 1 = 0.470 → Billing invoice 0.47
Nov 10  invalidation of U-10 (VM stopped 18:10, not 19:10) → Q 9 → child r2 0.423 → parent rev 2
        Billing: round(0.423) = 0.42 − cumulative 0.47 → credit −0.05 (pending posting)
Nov 12  invalidation of U-9 → Q 8 → child r3 0.376 → parent rev 3
        Billing: round(0.376) = 0.38 − cumulative 0.42 (0.47 − 0.05, pending credit included) → credit −0.04
        final receivable 0.38, never 0.33
```

Worked example (Atlas F26, hourly volume): an issued hour at Q = 9 → 0.18; corrected to Q = 10 → the
whole hour moves to the 0.015 band: 0.15; Billing credits 0.03 at the group aggregate. Pricing only
the extra unit would be wrong. Replaying the same invalidation produces no second credit (its key is
absorbed; the input digest is unchanged).

**Contract**: `cpt-cf-bss-rating-flow-reattribution-rtr` (`p2`), defined in [§2 migrated flows](#register-flows).

**Re-attribution** (a new segment version moves an interval to another line or payer): the affected
records are un-counted from the old aggregation key and counted under the new one in one transaction
([12 §4.2](../DESIGN.md#contract-12-4-2)); both children are re-evaluated; both parents get new revisions. Corrections to a
prior payer stay in that payer's billing group (Atlas D08).

<!-- /contract -->
