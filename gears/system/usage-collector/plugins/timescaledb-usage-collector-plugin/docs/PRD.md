Created:  2026-07-21 by Virtuozzo International GmbH
Updated:  2026-09-16 by Virtuozzo International GmbH

# PRD — TimescaleDB Usage Collector Storage Plugin

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
  - [3.1 Gear-Specific Environment Constraints](#31-gear-specific-environment-constraints)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 Record Persistence](#51-record-persistence)
  - [5.2 Query & Aggregation](#52-query--aggregation)
  - [5.3 Usage Feed & Reconciliation](#53-usage-feed--reconciliation)
  - [5.4 Data Lifecycle](#54-data-lifecycle)
  - [5.5 Plugin Integration & Error Contract](#55-plugin-integration--error-contract)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Gear-Specific NFRs](#61-gear-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
  - [Ingest a Usage Record with Idempotent Dedup](#ingest-a-usage-record-with-idempotent-dedup)
  - [Register the Backend at Plugin Startup](#register-the-backend-at-plugin-startup)
  - [Read a Feed Page](#read-a-feed-page)
  - [Refuse a Stale Cursor](#refuse-a-stale-cursor)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

> **Abbreviations**: SPI = **Service Provider Interface**; GTS = **Global Type System**. This PRD describes a **storage backend plugin** for the Usage Collector gear.

## 1. Overview

### 1.1 Purpose

The TimescaleDB Usage Collector Storage Plugin (`timescaledb-usage-collector-plugin`) is a storage backend for the Usage Collector gear. It implements the Usage Collector storage SPI (`UsageCollectorPluginV1`, Plugin SPI `cpt-cf-usage-collector-interface-plugin`) on top of PostgreSQL with the TimescaleDB extension, and is the durable system of record for usage entries only.

This PRD specifies **only plugin-specific requirements** for the TimescaleDB backend. All product-level requirements — ingestion semantics, the idempotency contract, the declared aggregation fold, attribution, tenant isolation, authorization, the query/aggregation product surface, correction primitives, usage-type declaration and resolution, and data classification — are defined in the parent gear PRD and are **inherited** by this plugin:

- **Parent PRD (authoritative)**: [../../../docs/PRD.md](../../../docs/PRD.md)

The core owns authentication, PDP authorization, attribution and shape validation, idempotency-key presence and usage-type resolution; the plugin is pure persistence and query and receives only already-authorized, structurally-validated calls.

This PRD is **normative for the gear's target seven-method Plugin SPI**; the shipped crate predates it (DESIGN §4.5).

### 1.2 Background / Problem Statement

The Usage Collector requires at least one deployed storage plugin to reach readiness. Its workload is append-heavy time-series ingestion combined with time-windowed analytical reads at a high throughput envelope (sustained ≥ 10,000 records/sec, per `cpt-cf-usage-collector-nfr-throughput-profile`). A time-series-optimized backend keeps inserts and time-range scans efficient at that envelope while providing the transactional guarantees the append-only invalidation model needs.

TimescaleDB — a PostgreSQL extension — is selected because it provides native time-partitioning (hypertables) for the append-heavy time-series workload while retaining PostgreSQL's ACID transactions, and because that partitioning is what lets the plugin run a per-type retention sweep and an hourly continuous aggregate natively.

### 1.3 Goals (Business Outcomes)

- Provide a production-grade time-series storage backend that satisfies the parent gear's query-latency, ingestion and feed obligations — including the replay-safe usage feed a charging consumer reads — without a separate downstream aggregation or feed layer. **Verification**: load tests against a bound backend within the parent throughput profile (`cpt-cf-usage-collector-nfr-throughput-profile`).
- Enforce per-type retention, driven by each type's current registry-declared retention policy, and idempotent ingestion natively in the backend, so correctness does not depend on gear-side coordination. **Verification**: tests asserting an entry is retained until its type's declared retention policy elapses and a retry never admits a duplicate.
- Keep all TimescaleDB-specific storage logic, schema, and dependencies isolated to this crate so the backend can evolve and be licensed independently of the host gear. **Verification**: conformance to the SDK SPI and a dependency check that the crate does not depend on the host gear crate.

All other business and product goals are defined by the parent Usage Collector PRD.

### 1.4 Glossary

The parent gear glossary is the primary source of truth. The terms below are plugin-specific.

| Term                | Definition                                                                                                                                                                                       |
| ------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Entry               | The ledger unit — an ordinary measurement or a withdrawal. "Usage record" in this document means any entry, withdrawals included, so the raw-list and retention obligations cover withdrawals too. |
| Hypertable          | A TimescaleDB time-partitioned table; usage records are stored in a hypertable partitioned on event time.                                                                                        |
| Retention policy    | A GTS type's declared retention policy, read from the registry, that governs how long the plugin keeps the entries it covers.                                                                    |
| Dedup key           | The tenant, GTS type, idempotency key, covered-period (`window_start`, `window_end`) and entry-type 6-tuple on which usage-record uniqueness is enforced; a record and its invalidation share the first five and differ in entry type. |
| Silent absorb       | An idempotent-retry outcome: a same-dedup-key submission whose canonical fields match the stored record returns the stored record instead of creating a duplicate.                               |
| Keyset pagination   | Seek-based pagination that walks records in a stable order by a cursor rather than by numeric offset.                                                                                            |
| Consistency profile | The read-visibility ceiling a backend deployment advertises above the gear's eventual-consistency floor (per `cpt-cf-usage-collector-nfr-query-freshness`).                                      |
| SPI                 | Service Provider Interface — the Usage Collector storage-plugin contract (`cpt-cf-usage-collector-interface-plugin`) this plugin implements; distinct from the gear SDK client and the REST API. |
| Rollup               | The hourly materialised aggregate that eligible `SUM`/`COUNT` queries are served from in place of a full ledger scan.                                                                             |
| Retention sweep     | The plugin's background process that drops storage holding entries once every type sharing it has passed its declared retention.                                                                |
| Feed position       | The opaque point in feed order a feed cursor carries, issued and interpreted by the plugin alone. Its age is the acceptance instant of the oldest entry of a subscribed GTS type after it, whatever the reader's scope, and a position with no such entry after it is current (DESIGN §3.1, §3.6). |
| Settled horizon     | The oldest transaction holding an id in the PostgreSQL instance; every entry written by an older transaction is final, so the feed serves only entries below it.                                                      |
| Retention floor     | The gear's minimum retention: its backfill window plus its operational replay horizon (`cpt-cf-usage-collector-fr-billing-retention-floor`). The plugin is not configured with it and never refuses a position on age — it refuses on a retention mark (`cpt-cf-uc-plugin-fr-usage-feed`). This plugin requires more retention than the floor (`cpt-cf-uc-plugin-fr-per-type-retention`).                                                               |
| Replay horizon      | H, the deployment's operational replay horizon (`feed_replay_horizon_secs`): the age within which the feed serves every position (`cpt-cf-uc-plugin-fr-usage-feed`). |
| Acceptance-order slack | 2 × `feed_acceptance_slack_secs` + `statement_timeout_secs`: how much earlier than an entry already delivered an entry later in feed order can have been accepted. `feed_acceptance_slack_secs` is the per-entry acceptance tolerance the write path enforces (`cpt-cf-uc-plugin-fr-record-persistence`); the derivation is DESIGN §3.6 `cpt-cf-uc-plugin-seq-feed-page`. |
| Dedup level         | The concurrent-submission guarantee a plugin declares: `linearizable` (convergence bound zero) or `eventual` (`cpt-cf-usage-collector-fr-idempotency`). This plugin is `linearizable`.                    |

## 2. Actors

> **Note**: Stakeholder needs are managed at project/task level. The plugin's product-facing actors (usage sources, usage consumers, tenant administrators) interact with the **gear**, not the plugin, and are documented in the gear PRD (`cpt-cf-usage-collector-actor-*`).

### 2.1 Human Actors

This plugin has no direct human actors. Platform operators, developers, and tenant administrators interact only with the Usage Collector gear, never with the plugin directly; operator configuration (DESIGN §3.5) reaches the plugin through the gear's configuration surface.

### 2.2 System Actors

#### Usage Collector Core (Plugin Host)

**ID**: `cpt-cf-uc-plugin-actor-plugin-host`

- **Role**: The Usage Collector gear core — the SPI's sole caller. It authenticates, authorizes and validates every call before invoking the plugin, and owns which plugin is bound as the active backend.

## 3. Operational Concept & Environment

This plugin operates within the standard Gears ToolKit lifecycle: it provisions its schema and registers at startup (`cpt-cf-uc-plugin-fr-registration`), opens no network listener and exposes no REST surface. Foundational runtime, lifecycle and integration patterns are inherited from the parent gear ([../../../docs/PRD.md](../../../docs/PRD.md)) and the platform.

### 3.1 Gear-Specific Environment Constraints

- Requires a PostgreSQL database with the TimescaleDB extension available.
- Requires a TLS-capable database endpoint. The plugin never falls back to plaintext silently; plaintext is reachable only by setting `sslmode=disable` explicitly, which the plugin records as a deliberate opt-out (see `cpt-cf-uc-plugin-nfr-transport-security`).
- The plugin is statically linked into the Usage Collector gear process; database deployment topology (HA, sizing, region) follows the operator's TimescaleDB deployment guide.

## 4. Scope

### 4.1 In Scope

- Full implementation of the Usage Collector storage SPI (`cpt-cf-usage-collector-interface-plugin`) — all seven methods of the gear DESIGN's `UsageCollectorPluginV1` — covering single and batch persistence, converged-only point read, pushed-down aggregation, keyset-paginated raw list, feed pages, and reconciliation metadata.
- Durable system-of-record storage for usage records, persisted in time order.
- In-backend deduplication on the tenant, GTS type, idempotency key, covered-period and entry-type identity.
- Append-only invalidation: a withdrawal is persisted as an ordinary appended entry that names the entry it withdraws, without rewriting the withdrawn entry.
- A replay-safe usage feed over subscribed GTS types (`cpt-cf-uc-plugin-fr-usage-feed`).
- Per-(tenant, GTS type) reconciliation counters and watermarks.
- Converged-only point lookup of an invalidation target.
- Server-side aggregation (SUM / COUNT / MIN / MAX / LATEST with grouping), from a materialised aggregate where eligible, and keyset pagination, both pushed into the backend.
- Per-type retention, driven by each type's current registry-declared retention policy.
- Injection-safe translation of the host-supplied filter, aggregation, and pagination into backend queries.
- Publication of the backend's consistency profile as required by the parent query-freshness contract.
- Push-based OpenTelemetry metrics for the plugin's backend-internal operation.
- Runtime discovery and registration, and operator configuration (DESIGN §3.5).

### 4.2 Out of Scope

- Any product-level behavior owned by the gear core — authentication, PDP authorization, attribution and shape validation, idempotency-key presence enforcement, usage-type resolution and the declared aggregation fold, and metadata closed-shape validation. These are inherited from the parent gear, not re-implemented here.
- Columnar compression of aging data — deferred post-v1; additive and non-breaking to the SPI.
- Preservation of the dedup identity beyond a type's declared retention — the gear's adopted floor (`cpt-cf-uc-plugin-fr-idempotent-dedup`).
- Multi-region replication and cross-region topology — governed by the operator's TimescaleDB deployment and the parent gear's deferred multi-region item.
- Storage backends other than PostgreSQL/TimescaleDB.
- Any REST or network-exposed surface — the plugin exposes only the in-process SPI.

## 5. Functional Requirements

> **Testing strategy**: All requirements are verified via automated tests (unit and integration) unless otherwise specified. Document a verification method only where a non-test approach (analysis, inspection, demonstration) applies. Each requirement lists the gear-level requirement it realizes; there is no plugin-level UPSTREAM_REQS document, so no `Covers` field is used.

### 5.1 Record Persistence

#### Record Persistence (Single and Batch)

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-fr-record-persistence`

The plugin **MUST** persist usage records supplied by the host, both singly and as a batch. A batch **MUST** return one result per input record, positionally aligned to input order, and a conflict or rejection on one record **MUST NOT** fail the others. Caller- and gateway-supplied values (including `metadata`) **MUST** be stored verbatim, without transformation or interpretation. Two same-identity entries inside one batch **MUST** resolve the later against the earlier: absorbed when identical, an idempotency conflict when divergent. The plugin **MUST** refuse, as a retryable error, an entry whose acceptance instant differs from the store's clock at insertion by more than its configured acceptance slack.

- **Rationale**: Faithful, order-preserving persistence is the backend's core responsibility and the foundation of every downstream query and aggregate.
- **Actors**: `cpt-cf-uc-plugin-actor-plugin-host`
- **Realizes (gear)**: `cpt-cf-usage-collector-fr-ingestion`, `cpt-cf-usage-collector-fr-record-metadata`

#### Idempotent Deduplication

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-fr-idempotent-dedup`

The plugin **MUST** deduplicate records in the backend on the identity of tenant, GTS type, idempotency key, covered period and entry type. Every constraint, conflict target, conflict read-back and in-batch comparison keyed on identity **MUST** include the entry type or key on the entry id, which covers all six inputs: a record and its invalidation carry the same idempotency key and covered period, and deduplicating on the first five alone would treat every invalidation as a collision with its target. On a duplicate identity whose caller-supplied canonical fields are identical, the plugin **MUST** return the stored record (silent absorb); on a duplicate identity whose canonical fields differ, the plugin **MUST** return an idempotency-conflict error. Concurrent same-identity submissions **MUST** resolve at the declared dedup level (`cpt-cf-uc-plugin-fr-dedup-level`), so exactly one entry is ever visible per identity. Idempotency-key presence is enforced upstream by the gear core and **MUST NOT** be re-validated here.

- **Rationale**: At-least-once emission from callers requires the storage boundary to be the exactly-once authority. One stable per-meter idempotency key therefore covers many covered periods: a replay of that key over a different covered period is a distinct entry by design, consistent with the parent contract. For the same reason a record and its invalidation, which share key and covered period, are two distinct entries.
- **Actors**: `cpt-cf-uc-plugin-actor-plugin-host`
- **Realizes (gear)**: `cpt-cf-usage-collector-fr-idempotency`
- **Note**: Preservation of the dedup identity is **retention-bounded** by the referenced type's current declared retention policy (`cpt-cf-uc-plugin-fr-per-type-retention`) — the gear's adopted reading for a time-series backend, not a narrowing ([§12](#12-risks), [§13](#13-open-questions)).

#### Invalidation Persistence

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-fr-invalidation-persistence`

The plugin **MUST** persist a withdrawal as an ordinary appended entry that names the entry it withdraws and carries a reason code, **MUST NOT** rewrite the withdrawn entry, and **MUST** admit at most one withdrawal per target. Every withdrawal of one target has the same dedup identity — the target's tenant, GTS type, idempotency key and covered period with entry type `invalidation` — so this bound follows from `cpt-cf-uc-plugin-fr-idempotent-dedup` rather than from a separate store-side rule.

- **Rationale**: Append-only invalidation (ADR-0010) keeps the ledger auditable — a correction is a new fact, not a mutation of history — and admitting at most one withdrawal per target keeps the fold's netting exact.
- **Actors**: `cpt-cf-uc-plugin-actor-plugin-host`
- **Realizes (gear)**: `cpt-cf-usage-collector-fr-record-invalidation`

#### Dedup Level

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-fr-dedup-level`

The plugin **MUST** declare the dedup level `linearizable` and meet it: its convergence bound is zero, every submission under one dedup identity is decided against every earlier one in the store's own commit order as it commits, and convergence is established from the store's commit state, never from elapsed time. An identical later submission **MUST** be absorbed and a divergent one **MUST** be rejected with an idempotency-conflict error. A write whose caller was already answered and that reaches the store after the identity converged **MUST** be discarded.

- **Rationale**: The gear requires every plugin to declare a dedup level and a convergence bound as part of its consistency profile; a single transactional primary decides every write at commit, so the strongest level costs this backend nothing.
- **Actors**: `cpt-cf-uc-plugin-actor-plugin-host`
- **Realizes (gear)**: `cpt-cf-usage-collector-fr-idempotency`, `cpt-cf-usage-collector-nfr-query-freshness`

#### Durable Acknowledgement

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-fr-durable-ack`

The plugin **MUST** return from a persist call only after every entry it reports accepted is durable in the store, **MUST NOT** buffer acknowledged entries in memory, **MUST** refuse to start under a store durability setting that could lose a committed write, and **MUST** force synchronous commit on its own write transactions.

- **Rationale**: An acknowledgement is the only surface the gear's consistency floor binds for write-derived state; losing an acknowledged entry breaks both the idempotency contract and every charge derived from it.
- **Actors**: `cpt-cf-uc-plugin-actor-plugin-host`
- **Realizes (gear)**: `cpt-cf-usage-collector-fr-ingestion`, `cpt-cf-usage-collector-nfr-query-freshness`

#### Quantity Fidelity

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-fr-quantity-fidelity`

The plugin **MUST** round-trip every quantity in the gear's published range and precision digit for digit — negative half included — on every read path that returns an entry, and **MUST NOT** convert, scale, round or truncate a stored quantity.

- **Rationale**: A charge is derived from these digits; a backend that alters a quantity corrupts a bill silently.
- **Actors**: `cpt-cf-uc-plugin-actor-plugin-host`
- **Realizes (gear)**: `cpt-cf-usage-collector-fr-record-quantity`, `cpt-cf-usage-collector-fr-canonical-units`

### 5.2 Query & Aggregation

#### Pushed-Down Aggregation

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-fr-aggregated-query`

The plugin **MUST** execute the host-supplied aggregation fold (`SUM`, `COUNT`, `MIN`, `MAX`, `LATEST`) with grouping over the requested dimensions inside the backend, applying the host-supplied filter, scope and metadata filter, and return the aggregated result. Aggregation **MUST** exclude a withdrawn entry and the invalidation that withdrew it from every fold, and the plugin **MUST NOT** return raw rows to the host for client-side aggregation. `LATEST` **MUST** select by the greatest covered-period end, then the latest acceptance instant, then the greatest entry identifier in byte order — a total order that holds across tenants. A metadata filter **MUST** OR the values of one key and AND distinct keys. A tenant dimension in a result bucket key **MUST** be rendered as the lowercase hyphenated UUID; every other dimension verbatim. Grouping on the subject identifier or subject type **MUST** exclude entries without a subject. A bucket whose selection is empty — the single empty-key bucket of an ungrouped query over no entries, and one whose entries are all withdrawn pairs alike — **MUST** carry `0` under `SUM` and `COUNT`, which are defined over an empty selection, and a null value under `MAX`, `MIN` and `LATEST`, which are not. A group nothing survives in yields no bucket at all. A fold the plugin does not implement **MUST** be answered as an internal error, never by substituting another fold.

- **Rationale**: Pushing aggregation into the backend is how the plugin meets the parent query-latency NFR (`cpt-cf-usage-collector-nfr-query-latency`) over large time ranges without a downstream aggregation layer.
- **Actors**: `cpt-cf-uc-plugin-actor-plugin-host`
- **Realizes (gear)**: `cpt-cf-usage-collector-fr-query-aggregation`

#### Rollup-Backed Aggregation

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-fr-rollup-aggregation`

The plugin **MUST** be able to answer eligible aggregation queries from a materialised aggregate and **MUST** answer every other query exactly from the stored entries, **MUST** keep the materialised aggregate consistent with the entries that remain after retention, and **MUST** expose which path served a query.

- **Rationale**: Serving eligible aggregations from a materialised aggregate is how the plugin meets the parent's aggregate-freshness expectation without scanning the ledger on every query, while every other query still reads the exact stored entries.
- **Actors**: `cpt-cf-uc-plugin-actor-plugin-host`
- **Realizes (gear)**: `cpt-cf-usage-collector-nfr-aggregate-freshness`, `cpt-cf-usage-collector-fr-query-aggregation`

#### Keyset-Paginated Raw Query

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-fr-raw-query`

The plugin **MUST** return raw entries as keyset-paginated pages over the host-supplied order, seeking from the structured keyset the host decoded from its cursor, and **MUST** return the page's rows together with the keyset of its last row. The plugin **MUST NOT** encode, decode or interpret a wire cursor — cursors are gateway-owned. The plugin **MUST NOT** widen the host-supplied filter and **MUST NOT** use offset-based scans. An order key on a field that may be absent reaching the plugin is a host-contract breach and **MUST** be answered as an internal error, so no matching entry is silently dropped.

- **Rationale**: Realizes the persistence side of `cpt-cf-usage-collector-fr-query-raw`; keyset pagination bounds query cost at the throughput envelope, and preserving the host filter keeps tenant scoping intact.
- **Actors**: `cpt-cf-uc-plugin-actor-plugin-host`
- **Realizes (gear)**: `cpt-cf-usage-collector-fr-query-raw`

#### Converged-Only Lookup

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-fr-converged-lookup`

The plugin **MUST** serve a point lookup by entry identifier under the host-supplied scope, applying the scope first so an out-of-scope entry answers exactly as an absent one. When the host requests a converged-only lookup, the plugin **MUST** return the surviving entry once its identity has converged, **MUST NOT** report an acknowledged, retained entry as absent, and **MUST** reach a definite answer — the entry, or not found — within its convergence bound plus its published query-path lag bound. At this plugin's declared level both bounds are zero, so the answer is immediate and a not-converged answer never occurs.

- **Rationale**: The gateway validates an invalidation's faithful copy against the target this lookup returns; a lookup that answered from a write later discarded, or reported a real target missing, would admit or refuse corrections wrongly.
- **Actors**: `cpt-cf-uc-plugin-actor-plugin-host`
- **Realizes (gear)**: `cpt-cf-usage-collector-fr-record-invalidation`, `cpt-cf-usage-collector-fr-record-identity`

### 5.3 Usage Feed & Reconciliation

#### Usage Feed

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-fr-usage-feed`

The plugin **MUST** serve feed pages over a subscription of GTS types under the host-supplied compiled scope, so an entry outside it is absent, and **MUST** provide the following for an unchanged compiled scope (entries a widened scope admits behind a returned position are not delivered):

- **a deterministic order** in which the same position yields the same continuation, extended only by entries settled since, and in which an invalidation follows the entry it withdraws; the order **MUST NOT** rest on the gateway-stamped acceptance instant;
- **completeness**: an entry **MUST NOT** become visible at or before a position the plugin has returned, whatever the concurrency, commit order or number of gateway replicas;
- **snapshot consistency**: a paginated scan observes no entry appearing, disappearing or changing, except arrivals ahead of its position;
- **a live head**: a page that reaches the settled head **MUST** return a position at the head, even when it carries no entries, so a regularly polled position stays current;
- **a defined start**: a read **MUST** name where it begins rather than leave it to an absent position. The start that names the oldest entry the subscription retains **MUST** begin there and **MUST NOT** begin at the head, so a new consumer replays the history the deployment still holds (`cpt-cf-usage-collector-fr-billing-usage-feed`);
- **bounded replay**: a replay bounded by a later position **MUST** return the same entries in the same order and **MUST** return no next position once that bound is reached;
- **retention refusal**: a position after which retention has removed an entry of a subscribed type **MUST** be refused with the cursor-beyond-retention error rather than served as a silently truncated range. Removal and age are both read over the subscription's GTS types rather than over the caller's compiled scope, so a position **MAY** be refused for a removed entry that scope excluded. Refusal reads what the store still holds and never the position's own age, so a position whose continuation is intact **MUST** be served whatever its age, a position within the replay horizon **MUST** be served, and a current position, with no entry of a subscribed type after it, is never refused. The known shortfall of the removed-entry check is listed in [§13](#13-open-questions).

- **Rationale**: A charging consumer's inbound path must be replay-safe under concurrent ingest; without these guarantees a scan is silently incomplete or silently duplicated.
- **Actors**: `cpt-cf-uc-plugin-actor-plugin-host`
- **Realizes (gear)**: `cpt-cf-usage-collector-fr-billing-usage-feed`, `cpt-cf-usage-collector-fr-billing-retention-floor`, `cpt-cf-usage-collector-fr-billing-fields-on-read`

#### Reconciliation Metadata

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-fr-reconciliation-metadata`

The plugin **MUST** report, for one `(tenant, GTS type)` scope per call, the count of accepted entries whose covered-period end falls in the requested range, a fold-appropriate quantity summary over the same selection — the accrued sum for a `SUM` type, otherwise the observation count and the latest observation — and two watermarks not bounded by the range: the latest acceptance instant and the latest covered-period end. Every figure **MUST** reflect one entry per dedup identity. The summary **MUST** exclude withdrawn pairs; the count **MUST** count every accepted entry, invalidations included. The host-supplied scope **MUST** be applied first, so a tenant outside it answers exactly as one holding no entries: a zero count, an empty-selection summary, and both watermarks absent.

- **Rationale**: Revenue assurance compares emitter, gear and consumer totals and spots a stalled emitter. The figures prove nothing about feed completeness. The counters and watermarks live in the plugin because the gear is stateless.
- **Actors**: `cpt-cf-uc-plugin-actor-plugin-host`
- **Realizes (gear)**: `cpt-cf-usage-collector-fr-reconciliation-metadata`
- **Note**: The gear's SPI signature carries the tenant, the GTS type and the fold, and the endpoint serves one scope per call, so this method needs no paging. Counts do not net withdrawals: `accepted_count` counts every accepted entry the range selects, invalidations included, while the quantity summary excludes withdrawn pairs. A scope with entries of the type but none in the range is reported with a zero count, an observation count of zero and an absent latest observation for a non-`SUM` fold; a scope holding no entries at all also reports both watermarks absent. The gear's REST schema renders each as `null`.

### 5.4 Data Lifecycle

#### Per-Type Retention

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-fr-per-type-retention`

The plugin **MUST** enforce retention **per GTS type**, from each type's current declared retention policy, measured from the end of the covered period, **MUST NOT** delete an entry before its retention elapses, **MAY** hold it longer, and **MUST** retain entries whose retention cannot be resolved while signalling that it did so.

- **Rationale**: Retention driven by each type's registry-declared trait is what lets one deployment host meters with different retention floors correctly, and retaining — rather than guessing — an entry whose retention cannot be resolved avoids an unrecoverable, silent data loss.
- **Actors**: `cpt-cf-uc-plugin-actor-plugin-host`, `cpt-cf-usage-collector-actor-platform-operator`
- **Realizes (gear)**: `cpt-cf-usage-collector-fr-billing-retention-floor`
- **Note**: Retention interacts with idempotency-key preservation — see `cpt-cf-uc-plugin-fr-idempotent-dedup`, [§12](#12-risks), and [§13](#13-open-questions). Every GTS type **MUST** declare retention at least the backfill window plus the replay horizon plus the acceptance-order slack ([§1.4](#14-glossary)), since dedup-identity preservation and feed replay are both bounded by it (a deployer obligation: the plugin does not check it, DESIGN §4.1 item 6).

#### Self-Provisioned Schema

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-fr-schema-provisioning`

The plugin **MUST** provision and evolve its own schema idempotently at startup, before serving traffic, so deployment requires no manual database setup and a restart re-runs provisioning as a no-op.

- **Rationale**: A backend must self-provision deterministically so deployment is turnkey and restarts are safe.
- **Actors**: `cpt-cf-uc-plugin-actor-plugin-host`
- **Realizes (gear)**: `cpt-cf-usage-collector-fr-pluggable-storage`
- **Note**: The Usage Collector is pre-release with no existing installations or data; this is forward schema setup only and carries no obligation to migrate data from a prior release.

### 5.5 Plugin Integration & Error Contract

#### GTS-Scoped Backend Registration

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-fr-registration`

At startup the plugin **MUST** create its connection pool, provision its schema, and register itself as a scoped SPI client under a GTS instance identifier via the platform registry, carrying its configured vendor and priority, so the Usage Collector's plugin selection can discover and bind it. The plugin **MUST NOT** decide whether it is the active backend — selection is host-side.

- **Rationale**: Realizes the discovery half of `cpt-cf-usage-collector-fr-pluggable-storage` and the parent registry contract (`cpt-cf-usage-collector-contract-gts-registry`); operator configuration binds the active backend without code changes.
- **Actors**: `cpt-cf-uc-plugin-actor-plugin-host`, `cpt-cf-usage-collector-actor-platform-operator`
- **Realizes (gear)**: `cpt-cf-usage-collector-fr-pluggable-storage`

#### Typed Error Classification

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-fr-error-classification`

The plugin **MUST** return the SPI's six-variant error vocabulary — transient, internal, idempotency conflict, entry not found, entry not converged, and cursor beyond retention — and classify every backend error as transient (retryable) or internal (non-retryable), so the host applies retry and fail-closed behavior without backend-specific parsing. A transient raised because the connection pool was saturated **MUST** carry a retry hint, so a caller can tell a busy backend from a failed one and back off rather than retry immediately. The cursor-beyond-retention variant **MUST** be raised by the feed alone. A malformed or unauthorized call reaching the SPI is a host-contract breach and **MUST** surface as internal.

- **Rationale**: A stable, classified error vocabulary lets the host make retry and failure decisions uniformly across any backend, decoupling host behavior from backend-specific errors.
- **Actors**: `cpt-cf-uc-plugin-actor-plugin-host`
- **Realizes (gear)**: `cpt-cf-usage-collector-nfr-plugin-contract-stability`

## 6. Non-Functional Requirements

> **Global baselines**: Project- and gear-wide NFRs are defined at those levels — see the gear PRD ([../../../docs/PRD.md](../../../docs/PRD.md)) and gear DESIGN ([../../../docs/DESIGN.md](../../../docs/DESIGN.md)). Only plugin-specific NFRs — those that realize a gear NFR at the storage tier or that are standalone to this backend — appear below. Architecture allocation for each is in the plugin DESIGN.md.

### 6.1 Gear-Specific NFRs

#### Aggregation Query Latency

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-nfr-query-latency`

Aggregation queries over a 30-day range for a single tenant **MUST** complete within 500ms at p95, measured over a steady-state window of at least 30 minutes, against the bound backend under the parent gear's load envelope (`cpt-cf-usage-collector-nfr-throughput-profile`).

- **Threshold**: p95 ≤ 500ms over a ≥ 30-minute steady-state window inside the `cpt-cf-usage-collector-nfr-throughput-profile` envelope; permitted measurement tolerance ±10% (p95 ≤ 550ms accepted for any single steady-state window) provided the 30-minute trailing trend stays at or below 500ms.
- **Verification**: No load test exists in this repository; conformance to this threshold is unverified.
- **Rationale**: This plugin is the allocation target for the parent's query-latency NFR (`cpt-cf-usage-collector-nfr-query-latency`).
- **Architecture Allocation**: See DESIGN.md §1.2 (NFR Allocation) and §4.3 (Metric Inventory, SLO Summary).

#### Ingestion Throughput

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-nfr-ingestion-throughput`

The plugin **MUST** sustain the parent gear's ingestion envelope — sustained and burst — through the batch write path, within the Plugin SPI's planning share of the parent ingestion-latency budget.

- **Threshold**: ≥ 10,000 entries/sec sustained sample-mean over a ≥ 30-minute steady-state window, instantaneous 1-minute sample-mean ≥ 0.95 × sustained rate; ≥ 30,000 entries/sec for ≤ 5 minutes in any 60-minute window; SPI persist p95 ≤ 75ms (the gear DESIGN §3.11.2 planning share, a target, not a conformance bound).
- **Verification**: No load test exists in this repository; conformance to this threshold is unverified.
- **Rationale**: Allocation target for `cpt-cf-usage-collector-nfr-throughput`.
- **Architecture Allocation**: See DESIGN.md §1.2 (NFR Allocation).

#### SPI Conformance & Contract Stability

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-nfr-spi-stability`

The plugin **MUST** implement the storage SPI exactly as declared by the gear, with changes additive-only within a major version, and **MUST** pass the gear's full SPI contract suite. Conformance **MUST** be verifiable before release: compile-time conformance to the trait and a green run of every contract check, none blocked.

- **Threshold**: Build-time conformance to the SPI; every contract check implemented and passing; no breaking change within a major SPI version.
- **Rationale**: Allocation target for `cpt-cf-usage-collector-nfr-plugin-contract-stability`; the host binds the backend by contract, so the contract must not silently drift.
- **Architecture Allocation**: See DESIGN.md §2.1 (Design Principles — SPI Conformance).

#### Transport & Query Security

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-nfr-transport-security`

Database connections **MUST** default to TLS. An unspecified `sslmode`, `prefer`, or `allow` — each of which can fall back to plaintext without signalling it — **MUST** be raised to `require`, and a stronger operator choice (`verify-ca`, `verify-full`) **MUST** be preserved. An explicit `sslmode=disable` is the one plaintext path, reserved for non-production use, and the plugin **MUST** emit a warning when it builds a pool under one. The database connection string — which embeds credentials — **MUST NOT** appear in logs, error messages, or debug output. Translation of the host-supplied query into the backend **MUST** be injection-safe: no caller-supplied string reaches query text as a literal or an identifier.

- **Threshold**: Zero connections that reach plaintext without an explicit `sslmode=disable`, and one warning emitted per pool built under one; zero credential disclosures in emitted diagnostics; no caller-supplied string reaches query text as a literal or identifier.
- **Rationale**: The plugin is the only component in the gear + plugin split that holds a database credential, opens a connection to the store, and translates untrusted query shapes; transport confidentiality, credential non-disclosure, and injection safety are its security obligations. Authentication and authorization of callers remain gear-core concerns (see [§6.2](#62-nfr-exclusions)).
- **Architecture Allocation**: See DESIGN.md §2.2 (Injection-Safe Query Translation) and §4 (Non-Applicable Design Domains — Security).

#### Backend Consistency Profile

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-nfr-consistency-profile`

The plugin **MUST** publish the consistency profile the gear's DESIGN §3.10 requires of every backend — all nine items — so downstream consumers couple to the backend's actual ceiling rather than the gear's eventual floor. The profile **MUST** declare the dedup level and convergence bound (`cpt-cf-uc-plugin-fr-dedup-level`) and a query-path lag bound. A deployment outside the single-primary posture **MUST** republish the affected items before serving traffic.

- **Threshold**: All nine items published in DESIGN §4.1; query-path lag bound zero on a single primary (the feed excepted: its lag is the settled-horizon bound, `cpt-cf-uc-plugin-nfr-feed-freshness`); convergence bound zero.
- **Rationale**: The parent floor is "eventually consistent, no upper bound"; `cpt-cf-usage-collector-nfr-query-freshness` obliges each plugin to publish its actual ceiling so consumers couple to it consciously.
- **Architecture Allocation**: See DESIGN.md §4, and the parent gear DESIGN.md §3.10 (Consistency Contract) and ADR-0006.

#### Feed Freshness

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-nfr-feed-freshness`

Acceptance → feed visibility **MUST** be bounded at p95 ≤ 5 minutes under the parent throughput-profile envelope, measured over a ≥ 30-minute steady-state window, so a deployment of this plugin qualifies to feed a charging consumer.

- **Threshold**: p95 ≤ 5 minutes acceptance → feed visibility. The design bound (DESIGN §4.1 item 2) is derived, not measured; no load test exists in this repository.
- **Rationale**: Allocation target for `cpt-cf-usage-collector-nfr-billing-feed-freshness`, a readiness gate for charging consumers.
- **Architecture Allocation**: See DESIGN.md §4.1 item 2.

#### Replay Throughput

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-nfr-replay-throughput`

A feed consumer 24 hours behind **MUST** be able to reach the head within 6 hours without breaching the ingestion SLOs, which requires a read rate of at least the subscribed arrival rate × (1 + backlog age / recovery time).

- **Threshold**: ≥ 50,000,000 entries/hour/region at the launch planning assumption of ≤ 10,000,000 entries/hour/region for a charging subscription. Not measured; no load test exists in this repository.
- **Rationale**: Allocation target for `cpt-cf-usage-collector-nfr-replay-throughput`.
- **Architecture Allocation**: See DESIGN.md §4.1 item 7.

#### Aggregate Freshness

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-nfr-aggregate-freshness`

The plugin **MUST** publish a finite acceptance → aggregate visibility bound and, separately, an invalidation-propagation bound for its materialised aggregate; a deployment serving a consumer that acts on aggregates **MUST** be configured so both are ≤ 5 minutes p95.

- **Threshold**: Both bounds published per DESIGN §4.1 item 4; ≤ 5 minutes p95 where a consumer acts on the aggregate. Derived from configuration, not measured.
- **Rationale**: Allocation target for `cpt-cf-usage-collector-nfr-aggregate-freshness`.
- **Architecture Allocation**: See DESIGN.md §4.1 item 4.

#### Operational Visibility

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-nfr-operational-visibility`

The plugin **MUST** emit push-based OpenTelemetry metrics for its backend-internal operation — at minimum ingestion latency, deduplication outcomes, query latency, connection-pool saturation, backend error rate by classification, and backend readiness — under its own metric sub-namespace (`uc_timescaledb_*`), distinct from the gear's request-path signals. Unbounded identifiers **MUST NOT** be used as metric labels.

- **Threshold**: the DESIGN §4.3 metric set, emitted with bounded label cardinality.
- **Rationale**: Allocation target for `cpt-cf-usage-collector-nfr-operational-visibility`; the plugin owns the backend-internal series the gear cannot see.
- **Architecture Allocation**: See DESIGN.md §4.3 (Metric Inventory).

### 6.2 NFR Exclusions

- **Authentication, authorization, and attribution enforcement**: Not a plugin concern — the gear core enforces them before every SPI call ([§4.2](#42-out-of-scope); `cpt-cf-usage-collector-fr-ingestion-authorization`, `cpt-cf-usage-collector-fr-tenant-isolation`). The plugin's only security obligations are transport security and injection safety ([§6.1](#61-gear-specific-nfrs)).
- **Data protection and disposal**: At-rest encryption, key management and masking are delegated to the operator's PostgreSQL/storage deployment. Disposal is the retention sweep's drop; there is no per-entry purge or erasure beyond it, and a data-subject erasure is an operator database action outside the SPI (DESIGN §4.4).
- **Data classification**: Not applicable at the plugin — it stores caller-supplied metadata verbatim and performs no classification; `cpt-cf-usage-collector-fr-data-classification` is gear-owned.
- **End-to-end ingestion latency and availability**: Not owned at plugin level. `cpt-cf-usage-collector-nfr-ingestion-latency` and `cpt-cf-usage-collector-nfr-availability` are gear-level, end-to-end NFRs realized jointly by the gear and the active backend; the plugin's contribution is bounded by its throughput and query-latency allocations. The plugin's availability is bounded by the operator's PostgreSQL/TimescaleDB HA posture, and the plugin publishes no availability SLO of its own beyond the `uc_timescaledb_ready` signal (DESIGN §4.4).
- **Workload isolation between ingestion and query**: Not realised by this backend — one connection pool serves both ingestion and query calls, so the two paths are not isolated. A burst on the query path can therefore contend with ingestion; `cpt-cf-usage-collector-nfr-workload-isolation` is allocated to the active plugin but is not met here, a known shortfall this plugin publishes in its consistency profile (DESIGN §4.1 item 1).
- **Safety, UI accessibility/usability, internationalization, and privacy/regulatory conformance as standalone obligations**: Not applicable — inherited from the parent gear's identical exclusions (parent PRD [§6.2](../../../docs/PRD.md)). The plugin is server-side infrastructure with no UI, holding only opaque identifiers and opaque metadata passed through from the gear.
- **Disaster recovery (RPO/RTO) and backup/restore**: Not applicable as standalone plugin requirements — governed by the operator's TimescaleDB/PostgreSQL deployment posture.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### Storage SPI Implementation

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-interface-storage-spi`

- **Type**: In-process async Rust trait implementation of the storage SPI (`UsageCollectorPluginV1`).
- **Stability**: pre-1.0 (`V1`), as the gear labels this same surface (`cpt-cf-usage-collector-interface-plugin`).
- **Description**: The plugin's sole public surface — the seven Plugin SPI methods ([§4.1](#41-in-scope)). Registered as a scoped client under a GTS instance identifier and consumed in-process by the Usage Collector core; there is no REST or network-exposed surface. Realizes the gear's Plugin SPI (`cpt-cf-usage-collector-interface-plugin` and its `cpt-cf-usage-collector-contract-storage-plugin`); the technical realization is defined in DESIGN.md (`cpt-cf-uc-plugin-interface-spi`).
- **Breaking Change Policy**: Follows the SPI's versioning (`cpt-cf-usage-collector-nfr-plugin-contract-stability`) — additive within a major version from that surface's 1.0 release onward, and until then a breaking change ships in place; breaking changes are coordinated through the SDK crate.

### 7.2 External Integration Contracts

#### PostgreSQL / TimescaleDB Backend Contract

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-contract-timescaledb`

- **Direction**: required from external system (the operator-provisioned database).
- **Protocol/Format**: PostgreSQL wire protocol over a TLS-by-default connection; requires the TimescaleDB extension (hypertables, continuous aggregates).
- **Compatibility**: The plugin provisions and evolves its own schema idempotently at startup; it requires a PostgreSQL version compatible with the TimescaleDB features it uses.

#### GTS Registration Contract

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-contract-gts-registration`

- **Direction**: provided by library (to the platform registry).
- **Protocol/Format**: Registers a scoped SPI client under a GTS instance identifier, carrying `vendor` and `priority` selection metadata.
- **Compatibility**: Realizes the gear's `cpt-cf-usage-collector-contract-gts-registry`; the GTS spec identity is fixed by the SDK.

## 8. Use Cases

### Ingest a Usage Record with Idempotent Dedup

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-usecase-ingest-dedup`

**Actor**: `cpt-cf-uc-plugin-actor-plugin-host`

**Preconditions**:

- The plugin is the bound backend and its schema is provisioned; the referenced usage type exists.
- The call arrives already authorized and structurally validated, carrying the gateway-derived entry id, the caller-supplied idempotency key and the entry type.

**Main Flow**:

1. The core calls the SPI to persist a usage record.
2. The plugin attempts an insert keyed on the dedup identity.
3. On no conflict, the record is stored and returned.

**Postconditions**:

- The record is durably stored and visible to subsequent dedup checks for as long as its type's declared retention keeps it.

**Alternative Flows**:

- **Exact-equality retry**: the dedup identity already exists with identical canonical fields — the stored record is returned (silent absorb).
- **Canonical mismatch**: the dedup identity exists with differing canonical fields — an idempotency-conflict error is returned.
- **Same key, different covered period**: a distinct dedup identity — a new record is created.
- **Same key and covered period, different entry type**: a record and its invalidation are distinct dedup identities — both are stored as two distinct entries, and a retry of either is absorbed against its own stored entry.
- **Transient backend error**: returned to the host classified as retryable.

### Register the Backend at Plugin Startup

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-usecase-bind-startup`

**Actor**: `cpt-cf-uc-plugin-actor-plugin-host`

**Preconditions**:

- Valid plugin configuration (DESIGN §3.5) is provided; the database is reachable.

**Main Flow**:

1. The plugin loads and validates config and creates its connection pool.
2. The plugin provisions its schema idempotently.
3. The plugin registers itself as a scoped SPI client under a GTS instance identifier, carrying vendor/priority.

**Postconditions**:

- The plugin is registered and dispatchable; the backend-readiness signal is set. Binding is the host's, and happens on its first dispatch rather than in this flow — the gear gates no readiness on it (gear DESIGN, Lazy plugin binding).

**Alternative Flows**:

- **Invalid config / unreachable database / missing TimescaleDB extension / a failed startup check** (an unsafe durability setting): startup fails fast; the plugin does not register, so no dispatch can reach it.

### Read a Feed Page

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-usecase-read-feed-page`

**Actor**: `cpt-cf-uc-plugin-actor-plugin-host`

**Preconditions**:

- The host has authorized the consumer at the PDP, compiled the scope, and decoded any cursor into the position this plugin issued.

**Main Flow**:

1. The core requests a feed page for a subscription under a compiled scope, from a named start — after a position the feed issued, or at the oldest entry the subscription retains — optionally bounded by a later position, with a page limit.
2. The plugin reads the settled, in-scope entries of the subscribed types from that start, in feed order, up to the limit.
3. The plugin returns the entries and the next position.

**Postconditions**:

- No entry will ever become visible at or before the returned position.

**Alternative Flows**:

- **First read**: the start names the oldest entry the subscription retains rather than a position — the plugin begins there, neither at the head nor at the replay horizon, as `cpt-cf-uc-plugin-fr-usage-feed` requires.
- **Head reached**: fewer entries than the limit are available — the plugin returns a position at the settled head, even with no entries.
- **Bound reached**: the page reaches the bounding position — the plugin returns no next position.

### Refuse a Stale Cursor

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-usecase-refuse-stale-cursor`

**Actor**: `cpt-cf-uc-plugin-actor-plugin-host`

**Preconditions**:

- A consumer resumes from a position after which retention has removed an entry of a subscribed type.

**Main Flow**:

1. The core requests a feed page after that position.
2. The plugin finds a retention mark of a subscribed type above the position (DESIGN §3.6).
3. The plugin returns the cursor-beyond-retention error and no entries.

**Postconditions**:

- No silently truncated range is served.

**Alternative Flows**:

- **Within the replay horizon**: the position is served normally, except in the known shortfall of [§13](#13-open-questions).
- **Nothing after the position**: the position is served and a head position returned; only a mark above it can refuse it.

## 9. Acceptance Criteria

- [ ] The plugin implements all seven methods of the gear's Plugin SPI and conforms to the SDK SPI at build time, and does not depend on the host gear crate.
- [ ] A usage record is persisted and retrievable; a second submission with the same dedup identity and identical canonical fields yields a single stored record (silent absorb).
- [ ] A submission with the same dedup identity but differing canonical fields is rejected with an idempotency-conflict error.
- [ ] A submission with the same idempotency key but a different covered period is stored as a distinct record.
- [ ] A record and its invalidation, sharing idempotency key and covered period, are stored as two distinct entries; a later retry of the record is absorbed, and a read returns exactly the two entries.
- [ ] Batch ingestion returns one outcome per input record in input order; a conflict on one record does not fail the others.
- [ ] A withdrawal is persisted as an ordinary appended entry naming the entry it withdraws and carrying a reason code, without rewriting the withdrawn entry; a second withdrawal of the same target is either absorbed or conflicts, never admitted as a second withdrawal.
- [ ] Aggregation (SUM/COUNT/MIN/MAX/LATEST) with grouping is computed in the backend, excludes a withdrawn entry and the invalidation that withdrew it from every fold, and honors the host filter and scope.
- [ ] An eligible aggregation query is answered from the materialised aggregate and every other query is answered exactly from the stored entries; which path served a query is exposed.
- [ ] Raw list seeks from the host-decoded keyset, returns rows with the last row's keyset, and never encodes or decodes a wire cursor — the target contract, and not verifiable yet: the SPI returns `toolkit_odata::Page`, whose `PageInfo` carries opaque cursor strings and no structured keyset, so the boundary remains the open question DESIGN §2.2 records.
- [ ] An entry is retained until its type's current declared retention policy elapses, measured from the end of its covered period; an entry whose retention cannot be resolved is retained, not deleted, and the plugin signals that it did so.
- [ ] Database connections default to TLS — an unspecified `sslmode`, `prefer`, or `allow` is raised to `require`, `verify-ca` / `verify-full` are preserved, and an explicit `sslmode=disable` is honoured only with a warning; the connection string and credentials never appear in logs, errors, or debug output; no caller-supplied string reaches query text as a literal or identifier.
- [ ] Aggregation queries meet p95 ≤ 500ms over a ≥ 30-minute steady-state window, and the batch write path sustains ≥ 10,000 records/sec sustained sample-mean over the same window, both within the parent throughput-profile envelope — unverified in this repository, since no load test exists here.
- [ ] The batch write path absorbs ≥ 30,000 records/sec for ≤ 5 minutes in any 60-minute window, within the same parent throughput-profile envelope — unverified in this repository, since no load test exists here.
- [ ] The plugin publishes its consistency profile per deployment topology; a single-node deployment provides read-after-write visibility of a committed record.
- [ ] The plugin emits the enumerated OpenTelemetry metrics under its `uc_timescaledb_*` sub-namespace, including a backend-readiness signal.
- [ ] The plugin registers under a GTS instance identifier with its configured vendor and priority and does not self-select as the active backend.
- [ ] The plugin declares the `linearizable` dedup level with a zero convergence bound; racing same-identity submissions yield one entry, a divergent later one an idempotency conflict.
- [ ] A persist call returns only after its accepted entries are durable; the plugin refuses to start under a durability setting that can lose a committed write and forces synchronous commit on its own write transactions.
- [ ] Every quantity in the published range and precision, negative half included, reads back digit for digit.
- [ ] `LATEST` breaks ties by covered-period end, then acceptance instant, then entry identifier in byte order, across tenants.
- [ ] A converged-only lookup returns the entry or not-found immediately and never reports an acknowledged, retained entry absent.
- [ ] Feed pages omit every entry outside the host-supplied scope, are complete behind every returned position under concurrent writers, snapshot-consistent, place an invalidation after its target, return a head position on a quiet subscription, replay identically when bounded, begin a first read at the oldest entry the subscription retains, serve a position whose continuation is intact whatever its age, and refuse a position after which retention has removed an entry of a subscribed type.
- [ ] Reconciliation reports, for one `(tenant, GTS type)` scope, a count including invalidations, a fold-appropriate summary excluding withdrawn pairs, and both watermarks — absent when the scope holds no entries, and likewise for a tenant outside the host-supplied scope.
- [ ] The full SPI contract suite passes with no blocked check.
- [ ] Acceptance → feed visibility p95 ≤ 5 minutes and the replay recovery objective are met — unverified in this repository, since no load test exists here.

## 10. Dependencies

| Dependency                                                                         | Description                                                                                                  | Criticality |
| ---------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ | ----------- |
| usage-collector-sdk                                                                | Storage SPI trait, domain models, error vocabulary, and GTS plugin spec — the contract the plugin implements | p1          |
| PostgreSQL + TimescaleDB extension                                                 | Durable system of record; provides time-partitioning (hypertables) and continuous aggregates                 | p1          |
| types-registry (+ ClientHub)                                                       | Publishes the plugin's GTS instance for host discovery and scoped binding, and is read by the retention sweep for each GTS type's current declared retention policy | p1          |
| Platform registry / orchestration (`cpt-cf-usage-collector-contract-gts-registry`) | Operator-driven active-backend selection                                                                     | p1          |

## 11. Assumptions

- The Usage Collector core performs all authentication, PDP authorization, attribution and shape validation, and semantics decisions before every SPI call; the plugin trusts each call as authorized and structurally valid.
- The gateway derives each entry's id, and on an invalidation the `invalidates` reference, from the entry's own fields; the plugin stores them and the caller-supplied idempotency key verbatim and does not mint identity.
- The operator provisions a PostgreSQL database with the TimescaleDB extension and a TLS-capable endpoint, sized for the deployment's throughput and retention.
- The deployment supplies its replay horizon (`feed_replay_horizon_secs`) to the plugin as configuration, because the SPI does not carry it. The retention rule of `cpt-cf-uc-plugin-fr-per-type-retention` adds the acceptance-order slack on top of the gear's retention floor, so the deployment holds more history than the floor and a first read, which begins at the oldest entry the subscription retains, replays all of it.
- The PostgreSQL instance hosts no long-running write transactions outside the plugin's own, since feed freshness is bounded by the oldest running write transaction instance-wide (DESIGN §4.1 item 2). Such a transaction, held open long enough, also causes the polled-cursor shortfall in [§13](#13-open-questions).

## 12. Risks

| Risk                                                                                     | Impact                                                                                                                                              | Mitigation                                                                                                                                                                                                       |
| ---------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Retention window shorter than the maximum client replay/backfill horizon                 | An entry whose retention has elapsed is dropped, so a dedup identity re-submitted afterward is accepted as a fresh insert, admitting a duplicate    | Size retention per `cpt-cf-uc-plugin-fr-per-type-retention`, which extends the gear's retention floor (`cpt-cf-usage-collector-fr-billing-retention-floor`) |
| Single-node PostgreSQL write ceiling below the ingestion envelope on undersized hardware | Ingestion-throughput NFR missed                                                                                                                     | Batch write path and time-partitioning; operator sizing per the deployment guide; high-volume routing at the gear                                                                                                |
| High-cardinality aggregation exceeds the 500ms p95 budget                                | Slow dashboard and billing queries                                                                                                                  | Time-partitioned indexing; the hourly rollup (`cpt-cf-uc-plugin-fr-rollup-aggregation`) serves eligible aggregations without scanning the ledger                                                                 |
| Read-replica deployments introduce query staleness                                       | Consumers coupled to read-after-write observe stale reads                                                                                           | Publish the per-topology consistency profile (`cpt-cf-uc-plugin-nfr-consistency-profile`); consumers couple only to the published ceiling                                                                        |
| A long-running write transaction anywhere in the PostgreSQL instance holds the feed's settled horizon back | Feed freshness breaches its 5-minute gate for every subscription at once | Bounded request-path transactions and batched rollup refreshes; a horizon-lag gauge and alert (DESIGN §4.3); the deployment rule in §11 |

## 13. Open Questions

Open questions for the gateway:

1. Whether the replay horizon should reach the plugin through the SPI rather than configuration ([§11](#11-assumptions)).
2. The raw-list SPI return type: the gear's trait returns an `ODataPage` while its raw-query sequence returns a keyset (`cpt-cf-uc-plugin-fr-raw-query`).

One known shortfall stands against the gateway's cursor zones (`cpt-cf-usage-collector-fr-billing-retention-floor`), argued in DESIGN §3.6 Retention refusal. A transaction that holds an id open for a long time keeps every later entry unsettled, and the mark check reads settled entries only. A cursor polled at the head can therefore be refused by a retention mark while such a transaction has been open for at least the replay horizon, less the time between the consumer's pages. Nothing is silently truncated, because a retention mark still refuses the cursor after any deletion. The horizon-lag gauge surfaces such a transaction (DESIGN §3.6 Retention refusal, §4.3, `cpt-cf-uc-plugin-fr-usage-feed`).

One mark per GTS type is not a shortfall. It is the granularity the gateway's rule names: removal and age are both read over the subscription's types rather than over the compiled scope (`cpt-cf-usage-collector-fr-billing-retention-floor`), so a cursor refused for an entry its own scope excluded is that rule applied, not a departure from it.

Retention-bounded dedup-identity preservation (`cpt-cf-uc-plugin-fr-idempotent-dedup`) is not open. It is the gear's adopted floor: gear DESIGN §3.10 keeps the identity visible for as long as the referenced type's retention keeps the entry.

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **Parent Gear PRD (authoritative)**: [../../../docs/PRD.md](../../../docs/PRD.md)
- **Parent Gear DESIGN**: [../../../docs/DESIGN.md](../../../docs/DESIGN.md)
- **ADRs (gear-level)**: [../../../docs/ADR/](../../../docs/ADR/) — notably [`0002-cpt-cf-usage-collector-adr-pluggable-storage`](../../../docs/ADR/0002-cpt-cf-usage-collector-adr-pluggable-storage.md), [`0004-cpt-cf-usage-collector-adr-mandatory-idempotency`](../../../docs/ADR/0004-cpt-cf-usage-collector-adr-mandatory-idempotency.md), [`0006-cpt-cf-usage-collector-adr-consistency-contract`](../../../docs/ADR/0006-cpt-cf-usage-collector-adr-consistency-contract.md), [`0008-cpt-cf-usage-collector-adr-registry-owned-typing`](../../../docs/ADR/0008-cpt-cf-usage-collector-adr-registry-owned-typing.md), [`0009-cpt-cf-usage-collector-adr-declared-fold`](../../../docs/ADR/0009-cpt-cf-usage-collector-adr-declared-fold.md), [`0010-cpt-cf-usage-collector-adr-append-only-invalidation`](../../../docs/ADR/0010-cpt-cf-usage-collector-adr-append-only-invalidation.md), [`0011-cpt-cf-usage-collector-adr-feed-aggregate-split`](../../../docs/ADR/0011-cpt-cf-usage-collector-adr-feed-aggregate-split.md), [`0014-cpt-cf-usage-collector-adr-window-end-selection`](../../../docs/ADR/0014-cpt-cf-usage-collector-adr-window-end-selection.md)
- **Gateway assumptions and open questions**: [§11](#11-assumptions) and [§13](#13-open-questions).
