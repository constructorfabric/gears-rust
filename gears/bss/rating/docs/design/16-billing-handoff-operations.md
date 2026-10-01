Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-10-01 by Virtuozzo International GmbH

<!-- CONFLUENCE_TITLE: [BSS]: Rating — Billing Delivery, Recovery & Operations (Design) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../SEAMS.md | Upstream: 15-rated-output-balance-effects | Downstream: Billing (no gear yet) | Owners: BSS Rating team -->

# DESIGN — Billing Delivery, Recovery & Operations (Slice 16, pipeline)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-billing-handoff`

<!-- toc -->

- [1. Architecture Overview](#1-architecture-overview)
  - [1.1 Architectural Vision](#11-architectural-vision)
  - [1.2 Architecture Drivers](#12-architecture-drivers)
  - [1.3 Architecture Layers](#13-architecture-layers)
- [2. Principles and Constraints](#2-principles-and-constraints)
  - [2.1 Design Principles](#21-design-principles)
  - [2.2 Constraints](#22-constraints)
- [3. Technical Architecture](#3-technical-architecture)
  - [3.1 Domain Model](#31-domain-model)
  - [3.2 Component Model](#32-component-model)
  - [3.3 API Contracts](#33-api-contracts)
  - [3.4 Internal Dependencies](#34-internal-dependencies)
  - [3.5 External Dependencies](#35-external-dependencies)
  - [3.6 Interactions and Sequences](#36-interactions-and-sequences)
  - [3.7 Database Schemas and Tables](#37-database-schemas-and-tables)
  - [3.8 Deployment Topology](#38-deployment-topology)
- [4. Additional Context](#4-additional-context)
  - [4.1 Billing Delivery Contract (normative)](#41-billing-delivery-contract-normative)
  - [4.2 Period State Is Billing's (normative)](#42-period-state-is-billings-normative)
  - [4.3 Operational Topology (normative)](#43-operational-topology-normative)
  - [4.4 Backpressure, Replay, Cold Start (normative)](#44-backpressure-replay-cold-start-normative)
  - [4.5 NFR Verification (normative)](#45-nfr-verification-normative)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

The outbound edge. Every parent result produces one `BillableItemDeliveryV1` row in
`rating_delivery`, committed with the result (slice 15). Billing receives it as an event when a
broker delivers (TARGET) and can always pull it — `RatingRunReadV1::deliveries_since` for the
stream, `find_runs` / `get_run` for recovery by business key (Atlas C07, P8). Rating never calls the
ledger, keeps no period fence and never reads invoice state (T-D-45).

This slice also owns the gear's operational topology and the NFR verification plan.

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design response |
|---|---|
| `cpt-cf-bss-rating-contract-billing-periodstate` | Billing owns period state; Rating publishes complete revisions and accepts a period hint only for observability (§4.2). |
| `cpt-cf-bss-rating-fr-posted-period-protection` | Revisions carry `previous_result_revision`; Billing derives credit/debit from aggregate targets (§4.1). |
| `cpt-cf-bss-rating-fr-period-floor-cap-obligation` | Obligations travel on the delivery; Billing executes (R-22). |

#### NFR Allocation

| NFR | Design response |
|---|---|
| `cpt-cf-bss-rating-nfr-throughput-latency` | Verification plan §4.5. |
| `cpt-cf-bss-rating-nfr-resilience` | Outbox delivery, replayable pull feed, recovery by business key. |

### 1.3 Architecture Layers

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-tech-stack-bhf`

`rating` crate: `infra/delivery.rs` (outbox handler: sequencer + optional event publisher),
`api/local_client.rs` (`RatingRunReadV1`, `RatingRunControlV1`, `OrderEvaluationV1`),
`api/rest/*` (operator plane).

## 2. Principles and Constraints

### 2.1 Design Principles

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-precision-out-bhf`
  Deliveries carry exact amounts; Billing rounds per `invoice_line_key` aggregate (T-D-46).
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-idempotent-delivery-bhf`
  Every delivery is replayable and self-describing; a consumer that misses one recovers it by
  business key.
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-lanes-bhf`
  New rating, corrections and administrative re-rates share the child queue with coalescing; a
  re-rate run enqueues at a bounded rate (`rerate_enqueue_per_second`, default 50); there is one
  delivery lane (no separate adjustment lane — Atlas D09).

### 2.2 Constraints

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-no-round-bhf`
  Rating never rounds and never applies period floor/cap.
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-periodstate-billing-bhf`
  Billing period state is never an input to rating or delivery; a missing hint blocks nothing.

## 3. Technical Architecture

### 3.1 Domain Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-domain-model-bhf`

- **`BillableItemDeliveryV1`** — DESIGN §3.3.
- **`RatingRunView`** — `{run_id, fact_id, fact_version, result_revision, state: pending | running |
  succeeded | failed, pending_reasons[], manifest, delivery_id?}`; a fact without a complete result
  reports `pending` with the reasons of its pending children.
- **`BillingPeriodHint`** — `{billing_group_id, state: open | frozen | posted, invoice_id?,
  state_version}` (Atlas C09); observational.

### 3.2 Component Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-component-billing-handoff`

| Component | Responsibility |
|---|---|
| `DeliverySequencer` | `toolkit_db::outbox` handler: assigns the per-tenant `feed_seq` after commit. |
| `DeliveryEventPublisher` | TARGET: publishes the delivery via `cf-gears-event-broker-sdk` `DbProducer`; marks `published_via_event_at`. Disabled while no broker delivers (SEAMS A-3). |
| `RatingRunClientLocal` | Implements the Rating SDK traits, registered in ClientHub. |
| `BillingHintSink` | Stores `BillingPeriodStateChanged` hints. |
| `OperatorApi` | REST plane (DESIGN §3.3). |

### 3.3 API Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-interface-billing-handoff-bhf`
  DESIGN §3.3. Errors (`CanonicalError`): `NotFound` (unknown fact/run/snapshot), `PermissionDenied`,
  `InvalidArgument` (cursor from another tenant/filter), `Unavailable`. Page tokens are opaque and
  bound to tenant and filter (Atlas C00).

### 3.4 Internal Dependencies

Reads `rating_delivery`, `rating_fact_result`, `rating_window_result`, `rating_child_window`.

### 3.5 External Dependencies

Billing (consumer, no gear — R-05, J-5); event broker (TARGET, A-3).

### 3.6 Interactions and Sequences

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-deliver-bhf`
  **Billing consumes deliveries** (Billing's side, shown for the contract — **PROPOSED — NOT YET
  IMPLEMENTED**, Atlas C08; no Billing gear exists):

  ```text
  page = rating.deliveries_since(ctx, T10, after_seq = billing_checkpoint, 500)   (or the event)
  for d in page:
    head = billing.fact_head(d.fact_id)
    d.result_revision ≤ head.revision → ignore (same digest) | quarantine (different digest)
    else: replace the fact's exact contributions with d.lines
          group target = Σ exact contributions per invoice_line_key over the billing group
          round each aggregate once, HALF_EVEN, to integer minor units
          group draft   → replace the draft line
          group frozen  → posting obligations for the rounded difference vs the latest accepted
                          target, including pending postings (credit/debit notes)
  billing_checkpoint = page.last_seq   (committed with Billing's own transaction)
  ```

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-periodstate-relay-bhf`
  **Recovery** (Atlas P8; Billing side PROPOSED): Billing notices an overdue group (a sealed set with a fact that has no
  complete delivery) and calls `find_runs(subscription_id, billing_group_id)`; the response carries
  the latest complete result per fact or its pending reasons. Re-read deliveries enter Billing's
  inbox with the same identity, so nothing doubles.

### 3.7 Database Schemas and Tables

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-storage-billing-handoff-bhf`
  `rating_delivery`, `rating_billing_hint` — DESIGN §3.7.

### 3.8 Deployment Topology

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-deployment-bhf`
  See §4.3.

## 4. Additional Context

### 4.1 Billing Delivery Contract (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-billing-delivery-bhf`

- One delivery per parent result revision; `complete = true` always; a finalized zero fact delivers
  `lines = [], zero_result = true` (Atlas C07) — Billing needs it to freeze.
- Per fact, `result_revision` increases; a consumer may skip revisions (each is a full replacement).
- Lines carry `line_key`, `invoice_line_key`, exact amount, GL/tax/template classification from the
  pinned pricing documents (`descriptorSet.glCode`, `taxCategoryRef`, `invoiceLineTemplate`), and
  provenance down to price id, slice and window.
- `billing_group_kind = derived` (MIGRATION R-20) tells Billing the group key was derived by Rating
  from `(seller, payer, subscription, currency, period_start)`; TARGET groups come from the fact.
- `evidence_mode = delay_only` (MIGRATION R-21) is carried so Billing and Finance can see which
  deliveries were finalized without completeness evidence.
- Ledger mapping (Billing's, PROPOSED): one invoice item per rounded `invoice_line_key` aggregate;
  `invoice_item_ref = invoice_line_key`; `amount_minor_ex_tax` = the rounded aggregate;
  `pricing_snapshot_ref` = the composite snapshot of the contributing parent line (DESIGN §4.1);
  when several facts contribute to one aggregate, Billing either splits the ledger item per fact or
  picks a reference, and `price_id` is Billing's choice when the aggregate spans several prices —
  both open (R-26).
  The ledger forbids negative invoice items; a net-negative revision on a posted invoice is a credit
  note (SEAMS L-2, L-4).

### 4.2 Period State Is Billing's (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-periodstate-relay-bhf`

- Rating has no `open/closed` period and no `close_period` command (superseded, T-D-42 → T-D-45).
- `BillingPeriodStateChanged` (TARGET, Atlas C09) is stored in `rating_billing_hint` and exposed on
  dashboards (`rating_post_final_revisions_total{group_state}`); it never gates evaluation,
  finalization or delivery (Atlas D09).
- Late usage after Billing froze or posted a group is handled exactly like any correction: a new
  child revision and a new parent revision (slice 08).

### 4.3 Operational Topology (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-lanes-bhf`

| Task | Scale unit | Coordination |
|---|---|---|
| `UsageFeedReader` | `(tenant, gts_type)` | lease + checkpoint CAS |
| Recovery sweeps (`period_facts`, `segments_since`) | tenant shard | lease `rating:recovery:{shard}` + CAS |
| Event consumers (TARGET) | consumer group | DB-committed offsets |
| `WindowScheduler` | shard | lease `rating:scheduler:{shard}` + cursor CAS |
| `Rater` | queue partition | leased queue |
| `ParentRollup` | queue partition | leased queue + fact head row lock |
| `DeliverySequencer` / publisher | tenant partition | library |
| `Reconciler` | tenant shard | lease `rating:reconcile:{shard}` |
| exception sweeper (15 min) | 1 | lease `rating:exception-sweep` |

### 4.4 Backpressure, Replay, Cold Start (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-backpressure-replay-bhf`

- Backpressure: feed readers stop above `max_queue_depth` and resume below 80 %; provisional
  evaluations are skipped under backlog (slice 14 §4.4); nothing is dropped.
- Replay: any feed or inbox source can be re-read (idempotent); any final result can be re-evaluated
  from stored inputs (DESIGN §4.1); any delivery can be re-published or re-pulled.
- Cold start: caches start empty and fall back to `rating_catalog_document` /
  `rating_subscription_version`, then to the upstream. Correctness never depends on a warm cache.

### 4.5 NFR Verification (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-nfr-verification-bhf`

| Test | Pass criterion |
|---|---|
| Sustained load | 10M usage records/day/region (≈ 116/s mean, 1 000/s peak for 1 h); feed lag < 5 min; provisional rating lag p95 < 5 min |
| Hourly finalization burst | 100 000 hourly lines due at one `HH:00 + delay`: all children final (given evidence) within 15 min |
| Rater latency | `rating_rating_duration_seconds` p95 < 1 s at peak |
| Catalog lookup | cached document read p95 ≤ 100 ms incl. cold `rating_catalog_document` reads |
| Crash/retry | kill readers, scheduler, raters and roll-up at random under load; counter integrity, expected-child completeness and delivery coverage report zero mismatches; no duplicate revision |
| Scheduler outage | 3 h scheduler downtime: catch-up produces every due child once (F28) |
| Replay | re-read 7 days of feed and inbox; zero new records, results or deliveries |
| Determinism | re-evaluate 10 000 random final results; identical `input_digest` and amounts |
| Re-rate storm | an administrative re-rate of 1M children keeps normal rating lag p95 < 5 min |

## 5. Traceability

- **DESIGN**: §3.3, §3.6 (Flow G), §3.8, §4.6, §4.9.
- **Decisions**: T-D-45, T-D-46, T-D-51, R-04, R-05, R-13, R-20, R-21, R-22.
- **Atlas**: C07, C08, C09, D09, D10, P8.
