Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-10-02 by Virtuozzo International GmbH

<!-- CONFLUENCE_TITLE: [BSS]: Rating — Usage Ingestion & Normalization (Design) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../SEAMS.md | Upstream: usage-collector, subscriptions, types-registry | Downstream: 13-q-store-attribution | Owners: BSS Rating team -->

# DESIGN — Usage Ingestion & Normalization (Slice 12, pipeline)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-usage-ingestion`

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
  - [4.1 Collector Entry → Rating Usage Mapping (normative)](#41-collector-entry--rating-usage-mapping-normative)
  - [4.2 Attribution (normative)](#42-attribution-normative)
  - [4.3 Usage Dedup (normative)](#43-usage-dedup-normative)
  - [4.4 Invalidations, Replacements and Negative Quantities (normative)](#44-invalidations-replacements-and-negative-quantities-normative)
  - [4.5 Exceptions (normative)](#45-exceptions-normative)
  - [4.6 Feed Checkpoint, Retention and Backpressure (normative)](#46-feed-checkpoint-retention-and-backpressure-normative)
  - [4.7 Usage Type Declarations (normative)](#47-usage-type-declarations-normative)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

The intake edge of the pipeline. `UsageFeedReader` pulls the usage-collector **usage feed** for the
GTS types Rating prices, stores every entry (records and invalidations) in `rating_usage_record`,
attributes records through the Subscriptions attribution projection (slice 13), and hands quantities
to the counter materializer — all in the page transaction that advances the feed cursor
(DESIGN §3.6 Flow A). It computes no money and reads no price.

The contract is the **usage-collector's own target V1 design** on upstream `main` (commit
`5de85f067`, "rework the metering model"): `read_usage_feed`, interval entries, invalidation as an
entry, a dedup identity over the covered period (usage-collector DESIGN §3.3, ADR-0007, ADR-0010,
ADR-0011). Rating follows it; the Seam Atlas's `UsageFeedV1` / `UsageCollectorClientV2` sketch is
withdrawn by the Atlas Plus overlay (2026-10-02) in favour of the same design.

**CURRENT**: the usage-collector code (SDK, gear and plugins, on this branch and on upstream
`main`) still implements the superseded point-record model — no feed, `created_at` instants,
`deactivate_usage_record`, `corrects_id` compensation. The collector's DESIGN names this as known
debt and states that the document, not the code, is normative (usage-collector DESIGN:2296).
`[DEPENDENCY GAP R-01]`. This branch's base (`8aca4d6df`) predates the redesign; the redesign is on
upstream `main`.

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design response |
|---|---|
| `cpt-cf-bss-rating-fr-idempotency` (usage) | Dedup on the collector entry id, which the collector derives from its dedup identity (§4.3). |
| `cpt-cf-bss-rating-fr-usage-corrections` | Invalidation entries withdraw the target's quantity; replacements are ordinary records; negative quantities are ordinary measurements (§4.4). |
| `cpt-cf-bss-rating-fr-dimension-population-contract` | `dimension_key` from declared metadata (§4.1; R-16). |
| `cpt-cf-bss-rating-fr-meter-mapping-granularity` | Entries are stored raw; granularity applies to the aggregate (slice 03). |

#### NFR Allocation

| NFR | Design response |
|---|---|
| `cpt-cf-bss-rating-nfr-throughput-latency` | One transaction per page of ≤ 1 000 entries; inserts and counter upserts only. The collector's replay target (a 24 h backlog cleared in 6 h, ≥ 5 × the subscribed arrival rate) bounds catch-up. |
| `cpt-cf-bss-rating-nfr-resilience` | Cursor advances with effects; replay is idempotent; poison entries become exceptions and never block the feed. |

### 1.3 Architecture Layers

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-tech-stack-ing`

`rating` crate: `infra/upstream/usage_feed.rs` (SDK adapter), `infra/upstream/usage_types.rs`
(types-registry declarations), `domain/ingest.rs` (normalization, pure),
`infra/storage/usage_repo.rs`.

## 2. Principles and Constraints

### 2.1 Design Principles

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-normalize-once-ing`
  An entry is stored once, exactly as received plus Rating's attribution columns; later stages read
  the stored row and never re-read the collector.
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-merge-before-round-ing`
  Round the aggregate, never the record: `billingGranularity` applies to the window quantity
  (slice 03).
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-dimension-passthrough-ing`
  Dimension values come only from the entry's declared metadata; ingestion never defaults or
  collapses a value.

### 2.2 Constraints

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-authoritative-dedup-ing`
  `rating_usage_record` is the authoritative record of what Rating counted: an entry contributes to
  counters at most once. The collector's dedup ends at its retention floor (125 days from
  `window_end`); Rating's key is kept for the correction horizon (≥ 7 years). The collector requires
  this of a charging consumer (usage-collector PRD:547, PRD:1503-1504).
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-no-price-ing`
  Ingestion reads no pricing document and pins no catalog version; window placement uses the stored
  meter spec (slice 13).
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-utc-units-ing`
  Times are UTC as received; quantities are in the usage type's canonical unit and never converted.
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-entries-not-aggregates-ing`
  Money is computed from feed entries only, never from the collector's aggregate or query paths
  (usage-collector ADR-0011: "a consumer that computes money reads entries, not aggregates"). The
  superseded `deactivate_usage_record` status flip and `corrects_id` compensation are never consumed.

## 3. Technical Architecture

### 3.1 Domain Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-domain-model-ing`

- **`FeedEntry`** (collector `UsageRecord`, target V1, usage-collector REST YAML:787-889):
  required `id, tenant_id, resource_ref {resource_id, resource_type}, gts_type_id, entry_type
  (record | invalidation), quantity, window_start, window_end, idempotency_key, accepted_at, origin
  (live | backfill)`; optional `subject_ref, metadata`; invalidations only `invalidates, reason_code`.
  `quantity` is a decimal string, at most 28 significant and 28 fractional digits (ADR-0013).
  `window_start = window_end` is a point event.
- **`FeedPage`** — `{entries[≤ 1000], page_info {next_cursor, prev_cursor = null, limit}}`;
  `next_cursor` is never null on a live read and is null only on the page that reaches `until`.
  No position, sequence or watermark is on the page.
- **`NormalizedUsage`** — the `rating_usage_record` row (DESIGN §3.7) minus attribution.
- **`Attribution`** — `{segment_id, segment_version, subscription_id, sub_line_key, payer_tenant_id,
  seller_tenant_id, meter = gts_type_id, dimension_key}`.

### 3.2 Component Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-component-usage-ingestion`

| Component | Responsibility |
|---|---|
| `UsageFeedReader` | Holds the lease of one feed subscription, fetches pages, runs the page transaction. |
| `UsageNormalizer` | Shape, interval and metadata validation, `content_sha256`, `dimension_key` (pure). |
| `UsageTypeCache` | Usage type declarations (fold, canonical unit) from types-registry (§4.7). |
| `Attributor` | Projection lookup `(resource_tenant, resource_id, interval)` → `Attribution` (slice 13 §4.6). |
| `UsageRepo` | Insert-if-absent, invalidation application, exception writes. |

### 3.3 API Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-interface-ingest-ing`

Consumed — **DOCUMENTED on upstream `main`, NOT YET IMPLEMENTED in code** (usage-collector
DESIGN:1127-1142, REST `GET /usage-collector/v1/feed`):

```rust
async fn read_usage_feed(&self, ctx: &SecurityContext, subscription: &FeedSubscription,
    start: FeedStart<&CursorV1>, until: Option<&CursorV1>, limit: Option<u64>)
    -> Result<FeedPage, UsageCollectorError>;
```

| Element | Semantics Rating relies on (usage-collector reference) |
|---|---|
| `FeedSubscription` | the set of GTS types one consumer reads; 1..100 types per subscription (YAML:342-359) |
| `FeedStart::Oldest` | the oldest retained entry; never refused (DESIGN:923-926, 1324) |
| `FeedStart::After(cursor)` | continue after a delivered page; the cursor binds the subscription (`INVALID_CURSOR`, `FILTER_MISMATCH` otherwise) |
| `until` | a later cursor bounding a replay; a bounded replay is identical entry for entry (DESIGN:919-920) |
| `limit` | 1..1 000, default 100 |
| ordering | one deterministic plugin-chosen order; the only promise is that an invalidation follows its target (DESIGN:600) |
| consistency | pages carry only settled entries; nothing appears behind a returned cursor; the feed is prefix-stable, and there is no snapshot token (ADR-0011) |
| retention | `InvalidArgument(CursorBeyondRetention)` when retention removed an entry after the cursor; a cursor within the 35-day replay horizon is served (PRD:683-699) |
| freshness | acceptance → feed visibility p95 ≤ 5 min is a plugin readiness gate for charging consumers (PRD:884-892) |

The feed subscription set is gear config `rating.usage_feed.subscriptions` (lists of ≤ 100 GTS
types). Provided: nothing externally — there is no usage ingestion API on Rating.

### 3.4 Internal Dependencies

`Attributor` reads the projection of slice 13; `CounterMaterializer::apply` (slice 13) runs inside
the page transaction; child work is enqueued through the outbox in the same transaction (slice 14).

### 3.5 External Dependencies

usage-collector (SEAMS §B), subscriptions attribution (SEAMS S-3, R-25), types-registry usage type
declarations (SEAMS U-9).

### 3.6 Interactions and Sequences

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-ingest-ing`
  The page transaction is DESIGN §3.6 Flow A. Example (hourly cloudlets):

  ```text
  entry   id=0192…a1  entry_type=record  tenant=T10  gts_type_id=gts.cf.oss.cloudlet_hours.v1~ (illustrative)
          resource_ref={ct-7f3a, container}  window=[2026-10-08T10:00Z, 2026-10-08T11:00Z)
          quantity="8"  accepted_at=2026-10-08T11:00:07Z  origin=live
  attribution ct-7f3a @ window → segment SEG-4 v2 → subscription S42, sub_line_key plan#1,
          payer C1, seller S1
  stored  rating_usage_record(T10, 0192…a1) meter=gts.cf.oss.cloudlet_hours.v1~,
          dimension_key="", window_start=2026-10-08T10:00Z (per_hour spec of S42)
  counter agg_key{S1,C1,T10,S42,plan#1,meter,"",subscription_line} window 10:00 slice 10:00
          q 0 → 8, q_version 0 → 1
  work    enqueue child wk1|F-USAGE-OCT|2026-10-08T10:00Z|agg  (provisional)
  ```

  An entry `[10:30, 11:30)` would be `boundary_split_required` under a `per_hour` spec (T-D-48).

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-correction-intake-ing`
  An invalidation entry is stored in the same page transaction as any other entry; its target's
  counter receives `−target.quantity` and the child is enqueued (§4.4; DESIGN Flow B).

### 3.7 Database Schemas and Tables

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-storage-ingestion-ing`
  `rating_usage_record`, `rating_source_checkpoint`, `rating_exception` — DESIGN §3.7.

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-storage-tiering-ing`
  `rating_usage_record` may be tiered: rows whose windows are final and older than K months move to
  immutable, digest-verified objects in an S3-compatible store partitioned by `(tenant_id,
  window_start month)`; the archiver verifies count and digest against a
  `rating_usage_archive_manifest (tenant_id, partition, row_count, digest, object_ref)` row before
  deleting hot rows. Cold data is read only by re-materialization, replay and audit. The dedup keys
  stay hot.

### 3.8 Deployment Topology

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-deployment-ing`
  One active reader per feed subscription (lease `rating:usage-feed:{subscription_id}`); throughput
  is scaled by splitting the priced GTS types over more subscriptions.

## 4. Additional Context

### 4.1 Collector Entry → Rating Usage Mapping (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-normalization-ing`

| Rating field | Collector target V1 (upstream `main` docs) | Collector code today | Treatment |
|---|---|---|---|
| `usage_record_id` | `id` = UUIDv5 over `(tenant_id, gts_type_id, idempotency_key, window_start, window_end, entry_type)` (ADR-0007) | `id` = UUIDv5 over `(tenant, gts_id, created_at, key)` | copied |
| `tenant_id` | `tenant_id` | `tenant_id` | copied; the record's resource tenant |
| `gts_type` | `gts_type_id` | `gts_id` | copied |
| `meter` | — | — | **derived**: `= gts_type_id` (R-09); the price row is resolved at rating time from the pinned plan |
| `resource_type`, `resource_id` | `resource_ref` | `resource_ref` | copied |
| `subject_ref` | `subject_ref` | `subject_ref` | copied (lineage only) |
| `interval_start`, `interval_end` | `window_start`, `window_end` (half-open; equal = point event) | **unavailable** (`created_at` only) | copied; code-today records are not rateable (`interval_missing`) |
| `quantity` | `quantity` (decimal string, ≤ 28 fractional digits) | `value` | copied as `NUMERIC` |
| unit, fold | not on the entry; on the types-registry declaration | `UsageKind` counter/gauge | **resolved** from types-registry (§4.7) |
| `dimension_key` | `metadata` (closed map of declared keys) | `metadata` | **derived**: R-16 encoding of the keys the pinned row declares; launch: `""` |
| `idempotency_key` | `idempotency_key` | `idempotency_key` | copied (natural key) |
| `entry_type` | `record` \| `invalidation` | — | copied |
| `invalidates_id`, `reason_code` | `invalidates` (server-stamped), `reason_code` | — | copied |
| `origin` | `live` \| `backfill` (the route, not the age) | — | copied |
| `accepted_at` | `accepted_at` (server) | — | copied; late-arrival detection (`accepted_at` vs `window_end`) |
| `subscription_id`, `sub_line_key`, payer, seller | — (commercial identity is the consumer's, PRD:513) | — | **resolved** through the attribution projection (§4.2); **CURRENT: no source** |
| — | removed from the target trait | `status`, `corrects_id`, `deactivate_usage_record` | **obsolete**: never consumed |

- `content_sha256` = SHA-256 over the canonical serialization of every copied field (metadata
  sorted by key).
- An entry with `window_end < window_start`, an unknown GTS type, or metadata keys outside the type's
  declared set is `usage_malformed`.
- Backfilled entries (`origin = backfill`, admitted up to 90 days back through the collector's
  backfill route) are processed identically.

### 4.2 Attribution (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-session-merge-ing`
  **Attribution rule** (this ID once named the session-merge rule):

- A record is attributed by `(resource_tenant_id = tenant_id, resource_id, interval)` to the segment
  whose `[valid_from, valid_to)` contains the whole interval (slice 13 §4.6).
- No covering segment ⇒ `awaiting_attribution`, stored uncounted; applied when a segment arrives.
- An interval crossing a segment boundary (payer transfer, line change) ⇒
  `boundary_split_required` (T-D-48, T-D-50).
- Two segments covering the interval ⇒ `quarantined` (`usage_ambiguous_attribution`); never an
  arbitrary choice (Atlas P2).
- Attribution is set once per entry and records the `(segment_id, segment_version)` used. A later
  segment version that changes the owner of an already-counted interval is a correction: the entry is
  un-counted from the old aggregation key and counted under the new one in one transaction, and both
  children are re-evaluated (slice 08).

### 4.3 Usage Dedup (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-usage-dedup-ing`

- Primary key `(tenant_id, usage_record_id)`; insert is `ON CONFLICT DO NOTHING`; a conflicting row
  with a different `content_sha256` raises `usage_content_conflict` and the stored row stands.
- Defence key: unique `(tenant_id, gts_type, idempotency_key, interval_start, interval_end,
  entry_type)` — the collector's own dedup identity. An entry with a new id but an existing natural
  key is a conflict, not a new measurement. The key is never weaker than the upstream scope, because
  the upstream id is a function of exactly that identity.
- Replay of any feed range — including a full replay from `Oldest` — is harmless.

### 4.4 Invalidations, Replacements and Negative Quantities (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-correction-intake-ing`

- An invalidation is a faithful copy of its target with `entry_type = invalidation`, a server-stamped
  `invalidates` and a `reason_code`; its quantity is an echo, not a negation (usage-collector
  DESIGN:589-590). Rating stores it (unique `(tenant_id, invalidates_id)`) and, if the target is
  counted, applies `−target.quantity` to the target's counter slice and enqueues the child. The feed
  places an invalidation after its target.
- At most one invalidation per record (a duplicate with the same reason is absorbed upstream; Rating's
  unique key absorbs any replay). There is no invalidation of an invalidation.
- **Replacement is not atomic upstream**: an invalidation, then a fresh record under a new key with
  the same attribution and period (usage-collector PRD:436-438). Rating counts each when it arrives;
  the window may briefly show the target withdrawn and the replacement not yet present. That state is
  never treated as settled: under `evidence_mode = full` the coverage digest does not match until both
  are stored (slice 14 §4.5); under `delay_only` (R-21) a later replacement produces a new revision.
- Target not stored ⇒ `invalidation_target_unknown`, retried every 15 minutes.
- A negative-quantity record is an ordinary measurement. A window driven below zero fails closed
  `negative_window_quantity`.

### 4.5 Exceptions (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-quarantine-ing`

| `reason_code` | Cursor advances | Retry | Resolution |
|---|---|---|---|
| `usage_malformed` | yes | operator | collector fix, `POST /exceptions/{id}:retry` |
| `interval_missing` | yes | operator | the collector's target V1 record (intervals) |
| `usage_type_not_chargeable` | yes | on declaration change | the type's fold is not `SUM` (§4.7) |
| `awaiting_attribution` (state, exception after 1 h) | yes | on segment arrival, every 15 min | automatic |
| `usage_ambiguous_attribution` | yes | no | Subscriptions fix |
| `boundary_split_required` | yes | on replacement | emitter re-split |
| `invalidation_target_unknown` | yes | every 15 min | automatic or operator |
| `usage_content_conflict` | yes | no | investigation (alarm) |

Operator retry (`usage × retry`) is authorized and audited because it can create charges. An
exception never blocks the feed.

### 4.6 Feed Checkpoint, Retention and Backpressure (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-feed-checkpoint-ing`

- **Checkpoint**: `rating_source_checkpoint['usage:{subscription_id}'] = {cursor}`, written only by
  the page transaction with a CAS on `cursor`. The collector's cursor is the only position; there is
  no snapshot token, sequence or watermark to store.
- **Advance**: after the page's effects; a crash before commit re-reads the page from the old cursor.
- **Bootstrap**: a new subscription starts at `FeedStart::Oldest`. Adding a GTS type is a new
  subscription (the cursor binds the subscription) that bootstraps from `Oldest`; dedup absorbs the
  overlap.
- **Widened authorization scope**: the cursor does not bind the PDP scope; entries that become
  visible behind the cursor are not delivered. When Rating's service grant widens (for example a new
  tenant), Rating replays that subscription from `Oldest` (usage-collector PRD:669).
- **Retention**: `CursorBeyondRetention` restarts the subscription from `Oldest` and raises
  `rating_feed_restart_total` (page). Entries removed upstream that Rating never stored are lost: the
  retention floor (125 days from `window_end`) must exceed the largest finalization delay plus the
  supported outage window (Atlas C09).
- **Bounded replay** (reconciliation): `read_usage_feed(subscription, After(c1), until = c2)` re-reads
  a delivered range identically; inserts are no-ops.
- **Backpressure**: a reader stops fetching while `rating.child_work` depth exceeds
  `max_queue_depth` (default 1 000 000) and resumes below 80 %; nothing is dropped.
- **Reconciliation**: daily per `(tenant, gts_type)` against the collector's reconciliation metadata
  (`GET /usage-collector/v1/reconciliation`: accepted count, quantity summary, watermarks). The route
  is REST-only and operator-scoped; whether Rating's service identity may call it is UNKNOWN /
  EXTERNAL CONTRACT REQUIRED. Watermarks prove nothing about completeness (ADR-0011).

### 4.7 Usage Type Declarations (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-usage-types-ing`

- Declarations live in types-registry, not in the collector (usage-collector ADR-0008): required
  `aggregation_fold ∈ {SUM, COUNT, MAX, MIN, LATEST}`, `canonical_unit`, `retention`; optional
  `nominal_sampling_interval` (`schemas/usage_record.v1.schema.json:44-65`). The collector serves no
  type reads.
- Rating reads a declaration through the types-registry SDK (`TypesRegistryClient` schema reads)
  and caches it by GTS type id (declarations are immutable). A typed accessor returning fold, unit and
  accrual method is not specified by anybody (Atlas Plus X7): UNKNOWN / EXTERNAL CONTRACT REQUIRED.
- Only `SUM` is chargeable (usage-collector ADR-0009, ADR-0011); entries of a type with another fold
  are stored and raise `usage_type_not_chargeable` (R-06).
- `nominal_sampling_interval`, when declared, is the input to Rating's stall signal per resource
  (`rating_usage_stall_seconds`); stall detection is the consumer's (usage-collector PRD:749).

## 5. Traceability

**Traces to**: `cpt-cf-bss-rating-fr-dimension-population-contract`

- **DESIGN**: §3.6 Flow A, Flow B; §3.7; §4.2; §4.3.
- **SEAMS**: U-1…U-14, S-3, §L C05, §M-5, §M-13.
- **Decisions**: R-01, R-06, R-09, R-16, R-21, R-25, T-D-36, T-D-48, T-D-51, T-D-52.
- **Usage collector (upstream `main`)**: DESIGN §3.3, ADR-0007, ADR-0008, ADR-0009, ADR-0010,
  ADR-0011, ADR-0013.
