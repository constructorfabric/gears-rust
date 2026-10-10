# Feature: Commercial Facts and Window Scheduling

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-featstatus-fact-scheduling-implemented`

- [ ] `p1` - `cpt-cf-bss-rating-feature-fact-scheduling`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Accept a commercial fact version](#21-accept-a-commercial-fact-version)
  - [2.2 Create due child windows](#22-create-due-child-windows)
  - [2.3 Wake children and fact heads by time](#23-wake-children-and-fact-heads-by-time)
  - [2.4 Write a finalization policy](#24-write-a-finalization-policy)
  - [Migrated namespace flows](#migrated-namespace-flows)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Map a current fact to a fact identity (MIGRATION)](#31-map-a-current-fact-to-a-fact-identity-migration)
  - [3.2 Resolve and pin the finalization policy](#32-resolve-and-pin-the-finalization-policy)
  - [3.3 Compute the expected children of a fact](#33-compute-the-expected-children-of-a-fact)
  - [3.4 Apply a newer fact version](#34-apply-a-newer-fact-version)
  - [3.5 Coalesce work and back off wake-ups](#35-coalesce-work-and-back-off-wake-ups)
  - [3.6 Watch for missing facts](#36-watch-for-missing-facts)
  - [Migrated namespace procedures](#migrated-namespace-procedures)
- [4. States (CDSL)](#4-states-cdsl)
  - [4.1 Child window creation and staleness](#41-child-window-creation-and-staleness)
  - [4.2 Window schedule cursor](#42-window-schedule-cursor)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Versioned fact intake](#51-versioned-fact-intake)
  - [5.2 Rating-owned finalization policy](#52-rating-owned-finalization-policy)
  - [5.3 Durable scheduling, wake-ups and coalescing](#53-durable-scheduling-wake-ups-and-coalescing)
  - [5.4 Fact recovery and watchdog](#54-fact-recovery-and-watchdog)
- [6. Acceptance Criteria](#6-acceptance-criteria)
  - [Facts and scheduling: Acceptance Vectors](#facts-and-scheduling-acceptance-vectors)
- [7. Detailed Behavior Contracts](#7-detailed-behavior-contracts)
  - [Facts and scheduling: Interactions and Sequences](#facts-and-scheduling-interactions-and-sequences)
  - [Facts and scheduling: Work Queue, Scheduler and Coalescing (normative)](#facts-and-scheduling-work-queue-scheduler-and-coalescing-normative)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Accept commercial facts and their versions through the inbox; bind each fact's prices by calling
Pricing's `resolve` with the pins the fact carries (date = the period start, again at a binding's
`ends_on`; T-D-73) and store the bindings, the Products derived usage declarations and the usage
policy projection of the line; pin the Rating-owned finalization policy for each usage fact; derive
and create the expected child windows beneath each fact from the binding's `UsageRatingPolicy`
(T-D-75) on a durable schedule; and wake every child and fact head whose state can change by time
alone.

### 1.2 Purpose

A commercial fact authorizes rating, and Subscriptions owns the commercial WHEN (T-D-33); Rating
owns the child windows beneath the fact, their schedule, input waits and catch-up (T-D-49).
A durable `next_due_window_start` cursor and durable wake-ups (T-D-68) let a month of 744 hourly
windows, an arrears line or a pending window progress — and catch up after an outage — without any
further upstream event. The finalization delay is Rating's, versioned and pinned per fact (T-D-59).

**Requirements** (primary, per DECOMPOSITION): `cpt-cf-bss-rating-contract-subscriptions-input`. **Supporting**: `cpt-cf-bss-rating-fr-tier-aggregation-window`, `cpt-cf-bss-rating-fr-idempotency`, `cpt-cf-bss-rating-nfr-resilience`.

**Principles**: `cpt-cf-bss-rating-principle-adopt-the-sor`, `cpt-cf-bss-rating-principle-evidence-before-final`, `cpt-cf-bss-rating-principle-idempotent-tick-syn`, `cpt-cf-bss-rating-principle-fail-closed`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-bss-rating-actor-subscriptions` | Publishes commercial facts and their versions (CURRENT: documented `BillableItemCreated`, no transport; TARGET: `BillableFactPublished`, PROPOSED) |
| `cpt-cf-bss-rating-actor-rating` | Runs fact intake, the scheduler and the wake scanner under its service identity |
| `cpt-cf-bss-rating-actor-platform-operator` | Writes platform-default finalization policies (`platform_finalization_policy × write`, audited). Seller finalization policies are written by a seller operator (`finalization_policy × write`, DESIGN §4.8), a role the PRD does not list as an actor |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §6.7 (tier aggregation window), §9.2 (Subscriptions input contract), §13.
- **Architecture**: [DESIGN.md](../DESIGN.md) §3.1 (child kinds, `WindowEvaluationKey`, expected children), §3.3 (REST `finalization_policies`), §3.6 Flow C and the usage parent and hourly scheduler, §3.7 (`bss_rating__fact`, `bss_rating__fact_head`, `bss_rating__window_schedule`, `bss_rating__finalization_policy`, `bss_rating__child_window`), §3.8, §4.2, §4.3, §4.8 (operation matrix), §4.10 (launch-gated windows); namespace [14](../DESIGN.md#contract-14) — [child kinds](../DESIGN.md#contract-14-4-1), [commercial facts](../DESIGN.md#contract-14-4-2). Complete runtime procedures are in [§7](#7-detailed-behavior-contracts).
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.6.
- **Dependencies**: [Foundation](01-foundation.md) (inbox, tenancy scopes, Cluster leader election, work queues, contract fakes); [Deterministic Evaluation Core](02-evaluation-core.md) (`window_geometry`).
- **Consumers**: [Child Evaluation and Finalization](07-child-evaluation.md) evaluates the children this feature creates and wakes, against the pinned policy; [Parent Roll-up and Billing Delivery](09-rollup-delivery.md) rolls up per fact version and is woken by `rollup_due_at`; [Corrections, Replay and Administrative Re-rate](08-corrections-rerate.md) relies on fact versions bumping `input_generation`.
- **Upstream**: `cpt-cf-bss-rating-upreq-subscriptions-versioned-facts`, `cpt-cf-bss-rating-upreq-subscriptions-usage-fact`, `cpt-cf-bss-rating-upreq-subscriptions-recovery-reads`, `cpt-cf-bss-rating-upreq-delay-only-acceptance`, `cpt-cf-bss-rating-upreq-cluster-coordination-backend` ([UPSTREAM_REQS](../UPSTREAM_REQS.md) §2.6, §2.9, §2.12); the facts must carry the period's pins, plan revision, committed quantity and `BillingTerms` (SUB-D-29; UPSTREAM_REQS §2.6); pricing's `PricingReadV1::resolve` (T-D-73) and the Products derived-declaration read (R-31, UPSTREAM_REQS §2.5). `cpt-cf-bss-rating-upreq-lifetime-settlement-model` is closed: pricing has no lifetime window (R-28).

**UI applicability**: no user interface. The finalization-policy REST routes follow the shared API conventions of DESIGN §3.3 (RFC 9457 problem details, keyset cursors, required `Idempotency-Key` on create); consoles own their UI.

## 2. Actor Flows (CDSL)

**Use cases**: none in the PRD; facts arrive from Subscriptions and policies are operator configuration.

### 2.1 Accept a commercial fact version

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-schedule-accept-fact`

**Actor**: `cpt-cf-bss-rating-actor-subscriptions` (through the inbox: event when enabled, pull recovery always)

**Success Scenarios**: a new fact version is stored with its head; its children are created (recurring, usage provisional) or its schedules are created (usage: one per usage item, `(tenant_id, fact_id, item_id)`); children of a newer version get `input_generation + 1`.

**Error Scenarios**: same key with a different digest (`fact_digest_conflict`, quarantined); lower version after a higher one (stored, not applied); no matching finalization policy (`finalization_policy_missing`); a cell without a binding (`price_uncovered`); Pricing refuses the resolve (`pricing_refused(PIN_FOREIGN | PIN_DUPLICATE | REVISION_NOT_PUBLISHED | REVISION_NOT_YET_AVAILABLE | …)`, `incomplete_commercial_inputs`) or is unavailable (`pricing_unavailable`); the derived declaration cannot be read (the fact is stored and its usage children wait `pending(derived_declaration_unavailable)`, R-31 — never failed or quarantined at intake); a `resource`-scoped usage policy (LAUNCH-GATED, `resource_scope_not_supported`); a CURRENT fact that carries no pins (children `pending(pins_unavailable)` — Rating never resolves a signup binding for an existing subscription).

**Steps**:
1. [ ] - `p1` - Accept the fact into the inbox keyed `(source = fact, fact_id, fact_version)` with its payload digest; **IF** the key exists with another digest, quarantine and stop - `inst-af-inbox`
2. [ ] - `p1` - **IF** the producer has no versioned identity (CURRENT), derive `fact_id` and `fact_version` with the MIGRATION mapping (§3.1) - `inst-af-migrate`
3. [ ] - `p1` - Insert the fact version (insert-only) and raise the head's `current_version` to the maximum - `inst-af-insert`
4. [ ] - `p1` - **IF** kind is `recurring`, insert the `period_line` child if absent with `next_check_at = now` (advance) or `served_to` (arrears) - `inst-af-recurring`
5. [ ] - `p1` - **IF** kind is `one_time`, store only; a `one_time` child exists only if R-19 is accepted (T-D-18) - `inst-af-one-time`
6. [ ] - `p1` - **IF** kind is `usage` or `recurring`, bind the fact before its transaction: `PricingBindingStore.ensure` calls `PricingReadV1::resolve(revision_id, date = period start, pins)` (splitting by `item_id` above 1 000 pins), stores the `AcceptedBinding`s verbatim in `bss_rating__pricing_binding` under their `binding_digest`, stores each usage binding's derived declaration (`bss_rating__derived_declaration`) and writes the line's `bss_rating__usage_policy_projection`; a binding with an `ends_on` inside the period makes Rating resolve again with the same pin on `ends_on` for the rest of the period (one `bss_rating__pricing_binding` row per resolve; the child lists them in `binding_resolves`). **IF** kind is `usage`, resolve and pin the finalization policy (§3.2), create one window schedule per usage item, keyed `(tenant_id, fact_id, item_id)`, from that item's binding `rating_window` (entries of one plan may differ, pricing D-504) and the provisional children of windows that already have counters (§3.3); **IF** the policy's `aggregation_scope` is `resource`, create its children `failed(resource_scope_not_supported)` with an exception and no schedule (LAUNCH-GATED, T-D-75) - `inst-af-usage`
7. [ ] - `p1` - **IF** kind is `usage` or timing is `arrears`, set the head's `rollup_due_at = served_to` - `inst-af-rollup-due`
8. [ ] - `p1` - **IF** this is a newer version, apply it to the existing children (§3.4) - `inst-af-new-version`
9. [ ] - `p1` - Enqueue the affected children, mark the inbox entry `applied`; COMMIT - `inst-af-commit`

Canonical: DESIGN §3.6 Flow C and the usage parent flow; detail in [14 §3.6](#contract-14-3-6-f06).

### 2.2 Create due child windows

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-schedule-due-scan`

**Actor**: `cpt-cf-bss-rating-actor-rating` (`WindowScheduler`, leader of one shard)

**Success Scenarios**: every expected child of every due window exists and is enqueued; the cursor has advanced past the windows that are due.

**Error Scenarios**: crash or lost leadership before commit (cursor not advanced, the next leader rescans; unique `child_id` absorbs re-insertion).

**Steps**:
1. [ ] - `p1` - Hold the shard leadership `bss-rating/scheduler/{shard}` (Cluster `LeaderElectionApi`) - `inst-ds-leader`
2. [ ] - `p1` - BEGIN; select up to 500 schedules whose window at `next_due_window_start` has `end + delay ≤ now`, `FOR UPDATE SKIP LOCKED` - `inst-ds-select`
3. [ ] - `p1` - **FOR EACH** due window inside the fact's served extent, insert every expected child (§3.3) if absent with `next_check_at = now` and enqueue it - `inst-ds-insert`
4. [ ] - `p1` - Advance `next_due_window_start` to the first window not yet due, in the same transaction; COMMIT - `inst-ds-advance`

Runs at least once per minute per shard; after an outage the same loop catches up every due slot.

### 2.3 Wake children and fact heads by time

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-schedule-wake`

**Actor**: `cpt-cf-bss-rating-actor-rating` (`ChildWakeScanner`, leader of one shard, independent of the creation cursor)

**Success Scenarios**: every `pending`, `provisional` or `final_stale` child with `next_check_at ≤ now` and every fact head with `rollup_due_at ≤ now` is enqueued, and its next check is set in the same transaction.

**Error Scenarios**: crash before commit (times not advanced; rescanned).

**Steps**:
1. [ ] - `p1` - BEGIN; select up to 500 due children through the partial index, `FOR UPDATE SKIP LOCKED` - `inst-wk-children`
2. [ ] - `p1` - Enqueue each child and set `next_check_at = now + backoff` (§3.5) - `inst-wk-child-next`
3. [ ] - `p1` - Select up to 500 fact heads with `rollup_due_at ≤ now`, `FOR UPDATE SKIP LOCKED`; enqueue each on `rating.rollup` and set its next `rollup_due_at`; COMMIT - `inst-wk-heads`

Event wake-ups (a counter change, a fact version, a segment past the scope's sequence, a sealed scope, a coverage declaration, a resolved source loss) are enqueued by the transaction that stores the input; this flow covers time alone ([14 §3.6](#contract-14-3-6-f06)).

### 2.4 Write a finalization policy

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-schedule-write-policy`

**Actor**: `cpt-cf-bss-rating-actor-platform-operator` (a seller operator for seller policies; a platform operator for platform defaults)

**Success Scenarios**: a new policy version is stored, insert-only; schedules already created keep the version they pinned.

**Error Scenarios**: `401`/`403`; `400` when a seller request names `owner_scope = platform`; `409 AlreadyExists` for the same `Idempotency-Key` with a different body; `delay_only` without the recorded Finance/Product acceptance (R-21).

**Steps**:
1. [ ] - `p1` - Receive `POST /bss-rating/v1/finalization-policies` (`finalization_policy × write`) or `POST /bss-rating/v1/platform/finalization-policies` (`platform_finalization_policy × write`) with a required `Idempotency-Key` - `inst-wp-receive`
2. [ ] - `p1` - Authorize through the PDP and record the operation key in `bss_rating__operation`; a retry returns the original response - `inst-wp-authorize`
3. [ ] - `p1` - **IF** seller route, write `owner_scope = seller` with the caller's seller tenant; **IF** platform route, write through the dedicated platform repository with `tenant_id = rating.platform_tenant_id` - `inst-wp-scope`
4. [ ] - `p1` - Insert the new version (`policy_id`, `version`, `window_policy` (a `rating_window` value: `billing_cycle` or `calendar_hour`), `delay`, `evidence_mode`, `effective_from`); audit actor and request - `inst-wp-insert`
5. [ ] - `p1` - **RETURN** `201` with the version - `inst-wp-return`

`GET /bss-rating/v1/finalization-policies` lists the policies of the caller's seller scope, platform defaults read-only (DESIGN §3.3).

<a id="register-flows"></a>

### Migrated namespace flows

The flows of the former slice documents that this feature implements are defined here and specified in [§7](#7-detailed-behavior-contracts); each entry links to its contract.

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-period-tick-syn`
  — Facts and scheduling — Interactions and Sequences ([contract](#contract-14-3-6-f06))

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-scheduler-syn`
  — Facts and scheduling — Interactions and Sequences ([contract](#contract-14-3-6-f06))

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-cascade-route-syn`
  — Facts and scheduling — Interactions and Sequences ([contract](#contract-14-3-6-f06))

## 3. Processes / Business Logic (CDSL)

### 3.1 Map a current fact to a fact identity (MIGRATION)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-schedule-migration-mapping`

**Input**: a CURRENT `BillableItemCreated(kind = recurring)` per `(subscriptionId, billing period, lineKey)`, or — without any usage fact (R-20) — a subscription version and the first attributed record of a line.

**Output**: `(fact_id, fact_version, parent_origin)`.

1. [ ] - `p1` - **IF** recurring, `fact_id = UUIDv5(NS_RATING_FACT, "{subscription_id}|{period_start}|{lineKey}|recurring")`, `fact_version = 1`, `parent_origin = published`; a re-emission with another digest is quarantined `fact_digest_conflict` - `inst-mm-recurring`
2. [ ] - `p1` - **IF** usage and no usage fact exists, derive one parent per `(subscription_id, sub_line_key, billing period)` from the billing period the subscription's recurring facts state, or else from its Subscriptions-owned `BillingTerms` (`cycle: Month | Year`, `anchor: Calendar | SubscriptionStart`, `anchor_at`, UTC; pricing-sdk `terms.rs`) — Rating never chooses or shifts an anchor: `fact_id = UUIDv5(NS_RATING_FACT, "{subscription_id}|{period_start}|{sub_line_key}|usage")`, `parent_origin = derived`, `billing_group_kind = derived` - `inst-mm-usage`
3. [ ] - `p1` - Create a derived parent only when the first attributed record or the subscription version names the line; never invent a subscription or a commercial period - `inst-mm-never-invent`
4. [ ] - `p1` - **IF** a TARGET `BillableFactPublished` exists, use its Subscriptions-assigned `fact_id` and `fact_version` and switch the derivation off - `inst-mm-target`

Canonical: DESIGN §3.6 Flow C step 1 and usage parent step 4; [14 §4.2](../DESIGN.md#contract-14-4-2).

### 3.2 Resolve and pin the finalization policy

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-schedule-pin-policy`

**Input**: a usage fact (its seller `tenant_id` and the binding's `rating_window`: `BillingCycle` or `CalendarHour`).

**Output**: `(policy_id, version, owner_scope, delay, evidence_mode)` pinned into the window schedule, or `finalization_policy_missing`.

1. [ ] - `p1` - Under a scope narrowed to the fact's tenant, select the effective seller policy for the `rating_window` - `inst-pp-seller`
2. [ ] - `p1` - **IF** none, select the platform default through the read-only platform repository method that returns only `owner_scope = platform` rows - `inst-pp-platform`
3. [ ] - `p1` - **IF** none, fail the schedule `finalization_policy_missing` and open an exception - `inst-pp-missing`
4. [ ] - `p1` - Check that the policy's `tenant_id` equals the fact's or its scope is `platform`; pin `(policy_id, version, owner_scope)`, `delay` and `evidence_mode` into the schedule - `inst-pp-pin`
5. [ ] - `p1` - Never re-pin on a later fact version or policy version: a pinned deadline never moves - `inst-pp-never-repin`

`evidence_mode = full` is the default; `delay_only` exists only per policy with the recorded Finance/Product acceptance of R-21 and never bypasses an open source loss (T-D-62).

### 3.3 Compute the expected children of a fact

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-schedule-expected-children`

**Input**: a fact version, its served extent, and the line's usage bindings (each with its `usage_rating_policy` and derived `meter`) from the binding of the period.

**Output**: the expected child keys and `expected_manifest_digest` on the fact head.

1. [ ] - `p1` - `period_line` and `one_time` facts: exactly one child - `inst-ec-single`
2. [ ] - `p1` - Usage, `subscription_line` scope: per usage binding, one child per window of `window_geometry(rating_window, served extent)` — the billing period for `BillingCycle`, each UTC hour for `CalendarHour{Utc}` — keyed `wk1` with the binding's derived `meter` (`dimension_key = ""` at launch, R-16) - `inst-ec-usage`
3. [ ] - `p1` - There is no per-event window (pricing offers only `BillingCycle` and `CalendarHour`, T-D-75), so no `ek1` child is ever created - `inst-ec-event`
4. [ ] - `p1` - Record the expected set as a manifest digest on the head; never derive it from the counters that happen to exist - `inst-ec-manifest`

Canonical: DESIGN §3.1 "Expected children of a fact"; `resource` scope is expressible in pricing (R-23 closed) but LAUNCH-GATED until scope proofs list resources (T-D-75).

### 3.4 Apply a newer fact version

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-schedule-new-version`

**Input**: a fact version higher than the head's previous `current_version`.

**Output**: every child of the fact marked for re-evaluation; the expected manifest updated.

1. [ ] - `p1` - Under each child's row lock, increment `input_generation`; a `final` child becomes `final_stale`. **IF** the new version's pricing query (revision, period or pins) differs from the previous one, bind it again (§2.1 step 6) so its children re-bind; otherwise every child keeps its binding of record - `inst-nv-bump`
2. [ ] - `p1` - Update `expected_manifest_digest`; for each usage schedule (one per usage item) update only `fact_version`, never the pinned policy - `inst-nv-manifest`
3. [ ] - `p1` - Never delete a child: a removed obligation is a zero fact version that produces a zero result - `inst-nv-no-delete`
4. [ ] - `p1` - Enqueue the children - `inst-nv-enqueue`

### 3.5 Coalesce work and back off wake-ups

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-schedule-coalesce`

**Input**: an enqueue request for a child; a child's pending reason.

**Output**: at most one queued work item per child; the child's next wake time.

1. [ ] - `p1` - **IF** `queued_at` is set, skip the enqueue; the rater clears it when it starts, and work created during a run enqueues normally - `inst-co-skip`
2. [ ] - `p1` - Dead-letter a work item after 10 attempts: the child becomes `failed` and an exception is opened - `inst-co-deadletter`
3. [ ] - `p1` - Set `next_check_at` to `window_end + delay` for a usage window, `served_to` for an arrears line, and `now + pending_rescan_interval` (15 min, doubling to 6 h) after an evidence-based `pending` - `inst-co-backoff`

A work item is a notification only: no semantic transition is carried by a queue message ([14 §4.4](#contract-14-4-4)).

### 3.6 Watch for missing facts

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-schedule-fact-watchdog`

**Input**: active subscription lines; counters with no known parent fact.

**Output**: recovered facts re-ingested through the inbox, or an alarm.

1. [ ] - `p1` - **IF** `SubscriptionBillingReadV1::period_facts` exists (TARGET; PROPOSED, not implemented), read the overlapping periods per tenant shard and feed missing facts into the same inbox, which absorbs duplicates - `inst-fw-target`
2. [ ] - `p1` - **ELSE** (CURRENT) raise `rating_fact_missing` when a subscription version shows an active line with no fact 24 h after period start - `inst-fw-current`
3. [ ] - `p1` - Report counters without a known fact as `rating_usage_without_fact`; never fabricate a commercial period (T-D-33) - `inst-fw-no-fabricate`

<a id="register-procedures"></a>

### Migrated namespace procedures

The normative procedures of the former slice documents that this feature implements are defined here and specified in [§7](#7-detailed-behavior-contracts); each entry links to its contract.

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-cascade-routing-syn`
  — Facts and scheduling — Work Queue, Scheduler and Coalescing (normative) ([contract](#contract-14-4-4))

## 4. States (CDSL)

### 4.1 Child window creation and staleness

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-state-schedule-child-creation`

**States**: `pending`, `provisional`, `final_stale`, `failed` (this feature's transitions of the child window status in `bss_rating__child_window`; evaluation transitions to `final`, `pending(reason)` and `failed` on evaluation errors belong to [Child Evaluation and Finalization](07-child-evaluation.md)).

**Initial State**: none — a child exists only beneath a known fact.

**Transitions**:
1. [ ] - `p1` - **FROM** — **TO** `pending` **WHEN** fact intake creates a `period_line` child or the scheduler creates a due expected child - `inst-cc-pending`
2. [ ] - `p1` - **FROM** — **TO** `provisional` **WHEN** fact intake creates the child of a window that already has counters (`next_check_at = window_end + delay`) - `inst-cc-provisional`
3. [ ] - `p1` - **FROM** — **TO** `failed(resource_scope_not_supported)` **WHEN** the binding's `aggregation_scope` is `resource` (LAUNCH-GATED, T-D-75) - `inst-cc-gated`
4. [ ] - `p1` - **FROM** `final` **TO** `final_stale` **WHEN** a newer fact version bumps `input_generation` - `inst-cc-stale`
5. [ ] - `p1` - **FROM** any non-terminal state **TO** `failed` **WHEN** its work item is dead-lettered after 10 attempts - `inst-cc-deadletter`

### 4.2 Window schedule cursor

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-state-schedule-cursor`

**States**: `scheduling`, `complete`.

**Initial State**: `scheduling`, with `next_due_window_start` at the first window of the served extent.

**Transitions**:
1. [ ] - `p1` - **FROM** `scheduling` **TO** `scheduling` **WHEN** a due scan commits; `next_due_window_start` only moves forward, in the transaction that creates the children - `inst-sc-advance`
2. [ ] - `p1` - **FROM** `scheduling` **TO** `complete` **WHEN** the cursor passes the fact's `served_to` - `inst-sc-complete`

## 5. Definitions of Done

### 5.1 Versioned fact intake

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-schedule-fact-intake`

The system **MUST** accept every fact through the inbox in one transaction per fact version, apply the MIGRATION mappings only while the producer offers no versioned identity, store fact versions insert-only with an ordered head, bind each fact through `resolve` with the fact's pins and store the binding of the period, create the children of recurring facts and the schedule and provisional children of usage facts from the binding's `UsageRatingPolicy`, reject `resource`-scoped policies with `resource_scope_not_supported` (LAUNCH-GATED), and apply a newer version to every child of the fact without deleting any.

**Implements**: `cpt-cf-bss-rating-flow-schedule-accept-fact`, `cpt-cf-bss-rating-algo-schedule-migration-mapping`, `cpt-cf-bss-rating-algo-schedule-expected-children`, `cpt-cf-bss-rating-algo-schedule-new-version`, `cpt-cf-bss-rating-state-schedule-child-creation`, `cpt-cf-bss-rating-flow-period-tick-syn`.

**Constraints**: `cpt-cf-bss-rating-constraint-no-commercial-clock-syn`, `cpt-cf-bss-rating-constraint-synthesis-only-syn`, `cpt-cf-bss-rating-constraint-in-process-contracts`.

**Touches**: `cpt-cf-bss-rating-dbtable-inbox`, `cpt-cf-bss-rating-dbtable-fact`, `cpt-cf-bss-rating-dbtable-child-window`, `cpt-cf-bss-rating-dbtable-window-schedule`, `bss_rating__pricing_binding`, `bss_rating__derived_declaration`, `bss_rating__usage_policy_projection`; `PricingReadV1::resolve` (as `bss-rating.system`, pricing `plan:read`); queues `rating.child_work`, `rating.rollup`.

### 5.2 Rating-owned finalization policy

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-schedule-policy`

The system **MUST** resolve the most specific finalization policy (seller, then platform default) at fact intake, pin its id, version, owner scope, delay and evidence mode into the window schedule, fail a fact with no matching policy `finalization_policy_missing`, serve the seller and platform write routes with their separate permissions and repositories, and never move a pinned deadline.

**Implements**: `cpt-cf-bss-rating-algo-schedule-pin-policy`, `cpt-cf-bss-rating-flow-schedule-write-policy`.

**Constraints**: `cpt-cf-bss-rating-constraint-pin-discipline-syn`.

**Touches**: REST `bss_rating.finalization_policies.list`, `bss_rating.finalization_policies.create`, `bss_rating.platform_finalization_policies.create`; `cpt-cf-bss-rating-dbtable-finalization-policy`, `cpt-cf-bss-rating-dbtable-window-schedule`, `bss_rating__operation`.

### 5.3 Durable scheduling, wake-ups and coalescing

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-schedule-durable`

The system **MUST** create every expected child of a due window in the transaction that advances the schedule cursor, catch up every due slot after downtime, wake children and fact heads from `next_check_at` and `rollup_due_at` independently of the creation cursor, coalesce work items on `queued_at`, and dead-letter a child after 10 failed attempts; correctness rests on the cursor CAS, `SKIP LOCKED` and the unique `child_id`, never on leadership.

**Implements**: `cpt-cf-bss-rating-flow-schedule-due-scan`, `cpt-cf-bss-rating-flow-schedule-wake`, `cpt-cf-bss-rating-algo-schedule-coalesce`, `cpt-cf-bss-rating-state-schedule-cursor`, `cpt-cf-bss-rating-flow-scheduler-syn`, `cpt-cf-bss-rating-flow-cascade-route-syn`, `cpt-cf-bss-rating-algo-cascade-routing-syn`.

**Constraints**: `cpt-cf-bss-rating-constraint-no-commercial-clock-syn`.

**Touches**: `cpt-cf-bss-rating-dbtable-window-schedule`, `cpt-cf-bss-rating-dbtable-child-window` (`next_check_at`, `queued_at`), `cpt-cf-bss-rating-dbtable-fact` (`rollup_due_at`); Cluster elections `bss-rating/scheduler/{shard}`.

### 5.4 Fact recovery and watchdog

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-dod-schedule-watchdog`

The system **MUST** recover missing facts through `period_facts` into the same inbox once that read exists (TARGET) and, until then (CURRENT), alarm an active line without a fact 24 h after its period start, without ever fabricating a commercial period.

**Implements**: `cpt-cf-bss-rating-algo-schedule-fact-watchdog`.

**Constraints**: `cpt-cf-bss-rating-constraint-no-commercial-clock-syn`.

**Touches**: `SubscriptionBillingReadV1::period_facts` (TARGET); `cpt-cf-bss-rating-dbtable-fact`; Cluster lock `bss-rating/reconcile/{shard}` (the `Reconciler`, DESIGN §3.6 Flow C).

Observability: this feature emits `rating_fact_versions_total`, `rating_fact_missing`, `rating_usage_without_fact`, `rating_scheduler_backlog{shard}`, `rating_child_wake_lag_seconds{child_kind}` and the `rating.inbox.accept` span (DESIGN §4.7).

## 6. Acceptance Criteria

- [ ] A fact redelivered with the same id, version and digest is a no-op; the same id and version with another digest is quarantined `fact_digest_conflict`.
- [ ] A lower fact version arriving after a higher one is stored and not applied.
- [ ] A newer fact version bumps `input_generation` of every child of the fact, turns `final` children `final_stale` and deletes none.
- [ ] An advance recurring fact's `period_line` child is enqueued at acceptance with no further event.
- [ ] Without a usage fact, the derived parent is keyed per `(subscription_id, sub_line_key, billing period)`, marked `billing_group_kind = derived`, and is created only for a line a record or the subscription version names.
- [ ] A usage fact with no seller or platform policy for its `rating_window` fails `finalization_policy_missing`; a policy version written after intake does not change the pinned delay.
- [ ] A seller operator cannot read another seller's policy; a platform default is visible read-only.
- [ ] A scheduler leader that loses its lease mid-batch does not advance the cursor; the next leader creates the same children with no duplicates.
- [ ] An enqueue while `queued_at` is set adds no work item; a work item created during a run is enqueued again; a child whose work item fails 10 times is `failed` with an exception.
- [ ] With no `period_facts` read (CURRENT), an active line with no fact 24 h after period start raises `rating_fact_missing`.
- [ ] A fact is bound with `resolve(revision_id, period start, the fact's pins)`; a renewal pin walks `all` successors and stops before a `new` one (pricing golden `resolve_renewal_walk`: pinned 10 → `all` 12 → `new` 15 binds 12); a binding with `ends_on` inside the period produces a second resolve on that date and a slice boundary there (pricing D-425).
- [ ] An uncovered cell creates the child `failed(price_uncovered)`; a `PIN_FOREIGN` refusal fails the whole fact binding `pricing_refused(PIN_FOREIGN)`; neither produces a charge.
- [ ] A `CalendarHour{Utc}` usage binding yields 744 children for October; a `BillingCycle` binding yields one child for the billing period.

Acceptance vectors owned by this feature (DESIGN §4.13 index):

| # | Scenario | Expected state |
|---|---|---|
| V14 | Arrears recurring fact with no further upstream event | final at `served_to` via the wake scanner |
| V15 | Evidence arrives before the window's deadline | evaluated at `window_end + delay` |
| V17 | Retired: pricing offers no per-event window (T-D-75) | — |
| V25 | A seller operator posts a platform-default policy | `400`; no row |

<a id="contract-14-4-7"></a>

<!-- contract:14-unit-synthesis-period-tick:4.7 -->
### Facts and scheduling: Acceptance Vectors

Copied from the Atlas (fixture ids kept) for Rating's scheduler and gate:

| Fixture | Given | Required result |
|---|---|---|
| F28 | line active all October UTC; worker down 3 h; one hour without resources | 744 children, no duplicates; empty hour = zero child; parent cannot omit a lost hour |
| F29 | activation 10:30, 8 cloudlets until 11:00 | window 10:00–11:00, served 10:30–11:00, Q = 4, 0.08 EUR |
| F30 | window ends 11:00, delay 5 min; coverage absent at 11:05, arrives 11:08 | pending at 11:05; final at/after 11:08 |
| F33 | one monthly fact, no further subscription events | every hour computed; restart resumes from checkpoints |
| F34 | 10:00–11:00 final, month open | readable via `window_results`; no parent delivery |
| F35 | usage before the fact / before attribution reaches the barrier | usage retained; fact recovered; child pending |
| gated scope | a usage binding whose `usage_rating_policy.content.aggregation_scope = resource` | children `failed(resource_scope_not_supported)`; exception; nothing delivered |

This namespace's table also cited the state-transition vectors V05 ([07-child-evaluation](07-child-evaluation.md#6-acceptance-criteria)), V08 ([08-corrections-rerate](08-corrections-rerate.md#6-acceptance-criteria)), V09 ([07-child-evaluation](07-child-evaluation.md#6-acceptance-criteria)), V14 ([06-fact-scheduling](#6-acceptance-criteria)), V15 ([06-fact-scheduling](#6-acceptance-criteria)), V20 ([07-child-evaluation](07-child-evaluation.md#6-acceptance-criteria)); each is defined once, in the acceptance criteria of the feature that owns it.

<!-- /contract -->

## 7. Detailed Behavior Contracts

**Contract namespace 14.** Section numbers inside the detailed contracts below resolve through the [contract address index](../DECOMPOSITION.md#4-contract-address-index), not the overview section numbers above. A bare section reference names a section of the same namespace; "DESIGN §n" names §1–§5 of [DESIGN.md](../DESIGN.md).

The following sections keep the runtime procedures of these namespaces — their interactions and sequences and the procedural parts of their §4. The flows, processes and acceptance criteria above summarize them; the architecture, schemas and evaluation semantics they rely on are defined once in [DESIGN.md](../DESIGN.md) §6.

<a id="contract-14-3-6-f06"></a>

<!-- contract:14-unit-synthesis-period-tick:3.6 -->
### Facts and scheduling: Interactions and Sequences

**Contract**: `cpt-cf-bss-rating-flow-period-tick-syn` (`p1`), defined in [§2 migrated flows](#register-flows).
  **Fact intake** (one transaction per fact version):

  ```text
  inbox entry source=fact business_id=fact_id version=fact_version digest=d
  BEGIN
    INSERT bss_rating__inbox … ON CONFLICT DO NOTHING            same key, digest ≠ d → quarantine; COMMIT
    INSERT bss_rating__fact (fact_id, fact_version, …) ON CONFLICT DO NOTHING
    UPSERT bss_rating__fact_head SET current_version = max(current_version, fact_version)
    kind = recurring → INSERT child period_line (wk1|fact|period_start|agg) ON CONFLICT DO NOTHING
                       next_check_at = timing = advance ? now : served_to
    kind = one_time  → R-19 accepted ? INSERT child one_time : store only
    binding (before BEGIN): PricingBindingStore.ensure(resolve(revision_id, period_start, fact.pins))
                       → bss_rating__pricing_binding (binding_digest), derived declarations,
                         bss_rating__usage_policy_projection; ends_on inside the period → resolve again with the
                         same pin on ends_on (one pricing_binding row per resolve)
    kind = usage     → binding.usage_rating_policy.content.aggregation_scope = resource
                         → children failed(resource_scope_not_supported), exception; no schedule  -- LAUNCH-GATED
                       for each usage item: INSERT bss_rating__window_schedule
                         (tenant_id, fact_id, item_id; window = the item's rating_window,
                          policy pinned now, next_due_window_start = first window)
                       INSERT provisional children for windows that already have counters (F35),
                         next_check_at = window_end + delay
                         ON CONFLICT DO UPDATE only fact_version (policy never re-pinned)
    usage or arrears → UPDATE bss_rating__fact_head SET rollup_due_at = served_to
    newer version    → UPDATE bss_rating__child_window SET input_generation += 1,
                              status = CASE status WHEN final THEN final_stale ELSE status END
                        WHERE fact_id = ?   -- under each child's row lock; every child of the fact,
                                            -- since a parent revision is per fact version ([contract 15](../DESIGN.md#contract-15))
                       UPDATE bss_rating__fact_head SET expected_manifest_digest = …
    enqueue affected children; INSERT bss_rating__inbox state = applied
  COMMIT
  ```

  Example (recurring, MIGRATION mapping of the current Subscriptions fact): subscription S42, period
  `[2026-09-15, 2026-10-15)`, `lineKey = plan#1`, `priceId = P-REC` → `fact_id = UUIDv5(…,
  "S42|2026-09-15T00:00Z|plan#1|recurring")`, `fact_version = 1`, one `period_line` child, bound by
  `resolve(revision_id, 2026-09-15, pins = [P-REC])`.

**Contract**: `cpt-cf-bss-rating-flow-scheduler-syn` (`p1`), defined in [§2 migrated flows](#register-flows).
  **Due scan** (`WindowScheduler`, every ≤ 60 s per shard, batches of ≤ 500 schedules):

  ```text
  BEGIN
    SELECT … FROM bss_rating__window_schedule
     WHERE shard = ? AND next_due_window_start + window_length + delay <= now()
     ORDER BY next_due_window_start LIMIT 500 FOR UPDATE SKIP LOCKED
    for s: for each window w from s.next_due_window_start while w.end + s.delay <= now()
                                                      and w.start < fact.served_to:
             for each aggregation key k of s.item in expected_children(fact, w) (DESIGN §3.1):
               INSERT bss_rating__child_window (wk1|s.fact|w.start|k) ON CONFLICT DO NOTHING
               enqueue child (notification)
           UPDATE s SET next_due_window_start = <first window not yet due>
  COMMIT
  ```

  After an outage the same loop catches up every due slot; the unique `child_id` absorbs re-runs
  (F28). For `subscription_line` scope the expected keys are one per usage binding of the line (its
  derived `meter`, empty `dimension_key` at launch), so an hour with no usage still gets its
  children — and a sealed empty scope gives each of those children zero evidence, so each becomes an
  explicit zero (F04); it never removes them. Only under `resource` scope (expressible in pricing,
  LAUNCH-GATED here) does a sealed empty resource set mean zero resource children. Inserted children get `next_check_at = now`.

**Contract**: `cpt-cf-bss-rating-flow-cascade-route-syn` (`p2`), defined in [§2 migrated flows](#register-flows).
  **Wake-ups** (T-D-68). Two mechanisms, both durable:

  1. **Event wake-ups**: children waiting on evidence are re-enqueued by the transaction that stores
     the evidence: a counter change, a fact version, a segment that raises `applied_seq` past the
     scope's `segments_through_seq`, a sealed scope, a coverage declaration, a source loss resolved.
  2. **Time wake-ups** (`ChildWakeScanner`, every ≤ 60 s per shard, independent of
     `next_due_window_start`):

     ```text
     BEGIN
       SELECT child_id FROM bss_rating__child_window
        WHERE shard = ? AND next_check_at <= now()          -- partial index: pending, provisional, final_stale
        ORDER BY next_check_at LIMIT 500 FOR UPDATE SKIP LOCKED
       for c: enqueue child; UPDATE c SET next_check_at = now() + backoff(c)
       SELECT fact_id FROM bss_rating__fact_head
        WHERE shard = ? AND rollup_due_at <= now() LIMIT 500 FOR UPDATE SKIP LOCKED
       for h: Outbox.enqueue(rating.rollup, fact_id); UPDATE h SET rollup_due_at = now() + backoff(h)
     COMMIT
     ```

     `next_check_at` is the earliest instant the gate can change by time alone: `window_end +
     delay` for usage windows (set when a provisional child is created), `served_to` for arrears
     `period_line` children, and after an evidence-based `pending` `now + pending_rescan_interval`
     (default 15 min, doubling per attempt to 6 h). The rater clears it on `final` and on `failed`;
     roll-up clears `rollup_due_at` when it delivers. A fact evaluated early therefore progresses
     with no later upstream event (arrears lines, pending usage children, event children once
     ungated, parent deadlines), and evidence that arrived before the deadline is evaluated at the
     deadline.

<!-- /contract -->

<a id="contract-14-4-4"></a>

<!-- contract:14-unit-synthesis-period-tick:4.4 -->
### Facts and scheduling: Work Queue, Scheduler and Coalescing (normative)

**Contract**: `cpt-cf-bss-rating-algo-cascade-routing-syn` (`p1`), defined in [§3 migrated procedures](#register-procedures).

- `rating.child_work`: partition `hash(subscription_id) mod N`; at-least-once; dead-letter after 10
  attempts (child `failed` + exception).
- Coalescing: enqueue is skipped when `bss_rating__child_window.queued_at` is set; the rater clears it
  when it starts; work created during a run re-enqueues normally. Coalescing combines
  notifications only: an ordinary correction and a re-rate of the same child collapse into one work
  item, and that item is evaluated at the re-rate's requested target because the target is on the
  row, written before the notification (T-D-63). No semantic transition is ever carried only by a
  queue message.
- Provisional evaluations are optional: under backlog (`rating.child_work` depth > threshold) the
  rater skips provisional work for windows not yet due; due and correction work is never skipped.
- Retry: dependency `Unavailable` (Pricing 503 `REGISTRY_UNAVAILABLE`, the Products declaration read) — exponential backoff 1 s → 5 min; serialization
  failure — immediate ×3; `EvaluationError` — none until an input changes or an operator retries.
- Expected geometry: `window_geometry(rating_window, served extent)`; for `CalendarHour{Utc}` the
  canonical windows are UTC `[HH:00, HH+1:00)`; for `BillingCycle` the window is the fact's billing
  period; a fact opening at 10:30 has a first window 10:00–11:00 served
  from 10:30 (F29).

<!-- /contract -->
