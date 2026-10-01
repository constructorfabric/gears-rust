<!-- CONFLUENCE_TITLE: [BSS]: Rating — Decision Register -->
<!-- Related: ./DESIGN.md, ./PRD.md, ./SEAMS.md | Owners: BSS Rating team -->

# Rating — Decision Register

<!-- toc -->

- [Status vocabulary](#status-vocabulary)
- [Open decisions](#open-decisions)
  - [R-01 — Usage feed](#r-01--usage-feed)
  - [R-02 — Pricing read contract](#r-02--pricing-read-contract)
  - [R-03 — Subscriptions contract](#r-03--subscriptions-contract)
  - [R-04 — Period state owner (revised 2026-10-01)](#r-04--period-state-owner-revised-2026-10-01)
  - [R-06 — Level meters](#r-06--level-meters)
  - [R-07 — FX](#r-07--fx)
  - [R-11 — Contracts / Promotions inputs](#r-11--contracts--promotions-inputs)
  - [R-17 — Atlas baseline vs repository](#r-17--atlas-baseline-vs-repository)
  - [R-21 — Finalization evidence](#r-21--finalization-evidence)
- [Decided](#decided)
- [Carried product opens](#carried-product-opens)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-decisions`

Two registers: **open decisions** (`R-*`) that block or constrain implementation, ranked by impact,
and **decided** rows (`T-D-*`) that the design implements. Evidence references point at
[`SEAMS.md`](./SEAMS.md) rows. Rows `T-D-01…T-D-34` were taken 2026-07-10…2026-08-01 (partly under
the gear's former name "Tariffs"). `T-D-35…T-D-42` were introduced by the 2026-09-25 revision that
aligned the design with the repository. `T-D-43…T-D-53` were introduced by the 2026-10-01 revision
that reconciled the design with the **Seam Atlas v2 contract baseline 1.1** (2026-09-30, "the
Atlas"; SEAMS §L). Proposed rows await owner confirmation.

**Decision-id collision.** The Atlas cites Rating decisions by number (`T-D-17`, `T-D-18`,
`T-D-33`, `T-D-37`, `T-D-38`) from a register that diverged from this one. Its "T-D-38 — Rating owns
the minimum-fee floor" and "T-D-37 — consumers adapt to PriceBook / `ends_on` ownership" are **not**
the rows of the same number below (here `T-D-37` is the pin rule and `T-D-38` the window-unit rule).
Where this register adopts an Atlas position it records it under its own id and names the Atlas
decision (`D01…D15`, `R1…R3`, `P1…P12`, `K1…K11`).

The **Seam Atlas v2 Plus overlay** (2026-10-02) confirms the split: the diverged register is the
fork's (`diffora`), whose Rating documents add their own `T-D-35…T-D-38` ("T-D-38 amended for
D-467"); its transfer row "adopt T-D-35…38 on the worktree" means those fork rows, not the rows of the
same number here. Known fork meanings and their counterparts here: fork `T-D-37` (price by pinned
id) ↔ R-18; fork `T-D-38` (minimum-fee floor in Rating) ↔ R-22. The fork's `T-D-35` and `T-D-36` are
not quoted by the overlay. The overlay's twelve decision tickets map onto this register as follows:

| Overlay ticket | Question | Here |
|---|---|---|
| T1 | Does an accepted order keep its price until activation (hold)? | R-18 (Pricing-owned) |
| T3 | One evaluation request DTO and money type | R-24 |
| T6 | Who owns the usage emitter and coverage declarations? | R-21, SEAMS J-13 |
| T7 | Who owns the finalization delay? | decided here: T-D-54 |
| T8 | Period state, correction lane, minimum-fee floor, one-time valuation, credit-note decider | R-04, R-05, R-19, R-22 |
| T2, T4, T5, T9–T12 | Orders, Workflow, entitlement, approval, Payments, change orders, deprovision, intent outcome | not Rating's |

Impact: **BLOCKER** — implementation of the affected path cannot safely proceed; **HIGH** — a
money-affecting capability is unavailable or ambiguous; **MEDIUM** — correctness risk with a safe
interim; **LOW** — contained.

## Status vocabulary

| Status | Meaning |
|---|---|
| adopted / accepted | Decided and implemented by this design. |
| proposed | Taken by Rating; awaits the named owner's confirmation. |
| dormant | Decided, but inert until a missing upstream source exists. |
| suspended | Decided earlier, now conflicts with an upstream SoR; not implemented. |
| superseded | Replaced by the named row. |

## Open decisions

| ID | Area | Missing / contradictory decision | Evidence | Impact | Recommendation |
|---|---|---|---|---|---|
| R-01 | Usage intake | The collector's own target V1 designs the feed (`read_usage_feed`, interval entries, invalidation entries — upstream `main` DESIGN §3.3, ADR-0011); its code (here and upstream) still has no feed. The Atlas `UsageFeedV1` is withdrawn by the Atlas Plus overlay. This branch's base predates the collector rework | SEAMS U-1…U-14, §L C05 | **BLOCKER** (usage rating) | usage-collector implements its documented V1 (J-1); Rating codes against it; rebase this branch onto upstream `main` |
| R-02 | Pricing reads | No implemented way to pin a catalog version or read plan/overlay documents at a version | SEAMS P-1, P-2, G-4 | **BLOCKER** (all rating) | pricing implements and registers `PricingCatalogClientV1` with document reads (J-2, J-3) |
| R-03 | Subscription inputs | Subscriptions has no code; facts, attribution, scope proofs and recovery reads are prose (facts) or absent (attribution, scope, recovery) | SEAMS S-1…S-8, §L C04 | **BLOCKER** (all rating) | subscriptions builds the Atlas C04 surface (J-4) |
| R-04 | Period state (overlay T8) | **Revised 2026-10-01.** Who decides invoice content vs adjustment | SEAMS L-1, §L C07 | **BLOCKER** (invoicing) | Billing owns period state and correction routing (Atlas D09); Rating keeps no fence (T-D-45) |
| R-05 | Output consumer | No gear consumes rated output | SEAMS L-2…L-4, §L C08 | **HIGH** | Billing consumes `BillableItemDeliveryV1` and recovers through `RatingRunReadV1` (T-D-45, J-5) |
| R-06 | Level meters | UC PRD requires emitter pre-integration; Rating T-D-17 / pricing D-44 fold gauge samples in Rating | SEAMS U-4 | **HIGH** (cloudlet peak, storage GB-month) | option (b) — same as Atlas R1 |
| R-07 | FX | No versioned FX source Rating can pin | SEAMS F-1…F-3 | **HIGH** (cross-currency billing) | native-currency launch (Atlas agrees: FX out of baseline, D12) |
| R-08 | Snapshot reference | PRD `pricingSnapshotRef` has no implementation; pricing's type is test-only; ledger carries a free string | SEAMS P-6, L-2 | **MEDIUM** | Rating-owned content-addressed snapshot (T-D-39); the Atlas "no snapshot ref" position depends on the absent PriceBook model (R-17) |
| R-09 | Meter binding | pricing `meter` is free text; the registry binding (`usageTypeRef`) is documented, not built | SEAMS G-5 | **MEDIUM** | pricing `meter` = UC GTS type id, validated at publish (J-7) |
| R-10 | Scope key | Rating docs adopted 8 axes; pricing code has 10 | SEAMS P-4 | closed | 10-axis key (ADR-0001 amended) |
| R-11 | Contracts / Promotions | No gear supplies contract overlays, commitment pools, negotiated reserved rates, or coupons | SEAMS G-1…G-3 | **HIGH** (feature scope) | launch without them; fail closed when referenced |
| R-12 | Validator hook | Rating's publish-time validators have no registration point in pricing | SEAMS P-5 | **MEDIUM** | pricing exposes a validator trait (J-10) |
| R-13 | Latency NFR | **Revised.** End-to-end time from usage to a final result is bounded by the finalization delay (minutes for hourly, ≥ 48 h for monthly profiles) plus evidence arrival, not by Rating | PRD §7.1; DESIGN §4.9 | **MEDIUM** | restate as Rating-internal latency (input change → provisional result committed) |
| R-14 | Precision | PRD left emitted precision "to be set with Billing" | PRD §4.1 | closed | exact rationals (T-D-46), superseding the nano-minor interim |
| R-15 | Interval attribution | A record may cross a window, price, or payer boundary | UC PRD `…-fr-usage-windows`; Atlas C05 | closed | boundary-split rule (T-D-48) supersedes "attribute by `window_start`" |
| R-16 | Dimension key encoding | pricing `DimensionKey` is an opaque trimmed string (`scope_key.rs:576`); nothing defines how usage metadata maps onto it | SEAMS P-4, U-7 | **MEDIUM** (dimensional pricing) | encode as `name=value` pairs of the GTS type's declared metadata, sorted by name, joined by `,` (no `\|`); pricing validates the same encoding at publish. Interim: only the empty key is rateable; a row with a non-empty `dimensionKey` fails closed `unsupported_primitive` |
| R-17 | Atlas baseline vs repository | The Atlas was audited against `diffora/gears-rust @ 01f670fa4e5c`, not this repository. Its Pricing model (PriceBook, `resolve` walk D-419…D-425, `PricingReadV1`, `SellabilityV1`, acceptance receipts, "no cohort, no catalog version") and decisions SUB-D-28/29, D-409 do not exist here. The Atlas Plus overlay (2026-10-02) confirms the split: that Pricing model is the fork tip `diffora/bss/products @ 7d3544156` (D-384…D-469); upstream `main` here still has the CatalogVersion model (verified 2026-10-02) | SEAMS §L, §M-13, K-10…K-14 | **HIGH** (seam alignment) | adopt the Atlas only where it is consistent with this repository's owners (T-D-43); raise the Pricing model divergence with the pricing owner before any Atlas C01 work |
| R-18 | Price-binding authority (overlay T1) | ADR-0001: Rating selects the row on the scope key at the pinned catalog version. Atlas D02/D06: Pricing accepts bindings at order time, Subscriptions freezes them into term slices, Rating never re-resolves | ADR-0001; Atlas D02, D06; SUB-D-20 | **HIGH** | keep ADR-0001 selection; assert it equals the `priceId` the fact carries (fail closed `binding_mismatch`). Revisit if pricing builds acceptance receipts |
| R-19 | One-time charges (overlay T8) | T-D-18 / SUB-D-24: not rated, Billing values them "from the ref". Atlas R2: Rating rates them. In this repository the ref is Rating's own snapshot (T-D-39), so no other gear can value a one-time charge | SUB-D-24; Atlas R2 | **HIGH** | adopt Atlas R2 (a `one_time` child under the one-time fact); needs Subscriptions SUB-D-24 owner sign-off. Until then T-D-18 stands and one-time facts are stored, not rated |
| R-20 | Usage parent fact | Atlas C04/D13: Subscriptions publishes a usage **parent fact** per period at period opening. Subscriptions docs have no usage fact kind; SUB-D-07/SUB-D-19 and AC 27 require one priced line per `(subscriptionId, period, lineKey)` | Subscriptions S08:215, S-PRD:1346; Atlas C04 | **BLOCKER** (usage delivery to Billing) | adopt Atlas C04 (J-4). Interim (MIGRATION): Rating derives a usage period key from the subscription version and the frozen `billingAnchorPolicy` and delivers per that key, flagged `parent = derived` |
| R-21 | Finalization evidence (overlay T6) | Atlas D07/C05: a usage window finalizes only with IRM inventory, sealed scope, final coverage and a matching record-set digest. None of these sources exists; the coverage emitter has no owner (overlay T6) | SEAMS U-10, U-11, §L C05 | **BLOCKER** (usage invoicing) | keep the Atlas gate as the target. An interim `delay_only` finalization profile (delay elapsed, no evidence) requires explicit Finance/Product acceptance; deliveries under it carry `evidence_mode = delay_only` and late usage produces corrections |
| R-22 | Usage minimum fee / floor (overlay T8) | PRD: Rating emits a `PeriodFloorCapObligation`, Billing executes it. Atlas K3/C07: Rating applies the floor once per price/subscription/period | PRD `fr-period-floor-cap-obligation`; Atlas K3, C07 | **MEDIUM** | keep the PRD: obligation on the parent delivery, Billing executes. Rejecting hourly floors (Atlas C10) is adopted either way |
| R-23 | Aggregation scope | Atlas C10: `aggregation_scope ∈ {subscription_line, resource}` changes the price (F25). Pricing has no such field | pricing `price_row.rs`; Atlas F25 | **MEDIUM** | interim: `subscription_line` only; a row needing per-resource tiering cannot be expressed. Ask pricing for the field (J-12) |
| R-24 | Pre-purchase evaluation (overlay T3) | Orders PRD needs resolved totals at submit (p1, OL:214); Rating PRD marks pre-purchase evaluation `p2`; Atlas D04 makes it P1 | OL:114, 214; Atlas D04, C06 | **MEDIUM** | raise Rating `fr-pre-purchase-evaluation` to p1 and implement `OrderEvaluationV1` (T-D-49); blocked on R-02 for prices |
| R-25 | Usage attribution owner | Subscriptions assumes usage arrives keyed by `subscriptionId` (S-PRD:360); UC PRD says commercial identity is the consumer's; Atlas C04 has Subscriptions publish attribution segments | S-PRD:360; UC PRD `contract-downstream-usage-reader`; Atlas C04 | **BLOCKER** (usage rating) | Atlas C04 (T-D-52); Subscriptions must retract S-PRD:360 |
| R-26 | Ledger item granularity | A Rating parent line (one `line_key`) may span several prices; the ledger `InvoiceItemDto` carries one `price_id` and one `pricing_snapshot_ref` | SEAMS L-2 | **LOW** | composite snapshot per parent line (DESIGN §4.1); Billing chooses the item's `price_id` or splits items — Billing to decide |

### R-01 — Usage feed

- **Current state**: code (this branch and upstream `main`): `UsageCollectorClientV1` offers
  `list_usage_records` (OData keyset page over `(created_at, id)`, not an acceptance-order cursor)
  and `deactivate_usage_record` (in-place `status` flip, cascading to `corrects_id` compensations);
  records are instants. Documents (upstream `main`, commit `5de85f067`, 15 ADRs): a breaking pre-1.0
  V1 with `read_usage_feed(FeedSubscription, FeedStart {Oldest, After}, until, limit)`, interval
  entries with `entry_type`, invalidation as an entry, a six-part dedup identity behind the entry id,
  a backfill route, watermarks only on an operator reconciliation route, and type declarations in
  types-registry. The collector's DESIGN calls the code known debt and itself normative
  (DESIGN:2296). This branch's base `8aca4d6df` still carries the older documents.
- **Problem**: polling by event time misses late-accepted records behind the cursor and cannot see
  invalidations; a status flip is invisible mutation of financial input.
- **Options**: (a) usage-collector implements its documented V1 feed; (b) Rating polls
  `list_usage_records` by `created_at` with overlap windows; (c) the Atlas `UsageFeedV1` snapshot
  feed.
- **Recommendation**: (a), which the Atlas Plus overlay also asks for ("withdraw UsageFeedV1 and
  regenerate C05 from the Collector's design"). (b) is unsafe because `created_at` is
  caller-supplied and not an acceptance sequence, and a status flip is invisible mutation of financial
  input; (c) has no owner.

### R-02 — Pricing read contract

- **Current state**: `PricingCatalogClientV1` has one method, `pin_frontier`, with no implementation;
  the read model is persisted insert-only per `(tenant, catalog_version, subject)` and readable
  internally via `read_model_repo::delta_at`.
- **Options**: (a) SDK methods returning the projected documents at a pin; (b) a per-price
  "resolve at `t`" API; (c) Rating reads pricing tables directly.
- **Recommendation**: (a). The Atlas `PricingReadV1::price(price_id)` would be a reasonable *addition*
  (prices are immutable by id here too) but it does not carry windows, overlays or cohorts, so it
  cannot replace (a) under ADR-0001.

### R-03 — Subscriptions contract

- **Current state**: design prose; `BillableItemCreated` designed as outbox CloudEvents with
  "sufficiency, not schema"; no attribution, no recovery reads.
- **Recommendation**: the Atlas C04 surface — `BillableFactPublished` (versioned facts),
  `BillableSetSealed`, `AttributionSegmentChanged`, `UsageScopeSealed`, and
  `SubscriptionBillingReadV1::{facts, period_facts, segments_since, scope_for_window, bindings}`
  for recovery. Rating consumes events **and** reads through one inbox (T-D-51), so it works with or
  without a broker.

### R-04 — Period state owner (revised 2026-10-01)

- **Current state**: no Billing gear; no producer of `periodState`; the ledger's fiscal period is a
  different concept with no read API.
- **Previous recommendation (2026-09-25, T-D-42)**: a Rating-owned `open → closed` fence closed by
  Billing's `close_period`, with signed delta entries routed `post_close`.
- **Revised recommendation**: Atlas D09. Rating publishes the complete exact result of every parent
  fact at a monotonic `result_revision`; Billing compares each revision with its latest accepted
  target (including pending postings) and alone decides between replacing a draft and issuing a
  credit/debit note. `BillingPeriodStateChanged` is an observational hint and never gates Rating.
- **Trade-offs**: removes the cross-gear fence and the period row lock from Rating's hot path and
  makes rounding residues impossible to double-count (Billing compares rounded aggregates, F14/F32);
  requires Billing to keep per-fact accepted heads (Atlas C08).

### R-06 — Level meters

- **Options**: (a) amend the UC PRD so a charging consumer may integrate `MAX`/`LATEST` series and
  keep T-D-17; (b) adopt UC's rule — level products are emitted as `SUM` meters in the billable unit
  (GB·h per hour; hourly peak as a distinct `SUM` meter, one record per hour).
- **Recommendation**: (b), which the Atlas also takes (R1, F-A9). Interim: rows with
  `aggregationFunction ≠ sum` fail closed (`unsupported_aggregation`). Needs Product, Pricing and
  Usage Collector sign-off because T-D-17 was a confirmed product call.

### R-07 — FX

- **Recommendation**: launch native-currency only (DESIGN §2.2); add FX when the ledger or a Finance
  gear exposes pinnable rate snapshots (J-8).

### R-11 — Contracts / Promotions inputs

- **Recommendation**: steps 5–7 run with empty inputs; a price row carrying `discountRef` or a
  reservation needing a contract match fails closed (`coupon_source_unavailable`,
  `reservation_match_unavailable`). `CommitmentBalanceEffect` publication (T-D-10/T-D-27) is not
  built until a Contracts consumer exists.

### R-17 — Atlas baseline vs repository

Divergences verified 2026-10-01 (SEAMS K-10…K-14):

- Pricing here is Plan / PlanRevision / price rows on the 10-axis scope key, `PriceWindow`,
  `PriceOverlay`, cohorts, `CatalogVersion` pins. There is no PriceBook, no `resolve` route or DTO,
  no `PricingReadV1`/`SellabilityV1`, no acceptance receipt, no `UsageRatingPolicy`, no
  `PlanRevisionPublished`/`PricesPublished`; pricing decisions end at D-367.
- Recurring proration here is pricing's `prorationBasis` enum (`domain/contracts.rs:215-251`), not
  "actual elapsed UTC seconds / full cycle seconds" (Atlas C04).
- The hourly rating window already exists as `tierAggregationWindow = per_hour`
  (`price_row.rs:119-185`); the Atlas `UsageRatingPolicy.rating_window` maps onto it.
- Recurring frequencies here include quarterly/semiannual/custom (`plan_shape.rs:263-277`); the
  Atlas "month/year only" limit is a property of its baseline slice.

### R-21 — Finalization evidence

Target (Atlas C05/C10, T-D-47): a child window is final when `now ≥ window.to + delay` **and** its
ownership scope is sealed, the attribution projection has reached the scope's sequence, every
expected resource interval has a final coverage declaration, and the record count, quantity sum and
record-set digest Rating computes over its stored entries for those resources match the
declarations. The collector never judges completeness and has no snapshot token (its ADR-0011); the
emitter that would declare coverage has no owner (overlay T6: IRM excludes metering, the collector
never judges completeness). Without those sources none of the evidence can be proven. A `delay_only` profile is an explicit, recorded business acceptance
that delay elapsed is treated as the finalization trigger for that profile; it is never the default.

## Decided

| # | Decision | Status |
|---|---|---|
| T-D-01 | Select on the pricing canonical scope key verbatim; `phase` is a `phase_id`; grandfathering generation by the pinned price id's `cohort`. **Amended 2026-09-25**: the key has 10 axes (adds `meter`, `dimension_key`) per pricing code (R-10). | adopted; challenged by Atlas D06 (R-18) |
| T-D-02 | Step 4 stacks all scope-matching overlays; class-specificity order breaks cross-class ties. | adopted, pricing confirmed |
| T-D-03 | ~~Four-writer `pricingSnapshotRef`~~ — **superseded by T-D-39** (no writer existed on any side). | superseded |
| T-D-04 | Corrections never read live catalog state; tier counter key `(subscription, meter, dimensionKey, window)`. **Amended 2026-10-01**: the counter key is the child window's aggregation key (T-D-48, T-D-50); corrections reuse the child's pin-of-record (T-D-37). | adopted, amended |
| T-D-05 | `modelKind ∈ {flat, per_unit, graduated, volume, package}`; Volume Variant B not authorable; hybrid/committed are compositions. | adopted |
| T-D-06 | One publish-governance engine (pricing Slice 5); Rating registers fail-closed validators. | adopted; hook missing (R-12) |
| T-D-07 | `prorationBasis` (incl. `none`) and `billingAnchorPolicy` (D-20 clamp) adopted verbatim. | adopted |
| T-D-08 | Usage rows phase-invariant by default; reserved-rate two-source rule; rev-share pass-through. | adopted |
| T-D-09 | `commitmentReservation` snapshot segment. | dormant (R-11) |
| T-D-10 | `CommitmentBalanceEffect` publication and `balanceVersion` cascade. | dormant (R-11) |
| T-D-11 | Delta dedup is Rating's. **Amended 2026-10-01**: realized as the parent result revision and delivery identity (T-D-45); Billing derives monetary deltas. | adopted, amended |
| T-D-12 | Band continuity across intra-window boundaries; slices are lines of one child window evaluated together. | adopted |
| T-D-13 | Reservation split re-runs steps 3–5 over the on-demand remainder. | adopted |
| T-D-14 | Commitment pool flavors and true-up formulas. | dormant (R-11) |
| T-D-15 | Period-driven units for recurring / capacity / true-up lines. **Amended**: they are children of the recurring parent fact (T-D-44). | adopted, amended |
| T-D-16 | One `rating` gear; `rating-core` I/O-free crate (ADR-0002). | accepted |
| T-D-17 | Level aggregation via granule fold in Rating. | **suspended** — conflicts with usage-collector (R-06) |
| T-D-18 | `one_time` / `one_time_setup` charges are not rated. | adopted; reversal proposed (R-19) |
| T-D-19 | `overageRate` presence selects flat vs banded residual pricing. | dormant (R-11) |
| T-D-20 | Coupon cross-scope exclusivity. | dormant (R-11) |
| T-D-21 | Administrative re-rate is the only sanctioned re-pricing of already-final usage; it advances the pin-of-record. Realized as `rating_rerate_run` (DESIGN §3.6). | adopted |
| T-D-22 | `applyScope = line_total` fails closed. | adopted |
| T-D-23 | Reservation remainder band axis is window-cumulative. | adopted |
| T-D-24 | After an administrative re-rate, later re-evaluations of the same window use the advanced pin-of-record. **Reinstated 2026-10-01** (T-D-37 refined). | adopted |
| T-D-25 | `capacityCharge = reservedRate × reservedQuantity × coveredGranules`. | adopted |
| T-D-26 | Boundary day / straddling granule belong to the slice that opened them. | adopted |
| T-D-27 | Pool observation effects + over-draw detection. | dormant (R-11) |
| T-D-28 | ~~`periodState` sequence fence~~ — superseded by T-D-42, itself superseded by T-D-45. | superseded |
| T-D-29 | `usageCounterOnPlanChange` from the target plan; absence = reset. Realized 2026-10-01 as window-group membership across the new `sub_line_key` (DESIGN §4.3). | adopted |
| T-D-30 | Overlay line `cohort` filter (pricing D-78). | adopted |
| T-D-31 | Prefix-closed pin frontier (pricing D-114): Rating pins only what pricing's `pin_frontier` returns and never derives a pin. `pin_frontier` is declared, not implemented (SEAMS P-1). | adopted; dependency missing (R-02) |
| T-D-32 | Finance simulation deferred. | adopted |
| T-D-33 | The subscriptions period fact is the commercial WHEN; Rating never self-synthesizes a commercial period. **Amended 2026-10-01**: Rating's scheduler only schedules child windows inside a received fact (T-D-44); the R-20 interim derived usage key is a labelled migration, not a commercial period. | adopted; dependency ASSUMED (R-03) |
| T-D-34 | Subscriptions `lineKey` rule `plan#n` / `addon:{addOnId}#n` (SUB-D-21). Carried in Rating as `sub_line_key` (T-D-53). | adopted |
| T-D-35 | Upstream integration needs no event transport. **Refined by T-D-51.** | proposed 2026-09-25, refined |
| T-D-36 | **Copy what you rate**: usage records, pricing documents, subscription versions and facts a result uses are stored immutably in Rating under their version ids. | proposed 2026-09-25 |
| T-D-37 | **Pin rule (refined 2026-10-01)**: a provisional evaluation pins the current frontier; the pin in force when a child window first becomes `final` is its **pin-of-record**; every later re-evaluation of that window (input correction) reuses it; only an administrative re-rate advances it (T-D-21, T-D-24). Safe because pricing's future-only effective dating means no pin published after `window.to` changes the window's prices (SEAMS P-3). | proposed |
| T-D-38 | A usage child window is one aggregation window of one fact; split points inside the fact (window activation, phase, money-only term slice) make slices = lines. **Amended 2026-10-01**: the window is the child of a parent fact (T-D-44); a tier window spanning a billing-period boundary or a `carry` plan change is a **window group** of children sharing band context (DESIGN §4.3). | proposed 2026-09-25, amended |
| T-D-39 | `rating_snapshot` is content-addressed (`rsnap1:` + SHA-256); its id is the `pricing_snapshot_ref` Billing passes to the ledger. | proposed 2026-09-25 |
| T-D-40 | ~~Charge entries are append-only signed deltas keyed `(unit_id, line_key, to_version)`~~ — **superseded by T-D-45**. | superseded |
| T-D-41 | `engine_version` is recorded on every result and changes amounts only through an administrative re-rate. ~~Nano-minor half-even amounts~~ — **superseded by T-D-46**. | proposed 2026-09-25, partly superseded |
| T-D-42 | ~~Rating owns the billing-period fence~~ — **superseded by T-D-45** (R-04 revised). | superseded |
| T-D-43 | **Atlas adoption rule.** Seam Atlas v2 baseline 1.1 is the target for Rating's cross-gear seams where it is consistent with this repository's dependency owners; every Atlas contract is labelled *proposed* until its owner adopts it; Atlas positions that depend on its PriceBook Pricing model are not adopted (R-17). | proposed 2026-10-01 |
| T-D-44 | **Parent fact / child window** (Atlas D13, D15, C07, C10). A commercial parent fact authorizes rating; Rating derives the child windows (`WindowEvaluationKey = {fact_id, canonical_window, aggregation_key}`), persists a `WindowSchedule` with a `next_due_window_start` cursor, and owns scheduling, input waits, retry and catch-up. Subscriptions emits no per-window timer. | proposed 2026-10-01 |
| T-D-45 | **Absolute replacement delivery** (Atlas D09). Each parent fact gets a complete exact result at a monotonic `result_revision`; delivery is `BillableItemDeliveryV1`; there is no delta lane and no Rating period fence; `BillingPeriodStateChanged` is a hint. Supersedes T-D-40, T-D-42. | proposed 2026-10-01 |
| T-D-46 | **Exact amounts** (Atlas C00, D10). Amounts are reduced rationals (`ExactAmount {numerator, denominator}`) in **minor units of the price currency**; Rating never rounds; Billing sums contributions per `invoice_line_key` and rounds once, HALF_EVEN, to integer minor units. Supersedes the rounding part of T-D-41. | proposed 2026-10-01; Billing to confirm |
| T-D-47 | **Finalization gate** (Atlas D07, C05, C10, P7). A usage child becomes `final` only when its pinned `FinalizationPolicy` delay has elapsed after `window.to` and its evidence is complete (R-21); an advance `period_line` is final at acceptance, an arrears one when its period has ended. A parent delivery is emitted when every expected child is final and — for usage and arrears facts — the served period has ended. Missing evidence is `pending`, never zero; a proven empty window is an explicit zero. | proposed 2026-10-01 |
| T-D-48 | **Window-scoped tiers and boundary splits** (Atlas D14, C05, C10). Tier `Q` is scoped to one child window and one aggregation key; `per_hour` is the UTC calendar hour; thresholds are never prorated for partial windows; a record crossing a window, price-slice or payer boundary must arrive split with exact quantities, otherwise `boundary_split_required`. Supersedes the R-15 interim. | proposed 2026-10-01 |
| T-D-49 | **Pre-purchase evaluation** (Atlas D04, C06) is `OrderEvaluationV1::evaluate`: pure, persists nothing, separate DTO from fact rating, same arithmetic library; usage excluded from TCV. | proposed 2026-10-01 |
| T-D-50 | **Payer in billing identity** (Atlas D08). Aggregation keys and deliveries carry all tenant axes; a payer transfer is a fact boundary; a usage interval crossing it needs an exact split. | proposed 2026-10-01 |
| T-D-51 | **Inbox-first intake** (Atlas C00, C09, P8). Every non-usage upstream fact (commercial fact, segment, scope, coverage, Billing hint) is accepted into a Rating inbox keyed by `(source, business_id, version)` with a payload digest (usage is deduplicated by `rating_usage_record` itself); pull reads and events are interchangeable sources; same id+version with a different digest is quarantined. Refines T-D-35. | proposed 2026-10-01 |
| T-D-52 | **Attribution from Subscriptions** (Atlas C04, P3). Usage is attributed by a local projection of Subscriptions attribution segments (gap-free per `resource_tenant_id`), and a window's ownership is proven by a `UsageScope` from `scope_for_window`; no per-record lookup. Supersedes the 2026-09-25 `resolve_resource` proposal. | proposed 2026-10-01 |
| T-D-53 | **Line identities** (Atlas C07). `line_key` = a contribution inside a parent fact (`fact_id` + item/dimension + kind + accounting/tax component); `invoice_line_key` = Billing's rounding group (billing group + `sub_line_key` + item + dimension + kind + currency + unit + GL/tax/template/rounding class); Subscriptions' `lineKey` is carried as `sub_line_key`. None contains an hour, a child revision or a price id. Replaces the 2026-09-25 `{billing_period_start}/{slice_start}/{price_id}` key. | proposed 2026-10-01 |
| T-D-54 | **Finalization policy is Rating's** (overlay T7, X12). `FinalizationPolicy {policy_id, version, seller_tenant_id?, window_policy, delay, evidence_mode}` is Rating-owned, versioned configuration in `rating_finalization_policy`; fact intake resolves the most specific policy (seller, then platform default) for the fact's window policy and pins its id and version into the `WindowSchedule`; a later version never moves a pinned deadline; no matching policy fails the schedule `finalization_policy_missing`. Illustrative values: `per_hour` PT5M, `calendar_month` PT48H (Atlas D12); not SLAs. | proposed 2026-10-02 |

## Carried product opens

Unchanged from [`PRD.md`](./PRD.md) §15 and still open: NFR ratification (R-13); clamp-vs-credit for
negative lines; default `maxCumulativeMarkup` and clamp-vs-hard mode; coupon equal-benefit
tie-break; mixed-settlement coupon rules; plan-scoped coupon base (T-D-22); `whole_unit` attribution
on a split (interim: fail closed); seat-change boundary transport with Subscriptions (SUB-R3);
bundle-level coupon attachment; composite × dimensions input-join rule; floor vs coupon clawback.
