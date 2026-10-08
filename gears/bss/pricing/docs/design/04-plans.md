<!-- CONFLUENCE_TITLE: [BSS]: Pricing — Plans (Design, Slice 4) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../DECISIONS.md | Owners: BSS Pricing team -->

# DESIGN — Plans (Slice 4)

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-design-slice-04`

<!-- toc -->

- [1. Context](#1-context)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Prepare and publish a revision](#prepare-and-publish-a-revision)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [revision-checks](#revision-checks)
  - [revision-apply](#revision-apply)
  - [revision-switch](#revision-switch)
  - [clone-and-retire](#clone-and-retire)
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

Publish independent revision structure against book coverage, preserving existing pins; author items, clone and retirement prerequisites.

Requirements: `cpt-cf-bss-pricing-fr-plans`, `cpt-cf-bss-pricing-fr-reference-protocol`. Architecture: `cpt-cf-bss-pricing-component-plans`, `cpt-cf-bss-pricing-component-approvals`, `cpt-cf-bss-pricing-component-reservations-client`, `cpt-cf-bss-pricing-principle-book-money-independent`, `cpt-cf-bss-pricing-principle-reserve-before-write`, `cpt-cf-bss-pricing-constraint-no-row-locks`, `cpt-cf-bss-pricing-seq-blocked-revision`, `cpt-cf-bss-pricing-seq-reserve-write-confirm`.
[FEATURE](../features/plans.md) owns the executable flow/algorithm/DoD identifiers; this slice defines no duplicate DoDs.
Dependencies: `cpt-cf-bss-pricing-feature-prices-windows-dimension`, `cpt-cf-bss-pricing-feature-approvals`.
Source: PriceBook spec §2.2, §5–§8, §12–§13 and [DECISIONS](../DECISIONS.md) D-384–D-433.

D-502: a plan item remains a SKU and its selected entry (D-467), with no policy override,
treatment, included quantity or minimum quantity. Copy/clone within a book preserves entry IDs.
Changing a draft's book matches the full (SKU, charge kind, normalized period, model, policy digest)
key and an equal dimension key. With no equivalent target, the item retains the old entry and
ITEM_BOOK_FOREIGN blocks publication. An hourly entry never silently becomes monthly, and an absent
legacy policy never becomes a new policy. Explicit item selection chooses the replacement entry.

D-503 validates a usage entry's policy at price and plan-revision submit and final apply.
Products and meter reads happen outside Pricing transactions, as the acting caller. The subjects
consume captured results, recheck the entry identity/version in their existing transaction and keep
provider evidence digests in approval snapshots. Dependency failures remain typed observations until
the engine reaches a semantic gate, preserving non-final votes, rejects and withdrawals. Authorized
successful command replay precedes dependency observations.

Detached publication observations are checked against the complete local selection (including
added, removed or re-pointed items/prices) and entry identities before any captured refusal is
consumed. Local drift rolls back and repeats the authorized replay lookup, detached capture and
transaction within `toolkit_db::DEFAULT_TX_RETRY_ATTEMPTS`; driver contention shares that same
budget. Exhaustion is `UNIT_CONTENDED`. The second provider evidence read runs immediately before
the transaction, still outside it. A different answer for the same captured selection remains
`METER_EVIDENCE_CHANGED` and is never retried. This applies to price submit, plan submit,
publish-changes and both subjects' voting/apply paths. Entry-create recovery continues to use
its persisted evidence, and historical reads make no provider calls.


The revision fingerprint now includes each selected entry ID and its policy ID/version/digest,
read from entry rows in the same transaction. Policy content remains entry-owned; no plan-item
column or override is added. Changed selection refreshes the approval generation (`UNIT_STALE`)
and an old approval cannot publish it. A scheduled revision is checked at approval; D-450's later
switch does not revalidate dependencies. New usage approvals require a policy-bearing entry;
legacy approved prices and published revisions remain readable.

## 2. Actor Flows (CDSL)

### Prepare and publish a revision

Actors: `cpt-cf-bss-pricing-actor-product-manager`, `cpt-cf-bss-pricing-actor-products`, `cpt-cf-bss-pricing-actor-subscriptions`. Feature flow: `cpt-cf-bss-pricing-flow-plans`.

1. [ ] - `p1` - Product Manager copies published structure into a new draft revision, or starts a new plan. - `inst-plans-flow-1`
2. [ ] - `p1` - Select one book, items (each a SKU and, once chosen, its entry in the book; the entry may be set after the SKU, D-467, D-512) and availability; add each item through the item sub-resource, which reserves its reference before the write (D-407), while a copied item attaches its reference after the copy is written (D-413). Grants and the sold-as bundle SKU are deferred by the owner (D-411). - `inst-plans-flow-2`
3. [ ] - `p1` - Read checks for the sale date and all dimension values; show ITEM_UNCOVERED and computed blocked_by price units when coverage is missing. - `inst-plans-flow-3`
4. [ ] - `p1` - After checks pass, submit a separate plan_revision unit; revalidate on apply. - `inst-plans-flow-4`
5. [ ] - `p1` - On approval publish the revision, supersede the previous published revision and advance plan.published_rev atomically; existing subscription pins remain unchanged. - `inst-plans-flow-5`

## 3. Processes / Business Logic (CDSL)

### revision-checks

Feature algorithm: `cpt-cf-bss-pricing-algo-plans-revision-checks`.

1. [ ] - `p1` - Read every item SKU fresh and check it is allowed, non-bundle and not newly deprecated (D-408); validate every item reference's receipt (D-413) and that every item names an entry (ITEM_ENTRY_MISSING, "Every item points at a price", D-467, D-512). A draft may hold an item that does not, and submit refuses it. - `inst-plans-revision-checks-1`
2. [ ] - `p1` - Enforce one recurring frequency and a unique usage meter among the items with an entry; reject foreign-book entries. - `inst-plans-revision-checks-2`
3. [ ] - `p1` - For every registered dimension value, verify sale-date coverage and an open tail through its own or the default chain; check book validity. - `inst-plans-revision-checks-3`
4. [ ] - `p1` - When uncovered, compute blocking pending price unit ids from current prices, with the pending prices behind them, and name on every row the items that turn it red (D-466); return checks, never persist blocked_by or create a unit while red. - `inst-plans-revision-checks-4`

### revision-apply

Feature algorithm: `cpt-cf-bss-pricing-algo-plans-revision-apply`.

1. [ ] - `p1` - Claim the unit version and verify generation, SoD and business-content fingerprint through slice 05. - `inst-plans-revision-apply-1`
2. [ ] - `p1` - Revalidate book, SKU lifecycle and complete coverage inside apply; changed environment refuses apply without partial publication. - `inst-plans-revision-apply-2`
3. [ ] - `p1` - Publish the selected revision, supersede the previous one and update published_rev, audit and PlanRevisionPublished in one transaction. - `inst-plans-revision-apply-3`
4. [ ] - `p1` - Keep all historical revision/book bindings and subscription pins intact. - `inst-plans-revision-apply-4`
5. [ ] - `p1` - When the sale date is after the apply's UTC day, schedule the revision instead: its lock becomes approved_by_unit_id, nothing is superseded or published and no PlanRevisionPublished is written; the switch on its date publishes it (D-449, D-450). - `inst-plans-revision-apply-5`

### revision-switch

Feature algorithm: `cpt-cf-bss-pricing-algo-plans-revision-switch`.

1. [ ] - `p1` - The pricing ticker's switch duty runs first in its tick, on the first tick and then every 60 ticks, with its own error handling: it scans the due scheduled revisions of every tenant, bounded and ordered, and switches each plan in its own transaction (D-450). - `inst-plans-revision-switch-1`
2. [ ] - `p1` - Persist the switch through switch_due and, only when it switched, enqueue PlanRevisionPublished (the approving unit, its latest current approver or else its submitter, the superseded revision) and audit plan_revision.switch under the system actor, in the same transaction (D-450). - `inst-plans-revision-switch-2`
3. [ ] - `p1` - The copy, clone and unschedule doors catch the plan's due switch up first; the copy is refused with REVISION_SCHEDULED while a revision waits for its sale date (D-451). - `inst-plans-revision-switch-3`
4. [ ] - `p1` - Unschedule returns a waiting revision to an unlocked draft under plan submit, with its items and references kept; a published revision is REVISION_IN_EFFECT and any other REVISION_NOT_SCHEDULED (D-452). - `inst-plans-revision-switch-4`
5. [ ] - `p1` - Every read derives the effective state from the stored revisions and never writes; resolve serves a waiting revision from its sale date and refuses it before with REVISION_NOT_YET_AVAILABLE (D-453, D-454). - `inst-plans-revision-switch-5`

### clone-and-retire

Feature algorithm: `cpt-cf-bss-pricing-algo-plans-clone-and-retire`. Retirement (steps 2 to 4) is deferred by the owner (D-410, 2026-09-25) and not built in phase 3; clone stays.

1. [ ] - `p1` - Clone creates a new uniquely coded draft with copied structure and fresh reference attempts; it does not clone approved identity or decisions. - `inst-plans-clone-and-retire-1`
2. [ ] - `p1` - Retirement requires an explicit migration proposal against an eligible published target. - `inst-plans-clone-and-retire-2`
3. [ ] - `p1` - Persist and approve the migration request through slice 06, retaining references required by live or historical bindings. - `inst-plans-clone-and-retire-3`
4. [ ] - `p1` - Wait for the separately implemented Subscriptions completion contract before claiming retirement completion; release references only after durable cancellation/removal is valid. - `inst-plans-clone-and-retire-4`

## 4. States (CDSL)

Revision states are draft → pending → published → superseded, and pending → scheduled → published when the unit is approved before the sale date (D-449): a scheduled revision reads as published from 00:00 UTC of its date, before the switch job or a door persists it (D-447, D-450, D-451), and unschedule returns it to draft (D-452). A rejected or withdrawn unit returns its revision to draft without publishing it. Plan retirement is deferred by the owner (D-410). A blocked draft has no unit; blocked_by is a computed check result. A retirement request does not mean all subscriptions have moved.

State definition: `cpt-cf-bss-pricing-state-plans` in the FEATURE.

## 5. API Surface

Under /bss-pricing/v1: POST/GET /plans and GET/PATCH /plans/{id}, where a new plan's code, on POST /plans and on the clone, is 1 to 32 characters of A-Z, 0-9, - and _, starting with a letter or a digit, judged as sent with no trim or case folding (400 PLAN_CODE_INVALID after FIELD_TOO_LONG and PLAN_CODE_REQUIRED, before the rest of D-456's order), a code stored before the rule keeps reading and the name-only PATCH judges no code (D-468); GET /plans pages on the stored summary (D-484, D-485): the page, the revisions, the current and in-effect items, the units and the current revisions' books, five statements for a non-empty page, and names each plan's current revision (the draft or pending one, else the scheduled one, else the published one in effect, with its item count, item SKUs, author and book — id, code, name, currency, valid_from and valid_until, D-515) and the published revision in effect (D-460), and each header's author, its book beside book_id (D-516), and the instants its revision was submitted and approved (D-461), and GET /plans?sku_id= answers, in the same shape, the plans whose draft, pending, scheduled or published revisions name the SKU through an entry, the SKU usage's plans (D-428, D-434); every read renders a revision's state as it reads today (D-453); GET /plan-items/{id} reads one item with its plan_id, rev_no and its revision's state, its version as the ETag (D-434); POST /plans/{id}/revisions copies the published revision to a draft (D-413), the one in effect after a due switch is caught up, and is 409 REVISION_SCHEDULED while a revision waits for its sale date (D-451); GET, PATCH and DELETE /plan-revisions/{id}, where the read and every answer of a revision carry submitted_at and approved_at (D-461) and a pending revision its vote progress, counts only, under plan read (D-462), and PATCH takes book_id and available_from but no item list (a new book_id remaps each item to the new book's entry of the same (SKU, charge kind, normalized period, model, policy digest), with an equal dimension key (D-502), bumping the item's version; an unmatched item keeps its entry, then ITEM_BOOK_FOREIGN; book_id omitted or null leaves the book unchanged, and the served text says both, D-469), and the DELETE of the last revision of a never-published plan deletes the plan too, freeing its code (D-417); items are a sub-resource: POST /plan-revisions/{id}/items, PATCH and DELETE /plan-items/{id}; an added item is one op with its own Idempotency-Key, and a removed item is one delete op (the DELETE takes no key), refused 409 ITEM_CONFIRMATION_PENDING while its confirm is outstanding (D-407); GET /plan-revisions/{id}/checks answers { checks, ready, sale_date }, each row with the items that turn it red (subjects) and the pending prices behind blocked_by (blocked_by_prices, D-466); POST /plan-revisions/{id}/submit (plan submit, D-418, and an Idempotency-Key, with an optional body { note }: the submitter's note, stored as the unit's submit_note, at most 2000 characters judged before any read, and no other key, D-464) records a plan_revision unit for an unlocked draft (409 REVISION_NOT_DRAFT otherwise) whose checks are all green: they are the checks of GET …/checks, built by the same function from the stored state of the unit's transaction and a fresh sku_for_write read of every item SKU; a red check is 400 REVISION_CHECKS_RED with the red checks and no unit, a registry that cannot answer is 503 with nothing written. The lock is the conditional pending_unit_id (409 ROW_LOCKED_PENDING when lost); the unit's after is the business content (book_id, available_from and the items by SKU: sku_id and price_book_entry_id, D-467) and before the published revision's; the item SKUs' current descriptors ride beside it, never in it (D-408). The submitter and the revision's author may not approve. Apply re-runs the checks with fresh reads (red: APPLY_REFUSED, nothing published), supersedes the published revision first, then publishes this one and advances published_rev; a sale date after the apply's day schedules the revision instead (D-449). POST /plan-revisions/{id}/unschedule (plan submit and an Idempotency-Key, no body) returns a waiting revision to a draft, 409 REVISION_IN_EFFECT for a published revision and REVISION_NOT_SCHEDULED for any other (D-452). POST /plans/{id}/clone with code and name (plan author and an Idempotency-Key) answers 201 with a new plan whose draft rev 1 copies the source's published revision (the one in effect, D-451) — book, availability and items, each written unreserved with an attach op (D-413) — and nothing of its approval: no decision, approved_by_unit_id, published_at or pin. An available_from in the body overrides the availability, and null clears it, as POST /plans takes one for rev 1 (400 DATE_INVALID for a date that does not read, D-463); 409 CLONE_SOURCE_UNPUBLISHED without a published revision, 409 PLAN_CODE_TAKEN, 400 PLAN_CODE_REQUIRED. A carried deprecated SKU attaches and is red in the new plan's checks (ITEM_SKU_DEPRECATED, D-408). The item create admits a deprecated SKU only when the plan's published revision in effect carries it, so a draft may add back a SKU its plan sells, and a clone's may not (D-465). Deferred by the owner: sold-as and grants in the revision PATCH (D-411), and POST /plans/{id}/retire with the migration proposals of slice 06 (D-410). Draft mutations require author, submission requires submit; POST replay and PATCH If-Match apply. POST /plans, the clone and a PATCH that names a book also judge price_book read on the book the plan names, as the money is judged (403 PRICE_BOOK_READ_REQUIRED, 503 when the policy cannot judge, D-456).

[DESIGN §3.3](../DESIGN.md#33-api-contracts) fixes canonical errors and route prefixes.
Each mounted route must appear in all four censuses with authz and precondition expectations.

## 6. Data Model

Phase 3 adds pricing_plan(id, tenant_id, code, name, published_rev), unique tenant/code; pricing_plan_revision(id, tenant_id, plan_id, rev_no, book_id, state draft|pending|scheduled|published|superseded, available_from, pending_unit_id, approved_by_unit_id, published_at), unique plan/rev_no, one draft or pending, one scheduled and one published revision per plan (scheduled: m20260929_000017, D-446); pricing_plan_item(id, tenant_id, revision_id, sku_id, price_book_entry_id nullable, treatment paid|optional|included, included_qty, qty_min, reservation_id, reference_state unreserved|confirmation_pending|confirmed|lost), unique revision/SKU. Mutable drafts carry versions/timestamps and creator attribution; plan_item references retain receipt/pending-confirm/release recovery as in slice 03, and a copied one starts unreserved (D-413). A published revision book binding is immutable, and its references outlive it (D-414). Since D-467 an item is a SKU and its entry: no door writes or reads treatment, included_qty or qty_min, and a row that names an entry stores paid, NULL and NULL. A null price_book_entry_id is a draft item waiting for its entry (D-512) or a legacy included item stored before D-467; that row stores treatment included, the shape the column CHECK admits, and the checks show ITEM_ENTRY_MISSING. Copy, clone and a book remap keep it entry-less and never choose an entry. Deferred by the owner and not in the phase 3 chain: plan.bundle_sku_id and grants with the sold-as reference (D-411), and the plan's retiring state (D-410). [DESIGN §3.7](../DESIGN.md#37-database-schemas--tables) holds the DDL.

Tenant-scoped parent validation is required even where foreign keys use entity ids. Never substitute a
cross-gear read for transactional local ownership/version guards. Approved money and historical pins survive.

## 7. Events & Alarms

PlanRevisionPublished { plan_id, revision_id, rev_no, book_id, superseded_revision_id (null for a plan's first publication), unit_id, actor_ref }, about the plan and naming its tenant_id, shares the successful apply transaction and terminal ApprovalUnitDecided; a revision approved before its sale date is announced instead in the transaction that persists its switch, with actor_ref the unit's latest current approver, or its submitter at quorum 0 (D-450); every terminal transition of a plan_revision unit (applied, rejected, withdrawn) writes ApprovalUnitDecided. PlanReferenceLost reports a lost item reference: { plan_id, revision_id, item_id, sku_id, reservation_id (null when a copied item never attached), actor_ref }, about the item and naming its tenant_id. PlanRetired represents an actual completed retirement, not merely an approved migration request; it is deferred with retirement (D-410). SubscriptionMigrationRequested belongs to slice 06. Existing pins remain readable after either event.

Audit and outbox inserts use the same mutation transaction; retry is lifecycle-managed and observes shutdown.

## 8. Definitions of Done

The sole definitions live in [features/plans.md](../features/plans.md):

- `cpt-cf-bss-pricing-dod-plan-revision-book` — Immutable revision book binding.
- `cpt-cf-bss-pricing-dod-plan-item-rules` — Item and frequency rules.
- `cpt-cf-bss-pricing-dod-plan-coverage` — Coverage for every value.
- `cpt-cf-bss-pricing-dod-plan-blocked-by` — Computed blocking units.
- `cpt-cf-bss-pricing-dod-plan-revision-unit` — Independent revision approval.
- `cpt-cf-bss-pricing-dod-plan-reference-protocol` — Item references.
- `cpt-cf-bss-pricing-dod-plan-grants` — Items and grants.
- `cpt-cf-bss-pricing-dod-plan-clone` — Clone into a fresh draft.
- `cpt-cf-bss-pricing-dod-plan-retire-migration` — Retirement requires migration (deferred, D-410).

## 9. Acceptance Criteria

1. PRD AC #15 / `cpt-cf-bss-pricing-dod-plan-revision-book`: Given published rev 4 in EUR book A, when rev 5 chooses book B then rev 4 keeps A; direct published PATCH is refused.
2. PRD AC #15 / `cpt-cf-bss-pricing-dod-plan-item-rules`: Given otherwise valid items, when a second recurring period or foreign-book entry is added then FREQUENCY_MIXED or ITEM_BOOK_FOREIGN blocks submit; the valid set passes.
3. PRD AC #15 / `cpt-cf-bss-pricing-dod-plan-coverage`: Given EU coverage but uncovered US, when checks run then ITEM_UNCOVERED identifies US; complete own-value chains pass without a default, while invalid book dates fail.
4. PRD AC #15 / `cpt-cf-bss-pricing-dod-plan-blocked-by`: Given pending price unit ap-12 covering a gap, when revision checks run then they name ap-12; rejection or withdrawal changes the next check rather than leaving a stored dependency.
5. PRD AC #15 / `cpt-cf-bss-pricing-dod-plan-revision-unit`: Given an approved repricing and rejected revision, when both outcomes are read then the old revision uses the new book money and the rejected revision is not published.
6. PRD AC #11 / `cpt-cf-bss-pricing-dod-plan-reference-protocol`: Given a plan_item reservation and confirmation outage, when the draft commits then the reference stays protective and retryable; bundle items remain forbidden.
7. PRD AC #15 / `cpt-cf-bss-pricing-dod-plan-grants`: Given a revision, when its structure is read then each item is a SKU and, once chosen, its entry, with no treatment, included quantity or minimum quantity, and an item create or PATCH that carries one of them is refused; a draft may add a SKU with no entry, and that item, like a legacy included item, reads with a null entry and is ITEM_ENTRY_MISSING until a PATCH sets one (D-467, D-512). Grants are deferred (D-411).
8. PRD AC #15 / `cpt-cf-bss-pricing-dod-plan-clone`: Given a published source, when clone succeeds then the destination is a separate draft; duplicate tenant code and a code off the rule (D-468) are refused and changing the clone leaves the source unchanged.
9. PRD AC #15 / `cpt-cf-bss-pricing-dod-plan-retire-migration` (deferred, D-410): Given subscriptions pinned to a retiring plan, when only the request is approved then movement is not reported complete; an invalid target blocks the request.

## 10. Non-Functional Considerations

All paths enforce tenant isolation, deny-by-default authz and append-only attribution. Mutations use conditional
versions, required POST replay and PATCH/PUT preconditions; PostgreSQL serializable chain changes and SQLite
writer serialization preserve the same invariants. A failed audit/outbox write cannot leave a committed act.
No timeout releases a live reference. Review and apply retain typed database failures for bounded retry.
Implementation gates cover both backends and route censuses; document gates cover toc, language and identifier ownership.
