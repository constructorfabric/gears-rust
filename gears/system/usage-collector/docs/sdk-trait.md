# Usage Collector SDK Trait Reference

<!-- toc -->

- [Overview](#overview)
- [Scope](#scope)
  - [In scope](#in-scope)
  - [Out of scope](#out-of-scope)
- [ToolKit SDK placement](#toolkit-sdk-placement)
  - [Crate layout](#crate-layout)
  - [Trait declaration shape](#trait-declaration-shape)
  - [Two-trait split](#two-trait-split)
- [Domain Model](#domain-model)
  - [Core ingestion and identity types](#core-ingestion-and-identity-types)
  - [Query types and views](#query-types-and-views)
  - [Internal authorization types (not part of the SDK signature)](#internal-authorization-types-not-part-of-the-sdk-signature)
  - [Method-specific output types](#method-specific-output-types)
  - [Cross-entity invariants honored by the SDK trait](#cross-entity-invariants-honored-by-the-sdk-trait)
- [Public SDK Trait](#public-sdk-trait)
- [Method Contracts](#method-contracts)
  - [PDP enforcement and plugin dispatch inside the trait implementation](#pdp-enforcement-and-plugin-dispatch-inside-the-trait-implementation)
  - [No dedicated `compensate` method — compensation rides the emit path](#no-dedicated-compensate-method--compensation-rides-the-emit-path)
  - [Method 1 — Create single usage record](#method-1--create-single-usage-record)
  - [Method 2 — Create batched usage records](#method-2--create-batched-usage-records)
  - [Method 3 — Aggregated query](#method-3--aggregated-query)
  - [Method 4 — Raw keyset-paginated query](#method-4--raw-keyset-paginated-query)
  - [Method 5 — Deactivate usage event](#method-5--deactivate-usage-event)
  - [Method 6 — Create usage type](#method-6--create-usage-type)
  - [Method 7 — Get usage type](#method-7--get-usage-type)
  - [Method 8 — List usage types](#method-8--list-usage-types)
  - [Method 9 — Delete usage type](#method-9--delete-usage-type)
  - [Method 10 — Get single usage record](#method-10--get-single-usage-record)
- [Error Taxonomy](#error-taxonomy)
- [Versioning/Compatibility](#versioningcompatibility)
- [Exclusions/Non-goals](#exclusionsnon-goals)
  - [REST-only exclusions](#rest-only-exclusions)
  - [Plugin SPI exclusions](#plugin-spi-exclusions)
  - [Gear non-goals reaffirmed on the SDK trait](#gear-non-goals-reaffirmed-on-the-sdk-trait)
- [Traceability](#traceability)
  - [Trait identifier and consumer contract](#trait-identifier-and-consumer-contract)
  - [Capabilities exposed by the SDK trait](#capabilities-exposed-by-the-sdk-trait)
  - [Domain entities](#domain-entities)
  - [Authorization, fail-closed, and attribution anchors](#authorization-fail-closed-and-attribution-anchors)
  - [Plugin SPI and persistence anchors (exclusions)](#plugin-spi-and-persistence-anchors-exclusions)
  - [Versioning, stability, and quality NFR anchors](#versioning-stability-and-quality-nfr-anchors)
  - [Components allocated to the SDK trait](#components-allocated-to-the-sdk-trait)
- [Open Questions](#open-questions)

<!-- /toc -->

## Overview

The Usage Collector SDK trait is the public, in-process, transport-agnostic
async Rust API for the Usage Collector gear. It exposes the
platform-developer-facing capabilities of the gear — usage record
ingestion, aggregated query, raw cursor-paginated query, and individual
event deactivation — as a single ClientHub-registered async trait. The
trait deliberately omits operator-only catalog and platform-observability
operations, which remain REST-only.

This document is the reference specification for the trait. It captures
the operation set, method contracts (inputs, outputs, error behaviour),
domain types, error taxonomy, ToolKit placement, versioning and stability
policy, and exclusions. The exact Rust signature is owned by the SDK
crate itself; this reference defines what every signature must satisfy.

**Consistency floor (read-after-write rule).** Reads through the SDK trait
inherit the gear-level consistency floor: a record `Acknowledged` by the
ingestion methods (`create_usage_record`, `create_usage_records`) is durable
and dedup-visible on the ingestion path, but the same record MAY be invisible
to a subsequent SDK aggregated query (`query_aggregated`), raw query
(`query_raw_keyset`), or catalog read (`get_usage_type`, `list_usage_types`) for an
indeterminate window. The window is driven by the
active plugin's replication topology and the workload-isolation routing it
implements. Source gears MUST NOT design admission control, post-emit
summary, or any same-request outcome flow against the SDK query methods —
they MUST consume the ingestion ack the SDK already returns. Consumers that
need a tighter bound consciously couple themselves to a specific plugin's
published ceiling. Full contract: DESIGN [§3.10](./DESIGN.md#310-consistency-contract)
(`cpt-cf-usage-collector-design-consistency-contract`,
`cpt-cf-usage-collector-adr-consistency-contract`,
`cpt-cf-usage-collector-nfr-query-freshness`).

## Scope

### In scope

The SDK trait realizes the following Usage Collector functional capabilities:

- Ingestion of UsageRecord submissions, including caller-supplied
  idempotency keys (`cpt-cf-usage-collector-fr-ingestion`,
  `cpt-cf-usage-collector-fr-idempotency`,
  `cpt-cf-usage-collector-seq-emit-usage`).
- Aggregated query against accepted UsageRecords, with PDP-narrowed
  scope and a single mandatory UsageType filter
  (`cpt-cf-usage-collector-fr-query-aggregation`,
  `cpt-cf-usage-collector-seq-query-aggregated`).
- Raw cursor-paginated query against accepted UsageRecords, with PDP
  narrowing and optional UsageType, tenant, resource, and subject filters
  (`cpt-cf-usage-collector-fr-query-raw`,
  `cpt-cf-usage-collector-seq-query-raw`).
- Individual event deactivation, performing a one-way monotonic
  `active -> inactive` status transition
  (`cpt-cf-usage-collector-fr-event-deactivation`,
  `cpt-cf-usage-collector-seq-deactivate-event`).
- UsageType catalog management (`create_usage_type`, `get_usage_type`,
  `list_usage_types`, `delete_usage_type`), realized against the plugin-owned
  `usage_type_catalog`
  (ADR 0012): every usage type carries a closed `metadata_fields`
  declared-key list (`Vec<String>`; all values typed as `String`)
  and its semantics (`counter` or `gauge`) are carried by the closed
  `UsageKind` enum on the catalog row (`UsageType.kind`) per ADR 0012's
  2026-06-08 amendment, read via the `UsageType::is_counter()` /
  `UsageType::is_gauge()` predicates. The `gts_id` derives from the
  reserved abstract base `gts.cf.core.uc.usage_record.v1~` and is
  independent of kind
  (`cpt-cf-usage-collector-fr-usage-type-registration`,
  `cpt-cf-usage-collector-fr-usage-type-deletion`,
  `cpt-cf-usage-collector-fr-usage-type-existence-and-semantics`,
  `cpt-cf-usage-collector-seq-register-usage-type`,
  `cpt-cf-usage-collector-seq-delete-usage-type`).

### Out of scope

The SDK trait does not expose platform health (those endpoints remain
REST-only). Operational telemetry is pushed via OTLP from ToolKit's
global `SdkMeterProvider`; no in-gear HTTP metrics surface exists on
either the SDK trait or the REST API. The SDK trait does not implement
authentication, authorization, storage, cursor token generation, or
aggregation pushdown — authentication is owned by the ToolKit gateway
upstream of the collector, PDP enforcement is allocated to the
per-component `authz_scope` helper inside the trait implementation,
and storage/aggregation pushdown are allocated to the
ClientHub-resolved Plugin SPI. Per
`cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference`
(ADR 0012), UsageType catalog management (`create_usage_type`, `get_usage_type`,
`list_usage_types`, `delete_usage_type`) is in scope for the SDK trait and is
realized by the gateway-side `UsageTypeCatalogService` that owns PDP
authorization, `gts_id` semantics-prefix validation, and closed-shape
`metadata_fields` validation on top of the plugin-owned catalog. The
SDK dispatches to the plugin SPI for durable persistence per ADR 0012;
referential integrity is enforced by the plugin's
`usage_records.gts_id` `ON DELETE RESTRICT` foreign key.

## ToolKit SDK placement

### Crate layout

The `UsageCollectorClientV1` trait lives in `usage-collector-sdk/src/api.rs`.
The companion plugin trait `UsageCollectorPluginV1` lives in
`usage-collector-sdk/src/plugin_api.rs`. Both traits, the GTS spec for
plugin discovery, the domain models, and the public error enum share a
single `usage-collector-sdk` crate — there is no separate `-contracts`
or `-plugin-api` crate. This follows the platform-standard `<gear>` +
`<gear>-sdk` two-crate layout used by every reference gear
(`credstore`, `authn-resolver`, `authz-resolver`).

Required files under `usage-collector-sdk/src/`, all transport-agnostic:

- `lib.rs` — crate root and re-exports.
- `api.rs` — public consumer SDK trait declaration (this document's subject).
- `plugin_api.rs` — public Plugin SPI trait declaration (subject of `plugin-spi.md`).
- `gts.rs` — GTS spec for plugin discovery and binding (reserved; populated by the plugin-registration step per DESIGN §3.12.9).
- `models.rs` — public domain types (see §"Domain Model").
- `error.rs` — public, domain-classified SDK error type (see §"Error
  Taxonomy").

REST DTOs and Axum types must not appear in the SDK crate; the REST
handler in the host `usage-collector` crate converts SDK domain errors
into RFC-9457 `Problem` responses via `IntoResponse`.

The in-process implementation `UsageCollectorLocalClient` lives in
`usage-collector/src/domain/local_client.rs` (inside the host crate)
and is registered un-scoped into ClientHub via
`ctx.client_hub().register::<dyn UsageCollectorClientV1>(...)`, mirroring
the pattern used by `credstore/src/gear.rs:57–58`.

### Trait declaration shape

- The trait is declared `async` (via the `async_trait` pattern), is
  `Send + Sync + 'static`, and is used through ClientHub as a
  trait object.
- The canonical trait name is `UsageCollectorClientV1`, following the
  ToolKit naming convention that places the gear name and capability
  before the `V1` suffix. The `V1` suffix encodes the SDK trait's major
  version and aligns with the gear's major-version stability contract.
- Every method takes `&self` as the receiver and accepts
  `&SecurityContext` as its first explicit parameter; the SDK never
  synthesizes identity or falls back to anonymous access.
- Methods return a `Result` whose `Err` variant is the
  `UsageCollectorError` enum declared in `error.rs` (see §"Error
  Taxonomy"); the `Ok` variant is the method-specific output type
  declared in `models.rs`.
- The trait is registered into ClientHub without scope by the gear's
  `init()`; consumers obtain the client through ClientHub.

### Two-trait split

The public SDK trait, `UsageCollectorClientV1`, is the consumer-facing
trait. The Usage Collector's Plugin SPI trait, `UsageCollectorPluginV1`,
is described in `plugin-spi.md` and lives in the same
`usage-collector-sdk` crate at `plugin_api.rs`, per the platform-standard
two-trait / single-SDK-crate pattern. Both traits share the same
`models.rs` domain types and `error.rs` error enum; no separate
`-contracts` or `-plugin-api` crate sits between them. The Plugin SPI is
not in scope for this reference.

## Domain Model

The SDK trait operates on the canonical Usage Collector domain types defined in
[`domain-model.md`](./domain-model.md). Refer to that document for field-level
semantics, identifier opacity, timestamp conventions, and cross-entity
invariants; the subsections below describe only the **SDK-specific** aspects.

Types reused from `domain-model.md` (declared in `usage-collector-sdk/src/models.rs`,
transport-agnostic):

- Core ingestion: `UsageRecord` (§2.1), `ResourceRef` (§2.2), `SubjectRef` (§2.3),
  `UsageType` (§2.4), `IdempotencyKey` (§2.5), `RecordMetadata` (§2.6),
  `SecurityContext` (§2.7), `UsageRecordStatus` (§2.8).
- Query: `RawQuery` (§3.2), `AggregationResult` (§3.3), plus the SDK-side `TimeWindow`, `AggregationOp`, `AggregationDimension`, `AggregationSpec`, and `AggregationBucket` (the aggregation surface). `AggregationBucket.key` is `Vec<String>`; per-dimension string-encoding rules live in §3.3 of `domain-model.md`.
- Filter / pagination: `UsageRecordQuery` (filterable-field schema, fed to `#[derive(ODataFilterable)]`), `UsageRecordFilterField` (macro-generated, §2.9), `MetadataFilter` (typed side channel for dynamic JSON-key filtering), `Keyset` (§2.10).

Per `cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference`,
counter / gauge semantics are carried by the `kind` column on the catalog row and
surfaced via `UsageType::is_counter()` / `UsageType::is_gauge()`; consumers use
those predicates throughout the SDK surface.

### Query types and views

`RawQuery`, `AggregationResult`, `UsageRecordQuery` /
`UsageRecordFilterField`, `MetadataFilter`, and `Keyset` are defined in
[`domain-model.md`](./domain-model.md) §3 and §2.9–2.10. The SDK-side
aggregation surface — `TimeWindow`, `AggregationOp`,
`AggregationDimension`, `AggregationSpec`, and `AggregationBucket` — is
declared in `models.rs`. `AggregationBucket.key` is `Vec<String>`: each
entry is the string form of the corresponding `AggregationDimension`
(TenantId → `Uuid::to_string()`, lowercase hyphenated; all other
dimensions verbatim from the record). Dimension *kind* is recoverable
by position from the caller-supplied `group_by`. SDK-specific aspects:

- `RawQuery` is realized as the tuple
  `(UsageTypeGtsId, TimeWindow, &toolkit_odata::ODataQuery,
  &[MetadataFilter])` threaded into `list_usage_records`:
  - `gts_id` is **type-enforced** per ADR 0012 — it travels as
    the typed `UsageTypeGtsId` parameter on the SDK signature, not as an
    OData predicate. The gateway resolves the usage type's declared
    `metadata_fields` from this parameter before admitting the rest of
    the query. The `gts_id` field on the OData filter surface is
    reserved: any `gts_id`-touching predicate in the filter AST is
    rejected as a typed validation error so the typed parameter remains the single
    source of truth.
  - The SDK-side `UsageRecordQuery` struct (in `models.rs`) is the
    filterable-field schema for the static portion of `$filter`. It carries
    `#[derive(ODataFilterable)]`; the proc macro generates
    `UsageRecordFilterField` and its `toolkit_odata::filter::FilterField` impl.
    The fixed-field set covers `gts_id` (reserved; see above),
    `tenant_id`, `resource_id`, `resource_type`, `subject_id`,
    `subject_type`, `corrects_id`, and `status`. Nested attribution
    composites are flattened to their leaf identifiers so the
    macro-derived path covers them without a hand-rolled slash-path
    `FilterField` impl.
  - `MetadataFilter` is the typed side channel for filtering on
    `UsageRecord.metadata` JSON-map keys. The OData filter grammar in
    `toolkit-odata` does not express filtering on `serde_json::Value` map
    keys, and no production gear in the workspace extends it for that
    purpose; the SDK signature accepts a `&[MetadataFilter]` instead. Slice
    semantics: AND across distinct keys, OR within `MetadataFilter::values`,
    empty slice = no metadata filter applied.
  - `order` is the canonical `created_at asc, uuid asc`, and `page_after` is
    the typed `(created_at, uuid)` `Keyset` captured by the gateway from the
    caller-supplied opaque `CursorV1`.
- Admissibility is declaration-driven (not a static Rust enum): the SDK
  rejects metadata-key filters whose `key()` is not in the referenced usage
  type's `metadata_fields` before plugin dispatch and surfaces the rejection
  as a typed validation error. `UsageRecordFilterField` itself is closed at compile
  time, so structural validation of static-field references is enforced by
  the `toolkit-odata` filter parser against the macro-generated enum.
- Cursor envelopes are opaque to SDK callers: `toolkit_odata::CursorV1` is minted,
  decoded, and validated by the toolkit gateway. SDK callers pass `limit` and the
  next-page `CursorV1` they read from `page_info.next_cursor`; they never
  observe `page_after` or `Keyset` in raw form. Decode failure, order mismatch,
  and filter mismatch surface as canonical Problem responses (`cursor_decode`,
  `order_mismatch`, `filter_mismatch`).
- Raw-query results are returned as `toolkit_odata::Page<UsageRecord>` with
  `items: Vec<UsageRecord>` and `page_info: PageInfo { next_cursor, prev_cursor,
  limit }`.

### Internal authorization types (not part of the SDK signature)

`PdpDecision` and `PdpConstraint` are
authorization-internal types used by the per-component `authz_scope`
helper invoked inside `UsageCollectorClientV1` (ingestion gateway,
query gateway, deactivation handler, and usage-type catalog). They are
not declared on the public SDK trait surface
and do not appear in SDK method signatures or return shapes. The SDK
trait callers see only the post-authorization outcome (permit produces
a result; deny produces an `Authorization` error variant).

### Method-specific output types

- **Single-record create output**: the persisted `UsageRecord`. A
  successful fresh insert returns the newly written row; an
  exact-equality idempotency retry (same dedup key tuple
  `(tenant_id, gts_id, idempotency_key)` AND all caller-supplied
  canonical fields equal: `value`, `created_at`, `resource_ref`,
  `subject_ref`, `corrects_id`, `metadata`) is
  silently absorbed and returns the previously persisted row as
  `Ok(UsageRecord)`. A same-key resubmission whose canonical fields
  differ from the stored record is NOT an acknowledgement — it
  surfaces as the `UsageCollectorError::IdempotencyConflict` error
  variant (see §"Error Taxonomy"), so a divergent same-key re-emission
  is rejected fail-closed and never silently dropped. There is no
  separate `UsageRecordAck` envelope and no `DedupOutcome` indicator on
  the `Ok` arm — callers that need to distinguish "newly inserted" from
  "silent-absorb retry" do so out of band (e.g., compare the returned
  record's `created_at` against the call's wall-clock).
- **Batch-create output**: a list of per-record `Result` aligned with
  the input order. Each `Ok` arm carries the persisted `UsageRecord`
  with the same semantics as the single-record method; per-record
  failures (validation, PDP denial, idempotency conflict, plugin
  error) surface as the `Err` arm on that record's slot.
- **Deactivate output**: `()` (unit). A successful return guarantees
  the targeted record was `Active` before the call and is now
  `Inactive`, AND every currently-active compensation row whose
  `corrects_id` equals the targeted row's id has likewise been
  flipped to `Inactive` in the same atomic plugin transaction. The
  set of cascade-flipped ids is not part of the return shape;
  operators that need it inspect the plugin-owned `status` and
  `corrects_id` columns through a follow-up `list_usage_records`
  query. The cascade is strictly depth-1 by construction —
  compensations are not themselves compensable because L1 rejects a
  `corrects_id` that targets a row with `corrects_id IS NOT NULL`.
  Rejection cases (unknown record, already-inactive record) are
  surfaced through error variants of `UsageCollectorError` (see §"Error
  Taxonomy"), per `domain-model.md` §2.10 "A second deactivation
  request for an inactive record is rejected with an actionable error"
  and DESIGN.md `cpt-cf-usage-collector-principle-monotonic-deactivation`
  "rejects deactivation requests against already-inactive records".

These output shapes are declared in `usage-collector-sdk/src/models.rs`
and `usage-collector-sdk/src/api.rs` so that callers can pattern-match
results without parsing error shapes. They are derived from the surface
mapping facts in phase-01 §"Plugin Binding And Surface Mapping" / "SDK
trait" and the dedup / deactivation outcome facts in phase-01
§"Ingestion" and §"Event deactivation"; phase-02 §"SDK Method Inputs
And Outputs" and §"Open Questions (annotated)" OQ-3.

Catalog-operation input/output types (per ADR 0012):

- `UsageTypeGtsId` — opaque GTS usage type identifier string (suffixed `~`)
  for a registered usage type. Declared in
  `usage-collector-sdk/src/models.rs` alongside the other identity
  types; this is the catalog primary key and the FK column on
  `usage_records` per ADR 0012. Derives structurally from the reserved
  abstract base `gts.cf.core.uc.usage_record.v1~`; counter / gauge
  classification is carried separately on `UsageType.kind` and does NOT
  travel on the identifier.
- `UsageKind` — closed Rust enum (`Counter`, `Gauge`) carried by
  every `UsageType` per ADR 0012's 2026-06-08 amendment. Serde
  `rename_all = "lowercase"` so the wire shape is `"counter"` /
  `"gauge"`; unknown variants are rejected at the serde deserialize
  boundary. CF-platform-internal; not vendor-extensible.
- `UsageType` — caller-supplied usage type registration
  payload (mirrors the plugin SPI per `plugin-spi.md` §"Method 6"):
  `gts_id` (GTS usage type identifier, suffixed `~`, required;
  deployment-unique; MUST derive from the reserved abstract base
  `gts.cf.core.uc.usage_record.v1~` with at least one further
  `~`-separated segment per ADR 0012),
  `kind` (`UsageKind`, required; closed counter / gauge classification),
  `metadata_fields` (`Vec<String>`, required; flat closed list of
  declared metadata keys for the usage type — unique non-empty strings;
  all corresponding values are typed as `String` at ingest;
  undeclared keys at record ingest are rejected with
  `UnknownMetadataKey`). Gateway-validated structurally (PDP
  authorization; `gts_id` base-derivation check against the reserved
  abstract base; `kind` closed-enum check at the serde deserialize
  boundary; `metadata_fields` well-formedness — unique non-empty strings)
  before the plugin SPI dispatch — see
  `cpt-cf-usage-collector-component-usage-type-catalog`. `gts_id` and
  `kind` are independent fields; the SDK exposes the `.is_counter()` /
  `.is_gauge()` predicates on `UsageType` (reading `self.kind`) rather
  than on the gts_id.
- `ODataQuery` — the platform `toolkit_odata::ODataQuery` envelope reused
  as the single input to `list_usage_types`. Carries the optional `limit`
  and `cursor` (the only fields the foundation surface exposes through the
  REST query params), plus the parsed `filter_ast` / `order` / `select`
  fields. Counter / gauge selection is performed client-side by reading
  `UsageType.kind` on the catalog row — the foundation surface declares
  no filterable usage-type fields, and any filter expression carried on
  the query is currently ignored by the plugin.
- `Page<T>` — the platform `toolkit_odata::Page<T>` envelope reused
  for catalog list responses: `items: Vec<T>` plus a `page_info`
  block carrying `next_cursor`, `prev_cursor`, and `limit`.

### Cross-entity invariants honored by the SDK trait

SDK-specific extensions of the cross-entity invariants in `domain-model.md` §7:

- Every read and write requires a resolved `SecurityContext` and a positive PDP
  decision. The SDK fails closed on missing/invalid `SecurityContext`, PDP
  failure, validation failure, plugin-readiness failure, or storage failure.
- Records referencing an unknown `gts_id` are rejected as `UsageTypeNotFound`
  before persistence; referential integrity of
  `usage_records.gts_id → usage_type_catalog(gts_id)` is enforced by the plugin
  via `ON DELETE RESTRICT` per ADR 0012.
- Per ADR 0012 (2026-06-02 amendment), metadata is closed-shape: undeclared
  keys are rejected at the SDK boundary as `UnknownMetadataKey { gts_id, key }`
  before plugin dispatch; values are typed as `String` end-to-end; per-usage-type
  queryable dimensions are exactly the keys in `metadata_fields`.

## Public SDK Trait

The Usage Collector exposes one public SDK trait,
`UsageCollectorClientV1`. The trait is async, `Send + Sync + 'static`,
declared in `usage-collector-sdk/src/api.rs`, and registered into
ClientHub without scope by the Usage Collector gear's `init()`.

The trait carries nine methods, one per SDK-exposed capability (ADR 0012 removed `read_usage_type_chain`):

| Method (logical)             | Realizes                                                                                                        | Inputs (beyond `&SecurityContext`)                                                                                                                                                                                                                                                                                                                                                             | Output (Ok variant)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| ---------------------------- | --------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Create single usage record   | `fr-ingestion`, `fr-idempotency`, `fr-usage-compensation`, `seq-emit-usage`                                     | One `UsageRecord` value (per-record fields described in §"Method Contracts", including the optional `corrects_id` discriminator and a signed `value`).                                                                                                                                                                                                                                         | The persisted `UsageRecord` (newly written on a fresh insert; the previously stored row on an exact-equality idempotency retry — silent absorb). Same-key canonical-field mismatch surfaces as the `IdempotencyConflict` error variant.                                                                                                                                                                                                                                                                            |
| Create batched usage records | `fr-ingestion`, `fr-idempotency`, `fr-usage-compensation`, `nfr-throughput`, `nfr-batch-and-report-timing`      | Non-empty list of `UsageRecord` (each carrying an optional `corrects_id`).                                                                                                                                                                                                                                                                                                                     | List of per-record `Result` aligned with the input order; each `Ok` arm carries the persisted `UsageRecord`.                                                                                                                                                                                                                                                                                                                                                                                                       |
| Aggregated query             | `fr-query-aggregation`, `seq-query-aggregated`                                                                  | `gts_id: UsageTypeGtsId` (typed usage-type key — required; the gateway rejects any `gts_id`-touching predicate in `query` as a typed validation error), `TimeWindow` (`[from, to)` UTC), `&ODataQuery` (filter only — pagination/order ignored), `&[MetadataFilter]` (typed JSON-key side channel), and `AggregationSpec` (`op` ∈ SUM / COUNT / MIN / MAX / AVG, plus ordered `group_by`).                                                                                                                          | `AggregationResult` — `Vec<AggregationBucket>`. Each bucket carries a `key: Vec<String>` (in `group_by` order; empty when `group_by` was empty) and `value: Option<Decimal>`. Dimension values are emitted as their canonical string form (TenantId → `Uuid::to_string()`, lowercase hyphenated; all others verbatim from the record).                                                                                                                                                                                                                                                                                                                                  |
| Raw keyset-paginated query   | `fr-query-raw`, `seq-query-raw`                                                                                 | `gts_id: UsageTypeGtsId` (typed usage-type key — required; the gateway rejects any `gts_id`-touching predicate in `query` as a typed validation error), `TimeWindow` (`[from, to)` UTC; replaces any `created_at` filter clause), `&ODataQuery` (parsed `$filter` over `UsageRecordFilterField`, `order`, optional `page_after`, `limit`), and `&[MetadataFilter]` (typed side channel for dynamic JSON-key filtering; AND across entries, OR within `values`).                                                                                                                                                                | `toolkit_odata::Page<UsageRecord>` (`items` + `page_info` with opaque `CursorV1`).                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| Deactivate usage event       | `fr-event-deactivation`, `seq-deactivate-event`                                                                 | The target `UsageRecord.uuid` (either an ordinary usage row or a compensation row).                                                                                                                                                                                                                                                                                                              | `()`. The targeted row and every currently-active compensation row whose `corrects_id` equals the targeted id are flipped to `inactive` atomically in one plugin backend transaction; the SDK / REST surface returns no body (HTTP 204 No Content). Rejections for already-inactive or unknown records are surfaced as error variants.                                                                                                                                                                            |
| Create usage type          | `fr-usage-type-registration`, `seq-register-usage-type`, `adr-0012-unified-plugin-catalog-and-gts-id-reference` | One `UsageType` (`gts_id: UsageTypeGtsId`, `kind: UsageKind`, `metadata_fields: Vec<String>`). The `gts_id` base-derivation check runs at the `UsageTypeGtsId::new` boundary (`UsageTypeGtsId::new` / `Deserialize`) upstream of this trait call; the `kind` field is validated as a closed `UsageKind` enum at the serde deserialize boundary; the trait implementation then performs PDP authorization and `metadata_fields` well-formedness (unique non-empty strings) before plugin SPI dispatch. | `UsageType` — the durable catalog row produced by the plugin's `create_usage_type` SPI call against the plugin-owned `usage_type_catalog`, carrying the registered `gts_id`, `kind`, and `metadata_fields` per ADR 0012. A `gts_id` that does not derive from the reserved abstract base `gts.cf.core.uc.usage_record.v1~` is rejected at the `UsageTypeGtsId::new` boundary as `UsageCollectorError::Validation` (REST: handler synthesises the canonical `invalid_base_gts_id` `Problem` envelope at HTTP `400` from the failed conversion on `CreateUsageTypeRequest::gts_id`); unknown `kind` values are rejected by closed-enum serde rejection at the deserialize boundary. Duplicate `gts_id` surfaces `UsageTypeAlreadyExists`. |
| Get usage type              | `fr-usage-type-existence-and-semantics`, `adr-0012-unified-plugin-catalog-and-gts-id-reference`                 | `gts_id: GtsId`.                                                                                                                                                                                                                                                                                                                                                                               | `UsageType` carrying `gts_id`, `kind`, and `metadata_fields` (counter / gauge classification is carried by the closed `UsageKind` enum on the row, read via `UsageType::is_counter()` / `UsageType::is_gauge()`). Missing identifier surfaces `UsageTypeNotFound`.                                                                                                                                                                                                                                                                                                                      |
| List usage types             | `fr-usage-type-existence-and-semantics`, `adr-0012-unified-plugin-catalog-and-gts-id-reference`                 | One `&ODataQuery` (`toolkit_odata`) carrying the optional `limit` and `cursor` — the foundation surface declares no filterable usage-type fields and any filter expression is ignored.                                                                                                                                                                                                          | `toolkit_odata::Page<UsageType>` — keyset-paginated list of registered usage types, each row including `gts_id`, `kind`, and `metadata_fields` (counter / gauge classification is carried by `UsageType.kind` per ADR 0012).                                                                                                                                                                                                                                                                                                                               |
| Delete usage type            | `fr-usage-type-deletion`, `seq-delete-usage-type`, `adr-0012-unified-plugin-catalog-and-gts-id-reference`       | `gts_id: GtsId`.                                                                                                                                                                                                                                                                                                                                                                               | `()`. Rejections: `UsageTypeNotFound`, and `UsageTypeReferenced { gts_id, sample_ref_count }` when the plugin's `ON DELETE RESTRICT` FK rejects the delete (lifts to HTTP 409 on the REST surface). Callers MUST expect 409 on referenced usage types.                                                                                                                                                                                                                                                             |

All methods return a `Result` over the listed Ok variant and
`UsageCollectorError` (see §"Error Taxonomy"). The ingestion input
type is `UsageRecord` itself (declared in `models.rs`): the create
surfaces accept the same `UsageRecord` they return. The caller
supplies every field including `uuid` and `created_at`; the persisted
row is byte-identical on a successful insert and on an exact-equality
idempotency replay, so consumers MAY treat the returned value as a
confirmation rather than as a transform of the input.

Note on batched ingestion: phase-02 §"SDK Method Candidates" records
the single-vs-batched shape as a Phase 3 decision (gap G-2). This
reference adopts a two-method form (single and batched) because the
Plugin SPI accepts single and batched records (phase-02 §"Plugin SPI
Boundary"), and the ingestion throughput NFR
(`cpt-cf-usage-collector-nfr-throughput`) requires batched
submissions at the gateway. The single-record method is retained as
the ergonomic case and to keep per-call latency budgets tractable.

Note on `SecurityContext` parameter placement: phase-02 §"ToolKit
Conventions" / "SecurityContext convention" requires `&SecurityContext`
as the first parameter of every SDK method; that convention is the
canonical realization of the cross-entity invariant in phase-01
§"SecurityContext" requiring a resolved context on every operation.

## Method Contracts

Each method contract below lists the realized FR/sequence identifiers,
the required `SecurityContext` invariant, additional structural inputs,
the success output, and the error categories the method may surface.
Concrete error variant names are defined in §"Error Taxonomy".

### PDP enforcement and plugin dispatch inside the trait implementation

The `UsageCollectorClientV1` trait implementation
(`UsageCollectorClientV1` realized by the in-process
`usage-collector` gear crate) is the canonical site for PDP
enforcement, UsageType-existence validation, and Plugin SPI dispatch
for every method below. This is the D11 decision from
`research-toolkit-alignment.md` and is anchored at the per-domain
components (`cpt-cf-usage-collector-component-ingestion-gateway`,
`cpt-cf-usage-collector-component-query-gateway`,
`cpt-cf-usage-collector-component-deactivation-handler`,
`cpt-cf-usage-collector-component-usage-type-catalog`) — each realized
as a service-layer component inside the trait implementation that
calls the shared `authz_scope` helper (a thin wrapper over
`PolicyEnforcer::access_scope_with`), not as Tower /
`OperationBuilder` middleware.

Concretely:

- `OperationBuilder::authenticated()` performs bearer-auth
  resolution and injects a `SecurityContext` extractor. Nothing
  beyond that runs at the framework layer.
- The REST handlers in the gear crate are thin pass-throughs that
  map REST DTO → domain, call
  `UsageCollectorClientV1::<op>(ctx, domain_query)`, map domain →
  REST DTO, and return. They do NOT perform PDP enforcement,
  UsageType-existence validation, or plugin dispatch themselves.
- Inside each `UsageCollectorClientV1::<op>` method, the trait
  implementation performs (in order):
  1. PDP call against the resolved `SecurityContext` for the
     operation under attempt; denial yields the `Authorization`
     error variant.
  2. `PdpConstraint` composition against the caller-supplied filters
     (intersection — user filters can only narrow PDP-authorized
     scope).
  3. UsageType-existence validation against the in-process UsageType
     Catalog projection (`cpt-cf-usage-collector-component-usage-type-catalog`)
     where the operation references a `gts_id`; an
     unregistered identifier yields the `UsageTypeNotFound` error
     variant before plugin dispatch.
  4. Plugin SPI dispatch (`UsageCollectorPluginV1::<spi_method>`)
     for persistence / aggregation / raw-page / deactivation /
     catalog operations as appropriate.
  5. Domain-level translation of `UsageCollectorPluginError` into
     `UsageCollectorError` per §"Error Taxonomy".

This composition keeps in-process SDK callers and out-of-process
REST callers traversing the same authorization, validation, and
plugin-dispatch path — there is no REST-only auth code, no
duplicate UsageType-existence check at the handler layer, and no
PDP-double seam between the SDK and the REST surface.

### No dedicated `compensate` method — compensation rides the emit path

There is NO dedicated `compensate` operation on the
`UsageCollectorClientV1` SDK trait. Compensation rides the unified emit
path — Method 1 `create_usage_record` and Method 2 `create_usage_records`
— by submitting a record with `corrects_id` set to the referenced
ordinary usage row's `UsageRecord.uuid` and a strictly-negative
`value` (counters only — a `corrects_id IS NOT NULL` record on a gauge
usage type is rejected by the four-cell value matrix). Rationale:
consistent PDP attribution on every
ingestion call and mandatory idempotency-key handling on every
ingestion call — one ingestion path, one set of guarantees, and no
second seam where authorization or idempotency could drift. This locks
the `api_shape = single-path` decision recorded in
`cpt-cf-usage-collector-adr-usage-compensation`,
`cpt-cf-usage-collector-fr-usage-compensation`, and
`cpt-cf-usage-collector-flow-usage-emission-compensation` (inlined in
`features/usage-emission.md`); the equivalent posture is mirrored on
the REST surface (no dedicated `compensate` endpoint) and on the
Plugin SPI (no dedicated `compensate` SPI call).

### Method 1 — Create single usage record

- Identifier: `create_usage_record`.
- Realizes: `cpt-cf-usage-collector-fr-ingestion`,
  `cpt-cf-usage-collector-fr-idempotency`,
  `cpt-cf-usage-collector-fr-usage-compensation`,
  `cpt-cf-usage-collector-seq-emit-usage`.
- SecurityContext: required and passed (as the leading `&SecurityContext`
  argument) to the per-component `authz_scope` helper invoked by
  `cpt-cf-usage-collector-component-ingestion-gateway` for ingestion
  authorization against the caller-supplied attribution tuple
  `(tenant_id, resource_ref, UsageType)` (and additionally `subject_ref`
  when present); the PDP authorizes this tuple against the caller's
  gear identity derived from `SecurityContext`.
- Unified ingestion path — single emit operation: this Method is the SDK
  surface for BOTH ordinary usage emission AND the counter
  value-reversal flow described as
  `cpt-cf-usage-collector-flow-usage-emission-compensation` in
  `features/usage-emission.md`. There is NO dedicated `compensate` SDK
  method on `UsageCollectorClientV1`; an explicit note appears at the
  start of §"Method Contracts" explaining the rationale (consistent PDP
  attribution + mandatory idempotency-key handling on every ingestion).
- Structural inputs (carried on a `UsageRecord`,
  named parameters, order-insensitive):
  - `uuid` — caller-supplied `Uuid`; required.
  - `tenant_id` — `Uuid`; required.
  - `resource_ref` — `ResourceRef`; required.
  - `subject_ref` — `SubjectRef`; optional.
  - `gts_id` — GTS usage type identifier string (suffixed `~`); required.
    MUST resolve to a row in `usage_type_catalog` per ADR 0012. This is
    the same value persisted by the plugin as the FK column on
    `usage_records`; no UUID derivation is performed by the trait or
    by the plugin.
  - `value` — signed `rust_decimal::Decimal` (wire-encoded as a JSON
    string, never a float); required. Permitted sign is determined
    jointly by the referenced usage type's semantics (read from the
    catalog row's `kind` field via
    `UsageType::is_counter()` / `UsageType::is_gauge()`) and the presence of
    `corrects_id` on the submission per the four-cell value matrix
    below. The SDK MUST accept strictly-negative numbers (the trait
    MUST NOT pre-clamp, pre-reject, or sign-flip `value`).
  - `created_at` — UTC `Timestamp`; required.
  - `idempotency_key` — `IdempotencyKey`; required.
  - `corrects_id` — opaque `UsageRecord.uuid`; optional. Presence
    (`corrects_id` set / IS NOT NULL) marks the submission as a
    counter-compensation row targeting the referenced ordinary usage
    row; absence (`corrects_id` IS NULL) marks the submission as an
    ordinary usage row. `corrects_id` presence is the sole structural
    discriminator between the two record kinds.
  - `metadata` — `RecordMetadata` (key/value map; string-typed values);
    optional. Validated at the gateway against the referenced usage
    type's `metadata_fields` list (resolved via a `get_usage_type` SPI
    dispatch per record) per ADR 0012 (closed shape, keyed by `gts_id`):
    every key MUST be a member of `metadata_fields`, every value is
    treated as `String`, and undeclared keys surface as
    `UnknownMetadataKey { gts_id, key }` (see §"Error Taxonomy").
    Plugins do NOT re-implement metadata validation.
- Four-cell value matrix (informational; server-enforced — the SDK MUST
  document the matrix and MUST NOT re-validate it locally):

  | Semantics | `corrects_id` presence | Allowed `value`      | Outcome on violation        |
  | --------- | ---------------------- | -------------------- | --------------------------- |
  | `counter` | IS NULL                | `value >= 0`         | typed validation variant   |
  | `counter` | SET                    | `value < 0` (strict) | typed validation variant   |
  | `gauge`   | IS NULL                | any signed value     | n/a                         |
  | `gauge`   | SET                    | (rejected)           | `GaugeCompensationRejected` |

- L1 `corrects_id` rule (informational; server-enforced — the SDK MUST
  document the rule and MUST NOT re-validate it locally). When
  `corrects_id` is present on the emit call, the server enforces the
  following preconditions on the referenced row R:
  1. R MUST exist. Violation surfaces the `CorrectsIdNotFound` error
     variant.
  2. R MUST itself be an ordinary usage row (`R.corrects_id IS NULL`).
     Violation surfaces the `CorrectsIdTargetsCompensation` error
     variant (a `corrects_id` reference MUST NOT target a row that
     already carries `corrects_id IS NOT NULL`; this rule is what
     bounds the deactivation cascade at depth 1).
  3. R.`tenant_id` MUST equal the caller's `tenant_id` AND
     R.`gts_id` MUST equal the caller's `gts_id` per ADR 0012.
     Violation surfaces the `CorrectsIdWrongScope` error variant.
  4. R MUST be active (`status = Active`, not deactivated). Violation
     surfaces the `CorrectsIdInactive` error variant; this rule also
     realizes the concurrency guard against a compensation arriving
     mid-deactivation.

  There is NO L2 remaining-amount check at the SDK layer; per-record
  remaining-amount tracking is an explicit non-goal of the gear.

- Validation behaviour (executed in order before plugin dispatch):
  1. Missing or invalid `SecurityContext` is rejected upstream by the
     ToolKit gateway (the SDK surfaces no `Authentication` error variant)
     and no record reaches the trait implementation.
  2. PDP denial yields the `Authorization` error variant and no record
     is persisted.
  3. Structural attribution validation (required fields present;
     `subject_id` present when `subject_ref` is supplied; `subject_type`
     only with `subject_id`) yields a typed validation error variant on
     failure.
  4. Missing `idempotency_key` yields a typed validation error variant
     (mandatory-idempotency invariant).
  5. Oversized `metadata` (default cap 8 KiB per record, operator
     configurable) yields a typed validation error variant. Any
     metadata key not present in the referenced usage type's declared
     `metadata_fields` yields the
     `UnknownMetadataKey { gts_id, key }` variant per ADR 0012
     (closed shape, keyed by `gts_id`); there is no preserved
     free-form remainder.
  6. Unknown `gts_id` (no row in `usage_type_catalog`) yields the
     `UsageTypeNotFound` error variant per ADR 0012.
  7. Per the four-cell value matrix: `counter` + `corrects_id IS NULL`
     with `value < 0`, and `counter` + `corrects_id SET` with
     `value >= 0`, each yield the typed validation error variant. `gauge`
     - `corrects_id SET` (any value) yields the
       `GaugeCompensationRejected` error variant. `gauge` +
       `corrects_id IS NULL` passes through unchanged.
  8. Per the L1 `corrects_id` rule above: the four `corrects_id_*`
     preconditions surface, respectively, `CorrectsIdNotFound`,
     `CorrectsIdTargetsCompensation`, `CorrectsIdWrongScope`, and
     `CorrectsIdInactive`.
  9. Structural plugin unavailability (the host's selector cache is
     empty OR `ClientHub::try_get_scoped` returns `None`) yields the
     `PluginUnavailable` error variant; types-registry lookup misses
     during scoped-client binding yield `TypesRegistryUnavailable`;
     retryable plugin failures (downstream timeouts, connection resets,
     upstream 5xx) lift through plugin-side `Transient` to
     `ServiceUnavailable`; other plugin errors lift to the unclassified
     `Internal` envelope (no retryable-kind taxonomy exists yet — see
     `plugin-spi.md` for the dispatch-boundary mapping).
  10. A same-key resubmission (same
      `(tenant_id, gts_id, idempotency_key)`) whose caller-supplied
      canonical fields (`value`, `created_at`, `resource_ref`,
      `subject_ref`, `corrects_id`,
      `metadata`) differ from the stored record yields the
      `IdempotencyConflict` error variant and no second record is
      persisted; an exact-equality retry instead silently returns the
      previously persisted `UsageRecord` on the `Ok` arm (silent absorb).
- SDK enforcement posture: the SDK MUST NOT validate net non-negativity
  locally; per-tenant per-UsageType net is owned by the server-side
  un-policed-net posture (`cpt-cf-usage-collector-adr-usage-compensation`) and the SDK only relays the
  resulting state through `AggregationResult`. The SDK MUST surface the
  server's validation error codes faithfully and MUST NOT collapse the
  typed validation variants instead of a single generic `Validation` variant.
- Declared error variants for this method (subset, full taxonomy below):
  `Authorization`, `UsageTypeNotFound`,
  `UnknownMetadataKey`, `GaugeCompensationRejected`,
  `CorrectsIdNotFound`, `CorrectsIdTargetsCompensation`,
  `CorrectsIdWrongScope`, `CorrectsIdInactive`,
  `IdempotencyConflict`, `PluginUnavailable`,
  `TypesRegistryUnavailable`, `ServiceUnavailable`, `Internal`.
- Success output: the persisted `UsageRecord`. The return shape is
  identical for ordinary usage submissions (`corrects_id IS NULL`) and
  counter-compensation submissions (`corrects_id IS NOT NULL`) — there
  is no compensation-specific output variant. When the plugin silently
  absorbs an exact-equality retry, the return value is the previously
  persisted record (whose `created_at` predates the call). A same-key
  submission with any differing canonical field is NOT a success — it
  surfaces as the `IdempotencyConflict` error variant
  (`context.reason = idempotency_conflict`, AIP-193 AlreadyExists /
  409), distinct from the keyless `idempotency` rejection.
- Latency budget: total p95 200 ms per the ingestion latency budget
  (`cpt-cf-usage-collector-nfr-ingestion-latency`); the budget is the
  same regardless of whether `corrects_id` is set on the submission.

### Method 2 — Create batched usage records

- Identifier: `create_usage_records`.
- Realizes: `cpt-cf-usage-collector-fr-ingestion`,
  `cpt-cf-usage-collector-fr-idempotency`,
  `cpt-cf-usage-collector-nfr-throughput`,
  `cpt-cf-usage-collector-seq-emit-usage`.
- SecurityContext: same as Method 1; PDP authorization is performed
  per record against the full attribution tuple.
- Structural inputs: a non-empty list of `UsageRecord`
  values. Each record carries the same fields as in Method 1,
  including the optional `corrects_id` discriminator (absence marks the
  submission as an ordinary usage row, presence marks it as a
  counter-compensation row). A single batched call MAY mix ordinary
  usage submissions and counter-compensation submissions in arbitrary
  order; per-record `corrects_id` presence is independent across the
  list.
- Validation behaviour: each record is validated independently using
  the same rules as Method 1, including the four-cell value matrix and
  the L1 `corrects_id` rule. Per-record outcomes are reported in the
  return list in input order. The call as a whole is rejected only
  when the `SecurityContext` is missing or the input list is empty;
  each yields a typed validation error variant on the call. Per-record
  validation failures, PDP denials, unknown UsageTypes, gauge-compensation
  rejections (the `GaugeCompensationRejected` error variant), invalid
  `corrects_id` references (the four `CorrectsId*` error variants),
  same-key canonical-field-mismatch conflicts (the
  `IdempotencyConflict` error variant, as in Method 1), and plugin
  errors are reported as per-record `Err`-shaped entries within the
  result list — never as a whole-call rejection — while the call
  returns `Ok` on the list as a whole; this aligns with the
  deterministic per-record acceptance promise of the Ingestion Gateway.
- Success output: a list of per-record results, each carrying either
  the persisted `UsageRecord` or a `UsageCollectorError`, in the same
  length and order as the input list.
- Latency and throughput: bounded by
  `cpt-cf-usage-collector-nfr-ingestion-latency` per record and by
  `cpt-cf-usage-collector-nfr-throughput` at the batch.

### Method 3 — Aggregated query

- Identifier: `query_aggregated_usage_records`.
- Realizes: `cpt-cf-usage-collector-fr-query-aggregation`,
  `cpt-cf-usage-collector-seq-query-aggregated`.
- SecurityContext: required; passed (as the leading `&SecurityContext`
  argument) to the per-component `authz_scope` helper invoked by
  `cpt-cf-usage-collector-component-query-gateway` for read
  authorization; PDP-returned constraints are intersected with the
  caller-supplied filters before plugin dispatch.
- Canonical Rust signature:

  ```rust
  async fn query_aggregated_usage_records(
      &self,
      ctx: &SecurityContext,
      gts_id: UsageTypeGtsId,
      window: TimeWindow,
      query: &ODataQuery,
      metadata_filter: &[MetadataFilter],
      aggregation: AggregationSpec,
  ) -> Result<AggregationResult, UsageCollectorError>;
  ```

  Filter/metadata shape and PDP composition are identical to
  [`Method 4 — Raw keyset-paginated query`](#method-4--raw-keyset-paginated-query);
  the only Method-3 specifics are the time window (carried by the
  validated [`TimeWindow`]) and the [`AggregationSpec`].

- Structural inputs:
  - `gts_id: UsageTypeGtsId` — the typed usage-type key (required). The
    gateway resolves the queried usage type's `metadata_fields` from
    this parameter; the `gts_id` field on `UsageRecordFilterField` is
    reserved on this method, and the gateway rejects any
    `gts_id`-touching predicate in `query` as a typed validation error so the typed
    parameter is the single source of truth.
  - `window: TimeWindow` — validated `[from, to)` UTC range; `from < to`
    is enforced at construction (and at the deserialize boundary).
  - `query: &ODataQuery` — same filter surface as `list_usage_records`,
    over [`UsageRecordFilterField`] minus the reserved `gts_id` field.
    Pagination/order fields on the query are ignored on this method
    (the result is not paginated). User-supplied filters can only
    narrow the PDP-authorized scope.
  - `metadata_filter: &[MetadataFilter]` — typed side channel for
    dynamic JSON-key filters (same shape and semantics as
    `list_usage_records`).
  - `aggregation: AggregationSpec` — `op` (one of SUM / COUNT / MIN /
    MAX / AVG) and `group_by` (ordered list of [`AggregationDimension`]
    variants; empty means single-scalar result). Group-by dimensions
    cover `tenant_id`, `resource_id`, `resource_type`, `subject_id`,
    `subject_type`, and `Metadata(<declared key>)`.
- Validation behaviour:
  1. Missing/invalid `SecurityContext` is rejected upstream by the
     ToolKit gateway (the SDK surfaces no `Authentication` error
     variant); PDP denial yields the `Authorization` error variant.
  2. `TimeWindow::new` rejects `from >= to` as `InvalidTimeRange` at the SDK
     boundary; the gateway also surfaces a typed validation error when `query`
     touches the reserved `gts_id` field (the typed `gts_id` parameter
     is the only admissible carrier), when a `Metadata(key)` dimension
     or `MetadataFilter::key()` is not in the resolved
     `metadata_fields`, or when a filter references an undeclared
     field.
  3. An unregistered `gts_id` parameter yields the `UsageTypeNotFound`
     error variant and is rejected before plugin dispatch.
  4. Plugin-side failures map to `PluginUnavailable`,
     `TypesRegistryUnavailable`, `ServiceUnavailable`, or `Internal` as
     in Method 1.
- Success output: `AggregationResult` (a `Vec<AggregationBucket>`; each
  bucket carries a `key: Vec<String>` of `group_by` values in caller
  order and a `value: Option<Decimal>`). Each `key` entry is the
  canonical string form of the corresponding `AggregationDimension`
  (TenantId → `Uuid::to_string()`, lowercase hyphenated; all other
  dimensions verbatim). An empty `group_by` produces a single bucket
  with an empty `key`; an empty match within the authorized scope
  returns an empty `buckets` list and is not an error.
- Latency budget: total p95 500 ms for a 30-day single-tenant
  aggregated query. The
  PRD NFR is authoritative for memory bounds; the SDK trait does not
  enforce numeric caps on result size at the SDK boundary, leaving
  them to the REST/OpenAPI contract and to the plugin.

### Method 4 — Raw keyset-paginated query

- Identifier: `list_usage_records`.
- Realizes: `cpt-cf-usage-collector-fr-query-raw`,
  `cpt-cf-usage-collector-seq-query-raw`.
- SecurityContext: required; the trait implementation invokes the
  per-component `authz_scope` helper inside
  `cpt-cf-usage-collector-component-query-gateway` (passing
  `&SecurityContext` first) for read authorization and intersects
  PDP-returned constraints with the caller-supplied filters before
  plugin dispatch (D11 — see §"PDP enforcement and plugin dispatch
  inside the trait implementation").
- Canonical Rust signature:

  ```rust
  /// Performs PDP enforcement, UsageType-existence validation, PDP-constraint
  /// composition against the filter AST, and plugin dispatch — all inside
  /// this implementation (D11). Cursor handling is invisible to SDK
  /// callers: they pass `page_after` / `limit` (via `ODataQuery`) and
  /// read `page_info.next_cursor` from the returned `Page<UsageRecord>`.
  async fn list_usage_records(
      &self,
      ctx: &SecurityContext,
      gts_id: UsageTypeGtsId,
      window: TimeWindow,
      query: &ODataQuery,
      metadata_filter: &[MetadataFilter],
  ) -> Result<toolkit_odata::Page<UsageRecord>, UsageCollectorError>;
  ```

  `gts_id: UsageTypeGtsId` is the typed usage-type key (required); the
  gateway resolves the queried usage type's declared `metadata_fields`
  from this parameter before admitting any `MetadataFilter`-keyed
  predicate. The `gts_id` field on `UsageRecordFilterField` is
  reserved on this method — any `gts_id`-touching predicate inside
  `query` is rejected as a typed validation error so the typed parameter remains
  the single source of truth.

  `window: TimeWindow` carries the half-open `[from, to)` time range
  (validated at construction; `from < to` is structural). The
  `ODataQuery` filter MUST NOT carry `created_at` predicates — the
  time axis is sourced exclusively from `window`, which is single-
  sourced to avoid a duplicate filter / silent-ignored predicate
  footgun.

  `ODataQuery` is the framework-canonical, non-generic query carrier from
  `toolkit-odata`. The static filter surface is bound to
  `UsageRecordFilterField` at parse time via the macro-derived
  `FilterField` impl on `UsageRecordQuery` (see §"Query types and views").
  Dynamic JSON-key filtering rides the separate `&[MetadataFilter]`
  parameter — the OData filter grammar does not express filters over
  `serde_json::Value` map keys.

- Structural inputs:
  - `gts_id: UsageTypeGtsId` — the typed usage-type key (required per
    ADR 0012; see §"Query types and views"). The gateway resolves the
    usage type's declared `metadata_fields` from this parameter before
    admitting the dynamic-key filters. The `gts_id` field on
    `UsageRecordFilterField` is reserved on this method; any
    `gts_id`-touching predicate inside `query` is rejected as
    a typed validation error so the typed parameter is the single source of truth.
  - `query: &ODataQuery` carrying the parsed `filter_ast` over
    `UsageRecordFilterField` minus the reserved `gts_id` field
    (already PDP-constrained on entry to the plugin dispatch step), the
    canonical `created_at asc, uuid asc` `order`, an optional `page_after`
    keyset (gateway-decoded from the caller-supplied `CursorV1`), and a
    bounded `limit`.
  - `metadata_filter: &[MetadataFilter]` carrying, for each filtered
    metadata key, the key name and the candidate value set. AND across
    entries; OR within `MetadataFilter::values`. An empty slice imposes
    no metadata filter.
- Validation behaviour (executed by the trait implementation, in
  order, before plugin dispatch):
  1. Missing or invalid `SecurityContext` is rejected upstream by the
     ToolKit gateway (the SDK surfaces no `Authentication` error
     variant) and never reaches the trait implementation.
  2. PDP denial yields the `Authorization` error variant. PDP
     constraints are composed against `filter_ast` so the plugin
     receives a filter AST that is authoritatively narrowed.
  3. Typed validation variants are surfaced for structural failures the SDK can
     detect at this boundary — non-positive `limit`, a `filter_ast`
     that touches the reserved `gts_id` field (the typed `gts_id`
     parameter is the only admissible carrier), a `filter_ast` that
     references a field outside the admissible set (fixed
     `UsageRecord` fields minus `gts_id`, plus per-usage-type declared
     keys resolved from the typed `gts_id` parameter's
     `metadata_fields`), an operator outside the per-field allowance
     (dimension filters accept `eq` / `in` only over `String`-typed
     values), or an `order` other than the canonical raw-query order.
  4. An unregistered `gts_id` parameter yields the `UsageTypeNotFound`
     error variant and is rejected before plugin dispatch (D11 —
     UsageType-existence validation inside the trait implementation).
  5. Plugin-side failures map to `PluginUnavailable`,
     `TypesRegistryUnavailable`, `ServiceUnavailable`, or `Internal`.
- Success output: `toolkit_odata::Page<UsageRecord>` with `items`
  (list of `UsageRecord`, each carrying its `status`) and
  `page_info: PageInfo { next_cursor, prev_cursor, limit }` whose
  `next_cursor` is the opaque `CursorV1` minted by the gateway from
  the plugin-returned last-row `Keyset`. An empty match within the
  authorized scope returns an empty `items` list with no
  `next_cursor` and is not an error.
- Pagination behaviour: cursor envelopes are gateway-minted and
  opaque to SDK callers. Callers pass `page_after` / `limit`
  implicitly by threading the `CursorV1` they read from
  `page_info.next_cursor` into the next call; they never observe
  `page_after` or `Keyset` directly. Cursor decode failure, order
  mismatch, and filter mismatch are surfaced as canonical Problem
  responses on the REST surface (`cursor_decode`, `order_mismatch`,
  `filter_mismatch`) and as a typed validation error on the SDK surface — no
  separate cursor-validity error variant exists.

### Method 5 — Deactivate usage event

- Identifier: `deactivate_usage_record`.
- Realizes: `cpt-cf-usage-collector-fr-event-deactivation`,
  `cpt-cf-usage-collector-seq-deactivate-event`.
- SecurityContext: required; passed (as the leading `&SecurityContext`
  argument) to the per-component `authz_scope` helper invoked by
  `cpt-cf-usage-collector-component-deactivation-handler` for
  operator authorization.
- Structural inputs: the target `UsageRecord.uuid`. Deactivation applies
  to a row of either kind — ordinary usage (`corrects_id IS NULL`) or
  counter compensation (`corrects_id IS NOT NULL`); the request
  parameters are unchanged by the compensation primitive.
- Validation behaviour:
  1. Missing/invalid `SecurityContext` is rejected upstream by the
     ToolKit gateway (the SDK surfaces no `Authentication` error
     variant); PDP denial yields the `Authorization` error variant.
  2. Plugin-side failures map to `PluginUnavailable`,
     `TypesRegistryUnavailable`, `ServiceUnavailable`, or `Internal`.
- Rejection behaviour (in addition to missing/invalid SecurityContext,
  PDP denial, and validation):
  - An already-`Inactive` target record yields the `AlreadyInactive`
    error variant; no state change occurs and no other field is
    mutated. This realizes the monotonicity invariant on the SDK
    boundary.
  - An unknown `UsageRecord.uuid` yields the `UsageRecordNotFound` error variant.
- Success output: `()`. The SDK trait returns `Ok(())` and the REST
  surface returns HTTP 204 No Content. On a successful return:
  - The targeted `UsageRecord.uuid` (the explicitly-deactivated row,
    regardless of whether its `corrects_id` is set) was `Active`
    before the call and is now `Inactive`.
  - Every currently-active compensation row whose `corrects_id`
    equals the targeted row's id has likewise been flipped from
    `Active` to `Inactive` inside the same atomic plugin transaction.
    The cascade is non-empty only when the targeted row is an
    ordinary usage row (`corrects_id IS NULL`) AND at least one active
    compensation row references it; deactivating a compensation row
    (`corrects_id IS NOT NULL`) is a single-row, no-cascade operation
    by construction because L1 rejects any `corrects_id` reference
    targeting a row that already carries `corrects_id IS NOT NULL`.

  Operators that need the set of cascade-flipped row ids inspect them
  through a follow-up `list_usage_records` query against the
  plugin-owned `status` and `corrects_id` columns; the SDK trait does
  not propagate the cascade payload on the `Ok` arm.

  The cascade is atomic at the plugin boundary;
  partial cascade is structurally impossible.

- Monotonicity: the only permitted transition is `Active -> Inactive`;
  no reactivation exists; deactivation is atomic at the plugin
  boundary; no other field of any flipped row is mutated. The one-way
  latch applies uniformly to the primary row and to every
  cascade-flipped compensation row.

### Method 6 — Create usage type

- Identifier: `create_usage_type`.
- Realizes: `cpt-cf-usage-collector-fr-usage-type-registration`,
  `cpt-cf-usage-collector-seq-register-usage-type`,
  `cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference`
  (ADR 0012).
- SecurityContext: required; passed (as the leading `&SecurityContext`
  argument) to the per-component `authz_scope` helper invoked by
  `cpt-cf-usage-collector-component-usage-type-catalog` for operator
  authorization. The trait implementation performs the same PDP call
  reached by the REST handler — there is one authorization site, not
  two.
- Canonical Rust signature:

  ```rust
  async fn create_usage_type(
      &self,
      ctx: &SecurityContext,
      input: UsageType) -> Result<UsageType, UsageCollectorError>;
  ```

- Structural inputs:
  `UsageType { gts_id, kind, metadata_fields }`.
  `gts_id` is the GTS usage type identifier (suffixed `~`,
  deployment-unique) and serves as the catalog primary key per
  ADR 0012; it MUST derive from the reserved abstract base
  `gts.cf.core.uc.usage_record.v1~` with at least one further
  `~`-separated segment. `kind` is the closed `UsageKind` enum
  (counter / gauge) carrying the row's counter / gauge classification;
  semantics are read per lookup via `UsageType::is_counter()` /
  `UsageType::is_gauge()`. `metadata_fields` is a `Vec<String>` —
  the closed list of declared metadata keys for this usage type
  (unique non-empty strings; all corresponding values are typed as
  `String` end-to-end). `gts_id` and `kind` are independent fields.
- Caller / plugin validation split: the gateway-side trait
  implementation performs `gts_id` base-derivation validation against
  the reserved abstract base, `kind` closed-enum validation at the
  serde deserialize boundary, and `metadata_fields` well-formedness
  (unique non-empty strings) BEFORE dispatching to the plugin SPI's
  `create_usage_type` (see `plugin-spi.md` §"Method 6"). The plugin
  enforces only structural persistence constraints (`gts_id`
  uniqueness, atomic insert).
- Persistence target: the plugin-owned `usage_type_catalog` table per
  ADR 0012. The catalog PK is `gts_id`; no UUID derivation is
  performed by the gateway or the plugin.
- Validation behaviour (executed in order):
  1. Missing/invalid `SecurityContext` is rejected upstream by the ToolKit gateway (the SDK surfaces no `Authentication` error variant); PDP
     denial yields `Authorization`.
  2. Any `gts_id` defect — empty, missing `~`, or does not derive from
     the reserved abstract base `gts.cf.core.uc.usage_record.v1~` — is
     caught at the `UsageTypeGtsId::new` boundary upstream of this
     trait call. The SDK surfaces it as
     `UsageCollectorError::Validation` (returned by
     `UsageTypeGtsId::new`); the REST handler synthesises the
     canonical `invalid_base_gts_id` `Problem` envelope (HTTP `400`,
     `context.gts_id`, `context.instance_path="/gts_id"`) from the
     same failure on the inbound `CreateUsageTypeRequest::gts_id`
     (whose DTO field is the permissive `GtsInstanceId`). Any unknown
     `kind` value is rejected by closed-enum serde rejection at the
     deserialize boundary before reaching the trait call.
  3. Malformed `metadata_fields` (duplicate keys, empty strings)
     yields `InvalidMetadataField` / `DuplicateMetadataField` carrying the offending
     `/metadata_fields/{index}: {reason}` pointer.
  4. Collision with a previously registered usage type (plugin UNIQUE
     violation on `gts_id`) whose payload differs from the stored
     row yields `UsageTypeAlreadyExists`. An identical payload
     resubmission MUST be idempotent per the SPI contract and
     returns the stored `UsageType` on `Ok`.
- Success output: `UsageType` carrying the durably stored row
  (`gts_id`, `metadata_fields`).
- Idempotency: registration is idempotent on byte-equal payloads;
  any payload divergence on a colliding `gts_id` is reported as
  `UsageTypeAlreadyExists` rather than silently absorbed.

### Method 7 — Get usage type

- Identifier: `get_usage_type`.
- Realizes: `cpt-cf-usage-collector-fr-usage-type-existence-and-semantics`,
  `cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference`
  (ADR 0012).
- SecurityContext: required; passed to `authz_scope` for read
  authorization. Usage types are platform-global, so PDP narrowing is
  applied at the read-permission granularity, not per-tenant.
- Canonical Rust signature:

  ```rust
  async fn get_usage_type(
      &self,
      ctx: &SecurityContext,
      gts_id: GtsId) -> Result<UsageType, UsageCollectorError>;
  ```

- Behaviour: returns the catalog row including `gts_id` and
  `metadata_fields` (with semantics inspected via the
  `UsageType::is_counter()` / `UsageType::is_gauge()` predicates on
  the returned row's `kind` field). The trait implementation dispatches the plugin
  SPI's `get_usage_type` per call (which returns
  `Result<CatalogRow, UsageCollectorPluginError>` per `plugin-spi.md`
  §"Method 7"); the plugin's `UsageTypeNotFound` surfaces verbatim as
  `UsageTypeNotFound` on the admin-GET surface, and is re-classified as
  `UsageTypeNotFound` at the ingestion-path call site.
- Success output: `UsageType`.
- Idempotency: pure read; no observable effect on repeat calls.

### Method 8 — List usage types

- Identifier: `list_usage_types`.
- Realizes: `cpt-cf-usage-collector-fr-usage-type-existence-and-semantics`,
  `cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference`
  (ADR 0012).
- SecurityContext: required; passed to `authz_scope` for read
  authorization.
- Canonical Rust signature:

  ```rust
  async fn list_usage_types(
      &self,
      ctx: &SecurityContext,
      query: &ODataQuery) -> Result<toolkit_odata::Page<UsageType>, UsageCollectorError>;
  ```

- Structural inputs: a single `&ODataQuery` (`toolkit_odata`) that carries
  the optional `limit` and `cursor`. The `cursor` envelope is opaque to
  callers (minted and decoded by the gateway via `toolkit_odata::CursorV1`).
  The foundation surface declares no filterable usage-type fields — any
  filter expression carried on the query is currently ignored by the
  plugin; counter / gauge classification is read from the `kind` field
  on each returned `UsageType` row via `UsageType::is_counter()` / `UsageType::is_gauge()`.
- Behaviour: returns the registered usage type catalog per ADR 0012 via
  the plugin SPI's `list_usage_types` (`plugin-spi.md` §"Method 8"). Each
  row includes `gts_id`, `kind`, and `metadata_fields`; the
  `UsageType::is_counter()` / `UsageType::is_gauge()` predicates on each
  row's `kind` field let consumers inspect counter / gauge semantics without an
  extra round-trip.
- Success output: `toolkit_odata::Page<UsageType>` with `items` and
  `page_info` carrying `next_cursor`, `prev_cursor`, and `limit`.
- Latency: bounded by the plugin's `list_usage_types` per-call timeout.

### Method 9 — Delete usage type

- Identifier: `delete_usage_type`.
- Realizes: `cpt-cf-usage-collector-fr-usage-type-deletion`,
  `cpt-cf-usage-collector-seq-delete-usage-type`,
  `cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference`
  (ADR 0012).
- SecurityContext: required; passed (as the leading `&SecurityContext`
  argument) to `authz_scope` inside
  `cpt-cf-usage-collector-component-usage-type-catalog`.
- Canonical Rust signature:

  ```rust
  async fn delete_usage_type(
      &self,
      ctx: &SecurityContext,
      gts_id: GtsId) -> Result<(), UsageCollectorError>;
  ```

- Delete protocol (executed in order by the trait implementation per
  ADR 0012):
  1. PDP authorize. Denial yields `Authorization`.
  2. Dispatch to the plugin SPI's `delete_usage_type`
     (`plugin-spi.md` §"Method 9"). The plugin attempts the row
     delete inside a single backend transaction; the
     `usage_records.gts_id` `ON DELETE RESTRICT` foreign key fires
     natively on a referenced usage type per ADR 0012.
  3. Translate plugin outcomes per the dispatch boundary:
     `UsageTypeNotFound { gts_id }` from the plugin lifts to
     `UsageCollectorError::UsageTypeNotFound`;
     `UsageTypeReferenced { gts_id, sample_ref_count }` from the plugin
     lifts to `UsageCollectorError::UsageTypeReferenced` and is
     surfaced to REST callers as HTTP 409 (callers MUST expect 409
     on referenced usage types).
- Success output: `()`.
- Idempotency: a second `delete_usage_type` call for the same `gts_id`
  yields `UsageTypeNotFound` (the plugin row is gone after the first
  successful delete). A delete against a referenced usage type never
  silently succeeds — it always surfaces `UsageTypeReferenced`.

> **Removed in ADR 0012.** A prior `read_usage_type_chain` method
> walked `parent_type_uuid` from a target usage type row up to the
> boot-seeded platform base. ADR 0012 flattens the usage type model
> (no `parent_type_uuid`, no ancestor walk); the method and its
> plugin SPI counterpart are removed from this trait.

### Method 10 — Get single usage record

- Identifier: `get_usage_record`.
- Realizes: `cpt-cf-usage-collector-fr-pluggable-storage`,
  `cpt-cf-usage-collector-fr-event-deactivation` (attribution prefetch
  for `cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization`).
- SecurityContext: required; passed (as the leading `&SecurityContext`
  argument) to `authz_scope` inside the
  `cpt-cf-usage-collector-component-deactivation-handler` and the
  point-read REST handler. PDP attributes include the loaded record's
  full attribution tuple (`tenant_id`, `gts_id`, `resource_ref`,
  optional `subject_ref`).
- Canonical Rust signature:

  ```rust
  async fn get_usage_record(
      &self,
      ctx: &SecurityContext,
      uuid: Uuid) -> Result<UsageRecord, UsageCollectorError>;
  ```

- Structural inputs: the target `UsageRecord.uuid`. No filtering by
  `status` — both `active` and `inactive` rows are returned verbatim
  so the deactivation flow can reject already-inactive targets with
  the typed `AlreadyInactive` error variant.
- Validation behaviour (executed in order by the trait implementation):
  1. A missing `SecurityContext` (which would only occur from a
     direct in-process caller that bypasses the gateway) is rejected
     by the trait implementation with no plugin SPI dispatch.
  2. PDP authorize over the resolved record. Existence-leak
     trade-off: the trait fetches the row before PDP enforcement so
     PDP can authorize over the full attribution tuple, which means
     a `UsageRecordNotFound` is observable to any caller that can hit
     the surface. The trade-off is accepted because the operator
     surface is already strongly authenticated by the ToolKit
     gateway, and the alternative (PDP over `uuid` alone) would break
     the resource-attribute reasoning model used by the rest of the
     gear.
  3. Dispatch to the plugin SPI's `get_usage_record`
     (`plugin-spi.md` §"Method 10").
- Success output: the persisted `UsageRecord` (every canonical field
  is present and byte-identical to the row originally persisted by
  Method 1 / Method 2, modulo any subsequent monotonic `status`
  transition through Method 5).
- Declared SDK error variants for this method: PDP denial as
  `Authorization`; an unknown `uuid` as the `UsageRecordNotFound`
  error variant; transient plugin failures as `ServiceUnavailable`;
  uncategorized plugin failures as `Internal`; structural plugin
  unavailability as `PluginUnavailable`.
- Idempotency: pure read; safe to retry.

## Error Taxonomy

All SDK trait methods return `Result<…, UsageCollectorError>`.
`UsageCollectorError` is declared in `usage-collector-sdk/src/error.rs`
as a flat `thiserror::Error` enum and is the public error envelope for
the SDK surface. The SDK crate **does NOT depend on
`toolkit-canonical-errors`**; consumers pattern-match variants of
`UsageCollectorError` directly. This mirrors the platform standard set
by `account-management-sdk`, `credstore-sdk`, `authn-resolver-sdk`, and
`authz-resolver-sdk`.

The host crate (`usage-collector`) provides
`From<UsageCollectorError> for toolkit_canonical_errors::CanonicalError`
in `usage-collector/src/infra/sdk_error_mapping.rs`; REST handlers in
`usage-collector/src/api/rest/handlers.rs` return
`Result<DtoT, UsageCollectorError>` and the canonical RFC-9457
`Problem` envelope is produced by `CanonicalError`'s built-in
`IntoResponse` impl. The `?` operator plus the `From` impl plus
`IntoResponse` drive the wire envelope — handlers do not call
`.map_err(|e| problem(...))` per-route. The SDK never returns a
`ProblemResponse` directly. The AIP-193 mapping (variant → category →
HTTP status) is documented in DESIGN.md §3.3 Error Envelopes.

Variant catalog:

- `Authorization` — PDP denial on the requested operation. Reported
  uniformly for read and write denials; the SDK does not surface the
  PDP reason payload.
- Typed validation variants — structural and semantic validation
  failures are exposed as a family of typed-per-rule variants rather
  than a single umbrella `Validation` variant. Each maps to the
  specific failure mode it names:
  - `InvalidBatchSize` — empty batch or batch exceeding the per-call cap.
  - `MetadataSizeExceeded` — serialized metadata exceeded the per-record cap.
  - `InvalidMetadataField` — entry in `CreateUsageType.metadata_fields`
    was not a well-formed key.
  - `DuplicateMetadataField` — duplicate entry in
    `CreateUsageType.metadata_fields`.
  - `InvalidUsageTypeGtsId` — `UsageTypeGtsId::new` rejected the input
    (malformed GTS id, type id rather than instance id, wrong base, or
    missing derivation segment).
  - `InvalidMetadataKey` — `MetadataKey::new` rejected the input
    (empty or contains a NUL byte).
  - `InvalidMetadataFilter` — `MetadataFilter::new` rejected the input
    (empty values set or invalid key).
  - `InvalidResourceRef` — `ResourceRef::new` rejected the input
    (empty / NUL `resource_id` or `resource_type`).
  - `InvalidSubjectRef` — `SubjectRef::new` rejected the input
    (empty / NUL `subject_id`; `subject_type = Some("")` or NUL).
  - `InvalidIdempotencyKey` — `IdempotencyKey::new` rejected the input
    (empty or NUL); also raised when the mandatory key is missing.
  - `InvalidUsageRecordId` — the URL path `id` segment was not a valid UUID.
  - `InvalidTimeRange` — `TimeWindow::new` rejected the bounds
    (`from >= to`).
  - `InvalidUsageKind` — `UsageKind::from_str` received a wire string
    other than `"counter"` or `"gauge"`.
  - `NegativeCounterValue` — counter ordinary record carried a
    negative value (counter + `corrects_id IS NULL` + `value < 0`).
  - `NonNegativeCounterCompensation` — counter compensation row
    carried a non-negative value (counter + `corrects_id SET` +
    `value >= 0`).
- `UsageTypeNotFound` — see the consolidated entry below. The same
  variant surfaces both ingestion-/query-path misses (the trait
  implementation rejects the request before plugin dispatch when the
  referenced `gts_id` is unknown) and catalog-admin misses
  (`get_usage_type` / `list_usage_types` / `delete_usage_type` on a
  missing target row); the wire shape is identical.
- `UnknownMetadataKey` — Ingestion supplied a `metadata` map carrying
  a key that is not a member of the referenced usage type's declared
  `metadata_fields` list per ADR 0012 (closed shape, keyed by
  `gts_id`). Carries the structured fields `{ gts_id, key }`:
  `gts_id` identifies the usage type whose closed-shape contract
  rejected the key; `key` is the offending undeclared key name.
  Distinct from the typed structural-validation variants because the failure originates at the
  gateway's L1 closed-shape check (not from a structural request
  error) and from `UsageTypeNotFound` because the usage type is known but
  the caller supplied a key outside its declared shape. Plugins do
  NOT re-implement metadata validation. Lifts to AIP-193
  `InvalidArgument` (HTTP 400) on the wire, surfaced as
  `Problem.context.reason="unknown_metadata_key"` with `gts_id` and
  `key` carried in `context`.
- Bad `gts_id` base derivation (REST `POST /usage-collector/v1/usage-types`
  or SDK `create_usage_type` with a manually-constructed
  identifier): the `UsageTypeGtsId` newtype constructor
  (`UsageTypeGtsId::new`) rejects any string that does not derive from
  the reserved abstract base `gts.cf.core.uc.usage_record.v1~` (per
  ADR 0012's 2026-06-08 amendment). The SDK surfaces this as
  `UsageCollectorError::Validation` (the SDK exposes no dedicated
  invalid-base error variant; the typed-newtype is the single
  validation point). Unknown `kind` values are rejected by closed-enum
  serde rejection at the deserialize boundary before this code runs;
  the SDK exposes no dedicated invalid-kind variant either. The REST
  handler synthesises the canonical `Problem` envelope from the same
  failure: HTTP `400`, `context.reason="invalid_base_gts_id"`,
  `context.gts_id` echoes the rejected identifier,
  `context.instance_path="/gts_id"`. The inbound
  `CreateUsageTypeRequest::gts_id` DTO field uses the permissive
  `GtsInstanceId` so the raw string reaches the handler unchanged and
  the structured envelope is built post-deserialize rather than at
  serde time.
- `GaugeCompensationRejected` — Ingestion submitted a record carrying
  `corrects_id IS NOT NULL` against a UsageType whose semantics are
  `gauge`; the four-cell value matrix forbids compensation on gauge
  UsageTypes (gauges already express down-movement directly). Distinct
  from the typed structural-validation variants because the request is
  structurally well-formed and the rejection is the gauge-specific cell
  of the value matrix.
  Lifts to AIP-193 `FailedPrecondition` (HTTP 422) on the wire — the
  request is well-formed but violates a UsageType-semantics precondition.
  Surfaces on the wire as `Problem.context.reason="gauge_compensation_rejected"` per `usage-collector-v1.yaml`.
- `CorrectsIdNotFound` — Ingestion submitted a record carrying
  `corrects_id` that references a `UsageRecord.uuid` not present in the
  caller-visible store (or visible-but-out-of-tenant-scope behind the
  PDP narrowing). Distinct from `UsageRecordNotFound` (which is reserved for the
  deactivation method's missing-target case) and from
  `CorrectsIdWrongScope` (which is raised when the row exists but is
  outside `(tenant_id, gts_id)` scope per ADR 0012). Lifts to AIP-193
  `NotFound` (HTTP 404) on the wire.
  Surfaces on the wire as `Problem.context.reason="corrects_id_not_found"` per `usage-collector-v1.yaml`.
- `CorrectsIdTargetsCompensation` — Ingestion submitted a record carrying
  `corrects_id` that references a row whose own `corrects_id IS NOT
NULL` (i.e. the target row is itself a compensation row); a
  `corrects_id` reference MUST target an ordinary usage row
  (`corrects_id IS NULL`). Lifts to AIP-193
  `FailedPrecondition` (HTTP 409) on the wire.
  Surfaces on the wire as `Problem.context.reason="corrects_id_targets_compensation"` per `usage-collector-v1.yaml`.
- `CorrectsIdWrongScope` — Ingestion submitted a record carrying
  `corrects_id` that references a row whose `(tenant_id, gts_id)`
  does not match the incoming compensation's `(tenant_id, gts_id)`
  per ADR 0012. Cross-tenant or cross-UsageType compensation is
  rejected. Lifts to AIP-193 `FailedPrecondition` (HTTP 409) on the
  wire.
  Surfaces on the wire as `Problem.context.reason="corrects_id_wrong_scope"` per `usage-collector-v1.yaml`.
- `CorrectsIdInactive` — Ingestion submitted a record carrying
  `corrects_id` that references a row whose `status` is `Inactive`
  (already deactivated, including a row that is concurrently in the
  process of being deactivated — the L1 "active" check serialises
  against the cascade transition).
  Distinct from `AlreadyInactive` (which is the deactivation method's
  rejection for a re-deactivation attempt). Lifts to AIP-193
  `FailedPrecondition` (HTTP 409) on the wire.
  Surfaces on the wire as `Problem.context.reason="corrects_id_inactive"` per `usage-collector-v1.yaml`.
- `UsageRecordNotFound` — Deactivation referenced a `UsageRecord.uuid` that does
  not exist.
- `AlreadyInactive` — Deactivation referenced a record whose `status`
  is already `Inactive`; rejecting the second deactivation realizes the
  monotonic-deactivation invariant on the SDK boundary
  (`cpt-cf-usage-collector-principle-monotonic-deactivation`).
- `UsageTypeAlreadyExists` — Catalog `create_usage_type` was called with
  a `gts_id` already present in `usage_type_catalog` and whose request
  payload differs from the stored row (plugin UNIQUE violation
  surfaced as `UsageTypeAlreadyExists { gts_id }` per ADR 0012 and
  `plugin-spi.md` §"Method 6"). An identical-payload resubmission
  is idempotent and returns the stored row on `Ok` — it does NOT
  raise this variant. Distinct from the typed structural-validation variants because the
  duplicate is a domain-state conflict, not a structural
  request error; lifts to AIP-193 `AlreadyExists` (HTTP 409) per
  DESIGN.md §3.3, surfaced on the wire as
  `Problem.context.reason="usage_type_already_exists"`. Name aligns with
  the plugin SPI variant `UsageTypeAlreadyExists`.
- `IdempotencyConflict` — Ingestion submitted a record whose
  `(tenant_id, gts_id, idempotency_key)` collides with a stored
  record **but** whose caller-supplied canonical fields (`value`,
  `created_at`, `resource_ref`, `subject_ref`,
  `corrects_id`, `metadata`) differ from that stored record (see
  §"Method-specific output types"). The second write is rejected
  fail-closed and never silently dropped. Distinct from the typed
  structural-validation variants because the request is structurally
  well-formed (it carries a valid idempotency key) — this is a
  domain-state conflict,
  not a missing-key or malformed-input error. Distinct, too, from the
  keyless `idempotency` rejection: a record submitted WITHOUT the
  mandatory idempotency key is an `InvalidIdempotencyKey` validation error lifting to AIP-193
  `InvalidArgument` (HTTP 400, `context.reason="idempotency"`), whereas
  `IdempotencyConflict` is a same-key divergent-content collision
  lifting to AIP-193 `AlreadyExists` (HTTP 409), surfaced on the wire as
  `Problem.context.reason="idempotency_conflict"` per DESIGN.md §3.3. An
  exact-equality retry is NOT this error — it silently returns the
  previously persisted `UsageRecord` on the `Ok` arm (silent absorb).
- `UsageTypeNotFound` — The referenced `gts_id` is not present in
  the plugin-owned catalog. The same variant surfaces both
  catalog-admin misses (`get_usage_type` / `list_usage_types` /
  `delete_usage_type`) and ingestion / aggregated-query references to
  an unregistered usage type — the wire shape is identical per ADR
  0012. Carries the structured field `{ gts_id }`. Distinct from
  `UsageRecordNotFound` (which is reserved for `UsageRecord.uuid`
  misses on deactivation / point-read). Lifts to AIP-193 `NotFound`
  (HTTP 404) per DESIGN.md §3.3, surfaced on the wire as the canonical
  `not_found` category with `context.resource_type="usage_type"` /
  `context.resource_name=<gts_id>`. Name aligns with the plugin SPI
  variant `UsageTypeNotFound { gts_id }`.
- `UsageTypeReferenced` — Catalog `delete_usage_type` was rejected by the
  plugin's `usage_records.gts_id` `ON DELETE RESTRICT` foreign key
  per ADR 0012. Carries the structured fields
  `{ gts_id, sample_ref_count }`: `sample_ref_count` is a bounded
  sample sufficient to surface "this usage type still has rows" without
  scanning the entire table (the exact bound is plugin-tunable but
  MUST be at least `1`). Callers MUST expect HTTP 409 on referenced
  usage types per the REST contract. Lifts to AIP-193
  `FailedPrecondition` (HTTP 409) per DESIGN.md §3.3, surfaced on
  the wire as `Problem.context.reason="usage_type_referenced"` with
  `context.sample_ref_count`. Name aligns with the plugin SPI
  variant `UsageTypeReferenced { gts_id, sample_ref_count }`.

> **Removed in ADR 0012.** Prior `DeclaredUsageTypeImmutable` variant
> (raised against the gateway-local-from-config catalog) is no
> longer reachable: ADR 0012 retired the local-from-config catalog,
> leaving the plugin-DB catalog as the sole usage-type catalog. The
> equivalent rejection no longer exists. Where the variant was
> raised, the request now succeeds, surfaces `UsageTypeAlreadyExists`,
> or surfaces `UsageTypeNotFound` per the unified semantics.

- `PluginUnavailable` — Structural condition: the host had no scoped
  `dyn UsageCollectorPluginV1` client under
  `ClientScope::gts_id(&instance_id)` (the selector cache was empty OR
  `ClientHub::try_get_scoped` returned `None`) at the time of the call.
  Lifted by the host service from the structural fact; the SPI itself
  exposes no `Unready` variant and no `ready()` probe.
- `TypesRegistryUnavailable` — The `types-registry` lookup the plugin
  host uses to bind the scoped client returned an unavailable result.
  Distinct from `PluginUnavailable`: the selector cache resolution
  itself failed (the registry could not be reached), not the scoped
  client lookup against an already-resolved binding.
- `ServiceUnavailable` — Transient infrastructure failure; carries an
  optional `retry_after_seconds` hint exposed via
  `context.retry_after_seconds` per the canonical envelope. Plugin-side
  `Transient` failures (downstream timeouts, connection resets,
  upstream 5xx) lift here at the dispatch boundary — the SPI does not
  carve a separate `Timeout` variant, so host-side per-call deadline
  expirations also surface as `ServiceUnavailable`. Wire-header
  treatment (whether a corresponding `Retry-After` HTTP header is set)
  is owned by `toolkit-canonical-errors`' `IntoResponse` and is not
  asserted by this gear's contract.
- `Internal` — Unclassified failure. The `detail` field **MUST** be
  DSN-free and pre-redacted at the construction site; no internal
  storage paths, credentials, or stack traces are surfaced through
  this variant. Plugin-side `BackendError { kind, detail }` lifts here
  too — there is no retryable-kind taxonomy at the dispatch boundary
  yet, so backend errors collapse to `Internal` (HTTP 500). When real
  plugins land with a classification scheme, retryable kinds will
  route to dedicated `#[non_exhaustive]` variants.

**Group-membership helpers** (mirroring the platform standard set by
`account-management-sdk`): `UsageCollectorError` exposes category
predicates as part of its public surface so consumers can do
category-level handling without enumerating every variant.

- `is_not_found()` — `UsageRecordNotFound`, `UsageTypeNotFound`,
  `CorrectsIdNotFound`.
- `is_unavailable()` — `PluginUnavailable`,
  `TypesRegistryUnavailable`, `ServiceUnavailable`.
- `is_retryable()` — `PluginUnavailable`,
  `TypesRegistryUnavailable`, `ServiceUnavailable`.
- `is_validation_error()` — `NegativeCounterValue`, `NonNegativeCounterCompensation`, `InvalidBatchSize`, `MetadataSizeExceeded`, `InvalidMetadataField`, `DuplicateMetadataField`, `InvalidUsageTypeGtsId`, `InvalidMetadataKey`, `InvalidMetadataFilter`, `InvalidResourceRef`, `InvalidSubjectRef`, `InvalidIdempotencyKey`, `InvalidUsageRecordId`, `InvalidTimeRange`, `InvalidUsageKind`,
  `UnknownMetadataKey`.
- `is_precondition_failed()` — `AlreadyInactive`,
  `UsageTypeReferenced`, `GaugeCompensationRejected`,
  `CorrectsIdTargetsCompensation`, `CorrectsIdWrongScope`,
  `CorrectsIdInactive`. Deactivation against an already-inactive record
  violates the monotonic-deactivation precondition on the SDK boundary
  (`cpt-cf-usage-collector-principle-monotonic-deactivation`);
  `UsageTypeReferenced` is the referential-delete rejection enforced
  by the plugin's `usage_records.gts_id` `ON DELETE RESTRICT` FK per
  ADR 0012; the four compensation-precondition variants violate the
  compensation preconditions locked by
  `cpt-cf-usage-collector-adr-usage-compensation` and
  `cpt-cf-usage-collector-fr-usage-compensation`.
  All lift to AIP-193 `FailedPrecondition` per DESIGN.md §3.3
  (HTTP 400 for the deactivation case, HTTP 409 for the catalog,
  `UsageTypeReferenced`, and `corrects_id` cases, HTTP 422 for the
  `gauge + compensation` cell, mirroring the canonical mapping for
  state conflicts).
- `is_already_exists()` — Returns true for `UsageTypeAlreadyExists`
  (raised by the catalog write path when `create_usage_type` collides
  on `gts_id` with a divergent payload per ADR 0012) and
  `IdempotencyConflict` (raised by the ingestion path on a same-key
  submission whose canonical fields differ from the stored record);
  reserved for already-exists state conflicts and lifts to AIP-193
  `AlreadyExists` (HTTP 409) per DESIGN.md §3.3.
- `is_permission_denied()` — `Authorization`.

Variants without dedicated predicates (`Authorization`, `Internal`)
are pattern-matched directly because they are either terminal
classifications or carry caller-specific payload.

Adding a new variant means extending the relevant helper in one place
rather than patching every call site.

Behavioural notes:

- Deactivation success returns `Ok(())` on the `Ok` variant (HTTP 204
  on the REST surface); the rejection cases for an already-inactive
  record (`AlreadyInactive`) or an unknown record (`UsageRecordNotFound`) are
  surfaced as error variants per domain-model.md §2.9 and DESIGN.md
  `cpt-cf-usage-collector-principle-monotonic-deactivation`.
- Exact-equality duplicate ingestion submissions are reported as a
  silent-absorb success on the `Ok` variant — the call returns the
  previously persisted `UsageRecord` rather than an error. A same-key
  submission whose canonical fields differ from the stored record is
  NOT reported on the `Ok` arm — it surfaces as the
  `IdempotencyConflict` error variant (`context.reason =
idempotency_conflict`, AIP-193 AlreadyExists / 409), distinct from the
  keyless `idempotency` (`InvalidIdempotencyKey`) rejection.
- Variant naming is canonical for this reference; the SDK crate may
  add per-variant context fields (such as a stable error code or a
  PDP reason category) as long as the public taxonomy preserves the
  domain classification above.

## Versioning/Compatibility

- The SDK trait is one of three independently versioned public
  surfaces (REST API, SDK trait, Plugin SPI). Each surface evolves
  under a major-version stability contract
  (`cpt-cf-usage-collector-adr-contract-stability`,
  `cpt-cf-usage-collector-principle-contract-stability`,
  `cpt-cf-usage-collector-nfr-plugin-contract-stability`).
- The SDK trait's major version is encoded in the trait name suffix
  `V1`. A new major version (`V2`, and so on) is required for any
  breaking change.
- Within a major version only additive changes are permitted: new
  optional methods, new optional fields on input types, and new
  non-required variants on output enums. Removing methods, removing
  or renaming fields, narrowing accepted values, changing semantics,
  or introducing a new required input is a breaking change and
  requires a new major version.
- Deprecation flow: an SDK trait method or field scheduled for
  removal in the next major release must be marked `deprecated` in
  the SDK trait rustdoc at least one minor release before the major
  bump.
- At most one prior major version is supported concurrently per
  surface. The Plugin SPI carries the same posture, so a Usage
  Collector gear instance may serve callers of the current SDK
  major and the immediately prior SDK major in parallel during a
  deprecation window.
- Rust trait compatibility tests gate every PR against the prior
  major per the Contract test category
  (`cpt-cf-usage-collector-nfr-plugin-contract-stability`).
- The SDK trait declares per-call timeouts; the timeout values
  themselves are documented in the SDK rustdoc and bounded by the
  per-operation latency budgets (200 ms for ingestion p95, 500 ms
  for the 30-day single-tenant aggregated query p95). A change to a
  per-call timeout value is an additive observation, not a breaking
  change to the trait shape.

## Exclusions/Non-goals

### REST-only exclusions

The SDK trait does not expose the following operations; consumers
must use the Usage Collector REST API:

- Platform liveness and readiness probes (handled by the ToolKit host above the gear boundary — the collector does not expose gear-local health endpoints).
- Operational telemetry; instruments are pushed via OTLP from
  ToolKit's global `SdkMeterProvider` (no in-gear HTTP metrics
  endpoint is provided).
- The REST-side error wire envelope (RFC-9457 `Problem`); SDK errors
  are domain-classified and the REST handler performs the conversion.
- CORS, TLS termination, and output encoding; these are platform API
  gateway responsibilities, not SDK or gear responsibilities.

Note: per ADR 0012, usage-type catalog operations (registration, read,
list, delete) are NOT REST-only — they are exposed on both the SDK
trait (Methods 6–9) and the REST API and converge on the same
gateway-side `UsageTypeCatalogService` over the plugin-owned
`usage_type_catalog`. The gateway owns PDP authorization, `gts_id`
base-derivation validation (against the reserved abstract base
`gts.cf.core.uc.usage_record.v1~` per ADR 0012's 2026-06-08 amendment),
closed-enum `kind` validation at the serde deserialize boundary, and
closed-shape `metadata_fields` validation; the plugin owns durable
storage and the `usage_records.gts_id` `ON DELETE RESTRICT`
foreign-key enforcement. Prior drafts of this section listed those
operations as REST-only; that exclusion has been lifted.

### Plugin SPI exclusions

The following are Plugin SPI responsibilities and must not appear on
the SDK trait:

- Durable persistence of `usage_records` AND of `usage_type_catalog`
  rows (per ADR 0012 — the catalog lives on the plugin alongside
  `usage_records` so the `usage_records.gts_id` `ON DELETE RESTRICT`
  FK can be enforced natively in a single backend transaction).
- Cursor token generation and validation.
- Dedup-on-conflict enforcement on
  `(tenant_id, gts_id, idempotency_key)`.
- Atomic `Active -> Inactive` transition at the storage boundary.
- Aggregation pushdown (the plugin executes aggregation server-side;
  the SDK trait emits the query, the plugin produces the result).
- Plugin-side per-method timeouts and `flush` for graceful shutdown.
- Plugin-host bulkheading, pooling, and circuit-breakers against the
  active plugin.
- Plugin SPI logical-table schema versioning.

### Gear non-goals reaffirmed on the SDK trait

- A dedicated backfill capability (watermarks, late-data coordination,
  or a bulk-import method beyond the existing batched `record_usage`
  path) is an explicit non-goal in v1. Old event timestamps are
  accepted without wall-clock validation, so bulk historical import
  rides the normal batched-ingestion path with each record's true
  event timestamp; see the timestamp / late-arrival invariant in
  `domain-model.md` §2.1 for the consequences for raw-tail consumers.
- Individual record amendment is intentionally omitted; the only
  post-acceptance mutation is the one-way `Active -> Inactive` status
  transition.
- Rate limiting, watermarks for high-cardinality bursts, and
  low-watermark coordination for late-arriving usage are caller- and
  operator-tuned at the gateway and calling-gear layers and are not
  surfaced on the SDK trait.
- Multi-region deployment is not a v1 capability of the gear.
- Gear-emitted audit events for operator-write paths are deferred;
  the v1 access trail is composed at the gateway and PDP decision
  points.
- Pricing, rating, billing, invoice generation, and quota decisions
  are out of scope.
- Gear-owned compliance scope is not claimed; concrete control
  mapping is platform-compliance-owned.
- The Usage Collector exposes REST, SDK, and Plugin SPI surfaces
  only; there is no end-user UI and no business-event publish or
  subscribe bus.
- Gear-side caching of PDP decisions is forbidden.
- At-rest encryption, key management, masking, disposal, backup,
  point-in-time recovery, disaster recovery, replication, tiering,
  retention windows, archival, compression, encoding, and
  partitioning as gear-owned mechanisms are out of scope and
  plugin-owned.
- Dead-letter queue, poison-message handling, and compensation-saga
  patterns are out of scope; ingestion is synchronous and fail-closed.

## Traceability

### Trait identifier and consumer contract

- `cpt-cf-usage-collector-interface-sdk-client` — the public SDK trait
  interface identifier carried by `UsageCollectorClientV1`. Source:
  phase-01 §"SDK trait surface (DESIGN section 3.3)"; phase-02 §"Public
  SDK Surface".
- `cpt-cf-usage-collector-contract-downstream-usage-reader` — the
  consumer contract referenced by the SDK trait. Source: phase-01
  §"SDK trait surface (DESIGN section 3.3)"; phase-02 §"Traceability
  Anchors" / "SDK trait surface and identifiers".

### Capabilities exposed by the SDK trait

- Create single usage record and create batched usage records:
  `cpt-cf-usage-collector-fr-ingestion`,
  `cpt-cf-usage-collector-fr-idempotency`,
  `cpt-cf-usage-collector-fr-usage-compensation`,
  `cpt-cf-usage-collector-adr-usage-compensation`,
  `cpt-cf-usage-collector-seq-emit-usage`. Throughput:
  `cpt-cf-usage-collector-nfr-throughput`. Ingestion
  latency: `cpt-cf-usage-collector-nfr-ingestion-latency`.
  Compensation rides this surface (no dedicated `compensate` method)
  per the §"No dedicated `compensate` method" note above. Sources:
  phase-01 §"SDK trait surface"; phase-02 §"Traceability Anchors" /
  "Capabilities exposed by the SDK trait" and §"SDK Method Inputs And
  Outputs" / "Ingestion" latency bullet; Phase 6 handoff
  §"Compensation Flow Text".
- Aggregated query: `cpt-cf-usage-collector-fr-query-aggregation`,
  `cpt-cf-usage-collector-seq-query-aggregated`,
  `cpt-cf-usage-collector-nfr-query-latency`. Sources: phase-01 §"SDK
  trait surface"; phase-02 §"Traceability Anchors" / "Capabilities
  exposed by the SDK trait".
- Raw cursor-paginated query: `cpt-cf-usage-collector-fr-query-raw`,
  `cpt-cf-usage-collector-seq-query-raw`. Source: same as aggregated
  query above.
- Deactivate usage event:
  `cpt-cf-usage-collector-fr-event-deactivation`,
  `cpt-cf-usage-collector-seq-deactivate-event`,
  `cpt-cf-usage-collector-adr-monotonic-deactivation`,
  `cpt-cf-usage-collector-principle-monotonic-deactivation`,
  `cpt-cf-usage-collector-flow-event-deactivation-cascade`,
  `cpt-cf-usage-collector-algo-event-deactivation-atomic-cascade-flip`,
  `cpt-cf-usage-collector-algo-event-deactivation-concurrency-guard`.
  Sources: phase-01 §"UsageRecordStatus", §"Event deactivation";
  phase-02 §"Traceability Anchors"; Phase 5 handoff §"Finalized cascade
  algorithm text" and §"Cascade response shape".
- UsageType catalog (register / read / list / delete — Methods 6–9):
  `cpt-cf-usage-collector-fr-usage-type-registration`,
  `cpt-cf-usage-collector-fr-usage-type-deletion`,
  `cpt-cf-usage-collector-fr-usage-type-existence-and-semantics`,
  `cpt-cf-usage-collector-seq-register-usage-type`,
  `cpt-cf-usage-collector-seq-delete-usage-type`,
  `cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference`
  (ADR 0012 — every usage type is a GTS type carrying a closed
  `metadata_fields: Vec<String>` declared-key list with `String`-typed
  values, and a closed `kind: UsageKind` enum on the catalog row
  carrying the counter / gauge classification per ADR 0012's
  2026-06-08 amendment; the `gts_id` derives from the reserved
  abstract base `gts.cf.core.uc.usage_record.v1~` and does NOT encode
  kind. Counter / gauge predicates read `self.kind` via
  `UsageType::is_counter()` / `UsageType::is_gauge()`).
  Referential integrity on delete is enforced by the plugin's
  `usage_records.gts_id → usage_type_catalog.gts_id` `ON DELETE RESTRICT`
  FK and surfaces to the SDK as
  `UsageTypeReferenced { gts_id, sample_ref_count }`; declared
  metadata is gateway-validated at L1 per ADR 0012 (closed shape,
  keyed by `gts_id`) with undeclared keys surfaced as
  `UnknownMetadataKey { gts_id, key }`. A `gts_id` that does not
  derive from the reserved abstract base is rejected at the
  `UsageTypeGtsId::new` boundary on registration (SDK:
  `UsageCollectorError::Validation`; REST: handler synthesises the
  canonical `invalid_base_gts_id` `Problem` envelope at HTTP `400`
  from the failed conversion on `CreateUsageTypeRequest::gts_id`);
  unknown `kind` values are rejected by closed-enum serde rejection
  at the deserialize boundary. The SDK exposes no dedicated
  invalid-base or invalid-kind error variant.

> **Removed in ADR 0012.** Prior drafts cited ADR-0007
> (gateway-local-from-config catalog), ADR-0009 (catalog-plugin
> referential integrity), and ADR-0010 (GTS-typed usage-type metadata
> with inheritance, indexability flag, and `abstract` flag). All
> three are superseded in full by ADR 0012, which unifies the
> catalog on the plugin-DB and removes inheritance, indexability
> flags, and the abstract flag from the trait surface. ADR 0012's
> 2026-06-02 amendment (as further amended 2026-06-08) further
> removes the open-but-typed JSON Schema surface (replaced by closed
> `metadata_fields: Vec<String>` with all values typed as `String`)
> and the per-usage-type trait map carrying the semantics classifier
> (replaced by a closed `kind: UsageKind` enum stored on the catalog
> row, read via `UsageType::is_counter()` / `UsageType::is_gauge()`).

### Domain entities

- `UsageRecord`,
  `ResourceRef`,
  `SubjectRef`,
  `UsageType`,
  `IdempotencyKey`,
  `RecordMetadata`,
  `UsageRecordStatus`,
  `AggregationQuery`,
  `RawQuery`,
  `AggregationResult`. Source: phase-01
  §"Domain Entities", §"Query Models And Views", §"Authorization And
  Tenancy Facts"; `out/phase-01-domain-contracts.md` §2. Raw-page
  output is the canonical `toolkit_odata::Page<UsageRecord>`
  envelope; cursor lifecycle is realized by
  `toolkit_odata::CursorV1` plus `validate_cursor_against`. The
  former gear-owned page and cursor-token entities defined in
  earlier drafts of `domain-model.md` are no longer carried on the
  SDK surface.

### Authorization, fail-closed, and attribution anchors

- `cpt-cf-usage-collector-contract-authz-resolver`,
  `cpt-cf-usage-collector-principle-fail-closed`,
  `cpt-cf-usage-collector-principle-pdp-centric-authorization`,
  `cpt-cf-usage-collector-adr-pdp-centric-authorization`,
  `cpt-cf-usage-collector-adr-caller-supplied-attribution`,
  `cpt-cf-usage-collector-adr-mandatory-idempotency`,
  `cpt-cf-usage-collector-constraint-pii-identity-layer`,
  `cpt-cf-usage-collector-constraint-no-business-logic`. Source:
  phase-02 §"Traceability Anchors" / "Security and authorization
  anchors"; phase-01 §"Authorization And Tenancy Facts".

### Plugin SPI and persistence anchors (exclusions)

- `cpt-cf-usage-collector-interface-plugin`,
  `cpt-cf-usage-collector-contract-storage-plugin`,
  `cpt-cf-usage-collector-component-plugin-host`,
  `cpt-cf-usage-collector-adr-pluggable-storage`,
  `cpt-cf-usage-collector-principle-pluggable-storage`,
  `cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference`
  (ADR 0012 — unifies the usage-type catalog on the plugin-DB and sets
  `gts_id` as the catalog PK / FK on `usage_records`; the gateway
  dispatches `get_usage_type` against the plugin SPI per call and
  reads counter / gauge semantics from the catalog row's `kind` field via
  `UsageType::is_counter()` / `UsageType::is_gauge()` — plugins do NOT
  re-implement metadata validation),
  `cpt-cf-usage-collector-interface-plugin` (the SPI surface against which
  the usage-type catalog and the usage records store are persisted).
  Source: phase-02 §"Traceability Anchors" / "Plugin SPI / persistence
  anchors"; ADR 0012.

### Versioning, stability, and quality NFR anchors

- `cpt-cf-usage-collector-adr-contract-stability`,
  `cpt-cf-usage-collector-principle-contract-stability`,
  `cpt-cf-usage-collector-nfr-plugin-contract-stability`,
  `cpt-cf-usage-collector-nfr-ingestion-latency`,
  `cpt-cf-usage-collector-nfr-query-latency`,
  `cpt-cf-usage-collector-nfr-throughput`,
  `cpt-cf-usage-collector-nfr-throughput-profile`,
  `cpt-cf-usage-collector-nfr-workload-isolation`. Source:
  phase-02 §"Traceability Anchors" / "Versioning, stability, and
  contracts" and / "NFRs that shape the SDK surface".

### Components allocated to the SDK trait

- `cpt-cf-usage-collector-component-ingestion-gateway`,
  `cpt-cf-usage-collector-component-query-gateway`,
  `cpt-cf-usage-collector-component-deactivation-handler`,
  `cpt-cf-usage-collector-component-usage-type-catalog`. Each component
  performs PDP enforcement inline via the shared `authz_scope` helper
  inside the trait implementation. Source: phase-01 §"Component
  allocation relevant to SDK trait (DESIGN section 3.2)".

## Open Questions

These are residual choices that the SDK crate may finalize during
implementation. None block this reference; each notes the conservative
default this reference adopts.

- OQ-1 (phase-02 OQ-1, gap G-9): Whether the SDK trait surfaces
  `PdpConstraint` values to callers on query responses so callers can
  see which scope was applied. This reference keeps `PdpDecision` and
  `PdpConstraint` internal to the gear and does not surface them on
  the trait; the SDK trait conveys only the post-authorization outcome
  through `Ok` results or the `Authorization` error variant. The SDK
  crate may add a non-required diagnostic field on query result types
  in a future minor version without breaking compatibility.
- OQ-2 (phase-02 OQ-2, gap G-10): Whether `IdempotencyKey` is a
  domain newtype in `models.rs` or a plain string accepted at the
  trait boundary. This reference treats `IdempotencyKey` as a domain
  newtype in `models.rs`, consistent with the ToolKit `models.rs`
  template and the invariant that idempotency keys are required and
  opaque.
- OQ-3 (phase-02 OQ-3, gap G-4): Whether the create methods return a
  dedicated acknowledgement envelope (id + dedup indicator) or the
  persisted record itself. This reference adopts the latter: every
  successful create returns the persisted `UsageRecord` on the `Ok`
  arm, with exact-equality idempotency retries silently returning the
  previously persisted record (silent absorb) and canonical-field
  mismatches surfacing as `IdempotencyConflict`. There is no separate
  `UsageRecordAck` envelope and no `DedupOutcome` indicator on the
  return shape.
- OQ-4 (gaps G-7 and G-11): Whether the SDK trait enforces numeric
  caps for raw `page_size`, aggregation result row count, group-by
  dimension count, and time-range window length at the trait
  boundary, and the inclusivity semantics of `time_range`. This
  reference defers numeric caps to the PRD NFR and the REST/OpenAPI
  contract; the SDK trait validates structural shape only (positive
  `page_size`, bounded `time_range`). The SDK rustdoc documents the
  conservative inclusive-start, exclusive-end convention for
  `time_range`, aligned with the REST wire semantics, without making
  it a breaking change surface.
- OQ-5 (gap G-8): Whether `correlation_id` on `SecurityContext` is
  caller-supplied or runtime-provided on SDK calls. This reference
  treats `correlation_id` as a field of the resolved `SecurityContext`
  provided by the ToolKit gateway upstream of the collector (REST) or
  by the in-process caller (SDK) and propagated by the caller; the
  SDK trait itself does not synthesize correlation IDs.
- OQ-6 (gap G-13): How the SDK trait surfaces its per-call timeout
  declaration. This reference adopts trait-level constant timeout
  values documented in the SDK rustdoc (one per method, bounded by
  the per-operation latency budgets). The SDK crate may evolve this
  to a configuration value in a future minor version without breaking
  the trait shape.

