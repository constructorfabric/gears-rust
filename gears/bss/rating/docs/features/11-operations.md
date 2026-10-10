# Feature: Operations, Reconciliation and Data Lifecycle

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-featstatus-operations-implemented`

- [ ] `p1` - `cpt-cf-bss-rating-feature-operations`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Inspect and retry an exception](#21-inspect-and-retry-an-exception)
  - [2.2 Resolve a source loss](#22-resolve-a-source-loss)
  - [2.3 Place and release a retention hold](#23-place-and-release-a-retention-hold)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Run the reconciliation checks](#31-run-the-reconciliation-checks)
  - [3.2 Narrow and repair a source loss](#32-narrow-and-repair-a-source-loss)
  - [3.3 Purge under retention and holds](#33-purge-under-retention-and-holds)
  - [3.4 Handle a deleted tenant](#34-handle-a-deleted-tenant)
  - [3.5 Verify the non-functional targets](#35-verify-the-non-functional-targets)
  - [Migrated namespace procedures](#migrated-namespace-procedures)
- [4. States (CDSL)](#4-states-cdsl)
  - [4.1 Retention hold](#41-retention-hold)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Operator REST plane](#51-operator-rest-plane)
  - [5.2 Reconciliation and source-loss repair](#52-reconciliation-and-source-loss-repair)
  - [5.3 Observability](#53-observability)
  - [5.4 Data lifecycle](#54-data-lifecycle)
  - [5.5 Backpressure, replay and NFR verification](#55-backpressure-replay-and-nfr-verification)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Detailed Behavior Contracts](#7-detailed-behavior-contracts)
  - [Billing delivery and operations: Backpressure, Replay, Cold Start (normative)](#billing-delivery-and-operations-backpressure-replay-cold-start-normative)
  - [Billing delivery and operations: NFR Verification (normative)](#billing-delivery-and-operations-nfr-verification-normative)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Give operators the audited surfaces to see and repair what the pipeline could not finish —
exceptions, source losses, retention holds — run the reconciliation checks that detect lost,
duplicated or drifting data, emit the gear's metrics, logs, spans and alerts, keep seven years of
results replayable through retention, holds and archive integrity, and prove the sizing targets with
the NFR verification suite before implementation lock.

### 1.2 Purpose

The pipeline's guarantees hold only if a failure is visible and repairable: an open exception, a
quarantined fact, an unresolved source loss, a missed fact, counter drift, a lost delivery or a
non-deterministic replay must each have a signal and a repair path. Retention and holds keep the
financial record and its replay inputs for as long as they may be audited, and the load-test gates
of DESIGN §4.9 turn the sizing targets into evidence.

**Requirements** (primary, per DECOMPOSITION): `cpt-cf-bss-rating-nfr-throughput-latency`, `cpt-cf-bss-rating-nfr-horizontal-scale`.

**Principles**: `cpt-cf-bss-rating-principle-evidence-before-final`, `cpt-cf-bss-rating-principle-lanes-bhf`, `cpt-cf-bss-rating-principle-fail-closed`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-bss-rating-actor-platform-operator` | Lists and retries exceptions, releases inbox entries, resolves source losses (all audited) |
| `cpt-cf-bss-rating-actor-finance-analyst` | Places and releases retention holds as the Finance or audit operator (audited) |
| `cpt-cf-bss-rating-actor-rating` | Runs the reconciler, exception sweeper and retention purge under its service identity |
| `cpt-cf-bss-rating-actor-oss-ams` | Source of the tenant-deletion signal once it is defined (`…-upreq-tenant-deletion-signal`) |

Actor labels grant nothing; every operation goes through the operation matrix of DESIGN §4.8.

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §7.1 (throughput, latency, horizontal scale, resilience).
- **Architecture**: [DESIGN.md](../DESIGN.md) [§3.3](../DESIGN.md#33-api-contracts) (operator REST routes — canonical), [§3.8](../DESIGN.md#38-deployment-topology), [§4.4](../DESIGN.md#44-failure-model), [§4.6](../DESIGN.md#46-reconciliation) (checks and repairs — canonical), [§4.7](../DESIGN.md#47-observability) (metrics, logs, spans, alerts — canonical), [§4.8](../DESIGN.md#48-multi-tenancy-and-authorization), [§4.9](../DESIGN.md#49-nfr-mapping) (sizing and acceptance gates), [§4.12](../DESIGN.md#412-data-lifecycle) (retention, holds, archive, tenant deletion — canonical); [16 §4.3](../DESIGN.md#contract-16-4-3) (operational topology). Backpressure, replay, cold start and the NFR test table are in [§7](#7-detailed-behavior-contracts).
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.11.
- **Dependencies**: [Usage Capture](04-usage-intake.md), [Attribution and Window Counters](05-attribution-counters.md), [Facts and Scheduling](06-fact-scheduling.md), [Child Evaluation](07-child-evaluation.md), [Corrections and Re-rate](08-corrections-rerate.md), [Roll-up and Delivery](09-rollup-delivery.md); shared surfaces from [Foundation](01-foundation.md).
- **Consumers**: operators and on-call; Finance and audit.
- **Upstream**: `cpt-cf-bss-rating-upreq-usage-reconciliation-access` ([UPSTREAM_REQS §2.1](../UPSTREAM_REQS.md#21-usage-collector)), `cpt-cf-bss-rating-upreq-tenant-deletion-signal` ([UPSTREAM_REQS §2.13](../UPSTREAM_REQS.md#213-account-management-and-tenant-resolver)).

**UI applicability**: no user interface is specified; consoles that call the REST plane own their UI. RFC 9457 problem details, keyset paging and non-disclosing `404` answers are this feature's API-usability obligations.

## 2. Actor Flows (CDSL)

**Use cases**: none in the PRD; the flows are operator flows over the REST plane of DESIGN §3.3.

### 2.1 Inspect and retry an exception

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-ops-exception-retry`

**Actor**: `cpt-cf-bss-rating-actor-platform-operator`

**Success Scenarios**: the operator lists open exceptions by `reason_code` or subject and queues one retry; the retry re-runs the subject's own conditional step and is audited.

**Error Scenarios**: exception already resolved (`409 FailedPrecondition`); outside the caller's scope (`404`); missing permission (`403`).

**Steps**:
1. [ ] - `p1` - List with `GET /bss-rating/v1/exceptions` (`usage × read`, keyset cursor, `$filter` on `reason_code`, `subject_kind`, `resolved`) - `inst-er-list`
2. [ ] - `p1` - Receive `POST /bss-rating/v1/exceptions/{id}:retry` with a reason; authorize `usage × retry` and audit it - `inst-er-retry`
3. [ ] - `p1` - **IF** the exception is resolved, refuse `409 FailedPrecondition` - `inst-er-resolved`
4. [ ] - `p1` - Queue one retry of the subject: a usage record is re-interpreted from its stored raw entry and can be counted only through the conditional `count` transition, so a withdrawn record is never counted - `inst-er-queue`
5. [ ] - `p1` - **RETURN** `202` with `ExceptionView` - `inst-er-return`

### 2.2 Resolve a source loss

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-ops-source-loss-resolve`

**Actor**: `cpt-cf-bss-rating-actor-platform-operator`

**Success Scenarios**: with evidence that covers the open scope the loss is resolved `repaired` or `accepted_loss`; the affected children are re-checked by the evidence gate.

**Error Scenarios**: the evidence does not cover the open scope (`409 FailedPrecondition`); missing `source_loss × resolve`.

**Steps**:
1. [ ] - `p1` - List with `GET /bss-rating/v1/source-losses` (`usage × read`, `$filter` on `state`, `source_id`) - `inst-sr-list`
2. [ ] - `p1` - Receive `POST /bss-rating/v1/source-losses/{id}:resolve` with `{evidence_ref, resolution}`; authorize `source_loss × resolve` and audit it - `inst-sr-authorize`
3. [ ] - `p1` - **IF** the evidence does not cover every remaining tainted day, refuse `409 FailedPrecondition` - `inst-sr-coverage`
4. [ ] - `p1` - Set the loss `resolved` with resolution, evidence, actor and time; bump `input_generation` of the affected children so they are re-evaluated - `inst-sr-resolve`
5. [ ] - `p1` - **RETURN** `SourceLossView` - `inst-sr-return`

A restart, a metric reset or the passage of time never resolves a loss (T-D-62).

### 2.3 Place and release a retention hold

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-ops-retention-hold`

**Actor**: `cpt-cf-bss-rating-actor-finance-analyst` (Finance or audit operator)

**Success Scenarios**: a hold blocks every purge its selector matches until it is released; both actions are audited.

**Error Scenarios**: selector outside the caller's scope (`403 PermissionDenied`, DESIGN §4.8); hold already released (`409 FailedPrecondition`); same `Idempotency-Key` with a different body (`409 AlreadyExists`).

**Steps**:
1. [ ] - `p1` - Receive `POST /bss-rating/v1/retention-holds` with `{scope, reason}` and a required `Idempotency-Key`; authorize `retention_hold × place` - `inst-rh-place`
2. [ ] - `p1` - **IF** the selector reaches outside the caller's scope, refuse `403 PermissionDenied` (DESIGN §4.8, V23–V25) - `inst-rh-scope`
3. [ ] - `p1` - Insert `bss_rating__retention_hold` and the operation key in one transaction; **RETURN** `201` - `inst-rh-insert`
4. [ ] - `p1` - Release through `POST /bss-rating/v1/retention-holds/{id}:release` (`retention_hold × release`), setting `released_at` - `inst-rh-release`

## 3. Processes / Business Logic (CDSL)

### 3.1 Run the reconciliation checks

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-ops-reconcile`

**Input**: a tenant shard, under the Cluster lock `bss-rating/reconcile/{shard}`.

**Output**: repairs, re-enqueued work, exceptions, and `rating_reconciliation_mismatch_total{check}`.

1. [ ] - `p1` - **FOR EACH** check of [DESIGN §4.6](../DESIGN.md#46-reconciliation) (usage completeness, source loss, coverage, attribution backlog, counter integrity, expected-child completeness, fact coverage, delivery coverage, replay determinism) - `inst-rc-each`
   1. [ ] - `p1` - Compare against its source and apply the repair that §4.6 names; never repair by fabricating input - `inst-rc-repair`
2. [ ] - `p1` - **IF** counter integrity fails, re-materialize through [Attribution and Window Counters](05-attribution-counters.md) and re-evaluate - `inst-rc-counters`
3. [ ] - `p1` - **IF** replay determinism fails, page and freeze deploys; **IF** delivery coverage fails, page - `inst-rc-page`
4. [ ] - `p1` - Run in small transactions; a crash resumes from the next check - `inst-rc-small-tx`

### 3.2 Narrow and repair a source loss

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-ops-source-loss-repair`

**Input**: an `open` or `narrowed` loss and the collector's reconciliation metadata per `(tenant, GTS type, day)`.

**Output**: a narrower `tainted_days` set, a `resolved(repaired)` loss, or no change.

1. [ ] - `p1` - Read the collector's reconciliation metadata under Rating's service identity; **IF** access is not granted (`…-upreq-usage-reconciliation-access`), leave the loss to an audited operator resolution - `inst-lr-read`
2. [ ] - `p1` - **FOR EACH** tainted day whose accepted count and quantity sum match Σ captured entries, remove it from `tainted_days` and set `narrowed` - `inst-lr-narrow`
3. [ ] - `p1` - For a mismatching day, request a collector backfill from the collector's operators (Rating cannot start it) and wait for the entries to be captured - `inst-lr-backfill`
4. [ ] - `p1` - **IF** every tainted day matches, set `resolved(repaired)` - `inst-lr-repaired`
5. [ ] - `p1` - Bump `input_generation` of children of any day removed from the taint - `inst-lr-bump`

### 3.3 Purge under retention and holds

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-ops-retention-purge`

**Input**: the retention classes of [DESIGN §4.12](../DESIGN.md#412-data-lifecycle), under the Cluster lock `bss-rating/purge`.

**Output**: expired partitions and operational rows removed; nothing under a hold or still referenced removed.

1. [ ] - `p1` - **FOR EACH** expired financial partition, drop it only if no hold matches - `inst-pg-financial`
2. [ ] - `p1` - Delete replay inputs only after the reference check against retained result manifests - `inst-pg-replay`
3. [ ] - `p1` - Before deleting hot rows of an archived partition, verify its archive manifest (row counts and SHA-256 per file) - `inst-pg-archive`
4. [ ] - `p1` - Delete operational rows at their own retention (inbox payloads after `applied`, resolved exceptions, `billing_hint`: 400 days; `operation`: 7 days) - `inst-pg-operational`
5. [ ] - `p1` - Never remove an engine generation here; that needs an ADR (DESIGN §4.1) - `inst-pg-engine`

### 3.4 Handle a deleted tenant

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-algo-ops-tenant-deletion`

**Input**: the account-management tenant-deletion signal — UNKNOWN / EXTERNAL CONTRACT REQUIRED (`…-upreq-tenant-deletion-signal`).

**Output**: intake stopped and queued work cancelled for the tenant; records kept until retention expiry.

1. [ ] - `p1` - Stop intake for the tenant: readers route its entries to an exception, never to charges - `inst-td-intake`
2. [ ] - `p1` - Let workers that find the tenant inactive acknowledge their work items without effect - `inst-td-work`
3. [ ] - `p1` - Keep financial records and replay inputs until retention expiry, or longer under a hold; purge them with their partition - `inst-td-keep`

Until the signal exists, Rating keeps every record until retention expiry.

### 3.5 Verify the non-functional targets

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-ops-nfr-gates`

**Input**: a load-test environment with the contract fakes.

**Output**: pass or fail per test of [16 §4.5](#contract-16-4-5) and per acceptance gate of [DESIGN §4.9](../DESIGN.md#49-nfr-mapping).

1. [ ] - `p1` - Run the 16 §4.5 test table (sustained load, hourly burst, rater latency, binding lookup, crash/retry, scheduler outage, replay, determinism, re-rate storm) - `inst-nfr-table`
2. [ ] - `p1` - Run the DESIGN §4.9 gates: hot-tenant skew, hourly burst, catch-up after a 24 h outage, a 1M-child re-rate beside live traffic, cold replay reads, a 744-child roll-up, `scope_for_window` fan-out - `inst-nfr-gates`
3. [ ] - `p1` - **RETURN** the results as the evidence required before implementation lock; a target is not met until measured - `inst-nfr-return`

<a id="register-procedures"></a>

### Migrated namespace procedures

The normative procedures of the former slice documents that this feature implements are defined here and specified in [§7](#7-detailed-behavior-contracts); each entry links to its contract.

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-backpressure-replay-bhf`
  — Billing delivery and operations — Backpressure, Replay, Cold Start (normative) ([contract](#contract-16-4-4))

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-nfr-verification-bhf`
  — Billing delivery and operations — NFR Verification (normative) ([contract](#contract-16-4-5))

## 4. States (CDSL)

### 4.1 Retention hold

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-state-ops-retention-hold`

**States**: `placed`, `released`.

**Initial State**: `placed`.

**Transitions**:
1. [ ] - `p1` - **FROM** — **TO** `placed` **WHEN** an authorized hold is created - `inst-hs-place`
2. [ ] - `p1` - **FROM** `placed` **TO** `released` **WHEN** an authorized release sets `released_at` - `inst-hs-release`

Source-loss states are owned by [Usage Capture](04-usage-intake.md); this feature drives their `narrowed` and `resolved` transitions.

## 5. Definitions of Done

### 5.1 Operator REST plane

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-ops-rest`

The system **MUST** serve the exception, inbox-release, source-loss and retention-hold routes of DESIGN §3.3 through OperationBuilder with the listed permissions, statuses, keyset paging and idempotency rules, audit every `retry`, `release`, `resolve`, `place` and hold `release`, and answer ids outside the caller's scope with `404`.

**Implements**: `cpt-cf-bss-rating-flow-ops-exception-retry`, `cpt-cf-bss-rating-flow-ops-source-loss-resolve`, `cpt-cf-bss-rating-flow-ops-retention-hold`, `cpt-cf-bss-rating-state-ops-retention-hold`.

**Constraints**: `cpt-cf-bss-rating-constraint-domain-boundaries`.

**Touches**: API: `GET /bss-rating/v1/exceptions`, `POST /exceptions/{id}:retry`, `POST /inbox/{id}:release` (its flow is implemented by Foundation, `cpt-cf-bss-rating-flow-foundation-inbox-release`), `GET /source-losses`, `POST /source-losses/{id}:resolve`, `GET|POST /retention-holds`, `POST /retention-holds/{id}:release` (`cpt-cf-bss-rating-interface-operator-rest`); `cpt-cf-bss-rating-dbtable-exception`, `cpt-cf-bss-rating-dbtable-source-loss`, `bss_rating__retention_hold`.

### 5.2 Reconciliation and source-loss repair

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-ops-reconciliation`

The system **MUST** run every check of DESIGN §4.6 per tenant shard with its repair, narrow and resolve source losses only from authoritative evidence or an audited resolution, and page on replay-determinism or delivery-coverage mismatches.

**Implements**: `cpt-cf-bss-rating-algo-ops-reconcile`, `cpt-cf-bss-rating-algo-ops-source-loss-repair`.

**Constraints**: `cpt-cf-bss-rating-constraint-domain-boundaries`.

**Touches**: collector reconciliation metadata; `cpt-cf-bss-rating-dbtable-source-loss`, `cpt-cf-bss-rating-dbtable-window-counter`, `cpt-cf-bss-rating-dbtable-child-window`, `cpt-cf-bss-rating-dbtable-charge-feed`.

### 5.3 Observability

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-ops-observability`

The system **MUST** emit the metrics, structured logs, spans and alerts of [DESIGN §4.7](../DESIGN.md#47-observability) through the metrics port, never logging other tenants' quantities or undeclared metadata.

**Implements**: `cpt-cf-bss-rating-algo-ops-reconcile`, `cpt-cf-bss-rating-algo-backpressure-replay-bhf`.

**Constraints**: `cpt-cf-bss-rating-constraint-domain-boundaries`.

**Touches**: metrics port (`domain/ports`) and its adapter (`infra/metrics.rs`); OpenTelemetry.

### 5.4 Data lifecycle

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-ops-lifecycle`

The system **MUST** apply the retention classes of DESIGN §4.12, block every purge a hold matches, verify archive manifests before deleting hot rows and on replay reads, and acknowledge without effect any work item whose row was purged or whose tenant was deleted.

**Implements**: `cpt-cf-bss-rating-algo-ops-retention-purge`, `cpt-cf-bss-rating-algo-ops-tenant-deletion`.

**Constraints**: `cpt-cf-bss-rating-constraint-domain-boundaries`.

**Touches**: `bss_rating__retention_hold`, `bss_rating__usage_archive_manifest`; every partitioned table.

### 5.5 Backpressure, replay and NFR verification

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-ops-nfr`

The system **MUST** behave as [16 §4.4](#contract-16-4-4) states under backlog, replay and cold start, and pass the [16 §4.5](#contract-16-4-5) test table and the DESIGN §4.9 acceptance gates before implementation lock.

**Implements**: `cpt-cf-bss-rating-algo-ops-nfr-gates`, `cpt-cf-bss-rating-algo-backpressure-replay-bhf`, `cpt-cf-bss-rating-algo-nfr-verification-bhf`.

**Constraints**: `cpt-cf-bss-rating-constraint-domain-boundaries`.

**Touches**: all work queues; the contract fakes of [Foundation](01-foundation.md).

## 6. Acceptance Criteria

- [ ] A retry of a resolved exception returns `409 FailedPrecondition`; a retry of a withdrawn record's exception never counts it.
- [ ] A source-loss resolution whose evidence does not cover the open scope returns `409 FailedPrecondition`; a restart or a metric reset leaves the loss open.
- [ ] A purge leaves every row a hold matches and every replay input a retained result references; a hold placed and released is audited twice.
- [ ] A work item whose row was purged is acknowledged without effect.
- [ ] Each DESIGN §4.6 check, seeded with its mismatch, raises `rating_reconciliation_mismatch_total{check}` and applies its repair; a replay-determinism mismatch pages.
- [ ] Every alert of DESIGN §4.7 fires in a test that provokes its condition.
- [ ] Every row of the 16 §4.5 test table and every DESIGN §4.9 gate has a recorded measurement before implementation lock.

Acceptance vectors owned by this feature (DESIGN §4.13 index):

| # | Scenario | Expected state |
|---|---|---|
| V06 | Reconciliation matches some tainted days | loss `narrowed`; only children of matched days may finalize |

## 7. Detailed Behavior Contracts

**Contract namespace 16.** Section numbers inside the detailed contracts below resolve through the [contract address index](../DECOMPOSITION.md#4-contract-address-index), not the overview section numbers above. A bare section reference names a section of the same namespace; "DESIGN §n" names §1–§5 of [DESIGN.md](../DESIGN.md).

The following sections keep the runtime procedures of these namespaces — their interactions and sequences and the procedural parts of their §4. The flows, processes and acceptance criteria above summarize them; the architecture, schemas and evaluation semantics they rely on are defined once in [DESIGN.md](../DESIGN.md) §6.

<a id="contract-16-4-4"></a>

<!-- contract:16-billing-handoff-operations:4.4 -->
### Billing delivery and operations: Backpressure, Replay, Cold Start (normative)

**Contract**: `cpt-cf-bss-rating-algo-backpressure-replay-bhf` (`p1`), defined in [§3 migrated procedures](#register-procedures).

- Backpressure: feed readers stop above `max_queue_depth` and resume below 80 %; provisional
  evaluations are skipped under backlog ([14 §4.4](06-fact-scheduling.md#contract-14-4-4)); nothing is dropped.
- Replay: any feed or inbox source can be re-read (idempotent); any final result can be re-evaluated
  from stored inputs under its recorded engine generation (DESIGN §4.1); any delivery can be
  re-published or re-pulled.
- Source loss: a `CursorBeyondRetention` or an unidentifiable entry opens a source loss that blocks
  finality of the affected scope under every evidence mode; operators list and resolve losses
  through `GET /source-losses` and `POST /source-losses/{id}:resolve` (`source_loss × resolve`,
  audited) with authoritative evidence (DESIGN §4.6). A restart or a metric reset never resolves
  one.
- Lifecycle: retention classes, holds, archive integrity and tenant deletion are DESIGN §4.12; a
  worker that finds its row purged or its tenant deleted acknowledges without effect.
- Cold start: caches start empty and fall back to `bss_rating__pricing_binding`,
  `bss_rating__derived_declaration` and `bss_rating__subscription_version`, then to the upstream
  (`PricingReadV1::resolve`, the Products declaration read). Correctness never depends on a warm cache.

<!-- /contract -->

<a id="contract-16-4-5"></a>

<!-- contract:16-billing-handoff-operations:4.5 -->
### Billing delivery and operations: NFR Verification (normative)

**Contract**: `cpt-cf-bss-rating-algo-nfr-verification-bhf` (`p1`), defined in [§3 migrated procedures](#register-procedures).

| Test | Pass criterion |
|---|---|
| Sustained load | 10M usage records/day/region (≈ 116/s mean, 1 000/s peak for 1 h); feed lag < 5 min; provisional rating lag p95 < 5 min |
| Hourly finalization burst | 100 000 hourly lines due at one `HH:00 + delay`: all children final (given evidence) within 15 min |
| Rater latency | `rating_rating_duration_seconds` p95 < 1 s at peak |
| Binding lookup | cached binding read p95 ≤ 100 ms incl. cold `bss_rating__pricing_binding` reads; a cache miss on a provisional child is one `resolve` call per item (≤ 1 000 pins, pricing D-419) |
| Crash/retry | kill readers, scheduler, raters and roll-up at random under load; counter integrity, expected-child completeness and delivery coverage report zero mismatches; no duplicate revision |
| Scheduler outage | 3 h scheduler downtime: catch-up produces every due child once (F28) |
| Replay | re-read 7 days of feed and inbox; zero new records, results or deliveries |
| Determinism | re-evaluate 10 000 random final results; identical `input_digest` and amounts |
| Re-rate storm | an administrative re-rate of 1M children keeps normal rating lag p95 < 5 min; correction fairness holds |
| Capacity gates | the DESIGN §4.9 acceptance gates (hot tenant, catch-up after 24 h, cold replay reads, 744-child roll-up) |
| Billing revision rule | DESIGN §4.13 V16: revision 3 after revision 4 ignored; equal revision with a different digest quarantined |
| Authorization | DESIGN §4.13 V23–V25: cross-tenant ids `404`; out-of-scope selector `PermissionDenied`; seller cannot write a platform default |

<!-- /contract -->
