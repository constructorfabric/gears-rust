<!-- CONFLUENCE_TITLE: [BSS]: Pricing — Books & Entries (Feature) -->
<!-- Related: ../DECOMPOSITION.md, ../DESIGN.md, ../PRD.md | Owners: BSS Pricing team -->

# Feature: Books & Entries

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-featstatus-books-entries-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-bss-pricing-feature-books-entries`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Author a book and entry](#author-a-book-and-entry)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [book-and-key](#book-and-key)
  - [dimension-registry](#dimension-registry)
  - [settings-and-export](#settings-and-export)
- [4. States (CDSL)](#4-states-cdsl)
  - [Books & Entries states](#books--entries-states)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Currency books and validity](#currency-books-and-validity)
  - [One entry per book key](#one-entry-per-book-key)
  - [Restricted entry metadata edits](#restricted-entry-metadata-edits)
  - [Tenant dimension registry](#tenant-dimension-registry)
  - [Versioned billing defaults](#versioned-billing-defaults)
  - [Read-only book export](#read-only-book-export)
  - [Creation uses the reference service](#creation-uses-the-reference-service)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

**Delivery:** phase 2c. Every checkbox is an implementation obligation, not an assertion about the legacy code.

This feature implements [slice 02](../design/02-books-entries.md).
[DECOMPOSITION](../DECOMPOSITION.md) records integration order; [DESIGN §3](../DESIGN.md#3-technical-architecture)
is the schema and transaction authority. Unchecked phase 3/4 work is not part of the phase 2 core gate.

### 1.2 Purpose

Author currency books and unique SKU entries, maintain dimensions/settings and export book facts; hand entry creation/removal to the reservation service.

Requirements: `cpt-cf-bss-pricing-fr-dimension-registry`, `cpt-cf-bss-pricing-fr-price-book`, `cpt-cf-bss-pricing-fr-entry-key`, `cpt-cf-bss-pricing-fr-book-export`, `cpt-cf-bss-pricing-fr-settings`.

Architecture: `cpt-cf-bss-pricing-component-books`, `cpt-cf-bss-pricing-component-reservations-client`, `cpt-cf-bss-pricing-principle-book-money-independent`, `cpt-cf-bss-pricing-principle-reserve-before-write`, `cpt-cf-bss-pricing-constraint-one-replay-store`, `cpt-cf-bss-pricing-seq-reserve-write-confirm`.

### 1.3 Actors

`cpt-cf-bss-pricing-actor-finance-manager`, `cpt-cf-bss-pricing-actor-products`, `cpt-cf-bss-pricing-actor-auditor`. Every operation authenticates and derives tenant scope before storage, replay or cross-gear calls.
Holding multiple permissions never bypasses separation of duties.

### 1.4 References

- [PRD](../PRD.md), especially the numbered acceptance criteria referenced below.
- [DESIGN](../DESIGN.md), §3 model, API contracts, transaction sequences and DDL.
- [Slice 02](../design/02-books-entries.md), including API, data and event obligations.
- [DECISIONS](../DECISIONS.md), D-384–D-444, D-522; spec means `docs/superpowers/specs/2026-09-24-pricebook-model-design.md` in the main checkout.
- Source: spec §2 decisions 4–8, 13–17, §2.2, §5–§8, §10, §12–§13; the phase 2 plan supplies delivery boundaries and D-399/D-400.

D-502 binds an immutable UsageRatingPolicy to each new usage entry. The create requires
`usage_rating_policy` for usage (`MISSING_RATING_POLICY` otherwise) and refuses it for recurring
or one-time entries (`UNEXPECTED_RATING_POLICY`). The closed input contains rating_window
(BillingCycle or CalendarHour with UTC), aggregation_scope (subscription_line or resource),
reset (rating_window_start), quantity_semantics (meter usage_type_id/version, unit, SUM fold,
accrual_policy_version), and partial_window (actual_quantity_full_thresholds). On author input,
`quantity_semantics.fold`, `reset` and `partial_window` may be absent or null; the server fills `SUM`,
`rating_window_start` and `actual_quantity_full_thresholds` before validation, the content digest and
the meter check (D-513). An explicit value is accepted and an unknown value is refused.
D-514 stores only the five rating rules. The meter, the unit and the accrual version are the SKU's.
A deploy-3 body may still send `quantity_semantics`; the server verifies it and drops it. The entry
stores `usage_sku_version`, the SKU head's `published_version` at create. Empty or whitespace-only
meter identifiers, versions, units or accrual versions in that deploy-3 object are `METER_POLICY_MISMATCH`. The server assigns
policy_id, version 1 and the lowercase SHA-256 canonical content digest; author input refuses these
identity fields. The entry PATCH cannot change or clear policy. Item and price requests refuse policy
fields. Changed content requires a new entry, then a revision explicitly selecting it.

Policy rows are append-only on both databases and deduplicate by (tenant_id, digest), checking stored
content on every reuse. Migration 18 adds the nullable entry reference (id, version, digest), an
all-null-or-all-present check, and a tenant-qualified composite foreign key including digest. The entry
key is (book_id, sku_id, charge_kind, coalesce(period, ''), model, coalesce(usage_policy_digest, ''));
only absent policy uses the empty index token. Hourly and billing-cycle variants coexist; equal content
cannot evade uniqueness through a new UUID. Entry reads, export, write answers and durable create
receipts materialize policy content with its identity; legacy/non-usage entries return null.

Tx A persists typed content (schema version 1 in D-502, version 2 with meter evidence in D-503) before the remote reserve. Tx B
inserts or reuses the policy and writes the entry atomically. A crash cannot change content; replay
returns the confirmed receipt. Unversioned persisted creates decode as legacy and may recover with
null policy; new versioned usage creates cannot take that path. Re-reserve and delete preserve the
original entry reference. Migration assigns no policy to old entries, including published plans;
they continue to read and resolve. D-503 adds meter verification, publication gates and resolve
policy projection. E1 = E1a (raw meters, the usage collector / types registry; external) + E1b (derived meters, provided by Products since P-D-233).

D-503 adds exact-version semantic validation to D-502. Pricing consumes
`pricing-sdk::meter_semantics::UsageMeterSemanticsV1::resolve(ctx, MeterRef)` as the authorized
caller, before opening a Pricing transaction. `MeterSemantics` carries the exact meter identity
and version, canonical unit, SUM fold, accrual-policy version, source-integrated flag and provider
evidence digest. All quantity fields and the SKU's unit and usage-type identity must agree;
otherwise `METER_POLICY_MISMATCH` refuses the write. There is no substitution of a latest version.

**Owner amendment of D-503, 2026-10-01.** A `MeterRef` names a raw or a derived meter, and one provider
behind the port answers both kinds. E1a, raw meters: Types Registry declarations through the Usage
Collector. E1b, derived meters: Products' derived usage type at its exact version (its canonical output
unit and the digest of its stored declaration, which names the inputs at their exact versions and the
formula; products P-D-229 and rating T-D-39). The port, `validate_meter_policy` and the publication and
acceptance gates do not change.

**Amended 2026-10-01 by products P-D-233: E1b is provided; E1a is still external.** Products registers the
one provider. It answers a derived meter from its own store, in the caller's tenant: the version's output
unit, SUM, `derived-v1:<stored digest hex>`, source integrated, and the stored digest. It answers every raw
meter exactly as an absent provider does (`UNCONFIGURED_DEPENDENCY`). A derived meter is sellable; a raw
one stays blocked at its semantic gates until E1a is delivered.
E1 = E1a (raw meters, the usage collector / types registry; external) + E1b (derived meters, provided by Products since P-D-233).

New entry-create work uses schema version 2 and persists the captured declaration before reservation.
Recovery validates that captured evidence against the reservation's SKU without another meter lookup.
Unversioned and version-1 work keep their original recovery rules; they acquire no invented evidence.
The existing D-401 cancellation of unreserved abandoned creates remains unchanged. A later fresh
request must resolve its own evidence. Confirmation recovery preserves the original entry and policy.

## 2. Actor Flows (CDSL)

### Author a book and entry

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-flow-books-entries`

1. [ ] - `p1` - Finance Manager creates a uniquely coded currency book and reads its ETag. - `inst-books-entries-flow-1`
2. [ ] - `p1` - Select a published non-bundle SKU, the entry's model, recurring period if applicable and optional registered dimension key. - `inst-books-entries-flow-2`
3. [ ] - `p1` - Resolve replay and pass the stable entry identity to the reserve-write-confirm protocol in slice 03. - `inst-books-entries-flow-3`
4. [ ] - `p1` - After reservation, re-read SKU type/lifecycle, derive charge kind, enforce key uniqueness and persist through that protocol. - `inst-books-entries-flow-4`
5. [ ] - `p1` - Return book/entry facts; later money is drafted as prices, not embedded in the key. - `inst-books-entries-flow-5`

## 3. Processes / Business Logic (CDSL)

### book-and-key

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-algo-books-entries-book-and-key`

1. [ ] - `p1` - Validate currency and nonempty validity interval; scope code uniqueness to tenant. - `inst-books-entries-book-and-key-1`
2. [ ] - `p1` - Derive charge_kind from the current SKU; recurring accepts month/year, usage and one_time require null period; the model must be one the charge kind allows (D-386, D-427). - `inst-books-entries-book-and-key-2`
3. [ ] - `p1` - Enforce the book/SKU/kind/coalesced-period/model/policy-digest unique index and map races to a conflict. - `inst-books-entries-book-and-key-3`
4. [ ] - `p1` - PATCH name/validity or permitted entry overrides conditionally; reject currency edits and dimension changes after valued prices exist. - `inst-books-entries-book-and-key-4`

### dimension-registry

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-algo-books-entries-dimension-registry`

1. [ ] - `p1` - Validate key syntax and distinct values (none yet, or at least two; exactly one is DIM_VALUES_FEW); a tenant with no stored registry reads the seed region with no values, and the first entry naming region stores it in that entry's transaction. - `inst-books-entries-dimension-registry-1`
2. [ ] - `p1` - Reject unknown price values or values without a declared entry dimension. - `inst-books-entries-dimension-registry-2`
3. [ ] - `p1` - Before removing a value, atomically check all tenant prices referencing it, including historical prices. - `inst-books-entries-dimension-registry-3`
4. [ ] - `p1` - Apply the direct versioned registry edit without an approval unit. - `inst-books-entries-dimension-registry-4`

### settings-and-export

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-algo-books-entries-settings-and-export`

1. [ ] - `p1` - Authorize settings separately from book authoring and enforce If-Match on settings changes. - `inst-books-entries-settings-and-export-1`
2. [ ] - `p1` - Resolve defaults by SKU billing-timing override then tenant timing; preserve rounding and per-type template inputs for binding. - `inst-books-entries-settings-and-export-2`
3. [ ] - `p1` - For export, authorize read and select only the scoped book, entries and prices; preserve price identities and chain windows. - `inst-books-entries-settings-and-export-3`
4. [ ] - `p1` - Return JSON without modifying state, submitting units or evaluating totals. - `inst-books-entries-settings-and-export-4`

## 4. States (CDSL)

### Books & Entries states

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-state-books-entries`

A book is valid or invalid for a queried date according to its optional interval; no approval state is added. An entry progresses through reference confirmation in slice 03. Currency and key identity, the entry's model included (D-427), are fixed; editable metadata uses versions. Registry/settings changes are direct and versioned.

## 5. Definitions of Done

Every DoD below is required for this feature's delivery phase. Constraints: `cpt-cf-bss-pricing-constraint-one-replay-store`.

### Currency books and validity

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-book-currency-validity`

Books have tenant-unique codes, one immutable currency and optional nonempty validity. Versioned metadata edits preserve money identity (spec §5, D-384). Every book read carries the book's stats from four grouped statements, one per source (D-441), and the book list pages on the toolkit's OData pager, searched by q and sku_id (D-442). The book list answers 304 when If-None-Match matches a weak ETag of its JSON, and sends Cache-Control: private, no-cache (D-518). A book carries an optional description of at most 2000 characters, and a book no entry and no plan revision names is deleted under If-Match with an audit row; otherwise 409 BOOK_HAS_ENTRIES, BOOK_IN_PLAN or BOOK_IN_PLAN_HISTORY, in that order (D-444). A finished book is archived under If-Match with an audit row: refused 409 BOOK_IN_PLAN while a revision that is not superseded names it, BOOK_HAS_PENDING while a prices unit of it is in review (a pending price, cancel or end), ENTRY_CONFIRMATION_PENDING while an entry's reference is being confirmed. In the same transaction each confirmed or lost entry becomes `released` with a `release` op (reason book_archived) that releases its SKU reference in Products after the commit, at most 8 at once and within 3 s for the whole door, the ticker finishing what the door does not; from then on the book's entries and prices are read-only (409 BOOK_ARCHIVED), and the book list hides it unless asked `archived eq true`. An unarchive is refused 409 ENTRY_RELEASE_PENDING, and writes nothing, while a release or a re-reservation of an entry of the book is still open. Otherwise it re-reserves each released entry, and an entry whose SKU refuses stays released and read-only (409 ENTRY_REFERENCE_RELEASED), listed in the answer's `released_entries`, which is null when that read fails after the unarchive committed (D-522).

Requirement: `cpt-cf-bss-pricing-fr-price-book`; PRD AC #2.

### One entry per book key

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-entry-key-unique`

The database enforces SKU × charge kind × normalized period × model × policy digest uniqueness inside a book (D-427, D-502). Charge kind follows the re-read SKU; a bundle, an invalid period or a model the charge kind does not allow is rejected (spec §5, D-386). GET /price-book-entries?sku_id= narrows, orders and pages one SKU's entries in memory (D-486), and $filter=id in (...) of at most 200 ids, and at most 8192 bytes, lists those entries instead (D-517): book_id (1 to 50), currency, q, status (priced, scheduled, unpriced) and changing; book_name or status, the id breaking a tie the same way; 500 per page by default and at most 500. The read makes seven statements for a SKU in 5 books and in 50. A SKU in more than 500 entries returns the first page.
Requirement: `cpt-cf-bss-pricing-fr-entry-key`; PRD AC #3.

### Restricted entry metadata edits

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-entry-metadata`

invoice_line_override remains editable under If-Match. dimension_key changes only before any valued price exists; approved or pending prices prevent entry deletion with ENTRY_PRICES_IN_USE and another author's draft with 403 NOT_DRAFT_AUTHOR (D-404), while the caller's draft and rejected prices are deleted with the entry (spec §7.2, phase 2c.6).

Requirement: `cpt-cf-bss-pricing-fr-entry-key`; PRD AC #3.

### Tenant dimension registry

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-dimension-registry`

Keys and values are tenant-owned and validated with the four DIM errors. The registry starts seeded with region, declared with no values; the first entry naming it stores the seed. A referenced value cannot be removed even when its price is historical, and registry edits require no approval (spec §5, §14). Every registry answer shows each value's use, the prices of any state that carry it, from one grouped count; PATCH /dimension-keys adds and removes one declared key's values, and a used value's refusal names it (D-436).

Requirement: `cpt-cf-bss-pricing-fr-dimension-registry`; PRD AC #1.

### Versioned billing defaults

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-dod-settings-defaults`

Tenant timing, rounding, GL, tax and per-type invoice templates are stored and exposed. SKU timing overrides tenant timing at binding; settings permissions and optimistic versioning apply (spec §5). The rounding is one of five modes, half_even for a tenant that never wrote its settings (D-437); the settings offer the currencies a new book may take and say who wrote them last and when (D-438). GET /settings keeps its version as the strong ETag a PUT sends back as If-Match; an If-None-Match that matches it is 304, and the read sends Cache-Control: private, no-cache (D-518). The read names its last writer (`updated_by_name`, D-519); the PUT accepts that field and ignores it, so a read without version, updated_at and updated_by stays a PUT body.

Requirement: `cpt-cf-bss-pricing-fr-settings`; PRD AC #13.

### Read-only book export

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-book-export`

Export returns the book and all its scoped entries/prices as JSON with ids, windows, dimensions and model inputs. It creates no approval or mutation (spec §2 decision 16). One entry's prices read alone, each with its status today, in the export's chain order, under price_book read on the entry's book as well as entry read; the entry reads carry the price in force and the approved prices by date (D-440), and the next price: the default chain's earliest scheduled price, else its newest draft or pending price (D-472). The book's entries list judges them all on its as_of, today by default, and an as_of other than today takes price_book read on the book (D-473). It pages on the toolkit's OData pager, 500 entries by default and at most 500, in the order (sku_id, charge_kind, model, id), with $filter over sku_id, charge_kind, model and reference_state; the cursor carries $filter and the day, the as_of refusal comes before any entry is read, and each page makes seven statements (D-483). A book of more than 500 entries returns the first page. Every price these reads, the SKU's entries, the export and the publish-changes listing show carries `created_by_name` beside `created_by`, resolved once per answer through Account Management (D-519).

Requirement: `cpt-cf-bss-pricing-fr-book-export`; PRD AC #12.

### Creation uses the reference service

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-entry-reference-handoff`

Every entry create and delete invokes slice 03 reference protocol rather than writing directly. Validation failures and duplicate-key races leave no unprotected object (spec §13).

Requirement: `cpt-cf-bss-pricing-fr-entry-key`; PRD AC #3.

## 6. Acceptance Criteria

| DoD | PRD criterion | Given / When / Then |
| --- | --- | --- |
| `cpt-cf-bss-pricing-dod-book-currency-validity` | AC #2; `cpt-cf-bss-pricing-fr-price-book` | Given a EUR book, when name/validity changes with its ETag then currency stays EUR; duplicate tenant code or inverted dates are refused. |
| `cpt-cf-bss-pricing-dod-entry-key-unique` | AC #3; `cpt-cf-bss-pricing-fr-entry-key` | Given the same nonrecurring SKU twice with one model, when concurrent creates use null period then only one entry persists; the same SKU with another model is a second entry of the book (D-427); a bundle has no entry. |
| `cpt-cf-bss-pricing-dod-entry-metadata` | AC #3; `cpt-cf-bss-pricing-fr-entry-key` | Given a valued price, when a dimension change is requested then it is refused DIMENSION_KEY_IN_USE; given an approved or pending price, entry deletion is refused ENTRY_PRICES_IN_USE, and NOT_DRAFT_AUTHOR (D-404) while another author's draft exists; the caller's drafts and all rejected prices are deleted with the entry; allowed metadata updates retain receipt identity. |
| `cpt-cf-bss-pricing-dod-dimension-registry` | AC #1; `cpt-cf-bss-pricing-fr-dimension-registry` | Given EU prices, when US is added then it is available; deleting EU or drafting UNKNOWN is refused without changing the registry. |
| `cpt-cf-bss-pricing-dod-settings-defaults` | AC #13; `cpt-cf-bss-pricing-fr-settings` | Given default arrears and SKU advance, when inputs bind then advance wins; stale settings update fails with no partial changes. |
| `cpt-cf-bss-pricing-dod-book-export` | AC #12; `cpt-cf-bss-pricing-fr-book-export` | Given two tenants, when one exports its book then only its facts appear; foreign-book access is denied and price/unit counts do not change. |
| `cpt-cf-bss-pricing-dod-entry-reference-handoff` | AC #3; `cpt-cf-bss-pricing-fr-entry-key` | Given Products unavailable before reserve, when an entry is created then REGISTRY_UNAVAILABLE leaves no entry; a successful create retains its live receipt. |

Verification uses domain tests, scoped repository tests on both backends and REST positive/denial/precondition probes as applicable. Phase 2 checks must not mark later-phase behavior implemented. Golden consumer contracts belong to phase 4.


**Final seam conformance (D-510).** Immutable UsageRatingPolicy remains owned by PriceBookEntry, with no item/price override. The
semantic key is book, SKU, charge kind, normalized period, model and canonical policy-content digest;
the policy reference is tenant-qualified `(tenant_id, policy_id, version, digest)`. Book remapping also
matches dimension key, and preserves an unmatched source selection for explicit repair. Changed policy
requires a new entry and revision. Creation verifies exact immutable meter evidence before reservation;
price/plan submit and final apply recheck it. Legacy entries remain policy-less/readable; new usage
creates require explicit policy. Shared backend tests prove upgrade preservation, content deduplication
and concurrent identical/different-policy creation. No policy column is added to items or prices.

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
