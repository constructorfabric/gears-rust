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
  - [3.1 Validate a fact version against the profile](#31-validate-a-fact-version-against-the-profile)
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

Accept commercial fact versions through the inbox — Subscriptions' versioned, full-replacement
facts (`subscriptions-rating/1`, Subscriptions PR #5516; T-D-87), each carrying its served extent,
period, timing, `BillingTerms` and a timeline of slices with the frozen accepted bindings and
purchase lots in force — and store each version with an ordered head; keep each accepted binding as
the binding of record (T-D-73 as amended by T-D-87), the Products derived usage declarations and the
usage policy projection of the line; pin the Rating-owned finalization policy for each usage fact;
derive and create the expected child windows beneath each fact from the binding's
`UsageRatingPolicy` (T-D-75) on a durable schedule; and wake every child and fact head whose state
can change by time alone. Rating never substitutes a term the fact carries: Pricing's `resolve` with
the accepted pins only proves, before a child's first final, that the binding is still current
(T-D-87).

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
| `cpt-cf-bss-rating-actor-subscriptions` | Publishes commercial fact versions (CURRENT on `main`: the documented, unversioned `BillableItemCreated`, no code or transport; TARGET: `BillableFactPublished` under the `subscriptions-rating/1` profile of Subscriptions PR #5516, open — what this feature consumes, T-D-87) |
| `cpt-cf-bss-rating-actor-rating` | Runs fact intake, the scheduler and the wake scanner under its service identity |
| `cpt-cf-bss-rating-actor-platform-operator` | Writes platform-default finalization policies (`platform_finalization_policy × write`, audited). Seller finalization policies are written by a seller operator (`finalization_policy × write`, DESIGN §4.8), a role the PRD does not list as an actor |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §6.7 (tier aggregation window), §9.2 (Subscriptions input contract), §13.
- **Architecture**: [DESIGN.md](../DESIGN.md) §3.1 (child kinds, `WindowEvaluationKey`, expected children), §3.3 (REST `finalization_policies`), §3.6 Flow C and the usage parent and hourly scheduler, §3.7 (`bss_rating__fact`, `bss_rating__fact_head`, `bss_rating__window_schedule`, `bss_rating__finalization_policy`, `bss_rating__child_window`), §3.8, §4.2, §4.3, §4.8 (operation matrix), §4.10 (launch-gated windows); namespace [14](../DESIGN.md#contract-14) — [child kinds](../DESIGN.md#contract-14-4-1), [commercial facts](../DESIGN.md#contract-14-4-2). Complete runtime procedures are in [§7](#7-detailed-behavior-contracts).
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.6.
- **Dependencies**: [Foundation](01-foundation.md) (inbox, tenancy scopes, Cluster leader election, work queues, contract fakes); [Deterministic Evaluation Core](02-evaluation-core.md) (`window_geometry`).
- **Consumers**: [Child Evaluation and Finalization](07-child-evaluation.md) evaluates the children this feature creates and wakes, against the pinned policy; [Parent Roll-up and Billing Delivery](09-rollup-delivery.md) rolls up per fact version and is woken by `rollup_due_at`; [Corrections, Replay and Administrative Re-rate](08-corrections-rerate.md) relies on fact versions bumping `input_generation`.
- **Upstream**: `cpt-cf-bss-rating-upreq-subscriptions-versioned-facts`, `cpt-cf-bss-rating-upreq-subscriptions-usage-fact`, `cpt-cf-bss-rating-upreq-subscriptions-recovery-reads`, `cpt-cf-bss-rating-upreq-delay-only-acceptance`, `cpt-cf-bss-rating-upreq-cluster-coordination-backend` ([UPSTREAM_REQS](../UPSTREAM_REQS.md) §2.6, §2.9, §2.12); the facts carry their served extent, timing, `BillingTerms`, slices, lots and frozen accepted bindings (Subscriptions PR #5516, T-D-87; UPSTREAM_REQS §2.6); pricing's `PricingReadV1::resolve` for the freshness proof (T-D-73, T-D-87) and the Products derived-declaration read (R-31, UPSTREAM_REQS §2.5). `cpt-cf-bss-rating-upreq-lifetime-settlement-model` is closed: pricing has no lifetime window (R-28).

**UI applicability**: no user interface. The finalization-policy REST routes follow the shared API conventions of DESIGN §3.3 (RFC 9457 problem details, keyset cursors, required `Idempotency-Key` on create); consoles own their UI.

## 2. Actor Flows (CDSL)

**Use cases**: none in the PRD; facts arrive from Subscriptions and policies are operator configuration.

### 2.1 Accept a commercial fact version

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-schedule-accept-fact`

**Actor**: `cpt-cf-bss-rating-actor-subscriptions` (through the inbox: event when enabled, pull recovery always)

**Success Scenarios**: a new fact version is stored with its head and its accepted bindings; its `period_line` child is created (recurring) or its schedule and expected manifest are created (usage: one schedule per fact, keyed `(tenant_id, fact_id, item_id)`, since a usage fact is one PriceBook item); a newer version bumps `input_generation` of every child of the fact.

**Error Scenarios**: same key with a different digest (`fact_digest_conflict`, quarantined); lower version after a higher one (stored, inbox `applied`, nothing else touched; V81); a sub-second instant anywhere in the fact (`instant_not_whole_second`, the fact version quarantined; V60); a fact that breaks the profile (§3.1: slices that do not partition the served extent, a chargeable slice without terms, a lot naming a term the fact does not carry, a binding whose `money_digest` or `template_digest` does not verify, a quantity outside canonical decimal text — `payload_invalid`, quarantined); no matching finalization policy (the schedule waits `policy_pending` with an exception `finalization_policy_missing`, V80); a derived declaration that cannot be read (the fact is stored, its head waits `pending(derived_declaration_unavailable)` and is retried on `rating.fact_bind`, R-31 — never failed or quarantined at intake) or that differs from the stored copy (`failed(derived_declaration_conflict)`, exception); a `resource`-scoped usage policy (LAUNCH-GATED, `resource_scope_not_supported`); usage bindings of one fact that disagree on `rating_window`, `aggregation_scope` or `meter` (`usage_policy_mismatch`, V86). An `unresolved` slice is not an intake error: the fact is stored and the children it touches wait at evaluation (V85).

**Steps**:
1. [x] - `p1` - Accept the fact into the inbox keyed `(source = fact, fact_id, fact_version)` with Rating's digest of the payload; **IF** the key exists with another digest, quarantine `fact_digest_conflict` and stop; **IF** the fact breaks the profile (§3.1), quarantine the fact version `instant_not_whole_second` or `payload_invalid` and stop, never truncating it (DESIGN 09 §4.1, V60) - `inst-af-inbox`
2. [x] - `p1` - **IF** kind is `usage`, before the transaction read the derived declaration of the fact's meter through `DerivedDeclarationStore.ensure` (R-31; stored verbatim in `bss_rating__derived_declaration`, its digest verified); a version lower than the stored head reads nothing (V81) - `inst-af-declarations`
3. [x] - `p1` - Insert the fact version (insert-only) with its timeline and accepted terms, and raise the head's `current_version` to the maximum - `inst-af-insert`
   1. [x] - `p1` - **IF** the fact version is lower than the head's `current_version` (a late redelivery), mark the inbox entry `applied` and COMMIT: steps 4–9 run only for the head's current version, so an old version never touches a binding, a schedule's `fact_version`, a child or the manifest (V81) - `inst-af-current-only`
4. [x] - `p1` - Store each accepted binding the version carries in `bss_rating__pricing_binding` under its `binding_digest` (pricing's `selected_bindings_digest` of that one cell; `ON CONFLICT DO NOTHING`, the same digest with other content raises an alarm): it is the binding of record of the slices that name it (T-D-87) - `inst-af-bindings`
5. [x] - `p1` - **IF** kind is `recurring`, insert the `period_line` child if absent with `next_check_at = now` (advance) or `served_to` (arrears), the fact's `timing` - `inst-af-recurring`
6. [x] - `p1` - **IF** kind is `one_time`, store only; a `one_time` child exists only once R-19 is accepted and Subscriptions' S-U08/S-U13 gates lift (T-D-18, T-D-87) - `inst-af-one-time`
7. [ ] - `p1` - **IF** kind is `usage` and its declaration is stored: **IF** the usage bindings disagree on `rating_window`, `aggregation_scope` or `meter`, create each cell's child over the billing period `failed(usage_policy_mismatch)` with an exception and no schedule; **IF** the policy's `aggregation_scope` is `resource`, create its children `failed(resource_scope_not_supported)` with an exception and no schedule (LAUNCH-GATED, T-D-75); otherwise write the line's `bss_rating__usage_policy_projection`, resolve and pin the finalization policy (§3.2), create the fact's window schedule from the binding's `rating_window`, record the expected manifest (§3.3) and create the provisional children of windows that already have counters; set the head `binding_state = bound` - `inst-af-usage`
   1. [x] - `p1` - **IF** kind is `usage` and its declaration could not be read, still store the fact and mark the inbox entry `applied`, but create no schedule, no manifest and no `rollup_due_at`: record the head's `binding_state = pending(derived_declaration_unavailable)` with `binding_next_check_at = now + backoff`, or `failed(derived_declaration_conflict)` with an exception. The `ChildWakeScanner` re-enqueues a pending head on `rating.fact_bind`, and step 7 runs again for the head's current version ([14 §3.6](#contract-14-3-6-f06)); roll-up never runs before the head is bound - `inst-af-bind-pending`
8. [x] - `p1` - **IF** kind is `usage` and the fact is bound, or kind is `recurring` and timing is `arrears`, set the head's `rollup_due_at = served_to` - `inst-af-rollup-due`
9. [x] - `p1` - **IF** this is a newer version, apply it to the existing children (§3.4) - `inst-af-new-version`
10. [x] - `p1` - Enqueue the affected children, mark the inbox entry `applied`; COMMIT - `inst-af-commit`

Canonical: DESIGN §3.6 Flow C and the usage parent flow; detail in [14 §3.6](#contract-14-3-6-f06).

### 2.2 Create due child windows

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-schedule-due-scan`

**Actor**: `cpt-cf-bss-rating-actor-rating` (`WindowScheduler`, leader of one shard)

**Success Scenarios**: every expected child of every due window exists and is enqueued; the cursor has advanced past the windows that are due.

**Error Scenarios**: crash or lost leadership before commit (cursor not advanced, the next leader rescans; unique `child_id` absorbs re-insertion).

**Steps**:
1. [ ] - `p1` - Hold the shard leadership `scheduler-{shard}` (Cluster `LeaderElectionV1`, scoped `bss-rating`) - `inst-ds-leader`
2. [ ] - `p1` - BEGIN; select up to 500 `scheduling` schedules of the shard with `next_due_at ≤ now` (the stored end of the window at `next_due_window_start` plus the pinned delay; a `BillingCycle` window is a month or a year, so no fixed length is assumed) through the index `(shard, next_due_at)`, `FOR UPDATE SKIP LOCKED` - `inst-ds-select`
3. [ ] - `p1` - **FOR EACH** due window inside the fact's served extent, at most 168 windows per schedule and 5 000 children per transaction, insert every expected child (§3.3) if absent with `next_check_at = now` and enqueue it - `inst-ds-insert`
4. [ ] - `p1` - Advance `next_due_window_start` to the first window not yet created and `next_due_at` to that window's end plus the delay, in the same transaction; set `complete` once the cursor passes `served_to`; COMMIT - `inst-ds-advance`
5. [ ] - `p1` - In the same scan, retry the pin of the shard's `policy_pending` schedules whose `next_due_at ≤ now` (§3.2); a schedule that pins becomes `scheduling` with its first window's `next_due_at`, one that does not is retried 15 minutes later (`next_due_at = now + 15 min`) - `inst-ds-repin`

Runs at least once per minute per shard; after an outage the same loop catches up every due slot in bounded transactions (a 30-day outage of an hourly line is five transactions for that schedule, not one).

### 2.3 Wake children and fact heads by time

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-schedule-wake`

**Actor**: `cpt-cf-bss-rating-actor-rating` (`ChildWakeScanner`, leader of one shard, independent of the creation cursor)

**Success Scenarios**: every `pending`, `provisional` or `final_stale` child with `next_check_at ≤ now`, every fact head with `rollup_due_at ≤ now` and every usage fact head whose binding is `pending(derived_declaration_unavailable)` with `binding_next_check_at ≤ now` is enqueued, and its next check is set in the same transaction.

**Error Scenarios**: crash before commit (times not advanced; rescanned).

**Steps**:
1. [ ] - `p1` - BEGIN; select up to 500 due children of the shard through the partial index `(shard, next_check_at)`, `FOR UPDATE SKIP LOCKED` - `inst-wk-children`
2. [ ] - `p1` - Enqueue each child and set `next_check_at = now + backoff` (§3.5) - `inst-wk-child-next`
3. [ ] - `p1` - Select up to 500 fact heads of the shard with `rollup_due_at ≤ now` through `(shard, rollup_due_at)`, `FOR UPDATE SKIP LOCKED`; enqueue each on `rating.rollup` and set `rollup_due_at = backoff(h)` = `now + min(6 h, max(15 min, now − served_to))`, so the wait roughly doubles with each check that finds the fact not ready, with no attempt counter; roll-up clears it when it delivers - `inst-wk-heads`
4. [ ] - `p1` - Select up to 500 fact heads of the shard with `binding_state = pending` and `binding_next_check_at ≤ now` through `(shard, binding_next_check_at)`, `FOR UPDATE SKIP LOCKED`; enqueue each on `rating.fact_bind` and set `binding_next_check_at = now + min(5 min, max(10 s, now − received_at))`, `received_at` being the inbox receipt of the head's current version, so the wait doubles without a counter; COMMIT - `inst-wk-bind`

Event wake-ups (a counter change, a fact version, a segment past the scope's sequence, a sealed scope, a coverage declaration, a resolved source loss) are enqueued by the transaction that stores the input; this flow covers time alone ([14 §3.6](#contract-14-3-6-f06)).

### 2.4 Write a finalization policy

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-schedule-write-policy`

**Actor**: `cpt-cf-bss-rating-actor-platform-operator` (a seller operator for seller policies; a platform operator for platform defaults)

**Success Scenarios**: a new policy version is stored, insert-only; schedules already pinned keep the version they pinned; `policy_pending` schedules it matches are pinned by the scheduler (at once for a seller policy, within 15 minutes for a platform default).

**Error Scenarios**: `401`/`403`; `400` when a seller request names `owner_scope = platform`; `409 AlreadyExists` for the same `Idempotency-Key` with a different body; `400 InvalidArgument` for `delay_only` without `delay_only_acceptance_ref` (V55); `400 FailedPrecondition` when the reference names no recorded acceptance of this tenant and window policy, or one the requester recorded (R-21).

**Steps**:
1. [ ] - `p1` - Receive `POST /bss-rating/v1/finalization-policies` (`finalization_policy × write`) or `POST /bss-rating/v1/platform/finalization-policies` (`platform_finalization_policy × write`) with a required `Idempotency-Key` - `inst-wp-receive`
2. [ ] - `p1` - Authorize through the PDP and record the operation key in `bss_rating__operation`; a retry returns the original response - `inst-wp-authorize`
3. [ ] - `p1` - **IF** seller route, write `owner_scope = seller` with the caller's seller tenant; **IF** platform route, write through the dedicated platform repository with `tenant_id = rating.platform_tenant_id` - `inst-wp-scope`
4. [ ] - `p1` - **IF** `evidence_mode = delay_only`: **IF** the body has no `delay_only_acceptance_ref`, return `400 InvalidArgument`; **IF** the reference names no `bss_rating__delay_only_acceptance` row in the same tenant (the platform tenant on the platform route) and for the same window policy, or one whose `accepted_by` is the requester, return `400 FailedPrecondition` (R-21, V55) - `inst-wp-delay-only`
5. [ ] - `p1` - Insert the new version (`policy_id`, `version`, `window_policy` (a `rating_window` value: `billing_cycle` or `calendar_hour`), `delay`, `evidence_slo`, `evidence_mode`, `delay_only_acceptance_ref`, `effective_from`); audit actor and request - `inst-wp-insert`
6. [ ] - `p1` - **IF** seller route, set `next_due_at = now` on the writer tenant's `policy_pending` schedules of the same window policy in the same transaction, so the next scan pins them (§2.2 `inst-ds-repin`); a platform default writes no other tenant's rows, and their waiting schedules pick it up at the next retry, within 15 minutes. A schedule pinned this way had no deadline before, so pinning it moves none; schedules already pinned keep their version - `inst-wp-wake`
7. [ ] - `p1` - **RETURN** `201` with the version - `inst-wp-return`

A Finance/Product approver records the acceptance first with `POST /bss-rating/v1/finalization-policies/delay-only-acceptances` (`finalization_policy × accept_delay_only`, audited) or, for a platform default, `POST /bss-rating/v1/platform/finalization-policies/delay-only-acceptances` (`platform_finalization_policy × accept_delay_only`, audited); it writes a `bss_rating__delay_only_acceptance` row, whose `acceptance_id` is the reference.

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

### 3.1 Validate a fact version against the profile

- [x] `p1` - **ID**: `cpt-cf-bss-rating-algo-schedule-validate-fact`

**Input**: a `BillableFactPublished` payload of the `subscriptions-rating/1` profile (Subscriptions PR #5516; T-D-87). Rating reads no `BillableItemCreated`: the MIGRATION mappings of the unversioned CURRENT fact and of a derived usage parent are withdrawn, since Subscriptions versions every fact and publishes the usage parent at period opening (R-03, R-20).

**Output**: the fact version Rating stores, or the reason it is quarantined.

1. [x] - `p1` - **IF** any instant of the fact — the period, the served extent, a slice bound, a suspension bound, `BillingTerms.anchor_at` — has a sub-second part, quarantine `instant_not_whole_second`; never truncate it (DESIGN 09 §4.1, V60) - `inst-vf-whole-seconds`
2. [x] - `p1` - Check the timeline: the period is half-open and contains the served extent; the slices are ordered, half-open and partition the served extent with no gap and no overlap; an `active` obligation has at least one slice and a `zeroed` one has none - `inst-vf-timeline`
3. [x] - `p1` - Check the slices: a `chargeable` slice names at least one accepted term the fact carries, at most one per dimension value; each lot of a chargeable slice names one of the slice's terms and a positive quantity in canonical decimal text (≤ 60 integer and 28 fractional digits); an `unresolved` slice names its reason and may keep lots without terms; a `nonchargeable` slice names its reason - `inst-vf-slices`
4. [x] - `p1` - Check each accepted term: its binding is for the fact's item and charge kind, and its `money_digest` and `template_digest` verify, so its `selected_bindings_digest` can be computed - `inst-vf-terms`
5. [x] - `p1` - **IF** any check of steps 2–4 fails, quarantine `payload_invalid` naming the first failure; the inbox keeps the payload for an operator - `inst-vf-invalid`

Canonical: DESIGN 11 §4.3 and [14 §4.2](../DESIGN.md#contract-14-4-2); T-D-87.

### 3.2 Resolve and pin the finalization policy

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-schedule-pin-policy`

**Input**: a usage fact (its seller `tenant_id` and the binding's `rating_window`: `BillingCycle` or `CalendarHour`).

**Output**: `(policy_id, version, owner_scope, delay, evidence_mode)` pinned into the window schedule, or the schedule `policy_pending` with the exception `finalization_policy_missing`.

1. [ ] - `p1` - Under a scope narrowed to the fact's tenant, select the effective seller policy for the `rating_window`: the latest version whose `effective_from` is at or before the pin instant (the fact's intake for a schedule pinned at intake; the successful retry for a `policy_pending` one) - `inst-pp-seller`
2. [ ] - `p1` - **IF** none, select the platform default through the read-only platform repository method that returns only `owner_scope = platform` rows, bound to the `AccessScope` the PDP compiles for `rating.platform_tenant_id` - `inst-pp-platform`
3. [ ] - `p1` - **IF** none, create the schedule `policy_pending` (no policy, no deadline, no due child; its provisional children wait `pending(finalization_policy_missing)`) and open the exception `finalization_policy_missing`; the scheduler retries the pin every 15 minutes and a seller policy write wakes it at once (§2.2 `inst-ds-repin`, §2.4 `inst-wp-wake`); the first successful pin sets the deadlines and closes the exception (V80) - `inst-pp-missing`
4. [ ] - `p1` - Check that the policy's `tenant_id` equals the fact's or its scope is `platform`; pin `(policy_id, version, owner_scope)`, `delay` and `evidence_mode` into the schedule - `inst-pp-pin`
5. [ ] - `p1` - Never re-pin on a later fact version or policy version: a pinned deadline never moves. Pinning a `policy_pending` schedule is its first pin, not a re-pin: it had no deadline to move - `inst-pp-never-repin`

`evidence_mode = full` is the default; `delay_only` exists only per policy with the recorded Finance/Product acceptance of R-21 and never bypasses an open source loss (T-D-62).

### 3.3 Compute the expected children of a fact

- [x] `p1` - **ID**: `cpt-cf-bss-rating-algo-schedule-expected-children`

**Input**: a fact version: its kind, served extent, period, slices and the accepted bindings they name (each usage binding with its `usage_rating_policy` and derived `meter`).

**Output**: the expected child keys and `expected_manifest_digest` on the fact head.

1. [x] - `p1` - A `period_line` fact has exactly one child. A `one_time` fact has no child while R-19 is open, because it is stored, not rated and never rolled up, so no zero is delivered; it has exactly one once R-19 is accepted (step `inst-af-one-time`). A `zeroed` version expects no child: its result is a complete empty replacement (SUB-D-45, V84) - `inst-ec-single`
2. [x] - `p1` - Usage, `subscription_line` scope: per cell (each dimension value of the fact's item that a chargeable slice binds), one child per window of `window_geometry(rating_window, served extent)` — the billing period for `BillingCycle`, each UTC hour for `CalendarHour{Utc}` — that holds at least one chargeable or unresolved second, keyed `wk1` with the binding's derived `meter` (`dimension_key = ""` at launch, R-16). A window touching an `unresolved` slice is expected and waits at evaluation (V85); one entirely inside `nonchargeable` slices is not expected, so a fact whose served extent is all nonchargeable expects no usage child, while one with unresolved time but no chargeable term expects one child over the billing period that waits for a version naming the terms. When the bindings disagree on the policy or meter, each cell has one child over the billing period with `meter = ""`, created `failed(usage_policy_mismatch)` (V86) - `inst-ec-usage`
3. [x] - `p1` - There is no per-event window (pricing offers only `BillingCycle` and `CalendarHour`, T-D-75), so no `ek1` child is ever created - `inst-ec-event`
4. [x] - `p1` - Record the expected set as a manifest digest on the head once the fact is bound (a head that is not `bound` has no manifest and is never rolled up); never derive it from the counters that happen to exist - `inst-ec-manifest`

Canonical: DESIGN §3.1 "Expected children of a fact"; `resource` scope is expressible in pricing (R-23 closed) but LAUNCH-GATED until scope proofs list resources (T-D-75).

### 3.4 Apply a newer fact version

- [x] `p1` - **ID**: `cpt-cf-bss-rating-algo-schedule-new-version`

**Input**: a fact version higher than the head's previous `current_version`.

**Output**: every child of the fact marked for re-evaluation; the expected manifest updated.

1. [x] - `p1` - Under each child's row lock, increment `input_generation`; a `final` child becomes `final_stale`. The new version's accepted bindings are stored (§2.1 step 4); a child takes its binding of record from the slices of the version it evaluates, so a child whose slices name the same bindings re-evaluates to the same digest (T-D-87) - `inst-nv-bump`
2. [x] - `p1` - Update `expected_manifest_digest`; for the usage schedule update only `fact_version`, never the pinned policy - `inst-nv-manifest`
3. [x] - `p1` - Never delete a child. A shortened served extent, a quantity change, a cut at a price's `ends_on` or a removal arrives as a new full version (Subscriptions SUB-D-21 as rewritten by PR #5516; SUB-D-32): a child no longer expected stays, and its next evaluation gives it an empty result, as a `zeroed` version's roll-up gives a complete empty replacement (SUB-D-45, V84). The stored subscription version no longer clips a child (T-D-82 superseded by T-D-87) - `inst-nv-no-delete`
4. [x] - `p1` - Enqueue the children - `inst-nv-enqueue`

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
1. [x] - `p1` - **FROM** — **TO** `pending` **WHEN** fact intake creates a `period_line` child or the scheduler creates a due expected child - `inst-cc-pending`
2. [ ] - `p1` - **FROM** — **TO** `provisional` **WHEN** fact intake creates the child of a window that already has counters (`next_check_at = window_end + delay`) - `inst-cc-provisional`
3. [x] - `p1` - **FROM** — **TO** `failed(resource_scope_not_supported)` **WHEN** the binding's `aggregation_scope` is `resource` (LAUNCH-GATED, T-D-75), or `failed(usage_policy_mismatch)` **WHEN** the usage bindings of one fact disagree on the policy or meter (T-D-87) - `inst-cc-gated`
4. [x] - `p1` - **FROM** `final` **TO** `final_stale` **WHEN** a newer fact version bumps `input_generation` - `inst-cc-stale`
5. [ ] - `p1` - **FROM** any non-terminal state **TO** `failed` **WHEN** its work item is dead-lettered after 10 attempts - `inst-cc-deadletter`

### 4.2 Window schedule cursor

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-state-schedule-cursor`

**States**: `policy_pending`, `scheduling`, `complete` — the `state` column of `bss_rating__window_schedule` (DESIGN §3.7).

**Initial State**: `scheduling` when a finalization policy pins at intake, with `next_due_window_start` at the first window of the served extent and `next_due_at` at its end plus the delay; else `policy_pending`.

**Transitions**:
1. [ ] - `p1` - **FROM** `policy_pending` **TO** `scheduling` **WHEN** a policy now matches and the scheduler pins it (its first pin; no deadline existed to move) - `inst-sc-pinned`
2. [ ] - `p1` - **FROM** `scheduling` **TO** `scheduling` **WHEN** a due scan commits; `next_due_window_start` and `next_due_at` only move forward, in the transaction that creates the children - `inst-sc-advance`
3. [ ] - `p1` - **FROM** `scheduling` **TO** `complete` **WHEN** the cursor passes the fact's `served_to` - `inst-sc-complete`

## 5. Definitions of Done

### 5.1 Versioned fact intake

- [x] `p1` - **ID**: `cpt-cf-bss-rating-dod-schedule-fact-intake`

The system **MUST** accept every fact through the inbox in one transaction per fact version, validate it against Subscriptions' `subscriptions-rating/1` profile and quarantine any version it cannot hold exactly, store fact versions insert-only with an ordered head, store each accepted binding the fact carries as the binding of record (T-D-87), create the children of recurring facts and the schedule and expected manifest of usage facts from the accepted binding's `UsageRatingPolicy`, reject `resource`-scoped (LAUNCH-GATED) and inconsistent usage policies, and apply a newer version to every child of the fact without deleting any. Its `PricingBindingStore` **MUST** serve the freshness proof: `resolve` with the accepted pins as `bss-rating.system` in the seller tenant's context, each answer stored once per distinct answer and once per query and UTC day.

**Implements**: `cpt-cf-bss-rating-flow-schedule-accept-fact`, `cpt-cf-bss-rating-algo-schedule-validate-fact`, `cpt-cf-bss-rating-algo-schedule-expected-children`, `cpt-cf-bss-rating-algo-schedule-new-version`, `cpt-cf-bss-rating-state-schedule-child-creation`, `cpt-cf-bss-rating-flow-period-tick-syn`.

**Constraints**: `cpt-cf-bss-rating-constraint-no-commercial-clock-syn`, `cpt-cf-bss-rating-constraint-synthesis-only-syn`, `cpt-cf-bss-rating-constraint-in-process-contracts`.

**Touches**: `cpt-cf-bss-rating-dbtable-inbox`, `cpt-cf-bss-rating-dbtable-fact`, `cpt-cf-bss-rating-dbtable-fact-head`, `cpt-cf-bss-rating-dbtable-child-window`, `cpt-cf-bss-rating-dbtable-window-schedule`, `bss_rating__pricing_binding`, `bss_rating__pricing_resolve`, `bss_rating__derived_declaration`, `bss_rating__usage_policy_projection`; `PricingReadV1::resolve` (as `bss-rating.system` with a per-work-item context for the seller tenant, pricing `plan:read`, D-523); the Products derived-declaration read (R-31); queues `rating.child_work`, `rating.rollup`, `rating.fact_bind`.

### 5.2 Rating-owned finalization policy

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-schedule-policy`

The system **MUST** resolve the most specific finalization policy (seller, then platform default) at fact intake, pin its id, version, owner scope, delay and evidence mode into the window schedule, keep a schedule with no matching policy `policy_pending` (exception `finalization_policy_missing`) and pin it once a policy exists, serve the seller and platform write routes with their separate permissions and repositories, and never move a pinned deadline.

**Implements**: `cpt-cf-bss-rating-algo-schedule-pin-policy`, `cpt-cf-bss-rating-flow-schedule-write-policy`.

**Constraints**: `cpt-cf-bss-rating-constraint-pin-discipline-syn`.

**Touches**: REST `bss_rating.finalization_policies.list`, `bss_rating.finalization_policies.create`, `bss_rating.finalization_policies.accept_delay_only`, `bss_rating.platform_finalization_policies.accept_delay_only`, `cpt-cf-bss-rating-dbtable-delay-only-acceptance`, `bss_rating.platform_finalization_policies.create`; `cpt-cf-bss-rating-dbtable-finalization-policy`, `cpt-cf-bss-rating-dbtable-window-schedule`, `bss_rating__operation`.

### 5.3 Durable scheduling, wake-ups and coalescing

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-schedule-durable`

The system **MUST** bind a usage fact before creating its schedules, manifest or roll-up time and retry a pending binding durably, create every expected child of a due window in the transaction that advances the schedule cursor, scan only through stored `shard` and due-time columns with an index each, catch up every due slot after downtime in bounded transactions, wake children and fact heads from `next_check_at` and `rollup_due_at` independently of the creation cursor, coalesce work items on `queued_at`, and dead-letter a child after 10 failed attempts; correctness rests on the cursor CAS, `SKIP LOCKED` and the unique `child_id`, never on leadership.

**Implements**: `cpt-cf-bss-rating-flow-schedule-due-scan`, `cpt-cf-bss-rating-flow-schedule-wake`, `cpt-cf-bss-rating-algo-schedule-coalesce`, `cpt-cf-bss-rating-state-schedule-cursor`, `cpt-cf-bss-rating-flow-scheduler-syn`, `cpt-cf-bss-rating-flow-cascade-route-syn`, `cpt-cf-bss-rating-algo-cascade-routing-syn`.

**Constraints**: `cpt-cf-bss-rating-constraint-no-commercial-clock-syn`.

**Touches**: `cpt-cf-bss-rating-dbtable-window-schedule` (`state`, `shard`, `next_due_at`), `cpt-cf-bss-rating-dbtable-child-window` (`shard`, `next_check_at`, `queued_at`), `cpt-cf-bss-rating-dbtable-fact-head` (`shard`, `rollup_due_at`, `binding_state`, `binding_next_check_at`); queue `rating.fact_bind`; Cluster elections `scheduler-{shard}`.

### 5.4 Fact recovery and watchdog

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-dod-schedule-watchdog`

The system **MUST** recover missing facts through `period_facts` into the same inbox once that read exists (TARGET) and, until then (CURRENT), alarm an active line without a fact 24 h after its period start, without ever fabricating a commercial period.

**Implements**: `cpt-cf-bss-rating-algo-schedule-fact-watchdog`.

**Constraints**: `cpt-cf-bss-rating-constraint-no-commercial-clock-syn`.

**Touches**: `SubscriptionBillingReadV1::period_facts` (TARGET); `cpt-cf-bss-rating-dbtable-fact`; Cluster leadership `reconcile-{shard}` (the `Reconciler`, DESIGN §3.6 Flow C).

Observability: this feature emits `rating_fact_versions_total`, `rating_fact_missing`, `rating_usage_without_fact`, `rating_scheduler_backlog{shard}`, `rating_child_wake_lag_seconds{child_kind}` and the `rating.inbox.accept` span (DESIGN §4.7).

## 6. Acceptance Criteria

- [ ] A fact redelivered with the same id, version and digest is a no-op; the same id and version with another digest is quarantined `fact_digest_conflict`.
- [ ] A lower fact version arriving after a higher one is stored and its inbox entry `applied`; it stores no binding and changes no schedule, child or manifest (V81).
- [ ] A newer fact version bumps `input_generation` of every child of the fact, turns `final` children `final_stale` and deletes none.
- [ ] An advance recurring fact's `period_line` child is enqueued at acceptance with no further event.
- [ ] A fact version with a sub-second instant is quarantined `instant_not_whole_second` (V60); one whose slices leave a gap or overlap, whose chargeable slice names no term, whose lot names a term the fact does not carry, or whose binding's `money_digest` does not verify is quarantined `payload_invalid`; neither creates a child.
- [ ] A usage fact with no seller or platform policy for its `rating_window` gets a `policy_pending` schedule and the exception `finalization_policy_missing`; it is pinned once a policy exists (V80); a policy version written after a schedule is pinned does not change its delay.
- [ ] A usage fact whose derived declaration cannot be read is stored with `binding_state = pending(derived_declaration_unavailable)` and no schedule, manifest or `rollup_due_at`; it binds when the read recovers and every due child is then created (V77).
- [ ] A seller operator cannot read another seller's policy; a platform default is visible read-only.
- [ ] A scheduler leader that loses its lease mid-batch does not advance the cursor; the next leader creates the same children with no duplicates.
- [ ] An enqueue while `queued_at` is set adds no work item; a work item created during a run is enqueued again; a child whose work item fails 10 times is `failed` with an exception.
- [ ] With no `period_facts` read (CURRENT), an active line with no fact 24 h after period start raises `rating_fact_missing`.
- [ ] Each accepted binding a fact version carries is stored once under its `binding_digest` (pricing's `selected_bindings_digest` of the cell) and is the binding of record of the slices that name it; intake calls no Pricing read. The freshness proof resolves with the accepted pins on the slice start and each `ends_on`, as `bss-rating.system` in the seller tenant's context (V59), stores the question per UTC day and the answer once (V82), and reports a discrepancy instead of pricing a successor (V78, V79).
- [ ] A window touching an `unresolved` slice is an expected child that waits `pending(terms_unresolved)` at evaluation, never priced as zero (V85); usage bindings of one fact that disagree on `rating_window`, `aggregation_scope` or `meter` give one `failed(usage_policy_mismatch)` child per cell over the billing period (V86); a `zeroed` version expects no child and keeps every earlier one (V84).
- [ ] A `one_time` fact has no expected child and is never rolled up while R-19 is open and Subscriptions' S-U08/S-U13 gates are closed (T-D-18, T-D-87).
- [ ] A child's first final requires a freshness answer fetched on or after the UTC day of its window's last second that names the fact's binding (V78, V79); two periods with the same answer are two query rows (V82).
- [ ] Every UUIDv5 name with an instant uses the canonical instant text (`2026-09-15T00:00:00Z`).
- [ ] A `CalendarHour{Utc}` usage binding yields 744 children for October; a `BillingCycle` binding yields one child for the billing period.

Acceptance vectors owned by this feature (DESIGN §4.13 index):

| # | Scenario | Expected state |
|---|---|---|
| V14 | Arrears recurring fact with no further upstream event | final at `served_to` via the wake scanner |
| V15 | Evidence arrives before the window's deadline | evaluated at `window_end + delay` |
| V17 | Retired: pricing offers no per-event window (T-D-75) | — |
| V25 | A seller operator posts a platform-default policy | `400`; no row |
| V59 | The freshness resolve of seller A's fact runs with `CatalogRef.tenant_id` = A; a work item of seller B that names `CatalogRef.tenant_id` = A (Pricing fake enforcing D-523) | **Rating**: A's resolve succeeds and is stored; B's is refused `403 PermissionDenied`, which DESIGN 11 §4.2 maps like every 4xx to `pricing_refused(PERMISSION_DENIED)`; nothing is stored for B and Rating never retries it with A's context |
| V60 | A fact version whose period starts `2026-11-01T00:00:00.5Z` (a sub-second `BillingTerms.anchor_at`); the same fact with a quantity change at `2026-11-15T12:00:00.250Z` | **Rating**: each fact version is quarantined `instant_not_whole_second` and gets no child, nothing delivered; neither instant is truncated (DESIGN 09 §4.1) |
| V55 | `POST /finalization-policies` with `evidence_mode = delay_only` and no `delay_only_acceptance_ref` | `400 InvalidArgument`; no policy version is written; a stored `delay_only` row without a reference is impossible (`ck_…__delay_only_accepted`); a reference to an acceptance the requester recorded, or of another window policy, is `400 FailedPrecondition` |
| V77 | Products' derived-declaration read is unavailable at the intake of an hourly cloudlet usage fact on 1 October 00:00 and recovers at 02:00 | the fact is stored and its inbox entry `applied`; the head is `pending(derived_declaration_unavailable)` with no schedule, manifest or `rollup_due_at`; the `ChildWakeScanner` re-enqueues it on `rating.fact_bind` until the read succeeds; then the projection, schedule, manifest and `rollup_due_at = served_to` are written and the scheduler catches up every due hour; at `served_to` roll-up rates all 744 hours, never an empty manifest or a zero |
| V78 | An hourly usage fact v1 carries one chargeable slice for October with no `ends_on`; on 10 October the operator ends the price effective 20 October (`Change::End`) and Subscriptions publishes v2, cut at 20 October with the authorized successor binding (SUB-D-41) | v2 bumps every child; children before 20 October keep the original binding, children from 20 October take the successor's from v2's slice, and the `BillingCycle` child of the same line holds two slices; a child that reaches its first final before v2 arrives and whose freshness answer already shows `ends_on = 20 October` inside its window stays `pending(terms_stale)` with a reconciliation exception, never priced by the successor Rating found itself; no final child changes without a new version |
| V79 | A temporary price ends on 10 October and Subscriptions' fact cuts its slice there, with the successor binding from 10 October; the hourly child of 20 October 15:00 | its binding of record is the successor slice's binding; it is never priced at the promotional price; were the cut missing (no successor authority), the fact carries an `unresolved` slice from 10 October and the child waits (V85) |
| V80 | A fresh install: an hourly usage fact is accepted before any seller or platform policy exists; a platform default is written two hours later | the schedule is `policy_pending` with the exception `finalization_policy_missing`; no due child is created and provisional children wait `pending(finalization_policy_missing)`; within 15 minutes of the write the schedule is pinned to that version (`effective_from` at or before the pin), becomes `scheduling`, catches up the due hours with `window_end + delay` deadlines, and the exception closes; nothing was finalized before the pin |
| V81 | Fact version 1 is redelivered after version 2 was applied | v1 is stored and its inbox entry `applied`; the head stays at version 2; no resolve, no schedule `fact_version` change, no child bump, no manifest change |
| V82 | September and October periods of one line with the same pin, for which Pricing returns the same answer | two `bss_rating__pricing_resolve` rows (two `resolve_date`s) pointing at one `bss_rating__pricing_binding` row; October's lookup finds its own row before calling Pricing; no same-key alarm; each month's children record their own resolve |
| V83 | **Joint fixture with Subscriptions**: an advance monthly per-unit recurring line with lot A = 10 seats; on 15 October purchase B adds 2 seats under its own binding, and Subscriptions publishes v2 of the same fact: `[1 Oct, 15 Oct)` A = 10, `[15 Oct, 1 Nov)` A = 10 and B = 2 (one slice, two lots) | v2 bumps the `period_line` child, whose next evaluation prices both slices in one result revision; October bills 10 seats for the first half and 12 for the second, never 10 + 22; Billing receives one full replacement, not a delta (T-D-87; Subscriptions SUB-D-46) |
| V84 | A recurring fact v2 is published `zeroed` (the obligation removed) after v1's child was final | v2 bumps the child to `final_stale`; v2 expects no child and the manifest is empty; nothing is deleted; roll-up (feature 09) delivers a complete empty replacement for v2, never a zero-priced line (SUB-D-45) |
| V85 | An hourly usage fact whose timeline is chargeable for `[1 Oct, 10 Oct)` and `unresolved(continuation_missing)` from 10 October (SUB-D-42) | the fact is stored and bound; every October hour is an expected child; the children from 10 October wait `pending(terms_unresolved)` at evaluation and are never priced at zero or at the earlier price; a later version that resolves the slice bumps them |
| V86 | A usage fact whose slices bind the cell to a `CalendarHour{Utc}` policy before 15 October and a `BillingCycle` policy after it | one child over the billing period `failed(usage_policy_mismatch)` with an exception, no schedule and no hourly child; nothing is delivered until a version with one policy arrives |

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
  **Fact intake** (one transaction per fact version; the `subscriptions-rating/1` profile, T-D-87):

  ```text
  inbox entry source=fact business_id=fact_id version=fact_version digest=d (Rating's, over the payload)
  validate (before BEGIN): whole seconds, timeline partition, slices, lots, terms (§3.1)
                       sub-second → quarantine instant_not_whole_second; otherwise invalid → payload_invalid
  declarations (before BEGIN, usage): decl = DerivedDeclarationStore.ensure(meter)  -- Products, R-31
                       unavailable → decl = pending(derived_declaration_unavailable)
                       digest ≠ stored copy → decl = failed(derived_declaration_conflict)
  BEGIN
    INSERT bss_rating__inbox … ON CONFLICT DO NOTHING            same key, digest ≠ d → quarantine; COMMIT
    INSERT bss_rating__fact (fact_id, fact_version, timeline, accepted terms, …) ON CONFLICT DO NOTHING
    UPSERT bss_rating__fact_head (shard = hash(fact_id) mod S on insert)
           SET current_version = max(current_version, fact_version)
    fact_version < head.current_version → INSERT bss_rating__inbox state = applied; COMMIT  -- stored only (V81)
    for each accepted term: INSERT bss_rating__pricing_binding (binding_digest = selected_bindings_digest
                           of the one cell) ON CONFLICT DO NOTHING      -- same digest, other content: alarm
    kind = recurring → INSERT child period_line (wk1|fact|period_start|agg) ON CONFLICT DO NOTHING
                       next_check_at = timing = advance ? now : served_to   -- the fact's timing
    kind = one_time  → R-19 accepted ? INSERT child one_time : store only    -- no manifest, no roll-up
    kind = usage, decl not stored
                     → UPDATE bss_rating__fact_head SET binding_state = decl.state, binding_reason = decl.reason,
                         binding_next_check_at = (pending ? now + 10 s : NULL)
                       failed → open exception decl.reason
                       -- no schedule, no manifest, no rollup_due_at: nothing can roll up an incomplete fact
    kind = usage, decl stored
                     → usage bindings disagree on rating_window, aggregation_scope or meter
                         → one child per cell over the period failed(usage_policy_mismatch), exception; no schedule
                       aggregation_scope = resource
                         → children failed(resource_scope_not_supported), exception; no schedule  -- LAUNCH-GATED
                       write bss_rating__usage_policy_projection from the bindings and decl
                       INSERT bss_rating__window_schedule
                         (tenant_id, fact_id, item_id, shard; window = the binding's rating_window;
                          policy pinned now → state = scheduling, next_due_window_start = first window,
                          next_due_at = its end + delay; no policy → state = policy_pending,
                          next_due_at = now + 15 min, exception finalization_policy_missing)
                         ON CONFLICT (tenant_id, fact_id, item_id) DO UPDATE SET fact_version = excluded.fact_version
                           WHERE bss_rating__window_schedule.fact_version < excluded.fact_version
                                                                     -- policy never re-pinned
                       INSERT provisional children for windows that already have counters (F35),
                         next_check_at = window_end + delay (NULL while policy_pending)
                         ON CONFLICT DO NOTHING
                       UPDATE bss_rating__fact_head SET binding_state = bound, bound_version = fact_version,
                         expected_manifest_digest = …   -- windows with a chargeable or unresolved second
    usage bound or recurring arrears → UPDATE bss_rating__fact_head SET rollup_due_at = served_to
    newer version    → UPDATE bss_rating__child_window SET input_generation += 1,
                              status = CASE status WHEN final THEN final_stale ELSE status END
                        WHERE fact_id = ?   -- under each child's row lock; every child of the fact,
                                            -- since a parent revision is per fact version ([contract 15](../DESIGN.md#contract-15))
                       UPDATE bss_rating__fact_head SET expected_manifest_digest = …
    enqueue affected children; INSERT bss_rating__inbox state = applied
  COMMIT
  ```

  **Re-bind a pending usage fact** (`FactIntake`, from `rating.fact_bind`; the `ChildWakeScanner`
  enqueues a head whose `binding_state = pending` and `binding_next_check_at ≤ now`):

  ```text
  h = SELECT current_version, binding_state FROM bss_rating__fact_head        -- no lock
  decl = DerivedDeclarationStore.ensure(meter of h.current_version)          -- outside tx
  BEGIN
    SELECT … FROM bss_rating__fact_head WHERE (tenant_id, fact_id) = … FOR UPDATE
    current_version ≠ h.current_version or binding_state ≠ pending → ROLLBACK  -- the newer version binds itself
    decl not stored → UPDATE binding_state/reason, binding_next_check_at (backoff); COMMIT
    decl stored     → the usage branch above: projection, schedule, provisional children, manifest,
                      binding_state = bound, rollup_due_at = served_to; enqueue; COMMIT
  ```

  **Freshness proof** (`PricingBindingStore`, called by the rater before a child's first final,
  [feature 07](07-child-evaluation.md); T-D-87): for each slice of the child, `resolve(revision_id,
  slice start, the slice's accepted pins)` and again on each `ends_on` inside the child's window, as
  `bss-rating.system` in the seller tenant's context (D-523), using a stored answer fetched on or
  after the UTC day of the window's last second, else calling Pricing; the answer must name the
  binding the fact carries for the slice. A different binding or an `ends_on` the fact did not cut
  at leaves the child `pending(terms_stale)` with an exception that requests reconciliation from
  Subscriptions; Rating never prices the successor itself (SUB-D-41).

  Example (recurring): subscription S42, period `[2026-09-15, 2026-10-15)`, component `plan#1`,
  item `I-REC`, v1 with one chargeable slice naming the accepted binding of `P-REC` and lot `L1 = 3`
  → one `period_line` child keyed `wk1|{fact_id}|2026-09-15T00:00:00Z|{agg}` (the canonical instant
  text of DESIGN §3.1), its binding of record `P-REC`'s `binding_digest`; the freshness proof before
  its first final is `resolve(revision_id, 2026-09-15, pins = [P-REC])`.

**Contract**: `cpt-cf-bss-rating-flow-scheduler-syn` (`p1`), defined in [§2 migrated flows](#register-flows).
  **Due scan** (`WindowScheduler`, every ≤ 60 s per shard, batches of ≤ 500 schedules):

  ```text
  BEGIN
    SELECT … FROM bss_rating__window_schedule
     WHERE shard = ? AND state = 'scheduling' AND next_due_at <= now()   -- index (shard, next_due_at)
     ORDER BY next_due_at LIMIT 500 FOR UPDATE SKIP LOCKED
    n = 0
    for s: k_s = 0
           for each window w from s.next_due_window_start while w.end + s.delay <= now()
                                                      and w.start < fact.served_to
                                                      and k_s < 168 and n < 5000:
             for each aggregation key k of s.item in expected_children(fact, w) (DESIGN §3.1):
               INSERT bss_rating__child_window (wk1|s.fact|w.start|k, shard = s.shard)
                 ON CONFLICT DO NOTHING
               enqueue child (notification); n += 1
             k_s += 1
           UPDATE s SET next_due_window_start = <first window not yet created>,
                        next_due_at = <that window's end> + s.delay,     -- a month or a year for BillingCycle
                        state = <complete once next_due_window_start ≥ fact.served_to>
    SELECT … WHERE shard = ? AND state = 'policy_pending' AND next_due_at <= now() … FOR UPDATE SKIP LOCKED
    for p: pin §3.2 → state = scheduling, next_due_at = first window's end + delay;
           else next_due_at = now() + 15 min
  COMMIT
  ```

  After an outage the same loop catches up every due slot, at most 168 windows per schedule and
  5 000 children per transaction; a schedule still due stays selected, so the next scan continues
  it. The unique `child_id` absorbs re-runs (F28). For `subscription_line` scope the expected keys are one per usage binding of the line (its
  derived `meter`, the rated cell the accepted binding names at launch, R-16), so an hour with no usage still gets its
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
        WHERE shard = ? AND next_check_at <= now()          -- partial index (shard, next_check_at):
        ORDER BY next_check_at LIMIT 500 FOR UPDATE SKIP LOCKED  -- pending, provisional, final_stale
       for c: enqueue child; UPDATE c SET next_check_at = now() + backoff(c)
       SELECT fact_id FROM bss_rating__fact_head
        WHERE shard = ? AND rollup_due_at <= now()          -- partial index (shard, rollup_due_at)
        LIMIT 500 FOR UPDATE SKIP LOCKED
       for h: Outbox.enqueue(rating.rollup, fact_id)
              UPDATE h SET rollup_due_at = now() + least(6 h, greatest(15 min, now() - h.served_to))
                                                            -- backoff(h): doubles per check, no counter
       SELECT fact_id FROM bss_rating__fact_head
        WHERE shard = ? AND binding_state = 'pending' AND binding_next_check_at <= now()
        LIMIT 500 FOR UPDATE SKIP LOCKED                    -- partial index (shard, binding_next_check_at)
       for h: Outbox.enqueue(rating.fact_bind, fact_id)
              UPDATE h SET binding_next_check_at = now() + least(5 min, greatest(10 s, now() - received_at))
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
