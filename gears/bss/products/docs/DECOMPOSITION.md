<!-- CONFLUENCE_TITLE: [BSS]: Products — Decomposition (PriceBook rewrite) -->
<!-- Related: ./PRD.md, ./DESIGN.md, ./DECISIONS.md | Owners: BSS Product Catalog team -->

# Decomposition: BSS Products — SKU Registry

**Overall implementation status:**
- [ ] `p1` - **ID**: `cpt-cf-bss-products-status-overall`

<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 Foundation - HIGH](#21-foundation---high)
  - [2.2 SKU & Categories - HIGH](#22-sku--categories---high)
  - [2.3 Lifecycle & Approvals - HIGH](#23-lifecycle--approvals---high)
  - [2.4 Read Model & Events - MEDIUM](#24-read-model--events---medium)
  - [2.5 Derived Usage Types - MEDIUM](#25-derived-usage-types---medium)
  - [2.6 Quota Catalog - HIGH](#26-quota-catalog---high)
  - [2.7 Quota Set Validation - HIGH](#27-quota-set-validation---high)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

## 1. Overview

Four features, one per design slice, implement the SKU registry in dependency order after the phase 1b
structural rewrite, during phase 1c of the PriceBook programme. A fifth, derived usage types, implements P-D-229 on
DESIGN §3 directly, with no slice of its own. A sixth, the quota catalog, implements the quotas package's feature 1
(P-D-266) on DESIGN §3 directly. A seventh, the quota-set validation, implements Products' part of the quotas
package's feature 2 (P-D-275) on DESIGN §3 directly. Phase 1a supplies `bss-approval`.
[DESIGN §3](DESIGN.md#3-technical-architecture) defines the architecture and schema; the features
own the 42 implementation DoDs (31 in the four slices' features, 5 in derived usage types, 5 in the quota
catalog, 1 in the quota-set validation). The source is `docs/superpowers/specs/2026-09-24-pricebook-model-design.md`
(spec §2.2, §4, §6, §7.2–§7.3 and §13), with the amendments recorded in [DECISIONS](DECISIONS.md).
Phase 0 and phase 1 remain unmerged until the phase 2 integration gate (spec §11).

## 2. Entries

### 2.1 [Foundation](features/foundation.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-products-feature-foundation`

- **Type**: Core
- **Phases**: 1c, first
- **Purpose**: Wire the crates and new migration chain for category, SKU, durable SKU versions, the four approval tables, audit and idempotency; provide If-Match, optional Idempotency-Key, audit rows and outbox writes in the act's transaction.
- **Depends On**: `bss-approval` from phase 1a and the phase 1b structural rewrite.
- **Scope**: Migrations `000001`–`000005`; entities; scoped repositories including conditional pending ownership and the unit Store; DomainError and canonical Problem mapping; settings; SQLite/Postgres parity.
- **Out of scope**: Routes; the reference-registry addition belongs to entry 2.4.
- **Design slice**: [01-foundation.md](design/01-foundation.md).
- **Requirements**: `cpt-cf-bss-products-fr-concurrency-idempotency`, `cpt-cf-bss-products-nfr-authz`, `cpt-cf-bss-products-nfr-audit`, `cpt-cf-bss-products-nfr-tenant-isolation`, `cpt-cf-bss-products-nfr-two-backends`.
- **Architecture**: `cpt-cf-bss-products-component-approvals`, `cpt-cf-bss-products-component-versions`, `cpt-cf-bss-products-component-events`; `cpt-cf-bss-products-constraint-two-backends`, `cpt-cf-bss-products-constraint-no-row-locks`.

### 2.2 [SKU & Categories](features/sku-categories.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-products-feature-sku-categories`

- **Type**: Core
- **Phases**: 1c
- **Depends On**: `cpt-cf-bss-products-feature-foundation`.
- **Purpose**: Provide draft and category authoring, SKU reads and dated versions, with SKU uniqueness, reference-guarded type changes, usage-type resolution, unpriced bundles and category retirement checks.
- **Scope**: SKU create/draft PATCH and reads, flat category CRUD/retire, immutable version reads and shared submit/apply validation.
- **Out of scope**: Approval decisions; search pagination and reference summary assembly.
- **Design slice**: [02-sku-categories.md](design/02-sku-categories.md).
- **Requirements**: `cpt-cf-bss-products-fr-sku-define`, `cpt-cf-bss-products-fr-sku-type-frozen`, `cpt-cf-bss-products-fr-sku-metering`, `cpt-cf-bss-products-fr-sku-bundle`, `cpt-cf-bss-products-fr-sku-versions`, `cpt-cf-bss-products-fr-category-flat`.
- **Architecture**: `cpt-cf-bss-products-component-registry`, `cpt-cf-bss-products-component-versions`; `cpt-cf-bss-products-principle-one-entity`.

### 2.3 [Lifecycle & Approvals](features/lifecycle-approvals.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-products-feature-lifecycle-approvals`

- **Type**: Core
- **Phases**: 1c
- **Depends On**: `cpt-cf-bss-products-feature-sku-categories`.
- **Purpose**: Implement the three ApprovalSubjects, governance and approval-unit doors, committed resumable fences, stale refresh with generations and the approval-policy door.
- **Scope**: Publish/change/retire, orphan unfence, queue/detail, approve/reject/withdraw, copied policy quorum, author SoD, conditional unit writes and every terminal audit/event path.
- **Out of scope**: Pricing's approval subjects and caller-side reference protocol. Fence safety requires entry 2.4 before release.
- **Design slice**: [03-lifecycle-approvals.md](design/03-lifecycle-approvals.md).
- **Requirements**: `cpt-cf-bss-products-fr-sku-descriptors`, `cpt-cf-bss-products-fr-sku-lifecycle`, `cpt-cf-bss-products-fr-sku-retire-fenced`, `cpt-cf-bss-products-fr-approval-units`.
- **Architecture**: `cpt-cf-bss-products-component-approvals`; `cpt-cf-bss-products-principle-fence-before-count`, `cpt-cf-bss-products-principle-business-content-fingerprint`, `cpt-cf-bss-products-constraint-approval-shape`; `cpt-cf-bss-products-seq-gl-change`, `cpt-cf-bss-products-seq-fenced-retire`, `cpt-cf-bss-products-seq-stale-refresh`.

### 2.4 [Read Model & Events](features/read-model-events.md) - MEDIUM

- [ ] `p1` - **ID**: `cpt-cf-bss-products-feature-read-model-events`

- **Type**: Supporting
- **Phases**: 1c
- **Depends On**: `cpt-cf-bss-products-feature-lifecycle-approvals`.
- **Purpose**: Complete list/search, the local reference registry and summaries, the kept catalog browse transport and transactional event payloads.
- **Scope**: Read indexes; `sku_reference` and reserve/confirm/release; reciprocal reserve/fence guards; published-only ProductCatalogClientV1 mapping; SkuPublished, SkuChanged, SkuRetired and ApprovalUnitDecided, plus the specified ReferenceForceReleased on operator release.
- **Out of scope**: Pricing's reserve/write/confirm implementation (phase 2), Rating and Subscriptions adaptations.
- **Design slice**: [04-read-model-events.md](design/04-read-model-events.md).
- **Requirements**: `cpt-cf-bss-products-fr-read-model`, `cpt-cf-bss-products-fr-reference-registry`, `cpt-cf-bss-products-fr-events`.
- **Architecture**: `cpt-cf-bss-products-component-read-model`, `cpt-cf-bss-products-component-references`, `cpt-cf-bss-products-component-events`; `cpt-cf-bss-products-seq-reserve-write-confirm`.

### 2.5 [Derived Usage Types](features/derived-usage-types.md) - MEDIUM

- [ ] `p1` - **ID**: `cpt-cf-bss-products-feature-derived-usage-types`

- **Type**: Supporting
- **Phases**: the derived usage types plan (P-D-229), on the seam branch, after phase 1c
- **Depends On**: `cpt-cf-bss-products-feature-foundation`; the SDK's `derived` module (P-D-230).
- **Purpose**: Store derived usage types and their append-only versions, serve them to catalog and pricing authors, let a usage SKU pin one at its first publish, and answer pricing's meter semantics for them.
- **Scope**: Migration `000012`; the type and version repository; the declaration's door rules, digest and input resolution; five REST doors, the `derived_usage_type` PDP resource and the audit rows (P-D-231); a usage SKU's derived ref, judged from the store before any catalog call, and its pin (P-D-232); the meter-semantics dispatcher registered for pricing, answering derived meters from the store and every other meter as unconfigured (P-D-233).
- **Out of scope**: The raw-meter provider (E1a), which the dispatcher would call; Rating's evaluation.
- **Design slice**: none; [DESIGN](DESIGN.md) §3.1, §3.3, §3.5 and §3.7 design it.
- **Requirements**: `cpt-cf-bss-products-fr-derived-usage-type`.
- **Architecture**: `cpt-cf-bss-products-component-registry`, `cpt-cf-bss-products-component-events`; `cpt-cf-bss-products-constraint-two-backends`.

### 2.6 [Quota Catalog](features/quota-catalog.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-products-feature-quota-catalog`

- **Type**: Supporting
- **Phases**: the quotas programme, after the derived usage types
- **Depends On**: `cpt-cf-bss-products-feature-foundation`; types-registry's `TypesRegistryApi`.
- **Purpose**: Turn authorized adapter evidence and manual records into reviewed, versioned quota definitions a plan author can select (quotas package feature 1, `cpt-cf-bss-quotas-feature-catalog`).
- **Scope**: Migration `000015`; the quota GTS schemas and discovery plugin specification in `products-sdk`, with no metric, unit or policy instance, which adapter owners register (P-D-267); adapter targets, discovery intake, candidates, review, definition versions and availability slots; two catalog reads and `ProductsQuotaCatalogClientV1` (P-D-266 … P-D-274).
- **Out of scope**: A discovery adapter and its instances; qualification records and sale availability decisions; the quota-set validation Pricing calls (entry 2.7).
- **Design slice**: none; [DESIGN](DESIGN.md) §3.1, §3.2, §3.3, §3.4, §3.5 and §3.7 design it.
- **Requirements**: `cpt-cf-bss-products-fr-quota-catalog`, `cpt-cf-bss-products-nfr-authz`, `cpt-cf-bss-products-nfr-audit`, `cpt-cf-bss-products-nfr-tenant-isolation`, `cpt-cf-bss-products-nfr-two-backends`.
- **Architecture**: `cpt-cf-bss-products-component-quota-catalog`; `cpt-cf-bss-products-constraint-two-backends`.

### 2.7 [Quota Set Validation](features/quota-set-validation.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-products-feature-quota-set-validation`

- **Type**: Supporting
- **Phases**: the quotas programme, after the quota catalog
- **Depends On**: `cpt-cf-bss-products-feature-quota-catalog`.
- **Purpose**: Validate a complete set of plan quota rules for Pricing (quotas package feature 2, Products' part).
- **Scope**: The SDK method and its judgement (P-D-275, P-D-276); no migration.
- **Out of scope**: Plan rules themselves (Pricing); qualification records (feature 5).
- **Design slice**: none; [DESIGN](DESIGN.md) §3.2, §3.3 and §3.4 design it.
- **Requirements**: `cpt-cf-bss-products-fr-quota-set-validation`, `cpt-cf-bss-products-nfr-authz`, `cpt-cf-bss-products-nfr-tenant-isolation`.
- **Architecture**: `cpt-cf-bss-products-component-quota-catalog`; `cpt-cf-bss-products-constraint-two-backends`.

## 3. Feature Dependencies

foundation → sku-categories → lifecycle-approvals → read-model-events

foundation → derived-usage-types

foundation → quota-catalog → quota-set-validation

This is implementation order, not independent release order. Type changes and retirement require both
the local registry and reciprocal guards from the last feature; a remote count is never a substitute.
All four features integrate before the Products phase gate. Pricing's durable reserve/write/confirm
and cancellation/release protocol must pass the phase 2 integration gate before merging the programme.
