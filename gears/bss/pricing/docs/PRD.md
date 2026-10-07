<!-- CONFLUENCE_TITLE: [BSS]: Pricing — PriceBook (PRD) -->
<!-- Related: ./DESIGN.md, ./DECISIONS.md, ./ADR/ | Owners: BSS Pricing team -->

# PRD — Pricing: PriceBook

- [ ] `p1` - **PRD implementation status**

**Supersedes:** charge-line/market-price documents on `bss/products-backup` (`3a38f0b28`).
**Source:** `docs/superpowers/specs/2026-09-24-pricebook-model-design.md` in the main checkout, called “spec” below.
**Delivery:** this document defines the full programme; phases 3/4 remain implementation obligations.

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
  - [3.1 Module-Specific Environment Constraints](#31-module-specific-environment-constraints)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

Pricing owns per-currency books, SKU entries, dated dimension chains and their approval units. Later phases add
plans, promotions, migration requests and the consumer read contract. Products owns SKU identity and history.

### 1.2 Background / Problem Statement

The former eight-axis charge-line key and two-axis market-price key coupled structure, money and publication.
Operators need one book-wide money timeline independent of plan revision approval, with reproducible bindings
and a reference barrier that cannot race SKU retirement (spec §1, §2 decisions 4–8, 13, 16–17).

### 1.3 Goals (Business Outcomes)

- Author one SKU × charge kind × period entry in each currency book.
- Publish clear, reviewable batches and temporary changes without changing unrelated chains.
- Preserve historical billing inputs through price and SKU-version pins.
- Keep references safe while either gear fails and preserve review accountability.

### 1.4 Glossary

| Term | Meaning |
| --- | --- |
| Book | Tenant commercial schedule in one currency, optionally date-bounded. |
| Entry | SKU × charge kind × period × model identity within a book; the model is fixed for the entry's life (D-427). |
| Price | Immutable approved money in its entry's model, eligibility and window on one entry chain. |
| Dimension | One registered key on an entry; a nullable price value selects a chain. |
| Default chain | Prices whose dim_value is null; fallback for an uncovered value. |
| Approval unit | Proposed content, snapshot, generation, quorum and decisions. |
| Binding / pin | Consumer-held money price and versioned descriptors for a period. |
| Eligibility | all reprices renewals; new stops a renewal walking the chain. |
| Reservation | Products-owned reference receipt protecting SKU lifecycle and type. |
| Migration | Approved request to change subscription structure or pins, executed by Subscriptions. |

## 2. Actors

### 2.1 Human Actors

#### Finance Manager

**ID**: `cpt-cf-bss-pricing-actor-finance-manager`

Authors books, entries and price batches; sets effective dates and requests approval.

#### Finance Reviewer

**ID**: `cpt-cf-bss-pricing-actor-finance-reviewer`

Reviews snapshots and live impact; votes independently of submitter and item authors.

#### Product Manager

**ID**: `cpt-cf-bss-pricing-actor-product-manager`

Authors plan revisions, promotions, clone/retire and migration proposals in phase 3; promotions (D-409), retirement
and migration proposals (D-410) are deferred by the owner.

#### Auditor

**ID**: `cpt-cf-bss-pricing-actor-auditor`

Reads scoped history, snapshots, decisions and immutable audit facts.

### 2.2 System Actors

#### Products

**ID**: `cpt-cf-bss-pricing-actor-products`

Supplies current SKUs, versions in force and the reserve/confirm/release registry.

#### Rating

**ID**: `cpt-cf-bss-pricing-actor-rating`

Consumes price resolution and durable pinned prices in its own adaptation plan.

#### Subscriptions

**ID**: `cpt-cf-bss-pricing-actor-subscriptions`

Owns period pins and migration execution; consumes the new read contract and requests.

## 3. Operational Concept & Environment

### 3.1 Module-Specific Environment Constraints

The existing pricing gear and SDK names, toolkit REST/authz, SecureORM, SQLite and Postgres remain. The new
migration chain starts from zero; deployed-data conversion is outside the programme. Pricing reads ProductsClient
synchronously on writes; phase 2 deliberately has no SkuChanged listener or local SKU read model. Phase 4 binds
versioned descriptors at resolve time. Toolkit outbox and broker TypedEvent provide durable event delivery.

## 4. Scope

### 4.1 In Scope

Phase 2: dimension registry, books, entries, prices, windows, minimum fees, temporary pairs, batch publication,
prices approval, reference reservations, export, settings and core events. Phase 3: plans, revisions and items;
the owner defers promotions (D-409), migration requests and retirement (D-410), and the sold-as bundle and grants
(D-411). Phase 4: resolve, durable price reads and golden
consumer contracts; quote is not built (D-415).

### 4.2 Out of Scope

**Dropped, spec §3 A–C item numbers:** 1 phases/trials; 2 overlays; 4 region market axis; 5 brand axis now;
6 cohort (eligibility stays as a price flag); 8 PlanTier; 9 net/gross market display; 10 plan minimum/cap
(minimum moves to prices, cap is dropped); 11 derived meters/level aggregation (derived meters: see E1b
under `fr-entry-key`); 13 bundle-of-plans;
14 structural schedules; 15 materiality; 18 CatalogVersion; 29 bulk import; 30 mass repricing.
Allowance compiled to zero-price bands, prepaid grants, FixtureGate and per-row frozen descriptors are removed.

**Later, by separate decision:** 3 customer-group-to-book mapping; 5 brand as a book attribute;
12 reserved capacity, prepaid credit, carry-over and trailing-tier qualification; 22 contract-locked protection.
Rating/Subscriptions adapters and migration execution, Studio API wiring, GET /pricing/v1/quote (D-415) and
trial_days are outside this programme.
The fixtures crate deletion and deployment reset occur in phase 4, not in this document phase.

## 5. Functional Requirements

#### `fr-dimension-registry`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-fr-dimension-registry`

**Phase:** 2. **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

A tenant registry stores dimension keys and their allowed values, seeded with region (declared with no values until the tenant adds them). An entry selects at most one key. Validate DIM_KEY_INVALID and DIM_VALUES_FEW (a key has no values yet or at least two); a price value outside the registry is DIM_VALUE_UNKNOWN and a value without a declared key is DIM_NOT_DECLARED. Registry changes need no approval, but a value referenced by any price cannot be removed.

#### `fr-price-book`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-fr-price-book`

**Phase:** 2. **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

A book has a tenant-unique code, name, immutable currency and optional valid_from/valid_until dates. Name and validity can be patched under If-Match. Every entry and price uses the book currency; validity must describe a nonempty interval. A different entry for one plan requires another book or another SKU.

#### `fr-entry-key`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-fr-entry-key`

**Phase:** 2. **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

Inside a book there is one entry per (sku_id, charge_kind, period, model, usage_policy_digest), with null period normalized for uniqueness. Charge kind is derived from SKU type: recurring uses month or year, usage and one_time have no period. The entry's model is required at its create and fixed for its life: usage takes per_unit, graduated, volume or package, and recurring and one_time take flat or per_unit; an unknown model is MODEL_INVALID and one the charge kind does not allow is MODEL_KIND_CHARGEKIND_MISMATCH, both 400 (D-427). Another model for the same SKU, charge kind and period is another entry of the book. A bundle is never priced. An entry can override invoice-line text and change dimension_key only while no price carries a value. New entries require a published, unfenced SKU, with type re-read after reservation. The reads of one entry and of a book's entries carry the entry's usage: its prices by state (approved, pending, draft; a rejected price is not counted), the distinct plans whose draft, pending, scheduled or published revisions name it, and the distinct plans that name it only through superseded revisions (D-428).

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

D-503 projects the entry's optional typed `usage_rating_policy` on each REST resolve item
and each SDK binding. The materialized identity/content is loaded from local policy storage alongside
the selected entry; historical reads never call the meter provider. SDK bindings retain the same
`price_book_entry_id` as their price. Entry reads and exports retain D-502's optional projection.
A BillingCycle VM entry beside a CalendarHour cloudlet entry keeps two independent policies;
there is no plan-wide window or aggregation across subscription lines. Missing legacy policy is null.

**External production dependency E1 (not delivered by Pricing).** Types Registry owns immutable
meter declaration storage/lifecycle; Usage Collector owns the semantic read adapter; source/IRM
owners supply accrual-definition provenance. Their delivery is separate from this Pricing work.
The consumer port, validation and contract-test provider do not establish authoritative production
meter semantics. ClientHub must supply a real `UsageMeterSemanticsV1`; there is no successful
production fallback. Its absence is typed `UnconfiguredMeterSemantics` with canonical
`UNCONFIGURED_DEPENDENCY`; a configured outage is 503, and denial is 403. None becomes
`MISSING_RATING_POLICY` or an empty semantic result.

E1 blocks real usage-entry creation, new price/plan publication and usage sales at their semantic
gates until the authoritative provider is wired. Delivery must identify the implementing gear/adapter
and its tracked work item, and demonstrate exact-version resolution, canonical unit matching,
declared SUM/additivity, source integration provenance, historical immutability, caller authorization,
outage behavior and VM/cloudlet contract vectors against the real provider. These responsibilities
are required ownership for handoff, not evidence that another team has accepted or implemented the
work. Pricing's contract tests certify its consumer behavior only; production readiness remains
blocked until that external evidence exists.
E1 = E1a (raw meters, the usage collector / types registry; external) + E1b (derived meters, provided by Products since P-D-233).

**Owner amendment of D-503, 2026-10-01: E1 has a raw and a derived kind.** A derived (composite)
usage meter computes one quantity from other usage; a cloudlet is 128 MB of RAM and 400 MHz of CPU.
Products declares it as a derived usage type with an immutable version: its inputs at exact versions,
the formula as data, the granularity it applies at and its output unit. Rating evaluates it per
subscription line and rating window. The usage collector reports raw meters only. These are
products P-D-229 and rating T-D-39, decisions made on branch `bss/pricebook-meters` (`d8f78cf9b`)
and carried onto this branch by the derived usage types plan. E1 therefore has two parts:

- **E1a, raw meters:** Types Registry declarations answer through the Usage Collector's semantic
  adapter, with source/IRM accrual provenance, as above.
- **E1b, derived meters:** Products' derived usage type at its exact version answers: its
  canonical output unit and the digest of its stored declaration, which names the inputs at their
  exact versions and the formula (products P-D-233).

A policy's `MeterRef` names either kind. `UsageMeterSemanticsV1`, `validate_meter_policy` and the
publication and acceptance gates do not change: one provider behind the port answers both kinds,
and each kind owes the delivery evidence above against its own source. Pricing computes no derived
quantity.
E1 = E1a (raw meters, the usage collector / types registry; external) + E1b (derived meters, provided by Products since P-D-233).
For derived meters, P-D-229 supersedes the disposition of spec §3 item 11 that §4.2 lists as dropped;
Pricing still computes none.

**Amended 2026-10-01 by products P-D-233: E1b is provided; E1a is still external.** Products registers
the one `UsageMeterSemanticsV1` in the ClientHub. For a derived meter, named
`MeterRef { usage_type_id: "products.derived/<code>@<n>", version: "<n>" }`, it answers from its own
store, in the caller's tenant and under products `sku:read`: `canonical_unit` the version's output
unit, `fold` SUM, `accrual_policy_version` `derived-v1:<stored digest hex>`, `source_integrated` true,
and `digest` the stored SHA-256 of the declaration's canonical bytes. A `version` that is not canonical
or disagrees with `@<n>` is 400 `METER_POLICY_MISMATCH`; an unknown code, version or tenant is one 400
`METER_VERSION_UNKNOWN`; a store outage is 503 and a denial 403. Every other meter answers exactly as an
absent provider does (`UNCONFIGURED_DEPENDENCY`): the raw-meter provider (E1a) is not built, so raw usage
stays blocked at its semantic gates. A derived meter is sellable: products' `tests/derived_meter_e2e.rs`
sells a cloudlet through Pricing's entry, price, plan and sellability gates with no test provider.
Pricing's checks do not change.
E1 = E1a (raw meters, the usage collector / types registry; external) + E1b (derived meters, provided by Products since P-D-233).

D-502 binds an immutable UsageRatingPolicy to each new usage entry. The create requires
`usage_rating_policy` for usage (`MISSING_RATING_POLICY` otherwise) and refuses it for recurring
or one-time entries (`UNEXPECTED_RATING_POLICY`). The closed input contains rating_window
(BillingCycle or CalendarHour with UTC), aggregation_scope (subscription_line or resource),
reset (rating_window_start), quantity_semantics (meter usage_type_id/version, unit, SUM fold,
accrual_policy_version), and partial_window (actual_quantity_full_thresholds). Empty or whitespace-only
meter identifiers, versions, units or accrual versions are `METER_POLICY_MISMATCH`. The server assigns
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

D-502: a plan item remains a SKU and its selected entry (D-467), with no policy override,
treatment, included quantity or minimum quantity. Copy/clone within a book preserves entry IDs.
Changing a draft's book matches the full (SKU, charge kind, normalized period, model, policy digest)
key and an equal dimension key. With no equivalent target, the item retains the old entry and
ITEM_BOOK_FOREIGN blocks publication. An hourly entry never silently becomes monthly, and an absent
legacy policy never becomes a new policy. Explicit item selection chooses the replacement entry.

#### `fr-price`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-fr-price`

**Phase:** 2. **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

Draft prices carry price_json in their entry's model, dates, optional dim_value and min_fee, eligibility all or new, note and author; a price carries no model of its own, and money whose shape does not match the entry's model is PRICE_MISSING (D-427). Approved money is append-only and survives forever for pins. Draft-only PATCH/DELETE and pending ownership prevent changing reviewed content. Tier bands are half-open [from, to), including volume boundaries.

#### `fr-chain-windows`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-fr-chain-windows`

**Phase:** 2. **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

Windows are half-open and close independently for each (price_book_entry_id, dim_value), including the null default chain. Approval recomputes each predecessor end from the next start. The default tail is open; a value tail may explicitly end and resume default fallback. A default chain is optional; coverage is evaluated per value. Refuse starts in the past and chain overlap with WINDOW_START_IN_PAST and WINDOW_OVERLAP.

#### `fr-pair-guard`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-fr-pair-guard`

**Phase:** 2. **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

On a usage chain, a successor preserves package size and the SKU unit; the model is the entry's and never changes on a chain (D-427). Submit refuses CHAIN_MODEL_CHANGED with 400 when any changes (D-403). Apply revalidates the same invariant under the chain transaction; a new dimension chain is checked against its own predecessors.

#### `fr-min-fee`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-fr-min-fee`

**Phase:** 2. **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

The floor belongs to a price per subscription per billing period, aggregating every value and slice rated by that price. Apply before promotions, prorated by the fraction of the period the price covered; no plan carries an included quantity to deduct first (D-467). There is no plan minimum or cap. Pricing stores and validates min_fee and resolve returns it; Rating applies the floor (D-415).

#### `fr-temporary-pair`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-fr-temporary-pair`

**Phase:** 2. **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

A temporary change on an existing chain creates two prices in one approval unit. The return price copies the money versionAt would apply at temporary_until and inherits dim_value; shifting the start preserves the interval length. A value with no own chain receives one closed price ending at temporary_until and no return price, then falls back to default.

#### `fr-publish-changes`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-fr-publish-changes`

**Phase:** 2. **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

Publish changes lists all draft prices of one book with full money, window, chain, predecessor and impact information, all pre-selected. The author may choose a subset and an optional common_effective_date. One prices unit contains the selected prices, keeping temporary companions together. Prices and plan revisions remain independent units.

#### `fr-approval-units`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-fr-approval-units`

**Phase:** 2. **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

Use bss-approval for prices now and plan_revision, promotion and migration in phase 3 (the promotion and migration kinds are deferred, D-409, D-410). Copy quorum from tenant policy (kind override, otherwise *, fail-safe 1). Exclude submitter and every item author from approving. Votes name generation; GENERATION_MISMATCH is 400, DUPLICATE_VOTE and UNIT_CONTENDED are 409. Content drift commits a refreshed snapshot and generation with UNIT_STALE; environmental apply failure rolls back as APPLY_REFUSED. Reject requires a note; only the submitter withdraws. Quorum zero still records an approved unit, audit and terminal event.

#### `fr-reference-protocol`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-fr-reference-protocol`

**Phase:** 2. **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

Before reserve, Tx A claims the key and persists a create op in reserving. Reserve kind price_book_entry with Products, re-read SKU type/lifecycle; Tx B commits the entry with reservation_id and reference_state = confirmation_pending and op written. Confirm, then Tx C sets entry confirmed, op done and answers the key (D-401). Registry outage before write is 503 REGISTRY_UNAVAILABLE; a fence refuses reserve as SKU_FENCED. A confirm timeout never releases the reservation. Retry durably; REFERENCE_RELEASED during confirm keeps the entry confirmation_pending and re-reserves it through a rereserve op. The ticker also reconciles confirmed entries through states(): a released receipt is re-reserved if the SKU is not fenced; otherwise the entry becomes lost and new prices fail ENTRY_REFERENCE_LOST. Definite rollback requires durable cancellation before release; deletion commits removal and a delete op in releasing before release. Every op not done is retried with bounded backoff and never dropped. Phase 3 uses the same protocol for plan_item; sold_as waits with the sold-as bundle (D-411).

#### `fr-book-export`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-fr-book-export`

**Phase:** 2. **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

Provide one read-only JSON export of a tenant-scoped book with its entries and prices. Export preserves currency, chain values, windows, model inputs and price identities for operator inspection. It creates no mutation or approval unit.

#### `fr-settings`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-fr-settings`

**Phase:** 2. **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

Tenant settings provide default billing timing, rounding, GL code, tax category and invoice-line templates by SKU type. SKU billing timing overrides the tenant default. Approval policy is per tenant with optional kind overrides, never per book or plan. Settings and dimension-registry edits are direct versioned changes.

#### `fr-events`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-fr-events`

**Phase:** 2. **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

Persist PricesPublished and ApprovalUnitDecided with state and audit in the toolkit outbox, using broker TypedEvent envelopes; include PriceBookEntryReferenceLost for a failed reference confirmation that proves release. Every terminal unit path emits ApprovalUnitDecided; submission audits without that event. Phase 3 adds PlanRevisionPublished and PlanReferenceLost; PlanRetired and SubscriptionMigrationRequested (D-410) and PromotionPublished (D-409) are deferred. No SkuChanged listener or local SKU cache is built in phase 2.

#### `fr-plans`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-fr-plans`

**Phase:** 3. **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

A plan has immutable published revisions; each revision binds one book and contains items, availability, minimal Grants and optional sold-as bundle SKU; an item is a SKU and its entry in the plan's book, with no treatment, included quantity or minimum quantity (D-467). Enforce an entry for every item (ITEM_ENTRY_MISSING), one recurring frequency (FREQUENCY_MIXED), no duplicate usage meter (METER_DUPLICATE), no bundle item, no deprecated SKU newly added, while one carried over from the same plan's published revision stays, and one that revision carries may be added again (ITEM_SKU_DEPRECATED, D-408, D-465), and entries only from its book (ITEM_BOOK_FOREIGN). Sale-date coverage is per dimension value with an open tail or default (ITEM_UNCOVERED); book validity is PLAN_BOOK_VALIDITY. Checks compute blocked_by pending price units, and each check row names the items that turn it red and the pending prices behind blocked_by (D-466). A new plan's code, on create and on clone, is 1 to 32 characters of A-Z, 0-9, - and _, starting with a letter or a digit, judged as sent with no trim or case folding (PLAN_CODE_INVALID); a code stored before the rule keeps reading and is never judged again (D-468). Clone produces a draft; retirement requires migration. An approved revision whose sale date is after the day of its approval is scheduled: it takes effect on that date, and until then the plan keeps selling its published revision (D-446, D-449, D-450). Grants and the sold-as bundle SKU are deferred (D-411), and so is retirement (D-410).

#### `fr-promotions`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-fr-promotions`

**Phase:** 3, deferred by the owner (D-409). **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

A dated percentage promotion targets plans and recurring or recurring-plus-usage charges. Promotion windows on the same plan cannot overlap. A discount applies when the period starts inside [from_date, to_date). Approved edits increment version; consumers pin (id, version). Submit uses the common approval shape; end-today and cancel retain historical bindings.

#### `fr-migrations`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-fr-migrations`

**Phase:** 3, deferred by the owner (D-410). **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

An approved migration_request records target plan/revision, subscription ids, next_renewal or explicit date, and the period-aware preview, then emits SubscriptionMigrationRequested. The target revision must be published. Pricing requests movement; Subscriptions executes it and confirms retirement in its own plan. A new revision alone changes no pins.

#### `fr-resolve`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-fr-resolve`

**Phase:** 4. **Source:** spec §2.2, §2.4, §5–§7, §12–§13; D-419, D-420, D-421, D-424, D-425.

GET /bss-pricing/v1/resolve (spec §7.1's /pricing/v1/resolve, D-419) accepts plan_revision_id, date, an optional item_id and optional pins (price_id, or price_id:dim_value for a default-chain price a value was bound to) and returns, for a published or superseded revision, or a scheduled one on or after its sale date (D-454), each item's full default/value chain matrix without totals; the active promotion (id, version) is deferred with promotions (D-409). New subscriptions bind the price in force, the value's own chain else the default. Renewal walks a pinned chain through all successors, stopping before the first new successor; a binding is always a price in force on the date, and a default-chain pin moves to the value's own later all price (D-420). Each binding carries ends_on, its own end (a temporary price's end or an explicit close, else null), and the consumer slices a period at ends_on, never at the stored effective_to (D-425). A chain that no price covers is explicit uncovered, never refused and never an invented price. Usage binds lazily per (item, dim_value); at a binding's ends_on inside a period the consumer resolves again with the pin on that date. Each item binds its SKU version from Products versions?as_of at the date, read as pricing's system actor, so a consumer needs only pricing's grants (D-424), and resolved invoice inputs with their source (entry, SKU or tenant): invoice-line template, GL code, tax category and billing timing, with rounding policy and currency scale (D-421). Each item carries its entry's model, null without an entry; a binding carries none (D-427).

D-501 delivers the matching authorized SDK matrix through `PricingReadV1::resolve`, with an explicit
catalog tenant and typed complete bindings. Missing priced-cell commercial inputs are a typed failure;
REST's nullable preview stays compatible. `current_revision` returns the revision in effect after
persisting any due approved switch. Acceptance and consumer integration remain separate work.

#### `fr-price-read`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-fr-price-read`

**Phase:** 4. **Source:** spec §2.2, §5–§7, §12–§13; D-422.

GET /bss-pricing/v1/prices/{id} (spec §7.1's /pricing/v1/prices/{id}, D-422) serves an approved price forever, including closed, superseded and keep_for_bound prices, with its entry's SKU, charge kind, period, model (D-427), book and currency: stored facts only, nothing computed from today. A draft, pending or rejected price, an unknown id and another tenant's id answer the same 404. The consumer retains price id, dimension value and used chain, SKU version/meter/unit, descriptors, timing, rounding, currency scale and promotion version (deferred with promotions, D-409) in its binding; later descriptor changes do not rewrite earlier pins.

`PricingReadV1::price` (D-501) exposes these approved facts as exact decimal models with a financial
content digest that excludes mutable closure metadata. The selected-binding digest separately freezes
the dated unit, entry and requested dimension, policy projection and complete invoice inputs.

#### `fr-quote`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-fr-quote`

**Phase:** 4, not built (D-415). **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

Not built (D-415): the owner dropped quote and the Studio wiring; consumers read resolve and GET /bss-pricing/v1/prices/{id}, and Rating owns the minimum-fee floor arithmetic. GET /pricing/v1/quote is the Studio preview with quantities, returning totals (since D-467 no plan item is optional or carries an included quantity). Apply price selection, half-open tiers, per-price prorated min_fee and promotions in that order. Recurring slices prorate by calendar days; usage readings use their timestamps and counters restart per slice. Quote is separate from the consumer resolve contract.

## 6. Non-Functional Requirements

#### `nfr-authz`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-nfr-authz`

**Phase:** 2. **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

Every door authenticates and enforces deny-by-default pricing:read, author, submit, approve or settings through PolicyEnforcer. Resource labels cover books, entries, prices, approval units, config and, from phase 3, plans (read, author and submit, D-418); the four route censuses carry positive, denial and precondition probes. Combined grants never bypass author separation.

#### `nfr-audit`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-nfr-audit`

**Phase:** 2. **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

Append tenant, actor, subject, correlation and before/after facts with each governed act. Submission and all terminal outcomes, including quorum zero and reference-loss handling, remain attributable. Audit persistence shares the mutation transaction; records cannot be deleted or rewritten.

#### `nfr-tenant-isolation`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-nfr-tenant-isolation`

**Phase:** 2. **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

Every repository uses SecureORM and PDP-derived AccessScope; child prices are reached through scoped parents. Uniqueness and replay identity include tenant boundaries. Cross-tenant ids never grant access through parent, child, export, approval or reservation paths.

#### `nfr-two-backends`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-nfr-two-backends`

**Phase:** 2. **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

The fresh migration chain, constraints and transactional rules work on SQLite and Postgres. Chain approval uses PostgreSQL serializable isolation and SQLite writer serialization. There is no FOR UPDATE. Retry serialization/lock-upgrade failure through a bounded loop, re-reading and revalidating each attempt.

#### `nfr-idempotency-concurrency`

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-nfr-idempotency-concurrency`

**Phase:** 2. **Source:** spec §2.2, §5–§7, §12–§13; phase 2 plan for delivery details.

All pricing POSTs require Idempotency-Key; PATCH/PUT require If-Match. Replay uses (tenant, concrete endpoint, client_key), payload hash and 24-hour retention, checked before reservation or unit work, with claim and answer in the mutation transaction. An approval unit has no separate idempotency key. Conditional object/unit versions prevent stale overwrites; a committed UNIT_STALE receipt replays as such.

## 7. Public Library Interfaces

### 7.1 Public API Surface

Authoring routes mount at `/bss-pricing/v1`: books and export, entries, prices, publish-changes, approval-units,
approval-policy, settings and dimension-keys. Later routes add plans/revisions/checks; promotions (D-409) and migrations (D-410) are deferred.
Phase 6 adds the operator reads and edits: where a SKU is priced and sold (a SKU's entries across books, the plans
that name it, one plan item, D-434), the reset of a kind's quorum override (D-435), the one-key edit of dimension
values with each value's use (D-436), and settings that offer currencies and say who changed them, with a closed set
of rounding modes (D-437, D-438).
Phase 7 adds the Price Books screen's reads: an entry's prices with their status today, its price in force and its
approved prices by date (D-440), every book read's stats (D-441), and a book list that pages on the toolkit's OData
pager and is searched by q and sku_id (D-442); and its writes: the dates of a temporary draft, whose pair follows
them (D-443), and a book's description and the delete of a book nothing uses (D-444). An approval unit carries its
submitter's note, `submit_note`, the unit shape products shares (D-445).
Phase 9 adds what the plans screen reads: each plan's current revision and the one in effect (D-460), who made
each revision and when it was submitted and approved (D-461), and a pending revision's vote progress (D-462); and
a plan's create and clone take its sale date (D-463). A plan revision's submit and publish-changes carry the
submitter's note to the unit's submit_note; a single price's submit takes none (D-464). A draft may add again a
deprecated SKU its plan's published revision in effect carries (D-465). The approval units are counted by state and
kind under the list's narrowing, the list pages newest first on request, and it skips the live impact on request
(D-470); every unit says whether its reader may approve it, by the engine's own rule (D-471). Each entry read names
the entry's next price beside its price in force (D-472), and a book's entries list reads its prices on a date
(D-473).
The frozen consumer contract is named `/pricing/v1/resolve` and `/pricing/v1/prices/{id}` in spec §7.1;
phase 4 must explicitly wire that public surface. Wire fields and query parameters are snake_case.
Doors use headers + Bytes and preconditions::parse_body with correlation::establish on mutations. Errors expose
code/field/message through canonical RFC-9457 Problem responses. Reads expose ETag, and so does every write answer that sets one, declared in the served contract; mutation preconditions are required. Every operation declares its 503, and the operations that read Products hard name REGISTRY_UNAVAILABLE (D-469).

D-504 defines the pure new-sale profile, narrower than the readable catalog:

| Selected binding | Supported profile | Stable refusal |
| --- | --- | --- |
| Recurring | Flat or PerUnit; month/year equal to BillingTerms | UnsupportedModel / BillingCycleMismatch |
| One-time | Flat or PerUnit; no recurring period or usage policy | UnsupportedModel / UnsupportedTerms |
| Usage BillingCycle | PerUnit, Volume or Graduated; period null; immutable explicit policy | MissingRatingPolicy / MeterPolicyMismatch |
| Usage CalendarHour | Same usage models; UTC, SUM, subscription_line or resource scope; no minimum fee, including zero | UnsupportedTerms |
| BillingCycle minimum fee | SubscriptionLine only; any Resource-scoped floor is refused | UnsupportedTerms |
| FX / cross-currency sale | Currency must equal the selected price currency | CURRENCY_MISMATCH |
| Package, promotions, phases, allowances, quarter | Not part of new-sale terms; historical catalog reads remain intact | UnsupportedModel / UnsupportedTerms |

Invoice terms must be schema version 1, month/year and UTC, with explicit order or positive,
non-nil seller-policy provenance. Orders resolves this Subscriptions-owned snapshot before Pricing
is called. Calendar anchors are the first day at midnight UTC (January 1 for a year). Hourly usage
requires an hour-aligned SubscriptionStart anchor; BillingCycle-only usage allows a 10:30 anniversary.
A 10:30 activation with a valid calendar invoice anchor is supported. Pricing never chooses, shifts
or rounds an anchor. Different entries in one plan may retain different rating windows.

Quantity and fixed period count must be positive. Selected bindings must cover exactly one cell per
item, with no duplicate/foreign selection, and agree on requested dimensions and binding/price entry
identity. Every price currency must equal the market currency; a region dimension must equal the
market region, including when the price came via the default chain. Invoice inputs require dated SKU
identity/version/code/name, the unit for PerUnit/usage, nonempty template/GL/tax, the existing book
currency scale and HalfEven. The existing book currency spelling/minor-digit rules apply; this slice
adds neither a currency registry nor FX. Money is nonnegative and tier validation delegates to
`domain::money::validate_tiers`. BillingTerms, policy, money and template digests are recomputed;
policy unit must equal the dated binding unit. No second tier interpreter is introduced.

The deterministic VM fixture uses entry 2, EUR, no dimension/region, 2026-10-01T00:00Z, monthly
calendar invoice terms, Rolling, quantity 1, VM BillingCycle policy, SKU v3, VM-2CPU-4GB,
VM 2 vCPU / 4 GB, VM·hour, PerUnit 0.047, no floor, VM_REVENUE, cloud-services, VM usage,
scale 2 and HalfEven. Supported test variants recertify altered content digests; integrity tests
intentionally retain a stale digest. Exact threshold 10 exercises the existing half-open arithmetic.

Pure SaleObservation is the specified five booleans, derived from verified live reads. Non-current or
unavailable revisions and inactive/unsellable SKUs return NotSellable; missing coverage returns
ResolutionChanged. That shape intentionally does not distinguish retired from deprecated or off-sale.
Provider failures are not observations of commercial ineligibility: missing E1 remains 400
UNCONFIGURED_DEPENDENCY naming UsageMeterSemanticsV1, configured outage remains 503 and denial 403.
E1 = E1a (raw meters, the usage collector / types registry; external) + E1b (derived meters, provided by Products since P-D-233).

Commercial RuleError carries a typed reason alongside its stable uppercase code. Canonical invalid
arguments retain the concrete reason in field-violation metadata (400); resolution/payload/expiry/
eligibility changes use aborted reason metadata (409); denial is permission denied (403), and an
authorized missing receipt is not found (404). The existing Toolkit RFC 9457 conversion is reused.
Wire scalars retain unsupported values in typed errors; strict BillingTerms decoding rejects missing
snapshots, unknown/duplicate fields, numeric versions, unsupported schema, cycles and timezone.
The SDK stays free of serde/storage types. The pure layer exposes no NewSale HTTP route; D-507 implements the authorized SDK acceptance command.

The SDK adds bss.billing-terms.v1, pricing.request.v1 and pricing.terms.v1 canonical JSON projections.
BillingTerms excludes its own digest; requests include all commercial intent and exclude command
metadata; accepted terms include query plus sorted complete bindings, including entry/policy identity,
without receipt identity/server timestamps. Exact decimals and all integers are strings; instants
normalize to UTC with nine fractional digits. Four new frozen vectors are verified in Rust and Node.

Task 5c supplies the complete live item universe, re-resolves selections and compares the caller's
selected-binding digest independently, validates authoritative meter observations outside transactions,
and preserves the accepted snapshot. The pure terms function cannot detect an item omitted from both
its query and its binding arguments. D-505–D-507 deliver migration 19, the shared authorized service,
immutable receipt reads and atomic acceptance/command/audit persistence. Successful replay preserves
the original deadline, including another command key for identical order/line/version intent.
Changed content conflicts; changed commercial intent needs a new order version. Provider failure
leaves no acceptance or poisoned command. D-508 supplies hold/live fulfilment, and acceptance alone
does not authorize activation. No downstream Rating/Billing scheduler is delivered here.

### 7.2 External Integration Contracts

ProductsClient supplies SKUs and reference receipts; Products versions?as_of supplies dated snapshots.
Reserve is idempotent for a live logical reference, confirm on a confirmed receipt succeeds, and released receipts
never reactivate. Rating and Subscriptions receive golden responses and adapt in separate plans. The retained
ProductCatalogClientV1 transport exists for compatibility until later demolition; it does not replace this protocol.
Pricing fills Products' SkuUsageV1 port, which gives each SKU read its count of entries, their currencies, prices
by state and distinct plans (D-428, Products P-D-197), and the priced and in-plan SKU sets the Products list
filters by (Products P-D-212).

D-508 delivers the SDK hold and fresh fulfilment capabilities. Accepting EUR 10/SKU v3 then publishing
EUR 12/SKU v4 retains the old entry, policy, money, descriptors and invoice inputs in the hold, even
after deprecation/off-sale or a successor revision. Retirement, explicit/temporary price ends and
server-time expiry refuse fresh eligibility at their boundary; successor effective_to is ignored.
A first activation may occur later than submit inside the accepted window and is pinned permanently.
Exact successful hold replay remains historical; new keys cannot extend TTL. Subscriptions owns
committed order/version and attempt fencing and checks immediately before its first activation
intent: no eligibility observation is a reusable admission token. G3 release validation and external
E1/consumer integration are separate obligations.
E1 = E1a (raw meters, the usage collector / types registry; external) + E1b (derived meters, provided by Products since P-D-233).

## 8. Use Cases

#### Publish a dated repricing batch

A Finance Manager drafts three prices for October 1 and selects Publish changes. An independent reviewer compares
predecessors, proposed prices and live impact, then approves one prices unit. The changed book affects every
plan using it; rejection of a separately proposed plan revision cannot undo this approved book fact (spec §8).

#### Recover a confirmation outage

Pricing reserves a SKU, commits an entry and loses the confirm response. The entry shows confirmation_pending;
the durable worker retries. Products refuses retirement while the live receipt exists. An operator-forced release
is re-reserved, or surfaced as reference_state = lost while the SKU is fenced; a confirm timeout itself never
triggers release (spec §13).

#### Bind a descriptor change

A SKU GL change effective October 1 creates a Products version. A period starting October 1 binds that version;
earlier pins retain the original GL. Pricing creates no refreeze prices or approval unit for the descriptor change.

## 9. Acceptance Criteria

| Criterion | Requirement | Given / When / Then |
| --- | --- | --- |
| AC #1 | `cpt-cf-bss-pricing-fr-dimension-registry` | Given a registered region with priced EU prices, when an operator adds US then it is available; removing EU is refused and a price for an unknown value is DIM_VALUE_UNKNOWN. |
| AC #2 | `cpt-cf-bss-pricing-fr-price-book` | Given a EUR book, when USD commercial terms are needed then a separate book is created; duplicate book code in the same tenant and invalid validity bounds are refused. |
| AC #3 | `cpt-cf-bss-pricing-fr-entry-key` | Given a published recurring SKU, when its monthly entry is created then charge_kind is recurring; a duplicate key, the model included, is refused while another model is another entry (D-427), a bundle cannot be priced, and changing the dimension key after a valued price exists is refused. |
| AC #4 | `cpt-cf-bss-pricing-fr-price` | Given a volume ladder with a boundary at 1000, when quantity is 1000 then the band starting at 1000 applies; editing approved money or creating a usage entry with flat is refused. |
| AC #5 | `cpt-cf-bss-pricing-fr-chain-windows` | Given EU and default chains, when a new EU price is approved then only the EU predecessor closes; after an explicit EU tail ends the default applies, and overlapping prices on the same chain are refused. |
| AC #6 | `cpt-cf-bss-pricing-fr-pair-guard` | Given a package usage predecessor, when its successor changes only money then it is admissible; changing package size or SKU (unit, usage_type_ref) as of each price's start produces 400 CHAIN_MODEL_CHANGED. |
| AC #7 | `cpt-cf-bss-pricing-fr-min-fee` | Given two regions rated at 10 each, when both bind one default price with min_fee 30 then their combined charge floors at 30; two separate prices each carrying 30 floor at 60, without applying the shared floor twice. |
| AC #8 | `cpt-cf-bss-pricing-fr-temporary-pair` | Given an existing chain, when a temporary pair is shifted by five days then both boundaries shift five days; for a value with no chain only one closed price is created, and an invalid end before the start is refused. |
| AC #9 | `cpt-cf-bss-pricing-fr-publish-changes` | Given three draft prices and a temporary companion, when the author deselects a normal price and supplies a common date then only the selected atomic set enters one unit; a ticked half of a temporary pair brings its partner into the unit (added_partner, D-405), and a foreign-book price is refused without partial locks. |
| AC #10 | `cpt-cf-bss-pricing-fr-approval-units` | Given a pending unit, when an independent reviewer meets quorum then it applies atomically; an item author receives SOD_VIOLATION, a stale generation cannot count, and content drift commits UNIT_STALE without publishing the old content. |
| AC #11 | `cpt-cf-bss-pricing-fr-reference-protocol` | Given a reservation and committed entry, when confirm times out then the entry remains confirmation_pending and retry is durable; retire remains blocked, and only durable cancellation/deletion permits release. An op is durable before reserve and survives restart even without an entry. Confirmed entries reconcile released receipts through re-reserve when unfenced, or lost state otherwise. |
| AC #12 | `cpt-cf-bss-pricing-fr-book-export` | Given an authorized reader, when a book is exported then its scoped entries and prices appear; another tenant cannot obtain its data and export creates no writes. |
| AC #13 | `cpt-cf-bss-pricing-fr-settings` | Given arrears as the tenant default and advance on the SKU, when binding resolves timing then advance wins; stale settings If-Match and unauthorized settings changes are refused. |
| AC #14 | `cpt-cf-bss-pricing-fr-events` | Given an approved price unit, when commit succeeds then domain and terminal events are durable; an outbox failure rolls back state and no rejected or withdrawn unit emits PricesPublished. |
| AC #15 | `cpt-cf-bss-pricing-fr-plans` | Given a revision awaiting a price unit, when checks run then ITEM_UNCOVERED names the blocking unit and submit is refused; once prices are approved checks pass, while mixed recurring frequencies and foreign-book entries still fail. |
| AC #16 | `cpt-cf-bss-pricing-fr-promotions` | Deferred by the owner (D-409). Given an approved promotion, when a period starts on its end date then it receives no discount; overlapping promotions are refused, and an approved edit creates a new version without changing a prior pin. |
| AC #17 | `cpt-cf-bss-pricing-fr-migrations` | Deferred by the owner (D-410). Given subscriptions pinned to revision 3, when revision 5 publishes then the pins remain; approving an eligible migration emits a request, an unpublished target is refused, and Pricing does not claim the move completed. |
| AC #18 | `cpt-cf-bss-pricing-fr-resolve` | Given pinned 10, an all price at 12 and a new price at 15, when renewal resolves then it binds 12 while signup binds 15; a date with no coverage is explicit uncovered, never refused, and no resolve response contains quantity-derived totals. |
| AC #19 | `cpt-cf-bss-pricing-fr-price-read` | Given a closed price referenced by a prior invoice, when its id is read then the original money remains available; an unknown id or another tenant's id exposes no price. |
| AC #20 | `cpt-cf-bss-pricing-fr-quote` | Not built (D-415). Given quantities spanning a temporary boundary, when preview runs then slices use their own models and the price floor aggregates correctly before promotion; invalid quantities fail and preview does not mutate pins. |
| AC #21 | `cpt-cf-bss-pricing-nfr-authz` | Given an otherwise valid request without the required action, when the door runs then it denies without a mutation; the permitted action succeeds under the same tenant scope. |
| AC #22 | `cpt-cf-bss-pricing-nfr-audit` | Given an approval or withdrawal, when its audit insert fails then its state change rolls back; successfully committed audit cannot be changed or deleted. |
| AC #23 | `cpt-cf-bss-pricing-nfr-tenant-isolation` | Given two tenants with the same book code or replay key, when they act then each remains independent; using the other tenant's price or unit id cannot read or mutate it. |
| AC #24 | `cpt-cf-bss-pricing-nfr-two-backends` | Given two writers approving intersecting chains on either backend, when they race then approved windows stay nonoverlapping; a losing conditional unit write is UNIT_CONTENDED rather than a second apply. |
| AC #25 | `cpt-cf-bss-pricing-nfr-idempotency-concurrency` | Given a successful keyed POST, when the same body repeats then the saved response returns without a second object/unit; a changed payload conflicts, stale If-Match preserves state, and concurrent claims cannot both mutate. |

## 10. Dependencies

Products SKU versions and reference registry; shared bss-approval engine and DDL; toolkit authz, scoped storage,
transaction retry, outbox and broker; the prototype pure rules and surviving arithmetic goldens. Pricing requires
Products availability before creating a reference; consumers do not participate in the pricing mutation transaction.

## 11. Assumptions

A default chain is optional. Plan coverage includes every registered dimension value. Registry/settings changes
need no approval. Quorum is tenant-wide with kind overrides. New revisions alone never migrate subscriptions.
The spec's §2.2 amendments override the obsolete idempotency column shown in its §6 illustrative DDL.

## 12. Risks

A dead caller may leave a reservation blocking retirement until durably recovered or force-released. A forced
release under a live object requires explicit remediation. Descriptor history must be queried by date, since the
current SKU may already hold future-effective content. Consumer adapters break until their separate migration.
Legacy pricing code remains during Part 2a; unchecked requirements are not claims of implementation.

## 13. Open Questions

No unresolved model decision blocks this document set. Phase 4 must verify routing of the public consumer paths
and golden responses before consumer adapters are implemented. Segment mapping and reserved-credit capabilities
remain later decisions, not implicit requirements of this rewrite.

## 14. Traceability

Content authority: spec §2 decisions 4–7, 13–17, §2.2, §3 A–C, §5–§8 and §12–§13.
[DESIGN](DESIGN.md) supplies the drivers and transactions; ADRs record four structural decisions;
[DECISIONS](DECISIONS.md) records D-384 onward. Slices and FEATUREs carry phase-specific implementation obligations.

D-509 supplies reusable executable Pricing provider fixtures for F07/F22/F31 and the Pricing-input
portions of F02/F23/F24 in `pricing/tests/seam_fixtures`. Each schema-1 envelope has explicit given and
expected typed values; complete commercial pins must survive resolve, acceptance and hold. Mixed VM
BillingCycle/cloudlet CalendarHour plans and entry reuse do not pool independent subscription lines.
The cloudlet profile is monthly billing, period=null, CalendarHour UTC, SubscriptionLine, SUM of
integrated cloudlet·hour v1 quantities, integrated-v1 accrual, full thresholds on partial windows,
volume bands 10 @ 0.02 then 0.015, and no minimum fee. Q=8 and Q=12 independently yield 0.34 rather
than combined-volume 0.30; graduated(12)=0.23 and the Q=10 boundary is volume 0.15/graduated 0.20.
These are existing money-helper representation checks, not executed hourly scheduler or invoice tests.

Acceptance provisions nothing. The new commercial commands remain on the authorized SDK ports;
there is no new public HTTP command API. A future remote transport binds the same ports and services.
Production meter declarations, Rating scheduling/roll-up and Billing invoice assertions remain external
obligations. Illustrative meter names are registered only by the contract-test provider. Task 8 and G4
are still required for the complete implementation handoff.


**Final commercial provider acceptance criteria (D-510).** The seven typed methods, canonical refusal
classes, indefinite retention and database evidence are consolidated in
[DESIGN](DESIGN.md#executable-seam-fixture-boundary-d-509). An SDK preview does not accept an order;
SellabilityV1::check issues the immutable receipt, PricingAcceptanceV1::acceptance reads it, hold freezes
the first eligible activation, and check_fulfilment rechecks current eligibility. Orders must retain the
receipt; Subscriptions retains accepted entry/policy/price, dated descriptors and BillingTerms. Existing
recurring/one-time Flat/PerUnit and usage PerUnit/Volume/Graduated support remains exactly the D-504
matrix above. No Package sale, allowance, FX, promotional phase, cross-line pooling or hourly minimum fee
is admitted. Approval submit and final apply revalidate authoritative meter evidence and local entry
identity. Different plan items may use different entry-owned windows; a book change must match the
policy digest and dimension key as well as SKU/kind/period/model, retaining an unmatched old entry for
explicit repair. Legacy policy-less reads remain valid while new usage sales fail closed.

**External production obligations remain open.** E1 = E1a (raw meters, the usage collector / types registry; external) + E1b (derived meters, provided by Products since P-D-233). E1a (raw meters): Types Registry owns immutable
declarations, Usage Collector the authorized exact-version semantic adapter, and source/IRM owners
the accrual provenance. E1b (derived meters; products P-D-229 and rating T-D-39) is delivered by
Products (products P-D-233): the one provider behind `UsageMeterSemanticsV1` answers a derived usage
type at its exact version with its canonical output unit and the digest of its stored declaration,
which names the inputs at their exact versions and the formula, and answers every raw meter as
unconfigured until E1a is delivered behind it. Delivery must identify the implementation and tracked
work and prove, for each kind, canonical units, SUM/additivity, source integration, historical
immutability, authorization, outage behavior and real VM/cloudlet vectors; for E1b that evidence is
products' meter-semantics tests and its `tests/derived_meter_e2e.rs`.
E2: Orders resolves Subscriptions-owned versioned BillingTerms and authenticates
payer/market; Subscriptions checks committed order/version and attempt fencing immediately before
activation. E3: deployment grants scoped actions to Orders, Subscriptions and Rating; names confer no
privilege. E4: Collector retains immutable source history, Subscriptions schedules incompatible policy
changes at the next UTC hour boundary, and Rating consumes the original history. Rating owns hourly
scheduling/reset/catch-up and exact amounts; Billing sums exact contributions before HALF_EVEN invoice
rounding. Pricing tests do not certify those downstream behaviors.
