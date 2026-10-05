<!-- CONFLUENCE_TITLE: [BSS]: Products — SKU & Categories (Feature) -->
<!-- Related: ../DECOMPOSITION.md, ../DESIGN.md, ../PRD.md | Owners: BSS Product Catalog team -->

# Feature: SKU & Categories

- [ ] `p1` - **ID**: `cpt-cf-bss-products-featstatus-sku-categories-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-bss-products-feature-sku-categories`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Author drafts a SKU](#author-drafts-a-sku)
  - [Administrator maintains categories](#administrator-maintains-categories)
  - [Consumer reads the version in force](#consumer-reads-the-version-in-force)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [sku-unique](#sku-unique)
  - [sku-type-frozen](#sku-type-frozen)
  - [usage-type-resolves](#usage-type-resolves)
  - [bundle-unpriced](#bundle-unpriced)
  - [category-retire-refused](#category-retire-refused)
  - [Versions as-of](#versions-as-of)
- [4. States (CDSL)](#4-states-cdsl)
  - [SKU & Categories states](#sku--categories-states)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Unique SKU creation and draft editing](#unique-sku-creation-and-draft-editing)
  - [Live references freeze SKU type](#live-references-freeze-sku-type)
  - [Usage metering resolves through the retained port](#usage-metering-resolves-through-the-retained-port)
  - [Bundle identity is unpriced](#bundle-identity-is-unpriced)
  - [Version history and as-of reads](#version-history-and-as-of-reads)
  - [Flat category CRUD](#flat-category-crud)
  - [Referenced categories cannot retire](#referenced-categories-cannot-retire)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

This phase 1c feature implements [design slice 02](../design/02-sku-categories.md).
The implementation order and integration
prerequisites are in [DECOMPOSITION](../DECOMPOSITION.md); [DESIGN §3](../DESIGN.md#3-technical-architecture)
is the architecture and schema authority. Unchecked items describe implementation obligations.

### 1.2 Purpose

Provide independent SKU and flat-category authoring, shared type/metering rules and immutable dated version reads. Published content uses the approval feature; the current head never substitutes for dated truth.

Requirements: `cpt-cf-bss-products-fr-sku-define`, `cpt-cf-bss-products-fr-sku-type-frozen`, `cpt-cf-bss-products-fr-sku-metering`, `cpt-cf-bss-products-fr-sku-bundle`, `cpt-cf-bss-products-fr-sku-versions`, `cpt-cf-bss-products-fr-category-flat`, `cpt-cf-bss-products-fr-concurrency-idempotency`.

Design principles: `cpt-cf-bss-products-principle-one-entity`.

### 1.3 Actors

`cpt-cf-bss-products-actor-catalog-admin`, `cpt-cf-bss-products-actor-pricing`, `cpt-cf-bss-products-actor-auditor`. Authenticated doors enforce the applicable products:read, author, submit,
approve or settings permission and tenant scope; holding multiple grants never bypasses SoD.

### 1.4 References

- [PRD](../PRD.md), especially §9's numbered acceptance criteria cited below.
- [DESIGN](../DESIGN.md), §3's model, API, transactions and schema.
- [Slice 02](../design/02-sku-categories.md), including API/data details and the matching instruction names.
- [DECISIONS](../DECISIONS.md), P-D-184–194, including the superseding reservation, version and generation decisions.
- Source “spec”: `docs/superpowers/specs/2026-09-24-pricebook-model-design.md` in the main checkout, §2.2, §4, §6, §7.2–§7.3 and §13.

## 2. Actor Flows (CDSL)

### Author drafts a SKU

- [ ] `p1` - **ID**: `cpt-cf-bss-products-flow-sku-categories-author-drafts-a-sku`

1. [ ] - `p1` - Author reads the tenant's categories and creates an independent SKU with code, name, type, an optional category and initial content; authenticate products:author and resolve optional POST replay first - `inst-sku-create-input`
2. [ ] - `p1` - Resolve a given category in tenant scope (an omitted one stays null, with no default fallback), validate type-specific fields, apply the draft-save catalog posture, then insert under the separate code/name unique indexes - `inst-sku-create-validate`
3. [ ] - `p1` - Commit the draft with revision 1, published_version 0 (revision is the concurrency version) and created_by; return its id and ETag - `inst-sku-create-commit`
4. [ ] - `p1` - For subsequent PATCH, require If-Match, draft lifecycle and no pending unit; draft type changes need no fence because drafts cannot be reserved - `inst-sku-patch-guards`
5. [ ] - `p1` - Conditionally update content and increment revision; preserve published_version until publication; hand publication to slice 03 - `inst-sku-patch-commit`

### Administrator maintains categories

- [ ] `p1` - **ID**: `cpt-cf-bss-products-flow-sku-categories-administrator-maintains-categories`

1. [ ] - `p1` - Create or rename a tenant category directly with products:author; category code uniqueness is tenant-local and no approval unit is created - `inst-sku-category-write`
2. [ ] - `p1` - PATCH checks If-Match and increments version; a stale precondition returns STALE_REVISION without changing the category - `inst-sku-category-patch`
3. [ ] - `p1` - Retire only when no tenant SKU that is not retired points at the category, checking the predicate atomically with the write (P-D-208) - `inst-sku-category-retire`

### Consumer reads the version in force

- [ ] `p1` - **ID**: `cpt-cf-bss-products-flow-sku-categories-consumer-reads-the-version-in-force`

1. [ ] - `p1` - Authorize products:read and scope the SKU before querying history for as_of - `inst-sku-version-scope`
2. [ ] - `p1` - Select the latest effective date not after as_of, breaking ties by published_version; return the immutable snapshot or NO_VERSION_IN_FORCE - `inst-sku-version-read`
3. [ ] - `p1` - Pricing binds that snapshot's version and descriptors for the period start; a later change never rewrites the previous period's binding - `inst-sku-version-bind`

## 3. Processes / Business Logic (CDSL)

### sku-unique

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-sku-categories-sku-unique`

1. [ ] - `p1` - Validate code and name separately and insert/update under UNIQUE (tenant_id, code) and UNIQUE (tenant_id, name); a preflight lookup alone is not enforcement - `inst-sku-unique-write`
2. [ ] - `p1` - Map the losing code race to 409 SKU_CODE_TAKEN and the losing name race to 409 SKU_NAME_TAKEN; roll back the draft mutation - `inst-sku-unique-conflict`

### sku-type-frozen

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-sku-categories-sku-type-frozen`

1. [ ] - `p1` - For published/deprecated type changes, reject pending ownership and require a fence guarded by absence of reserved/confirmed references; any live reference yields SKU_TYPE_FROZEN. Drafts cannot be reserved and change type without fencing - `inst-sku-type-check`
2. [ ] - `p1` - Set type_change_pending and durable fence metadata through slice 03's atomic fence-and-submit protocol; concurrent reserve is excluded by slice 04's reciprocal transaction - `inst-sku-type-fence`
3. [ ] - `p1` - For a draft, revalidate target-type fields and conditionally apply the direct edit without fencing, bumping revision; published/deprecated changes submit sku_change in the fence transaction - `inst-sku-type-apply`
4. [ ] - `p1` - Interrupted operations recover using fence_op_id and the orphan TTL; failures must not silently remove another operation's fence - `inst-sku-type-recover`

### usage-type-resolves

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-sku-categories-usage-type-resolves`

1. [ ] - `p1` - Permit usage drafts to remain incomplete; publication requires both usage_type_ref and unit, otherwise USAGE_NEEDS_METER - `inst-sku-meter-required`
2. [ ] - `p1` - Resolve using the retained UsageTypeCatalog port: registered catalog, usage-collector adapter, then configured local-development catalog or unconfigured mode, preserving provenance - `inst-sku-meter-port`
3. [ ] - `p1` - On draft save, check a changed ref when a catalog is configured; definitive unresolved is 400 USAGE_TYPE_UNRESOLVED, while a catalog non-answer does not block the save - `inst-sku-meter-draft`
4. [ ] - `p1` - At submit and apply, revalidate the proposed usage content and fail closed on unresolved refs; an unreachable configured catalog is 503, not acceptance; a catalog that refuses the caller is 403 USAGE_TYPE_FORBIDDEN (P-D-207) - `inst-sku-meter-publish`
5. [ ] - `p1` - Refuse usage metering on non-usage types; a bundle with metering yields BUNDLE_HAS_NO_METER - `inst-sku-meter-type`

### bundle-unpriced

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-sku-categories-bundle-unpriced`

1. [ ] - `p1` - Persist bundle identity and descriptors without component members or usage metering - `inst-sku-bundle-content`
2. [ ] - `p1` - Expose bundle type to Pricing so it cannot create a price book entry or plan item for it; a plan may be sold_as that SKU - `inst-sku-bundle-pricing`
3. [ ] - `p1` - Apply the same reference barrier to sold_as reservations; bundle type does not bypass retire/type fencing - `inst-sku-bundle-reference`

### category-retire-refused

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-sku-categories-category-retire-refused`

1. [ ] - `p1` - Within the tenant transaction, refuse retirement while a SKU in draft, published, deprecated or retiring points to the category; return 409 CATEGORY_IN_USE; a retired SKU and a SKU without a category never count, and a retired category is 409 CATEGORY_RETIRED (P-D-208, amending P-D-186) - `inst-sku-category-count`
2. [ ] - `p1` - Otherwise conditionally set status retired and increment version without an approval unit; category assignment and retirement must serialize their reciprocal checks so a concurrent assignment cannot bypass this rule - `inst-sku-category-retire-write`

### Versions as-of

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-sku-categories-versions-as-of`

1. [ ] - `p1` - On publish or applied change, validate effective_from against the latest version date; an earlier date yields 409 VERSION_ORDER, equal dates are allowed - `inst-sku-version-order`
2. [ ] - `p1` - Increment published_version and append the complete business snapshot in the apply transaction; publication is effective immediately, changes use effective_from defaulting to today - `inst-sku-version-append`
3. [ ] - `p1` - For a dated read filter effective_from <= as_of, order by effective_from DESC, published_version DESC and select one; before the first version return 404 NO_VERSION_IN_FORCE - `inst-sku-version-select`
4. [ ] - `p1` - The history read returns the durable history as an array, and the dated read one version at its own path (P-D-214); never substitute the current SKU row for a version-in-force read or update an old snapshot - `inst-sku-version-history`

## 4. States (CDSL)

### SKU & Categories states

- [ ] `p1` - **ID**: `cpt-cf-bss-products-state-sku-categories`

1. [ ] - `p1` - SKU create → draft; draft PATCH stays draft and changes revision, not published_version. A pending unit excludes direct edits.
2. [ ] - `p1` - Published/deprecated content changes go through sku_change; no direct PATCH can bypass review. Slice 03 owns the lifecycle edges and fences.
3. [ ] - `p1` - Category create → active; direct edits retain status; active → retired requires no referencing SKU that is not retired (P-D-208); retired → retired is refused CATEGORY_RETIRED. No category hierarchy or approval lifecycle exists.
4. [ ] - `p1` - SKU version history grows only on publication/applied change; stored versions have no edit/delete transition.

## 5. Definitions of Done

These definitions own this feature's 7 DoDs; the design slice references them without redefining them.
Design constraints: `cpt-cf-bss-products-constraint-two-backends`.

### Unique SKU creation and draft editing

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-sku-create-unique`

Verified at `b74e8783b49b03fa827f1052a99cf6553683f4aa`; implementation marker in `products/src/api/rest/skus.rs`.

Create persists an independent draft with tenant-scoped code and name uniqueness, creator attribution, revision as the ETag/If-Match/CAS concurrency version and published_version as the snapshot counter. Direct PATCH changes drafts only under If-Match and null pending ownership; unique-index collisions map to SKU_CODE_TAKEN or SKU_NAME_TAKEN and pending ownership to ROW_LOCKED_PENDING. Published/deprecated changes use sku_change (spec §4, §6, §7.2).

### Live references freeze SKU type

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-sku-type-frozen`

Verified at `b74e8783b49b03fa827f1052a99cf6553683f4aa`; implementation marker in `products/src/domain/approvals/change.rs`.

A draft is never priced and cannot have a reservation, so its type changes freely through draft PATCH without a fence, subject to content validation and pending ownership. Only published/deprecated type changes use the local reference barrier and sku_change: any reserved or confirmed price book entry, plan_item or sold_as reference refuses the fence with SKU_TYPE_FROZEN. Fence and submission share one transaction, with apply revalidation (spec decision 17; DESIGN §3.1).

### Usage metering resolves through the retained port

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-usage-type-resolves`

Verified at `4c5577f1cb08d072e79880599a3ae1db8ed8d1e0`; implementation marker in `products/src/api/rest/governance.rs`.

Usage publication requires usage_type_ref and unit and resolves through the retained UsageTypeCatalog port at submit and apply. Draft save checks a changed ref when configured: definitive unresolved is 400 USAGE_TYPE_UNRESOLVED, while a catalog non-answer does not block saving; submit/apply fail closed, with an unreachable configured catalog returning 503 and a catalog that refuses the caller (it is read as the caller) returning 403 USAGE_TYPE_FORBIDDEN (P-D-207). Resolution order/provenance and resolvability-only semantics remain as P-D-184 and DESIGN §3.5 specify (spec §4, §6, §15). A draft may still change its ref. A published usage SKU keeps its ref and its unit (P-D-258): a change of either, a clear of either, or a type change away from usage is 400 METERING_IMMUTABLE at the change door, before any catalog is asked, and 409 at apply, except a raw meter moving onto the identity wrapper of that meter in the same unit (P-D-251). A usage SKU names a derived usage type (P-D-259). A raw ref is 400 DERIVED_USAGE_TYPE_REQUIRED before any catalog is asked. The unit is the derived type's output unit: a write does not store it, and a read serves it.

### Bundle identity is unpriced

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-bundle-unpriced`

Verified at `b74e8783b49b03fa827f1052a99cf6553683f4aa`; implementation marker in `products/src/domain/sku.rs`.

Products stores and serves bundle identity, type and descriptors without composition or metering; metering assignment fails with BUNDLE_HAS_NO_METER. Products serves the bundle type to consumers and applies the ordinary reference barrier to sold_as reservations (spec §4, §13; DESIGN §3.1).

### Version history and as-of reads

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-versions-as-of`

Verified at `b74e8783b49b03fa827f1052a99cf6553683f4aa`; implementation marker in `products/src/infra/storage/repo/version_repo.rs`.

GET versions returns the immutable history as an array; GET versions/as-of?date= returns the latest effective_from not after that date and then the highest published_version (P-D-214). Publish is immediate and applied changes append dated snapshots; earlier-than-latest dates fail with VERSION_ORDER, equal dates are allowed and dates before publication return NO_VERSION_IN_FORCE. The latest head may be future-effective and never replaces the dated read (spec §2.2, §4, §7.2; DESIGN §3.3, §3.7).

### Flat category CRUD

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-category-flat-crud`

Verified at `4c5577f1cb08d072e79880599a3ae1db8ed8d1e0`; implementation marker in `products/src/api/rest/categories.rs`.

Categories expose code, name, is_default, sort_order and active/retired status, with at most one tenant-qualified category per SKU (P-D-196: optional, no default fallback) and no hierarchy. GET/POST/PATCH use scoped reads, tenant-unique code and If-Match on patch; a tenant has at most one default, and `is_default: true` on POST or PATCH moves it in one transaction, a lost race being 409 CATEGORY_DEFAULT_TAKEN (P-D-218); a retired category never becomes the default (PATCH `is_default: true` on it is 409 CATEGORY_RETIRED), and retiring the default clears it in the retirement's transaction, audited, leaving the tenant without one (P-D-220); creation and rename are direct operations without approval units (spec §2 decision 12, §4, §7.2; DESIGN §3.1, §3.3). GET categories pages on the toolkit's OData in sort_order then code, 200 to a page, and GET categories/{id} reads one with its ETag; each carries sku_count, the SKUs that are not retired naming it, from one grouped count (P-D-215). The list answers 304 when If-None-Match matches a weak ETag of its JSON, and sends Cache-Control: private, no-cache (P-D-261). A retired category may be archived and unarchived under If-Match, audited (`POST categories/{id}/archive`, `POST categories/{id}/unarchive`, the 200's declared `ETag` the next If-Match; an active one is 409 CATEGORY_NOT_RETIRED): GET categories leaves an archived category out unless `$filter` asks `archived eq true`, and GET categories/{id} ignores the mark (P-D-263).

### Referenced categories cannot retire

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-category-retire-refused`

Verified at `4c5577f1cb08d072e79880599a3ae1db8ed8d1e0`; implementation marker in `products/src/api/rest/categories.rs`.

Category retirement refuses with CATEGORY_IN_USE while a SKU in draft, published, deprecated or retiring references it; a retired SKU and a SKU without a category never count (P-D-196, P-D-208, which amends P-D-186), and retiring a retired category is CATEGORY_RETIRED. Otherwise it directly retires the category and advances version; assignment and retirement serialize their reciprocal checks so a concurrent assignment cannot bypass the rule (spec §4, §7.2; DESIGN §3.1; slice 02 §3).

**Owed by pricing (phase 2).** Pricing must refuse bundle price book entries and plan items and allow a bundle only as a plan’s sold_as identity.

## 6. Acceptance Criteria

Each criterion below corresponds to exactly one DoD above and cites [PRD §9](../PRD.md#9-acceptance-criteria).
Verify these during phase 1c on SQLite and Postgres, including tenant isolation and denied permissions;
this document does not claim those implementation tests have run. Pricing-side assertions are contract
obligations here and integration checks when its phase 2 caller path exists.

| DoD | PRD trace | Given / When / Then |
| --- | --- | --- |
| `cpt-cf-bss-products-dod-sku-create-unique` | AC #1, #4, #19, #27; `cpt-cf-bss-products-fr-sku-define`, `cpt-cf-bss-products-fr-concurrency-idempotency` | Given a tenant category and unused SKU identity, when an author creates and patches a draft with current If-Match, then the draft and new ETag persist; concurrent code/name reuse fails with SKU_CODE_TAKEN/SKU_NAME_TAKEN, stale PATCH fails with STALE_REVISION, and a locked edit fails with ROW_LOCKED_PENDING. |
| `cpt-cf-bss-products-dod-sku-type-frozen` | AC #2, #23; `cpt-cf-bss-products-fr-sku-type-frozen` | Given an unreferenced draft, when its type changes without a fence, then target-type validation and the update succeed; given a published/deprecated SKU with a reserved or confirmed price book entry, plan_item or sold_as reference, a type-change request fails with SKU_TYPE_FROZEN without changing type or acquiring a fence. |
| `cpt-cf-bss-products-dod-usage-type-resolves` | AC #5; `cpt-cf-bss-products-fr-sku-metering` | Given a usage draft and a resolving configured catalog, when complete metering is submitted and applied, then publication succeeds; missing fields give USAGE_NEEDS_METER, an unresolved ref gives USAGE_TYPE_UNRESOLVED, and catalog outage at submit/apply gives 503 without publication (a catalog refusing the caller 403 USAGE_TYPE_FORBIDDEN, P-D-207), while a draft-save non-answer remains saveable. |
| `cpt-cf-bss-products-dod-bundle-unpriced` | AC #6, #23; `cpt-cf-bss-products-fr-sku-bundle` | Given a bundle SKU, when it is read for a sold_as relationship, then its bundle identity and descriptors are available without composition; assigning usage metering fails with BUNDLE_HAS_NO_METER, and Pricing contract checks refuse pricing or plan-item use rather than treating it as another charge kind. |
| `cpt-cf-bss-products-dod-versions-as-of` | AC #8, #9; `cpt-cf-bss-products-fr-sku-versions` | Given publication September 24 and an applied change effective October 1, when as_of is September 30, October 1 or September 23, then return the old snapshot, new snapshot or NO_VERSION_IN_FORCE respectively; September 30 proposed after the October version fails with VERSION_ORDER, while another October 1 version wins by its higher number. |
| `cpt-cf-bss-products-dod-category-flat-crud` | AC #13, #27; `cpt-cf-bss-products-fr-category-flat`, `cpt-cf-bss-products-fr-concurrency-idempotency` | Given a tenant category, when it is created or renamed with a current ETag, then the flat record changes with no approval unit; stale PATCH returns STALE_REVISION, duplicate tenant code is refused, and cross-tenant category assignment fails. |
| `cpt-cf-bss-products-dod-category-retire-refused` | AC #13; `cpt-cf-bss-products-fr-category-flat` | Given referenced and unreferenced categories, when retirement is requested, then the first fails with CATEGORY_IN_USE and the second retires without a unit (a category whose SKUs are all retired is unreferenced, P-D-208; a retired one fails CATEGORY_RETIRED); racing assignment and retirement cannot leave a newly assigned SKU on a category whose retirement check passed as unreferenced. |
