# Usage Collector Plugin SPI Reference

<!-- toc -->

- [Overview](#overview)
- [Scope](#scope)
  - [In scope](#in-scope)
  - [Out of scope](#out-of-scope)
- [ToolKit Plugin SPI placement](#toolkit-plugin-spi-placement)
  - [Crate layout](#crate-layout)
  - [Trait declaration shape](#trait-declaration-shape)
  - [Two-trait split](#two-trait-split)
- [Plugin registration and discovery](#plugin-registration-and-discovery)
  - [GTS spec](#gts-spec)
  - [Plugin-side `init()` flow](#plugin-side-init-flow)
  - [Host-side resolution flow](#host-side-resolution-flow)
  - [Compile-time linkage](#compile-time-linkage)
  - [Vendor + priority selection](#vendor--priority-selection)
- [Domain Model](#domain-model)
  - [Core ingestion and identity types](#core-ingestion-and-identity-types)
  - [Query types and views](#query-types-and-views)
  - [Plugin-specific outputs](#plugin-specific-outputs)
  - [Trace context propagation](#trace-context-propagation)
  - [Cross-entity invariants honored by the Plugin SPI](#cross-entity-invariants-honored-by-the-plugin-spi)
- [Public Plugin SPI Trait](#public-plugin-spi-trait)
- [Method Contracts](#method-contracts)
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
- [Catalog and validation surface](#catalog-and-validation-surface)
- [Contract Tests](#contract-tests)
  - [`spi-contract-test-deactivate-cascade-usage`](#spi-contract-test-deactivate-cascade-usage)
  - [`spi-contract-test-deactivate-cascade-compensation`](#spi-contract-test-deactivate-cascade-compensation)
  - [`spi-contract-test-counter-only-compensation`](#spi-contract-test-counter-only-compensation)
  - [`spi-contract-test-value-matrix`](#spi-contract-test-value-matrix)
  - [`spi-contract-test-aggregation-sum-nets-and-usage-only-others`](#spi-contract-test-aggregation-sum-nets-and-usage-only-others)
- [Error Taxonomy](#error-taxonomy)
- [Consistency profile](#consistency-profile)
- [Versioning/Compatibility](#versioningcompatibility)
- [Exclusions/Non-goals](#exclusionsnon-goals)
  - [SDK-trait-only exclusions](#sdk-trait-only-exclusions)
  - [REST-only exclusions](#rest-only-exclusions)
  - [Gear non-goals reaffirmed on the Plugin SPI](#gear-non-goals-reaffirmed-on-the-plugin-spi)
- [Traceability](#traceability)
  - [Surface identifier and consumer contract](#surface-identifier-and-consumer-contract)
  - [Capabilities exposed by the Plugin SPI](#capabilities-exposed-by-the-plugin-spi)
  - [Domain entities](#domain-entities)
  - [Components allocated to the Plugin SPI](#components-allocated-to-the-plugin-spi)
  - [Persistence anchors](#persistence-anchors)
  - [Authorization, fail-closed, and attribution anchors (exclusions)](#authorization-fail-closed-and-attribution-anchors-exclusions)
  - [Versioning, stability, and quality NFR anchors](#versioning-stability-and-quality-nfr-anchors)
- [Open Questions](#open-questions)
- [Document Changelog](#document-changelog)

<!-- /toc -->

## Overview

The Usage Collector Plugin SPI is the in-process async Rust service
provider interface (SPI) that storage-backend authors implement so the
Usage Collector gear can persist, query, deactivate, and read usage
data without binding to any specific backend technology. The SPI is
the canonical realization of `cpt-cf-usage-collector-interface-plugin`
and the `cpt-cf-usage-collector-contract-storage-plugin` contract, and
it is the only path through which the Usage Collector's core reaches
durable state. Per
`cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference`
(see [`./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md`](./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md)),
this surface includes the UsageType Catalog (managed via the Plugin
SPI, persisted in the active storage plugin's database): catalog rows
live alongside `usage_records` in the plugin backend and the
`usage_records → usage_type_catalog` reference is enforced by a real
in-database `ON DELETE RESTRICT` foreign key on the `gts_id` column.
The catalog carries the GTS-typed usage-type metadata schema per
ADR 0012;
the gateway is the semantic owner of catalog operations (registration
API, PDP, validation, schema authority) and the plugin owns durable
storage and FK enforcement. The in-plugin reference scheme (column
type, index choice, or any other implementation choice) used to store
or look up `gts_id` is the plugin author's choice and is explicitly
out of SPI scope.

This document is the reference specification for the SPI trait. It
captures the operation set, method contracts (inputs, outputs, error
behaviour), domain types shared with the SDK trait, the SPI-only error
taxonomy, ToolKit crate placement, versioning and stability policy,
trace-context propagation requirements, and exclusions. The exact
Rust signature lives in `usage-collector-sdk/src/plugin_api.rs`;
this reference defines what every implementation and the calling
Plugin Host (`cpt-cf-usage-collector-component-plugin-host`) must
satisfy.

The Plugin SPI is one of three independently versioned public surfaces
described in DESIGN §3.3 — alongside `cpt-cf-usage-collector-interface-sdk-client`
(in-process SDK trait, see `sdk-trait.md`) and `cpt-cf-usage-collector-interface-rest-api`
(REST API, see `usage-collector-v1.yaml`). Each surface
evolves under the major-version stability contract anchored by
`cpt-cf-usage-collector-adr-contract-stability`,
`cpt-cf-usage-collector-principle-contract-stability`, and
`cpt-cf-usage-collector-nfr-plugin-contract-stability`.

## Scope

### In scope

The Plugin SPI realizes the following Usage Collector functional
capabilities at the persistence boundary:

- Durable persistence of single and batched `UsageRecord` submissions
  with caller-supplied idempotency keys, including dedup-on-conflict
  enforcement on the composite `(tenant_id, gts_id, idempotency_key)`
  (`cpt-cf-usage-collector-fr-pluggable-storage`,
  `cpt-cf-usage-collector-fr-ingestion`,
  `cpt-cf-usage-collector-fr-idempotency`,
  `cpt-cf-usage-collector-seq-emit-usage`).
- Server-side aggregated query execution with pushed-down SUM /
  COUNT / MIN / MAX / AVG and group-by, returning bucketed results
  (`cpt-cf-usage-collector-fr-query-aggregation`,
  `cpt-cf-usage-collector-fr-pluggable-storage`,
  `cpt-cf-usage-collector-seq-query-aggregated`,
  `cpt-cf-usage-collector-nfr-query-latency`).
- Cursor-paginated raw record retrieval, including plugin-owned cursor
  token generation and validation
  (`cpt-cf-usage-collector-fr-query-raw`,
  `cpt-cf-usage-collector-fr-pluggable-storage`,
  `cpt-cf-usage-collector-seq-query-raw`).
- Atomic one-way `active → inactive` deactivation of an individual
  `UsageRecord`
  (`cpt-cf-usage-collector-fr-event-deactivation`,
  `cpt-cf-usage-collector-seq-deactivate-event`,
  `cpt-cf-usage-collector-adr-monotonic-deactivation`,
  `cpt-cf-usage-collector-principle-monotonic-deactivation`).
- Durable storage of the UsageType Catalog alongside `usage_records`,
  with in-database `ON DELETE RESTRICT` referential integrity between
  `usage_records.gts_id` and `usage_type_catalog.gts_id`
  ([`./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md`](./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md)).
  The SPI exposes registration, read-by-`gts_id`, list, and delete so
  the gateway can administer the catalog and read it per call against
  the plugin SoR. Counter / gauge classification is derived at the call
  site from the `gts_id` prefix; `metadata_fields` is consumed verbatim
  from the row. The catalog row carries `metadata_fields: Vec<String>`
  (the closed,
  declared list of allowed metadata key names; all values typed as
  String end-to-end); the plugin does NOT validate the closed-shape
  membership rule itself — that hot path runs at the gateway per ADR 0012.
  The in-plugin reference scheme used by the storage plugin (column
  type, index choice, etc.) is the plugin author's choice and out of
  SPI scope.

The Plugin SPI does NOT expose a plugin-side readiness probe or
flush hook. Plugin availability is detected structurally by the
Plugin Host via `ClientHub::try_get_scoped` (an empty scoped slot
means the plugin gear has not yet registered or is gone); there
is no trait method that asks the plugin "are you ready?" and no
trait method that asks the plugin to drain. Graceful shutdown is
handled by the Plugin Host's process-level lifecycle, not by an SPI
call. See OQ-4 for the resolution.

### Out of scope

The Plugin SPI does not perform authentication, PDP authorization,
attribution validation, idempotency-key presence enforcement, kind
invariant enforcement against the UsageType Catalog, metadata size
checking, PDP constraint composition, or any pricing / billing /
quota logic. Every call arrives at the SPI already authorized and
structurally validated by the core (`cpt-cf-usage-collector-component-ingestion-gateway`,
`cpt-cf-usage-collector-component-query-gateway`,
`cpt-cf-usage-collector-component-deactivation-handler`, and
`cpt-cf-usage-collector-component-usage-type-catalog`), each of which
performs PDP enforcement inside its own service via the shared
`authz_scope` helper (calling `PolicyEnforcer::access_scope_with`).

The SPI does not own REST wire shapes, OpenAPI generation, RFC-9457
`Problem` mapping, CORS, TLS termination, or output encoding; those
are platform API gateway and gear-REST-handler responsibilities.
The SPI also does not declare per-tenant access tables, role
matrices, or PDP-decision caching — gear-side caching of PDP
decisions is forbidden by `cpt-cf-usage-collector-principle-pdp-centric-authorization`.

## ToolKit Plugin SPI placement

### Crate layout

The Plugin SPI trait belongs in the Usage Collector's single
`usage-collector-sdk` crate alongside the consumer SDK trait, the
GTS spec for plugin discovery, the domain models, and the public
error enum, following the platform-standard `<gear>` +
`<gear>-sdk` two-crate layout documented in DESIGN §3.12.9 Package
and Namespace Conventions. There is no separate `-contracts` crate
and no separate `-plugin-api` crate. Required files under
`usage-collector-sdk/src/`, all transport-agnostic:

- `lib.rs` — crate root and re-exports.
- `api.rs` — public consumer SDK trait declaration
  (`UsageCollectorClientV1`, the subject of `sdk-trait.md`).
- `plugin_api.rs` — public Plugin SPI trait declaration
  (`UsageCollectorPluginV1`, this document's subject).
- `gts.rs` — GTS spec for plugin discovery and binding (reserved; populated by the plugin-registration step per DESIGN §3.12.9).
- `models.rs` — pure-data domain types (UsageRecord, UsageType,
  queries, results, decisions, constraints) shared by both traits.
- `error.rs` — public, domain-classified error enum (see §"Error
  Taxonomy") surfaced through both the SDK trait and the Plugin SPI
  trait.

One concrete `usage-collector-plugin-<backend>` crate per backend
(for example `usage-collector-plugin-clickhouse`,
`usage-collector-plugin-timescaledb`) implements this trait and lives
under `gears/system/usage-collector/plugins/<backend>/`. Each
concrete-plugin crate depends on `usage-collector-sdk` only, never on
the host `usage-collector` crate, and is owned by the plugin's
authoring team.

### Trait declaration shape

- The trait is declared `async` (via the `async_trait` pattern), is
  `Send + Sync + 'static`, and is used through ClientHub as a trait
  object.
- The canonical trait name is `UsageCollectorPluginV1`, mirroring the
  `UsageCollectorClientV1` naming used by the SDK trait per DESIGN
  §3.12.9 and the ToolKit naming convention that places the gear
  name and capability before the `V1` suffix. The `V1` suffix encodes
  the Plugin SPI's major version and aligns with the gear's
  major-version stability contract.
- Every method takes `&self` as the receiver and accepts only the
  per-method domain inputs declared in §"Method Contracts" (see
  §"Trace context propagation" for the ambient-context model). Tracing
  is propagated via the ambient `tracing::Span` / OpenTelemetry context
  — no explicit `TraceContext` parameter is required (mirrors the
  reference plugin traits in `gears/credstore/credstore-sdk/src/plugin_api.rs:12-19`,
  `gears/system/authn-resolver/authn-resolver-sdk/src/plugin_api.rs:31-55`,
  and `gears/system/authz-resolver/authz-resolver-sdk/src/plugin_api.rs:19-22`,
  none of which carry a `TraceContext` parameter).
- The SPI does not accept a `SecurityContext` either, because
  authorization is already enforced upstream inside each domain
  component's `authz_scope` helper call.
- Methods return a `Result` whose `Err` variant is the
  `UsageCollectorPluginError` enum declared in
  `usage-collector-sdk/src/error.rs` (see §"Error Taxonomy"); the
  `Ok` variant is the method-specific output type declared in
  `usage-collector-sdk/src/models.rs` or, for SPI-local outcome
  enums, in `usage-collector-sdk/src/plugin_api.rs`.
- The trait is registered into ClientHub with **GTS instance scope**
  by each `usage-collector-plugin-<backend>` crate's own
  `#[toolkit::gear]` `init()` (the host
  `cpt-cf-usage-collector-component-plugin-host` does not register
  scoped plugin clients itself). The Plugin Host resolves the bound
  instance lazily on the first dispatch call after the
  `types-registry` has become consistent — the host's
  `GtsPluginSelector` runs the `[usage_collector].vendor`-driven
  match against `TypesRegistryClient::list_instances` exactly once
  via `get_or_init`, then caches the resolved `GtsInstanceId` for the
  `Service`'s lifetime; subsequent dispatches reuse the cached id
  through `ClientHub::try_get_scoped::<dyn UsageCollectorPluginV1>`
  under `ClientScope::gts_id(&instance_id)`. The
  `[usage_collector].vendor` value itself is read once in
  `Gear::init` via `ctx.config_or_default()?` and is never re-read
  at runtime (mirrors `gears/credstore/credstore/src/gear.rs:44-47`
  and `gears/credstore/credstore/src/domain/service.rs:53-75`);
  changing the binding requires a gear restart.

### Two-trait split

The public Plugin SPI trait, `UsageCollectorPluginV1`, is the
storage-backend-facing trait. The Usage Collector's separate public
SDK trait, `UsageCollectorClientV1`, is the consumer-facing trait
used by calling gears and downstream readers and is described in
`sdk-trait.md`. Both traits live side by side in the single
`usage-collector-sdk` crate (`api.rs` for the consumer SDK trait,
`plugin_api.rs` for the Plugin SPI trait) and share the same
`models.rs` domain types and `error.rs` error enum — there is no
separate `-contracts` or `-plugin-api` crate.

## Plugin registration and discovery

The Plugin SPI is wired into the runtime through the platform-standard
`PluginV1<P>` GTS base type + `types-registry` + `ClientHub` scoped
registration pattern — the same pattern used by `credstore`,
`authn-resolver`, and `authz-resolver`. Per
`cpt-cf-usage-collector-principle-plugin-resolution-via-client-hub`
(DESIGN §2.1), the SDK declares a unit-struct GTS spec; each plugin
publishes a `PluginV1<UsageCollectorPluginSpecV1>` instance to
`types-registry` and registers a scoped `dyn UsageCollectorPluginV1`
client in `ClientHub`; the host's plugin component lazily resolves
the bound instance by GTS schema id + configured vendor and caches it
for per-request in-memory dispatch.

### GTS spec

The SDK declares the GTS spec for usage-collector plugins in
`usage-collector-sdk/src/gts.rs`:

```rust
use toolkit::gts::PluginV1;
use toolkit_gts::gts_type_schema;

#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    schema_id = "gts.cf.toolkit.plugins.plugin.v1~cf.core.uc.plugin.v1~",
    description = "Usage Collector plugin specification",
    properties = "")]
pub struct UsageCollectorPluginSpecV1;
```

The empty `properties = ""` is intentional — plugin instance metadata
(`vendor`, `priority`) is carried by the `PluginV1<P>` base type and
is not duplicated in usage-collector-specific spec data. The
`schema_id` `gts.cf.toolkit.plugins.plugin.v1~cf.core.uc.plugin.v1~`
is the type identifier under which every concrete plugin instance is
registered with `types-registry`.

### Plugin-side `init()` flow

Each `usage-collector-plugin-<backend>` crate's `#[toolkit::gear]`
`init(...)` follows a four-step pattern: `build_registration` →
publish to `types-registry` → register the scoped client in
`ClientHub` → ready for dispatch.

```rust
let (instance_id, instance_json) = PluginV1::<UsageCollectorPluginSpecV1>::build_registration(
    "<vendor>.<package>.usage_collector_plugin.v1",
    cfg.vendor,
    cfg.priority)?;
let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
let results = registry.register(vec![instance_json]).await?;
RegisterResult::ensure_all_ok(&results)?;
let api: Arc<dyn UsageCollectorPluginV1> = service;
ctx.client_hub()
    .register_scoped::<dyn UsageCollectorPluginV1>(ClientScope::gts_id(&instance_id), api);
```

The final `GtsInstanceId` is
`UsageCollectorPluginSpecV1::SCHEMA_ID` concatenated with the supplied
instance segment (for example
`gts.cf.toolkit.plugins.plugin.v1~cf.core.uc.plugin.v1~<vendor>.<package>.usage_collector_plugin.v1`).
The `RegisterResult::ensure_all_ok` gate enforces that the
`types-registry` accepted every payload before the `ClientHub`
scoped registration commits, so callers never see a half-registered
plugin.

### Host-side resolution flow

The Plugin Host (`cpt-cf-usage-collector-component-plugin-host`,
implemented in `usage-collector/src/domain/service.rs`) owns a
`GtsPluginSelector` that lazily resolves the bound plugin instance.
The resolution flow:

```rust
let plugin_type_id = UsageCollectorPluginSpecV1::gts_schema_id().clone();
let instances = registry
    .list_instances(InstanceQuery::new().with_pattern(format!("{plugin_type_id}*")))
    .await?;
let instance_id = choose_plugin_instance::<UsageCollectorPluginSpecV1>(
    &self.vendor,
    instances.iter().map(|e| (e.id.as_ref(), &e.object)))?;
let scope = ClientScope::gts_id(instance_id.as_ref());
let client = self.hub.try_get_scoped::<dyn UsageCollectorPluginV1>(&scope)?;
```

The selector caches the resolved `GtsInstanceId` for the `Service`'s
lifetime. Warm-path dispatch reuses the cached id and performs a
single `ClientHub::try_get_scoped` lookup; the `types-registry`
round-trip happens only on the cold path (first call after bootstrap).
When no matching instance is registered, the host returns the
`plugin-unavailable` outcome documented in §"Error Taxonomy"; the
same structural fact (selector cached AND `try_get_scoped is Some`)
drives the `usage_collector.plugin.ready` gauge surfaced via OTLP
per the foundation feature.

### Compile-time linkage

Plugins are statically linked at the workspace level — every
`usage-collector-plugin-<backend>` crate is compiled in as a Cargo
workspace member and registered with ToolKit via `#[toolkit::gear]`
at startup. The host `usage-collector` crate has **no compile-time
dependency** on any concrete `usage-collector-plugin-<backend>`
crate; binding is purely a runtime concern resolved through
`types-registry` + `ClientHub`. Adding or swapping a plugin is a
workspace-build + config-vendor change, not a host-crate change, and
no dynamic-loading (`dlopen`, `libloading`, …) machinery is involved.

### Vendor + priority selection

When multiple plugin instances are registered under the same
`UsageCollectorPluginSpecV1` schema id (for example a deployment that
ships both ClickHouse and TimescaleDB plugins), the host's configured
vendor (field on `usage-collector`'s `config.rs`, populated from the
`[usage_collector].vendor` configuration key) drives
`choose_plugin_instance`. Matching is exact on `PluginV1.vendor`;
ties are broken by the lowest `PluginV1.priority` (lower number =
higher priority, mirroring the `PluginV1<P>` contract documented at
`libs/toolkit-gts/src/plugin.rs`). The resolved instance is cached
for the `Service`'s lifetime; binding changes require a gear
restart. There is no parallel cache and no retain-prior fallback —
`ClientHub::register_scoped` is a plain `HashMap::insert` under a
`parking_lot::RwLock` (see `libs/toolkit/src/client_hub.rs:155-165`),
and the host service performs `try_get_scoped` per call: `None` is
lifted to a per-call `plugin-unavailable` error rather than
substituting a prior binding.

## Domain Model

The Plugin SPI operates on the canonical Usage Collector domain types defined
in [`domain-model.md`](./domain-model.md). Refer to that document for
field-level semantics, identifier opacity, timestamp conventions, and
cross-entity invariants; the subsections below describe only the **SPI-specific**
aspects.

Types reused from `domain-model.md` (declared in `usage-collector-sdk/src/models.rs`,
transport-agnostic):

- Core ingestion: `UsageRecord` (§2.1), `ResourceRef` (§2.2), `SubjectRef` (§2.3),
  `UsageType` (§2.4), `IdempotencyKey` (§2.5), `RecordMetadata` (§2.6),
  `UsageRecordStatus` (§2.8).
- Query: `AggregationResult` (§3.3), and the SDK-side aggregation surface — `TimeWindow`, `AggregationOp`, `AggregationDimension`, `AggregationSpec`, `AggregationBucket` (declared in `usage-collector-sdk/src/models.rs`). `AggregationBucket.key` is `Vec<String>`; plugins MUST emit each entry as the canonical string form of the corresponding `AggregationDimension`: `TenantId` as `Uuid::to_string()` (lowercase, hyphenated), and every other dimension verbatim from the record (record metadata values are already strings per the `metadata_fields` closed-shape rule). Dimension *kind* is recoverable by position from the caller-supplied `group_by`; the wire shape carries no per-element discriminator.
- Filter / pagination: `UsageRecordQuery` (filterable-field schema, fed to `#[derive(ODataFilterable)]`), `UsageRecordFilterField` (macro-generated, §2.9), `MetadataFilter` (typed side channel for dynamic JSON-key filtering), `Keyset` (§2.10).

`SecurityContext` is **not** passed to the SPI — authorization is enforced
upstream.

### Core ingestion and identity types

SPI-specific aspects of the canonical types:

- `UsageRecord.uuid` is **plugin-minted** on first acceptance and returned on
  every subsequent read; the plugin is the authority for `id` allocation. The
  accepted row is immutable except for the one-way `Active → Inactive` status
  transition issued through Method 5 (`deactivate_usage_record`).
- Per `cpt-cf-usage-collector-adr-usage-compensation`, the SPI accepts both
  ordinary usage rows (`corrects_id IS NULL`) and counter-compensation rows
  (`corrects_id IS NOT NULL`) via the **same** persist call (Method 1); there
  is no separate `compensate` SPI method. The `corrects_id` predicate drives
  the value-sign matrix (Method 1) and the aggregation contract (Method 3).
- `RecordMetadata` MUST be persisted **byte-for-byte** and returned verbatim
  on read; the plugin MUST NOT index, aggregate, normalize, classify, or
  transform `metadata` content,
  and MUST NOT re-implement closed-shape metadata-key validation (the gateway
  validates against the per-usage-type `metadata_fields` set resolved via a
  `get_usage_type` SPI dispatch).
- `UsageType`:
  the catalog lives alongside `usage_records` in the same backend so that
  `usage_records.gts_id → usage_type_catalog.gts_id` is enforced by an
  in-database `ON DELETE RESTRICT` foreign key. The catalog row is flat for
  v1 — `gts_id` (PK and the FK target on every usage record) plus
  `metadata_fields: Vec<String>`; counter / gauge semantics are carried by
  the `kind` column on the catalog row and surfaced via `UsageType::is_counter()` /
  `is_gauge()`. The plugin sees `gts_id` as an opaque
  platform identifier — MUST NOT classify, parse, or interpret it. The
  in-plugin reference scheme (column type, index choice, or any other
  implementation choice) used to store or look up `gts_id` is plugin-author
  choice and out of SPI scope.

### Query types and views

SPI-specific aspects of the canonical query types:

- The aggregation-query inputs (`TimeWindow`, `&ODataQuery`, `&[MetadataFilter]`,
  `AggregationSpec`) reach the SPI **already PDP-constrained**: PDP-returned
  `PdpConstraint` filters have been intersected with user-supplied filters in
  `cpt-cf-usage-collector-component-query-gateway` before plugin dispatch (the
  OData filter via `Expr::and`-composition, the metadata side channel via slice
  extension). The plugin MUST treat every filter as authoritative and MUST NOT
  widen the result set beyond it.
- `RawQuery` does **not** reach the SPI directly: the Query Gateway parses it
  into the pair `(&toolkit_odata::ODataQuery, &[MetadataFilter])` before
  dispatching to Method 4. The `ODataQuery` carries the PDP-constrained
  `$filter` AST over `UsageRecordFilterField` (the macro-generated enum
  declared next to `UsageRecordQuery` in the SDK), canonical
  `created_at asc, uuid asc` order, decoded `Option<CursorV1>`, and
  gateway-clamped `limit`; `&[MetadataFilter]` carries the dynamic
  per-metadata-key filters that the OData grammar cannot express. The
  plugin consumes the parsed filter `ast::Expr` on `ODataQuery` and
  follows the per-field operator allowances in `domain-model.md` §2.10;
  metadata filters are lowered to the plugin's JSON-path facility (e.g.
  `metadata->>'key' = ANY($values)` on Postgres).
- Keyset pagination uses the canonical `(created_at, uuid)` tuple, carried in
  wire form as `toolkit_odata::CursorV1`. Plugins receive the decoded
  `Option<CursorV1>` on `ODataQuery::cursor` and emit the next-page keyset via
  `CursorV1::encode` into `Page::page_info::next_cursor`. Cursor decoding and
  structural validity (order/filter binding via `validate_cursor_against`) are
  gateway-owned; the plugin only consumes the decoded cursor and emits the
  next-row keyset.

### Plugin-specific outputs

Create and deactivate methods return plain data shapes rather than
dedicated outcome enums; failures use error variants instead.

- **Create single record output**: the persisted `UsageRecord`. The
  plugin returns either the newly written row (fresh insert) or the
  previously stored row when an exact-equality idempotency retry is
  silently absorbed. The dedup-key tuple is
  `(tenant_id, gts_id, idempotency_key)`; on a key collision the plugin
  compares the incoming record's canonical fields — `value`,
  `created_at`, `resource_ref`, `subject_ref`,
  `corrects_id`, and `metadata` — against the stored record. The
  dedup-key tuple itself is excluded (it is the match key) and the
  server-owned fields (`id`, `status`) are excluded. ALL compared
  fields equal → silent absorb (return the stored record on `Ok`);
  ANY compared field differs — including a metadata-only difference —
  → `IdempotencyConflict` error variant (the Plugin Host lifts this to
  the SDK-side `IdempotencyConflict` and the core surfaces it as a
  fail-closed `idempotency_conflict` rejection, AIP-193 AlreadyExists /
  `409`, DESIGN §3.3; the second write
  is never silently dropped). Duplicates MUST NOT accumulate the
  counter total.
- **Create batch output**: a list of per-record `Result<UsageRecord,
UsageCollectorPluginError>` in the same length and order as the input
  batch. Per-record errors do not cause the batch call as a whole to
  fail; the batch returns `Ok` on the list and the Plugin Host
  surfaces per-record outcomes to the Ingestion Gateway.
- **Deactivate output**: `()`. On `Ok(())` the targeted record was
  `Active` before the call and is now `Inactive`, AND every active
  compensation row whose `corrects_id` equals the targeted id has
  likewise been flipped to `Inactive` in the same atomic plugin
  transaction. The
  cascade is **empty** when the targeted row is itself a compensation
  (`corrects_id IS NOT NULL`; single-row, no cascade) — no row can
  reference a compensation row because L1 rejects
  `corrects_id_targets_compensation`, so no second hop is possible.
  The cascade is strictly **depth-1** by construction. The set of
  cascade-flipped row ids is not part of the SPI return shape;
  consumers that need it issue a follow-up read against the plugin's
  `status` and `corrects_id` columns. Rejection cases:
  - `UsageRecordNotFound { id }` — no record exists with the supplied
    `id` (Plugin Host lifts this to the SDK-side `UsageRecordNotFound` variant).
  - `UsageRecordAlreadyInactive { id }` — the record exists but its
    status is already `Inactive`; no state change occurred. This
    realizes the monotonicity invariant at the storage boundary. The
    one-way `Active → Inactive` latch applies to BOTH primary rows AND
    cascade-flipped compensation rows — no reverse transition exists.

  The catalog methods (Methods 6–9) likewise return plain data shapes:
  a structured `UsageTypeReferenced` error (see §"Error Taxonomy")
  surfaces FK-rejected deletes, `UsageTypeNotFound` covers a single-row
  miss on `get_usage_type` / `delete_usage_type`, and the list method
  uses a `Page` shape for pagination.

- `CatalogRow` — the per-row payload returned by Methods 7 and 8.
  Carries every `usage_type_catalog` column the gateway needs:
  `gts_id` (PK; the GTS identifier string deriving from the reserved
  abstract base `gts.cf.core.uc.usage_record.v1~`), `kind: UsageKind`
  (closed enum, counter / gauge — carries the row's counter / gauge
  classification), and `metadata_fields: Vec<String>` (the closed,
  declared list of allowed metadata key names for this usage type; all
  values typed as String end-to-end). Counter / gauge semantics are
  carried by the closed `UsageKind` enum on the catalog row and read via
  `UsageType::is_counter()` / `UsageType::is_gauge()`; `gts_id` does not
  encode kind. The SPI exposes these fields verbatim per ADR 0012.
- `toolkit_odata::Page<UsageType>` — the keyset-paginated shape Method 8
  emits. Carries `items: Vec<UsageType>` plus a `page_info` block with
  `next_cursor` (an opaque `toolkit_odata::CursorV1` over `gts_id`,
  `None` when the page is the last), `prev_cursor`, and the effective
  `limit`. Cursor minting and decoding are owned by the gateway; the
  plugin only consumes the decoded cursor and emits the next-row
  keyset.

### Trace context propagation

- Tracing is propagated via the ambient `tracing::Span` /
  OpenTelemetry context — no explicit `TraceContext` parameter is
  declared on any SPI method. The W3C Trace Context propagation values
  required by DESIGN §3.11.5 Distributed Tracing (`traceparent`,
  required, and `tracestate`, optional, per
  [W3C Trace Context Level 1](https://www.w3.org/TR/trace-context/))
  are carried by the active span / OpenTelemetry context that the
  Plugin Host establishes around each dispatch.
- The Plugin Host opens the per-call span before dispatching to the
  trait method (the host's `Service::*` methods are annotated with
  `#[tracing::instrument(...)]` mirroring
  `gears/credstore/credstore/src/domain/service.rs:109`); the SPI
  implementation runs inside that ambient span and MUST continue the
  span over its backend dispatch so end-to-end traces span gateway
  → core → plugin → backend.
- The reference plugin traits in
  `gears/credstore/credstore-sdk/src/plugin_api.rs:12-19`,
  `gears/system/authn-resolver/authn-resolver-sdk/src/plugin_api.rs:31-55`,
  and `gears/system/authz-resolver/authz-resolver-sdk/src/plugin_api.rs:19-22`
  carry no `TraceContext` parameter; this SPI follows the same pattern.
- `SecurityContext` is deliberately not passed to the SPI either,
  because authorization is already enforced upstream.

### Cross-entity invariants honored by the Plugin SPI

- Records persisted through the SPI honour the
  `(tenant_id, gts_id, idempotency_key)` UNIQUE constraint (the reference
  column carries the GTS identifier string `gts_id`;
  the in-plugin column type and index choice are the plugin author's
  choice and out of SPI scope). On a key
  collision the plugin compares the incoming record's caller-supplied
  canonical fields against the stored record (see §"Plugin-specific
  outputs"): an exact-equality retry is silently absorbed and the
  previously persisted `UsageRecord` is returned on the `Ok` arm (not
  an error), while a canonical-field mismatch surfaces as the
  `IdempotencyConflict` error variant, which the Plugin Host lifts to
  the SDK-side `IdempotencyConflict` and the core then surfaces as a
  fail-closed `idempotency_conflict` rejection (AlreadyExists / `409`)
  rather than a silent absorb.
- **Strict dedup-key preservation (normative).** The idempotency window
  is unbounded: the
  `(tenant_id, gts_id, idempotency_key)` dedup key never
  expires, has no TTL, and is never intentionally reusable, so the
  UNIQUE constraint is permanent. A storage plugin MUST preserve the
  `(tenant_id, gts_id, idempotency_key)` tuple permanently —
  even when the corresponding record bodies are purged or archived by
  the plugin's own retention policy. Retention, purge, and archival
  remain plugin-owned (`cpt-cf-usage-collector-adr-pluggable-storage`;
  see also §"Exclusions/Non-goals"), and this obligation refines, not
  contradicts, that ownership: the plugin still owns retention but
  MUST NOT free a dedup key. Retention / purge / archival MUST NOT
  release a `(tenant_id, gts_id, idempotency_key)` tuple, so
  a replayed key always resolves to a silently-absorbed exact-equality
  retry (the previously persisted `UsageRecord` is returned on the `Ok`
  arm) or an `IdempotencyConflict` error (canonical-field mismatch),
  never a fresh insertion.
- The referential rule
  `usage_records.gts_id → usage_type_catalog.gts_id` is enforced by a
  real `ON DELETE RESTRICT` foreign key inside the plugin's backend
  database
  ([`./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md`](./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md)).
  Both tables live in the same plugin backend so the FK rejection is
  atomic with the delete attempt — no cross-replica protocol, no
  distributed coordination, no gear-side `UsageTypeCatalogRepo`.
  The plugin attempts the delete via Method 9 (`delete_usage_type`)
  and lets the FK fire; on rejection the plugin returns a structured
  `UsageTypeReferenced` error (see §"Error Taxonomy") that the
  gateway surfaces deterministically. Backends that cannot enforce a
  native `ON DELETE RESTRICT` FK MUST emulate the check with a
  transactionally serializable read-before-delete inside the same
  transaction as the delete attempt; this is a plugin obligation.
  Historical `usage_records` rows cannot become orphaned at all
  because the FK rejects any unsafe delete; deleting a usage type
  without first deactivating its `usage_records` is structurally
  impossible.
- Deactivation is a status-only update; no other column of
  `usage_records` may be mutated by the SPI.
  Deactivation is a **depth-1 atomic set flip** (see Method 5):
  deactivating a usage row (one with `corrects_id IS NULL`) atomically
  flips the primary row plus every active compensation row (one with
  `corrects_id` set) whose `corrects_id` references the primary;
  deactivating a compensation row (one with `corrects_id IS NOT NULL`)
  flips that single row only (no cascade). The set commits as one
  atomic unit; partial cascades MUST be structurally impossible. The
  one-way `Active → Inactive` latch applies to primary rows AND
  cascade-flipped compensation rows.
- **`corrects_id`-driven persistence (compensation primitive).** Per
  `cpt-cf-usage-collector-adr-usage-compensation`, the plugin persists
  the signed `value` (carried as [`rust_decimal::Decimal`] on every
  surface — SDK, REST `UsageValue` (JSON string), plugin SPI — and
  intended to be stored as Postgres `NUMERIC`) and optional
  `corrects_id` on every accepted record;
  presence of `corrects_id` is itself the structural marker that
  distinguishes a counter-compensation row from an ordinary usage row.
  The value-sign matrix in Method 1 is enforced as a **structural
  precondition** at the persistence boundary; violations are surfaced
  as `Internal(detail)` (a non-retryable host-contract breach) and no
  row is inserted. The L1 referent
  checks for `corrects_id` (existence, `corrects_id IS NULL` on the
  referent, shared `(tenant_id, gts_id)`, `Active`) are caller
  responsibilities; the plugin enforces
  only the structural shape of each record (signed `value` and
  optional `corrects_id`).
- **No business logic (normative; refined for the compensation
  primitive).** The Plugin SPI defines no business logic. The plugin
  MUST NOT decide refunds, credits, credit-notes, quotas, lots,
  per-record remaining amounts, or net-non-negative enforcement. The
  plugin stores caller-supplied signed deltas and reports aggregates;
  recording a caller-supplied negative quantity is **recording, not
  computing**. A negative `SUM(value)` is an ordinary aggregation
  outcome — the plugin MUST NOT emit a negative-net detection signal,
  per `cpt-cf-usage-collector-adr-usage-compensation`.
- The plugin is the authority for `id` allocation on accepted
  records. Cursor decode and structural validation
  (`toolkit_odata::CursorV1::decode` plus `validate_cursor_against`)
  are gateway-owned; the plugin receives an already-decoded
  `Option<CursorV1>` carried on the `ODataQuery` and emits the
  next-page cursor token via `CursorV1::encode` into
  `Page::page_info::next_cursor`. Plugins are the authority for
  keyset-pagination ordering over the canonical `(created_at, uuid)`
  sort keys.
- The plugin MUST classify backend errors into the
  `UsageCollectorPluginError` taxonomy below so the Plugin Host can
  apply retry, circuit-break, or fail-closed behaviour without
  backend-specific parsing.

## Public Plugin SPI Trait

The Usage Collector exposes one public Plugin SPI trait,
`UsageCollectorPluginV1`. The trait is async, `Send + Sync + 'static`,
declared in `usage-collector-sdk/src/plugin_api.rs`, and registered
into ClientHub with GTS instance scope by each
`usage-collector-plugin-<backend>` crate's own `init()` (not by the
Plugin Host). The Plugin Host
(`cpt-cf-usage-collector-component-plugin-host`) resolves the bound
instance lazily on the first dispatch after the `types-registry` is
consistent and looks the client up through
`ClientHub::try_get_scoped`.

The trait carries the methods listed below, one per SPI-exposed
capability:

| Method (logical)           | Realizes                                                                                                                                | Inputs                                                                                                                                                                                                                                                                                                                                                                           | Output (Ok variant)                                                                                                                                                                                                                                                            |
| -------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Create single record       | `fr-pluggable-storage`, `fr-ingestion`, `fr-idempotency`, `fr-usage-compensation`, `seq-emit-usage`                                     | One `UsageRecord` (caller-supplied fields; `id` is plugin-allocated). Carries signed `value` and optional `corrects_id` (present marks a counter-compensation row; absent marks an ordinary usage row).                                                                                                                                                                          | `UsageRecord` — the newly persisted row on first acceptance, or the previously persisted row on a silently-absorbed exact-equality idempotency retry. A same-key canonical-field mismatch surfaces as the `IdempotencyConflict` error variant.                                  |
| Create batched records     | `fr-pluggable-storage`, `fr-usage-compensation`, `nfr-throughput`, `nfr-batch-and-report-timing`, `seq-emit-usage`                      | Non-empty list of `UsageRecord`. Per-record fields carry signed `value` and optional `corrects_id` as in Method 1.                                                                                                                                                                                                                                                               | `Vec<Result<UsageRecord, UsageCollectorPluginError>>` — per-record results in input order; each `Ok` carries the persisted (or silently-absorbed) `UsageRecord` and each `Err` is the per-record plugin error (e.g., `IdempotencyConflict`).                                    |
| Aggregated query           | `fr-pluggable-storage`, `fr-query-aggregation`, `nfr-query-latency`, `seq-query-aggregated`                                             | `TimeWindow` (`[from, to)` UTC); `&ODataQuery` (parsed PDP-constrained filter over `UsageRecordFilterField`; pagination/order fields ignored on this method); `&[MetadataFilter]` (typed JSON-key side channel); `AggregationSpec` (`op` ∈ SUM / COUNT / MIN / MAX / AVG, plus ordered `group_by` over `AggregationDimension`).                                                  | `AggregationResult` — `Vec<AggregationBucket>`; each bucket carries `key: Vec<String>` (in `group_by` order; empty for the no-grouping case) and `value: Option<Decimal>` (wire-encoded as a JSON string, `AVG` may carry a plugin-chosen rounding scale on non-terminating quotients). Dimension values follow the canonical encoding rule above (TenantId → `Uuid::to_string()`, lowercase hyphenated; all others verbatim).                                                                                                  |
| Raw keyset-paginated query | `fr-pluggable-storage`, `fr-query-raw`, `nfr-batch-and-report-timing`, `seq-query-raw`                                                  | `TimeWindow` (`[from, to)` UTC; replaces any `created_at` filter clause); `&ODataQuery` (`toolkit_odata`) carrying the parsed PDP-constrained filter, the canonical raw-query order (`created_at asc, uuid asc`), the optional decoded `CursorV1`, and the gateway-clamped `limit`; `&[MetadataFilter]` (typed JSON-key side channel).                                              | `toolkit_odata::Page<UsageRecord>` — page rows plus the last-row keyset wrapped as a `page_info` block; the plugin mints `page_info.next_cursor` via `CursorV1::encode`.                                                                                                         |
| Deactivate usage event     | `fr-event-deactivation`, `fr-usage-compensation`, `adr-monotonic-deactivation`, `adr-usage-compensation`, `seq-deactivate-event`        | `id` (`UsageRecord.uuid`); accepts any active row regardless of whether `corrects_id` is set.                                                                                                                                                                                                                                                                                      | `()` on successful transition (depth-1 atomic set flip; the primary row plus every active referencing compensation are flipped together when the primary is a usage row). Rejections surface as error variants: `UsageRecordNotFound { id }` or `UsageRecordAlreadyInactive { id }`. |
| Create usage type        | `fr-usage-type-registration`, `fr-pluggable-storage`, `adr-0012-unified-plugin-catalog-and-gts-id-reference`, `seq-register-usage-type` | `UsageType`: `gts_id` (GTS identifier string; catalog PK and the reference value on every usage record; MUST derive from the reserved abstract base `gts.cf.core.uc.usage_record.v1~` with at least one further `~`-separated segment), `kind: UsageKind` (closed enum, counter / gauge), `metadata_fields: Vec<String>` (the closed, declared list of allowed metadata key names for this usage type; all values typed as String end-to-end). | `CatalogRow` (the stored row keyed by `gts_id`).                                                                                                                                                                                                                               |
| Get usage type            | `fr-usage-type-existence-and-semantics`, `fr-pluggable-storage`, `adr-0012-unified-plugin-catalog-and-gts-id-reference`                 | `gts_id: String`.                                                                                                                                                                                                                                                                                                                                                                | `CatalogRow` on a hit; a miss surfaces as the `UsageTypeNotFound { gts_id }` error variant.                                                                                                                                                                                    |
| List usage types           | `fr-pluggable-storage`, `adr-0012-unified-plugin-catalog-and-gts-id-reference`                                                          | One `&ODataQuery` (`toolkit_odata`) carrying the optional `limit` and `cursor`. The foundation surface declares no filterable usage-type fields and any filter expression carried on the query is currently ignored — counter / gauge selection is performed client-side by reading `UsageType.kind` on the catalog row.                                                       | `toolkit_odata::Page<UsageType>` — page rows plus the last-row keyset wrapped as a `page_info` block.                                                                                                                                                                            |
| Delete usage type          | `fr-usage-type-deletion`, `fr-pluggable-storage`, `adr-0012-unified-plugin-catalog-and-gts-id-reference`, `seq-delete-usage-type`       | `gts_id: String`.                                                                                                                                                                                                                                                                                                                                                                | `()` on successful delete. The plugin attempts the row delete and relies on the `usage_records.gts_id` `ON DELETE RESTRICT` FK to fire on a referenced row; FK violations surface as the `UsageTypeReferenced` error variant (see §"Error Taxonomy"), not as `Ok`.             |

The trait carries exactly nine methods (five ingest / query / deactivate
plus four catalog); there is no plugin-side readiness probe and no
plugin-side flush. All methods return a `Result` over the listed Ok
variant and `UsageCollectorPluginError` (see §"Error Taxonomy"). The
UsageType Catalog (managed via the Plugin SPI, persisted in the
active storage plugin's database) is the sole usage-type catalog
([`./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md`](./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md));
the SPI exposes the four catalog methods above so the gateway can
administer the catalog and read it per call against the plugin SoR.
The in-plugin
reference scheme (column type, index choice, or any other
implementation choice) used to store or look up `gts_id` is the plugin
author's choice and out of SPI scope.

Note on batched persistence: DESIGN §3.3 and §1.2
(`cpt-cf-usage-collector-nfr-throughput-profile`) require the SPI to
expose batch ingestion so each plugin can drive its native bulk-write
paths. The two-method form (single + batched) mirrors the SDK trait's
two ingestion methods and matches the per-record acceptance
acknowledgement promise of `cpt-cf-usage-collector-component-ingestion-gateway`.

Note on trace-context propagation: DESIGN §3.11.5 requires
trace-context propagation on every SPI call so end-to-end traces span
gateway → core → plugin → backend. Propagation is carried by the
ambient `tracing::Span` / OpenTelemetry context opened by the Plugin
Host around each dispatch — no explicit `TraceContext` parameter
appears on any method (the reference plugin traits in `credstore`,
`authn-resolver`, and `authz-resolver` follow the same pattern).

## Method Contracts

Each method contract below lists the realized FR / sequence
identifiers, the structural inputs, the success output, and the
error categories the method may surface. Trace-context propagation is
ambient (see §"Trace context propagation") and is not declared per
method. Concrete error variant names are defined in §"Error
Taxonomy".

### Method 1 — Create single usage record

- Identifier: `create_usage_record`.
- Realizes: `cpt-cf-usage-collector-fr-pluggable-storage`, `cpt-cf-usage-collector-fr-ingestion`, `cpt-cf-usage-collector-fr-idempotency`, `cpt-cf-usage-collector-fr-usage-compensation`, `cpt-cf-usage-collector-seq-emit-usage`.
- Full input/output/error contract: see [`sdk-trait.md` §"Method 1 — Create single usage record"](./sdk-trait.md#method-1--create-single-usage-record). This SPI method is the durable persistence target dispatched by `UsageCollectorClientV1::create_usage_record` after gateway PDP, attribution validation, UsageType-existence lookup, closed-shape metadata validation, and L1 `corrects_id` referential checks.
- Structural inputs reaching the SPI: a `UsageRecord` value with all caller-supplied fields populated. `id` is plugin-minted.
- **Caller/plugin validation split.** The gateway enforces PDP attribution, idempotency-key presence, UsageType existence and counter/gauge semantics (via per-record `get_usage_type` SPI dispatch and `gts_id`-prefix derivation), closed-shape metadata-key membership, and the four `corrects_id` preconditions (existence, not-a-compensation, same `(tenant_id, gts_id)`, active) BEFORE invoking this method. The plugin MUST NOT re-execute those checks; a malformed or unauthorized call reaching the SPI is a Plugin Host contract breach surfaced as `Internal(detail)` (non-retryable).
- **Value-sign matrix (structural; enforced at the persistence boundary).**

  | Semantics | `corrects_id` | Allowed `value`                 | Outcome on violation              |
  | --------- | ------------- | ------------------------------- | --------------------------------- |
  | `counter` | `IS NULL`     | `value >= 0`                    | reject as `Internal(detail)`      |
  | `counter` | `SET`         | `value < 0` (strictly negative) | reject as `Internal(detail)`      |
  | `gauge`   | `IS NULL`     | Any signed value                | accept                            |
  | `gauge`   | `SET`         | REJECTED before persistence     | reject as `Internal(detail)`      |

  The plugin records the caller-supplied signed delta; it does NOT compute the delta.
- Plugin invariants:
  1. UNIQUE `(tenant_id, gts_id, idempotency_key)`. On collision, compare the incoming record's caller-supplied canonical fields (`value`, `created_at`, `resource_ref`, `subject_ref`, `metadata`, `corrects_id`) against the stored row (the dedup tuple itself and server-owned `id`/`status` are excluded). ALL-equal → silently absorb and return the stored row on `Ok`; ANY-differ — including metadata-only or divergent `corrects_id` — → `IdempotencyConflict`.
  2. Persist `metadata` byte-for-byte; the size cap is enforced upstream and the SPI MUST NOT silently truncate.
  3. Persist `status = Active` on first acceptance.
  4. Persist `corrects_id` exactly as supplied; the value-sign matrix above is a structural precondition (no row inserted on rejection).
  5. **Permanent dedup-key preservation.** The `(tenant_id, gts_id, idempotency_key)` UNIQUE constraint is unbounded: the key never expires, has no TTL, and is never intentionally reusable. Even after a record body is purged or archived, a replayed key MUST still resolve to a silently-absorbed exact-equality retry or `IdempotencyConflict` — never a fresh insertion. See §"Cross-entity invariants honored by the Plugin SPI".
- **Single ingestion path (no dedicated `compensate` SPI call).** Per `cpt-cf-usage-collector-adr-usage-compensation`, this same persist accepts both ordinary usage payloads (`corrects_id IS NULL`) and counter-compensation payloads (`corrects_id` set).
- Error variants the plugin may surface: `Transient`, `Internal`, `IdempotencyConflict`. Host-contract breaches the plugin happens to detect (value-sign matrix violation, …) lift through `Internal(detail)` (non-retryable). Upstream-enforced categories (typed validation variants, `UsageTypeNotFound`, `UnknownMetadataKey`, `Authorization`, `GaugeCompensationRejected`, `CorrectsId*`) are not raised by the SPI. "Not ready" is detected structurally by the Plugin Host before dispatch (no scoped client under `ClientScope::gts_id(instance_id)`); the SPI has no `Unready` variant.
- Latency budget: 75 ms p95 of the 200 ms total ingestion p95 per DESIGN §3.11.2.

### Method 2 — Create batched usage records

- Identifier: `create_usage_records`.
- Realizes: `cpt-cf-usage-collector-fr-pluggable-storage`, `cpt-cf-usage-collector-fr-ingestion`, `cpt-cf-usage-collector-fr-idempotency`, `cpt-cf-usage-collector-nfr-throughput`, `cpt-cf-usage-collector-seq-emit-usage`.
- Full input/output/error contract: see [`sdk-trait.md` §"Method 2 — Create batched usage records"](./sdk-trait.md#method-2--create-batched-usage-records).
- Structural inputs reaching the SPI: a non-empty list of `UsageRecord` values; an empty list is a host contract breach surfaced as `Internal(detail)` (non-retryable). List size is bounded upstream at the wire boundary (≤ 100 records per `usage-collector-v1.yaml`). The plugin's backend bulk-write path MUST be exercised so the `cpt-cf-usage-collector-nfr-throughput-profile` envelope is reachable.
- Plugin invariants: same per-record invariants as Method 1 (UNIQUE dedup tuple, byte-for-byte metadata, value-sign matrix, permanent key preservation). Per-record failures are reported as `Err` slots in the result list rather than failing the batch as a whole.
- Trace propagation: the ambient batch span (active `tracing` / OpenTelemetry context) is the parent of each per-record child span the plugin opens; no explicit `TraceContext` parameter.
- Success output: `Vec<Result<UsageRecord, UsageCollectorPluginError>>` in the same length and order as the input — the bare vec, no wrapper struct.
- Error variants at the call level: `Transient`, `Internal` (host-contract breaches such as an empty batch lift through `Internal(detail)`). Per-record errors carry the same variant catalog as Method 1.
- Latency budget: total end-to-end p95 envelope of 500 ms for a 100-record batch. DESIGN §3.11.2 does not carve a Plugin-SPI sub-allocation; plugins SHOULD reserve ≥ 25 ms for upstream gateway + PDP + core overhead (mirroring §3.11.2 patterns). See OQ-7.

### Method 3 — Aggregated query

- Identifier: `query_aggregated_usage_records`.
- Realizes: `cpt-cf-usage-collector-fr-pluggable-storage`, `cpt-cf-usage-collector-fr-query-aggregation`, `cpt-cf-usage-collector-fr-usage-compensation`, `cpt-cf-usage-collector-nfr-query-latency`, `cpt-cf-usage-collector-seq-query-aggregated`.
- Full input/output/error contract: see [`sdk-trait.md` §"Method 3 — Aggregated query"](./sdk-trait.md#method-3--aggregated-query).
- Canonical SPI signature:

  ```rust
  async fn query_aggregated_usage_records(
      &self,
      gts_id: UsageTypeGtsId,
      window: TimeWindow,
      query: &ODataQuery,
      metadata_filter: &[MetadataFilter],
      aggregation: AggregationSpec,
  ) -> Result<AggregationResult, PluginError>;
  ```

  Trace context is ambient (active `tracing::Span` / OpenTelemetry context); no explicit context parameter.
- Structural inputs reaching the SPI:
  - `gts_id: UsageTypeGtsId` — the typed usage-type key. The gateway has already validated usage-type existence and resolved the declared `metadata_fields`; the plugin lowers this to its `gts_id` column filter (`WHERE gts_id = $1`). The `gts_id` field on the OData filter surface is reserved and is guaranteed by the gateway to be absent from `query`.
  - `window: TimeWindow` — validated `[from, to)` UTC range.
  - `query: &ODataQuery` — parsed PDP-constrained filter over `UsageRecordFilterField` (minus the reserved `gts_id` field); pagination/order/cursor fields are ignored on this method (aggregation results are not paginated).
  - `metadata_filter: &[MetadataFilter]` — validated `(key, values)` entries for dynamic JSON-key filtering. Same lowering rule as Method 4 (`metadata->>'key' = ANY($values)` on Postgres, ANDed onto the OData-derived `WHERE`).
  - `aggregation: AggregationSpec` — `op` and `group_by`.

  Filters have already been intersected with PDP `PdpConstraint` filters by `cpt-cf-usage-collector-component-query-gateway`; the plugin MUST treat every filter as authoritative and MUST NOT widen the result set.
- **Pushdown obligation.** The plugin executes the chosen `aggregation` (SUM, COUNT, MIN, MAX, AVG) and any `group_by` dimensions server-side using its native acceleration structures (pre-aggregated materialized views, columnar indexes, etc.) per the NFR row for `cpt-cf-usage-collector-nfr-query-latency`. Fanning out per-row reads is forbidden — the SPI exposes aggregation as a single call so the core never iterates rows itself.
- **Aggregation contract (`corrects_id`-driven; normative).** Across every accepted filter scope, on rows where `status = Active`:
  - `SUM` MUST net across rows regardless of `corrects_id`, treating `value` as a signed quantity (counter compensation entries reduce the running total).
  - `COUNT`, `MIN`, `MAX`, `AVG` MUST operate over rows where `corrects_id IS NULL` only — compensation entries adjust `SUM`; they are not events, so including them would double-count or corrupt extremes/means.
  - Inactive rows (any `corrects_id`) are excluded from all five aggregations BEFORE the `corrects_id` partition. The status filter and the `corrects_id` partition are orthogonal.
  - A negative `SUM(value)` is an ordinary outcome — the plugin MUST NOT validate non-negative net per the un-policed-net stance (`cpt-cf-usage-collector-adr-usage-compensation`).
- Success output: `AggregationResult` bounded at the wire boundary by the caps declared in `usage-collector-v1.yaml` (≤ 100,000 rows over a 90-day single-tenant window with ≤ 2 groupings). An empty result returns empty `buckets`, not an error.
- Error variants: `Transient`, `Internal` (host-contract breaches lift through `Internal(detail)`).
- Latency budget: 425 ms p95 of the 500 ms total query p95 per DESIGN §3.11.2.

### Method 4 — Raw keyset-paginated query

- Identifier: `list_usage_records`.
- Realizes: `cpt-cf-usage-collector-fr-pluggable-storage`, `cpt-cf-usage-collector-fr-query-raw`, `cpt-cf-usage-collector-seq-query-raw`.
- Full input/output/error contract: see [`sdk-trait.md` §"Method 4 — Raw keyset-paginated query"](./sdk-trait.md#method-4--raw-keyset-paginated-query).
- Canonical SPI signature:

  ```rust
  async fn list_usage_records(
      &self,
      gts_id: UsageTypeGtsId,
      window: TimeWindow,
      query: &ODataQuery,
      metadata_filter: &[MetadataFilter],
  ) -> Result<Page<UsageRecord>, PluginError>;
  ```

  Trace context is ambient (active `tracing::Span` / OpenTelemetry context); no explicit context parameter.
- Structural inputs reaching the SPI:
  - `gts_id: UsageTypeGtsId` — the typed usage-type key. The gateway has already validated usage-type existence and resolved the declared `metadata_fields`; the plugin lowers this to its `gts_id` column filter (`WHERE gts_id = $1`). The `gts_id` field on the OData filter surface is reserved and is guaranteed by the gateway to be absent from `query`.
  - `window: TimeWindow` — validated `[from, to)` UTC range. Replaces any `created_at` predicate (which the SDK's `UsageRecordQuery` deliberately does not expose as a filterable field).
  - `query: &ODataQuery` carrying the parsed PDP-constrained `filter` over `UsageRecordFilterField` minus the reserved `gts_id` field (operator allowances per `domain-model.md` §2.9–§2.10), the parsed `order` (the gateway rejects anything other than canonical `created_at asc, uuid asc` upstream), `cursor: Option<CursorV1>` (decoded by the gateway and validated against order/filter via `toolkit_odata::validate_cursor_against` — plugins MAY treat it as a structural assertion), and gateway-clamped `limit: Option<u64>` bounded by the wire-level cap declared in `usage-collector-v1.yaml` (≤ 1,000 records over a 24-hour window).
  - `metadata_filter: &[MetadataFilter]` carrying validated `(key, values)` entries for filtering on the dynamic `UsageRecord.metadata` JSON map. Semantics: AND across distinct entries, OR within `MetadataFilter::values`; an empty slice imposes no metadata filter. Plugins lower this to their JSON-path facility (e.g. `metadata->>'key' = ANY($values)` on Postgres) and MUST AND the result with the `ODataQuery`-derived `WHERE`.

  The plugin MUST treat every filter as authoritative and MUST NOT widen the result set.
- **Keyset pagination obligation.** Plugins MUST implement keyset pagination over `(created_at, uuid)` so the combined order is total and stable across plugins. Offset/limit scans are forbidden. Plugins resume strictly after the cursor's `(created_at, uuid)` tuple and emit the next-row keyset as `Page::page_info::next_cursor` via `CursorV1::encode`.
- Cursor lifecycle: decode, structural validation, and order/filter-binding checks are gateway-owned. No plugin-error category exists for cursor validity (`cursor_decode`, `order_mismatch`, `filter_mismatch` are gateway-surfaced canonical Problems).
- Success output: `toolkit_odata::Page<UsageRecord>` (`items` plus `page_info { next_cursor, prev_cursor, limit }`; `next_cursor: None` on the last page). An empty match inside the authorized scope returns an empty page, not an error.
- Error variants: `Transient`, `Internal` (host-contract breaches lift through `Internal(detail)`).
- Latency budget: total end-to-end p95 envelope of 1 s for a 1,000-record raw page. DESIGN §3.11.2 does not carve a Plugin-SPI sub-allocation; plugins SHOULD reserve ≥ 25 ms for upstream gateway + PDP + core overhead. See OQ-7.

### Method 5 — Deactivate usage event

- Identifier: `deactivate_usage_record`.
- Realizes: `cpt-cf-usage-collector-fr-event-deactivation`, `cpt-cf-usage-collector-fr-usage-compensation`, `cpt-cf-usage-collector-seq-deactivate-event`, `cpt-cf-usage-collector-adr-monotonic-deactivation`, `cpt-cf-usage-collector-adr-usage-compensation`, `cpt-cf-usage-collector-principle-monotonic-deactivation`.
- Full input/output/error contract: see [`sdk-trait.md` §"Method 5 — Deactivate usage event"](./sdk-trait.md#method-5--deactivate-usage-event).
- Structural inputs reaching the SPI: the target `UsageRecord.uuid` (any active row, regardless of `corrects_id`).
- **Outcome shape (depth-1 atomic set flip; normative).** The capability returns `()` on success. The transition is a depth-1 atomic set flip, NOT a single-row flip:
  - When the primary row has `corrects_id IS NULL` (a usage row): the plugin MUST flip the primary row's `status` from `Active` to `Inactive` AND every currently-active row where `corrects_id = primary_id` AND `(tenant_id, gts_id) = primary.(tenant_id, gts_id)` in the **same atomic transition**. The set of cascade-flipped ids is NOT in the return shape; operators issue a follow-up `list_usage_records` against `status` and `corrects_id`.
  - When the primary row has `corrects_id IS NOT NULL` (a compensation row): single-row flip, no cascade evaluation — no row may reference a compensation.
  - The cascade depth bound is **structural**: the L1 rule that `corrects_id` MUST target a row with `corrects_id IS NULL` (caller-enforced at ingestion as `corrects_id_targets_compensation`) makes compensation-referencing-compensation impossible.
- **Atomicity invariant.** The set flip commits as one unit; partial cascades MUST be structurally impossible. Two concurrent deactivations targeting the same primary: exactly one returns `Ok(())`; the other returns `UsageRecordAlreadyInactive`. No column other than `status` is mutated.
- **One-way latch.** `Active → Inactive` is permanent for every row touched (primary and cascade-flipped).
- **Concurrency rule (caller-side).** A compensation referencing a row R that arrives while R is being deactivated is rejected by the caller-side L1 "MUST be active" check BEFORE this method is invoked, so the plugin sees an inert request and never coordinates with the in-flight cascade.
- Success output: `()`.
- Error variants: `Transient`, `Internal` (host-contract breaches lift through `Internal(detail)`), `UsageRecordNotFound`, `UsageRecordAlreadyInactive`. Cascade-flip failures surface as `Transient` or `Internal`; partial commit MUST NOT be observable.

### Method 6 — Create usage type

- Identifier: `create_usage_type`.
- Realizes: `cpt-cf-usage-collector-fr-usage-type-registration`, `cpt-cf-usage-collector-fr-pluggable-storage`, `cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference`, `cpt-cf-usage-collector-seq-register-usage-type`.
- Full input/output/error contract: see [`sdk-trait.md` §"Method 6 — Create usage type"](./sdk-trait.md#method-6--create-usage-type).
- Structural inputs reaching the SPI: a `UsageType` value with `gts_id` (catalog PK and FK target on every referencing `usage_records` row per ADR 0012; counter/gauge derived from prefix) and `metadata_fields: Vec<String>` (closed declared keys; all values typed as `String`).
- **Caller/plugin validation split.** The gateway enforces PDP authorization, `metadata_fields` well-formedness, and the `gts_id` reserved-prefix check (via `UsageTypeGtsId::new` upstream — REST synthesises `invalid_base_gts_id` Problem at HTTP 400; SDK surfaces a typed validation error) BEFORE invoking this method. The SPI never receives a malformed identifier. The plugin enforces structural constraints only: row uniqueness on `gts_id`, atomic insert.
- Plugin invariants:
  1. Insert a new row keyed by `gts_id`. The in-plugin reference scheme (column type, index choice) is plugin-author choice per ADR 0012.
  2. Persist `metadata_fields` verbatim (element order and content preserved); MUST NOT normalize, canonicalize, deduplicate, or interpret list contents.
  3. **Idempotency on `gts_id`.** A resubmission of an identical `UsageType` (same `gts_id`, element-equal `metadata_fields`) MUST return the existing `CatalogRow`. A resubmission with a differing payload MUST surface `UsageTypeAlreadyExists { gts_id }` so the gateway can lift it as a deterministic conflict.
- Success output: `CatalogRow` containing the stored row.
- Error variants: `Transient`, `Internal` (host-contract breaches lift through `Internal(detail)`), `UsageTypeAlreadyExists { gts_id }`.

### Method 7 — Get usage type

- Identifier: `get_usage_type`.
- Realizes: `cpt-cf-usage-collector-fr-usage-type-existence-and-semantics`, `cpt-cf-usage-collector-fr-pluggable-storage`, `cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference`.
- Full input/output/error contract: see [`sdk-trait.md` §"Method 7 — Get usage type"](./sdk-trait.md#method-7--get-usage-type).
- Structural inputs reaching the SPI: `gts_id: String`.
- Behaviour: verbatim read of the matching row including `gts_id` and `metadata_fields` (the full `CatalogRow`). Used by the gateway for per-call UsageType existence resolution on the ingestion hot path and by query-time declared-key resolution per ADR 0012. The plugin MUST NOT post-process the row content.
- Success output: `CatalogRow` on a hit.
- Error variants: `Transient`, `Internal`, `UsageTypeNotFound { gts_id }` (catalog miss; the gateway re-classifies it as `UsageTypeNotFound` on the ingestion path and surfaces it verbatim as `UsageTypeNotFound` on the admin-GET surface).

### Method 8 — List usage types

- Identifier: `list_usage_types`.
- Realizes: `cpt-cf-usage-collector-fr-pluggable-storage`, `cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference`.
- Full input/output/error contract: see [`sdk-trait.md` §"Method 8 — List usage types"](./sdk-trait.md#method-8--list-usage-types).
- Structural inputs reaching the SPI: `&ODataQuery` carrying optional `cursor` (`toolkit_odata::CursorV1` keyset over `gts_id`, gateway-minted and decoded) and `limit` (gateway-enforced cap). The foundation surface declares no filterable usage-type fields; any filter expression is currently ignored. Counter/gauge classification is carried by the `kind` column on each returned `UsageType` row and read via `UsageType::is_counter()` / `UsageType::is_gauge()`.
- Behaviour: return a page of `usage_type_catalog` rows ordered by `gts_id` ascending. The plugin MUST NOT impose any other ordering and MUST NOT mutate rows.
- Success output: `toolkit_odata::Page<UsageType>` (`items` plus `page_info { next_cursor, prev_cursor, limit }`; `next_cursor: None` on the last page).
- Error variants: `Transient`, `Internal` (host-contract breaches such as `limit = 0` lift through `Internal(detail)`).

### Method 9 — Delete usage type

- Identifier: `delete_usage_type`.
- Realizes: `cpt-cf-usage-collector-fr-usage-type-deletion`, `cpt-cf-usage-collector-fr-pluggable-storage`, `cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference`, `cpt-cf-usage-collector-seq-delete-usage-type`.
- Full input/output/error contract: see [`sdk-trait.md` §"Method 9 — Delete usage type"](./sdk-trait.md#method-9--delete-usage-type).
- Structural inputs reaching the SPI: `gts_id: String`.
- Behaviour: attempt to delete the `usage_type_catalog` row. The native `usage_records.gts_id ON DELETE RESTRICT` FK fires inside the same transaction; the plugin MUST NOT perform a separate "is it referenced?" probe — the FK is the single source of truth. On FK rejection the plugin surfaces a structured `UsageTypeReferenced { gts_id, sample_ref_count }` (sample count ≥ 1, plugin-tunable upper bound).
- **FK-emulation requirement.** Backends without native `ON DELETE RESTRICT` MUST emulate via a transactionally serializable read-before-delete in the same transaction; the emulation MUST NOT admit a window where a concurrent `create_usage_record` could insert a row referencing the `gts_id` being deleted.
- Plugin invariants:
  1. Reject with `UsageTypeNotFound { gts_id }` if no row has that `gts_id` (absence surfaced as error, not silent success, so the gateway distinguishes "already gone" from "successfully deleted now").
  2. Reject with `UsageTypeReferenced { gts_id, sample_ref_count }` on any referencing row.
- Success output: `()`.
- Error variants: `Transient`, `Internal` (host-contract breaches lift through `Internal(detail)`), `UsageTypeNotFound { gts_id }`, `UsageTypeReferenced { gts_id, sample_ref_count }`.

### Method 10 — Get single usage record

- Identifier: `get_usage_record`.
- Realizes: `cpt-cf-usage-collector-fr-pluggable-storage`, `cpt-cf-usage-collector-fr-event-deactivation` (attribution prefetch).
- Full input/output/error contract: see [`sdk-trait.md` §"Method 10 — Get single usage record"](./sdk-trait.md#method-10--get-single-usage-record).
- Structural inputs reaching the SPI: `uuid: Uuid` — the caller-supplied `UsageRecord.uuid`.
- Behaviour: verbatim read of the `usage_records` row whose primary key matches `uuid`. The plugin MUST NOT filter by `status` (both `active` and `inactive` rows are returned verbatim) and MUST NOT post-process the row content; the gateway needs the full attribution tuple (`tenant_id`, `gts_id`, `resource_ref`, optional `subject_ref`, `corrects_id`, `status`, …) for PDP enforcement and lifecycle gating on the deactivation flow (`cpt-cf-usage-collector-algo-event-deactivation-operator-pdp-authorization`).
- Plugin invariants:
  1. Reject with `UsageRecordNotFound { id }` if no row has that primary key (absence surfaced as a typed error, not silent success).
  2. The returned `UsageRecord` is byte-identical to the row originally persisted by Method 1 / Method 2 (modulo any subsequent monotonic `status` transition through Method 5).
- Success output: `UsageRecord` on a hit.
- Error variants: `Transient`, `Internal` (host-contract breaches lift through `Internal(detail)`), `UsageRecordNotFound { id }`.

## Catalog and validation surface

Per `cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference`
([`./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md`](./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md)),
the UsageType Catalog (managed via the Plugin SPI, persisted in the
active storage plugin's database) is the sole usage-type catalog and
lives alongside `usage_records`. The in-plugin reference scheme
(column type, index choice, or any other implementation choice) used
to store or look up `gts_id` is the plugin author's choice and is
out of SPI scope.
usage-collector owns the catalog **semantically** (registration API,
PDP, validation, schema authority); the plugin owns **durable
storage and FK enforcement**. The split is:

| Concern                                                            | Owner                            | Where it runs                                        |
| ------------------------------------------------------------------ | -------------------------------- | ---------------------------------------------------- |
| Catalog REST API (`POST` / `GET` / `DELETE /usage-types`)          | usage-collector gateway          | gateway process                                      |
| PDP authorization on register / read / list / delete               | usage-collector gateway          | gateway process                                      |
| `gts_id` prefix validation at register time                        | usage-collector gateway          | gateway process at register time                     |
| `metadata_fields` well-formedness at register time                 | usage-collector gateway          | gateway process at register time                     |
| Closed-shape metadata-key membership on incoming record `metadata` | usage-collector gateway          | gateway process at ingest hot path                   |
| Catalog rows (System of Record)                                    | storage plugin                   | plugin's backend DB, alongside `usage_records`       |
| `usage_records → usage_type_catalog` referential integrity         | storage plugin (engine)          | plugin's backend DB, via FK / serializable emulation |
| Catalog row inserts / reads / lists / deletes                      | storage plugin (engine)          | plugin's backend DB, via Methods 6 / 7 / 8 / 9       |

**Validation handoff (normative).** Closed-shape metadata-key
validation runs at the gateway, not the plugin. The plugin stores
`metadata_fields` verbatim; the gateway resolves each usage type's
`metadata_fields` per record via a `get_usage_type` SPI dispatch
(Method 7), per ADR 0012. Incoming `UsageRecord.metadata` keys are
checked for membership in the per-usage-type `metadata_fields` set at
the gateway before Method 1 or Method 2 dispatch; an undeclared key
is rejected as `UnknownMetadataKey { gts_id, key }`. **Plugins do NOT
re-implement closed-shape validation** — and the plugin MUST NOT
reject a `create_usage_record` call on metadata-key grounds (it MAY
reject on the structural value-sign matrix in Method 1, but the
closed-shape membership rule is the gateway's responsibility and
arrives at the SPI already enforced).

## Contract Tests

Every conforming plugin implementation MUST pass the contract tests
listed below. The tests are **behavioural**, not implementation-
specific: each test names a deterministic precondition (`setup`), a
deterministic action (`when`), and a deterministic acceptance
assertion (`then`). The tests are backend-agnostic — they MUST pass
regardless of the storage backend the plugin selects — and apply
uniformly to the persist capability (Method 1) and the deactivate
capability (Method 5). The pseudocode below uses a CDSL fence and
deliberately avoids any language-specific syntax.

The tests reinforce the **no-business-logic** invariant (see
§"Cross-entity invariants honored by the Plugin SPI" and the
`cpt-cf-usage-collector-constraint-no-business-logic` constraint):
the plugin records caller-supplied signed deltas and reports
aggregates; it never decides refunds, credits, quotas, or net-non-
negative enforcement.

### `spi-contract-test-deactivate-cascade-usage`

Cascade-on-usage: deactivating a usage row atomically flips every
active compensation referencing it.

```cdsl
setup:
  M is a counter usage type in tenant T with gts_id G   // G is M's GTS identifier string
  R = persist({ tenant_id: T, gts_id: G, value: +10, idempotency_key: K_R })
  for i in 1..N:
    C[i] = persist({ tenant_id: T, gts_id: G,
                     value: -1, corrects_id: R.id, idempotency_key: K_C_i })
  assert R.status = Active and every C[i].status = Active

when:
  outcome = deactivate(R.id)

then:
  assert outcome = Ok(())
  assert R.status = Inactive
  for i in 1..N:
    assert C[i].status = Inactive
  assert no other row in M's tenant scope changed status
  assert the (N + 1) status flips committed in a single atomic transition
```

Acceptance assertion: the call MUST return `Ok(())`, all N + 1 rows
MUST be `status = Inactive` in a single atomic commit, and no other
rows MUST change. The set of cascade-flipped ids is not part of the
return shape; a follow-up `list_usage_records` query against the
`status` and `corrects_id` columns enumerates it when needed.

### `spi-contract-test-deactivate-cascade-compensation`

No-cascade-on-compensation: deactivating a compensation row never
cascades.

```cdsl
setup:
  M is a counter usage type in tenant T with gts_id G   // G is M's GTS identifier string
  R = persist({ tenant_id: T, gts_id: G, value: +10, idempotency_key: K_R })
  C = persist({ tenant_id: T, gts_id: G,
                value: -3, corrects_id: R.id, idempotency_key: K_C })
  assert R.status = Active and C.status = Active

when:
  outcome = deactivate(C.id)

then:
  assert outcome = Ok(())
  assert C.status = Inactive
  assert R.status = Active  // R is untouched
```

Acceptance assertion: the call MUST return `Ok(())`, only C MUST flip
to `Inactive`, and R MUST remain `Active`. No cascade evaluation
occurs because the L1 rule rejects compensations targeting
compensations.

### `spi-contract-test-counter-only-compensation`

Counter-only compensation: a persist against a gauge usage type with
`corrects_id` set (a counter-compensation shape on a gauge) is
rejected at the structural boundary.

```cdsl
setup:
  M_g is a gauge usage type in tenant T with gts_id G_g   // G_g is M_g's GTS identifier string
  let pre_count = COUNT(*) over usage_records WHERE gts_id = G_g

when:
  attempt persist({ tenant_id: T, gts_id: G_g,
                    value: -5, corrects_id: <any>, idempotency_key: K })

then:
  assert persist returned Internal(detail) (deterministic non-retryable rejection signal)
  assert COUNT(*) over usage_records WHERE gts_id = G_g = pre_count  // no row inserted
```

Acceptance assertion: persist MUST be rejected at the structural
boundary with a deterministic non-retryable rejection signal
(`Internal(detail)`), and no row MUST be inserted.

### `spi-contract-test-value-matrix`

Value-matrix: the persist capability enforces the four-cell value
sign matrix structurally.

```cdsl
setup:
  M_c is a counter usage type in tenant T with gts_id G_c   // G_c is M_c's GTS identifier string
  M_g is a gauge usage type in tenant T with gts_id G_g     // G_g is M_g's GTS identifier string
  let pre_count = COUNT(*) over usage_records WHERE tenant_id = T

when / then (each row independent):

  // counter + corrects_id IS NULL with negative value -> REJECTED
  attempt persist({ gts_id: G_c, value: -1, ... })
  assert result = Internal(detail)
  assert COUNT(*) unchanged

  // counter + corrects_id SET with non-negative value -> REJECTED
  attempt persist({ gts_id: G_c, value: 0, ..., corrects_id: <some active usage row> })
  assert result = Internal(detail)
  assert COUNT(*) unchanged

  attempt persist({ gts_id: G_c, value: +1, ..., corrects_id: <some active usage row> })
  assert result = Internal(detail)
  assert COUNT(*) unchanged

  // gauge + corrects_id IS NULL with any signed value -> ACCEPTED
  attempt persist({ gts_id: G_g, value: -7, ... })
  assert result = Ok(UsageRecord { id: <id>... })

  attempt persist({ gts_id: G_g, value: +9, ... })
  assert result = Ok(UsageRecord { id: <id>... })

  // gauge + corrects_id SET (any value) -> REJECTED
  attempt persist({ gts_id: G_g, value: -2, ..., corrects_id: <some active usage row> })
  assert result = Internal(detail)
  assert no gauge row with corrects_id IS NOT NULL exists for tenant T
```

Acceptance assertion: rejections MUST be deterministic and no row
MUST be inserted on a rejected call. Accepted cells MUST return
`Ok(UsageRecord)` carrying a fresh plugin-allocated `id`.

### `spi-contract-test-aggregation-sum-nets-and-usage-only-others`

Aggregation-semantics: `SUM` nets across rows regardless of
`corrects_id` presence; `COUNT`, `MIN`, `MAX`, `AVG` operate over
rows where `corrects_id IS NULL` only.

```cdsl
setup:
  M is a counter usage type in tenant T with gts_id G   // G is M's GTS identifier string
  for i in 1..k:
    U[i] = persist({ tenant_id: T, gts_id: G, value: U_i_value, idempotency_key: K_U_i })
  for j in 1..m:
    X[j] = persist({ tenant_id: T, gts_id: G,
                     value: X_j_value, corrects_id: U[pick(j)].id, idempotency_key: K_X_j })
  assert U_i_value >= 0 for all i and X_j_value < 0 for all j

when:
  result_sum   = aggregate(SUM,   filter { gts_id: G, status: Active })
  result_count = aggregate(COUNT, filter { gts_id: G, status: Active })
  result_min   = aggregate(MIN,   filter { gts_id: G, status: Active })
  result_max   = aggregate(MAX,   filter { gts_id: G, status: Active })
  result_avg   = aggregate(AVG,   filter { gts_id: G, status: Active })

then:
  assert result_sum   = sum(U[i].value for i in 1..k) + sum(X[j].value for j in 1..m)   // signed net total
  assert result_count = k                                                                 // usage rows only
  assert result_min   = min(U[i].value for i in 1..k)                                     // usage rows only
  assert result_max   = max(U[i].value for i in 1..k)                                     // usage rows only
  assert result_avg   = sum(U[i].value for i in 1..k) / k                                 // usage rows only
  assert no X[j].value is included in COUNT, MIN, MAX, or AVG
```

Acceptance assertion: `SUM(value)` MUST return
`sum(U[i].value) + sum(X[j].value)` (compensation values are
negative, so SUM nets); `COUNT` MUST return `k` (the number of
usage rows); `MIN`, `MAX`, `AVG` MUST be computed over `{U[i].value}`
only and MUST NOT include any `X[j].value`. Compensation entries
adjust SUM; they are not events.

## Error Taxonomy

All Plugin SPI methods return `Result<…, UsageCollectorPluginError>`.
`UsageCollectorPluginError` is declared in
`usage-collector-sdk/src/error.rs` as a flat `thiserror::Error` enum
and is the plugin-side error vocabulary. The SDK crate (which owns
both `UsageCollectorError` and `UsageCollectorPluginError`) **does
NOT depend on `toolkit-canonical-errors`**; plugin authors and the
host's dispatch boundary pattern-match `UsageCollectorPluginError`
variants directly.

The host crate translates `UsageCollectorPluginError` into
`UsageCollectorError` variants at the dispatch boundary in
`usage-collector/src/domain/service.rs`. The translation is exhaustive
and per-variant:

| `UsageCollectorPluginError` variant                      | `UsageCollectorError` variant                              |
| -------------------------------------------------------- | ---------------------------------------------------------- |
| `Transient(detail)`                                      | `ServiceUnavailable { detail, retry_after_seconds: None }` |
| `Internal(detail)`                                       | `Internal(detail)`                                         |
| `UsageTypeAlreadyExists { gts_id }`                      | `UsageTypeAlreadyExists { gts_id }`                        |
| `UsageTypeNotFound { gts_id }`                           | `UsageTypeNotFound { gts_id }`                             |
| `UsageTypeReferenced { gts_id, sample_ref_count }`       | `UsageTypeReferenced { gts_id, sample_ref_count }`         |
| `IdempotencyConflict { idempotency_key, existing_uuid }` | `IdempotencyConflict { idempotency_key, existing_uuid }`  |
| `UsageRecordNotFound { id }`                             | `UsageRecordNotFound { id }`                               |
| `UsageRecordAlreadyInactive { id }`                      | `AlreadyInactive { id }`                                   |

The plugin classifies every non-domain failure into one of two
non-domain buckets: `Transient(detail)` for retryable backend failures
(downstream timeout, connection reset, upstream 5xx) and
`Internal(detail)` for non-retryable failures (host-contract breaches
the plugin happens to detect, plugin invariant violations, or any
uncategorized backend error). `Transient` lifts to the retryable
`ServiceUnavailable` envelope and is observed as retryable by
`UsageCollectorError::is_retryable`; `Internal` lifts to the
non-retryable `Internal` envelope.

Host-contract breaches the plugin happens to detect (empty batch,
value-sign matrix violation, `limit = 0`, …) lift through `Internal`
like any other non-retryable backend failure — a host-contract breach
reaching the SPI is observationally a fail-closed backend rejection
from the caller's perspective, and the gateway is the authority for
keeping malformed calls out of the SPI in the first place. The
catalog-domain variants `UsageTypeAlreadyExists`, `UsageTypeNotFound`,
and `UsageTypeReferenced` are plugin-surfaced now that the UsageType
Catalog (managed via the Plugin SPI, persisted in the active storage
plugin's database) is the sole usage-type catalog
([`./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md`](./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md)):
they originate from Methods 6 / 9 inside the plugin and are
translated by the dispatch boundary to the corresponding
`UsageCollectorError` variants for the SDK / REST surface (HTTP
mapping: `UsageTypeAlreadyExists → 409`, `UsageTypeNotFound → 404`,
`UsageTypeReferenced → 409`). The lift to the canonical `Problem`
envelope happens **downstream** of this translation, in the host's
REST layer at `usage-collector/src/infra/sdk_error_mapping.rs`
(`From<UsageCollectorError> for CanonicalError`); the plugin SDK
itself never sees `toolkit-canonical-errors`.

**Removed in ADR 0012:** the `DeclaredUsageTypeImmutable` variant and
the "declared usage type" boot-seeded notion are gone — the
gateway-local-from-config catalog has been retired and the
UsageType Catalog is the sole usage-type catalog.

The Plugin Host also projects each variant onto the operational-metric
`error_category` label set documented for
`usage_collector.plugin.accept_errors` in DESIGN §3.11.6 (`unready`,
`backend_error`, `timeout`). The mapping is host-side and lives in
`usage-collector/src/domain/service.rs`: `Transient` projects onto
`backend_error` (with `timeout` reserved for the host-side dispatch
deadline path, since the SPI does not carve a separate `Timeout`
variant); `Internal` projects onto `backend_error` like every other
backend-classified failure; and structural unavailability —
`ClientHub::try_get_scoped` returns `None` and the host lifts that to
`PluginUnavailable` — projects onto `unready` (the SPI itself exposes
no `Unready` error variant and no `ready()` probe). Cursor validity
is NOT a plugin-error category — cursor decode failure, order
mismatch, and filter mismatch are caught by the gateway before plugin
dispatch and surfaced as `UsageCollectorError::Validation` variants
whose host lift produces canonical `Problem` responses
(`cursor_decode`, `order_mismatch`, `filter_mismatch`), anchored by
`cpt-cf-usage-collector-principle-cursor-gateway-ownership`.

Variant catalog:

- `Transient(detail)` — retryable backend failure: downstream timeout,
  connection reset, upstream 5xx, or any other condition the plugin
  considers safe to retry. `detail` is operator-facing. Lifts to
  `UsageCollectorError::ServiceUnavailable` and is observed as
  retryable by `UsageCollectorError::is_retryable`. Structural
  unavailability is host-side and surfaces as `PluginUnavailable`, not
  as `Transient`.
- `Internal(detail)` — non-retryable failure: an uncategorized backend
  error, a broken plugin invariant, or a host-contract breach the
  plugin happens to detect (for example an empty batch, an
  `AggregationQuery` with an `aggregation` the plugin does not
  support, a Method 4 invocation that contradicts the canonical
  `(created_at, uuid)` keyset, or a value-sign matrix violation). The
  SPI does not carve out a separate variant for host-contract
  breaches: the gateway validates inputs upstream (§"Caller/plugin
  validation split"), so a breach reaching the SPI is observationally
  a fail-closed backend rejection from the caller's perspective and
  is non-retryable. `detail` is operator-facing and MUST be
  DSN-free / pre-redacted at the construction site. Cursor decode
  failure, order mismatch, and filter mismatch on raw queries are NOT
  reported through this channel — they are gateway-only failures
  surfaced as canonical Problem responses (`cursor_decode`,
  `order_mismatch`, `filter_mismatch`) before any plugin dispatch.
- `IdempotencyConflict { idempotency_key, existing_uuid }` — Methods
  1 / 2 (`create_usage_record` / `create_usage_records`) found the
  caller-supplied `idempotency_key` already bound to a different
  stored record. Carries the UUID of the previously persisted row so
  the gateway / caller can `get_usage_record` and reconcile. Lifts
  verbatim to `UsageCollectorError::IdempotencyConflict`. Exact-equality
  retries are silently absorbed on the `Ok` arm and MUST NOT raise
  this variant.
- `UsageRecordNotFound { id }` — Methods (`get_usage_record` /
  `deactivate_usage_record`) referenced an `id` that does not exist.
  Lifts verbatim to `UsageCollectorError::UsageRecordNotFound`.
- `UsageRecordAlreadyInactive { id }` — Method 5
  (`deactivate_usage_record`) targeted a row whose `status` was
  already `Inactive`. Lifts to `UsageCollectorError::AlreadyInactive`.
- `UsageTypeAlreadyExists { gts_id }` — Method 6
  (`create_usage_type`) saw a row with the same `gts_id` already
  present, and the request payload differs from the stored row.
  Surfaced by the dispatch boundary as
  `UsageCollectorError::UsageTypeAlreadyExists` and mapped to HTTP 409
  on the REST surface. An identical-payload resubmission MUST NOT
  raise this variant (Method 6 is idempotent on `gts_id` for
  byte-equal payloads).
- `UsageTypeNotFound { gts_id }` — raised by Method 7
  (`get_usage_type`) and Method 9 (`delete_usage_type`) when no
  `usage_type_catalog` row has the supplied `gts_id`. The SDK
  collapses ingestion-path and admin-path catalog misses into the
  single `UsageCollectorError::UsageTypeNotFound` variant per
  `error.rs`; the wire shape is identical.
- `UsageTypeReferenced { gts_id, sample_ref_count }` — Method 9
  (`delete_usage_type`) was rejected by the `usage_records.gts_id`
  `ON DELETE RESTRICT` FK. `sample_ref_count` carries a bounded
  sample of how many referencing rows the plugin observed (the
  plugin MUST NOT scan the entire table to compute an exact count —
  a small sample sufficient to confirm "still referenced" is
  enough). Surfaced by the dispatch boundary as
  `UsageCollectorError::UsageTypeReferenced` and mapped to HTTP 409 on
  the REST surface.
- Bad `gts_id` base-derivation rejection — Method 6
  (`create_usage_type`) cannot receive a `gts_id` that does not
  derive from the reserved abstract base
  `gts.cf.core.uc.usage_record.v1~` (with at least one further
  `~`-separated segment) per ADR 0012: the
  `UsageTypeGtsId::new` boundary upstream of the gateway rejects
  such payloads (REST: handler synthesises the canonical
  `invalid_base_gts_id` `Problem` envelope at HTTP `400` from the
  failed conversion on `CreateUsageTypeRequest::gts_id`; SDK:
  `UsageCollectorError::Validation` from `UsageTypeGtsId::new`).
  Unknown `kind` values are rejected by closed-enum serde rejection at
  the deserialize boundary; the SPI's `UsageCollectorPluginError`
  taxonomy therefore exposes no dedicated invalid-kind variant.
  Plugins MUST NOT parse `gts_id` to re-check the base derivation
  and MUST NOT synthesize a kind / base rejection.

`UnknownMetadataKey { gts_id, key }` is **not** an SPI variant.
Closed-shape metadata-key membership runs at the gateway against the
`metadata_fields` resolved via a `get_usage_type` SPI dispatch before
the Method 1 / Method 2 write call; an undeclared key is rejected as
`UsageCollectorError::UnknownMetadataKey` on the SDK / REST surface
without ever reaching the plugin. Plugins MUST NOT re-check
closed-shape membership and MUST NOT raise a closed-shape error of
any kind — they store `metadata` byte-for-byte (Method 1 invariant
2). See §"Catalog and validation surface" for the gateway-side
contract.

Behavioural notes:

- Exact-equality duplicate ingestion submissions are silently absorbed
  on the `Ok` arm — the previously persisted `UsageRecord` is returned
  on `Ok`, not as an error — preserving the silent-absorb success
  dedup semantics. A same-key submission whose canonical fields differ
  from the stored record surfaces as the `IdempotencyConflict` error
  variant; the Plugin Host translates `IdempotencyConflict` to the
  SDK-side `IdempotencyConflict`, which the core surfaces to the
  caller as a `UsageCollectorError` (the `idempotency_conflict`
  rejection, AlreadyExists / `409`, DESIGN §3.3) — NOT an `Ok` ack.
  Repeat deactivation against an already-inactive record is reported
  as the `UsageRecordAlreadyInactive` error variant; a missing target
  row is reported as the `UsageRecordNotFound` error variant — neither
  appears on the `Ok` arm. Catalog admin failures use error variants:
  a same-`gts_id` register-conflict is surfaced as
  `UsageTypeAlreadyExists`, a missing target row as
  `UsageTypeNotFound`, and an FK-rejected delete as
  `UsageTypeReferenced`
  ([`./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md`](./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md)).
  The SPI uses error variants only for failures the Plugin Host
  must classify for retry or fail-closed disposition.
- The Plugin SPI does NOT surface authentication-class, `Authorization`,
  or structural-validation variants because those failure
  classes are enforced upstream by
  `cpt-cf-usage-collector-component-ingestion-gateway`,
  `cpt-cf-usage-collector-component-query-gateway`,
  `cpt-cf-usage-collector-component-deactivation-handler`, and
  `cpt-cf-usage-collector-component-usage-type-catalog` (each performing
  PDP enforcement inline via the `authz_scope` helper) before any SPI
  call. A plugin that observed such a failure has, by definition,
  observed a host-contract breach and SHOULD return `Internal(detail)`
  rather than inventing a new error class.
- The five compensation-related codes
  (`gauge_compensation_rejected`, `corrects_id_not_found`,
  `corrects_id_targets_compensation`, `corrects_id_wrong_scope`,
  `corrects_id_inactive`) are SDK/REST-surface errors enforced on
  the ingestion path before SPI dispatch and do NOT appear in any
  SPI method outcome — see `sdk-trait.md` §Error Taxonomy and
  `usage-collector-v1.yaml` per-record 207 outcomes.
- Variant naming is canonical for this reference; the SPI crate MAY
  add per-variant context fields (such as a stable error code or
  operational trace pointer) as long as the public taxonomy
  preserves the domain classification above and the
  `usage_collector.plugin.accept_errors` label mapping.

## Consistency profile

The Plugin SPI inherits Usage Collector's plugin-agnostic consistency
floor and obliges every active plugin to publish its actual
consistency profile. The floor is the gear-level contract
documented in DESIGN §3.10 (Consistency Contract) and
`cpt-cf-usage-collector-adr-consistency-contract` (ADR-0011); this
section restates the floor on the SPI side and adds the per-plugin
deployment-guide obligation.

**SPI floor (normative).** The Plugin SPI's consistency floor is
identical to DESIGN §3.10's gear-level floor; nothing in the SPI
relaxes or strengthens it.

- **Ingestion ack** — once `create_usage_record` /
  `create_usage_records` return the persisted `UsageRecord` on the
  `Ok` arm (whether on first acceptance or on a silently-absorbed
  exact-equality retry), the record is durable; the
  `(tenant_id, gts_id, idempotency_key)` dedup tuple is
  permanently visible to subsequent persistence attempts on the
  ingestion path per §"Cross-entity invariants honored by the Plugin
  SPI" (strict dedup-key preservation, refining plugin-owned
  retention); and a subsequent `deactivate_usage_event` of the same
  row commits atomically with its depth-1 compensation cascade in a
  single backend transaction per Method 5 and
  `cpt-cf-usage-collector-adr-monotonic-deactivation` /
  `cpt-cf-usage-collector-adr-usage-compensation`. These are the
  in-transaction invariants the SPI already binds; the consistency
  contract restates them so the ingestion-side guarantee is named
  alongside the read-side guarantee.
- **Query SPI** — `query_aggregated`, `query_raw_keyset`,
  `get_usage_type`, and `list_usage_types` are **eventually
  consistent with no upper bound** relative to a
  same-tenant ingestion ack. The same record MAY be invisible to any
  of those methods for an indeterminate window after acknowledgement;
  the window is driven by the plugin's chosen replication topology
  and the workload-isolation routing it implements. **No
  monotonic-reads guarantee at the floor.** The floor is per-`(tenant_id,
gts_id)`; the SPI publishes no cross-tenant or
  cross-usage-type ordering claim.
- **Scope** — the floor covers BOTH the plugin's `usage_records`
  table and the plugin-owned `usage_type_catalog` table.
- **Within-transaction atomicity is not a cross-path guarantee.**
  The deactivate cascade documented in Method 5 commits as one
  backend transaction; that atomicity is a plugin-transaction
  invariant. A subsequent query against any read pool MAY observe a
  pre-cascade state until the pool converges. Consumers that need
  the post-cascade state for an immediate decision use the
  deactivate ack, not a follow-up query.

**Per-plugin profile (deployment-guide obligation).** Each
`usage-collector-plugin-<backend>` crate's deployment guide MUST
publish the plugin's actual consistency profile so consumers that
need a stronger bound can opt in by coupling to that plugin. The
profile is an honest description of the active deployment posture;
it is NOT a promise the SPI binds and it is NOT enforced by the host
or by Usage Collector itself.

- **Required content.** Every deployment guide MUST state, at
  minimum: (a) whether ingestion and query land on the same backend
  pool (sync, single-pool) or on isolated pools (asynchronous read
  replicas, separate executor pools); (b) the expected upper bound
  on Query-SPI lag relative to ingestion ack under the documented
  deployment posture (e.g., "sync — no observable lag",
  "bounded-staleness ≤ N ms with replication-lag alerting at N/2",
  or "eventual, no bound — see workload-isolation routing"); (c)
  whether monotonic-reads-per-`(tenant_id, gts_id)` holds
  under the default deployment posture, and if so, the configuration
  knobs that preserve it (e.g., session affinity, read-replica
  delay clamps, `select_sequential_consistency`); (d) whether the
  same profile applies to the catalog reads
  (`get_usage_type`, `list_usage_types`) or whether the catalog has
  its own stronger / weaker bound; (e) the
  procedure operators MUST follow if they deploy outside the
  documented posture (custom routing, non-default replica counts,
  cross-region read pools) and how that procedure interacts with the
  published profile.
- **Consumer-coupling rule.** Consumers that depend on a tighter
  bound than the gear floor couple themselves to a specific
  plugin's published ceiling; that coupling is intentional and MUST
  be recorded in the consumer's own design document so a plugin
  substitution surfaces as a known impact rather than a latent
  regression.
- **Drift discipline.** A change to the published profile that
  weakens any guaranteed bound is treated as a breaking change for
  every consumer coupled to the prior profile; the deployment guide
  MUST announce such changes with the same notice expected for
  ingestion or query availability. A strengthening change is
  additive and does not require notice.

**No typed `consistency_profile()` SPI method in v1.** The SPI
surface does not carry a typed accessor for the profile. The Plugin
SPI's major-version contract (ADR-0006) treats
new optional methods as additive, so a typed accessor MAY be added
in a later Plugin SPI minor release if a real consumer needs to
branch behavior on the profile at runtime. Until then, profile
discovery is documentation-only.

## Versioning/Compatibility

- The Plugin SPI is one of three independently versioned public
  surfaces (REST API, SDK trait, Plugin SPI). Each surface evolves
  under a major-version stability contract
  (`cpt-cf-usage-collector-adr-contract-stability`,
  `cpt-cf-usage-collector-principle-contract-stability`,
  `cpt-cf-usage-collector-nfr-plugin-contract-stability`,
  `cpt-cf-usage-collector-constraint-plugin-contract-stability`).
- The Plugin SPI's major version is encoded in the trait name
  suffix `V1`. A new major version (`V2`, and so on) is required for
  any breaking change.
- Within a major version only additive changes are permitted: new
  optional methods (with default implementations so existing plugin
  crates keep compiling), new optional fields on input types, new
  non-required variants on output enums, and new optional outcome
  variants. Removing methods, removing or renaming fields, narrowing
  accepted values, changing semantics, introducing a new required
  input, or removing a `default` implementation from a previously
  defaulted method is a breaking change and requires a new major
  version.
- Logical-table schema versioning is owned at the Plugin SPI surface
  per `cpt-cf-usage-collector-adr-contract-stability` (ADR-0006):
  additions to the logical record shape (new optional `usage_records`
  fields, new enum members) are additive within the current Plugin SPI
  major version; removals or semantic changes require a new major
  version.
- Deprecation flow: a Plugin SPI method, outcome variant, or field
  scheduled for removal in the next major release MUST be marked
  `deprecated` in the SPI trait rustdoc at least one minor release
  before the major bump.
- At most one prior major version is supported concurrently per
  surface.
  A Usage Collector gear instance MAY bind a `V1` plugin while
  another instance binds a `V2` plugin during a deprecation window;
  one Plugin Host instance binds exactly one plugin instance at a
  time
  Responsibility boundaries.
- Compile-time Rust trait compatibility tests gate every PR against
  the prior major per DESIGN §3.12.3 Contract test row.
- Per-method timeouts are part of the SPI rustdoc (one per method,
  bounded by the per-operation latency budgets in DESIGN §3.11.2:
  75 ms ingestion p95, 425 ms aggregated query p95, 1 s raw-page
  p95). A change to a per-call timeout value is an additive
  observation, not a breaking change to the trait shape.
- Plugins ship on independent release schedules from the Usage
  Collector itself.

## Exclusions/Non-goals

### SDK-trait-only exclusions

The Plugin SPI does not expose the operator-and-developer-facing
shape that the SDK trait surfaces; the SDK trait
(`cpt-cf-usage-collector-interface-sdk-client`, `sdk-trait.md`) owns
the consumer-facing concerns the SPI deliberately avoids:

- The SDK-side mapping of idempotency outcomes onto the consumer
  trait is an SDK-side concern; the SPI returns the persisted
  `UsageRecord` directly on `Ok` (silent-absorb for exact-equality
  retries) and surfaces canonical-field mismatches as the
  `IdempotencyConflict` error variant for the Plugin Host to adapt
  onto the SDK shape.
- The HTTP 204 No Content REST response shape on a successful
  deactivation and the SDK-side `AlreadyInactive` / `UsageRecordNotFound` error
  variants are SDK / REST-side; the SPI returns `()` on `Ok` and
  surfaces `UsageRecordAlreadyInactive` / `UsageRecordNotFound` as
  error variants for the Deactivation Handler to translate.
- The SDK-trait per-call timeout values, the SDK-trait `Result`
  shape, and the SDK error taxonomy are SDK-side; the SPI has its
  own taxonomy.

### REST-only exclusions

The Plugin SPI does not expose REST-handling concerns:

- The OpenAPI wire contract (`usage-collector-v1.yaml`),
  endpoint paths, and request/response schemas remain REST-side.
- RFC-9457 `Problem` envelope conversion is performed by the
  REST handler in the gear crate.
- CORS, TLS termination, output encoding, and HTTP-level rate
  limiting are platform API gateway responsibilities per DESIGN
  §3.9.3.
- Platform liveness and readiness probes are handled by the ToolKit host above the gear boundary; the collector exposes no gear-local health endpoints. Operational telemetry is pushed via OTLP from ToolKit's global `SdkMeterProvider` (no in-gear `/metrics` scrape endpoint exists). The SPI contributes no `ready` or `flush` operation; the structural readiness fact (selector cached AND `ClientHub::try_get_scoped` returns `Some`) is composed by the Plugin Host and surfaced via the `usage_collector.plugin.ready` gauge.

### Gear non-goals reaffirmed on the Plugin SPI

- A dedicated backfill capability (watermarks, late-data
  coordination, or a bulk-import method beyond `create_usage_records`)
  is an explicit non-goal in v1. Old event timestamps are accepted
  without wall-clock validation, so bulk historical import uses the
  same `create_usage_records` path with each record's true event
  timestamp (which still requires per-record idempotency keys and
  triggers the same dedup contract); see the timestamp /
  late-arrival invariant in `domain-model.md` §2.1 for the
  consequences for raw-tail consumers.
- Individual record amendment beyond deactivation is intentionally
  omitted; the SPI provides no `update_record` method. Corrections
  follow the §4 forward-looking pattern: deactivate the prior
  record, then emit a fresh idempotency-keyed record.
- Reactivation (`inactive → active`) is intentionally omitted; the
  SPI provides no transition for it.
- Multi-region deployment is not a v1 capability of the gear;
  cross-region durability, read locality, and conflict resolution
  remain plugin-deployment and platform-topology concerns per
  `cpt-cf-usage-collector-adr-pluggable-storage` (ADR-0002).
- Gear-emitted audit events for operator-write paths are not the
  SPI's responsibility; the v1 access trail is composed at the
  gateway and PDP decision points per DESIGN §3.9.5 and §4.
- Pricing, rating, billing, invoice generation, and quota
  decisions are out of scope for every SPI operation. The plugin
  MUST NOT decide refunds, credits, credit-notes, or net-non-negative
  enforcement; recording a
  caller-supplied negative quantity (a row with `corrects_id` set and
  `value < 0`) is **recording, not computing**. Per-record remaining-amount tracking, lot /
  FIFO-LIFO accounting, and negative-`SUM` detection / alerting are
  explicit non-goals.
- Gear-side caching of PDP decisions is forbidden; the
  SPI sees only post-authorization queries.
- At-rest encryption, key management, masking, disposal, backup,
  point-in-time recovery, disaster recovery, replication, tiering,
  retention windows, archival, compression, encoding, partitioning,
  and acceleration structures (including any indexing the plugin
  chooses to satisfy `cpt-cf-usage-collector-nfr-query-latency`) are
  plugin-owned and operator-tuned, not part of the SPI contract. This
  plugin ownership of retention and archival is refined, not
  contradicted, by the strict dedup-key-preservation obligation in
  §"Cross-entity invariants honored by the Plugin SPI": the plugin
  still owns retention but MUST NOT free a
  `(tenant_id, gts_id, idempotency_key)` dedup key when it purges
  or archives the corresponding record bodies.
- Dead-letter queue, poison-message handling, and compensation-saga
  patterns are out of scope for the SPI; persistence is a single
  synchronous call that either succeeds (returning the persisted
  `UsageRecord` on `Ok`, whether freshly written or silently absorbed
  on an exact-equality retry), surfaces a canonical-field mismatch as
  the `IdempotencyConflict` error variant, or returns a classified
  error.

## Traceability

### Surface identifier and consumer contract

- `cpt-cf-usage-collector-interface-plugin` — the public Plugin SPI
  interface identifier carried by `UsageCollectorPluginV1`. Source:
  DESIGN §3.3 "Plugin SPI" row.
- `cpt-cf-usage-collector-contract-storage-plugin` — the consumer
  contract realized by the SPI. Source: DESIGN §3.3 Plugin SPI
  "Contracts" row; §3.5 Storage Plugin Contract.

### Capabilities exposed by the Plugin SPI

- Create single record and create batched records:
  `cpt-cf-usage-collector-fr-pluggable-storage`,
  `cpt-cf-usage-collector-fr-ingestion`,
  `cpt-cf-usage-collector-fr-idempotency`,
  `cpt-cf-usage-collector-fr-record-metadata`,
  `cpt-cf-usage-collector-seq-emit-usage`. Throughput and
  Throughput:
  `cpt-cf-usage-collector-nfr-throughput`,
  `cpt-cf-usage-collector-nfr-throughput-profile`. Ingestion
  latency:
  `cpt-cf-usage-collector-nfr-ingestion-latency` (Plugin SPI
  allocation per §3.11.2). Sources: DESIGN §3.3, §3.6, §3.11.2.
- Aggregated query:
  `cpt-cf-usage-collector-fr-pluggable-storage`,
  `cpt-cf-usage-collector-fr-query-aggregation`,
  `cpt-cf-usage-collector-seq-query-aggregated`,
  `cpt-cf-usage-collector-nfr-query-latency`. Source: DESIGN §3.3,
  §3.6, §3.11.2.
- Raw cursor-paginated query:
  `cpt-cf-usage-collector-fr-pluggable-storage`,
  `cpt-cf-usage-collector-fr-query-raw`,
  `cpt-cf-usage-collector-seq-query-raw`. Source:
  DESIGN §3.3, §3.6.
- Deactivate usage event (depth-1 atomic set flip):
  `cpt-cf-usage-collector-fr-event-deactivation`,
  `cpt-cf-usage-collector-fr-usage-compensation`,
  `cpt-cf-usage-collector-seq-deactivate-event`,
  `cpt-cf-usage-collector-adr-monotonic-deactivation`,
  `cpt-cf-usage-collector-adr-usage-compensation`,
  `cpt-cf-usage-collector-principle-monotonic-deactivation`. Source:
  DESIGN §3.3 Deactivate response shape; §3.6 Deactivate Usage Event;
  ADR-0005 Decision and Consequences (deactivation cascade).
- Counter compensation (value-reversal; rides Method 1 / Method 2):
  `cpt-cf-usage-collector-fr-usage-compensation`,
  `cpt-cf-usage-collector-adr-usage-compensation`. Source: DESIGN §3.3
  Unified ingestion request shape; ADR-0008 Decision and Consequences
  (compensation primitive).
- Catalog write / read / list / delete (Methods 6–9):
  `cpt-cf-usage-collector-fr-usage-type-registration`,
  `cpt-cf-usage-collector-fr-usage-type-existence-and-semantics`,
  `cpt-cf-usage-collector-fr-usage-type-deletion`,
  `cpt-cf-usage-collector-seq-register-usage-type`,
  `cpt-cf-usage-collector-seq-delete-usage-type`,
  `cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference`.
  Per ADR 0012, the UsageType Catalog (managed via the Plugin SPI,
  persisted in the active storage plugin's database) is the sole
  usage-type catalog: its rows live alongside `usage_records` and the
  FK `usage_records.gts_id → usage_type_catalog(gts_id) ON DELETE RESTRICT`
  is enforced natively. The gateway owns the semantic surface (PDP,
  validation, schema authority) and dispatches catalog ops through
  these four methods. The catalog row carries `metadata_fields:
Vec<String>` (the closed, declared list of allowed metadata key
  names; stored verbatim, all values typed as String end-to-end); the
  gateway reads the row per call via `get_usage_type` and derives the
  counter / gauge discriminator at the call site from the `gts_id`
  prefix.
  The in-plugin reference scheme (column type, index choice) is the
  plugin author's choice and out of SPI scope. Source: DESIGN §3.6
  Register UsageType / Delete UsageType sequences;
  [`./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md`](./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md).
- Readiness and graceful shutdown:
  `cpt-cf-usage-collector-nfr-availability`. Source: DESIGN
  §3.11.6 `usage_collector.plugin.ready` gauge (the structural
  readiness fact pushed via OTLP).

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
  `AggregationResult`. Source: DESIGN
  §3.1 Domain Model; `domain-model.md` §2 Core Entities, §3 Query
  Domain, §5 Plugin Binding Domain. Raw-page output is the canonical
  `toolkit_odata::Page<UsageRecord>` shape; cursor lifecycle is
  realized by `toolkit_odata::CursorV1` plus
  `validate_cursor_against`. The
  former gear-owned `RawRecordPage` and `CursorToken` entities
  defined in earlier drafts of `domain-model.md` are no longer
  carried by this SPI (phase-01
  `out/phase-01-domain-contracts.md` §1).

### Components allocated to the Plugin SPI

- `cpt-cf-usage-collector-component-plugin-host` — the sole
  in-process component that dispatches against the SPI per DESIGN
  §3.3 ("Allocated To") and §3.2 Plugin Host Responsibility scope.
- The SPI is also a downstream collaborator of
  `cpt-cf-usage-collector-component-ingestion-gateway`,
  `cpt-cf-usage-collector-component-query-gateway`,
  `cpt-cf-usage-collector-component-deactivation-handler`, and
  `cpt-cf-usage-collector-component-usage-type-catalog`, but only the
  Plugin Host calls the SPI directly per §3.2 Plugin Host
  Responsibility scope.

### Persistence anchors

The Plugin SPI dispatches against two logical persistence anchors
owned by the active storage plugin; concrete table layouts are
plugin-internal and live in each plugin's own DESIGN document.

- **Usage records store** — durable rows emitted by Methods 1 / 2
  and read by Methods 3 / 4; status updated by Method 5. The plugin
  enforces the dedup composite `(tenant_id, gts_id, idempotency_key)`
  permanently (see §"Cross-entity invariants honored by the Plugin
  SPI") and the `ON DELETE RESTRICT` reference to the usage-type
  catalog.
- **Usage-type catalog** — durable catalog rows written by Method 6,
  read by Methods 7 / 8, and deleted by Method 9. Owned by the plugin
  per ADR 0012 so the cross-table FK with the usage records store is
  enforceable natively.
  Source: ADR-0009; ADR-0010; ADR-0012.

### Authorization, fail-closed, and attribution anchors (exclusions)

The SPI does NOT participate in any of the following — these anchors
are listed so reviewers can confirm the SPI's responsibility
boundary against them:

- `cpt-cf-usage-collector-contract-authz-resolver`,
  `cpt-cf-usage-collector-principle-fail-closed`,
  `cpt-cf-usage-collector-principle-pdp-centric-authorization`,
  `cpt-cf-usage-collector-adr-pdp-centric-authorization`,
  `cpt-cf-usage-collector-adr-caller-supplied-attribution`,
  `cpt-cf-usage-collector-adr-mandatory-idempotency`,
  `cpt-cf-usage-collector-constraint-pii-identity-layer`,
  `cpt-cf-usage-collector-constraint-no-business-logic`. Source:
  DESIGN §3.2 "Plugin Host" Responsibility boundaries; §3.9.6
  Authorization Architecture; `cpt-cf-usage-collector-adr-mandatory-idempotency`
  ("retries are caller-owned, made safe by mandatory idempotency").
- Semantics-violation enforcement
  (`cpt-cf-usage-collector-fr-counter-semantics`,
  `cpt-cf-usage-collector-fr-gauge-semantics`) is upstream in
  `cpt-cf-usage-collector-component-ingestion-gateway` /
  `cpt-cf-usage-collector-component-usage-type-catalog`; the SPI
  persists `value` byte-for-byte without invariant enforcement.

### Versioning, stability, and quality NFR anchors

- `cpt-cf-usage-collector-adr-contract-stability`,
  `cpt-cf-usage-collector-adr-pluggable-storage`,
  `cpt-cf-usage-collector-principle-contract-stability`,
  `cpt-cf-usage-collector-principle-pluggable-storage`,
  `cpt-cf-usage-collector-nfr-plugin-contract-stability`,
  `cpt-cf-usage-collector-nfr-ingestion-latency`,
  `cpt-cf-usage-collector-nfr-query-latency`,
  `cpt-cf-usage-collector-nfr-throughput`,
  `cpt-cf-usage-collector-nfr-throughput-profile`,
  `cpt-cf-usage-collector-nfr-workload-isolation`,
  `cpt-cf-usage-collector-nfr-availability`,
  `cpt-cf-usage-collector-constraint-plugin-contract-stability`,
  `cpt-cf-usage-collector-constraint-vendor-pluggable`. Source:
  `cpt-cf-usage-collector-adr-contract-stability` (ADR-0006);
  DESIGN §3.12.8 Versioning and Deprecation Policy; §3.11.2 Latency
  Budgets; §1.2 NFR rows enumerated by ID.
- `cpt-cf-usage-collector-adr-consistency-contract` —
  floor-and-ceiling consistency contract restated on the SPI side in
  §"Consistency profile"; obliges every active plugin's deployment
  guide to publish its actual profile; no typed
  `consistency_profile()` SPI method in v1. Source: DESIGN §3.10
  Consistency Contract; ADR-0011 Decision and Consequences;
  §"Cross-entity invariants honored by the Plugin SPI" (the
  in-transaction invariants the floor cites).

## Open Questions

These are residual choices the `usage-collector-sdk` crate may
finalize during implementation. None block this reference; each notes
the conservative default this reference adopts.

- OQ-1 — **Resolved (bare `Vec` is canonical)**: the batched-persist
  return shape is the bare
  `Vec<Result<UsageRecord, UsageCollectorPluginError>>` directly on
  the `Ok` arm; no named wrapper struct is interposed. This shape is
  ergonomic for `Iterator` and `?` propagation in the Plugin Host and
  removes the prior `BatchPersistOutcome` envelope.
- OQ-2 — **Resolved (catalog is a Plugin SPI capability; snapshot
  reads out-of-scope for v1)**: prior drafts asked whether
  `get_usage_type` and `list_usage_types` accept an explicit `at`
  timestamp for snapshot reads. Per
  `cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference`
  ([`./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md`](./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md))
  the UsageType Catalog (managed via the Plugin SPI, persisted in the
  active storage plugin's database) is the sole usage-type catalog
  and is a Plugin SPI capability (Methods 6 / 7 / 8 / 9). The
  `at`-timestamp snapshot variant of Methods 7 / 8 is deliberately
  **omitted** in v1 — the catalog is read at "now" semantics only
  via per-call `get_usage_type` SPI dispatch, and historical /
  point-in-time catalog reads are a non-goal. A future minor version
  MAY add an optional
  `at: Option<Timestamp>` field on the read methods without breaking
  compatibility per §"Versioning / Compatibility".
- OQ-3 — Whether `query_aggregated_usage_records` accepts a per-call hint at
  acceleration structures (for example "prefer pre-aggregated
  rollups" or "fall back to scan"). This reference omits the hint;
  plugins choose acceleration internally to meet
  `cpt-cf-usage-collector-nfr-query-latency`. Hints MAY be added as
  optional fields on `AggregationQuery` in a future minor version
  without breaking compatibility.
- OQ-4 — Whether the SPI surfaces a separate `health` probe distinct
  from `ready`. **Resolved (no separate probe)**: this reference
  exposes **no** plugin-side readiness or health probe at all.
  Plugin availability is detected structurally by the Plugin Host
  via `GtsPluginSelector::get_or_init` (selector cached) AND
  `ClientHub::try_get_scoped::<dyn UsageCollectorPluginV1>` returns
  `Some` — these two structural facts are the only "is the plugin
  live?" signal, matching the reference-gear pattern in
  `credstore`, `authn-resolver`, and `authz-resolver`. Liveness is
  the Plugin Host's process-level health, observed by the ToolKit
  host outside the gear surface (the collector exposes no
  gear-local liveness endpoint). Plugins MAY expose
  backend-internal liveness through backend-specific metrics under
  their own `usage_collector_*` prefix per §3.11.6.
- OQ-5 — Whether `flush` accepts a deadline parameter or relies on
  the Plugin Host's operator-tuned drain timeout. **Resolved (no
  flush)**: this reference exposes no plugin-side flush hook;
  graceful shutdown is the Plugin Host's process-level lifecycle
  responsibility, not an SPI call. Plugins that buffer writes
  internally MUST drain on their own `Gear::shutdown` via the
  ToolKit gear lifecycle.
- OQ-6 — Whether trace context is passed as an explicit parameter or
  carried by the ambient task-local span. **Resolved (ambient
  context)**: this reference carries trace context via the active
  `tracing::Span` / OpenTelemetry context — no explicit `TraceContext`
  parameter appears on any SPI method, mirroring the reference plugin
  traits in `credstore`, `authn-resolver`, and `authz-resolver`. The
  Plugin Host opens the per-call span via
  `#[tracing::instrument(...)]` on its own `Service::*` methods before
  dispatching to the trait method; the SPI implementation runs inside
  that ambient span and continues it over the backend dispatch so the
  DESIGN §3.11.5 propagation invariant is satisfied without a
  syntactic parameter.
- OQ-7 — Whether DESIGN §3.11.2 should carve formal Plugin-SPI
  sub-allocations for the batched-ingestion (Method 2) and
  raw-cursor-paginated-query (Method 4) end-to-end envelopes. Today
  §3.11.2 carves sub-budgets only for ingestion (75 ms of 200 ms)
  and aggregated query (425 ms of 500 ms); this reference adopts the
  conservative "treat the SPI fraction as the dominant share with
  ≥ 25 ms reserved for gateway + PDP enforcement + core overhead" pattern
  in the meantime. A formal sub-allocation is a follow-up against
  DESIGN.md and is out of scope for this reference.

## Document Changelog

- **2026-06-08 (amendment)** — Aligned with the ADR-0012 2026-06-08
  amendment. Kind moves from the `gts_id` prefix to a closed
  `UsageKind` enum on the catalog row: the SPI's `UsageType` /
  `CatalogRow` payload now carries `kind: UsageKind` alongside
  `gts_id` and `metadata_fields`. Every catalog `gts_id` derives
  from the reserved abstract base
  `gts.cf.core.uc.usage_record.v1~` (with at least one further
  `~`-separated segment); the prior counter / gauge base type ids
  are removed. The plugin GTS spec id realigns to
  `gts.cf.toolkit.plugins.plugin.v1~cf.core.uc.plugin.v1~`. The
  `usage_type_catalog` row schema gains a `kind TEXT NOT NULL`
  column constrained to `'counter'` / `'gauge'`; the gateway
  validates `kind` as a closed `UsageKind` enum at the serde
  deserialize boundary, and unknown values are rejected there
  (no dedicated SPI error variant). `gts_id` and `kind` are
  independent — there is no "wrong kind for this gts_id" failure
  mode. The plugin stores `kind` verbatim and surfaces it on
  `get_usage_type` / `list_usage_types`. List-usage-types client-side
  counter / gauge selection is now performed by reading
  `UsageType.kind` (was: derived `UsageTypeGtsId` prefix predicates).
- **2026-06-02 (amendment)** — Aligned with the ADR-0012 2026-06-02
  amendment (simplifications 5 and 6, per
  [`./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md`](./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md)
  Amendment block). Replaced the open-but-typed schema surface and
  the trait map on the catalog row with a single
  `metadata_fields: Vec<String>` field (closed, declared list of
  allowed metadata key names; all values typed as String end-to-end);
  removed the per-usage-type schema validator compile and the schema
  runtime dependency from the gateway.
  The catalog row no longer carries a `kind` column — the counter /
  gauge discriminator is derived from the `gts_id` prefix matching
  one of a pair of reserved base type prefixes (one each for counter
  and gauge); identifiers that do not begin with one of those prefixes
  are rejected at the `UsageTypeGtsId::new` boundary (REST: handler
  synthesises the canonical `invalid_base_gts_id` `Problem` envelope
  at HTTP `400` from the failed conversion on
  `CreateUsageTypeRequest::gts_id`; SDK:
  `UsageCollectorError::Validation` from `UsageTypeGtsId::new`) and
  never reach `create_usage_type`. (Superseded by the 2026-06-08
  amendment — kind moves to a closed `UsageKind` enum on the catalog
  row, and every `gts_id` derives from
  `gts.cf.core.uc.usage_record.v1~`.)
  Added the `UnknownMetadataKey { gts_id, key }` error variant for
  ingest-time closed-shape membership violation; removed the
  schema-validation error surface. The gateway resolves the catalog
  row per call via `get_usage_type` and (at the time of this entry)
  derives `kind` at the call site from the `gts_id` prefix; the
  keyspace is flat.
- **2026-06-02** — Aligned with ADR 0012 ([`./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md`](./ADR/0012-unified-plugin-catalog-and-gts-id-reference.md)).
  Catalog SPI methods are `create_usage_type` / `get_usage_type` /
  `list_usage_types` / `delete_usage_type`; the pre-amendment catalog
  row is reduced to `gts_id` (PK) / the open-but-typed schema surface
  / the trait map; the prior catalog-row fields for
  ancestor-pointer, abstract / non-abstract distinction, type-uuid,
  type-id, and per-property indexable annotation are out of the SPI
  surface; usage records reference usage types by `gts_id`. Stated
  explicitly that the in-plugin reference scheme (column type, index
  choice) is the plugin author's choice and out of SPI scope.
  Preserved unrelated SPI behaviour: per-usage-type declared-key
  surface, lifecycle ops, idempotency tuple (keyed by `gts_id`),
  consistency-contract calls, late-arrival semantics. (This entry was
  subsequently superseded in part by the 2026-06-02 amendment entry
  above.)
