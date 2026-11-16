---
description: "Technical design of the Model Registry gear: the tenant-aware catalog of AI providers and models that LLM Gateway uses to resolve and authorize models for evaluation."
---

Created: 2026-10-23 by Constructor Tech

# Technical Design — Model Registry


<!-- toc -->

- [1. Architecture Overview](#1-architecture-overview)
  - [1.1 Architectural Vision](#11-architectural-vision)
  - [1.2 Architecture Drivers](#12-architecture-drivers)
  - [1.3 Architecture Layers](#13-architecture-layers)
- [2. Principles & Constraints](#2-principles--constraints)
  - [2.1 Design Principles](#21-design-principles)
  - [2.2 Constraints](#22-constraints)
- [3. Technical Architecture](#3-technical-architecture)
  - [3.1 Domain Model](#31-domain-model)
  - [3.2 Component Model](#32-component-model)
  - [3.3 API Contracts](#33-api-contracts)
  - [3.4 Internal Dependencies](#34-internal-dependencies)
  - [3.5 External Dependencies](#35-external-dependencies)
  - [3.6 Interactions & Sequences](#36-interactions--sequences)
  - [3.7 Database schemas & tables](#37-database-schemas--tables)
  - [3.8 Deployment Topology](#38-deployment-topology)
- [4. Additional context](#4-additional-context)
  - [Authorization](#authorization)
  - [GTS Checks](#gts-checks)
  - [Error Handling](#error-handling)
  - [Tenant Isolation and Data Protection](#tenant-isolation-and-data-protection)
  - [Credential Binding](#credential-binding)
  - [Security Assumptions](#security-assumptions)
  - [Observability](#observability)
  - [Fault Tolerance and Consistency](#fault-tolerance-and-consistency)
  - [Configuration](#configuration)
  - [Testability](#testability)
  - [Documentation](#documentation)
  - [Compliance](#compliance)
  - [Not Applicable](#not-applicable)
  - [Change Audit Follow-up](#change-audit-follow-up)
  - [Deviations](#deviations)
  - [Limitations](#limitations)
  - [Open Questions](#open-questions)
- [5. Traceability](#5-traceability)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-model-registry-design-model-registry`

## 1. Architecture Overview

### 1.1 Architectural Vision

Model Registry is an in-process gear that owns two tables: providers and models. Each row belongs to exactly one tenant. Tenant administrators manage the rows of their own tenant through a management API. LLM Gateway and other consumer gears use two resolution operations: resolve one model by its canonical id, and list the models available for evaluation. Both resolution operations compute the caller's effective catalog on every request. The effective catalog is the set of providers and models a tenant sees after inheritance and shadowing.

The effective catalog is built from three inputs: the caller's ancestor chain, the provider rows owned by the tenants in that chain, and the model rows of the winning providers. The ancestor chain comes from Tenant Resolver and is kept in a small in-process cache. Provider and model rows are always read from the database. Visibility is decided in one place: for each provider slug, the nearest visible provider wins. A model is visible only when its provider is the winner for its slug. Provider enabled state, model approval, and the policy decision point (PDP) then decide whether a visible model can be evaluated.

The gear follows the platform SDK pattern. The SDK crate `cf-gears-model-registry-sdk` holds one versioned trait, `ModelRegistryClientV1`, together with its models and error vocabulary. The gear crate `cf-gears-model-registry` implements the trait, registers it in ClientHub, and exposes a REST API that maps one-to-one to the trait methods. Provider-specific settings are opaque JSON documents. The gear validates them on write against the GTS schema that the LLM Gateway provider plugin registered in Types Registry. Model cost is a provider-independent field of the model record, not part of the provider settings. Nothing on the resolution path calls Types Registry.

Terms used below:

- **PDP**: the policy decision point. The PRD's Authorization PDP is the platform AuthZ Resolver (`authz-resolver`), called through the toolkit `PolicyEnforcer`.
- **Fail closed**: when a check cannot complete, the request is refused, never granted.
- **Dependency unavailable**: the `service_unavailable` error (HTTP 503) returned when Tenant Resolver, the PDP, Types Registry, CredStore, or the database cannot answer within its deadline (§4 Error Handling).
- **Row predicate**: the SQL condition built from PDP row constraints and added to a list query.
- **Promoted column**: a real column that holds a copy of a value stored inside a JSON column, used for filtering and ordering.
- **Keyset page**: a page addressed by an opaque cursor that encodes the order key of the last row, not an offset.
- **`SecureConn`**, **`Scopable`**: the toolkit-db connection that applies an `AccessScope` to every query, and the entity derive that declares the tenant and resource columns the scope uses ([secure ORM](../../../docs/toolkit_unified_system/06_authn_authz_secure_orm.md)).

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design Response |
|-------------|------------------|
| `cpt-cf-model-registry-fr-provider-management` | Provider CRUD in `cpt-cf-model-registry-component-catalog-service`, stored in `cpt-cf-model-registry-dbtable-providers` with a unique `(tenant_id, slug)` key. Slug is immutable (`cpt-cf-model-registry-constraint-immutable-identity`); `gts_type` changes only through type migration, which re-validates every model of the provider against the new type and switches the provider and its models in one transaction, keeping ids and canonical ids (§3.6 `cpt-cf-model-registry-seq-provider-write`). Delete removes the provider and its models in one transaction. Visibility is recomputed per request, so a provider it shadowed becomes visible again (§3.6 `cpt-cf-model-registry-seq-provider-write`). |
| `cpt-cf-model-registry-fr-provider-disable` | The `enabled` flag does not change who wins a slug. A disabled winner is left out of the allow-list, so none of its models resolve or appear in the available list, for the owner and every inheriting tenant. Rows are kept; re-enabling restores them. Resolution reports `PROVIDER_DISABLED` only after the model is found (§3.6 `cpt-cf-model-registry-seq-resolve-model`). |
| `cpt-cf-model-registry-fr-provider-sharing` | `sharing_mode` column with values `tenant` and `shared`. Only `shared` ancestor providers become candidates for descendants. The mode is read per request, so a change applies to the next resolution (§3.6 `cpt-cf-model-registry-seq-effective-catalog`). |
| `cpt-cf-model-registry-fr-provider-shadowing` | Winner per slug over the candidate set ordered by chain distance; a tenant's own provider is always a candidate for itself, and an ancestor's provider is a candidate only when `shared`. Implemented once in `cpt-cf-model-registry-component-visibility-resolver`. |
| `cpt-cf-model-registry-fr-model-identity` | `canonical_id = provider_slug::name`, built by the gear on create from the caller-chosen model `name`, stored with unique keys `(tenant_id, canonical_id)` and `(provider_id, name)`. Parsing splits on the first `::`. `provider_id` and `name` are immutable. `provider_model_id` is not part of the identity: it is mutable, and several models of one provider may share it. |
| `cpt-cf-model-registry-fr-model-management` | Model CRUD in `cpt-cf-model-registry-component-catalog-service`. The record holds general settings (optional `base_url`, optional `secret_ref`), cost in AI credits, default parameters with override policy, capabilities, user-facing information, sharing mode, approval, and opaque `provider_settings` (§3.1 `cpt-cf-model-registry-entity-model-info`). `gts_type` is copied from the provider on create and never set by the caller. A `secret_ref` must name a CredStore secret the writing tenant owns; the gear checks it on create and on an update that changes it to a non-null value (§4 Credential Binding). |
| `cpt-cf-model-registry-fr-model-settings-validation` | `cpt-cf-model-registry-component-gts-validator` fetches the resolved schema of the provider type from Types Registry on create and on any update whose `provider_settings` differs from the stored value, validates the document locally, and rejects it with field violations. Types Registry failure rejects the write as dependency unavailable (§3.6 `cpt-cf-model-registry-seq-model-write`). |
| `cpt-cf-model-registry-fr-model-cost` | The `cost` object of `cpt-cf-model-registry-entity-model-info`, one shape for every provider type, stored in the `cost` JSON column of `cpt-cf-model-registry-dbtable-models` and returned in `ModelV1`. Not filterable in v1. |
| `cpt-cf-model-registry-fr-model-approval` | Approval state, decider, and decision time live on the model row (`cpt-cf-model-registry-entity-approval`). Create and update set them under the model `update`/`create` permissions; a create without a state starts as Approved. Only Approved models resolve or appear in the available list. Approval travels with the model; descendants cannot write it. |
| `cpt-cf-model-registry-fr-model-sharing` | `sharing_mode` on models. A descendant sees a model only when the model is `shared` and its provider is a `shared` winner. There is no model-level shadowing: visibility is keyed on `provider_id` only. |
| `cpt-cf-model-registry-fr-effective-catalog` | `cpt-cf-model-registry-component-visibility-resolver` computes winners per slug and the allow-list of provider ids, and filters models by `provider_id` membership, never by `canonical_id` dedupe. Inherited entries are read-only and returned with their full record. |
| `cpt-cf-model-registry-fr-resolve-model` | `resolve_model` runs the PRD order: find in the effective catalog, provider enabled, approved, PDP `evaluate`. Each failure has a distinct canonical error and reason code (§4 Error Handling). PDP or Tenant Resolver failures fail closed. |
| `cpt-cf-model-registry-fr-list-available-models` | `list_available_models` takes one PDP `evaluate` decision, the same action `resolve_model` takes per model, and applies the gates of `resolve_model` in one model query, so every listed model resolves (decision rules in §4 Authorization). It returns a page of full `ModelV1` records, the same record `resolve_model` returns. OData options run on promoted columns (§3.6 `cpt-cf-model-registry-seq-list-available-models`). |
| `cpt-cf-model-registry-fr-tenant-isolation` | Every write is clamped to `tenant_id = caller tenant` through `SecureConn`. Reads return own rows and rows that ancestors share. There are no reads of sibling or descendant rows and no writes to ancestor rows (`cpt-cf-model-registry-principle-owner-only-writes`). |
| `cpt-cf-model-registry-fr-authorization` | `cpt-cf-model-registry-component-access-guard` calls the PDP through `PolicyEnforcer` on every operation. Two GTS-identified resource types; distinct action groups for provider management (provider `get`, `list`, `create`, `update`, `delete`), model management (model `get`, `list`, `create`, `update`, `delete`), and model evaluation (model `evaluate`, for resolving one model and for listing the evaluable models) (§4 Authorization). |
| `cpt-cf-model-registry-fr-management-api-conventions` | Versioned paths under `/model-registry/v1/`, `OperationBuilder` with `.authenticated()` and `.standard_errors()`, OData on every list, canonical Problem errors. Management reads return own and inherited entries with `inherited`, `shadowed`, and eval-availability flags; writes on inherited entries fail with `INHERITED_READ_ONLY`. Every record carries `version`; single-entry responses carry `ETag`, and `PATCH` requires `If-Match` (§3.3). |
| `cpt-cf-model-registry-fr-change-audit` | `p2`. The audit columns `created_by`, `created_at`, `updated_by`, `updated_at` on both tables and `approval_decided_by`, `approval_decided_at` on models ship in p1 as part of the record. The append-only change log of this FR is not part of this design (§4 Change Audit Follow-up). |
| `cpt-cf-model-registry-fr-change-notifications` | `p2`, not covered. The gear publishes no change events; consumers read the catalog on each request (§4 Open Questions). |
| `cpt-cf-model-registry-fr-inherited-model-restriction` | `p2`, not covered. A tenant excludes an inherited model only by shadowing its provider (`cpt-cf-model-registry-seq-effective-catalog`). |

#### NFR Allocation

| NFR ID | NFR Summary | Allocated To | Design Response | Verification Approach |
|--------|-------------|--------------|-----------------|----------------------|
| `cpt-cf-model-registry-nfr-resolution-latency` | p99 ≤ 10 ms in-gear, excluding PDP and Tenant Resolver wait; 1,000 rps sustained, 5,000 rps peak; 10,000 models; hierarchy depth up to 10 | `cpt-cf-model-registry-component-chain-cache`, `cpt-cf-model-registry-component-visibility-resolver`, `cpt-cf-model-registry-component-storage` | On a cache hit, resolution makes no Tenant Resolver call. It runs two indexed point queries in one read-only snapshot transaction: providers by `(tenant_id IN chain, slug)` on the unique key (at most 11 rows), and the model by the unique `(provider_id, name)` key. No Types Registry call on the read path. The available list runs one chain-wide provider query and one model query with the allow-list predicate on indexed columns. p99 sub-budget: chain cache lookup 0.1 ms, transaction begin and end 1 ms, provider query 3 ms, model query 3 ms, mapping and constraint check 1 ms; the rest is headroom. Each resolution takes one PDP decision, so the PDP receives up to 5,000 decisions per second at peak. Outside the in-gear budget, a resolution waits for that decision (Policy Engine target 25 ms p95, `cpt-cf-policy-engine-nfr-decision-latency`; bounded by `pdp_deadline_ms`) and, on a chain cache miss, for Tenant Resolver (bounded by `tenant_resolver_deadline_ms`). | Automated benchmark with 10,000 models, hierarchy depth 10, 1,000 rps sustained and 5,000 rps bursts; the gear reports in-gear duration with dependency wait subtracted (§4 Observability). |
| `cpt-cf-model-registry-nfr-availability` | 99.9% monthly availability of resolution; recovery within 15 minutes after a gear failure | `cpt-cf-model-registry-topology-in-process-gear`, `cpt-cf-model-registry-component-chain-cache` | Stateless replicas; all state is in the platform database. A chain cache miss calls Tenant Resolver. A failed replica is replaced by platform restart without data recovery (§4 Fault Tolerance and Consistency). Every PDP, Tenant Resolver, Types Registry, CredStore, and database call has a deadline (§4 Configuration). The resolution path makes no retries. An expired deadline fails closed as dependency unavailable. | Availability SLI of §4 Observability. Recovery time is measured from the outcomes-by-reason metric after injected gear failures. |
| `cpt-cf-model-registry-nfr-durability` | Zero loss of acknowledged writes | `cpt-cf-model-registry-component-storage`, `cpt-cf-model-registry-db-catalog-storage` | Every write commits in a database transaction before the response is sent. Provider delete and its model deletes share one transaction. No catalog state lives only in memory. The tables are part of the platform database and its standard backups. | Restart tests and backup-restore tests confirming that acknowledged writes survive. |

#### Key ADRs

| ADR | Status | Decision |
|-----|--------|----------|
| [ADR-0001](./ADR/0001-cpt-cf-model-registry-adr-inherited-read-scope.md) `cpt-cf-model-registry-adr-inherited-read-scope` | accepted | Reads run under a gear-built upward scope (acting tenant and its ancestors) with the PDP tenant constraint as a gate; the ancestor chain respects isolation barriers (`BarrierMode::Respect`); `InTenantSubtree` tenant constraints fail closed. As in CredStore ADR-0005, except for barriers. |
| [ADR-0002](./ADR/0002-cpt-cf-model-registry-adr-credential-binding.md) `cpt-cf-model-registry-adr-credential-binding` | accepted | A `secret_ref` is accepted only when the writing tenant's own CredStore row holds a secret, checked once at write time by a metadata-only `get_record` read under the caller's context; no owner check at evaluation; the fallback to an ancestor's `shared` secret after the owner removes its own is an accepted residual that CredStore suppression closes. |
| [ADR-0003](./ADR/0003-cpt-cf-model-registry-adr-caching-strategy.md) `cpt-cf-model-registry-adr-caching-strategy` | accepted | Provider and model rows are read from the database on every request and never cached by the gear or its consumers; only the tenant ancestor chain is cached, per replica, in a bounded LRU with TTL and jitter, with no invalidation, no change events, and no cluster gear dependency. |
| [ADR-0004](./ADR/0004-cpt-cf-model-registry-adr-update-semantics.md) `cpt-cf-model-registry-adr-update-semantics` | accepted | Present top-level fields replace the stored value as a whole on SDK and REST, with no RFC 7396 merge and `null` clearing optional fields; every record carries `version`, REST `PATCH` requires `If-Match` (strong `ETag` is the bare version, `*` is unconditional, optional on `DELETE`) and SDK `expected_version` is optional; a stale precondition is `aborted` (409) with reason `OPTIMISTIC_LOCK_FAILURE`, not 412. |

### 1.3 Architecture Layers

```mermaid
graph TB
    subgraph Callers
        LLMGW["LLM Gateway"]
        CG["Consumer gears"]
        ADM["Tenant administrator via API Gateway"]
    end
    subgraph SDK["cf-gears-model-registry-sdk"]
        TRAIT["ModelRegistryClientV1 trait, models, error vocabulary"]
    end
    subgraph Gear["cf-gears-model-registry"]
        subgraph API["API layer: api/rest"]
            REST["Handlers, DTOs, routes, Problem mapping"]
        end
        subgraph Domain["Domain layer: domain"]
            LOCAL["Local client"]
            SVC["Catalog service"]
            VIS["Visibility resolver"]
            AG["Access guard"]
            GV["GTS validator"]
        end
        subgraph Infra["Infrastructure layer: infra"]
            STORE["Storage: entities, repositories, migrations"]
            CACHE["Chain cache"]
        end
    end
    DB[("Platform database")]
    TR["Tenant Resolver SDK"]
    PDP["AuthZ Resolver SDK"]
    TY["Types Registry SDK"]
    CRS["CredStore SDK"]

    LLMGW --> TRAIT
    CG --> TRAIT
    TRAIT -.implemented by.-> LOCAL
    ADM --> REST
    REST --> SVC
    LOCAL --> SVC
    SVC --> VIS
    SVC --> AG
    SVC --> GV
    SVC --> CRS
    SVC --> STORE
    VIS --> CACHE
    VIS --> STORE
    CACHE --> TR
    AG --> PDP
    GV --> TY
    STORE --> DB
```

- [ ] `p1` - **ID**: `cpt-cf-model-registry-tech-toolkit-gear`
- [ ] `p1` - **ID**: `cpt-cf-model-registry-tech-relational-storage`
- [ ] `p1` - **ID**: `cpt-cf-model-registry-tech-json-schema-validation`

| Layer | Responsibility | Technology |
|-------|---------------|------------|
| Presentation | REST handlers and DTOs, OData query extraction, `$select` projection, mapping of domain errors to canonical Problem responses | Axum handlers registered with toolkit `OperationBuilder`; `utoipa` DTOs in `api/rest/` only |
| Domain | Local client; catalog service (operation flow, authorization calls, validation order, transactions, audit columns); effective-catalog algorithm, approval and sharing rules, PDP scope gate, GTS type and settings checks | Rust in `domain/` (local client included), `#[domain_model]` types, toolkit `PolicyEnforcer` (`cpt-cf-model-registry-tech-toolkit-gear`); settings validated with the workspace JSON Schema validator (`jsonschema`) against schemas from Types Registry (`cpt-cf-model-registry-tech-json-schema-validation`) |
| Infrastructure | SeaORM entities with `Scopable`, repositories over `DBRunner`, migrations, OData field-to-column mapping, chain cache | SeaORM through `toolkit-db` `SecureConn`; PostgreSQL in production, SQLite in dev/test (`cpt-cf-model-registry-tech-relational-storage`) |

Domain components call dependency SDK traits (`TenantResolverClient`, `AuthZResolverClient` through `PolicyEnforcer`, `TypesRegistryClient`, `CredStoreClientV2`) obtained from ClientHub, and storage through repository traits. These traits are the seams where tests inject mocks.

Layer rules follow the [platform gear layout](../../../docs/toolkit_unified_system/02_gear_layout_and_sdk_pattern.md): serde and `ToSchema` derives appear only in `api/rest/` DTOs and in SDK models; domain types carry `#[domain_model]`; all endpoints are versioned. `unsafe` is forbidden and `unwrap`/`expect` are denied in non-test code.

Technology risks:

- PostgreSQL and SQLite differ in locking and type handling. Integration tests run on both (§4 Testability).
- JSON Schema validation cost grows with schema and document size. Documents are bounded by `max_json_document_bytes`.
- The per-replica chain cache can serve a stale chain. Staleness is bounded by the TTL (`cpt-cf-model-registry-component-chain-cache`).

## 2. Principles & Constraints

### 2.1 Design Principles

When principles conflict, Fail Closed and Owner-Only Writes take precedence over the others.

#### SDK-First Contract

- [ ] `p1` - **ID**: `cpt-cf-model-registry-principle-sdk-first-contract`

`ModelRegistryClientV1` is the public contract. Every REST endpoint maps to exactly one trait method with the same semantics and error categories. The REST layer adds only transport concerns: path and query parsing, flattened entry DTOs, `$select` projection, rejection of immutable fields in update bodies (`IMMUTABLE_FIELD`), status codes, and Problem rendering. Both surfaces call the same domain service and the same error mapping. Drivers: `cpt-cf-model-registry-fr-management-api-conventions`, `cpt-cf-model-registry-fr-resolve-model`.

#### One Visibility Rule

- [ ] `p1` - **ID**: `cpt-cf-model-registry-principle-slug-shadowing-allow-list`

Visibility is computed by one algorithm (§3.6 `cpt-cf-model-registry-seq-effective-catalog`). The provider slug decides which provider wins; the winner's `provider_id` decides which models are visible. Resolution, the available list, and the management reads all use this algorithm. No path filters models by deduplicating `canonical_id`. Drivers: `cpt-cf-model-registry-fr-effective-catalog`, `cpt-cf-model-registry-fr-provider-shadowing`.

#### Fail Closed

- [ ] `p1` - **ID**: `cpt-cf-model-registry-principle-fail-closed`

When the gear cannot decide, it refuses. A PDP denial or an unknown constraint denies. An unreachable PDP, Tenant Resolver, or Types Registry fails the request as dependency unavailable. A failed provider query fails the whole read; a partial provider set is never used. Drivers: `cpt-cf-model-registry-fr-authorization`, `cpt-cf-model-registry-fr-tenant-isolation`.

#### Catalog Rows Are Not Cached

- [ ] `p1` - **ID**: `cpt-cf-model-registry-principle-uncached-catalog-rows`

Provider and model rows are read from the database on every request. Only the tenant ancestor chain is cached ([ADR-0003](./ADR/0003-cpt-cf-model-registry-adr-caching-strategy.md)). A provider disable, a sharing change, a revocation, or a delete takes effect on the next request on every replica. Drivers: `cpt-cf-model-registry-fr-provider-disable`, `cpt-cf-model-registry-fr-model-approval`.

#### One Snapshot per Read

- [ ] `p1` - **ID**: `cpt-cf-model-registry-principle-snapshot-reads`

Every catalog read runs all its row queries in one read-only database transaction at snapshot isolation (§3.6 `cpt-cf-model-registry-seq-effective-catalog`). The provider winner, its `enabled` and `sharing_mode`, the approval state of the model, and every row of a list page come from one committed state. A request never combines rows from states that were never committed together. No dependency call runs inside the transaction: it starts after the chain lookup and after any PDP decision the operation takes first, and it ends before the PDP call of resolution. Drivers: `cpt-cf-model-registry-fr-provider-disable`, `cpt-cf-model-registry-fr-model-approval`, `cpt-cf-model-registry-fr-resolve-model`.

#### Validate on Write

- [ ] `p1` - **ID**: `cpt-cf-model-registry-principle-write-time-validation`

All GTS checks run on writes: provider type checks on provider create and settings validation on model create or settings update. Records that reach the database are valid against the schema that was current at write time. The resolution path performs no schema fetch and no schema validation. Drivers: `cpt-cf-model-registry-fr-model-settings-validation`, `cpt-cf-model-registry-nfr-resolution-latency`.

#### Owner-Only Writes

- [ ] `p1` - **ID**: `cpt-cf-model-registry-principle-owner-only-writes`

A tenant writes only rows it owns. It reads its own rows and the rows its ancestors share with it. Inherited rows are read-only, including their approval state. The root administrator manages the platform-wide shared catalog as the root tenant's own rows; it does not reach a self-managed tenant or its subtree, where the self-managed tenant's own shared rows take that role. There is no cross-tenant administration path in this gear. Drivers: `cpt-cf-model-registry-fr-tenant-isolation`, `cpt-cf-model-registry-fr-model-approval`.

### 2.2 Constraints

#### No Secret Storage

- [ ] `p1` - **ID**: `cpt-cf-model-registry-constraint-no-secret-storage`

The gear stores only a CredStore secret reference (`secret_ref`), never secret material. The reference uses the CredStore `SecretRef` format ([credstore-sdk](../../credstore/credstore-sdk/src/models.rs)): 1 to 255 characters from `[a-zA-Z0-9_-]`. The gear calls CredStore on writes only, to check that a `secret_ref` names a secret the writing tenant owns (§4 Credential Binding); the check is a metadata read that returns no secret value. Nothing on the resolution path calls CredStore. LLM Gateway resolves the reference through CredStore at evaluation time. The gear does not bind a secret reference to a destination; that control belongs to LLM Gateway and OAGW (§4 Security Assumptions).

#### Opaque Provider Settings

- [ ] `p1` - **ID**: `cpt-cf-model-registry-constraint-opaque-provider-settings`

The gear does not define or interpret provider-specific fields. `provider_settings` is a JSON document whose shape belongs to the LLM Gateway provider plugin that registered the provider type schema. The gear validates the document against that schema and stores it unchanged. Cost is not a provider-specific field: it is the `cost` object of Model Info, with one shape for every provider type.

#### OData Filters on Real Columns

- [ ] `p1` - **ID**: `cpt-cf-model-registry-constraint-real-column-filters`

The toolkit OData layer maps each filter field to exactly one real SeaORM column; it has no JSON-path filtering and no joins. Every filterable field is a real column with a B-tree index, low-cardinality columns included. Fields stored inside JSON sub-objects are promoted to real columns and rewritten on every create and update. No GIN indexes; SQLite and PostgreSQL use the same schema. The filterable fields and their columns are listed once, in §3.3 OData.

#### No Model Lifecycle Status

- [ ] `p1` - **ID**: `cpt-cf-model-registry-constraint-no-model-lifecycle`

Models have no lifecycle status. Model delete is a hard delete. Setting approval to Revoked is the non-destructive way to stop a model. Aliases, tags, model discovery, approval-service integration, and provider health are not part of this gear.

#### Immutable Identity

- [ ] `p1` - **ID**: `cpt-cf-model-registry-constraint-immutable-identity`

Provider `slug`, model `provider_id`, model `name`, and the owning `tenant_id` of both entities never change after create. These fields are absent from the update requests. As a result, `canonical_id` never changes. The provider `gts_type` is not caller-immutable: it changes only through type migration (§3.6 `cpt-cf-model-registry-seq-provider-write`), which rewrites the `gts_type` of every model of the provider in the same transaction, so a model's `gts_type` always equals its provider's and is never set by a caller. This is the canonical list of immutable fields; other sections refer to it.

#### Local Chain Cache Only

- [ ] `p1` - **ID**: `cpt-cf-model-registry-constraint-local-chain-cache`

The ancestor-chain cache is an in-process map per replica ([ADR-0003](./ADR/0003-cpt-cf-model-registry-adr-caching-strategy.md)). The gear uses no platform cache primitive and no `cluster` gear. The cache holds tenant ids only. Hierarchy changes take effect within the TTL bound of `cpt-cf-model-registry-component-chain-cache`, as the PRD contract `cpt-cf-model-registry-contract-tenant-hierarchy` allows.

## 3. Technical Architecture

### 3.1 Domain Model

**Technology**: Rust structs in the SDK crate; provider types are GTS types derived from the provider base type `gts.cf.genai.model.provider.v1~`.

**Location**: [model-registry-sdk/src/models/](../model-registry-sdk/src/models/).

**Core Entities**:

| Entity | Description | Schema |
|--------|-------------|--------|
| Provider | Where models come from: a tenant-owned record with slug, name, provider GTS type, enabled flag, and sharing mode | [models/](../model-registry-sdk/src/models/) (`ProviderV1`) |
| Model | A tenant-owned catalog entry for one model of one provider: identity, sharing, approval, audit, and the model info | [models/](../model-registry-sdk/src/models/) (`ModelV1`) |
| Model Info | The record LLM Gateway uses to evaluate the model; the shape of its `provider_settings` is fixed by the provider type | [models/](../model-registry-sdk/src/models/) (`ModelInfoV1`) |
| Provider Type | GTS type derived from `gts.cf.genai.model.provider.v1~` and published by an LLM Gateway provider plugin; its schema constrains `provider_settings` | Types Registry |
| Approval | The owning tenant's latest decision on a model | [models/](../model-registry-sdk/src/models/) (`ApprovalV1`) |
| Effective Catalog | The computed view of providers and models one tenant sees | Computed per request; not stored |

Type notation in the field tables: `?` marks an optional field; `uuid`, `string`, `bool`, `u32`, `u64`, `i32`, `f64`, `timestamp` (UTC, ISO-8601), `enum`, `json` (any JSON value), `object` (a nested structure described below the table). Enum values are lowercase on the wire; the PRD's `Tenant`/`Shared` and Pending/Approved/Rejected/Revoked are the same values. Strings without a stated limit are bounded by their column length in §3.7, and JSON values by `max_json_document_bytes`; a value over a bound is rejected with `TOO_LARGE`.

#### Provider

- [ ] `p1` - **ID**: `cpt-cf-model-registry-entity-provider`

| Field | Type | Meaning |
|-------|------|---------|
| `id` | uuid | Provider id, generated by the gear (UUIDv7). Management operations address the provider by this id. |
| `tenant_id` | uuid | Owning tenant. Immutable. |
| `slug` | string | 1–64 characters: lowercase letters, digits, hyphen. Unique among the providers of the owning tenant. Immutable. Shadowing key and left part of every canonical id. |
| `name` | string | Display name, 1–255 characters, free text. |
| `gts_type` | string | Provider GTS type: a type derived from `gts.cf.genai.model.provider.v1~`, for example `gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~`. Names the LLM Gateway plugin family that evaluates the provider's models. Changes only through type migration, together with the `gts_type` of every model of the provider (§3.6 `cpt-cf-model-registry-seq-provider-write`). |
| `enabled` | bool | When false, no model of this provider can be evaluated by the owner or any inheriting tenant. Default true. |
| `sharing_mode` | enum | `tenant` (owner only) or `shared` (owner and all descendants). Default `tenant`. |
| `managed` | bool | Whether the platform manages the provider runtime, for example a local runtime. Informational; the gear does not act on it. Default false. |
| `metadata` | json? | Optional provider metadata. Opaque; stored and returned unchanged. |
| `version` | i64 | Row version: 1 on create, incremented by every update. Returned as `ETag` on REST; updates are conditional on it (§3.3). |
| `created_at`, `created_by` | timestamp, uuid | When and by which subject the provider was created. |
| `updated_at`, `updated_by` | timestamp, uuid | When and by which subject the provider was last changed. |

#### Model

- [ ] `p1` - **ID**: `cpt-cf-model-registry-entity-model`

| Field | Type | Meaning |
|-------|------|---------|
| `id` | uuid | Model id, generated by the gear (UUIDv7). Management operations address the model by this id. |
| `tenant_id` | uuid | Owning tenant. Always equal to the provider's `tenant_id`. Immutable. |
| `provider_id` | uuid | The provider that serves the model. Must be owned by the same tenant. Immutable. |
| `name` | string | 1–128 characters: lowercase letters, digits, hyphen, dot, underscore. Chosen by the caller on create. Unique among the models of the provider. Immutable. Right part of the canonical id; independent of `provider_model_id`. |
| `canonical_id` | string | `provider_slug::name`, for example `openai::gpt-4o`. Built by the gear. Unique among the models of the owning tenant. Never changes. |
| `sharing_mode` | enum | `tenant` or `shared`. Descendants see the model only when it is `shared` and its provider is `shared`. Default `tenant`. |
| `approval` | object | Approval decision, see Approval. |
| `info` | object | Model info, see Model Info. |
| `version` | i64 | Row version: 1 on create, incremented by every update. Returned as `ETag` on REST; updates are conditional on it (§3.3). |
| `created_at`, `created_by` | timestamp, uuid | Creation time and subject. |
| `updated_at`, `updated_by` | timestamp, uuid | Last change time and subject. |

#### Model Info

- [ ] `p1` - **ID**: `cpt-cf-model-registry-entity-model-info`

`ModelInfoV1` is the SDK record that LLM Gateway uses to evaluate a model; it is not a GTS type. Its `gts_type` names the provider type: a GTS type derived from the provider base type `gts.cf.genai.model.provider.v1~` (§3.3 `cpt-cf-model-registry-interface-model-info-schema`). The provider type's schema fixes the shape of `provider_settings`. The registry returns `ModelInfoV1` unchanged to LLM Gateway, which narrows `provider_settings` to the plugin's typed view.

| Field | Type | Meaning |
|-------|------|---------|
| `gts_type` | string | The provider GTS type, copied from the model's provider on create and rewritten by a type migration of the provider. Read-only for callers; never part of a create or update request. LLM Gateway selects the plugin whose provider type equals this value. |
| `provider_model_id` | string | The provider's own model identifier, sent to the provider. 1–255 characters without whitespace or control characters. Mutable; not part of the canonical id. Several models of one provider may share it, for example the same provider model in two regions or with different settings. |
| `base_url` | string? | General setting: the absolute `http` or `https` URL of the model's endpoint, up to 2,048 characters, with no user information (`user:password@`). The plugin calls it only through OAGW: the host and port name the OAGW upstream alias, and the path is the path prefix of the call. The scheme on the wire is the OAGW upstream's, and OAGW admits only HTTPS upstreams (`cpt-cf-oagw-constraint-https-only`). SSRF protection is handled by OAGW at call time (§4 Security Assumptions). When absent, the LLM Gateway provider plugin uses its provider's default URL. The registry stores no default and returns the field as absent. |
| `secret_ref` | string? | General setting: optional CredStore secret reference for the provider credential. Must name a secret owned by the model's tenant (§4 Credential Binding). |
| `display_name` | string | Name shown to users, 1–255 characters. |
| `description` | string? | Free-text description, up to 4,096 characters. |
| `family` | string? | Model family label, for example `gpt-4` or `llama`. |
| `vendor` | string? | Organization that produced the model weights. Independent of the provider that serves it. |
| `managed` | bool | Whether the platform can load and unload this model on a managed runtime. Informational. Default false. |
| `architecture` | string? | Architecture class, for example `llama` or `mistral`. |
| `size_bytes` | u64? | On-disk size of locally hosted weights, at most 2^63 − 1 (stored as signed 64-bit). |
| `format` | string? | Weight or serving format, for example `gguf` or `safetensors`. |
| `region` | string? | Deployment region, for example `eu-west-1`. |
| `hosted_by` | string? | Infrastructure host, for example `Azure` or `self-hosted`. |
| `last_release_at` | timestamp? | When the vendor last released this model version. |
| `reasoning_level` | string? | Display-only reasoning level label. |
| `model_version` | string? | The model's release version as published by the vendor. |
| `sort_order` | i32 | Position in model pickers; lower first. Default 0. |
| `icon` | string? | Absolute `http` or `https` URL of the model icon, up to 2,048 characters. Other schemes are rejected with `INVALID_FORMAT`. |
| `multiplier_display` | string? | Display label for the relative cost, for example `3x`. |
| `cost` | object | Cost in AI credits, see Cost. |
| `performance` | object | Expected performance: `response_latency_ms` (u32?) and `tokens_per_second` (u32?). |
| `additional_info` | json | Map from string keys to JSON values for deployment-specific metadata. Default empty. |
| `supported_api` | set of enum | API kinds the model serves: `completion`, `embedding`, `batch`. A closed set owned by the registry; adding a value is a non-breaking change within `v1`. |
| `capabilities` | object | What the model can do, see Capabilities. |
| `disabled_capabilities` | object | Capabilities an administrator turned off for this model, see Disabled Capabilities. |
| `context_window` | object | `max_input_tokens` (u32), `max_output_tokens` (u32?, absent for embedding models), `output_vector_size` (u32?, embedding dimension). |
| `default_parameters` | object | Provider-independent inference defaults, see Default Parameters. |
| `allow_parameter_override` | bool | Whether callers may override `default_parameters` per request. Default false. |
| `allow_extra_params` | list of string | Names of extra request parameters callers may pass to the provider, up to 64 names of up to 128 characters. Default empty. |
| `provider_settings` | json | Provider-specific settings. Shape defined by the provider type schema. Opaque to the registry beyond validation. |

**Capabilities** (`capabilities`). A media capability has two fields: `enabled` (bool) and `supported_mime_types` (list of up to 64 strings). MIME types follow RFC 6838 in lowercase. `disabled_mime_types` has the same bound.

| Field | Type | Meaning |
|-------|------|---------|
| `vision` | media capability | Image input. |
| `reasoning` | object | `effort`, `toggle`, `resume`, `budget`, `summary` (all bool): support for reasoning effort, switching reasoning on or off, resuming a reasoning chain, a reasoning token budget, and a reasoning summary. |
| `tools` | object | `enabled`, `parallel_tool_calls`, `max_tool_calls`, `tool_choice_required`, `tool_choice_none`, `tool_choice_named` (all bool): tool or function calling, and support for parallel tool calls, a tool call limit, and the `required`, `none`, and named `tool_choice` forms. |
| `output` | object | `json_mode`, `json_schema`, `strict_schema`, `verbosity` (all bool): JSON output without a schema, structured output against a JSON schema, strict schema enforcement, and the text verbosity setting. |
| `sampling` | object | `temperature`, `top_p`, `seed`, `presence_penalty`, `frequency_penalty`, `logprobs` (all bool): support for each sampling parameter; `logprobs` covers `top_logprobs` and the log-probabilities include. |
| `streaming` | bool | Streaming responses. |
| `file_input` | media capability | Document or file input. |
| `image_generation` | media capability | Image output. |
| `audio_input` | media capability | Audio input. |
| `audio_output` | media capability | Audio output. |
| `code_interpreter` | bool | Sandboxed code execution. |
| `web_search` | object | `enabled`, `allowed_domains`, `excluded_domains` (all bool): web search, and support for allow-lists and deny-lists of domains. |
| `truncation` | bool | The `truncation` request parameter. |
| `instructions` | bool | System-level `instructions`. |

Every capability field is optional in JSON and reads as "not supported" when absent.

**Disabled Capabilities** (`disabled_capabilities`) mirror the capability structure; every flag reads as "disabled". Media entries have `disabled` (bool) and `disabled_mime_types` (list of string). `reasoning` has `effort`, `toggle`, `resume`, `budget`, `summary`. `tools` has `disabled` plus `parallel_tool_calls`, `max_tool_calls`, `tool_choice_required`, `tool_choice_none`, `tool_choice_named`. `output` and `sampling` have the same fields as their capability. `web_search` has `disabled`, `allowed_domains`, `excluded_domains`. The remaining entries are single booleans. Absent fields read as "not disabled". The effective capability is the declared capability minus the disabled one.

**Default Parameters** (`default_parameters`). All fields are optional. The registry stores them and applies no range limits.

| Field | Type | Meaning |
|-------|------|---------|
| `temperature` | f64? | Sampling temperature. |
| `top_p` | f64? | Nucleus sampling mass. |
| `max_output_tokens` | u32? | Maximum output tokens. |
| `max_tool_calls` | u32? | Maximum tool calls per response. |
| `presence_penalty` | f64? | Presence penalty. |
| `frequency_penalty` | f64? | Frequency penalty. |
| `top_logprobs` | u8? | Number of top log-probabilities per token. |
| `truncation` | enum? | `auto` or `disabled`: context truncation strategy. |
| `service_tier` | enum? | `auto` or `default`. |
| `parallel_tool_calls` | bool? | Whether the model may issue tool calls in parallel. |
| `text` | object? | `format`: `text`, `json_object`, or `json_schema` with `name`, `description?`, `schema?`, `strict`; `verbosity?`: `low`, `medium`, `high`. |
| `reasoning` | object? | `effort?`: `none`, `low`, `medium`, `high`, `xhigh`; `summary?`: `concise`, `detailed`, `auto`. |
| `tool_choice` | enum? | `auto`, `required`, `none`, or a named function. |

**Cost** (`cost`). Rates in micro-credits: 1 AI credit = 1,000,000 micro-credits. Every field is optional; an absent rate means the model does not price that kind of usage. The registry stores the rates and applies no range limits. LLM Gateway computes the cost of a call from them. Default: all absent.

| Field | Type | Meaning |
|-------|------|---------|
| `input_per_1m_micro` | u64? | Per 1,000,000 input tokens. |
| `output_per_1m_micro` | u64? | Per 1,000,000 output tokens. |
| `cache_read_per_1m_micro` | u64? | Per 1,000,000 input tokens read from the provider's prompt cache. |
| `cache_write_per_1m_micro` | u64? | Per 1,000,000 input tokens written to the provider's prompt cache. |
| `web_search_per_1k_calls_micro` | u64? | Per 1,000 web search calls. |

#### Approval

- [ ] `p1` - **ID**: `cpt-cf-model-registry-entity-approval`

| Field | Type | Meaning |
|-------|------|---------|
| `state` | enum | `pending`, `approved`, `rejected`, or `revoked`. Only `approved` models can be evaluated. |
| `decided_by` | uuid | Subject that made the latest decision. |
| `decided_at` | timestamp | Time of the latest decision. |

Any create or update request that carries an approval state records a decision, even when the state does not change. A create without an approval state stores `approved`, decided by the creating subject at creation time. Any state may follow any other state.

#### Effective Catalog

- [ ] `p1` - **ID**: `cpt-cf-model-registry-entity-effective-catalog`

The effective catalog of tenant T0 is computed per request from T0's ancestor chain and the provider and model rows of the tenants in it (§3.6 `cpt-cf-model-registry-seq-effective-catalog`). It is never stored. Its parts are: the candidate providers, the winning provider per slug, the allow-list (winning providers that are enabled), and the visible models (models of winning providers that T0 owns or that are `shared`).

**Relationships**:
- Provider → Tenant: many-to-one; a provider belongs to exactly one tenant.
- Model → Provider: many-to-one through `provider_id`; the model and its provider have the same tenant. Deleting a provider deletes its models.
- Model → Approval: one-to-one, embedded in the model row.
- Model → Model Info: one-to-one, embedded in the model row.
- Model Info → Provider type schema: `gts_type` names the schema in Types Registry that constrains `provider_settings`.

**Invariants**:
- `(tenant_id, slug)` is unique for providers; `(tenant_id, canonical_id)` and `(provider_id, name)` are unique for models. `provider_model_id` is not unique.
- A model's `tenant_id` and `gts_type` equal its provider's.
- Identity fields listed in `cpt-cf-model-registry-constraint-immutable-identity` never change.
- Every model row's `provider_settings` passed schema validation when it was last written.

### 3.2 Component Model

```mermaid
graph LR
    subgraph SDKC["SDK crate"]
        SDK["ModelRegistryClientV1, models, ModelRegistryError, OData schemas"]
    end
    subgraph GearC["Gear crate"]
        RESTC["REST API"]
        LC["Local client"]
        CS["Catalog service"]
        VR["Visibility resolver"]
        AGC["Access guard"]
        GVC["GTS validator"]
        CC["Chain cache"]
        ST["Storage"]
    end
    HUB["ClientHub"]
    TRS["Tenant Resolver SDK"]
    AZ["AuthZ Resolver SDK"]
    TYS["Types Registry SDK"]
    CRSS["CredStore SDK"]
    DBS[("Database")]

    LC -.implements.-> SDK
    LC -->|registered in| HUB
    RESTC --> CS
    LC --> CS
    CS --> VR
    CS --> AGC
    CS --> GVC
    CS --> CRSS
    CS --> ST
    VR --> CC
    VR --> ST
    CC --> TRS
    AGC --> AZ
    GVC --> TYS
    ST --> DBS
```

#### SDK

- [ ] `p1` - **ID**: `cpt-cf-model-registry-component-sdk`

##### Why this component exists

Consumers need a stable, transport-free contract that does not depend on gear internals.

##### Responsibility scope

Crate `cf-gears-model-registry-sdk`. Holds the `ModelRegistryClientV1` trait, the request and response models (`ProviderV1`, `ModelV1`, `ModelInfoV1`, `ApprovalV1`, `ProviderEntryV1`, `ModelEntryV1`, create and update requests), the OData query schemas and field references for providers, models, and available models, the public reason-code and field-code constants, and the opt-in typed error projection `ModelRegistryError`. Declares the provider base type `gts.cf.genai.model.provider.v1~` (§3.3 `cpt-cf-model-registry-interface-model-info-schema`).

##### Responsibility boundaries

No storage, no HTTP types, no provider-specific settings types. Trait methods return the platform `CanonicalError`; `ModelRegistryError` is a projection over it, not part of the trait signature.

##### Related components (by ID)

- `cpt-cf-model-registry-component-local-client` — implemented by
- `cpt-cf-model-registry-component-rest-api` — shares models with (DTOs convert from SDK models)

#### REST API

- [ ] `p1` - **ID**: `cpt-cf-model-registry-component-rest-api`

##### Why this component exists

Administrators and out-of-process callers reach the gear over HTTP through API Gateway.

##### Responsibility scope

Registers the endpoints of §3.3 with `OperationBuilder` (`.authenticated()`, the platform license check, OData helpers, `.standard_errors()`), parses paths, bodies, and OData options, applies `$select` projection to responses, and renders errors as canonical Problem documents through the same domain-to-canonical mapping the SDK uses.

##### Responsibility boundaries

No business rules. No database or dependency calls. Every handler calls exactly one catalog-service operation.

##### Related components (by ID)

- `cpt-cf-model-registry-component-catalog-service` — calls

#### Local Client

- [ ] `p1` - **ID**: `cpt-cf-model-registry-component-local-client`

##### Why this component exists

In-process gears, LLM Gateway first, call the registry through ClientHub without HTTP.

##### Responsibility scope

Implements `ModelRegistryClientV1` over the catalog service and is registered in ClientHub as `dyn ModelRegistryClientV1` during gear `init`. Converts domain errors to `CanonicalError` once per method.

##### Responsibility boundaries

No logic beyond delegation and error conversion.

##### Related components (by ID)

- `cpt-cf-model-registry-component-sdk` — implements
- `cpt-cf-model-registry-component-catalog-service` — calls

#### Catalog Service

- [ ] `p1` - **ID**: `cpt-cf-model-registry-component-catalog-service`

##### Why this component exists

One place defines the order of checks and side effects for every operation.

##### Responsibility scope

Implements the twelve operations of §3.3:

- validates request fields;
- calls the access guard, the visibility resolver, and the GTS validator in the order defined in §3.6;
- checks `secret_ref` ownership through the CredStore client (§4 Credential Binding);
- builds `canonical_id`, copies `gts_type` from the provider, and sets approval and audit fields from the `SecurityContext` subject;
- runs each read in one read-only snapshot transaction (`cpt-cf-model-registry-principle-snapshot-reads`) and each write in one read-write transaction, and enforces `expected_version` (§3.3);
- computes the `inherited`, `shadowed`, `provider_enabled`, and `available_for_eval` flags on management reads.

##### Responsibility boundaries

Does not compute visibility itself and does not talk to the PDP, Tenant Resolver, or Types Registry directly; CredStore is called for the ownership check only. Does not interpret `provider_settings`.

##### Related components (by ID)

- `cpt-cf-model-registry-component-visibility-resolver` — calls
- `cpt-cf-model-registry-component-access-guard` — calls
- `cpt-cf-model-registry-component-gts-validator` — calls
- `cpt-cf-model-registry-component-storage` — calls

#### Visibility Resolver

- [ ] `p1` - **ID**: `cpt-cf-model-registry-component-visibility-resolver`

##### Why this component exists

Inheritance and shadowing must give the same answer on every path.

##### Responsibility scope

Gets the caller's chain from the chain cache, loads candidate providers through storage inside the operation's read transaction, and computes winners per slug, the allow-list, and the shadowed set (§3.6 `cpt-cf-model-registry-seq-effective-catalog`). Provides three views: the winner for one slug (resolution), the allow-list and own-or-shared model predicate (available list), and the candidate sets with flags (management reads).

##### Responsibility boundaries

Does not apply approval or PDP gates. Holds no state between requests.

##### Related components (by ID)

- `cpt-cf-model-registry-component-chain-cache` — calls
- `cpt-cf-model-registry-component-storage` — calls

#### Access Guard

- [ ] `p1` - **ID**: `cpt-cf-model-registry-component-access-guard`

##### Why this component exists

Every operation needs a PDP decision, and the tenant dimension of inherited data needs a single, consistent treatment.

##### Responsibility scope

Applies the decision rules of §4 Authorization:

- declares the PDP resource types and actions;
- holds a `PolicyEnforcer` whose deadline is `pdp_deadline_ms` instead of the enforcer's 30 s default;
- asks the PDP with the acting tenant as `owner_tenant_id`, the resource id for point operations, and the model properties for evaluation;
- evaluates tenant constraints as a gate on the acting tenant, and turns row constraints into a row predicate for lists or an in-memory check for single rows;
- maps enforcer errors: denial or compile failure to `NOT_PERMITTED`, evaluation failure to dependency unavailable.

##### Responsibility boundaries

Does not decide catalog visibility; that is the visibility resolver's job.

##### Related components (by ID)

- `cpt-cf-model-registry-component-catalog-service` — called by

#### GTS Validator

- [ ] `p1` - **ID**: `cpt-cf-model-registry-component-gts-validator`

##### Why this component exists

Provider types and provider settings come from plugins the gear does not know at build time; they must be checked against Types Registry on write.

##### Responsibility scope

Checks provider GTS types on provider create and on type migration (§4 GTS Checks). Fetches the resolved schema of a provider type from Types Registry and validates `provider_settings` against it on model create, on settings update, and for every model of a provider on type migration. Publishes nothing itself.

##### Responsibility boundaries

Not called on the resolution path. Keeps no schema cache. Does not check that an LLM Gateway plugin instance exists for a type.

##### Related components (by ID)

- `cpt-cf-model-registry-component-catalog-service` — called by

#### Chain Cache

- [ ] `p1` - **ID**: `cpt-cf-model-registry-component-chain-cache`

##### Why this component exists

Without it, every resolution calls Tenant Resolver. That wait is outside the in-gear latency NFR but inside LLM Gateway's 50 ms overhead budget (PRD rationale of `cpt-cf-model-registry-nfr-resolution-latency`).

##### Responsibility scope

An in-process map from tenant id to that tenant's ordered ancestor chain (parent first, root last) plus the start time of the Tenant Resolver call that fetched it ([ADR-0003](./ADR/0003-cpt-cf-model-registry-adr-caching-strategy.md)). The chain is read with the default `BarrierMode::Respect` and drives inheritance and shadowing ([ADR-0001](./ADR/0001-cpt-cf-model-registry-adr-inherited-read-scope.md)). On a miss or an expired entry it calls `TenantResolverClient::get_ancestors` once, bounded by `tenant_resolver_deadline_ms`, and stores the result in one entry. Rules:

1. TTL is `chain_cache_ttl_seconds`, default 30, maximum 300 (checked at gear start). Value 0 disables caching; every request calls Tenant Resolver. Each stored entry gets a TTL reduced by a random jitter of up to 10%, so entries stored together do not expire together. Expiry counts from the start of the Tenant Resolver call, so a slow call does not extend the bound of rule 5; a result already expired on arrival serves its request and is not stored.
2. An empty chain (the root tenant or a self-managed tenant) is a cached value, not a miss.
3. Failures are not cached. A failed call fails the request with dependency unavailable; no entry is stored.
4. The map is a bounded concurrent LRU cache of `chain_cache_max_entries` (default 100,000) entries. Inserting into a full map evicts the least recently used entry in constant time.
5. There is no invalidation API. A stale entry is used for at most one TTL plus the duration of the request that read it. A stale chain can narrow or widen visibility. A tenant moved under a new parent keeps its former ancestors' shared catalog and does not yet see the new ancestors' shared providers, including ones that would shadow an inherited slug. A detached tenant keeps its former ancestors' shared catalog. A tenant whose `self_managed` flag changes keeps its previous chain, and so does each tenant below it. Deployments that need hierarchy changes to apply on the next request set the TTL to 0.
6. Concurrent misses for the same tenant share one Tenant Resolver call (single flight per tenant). At TTL 0 every request makes its own call, so a request never receives a chain fetched by a call that started before it.
7. Expiry is checked against an injectable clock, the same clock that sets audit and approval timestamps.

##### Responsibility boundaries

Holds tenant ids only, never provider or model data. The cache affects visibility only within the staleness bound of rule 5. With the cache empty or disabled, every request depends on Tenant Resolver. The key is the tenant id alone; the chain is fixed per tenant and does not depend on the caller.

##### Related components (by ID)

- `cpt-cf-model-registry-component-visibility-resolver` — called by

#### Storage

- [ ] `p1` - **ID**: `cpt-cf-model-registry-component-storage`

##### Why this component exists

Durable, tenant-scoped persistence of providers and models.

##### Responsibility scope

SeaORM entities for `model_registry__providers` and `model_registry__models` deriving `Scopable`, with `tenant_id` as the tenant column, `id` as the resource column, and PEP properties `provider_id`, `canonical_id`, and `gts_type`. Repositories over `DBRunner` that run every query through `SecureConn` with an `AccessScope`. Read operations open their transaction with `TxConfig { isolation: RepeatableRead, access_mode: ReadOnly }` (`cpt-cf-model-registry-principle-snapshot-reads`): on PostgreSQL every statement of the transaction reads one snapshot, and on SQLite every transaction is serializable and its read view is stable for its duration. Write operations use the default read-write transaction. Whole-row projection between the SDK model and the columns, including promoted filter columns. Resolution and the available list read providers through a narrow projection (`id`, `tenant_id`, `slug`, `enabled`, `sharing_mode`); `metadata` and other provider fields are read only by management operations. OData field-to-column mappings (§3.3). Migrations exposed through the gear's database capability.

##### Responsibility boundaries

No raw SQL outside migrations. No visibility or approval logic.

##### Related components (by ID)

- `cpt-cf-model-registry-component-catalog-service` — called by
- `cpt-cf-model-registry-component-visibility-resolver` — called by

### 3.3 API Contracts

The gear realizes the PRD interfaces `cpt-cf-model-registry-interface-sdk-client` and `cpt-cf-model-registry-interface-rest-api`, and the integration contracts `cpt-cf-model-registry-contract-provider-settings-schema` and `cpt-cf-model-registry-contract-tenant-hierarchy`.

#### SDK Client Trait

- [ ] `p1` - **ID**: `cpt-cf-model-registry-interface-client-v1`

- **Contracts**: `cpt-cf-model-registry-contract-provider-settings-schema`, `cpt-cf-model-registry-contract-tenant-hierarchy`
- **Technology**: Rust trait `ModelRegistryClientV1` in `cf-gears-model-registry-sdk`, obtained through ClientHub; realizes `cpt-cf-model-registry-interface-sdk-client`
- **Location**: [model-registry-sdk/src/](../model-registry-sdk/src/). Consumer obligations are in §3.3 Consumer Contract.

Every method takes `&SecurityContext` and returns `Result<_, CanonicalError>`. The acting tenant is the context's subject tenant.

| Method | Input | Output | Behavior |
|--------|-------|--------|----------|
| `resolve_model` | canonical id | `ModelV1` | Resolution for evaluation (§3.6 `cpt-cf-model-registry-seq-resolve-model`). |
| `list_available_models` | `ODataQuery` | `Page<ModelV1>` | Exactly the models `resolve_model` would return for the caller, as full records. Permissions per §4 Authorization (§3.6 `cpt-cf-model-registry-seq-list-available-models`). |
| `create_provider` | `CreateProviderRequestV1` | `ProviderEntryV1` | Creates a provider in the acting tenant. |
| `get_provider` | provider id | `ProviderEntryV1` | Own or inherited provider. |
| `list_providers` | `ODataQuery` | `Page<ProviderEntryV1>` | Own providers plus inherited candidates. |
| `update_provider` | provider id, `UpdateProviderRequestV1` | `ProviderEntryV1` | Own providers only; conditional on `expected_version`. A request whose `gts_type` differs from the stored one is a type migration (§3.6 `cpt-cf-model-registry-seq-provider-write`). |
| `delete_provider` | provider id, `expected_version?` | nothing | Own providers only; deletes its models. |
| `create_model` | `CreateModelRequestV1` | `ModelEntryV1` | Creates a model under an own provider. |
| `get_model` | model id | `ModelEntryV1` | Own or inherited model. |
| `list_models` | `ODataQuery` | `Page<ModelEntryV1>` | Admin list under the model `list` action: own models plus inherited candidates in every state, including models of disabled providers and models that are not approved, with flags. |
| `update_model` | model id, `UpdateModelRequestV1` | `ModelEntryV1` | Own models only; may change approval; conditional on `expected_version`. |
| `delete_model` | model id, `expected_version?` | nothing | Own models only; hard delete. |

Request contents:

| Request | Fields |
|---------|--------|
| `CreateProviderRequestV1` | `slug`, `name`, `gts_type`, `enabled?` (default true), `sharing_mode?` (default `tenant`), `managed?` (default false), `metadata?` |
| `UpdateProviderRequestV1` | `expected_version?`, and any of `name`, `enabled`, `sharing_mode`, `managed`, `metadata` (null clears), `gts_type` (type migration) |
| `CreateModelRequestV1` | `provider_id`, `name`, `sharing_mode?` (default `tenant`), `approval_state?` (default `approved`), and every Model Info field except `gts_type` |
| `UpdateModelRequestV1` | `expected_version?`, and any of `sharing_mode`, `approval_state`, and every Model Info field except `gts_type`; optional fields accept null to clear |

Update semantics: a field present in the request replaces the stored value as a whole, including nested objects such as `capabilities`, `cost`, `default_parameters`, and `provider_settings`. Absent fields stay unchanged.

Optimistic concurrency ([ADR-0004](./ADR/0004-cpt-cf-model-registry-adr-update-semantics.md)): `ProviderV1` and `ModelV1` carry `version`. An update or delete with `expected_version` applies only when the stored `version` equals it; otherwise it fails with `OPTIMISTIC_LOCK_FAILURE` (§4 Error Handling) and nothing changes. Every successful update increments `version`. An update without `expected_version` is an explicit unconditional write; on REST it must be requested with `If-Match: *`.

Management responses:

| Type | Fields |
|------|--------|
| `ProviderEntryV1` | `provider` (`ProviderV1`); `inherited` (owned by an ancestor); `shadowed` (inherited and not the winner of its slug for the caller); `available_for_eval` (winner of its slug and enabled) |
| `ModelEntryV1` | `model` (`ModelV1`); `inherited`; `shadowed` (its provider is not the winner of its slug for the caller); `provider_enabled`; `available_for_eval` (not shadowed, provider enabled, and approved) |

`available_for_eval` reflects catalog state only. It does not include the PDP `evaluate` decision, which depends on the subject; `list_available_models` is the authority for a given subject.

OData on lists: `$filter`, `$orderby`, `$select`, `limit` (alias `$top`), and `cursor` (alias `$skiptoken`).

- Page size defaults to `default_page_size` (20) and is capped at `max_page_size` (100). Lists read whole rows; the page cap and the document size limits bound a page.
- Default order is `canonical_id asc` for models and `slug asc` for providers, with `id` as the final tie-breaker. Pages are keyset pages.
- `$orderby` accepts only non-nullable fields. Nullable fields are filter-only, and a `$orderby` on one is rejected as `invalid_argument` with a field violation. Filter-only model fields: `family`, `vendor`, `hosted_by`, `region`, `model_version`, `architecture`, `format`, `last_release_at`, `max_output_tokens`. The other fields in the table below are orderable, and every provider field is orderable.
- `$select` is applied to the serialized response on REST. The SDK always returns whole records.

Filter and order fields, with their columns (this is the canonical list):

| List | Group | Fields | Column |
|------|-------|--------|--------|
| Available models and models | Identity | `id`, `tenant_id`, `provider_id`, `name`, `canonical_id`, `provider_model_id`, `gts_type`, `sharing_mode` | Same name |
| Available models and models | Approval | `approval_state`, `approval_decided_at` | Same name |
| Available models and models | Capabilities (effective values) | `vision`, `reasoning_effort`, `tools`, `json_schema`, `streaming`, `file_input`, `image_generation`, `audio_input`, `audio_output`, `code_interpreter`, `web_search` | `cap_<field>` |
| Available models and models | Supported API | `api_completion`, `api_embedding`, `api_batch` | Same name |
| Available models and models | User-facing | `display_name`, `family`, `vendor`, `hosted_by`, `region`, `model_version`, `managed`, `architecture`, `format`, `sort_order`, `last_release_at` | Same name |
| Available models and models | Context window | `max_input_tokens`, `max_output_tokens` | `ctx_<field>`: the `context_window` limits, not `default_parameters.max_output_tokens` |
| Available models and models | Audit | `created_at`, `updated_at` | Same name |
| Providers | All | `id`, `tenant_id`, `slug`, `name`, `gts_type`, `enabled`, `sharing_mode`, `managed`, `created_at`, `updated_at` | Same name |

The flags `inherited`, `shadowed`, `provider_enabled`, and `available_for_eval` are response fields only and are not filterable. A filter can only narrow a result set: it is ANDed onto the mandatory visibility predicates and cannot switch them off.

#### REST Endpoints

- [ ] `p1` - **ID**: `cpt-cf-model-registry-interface-rest-v1`

- **Contracts**: `cpt-cf-model-registry-contract-provider-settings-schema`, `cpt-cf-model-registry-contract-tenant-hierarchy`
- **Technology**: REST over JSON, generated OpenAPI from `OperationBuilder` registrations; realizes `cpt-cf-model-registry-interface-rest-api`
- **Location**: the `api/rest` layer of the gear crate; published in the platform OpenAPI document

**Endpoints Overview**:

| Method | Path | Description | Stability |
|--------|------|-------------|-----------|
| `GET` | `/model-registry/v1/available-models/{canonical_id}` | `resolve_model`. The canonical id is one percent-encoded path segment: clients encode `/`, `%`, `?`, and `#`. API Gateway routes on the encoded path and the handler decodes the segment once, so `%2F` stays inside the segment. 200 with the full `ModelV1`, the same record the SDK returns. | stable |
| `GET` | `/model-registry/v1/available-models` | `list_available_models`. 200 with a page of `ModelV1`. | stable |
| `POST` | `/model-registry/v1/providers` | `create_provider`. 201 with `Location`, `ETag`, and the entry. | stable |
| `GET` | `/model-registry/v1/providers/{id}` | `get_provider`. 200 with `ETag`. | stable |
| `GET` | `/model-registry/v1/providers` | `list_providers`. 200 with a page. | stable |
| `PATCH` | `/model-registry/v1/providers/{id}` | `update_provider`. Requires `If-Match`. 200 with `ETag` and the entry. | stable |
| `DELETE` | `/model-registry/v1/providers/{id}` | `delete_provider`. Optional `If-Match`. 204. | stable |
| `POST` | `/model-registry/v1/models` | `create_model`. 201 with `Location`, `ETag`, and the entry. | stable |
| `GET` | `/model-registry/v1/models/{id}` | `get_model`. 200 with `ETag`. | stable |
| `GET` | `/model-registry/v1/models` | `list_models`. 200 with a page. | stable |
| `PATCH` | `/model-registry/v1/models/{id}` | `update_model`. Requires `If-Match`. 200 with `ETag` and the entry. | stable |
| `DELETE` | `/model-registry/v1/models/{id}` | `delete_model`. Optional `If-Match`. 204. | stable |

REST conventions:

- Every route is registered with `OperationBuilder`, `.authenticated()`, the platform license check with no gear-specific feature, `.standard_errors()` plus the 503 response, and operation id `model_registry.<sdk_method>`. List routes add `.with_odata_filter`, `.with_odata_orderby`, and `.with_odata_select` for their field enums.
- JSON field names are snake_case. REST DTOs flatten the entry types: provider or model fields plus the flags at the top level.
- REST update DTOs reject the immutable fields of `cpt-cf-model-registry-constraint-immutable-identity`, `canonical_id`, `version`, the model `gts_type`, and audit fields with `invalid_argument` and field reason `IMMUTABLE_FIELD`. The provider `gts_type` is accepted on `PATCH /providers/{id}` as a type migration.
- Optimistic concurrency: responses that return one entry (`POST`, `GET /{id}`, `PATCH`) carry the strong `ETag` `"<version>"`. `PATCH` requires `If-Match`: a validator maps to `expected_version`, `*` to an unconditional update, and a missing header is rejected as `invalid_argument` with a field violation on `If-Match`, reason `IF_MATCH_REQUIRED`. `DELETE` accepts an optional `If-Match`. A stale validator is `aborted` (409) with reason `OPTIMISTIC_LOCK_FAILURE` (§4 Error Handling; reason for 409 instead of 412 in [ADR-0004](./ADR/0004-cpt-cf-model-registry-adr-update-semantics.md)). `PATCH` takes `Content-Type: application/json` with top-level whole-field replacement semantics and is not advertised as `application/merge-patch+json`.
- Errors are canonical Problem documents (RFC 9457) produced by `toolkit-canonical-errors`. The wire carries category, title, detail, resource type and name, and the category context (reason, field violations). Internal diagnostics, SQL text, stored JSON documents, and the ids of rows the caller cannot see are never put on the wire. The error table is in §4 Error Handling.
- Versioning follows the [platform versioning guideline](../../../guidelines/DNA/REST/VERSIONING.md). Breaking REST changes require `/v2/`. A breaking SDK change adds `ModelRegistryClientV2` next to `ModelRegistryClientV1`, and V1 is deprecated per that guideline. The first SDK release that a gear implements publishes `ModelRegistryClientV1` as defined in this document, and the V2 rule applies from then on. Additions of optional fields, endpoints, filter fields, and `supported_api` values are non-breaking. SDK structs with public fields are `#[non_exhaustive]`: they are built through `new`, `Default`, or the builder and then by field assignment, so an added field breaks no caller. Record fields with a documented default deserialize to that default when absent.

#### Provider Base Type

- [ ] `p1` - **ID**: `cpt-cf-model-registry-interface-model-info-schema`

- **Contracts**: `cpt-cf-model-registry-contract-provider-settings-schema`
- **Technology**: GTS type schema in Types Registry, declared in the SDK crate
- **Location**: [model-registry-sdk/src/models/](../model-registry-sdk/src/models/)

The gear owns the provider base type `gts.cf.genai.model.provider.v1~` (the AI model provider type of the [platform GTS guideline](../../../guidelines/GTS.md)). Its schema declares the `gts_type` discriminator and one open object property, `provider_settings`. Each LLM Gateway provider plugin publishes a derived provider type, for example `gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~`, whose schema narrows `provider_settings` to the plugin's settings. Plugins follow the compatibility rule of `cpt-cf-model-registry-contract-provider-settings-schema`: schema changes stay backward compatible with stored settings, or ship as a new type version. The base type changes only additively within `v1`. A provider moves to a new provider type version in place: an `update_provider` that carries the new `gts_type` validates every model of the provider against the new type's schema and, only when all pass, switches the `gts_type` of the provider and of all its models in one transaction (§3.6 `cpt-cf-model-registry-seq-provider-write`). Ids, slugs, and canonical ids are kept. A migration fails if the settings of any model fail the new schema.

The gear declares the provider base type `gts.cf.genai.model.provider.v1~` and the permission instances of §4 Authorization; the platform's GTS publication registers them in Types Registry before the gear starts. Provider plugins publish their derived types after the base type exists; until then, provider and model writes that need a provider type fail with `GTS_TYPE_NOT_REGISTERED`.

#### Consumer Contract

What LLM Gateway and its provider plugins rely on:

1. **SDK surface.** `ModelInfoV1` is not generic; `provider_settings` is JSON that the plugin narrows. The SDK defines no provider settings types; they live in the LLM Gateway provider plugins that own the derived provider types. Models have no lifecycle status (`cpt-cf-model-registry-constraint-no-model-lifecycle`), and provider availability is the `enabled` flag. The resolution methods are `resolve_model`, which returns the full `ModelV1` on the SDK and on REST alike, and `list_available_models`, which returns a page of the same full `ModelV1`; the management list is `list_models`, which returns `ModelEntryV1`. `llm-gateway-sdk` depends on the SDK crate; the `provider_type` of its plugin specification carries provider type ids derived from `gts.cf.genai.model.provider.v1~`.
2. **Errors.** `resolve_model` reports distinct outcomes: model not found, `PROVIDER_DISABLED`, `MODEL_NOT_APPROVED`, `NOT_PERMITTED`, and dependency unavailable (§4 Error Handling). LLM Gateway decides its own external mapping of each.
3. **Destination.** The model is called at `base_url` when set, otherwise at the plugin's default URL for the provider. The plugin sends every call through OAGW to the upstream alias named by the URL's host and port; the upstream's configuration sets the scheme, and OAGW admits only HTTPS upstreams. OAGW egress policy admits only upstreams enabled for the caller's tenant and otherwise fails with `unapproved_destination` (§4 Security Assumptions).
4. **Credentials.** `secret_ref` names a credential that the model's owning tenant, `ModelV1.tenant_id`, held under that name when the model was written; the gear enforces this on write and not at evaluation (§4 Credential Binding). LLM Gateway loads the secret under the caller's context with `CredStoreClientV2::get_secret` and performs no owner check: CredStore returns no owner, and for an inherited model the caller legitimately loads a secret it does not own. A missing, suppressed, or expired secret is `credentials_unavailable` without a provider call. For descendants of the model's tenant to evaluate a shared model, the secret must be `shared` in CredStore; the registry does not check this.
5. **Permissions.** The caller's `SecurityContext` must be granted model `evaluate` for both `resolve_model` and `list_available_models`; there is no separate list permission.
6. **Plugin obligations.** Each plugin publishes a provider type derived from `gts.cf.genai.model.provider.v1~` whose schema constrains `provider_settings`, after the base type exists. It keeps the schema backward compatible with stored settings or publishes a new type version.
7. **Cost.** The plugin computes the AI credits of a call from `info.cost`: micro-credits per 1M input, output, cache read, and cache write tokens, and per 1K web search calls. Provider type schemas carry no cost rates.

### 3.4 Internal Dependencies

| Dependency Gear | Interface Used | Purpose |
|-------------------|----------------|----------|
| `tenant-resolver` | `TenantResolverClient::get_ancestors` via ClientHub, once with the default `BarrierMode::Respect` per cache miss | Ancestor chain for inheritance and shadowing |
| `authz-resolver` | `AuthZResolverClient` wrapped in `PolicyEnforcer` (`authz-resolver-sdk`) | PDP decisions for every operation |
| `types-registry` | `TypesRegistryClient::get_type_schema` (the effective schema, parent chain inlined) under the caller's context; platform GTS publication (provider base type and permission instances) | Provider type checks and settings validation on writes |
| `credstore` | `CredStoreClientV2::get_record` via ClientHub, under the caller's context | Ownership check of `secret_ref` on model create and on an update that changes it to a non-null value (§4 Credential Binding) |
| `llm-gateway` (consumer) | Calls `ModelRegistryClientV1` | Resolves models on every inference request; its provider plugins publish provider types derived from `gts.cf.genai.model.provider.v1~` (§3.3 Consumer Contract) |

The gear declares `deps = [tenant_resolver, authz_resolver, types_registry, credstore]` and `capabilities = [db, rest]`. CredStore is called on writes only, never on the resolution path.

**Dependency Rules** (per [platform dependency rules](../../../docs/GEARS.md#dependency-rules)):
- No circular dependencies
- Always use SDK modules for inter-gear communication
- No cross-category sideways deps except through contracts
- Only integration/adapter gears talk to external systems
- `SecurityContext` must be propagated across all in-process calls

### 3.5 External Dependencies

#### Tenant Resolver Ancestor Chain

- **Contract**: `cpt-cf-model-registry-contract-tenant-hierarchy`

| Dependency Gear | Interface Used | Purpose |
|-------------------|---------------|---------|
| `tenant-resolver` | `get_ancestors(ctx, tenant, GetAncestorsOptions::default())` (`BarrierMode::Respect`), returning the tenant's ancestors from parent up to the nearest isolation barrier | The chain `[T0, parent, …, root or nearest self-managed ancestor]` drives `cpt-cf-model-registry-seq-effective-catalog` |

The gear uses the parent-first, root-last order that Tenant Resolver returns, as the PRD contract states. Under `BarrierMode::Respect`, a self-managed tenant has an empty chain and sees its own rows only. A tenant below a self-managed tenant has a chain that ends at that tenant: it inherits the self-managed tenant's shared providers and models and nothing from above it ([ADR-0001](./ADR/0001-cpt-cf-model-registry-adr-inherited-read-scope.md)). The gear never calls Tenant Resolver with `BarrierMode::Ignore`. Ancestors never read the self-managed subtree's rows; the gear never reads descendant rows. Tenant Resolver errors map as listed in §4 Error Handling. Hierarchy changes take effect within the chain cache TTL (`cpt-cf-model-registry-component-chain-cache` rule 5).

#### Provider Settings Schemas

- **Contract**: `cpt-cf-model-registry-contract-provider-settings-schema`

| Dependency Gear | Interface Used | Purpose |
|-------------------|---------------|---------|
| `types-registry` (schemas registered by LLM Gateway provider plugins) | `TypesRegistryClient::get_type_schema`, taking the effective schema (the type's schema with its parent chain inlined) | Source of the JSON Schema for `provider_settings` of each provider type |

Schema reads go through the Types Registry client registered in ClientHub and take the effective schema, the type's schema with its parent chain inlined. The read runs under the caller's context, which needs the Types Registry read grant.

#### Credential Ownership

- **Contract**: none; the CredStore SDK read is used as published

| Dependency Gear | Interface Used | Purpose |
|-------------------|---------------|---------|
| `credstore` | `CredStoreClientV2::get_record(ctx, secret_ref)` under the caller's context, bounded by `credstore_deadline_ms` | Confirms that the writing tenant's own row under `secret_ref` holds a secret: `own` is present with `status` `active` or `expired` |

The read is metadata only and returns no secret value (`cpt-cf-model-registry-constraint-no-secret-storage`); it requires the CredStore `read` action, not `read_secret`. `Ok(None)` is `SECRET_REF_NOT_FOUND`; no own row, or an own row with `status` `declared`, is `SECRET_REF_NOT_OWNED`; `PermissionDenied` is `NOT_PERMITTED`; other failures are dependency unavailable (§4 Error Handling, [ADR-0002](./ADR/0002-cpt-cf-model-registry-adr-credential-binding.md)).

#### Platform Database

- **Contract**: none; the gear owns its schema (§3.7 `cpt-cf-model-registry-db-catalog-storage`)

| Dependency Gear | Interface Used | Purpose |
|-------------------|---------------|---------|
| Platform database (PostgreSQL in production, SQLite in dev/test) | SeaORM through `toolkit-db` `SecureConn` / `DBRunner`; migrations through the gear's database capability | Durable storage of providers and models |

The dependency rules of §3.4 apply.

### 3.6 Interactions & Sequences

#### Effective Catalog Resolution

**ID**: `cpt-cf-model-registry-seq-effective-catalog`

**Use cases**: `cpt-cf-model-registry-usecase-share-and-shadow-provider`, `cpt-cf-model-registry-usecase-resolve-model`, `cpt-cf-model-registry-usecase-list-available-models`

**Actors**: `cpt-cf-model-registry-actor-tenant-admin`, `cpt-cf-model-registry-actor-llm-gateway`, `cpt-cf-model-registry-actor-tenant-resolver`

```mermaid
graph TD
    R["Root"] --> P["Partner"]
    R --> O["Other partner"]
    P --> C["Customer"]
    C --> T["Team"]
    R -.owns.-> RP["openai, shared, enabled<br/>gpt-4o shared approved<br/>gpt-4o-mini shared approved<br/>o1-preview tenant mode"]
    P -.owns.-> PP["openai, tenant mode, enabled<br/>gpt-4o"]
    C -.owns.-> CP["openai, shared, enabled<br/>gpt-4o shared approved"]
```

**Description**: Builds the effective catalog of the acting tenant T0. All read paths use these steps. The row reads of steps 2 and 5, and every other row read of the same request, run in one read-only snapshot transaction (`cpt-cf-model-registry-principle-snapshot-reads`), so the candidate providers and the model rows come from one committed state.

1. Get the chain `C = [T0, a1, …, ak]`, nearest first, from the chain cache. Distance of `ai` is `i`; distance of T0 is 0.
2. Load candidate providers in one query: providers with `tenant_id = T0` in any sharing mode, plus providers with `tenant_id IN (a1 … ak)` and `sharing_mode = shared`. A failed query fails the request.
3. For each slug, the winner is the candidate with the smallest distance. The `(tenant_id, slug)` key guarantees one candidate per tenant, so there are no ties. The `enabled` flag is not used here.
4. The allow-list is the set of winner ids whose provider is enabled.
5. A model is visible to T0 when `provider_id` is a winner id and either `tenant_id = T0` or `sharing_mode = shared`.
6. A visible model is available for evaluation when its provider is in the allow-list, its approval state is `approved`, and the PDP `evaluate` decision admits it.
7. A candidate provider that is not the winner of its slug is shadowed for T0, together with all its models.

Rules that follow from these steps:

- A tenant's own provider always wins its slug for that tenant, in any sharing mode.
- A `shared` shadowing provider also wins for every descendant that does not own a provider with the same slug.
- A `tenant`-mode shadowing provider is not a candidate for descendants, so they keep inheriting the ancestor's shared provider.
- A `shared`, disabled shadowing provider still wins its slug but is not in the allow-list, so no model with that slug resolves anywhere in its subtree.
- Deleting a shadowing provider removes it from the candidates; the ancestor's provider wins again on the next request.
- A read sees every write committed before its snapshot and none committed after it, so it never combines a provider state and a model state that were never committed together.
- Filtering is by `provider_id` membership, never by `canonical_id` dedupe. In the example below, Root's `openai::gpt-4o-mini` stays invisible to Customer although Customer owns no model with that name.

Result of the example hierarchy in the diagram:

| Acting tenant | Winner for `openai` | Visible `openai` models | Reason |
|---------------|---------------------|-------------------------|--------|
| Root | Root's provider | `gpt-4o`, `gpt-4o-mini`, `o1-preview` | Own provider; own models in any mode |
| Partner | Partner's provider | Partner's `gpt-4o` | Own provider wins even in `tenant` mode; Root's models are shadowed |
| Other partner | Root's provider | Root's `gpt-4o`, `gpt-4o-mini` | `o1-preview` is in `tenant` mode |
| Customer | Customer's provider | Customer's `gpt-4o` | Partner's `tenant`-mode provider is not a candidate; Customer's own provider is nearer than Root's |
| Team | Customer's provider | Customer's `gpt-4o` | Customer's `shared` provider is nearer than Root's |
| Customer or Team, with Customer's provider disabled | Customer's provider | none | `openai::gpt-4o` fails with `PROVIDER_DISABLED`; `openai::gpt-4o-mini` fails with model not found |

#### Resolve Model

**ID**: `cpt-cf-model-registry-seq-resolve-model`

**Use cases**: `cpt-cf-model-registry-usecase-resolve-model`

**Actors**: `cpt-cf-model-registry-actor-llm-gateway`, `cpt-cf-model-registry-actor-tenant-resolver`, `cpt-cf-model-registry-actor-authz-pdp`

```mermaid
sequenceDiagram
    participant GW as LLM Gateway
    participant MR as Model Registry
    participant CC as Chain cache
    participant TR as Tenant Resolver
    participant DB as Database
    participant PDP as AuthZ Resolver PDP

    GW->>MR: resolve_model(ctx, "openai::gpt-4o")
    MR->>MR: split on first "::" into slug and name
    MR->>CC: chain(T0)
    alt miss or expired
        CC->>TR: get_ancestors(T0, Respect)
        TR-->>CC: ancestors or error
        Note over CC: error gives dependency unavailable
    end
    CC-->>MR: [T0, a1, ..., ak]
    MR->>DB: begin read-only snapshot transaction
    MR->>DB: candidate providers with slug in chain
    DB-->>MR: up to one row per chain tenant
    Note over MR: winner = nearest candidate, none gives model not found
    MR->>DB: model by provider_id = winner and name
    DB-->>MR: row or none
    MR->>DB: end transaction
    Note over MR: none, or tenant mode and not owned by T0, gives model not found
    Note over MR: winner disabled gives PROVIDER_DISABLED
    Note over MR: approval not approved gives MODEL_NOT_APPROVED
    MR->>PDP: evaluate on model id, owner_tenant_id = T0, model properties
    PDP-->>MR: decision and constraints, or failure
    Note over MR: denied gives NOT_PERMITTED, failure gives dependency unavailable
    MR-->>GW: ModelV1
```

**Description**: Resolves a canonical id for the acting tenant T0 in the PRD order.

1. Parse the canonical id on the first `::` into the provider slug and the model `name`. A missing separator or an empty part yields model not found.
2. Get the chain (cache first). A Tenant Resolver failure yields dependency unavailable.
3. Open one read-only snapshot transaction and load candidate providers with the parsed slug (step 2 of the effective-catalog algorithm, restricted to one slug). No candidate yields model not found.
4. Load the model by `(provider_id = winner.id, name)` in the same transaction, then end the transaction. If no row exists, or the row belongs to an ancestor and is in `tenant` mode, the result is model not found.
5. If the winner is disabled, return `PROVIDER_DISABLED`. Provider state is reported only for a canonical id that names a visible model.
6. If the approval state is not `approved`, return `MODEL_NOT_APPROVED`.
7. Ask the PDP for `evaluate` on the model resource with the model id, `owner_tenant_id = T0`, and the properties `provider_id`, `canonical_id`, `gts_type`. Apply any returned row constraints to the found row in memory. Denial yields `NOT_PERMITTED`; a PDP failure yields dependency unavailable.
8. Return the full `ModelV1`, including `provider_settings`, `base_url` and `secret_ref` when set, defaults, and capabilities. An absent `base_url` stays absent; the gear does not fill in a default.

The rows are read from the database on every call, from one snapshot (`cpt-cf-model-registry-principle-snapshot-reads`): the winner's `enabled` and `sharing_mode` and the model's approval state come from one committed state, and the transaction ends before the PDP call of step 7. A revocation, disable, sharing change, or delete committed after the snapshot applies to the next resolution.

#### List Available Models

**ID**: `cpt-cf-model-registry-seq-list-available-models`

**Use cases**: `cpt-cf-model-registry-usecase-list-available-models`

**Actors**: `cpt-cf-model-registry-actor-consumer-gear`, `cpt-cf-model-registry-actor-llm-gateway`, `cpt-cf-model-registry-actor-authz-pdp`

```mermaid
sequenceDiagram
    participant CG as Consumer gear
    participant MR as Model Registry
    participant CC as Chain cache
    participant DB as Database
    participant PDP as AuthZ Resolver PDP

    CG->>MR: list_available_models(ctx, filter vision eq true, limit 20)
    MR->>CC: chain(T0)
    CC-->>MR: [T0, a1, ..., ak]
    MR->>PDP: evaluate on model type, owner_tenant_id = T0, no resource id
    PDP-->>MR: decision and constraints
    Note over MR: denied gives NOT_PERMITTED, failure gives dependency unavailable; tenant constraints gate T0, other constraints become a row predicate
    MR->>DB: begin read-only snapshot transaction
    MR->>DB: all candidate providers in chain
    DB-->>MR: provider rows
    Note over MR: winners per slug, allow-list = enabled winners
    alt allow-list empty
        MR->>DB: end transaction
        MR-->>CG: empty page
    else
        MR->>DB: models where provider_id in allow-list and own-or-shared and approved and evaluate predicate and filter, ordered, keyset page
        DB-->>MR: rows
        MR->>DB: end transaction
        MR-->>CG: Page of ModelV1
    end
```

**Description**: Returns exactly the models that `resolve_model` would return for the same caller.

1. Get the chain and take one PDP `evaluate` decision on the model resource type, with `owner_tenant_id = T0` and no resource id. Its tenant constraints gate T0 and its row constraints become the row predicate. A denial fails the request with `NOT_PERMITTED`.
2. A PDP failure fails the request with dependency unavailable; nothing is listed by default.
3. Open one read-only snapshot transaction and build the candidates, winners, and allow-list as in `cpt-cf-model-registry-seq-effective-catalog`.
4. Run one model query in the same transaction with these mandatory predicates: `provider_id IN allow-list`, `(tenant_id = T0 OR sharing_mode = shared)`, `approval_state = approved`, and the row predicate from the `evaluate` constraints on `id`, `provider_id`, `canonical_id`, and `gts_type`. The caller's `$filter`, `$orderby`, cursor, and limit are applied on top. The allow-list holds at most one id per slug among the chain's candidates; PDP value lists are capped by `max_pdp_constraint_values` (§4 Authorization rule 3).
5. If the allow-list is empty, return an empty page without querying models. The transaction ends after the last query, before the response is built.

One `evaluate` decision decides which rows appear, and the resolve path applies the same PDP property set and constraint rules to a single row, so a consistent PDP gives the same membership on both paths. The allow-list and the page rows come from one snapshot, so a model is listed only when its provider was enabled and its approval was `approved` in the same committed state. The cursor's filter hash covers the caller's `$filter` only, and each page is a separate request with its own snapshot; a provider disable or approval change between pages changes later pages.

#### Provider Write Path

**ID**: `cpt-cf-model-registry-seq-provider-write`

**Use cases**: `cpt-cf-model-registry-usecase-share-and-shadow-provider`

**Actors**: `cpt-cf-model-registry-actor-tenant-admin`, `cpt-cf-model-registry-actor-authz-pdp`, `cpt-cf-model-registry-actor-types-registry`

```mermaid
sequenceDiagram
    participant A as Tenant administrator
    participant MR as Model Registry
    participant PDP as AuthZ Resolver PDP
    participant TY as Types Registry
    participant DB as Database

    A->>MR: create_provider(slug, name, gts_type, enabled, sharing_mode)
    MR->>MR: validate slug and name format
    MR->>PDP: create on provider type, owner_tenant_id = T0
    PDP-->>MR: decision
    MR->>MR: parse gts_type, check it derives from the provider base type
    MR->>TY: get_type_schema(gts_type)
    TY-->>MR: schema, not found, or failure
    Note over MR: not found gives invalid_argument, failure gives dependency unavailable
    MR->>DB: insert provider with tenant_id = T0 in transaction
    DB-->>MR: ok, or unique violation on tenant and slug
    MR-->>A: 201 ProviderEntryV1, or 409 already_exists
```

**Description**: Provider create, update, and delete.

- **Create**: format checks, PDP `create`, the GTS checks of §4 GTS Checks, a check of any PDP row constraints in memory against the built record, then insert with `tenant_id = T0`, `created_by` and `updated_by` set to the subject. A duplicate slug in the tenant yields `already_exists`. A new provider with an inherited slug immediately shadows the inherited provider as described in `cpt-cf-model-registry-seq-effective-catalog`.
- **Update and type migration**: an update whose request carries a `gts_type` different from the stored one is a type migration; an equal `gts_type` is a plain update. Guarantees:
  - PDP `update` with the provider id precedes any write.
  - Every write runs in one transaction scoped to `tenant_id = T0`. No database connection is held during the Types Registry call.
  - The transaction of an update, a type migration, and a delete locks the provider row exclusively (a PostgreSQL row lock; SQLite serializes writers); a type migration also locks the provider's model rows. Concurrent writes of the same provider are serialized by the lock.
  - `expected_version`, when given, is compared against the locked row and must equal its `version`; otherwise the request fails with `OPTIMISTIC_LOCK_FAILURE` and nothing is written. Every successful write increments `version`.
  - A request `gts_type` equal to the value read before the transaction is compared again with the locked row; a difference fails with `aborted` and reason `OPTIMISTIC_LOCK_FAILURE` and writes nothing, including for an unconditional update.
  - Fields present in the request are applied to the locked row, including those equal to the stored value. Changes to `enabled` or `sharing_mode` apply to the next request.
  - A type migration runs the provider GTS checks of §4 GTS Checks on the new type and validates every model's `provider_settings` against the `provider_settings` subschemas of the new type (§4 GTS Checks). Each violation is one field violation with field `gts_type`, reason `SCHEMA_MISMATCH`, and a description naming the model's canonical id and the JSON pointer; one failing model fails the request and nothing is written.
  - A type migration is atomic across the provider and all its models: it fails with `OPTIMISTIC_LOCK_FAILURE` if the locked provider row or model rows differ from the validated state (the provider version, the model set, or any model version changed), and otherwise sets `gts_type` on the provider and every model, increments every row's `version`, and sets `updated_at` and `updated_by` on every row. Readers see the old or the new type atomically; the next resolution selects the plugin of the new type. The provider row and its model rows are updated in place and never deleted and recreated, so the provider keeps its id and stays the candidate for its slug throughout; an inherited provider it shadows is never exposed.
  - Validation cost grows with the provider's model count and is bounded by `max_json_document_bytes` per model and by the Types Registry deadline for the single schema read.
  - A write on an inherited provider fails with `INHERITED_READ_ONLY`; an invisible provider is not found.
- **Delete**: PDP `delete` with the provider id precedes the delete. One transaction scoped to `tenant_id = T0` locks the provider row, checks `expected_version` against it when given, and deletes the provider's models and the provider (FK `ON DELETE CASCADE` backstop). If the provider shadowed an inherited one, the inherited provider wins its slug again on the next request.

#### Model Write Path

**ID**: `cpt-cf-model-registry-seq-model-write`

**Use cases**: `cpt-cf-model-registry-usecase-approve-model`, `cpt-cf-model-registry-usecase-share-and-shadow-provider`

**Actors**: `cpt-cf-model-registry-actor-tenant-admin`, `cpt-cf-model-registry-actor-authz-pdp`, `cpt-cf-model-registry-actor-types-registry`

```mermaid
sequenceDiagram
    participant A as Tenant administrator
    participant MR as Model Registry
    participant PDP as AuthZ Resolver PDP
    participant DB as Database
    participant TY as Types Registry
    participant CR as CredStore

    A->>MR: create_model(provider_id, name, info, sharing_mode, approval_state)
    MR->>MR: validate name, field formats and sizes
    MR->>PDP: create on model type, owner_tenant_id = T0, provider_id
    PDP-->>MR: decision
    MR->>DB: provider by id, tenant_id = T0
    DB-->>MR: provider or none
    Note over MR: inherited provider gives INHERITED_READ_ONLY, unknown gives not found
    MR->>TY: get_type_schema(provider.gts_type, resolved)
    TY-->>MR: resolved schema or failure
    MR->>MR: validate provider_settings against the provider_settings part of the schema
    Note over MR: violations give invalid_argument with one field violation each
    opt secret_ref set
        MR->>CR: get_record(ctx, secret_ref)
        CR-->>MR: own row status, none, denied, or failure
        Note over MR: no own row holding a secret gives invalid_argument
    end
    MR->>MR: canonical_id = slug::name, gts_type from provider, approval and audit fields
    MR->>MR: check PDP row constraints against the built record
    MR->>DB: insert model with promoted columns in transaction
    DB-->>MR: ok, unique violation, or foreign-key violation
    Note over MR: foreign-key violation gives provider not found
    MR-->>A: 201 ModelEntryV1
```

**Description**: Model create and update, including approval changes.

- **Create**: guarantees:
  - Formats and sizes are validated first, including the `name` format (`base_url` only when present).
  - PDP `create` on the model type with `owner_tenant_id = T0` and `provider_id` precedes any write. Row constraints of the decision are checked in memory against the built record.
  - `provider_settings` is validated (§4 GTS Checks) and `secret_ref`, when set, is ownership-checked (§4 Credential Binding). No database connection is held during the Types Registry or CredStore call.
  - The record gets `canonical_id = slug::name`, the provider's `gts_type`, `version = 1`, and approval (default `approved`, decided by the subject now). All columns are written in one transaction.
  - The insert transaction re-reads the provider's `gts_type` under a shared lock on the provider row; a create validated against a provider type that changed since fails with `aborted` and reason `PROVIDER_TYPE_CHANGED` (the caller retries).
  - A duplicate `name` under the provider, and so a duplicate `canonical_id`, yields `already_exists`. A provider deleted in between yields provider not found.
- **Update**: guarantees:
  - PDP `update` with the model id precedes any write. A write on an inherited model fails with `INHERITED_READ_ONLY`; an invisible model is not found.
  - `base_url` is validated when the request sets it (null clears it), `provider_settings` when the request carries a value different from the stored one, and `secret_ref` ownership when the request carries a non-null value different from the stored one (null clears it; §4 Credential Binding). A present field equal to the stored value triggers neither check and is written.
  - The write runs in one transaction scoped to `tenant_id = T0` that locks the model row exclusively (a PostgreSQL row lock; SQLite serializes writers). Under the lock, in this order:
    - `expected_version`, when given, is compared with the locked `version`; a mismatch fails with `OPTIMISTIC_LOCK_FAILURE` and writes nothing.
    - Each field whose check was skipped because the request value equalled the stored value read before the transaction (`provider_settings`, `secret_ref`) is compared again with the locked row; a difference fails with `OPTIMISTIC_LOCK_FAILURE` and writes nothing, including for an unconditional update.
    - When `provider_settings` was validated, the locked row's `gts_type` is compared with the value read before validation; a difference fails with `aborted` and reason `PROVIDER_TYPE_CHANGED`.
  - `OPTIMISTIC_LOCK_FAILURE` takes precedence over `PROVIDER_TYPE_CHANGED`: the version compare is evaluated first, so a conditional update that raced a type migration reports `OPTIMISTIC_LOCK_FAILURE`.
  - Two updates with the same `expected_version` apply in commit order and the second fails. Unconditional updates are serialized by the lock.
  - The request's present fields are applied to the locked row and `version` is incremented, so concurrent changes to other fields are kept. Unconditional updates (no `expected_version`) change only the fields they carry.
  - When the request carries `approval_state`, `approval_decided_by` is set to the subject and `approval_decided_at` to now.
  - Every promoted column is rewritten from the merged record (the locked row with the present fields applied).
- **Approve**: an update with `approval_state = approved` (PRD use case approve-model). Later resolutions of the model succeed when its provider is enabled and the PDP permits evaluation. Revoke is the same update with `revoked`; later resolutions report `MODEL_NOT_APPROVED`.
- **Delete**: PDP `delete` with the model id precedes a hard delete scoped to `tenant_id = T0`, conditional on `expected_version` when given.

There is no bulk create or update in this design. Each model write fetches and compiles its provider type schema once and makes at most one CredStore call.

#### Management Reads

**ID**: `cpt-cf-model-registry-seq-management-read`

**Use cases**: `cpt-cf-model-registry-usecase-share-and-shadow-provider`

**Actors**: `cpt-cf-model-registry-actor-tenant-admin`, `cpt-cf-model-registry-actor-tenant-resolver`, `cpt-cf-model-registry-actor-authz-pdp`

```mermaid
sequenceDiagram
    participant A as Tenant administrator
    participant MR as Model Registry
    participant PDP as AuthZ Resolver PDP
    participant CC as Chain cache
    participant DB as Database

    A->>MR: list_models(ctx, odata)
    MR->>PDP: list on model type, owner_tenant_id = T0
    PDP-->>MR: decision and constraints
    MR->>CC: chain(T0)
    CC-->>MR: [T0, a1, ..., ak]
    MR->>DB: begin read-only snapshot transaction
    MR->>DB: candidate providers in chain
    DB-->>MR: provider rows
    Note over MR: winners, shadowed set, inherited shared provider ids
    MR->>DB: models where tenant_id = T0, or provider in inherited shared candidates and model shared, plus PDP predicate and filter
    DB-->>MR: rows
    MR->>DB: end transaction
    Note over MR: flags inherited, shadowed, provider_enabled, available_for_eval
    MR-->>A: Page of ModelEntryV1
```

**Description**: `list_models` is the admin list, authorized by the model `list` action and separate from the available list (`list_available_models`). It returns every model the tenant manages or inherits, including models of disabled providers and models that are not approved. `list_providers`, `get_provider`, `list_models`, and `get_model` return own entries in any state and inherited entries that are candidates for T0, including shadowed ones. An inherited provider is a candidate when it is `shared`. An inherited model is a candidate when it is `shared` and its provider is a `shared` candidate for T0. Each entry carries `inherited` and `shadowed` flags and its eval availability. Point reads take the PDP `get` decision first, then load the row by id and check that it is own or an inherited candidate; anything else is not found. The candidate provider query and the row query of a management read run in one read-only snapshot transaction (`cpt-cf-model-registry-principle-snapshot-reads`), so the `shadowed`, `provider_enabled`, and `available_for_eval` flags of an entry are computed from the same committed state as the entry. Inherited entries contain the full record. Management reads never return rows of descendant or sibling tenants.

### 3.7 Database schemas & tables

- [ ] `p1` - **ID**: `cpt-cf-model-registry-db-catalog-storage`

Two tables in the platform database under the gear's stable `db_namespace = model_registry` ([database object namespacing ADR](../../../docs/arch/database/ADR/0001-cpt-cf-database-adr-object-namespacing.md)): `model_registry__providers` and `model_registry__models`, created by the gear's migrations and recorded in the gear's own migration history table. Explicitly named indexes and constraints follow the ADR grammar `idx_`, `uq_`, `fk_`, `ck_` + `<table>__<purpose>`. Logical types map per backend: `UUID` (PostgreSQL `UUID`, SQLite `TEXT`), `TIMESTAMP` (PostgreSQL `TIMESTAMPTZ`, SQLite `TEXT` in ISO-8601 UTC), `JSON` (PostgreSQL `JSONB`, SQLite `TEXT`), `BOOLEAN`, `INTEGER` (32-bit), `BIGINT` (64-bit; used for unsigned 32-bit values so they fit on PostgreSQL), `VARCHAR(n)`, `TEXT`. All indexes are B-tree. JSON columns have no indexes and are never filtered.

#### Table: model_registry__providers

**ID**: `cpt-cf-model-registry-dbtable-providers`

**Schema**:

| Column | Type | Description |
|--------|------|-------------|
| id | UUID | Provider id |
| tenant_id | UUID | Owning tenant; `Scopable` tenant column |
| slug | VARCHAR(64) | Lowercase letters, digits, hyphen |
| name | VARCHAR(255) | Display name |
| gts_type | VARCHAR(512) | Provider GTS type id |
| enabled | BOOLEAN | Provider enabled flag |
| sharing_mode | VARCHAR(16) | `tenant` or `shared` |
| managed | BOOLEAN | Platform-managed provider flag |
| metadata | JSON | Opaque metadata, nullable |
| version | BIGINT | Row version, 1 on create, incremented by every update |
| created_at | TIMESTAMP | Creation time |
| created_by | UUID | Creating subject |
| updated_at | TIMESTAMP | Last change time |
| updated_by | UUID | Last changing subject |

**PK**: `id`

**Constraints**: all columns NOT NULL except `metadata`; `uq_model_registry__providers__tenant_slug` UNIQUE `(tenant_id, slug)`; `uq_model_registry__providers__id_tenant` UNIQUE `(id, tenant_id)`, the target of the models foreign key; `ck_model_registry__providers__sharing_mode` CHECK on `sharing_mode` values.

**Additional info**: Indexes: the unique `(tenant_id, slug)` key serves the chain-wide candidate query and the slug lookup of resolution; `idx_model_registry__providers__slug_id` on `(slug, id)` serves the default keyset order; `idx_model_registry__providers__<column>` for every other filterable provider field of §3.3. Referenced by `model_registry__models (provider_id, tenant_id)`.

**Example**:

| id | tenant_id | slug | name | gts_type | enabled | sharing_mode | managed |
|----|-----------|------|------|----------|---------|--------------|---------|
| `0192…a1` | `root…` | `openai` | OpenAI | `gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~` | true | shared | false |
| `0192…b7` | `customer…` | `openai` | OpenAI (Customer contract) | `gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~` | true | shared | false |

#### Table: model_registry__models

**ID**: `cpt-cf-model-registry-dbtable-models`

**Schema**:

| Column | Type | Description |
|--------|------|-------------|
| id | UUID | Model id |
| tenant_id | UUID | Owning tenant; equals the provider's |
| provider_id | UUID | Provider; FK with `tenant_id` to `model_registry__providers(id, tenant_id)` |
| name | VARCHAR(128) | Model name; right part of the canonical id |
| canonical_id | VARCHAR(194) | `slug::name` |
| provider_model_id | VARCHAR(255) | Identifier sent to the provider; not unique |
| gts_type | VARCHAR(512) | Copied from the provider on create |
| sharing_mode | VARCHAR(16) | `tenant` or `shared` |
| approval_state | VARCHAR(16) | `pending`, `approved`, `rejected`, `revoked` |
| approval_decided_by | UUID | Subject of the latest decision |
| approval_decided_at | TIMESTAMP | Time of the latest decision |
| base_url | VARCHAR(2048) | General setting, nullable; NULL means the plugin's default URL |
| secret_ref | VARCHAR(255) | CredStore reference, nullable |
| display_name | VARCHAR(255) | Display name |
| description | TEXT | Nullable |
| family | VARCHAR(128) | Nullable |
| vendor | VARCHAR(128) | Nullable |
| managed | BOOLEAN | Platform-managed model flag |
| architecture | VARCHAR(64) | Nullable |
| size_bytes | BIGINT | Nullable; 0 to 2^63 − 1 |
| format | VARCHAR(64) | Nullable |
| region | VARCHAR(64) | Nullable |
| hosted_by | VARCHAR(128) | Nullable |
| last_release_at | TIMESTAMP | Nullable |
| reasoning_level | VARCHAR(64) | Nullable |
| model_version | VARCHAR(64) | Nullable |
| sort_order | INTEGER | Default 0 |
| icon | VARCHAR(2048) | Nullable |
| multiplier_display | VARCHAR(32) | Nullable |
| cost | JSON | `cost` object |
| performance | JSON | `performance` object |
| additional_info | JSON | `additional_info` map |
| supported_api | JSON | `supported_api` set (source of truth) |
| api_completion, api_embedding, api_batch | BOOLEAN | Promoted from `supported_api` |
| capabilities | JSON | `capabilities` object (source of truth) |
| disabled_capabilities | JSON | `disabled_capabilities` object (source of truth) |
| cap_vision, cap_reasoning_effort, cap_tools, cap_json_schema, cap_streaming, cap_file_input, cap_image_generation, cap_audio_input, cap_audio_output, cap_code_interpreter, cap_web_search | BOOLEAN | Promoted effective capability: declared and not disabled (`reasoning.effort` for `cap_reasoning_effort`; `tools.enabled` for `cap_tools`; `output.json_schema` for `cap_json_schema`; `enabled` for media and web search) |
| ctx_max_input_tokens | BIGINT | `context_window.max_input_tokens` |
| ctx_max_output_tokens | BIGINT | Nullable |
| ctx_output_vector_size | BIGINT | Nullable |
| default_parameters | JSON | `default_parameters` object |
| allow_parameter_override | BOOLEAN | Override policy |
| allow_extra_params | JSON | List of parameter names |
| provider_settings | JSON | Opaque provider-specific settings |
| version | BIGINT | Row version, 1 on create, incremented by every update |
| created_at | TIMESTAMP | Creation time |
| created_by | UUID | Creating subject |
| updated_at | TIMESTAMP | Last change time |
| updated_by | UUID | Last changing subject |

**PK**: `id`

**Constraints**: NOT NULL on every column not marked nullable; `uq_model_registry__models__tenant_canonical_id` UNIQUE `(tenant_id, canonical_id)`; `uq_model_registry__models__provider_name` UNIQUE `(provider_id, name)`; `fk_model_registry__models__provider` FK `(provider_id, tenant_id) → model_registry__providers(id, tenant_id)` ON DELETE CASCADE, which enforces the same-tenant rule; `ck_model_registry__models__sharing_mode` and `ck_model_registry__models__approval_state` CHECK on the enum values. SQLite connections run with foreign-key enforcement on, so the FK holds on both backends.

**Additional info**: Indexes: the unique `(provider_id, name)` key serves resolution and the `provider_id IN allow-list` predicate; the unique `(tenant_id, canonical_id)` key serves own-tenant scans; `idx_model_registry__models__canonical_id_id` on `(canonical_id, id)` serves the default keyset order; `idx_model_registry__models__<column>` for every other filterable model field of §3.3, on its mapped column, low-cardinality `cap_*`, `api_*`, `sharing_mode`, and `approval_state` included (`cpt-cf-model-registry-constraint-real-column-filters`). A migration test fails when a filter field has no index. Every create and update rewrites all columns from the merged record (§3.6 `cpt-cf-model-registry-seq-model-write`), so promoted columns never drift from their JSON sources. Eval visibility, `shadowed`, and provider enabled state are not stored on models; they are computed per request from `model_registry__providers`.

**Example**:

| id | tenant_id | provider_id | name | canonical_id | provider_model_id | region | gts_type | sharing_mode | approval_state | base_url | secret_ref |
|----|-----------|-------------|------|--------------|-------------------|--------|----------|--------------|----------------|----------|------------|
| `0192…c3` | `root…` | `0192…a1` | `gpt-4o` | `openai::gpt-4o` | `gpt-4o` | NULL | `gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~` | shared | approved | `https://api.openai.com/v1` | `platform-openai-key` |
| `0192…c4` | `root…` | `0192…a1` | `gpt-4o-eu` | `openai::gpt-4o-eu` | `gpt-4o` | `eu-west-1` | `gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~` | shared | approved | `https://eu.api.openai.com/v1` | `platform-openai-eu-key` |
| `0192…d9` | `customer…` | `0192…b7` | `gpt-4o` | `openai::gpt-4o` | `gpt-4o` | NULL | `gts.cf.genai.model.provider.v1~cf.genai._.openai.v1~` | shared | pending | NULL (plugin default URL) | `customer-openai-key` |

The two root rows are two models of one provider for the same provider model id, with their own region, URL, and credential.

### 3.8 Deployment Topology

- [ ] `p1` - **ID**: `cpt-cf-model-registry-topology-in-process-gear`

```mermaid
graph LR
    subgraph R1["Platform server replica 1"]
        G1["Model Registry gear, chain cache"]
        L1["LLM Gateway gear"]
    end
    subgraph R2["Platform server replica N"]
        G2["Model Registry gear, chain cache"]
        L2["LLM Gateway gear"]
    end
    APIGW["API Gateway"] --> G1
    APIGW --> G2
    L1 --> G1
    L2 --> G2
    G1 --> DB[("Platform database")]
    G2 --> DB
```

The gear runs in-process inside the platform server binary, enabled by a cargo feature and registered with the other gears. It runs no background tasks of its own and has no listener of its own; REST traffic arrives through API Gateway and SDK calls arrive through ClientHub. The gear declares its GTS types and the platform's GTS publication registers them in Types Registry before the gear starts. Each replica keeps its own chain cache. All replicas share the platform database. Replicas are stateless and can be added, removed, or restarted at any time. Packaging, orchestration, environment promotion, configuration injection, and database credentials are owned by the platform server deployment.

Database load: at most two point queries per resolution, so up to 10,000 queries per second at the 5,000 rps peak, on the primary (no read replica). Connections come from the platform pool and are never held across a PDP, Tenant Resolver, or Types Registry call.

Migrations are expand-contract: a release adds columns with defaults that rows written by the previous release keep valid, and backfills promoted columns from their JSON sources in the same migration. A later release drops columns that no running replica uses and backfills new promoted columns again, covering rows that previous-release replicas wrote during the rolling upgrade.

## 4. Additional context

### Authorization

PDP resource types (GTS-identified) and actions:

| Resource type | Supported properties | Actions |
|---------------|---------------------|---------|
| `gts.cf.genai.model_registry.provider.v1~` | `owner_tenant_id`, `id`, `gts_type` | `get`, `list`, `create`, `update`, `delete` (provider management) |
| `gts.cf.genai.model_registry.model.v1~` | `owner_tenant_id`, `id`, `provider_id`, `canonical_id`, `gts_type` | `get`, `list`, `create`, `update`, `delete` (model management, including approval; `list` is the admin list); `evaluate` (model evaluation: resolve one model and list the evaluable models) |

The resource types do not opt into native group predicates.

| Operation | Resource type | Action | Resource id | Properties sent |
|-----------|---------------|--------|-------------|-----------------|
| `resolve_model` | model | `evaluate` | model id | `owner_tenant_id = T0`, `provider_id`, `canonical_id`, `gts_type` |
| `list_available_models` | model | `evaluate` | none | `owner_tenant_id = T0` |
| `create_provider` | provider | `create` | none | `owner_tenant_id = T0` |
| `get_provider`, `update_provider`, `delete_provider` | provider | `get`, `update`, `delete` | provider id | `owner_tenant_id = T0` |
| `list_providers` | provider | `list` | none | `owner_tenant_id = T0` |
| `create_model` | model | `create` | none | `owner_tenant_id = T0`, `provider_id` |
| `get_model`, `update_model`, `delete_model` | model | `get`, `update`, `delete` | model id | `owner_tenant_id = T0` |
| `list_models` | model | `list` | none | `owner_tenant_id = T0` |

Decision rules:

1. Every call uses `PolicyEnforcer::access_scope_with` with `require_constraints(false)`, since the gear builds the row scope itself (rule 7). A permit with no constraints admits the operation for T0 with no row constraint. T0 is the subject tenant of the `SecurityContext`.
2. `owner_tenant_id` is always the acting tenant T0, also for inherited rows owned by an ancestor. The decision answers "may this subject perform the action for tenant T0"; the effective-catalog algorithm decides which ancestor rows T0 sees.
3. Tenant constraints returned by the PDP are a gate: each constraint alternative is kept only if its `owner_tenant_id` filter is `Eq` or `In` and admits T0. An alternative with no `owner_tenant_id` filter is kept, and its row constraints apply. An `InTenantSubtree` filter on `owner_tenant_id` fails closed regardless of `respect_barriers` or `descendant_status`: the alternative admits nothing. Unknown `ScopeFilter` variants and constraint kinds that cannot be evaluated in memory deny, and so does a filter with more than `max_pdp_constraint_values` values. If no alternative remains, the result is `NOT_PERMITTED` ([ADR-0001](./ADR/0001-cpt-cf-model-registry-adr-inherited-read-scope.md)).
4. The remaining filters on `id`, `provider_id`, `canonical_id`, and `gts_type` are row constraints. Lists apply them in SQL through `SecureConn`; point operations check them in memory against the row.
5. `EnforcerError::AccessDenied` and `CompileFailed` map to `NOT_PERMITTED`; `EvaluationFailed` maps to dependency unavailable. Access is never granted by default.
6. Writes are additionally clamped to `tenant_id = T0` in SQL, independent of the PDP answer: own-row writes run through `SecureConn` under `AccessScope::for_tenant(T0)`.
7. Reads run through `SecureConn` under a scope the gear builds from the validated ancestor chain: `AccessScope::for_tenants` over T0 and its ancestors (`tenant_id IN chain`), built in one storage function that every read path uses ([ADR-0001](./ADR/0001-cpt-cf-model-registry-adr-inherited-read-scope.md)). The mandatory sharing and winner predicates of `cpt-cf-model-registry-seq-effective-catalog` are ANDed onto it; on lists, so are the PDP row constraints of rule 4. The PDP tenant constraints gate T0 only (rule 3) and are never used as the row scope. A caller `$filter` is ANDed on top and never widens the scope.

This section is the canonical statement of the `evaluate` rules. Management operations call the PDP before any row lookup. `list_available_models` takes its one `evaluate` decision before the model query: a denial is `NOT_PERMITTED`, a failure is dependency unavailable, and a permit whose row constraints match no row is an empty page. The list is exactly the set of models that resolve. `resolve_model` calls the PDP last, per the PRD order: catalog reasons (not found, provider disabled, not approved) about the caller's own effective catalog are reported before the PDP is asked.

Permission catalog. The two resource types are declared as abstract GTS type schemas, and each action is declared as one instance of `gts.cf.toolkit.authz.permission.v1~`. All are published to Types Registry (§3.3 `cpt-cf-model-registry-interface-model-info-schema`), so administrators can discover and grant them.

| Permission instance | `resource_type` | `action` |
|---------------------|-----------------|----------|
| `gts.cf.toolkit.authz.permission.v1~cf.model_registry._.provider_get.v1` | `gts.cf.genai.model_registry.provider.v1~` | `get` |
| `gts.cf.toolkit.authz.permission.v1~cf.model_registry._.provider_list.v1` | `gts.cf.genai.model_registry.provider.v1~` | `list` |
| `gts.cf.toolkit.authz.permission.v1~cf.model_registry._.provider_create.v1` | `gts.cf.genai.model_registry.provider.v1~` | `create` |
| `gts.cf.toolkit.authz.permission.v1~cf.model_registry._.provider_update.v1` | `gts.cf.genai.model_registry.provider.v1~` | `update` |
| `gts.cf.toolkit.authz.permission.v1~cf.model_registry._.provider_delete.v1` | `gts.cf.genai.model_registry.provider.v1~` | `delete` |
| `gts.cf.toolkit.authz.permission.v1~cf.model_registry._.model_get.v1` | `gts.cf.genai.model_registry.model.v1~` | `get` |
| `gts.cf.toolkit.authz.permission.v1~cf.model_registry._.model_list.v1` | `gts.cf.genai.model_registry.model.v1~` | `list` |
| `gts.cf.toolkit.authz.permission.v1~cf.model_registry._.model_create.v1` | `gts.cf.genai.model_registry.model.v1~` | `create` |
| `gts.cf.toolkit.authz.permission.v1~cf.model_registry._.model_update.v1` | `gts.cf.genai.model_registry.model.v1~` | `update` |
| `gts.cf.toolkit.authz.permission.v1~cf.model_registry._.model_delete.v1` | `gts.cf.genai.model_registry.model.v1~` | `delete` |
| `gts.cf.toolkit.authz.permission.v1~cf.model_registry._.model_evaluate.v1` | `gts.cf.genai.model_registry.model.v1~` | `evaluate` |

### GTS Checks

On provider create, and on a provider update whose `gts_type` differs from the stored one (type migration):

1. `gts_type` parses as a GTS type id (schema id ending with `~`). Failure: `invalid_argument`, field `gts_type`, reason `INVALID_GTS_TYPE`.
2. The chain starts with the base type `gts.cf.genai.model.provider.v1~` and has at least one derived segment. Failure: reason `NOT_A_PROVIDER_TYPE`.
3. Types Registry returns an active type schema for the id (`TypesRegistryClient::get_type_schema` under the caller's context). Not found: reason `GTS_TYPE_NOT_REGISTERED`. Read denied: `NOT_PERMITTED`. Types Registry failure: dependency unavailable.

On model create, on model update when the request's `provider_settings` differs from the stored value, and for every model of the provider on type migration (against the new type):

1. Read the resolved schema of the provider's `gts_type` from Types Registry (read options in §3.5). Not found: `invalid_argument`, field `provider_settings`, reason `GTS_TYPE_NOT_REGISTERED`. Read denied: `NOT_PERMITTED`. Failure: dependency unavailable.
2. Take every `provider_settings` subschema of the resolved schema: `properties.provider_settings` at the root and inside each `allOf` branch, where a derived type inlines its parent chain. The root `definitions` and `$defs` travel with them. Validate the bare `provider_settings` document against all of them with the workspace JSON Schema validator as JSON Schema Draft-07, the GTS dialect ([GTS guideline](../../../guidelines/GTS.md)). Schema-only `x-gts-*` keywords do not affect validation. A provider type that does not constrain `provider_settings` accepts any JSON object.
3. Report every violation as one field violation: field `provider_settings` plus the JSON pointer of the offending value within the `provider_settings` document, reason `SCHEMA_MISMATCH`, and the validator message as description.

Schemas are fetched on these writes only. They are not cached and not fetched on resolution or listing. Stored settings are not re-validated when a plugin publishes a new schema; plugins keep schemas backward compatible or publish a new type version, to which administrators move providers by type migration.

### Error Handling

All errors are `CanonicalError` values at the SDK boundary and canonical Problem documents on REST, from one mapping in the gear. Reason codes and field reason codes are public constants in the SDK. The opt-in projection `ModelRegistryError` has the variants `ModelNotFound`, `ProviderNotFound`, `ProviderDisabled`, `ModelNotApproved`, `NotPermitted`, `InheritedReadOnly`, `AlreadyExists`, `OptimisticLockFailure` (the projection of both `aborted` reasons `OPTIMISTIC_LOCK_FAILURE` and `PROVIDER_TYPE_CHANGED`; the wire reasons stay distinct), `Invalid`, `DependencyUnavailable`, `Internal`, and `Other`, which wraps any other `CanonicalError`.

| Condition | Category | HTTP | Reason / context | Raised by |
|-----------|----------|------|------------------|-----------|
| Model not found (not in the effective catalog, malformed canonical id, unknown or invisible id) | `not_found` | 404 | resource type `gts.cf.genai.model_registry.model.v1~`, resource name = canonical id or id | resolution, model reads and writes |
| Provider not found | `not_found` | 404 | resource type `gts.cf.genai.model_registry.provider.v1~`, resource name = id | provider operations, `create_model` |
| Provider disabled | `permission_denied` | 403 | reason `PROVIDER_DISABLED` | `resolve_model` |
| Model not approved (pending, rejected, revoked) | `permission_denied` | 403 | reason `MODEL_NOT_APPROVED` | `resolve_model` |
| PDP denies, Types Registry denies the caller's schema read, or CredStore denies the caller's credential read | `permission_denied` | 403 | reason `NOT_PERMITTED` | all operations |
| Write against an inherited entry | `permission_denied` | 403 | reason `INHERITED_READ_ONLY` | updates, deletes, `create_model` under an inherited provider |
| Duplicate provider slug in the tenant, or duplicate model name under the provider | `already_exists` | 409 | resource type and name | creates |
| `expected_version` or `If-Match` does not match the stored `version`, or the provider or its model set changed during a type migration, or a resent `provider_settings`, `secret_ref`, or provider `gts_type` changed concurrently | `aborted` | 409 | reason `OPTIMISTIC_LOCK_FAILURE`; takes precedence over `PROVIDER_TYPE_CHANGED` | updates and deletes with a precondition; type migration; `update_model` |
| The provider's `gts_type` changed between validation and the model write | `aborted` | 409 | reason `PROVIDER_TYPE_CHANGED`; reported only when `OPTIMISTIC_LOCK_FAILURE` does not apply | `create_model`, `update_model` with `provider_settings` validated |
| `PATCH` without `If-Match` | `invalid_argument` | 400 | field violation on `If-Match` with reason `IF_MATCH_REQUIRED` | REST updates |
| Invalid request field, GTS type, settings, or secret reference | `invalid_argument` | 400 | field violations with reasons `INVALID_FORMAT`, `TOO_LARGE`, `IMMUTABLE_FIELD`, `INVALID_GTS_TYPE`, `NOT_A_PROVIDER_TYPE`, `GTS_TYPE_NOT_REGISTERED`, `SCHEMA_MISMATCH`, `SECRET_REF_NOT_FOUND`, `SECRET_REF_NOT_OWNED` | writes |
| Invalid OData option or cursor | `invalid_argument` | 400 | toolkit OData field violations | lists |
| Tenant Resolver, PDP, Types Registry, or CredStore cannot answer (unavailable, deadline expired, internal failure) | `service_unavailable` | 503 | no context; the category is the reason "dependency unavailable" | all operations that call the dependency |
| Database connectivity loss, pool acquire timeout, statement timeout, serialization failure, or deadlock | `service_unavailable` | 503 | no context; diagnostic kept in logs | all operations |
| Logic or invariant failure, unexpected database error | `internal` | 500 | diagnostic kept in logs, stripped from the wire | all operations |

Dependency errors map as follows:

| Dependency error | Result |
|------------------|--------|
| Tenant Resolver `TenantNotFound` for the caller's own tenant | `NOT_PERMITTED` |
| Tenant Resolver `Unauthorized` | `NOT_PERMITTED` |
| Tenant Resolver `NoPluginAvailable`, `ServiceUnavailable`, `Internal`, or `tenant_resolver_deadline_ms` expired | dependency unavailable |
| PDP `EnforcerError::EvaluationFailed`, including an expired `pdp_deadline_ms`, which the enforcer reports as `service_unavailable` | dependency unavailable |
| Types Registry `NotFound` | `invalid_argument` with `GTS_TYPE_NOT_REGISTERED` (§4 GTS Checks) |
| Types Registry `permission_denied` (the caller lacks the Types Registry read grant; projected as `TypesRegistryError::Other`) | `NOT_PERMITTED`, with an operator-visible log diagnostic naming the missing grant |
| Types Registry `Unavailable`, `DeadlineExceeded`, `Internal`, or `types_registry_deadline_ms` expired | dependency unavailable |
| CredStore `Ok(None)` for `secret_ref` | `invalid_argument`, field `secret_ref`, reason `SECRET_REF_NOT_FOUND` |
| CredStore `Ok(Some)` with no own row, or an own row with `status` `declared` | `invalid_argument`, field `secret_ref`, reason `SECRET_REF_NOT_OWNED` |
| CredStore `PermissionDenied` (the caller lacks the CredStore `read` grant) | `NOT_PERMITTED`, with an operator-visible log diagnostic naming the missing grant |
| CredStore `Unavailable`, `DeadlineExceeded`, `Internal`, or `credstore_deadline_ms` expired | dependency unavailable |

Database errors are classified with the toolkit-db contention check for serialization failures and deadlocks, and by error kind for connectivity and timeouts. The gear does not retry. Callers retry only `service_unavailable`, with backoff; every other category is final for the same request.

Authentication failures (401) are produced by the platform before the gear runs.

### Tenant Isolation and Data Protection

- Rows of a tenant reach other tenants only through the effective-catalog algorithm: downward, read-only, only when `shared`, and never across an isolation barrier. Reads never return sibling or descendant rows. Not-found answers do not distinguish "does not exist" from "not visible".
- Inheriting tenants see the full inherited record, including `cost`, `provider_settings`, `base_url` when set, and `secret_ref`, as the PRD requires (`cpt-cf-model-registry-fr-effective-catalog`). `secret_ref` is a name, not a secret; secret values are never stored or returned.
- `resolve_model` and `list_available_models` return the full record on the SDK and on REST alike, including `cost`, `provider_settings`, `base_url`, and `secret_ref`, as the PRD requires (`cpt-cf-model-registry-fr-resolve-model`, `cpt-cf-model-registry-fr-list-available-models`). There is no reduced record: both operations are granted to holders of model `evaluate`, and deployments that expose either to end users grant `evaluate` accordingly.
- A `secret_ref` is bound to the owning tenant: a tenant can store only references to secrets it owns (§4 Credential Binding).
- The gear stores configuration and subject ids of administrators (audit and approval columns), and no end-user content. Platform baselines for encryption at rest and in transit apply.
- Administrator subject ids stay for the life of their row and are removed with it by hard delete; copies in backups age out with the platform backup retention. Rows of a deleted tenant are removed by an operator with the runbook procedure (§4 Limitations).
- Logs and traces carry tenant id, ids, canonical id, slug, outcome, and reason code. They never carry `provider_settings`, `metadata`, `additional_info`, or request bodies.

### Credential Binding

A model's `secret_ref` is bound to the model's owning tenant. The binding check rejects an inherited or unknown reference on write; secret values stay governed by CredStore sharing and the `read_secret` action. Rules ([ADR-0002](./ADR/0002-cpt-cf-model-registry-adr-credential-binding.md)):

1. On model create with `secret_ref`, and on an update whose `secret_ref` is a non-null value different from the stored one, the gear reads the record through `CredStoreClientV2::get_record` under the caller's context (§3.5 Credential Ownership). The reference is accepted only when the response is `Ok(Some)` and the caller's own row holds a secret: `own` is present with `status` `active` or `expired`. `Ok(None)` is rejected with `SECRET_REF_NOT_FOUND`, no own row or a `declared` own row with `SECRET_REF_NOT_OWNED`, `PermissionDenied` with `NOT_PERMITTED`, and any other failure as dependency unavailable (§4 Error Handling). No row is written on rejection.
2. The read is metadata only; no secret value enters the gear (`cpt-cf-model-registry-constraint-no-secret-storage`). The secret's sharing mode and expiry are not checked: an expired secret is accepted; CredStore sharing is checked at evaluation.
3. The check runs before the transaction and holds no database connection; the write is otherwise unchanged. Clearing `secret_ref` (null) needs no CredStore call. A provider type migration does not re-check model secret references.
4. The check is not repeated on resolution or at evaluation. When CredStore cannot resolve the reference at evaluation, evaluation fails with `credentials_unavailable` while `resolve_model` keeps returning the model.

### Security Assumptions

Controls this gear relies on but does not enforce:

| Threat | Assumed control | Owner |
|--------|-----------------|-------|
| Stale binding: the owner removes its own secret and CredStore resolves the same `secret_ref` to an ancestor's `shared` secret | The fallback reaches only an ancestor secret `shared` to the owner's tenant; CredStore sharing and `read_secret` govern it ([ADR-0002](./ADR/0002-cpt-cf-model-registry-adr-credential-binding.md)). The owner suppresses the reference in CredStore (`fallback: none`); the stop covers its descendants only with `sharing: shared`. LLM Gateway sends provider credentials only through OAGW, whose egress policy admits only upstreams enabled for the caller's tenant and otherwise fails with `unapproved_destination`. | Tenant administrators (suppression); LLM Gateway team, with the OAGW team (egress) |
| Server-side request forgery (SSRF) through `base_url`: a model points the plugin at an internal host | The registry checks `base_url` on write only: optional, absolute `http` or `https`, no user information, at most 2,048 characters. SSRF protection is handled by OAGW at call time: the host must name an upstream enabled for the caller's tenant, and upstream connections are HTTPS only. | LLM Gateway team, with the OAGW team |
| Secret material in free-form fields (`base_url` query, `provider_settings`, `metadata`, `additional_info`), which inheriting tenants can read | Plugin schemas reference credentials through `secret_ref` and define no secret-valued fields; administrators put no secrets in free-form fields. The registry cannot detect secrets in opaque values. | LLM Gateway team (plugin schemas); tenant administrators |
| Catalog disclosure before the PDP: `resolve_model` reports not found, `PROVIDER_DISABLED`, or `MODEL_NOT_APPROVED` to a caller without `evaluate` | Accepted per the PRD resolution order. The disclosure is limited to the caller's own effective catalog. | Model Registry team |
| A malicious provider type schema registered in Types Registry | Type registration is trusted platform input from LLM Gateway provider plugins; Types Registry governs who may register types. | Types Registry team, with the LLM Gateway team |
| Validator resource exhaustion through a large document or schema | Documents are bounded by `max_json_document_bytes`; the validator applies its own recursion and size limits; the Types Registry read is bounded by `types_registry_deadline_ms`. | Model Registry team |
| Long-lived stale hierarchy | `chain_cache_ttl_seconds` is at most 300, checked at gear start. | Model Registry team |

### Observability

- Metrics: resolution duration histogram measured in-gear with PDP and Tenant Resolver wait subtracted; outcomes by reason code for `resolve_model` and `list_available_models`; chain cache hits, misses, and evictions; dependency errors by dependency (Tenant Resolver, PDP, Types Registry, CredStore, database); settings validation failures; secret reference rejections by reason; version mismatches.
- Availability SLI for `cpt-cf-model-registry-nfr-availability`: measured at the gear's interfaces (SDK and REST) for `resolve_model` and `list_available_models`, from the outcomes-by-reason metric. A bad event is an `internal` (500) or `service_unavailable` (503) response, including `service_unavailable` caused by a dependency. Every other response is good, including domain refusals such as model not found, `PROVIDER_DISABLED`, `MODEL_NOT_APPROVED`, and `NOT_PERMITTED`. The window is a calendar month and the target is 99.9% good events, an error budget of about 43 minutes per month. Recovery time runs from the first failed resolution to the first successful one after a gear failure, read from the same metric.
- Tracing: one span per operation and one child span per dependency call (Tenant Resolver, PDP, Types Registry, CredStore, database). The CredStore span carries the reference, never the value.
- Logs: structured records carrying the trace id. `info` per write (operation, tenant id, entity id, subject id, outcome); `warn` per dependency failure or deadline expiry; `error` per `internal`. Successful resolutions log nothing above `debug`. Aggregation follows the platform logging baseline.
- Security events: each `NOT_PERMITTED` and `INHERITED_READ_ONLY` is logged with subject, tenant, operation, and resource id. A write that changes `sharing_mode`, `enabled`, `gts_type` (type migration, with the number of models moved), or approval state is logged with the old and new values. Retention and tamper protection follow the platform log baseline.
- Alerts: availability SLI burn rate against the monthly error budget; in-gear p99 resolution latency above 10 ms for 10 minutes; dependency errors or deadline expiries above their baseline rate. Dashboards follow the platform monitoring baseline.
- Readiness: a replica reports ready through the platform gear lifecycle after its migrations run and its client is registered in ClientHub (start conditions in §4 Fault Tolerance and Consistency). Dependency health shows in the dependency-error metrics, not in readiness.

### Fault Tolerance and Consistency

| Failure | Behavior |
|---------|----------|
| Tenant Resolver unavailable or slow | Requests with a valid cache entry proceed; others wait at most `tenant_resolver_deadline_ms`, then fail with `service_unavailable` |
| PDP unavailable or slow | Each decision waits at most `pdp_deadline_ms`, then the request fails with `service_unavailable`; never grant |
| Types Registry unavailable or slow | Provider and model writes that need it wait at most `types_registry_deadline_ms`, then fail with `service_unavailable`; reads are unaffected |
| CredStore unavailable or slow | Model writes that bind a new `secret_ref` wait at most `credstore_deadline_ms`, then fail with `service_unavailable`; other writes and all reads are unaffected |
| Types Registry fails at replica start | The platform's GTS publication registers the gear's declared types before the gear starts (platform dependency). A failed registration prevents the server from becoming ready; running replicas are unaffected |
| Database unavailable or slow | Waits are bounded by the pool `acquire_timeout` and the PostgreSQL `statement_timeout` (§4 Configuration); then the request fails with `service_unavailable`. No catalog data is served from memory |
| Database serialization failure or deadlock | The transaction is rolled back and the request fails with `service_unavailable`. Read-only snapshot transactions take no row locks and raise neither; only writes can |
| Replica crash | No state lost; the platform restarts or replaces the replica |

No dependency or database call is retried inside the gear, on the resolution path or elsewhere; callers retry `service_unavailable` (§4 Error Handling).

Creates are not idempotent: a retried create after a lost response fails with `already_exists` on the slug or model name, and the caller reads the existing entry. A retried delete of a removed row reports not found.

Catalog rows are never cached, and every read runs its row queries in one read-only transaction at snapshot isolation (`cpt-cf-model-registry-principle-snapshot-reads`). A read on any replica reflects every write committed before its snapshot and none committed after it; a write committed during the request applies to the next request. Within one request the provider winner, its `enabled` and `sharing_mode`, the model's approval state, and every row of a list page come from one committed state, so a request never combines states that were never committed together: with an enabled provider and a pending model, a resolution that runs while a provider disable and then a model approval commit returns `MODEL_NOT_APPROVED` or `PROVIDER_DISABLED`, never the model, and an available list run at the same time never contains it. Pages of one listing are separate requests with separate snapshots. The chain and the PDP decision are not part of the snapshot: hierarchy changes become visible within `chain_cache_ttl_seconds` plus the duration of one request, and until then a stale chain can narrow or widen visibility (`cpt-cf-model-registry-component-chain-cache`). Concurrent updates of the same row are serialized by the row lock and guarded by `expected_version` and the recompare under the lock (guarantees of `cpt-cf-model-registry-seq-provider-write`, `cpt-cf-model-registry-seq-model-write`): two updates with the same `expected_version` apply in commit order and the second fails with `OPTIMISTIC_LOCK_FAILURE`, so a concurrent edit with `expected_version` cannot revert an approval change or `enabled`; an unconditional update changes only the fields it carries, may overwrite a concurrent change to a field it carries, and fails with `OPTIMISTIC_LOCK_FAILURE` when a skipped-check field it resends unchanged (`provider_settings`, `secret_ref`, or a provider's `gts_type`) was changed concurrently. Creates are protected against duplicates by the unique keys, and a model create whose provider was deleted concurrently fails with provider not found.

### Configuration

| Key | Default | Meaning |
|-----|---------|---------|
| `chain_cache_ttl_seconds` | 30 | Chain cache TTL; 0 disables the cache; at most 300 |
| `chain_cache_max_entries` | 100000 | Chain cache capacity |
| `default_page_size` | 20 | Page size when the request has no limit |
| `max_page_size` | 100 | Largest accepted page size |
| `max_json_document_bytes` | 65536 | Size limit for each of `provider_settings`, `metadata`, `additional_info`, `default_parameters` (`TOO_LARGE` when exceeded) |
| `pdp_deadline_ms` | 1000 | Deadline for one PDP decision, passed to `PolicyEnforcer::with_deadline` |
| `tenant_resolver_deadline_ms` | 1000 | Deadline for one `get_ancestors` call |
| `types_registry_deadline_ms` | 3000 | Deadline for one Types Registry schema read on writes |
| `credstore_deadline_ms` | 1000 | Deadline for one CredStore read in the `secret_ref` ownership check |
| `max_pdp_constraint_values` | 1000 | Largest value list in one PDP row-constraint filter; a larger list denies |

The keys live in the gear's `config` section of the platform YAML configuration, with the platform's environment-variable overrides; every key has a default, and absent keys take the defaults listed above. The gear checks the values at start and refuses to start when a check fails: `chain_cache_ttl_seconds` is between 0 and 300; `chain_cache_max_entries`, the page sizes, the document size, the deadlines, and `max_pdp_constraint_values` are greater than 0; `default_page_size` is at most `max_page_size`.

Database waits are bounded by the gear's `database` section of the platform configuration: the pool `acquire_timeout` (toolkit-db default 30 s) and, on PostgreSQL, the `statement_timeout` runtime parameter, which has no platform default. Deployments of the gear set both to 2 s.

### Testability

- Unit tests for the effective-catalog algorithm cover the hierarchy table of `cpt-cf-model-registry-seq-effective-catalog`, including its shadowing and dedupe rules.
- Integration tests run on SQLite and on PostgreSQL, the production backend, with migrations applied and mock Tenant Resolver, PDP, Types Registry, and CredStore clients, through the real service. Each test builds its own database (in-memory SQLite or a fresh PostgreSQL schema) and seeds its hierarchy through the service. Mocks return configured chains, PDP decisions with constraints, schemas, errors, and delays past deadlines.
- The service takes an injectable clock. Tests advance it to expire chain cache entries and pin audit and approval timestamps; TTL 0 tests the uncached path. No test sleeps.
- Promoted columns: integration tests on the storage layer check that the `cap_*`, `api_*`, and `ctx_*` columns stay in sync with their JSON sources after create and update.
- Contract tests: each REST endpoint and its SDK method give the same result and error category for the same input, including the full `ModelV1` from `GET /available-models/{canonical_id}`; the provider base type and permission instances the gear declares are valid GTS; stored settings fixtures validate against a sample derived provider type.
- End-to-end REST tests run against the example server and make at least one call per API method ([E2E guide](../../../docs/toolkit_unified_system/13_e2e_testing.md)). Coverage follows the workspace CI gates.
- A property test checks that `list_available_models` returns exactly the canonical ids for which `resolve_model` succeeds.
- Model identity: integration tests through the service cover resolution by `canonical_id`, `name` uniqueness per provider, and the stability of `canonical_id` across updates.
- REST round-trip tests pin every reason code and field reason to its Problem JSON location.
- The latency benchmark for `cpt-cf-model-registry-nfr-resolution-latency` runs on PostgreSQL at the NFR load, catalog size, and hierarchy depth. It also measures `list_available_models` in the default order at the same catalog size.
- Restart and backup-restore durability tests for `cpt-cf-model-registry-nfr-durability` run against a persistent PostgreSQL database.

PRD §9 acceptance criteria map to test layers:

| PRD §9 criterion | Test layer |
|------------------|------------|
| A model of a disabled provider never resolves and is absent from the available list | Unit (algorithm); integration on SQLite and PostgreSQL |
| A descendant sees shared ancestor providers and models and none of their `tenant`-mode entries | Unit; integration on SQLite and PostgreSQL |
| Inheritance stops at a self-managed tenant: it inherits nothing, and its descendants inherit from it only | Integration on SQLite and PostgreSQL |
| A `shared` shadowing provider replaces the inherited models of its slug for the tenant and its subtree | Unit; integration on SQLite and PostgreSQL |
| A `tenant`-mode shadowing provider replaces them for the owner only | Unit; integration on SQLite and PostgreSQL |
| A `shared`, disabled shadowing provider blocks the slug in the whole subtree | Unit; integration on SQLite and PostgreSQL |
| Deleting a shadowing provider restores the inherited provider and models | Integration on SQLite and PostgreSQL |
| Only approved models resolve, and a revoke stops resolution on the next call | Integration on SQLite and PostgreSQL; REST |
| Settings that violate the provider type schema are rejected with a validation error | Integration on SQLite (mock Types Registry); REST |
| A provider moves to a new type version, keeping ids and canonical ids, or not at all when one model fails validation | Integration on SQLite and PostgreSQL; REST |
| Management calls never return another tenant's owned entries beyond inherited candidates | Integration on SQLite and PostgreSQL (isolation matrix below); REST |
| Resolution meets the latency NFR | Benchmark on PostgreSQL |
| Resolution meets the availability NFR | Production monitoring of the availability SLI (§4 Observability) |

Security tests:

- Multi-tenant isolation matrix over a hierarchy with siblings and a self-managed subtree: no sideways reads, no reads across a barrier, no reads of descendant rows, no writes to ancestor rows, and inherited rows read-only (`INHERITED_READ_ONLY`).
- Every PDP constraint kind and error of §4 Authorization is exercised against a PDP stub; tests assert the result and reason of §4 Authorization and §4 Error Handling.
- Fail closed per dependency: PDP, Tenant Resolver, Types Registry, CredStore, and database failures and deadline expiries each yield `service_unavailable` and never a grant.
- Credential binding: integration tests of the write path against a mock CredStore client (the `get_record` seam) cover the accepted and rejected `secret_ref` states and reasons of §4 Credential Binding; unit tests cover the write conditions that trigger the CredStore read and the schema validation (§3.6 `cpt-cf-model-registry-seq-model-write`).
- Type migration: integration tests on SQLite and PostgreSQL, with a mock Types Registry client, cover the atomic migration of §3.6 `cpt-cf-model-registry-seq-provider-write` (all models switch with the provider, or no row changes). The concurrency guarantees of §3.6 are covered on PostgreSQL only, because SQLite serializes writers, so these interleavings cannot be reproduced there.
- Optimistic concurrency: integration and REST tests cover the precondition rules of §3.3 (stale `expected_version` or `If-Match`, `IF_MATCH_REQUIRED`, `If-Match: *`) and that a stale update cannot re-approve a revoked model.
- A caller `$filter` cannot widen visibility on any list.
- A self-managed tenant sees none of its ancestors' entries, `shared` or not; a descendant of a self-managed tenant sees that tenant's `shared` entries and nothing from above it; ancestors see nothing of the self-managed subtree; any ancestor-rooted subtree constraint denies.
- Concurrent updates of different fields leave a revoke or a provider disable intact, and an unconditional provider update that resends the old `gts_type` during a type migration fails (§3.6); PostgreSQL only, because SQLite serializes writers, so these interleavings cannot be reproduced there.
- Snapshot reads (`cpt-cf-model-registry-principle-snapshot-reads`; PostgreSQL only, for the same reason): the service exposes a test-only pause point between the candidate provider query and the model query of a read transaction. With an enabled provider and a pending model, a test pauses a resolution there, commits a provider disable and then a model approval on other connections, and resumes it: the result is `MODEL_NOT_APPROVED`, never `ModelV1`, and a resolution started after both commits reports `PROVIDER_DISABLED`. The same sequence around `list_available_models` never lists the model, and around `list_models` returns the entry with `provider_enabled` true and approval `pending`, never `provider_enabled` false with `pending` or `provider_enabled` true with `approved`.

### Documentation

Rustdoc covers every public item of the SDK crate. The gear README lists the configuration keys of §4 Configuration. An operator runbook covers dependency outages, chain cache TTL changes, recovery of an accidental delete from the platform backup, and removal of a deleted tenant's rows.

### Compliance

Platform baselines apply ([ARCHITECTURE_MANIFEST.md](../../../docs/ARCHITECTURE_MANIFEST.md), [SECURITY.md](../../../docs/security/SECURITY.md)). The gear holds no end-user personal data and no secrets. It stores administrator subject ids in audit and approval columns; consent, privacy impact assessment, and cross-border transfer of these ids follow the platform baseline for administrator identity data. Retention is in §4 Tenant Isolation and Data Protection.

### Not Applicable

- User-facing architecture, accessibility, internationalization, offline use, safety: the gear has no user interface; consumer gears own presentation (PRD §6.2).
- CDN and edge caching: SDK calls are in-process; REST traffic is administrative.
- Events, dead-letter queues, poison messages, sagas, distributed transactions: the gear uses no messaging; every write is a single-database transaction. Change notifications are p2 (§4 Open Questions).
- Authentication, MFA, SSO, token and session management: owned by the platform; the gear consumes the authenticated `SecurityContext` and propagates it on service-to-service calls (§3.4, §3.5).
- Queues, sessions, load balancing: replicas are stateless; request distribution is owned by the platform.
- CORS, DMZ, firewall, network segmentation: REST enters through API Gateway; the gear runs in-process and has no listener.
- Rate limiting and overload protection: owned by API Gateway ([platform gears overview](../../../docs/GEARS.md#api-ingress)) and the platform server.
- IaC, containers, autoscaling, resource tagging: owned by the platform server deployment.
- Partitioning, sharding, tiering, archival: the catalog is small, deletes are hard, and no history is kept.
- Replication, backup, point-in-time recovery, disaster recovery: the platform database baseline applies. An accidental hard delete is recovered by an operator from the platform backup; the gear has no undelete.
- Cost budgets and time-to-market: no new infrastructure; the gear adds two tables to the platform database and runs in-process.
- Circuit breaker, bulkhead: none in the gear; each dependency call has a deadline and fails closed with `service_unavailable`; callers retry (§4 Error Handling).
- Rollback, canary, blue-green: platform deployment baseline; schema changes are expand-contract (§3.8), so a rollback to the previous release keeps working.
- Data lineage, data catalog, data dictionary, data-quality monitoring: not applicable; the gear owns two configuration tables and has no analytical data flows.
- Technical-debt roadmap, deprecation timeline: not applicable beyond §4 Limitations and the versioning rules in §3.3.
- Data residency, licensing, legacy integration, team and budget constraints: platform baselines and the workspace license policy apply; the gear is new and replaces no legacy system.

### Change Audit Follow-up

`cpt-cf-model-registry-fr-change-audit` is p2. This design provides per-row audit columns and approval decision columns. The append-only change log that records deletes and full change history, readable by the owning tenant's administrator, is not part of this design; it is designed with the p2 scope (owner: Model Registry team).

### Deviations

| Deviation | Baseline | Rationale | Review owner |
|-----------|----------|-----------|--------------|
| Reads run under an `AccessScope` the gear builds from the ancestor chain, with the PDP tenant constraint as a gate. | [Secure ORM](../../../docs/toolkit_unified_system/06_authn_authz_secure_orm.md): do not construct `AccessScope` manually in production code | A flat tenant clamp would drop the inherited half of the catalog. [ADR-0001](./ADR/0001-cpt-cf-model-registry-adr-inherited-read-scope.md) | Platform Architecture |
| Update replaces present top-level fields as a whole, including nested objects; it does not merge nested objects. | [REST guideline](../../../guidelines/DNA/REST/API.md): PATCH with JSON Merge Patch | Gives SDK and REST identical update semantics and makes "settings changed" a comparison of the sent object with the stored one. [ADR-0004](./ADR/0004-cpt-cf-model-registry-adr-update-semantics.md) | Platform Architecture |
| A stale `If-Match` is reported as `aborted` (409), not 412. | [REST guideline](../../../guidelines/DNA/REST/API.md): `If-Match` mismatch is 412 | The canonical error categories have no 412; CredStore reports the same condition as 409. [ADR-0004](./ADR/0004-cpt-cf-model-registry-adr-update-semantics.md) | Platform Architecture |
| `gts_type` is stored as text on both tables, not as the GTS UUID. | [GTS guideline](../../../guidelines/GTS.md) §5.1: GTS types stored as UUIDs | The value is an OData filter field and a PDP property in its string form and is passed to Types Registry by id; no join or foreign key uses it | Model Registry team |
| `cpt-cf-model-registry-fr-change-audit` (p2) is covered by per-row audit columns only; deletes and change history are not recorded. | `cpt-cf-model-registry-fr-change-audit` | p2 scope; the change log is designed with the p2 scope (§4 Change Audit Follow-up) | Model Registry team |

### Limitations

- The gear has no tenant event surface. A hierarchy change, including a change of a tenant's `self_managed` flag, is picked up only through the chain cache TTL.
- Rows of a deleted tenant stay until an operator removes them with the runbook procedure (§4 Documentation). Owner: Model Registry team provides the procedure; platform operations run it.
- The gear does not check that an LLM Gateway plugin instance exists for a provider type; LLM Gateway reports a missing plugin at evaluation time.

### Open Questions

PRD §13 questions, with their disposition in this design:

- External approval service and model auto-discovery: carried open to P2 planning; out of scope of this design (`cpt-cf-model-registry-constraint-no-model-lifecycle`) (owners: Platform Architecture; Model Registry team).
- Change notifications (`cpt-cf-model-registry-fr-change-notifications`): event types and payload, recipients of a change to a shared entry, and crash-safe publication after commit (for example, an outbox in the catalog transaction). Carried open to P2 planning; this design publishes no events (owner: Model Registry team).

New in this design: none.

## 5. Traceability

- **PRD**: [PRD.md](./PRD.md)
- **ADRs**: [ADR-0001](./ADR/0001-cpt-cf-model-registry-adr-inherited-read-scope.md) inherited read scope; [ADR-0002](./ADR/0002-cpt-cf-model-registry-adr-credential-binding.md) credential binding; [ADR-0003](./ADR/0003-cpt-cf-model-registry-adr-caching-strategy.md) caching strategy; [ADR-0004](./ADR/0004-cpt-cf-model-registry-adr-update-semantics.md) update semantics and concurrency
- **Features**: none yet
- **SDK crate**: [model-registry-sdk](../model-registry-sdk/) (`cf-gears-model-registry-sdk`)
- **Gear crate**: `gears/model-registry/model-registry/` (`cf-gears-model-registry`). Layers: `api/rest` (DTOs, handlers, routes), `domain` (local client, catalog service, visibility resolver, access guard, GTS validator), and `infra` (storage entities, repositories, mapper, migrations, chain cache); gear registration and configuration at the crate root.
- **Platform sources**: [ARCHITECTURE_MANIFEST.md](../../../docs/ARCHITECTURE_MANIFEST.md), [guidelines](../../../guidelines/README.md), [REST API guideline](../../../guidelines/DNA/REST/API.md), toolkit [OperationBuilder](../../../docs/toolkit_unified_system/04_rest_operation_builder.md), [errors](../../../docs/toolkit_unified_system/05_errors_rfc9457.md), [secure ORM](../../../docs/toolkit_unified_system/06_authn_authz_secure_orm.md), [OData](../../../docs/toolkit_unified_system/07_odata_pagination_select_filter.md).
- **Precedence**: the PRD for requirements; platform sources for conventions, except where §4 Deviations records a departure; this DESIGN for gear specifics.
