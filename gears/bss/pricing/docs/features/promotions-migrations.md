<!-- CONFLUENCE_TITLE: [BSS]: Pricing — Promotions & Migrations (Feature) -->
<!-- Related: ../DECOMPOSITION.md, ../DESIGN.md, ../PRD.md | Owners: BSS Pricing team -->

# Feature: Promotions & Migrations

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-featstatus-promotions-migrations-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-bss-pricing-feature-promotions-migrations`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Approve a migration with a period-aware preview](#approve-a-migration-with-a-period-aware-preview)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [promotion-window](#promotion-window)
  - [migration-request](#migration-request)
- [4. States (CDSL)](#4-states-cdsl)
  - [Promotions & Migrations states](#promotions--migrations-states)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Nonoverlapping promotion windows](#nonoverlapping-promotion-windows)
  - [Versioned promotion bindings](#versioned-promotion-bindings)
  - [Promotion approval and ending](#promotion-approval-and-ending)
  - [Period-aware migration preview](#period-aware-migration-preview)
  - [Approval requests movement](#approval-requests-movement)
  - [Migration revalidation and review](#migration-revalidation-and-review)
  - [Retirement completion boundary](#retirement-completion-boundary)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

**Delivery:** phase 3. Every checkbox is an implementation obligation, not an assertion about the legacy code. This complete phase 3 design remains unchecked during phase 2.

**Wholly deferred by the owner (2026-09-25): promotions by D-409, migration requests and plan retirement by D-410.** Nothing in this slice is built in phase 3; every obligation below stays unchecked and describes the planned shape for their return.

This feature implements [slice 06](../design/06-promotions-migrations.md).
[DECOMPOSITION](../DECOMPOSITION.md) records integration order; [DESIGN §3](../DESIGN.md#3-technical-architecture)
is the schema and transaction authority. Unchecked phase 3/4 work is not part of the phase 2 core gate.

### 1.2 Purpose

Version dated percentage promotions and approve explicit subscription migration requests; retain period-aware previews without executing consumer-owned movement.

Requirements: `cpt-cf-bss-pricing-fr-promotions`, `cpt-cf-bss-pricing-fr-migrations`.

Architecture: `cpt-cf-bss-pricing-component-promotions`, `cpt-cf-bss-pricing-component-approvals`, `cpt-cf-bss-pricing-component-events`, `cpt-cf-bss-pricing-principle-book-money-independent`, `cpt-cf-bss-pricing-principle-business-content-fingerprint`, `cpt-cf-bss-pricing-constraint-no-row-locks`.

### 1.3 Actors

`cpt-cf-bss-pricing-actor-product-manager`, `cpt-cf-bss-pricing-actor-finance-reviewer`, `cpt-cf-bss-pricing-actor-subscriptions`. Every operation authenticates and derives tenant scope before storage, replay or cross-gear calls.
Holding multiple permissions never bypasses separation of duties.

### 1.4 References

- [PRD](../PRD.md), especially the numbered acceptance criteria referenced below.
- [DESIGN](../DESIGN.md), §3 model, API contracts, transaction sequences and DDL.
- [Slice 06](../design/06-promotions-migrations.md), including API, data and event obligations.
- [DECISIONS](../DECISIONS.md), D-384–D-433; spec means `docs/superpowers/specs/2026-09-24-pricebook-model-design.md` in the main checkout.
- Source: spec §2 decisions 4–8, 13–17, §2.2, §5–§8, §10, §12–§13; the phase 2 plan supplies delivery boundaries and D-399/D-400.

## 2. Actor Flows (CDSL)

### Approve a migration with a period-aware preview

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-flow-promotions-migrations`

Deferred by the owner (D-410, 2026-09-25): not built in phase 3.

1. [ ] - `p1` - Product Manager selects subscriptions, a published target revision and next_renewal or a concrete date. - `inst-promotions-migrations-flow-1`
2. [ ] - `p1` - Compute and store a preview using each subscription period and intended target; disclose changed structure and binding consequences. - `inst-promotions-migrations-flow-2`
3. [ ] - `p1` - Submit a migration unit with proposal content and author attribution; review under the shared generation protocol. - `inst-promotions-migrations-flow-3`
4. [ ] - `p1` - At apply revalidate target publication and proposal preconditions; persist approved request and enqueue SubscriptionMigrationRequested. - `inst-promotions-migrations-flow-4`
5. [ ] - `p1` - Leave actual movement and retirement completion to Subscriptions; expose request status without claiming execution. - `inst-promotions-migrations-flow-5`

## 3. Processes / Business Logic (CDSL)

### promotion-window

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-algo-promotions-migrations-promotion-window`

Deferred by the owner (D-409, 2026-09-25): not built in phase 3.

1. [ ] - `p1` - Validate percentage, target plans, apply_to and a nonempty half-open date interval. - `inst-promotions-migrations-promotion-window-1`
2. [ ] - `p1` - Reject overlap against approved competing promotions on any shared plan under transactional revalidation. - `inst-promotions-migrations-promotion-window-2`
3. [ ] - `p1` - On approval preserve the old approved version and increment the new version; bindings retain id/version. - `inst-promotions-migrations-promotion-window-3`
4. [ ] - `p1` - Apply discount only to periods starting inside the interval and after price floors; end-today/cancel do not rewrite earlier pins. - `inst-promotions-migrations-promotion-window-4`

### migration-request

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-algo-promotions-migrations-migration-request`

Deferred by the owner (D-410, 2026-09-25): not built in phase 3.

1. [ ] - `p1` - Require a published target and coherent timing: next_renewal or explicit at date. - `inst-promotions-migrations-migration-request-1`
2. [ ] - `p1` - Capture subscription identities, target revision and period-aware preview as proposed business content. - `inst-promotions-migrations-migration-request-2`
3. [ ] - `p1` - Use the migration ApprovalSubject for submit, lock, fingerprint and revalidation. - `inst-promotions-migrations-migration-request-3`
4. [ ] - `p1` - Commit approved request, terminal audit/event and SubscriptionMigrationRequested; do not execute move or mark retirement complete. - `inst-promotions-migrations-migration-request-4`

## 4. States (CDSL)

### Promotions & Migrations states

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-state-promotions-migrations`

Deferred by the owner (D-409, D-410, 2026-09-25): not built in phase 3.

Promotions move draft → pending → approved/rejected; approved edits create new versions. End/cancel prevent future application while historical version pins remain stable. Migration proposals become pending units and approved requests; requested is distinct from executed by Subscriptions. Withdrawal/rejection creates no movement event.

## 5. Definitions of Done

Every DoD below is required for this feature's delivery phase. Constraints: `cpt-cf-bss-pricing-constraint-no-row-locks`.

### Nonoverlapping promotion windows

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-dod-promotion-windows`

Deferred by the owner (D-409, 2026-09-25): not built in phase 3. This DoD stays unticked.

Target-plan intervals are half-open and overlap is rejected at submit and apply. Discounts depend on period start and apply_to, not every day touched by the period (spec §5).

Requirement: `cpt-cf-bss-pricing-fr-promotions`; PRD AC #16.

### Versioned promotion bindings

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-dod-promotion-version-pins`

Deferred by the owner (D-409, 2026-09-25): not built in phase 3. This DoD stays unticked.

Every approved edit increments version and preserves historical approved inputs. A binding retains promotion id/version so replay cannot read the newest percentage accidentally (spec §5, §7.1).

Requirement: `cpt-cf-bss-pricing-fr-promotions`; PRD AC #16.

### Promotion approval and ending

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-dod-promotion-governance`

Deferred by the owner (D-409, 2026-09-25): not built in phase 3. This DoD stays unticked.

Promotion uses the shared subject, quorum and SoD checks. End-today and cancel respect pending ownership and historical pins, without inventing per-signup trials (spec §2 decision 15, §7.2).

Requirement: `cpt-cf-bss-pricing-fr-promotions`; PRD AC #16.

### Period-aware migration preview

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-dod-migration-preview`

Deferred by the owner (D-410, 2026-09-25): not built in phase 3. This DoD stays unticked.

The proposal records subscriptions, target and renewal/date timing with a reviewable period-aware preview. An unpublished target and incoherent date mode are refused (spec §5, §8).

Requirement: `cpt-cf-bss-pricing-fr-migrations`; PRD AC #17.

### Approval requests movement

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-dod-migration-request-only`

Deferred by the owner (D-410, 2026-09-25): not built in phase 3. This DoD stays unticked.

Applying migration persists the request and emits SubscriptionMigrationRequested. Pricing does not execute Subscriptions move or claim completion (spec §11 phase 3).

Requirement: `cpt-cf-bss-pricing-fr-migrations`; PRD AC #17.

### Migration revalidation and review

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-dod-migration-approval`

Deferred by the owner (D-410, 2026-09-25): not built in phase 3. This DoD stays unticked.

Migration uses generation, author separation and conditional ownership. Target/environment drift at apply cannot partially request movement for a subset (spec §6).

Requirement: `cpt-cf-bss-pricing-fr-migrations`; PRD AC #17.

### Retirement completion boundary

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-dod-retirement-request-boundary`

Deferred by the owner (D-410, 2026-09-25): not built in phase 3. This DoD stays unticked.

An approved migration is a prerequisite rather than proof of retirement completion. Pricing retains historical references and pins until consumer-owned movement is confirmed through its separate integration (spec §11–§12).

Requirement: `cpt-cf-bss-pricing-fr-migrations`; PRD AC #17.

## 6. Acceptance Criteria

| DoD | PRD criterion | Given / When / Then |
| --- | --- | --- |
| `cpt-cf-bss-pricing-dod-promotion-windows` | AC #16; `cpt-cf-bss-pricing-fr-promotions` | Deferred (D-409). Given a promotion ending December 1, when a period starts December 1 then no discount applies; a second overlapping promotion on the same plan is refused. |
| `cpt-cf-bss-pricing-dod-promotion-version-pins` | AC #16; `cpt-cf-bss-pricing-fr-promotions` | Deferred (D-409). Given a pin to promotion version 1, when version 2 is approved then replay still uses version 1; direct mutation of approved version 1 is refused. |
| `cpt-cf-bss-pricing-dod-promotion-governance` | AC #16; `cpt-cf-bss-pricing-fr-promotions` | Deferred (D-409). Given a pending promotion, when an unauthorized author attempts bypass then it fails; an approved end stops future eligible periods while earlier pins retain their version. |
| `cpt-cf-bss-pricing-dod-migration-preview` | AC #17; `cpt-cf-bss-pricing-fr-migrations` | Deferred (D-410). Given subscriptions with different renewal dates, when next_renewal is previewed then each keeps its own boundary; an unpublished target cannot be submitted. |
| `cpt-cf-bss-pricing-dod-migration-request-only` | AC #17; `cpt-cf-bss-pricing-fr-migrations` | Deferred (D-410). Given a valid approved migration, when its result is read then it is requested and evented; subscriber pins do not change merely because Pricing committed. |
| `cpt-cf-bss-pricing-dod-migration-approval` | AC #17; `cpt-cf-bss-pricing-fr-migrations` | Deferred (D-410). Given a reviewed target that becomes invalid, when approval applies then it fails atomically; a refreshed proposal requires fresh current-generation votes. |
| `cpt-cf-bss-pricing-dod-retirement-request-boundary` | AC #17; `cpt-cf-bss-pricing-fr-migrations` | Deferred (D-410). Given a retiring plan with approved request but no completion, when status is read then retirement is not completed and live references are not released. |

Verification uses domain tests, scoped repository tests on both backends and REST positive/denial/precondition probes as applicable. Phase 2 checks must not mark later-phase behavior implemented. Golden consumer contracts belong to phase 4.
