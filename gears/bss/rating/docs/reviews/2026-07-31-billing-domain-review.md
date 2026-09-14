<!-- Related: ../DESIGN.md, ../DECISIONS.md, ../SEAMS.md, ../design/ | Owners: BSS Rating team -->

# Rating gear — billing-domain review (2026-07-31)

**Scope**: the 16 slice designs ([`../design/`](../design/)) + [`../PRD.md`](../PRD.md),
[`../DESIGN.md`](../DESIGN.md), [`../SEAMS.md`](../SEAMS.md), [`../DECISIONS.md`](../DECISIONS.md),
`ADR/`, cross-checked against the adopted pricing design set. Reviewed as a **billing system**:
rating semantics, money-affecting rules, idempotency, cross-gear contracts.
**Method**: 7 parallel finders produced **175 candidates**; 8 refutation passes verified each
against the real documents. **96 candidates were retracted during verification** — most defeated
by a passage in the same slice, in `PRD.md` verbatim, or in the pricing gear's own decision
register. Items whose claim was narrowed during verification say so inline.

> **Coverage caveat**: the 2026-07-31 pricing decisions (D-79…D-122) were checked only where a
> rating document cites them. Pricing's register keeps a live list of owed Rating-side
> adoptions; items #13/#14/#15 came from it — that list is the cheapest place to look next.

> **Status (fix wave 2026-08-01, on the owner's go): ALL 52 SURVIVING ITEMS FIXED.**
> Blocking #1–#4 + High #5–#20 → **T-D-23…T-D-31** where a decision was owed, direct
> normative fixes otherwise; #21 → the slice-10 §4.6 **authz catalog**; #22 → the
> `**Traces to**:` conversion across the 12 FR-bearing slices (43 FRs now machine-checked,
> single-owner, P2 clean on first pass; pipeline slices 13–16 own no PRD FRs); Cleanup #23–#46
> and Minor #47–#52 in place. Per-item mapping in the
> [fix record](#verification--fix-record). **Veto round 2026-08-01: T-D-18…T-D-28 and T-D-32
> all CONFIRMED per-item** (T-D-27/T-D-28 as joint — the Contracts/Billing cross-PRD mirrors
> stay owed); adoptions T-D-29/30/31 unqualified after pricing's same-day confirmations
> (D-113 incl.). Nothing in rating awaits veto.

Tier legend: **Blocking** — an implementer following the text builds wrong money today ·
**High** — money/correctness divergence on a reachable path · **Guideline** — platform MUST ·
**Cleanup / Minor** — contained or latent.

---

## Blocking

1. **Stale sum-only guarantee in the slice-03 §4.1 catalog-guarantee list** (03:310): "launch
   aggregation is sum only (SEAMS M10)" survived the T-D-17 re-scope that voided it — the same
   file contradicted it twice (§2.2, §4.3) and SEAMS M10 records the original action as void.
   An implementer reading the guarantee list builds sum-only band math and the two level-billed
   launch products (cloudlet peak-per-hour, storage GB-month) cannot be rated. Same class as
   the 2026-07-28 review's finding #2 — the fix missed this line. **Fixed** (line rewritten to
   the re-scoped enum with provenance).
2. **Band interval for the on-demand remainder when a reservation sits inside a sub-window
   slice**: T-D-12 places `Q_slice` over `[bandOffsetQ, bandOffsetQ + Q_slice)`; T-D-13 (and
   05 §4.2) re-band the remainder "from zero"; `bandOffsetQ` is raw accumulated `Q` with no
   reservation exclusion — on a slice carrying both a `reservationMatch` and a non-zero offset
   the two normative rules gave different placements with no precedence (worked-example bands:
   five figures per line). The mandated joint fixtures covered reservation + pool + overage but
   not reservation + slice. **Fixed as T-D-23**: the banded axis of a reservation-carrying line
   is the cumulative post-reservation remainder (`remainderOffsetQ`, window-cumulative —
   T-D-19's basis); "from zero" = the remainder axis starts at the window origin with reserved
   quantity excluded, never a per-slice reset; fixture added.
3. **Pin of record after an administrative re-rate**: 08 §4.1 fixed the replay source as "the
   snapshot pinned at first rating" and ended "input corrections replay the original pin" —
   so a late usage record after a corrective publish computed *(S1 + late usage) − (v2 on S2)*,
   silently bundling the negation of the two-person-approved repricing into one signed delta.
   **Fixed as T-D-24**: the re-rate advances the window's pin of record; later input
   corrections replay the superseding pin.
4. **Bundle `sum_of_parts` sum unqualified** (10 §4.5): "the sum of component resolved amounts"
   — pricing `inst-bb-sum` (2026-07-30, postdating rating's last edit) restricts the sum to
   component **recurring** amounts, usage rating per its own rows. Unadopted, the sum
   double-counted a metered component's usage and read a usage-only component as unresolvable.
   **Fixed** (adopted; partial-failure rule narrowed to recurring lines the sum requires).

## High (5–20)

5. **`overageRate` missing from the canonical `commitmentReservation` enumeration** — present in
   05 §4.1/PRD §6.6 as the T-D-19 selector, absent from all three lists claimed "recorded
   identically" (01 §4.3, 11 §4.7, SEAMS S1) and from the T-D-09 row. A composer built from any
   of them dropped the money selector. *Verification note: the sharper "selector flips on
   replay" framing was retracted — both sources say the field is frozen at contract publish;
   propagation defect, not a determinism hole.* **Fixed** (all four lists).
6. **`capacityCharge` had no time factor**: `reservedRate × reservedQuantity` once per period is
   money-per-granule, not a period charge (`reservedRate` is denominated in the level·granule
   billable unit); proration is recurring-only and usage is never prorated, so a disk allocated
   day 20 or resized mid-period had no defined charge. **Fixed as T-D-25**:
   `× coveredGranules`, summed per coverage sub-interval.
7. **`CommitmentBalanceEffect` published only for draw/refill** while T-D-10 obliges Contracts
   to re-resolve units "rated overage against" the pool — pure-overage units were unnameable,
   so a retroactive refill never reached them and they kept the overage price after every
   pool-affecting correction. **Fixed as T-D-27(1)**: an effect per pool-observing outcome
   (zero-draw + overage marker included).
8. **Stale-frozen-balance over-draw undetectable**: two units sharing a pool at one frozen
   `balanceVersion` each absorb the full remainder (the steady state under load); every repair
   route was scoped to later-`balanceVersion` units and the true-up is shortfall-only.
   **Fixed as T-D-27(2)**: Contracts detects over-draw at write-back and emits the same T-D-10
   triggers; joint obligation recorded.
9. **`qVersion` bump and `ReMaterializationSignal` not transactionally coupled** (13 §3.6/§4.4)
   while the sibling slices are explicit ("same commit as the store write"). A crash between
   them froze the rated output at a superseded `Q` until an unrelated correction self-healed
   it. **Fixed** (same transaction, signal via outbox).
10. **"Three writers" is false** — the segment table has four (registry, pricing ×2,
    Subscriptions, Rating) since the D-66 producer split; the "recorded identically" claim
    failed at SEAMS S1 and 01 §4.3, and slice 11's row omitted the T-D-17 level-aggregation
    triple. **Fixed** (four writers + the triple, all copies; DESIGN §4 S1 bullet too).
11. **No Contracts & Agreements contract in slice 11** — "six contracts partition the boundary"
    while a p1 dependency authoring pools/balances/`overageRate`/negotiated rates, receiving
    effects, and owing the T-D-10 triggers had no row, and 05 §3.5's pointer dangled. The
    semantics existed in 05/14/15 — a completeness and routing gap. **Fixed**: §4.9 + a §3.3
    interface id + a §3.5 row + PRD §9.2 `cpt-cf-bss-rating-contract-contracts-input`.
12. **The T-D-22 `line_total` launch rule missing from the Promotions contract** (11 §4.5) —
    the slice a Promotions engineer builds against documented `default line_total` and failed
    closed only on absence, while 06 §4.3 obliges Promotions to write an explicit line scope
    for every launch coupon. Shipped on the default ⇒ no discount ever applies at launch.
    **Fixed** (launch rule + driver row; `valueCurrency` mirrored, #31).
13. **Pricing D-113 unadopted**: plural "carry-vs-reset flags … all fail-closed when absent"
    (09 ×2, 05 §4.5) vs the published singular `usageCounterOnPlanChange` with absence = reset
    and **no pool flag**; the two owed guards (target-plan routing; per-line D-82/D-98/D-122
    unit-field match) were recorded nowhere rating-side. **Fixed as T-D-29** (+ SEAMS P3 row,
    PRD ×3 sites).
14. **Pricing D-78 unadopted**: the step-4 stack had no `cohort` eligibility filter, so a
    partner markup line reached `existing_grandfathered` rows at evaluation. **Fixed as
    T-D-30** (04 §4.2).
15. **Pricing D-114 unadopted**: 11 §4.2/§3.6 and 14 §2.2 kept the two-condition
    pin-eligibility rule; 14 §2.2 is gate-side, so the missing prefix-closure mattered.
    **Fixed as T-D-31** (five sites).
16. **`maxHold` unit never stated** — it is an integer count of **granules** (pricing's
    declaration), so a `time_weighted` hold crosses granule boundaries and a late sample in N
    re-folds N…N+`maxHold`, against four sites claiming "re-folds only its granule". Money was
    saved only by the whole-window recompute. **Fixed** (03/13/PRD/T-D-17 row restated).
17. **Granule straddling a slice cut unattributed** — per-slice peak maxima summed to more than
    the whole-granule max, breaking `Q = Σ granule folds`; the same section already had the
    rule for a straddling package block. *Verification note: the companion "granule origin
    unspecified" claim was downgraded — "the granularity cut of the window" is a defensible
    single reading.* **Fixed as T-D-26(b)**: the granule belongs to the slice that opened it.
18. **`per_unit` quantity had no version coordinate or replay source** in the sealed-context
    enumerations (14 §4.3, 01 §4.2). *Verification note: the "re-rate reads a live seat count"
    framing was retracted — the value is stated frozen in context; what was missing is the
    sealed replay source.* **Fixed**: the pair `(N, source ref)` joins the sealed set and the
    determinism tuple.
19. **`calendar_days_actual` boundary day and the sum-to-one invariant undefined** — a mid-day
    cut left the boundary day countable in A, B, both or neither (10/31 + 22/31 over-bills).
    **Fixed as T-D-26(a)**: the day belongs to the slice covering its 00:00 UTC; fractions sum
    to exactly 1, fixture-asserted.
20. **Correction routing on a stale `open` had no fence** — `period_state` is a projection with
    no version and no re-read-after-route rule, in the same assembler that imposes a 5s pin
    lag. *Verification note: the double-billing half was retracted (delta dedup + Billing
    idempotency); what survives is mis-routing — wrong artifact kind and audit posture.*
    **Fixed as T-D-28**: a per-`(subscription, period)` Billing sequence, highest-wins,
    route-time re-read, sequence recorded in the delta audit. (#48 fixed together.)

## Guideline compliance

21. **No `(resource_type, action)` catalog / PDP coverage for the gear's eleven SecureORM
    stores** (a `docs/toolkit_unified_system/06` MUST + GTS §12.7–12.8/§14) — per-subscription
    usage volumes and money amounts with no `PolicyEnforcer` mention anywhere, while pricing
    S5 carries a dedicated catalog for data no more sensitive. **Fixed**: slice 10 §4.6 —
    `gts.cf.bss.rating.{usage, rated_output, rerate, operations}.v1~`, PEP-before-repository,
    deny-by-default, RatingOperator/Auditor matrix, service identities; quarantine replay
    (#51) and the administrative re-rate (#49) are its named money-affecting actions.
22. **Prose traceability invisible to the checker** — `P2/traceability-convention-unknown`, 43
    requirements unchecked. **Fixed**: every slice's §5 now opens with a `**Traces to**:`
    block; each of the 43 FRs assigned to exactly one owning slice; spec-check P2 clean on
    first pass (no multiply-claimed, no unclaimed).

## Cleanup (23–46) — all fixed in place

23. `ReadModelPinAdapter` "five **pricing** events" → five catalog events, two producers (11 §3.2). Narrowed: the count was right, one word misattributed the producer.
24. Residual absorber re-typed per pricing D-55 — per `(bundle, vendor SKU)` `residual_absorber_party`; the bundle-level absorber is superseded (10 §4.5 + §3.5). The claimed §3.1-vs-§4.5 self-contradiction was retracted.
25. DESIGN §4's later-decisions list gained the 2026-07-11 wave (T-D-09…T-D-16) — seven adopted money-affecting decisions had been invisible on the "binding every slice" page.
26. The stale O3-confirm open closed in 04 (§4.2 + §5) — SEAMS/T-D-02/pricing had closed it 2026-07-28.
27. Slice 11's async-surface inventory gained Billing's periodState transitions (the slice-16 relay) — the D-66 fix class.
28. Duplicate `stackSequence` under `ordered_stack` fails closed (06 §4.2/§4.4 + snapshot minimum) — mixed types make the fold order-dependent (72 vs 70 on the worked example).
29. "usage key ⇒ RatedCharge dedup" → "usage/period key (both unit families)" (11 §4.1). The "no dedup key for a period tick" consequence was retracted — slice 15's index carries it.
30. Validator 3's "material multi-link chain" defined and renamed — **chain-depth cap presence** over the submitted publish unit (depth ≥ 2, no configured cap, no Finance default); the word-collision with pricing's MaterialityEvaluator removed. The ordering and Finance-default objections were refuted during verification.
31. The coupon snapshot minimum gained `valueCurrency` — the `fixed_amount` denomination guard finally has an operand (06 §4.4, 11 §4.5).
32. The four validators got ids (`rating-val-01…04`) + failure codes (`RATING_*`), completing the `ValidatorSpec` shape; codes surface through the pricing pipeline's RFC 9457 report (a Problem-responses block added — the checker's convention).
33. `lineKey` defined (01 §3.1) — the stable billable-component coordinate, shared with subscriptions' SUB-D-19 fact key; the "two prorated halves collapse onto one key" consequence was retracted (priceId differs per slice).
34. The cascade `generation` defined (14 §4.4) — per-unit monotonic, minted per superseding trigger source, coalescing keeps highest.
35. `whole_unit` on a split **fails closed while the open stands** (the T-D-22 posture); `none` on a split bears on the opening slice (09 §4.1/§4.3).
36. The 30-day cap applies **per period** (split fractions clamp so the total ≤ 1; 20+11 ⇒ 30/30). Narrowed: the other half was wrong — the fixed denominator already pinned February at 30/30.
37. A collapsed coincident boundary consults the plan-change flag — the only configurable rule at the cut (03 §4.3, 09 §4.3). Downgraded from the initial framing: the precedence was defensible, one sentence closes it.
38. Under per-window rate-lock FX, period-level floor/cap comparisons convert at the period close-time `fxTableVersion` (09 §4.2). Narrowed to the rate-lock case.
39. The `commitmentReservation` segment is re-composed at the new `balanceVersion` on a T-D-10 cascade — covered by 08 §4.4/14 §4.3 as amended; the "whole cascade is a no-op" claim was retracted.
40. `customerGroup` provenance pinned: the pinned read model's membership subject at context assembly, never caller claims (04 §4.1, 11 §4.2, 14 §4.3). The live-I/O and replay-divergence claims were retracted.
41. The step-4 stack does not reach the reserved portion — stated as intended (04 §4.2, T-D-23 clause). The claimed self-inconsistency was retracted.
42. Slice 12's fallback dedup digest pinned: field list, SHA-256, `digest_key_version = 1`. Latent — the launch source supplies stable ids.
43. `delta_dedup` gained the never-prune rule (15 §3.7); outbox/counters noted exempt.
44. The three stale "§4.1 open" cross-references in 05 fixed (four sites — T-D-09/T-D-10 resolved them 2026-07-11).
45. `TrueUpObligation` gained an explicit `currency` (05 §4.5).
46. The level-aggregation billable unit restated as level unit × granule duration (GB·day at `day`; the D-77 pairing) — began as a claimed 24× mispricing, retracted to wording (the pricing publish gate makes the units agree).

## Minor / hardening (47–52) — all fixed in place

47. The period tick records its pin as an **intent** before evaluating; a crashed re-run reuses it (14 §3.6) — defence in depth behind the slice-16 idempotency.
48. The `period_state` relay: sequence + highest-wins (fixed with #20 / T-D-28).
49. The administrative re-rate got a trigger (`rerate × execute`), an affected-unit enumeration (every unit pinned to the superseded snapshot), an operator confirm, and a fan-out bound (08 §4.1).
50. `usecase-finance-simulation` **deferred to Follow-on as T-D-32** — no owning slice, and a candidate-window evaluation needs a draft read the pin discipline forbids; the partner-overlay "simulate" step rides the same gate.
51. Quarantine replay is an authorized, audited operator action (`usage × replay`; 12 §4.5).
52. The ASC 606 null-at-MVP posture got its dated revisit checkpoint (the D-48 pattern): when Catalog/Contracts ships the `performanceObligationRef` supplier (10 §4.4). The "permanently stranded revenue" claim was retracted.

## Tooling false positives (recorded, no action)

- `P3/code-convention-divergent` on 15:228 — the trigger is `RATED`, a row-status value, not an
  error code; the gear (correctly) has no external synchronous API, so the RFC 9457 rules do
  not bind it. Stays a live finding, recorded here as a known FP.
- The RFC 9457 / GTS error-type convention candidate — retracted for the same no-API-boundary
  reason. Residue kept: the quarantine typed reasons + the three operator signals
  (`max_hold_exceeded`, `cap_exceeded_hard`, `missing_cap_material_chain`) are not enumerated
  as one taxonomy — matters for the joint fixtures, not for compliance.

## Verification & fix record

| # | Tier | Disposition | Where it landed |
|---|------|------------|-----------------|
| 1 | Blocking | direct fix | 03 §4.1 |
| 2 | Blocking | **T-D-23** (veto) | 03 §4.3, 05 §4.1/§4.2, 04 §4.2, 13 §4.3; fixture list |
| 3 | Blocking | **T-D-24** (veto) | 08 §4.1 ×3, 14 §4.3 |
| 4 | Blocking | direct fix (pricing inst-bb-sum adopted) | 10 §4.5 ×2 |
| 5 | High | direct fix | 01 §4.3, 11 §4.7, SEAMS S1, T-D-09 row |
| 6 | High | **T-D-25** (veto) | 05 §3.6/§4.2, 09 §3.6 |
| 7 | High | **T-D-27(1)** (veto, joint Contracts) | 05 §4.1, 15 §4.5, 11 §4.9 |
| 8 | High | **T-D-27(2)** (same) | 05 §4.1, 14 §4.5 |
| 9 | High | direct fix | 13 §3.6 + §4.4 |
| 10 | High | direct fix | 11 §4.7 ×3, 01 §4.3, SEAMS S1, DESIGN §4 |
| 11 | High | direct fix (new contract) | 11 §1.1/§3.3/§3.5/§4.9; PRD §9.2 |
| 12 | High | direct fix | 11 §4.5 + §1.2 row |
| 13 | High | **T-D-29** (pricing D-113) | 09 ×5, 05 §4.5, 03 §4.3, SEAMS P3, PRD ×3 |
| 14 | High | **T-D-30** (pricing D-78) | 04 §4.2 |
| 15 | High | **T-D-31** (pricing D-114) | 11 §3.2/§3.6/§4.2, 14 §2.2/§4.3 |
| 16 | High | direct fix | 03 §4.3, 13 §4.1, PRD `fr-level-aggregation`, T-D-17 row |
| 17 | High | **T-D-26(b)** (veto) | 03 §4.3, 13 §4.1 |
| 18 | High | direct fix | 14 §4.3, 01 §4.2 |
| 19 | High | **T-D-26(a)** (veto) | 09 §4.3 |
| 20 | High | **T-D-28** (veto, joint Billing) | 16 §3.6/§3.7, 08 §4.3, 14 §4.3 |
| 21 | Guideline | direct fix (catalog) | 10 §4.6; 12 §4.5; 08 §4.1 |
| 22 | Guideline | direct fix (convention) | all 16 slices §5; P2 clean |
| 23–46 | Cleanup | direct fixes | per item above |
| 47–52 | Minor | direct fixes (+ **T-D-32**, veto, for #50) | per item above |
