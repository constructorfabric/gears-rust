<!-- CONFLUENCE_TITLE: [BSS]: Products — Read Model, References & Events (Design, Slice 4) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../DECISIONS.md | Owners: BSS Product Catalog team -->

# DESIGN — Read Model, References & Events (Slice 4)

- [ ] `p1` - **ID**: `cpt-cf-bss-products-design-slice-04`

<!-- toc -->

- [1. Context](#1-context)
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
- [5. API Surface](#5-api-surface)
- [6. Data Model](#6-data-model)
- [7. Events & Alarms](#7-events--alarms)
- [8. Definitions of Done](#8-definitions-of-done)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Non-Functional Considerations](#10-non-functional-considerations)

<!-- /toc -->

## 1. Context

This slice completes the Read model, References and Events components in
[DESIGN §3.2](../DESIGN.md#32-component-model). It depends on slices 01–03 for scoped storage,
SKU/version rules and subject transitions, and supplies the reciprocal registry checks needed by
slice 03's fences. Products answers reference queries locally; no remote Pricing count participates
in a fence. The implementation order does not waive this integration dependency.

The content authority is spec §2.2, §4, §6, §7.2–§7.3 and §13, meaning
`docs/superpowers/specs/2026-09-24-pricebook-model-design.md` in the main checkout.
[DECISIONS](../DECISIONS.md) P-D-191, P-D-193 and P-D-194 fix dated truth, atomic events and the
reservation protocol. Pricing's caller path arrives in phase 2; phases 0–1 remain unmerged until
that integration gate. The retained ProductCatalogClientV1 transport is an interim compatibility door.

## 2. Actor Flows (CDSL)

### Reader searches and opens a card

1. [ ] - `p1` - Authenticate products:read and scope the query before code/name search, type/category/lifecycle filters and limit/after pagination - `inst-read-search`
2. [ ] - `p1` - Return current heads and their version tokens; the card reads reference rows and live counts grouped by owner and kind from Products' registry - `inst-read-card`
3. [ ] - `p1` - Expose unresolved reserved rows for inspection and explicit release; use the dated versions door when the reader needs descriptors in force on a date - `inst-read-history`

### Pricing reserves, writes and confirms

1. [ ] - `p1` - Allocate the logical owner/kind/ref_id identity and reserve it in Products; if Products is unavailable, return REGISTRY_UNAVAILABLE and write no Pricing object - `inst-ref-reserve`
2. [ ] - `p1` - After reserve succeeds, re-read the SKU and apply Pricing's type/lifecycle guards, including SKU_RETIRING and SKU_DEPRECATED where applicable - `inst-ref-reread`
3. [ ] - `p1` - Commit the Pricing object, reservation_id and durable confirmation work in one Pricing transaction; the sold_as relation follows the same sequence - `inst-ref-owner-commit`
4. [ ] - `p1` - Confirm in Products and clear confirmation_pending after success; retry durably after timeout, accepting already-confirmed as success - `inst-ref-confirm`
5. [ ] - `p1` - After definite rollback or failed validation, durably cancel the attempt then release; after deletion, durably remove the object then release; never release merely because confirmation timed out - `inst-ref-cancel`

### Operator releases an abandoned reservation

1. [ ] - `p1` - Inspect the reservation through the scoped card/reference read and invoke DELETE with force true and a reason under explicit operator authorization - `inst-ref-force-input`
2. [ ] - `p1` - Record released state, actor/reason and timestamp together with audit and ReferenceForceReleased in one transaction - `inst-ref-force-release`
3. [ ] - `p1` - The owner consumes the event, verifies whether its object still exists and re-reserves if necessary; Products cannot itself detect release beneath a live owner object - `inst-ref-force-owner`

### Pricing receives a SKU change

1. [ ] - `p1` - Deliver the committed SkuChanged through the toolkit outbox after the apply transaction succeeds - `inst-event-deliver`
2. [ ] - `p1` - Pricing refreshes type, descriptors, metering and versions in its SKU read model; it creates no book approval or refreeze work - `inst-event-refresh`
3. [ ] - `p1` - Bind each period from the version in force at its start; existing bindings remain intact even if the latest head is future-effective - `inst-event-bind`

## 3. Processes / Business Logic (CDSL)

### outbox-same-tx

1. [ ] - `p1` - Build events from the transition's applied before/after content and recorded unit outcome using the caller's scoped transaction - `inst-event-build`
2. [ ] - `p1` - Append terminal audit, ApprovalUnitDecided and successful apply's SKU event through Foundation's outbox writer before committing state/unlock - `inst-event-atomic`
3. [ ] - `p1` - If any required write fails, roll back the transaction; neither APPLY_REFUSED nor UNIT_STALE can produce a successful domain apply event - `inst-event-rollback`

### sku-changed-payload

1. [ ] - `p1` - Compare before and applied after business fields to obtain changed; include descriptor, type, metering or lifecycle changes as applicable, excluding lock/fence/version metadata - `inst-event-changed-fields`
2. [ ] - `p1` - Serialize SkuChanged as tenantId, skuId, changed, effectiveFrom, publishedVersion and actorRef using the gear’s camelCase broker convention - `inst-event-changed-payload`
3. [ ] - `p1` - Write it alongside the appended SkuVersion and new head; consumers fetch dated snapshots instead of inferring effective content from event delivery time - `inst-event-changed-date`

### reserve-refused-when-fenced

1. [ ] - `p1` - Authorize the tenant and authenticated owner, check optional client-key replay, and look up a live logical key (tenant_id, owner_gear, ref_kind, ref_id) - `inst-ref-reserve-key`
2. [ ] - `p1` - A retry for the same live logical reference and SKU returns 200 with its existing reservation_id; a key bound to another SKU cannot silently be rebound or counted for the requested SKU - `inst-ref-reserve-existing`
3. [ ] - `p1` - For a new attempt, check the scoped SKU is neither retiring, type_change_pending nor retired in the same transaction as inserting reserved; fenced SKUs return 409 SKU_FENCED - `inst-ref-reserve-guard`
4. [ ] - `p1` - Insert a fresh id under the live-key unique index and commit with 201; re-read/retry after serialization or uniqueness contention so two concurrent retries cannot create two live reservations - `inst-ref-reserve-insert`

### reservation-counts-until-released

1. [ ] - `p1` - Define live as reserved or confirmed, without any age, deadline, caller-liveness or confirmation-timeout exclusion - `inst-ref-live`
2. [ ] - `p1` - Confirm reserved → confirmed conditionally; already confirmed returns 200, released returns 409 REFERENCE_RELEASED and never reactivates - `inst-ref-confirm-state`
3. [ ] - `p1` - Permit owner release only for its authenticated ownership after durable cancellation/deletion; require force and reason for an operator - `inst-ref-release-authorize`
4. [ ] - `p1` - Conditionally mark live → released and persist released_at/by/reason; concurrent confirm/release cannot undo release or overwrite its attribution - `inst-ref-release-state`
5. [ ] - `p1` - Keep released history; a later reserve for that logical key is a fresh attempt with a fresh id - `inst-ref-release-history`

### fence-refused-when-referenced

1. [ ] - `p1` - In the fence transaction, use the same tenant/SKU live predicate as reserve and grouped counts: state in reserved, confirmed - `inst-ref-fence-predicate`
2. [ ] - `p1` - Guard slice 03's conditional SKU update with NOT EXISTS live reference; refuse retirement with SKU_REFERENCED or type change with SKU_TYPE_FROZEN - `inst-ref-fence-write`
3. [ ] - `p1` - Run the reciprocal read/write transactions under Postgres serializable isolation or SQLite writer serialization, using Foundation's bounded retry; either fence or reserve can win, never both - `inst-ref-fence-race`
4. [ ] - `p1` - Revalidate at retirement apply; an invalid defensive reference environment yields APPLY_REFUSED with SKU_REFERENCED and preserves the prior committed fence - `inst-ref-fence-apply`

### browse-maps-published-only

1. [ ] - `p1` - Authenticate and scope the retained ProductCatalogClientV1 browse request before selecting source SKUs - `inst-read-browse-scope`
2. [ ] - `p1` - Serve Published and Deprecated SKUs with lifecycle status and deprecated flag; exclude draft, retiring and retired entries - `inst-read-browse-map`
3. [ ] - `p1` - Preserve the transport's existing response contract until phase 2 without recreating Product parents, CatalogVersion freezes or a second catalog authority - `inst-read-browse-contract`

Browse is the current published catalog surface; historical period binding always uses versions/as-of?date=.
Authoring list/card may show other lifecycle states in the authorized tenant and must not inherit the
Published/Deprecated predicate by accident.

## 4. States (CDSL)

1. [ ] - `p1` - No live logical reference → reserved with a fresh id, only against an available unfenced SKU.
2. [ ] - `p1` - Reserved → confirmed on owner confirmation; confirmed → confirmed returns success on retry.
3. [ ] - `p1` - Reserved/confirmed → released after owner cancellation/deletion or authorized operator force-release. Released is terminal for that id; confirm cannot resurrect it.
4. [ ] - `p1` - Reserved remains live indefinitely without confirm/release. Time passing changes neither its state nor fence eligibility.
5. [ ] - `p1` - A new attempt after release is a different row; the prior attempt's timestamps and attribution remain queryable.

## 5. API Surface

Paths are relative to `/bss-products/v1`, except the explicitly absolute browse path. POSTs accept
optional Idempotency-Key; logical-reference idempotency applies even without a key. All responses
use snake_case, scoped SDK types and Foundation's Problem mapping.

| Route | Contract |
| --- | --- |
| `GET /skus?$filter&$orderby&$top&cursor&q&priced&in_plan&priced_in&not_priced_in&not_in_revision` | products:read; the toolkit's OData pager (P-D-210): `$filter` over id, code, name, lifecycle, type, category_id and pending_unit_id (`eq null`/`ne null` on the last two); `lifecycle` compares the lifecycle in force with `eq`, `ne` or `in`, or with `contains`, `startswith` or `endswith` as the `in` of the tokens the text matches, case-sensitively, none matching keeping nothing, at the top level or joined by `and`, and a `lifecycle` term under `or`/`not` is 400 (P-D-249, P-D-264); `$orderby` code, name or updated_at with tie-break id (default code); `$top` (alias `limit`) 50, clamped at 200; `cursor` (alias `$skiptoken`) whose hash covers `$filter`, `q`, `priced` and `in_plan`; `q` is `lower(col) LIKE lower(?) ESCAPE '\'` over code, name, unit, usage_type_ref and gl_code (ASCII folding on SQLite; Unicode on Postgres, both sides through the ICU root collation `und-x-icu` whatever the database's locale); `priced`/`in_plan` keep or drop pricing's sets from one `usage_sets` call, bound as one value, 403 `USAGE_FORBIDDEN` / 503 `USAGE_UNAVAILABLE` when it cannot answer (P-D-212); the picker keys `priced_in`/`not_priced_in` (one book, at most one of the two) and `not_in_revision` (one revision) keep or drop the scope's set from one `sku_ids_in(UsageScope)` call per key, bound as one value, alike (P-D-246), each inside the cursor's hash; `$filter=id in (...)` is the multi-id read, at most `$top` 200 within the 8 KiB filter. Other keys, `$select` and `$count` are 400. Scope before filtering and cursor evaluation; orphan fences recovered in the read's transaction. Answers `Page<SkuListItem>`; each item carries `usage` from pricing's `SkuUsageV1` port, asked once per page with the page's ids, or `null` (P-D-197). |
| `GET /skus/counts?$filter&q&priced&in_plan&priced_in&not_priced_in&not_in_revision` | products:read; `{ all, draft, published, deprecated, retired, in_review }` narrowed like the list, `$filter`'s top-level `lifecycle` terms dropped, text functions included (under `or`/`not` 400; P-D-264); paging, order and `$select` 400; orphan fences recovered in the same transaction; one grouped statement (P-D-211). |
| `GET /skus/{id}` | products:read; current card with ETag and reference summary, including unconfirmed reservations, and `usage` from pricing's `SkuUsageV1` port or `null` (P-D-197). Shares slice 02's head read. |
| `GET /skus/{id}/history?$top&cursor` | products:read; the SKU's audit rows and its approval units' rows, in the order the acts wrote them (`audit_id`, a UUID v7 minted in the act's transaction): `Page<ProductsSkuHistoryEntry>` with `{ at, actor, action, from_lifecycle, to_lifecycle, unit_id, unit_kind, note }`; `$top`/`limit` 50, clamped at 200; `cursor` bound to the SKU; other keys 400; the SKU's orphan fence expires first; 404 for another tenant's SKU or a deleted draft (P-D-213). |
| `GET /skus/{id}/references` | products:read; live rows by default; include_released=true adds history with released_at, released_by, forced and release_reason. Live summary retains price_book_entries/plans/reserved totals and adds by_owner maps keyed by owner then kind, plus each owner’s reserved subset. |
| `POST /skus/{id}/references/reserve { owner, kind, ref_id }` | products:author plus authenticated owner check; 201 `{ reservation_id }` or 200 for the same live attempt; 409 SKU_FENCED for a new reservation through a fence. |
| `POST /references/{id}/confirm` | products:author plus owner check; 200 also when already confirmed; 409 REFERENCE_RELEASED for a released id. |
| `DELETE /references/{id}` | products:author plus owner check, or explicit operator authorization with `force: true` and reason. Records release, never deletes history. |
| `GET /bss-products/v1/browse` | Absolute retained ProductCatalogClientV1 transport; products:read; Published and Deprecated catalog mapping until phase 2. |

Cross-tenant ids and cursors must not disclose another tenant's SKU/reference. Caller-provided owner
must agree with authenticated ownership; a client cannot force-release by merely naming another gear.
A reserve outage produces Pricing's 503 REGISTRY_UNAVAILABLE and prevents its object write. Products'
registry has no fallback remote-count path and no pretend-zero response when storage is unavailable.
The `usage` of a SKU read is `{ entries, currencies, prices { approved, pending, draft }, plans }` as pricing
answers it (pricing D-428). It is `null` when no port is registered, when the port refuses the caller (no pricing
`price_book_entry:read`) and when it cannot answer: the SKU read never fails for it, and it calls the port on a task
of its own outside any transaction. The usage is information and never takes part in a fence (P-D-197). The
list's `priced` and `in_plan` filters are the one place the port's answer decides a read: they ask its
`usage_sets` once per request (`priced` ⇔ `entries > 0`, `in_plan` ⇔ `plans > 0`), and a refusal is 403
`USAGE_FORBIDDEN`, an absent, failing or late port 503 `USAGE_UNAVAILABLE`, never an unfiltered page (P-D-212).
The picker keys ask `sku_ids_in` once per key under the same rules (P-D-246): a book's SKUs under pricing
`price_book_entry:read`, a revision's under `plan:read` too; a book or a revision the tenant does not hold is the
empty set.

## 6. Data Model

[DESIGN §3.7](../DESIGN.md#37-database-schemas--tables) defines `products_sku_reference` and read
indexes. This slice adds that registry after Foundation's five migrations, with the reciprocal guard
integration required before the Products phase gate.

| Shape | Constraint/use |
| --- | --- |
| Reference row | id, tenant_id, sku_id, owner_gear, ref_kind, ref_id, state, reserved_at, confirmed_at, released_at, released_by, release_reason. Kinds are price_book_entry/plan_item/sold_as; SKU link is tenant-qualified. |
| Live identity index | UNIQUE `(tenant_id, owner_gear, ref_kind, ref_id) WHERE state <> 'released'`; sku_id is checked for retry identity but not added to this key. |
| Live SKU index | `(tenant_id, sku_id, owner_gear, ref_kind) WHERE state <> 'released'`; serves fence predicates and grouped live counts. |
| Browse index | `(tenant_id, lifecycle, type, category_id, id)` over current SKU heads; combine tenant-scoped query predicates for code/name search and optional filters. |
| Dated read index | `(tenant_id, sku_id, effective_from, published_version)` supplied by Foundation/slice 02; separate from the latest-head browse read. |

The read model uses scoped catalog/version/reference data; it does not add a second writable catalog.
Reference state transitions use conditional predicates so confirmation cannot race release into
reactivation. Confirmed_at and release provenance survive retries. No expires_at or automatic cleanup
removes a live reservation from a fence predicate. Audit and outbox use Foundation's stores.

## 7. Events & Alarms

Broker payload fields are camelCase; the toolkit envelope supplies established tenant and correlation
context. Use existing outbox delivery behavior rather than introducing a second dispatcher.

| Event | Trigger and payload contract |
| --- | --- |
| `SkuPublished` | Successful sku_publish, with SKU identity and the newly published version available through the SDK/version read. |
| `SkuChanged` | Successful sku_change; `SkuChanged { tenantId, skuId, changed, effectiveFrom, publishedVersion, actorRef }`. changed names applied business fields; effectiveFrom is the approved date, not delivery time. |
| `SkuRetired` | Successful retirement apply, identifying the retired SKU. No event on a refused fence/apply. |
| `ApprovalUnitDecided` | Every terminal approval/reject/withdraw/quorum-zero outcome; `{ tenantId, unitId, kind, state, generation, actors: [...] }` from the recorded terminal action. |
| `ReferenceForceReleased` | Operator release; identifies reservation, SKU, owner/kind/ref_id and actor/reason so the owner can reconcile; committed with the attributed release and audit. |

Submission writes audit without an event unless quorum zero applies. A stale refresh does not announce
a successful apply. Pricing consumes SkuChanged to refresh its SKU read model, never to draft book
changes. Dated version lookup protects bindings from delayed event delivery and future-effective heads.
The SKU card exposes abandoned reservations; explicit release, rather than expiry or an alarm handler,
changes their protection. No additional alarm contract is required by this slice.

SkuChanged has type id `gts.cf.core.events.event.v1~cf.bss.products.sku_changed.v1~`;
its payload is `tenantId`, `skuId`, `changed`, `effectiveFrom`, `publishedVersion`, `actorRef`.

## 8. Definitions of Done

- see [features/read-model-events.md](../features/read-model-events.md) — `cpt-cf-bss-products-dod-list-search`
- see [features/read-model-events.md](../features/read-model-events.md) — `cpt-cf-bss-products-dod-card-with-references`
- see [features/read-model-events.md](../features/read-model-events.md) — `cpt-cf-bss-products-dod-reference-registry`
- see [features/read-model-events.md](../features/read-model-events.md) — `cpt-cf-bss-products-dod-reserve-refused-when-fenced`
- see [features/read-model-events.md](../features/read-model-events.md) — `cpt-cf-bss-products-dod-browse-published-only`
- see [features/read-model-events.md](../features/read-model-events.md) — `cpt-cf-bss-products-dod-events-in-outbox-tx`
- see [features/read-model-events.md](../features/read-model-events.md) — `cpt-cf-bss-products-dod-sku-changed-payload`

## 9. Acceptance Criteria

Numbered criteria refer to [PRD §9](../PRD.md#9-acceptance-criteria).

| Trace | Given / When / Then |
| --- | --- |
| `cpt-cf-bss-products-fr-read-model`; AC #22 | Given overlapping tenant names and mixed lifecycle/type/category data, when searching/paging and opening cards, then only scoped matching rows appear and live reference counts include both reserved and confirmed. Cross-tenant card/reference/version access exposes nothing. |
| Same FR; AC #22 plus retained-interface boundary in PRD §7.1 | Given SKUs in every lifecycle, when using browse, then Published and Deprecated entries map to ProductCatalogClientV1 with status and deprecated flag; authoring list/card can still show authorized non-published heads. |
| `cpt-cf-bss-products-fr-reference-registry`; AC #23 | Given reserve racing retire/type fencing on either backend, when transactions complete, then a winning reserve yields SKU_REFERENCED/SKU_TYPE_FROZEN on the fence, or a winning fence yields SKU_FENCED on reserve; both never succeed. |
| Same FR; AC #24 | Given a live key, when reserved twice then released and attempted anew, then the live retry is 200 with the same id, the new attempt has a fresh id and old history remains. Confirming the old id returns REFERENCE_RELEASED. |
| Same FR; AC #25 | Given Products outage before reserve, when Pricing writes, then REGISTRY_UNAVAILABLE leaves no object. Given timeout after Pricing commit, then confirmation_pending and durable retries remain, reservation stays live and repeated confirmed confirmation is 200. |
| Same FR; `cpt-cf-bss-products-nfr-audit`; AC #26 | Given an abandoned reservation, when force-release lacks force/reason/authorization, then it remains live; valid operator release commits actor/reason, audit and ReferenceForceReleased together. |
| `cpt-cf-bss-products-fr-events`; AC #20–21 | Given approve/reject/withdraw/quorum-zero paths, when committed, then each records terminal audit/ApprovalUnitDecided, with domain events only on apply. Injected outbox failure or APPLY_REFUSED leaves no success event. |
| Same FR; `cpt-cf-bss-products-fr-sku-descriptors`; AC #3 | Given an October 1 GL change, when approved, then SkuChanged contains skuId, changed including gl_code and effectiveFrom October 1; Pricing refreshes without a book unit and earlier bindings keep the prior GL. |

## 10. Non-Functional Considerations

`cpt-cf-bss-products-nfr-authz` and `cpt-cf-bss-products-nfr-tenant-isolation` apply equally to browse,
search, grouped counts and reference mutations (AC #22, #28). Bound page sizes and stable ordering
avoid unbounded list responses; the spec sets no new latency target. Verify reserve/fence and
confirm/release races on both backends (`cpt-cf-bss-products-nfr-two-backends`, AC #29).
`cpt-cf-bss-products-nfr-audit` requires attributed, atomic operator releases and terminal events.
Fail-safe reservations intentionally trade availability of retire/type changes for reference safety;
Pricing's durable confirmation/cancellation implementation is required at the phase 2 integration gate.
