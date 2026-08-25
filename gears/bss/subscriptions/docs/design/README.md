<!-- CONFLUENCE_TITLE: [BSS]: Subscriptions — Design Set -->
<!-- Related: ../DESIGN.md, ../PRD.md, ../SEAMS.md, ../DECISIONS.md, ../ADR/ | Owners: BSS Subscriptions team -->

# Subscriptions — Design Set

<!-- toc -->

- [Slice documents](#slice-documents)
- [Slice map (PRD §6 ↔ slice)](#slice-map-prd-6--slice)

<!-- /toc -->

This folder holds the **subscriptions gear's** technical design as a **set of slice designs**: a
shared **Lifecycle Foundation** ([`01-foundation-lifecycle.md`](./01-foundation-lifecycle.md)) — the
subscription aggregate, the manifest-closed status machine, the `TransitionRequest` envelope with
idempotency + pinned `(orderingTenantId, subscriptionId)` ordering (SUB-D-06), the Policy/OSS fail-closed gate,
versioning, and the audit store — plus per-capability slices decomposed along the PRD §6 subsections.
Every capability slice requests transitions **through** the Foundation under those invariants; none
re-implements the commit/idempotency/gate mechanics.

The gear is not an authoring System of Record: the pricing gear owns the catalog (`Plan`/`Price`/
`PriceWindow`/`PriceOverlay`/`CatalogVersion`), the rating gear owns tariff evaluation + proration
math, Billing owns posting. The cross-gear contract is frozen in [`../SEAMS.md`](../SEAMS.md); this
design implements the subscriptions side of every listed seam.

**The canonical index — architecture overview, slice map, dependency order, cross-cutting
normatives, the ADR index, and traceability — is [`../DESIGN.md`](../DESIGN.md).** Requirements
(WHAT/WHY) live in [`../PRD.md`](../PRD.md); the autonomous decisions in
[`../DECISIONS.md`](../DECISIONS.md); the cross-gear seam register in [`../SEAMS.md`](../SEAMS.md).

## Slice documents

- [`01-foundation-lifecycle.md`](./01-foundation-lifecycle.md) — **Lifecycle Foundation** — The shared substrate every capability slice runs *through*: the `Subscription` aggregate, the closed manifest status machine (`draft`→`active`↔`suspended`→`cancelled`→`archived`) with guards + terminality, the `TransitionRequest` envelope (idempotency on `(subscriptionId, idempotencyKey)`, ordering on the pinned `(orderingTenantId, subscriptionId)`, SUB-D-06), scheduled pending intents, the three activation instants, the Policy/OSS fail-closed gate, versioning, and the audit store. (PRD §6.1)
- [`02-composition-versioning.md`](./02-composition-versioning.md) — **Composition & Versioning** — Effective-dated `PlanLink`/`AddOn` intervals, monotonic `version`, snapshot discipline (the Subscriptions-written `(currency, region)` `pricingSnapshotRef` segment), `PlanTier` derivability @ `t`, per-sale brand attribution. (PRD §6.2)
- [`03-plan-changes.md`](./03-plan-changes.md) — **Plan & Quantity Changes** — The change boundary/mode + up/down asymmetry, `updateQuantity` seat provenance, ramp execution of scheduled intents, overlap-cardinality detection on the `overlapScopeKey`, and backdated-change guards against posted invoices. (PRD §6.3)
- [`04-suspension-renewal-grace.md`](./04-suspension-renewal-grace.md) — **Suspension, Renewal & Grace** — Suspend/resume semantics, the `collectionPaused` billing-only posture, the Contract-driven renewal job (auto/manual), notices + opt-out, and the failed-renewal grace ladder with evaluated fields. (PRD §6.4, §6.5)
- [`05-entitlements.md`](./05-entitlements.md) — **Entitlement Lifecycle** — Issue/revoke from transitions, assignment from the pricing published grant set (incl. the per-phase map), the point-of-use check decision state (p95 < 100ms, OSS enforces), and quota soft/hard limits. (PRD §6.9)
- [`06-trials.md`](./06-trials.md) — **Trial Runtime & Conversion** — Trial provisioning on the phase machinery, end-of-trial conversion (`convertsToPhaseId`), early `convertTrial`, expiry without conversion, and approval-gated extension. (PRD §6.10)
- [`07-tenancy-transfer.md`](./07-tenancy-transfer.md) — **Multi-Tenant Ownership & Transfer** — The three tenant axes referenced from AMS, delegation-proof enforcement, hierarchy by reference, and the approval-gated ownership-transfer flow. (PRD §6.6)
- [`08-events-billing.md`](./08-events-billing.md) — **Event Model & Billing Alignment** — The producer inventory + payload-sufficiency rules, ordering, recurring `BillableItem` idempotency per `(subscriptionId, billing period, lineKey)`, charge-to-catalog traceability, and the event outbox. (PRD §6.7, §6.8)
- [`09-consumer-contracts.md`](./09-consumer-contracts.md) — **Consumer & Integration Contracts** — The integration surface: the Billing handoff, the rating read-model, the Contracts input, the Policy gate, OSS provisioning, and Payments signals. (PRD §9)

## Slice map (PRD §6 ↔ slice)

| Doc | PRD §6 | Seams owned |
|-----|--------|-------------|
| `01-foundation-lifecycle` | §6.1 | SUB-E1, SUB-C4, SUB-N1 |
| `02-composition-versioning` | §6.2 | SUB-R2, SUB-G2 |
| `03-plan-changes` | §6.3 | SUB-R1, SUB-R3, SUB-P1, SUB-C2, SUB-G1, SUB-B6, SUB-P7 (§4.7) |
| `04-suspension-renewal-grace` | §6.4, §6.5 | SUB-B2, SUB-C1, SUB-F1, SUB-B5, SUB-E2, SUB-P6 (§4.3/§4.5) |
| `05-entitlements` | §6.9 | SUB-P2, SUB-E3, SUB-B4 |
| `06-trials` | §6.10 | SUB-R4, SUB-P3 |
| `07-tenancy-transfer` | §6.6 | (AMS delegation; transfer approvals) |
| `08-events-billing` | §6.7, §6.8 | SUB-B1, SUB-R1 (ordering), SUB-R6, SUB-C5, SUB-B7, SUB-B8 |
| `09-consumer-contracts` | §9 | SUB-R1, SUB-B1, SUB-B7/B8 (wire), SUB-C1, SUB-E1/E3, SUB-F1, SUB-G1, SUB-P6, SUB-P8 (§4.1) |

The numeric prefix follows the PRD §6 decomposition and the dependency order in
[`../DESIGN.md`](../DESIGN.md) §1.3.
