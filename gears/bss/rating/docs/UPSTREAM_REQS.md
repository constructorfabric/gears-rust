<!-- CONFLUENCE_TITLE: [BSS]: Rating — Upstream Requirements -->
<!-- Related: ./DESIGN.md, ./DECISIONS.md, ./DECOMPOSITION.md, ./features/ | Owners: BSS Rating team -->

# UPSTREAM_REQS — Rating

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Requesting Gears](#12-requesting-gears)
  - [1.3 Status legend](#13-status-legend)
- [2. Requirements](#2-requirements)
  - [2.1 Usage Collector](#21-usage-collector)
  - [2.2 Usage emitter and IRM](#22-usage-emitter-and-irm)
  - [2.3 Types registry](#23-types-registry)
  - [2.4 Pricing](#24-pricing)
  - [2.5 Products registry](#25-products-registry)
  - [2.6 Subscriptions](#26-subscriptions)
  - [2.7 Billing](#27-billing)
  - [2.8 bss-ledger](#28-bss-ledger)
  - [2.9 Finance and FX](#29-finance-and-fx)
  - [2.10 Contracts and Promotions](#210-contracts-and-promotions)
  - [2.11 Orders Lifecycle](#211-orders-lifecycle)
  - [2.12 Platform infrastructure](#212-platform-infrastructure)
  - [2.13 Account management and tenant resolver](#213-account-management-and-tenant-resolver)
  - [2.14 Shared fixtures](#214-shared-fixtures)
  - [2.15 Closed and superseded asks](#215-closed-and-superseded-asks)
- [3. Seam Atlas v2 baseline status](#3-seam-atlas-v2-baseline-status)
- [4. Priorities](#4-priorities)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

This register states what Rating needs from gears and systems it does not own, and what each of
them provides today. It replaces the former `SEAMS.md` contract inventory. Every row is checked
against code first and documents second. It was re-verified on 2026-10-08 against this branch's
base, upstream `main` at `a35dfc3e2` (the usage-collector redesign `5de85f067`, the PriceBook pricing
model and SKU registry `e81cac81d`, the implemented event broker `c7de7b80a`); one later upstream
decision (products P-D-265 at `95eb3f46f`) is noted where it applies.

[`DESIGN.md`](./DESIGN.md) states the architecture and labels every external contract CURRENT,
TARGET, MIGRATION or OPEN DEPENDENCY (DESIGN §0). This document owns the **evidence** behind those
labels and the **asks** (`upreq` IDs) that move a dependency from its current state to the TARGET
Rating is designed against. [`DECISIONS.md`](./DECISIONS.md) owns the decisions (`R-*` open,
`T-D-*` decided); an ask here never decides anything and never marks a proposed contract as built.
[`DECOMPOSITION.md`](./DECOMPOSITION.md) §3 states which feature each ask blocks.

Row identifiers of the former inventory are kept so existing citations still resolve: `A-*`
platform, `U-*` usage collector and emitter, `P-*` and `G-4`/`G-5` pricing and products, `S-*`
subscriptions, `L-*` billing and ledger, `F-*` FX, `G-*` other gears, `H-*` fixtures, and `J-*` the
required changes, now carried as aliases on the `upreq` IDs below.

### 1.2 Requesting Gears

| Requesting gear | Why it needs the targets |
|---|---|
| `rating` (`gears/bss/rating`) | Rates usage and commercial facts into exact, replayable results for Billing. It needs a replay-safe usage feed and its corrections from the usage collector; usage type declarations from types-registry; coverage and inventory evidence from a usage emitter and IRM; bindings, money and usage rating policy from Pricing; derived usage declarations from Products; commercial facts, attribution and scope proofs from Subscriptions; a delivery consumer and period ownership from Billing; pinnable FX rates from Finance; contract overlays, commitment pools and coupon snapshots from Contracts and Promotions; event contracts and a coordination backend from the platform; a tenant-deletion signal from account management. |

| Target gear | Asks (§2) | Blocks |
|---|---|---|
| usage-collector | §2.1 | usage rating (R-01) |
| usage emitter (owner unassigned), IRM | §2.2 | usage finalization (R-21) |
| types-registry | §2.3 | typed fold/unit reads |
| pricing | §2.4 | live rating needs only the read grant; dimensional pricing; proration confirmation (R-30) |
| products registry | §2.5 | usage rating (R-31: derived declaration read) |
| subscriptions | §2.6 | all rating (R-03, R-20, R-25): facts with pins, plan revision, quantity and billing terms |
| Billing (no gear) | §2.7 | invoicing (R-04, R-05) |
| bss-ledger | §2.8 | none (Billing posts) |
| Finance / FX | §2.9 | cross-currency billing, `delay_only` finalization |
| Contracts, Promotions | §2.10 | contract overlays, commitments, coupons (R-11) |
| Orders Lifecycle | §2.11 | pre-purchase evaluation (R-24) |
| platform (event broker, Cluster Plane) | §2.12 | first deployment (R-27); event paths |
| account-management | §2.13 | tenant deletion |
| shared fixtures | §2.14 | none (the joint corpus was deleted; pricing golden contracts replace it) |

### 1.3 Status legend

Evidence status of a row:

| Status | Meaning |
|---|---|
| **CONFIRMED** | Exists in code and matches what Rating needs. |
| **PARTIAL** | Exists in code but lacks something Rating needs. |
| **DOCUMENTED** | Specified in the owner's accepted documents; not implemented. |
| **MISSING** | Specified in a dependency's docs (or needed by Rating) but not implemented. |
| **CONFLICTING** | Documentation and code, or two specifications, disagree. |
| **ASSUMED** | The dependency has no implementation at all; the contract exists only in design prose. |
| **PROPOSED** | Exists only in the Seam Atlas (or as a Rating requirement); no owner document adopts it. |
| **ABSENT** | Neither code nor any owner document. |
| **DOC/CODE DRIFT** | A dependency's code contradicts its own documents. |

An ask (`upreq`) is open until its owner adopts and delivers it; the checkbox stays unchecked until
then. "Requested" means Rating asked; nothing here is agreed unless the row says so.

## 2. Requirements

### 2.1 Usage Collector

`gears/system/usage-collector`. Its documents describe a breaking pre-1.0 **target V1** (PRD,
DESIGN §3.3, 15 accepted ADRs, JSON schemas); its code — SDK, gear and plugins — still implements
the superseded point-record model. The collector records this itself: "This document is normative,
and the code is not a second source" (DESIGN:2296); the REST YAML is `x-contract-status:
unreleased`. References are to the collector's documents unless marked "code".

| # | Contract | Direction / mode | Fields / semantics | Evidence | Status |
|---|---|---|---|---|---|
| U-1 | **Usage feed** `read_usage_feed(ctx, &FeedSubscription, FeedStart<&CursorV1>, until: Option<&CursorV1>, limit) -> FeedPage`; REST `GET /usage-collector/v1/feed` | UC → Rating, pull | subscription = 1..100 GTS types; `FeedStart::{Oldest, After(cursor)}`; `until` bounds a replay (identical entry for entry); limit 1..1 000; pages carry only settled entries, nothing appears behind a returned cursor; prefix-stable, **no snapshot token**; one deterministic plugin-chosen order, only "an invalidation follows its target"; `next_cursor` never null on a live read | DESIGN:536-600, 919-932, 1127-1142, 1355; ADR-0011; YAML:315-371 | **DOCUMENTED**, not implemented — code `UsageCollectorClientV1` (`usage-collector-sdk/src/api.rs:17-110`) has ten methods (create, create batch, get, aggregate, list, deactivate, and four usage-type methods); no `FeedPage`, `FeedStart`, `read_feed_page` or `entry_type` in any `.rs` file; the REST YAML is `x-contract-status: unreleased` (`docs/usage-collector-v1.yaml:35`) |
| U-2 | Entry identity + dedup | UC | dedup identity `(tenant_id, gts_type_id, idempotency_key, window_start, window_end, entry_type)`; `id = UUIDv5(NS 56313026-…, that tuple)`, reproducible offline; consumers MUST use it as their dedup and reference key | ADR-0007; DESIGN:578; PRD:547 | DOCUMENTED; code: UUIDv5 over `(tenant, gts_id, created_at, key)` (`id.rs:41-58`) — DOC/CODE DRIFT |
| U-3 | Corrections | UC → Rating | invalidation = entry with `entry_type = invalidation`, faithful copy, server-stamped `invalidates`, `reason_code`, quantity echoed; at most one per record; no invalidation of an invalidation; replacement = invalidation + fresh record under a new key, **not atomic** — consumers must not treat the in-between state as settled | ADR-0010; DESIGN:579-594; PRD:436-438 | DOCUMENTED; code: `deactivate_usage_record` status flip + `corrects_id` compensation (being removed) |
| U-4 | Fold; **only `SUM` is chargeable**; levels pre-integrated at the emitter | types-registry declaration | consumer applies the declared fold; excludes both entries of a withdrawn pair | ADR-0009, ADR-0011; PRD:288 | **CONFLICTING** with Rating T-D-17 / pricing D-44 — R-06 (Atlas R1 agrees with UC) |
| U-5 | Aggregate / query paths | UC | eventually consistent, never a charging input ("a consumer that computes money reads entries, not aggregates") | ADR-0011 | not used by Rating |
| U-6 | Retention floor = backfill window (90 d) + replay horizon (35 d) = 125 days from `window_end`; refusal `InvalidArgument(CursorBeyondRetention)` depends on what the store holds, not cursor age; a polled cursor is never refused | UC | — | PRD:683-699; DESIGN:927-932 | DOCUMENTED; plugin today: one deployment-wide TTL. Rating: a restart from `Oldest` does not repair entries lost before capture, and the feed's order is not event-time order, so Rating records a **source loss** that blocks finality of the affected scope under every evidence mode until reconciliation (U-12) or a backfill (U-13) proves repair (T-D-62, DESIGN §4.6) |
| U-7 | Commercial identity is the consumer's | UC | `resource_ref {resource_id, resource_type}`, `subject_ref?`, metadata | PRD:513, 1072 | CONFIRMED by design — **CONFLICTING** with Subscriptions S-PRD:360 (R-25) |
| U-8 | Interval entries | UC | half-open `[window_start, window_end)` UTC, µs; equal bounds = point event; `accepted_at` and `origin` server-assigned; quantity decimal string ≤ 28 fractional digits (ADR-0013); rounding belongs to the rating consumer (ADR-0013:209) | YAML:649-667, 787-889; DESIGN:547-586 | DOCUMENTED; code record is `UsageRecord{id, gts_id, tenant_id, resource_ref, subject_ref?, metadata, value: Decimal (string), idempotency_key, corrects_id?, status, created_at}` — field `value`, a `created_at` instant only (`usage-collector-sdk/src/models.rs:699-750`); the ClickHouse plugin stores `Decimal128(9)` and refuses more precision (`plugins/clickhouse-usage-collector-plugin/src/infra/storage/entity.rs:116`) against 28 documented — DOC/CODE DRIFT |
| U-9 | Usage type declarations in types-registry: `aggregation_fold`, `canonical_unit`, `retention`, optional `nominal_sampling_interval`; the collector serves no type reads | types-registry → consumers | — | ADR-0008; `schemas/usage_record.v1.schema.json:44-65`; DESIGN:428-437 | DOCUMENTED; code: the collector's own plugin catalog `UsageType{gts_id, kind: Counter\|Gauge, metadata_fields}` with no unit (`usage-collector-sdk/src/models.rs:574-589`), validated at ingest; Products' usage-type adapter depends on those Phase 1 routes, which the target removes (`gears/bss/products/products/src/infra/usage_types.rs:123, 189, 274`) — DOC/CODE DRIFT |
| U-15 | Raw usage types are the inputs of Products' derived usage types; a usage SKU never sells a raw GTS type (§2.5 G-10) | UC → Rating via Products | Rating counts raw inputs and evaluates the derived meter (T-D-76) | products P-D-259; `gears/bss/products/products-sdk/src/derived.rs:69-81` | CONFIRMED (Products side) |
| U-12 | Reconciliation metadata `GET /usage-collector/v1/reconciliation` (`accepted_count`, quantity summary, `accepted_at_watermark`, `window_end_watermark`) | UC → operators | REST-only, operator-scoped, no trait; watermarks "prove nothing about completeness" | DESIGN:1052, 1356-1362; YAML:373-424, 1238-1320; ADR-0011 | DOCUMENTED; Rating's service-identity access UNKNOWN (`…-upreq-usage-reconciliation-access`). It is the evidence that narrows or resolves a source loss; without access only an audited operator resolution can |
| U-13 | Backfill route `backfill_usage_records` / `POST /records/backfill` | emitter → UC | live path refuses periods ending > 48 h ago; backfill up to 90 d, `origin = backfill` | DESIGN:665-671, 1082-1086 | DOCUMENTED |
| U-14 | Consumer obligations | Rating | dedup by entry id across at-least-once delivery; persist the rated usage identity per charge; replay from `Oldest` after a widened authorization scope; stall detection is the consumer's | PRD:547, 669, 749, 1503-1504 | adopted by Rating: every entry is captured raw before it is interpreted (T-D-61); one canonical source identity per feed subscription (its sorted type set and Rating's grant) names the checkpoint, lock and cursor; a changed type set or grant is a new source from `Oldest` (DESIGN §3.7) |

#### Implement the documented V1 feed

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-upreq-usage-feed-v1`

The usage-collector **MUST** implement its own documented target V1 (DESIGN §3.3): `read_usage_feed`
with the opaque continuing cursor and `until` (U-1), interval entries (U-8), invalidation entries
(U-3), the dedup identity behind the entry id (U-2), the backfill route (U-13) and the retention
floor and `CursorBeyondRetention` refusal (U-6). Nothing in it needs redesign for Rating; the Atlas
`UsageFeedV1` is not requested (the Atlas Plus overlay withdraws it).

- **Rationale**: polling `list_usage_records` by `created_at` misses late-accepted records behind the
  cursor and cannot see invalidations; a status flip is an invisible mutation of financial input.
- **Source**: `gears/bss/rating` — DESIGN §3.6 Flow A, Flow B; feature [`04-usage-intake`](./features/04-usage-intake.md).
- **Status**: DOCUMENTED, not implemented (U-1…U-3, U-8). **BLOCKER** for usage rating.
- **Register aliases**: J-1; DECISIONS R-01.
- **Acceptance signal**: a Rating feed reader passes the collector's own feed conformance tests and
  Rating's capture vectors V01, V03 and V04 against the implemented trait instead of the contract fake.

#### Reconciliation access for Rating's service identity

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-upreq-usage-reconciliation-access`

The usage-collector **MUST** let Rating's service identity read its reconciliation metadata (U-12)
per `(tenant, GTS type, day)`, either on the existing REST route or through a trait over it.

- **Rationale**: it is the authoritative evidence that narrows or repairs a source loss (T-D-62);
  without it a loss can only be closed by an audited operator resolution.
- **Source**: `gears/bss/rating` — DESIGN §4.6; feature [`11-operations`](./features/11-operations.md).
- **Status**: DOCUMENTED for operators; consumer access UNKNOWN.
- **Register aliases**: J-17.
- **Acceptance signal**: Rating's reconciler reads the metadata under its service identity and
  narrows an open source loss in an integration test (vector V06).

### 2.2 Usage emitter and IRM

The emitter that would declare coverage has **no owner** (Atlas Plus overlay ticket T6: IRM excludes
metering; the collector never judges completeness). IRM (`gears/infrastructure-resource-manager`) is
**documents only**.

| # | Contract | Direction / mode | Fields / semantics | Evidence | Status |
|---|---|---|---|---|---|
| U-10 | Coverage declarations (`MeterCoverageDeclared`, `MeterCoverageReadV1`) | emitter → Rating | count, sum, record-set digest, `final` | Atlas C05 only; collector "never judges completeness" (PRD:749) | **ABSENT** — no owner (overlay T6) |
| U-11 | Historical inventory barrier (`ResourceHistoryV1`), `ResourceReady`/`ResourceDeleted` | IRM → Subscriptions | resources overlapping a period incl. deleted | Atlas C05/C09; IRM DESIGN `:816-827` (lifecycle event without `ready_at` or sequence until p3) | **ABSENT** (Atlas Plus X10) |

#### Coverage declarations

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-upreq-coverage-declarations`

An owner (to be assigned: a metering-adapter PRD per provider adapter, or an IRM PRD §5.2 amendment)
**MUST** declare final usage coverage per resource and period — `MeterCoverageDeclared` and
`MeterCoverageReadV1` with record count, quantity sum, active record-set digest and a `final` flag
(Atlas C05).

- **Rationale**: a usage window becomes final only on proven-complete inputs (T-D-52); time alone, a
  feed watermark or an empty page proves nothing.
- **Source**: `gears/bss/rating` — DESIGN §4.6; feature [`07-child-evaluation`](./features/07-child-evaluation.md).
- **Status**: ABSENT (U-10); owner unassigned.
- **Register aliases**: J-13 (emitter part); DECISIONS R-21.
- **Acceptance signal**: a coverage declaration from the owner's implementation completes the
  evidence gate for a usage window in an end-to-end test (Atlas F30).

#### Historical inventory barrier

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-upreq-resource-history`

IRM **MUST** provide `ResourceHistoryV1` (resources overlapping a period, deleted ones included) and
lifecycle events with `ready_at` and a sequence, so Subscriptions can seal a usage scope against an
inventory snapshot.

- **Rationale**: a sealed scope (§2.6) cannot prove which resources a window must cover without it.
- **Source**: `gears/bss/rating` — DESIGN §4.6; feature [`07-child-evaluation`](./features/07-child-evaluation.md).
- **Status**: ABSENT (U-11).
- **Register aliases**: J-13 (IRM part); DECISIONS R-21.
- **Acceptance signal**: a sealed `UsageScope` carries an `inventory_snapshot_id` that IRM can
  resolve to the resource set.

### 2.3 Types registry

Raw usage type declarations are documented to live in types-registry (U-9); in code the collector
still serves its own plugin catalog. The registry has only generic schema reads. Derived usage types
— the meters Rating actually prices — are Products' (§2.5); their declaration read is
`…-upreq-products-derived-declaration-read`. The raw-meter semantics provider pricing asks for (E1a)
is documented, not built (`gears/bss/products/products/src/infra/meter_semantics.rs:11-15`).

#### Typed usage type declaration read

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-upreq-usage-type-declaration-read`

types-registry **MUST** expose a typed read of a **raw** usage type declaration
`get_usage_type_declaration(gts_id) -> {aggregation_fold, canonical_unit, accrual_method}` (Atlas
Plus X7).

- **Rationale**: Rating checks that each raw input of a derived meter has the fold and unit the
  derived declaration expects (`DerivedInput.unit`, `granule_fold`); a generic schema read works but
  is untyped.
- **Source**: `gears/bss/rating` — [12 §4.7](./DESIGN.md#contract-12-4-7).
- **Status**: PARTIAL (`TypesRegistryClient` schema reads exist; code still serves the collector's
  plugin catalog, U-9).
- **Register aliases**: J-14.
- **Acceptance signal**: Rating's normalizer reads fold and unit through the typed trait.

### 2.4 Pricing

`gears/bss/pricing` (crates `pricing`, `pricing-sdk`). In pricing code and docs "Tariffs" means
Rating's evaluation core (ADR-0002). Paths below are relative to `gears/bss/pricing/`.

Upstream `e81cac81d` (2026-10-02) replaced the pricing gear with the **PriceBook model**, decided for
Rating as T-D-37 and consumed through Rating's pricing adapter (T-D-73…T-D-78). Rows P-15…P-27
are what `main` provides. Rows P-1…P-14 describe the `CatalogVersion` model this design was first
written against; that code is **gone** — pricing refuses a legacy schema at boot
(`pricing/src/infra/storage/migrations/m0000_pricing_refuse_a_legacy_or_stale_schema.rs:52-77`) —
and they are kept only so earlier citations still resolve.

| # | Contract on `main` | Evidence | Status |
|---|---|---|---|
| P-15 | `PricingReadV1::resolve(ctx, ResolveQuery{catalog: CatalogRef{tenant_id}, revision_id, date, item_id?, pins: [PricePin{item_id, dimension_value?, price_id}]}) -> ResolvedBindings{plan_id, revision_id, cells: [ResolvedCell{selection: BindingSelection{item_id, dimension_value?}, binding: Option<AcceptedBinding>}]}`; REST `GET /bss-pricing/v1/resolve?plan_revision_id=&date=&item_id=&pins=` (D-419). `binding = None` means uncovered, never free. SafeRead, no idempotency key, no totals | `pricing-sdk/src/read.rs:192-231, 262-291`; `pricing/src/api/pricing_read.rs:40-164`; `pricing/src/api/rest/read_contract.rs:65`; registered in ClientHub (`pricing/src/module.rs:153`) | **CONFIRMED** — Rating's pricing input (T-D-73) |
| P-16 | `AcceptedBinding{item_id, price_book_entry_id, dimension_key?, dimension_value?, sku_id, sku_version, sku_code, sku_name, unit?, meter: Option<MeterRef>, price: ImmutablePrice, kind: ChargeKind, recurring_period: Option<BillingCycle>, via_default, usage_rating_policy: Option<UsageRatingPolicy>, invoice: InvoiceInputs}`; `ImmutablePrice{price_id, price_book_entry_id, money_digest, currency, model: PriceModel, minimum_fee?, effective_from, ends_on?, state: Approved \| Cancelled}` | `pricing-sdk/src/read.rs:80-169` | **CONFIRMED** — stored verbatim as Rating's binding of record (T-D-73) |
| P-17 | `PricingReadV1::price(ctx, PriceQuery{catalog, price_id}) -> ImmutablePrice`; REST `GET /bss-pricing/v1/prices/{id}`: an approved price is served forever whatever its window, a cancelled one with state `Cancelled` (D-422, D-520) | `pricing-sdk/src/read.rs:273-282`; `pricing/src/infra/pricing_reads.rs:126-155`; `pricing/src/api/rest/read_contract.rs:101` | **CONFIRMED** — Rating's replay check of `money_digest` |
| P-18 | `PricingReadV1::current_revision(ctx, PlanQuery{catalog, plan_id}) -> RevisionRef{plan_id, revision_id, revision_no}`; catches up due scheduled switches; 409 `PLAN_UNPUBLISHED`. SDK only, no REST route | `pricing-sdk/src/read.rs:234-242, 283-290`; `pricing/src/api/pricing_read.rs:139-153` | **CONFIRMED**; not needed by Rating (the fact names the revision) |
| P-19 | Pins and the renewal walk (D-397, D-420): pricing stores **no** pins — the consumer sends them on each call (`DOCS/DECISIONS.md:2220`). Signup (no pin) binds the price in force on `date` (the value's own chain, else the default chain); renewal walks the pinned chain through approved `all` successors and stops before the first `new` one; a binding is always in force; `keep_for_bound` marks the price a pinned renewal stays on. Publishing a revision never moves pins (D-394) | `pricing/src/domain/resolve.rs:259-341`; `docs/DECISIONS.md:221, 239-241, 441-452, 2218-2220` | **CONFIRMED** — the walk is Pricing's; Rating sends the fact's pins and never advances one (T-D-73) |
| P-20 | `ends_on` (D-425) = `temporary_until`, else the stored `effective_to` of an explicitly closed price, else null; excluded from `money_digest`. "A consumer slices a period at `ends_on`, never at `effective_to`"; at an `ends_on` inside a period the consumer resolves again with the pin on that date (D-397) | `pricing/src/api/pricing_read.rs:229-231`; `pricing/src/domain/resolve.rs:90-95, 330-341`; `docs/DECISIONS.md:241, 504-508` | **CONFIRMED** — Rating's slice point (T-D-73) |
| P-21 | `UsageRatingPolicy{policy_id, version, digest, content}` on every new usage entry, immutable (D-502, D-513, D-514): `rating_window ∈ {BillingCycle, CalendarHour{timezone: Utc}}`, `aggregation_scope ∈ {subscription_line, resource}`, `reset = rating_window_start`, `partial_window = actual_quantity_full_thresholds`, `fold = SUM`. No plan-wide window, no cross-subscription aggregation (D-503, D-509); `CalendarHour` or resource-scoped entries carry no `min_fee` (D-503, D-504); a policy change is a new entry plus a revision | `pricing-sdk/src/terms.rs:24-109`; `docs/DECISIONS.md:1494-1532, 1586-1587, 1920, 2042-2077` | **CONFIRMED** — Rating's window and scope source (T-D-75) |
| P-22 | Money (`PriceModel`): exact `rust_decimal::Decimal` in **major** currency units, JSON strings on REST; `Flat{amount}`, `PerUnit{unit_amount}` (REST `{rate}`), `Graduated{tiers}`, `Volume{tiers}`, `Package{package_size, package_price}` (readable, not saleable, D-504); `Tier{up_to: Option<Decimal> exclusive, rate}`, last band open (D-387); models per charge kind (D-386): usage = per_unit/graduated/volume/package, recurring and one_time = flat/per_unit; `minimum_fee` scale-checked against the currency (`MIN_FEE_INVALID`). Pricing computes no totals, proration, floor or rounding (D-415) | `pricing-sdk/src/read.rs:27-65`; `pricing/src/domain/money.rs:20-55, 75-169`; `pricing/src/domain/price.rs:94-99`; `pricing/src/domain/price_book_entry.rs:45-52` | **CONFIRMED** — kept exact in major units as fractions with the binding's `currency_scale` (T-D-74, T-D-80) |
| P-23 | `InvoiceInputs{template, template_digest, template_source: Entry \| SkuVersion \| SellerSettings, gl_code, tax_category, timing: BillingTiming{Advance, Arrears}, currency_scale, rounding: Rounding{HalfEven}}` on every binding, resolved as of `date` (D-421); prices freeze no descriptors (D-389). The SDK requires them all: one incomplete covered cell, or a tenant rounding other than half_even, fails the whole `resolve` with `INCOMPLETE_COMMERCIAL_INPUTS` | `pricing-sdk/src/terms.rs:112-158`; `pricing/src/api/pricing_read.rs:284-399`; `docs/DECISIONS.md:183, 458-462, 645` | **CONFIRMED** — frozen in Rating's binding copy; a failed call is `incomplete_commercial_inputs` (Rating may resolve per `item_id` to isolate a cell) |
| P-24 | Refusals: 400 `PIN_FOREIGN` (whole request), `PIN_DUPLICATE`, `PINS_TOO_MANY` (> 1 000; a consumer with more splits by `item_id`), `DATE_INVALID`, `QUERY_INVALID`, `ID_INVALID`; 409 `REVISION_NOT_PUBLISHED`, `REVISION_NOT_YET_AVAILABLE`; 404 for an unknown or foreign revision, item or price; 503 `REGISTRY_UNAVAILABLE` when Products cannot answer. An uncovered chain is not a refusal (D-420) | `docs/DESIGN.md:514-517`; `pricing/src/infra/pricing_reads.rs:42-93, 188-198, 384-419`; `pricing/src/domain/resolve.rs:25` | **CONFIRMED** — mapped to `pricing_refused(<code>)` / `pricing_unavailable` (DESIGN) |
| P-25 | Authorization: the PDP is asked for `plan:read` (resolve, current_revision) or `price:read` (price); `CatalogRef.tenant_id` is only a hint inside the PDP constraints; "a system-looking subject has no authorization bypass"; Products reads during resolve run as pricing's system actor (D-424) | `pricing/src/api/pricing_read.rs:50-80`; `docs/DECISIONS.md:492-498` | **CONFIRMED** — Rating calls as `bss-rating.system` with `plan:read` and `price:read`; the deployment grant is `…-upreq-pricing-read-grant` |
| P-26 | Events on topic `gts.cf.core.events.topic.v1~cf.bss.pricing.catalog.v1`: `prices_published`, `plan_revision_published`, `approval_unit_decided`, `price_book_entry_reference_lost`, `plan_reference_lost`; written to the toolkit outbox in the act's transaction and dispatched by `DbProducer` when a broker is registered, otherwise held (D-400, D-455) | `pricing/src/infra/events.rs:1-58, 191-293`; `pricing/src/infra/reference_events.rs:16-80` | **CONFIRMED**; Rating consumes none (pull is sufficient) |
| P-27 | Commercial sale capability: `SellabilityV1::{check, check_fulfilment}`, `PricingAcceptanceV1::{acceptance, hold}`, `AcceptanceReceipt{acceptance_id, request_digest, terms_digest, query, accepted_at, hold_until, bindings}` (immutable, retained), `HeldBindings`; `NewSaleQuery` carries `quantity`, `market`, `term` and the Subscriptions-owned `BillingTerms`. SDK only, used by Orders and Subscriptions (D-504, D-508, D-509) | `pricing-sdk/src/acceptance.rs:42-418`; `pricing/src/module.rs:163,167`; `pricing-sdk/src/terms.rs:160-198` | **CONFIRMED**; not called by Rating — the accepted bindings reach Rating through Subscriptions' facts (§2.6) |
| P-28 | Golden consumer contracts (fifteen files, SQLite and Postgres: `resolve_*`, `price_*`, `price_book_entry_usage`) and SDK seam fixtures (`vm-hour`, `cloudlets-hourly-graduated`, `cloudlets-hourly-volume`, `frozen-acceptance`, `unsupported-terms`) | `pricing/tests/contract/*.json`; `pricing/tests/seam_fixtures/*.json`; `docs/DECISIONS.md:1914-1932` | **CONFIRMED** — Rating's adapter tests and arithmetic vectors read them (cloudlets Q = 8 and Q = 12 hourly give 0.34, not 0.30) |

The SDK binding is narrower than REST: it lacks `eligibility`, `effective_to`, `temporary_until`,
`pinned_from`, `keep_for_bound`, `dim_used` and `version_no` (`pricing-sdk/src/read.rs:80-169` vs
`docs/design/07-read-contract-events.md:164-193`). Rating needs none of them: it slices at `ends_on`
(present), records the pins it sent (so `pinned_from` is derivable), and does no eligibility or
renewal logic of its own. No ask is raised for them.

Rows P-1…P-14 (superseded model, gone on `main`):

| # | Contract | Version semantics | Evidence (pre-PriceBook tree) | Status |
|---|---|---|---|---|
| P-1 | `PricingCatalogClientV1::pin_frontier(ctx) -> Result<Option<PinFrontier { catalog_version, advanced_at }>, CanonicalError>` | `CatalogVersion(u64)` | `pricing-sdk/src/api.rs:17-43` (removed) | **SUPERSEDED** — no code defines it; pins replace cohorts and catalog versions (D-397); table `pricing_pin_frontier` refused at boot |
| P-2 | Plan / overlay documents at a catalog version (`tierAggregationWindow`, `billingAnchorPolicy`, `prorationBasis`, descriptor set, `usageCounterOnPlanChange`, `periodFloorCaps`) | read model per catalog version | `domain/projection.rs` (removed) | **SUPERSEDED** by P-15/P-16 |
| P-3 | Future-only effective dating and historical immutability | — | — | **SUPERSEDED** in form; on `main` a new draft cannot start in the past (`WINDOW_START_IN_PAST`, `pricing/src/domain/price.rs:72-74`) and an approved price is immutable by id (P-17) |
| P-4 | Canonical scope key, 10 axes | — | `domain/scope_key.rs` (removed) | **SUPERSEDED** — entry key `(book_id, sku_id, charge_kind, period, model, usage_policy_digest)` plus one optional dimension per entry; "no plan, phase, variant, cohort, region or currency axis" (D-385, D-386) |
| P-5 | Validator registration in the Slice 5 pipeline | — | none | **CLOSED — not applicable** (R-12): approval is the shared `bss-approval` engine (§2.5) |
| P-6 | `PricingSnapshotRef { version_ref, price_ids, evaluation_policy_version }` | — | test-only (removed) | **SUPERSEDED** — Rating owns its snapshot (T-D-44); Orders calls `pricingSnapshotRef` a retired term (§2.11) |
| P-7 | Catalog events (`PlanPublished`, `PriceWindow*`, `PriceOverlayPublished`, …) | — | removed | **SUPERSEDED** by P-26 |
| P-8 | Money scales `MinorAmount(i64)`, `RateMinor(i64)` = 10⁻⁹ minor | — | removed | **SUPERSEDED** by P-22 (major-unit decimals) |
| P-9 | Enums `TierAggregationWindow`, `AggregationFunction`, `QuantitySource`, `ReservationFlavor`, `BillingAnchorPolicy`, `ProrationBasis`, `UsageCounterOnPlanChange`, `ScopeClass`, `AdjustmentKind` | — | removed | **SUPERSEDED** — window and scope are P-21; anchors are Subscriptions' `BillingTerms` (P-27, §2.6); proration is Rating's (T-D-77) |
| P-10 | `tierQualificationWindow`, `includedAllowance` | — | removed | **SUPERSEDED** — no included or minimum quantity (D-467) |
| P-11 | `billingTiming` values | `advance \| arrears` | — | **SUPERSEDED** by `InvoiceInputs.timing` (P-23) |
| P-12 | Migrated-origin snapshot | — | removed | **SUPERSEDED** |
| P-13 | Aggregation scope for usage tiers | — | — | **SUPERSEDED** by P-21 (`aggregation_scope`) |
| P-14 | Meter → usage type binding | — | — | **SUPERSEDED** — a usage SKU's meter is a Products derived usage type (§2.5 G-10); pricing gates entry create and publication through `UsageMeterSemanticsV1` (D-503) |

#### Validate `meter` as a usage type at publish

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-upreq-pricing-meter-binding`

**Closed 2026-10-08.** The ask was that Pricing validate a usage row's free-text `meter` at publish.
On `main` a usage SKU's meter is fixed by Products as a derived usage type
`products.derived/<code>@<n>` (products P-D-259, §2.5), the binding carries it as `MeterRef`, and
Pricing checks meter semantics through `UsageMeterSemanticsV1` at entry create and publication
(D-503).

- **Register aliases**: J-7; DECISIONS R-09 (closed).

#### Validator registration hook

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-upreq-pricing-validator-hook`

**Closed 2026-10-08 — not applicable.** Approval is the shared `bss-approval` engine with the
`bss-approvals` inbox (§2.5 G-12); no gear registers validators and Rating has no role in it.
Pricing runs its own publish checks (`METER_DUPLICATE`, `ITEM_UNCOVERED`, `FREQUENCY_MIXED`, …,
`pricing/src/domain/plan.rs:738-833`); Rating keeps only its rating-time fail-closed checks.

- **Register aliases**: J-10; DECISIONS R-12 (closed).

#### Per-row aggregation scope

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-upreq-pricing-aggregation-scope`

**Closed 2026-10-08 — delivered.** `usage_rating_policy.content.aggregation_scope ∈ {subscription_line,
resource}` (P-21). Rating maps it onto `AggregationKey.scope` (T-D-75); `resource` scope stays
LAUNCH-GATED until scope proofs list resources (`…-upreq-subscriptions-scope-proofs`, R-21).

- **Register aliases**: J-12; DECISIONS R-23 (closed).

#### Recurring proration basis confirmation

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-upreq-pricing-proration-confirmation`

Pricing and Finance **MUST** confirm Rating's proration basis (T-D-77): the covered fraction of a
recurring slice and of the minimum-fee floor is covered UTC seconds over the billing period's UTC
seconds, exact.

- **Rationale**: Pricing defines no basis and leaves proration and the floor to Rating (D-388, D-415),
  but its unbuilt period-slice note says "prorate recurring slices by calendar days"
  (`docs/design/07-read-contract-events.md:71-76`).
- **Source**: `gears/bss/rating` — DECISIONS T-D-77, T-D-78.
- **Status**: CONFLICTING (documents only; nothing built on either side).
- **Register aliases**: DECISIONS R-30.
- **Acceptance signal**: Pricing's slice-07 note and Rating's T-D-77 state the same basis.

#### Published money limits

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-upreq-pricing-numeric-limits`

Pricing **MUST** publish the limits of the money it serves — maximum significant digits,
fractional digits and magnitude of rates, amounts, tier bounds, package sizes and `minimum_fee` —
as the proposed cross-BSS money ADR's rule 3 asks of a price producer, and, if it adds a minimum
billable unit, declare it so that Rating can apply rule 5 (aggregate, round up once, then rate).

- **Rationale**: Rating's `ExactAmount` bounds (DESIGN §3.3, T-D-80) are derived from the only limit
  Pricing has, the `rust_decimal::Decimal` range (scale ≤ 28); a published narrower limit (the money
  ADR names 12 fractional places as a proposal for Pricing to assess) would let Rating tighten them.
  Pricing's `MIN_FEE_INVALID` compares `Decimal::scale()` with the currency scale
  (`pricing/src/domain/price.rs`), so `30.000` EUR is refused although the money ADR's rule 9 treats
  trailing zeros as insignificant — Pricing's to decide.
- **Source**: `gears/bss/rating` — DESIGN §3.3; DECISIONS T-D-80, R-33.
- **Status**: ABSENT (no published limit; money ADR proposed).
- **Register aliases**: DECISIONS R-33.
- **Acceptance signal**: Pricing's contract states the limits, and Rating's DESIGN §3.3 derivation
  cites them.

#### Pricing read grant for Rating's service identity

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-upreq-pricing-read-grant`

The deployment **MUST** grant Rating's system subject (`bss-rating.system`) pricing `plan:read`
(resolve) and `price:read` (price by id) for the tenants it rates (D-424, D-510 E3).

- **Rationale**: "names confer no privilege" (D-510 E3) and "a system-looking subject has no
  authorization bypass" (P-25); without the grant every resolve is denied.
- **Source**: `gears/bss/rating` — DESIGN §4.8; DECISIONS T-D-73.
- **Status**: DOCUMENTED (pricing D-424); deployment grant UNKNOWN.
- **Acceptance signal**: Rating resolves a pricing golden-contract revision under its service
  identity in an integration test.

#### Dimension key encoding

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-upreq-dimension-encoding`

Pricing and the usage-collector **MUST** agree how a usage record's declared metadata yields the
`dimension_value` of the entry's dimension key (pricing keeps one tenant registry of dimension keys,
seeded with `region`; each entry selects at most one key, and a null `dim_value` is the default
chain — D-385).

- **Rationale**: a binding names `dimension_key` and `dimension_value` (P-16), but nothing defines
  which metadata field carries the value; until it is defined only the default chain (no value) is
  rateable.
- **Source**: `gears/bss/rating` — [03 §4.2](./DESIGN.md#contract-03-4-2), [12 §4.1](./DESIGN.md#contract-12-4-1).
- **Status**: ABSENT.
- **Register aliases**: J-11; DECISIONS R-16.
- **Acceptance signal**: a usage record whose metadata carries `region = eu` rates on the `eu` value
  chain in both gears' tests.

#### Level meter rule

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-upreq-level-meter-rule`

Product, Products and the usage-collector **MUST** decide R-06: whether a charging consumer may fold
level inputs per granule (`Peak`, `TimeWeighted`, as a Products derived usage type declares them,
§2.5 G-10), or whether level products are emitted as `SUM` meters in the billable unit (the
collector's rule, also Atlas R1).

- **Rationale**: the collector's documents make only `SUM` chargeable (U-4), while Products' derived
  declarations allow `Peak` and `TimeWeighted` input folds that Rating applies (T-D-39, T-D-76).
  Until one rule stands, only `Sum` inputs are rated (LAUNCH-GATED).
- **Source**: `gears/bss/rating` — DESIGN §4.3 (level meters); DECISIONS T-D-76.
- **Status**: CONFLICTING (U-4).
- **Register aliases**: J-6; DECISIONS R-06.
- **Acceptance signal**: the losing document is amended; the launch capability matrix (DESIGN §4.10)
  admits `Peak`/`TimeWeighted` inputs or removes them.

### 2.5 Products registry

`gears/bss/products` (crates `products`, `products-sdk`), with the shared approval library
`gears/bss/libs/approval` and the `bss-approvals` inbox gear. Paths are relative to `gears/bss/`.

| # | Contract | Evidence | Status |
|---|---|---|---|
| G-4 | `CatalogVersionRegistryV1`, `CatalogVersionPublished`, "the registry allocates catalog versions" | the Products PRD puts CatalogVersion snapshots, freeze and diff out of scope (`products/docs/PRD.md:147-148`); legacy `products_catalog_version*` tables are refused at boot (`products/products/src/infra/storage/migrations/m0000_products_refuse_a_legacy_or_stale_schema.rs:45-49`) | **SUPERSEDED** — no catalog-version concept remains |
| G-5 | `usage_type_ref` on the registry metering declaration | `CatalogSku.usage_type_ref` exists (`pricing/pricing-sdk/src/product_catalog.rs:63-66`), filled by Products | **SUPERSEDED** by G-10 (a usage SKU sells a derived usage type) |
| G-9 | Dated SKU versions: `SkuVersion{sku_id, published_version, effective_from, content}`; dated read `sku_version_as_of` / `GET /skus/{id}/versions/as-of?date=`; lifecycle `draft \| published \| deprecated \| retired` plus a `retire_pending` flag; events `SkuPublished`, `SkuChanged` (`publishedVersion`, `effectiveFrom`), `SkuRetired` | `products/products-sdk/src/models.rs:221-247`; `products/products-sdk/src/references.rs:130-143`; `products/products/src/infra/broker.rs:30-136` | **CONFIRMED** — Rating does not read them directly: the binding carries `sku_version` as of the resolve date (P-16). `sku_version_as_of` is registered only as the pricing-keyed `PricingReferenceRegistry` and refuses other system subjects (`products/products-sdk/src/references.rs:24-36`) |
| G-10 | **Derived usage types**: a usage SKU must sell `products.derived/<code>@<n>` (raw GTS refs refused, `DERIVED_USAGE_TYPE_REQUIRED`); its unit is the version's `output_unit`; metering is fixed after the first publish (`METERING_IMMUTABLE`, P-D-258; one exception: a raw meter moved onto its identity wrapper, P-D-251). `DerivedUsageDeclaration{output_unit, granularity: Hour, inputs: [DerivedInput{name, usage_type_ref (raw GTS id), granule_fold: Sum \| Peak \| TimeWeighted, max_hold_seconds?, unit}], formula: Expr, output_scale, output_round}`; `MIN_INPUTS = 1`; versions append-only | `products/docs/DECISIONS.md:1361-1362, 1465-1500, 2009-2036` (P-D-229, P-D-231, P-D-251, P-D-258, P-D-259); `products/products-sdk/src/derived.rs:27-139, 830-934` | **CONFIRMED** — every usage meter Rating rates is one (T-D-76) |
| G-11 | Derived evaluator: `bss_products_sdk::derived::{validate, evaluate(decl, &BTreeMap<String, Decimal>) -> Result<Decimal, EvalError>, evaluate_window, canonical_bytes}`; "Rating folds each input over one granule (an hour) … `evaluate` applies the formula … a window's output is the sum of its granule outputs"; the SDK does not fold | `products/products-sdk/src/derived.rs:1-21, 309, 475-544, 622` | **CONFIRMED** — called by `rating-core` (T-D-39, T-D-76) |
| G-12 | Approvals: the `bss-approval` engine (quorum, separation of duties, generation-checked votes) used by Pricing and Products; the `bss-approvals` inbox lists kinds `prices`, `plan_revision`, `sku_publish`, `sku_change`, `sku_retire` through `ApprovalSourceV1` | `libs/approval`; `approvals/approvals-sdk/src/lib.rs:18` | **CONFIRMED**; Rating has no role (R-12 closed) |
| G-13 | Meter semantics for Pricing: `UsageMeterSemanticsV1::resolve(ctx, MeterRef) -> {meter, canonical_unit, fold, accrual_policy_version, source_integrated, digest}`; Products answers derived meters (E1b, P-D-233: `fold = Sum`, `accrual_policy_version = derived-v1:<digest>`); raw meters (E1a) are not built | `pricing/pricing-sdk/src/meter_semantics.rs:9-51`; `products/products/src/infra/meter_semantics.rs:5-19` | **CONFIRMED** for derived meters; carries no inputs or formula, so it is not the declaration read Rating needs |
| G-14 | Products reads are limited to the caller's own tenant: the PEP asks the PDP for `TenantMode::RootOnly`, so a parent tenant's lists hold its own rows only (P-D-265, decided 2026-10-04; on upstream `main` `95eb3f46f`, after this branch's base) | upstream `products/docs/DECISIONS.md:87, 2186` | **DOCUMENTED** upstream — any Products read Rating makes runs per owning tenant, never parent-for-child |

#### Derived usage declaration read

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-upreq-products-derived-declaration-read`

Products **MUST** provide a ClientHub read of a stored derived usage declaration by meter id, under
the caller's tenant — for example `DerivedUsageTypeReadV1::declaration(ctx, MeterId{code, version})
-> {DerivedUsageDeclaration, digest}` — authorized for Rating's service identity.

- **Rationale**: Rating evaluates every usage meter through `derived::evaluate` (G-11) and must
  freeze the declaration it rated with (T-D-76); products-sdk has the types and the evaluator but no
  read (`products/products-sdk/src/lib.rs:4-8`). The only read is REST `GET
  /bss-products/v1/derived-usage-types/{code}/versions/{n}` (`sku:read`,
  `products/products/src/api/rest/derived_usage_types.rs:224`); `UsageMeterSemanticsV1` (G-13)
  returns no inputs or formula.
- **Source**: `gears/bss/rating` — DECISIONS T-D-76; [contract 03](./DESIGN.md#contract-03).
- **Status**: UNKNOWN / EXTERNAL CONTRACT REQUIRED.
- **Register aliases**: DECISIONS R-31.
- **Acceptance signal**: Rating's child evaluation reads a declaration through the trait and its
  stored digest equals Products' version digest.

#### Derived output limits

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-upreq-products-derived-output-limits`

Products **MUST** publish the full magnitude limit of a derived usage type's granule output, beside
its `output_scale ≤ 12` (`products-sdk/src/derived.rs` `MAX_SCALE`), as the proposed money ADR's
Adoption section asks, and **MUST** make formula evaluation exact up to the declared rounding: today
`Mul` and `DivConst` use `rust_decimal` checked operations (`derived.rs` `eval`), which round a result
past 28 fractional digits before `output_round` applies (for example `DivConst(x, 3)`), an undeclared
second rounding the money ADR's rules 4 and 5 forbid. Products either evaluates exactly (or with a
proven-sufficient width) or refuses a formula whose intermediate cannot be exact.

- **Rationale**: Rating sums granule outputs into `ExactQuantity` (≤ 60 integer digits) and passes
  granule inputs to `derived::evaluate` as `rust_decimal::Decimal`; a published output limit lets
  Rating prove the window sums fit and narrow its bounds (DESIGN §3.3).
- **Source**: `gears/bss/rating` — DESIGN §3.3; DECISIONS T-D-80, R-33.
- **Status**: ABSENT (scale published, magnitude not); CONFLICTING for exactness (intermediate `rust_decimal` rounding in `eval`).
- **Register aliases**: DECISIONS R-33.
- **Acceptance signal**: the Products quantity contract states the magnitude limit.

### 2.6 Subscriptions

`gears/bss/subscriptions` — **documentation only; no crate, SDK, REST or migrations exist**. Its
decision log runs to SUB-D-29; its three ADRs are `status: proposed`. Paths are relative to
`gears/bss/subscriptions/docs/`.

**DOC/DOC DRIFT inside Subscriptions.** SUB-D-29 (2026-09-25) adopts the PriceBook model: "the
eight-axis key …, phases, cohort, grandfatherUntil and the catalog-version contract are replaced …
Bindings pin price ids, dimension choices, SKU versions/descriptors and promotion versions per
period; renewal walks all successors and stops before the first new price" (`DECISIONS.md:69,
272-277`). It exists **only** in DECISIONS: the PRD, DESIGN, SEAMS, slices and ADRs never mention
PriceBook, `resolve`, `ends_on` or plan revisions, and still use `pricingSnapshotRef` (a composite
where Subscriptions writes only `(currency, region)`), `cohort`, `priceEligibility`,
`grandfatherUntil`, `prorationBasis`, `billingAnchorPolicy` and `quantitySource` — for example
`design/02-composition-versioning.md:104-106, 212`, `design/08-events-billing.md:53, 67, 138-149,
249-265`, `design/09-consumer-contracts.md:211, 217`, `design/03-plan-changes.md:109, 247`,
`PRD.md:130-134, 548, 852, 1055-1057`, `SEAMS.md:45, 65, 197, 200`, `DESIGN.md:452`. SUB-D-14
(cohort-based re-binding) is not marked superseded. Rating reads Subscriptions through SUB-D-29 and
the rows below; it does not rely on the stale body text.

| # | Contract | Required fields | Version semantics | Evidence | Status |
|---|---|---|---|---|---|
| S-1 | Subscription version (timeline) | `subscription_id, version`, tenant axes (payer/seller/resource/ordering), plan links, add-ons, `QuantityInterval`s, `(currency, region)`, suspensions, `(changeEffectiveAt, changeMode)` | monotonic | `design/01-foundation-lifecycle.md:187`, `design/03-plan-changes.md`, `design/09-consumer-contracts.md:217` | ASSUMED; SUB-R1 field list still names phases and cohort (pre-PriceBook) |
| S-2 | Recurring period fact `BillableItemCreated(kind = recurring)` per `(subscriptionId, billing period, lineKey)`, money-free, traceability tuple `{subscriptionId, skuId, planId, priceId}` + `pricingSnapshotRef` (the accepted price binding, not Rating's snapshot — R-29), suspended intervals + `pause_recurring\|continue`, period-start `payerTenantId`, `collectionPaused`; cut daily by 00:00 | as listed; **no** `plan_revision_id`, pins or `BillingTerms` | unique key; no fact versions | `design/08-events-billing.md:118, 192, 215, 249-257`; SUB-D-07/19/20/21/27 | ASSUMED; emission timing ambiguous (opening vs end) |
| S-3 | Resource → subscription attribution | — | — | none; `PRD.md:360` assumes usage arrives "already keyed by `subscriptionId`" | **ABSENT** — CONFLICTING with UC U-7 (R-25) |
| S-4 | `lineKey` rule `plan#n`, `addon:{addOnId}#n` | — | immutable | SUB-D-21 | ASSUMED (T-D-34) |
| S-5 | Ordering key `(orderingTenantId, subscriptionId)` | — | — | SUB-D-06 | ASSUMED |
| S-6 | One-time fact `BillableItemCreated(kind = one_time)`, deduplicated per phase-entry occurrence `(tenantId, subscriptionId, phaseEntryId, componentOccurrenceId, chargeLineId)` (SUB-D-28, replacing SUB-D-24's lifetime `(subscriptionId, priceId)`) | — | per occurrence | `DECISIONS.md:68, 262-270`; stale lifetime key still at `design/08-events-billing.md:149`, `SEAMS.md:197` | ASSUMED; whether pricing's resolve exposes `chargeLineId` after SUB-D-29 is unreconciled; not rated by Rating (T-D-18, R-19) |
| S-7 | Recovery reads (`facts`, `period_facts`, `segments_since`, `scope_for_window`, `bindings`, `groups_due`) | — | — | none; outbox replay + 02:00 charge-coverage reconciliation only (`PRD.md:1483`) | **ABSENT** (Atlas C04 PROPOSED) |
| S-8 | Billing group / sealed composition | — | — | none ("billing group" occurs nowhere) | **ABSENT** (Atlas C04 PROPOSED) |
| S-9 | Per-period price bindings (pins): renewal sends the current bindings as `pins` to `resolve` with the period start as `date`; signup sends none; first period = the accepted order's bindings (Orders D-162) | price ids, dimension choices, SKU versions/descriptors per period | per period | SUB-D-29 (`DECISIONS.md:275-277`); Orders `DECISIONS.md:3153, 3156` | DOCUMENTED (decision only); **no store, table or fact field names them** |
| S-10 | Billing periods and anchors: Subscriptions cuts periods (`RecurringEmitter`; SB1, T-D-33/34); `billingAnchor` from Contract terms; SUB-D-27 adopts pricing's `billingAnchorPolicy` | — | — | `SEAMS.md:94`; `DECISIONS.md:67, 258-260`; `PRD.md:134` | **CONFLICTING** — `billingAnchorPolicy` no longer exists in Pricing; pricing-sdk defines `BillingTerms{cycle: Month\|Year, anchor: Calendar\|SubscriptionStart, anchor_at, timezone: Utc, source, digest}` as "Subscriptions-owned invoice terms" (`gears/bss/pricing/pricing-sdk/src/terms.rs:160-198`) |
| S-11 | Committed quantity: `updateQuantity` transitions stored effective-dated (`QuantityInterval`), `quantity @ t`; `SubscriptionQuantityChanged` rides the existing fact | — | effective-dated | `PRD.md:636`; `design/03-plan-changes.md:247`; `design/02-composition-versioning.md:205` | ASSUMED; still named `quantitySource = subscription_seat_count` (pre-PriceBook) |
| S-12 | Re-resolve at an `ends_on` inside a period: SUB-D-29 says "Subscriptions resolves again with the pin on that date"; Orders records the cut owner as **unagreed** between Subscriptions and Rating | — | — | `DECISIONS.md:276`; Orders `DECISIONS.md:3190`, `UPSTREAM_REQS.md:300-301` | CONFLICTING |

The Subscriptions asks below together form the Atlas C04 surface (former J-4); they are split so
each can be agreed and delivered on its own.

#### Versioned commercial facts

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-upreq-subscriptions-versioned-facts`

Subscriptions **MUST** publish commercial facts with a Subscriptions-assigned `fact_id` and a new
`fact_version` per change (`BillableFactPublished`, Atlas C04), carrying the billing group and
composition version (`BillableSetSealed`), the served extent, timing, the period's pricing inputs
(`…-upreq-subscriptions-fact-pricing-inputs`) and all tenant axes. Subscriptions owns the commercial
WHEN; Rating never self-synthesizes a commercial period (T-D-33).

- **Rationale**: Rating's child windows, re-evaluation and parent revisions are per fact version;
  today's `BillableItemCreated` has no versions, so a re-emission with a different digest can only
  be quarantined.
- **Source**: `gears/bss/rating` — DESIGN §3.6 Flow C; feature [`06-fact-scheduling`](./features/06-fact-scheduling.md).
- **Status**: ASSUMED (S-2); ABSENT (S-8); MIGRATION mapping in DESIGN Flow C.
- **Register aliases**: J-4; DECISIONS R-03; reconcile SUB-D-07/19/28 and AC 27.
- **Acceptance signal**: Rating's fact intake applies two versions of one fact from the producer's
  implementation and produces two parent revisions.

#### Pricing inputs on every fact

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-upreq-subscriptions-fact-pricing-inputs`

Every recurring and usage fact **MUST** carry the inputs Rating resolves prices with (T-D-73): the
`plan_revision_id`; the period's pins as `[(item_id, dimension_value?, price_id)]` (SUB-D-29; the
accepted order's bindings for the first period, Orders D-162); the committed quantity per item for
the period (S-11); and the period's `BillingTerms` (cycle, anchor, `anchor_at`, UTC).

- **Rationale**: Pricing stores no pins (P-19) and SUB-D-29 names no store for them (S-9); without
  them on the fact Rating cannot call `resolve`, cannot price a per_unit recurring line (no pricing
  read carries quantity, P-27), and cannot place proration and the floor (T-D-77, T-D-78).
- **Source**: `gears/bss/rating` — DECISIONS T-D-73, T-D-77; feature [`06-fact-scheduling`](./features/06-fact-scheduling.md).
- **Status**: DOCUMENTED in SUB-D-29 as a decision; ABSENT as a fact field.
- **Register aliases**: DECISIONS R-03.
- **Acceptance signal**: Rating resolves a fact's bindings from the fact alone and gets the same
  `binding_digest` Subscriptions recorded.

#### Re-resolve at `ends_on` inside a period

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-upreq-subscriptions-ends-on-cut`

Subscriptions **MUST** confirm who cuts a period at a binding's `ends_on` (S-12). Rating's design
resolves again with the fact's pin on that date for the rest of the period and slices the child
there (T-D-73, pricing D-397/D-425); if Subscriptions instead issues a new fact version with the new
binding, Rating applies that version and the result is the same.

- **Rationale**: SUB-D-29 gives the re-resolve to Subscriptions, Orders marks it unagreed, and both
  gears doing it independently could disagree on the binding for the rest of the period.
- **Source**: `gears/bss/rating` — DECISIONS T-D-73.
- **Status**: CONFLICTING (S-12).
- **Register aliases**: Orders open item "`ends_on` cut owner".
- **Acceptance signal**: both gears' documents name one owner and a temporary-price fixture
  (`resolve_nested_pairs`) produces the same slices in both.

#### Usage parent fact per period

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-upreq-subscriptions-usage-fact`

Subscriptions **MUST** publish a usage parent fact per subscription line and billing period at period
opening, naming the served extent, the billing group and the period's pricing inputs (Atlas C04,
D13).

- **Rationale**: Rating schedules usage child windows only beneath a received fact (T-D-49). The
  current Subscriptions documents have no usage fact kind and SUB-D-07/SUB-D-19 and AC 27 require one
  priced line per `(subscriptionId, period, lineKey)` — a CONTRACT CONFLICT.
- **Source**: `gears/bss/rating` — DESIGN §3.6 usage parent; feature [`06-fact-scheduling`](./features/06-fact-scheduling.md).
- **Status**: ABSENT; MIGRATION: Rating derives a usage parent (`parent_origin = derived`).
- **Register aliases**: J-4; DECISIONS R-20; Atlas Plus X9.
- **Acceptance signal**: Rating's MIGRATION derivation is switched off and every usage delivery
  carries `billing_group_kind = published`.

#### Attribution segments

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-upreq-subscriptions-attribution-segments`

Subscriptions **MUST** publish attribution segments that map a resource to a subscription line over
time — `AttributionSegmentChanged{segment, segments_stream_seq}` with full-state replacement and an
explicit version, gap-free `lifecycle_seq` per `resource_tenant_id` — and **MUST** retract
`PRD.md:360` ("usage arrives already keyed by `subscriptionId`").

- **Rationale**: the collector carries no commercial identity (U-7); Rating attributes from a local
  projection and never per-record lookups (T-D-57). Until a source exists every record stays
  `awaiting_attribution`.
- **Source**: `gears/bss/rating` — feature [`05-attribution-counters`](./features/05-attribution-counters.md).
- **Status**: ABSENT (S-3); CONFLICTING owner.
- **Register aliases**: J-4; DECISIONS R-25.
- **Acceptance signal**: Rating's projection applies segments from the producer and attributes a
  usage record without operator action.

#### Usage scope proofs

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-upreq-subscriptions-scope-proofs`

Subscriptions **MUST** answer `scope_for_window(fact_id, fact_version, window,
inventory_snapshot_id)` with `Pending(required_seq)` or a sealed, content-addressed `UsageScope`
(`UsageScopeSealed`), **batched per fact**, listing resources when the entry's
`aggregation_scope = resource` (P-21).

- **Rationale**: a sealed scope is part of the finalization evidence (T-D-52); a sealed empty scope
  is how an hour without usage becomes an explicit zero; resource-scoped entries stay LAUNCH-GATED
  until the scope lists resources (T-D-75). Per-window calls do not scale to the hourly finalization
  burst (DESIGN §4.9).
- **Source**: `gears/bss/rating` — DESIGN §3.6 usage parent; feature [`07-child-evaluation`](./features/07-child-evaluation.md).
- **Status**: ABSENT (S-7, Atlas C04 PROPOSED; Atlas Plus X11).
- **Register aliases**: J-4; DECISIONS R-03, R-21.
- **Acceptance signal**: a batched scope read finalizes all 744 hourly children of a month in the
  load test of DESIGN §4.9.

#### Recovery reads

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-upreq-subscriptions-recovery-reads`

Subscriptions **MUST** provide `SubscriptionBillingReadV1::{facts, period_facts, segments_since,
bindings, groups_due}` so every fact, segment and period binding can be recovered by business key
without events.

- **Rationale**: pull reads are always sufficient for Rating (T-D-56); Subscriptions has no code and
  publishes no events yet.
- **Source**: `gears/bss/rating` — DESIGN §3.6 Flow C watchdog, §4.6; feature [`06-fact-scheduling`](./features/06-fact-scheduling.md).
- **Status**: ABSENT (S-7).
- **Register aliases**: J-4; DECISIONS R-03.
- **Acceptance signal**: Rating's reconciler recovers a fact that was never delivered by event.

#### One-time charge rating

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-upreq-subscriptions-one-time-rating`

The Subscriptions owner **MUST** decide whether one-time facts are rated by Rating (Atlas R2,
Rating's recommendation) and, if so, amend SUB-D-24 and SUB-D-28.

- **Rationale**: T-D-18 and SUB-D-24 (its dedup key replaced by SUB-D-28) keep one-time charges
  unrated and billed "from the frozen snapshot" by Subscriptions/Billing; in this repository no gear but Rating composes a
  snapshot (T-D-44), and the binding a one-time charge needs is the same `resolve` binding Rating
  already prices (P-16).
- **Source**: `gears/bss/rating` — DESIGN §3.1 child kinds (`one_time`).
- **Status**: CONFLICTING (S-6); until decided one-time facts are stored, not rated (T-D-18).
- **Register aliases**: DECISIONS R-19; overlay ticket T8.
- **Acceptance signal**: SUB-D-24/SUB-D-28 are amended and the launch capability matrix moves one-time charges
  from "stored, not rated".

#### Accepted price binding naming

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-upreq-accepted-binding-naming`

Subscriptions, Billing and Rating **MUST** agree names that distinguish the upstream **accepted price
binding** (Subscriptions-owned pins, SUB-D-29) from the **Rating snapshot**
(`pricing_snapshot_ref`), and the mapping for facts already accepted.

- **Rationale**: Subscriptions' body documents still name the binding `pricingSnapshotRef`; Orders
  calls it a "Retired term (PriceBook, 2026-09-29)" (Orders `PRD.md:127`); the ledger slot
  `pricing_snapshot_ref` carries Rating's snapshot id (§2.8) — a CONTRACT CONFLICT.
- **Source**: `gears/bss/rating` — DESIGN §4.1.
- **Status**: CONFLICTING.
- **Register aliases**: DECISIONS R-29.
- **Acceptance signal**: Subscriptions' consumer contract names the field as agreed.

### 2.7 Billing

There is **no Billing gear** (`docs/GEARS.md`: Invoicing is a placeholder entry with no links).

| # | Contract | Semantics | Evidence | Status |
|---|---|---|---|---|
| L-1 | Billing period state | — | `periodState`, `BillingPeriodStateChanged`, invoice freeze: no producer anywhere | **ABSENT** — under T-D-50 Rating does not need it |

Billing still rounds once per invoice line HALF_EVEN (pricing D-510 E4: "Billing sums exact
contributions before HALF_EVEN invoice rounding"); the minimum-fee floor is applied by Rating, not
Billing (T-D-38, T-D-78). The proposed cross-BSS money ADR
([PR #5270](https://github.com/constructorfabric/gears-rust/pull/5270), not merged) names Billing
(planned) as the owner of the invoice-line contract: the line key, `HALF_EVEN` line rounding to a
major-unit `PostedMoney`, and cumulative corrections (its rules 7, 8).

#### Delivery consumer

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-upreq-billing-delivery-consumer`

A Billing gear **MUST** consume `BillableItemDeliveryV1` (event when enabled, `RatingRunReadV1::
deliveries_since` pull always), keep per-fact accepted heads, ignore a stale revision, treat an
identical revision as a no-op and quarantine a same-revision different-digest payload, sum exact
contributions per invoice line key and round once HALF_EVEN at the stored `currency_scale` to a
major-unit decimal validated as `PostedMoney` (Atlas C08; money ADR rule 7), and confirm T-D-51 as
amended by T-D-80.

- **Rationale**: no gear consumes rated output; Rating never rounds and never posts.
- **Source**: `gears/bss/rating` — [16 §4.1](./DESIGN.md#contract-16-4-1); feature [`09-rollup-delivery`](./features/09-rollup-delivery.md).
- **Status**: ABSENT (Atlas C08 PROPOSED).
- **Register aliases**: J-5; DECISIONS R-05; T-D-51 ("Billing to confirm").
- **Acceptance signal**: Billing passes acceptance vector V16 and Atlas F27 against Rating's pull feed.

#### Invoice-line contract

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-upreq-billing-invoice-line-contract`

Billing **MUST** publish the invoice line key and the rounding and correction rules of the
proposed money ADR (rules 7, 8) and agree with Rating the exact charge-delivery form and its limits:

1. the key, with at least billing group, subscription line (`sub_line_key`), item and dimension
   value, charge kind, currency, stored `currency_scale`, unit, and the accounting and tax classes
   that cannot share a line — including whether a `min_fee_topup` shares the line of the charge it
   tops up (DESIGN [15 §4.3](./DESIGN.md#contract-15-4-3) is Rating's proposal);
2. line rounding: sum the exact contributions of one key (discounts, proration and top-ups
   included), round once with HALF_EVEN at the stored `currency_scale`, validate as `PostedMoney`;
3. corrections: `round_half_even(corrected line total, stored scale) − cumulative posted amount`
   (the original posting and every prior note), validated and posted unrounded, the original
   grouping and stored scale kept; posted amounts never sent back to rating;
4. the minimum-fee floor under line rounding: when a floored price rated several lines, a rule
   that keeps the price's posted total at the rounded floor (for example top-up =
   `round(floor) − sum of the posted rated lines of the price`), since per-line rounding can post 29.99
   against a 30.00 floor (DESIGN 15 §4.3, V49);
5. Billing's own bound for a line sum across facts (contributions of facts with different period
   lengths widen the denominator beyond one delivery's bound);
6. the delivery numeric contract: `ExactAmount{numerator, denominator, currency, currency_scale}`
   in major units, canonical integer strings, the bounds of DESIGN §3.3 (`abs(numerator) < 10^84`,
   `denominator < 10^64`) or narrower ones Billing can justify.

- **Rationale**: the money ADR makes these Billing's and asks Rating and Billing to define the
  delivery form together; the key decides the posted amount (`0.005 + 0.005` EUR posts 0.01 on one
  line and 0.00 on each of two).
- **Source**: `gears/bss/rating` — DESIGN §3.3, [15 §4.3–§4.4](./DESIGN.md#contract-15-4-3),
  [16 §4.1](./DESIGN.md#contract-16-4-1); DECISIONS T-D-51, T-D-80, R-32.
- **Status**: ABSENT (no Billing gear; money ADR proposed).
- **Register aliases**: DECISIONS R-32.
- **Acceptance signal**: Billing passes the Billing golden rows of V35–V38, V40, V43–V45, V49 and F27
  against Rating's pull feed, and its published key names every component above.

#### Period state and correction routing

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-upreq-billing-period-ownership`

Billing **MUST** own period state and decide, per parent revision, between replacing a draft and
issuing an ordered credit/debit note (Atlas D09, C08); `BillingPeriodStateChanged` is at most an
observational hint to Rating.

- **Rationale**: Rating keeps no period fence (T-D-50); posted-period protection is Billing's.
- **Source**: `gears/bss/rating` — [16 §4.2](./DESIGN.md#contract-16-4-2).
- **Status**: ABSENT (L-1).
- **Register aliases**: DECISIONS R-04; overlay ticket T8.
- **Acceptance signal**: a correction after posting produces a credit or debit note from Billing,
  with no Rating period state involved.

#### Ledger item granularity

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-upreq-billing-ledger-item-granularity`

Billing **MUST** decide which `price_id` a ledger item carries when one Rating parent line spans
several prices, or split the item.

- **Rationale**: the ledger `InvoiceItemDto` carries one `price_id` and one `pricing_snapshot_ref`;
  Rating supplies a composite snapshot per parent line and the full lineage (DESIGN §4.1).
- **Source**: `gears/bss/rating` — DESIGN §4.1.
- **Status**: UNKNOWN / EXTERNAL CONTRACT REQUIRED.
- **Register aliases**: DECISIONS R-26.
- **Acceptance signal**: Billing's posting contract names the rule.

### 2.8 bss-ledger

| # | Contract | Semantics | Evidence | Status |
|---|---|---|---|---|
| L-2 | Invoice post `POST /bss-ledger/v1/journal-entries` (`PostInvoiceRequestDto`: `tenant_id, invoice_id, payer_tenant_id, resource_tenant_id?, effective_at, due_date?, period_id, items, tax, correlation_id`; `InvoiceItemDto.amount_minor_ex_tax: i64`, `gl_code`, `price_id`, `pricing_snapshot_ref`, `invoice_item_ref`, `recognition?`) | idempotent on `(tenant, INVOICE_POST, invoice_id)` | `ledger/src/api/rest/journal_entries.rs:123-140`, `dto.rs:224-243, 417-435` | CONFIRMED — `pricing_snapshot_ref` is `Option<String>` with no presence check (`ledger-sdk/src/posting.rs:37`; column `varchar(128)`); credit-note lines set it to `None` (`infra/adjustment/credit_note_service.rs:863`). Billing fills it with Rating's `snapshot_id`; nothing on `main` assigns it otherwise. Billing rounds each line to a major-unit decimal (money ADR rule 7); filling the current `i64` minor field is an exact `× 10^currency_scale` integer conversion of that validated decimal, an interim step until the ledger accepts `PostedMoney`. The proposed money ADR's target is a `PostedMoney` decimal in major units (ledger and Billing adoption; Rating is not involved) |
| L-3 | `LedgerClientV1` (21 methods: `post_balanced_entry`, `close_period`, `trigger_recognition_run`, …) | no invoice/credit/debit method | `ledger-sdk/src/api.rs:22-445` | PARTIAL — Billing's concern (Atlas N11) |
| L-4 | `POST /credit-notes`, `POST /debit-notes`, `/manual-adjustments` | idempotent on the note id | `api/rest/adjustments.rs:126,173,215` | CONFIRMED (REST) — Billing posts them from aggregate-target differences (Atlas C08) |
| L-5 | Ledger fiscal period (`ledger_fiscal_period OPEN\|CLOSED`; `ledger_period_close OPEN\|CLOSING\|CLOSED\|REOPENED`) | posts into a closed period rejected `PeriodClosed` | `m20260628_000033`, `domain/error.rs:111` | CONFIRMED but a different concept; not used by Rating |
| L-6 | Rounding | ledger `round_half_even` for its own splits; invoice items arrive in integer minor units | `ledger/src/domain/money_math.rs:24`; ledger PRD `:661` | CONFIRMED — invoice rounding is the invoice builder's (Billing); the proposed money ADR moves postings to major-unit `PostedMoney` decimals validated, never rounded, by the ledger (ledger's adoption) |

Rating never calls the ledger. One wording ask:

#### Credit-note poster wording

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-upreq-ledger-credit-note-wording`

The ledger owner **MUST** reword `ledger/docs/design/01`:364, which names a Rating-side adapter as the
credit-note poster: Billing decides and posts notes (T-D-50).

- **Rationale**: the sentence contradicts T-D-50 and Atlas C08 (Atlas Plus overlay T8, row 22d).
- **Source**: `gears/bss/rating` — DECISIONS T-D-50.
- **Status**: CONFLICTING (documents only).
- **Register aliases**: overlay T8 row 22d.
- **Acceptance signal**: the ledger document names Billing.

### 2.9 Finance and FX

| # | Contract | Evidence | Status |
|---|---|---|---|
| F-1 | Versioned FX table / locked-rate id readable by Rating | none in any gear | **MISSING** |
| F-2 | `RateProviderV1::fetch_latest(ctx, pairs, request_id) -> Vec<ProviderRate {base, quote, rate_micro, as_of, provider}>` — latest only; ledger discovers the provider through a types-registry plugin instance + ClientHub scoped client | `ledger-sdk/src/rate_provider.rs:64-90`, `rate_provider_plugin.rs` | CONFIRMED but unusable by Rating |
| F-3 | Ledger `ledger_fx_rate_snapshot` minted inside a post | `infra/fx/rate_locker.rs`, `api/rest/fx.rs:110` | CONFIRMED, not pinnable before a post |

Consequence: native currency only (R-07). On `main` a price book has one immutable currency
(pricing D-384) and a binding's price carries it, so a currency mismatch is a fact/binding
inconsistency, failed closed `fx_not_supported`.

#### Pinnable FX rate snapshots

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-upreq-fx-rate-snapshots`

The ledger or a Finance gear **MUST** expose versioned FX rate snapshots readable by id before a post,
so Rating can pin a rate as a rating input.

- **Rationale**: an unpinned rate makes a result unreplayable; until then a child whose billing
  currency differs from the price currency fails `fx_not_supported`.
- **Source**: `gears/bss/rating` — [contract 07](./DESIGN.md#contract-07), [11 §4.4](./DESIGN.md#contract-11-4-4).
- **Status**: MISSING (F-1).
- **Register aliases**: J-8; DECISIONS R-07.
- **Acceptance signal**: a cross-currency fixture rates with a recorded `rate_ref` and replays
  identically.

#### `delay_only` finalization acceptance

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-upreq-delay-only-acceptance`

Finance and Product **MUST** record, per finalization policy, whether the interim `delay_only`
evidence mode (delay elapsed, no coverage evidence) is acceptable until coverage declarations exist.

- **Rationale**: without that acceptance usage windows stay `pending` until the coverage owner
  delivers (`…-upreq-coverage-declarations`); `delay_only` is never a default and never bypasses a
  known source loss (T-D-62).
- **Source**: `gears/bss/rating` — DESIGN §4.10; feature [`06-fact-scheduling`](./features/06-fact-scheduling.md).
- **Status**: no decision.
- **Register aliases**: DECISIONS R-21; overlay ticket T6.
- **Acceptance signal**: an approved policy version with `evidence_mode = delay_only` exists for the
  launch window policies, with its approval recorded.

#### Lifetime tier settlement model

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-upreq-lifetime-settlement-model`

**Closed 2026-10-08 — not applicable.** Pricing offers only `BillingCycle` and `CalendarHour`
rating windows (P-21, D-514); `subscription_lifetime` no longer exists.

- **Register aliases**: DECISIONS R-28 (closed).

### 2.10 Contracts and Promotions

| # | Contract | Evidence | Status |
|---|---|---|---|
| G-1 | Contract price-override windows, applied by Rating as the step-5 overlay | `gears/bss/contracts/docs/PRD.md:296-307` (first draft, §7–17 TBD) | ASSUMED; pricing's own `PriceOverlay` no longer exists (a plan-specific exception uses another book or SKU, pricing `docs/PRD.md:147, 154`) |
| G-2 | Commitment pools, `poolType`, `balanceVersion`, draw order, rollover, `CommitmentBalanceEffect` | contracts PRD `:310-322` defines commitment type/threshold only; balances "owned downstream" | **ABSENT** (R-11) |
| G-3 | Promotions / coupons: **owned by Pricing and deferred** (pricing D-409); `resolve` returns no promotion and promotion-aware renewal "comes back with promotions" (D-420); the only dated discount-like mechanism is a temporary price pair (`temporary_until`), which a new sale refuses | `gears/bss/pricing/docs/DECISIONS.md:343-365, 445`; `gears/bss/pricing/pricing/src/infra/commercial_terms/check.rs:297-299` | **ABSENT** (deferred by its owner) |

#### Contract overlays, commitment pools and balances

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-upreq-contracts-inputs`

The Contracts gear **MUST** supply version-keyed contract price overrides (step 4, contract-override slot), commitment pools
with `poolType`, `balanceVersion`, draw order, rollover and optional `overageRate` (step 4, commitments slot),
negotiated reserved rates, and the contract for `CommitmentBalanceEffect` consumption and
`balanceVersion` serialization.

- **Rationale**: the contract-override and commitments slots of step 4 run with empty inputs at launch and fail closed when referenced
  (`reservation_match_unavailable`); the balance cascade (DESIGN Flow F) is dormant.
- **Source**: `gears/bss/rating` — [11 §4.9](./DESIGN.md#contract-11-4-9), contracts [04](./DESIGN.md#contract-04)–[05](./DESIGN.md#contract-05).
- **Status**: ASSUMED (G-1), ABSENT (G-2); external contract UNKNOWN.
- **Register aliases**: DECISIONS R-11.
- **Acceptance signal**: a contract overlay and a commitment pool from the Contracts implementation
  rate through the step-4 contract-override and commitments slots in an end-to-end test.

#### Frozen coupon snapshots

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-upreq-promotions-coupon-snapshots`

Pricing, which owns promotions (D-409), **MUST**, when it builds them, supply frozen, version-keyed
promotion versions on the binding that Rating can pin (SUB-D-29 already lists "promotion versions per
period" among the bindings), with the fields [contract 06](./DESIGN.md#contract-06) requires
(`applyScope`, stacking policy, `stackSequence`, `valueCurrency`, settlement currency).

- **Rationale**: promotions are deferred by their owner; until they exist the coupons slot of step 4 has no input and a
  referenced promotion fails closed `coupon_source_unavailable`.
- **Source**: `gears/bss/rating` — [11 §4.5](./DESIGN.md#contract-11-4-5).
- **Status**: ABSENT (G-3; deferred by Pricing, D-409).
- **Register aliases**: DECISIONS R-11.
- **Acceptance signal**: a promotion version on a binding rates through the step-4 coupons slot and replays
  identically.

### 2.11 Orders Lifecycle

Orders Lifecycle is a **consumer** of a Rating-provided contract, not a provider.

| # | Contract | Evidence | Status |
|---|---|---|---|
| G-6 | Order-time evaluation: one batched ClientHub call over the exact accepted chain matrix — request `{assessment_id, resolve_date, tenant axes, lines[{line_id, plan_revision_id, items[{item_id, quantity, chains[{dim_value, binding}]}]}]}`, response per item/line/order gross/net/discount, three charge kinds, minimum-fee application, currency/scale/rounding, net pre-tax TCV (×12 `month`, ×1 `year`), integer minor units (retired by the proposed money ADR: display totals become major-unit decimals marked as estimates, T-D-80); no renewal walk; failure `evaluation-unavailable`; Atlas C06 `acceptance_ref`/`ExactAmount` not consumed (Orders D-154, D-167) | `gears/bss/orders-lifecycle/docs/UPSTREAM_REQS.md:303-351`; `DESIGN.md:6705-6726`; `DECISIONS.md:3143, 3161` | **adopted** by Rating as T-D-79 (proposed); Orders confirms |
| G-15 | Accepted order pin `OrderPin` (`orders_order_line.order_pin`): `plan_revision_id`, `book_id`, currency/scale/rounding, `items[]` with `sku_version`, descriptor snapshot and `chains[]{dim_value, uncovered, binding}`; at activation Subscriptions resolves with the consumed slots as pins and stores the result as the first period's pins, else refuses `accepted-price-mismatch` (D-162) | `gears/bss/orders-lifecycle/docs/DESIGN.md:6511-6604`; `DECISIONS.md:3153, 3156` | DOCUMENTED (Orders and Subscriptions have no code) |

Orders' acceptance model lags Pricing on `main`: Orders D-162/D-177/Q-32 say Pricing has no receipt,
no hold and no `SellabilityV1` (`gears/bss/orders-lifecycle/docs/DECISIONS.md:3156, 3171, 3311`),
while `pricing-sdk/src/acceptance.rs` defines and registers them (P-27). This is Orders' and
Pricing's to reconcile; Rating only consumes the bindings that reach it through Subscriptions'
facts. Orders also calls `pricingSnapshotRef` a retired term (`orders-lifecycle/docs/PRD.md:127`).

#### Pre-purchase evaluation request shape and priority

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-upreq-orders-evaluation-dto`

Orders Lifecycle **MUST** confirm Rating's adoption of its D-167 request and response (T-D-79):
the response returns every amount both exact and as a major-unit display decimal rounded HALF_EVEN
at the currency scale and marked as a non-authoritative estimate (T-D-80; the proposed money ADR
retires integer minor units, so Orders' D-40/D-167 storage moves to that decimal); and both gears **MUST** agree the priority of
`cpt-cf-bss-rating-fr-pre-purchase-evaluation` (p1 for Orders, which also asks it to lose its
catalog-version prefix).

- **Rationale**: Rating adopted Orders' shape instead of Atlas C06 (overlay ticket T3); the remaining
  difference is that Rating's amounts are exact and Orders' documents still store integer minor
  units verbatim, which the money ADR replaces with major-unit decimals marked as estimates.
- **Source**: `gears/bss/rating` — [11 §4.10](./DESIGN.md#contract-11-4-10); feature [`10-order-evaluation`](./features/10-order-evaluation.md).
- **Status**: Rating side adopted (T-D-79, proposed); Orders confirmation open.
- **Register aliases**: DECISIONS R-24; overlay ticket T3.
- **Acceptance signal**: both gears' documents name the same DTO and priority.

### 2.12 Platform infrastructure

| # | Contract | Evidence | Status | Consequence for Rating |
|---|---|---|---|---|
| A-1 | ClientHub typed clients (`register`, `register_scoped`, `get`, `get_scoped`), in-process or OoP gRPC | `libs/toolkit/src/client_hub.rs:152-273`; `docs/toolkit_unified_system/03_clienthub_and_plugins.md`, `09_oop_grpc_sdk_pattern.md` | CONFIRMED | All inter-gear reads are SDK calls. |
| A-2 | `toolkit_db::outbox`: `Outbox::builder(db).queue(name, Partitions::of(n)).leased(handler)`, `Outbox::enqueue(&db, Record)`, per-partition sequencing after commit, dead letters | `libs/toolkit-db/src/outbox/` (`core.rs:446`) | CONFIRMED | Work queues and the delivery sequence. |
| A-3 | Cross-gear event delivery | the event-broker gear is **implemented** (`c7de7b80a`, 2026-09-23: dispatcher routing, REST handlers, standalone single-process service; `gears/system/event-broker/event-broker/src/module.rs:1-14` wires ingest, delivery, dispatcher and reaper; a `sqlite-event-broker-plugin` exists); Pricing publishes through `event_broker_sdk::ProducerOutbox` (`gears/bss/pricing/pricing/src/infra/events.rs:51-58`) and Products through `ProducerOutboxHandle` with an interim fallback (`gears/bss/products/products/src/gear.rs:136-140`); the ledger publisher is still parked (`ledger/ledger/Cargo.toml:71-73`) | **CONFIRMED** (broker); whether a deployment registers it is UNKNOWN | Rating still never depends on events alone: inbox-first (T-D-56), pull reads authoritative; producer contracts for the events Rating would consume do not exist (`…-upreq-event-contracts`). |
| A-4 | `bss-coord` leases (`LeaseManager::acquire(key, ttl) -> LeaseGuard`, `renew`, `with_ack_in_tx`, `spawn_renewal`) | `gears/bss/libs/coord/src/lease/manager.rs:46-61` | CONFIRMED | **Not Rating's target** (DESIGN §3.5): only a MIGRATION adapter behind Rating's `LeaseProvider` port if R-27 approves it, with an owner and a removal milestone. |
| A-5 | SecureORM / `PolicyEnforcer` / `AccessScope`; `DBRunner`, `in_transaction_mapped` | `docs/toolkit_unified_system/06_*.md`, `11_database_patterns.md` | CONFIRMED | All repositories. |
| A-6 | Consumer offsets committed with a DB transaction (`LocalDbOffsetManager`, `TxCommitHandle`) | `event-broker-sdk/src/consumer/offset_manager.rs:165` | CONFIRMED (SDK) | TARGET event intake commits offset + inbox atomically. |
| A-7 | `cluster-sdk` distributed locks (`DistributedLockApi::{try_lock, lock, renew, release}` on a fenced `LeaseToken`) and leader election (`LeaderElectionApi::{join, renew, resign}`); backends as plugins (standalone, postgres, redis, k8s) | `gears/system/cluster/cluster-sdk/src/contract.rs:199-300`; `gears/system/cluster/plugins/`; the Cluster Plane owns cross-instance coordination (`docs/GEARS.md`) | SDK CONFIRMED; binding of a backend in the BSS deployment profile UNKNOWN | **Rating's target** for single-active feed readers, scheduler/wake-scanner leaders and the reconciler. Leases only save work; the database CAS and unique keys stay the correctness fence. |

#### Coordination backend in the BSS deployment profile

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-upreq-cluster-coordination-backend`

The platform (Cluster Plane) **MUST** bind a `DistributedLockApi` / `LeaderElectionApi` backend for BSS
gears, or Platform/Cluster and BSS **MUST** approve a transitional `bss-coord` adapter behind
Rating's `LeaseProvider` port with an owner and a removal milestone.

- **Rationale**: Rating's feed readers, scheduler and wake-scanner leaders and reconciler need
  leases (DESIGN §3.8); Rating does not choose a backend alone.
- **Source**: `gears/bss/rating` — DESIGN §3.5, §3.8; feature [`01-foundation`](./features/01-foundation.md).
- **Status**: SDK CONFIRMED; profile binding UNKNOWN (A-7).
- **Register aliases**: J-16; DECISIONS R-27.
- **Acceptance signal**: the BSS deployment profile resolves `DistributedLockApi` from ClientHub, or
  the adapter approval names its owner and removal milestone. **Mandatory before Rating's first
  deployment.**

#### Producer event contracts

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-upreq-event-contracts`

Each producer of an event Rating consumes — Subscriptions (`BillableFactPublished`,
`BillableSetSealed`, `AttributionSegmentChanged`, `UsageScopeSealed`), the usage emitter
(`MeterCoverageDeclared`) and Billing (`BillingPeriodStateChanged`) — **MUST** define its typed payload
schema, GTS event type, topic instance, partition key, dedup identity, retention floor and evolution
policy. Rating states what it needs (DESIGN §3.3 event contracts) and defines none of them.

- **Rationale**: every event path stays disabled until both sides adopt one contract (T-D-71); pull
  reads stay authoritative. The broker now delivers (A-3), but none of these producers has code or a
  typed payload yet.
- **Source**: `gears/bss/rating` — DESIGN §3.3; feature [`01-foundation`](./features/01-foundation.md).
- **Status**: REQUESTED; needed only to enable an event path.
- **Register aliases**: J-15.
- **Acceptance signal**: an event path is enabled by configuration and passes the inbox
  idempotency vectors with the producer's events.

### 2.13 Account management and tenant resolver

| # | Contract | Evidence | Status |
|---|---|---|---|
| G-7 | Tenant directory | `AccountManagementClient::get_tenant` (`account-management-sdk/src/client.rs:127`), `TenantResolverClient` (`tenant-resolver-sdk/src/api.rs:44-127`) | CONFIRMED; commercial axes come from Subscriptions (`fr-tenant-axes`); AM tenant types (`provider/reseller/customer`) differ from ledger seller defaults (`vz.ams.tenants.partner/platform`, `ledger/src/config.rs:42-43`) |
| G-8 | OrgTier, customer-group membership | pricing taxonomies only (`m20260821_000012/13/17`) | CONFIRMED in pricing; no runtime membership read for Rating (`unresolvable_scope`) |

#### Tenant-deletion signal

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-upreq-tenant-deletion-signal`

account-management **MUST** define a tenant-deletion signal and its semantics that consumers can act on.

- **Rationale**: on deletion Rating stops intake and cancels queued work while keeping financial
  records until retention expiry (DESIGN §4.12); without a signal it can only keep records.
- **Source**: `gears/bss/rating` — DESIGN §4.12; feature [`11-operations`](./features/11-operations.md).
- **Status**: UNKNOWN / EXTERNAL CONTRACT REQUIRED.
- **Register aliases**: J-18.
- **Acceptance signal**: Rating's tenant-deletion test consumes the defined signal.

### 2.14 Shared fixtures

| # | Contract | Evidence | Status |
|---|---|---|---|
| H-1 | `bss_fixtures_conformance::CorpusEvaluator { evaluate, supported_families }` | `gears/bss/fixtures/` was **deleted** in pricing phase 4 with the legacy joint corpus, proration family included (DECISIONS T-D-37 amendment) | **SUPERSEDED** — Rating authors its own proration and floor vectors; pricing's golden contracts and seam fixtures (P-28) are the shared vectors |
| H-2 | Band amount scale: corpus `unit_amount_minor` vs catalog `unit_price_nano` | corpus deleted; pricing money is major-unit decimal (P-22) | **SUPERSEDED** |
| H-3 | K5 anchor fixture, `lineKey` fixture | none | MISSING (Subscriptions-side) |
| H-4 | Atlas acceptance scenarios F01–F35 | Atlas page (not vendored) | PROPOSED — Rating-applicable vectors copied into the feature acceptance criteria |

#### Corpus band scale

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-upreq-fixtures-band-scale`

**Closed 2026-10-08 — not applicable.** The joint corpus whose band scale differed was deleted with
`gears/bss/fixtures/` (H-1, H-2); pricing money is exact major-unit decimal (P-22).

- **Register aliases**: J-9.

### 2.15 Closed and superseded asks

| Former ask | Owner | What it asked | Status |
|---|---|---|---|
| J-2 | pricing | implement and register `PricingCatalogClientV1`; add plan and overlay document reads at a pin | **superseded** by T-D-37: `main` ships `PricingReadV1::{resolve, price, current_revision}` (P-15, P-16); DECISIONS R-02 closed; Rating's adapter is R-17 (Rating-owned) |
| J-3 | products registry | implement `CatalogVersionRegistryV1` and `CatalogVersionPublished` | **superseded** by T-D-37 (catalog-version allocation is not part of the PriceBook model; G-4) |
| J-7 (`…-upreq-pricing-meter-binding`) | pricing | validate a usage row's `meter` at publish | **closed 2026-10-08** — meters are Products derived usage types (P-D-259) checked by Pricing through `UsageMeterSemanticsV1` (D-503); DECISIONS R-09 closed |
| J-10 (`…-upreq-pricing-validator-hook`) | pricing | a validator registration hook in pricing Slice 5 | **closed 2026-10-08 — not applicable** — approval is `bss-approval`; Rating has no role; DECISIONS R-12 closed |
| J-12 (`…-upreq-pricing-aggregation-scope`) | pricing | an aggregation scope per price | **closed 2026-10-08 — delivered** by `usage_rating_policy.content.aggregation_scope` (P-21); DECISIONS R-23 closed |
| `…-upreq-lifetime-settlement-model` | Pricing, Finance, Billing | a settlement model for lifetime windows | **closed 2026-10-08 — not applicable** — no lifetime window exists (P-21); DECISIONS R-28 closed |
| J-9 (`…-upreq-fixtures-band-scale`) | shared fixtures | align the corpus band scale | **closed 2026-10-08 — not applicable** — the corpus was deleted (H-1) |

## 3. Seam Atlas v2 baseline status

The Seam Atlas v2 contract baseline 1.1 (rev 8, 2026-09-30) defines contracts C00–C10. It states
itself that every red contract is "specified here, no code" and that nothing has passed an
end-to-end fixture. Its **Plus overlay** (2026-10-02) keeps the contract pack unchanged and adds a
second grade per seam ("written down": whether both owners' documents carry the same contract),
twelve decision tickets (mapped in [`DECISIONS.md`](./DECISIONS.md)), 24 unlisted hops and an errata
list. The Atlas was audited against `diffora/gears-rust @ 01f670fa4e5c`. Status below is for
**this** repository; the adoption column names the decision that adopts the position.

| Contract | Owner → consumer | Atlas content relevant to Rating | Status here | Overlay grade (2 Oct) | Rating adoption |
|---|---|---|---|---|---|
| C00 shared rules | all | `SecurityContext`, `CommonError`, idempotency scope, `EventEnvelope`, at-least-once + inbox, `ExactAmount`, `TenantAxes` | PROPOSED; toolkit conventions CONFIRMED | Atlas only | adopted (T-D-51, T-D-56) |
| C01 accepted pricing | Pricing → Orders, Subscriptions, Rating | `PricingReadV1::{resolve, price, hold, acceptance}`, `SellabilityV1`, `AcceptanceReceipt`, `UsageRatingPolicy` | **CONFIRMED on `main`**: `PricingReadV1::{resolve, price, current_revision}`, `AcceptedBinding`, `UsageRatingPolicy`, `SellabilityV1`, `PricingAcceptanceV1::{acceptance, hold}`, `AcceptanceReceipt` (P-15…P-27) | Written down (pricing) | adopted (T-D-37); Rating's adapter designed (T-D-73…T-D-78, R-17 closed); Rating calls only `resolve` and `price` |
| C02 Orders lifecycle | Orders → Portal, Workflow | — | PROPOSED | Contested | not Rating's |
| C03 Subscriptions fulfilment | Subscriptions → Workflow, Orders | activation validates billing terms and usage policy | PROPOSED; pricing defines `BillingTerms` as Subscriptions-owned (S-10); Orders D-162 activation check | Contested | consumed indirectly (the fact's pricing inputs, `…-upreq-subscriptions-fact-pricing-inputs`) |
| C04 billable facts, attribution, sealed composition | Subscriptions → Rating, Billing | `BillableFactPublished`, `BillableSetSealed`, `AttributionSegmentChanged`, `UsageScopeSealed`, `SubscriptionBillingReadV1` | PROPOSED; S-2 documented, S-3/S-7/S-8 absent; per-period pins decided (SUB-D-29) but carried on no fact (S-9); conflicts R-20, R-25 | Atlas only | adopted as TARGET (T-D-49, T-D-57, T-D-73) |
| C05 usage feed, coverage, inventory | Collector, IRM → Rating, Subscriptions | `UsageFeedV1`, V2 interval writes, invalidations, `MeterCoverageReadV1`, `ResourceHistoryV1`, finalization rule, boundary splits | feed: the collector's own design supersedes it (U-1…U-3, overlay: withdraw `UsageFeedV1`); coverage/inventory ABSENT (U-10/U-11) | Uncovered | collector feed adopted (R-01); finalization rule and boundary splits adopted (T-D-52, T-D-53); coverage owner open (R-21, T6) |
| C06 pre-purchase evaluation | Rating → Orders | `OrderEvaluationV1::evaluate` | PROPOSED; Orders asks its own shape instead (D-167, G-6) | Contested | Rating adopts Orders' D-167 shape, not C06's (T-D-54 refined by T-D-79) |
| C07 deterministic run + replacement delivery | Rating → Billing | `RatingRunReadV1`, `RatingRunControlV1`, `WindowEvaluationKey`, `WindowResult`, `WindowManifest`, `BillableItemDeliveryV1`, input-generation CAS | Rating-owned | One-sided | adopted (T-D-49, T-D-50, T-D-58); exact amounts in major units (T-D-51 as amended by T-D-80); durable re-rate targets and freshness-gated roll-up (T-D-63, T-D-64) |
| C08 Billing acceptance + posting | Billing, Ledger | per-fact heads, aggregate rounding, ordered posting obligations | PROPOSED; no Billing gear | One-sided | Rating's dependency (`…-upreq-billing-delivery-consumer`) |
| C09 event names + recovery | per producer | event vocabulary, recovery reads, retention ≥ largest delay + outage | PROPOSED; the broker delivers (A-3) but no producer Rating consumes has a typed payload | Contested | adopted as inbox sources (T-D-56); producer contracts requested (`…-upreq-event-contracts`), Rating's delivery event specified (T-D-71) |
| C10 hourly windows + monthly composition | Pricing, Subscriptions, Rating, Billing | Rating-owned scheduler, `FinalizationPolicy`, `RatingWindow`, `AggregationKey`, `WindowSchedule`, hourly math | Pricing side **CONFIRMED**: `usage_rating_policy.content{rating_window: BillingCycle \| CalendarHour{Utc}, aggregation_scope, reset, partial_window, fold}` (`UsageRatingPolicy{policy_id, version, digest, content}`; P-21); pricing D-509/D-510 assign hourly scheduling, reset, catch-up and exact amounts to Rating and rounding to Billing | Written down (pricing) | adopted (T-D-49, T-D-52, T-D-53, T-D-75); floor mechanism T-D-78 (hourly and resource-scoped entries carry no floor, D-503/D-504); included quantities removed (D-467); no lifetime or per-event window exists (R-28 closed) |

Errata the overlay lists that touch Rating: three event vocabularies and two
`BillingPeriodStateChanged` enums. Rating uses the C09 names for TARGET and Subscriptions' documented
names for CURRENT; the hint enum is `open | frozen | posted` (C09).

## 4. Priorities

| Priority | Requirements |
|----------|-------------|
| `p1` (critical) | `…-upreq-usage-feed-v1`, `…-upreq-coverage-declarations`, `…-upreq-resource-history`, `…-upreq-pricing-read-grant`, `…-upreq-products-derived-declaration-read`, `…-upreq-subscriptions-versioned-facts`, `…-upreq-subscriptions-fact-pricing-inputs`, `…-upreq-subscriptions-usage-fact`, `…-upreq-subscriptions-attribution-segments`, `…-upreq-subscriptions-scope-proofs`, `…-upreq-subscriptions-recovery-reads`, `…-upreq-billing-delivery-consumer`, `…-upreq-billing-invoice-line-contract`, `…-upreq-billing-period-ownership`, `…-upreq-delay-only-acceptance`, `…-upreq-cluster-coordination-backend` |
| `p2` (important) | `…-upreq-usage-reconciliation-access`, `…-upreq-usage-type-declaration-read`, `…-upreq-pricing-proration-confirmation`, `…-upreq-pricing-numeric-limits`, `…-upreq-products-derived-output-limits`, `…-upreq-dimension-encoding`, `…-upreq-level-meter-rule`, `…-upreq-subscriptions-ends-on-cut`, `…-upreq-subscriptions-one-time-rating`, `…-upreq-accepted-binding-naming`, `…-upreq-fx-rate-snapshots`, `…-upreq-contracts-inputs`, `…-upreq-promotions-coupon-snapshots`, `…-upreq-orders-evaluation-dto`, `…-upreq-event-contracts` |
| `p3` (nice-to-have) | `…-upreq-billing-ledger-item-granularity`, `…-upreq-ledger-credit-note-wording`, `…-upreq-tenant-deletion-signal` |
| closed (2026-10-08) | `…-upreq-pricing-meter-binding`, `…-upreq-pricing-validator-hook`, `…-upreq-pricing-aggregation-scope`, `…-upreq-lifetime-settlement-model`, `…-upreq-fixtures-band-scale` (§2.15) |

`p1` asks block a launch capability in DESIGN §4.10; their feature-level effect is in
[`DECOMPOSITION.md`](./DECOMPOSITION.md) §3.1. Development against the contract fakes of
[`01-foundation`](./features/01-foundation.md) can proceed meanwhile; a fake never satisfies a
production dependency.

## 5. Traceability

- **PRD**: [`PRD.md`](./PRD.md) — §13 dependencies, §15 open questions.
- **DESIGN**: [`DESIGN.md`](./DESIGN.md) §0 (labels), §3.3 (event contracts), §3.5 (external dependencies), §4.6, §4.10 (launch capability matrix).
- **Decisions**: [`DECISIONS.md`](./DECISIONS.md) — R-01, R-03…R-07, R-09, R-11, R-12, R-16, R-17, R-19…R-33; T-D-18, T-D-33, T-D-36…T-D-39, T-D-50, T-D-51, T-D-52, T-D-56, T-D-57, T-D-62, T-D-71, T-D-73…T-D-80.
- **Cross-BSS money ADR** (proposed, [PR #5270](https://github.com/constructorfabric/gears-rust/pull/5270), head `0a0f6d7a3`): the target for money form, limits, storage and rounding (T-D-80).
- **Dependency evidence** (2026-10-08): pricing `gears/bss/pricing` (D-384…D-427, D-437, D-467, D-501…D-514, D-520, D-521), products `gears/bss/products` (P-D-229…P-D-259, upstream P-D-265), subscriptions SUB-D-28/29, orders-lifecycle D-154/D-162/D-167.
- **Decomposition**: [`DECOMPOSITION.md`](./DECOMPOSITION.md) §3.1 (which feature each ask blocks).
- **Downstream register**: Orders Lifecycle's asks of Rating are in `gears/bss/orders-lifecycle/docs/UPSTREAM_REQS.md` §2.2.
