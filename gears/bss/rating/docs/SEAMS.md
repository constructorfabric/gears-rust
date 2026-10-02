<!-- CONFLUENCE_TITLE: [BSS]: Rating — Cross-Gear Contract Inventory -->
<!-- Related: ./DESIGN.md, ./DECISIONS.md, ./PRD.md | Owners: BSS Rating team -->

# Rating — Cross-Gear Contract Inventory

<!-- toc -->

- [Status legend](#status-legend)
- [A. Platform infrastructure](#a-platform-infrastructure)
- [B. Usage Collector and IRM](#b-usage-collector-and-irm)
- [C. Pricing and Products registry](#c-pricing-and-products-registry)
- [D. Subscriptions](#d-subscriptions)
- [E. Billing orchestration and Ledger](#e-billing-orchestration-and-ledger)
- [F. FX](#f-fx)
- [G. Contracts, Promotions, Orders, Tenant identity](#g-contracts-promotions-orders-tenant-identity)
- [H. Shared fixtures](#h-shared-fixtures)
- [I. Adopted pricing semantics](#i-adopted-pricing-semantics)
- [J. Required dependency changes](#j-required-dependency-changes)
- [K. Documentation / code mismatches found](#k-documentation--code-mismatches-found)
- [L. Seam Atlas v2 baseline — status in this repository](#l-seam-atlas-v2-baseline--status-in-this-repository)
- [M. Artifact impact analysis](#m-artifact-impact-analysis)
  - [M-1 Commercial facts drive work; Rating owns the window schedule (Atlas D13, D15, C04, C10)](#m-1-commercial-facts-drive-work-rating-owns-the-window-schedule-atlas-d13-d15-c04-c10)
  - [M-2 Absolute replacement results; Billing owns period state and rounding (Atlas D09, D10, C07, C08)](#m-2-absolute-replacement-results-billing-owns-period-state-and-rounding-atlas-d09-d10-c07-c08)
  - [M-3 Completeness evidence (Atlas D07, C05, P7)](#m-3-completeness-evidence-atlas-d07-c05-p7)
  - [M-4 Hourly tiers and window scope (Atlas D14, C10, F23–F29)](#m-4-hourly-tiers-and-window-scope-atlas-d14-c10-f23f29)
  - [M-5 Usage feed and interval records (Atlas C05; revised 2026-10-02)](#m-5-usage-feed-and-interval-records-atlas-c05-revised-2026-10-02)
  - [M-6 Attribution (Atlas C04, P2, P3, P11)](#m-6-attribution-atlas-c04-p2-p3-p11)
  - [M-7 Pricing model and price binding (Atlas C01, D02, D06)](#m-7-pricing-model-and-price-binding-atlas-c01-d02-d06)
  - [M-8 One-time charges (Atlas R2)](#m-8-one-time-charges-atlas-r2)
  - [M-9 Pre-purchase evaluation (Atlas D04, C06)](#m-9-pre-purchase-evaluation-atlas-d04-c06)
  - [M-10 Level meters (Atlas R1)](#m-10-level-meters-atlas-r1)
  - [M-11 Event transport and recovery (Atlas C00, C09, P8, P11)](#m-11-event-transport-and-recovery-atlas-c00-c09-p8-p11)
  - [M-12 Floors and included quantities (Atlas C07, C10, K3)](#m-12-floors-and-included-quantities-atlas-c07-c10-k3)
  - [M-13 Seam Atlas v2 Plus overlay (2026-10-02)](#m-13-seam-atlas-v2-plus-overlay-2026-10-02)

<!-- /toc -->

This is the single place that records **what each dependency actually provides** to Rating, verified
against code first and documentation second (revalidated 2026-10-01; usage-collector re-verified
2026-10-02 at upstream `main` `b7a2f8281`). [`DESIGN.md`](./DESIGN.md) states the design; this file
states whether the contracts the design uses exist, and §L–§M record how the **Seam Atlas v2
contract baseline 1.1** (2026-09-30) and its **Plus overlay** (2026-10-02) relate to them. Paths are
relative to the repository root unless a gear prefix is obvious.

**Evidence base.** This branch is based on `8aca4d6df`, 803 commits behind upstream `main`. Between
them only the usage-collector changed in a way that matters to Rating (commit `5de85f067`, "rework
the metering model"); pricing, subscriptions, IRM and the Rating gear itself are unchanged, and the
ledger changed only in reconciliation. §B therefore cites upstream `main` for the usage-collector's
documents; rebase this branch before merge.

## Status legend

| Status | Meaning |
|---|---|
| **CONFIRMED** | Exists in code and matches what Rating needs. |
| **PARTIAL** | Exists in code but lacks something Rating needs. |
| **MISSING** | Specified in a dependency's docs (or needed by Rating) but not implemented. |
| **CONFLICTING** | Documentation and code, or two specifications, disagree. |
| **ASSUMED** | The dependency has no implementation at all; the contract exists only in design prose. |
| **PROPOSED** | Exists only in the Seam Atlas (or as a Rating requirement); no owner document adopts it. |
| **ABSENT** | Neither code nor any owner document. |

## A. Platform infrastructure

| # | Contract | Evidence | Status | Consequence for Rating |
|---|---|---|---|---|
| A-1 | ClientHub typed clients (`register`, `register_scoped`, `get`, `get_scoped`), in-process or OoP gRPC | `libs/toolkit/src/client_hub.rs:152-273`; `docs/toolkit_unified_system/03_clienthub_and_plugins.md`, `09_oop_grpc_sdk_pattern.md` | CONFIRMED | All inter-gear reads are SDK calls. |
| A-2 | `toolkit_db::outbox`: `Outbox::builder(db).queue(name, Partitions::of(n)).leased(handler)`, `Outbox::enqueue(&db, Record)`, per-partition sequencing after commit, dead letters | `libs/toolkit-db/src/outbox/` (`core.rs:446`) | CONFIRMED | Work queues and the delivery sequence. |
| A-3 | Cross-gear event delivery | `event-broker-sdk` implemented with a mock backend (`gears/system/event-broker/event-broker-sdk/src/api.rs:442-575`: publish, consumer groups, `seek` with `ResolvedPosition::{Exact, Earliest, Latest, AtTimestamp}`); **broker gear is a scaffold** (`event-broker/src/module.rs:1-11`, 24 unimplemented-macro stubs); pricing outbox has no relay (`pricing/src/module.rs:1210-1215`); ledger publisher parked (`ledger/src/infra/events/publisher.rs:1-19`) | SDK CONFIRMED; delivery **MISSING** | Rating never depends on events alone; inbox-first (T-D-51), pull reads authoritative. |
| A-4 | `bss-coord` leases (`LeaseManager::acquire(key, ttl) -> LeaseGuard`, `renew`, `with_ack_in_tx`, `spawn_renewal`) | `gears/bss/libs/coord/src/lease/manager.rs:46-61` | CONFIRMED | Single-active readers/scheduler shards; not a fencing token — Rating fences with CAS. |
| A-5 | SecureORM / `PolicyEnforcer` / `AccessScope`; `DBRunner`, `in_transaction_mapped` | `docs/toolkit_unified_system/06_*.md`, `11_database_patterns.md` | CONFIRMED | All repositories. |
| A-6 | Consumer offsets committed with a DB transaction (`LocalDbOffsetManager`, `TxCommitHandle`) | `event-broker-sdk/src/consumer/offset_manager.rs:165` | CONFIRMED (SDK) | TARGET event intake commits offset + inbox atomically. |
| A-7 | `cluster-sdk` leader election / distributed locks | `gears/system/cluster/…/contract.rs:73,199` | CONFIRMED | Alternative to `bss-coord` leases; not required. |

## B. Usage Collector and IRM

`gears/system/usage-collector`. On upstream `main` its documents describe a breaking pre-1.0 **target
V1** (PRD, DESIGN §3.3, 15 accepted ADRs, JSON schemas); its code — SDK, gear and plugins, here and
upstream — still implements the superseded point-record model. The collector records this itself:
"This document is normative, and the code is not a second source" (DESIGN:2296); the REST YAML is
`x-contract-status: unreleased`. IRM (`gears/infrastructure-resource-manager`) is **documents only**.
References below are to upstream `main` unless marked "code".

| # | Contract | Direction / mode | Fields / semantics | Evidence | Status |
|---|---|---|---|---|---|
| U-1 | **Usage feed** `read_usage_feed(ctx, &FeedSubscription, FeedStart<&CursorV1>, until: Option<&CursorV1>, limit) -> FeedPage`; REST `GET /usage-collector/v1/feed` | UC → Rating, pull | subscription = 1..100 GTS types; `FeedStart::{Oldest, After(cursor)}`; `until` bounds a replay (identical entry for entry); limit 1..1 000; pages carry only settled entries, nothing appears behind a returned cursor; prefix-stable, **no snapshot token**; one deterministic plugin-chosen order, only "an invalidation follows its target"; `next_cursor` never null on a live read | DESIGN:536-600, 919-932, 1127-1142, 1355; ADR-0011; YAML:315-371 | **DOCUMENTED**, not implemented — code (`usage-collector-sdk/src/api.rs:17-110`) has create/get/list/aggregate/deactivate/usage-types only |
| U-2 | Entry identity + dedup | UC | dedup identity `(tenant_id, gts_type_id, idempotency_key, window_start, window_end, entry_type)`; `id = UUIDv5(NS 56313026-…, that tuple)`, reproducible offline; consumers MUST use it as their dedup and reference key | ADR-0007; DESIGN:578; PRD:547 | DOCUMENTED; code: UUIDv5 over `(tenant, gts_id, created_at, key)` (`id.rs:41-58`) — DOC/CODE DRIFT |
| U-3 | Corrections | UC → Rating | invalidation = entry with `entry_type = invalidation`, faithful copy, server-stamped `invalidates`, `reason_code`, quantity echoed; at most one per record; no invalidation of an invalidation; replacement = invalidation + fresh record under a new key, **not atomic** — consumers must not treat the in-between state as settled | ADR-0010; DESIGN:579-594; PRD:436-438 | DOCUMENTED; code: `deactivate_usage_record` status flip + `corrects_id` compensation (being removed) |
| U-4 | Fold; **only `SUM` is chargeable**; levels pre-integrated at the emitter | types-registry declaration | consumer applies the declared fold; excludes both entries of a withdrawn pair | ADR-0009, ADR-0011; PRD:288 | **CONFLICTING** with Rating T-D-17 / pricing D-44 — R-06 (Atlas R1 agrees with UC) |
| U-5 | Aggregate / query paths | UC | eventually consistent, never a charging input ("a consumer that computes money reads entries, not aggregates") | ADR-0011 | not used by Rating |
| U-6 | Retention floor = backfill window (90 d) + replay horizon (35 d) = 125 days from `window_end`; refusal `InvalidArgument(CursorBeyondRetention)` depends on what the store holds, not cursor age; a polled cursor is never refused | UC | — | PRD:683-699; DESIGN:927-932 | DOCUMENTED; plugin today: one deployment-wide TTL |
| U-7 | Commercial identity is the consumer's | UC | `resource_ref {resource_id, resource_type}`, `subject_ref?`, metadata | PRD:513, 1072 | CONFIRMED by design — **CONFLICTING** with Subscriptions S-PRD:360 (R-25) |
| U-8 | Interval entries | UC | half-open `[window_start, window_end)` UTC, µs; equal bounds = point event; `accepted_at` and `origin` server-assigned; quantity decimal string ≤ 28 fractional digits (ADR-0013; the ClickHouse plugin stores 9 — a documented plugin deviation) | YAML:787-889; DESIGN:547-586 | DOCUMENTED; code: `created_at` instant only |
| U-9 | Usage type declarations in types-registry: `aggregation_fold`, `canonical_unit`, `retention`, optional `nominal_sampling_interval`; the collector serves no type reads | types-registry → consumers | — | ADR-0008; `schemas/usage_record.v1.schema.json:44-65`; DESIGN:428-437 | DOCUMENTED; no typed read trait returns fold/unit (Atlas Plus X7); code still has an in-collector counter/gauge catalog |
| U-10 | Coverage declarations (`MeterCoverageDeclared`, `MeterCoverageReadV1`) | emitter → Rating | count, sum, record-set digest, `final` | Atlas C05 only; collector "never judges completeness" (PRD:749) | **ABSENT** — no owner (overlay T6) |
| U-11 | Historical inventory barrier (`ResourceHistoryV1`), `ResourceReady`/`ResourceDeleted` | IRM → Subscriptions | resources overlapping a period incl. deleted | Atlas C05/C09; IRM DESIGN `:816-827` (lifecycle event without `ready_at` or sequence until p3) | **ABSENT** (Atlas Plus X10) |
| U-12 | Reconciliation metadata `GET /usage-collector/v1/reconciliation` (`accepted_count`, quantity summary, `accepted_at_watermark`, `window_end_watermark`) | UC → operators | REST-only, operator-scoped, no trait; watermarks "prove nothing about completeness" | DESIGN:1052, 1356-1362; YAML:373-424, 1238-1320; ADR-0011 | DOCUMENTED; Rating's service-identity access UNKNOWN |
| U-13 | Backfill route `backfill_usage_records` / `POST /records/backfill` | emitter → UC | live path refuses periods ending > 48 h ago; backfill up to 90 d, `origin = backfill` | DESIGN:665-671, 1082-1086 | DOCUMENTED |
| U-14 | Consumer obligations | Rating | dedup by entry id across at-least-once delivery; persist the rated usage identity per charge; replay from `Oldest` after a widened authorization scope; stall detection is the consumer's | PRD:547, 669, 749, 1503-1504 | adopted (slice 12) |

## C. Pricing and Products registry

`gears/bss/pricing` (crates `pricing`, `pricing-sdk`). In pricing code and docs "Tariffs" means
Rating's evaluation core (ADR-0002). Pricing's decision register ends at D-367.

| # | Contract | Version semantics | Evidence | Status |
|---|---|---|---|---|
| P-1 | `PricingCatalogClientV1::pin_frontier(ctx) -> Result<Option<PinFrontier { catalog_version, advanced_at }>, CanonicalError>` | `CatalogVersion(u64)` monotonic per tenant, prefix-closed pin-eligibility (D-114, D-136) | `pricing-sdk/src/api.rs:17-43`; D-347 | **PARTIAL** — declared, no implementation, not registered |
| P-2 | Plan / overlay documents at a catalog version (`domain/projection.rs:595-1290`: plan, price and row fields incl. `tierAggregationWindow`, `billingAnchorPolicy`, `prorationBasis`, `descriptorSet{glCode, invoiceLineTemplate}`, `taxCategoryRef`, `usageCounterOnPlanChange`, `periodFloorCaps`) | read model INSERT-only per `(tenant, catalog_version, subject)` | `read_model_repo.rs:203` (`delta_at`, internal) | **MISSING** — no SDK method or route; `/plans/{planId}/preview` reads the frontier only |
| P-3 | Future-only effective dating and historical immutability | `WINDOW_START_IN_PAST`, `WINDOW_HISTORICAL_IMMUTABLE`, `SUPERSESSION_INSTANT_PASSED`, append-only price trigger, frozen revisions | `domain/window.rs`, `infra/error_mapping.rs:356,536` | **CONFIRMED** — basis of the pin rule |
| P-4 | Canonical scope key, 10 axes | — | `domain/scope_key.rs:693-771` | CONFIRMED (adopted, ADR-0001) |
| P-5 | Validator registration in the Slice 5 pipeline | — | none | ASSUMED (R-12) |
| P-6 | `PricingSnapshotRef { version_ref, price_ids, evaluation_policy_version }` | — | `domain/snapshot.rs:152-156` (test-only) | CONFLICTING — Rating owns its snapshot (T-D-39) |
| P-7 | Catalog events (`CatalogEvent`, 14 names: `PlanPublished`, `PriceWindowScheduled/Activated/Expired/Cancelled`, `PriceOverlayPublished`, …) | per-aggregate | `domain/events.rs:33-77`; payloads `outbox_repo.rs:143-1354` | written, **never delivered** (A-3); not consumed by Rating |
| P-8 | Money scales: `MinorAmount(i64)`; `RateMinor(i64)` = 10⁻⁹ minor | — | `domain/money.rs`; D-311 | CONFIRMED — inputs to exact arithmetic (T-D-46) |
| P-9 | Enums (`ModelKind`, `TierAggregationWindow {CalendarMonth, InvoicePeriod, SubscriptionLifetime, PerEvent, PerHour}`, `AggregationFunction`, `BillingGranularity`, `QuantitySource`, `ReservationFlavor`, `BillingAnchorPolicy`, `ProrationBasis`, `UsageCounterOnPlanChange`, `ScopeClass`, `AdjustmentKind`) | — | `fixtures/bss-fixtures/src/kinds.rs`, `domain/{price_row,contracts,overlay}.rs` | CONFIRMED |
| P-10 | `tierQualificationWindow = trailing_period`, `includedAllowance` carry | refused at publish | `domain/evaluation_policy.rs:51-76`; `projection.rs:66-90` | CONFIRMED refused |
| P-11 | `billingTiming` values | `advance \| arrears`; usage forced to arrears | `contracts.rs:600-634` | CONFIRMED in code; pricing docs say `in_advance \| in_arrears` (DOC/CODE DRIFT) |
| P-12 | Migrated-origin snapshot | always 404 | `migrated_origin_snapshots.rs:179` | MISSING — not used |
| P-13 | Aggregation scope for usage tiers (`subscription_line` vs `resource`) | — | none | **ABSENT** (R-23) |
| P-14 | Meter → usage type binding (`METER_USAGE_TYPE_UNBOUND`, `METER_DIMENSION_UNDECLARED`) | — | `pricing/docs/design/02-plan-definition.md:236`; codes "deliberately absent" (`plan_rules.rs:30-49`) | ASSUMED (R-09) |
| G-4 | `CatalogVersionRegistryV1 { request_version, committed_version }` — registry is the sole incrementer; `CatalogVersionPublished` is the registry's event (D-66(1)) | — | `pricing-sdk/src/catalog_version_registry.rs:52-183`; only `LocalDevCatalogVersionRegistryV1` (`infra/local_dev_registry.rs`); products gear docs only (`products/docs/PRD.md:326,1006-1012`) | PARTIAL (dev stand-in) |
| G-5 | `usageTypeRef` on the registry metering-unit declaration | — | `products/docs/PRD.md:448-452`; `CatalogSku` has no `usage_type_ref` (`pricing-sdk/src/product_catalog.rs`) | ASSUMED |

## D. Subscriptions

`gears/bss/subscriptions` — **documentation only; no crate exists** (`Cargo.toml:155-164`). Its
decision log ends at SUB-D-27; its three ADRs are `status: proposed`.

| # | Contract | Required fields | Version semantics | Evidence | Status |
|---|---|---|---|---|---|
| S-1 | Subscription version (timeline) | `subscription_id, version`, tenant axes (payer/seller/resource/ordering), plan links, add-ons, phases, `QuantityInterval`s, `(currency, region)`, `activatedAt`, pinned price ids/cohort, suspensions, `(changeEffectiveAt, changeMode)` | monotonic | `design/01-foundation-lifecycle.md:187`, `03*`, `09-consumer-contracts.md:217` | ASSUMED; SUB-R1 field list unreconciled |
| S-2 | Recurring period fact `BillableItemCreated(kind = recurring)` per `(subscriptionId, billing period, lineKey)`, money-free, traceability tuple `{subscriptionId, skuId, planId, priceId}` + `pricingSnapshotRef`, suspended intervals + `pause_recurring|continue`, period-start `payerTenantId`, `collectionPaused`; cut daily by 00:00 | as listed | unique key; no fact versions | `design/08-events-billing.md:118,192,215,249-257`; SUB-D-07/19/20/21/27 | ASSUMED; emission timing ambiguous (opening vs end, AC 5 vs S04:255) |
| S-3 | Resource → subscription attribution | — | — | none; S-PRD:360 assumes usage arrives "already keyed by `subscriptionId`" | **ABSENT** — CONFLICTING with UC U-7 (R-25) |
| S-4 | `lineKey` rule `plan#n`, `addon:{addOnId}#n` | — | immutable | SUB-D-21 | ASSUMED (T-D-34) |
| S-5 | Ordering key `(orderingTenantId, subscriptionId)` | — | — | SUB-D-06 | ASSUMED |
| S-6 | One-time fact `BillableItemCreated(kind = one_time)` per `(subscriptionId, priceId)`, valued by Billing "from the ref" | — | once per lifetime | SUB-D-24; S08:195,257 | ASSUMED; CONFLICTING with Atlas R2 (R-19) |
| S-7 | Recovery reads (`facts`, `period_facts`, `segments_since`, `scope_for_window`, `bindings`, `groups_due`) | — | — | none; outbox replay + 02:00 charge-coverage reconciliation only (S-PRD:1483) | **ABSENT** (Atlas C04 PROPOSED) |
| S-8 | Billing group / sealed composition | — | — | none ("billing group" occurs nowhere in the repository) | **ABSENT** (Atlas C04 PROPOSED) |

## E. Billing orchestration and Ledger

There is **no Billing gear** (`docs/GEARS.md:874` Invoicing is a placeholder entry with no links). `bss-ledger` exists.

| # | Contract | Semantics | Evidence | Status |
|---|---|---|---|---|
| L-1 | Billing period state | — | `periodState`, `BillingPeriodStateChanged`, invoice freeze: no producer anywhere | **ABSENT** — under T-D-45 Rating does not need it |
| L-2 | Invoice post `POST /bss-ledger/v1/journal-entries` (`PostInvoiceRequestDto`: `tenant_id, invoice_id, payer_tenant_id, resource_tenant_id?, effective_at, due_date?, period_id, items, tax, correlation_id`; `InvoiceItemDto.amount_minor_ex_tax: i64`, `gl_code`, `price_id`, `pricing_snapshot_ref`, `invoice_item_ref`, `recognition?`) | idempotent on `(tenant, INVOICE_POST, invoice_id)` | `ledger/src/api/rest/journal_entries.rs:123-140`, `dto.rs:224-243, 417-435` | CONFIRMED — Billing maps `snapshot_id` → `pricing_snapshot_ref`, rounds to integer minor |
| L-3 | `LedgerClientV1` (21 methods: `post_balanced_entry`, `close_period`, `trigger_recognition_run`, …) | no invoice/credit/debit method | `ledger-sdk/src/api.rs:22-445` | PARTIAL — Billing's concern (Atlas N11) |
| L-4 | `POST /credit-notes`, `POST /debit-notes`, `/manual-adjustments` | idempotent on the note id | `api/rest/adjustments.rs:126,173,215` | CONFIRMED (REST) — Billing posts them from aggregate-target differences (Atlas C08) |
| L-5 | Ledger fiscal period (`ledger_fiscal_period OPEN|CLOSED`; `ledger_period_close OPEN|CLOSING|CLOSED|REOPENED`) | posts into a closed period rejected `PeriodClosed` | `m20260628_000033`, `domain/error.rs:111` | CONFIRMED but a different concept; not used by Rating |
| L-6 | Rounding | ledger `round_half_even` for its own splits; invoice items arrive in integer minor units | `ledger/src/domain/money_math.rs:24`; ledger PRD `:661` | CONFIRMED — invoice rounding is the invoice builder's (Billing) |

## F. FX

| # | Contract | Evidence | Status |
|---|---|---|---|
| F-1 | Versioned FX table / locked-rate id readable by Rating | none in any gear | **MISSING** |
| F-2 | `RateProviderV1::fetch_latest(ctx, pairs, request_id) -> Vec<ProviderRate {base, quote, rate_micro, as_of, provider}>` — latest only; ledger discovers the provider through a types-registry plugin instance + ClientHub scoped client | `ledger-sdk/src/rate_provider.rs:64-90`, `rate_provider_plugin.rs` | CONFIRMED but unusable by Rating |
| F-3 | Ledger `ledger_fx_rate_snapshot` minted inside a post | `infra/fx/rate_locker.rs`, `api/rest/fx.rs:110` | CONFIRMED, not pinnable before a post |

Consequence: native currency only (R-07).

## G. Contracts, Promotions, Orders, Tenant identity

| # | Contract | Evidence | Status |
|---|---|---|---|
| G-1 | Contract price-override windows, applied by Rating as the step-5 overlay | `gears/bss/contracts/docs/PRD.md:296-307` (first draft, §7–17 TBD) | ASSUMED |
| G-2 | Commitment pools, `poolType`, `balanceVersion`, draw order, rollover, `CommitmentBalanceEffect` | contracts PRD `:310-322` defines commitment type/threshold only; balances "owned downstream" | **ABSENT** (R-11) |
| G-3 | Frozen coupon snapshots | no Promotions gear or PRD (`pricing/docs/DESIGN.md:465`); `discountRef` unvalidated (D-255) | **ABSENT** |
| G-6 | Order-time "resolved order total" / TCV (p1), preview (p2), delta totals for change orders | `orders-lifecycle/docs/PRD.md:114,214,223,767-791`; `orders-changes/docs/PRD.md:103,305,403` | ASSUMED consumer of `OrderEvaluationV1` (R-24) |
| G-7 | Tenant directory | `AccountManagementClient::get_tenant` (`account-management-sdk/src/client.rs:127`), `TenantResolverClient` (`tenant-resolver-sdk/src/api.rs:44-127`) | CONFIRMED; commercial axes come from Subscriptions (`fr-tenant-axes`); AM tenant types (`provider/reseller/customer`) differ from ledger seller defaults (`vz.ams.tenants.partner/platform`, `ledger/src/config.rs:42-43`) |
| G-8 | OrgTier, customer-group membership | pricing taxonomies only (`m20260821_000012/13/17`) | CONFIRMED in pricing; no runtime membership read for Rating (`unresolvable_scope`) |

## H. Shared fixtures

| # | Contract | Evidence | Status |
|---|---|---|---|
| H-1 | `bss_fixtures_conformance::CorpusEvaluator { evaluate, supported_families }` | `gears/bss/fixtures/bss-fixtures-conformance/src/traits.rs:83-101` | CONFIRMED |
| H-2 | Band amount scale: corpus `Band.unit_amount_minor: i64` vs catalog `unit_price_nano` | corpus vs `price_row.rs` | CONFLICTING (factor 10⁹) — adapter converts |
| H-3 | K5 anchor fixture, `lineKey` fixture | none | MISSING |
| H-4 | Atlas acceptance scenarios F01–F35 | Atlas page (not vendored) | PROPOSED — Rating-applicable vectors copied into slices 03/08/14/15 |

## I. Adopted pricing semantics

| Seam | Adopted rule | Status in pricing code |
|---|---|---|
| K1–K5 | Select on the full canonical key; `phase` is a `phase_id`; cohort by the pinned price id; class order `existing_grandfathered > new_subscriptions_only > all_subscriptions` | Key has 10 axes (P-4) |
| O1–O3 | Stack all scope-matching overlays; ascending `precedence`, cross-class ties by `customerGroup > partner > orgTier > brand > region > global`, then `priceOverlayId` | Overlay documents projected (P-2 needs a read API) |
| W1–W2 | Window state is read from the pinned document; final windows keep their pin-of-record | Windows projected per scope key |
| M1–M5, M11 | `modelKind` set, per_unit via `quantitySource`, package `ceil`, Volume Variant B not authorable, open-top bands, usage-only banded kinds | CONFIRMED (P-9) |
| M6 | `dimensionKey` declared by pricing, values from usage metadata | `dimension_key: String` on the row |
| M7 | Counter key = the child's aggregation key (T-D-48) | Rating-internal |
| M10 / T-D-17 | `aggregationFunction ∈ {sum, peak, time_weighted}` | CONFLICTING with UC (U-4, R-06) |
| M12 | `tierQualificationWindow` | Refused at publish (P-10) |
| P1–P3, F1 | `prorationBasis` incl. `none`; `billingAnchorPolicy` + D-20 clamp; `usageCounterOnPlanChange` (absent = reset); usage rows phase-invariant | CONFIRMED (P-9) |
| B1 | `sum_of_parts` bundle: Rating sums component recurring amounts; rev-share passes through | `PriceBasis {SumOfParts, OwnPrice}` |
| G1 | One publish-governance engine (pricing Slice 5) | P-5 ASSUMED |
| SB1 | The subscriptions period fact is the commercial WHEN (T-D-33) | S-2 ASSUMED |

## J. Required dependency changes

| # | Affected gear | Missing capability | Required change | Class | Rating workaround |
|---|---|---|---|---|---|
| J-1 | usage-collector | the documented V1 in code | implement upstream `main` DESIGN §3.3 (feed, intervals, `entry_type`, backfill, types-registry declarations) — the collector's own plan; nothing to redesign | **Mandatory** (R-01) | none for production; fixture adapter in tests |
| J-2 | pricing | document read at a pin | implement + register `PricingCatalogClientV1`; add `plan_document(ctx, plan_id, catalog_version)` and `overlay_documents(ctx, catalog_version)` over `read_model_repo::delta_at` | **Mandatory** (R-02) | none |
| J-3 | products registry | committed catalog versions | implement `CatalogVersionRegistryV1` and `CatalogVersionPublished` | **Mandatory** for production pins | local-dev mode |
| J-4 | subscriptions | facts, attribution, scope proofs, recovery reads | Atlas C04 surface (R-03, R-20, R-25); reconcile SUB-D-07/19/24, AC 27, S-PRD:360 | **Mandatory** | MIGRATION derived usage parent (R-20) |
| J-5 | Billing (new gear) | delivery consumer, per-fact heads, aggregate rounding, posting obligations | Atlas C08 against `BillableItemDeliveryV1` / `RatingRunReadV1`; candidate for an `UPSTREAM_REQS.md` seed (repository convention, `docs/spec-templates/gears-sdlc/UPSTREAM_REQS/template.md`) | **Mandatory** for invoicing | none |
| J-6 | usage-collector + pricing + products | level meters | decide R-06 and amend the losing document | **Mandatory** before level products | non-sum fails closed |
| J-7 | pricing | `meter` free text | validate `meter` = GTS id at publish | Recommended (R-09) | `meter_unpriced` |
| J-8 | ledger / finance | pinnable FX | rate snapshots readable by id | Optional until cross-currency (R-07) | native currency |
| J-9 | fixtures | band scale | align or document conversion | Recommended | adapter converts |
| J-10 | pricing | validator hook | registration trait in Slice 5 | Recommended (R-12) | runtime fail-closed |
| J-11 | pricing + usage-collector | dimension encoding | adopt the R-16 encoding | Recommended | empty key only |
| J-12 | pricing | aggregation scope | per-row `aggregationScope ∈ {subscription_line, resource}` | Recommended (R-23) | `subscription_line` only |
| J-13 | unassigned (overlay T6: a metering-adapter PRD per provider adapter, or an IRM PRD §5.2 amendment); IRM for inventory | coverage declarations, historical inventory | `MeterCoverageDeclared`, `MeterCoverageReadV1` (emitter); `ResourceHistoryV1` (IRM) | **Mandatory** for evidence-gated finalization (R-21) | `delay_only` profile only with sign-off |
| J-14 | types-registry / products | typed usage type declaration read | `get_usage_type_declaration(gts_id) -> {aggregation_fold, canonical_unit, accrual_method}` (Atlas Plus X7) | Recommended | generic schema read (slice 12 §4.7) |

## K. Documentation / code mismatches found

| Rating documentation said | Repository actually contains | Resolution |
|---|---|---|
| Usage arrives on a durable stream (former UC1) | UC PRD specifies a pull feed; code has none | pull feed (R-01) |
| Collector corrections are signed compensations + deactivation | UC PRD: invalidation entries only; code still has both (U-3) | invalidation entries only (Flow B) |
| Pricing read model supplies prices at `t` | only `pin_frontier`, unimplemented | J-2 |
| `pricingSnapshotRef` has four writers | no writer; pricing type test-only | Rating-owned snapshot |
| 8-axis scope key | 10 axes | 10-axis key |
| Rating consumes `PriceWindow*` / `CatalogVersionPublished` events | outbox never drained; `CatalogVersionPublished` is the registry's (D-66(1)) | no event consumption |
| `periodState` supplied by Billing; later a Rating period fence | no Billing gear; Atlas D09 makes period state Billing's and not a Rating input | T-D-45 |
| Finance FX tables with `fxTableVersion` | no such contract | native currency |
| Promotions, Contracts pools, `CommitmentBalanceEffect` | absent / first-draft PRD | fail closed |
| `tierAggregationWindow` four values | five incl. `per_hour` | five values |
| Rating folds gauge samples (T-D-17) | UC PRD forbids it | R-06 |
| K-10: Atlas: Pricing is a PriceBook with `resolve`/`price`/`hold`, `PricingReadV1`, acceptance receipts; "Rating rates with no cohort, no catalog version" | Plan/price rows/scope key/windows/overlays/cohorts/`CatalogVersion`; no `resolve`, no `PricingReadV1`; decisions end at D-367 | CONTRACT CONFLICT (R-17) |
| K-11: Atlas cites SUB-D-28/29, pricing D-409/D-410/D-415/D-419…D-425 | not present in this repository | not adopted (R-17) |
| K-12: Atlas cites Rating "T-D-38 floor", "T-D-37 ends_on" | here T-D-37 is the pin rule, T-D-38 the window-unit rule | id collision noted (DECISIONS preamble) |
| K-13: Atlas: recurring proration is actual elapsed seconds | pricing `prorationBasis` enum per price | pricing SoR kept (R-17) |
| K-14: Atlas: hourly window metadata is a new proposed Pricing contract (`UsageRatingPolicy`) | `tierAggregationWindow = per_hour` exists in code | mapped onto the existing field (T-D-48); only aggregation scope is missing (P-13) |
| K-15: Subscriptions: usage arrives keyed by `subscriptionId` (S-PRD:360) | UC carries no commercial identity (U-7) | R-25 |
| K-16: SEAMS (2026-09-25) A-3 "event-broker storage is memory-only" | SDK implemented; broker gear a scaffold | A-3 corrected |
| K-17: SEAMS (2026-10-01) §B read the usage-collector at this branch's base (no feed in DESIGN, "watermarks non-goal", 11 ADRs) and adopted the Atlas `UsageFeedV1` | upstream `main` documents a feed of its own (`read_usage_feed`, ADR-0011) and 15 new ADRs | §B rebuilt; Atlas C05 feed not adopted (R-01) |
| K-18: the Atlas Plus Rating card grades "Rating documents" as having no read surface, a `periodState` gate, an Adjustment lane, no hourly scheduler | it read the Rating documents on `main` (before this revision) | addressed here: RatingRunReadV1/ControlV1, T-D-45, T-D-44, T-D-48, T-D-52; open: R-19, R-22, R-24 |

## L. Seam Atlas v2 baseline — status in this repository

The Atlas (rev 8, baseline 1.1, 2026-09-30) defines contracts C00–C10. It states itself that every
red contract is "specified here, no code" and that nothing has passed an end-to-end fixture. Status
below is for **this** repository.

| Contract | Owner → consumer | Atlas content relevant to Rating | Status here | Overlay grade (2 Oct) | Rating adoption |
|---|---|---|---|---|---|
| C00 shared rules | all | `SecurityContext`, `CommonError`, idempotency scope, `EventEnvelope`, at-least-once + inbox, `ExactAmount`, `TenantAxes` | PROPOSED; toolkit conventions CONFIRMED | Atlas only | adopted (T-D-46, T-D-51) |
| C01 accepted pricing | Pricing → Orders, Subscriptions, Rating | `PricingReadV1::{resolve, price, hold, acceptance}`, `SellabilityV1`, `AcceptanceReceipt`, `UsageRatingPolicy` | **CONTRACT CONFLICT** — model absent (K-10) | Contested | not adopted (R-17, R-18); window semantics mapped onto `tierAggregationWindow` |
| C02 Orders lifecycle | Orders → Portal, Workflow | — | PROPOSED | Contested | not Rating's |
| C03 Subscriptions fulfilment | Subscriptions → Workflow, Orders | activation validates billing terms and usage policy | PROPOSED | Contested | consumed indirectly |
| C04 billable facts, attribution, sealed composition | Subscriptions → Rating, Billing | `BillableFactPublished`, `BillableSetSealed`, `AttributionSegmentChanged`, `UsageScopeSealed`, `SubscriptionBillingReadV1` | PROPOSED; S-2 documented, S-3/S-7/S-8 absent; conflicts R-20, R-25 | Atlas only | adopted as TARGET (T-D-44, T-D-52) |
| C05 usage feed, coverage, inventory | Collector, IRM → Rating, Subscriptions | `UsageFeedV1`, V2 interval writes, invalidations, `MeterCoverageReadV1`, `ResourceHistoryV1`, finalization rule, boundary splits | feed: the collector's own design supersedes it (U-1…U-3, overlay: withdraw `UsageFeedV1`); coverage/inventory ABSENT (U-10/U-11) | Uncovered | collector feed adopted (R-01); finalization rule and boundary splits adopted (T-D-47, T-D-48); coverage owner open (R-21, T6) |
| C06 pre-purchase evaluation | Rating → Orders | `OrderEvaluationV1::evaluate` | PROPOSED | Contested | adopted (T-D-49) |
| C07 deterministic run + replacement delivery | Rating → Billing | `RatingRunReadV1`, `RatingRunControlV1`, `WindowEvaluationKey`, `WindowResult`, `WindowManifest`, `BillableItemDeliveryV1`, input-generation CAS | Rating-owned | One-sided | adopted (T-D-44, T-D-45, T-D-53); exact amounts in minor units (T-D-46) |
| C08 Billing acceptance + posting | Billing, Ledger | per-fact heads, aggregate rounding, ordered posting obligations | PROPOSED; no Billing gear | One-sided | Rating's dependency (J-5) |
| C09 event names + recovery | per producer | event vocabulary, recovery reads, retention ≥ largest delay + outage | PROPOSED; no delivery (A-3) | Contested | adopted as inbox sources (T-D-51) |
| C10 hourly windows + monthly composition | Pricing, Subscriptions, Rating, Billing | Rating-owned scheduler, `FinalizationPolicy`, `RatingWindow`, `AggregationKey`, `WindowSchedule`, hourly math | PROPOSED; window enum exists (P-9), scope absent (P-13) | Atlas only | adopted (T-D-44, T-D-47, T-D-48); floor/included quantity per R-22/P-10 |

## M. Artifact impact analysis

Each entry: what the Atlas says, what the repository says, what the 2026-09-25 Rating worktree said,
the impact, and the status.

### M-1 Commercial facts drive work; Rating owns the window schedule (Atlas D13, D15, C04, C10)

- **Atlas**: Subscriptions publishes one parent period fact at period opening; Rating derives child
  windows, persists `next_due`, catches up after downtime; no hourly event.
- **Repository**: Subscriptions documents a money-free `BillableItemCreated` per `(subscriptionId,
  period, lineKey)`, cut daily, recurring and one-time kinds only; SUB-D-07 rejects Rating
  self-triggering periods; AC 27 allows one priced line per key.
- **Worktree**: period facts create `period_line` units; usage units are created by counter changes
  with no schedule and no finalization.
- **Impact**: new `rating_fact`, `rating_window_schedule`, `rating_child_window`; usage needs a parent.
- **Rating changes**: DESIGN §3.1/§3.6, slices 13–15. **External**: Subscriptions adds the usage
  fact kind and versions (J-4). **Status**: proposed; CONTRACT CONFLICT with SUB-D-07/19 and AC 27
  for usage facts (R-20); MIGRATION derived usage parent.

### M-2 Absolute replacement results; Billing owns period state and rounding (Atlas D09, D10, C07, C08)

- **Atlas**: complete exact parent result per revision; no delta lane; `BillingPeriodStateChanged`
  is a hint; Billing rounds per `invoice_line_key` aggregate once.
- **Repository**: no Billing gear; no period state anywhere; ledger invoice items are integer minor.
- **Worktree**: signed delta charge entries, Rating period fence, nano-minor half-even.
- **Impact**: `rating_period`, `rating_charge_entry`, `rating_charge_feed`, `close_period` removed;
  `rating_fact_result` + `rating_delivery` added.
- **Status**: proposed (T-D-45, T-D-46); supersedes T-D-40/T-D-42; PRD posted-period wording
  realized by Billing (PRD §9.2 note). Billing confirmation outstanding.

### M-3 Completeness evidence (Atlas D07, C05, P7)

- **Atlas**: sealed scope + IRM inventory barrier + final coverage + snapshot digest; time never
  proves completeness.
- **Repository**: none of the sources exist; the collector raises no stall signal.
- **Worktree**: no finalization (period close fenced invoice content).
- **Impact**: `EvidenceGate`, `rating_usage_scope`, `rating_coverage_declaration`, pending reasons.
- **Status**: proposed (T-D-47); OPEN DEPENDENCY J-13; interim R-21.

### M-4 Hourly tiers and window scope (Atlas D14, C10, F23–F29)

- **Atlas**: tiers reset each UTC hour; volume/graduated per window; partial windows keep
  thresholds; aggregation scope `subscription_line | resource`.
- **Repository**: `tierAggregationWindow = per_hour` exists; no aggregation-scope field.
- **Worktree**: `per_hour` window supported; counter key per subscription/meter/dimension.
- **Status**: adopted (T-D-48) with `subscription_line` only (R-23).

### M-5 Usage feed and interval records (Atlas C05; revised 2026-10-02)

- **Atlas**: `UsageFeedV1::{open_snapshot, read}`, V2 interval writes with `source_key` /
  `source_revision`, invalidations without mutating snapshots, `BoundarySplitRequired`.
- **Repository**: upstream `main` documents the collector's own target V1 — `read_usage_feed` with an
  opaque continuing cursor and `until`, no snapshot token, interval entries, invalidation entries, a
  backfill route (U-1…U-13); code has none of it yet (U-1, U-8); the code's `deactivate` mutates
  status and is being removed.
- **Worktree (2026-09-25)**: a Rating-invented `UsageCollectorClientV1::feed(…)`; attribution by
  `window_start`. **Revision 2026-10-01**: the Atlas snapshot shape.
- **Atlas Plus overlay (2026-10-02)**: grades C05 *Uncovered* and asks to "withdraw UsageFeedV1 and
  regenerate C05 from the Collector's design"; the 22a fixture uses a method the collector is
  deleting.
- **Status**: Rating adopts the collector's documented feed (R-01, slice 12) and keeps the Atlas
  boundary rule (T-D-48); the snapshot-continuation question is answered by the collector (one
  cursor, no snapshot).

### M-6 Attribution (Atlas C04, P2, P3, P11)

- **Atlas**: Subscriptions publishes gap-free attribution segments per `resource_tenant_id`;
  Rating projects them; `segments_since` recovery; orphan policy upstream.
- **Repository**: absent; Subscriptions assumes usage is already keyed by subscription.
- **Worktree**: `SubscriptionsClientV1::resolve_resource` per lookup (proposed).
- **Status**: proposed (T-D-52); CONTRACT CONFLICT on the owner (R-25).

### M-7 Pricing model and price binding (Atlas C01, D02, D06)

- **Atlas**: Pricing PriceBook with acceptance receipts; Subscriptions freezes term slices; Rating
  never re-resolves.
- **Repository**: scope-key catalog with cohorts and catalog versions (K-10).
- **Worktree**: ADR-0001 selection at the current frontier.
- **Status**: CONTRACT CONFLICT (R-17); selection kept, cross-checked against the fact's `priceId`
  (R-18); pin rule refined to pin-of-record (T-D-37).

### M-8 One-time charges (Atlas R2)

- **Atlas**: Rating rates one-time occurrences.
- **Repository**: SUB-D-24 Billing values them from the ref; the ref is Rating's (T-D-39).
- **Worktree**: T-D-18 not rated.
- **Status**: conflicting; recommendation adopt R2 (R-19).

### M-9 Pre-purchase evaluation (Atlas D04, C06)

- **Atlas**: P1 `OrderEvaluationV1`, usage excluded, TCV rules.
- **Repository**: Orders PRD p1 resolved totals; Rating PRD p2.
- **Worktree**: `RatingClientV1::preview` p2.
- **Status**: adopted (T-D-49); priority R-24.

### M-10 Level meters (Atlas R1)

- **Atlas** and **UC PRD**: emitter integrates; peaks are distinct `SUM` meters.
- **Worktree**: T-D-17 suspended, recommendation (b).
- **Status**: aligned; product sign-off (R-06).

### M-11 Event transport and recovery (Atlas C00, C09, P8, P11)

- **Atlas**: at-least-once events + durable inbox + business-key recovery reads.
- **Repository**: broker SDK exists, broker gear a scaffold, no delivery.
- **Worktree**: pull-only, no events.
- **Status**: refined (T-D-51): inbox-first, pull authoritative, events additive.

### M-12 Floors and included quantities (Atlas C07, C10, K3)

- **Atlas**: monthly minimum fee once per price/subscription/period by Rating; hourly floors and
  included quantities rejected.
- **Repository**: `PeriodFloorCap` per plan-market (D-319), executed by Billing per Rating PRD;
  `includedAllowance` carry refused at publish.
- **Status**: hourly rejection adopted; floor executor CONTRACT CONFLICT (R-22).

### M-13 Seam Atlas v2 Plus overlay (2026-10-02)

The overlay keeps rev 8 and baseline 1.1 unchanged (the embedded contract pack is byte-identical) and
adds: a second grade per seam ("written down": whether both owners' documents carry the same
contract — connectable, contested, one-sided, Atlas only, uncovered), twelve decision tickets
(T1–T12), 24 unlisted hops (X1–X24) and an errata list. Its baselines: Pricing and Products at the
fork tip `diffora/bss/products @ 7d3544156`; Rating and Subscriptions on documents that predate that
fork — for Rating, the documents on `main` before this revision (K-18).

| Overlay item | What it says for Rating | Here |
|---|---|---|
| Rating card / C07 | no read surface, `periodState` gate, Adjustment lane, no hourly scheduler, D04/D06/D07/D09/D10 unrecorded; FinalizationPolicy has no owner | read surface §3.3; T-D-44, T-D-45, T-D-46, T-D-47, T-D-49 recorded; D06 recorded as R-18; finalization policy owned by Rating (T-D-54) |
| C05, rows 15a/15b/16/22a/22b | adopt the collector's own feed; coverage has no owner | R-01, slice 12; R-21, J-13 |
| C06, T3 | evaluation DTO contested with Orders D-167 (accepted matrix, integer minor units) | R-24 |
| C10, X20 | Pricing has no window, scope or reset concept at the fork tip | this repository's pricing has `tierAggregationWindow = per_hour` (K-14); only aggregation scope is missing (R-23) |
| Transfer row for Rating | "drop CatalogVersion" | depends on Pricing adopting the fork's model here (R-17); not done |
| Transfer row for Rating | "adopt T-D-35…38 on the worktree" | the fork's rows of those numbers (DECISIONS preamble) |
| T8, row 22d | Ledger design/01:364 names a Rating-side adapter as the credit-note poster | Billing decides and posts (T-D-45); Ledger documents need rewording (ledger owner) |
| X7 | no typed read of fold / unit / accrual method | J-14, slice 12 §4.7 |
| X9, X11 | usage fact at period opening; `scope_for_window`, `period_facts` are Atlas-only | R-20, J-4 |
| X10 | `ResourceHistoryV1` has no IRM counterpart | U-11, J-13 |
| X12, T7 | finalization delay owner | T-D-54 |
| X18 | no shared inbox or C00 types crate (`cf-gears-bss-inbox`, `cf-gears-bss-common` proposed) | Rating's inbox is gear-local (T-D-51); adopt the shared crate if it lands |
| Errata: three event vocabularies; two `BillingPeriodStateChanged` enums | — | Rating uses the C09 names for TARGET and Subscriptions' documented names for CURRENT; the hint enum is `open \| frozen \| posted` (C09) |
