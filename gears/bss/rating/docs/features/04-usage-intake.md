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
- **Upstream**: `cpt-cf-bss-rating-upreq-usage-feed-v1`, `cpt-cf-bss-rating-upreq-usage-feed-scope-digest`, `cpt-cf-bss-rating-upreq-usage-type-declaration-read`, `cpt-cf-bss-rating-upreq-dimension-encoding` ([UPSTREAM_REQS](../UPSTREAM_REQS.md) §2.1, §2.3, §2.4).

**UI applicability**: none; this feature has no endpoint and no user interface. Operator surfaces are in [Operations](11-operations.md).

## 2. Actor Flows (CDSL)

**Use cases**: none in the PRD; the flows are system flows driven by the usage collector's feed.

### 2.1 Read and capture one feed page

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-intake-read-page`

**Actor**: `cpt-cf-bss-rating-actor-rating` (feed reader for one canonical source)

**Success Scenarios**: every entry of the page is captured once with its digest and interpreted; the checkpoint advances in the same transaction.

**Error Scenarios**: feed unavailable (backoff); `InvalidArgument(CursorBeyondRetention)` (source loss and restart from `Oldest`, under the cursor CAS); Rating's grant no longer covers the source's configured tenant scope (the page is not committed and the source pauses); the types registry is unavailable (backoff before the transaction; nothing is rejected); lost lease (checkpoint CAS fails, rollback); queue depth above the backpressure threshold (reader pauses).

**Steps**:
1. [ ] - `p1` - Hold the Cluster leadership `usage-feed-{source_uuid}` (`LeaderElectionV1`, scoped `bss-rating`) for the canonical source; without it, do not read - `inst-rp-lock`
2. [ ] - `p1` - Read the checkpoint and call `read_usage_feed(subscription, Oldest | After(cursor), until = None, limit ≤ 1 000)` outside the transaction - `inst-rp-read`
3. [ ] - `p1` - **IF** the collector answers `InvalidArgument(CursorBeyondRetention)` - `inst-rp-retention`
   1. [ ] - `p1` - In one restart transaction, lock the checkpoint row with the same cursor CAS as a page transaction (roll back on a mismatch, so a stale leader neither resets the cursor nor opens a loss), open one source loss against the source scope (its configured tenant scope × type set; §3.4) and reset the cursor so the next read starts at `Oldest` - `inst-rp-open-loss`
4. [ ] - `p1` - Before the transaction, resolve the usage type declarations of the page's distinct GTS types (cached; declarations are immutable) and check that Rating's grant still covers the source's configured tenant scope (§3.3); on a transient registry or PDP error, back off without opening the transaction, so nothing is rejected and the cursor stays; **IF** the grant no longer covers the scope, do not commit the page: pause the source and raise `usage_grant_narrowed` ([12 §4.6](#contract-12-4-6)) - `inst-rp-prepare`
5. [ ] - `p1` - BEGIN; lock the checkpoint row and check the cursor (CAS) - `inst-rp-cas`
6. [ ] - `p1` - **FOR EACH** entry in feed order: capture it (§3.1), then interpret the stored row (§3.2) or apply it as an invalidation (§2.2) - `inst-rp-each`
7. [ ] - `p1` - Advance the checkpoint cursor; COMMIT - `inst-rp-commit`
8. [ ] - `p1` - **IF** the page is short or empty, poll again after `rating.usage_feed.poll_interval` - `inst-rp-poll`

The page transaction is canonical in [12 §3.6](#contract-12-3-6) (DESIGN §3.6 Flow A is its sequence); this flow names the steps this feature implements.

### 2.2 Apply an invalidation entry

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-intake-invalidation`

**Actor**: `cpt-cf-bss-rating-actor-rating` (feed reader)

**Success Scenarios**: the invalidation is captured once and set `applied`; its target is withdrawn exactly once; a counted target's contribution is removed at its recorded coordinates by the counters feature.

**Error Scenarios**: target not captured yet (`invalidation_target_unknown`, withdrawn on later capture); replayed invalidation (absorbed by its unique key).

**Steps**:
1. [ ] - `p1` - Capture the invalidation entry like any entry, unique on `(tenant_id, invalidates_id)` - `inst-iv-capture`
2. [ ] - `p1` - Withdraw the target with the conditional update on its full key, `WHERE tenant_id = inv.tenant_id AND usage_record_id = inv.invalidates_id AND withdrawn_by IS NULL`, returning its previous state and recorded coordinates; a target is only ever looked up in the invalidation's own tenant - `inst-iv-withdraw`
3. [ ] - `p1` - **IF** the previous state was `counted`, hand the coordinates to the counter decrement of [Attribution and Window Counters](05-attribution-counters.md) in the same transaction - `inst-iv-decrement`
4. [ ] - `p1` - **IF** the target is not stored, keep the invalidation and open `invalidation_target_unknown` - `inst-iv-unknown`
5. [ ] - `p1` - Set the invalidation row `applied` in the same transaction, whether or not its target is stored: an invalidation has no contribution of its own, so it never holds a window open (DESIGN [14 §4.5](../DESIGN.md#contract-14-4-5) condition 6) - `inst-iv-applied`
6. [ ] - `p1` - **RETURN** with no delta computed: corrections become new revisions downstream - `inst-iv-return`

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
2. [ ] - `p1` - Insert `bss_rating__usage_record` with `raw_entry` (the complete entry as received), `raw_sha256` and `raw_digest_version` (the digest over the pinned field list of [12 §4.1](../DESIGN.md#contract-12-4-1), never over the whole entry), `contribution_state = captured`, `ON CONFLICT DO NOTHING` - `inst-cap-insert`
3. [ ] - `p1` - **IF** not inserted, digest the re-read entry under the stored row's `raw_digest_version`; **IF** that equals the stored `raw_sha256`, stop: the entry has no further effect (an optional field the collector added since is outside the pinned list, so it is no conflict) - `inst-cap-duplicate`
4. [ ] - `p1` - **IF** not inserted and the digest differs, raise `usage_content_conflict`; the stored row wins - `inst-cap-conflict`
5. [ ] - `p1` - **RETURN** the stored row for interpretation in the same transaction - `inst-cap-return`

### 3.2 Interpret a stored entry

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-intake-normalize`

**Input**: a stored `bss_rating__usage_record` row (from capture, or from an operator retry).

**Output**: a normalized record handed to attribution, or `rejected(reason)` with an exception.

1. [ ] - `p1` - **IF** a stored invalidation of this tenant already names this record (`(tenant_id, invalidates_id = id)`), set `withdrawn` and stop - `inst-norm-prewithdrawn`
2. [ ] - `p1` - Map the entry per [12 §4.1](../DESIGN.md#contract-12-4-1): interval, quantity (exact, ≤ 28 fractional digits), `gts_type`, `idempotency_key`, resource and subject references, metadata, origin - `inst-norm-map`
3. [ ] - `p1` - **IF** the entry is malformed, has no interval or names a GTS type the registry answers as not declared, set `rejected(usage_malformed | interval_missing)` and open an exception - `inst-norm-malformed`
4. [ ] - `p1` - **IF** another stored entry already holds the natural key `(tenant_id, gts_type, idempotency_key, interval_start, interval_end, entry_type)`, set `rejected(usage_natural_key_conflict)`, raise the alarm and stop; the stored entry stands. The normalized columns are written under a savepoint, so a concurrent writer's unique violation on `uq_…__natural_key` rolls back to it and takes this branch instead of aborting the page ([12 §4.3](../DESIGN.md#contract-12-4-3)) - `inst-norm-natural-key`
5. [ ] - `p1` - Keep the usage type declaration resolved before the transaction (§2.1 `inst-rp-prepare`) with the record; only a definitive "not declared" answer rejects an entry, and an unavailable registry never does, because the page is not committed until every declaration it needs is resolved. A raw type whose fold is not `SUM` is **not** rejected: its records are captured and counted, and evaluation decides (a `Sum` input of such a type fails the child `usage_type_not_chargeable`; a `Peak` / `TimeWeighted` input fails it `unsupported_input_fold` until R-06; [12 §4.7](../DESIGN.md#contract-12-4-7)) - `inst-norm-fold`
6. [ ] - `p1` - Keep `gts_type` as the record's **raw input usage type** (a usage SKU sells a Products derived usage type whose inputs are raw GTS types, products P-D-259; T-D-76) and derive no dimension value from metadata at launch: the record counts toward its item's one rated cell, the value the fact's accepted binding names (DESIGN [03 §4.2](../DESIGN.md#contract-03-4-2)); the metadata mapping waits for R-16 - `inst-norm-dimension`
7. [ ] - `p1` - **RETURN** the normalized record to the attribution step of [Attribution and Window Counters](05-attribution-counters.md) - `inst-norm-return`

A rejection never blocks the feed: the raw entry is already stored, so a later retry re-runs this process without the collector ([12 §4.5](#contract-12-4-5)).

### 3.3 Maintain the canonical source and its checkpoint

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-intake-source-identity`

**Input**: the configured feed subscription (sorted GTS types and its configured tenant scope, `rating.usage_feed.subscriptions[*].tenant_scope`) and Rating's service identity.

**Output**: one `source_id` naming the configuration, the Cluster leadership `usage-feed-{source_uuid}`, the checkpoint, metrics and losses.

1. [ ] - `p1` - Compute `source_id = "usage:" + UUIDv5(NS_RATING_SOURCE, canonical JSON {sorted gts_type_ids, service_identity, configured tenant scope})`; the tenant scope is configuration, never inferred from what the feed returns - `inst-src-id`
2. [ ] - `p1` - **IF** the type set or the configured tenant scope changed, create the new source from `Oldest` and drain the old one to its current end, then retire it; a source whose scope was narrowed on purpose is retired without draining (its grant no longer covers it) - `inst-src-change`
3. [ ] - `p1` - **IF** a widened scope exposes history already past the retention floor, open `scope_widened_beyond_retention` for that tenant - `inst-src-widened`
4. [ ] - `p1` - Before each read and again before each page commit, evaluate Rating's grant on the feed read with the platform PDP; **IF** it no longer covers the configured tenant scope, commit nothing and pause the source with `usage_grant_narrowed` until it does or the operator narrows the configured scope ([12 §4.6](#contract-12-4-6)) - `inst-src-grant`
5. [ ] - `p1` - Never send a cursor with any subscription other than the one that produced it - `inst-src-cursor`

Details: [12 §4.6](#contract-12-4-6).

### 3.4 Record and keep a source loss

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-intake-source-loss`

**Input**: `InvalidArgument(CursorBeyondRetention)`, an unidentifiable entry, or a scope widened beyond retention.

**Output**: one `open` `bss_rating__source_loss` row against the source scope (`tenant_scope × gts_type_ids`), which blocks finality of every not-yet-final child that can hold an entry of a tainted day (ADR-0007).

1. [ ] - `p1` - Insert the loss in the transaction that detects it, under the checkpoint row lock and cursor CAS (the restart transaction for `cursor_beyond_retention`, the page transaction for `unidentifiable_entry`, the source-creation transaction for `scope_widened_beyond_retention`), with `kind`, `gts_type_ids`, `last_good_cursor`, `last_good_at` (the checkpoint's `updated_at` when that cursor was committed), `restart_cursor` (the cursor the transaction writes; NULL means `FeedStart::Oldest`) and `tainted_days` per kind ([12 §4.6](#contract-12-4-6)): for `cursor_beyond_retention`, the days from one retention floor before `last_good_at` to one retention floor before detection, which is where the removed entries can lie - `inst-sl-open`
2. [ ] - `p1` - Bump `input_generation` of the affected non-final children so the evidence gate re-checks them, and list the final children inside the taint as suspect (`rating_source_loss_suspect_final`); they are not changed automatically - `inst-sl-bump`
3. [ ] - `p1` - Never resolve the loss here: only reconciliation evidence for days the collector still holds in full, or an audited operator resolution, does ([Operations](11-operations.md)); a `cursor_beyond_retention` loss lies wholly beyond the collector's retention and backfill window, so it can only end as `accepted_loss`, never `repaired` - `inst-sl-no-auto`

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

**States**: `captured`, `rejected`, `withdrawn`, `applied` (this feature's transitions of the contribution state machine defined in DESIGN §3.6 Flow A; deferred states and `counted` belong to [Attribution and Window Counters](05-attribution-counters.md)).

**Initial State**: `captured`.

**Transitions**:
1. [ ] - `p1` - **FROM** — **TO** `captured` **WHEN** an entry is stored for the first time - `inst-st-capture`
2. [ ] - `p1` - **FROM** `captured` **TO** `rejected(reason)` **WHEN** normalization fails, including `usage_natural_key_conflict` (a `Peak` / `TimeWeighted` input is not a rejection: the record stays counted and only its child fails, R-06) - `inst-st-reject`
3. [ ] - `p1` - **FROM** `captured` **TO** `applied` **WHEN** the entry is an invalidation: set by Flow B step 2 in its capture transaction, whether or not its target is stored; `applied` is terminal and never blocks finality - `inst-st-applied`
4. [ ] - `p1` - **FROM** `rejected` **TO** `captured` **WHEN** an operator retry (`usage × retry`, audited) re-interprets the stored raw entry; it then follows the transitions of [Attribution and Window Counters](05-attribution-counters.md) - `inst-st-retry`
5. [ ] - `p1` - **FROM** any non-`withdrawn` state of a record **TO** `withdrawn` **WHEN** its invalidation is applied (`withdrawn_by` set once) - `inst-st-withdraw`
6. [ ] - `p1` - **FROM** — **TO** `withdrawn` **WHEN** a record is captured after its invalidation - `inst-st-late-target`

### 4.2 Source loss

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-state-intake-source-loss`

**States**: `open`, `narrowed`, `pending_approval`, `resolved`.

**Initial State**: `open`.

**Transitions**:
1. [ ] - `p1` - **FROM** `open` or `narrowed` **TO** `narrowed` **WHEN** reconciliation figures (Rating's own read, or figures an operator submitted; [Operations](11-operations.md)) match some (more) tainted days that the collector still holds in full; narrowing repeats until none remain - `inst-sls-narrow`
2. [ ] - `p1` - **FROM** `open` or `narrowed` **TO** `resolved(repaired)` **WHEN** every tainted day matches; never for a day the collector no longer holds in full, so never for a `cursor_beyond_retention` loss ([12 §4.6](#contract-12-4-6)) - `inst-sls-repaired`
3. [ ] - `p1` - **FROM** `open` or `narrowed` **TO** `pending_approval` **WHEN** an audited `source_loss × resolve` requests `accepted_loss` with evidence - `inst-sls-accepted`
4. [ ] - `p1` - **FROM** `pending_approval` **TO** `resolved(accepted_loss)` **WHEN** a second operator approves (`source_loss × approve`, never the requester) - `inst-sls-approved`
5. [ ] - `p1` - **FROM** `pending_approval` **TO** `open` or `narrowed` (its prior state) **WHEN** the approver rejects the request (`POST /source-losses/{id}:reject`, `source_loss × approve`, audited) - `inst-sls-rejected`

## 5. Definitions of Done

### 5.1 Lossless, idempotent page capture

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-intake-capture`

The system **MUST** store every feed entry complete with its SHA-256 before interpreting it, commit a page's captures, their effects and the checkpoint advance in one transaction with the cursor CAS, and treat an already-captured entry with the same digest as having no effect at all.

**Implements**: `cpt-cf-bss-rating-flow-intake-read-page`, `cpt-cf-bss-rating-algo-intake-capture`, `cpt-cf-bss-rating-state-intake-capture`, `cpt-cf-bss-rating-flow-ingest-ing`.

**Constraints**: `cpt-cf-bss-rating-constraint-authoritative-dedup-ing`, `cpt-cf-bss-rating-constraint-entries-not-aggregates-ing`, `cpt-cf-bss-rating-constraint-in-process-contracts`.

**Touches**: `read_usage_feed`; `cpt-cf-bss-rating-dbtable-source-checkpoint`, `cpt-cf-bss-rating-dbtable-usage-record`; Cluster leadership `usage-feed-{source_uuid}` (`LeaderElectionV1`, scoped `bss-rating`).

### 5.2 Retryable normalization and invalidation intake

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-intake-normalize`

The system **MUST** interpret only stored entries, reject malformed, interval-less, undeclared-type and natural-key-conflicting entries into the exception register without blocking the feed (an unavailable registry rejects nothing), populate `meter` and `dimension_key` per [12 §4.1](../DESIGN.md#contract-12-4-1), and apply invalidations through the conditional withdrawal on the target's full key so a withdrawn record can never contribute again, setting each invalidation row `applied` in the same transaction.

**Implements**: `cpt-cf-bss-rating-algo-intake-normalize`, `cpt-cf-bss-rating-flow-intake-invalidation`, `cpt-cf-bss-rating-flow-correction-intake-ing`, `cpt-cf-bss-rating-algo-correction-intake-ing`, `cpt-cf-bss-rating-algo-quarantine-ing`.

**Constraints**: `cpt-cf-bss-rating-constraint-no-price-ing`, `cpt-cf-bss-rating-constraint-utc-units-ing`.

**Touches**: `cpt-cf-bss-rating-dbtable-usage-record`, `cpt-cf-bss-rating-dbtable-exception`; types-registry usage type declarations.

### 5.3 Source identity, source loss and backpressure

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-intake-source-loss`

The system **MUST** derive one canonical `source_id` per feed subscription and its configured tenant scope, start a changed subscription as a new source from `Oldest`, commit no page while Rating's grant does not cover the configured scope, open a durable source loss for `CursorBeyondRetention` (under the cursor CAS, tainting the days the removed entries can belong to), unidentifiable entries and widened scopes, keep it open until reconciliation of days the collector still holds or an audited resolution closes it, and pause reading under backpressure without dropping anything.

**Implements**: `cpt-cf-bss-rating-algo-intake-source-identity`, `cpt-cf-bss-rating-algo-intake-source-loss`, `cpt-cf-bss-rating-algo-intake-backpressure`, `cpt-cf-bss-rating-algo-feed-checkpoint-ing`, `cpt-cf-bss-rating-state-intake-source-loss`.

**Constraints**: `cpt-cf-bss-rating-constraint-authoritative-dedup-ing`.

**Touches**: `cpt-cf-bss-rating-dbtable-source-loss`, `cpt-cf-bss-rating-dbtable-source-checkpoint`, `bss_rating__usage_capture_reject`.

Observability: this feature emits `rating_feed_lag_seconds`, `rating_feed_cursor_age_seconds`, `rating_feed_oldest_unread_age_seconds`, `rating_feed_restart_total`, `rating_feed_paused{reason}`, `rating_source_loss_open`, `rating_source_loss_suspect_final`, `rating_usage_rejected_total{reason}`, `rating_usage_duplicate_total`, `rating_usage_content_conflict_total`, `rating_usage_natural_key_conflict_total` and the `rating.feed.page` span (DESIGN §4.7).

## 6. Acceptance Criteria

- [ ] A re-read entry with a different digest under its stored `raw_digest_version` raises `usage_content_conflict` and keeps the stored row; an optional field the collector adds later is no conflict.
- [ ] A malformed entry is `rejected` and the cursor advances past it; the feed is never blocked by an exception.
- [ ] An entry with a new id but a stored entry's natural key is `rejected(usage_natural_key_conflict)` and the page commits past it.
- [ ] An unavailable types registry rejects nothing: no transaction is opened and the cursor stays until the declarations resolve.
- [ ] A re-read entry with the same digest is not normalized again.
- [ ] A record of a non-`SUM` raw type is captured and counted, never rejected; its child fails `usage_type_not_chargeable` (a `Sum` input) or `unsupported_input_fold` (a `Peak` / `TimeWeighted` input, R-06); a record of an item on a dimensional entry counts toward the item's one rated cell (V56); an accepted binding that names more than one value for one item fails `unsupported_primitive` until R-16 is decided.
- [ ] An invalidation of a counted record withdraws it once and is itself `applied`; a replayed invalidation is a no-op; an invalidation whose target is captured later withdraws the target on capture; an invalidation never holds a window open.
- [ ] `InvalidArgument(CursorBeyondRetention)` under any evidence mode opens a source loss under the cursor CAS and restarts from `Oldest`; the loss stays `open` after the restart and taints only the days the removed entries can belong to.
- [ ] While Rating's grant does not cover the source's configured tenant scope, no page is committed; once it does, the same pages are read again and nothing is skipped.
- [ ] Above `max_queue_depth` the reader stops fetching and resumes below 80 %; no entry is dropped.

Acceptance vectors owned by this feature (DESIGN §4.13 index):

| # | Scenario | Expected state |
|---|---|---|
| V01 | [PostgreSQL] The same page is applied twice (crash after commit, re-read) | `Q`, `q_version` and contribution counts unchanged |
| V03 | Malformed entry (a GTS type whose declaration Rating's cache holds wrongly), collector retention then expires, operator retries after the Rating-side fix | the stored raw entry is re-interpreted and counted once |
| V04 | [PostgreSQL] Crash after capture, before commit | nothing persisted; re-read captures once |
| V07 | Type set of a feed subscription changes | new `source_id` from `Oldest`; the old cursor is never used with the new filter |
| V62 | An invalidation is read before its target (the target arrives later, for example through a source whose type set overlaps) | the invalidation is `applied` and `invalidation_target_unknown` is opened; the target is captured `withdrawn` and never counted; the exception closes |
| V63 | A target and its invalidation on the same page | the target is counted, then withdrawn in the same transaction: `Q` unchanged, `q_version` + 2; the invalidation is `applied` |
| V64 | An entry is re-read with the same id and different content | `usage_content_conflict` alarm; the stored row stands; no normalization, contribution or invalidation follows |
| V65 | An entry with no readable id or tenant | its bytes go to `bss_rating__usage_capture_reject`; one `unidentifiable_entry` loss over the source scope; the cursor advances |
| V66 | A widened scope exposes a tenant whose history is already past the retention floor | a new source from `Oldest`; one `scope_widened_beyond_retention` loss for that tenant |
| V67 | `rating.child_work` depth rises above `max_queue_depth`, then falls below 80 % | the reader stops fetching and resumes from the stored cursor; every entry is captured once, none is dropped |
| V68 | [PostgreSQL] Two readers of one source commit the same page concurrently | one commits; the other fails the cursor CAS and rolls back with no capture or counter effect |
| V69 | An invalidation of a counted record whose window is already final | the target is withdrawn once (`−quantity` at its recorded coordinates); the child is re-evaluated to window revision n + 1 with its binding of record, and the parent to a new result revision |
| V70 | An invalidation of a counted record in an hourly window; then that window's `window_end + delay` passes | the invalidation is `applied`, never `captured`; the window finalizes without the target's quantity (condition 6 reads only `record` rows) |
| V71 | A page is stuck for 130 days; then the read answers `InvalidArgument(CursorBeyondRetention)` | one loss tainting the days from `last_good_at − 125 d` to `detected_at − 125 d`; the recent days re-read from `Oldest` are not tainted; reconciliation of the tainted days narrows nothing; the final children inside the taint are listed as suspect; the loss ends only as `accepted_loss` with a second approver, never `repaired` |
| V72 | Rating's grant loses tenant T10 while T10's hourly window is open, and is restored a day later | no page is committed while T10 is outside the grant; the source pauses with `usage_grant_narrowed`; after the restore the same pages are read again and T10's entries are captured; no window of T10 finalizes without them |
| V73 | An entry with a new id but the natural key of a stored entry | `rejected(usage_natural_key_conflict)` with an alarm; the stored entry stands; the page commits and the cursor advances past it |
| V74 | The collector adds an optional field to entries; a restart from `Oldest` (or V07) re-delivers stored entries with it | no `usage_content_conflict`: each stored row's digest is recomputed under its `raw_digest_version` and matches |
| V75 | The types registry is unavailable while a page holds an entry of an uncached GTS type | no transaction is opened and the cursor stays; nothing is rejected; after recovery the page is captured and interpreted |
| V76 | [PostgreSQL] A stale leader gets `InvalidArgument(CursorBeyondRetention)` for a cursor the new leader has already advanced | the restart transaction fails the cursor CAS and rolls back: no loss row, no cursor reset |

## 7. Detailed Behavior Contracts

**Contract namespace 12.** Section numbers inside the detailed contracts below resolve through the [contract address index](../DECOMPOSITION.md#4-contract-address-index), not the overview section numbers above. A bare section reference names a section of the same namespace; "DESIGN §n" names §1–§5 of [DESIGN.md](../DESIGN.md).

The following sections keep the runtime procedures of these namespaces — their interactions and sequences and the procedural parts of their §4. The flows, processes and acceptance criteria above summarize them; the architecture, schemas and evaluation semantics they rely on are defined once in [DESIGN.md](../DESIGN.md) §6.

<a id="contract-12-3-6"></a>

<!-- contract:12-usage-ingestion-normalization:3.6 -->
### Usage ingestion: Interactions and Sequences

**Contract**: `cpt-cf-bss-rating-flow-ingest-ing` (`p1`), defined in [§2 migrated flows](#register-flows).
  **Read and capture one page** (`UsageFeedReader`, one transaction per page; the sequence diagram
  and the contribution state machine are DESIGN §3.6 Flow A):

  ```text
  leader of usage-feed-{source_uuid} (Cluster LeaderElectionV1, scoped "bss-rating"); advisory:
  no lock guard is held across the feed read or the transaction (Cluster ADR-002)
  cp    = bss_rating__source_checkpoint[source_id]
  start = cp.cursor ? After(cp.cursor) : Oldest
  grant_covers(source.tenant_scope) or pause                                      -- outside tx (PDP)
  page  = read_usage_feed(ctx, subscription, start, until = None, limit = 1000)  -- outside tx
          Err(InvalidArgument(CursorBeyondRetention)) →
            BEGIN                                                                -- restart transaction
              SELECT … FROM bss_rating__source_checkpoint WHERE source_id = … FOR UPDATE
              cursor ≠ cp.cursor → ROLLBACK                       -- a stale leader resets nothing
              SourceLossTracker.open(source_id, cursor_beyond_retention, last_good_cursor = cp.cursor,
                                     last_good_at = cp.updated_at, restart_cursor = NULL)  -- NULL = Oldest
              UPDATE bss_rating__source_checkpoint SET cursor = NULL, updated_at = now()
            COMMIT; read again from Oldest
  decls = declaration of each distinct gts_type of the page (cache, else types-registry)  -- outside tx
          transient error → back off; no transaction, cursor unchanged, nothing rejected
  grant_covers(source.tenant_scope) or pause; nothing committed  -- usage_grant_narrowed (12 §4.6)
  BEGIN
    SELECT … FROM bss_rating__source_checkpoint WHERE source_id = … FOR UPDATE  -- CAS: cursor = cp.cursor
    for e in page.entries (feed order; an invalidation follows its target):
      -- 1. capture, before any interpretation
      id, tenant unreadable → INSERT bss_rating__usage_capture_reject (source_id, raw_sha256, raw);
                              SourceLossTracker.open(source_id, unidentifiable_entry,
                                last_good_cursor = cp.cursor, restart_cursor = page.next_cursor); continue
      INSERT bss_rating__usage_record (tenant_id, usage_record_id, raw_entry,
             raw_sha256 = digest_v(e, current), raw_digest_version = current,
             contribution_state = captured) ON CONFLICT (tenant_id, usage_record_id) DO NOTHING
        not inserted, digest_v(e, stored.raw_digest_version) = stored.raw_sha256
                                           → continue            -- already captured: no effect at all
        not inserted, different            → alarm usage_content_conflict; continue
      -- 2. interpret the stored row (same transaction)
      e.entry_type = invalidation → Flow B step 2 (target WHERE tenant_id = e.tenant_id AND
                                    usage_record_id = e.invalidates); state of e = applied; continue
      exists invalidation (tenant_id = e.tenant_id, invalidates_id = e.id) → state = withdrawn; continue
      n = normalize(raw, decls) malformed / no interval / type not declared
                              → state = rejected(usage_malformed | interval_missing)
                              (exception opened; the raw row is the retry input)          ; continue
      SAVEPOINT; write n's normalized columns                                 -- uq_…__natural_key
        natural key held by another id (found first, or 23505 from a concurrent writer)
                              → ROLLBACK TO SAVEPOINT; state = rejected(usage_natural_key_conflict),
                                alarm; continue
      a = attribute(n)        none → awaiting_attribution; ambiguous → quarantined; continue
                              interval crosses a segment boundary → boundary_split_required; continue
      count(e, a)             -- once per record, under its raw GTS type: usage-policy projection,
                              -- layout part, bound check and counter (13 §3.6, feature 05)
      -- a Peak / TimeWeighted input is still counted: the child of a meter whose declaration
      -- has one fails unsupported_input_fold (R-06); the record itself is never rejected for it
    UPDATE bss_rating__source_checkpoint SET cursor = page.next_cursor, updated_at = now()
  COMMIT; wake.fire()                                  -- Wake::fire after commit (toolkit_db outbox)
  short or empty page → the reader is caught up; it polls again after rating.usage_feed.poll_interval
  ```

  Example (hourly cloudlets):

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
          policy): enqueue child wk1|F-USAGE-OCT|2026-10-08T10:00:00Z|agg(meter=products.derived/cloudlets@1)
          (provisional)
  ```

  An entry `[10:30, 11:30)` would be `boundary_split_required`: it crosses a UTC-hour granule
  (T-D-53, T-D-75).

**Contract**: `cpt-cf-bss-rating-flow-correction-intake-ing` (`p2`), defined in [§2 migrated flows](#register-flows).
  An invalidation entry is captured in the same page transaction as any other entry and set
  `applied`; it withdraws its target, looked up by the invalidation's tenant and `invalidates`,
  through the contribution state machine — a counted target's recorded coordinates
  receive `−quantity` once and the child is enqueued; a deferred or rejected target is withdrawn
  without a counter change and can never be counted later (§4.4; DESIGN Flow B).

<!-- /contract -->

<a id="contract-12-4-4"></a>

<!-- contract:12-usage-ingestion-normalization:4.4 -->
### Usage ingestion: Invalidations, Replacements and Negative Quantities (normative)

**Contract**: `cpt-cf-bss-rating-algo-correction-intake-ing` (`p1`), defined in [§3 migrated procedures](#register-procedures).

- An invalidation is a faithful copy of its target with `entry_type = invalidation`, a server-stamped
  `invalidates` and a `reason_code`; its quantity is an echo, not a negation (usage-collector
  DESIGN:589-590). Rating captures it (unique `(tenant_id, invalidates_id)`), sets it `applied` in
  the same transaction (an invalidation contributes nothing itself and never holds a window open)
  and withdraws the target, keyed `(tenant_id = the invalidation's, usage_record_id = invalidates)`,
  through the contribution state machine (DESIGN §3.6 Flow A, Flow B): `withdrawn_by` is set
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
- Target not stored ⇒ the invalidation is kept (`applied`) and `invalidation_target_unknown` is
  opened (checked every 15 minutes); a target captured later is withdrawn on capture (Flow A), never
  counted.
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
| `usage_unidentifiable` (no readable `id` / `tenant_id`) | yes — raw row in `bss_rating__usage_capture_reject` | no | opens a source loss over the whole source scope (§4.6), resolved through `source_losses:resolve`; a collector fix stops new ones but cannot change bytes already stored |
| `usage_malformed` (`rejected`) | yes | operator, re-interpreting the stored raw entry | a **Rating-side** fix (the usage type declaration, the mapping or a stale cache), then `POST /exceptions/{id}:retry`; or the emitter invalidates the entry and sends a replacement — the invalidation withdraws the rejected row and closes its exception, and the replacement is captured as a new record. A collector fix cannot change the outcome: the raw row is never changed, the collector modifies no accepted entry (usage-collector DESIGN:584) and a re-sent entry is either the same id (the stored row wins) or a new key |
| `interval_missing` (`rejected`) | yes | operator | the emitter re-emits under the collector's target V1 record (intervals): an invalidation withdraws the stored row |
| `usage_natural_key_conflict` (`rejected`; a new id with a stored entry's natural key) | yes | no | investigation (alarm `rating_usage_natural_key_conflict_total`); the stored entry stands, as for a content conflict (12 §4.3) |
| `usage_type_not_chargeable` (child `failed`, the record stays counted) | yes | on declaration change | a derived type takes a raw type whose fold is not `SUM` as a `Sum` input (§4.7) |
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
  service_identity, configured tenant scope})`. The tenant scope is an explicit value of the
  source's configuration (`rating.usage_feed.subscriptions[*].tenant_scope`: tenant subtree roots),
  never inferred from what the feed returns, because the collector neither reports a caller's grant
  nor signals a change of it (usage-collector PRD:669). The same `source_id` names the configuration entry, the
  Cluster leadership `usage-feed-{source_uuid}` (`LeaderElectionV1`, scoped `bss-rating`), the checkpoint row, the metrics `source` label,
  the source-loss rows and the filter a cursor is used with. A cursor is only ever sent with the
  `FeedSubscription` of the source that received it, so `FILTER_MISMATCH` cannot arise from Rating's
  own configuration.
- **Checkpoint**: `bss_rating__source_checkpoint[source_id] = {cursor}`, written only by the
  page transaction or the restart transaction (retention loss, below), each holding the checkpoint
  row lock with a CAS on `cursor`. The collector's cursor is the
  only position; there is no snapshot token, sequence or watermark to store. A NULL cursor means
  the next read starts at `FeedStart::Oldest`.
- **Advance**: after the page's captures and their effects; a crash before commit re-reads the page
  from the old cursor and every capture is a no-op.
- **Bootstrap**: a new source starts at `FeedStart::Oldest`.
- **Changing the type set**: adding or removing a GTS type changes `source_id`. The new source
  bootstraps from `Oldest`; the old source keeps reading until it reaches the current end, is set
  `draining` and then `retired`; dedup absorbs the overlap. An old cursor is never reused under the
  new filter.
- **Widened authorization scope**: the collector cursor does **not** bind the PDP scope, and entries
  that become visible behind a cursor are not delivered to it. The tenant scope is therefore part of
  the source identity: widening the configured scope (for example a new tenant) creates a new source
  replayed from `Oldest` (usage-collector PRD:669). If `Oldest` is already past the retention floor
  for the newly visible tenant's history, a source loss of kind `scope_widened_beyond_retention` is
  opened for that tenant.
- **Narrowed authorization scope**: the collector skips the entries a caller may no longer read "as
  the cursor advances, with no error raised", and a scope change "is never a `FILTER_MISMATCH`"
  (usage-collector DESIGN:911-913, 1411-1413). A narrowing therefore changes neither the source nor
  the cursor, and reading on would lose the dropped tenants' usage for good. Before each read and
  again before committing the page, the reader evaluates its own grant with the platform PDP: the
  subject, action and resource type the collector evaluates for the feed read (the action's name is
  asked of the collector, `…-upreq-usage-feed-scope-digest`). If the grant no longer covers the
  configured tenant scope, the reader commits nothing and pauses the source with
  `usage_grant_narrowed` (`rating_feed_paused{reason = grant_narrowed}`, page). Nothing is skipped:
  the cursor has not moved, so once the grant covers the scope again the same pages are read again,
  with every entry. An intended narrowing is a configuration change instead: the operator removes
  the tenants from the configured scope, which creates a new source from `Oldest`, and the old
  source is retired without draining. A pause longer than the replay horizon ends in a retention
  loss (below). A narrowing and its reversal that both fall between the two checks of one page
  cannot be seen by Rating; the scope digest asked on `FeedPage` closes that gap.
- **Retention loss** (T-D-62, ADR-0007): `InvalidArgument(CursorBeyondRetention)` means the
  collector removed entries after Rating's cursor that Rating never captured. The reader opens one
  `bss_rating__source_loss` row against the source scope (never per enumerated tenant) —
  `kind = cursor_beyond_retention`, `last_good_cursor` = the refused cursor, `last_good_at` = the
  checkpoint's `updated_at` when that cursor was committed, `restart_cursor = NULL` (`Oldest`) — in
  the **restart transaction**: it takes the checkpoint row lock and the cursor CAS like a page
  transaction (a mismatch rolls back, so a stale leader neither resets a newer cursor nor opens a
  second loss), resets the cursor to NULL and raises `rating_feed_restart_total` and
  `rating_source_loss_open` (page).
  - **Which days the removed entries can belong to.** The feed's order reveals no economic time,
    but the collector's own retention rule bounds it. Retention runs from the end of an entry's
    covered period and keeps it at least the retention floor `F` = backfill window + replay
    horizon (125 days at launch; usage-collector PRD:691-701). An entry removed by the detection
    instant `t_d` therefore has `window_end ≤ t_d − F`. Every read up to `last_good_cursor`
    succeeded, so at `last_good_at` the collector still held every entry after that cursor; relying
    on the floor only, a removed entry has `window_end ≥ last_good_at − F`. The loss taints every
    `(gts_type, day)` of the source scope with `day(last_good_at − F) ≤ day ≤ day(t_d − F)`, the day
    of an entry being the UTC day of its `window_end`. Entries with a later `window_end` are still
    held in full and are re-read from `Oldest`; those days are not tainted. A reader more than `F`
    behind at its last good page could have lost older entries as well; the oldest-unread alert
    (DESIGN §4.7) pages seven days before a reader falls that far behind.
  - **What it blocks.** A non-final child whose window can hold an entry of a tainted day of one of
    its input types (its `window_start` is before the end of that day and its `window_end` is at or
    after the day's start) stays `pending(source_loss)` under every evidence mode, `delay_only`
    included — in practice a `BillingCycle` window that is still open, since hourly windows that old
    are final. The final children inside the taint are listed as suspect
    (`rating_source_loss_suspect_final`) and are not changed automatically.
  - **How it ends.** Every tainted day is older than the collector's retention when the loss is
    opened. The collector's reconciliation figures for such a day count only the entries that
    survived, so they match Rating's captures whether or not something was removed: they prove
    nothing and never narrow the loss. The backfill route refuses a covered period older than its
    window (`PAST_WINDOW`; a hard bound unless the collector's operator widens the window before an
    import, usage-collector PRD:717-723), and the tainted days lie beyond it by construction
    (`F` > the backfill window). A `cursor_beyond_retention` loss therefore never becomes
    `repaired`: it ends only as `resolved(accepted_loss)` — an audited `source_loss × resolve` with
    evidence, approved by a second operator — which is also the explicit decision on its suspect
    final children (they stand, or an administrative re-rate corrects them after a re-import). A
    restart, a metric reset or the passage of time never resolves it (DESIGN §4.4, §4.6). The floor
    must exceed the largest finalization delay plus the supported outage window (Atlas C09) so that
    this stays exceptional.
- **Taint of the other kinds**: an `unidentifiable_entry` loss taints every day the feed still
  holds, `day(t_d − F) ≤ day ≤ day(t_d)` (the entry's own period cannot be trusted); those days are
  inside retention when the loss opens, so reconciliation can narrow each of them while it stays
  inside (a day that ages past the floor unmatched stays tainted) and the loss can end `repaired`. A
  `scope_widened_beyond_retention` loss taints the newly visible tenant's days before
  `day(t_d − F)` that a stored fact of that tenant covers; they are past retention, so it ends as
  `accepted_loss`.
- **Bounded replay** (reconciliation): `read_usage_feed(subscription, After(c1), until = c2)` re-reads
  a delivered range identically; inserts are no-ops.
- **Backpressure**: a reader stops fetching while `rating.child_work` depth exceeds
  `max_queue_depth` (default 1 000 000) and resumes below 80 %; nothing is dropped.
- **Reconciliation**: daily per `(tenant, gts_type)` against the collector's reconciliation metadata
  (`GET /usage-collector/v1/reconciliation`: accepted count, quantity summary, watermarks); it is
  also the evidence that narrows a source loss, for days the collector still holds in full. The
  route is REST-only and operator-scoped; whether Rating's service identity may call it is UNKNOWN /
  EXTERNAL CONTRACT REQUIRED (`…-upreq-usage-reconciliation-access`). Two paths therefore feed one
  comparison: Rating's reconciler reads the route itself once that access exists, and until then an
  operator who can call it submits the figures with `POST /source-losses/{id}:reconciliation`
  (`source_loss × resolve`, audited; [Operations](11-operations.md) §2.2). Either way Rating stores
  the figures in `bss_rating__reconciliation_evidence` and compares them with its own captures; a
  submitted figure is never taken as a match by itself. Watermarks prove nothing about completeness
  (ADR-0011).

<!-- /contract -->
