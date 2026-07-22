# Decomposition: TimescaleDB Usage Collector Storage Plugin

**Overall implementation status:**

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-status-overall`

<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 Foundation: Bootstrap, Schema & SPI Wiring ⏳ HIGH](#21-foundation-bootstrap-schema--spi-wiring--high)
  - [2.2 Record Persistence & Lifecycle ⏳ HIGH](#22-record-persistence--lifecycle--high)
  - [2.3 Query & Aggregation ⏳ HIGH](#23-query--aggregation--high)
  - [2.4 Usage-Type Catalog & Referential Integrity ⏳ MEDIUM](#24-usage-type-catalog--referential-integrity--medium)
  - [2.5 Data Retention ⏳ MEDIUM](#25-data-retention--medium)
  - [2.6 Backend Observability & Metrics ⏳ MEDIUM](#26-backend-observability--metrics--medium)
  - [2.7 Deliberate Omissions](#27-deliberate-omissions)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

## 1. Overview

The TimescaleDB Usage Collector Storage Plugin DESIGN is decomposed into six capability features that mirror the backend's distinct persistence and query responsibilities rather than its internal crate layering. The plugin is already implemented and merged; this is a brownfield decomposition that records the coverage of the existing backend against its PRD and DESIGN, so the checkboxes track feature-level acceptance/verification rather than code existence.

- **Foundation: Bootstrap, Schema & SPI Wiring** — gear `init`, typed config, TLS-required connection pool, idempotent schema provisioning, GTS + ClientHub registration, the SPI Storage Adapter shell with its backend-error classification, and the vendor-isolation and SPI-conformance guarantees every capability plugs into.
- **Record Persistence & Lifecycle** — the write plane: single and batch insert with 4-tuple backend deduplication, append-only compensation rows, and the depth-1 atomic deactivation cascade.
- **Query & Aggregation** — the read plane: pushed-down SUM/COUNT/MIN/MAX/AVG with grouping and keyset-paginated raw list, over injection-safe query translation.
- **Usage-Type Catalog & Referential Integrity** — the catalog plane: usage-type create/get/list/delete with the in-database `ON DELETE RESTRICT` foreign key that makes orphaned records structurally impossible.
- **Data Retention** — the declarative, event-time TimescaleDB chunk-drop retention policy for `usage_records`, and the retention-bounded dedup-key preservation it implies.
- **Backend Observability & Metrics** — the `uc_timescaledb_*` OpenTelemetry metric inventory that instruments the other five features' backend-internal operation.

Splitting by capability rather than by SPI-method or file boundary keeps each feature mutually exclusive and lines the decomposition up with the plugin PRD's functional-requirement clusters (Record Persistence, Query & Aggregation, Usage-Type Catalog, Data Lifecycle, Plugin Integration). Foundation owns the cross-cutting plumbing — the Plugin Module, the SPI Storage Adapter, the Schema Migrations, and the SPI/security/registration contracts — once; capability features reference rather than duplicate them.

Dependencies flow outward from Foundation. Every capability feature builds on the foundation's schema, adapter, and registration handshake. Query & Aggregation additionally depends on Record Persistence & Lifecycle, because there is nothing to read until the write plane has accepted records. Data Retention is coupled to Record Persistence & Lifecycle through the dedup-key-preservation contract (dropping a chunk reclaims that chunk's dedup keys). Backend Observability & Metrics instruments the other capabilities but requires only the foundation's meter and pool to exist.

This shape preserves the DESIGN's pure-persistence posture — the plugin performs no authentication, authorization, or business-delta computation — while keeping the write, read, catalog, lifecycle, and telemetry planes implementable and reviewable independently.

**Decomposition Strategy**:

- Cohesion by capability: each feature groups the DESIGN components, sequences, and data tables that collaborate to deliver one backend capability (e.g., Record Persistence & Lifecycle owns the Record Store component, the ingest/batch/deactivate sequences, and the `usage_records` table together).
- Loose coupling via explicit `Depends On`: every feature declares its upstream features by ID. Foundation has no dependencies; downstream features list only the minimum upstream features they need.
- 100% DESIGN/PRD element coverage: every `cpt-cf-uc-plugin-*` ID introduced by PRD.md and DESIGN.md is assigned to exactly one feature, or recorded as a deliberate omission with justification in [§2.7](#27-deliberate-omissions).
- Mutual exclusivity at the capability layer: each DESIGN component and sequence is assigned to exactly one feature, and each `dbtable` has a single writer-owner. The `usage_records` DDL is created by Foundation's Schema Migrations but the table node is owned by its row-writer (Record Persistence & Lifecycle); Query & Aggregation and Data Retention read/expire it and note the shared usage rather than re-owning it. Cross-cutting elements (the SPI Storage Adapter, the Schema Migrations, the SPI and security contracts, the overall design and tech-stack nodes) are owned by Foundation and referenced — not duplicated — by dependent features.
- Domain entities may appear under multiple features' "Domain Model Entities" lists because they cross feature boundaries by value (`UsageRecord` is written by Record Persistence, read by Query, and expired by Retention); this is reference, not duplicated ownership.
- Write vs. read plane separation: the write-side (Record Persistence & Lifecycle) and read-side (Query & Aggregation) capabilities are split into distinct features so the ingestion-throughput and aggregation-query-latency NFRs can be sequenced and validated independently.

## 2. Entries

### 2.1 Foundation: Bootstrap, Schema & SPI Wiring ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-feature-foundation`

- **Purpose**: Establish the plugin's runtime substrate and its single public surface — the storage SPI — so every capability plugs into one identical execution shape. At `#[toolkit::gear]` `init` the Plugin Module loads and validates the typed configuration, builds the TLS-required `sqlx` connection pool, runs the idempotent Schema Migrations, and performs the GTS handshake (`PluginV1::<UsageCollectorPluginSpecV1>::build_registration(...)`, publish to `types-registry`, `ClientHub::register_scoped::<dyn UsageCollectorPluginV1>` under the GTS instance scope, carrying the configured `vendor`/`priority`). The SPI Storage Adapter is the host's only entry point: it implements `UsageCollectorPluginV1`, routes record and catalog work to the stores, and owns translation of backend/SQL errors into the SDK's `UsageCollectorPluginError` vocabulary classified as `Transient` vs `Internal` plus the typed domain variants. Foundation also owns the pure-persistence and SPI-conformance guarantees, the TLS/credential-non-disclosure posture, the published single-node read-after-write consistency profile, and the vendor isolation that keeps all TimescaleDB-specific code in this crate with no dependency on the host gear.

- **Depends On**: None

- **Scope**:
  - Overall backend design node (`cpt-cf-uc-plugin-design-timescaledb`) and the declared tech stack across the Wiring, Domain, and Infrastructure layers (`cpt-cf-uc-plugin-tech-stack`): `toolkit::gear` + `types-registry-sdk` wiring, `usage-collector-sdk` domain types, `sqlx`/TimescaleDB/`opentelemetry` infrastructure.
  - Plugin Module lifecycle: config load, pool creation, migration invocation, and the GTS + ClientHub registration handshake; the module does not decide whether it is the active backend (selection is host-side) and does not implement SPI methods.
  - SPI Storage Adapter: the single `UsageCollectorPluginV1` implementation, delegating record operations to the Record Store and catalog operations to the Catalog Store, holding no business logic or authorization, and owning backend-error classification and keyset cursor encoding.
  - Schema Migrations: idempotent creation at `init` of the `timescaledb` extension, the `usage_type_catalog` table, the `usage_records` hypertable partitioned on event time with its 4-tuple dedup uniqueness constraint (the dedup authority), the `ON DELETE RESTRICT` foreign key to the catalog, the query-supporting indexes, and registration of the retention policy — all re-runnable as no-ops on restart.
  - TLS-required, secret-wrapped DSN so the embedded Postgres password never reaches `Debug`, logs, or error output; the plugin refuses non-TLS connections.
  - Published consistency profile: single-node PostgreSQL/TimescaleDB provides read-after-write visibility of a committed record; a read-replica deployment documents its staleness bound.
  - Pure-persistence and SPI-conformance principles: no authentication, PDP authorization, attribution/shape validation, idempotency-key presence, or counter/gauge decisions; a malformed or unauthorized call reaching the SPI is a host-contract breach surfaced as `Internal`.

- **Out of scope**:
  - Record insert, dedup, compensation, and deactivation semantics — owned by [§2.2](#22-record-persistence--lifecycle--high) Record Persistence & Lifecycle.
  - Aggregation and keyset raw-list execution and injection-safe translation — owned by [§2.3](#23-query--aggregation--high) Query & Aggregation.
  - Usage-type CRUD and FK-rejection lift — owned by [§2.4](#24-usage-type-catalog--referential-integrity--medium) Usage-Type Catalog & Referential Integrity.
  - The declarative retention policy's expiry behavior and retention-bounded key preservation — owned by [§2.5](#25-data-retention--medium) Data Retention (the DDL that registers the policy is created here; its runtime effect is owned there).
  - The `uc_timescaledb_*` metric inventory — owned by [§2.6](#26-backend-observability--metrics--medium) Backend Observability & Metrics.
  - Database deployment topology (HA, sizing, region), backup/restore, and DR — owned by the operator's TimescaleDB deployment guide.

- **Requirements Covered**:
  - [x] `p1` - `cpt-cf-uc-plugin-fr-schema-provisioning`
  - [x] `p1` - `cpt-cf-uc-plugin-fr-registration`
  - [x] `p1` - `cpt-cf-uc-plugin-fr-error-classification`
  - [x] `p1` - `cpt-cf-uc-plugin-nfr-spi-stability`
  - [x] `p1` - `cpt-cf-uc-plugin-nfr-transport-security`
  - [x] `p1` - `cpt-cf-uc-plugin-nfr-consistency-profile`

- **Design Principles Covered**:
  - [ ] `p2` - `cpt-cf-uc-plugin-principle-pure-persistence`
  - [ ] `p2` - `cpt-cf-uc-plugin-principle-spi-conformance`

- **Design Constraints Covered**:
  - [ ] `p2` - `cpt-cf-uc-plugin-constraint-vendor-isolation`

- **Domain Model Entities**:
  - `UsageCollectorPluginV1` (SPI trait)
  - `UsageCollectorPluginError` (error vocabulary)
  - Typed plugin configuration and connection-pool handle (plugin-local)

- **Design Components**:
  - [ ] `p2` - `cpt-cf-uc-plugin-component-module`
  - [ ] `p2` - `cpt-cf-uc-plugin-component-adapter`
  - [ ] `p2` - `cpt-cf-uc-plugin-component-migrations`

- **API**:
  - [ ] `p1` - `cpt-cf-uc-plugin-interface-storage-spi`
  - [ ] `p2` - `cpt-cf-uc-plugin-interface-spi`
  - In-process async `UsageCollectorPluginV1` trait object (`async_trait`, `Send + Sync + 'static`), consumed by the Plugin Host via `ClientHub`. Ten SPI methods across the record group (`create_usage_record`, `create_usage_records`, `get_usage_record`, `query_aggregated_usage_records`, `list_usage_records`, `deactivate_usage_record`) and the catalog group (`create_usage_type`, `get_usage_type`, `list_usage_types`, `delete_usage_type`); their per-capability flows are owned by the features below. No REST or network-exposed surface.

- **Sequences**:
  - None (bootstrap and registration expose no runtime SPI sequence).

- **Data**:
  - [ ] `p3` - `cpt-cf-uc-plugin-db-schema`

- **Contracts**:
  - [ ] `p1` - `cpt-cf-uc-plugin-contract-timescaledb`
  - [ ] `p2` - `cpt-cf-uc-plugin-contract-gts-registration`

### 2.2 Record Persistence & Lifecycle ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-feature-record-persistence`

- **Purpose**: Provide the backend write plane — the sole path into `usage_records`. Single and batch inserts resolve against the hypertable's 4-tuple dedup constraint as the atomic serialization authority: a fresh key inserts; on a dedup-key collision the canonical fields (`value`, `resource_ref`, `subject_ref`, `corrects_id`, `metadata`) are compared — all equal yields silent absorb of the stored row, any differ yields `IdempotencyConflict`. Because `created_at` is part of the dedup key rather than a compared field, a same-key submission with a different `created_at` is a distinct 4-tuple and a new record (ADR-0014). Batch writes return one positionally-aligned outcome per input record, and a conflict on one record never fails the others. Compensation rows (a caller-supplied signed `value` plus `corrects_id`) persist verbatim through the same insert path with no dedicated operation and no netting. Deactivation is a one-way `active → inactive` status flip that, in a single transaction, flips the target row and its depth-1 active compensations and mutates no other field.

- **Depends On**: `cpt-cf-uc-plugin-feature-foundation`

- **Scope**:
  - Single insert with 4-tuple dedup (exact-equality canonical-field comparison on conflict), `metadata` persisted byte-for-byte, `status = Active` on first accept.
  - Batch insert as a single multi-row write returning per-record results in input order.
  - Compensation persistence: signed `value` + optional `corrects_id` on the ordinary insert path; the plugin computes and validates no netting.
  - Depth-1 atomic deactivation cascade: `UsageRecordNotFound` on a missing target, `UsageRecordAlreadyInactive` on an already-inactive target, otherwise flip target plus depth-1 active compensations.
  - `get_usage_record` by `id` (the deterministic `UUIDv5` of the 4-tuple, ADR-0013/ADR-0014, resolving a single row).
  - Bulk-ingestion throughput allocation through the batch write path within the parent throughput-profile envelope.

- **Out of scope**:
  - Reading records back for aggregation or keyset list — owned by [§2.3](#23-query--aggregation--high) Query & Aggregation.
  - Schema DDL for `usage_records` (hypertable, dedup UNIQUE, FK, indexes) — created by [§2.1](#21-foundation-bootstrap-schema--spi-wiring--high) Foundation's Schema Migrations; this feature is the row-writer.
  - Chunk expiry / retention of stored rows — owned by [§2.5](#25-data-retention--medium) Data Retention.

- **Requirements Covered**:
  - [x] `p1` - `cpt-cf-uc-plugin-fr-record-persistence`
  - [x] `p1` - `cpt-cf-uc-plugin-fr-idempotent-dedup`
  - [x] `p2` - `cpt-cf-uc-plugin-fr-compensation-persistence`
  - [x] `p1` - `cpt-cf-uc-plugin-fr-deactivation`
  - [x] `p1` - `cpt-cf-uc-plugin-nfr-ingestion-throughput`

- **Design Principles Covered**:
  - None (realizes the pure-persistence principle owned by Foundation).

- **Design Constraints Covered**:
  - [ ] `p2` - `cpt-cf-uc-plugin-constraint-dedup-key-preservation`

- **Domain Model Entities**:
  - `UsageRecord`

- **Design Components**:
  - [ ] `p2` - `cpt-cf-uc-plugin-component-record-store`

- **API**:
  - `create_usage_record` — persist one record; dedup or `IdempotencyConflict`.
  - `create_usage_records` — batch persist; per-record results in input order.
  - `get_usage_record` — fetch one record by `id`.
  - `deactivate_usage_record` — depth-1 atomic `active → inactive` set flip.

- **Sequences**:
  - `p1` - `cpt-cf-uc-plugin-seq-ingest-dedup`
  - `p1` - `cpt-cf-uc-plugin-seq-ingest-batch`
  - `p1` - `cpt-cf-uc-plugin-seq-deactivate-cascade`

- **Data**:
  - `p1` - `cpt-cf-uc-plugin-dbtable-usage-records`

### 2.3 Query & Aggregation ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-feature-query-aggregation`

- **Purpose**: Provide the backend read plane, pushing analytical work into the backend so TimescaleDB's native acceleration does it. Aggregation executes SUM/COUNT/MIN/MAX/AVG with grouping over the requested dimensions inside the backend, applying the host-supplied filter and scope over the active-row set, and never returns raw rows for client-side aggregation. The raw list is keyset (seek) pagination over the canonical `(created_at, id)` order: it fetches one extra row to detect a next page, trims, and seeds the next `CursorV1` from the last in-page row, honoring the host order and cursor and refusing an order key on a nullable field so no matching record is silently dropped. All translation is injection-safe: comparison values (scope/tenant predicate, time bounds, cursor seek key, metadata key/value) are bound parameters, and any caller-influenced identifier is resolved through a closed allowlist of `usage_records` columns or rejected as `Internal`. This feature is the allocation target for the aggregation query-latency NFR (p95 ≤ 500 ms over a 30-day single-tenant range).

- **Depends On**: `cpt-cf-uc-plugin-feature-foundation`, `cpt-cf-uc-plugin-feature-record-persistence`

- **Scope**:
  - Pushed-down aggregation (SUM/COUNT/MIN/MAX/AVG with grouping) over the active-row set with the host filter and scope applied.
  - Keyset-paginated raw list over `(created_at, id)` honoring the supplied order and cursor, with a one-row look-ahead and next-cursor encoding; the effective page size is floored to 1.
  - Injection-safe translation: comparison values are bound parameters and identifiers are allowlisted; metadata predicates bind both the metadata key and the compared value as parameters, so the open-ended metadata namespace needs no key enumeration.
  - Fail-closed rejection of a `$orderby` on a domain-optional (nullable) field, so a crafted cursor cannot smuggle a nullable ordering key past the query boundary.
  - Aggregation query-latency NFR allocation through server-side aggregation and time-bucketed hypertable indexes.

- **Out of scope**:
  - Writing, dedup, compensation, or deactivation of records — owned by [§2.2](#22-record-persistence--lifecycle--high) Record Persistence & Lifecycle.
  - Catalog listing keyset pagination (`list_usage_types`) — reuses this same keyset pattern but is owned by [§2.4](#24-usage-type-catalog--referential-integrity--medium) Usage-Type Catalog & Referential Integrity against the Catalog Store.
  - The `usage_records` table DDL and row-writer ownership — created by Foundation, written by Record Persistence & Lifecycle; this feature is a reader and re-owns neither.

- **Requirements Covered**:
  - [x] `p1` - `cpt-cf-uc-plugin-fr-aggregated-query`
  - [x] `p2` - `cpt-cf-uc-plugin-fr-raw-query`
  - [x] `p1` - `cpt-cf-uc-plugin-nfr-query-latency`

- **Design Principles Covered**:
  - None (realizes the pure-persistence principle owned by Foundation).

- **Design Constraints Covered**:
  - [ ] `p2` - `cpt-cf-uc-plugin-constraint-injection-safe-translation`

- **Domain Model Entities**:
  - `UsageRecord` (read)
  - `AggregationSpec`
  - `AggregationResult`
  - `ODataQuery`
  - `CursorV1`
  - `Page`

- **Design Components**:
  - None (query execution lives in the Record Store component owned by [§2.2](#22-record-persistence--lifecycle--high) Record Persistence & Lifecycle; this feature owns its read-path sequences and the injection-safe translation constraint, referencing that component rather than re-owning it).

- **API**:
  - `query_aggregated_usage_records` — pushed-down SUM/COUNT/MIN/MAX/AVG + group-by.
  - `list_usage_records` — keyset-paginated raw page.

- **Sequences**:
  - `p1` - `cpt-cf-uc-plugin-seq-query-aggregated`
  - `p2` - `cpt-cf-uc-plugin-seq-list-keyset`

- **Data**:
  - `cpt-cf-uc-plugin-dbtable-usage-records` (reader; written by [§2.2](#22-record-persistence--lifecycle--high) Record Persistence & Lifecycle — shared usage, not re-owned).

### 2.4 Usage-Type Catalog & Referential Integrity ⏳ MEDIUM

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-feature-usage-type-catalog`

- **Purpose**: Own the sole store for the usage-type catalog and the in-database referential integrity between records and types. `create_usage_type` inserts a catalog row keyed on the `gts_id` primary key (a collision surfaces `UsageTypeAlreadyExists`); `get_usage_type` returns the row or `UsageTypeNotFound`; `list_usage_types` is keyset-paginated ordered by `gts_id`; `delete_usage_type` attempts the delete and lifts the `usage_records.gts_id → usage_type_catalog.gts_id` `ON DELETE RESTRICT` FK violation to `UsageTypeReferenced`. Because both tables live in the same database, the FK rejection is atomic with the delete attempt and orphaned records are structurally impossible. `kind` and `metadata_fields` are stored verbatim; the plugin derives and validates no semantics.

- **Depends On**: `cpt-cf-uc-plugin-feature-foundation`

- **Scope**:
  - Catalog create (insert; `gts_id` PK collision → `UsageTypeAlreadyExists`), storing `kind` and `metadata_fields` verbatim.
  - Catalog point read (`get_usage_type`; absent → `UsageTypeNotFound`).
  - Catalog keyset-paginated list ordered by `gts_id` (reusing the raw-list keyset pattern against the Catalog Store).
  - Catalog delete with in-database FK rejection lifted to `UsageTypeReferenced`; the catalog is reference data and is not retention-bounded.

- **Out of scope**:
  - Validation of metadata-key well-formedness or membership, and counter/gauge derivation — inherited pure-persistence posture owned by [§2.1](#21-foundation-bootstrap-schema--spi-wiring--high) Foundation and enforced upstream by the gear core.
  - The `usage_records` FK column definition (DDL) — created by Foundation's Schema Migrations; this feature owns the catalog-side delete behavior that the FK backs.

- **Requirements Covered**:
  - [x] `p1` - `cpt-cf-uc-plugin-fr-usage-type-catalog`
  - [x] `p1` - `cpt-cf-uc-plugin-fr-referential-integrity`

- **Design Principles Covered**:
  - None (realizes the pure-persistence principle owned by Foundation).

- **Design Constraints Covered**:
  - [ ] `p2` - `cpt-cf-uc-plugin-constraint-fk-referential-integrity`

- **Domain Model Entities**:
  - `UsageType`

- **Design Components**:
  - [ ] `p2` - `cpt-cf-uc-plugin-component-catalog-store`

- **API**:
  - `create_usage_type` — insert catalog row.
  - `get_usage_type` — fetch catalog row by `gts_id`.
  - `list_usage_types` — keyset-paginated catalog page.
  - `delete_usage_type` — delete; FK rejection → `UsageTypeReferenced`.

- **Sequences**:
  - `p1` - `cpt-cf-uc-plugin-seq-create-type`
  - `p1` - `cpt-cf-uc-plugin-seq-delete-type-fk`

- **Data**:
  - `p1` - `cpt-cf-uc-plugin-dbtable-usage-type-catalog`

### 2.5 Data Retention ⏳ MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-feature-retention`

- **Purpose**: Bound `usage_records` storage growth for the append-heavy time-series workload without a gear-side delete path. A declarative TimescaleDB retention policy — registered idempotently at startup by Schema Migrations and run as a backend background job — drops every chunk whose `created_at` window lies wholly outside the configured `retention_period` (default 365 days); the plugin issues no row-level delete for expiry. Retention is keyed on event time, so a row's eligibility follows its `created_at` independent of `ingested_at`. Because the dedup `UNIQUE` index rides the hypertable's chunk lifecycle, dropping a chunk also reclaims that chunk's dedup keys — which is why idempotency-key preservation is retention-bounded (chunk-granular) rather than permanent, a divergence tracked for upstream reconciliation. The `usage_type_catalog` is reference data and is not retained.

- **Depends On**: `cpt-cf-uc-plugin-feature-foundation`

- **Scope**:
  - Declarative event-time retention policy on the `usage_records` hypertable, dropping wholly-expired chunks at the configured `retention_period`.
  - Idempotent policy registration at `init` (no row-level expiry deletes), re-runnable as a no-op on restart.
  - The retention-bounded dedup-key-preservation consequence and its operator sizing guidance (retention window must exceed the maximum client replay/backfill horizon).

- **Out of scope**:
  - Creation of the `usage_records` hypertable and the registration call site — created by [§2.1](#21-foundation-bootstrap-schema--spi-wiring--high) Foundation's Schema Migrations; this feature owns the policy's runtime expiry semantics and the key-preservation contract.
  - The 4-tuple dedup-write behavior itself — owned by [§2.2](#22-record-persistence--lifecycle--high) Record Persistence & Lifecycle (coupled here via key preservation).
  - Columnar compression of aging chunks and continuous-aggregate rollups — deferred post-v1 ([§2.7](#27-deliberate-omissions)).

- **Requirements Covered**:
  - [x] `p2` - `cpt-cf-uc-plugin-fr-retention`

- **Design Principles Covered**:
  - None.

- **Design Constraints Covered**:
  - [ ] `p2` - `cpt-cf-uc-plugin-constraint-retention`

- **Domain Model Entities**:
  - `UsageRecord` (chunk-expired subject; not re-owned)

- **Design Components**:
  - None (the retention policy is registered by the Schema Migrations component owned by [§2.1](#21-foundation-bootstrap-schema--spi-wiring--high) Foundation and thereafter runs as a backend job; this feature owns the retention and dedup-key-preservation constraints, referencing that component rather than re-owning it).

- **API**:
  - None (declarative backend policy; no SPI method).

- **Sequences**:
  - None (backend background job; no host-facing SPI sequence).

- **Data**:
  - `cpt-cf-uc-plugin-dbtable-usage-records` (retention target; written by [§2.2](#22-record-persistence--lifecycle--high) Record Persistence & Lifecycle — shared usage, not re-owned).

### 2.6 Backend Observability & Metrics ⏳ MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-feature-observability`

- **Purpose**: Emit the backend-internal telemetry the gear cannot see, under the plugin's own `uc_timescaledb_*` OpenTelemetry sub-namespace exported via OTLP, distinct from the gear's request-path signals. The inventory covers performance (insert/query/deactivate/pool-acquire duration histograms), efficiency (pool gauges, dedup-absorbed/stale counters, batch-row histogram), reliability (backend errors by classification, batch retries, idempotency conflicts, usage-type-referenced rejections, migration failures, and the plugin-local `uc_timescaledb_ready` health gauge), security (TLS handshake failures), and catalog/workload (catalog size, compensations, query-request mix). Histogram bucket layouts bracket the NFR p95 budgets and are part of the contract; instrument names are full literal Prometheus names with no unit hint; and unbounded identifiers (`tenant_id`, `gts_id`, `id`, `idempotency_key`, `corrects_id`, `request_id`, `trace_id`) are never used as labels. Each SPI dispatch records its SQL work under the host's ambient tracing span, so backend latency is attributable end-to-end through the host `trace_id`.

- **Depends On**: `cpt-cf-uc-plugin-feature-foundation`

- **Scope**:
  - The `uc_timescaledb_*` metric inventory (performance, efficiency, reliability, security, catalog/workload groups) with bounded label cardinality and the SLO summary.
  - The plugin-local `uc_timescaledb_ready` backend-health gauge (set after a successful pool build + migration, cleared on pool-acquire failure), distinct from the host-computed structural readiness gauge.
  - Recording each SPI dispatch's SQL work under the host's ambient tracing span (the plugin opens no root span).

- **Out of scope**:
  - The request-path `usage_collector.*` signals and the host-computed structural `usage_collector.plugin.ready` gauge — owned by the gear core.
  - The operations being measured (insert, query, catalog, deactivation, retention) — owned by their respective features above; this feature instruments them cross-cuttingly.

- **Requirements Covered**:
  - [x] `p2` - `cpt-cf-uc-plugin-nfr-operational-visibility`

- **Design Principles Covered**:
  - None.

- **Design Constraints Covered**:
  - None.

- **Domain Model Entities**:
  - None (OpenTelemetry instruments; no persisted entity).

- **Design Components**:
  - [ ] `p3` - `cpt-cf-uc-plugin-design-metric-inventory`

- **API**:
  - None (push-based OTLP export; no SPI method and no scrape endpoint).

- **Sequences**:
  - None.

- **Data**:
  - None.

### 2.7 Deliberate Omissions

The following DESIGN/PRD items are intentionally not assigned to a feature, each with justification:

- **Columnar compression of aging chunks** and **continuous-aggregate (rollup) tables** — deferred post-v1 in DESIGN §4 and PRD §4.2; additive and non-breaking to the SPI surface, so they carry no v1 feature. To be introduced as additive features when scheduled.
- **Product-level gear concerns** — authentication, PDP authorization, attribution and shape validation, idempotency-key presence, counter/gauge semantics, and data classification — owned by the parent Usage Collector gear (`cpt-cf-usage-collector-*`), not re-implemented here; surfaced only as the pure-persistence boundary in [§2.1](#21-foundation-bootstrap-schema--spi-wiring--high) Foundation.
- **Multi-region replication, HA topology, backup/restore, and DR (RPO/RTO)** — governed by the operator's TimescaleDB deployment guide, not by plugin features.
- **Use-case IDs** (`cpt-cf-uc-plugin-usecase-*`) and **actor ID** (`cpt-cf-uc-plugin-actor-plugin-host`) from the PRD are requirement-framing artifacts, realized transitively by the features that cover their underlying FRs (ingest-dedup → [§2.2](#22-record-persistence--lifecycle--high); delete-referenced-type → [§2.4](#24-usage-type-catalog--referential-integrity--medium); bind-startup → [§2.1](#21-foundation-bootstrap-schema--spi-wiring--high)); they introduce no additional design element to own.

## 3. Feature Dependencies

```text
cpt-cf-uc-plugin-feature-foundation
    ↓
    ├─→ cpt-cf-uc-plugin-feature-record-persistence
    │       └─→ cpt-cf-uc-plugin-feature-query-aggregation      (also ← foundation)
    ├─→ cpt-cf-uc-plugin-feature-usage-type-catalog
    ├─→ cpt-cf-uc-plugin-feature-retention                      (dedup-key preservation ⇢ record-persistence)
    └─→ cpt-cf-uc-plugin-feature-observability                  (instruments record-persistence, query-aggregation, usage-type-catalog, retention)
```

**Dependency Rationale**:

- `cpt-cf-uc-plugin-feature-record-persistence` requires `cpt-cf-uc-plugin-feature-foundation`: the write path runs against the `usage_records` hypertable, dedup `UNIQUE`, and connection pool created by Foundation's Schema Migrations and Plugin Module, and returns errors through the adapter's classification.
- `cpt-cf-uc-plugin-feature-query-aggregation` requires `cpt-cf-uc-plugin-feature-foundation`: read execution uses the pool, the adapter's cursor encoding, and the injection-safe translation surface anchored on the schema's column allowlist, all established by Foundation.
- `cpt-cf-uc-plugin-feature-query-aggregation` requires `cpt-cf-uc-plugin-feature-record-persistence`: aggregation and raw list scan `usage_records`, which is written exclusively by the write plane — there is nothing to read until Record Persistence & Lifecycle has accepted records.
- `cpt-cf-uc-plugin-feature-usage-type-catalog` requires `cpt-cf-uc-plugin-feature-foundation`: catalog CRUD runs against the `usage_type_catalog` table and the `ON DELETE RESTRICT` FK created by Foundation's Schema Migrations, and reuses the adapter's keyset pattern.
- `cpt-cf-uc-plugin-feature-retention` requires `cpt-cf-uc-plugin-feature-foundation`: the declarative retention policy is registered at `init` by Foundation's Schema Migrations against the `usage_records` hypertable; this feature owns its runtime expiry and key-preservation semantics.
- `cpt-cf-uc-plugin-feature-retention` is coupled to `cpt-cf-uc-plugin-feature-record-persistence` via **dedup-key preservation**: dropping an expired chunk also drops that chunk's entries in `usage_records_dedup_uniq`, so a replay of the now-recordless 4-tuple is accepted as a fresh insert. This is a runtime coupling (the retention window bounds the dedup guarantee), not an implementation prerequisite — retention operates on rows the write plane produces but does not require it to be built first.
- `cpt-cf-uc-plugin-feature-observability` requires `cpt-cf-uc-plugin-feature-foundation`: metrics are emitted from within the adapter and stores using the meter and pool established by Foundation; the feature instruments the other capabilities cross-cuttingly but requires only the foundation to exist.
- `cpt-cf-uc-plugin-feature-usage-type-catalog`, `cpt-cf-uc-plugin-feature-retention`, and `cpt-cf-uc-plugin-feature-observability` are independent of each other and of Query & Aggregation, and can be developed in parallel once Foundation exists (Query & Aggregation additionally waits on Record Persistence & Lifecycle for data to read).
