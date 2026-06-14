# Usage Collector - Domain Model

<!-- toc -->

- [1. Modeling Conventions](#1-modeling-conventions)
- [2. Core Entities](#2-core-entities)
  - [2.1 UsageRecord](#21-usagerecord)
  - [2.2 ResourceRef](#22-resourceref)
  - [2.3 SubjectRef](#23-subjectref)
  - [2.4 UsageType](#24-usagetype)
  - [2.5 IdempotencyKey](#25-idempotencykey)
  - [2.6 RecordMetadata](#26-recordmetadata)
  - [2.7 SecurityContext](#27-securitycontext)
  - [2.8 UsageRecordStatus](#28-usagerecordstatus)
  - [2.9 UsageRecordFilterField](#29-usagerecordfilterfield)
  - [2.10 Keyset](#210-keyset)
- [3. Query Domain](#3-query-domain)
  - [3.1 AggregationQuery](#31-aggregationquery)
  - [3.2 RawQuery](#32-rawquery)
  - [3.3 AggregationResult](#33-aggregationresult)
- [4. Authorization Domain](#4-authorization-domain)
  - [4.1 PdpDecision](#41-pdpdecision)
  - [4.2 PdpConstraint](#42-pdpconstraint)
- [5. Plugin Binding Domain](#5-plugin-binding-domain)
  - [5.1 PluginBinding](#51-pluginbinding)
- [6. Surface Mapping](#6-surface-mapping)
  - [6.1 Error Envelope](#61-error-envelope)
- [7. Cross-Entity Invariants](#7-cross-entity-invariants)

<!-- /toc -->

This companion document defines the field-level domain model referenced by
`DESIGN.md` section 3.1. It is the shared data dictionary for the Usage
Collector core, SDK trait, REST wire contract, and Plugin SPI. The dedicated
`sdk-trait.md`, `plugin-spi.md`, and `usage-collector-v1.yaml` artifacts
specify each surface's operation set, types, and wire schemas on top of the
domain semantics captured here; this document remains the single source of
truth for entity field semantics shared across those surfaces.

This document is not executable DDL, an ORM mapping, or a complete OpenAPI
schema. Physical storage layout, backend-specific indexes, retention, and
query acceleration remain plugin-owned. REST endpoint paths and wire envelope
details remain owned by the OpenAPI contract `usage-collector-v1.yaml` (sibling to DESIGN.md).

## 1. Modeling Conventions

Field names use the canonical snake_case names from the SPI persist
surface (`plugin-spi.md`). Rust implementations may wrap these fields in
newtype or enum types, but must preserve the domain semantics documented
here.

Identifiers such as `tenant_id`, `resource_id`, `subject_id`, and `gts_id`
are opaque platform identifiers. The Usage Collector stores and compares them
but does not parse, classify, or derive identity information from them.

All timestamps are UTC instants. The REST representation is RFC 3339 / ISO 8601
UTC text; Rust and plugin implementations may use native timestamp types at
their own boundaries.

Numeric usage values are measurement values, not money. Pricing, rating,
billing, invoice generation, and quota decisions are downstream concerns and
must not be added to these domain types.

Optional fields are omitted when absent. `SubjectRef` is present when
`subject_id` is present; `subject_type` is an optional qualifier because some
source systems do not maintain subject-type taxonomies.

## 2. Core Entities

### 2.1 UsageRecord

A `UsageRecord` is one accepted measurement of resource consumption attributed
to a tenant, resource, optional subject, and UsageType (resolved against the
usage-type catalog, managed via the Plugin SPI and persisted in the active
storage plugin's database; see ADR 0012 at
`./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md`).
It is immutable after acceptance except for the one-way `status` transition
from `active` to `inactive`.

| Field               | Required             | Type                     | Description                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| ------------------- | -------------------- | ------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `uuid`              | Yes                  | `Uuid`                   | Caller-supplied record UUID. Persisted verbatim; ingestion requests supply it directly.                                                                                                                                                                                                                                                                                                                                                       |
| `tenant_id`         | Yes                  | `Uuid`                   | Caller-supplied tenant attribution. PDP authorization decides whether the caller may emit or read this tenant scope.                                                                                                                                                                                                                                                                                                                          |
| `resource_ref`      | Yes                  | `ResourceRef`            | Caller-supplied resource attribution. Both `resource_id` and `resource_type` are mandatory.                                                                                                                                                                                                                                                                                                                                                   |
| `subject_ref`       | No                   | `SubjectRef`             | Optional caller-supplied subject attribution. When present, `subject_id` is mandatory and `subject_type` is optional.                                                                                                                                                                                                                                                                                                                         |
| `gts_id`            | Yes                  | `UsageTypeGtsId`         | Reference to a `UsageType.gts_id` present in the usage-type catalog (managed via the Plugin SPI, persisted in the active storage plugin's database). The same `gts_id` string that identifies the usage type in the catalog is the value stored on every usage record that references it — no UUID derivation. Unknown UsageTypes are rejected before persistence. See ADR 0012 (`./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md`). |
| `value`             | Yes                  | `Decimal`                | Signed fixed-precision measurement value, carried as [`rust_decimal::Decimal`] on every surface (SDK, REST `UsageValue` schema, plugin SPI), wire-encoded as a JSON **string** (never a float), and persisted as Postgres `NUMERIC`. Floating-point representation is intentionally excluded: `SUM(value)` and counter-compensation netting MUST be bit-exact, which `f64` cannot guarantee at scale. Permitted sign depends jointly on the UsageType's `gts_id` prefix (counter vs gauge) and the presence of `corrects_id` per the four-cell validation matrix in this section. |
| `corrects_id`       | Optional             | `Uuid`                   | Sole structural discriminator between an ordinary usage row and a counter-compensation row. When `corrects_id IS NULL`, the record is an ordinary usage row. When `corrects_id` is set, the record is a counter-compensation row that references the `UsageRecord.uuid` of the usage row being offset; the referenced row MUST itself have `corrects_id IS NULL` and share the full identity tuple `(tenant_id, gts_id, resource_ref, subject_ref)` (`subject_ref` presence is part of the identity). See the validation matrix below.    |
| `created_at`        | Yes                  | UTC timestamp            | Event timestamp supplied by the usage source (event time, not arrival time). Accepted without wall-clock validation; see the `created_at` / late-arrival invariant under §2.1 Invariants.                                                                                                                                                                                                                                                     |
| `idempotency_key`   | Yes                  | `IdempotencyKey`         | Caller-supplied key used to deduplicate retries within `(tenant_id, gts_id)`. A same-key collision is resolved by exact-equality of the caller-supplied canonical fields: an exact-equality retry is silently deduplicated, while any differing canonical field is a Conflict (rejected, not absorbed).                                                                                                                                       |
| `status`            | Yes                  | `UsageRecordStatus`      | `active` on acceptance; may transition once to `inactive`. No reactivation exists.                                                                                                                                                                                                                                                                                                                                                            |
| `metadata`          | No                   | `RecordMetadata`         | Optional opaque JSON object persisted and returned verbatim.                                                                                                                                                                                                                                                                                                                                                                                  |

The permitted sign of `value` is jointly governed by the UsageType's
`gts_id` prefix (counter vs gauge) and the presence of
`corrects_id` per the following four-cell validation matrix, enforced
before persistence:

|             | `corrects_id IS NULL`   | `corrects_id SET`                        |
| ----------- | ----------------------- | ---------------------------------------- |
| **counter** | `value >= 0` (accepted) | `value < 0` (accepted)                   |
| **gauge**   | any `value` (accepted)  | REJECTED (`gauge_compensation_rejected`) |

Throughout this document, "counter" and "gauge" denote the two variants of
the closed `UsageKind` enum (`UsageKind::Counter`, `UsageKind::Gauge`)
carried on `UsageType.kind`. They are independent of `gts_id`, which
derives from the reserved abstract base
`gts.cf.core.uc.usage_record.v1~`.

Invariants:

- `uuid`, `tenant_id`, `resource_ref`, `gts_id`, `value`, `created_at`,
  `idempotency_key`, and `status` are never null on an accepted record.
- `created_at` is event time, not ingestion time, and is not validated against
  wall-clock: any UTC instant (past or future) is accepted, so late-arriving
  and historical records are ingested at their event-time position in the
  `(created_at, uuid)` sort order. Aggregation over a bounded `time_range`
  re-scans and stays complete regardless of arrival order; but a consumer
  tailing raw records by a forward `(created_at, uuid)` cursor may not observe a
  record that lands behind a position it already passed — incremental raw
  tailing is best-effort, not a lossless change feed. There is no dedicated
  backfill capability (no watermarks or late-data coordination); bulk
  historical import uses the normal batch ingestion path with each record's
  true event timestamp.
- `gts_id` must resolve to a UsageType in the usage-type catalog
  (managed via the Plugin SPI, persisted in the active storage plugin's
  database) before the record reaches the plugin; the record's
  `gts_id` value is the same `gts_id` string used as the catalog
  primary key (no UUID derivation; see ADR 0012).
- Deduplication is unique on `(tenant_id, gts_id, idempotency_key)`.
  The caller's gear identity is not part of that key; multiple emitting
  gears authorized for the same tenant and UsageType must coordinate key
  allocation. On a collision the outcome is decided by exact equality of the
  caller-supplied canonical fields (`value`, `created_at`, `resource_ref`,
  `subject_ref`, `metadata`; the match-key tuple and the server-owned
  `status` are excluded): an exact-equality retry is silently deduplicated,
  and any differing canonical field — including a metadata-only difference —
  is a Conflict that is rejected, not absorbed.
- The idempotency window is unbounded: the key never expires, has no TTL, and
  is never intentionally reusable, so the `(tenant_id, gts_id,
idempotency_key)` uniqueness is permanent. The active plugin must preserve
  that key tuple permanently even when record bodies are purged or archived by
  retention — a retention purge must not free a dedup key.
- Accepted records are immutable except for the `status` transition performed
  by the deactivation path. `corrects_id` is set at acceptance and never
  mutated thereafter.
- Deactivation does not mutate `tenant_id`, `resource_ref`, `subject_ref`,
  `gts_id`, `value`, `created_at`, `idempotency_key`,
  `corrects_id`, or `metadata`.
- A **usage row** is a `UsageRecord` with `corrects_id IS NULL`; a
  **compensation row** is a `UsageRecord` with `corrects_id` set. Presence
  of `corrects_id` is the sole structural discriminator between the two.
- When `corrects_id` is set, it MUST reference an existing `UsageRecord`
  whose `corrects_id IS NULL` (the referenced row MUST itself be a usage
  row, never another compensation), whose full identity tuple
  `(tenant_id, gts_id, resource_ref, subject_ref)` matches this
  record's identity tuple — `subject_ref` presence is part of the identity,
  so `None` vs `Some(_)` is a scope mismatch — and whose `status = active`.
  These constraints are enforced at ingestion (L1) and surface the canonical
  error variants `corrects_id_not_found`,
  `corrects_id_targets_compensation`, `corrects_id_wrong_scope`, and
  `corrects_id_inactive`. No per-record remaining-amount tracking is
  introduced.
- The L1 rejection of `corrects_id` targeting a row with `corrects_id IS
NOT NULL` makes the compensation graph strictly depth-1: a compensation
  row cannot itself be compensated.
- A compensation referencing a row that is concurrently deactivating is
  rejected by the L1 "referenced record must be active" check.
- The four-cell validation matrix governs the permitted sign of `value`:
  a counter UsageType (`UsageType.kind == Counter` on the referenced
  catalog row) with `corrects_id IS NULL` requires `value >= 0`;
  counter with `corrects_id` set requires `value < 0`; a gauge UsageType
  (`UsageType.kind == Gauge`) with `corrects_id IS NULL` accepts any
  signed value; gauge with `corrects_id` set is rejected before
  persistence with `gauge_compensation_rejected` (gauges natively express
  down-movement by emitting a smaller point-in-time reading, so a
  compensation row on a gauge UsageType is meaningless and disallowed).

### 2.2 ResourceRef

`ResourceRef` identifies the resource instance to which usage is attributed.
The composite is mandatory on every usage record and can be used to narrow
authorized read queries.

| Field           | Required | Type                       | Description                                                                             |
| --------------- | -------- | -------------------------- | --------------------------------------------------------------------------------------- |
| `resource_id`   | Yes      | Opaque resource identifier | Resource instance identifier inside the attributed tenant scope.                        |
| `resource_type` | Yes      | Opaque resource type       | Type discriminator such as `compute.vm` or another platform-owned resource type string. |
|                 |          |                            |                                                                                         |

Invariants:

- `resource_id` and `resource_type` must be supplied together.
- The Usage Collector validates only presence and structural shape; ownership
  and caller permission are PDP decisions.

### 2.3 SubjectRef

`SubjectRef` optionally identifies the user, service account, or other principal
to which usage is attributed. It is caller-supplied and never derived from the
caller `SecurityContext`.

| Field          | Required    | Type                      | Description                                                                                                            |
| -------------- | ----------- | ------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| `subject_id`   | Conditional | Opaque subject identifier | Internal platform identifier issued and governed by the identity layer. Required when subject attribution is supplied. |
| `subject_type` | No          | Opaque subject type       | Optional type discriminator for systems that maintain subject-type taxonomies.                                         |

Invariants:

- `subject_id` defines whether subject attribution is present.
- `subject_type` may be omitted when the source system has no meaningful subject
  type.
- `subject_type` must not be supplied without `subject_id`.
- When a subject is present, PDP authorization includes `subject_id` and includes
  `subject_type` only when supplied.
- When no subject is present, subject authorization is skipped; the system must
  not infer subject identity from the authenticated caller.
- Subject identifiers are opaque and are not PII within the Usage Collector
  boundary.

### 2.4 UsageType

`UsageType` is a platform-global definition of something the collector
measures. UsageTypes are not tenant-scoped and are managed by platform
operators via the SDK trait method
`UsageCollectorClient::create_usage_type` or the REST endpoint
`POST /usage-collector/v1/usage-types` — both ingress paths converge on a
single usage-type catalog, managed via the Plugin SPI and persisted in the
active storage plugin's database. See ADR 0012
(`./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md`).

Each UsageType is a **GTS Type Schema** — its identifier is a GTS _type_ id
whose last segment ends `~` per the GTS naming convention — not a GTS instance
id. UsageType types are flat for v1: there is no parent pointer on the catalog
row and no inheritance chain walked at validation time. Every registered
usage type is concrete and may receive usage rows on its declared shape.
UsageType definitions are owned by usage-collector (semantic ownership) and
physically stored on the active storage plugin's backend database alongside
`usage_records`, per ADR 0012, which supersedes the prior gateway-local
catalog (ADR-0007), the dual-catalog referential-integrity ADR (ADR-0009),
and the inheritance-based usage-type-metadata model (ADR-0010). The
gateway dispatches `get_usage_type` against the plugin SPI per call for
ingest-time validation.

UsageType **semantics** — counter or gauge — is carried by the closed
`UsageKind` enum stored on the catalog row's `kind` field per ADR 0012's
2026-06-08 amendment. `UsageType` exposes `.is_counter()` / `.is_gauge()`
predicates that read `self.kind`. `gts_id` derives from the reserved
abstract base `gts.cf.core.uc.usage_record.v1~` and is independent of
kind; the two fields are validated separately at the SDK boundary.

| Field             | Required | Type             | Description                                                                                                                                                                                                                                                                                   |
| ----------------- | -------- | ---------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `gts_id`          | Yes      | `UsageTypeGtsId` | Deployment-unique UsageType **type** identifier; the last segment MUST end `~` (GTS type id) and the identifier MUST derive from the reserved abstract base `gts.cf.core.uc.usage_record.v1~` with at least one further `~`-separated segment. Catalog primary key; FK target on usage records. |
| `kind`            | Yes      | `UsageKind`      | Closed enum (`Counter` / `Gauge`) carrying the row's counter / gauge classification. Wire shape is lowercase (`"counter"` / `"gauge"`); unknown values are rejected by closed-enum serde rejection at the deserialize boundary. Independent of `gts_id`.                                       |
| `metadata_fields` | Yes      | array\<string\>  | set of allowed metadata keys; closed shape — record metadata keys MUST be a subset, all values typed as String                                                                                                                                                                                  |

The per-UsageType declared-key set is owned by the usage type's own
`metadata_fields` — there is no inheritance, no ancestor walk, and no
inheritable trait. The gateway extracts the keys named in
`metadata_fields` per record and hands them to the active storage plugin
for backend-side `group_by` and `$filter` (see §2.9 and §3). UsageType
semantics (counter vs gauge) is carried by the closed `UsageKind` enum
stored on the catalog row's `kind` field; it is independent of `gts_id`
and validated separately at the SDK boundary.

The usage-record reference to a usage type is the `gts_id` string itself. The
catalog primary key on the `usage_type_catalog` table is `gts_id`, and the FK
column on `usage_records` is `gts_id` under `ON DELETE RESTRICT`. No UUID is
derived from the type id; consumers and plugin authors join on the textual
`gts_id` directly.

UsageType semantics — counter or gauge — is carried by the closed
`UsageKind` enum on the catalog row's `kind` field per ADR 0012's
2026-06-08 amendment. Predicates read `self.kind`:

- counter ⇐ `kind == UsageKind::Counter`
- gauge ⇐ `kind == UsageKind::Gauge`

A counter UsageType means: callers submit non-negative gross usage
deltas as usage rows (`corrects_id IS NULL`, `value >= 0`). The cumulative
total per `(tenant_id, gts_id)` is the signed `SUM` over usage
rows together with compensation rows (`corrects_id` set, `value < 0`):
append-only compensation rows reduce that total. `SUM` is therefore not
monotonically increasing in the presence of compensation, and the plugin
MUST NOT impose monotonicity checks across rows of either kind.

A gauge UsageType means: callers submit point-in-time readings as
usage rows (`corrects_id IS NULL`) that may rise or fall. Values are
stored as-is without monotonicity checks or delta accumulation. Gauge
UsageTypes do not admit compensation rows: a gauge already expresses
down-movement directly, and any record with `corrects_id` set on a gauge
UsageType is rejected before persistence with
`gauge_compensation_rejected`.

`unit` is a **deferred open item**. Whether `unit` becomes a declared
key in `metadata_fields` (with a domain-conventional name) or is
introduced via a separate dedicated field on the catalog row is
intentionally left open; both options remain on the table and the choice
does not block the usage-type / dimensions decision. The UsageType entity
does NOT yet declare a `unit` field.

`UsageTypeGtsId` is a newtype wrapping the platform-primitive `gts::GtsID`
(re-exported by `libs/toolkit-gts`). Its `Deserialize` impl parses the input
string as a GTS type id (trailing `~`) and asserts the parsed value derives
from the reserved abstract base `USAGE_RECORD_BASE = "gts.cf.core.uc.usage_record.v1~"`
(exposed by the usage-collector SDK / contracts crate) with at least one
further `~`-separated derivation segment. The newtype is the validation
point on REST `Json<UsageType>` deserialization at
`POST /usage-collector/v1/usage-types`. The `kind` field is validated
independently as a closed `UsageKind` enum at the serde deserialize
boundary.

Invariants:

- `gts_id` is unique across the deployment, is a GTS _type_ id (ends `~`), and
  MUST derive from the reserved abstract base
  `gts.cf.core.uc.usage_record.v1~` with at least one further `~`-separated
  segment. Identifiers that do not satisfy that derivation (or are
  non-type identifiers) are rejected at the `UsageTypeGtsId::deserialize`
  boundary and surface on the REST path as a structured `400` `Problem`
  envelope (`context.reason="invalid_base_gts_id"`).
- UsageType semantics (counter / gauge) is carried by the closed
  `UsageKind` enum on the catalog row's `kind` field per ADR 0012's
  2026-06-08 amendment; registration validates `kind` as a closed enum
  at the serde deserialize boundary (unknown values rejected). `gts_id`
  and `kind` are independent — there is no "wrong kind for this gts_id"
  failure mode. Compensation is counter-only: a gauge UsageType paired
  with a record carrying `corrects_id` is rejected before persistence
  with `gauge_compensation_rejected`. Cumulative totals per
  `(tenant_id, gts_id)` are governed by the four-cell
  validation matrix in §2.1 and MAY be reduced by compensation rows.
- UsageType types are flat for v1: there is no parent pointer, no ancestor chain
  validation, and no implicit ancestor materialization. If inheritance is
  required by a future capability, it will be reintroduced by a dedicated ADR
  that names its consumer (see ADR 0012).
- The catalog primary key is `gts_id`. The FK column on `usage_records` is
  `gts_id` under `ON DELETE RESTRICT`. No UUID is derived from the type id.
- Every registered usage type is concrete and may receive usage rows on its
  declared shape; there is no abstract / non-abstract distinction on the
  catalog row.
- `metadata_fields` declares a closed list of allowed metadata keys per
  usage type. Only declared keys are accepted at ingest; every value is typed
  as `String` end-to-end (see §2.6). There is no free-form remainder and no
  per-key JSON-Schema surface.
- Per-UsageType declared keys are owned by the usage type's own
  `metadata_fields`; they are not inheritable and indexing strategy is a
  plugin implementation concern. Every key in `metadata_fields` is queryable
  (declared = queryable); there is no separate indexable-trait gate on the
  UsageType specification surface per ADR 0012 (see §2.9).
- UsageType registration is PDP-authorized and rejects duplicate identifiers
  (`usage_type_already_exists`).
- UsageType deletion is PDP-authorized and is rejected by the storage plugin
  when the catalog row is still referenced by any `usage_records` row, via
  the `ON DELETE RESTRICT` FK established on `gts_id`. The plugin returns a
  structured `usage_type_referenced` error that the gateway surfaces as a
  deterministic REST / SDK error response.
- The collector does not store caller-to-UsageType authorization mappings;
  the PDP authorizes the supplied tenant/resource/subject against the
  caller's `SecurityContext`-derived gear identity, and those policies
  belong to the PDP.

### 2.5 IdempotencyKey

`IdempotencyKey` is a caller-supplied opaque string required on every usage
record.

Invariants:

- Keyless records are rejected before persistence.
- A same-key submission with the same `(tenant_id, gts_id,
idempotency_key)` is resolved by exact equality of the caller-supplied
  canonical fields (`value`, `created_at`, `resource_ref`, `subject_ref`,
  `metadata`; the match-key tuple and the server-owned `id` and `status`
  are excluded). An exact-equality retry is silently deduplicated; any
  differing canonical field — including a metadata-only difference — is a
  Conflict that is rejected fail-closed (surfaced on the wire as the
  `idempotency_conflict` reason), never silently dropped.
- The idempotency window is unbounded: the key never expires, has no TTL, and
  is never intentionally reusable. The active plugin must preserve the
  `(tenant_id, gts_id, idempotency_key)` tuple permanently even
  when record bodies are purged or archived by retention; a retention purge
  must not free a dedup key.
- The same key may legitimately appear under a different tenant or UsageType.
- Emitting callers sharing the same tenant and UsageType namespace must
  coordinate key prefixes or another allocation convention.

### 2.6 RecordMetadata

`RecordMetadata` is optional per-record context supplied by the usage
source. It is validated against the referenced usage type's
`metadata_fields` (see ADR 0012 at
`./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md`) on a **closed
shape**: every key MUST appear in the usage type's declared `metadata_fields`,
every value is typed as String (or string-coercible per the SDK / wire
spec), and undeclared keys are rejected.

The gateway resolves `metadata_fields` per record via a `get_usage_type`
SPI dispatch against the plugin-side usage-type catalog. Plugins do not
own this membership check; they store the raw `metadata_fields` array only.

| Property             | Value                                                                                                                                                                                                                                                    |
| -------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Shape                | Key/value map; every key is a String drawn from the usage type's `metadata_fields`; every value is a String (or string-coercible per spec).                                                                                                              |
| Default maximum size | 8 KiB per record unless operator configuration overrides it                                                                                                                                                                                              |
| Validation           | **Closed shape**: every key MUST be a member of the usage type's `metadata_fields`; every value MUST be a String (or string-coercible per spec); any key not in `metadata_fields` is rejected before persistence with error name `unknown_metadata_key`. |
| Queryable keys       | Every key in the usage type's `metadata_fields` is queryable (see §2.9 and §3). The gateway extracts these per record for backend-side `group_by` and `$filter`. There is no separate indexable-trait gate; declared = queryable.                        |
| Interpretation       | Not interpreted, aggregated, classified, or transformed beyond closed-shape membership validation and declared-key extraction. Downstream consumers own any further interpretation.                                                                      |
| Query behavior       | Persisted and returned verbatim with raw records. Only declared keys exist on a record; there are no preserved "extras".                                                                                                                                 |

Invariants:

- Validation failures against the usage type's `metadata_fields` are rejected
  before persistence with an actionable validation error. An undeclared
  metadata key is rejected with error name `unknown_metadata_key`. A
  non-string-coercible value on a declared key is likewise rejected.
- Oversized metadata is rejected with an actionable validation error.
- Undeclared keys are never silently accepted, never preserved as extras, and
  never reach the storage plugin — the surface is closed.
- Declared keys extracted from `RecordMetadata` are the metadata values the
  plugin uses for query acceleration; any subsequent change to
  `metadata_fields` on a usage type is observed by the next `get_usage_type`
  SPI dispatch (catalog reads round-trip the storage plugin per call).
  Indexing strategy on those keys is a plugin implementation concern (see
  ADR 0012 at `./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md`).
- Metadata must not be used to add pricing, billing, quota, or authorization
  logic inside the collector.

### 2.7 SecurityContext

`SecurityContext` is the platform-authenticated caller context. The Usage
Collector receives it from the ToolKit gateway (REST) or directly from the
caller (in-process SDK trait) and consumes it for per-operation PDP
authorization and correlation propagation, but does not own its schema or
persist it on usage records.

| Field                 | Required               | Type                       | Description                                                                                                             |
| --------------------- | ---------------------- | -------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| `principal`           | Yes                    | Opaque principal reference | Authenticated caller identity as provided by the platform identity layer.                                               |
| `tenant_scope_claims` | Platform-owned         | Opaque claims              | Tenant-scope claims available to PDP evaluation. The collector does not infer authorization from these claims directly. |
| `auxiliary_claims`    | Platform-owned         | Opaque claims              | Additional platform claims passed through to PDP evaluation when available.                                             |
| `correlation_id`      | Yes for API operations | Opaque request identifier  | Identifier propagated through gateway, PDP decision logs, gear logs, and platform audit trail.                        |

Invariants:

- Requests without a resolved `SecurityContext` fail closed.
- The collector never synthesizes identities and never falls back to anonymous
  access.
- `SecurityContext` is input to authorization, not a source for implicit tenant,
  resource, or subject attribution.

### 2.8 UsageRecordStatus

`UsageRecordStatus` records the lifecycle state of an accepted
`UsageRecord` regardless of whether the row is a usage row
(`corrects_id IS NULL`) or a compensation row (`corrects_id` set).

| Value      | Meaning                                                                                                                                                                                               |
| ---------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `active`   | Default state for newly accepted records (both usage rows with `corrects_id IS NULL` and compensation rows with `corrects_id` set).                                                                   |
| `inactive` | Record was deactivated by an authorized operator. The record remains queryable and distinguishable from active records and is excluded from authoritative aggregation and event-style query surfaces. |

Invariants:

- The only transition is `active -> inactive`. Deactivation is one-way and
  is atomic at the plugin boundary.
- Deactivation applies uniformly to any `UsageRecord`: a usage row and a
  compensation row are individually deactivatable.
- **Depth-1 cascade**: deactivating an active usage row (a `UsageRecord`
  with `corrects_id IS NULL`) atomically flips every currently-active
  compensation row whose `corrects_id` equals the deactivated row's id
  from `active` to `inactive` as part of the same plugin-side operation.
  Deactivating a compensation row never cascades — by the L1 referential
  rule, no row may carry `corrects_id` targeting a row whose
  `corrects_id IS NOT NULL`, so the compensation graph is strictly
  depth-1 and compensations are not themselves compensable by
  construction.
- A second deactivation request for an already-inactive record is rejected
  with an actionable error, regardless of whether the target is a usage
  row or a compensation row.

### 2.9 UsageRecordFilterField

`UsageRecordFilterField` is the domain-level description of every wire field
that may appear in an OData `$filter` expression on the raw-query surface (and
in `group_by` on the aggregation surface — see §3). It is **not** a fixed
static enum: it is a **derived shape** composed of a fixed-field core plus
every key in the queried usage type's `metadata_fields`, resolved per
request from the usage type's own catalog row (see §2.4 and §2.6) via a
`get_usage_type` SPI dispatch against the storage plugin. See ADR 0012
(`./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md`) for the
usage-type catalog and reference-model decisions.

Field names on the wire match the canonical `UsageRecord` field names in §2.1
exactly; declared-key names are taken verbatim from the usage type's
`metadata_fields`. No alternative shorthand spellings are accepted.

**Fixed fields (always available, all UsageTypes):**

| Wire field name | Source                              | Allowed OData operators            |
| --------------- | ----------------------------------- | ---------------------------------- |
| `tenant_id`     | `tenant_id` on §2.1                 | `eq`, `in`                         |
| `resource_id`   | `ResourceRef.resource_id` §2.2      | `eq`, `in`                         |
| `resource_type` | `ResourceRef.resource_type` §2.2    | `eq`, `in`                         |
| `subject_id`    | `SubjectRef.subject_id` §2.3        | `eq`, `in`                         |
| `subject_type`  | `SubjectRef.subject_type` §2.3      | `eq`, `in`                         |
| `corrects_id`   | `corrects_id` on §2.1               | `eq`, `in`                         |
| `status`        | §2.8                                | `eq`, `ne`, `lt`, `le`, `gt`, `ge` |

The fixed-field core is identical across every UsageType and survives any
usage-type-catalog change.

**Per-UsageType declared keys (resolved per request):**

The set of additional fields available in `$filter` (and `group_by`) is
**every key in the queried usage type's `metadata_fields`** (see §2.4 and
§2.6). The set is resolved per request from the `gts_id` on the
query (REQUIRED on `RawQuery` and `AggregationQuery`, see §3): the gateway
reads the usage type's `metadata_fields` via a `get_usage_type` SPI dispatch
against the storage plugin and admits exactly those key names as filter
fields for that request. There is no separate indexable-trait gate —
declared = queryable.

Because declared-key values are typed as String (per §2.6 closed shape),
the natural operator set for per-UsageType declared keys is `eq` / `in`,
matching the other opaque-identifier fixed fields above. Wider operator
sets (range, ordering) on a declared key MAY be supported in a future
revision if a specific value family warrants them; v1 admits `eq` / `in`
only.

Invariants:

- The fixed-field opaque identifiers (`tenant_id`, `resource_id`,
  `resource_type`, `subject_id`, `subject_type`, `corrects_id`) accept
  only `eq` and `in`; ordering and range operators are rejected as a
  structural validation error.
- `status` accepts the full comparison operator set
  (`eq`, `ne`, `lt`, `le`, `gt`, `ge`). The time-window predicate
  `created_at` is carried by the dedicated `TimeWindow` parameter on
  the SDK / REST surfaces (not by OData).
- Per-UsageType declared keys accept `eq` and `in` only in v1; any other
  operator on a declared-key field is rejected as a structural validation
  error before plugin dispatch.
- The set of admissible filter fields is computed per request from the
  usage type's own `metadata_fields` for the usage type named in the query's
  `gts_id`. Requests using any field outside the union of (fixed
  fields, that usage type's declared keys) are rejected before plugin
  dispatch with an actionable error naming the offending field and the
  usage type.
- Every key in `metadata_fields` is filterable; there is no separate
  indexable-trait gate. Conversely, any name not in `metadata_fields` is
  rejected before plugin dispatch — there are no undeclared "extras"
  reaching the record per §2.6 closed-shape semantics.
- The derived shape is recomputed on every request via a `get_usage_type`
  SPI dispatch against the storage plugin, so a freshly-registered declared
  key is filterable on the very next request to that usage type.

**SDK realization.** The fixed-field core and the per-UsageType declared
keys travel through two distinct SDK parameters on
`list_usage_records`:

- The fixed-field core is realized in code by `UsageRecordQuery` (in
  `usage-collector-sdk/src/models.rs`), which carries
  `#[derive(ODataFilterable)]`. The macro generates
  `UsageRecordFilterField` and its `toolkit_odata::filter::FilterField`
  impl, and the SDK ships them via `&toolkit_odata::ODataQuery`. Wire field
  names match `UsageRecord` field names exactly, with nested attribution
  composites flattened to their leaf identifiers (`resource_id`,
  `resource_type`, `subject_id`, `subject_type`) — the macro names
  variants from Rust idents, so the wire spelling is the flat form rather
  than the nested `resource/resource_id` path documented above in the
  conceptual table. Plugin impl crates bind each variant to a storage
  column through a `FieldToColumn<UsageRecordFilterField>` mapper.
- Per-UsageType declared keys are realized by the typed `&[MetadataFilter]`
  side channel — the OData filter grammar in `toolkit-odata` does not
  express filtering on `serde_json::Value` map keys, and no production
  gear in the workspace extends it for that purpose. Each
  `MetadataFilter` carries a key name and a non-empty value set; AND
  across distinct entries, OR within `values()`. Admissibility
  (declared-key membership) is checked by the gateway against the
  resolved `metadata_fields`; rejections surface as a typed validation variant.

### 2.10 Keyset

`Keyset` is the typed last-row sort-key tuple consumed by the toolkit cursor
encoder when paginating raw queries.

| Component   | Type                     | Description                                                                             |
| ----------- | ------------------------ | --------------------------------------------------------------------------------------- |
| `created_at` | UTC timestamp            | Primary sort key; matches `UsageRecord.created_at` of the last row on the emitted page.  |
| `id`        | Opaque record identifier | Deterministic tiebreaker; matches `UsageRecord.uuid` of the last row on the emitted page. |

Invariants:

- `created_at` is the primary sort key; `id` is the deterministic tiebreaker.
  Together they MUST yield a total, stable order across all plugins so that
  `(created_at, uuid)` pairs are unique within a tenant's record stream.
- `Keyset` is produced from the last `UsageRecord` of an emitted page and is
  serialized by the toolkit gateway into the opaque `CursorV1` returned in
  `toolkit_odata::Page<UsageRecord>.page_info.next_cursor`.
- `Keyset` is never exposed to callers in raw form; consumers receive only the
  opaque `CursorV1` token and pass it back unmodified.

## 3. Query Domain

The query surface (raw and aggregation) is single-UsageType in v1: every
query names exactly one usage type via `gts_id`, which lets the
gateway resolve the usage type's full `metadata_fields` set (per §2.4 and
§2.6) into the request's admissible filter and grouping fields without
cross-UsageType reconciliation. Cross-UsageType aggregation is a non-goal of
the v1 query surface — it would require either a common-dimension projection
across heterogeneous declared-key sets or a degenerate fixed-field-only mode,
and neither is in scope for this revision. See ADR 0012
(`./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md`) for the
usage-type catalog and reference-model decisions.

The usage-type catalog backing this resolution is the single usage-type
catalog, managed via the Plugin SPI and persisted in the active storage
plugin's database alongside `usage_records`, per ADR 0012. The gateway
dispatches `get_usage_type` against the storage plugin SPI per request when
admitting filter fields and group-by dimensions.

### 3.1 AggregationQuery

`AggregationQuery` requests server-side aggregation over authorized usage
records. It is available through the SDK trait and REST API and is pushed down
to the Plugin SPI after PDP constraints are applied.

| Field               | Required | Type                     | Description                                                                                                                                                                 |
| ------------------- | -------- | ------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `time_range`        | Yes      | UTC start/end interval   | Mandatory bounded interval. The REST contract defines inclusive/exclusive wire semantics.                                                                                   |
| `gts_id` | Yes      | `UsageTypeGtsId`         | Exactly one UsageType. Requests with no UsageType or multiple UsageTypes are rejected. Used to resolve the usage type's `metadata_fields` set for `group_by` and `$filter`. |
| `aggregation`       | Yes      | Enum                     | One of `SUM`, `COUNT`, `MIN`, `MAX`, or `AVG`.                                                                                                                              |
| `tenant_id`         | No       | `Uuid`                   | User-supplied narrowing filter applied after PDP constraints.                                                                                                               |
| `resource_ref`      | No       | `ResourceRef`            | Optional resource narrowing filter (filterable on the flattened `resource_id` / `resource_type` leaves; see §2.9).                                                          |
| `subject_ref`       | No       | `SubjectRef`             | Optional subject narrowing filter (filterable on the flattened `subject_id` / `subject_type` leaves; see §2.9).                                                             |
| `group_by`          | No       | List of dimensions       | Any combination of the **fixed fields** in §2.9 plus every key in the queried usage type's **`metadata_fields`** (resolved per request from `gts_id`).                      |
| `$filter`           | No       | OData filter expression  | Restricted to the union of fixed fields (§2.9) and every key in the queried usage type's `metadata_fields`, with the per-field operator allowances in §2.9.                 |

Invariants:

- PDP constraints define the authorization boundary and are applied before
  user-supplied filters.
- User filters can only narrow the authorized scope.
- `group_by` and `$filter` admissibility is computed per request from the
  queried usage type's own `metadata_fields` via a `get_usage_type` SPI
  dispatch against the storage plugin; fields outside the union of (fixed
  fields, every key in that usage type's `metadata_fields`) are rejected
  before plugin dispatch with an actionable error naming the offending
  field and the usage type.
- Empty result sets inside the authorized scope are not errors.
- Aggregation result size and report-shape limits follow the PRD
  batch-and-report timing NFR and the OpenAPI contract `usage-collector-v1.yaml` (sibling to DESIGN.md).
- **Aggregation contract**: `SUM(value)` is computed over active rows of
  both kinds — usage rows (`corrects_id IS NULL`) and compensation rows
  (`corrects_id IS NOT NULL`) — and yields the signed net total per
  group, with compensation rows reducing it. `COUNT`, `MIN`, `MAX`, and
  `AVG` operate over active rows `WHERE corrects_id IS NULL`;
  compensation rows are excluded from these aggregations because
  compensation rows adjust `SUM` and are not events. Inactive records of
  either kind are excluded from all five aggregations.

**SDK realization.** `AggregationQuery` is not a single struct in
`usage-collector-sdk`; the conceptual fields are split across five typed
inputs to `query_aggregated_usage_records` (mirroring the
`list_usage_records` signature for the WHERE-clause portion):

- `gts_id` → `gts_id: UsageTypeGtsId` named parameter on the
  SDK signature (type-enforced; the `gts_id` field on the OData filter
  surface is reserved and any `gts_id`-touching predicate in `query`
  is rejected as a typed validation error — the typed parameter is the single
  source of truth).
- `time_range` → `window: TimeWindow` (validated `[from, to)` UTC, `from
  < to` enforced at construction).
- `tenant_id` / `resource_ref` / `subject_ref` narrowing
  filters → predicates on `query: &ODataQuery` over
  `UsageRecordFilterField` minus the reserved `gts_id` field (flattened
  to `tenant_id`, `resource_id`, `resource_type`, `subject_id`,
  `subject_type`).
- `$filter` over declared metadata keys → `metadata_filter:
  &[MetadataFilter]` (typed side channel; the OData filter grammar in
  `toolkit-odata` does not express `serde_json::Value` map-key filters).
- `aggregation` (the op) and `group_by` → bundled into `AggregationSpec`
  (an `AggregationOp` plus an ordered `Vec<AggregationDimension>`).

The result type is `AggregationResult { buckets: Vec<AggregationBucket> }`,
where each `AggregationBucket` carries `key: Vec<String>` (in
`group_by` order; empty for the no-grouping case) and
`value: Option<Decimal>` (wire-encoded as a JSON string; `None` when no
rows matched in the bucket; `AVG` may carry a plugin-chosen rounding
scale on non-terminating quotients). Each entry in `key` is the string
form of the corresponding `AggregationDimension`: `TenantId` is
emitted as `Uuid::to_string()` (lowercase, hyphenated); all other
dimensions are already strings on the record. The dimension's *kind* is
recoverable by position from the caller-supplied `group_by`.

### 3.2 RawQuery

`RawQuery` requests cursor-paginated raw usage records for **exactly one
UsageType**. It is available through the SDK trait and REST API and is
pushed down to the Plugin SPI after PDP constraints are applied.

| Field               | Required | Type                     | Description                                                                                                                                                                  |
| ------------------- | -------- | ------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `time_range`        | Yes      | UTC start/end interval   | Mandatory bounded interval.                                                                                                                                                  |
| `gts_id` | **Yes**  | `UsageTypeGtsId`         | Exactly one UsageType. Used to resolve the usage type's `metadata_fields` set for `$filter`. Requests with no UsageType or multiple UsageTypes are rejected before dispatch. |
| `tenant_id`         | No       | `Uuid`                   | Optional tenant narrowing filter.                                                                                                                                            |
| `resource_ref`      | No       | `ResourceRef`            | Optional resource narrowing filter (filterable on the flattened `resource_id` / `resource_type` leaves; see §2.9).                                                            |
| `subject_ref`       | No       | `SubjectRef`             | Optional subject narrowing filter (filterable on the flattened `subject_id` / `subject_type` leaves; see §2.9).                                                              |
| `$filter`           | No       | OData filter expression  | Restricted to the union of fixed fields (§2.9) and every key in the queried usage type's `metadata_fields`, with the per-field operator allowances in §2.9.                  |
| `cursor`            | No       | `toolkit_odata::CursorV1` | Toolkit-owned opaque continuation marker carried in a previous `toolkit_odata::Page<UsageRecord>`.                                                                             |
| `page_size`         | No       | Positive integer         | Requested page size, bounded by the REST/SDK contract.                                                                                                                       |

Invariants:

- `gts_id` is REQUIRED. Rationale: `RawQuery` is single-UsageType
  so the usage type's `metadata_fields` set is always resolvable per request,
  and the admissible filter-field set in §2.9 is well-defined.
  Cross-UsageType raw scans (across heterogeneous declared-key sets) are a
  non-goal of the v1 query surface and remain out of scope; a query intended
  to span multiple usage types is rejected. Query-time dimensions resolve to
  the usage type's full `metadata_fields` set.
- PDP constraints are intersected with user filters before plugin dispatch.
- `$filter` admissibility is computed per request from the queried usage
  type's own `metadata_fields` via a `get_usage_type` SPI dispatch against
  the storage plugin; fields outside the union of (fixed fields, every key
  in that usage type's `metadata_fields`) are rejected before plugin
  dispatch with an actionable error naming the offending field and the
  usage type.
- PDP denial, empty read constraints, or invalid cursor input fail closed with a
  deterministic error.
- No matching records inside the authorized scope returns an empty page, not an
  error.

### 3.3 AggregationResult

`AggregationResult` is the bucketed output of `query_aggregated_usage_records`.

| Field     | Required | Type            | Description                                                            |
| --------- | -------- | --------------- | ---------------------------------------------------------------------- |
| `buckets` | Yes      | List of buckets | Each bucket carries the dimension values and the aggregated quantity.  |

Bucket shape (`AggregationBucket`):

| Field   | Required | Type                                 | Description                                                                                                                                              |
| ------- | -------- | ------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `key`   | Yes      | `Vec<String>` (may be empty)         | One entry per dimension in `AggregationSpec::group_by`, in the same order. Empty when `group_by` was empty (the no-grouping case yields a single bucket).|
| `value` | Yes      | `Option<Decimal>`                    | Aggregated value for the bucket, wire-encoded as a JSON string. `None` when no rows matched (e.g. `MIN` over an empty set). The `op` originated from the call's `AggregationSpec`; `AVG` may carry a plugin-chosen rounding scale on non-terminating quotients. |

Each entry in `key` is the string form of the corresponding
`AggregationDimension`. `TenantId` MUST be emitted as `Uuid::to_string()`
(lowercase, hyphenated — e.g. `01234567-89ab-cdef-0123-456789abcdef`).
`ResourceId`, `ResourceType`, `SubjectId`, and `SubjectType` are emitted
verbatim from the record. `Metadata(<key>)` is the metadata value at
that key, which is already a `String` (or string-coercible) per the
`metadata_fields` closed-shape rule (§2.6). The dimension's *kind* is
recoverable by position from the caller-supplied `group_by`; the bucket
carries no per-element discriminator.

The aggregation function (`op`) and the queried usage type (`gts_id`)
are NOT carried on `AggregationResult` — they are inputs to the call
(via `AggregationSpec` and the typed `gts_id: UsageTypeGtsId` named
parameter on the SDK signature, respectively) and remain the caller's
responsibility to associate with the response.

Interpretation of `value`:

- When `aggregation = SUM`, `value` is the signed net total within the
  bucket across active rows of both kinds — usage rows
  (`corrects_id IS NULL`) and compensation rows
  (`corrects_id IS NOT NULL`) — with compensation rows reducing it.
- When `aggregation` is `COUNT`, `MIN`, `MAX`, or `AVG`, `value` is
  computed over active rows `WHERE corrects_id IS NULL` within the
  bucket; compensation rows are excluded from these aggregations because
  compensation rows adjust `SUM` and are not events.
- Inactive records of either kind are excluded from every aggregation.

## 4. Authorization Domain

### 4.1 PdpDecision

`PdpDecision` is the permit-or-deny result returned by `authz-resolver` for a
single operation.

| Field         | Required         | Type                    | Description                                                            |
| ------------- | ---------------- | ----------------------- | ---------------------------------------------------------------------- |
| `effect`      | Yes              | Enum                    | `permit` or `deny`.                                                    |
| `constraints` | Read permit only | List of `PdpConstraint` | Read-scope filters that define the authorization boundary.             |
| `reason`      | No               | Opaque reason/category  | Optional PDP-owned explanation used for diagnostics and error mapping. |

Invariants:

- Deny decisions reject the operation before any state change or plugin read.
- Read operations require permit plus non-empty authorized constraints.
- Write operations use the decision over the full attribution tuple and do not
  rely on cached or inferred authorization.

### 4.2 PdpConstraint

`PdpConstraint` is a server-side query filter returned by PDP with a permit
decision. It is applied before user-supplied filters.

| Field                | Required  | Type                        | Description                              |
| -------------------- | --------- | --------------------------- | ---------------------------------------- |
| `tenant_ids`         | PDP-owned | Set of tenant identifiers   | Authorized tenant scope for the read.    |
| `resource_refs`      | PDP-owned | Set of `ResourceRef` values | Optional authorized resource scope.      |
| `subject_refs`       | PDP-owned | Set of `SubjectRef` values  | Optional authorized subject scope.       |
| `gts_ids` | PDP-owned | Set of `UsageTypeGtsId`     | Optional authorized UsageType scope.     |

Invariants:

- Constraints are combined with user filters as an intersection.
- User filters never widen the scope returned by PDP.
- The collector does not cache constraints across requests.

## 5. Plugin Binding Domain

### 5.1 PluginBinding

`PluginBinding` is the in-process pair returned per call by the host
`Service`'s lazy resolution path: the `GtsInstanceId` cached on first use
by `GtsPluginSelector::get_or_init`, and the `Arc<dyn
UsageCollectorPluginV1>` looked up via `ClientHub::try_get_scoped` under
`ClientScope::gts_id(&instance_id)`. There is no separate "Gear
Orchestrator" component — the host gear's own `Service` constructor
materializes the selector, and the plugin gear's `init()` materializes
the scoped client. The SPI major version
is encoded structurally inside `gts_schema_id` (the trailing `.v1~`
segment of `UsageCollectorPluginSpecV1::gts_schema_id()`) and is not
materialized as a separate runtime field.

| Field             | Required | Type                              | Description                                                                                                                                                                                                   |
| ----------------- | -------- | --------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `gts_instance_id` | Yes      | `GtsInstanceId`                   | Scope used to resolve the selected storage plugin; cached by `GtsPluginSelector::get_or_init` on first dispatch and reused for the host `Service`'s lifetime. Encodes the SPI major version as a path suffix. |
| `client`          | Yes      | `Arc<dyn UsageCollectorPluginV1>` | The bound plugin's scoped trait object — registered by the plugin gear's `init()` via `ClientHub::register_scoped` under `ClientScope::gts_id(&instance_id)` and cloned out on each dispatch.               |

Invariants:

- The Usage Collector has exactly one active storage binding per configured GTS
  instance scope.
- Bootstrap fails when no binding can be resolved (no matching
  `PluginV1<UsageCollectorPluginSpecV1>` instance in `types-registry`, or
  `ClientHub::try_get_scoped` returns `None` after selector resolution —
  surfaced as `PluginUnavailable`).
- The collector does not invent a fallback binding or keep a parallel local
  persistence path.
- Plugin SPI compatibility follows the public major-version stability contract;
  the SPI version is encoded in `gts_schema_id` (e.g.
  `cf.core.credstore.plugin.v1~`). No runtime
  negotiation.
- Binding "state" is not modeled as a finite state machine (no
  `Unbound`/`Resolving`/`Bound`/`Refreshing`/`Failed` discriminants exist in
  the reference gears); it is recomputed on each call from the two
  structural facts above.

## 6. Surface Mapping

| Surface    | Consumes                                                                                                                  | Produces                                                                                                                                                                                                                                             |
| ---------- | ------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| SDK trait  | Usage submissions, `AggregationQuery`, `RawQuery`, deactivation requests                                                  | Per-record acknowledgements, `AggregationResult`, `toolkit_odata::Page<UsageRecord>`, deactivation outcome                                                                                                                                            |
| REST API   | Same as SDK plus UsageType registration, UsageType list/get/delete, and health probe requests                             | Same as SDK plus UsageType catalog state, health probe payloads, and platform-standard errors. Operational telemetry is pushed via OTLP from ToolKit's `SdkMeterProvider`; no in-gear HTTP metrics endpoint is exposed.                             |
| Plugin SPI | Idempotency-keyed `UsageRecord` persistence commands, query commands, UsageType lifecycle commands, deactivation commands | Persistence acknowledgements, dedup outcomes (silently deduplicated on an exact-equality retry, Conflict on a canonical-field mismatch), `AggregationResult`, `toolkit_odata::Page<UsageRecord>`, UsageType catalog results, classified plugin errors |

### 6.1 Error Envelope

`UsageCollectorError` is the public error envelope across all SDK surface
methods. It is declared in `usage-collector-sdk/src/error.rs` (the SDK
crate, not the host) as a flat `thiserror::Error` enum and is
transport-agnostic. The SDK crate does **not** depend on
`toolkit-canonical-errors`; consumers pattern-match variants directly.

The host crate (`usage-collector`) lifts `UsageCollectorError` onto the
canonical `toolkit_canonical_errors::CanonicalError` via
`From<UsageCollectorError> for CanonicalError` in
`usage-collector/src/infra/sdk_error_mapping.rs`; `CanonicalError`'s
built-in `IntoResponse` produces the RFC-9457 `Problem` envelope on the
REST surface. The variant → AIP-193 category → HTTP-status mapping
table is owned by DESIGN.md §3.3 Error Envelopes; the SDK variant
catalog is owned by `sdk-trait.md` "Error Taxonomy". A companion
plugin-side enum `UsageCollectorPluginError` is owned by `plugin-spi.md`
"Error Taxonomy".

## 7. Cross-Entity Invariants

- Every accepted record references a UsageType in the usage-type catalog
  (managed via the Plugin SPI, persisted in the active storage plugin's
  database; see ADR 0012 at
  `./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md`) by `gts_id`.
- Every write and read operation requires a resolved `SecurityContext` and a PDP
  decision.
- The collector fails closed on missing/invalid `SecurityContext`, PDP
  unavailability, validation failure, plugin readiness, or storage errors.
- `RecordMetadata` is the only extensible per-record payload; it remains opaque
  to the collector.
- Physical lifecycle, retention, backup, archival, purging, and backend-specific
  query acceleration are plugin-owned.
- Public REST, SDK, and Plugin SPI schemas may add optional fields within a
  major version, but removing fields or changing semantics requires the
  appropriate major-version break.
