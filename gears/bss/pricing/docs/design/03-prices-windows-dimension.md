<!-- CONFLUENCE_TITLE: [BSS]: Pricing — Prices, Windows & Dimension (Design, Slice 3) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../DECISIONS.md | Owners: BSS Pricing team -->

# DESIGN — Prices, Windows & Dimension (Slice 3)

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-design-slice-03`

<!-- toc -->

- [1. Context](#1-context)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Draft a temporary money change](#draft-a-temporary-money-change)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [normalize-and-select](#normalize-and-select)
  - [model-and-floor](#model-and-floor)
  - [reserve-write-confirm](#reserve-write-confirm)
- [4. States (CDSL)](#4-states-cdsl)
- [5. API Surface](#5-api-surface)
- [6. Data Model](#6-data-model)
- [7. Events & Alarms](#7-events--alarms)
- [8. Definitions of Done](#8-definitions-of-done)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Non-Functional Considerations](#10-non-functional-considerations)

<!-- /toc -->

## 1. Context

**Delivery:** phase 2c. Every checkbox is an implementation obligation, not an assertion about the legacy code.

Implement immutable money chains, models, temporary pairs and price floors; protect every entry reference with durable reservation recovery.

Requirements: `cpt-cf-bss-pricing-fr-price`, `cpt-cf-bss-pricing-fr-chain-windows`, `cpt-cf-bss-pricing-fr-pair-guard`, `cpt-cf-bss-pricing-fr-min-fee`, `cpt-cf-bss-pricing-fr-temporary-pair`, `cpt-cf-bss-pricing-fr-reference-protocol`. Architecture: `cpt-cf-bss-pricing-component-prices`, `cpt-cf-bss-pricing-component-reservations-client`, `cpt-cf-bss-pricing-principle-reserve-before-write`, `cpt-cf-bss-pricing-principle-book-money-independent`, `cpt-cf-bss-pricing-constraint-two-backends`, `cpt-cf-bss-pricing-constraint-no-row-locks`, `cpt-cf-bss-pricing-seq-reserve-write-confirm`, `cpt-cf-bss-pricing-seq-temporary-pair`.
[FEATURE](../features/prices-windows-dimension.md) owns the executable flow/algorithm/DoD identifiers; this slice defines no duplicate DoDs.
Dependencies: `cpt-cf-bss-pricing-feature-books-entries`.
Source: PriceBook spec §2.2, §5–§8, §12–§13 and [DECISIONS](../DECISIONS.md) D-384–D-443.

D-503 refuses CalendarHour with any `min_fee` at price create, submit and apply
(`UNSUPPORTED_TERMS`), and when publishing a revision selecting such approved money. D-504's
predicate also refuses a resource-scoped floor at those gates and at sale validation (amended
2026-10-02). A successor,
temporary pair and return keep their entry and therefore the same policy, window, scope and reset.
Policy changes require a different entry and an explicitly selected revision. The existing dated
SKU chain guard uses immutable Products history captured before the transaction.

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

### Draft a temporary money change

Actors: `cpt-cf-bss-pricing-actor-finance-manager`, `cpt-cf-bss-pricing-actor-products`. Feature flow: `cpt-cf-bss-pricing-flow-prices-windows-dimension`.

1. [ ] - `p1` - Finance Manager chooses an entry, dimension value, the money in the entry's model (D-427), effective_from and optional temporary_until. - `inst-prices-windows-dimension-flow-1`
2. [ ] - `p1` - Read chain history and validate the money's shape against the entry's model, dates, dimension membership and usage structure. - `inst-prices-windows-dimension-flow-2`
3. [ ] - `p1` - For an existing chain, copy the return money selected at the end into a linked price; for a previously unowned value create only one closed price. - `inst-prices-windows-dimension-flow-3`
4. [ ] - `p1` - Persist draft prices atomically with author attribution and return their ETags; later edits require draft/unlocked state. - `inst-prices-windows-dimension-flow-4`
5. [ ] - `p1` - Submit the complete pair or single price through slice 05; common-date shifts preserve temporary duration, and a shift that would carry a temporary across another start of its chain is refused TEMPORARY_SPANS_A_CHANGE (D-406). - `inst-prices-windows-dimension-flow-5`

## 3. Processes / Business Logic (CDSL)

### normalize-and-select

Feature algorithm: `cpt-cf-bss-pricing-algo-prices-windows-dimension-normalize-and-select`.

1. [ ] - `p1` - Group approved prices by price_book_entry_id and nullable dim_value and sort by effective_from. - `inst-prices-windows-dimension-normalize-and-select-1`
2. [ ] - `p1` - Refuse same-start/overlap conflicts; set predecessor ends to successor starts independently per chain. - `inst-prices-windows-dimension-normalize-and-select-2`
3. [ ] - `p1` - Keep default tail open; retain an explicit value-tail end so selection can fall back afterward. - `inst-prices-windows-dimension-normalize-and-select-3`
4. [ ] - `p1` - At date/value select an in-force own price then default; report uncovered when neither applies. - `inst-prices-windows-dimension-normalize-and-select-4`

### model-and-floor

Feature algorithm: `cpt-cf-bss-pricing-algo-prices-windows-dimension-model-and-floor`.

1. [ ] - `p1` - Derive permitted models from charge kind for the entry's model, chosen at its create (D-427); validate each price's money against its entry's model, nonnegative prices and coherent model parameters. - `inst-prices-windows-dimension-model-and-floor-1`
2. [ ] - `p1` - Evaluate per_unit, graduated, volume and package with decimal arithmetic and half-open tier bands; recurring/one_time allow flat or per_unit. - `inst-prices-windows-dimension-model-and-floor-2`
3. [ ] - `p1` - Preserve usage package size and SKU (unit, usage_type_ref) read as of each price's start across successors, a start before the SKU's first version reading that first version (D-402); CHAIN_MODEL_CHANGED fails submit and is rechecked at apply. The model cannot change on a chain: it is the entry's (D-427). - `inst-prices-windows-dimension-model-and-floor-3`
4. [ ] - `p1` - Aggregate rated amounts by price/subscription/period across every bound value and slice (no plan carries an included quantity since D-467); apply the prorated price floor, then promotions; not built in pricing, Rating applies the floor and pricing stores and validates min_fee (D-415). - `inst-prices-windows-dimension-model-and-floor-4`

### reserve-write-confirm

Feature algorithm: `cpt-cf-bss-pricing-algo-prices-windows-dimension-reserve-write-confirm`.

1. [ ] - `p1` - Replay first; Tx A claims the key, mints price_book_entry_id and inserts a create op in reserving before reserve. - `inst-prices-windows-dimension-reserve-write-confirm-1`
2. [ ] - `p1` - Reserve idempotently per (owner, kind, ref_id), then re-read SKU type/lifecycle; a refusal moves the op to cancelling. - `inst-prices-windows-dimension-reserve-write-confirm-2`
3. [ ] - `p1` - Tx B judges the create input's period and model against the SKU type the reservation froze (a refusal is a 400 receipt, D-427), then commits the entry, reservation_id, reference_state = confirmation_pending and op written together. - `inst-prices-windows-dimension-reserve-write-confirm-3`
4. [ ] - `p1` - Confirm after commit; Tx C sets entry confirmed, op done and answers the key. Retry transient failure with bounded backoff; never release on timeout. - `inst-prices-windows-dimension-reserve-write-confirm-4`
5. [ ] - `p1` - On REFERENCE_RELEASED during confirm, or a 404 for a reservation Products does not know, keep the entry confirmation_pending and start a rereserve op; a release answered 404 counts as released. Reconcile confirmed entries through states(): re-reserve when not fenced, else mark lost, audit, enqueue PriceBookEntryReferenceLost and refuse new prices with ENTRY_REFERENCE_LOST; re-reserve lost entries once their SKU admits a reservation. - `inst-prices-windows-dimension-reserve-write-confirm-5`
6. [ ] - `p1` - Cancellation persists op cancelling before release; deletion removes the entry and inserts a delete op releasing in one transaction. Release finishes the op; every op not done survives restart and is never dropped. - `inst-prices-windows-dimension-reserve-write-confirm-6`

## 4. States (CDSL)

Prices move draft → pending → approved through the unit engine; pending ownership prohibits edits. Rejected proposals retain their review history and cannot become approved by direct PATCH; replacement proposals use drafts. Withdrawal unlocks the proposal for authoring. An approved price that has not started moves approved → cancelled, a terminal state, when a unit applies a `cancel` row that names it (D-520); a `cancel` or `end` row moves draft → pending → approved like a price and stays the record of the change (D-520, D-521). Entry reference_state moves confirmation_pending → confirmed, or lost on proven release. Durable ops move reserving → written → done for creation, cancelling → done for refusal, or releasing → done for deletion; a rereserve op recovers a released receipt when the SKU is not fenced (D-401).

State definition: `cpt-cf-bss-pricing-state-prices-windows-dimension` in the FEATURE.

## 5. API Surface

POST /bss-pricing/v1/price-book-entries/{id}/prices; PATCH/DELETE /bss-pricing/v1/prices/{id} draft only (409 PRICE_NOT_DRAFT for a pending, approved or rejected price, and for the PATCH of a cancel or end row), by its author only (403 NOT_DRAFT_AUTHOR, D-404), at the current version (409 STALE_REVISION). POST /bss-pricing/v1/prices/{id}/cancel (no body) and POST /bss-pricing/v1/prices/{id}/end ({ effective_to }) open a draft `cancel` or `end` row of an approved price under the price author grant and an Idempotency-Key, answered 201 with the row; their guards (409 PRICE_NOT_SCHEDULED, PRICE_ALREADY_STARTED at apply, PRICE_CHANGE_PENDING, PRICE_BOUND when a consumer's binding names the price (an acceptance whose bindings name it; `keep_for_bound` alone never refuses), PRICE_ALREADY_ENDED; 400 END_DATE_INVALID) run at the door, at submit and again at apply (D-520, D-521). A decimal sent as a JSON number is 400 AMOUNT_INVALID. Neither the create nor the PATCH carries model: a price's money is in its entry's model, a shape that does not match it is 400 PRICE_MISSING, and every price read carries the entry's model, read-only (D-427). The PATCH of the temporary half of a draft takes effective_from and temporary_until and runs the pair builder again over the new dates in the same transaction (D-443): a pair stays a pair, its return derived again in place; the return is deleted when the chain's next approved price starts on the new end (the promo alone) or nothing of the chain is in force there (one closed price), the promo written first; a return is created, numbered after every price of the entry and written before the promo names it. Both halves are judged as the create judges them, and the answer is the edited price with its paired_price_id now. TEMPORARY_PRICE_FIXED stays for a return's own dates, the dim_value of either half, temporary_until on any other price and a null temporary_until. Submit and publish-changes enter slice 05. Entry create/delete use slice 02 doors but this slice owns their reference protocol, and GET /bss-pricing/v1/reference-ops?state&limit&cursor (config settings permission) lists that durable reference work for operators, in op-id order and paged by an exclusive cursor. Products calls are reserve, SKU read, confirm and release through ProductsClient.

[DESIGN §3.3](../DESIGN.md#33-api-contracts) fixes canonical errors and route prefixes.
Each mounted route must appear in all four censuses with authz and precondition expectations.

## 6. Data Model

pricing_price holds version_no, nullable dim_value, price_json (in its entry's model: m20260926_000013 dropped the price's own model column, D-427), min_fee, eligibility, dates, keep_for_bound, pair links, state, author and approval ownership. From m20261003_000022 it also holds change_kind (`set`, `cancel` or `end`), target_price_id and cancelled_by_unit_id, and state takes `cancelled` (D-520, D-521); two CHECKs pair them: a change and only a change names a target price, and a cancelled price and only a cancelled price names the unit that cancelled it. Approved chain indexes are per entry/value/start; the approved-start index counts `set` rows only, since an applied change keeps its price's start. pricing_price_book_entry holds its model, reservation_id and reference_state; pricing_reference_op holds durable work before reserve, retry metadata and cancellation/deletion work without an entry FK. closed_explicitly preserves a temporary price's end through normalization. No price-level frozen descriptors or remote reference count exists.

Tenant-scoped parent validation is required even where foreign keys use entity ids. Never substitute a
cross-gear read for transactional local ownership/version guards. Approved money and historical pins survive.

## 7. Events & Alarms

PricesPublished is emitted only by successful prices apply, together with ApprovalUnitDecided; it lists every price whose window or state the apply changed, each with its `state`: the unit's prices, each predecessor whose end moved, a cancelled price (`cancelled`) and an ended price with its new end; never a cancel or end row (D-520, D-521). Proven lost receipts emit PriceBookEntryReferenceLost with tenant, entry, SKU and reservation identity. Pending confirmation/release depth and last failures remain observable; retry is bounded and restart-safe.

Audit and outbox inserts use the same mutation transaction; retry is lifecycle-managed and observes shutdown.

## 8. Definitions of Done

The sole definitions live in [features/prices-windows-dimension.md](../features/prices-windows-dimension.md):

- `cpt-cf-bss-pricing-dod-price-models` — Type-appropriate price models.
- `cpt-cf-bss-pricing-dod-tier-bands-half-open` — Half-open tier arithmetic.
- `cpt-cf-bss-pricing-dod-chain-windows` — Per-chain window normalization.
- `cpt-cf-bss-pricing-dod-dimension-fallback` — Default fallback after value tails.
- `cpt-cf-bss-pricing-dod-pair-guard` — Usage successor structure guard.
- `cpt-cf-bss-pricing-dod-min-fee-price-period` — Minimum fee aggregation grain.
- `cpt-cf-bss-pricing-dod-temporary-pair` — Existing-chain temporary pairs.
- `cpt-cf-bss-pricing-dod-temporary-value-fallback` — Temporary value without its own chain.
- `cpt-cf-bss-pricing-dod-price-pending-guard` — Draft and pending ownership guards.
- `cpt-cf-bss-pricing-dod-reference-protocol` — Reference reservation barrier.
- `cpt-cf-bss-pricing-dod-confirmation-retry` — Durable confirmation and release recovery.

## 9. Acceptance Criteria

1. PRD AC #4 / `cpt-cf-bss-pricing-dod-price-models`: Given usage and recurring entries, when prices in their entries' models are drafted then they persist; a usage entry created flat (MODEL_KIND_CHARGEKIND_MISMATCH), a price whose shape does not match its entry's model (PRICE_MISSING), negative prices and approved-money PATCH fail (D-427).
2. PRD AC #4 / `cpt-cf-bss-pricing-dod-tier-bands-half-open`: Given a boundary of 1000, when quantity equals 1000 then volume selects the next band; 999 remains in the prior band.
3. PRD AC #5 / `cpt-cf-bss-pricing-dod-chain-windows`: Given default and EU prices, when EU gains a successor then default stays unchanged; duplicate approved start, overlap and past start fail.
4. PRD AC #5 / `cpt-cf-bss-pricing-dod-dimension-fallback`: Given a closed EU tail and an open default, when its end date arrives then default applies; if both are absent selection reports uncovered.
5. PRD AC #6 / `cpt-cf-bss-pricing-dod-pair-guard`: Given a package predecessor, when only its amount changes then validation passes; a size, dated unit or meter change returns 400 CHAIN_MODEL_CHANGED; the model is the entry's and never changes on a chain (D-427).
6. PRD AC #7 / `cpt-cf-bss-pricing-dod-min-fee-price-period` (the floor is applied by Rating, D-415): Given two 10 charges bound to one price with floor 30 then the result is 30; two distinct floor-30 prices yield 60, not 30 or 120.
7. PRD AC #8 / `cpt-cf-bss-pricing-dod-temporary-pair`: Given an existing EU chain, when a five-day temporary change shifts by three days then both boundaries shift and the return stays EU; partial pair submission fails.
8. PRD AC #8 / `cpt-cf-bss-pricing-dod-temporary-value-fallback`: Given only a default chain, when a temporary EU override ends then EU follows the current default; no paired return price exists.
9. PRD AC #4 / `cpt-cf-bss-pricing-dod-price-pending-guard`: Given a pending price and its old ETag, when PATCH or DELETE runs then it is refused and unit content is unchanged; an unlocked current draft can be edited.
10. PRD AC #11 / `cpt-cf-bss-pricing-dod-reference-protocol`: Given Products and Pricing on real SQLite/Postgres storage, when reserve races retire then both cannot succeed; a live entry blocks retire until durable removal and release.
11. PRD AC #11 / `cpt-cf-bss-pricing-dod-confirmation-retry`: Given a committed entry and lost confirm response, when retry resumes then it confirms safely without release; a receipt released before its confirm is re-reserved (lost, with PriceBookEntryReferenceLost, only when the SKU is fenced, retiring or retired) and deletion release failure remains queued.

## 10. Non-Functional Considerations

All paths enforce tenant isolation, deny-by-default authz and append-only attribution. Mutations use conditional
versions, required POST replay and PATCH/PUT preconditions; PostgreSQL serializable chain changes and SQLite
writer serialization preserve the same invariants. A failed audit/outbox write cannot leave a committed act.
No timeout releases a live reference. Review and apply retain typed database failures for bounded retry.
Implementation gates cover both backends and route censuses; document gates cover toc, language and identifier ownership.
