<!-- CONFLUENCE_TITLE: [BSS]: Rating — Design Set -->
<!-- Related: ../DESIGN.md, ../PRD.md, ../SEAMS.md, ../DECISIONS.md, ../ADR/ | Owners: BSS Rating team -->

# Rating — Design Set

<!-- toc -->

- [How to read this set](#how-to-read-this-set)
- [Slice documents](#slice-documents)
- [Where each rule is defined](#where-each-rule-is-defined)

<!-- /toc -->

## How to read this set

[`../DESIGN.md`](../DESIGN.md) is the implementation contract: architecture, dependencies, domain
model, tables, flows A–G, transactions, failure model, observability, and the CURRENT / TARGET /
MIGRATION labels (§0). The slices refine it — slices 01–10 specify the pricing semantics
`rating-core` implements (one per step of the PRD §17.1 order), and slices 11–16 specify the
pipeline mechanics. A slice never restates a rule that DESIGN.md defines; it links to it. Dependency
status lives only in [`../SEAMS.md`](../SEAMS.md) (§L: status of the Seam Atlas v2 baseline in this
repository); decisions only in [`../DECISIONS.md`](../DECISIONS.md).

## Slice documents

| Slice | Crate | Scope |
|---|---|---|
| [`01-foundation.md`](./01-foundation.md) | `rating-core` | Step order, `EvaluationInput`/`EvaluationOutcome`, exact money, determinism, emission guards, error taxonomy |
| [`02-selection-eligibility.md`](./02-selection-eligibility.md) | `rating-core` | Steps 1–2: phase, base row on the 10-axis key, eligibility, cohort, binding cross-check |
| [`03-metering-models.md`](./03-metering-models.md) | `rating-core` | Step 3: meter/dimension mapping, granularity, model formulas, slice band continuity, hourly windows |
| [`04-overlays-precedence.md`](./04-overlays-precedence.md) | `rating-core` | Steps 4–5: overlay stack, contract overlay, composition cap |
| [`05-commitments-reservations.md`](./05-commitments-reservations.md) | `rating-core` | Step 6: reservations, commitment pools (dormant) |
| [`06-coupons.md`](./06-coupons.md) | `rating-core` | Step 7 (no source at launch) |
| [`07-currency-fx.md`](./07-currency-fx.md) | `rating-core` | Step 8: native currency at launch; FX policies when a source exists |
| [`08-retroactivity-corrections.md`](./08-retroactivity-corrections.md) | both | Re-evaluation, revisions, pin-of-record, replay, administrative re-rate |
| [`09-period-plan-change.md`](./09-period-plan-change.md) | `rating-core` | Split points, proration, plan change, floor/cap obligation |
| [`10-governance-asc606.md`](./10-governance-asc606.md) | `rating-core` | Publish-time rules, ASC 606 refs, bundles |
| [`11-consumer-contracts.md`](./11-consumer-contracts.md) | `rating` | Every cross-gear contract, inbox and recovery, order evaluation, test doubles |
| [`12-usage-ingestion-normalization.md`](./12-usage-ingestion-normalization.md) | `rating` | Usage feed, collector → Rating mapping, dedup, invalidations, checkpoint |
| [`13-q-store-attribution.md`](./13-q-store-attribution.md) | `rating` | Meter specs, windows, counters, attribution projection, scope and coverage evidence |
| [`14-unit-synthesis-period-tick.md`](./14-unit-synthesis-period-tick.md) | `rating` | Facts, child windows, scheduler, finalization gate, the rater |
| [`15-rated-output-balance-effects.md`](./15-rated-output-balance-effects.md) | `rating` | Window results, parent roll-up, line identities, exact amounts, snapshots |
| [`16-billing-handoff-operations.md`](./16-billing-handoff-operations.md) | `rating` | Billing delivery, recovery reads, operations, NFR verification |

## Where each rule is defined

| Rule | Canonical location |
|---|---|
| Dependency status and evidence; Atlas status | SEAMS (§L, §M) |
| CURRENT / TARGET labels | DESIGN §0 |
| Child kinds, keys, aggregation key | DESIGN §3.1 |
| Tables, keys, constraints | DESIGN §3.7 |
| Flows A–G | DESIGN §3.6 |
| Pin rule, input freezing, snapshot body, replay | DESIGN §4.1 |
| Transactions and delivery guarantees | DESIGN §4.2 |
| Aggregation windows and worked tier examples | DESIGN §4.3, slice 03 §4.3 |
| Failure handling | DESIGN §4.4; error variants slice 01 §4.4; pending reasons slice 14 §3.1 |
| Scope key, selection, binding cross-check | slice 02 |
| Model formulas | slice 03 §4.1 |
| Split points and proration | slice 09 |
| Finalization gate | slice 14 §4.5 |
| Result identities and exact amounts | slice 15 §4.3, §4.4 |
| Billing delivery contract | slice 16 §4.1 |
