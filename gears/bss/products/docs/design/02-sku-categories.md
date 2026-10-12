<!-- CONFLUENCE_TITLE: [BSS]: Products — SKU & Categories (Design, Slice 2) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../DECISIONS.md | Owners: BSS Product Catalog team -->

# DESIGN — SKU & Categories (Slice 2)

- [ ] `p1` - **ID**: `cpt-cf-bss-products-design-slice-02`

<!-- toc -->

- [1. Context](#1-context)
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
  - [versions-as-of](#versions-as-of)
- [4. States (CDSL)](#4-states-cdsl)
- [5. API Surface](#5-api-surface)
- [6. Data Model](#6-data-model)
- [7. Events & Alarms](#7-events--alarms)
- [8. Definitions of Done](#8-definitions-of-done)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Non-Functional Considerations](#10-non-functional-considerations)

<!-- /toc -->

## 1. Context

This slice implements Registry authoring and Versions reads from [DESIGN §3.2](../DESIGN.md#32-component-model).
It depends on [Foundation](01-foundation.md) and provides subject validation to
[Lifecycle & Approvals](03-lifecycle-approvals.md). [Read Model & Events](04-read-model-events.md)
adds search, reference summaries and retained browse. The slice map is DESIGN §4.

A SKU has no Product parent. Categories are flat, at most one per SKU (P-D-196); bundles have no composition here.
Spec §2.2 and §4 are the content authority, with §6 for submit/apply validation and §7.2 for doors
(spec means `docs/superpowers/specs/2026-09-24-pricebook-model-design.md` in the main checkout).
[DECISIONS](../DECISIONS.md) P-D-184–188 and P-D-191 fix metering, identity, type and version behavior.

## 2. Actor Flows (CDSL)

### Author drafts a SKU

1. [ ] - `p1` - Author reads the tenant's categories and creates an independent SKU with code, name, type, an optional category and initial content; authenticate products:author and resolve optional POST replay first - `inst-sku-create-input`
2. [ ] - `p1` - Resolve a given category in tenant scope (an omitted one stays null, with no default fallback), validate type-specific fields, apply the draft-save catalog posture, then insert under the separate code/name unique indexes - `inst-sku-create-validate`
3. [ ] - `p1` - Commit the draft with revision 1, published_version 0 (revision is the concurrency version) and created_by; return its id and ETag - `inst-sku-create-commit`
4. [ ] - `p1` - For subsequent PATCH, require If-Match, draft lifecycle and no pending unit; draft type changes need no fence because drafts cannot be reserved - `inst-sku-patch-guards`
5. [ ] - `p1` - Conditionally update content and increment revision; preserve published_version until publication; hand publication to slice 03 - `inst-sku-patch-commit`

### Administrator maintains categories

1. [ ] - `p1` - Create or rename a tenant category directly with products:author; category code uniqueness is tenant-local and no approval unit is created - `inst-sku-category-write`
2. [ ] - `p1` - PATCH checks If-Match and increments version; a stale precondition returns STALE_REVISION without changing the category - `inst-sku-category-patch`
3. [ ] - `p1` - Retire only when no tenant SKU that is not retired points at the category, checking the predicate atomically with the write (P-D-208) - `inst-sku-category-retire`

### Consumer reads the version in force

1. [ ] - `p1` - Authorize products:read and scope the SKU before querying history for as_of - `inst-sku-version-scope`
2. [ ] - `p1` - Select the latest effective date not after as_of, breaking ties by published_version; return the immutable snapshot or NO_VERSION_IN_FORCE - `inst-sku-version-read`
3. [ ] - `p1` - Pricing binds that snapshot's version and descriptors for the period start; a later change never rewrites the previous period's binding - `inst-sku-version-bind`

## 3. Processes / Business Logic (CDSL)

### sku-unique

1. [ ] - `p1` - Validate code and name separately and insert/update under UNIQUE (tenant_id, code) and UNIQUE (tenant_id, name); a preflight lookup alone is not enforcement - `inst-sku-unique-write`
2. [ ] - `p1` - Map the losing code race to 409 SKU_CODE_TAKEN and the losing name race to 409 SKU_NAME_TAKEN; roll back the draft mutation - `inst-sku-unique-conflict`

### sku-type-frozen

1. [ ] - `p1` - For published/deprecated type changes, reject pending ownership and require a fence guarded by absence of reserved/confirmed references; any live reference yields SKU_TYPE_FROZEN. Drafts cannot be reserved and change type without fencing - `inst-sku-type-check`
2. [ ] - `p1` - Set type_change_pending and durable fence metadata through slice 03's atomic fence-and-submit protocol; concurrent reserve is excluded by slice 04's reciprocal transaction - `inst-sku-type-fence`
3. [ ] - `p1` - For a draft, revalidate target-type fields and conditionally apply the direct edit without fencing, bumping revision; published/deprecated changes submit sku_change in the fence transaction - `inst-sku-type-apply`
4. [ ] - `p1` - Interrupted operations recover using fence_op_id and the orphan TTL; failures must not silently remove another operation's fence - `inst-sku-type-recover`

### usage-type-resolves

1. [ ] - `p1` - Permit usage drafts to remain incomplete; publication requires both usage_type_ref and unit, otherwise USAGE_NEEDS_METER - `inst-sku-meter-required`
2. [ ] - `p1` - Resolve using the retained UsageTypeCatalog port: registered catalog, usage-collector adapter, then configured local-development catalog or unconfigured mode, preserving provenance - `inst-sku-meter-port`
3. [ ] - `p1` - On draft save, check a changed ref when a catalog is configured; definitive unresolved is 400 USAGE_TYPE_UNRESOLVED, while a catalog non-answer does not block the save - `inst-sku-meter-draft`
4. [ ] - `p1` - At submit and apply, revalidate the proposed usage content and fail closed on unresolved refs; an unreachable configured catalog is 503, not acceptance; a catalog that refuses the caller is 403 USAGE_TYPE_FORBIDDEN (P-D-207) - `inst-sku-meter-publish`
5. [ ] - `p1` - Refuse usage metering on non-usage types; a bundle with metering yields BUNDLE_HAS_NO_METER - `inst-sku-meter-type`

### bundle-unpriced

1. [ ] - `p1` - Persist bundle identity and descriptors without component members or usage metering - `inst-sku-bundle-content`
2. [ ] - `p1` - Expose bundle type to Pricing so it cannot create a price book entry or plan item for it; a plan may be sold_as that SKU - `inst-sku-bundle-pricing`
3. [ ] - `p1` - Apply the same reference barrier to sold_as reservations; bundle type does not bypass retire/type fencing - `inst-sku-bundle-reference`

### category-retire-refused

1. [ ] - `p1` - Within the tenant transaction, refuse retirement while a SKU in draft, published, deprecated or retiring points to the category; return 409 CATEGORY_IN_USE; a retired SKU and a SKU without a category never count, and a retired category is 409 CATEGORY_RETIRED (P-D-208, amending P-D-186) - `inst-sku-category-count`
2. [ ] - `p1` - Otherwise conditionally set status retired and increment version without an approval unit; category assignment and retirement must serialize their reciprocal checks so a concurrent assignment cannot bypass this rule - `inst-sku-category-retire-write`

### versions-as-of

1. [ ] - `p1` - On publish or applied change, validate effective_from against the latest version date; an earlier date yields 409 VERSION_ORDER, equal dates are allowed - `inst-sku-version-order`
2. [ ] - `p1` - Increment published_version and append the complete business snapshot in the apply transaction; publication is effective immediately, changes use effective_from defaulting to today - `inst-sku-version-append`
3. [ ] - `p1` - For a dated read filter effective_from <= as_of, order by effective_from DESC, published_version DESC and select one; before the first version return 404 NO_VERSION_IN_FORCE - `inst-sku-version-select`
4. [ ] - `p1` - The history read returns the durable history as an array, and the dated read one version at its own path (P-D-214); never substitute the current SKU row for a version-in-force read or update an old snapshot - `inst-sku-version-history`

## 4. States (CDSL)

1. [ ] - `p1` - SKU create → draft; draft PATCH stays draft and changes revision, not published_version. A pending unit excludes direct edits.
2. [ ] - `p1` - Published/deprecated content changes go through sku_change; no direct PATCH can bypass review. Slice 03 owns the lifecycle edges and fences.
3. [ ] - `p1` - Category create → active; direct edits retain status; active → retired requires no referencing SKU that is not retired (P-D-208); retired → retired is refused CATEGORY_RETIRED. No category hierarchy or approval lifecycle exists.
4. [ ] - `p1` - SKU version history grows only on publication/applied change; stored versions have no edit/delete transition.

## 5. API Surface

All paths are relative to `/bss-products/v1`; fields and queries use snake_case. Authenticated
OperationBuilder doors use the Foundation Problem mapping and optional POST Idempotency-Key.

| Route | Permission and behavior |
| --- | --- |
| `POST /skus` | products:author; 201 draft with id and ETag; `category_id` is optional and an omitted one is null; code/name conflicts are 409. |
| `PATCH /skus/{id}` | products:author; draft only, If-Match required; `category_id: null` clears the category; pending ownership returns ROW_LOCKED_PENDING; stale version returns STALE_REVISION. |
| `DELETE /skus/{id}` | products:author; a never-published draft only, by its author (403 NOT_DRAFT_AUTHOR), If-Match required; 204 with an audit row; SKU_NOT_DRAFT, ROW_LOCKED_PENDING, SKU_REFERENCED or STALE_REVISION (409). A draft is deleted, never retired (P-D-206). |
| `GET /usage-types?q&kind&limit&cursor` | products:author; the usage-type catalog read as the caller, `{ source, items, page_info }`; 403 when the catalog refuses the caller, 501 unconfigured, 503 unreachable. `q` (case-insensitive substring of the id) is products' own search over the collector: `kind eq` only is sent, at most 1000 types are walked, the cursor is bound to `q` and `kind`; past 1000, 503 `USAGE_TYPE_CATALOG_TOO_LARGE` (P-D-207). A page answers `Cache-Control: private, max-age=60` (P-D-247). |
| `GET /skus` | products:read; scoped current heads. Slice 04 owns search, filters and pagination. |
| `GET /skus/{id}` | products:read; current SKU and ETag. Current applied content may be future-effective; use versions for dated truth. |
| `GET /skus/{id}/versions` | products:read; the history, always an array, oldest first; any query key is 400 (P-D-214). |
| `GET /skus/{id}/versions/as-of?date=<date>` | products:read; one version in force, or 404 NO_VERSION_IN_FORCE; a missing or malformed `date` is 400 (P-D-214). |
| `GET /categories?$filter&$orderby&$top&cursor` | products:read; one page of the tenant's categories on the toolkit's OData: `$filter` over id, code, name, status, is_default and sort_order; `$orderby` sort_order, code or name (tie-break id; default sort_order, then code); `$top`/`limit` 200, clamped at 200; each item with `sku_count`, from one grouped count (P-D-215). |
| `GET /categories/{id}` | products:read; one category with its ETag and `sku_count`; 404 off the tenant (P-D-215). |
| `POST /categories` | products:author; create directly with tenant-unique code; `is_default: true` moves the tenant's default to it (P-D-218). |
| `PATCH /categories/{id}` | products:author; direct edit under If-Match; return the new ETag. `is_default: true` moves the default: the previous holder is cleared (new version, its own audit row) in the same transaction; a lost race is 409 CATEGORY_DEFAULT_TAKEN (P-D-218). On a retired category `is_default: true` is 409 CATEGORY_RETIRED, after the If-Match check, and nothing moves (P-D-220). |
| `POST /categories/{id}/retire` | products:author; retire only while no SKU that is not retired references it, otherwise CATEGORY_IN_USE; a retired category is CATEGORY_RETIRED (P-D-208). Retiring the tenant's default first clears it (its own version and `category.update` row), then retires it: the tenant has no default after (P-D-220). |

Submit-time subject validation failures are 400 with their code and no unit created (never 422, pricing D-403); apply-time environment refusal
rolls back through slice 03's APPLY_REFUSED path. P-D-184's draft-save behavior remains distinct.
No Product, clone, category-tree or CatalogVersion routes are added.

## 6. Data Model

The SQL lives in [DESIGN §3.7](../DESIGN.md#37-database-schemas--tables); Foundation creates it.
This slice assigns write ownership and snapshot contents.

| Record | Fields and write rules |
| --- | --- |
| `products_sku` | Identity: id, tenant_id, code, name, type, category_id. Business content: description, sellable, lifecycle, gl_code, tax_category, invoice_line_template, billing_timing, usage_type_ref, unit. Attribution: created_by, created_at, updated_at. Counters: revision (concurrency version), published_version. Approval/fence columns are owned jointly with slice 03. |
| `products_category` | id, tenant_id, code, name, is_default, sort_order, status, timestamps, version; at most one default per tenant (`uq_products_category_default`, P-D-218), and never a retired one (P-D-220). There is no parent_id. A nullable tenant-qualified category_id links a SKU to at most one category. |
| `products_sku_version` | tenant_id, sku_id, published_version, effective_from, snapshot. Snapshot preserves applied SKU business content, including type, lifecycle, descriptors and metering; pending ownership, fence metadata and concurrency tokens are not business content. |

`revision` is the SKU concurrency version for ETag, If-Match and conditional writes;
`published_version` advances only when a published snapshot is appended. The as-of index is
`(tenant_id, sku_id, effective_from, published_version)` with no unique effective-date constraint.
The SKU row is the latest applied content, even when its effective date is in the future. A snapshot
and its matching head increment commit together so readers cannot see a version number without history.
`billing_timing` is nullable (`advance | arrears`); absence inherits the tenant default.

## 7. Events & Alarms

Draft/category operations do not invent a replacement event family. Publication and applied changes
emit `SkuPublished` or `SkuChanged` through slice 03; slice 04 specifies the payload and consumer
behavior. Unique conflicts, unresolved usage refs and category-in-use are ordinary domain refusals.
Catalog outages at submit/apply are visible through the existing service diagnostics; the spec defines
no new alarm surface here.

## 8. Definitions of Done

- see [features/sku-categories.md](../features/sku-categories.md) — `cpt-cf-bss-products-dod-sku-create-unique`
- see [features/sku-categories.md](../features/sku-categories.md) — `cpt-cf-bss-products-dod-sku-type-frozen`
- see [features/sku-categories.md](../features/sku-categories.md) — `cpt-cf-bss-products-dod-usage-type-resolves`
- see [features/sku-categories.md](../features/sku-categories.md) — `cpt-cf-bss-products-dod-bundle-unpriced`
- see [features/sku-categories.md](../features/sku-categories.md) — `cpt-cf-bss-products-dod-versions-as-of`
- see [features/sku-categories.md](../features/sku-categories.md) — `cpt-cf-bss-products-dod-category-flat-crud`
- see [features/sku-categories.md](../features/sku-categories.md) — `cpt-cf-bss-products-dod-category-retire-refused`

## 9. Acceptance Criteria

Numbered criteria refer to [PRD §9](../PRD.md#9-acceptance-criteria).

| Trace | Given / When / Then |
| --- | --- |
| `cpt-cf-bss-products-fr-sku-define`; AC #1 | Given a tenant code/name already used, when a concurrent create reuses either, then 409 SKU_CODE_TAKEN or SKU_NAME_TAKEN leaves no second SKU; another tenant may use the same values. |
| `cpt-cf-bss-products-fr-sku-type-frozen`; AC #2 | Given any live price book entry, plan_item or sold_as reference, when type changes, then SKU_TYPE_FROZEN leaves both type and fence unchanged; drafts cannot have reservations and change type without fencing. |
| `cpt-cf-bss-products-fr-sku-metering`; AC #5 | Given missing or unresolved usage metering, when publication is submitted, then USAGE_NEEDS_METER or USAGE_TYPE_UNRESOLVED prevents a unit/version; apply revalidates if the catalog changes. A draft-save catalog non-answer remains saveable per P-D-184. |
| `cpt-cf-bss-products-fr-sku-bundle`; AC #6 | Given a bundle, when usage metering is assigned, then BUNDLE_HAS_NO_METER refuses it; the exposed type supports sold_as and prevents Pricing treating it as a priced SKU or item. |
| `cpt-cf-bss-products-fr-sku-versions`; AC #8–9 | Given publication on September 24 and a change effective October 1, when reading September 30/October 1/September 23, then return old/new/NO_VERSION_IN_FORCE. Earlier-date changes fail VERSION_ORDER; equal dates retain both versions and select the larger number. |
| `cpt-cf-bss-products-fr-category-flat`; AC #13, #27 | Given a flat category, when created/renamed it needs no approval; when referenced by a SKU that is not retired retirement fails CATEGORY_IN_USE; stale PATCH fails STALE_REVISION. An unreferenced category retires directly, also beside SKUs without a category or with only retired SKUs; a retired one fails CATEGORY_RETIRED (P-D-208). |
| `cpt-cf-bss-products-fr-sku-define`, `cpt-cf-bss-products-fr-sku-descriptors`; AC #4, #19 | Given pending ownership, when direct edit or second submission is attempted, then ROW_LOCKED_PENDING preserves the reviewed content. |

## 10. Non-Functional Considerations

All authoring, category and version queries enforce `cpt-cf-bss-products-nfr-authz` and
`cpt-cf-bss-products-nfr-tenant-isolation` (AC #22, #28). Test uniqueness races and immutable versions
on both backends (`cpt-cf-bss-products-nfr-two-backends`, AC #29). Category retirement and assignment
must not rely on an unguarded read-then-write check. Catalog resolution retains provenance without
turning optional draft validation into an availability dependency for saving incomplete work.
