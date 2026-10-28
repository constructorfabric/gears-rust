---
status: proposed
date: 2026-10-12
decision-makers: "BSS Rating team"
---

Created:  2026-10-12 by Virtuozzo International GmbH
Updated:  2026-10-12 by Virtuozzo International GmbH

# ADR-0004: Deliver Absolute Result Revisions per Commercial Fact

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Absolute result revisions (chosen)](#absolute-result-revisions-chosen)
  - [Signed delta entries](#signed-delta-entries)
  - [Rating-owned period fence](#rating-owned-period-fence)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-rating-adr-absolute-revisions`

## Context and Problem Statement

Rating prices commercial facts that Subscriptions publishes. A fact's result changes after it was
first delivered: late or withdrawn usage, a new fact version, a coverage change or an
administrative re-rate all produce a new result for the same fact.

The 2026-09-25 design answered this with two mechanisms:

- **Signed delta entries** (T-D-45): append-only charge entries keyed
  `(unit_id, line_key, to_version)` that carried the difference to the previous result.
- **A Rating-owned period fence** (T-D-47): an `open -> closed` billing-period state in Rating,
  closed by Billing's `close_period`, with deltas after the close routed `post_close`.

Both put Billing's decisions into Rating. The Seam Atlas (D09) assigns period state, invoice
content and correction routing to Billing. The money ADR rounds each invoice line once and computes
a correction from the rounded corrected total (rules 7 and 8), which needs totals, not deltas. No
Billing gear exists yet (R-04, R-05).

What does Rating deliver for a commercial fact when its result changes, and who decides how the
change reaches the invoice?

## Decision Drivers

* Billing owns period state, invoice content, line rounding and credit/debit notes (Atlas D09;
  DECISIONS R-04).
* Line rounding happens once per invoice line, and a correction is the rounded corrected total minus
  the cumulative posted amount ([money ADR](../../../docs/ADR/0001-cpt-cf-bss-adr-exact-decimal-boundaries.md)
  rules 7 and 8). Rounding separately rounded deltas double-counts residues.
* Delivery is at-least-once over an outbox, with pull recovery; the consumer must reach the same
  state whatever it receives twice or misses.
* Rating's hot path must not wait on a cross-gear lock or on the state of a gear that does not exist.

## Considered Options

1. Absolute result revisions per commercial fact (chosen).
2. Signed delta entries (T-D-45).
3. A Rating-owned period fence (T-D-47), used with either delivery form.

## Decision Outcome

Chosen option: **absolute result revisions** (DECISIONS T-D-50; supersedes T-D-45 and T-D-47).

- **Revision.** Each commercial fact (`fact_id`) has a fact head. Roll-up writes a complete exact
  result for the whole fact at `result_revision = head + 1`. `previous_result_revision` is absent on
  revision 1 and equals `result_revision - 1` on every later revision. A `manifest_digest` covers
  the fact version, the expected children and their revisions; an equal digest produces no new
  revision.
- **Delivery.** Each revision is delivered as `BillableItemDeliveryV1` with `complete: true`: every
  line of the fact with its exact amount (ADR-0005), its `invoice_line_key` components and its
  provenance, plus `zero_result` when the fact charges nothing. It is published through the outbox,
  ordered per `fact_id`, and recoverable through `deliveries_since` and `RatingRunReadV1`.
- **Consumer rule.** Billing keys a delivery by `(fact_id, result_revision)` and `manifest_digest`,
  ignores a revision at or below its accepted head, and quarantines a delivery whose
  `previous_result_revision` breaks the chain. Each revision is complete, so a skipped revision is
  harmless.
- **No delta lane, no period fence.** Rating publishes no signed deltas and keeps no billing-period
  state. Period state is not an input to evaluation; `BillingPeriodStateChanged` is an
  observational hint and never gates Rating.
- **Billing's decisions.** Billing compares each revision with its latest accepted target and alone
  decides between replacing a draft and issuing a credit or debit note. It owns line rounding and
  the correction amount (ADR-0005).

### Consequences

* Rating holds no cross-gear fence or period row lock on its hot path, and rounding residues cannot
  be double-counted, because Billing compares rounded aggregates of complete totals.
* Every correction re-sends the whole fact. Payloads grow with the fact's line count, not with the
  size of the change.
* Billing must keep a per-fact accepted head and pending postings (Atlas C08). Until a Billing gear
  exists, the consumer side is a contract proposal (R-04, R-05, R-32).
* T-D-11 (delta dedup) is realized by the revision and delivery identity; Billing derives monetary
  deltas.

### Confirmation

* Roll-up tests: a late usage entry for a final window produces revision `n + 1` with every line of
  the fact; a re-run with the same manifest digest produces no revision.
* A delivery whose `previous_result_revision` is not `result_revision - 1` is rejected by the
  consumer contract test.
* A consumer simulator that receives revisions 1 and 3 but not 2 reaches the same totals as one that
  receives all three.
* Schema review: no Rating table or column stores billing-period state.

## Pros and Cons of the Options

### Absolute result revisions (chosen)

* Good: idempotent and order-tolerant; recovery is "read the latest revision"; matches money ADR
  rules 7 and 8; keeps Billing's decisions in Billing.
* Bad: larger payloads per correction; Billing must keep accepted heads per fact.

### Signed delta entries

* Good: small payloads; a ledger-like append-only log.
* Bad: the consumer must apply every delta exactly once and in order, so a lost or duplicated entry
  changes the invoice. Rounding each delta separately leaves residues that a later delta can count
  twice. A correction cannot be computed as rule 8 requires without rebuilding the total.

### Rating-owned period fence

* Good: Rating could route late changes itself (`post_close`).
* Bad: Rating would own a Billing state (Atlas D09), take a period row lock on its hot path and
  depend on a `close_period` call from a gear that does not exist. A fence decision made in Rating
  can disagree with Billing's own draft and posting state.

## More Information

- Seam Atlas v2 contract baseline 1.1: D09 (absolute replacement), C07 (delivery), C08 (Billing
  accepted heads); see [`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) §3.
- Delivery DTO: [DESIGN §3.3](../DESIGN.md#33-api-contracts); delivery semantics:
  [DESIGN §4.2](../DESIGN.md#42-transactions-idempotency-delivery-semantics); roll-up and delivery
  behaviour: [`../features/09-rollup-delivery.md`](../features/09-rollup-delivery.md).

## Traceability

- **Decisions**: [`../DECISIONS.md`](../DECISIONS.md) T-D-50 (this decision), T-D-45 and T-D-47
  (superseded), T-D-11 (amended), R-04, R-05, R-32.
- **Design**: [DESIGN §3.3](../DESIGN.md#33-api-contracts) (`BillableItemDeliveryV1`),
  [DESIGN §4.1](../DESIGN.md#41-versioning-and-historical-correctness) (period state is not an
  input), [DESIGN §4.2](../DESIGN.md#42-transactions-idempotency-delivery-semantics).
- **Related ADRs**: [ADR-0005](./0005-cpt-cf-bss-rating-adr-exact-money.md) (exact amounts and
  Billing's rounding).
