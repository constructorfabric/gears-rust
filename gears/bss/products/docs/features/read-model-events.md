<!-- CONFLUENCE_TITLE: [BSS]: Products — Read Model & Events (Feature) -->
<!-- Related: ../DECOMPOSITION.md, ../DESIGN.md, ../PRD.md | Owners: BSS Product Catalog team -->

# Feature: Read Model & Events

- [ ] `p1` - **ID**: `cpt-cf-bss-products-featstatus-read-model-events-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-bss-products-feature-read-model-events`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Reader searches and opens a card](#reader-searches-and-opens-a-card)
  - [Pricing reserves, writes and confirms](#pricing-reserves-writes-and-confirms)
  - [Operator releases an abandoned reservation](#operator-releases-an-abandoned-reservation)
  - [Pricing receives a SKU change](#pricing-receives-a-sku-change)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [outbox-same-tx](#outbox-same-tx)
  - [sku-changed-payload](#sku-changed-payload)
  - [reserve-refused-when-fenced](#reserve-refused-when-fenced)
  - [reservation-counts-until-released](#reservation-counts-until-released)
  - [fence-refused-when-referenced](#fence-refused-when-referenced)
  - [browse-maps-published-only](#browse-maps-published-only)
- [4. States (CDSL)](#4-states-cdsl)
  - [Read Model & Events states](#read-model--events-states)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Scoped list and search](#scoped-list-and-search)
  - [SKU card shows local reference facts](#sku-card-shows-local-reference-facts)
  - [Durable reference identity and release](#durable-reference-identity-and-release)
  - [Reserve and fence exclude each other](#reserve-and-fence-exclude-each-other)
  - [Retained browse maps published SKUs](#retained-browse-maps-published-skus)
  - [Domain and decision events share the act transaction](#domain-and-decision-events-share-the-act-transaction)
  - [SKU change payload identifies dated business changes](#sku-change-payload-identifies-dated-business-changes)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

This phase 1c feature implements [design slice 04](../design/04-read-model-events.md).
The implementation order and integration
prerequisites are in [DECOMPOSITION](../DECOMPOSITION.md); [DESIGN §3](../DESIGN.md#3-technical-architecture)
is the architecture and schema authority. Unchecked items describe implementation obligations.

### 1.2 Purpose

Complete scoped search, cards and retained browse; own the local reservation registry and reciprocal fence guards; assemble the committed domain and approval event payloads. Pricing owns its phase 2 caller-side protocol and dated descriptor bindings.

Requirements: `cpt-cf-bss-products-fr-read-model`, `cpt-cf-bss-products-fr-reference-registry`, `cpt-cf-bss-products-fr-events`, `cpt-cf-bss-products-fr-sku-descriptors`, `cpt-cf-bss-products-nfr-authz`, `cpt-cf-bss-products-nfr-tenant-isolation`, `cpt-cf-bss-products-nfr-audit`, `cpt-cf-bss-products-nfr-two-backends`, `cpt-cf-bss-products-fr-sku-retire-fenced`, `cpt-cf-bss-products-fr-sku-type-frozen`, `cpt-cf-bss-products-fr-sku-versions`.

Design principles: `cpt-cf-bss-products-principle-fence-before-count`.

### 1.3 Actors

`cpt-cf-bss-products-actor-catalog-admin`, `cpt-cf-bss-products-actor-auditor`, `cpt-cf-bss-products-actor-pricing`. Authenticated doors enforce the applicable products:read, author, submit,
approve or settings permission and tenant scope; holding multiple grants never bypasses SoD.

### 1.4 References

- [PRD](../PRD.md), especially §9's numbered acceptance criteria cited below.
- [DESIGN](../DESIGN.md), §3's model, API, transactions and schema.
- [Slice 04](../design/04-read-model-events.md), including API/data details and the matching instruction names.
- [DECISIONS](../DECISIONS.md), P-D-184–194, including the superseding reservation, version and generation decisions.
- Source “spec”: `docs/superpowers/specs/2026-09-24-pricebook-model-design.md` in the main checkout, §2.2, §4, §6, §7.2–§7.3 and §13.

## 2. Actor Flows (CDSL)

### Reader searches and opens a card

- [ ] `p1` - **ID**: `cpt-cf-bss-products-flow-read-model-events-reader-searches-and-opens-a-card`

1. [ ] - `p1` - Authenticate products:read and scope the query before code/name search, type/category/lifecycle filters and limit/after pagination - `inst-read-search`
2. [ ] - `p1` - Return current heads and their version tokens; the card reads reference rows and live counts grouped by owner and kind from Products' registry - `inst-read-card`
3. [ ] - `p1` - Expose unresolved reserved rows for inspection and explicit release; use the dated versions door when the reader needs descriptors in force on a date - `inst-read-history`

### Pricing reserves, writes and confirms

- [ ] `p1` - **ID**: `cpt-cf-bss-products-flow-read-model-events-pricing-reserves-writes-and-confirms`

1. [ ] - `p1` - Allocate the logical owner/kind/ref_id identity and reserve it in Products; if Products is unavailable, return REGISTRY_UNAVAILABLE and write no Pricing object - `inst-ref-reserve`
2. [ ] - `p1` - After reserve succeeds, re-read the SKU and apply Pricing's type/lifecycle guards, including SKU_RETIRING and SKU_DEPRECATED where applicable - `inst-ref-reread`
3. [ ] - `p1` - Commit the Pricing object, reservation_id and durable confirmation work in one Pricing transaction; the sold_as relation follows the same sequence - `inst-ref-owner-commit`
4. [ ] - `p1` - Confirm in Products and clear confirmation_pending after success; retry durably after timeout, accepting already-confirmed as success - `inst-ref-confirm`
5. [ ] - `p1` - After definite rollback or failed validation, durably cancel the attempt then release; after deletion, durably remove the object then release; never release merely because confirmation timed out - `inst-ref-cancel`

### Operator releases an abandoned reservation

- [ ] `p1` - **ID**: `cpt-cf-bss-products-flow-read-model-events-operator-releases-an-abandoned-reservation`

1. [ ] - `p1` - Inspect the reservation through the scoped card/reference read and invoke DELETE with force true and a reason under explicit operator authorization - `inst-ref-force-input`
2. [ ] - `p1` - Record released state, actor/reason and timestamp together with audit and ReferenceForceReleased in one transaction - `inst-ref-force-release`
3. [ ] - `p1` - The owner consumes the event, verifies whether its object still exists and re-reserves if necessary; Products cannot itself detect release beneath a live owner object - `inst-ref-force-owner`

### Pricing receives a SKU change

- [ ] `p1` - **ID**: `cpt-cf-bss-products-flow-read-model-events-pricing-receives-a-sku-change`

1. [ ] - `p1` - Deliver the committed SkuChanged through the toolkit outbox after the apply transaction succeeds - `inst-event-deliver`
2. [ ] - `p1` - Pricing refreshes type, descriptors, metering and versions in its SKU read model; it creates no book approval or refreeze work - `inst-event-refresh`
3. [ ] - `p1` - Bind each period from the version in force at its start; existing bindings remain intact even if the latest head is future-effective - `inst-event-bind`

## 3. Processes / Business Logic (CDSL)

### outbox-same-tx

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-read-model-events-outbox-same-tx`

1. [ ] - `p1` - Build events from the transition's applied before/after content and recorded unit outcome using the caller's scoped transaction - `inst-event-build`
2. [ ] - `p1` - Append terminal audit, ApprovalUnitDecided and successful apply's SKU event through Foundation's outbox writer before committing state/unlock - `inst-event-atomic`
3. [ ] - `p1` - If any required write fails, roll back the transaction; neither APPLY_REFUSED nor UNIT_STALE can produce a successful domain apply event - `inst-event-rollback`

### sku-changed-payload

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-read-model-events-sku-changed-payload`

1. [ ] - `p1` - Compare before and applied after business fields to obtain changed; include descriptor, type, metering or lifecycle changes as applicable, excluding lock/fence/version metadata - `inst-event-changed-fields`
2. [ ] - `p1` - Serialize SkuChanged as tenantId, skuId, changed, effectiveFrom, publishedVersion and actorRef using the gear’s camelCase broker convention - `inst-event-changed-payload`
3. [ ] - `p1` - Write it alongside the appended SkuVersion and new head; consumers fetch dated snapshots instead of inferring effective content from event delivery time - `inst-event-changed-date`

### reserve-refused-when-fenced

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-read-model-events-reserve-refused-when-fenced`

1. [ ] - `p1` - Authorize the tenant and authenticated owner, check optional client-key replay, and look up a live logical key (tenant_id, owner_gear, ref_kind, ref_id) - `inst-ref-reserve-key`
2. [ ] - `p1` - A retry for the same live logical reference and SKU returns 200 with its existing reservation_id; a key bound to another SKU cannot silently be rebound or counted for the requested SKU - `inst-ref-reserve-existing`
3. [ ] - `p1` - For a new attempt, check the scoped SKU is neither retiring, type_change_pending nor retired in the same transaction as inserting reserved; fenced SKUs return 409 SKU_FENCED - `inst-ref-reserve-guard`
4. [ ] - `p1` - Insert a fresh id under the live-key unique index and commit with 201; re-read/retry after serialization or uniqueness contention so two concurrent retries cannot create two live reservations - `inst-ref-reserve-insert`

### reservation-counts-until-released

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-read-model-events-reservation-counts-until-released`

1. [ ] - `p1` - Define live as reserved or confirmed, without any age, deadline, caller-liveness or confirmation-timeout exclusion - `inst-ref-live`
2. [ ] - `p1` - Confirm reserved → confirmed conditionally; already confirmed returns 200, released returns 409 REFERENCE_RELEASED and never reactivates - `inst-ref-confirm-state`
3. [ ] - `p1` - Permit owner release only for its authenticated ownership after durable cancellation/deletion; require force and reason for an operator - `inst-ref-release-authorize`
4. [ ] - `p1` - Conditionally mark live → released and persist released_at/by/reason; concurrent confirm/release cannot undo release or overwrite its attribution - `inst-ref-release-state`
5. [ ] - `p1` - Keep released history; a later reserve for that logical key is a fresh attempt with a fresh id - `inst-ref-release-history`

### fence-refused-when-referenced

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-read-model-events-fence-refused-when-referenced`

1. [ ] - `p1` - In the fence transaction, use the same tenant/SKU live predicate as reserve and grouped counts: state in reserved, confirmed - `inst-ref-fence-predicate`
2. [ ] - `p1` - Guard slice 03's conditional SKU update with NOT EXISTS live reference; refuse retirement with SKU_REFERENCED or type change with SKU_TYPE_FROZEN - `inst-ref-fence-write`
3. [ ] - `p1` - Run the reciprocal read/write transactions under Postgres serializable isolation or SQLite writer serialization, using Foundation's bounded retry; either fence or reserve can win, never both - `inst-ref-fence-race`
4. [ ] - `p1` - Revalidate at retirement apply; an invalid defensive reference environment yields APPLY_REFUSED with SKU_REFERENCED and preserves the prior committed fence - `inst-ref-fence-apply`

### browse-maps-published-only

- [ ] `p1` - **ID**: `cpt-cf-bss-products-algo-read-model-events-browse-maps-published-only`

1. [ ] - `p1` - Authenticate and scope the retained ProductCatalogClientV1 browse request before selecting source SKUs - `inst-read-browse-scope`
2. [ ] - `p1` - Serve Published and Deprecated SKUs with lifecycle status and deprecated flag; exclude draft and retired entries. A retire under review stays published or deprecated (P-D-248) - `inst-read-browse-map`
3. [ ] - `p1` - Preserve the transport's existing response contract until phase 2 without recreating Product parents, CatalogVersion freezes or a second catalog authority - `inst-read-browse-contract`

Browse is the current published catalog surface; historical period binding always uses versions/as-of?date=.
Authoring list/card may show other lifecycle states in the authorized tenant and must not inherit the
Published/Deprecated predicate by accident.

## 4. States (CDSL)

### Read Model & Events states

- [ ] `p1` - **ID**: `cpt-cf-bss-products-state-read-model-events`

1. [ ] - `p1` - No live logical reference → reserved with a fresh id, only against an available unfenced SKU.
2. [ ] - `p1` - Reserved → confirmed on owner confirmation; confirmed → confirmed returns success on retry.
3. [ ] - `p1` - Reserved/confirmed → released after owner cancellation/deletion or authorized operator force-release. Released is terminal for that id; confirm cannot resurrect it.
4. [ ] - `p1` - Reserved remains live indefinitely without confirm/release. Time passing changes neither its state nor fence eligibility.
5. [ ] - `p1` - A new attempt after release is a different row; the prior attempt's timestamps and attribution remain queryable.

## 5. Definitions of Done

These definitions own this feature's 7 DoDs; the design slice references them without redefining them.
Design constraints: `cpt-cf-bss-products-constraint-two-backends`.

### Scoped list and search

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-list-search`

Verified at `b74e8783b49b03fa827f1052a99cf6553683f4aa`; implementation marker in `products/src/api/rest/skus.rs`.

GET skus pages on the toolkit's OData under tenant scope: `$filter` over id, code, name, type, lifecycle, category_id (`eq null`: no category) and pending_unit_id (`ne null`: in review), where `lifecycle` compares the lifecycle in force with `eq`, `ne` or `in`, or with `contains`, `startswith` or `endswith` as the `in` of the lifecycles whose token the text matches, case-sensitively (none matching is an empty page), at the top level or joined by `and`, and a `lifecycle` term under `or` or `not` is 400 (P-D-249, P-D-264); `$orderby` code, name or updated_at with the tie-break id, `$top`/`limit` 50 clamped at 200 and an opaque cursor whose hash covers `$filter`, `q`, `priced` and `in_plan`; `q` is a literal case-insensitive substring of code, name, unit, usage type and GL code; `priced` and `in_plan` filter on pricing's usage sets and fail the read when pricing cannot answer (P-D-212). The pickers' `priced_in` or `not_priced_in` (one book) and `not_in_revision` (one plan revision) keep or drop pricing's set of that scope, one port call per key, inside the cursor's hash, and fail the read the same way (P-D-246); `$filter=id in (...)` is the multi-id read, at most `$top` 200 within the 8 KiB filter. GET skus/counts answers the tab counts narrowed alike (P-D-210, P-D-211). GET skus and GET skus/counts answer 304 when `If-None-Match` matches a weak `ETag` of the JSON body, and send `Cache-Control: private, no-cache` (P-D-261). GET usage-types answers `Cache-Control: private, max-age=60` (P-D-247). It reads current heads using the DESIGN §3.7 read indexes and exposes authorized lifecycle states; dated truth comes from versions rather than a future-effective head (spec §4, §7.2; DESIGN §3.3; slice 04 §5). The SKU list, the card and the history name their actors (`created_by_name`, `actor_name`): the current name through Account Management, resolved once per answer, null when it is not available now, "System" for the system's own acts (P-D-262). A retired SKU may be archived and unarchived by its author (`POST skus/{id}/archive`, `POST skus/{id}/unarchive`, If-Match, audited, the 200's declared `ETag` the next If-Match): GET skus, its pickers and its counts leave an archived SKU out unless `$filter` asks `archived eq true`, the counts carry it in `archived` alone, and the card, the browse and the consumer reads ignore the mark (P-D-263).

### SKU card shows local reference facts

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-card-with-references`

Verified at `b74e8783b49b03fa827f1052a99cf6553683f4aa`; implementation marker in `products/src/api/rest/skus.rs`.

The SKU card and GET references return local registry rows and live counts grouped by owner and kind, including abandoned reserved rows for inspection. Reserved and confirmed both count until release; GET references?include_released=true includes released_at, released_by, forced and release_reason without counting released rows; flat price_book_entries/plans/reserved totals remain alongside by_owner maps, and no remote Pricing count substitutes for the local read; the card's `usage` from pricing's `SkuUsageV1` port is information beside them, never a reference count (P-D-197) (spec §2 decision 17, §4, §13; DESIGN §3.2–§3.3). GET skus/{id}/history reads the SKU's audit rows and its units' rows, oldest first, each with its actor, its lifecycle move, its unit and its note, under the card's scope (P-D-213).

### Durable reference identity and release

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-reference-registry`

Verified at `b74e8783b49b03fa827f1052a99cf6553683f4aa`; implementation marker in `products/src/api/rest/references.rs`.

Products stores tenant-scoped price_book_entry/plan_item/sold_as attempts with a unique live logical key and retained released history. Reserve retries return the same live ID; a later attempt after release gets a fresh ID. Confirm is idempotent on confirmed and returns REFERENCE_RELEASED on released. Owner release checks authenticated ownership; operator release requires force and reason and atomically records actor/reason, audit and ReferenceForceReleased. Products never expires a reservation and cannot detect release beneath a live owner object (spec decision 17, §13; DESIGN §3.7).

### Reserve and fence exclude each other

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-reserve-refused-when-fenced`

Verified at `4c5577f1cb08d072e79880599a3ae1db8ed8d1e0`; implementation marker in `products/src/api/rest/references.rs`.

New reservations check retiring, type_change_pending and retired in the same transaction as insertion, returning SKU_FENCED for a fenced SKU. Fence acquisition checks no reserved/confirmed row in its own conditional write; Postgres serializable isolation and SQLite writer serialization enforce reciprocal exclusion with bounded retry. Neither an unconfirmed age nor a remote count weakens the predicate (spec §2 decision 17, §2.2, §4, §13; DESIGN §3.7).

### Retained browse maps published SKUs

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-browse-published-only`

Verified at `b74e8783b49b03fa827f1052a99cf6553683f4aa`; implementation marker in `products/src/infra/catalog_provider.rs`.

GET /bss-products/v1/browse preserves ProductCatalogClientV1 transport and serves Published and Deprecated SKUs with their lifecycle status and deprecated flag; drafts, retiring and retired are absent. Tenant scope applies and pricing can read deprecated SKUs it already references. No Product parent or CatalogVersion authority is recreated (DESIGN §3.3; slice 04 §3).

### Domain and decision events share the act transaction

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-events-in-outbox-tx`

Verified at `4c5577f1cb08d072e79880599a3ae1db8ed8d1e0`; implementation marker in `products/src/infra/events.rs`.

SkuPublished, SkuChanged, SkuRetired and ApprovalUnitDecided use Foundation's outbox with state and audit, including every terminal decision and quorum zero. Operator force-release additionally records ReferenceForceReleased in its release transaction; submission alone has audit without an event. Failed apply/rollback cannot emit a successful domain or terminal event and stale refresh cannot announce apply (spec §4, §6, §7.3; DESIGN §3.4).

### SKU change payload identifies dated business changes

- [x] `p1` - **ID**: `cpt-cf-bss-products-dod-sku-changed-payload`

Verified at `b74e8783b49b03fa827f1052a99cf6553683f4aa`; implementation marker in `products/src/infra/broker.rs`.

SkuChanged uses this gear’s camelCase broker convention: tenantId, skuId, changed, effectiveFrom, publishedVersion and actorRef; its type id is gts.cf.core.events.event.v1~cf.bss.products.sku_changed.v1~. The applied change supplies its date and business-field names in changed, excluding lock/fence/version metadata. The new head, immutable version and event commit together; consumers read the dated snapshot separately (spec §2.2, §6, §7.3; DESIGN §3.4).

**Owed by pricing (phase 2).** Pricing must reserve, re-read the SKU, and commit its object, reservation id and confirmation work together; retry confirmation durably, never release on confirmation timeout, and release only after durable cancellation or deletion. Pricing must refresh its SKU read model from SkuChanged and bind descriptors from the version in force at period start.

## 6. Acceptance Criteria

Each criterion below corresponds to exactly one DoD above and cites [PRD §9](../PRD.md#9-acceptance-criteria).
Verify these during phase 1c on SQLite and Postgres, including tenant isolation and denied permissions;
this document does not claim those implementation tests have run. Pricing-side assertions are contract
obligations here and integration checks when its phase 2 caller path exists.

| DoD | PRD trace | Given / When / Then |
| --- | --- | --- |
| `cpt-cf-bss-products-dod-list-search` | AC #22, #28; `cpt-cf-bss-products-fr-read-model`, `cpt-cf-bss-products-nfr-tenant-isolation` | Given two tenants with matching names and varied types/categories/lifecycles, when `$filter`, `q` and cursor pages are read, then only scoped matching heads appear in stable order and the counts agree with the list; another tenant's cursor or absent read permission discloses no protected data. |
| `cpt-cf-bss-products-dod-card-with-references` | AC #10, #22, #26; `cpt-cf-bss-products-fr-read-model`, `cpt-cf-bss-products-fr-reference-registry` | Given a SKU with reserved, confirmed and released attempts, when its scoped card and references are read, then both live states count by owner/kind and abandoned reservations remain visible; elapsed time cannot hide a reservation, released history is excluded from live counts and cross-tenant reads reveal nothing. |
| `cpt-cf-bss-products-dod-reference-registry` | AC #24, #25, #26; `cpt-cf-bss-products-fr-reference-registry`, `cpt-cf-bss-products-nfr-audit` | Given a live logical reference, when reserve and confirm repeat, then they return the same ID and success; after legitimate release a new attempt gets a fresh ID, old confirm fails with REFERENCE_RELEASED and history remains; force-release without authorization/force/reason is refused, and post-commit confirmation timeout leaves the reservation live for durable retry. |
| `cpt-cf-bss-products-dod-reserve-refused-when-fenced` | AC #2, #10, #23, #29; `cpt-cf-bss-products-fr-reference-registry`, `cpt-cf-bss-products-fr-sku-retire-fenced`, `cpt-cf-bss-products-fr-sku-type-frozen` | Given concurrent reserve and retire/type-fence attempts on either backend, when they run, then a winning reservation causes SKU_REFERENCED/SKU_TYPE_FROZEN or a winning fence causes SKU_FENCED, never both successes; retired SKUs admit no new references and unconfirmed reservations keep blocking fences indefinitely. |
| `cpt-cf-bss-products-dod-browse-published-only` | AC #22, #28; PRD §7.1 retained-interface boundary; `cpt-cf-bss-products-fr-read-model` | Given tenant SKUs in all five lifecycles, when the retained browse endpoint is called, then Published and Deprecated SKUs map to its existing response contract with lifecycle status and deprecated flag; other lifecycles and other tenants never leak into browse, while the authorized authoring list still exposes its requested lifecycle states. |
| `cpt-cf-bss-products-dod-events-in-outbox-tx` | AC #20, #21, #26; `cpt-cf-bss-products-fr-events`, `cpt-cf-bss-products-nfr-audit` | Given successful publish/change/retire, reject, withdraw, quorum-zero and force-release operations, when each commits, then its required audit/events commit with state; an injected outbox failure or APPLY_REFUSED produces no success event, and submission without apply emits no domain event. |
| `cpt-cf-bss-products-dod-sku-changed-payload` | AC #3, #9, #21; `cpt-cf-bss-products-fr-events`, `cpt-cf-bss-products-fr-sku-descriptors`, `cpt-cf-bss-products-fr-sku-versions` | Given an approved October 1 GL change, when SkuChanged is serialized, then it carries skuId, changed including gl_code and effectiveFrom October 1; lock/version fields are absent from changed, delayed delivery does not rewrite earlier bindings, and failed apply emits no payload. |
