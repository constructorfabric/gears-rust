<!-- CONFLUENCE_TITLE: [BSS]: Subscriptions — Cross-Gear Seam Map -->
<!-- Related: ./PRD.md, ./DESIGN.md, ./DECISIONS.md | Owners: BSS Subscriptions team -->

# Subscriptions — Cross-Gear Seam Map

> Cross-gear seam analysis produced before the Subscriptions design, against the mature
> **pricing** and **rating** design sets (`gears/bss/pricing/docs/`, `gears/bss/rating/docs/`)
> and the subscription lifecycle PRD ([`PRD.md`](./PRD.md)). Subscriptions is the **System of
> Record** for the subscription commercial aggregate: the lifecycle state machine, effective-dated
> composition (`PlanLink`/`AddOn`), the change **boundary/mode**, renewal execution, entitlement
> assignment + point-of-use check state, and multi-tenant ownership. It is **not** an authoring
> catalog and it computes **no** money — pricing owns the catalog, rating owns evaluation and
> proration math, Billing owns posting.
>
> Unlike the rating⇄pricing map (a single 1:1 complementarity), the subscription aggregate sits at
> the centre of the BSS lifecycle and its seams fan out to **many** neighbours. This map is
> therefore organised **by neighbour** (A–I), each seam carrying a stable `SUB-<letter><n>` id.
>
> **Verdict legend:** `SUB-authors` = Subscriptions is the SoR the neighbour consumes (no
> Subscriptions change beyond exposing the contract); `SUB-adopts` = Subscriptions adopts a
> neighbour-side fact verbatim (no neighbour change); `Joint` = a shared contract needs a
> co-decision; `Neighbour-extends` = a neighbour must expose/extend something; `Product` = a
> launch-scope / commercial call; `ALIGNED` = the counterpart contract is already written on the
> other side, no action beyond citing it.
>
> Severity: `CRIT` (breaks lifecycle/billing correctness), `HIGH`, `MED`, `LOW`. Line refs: `S:` =
> this gear's [`PRD.md`](./PRD.md); neighbour refs carry their path. The autonomous decisions
> `SUB-D-01…27` live in [`DECISIONS.md`](./DECISIONS.md) (01…12: 2026-07-15 wave; 13…15:
> 2026-07-28 review fixes; 16…19: 2026-07-28 billing pass; 20…26: 2026-08-01 wave-3 fixes;
> 27: 2026-08-01 SB1-resolution round).

---

## A. Rating (evaluation core + pipeline)

> The rating gear consumes subscription composition and the change boundary, and owns all
> commercial math. The counterpart contract is **already written on the rating side**
> (`gears/bss/rating/docs/PRD.md` §9.2 "Subscriptions input contract"; proration split in rating
> §6.11 / design slice `09-period-plan-change`). Ordering shares one key
> `(resourceTenantId, subscriptionId)` (S:803, rating `11-consumer-contracts`).

| # | Sev | Verdict | Seam |
|---|-----|---------|------|
| **SUB-R1** | CRIT | **Joint (field alignment open; was ALIGNED)** | **Composition read-model + change boundary.** Subscriptions exposes effective `PlanLink`/`AddOn` intervals, `PlanTier` @ `t`, the **active plan phase** @ `t`, the plan-change `(changeEffectiveAt, changeMode)`, the **committed seat quantity @ `t`** (effective-dated, SUB-D-02), the **`priceEligibility` inputs** (`activatedAt`, bound `cohort`), and the per-sale `brandId` context; rating consumes them and slices usage + prorates at the same boundary (S:1034; rating PRD §9.2, rating `design/09`). **Downgraded from ALIGNED (2026-07-15 review):** the rating counterpart names the seat count and `priceEligibility` inputs that this side's contract omitted — the two field lists MUST be mirror-reconciled before Design lock. Ordering rides the **pinned `orderingTenantId`** (immutable across transfers — SUB-D-06). **WHEN vs MATH split is binding**: Subscriptions owns the boundary/mode, rating owns proration day-count and tier-`Q`/commitment carry-vs-reset (S:590). No Subscriptions-side math. Design implements the exposure surface (slice `09-consumer-contracts`). |
| **SUB-R2** | MED | **ALIGNED (SUB-authors)** | **Snapshot `(currency, region)` segment.** Subscriptions freezes the `(currency, region)` binding at activation into the composite `pricingSnapshotRef`; rating is the composition SoR that seals the ref (S:532; rating SEAMS **S1**). **Open to confirm at design:** after SUB-D-02/05, the **seat-count provenance** (SUB-R3) and the **activation date-trio** (SUB-C4) are **not** new snapshot segments — they ride events/read-models, not the pinned pricing ref. Design must state this explicitly so no fourth Subscriptions segment silently appears. |
| **SUB-R3** | HIGH | **Joint** | **Seat-count provenance + mid-period seat boundary.** Pricing `quantitySource = subscription_seat_count` (D-18) makes this gear the seat supplier; the count rating reads MUST originate **only from committed `updateQuantity` transitions** (SUB-D-02, S:620) — never an untyped attribute edit — and is stored **effective-dated** so `quantity @ t` resolves for replay (2026-07-15 review fix; a single mutable value cannot serve the replay contract). **Open (mirrors rating's "seat-change boundary transport", rating `design/09` §4.3):** a mid-period seat change is transported as a **Subscriptions-driven change boundary** (default) that rating prorates — *not* Subscriptions-side proration. Pin the default with rating at design; increases MAY be immediate, decreases default `next-cycle` (SUB-D-02). |
| **SUB-R4** | HIGH | **Joint** | **Phase boundary = change boundary.** `convertTrial` / scheduled trial conversion advance the plan **phase** boundary; rating consumes that instant like any `changeEffectiveAt` (S:919). Phase axis is a `phase_id` (pricing D-19); Subscriptions is the **phase-structure SoR** and resolves the active phase @ `t` for rating (S:460). Confirm with rating that the phase-boundary instant travels on the same `(changeEffectiveAt, changeMode)` channel as a plan change (no second boundary vocabulary). A **trial extension** moves the phase boundary and MUST emit the boundary-move on this same channel (slice 06). |
| **SUB-R5** | MED | **Joint (discrepancy)** | **Brand context source.** This PRD publishes the **per-sale `brandId`** as a Subscriptions attribute in the evaluation context (S: §6.2, AC 20); the rating PRD's step-4 overlay scope matches `brand` against **Plan/SKU `brandId` @ `t`** (rating PRD §6 step 4, §17.4) — two different sources. Pin with rating which one feeds brand-scoped overlay matching (per-sale storefront attribution only this gear can supply vs catalog-declared membership); AC 20 is not implementable while they disagree. Tracked in PRD §15. |
| **SUB-R6** | HIGH | **Joint (SUB-D-07)** | **Recurring pricing enrichment.** Subscriptions cuts the **money-free recurring period fact** — one **per billable component**, key `(subscriptionId, billing period, lineKey)` (SUB-D-19, 2026-07-28: `lineKey` is the same coordinate rating carries in its period-driven unit key `(subscription, priceId, chargeKind, lineKey, AnchorPeriod)`, which is what makes the inheritance rule below well-defined for a plan-plus-add-ons subscription; the rating counterpart adopted it 2026-08-01, rating T-D-34 — joint fixture owed), traceability tuple, `pricingSnapshotRef`, pause/intent posture); **rating prices** the recurring component from the frozen snapshot (flat / per-unit × quantity / hybrid recurring line) and the priced line **inherits the fact's key** before Billing posts (SUB-D-07, S: §6.8, AC 27). Removes the double-producer collision with rating's step-9 recurring lines. Needs the rating counterpart contract + joint fixture before Design lock. |

## B. Pricing (Product Catalog)

> Pricing is the authoring SoR for `Plan`/`Price`/`PriceWindow`/`PriceOverlay`/`CatalogVersion` and
> publish governance; Subscriptions resolves published catalog keys and adopts its consumer
> contracts. Most seams are **adopt-verbatim** against the frozen pricing consumer contract
> (`pricing/docs/design/06-consumer-contracts.md`).

| # | Sev | Verdict | Seam |
|---|-----|---------|------|
| **SUB-P1** | HIGH | **SUB-adopts (amended 2026-07-31, pricing D-93)** | **Plan-change classification contract.** Pricing publishes `allowedChangeTargets` / `comparabilityRank` in the frozen consumer contract (pricing `design/06`); **the boundary class (`in_place` \| `cancel_plus_new`) is no longer a published stamp — this gear computes it at change time** from both plans' published market/frequency facts at its pinned version (pricing **D-93**: a publish-time stamp could not be re-computed under pricing's frozen per-subject read model; the rule — target covers the subscription's frozen `(currency, region)` with matching frequency ⇒ `in_place`, else `cancel_plus_new` — is adopted verbatim, the same read-time discipline as `comparabilityRank`). **Subscriptions classifies** an upgrade/downgrade/cross and enforces the boundary — it does not re-derive comparability. **Cross-currency / cross-region / cross-frequency = cancel+new**, not an in-place change (pricing §15 cross-boundary sign-off; S:610 overlap); credit forfeiture disclosed before execution. |
| **SUB-P2** | HIGH | **SUB-adopts** | **Phase → grant-set map.** Entitlement assignment reads the plan's **published grant set**, including the **per-phase map** where the plan is phased (pricing **D-41**; S:867). `convertsToPhaseId` drives end-of-trial conversion (S:909). Catalog authors the templates; this gear resolves + materialises per subscription. Adopt the phase/grant contract; author nothing catalog-side. |
| **SUB-P3** | MED | **OPEN (2026-07-28 cross-gear review — was ALIGNED)** | **Trial sellable definition.** Catalog is authoritative for the trial offer — trial plan/SKU or a leading trial **phase** (S:470, S:899). Subscriptions persists **evaluated** trial state (attributes + `PlanLink`/snapshot pointers), never a `trial` status (S:458). **Promotional-window trial offers are deferred**: pricing defines no promotional `PriceWindow` kind (a `PriceWindow` only schedules *when* a row is effective — pricing ADR-0003 / `design/07`), and the Promotions PRD does not yet exist (pricing `DESIGN.md` §Out of scope). The third enumeration member was removed here and in PRD §6.10 / `design/06-trials.md`; re-open if Promotions lands a discount-bearing offer primitive. Attribute/event naming closes at design. |
| **SUB-P4** | MED | **Product (deferred)** | **Prepaid credit grant.** The prepaid credit **definition** is pricing **D-43**; the balance/drawdown is **Billing/Rating**, GA-gated (S:182). Subscriptions keeps **subscription-side hooks only** — it neither defines nor draws down the wallet. See the Billing-facing drawdown line **SUB-B4**. No launch dependency for the core lifecycle. |
| **SUB-P5** | MED | **SUB-adopts** | **Sellability / publish gate.** `PlanLink`/`AddOn` resolve only against **published** plans that pass the pricing sellability gate (pricing `design/07`; S:150). A draft or `not_sellable_ga` plan MUST fail the `create`/`changePlan` guard fail-closed. Adopt the gate as a precondition; the overlap **key** itself is registry-owned (**SUB-G1**). **Pricing D-80 (2026-07-30) extends predicate (1)** with a coverage horizon — the key's active-plus-scheduled coverage must reach `now + the longest billing cycle sold on the key` (pricing exposes the per-key coverage end on the surface; the predicate count stays six) — the adopted gate includes it. **Pricing D-94 (2026-07-31) pins the gate's granularity**: the adopted check is the **conjunction over every scope key the purchase binds** on the bound `(currency, region)` — chargeKind components and the phase chain alike, eligibility-resolved, grandfathered generations excluded; one failing component key blocks the plan-market (never a partial sale). → **Obligation here:** the `create`/`changePlan` guard evaluates the full conjunction, not a single key — **carried into the design set 2026-08-01** (wave-3 review #17: this row had been the only carrier): slice 01 §4.5 sellability-gate bullet + PRD §6.1 gate FR. |
| **SUB-P6** | HIGH | **Joint (added 2026-07-28, billing-pass review #2)** | **Grandfathering eligibility expiry — inbound signal + outbound feedback.** Pricing publishes **`EligibilityExpirySignal`** ("a generation's `grandfatherUntil` passed — Subscriptions re-binds at next renewal", pricing `design/07`) and alarms on expired-but-still-bound rows **reported back by this gear** past one renewal cycle. Both halves are obligations here: (inbound) the `RenewalJob` consults the signal per renewal — it is what gates the SUB-D-14 re-bind away from a pinned generation (slice 04 §4.3); (outbound) after each renewal of a bound-to-expired-generation subscription that could **not** re-bind (firing failure, held term), this gear reports the still-bound state back on the same read-model feedback surface pricing's backlog alarm consumes (pricing's "two-way Subscriptions publish contract"). **Two additions 2026-08-01 (wave-3 review #23 / SUB-D-20):** (i) the SUB-D-17 notice lookahead reads the **scheduled `PriceWindow` supersessions + the bound generation's `grandfatherUntil` date** against the renewal instant — the read-time `EligibilityExpirySignal` is true only at/after expiry and cannot arm a 30-day-ahead notice; (ii) the read surface must answer **as-of-instant reads up to the dwell bound (90d default) in the past** — the §4.3b revival re-resolves eligibility as of the backdated term start (a read pattern over pricing's immutable published rows, not new state). Consumed via the pricing read model + events; contract fields close at design freeze with pricing (slice 09 §4.2 lists them). |
| **SUB-P7** | HIGH | **Joint (added 2026-07-29, cross-gear review; pricing D-65)** | **Plan-migration execution handshake — this gear executes, and until now no seam recorded it.** Scope boundaries say only "pricing lifecycle slice authors migration; Subscriptions executes `PlanMigrationScheduled`" (§ scope table), and this register carried **zero** migration rows — while pricing's migration state machine names this gear as the actor of **both** non-terminal transitions and hangs D-34 (in-flight cancel), D-36 (execution-time lock/boundary re-validation) and D-38 (T-ε race closure) on the handshake. Pricing has now authored the surface (`design/11` §5): before the first `PlanLink` batch this gear **MUST** call `POST /v1/pricing/migrations/{id}:start`, which flips `scheduled → in_progress`, re-resolves contract locks + boundary deltas against fresh state, and **returns the exclusion set this gear MUST honour** (freshly locked subscriptions are never migrated — `dod-contract-lock`: "reported, never broken"); this gear then re-reads schedule state **per processing batch** (so an operator's in-flight cancel is observed mid-run), honours the `(migration_id, subscription)` dedup key (M2 idempotency), and closes with `POST /v1/pricing/migrations/{id}:complete` carrying the processed / excluded / failed sets. **`:start` retries replay, never recompute** (pricing D-65, sharpened 2026-07-31): pricing persists the first call's exclusion set per `migration_id` and returns that stored snapshot verbatim on every retry — this gear consumes the persisted snapshot and MUST NOT re-accept a recomputed result (a recompute could differ from the set already honoured mid-run). → **Obligation here:** author the executor side (start call, per-batch state re-read, dedup key, completion report) in the plan-change slice before design freeze; pricing's D-36/D-38 text is the normative source. |
| **SUB-P8** | HIGH | **Joint (added 2026-07-30, pricing slice review; pricing D-79)** | **Per-scope-key in-flight-subscription presence — pricing §9.2 inbound lane 3.** Pricing's retirement rule (D-51: keep the continuing-coverage window of any key with in-flight subscribers), the D-62 window-cancel/shorten exemption (narrowed by D-80), its materiality routing, and the retirement story's "view the active subscription count" all evaluate a per-scope-key in-flight-subscriber predicate — and pricing owns no subscription data, so **this gear is the data source**. Contract: pricing submits a price-id set (the canonical scope key's rows — the pinned price id is what this gear holds via `pricingSnapshotRef`); this gear returns the count of **non-terminal** subscriptions whose pinned snapshot references any of them. Pricing re-resolves inside its mutating commit and fails closed on outage (subscribers presumed present — windows kept, approval required), so availability of this lane bounds pricing's retirement/cancel UX, never its safety. → **Obligation here:** author the presence read (indexed by pinned price id, non-terminal states enumerated) before design freeze; pricing `design/07` `inst-fg-trailing` / `design/11` `inst-rt-cancel` are the consuming sites. **Authored 2026-08-01** (wave-3 review #18): slice 09 §4.1 presence read + PRD §9.1 row. |
| **SUB-P9** | MED | **SUB-adopts / Joint (added 2026-08-01, wave-3 review #20; gated on SUB-B1/SB1)** | **`billingAnchorPolicy` + the month-end clamp + the joint anchor fixture.** Pricing requires every recurring row to publish `billingAnchorPolicy` (K2 enum, no-drift month-end clamp — pricing `design/06`; D-20: "confirm with Subscriptions (they execute the math)", the K5 joint proration/anchor fixture "exists before code"); rating asserts "a plan change that alters `billingAnchorPolicy` takes effect from the next period boundary" in the same fixture (rating `design/09`). This gear adopts the sibling field the way it adopted `prorationBasis` (verbatim, glossary row, drift gate — PRD §6.2) **once SB1 resolves the recurring-WHEN owner** (SUB-B1): if it resolves this gear's way, the clamp execution and the anchor-move-at-boundary rule land in slices 08/03 and the fixture row becomes a design-freeze gate. Until then the adoption obligation is parked **here, tracked**, not silently absent (the wave-3 review found `billingAnchorPolicy` in one ADR driver bullet and one vendor-gap row only, with `billingAnchor` attributed to Contract terms — slice 01 §3.7). |

## C. Contracts & Agreements

> Contracts is the SoR for signed terms, renewal, grace, regional templates, ramps, commitment
> pools, and booking dates. **Risk:** the upstream Contracts PRD does **not yet author** several of
> these (S:1325, §16) — until it does, the platform defaults in the Subscriptions PRD govern and the
> obligation is tracked as a cross-PRD follow-up.

| # | Sev | Verdict | Seam |
|---|-----|---------|------|
| **SUB-C1** | HIGH | **Joint (RISK — upstream unauthored)** | **Renewal / grace ladder / regional templates.** §6.5 assumes Contracts is the SoR for `Renewal` (`autoRenew`, term windows, notice), grace length/ladder, and regional templates (S:674, S:718). Upstream Contracts PRD has not authored them (S:1360). **Until authored:** the platform defaults govern — **7-day grace**, **30/14/7/1 notices**, hybrid exit trigger (S:722–725). Subscriptions stores **evaluated fields** at renewal-evaluation time for replay. Cross-PRD obligation on Contracts to author the SoR; Subscriptions consumes via events + read models. |
| **SUB-C2** | MED | **Joint** | **Ramps (committed multi-step schedules).** Contracts authors the committed ramp; Subscriptions **executes** it as a sequence of scheduled `changePlan`/`updateQuantity` intents (**SUB-D-04**, S:630). No native `SubscriptionSchedule` aggregate at launch. **Open:** atomic multi-action submission (Zuora-Orders-style) is a Contracts/Design follow-up (S:1342). Depends on the SUB-D-01/02 intent envelopes. |
| **SUB-C3** | MED | **SUB-adopts (owner = Contracts)** | **Commitment pools.** Committed-usage pools are **Contracts SoR**, true-up is **rating** (rating **T-D-14**, rating SEAMS **M8**; S:181). This gear keeps **subscription-side hooks only** — it neither owns the pool balance nor computes the true-up. Adopt the owner split; expose the subscription linkage. |
| **SUB-C4** | MED | **Joint** | **Activation date-trio + acceptance.** `contractEffectiveAt` (booking) is **referenced from the Contract** — booking semantics stay Contracts/Finance SoR; `serviceActivatedAt` is stamped at the `activate` commit here; `customerAcceptedAt` is stamped by an optional **acceptance confirmation** where Contract clauses require it, else = service activation (**SUB-D-05**, S:500). **No new statuses.** All three ride lifecycle events + ASC hooks. **Open:** the confirmation-flow shape (who confirms, evidence) is design (S:1343). |
| **SUB-C5** | MED | **SUB-adopts** | **`PriceOverride` windows.** Contracts supplies negotiated override windows consumed via events/read models; in rating these are the **step-5 contract overlay** (rating `04-overlays-precedence`, precedence Contract > Partner PriceOverlay > Catalog base). Subscriptions references the override binding for composition/renewal; it does not evaluate the override. |

## D. Billing & Invoicing

> Billing ingests recurring `BillableItem`s, posts immutable invoices, executes
> adjustments/credit/debit notes and dunning, and owns floor/cap + rounding. This section is the
> single **Billing-facing** surface: it folds in the still-open **pricing** gap **G-4** (prepaid
> drawdown / tax placement) as a joint line, since that too resolves at the Billing boundary.

| # | Sev | Verdict | Seam |
|---|-----|---------|------|
| **SUB-B1** | **CRIT** | **RESOLVED 2026-08-01 (rating T-D-33/T-D-34 adopt the fact as the recurring WHEN; joint fixtures owed)** | **Recurring idempotency + no-retro-edit + traceability — and the recurring WHEN owner.** `BillableItemCreated(kind=recurring)` idempotent per `(subscriptionId, billing period, lineKey)` — per component since SUB-D-19, `lineKey` value pinned by SUB-D-21 (S:815); posted invoice lines never rewritten — corrections flow as new billable/adjustment artifacts (S:825); every item traces **per component** to its own `{skuId, planId, priceId}` + `pricingSnapshotRef` (S:835). What crosses this seam is the **money-free period fact**; the priced line arrives at Billing via the rating enrichment (**SUB-R6**, SUB-D-07). These are manifest §4.3/§4.4 invariants shared with Billing; design exposes the handoff payload (slice `08-events-billing`, `09-consumer-contracts`). **Why not ALIGNED:** the counterpart gear files the same key as **CRIT \| Joint — OPEN** (rating `SEAMS.md` SB1: "two owners of the recurring WHEN, two idempotency keys, no consumption contract between them"), rating's live normative design still **self-triggers recurring off its own period tick** (rating `design/14`) — the exact option (c) SUB-D-07 rejected — and no register on either side records who retires or subordinates that tick; the previous label pointed a veto reviewer the wrong way (REVIEW F-08-3 discloses the substance). Resolve in the SB1 joint round **before either gear implements `billingAnchorPolicy`** (SUB-P9); this gear's position: SUB-D-07/19/21 make the fact the only recurring WHEN and rating's tick subordinates to consuming it. **Resolved exactly that way 2026-08-01 — rating T-D-33** (fact-driven `PeriodTick`, `AnchorPeriod` ≡ the fact's period identity, carried fields absorbed, `AnchorCalendar` = geometry + watchdog only) **+ T-D-34** (the SUB-D-21 `lineKey` value rule adopted verbatim). Owed: the joint K5 anchor fixture (calendar geometry ≡ fact identity) + the SUB-R6 `lineKey` fixture cases; `billingAnchorPolicy` adoption unparked here as **SUB-D-27** (SUB-P9). |
| **SUB-B2** | MED | **Joint** | **`collectionPaused` artifact treatment.** The billing-only pause (service running, collection suppressed/deferred) is a Subscriptions **attribute posture on `active`** (**SUB-D-03**, S:662); **Billing chooses the artifact treatment** per policy (suppress vs defer) — the period fact is still emitted, marked `collectionPaused`, so Billing has the artifact to treat (AC 24 "not posted" holds). Renewal **collection** inside the window is deferred with it (payment pre-check/grace/dunning suspended; term extension continues — **SUB-D-12**, AC 29). **Open (§15):** pause-day limits and resume-proration mechanics — Product/Billing (S:1347). Finance may still prefer a Billing-side AR hold; the aggregate-posture shape keeps the audit trail on the subscription either way. |
| **SUB-B3** | MED | **SUB-adopts (owner = Billing)** | **Period floor/cap + rounding execution.** Floor/cap and rounding are **Billing-executed** (rating §17.1 post-step-9; rating `09-period-plan-change` `PeriodFloorCapObligation`). Subscriptions neither floors nor rounds; it coordinates the artifacts only. Adopt the owner. |
| **SUB-B4** | MED | **Joint (pricing G-4 — closed)** | **Prepaid drawdown + tax placement.** The pricing gap **G-4** was **closed 2026-07-28 (pricing D-48)**: drawdown applies **post-discount, pre-tax** (a credit reduces the charge, never pays the invoice), dormant at MVP (tax-exclusive) with a revisit checkpoint at Tax Engine GA; Billing countersigns at its gear PRD. Subscriptions still supplies only the subscription-side hooks (which subscription, which grant reference); nothing further to resolve here before prepaid GA beyond the Billing countersign. |
| **SUB-B5** | MED | **SUB-authors** | **Dunning handoff.** Post-renewal billing failure hands off to **dunning** (Billing/Payments §4.4–4.5); the §6.5 grace rules and triggers apply (S:707). Subscriptions emits the failure/grace signals and the audit trail; **dunning execution + PSP webhook payloads are Billing/Payments + Design** (S:1058, S:1326). |
| **SUB-B6** | MED | **Neighbour-extends (Billing)** | **Posted-period watermark for the backdating guard.** The §6.3 backdating guard ("reject a boundary inside an already-posted invoice period", AC 6) needs to *know* what is posted — Billing MUST expose a per-subscription **`billedThroughAt`** watermark (read model/event) that Subscriptions consumes fail-closed (unknown watermark ⇒ treat as posted, reject). Identified by the 2026-07-15 design review: without this the guard has no data source (design slice 03 §4.6). |
| **SUB-B7** | MED | **Joint (added 2026-07-28, billing-pass review #10)** | **Mid-term cancellation money path.** PRD §6.8: "cancel mid-period → early-termination fee or refund per **contract**, materialized as Billing artifacts". Split: **Subscriptions** supplies the facts (`SubscriptionCancelled` with instant + `cancelMode` + reason + contract ref **+ the current term window + the containing billing-period identity** — SUB-D-25's join key, 2026-08-01, so Billing joins against the period facts it holds instead of re-reading the aggregate; the in-flight period fact stands — SUB-D-18); **Contracts** defines the ETF/refund/credit terms **within the early-termination reason class (`customer`/`operator`) — `term_expired`/`nonpayment_exhausted`/`saga_superseded` never derive an ETF or credit (SUB-D-25)**; **Billing** materialises the artifacts (new billable/adjustment paths, never a retro-edit). This gear computes no money on this path. |
| **SUB-B8** | MED | **Joint (added 2026-08-01, wave-3 review #6 / SUB-D-24)** | **One-time / setup charge valuation.** Rating synthesizes no unit for `chargeKind ∈ {one_time, one_time_setup}` (rating T-D-18) and this gear emits an **amount-less** `BillableItemCreated(kind=one_time)` at the qualifying instant (activation / trial conversion), deduped once per subscription lifetime per `(subscriptionId, priceId)` — so **Billing values the charge from the frozen `pricingSnapshotRef`** (copying a frozen flat amount is resolving a published price fact; pricing forbids tier machinery on one-time rows) and posts it. Billing is unauthored — the obligation is recorded here the way SUB-C1 records the Contracts defaults; wire payload slice 09 §4.3. This closes the "Subscriptions/Billing" split rating T-D-18 left as a pair. |

## E. Policy Engine & OSS Provisioning

> Every resource-affecting transition is fail-closed gated by Policy before commit; OSS executes
> provisioning; entitlement **enforcement** executes in OSS while the **decision state** stays here.

| # | Sev | Verdict | Seam |
|---|-----|---------|------|
| **SUB-E1** | CRIT | **ALIGNED (SUB-adopts)** | **Fail-closed Policy gate.** Every resource-affecting transition passes a pre-commit allow/deny + `reasonCodes`; on deny **or unavailability** the state MUST NOT change (S:450, S:1046; AC 1). Manifest §6. Aligned; design maps the gate call + reason surfacing. |
| **SUB-E2** | HIGH | **SUB-authors** | **OSS provisioning confirmation.** `activate`/`suspend`/`resume`/`cancel` coordinate provision/deprovision/pause **work orders confirmed by events**; **BSS never mutates OSS resource topology directly** (S:272, S:1052). Subscriptions issues the intent + consumes the confirmation; it does not touch OSS state. |
| **SUB-E3** | HIGH | **Joint** | **Entitlement enforcement split.** Subscriptions serves the **point-of-use check decision state** (feature flag, quota remaining, limit state) at **p95 < 100ms** (S:877); **OSS enforces** (allow/block/degrade) — this gear never executes enforcement. **Open (§15):** mid-request behaviour at the exhaustion instant (graceful degradation vs hard block) is OSS/Design (S:887, S:1344). Pin the check-state ↔ enforcement contract with OSS at design. |

## F. Payments & Notifications

| # | Sev | Verdict | Seam |
|---|-----|---------|------|
| **SUB-F1** | MED | **SUB-adopts** | **Payments signals.** Payment **pre-check** outcomes + **retry-exhaustion** declarations feed the renewal/grace ladder (S:1056); authorization is requested at renewal / trial conversion. **PSP capture + webhook behaviour are out of scope** (S:278, §4.5) — this gear only triggers requests and consumes outcome signals. The grace exit trigger (ii) — "no further automated retries" — is a Payments declaration (S:725). |
| **SUB-F2** | LOW | **SUB-authors** | **Notification triggers.** Renewal-notice **triggers + intervals** (30/14/7/1) and the trial-expiry **win-back hook** are owned here (S:694, S:929); **delivery channels are Notifications/Comms** (out of scope §5.2). Subscriptions emits the trigger events; it never delivers. |

## G. Catalog Registry (Product & SKU)

> The registry carves Product/SKU/Category/Attribute/`PlanTier` taxonomy/`CatalogVersion` out of the
> manifest. It is now **vendored on this branch** as the **products** gear (`gears/bss/products/docs/PRD.md`, 2026-07-16, from upstream **PR #4177**; merged in
> vhp-architecture `main`, tracked in [product-sku-prd-location]). Seams are tracked **prospectively**.

| # | Sev | Verdict | Seam |
|---|-----|---------|------|
| **SUB-G1** | HIGH | **Joint (prospective, PR #4177)** | **`overlapScopeKey` binds to a registry key.** The overlap cardinality rule defaults to at most **one `active`** per `(payerTenantId, catalogSubscriptionProductKey)` (S:610). `catalogSubscriptionProductKey` is the **registry-owned** stable key for the sellable subscription product/family; **Design binds the stored field to a published SKU/product key**. Engage on PR #4177 before merge so the key shape is agreed; `maxConcurrentActive` override may come from Catalog **or** Contract. |
| **SUB-G2** | MED | **SUB-adopts** | **Published `skuId` / `PlanTier` / `CatalogVersion`.** Subscriptions reads **published** registry facts only (S:242); `PlanLink`/overlap key bind to published keys; effective `PlanTier` MUST be derivable @ event time (S:540). Read-only consumer; never re-authors taxonomy. |

## H. Naming / cross-cutting

| # | Sev | Verdict | Seam |
|---|-----|---------|------|
| **SUB-N1** | LOW | **Joint (manifest alignment)** | **New `TransitionRequest.type` values + scheduled-intent envelope.** `updateQuantity` (SUB-D-02), `convertTrial` (G-2), and the SUB-D-08 completion set (`renew`, `unschedule`, `pauseCollection`, `resumeCollection`, `confirmAcceptance`, `extendTrial`, plus `archive` — the `cancelled → archived` retention edge, added to the set 2026-07-28) extend the manifest §4.3 `type` list; the scheduled-intent envelope (`cancelMode`, `resumeAt`) extends the change vocabulary (S:480, S:490). Manifest alignment is tracked in §15; **not re-opened here** — the seam records that downstream consumers keying on `TransitionRequest.type` must not fail-closed on the new values before the manifest lands. |
| **SUB-N2** | LOW | **Naming** | **Gear-name pinning.** "Pricing (Product Catalog)", "Rating (evaluation core + pipeline)", "Catalog registry (Product & SKU)" are pinned in §2.1 (S:156). Downstream references MUST use these canonical names; no drift to legacy "Tariffs"/"PLAL"/"Price Book". |

## I. Orders (Lifecycle + Workflow)

> Added 2026-08-17. Orders was **not a neighbour** when this map was produced — the seam analysis ran
> against the mature pricing and rating design sets, and no order artifact existed on either side.
> An **Orders Lifecycle** PRD is now drafted upstream (vhp-architecture `docs/bss/prd/PRD-orders-lifecycle-202608101404/`,
> branch VHP-2632; findings register `REVIEW-orders-lifecycle-202608141426.md` in the same folder);
> its sibling **Orders Workflow** PRD is unwritten. Orders is **additive and one-directional**: it
> consumes this gear and this gear consumes nothing from it — subscription `create` remains a
> client-invoked constructor commit and the direct (non-order) path stays valid. Sequencing is
> therefore Subscriptions first, Orders second; the rows below are the hooks that are cheap to honour
> while this gear is being designed and expensive to retrofit afterwards. Rows cite Orders findings as
> `REVIEW F-n`.

| # | Sev | Verdict | Seam |
|---|-----|---------|------|
| **SUB-O1** | **CRIT** | **Neighbour-extends** | **Cancellation reason for order-fulfilment compensation.** `SubscriptionCancelled.reason` is a **closed** enum (`design/08` §4: `{customer, operator, term_expired, nonpayment_exhausted, saga_superseded}`) and **SUB-D-25** scopes `customer`/`operator` into the early-termination class that derives an ETF or credit, while the other three never do. An order fulfilment that fails **after** one or more lines have already activated must compensate by cancelling the subscriptions it created — a cancellation that is neither an early termination (no party defaulted, no fee is owed) nor a plan-change supersession. **No existing value fits:** `customer`/`operator` wrongly derive an ETF/credit, `saga_superseded` is reserved for the cancel+new pair of a plan change. → **Obligation here:** add a value (e.g. `order_compensation`) and scope it **out** of the SUB-D-25 ETF/credit class. Reason values ride event payloads and downstream consumers key on them, so adding one after Billing consumes the contract is a **breaking** change — this is the one Orders seam materially cheaper now than later. `REVIEW F-5`. |
| **SUB-O2** | HIGH | **Neighbour-extends** | **External order reference on `create` + idempotency-key derivation.** `create` is a constructor commit deduped on `(orderingTenantId, operation = create, client idempotencyKey)` (`design/01` §4.2). Orders Lifecycle is the SoR for the order document and needs the linkage **bidirectional**: the order persists the resulting `subscriptionId` per line (its own FR), and the subscription must carry the originating `orderId`/`orderLineId` so "which order produced this subscription" is answerable from either side without a join through events. The field is additive; the **design question** is whether an order line may supply the create key **deterministically** (line id ⇒ exactly-once create under workflow retries, no client-generated key to lose) or whether the client key stays opaque and the order reference is metadata only. → **Obligation here:** accept an optional order reference on `create`, and pin the key-derivation rule before the idempotency registry is built. `REVIEW F-5`. |
| **SUB-O3** | HIGH | **SUB-authors** | **`draft → activate` MUST remain an externally callable two-phase pair.** No change beyond keeping the contract **SUB-D-11** already establishes: `draft` is exitable without activation and the `draft → cancelled` void is **not** resource-affecting — no Policy gate, no OSS leg, no billable facts. Order-level all-or-nothing fulfilment is built exactly on this: the workflow creates **every** line's subscription in `draft`, activates only once all creates have succeeded, and compensates anything before the first activation with the void. If `create` were ever collapsed into create-and-activate for convenience, order atomicity would have to be rebuilt as a compensating saga over **`active`** subscriptions — which is **unachievable**: `active → cancelled` is Policy-gated **fail-closed on unavailability** (SUB-E1), its deprovision leg can terminate unconfirmed, and the one-time billable fact has already been emitted at activation and posted by Billing (SUB-B8, SUB-D-24). → **Obligation here:** record the two-phase pair as an external contract, not an internal step. `REVIEW F-5`, `F-6`. |
| **SUB-O4** | HIGH | **Joint (with pricing)** | **Sellability gate as a pre-purchase check for a caller that is not yet creating a subscription.** **SUB-P5** adopts the pricing gate as the `create`/`changePlan` guard, evaluated as the pricing **D-94** conjunction over every scope key the purchase binds. Orders must run the **same** predicates at order submit — before any subscription exists — so a basket is rejected at capture rather than at activation, when the first line may already have provisioned. Re-implementing the list Orders-side forks it: the drafted Orders gate already omits committed `CatalogVersion`, lifecycle-not-retired, per-market GA and registry `sellable = true`, and adds a party-eligibility predicate that exists in neither gear. → **Obligation here (joint with pricing):** expose the conjunction evaluator as a callable pre-check over prospective-purchase inputs (no `subscriptionId`, no `activatedAt`, no bound `cohort`), noting pricing **D-167** records three of the six predicates as not yet evaluable from the built read model. `REVIEW F-9`. |
| **SUB-O5** | HIGH | **Neighbour-extends** | **`overlapScopeKey` presence read for order-time conflict detection.** Default cardinality is at most one **`active`** per `(payerTenantId, catalogSubscriptionProductKey)` (S:623), bound to a registry key per **SUB-G1** and evaluated fail-closed on **every** entry into `active`. A multi-line order touching the same product family under one payer therefore passes order submit and fails on the **second** activation — after the first line has provisioned — forcing precisely the compensation path SUB-O1/SUB-O3 exist to avoid. → **Obligation here:** expose an overlap-key presence read (does a non-terminal subscription already hold this key for this payer, and what is the effective `maxConcurrentActive`), so the order gate can check both **within** the basket and **against** existing subscriptions. Shape follows the **SUB-P8** precedent — neighbour submits keys, this gear answers presence. `REVIEW F-8`. |
| **SUB-O6** | MED | **Joint (reopens SUB-D-04 / SUB-C2)** | **Atomic multi-subscription submission is cheaper than the deferral assumed.** **SUB-C2** records "atomic multi-action submission (Zuora-Orders-style) is a Contracts/Design follow-up" and **SUB-D-04** declined a native schedule aggregate at launch — both reasoned against a saga over **committed** subscriptions. The Orders two-phase shape (SUB-O3) needs no such saga: N creates in `draft`, then N activates, with the void edge as compensation for everything before the first activation. The residual non-atomic window is activation-to-activation only, and it is bounded by SUB-O1's compensation value. → **Obligation here:** revisit the deferral against the two-phase shape rather than treating multi-line orders as Contracts-blocked; the reopen is an Orders/Subscriptions joint call. `REVIEW F-5`, `F-30`. |

---

## Ownership matrix (contested / adjacent responsibilities)

| Responsibility | Owner | Seam |
|---|---|---|
| Subscription lifecycle state machine + terminality | **Subscriptions** (SoR) | SUB-R1 |
| Change **boundary/mode** (`changeEffectiveAt`, `changeMode`) | **Subscriptions** | SUB-R1, SUB-R3 |
| Proration / plan-change **math** | **Rating** (evaluates); Subscriptions sets the boundary | SUB-R1 |
| Usage slicing at the change boundary | **Rating** (pipeline) | SUB-R1 |
| `(currency, region)` snapshot segment | **Subscriptions** @ activation; rating seals the ref | SUB-R2 |
| Seat-count provenance (effective-dated, `quantity @ t`) | **Subscriptions** (committed `updateQuantity` only); rating consumes frozen | SUB-R3 |
| Ordering/partition key (`orderingTenantId`, pinned at creation) | **Subscriptions** — immutable across transfers | SUB-R1 |
| Recurring period **cut** (anchor, pauses, intents, idempotency key) | **Subscriptions**; rating **prices** the fact; Billing posts | SUB-R6, SUB-B1 |
| Brand context source for overlay matching | **Contested** — per-sale (here) vs Plan/SKU (rating); pin at design | SUB-R5 |
| Posted-period watermark (`billedThroughAt`) | **Billing** exposes; Subscriptions enforces the backdating guard | SUB-B6 |
| Plan phase **structure** | **Subscriptions** (SoR); rating resolves @ `t` | SUB-R4 |
| Plan-change **classification** (comparability/targets) | **Pricing** (publishes); Subscriptions enforces | SUB-P1 |
| Entitlement grant-set **templates** (incl. per-phase) | **Pricing** (authors); Subscriptions assigns | SUB-P2 |
| Entitlement **assignment** per subscription | **Subscriptions** | SUB-P2 |
| Entitlement point-of-use **decision state** | **Subscriptions** (serves); OSS enforces | SUB-E3 |
| Trial sellable **definition** | **Pricing/Catalog**; Contract legal clauses | SUB-P3, SUB-C1 |
| Prepaid credit **definition** / **drawdown** | **Pricing** (D-43 def) / **Billing+Rating** (balance) | SUB-P4, SUB-B4 |
| Renewal / grace / regional templates SoR | **Contracts** (unauthored upstream → platform default) | SUB-C1 |
| Ramp authoring / execution | **Contracts** (authors) / **Subscriptions** (executes intents) | SUB-C2 |
| Commitment pools | **Contracts** SoR; true-up = Rating; Subscriptions hooks only | SUB-C3 |
| Booking date (`contractEffectiveAt`) | **Contracts/Finance**; Subscriptions references | SUB-C4 |
| Recurring idempotency + posted-invoice immutability | **Subscriptions** (keys/emits) + **Billing** (posts) | SUB-B1 |
| `collectionPaused` posture / artifact treatment | **Subscriptions** (posture) / **Billing** (artifact) | SUB-B2 |
| Period floor/cap + rounding | **Billing** | SUB-B3 |
| Dunning execution + PSP webhooks | **Billing/Payments** | SUB-B5, SUB-F1 |
| Fail-closed transition gate | **Policy Engine** | SUB-E1 |
| OSS resource topology | **OSS** (BSS never mutates directly) | SUB-E2 |
| Notice/win-back **delivery** | **Notifications/Comms** | SUB-F2 |
| `catalogSubscriptionProductKey` / published SKU/`CatalogVersion` | **Registry** (Product & SKU) | SUB-G1, SUB-G2 |
| Mid-term cancellation money path (facts / terms / artifacts) | **Subscriptions** (facts + reason class) / **Contracts** (ETF terms) / **Billing** (materialises) | SUB-B7 |
| One-time / setup charge (emission / valuation) | **Subscriptions** (amount-less fact + lifetime dedup) / **Billing** (values from the frozen ref, posts) | SUB-B8 |
| Plan migration (authoring / execution) | **Pricing** (authors, state machine, exclusion set) / **Subscriptions** (executes batches, reports) | SUB-P7 |
| Per-scope-key in-flight-subscriber presence | **Subscriptions** (data source; presence read) — pricing consumes fail-closed | SUB-P8 |
| `billingAnchorPolicy` publication / clamp execution | **Pricing** (publishes K2 enum + clamp) / execution owner **gated on SB1** (SUB-B1) | SUB-P9 |

---

## Decisions register (to close before Design lock)

**Resolved on this gear (autonomous; ALL 26 CONFIRMED per-item 2026-08-01 — the gear's first veto round; [`DECISIONS.md`](./DECISIONS.md)):**
- **SUB-D-01** — scheduled lifecycle intents (`cancelMode`, `resumeAt`; pending intents suppress renewal/next-term recurring). Seams SUB-C2, SUB-B2.
- **SUB-D-02** — `updateQuantity` first-class transition with the change envelope; up/down asymmetry. Seams SUB-R3, SUB-P1.
- **SUB-D-03** — `collectionPaused` posture on `active`. Seam SUB-B2.
- **SUB-D-04** — ramps: Contracts authors, Subscriptions executes scheduled intents. Seam SUB-C2.
- **SUB-D-05** — activation date-trio as attributes; no new statuses. Seam SUB-C4.
- **SUB-D-06** — ordering tenant pinned at creation; transfers never rebind the ordering/partition key. Seam SUB-R1.
- **SUB-D-07** — recurring split: money-free period fact here, rating prices, Billing posts. Seams SUB-R6, SUB-B1.
- **SUB-D-08** — mutation-type inventory completed (`renew`, `unschedule`, `pauseCollection`/`resumeCollection`, `confirmAcceptance`, `extendTrial`). Seam SUB-N1.
- **SUB-D-09** — secondary producer-event inventory named in design slice 08. Seams SUB-R1, SUB-B1.
- **SUB-D-10** — entitlement check surface: bounded-staleness degraded mode. Seam SUB-E3.
- **SUB-D-11** — `draft → cancelled` (void) edge. (Status machine; no cross-gear seam.)
- **SUB-D-12** — `collectionPaused` defers renewal collection (pre-check/grace/dunning), not term extension. Seam SUB-B2.
- **SUB-D-13** (2026-07-28) — term boundaries always resolve: `autoRenew=false` → system `term_expired` cancel; post-suspension payment backdates the term. Seams SUB-C1, SUB-B1.
- **SUB-D-14** (2026-07-28, **amended by the billing pass**) — renewal snapshot-ref re-resolution is **eligibility-first**: non-grandfathered re-binds to the current row; grandfathered keeps its pinned generation until `grandfatherUntil` passes (`EligibilityExpirySignal`). Seam **SUB-P6**.
- **SUB-D-15** (2026-07-28) — manual change mid-ramp supersedes the remaining Contract-authored steps. Seam SUB-C2.
- **SUB-D-16** (2026-07-28, billing pass) — nonpayment `suspended` dwell: 90-day platform default → system `nonpayment_exhausted` cancel. Seam SUB-C1.
- **SUB-D-17** (2026-07-28, billing pass) — renewal price-change notice input (pricing-sourced supersession/expiry flag arms the 30-day commercial notice). Seams SUB-P6, SUB-F2.
- **SUB-D-18** (2026-07-28, billing pass) — mid-term cancellation money path: fact stands; ETF/refund = Contracts defines, Billing materialises. Seam **SUB-B7**.
- **SUB-D-19** (2026-07-28, billing pass — row added 2026-08-01, wave-3 review #24a) — the recurring period fact is **per component**: key `(subscriptionId, billing period, lineKey)`, per-component traceability tuple. Seams SUB-B1, **SUB-R6**.
- **SUB-D-20** (2026-08-01, wave-3) — a cut resolves **as of what it describes**: composition over the period, payer at period start, revival eligibility as of the backdated term start; conversion-suspension revival needs no term extension. Seams SUB-P6, SUB-B1, SUB-R6.
- **SUB-D-21** (2026-08-01, wave-3) — `lineKey` = component **interval** (`plan#n` / `addon:{addOnId}#n`); mid-period interval opens get a targeted cut; terminal-before-cut periods still cut for the served stretch; trial-phase periods cut normally; quota reset per period. Seams SUB-B1, **SUB-R6** (joint fixture value rule).
- **SUB-D-22** (2026-08-01, wave-3) — dwell deadline lifecycle: voided by payment/exit, re-resolved on transfer, dispute hold, daily re-derivation + no-op deprovision. Seams SUB-C1, SUB-F1.
- **SUB-D-23** (2026-08-01, wave-3) — firing-failure taxonomy third class: **parked (state-precondition)**, re-armed by the unblocking commit. (Foundation; no cross-gear seam.)
- **SUB-D-24** (2026-08-01, wave-3) — one-time/setup lane: amount-less fact here, **Billing values from the frozen ref**; lifetime dedup `(subscriptionId, priceId)`. Seam **SUB-B8**.
- **SUB-D-25** (2026-08-01, wave-3) — ETF derivation reason-aware (`customer`/`operator` only) + the term/period join key on `SubscriptionCancelled`. Seam **SUB-B7**.
- **SUB-D-26** (2026-08-01, wave-3) — the grandfathered cohort does not carry across cancel+new; loss disclosed pre-execution. Seams SUB-P6, SUB-P1.
- **SUB-D-27** (2026-08-01, SB1-resolution round) — `billingAnchorPolicy` adopted verbatim (K2 enum + D-20 no-drift clamp), executed by the emitter's period derivation; an anchor-altering plan change takes effect at the next boundary; K5 joint anchor fixture = design-freeze gate. Seams **SUB-P9**, SUB-B1.

**Aligned (counterpart written; no action beyond citing):**
- SUB-R2 (rating SEAMS S1), SUB-E1. *(SUB-P3 and SUB-B1 were removed from this list 2026-08-01 — wave-3 review #21: SUB-P3's own verdict is OPEN since 2026-07-28, and SUB-B1 is the rating-SB1 CRIT joint seam.)*

**Joint / cross-PRD obligations still open (engage the owner before Design lock):**
- **SUB-C1** — Contracts must author the renewal/grace/regional-template SoR (upstream PRD unauthored; §16 risk). Highest cross-PRD risk.
- **SUB-B1 / rating SB1 — RESOLVED 2026-08-01** (rating T-D-33/34: the fact is the recurring WHEN, the tick consumes it). Remaining joint work = the K5 anchor fixture + SUB-R6 `lineKey` fixture cases, both design-freeze gates.
- **SUB-P6** — grandfathering expiry signal + notice-lookahead + as-of-read contract fields — close with pricing at design freeze.
- **SUB-P7** — plan-migration executor handshake — executor side authored (slice 03 §4.7); the wire fields close with pricing.
- **SUB-P8** — in-flight-subscriber presence read — authored (slice 09 §4.1, PRD §9.1); the price-id-set request shape closes with pricing.
- **SUB-B7 / SUB-B8** — ETF/credit materialisation and one-time valuation — obligations on the unauthored Billing gear (recorded like SUB-C1's defaults).
- **SUB-P9** — `billingAnchorPolicy` adoption + the K5 joint anchor fixture — **unparked 2026-08-01** (SB1 resolved this gear's way): adopted as **SUB-D-27** — the emitter executes the K2 enum + D-20 no-drift clamp; the fixture is a design-freeze gate.
- **SUB-R1** — mirror-reconcile the read-model field lists with rating PRD §9.2 (seat quantity @ `t`, `priceEligibility` inputs; downgraded from ALIGNED by the 2026-07-15 review).
- **SUB-R3** — mid-period seat-change boundary transport (default: change-boundary) — pin with rating.
- **SUB-R4** — phase-boundary instant on the shared `(changeEffectiveAt, changeMode)` channel (incl. trial extension moves) — confirm with rating.
- **SUB-R5** — brand context source for overlay matching (per-sale vs Plan/SKU) — pin with rating; AC 20 blocked until resolved.
- **SUB-R6** — recurring pricing enrichment (SUB-D-07): rating counterpart contract + joint fixture (fixture scope now includes the SUB-D-21 `lineKey` value rule).
- **SUB-B6** — Billing to expose the `billedThroughAt` posted-period watermark for the backdating guard.
- **SUB-E3** — check-state ↔ OSS enforcement contract + quota mid-request instant — pin with OSS; staleness-budget default (SUB-D-10) to confirm.
- **SUB-B2 / SUB-B4** — pause-day limits + resume proration (Product/Billing); prepaid drawdown/tax placement (pricing G-4) — pin with Billing.
- **SUB-C2** — atomic multi-action ramp submission — Contracts/Design follow-up.
- **SUB-C4** — acceptance-confirmation flow shape — Design.
- **SUB-G1** — `catalogSubscriptionProductKey` shape — engage PR #4177 before merge.

**Manifest alignment (tracked §15, not a design blocker):**
- **SUB-N1** — `updateQuantity` / `convertTrial` `TransitionRequest.type` values + `cancelMode`/`resumeAt` envelope await manifest §4.3 alignment.

**Propagation status:** the SUB-D-01…26 decisions are propagated into [`PRD.md`](./PRD.md)
(§5.1/§6/§12/§15) and [`DECISIONS.md`](./DECISIONS.md). This seam map is the input to
the design set — each slice implements the Subscriptions side of the seams listed for it in
[`DESIGN.md`](./DESIGN.md) §1.3.
