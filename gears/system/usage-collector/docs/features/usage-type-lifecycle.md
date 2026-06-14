# Feature: Usage Type Lifecycle

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
  - [1.5 Explicit Non-Applicability](#15-explicit-non-applicability)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Register UsageType](#register-usagetype)
  - [Delete UsageType](#delete-usagetype)
  - [List UsageTypes](#list-usagetypes)
  - [Read UsageType](#read-usagetype)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [UsageType Shape Validation](#usagetype-shape-validation)
  - [Ingest Metadata Validation](#ingest-metadata-validation)
- [4. States (CDSL)](#4-states-cdsl)
  - [UsageType Registration Lifecycle State Machine](#usagetype-registration-lifecycle-state-machine)
- [5. Definitions of Done](#5-definitions-of-done)
  - [FR: UsageType Registration](#fr-usagetype-registration)
  - [FR: UsageType Deletion](#fr-usagetype-deletion)
  - [FR: Counter Semantics](#fr-counter-semantics)
  - [FR: Gauge Semantics](#fr-gauge-semantics)
  - [NFR: Availability](#nfr-availability)
  - [Principle: Semantics Enforcement](#principle-semantics-enforcement)
  - [Constraint: No Business Logic](#constraint-no-business-logic)
  - [Component: UsageType Catalog](#component-usagetype-catalog)
  - [Sequence: Register UsageType](#sequence-register-usagetype)
  - [Sequence: Delete UsageType](#sequence-delete-usagetype)
  - [Entity: UsageType](#entity-usagetype)
  - [API: POST /usage-collector/v1/usage-types](#api-post-usage-collectorv1usage-types)
  - [API: DELETE /usage-collector/v1/usage-types/{gts_id}](#api-delete-usage-collectorv1usage-typesgts_id)
  - [API: GET /usage-collector/v1/usage-types](#api-get-usage-collectorv1usage-types)
  - [API: GET /usage-collector/v1/usage-types/{gts_id}](#api-get-usage-collectorv1usage-typesgts_id)
  - [Error Mapping: SPI → REST / SDK](#error-mapping-spi--rest--sdk)
  - [§2.2-item → DoD-ID Coverage Matrix](#22-item--dod-id-coverage-matrix)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-featstatus-usage-type-lifecycle`

<!-- reference to DECOMPOSITION entry -->

- [ ] `p1` - `cpt-cf-usage-collector-feature-usage-type-lifecycle`

## 1. Feature Context

### 1.1 Overview

Provides the operator-driven lifecycle for UsageType definitions — register, list, get, and delete. The platform-global usage-type catalog, persisted in the active storage plugin's database and managed via the Plugin SPI, exists as a single authoritative surface that the ingestion path consults for kind-and-existence enforcement and the query path consults for UsageType validation, with PDP-gated mutations.

### 1.2 Purpose

This feature exists so the operator-controlled usage-type catalog is the single authoritative surface for UsageType existence, UsageType semantics (carried by the closed `kind: UsageKind` enum), and the closed `metadata_fields` declaration across the gear: registration and deletion are gated by per-component PDP enforcement so only authorized platform operators can mutate the catalog; UsageType existence and `metadata_fields` lookups on the ingestion hot path dispatch a `get_usage_type` SPI call against the bound storage plugin per record.

**Requirements**: `cpt-cf-usage-collector-fr-usage-type-registration`, `cpt-cf-usage-collector-fr-usage-type-deletion`, `cpt-cf-usage-collector-fr-counter-semantics`, `cpt-cf-usage-collector-fr-gauge-semantics`, `cpt-cf-usage-collector-nfr-availability`

**Principles**: `cpt-cf-usage-collector-principle-semantics-enforcement`

### 1.3 Actors

| Actor                                             | Role in Feature                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| ------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `cpt-cf-usage-collector-actor-platform-operator`  | Registers and deletes UsageType definitions via either the REST surface (`POST /usage-collector/v1/usage-types`, `DELETE /usage-collector/v1/usage-types/{gts_id}`) or the SDK trait methods `UsageCollectorClientV1::create_usage_type` / `delete_usage_type`; sole mutator of the platform-global usage-type catalog, gated by PDP authorization                                                                                                                                                                                                                                                                                                                   |
| `cpt-cf-usage-collector-actor-platform-developer` | Consumes the catalog read surface via either REST (`GET /usage-collector/v1/usage-types`, `GET /usage-collector/v1/usage-types/{gts_id}`) or the SDK trait methods `UsageCollectorClientV1::list_usage_types` / `get_usage_type` for UsageType existence, UsageType semantics (carried by the closed `kind: UsageKind` enum), and `metadata_fields` discovery during caller integration; in-process consumption on the ingestion hot path dispatches `get_usage_type` directly against `cpt-cf-usage-collector-contract-storage-plugin` per record |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) -- UsageType Existence and Semantics Enforcement §5.7, UsageType Registration §5.7, UsageType Deletion §5.7, Counter §5.2, Gauge §5.2, Authorization Enforcement §6.1, High Availability §6.1
- **Design**: [DESIGN.md](../DESIGN.md) -- UsageType Catalog component (§3.2), Register / Delete UsageType sequences (§3.6), the SPI catalog-row payload shape (`plugin-spi.md` §"Domain Model" `CatalogRow`), PRD→DESIGN realization for fr-usage-type-registration / fr-usage-type-deletion / fr-counter-semantics / fr-gauge-semantics (§5.3)
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md) -- §2.2 UsageType Catalog & Lifecycle
- **Foundation feature**: [foundation.md](./foundation.md) -- SecurityContext acceptance at the REST surface (`Extension<SecurityContext>` from ToolKit gateway middleware via `OperationBuilder::authenticated()`) and at the SDK trait surface (`&SecurityContext` argument), PDP enforcement via the per-component `authorize` helper, plugin host, gateway-resident auxiliary DB binding (`DBProvider<UsageCollectorError>`), audit-correlation, tenant isolation (reused, not re-defined); the durable `usage_type_catalog` table itself lives in the plugin's backend database
- **Plugin SPI reference**: [plugin-spi.md](../plugin-spi.md) -- the catalog SoR lives in the plugin; catalog write/read/list/delete SPI methods carry the `gts_id` + `kind: UsageKind` + `metadata_fields` payload (counter / gauge classification is the closed `kind` enum on the catalog row, first-class on the SPI payload — see ADR 0012 and the SDK `UsageType` struct in `usage-collector-sdk/src/models.rs`).
- **SDK trait reference**: [sdk-trait.md](../sdk-trait.md) -- `UsageCollectorClientV1::create_usage_type` / `delete_usage_type` / `list_usage_types` / `get_usage_type` and the flat `UsageCollectorError` variants `UsageTypeAlreadyExists`, `UsageTypeNotFound`, `UsageTypeReferenced`, `UnknownMetadataKey` (kind-prefix violations on a candidate `gts_id` are caught at the `UsageTypeGtsId` newtype boundary and surface as a typed validation error; the REST register handler lifts the same failure into the canonical `invalid_base_gts_id` `Problem` envelope at the boundary)
- **REST contract**: [usage-collector-v1.yaml](../usage-collector-v1.yaml) -- `/usage-collector/v1/usage-types` paths keyed by `{gts_id}`
- **ADR references**: [ADR/0012-unified-plugin-catalog-and-gts-id-reference.md](../ADR/0012-unified-plugin-catalog-and-gts-id-reference.md) -- `cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference` (supersedes ADR 0007 / 0009 / 0010; see §7 Changelog)
- **Dependencies**: `cpt-cf-usage-collector-feature-foundation`

### 1.5 Explicit Non-Applicability

- **UX** (`UX-FDESIGN-001` user journey, `UX-FDESIGN-002` accessibility): Not applicable because the usage-type-lifecycle feature is a backend surface reachable via both the public REST contract (`POST/DELETE/GET /usage-collector/v1/usage-types`) and the in-process SDK trait `UsageCollectorClientV1::create_usage_type` / `delete_usage_type` / `list_usage_types` / `get_usage_type`, and is consumed by callers in-process via per-record `get_usage_type` SPI round-trips against `cpt-cf-usage-collector-contract-storage-plugin`; there is no human-facing UI in this gear, and any UI consumption of UsageType definitions is delivered by upstream products outside this scope. User-friendliness on the operator surface is encoded through the deterministic `Problem` error envelopes published by `usage-collector-v1.yaml` (REST) and the flat `UsageCollectorError` enum (SDK).

## 2. Actor Flows (CDSL)

### Register UsageType

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type`

**Actor**: `cpt-cf-usage-collector-actor-platform-operator`

**Success Scenarios**:

- A platform operator submits a UsageType definition (`gts_id` ending `~` and deriving from the reserved abstract base `gts.cf.core.uc.usage_record.v1~` with at least one further `~`-separated segment, `kind: UsageKind` — closed counter / gauge enum, and a closed `metadata_fields: array<string>` declaring the allowed metadata key names) either via `POST /usage-collector/v1/usage-types` or via the SDK trait method `UsageCollectorClientV1::create_usage_type`; the REST handler receives `Extension<SecurityContext>` populated by ToolKit gateway middleware (`OperationBuilder::authenticated()`) (or, on the SDK path, the in-process trait impl receives `&SecurityContext` directly) and delegates to the gateway UsageType Catalog service, the gateway service invokes the per-component `authorize` helper (`PolicyEnforcer::access_scope_with(ctx, ...)` against `cpt-cf-usage-collector-contract-authz-resolver`) to authorize the register operation, the gateway validates the `metadata_fields` shape (the `gts_id` base-derivation check is owned upstream by `UsageTypeGtsId::new` on the REST handler's `String` `gts_id` field or by the typed `UsageTypeGtsId` argument on the SDK path; `kind` is validated as a closed `UsageKind` enum at the serde deserialize boundary), the gateway dispatches the catalog write through `cpt-cf-usage-collector-contract-storage-plugin` (the plugin persists the new row in `usage_type_catalog` inside a transaction with the unique constraint on `gts_id` rejecting duplicates, and the row carries `kind` and `metadata_fields` verbatim), and the canonical `UsageType` resource is returned with a `Location` header per `usage-collector-v1.yaml`.

**Error Scenarios**:

- PDP denies the register operation — propagated platform-authorization error envelope from `cpt-cf-usage-collector-flow-foundation-pdp-authorize` (per-component `authorize` helper against `cpt-cf-usage-collector-contract-authz-resolver`); no catalog mutation occurs.
- Type shape is invalid in one of three distinct ways: (a) the supplied `gts_id` does not derive from the reserved abstract base `gts.cf.core.uc.usage_record.v1~`, is empty, or is missing a derivation segment after the reserved base — the REST handler lifts the failed `UsageTypeGtsId::new` conversion into the canonical `invalid_base_gts_id` `Problem` envelope (HTTP `400`, `context.reason="invalid_base_gts_id"`, `context.gts_id` echoes the rejected identifier, `context.instance_path="/gts_id"`) before any plugin dispatch; (b) `kind` carries an unknown value (anything other than `"counter"` or `"gauge"`) — closed-enum serde rejection at the deserialize boundary returns a structured `400` before any handler code runs; (c) `metadata_fields` is malformed (missing, not an array of strings, contains empty strings, or contains duplicates) — actionable fields-validation envelope (HTTP `400`, `context.reason="invalid_metadata_fields"`) is returned before any plugin dispatch.
- Duplicate `gts_id` already present in the plugin's `usage_type_catalog` table — the unique constraint surfaces a `UsageTypeAlreadyExists` SPI error which the gateway returns as an actionable conflict error envelope (HTTP `409`).
- Plugin transport or persistence failure — propagated as a deterministic platform error envelope (HTTP `503` for `PluginUnavailable` / `TypesRegistryUnavailable` / `ServiceUnavailable` (plugin-side `Transient` and host-side per-call deadline expirations both lift to `ServiceUnavailable`); HTTP `500` for plugin `BackendError` (lifted to `Internal`)); no synthesized UsageType handle.

**Steps**:

1. [ ] - `p1` - Operator submits the register request via either `POST /usage-collector/v1/usage-types` or `UsageCollectorClientV1::create_usage_type` (SDK trait); both surfaces accept a resolved `SecurityContext` (REST: `Extension<SecurityContext>` populated by ToolKit gateway middleware via `OperationBuilder::authenticated()`; SDK: `&SecurityContext` argument) and W3C audit-correlation context, and both converge on the same gateway domain service - `inst-register-usage-type-submit`
2. [ ] - `p1` - Delegate to the gateway UsageType Catalog service (the REST handler and the SDK trait impl share the same domain service entry point), passing the inbound `SecurityContext` and the register-operation payload (`gts_id`, `metadata_fields`) - `inst-register-usage-type-service-call`
3. [ ] - `p1` - Inside the gateway service, invoke `cpt-cf-usage-collector-flow-foundation-pdp-authorize` via the per-component `authorize` helper (`PolicyEnforcer::access_scope_with(ctx, ...)` against `cpt-cf-usage-collector-contract-authz-resolver`) with the register-operation attribution tuple and receive (`PdpDecision`, `PdpConstraint` set) - `inst-register-usage-type-pdp`
4. [ ] - `p1` - **IF** the PDP decision is deny **RETURN** the propagated platform-authorization error envelope (HTTP `403` per the yaml `Problem` response shape) - `inst-register-usage-type-pdp-deny`
5. [ ] - `p1` - Invoke `cpt-cf-usage-collector-algo-usage-type-lifecycle-usage-type-shape-validation` against the submitted payload (`metadata_fields` shape — array of unique non-empty strings). `gts_id` form and kind-prefix membership are pre-validated at the `UsageTypeGtsId::new` boundary (the REST handler runs the conversion on the permissive `CreateUsageTypeRequest::gts_id` DTO field before this step; the SDK trait's typed `gts_id: UsageTypeGtsId` argument carries the same guarantee), so this algorithm sees only well-typed identifiers - `inst-register-usage-type-validate-shape`
6. [ ] - `p1` - **IF** the shape-validation algorithm reports invalid **RETURN** the actionable `InvalidMetadataField` / `DuplicateMetadataField` envelope (HTTP `400`, `context.reason="invalid_metadata_fields"`) before any plugin dispatch - `inst-register-usage-type-invalid-shape`
7. [ ] - `p1` - **TRY** dispatch the catalog write through `cpt-cf-usage-collector-contract-storage-plugin` carrying (`gts_id`, `metadata_fields`); the plugin persists the new row in `usage_type_catalog` (PK `gts_id`) inside a transaction with `metadata_fields` stored as a typed array of strings - `inst-register-usage-type-spi-insert`
8. [ ] - `p1` - **CATCH** plugin SPI error - `inst-register-usage-type-spi-catch`
   1. [ ] - `p1` - **IF** the error is `UsageTypeAlreadyExists` (the plugin's unique constraint on `gts_id` fired) **RETURN** the actionable conflict error envelope (HTTP `409`, `context.reason="usage_type_already_exists"` injected by `register_error_to_problem`, plus `context.gts_id` echoing the offending identifier) - `inst-register-usage-type-duplicate`
   2. [ ] - `p1` - **ELSE** (transport / availability / persistence error from the plugin) **RETURN** the propagated platform-error envelope (HTTP `503` for `PluginUnavailable` / `TypesRegistryUnavailable` / `ServiceUnavailable` (plugin-side `Transient` and host-side per-call deadline expirations both lift to `ServiceUnavailable`); HTTP `500` for plugin `BackendError` (lifted to `Internal`)) - `inst-register-usage-type-spi-fail`
9. [ ] - `p1` - **RETURN** HTTP `201` with the canonical `UsageType` resource body (as the `UsageTypeDto` REST projection) and `Location: /usage-collector/v1/usage-types/{gts_id}` per `usage-collector-v1.yaml` (or, on the SDK path, return `Ok(UsageType)` to the trait caller) - `inst-register-usage-type-return`

**Acceptance Scenarios (Given-When-Then)**:

- **Given** the catalog has no row for the proposed `gts_id`, **when** an operator calls `POST /usage-collector/v1/usage-types` with `gts_id` ending `~` and deriving from the reserved abstract base `gts.cf.core.uc.usage_record.v1~`, plus `kind: UsageKind` (counter / gauge) and a valid closed `metadata_fields: array<string>`, **then** the gateway runs PDP and `metadata_fields` shape-validation (both pass), the plugin persists the new row in `usage_type_catalog` carrying `gts_id`, `kind`, and `metadata_fields`, and HTTP `201` is returned with the canonical `UsageType` resource (as `UsageTypeDto`) and a `Location` header.
- **Given** the catalog already contains a row keyed by the submitted `gts_id`, **when** an operator calls `POST /usage-collector/v1/usage-types` with the same `gts_id`, **then** the plugin's unique constraint on `gts_id` fires, the gateway returns HTTP `409` with `Problem.context.reason="usage_type_already_exists"` and `Problem.context.gts_id` carrying the offending identifier (injected by `register_error_to_problem`), and no row mutation occurs.

### Delete UsageType

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type`

**Actor**: `cpt-cf-usage-collector-actor-platform-operator`

**Success Scenarios**:

- A platform operator submits the delete request via either `DELETE /usage-collector/v1/usage-types/{gts_id}` or the SDK trait method `UsageCollectorClientV1::delete_usage_type` (both surfaces converge on the same gateway domain service); the gateway UsageType Catalog service authorizes the delete via the per-component `authorize` helper against `cpt-cf-usage-collector-contract-authz-resolver`, dispatches the catalog delete through `cpt-cf-usage-collector-contract-storage-plugin`, the plugin's in-database `ON DELETE RESTRICT` foreign key from `usage_records.gts_id` to `usage_type_catalog(gts_id)` confirms zero references and the plugin removes the row inside the same transaction, and `204 No Content` (REST) or `Ok(())` (SDK) is returned.

**Error Scenarios**:

- Request arrives without a resolved `SecurityContext` (gateway middleware rejected upstream) — the canonical `Unauthenticated` `Problem` envelope is returned by the gateway; no catalog mutation occurs.
- PDP denies the delete operation — propagated platform-authorization error envelope from `cpt-cf-usage-collector-flow-foundation-pdp-authorize` (per-component `authorize` helper against `cpt-cf-usage-collector-contract-authz-resolver`).
- UsageType `gts_id` not present in the plugin's `usage_type_catalog` table — the plugin surfaces `UsageTypeNotFound` and the gateway returns the canonical not-found `Problem` envelope (HTTP `404`, `resource.type="usage_type"`, `resource.id=<gts_id>`; the canonical envelope does not carry a top-level `Problem.context.reason` here).
- Plugin's FK rejects the delete because at least one `usage_records` row references the target `gts_id` — the plugin surfaces `UsageTypeReferenced { gts_id, sample_ref_count }` and the gateway returns the canonical conflict `Problem` envelope (HTTP `409`, `reason="USAGE_TYPE_REFERENCED"`, `resource.type="usage_type"`, `resource.id=<gts_id>`, `detail` carrying the human-readable reference count); `sample_ref_count` is conveyed in the `detail` string only and is NOT exposed as a structured `context.sample_ref_count` field (referential-delete).
- Plugin transport or persistence failure — propagated as a deterministic platform error envelope (HTTP `503` for `PluginUnavailable` / `TypesRegistryUnavailable` / `ServiceUnavailable` (plugin-side `Transient` and host-side per-call deadline expirations both lift to `ServiceUnavailable`); HTTP `500` for plugin `BackendError` (lifted to `Internal`)).

**Steps** (the referential-delete protocol):

1. [ ] - `p1` - Operator submits the delete request via either `DELETE /usage-collector/v1/usage-types/{gts_id}` or `UsageCollectorClientV1::delete_usage_type` (SDK trait); both surfaces accept a resolved `SecurityContext` and W3C audit-correlation context, both converge on the same gateway domain service, and the gateway service invokes `cpt-cf-usage-collector-flow-foundation-pdp-authorize` via the per-component `authorize` helper (`PolicyEnforcer::access_scope_with(ctx, ...)` against `cpt-cf-usage-collector-contract-authz-resolver`) with the delete-operation attribution tuple; **IF** the PDP decision is deny **RETURN** the propagated platform-authorization error envelope (HTTP `403`) - `inst-delete-usage-type-pdp-authorize`
2. [ ] - `p1` - Dispatch the catalog delete through `cpt-cf-usage-collector-contract-storage-plugin` carrying the target `gts_id`; the plugin enters a transaction, the `ON DELETE RESTRICT` foreign key on `usage_records.gts_id` → `usage_type_catalog(gts_id)` either allows the row removal (zero references) or rejects the delete with a structured `UsageTypeReferenced` error inside the same transaction - `inst-delete-usage-type-spi-dispatch`
3. [ ] - `p1` - **CATCH** plugin SPI error - `inst-delete-usage-type-spi-catch`
   1. [ ] - `p1` - **IF** the error is `UsageTypeNotFound` (no `usage_type_catalog` row exists for the target `gts_id`) **RETURN** the canonical not-found `Problem` envelope (HTTP `404`, `resource.type="usage_type"`, `resource.id=<gts_id>`) - `inst-delete-usage-type-not-found`
   2. [ ] - `p1` - **IF** the error is `UsageTypeReferenced { gts_id, sample_ref_count }` (the plugin's FK rejected the delete because at least one `usage_records` row references the `gts_id`) **RETURN** the canonical conflict `Problem` envelope (HTTP `409`, `reason="USAGE_TYPE_REFERENCED"`, `resource.type="usage_type"`, `resource.id=<gts_id>`, `detail` carrying the human-readable reference count); the `sample_ref_count` is conveyed in `detail` only and is NOT exposed as a structured `context.sample_ref_count` field - `inst-delete-usage-type-referenced`
   3. [ ] - `p1` - **ELSE** (transport / availability / persistence error from the plugin) **RETURN** the propagated platform-error envelope (HTTP `503` for `PluginUnavailable` / `TypesRegistryUnavailable` / `ServiceUnavailable`; HTTP `500` for plugin `BackendError` lifted to `Internal`) - `inst-delete-usage-type-spi-fail`
4. [ ] - `p1` - On a successful delete, **RETURN** HTTP `204 No Content` (REST) or `Ok(())` (SDK trait) per `usage-collector-v1.yaml` - `inst-delete-usage-type-spi-delete-return`

**Acceptance Scenarios (Given-When-Then)**:

- **Given** the plugin's `usage_type_catalog` table holds a row for `gts_id = G` and the plugin's `usage_records` table holds zero rows whose `gts_id = G`, **when** an operator calls `DELETE /usage-collector/v1/usage-types/{gts_id}` (or `UsageCollectorClientV1::delete_usage_type`) for `G`, **then** the gateway runs PDP (passes), the plugin's FK confirms zero references and removes the row inside the same transaction, HTTP `204 No Content` (REST) or `Ok(())` (SDK) is returned, and no other row in either table is mutated.
- **Given** the plugin's `usage_type_catalog` table holds a row for `gts_id = G` and the plugin's `usage_records` table holds at least one row whose `gts_id = G`, **when** an operator calls `DELETE /usage-collector/v1/usage-types/{gts_id}` (or `UsageCollectorClientV1::delete_usage_type`) for `G`, **then** the plugin's `ON DELETE RESTRICT` foreign key rejects the delete inside the same transaction and surfaces `UsageTypeReferenced { gts_id: G, sample_ref_count }` to the gateway, the gateway returns the canonical conflict `Problem` envelope (HTTP `409`, `reason="USAGE_TYPE_REFERENCED"`, `resource.type="usage_type"`, `resource.id=G`, `detail` carrying the human-readable reference count), no `usage_type_catalog` row is removed, and no `usage_records` row is mutated.

### List UsageTypes

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types`

**Actor**: `cpt-cf-usage-collector-actor-platform-developer`

**Success Scenarios**:

- A platform developer (any authorized REST caller, or any in-process SDK consumer) submits the list request via either `GET /usage-collector/v1/usage-types` with optional `limit` and `cursor` paging parameters or `UsageCollectorClientV1::list_usage_types` (SDK trait) with a `&ODataQuery` argument carrying the same optional `limit` and `cursor`; both surfaces converge on the same gateway domain service, the gateway service authorizes the read via the per-component `authorize` helper against `cpt-cf-usage-collector-contract-authz-resolver`, the page is composed from a paginated catalog read dispatched through `cpt-cf-usage-collector-contract-storage-plugin` against the plugin's `usage_type_catalog` table, and a `toolkit_odata::Page<UsageType>` is returned (REST projects each item to `UsageTypeDto` for the wire response; SDK returns the SDK-side `UsageType` directly) per `usage-collector-v1.yaml`.

**Error Scenarios**:

- Request arrives without a resolved `SecurityContext` (gateway middleware rejected upstream) — the canonical `Unauthenticated` `Problem` envelope is returned by the gateway.
- PDP denies the read operation — propagated platform-authorization error envelope from `cpt-cf-usage-collector-flow-foundation-pdp-authorize` (per-component `authorize` helper against `cpt-cf-usage-collector-contract-authz-resolver`).
- Plugin SPI transport or persistence failure on the paginated catalog read — propagated as a deterministic platform error envelope (HTTP `503` for `PluginUnavailable` / `TypesRegistryUnavailable` / `ServiceUnavailable` (plugin-side `Transient` and host-side per-call deadline expirations both lift to `ServiceUnavailable`); HTTP `500` for plugin `BackendError` (lifted to `Internal`)).

**Steps**:

1. [ ] - `p1` - Caller submits the list request via either `GET /usage-collector/v1/usage-types` with optional `limit` and `cursor` paging parameters per the yaml schema or `UsageCollectorClientV1::list_usage_types(ctx, &ODataQuery)` (SDK trait), with the `ODataQuery` carrying the same optional `limit` and `cursor`; both surfaces accept a resolved `SecurityContext` and W3C audit-correlation context - `inst-list-usage-types-submit`
2. [ ] - `p1` - Delegate to the gateway UsageType Catalog service (the REST handler and the SDK trait impl share the same domain service entry point), passing the inbound `SecurityContext` and the paging parameters - `inst-list-usage-types-service-call`
3. [ ] - `p1` - Inside the gateway service, invoke `cpt-cf-usage-collector-flow-foundation-pdp-authorize` via the per-component `authorize` helper (`PolicyEnforcer::access_scope_with(ctx, ...)` against `cpt-cf-usage-collector-contract-authz-resolver`) with the read-operation attribution tuple and receive (`PdpDecision`, `PdpConstraint` set) - `inst-list-usage-types-pdp`
4. [ ] - `p1` - **IF** the PDP decision is deny **RETURN** the propagated platform-authorization error envelope (HTTP `403`) - `inst-list-usage-types-pdp-deny`
5. [ ] - `p1` - Dispatch the paginated catalog read through `cpt-cf-usage-collector-contract-storage-plugin` against the plugin's `usage_type_catalog` table and compose the requested page from the returned rows - `inst-list-usage-types-plugin-read`
6. [ ] - `p1` - **RETURN** HTTP `200` with the populated `toolkit_odata::Page<UsageTypeDto>` envelope (next-cursor included via `page_info.next_cursor` per `usage-collector-v1.yaml`) on the REST path, or `Ok(toolkit_odata::Page<UsageType>)` on the SDK path - `inst-list-usage-types-return`

### Read UsageType

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type`

**Actor**: `cpt-cf-usage-collector-actor-platform-developer`

**Success Scenarios**:

- A platform developer (any authorized REST caller, or any in-process SDK consumer) submits the get request via either `GET /usage-collector/v1/usage-types/{gts_id}` or `UsageCollectorClientV1::get_usage_type(ctx, gts_id)` (SDK trait); both surfaces converge on the same gateway domain service, the gateway service authorizes the read via the per-component `authorize` helper against `cpt-cf-usage-collector-contract-authz-resolver`, a catalog get is dispatched through `cpt-cf-usage-collector-contract-storage-plugin`, and the canonical `UsageType` resource is returned (REST projects to `UsageTypeDto` for the wire response; SDK returns the SDK-side `UsageType` directly) per `usage-collector-v1.yaml`.

**Error Scenarios**:

- Request arrives without a resolved `SecurityContext` (gateway middleware rejected upstream) — the canonical `Unauthenticated` `Problem` envelope is returned by the gateway.
- PDP denies the read operation — propagated platform-authorization error envelope from `cpt-cf-usage-collector-flow-foundation-pdp-authorize` (per-component `authorize` helper against `cpt-cf-usage-collector-contract-authz-resolver`).
- The requested `gts_id` is absent from the `usage_type_catalog` table — the canonical not-found `Problem` envelope (HTTP `404`, `resource.type="usage_type"`, `resource.id=<gts_id>`) is returned.
- Plugin SPI transport or persistence failure on the catalog get dispatch — propagated as a deterministic platform error envelope (HTTP `503` for `PluginUnavailable` / `TypesRegistryUnavailable` / `ServiceUnavailable` (plugin-side `Transient` and host-side per-call deadline expirations both lift to `ServiceUnavailable`); HTTP `500` for plugin `BackendError` (lifted to `Internal`)).

**Steps**:

1. [ ] - `p1` - Caller submits the get request via either `GET /usage-collector/v1/usage-types/{gts_id}` or `UsageCollectorClientV1::get_usage_type(ctx, gts_id)` (SDK trait); both surfaces accept a resolved `SecurityContext` and W3C audit-correlation context - `inst-get-usage-type-submit`
2. [ ] - `p1` - Delegate to the gateway UsageType Catalog service (the REST handler and the SDK trait impl share the same domain service entry point), passing the inbound `SecurityContext` and the target `gts_id` - `inst-get-usage-type-service-call`
3. [ ] - `p1` - Inside the gateway service, invoke `cpt-cf-usage-collector-flow-foundation-pdp-authorize` via the per-component `authorize` helper (`PolicyEnforcer::access_scope_with(ctx, ...)` against `cpt-cf-usage-collector-contract-authz-resolver`) with the read-operation attribution tuple (including the target `gts_id`) and receive (`PdpDecision`, `PdpConstraint` set) - `inst-get-usage-type-pdp`
4. [ ] - `p1` - **IF** the PDP decision is deny **RETURN** the propagated platform-authorization error envelope (HTTP `403`) - `inst-get-usage-type-pdp-deny`
5. [ ] - `p1` - Dispatch the catalog get through `cpt-cf-usage-collector-contract-storage-plugin` for the supplied `gts_id` - `inst-get-usage-type-repo-find-by-id`
6. [ ] - `p1` - **IF** the plugin returns `Err(UsageTypeNotFound { gts_id })` (no row for the supplied `gts_id`) the gateway lifts verbatim to `UsageCollectorError::UsageTypeNotFound { gts_id }` and **RETURNS** the canonical not-found `Problem` envelope (HTTP `404`, `resource.type="usage_type"`, `resource.id=<gts_id>`) - `inst-get-usage-type-not-found`
7. [ ] - `p1` - **RETURN** HTTP `200` with the canonical `UsageType` resource projected to `UsageTypeDto` for the wire response (`gts_id` as `String`, `kind: UsageKind`, `metadata_fields: Vec<String>` — every field of the SDK `UsageType` struct, see `usage-collector-sdk/src/models.rs`) per `usage-collector-v1.yaml` (or `Ok(UsageType)` on the SDK path) - `inst-get-usage-type-return`

## 3. Processes / Business Logic (CDSL)

### UsageType Shape Validation

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-algo-usage-type-lifecycle-usage-type-shape-validation`

**Input**: a well-typed `UsageType` (carrying `gts_id: UsageTypeGtsId`, `kind: UsageKind`, and `metadata_fields: Vec<String>`) from `cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type`. `gts_id` is pre-validated by `UsageTypeGtsId::new` upstream (the REST handler runs the conversion on the permissive `CreateUsageTypeRequest::gts_id` DTO field; the SDK trait's typed `gts_id: UsageTypeGtsId` argument carries the same guarantee), so this algorithm sees only identifiers deriving from the reserved abstract base `gts.cf.core.uc.usage_record.v1~` with at least one further `~`-separated segment. `kind` is pre-validated as a closed `UsageKind` enum at the serde deserialize boundary (unknown values rejected before this algorithm runs).

**Output**: `valid` (the payload is ready for plugin dispatch), or a structured `InvalidMetadataField` / `DuplicateMetadataField` envelope (HTTP `400`, `context.reason="invalid_metadata_fields"`) citing the offending `/metadata_fields/{index}` per the `Problem` shape in `usage-collector-v1.yaml`. This algorithm performs structural validation of `metadata_fields` only — duplicate-`gts_id` detection is owned by the plugin's unique constraint on `usage_type_catalog(gts_id)` (surfaced as `UsageTypeAlreadyExists`), NOT by this algorithm. Kind-prefix violations on a candidate `gts_id` are caught earlier at `UsageTypeGtsId::new` and surface as `UsageCollectorError::Validation` on the SDK side / the canonical `invalid_base_gts_id` `Problem` envelope on the REST side; they never reach this algorithm.

**Steps**:

1. [ ] - `p1` - **IF** the `metadata_fields` field contains an empty string or a duplicate, **RETURN** the structured `InvalidMetadataField` / `DuplicateMetadataField` envelope with `context.reason="invalid_metadata_fields"` and `instance_path="/metadata_fields/{index}"` of the offending entry - `inst-algo-shape-invalid-metadata-fields`
2. [ ] - `p1` - **RETURN** `valid`; the payload is ready for plugin dispatch by `cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type` - `inst-algo-shape-return-valid`

### Ingest Metadata Validation

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-algo-usage-type-lifecycle-ingest-metadata-validation`

**Input**: target `gts_id` (resolved from the incoming usage row) and the candidate `metadata` JSON object as supplied by the ingest call.

**Output**: `valid` (every key in the candidate `metadata` is a declared member of the usage type's `metadata_fields`; all values are conveyed as `String`), or a structured `UnknownMetadataKey { gts_id, key }` error lifted to a `Problem` envelope whose `field_violations[*]` carries the offending key (HTTP `400`, `field_violations[0].reason="UNKNOWN_METADATA_KEY"`, `field_violations[0].field="metadata.<key>"`) per the `Problem` shape in `usage-collector-v1.yaml`. The lookup of `metadata_fields` comes from the plugin SoR via a direct `get_usage_type` dispatch against `cpt-cf-usage-collector-contract-storage-plugin`. This validation is **closed-shape**: there is no free-form remainder and undeclared keys are never silently preserved.

**Steps**:

1. [ ] - `p1` - Resolve the `metadata_fields` set for the target `gts_id` by dispatching `get_usage_type(gts_id)` against `cpt-cf-usage-collector-contract-storage-plugin`; **IF** the plugin returns `Err(UsageTypeNotFound { gts_id })` **RETURN** the `UsageTypeNotFound { gts_id }` error (lifted by the ingest flow to the canonical not-found `Problem` envelope, HTTP `404`, `resource.type="usage_type"`, `resource.id=<gts_id>`) so the ingest path can reject the row before any plugin write dispatch — the SDK collapses ingestion-path and catalog-admin misses into the single `UsageTypeNotFound` variant per `error.rs` - `inst-algo-ingest-validate-resolve-fields`
2. [ ] - `p1` - Validate the candidate `metadata` object against the resolved `metadata_fields` by checking that every key in the candidate is a member of the closed `metadata_fields` array; values are accepted as `String` end-to-end - `inst-algo-ingest-validate-closed-shape`
3. [ ] - `p1` - **IF** the candidate carries any key that is not a member of `metadata_fields` **RETURN** `UnknownMetadataKey { gts_id, key }`, lifted by the ingest flow to a canonical `Problem` envelope (HTTP `400`, `field_violations[0].reason="UNKNOWN_METADATA_KEY"`, `field_violations[0].field="metadata"` carrying the offending key in the human-readable detail) so the caller can pinpoint the offending key — note: the offending key is conveyed via `field_violations[*]`, NOT via a top-level `Problem.context.reason="unknown_metadata_key"` discriminator - `inst-algo-ingest-validate-reject`
4. [ ] - `p1` - **RETURN** `valid`; the candidate metadata may now flow to the plugin's usage-record insert path - `inst-algo-ingest-validate-return-valid`

**Acceptance Scenarios (Given-When-Then)**:

- **Given** a registered UsageType `T` whose `metadata_fields = ["region"]`, **when** any caller submits a usage row carrying `gts_id = T` and `metadata = { "region": "eu-west-1" }`, **then** the gateway resolves the declared keys via a plugin SoR round-trip, verifies that every candidate key is a member of `metadata_fields` (passes), and accepts the row for plugin dispatch (closed-shape validation).
- **Given** a registered UsageType `T` whose `metadata_fields = ["region"]`, **when** any caller submits a usage row carrying `gts_id = T` and `metadata = { "region": "eu-west-1", "extra_tag": "x" }`, **then** the gateway returns HTTP `400` with a `Problem` envelope whose `field_violations[0].reason = "UNKNOWN_METADATA_KEY"` and `field_violations[0].field = "metadata.extra_tag"`, no plugin write dispatch occurs, and no `usage_records` row is mutated.

## 4. States (CDSL)

### UsageType Registration Lifecycle State Machine

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-state-usage-type-lifecycle-usage-type-registration-lifecycle`

**States**: `not-registered`, `registered`

**Initial State**: `not-registered`

**Scope note**: This state machine models the lifecycle of a `gts_id` in the unified usage-type catalog persisted in the plugin's database. Registration and deletion go through the REST surface or the SDK trait surface; both surfaces converge on the same gateway domain service which dispatches catalog writes through `cpt-cf-usage-collector-contract-storage-plugin`.

**Transitions**:

1. [ ] - `p1` - **FROM** `not-registered` **TO** `registered` **WHEN** `cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type` completes successfully — the plugin SPI catalog-insert persisted the new row in the plugin's `usage_type_catalog` table inside a transaction (mirrors `inst-register-usage-type-spi-insert`) - `inst-state-usage-type-lifecycle-registered`
2. [ ] - `p1` - **FROM** `registered` **TO** `not-registered` **WHEN** `cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type` completes successfully — the plugin SPI catalog-delete removed the row from the plugin's `usage_type_catalog` table after the `ON DELETE RESTRICT` foreign key confirmed zero `usage_records` references (mirrors `inst-delete-usage-type-spi-delete-return`); the transition is REJECTED while the plugin's FK reports at least one `usage_records` row references the `gts_id` (the plugin returns `UsageTypeReferenced` and the gateway returns the canonical conflict `Problem` envelope at HTTP `409`), and the state remains `registered` - `inst-state-usage-type-lifecycle-not-registered`

## 5. Definitions of Done

### FR: UsageType Registration

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-type-lifecycle-fr-usage-type-registration`

The system **MUST** accept a UsageType definition (`gts_id`, `kind: UsageKind`, `metadata_fields`) on `POST /usage-collector/v1/usage-types` or via the SDK trait method `UsageCollectorClientV1::create_usage_type` (both surfaces converge on the same gateway domain service); reject any `gts_id` that does not derive from the reserved abstract base `gts.cf.core.uc.usage_record.v1~` at the `UsageTypeGtsId::new` boundary (REST: the handler synthesises the canonical `invalid_base_gts_id` `Problem` envelope at HTTP `400` from the failed conversion on `CreateUsageTypeRequest::gts_id` — whose DTO field is a permissive `String`; SDK: `UsageCollectorError::Validation` from `UsageTypeGtsId::new`) and reject any unknown `kind` value via closed-enum serde rejection at the deserialize boundary, both before any plugin dispatch; validate the well-typed payload via `cpt-cf-usage-collector-algo-usage-type-lifecycle-usage-type-shape-validation` for malformed `metadata_fields` before any plugin dispatch, persist the new catalog entry (`gts_id` plus the `kind` enum and the typed `metadata_fields` array of strings — `gts_id` and `kind` are independent) by dispatching the catalog write through `cpt-cf-usage-collector-contract-storage-plugin` inside a transaction, and surface a deterministic `UsageTypeAlreadyExists` envelope (HTTP `409`, with `Problem.context.reason="usage_type_already_exists"` and `Problem.context.gts_id` injected by the POST-only `register_error_to_problem` decorator) when the plugin's unique constraint on `gts_id` fires so retried submissions of the same `gts_id` never produce silent duplication or partial catalog mutation.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type`
- `cpt-cf-usage-collector-algo-usage-type-lifecycle-usage-type-shape-validation`

**Constraints**: `cpt-cf-usage-collector-fr-usage-type-registration`

**Touches**:

- API: `POST /usage-collector/v1/usage-types`
- Entities: `UsageType`

### FR: UsageType Deletion

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-type-lifecycle-fr-usage-type-deletion`

The system **MUST** implement `DELETE /usage-collector/v1/usage-types/{gts_id}` and the SDK trait method `UsageCollectorClientV1::delete_usage_type` (both surfaces converge on the same gateway domain service) per the referential-delete protocol of §"Delete UsageType" — PDP authorize, dispatch the catalog delete through `cpt-cf-usage-collector-contract-storage-plugin` so that: a `UsageTypeNotFound` SPI error ⇒ canonical not-found `Problem` envelope (HTTP `404`, `resource.type="usage_type"`, `resource.id=<gts_id>`); a `UsageTypeReferenced { gts_id, sample_ref_count }` SPI error (the plugin's `ON DELETE RESTRICT` FK on `usage_records.gts_id` rejected the delete because at least one row references the `gts_id`) ⇒ canonical conflict `Problem` envelope (HTTP `409`, `reason="USAGE_TYPE_REFERENCED"`, `resource.type="usage_type"`, `resource.id=<gts_id>`, `detail` carrying the human-readable reference count; `sample_ref_count` is NOT exposed as a structured `context.sample_ref_count` field); a successful row removal ⇒ `204 No Content`. The implementation MUST NOT introduce a tombstone, a state column, or a second transaction.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type`

**Constraints**: `cpt-cf-usage-collector-fr-usage-type-deletion`

**Touches**:

- API: `DELETE /usage-collector/v1/usage-types/{gts_id}`
- Entities: `UsageType`

### FR: Counter Semantics

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-type-lifecycle-fr-counter-semantics`

The system **MUST** classify a UsageType whose registered `kind` is `UsageKind::Counter` as a non-negative delta accumulation UsageType (counter / gauge classification carried by the closed `UsageKind` enum on the catalog row). The §2.3 Usage Emission ingestion path resolves UsageType existence by dispatching `get_usage_type` directly against `cpt-cf-usage-collector-contract-storage-plugin` and reads the counter classification at the call site from `UsageType.kind` via the `UsageType::is_counter` predicate so that the non-negative-delta invariant is enforced without re-implementing semantics locally; counter / gauge classification is carried verbatim as the row's `kind` field on every catalog read.

**Implements**:

- `cpt-cf-usage-collector-algo-usage-type-lifecycle-usage-type-shape-validation`

**Constraints**: `cpt-cf-usage-collector-fr-counter-semantics`

**Touches**:

- API: `POST /usage-collector/v1/usage-types`
- Entities: `UsageType`

### FR: Gauge Semantics

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-type-lifecycle-fr-gauge-semantics`

The system **MUST** classify a UsageType whose registered `kind` is `UsageKind::Gauge` as a point-in-time UsageType stored as-is (counter / gauge classification carried by the closed `UsageKind` enum on the catalog row). The §2.3 Usage Emission ingestion path resolves UsageType existence by dispatching `get_usage_type` directly against `cpt-cf-usage-collector-contract-storage-plugin` and reads the gauge classification at the call site from `UsageType.kind` via the `UsageType::is_gauge` predicate so gauge values are accepted without delta-accumulation rewriting; counter / gauge classification is carried verbatim as the row's `kind` field on every catalog read.

**Implements**:

- `cpt-cf-usage-collector-algo-usage-type-lifecycle-usage-type-shape-validation`

**Constraints**: `cpt-cf-usage-collector-fr-gauge-semantics`

**Touches**:

- API: `POST /usage-collector/v1/usage-types`
- Entities: `UsageType`

### NFR: Availability

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-type-lifecycle-nfr-availability`

The system **MUST** keep the read endpoints `GET /usage-collector/v1/usage-types` and `GET /usage-collector/v1/usage-types/{gts_id}` (and their SDK trait counterparts `UsageCollectorClientV1::list_usage_types` / `get_usage_type`) available whenever the bound storage plugin is available — catalog reads dispatch through `cpt-cf-usage-collector-contract-storage-plugin` against the plugin's `usage_type_catalog` table per call. When the plugin is unavailable, the read endpoints surface deterministic platform-error envelopes (HTTP `503` for `PluginUnavailable` / `TypesRegistryUnavailable` / `ServiceUnavailable` (plugin-side `Transient` and host-side per-call deadline expirations both lift to `ServiceUnavailable`); HTTP `500` for plugin `BackendError` (lifted to `Internal`)) per `usage-collector-v1.yaml`, and the SDK trait surface returns the matching `UsageCollectorError` variant. Plugin binding is lazy (resolved on first dispatch via the `GtsPluginSelector`), and gear init does not block on plugin readiness — the plugin's availability is observed per-call via the `usage_collector.plugin.ready` gauge but is NOT a hard gear-readiness gate that withholds the REST router from the gateway.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types`
- `cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type`

**Constraints**: `cpt-cf-usage-collector-nfr-availability`

**Touches**:

- API: `GET /usage-collector/v1/usage-types`, `GET /usage-collector/v1/usage-types/{gts_id}`
- Entities: `UsageType`

### Principle: Semantics Enforcement

- [x] `p2` - **ID**: `cpt-cf-usage-collector-dod-usage-type-lifecycle-principle-semantics-enforcement`

The system **MUST** enforce UsageType semantics invariants at the catalog boundary: every UsageType registration validates that the supplied `gts_id` derives from the reserved abstract base `gts.cf.core.uc.usage_record.v1~` at the `UsageTypeGtsId::new` boundary before any repo dispatch (REST: the handler synthesises the canonical `invalid_base_gts_id` `Problem` envelope at HTTP `400` from the failed conversion on `CreateUsageTypeRequest::gts_id` — whose DTO field is a permissive `String`; SDK: `UsageCollectorError::Validation` returned by `UsageTypeGtsId::new`) and validates `kind` as a closed `UsageKind` enum at the serde deserialize boundary (unknown values rejected before any repo dispatch), and every read-side consumer obtains catalog membership for a `gts_id` exclusively by dispatching `get_usage_type` against `cpt-cf-usage-collector-contract-storage-plugin` and reads counter / gauge classification at the call site from the catalog row's `kind` field via the `UsageType::is_counter` / `UsageType::is_gauge` predicates so that ingestion (`cpt-cf-usage-collector-fr-counter-semantics`, `cpt-cf-usage-collector-fr-gauge-semantics`) and aggregation-query validation never re-implement classification semantics locally.

**Implements**:

- `cpt-cf-usage-collector-algo-usage-type-lifecycle-usage-type-shape-validation`

**Constraints**: `cpt-cf-usage-collector-principle-semantics-enforcement`

**Touches**:

- API: `POST /usage-collector/v1/usage-types`, `GET /usage-collector/v1/usage-types`, `GET /usage-collector/v1/usage-types/{gts_id}`
- Entities: `UsageType`

### Constraint: No Business Logic

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-dod-usage-type-lifecycle-constraint-no-business-logic`

The system **MUST** keep the UsageType Catalog free of **per-UsageType business-rule fields**: the catalog entry carries `gts_id`, `kind`, and `metadata_fields` (closed `array<string>` of declared metadata key names) with no tenant scoping, and **MUST NOT** introduce accounting / billing / per-UsageType value-rule fields. Counter / gauge classification is carried by the closed `kind: UsageKind` enum; it is a classification discriminator, not a business-rule field. Carrying a closed `metadata_fields` declaration is **metadata-shape typing**, not business logic — it constrains payload shape. Every per-UsageType business rule (counter/gauge value enforcement on the ingestion path, accounting interpretation, billing transforms) remains owned by callers and downstream consumers — never by the catalog.

**Implements**:

- `cpt-cf-usage-collector-algo-usage-type-lifecycle-usage-type-shape-validation`
- `cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type`

**Constraints**: `cpt-cf-usage-collector-constraint-no-business-logic`

**Touches**:

- API: `POST /usage-collector/v1/usage-types`
- Entities: `UsageType`

### Component: UsageType Catalog

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-dod-usage-type-lifecycle-component-usage-type-catalog`

The system **MUST** realize `cpt-cf-usage-collector-component-usage-type-catalog` as the sole owner of the unified UsageType catalog — a single catalog persisted in the plugin's `usage_type_catalog` table reached through `cpt-cf-usage-collector-contract-storage-plugin` — plus the UsageType lifecycle entry points (register, list, get, delete) reachable via REST and the SDK trait `UsageCollectorClientV1`. All catalog reads and writes dispatch through the Plugin SPI per call; the plugin SoR is the single source of truth for both the operator-facing catalog endpoints and the ingestion-path UsageType lookup (the ingestion path dispatches `get_usage_type` against the storage plugin SPI per record). On every catalog delete the component MUST surface the plugin's `UsageTypeReferenced { gts_id, sample_ref_count }` SPI error as the canonical conflict `Problem` envelope (HTTP `409`, `reason="USAGE_TYPE_REFERENCED"`, `resource.type="usage_type"`, `resource.id=<gts_id>`, `detail` carrying the human-readable reference count).

**Implements**:

- `cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type`
- `cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type`
- `cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types`
- `cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type`

**Constraints**: `cpt-cf-usage-collector-component-usage-type-catalog`

**Touches**:

- API: `POST /usage-collector/v1/usage-types`, `DELETE /usage-collector/v1/usage-types/{gts_id}`, `GET /usage-collector/v1/usage-types`, `GET /usage-collector/v1/usage-types/{gts_id}`
- Entities: `UsageType`

### Sequence: Register UsageType

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-type-lifecycle-seq-register-usage-type`

The system **MUST** implement the `cpt-cf-usage-collector-seq-register-usage-type` sequence end-to-end on both the REST and the SDK trait surfaces (the REST handler accepts `Extension<SecurityContext>` from ToolKit gateway middleware and the SDK trait method `UsageCollectorClientV1::create_usage_type` accepts `&SecurityContext` directly; both surfaces converge on the same gateway domain service) → gateway UsageType Catalog service (per-component `authorize` helper against `cpt-cf-usage-collector-contract-authz-resolver` + closed `metadata_fields` array validation; the `gts_id` kind-prefix check is owned upstream by `UsageTypeGtsId::new`) → catalog-insert dispatched through `cpt-cf-usage-collector-contract-storage-plugin` against the plugin's `usage_type_catalog` table inside a transaction (the unique constraint on `gts_id` enforces deduplication, surfaced as `UsageTypeAlreadyExists`), with PDP denial, kind-prefix rejection at the `UsageTypeGtsId::new` boundary (REST: `400` canonical `Problem` envelope with `context.reason="invalid_base_gts_id"`; SDK: `UsageCollectorError::Validation`), malformed `metadata_fields`, and unique-constraint duplicate outcomes rejecting the call before or at the plugin boundary and the successful path returning the canonical `UsageType` resource with a `Location` header on the REST path or `Ok(UsageType)` on the SDK path per `usage-collector-v1.yaml`.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type`
- `cpt-cf-usage-collector-algo-usage-type-lifecycle-usage-type-shape-validation`

**Constraints**: `cpt-cf-usage-collector-seq-register-usage-type`

**Touches**:

- API: `POST /usage-collector/v1/usage-types`
- Entities: `UsageType`

### Sequence: Delete UsageType

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-type-lifecycle-seq-delete-usage-type`

The system **MUST** implement the `cpt-cf-usage-collector-seq-delete-usage-type` sequence end-to-end on both the REST and the SDK trait surfaces (`DELETE /usage-collector/v1/usage-types/{gts_id}` and `UsageCollectorClientV1::delete_usage_type`; both surfaces converge on the same gateway domain service) per the referential-delete protocol: PDP authorize → dispatch the catalog delete through `cpt-cf-usage-collector-contract-storage-plugin` → `UsageTypeNotFound` SPI error ⇒ canonical not-found `Problem` envelope (HTTP `404`, `resource.type="usage_type"`, `resource.id=<gts_id>`) → `UsageTypeReferenced { gts_id, sample_ref_count }` SPI error (the plugin's `ON DELETE RESTRICT` FK on `usage_records.gts_id` rejected the delete) ⇒ canonical conflict `Problem` envelope (HTTP `409`, `reason="USAGE_TYPE_REFERENCED"`, `resource.type="usage_type"`, `resource.id=<gts_id>`, `detail` carrying the human-readable reference count; `sample_ref_count` is NOT exposed as a structured `context.sample_ref_count` field) → successful plugin row removal ⇒ `204 No Content` (REST) / `Ok(())` (SDK). The implementation MUST NOT introduce a tombstone, a state column, or a second transaction.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type`

**Constraints**: `cpt-cf-usage-collector-seq-delete-usage-type`

**Touches**:

- API: `DELETE /usage-collector/v1/usage-types/{gts_id}`
- Entities: `UsageType`


### Entity: UsageType

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-type-lifecycle-entity-usage-type`

The system **MUST** treat `UsageType` as the platform-global, identity-bearing UsageType definition keyed by `gts_id` (the human-readable `gts_id` is a unique `UsageTypeGtsId` newtype whose `Deserialize` impl rejects any string that does not derive from the reserved abstract base `gts.cf.core.uc.usage_record.v1~` with at least one further `~`-separated segment). The entity is described by its closed `metadata_fields: array<string>` (declared metadata key names; all values typed as `String` end-to-end) and its `kind: UsageKind` (closed counter / gauge enum carrying the row's classification). Counter / gauge semantics — `counter` (non-negative delta accumulation) and `gauge` (point-in-time, stored as-is) per DESIGN §3.1 — are carried by the closed `UsageKind` enum on the catalog row's `kind` field per ADR 0012's 2026-06-08 amendment; counter / gauge classification is a first-class column on the catalog row (`kind`), independent of `gts_id`. The system **MUST** reject any UsageType registration whose `gts_id` does not derive from the reserved abstract base at the `UsageTypeGtsId::new` boundary before any repo dispatch (REST: handler synthesises the canonical `invalid_base_gts_id` `Problem` envelope at HTTP `400` from the failed conversion on `CreateUsageTypeRequest::gts_id`; SDK: `UsageCollectorError::Validation` from `UsageTypeGtsId::new`); unknown `kind` values are rejected by closed-enum serde rejection at the deserialize boundary. The entity's identity (`gts_id`) MUST be unique deployment-wide, MUST NOT carry tenant scoping, MUST be validated through the `UsageTypeGtsId` newtype boundary on the REST ingress path, and MUST be re-registrable after a successful clean delete; idempotency-key collisions are not a concern because idempotency-keyed dedup is per (tenant_id, gts_id, idempotency_key) and any surviving `usage_records` rows are tolerated by the catalog deletion.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type`
- `cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type`
- `cpt-cf-usage-collector-algo-usage-type-lifecycle-usage-type-shape-validation`

**Constraints**: `UsageType`

**Touches**:

- API: `POST /usage-collector/v1/usage-types`, `DELETE /usage-collector/v1/usage-types/{gts_id}`, `GET /usage-collector/v1/usage-types`, `GET /usage-collector/v1/usage-types/{gts_id}`
- Entities: `UsageType`

### API: POST /usage-collector/v1/usage-types

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-type-lifecycle-api-post-usage-types`

The system **MUST** expose `POST /usage-collector/v1/usage-types` as the REST UsageType-registration entry point per `usage-collector-v1.yaml`, accepting the `CreateUsageTypeRequest` payload (`gts_id: String` — a permissive `String` rather than a typed `UsageTypeGtsId` so the handler can synthesise a deterministic `invalid_base_gts_id` `Problem` envelope from a failed `UsageTypeGtsId::new` conversion; `metadata_fields: Vec<String>`) and returning the canonical `UsageType` resource projected to `UsageTypeDto` for the wire response with a `Location: /usage-collector/v1/usage-types/{gts_id}` header, surfacing deterministic `Problem` envelopes for `403` PDP denial, `400` with `context.reason="invalid_base_gts_id"` + `context.gts_id` (echoing the rejected identifier) + `context.instance_path="/gts_id"` (the request `gts_id` failed `UsageTypeGtsId::new`), `400` with `context.reason="invalid_metadata_fields"` + `context.instance_path` + `context.sub_reason` (malformed `metadata_fields`; one of `invalid_metadata_fields_empty_string` / `invalid_metadata_fields_duplicate`), `409` with `context.reason="usage_type_already_exists"` + `context.gts_id` when the plugin's unique constraint on `gts_id` fires, and propagated platform-error envelopes for upstream resolver or plugin SPI failures. The POST handler is the only entry point that decorates `Problem.context` via `register_error_to_problem`; the snake_case `context.reason` discriminators are emitted only on this surface. The same operation is reachable via the SDK trait method `UsageCollectorClientV1::create_usage_type(ctx, UsageType)`, which accepts a typed `UsageType { gts_id: UsageTypeGtsId, metadata_fields }` directly and therefore cannot reach the `invalid_base_gts_id` path (bad-prefix `gts_id` is rejected at `UsageTypeGtsId::new` and surfaces as `UsageCollectorError::Validation`); both surfaces converge on the same gateway domain service.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type`

**Constraints**: `cpt-cf-usage-collector-fr-usage-type-registration`

**Touches**:

- API: `POST /usage-collector/v1/usage-types`
- Entities: `UsageType`

### API: DELETE /usage-collector/v1/usage-types/{gts_id}

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-type-lifecycle-api-delete-usage-type`

The system **MUST** expose `DELETE /usage-collector/v1/usage-types/{gts_id}` as the REST UsageType-deletion entry point per `usage-collector-v1.yaml`, returning `204 No Content` on a clean delete and deterministic `Problem` envelopes for `403` PDP denial, the canonical not-found envelope when the plugin returns `UsageTypeNotFound` (HTTP `404`, `resource.type="usage_type"`, `resource.id=<gts_id>`), the canonical conflict envelope when the plugin's `ON DELETE RESTRICT` foreign key on `usage_records.gts_id` rejects the delete and surfaces `UsageTypeReferenced { gts_id, sample_ref_count }` (HTTP `409`, `reason="USAGE_TYPE_REFERENCED"`, `resource.type="usage_type"`, `resource.id=<gts_id>`, `detail` carrying the human-readable reference count; `sample_ref_count` is NOT exposed as a structured `context.sample_ref_count` field), and propagated platform-error envelopes for upstream resolver or plugin SPI failures (HTTP `503` for `PluginUnavailable` / `TypesRegistryUnavailable` / `ServiceUnavailable` (plugin-side `Transient` and host-side per-call deadline expirations both lift to `ServiceUnavailable`); HTTP `500` for plugin `BackendError` (lifted to `Internal`)) — never mutating the plugin's `usage_type_catalog` table on any non-`Deleted` outcome. The DELETE handler does NOT route through the POST-only `register_error_to_problem` decorator, so the canonical envelope does not carry a snake_case `Problem.context.reason` discriminator; callers disambiguate via HTTP status, `reason` (when set via `with_reason`), and `resource.*`. The same operation is reachable via the SDK trait method `UsageCollectorClientV1::delete_usage_type`; both surfaces converge on the same gateway domain service.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type`

**Constraints**: `cpt-cf-usage-collector-fr-usage-type-deletion`

**Touches**:

- API: `DELETE /usage-collector/v1/usage-types/{gts_id}`
- Entities: `UsageType`

### API: GET /usage-collector/v1/usage-types

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-type-lifecycle-api-list-usage-types`

The system **MUST** expose `GET /usage-collector/v1/usage-types` as the REST UsageType-list entry point per `usage-collector-v1.yaml`, serving a paged `toolkit_odata::Page<UsageTypeDto>` (REST wire projection of the SDK-side `UsageType`) from a paginated catalog-list dispatched through `cpt-cf-usage-collector-contract-storage-plugin` against the plugin's `usage_type_catalog` table per call. The same operation is reachable via the SDK trait method `UsageCollectorClientV1::list_usage_types(ctx, &ODataQuery)` where the `ODataQuery` carries the optional `limit` and `cursor` and returns `toolkit_odata::Page<UsageType>` (the SDK-side type, not the REST projection); both surfaces converge on the same gateway domain service.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types`

**Constraints**: `cpt-cf-usage-collector-component-usage-type-catalog`

**Touches**:

- API: `GET /usage-collector/v1/usage-types`
- Entities: `UsageType`

### API: GET /usage-collector/v1/usage-types/{gts_id}

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-type-lifecycle-api-get-usage-type`

The system **MUST** expose `GET /usage-collector/v1/usage-types/{gts_id}` as the REST UsageType-lookup entry point per `usage-collector-v1.yaml`, serving the canonical `UsageType` resource projected to `UsageTypeDto` for the wire response (`gts_id: String`, `kind: UsageKind`, `metadata_fields: Vec<String>` — every field of the SDK `UsageType` struct, see `usage-collector-sdk/src/models.rs`) via a catalog-get dispatch through `cpt-cf-usage-collector-contract-storage-plugin`, and returning the canonical not-found `Problem` envelope (HTTP `404`, `resource.type="usage_type"`, `resource.id=<gts_id>`) when the plugin returns `Err(UsageTypeNotFound { gts_id })` for the supplied `gts_id`. The same operation is reachable via the SDK trait method `UsageCollectorClientV1::get_usage_type(ctx, gts_id)` (returns the SDK-side `UsageType` directly); both surfaces converge on the same gateway domain service.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type`

**Constraints**: `cpt-cf-usage-collector-component-usage-type-catalog`

**Touches**:

- API: `GET /usage-collector/v1/usage-types/{gts_id}`
- Entities: `UsageType`

### Error Mapping: SPI → REST / SDK

Mapping of usage-type-lifecycle outcomes from the storage plugin SPI surface and the gateway ingest-validation surface onto the REST `Problem` envelope and the flat `UsageCollectorError` SDK variant. The shape of the REST envelope is **per-operation**: only the POST register handler decorates `Problem.context` with snake_case discriminator fields (via `register_error_to_problem`); the DELETE / GET / LIST handlers emit the canonical `Problem` envelope unmodified, relying on HTTP status, `Problem.reason` (when set via `with_reason`), and `Problem.resource.*` to disambiguate. The ingest `UnknownMetadataKey` failure is conveyed via `Problem.field_violations[*]` rather than a top-level discriminator.

**POST `/usage-collector/v1/usage-types`** (decorated by `register_error_to_problem`):

| SPI / gateway outcome                  | HTTP  | `Problem.context.reason`    | Other `Problem.context` fields                                                | SDK `UsageCollectorError` variant         |
| -------------------------------------- | ----- | --------------------------- | ----------------------------------------------------------------------------- | ----------------------------------------- |
| Bad `gts_id` base type prefix          | `400` | `invalid_base_gts_id`       | `gts_id` (rejected identifier), `instance_path="/gts_id"`                     | `InvalidUsageTypeGtsId` |
| Malformed `metadata_fields`            | `400` | `invalid_metadata_fields`   | `instance_path="/metadata_fields/{i}"`, `sub_reason` (one of `invalid_metadata_fields_empty_string` / `invalid_metadata_fields_duplicate`) | `InvalidMetadataField` / `DuplicateMetadataField`                 |
| `UsageTypeAlreadyExists`               | `409` | `usage_type_already_exists` | `gts_id` (offending identifier)                                               | `UsageTypeAlreadyExists`                  |
| PDP deny                               | `403` | `authz`                     | (PDP-supplied attribution)                                                    | |

**DELETE / GET / LIST `/usage-collector/v1/usage-types[/{gts_id}]`** (canonical envelope, no `register_error_to_problem` decoration — no `Problem.context.reason` discriminator):

| SPI / gateway outcome      | HTTP  | `Problem` shape                                                                                                          | SDK `UsageCollectorError` variant      |
| -------------------------- | ----- | ------------------------------------------------------------------------------------------------------------------------ | -------------------------------------- |
| `UsageTypeNotFound`        | `404` | canonical not-found; `resource.type="usage_type"`, `resource.id=<gts_id>`                                                | `UsageTypeNotFound`                    |
| `UsageTypeReferenced`      | `409` | canonical conflict; `reason="USAGE_TYPE_REFERENCED"`, `resource.type="usage_type"`, `resource.id=<gts_id>`, `detail` carrying the human-readable reference count (`sample_ref_count` NOT exposed as a structured field) | `UsageTypeReferenced { gts_id, sample_ref_count }` |
| PDP deny                   | `403` | canonical authz-deny; `reason="authz"`                                                                                   | |

**Ingest path (consumed by usage-emission, listed here for completeness)** — `UnknownMetadataKey` is conveyed via `Problem.field_violations[*]`, not a top-level `Problem.context.reason`:

| Gateway outcome                          | HTTP  | `Problem` shape                                                              | SDK `UsageCollectorError` variant |
| ---------------------------------------- | ----- | ---------------------------------------------------------------------------- | --------------------------------- |
| `UnknownMetadataKey { gts_id, key }`     | `400` | `field_violations[0].reason="UNKNOWN_METADATA_KEY"`, `field_violations[0].field="metadata.<key>"` | `UnknownMetadataKey`              |
| `UsageTypeNotFound(gts_id)` (ingest miss) | `404` | canonical not-found; `resource.type="usage_type"`, `resource.id=<gts_id>`     | `UsageTypeNotFound`                |

**Plugin transport / availability failures** (all surfaces):

| SPI outcome                                | HTTP  | SDK `UsageCollectorError` variant |
| ------------------------------------------ | ----- | --------------------------------- |
| `PluginUnavailable`                        | `503` | `PluginUnavailable`               |
| `TypesRegistryUnavailable`                 | `503` | `TypesRegistryUnavailable`        |
| Plugin `Transient { detail }` or host-side per-call deadline | `503` | `ServiceUnavailable`              |
| Plugin `BackendError { kind, detail }`     | `500` | `Internal("{kind}: {detail}")`    |

The SPI no longer carves a separate `Timeout` variant — downstream timeouts surface as plugin-side `Transient` (lifted to `ServiceUnavailable`); host-side per-call deadline expirations also surface as `ServiceUnavailable`.

Plugin `BackendError` lifts to the unclassified `Internal` envelope (HTTP 500) until a documented retryable-kind taxonomy is defined; once real plugins ship with one, retryable kinds will route to dedicated `#[non_exhaustive]` SDK variants.

The `invalid_base_gts_id` envelope is synthesised by the REST POST handler from a failed `UsageTypeGtsId::new` conversion on the inbound `CreateUsageTypeRequest::gts_id` (whose DTO field is a permissive `String`). The SDK trait's `create_usage_type` takes a typed `UsageType { gts_id: UsageTypeGtsId, ... }` and therefore cannot reach `invalid_base_gts_id` on the SDK path; bad prefixes surface as `UsageCollectorError::Validation` from `UsageTypeGtsId::new` instead.

### §2.2-item → DoD-ID Coverage Matrix

Coverage of every DECOMPOSITION §2.2 catalog item:

| §2.2 Item                                                      | Kind              | DoD ID                                                                                  |
| -------------------------------------------------------------- | ----------------- | --------------------------------------------------------------------------------------- |
| `cpt-cf-usage-collector-fr-usage-type-registration`            | FR                | `cpt-cf-usage-collector-dod-usage-type-lifecycle-fr-usage-type-registration`            |
| `cpt-cf-usage-collector-fr-usage-type-deletion`                | FR                | `cpt-cf-usage-collector-dod-usage-type-lifecycle-fr-usage-type-deletion`                |
| `cpt-cf-usage-collector-fr-counter-semantics`                  | FR                | `cpt-cf-usage-collector-dod-usage-type-lifecycle-fr-counter-semantics`                  |
| `cpt-cf-usage-collector-fr-gauge-semantics`                    | FR                | `cpt-cf-usage-collector-dod-usage-type-lifecycle-fr-gauge-semantics`                    |
| `cpt-cf-usage-collector-nfr-availability`                      | NFR               | `cpt-cf-usage-collector-dod-usage-type-lifecycle-nfr-availability`                      |
| `cpt-cf-usage-collector-principle-semantics-enforcement`            | Principle         | `cpt-cf-usage-collector-dod-usage-type-lifecycle-principle-semantics-enforcement`            |
| `cpt-cf-usage-collector-constraint-no-business-logic`          | Design constraint | `cpt-cf-usage-collector-dod-usage-type-lifecycle-constraint-no-business-logic`          |
| `cpt-cf-usage-collector-component-usage-type-catalog`          | Design component  | `cpt-cf-usage-collector-dod-usage-type-lifecycle-component-usage-type-catalog`          |
| `cpt-cf-usage-collector-seq-register-usage-type`               | Sequence          | `cpt-cf-usage-collector-dod-usage-type-lifecycle-seq-register-usage-type`               |
| `cpt-cf-usage-collector-seq-delete-usage-type`                 | Sequence          | `cpt-cf-usage-collector-dod-usage-type-lifecycle-seq-delete-usage-type`                 |
| `UsageType`                     | Domain entity     | `cpt-cf-usage-collector-dod-usage-type-lifecycle-entity-usage-type`                     |
| `POST /usage-collector/v1/usage-types`                         | API               | `cpt-cf-usage-collector-dod-usage-type-lifecycle-api-post-usage-types`                  |
| `DELETE /usage-collector/v1/usage-types/{gts_id}`              | API               | `cpt-cf-usage-collector-dod-usage-type-lifecycle-api-delete-usage-type`                 |
| `GET /usage-collector/v1/usage-types`                          | API               | `cpt-cf-usage-collector-dod-usage-type-lifecycle-api-list-usage-types`                  |
| `GET /usage-collector/v1/usage-types/{gts_id}`                 | API               | `cpt-cf-usage-collector-dod-usage-type-lifecycle-api-get-usage-type`                    |

Coverage totals: FR=4, NFR=2, Principle=1, Design constraint=1, Design component=1, Sequence=2, Data=1, Domain entity=1, API=4 — total 17 DoD entries, zero duplicates, zero §2.2 gaps. The DoD set covers every DECOMPOSITION §2.2 coverage item with exactly one DoD entry per item, and the closing matrix maps every §2.2 row to its DoD ID.

## 6. Acceptance Criteria

- [ ] `p1` - After a successful `POST /usage-collector/v1/usage-types` with a valid `UsageType` (`gts_id`, `metadata_fields`), a subsequent `GET /usage-collector/v1/usage-types/{gts_id}` returns the canonical `UsageType` resource (projected to `UsageTypeDto` on the wire) whose `gts_id` and `metadata_fields` are byte-identical to the persisted catalog entry (and whose `kind` classification matches the registration payload), and a `GET /usage-collector/v1/usage-types` page includes that same entry with the same field values (catalog round-trip correctness).
- [ ] `p1` - Every `POST /usage-collector/v1/usage-types` / `DELETE /usage-collector/v1/usage-types/{gts_id}` REST call and every `UsageCollectorClientV1::create_usage_type` / `delete_usage_type` SDK trait call accepts a resolved `SecurityContext` (REST: `Extension<SecurityContext>` populated by ToolKit gateway middleware via `OperationBuilder::authenticated()`; SDK: `&SecurityContext` argument) at the surface boundary and dispatches authorization through `cpt-cf-usage-collector-flow-foundation-pdp-authorize` (per-component `authorize` helper against `cpt-cf-usage-collector-contract-authz-resolver`) before any plugin SPI dispatch — both surfaces converge on the shared gateway domain service; a PDP `deny` decision returns the platform-authorization error envelope (HTTP `403` on REST, `UsageCollectorError` on SDK) and leaves the durable `usage_type_catalog` row count unchanged (PDP gating).
- [ ] `p1` - A `DELETE /usage-collector/v1/usage-types/{gts_id}` (or `UsageCollectorClientV1::delete_usage_type`) whose target UsageType has zero referencing usage records removes the catalog entry via the plugin SPI catalog-delete dispatch; on any non-`Deleted` outcome (canonical 404 not-found envelope for `UsageTypeNotFound`, canonical 409 conflict envelope with `reason="USAGE_TYPE_REFERENCED"` for `UsageTypeReferenced`, or any platform-error envelope) the plugin's catalog entry is unchanged (referential-delete protocol).
- [ ] `p1` - A `POST /usage-collector/v1/usage-types` whose supplied `gts_id` does not derive from the reserved abstract base `gts.cf.core.uc.usage_record.v1~` (or is missing a derivation segment after the base) returns HTTP `400` with the canonical `invalid_base_gts_id` `Problem` envelope (`context.reason="invalid_base_gts_id"`, `context.gts_id` echoing the rejected identifier, `context.instance_path="/gts_id"`; emitted by the POST-only `register_error_to_problem` decorator), synthesised by the REST handler from a failed `UsageTypeGtsId::new` conversion on the inbound `CreateUsageTypeRequest::gts_id` (whose DTO field is a permissive `String`). The SDK equivalent `UsageCollectorClientV1::create_usage_type` is unreachable with a bad-base `gts_id` because the typed `UsageType` argument carries a `UsageTypeGtsId` newtype whose constructor (`UsageTypeGtsId::new`) returns `UsageCollectorError::Validation` on a non-derived identifier. Either way no plugin dispatch happens and the plugin's `usage_type_catalog` table is unchanged (base-derivation enforcement at the `UsageTypeGtsId::new` boundary).
- [ ] `p1` - `GET /usage-collector/v1/usage-types` / `UsageCollectorClientV1::list_usage_types` and `GET /usage-collector/v1/usage-types/{gts_id}` / `UsageCollectorClientV1::get_usage_type` are served via a paginated/single-row dispatch through `cpt-cf-usage-collector-contract-storage-plugin` against the unified usage-type catalog per call; when the bound plugin is unavailable the endpoints surface deterministic 503 / 500 platform-error envelopes (`PluginUnavailable` / `TypesRegistryUnavailable` / `ServiceUnavailable` ⇒ 503; plugin `BackendError` ⇒ 500 via `Internal`), `cpt-cf-usage-collector-dod-usage-type-lifecycle-api-list-usage-types`, and `cpt-cf-usage-collector-dod-usage-type-lifecycle-api-get-usage-type` — the gear does not withhold the REST router on plugin absence (plugin binding is lazy).
- [ ] `p1` - **Given** the catalog contains a registered UsageType `T` whose `metadata_fields = ["region"]`, **when** any caller submits a usage row carrying `gts_id = T` and `metadata = { "region": "eu-west-1" }`, **then** the gateway ingest-metadata-validation algorithm resolves the declared keys via a plugin SoR round-trip, verifies every candidate key is a member of `metadata_fields`, and accepts the row for plugin dispatch (ingest success).
- [ ] `p1` - **Given** the catalog contains a registered UsageType `T` whose `metadata_fields = ["region"]`, **when** any caller submits a usage row carrying `gts_id = T` and `metadata = { "region": "eu-west-1", "extra_tag": "x" }`, **then** the gateway ingest-metadata-validation algorithm returns HTTP `400` with a `Problem` envelope whose `field_violations[0].reason = "UNKNOWN_METADATA_KEY"` and `field_violations[0].field = "metadata"` (the offending key `extra_tag` carried in the human-readable detail), no plugin write dispatch occurs, and no `usage_records` row is mutated (ingest rejection).
- [ ] `p1` - **Given** the catalog contains a registered UsageType `L` and the plugin's `usage_records` table holds at least one row whose `gts_id = L`, **when** an operator calls `DELETE /usage-collector/v1/usage-types/{gts_id}` (or `UsageCollectorClientV1::delete_usage_type`) for `L`, **then** the plugin's `ON DELETE RESTRICT` foreign key rejects the delete inside the same transaction and surfaces `UsageTypeReferenced { gts_id: L, sample_ref_count }`, the gateway returns the canonical conflict `Problem` envelope (HTTP `409`, `reason="USAGE_TYPE_REFERENCED"`, `resource.type="usage_type"`, `resource.id=L`, `detail` carrying the human-readable reference count; `sample_ref_count` is NOT exposed as a structured `context.sample_ref_count` field), no `usage_type_catalog` row is removed, and no `usage_records` row is mutated (referential-delete rejection).
