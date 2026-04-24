# Feature: Tenant Metadata


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [List Tenant Metadata](#list-tenant-metadata)
  - [Get Tenant Metadata Entry](#get-tenant-metadata-entry)
  - [Put Tenant Metadata Entry](#put-tenant-metadata-entry)
  - [Delete Tenant Metadata Entry](#delete-tenant-metadata-entry)
  - [Resolve Tenant Metadata](#resolve-tenant-metadata)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Schema UUID Derivation](#schema-uuid-derivation)
  - [Metadata Resolve Walk-Up](#metadata-resolve-walk-up)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Schema Registration + UUIDv5 Derivation](#schema-registration--uuidv5-derivation)
  - [CRUD Contract](#crud-contract)
  - [Distinct 404 Sub-Codes](#distinct-404-sub-codes)
  - [Inheritance Resolution Contract](#inheritance-resolution-contract)
  - [Application-Only Enforcement](#application-only-enforcement)
  - [Cascade-Delete on Tenant Removal](#cascade-delete-on-tenant-removal)
  - [Per-Schema Authorization Attribute](#per-schema-authorization-attribute)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Deliberate Omissions](#7-deliberate-omissions)

<!-- /toc -->

- [ ] `p2` - **ID**: `cpt-cf-account-management-featstatus-tenant-metadata`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p2` - `cpt-cf-account-management-feature-tenant-metadata`

## 1. Feature Context

### 1.1 Overview

Owns the extensible tenant-metadata subsystem: GTS-registered metadata schemas, tenant-scoped CRUD keyed by `(tenant_id, schema_uuid)`, direct-on-tenant listing, and barrier-aware effective-value resolution driven by each schema's `inheritance_policy` trait. Encapsulates `MetadataService`, the `tenant_metadata` storage table (one row per directly-written value — inheritance is NEVER materialized per ADR-0002), and the `/api/account-management/v1/tenants/{tenant_id}/metadata` REST family so new metadata categories (branding, billing contacts, future attributes) register as GTS schemas without AM code changes.

### 1.2 Purpose

Delivers PRD §5.7 (Extensible Tenant Metadata) by making every metadata category a GTS-registered schema that carries its own `inheritance_policy` trait (`override_only` default, or `inherit`) rather than a hard-coded AM table. `MetadataService` persists only directly-written values and derives inheritance on every read via ancestor walk-up stopping at self-managed barriers — so a self-managed tenant never inherits metadata from ancestors above its barrier, and suspension of an intermediate tenant does NOT stop the walk (suspension is a lifecycle state, not a barrier). The feature threads public `schema_id` (full chained `GtsSchemaId`) through the REST URL, the AuthZ resource attribute, the audit payload, and the SDK, while internally deriving a deterministic UUIDv5 `schema_uuid` for the `UNIQUE (tenant_id, schema_uuid)` storage key and reverse-hydrating to `schema_id` on every read response. Distinct 404 sub-codes — `metadata_schema_not_registered` vs `metadata_entry_not_found` — let clients separate "unknown schema" from "schema known but no value on this tenant". All metadata rows cascade-delete via DB `ON DELETE CASCADE` when the tenant row is removed.

**Requirements**: `cpt-cf-account-management-fr-tenant-metadata-schema`, `cpt-cf-account-management-fr-tenant-metadata-crud`, `cpt-cf-account-management-fr-tenant-metadata-api`, `cpt-cf-account-management-fr-tenant-metadata-list`, `cpt-cf-account-management-fr-tenant-metadata-permissions`

**Principles**: None. DECOMPOSITION §2.7 assigns no principle rows to this feature — the platform Source-of-Truth and Tree-Invariant principles are inherited transitively from `cpt-cf-account-management-feature-tenant-hierarchy-management`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-account-management-actor-tenant-admin` | Primary caller of every REST endpoint in the `/tenants/{tenant_id}/metadata` family; invokes list, per-schema get / put / delete, and `/resolved` within an authorized tenant scope. |
| `cpt-cf-account-management-actor-platform-admin` | Counterparty for cross-tenant metadata reads authorized by platform-level policy (e.g., operator diagnostics); subject to the same barrier invariants as tenant-admins for `/resolved` reads. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §5.7 Extensible Tenant Metadata (`fr-tenant-metadata-schema`, `fr-tenant-metadata-crud`, `fr-tenant-metadata-api`, `fr-tenant-metadata-list`, `fr-tenant-metadata-permissions`); §5.8 Deterministic Error Semantics (sub-code taxonomy consumed here).
- **Design**: [DESIGN.md](../DESIGN.md) §3.1 Domain Model — Tenant Metadata Schemas with Traits (base envelope `gts.x.core.am.tenant_metadata.v1~`, `inheritance_policy` trait); §3.2 Component Model — `MetadataService` (`cpt-cf-account-management-component-metadata-service`); §3.6 Sequences — `cpt-cf-account-management-seq-resolve-metadata`; §3.7 `dbtable-tenant-metadata` storage contract (UNIQUE `(tenant_id, schema_uuid)`, `ON DELETE CASCADE` to `tenants`, index on `schema_uuid`); §3.8 Error Codes Reference (`metadata_schema_not_registered`, `metadata_entry_not_found`); [ADR-0002](../ADR/0002-cpt-cf-account-management-adr-metadata-inheritance.md) (`adr-metadata-inheritance` — application-only enforcement, no DB trigger / materialized column).
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.7 Tenant Metadata.
- **Dependencies**:
  - `cpt-cf-account-management-feature-tenant-hierarchy-management` — owns `tenants` + `tenant_closure`, the `parent_id` walk-up primitive, and the `ON DELETE CASCADE` cascade on `tenant_metadata.tenant_id`.
  - `cpt-cf-account-management-feature-tenant-type-enforcement` — owns the GTS types-registry integration path reused by `MetadataService` for metadata-schema resolution and `x-gts-traits` lookup.
  - `cpt-cf-account-management-feature-managed-self-managed-modes` — owns the `tenants.self_managed` flag whose value is the stop condition for the inheritance walk-up.
  - `cpt-cf-account-management-feature-errors-observability` — owns the RFC 9457 envelope and the authoritative sub-code catalog (`metadata_schema_not_registered`, `metadata_entry_not_found`, `validation`, `cross_tenant_denied`).

## 2. Actor Flows (CDSL)

### List Tenant Metadata

- [ ] `p2` - **ID**: `cpt-cf-account-management-flow-tenant-metadata-list`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Authenticated tenant-admin GETs `/api/account-management/v1/tenants/{tenant_id}/metadata` with optional pagination params; `MetadataService::list_for_tenant` returns the paginated list of direct-on-tenant `(schema_uuid, value)` rows from `dbtable-tenant-metadata` for `{tenant_id}`, reverse-hydrating each `schema_uuid` to its public chained `schema_id` via Types Registry before building the response; inherited ancestor values are NOT included (clients use `/resolved` for per-schema effective values).

**Error Scenarios**:

- `{tenant_id}` does not resolve to a visible tenant (not found, or not visible to caller scope) — rejected with `not_found` / `cross_tenant_denied` via the `feature-errors-observability` envelope; no DB read is issued beyond the tenant existence guard.

**Steps**:

1. [ ] - `p2` - Validate caller identity and `SecurityContext` per platform AuthN middleware; BEFORE any DB access, apply `PolicyEnforcer::enforce` for `Metadata.list` on `{tenant_id}` per `fr-tenant-metadata-permissions`, omitting `SCHEMA_ID` because the list endpoint has no per-schema selector - `inst-flow-mdlist-authz`
2. [ ] - `p2` - Resolve `{tenant_id}` to a visible tenant via `TenantService`; forward not-found / cross-tenant-denied outcomes as envelope-mapped errors - `inst-flow-mdlist-resolve-tenant`
3. [ ] - `p2` - Invoke `MetadataService::list_for_tenant(tenant_id, pagination)` via MetadataRepository; the query hits `dbtable-tenant-metadata` filtered by `tenant_id` only (NO ancestor walk) - `inst-flow-mdlist-query`
4. [ ] - `p2` - Reverse-hydrate each distinct `schema_uuid` to its public chained `schema_id` via Types Registry per DESIGN §3.2.3 - `inst-flow-mdlist-hydrate`
5. [ ] - `p2` - **RETURN** 200 OK with the paginated response carrying `{schema_id, value, updated_at}` entries; inherited values are observable ONLY through `/resolved` - `inst-flow-mdlist-return`

### Get Tenant Metadata Entry

- [ ] `p2` - **ID**: `cpt-cf-account-management-flow-tenant-metadata-get`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Authenticated tenant-admin GETs `/api/account-management/v1/tenants/{tenant_id}/metadata/{schema_id}`; `MetadataService` resolves the schema via Types Registry, derives `schema_uuid` via `algo-schema-uuid-derivation`, and reads the tenant's direct entry from `dbtable-tenant-metadata` keyed by `(tenant_id, schema_uuid)`; returns 200 OK with `{schema_id, value, updated_at}`.

**Error Scenarios**:

- `{schema_id}` is not registered in the GTS types registry — rejected with 404 `sub_code=metadata_schema_not_registered` per DESIGN §3.2.3 and §3.8; the caller distinguishes "unknown schema" from "schema known but no value".
- `{schema_id}` is registered but the tenant has no direct entry at `(tenant_id, schema_uuid)` — rejected with 404 `sub_code=metadata_entry_not_found`; clients reading this single-entry endpoint see the distinction clearly (the `/resolved` endpoint has different empty-value semantics).
- `{tenant_id}` does not resolve to a visible tenant — rejected with `not_found` / `cross_tenant_denied` via the envelope.

**Steps**:

1. [ ] - `p2` - Validate caller identity + `SecurityContext`; apply `PolicyEnforcer::enforce` for `Metadata.read` with `schema_id` as resource attribute - `inst-flow-mdget-authz`
2. [ ] - `p2` - Resolve `{tenant_id}` via `TenantService` and `{schema_id}` via GTS types registry - `inst-flow-mdget-resolve`
3. [ ] - `p2` - **IF** schema is not registered in GTS - `inst-flow-mdget-schema-unknown`
   1. [ ] - `p2` - **RETURN** `(reject, sub_code=metadata_schema_not_registered)` via the envelope (HTTP 404) - `inst-flow-mdget-schema-return`
4. [ ] - `p2` - Derive `schema_uuid` via `algo-schema-uuid-derivation` with the resolved chained `schema_id` - `inst-flow-mdget-derive-uuid`
5. [ ] - `p2` - Query the tenant's direct entry by `(tenant_id, schema_uuid)` via MetadataRepository - `inst-flow-mdget-query`
6. [ ] - `p2` - **IF** no row exists - `inst-flow-mdget-entry-missing`
   1. [ ] - `p2` - **RETURN** `(reject, sub_code=metadata_entry_not_found)` via the envelope (HTTP 404) so the caller distinguishes unregistered schemas from unset entries - `inst-flow-mdget-entry-return`
7. [ ] - `p2` - **RETURN** 200 OK with `{schema_id, value, updated_at}` — `schema_id` is the public chained form re-hydrated from Types Registry - `inst-flow-mdget-success-return`

### Put Tenant Metadata Entry

- [ ] `p2` - **ID**: `cpt-cf-account-management-flow-tenant-metadata-put`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Authenticated tenant-admin PUTs `/api/account-management/v1/tenants/{tenant_id}/metadata/{schema_id}` with a validated payload; `MetadataService` resolves the GTS schema, validates the body against it, derives `schema_uuid`, upserts the row at `(tenant_id, schema_uuid)` in `dbtable-tenant-metadata`, and returns 200 OK (update) or 201 Created (insert) with the projected entry.

**Error Scenarios**:

- `{schema_id}` is not registered — rejected with `metadata_schema_not_registered`; no row written.
- Payload fails GTS schema validation — rejected with `validation`; no row written.
- `{tenant_id}` does not resolve to a visible `active` or `suspended` tenant — rejected via the envelope.

**Steps**:

1. [ ] - `p2` - Validate caller identity + `SecurityContext`; apply `PolicyEnforcer::enforce` for `Metadata.write` with `schema_id` as resource attribute - `inst-flow-mdput-authz`
2. [ ] - `p2` - Resolve `{tenant_id}` via `TenantService` and `{schema_id}` via GTS types registry - `inst-flow-mdput-resolve`
3. [ ] - `p2` - **IF** schema is not registered - `inst-flow-mdput-schema-unknown`
   1. [ ] - `p2` - **RETURN** `(reject, sub_code=metadata_schema_not_registered)` - `inst-flow-mdput-schema-return`
4. [ ] - `p2` - Validate the request body against the registered GTS schema body per `fr-tenant-metadata-schema`; reject with `validation` on failure - `inst-flow-mdput-validate-body`
5. [ ] - `p2` - Derive `schema_uuid` via `algo-schema-uuid-derivation` - `inst-flow-mdput-derive-uuid`
6. [ ] - `p2` - Upsert `(tenant_id, schema_uuid, value, updated_at=now())` via MetadataRepository on `dbtable-tenant-metadata`; the `UNIQUE (tenant_id, schema_uuid)` constraint is the authoritative at-most-one-entry-per-pair guard - `inst-flow-mdput-upsert`
7. [ ] - `p2` - **RETURN** 200 OK (update) or 201 Created (insert) with the projected entry `{schema_id, value, updated_at}` - `inst-flow-mdput-success-return`

### Delete Tenant Metadata Entry

- [ ] `p2` - **ID**: `cpt-cf-account-management-flow-tenant-metadata-delete`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Authenticated tenant-admin DELETEs `/api/account-management/v1/tenants/{tenant_id}/metadata/{schema_id}`; `MetadataService` removes the direct entry at `(tenant_id, schema_uuid)`; ancestor values are NOT affected; returns 204 No Content.

**Error Scenarios**:

- `{schema_id}` is not registered — rejected with `metadata_schema_not_registered` before the DB write is issued.
- `{schema_id}` is registered but no direct entry exists at `(tenant_id, schema_uuid)` — rejected with `metadata_entry_not_found` (HTTP 404); DELETE is NOT idempotent-success on missing rows because `/resolved` semantics make the distinction observable to clients.

**Steps**:

1. [ ] - `p2` - Validate caller identity + `SecurityContext`; apply `PolicyEnforcer::enforce` for `Metadata.delete` with `schema_id` as resource attribute - `inst-flow-mddel-authz`
2. [ ] - `p2` - Resolve `{tenant_id}` and `{schema_id}`; reject unregistered schemas with `metadata_schema_not_registered` - `inst-flow-mddel-resolve`
3. [ ] - `p2` - Derive `schema_uuid` via `algo-schema-uuid-derivation` - `inst-flow-mddel-derive-uuid`
4. [ ] - `p2` - Delete the row at `(tenant_id, schema_uuid)` via MetadataRepository - `inst-flow-mddel-delete`
5. [ ] - `p2` - **IF** zero rows were deleted - `inst-flow-mddel-missing-branch`
   1. [ ] - `p2` - **RETURN** `(reject, sub_code=metadata_entry_not_found)` via the envelope (HTTP 404) - `inst-flow-mddel-missing-return`
6. [ ] - `p2` - **RETURN** 204 No Content on successful removal; ancestor entries are untouched - `inst-flow-mddel-success-return`

### Resolve Tenant Metadata

- [ ] `p2` - **ID**: `cpt-cf-account-management-flow-tenant-metadata-resolve`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Authenticated tenant-admin GETs `/api/account-management/v1/tenants/{tenant_id}/metadata/{schema_id}/resolved`; `MetadataService::resolve` applies the schema's `inheritance_policy` trait per `algo-metadata-resolve-walk-up` — `override_only` returns the tenant's own entry or empty, `inherit` walks the `parent_id` chain stopping at the nearest self-managed ancestor; returns 200 OK with the effective value OR an empty value (the normal terminal state of the walk when nothing is found).
- Empty resolution is NOT `metadata_entry_not_found` — it is the expected terminal state of an inheritance walk; clients treat empty as "no effective value" rather than "error" per DESIGN §3.2.3.

**Error Scenarios**:

- `{schema_id}` is not registered — rejected with `metadata_schema_not_registered` (HTTP 404).
- `{tenant_id}` does not resolve to a visible tenant — rejected via the envelope.

**Steps**:

1. [ ] - `p2` - Validate caller identity + `SecurityContext`; apply `PolicyEnforcer::enforce` for `Metadata.resolve` with `schema_id` as resource attribute - `inst-flow-mdres-authz`
2. [ ] - `p2` - Resolve `{tenant_id}` and `{schema_id}`; reject unregistered schemas with `metadata_schema_not_registered` - `inst-flow-mdres-resolve`
3. [ ] - `p2` - Resolve the schema's `inheritance_policy` trait from its `x-gts-traits` (falling back to the base schema default `override_only`) via Types Registry - `inst-flow-mdres-policy`
4. [ ] - `p2` - Invoke `algo-metadata-resolve-walk-up` with `(tenant_id, schema_uuid, policy)` — returns effective value or empty - `inst-flow-mdres-invoke-algo`
5. [ ] - `p2` - **RETURN** 200 OK with the effective value or empty; empty is a valid terminal outcome, NOT a 404 - `inst-flow-mdres-success-return`

## 3. Processes / Business Logic (CDSL)

### Schema UUID Derivation

- [ ] `p2` - **ID**: `cpt-cf-account-management-algo-tenant-metadata-schema-uuid-derivation`

**Input**: Full chained `GtsSchemaId` `schema_id` (e.g., `gts.x.core.am.tenant_metadata.v1~z.cf.metadata.branding.v1~`).

**Output**: Deterministic UUIDv5 `schema_uuid` computed from `schema_id` using the shared GTS namespace convention.

**Steps**:

> The UUIDv5 derivation is the only mapping between public `schema_id` (URL / SDK / AuthZ / audit) and storage `schema_uuid` (DB PK component). Determinism is mandatory — the same `schema_id` always produces the same `schema_uuid` so CRUD keyed by `(tenant_id, schema_uuid)` stays stable across restarts, deployments, and AM replica sets. The function MUST be a pure computation — no registry call, no cache lookup.

1. [ ] - `p2` - Normalize the chained `schema_id` per the GTS canonical form (trim, lowercase where specified, preserve case-sensitive segments) per DESIGN §3.1 - `inst-algo-uuid-normalize`
2. [ ] - `p2` - Apply UUIDv5 derivation using the shared GTS namespace per the platform convention referenced by DESIGN §3.2.3 - `inst-algo-uuid-compute`
3. [ ] - `p2` - **RETURN** the computed `schema_uuid` - `inst-algo-uuid-return`

### Metadata Resolve Walk-Up

- [ ] `p2` - **ID**: `cpt-cf-account-management-algo-tenant-metadata-resolve-walk-up`

**Input**: `(tenant_id, schema_uuid, inheritance_policy)` where `inheritance_policy ∈ {override_only, inherit}`.

**Output**: Effective value (from `tenant_id`'s own entry or an ancestor's entry) or empty (normal terminal state of an unsuccessful walk).

**Steps**:

> Barrier invariant per DESIGN §3.2.3 and ADR-0002: the walk stops at the nearest self-managed ancestor (barrier-stop) so a self-managed tenant NEVER inherits metadata from ancestors above its barrier. Suspended tenants on the walk path do NOT stop the walk — suspension is a lifecycle state, not a barrier; the walk skips them and continues to their ancestors. Application-only enforcement: no DB trigger, no materialized column, no walk-up view.

1. [ ] - `p2` - Read the `tenant_id` row's direct entry at `(tenant_id, schema_uuid)` via MetadataRepository on `dbtable-tenant-metadata` - `inst-algo-walk-read-own`
2. [ ] - `p2` - **IF** direct entry exists - `inst-algo-walk-own-found`
   1. [ ] - `p2` - **RETURN** the direct value (own values are always returned regardless of `self_managed` status — the barrier only blocks inheritance from ancestors) - `inst-algo-walk-own-return`
3. [ ] - `p2` - **IF** `inheritance_policy == override_only` - `inst-algo-walk-override-only`
   1. [ ] - `p2` - **RETURN** empty (no ancestor walk for `override_only`) - `inst-algo-walk-override-return`
4. [ ] - `p2` - Load `tenant_id` row's `(parent_id, self_managed, status)` via TenantRepository - `inst-algo-walk-load-start`
5. [ ] - `p2` - **IF** `tenant_id` is self-managed (`tenants.self_managed == true`) - `inst-algo-walk-start-barrier`
   1. [ ] - `p2` - **RETURN** empty — a self-managed tenant never inherits from ancestors above its barrier per DESIGN §3.2.3 and `principle-barrier-as-data` - `inst-algo-walk-start-barrier-return`
6. [ ] - `p2` - Set `current = tenant_id` - `inst-algo-walk-init`
7. [ ] - `p2` - **IF** `current.parent_id IS NULL` (root reached without a value) - `inst-algo-walk-root-reached`
   1. [ ] - `p2` - **RETURN** empty — normal terminal state of an inheritance walk - `inst-algo-walk-root-return`
8. [ ] - `p2` - Advance `current` to `current.parent_id`; load `(parent_id, self_managed, status)` for the new `current` via TenantRepository - `inst-algo-walk-advance`
9. [ ] - `p2` - **IF** `current` is self-managed (`tenants.self_managed == true`) - `inst-algo-walk-ancestor-barrier`
   1. [ ] - `p2` - **RETURN** empty — barrier-stop BEFORE reading the self-managed ancestor's value per `principle-barrier-as-data` - `inst-algo-walk-ancestor-barrier-return`
10. [ ] - `p2` - **IF** `current.status == suspended` - `inst-algo-walk-suspended-skip`
    1. [ ] - `p2` - Skip reading the suspended ancestor's value; loop back to the root-reached check at step 7 with the new `current` — suspension is a lifecycle state, not a barrier per DESIGN §3.2.3 - `inst-algo-walk-suspended-continue`
11. [ ] - `p2` - Read the direct entry at `(current, schema_uuid)` via MetadataRepository - `inst-algo-walk-read-ancestor`
12. [ ] - `p2` - **IF** direct entry exists at `(current, schema_uuid)` - `inst-algo-walk-ancestor-found`
    1. [ ] - `p2` - **RETURN** the ancestor's value - `inst-algo-walk-ancestor-return`
13. [ ] - `p2` - **ELSE** loop back to the root-reached check at step 7 with the current `current` - `inst-algo-walk-loop`

## 4. States (CDSL)

**Not applicable.** This feature owns no entity lifecycle — `dbtable-tenant-metadata` rows are a flat key-value store under `(tenant_id, schema_uuid)` with no state column, no transitions, no lifecycle events. Cascade-deletion on tenant removal is a DB-level `ON DELETE CASCADE` side-effect owned by `dbtable-tenants`, not a state machine owned here. Tenant-lifecycle states that affect the walk-up (`active`, `suspended`, `self_managed`) are owned by `feature-tenant-hierarchy-management` and `feature-managed-self-managed-modes`; this feature only reads them.

## 5. Definitions of Done

### Schema Registration + UUIDv5 Derivation

- [ ] `p2` - **ID**: `cpt-cf-account-management-dod-tenant-metadata-schema-registration-and-uuid-derivation`

The system **MUST** accept the full chained `GtsSchemaId` as the `schema_id` path parameter on every REST operation and **MUST** validate that the schema is registered in the GTS types registry before any DB read or write; unregistered schemas **MUST** be refused with `sub_code=metadata_schema_not_registered`. `MetadataService` **MUST** derive `schema_uuid` as a deterministic UUIDv5 from `schema_id` using the shared GTS namespace; the derivation **MUST** be a pure computation so `(tenant_id, schema_uuid)` remains stable across restarts, deployments, and replicas. `dbtable-tenant-metadata` **MUST NOT** retain the public `schema_id`; responses re-hydrate `schema_id` from Types Registry on every read.

**Implements**:

- `cpt-cf-account-management-flow-tenant-metadata-get`
- `cpt-cf-account-management-flow-tenant-metadata-put`
- `cpt-cf-account-management-flow-tenant-metadata-delete`
- `cpt-cf-account-management-flow-tenant-metadata-resolve`
- `cpt-cf-account-management-algo-tenant-metadata-schema-uuid-derivation`

**Touches**:

- Entities: `TenantMetadataEntry`, `MetadataSchema`
- Data: `cpt-cf-account-management-dbtable-tenant-metadata`, `gts://gts.x.core.am.tenant_metadata.v1~`
- Sibling integration: Types Registry (`feature-tenant-type-enforcement` integration path)
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner); `metadata_schema_not_registered` referenced by name only.

### CRUD Contract

- [ ] `p2` - **ID**: `cpt-cf-account-management-dod-tenant-metadata-crud-contract`

The system **MUST** support tenant-scoped CRUD on `dbtable-tenant-metadata` keyed by `(tenant_id, schema_uuid)`: `GET` per-schema reads the tenant's own entry; `PUT` upserts after validating the payload body against the registered GTS schema; `DELETE` removes the direct entry without affecting ancestor values; `GET /metadata` lists the tenant's own entries (paginated) and **MUST NOT** walk ancestors. `UNIQUE (tenant_id, schema_uuid)` **MUST** be the authoritative at-most-one-direct-entry-per-pair guard at the DB layer. Payloads that fail GTS schema validation **MUST** be rejected with `sub_code=validation` before any row is written.

**Implements**:

- `cpt-cf-account-management-flow-tenant-metadata-list`
- `cpt-cf-account-management-flow-tenant-metadata-get`
- `cpt-cf-account-management-flow-tenant-metadata-put`
- `cpt-cf-account-management-flow-tenant-metadata-delete`

**Touches**:

- Entities: `TenantMetadataEntry`
- Data: `cpt-cf-account-management-dbtable-tenant-metadata`
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner); `validation` referenced by name only.

### Distinct 404 Sub-Codes

- [ ] `p2` - **ID**: `cpt-cf-account-management-dod-tenant-metadata-distinct-404-sub-codes`

The system **MUST** distinguish "unknown metadata schema" from "schema known but no direct entry on this tenant" on the `GET` and `DELETE` per-schema endpoints via two distinct sub-codes — `metadata_schema_not_registered` and `metadata_entry_not_found` — both surfaced as HTTP 404 through the `feature-errors-observability` envelope. The `/resolved` endpoint **MUST NOT** return `metadata_entry_not_found` for empty resolution — empty is a valid terminal state of the walk per DESIGN §3.2.3 — but **MUST** return `metadata_schema_not_registered` when the schema itself is unknown.

**Implements**:

- `cpt-cf-account-management-flow-tenant-metadata-get`
- `cpt-cf-account-management-flow-tenant-metadata-delete`
- `cpt-cf-account-management-flow-tenant-metadata-resolve`

**Touches**:

- Error taxonomy: delegated to `feature-errors-observability` (catalog owner); `metadata_schema_not_registered` and `metadata_entry_not_found` sub-codes referenced by name only.

### Inheritance Resolution Contract

- [ ] `p2` - **ID**: `cpt-cf-account-management-dod-tenant-metadata-inheritance-resolution-contract`

`MetadataService::resolve` **MUST** apply each schema's `inheritance_policy` trait — resolved from `x-gts-traits` with default `override_only` — as the sole controller of the walk: `override_only` returns the tenant's own entry or empty; `inherit` walks `parent_id` ancestors. The walk **MUST** stop at the nearest self-managed ancestor (barrier-stop per `principle-barrier-as-data`), so a self-managed tenant never inherits metadata from above its barrier. The walk **MUST** skip (traverse through) ancestors whose `status == suspended` and continue to their parents — suspension is a lifecycle state, not a barrier. Empty resolution **MUST** be returned as an empty success response, NOT `metadata_entry_not_found`. There **MUST NOT** be any service-local `inheritance_policy` table, side configuration, or policy override — the trait is authoritative.

**Implements**:

- `cpt-cf-account-management-flow-tenant-metadata-resolve`
- `cpt-cf-account-management-algo-tenant-metadata-resolve-walk-up`

**Constraints**: `cpt-cf-account-management-adr-metadata-inheritance`

**Touches**:

- Entities: `ResolvedMetadataValue`, `MetadataSchema`
- Data: `cpt-cf-account-management-dbtable-tenant-metadata`, `cpt-cf-account-management-dbtable-tenants`, `cpt-cf-account-management-dbtable-tenant-closure`
- Sibling integration: `feature-tenant-hierarchy-management` (`parent_id` walk-up primitive); `feature-managed-self-managed-modes` (`self_managed` flag as barrier-stop condition)

### Application-Only Enforcement

- [ ] `p2` - **ID**: `cpt-cf-account-management-dod-tenant-metadata-application-only-enforcement`

Per `adr-metadata-inheritance` (ADR-0002), inheritance semantics **MUST** be enforced exclusively inside `MetadataService::resolve` at read time. `dbtable-tenant-metadata` **MUST** store only directly-written values; the feature **MUST NOT** introduce a DB-level CHECK, trigger, materialized-inheritance column, or reconciliation job that mirrors the walk-up. Any SQL reader bypassing `MetadataService` **MUST** see only directly-written values — consumers that need inherited values **MUST** call the `/resolved` API boundary or the `MetadataService::resolve` entry point. No reconciliation job is needed because inheritance is derived on every read rather than materialized; write amplification is zero for inheritance semantics.

**Implements**:

- `cpt-cf-account-management-algo-tenant-metadata-resolve-walk-up`
- `cpt-cf-account-management-flow-tenant-metadata-resolve`

**Constraints**: `cpt-cf-account-management-adr-metadata-inheritance`

**Touches**:

- Data: `cpt-cf-account-management-dbtable-tenant-metadata`
- DESIGN anchor: §3.2.3 MetadataService "Enforcement layer — application-only, by design"

### Cascade-Delete on Tenant Removal

- [ ] `p2` - **ID**: `cpt-cf-account-management-dod-tenant-metadata-cascade-delete`

All `dbtable-tenant-metadata` rows for a tenant **MUST** be removed atomically with the tenant row via DB-level `ON DELETE CASCADE` on `tenant_metadata.tenant_id → tenants.id`. This feature **MUST NOT** implement an application-layer cascade cleanup job or a separate explicit delete-all-entries call at tenant-hard-delete time — the database constraint is the single source of truth. Schema-migration owners **MUST** preserve the cascade clause in all future `dbtable-tenant-metadata` migrations.

**Implements**:

- `cpt-cf-account-management-flow-tenant-metadata-put`
- `cpt-cf-account-management-flow-tenant-metadata-delete`

**Touches**:

- Data: `cpt-cf-account-management-dbtable-tenant-metadata`, `cpt-cf-account-management-dbtable-tenants`
- Sibling integration: `feature-tenant-hierarchy-management` (hard-delete flow owner)

### Per-Schema Authorization Attribute

- [ ] `p2` - **ID**: `cpt-cf-account-management-dod-tenant-metadata-per-schema-authz`

Per-schema handlers in the `/tenants/{tenant_id}/metadata` family **MUST** pass the public chained `schema_id` into `PolicyEnforcer::enforce` as a resource attribute (e.g., `SCHEMA_ID`) on the corresponding action (`Metadata.read`, `Metadata.write`, `Metadata.delete`, `Metadata.resolve`) so external AuthZ policy can express per-schema grants without AM evaluating policy itself. The list handler for `GET /tenants/{tenant_id}/metadata` **MUST** call `PolicyEnforcer::enforce` for `Metadata.list` before any DB access but **MUST** omit `SCHEMA_ID` because that endpoint has no per-schema selector. AM **MUST NOT** cache or interpret the policy decision beyond the permit/deny outcome returned by `PolicyEnforcer`.

**Implements**:

- `cpt-cf-account-management-flow-tenant-metadata-list`
- `cpt-cf-account-management-flow-tenant-metadata-get`
- `cpt-cf-account-management-flow-tenant-metadata-put`
- `cpt-cf-account-management-flow-tenant-metadata-delete`
- `cpt-cf-account-management-flow-tenant-metadata-resolve`

**Touches**:

- Sibling integration: `PolicyEnforcer` at the REST layer (external to this feature's surface)
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner); `cross_tenant_denied` referenced by name only.

## 6. Acceptance Criteria

- [ ] A `GET /api/account-management/v1/tenants/{tenant_id}/metadata` for an authorized tenant returns a paginated list of direct-on-tenant entries from `dbtable-tenant-metadata` filtered by `tenant_id` only (NO ancestor walk); each response entry carries the public chained `schema_id` re-hydrated from Types Registry, and no `schema_uuid` appears in the response body. Fingerprints `dod-tenant-metadata-crud-contract`, `dod-tenant-metadata-schema-registration-and-uuid-derivation`.
- [ ] A `GET /api/account-management/v1/tenants/{tenant_id}/metadata/{schema_id}` against an unregistered `schema_id` returns HTTP 404 with `sub_code=metadata_schema_not_registered` via the `feature-errors-observability` envelope; the same endpoint against a registered `schema_id` for which no direct entry exists at `(tenant_id, schema_uuid)` returns HTTP 404 with `sub_code=metadata_entry_not_found` — the two sub-codes are distinct so clients can separate "unknown schema" from "schema known but no value". Fingerprints `dod-tenant-metadata-distinct-404-sub-codes`.
- [ ] A `PUT /api/account-management/v1/tenants/{tenant_id}/metadata/{schema_id}` with a payload that fails the registered GTS schema's body validation returns `sub_code=validation` without writing any row; a payload that validates successfully upserts at `(tenant_id, schema_uuid)` — insert returns HTTP 201, update returns HTTP 200 — and the `UNIQUE (tenant_id, schema_uuid)` constraint rejects any concurrent duplicate at the DB layer. Fingerprints `dod-tenant-metadata-crud-contract`, `dod-tenant-metadata-schema-registration-and-uuid-derivation`.
- [ ] A `DELETE /api/account-management/v1/tenants/{tenant_id}/metadata/{schema_id}` on an existing direct entry returns HTTP 204 and removes only that `(tenant_id, schema_uuid)` row — ancestor entries for the same `schema_id` remain untouched; a DELETE on a registered schema with no direct entry returns HTTP 404 with `sub_code=metadata_entry_not_found` (DELETE is NOT idempotent-success on missing rows here because the distinct-404 contract makes the signal observable). Fingerprints `dod-tenant-metadata-crud-contract`, `dod-tenant-metadata-distinct-404-sub-codes`.
- [ ] A `GET /resolved` for a schema whose `inheritance_policy` is `override_only` returns the tenant's own value if present or HTTP 200 with an empty response if absent; for a schema whose `inheritance_policy` is `inherit`, the walk-up traverses `parent_id` ancestors, returns the first ancestor value found, stops at the nearest self-managed ancestor (barrier-stop) returning empty, and skips (continues past) ancestors whose `status == suspended`. An empty terminal state is HTTP 200 with an empty response, NOT HTTP 404. Fingerprints `dod-tenant-metadata-inheritance-resolution-contract`.
- [ ] A direct `SELECT ... FROM tenant_metadata WHERE tenant_id = X` bypassing `MetadataService` returns only the rows directly written on tenant X — inherited values from ancestors are NOT visible through this path, confirming application-only enforcement. The schema-migration ddl for `tenant_metadata` contains no DB-level CHECK, trigger, or materialized-inheritance column, and no reconciliation job ships with this feature. Fingerprints `dod-tenant-metadata-application-only-enforcement`.
- [ ] Deleting a tenant row from `dbtable-tenants` atomically removes all its `dbtable-tenant-metadata` rows via `ON DELETE CASCADE` on `tenant_metadata.tenant_id → tenants.id`; no application-layer delete-all-entries call is issued as part of the tenant hard-delete flow, and the cascade is preserved across schema migrations. Fingerprints `dod-tenant-metadata-cascade-delete`.
- [ ] The per-schema handlers in the `/tenants/{tenant_id}/metadata` family call `PolicyEnforcer::enforce` with the public chained `schema_id` as a resource attribute on the corresponding action (`Metadata.read` / `Metadata.write` / `Metadata.delete` / `Metadata.resolve`) BEFORE any DB read or write; the list handler calls `PolicyEnforcer::enforce` for `Metadata.list` BEFORE any DB access but omits `SCHEMA_ID` because the endpoint has no per-schema selector. A denied policy decision returns `sub_code=cross_tenant_denied` (or the mapped category per `feature-errors-observability`) without touching `dbtable-tenant-metadata`. Fingerprints `dod-tenant-metadata-per-schema-authz`.
- [ ] The deterministic UUIDv5 derivation at `algo-schema-uuid-derivation` produces the same `schema_uuid` for a given `schema_id` across restarts, deployments, and replica sets — verified by a test that derives the UUID in two independent processes and asserts byte-equality; the derivation is a pure computation and does NOT call Types Registry. Fingerprints `dod-tenant-metadata-schema-registration-and-uuid-derivation`.

## 7. Deliberate Omissions

- **Tenant-hierarchy traversal primitives and ownership of `dbtable-tenants` / `dbtable-tenant-closure`** — *Owned by `cpt-cf-account-management-feature-tenant-hierarchy-management`* (DECOMPOSITION §2.2). This feature consumes the `parent_id` walk-up primitive for inheritance resolution but does not own tenant CRUD, closure maintenance, or status transitions.
- **Barrier state and `self_managed` flag writes / mode-conversion workflow** — *Owned by `cpt-cf-account-management-feature-managed-self-managed-modes`* (DECOMPOSITION §2.4). This feature reads `tenants.self_managed` as the stop condition for the inheritance walk but does not own the flag's lifecycle.
- **GTS types-registry availability and tenant-type / metadata-schema registration workflows** — *Owned by `cpt-cf-account-management-feature-tenant-type-enforcement`* (DECOMPOSITION §2.3). This feature consumes the registry to resolve metadata schemas and `x-gts-traits` but does not own the registration path or the availability contract.
- **Problem Details envelope shape, RFC 9457 formatting, HTTP status mapping, and the authoritative error-code taxonomy** — *Owned by `cpt-cf-account-management-feature-errors-observability`* (DECOMPOSITION §2.8). Sub-codes (`metadata_schema_not_registered`, `metadata_entry_not_found`, `validation`, `cross_tenant_denied`) are catalogued authoritatively there; this feature emits them by name and defers envelope formatting, HTTP status mapping, audit emission, and metric sample naming to that feature.
- **Metadata-schema authoring, JSON Schema body design, `inheritance_policy` trait values, and semantic interpretation of metadata content** — *GTS-registry authoring concern.* Schemas are registered by deployment owners; `MetadataService` treats values as opaque GTS-validated payloads and does not inspect or normalize their semantics.
- **Materialized inheritance views, DB-level CHECK/trigger enforcement, materialized-inheritance columns, and reconciliation jobs** — *Forbidden by `cpt-cf-account-management-adr-metadata-inheritance`* (ADR-0002). Inheritance is derived on every read inside `MetadataService::resolve`; no write-side materialization exists.
- **Authentication, session, token issuance, and user lifecycle** — *Inherited from the platform AuthN layer and `feature-idp-user-operations-contract`* (DESIGN §4.2). `SecurityContext` validation is a platform-layer precondition; AuthZ policy evaluation for `Metadata.*` actions is delegated to `PolicyEnforcer` at the REST layer.
- **Future `inheritance_policy` values (`merge`, `readonly`, `computed`)** — *Deliberate v1 scope reduction per DESIGN §3.1 rationale.* `inheritance_policy` is a string enum precisely so new values can be added additively without a breaking contract change; v1 ships `override_only` and `inherit` only.
