Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-10-01 by Virtuozzo International GmbH

<!-- CONFLUENCE_TITLE: [BSS]: Rating — Governance & ASC 606 (Design) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../SEAMS.md, ../DECISIONS.md | Upstream: pricing | Downstream: pricing (publish pipeline), Billing orchestration | Owners: BSS Rating team -->

# DESIGN — Governance & ASC 606 (Slice 10)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-governance-asc606`

<!-- toc -->

- [1. Architecture Overview](#1-architecture-overview)
  - [1.1 Architectural Vision](#11-architectural-vision)
  - [1.2 Architecture Drivers](#12-architecture-drivers)
  - [1.3 Architecture Layers](#13-architecture-layers)
- [2. Principles and Constraints](#2-principles-and-constraints)
  - [2.1 Design Principles](#21-design-principles)
  - [2.2 Constraints](#22-constraints)
- [3. Technical Architecture](#3-technical-architecture)
  - [3.1 Domain Model](#31-domain-model)
  - [3.2 Component Model](#32-component-model)
  - [3.3 API Contracts](#33-api-contracts)
  - [3.4 Internal Dependencies](#34-internal-dependencies)
  - [3.5 External Dependencies](#35-external-dependencies)
  - [3.6 Interactions and Sequences](#36-interactions-and-sequences)
  - [3.7 Database Schemas and Tables](#37-database-schemas-and-tables)
  - [3.8 Deployment Topology](#38-deployment-topology)
- [4. Additional Context](#4-additional-context)
  - [4.1 Single Governance Engine (normative)](#41-single-governance-engine-normative)
  - [4.2 The Four Publish-Time Validators (normative)](#42-the-four-publish-time-validators-normative)
  - [4.3 Ledger Separation Rationale (normative)](#43-ledger-separation-rationale-normative)
  - [4.4 ASC 606 Reference Emission (normative)](#44-asc-606-reference-emission-normative)
  - [4.5 Bundle Rev-Share Pass-Through (normative)](#45-bundle-rev-share-pass-through-normative)
  - [4.6 AuthZ Resource and Action Catalog (normative)](#46-authz-resource-and-action-catalog-normative)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

Three concerns, none of them on the rating hot path as a workflow:

1. **Publish governance** belongs to the pricing gear. There is one approval engine — pricing
   Slice 5 (`MaterialityEvaluator`, `ApprovalWorkflow`, `approval_policy`, FinanceReviewer). Rating
   contributes **four rules** that must hold for any catalog it rates, intended to run as
   registered fail-closed validators in pricing's publish pipeline (T-D-06). Pricing exposes no
   registration hook today (SEAMS P-5, `[DEPENDENCY GAP R-12]`); until it does, the same four rules
   are evaluated by `rating-core` at rating time and fail the child closed.
2. **ASC 606 references** (`performanceObligationRef`, `sspSnapshotPointer`) ride every rated line
   as nullable pass-through fields; both are null at MVP.
3. **Bundle `sum_of_parts`**: Rating sums the **recurring** amounts of the component plans and
   passes the published effective rev-shares through untouched.

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design Response |
|-------------|-----------------|
| `cpt-cf-bss-rating-fr-publish-approval-governance` | Four rules with ids and failure codes (§4.2); registered in pricing Slice 5 once a hook exists; meanwhile enforced at rating time as evaluation errors. Rating runs no approval workflow. |
| `cpt-cf-bss-rating-fr-asc606-traceable-identifiers` | Nullable `performanceObligationRef` / `sspSnapshotPointer` on every rated line; immutable once non-null (§4.4). |
| `cpt-cf-bss-rating-contract-pricing-readmodel` (bundle clause) | `sum_of_parts` summing over component recurring lines; effective shares passed through (§4.5). |

#### NFR Allocation

| NFR theme | Allocated To | Design Response | Verification / Status |
|-----------|--------------|-----------------|-----------------------|
| `cpt-cf-bss-rating-nfr-audit-segregation` | pricing Slice 5 | Multi-approver sign-off and audit rows live in pricing (`pricing_audit_log`); Rating's own money-affecting operator actions are audited per [`../DESIGN.md`](../DESIGN.md) §4.8 | Depends on R-12 for publish-time enforcement |
| `cpt-cf-bss-rating-nfr-resilience` | Rule verdicts | A rule that cannot evaluate its subject fails closed | Negative fixtures |

#### Key ADRs

| ADR ID | Decision Summary |
|--------|------------------|
| `cpt-cf-bss-rating-adr-scope-key-adoption` | Same posture as the scope key: adopt pricing's machinery, contribute rules, never fork a workflow. |

### 1.3 Architecture Layers

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-tech-stack-gov`

| Layer | Responsibility | Technology |
|-------|----------------|------------|
| Domain (`rating-core`) | The four rule predicates as pure functions over plan/overlay documents; ASC ref pass-through; bundle summing | Rust, pure |
| Host (pricing gear) | Publish pipeline that would call the rules via a registration trait | pricing crate (hook missing, R-12) |
| Infrastructure | none owned by this slice | — |

## 2. Principles and Constraints

### 2.1 Design Principles

#### One engine, registered rules

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-one-engine-gov`

Materiality, two-person approval, content pinning and approver scope are pricing Slice 5's; Rating
contributes rule content only.

#### Enforce at publish, trust at evaluation

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-publish-enforce-gov`

The target is that a catalog violating a rule is unpublishable. Rating still checks the same rules
at evaluation and fails closed, which is the only enforcement until the pricing hook exists.

#### Pass through evidence, never re-derive

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-pass-through-gov`

ASC 606 references, GL codes and effective rev-shares are copied from the pinned documents onto the
rated line; Rating never computes, normalizes or backfills them.

### 2.2 Constraints

#### No second workflow, no approval state

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-no-second-workflow-gov`

Rating has no approval state machine, approver role, approval store or approval API. Contract-level
overrides are governed by the (not yet existing) Contracts gear.

#### Publish-time only

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-publish-time-gov`

No governance *workflow* runs on the rating path. The rating-time rule checks are ordinary
fail-closed evaluation errors (`rule_violation:<id>`), not approvals.

#### Emitted references are immutable

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-immutable-refs-gov`

A non-null ASC 606 reference on a window result never changes on that revision; a catalog change can
only reach a new revision through an administrative re-rate, where the reference is re-copied from the pinned
document.

## 3. Technical Architecture

### 3.1 Domain Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-domain-model-gov`

- **`RuleSpec`** — `{ id, failure_code, subject (plan document | overlay documents | contract overlay), predicate }` for the four rules of §4.2.
- **`RuleVerdict`** — `pass | fail { code, findings[] }`.
- **`Asc606Refs`** — `performance_obligation_ref: Option<String>`, `ssp_snapshot_pointer: Option<String>`; null at MVP.
- **`BundleComposition`** — for a `sum_of_parts` bundle: component `plan_id` set and effective rev-shares per `(bundle, vendor SKU)` (`effective_share_bp`, `platform_cut_bp`, summing to 10 000 bp), from the pinned bundle document.

### 3.2 Component Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-component-governance-gov`

- **`RuleSet`** (`rating-core`) — the four predicates; callable by pricing's publish pipeline (once registered) and by `evaluate`.
- **`Asc606RefCopier`** — copies the references from the pinned document onto each line.
- **`BundleSummer`** — evaluates each component plan's recurring lines through the ordinary steps and sums them into the bundle line; attaches effective shares to component lineage.

### 3.3 API Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-interface-validator-registration-gov`

**Rule registration** (Rating → pricing): each rule exposes `id`, `failure_code`, and
`check(&PublishUnit) -> RuleVerdict`. Pricing must add a registration trait to its Slice 5
validation pipeline and call it at submit pre-check and inside the publish-commit transaction
(SEAMS J-10). `[DEPENDENCY GAP R-12]`

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-interface-asc606-envelope-gov`

**Line fields**: every `RatedLine` carries `performance_obligation_ref`, `ssp_snapshot_pointer`
(nullable), `gl_code`, `tax_category` and `invoice_line_template` — taken from the pinned plan
document's `descriptorSet {glCode, invoiceLineTemplate}` and the price's `taxCategoryRef` /
`resolvedTaxCategory` (pricing `domain/projection.rs`, SEAMS P-2), carried on the parent delivery
line (slice 16 §4.1) and from there by Billing into the ledger invoice item
(`InvoiceItemDto.gl_code`, SEAMS L-2). A descriptor absent from the document stays null; Billing
decides the fallback (the ledger routes an unmapped line by `catalog_class`/SUSPENSE). GL, tax and
template values are part of `invoice_line_key` (T-D-53).

### 3.4 Internal Dependencies

Slice [`03`](./03-metering-models.md) defines the injective mapping behind rule 2; slice
[`04`](./04-overlays-precedence.md) the precedence and composition-cap semantics behind rules 1 and
3; slice [`01`](./01-foundation.md) the line envelope.

### 3.5 External Dependencies

| Dependency | What it provides | Contract |
|------------|------------------|----------|
| pricing Slice 5 | The approval engine and audit trail | pricing `design/05`; hook SEAMS P-5 — `[DEPENDENCY GAP R-12]` |
| pricing documents | Plan, overlay and bundle documents (incl. `PriceBasis {SumOfParts, OwnPrice}`, effective shares) | SEAMS P-2 — `[DEPENDENCY GAP R-02]` |
| Contracts | Contract overlays checked by rule 4 | SEAMS G-1 — no source (R-11) |
| Billing / Finance | Consume ASC refs and GL codes | SEAMS L-2 |

### 3.6 Interactions and Sequences

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-publish-gate-gov`

**Rule enforcement**:

1. *Target (after R-12)*: an author submits a catalog publish; pricing's validation pipeline calls Rating's four rules; any `fail` blocks the publish with the enumerated findings; the commit transaction re-runs them; verdicts are audited in `pricing_audit_log`.
2. *Interim (now)*: `evaluate` runs the rules against the pinned documents it uses; a failure returns `EvaluationError::RuleViolation { rule_id, findings }`, the child is `failed`, and a `rating_exception` is raised — no charge is produced for a catalog Rating considers invalid.

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-bundle-sum-passthrough-gov`

**Bundle summing**:

1. A `period_line` child for a `sum_of_parts` bundle plan is evaluated.
2. Each component plan's **recurring** lines resolve through the ordinary steps on their own keys; component **usage** lines rate as their own children and never enter the bundle sum.
3. The bundle line amount = Σ component recurring amounts; if any required recurring component fails to resolve, the whole bundle line fails closed.
4. Effective shares attach to the component lineage untouched.

### 3.7 Database Schemas and Tables

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-storage-none-gov`

No table. Rule verdicts at rating time are `rating_exception` rows; refs and shares are part of
`rating_window_result.lines` ([`../DESIGN.md`](../DESIGN.md) §3.7).

### 3.8 Deployment Topology

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-deployment-gov`

The rules live in `rating-core` so both pricing (as a dependency of its publish pipeline) and the
rating pipeline can call the same code; no new deployable.

## 4. Additional Context

### 4.1 Single Governance Engine (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-single-engine-gov`

- One catalog-publish approval engine: pricing Slice 5. Rating defines no approval semantics.
- Contract-level overrides are governed by Contracts, not by pricing and not by Rating.
- The `cpt-cf-bss-rating-nfr-audit-segregation` threshold is met inside pricing: validator verdicts
  and approvals commit as audit rows with actor, before/after references and effective times.

### 4.2 The Four Publish-Time Validators (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-four-validators-gov`

| # | Rule id | Failure code | Check | PRD anchor |
|---|---|---|---|---|
| 1 | `rating-val-01` | `RATING_OVERLAY_PRECEDENCE_AMBIGUOUS` | Equal `precedence` among overlays with overlapping scope within one scope class | §6.12; §17.1 step 4 |
| 2 | `rating-val-02` | `RATING_METER_MAPPING_AMBIGUOUS` | `(meter, dimension_key)` → charge line mapping not injective per plan revision | §6.12; §17.1 step 3 |
| 3 | `rating-val-03` | `RATING_CHAIN_CAP_MISSING` | An overlay chain of depth ≥ 2 reaching one priced row with no `maxCumulativeMarkup` and no Finance default | §6.12; §17.1 step 4 |
| 4 | `rating-val-04` | `RATING_OVERLAY_DIMENSION_UNDECLARED` | A contract overlay introducing a metering dimension absent from the published plan revision | §6.12; §17.1 step 5 |

- At publish (target): any failure blocks the publish; findings are enumerated; the codes surface in
  pricing's RFC 9457 validation report (422, `violations[]`).
- At rating time (interim and defensive): the same predicate failing yields
  `rule_violation:<rule id>`; rule 4 is inert until a Contracts source exists (R-11).
- `[OPEN QUESTION]` default `maxCumulativeMarkup` and clamp-vs-hard mode (PRD §15).
- `[OPEN QUESTION]` whether rule 4 also gates the Contracts-side override publish.

### 4.3 Ledger Separation Rationale (normative)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-normative-ledger-separation-gov`

The ledger's `dual_control_policy` governs financial postings (journal entries, refunds, period
reopen); pricing Slice 5 governs catalog publish. Same maker-checker pattern, different subject,
approver and materiality — so catalog governance does not move into the ledger, and Rating adds
nothing to either.

### 4.4 ASC 606 Reference Emission (normative)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-normative-asc606-refs-gov`

- Every rated line carries `performance_obligation_ref` and `ssp_snapshot_pointer`, nullable; both
  are null at MVP because no gear supplies them (pricing defers SSP; Contracts does not exist).
- The values are copied from the pinned documents; they are immutable on a version.
- The ledger accepts `ssp_snapshot_ref` only on its recognition input and requires it only for
  multi-PO lines (SEAMS L-2); Billing maps it there. Recognition schedules are Billing/Finance's.

### 4.5 Bundle Rev-Share Pass-Through (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-revshare-passthrough-gov`

- For `PriceBasis::SumOfParts` the bundle amount is the sum of the component plans' **recurring**
  amounts; component usage rates per its own rows and is itemized separately.
- A required recurring component that fails to resolve fails the bundle line closed; a usage-only
  component contributes nothing to the sum and does not fail it.
- Effective rev-shares arrive publish-normalized (`Σ effective_share_bp + platform_cut_bp = 10 000`
  per `(bundle, vendor SKU)`); Rating attaches them to component lineage untouched and never
  re-normalizes. Settlement rounding is downstream.
- `[OPEN QUESTION]` bundle-level coupon attachment (coupons unavailable at launch, R-11).

### 4.6 AuthZ Resource and Action Catalog (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-authz-catalog-gov`

The gear's resource types, actions and role mapping are defined once in
[`../DESIGN.md`](../DESIGN.md) §4.8.

## 5. Traceability

**Traces to**: `cpt-cf-bss-rating-fr-asc606-traceable-identifiers`, `cpt-cf-bss-rating-fr-publish-approval-governance`

- **PRD**: §6.12, §7.1 `nfr-audit-segregation`, §9.2 (pricing read-model contract), AC 11.
- **Design**: [`../DESIGN.md`](../DESIGN.md) §3.3, §4.8.
- **Contracts**: [`../SEAMS.md`](../SEAMS.md) P-2, P-5, G-1, L-2, J-10; §I (G1, B1).
- **Decisions**: T-D-06, T-D-08; open R-02, R-11, R-12 — [`../DECISIONS.md`](../DECISIONS.md).
- **Slices**: [`03`](./03-metering-models.md), [`04`](./04-overlays-precedence.md), [`01`](./01-foundation.md).
