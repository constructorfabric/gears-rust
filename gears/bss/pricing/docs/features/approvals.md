<!-- CONFLUENCE_TITLE: [BSS]: Pricing — Approvals (Feature) -->
<!-- Related: ../DECOMPOSITION.md, ../DESIGN.md, ../PRD.md | Owners: BSS Pricing team -->

# Feature: Approvals

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-featstatus-approvals-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-bss-pricing-feature-approvals`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Review a selected batch](#review-a-selected-batch)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [submit-unit](#submit-unit)
  - [vote-and-refresh](#vote-and-refresh)
  - [apply-prices](#apply-prices)
  - [withdraw-unit](#withdraw-unit)
- [4. States (CDSL)](#4-states-cdsl)
  - [Approvals states](#approvals-states)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Selected price batch](#selected-price-batch)
  - [Transactional price apply](#transactional-price-apply)
  - [Separation from every author](#separation-from-every-author)
  - [Copied quorum including zero](#copied-quorum-including-zero)
  - [Committed refresh of stale content](#committed-refresh-of-stale-content)
  - [Generation-scoped votes](#generation-scoped-votes)
  - [Unit contention and environmental refusal](#unit-contention-and-environmental-refusal)
  - [Every terminal outcome is attributable](#every-terminal-outcome-is-attributable)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

**Delivery:** phase 2c for prices; 3 for the other subjects. Every checkbox is an implementation obligation, not an assertion about the legacy code.

This feature implements [slice 05](../design/05-approvals.md).
[DECOMPOSITION](../DECOMPOSITION.md) records integration order; [DESIGN §3](../DESIGN.md#3-technical-architecture)
is the schema and transaction authority. Unchecked phase 3/4 work is not part of the phase 2 core gate.

### 1.2 Purpose

Compose price batches and govern every pricing subject with shared quorum, author separation, generation refresh and atomic terminal outcomes. The unit doors dispatch on the unit's stored kind (`prices`, and `plan_revision` from phase 3): its subject, the domain event of its apply and the impact of its card. A prices unit's impact names the plan revisions whose items name its entries (phase 3). Phase 9 counts the units by state and kind under the list's narrowing, pages the list newest first on request and lets it skip the live impact (D-470); every unit says whether its reader may approve it, by the engine's own rule (D-471). The unit reads name the submitter and each decision's actor (`submitted_by_name`, `actor_name`), resolved once per answer through Account Management under the reader's own rights, null when no name is available now (D-519).

Requirements: `cpt-cf-bss-pricing-fr-publish-changes`, `cpt-cf-bss-pricing-fr-approval-units`, `cpt-cf-bss-pricing-fr-events`.

Architecture: `cpt-cf-bss-pricing-component-approvals`, `cpt-cf-bss-pricing-component-prices`, `cpt-cf-bss-pricing-component-events`, `cpt-cf-bss-pricing-principle-business-content-fingerprint`, `cpt-cf-bss-pricing-principle-book-money-independent`, `cpt-cf-bss-pricing-constraint-no-row-locks`, `cpt-cf-bss-pricing-constraint-one-replay-store`, `cpt-cf-bss-pricing-seq-publish-changes`, `cpt-cf-bss-pricing-seq-temporary-pair`, `cpt-cf-bss-pricing-seq-blocked-revision`.

### 1.3 Actors

`cpt-cf-bss-pricing-actor-finance-manager`, `cpt-cf-bss-pricing-actor-finance-reviewer`, `cpt-cf-bss-pricing-actor-auditor`. Every operation authenticates and derives tenant scope before storage, replay or cross-gear calls.
Holding multiple permissions never bypasses separation of duties.

### 1.4 References

- [PRD](../PRD.md), especially the numbered acceptance criteria referenced below.
- [DESIGN](../DESIGN.md), §3 model, API contracts, transaction sequences and DDL.
- [Slice 05](../design/05-approvals.md), including API, data and event obligations.
- [DECISIONS](../DECISIONS.md), D-384–D-433; spec means `docs/superpowers/specs/2026-09-24-pricebook-model-design.md` in the main checkout.
- Source: spec §2 decisions 4–8, 13–17, §2.2, §5–§8, §10, §12–§13; the phase 2 plan supplies delivery boundaries and D-399/D-400.

## 2. Actor Flows (CDSL)

### Review a selected batch

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-flow-approvals`

1. [ ] - `p1` - Finance Manager sees all draft prices of the book with full predecessor, money, chain, dates and impact; all start selected. - `inst-approvals-flow-1`
2. [ ] - `p1` - Untick ordinary prices or select atomic temporary pairs; optionally supply a common effective date. - `inst-approvals-flow-2`
3. [ ] - `p1` - Submit validates one-book membership and shifts dates, then locks items conditionally and copies policy quorum. - `inst-approvals-flow-3`
4. [ ] - `p1` - Finance Reviewer reads the stored snapshot, live impact and generation, then approves or rejects with a required client key. - `inst-approvals-flow-4`
5. [ ] - `p1` - Apply only when the current generation meets quorum and fingerprint/revalidation pass; otherwise retain pending votes, refresh stale content or refuse as specified. - `inst-approvals-flow-5`

## 3. Processes / Business Logic (CDSL)

### submit-unit

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-algo-approvals-submit-unit`

1. [ ] - `p1` - Resolve replay before any work; collect selected business content with every item author and the common date. - `inst-approvals-submit-unit-1`
2. [ ] - `p1` - Validate submit, pair completeness, each temporary's return against the approved chain on its shifted end (PAIR_RETURN_STALE, D-391), uncrossed temporary windows (PRICE_INSIDE_TEMPORARY, TEMPORARY_SPANS_A_CHANGE, D-406), chain rules and ownership; red checks return 400 with their code and no unit. - `inst-approvals-submit-unit-2`
3. [ ] - `p1` - Read kind quorum or tenant default (missing default is fail-safe one), insert unit/items and acquire ordered conditional ownership. - `inst-approvals-submit-unit-3`
4. [ ] - `p1` - Write submission audit; if quorum is zero, apply and write terminal audit/events in the same transaction with no votes. - `inst-approvals-submit-unit-4`

### vote-and-refresh

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-algo-approvals-vote-and-refresh`

1. [ ] - `p1` - Conditionally claim the observed unit version, require pending state and compare the requested generation. - `inst-approvals-vote-and-refresh-1`
2. [ ] - `p1` - Reject submitter/item-author approval and duplicate current-generation votes. - `inst-approvals-vote-and-refresh-2`
3. [ ] - `p1` - Recollect business content and compare fingerprint before counting a vote; on drift refresh items/snapshot/hash, increment generation, mark earlier decisions stale and commit UNIT_STALE. - `inst-approvals-vote-and-refresh-3`
4. [ ] - `p1` - Otherwise insert a decision; an approval below quorum stays pending, a rejection with note unlocks and closes the unit. - `inst-approvals-vote-and-refresh-4`
5. [ ] - `p1` - At quorum revalidate and apply; environment failure rolls back, successful apply emits domain plus terminal events. - `inst-approvals-vote-and-refresh-5`

### apply-prices

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-algo-approvals-apply-prices`

1. [ ] - `p1` - Order affected entries by id and prices by id after the unit; use conditional guards and serializable Postgres transaction. - `inst-approvals-apply-prices-1`
2. [ ] - `p1` - Shift selected prices to the common date with temporary duration preserved. - `inst-approvals-apply-prices-2`
3. [ ] - `p1` - Re-read each chain, enforce usage pair guard and approved start uniqueness, and recompute effective_to independently per value. - `inst-approvals-apply-prices-3`
4. [ ] - `p1` - Set keep_for_bound on the current predecessor of every new price of each touched chain, including a new price approved earlier that a price of this unit now precedes, and never clear it; preserve approved money and historical ids. - `inst-approvals-apply-prices-4`
5. [ ] - `p1` - Replace owned pending locks with approved_by_unit_id and atomically persist audit, PricesPublished and ApprovalUnitDecided. - `inst-approvals-apply-prices-5`

### withdraw-unit

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-algo-approvals-withdraw-unit`

1. [ ] - `p1` - Authenticate the original submitter, require pending state and conditionally claim unit version. - `inst-approvals-withdraw-unit-1`
2. [ ] - `p1` - Clear only this unit's pending ownership without publishing money. - `inst-approvals-withdraw-unit-2`
3. [ ] - `p1` - Persist withdrawn state, attributed terminal audit, ApprovalUnitDecided and replay response together. - `inst-approvals-withdraw-unit-3`

## 4. States (CDSL)

### Approvals states

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-state-approvals`

Unit states are pending → approved/rejected/withdrawn; every terminal state is final. Quorum zero enters approved in the submit transaction. Pending generation g → g+1 refresh commits with UNIT_STALE and earlier decisions stale. Business-content drift is distinct from a lost version race (UNIT_CONTENDED) and environment refusal (APPLY_REFUSED rollback).

## 5. Definitions of Done

Every DoD below is required for this feature's delivery phase. Constraints: `cpt-cf-bss-pricing-constraint-no-row-locks`, `cpt-cf-bss-pricing-constraint-one-replay-store`.

### Selected price batch

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-publish-changes-selection`

Publish changes exposes all book drafts and their full review information, initially selected. The selected subset and optional common date form one atomic prices unit, preserving temporary pairs: a ticked half brings its partner, recorded as added_partner (spec §2 decision 7, D-405).

Requirement: `cpt-cf-bss-pricing-fr-publish-changes`; PRD AC #9.

### Transactional price apply

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-prices-unit`

PricesSubject revalidates shifted prices and chain invariants, normalizes windows and sets keep_for_bound. Approved money, audit and events commit atomically with ordered conditional ownership (spec §6).

Requirement: `cpt-cf-bss-pricing-fr-approval-units`; PRD AC #10.

### Separation from every author

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-sod-excludes-authors`

Approvers exclude both submitter and every item created_by. Holding submit and approve grants does not override the check; a reviewer need not hold submit (spec §6).

Requirement: `cpt-cf-bss-pricing-fr-approval-units`; PRD AC #10.

### Copied quorum including zero

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-quorum-policy`

Policy resolves kind override then tenant default, failing safe to one if absent. A unit snapshots quorum; zero records an approved unit and terminal audit/event without decision rows (spec §6). An override is removed with DELETE /approval-policy/{kind} under If-Match, and the kind follows the default again; the default itself is never deleted (D-435). GET /approval-policy/{kind}/effective answers that quorum after the caller's grant, and reads the policy rows in the tenant scope, so a resource constraint is not applied to kind (D-481).

Requirement: `cpt-cf-bss-pricing-fr-approval-units`; PRD AC #10.

### Committed refresh of stale content

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-stale-refresh-generation`

Fingerprint mismatch replaces items/snapshot/hash and increments generation, preserving old decisions as stale and the unit's submit_note, which is not a fingerprint input (D-445). Caller receives committed UNIT_STALE with the new generation and no old-content publication (spec §2.2).

Requirement: `cpt-cf-bss-pricing-fr-approval-units`; PRD AC #10.

### Generation-scoped votes

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-generation-and-duplicate-vote`

Approve/reject require generation and refuse a delayed vote with GENERATION_MISMATCH. Each actor votes once per generation; duplicate is DUPLICATE_VOTE and terminal state is UNIT_ALREADY_DECIDED (spec §2.2).

Requirement: `cpt-cf-bss-pricing-fr-approval-units`; PRD AC #10.

### Unit contention and environmental refusal

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-unit-contended`

Conditional version admits one writer and returns UNIT_CONTENDED to a lost race. Revalidation failures inside apply roll back as APPLY_REFUSED, distinct from committed content refresh (spec §2.2, §6).

Requirement: `cpt-cf-bss-pricing-fr-approval-units`; PRD AC #10.

### Every terminal outcome is attributable

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-terminal-audit-event`

Reject requires a note and withdraw requires the submitter. Approve, reject, withdraw and zero-quorum paths all persist terminal audit and ApprovalUnitDecided, clearing only owned locks (spec §6).

Requirement: `cpt-cf-bss-pricing-fr-events`; PRD AC #14.

## 6. Acceptance Criteria

| DoD | PRD criterion | Given / When / Then |
| --- | --- | --- |
| `cpt-cf-bss-pricing-dod-publish-changes-selection` | AC #9; `cpt-cf-bss-pricing-fr-publish-changes` | Given three drafts and one temporary companion, when an ordinary price is unticked then only the selected atomic set is locked; a ticked pair half brings its partner (added_partner, D-405), and a foreign-book price fails with no unit. |
| `cpt-cf-bss-pricing-dod-prices-unit` | AC #10; `cpt-cf-bss-pricing-fr-approval-units` | Given two overlapping batches, when approvals race then no overlapping approved windows survive; a valid batch applies all prices and a failed batch applies none. |
| `cpt-cf-bss-pricing-dod-sod-excludes-authors` | AC #10; `cpt-cf-bss-pricing-fr-approval-units` | Given a price authored by A but submitted by B, when A approves then SOD_VIOLATION refuses it; independent C with approve-only permission votes on any unit whose rules read no SKU, and a rule read (usage metering, plan checks) needs products read too (D-416). |
| `cpt-cf-bss-pricing-dod-quorum-policy` | AC #10; `cpt-cf-bss-pricing-fr-approval-units` | Given quorum 0, 1 and 2 units, when the valid number of independent votes is supplied then each applies once; changing policy cannot silently lower an existing unit's snapshot. |
| `cpt-cf-bss-pricing-dod-stale-refresh-generation` | AC #10; `cpt-cf-bss-pricing-fr-approval-units` | Given content drift after a vote, when another vote arrives then the unit refreshes and old votes stop counting; repeated keyed request replays the refresh outcome. |
| `cpt-cf-bss-pricing-dod-generation-and-duplicate-vote` | AC #10; `cpt-cf-bss-pricing-fr-approval-units` | Given refreshed generation 2, when a generation-1 vote arrives then it counts nothing; two votes by one actor in generation 2 cannot meet quorum 2. |
| `cpt-cf-bss-pricing-dod-unit-contended` | AC #10; `cpt-cf-bss-pricing-fr-approval-units` | Given two connections observing one version, when both approve then one terminal apply persists; if environment validation fails neither partial prices nor success events survive. |
| `cpt-cf-bss-pricing-dod-terminal-audit-event` | AC #14; `cpt-cf-bss-pricing-fr-events` | Given a reject or withdraw, when it succeeds then locks clear and a terminal event exists without PricesPublished; missing reject note or foreign withdrawal fails. |

Verification uses domain tests, scoped repository tests on both backends and REST positive/denial/precondition probes as applicable. Phase 2 checks must not mark later-phase behavior implemented. Golden consumer contracts belong to phase 4.
