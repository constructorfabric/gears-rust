# Rating ⇄ Pricing — Complementarity Seam Map

> Cross-gear seam analysis produced before the Tariffs design, against the mature
> **pricing** design set (`gears/bss/pricing/docs/`). The pricing gear is the ratified
> System of Record for the canonical scope key, PriceWindow machinery, publish-time
> governance, and the frozen consumer contract (`design/06`). The Tariffs PRD is the
> incoming baseline (`gears/bss/rating/docs/PRD.md`, vendored from upstream PR #4143).
>
> **Verdict legend:** `T-adopts` = Tariffs adopts a pricing-side fact (no pricing change);
> `P-extends` = pricing must expose/extend something; `Joint` = a shared contract needs a
> co-decision; `Product` = a launch-scope / commercial call for the product owner.
>
> Severity: `CRIT` (breaks resolution correctness), `HIGH`, `MED`, `LOW`.
> Line refs: `T:` = tariffs PRD.md (frozen historical baseline — its line numbers stay valid).
> Pricing / usage-collector refs were re-anchored to **sections/ids** on 2026-07-28: those docs
> are living and their line numbers had drifted (2026-07-22 review, finding #7).
>
> **2026-07-11 (ADR-0002 / T-D-16)**: the Tariffs gear was consolidated into the **rating** gear
> as its evaluation core (`rating-core`). This map is the **historical record** of the 2026-07-10
> analysis: "Tariffs" below reads as the rating gear's evaluation core, and "Rating" as the rating
> pipeline (same gear since consolidation). All seam resolutions remain binding verbatim.

---

## A. Canonical scope key & resolution order

| # | Sev | Verdict | Seam |
|---|-----|---------|------|
| **K1** | CRIT | T-adopts | **Step-2 selection key is 4-axis, owner's is 8-axis.** Tariffs selects on `(planId, currency, region, phase)[+priceOverlay]` and asserts "at most one window MUST match" (T:125, T:447). Pricing's canonical key is 8 additive axes `(planId, currency, region, priceOverlay, phase, priceEligibility, chargeKind, cohort)` (pricing design/01 §4.1 canonical scope key), and it *deliberately* publishes many concurrently-active rows on the shorter tuple (hybrid chargeKinds; grandfathered generations + successor). Tariffs' "one match" fails-closed on exactly the legitimate catalogs pricing engineered. → Tariffs adopts the 8-column non-overlap/selection key verbatim (already a cross-team obligation: `ADR/0002:75`, `ADR/0001:102`). |
| **K2** | CRIT | T-adopts | **cohort / multi-generation grandfathering absent.** Tariffs models a *flat* `existing_grandfathered` (activated-before-cutover, T:521). Pricing's ADR-0002/D-02: N generations coexist, each an active window; Tariffs must select the row whose `cohort` = the cohort of the subscription's **pinned price id** in `pricingSnapshotRef` (pricing ADR/0002 decision §, design/07 grandfathering-eligibility algo). No new store — the pin already exists. → Tariffs adds class-then-cohort within-class selection. |
| **K3** | HIGH | T-adopts | **`chargeKind` is not a selection axis.** Recurring and usage rows differ *only* by `chargeKind`; without it in the key, step-2 selection is non-unique (T:413 knows hybrid emits two lines but never keys on it). Pricing: `chargeKind ∈ {recurring, usage, one_time, one_time_setup}` is column-6 (pricing design/01 §4.1, axis 6). → include `chargeKind` in the step-2 key; each emitted line resolves its own row. *(T-D-18, 2026-07-28: the `one_time`/`one_time_setup` values select rows for coverage/preview only — no evaluation unit is synthesized for them; billed at sale by Subscriptions/Billing.)* |
| **K4** | MED | T-adopts | **Eligibility class order unstated.** Pricing: `existing_grandfathered > new_subscriptions_only > all_subscriptions`, class ordering only (pricing design/07 §1.6 / eligibility algo). Tariffs implies precedence but never states the total order. → state it as the step-2 tie-break (pairs with K1/K2). |
| **K5** | MED | T-adopts | **`phase` axis typing drift.** Tariffs refers to phase by kind-name (trial/intro/evergreen, T:531). Pricing D-19: `phase` axis is always a `phase_id` (uuid); non-phased/one-time rows ride an implicit **terminal `phase_id`** (pricing design/01 §4.1, phase axis). A name-based match won't join the uuid axis. → specify `phase` as `phase_id`; kind names are display only. |

## B. Overlays / PriceOverlay precedence

| # | Sev | Verdict | Seam |
|---|-----|---------|------|
| **O1** | HIGH | T-adopts | **Cross-class tie-break uses `priceOverlayId`; owner mandates class-specificity order.** Tariffs: ascending `precedence` then ascending `priceOverlayId` (T:479). Pricing: `precedence` is unique only *within* a class; cross-class ties resolve by `customerGroup > partner > orgTier > brand > region > global`, which "Tariffs MUST adopt verbatim" (pricing design/09 precedence rules). → replace the `priceOverlayId` cross-class tie-break with the class order. |
| **O2** | HIGH | T-adopts | **`customerGroup` scope class missing.** Tariffs scope enum = `{partner, orgTier, brand, region, global}` (T:126). Pricing adds `customerGroup` as a first-class, server-resolved segment overlay and the *most-specific* class (pricing design/09 scope classes + precedence rules). → add `customerGroup` to the step-4 scope mapping (matches payer's BSS-resolved group at `t`). |
| **O3** | HIGH | **RESOLVED → stack** | **Stack-all vs single-winner at step 4.** Tariffs *stacks* all scope-matching survivors sequentially (T:479); pricing frames overlay resolution as most-specific-*winner*. **Decision (2026-07-10):** step 4 **stacks** all survivors (partner + brand + region overlays are legitimately cumulative). Pricing's "most-specific-wins" governs (a) `priceEligibility` *class* selection and (b) *ties at equal precedence* — NOT overlay exclusivity. The class-specificity order (O1) is the tie-break within the stack, not a winner-take-all filter. Pricing **confirmed 2026-07-28** (design/09 `inst-plv-class-tiebreak` now states stack-all normatively — every scope-matching overlay contributes its most-specific line to the sequential stack; the grandfathering analogy was dropped as the opposite semantics). O3 fully closed. |

## C. PriceWindow ownership & events

| # | Sev | Verdict | Seam |
|---|-----|---------|------|
| **W0** | — | ALIGNED | **Marquee ownership (D-03/ADR-0003): no conflict.** Tariffs treats PriceWindows as externally-owned read-only inputs — Catalog/Price-Book is "SoR for … PriceWindow; emits schedule-change events" and Tariffs "MUST consume PriceWindow* events" (T:212/261). Pricing (Slice 7) owns the store, state machine, activation job, and event production (`ADR/0003:72`). The pricing gear *is* the Catalog/Price-Book SoR Tariffs references. |
| **W1** | HIGH | T-adopts | **`PriceWindowCancelled` not consumed.** Pricing emits four events incl. `Cancelled` (retirement/cutover unwind, operator DELETE of not-yet-active windows; pricing design/07 WindowScheduler/state machine). Tariffs' consume list omits it (T:261). A pre-cached scheduled window that pricing later cancels would never be retracted → Tariffs resolves against a voided window. → add `PriceWindowCancelled` to the consumed set. |
| **W2** | MED | **RESOLVED → Tariffs replays snapshot** | **Open-period re-resolution needs historical windows.** Tariffs re-resolves late-arriving usage for `periodState=open` and mid-period splits (T:663/697), which read window(s) effective at a *past* `t`. Pricing's read-model contract exposes only *active-window-at-`t`* (pricing PRD sellability-gate read-model FR); history is in-table SoR (R-12). **Decision (2026-07-10):** Tariffs re-resolves **strictly from the pinned snapshot** (the snapshot pinned at first rating of that window), never a live catalog read — for both posted and open-period late-arrival. No pricing read-model change; pricing only guarantees snapshot retention for the open window. (Rejected: pricing exposing a historical-window query surface — more cost, weaker determinism.) |

## D. Snapshot composition

| # | Sev | Verdict | Seam |
|---|-----|---------|------|
| **S1** | MED | **RESOLVED → canonical list** | **`pricingSnapshotRef` = one ref, four writers; segments not textually reconciled.** Minting authority *is* aligned — pricing names Tariffs the composition SoR (pricing design/01 §4.4 pricingSnapshotRef). **Decision (2026-07-10):** canonical field list, per-segment writer: `catalogVersion` (**registry (products gear)** — the sole incrementer since the D-66 producer split; pricing only requests addressability; "three writers" was the pre-split count — 2026-07-31 review #10) · `resolved price ids` incl. `cohort` (pricing) · `eval-policy version` incl. the T-D-17 level-aggregation triple (pricing) · `(currency, region)` binding (Subscriptions @ activation) · resolved overlay/`priceOverlay` ids (Tariffs @ eval) · applied coupon ids + stacking policy (Tariffs @ eval) · FX-lock id (Tariffs @ eval). Tariffs = composition SoR; all copies record the list identically. "resolved price ids" carry `cohort` (feeds K2). **2026-07-11 (T-D-09):** the canonical list gains the eighth Tariffs-written segment `commitmentReservation` (reservation match; pool set incl. `poolType`, balances @ `balanceVersion`, draw order, rollover, **and the optional `overageRate` — the T-D-19 selector, restored to this "canonical" list by the 2026-07-31 review #5**; reserved-vs-pool split). |

## E. Pricing models & metering

| # | Sev | Verdict | Seam |
|---|-----|---------|------|
| **M1** | HIGH | T-adopts | **model-kind enum mismatch.** Pricing enum: `{flat, per_unit, graduated, volume, package}` (pricing PRD §6.2 explicit-model-kind FR). Tariffs §6.2: `{flat, tiered(graduated), volumeA, volumeB, hybrid, committed}`. `hybrid`/`committed` are plan-composition/commercial constructs, not model-kinds (pricing already treats them so). → adopt the pricing §17.2 kind→formula mapping as shared SoR; reclassify hybrid/committed. |
| **M2** | HIGH | **T-adopts (launch-blocking)** | **`per_unit` (per-seat) uncovered.** Pricing p1 launch (`quantitySource ∈ {subscription_seat_count, manual}`, pricing PRD §2.2 per-seat row + §6.2 per-unit FR); FixtureGate blocks publish without a joint Tariffs fixture. Tariffs has no per-seat FR (classifies it Follow-on, T:1273). → Tariffs adds a `per_unit` formula FR (unit price × seat count from Subscriptions) + joint fixture, or per-seat plans cannot publish. |
| **M3** | MED | **RESOLVED → T-adopts** | **`package` (block) modelKind — pricing has it, Tariffs has zero (0 hits).** Pricing: `ceil(used/packageSize) × packagePrice`, Tariffs computes the round-up (pricing PRD §1.4 package row + §6.2 model-kind conformance FR); p2, own joint fixture required. `package` ≠ Variant B (different math). **Decision (2026-07-10):** Tariffs adds a package-pricing FR (`ceil(used/packageSize) × packagePrice`) — parity with pricing p2 launch. |
| **M4** | MED | T-adopts (delete) | **Volume Variant B (block fee) is an orphan.** Tariffs authors it (T:399); pricing explicitly refuses — "`volume` maps to Variant A only … Variant B dropped and not authorable" (pricing design/03 §1.6 Q3). No catalog authoring home. → delete Variant B from Tariffs §6.2. |
| **M5** | MED | **RESOLVED → T-adopts** | **Composite/derived meter status drift.** Tariffs: Follow-on, "blocked on a derived-meter primitive that must land first" (T:1259). Pricing *delivers* it at launch (Slice 10 formula-as-data over ≥2 units) and hands Tariffs the eval math (pricing design/10 §1.1, PRD `fr-derived-composite-meter`). **Decision (2026-07-10):** composite **is in launch**; Tariffs drops the Follow-on marker and adds a composite-formula eval FR consuming the pricing-declared derived meter. |
| **M6** | MED | Joint | **Dimensional pricing status/ownership.** Tariffs §6.7 p1 launch-critical; pricing glossary marks dimensional **Future** yet persists `dimension_key` structurally (pricing design/03 §6 `dimension_key` column). Both effectively launch empty-tuple (OSS emission gates values). Declaration ownership unassigned. → agree wording: declaration+freeze in-scope now (catalog persists `dimension_key`, Tariffs freezes in snapshot); value-pricing OSS-gated/Future. **Joint wording landed both sides 2026-07-28** (pricing design/03 §6 dimension_key note + rating design/03 §4.2) — M6 closed. |
| **M7** | MED | **RESOLVED → superset key** | **Tier-counter `Q` key differs.** Tariffs single-writer key `(meter, dimensionKey, window)` (T:457); pricing `Q` is per `(subscription, meter, window)`, survives supersession (pricing design/03 `inst-tb-window-continuity`). **Decision (2026-07-10):** canonical key = `(subscription, meter, dimensionKey, window)` — the superset satisfying both (per-subscription reset scope + per-dimension counters). Recorded identically on both sides **since 2026-07-28**: pricing `inst-tb-window-continuity` had kept the 3-tuple (no `dimensionKey`) — now landed as the 4-tuple there too (`dimensionKey` = the empty tuple until OSS dimensional emission, M6, so the keys coincided at launch anyway); Rating remains single-writer per this key. |
| **M8** | LOW | Naming | **"prepaid" collision.** Tariffs "prepaid pool" = `commitmentPools[]` (Contracts SoR, waterfall). Pricing "prepaid credit grant" = plan-attached wallet (`grantAmount`/`expiryPolicy`), balance owned by Billing/Rating, GA-gated. Different constructs, same word. → name distinctly; note in Tariffs §6.6 the wallet grant is a separate Billing-executed drawdown, not `commitmentPools[]`. |
| **M9** | LOW | T-adopts | **Reserved-rate sourcing split unstated.** Pricing: self-service reserved rate from the catalog snapshot; negotiated RI rates from Contracts (pricing PRD `fr-reserved-capacity`, design/10 §3 Reserved-Capacity Attributes). Tariffs prices "the reserved rate" without the source split. → Tariffs §6.6 states the two-source rule (snapshot vs Contracts overlay/step 5). |
| **M10** | LOW | T-adopts | **`aggregationFunction = sum`-only launch limit stated only by pricing** (pricing PRD §1.4, design/03 Q2; peak/last/unique Future). Tariffs §6.5 silent. → *(the original action "add the sum-only note" is void — superseded by the re-scope below; 2026-07-28 review fix)* **RE-SCOPED 2026-07-16 (product call — pricing D-44 / rating T-D-17)**: the launch product set bills on levels (cloudlet peak-per-hour, storage GB-month) → `aggregationFunction ∈ {sum, peak, time_weighted}` + `aggregationGranularity {hour, day}` in launch; non-`sum` = granule fold over gauge samples, `Q` = Σ granule folds (**additive** — M7/T-D-12/corrections untouched); frozen in the snapshot; joint fixture required; `last`/`unique` stay Future; no composite co-occurrence. Recorded both sides: pricing PRD §1.4/§15(F-40)/§17.8 + design/03 Q2; rating PRD `fr-level-aggregation` + design/03/12/13. |
| **M11** | LOW | T-adopts | **D-17 open-top & D-18 usage-only invariants relied on but unrecorded.** Pricing forbids closed-top bands (`TIER_TOP_CLOSED`, deletes the "fail-closed-above-max" branch, D-17) and restricts graduated/volume/package to `chargeKind=usage` (D-18). Tariffs is consistent (no above-max branch, tier machinery presupposes metered `Q`) but never states the guarantees. → add a one-line "catalog guarantees" note. |
| **M12** | HIGH | **Joint — OPEN (2026-07-28 cross-gear review)** | **D-40 `tierQualificationWindow` counterpart absent on the rating side.** Pricing D-40 (DECIDED 2026-07-12, CONFIRMED 2026-07-28): a tiered usage row MAY carry `tierQualificationWindow` (`current` default \| `trailing_period`) — `trailing_period` qualifies the rate tier from the **prior anchor-derived billing period's total** (single-band volume-style selection), **locks that one rate** for the current period, and bills actual usage at `billingGranularity`; first-period **bootstrap** resolves to the **lowest tier unconditionally** (the "or an authored bootstrap" escape hatch was cut by the 2026-07-28 review fix — it named no field, no publish check and no snapshot segment). Corrected 2026-07-29 (pricing D-60): the lock is **per subscription per billing period** and therefore does **not** live in `pricingSnapshotRef` (plan-scoped and immutable) — pricing freezes only the window + band set, and **rating owns the pin**: `{priceId, periodStart, qualifiedBandId, lockedUnitPriceMinor}` written on the period's rated charges beside `pricingSnapshotRef`, which is the replay source. Pricing now also carries the matching publish gates (`TIER_QUAL_WINDOW_INCOMPATIBLE`, `TIER_QUAL_ZERO_BAND_LOCK`) and the `trailing_tier` `FixtureGate` variant that makes this entry's publish block real — **design-set only, corrected 2026-08-17**: none of the three exists in pricing's code. The two codes appear in a doc comment and inside the refusal message saying they have *not* landed, and `bss_fixtures::Variant` has no `TrailingTier` member (recorded as deliberate). **The block this entry depends on holds anyway, by a stronger route**: pricing refuses `tierQualificationWindow` on *every* authoring path with `PRIMITIVE_RULES_UNBUILT` — `refuse_unlanded_primitives` at the authoring door, `unjudged_primitives` at publish and at bulk import — so the field cannot be authored at all, rather than being authored and then gated at publish. Cite that mechanism, not the three named gates, until they land. The ownership split names **Rating**: "Rating computes the trailing aggregate and re-qualifies at each period boundary; Tariffs applies the locked rate" — declared "part of the **joint Rating contract** (PRD §consumer-contracts)" (pricing design/10 `cpt-cf-bss-pricing-algo-trailing-tier` + `inst-tt-joint`; pricing PRD §Trailing-tier qualification). No rating artifact mentions the primitive (PRD, slices 03/13/14, this register — zero references before this entry). → OPEN: rating must author the counterpart — trailing aggregate over the prior `AnchorPeriod`, period-boundary re-qualification, locked-rate pin read by step 2 — plus the joint fixture, **before any `trailing_period` row publishes**; until adopted the primitive is unrateable. |

## F. Proration, FX, anchors

| # | Sev | Verdict | Seam |
|---|-----|---------|------|
| **P1** | HIGH | **T-adopts (live CI gate)** | **`prorationBasis` enum missing `none`.** Pricing canonical `{calendar_days_actual, calendar_days_30, by_second, whole_unit, none}`, adopted verbatim by Tariffs, guarded by CI gate `pricing.contracts.enum_drift` (Critical) (pricing design/06 proration-basis enum + CI-gate note). Tariffs lists only 4 (T:114). → add `none` verbatim (pricing rejects `creditOnDowngrade=true` + `none` at publish, so Tariffs never prorates a `none` row, but must recognize the value for the conformance fixture). |
| **P2** | MED | T-adopts | **`billingAnchorPolicy` + D-20 no-drift clamp not referenced.** Pricing owns `billingAnchorPolicy ∈ {calendar_month, subscription_start, fixed_day(d)}` with the 31→28→31 clamp, frozen in the snapshot (pricing design/06 billingAnchorPolicy rule, D-20). Tariffs uses a generic "subscription billing anchor" (T:255). → Tariffs consumes `billingAnchorPolicy` (frozen) as the authority for `invoice_period` boundaries and plan-change split points. |
| **P3** | HIGH | **T-adopts (2026-08-01, T-D-29)** | **`usageCounterOnPlanChange` — the plan-change tier-`Q` continuity flag (pricing D-113).** Rating `design/09` had consumed "carry-vs-reset flags … all in the pinned snapshot" while **no pricing document defined any such field** — pricing D-113 (2026-07-31; **CONFIRMED by the product owner 2026-08-01**) created the single flag `usageCounterOnPlanChange ∈ {reset (default) \| carry}` on the plan, and named the Rating adoption owed. Adopted (T-D-29): the **target** plan's frozen flag governs at an in-place change; **absence = `reset`, never a rating failure** (supersedes `design/09`'s "fail-closed when absent"); `carry` honoured per shared `(meter, dimensionKey)` line **only** under the D-82/D-98/D-122 unit-field match across both frozen snapshots (else reset + operator signal); **no pool flag is published** — commitment-pool carry stays Rating/Contracts-side, default reset. Rating `design/09` §3.1/§3.5/§4.3, `design/03` §4.3, `design/05` §4.5. |
| **F1** | HIGH | T-adopts | **D-15 phase-invariant usage fallback absent, collides with strict no-gap.** Tariffs: key includes phase + fail on gap (T:141/447). Pricing D-15: usage rows are phase-invariant by default, phase-specific wins — "adopted verbatim by Tariffs" (`DECISIONS:204`). Literal Tariffs raises a spurious eval-fail for a phase covered only by a phase-invariant usage row. → Tariffs encodes phase-fallback for usage rows so the no-gap rule applies to the *resolved* set. |

## G. Bundles, governance, contracts, ASC 606

| # | Sev | Verdict | Seam |
|---|-----|---------|------|
| **G1** | HIGH | **RESOLVED → single engine** | **Double governance.** Both gears claim the two-person publish workflow (Tariffs `fr-publish-approval-governance`, T:766; pricing Slice 5 owns `MaterialityEvaluator` + `ApprovalWorkflow` end-to-end, pricing design/05 §1.1 + materiality algo). Same artifact (price-overlay/overlay publish). **Decision (2026-07-10):** single catalog-publish approval engine = **pricing Slice 5**; Tariffs contributes its four publish-time checks as **registered fail-closed validators** in the pricing pipeline, NOT a second workflow. Contract-level overrides (step 5) stay with Contracts. **Ledger stays separate** — see note below. |
| **B1** | MED | T-adopts | **Bundle summing assigned to Tariffs, but Tariffs is silent on bundles/rev-share.** Pricing: for `sum_of_parts` "the summing is Tariffs'" (pricing design/08 sum_of_parts summing rule); rev-share normalized to `effective_share_bp` at publish, absorber default platform, "downstream reads only effective shares" (D-07). Tariffs never mentions bundle/rev-share. Clean ownership split (no overlap), but Tariffs must acknowledge the eval-time summing + effective-share pass-through. → add `sum_of_parts` summing + rev-share pass-through to Tariffs scope + §9.2 note. |
| **C1** | MED | T-adopts | **No Catalog/Pricing read-model consumer contract in Tariffs §9.2.** Tariffs §9.2 lists only Rating/Finance/Promotions/Billing (T:818). Pricing `design/06` is a full frozen consumer contract Tariffs must adopt (prorationBasis verbatim, rating-compat `{skuId,planId,priceId}`, grant set, plan-change contract). → add a §9.2 "Catalog/Pricing read-model input contract" + a Subscriptions `(changeEffectiveAt, changeMode)` input contract. |
| **ASC** | LOW | P-extends (deferred) | **SSP/PO source gap.** Tariffs requires `performanceObligationRef` + `sspSnapshotPointer` (nullable, T:754). Pricing supplies `glCode` now but defers SSP to Future (pricing PRD §15 SSP row). Fields resolve **null at MVP** — no contradiction. → acknowledge null@MVP on both sides; `glCode` flows now. |

## H. Naming

| # | Sev | Verdict | Seam |
|---|-----|---------|------|
| **N1** | LOW | Naming | **Gear-name drift.** Tariffs says "Catalog / Price Book" / "Product Catalog"; pricing gear is "Pricing / Product Catalog" and names the consumer `cpt-cf-bss-pricing-actor-rating`. → pin one canonical name for the pricing/catalog gear across both PRDs. |

---

## I. Registry (Product & SKU) — prospective seams (2026-07-11, PR #4177)

> The **Product & SKU Management** PRD carves the catalog **registry** out of manifest §4.1:
> Product / SKU / Category / Attribute, lifecycle, approvals, and the registry-wide
> `CatalogVersion`. The rating gear becomes a **second-order consumer** (registry → plan-price →
> rating); the resource half of "resource @ price" is the SKU's **metering-unit declaration**.
> **2026-07-16 — vendored**: the PRD now lives on this branch at
> `gears/bss/products/docs/PRD.md` (from `constructorfabric/gears-rust` PR #4177 @ `6d3aab4`;
> upstream-detach decision — this branch is canonical, no PR engagement owed). The RG2 fix is
> **applied in the vendored copy**; RG1 stays open; RG3 is recorded as localization debt in the
> vendored PRD's provenance note.

| # | Sev | Verdict | Seam |
|---|-----|---------|------|
| **RG1** | HIGH | Joint | **Freeze protocols must compose.** The registry gates posted/contractual resolution on `freezeComplete` (participants: plan-price, Contracts, Billing — **rating absent**); pricing gates the rating pin on `CatalogVersionPublished` + warm-completion (pin lag ≤ 5s); rating replays posted periods strictly from pinned snapshots (W2). One composed contract is needed: is pricing's warm marker the rating-facing freeze ack, or must rating join the participant set? A registry-side reject-until-freeze must not stall the rating pin path. |
| **RG2** | HIGH | Joint | **Single-unit-per-SKU vs dimensional & composite pricing.** Registry: a usage SKU declares **exactly one** metering unit; multi-dimension usage = **separate SKUs** composed at plan/bundle level. Rating/pricing: dimensional pricing is one meter + `dimensionKey` tuple **lines** (rating PRD §6.7), and composite meters are formula-as-data over **≥ 2 input units** (pricing design/10). Where composite input units and declared dimension sets live in the registry model must be pinned jointly — "separate SKUs" and "dimension tuples on one meter" are different shapes. **Proposed pin (2026-07-16, BSS side, for PR #4177 FR `…fr-metering-unit-declaration`):** the root defect is a **unit ≠ dimension conflation** ("exactly one unit *(single-dimension)*"). (1) Keep the invariant verbatim: one usage SKU = one metering unit (counted identity, immutable). (2) Drop "(single-dimension)" and the separate-SKUs mandate: the **declared dimension set** over a meter is a **plan-price concern** — the registry itself delegates "plan-level meter binding is plan-price" (PR PRD:126), pricing already persists `dimension_key` on the plan-price revision (rating PRD §6.7), rating freezes it in the snapshot; no new registry field needed. (3) Separate SKUs demoted rule→option (legitimate only where variants differ commercially: accounting codes/lifecycle); per-combination minting is the rating-PRD §17.3 "exploding cardinality" workaround and must not be the permanent shape (S3 = 6×30×5 = 900 SKUs vs 1 SKU + 3 keys). (4) Composite: the SKU declares the composite's **output** unit as its one unit; input units are referenced by the plan-price formula against the recognized-unit set and **need no SKUs**; "composed at plan/bundle level" must not conflate bundle composition with meter formulas. **APPLIED 2026-07-16** in the vendored copy (`gears/bss/products/docs/PRD.md`, `fr-metering-unit-declaration` reworded; provenance note records the local change) — RG2 **closed**. |
| **RG3** | MED | Naming | **Pre-consolidation naming.** The draft references `PRD-tariffs-pricing-logic` and `PRD-rating-engine` as separate modules and mints `cpt-cf-bss-products-actor-tariffs`. Post ADR-0002 there is one **rating** gear (evaluation core + pipeline; the rating-engine PRD is absorbed), and pricing's consumer actor is already merged into the rating actor. **RECONCILED 2026-07-16** in the vendored copy (first substantive edit, per its provenance note — the same edit pass as pricing **D-46**, which invoked the first-substantive-edit rule): `…-actor-tariffs` merged into `…-actor-rating`; §2.1 delegations + §17 reference table localized to `gears/bss/rating/docs/PRD.md`; `refs:` front-matter kept verbatim as provenance — RG3 **closed**. |

Aligned (no action needed): unit-identity **immutability** + deprecate-then-remove (GB ≠ GiB — a
correction is a new unit) matches the frozen-snapshot / pinned-replay doctrine (W2, T-D-04); the
`{skuId, planId, priceId}` rating triple keys off the registry's immutable `skuId`; the
grandfathering invariant (snapshots never mutated) matches posted-period protection.

---

## J. Usage Collector (OSS metering) — ingestion seams (2026-07-16)

> Pipeline slice [`design/12`](./design/12-usage-ingestion-normalization.md) was designed against
> an abstract "OSS metering" upstream that emits a **durable at-least-once usage stream**. The
> platform's actual metering SoR is the **built v1 Usage Collector**
> (`gears/system/usage-collector/` — implemented, released): deliberately **synchronous
> request/response on every public surface**, with a polled Query SPI that is *eventually
> consistent with no upper bound* at the gear floor, and events / watermarks / backfill
> **explicitly deferred** to a later phase (UC-DESIGN §3.12.3, §3.12.2 deferred-items table,
> UC-PRD §4). Its domain is product-neutral — `UsageType` (gts_id) / tenant / `resource_ref` /
> `subject_ref` / closed-shape `metadata` — with **no subscription, meter, plan, price, or
> dimension concepts** (business logic is an explicit non-goal, UC-PRD §1.2). The seams below
> bridge the slice-12 design intent to the built gear; **UC1 gates slice-12 implementation**.
> Line refs: `UC-PRD:` / `UC-DESIGN:` = `gears/system/usage-collector/docs/{PRD,DESIGN}.md`.
> Verdict `UC-extends` = the usage-collector gear must expose/extend something (phase-2 scope).

| # | Sev | Verdict | Seam |
|---|-----|---------|------|
| **UC1** | CRIT | **UC-extends (metering phase 2)** | **Transport model: designed-for stream vs built pull.** Slice 12's `IntakeConsumer` presumes a durable, at-least-once, per-partition-ordered transport with offsets + source replay (design/12 §3.3/§3.8/§4.3). Collector v1 has **no emission surface**: event architecture "Not applicable in v1"; near-real-time consumers poll a Query SPI that is eventually consistent **with no upper bound**, read-after-write only via the ingestion ack (UC-DESIGN §3.12.3 + §3.10). A poll bridge is not a substitute: the raw query has **no accepted-order cursor** (event-time filters miss late-accepted records behind the cursor), no freshness bound, per-plugin consistency ceilings, and deactivations are invisible to it (UC5) — a naive poller silently misses records and retractions → wrong charges. → collector grows a **transactional-outbox emission surface** (**accepted entries** — covering usage *and* compensation, feed PRD `fr-usage-event-feed` — and **status transition entries** incl. the depth-1 cascaded compensation flips, feed PRD `fr-feed-status-entries`), ordered per `tenant_id` partition, at-least-once, idempotency-tuple-keyed. The hook is already reserved as **additive within REST v1 / SDK v1** (ADR-0006, UC-DESIGN §3.12.3), and the collector's deferred-items table names exactly this trigger ("re-evaluate when downstream consumers require coordinated historical re-emission"). |
| **UC2** | HIGH | Joint | **Completeness / watermark signal for period close.** Rating stays *correct* without one — open-period late arrival re-resolves from the pinned snapshot, posted-period is delta-only (design/08) — but with no per-`(tenant, UsageType)` ingestion watermark every period close is maximally provisional and the correction-cascade / `Adjustment` volume grows with lateness. Collector v1 explicitly defers watermark/reconciliation metadata (UC-PRD §4 watermark out-of-scope bullet; UC-DESIGN §3.12.2 deferred-items table). → phase 2 adds an accepted high-watermark per `(tenant, UsageType)` (API + periodic watermark event); rating consumes it as a **soft** close gate (grace window before the period tick finalizes usage lines) — never a correctness dependency. |
| **UC3** | HIGH | Joint | **Attribution join: `(tenant, UsageType, resource_ref, subject_ref)` → `(subscription, meter, dimensionKey)`.** The collector deliberately knows none of the right-hand side. Slice 12's `Normalizer` presumes a frozen subscription/meter linkage (design/12 §4.1). Three pins needed: **(a) meter binding** — the catalog-declared meter carries a `usageTypeRef` (collector `gts_id`) as the authoritative binding, validated at publish (registry/pricing side; adjacent to RG2's metering-unit declaration); **(b) subscription resolution** — `resource_ref` → subscription via the Subscriptions entitlement/resource linkage (S1/SUB-R1 frozen context); an ambiguous or unresolvable `resource_ref` quarantines, never guesses (design/12 §4.5); **(c) dimension carrier** — `dimensionKey` values ride the UsageType's declared closed-shape, string-only `metadata_fields`; the meter's declared dimension set must equal the declared metadata keys (publish-time cross-validation — the G1 registered-validator pattern). (c) closes the §17.3 emission-shape half of **M6**. **Owing-side adoption landed 2026-07-28** (cross-gear review): (a)+(c) are now authored where they are owed — products `fr-metering-unit-declaration` requires `usageTypeRef` on the declaration and validates the dimension-set/`metadata_fields` equality at registry publish, and pricing `inst-cmp-usagetype` (design/02) blocks plan publish on an unbound meter (`METER_USAGE_TYPE_UNBOUND`) or a priced dimension outside the declared set (`METER_DIMENSION_UNDECLARED`); both flagged for veto. (b) subscription resolution remains open with Subscriptions. |
| **UC4** | MED | Joint | **Dedup-key derivation across the boundary.** Slice-12 `usageKey = (sourceSystem, meterId, sourceEventId)` (design/12 §4.3); collector identity is `(tenant_id, gts_id, idempotency_key)` — unique per tenant **per UsageType**, unbounded window, exact-equality retry absorbed / divergent reuse rejected as conflict (UC-PRD fr-idempotency). Map: `sourceSystem = usage-collector`; `meterId` ← the UC3(a) binding; `sourceEventId` ← the full `(tenant_id, gts_id, idempotency_key)` tuple — the *contractual* identity (plugin record ids are backend-scoped). Key reuse across UsageTypes is legitimate collector-side, so `sourceEventId` MUST embed the `gts_id` scope. Both dedup windows unbounded — aligned. |
| **UC5** | MED | Joint | **Corrections mapping + visibility.** Semantics map cleanly: a collector **compensation** (counter-only, strictly-negative, references the original active entry) → a correcting `UsageRecord` with `corrects` lineage (design/12 §4.4); an **event deactivation** (whole-row one-way `active→inactive`, depth-1 cascade flipping its active compensations) → a full-reversal correction of the record's prior contribution to `Q`. But rating must *observe* both: compensation rides the collector's ingestion path (UC1's **accepted entries** cover it); **deactivation is an operator REST write with no ingestion event** — the UC1 surface MUST emit a **status transition entry** per deactivation (incl. its depth-1 cascaded compensation flips — feed PRD `fr-feed-status-entries`), else retractions are silently missed. Rating side unchanged: the mapped correction re-materializes `Q` (design/13 §4.4) and re-resolves under `periodState` rules (design/08 §4.3). |
| **UC6** | MED | Joint | **Temporal shape: point-stamped values vs intervals.** The collector v1 record is `(value, created_at)` under `counter`/`gauge` kinds — **no interval kind**; rating design/12 §3.1 expects "quantity **or start/stop interval**" (the `SessionMerger` overlap-union machinery presupposes `[start, stop)` geometry). Bridge today: duration consumption is emitted as **chunked counter deltas** (chunk cadence ≤ the finest attribution boundary — splits land on day granularity, practical guidance: hourly), which reduces slice-12 session merge to summation; `gauge` **is rateable since D-44/T-D-17** (M10 re-scoped 2026-07-16): level meters declare `aggregationFunction ∈ {peak, time_weighted}` and rating folds samples per granule — sources emit raw levels, never pre-folded values. The event never carries a billing period — windowing is rating's (`WindowResolver` from event time under the frozen anchor), and sources MUST NOT round (`billingGranularity` round-up is the core's, after merge). → a native **interval/duration kind** is a phase-2+ collector increment candidate (additive; the feed entry shape of `fr-usage-event-feed` carries it untouched); until then the chunked-delta contract + granularity rule is the pinned bridge. |

Aligned (no action needed): mandatory client idempotency keys + **unbounded** dedup window +
fail-closed ingestion + immutable rows + append-only corrections + "no business logic in the
collector" all match slice 12's mediation doctrine (normalize-once/freeze-forever, authoritative
dedup, quarantine-never-guess); NFR envelopes compose (collector sustains ≥ 10k rec/s ≈ 864M/day
platform-wide vs rating's ≥ 10M ev/day/region intake).

---

## K. Subscriptions — recurring period cut (2026-07-28)

> Cross-gear review finding, 2026-07-28. The subscriptions register tracks the same open as its
> **SUB-R6** ("recurring pricing enrichment (SUB-D-07): rating counterpart contract + joint
> fixture"); this is the rating-side entry so the conflict cannot be missed here.

| # | Sev | Verdict | Seam |
|---|-----|---------|------|
| **SB1** | CRIT | **RESOLVED 2026-08-01 (T-D-33/T-D-34; joint fixtures owed)** | **Recurring period-cut ownership conflict.** Subscriptions' `RecurringEmitter` cuts the **money-free recurring period fact** idempotent per `(subscriptionId, billing period, lineKey)` (per component since SUB-D-19) and mandates that "the priced line inherits the fact's idempotency key" — "this gear owns the period cut (anchor, pauses, pending intents, the key) and never the amount" (subscriptions DECISIONS SUB-D-07, 2026-07-15, amended 2026-07-28, *flagged for veto*; subscriptions `design/08` §4.3). Rating's slice-14 `PeriodTick` self-derives the same recurring lines at `AnchorPeriod` boundaries from the frozen `billingAnchorPolicy` via its own `AnchorCalendar`, idempotent per `(subscription, AnchorPeriod)` (T-D-15; `design/14` §4.2) — **two owners of the recurring WHEN, two idempotency keys, no consumption contract between them**: double-emission or missed-pause/suspension risk on every recurring line. → resolve ownership + land the joint fixture **before either gear implements `billingAnchorPolicy`**. If rating adopts the fact, it must also absorb the 2026-07-28 fact amendments: **`payerTenantId` snapshotted at cut time**, the **suspended interval(s)** overlapping the period, and the **`pause_recurring \| continue` suspension-billing posture** (rating currently models none of them). **Key alignment landed on the subscriptions side 2026-07-28 (SUB-D-19, flagged for veto):** the fact is now cut **per billable component**, keyed `(subscriptionId, billing period, lineKey)` — previously one fact per subscription-period with a singular traceability tuple, which could not represent plan + add-ons and could not be inherited by this gear's per-component period-driven units. The `lineKey` coordinate is deliberately the one this gear already carries, so the two keys now differ only in the period coordinate (`billing period` vs `AnchorPeriod`) — that residue is exactly what the ownership resolution must settle. **Resolution (T-D-33/T-D-34, 2026-08-01; the subscriptions side SUB-D-07/19/20/21 ratified in its veto round the same day):** the subscriptions fact IS the recurring WHEN — the `PeriodTick` is fact-driven, `AnchorPeriod` ≡ the fact's period identity, the fact's carried fields (suspended intervals + posture, period-start `payerTenantId`, pause marker) are absorbed, the `lineKey` value rule is adopted verbatim, and the `AnchorCalendar` survives as geometry + watchdog only. Owed: the joint K5 anchor fixture (calendar geometry ≡ fact identity; subscriptions executes the D-20 clamp — its SUB-D-27) and the SUB-R6 `lineKey` fixture cases. |

---

## Ownership matrix (contested/adjacent responsibilities)

| Responsibility | Owner | Seam |
|---|---|---|
| Canonical scope key (8 axes) | **Pricing** (SoR); Tariffs adopts | K1–K5 |
| cohort/generation selection | Split: pricing publishes generations, Tariffs selects by pinned cohort | K2 |
| PriceOverlay authoring | **Pricing** | O1–O3 |
| Overlay *evaluation* (markup) | **Tariffs** (step 4/5) | O3 |
| PriceWindow store / state machine / events | **Pricing** (Slice 7, D-03) | W0–W2 |
| `pricingSnapshotRef` composition SoR | **Tariffs**; pricing pre-stamps catalog subset; Subscriptions freezes binding | S1 |
| Model-kind → formula mapping | **Pricing** (§17.2 SoR); Tariffs evaluates | M1 |
| Windowed `Q` aggregation | **Rating** (single-writer); Tariffs consumes frozen `Q` | M7 |
| Raw usage SoR (measurement) | **Usage Collector** (built v1); Rating normalizes into its own `usage_record` (mediation) | UC1 |
| Usage → Rating transport | **Joint — unpinned**; proposed: collector phase-2 emission surface (outbox stream) | UC1, UC5 |
| UsageType ↔ meter binding + dimension carrier | **Registry + Pricing** (pinned 2026-07-28): products `fr-metering-unit-declaration` requires `usageTypeRef` on the meter and validates dimension-set ≡ `metadata_fields` at registry publish; pricing `inst-cmp-usagetype` blocks plan publish on an unbound meter or an undeclared priced dimension. Subscription resolution (UC3 b) stays open with Subscriptions | UC3, M6, RG2 |
| Commitment pools (drawdown/overage) | **Contracts** SoR; Tariffs evaluates step 6 | M8 |
| Prepaid credit wallet grant | **Pricing** authors, **Billing/Rating** executes | M8 |
| Reserved rate: self-service | **Pricing** (catalog snapshot) | M9 |
| Reserved rate: negotiated RI | **Contracts** (step-5 overlay) | M9 |
| `prorationBasis` / `billingAnchorPolicy` enums | **Pricing** (canonical); Tariffs adopts verbatim | P1, P2 |
| Proration/plan-change *math* | **Tariffs** evaluates; **Subscriptions** executes | 6.11 |
| FX math (step 8) | **Tariffs/PLAL** | aligned |
| Tax calc / display basis | **Tax Engine** (calc) / **Pricing** (display basis, D-01) | aligned |
| Two-person publish workflow | **Pricing** Slice 5 (single engine); Tariffs registers validators | G1 |
| Audit trail / retention | **Pricing** (`pricing_audit_log`, D-14) | aligned |
| Rev-share normalization (publish) | **Pricing** (D-07) | B1 |
| Bundle component summing (eval) | **Tariffs** | B1 |
| Floor/cap execution + rounding | **Billing** | aligned |
| ASC 606 recognition | **Finance/Billing** (Tariffs emits refs, null@MVP) | ASC |

---

## Governance topology note (G1 — why ledger is not the merge target)

Three maker-checker engines exist in adjacent bounded contexts; they share the *pattern*, not
the *policy*:
- **Ledger** — `dual_control_policy` + **Finance Approver**; governs financial **postings** (JE,
  refund > threshold, backdating, period reopen, suspense clearing, GL write-off). Materiality =
  amount/entity thresholds + backdating days. Subject = posted journal entries.
- **Pricing Slice 5** — `MaterialityEvaluator` + `ApprovalWorkflow` + a **separate** `approval_policy`
  resource; **FinanceReviewer** approver; governs catalog **publish** (price/plan deltas +
  always-material triggers). Pricing already cites the ledger `dual_control_policy` as **precedent**
  (pricing design/05 policy-object schema) and deliberately keeps its own policy resource.
- **Tariffs** — folds into pricing Slice 5 (same subject: catalog publish).

Merging catalog-publish governance into the ledger is a category error — different subject
(commercial config vs financial posting), approver role, and materiality; a price change becomes a
posting only later via Rating→Billing→Ledger. No shared platform governance library exists today
(`gears/bss/libs/` holds only `coord`). **Optional post-launch:** extract a shared maker-checker/audit
mechanism into `libs/` (à la `libs/coord`) that pricing + tariffs + ledger instantiate, each with its
own policy — DRY the mechanism, not the policy. Not a launch blocker.

## Decisions register (to close before Design lock)

**Resolved this session (2026-07-10):**
- **M3** — `package` pricing: **Tariffs adds a package FR** (parity with pricing p2 launch).
- **M5** — composite/derived meter: **in launch**; Tariffs adds a composite-formula eval FR.
- **G1** — governance: **single pricing Slice 5 engine**; Tariffs registers validators; ledger separate.
- **O3** — step-4 overlay composition: **stack all survivors**; class order = tie-break, not exclusivity.
- **W2** — open-period re-resolution: **Tariffs replays the pinned snapshot** (no pricing read-model change).
- **S1** — canonical `pricingSnapshotRef` field list fixed, per-segment writer ownership (Tariffs = composition SoR).
- **M7** — canonical tier-counter `Q` key = `(subscription, meter, dimensionKey, window)`.

**Resolved 2026-07-11 (Design pass, tri-review closures — T-D-09…T-D-15 in [`DECISIONS.md`](./DECISIONS.md)):**
- **S1 residue** — eighth `commitmentReservation` snapshot segment (T-D-09).
- **Balance write-back / sequencing** — Rating publishes `CommitmentBalanceEffect`s; Contracts serializes per-pool `balanceVersion`; balance-affecting corrections cascade delta-only re-resolution of later units (T-D-10; cross-PRD obligation on Contracts/Rating).
- **Delta-dedup owner** — Rating (T-D-11).
- **Windowed-model continuity across intra-window boundaries** — per-slice sub-window units + frozen `bandOffsetQ`, adopting pricing `inst-tb-window-continuity`; correction key gains the slice coordinate (T-D-12).
- **Step-6 recompute** — steps 3–5 re-run as a unit over the reservation remainder; tier-counter exclusion + pool asymmetry registered in Tariffs PRD §6.6 / AC 19 (T-D-13).
- **Commitment pool flavors + true-up formulas** — frozen `poolType ∈ {prepaid_drawdown, committed_rate}`; sale bills outside PLAL (T-D-14).
- **Period-driven units** — Rating's period tick synthesizes recurring/capacity/true-up units at `AnchorPeriod` boundaries (T-D-15).

**Product / commercial calls (owner sign-off):**
- **M6** — dimensional pricing launch posture (empty-tuple at launch either way; wording + declaration ownership). **RESOLVED 2026-07-28**: declaration + freeze in-scope now (the catalog persists `dimension_key`, Rating freezes the declared set in the snapshot), value-pricing OSS-gated — joint wording landed both sides (pricing `design/03` §6 `dimension_key` note + rating `design/03` §4.2); see the M6 row above. *(This list had kept M6 open after the row closed — corrected in the 2026-07-28 cross-gear review.)*
- **T-D-22 — plan-scoped coupon evaluation base** — **still open**: `applyScope = line_total` (and T-D-20's cross-scope exclusivity) presume a point holding every line of a plan for the period at once; step 7 is per-line and all evaluation units are sub-plan, so no such point exists. Launch posture: `line_total` fails closed; the assembly point + trigger + cascade behaviour pin jointly with Promotions ([`DECISIONS.md`](./DECISIONS.md) T-D-22).

**Technical residues — still open (from the 2026-07-11 slice review):**
- **S1 segment naming** — **RESOLVED 2026-07-11 (T-D-09)**: the step-6 frozen identifiers form the eighth named segment `commitmentReservation` (writer: Tariffs @ eval; no pricing-side change), recorded identically in tariffs `design/01` §4.3, `design/05` §4.1, and `design/11` §4.7.
- **Composite × dimensions input-join rule** — pin with pricing before dimensional values and composite meters co-occur (tariffs `design/03` §3.6).
- **Mixed-settlement coupon rules** — `exclusive_best` cross-currency benefit comparison and mixed-settlement `ordered_stack` folding: both fail-closed until pinned with Promotions (tariffs `design/06` §4.2).
- **Seat-change boundary transport** — Subscriptions-driven change boundary vs Subscriptions-side proration for mid-period `subscription_seat_count` changes (tariffs `design/09` §4.3; default: change-boundary).
- **Bundle-level coupon attachment** — bundle-total coupon scope vs component-line attachment (tariffs `design/10` §4.5; until pinned: component lines only).
- **Usage-Collector ingestion bridge (2026-07-16)** — UC1–UC6 (§J): emission surface / transport (UC1, gates pipeline slice-12 implementation), completeness watermark (UC2), attribution join incl. dimension carrier (UC3, closes the M6 emission-shape half), dedup-key derivation (UC4), correction visibility (UC5), temporal shape / interval kind (UC6: chunked-delta bridge now, native interval kind phase-2+; **gauge rateable since D-44/T-D-17** — the granule fold; this summary previously said "not rateable", contradicting the re-scoped UC6 body, fixed 2026-07-28) — **to be** pinned jointly with the usage-collector gear (the transport itself stays **unpinned** per the ownership matrix until the feed is adopted — wording reconciled 2026-07-28); the remedy this PRD commits to (raised 2026-07-16) is a **Usage Event Feed + ingestion watermarks**, **not yet authored in the usage-collector design set** (it executes the collector's reserved DESIGN §4 hooks, additive under its ADR-0006; closes UC1/UC2/UC5 and the UC4 key-derivation rule on adoption; UC3 attribution join stays with registry/pricing).
- **Recurring period-cut ownership (2026-07-28) — RESOLVED 2026-08-01** — SB1 (§K): the subscriptions fact per `(subscriptionId, billing period, lineKey)` is the recurring WHEN; the `PeriodTick` consumes it (T-D-33) and the `lineKey` value rule is adopted (T-D-34); joint K5 anchor + SUB-R6 lineKey fixtures owed before `billingAnchorPolicy` code.
- **Trailing-tier qualification counterpart (pricing D-40)** — M12 (§E): rating-side trailing aggregate / period-boundary re-qualification / locked-rate pin unauthored; blocks any `trailing_period` row publish until adopted.

**Propagation status:** all Tariffs-adopts items below + the resolved decisions (M1–M5, O1–O3, K1–K5, P1–P2, W1, F1, S1, M7, M9–M11, B1, C1, N1, M6) are **propagated into `PRD.md`** (2026-07-10, 5 assert-anchored batches; `cfs validate` + `validate-toc` green). A 4-agent fresh-eye consistency pass then closed the propagation residues (stale ACs 2/6/14, §2.2 key, §6.3 scope-mapping, governance FR/NFR/AC self-workflow, §17.3/17.4 composite-Follow-on, model-coverage lists, gear name) and added joint-fixture ACs 5a/5b/5c for per_unit / package / composite. The pricing-side confirmations Design lock was waiting on — O3 (tie-break-not-exclusivity) and the M6 joint wording — both landed 2026-07-28 (rows above). **Pre-existing upstream note — resolved 2026-07-28:** the vendored-PRD numbering drift (§5.1 AC cross-references citing AC 21/22/23 etc. against §12's max of 20) is fixed in the PRD — the §5 scope table and FR rationales now cite the actual §12 numbers, and the slice 05/06 traceability sections were updated to match.

**Tariffs-adopts (mechanical conformance — no product/pricing decision):**
- K1, K2, K3, K4, K5, O1, O2, W1, M1, M2 (launch-blocking), M4 (delete), M9, M10, M11, P1 (live CI gate), P2, F1, B1, C1, N1
