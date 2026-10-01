# Feature: Usage Capture and Normalization

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-featstatus-usage-intake-implemented`

- [ ] `p1` - `cpt-cf-bss-rating-feature-usage-intake`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Read and capture one feed page](#21-read-and-capture-one-feed-page)
  - [2.2 Apply an invalidation entry](#22-apply-an-invalidation-entry)
  - [Migrated namespace flows](#migrated-namespace-flows)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Capture before interpretation](#31-capture-before-interpretation)
  - [3.2 Interpret a stored entry](#32-interpret-a-stored-entry)
  - [3.3 Maintain the canonical source and its checkpoint](#33-maintain-the-canonical-source-and-its-checkpoint)
  - [3.4 Record and keep a source loss](#34-record-and-keep-a-source-loss)
  - [3.5 Apply backpressure](#35-apply-backpressure)
  - [Migrated namespace procedures](#migrated-namespace-procedures)
- [4. States (CDSL)](#4-states-cdsl)
  - [4.1 Capture side of the contribution state](#41-capture-side-of-the-contribution-state)
  - [4.2 Source loss](#42-source-loss)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Lossless, idempotent page capture](#51-lossless-idempotent-page-capture)
  - [5.2 Retryable normalization and invalidation intake](#52-retryable-normalization-and-invalidation-intake)
  - [5.3 Source identity, source loss and backpressure](#53-source-identity-source-loss-and-backpressure)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Detailed Behavior Contracts](#7-detailed-behavior-contracts)
  - [Usage ingestion: Interactions and Sequences](#usage-ingestion-interactions-and-sequences)
  - [Usage ingestion: Invalidations, Replacements and Negative Quantities (normative)](#usage-ingestion-invalidations-replacements-and-negative-quantities-normative)
  - [Usage ingestion: Exceptions (normative)](#usage-ingestion-exceptions-normative)
  - [Usage ingestion: Feed Checkpoint, Retention and Backpressure (normative)](#usage-ingestion-feed-checkpoint-retention-and-backpressure-normative)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Read the usage collector's feed per canonical source, store every entry complete before interpreting
it, interpret the stored copy into a normalized usage record or a recorded rejection, withdraw the
targets of invalidation entries, and record a durable source loss whenever the feed cannot prove that
nothing was lost.

### 1.2 Purpose

Usage is financial input that the collector keeps only for its retention floor (125 days). Rating
must be able to rate, correct and replay it for seven years, so capture is lossless and comes first;
interpretation can be retried from the stored copy at any time; and an unprovable gap blocks
finality instead of silently under-billing (T-D-61, T-D-62).

**Requirements** (primary, per DECOMPOSITION): `cpt-cf-bss-rating-fr-dimension-population-contract`. **Supporting**: `cpt-cf-bss-rating-fr-idempotency`, `cpt-cf-bss-rating-nfr-resilience`, `cpt-cf-bss-rating-nfr-throughput-latency`.

**Principles**: `cpt-cf-bss-rating-principle-capture-before-interpret`, `cpt-cf-bss-rating-principle-copy-what-you-rate`, `cpt-cf-bss-rating-principle-evidence-before-final`, `cpt-cf-bss-rating-principle-normalize-once-ing`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-bss-rating-actor-oss-metering` | Emits the usage entries and their declared metadata, through the usage collector |
| `cpt-cf-bss-rating-actor-rating` | Runs the feed readers and the page transaction under its service identity |
| `cpt-cf-bss-rating-actor-platform-operator` | Retries rejected entries and resolves source losses (audited; operations feature) |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §6.4 (meter mapping, dimension population), §7.1, §13 (usage feed).
- **Architecture**: [DESIGN.md](../DESIGN.md) §3.6 Flow A (page transaction, contribution state machine), Flow B (invalidation), §3.7 (`bss_rating__source_checkpoint`, `bss_rating__usage_record`, `bss_rating__source_loss`), §4.2, §4.4; namespace [12](../DESIGN.md#contract-12) — [mapping](../DESIGN.md#contract-12-4-1), [attribution rule](../DESIGN.md#contract-12-4-2), [dedup](../DESIGN.md#contract-12-4-3), [usage type declarations](../DESIGN.md#contract-12-4-7). Complete runtime procedures are in [§7](#7-detailed-behavior-contracts).
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.4.
- **Dependencies**: [Foundation](01-foundation.md) (`cpt-cf-bss-rating-feature-foundation`): exception register, Cluster lease port, tenancy scopes, the feed contract fake.
- **Consumers**: [Attribution and Window Counters](05-attribution-counters.md) attributes and counts what this feature stores; [Operations](11-operations.md) reconciles, retries and resolves.
- **Upstream**: `cpt-cf-bss-rating-upreq-usage-feed-v1`, `cpt-cf-bss-rating-upreq-usage-type-declaration-read`, `cpt-cf-bss-rating-upreq-dimension-encoding` ([UPSTREAM_REQS](../UPSTREAM_REQS.md) §2.1, §2.3, §2.4).

**UI applicability**: none; this feature has no endpoint and no user interface. Operator surfaces are in [Operations](11-operations.md).

## 2. Actor Flows (CDSL)

**Use cases**: none in the PRD; the flows are system flows driven by the usage collector's feed.

### 2.1 Read and capture one feed page

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-intake-read-page`

**Actor**: `cpt-cf-bss-rating-actor-rating` (feed reader for one canonical source)

**Success Scenarios**: every entry of the page is captured once with its digest and interpreted; the checkpoint advances in the same transaction.

**Error Scenarios**: feed unavailable (backoff); `CursorBeyondRetention` (source loss, restart from `Oldest`); lost lease (checkpoint CAS fails, rollback); queue depth above the backpressure threshold (reader pauses).

**Steps**:
1. [ ] - `p1` - Hold the Cluster lock `bss-rating/usage-feed/{source_id}` for the canonical source; without it, do not read - `inst-rp-lock`
2. [ ] - `p1` - Read the checkpoint and call `read_usage_feed(subscription, Oldest | After(cursor), until = None, limit ≤ 1 000)` outside the transaction - `inst-rp-read`
3. [ ] - `p1` - **IF** the collector answers `CursorBeyondRetention` - `inst-rp-retention`
   1. [ ] - `p1` - Open a source loss per affected tenant and restart the source from `Oldest` in one transaction (§3.4) - `inst-rp-open-loss`
4. [ ] - `p1` - BEGIN; lock the checkpoint row and check the cursor and fence (CAS) - `inst-rp-cas`
5. [ ] - `p1` - **FOR EACH** entry in feed order: capture it (§3.1), then interpret the stored row (§3.2) or apply it as an invalidation (§2.2) - `inst-rp-each`
6. [ ] - `p1` - Advance the checkpoint cursor; COMMIT - `inst-rp-commit`
7. [ ] - `p1` - **IF** the page is short or empty, poll again after `rating.usage_feed.poll_interval` - `inst-rp-poll`

The page transaction is canonical in DESIGN §3.6 Flow A; this flow names the steps this feature implements.

### 2.2 Apply an invalidation entry

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-intake-invalidation`

**Actor**: `cpt-cf-bss-rating-actor-rating` (feed reader)

**Success Scenarios**: the invalidation is captured once; its target is withdrawn exactly once; a counted target's contribution is removed at its recorded coordinates by the counters feature.

**Error Scenarios**: target not captured yet (`invalidation_target_unknown`, withdrawn on later capture); replayed invalidation (absorbed by its unique key).

**Steps**:
1. [ ] - `p1` - Capture the invalidation entry like any entry, unique on `(tenant_id, invalidates_id)` - `inst-iv-capture`
2. [ ] - `p1` - Withdraw the target with the conditional update `withdrawn_by IS NULL`, returning its previous state and recorded coordinates - `inst-iv-withdraw`
3. [ ] - `p1` - **IF** the previous state was `counted`, hand the coordinates to the counter decrement of [Attribution and Window Counters](05-attribution-counters.md) in the same transaction - `inst-iv-decrement`
4. [ ] - `p1` - **IF** the target is not stored, keep the invalidation and open `invalidation_target_unknown` - `inst-iv-unknown`
5. [ ] - `p1` - **RETURN** with no delta computed: corrections become new revisions downstream - `inst-iv-return`

Canonical: DESIGN §3.6 Flow B steps 1–2 and [12 §4.4](#contract-12-4-4).

<a id="register-flows"></a>

### Migrated namespace flows

The flows of the former slice documents that this feature implements are defined here and specified in [§7](#7-detailed-behavior-contracts); each entry links to its contract.

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-ingest-ing`
  — Usage ingestion — Interactions and Sequences ([contract](#contract-12-3-6))

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-correction-intake-ing`
  — Usage ingestion — Interactions and Sequences ([contract](#contract-12-3-6))

## 3. Processes / Business Logic (CDSL)

### 3.1 Capture before interpretation

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-intake-capture`

**Input**: one wire entry from a feed page, the canonical `source_id`.

**Output**: a captured row, an "already captured" no-op, a content-conflict alarm, or a capture reject.

1. [ ] - `p1` - **IF** the entry has no readable `id` or `tenant_id`, insert `bss_rating__usage_capture_reject (source_id, raw_sha256, raw)` and open a source loss of kind `unidentifiable_entry` over the source scope - `inst-cap-unidentifiable`
2. [ ] - `p1` - Insert `bss_rating__usage_record` with `raw_entry`, `raw_sha256`, `contribution_state = captured`, `ON CONFLICT DO NOTHING` - `inst-cap-insert`
3. [ ] - `p1` - **IF** not inserted and the stored digest is equal, stop: the entry has no further effect - `inst-cap-duplicate`
4. [ ] - `p1` - **IF** not inserted and the digest differs, raise `usage_content_conflict`; the stored row wins - `inst-cap-conflict`
5. [ ] - `p1` - **RETURN** the stored row for interpretation in the same transaction - `inst-cap-return`

### 3.2 Interpret a stored entry

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-intake-normalize`

**Input**: a stored `bss_rating__usage_record` row (from capture, or from an operator retry).

**Output**: a normalized record handed to attribution, or `rejected(reason)` with an exception.

1. [ ] - `p1` - **IF** a stored invalidation already names this record, set `withdrawn` and stop - `inst-norm-prewithdrawn`
2. [ ] - `p1` - Map the entry per [12 §4.1](../DESIGN.md#contract-12-4-1): interval, quantity (exact, ≤ 28 fractional digits), `gts_type`, resource and subject references, metadata, origin - `inst-norm-map`
3. [ ] - `p1` - **IF** the entry is malformed or has no interval, set `rejected(usage_malformed | interval_missing)` and open an exception - `inst-norm-malformed`
4. [ ] - `p1` - Read the usage type declaration; **IF** its fold is not `SUM`, set `rejected(usage_type_not_chargeable)` ([12 §4.7](../DESIGN.md#contract-12-4-7)) - `inst-norm-fold`
5. [ ] - `p1` - Keep `gts_type` as the record's **raw input usage type** (a usage SKU sells a Products derived usage type whose inputs are raw GTS types, products P-D-259; T-D-76) and derive `dimension_key` from declared metadata (R-16: the empty key only at launch) - `inst-norm-dimension`
6. [ ] - `p1` - **RETURN** the normalized record to the attribution step of [Attribution and Window Counters](05-attribution-counters.md) - `inst-norm-return`

A rejection never blocks the feed: the raw entry is already stored, so a later retry re-runs this process without the collector ([12 §4.5](#contract-12-4-5)).

### 3.3 Maintain the canonical source and its checkpoint

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-intake-source-identity`

**Input**: the configured feed subscription (sorted GTS types), Rating's service identity and its granted tenant scope.

**Output**: one `source_id` naming the configuration, lock, checkpoint, metrics and losses.

1. [ ] - `p1` - Compute `source_id = "usage:" + UUIDv5(NS_RATING_SOURCE, canonical JSON {sorted gts_type_ids, service_identity, granted tenant scope})` - `inst-src-id`
2. [ ] - `p1` - **IF** the type set or grant changed, create the new source from `Oldest` and drain the old one to its current end, then retire it - `inst-src-change`
3. [ ] - `p1` - **IF** a widened grant exposes history already past the retention floor, open `scope_widened_beyond_retention` for that tenant - `inst-src-widened`
4. [ ] - `p1` - Never send a cursor with any subscription other than the one that produced it - `inst-src-cursor`

Details: [12 §4.6](#contract-12-4-6).

### 3.4 Record and keep a source loss

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-intake-source-loss`

**Input**: `CursorBeyondRetention`, an unidentifiable entry, or a scope widened beyond retention.

**Output**: an `open` `bss_rating__source_loss` row per affected tenant that blocks finality of its scope.

1. [ ] - `p1` - Insert the loss with `kind`, `gts_type_ids`, `last_good_cursor`, `restart_cursor` and `tainted_days` = every day of the scope from the earliest day not yet final - `inst-sl-open`
2. [ ] - `p1` - Bump `input_generation` of the affected children so the evidence gate re-checks them - `inst-sl-bump`
3. [ ] - `p1` - Never resolve the loss here: only reconciliation evidence or an audited operator resolution does ([Operations](11-operations.md)) - `inst-sl-no-auto`

### 3.5 Apply backpressure

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-algo-intake-backpressure`

**Input**: `rating.child_work` queue depth.

**Output**: the reader pauses or resumes; nothing is dropped.

1. [ ] - `p1` - **IF** depth exceeds `max_queue_depth` (default 1 000 000), stop fetching pages - `inst-bp-stop`
2. [ ] - `p1` - **IF** depth falls below 80 % of the threshold, resume from the stored cursor - `inst-bp-resume`

<a id="register-procedures"></a>

### Migrated namespace procedures

The normative procedures of the former slice documents that this feature implements are defined here and specified in [§7](#7-detailed-behavior-contracts); each entry links to its contract.

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-correction-intake-ing`
  — Usage ingestion — Invalidations, Replacements and Negative Quantities (normative) ([contract](#contract-12-4-4))

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-quarantine-ing`
  — Usage ingestion — Exceptions (normative) ([contract](#contract-12-4-5))

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-feed-checkpoint-ing`
  — Usage ingestion — Feed Checkpoint, Retention and Backpressure (normative) ([contract](#contract-12-4-6))

## 4. States (CDSL)

### 4.1 Capture side of the contribution state

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-state-intake-capture`

**States**: `captured`, `rejected`, `withdrawn` (this feature's transitions of the contribution state machine defined in DESIGN §3.6 Flow A; deferred states and `counted` belong to [Attribution and Window Counters](05-attribution-counters.md)).

**Initial State**: `captured`.

**Transitions**:
1. [ ] - `p1` - **FROM** — **TO** `captured` **WHEN** an entry is stored for the first time - `inst-st-capture`
2. [ ] - `p1` - **FROM** `captured` **TO** `rejected(reason)` **WHEN** normalization or the fold check fails - `inst-st-reject`
3. [ ] - `p1` - **FROM** any non-`withdrawn` state **TO** `withdrawn` **WHEN** its invalidation is applied (`withdrawn_by` set once) - `inst-st-withdraw`
4. [ ] - `p1` - **FROM** — **TO** `withdrawn` **WHEN** a record is captured after its invalidation - `inst-st-late-target`

### 4.2 Source loss

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-state-intake-source-loss`

**States**: `open`, `narrowed`, `resolved`.

**Initial State**: `open`.

**Transitions**:
1. [ ] - `p1` - **FROM** `open` **TO** `narrowed` **WHEN** reconciliation matches some tainted days - `inst-sls-narrow`
2. [ ] - `p1` - **FROM** `open` or `narrowed` **TO** `resolved(repaired)` **WHEN** every tainted day matches after re-delivery - `inst-sls-repaired`
3. [ ] - `p1` - **FROM** `open` or `narrowed` **TO** `resolved(accepted_loss)` **WHEN** an audited `source_loss × resolve` records evidence - `inst-sls-accepted`

## 5. Definitions of Done

### 5.1 Lossless, idempotent page capture

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-intake-capture`

The system **MUST** store every feed entry complete with its SHA-256 before interpreting it, commit a page's captures, their effects and the checkpoint advance in one transaction with the cursor CAS and lease fence, and treat an already-captured entry with the same digest as having no effect at all.

**Implements**: `cpt-cf-bss-rating-flow-intake-read-page`, `cpt-cf-bss-rating-algo-intake-capture`, `cpt-cf-bss-rating-state-intake-capture`, `cpt-cf-bss-rating-flow-ingest-ing`.

**Constraints**: `cpt-cf-bss-rating-constraint-authoritative-dedup-ing`, `cpt-cf-bss-rating-constraint-entries-not-aggregates-ing`, `cpt-cf-bss-rating-constraint-in-process-contracts`.

**Touches**: `read_usage_feed`; `cpt-cf-bss-rating-dbtable-feed-cursor`, `cpt-cf-bss-rating-dbtable-usage-record`; Cluster lock `bss-rating/usage-feed/{source_id}`.

### 5.2 Retryable normalization and invalidation intake

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-intake-normalize`

The system **MUST** interpret only stored entries, reject malformed, interval-less and non-`SUM` entries into the exception register without blocking the feed, populate `meter` and `dimension_key` per [12 §4.1](../DESIGN.md#contract-12-4-1), and apply invalidations through the conditional withdrawal so a withdrawn record can never contribute again.

**Implements**: `cpt-cf-bss-rating-algo-intake-normalize`, `cpt-cf-bss-rating-flow-intake-invalidation`, `cpt-cf-bss-rating-flow-correction-intake-ing`, `cpt-cf-bss-rating-algo-correction-intake-ing`, `cpt-cf-bss-rating-algo-quarantine-ing`.

**Constraints**: `cpt-cf-bss-rating-constraint-no-price-ing`, `cpt-cf-bss-rating-constraint-utc-units-ing`.

**Touches**: `cpt-cf-bss-rating-dbtable-usage-record`, `cpt-cf-bss-rating-dbtable-exception`; types-registry usage type declarations.

### 5.3 Source identity, source loss and backpressure

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-intake-source-loss`

The system **MUST** derive one canonical `source_id` per feed subscription, start a changed subscription as a new source from `Oldest`, open a durable source loss for `CursorBeyondRetention`, unidentifiable entries and widened scopes, keep it open until reconciliation or an audited resolution closes it, and pause reading under backpressure without dropping anything.

**Implements**: `cpt-cf-bss-rating-algo-intake-source-identity`, `cpt-cf-bss-rating-algo-intake-source-loss`, `cpt-cf-bss-rating-algo-intake-backpressure`, `cpt-cf-bss-rating-algo-feed-checkpoint-ing`, `cpt-cf-bss-rating-state-intake-source-loss`.

**Constraints**: `cpt-cf-bss-rating-constraint-authoritative-dedup-ing`.

**Touches**: `cpt-cf-bss-rating-dbtable-source-loss`, `cpt-cf-bss-rating-dbtable-feed-cursor`, `bss_rating__usage_capture_reject`.

Observability: this feature emits `rating_feed_lag_seconds`, `rating_feed_cursor_age_seconds`, `rating_feed_oldest_unread_age_seconds`, `rating_feed_restart_total`, `rating_source_loss_open`, `rating_usage_rejected_total{reason}`, `rating_usage_duplicate_total`, `rating_usage_content_conflict_total` and the `rating.feed.page` span (DESIGN §4.7).

## 6. Acceptance Criteria

- [ ] A re-read entry with a different digest raises `usage_content_conflict` and keeps the stored row.
- [ ] A malformed entry is `rejected` and the cursor advances past it; the feed is never blocked by an exception.
- [ ] A re-read entry with the same digest is not normalized again.
- [ ] A non-`SUM` usage type is `rejected(usage_type_not_chargeable)`; an entry with a non-empty `dimension_key` fails closed until R-16 is decided.
- [ ] An invalidation of a counted record withdraws it once; a replayed invalidation is a no-op; an invalidation whose target is captured later withdraws the target on capture.
- [ ] `CursorBeyondRetention` under any evidence mode opens a source loss and restarts from `Oldest`; the loss stays `open` after the restart.
- [ ] Above `max_queue_depth` the reader stops fetching and resumes below 80 %; no entry is dropped.

Acceptance vectors owned by this feature (DESIGN §4.13 index):

| # | Scenario | Expected state |
|---|---|---|
| V01 | The same page is applied twice (crash after commit, re-read) | `Q`, `q_version` and contribution counts unchanged |
| V03 | Malformed entry, collector retention then expires, operator retries after a fix | the stored raw entry is re-interpreted and counted once |
| V04 | Crash after capture, before commit | nothing persisted; re-read captures once |
| V07 | Type set of a feed subscription changes | new `source_id` from `Oldest`; the old cursor is never used with the new filter |

## 7. Detailed Behavior Contracts

**Contract namespace 12.** Section numbers inside the detailed contracts below resolve through the [contract address index](../DECOMPOSITION.md#4-contract-address-index), not the overview section numbers above. A bare section reference names a section of the same namespace; "DESIGN §n" names §1–§5 of [DESIGN.md](../DESIGN.md).

The following sections keep the runtime procedures of these namespaces — their interactions and sequences and the procedural parts of their §4. The flows, processes and acceptance criteria above summarize them; the architecture, schemas and evaluation semantics they rely on are defined once in [DESIGN.md](../DESIGN.md) §6.

<a id="contract-12-3-6"></a>

<!-- contract:12-usage-ingestion-normalization:3.6 -->
### Usage ingestion: Interactions and Sequences

**Contract**: `cpt-cf-bss-rating-flow-ingest-ing` (`p1`), defined in [§2 migrated flows](#register-flows).
  The page transaction is DESIGN §3.6 Flow A. Example (hourly cloudlets):

  ```text
  entry   id=0192…a1  entry_type=record  tenant=T10  gts_type_id=gts.cf.oss.cloudlet_hours.v1~ (illustrative)
          resource_ref={ct-7f3a, container}  window=[2026-10-08T10:00Z, 2026-10-08T11:00Z)
          quantity="8"  accepted_at=2026-10-08T11:00:07Z  origin=live
  attribution ct-7f3a @ window → segment SEG-4 v2 → subscription S42, sub_line_key plan#1,
          payer C1, seller S1
  capture bss_rating__usage_record(T10, 0192…a1) raw_entry, raw_sha256, state captured
  stored  contrib_counter_key_digest=digest(CounterKey), contrib_input_usage_type=
          gts.cf.oss.cloudlet_hours.v1~, contrib_layout_version=1,
          contrib_part_start=2026-10-08T10:00Z, contrib_granule_start=2026-10-08T10:00Z,
          dimension_key="", state counted
  counter CounterKey{S1,C1,T10,S42,plan#1,"",subscription_line} input gts.cf.oss.cloudlet_hours.v1~
          layout 1, part [10:00, 11:00), granule 10:00, q 0 → 8, q_version 0 → 1
  work    the line's usage binding sells products.derived/cloudlets@1 (one Sum input, CalendarHour
          policy): enqueue child wk1|F-USAGE-OCT|2026-10-08T10:00Z|agg(meter=products.derived/cloudlets@1)
          (provisional)
  ```

  An entry `[10:30, 11:30)` would be `boundary_split_required`: it crosses a UTC-hour granule
  (T-D-53, T-D-75).

**Contract**: `cpt-cf-bss-rating-flow-correction-intake-ing` (`p2`), defined in [§2 migrated flows](#register-flows).
  An invalidation entry is captured in the same page transaction as any other entry; it withdraws
  its target through the contribution state machine — a counted target's recorded coordinates
  receive `−quantity` once and the child is enqueued; a deferred or rejected target is withdrawn
  without a counter change and can never be counted later (§4.4; DESIGN Flow B).

<!-- /contract -->

<a id="contract-12-4-4"></a>

<!-- contract:12-usage-ingestion-normalization:4.4 -->
### Usage ingestion: Invalidations, Replacements and Negative Quantities (normative)

**Contract**: `cpt-cf-bss-rating-algo-correction-intake-ing` (`p1`), defined in [§3 migrated procedures](#register-procedures).

- An invalidation is a faithful copy of its target with `entry_type = invalidation`, a server-stamped
  `invalidates` and a `reason_code`; its quantity is an echo, not a negation (usage-collector
  DESIGN:589-590). Rating captures it (unique `(tenant_id, invalidates_id)`) and withdraws the
  target through the contribution state machine (DESIGN §3.6 Flow A, Flow B): `withdrawn_by` is set
  once (`WHERE withdrawn_by IS NULL`); a `counted` target's recorded coordinates receive `−quantity`
  exactly once and the child is enqueued; a target in `awaiting_attribution`, `awaiting_spec`,
  `boundary_split_required`, `quarantined` or `rejected` is withdrawn with no counter change, and no
  later attribution, spec arrival, layout repair or operator retry can count it. The feed places an
  invalidation after its target.
- At most one invalidation per record (a duplicate with the same reason is absorbed upstream; Rating's
  unique key absorbs any replay). There is no invalidation of an invalidation.
- **Replacement is not atomic upstream**: an invalidation, then a fresh record under a new key with
  the same attribution and period (usage-collector PRD:436-438). Rating counts each when it arrives;
  the window may briefly show the target withdrawn and the replacement not yet present. That state is
  never treated as settled: under `evidence_mode = full` the coverage digest does not match until both
  are stored ([14 §4.5](../DESIGN.md#contract-14-4-5)); under `delay_only` (R-21) a later replacement produces a new revision.
- Target not stored ⇒ the invalidation is kept and `invalidation_target_unknown` is opened (checked
  every 15 minutes); a target captured later is withdrawn on capture (Flow A), never counted.
- A negative-quantity record is an ordinary measurement. A window driven below zero fails closed
  `negative_window_quantity`.

<!-- /contract -->

<a id="contract-12-4-5"></a>

<!-- contract:12-usage-ingestion-normalization:4.5 -->
### Usage ingestion: Exceptions (normative)

**Contract**: `cpt-cf-bss-rating-algo-quarantine-ing` (`p1`), defined in [§3 migrated procedures](#register-procedures).

The cursor advances past every row of this table **because the complete raw entry is already
captured** in the same transaction; a retry re-interprets the stored raw entry and never needs the
collector.

| `reason_code` | Cursor advances | Retry | Resolution |
|---|---|---|---|
| `usage_unidentifiable` (no readable `id` / `tenant_id`) | yes — raw row in `bss_rating__usage_capture_reject` | no | opens a source loss over the whole source scope (§4.6); collector fix, then `source_losses:resolve` |
| `usage_malformed` (`rejected`) | yes | operator, re-interpreting the stored raw entry | collector fix, `POST /exceptions/{id}:retry` |
| `interval_missing` (`rejected`) | yes | operator | the collector's target V1 record (intervals) |
| `usage_type_not_chargeable` (`rejected`) | yes | on declaration change | the type's fold is not `SUM` (§4.7) |
| `awaiting_attribution` (state, exception after 1 h) | yes | on segment arrival, every 15 min | automatic |
| `usage_ambiguous_attribution` | yes | no | Subscriptions fix |
| `boundary_split_required` | yes | on replacement | emitter re-split |
| `invalidation_target_unknown` | yes | every 15 min | automatic or operator |
| `usage_content_conflict` | yes | no | investigation (alarm) |

Operator retry (`usage × retry`) is authorized and audited because it can create charges; it runs the
conditional `count` transition, so it can never count a withdrawn record. An exception never blocks
the feed.

<!-- /contract -->

<a id="contract-12-4-6"></a>

<!-- contract:12-usage-ingestion-normalization:4.6 -->
### Usage ingestion: Feed Checkpoint, Retention and Backpressure (normative)

**Contract**: `cpt-cf-bss-rating-algo-feed-checkpoint-ing` (`p1`), defined in [§3 migrated procedures](#register-procedures).

- **Canonical source identity** (DESIGN §3.7): each configured feed subscription is one source,
  `source_id = "usage:" + UUIDv5(NS_RATING_SOURCE, canonical JSON {sorted gts_type_ids,
  service_identity, granted tenant scope})`. The same `source_id` names the configuration entry, the
  Cluster lock `bss-rating/usage-feed/{source_id}`, the checkpoint row, the metrics `source` label,
  the source-loss rows and the filter a cursor is used with. A cursor is only ever sent with the
  `FeedSubscription` of the source that received it, so `FILTER_MISMATCH` cannot arise from Rating's
  own configuration.
- **Checkpoint**: `bss_rating__source_checkpoint[source_id] = {cursor, fence}`, written only by the
  page transaction with a CAS on `cursor` and `fence ≥ stored fence`. The collector's cursor is the
  only position; there is no snapshot token, sequence or watermark to store.
- **Advance**: after the page's captures and their effects; a crash before commit re-reads the page
  from the old cursor and every capture is a no-op.
- **Bootstrap**: a new source starts at `FeedStart::Oldest`.
- **Changing the type set**: adding or removing a GTS type changes `source_id`. The new source
  bootstraps from `Oldest`; the old source keeps reading until it reaches the current end, is set
  `draining` and then `retired`; dedup absorbs the overlap. An old cursor is never reused under the
  new filter.
- **Widened authorization scope**: the collector cursor does **not** bind the PDP scope, and entries
  that become visible behind a cursor are not delivered to it. A grant change is therefore part of
  the source identity: a widened grant (for example a new tenant) creates a new source replayed
  from `Oldest` (usage-collector PRD:669). If `Oldest` is already past the retention floor for the
  newly visible tenant's history, a source loss of kind `scope_widened_beyond_retention` is opened
  for that tenant.
- **Retention loss** (T-D-62): `CursorBeyondRetention` means entries were removed upstream that
  Rating never captured. The reader opens a `bss_rating__source_loss` row per affected tenant
  (`kind = cursor_beyond_retention`, `last_good_cursor`, `restart_cursor`) in the transaction that
  restarts the source from `Oldest`, and raises `rating_feed_restart_total` and
  `rating_source_loss_open` (page). Because the feed is ordered by the plugin, not by event time, an
  opaque gap reveals no economic timestamps: the loss conservatively taints every `(gts_type, day)`
  of the source's scope from the earliest day whose windows are not yet final. While a loss is
  `open` or `narrowed`, every child of the tainted scope stays `pending(source_loss)` under every
  evidence mode, `delay_only` included; final children of the scope are listed as suspect for
  review and are not changed automatically. The loss is narrowed only by the collector's
  reconciliation matching a tainted day, and resolved only as `repaired` (every tainted day matches,
  missing entries re-delivered by a collector backfill and captured) or `accepted_loss` (audited
  `source_loss × resolve` with evidence) — never by a restart, a metric reset or the passage of time
  (DESIGN §4.4, §4.6). The retention floor (125 days from `window_end`) must exceed the largest
  finalization delay plus the supported outage window (Atlas C09) so that this stays exceptional.
- **Bounded replay** (reconciliation): `read_usage_feed(subscription, After(c1), until = c2)` re-reads
  a delivered range identically; inserts are no-ops.
- **Backpressure**: a reader stops fetching while `rating.child_work` depth exceeds
  `max_queue_depth` (default 1 000 000) and resumes below 80 %; nothing is dropped.
- **Reconciliation**: daily per `(tenant, gts_type)` against the collector's reconciliation metadata
  (`GET /usage-collector/v1/reconciliation`: accepted count, quantity summary, watermarks); it is
  also the evidence that narrows a source loss. The route
  is REST-only and operator-scoped; whether Rating's service identity may call it is UNKNOWN /
  EXTERNAL CONTRACT REQUIRED. Watermarks prove nothing about completeness (ADR-0011).

<!-- /contract -->
