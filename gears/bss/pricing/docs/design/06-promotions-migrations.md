<!-- CONFLUENCE_TITLE: [BSS]: Pricing — Promotions & Migrations (Design, Slice 6) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../DECISIONS.md | Owners: BSS Pricing team -->

# DESIGN — Promotions & Migrations (Slice 6)

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-design-slice-06`

<!-- toc -->

- [1. Context](#1-context)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Approve a migration with a period-aware preview](#approve-a-migration-with-a-period-aware-preview)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [promotion-window](#promotion-window)
  - [migration-request](#migration-request)
- [4. States (CDSL)](#4-states-cdsl)
- [5. API Surface](#5-api-surface)
- [6. Data Model](#6-data-model)
- [7. Events & Alarms](#7-events--alarms)
- [8. Definitions of Done](#8-definitions-of-done)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Non-Functional Considerations](#10-non-functional-considerations)

<!-- /toc -->

## 1. Context

**Delivery:** phase 3. Every checkbox is an implementation obligation, not an assertion about the legacy code. This complete phase 3 design remains unchecked during phase 2.

**Wholly deferred by the owner (2026-09-25): promotions by D-409, migration requests and plan retirement by D-410.** Nothing in this slice is built in phase 3; every obligation below stays unchecked and describes the planned shape for their return.

Version dated percentage promotions and approve explicit subscription migration requests; retain period-aware previews without executing consumer-owned movement.

Requirements: `cpt-cf-bss-pricing-fr-promotions`, `cpt-cf-bss-pricing-fr-migrations`. Architecture: `cpt-cf-bss-pricing-component-promotions`, `cpt-cf-bss-pricing-component-approvals`, `cpt-cf-bss-pricing-component-events`, `cpt-cf-bss-pricing-principle-book-money-independent`, `cpt-cf-bss-pricing-principle-business-content-fingerprint`, `cpt-cf-bss-pricing-constraint-no-row-locks`.
[FEATURE](../features/promotions-migrations.md) owns the executable flow/algorithm/DoD identifiers; this slice defines no duplicate DoDs.
Dependencies: `cpt-cf-bss-pricing-feature-plans`, `cpt-cf-bss-pricing-feature-approvals`.
Source: PriceBook spec §2.2, §5–§8, §12–§13 and [DECISIONS](../DECISIONS.md) D-384–D-433.

## 2. Actor Flows (CDSL)

### Approve a migration with a period-aware preview

Deferred (D-410). Actors: `cpt-cf-bss-pricing-actor-product-manager`, `cpt-cf-bss-pricing-actor-finance-reviewer`, `cpt-cf-bss-pricing-actor-subscriptions`. Feature flow: `cpt-cf-bss-pricing-flow-promotions-migrations`.

1. [ ] - `p1` - Product Manager selects subscriptions, a published target revision and next_renewal or a concrete date. - `inst-promotions-migrations-flow-1`
2. [ ] - `p1` - Compute and store a preview using each subscription period and intended target; disclose changed structure and binding consequences. - `inst-promotions-migrations-flow-2`
3. [ ] - `p1` - Submit a migration unit with proposal content and author attribution; review under the shared generation protocol. - `inst-promotions-migrations-flow-3`
4. [ ] - `p1` - At apply revalidate target publication and proposal preconditions; persist approved request and enqueue SubscriptionMigrationRequested. - `inst-promotions-migrations-flow-4`
5. [ ] - `p1` - Leave actual movement and retirement completion to Subscriptions; expose request status without claiming execution. - `inst-promotions-migrations-flow-5`

## 3. Processes / Business Logic (CDSL)

### promotion-window

Feature algorithm: `cpt-cf-bss-pricing-algo-promotions-migrations-promotion-window`. Deferred by the owner (D-409, 2026-09-25): not built in phase 3.

1. [ ] - `p1` - Validate percentage, target plans, apply_to and a nonempty half-open date interval. - `inst-promotions-migrations-promotion-window-1`
2. [ ] - `p1` - Reject overlap against approved competing promotions on any shared plan under transactional revalidation. - `inst-promotions-migrations-promotion-window-2`
3. [ ] - `p1` - On approval preserve the old approved version and increment the new version; bindings retain id/version. - `inst-promotions-migrations-promotion-window-3`
4. [ ] - `p1` - Apply discount only to periods starting inside the interval and after price floors; end-today/cancel do not rewrite earlier pins. - `inst-promotions-migrations-promotion-window-4`

### migration-request

Feature algorithm: `cpt-cf-bss-pricing-algo-promotions-migrations-migration-request`. Deferred by the owner (D-410, 2026-09-25): not built in phase 3.

1. [ ] - `p1` - Require a published target and coherent timing: next_renewal or explicit at date. - `inst-promotions-migrations-migration-request-1`
2. [ ] - `p1` - Capture subscription identities, target revision and period-aware preview as proposed business content. - `inst-promotions-migrations-migration-request-2`
3. [ ] - `p1` - Use the migration ApprovalSubject for submit, lock, fingerprint and revalidation. - `inst-promotions-migrations-migration-request-3`
4. [ ] - `p1` - Commit approved request, terminal audit/event and SubscriptionMigrationRequested; do not execute move or mark retirement complete. - `inst-promotions-migrations-migration-request-4`

## 4. States (CDSL)

Deferred (D-409, D-410). Promotions move draft → pending → approved/rejected; approved edits create new versions. End/cancel prevent future application while historical version pins remain stable. Migration proposals become pending units and approved requests; requested is distinct from executed by Subscriptions. Withdrawal/rejection creates no movement event.

State definition: `cpt-cf-bss-pricing-state-promotions-migrations` in the FEATURE.

## 5. API Surface

Deferred by the owner and not built in phase 3 (promotions D-409, migrations D-410). Under /bss-pricing/v1: POST /promotions; PATCH /promotions/{id} draft only; POST /promotions/{id}/submit, /end-today and /cancel; POST /plans/{id}/migrations. Promotion and migration submission use the common unit doors in slice 05. POST keys, PATCH If-Match, author/submit/approve separation and tenant scope remain required.

[DESIGN §3.3](../DESIGN.md#33-api-contracts) fixes canonical errors and route prefixes.
Each mounted route must appear in all four censuses with authz and precondition expectations.

## 6. Data Model

Deferred by the owner and not in the phase 3 chain (promotions D-409, migration requests D-410). Phase 3 adds pricing_promotion(id, tenant_id, name, version, percent, from_date, to_date, plan_ids, apply_to recurring|recurring_usage, approval draft|pending|approved|rejected, pending_unit_id, approved_by_unit_id). Approved versions remain available for bindings. pricing_migration_request(id, tenant_id, plan_id, target_plan_id, target_rev, subscription_ids, timing next_renewal|date, at nullable, preview, pending_unit_id, approved_by_unit_id) records proposals and approval attribution; mutable proposals also carry author/version/timestamps. There is no completed-subscription-move state inferred merely from approval.

Tenant-scoped parent validation is required even where foreign keys use entity ids. Never substitute a
cross-gear read for transactional local ownership/version guards. Approved money and historical pins survive.

## 7. Events & Alarms

Deferred (D-409, D-410). PromotionPublished and SubscriptionMigrationRequested share successful apply transactions with ApprovalUnitDecided and audit. A rejected/withdrawn unit has only its terminal event. PlanRetired is reserved for actual retirement completion under the later consumer integration.

Audit and outbox inserts use the same mutation transaction; retry is lifecycle-managed and observes shutdown.

## 8. Definitions of Done

The sole definitions live in [features/promotions-migrations.md](../features/promotions-migrations.md):

- `cpt-cf-bss-pricing-dod-promotion-windows` — Nonoverlapping promotion windows (deferred, D-409).
- `cpt-cf-bss-pricing-dod-promotion-version-pins` — Versioned promotion bindings (deferred, D-409).
- `cpt-cf-bss-pricing-dod-promotion-governance` — Promotion approval and ending (deferred, D-409).
- `cpt-cf-bss-pricing-dod-migration-preview` — Period-aware migration preview (deferred, D-410).
- `cpt-cf-bss-pricing-dod-migration-request-only` — Approval requests movement (deferred, D-410).
- `cpt-cf-bss-pricing-dod-migration-approval` — Migration revalidation and review (deferred, D-410).
- `cpt-cf-bss-pricing-dod-retirement-request-boundary` — Retirement completion boundary (deferred, D-410).

## 9. Acceptance Criteria

1. PRD AC #16 / `cpt-cf-bss-pricing-dod-promotion-windows` (deferred, D-409): Given a promotion ending December 1, when a period starts December 1 then no discount applies; a second overlapping promotion on the same plan is refused.
2. PRD AC #16 / `cpt-cf-bss-pricing-dod-promotion-version-pins` (deferred, D-409): Given a pin to promotion version 1, when version 2 is approved then replay still uses version 1; direct mutation of approved version 1 is refused.
3. PRD AC #16 / `cpt-cf-bss-pricing-dod-promotion-governance` (deferred, D-409): Given a pending promotion, when an unauthorized author attempts bypass then it fails; an approved end stops future eligible periods while earlier pins retain their version.
4. PRD AC #17 / `cpt-cf-bss-pricing-dod-migration-preview` (deferred, D-410): Given subscriptions with different renewal dates, when next_renewal is previewed then each keeps its own boundary; an unpublished target cannot be submitted.
5. PRD AC #17 / `cpt-cf-bss-pricing-dod-migration-request-only` (deferred, D-410): Given a valid approved migration, when its result is read then it is requested and evented; subscriber pins do not change merely because Pricing committed.
6. PRD AC #17 / `cpt-cf-bss-pricing-dod-migration-approval` (deferred, D-410): Given a reviewed target that becomes invalid, when approval applies then it fails atomically; a refreshed proposal requires fresh current-generation votes.
7. PRD AC #17 / `cpt-cf-bss-pricing-dod-retirement-request-boundary` (deferred, D-410): Given a retiring plan with approved request but no completion, when status is read then retirement is not completed and live references are not released.

## 10. Non-Functional Considerations

All paths enforce tenant isolation, deny-by-default authz and append-only attribution. Mutations use conditional
versions, required POST replay and PATCH/PUT preconditions; PostgreSQL serializable chain changes and SQLite
writer serialization preserve the same invariants. A failed audit/outbox write cannot leave a committed act.
No timeout releases a live reference. Review and apply retain typed database failures for bounded retry.
Implementation gates cover both backends and route censuses; document gates cover toc, language and identifier ownership.
