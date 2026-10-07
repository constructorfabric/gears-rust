<!-- CONFLUENCE_TITLE: [BSS]: Pricing — Books & Entries (Design, Slice 2) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../DECISIONS.md | Owners: BSS Pricing team -->

# DESIGN — Books & Entries (Slice 2)

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-design-slice-02`

<!-- toc -->

- [1. Context](#1-context)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Author a book and entry](#author-a-book-and-entry)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [book-and-key](#book-and-key)
  - [dimension-registry](#dimension-registry)
  - [settings-and-export](#settings-and-export)
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

Author currency books and unique SKU entries, maintain dimensions/settings and export book facts; hand entry creation/removal to the reservation service.

Requirements: `cpt-cf-bss-pricing-fr-dimension-registry`, `cpt-cf-bss-pricing-fr-price-book`, `cpt-cf-bss-pricing-fr-entry-key`, `cpt-cf-bss-pricing-fr-book-export`, `cpt-cf-bss-pricing-fr-settings`. Architecture: `cpt-cf-bss-pricing-component-books`, `cpt-cf-bss-pricing-component-reservations-client`, `cpt-cf-bss-pricing-principle-book-money-independent`, `cpt-cf-bss-pricing-principle-reserve-before-write`, `cpt-cf-bss-pricing-constraint-one-replay-store`, `cpt-cf-bss-pricing-seq-reserve-write-confirm`.
[FEATURE](../features/books-entries.md) owns the executable flow/algorithm/DoD identifiers; this slice defines no duplicate DoDs.
Dependencies: `cpt-cf-bss-pricing-feature-foundation`.
Source: PriceBook spec §2.2, §5–§8, §12–§13 and [DECISIONS](../DECISIONS.md) D-384–D-444.

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

Actors: `cpt-cf-bss-pricing-actor-finance-manager`, `cpt-cf-bss-pricing-actor-products`, `cpt-cf-bss-pricing-actor-auditor`. Feature flow: `cpt-cf-bss-pricing-flow-books-entries`.

1. [ ] - `p1` - Finance Manager creates a uniquely coded currency book and reads its ETag. - `inst-books-entries-flow-1`
2. [ ] - `p1` - Select a published non-bundle SKU, the entry's model, recurring period if applicable and optional registered dimension key. - `inst-books-entries-flow-2`
3. [ ] - `p1` - Resolve replay and pass the stable entry identity to the reserve-write-confirm protocol in slice 03. - `inst-books-entries-flow-3`
4. [ ] - `p1` - After reservation, re-read SKU type/lifecycle, derive charge kind, enforce key uniqueness and persist through that protocol. - `inst-books-entries-flow-4`
5. [ ] - `p1` - Return book/entry facts; later money is drafted as prices, not embedded in the key. - `inst-books-entries-flow-5`

## 3. Processes / Business Logic (CDSL)

### book-and-key

Feature algorithm: `cpt-cf-bss-pricing-algo-books-entries-book-and-key`.

1. [ ] - `p1` - Validate currency and nonempty validity interval; scope code uniqueness to tenant. - `inst-books-entries-book-and-key-1`
2. [ ] - `p1` - Derive charge_kind from the current SKU; recurring accepts month/year, usage and one_time require null period; the model must be one the charge kind allows (D-386, D-427). - `inst-books-entries-book-and-key-2`
3. [ ] - `p1` - Enforce the book/SKU/kind/coalesced-period/model/policy-digest unique index and map races to a conflict. - `inst-books-entries-book-and-key-3`
4. [ ] - `p1` - PATCH name/validity or permitted entry overrides conditionally; reject currency edits and dimension changes after valued prices exist. - `inst-books-entries-book-and-key-4`

### dimension-registry

Feature algorithm: `cpt-cf-bss-pricing-algo-books-entries-dimension-registry`.

1. [ ] - `p1` - Validate key syntax and distinct values (none yet, or at least two; exactly one is DIM_VALUES_FEW); a tenant with no stored registry reads the seed region with no values, and the first entry naming region stores it in that entry's transaction. - `inst-books-entries-dimension-registry-1`
2. [ ] - `p1` - Reject unknown price values or values without a declared entry dimension. - `inst-books-entries-dimension-registry-2`
3. [ ] - `p1` - Before removing a value, atomically check all tenant prices referencing it, including historical prices. - `inst-books-entries-dimension-registry-3`
4. [ ] - `p1` - Apply the direct versioned registry edit without an approval unit. - `inst-books-entries-dimension-registry-4`

### settings-and-export

Feature algorithm: `cpt-cf-bss-pricing-algo-books-entries-settings-and-export`.

1. [ ] - `p1` - Authorize settings separately from book authoring and enforce If-Match on settings changes. - `inst-books-entries-settings-and-export-1`
2. [ ] - `p1` - Resolve defaults by SKU billing-timing override then tenant timing; preserve rounding and per-type template inputs for binding. - `inst-books-entries-settings-and-export-2`
3. [ ] - `p1` - For export, authorize read and select only the scoped book, entries and prices; preserve price identities and chain windows. - `inst-books-entries-settings-and-export-3`
4. [ ] - `p1` - Return JSON without modifying state, submitting units or evaluating totals. - `inst-books-entries-settings-and-export-4`

## 4. States (CDSL)

A book is valid or invalid for a queried date according to its optional interval; no approval state is added. An entry progresses through reference confirmation in slice 03. Currency and key identity, the entry's model included (D-427), are fixed; editable metadata uses versions. Registry/settings changes are direct and versioned.

State definition: `cpt-cf-bss-pricing-state-books-entries` in the FEATURE.

## 5. API Surface

Under /bss-pricing/v1: POST/GET /price-books; GET/PATCH/DELETE /price-books/{id} (D-444); GET /price-books/{id}/entries; GET /price-books/{id}/export; POST /price-books/{id}/entries; GET/PATCH/DELETE /price-book-entries/{id}; GET /price-book-entries/{id}/prices (D-440); GET /price-book-entries?sku_id= (D-434); GET/PUT /settings; GET/PUT/PATCH /dimension-keys (D-436). Entry deletion refuses approved or pending prices with 409 ENTRY_PRICES_IN_USE and another author's draft with 403 NOT_DRAFT_AUTHOR (D-404), and, from phase 3, an entry a plan item names in a revision of any state with 409 ENTRY_IN_USE (D-408); the caller's draft and all rejected prices are deleted with the entry (a rejected price's review history stays in its unit snapshot), and DELETE answers 204 once the removal commits while the release completes as durable reference work. POST requires Idempotency-Key; PATCH/PUT require If-Match, and a stale token is 409 STALE_REVISION. POST /price-books/{id}/entries requires model, fixed for the entry's life (D-427): 400 MODEL_INVALID for a string that is not flat, per_unit, graduated, volume or package, and 400 MODEL_KIND_CHARGEKIND_MISMATCH for a model the charge kind does not allow, judged at the door and again after the reservation (a 400 receipt that releases it); 409 ENTRY_KEY_TAKEN for a (SKU, charge kind, normalized period, model, policy digest) the book already has. The PATCH does not carry model. A new entry on a SKU that is fenced, retiring or retired, deprecated or still draft is 409 SKU_FENCED (Products' own refusal), SKU_RETIRING, SKU_DEPRECATED or SKU_DRAFT; a bundle SKU is 409 BUNDLE_SKU_NOT_PRICEABLE. A dimension_key change after a valued price, or removing a registry key an entry names, is 409 DIMENSION_KEY_IN_USE. A change of invoice_line_override once the entry has an approved or pending price is 409 INVOICE_LINE_LOCKED (D-426): the override reaches consumers through resolve, so another invoice line needs another entry. Permissions are read, author and settings as appropriate.

The two entry reads, GET /price-book-entries/{id} and GET /price-books/{id}/entries, carry each entry's usage (D-428): usage = { prices: { approved, pending, draft, scheduled, active, superseded }, plans, plans_superseded_only }. prices counts the entry's prices by state, and a rejected price is not counted; the approved ones are also counted by where their window stands today, so approved = scheduled + active + superseded, from the same grouped count (D-440); plans counts the distinct plans with a draft, pending, scheduled or published revision whose items name the entry, from the stored state (D-453); plans_superseded_only counts the distinct plans that name it only through superseded revisions, which still keep it ENTRY_IN_USE. An entry in any reference state counts. The counts are read tenant-scoped under price_book_entry read, with a fixed number of set-based reads per request, whatever the number of entries. The POST and PATCH answers, the stored receipt, the export and the publish-changes listing carry no usage. The same counts, added up per SKU with plans distinct across the SKU's entries, fill Products' SkuUsageV1 port, which pricing registers at init (P-D-197); its usage_sets answers the SKUs whose entries and plans counts are above zero, as two sets read set-based, for the Products list's filters (P-D-212).

Where a SKU is priced (D-434, D-486): GET /price-book-entries?sku_id= lists the tenant's entries of one SKU across its books, read under the caller's entry scope, then narrowed, ordered and paged in memory. The plain keys are book_id (1 to 50 distinct ids), currency, q, status (priced, scheduled, unpriced) and changing. The order is book_name (the default) or status, and the id breaks a tie in that direction. A page is 500 entries by default and at most 500. Each item carries its book's code, name and currency, its usage, status and changing on today, current_price: the default chain's approved price in force today, or null, and next_price (D-472). The money is shown only to a caller who also holds price_book read (the export's grant); a caller without that grant still sees the entries, with current_price and next_price null. status and changing are not money. sku_id is required (400 QUERY_INVALID), unless `$filter=id in (…)` names at most 200 entry ids instead (D-517). That filter is declared as a plain query parameter naming its two shapes, `id eq` and `id in`, and one longer than 8192 bytes (the toolkit's `MAX_FILTER_LEN`) is 400 QUERY_INVALID before it is parsed. The read makes seven statements whatever the number of entries, for a SKU in 5 books and in 50.

An entry's prices (D-440): GET /price-book-entries/{id}/prices lists every price of the entry in every state, each with its display status today, the default chain first and then each dimension value's chain in ascending order, each chain by effective_from and then version_no; status= keeps one status or several, comma-separated (400 QUERY_INVALID for any other). The prices are money: price_book_entry read reaches the entry (404 ENTRY_NOT_FOUND before the money is judged), and price_book read on its book, judged a second time, shows them (403 PRICE_BOOK_READ_REQUIRED; 503 when the policy cannot judge). Three statements, whatever the number of prices. The two entry reads carry current_price, the default chain's approved price in force today, chosen and shown as D-434 does: null without price_book read on the entry's book. Beside it they carry next_price (D-472), shown by the same rule: the default chain's earliest approved price that starts after today, else its newest draft or pending price (the highest version_no, then the latest created_at), else null. A rejected price and a dimension value's price are never the next price. Both come from one read of the default chain's approved, pending and draft prices, so no read gains a statement: the book's entry list makes seven, the SKU's entry list seven. The book's entry list takes as_of, a YYYY-MM-DD date (D-473): its prices in force, next prices, statuses and usage split are all judged on that one day, today by default. A day other than today is money: without price_book read on the book it is 403 PRICE_BOOK_READ_REQUIRED. A day before the book's valid_from, or on or after its valid_until, still answers with the prices in force on it, which are not sellable on that day. A malformed date is 400 DATE_INVALID, after the 503 of the money's policy and before the 404 of the book. The single read and the SKU's entry list take no as_of. The book's entry list pages on the toolkit's OData pager (D-483): limit is 500 by default and at most 500, cursor comes from page_info, and $filter takes sku_id, charge_kind, model and reference_state. The order is (sku_id, charge_kind, model, id), so month and year entries of one model follow their id; the export keeps (sku_id, charge_kind, period, id). The cursor hashes $filter and the day: another filter or another as_of is 400 FILTER_MISMATCH. Any other plain key, or one given twice, is 400 QUERY_INVALID; $orderby, $select and $count are 400. PRICE_BOOK_READ_REQUIRED is judged before any entry is read, and a page makes seven statements whatever its size. A SKU search goes to GET /bss-products/v1/skus?q=, then narrows the list with $filter=sku_id in (...).

A book carries an optional description (D-444): POST and PATCH take it, at most 2000 characters (400 BOOK_DESCRIPTION_TOO_LONG); the PATCH keeps it when the field is omitted and clears it with null; every book answer carries it. DELETE /price-books/{id}, under the book write grant and If-Match, removes a book nothing uses: 204 and a price_book.delete audit row. After 403, 400 for the If-Match, 404 and 409 STALE_REVISION, it refuses 409 BOOK_HAS_ENTRIES for an entry of any reference state, 409 BOOK_IN_PLAN for a plan with a draft, pending, scheduled or published revision on the book (D-453), and 409 BOOK_IN_PLAN_HISTORY when only superseded revisions name it, whose history keeps the book; both from the read stats.plans and stats.plans_superseded_only count, so the delete succeeds exactly when stats.entries, stats.plans and stats.plans_superseded_only are 0. No unit can be pending on a book without entries, so a pending unit has no refusal of its own. A row that a concurrent writer adds after these reads meets the book's foreign key and is the same 409. Decided units that named the book stay readable without it, and the create's key still replays its 201 for a day.

The book reads (D-441, D-442): GET /price-books and GET /price-books/{id} carry each book's stats: its entries and their distinct SKUs, the distinct plans with a non-superseded revision on the book and those that name it only through superseded revisions (plans and plans_superseded_only, the read the book delete's BOOK_IN_PLAN and BOOK_IN_PLAN_HISTORY judge by), its prices by state with the approved ones by date and the rejected ones counted, its prices units in review, and last_change_at, the latest instant of the book, its entries, their prices and its units' submissions and decisions. Four grouped statements, one per source, whatever the number of books. The write answers, the export and publish-changes keep the book alone. GET /price-books pages on the toolkit's OData pager, as the Products SKU list does: $filter over id, code, name, currency, valid_from and valid_until (id eq and id in already answer, D-516) (the dates nullable), $orderby code or name (tie-break id), $top 200 by default and clamped at 500, the cursor from page_info; q is a case-insensitive substring of the code or the name (ICU folding on Postgres, ASCII on SQLite) and sku_id keeps the books with an entry of that SKU, both inside the cursor's filter hash (400 FILTER_MISMATCH otherwise); any other plain key is 400 QUERY_INVALID. The answer is a page, { items, page_info }, with no total.

Dimension values (D-436): every registry answer (GET, PUT and PATCH /dimension-keys) is PricingDimensionRegistry, each value with usage { prices }: the prices of any state whose entry names the key and whose chain is the value, from one grouped count. PATCH /dimension-keys { key, add, remove } edits the values of one declared key under If-Match; keys are added and removed by the PUT. Removing a value a price uses is 409 DIM_VALUE_IN_USE naming it, at the PATCH and at the PUT, and both judge from the same grouped count.

Settings (D-437, D-438): default_rounding is half_up, half_even, half_down, up or down (400 ROUNDING_INVALID), and half_even, the ledger's platform default, for a tenant that never wrote its settings; PUT /settings requires currencies, the codes a new book may take ([] offers any; 400 CURRENCY_INVALID for a malformed or repeated code); a new book outside a non-empty list is 409 CURRENCY_NOT_OFFERED, and existing books are untouched. The settings answer carries updated_at and updated_by, null before the first write; updated_by is also null on a row written before m20260927_000014, and each PUT stamps both.

[DESIGN §3.3](../DESIGN.md#33-api-contracts) fixes canonical errors and route prefixes.
Each mounted route must appear in all four censuses with authz and precondition expectations.

## 6. Data Model

pricing_price_book stores code, name, currency, validity, an optional description (m20260928_000015, D-444) and version. pricing_price_book_entry uses a normalized nullable period and its model in its unique book/SKU/kind/period/model/policy-digest key and carries model (NOT NULL, CHECKed, fixed for its life; m20260926_000013, D-427), dimension_key, invoice_line_override and its reference receipt. pricing_dimension_key stores tenant/key/allowed values; pricing_settings stores defaults, invoice-line templates by SKU type, the offered currencies and the last writer (currencies and updated_by, m20260927_000014, D-438). Entry authoring delegates reference work to slice 03.

Tenant-scoped parent validation is required even where foreign keys use entity ids. Never substitute a
cross-gear read for transactional local ownership/version guards. Approved money and historical pins survive.

## 7. Events & Alarms

Book/registry/settings edits do not invent new publish events. Required audit and replay persist with direct acts. Entry reference failures and lost receipts use slice 03 recovery; approved price events are defined by slices 05 and 07.

Audit and outbox inserts use the same mutation transaction; retry is lifecycle-managed and observes shutdown.

## 8. Definitions of Done

The sole definitions live in [features/books-entries.md](../features/books-entries.md):

- `cpt-cf-bss-pricing-dod-book-currency-validity` — Currency books and validity.
- `cpt-cf-bss-pricing-dod-entry-key-unique` — One entry per book key.
- `cpt-cf-bss-pricing-dod-entry-metadata` — Restricted entry metadata edits.
- `cpt-cf-bss-pricing-dod-dimension-registry` — Tenant dimension registry.
- `cpt-cf-bss-pricing-dod-settings-defaults` — Versioned billing defaults.
- `cpt-cf-bss-pricing-dod-book-export` — Read-only book export.
- `cpt-cf-bss-pricing-dod-entry-reference-handoff` — Creation uses the reference service.

## 9. Acceptance Criteria

1. PRD AC #2 / `cpt-cf-bss-pricing-dod-book-currency-validity`: Given a EUR book, when name/validity changes with its ETag then currency stays EUR; duplicate tenant code or inverted dates are refused.
2. PRD AC #3 / `cpt-cf-bss-pricing-dod-entry-key-unique`: Given the same nonrecurring SKU twice with one model and equal policy content (or absent policy), when concurrent creates use null period then only one entry persists; the same SKU with another model or usage-policy digest is another entry of the book (D-427, D-502); a bundle has no entry.
3. PRD AC #3 / `cpt-cf-bss-pricing-dod-entry-metadata`: Given a valued price, when a dimension change is requested then it is refused DIMENSION_KEY_IN_USE; given an approved or pending price, entry deletion is refused ENTRY_PRICES_IN_USE, and NOT_DRAFT_AUTHOR (D-404) while another author's draft exists; the caller's drafts and all rejected prices are deleted with the entry; allowed metadata updates retain receipt identity.
4. PRD AC #1 / `cpt-cf-bss-pricing-dod-dimension-registry`: Given EU prices, when US is added then it is available; deleting EU or drafting UNKNOWN is refused without changing the registry.
5. PRD AC #13 / `cpt-cf-bss-pricing-dod-settings-defaults`: Given default arrears and SKU advance, when inputs bind then advance wins; stale settings update fails with no partial changes.
6. PRD AC #12 / `cpt-cf-bss-pricing-dod-book-export`: Given two tenants, when one exports its book then only its facts appear; foreign-book access is denied and price/unit counts do not change.
7. PRD AC #3 / `cpt-cf-bss-pricing-dod-entry-reference-handoff`: Given Products unavailable before reserve, when an entry is created then REGISTRY_UNAVAILABLE leaves no entry; a successful create retains its live receipt.

## 10. Non-Functional Considerations

All paths enforce tenant isolation, deny-by-default authz and append-only attribution. Mutations use conditional
versions, required POST replay and PATCH/PUT preconditions; PostgreSQL serializable chain changes and SQLite
writer serialization preserve the same invariants. A failed audit/outbox write cannot leave a committed act.
No timeout releases a live reference. Review and apply retain typed database failures for bounded retry.
Implementation gates cover both backends and route censuses; document gates cover toc, language and identifier ownership.
