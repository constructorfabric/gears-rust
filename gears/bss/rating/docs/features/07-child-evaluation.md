# Feature: Child Evaluation and Finalization

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-featstatus-child-evaluation-implemented`

- [ ] `p1` - `cpt-cf-bss-rating-feature-child-evaluation`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Evaluate one child](#21-evaluate-one-child)
  - [Migrated namespace flows](#migrated-namespace-flows)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Prefetch stored inputs](#31-prefetch-stored-inputs)
  - [3.2 Decide finality](#32-decide-finality)
  - [3.3 Assemble the evaluation input](#33-assemble-the-evaluation-input)
  - [3.4 Dispatch to the engine generation](#34-dispatch-to-the-engine-generation)
  - [3.5 Persist a final outcome](#35-persist-a-final-outcome)
  - [3.6 Verify retained engine generations at start-up](#36-verify-retained-engine-generations-at-start-up)
  - [3.7 Commitment balance effects (dormant)](#37-commitment-balance-effects-dormant)
  - [Migrated namespace procedures](#migrated-namespace-procedures)
- [4. States (CDSL)](#4-states-cdsl)
  - [4.1 Child status from the evaluation side](#41-child-status-from-the-evaluation-side)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 One transaction per child, current or nothing](#51-one-transaction-per-child-current-or-nothing)
  - [5.2 Finality only on proven evidence](#52-finality-only-on-proven-evidence)
  - [5.3 Frozen, replayable inputs and immutable results](#53-frozen-replayable-inputs-and-immutable-results)
  - [5.4 Retained engine generations and dormant effects](#54-retained-engine-generations-and-dormant-effects)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Detailed Behavior Contracts](#7-detailed-behavior-contracts)
  - [Integration contracts: Interactions and Sequences](#integration-contracts-interactions-and-sequences)
  - [Facts and scheduling: Interactions and Sequences](#facts-and-scheduling-interactions-and-sequences)
  - [Facts and scheduling: Input Assembly (normative)](#facts-and-scheduling-input-assembly-normative)
  - [Results: Interactions and Sequences](#results-interactions-and-sequences)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Evaluate one child window per transaction: lock the child, re-derive its binding and engine (the
binding of record, or the period's binding that Pricing's `resolve` returned for the fact's pins,
T-D-73), decide
finality with the evidence gate, assemble the input only from stored copies, call the engine
generation the child names, and persist a provisional estimate or an immutable final window result
with its snapshots, group context and roll-up work.

### 1.2 Purpose

The rater is the only writer of rated money. Rating promises exact results that are current against
their inputs and replayable for seven years. That holds only if every evaluation reads frozen,
stored inputs, checks under the child lock that nothing moved since the prefetch, and finalizes only
on proven evidence (T-D-52, T-D-62, T-D-63, T-D-65, T-D-67).

**Requirements** (primary, per DECOMPOSITION): `cpt-cf-bss-rating-fr-snapshot-carry`, `cpt-cf-bss-rating-nfr-resilience`, `cpt-cf-bss-rating-contract-rating-handoff`, `cpt-cf-bss-rating-contract-pricing-readmodel`. **Supporting**: `cpt-cf-bss-rating-fr-single-outcome-determinism`, `cpt-cf-bss-rating-fr-idempotency`.

**Principles**: `cpt-cf-bss-rating-principle-copy-what-you-rate`, `cpt-cf-bss-rating-principle-append-only-money`, `cpt-cf-bss-rating-principle-evidence-before-final`, `cpt-cf-bss-rating-principle-freeze-then-invoke-syn`, `cpt-cf-bss-rating-principle-fail-closed`, `cpt-cf-bss-rating-principle-persist-sealed-rob`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-bss-rating-actor-rating` | Runs the rater workers on `rating.child_work` under its service identity, narrowed to the work item's tenant |
| `cpt-cf-bss-rating-actor-catalog` | Pricing: answers `PricingReadV1::resolve` (bindings) and `price` (replay verification); inside the transaction the rater reads only the stored `bss_rating__pricing_binding` copies |
| `cpt-cf-bss-rating-actor-subscriptions` | Supplies the subscription versions, scope proofs and accepted price bindings the gate and assembly read (TARGET; ASSUMED/ABSENT today — no Subscriptions code, UPSTREAM_REQS §2.6) |
| `cpt-cf-bss-rating-actor-billing` | Reads window results and snapshots for inspection (`windows.list`, `snapshots.get`); never receives a provisional result (no Billing gear exists — ABSENT, UPSTREAM_REQS §2.7) |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §6.1 (determinism, snapshot carry, idempotency), §9.2 (rating handoff, pricing read-model input).
- **Architecture**: [DESIGN.md](../DESIGN.md) §3.6 Flow A′ (rater transaction, input-generation CAS, durable intent) — [§3.6](../DESIGN.md#36-interactions--sequences); [§3.7](../DESIGN.md#37-database-schemas--tables) (`bss_rating__child_window`, `bss_rating__window_result`, `bss_rating__snapshot`, `bss_rating__group_context`, `bss_rating__pricing_binding`, `bss_rating__derived_declaration`, `bss_rating__subscription_version`, `bss_rating__engine_generation`); [§4.1](../DESIGN.md#41-versioning-and-historical-correctness) (what is frozen, binding of record, engine generations, accepted binding vs snapshot); [§4.3](../DESIGN.md#43-aggregation-semantics) (window groups); [§4.4](../DESIGN.md#44-failure-model); [§4.5](../DESIGN.md#45-concurrency). Namespaces: [11 §4.1–§4.3](../DESIGN.md#contract-11-4-1), [13 §4.7](../DESIGN.md#contract-13-4-7) (scope and coverage evidence), [14 §4.5](../DESIGN.md#contract-14-4-5) (finalization gate), [14 §4.6](../DESIGN.md#contract-14-4-6), [15 §4.1](../DESIGN.md#contract-15-4-1), [15 §4.2](../DESIGN.md#contract-15-4-2), [15 §4.4](../DESIGN.md#contract-15-4-4), [15 §4.5](../DESIGN.md#contract-15-4-5). Complete runtime procedures are in [§7](#7-detailed-behavior-contracts).
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.7.
- **Dependencies**: [Foundation](01-foundation.md) (queues, tenancy, exceptions, fakes); [Deterministic Evaluation Core](02-evaluation-core.md) and [Price Adjustments](03-price-adjustments.md) (`EngineRegistry`, `evaluate`, `split_points`); [Attribution and Window Counters](05-attribution-counters.md) (counters, layouts, stored scope and coverage evidence); [Commercial Facts and Window Scheduling](06-fact-scheduling.md) (children, schedules, `next_check_at`).
- **Consumers**: [Parent Roll-up and Billing Delivery](09-rollup-delivery.md) rolls up current final results; [Corrections, Replay and Administrative Re-rate](08-corrections-rerate.md) sets the requested target this feature honours.
- **Upstream**: `cpt-cf-bss-rating-upreq-subscriptions-scope-proofs`, `cpt-cf-bss-rating-upreq-coverage-declarations`, `cpt-cf-bss-rating-upreq-resource-history`, `cpt-cf-bss-rating-upreq-accepted-binding-naming`, `cpt-cf-bss-rating-upreq-subscriptions-one-time-rating`, `cpt-cf-bss-rating-upreq-delay-only-acceptance` ([UPSTREAM_REQS](../UPSTREAM_REQS.md) §2.2, §2.6, §2.9); Pricing's `PricingReadV1::{resolve, price}` (CURRENT on `main`, T-D-73) and the Products derived-declaration read (R-31).

**Pricing input**: the rater prices the bindings Pricing's `resolve` returned for the fact's pins (T-D-73), stored verbatim at fact intake ([Commercial Facts and Window Scheduling](06-fact-scheduling.md)); the binding a child is finalized with is its **binding of record** and is never re-resolved by a correction. There is no catalog version, frontier, plan document or overlay document.

**UI applicability**: none. The two read routes (`bss_rating.windows.list`, `bss_rating.snapshots.get`) are API only and follow the REST table in [DESIGN §3.3](../DESIGN.md#33-api-contracts).

## 2. Actor Flows (CDSL)

**Use cases**: none in the PRD; the flow is a system flow driven by work items.

### 2.1 Evaluate one child

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-eval-child`

**Actor**: `cpt-cf-bss-rating-actor-rating` (rater worker)

**Success Scenarios**: unchanged inputs commit a no-op; a pending gate stores the provisional outcome, the pending reason and the next check; an eligible gate writes final revision `n + 1` and enqueues roll-up.

**Error Scenarios**: stale prefetch (restart ×3, then requeue); a binding other than the binding of record without a re-bind (a requested `requested_rebind`, or a fact version whose pricing query — revision, period or pins — changed), or an engine lower than of-record (rollback, alarm); `EvaluationError` (child `failed`, exception, no retry until an input changes); `engine_unavailable`; serialization failure (retry ×3, then requeue).

**Steps**:
1. [ ] - `p1` - Receive a work item `(tenant_id, child_id)`; it is a notification and carries no decision - `inst-ec-receive`
2. [ ] - `p1` - Prefetch outside the transaction: derive `(binding_of_record, engine)` from the unlocked row and ensure every stored copy exists (§3.1) - `inst-ec-prefetch`
3. [ ] - `p1` - BEGIN; lock the child row; re-derive `(binding_of_record, engine)` from the locked row - `inst-ec-lock`
4. [ ] - `p1` - **IF** the locked pair differs from the prefetched pair, roll back and restart assembly (×3, then requeue) - `inst-ec-stale`
5. [ ] - `p1` - **IF** the binding differs from `binding_of_record` without a re-bind (a requested `requested_rebind`, or a fact version whose pricing query — revision, period or pins — changed), or the engine is lower than `engine_of_record`, roll back and alarm - `inst-ec-backwards`
6. [ ] - `p1` - Run the evidence gate (§3.2) and assemble the input from stored copies (§3.3) - `inst-ec-gate`
7. [ ] - `p1` - **IF** `input_digest` and the gate outcome are unchanged, COMMIT as a no-op - `inst-ec-noop`
8. [ ] - `p1` - Evaluate with the engine generation the pair names (§3.4) - `inst-ec-evaluate`
9. [ ] - `p1` - **IF** the gate is `pending(reason)`, store the provisional outcome, `pending_reason` and `next_check_at` - `inst-ec-pending`
10. [ ] - `p1` - **IF** the gate is `final_eligible`, persist the final outcome (§3.5) and enqueue `fact_id` on `rating.rollup` in the same transaction - `inst-ec-final`
11. [ ] - `p1` - **RETURN** acknowledgement after COMMIT - `inst-ec-ack`

The transaction is canonical in DESIGN §3.6 Flow A′; this flow names the steps this feature implements.

<a id="register-flows"></a>

### Migrated namespace flows

The flows of the former slice documents that this feature implements are defined here and specified in [§7](#7-detailed-behavior-contracts); each entry links to its contract.

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-evaluate-handoff-cc`
  — Integration contracts — Interactions and Sequences ([contract](#contract-11-3-6))

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-pricewindow-events-cc`
  — Integration contracts — Interactions and Sequences ([contract](#contract-11-3-6))

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-synthesize-syn`
  — Facts and scheduling — Interactions and Sequences ([contract](#contract-14-3-6))

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-persist-outcome-rob`
  — Results — Interactions and Sequences ([contract](#contract-15-3-6))

## 3. Processes / Business Logic (CDSL)

### 3.1 Prefetch stored inputs

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-eval-prefetch`

**Input**: the unlocked child row.

**Output**: `(binding_of_record, engine)` and every input stored locally, or `pending(pins_unavailable)` / `pending(pricing_unavailable)` with a requeue.

1. [ ] - `p1` - Choose `(binding_of_record, engine)`: with `requested_rebind`, or when the fact version's pricing query (revision, period or pins) differs from the one the binding of record was resolved for, a fresh `resolve` with the fact's current pins at the period start and again at each `ends_on` (one `bss_rating__pricing_binding` row per resolve; `binding_of_record` = digest of the ordered per-resolve digests, listed in `binding_resolves`) and the requested or current engine; else the binding and engine of record; else the period's binding stored at fact intake and the current engine generation - `inst-pf-pair`
2. [ ] - `p1` - Ensure the subscription version, the `bss_rating__pricing_binding` rows for that digest, the derived declarations of its usage bindings (`bss_rating__derived_declaration`), the usage-policy projection and the slice layout; a layout change re-materializes in its own transaction and re-enqueues the child - `inst-pf-ensure`
3. [ ] - `p1` - **IF** no binding is available, requeue with backoff as `pending(pins_unavailable)` when the fact carries no pins, or `pending(pricing_unavailable)` when Pricing is unavailable - `inst-pf-no-pin`

Details: [14 §4.3](#contract-14-4-3), [11 §3.6](#contract-11-3-6).

### 3.2 Decide finality

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-eval-gate`

**Input**: the locked child, its fact, its schedule's pinned finalization policy, stored scope and coverage evidence, open source losses of its scope.

**Output**: `final_eligible` or `pending(reason)` with `next_check_at`.

1. [ ] - `p1` - Apply the conditions of [14 §4.5](../DESIGN.md#contract-14-4-5) for the child's kind and evidence mode - `inst-gt-conditions`
2. [ ] - `p1` - **IF** any `open` or `narrowed` source loss covers the child's tenant, GTS type and day, the outcome is `pending(source_loss)` under every evidence mode, `delay_only` included - `inst-gt-source-loss`
3. [ ] - `p1` - **IF** evidence is missing, the outcome is `pending(reason)` with `next_check_at = now + pending_rescan_interval` (doubling to 6 h); never a zero - `inst-gt-missing`
4. [ ] - `p1` - **IF** the scope is sealed and empty, the expected child is eligible with zero quantity - `inst-gt-empty`
5. [ ] - `p1` - **RETURN** the outcome; record the loss ids checked in `source_loss_check` - `inst-gt-return`

### 3.3 Assemble the evaluation input

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-eval-assemble`

**Input**: the locked child and the stored copies.

**Output**: `EvaluationInput`, its input manifest and `input_digest`.

1. [ ] - `p1` - Read the fact version, the counters at the current `layout_version` (per slice, granule and input), the stored bindings (`PricingInput {plan_id, revision_id, resolve_date, bindings, binding_digest}`), the derived declaration and the subscription version; nothing is fetched inside the transaction - `inst-as-read`
2. [ ] - `p1` - **IF** the child is a window-group member, hold `FOR SHARE` on the other members and capture an immutable `GroupContext`; store it content-addressed (T-D-67) - `inst-as-group`
3. [ ] - `p1` - **IF** a stored copy is missing, roll back, ensure it and requeue - `inst-as-missing`
4. [ ] - `p1` - **RETURN** the input, the manifest of every frozen dependency ([DESIGN §4.1](../DESIGN.md#41-versioning-and-historical-correctness)) and `input_digest` = SHA-256 of the canonical input - `inst-as-return`

### 3.4 Dispatch to the engine generation

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-eval-dispatch`

**Input**: `engine_generation` and the assembled input.

**Output**: `EvaluationOutcome`, or a typed error recorded on the child.

1. [ ] - `p1` - Look up the generation in `EngineRegistry`; **IF** it is absent, fail the child `engine_unavailable` and never substitute another generation - `inst-dp-lookup`
2. [ ] - `p1` - Call `evaluate`; **IF** it returns `EvaluationError` (including `precision_overflow`), set the child `failed` and open an exception with the reason code - `inst-dp-error`
3. [ ] - `p1` - **RETURN** the outcome - `inst-dp-return`

### 3.5 Persist a final outcome

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-eval-persist`

**Input**: a final outcome, the locked pair, the child's `input_generation`.

**Output**: window revision `n + 1`, snapshots, group context, closed re-rate targets, roll-up work.

1. [ ] - `p1` - Insert content-addressed snapshots per tenant and the group context (insert-only, `ON CONFLICT DO NOTHING`) - `inst-ps-snapshots`
2. [ ] - `p1` - Insert `bss_rating__window_result` revision `n + 1` with `covered_generation = input_generation`, the manifest, `engine_generation` and `engine_digest` - `inst-ps-result`
3. [ ] - `p1` - Set the child `final`, its current revision and covered generation, `binding_of_record` and `engine_of_record` to the locked pair; clear `next_check_at` - `inst-ps-child`
4. [ ] - `p1` - Close every pending re-rate target the revision meets (the engine generation reached and, for a re-bind target, a binding resolved at or after `rebind_as_of`); clear `requested_rebind` / `requested_engine` when met - `inst-ps-targets`
5. [ ] - `p1` - Enqueue `fact_id` on `rating.rollup` - `inst-ps-rollup`

Mapping from outcome to rows: [15 §4.1](../DESIGN.md#contract-15-4-1); procedure: [15 §3.6](#contract-15-3-6).

### 3.6 Verify retained engine generations at start-up

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-eval-engine-startup`

**Input**: `bss_rating__engine_generation` and the generations compiled into the build.

**Output**: rater workers started, or not started.

1. [ ] - `p1` - **FOR EACH** generation referenced by a retained result, check that it is compiled in and that its `engine_digest` matches - `inst-es-check`
2. [ ] - `p1` - **IF** any check fails, do not start the rater and page; other components may run - `inst-es-refuse`

### 3.7 Commitment balance effects (dormant)

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-algo-eval-balance-effects`

**Input**: a final result that observed commitment pools.

**Output**: nothing at launch.

1. [ ] - `p3` - **IF** a Contracts source exists (R-11), append `CommitmentBalanceEffect` rows keyed `(child_id, window_revision, pool_id)` in the rater transaction; until then read no balance and publish nothing - `inst-be-dormant`

Details: [14 §4.6](../DESIGN.md#contract-14-4-6), [15 §4.5](../DESIGN.md#contract-15-4-5).

<a id="register-procedures"></a>

### Migrated namespace procedures

The normative procedures of the former slice documents that this feature implements are defined here and specified in [§7](#7-detailed-behavior-contracts); each entry links to its contract.

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-context-assembly-syn`
  — Facts and scheduling — Input Assembly (normative) ([contract](#contract-14-4-3))

## 4. States (CDSL)

### 4.1 Child status from the evaluation side

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-state-eval-child`

**States**: `pending`, `provisional`, `final`, `final_stale`, `failed` (`bss_rating__child_window.status`, DESIGN §3.7). Creation is by [Attribution and Window Counters](05-attribution-counters.md) (`provisional`) and [Commercial Facts and Window Scheduling](06-fact-scheduling.md) (expected children).

**Initial State**: `pending` or `provisional`, as created.

**Transitions**:
1. [ ] - `p1` - **FROM** `pending` or `provisional` **TO** `pending(reason)` **WHEN** the gate finds missing evidence or an open source loss; the provisional outcome is overwritten - `inst-cs-pending`
2. [ ] - `p1` - **FROM** `pending`, `provisional` or `final_stale` **TO** `final` **WHEN** the gate is eligible and revision `n + 1` commits - `inst-cs-final`
3. [ ] - `p1` - **FROM** `final` **TO** `final_stale` **WHEN** any input change bumps `input_generation` past `current_covered_generation` (the bump is made by the feature that changes the input) - `inst-cs-stale`
4. [ ] - `p1` - **FROM** any non-final state **TO** `failed` **WHEN** evaluation returns an error, the engine generation is unavailable (a `resource`-scoped policy or an uncovered cell is failed at fact intake, [06-fact-scheduling](06-fact-scheduling.md)) - `inst-cs-failed`
5. [ ] - `p1` - **FROM** `failed` **TO** `pending` **WHEN** an input changes and the child is re-enqueued - `inst-cs-retry`

## 5. Definitions of Done

### 5.1 One transaction per child, current or nothing

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-eval-transaction`

The system **MUST** evaluate each child in one transaction under its row lock, re-derive the binding and engine from the locked row, write only when `input_generation` still equals the value read, never replace a binding of record except by a re-bind (a requested `requested_rebind`, or a fact version whose pricing query — revision, period or pins — changed) and never move an engine of record backwards, and treat a redelivered work item on unchanged inputs as a no-op.

**Implements**: `cpt-cf-bss-rating-flow-eval-child`, `cpt-cf-bss-rating-algo-eval-prefetch`, `cpt-cf-bss-rating-algo-eval-dispatch`, `cpt-cf-bss-rating-state-eval-child`, `cpt-cf-bss-rating-flow-synthesize-syn`, `cpt-cf-bss-rating-flow-evaluate-handoff-cc`.

**Constraints**: `cpt-cf-bss-rating-constraint-pin-discipline-syn`, `cpt-cf-bss-rating-constraint-stateless-hot-path`.

**Touches**: `rating.child_work`, `rating.rollup`; `cpt-cf-bss-rating-dbtable-child-window`, `cpt-cf-bss-rating-dbtable-rated-version`.

### 5.2 Finality only on proven evidence

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-eval-gate`

The system **MUST** finalize a usage child only when every condition of [14 §4.5](../DESIGN.md#contract-14-4-5) holds for its evidence mode, keep it `pending(reason)` with a durable `next_check_at` otherwise, block it while a source loss covers its scope under every evidence mode, and finalize a sealed empty scope as an explicit zero.

**Implements**: `cpt-cf-bss-rating-algo-eval-gate`.

**Constraints**: `cpt-cf-bss-rating-constraint-no-fabricated-proof-qst`.

**Touches**: `bss_rating__usage_scope`, `bss_rating__coverage_declaration` (within `cpt-cf-bss-rating-dbtable-attribution-segment`), `cpt-cf-bss-rating-dbtable-source-loss`, `cpt-cf-bss-rating-dbtable-window-schedule`.

### 5.3 Frozen, replayable inputs and immutable results

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-eval-persist`

The system **MUST** assemble every input from stored, version-keyed copies, record the input manifest and `input_digest` on each final revision, store snapshots content-addressed within the owning tenant and group contexts content-addressed, keep every money-bearing row insert-only, and close the re-rate targets a final revision meets.

**Implements**: `cpt-cf-bss-rating-algo-eval-assemble`, `cpt-cf-bss-rating-algo-eval-persist`, `cpt-cf-bss-rating-algo-context-assembly-syn`, `cpt-cf-bss-rating-flow-persist-outcome-rob`, `cpt-cf-bss-rating-flow-pricewindow-events-cc`.

**Constraints**: `cpt-cf-bss-rating-constraint-record-not-compute-rob`.

**Touches**: `cpt-cf-bss-rating-dbtable-snapshot`, `cpt-cf-bss-rating-dbtable-group-context`, `cpt-cf-bss-rating-dbtable-catalog-document` (now `bss_rating__pricing_binding`), `bss_rating__derived_declaration`, `cpt-cf-bss-rating-dbtable-subscription-version`, `bss_rating__rerate_target`.

### 5.4 Retained engine generations and dormant effects

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-eval-engines`

The system **MUST** dispatch by `engine_generation` without substitution, refuse to start the rater when a generation referenced by a retained result is missing or its digest differs, and publish no commitment balance effect until a Contracts source exists.

**Implements**: `cpt-cf-bss-rating-algo-eval-engine-startup`, `cpt-cf-bss-rating-algo-eval-balance-effects`.

**Constraints**: `cpt-cf-bss-rating-constraint-stateless-hot-path`.

**Touches**: `cpt-cf-bss-rating-dbtable-engine-generation`; `bss_rating__balance_effect` (dormant).

Observability: this feature emits `rating_rating_duration_seconds`, `rating_context_failclosed_total{reason}`, `rating_child_pending{reason}`, `rating_dependency_errors_total{dependency=pricing|products}`, `rating_engine_unavailable_total`, `rating_precision_overflow_total` and the `rating.child.evaluate`, `rating.core.evaluate` spans (DESIGN §4.7).

## 6. Acceptance Criteria

- [ ] A work item redelivered on unchanged inputs commits nothing new: no revision, no roll-up work.
- [ ] A child whose `input_generation` moves between read and write commits nothing; it is evaluated again on its next work item.
- [ ] A child whose inputs fail evaluation becomes `failed` with an exception and is not retried until an input changes.
- [ ] A child whose recorded engine generation is not compiled in fails `engine_unavailable`; no other generation is used. An instance missing a referenced generation does not start the rater.
- [ ] Coverage that arrives after the deadline finalizes the child only once it is stored; before that the child stays `pending` with a `next_check_at`, never zero.
- [ ] A provisional outcome is never delivered and is overwritten by the next evaluation.
- [ ] The same snapshot body rated for two tenants is stored as two rows; a snapshot id alone authorizes nothing.
- [ ] With no Contracts source, no commitment balance effect row is written.
- [ ] **V32** A successor price approved after a window is final leaves that child unchanged; the next period's fact is rated with whatever the renewal walk binds (`all` taken, `new` not taken, pricing D-420).
- [ ] **V31** A correction of a final child reuses its binding of record and calls no `resolve`; a re-bind target, or a new fact version whose revision, period or pins changed, re-resolves with the fact's current pins; a cancelled price read back through `price(price_id)` is served with state `Cancelled` and never replaces the binding of record (pricing D-422, D-520).
- [ ] The snapshot body records `plan_id`, `revision_id`, `resolve_date`, every binding's `price_id`, `money_digest`, `sku_version`, `usage_rating_policy` and derived declaration digest, and the `binding_digest`; no catalog version appears.

Acceptance vectors owned by this feature (DESIGN §4.13 index):

| # | Scenario | Expected state |
|---|---|---|
| V05 | `CursorBeyondRetention` under `delay_only` | source loss `open`; affected children stay `pending(source_loss)` after `delay`; no delivery |
| V09 | Worker A prefetches binding B1; a re-bind re-rate moves the child to binding B2; A then locks | A restarts; no revision on B1 |
| V19 | A plan change inside one `BillingCycle` window continues the counter (T-D-36 continuation key); member 1 is corrected later | member 2 replays identically from its stored group context |
| V20 | Sealed empty `subscription_line` scope | one zero child per usage binding and window |

## 7. Detailed Behavior Contracts

**Contract namespaces 11, 14, 15.** Section numbers inside the detailed contracts below resolve through the [contract address index](../DECOMPOSITION.md#4-contract-address-index), not the overview section numbers above. A bare section reference names a section of the same namespace; "DESIGN §n" names §1–§5 of [DESIGN.md](../DESIGN.md).

The following sections keep the runtime procedures of these namespaces — their interactions and sequences and the procedural parts of their §4. The flows, processes and acceptance criteria above summarize them; the architecture, schemas and evaluation semantics they rely on are defined once in [DESIGN.md](../DESIGN.md) §6.

<a id="contract-11-3-6"></a>

<!-- contract:11-consumer-contracts:3.6 -->
### Integration contracts: Interactions and Sequences

**Contract**: `cpt-cf-bss-rating-flow-evaluate-handoff-cc` (`p1`), defined in [§2 migrated flows](#register-flows).
  **Ensure inputs for one evaluation** (outside the rating transaction):

  ```text
  (binding, engine) = child.requested_rebind or pricing_query(fact) ≠ child.binding_resolves.query
                      ? (resolve(revision_id, period_start, fact.current_pins) and again at each ends_on,
                         child.requested_engine ?? engine_of_record)
                    : child.*_of_record
                    ?? (period binding stored at fact intake, EngineRegistry.current())
                    -- DESIGN Flow A′; no pins on the fact → pending(pins_unavailable);
                    -- Pricing 503 / unavailable → pending(pricing_unavailable)
                    -- the rater re-derives (binding, engine) under the child lock and restarts on mismatch
  PricingBindingStore.ensure(tenant, binding_digest)       -- resolve as bss-rating.system, plan:read;
                                                           -- AcceptedBindings stored verbatim
  DerivedDeclarationStore.ensure(tenant, meter)            -- usage bindings; Products read (R-31)
  SubscriptionVersionStore.ensure(tenant, subscription_id, latest)
  → the rater opens its transaction with every input present locally
  ```

**Contract**: `cpt-cf-bss-rating-flow-pricewindow-events-cc` (`p2`), defined in [§2 migrated flows](#register-flows).
  **No pricing event consumption.** Price changes reach a subscription only through its pins and a
  new fact version (SUB-D-29), and a binding's own end is its `ends_on` (pricing D-425), so Rating
  needs no pricing event. Pricing publishes `prices_published`, `plan_revision_published`,
  `approval_unit_decided` and the reference-lost events through the event broker (topic
  `gts.cf.core.events.topic.v1~cf.bss.pricing.catalog.v1`; held in the outbox while no broker is
  registered); Rating consumes none of them (pull is sufficient, T-D-73).

<!-- /contract -->

<a id="contract-14-3-6"></a>

<!-- contract:14-unit-synthesis-period-tick:3.6 -->
### Facts and scheduling: Interactions and Sequences

**Contract**: `cpt-cf-bss-rating-flow-synthesize-syn` (`p1`), defined in [§2 migrated flows](#register-flows).
  **Evaluate a child** — DESIGN §3.6 Flow A′. If the line's cut set computed from the assembled
  inputs (UTC hours, every usage item's window bounds and slice cuts such as a binding's `ends_on`)
  differs from the stored line layout, the rater calls `ensure_layout`
  (re-materialization, its own transaction), re-enqueues the child, and acks.

<!-- /contract -->

<a id="contract-14-4-3"></a>

<!-- contract:14-unit-synthesis-period-tick:4.3 -->
### Facts and scheduling: Input Assembly (normative)

**Contract**: `cpt-cf-bss-rating-algo-context-assembly-syn` (`p1`), defined in [§3 migrated procedures](#register-procedures).

In order, outside the transaction (a **prefetch**; the transaction re-checks it):

1. Read the child row without a lock and derive `(binding_of_record, engine)`: with
   `requested_rebind`, or when the fact version's pricing query (revision, period or pins) changed,
   a fresh `resolve(revision_id, period_start, fact.current_pins)` (again at each `ends_on`) and
   `requested_engine`; else `(binding_of_record, engine_of_record)` if set; else
   the period's binding stored at fact intake and `EngineRegistry::current()` (no pins on the fact ⇒ requeue with
   backoff, `pending(pins_unavailable)`; Pricing unavailable ⇒ `pending(pricing_unavailable)`). The work item carries no reason that could
   override this.
2. `SubscriptionVersionStore.ensure(latest)` → `subscription_version`.
3. `PricingBindingStore.ensure(binding_digest)`: the `AcceptedBinding`s stored verbatim
   (`bss_rating__pricing_binding`); for usage children `DerivedDeclarationStore.ensure(meter)`
   (`bss_rating__derived_declaration`, R-31).
4. `PolicyProjectionWriter.ensure_policy_projection(subscription, item)` for usage children: the usage-policy projection.
5. the line's cut set → `ensure_layout` (usage children; one layout per line and fact period).

Inside the transaction: re-read the child row (`FOR UPDATE`) and **re-derive `(binding_of_record, engine)` from
the locked row**; if it differs from the prefetched pair, roll back and restart the assembly (×3,
then requeue) — an older worker can never publish an obsolete binding after a newer re-rate
(T-D-63); a binding other than the binding of record without a re-bind (a requested
re-bind or a changed pricing query of the fact version), or an engine lower than the engine of
record, is a defect (roll back, alarm). Then read the fact version, counters at
the current `layout_version`, the window-group context (members `FOR SHARE`, stored as
`bss_rating__group_context`), the stored bindings and derived declaration, the subscription version,
the evidence rows and the open source losses of the child's scope. The assembled set is the input
manifest; `input_digest` = SHA-256 of the canonical `EvaluationInput`. The rater writes only if the
child's `input_generation` still equals the value it read; a final revision records
`covered_generation = input_generation` and closes every pending re-rate target it meets.

<!-- /contract -->

<a id="contract-15-3-6"></a>

<!-- contract:15-rated-output-balance-effects:3.6 -->
### Results: Interactions and Sequences

**Contract**: `cpt-cf-bss-rating-flow-persist-outcome-rob` (`p1`), defined in [§2 migrated flows](#register-flows).
  **Persist a final child outcome** (inside the rater transaction):

  ```text
  INSERT bss_rating__snapshot (tenant_id, snapshot_id, body) (each distinct line snapshot) ON CONFLICT DO NOTHING
  INSERT bss_rating__group_context (tenant_id, window_group_id, context_digest, …) ON CONFLICT DO NOTHING
                                                                       -- grouped children only
  INSERT bss_rating__window_result (tenant_id, window_start = child.window_start, child_id,
         window_revision = current + 1, input_generation, covered_generation = child.input_generation,
         manifest, input_digest, lines, obligations, state = final)
  UPDATE bss_rating__child_window SET current_revision = current + 1, status = final,
         current_covered_generation = child.input_generation,
         binding_of_record = digest(ordered per-resolve binding_digests),
         binding_resolves = [{resolve_date, binding_digest}], engine_of_record = engine,
                                                       -- the pair the rater locked (T-D-42, T-D-73, T-D-24, T-D-63)
         current_input_digest = …, next_check_at = null
  UPDATE bss_rating__rerate_target SET state = done … (targets this revision meets, DESIGN Flow A′)
  Outbox.enqueue(rating.rollup, fact_id)
  ```

  Parent roll-up: DESIGN §3.6 `seq-parent-rollup`.

<!-- /contract -->
