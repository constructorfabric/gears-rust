<!-- CONFLUENCE_TITLE: [BSS]: Pricing — Read Contract & Events (Design, Slice 7) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../DECISIONS.md | Owners: BSS Pricing team -->

# DESIGN — Read Contract & Events (Slice 7)

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-design-slice-07`

<!-- toc -->

- [1. Context](#1-context)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Resolve a renewal and preserve invoice inputs](#resolve-a-renewal-and-preserve-invoice-inputs)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [renewal-walk](#renewal-walk)
  - [period-slices-and-quote](#period-slices-and-quote)
  - [typed-events](#typed-events)
- [4. States (CDSL)](#4-states-cdsl)
- [5. API Surface](#5-api-surface)
- [6. Data Model](#6-data-model)
- [7. Events & Alarms](#7-events--alarms)
- [8. Definitions of Done](#8-definitions-of-done)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Non-Functional Considerations](#10-non-functional-considerations)

<!-- /toc -->

## 1. Context

**Delivery:** phase 4 for reads, with quote not built (D-415); 2c for core events; 3 for added events. Every checkbox is an implementation obligation, not an assertion about the legacy code. Core event payloads are built with phase 2 approvals; dependencies on plans/promotions apply only to their later reads and events.

Deliver reproducible resolution matrices, pinned-price reads and Studio quote, plus typed transactional events and consumer goldens. The Studio quote is not built (D-415).

Requirements: `cpt-cf-bss-pricing-fr-resolve`, `cpt-cf-bss-pricing-fr-price-read`, `cpt-cf-bss-pricing-fr-quote`, `cpt-cf-bss-pricing-fr-events`. Architecture: `cpt-cf-bss-pricing-component-read-contract`, `cpt-cf-bss-pricing-component-events`, `cpt-cf-bss-pricing-component-prices`, `cpt-cf-bss-pricing-principle-book-money-independent`, `cpt-cf-bss-pricing-constraint-two-backends`.
[FEATURE](../features/read-contract-events.md) owns the executable flow/algorithm/DoD identifiers; this slice defines no duplicate DoDs.
Dependencies: `cpt-cf-bss-pricing-feature-plans`, `cpt-cf-bss-pricing-feature-promotions-migrations`, `cpt-cf-bss-pricing-feature-approvals`.
Source: PriceBook spec §2.2, §5–§8, §12–§13 and [DECISIONS](../DECISIONS.md) D-384–D-433.

D-503 projects the entry's optional typed `usage_rating_policy` on each REST resolve item
and each SDK binding. The materialized identity/content is loaded from local policy storage alongside
the selected entry; historical reads never call the meter provider. SDK bindings retain the same
`price_book_entry_id` as their price. Entry reads and exports retain D-502's optional projection.
A BillingCycle VM entry beside a CalendarHour cloudlet entry keeps two independent policies;
there is no plan-wide window or aggregation across subscription lines. Missing legacy policy is null.

## 2. Actor Flows (CDSL)

### Resolve a renewal and preserve invoice inputs

Actors: `cpt-cf-bss-pricing-actor-rating`, `cpt-cf-bss-pricing-actor-subscriptions`, `cpt-cf-bss-pricing-actor-products`, `cpt-cf-bss-pricing-actor-finance-manager`. Feature flow: `cpt-cf-bss-pricing-flow-read-contract-events`.

1. [ ] - `p1` - Rating or Subscriptions sends the revision id, period start and optional current pins. - `inst-read-contract-events-flow-1`
2. [ ] - `p1` - Load immutable revision structure and the full chain matrix, scoped to the tenant. - `inst-read-contract-events-flow-2`
3. [ ] - `p1` - For existing pins walk eligible all successors, stopping before the first new price; for signup select in-force prices. - `inst-read-contract-events-flow-3`
4. [ ] - `p1` - Read Products versions?as_of for the period start, bind descriptors/timing/meter/unit with their source (entry, SKU or tenant, D-421) and return the whole dimension matrix. - `inst-read-contract-events-flow-4`
5. [ ] - `p1` - A later replay reads the pinned price by id (GET /bss-pricing/v1/prices/{id}, D-422): the approved money is served forever, whatever its window; binding usage lazily per value and keeping the complete inputs are the consumer's part. - `inst-read-contract-events-flow-5`
6. [ ] - `p1` - Return the active promotion (id, version) with the matrix: deferred with promotions (D-409), this step stays unticked until they return. - `inst-read-contract-events-flow-6`

## 3. Processes / Business Logic (CDSL)

### renewal-walk

Feature algorithm: `cpt-cf-bss-pricing-algo-read-contract-events-renewal-walk`.

1. [ ] - `p1` - Validate the supplied pin belongs to the tenant, revision item and entry chain. - `inst-read-contract-events-renewal-walk-1`
2. [ ] - `p1` - From the pinned price walk successors with eligibility all, bounded by the relevant date; stop before the first new successor. - `inst-read-contract-events-renewal-walk-2`
3. [ ] - `p1` - Without a pin choose the in-force price per value, falling back to default where the value has no active price. - `inst-read-contract-events-renewal-walk-3`
4. [ ] - `p1` - Return uncovered rather than inventing a price when neither chain covers; preserve historical keep_for_bound prices. - `inst-read-contract-events-renewal-walk-4`

### period-slices-and-quote

Feature algorithm: `cpt-cf-bss-pricing-algo-read-contract-events-period-slices-and-quote`. Not built (D-415): the owner dropped quote and the Studio wiring; consumers read resolve and GET /bss-pricing/v1/prices/{id}, and Rating owns the minimum-fee floor arithmetic.

1. [ ] - `p1` - Split the period at every price boundary inside the bound chain, including a temporary end. - `inst-read-contract-events-period-slices-and-quote-1`
2. [ ] - `p1` - Prorate recurring slices by calendar days; rate usage by reading timestamp with tier counters per slice. - `inst-read-contract-events-period-slices-and-quote-2`
3. [ ] - `p1` - Aggregate per price/subscription/period (no plan carries an included quantity to deduct since D-467) and apply the coverage-prorated min_fee once per price; not built in pricing, Rating applies the floor (D-415). - `inst-read-contract-events-period-slices-and-quote-3`
4. [ ] - `p1` - Apply period-start promotion after floors, then the bound rounding/currency policy; quote returns totals while resolve never does; quote is not built (D-415). - `inst-read-contract-events-period-slices-and-quote-4`

### typed-events

Feature algorithm: `cpt-cf-bss-pricing-algo-read-contract-events-typed-events`.

1. [ ] - `p1` - Implement PricesPublished, ApprovalUnitDecided and PriceBookEntryReferenceLost through broker TypedEvent in phase 2. - `inst-read-contract-events-typed-events-1`
2. [ ] - `p1` - Add PlanRevisionPublished, PlanReferenceLost, PlanRetired, PromotionPublished and SubscriptionMigrationRequested as their phase 3 acts become real; PromotionPublished (D-409), PlanRetired and SubscriptionMigrationRequested (D-410) are deferred. - `inst-read-contract-events-typed-events-2`
3. [ ] - `p1` - Append event and audit through the same mutation transaction; encode the tenant and stable subject identities in the durable envelope, the correlation id staying on the audit rows of the same transaction. - `inst-read-contract-events-typed-events-3`
4. [ ] - `p1` - Deliver from the toolkit dispatcher after commit; test restart/retry and prevent domain publish on reject/withdraw/refresh. - `inst-read-contract-events-typed-events-4`

## 4. States (CDSL)

A consumer binding is created for a period and stays immutable for replay. New all prices affect later renewal binding; a new price blocks forward renewal walking until explicit migration. Closed/superseded/keep_for_bound prices remain readable. Event states belong to toolkit delivery; Pricing does not maintain a second outbound state machine.

State definition: `cpt-cf-bss-pricing-state-read-contract-events` in the FEATURE.

## 5. API Surface

The spec consumer paths GET /pricing/v1/resolve and GET /pricing/v1/prices/{id} are mounted below the gear's base; phase 4 registers them with golden snake_case request/response contracts:

- GET /bss-pricing/v1/resolve?plan_revision_id=&date=&item_id=&pins= (label plan, action read; D-419). plan_revision_id and date (YYYY-MM-DD) are required; item_id resolves that one item only; pins is comma-separated, each pin price_id (the price's own chain) or price_id:dim_value (a default-chain price that value was bound to), at most 1 000. Only a published or superseded revision resolves, and a scheduled one on a date on or after its sale date, judged by the state the revision reads today (D-447, D-454): a due revision resolves as published on every date, before and after its switch is persisted. Refusals: 409 REVISION_NOT_PUBLISHED (a draft or pending revision); 409 REVISION_NOT_YET_AVAILABLE (a scheduled revision on a date before its sale date); 400 DATE_INVALID; 400 PIN_FOREIGN for the whole request (a pin that names no approved price of an entry an item of this revision names, or a :dim_value pin on a price that is not a default-chain price; the value itself is not checked against today's registry); 400 PIN_DUPLICATE (two pins for one item and value); 400 PINS_TOO_MANY; 404 for an unknown or another tenant's revision, before any Products read, and for an item_id the revision does not have. Each item's SKU version is read as of date as pricing's system actor, only after the caller has passed plan:read and the revision was found in its tenant (D-424), so a consumer needs pricing plan:read and never products read: 503 REGISTRY_UNAVAILABLE when Products cannot answer, Products' own status and code on a definite refusal, and sku_version null for a SKU Products does not know (D-421). A chain that no price covers is not a refusal: it is uncovered (D-420, PRD AC #18).
- GET /bss-pricing/v1/prices/{id} (label price, action read; D-422): an approved price of the tenant, whatever its window (closed, followed by a later price, keep_for_bound), with its entry's SKU, charge kind, period, model (D-427), book and currency; stored facts only, no status or other value computed from today, no authoring internals (version, pending_unit_id, note, created_by). A cancelled price is served too, with `status: cancelled`, a stored fact; an approved price carries no status (D-520). The SDK's `ImmutablePrice.state` is `Approved` or `Cancelled` the same way, and a binding's price is always `Approved`. A draft, pending or rejected price, an unknown id and another tenant's id are 404 with one body; an id that is not an id is 400 ID_INVALID. Every refusal names the type of what it refused: each GET /resolve refusal is a cf.bss.pricing.plan.v1~ resource error, each GET /prices/{id} refusal a cf.bss.pricing.price.v1~ one.

GET /pricing/v1/quote is a Studio preview with quantities (no plan item is optional since D-467); it is not built, and the Studio is not wired to the API (D-415). Both reads are tenant-scoped and deny-by-default, and write nothing: no binding, no audit row, no idempotency key.

[DESIGN §3.3](../DESIGN.md#33-api-contracts) fixes canonical errors and route prefixes.
Each mounted route must appear in all four censuses with authz and precondition expectations.

The delivered in-process surface (D-501) is `pricing-sdk::read::PricingReadV1`, registered as
`dyn PricingReadV1` in ClientHub beside Products' existing `SkuUsageV1` provider:

```rust
async fn resolve(&self, ctx: &SecurityContext, query: ResolveQuery)
    -> Result<ResolvedBindings, CanonicalError>;
async fn price(&self, ctx: &SecurityContext, query: PriceQuery)
    -> Result<ImmutablePrice, CanonicalError>;
async fn current_revision(&self, ctx: &SecurityContext, query: PlanQuery)
    -> Result<RevisionRef, CanonicalError>;
```

Every query carries `catalog: CatalogRef { tenant_id }`. This is the target catalog, not an
assertion of permission or a replacement for the caller's tenant. The provider authenticates the
context and obtains PDP constraints for `plan:read` (resolve/current revision) or `price:read`.
The gate checks the explicit catalog tenant against those constraints; unknown/nonapproved/foreign
price IDs disclose no price. A system-looking subject has no authorization bypass. Configured PDP
or Products unavailability remains 503; definite Products denials retain their canonical error.

`ResolveQuery` names revision, date, optional item and `PricePin { item_id, dimension_value,
price_id }` values. Its result is a matrix of `ResolvedCell { selection, binding }`; an uncovered
cell has `binding=None`. Complete bindings carry entry identity, dimension key/requested value,
dated SKU version/code/name/unit, typed exact-decimal money, invoice inputs with template provenance,
and `via_default`. Legacy entries carry `usage_rating_policy=None`. D-502 stores immutable entry policies and D-503 projects their materialized
content here; the semantic provider is required only at new-entry and publication gates. Missing priced-cell descriptors or a rounding value not representable by the initial
`HalfEven` projection returns the typed `IncompleteCommercialInputs` canonical precondition violation
(type `INCOMPLETE_COMMERCIAL_INPUTS`, subject the missing field).
REST retains nullable descriptors and its existing rounding vocabulary and goldens. Neither transport
computes totals. Both adapters map the shared local snapshot, matrix and invoice inputs explicitly;
dated Products reads occur only after the local read transaction finishes.

`current_revision` catches up a due scheduled revision using the existing atomic switch/audit/outbox
path before reading `published_rev`. A future revision stays scheduled. Retries announce a switch only
once; no running ticker is required. It is classified `SafeRead` (no commercial command or idempotency
key), although catch-up may persist the already-approved switch. Other read methods write nothing.

The digest helpers implement restricted RFC 8785 canonical JSON, wrapped as `{domain,payload}` before
SHA-256. Domains delivered here are `pricing.money.v1`, `pricing.bindings.v1`, `pricing.policy.v1` and
`pricing.template.v1`. Money payload is `{currency, model, minimum_fee}` with every model operand,
excluding IDs, all dates, closing observations and its own digest. Binding payload is
`{plan_id, revision_id, bindings:[{selection,binding}]}` and contains every declared binding field,
including entry, requested dimensions, dated unit, complete price observation, policy identity/version/
content and invoice template/digest/provenance. Policy payload is its content, excluding record ID and
version. Template payload is the exact string. `selected_bindings_digest` rejects duplicate, unknown,
uncovered or inconsistent selections, then sorts by item UUID and requested dimension (null first).

All numeric meaning uses strings: normalized plain decimals, base-10 integer versions, lowercase UUIDs
and lowercase hexadecimal digest references. Optional fields are explicit null. Objects sort keys by
UTF-16 code units; arrays preserve order (tier order is significant); strings use JSON escaping without
Unicode normalization. Rust types exclude malformed Unicode and numeric JSON from the private encoder;
no public untyped evidence or new REST wire DTO is introduced. Future wire adapters must reject duplicate
keys and invalid typed values before projection. A single checked-in fixture contains canonical text
and SHA-256 vectors verified independently by Rust and Node, including full bindings, policy versions,
changed units/templates, maximum u64, control characters and supplementary Unicode keys. Timestamp
normalization vectors specify UTC RFC3339 with nine fractional digits for later timestamp-bearing types;
Task 1 itself introduces no acceptance or BillingTerms methods.

## 6. Data Model

Resolution is a per-item matrix of default and value chains (D-420) with each item's SKU version and resolved invoice inputs (D-421); it carries no totals, and the active promotion (id, version) is deferred with promotions (D-409). The resolve response, field by field (the golden contracts freeze it):

| Level | Field | Content |
| --- | --- | --- |
| revision | plan_revision_id, plan_id, rev_no, state | The revision resolved; state is published, superseded or scheduled, as the revision reads today (D-454). |
| revision | book_id, currency, currency_minor_digits | The revision's book, its currency and that currency's scale (domain::book::minor_digits). |
| revision | rounding_policy | The tenant default_rounding: half_up, half_even, half_down, up or down since D-437; a value stored before it reads as stored. |
| revision | date | The date resolved (YYYY-MM-DD). |
| revision | items | One per item of the revision, or the one item_id names. |
| item | item_id, sku_id | The item and its SKU; since D-467 an item carries no treatment, included_qty or qty_min. |
| item | price_book_entry_id, charge_kind, period, model | The item's entry and its key, model included (the entry's, fixed for its life, D-427); null for a legacy item stored without an entry (D-467), which has no chains. |
| item | usage_rating_policy | Optional typed entry policy: policy_id, exact version string, digest and content; null for legacy/non-usage entries (D-503). |
| item | sku_version | { published_version, effective_from } of the SKU version in force on date; null when Products has no version on that date or does not know the SKU. |
| item | invoice_line_template | { value, source }: the entry's invoice_line_override (source entry), else the SKU version's template (sku), else the tenant template for the charge kind (tenant; an item without an entry takes its SKU version's type); { null, null } when none. |
| item | gl_code | { value, source }: the SKU version's (sku), else the tenant default_gl (tenant), else { null, null }. |
| item | tax_category | { value, source }: the SKU version's (sku), else the tenant default_tax_category (tenant), else { null, null }. |
| item | billing_timing | { value, source }: the SKU version's (sku), else the tenant default_timing (tenant); PRD AC #13. |
| item | meter | { usage_type_ref, unit } of that SKU version; both null without a version. |
| item | chains | The default chain first, then each value registered today for the entry's dimension key in the registry's order, then any other value a pin names (a removed value still resolves for its pin). |
| chain | dim_value | The value; null for the default chain. |
| chain | uncovered | True when neither the value's own chain nor the default binds on date; binding is then null. Never a refusal, never an invented price. |
| chain | binding | The price bound for the period, or null. |
| binding | price_id | The bound price. |
| binding | dim_used | The chain the bound price belongs to: the value, or null for the default chain. |
| binding | pinned_from | The pin the renewal walk started from; null for a signup. |
| binding | price, min_fee | The price's money object as stored, in the item's model (the binding carries no model of its own, D-427): { amount } for flat; { rate } for per_unit; { tiers: [{ up_to, rate }] } for graduated and volume, the last up_to null; { package_size, package_price } for package. Every amount is exact decimal text; min_fee is null when the price has none. |
| binding | eligibility | all or new. |
| binding | effective_from, effective_to, temporary_until | The stored window and the temporary end, if any. effective_to is for information: a successor's start sets it, including a new successor that a pinned subscription does not take. |
| binding | ends_on | Where the binding ends for its holder (D-425): temporary_until for a temporary price, the stored end of an explicitly closed price, null when it has none. A consumer slices a period at ends_on, never at effective_to. |
| binding | keep_for_bound | Whether the price is kept for pinned subscriptions (the predecessor of a new price). |

In the served OpenAPI, the revision's state, charge_kind, period, model, each input's source and eligibility are enums of exactly the tokens above; rounding_policy stays a string, because no CHECK guards default_rounding (D-439). The pinned price read's charge_kind, period, model and eligibility are the same enums.

The pinned price read returns one approved price's stored facts with its entry's SKU, charge kind, period, model (D-427), book and currency (D-422). Pricing prices are read forever; consumer pins persist outside this gear. Events use toolkit outbox envelopes, not a second pricing schema.

Tenant-scoped parent validation is required even where foreign keys use entity ids. Never substitute a
cross-gear read for transactional local ownership/version guards. Approved money and historical pins survive.

## 7. Events & Alarms

Core payloads (camelCase on the wire): PricesPublished { book_id, unit_id, prices[] { price_id, price_book_entry_id, dim_value (null is the default chain), effective_from, effective_to, eligibility, state (`approved` or `cancelled`; additive, absent from an older event) }, actor_ref } (prices[] holds every price whose window or state the apply changed: the unit's prices, each predecessor whose end moved, a cancelled price and an ended price; never a cancel or end row, D-520, D-521), ApprovalUnitDecided { unit_id, kind, state (approved, rejected or withdrawn), generation, actors[] }, PriceBookEntryReferenceLost { price_book_entry_id, sku_id, reservation_id, actor_ref }, and in phase 3 PlanRevisionPublished { plan_id, revision_id, rev_no, book_id, superseded_revision_id (null for a first publication), unit_id, actor_ref } about the plan; each also names its tenant_id. Later payloads name the published revision, retired plan, promotion id/version or migration request/target/subscriptions. The tenant is an envelope fact. The envelope carries no correlation id (trace_parent is unset); an event joins its audit rows through the subject ids it names (unit_id, price_book_entry_id), which the same transaction audits with the request's correlation id. No SkuChanged subscription is required by phase 2.

Every event is a broker TypedEvent of source bss-pricing on the topic `gts.cf.core.events.topic.v1~cf.bss.pricing.catalog.v1`, and the bound producer prepares every type at bind (a broker that lacks one fails the boot). Type ids are `gts.cf.core.events.event.v1~cf.bss.pricing.<name>.v1~`; subject types are `gts.cf.core.events.subject.v1~cf.bss.pricing.<subject>.v1~` (a subject type names a kind of entity, so it is a GTS type id: the broker refuses one without the trailing `~`).

| Event | `<name>` | Subject | Trigger | Phase |
| --- | --- | --- | --- | --- |
| PricesPublished | `prices_published` | `price_book` (the book) | A `prices` unit is applied: quorum 0 at submit or publish-changes, or the approving vote. | 2 |
| ApprovalUnitDecided | `approval_unit_decided` | `approval_unit` (the unit) | Every terminal transition of a unit of any kind: approved (applied), rejected or withdrawn. | 2 |
| PriceBookEntryReferenceLost | `price_book_entry_reference_lost` | `price_book_entry` (the entry) | A rereserve of an entry whose reservation Products released ends refused (the SKU is fenced, retiring or retired). | 2 |
| PlanRevisionPublished | `plan_revision_published` | `plan` (the plan) | A `plan_revision` unit is applied: the revision is published, its predecessor superseded and `published_rev` advanced. A revision approved before its sale date is announced at its switch on that date instead, once, by the job or the door that persists it (D-449, D-450). | 3 |
| PlanReferenceLost | `plan_reference_lost` | `plan_item` (the item) | A plan item's attach (a copied item, D-413) or rereserve ends refused. | 3 |
| PromotionPublished | — | — | Deferred with promotions (D-409). | — |
| PlanRetired, SubscriptionMigrationRequested | — | — | Deferred with retirement and migration requests (D-410). | — |

Audit and outbox inserts use the same mutation transaction; retry is lifecycle-managed and observes shutdown.

## 8. Definitions of Done

The sole definitions live in [features/read-contract-events.md](../features/read-contract-events.md):

- `cpt-cf-bss-pricing-dod-resolve-matrix` — Full chain resolution matrix.
- `cpt-cf-bss-pricing-dod-renewal-all-new` — Renewal walk and eligibility.
- `cpt-cf-bss-pricing-dod-binding-sku-version` — Descriptors from the dated SKU version.
- `cpt-cf-bss-pricing-dod-price-read-forever` — Durable pinned-price read.
- `cpt-cf-bss-pricing-dod-period-slices` — Period boundary semantics. (not built, D-415)
- `cpt-cf-bss-pricing-dod-quote-totals` — Studio quote calculation (not built, D-415).
- `cpt-cf-bss-pricing-dod-events-typed-outbox` — Typed transactional domain events.
- `cpt-cf-bss-pricing-dod-consumer-golden-contracts` — Frozen consumer golden responses.

## 9. Acceptance Criteria

1. PRD AC #18 / `cpt-cf-bss-pricing-dod-resolve-matrix`: Given one item with EU/default prices, when resolve runs then both inputs are returned; an uncovered chain is explicit and no quantity total appears.
2. PRD AC #18 / `cpt-cf-bss-pricing-dod-renewal-all-new`: Given pinned 10 → all 12 → new 15, when renewal resolves then it chooses 12 and signup 15; a forged foreign-chain pin is refused.
3. PRD AC #18 / `cpt-cf-bss-pricing-dod-binding-sku-version`: Given an October 1 GL change already applied to the current SKU, when September resolves then it binds the earlier version; October binds the new one and prior pins do not change.
4. PRD AC #19 / `cpt-cf-bss-pricing-dod-price-read-forever`: Given a closed price id from an old invoice, when read then its original money is returned; unknown/foreign ids reveal no price.
5. PRD AC #20 / `cpt-cf-bss-pricing-dod-period-slices` (not built, D-415): Given a temporary price ending October 11 inside October 5–November 5, when preview runs then two slices appear; their common-price floors are not charged twice.
6. PRD AC #20 / `cpt-cf-bss-pricing-dod-quote-totals` (not built, D-415): Given valid quantities and a promotion, when quote runs then totals apply the prorated floor and the promotion afterward (no included quantity since D-467); invalid quantities fail without changing pins.
7. PRD AC #14 / `cpt-cf-bss-pricing-dod-events-typed-outbox`: Given approve/reject/withdraw/quorum-zero outcomes, when committed then each has its terminal event and only successful apply has its domain publication; rollback has neither.
8. PRD AC #19 / `cpt-cf-bss-pricing-dod-consumer-golden-contracts`: Given stored contract fixtures including negative tenant/uncovered cases, when either backend serves the public paths then responses match; a shape drift fails the contract gate.

## 10. Non-Functional Considerations

All paths enforce tenant isolation, deny-by-default authz and append-only attribution. Mutations use conditional
versions, required POST replay and PATCH/PUT preconditions; PostgreSQL serializable chain changes and SQLite
writer serialization preserve the same invariants. A failed audit/outbox write cannot leave a committed act.
No timeout releases a live reference. Review and apply retain typed database failures for bounded retry.
Implementation gates cover both backends and route censuses; document gates cover toc, language and identifier ownership.

### Durable commercial persistence — Task 5a (D-505)

The SDK declares `PricingAcceptanceV1::{acceptance, hold}` and
`SellabilityV1::{check, check_fulfilment}` with SecurityContext and the plan's exact typed arguments
and results. CommandMeta carries only the idempotency key. Caller tenant/id is an authenticated
provider input, never a supplied command field. Signature availability does not imply a registered
provider: PDP, Contract IR and ClientHub delivery remain Task 5b; acceptance commit orchestration is
Task 5c; hold/live eligibility delivery is Task 6.

Migration 19 creates `pricing_acceptance`, `pricing_hold` and `pricing_commercial_command` on SQLite
and PostgreSQL. Each entity is Scopable by catalog tenant and row identity. The repositories require
AccessScope for every insert/read and additionally filter the explicit catalog tenant. The acceptance
business key is `(tenant_id, order_id, order_version, line_id)`; the hold key is
`(tenant_id, acceptance_id)`; the command key includes catalog tenant, caller tenant/id, operation and
idempotency key. Reusing a key under another caller is a different scope. A hold references its
acceptance by a composite tenant-qualified FK. Command kind/id selects exactly one of two nullable
FK targets guarded by a CHECK, preventing dangling or cross-tenant receipt references.

The acceptance primary key indexes tenant/acceptance lookup. Its business unique index indexes order
lookup; the hold unique index and command target indexes support receipt recovery. Exact u64 order
versions use bounded canonical decimal strings. Timestamps use fixed-width UTC nanosecond strings,
including relational columns, for identical precision on both databases. Receipt JSON is stored as
TEXT without database JSON reformatting. No expiry column or cleanup path exists for commands.
`hold_until` is an eligibility boundary, never a deletion deadline.

`infra/commercial_terms/wire.rs` owns versioned, typed persistence decoding, separate from Task 4's
new-sale BillingTerms validation adapter. Acceptance schema 1 stores the full query, including the
BillingTerms snapshot's own schema version, accepted/deadline timestamps, digests and all selected
bindings. Hold schema 1 stores its frozen bindings and activation; the retained acceptance supplies
the referenced BillingTerms. The v1 reader is frozen: a later additive format gets another explicit
version reader without changing stored digest meaning or supplying missing historical fields.
Exact decimal/time adapters reject lossy input. Receipt reading does not recalculate digests, choose
anchors, replace descriptors or require currently sellable catalog content.

Insert-or-get executes scoped INSERT ON CONFLICT DO NOTHING against the specific unique business
key, then rereads and compares the winner. A different request digest yields ACCEPTANCE_MISMATCH;
a different command payload/target yields IDEMPOTENCY_CONFLICT. Holds also compare first activation.
The caller owns the transaction and bounded contention retry; atomic acceptance/command/audit writes
and authorization-before-replay are application-layer work in the subsequent chunks. No public
commercial endpoint or partially implemented success response is introduced by persistence alone.

### Commercial provider boundary — Task 5b (D-506)

ClientHub now registers three independent SDK capabilities. PricingReadV1 keeps only
resolve/price/current_revision. SellabilityProvider and PricingAcceptanceProvider share one
CommercialTermsService with AuthoringState, PolicyEnforcer, Clock and SellerHoldPolicy. Explicit
Contract IR marks check/hold IdempotentWrite; every other C01 method is SafeRead.

| Method | PDP resource/action | Current delivery |
| --- | --- | --- |
| SellabilityV1::check | acceptance:create, catalog collection | Atomic acceptance/command/audit transaction (D-507) |
| SellabilityV1::check_fulfilment | acceptance:read, receipt id | Fresh original-binding eligibility (D-508) |
| PricingAcceptanceV1::acceptance | acceptance:read, receipt id | Stored immutable receipt read |
| PricingAcceptanceV1::hold | acceptance:hold, receipt id | Atomic frozen first hold and command replay (D-508) |

The resource label is `gts.cf.bss.pricing.acceptance.v1~`. SecurityContext supplies caller identity;
CommandMeta has only an idempotency key. The requested seller/catalog must belong to the PDP scope,
and the repository filters that exact tenant in addition to all returned tenant/resource predicates.
Even a two-catalog grant cannot reveal another catalog's receipt through the requested one. Receipt
reading preserves the 5a versioned snapshot and never re-evaluates expiry or live sale policy.

The gear's `seller_hold_policy` defaults to `{version: 1, duration_seconds: 86400}`. Explicit policies
require both positive integer fields; startup rejects invalid values before provider registration.
No issued receipt changes when deployment configuration changes. `infra::clock::{Clock,SystemClock}`
reuses the reference recovery Clock/WallClock implementation, including its harmless default jitter
hook; it introduces no second server-time source. Tests inject an explicitly advanced FixedClock.
Acceptance and hold sample it inside the final transaction; receipt reads require no clock observation.

Canonical failures preserve typed commercial reasons: invalid argument 400, conflict 409, permission
denied 403 and authorized not found 404. Configured PDP/storage outages are 503; a missing required
provider names its UNCONFIGURED_DEPENDENCY. Tests pin identity,
actions, multi-tenant filtering, unknown ids, byte-identical restart reads and separate registrations.
The complete G3 controller gate remains required before public release.

### Acceptance transaction — Task 5c (D-507)

The shared service now implements check in the specified order: authorize catalog/caller, canonical
request digest, scoped command replay, business-identity replay/command attachment, due promotion
and local snapshot, detached resolution/live SKU/meter reads, complete-selection/profile/digest
validation, then serializable generation recheck and receipt commit. New resolution also requires
plan/price read grants. Exact authorized replay never consults live dependencies or refreshes TTL.

The snapshot records all revision/price generations and the local inputs used to project bindings.
Recheck covers entries, policies, dimensions and settings too. A local change rolls back and uses
the G2 bounded capture loop; repeated change returns ResolutionChanged. Server time is sampled only
after rechecking command/business uniqueness and generations in the final transaction. A revision
becoming due during provider work forces recapture. Price windows are UTC and half-open; temporary
promotional prices and wrong seller-policy versions refuse new acceptance.

The same commit freezes entry IDs, exact policy references/content, prices, SKU descriptors, invoice
inputs and supplied BillingTerms; accepted_at plus the configured duration yields hold_until. It
inserts the immutable receipt, successful command mapping and local audit with observed meter
provenance. Targeted insert-or-get rereads unique winners; no persistent in-flight claim is needed.
A crash before commit rolls back all three records; restart after commit returns the original v1
receipt even with providers down. Changing seller TTL or selecting another entry in a successor
revision cannot rewrite it. D-508 supplies holds and live fulfilment eligibility.

### Frozen holds and fresh fulfilment — Task 6 (D-508)

check_fulfilment always performs an authorized acceptance lookup, exact terms-digest/tenant-axis/
current-market comparison, frozen BillingTerms compatibility, live Products retirement check and
original-price metadata reads. Retirement, explicit close/end, temporary end and server-time TTL
expiry refuse eligibility. Deprecation, off-sale and revision supersession are allowed. There is no
renewal resolution, successor traversal, historical meter refresh or current descriptor substitution.
A successor-induced effective_to is not an end. Future original ends bound valid_before at 00:00Z.
Both server time and requested activation must precede price ends and hold_until; activation cannot
precede original price effectiveness or accepted start_at.

hold authorizes and finds the exact acceptance under the full PDP scope before exact command replay.
Its related hold/command rows use the PDP-derived tenant_only scope after that parent check, since
their primary keys are different resources. A revoked receipt-ID grant cannot replay an old command.
Without a replay, it performs these fresh checks,
rereads original price generations and samples Clock inside its serializable commit transaction.
Hold and command mapping commit together. Unique-key winners preserve the original hold; another
key with identical activation passes fresh checks and cannot extend TTL. The first chosen activation
instant is pinned. Another activation conflicts, while a 10:00 submission may first hold at 10:03.
Exact successful commands replay after expiry; new keys and check_fulfilment cannot use that replay
as fresh eligibility. Local drift recaptures within the existing bounded retry budget.

Eligibility is never a reusable admission token. Subscriptions must fence its committed order
version and attempt, revalidate immediately before its first activation intent, and own actual
served intervals. Entry ID, immutable policy, original money, SKU v3 descriptors and invoice inputs
survive successor prices and revisions. Historical receipt reads remain independent of eligibility.


D-511 tightens the command boundary: check reads each selected price under its compiled price-read
scope; a price outside that scope yields 403, as do fresh hold/check_fulfilment reads. A foreign
receipt reference remains 404. At commit, start_at >= hold_until yields
ActivationOutsideAcceptedWindow before any acceptance or command is stored. SDK command keys use
the same bounded printable-ASCII invalid-argument rule as REST. Products contention maps to 503.
