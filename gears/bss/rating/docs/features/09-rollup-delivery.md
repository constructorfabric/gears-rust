# Feature: Parent Roll-up and Billing Delivery

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-featstatus-rollup-delivery-implemented`

- [ ] `p1` - `cpt-cf-bss-rating-feature-rollup-delivery`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Pull deliveries](#21-pull-deliveries)
  - [2.2 Recover by business key and inspect](#22-recover-by-business-key-and-inspect)
  - [Migrated namespace flows](#migrated-namespace-flows)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Roll up one fact](#31-roll-up-one-fact)
  - [3.2 Sequence and publish deliveries](#32-sequence-and-publish-deliveries)
  - [3.3 Store a Billing period hint](#33-store-a-billing-period-hint)
- [4. States (CDSL)](#4-states-cdsl)
  - [4.1 Parent result head](#41-parent-result-head)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Complete, fresh, absolute parent revisions](#51-complete-fresh-absolute-parent-revisions)
  - [5.2 Recoverable delivery](#52-recoverable-delivery)
  - [5.3 Period state stays Billing's](#53-period-state-stays-billings)
- [6. Acceptance Criteria](#6-acceptance-criteria)
  - [Results: Acceptance Vectors](#results-acceptance-vectors)
- [7. Detailed Behavior Contracts](#7-detailed-behavior-contracts)
  - [Results: Interactions and Sequences](#results-interactions-and-sequences)
  - [Billing delivery and operations: Interactions and Sequences](#billing-delivery-and-operations-interactions-and-sequences)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Roll the current final child results of one parent fact version up into a complete, exact, absolute
parent result revision; record its `BillableItemDeliveryV1` in an outbox with a `feed_seq`-ordered pull
feed; serve the recovery reads Billing uses; and store Billing's period hints without ever gating on
them.

### 1.2 Purpose

Billing needs one complete replacement per parent fact revision, built only from children that are
current against their inputs and the fact's present version, and a delivery it can always recover by
pull even when no event is delivered. Rolling up under the fact lock with a freshness check and
publishing through an outbox guarantees that a stale or partial result never leaves Rating (T-D-50,
T-D-58, T-D-64). Rounding, period state, notes and posting stay with Billing (T-D-50, T-D-51).

**Requirements** (primary, per DECOMPOSITION): `cpt-cf-bss-rating-contract-billing-periodstate`. **Supporting**: `cpt-cf-bss-rating-fr-separation`, `cpt-cf-bss-rating-fr-idempotency`, `cpt-cf-bss-rating-fr-snapshot-carry`.

**Principles**: `cpt-cf-bss-rating-principle-absolute-results`, `cpt-cf-bss-rating-principle-append-only-money`, `cpt-cf-bss-rating-principle-idempotent-delivery-bhf`, `cpt-cf-bss-rating-principle-precision-out-bhf`, `cpt-cf-bss-rating-principle-immutable-charges-rob`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-bss-rating-actor-rating` | Runs roll-up and the delivery publisher under its service identity, narrowed to the fact's tenant |
| `cpt-cf-bss-rating-actor-billing` | Consumes deliveries by pull (`deliveries_since`) or event when enabled, recovers by business key, sends the observational period hint. **No Billing gear exists**: its side is PROPOSED (Atlas C08) |
| `cpt-cf-bss-rating-actor-platform-operator` | Reads runs and window results for inspection (`result × read`) |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §6.1 (separation, idempotency), §9.2 (Billing delivery and obligation contract).
- **Architecture**: [DESIGN.md](../DESIGN.md) §3.6 parent roll-up and delivery, Flow G (Billing period close, Billing-owned) — [§3.6](../DESIGN.md#36-interactions--sequences); [§3.3](../DESIGN.md#33-api-contracts) (`RatingRunReadV1`, `BillableItemDeliveryV1`, V1 compatibility, the delivery event contract, REST `runs.get`); [§3.7](../DESIGN.md#37-database-schemas--tables) (`bss_rating__fact_head`, `bss_rating__fact_result`, `bss_rating__delivery`, `bss_rating__delivery_feed`, `bss_rating__billing_hint`); [§4.1](../DESIGN.md#41-versioning-and-historical-correctness) (composite snapshot per parent line); [§4.2](../DESIGN.md#42-transactions-idempotency-delivery-semantics); [§4.5](../DESIGN.md#45-concurrency); [§4.8](../DESIGN.md#48-multi-tenancy-and-authorization). Namespaces: [15 §4.2](../DESIGN.md#contract-15-4-2) (result revisions), [15 §4.3](../DESIGN.md#contract-15-4-3) (line identities), [16 §4.1](../DESIGN.md#contract-16-4-1) (delivery contract), [16 §4.2](../DESIGN.md#contract-16-4-2) (period state is Billing's), [11 §4.6](../DESIGN.md#contract-11-4-6), [11 §4.7](../DESIGN.md#contract-11-4-7). Complete runtime procedures are in [§7](#7-detailed-behavior-contracts).
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.9.
- **Dependencies**: [Foundation](01-foundation.md) (`rating.rollup` and `rating.delivery` queues, tenancy, event scaffolding); [Child Evaluation and Finalization](07-child-evaluation.md) (current final child results, snapshots).
- **Consumers**: [Corrections, Replay and Administrative Re-rate](08-corrections-rerate.md) relies on new parent revisions; [Operations](11-operations.md) checks delivery coverage.
- **Upstream**: `cpt-cf-bss-rating-upreq-billing-delivery-consumer`, `cpt-cf-bss-rating-upreq-billing-period-ownership`, `cpt-cf-bss-rating-upreq-billing-ledger-item-granularity`, `cpt-cf-bss-rating-upreq-event-contracts` ([UPSTREAM_REQS](../UPSTREAM_REQS.md) §2.7, §2.12).

**UI applicability**: none. `GET /bss-rating/v1/runs/{runId}` is API only and follows the REST table in [DESIGN §3.3](../DESIGN.md#33-api-contracts).

## 2. Actor Flows (CDSL)

**Use cases**: none in the PRD; Billing's consumption is a system flow.

### 2.1 Pull deliveries

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-delivery-pull`

**Actor**: `cpt-cf-bss-rating-actor-billing` (service identity; PROPOSED consumer)

**Success Scenarios**: a page of committed deliveries after `after_seq`, in `feed_seq` order per tenant partition (the outbox claim order, not necessarily commit order), never skipping a later-committed lower sequence.

**Error Scenarios**: `PermissionDenied` when `tenant_id` is outside the caller's scope; `InvalidArgument` when `limit > 500`.

**Steps**:
1. [ ] - `p1` - Receive `RatingRunReadV1::deliveries_since(ctx, tenant_id, after_seq?, limit)` - `inst-dp-receive`
2. [ ] - `p1` - Authorize `result × read` on `tenant_id`; a tenant outside the PDP scope is refused - `inst-dp-authorize`
3. [ ] - `p1` - Scan the tenant's `bss_rating__delivery_feed` partition after `after_seq`, filtered by tenant, up to `limit ≤ 500` - `inst-dp-scan`
4. [ ] - `p1` - **RETURN** the page of `BillableItemDeliveryV1` payloads with their sequences - `inst-dp-return`

### 2.2 Recover by business key and inspect

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-delivery-recover`

**Actor**: `cpt-cf-bss-rating-actor-billing`, `cpt-cf-bss-rating-actor-platform-operator`

**Success Scenarios**: `find_runs` returns the latest complete result per fact or its pending reasons; `get_run` returns a parent result with its manifest and snapshots resolved in the same tenant; `window_results` returns child results for inspection.

**Error Scenarios**: `404` for a run, fact or snapshot outside the caller's scope (indistinguishable from absent).

**Steps**:
1. [ ] - `p1` - Receive `find_runs(subscription_id, billing_group_id, cursor?)`, `get_run(run_id)` (also `GET /runs/{runId}`) or `window_results(fact_id, cursor?)` - `inst-dr-receive`
2. [ ] - `p1` - Authorize `result × read`; identifiers are selectors inside the PDP scope - `inst-dr-authorize`
3. [ ] - `p1` - **RETURN** the view; `window_results` is inspection, never a delivery - `inst-dr-return`

<a id="register-flows"></a>

### Migrated namespace flows

The flows of the former slice documents that this feature implements are defined here and specified in [§7](#7-detailed-behavior-contracts); each entry links to its contract.

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-persist-delta-rob`
  — Results — Interactions and Sequences ([contract](#contract-15-3-6-f09))

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-deliver-bhf`
  — Billing delivery and operations — Interactions and Sequences ([contract](#contract-16-3-6))

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-periodstate-relay-bhf`
  — Billing delivery and operations — Interactions and Sequences ([contract](#contract-16-3-6))

## 3. Processes / Business Logic (CDSL)

### 3.1 Roll up one fact

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-delivery-rollup`

**Input**: a `fact_id` from `rating.rollup` (child final, fact version, `rollup_due_at`, reconciliation).

**Output**: a new parent revision and delivery row, or no delivery with the reason recorded.

1. [ ] - `p1` - BEGIN; lock the fact head; take the current fact version and its expected children; hold `FOR SHARE` on them - `inst-ru-lock`
2. [ ] - `p1` - **IF** an expected child is missing, not final, `final_stale`, or computed for an older fact version, re-enqueue it and commit with no delivery - `inst-ru-fresh`
3. [ ] - `p1` - **IF** the fact is usage or arrears and its served period has not ended, set `rollup_due_at = served_to` and commit with no delivery - `inst-ru-wait`
4. [ ] - `p1` - **IF** the manifest digest (fact version, expected-child manifest, child vector with covered generations) equals the last one, commit as a no-op - `inst-ru-noop`
5. [ ] - `p1` - Sum the exact child lines per `line_key` with no rounding, only amounts of one `currency` and `currency_scale` (else the fact fails `binding_currency_mismatch`, never converted; T-D-80); then apply the minimum-fee floor per `price_id` of the fact: billed = `max(Σ exact amounts of every slice and dimension value rated by that price, minimum_fee × covered_fraction)`, the difference written as a separate exact `min_fee_topup` line with the price's lineage — only for `BillingCycle` + `subscription_line` bindings (T-D-78, pricing D-388, D-503, D-504); build the window manifest and composite snapshots per parent line - `inst-ru-sum`
6. [ ] - `p1` - Insert `bss_rating__fact_result` revision `head + 1` and its `bss_rating__delivery` row; advance the head and clear `rollup_due_at`; enqueue the delivery; COMMIT - `inst-ru-insert`

The transaction is canonical in DESIGN §3.6 parent roll-up; identities in [15 §4.3](../DESIGN.md#contract-15-4-3).

### 3.2 Sequence and publish deliveries

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-delivery-publish`

**Input**: committed rows on the `rating.delivery` outbox queue.

**Output**: a `bss_rating__delivery_feed` position per delivery; an event when the event path is enabled.

1. [ ] - `p1` - Write the feed row with `feed_seq` = the outbox message's per-partition `seq` (partition `hash(tenant_id) mod n`); the sequencer numbers only committed rows under the partition lock and the processor delivers in `seq` order, so no row becomes visible before a lower one (DESIGN §3.7); the insert is idempotent on `(partition, feed_seq)` and `delivery_id` - `inst-pb-sequence`
2. [ ] - `p1` - **IF** the delivery event path is enabled by configuration, publish `gts.cf.bss.rating.billable_item_delivery.v1~` keyed by `fact_id` and record `published_via_event_at`; otherwise publish nothing - `inst-pb-event`
3. [ ] - `p1` - On a broker failure, retry from the outbox; the pull feed stays available - `inst-pb-retry`

The event path is disabled until Rating and the consumer both adopt the event contract (T-D-71, `cpt-cf-bss-rating-upreq-event-contracts`); pull reads stay authoritative.

### 3.3 Store a Billing period hint

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-algo-delivery-hint`

**Input**: `BillingPeriodStateChanged{billing_group_id, state, state_version}` through the inbox (TARGET; no producer exists).

**Output**: a `bss_rating__billing_hint` row.

1. [ ] - `p1` - Insert the hint keyed `(tenant_id, billing_group_id, state_version)`; ignore unknown values - `inst-hn-store`
2. [ ] - `p1` - Never gate evaluation, finalization, roll-up or delivery on it; it labels `rating_post_final_revisions_total{group_state}` only - `inst-hn-observe`

## 4. States (CDSL)

### 4.1 Parent result head

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-state-delivery-fact-head`

**States**: `awaiting_children`, `awaiting_period_end`, `delivered(result_revision)` (derived from `bss_rating__fact_head`: `result_revision`, `last_manifest_digest`, `rollup_due_at`).

**Initial State**: `awaiting_children`.

**Transitions**:
1. [ ] - `p1` - **FROM** `awaiting_children` **TO** `awaiting_period_end` **WHEN** every expected child is current but the usage or arrears period has not ended - `inst-fh-wait`
2. [ ] - `p1` - **FROM** `awaiting_children` or `awaiting_period_end` **TO** `delivered(m + 1)` **WHEN** roll-up commits a changed manifest - `inst-fh-deliver`
3. [ ] - `p1` - **FROM** `delivered(m)` **TO** `awaiting_children` **WHEN** a child becomes stale or the fact version changes - `inst-fh-reopen`

## 5. Definitions of Done

### 5.1 Complete, fresh, absolute parent revisions

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-delivery-rollup`

The system **MUST** roll up one fact per transaction under the fact-head lock with shared locks on its expected children, write a parent revision only when every expected child is current for the present fact version and the served period has ended where required, write it only when the manifest changed, sum exact amounts without rounding, apply the minimum-fee floor per price with a separate `min_fee_topup` line (T-D-78), and deliver a finalized zero fact as `lines = [], zero_result = true` (or with only a `min_fee_topup` line when a floor applies).

**Implements**: `cpt-cf-bss-rating-algo-delivery-rollup`, `cpt-cf-bss-rating-state-delivery-fact-head`, `cpt-cf-bss-rating-flow-persist-delta-rob`.

**Constraints**: `cpt-cf-bss-rating-constraint-exact-no-rounding`, `cpt-cf-bss-rating-constraint-complete-or-nothing-rob`, `cpt-cf-bss-rating-constraint-posted-immutability`.

**Touches**: `rating.rollup`; `cpt-cf-bss-rating-dbtable-fact`, `cpt-cf-bss-rating-dbtable-fact-result`.

### 5.2 Recoverable delivery

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-delivery-feed`

The system **MUST** record each delivery insert-only in the roll-up transaction, assign its feed position after commit without gaps visible to a reader, serve `deliveries_since`, `find_runs`, `get_run` and `window_results` under the operation matrix, and keep the delivery event disabled until both sides adopt its contract.

**Implements**: `cpt-cf-bss-rating-flow-delivery-pull`, `cpt-cf-bss-rating-flow-delivery-recover`, `cpt-cf-bss-rating-algo-delivery-publish`.

**Constraints**: `cpt-cf-bss-rating-constraint-no-round-bhf`, `cpt-cf-bss-rating-constraint-in-process-contracts`.

**Touches**: SDK `RatingRunReadV1`; API `GET /bss-rating/v1/runs/{runId}`; `cpt-cf-bss-rating-dbtable-charge-feed` (`bss_rating__delivery`, `bss_rating__delivery_feed`).

### 5.3 Period state stays Billing's

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-dod-delivery-hint`

The system **MUST** store Billing period hints observationally and never read period state as a rating input, a finalization condition or a delivery gate.

**Implements**: `cpt-cf-bss-rating-algo-delivery-hint`.

**Constraints**: `cpt-cf-bss-rating-constraint-periodstate-billing-bhf`.

**Touches**: `bss_rating__billing_hint` (within `cpt-cf-bss-rating-dbtable-rerate-run`).

Observability: this feature emits `rating_delivery_lag_seconds`, `rating_rollup_superseded_total`, `rating_parent_incomplete`, `rating_post_final_revisions_total{group_state}` and the `rating.rollup`, `rating.delivery.publish` spans (DESIGN §4.7).

The migrated flows `cpt-cf-bss-rating-flow-deliver-bhf` and `cpt-cf-bss-rating-flow-periodstate-relay-bhf` describe Billing's side of the contract (**PROPOSED — NOT YET IMPLEMENTED**, Atlas C08; no Billing gear exists). Rating implements only what they consume — the pull feed and `find_runs` — and keeps them as contract references, not Rating work.

## 6. Acceptance Criteria

- [ ] A fact whose expected child is missing or not current produces no parent revision; the child is re-enqueued.
- [ ] **V34** (R-16-gated: dimension values need the dimension encoding; launch rates the empty dimension only) Two dimension values bound to one default price with `minimum_fee` 30, rated 10 + 10, deliver 20 of usage lines plus a `min_fee_topup` of 10 (total 30); two own prices with `minimum_fee` 30 each deliver 60; a fact covering half its billing period applies `minimum_fee × 1/2` (pricing D-388, D-415; T-D-77, T-D-78).
- [ ] A usage or arrears fact whose children are final before `served_to` delivers at `served_to` through `rollup_due_at`, without a further upstream event.
- [ ] A roll-up on an unchanged manifest commits nothing new.
- [ ] Revision 1 carries no `previous_result_revision`; every later revision names `result_revision − 1`.
- [ ] `deliveries_since` never returns a lower sequence after a higher one for the same partition, and a later-committed lower sequence is not skipped.
- [ ] With the event path disabled, no event is published and `deliveries_since` returns every delivery.
- [ ] A Billing period hint changes no child, parent or delivery.

Acceptance vectors owned by this feature (DESIGN §4.13 index):

| # | Scenario | Expected state |
|---|---|---|
| V12 | A fact-version-only change | new parent revision with unchanged amounts |
| V13 | Child 1 corrected (generation bumped) while child 2 finalizes | roll-up publishes nothing until child 1 is fresh |
| V16 | Billing receives revision 3 after revision 4 | revision 3 ignored; equal revision with a different digest quarantined |
| V36 | Two hourly contributions of `1/200` EUR under one `invoice_line_key`; the same under two keys (different `gl_code`) | **Rating**: one parent line of exactly `1/100`; or two lines of `1/200`, each with all key components as fields. Billing golden (R-32): 0.01; 0.00 and 0.00 |
| V37 | Line totals 0.005, 0.015, 0.025, −0.005, −0.015, −0.025 EUR | **Rating**: positive values delivered unrounded (`1/200`, `3/200`, `1/40`); no Rating line is negative after the step-6 guard. Billing golden: 0.00, 0.02, 0.02, 0, −0.02, −0.02 (HALF_EVEN) |
| V49 | (R-16-gated) Two dimension values of one price rated `1001/200` EUR (5.005) each, `minimum_fee` 30.00 | **Rating**: two rated lines and a `min_fee_topup` of exactly `1999/100` (19.99) with the empty dimension value, `price_id` in every provenance entry and the exact floor, rated sum and covered fraction in the top-up's provenance. Billing golden (R-32): the price posts 30.00, never 5.00 + 5.00 + 19.99 = 29.99 |
| V43 | Usage rated `2001/200` EUR (10.005) on a price with `minimum_fee` 30.00, full period | **Rating**: a `min_fee_topup` line of exactly `3999/200` (19.995) with the floor in provenance. Billing golden: 10.00 + 20.00 as separate lines (the proposed key), 30.00 as one line; a discount reference fails `coupon_source_unavailable` |

<a id="contract-15-4-6"></a>

<!-- contract:15-rated-output-balance-effects:4.6 -->
### Results: Acceptance Vectors

| Fixture | Given | Required result |
|---|---|---|
| F02 | 0.047 EUR/VM·h, 10 h | exact 0.470 EUR = `47/100`, scale 2; Billing 0.47 |
| F03 | 2.5 VM·h | exact 0.1175 EUR = `47/400`; Billing 0.12 |
| F19 | 1 EUR prorated 1/3 | exact `1/3` EUR; Billing 0.33 |
| F21 | older-input run finishes after a newer generation | superseded; cannot publish a higher revision |
| F27 | two hourly 0.005 contributions, one `invoice_line_key` | parent exact `1/100` EUR → Billing 0.01 (two lines would post 0.00 each, V36) |
| F32 | corrections to different hours | exact revisions 0.34 → 0.33 → 0.351 (Billing 0.34 → 0.33 → 0.35); freshness check |

This namespace's table also cited the state-transition vectors V12 ([09-rollup-delivery](#6-acceptance-criteria)), V13 ([09-rollup-delivery](#6-acceptance-criteria)), V19 ([07-child-evaluation](07-child-evaluation.md#6-acceptance-criteria)), V23 ([01-foundation](01-foundation.md#6-acceptance-criteria)); each is defined once, in the acceptance criteria of the feature that owns it.

<!-- /contract -->

## 7. Detailed Behavior Contracts

**Contract namespaces 15, 16.** Section numbers inside the detailed contracts below resolve through the [contract address index](../DECOMPOSITION.md#4-contract-address-index), not the overview section numbers above. A bare section reference names a section of the same namespace; "DESIGN §n" names §1–§5 of [DESIGN.md](../DESIGN.md).

The following sections keep the runtime procedures of these namespaces — their interactions and sequences and the procedural parts of their §4. The flows, processes and acceptance criteria above summarize them; the architecture, schemas and evaluation semantics they rely on are defined once in [DESIGN.md](../DESIGN.md) §6.

<a id="contract-15-3-6-f09"></a>

<!-- contract:15-rated-output-balance-effects:3.6 -->
### Results: Interactions and Sequences

**Contract**: `cpt-cf-bss-rating-flow-persist-delta-rob` (`p2`), defined in [§2 migrated flows](#register-flows).
  **Worked example** (Atlas F32 + F26, hourly volume, bands `[0, 10)` €0.02, `[10, ∞)` €0.015;
  amounts in EUR for readability, stored as exact major-unit fractions with `currency_scale = 2`):

  | Event | Child A (10:00) | Child B (11:00) | Parent result | Billing (rounded target, obligation) |
  |---|---|---|---|---|
  | month closes, both final | r1: Q=8 → 0.16 | r1: Q=12 → 0.18 | rev 1 = 0.34 | 0.34, invoice |
  | A corrected (Q 8 → 7.5) | r2: 0.15 | — | rev 2 = 0.33 | 0.33, credit −0.01 (pending) |
  | B corrected (Q 12 → 13.4; a quantity is a finite decimal) | — | r2: 0.201 | rev 3 = 0.351 | round(0.351) = 0.35 − cumulative 0.33 → debit +0.02 after the credit |

  Rev 3's vector is `[(A, r2), (B, r2)]`. While A's correction is in flight (A's `input_generation`
  bumped, A `final_stale` with r1 still its latest revision), a roll-up triggered by B's r2 finds A
  not current and publishes nothing; it re-enqueues A. It can never publish `[(A, r1), (B, r2)]` as
  rev 3 (DESIGN §4.13 V13).

<!-- /contract -->

<a id="contract-16-3-6"></a>

<!-- contract:16-billing-handoff-operations:3.6 -->
### Billing delivery and operations: Interactions and Sequences

**Contract**: `cpt-cf-bss-rating-flow-deliver-bhf` (`p1`), defined in [§2 migrated flows](#register-flows).
  **Billing consumes deliveries** (Billing's side, shown for the contract — **PROPOSED — NOT YET
  IMPLEMENTED**, Atlas C08; no Billing gear exists):

  ```text
  page = rating.deliveries_since(ctx, T10, after_seq = billing_checkpoint, 500)   (or the event)
  for d in page:                       -- after authentication and payload validation
    head = billing.fact_head(d.fact_id)
    d.result_revision < head.revision → ignore, unconditionally (an older complete replacement;
                                         its digest normally differs and that is not a conflict)
    d.result_revision = head.revision → same manifest_digest: ignore (duplicate)
                                         different manifest_digest: quarantine (conflict)
    d.result_revision > head.revision → replacement (revisions may be skipped):
          replace the fact's exact contributions with d.lines
          group target = Σ exact contributions per invoice_line_key over the billing group
          round each line total once, HALF_EVEN, at its stored currency_scale, to a major-unit
                          decimal validated as PostedMoney (money ADR rule 7)
          group draft   → replace the draft line
          group frozen  → posting obligation = rounded corrected line total − cumulative posted
                          amount (original posting + every prior note, pending ones included),
                          validated as PostedMoney, never rounded again (money ADR rule 8)
  billing_checkpoint = page.last_seq   (committed with Billing's own transaction)
  ```

**Contract**: `cpt-cf-bss-rating-flow-periodstate-relay-bhf` (`p1`), defined in [§2 migrated flows](#register-flows).
  **Recovery** (Atlas P8; Billing side PROPOSED): Billing notices an overdue group (a sealed set with a fact that has no
  complete delivery) and calls `find_runs(subscription_id, billing_group_id)`; the response carries
  the latest complete result per fact or its pending reasons. Re-read deliveries enter Billing's
  inbox with the same identity, so nothing doubles.

<!-- /contract -->
