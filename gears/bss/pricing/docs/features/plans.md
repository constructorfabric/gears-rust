<!-- CONFLUENCE_TITLE: [BSS]: Pricing — Plans (Feature) -->
<!-- Related: ../DECOMPOSITION.md, ../DESIGN.md, ../PRD.md | Owners: BSS Pricing team -->

# Feature: Plans

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-featstatus-plans-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-bss-pricing-feature-plans`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Prepare and publish a revision](#prepare-and-publish-a-revision)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [revision-checks](#revision-checks)
  - [revision-apply](#revision-apply)
  - [revision-switch](#revision-switch)
  - [clone-and-retire](#clone-and-retire)
- [4. States (CDSL)](#4-states-cdsl)
  - [Plans states](#plans-states)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Immutable revision book binding](#immutable-revision-book-binding)
  - [Item and frequency rules](#item-and-frequency-rules)
  - [Coverage for every value](#coverage-for-every-value)
  - [Computed blocking units](#computed-blocking-units)
  - [Independent revision approval](#independent-revision-approval)
  - [Item references](#item-references)
  - [Items and grants](#items-and-grants)
  - [Clone into a fresh draft](#clone-into-a-fresh-draft)
  - [Retirement requires migration](#retirement-requires-migration)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

**Delivery:** phase 3. Every checkbox is an implementation obligation, not an assertion about the legacy code. This complete phase 3 design remains unchecked during phase 2.

This feature implements [slice 04](../design/04-plans.md).
[DECOMPOSITION](../DECOMPOSITION.md) records integration order; [DESIGN §3](../DESIGN.md#3-technical-architecture)
is the schema and transaction authority. Unchecked phase 3/4 work is not part of the phase 2 core gate.

### 1.2 Purpose

Publish independent revision structure against book coverage, preserving existing pins; author items, clone and retirement prerequisites.
Phase 6 adds the reads the SKUs screen needs (D-434): GET /plans?sku_id= lists the plans whose draft, pending, scheduled or published revisions name the SKU through an entry (the scheduled state since D-453), in the shape of GET /plans, which now reads its revisions in one statement for every plan; GET /plan-items/{id} reads one item with its plan and its revision's number and state.
Phase 8 adds scheduled revisions (D-446 to D-454): an approval before the sale date schedules the revision, the pricing ticker's switch duty and the copy, clone and unschedule doors persist its switch on the date and announce it once, POST /plan-revisions/{id}/unschedule withdraws it to a draft, and every read derives the state a revision reads today.
Phase 9 adds what the plans screen reads per plan: its current revision, with its item count, item SKUs and author, and the published revision in effect (D-460); each revision's author, its book beside book_id (D-516), and when it was submitted and approved (D-461); and a pending revision's vote progress (D-462). A plan's create and clone take its sale date (D-463), and its submit a note for the approver (D-464); a draft may add again a deprecated SKU its published revision in effect carries (D-465). Run 9.2 names on each check row the items that turn it red and the pending prices behind blocked_by (D-466); makes a plan item a SKU and its entry, with no treatment, included quantity or minimum quantity (D-467); gives a new plan's code a rule (D-468); and states in the served contract the 503 of every door, the REGISTRY_UNAVAILABLE of the doors that read Products hard, the ETag of every answer that sets one and the refusals of the item create, the revision PATCH and the revision delete (D-469). Run 9.8b stores a time-stable summary on the plan (D-484) and pages GET /plans on it (D-485): selling and change are derived from that summary and the request's day, the current revision names its book (id, code, name, currency, valid_from and valid_until, D-515), and GET /plans/counts groups the same narrowing. A tenant of more than 500 plans follows next_cursor. Run 9.6 makes GET /plan-revisions/{id} a read-only answer: the sale date, one summary per distinct entry with the price in force on that date, and, for a draft or a pending revision, the SKUs the plan sells today (D-480). reservations_settled is on every revision answer, and GET /plan-revisions/{id}/reservations reads each item's reference in two statements. The plan in effect names those SKUs (D-480). The checks name the quorum a submit needs, and GET /approval-policy/{kind}/effective reads it under the kind's own grant (D-481). The book list filters on id (D-442, amended by D-480). GET /plans and GET /plans/counts answer 304 when If-None-Match matches a weak ETag of the JSON body, and send Cache-Control: private, no-cache (D-518). Every plan, revision and item read names its authors: each `created_by` carries `created_by_name`, the current name through Account Management, resolved once per answer after the read's statements, null when it is not available now and "System" for pricing's system actor; a write answer's is null (D-519).

Requirements: `cpt-cf-bss-pricing-fr-plans`, `cpt-cf-bss-pricing-fr-reference-protocol`.

Architecture: `cpt-cf-bss-pricing-component-plans`, `cpt-cf-bss-pricing-component-approvals`, `cpt-cf-bss-pricing-component-reservations-client`, `cpt-cf-bss-pricing-principle-book-money-independent`, `cpt-cf-bss-pricing-principle-reserve-before-write`, `cpt-cf-bss-pricing-constraint-no-row-locks`, `cpt-cf-bss-pricing-seq-blocked-revision`, `cpt-cf-bss-pricing-seq-reserve-write-confirm`.

### 1.3 Actors

`cpt-cf-bss-pricing-actor-product-manager`, `cpt-cf-bss-pricing-actor-products`, `cpt-cf-bss-pricing-actor-subscriptions`. Every operation authenticates and derives tenant scope before storage, replay or cross-gear calls.
Holding multiple permissions never bypasses separation of duties.

### 1.4 References

- [PRD](../PRD.md), especially the numbered acceptance criteria referenced below.
- [DESIGN](../DESIGN.md), §3 model, API contracts, transaction sequences and DDL.
- [Slice 04](../design/04-plans.md), including API, data and event obligations.
- [DECISIONS](../DECISIONS.md), D-384–D-433; spec means `docs/superpowers/specs/2026-09-24-pricebook-model-design.md` in the main checkout.
- Source: spec §2 decisions 4–8, 13–17, §2.2, §5–§8, §10, §12–§13; the phase 2 plan supplies delivery boundaries and D-399/D-400.

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

The revision fingerprint now includes each selected entry ID and its policy ID/version/digest,
read from entry rows in the same transaction. Policy content remains entry-owned; no plan-item
column or override is added. Changed selection refreshes the approval generation (`UNIT_STALE`)
and an old approval cannot publish it. A scheduled revision is checked at approval; D-450's later
switch does not revalidate dependencies. New usage approvals require a policy-bearing entry;
legacy approved prices and published revisions remain readable.

## 2. Actor Flows (CDSL)

### Prepare and publish a revision

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-flow-plans`

1. [x] - `p1` - Product Manager copies published structure into a new draft revision, or starts a new plan. - `inst-plans-flow-1`
2. [x] - `p1` - Select one book, items (each a SKU and, once chosen, its entry in the book; the entry may be set after the SKU, D-467, D-512) and availability; add each item through the item sub-resource, which reserves its reference before the write (D-407), while a copied item attaches its reference after the copy is written (D-413). Grants and the sold-as bundle SKU are deferred by the owner (D-411). - `inst-plans-flow-2`
3. [x] - `p1` - Read checks for the sale date and all dimension values, for one revision or for up to 50 in one read (D-482); show ITEM_UNCOVERED and computed blocked_by price units when coverage is missing. - `inst-plans-flow-3`
4. [x] - `p1` - After checks pass, submit a separate plan_revision unit; revalidate on apply. - `inst-plans-flow-4`
5. [x] - `p1` - On approval publish the revision, supersede the previous published revision and advance plan.published_rev atomically; existing subscription pins remain unchanged. - `inst-plans-flow-5`

## 3. Processes / Business Logic (CDSL)

### revision-checks

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-algo-plans-revision-checks`

1. [x] - `p1` - Read every item SKU fresh, in one call, and check it is allowed, non-bundle and not newly deprecated (D-408, D-482); validate every item reference's receipt (D-413) and that every item names an entry (ITEM_ENTRY_MISSING, "Every item points at a price", D-467, D-512). A draft may hold an item that does not, and submit refuses it. - `inst-plans-revision-checks-1`
2. [x] - `p1` - Enforce one recurring frequency and a unique usage meter among the items with an entry; reject foreign-book entries. - `inst-plans-revision-checks-2`
3. [x] - `p1` - For every registered dimension value, verify sale-date coverage and an open tail through its own or the default chain; check book validity. - `inst-plans-revision-checks-3`
4. [x] - `p1` - When uncovered, compute blocking pending price unit ids from current prices, with the pending prices behind them, and name on every row the items that turn it red (D-466); return checks, never persist blocked_by or create a unit while red. - `inst-plans-revision-checks-4`

### revision-apply

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-algo-plans-revision-apply`

1. [x] - `p1` - Claim the unit version and verify generation, SoD and business-content fingerprint through slice 05. - `inst-plans-revision-apply-1`
2. [x] - `p1` - Revalidate book, SKU lifecycle and complete coverage inside apply; changed environment refuses apply without partial publication. - `inst-plans-revision-apply-2`
3. [x] - `p1` - Publish the selected revision, supersede the previous one and update published_rev, audit and PlanRevisionPublished in one transaction. - `inst-plans-revision-apply-3`
4. [x] - `p1` - Keep all historical revision/book bindings and subscription pins intact. - `inst-plans-revision-apply-4`
5. [x] - `p1` - When the sale date is after the apply's UTC day, schedule the revision instead: its lock becomes approved_by_unit_id, nothing is superseded or published and no PlanRevisionPublished is written; the switch on its date publishes it (D-449, D-450). - `inst-plans-revision-apply-5`

### revision-switch

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-algo-plans-revision-switch`

1. [x] - `p1` - The pricing ticker's switch duty runs first in its tick, on the first tick and then every 60 ticks, with its own error handling: it scans the due scheduled revisions of every tenant, bounded and ordered, and switches each plan in its own transaction (D-450). - `inst-plans-revision-switch-1`
2. [x] - `p1` - Persist the switch through switch_due and, only when it switched, enqueue PlanRevisionPublished (the approving unit, its latest current approver or else its submitter, the superseded revision) and audit plan_revision.switch under the system actor, in the same transaction (D-450). - `inst-plans-revision-switch-2`
3. [x] - `p1` - The copy, clone and unschedule doors catch the plan's due switch up first; the copy is refused with REVISION_SCHEDULED while a revision waits for its sale date (D-451). - `inst-plans-revision-switch-3`
4. [x] - `p1` - Unschedule returns a waiting revision to an unlocked draft under plan submit, with its items and references kept; a published revision is REVISION_IN_EFFECT and any other REVISION_NOT_SCHEDULED (D-452). - `inst-plans-revision-switch-4`
5. [x] - `p1` - Every read derives the effective state from the stored revisions and never writes; resolve serves a waiting revision from its sale date and refuses it before with REVISION_NOT_YET_AVAILABLE (D-453, D-454). - `inst-plans-revision-switch-5`

### clone-and-retire

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-algo-plans-clone-and-retire`

Retirement (steps 2 to 4) is deferred by the owner (D-410, 2026-09-25) and not built in phase 3; clone stays.

1. [x] - `p1` - Clone creates a new uniquely coded draft with copied structure and fresh reference attempts; it does not clone approved identity or decisions. - `inst-plans-clone-and-retire-1`
2. [ ] - `p1` - Retirement requires an explicit migration proposal against an eligible published target. - `inst-plans-clone-and-retire-2`
3. [ ] - `p1` - Persist and approve the migration request through slice 06, retaining references required by live or historical bindings. - `inst-plans-clone-and-retire-3`
4. [ ] - `p1` - Wait for the separately implemented Subscriptions completion contract before claiming retirement completion; release references only after durable cancellation/removal is valid. - `inst-plans-clone-and-retire-4`

## 4. States (CDSL)

### Plans states

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-state-plans`

Revision states are draft → pending → published → superseded, and pending → scheduled → published when the unit is approved before the sale date (D-449): a scheduled revision reads as published from 00:00 UTC of its date, before the switch job or a door persists it (D-447, D-450, D-451), and unschedule returns it to draft (D-452). Submit locks the draft under its unit (pending_unit_id, conditional), a rejected or withdrawn unit returns its revision to an editable draft without publishing it, and apply supersedes the published revision before it publishes this one. Plan retirement is deferred by the owner (D-410). A blocked draft has no unit; blocked_by is a computed check result. A retirement request does not mean all subscriptions have moved.

## 5. Definitions of Done

Every DoD below is required for this feature's delivery phase. Constraints: `cpt-cf-bss-pricing-constraint-no-row-locks`.

### Immutable revision book binding

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-plan-revision-book`

Each published revision owns one book and immutable item structure. Copying to draft allocates a new revision number; publishing cannot rewrite a historical book binding (spec §5).

Requirement: `cpt-cf-bss-pricing-fr-plans`; PRD AC #15.

### Item and frequency rules

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-plan-item-rules`

Validate recurring frequency, duplicate usage meters and an entry for every item (D-467). Reject bundle items, a deprecated SKU newly added (one carried over from the same plan's published revision stays, D-408, and one that revision carries may be added again, D-465) and foreign-book entries using the named spec errors (spec §5).

Requirement: `cpt-cf-bss-pricing-fr-plans`; PRD AC #15.

### Coverage for every value

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-plan-coverage`

Sale-date checks include each dimension value via own chain or default plus open-tail coverage and book validity. Missing default alone is not an error when every value is covered (spec §5, §14).

Requirement: `cpt-cf-bss-pricing-fr-plans`; PRD AC #15.

### Computed blocking units

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-plan-blocked-by`

Checks derive blocked_by from current pending prices of uncovered entries, and blocked_by_prices names each of those prices with its unit and entry; every row names the items that turn it red, in the revision's item order, and a plan-wide or green row names none (D-466). A red draft creates no plan_revision unit and becomes eligible only after a fresh successful check (spec §6, §8).

Requirement: `cpt-cf-bss-pricing-fr-plans`; PRD AC #15.

### Independent revision approval

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-plan-revision-unit`

plan_revision uses the shared generation/SoD/quorum rules and revalidates coverage on apply. It publishes structure independently of approved money and never migrates pins (spec §6, §12).

Requirement: `cpt-cf-bss-pricing-fr-plans`; PRD AC #15.

### Item references

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-plan-reference-protocol`

A new plan_item reference, added through the item sub-resource, uses reserve, SKU re-read, local receipt/work commit and confirm (D-407). A copied item is written unreserved and attaches after the write (D-413). Removal/cancellation precedes durable release, with the same lost-receipt handling as entries; published and superseded revisions keep their references (D-414) (spec §13). The sold_as reference is deferred with the sold-as bundle (D-411).

Requirement: `cpt-cf-bss-pricing-fr-reference-protocol`; PRD AC #11.

### Items and grants

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-dod-plan-grants`

A plan item is a SKU and its entry in the plan's book: no included item, no optional item and no quantity remain, and no phase schedule or prepaid-grant system returns (spec §3 item 25, §5; D-467). Rows stored before D-467 keep reading with those fields hidden. Revision grants are deferred by the owner (D-411).

Requirement: `cpt-cf-bss-pricing-fr-plans`; PRD AC #15.

### Clone into a fresh draft

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-plan-clone`

Clone preserves authorable structure while allocating new plan/revision ids and reference attempts: the new plan's draft rev 1 copies the source's published revision (book, availability, items), each item written unreserved and attached after the write (D-413); an available_from in the body overrides the availability, and null clears it (D-463). The new plan's code follows the rule of POST /plans: 1 to 32 characters of A-Z, 0-9, - and _, starting with a letter or a digit, judged as sent with no trim or case folding, else PLAN_CODE_INVALID (D-468); the source's own code, if stored before the rule, is never judged. It does not inherit approval decisions, published state (approved_by_unit_id, published_at) or subscription pins; a source with no published revision is refused CLONE_SOURCE_UNPUBLISHED, and a carried deprecated SKU is red in the new plan's checks (D-408) (spec §3 item 28).

Requirement: `cpt-cf-bss-pricing-fr-plans`; PRD AC #15.

### Retirement requires migration

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-dod-plan-retire-migration`

Deferred by the owner (D-410, 2026-09-25): not built in phase 3. This DoD stays unticked.

Retirement is coupled to an explicit migration request and cannot silently strand subscribers. Pricing records request approval; Subscriptions executes movement and confirms completion in its own plan (spec §11).

Requirement: `cpt-cf-bss-pricing-fr-plans`; PRD AC #15.

## 6. Acceptance Criteria

| DoD | PRD criterion | Given / When / Then |
| --- | --- | --- |
| `cpt-cf-bss-pricing-dod-plan-revision-book` | AC #15; `cpt-cf-bss-pricing-fr-plans` | Given published rev 4 in EUR book A, when rev 5 chooses book B then rev 4 keeps A; direct published PATCH is refused. |
| `cpt-cf-bss-pricing-dod-plan-item-rules` | AC #15; `cpt-cf-bss-pricing-fr-plans` | Given otherwise valid items, when a second recurring period or foreign-book entry is added then FREQUENCY_MIXED or ITEM_BOOK_FOREIGN blocks submit; the valid set passes. |
| `cpt-cf-bss-pricing-dod-plan-coverage` | AC #15; `cpt-cf-bss-pricing-fr-plans` | Given EU coverage but uncovered US, when checks run then ITEM_UNCOVERED identifies US; complete own-value chains pass without a default, while invalid book dates fail. |
| `cpt-cf-bss-pricing-dod-plan-blocked-by` | AC #15; `cpt-cf-bss-pricing-fr-plans` | Given pending price unit ap-12 covering a gap, when revision checks run then they name ap-12; rejection or withdrawal changes the next check rather than leaving a stored dependency. |
| `cpt-cf-bss-pricing-dod-plan-revision-unit` | AC #15; `cpt-cf-bss-pricing-fr-plans` | Given an approved repricing and rejected revision, when both outcomes are read then the old revision uses the new book money and the rejected revision is not published. |
| `cpt-cf-bss-pricing-dod-plan-reference-protocol` | AC #11; `cpt-cf-bss-pricing-fr-reference-protocol` | Given a plan_item reservation and confirmation outage, when the draft commits then the reference stays protective and retryable; bundle items remain forbidden. |
| `cpt-cf-bss-pricing-dod-plan-grants` | AC #15; `cpt-cf-bss-pricing-fr-plans` | Given a revision, when its structure is read then each item is a SKU and, once chosen, its entry, with no treatment, included quantity or minimum quantity, and an item create or PATCH that carries one of them is refused; a draft may add a SKU with no entry, and that item, like a legacy included item, reads with a null entry and is ITEM_ENTRY_MISSING until a PATCH sets one (D-467, D-512). Grants are deferred (D-411). A revision read names its sale date, one entry summary per distinct entry and, while draft or pending, the SKUs in effect; a price is shown only for a book the caller's price_book read admits (D-480). |
| `cpt-cf-bss-pricing-dod-plan-clone` | AC #15; `cpt-cf-bss-pricing-fr-plans` | Given a published source, when clone succeeds then the destination is a separate draft; duplicate tenant code and a code off the rule (D-468) are refused and changing the clone leaves the source unchanged. |
| `cpt-cf-bss-pricing-dod-plan-retire-migration` | AC #15; `cpt-cf-bss-pricing-fr-plans` | Deferred (D-410). Given subscriptions pinned to a retiring plan, when only the request is approved then movement is not reported complete; an invalid target blocks the request. |

Verification uses domain tests, scoped repository tests on both backends and REST positive/denial/precondition probes as applicable. Phase 2 checks must not mark later-phase behavior implemented. Golden consumer contracts belong to phase 4.


**Final seam conformance (D-510).** Items select entries and expose immutable policy only by projection. Mixed-window plans preserve each
item's policy; reusing an entry across plans never pools subscription-line usage. Copy/clone keeps entry
identity; book remapping matches SKU/kind/period/model, dimension and policy digest. Submit and final
apply bind the exact selected entry/policy version and validate authoritative meter evidence. Concurrent
scheduled promotion has one durable published revision while existing acceptance and holds preserve
the old entry/policy/price. Orders supplies explicit versioned BillingTerms independently of usage period
(null); Pricing never infers a month or normalizes an anchor. Subscriptions owns order/version/attempt
fencing and activates within accepted bounds; a hold is never a reusable eligibility token.

The final seven typed signatures, CommercialReason mappings, PDP authorization, indefinite receipt
retention, supported-model matrix and provider-test links are consolidated in
[DESIGN](../DESIGN.md#executable-seam-fixture-boundary-d-509) and [PRD](../PRD.md). Acceptance and hold
commands are SDK-only; historical replay never refreshes the original 24-hour seller-policy deadline.
E1a (real raw meter declarations/adapter/provenance), E2 (resolved terms, authenticated market and
consumer fencing), E3 (runtime PDP grants) and E4 (source history and safe policy transitions) remain
external; E1b (Products' derived usage types) is provided by Products (products P-D-233).
E1 = E1a (raw meters, the usage collector / types registry; external) + E1b (derived meters, provided by Products since P-D-233).
The atlas owner reconciles C00/C01/C10; downstream Rating scheduling and Billing invoicing remain
unexecuted integration obligations, even when Pricing provider parity is green.
