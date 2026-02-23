# PRD — Simple Resource Registry

## 1. Overview

### 1.1 Purpose

Simple Resource Registry is a universal, environment-agnostic CRUD storage layer for resource types that are too simple to justify their own CyberFabric module. It exposes a single, consistent API for creating, reading, updating, and deleting typed resources, using a fixed envelope (identity, ownership, timestamps) plus a flexible JSON payload validated against GTS type definitions. The registry is designed to run on any deployment target — edge devices (SQLite), cloud infrastructure (PostgreSQL, MariaDB), and enterprise on-premises environments — adapting its storage backend to the deployment constraints without changing the consumer-facing API.

The module solves a recurring problem in modular SaaS platforms: many features need secure, schema-aware object storage with proper authorization (by tenant, owner/user, and resource type), but do not require the complexity of a full domain-specific service. Instead of building custom modules or relying on ad-hoc storage with inconsistent security and governance, one can use the Simple Resource Registry for cases such as simple objects storage, workflow-generated objects, projections of external system entities, partial models used for internal consistency, tenant-level configuration data, and auxiliary artifacts produced by agent execution.

This module is designed for lightweight structured data sets, on the order of up to ~1M items per resource type and ~100M total, and is not intended to serve as a general-purpose database for the entire platform.

Optionally, the registry can emit lifecycle notification events (created, updated, deleted) for configured resource types, allowing these resources to participate directly in workflows and to act as workflow triggers.

### 1.2 Background / Problem Statement

In a modular SaaS platform like CyberFabric, first-class domain objects (chat messages, model definitions, events, settings, files) are managed by dedicated modules with rich APIs and domain-specific behavior. However, many resource types lack the complexity to justify a dedicated module — they need simple CRUD semantics with tenant isolation and standard governance hooks (audit, events).

Without a generic registry, teams face two poor choices: either build a new module for every simple resource type (high cost, code duplication) or store resources in ad-hoc locations (inconsistent APIs, missing security controls, no traceability). Simple Resource Registry eliminates this by providing a single, extensible storage layer that any module or workflow can use for structured data that conforms to a GTS-registered type.

The storage layer is abstracted behind a well-defined interface, allowing the same API to serve relational database-backed resources today and alternative backends (search engines, object stores) in the future, without changing consumers. Platform vendors can also implement their own storage backends to integrate with existing platform components that already store the appropriate resources, effectively using Simple Resource Registry as a unified API façade over heterogeneous storage systems. Multiple storage backends can coexist, with different resource types routed to different backends via configuration.

### 1.3 Goals (Business Outcomes)

- Reduce time-to-ship for features that require simple resource storage from days to hours by eliminating the need for a dedicated module
- Provide a single, consistent CRUD API surface for generic resources across the platform
- Validate resource payloads against GTS type definitions to ensure schema consistency and safe extensibility
- Enforce tenant and owner isolation, authentication, and GTS-driven attribute-based access control (ABAC) uniformly for all registered resource types
- Enable governance (audit and lifecycle events) for all resource operations without per-type implementation effort

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Resource | A single stored object managed by the registry, consisting of a fixed GTS schema envelope and a type-specific JSON payload |
| Resource Type (`type`)| A GTS-registered type definition that describes the schema of a resource's payload and its behavioral flags (event/audit configuration). The resource `type` value is a GTS type identifier in GTS ID format — i.e., resource type == resource type ID == GTS type ID. |
| Resource Tenant (`tenant_id`) | The platform tenant (organization / workspace) that owns the resource. Set from `SecurityContext.subject_tenant_id` and never caller-supplied. Every resource must belong to exactly one tenant. Tenant isolation is enforced at the storage query level. |
| Resource Owner (`owner_id`) | The subject (typically a human user or a registered API client identified by their `subject_id`) that created or is associated with the resource. Set from `SecurityContext.subject_id`. Populated only when `is_per_owner_resource=true` on the resource type. When set, owner-scoped resources are filtered by `owner_id = ctx.subject_id` at the storage query level, restricting visibility to that subject within the tenant. |
| Base Resource Type | The GTS schema defining the common envelope fields shared by all resources (id, tenant, timestamps, etc.) |
| Derived Resource Type | A GTS type that extends the base resource type with a specific payload schema and behavioral configuration |
| Storage Backend | An interchangeable storage implementation responsible for persisting and querying resources. The registry abstracts storage behind a well-defined interface, allowing different backends (relational databases, search engines, vendor-provided stores) to be used depending on deployment environment and resource type needs. |
| Schema Fields | The fixed set of envelope fields (id, tenant_id, owner_id, type, timestamps) stored as dedicated queryable fields and filterable via OData |
| Payload | A JSON object stored alongside schema fields, whose structure is defined by the resource's GTS type; not queryable via OData |
| Deleted Resource Retention | Configurable period after which soft-deleted resources are permanently purged from storage (default: 30 days) |
| GTS Type-Based Access Control | Authorization mechanism that checks whether the caller's token permissions include a matching GTS resource_pattern and action for the target resource type |
| Resource Group | A logical grouping of resources that share a common lifecycle or ownership context (`cpt-cf-srr-fr-resource-groups`) |

## 2. Actors

### 2.1 Human Actors

#### Platform User

**ID**: `cpt-cf-srr-actor-platform-user`

**Role**: Authenticated user interacting with CyberFabric applications or UI components that store and retrieve resources through the Simple Resource Registry.
**Needs**: Reliable and secure storage of application data. Predictable behavior and access control aligned with tenant and user permissions. Ability to create, view, update, and delete resources through UI workflows

#### API Client

**ID**: `cpt-cf-srr-actor-api-client`

**Role**: External or internal consumer calling the Simple Resource Registry REST API directly (e.g., third-party integrations, CLI tools, automated scripts).
**Needs**: OData-capable query interface for filtering, ordering, and paginating resources by schema fields. Predictable HTTP semantics and standard error responses.

### 2.2 System Actors

#### Consumer Module

**ID**: `cpt-cf-srr-actor-consumer-module`

**Role**: Internal CyberFabric module that creates, reads, updates, or deletes resources via the SDK client (e.g., Workflows engine storing custom objects, Agent Runtime storing execution artifacts).

## 3. Operational Concept & Environment

No module-specific environment constraints beyond project defaults. The module operates within the standard CyberFabric modkit lifecycle and uses the platform's shared database infrastructure.

## 4. Scope

### 4.1 In Scope

- CRUD REST API for generic resources with fixed schema envelope + JSON payload
- GTS-based resource type definition (base type + derived types)
- Tenant-scoped storage with mandatory tenant isolation via SecurityContext
- Optional user-scoped resources (per-user ownership when resource type requires it)
- OData $filter/$orderby and cursor-based pagination for schema fields (id, tenant_id, owner_id, type, created_at, updated_at, deleted_at)
- GTS type-based access control on all CRUD operations (permissions checked against token's GTS resource_pattern + action)
- GTS wildcard filtering on resource listing (trailing `*` per GTS spec)
- Multi-backend storage architecture with interchangeable storage implementations; relational database as the default backend
- Per-resource-type event emission (created, updated, deleted) via Events Broker
- Per-resource-type audit event emission via Audit Module
- SDK client for in-process consumption by other modules
- Soft-delete support via deleted_at timestamp
- Configurable deleted resource retention with automatic purge of soft-deleted resources (default: 30 days, type-level override)
- Dedicated search API with backend capability checks (`cpt-cf-srr-fr-search-api`)
- Batch CRUD operations per DNA BATCH.md (`cpt-cf-srr-fr-batch-operations`)
- Resource groups support (`cpt-cf-srr-fr-resource-groups`)

### 4.2 Out of Scope

- OData query support for payload fields (payload is opaque JSON; filtering/ordering within it is not supported)
- Full-text search across payload content (use external index if needed)
- Resource versioning or change history tracking
- Cross-resource-type queries (e.g., all resources regardless of type in a single request)
- Resource relationships or foreign key enforcement between resources
- Complex business logic or domain-specific validation beyond JSON Schema
- Real-time streaming or SSE for resource changes (consumers use Events Broker for notifications)
- File or binary attachment storage (use File Storage module)
- UI components or admin dashboards
- Automatic resource soft-deletion based on TTL / auto-deletion retention (error-prone; must not be accidentally enabled)
- OData `$select` field projection (not applicable — all schema fields and payload are always returned; the payload is an opaque JSON object whose structure varies by resource type)

## 5. Functional Requirements

### 5.1 Core CRUD and Storage

> IMPORTANT NOTE: All API endpoints in this section must enforce tenant access scope, user access scope (when applicable), and GTS type access scope filtering.

#### Create Resource

- [ ] `p1` - **ID**: `cpt-cf-srr-fr-create-resource`

The system **MUST** allow authenticated users to create a new resource by specifying a resource `type` (a GTS type ID), a mandatory `idempotency_key` (UUID), and a JSON payload. The system **MUST** assign a system-generated UUID (if not provided) as the resource `id`, set `tenant_id` from `SecurityContext.subject_tenant_id`, set `owner_id` from `SecurityContext.subject_id` (when `is_per_owner_resource=true`), and set `created_at` and `updated_at` timestamps. If the target resource type is per-owner (`is_per_owner_resource=true`) and `SecurityContext.subject_id` is absent, the system **MUST** return 422 Unprocessable Entity. The system **MUST** validate the payload against the derived GTS type's JSON Schema (using the `gts` crate) and return 422 Unprocessable Entity if validation fails.

**Rationale**: Core functionality — all consumers need to persist typed resources.
**Actors**: `cpt-cf-srr-actor-platform-user`, `cpt-cf-srr-actor-consumer-module`

#### Idempotent Resource Creation

- [ ] `p1` - **ID**: `cpt-cf-srr-fr-idempotent-create`

The system **MUST** require a `idempotency_key` in every POST /resources request body. The storage plugin **MUST** atomically check for an existing `(tenant_id, idempotency_key)` record and persist the resource + idempotency record in a single transaction. If a matching record exists and is within the retention window (default 24 h), the plugin returns `CreateOutcome::Duplicate` and the system **MUST** return 409 Conflict with the `id` of the previously created resource. Idempotency keys are scoped per tenant — the same key in different tenants is independent. Idempotency deduplication is storage-backend-owned; callers must always supply a unique key (e.g., a UUID) per intended creation.

**Rationale**: Distributed consumers and workflow engines frequently retry failed HTTP requests. Without idempotency, retries after network failures create duplicate resources. The idempotency key lets callers safely retry POST requests without risk of double-creation.
**Actors**: `cpt-cf-srr-actor-consumer-module`, `cpt-cf-srr-actor-platform-user`

#### Read Single Resource

- [ ] `p1` - **ID**: `cpt-cf-srr-fr-read-resource`

The system **MUST** allow authenticated users to retrieve a single resource by ID. All access checks — tenant scope, GTS type scope (from token claims), and owner_id scope (when `is_per_owner_resource=true`) — are applied as backend query filters. If the resource does not exist or is filtered out by any of these security filters, the system **MUST** return 404 Not Found. The caller cannot distinguish between "does not exist" and "not authorized" for individual resources.

**Rationale**: Core functionality — consumers need to fetch individual resources.
**Actors**: `cpt-cf-srr-actor-platform-user`

#### List Resources

- [ ] `p1` - **ID**: `cpt-cf-srr-fr-list-resources`

The system **MUST** allow authenticated users to list resources filtered by resource `type` (a GTS type ID), with OData query support ($filter, $orderby) and cursor-based pagination (limit, cursor) on schema fields. All access checks — tenant scope, GTS type scope (from token claims), and owner_id scope (when `is_per_owner_resource=true`) — are applied as backend query filters. If the requested type filter does not intersect with the caller's permitted GTS types, the system **MUST** return 403 Forbidden. Otherwise, results are filtered at the backend level and an empty result set is a valid response (not an error).

**Rationale**: Consumers need to discover and paginate through resources of a given type.
**Actors**: `cpt-cf-srr-actor-platform-user`

#### Update Resource

- [ ] `p1` - **ID**: `cpt-cf-srr-fr-update-resource`

The system **MUST** allow authenticated users to update the payload of an existing resource. The system **MUST** update the updated_at timestamp. All access checks — tenant scope, GTS type scope (from token claims), and `owner_id` scope (when `is_per_owner_resource=true`) — are applied as backend query filters. If the resource does not exist or is filtered out by any of these security filters, the system **MUST** return 404 Not Found.

**Rationale**: Core functionality — consumers need to modify resource data.
**Actors**: `cpt-cf-srr-actor-platform-user`, `cpt-cf-srr-actor-consumer-module`

#### Delete Resource

- [ ] `p1` - **ID**: `cpt-cf-srr-fr-delete-resource`

The system **MUST** allow authenticated users to soft-delete a resource by setting the deleted_at timestamp. All access checks — tenant scope, GTS type scope (from token claims), and `owner_id` scope (when `is_per_owner_resource=true`) — are applied as backend query filters. If the resource does not exist or is filtered out by any of these security filters, the system **MUST** return 404 Not Found. Soft-deleted resources **MUST** be excluded from list results by default.

**Rationale**: Core functionality — consumers need to remove resources while maintaining audit trail.
**Actors**: `cpt-cf-srr-actor-platform-user`, `cpt-cf-srr-actor-consumer-module`

#### GTS Resource Type Registration

- [ ] `p1` - **ID**: `cpt-cf-srr-fr-gts-type-registration`

The system **MUST** define a base GTS resource type schema with major-version-only versioning. Derived resource types **MUST** extend the base type via GTS schema inheritance. The base type **MUST** include:
- Behavioral flags: is_per_owner_resource, is_create_event_needed, is_delete_event_needed, is_update_event_needed, is_create_audit_event_needed, is_update_audit_event_needed, is_delete_audit_event_needed
- Hard-delete retention configuration: deleted_resource_retention_days (integer or null; default 30 if null; 0 means immediate hard-delete on soft-delete)

**Rationale**: GTS type system ensures consistency, discoverability, and validation for all resource types. Embedding the deleted-resource retention policy in the type definition keeps it co-located with other behavioral configuration.

#### Default Storage Backend

- [ ] `p1` - **ID**: `cpt-cf-srr-fr-default-storage-backend`

The system **MUST** ship with a default storage backend that uses a relational database as its persistence layer. The default backend **MUST** store schema fields as dedicated queryable columns and the payload as a JSON column. The default backend **MUST** support OData query operations on schema fields and **MUST** work on PostgreSQL, MariaDB, and SQLite without requiring database-specific configuration.

**Rationale**: Relational databases are the standard CyberFabric storage tier, providing ACID guarantees and existing infrastructure across all deployment targets (edge, cloud, enterprise). OData support enables standard filtering and pagination.

#### OData Query Support for Schema Fields

- [ ] `p1` - **ID**: `cpt-cf-srr-fr-odata-schema-fields`

The system **MUST** support OData $filter and $orderby operations, plus cursor-based pagination (limit, cursor) on schema fields (id, tenant_id, owner_id, type, created_at, updated_at, deleted_at). OData operations on payload fields are explicitly not supported. List responses **MUST** use the `items`/`page_info` envelope per DNA API guidelines.

**Rationale**: Consumers need standard query capabilities for resource discovery and pagination without requiring module-specific code.
**Actors**: `cpt-cf-srr-actor-api-client`, `cpt-cf-srr-actor-platform-user`

#### GTS Type-Based Access Control

- [ ] `p1` - **ID**: `cpt-cf-srr-fr-gts-access-control`

All CRUD operations (GET, POST, PUT, DELETE) **MUST** enforce GTS type-based access control using token `Permission` entries (`resource_pattern` + `action`, where action is `read`/`create`/`update`/`delete`). GTS wildcard matching rules apply: a permission with `resource_pattern` = `gts.x.srr.resource.v1~acme.*` grants access to all derived types under the `acme` vendor namespace. For POST, if the requested resource `type` is not in scope, the system **MUST** return 403 Forbidden with error code `gts-type-not-in-scope`. For LIST, if the requested type filter has no intersection with the caller's permitted GTS types, the system **MUST** return 403 Forbidden with error code `gts-type-not-in-scope`. For individual resource operations (GET/PUT/DELETE), type scope is enforced via backend query filters (together with tenant and user filters), so out-of-scope resources are not returned and the API **MUST** return 404 Not Found.

**Rationale**: Resources in the registry represent diverse data types with different sensitivity levels. GTS type-based access control ensures that API consumers can only operate on resource types explicitly granted in their token, preventing unauthorized access to resource families the consumer was not designed or approved to use.
**Actors**: `cpt-cf-srr-actor-platform-user`, `cpt-cf-srr-actor-consumer-module`

#### GTS Wildcard Filtering on List

- [ ] `p1` - **ID**: `cpt-cf-srr-fr-gts-wildcard-filtering`

The GET /resources list endpoint **MUST** support filtering by GTS type ID with trailing wildcard (`*`) per GTS spec section 10. The wildcard **MUST** appear only once, at the end of the pattern, and is greedy (matches through `~` chain separator). Example: `type eq 'gts.x.srr.resource.v1~acme.*'` matches all derived types under the `acme` vendor. When a wildcard filter is used, the system **MUST** only return resources whose GTS types fall within the caller's permitted scope (intersection of wildcard query with token permissions).

**Rationale**: Consumers often need to discover resources across a family of related types (e.g., all resources from a vendor namespace) without knowing every specific derived type.
**Actors**: `cpt-cf-srr-actor-platform-user`, `cpt-cf-srr-actor-consumer-module`

#### GTS Type Existence Validation

- [ ] `p1` - **ID**: `cpt-cf-srr-fr-gts-type-validation`

On POST (create) and any operation that references a resource `type` (a GTS type ID), the system **MUST** verify that the specified GTS type exists in the Types Registry. If the GTS type is not found, the system **MUST** return 400 Bad Request with appropriate error code and include the unresolved GTS type ID in the error response.

**Rationale**: Prevents creation of orphaned resources with invalid type references and provides clear error feedback.
**Actors**: `cpt-cf-srr-actor-platform-user`, `cpt-cf-srr-actor-consumer-module`

#### Deleted Resource Retention

- [ ] `p3` - **ID**: `cpt-cf-srr-fr-deleted-retention`

The system **MUST** support configurable retention for soft-deleted resources. The default retention period is 30 days, but it must be configurable globally on service level. Individual resource types **MAY** override the default by specifying `deleted_resource_retention_days` in the GTS type definition. A value of 0 means immediate hard-delete upon soft-delete. A value of null inherits the system default (30 days). After the retention period expires, the system **MUST** permanently purge the resource from storage via a dedicated Jobs Manager job.

**Rationale**: Soft-deleted resources accumulate storage over time. Configurable retention balances data recovery needs with storage efficiency while allowing type authors to define appropriate policies for their data.
**Actors**: `cpt-cf-srr-actor-consumer-module`

#### Batch Operations

- [ ] `p2` - **ID**: `cpt-cf-srr-fr-batch-operations`

The system **MUST** support batch CRUD operations on resources following the DNA batch conventions (`POST /resources:batch` and `POST /resources:batch-get`):
- **Batch GET** (`POST /resources:batch-get`): Retrieve multiple resources by a list of IDs in a single request.
- **Batch Create/Update/Delete** (`POST /resources:batch`): Process multiple create, update, and delete operations in a single request. Each item specifies its action, `type` (for create), and `payload`. The system **MUST** validate each item independently and return per-item results.

All batch operations **MUST** return `207 Multi-Status` for partial success with per-item results (including `index`, HTTP `status`, `data` or RFC 9457 Problem Details `error`), per DNA BATCH.md conventions. Per-item `idempotency_key` **SHOULD** be supported for safe retries. All batch operations **MUST** enforce the same authentication, tenant scoping, GTS type-based access control, and behavioral flag evaluation as their single-resource counterparts. Batch size **MUST** be capped by a configurable limit (default: 100 items per request).

**Rationale**: Consumers that manage related resources (workflow engines, data connectors, agent pipelines) frequently need to operate on multiple resources in a single logical operation. Batch endpoints reduce round-trip overhead and improve throughput for bulk workloads.
**Actors**: `cpt-cf-srr-actor-consumer-module`, `cpt-cf-srr-actor-platform-user`


### 5.2 Events and Audit

#### Notification Events

- [ ] `p2` - **ID**: `cpt-cf-srr-fr-notification-events`

The system **MUST** emit domain events (resource.created, resource.updated, resource.deleted) to the Events Broker when the corresponding behavioral flags are enabled on the resource's GTS type definition. The event schema **MUST** include: `id` (event id), `type` (event type), `subject_type` (resource type), and `subject_id` (resource id). The event payload **MUST NOT** include the full resource payload to keep events lightweight.

**Rationale**: Enables reactive integrations — other modules can respond to resource lifecycle changes without polling. Fixed event schema ensures consistent event handling across all resource types.

#### Audit Events

- [ ] `p2` - **ID**: `cpt-cf-srr-fr-audit-events`

The system **MUST** emit audit events for create, update, and delete operations to the Audit Module when the corresponding audit flags are enabled on the resource's GTS type definition or any of the parent GTS type. The audit event schema **MUST** be fixed and include: `id` (event id), `type` (event type), `subject_type` (resource type), `subject_id` (resource id), previous resource payload (null for create), and new resource payload (null for delete). This enables full audit trail reconstruction.

**Rationale**: Compliance and traceability requirements demand auditable resource operations with complete before/after state for change tracking.

### 5.3 Multi-Backend Storage

#### Multi-Backend Storage

- [ ] `p3` - **ID**: `cpt-cf-srr-fr-multi-backend-storage`

The system **MUST** support multiple interchangeable storage backends beyond the default relational database (e.g., search engines for full-text-searchable resources, vendor-provided stores for existing platform data). New backends **MUST** conform to the storage backend interface and declare their capabilities (e.g., `odata_support`, `search_support`). Platform vendors **MAY** implement custom storage backends to integrate with existing platform components that already persist the appropriate resources (e.g., a vendor-specific CMDB, asset inventory, or configuration store). Multiple backends **MAY** coexist simultaneously, with per-resource-type routing directing operations to the appropriate backend.

**Rationale**: Different resource types have different query and scalability needs; a single storage backend cannot serve all use cases optimally. Backend capabilities enable the API layer to route requests appropriately and return errors when a requested operation is not supported by the target backend. Allowing vendor-provided backends enables the registry to act as a unified API façade over heterogeneous storage systems without requiring data migration.
**Actors**: `cpt-cf-srr-actor-platform-user`

#### Per-Resource-Type Storage Routing

- [ ] `p3` - **ID**: `cpt-cf-srr-fr-storage-routing`

The system **MUST** support configuration-level routing that maps GTS resource types to specific storage backends. When a resource is created or queried, the system **MUST** route the operation to the configured storage backend for that resource type. Unrouted types **MUST** fall back to the default storage backend.

**Rationale**: Enables heterogeneous storage strategies — relational databases for transactional resources, search engines for search-heavy resources — managed through configuration.
**Actors**: `cpt-cf-srr-actor-platform-user`

#### Resource Groups

- [ ] `p3` - **ID**: `cpt-cf-srr-fr-resource-groups`

The system **MUST** support logical grouping of resources into resource groups. A resource group is identified by a UUID and associated with a tenant. Resources **MAY** belong to zero, one or many resource groups. Resource groups enable batch operations (e.g., delete all resources in a group), lifecycle management (e.g., auto-delete a group when a parent workflow completes), and organizational queries (e.g., list all resources in a group).

**Rationale**: Many use cases produce multiple related resources (workflow steps, agent execution artifacts) that share a lifecycle. Resource groups provide a first-class mechanism for managing these collections without requiring consumer-side tracking.
**Actors**: `cpt-cf-srr-actor-consumer-module`

#### Resource Search API

- [ ] `p4` - **ID**: `cpt-cf-srr-fr-search-api`

The system **MUST** provide a dedicated search API endpoint (POST /resources:search) that accepts full-text search queries and filters. The search operation **MUST** rely on the storage backend's `search_support` capability. If the target resource type's storage backend does not support search, the API **MUST** return 501 Not Implemented (RFC 9457 Problem Details). Search queries **MUST** support:
- Full-text search within resource payloads
- Filtering by schema fields (type, tenant_id, owner_id, timestamps)
- Pagination and result ranking

**Rationale**: Full-text search within JSON payloads is not feasible with OData on relational databases. A dedicated search API enables consumers to leverage search-capable plugins (e.g., ElasticSearch) when available, while providing clear error feedback when the feature is unavailable for a given resource type.
**Actors**: `cpt-cf-srr-actor-platform-user`, `cpt-cf-srr-actor-consumer-module`

## 6. Non-Functional Requirements

### 6.1 Module-Specific NFRs

#### Single-Resource Read Latency

- [ ] `p1` - **ID**: `cpt-cf-srr-nfr-read-latency`

Single-resource GET operations **MUST** respond within 50ms at p95 under normal load (stricter than project default due to registry being a frequently called building block).

**Threshold**: 50ms p95 for single-resource reads
**Rationale**: The registry serves as a data access primitive for other modules and workflows; high latency compounds across dependent operations.
**Architecture Allocation**: See DESIGN.md section "NFR Allocation"

#### Payload Size Limit

- [ ] `p1` - **ID**: `cpt-cf-srr-nfr-payload-size`

Individual resource payloads **MUST** not exceed 64 KB.

**Threshold**: 64 KB per resource payload
**Rationale**: Prevents storage abuse and ensures predictable query performance; large binary data belongs in File Storage.
**Architecture Allocation**: See DESIGN.md section "NFR Allocation"

#### Scalability

- [ ] `p1` - **ID**: `cpt-cf-srr-nfr-scalability`

The system **MUST** support storing up to 100 million total resources across all resource types. The default storage backend **MUST** sustain at least 100 write operations per second (resource creates/updates) and at least 1000 single-resource fetch-by-ID operations per second under normal load.

**Threshold**: 100M total resources; 100 writes/s; 1000 reads-by-ID/s
**Rationale**: The registry is a shared building block used across the platform. Scalability targets ensure it can serve as the primary storage layer for lightweight resource types without becoming a bottleneck. How these targets are achieved is a per-backend concern (see each backend's DESIGN and ADRs).
**Architecture Allocation**: See DESIGN.md section "NFR Allocation"

### 6.2 NFR Exclusions

- **Real-time streaming**: Not applicable because the module does not expose SSE or WebSocket endpoints; consumers use Events Broker for change notifications.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### REST API

- [ ] `p1` - **ID**: `cpt-cf-srr-interface-rest-api`

**Type**: REST API (OpenAPI 3.0)
**Stability**: stable
**Description**: HTTP REST API for resource CRUD with OData query support on schema fields
**Breaking Change Policy**: Major version bump required for endpoint removal or incompatible request/response schema changes

#### SDK Client

- [ ] `p1` - **ID**: `cpt-cf-srr-interface-sdk-client`

**Type**: Rust trait (async)
**Stability**: stable
**Description**: `SimpleResourceRegistryClient` trait for in-process resource access via ClientHub
**Breaking Change Policy**: Major version bump for trait method signature changes

#### Storage Backend Interface

- [ ] `p1` - **ID**: `cpt-cf-srr-interface-storage-backend`

**Type**: Rust trait (async)
**Stability**: stable
**Description**: `ResourceStoragePluginClient` trait that storage backends implement. Platform vendors may provide their own implementations to integrate with existing platform storage components
**Breaking Change Policy**: Major version bump required for trait method signature changes

### 7.2 External Integration Contracts

#### Events Broker Contract

- [ ] `p2` - **ID**: `cpt-cf-srr-contract-events`

**Direction**: provided by library (emits events)
**Protocol/Format**: Internal event bus (Events Broker SDK)
**Event Schema**: Fixed schema with `id`, `type` (event type), `subject_type` (resource type), `subject_id` (resource id)
**Compatibility**: Event schema is stable; backward-compatible additions only

#### Audit Module Contract

- [ ] `p2` - **ID**: `cpt-cf-srr-contract-audit`

**Direction**: provided by library (emits audit events)
**Protocol/Format**: Internal audit bus (Audit SDK)
**Event Schema**: Fixed schema with `id`, `type` (event type), `subject_type` (resource type), `subject_id` (resource id), previous_payload (null for create), new_payload (null for delete)
**Compatibility**: Audit event schema follows platform conventions; backward-compatible additions only

## 8. Use Cases

### Reflect External Resource Entry

- [ ] `p1` - **ID**: `cpt-cf-srr-usecase-reflect-external`

**Actor**: `cpt-cf-srr-actor-consumer-module`

**Preconditions**:
- Data connector has fetched metadata about an external resource
- A derived GTS type exists for this external resource representation

**Main Flow**:
1. Data connector creates a resource entry representing the external object
2. System stores the partial representation with appropriate GTS type
3. Other modules query the registry to discover available external resources

**Postconditions**:
- External resource representation is queryable within the platform

### Query Resources with OData Filtering

- [ ] `p1` - **ID**: `cpt-cf-srr-usecase-query-odata`

**Actor**: `cpt-cf-srr-actor-api-client`, `cpt-cf-srr-actor-platform-user`

**Preconditions**:
- Resources of the target GTS type exist for the caller's tenant

**Main Flow**:
1. Consumer sends GET /simple-resource-registry/v1/resources?$filter=type eq '{type_id}'&$orderby=created_at desc&limit=20
2. System validates authentication and tenant context
3. System queries the storage plugin with OData parameters applied to schema fields
4. System returns paginated results

**Postconditions**:
- Consumer receives filtered, ordered, paginated resource list

### Store Workflow-Generated Custom Object

- [ ] `p2` - **ID**: `cpt-cf-srr-usecase-store-workflow-object`

**Actor**: `cpt-cf-srr-actor-consumer-module`

**Preconditions**:
- Workflow engine has a derived GTS resource type registered for its output objects
- Workflow execution has produced a result object

**Main Flow**:
1. Workflow engine calls POST /simple-resource-registry/v1/resources with resource `type` (GTS type ID) and payload
2. System validates authentication and tenant context
3. System assigns a system-generated UUID as the resource `id` (if not specified), sets `tenant_id` from `SecurityContext.subject_tenant_id`, sets `owner_id` from `SecurityContext.subject_id`, sets timestamps
4. System persists resource via configured storage backend
5. System returns created resource with ID

**Postconditions**:
- Resource is persisted and retrievable by ID
- If event/audit flags are enabled, corresponding events are emitted

**Alternative Flows**:
- **Invalid resource type (GTS type ID)**: System returns 400 Bad Request with details
- **Resource `type` not in caller GTS scope**: System returns 403 Forbidden
- **Per-owner resource type with missing SecurityContext.subject_id**: System returns 422 Unprocessable Entity
- **Payload validation failure**: System returns 422 Unprocessable Entity

## 9. Acceptance Criteria

- [ ] Authenticated user can create a resource with a valid resource `type` (GTS type ID), `idempotency_key`, and JSON payload
- [ ] POST without `idempotency_key` returns HTTP STATUS 400 (required field missing)
- [ ] Created resource is retrievable by ID within the same tenant
- [ ] Resources are isolated by tenant and cannot be accessed across tenant boundaries (except when explicitly shared via resource groups)
- [ ] User-scoped resources (is_per_owner_resource=true) enforce owner_id matching
- [ ] OData $filter, $orderby work correctly on schema fields with cursor-based pagination (limit, cursor)
- [ ] Soft-deleted resources are excluded from list results by default
- [ ] All CRUD operations enforce GTS type-based access control against token permissions
- [ ] GET list supports GTS wildcard filtering with trailing `*` per GTS spec
- [ ] System returns HTTP STATUS 400 when a referenced GTS type does not exist in Types Registry
- [ ] System returns HTTP STATUS 403 when the caller's token lacks permissions for POST target type or when LIST type filter has no intersection with caller scope
- [ ] System returns HTTP STATUS 404 for GET/PUT/DELETE when the target resource is filtered out by tenant/user/type backend-level security filters
- [ ] POST with a duplicate `idempotency_key` within the same tenant returns HTTP STATUS 409 with the existing resource `id`; no duplicate resource is created (`cpt-cf-srr-fr-idempotent-create`)
- [ ] Soft-deleted resources are automatically purged (hard-deleted) after the configured retention period (default 30 days)
- [ ] Event emission respects the behavioral flags on the resource's GTS type (`cpt-cf-srr-fr-notification-events`)
- [ ] Audit events are emitted according to audit flags on the resource's GTS type or any of its parent GTS types (`cpt-cf-srr-fr-audit-events`)
- [ ] Alternative storage backend can be wired without changing API or domain logic (`cpt-cf-srr-fr-multi-backend-storage`)
- [ ] Default storage backend works on PostgreSQL, MariaDB, and SQLite (`cpt-cf-srr-fr-default-storage-backend`)
- [ ] System sustains 100M total resources, 100 writes/s, 1000 reads-by-ID/s (`cpt-cf-srr-nfr-scalability`)
- [ ] Batch operations return 207 Multi-Status with per-item results (`cpt-cf-srr-fr-batch-operations`)
- [ ] Resource groups allow batch lifecycle management (`cpt-cf-srr-fr-resource-groups`)
- [ ] Search API returns 501 when target backend lacks search_support capability (`cpt-cf-srr-fr-search-api`)

## 10. Dependencies

| Dependency | Description | Needed By |
|------------|-------------|-----------|
| Types Registry | GTS type definitions for resource schemas and behavioral flags | `cpt-cf-srr-fr-gts-type-registration`, `cpt-cf-srr-fr-gts-type-validation` |
| Events Broker | Domain event emission for resource lifecycle | `cpt-cf-srr-fr-notification-events` |
| Audit Module | Audit event emission for compliance | `cpt-cf-srr-fr-audit-events` |
| gts-rust crate | GTS library for schema ID generation, validation, and wildcard matching (`GtsID`, `GtsWildcard`) | `cpt-cf-srr-fr-gts-type-registration`, `cpt-cf-srr-fr-gts-wildcard-filtering` |

## 11. Assumptions

- GTS type definitions for derived resource types are registered in Types Registry before resources of that type are created
- A default storage backend is available and configured as part of the standard CyberFabric deployment
- SecurityContext is always available for authenticated requests (enforced by API Gateway middleware)
- Payload validation against GTS schemas is performed at the application level, not at the database level

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Default storage backend may not scale beyond 100M resources | Query performance degrades as resource count grows | Per-resource-type routing (`cpt-cf-srr-fr-storage-routing`) enables dedicated storage backends for high-volume types; storage optimization is a per-backend concern |
| Uncontrolled payload sizes impact database performance | Storage costs and query latency increase | Enforce 64 KB payload limit; guide consumers toward File Storage for large data |
| GTS type definition changes after resources exist | Existing resources may not validate against updated schema | Schema evolution and validation are enforced by the Types Registry module; the registry relies on Types Registry for schema validation at creation time |
| Over-reliance on JSON payload queries despite explicit exclusion | Consumers may expect full-text or JSON path queries on payload | Clear documentation; search API (`cpt-cf-srr-fr-search-api`) with search-capable backends for search-heavy use cases |
| Background purge process misses resources under high churn | Soft-deleted or TTL-expired resources accumulate beyond retention window | Purge process runs periodically with batch-size limits; alerting on purge backlog |
| GTS type-based access control creates permission management overhead | Administrators must manage per-type permissions for each consumer | Support GTS wildcard patterns in permissions (e.g., `gts.x.srr.resource.v1~acme.*`) to grant access to type families |

## 13. Open Questions

- Expected resource counts per tenant/type (p50/p95/p99) for capacity and sizing?
- Do we need per-tenant/type quotas (count/size), and should they hard-fail, soft-fail, or be observability-only at launch?
- Is a caching layer (e.g., Redis) required for hot reads, and with what TTL and consistency model?
- What baseline and burst rate limits apply per tenant/user/client, and do some resource types need stricter write/batch limits?

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/)
- **Features**: [features/](./features/)
