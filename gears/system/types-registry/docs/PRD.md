# PRD - Types Registry

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
  - [5.1 Registry Core](#51-registry-core)
  - [5.2 References, Aliases, And Queries](#52-references-aliases-and-queries)
  - [5.3 Ownership, Lifecycle, And Caching](#53-ownership-lifecycle-and-caching)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Gear-Specific NFRs](#61-gear-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Traceability](#13-traceability)
- [14. References](#14-references)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

Types Registry is the central platform registry for type contracts used by gears to communicate, exchange typed data, discover capabilities, and extend platform functionality. It gives gears one shared authority for type identity, schema validation, derivation compatibility, type casting/conversion, P2 aliases, lifecycle, discovery, and resolving between user-facing type identifiers and machine-readable registry references.

Types Registry governs contract registration and activation metadata, while owning gears remain responsible for runtime object storage and business behavior.

### 1.2 Background / Problem Statement

The platform currently needs shared type contracts for gear contracts, configuration, plugin discovery, and typed references between domain objects. Without a central registry, each gear would need to duplicate schema management, version compatibility, type derivation compatibility checks, type casting/conversion, future Alias resolution, tenant/global ownership, lifecycle rules, and cache invalidation.

Some vendors may already have an existing type registry or contract catalog that remains the source of truth for their contracts. Types Registry must still provide one platform-facing control plane for gears, while allowing selected registry entities to be resolved and queried live through vendor Registry Source Plugins without replicating those entities into Types Registry storage.

Industry systems solve adjacent parts of this problem separately. Kubernetes CRDs, Azure Resource Providers, and AWS CloudFormation Registry cover controlled resource-type registration. Confluent Schema Registry, AWS Glue Schema Registry, Azure Event Hubs Schema Registry, and Google Pub/Sub Schemas cover schema compatibility and client lookup. Dataverse metadata covers tenant-facing metadata customization. Types Registry combines these patterns for the platform's type-contract control plane.

The canonical representation of registry contracts is based on [Global Type System](https://github.com/globaltypesystem/gts-spec) (GTS) Types, GTS Type Schemas, and registered GTS Instances.

### 1.3 Goals (Business Outcomes)

- Provide one governed registry for platform type contracts instead of bespoke per-gear type-registration mechanisms.
- Allow gears to use stable machine-readable type references while preserving user-facing GTS Identifiers and, in P2, Aliases.
- Enable safe type evolution through compatibility checks, lifecycle state, dependency awareness, and casting.
- Support global platform types and tenant-owned custom types with predictable ownership and visibility rules.
- Federate local and external registry sources behind one platform-facing registry contract.
- Make registry lookups cacheable for SDK clients without sacrificing correctness in multi-pod deployments.

### 1.4 Glossary

| Term | Definition |
|------|------------|
| GTS | Global Type System: specification for globally unique, versioned type identities and JSON Schema-based type definitions. |
| GTS Type | A type entity identified by a GTS Type Identifier and defined by a GTS Type Schema. |
| GTS Type Identifier | Canonical GTS identifier ending with `~` that identifies a GTS Type. |
| GTS Type Schema | Canonical definition of a GTS Type: a JSON Schema document annotated with GTS-specific keywords and describing instance shape, traits, and derivation. |
| GTS Instance | A concrete object, value, or document that conforms to a GTS Type. |
| GTS Instance Identifier | GTS identifier without the trailing `~`, used to identify a well-known instance. |
| GTS Identifier | Canonical user-facing identifier for a GTS Type or GTS Instance. |
| Type Schema Evolution Compatibility | Compatibility between successive revisions of the same logical GTS Type Schema identity. It determines whether a schema may evolve in place under its selected backward, forward, full, and transitive compatibility guarantees. |
| Type Derivation Compatibility | Compatibility between a derived GTS Type Schema and its base-type chain. It requires every instance valid against the derived Type Schema to remain valid against every base Type Schema in that chain. |
| Version Successor | A distinct logical GTS entity in the same version family whose concrete GTS version is higher than the entity it succeeds. It is not an internal content revision of the same logical entity. For Managed Entities, ADR-0004's major-only policy means that a Version Successor has a higher major version. |
| Registry Reference | Opaque UUID returned by the Types Registry SDK for one exact client-supplied GTS Identifier and persisted by a domain gear as its type reference. Domain gears do not derive Registry References and do not persist GTS Identifiers as type references. When P2 Aliases are introduced, an Alias GTS Identifier has its own Registry Reference. |
| Concrete Reference Set | Complete, deduplicated, bounded set of Registry Reference UUIDs selected by a type filter for use as a domain-storage query constraint. It is not a paginated result, normalized predicate, or opaque executable query plan. |
| Alias | Strictly P2 Registry-managed alternate GTS identifier that resolves only to a Managed GTS Type Schema or Managed registered GTS Instance. Every Alias is a Managed Entity; Externally Managed Aliases and Aliases targeting Externally Managed Entities are not supported. |
| Owning Gear | Gear that owns runtime storage and behavior for objects that use a registered type. |
| Validation Hook | P2 registry-governed declaration that allows an owning gear to semantically validate admission of a Managed Type Schema or registered Instance. |
| Admission Candidate | Proposed initial definition or content update undergoing validation. It is not a logical registry entity or an admitted immutable revision and is never returned by ordinary resolving or discovery. |
| Admission Status | State of an Admission Candidate or its admission operation, including `PENDING`, `ADMITTED`, `REJECTED`, or `CANCELLED`. Admission Status is separate from Lifecycle Status. |
| Registry Federation | Types Registry capability to expose one platform-facing registry contract over multiple registry sources. |
| Registry Source | Authoritative provider of registry definitions: either Types Registry's managed storage or a configured External Registry Source integrated through a Registry Source Plugin. |
| External Registry Source | Vendor or platform-integrated registry source outside Types Registry's own authoritative storage. |
| Registry Source Plugin | Governed ToolKit plugin through which Types Registry resolves and queries an External Registry Source. The plugin owns external definitions, Registry Reference mappings, revisions, caches, indexes, tombstones, and tenant state. |
| Source Claim | GTS pattern declared by a Registry Source Plugin instance to identify the non-overlapping identifier space served by that source. |
| External Revision | Opaque, source-owned freshness token for one exact Externally Managed Entity. Equal revisions identify equal canonical content and content hash. |
| Managed Entity | Registry entity for which Types Registry is the source of truth. |
| Externally Managed Entity | Registry entity whose definition, Registry Reference mapping, revisions, caches, history, and source-owned state are authoritative in an External Registry Source and obtained live through its Registry Source Plugin, while Types Registry governs platform visibility and usage semantics. |
| Tenant Subtree | A tenant and all of its descendants in the platform tenant hierarchy. |
| Lifecycle Status | Platform-level state of an admitted logical registry entity: `ACTIVE`, `DEPRECATED`, or `DELETED`. `PENDING` is an Admission Status, not a Lifecycle Status. |
| Tenant Enablement State | Tenant-level policy input for an entity: `NOT_INITIALIZED`, `ENABLED`, `TEMPORARILY_DISABLED`, or `DISABLED`. In P1 it may be source-owned for an Externally Managed Entity; post-P1 Types Registry also stores and manages it for Managed Entities. It is not the consumer-facing availability result. |
| Tenant Availability State | Computed, consumer-facing state for a concrete entity and tenant. It is derived from lifecycle status, tenant enablement state, dependencies, and external-source state when applicable; its candidate values are `AVAILABLE` or `UNAVAILABLE` with a reason. |

## 2. Actors

### 2.1 Human Actors

#### XaaS Vendor Architect

**ID**: `cpt-cf-types-registry-actor-xaas-vendor-architect`

- **Role**: Chooses how Gears are composed into a vendor product and defines derived GTS Types for existing platform and domain Constructor Fabric Gears.
- **Needs**: Governed registration and lifecycle management for product-level derived Types without forked per-gear mechanisms.

#### Gears Developer

**ID**: `cpt-cf-types-registry-actor-gears-developer`

- **Role**: Develops platform and domain Gears; defines their base GTS Types, Type Schemas, and registered Instances, and may define derived Types from Types registered by other Gears.
- **Needs**: Safe registration, compatibility checks, dependency awareness, lifecycle management, and predictable startup behavior.

#### XaaS Vendor Developer

**ID**: `cpt-cf-types-registry-actor-xaas-vendor-developer`

- **Role**: Develops vendor-specific Gears and defines their base GTS Types, Type Schemas, and registered Instances.
- **Needs**: Safe registration, compatibility checks, dependency awareness, lifecycle management, and predictable startup behavior for vendor-specific Gears.

#### Tenant Administrator

**ID**: `cpt-cf-types-registry-actor-tenant-admin`

- **Role**: Manages tenant-owned custom types and, in P2, Aliases exposed through authenticated platform APIs.
- **Needs**: Tenant-scoped type management, discovery of global and tenant-visible types, and protection from cross-tenant changes.

### 2.2 System Actors

#### Platform Gear

**ID**: `cpt-cf-types-registry-actor-platform-gear`

- **Role**: Registers platform Type Schemas and Instances during initialization and resolves registry references at runtime.

#### Domain Gear

**ID**: `cpt-cf-types-registry-actor-domain-gear`

- **Role**: Owns runtime domain objects that refer to registered types and uses Types Registry for resolving, discovery, and query assistance.

#### Registry Source Plugin

**ID**: `cpt-cf-types-registry-actor-registry-source-plugin`

- **Role**: Provides live forward/reverse resolution, querying, caching, dependency information, revision metadata, lifecycle assertions, and tenant state for an External Registry Source through a platform-governed plugin contract.

#### CI Pipeline

**ID**: `cpt-cf-types-registry-actor-ci-pipeline`

- **Role**: Validates type compatibility, dependency impact, and registry changes before deployment.

## 3. Operational Concept & Environment

Runtime, gear architecture, and project-wide quality baselines follow the repository foundations:

- [docs/ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md)
- [guidelines/README.md](../../../../guidelines/README.md)
- [docs/toolkit_unified_system/README.md](../../../../docs/toolkit_unified_system/README.md)

### 3.1 Gear-Specific Environment Constraints

Types Registry has one gear-specific operational constraint: managed registry state and Registry Source Plugin configuration must be persistent and consistent across multi-pod deployments. External registry state remains plugin-owned. Process-local state and client caches are allowed only as derived cache state.

## 4. Scope

### 4.1 In Scope

- GTS Type Schema registration, retrieval, search, lifecycle, Type Schema Evolution Compatibility checks, and Type Derivation Compatibility checks.
- GTS Instance registration, retrieval, search, lifecycle, validation, and casting.
- P2 owning-gear semantic validation hooks for initial admission and content revisions of Managed Type Schemas and registered Instances.
- Registry federation and live support for externally managed entities through ordered Registry Source Plugins, including platform-owned federation boundary enforcement, forward/reverse resolving, querying, source-owned caching, revision metadata, lifecycle assertions, dependencies, and tenant state.
- P2 Alias management and alias-aware resolving.
- Stable registry reference support for domain gears.
- Tenant/global ownership, visibility, and management boundaries.
- Lifecycle status, post-P1 tenant enablement state, and computed tenant availability state for registry entities.
- Dependency tracking for GTS and JSON Schema references.
- `gts-rust` integration for GTS parsing, validation, reference derivation, wildcard matching, compatibility, casting, and schema generation/conversion capabilities required by registry workflows.
- SDK and REST contracts for registry management, resolving, validation, casting, and discovery.
- Client-side cache correctness protocol.

### 4.2 Out of Scope

- Runtime domain-object storage and business behavior owned by other Gears, except explicitly registered well-known GTS Instances.
- Read and query policy for existing runtime domain objects whose referenced registry entity becomes unavailable; this policy is owned by the respective Domain Gear.
- Authoritative management of external registry sources that remain outside the platform's ownership boundary.
- GTS namespace governance outside registration-time validation and conflict detection.
- Full audit/history of every registry mutation beyond lifecycle and dependency state required by this PRD.
- Local projection, synchronization, indexing, revision history, or caching of Externally Managed Entity content inside Types Registry.

## 5. Functional Requirements

> **Testing strategy**: Functional requirements are verified through automated unit, integration, and end-to-end tests in accordance with the repository testing architecture, targeting 90%+ code coverage unless a requirement specifies another verification method.

Functional requirements define what Types Registry must provide. Design details such as DB tables, route paths, cache transport, and exact SDK or REST DTOs are intentionally outside this PRD and will be specified in the Types Registry DESIGN document and, where appropriate, ADRs.

### 5.1 Registry Core

#### Type Schema Management

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-register-schemas`

The system **MUST** allow authorized actors to register, retrieve, search, update lifecycle state for, and delete GTS Type Schemas, subject to validation, ownership, dependency, and compatibility rules.

- **Rationale**: Gears need one authoritative registry for type contracts.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`, `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-architect`, `cpt-cf-types-registry-actor-xaas-vendor-developer`, `cpt-cf-types-registry-actor-tenant-admin`

#### Instance Management

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-register-instances`

The system **MUST** allow authorized actors to register, retrieve, search, update lifecycle state for, and delete named GTS Instances that conform to registered Type Schemas.

- **Rationale**: Platform gears need registered well-known instances for configuration and discovery metadata.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`, `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-developer`, `cpt-cf-types-registry-actor-tenant-admin`

#### GTS Validation

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-gts-validation`

For Managed Entities and explicit platform validation operations, the system **MUST** validate GTS Identifiers, Type Schemas, Instances, references, wildcard patterns, and version semantics using the platform-approved GTS implementation. For Externally Managed Entities, this requirement applies only to the identifier and response-envelope conformance needed to enforce the federation contract; Types Registry **MUST NOT** interpret or reproduce source-owned entity validation.

- **Rationale**: Registry behavior must match the GTS specification and avoid divergent local interpretations.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`, `cpt-cf-types-registry-actor-ci-pipeline`

#### Type Schema Evolution Compatibility Checks

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-validate-schema-compat`

The system **MUST** check a proposed GTS Type Schema revision against the applicable previously admitted revisions of the same logical Type Schema identity according to its selected Type Schema Evolution Compatibility profile, and **MUST** reject a revision that violates the required backward, forward, full, or transitive guarantees.

- **Rationale**: In-place Type Schema evolution must not silently break producers, consumers, or historical payload processing.
- **Actors**: `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-architect`, `cpt-cf-types-registry-actor-xaas-vendor-developer`, `cpt-cf-types-registry-actor-ci-pipeline`

#### Type Derivation Compatibility Checks

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-validate-type-derivation`

The system **MUST** check every derived GTS Type Schema against its immediate base Type Schema and the complete transitive base-type chain. Every instance valid against the derived Type Schema **MUST** remain valid against every base Type Schema in that chain. Registration and activation **MUST** reject derivations that violate base constraints or applicable GTS derivation, finality, and inherited-trait rules.

- **Rationale**: A derived GTS Type must remain safely substitutable for every base Type declared by its GTS identifier chain, independently of compatibility between revisions of any one Type Schema.
- **Actors**: `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-architect`, `cpt-cf-types-registry-actor-xaas-vendor-developer`, `cpt-cf-types-registry-actor-ci-pipeline`

#### Dependency Awareness

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-ref-tracking`

The system **MUST** track dependencies between Managed Entities and **MUST** federate dependency and impact information supplied by Registry Source Plugins for Externally Managed Entities before deletion, incompatible changes, or lifecycle transitions that can affect dependents. A visible and tenant-available `DEPRECATED` entity **MUST** remain a valid target for both existing and newly admitted GTS and JSON Schema references; reference validation **MUST NOT** reject a target solely because it is `DEPRECATED`.

- **Rationale**: Platform teams need predictable blast-radius analysis for type changes.
- **Actors**: `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-architect`, `cpt-cf-types-registry-actor-xaas-vendor-developer`, `cpt-cf-types-registry-actor-ci-pipeline`

#### Registry Federation

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-registry-federation`

The system **MUST** support multiple Registry Sources, including Types Registry's own managed storage and External Registry Sources integrated through governed Registry Source Plugins. Types Registry **MUST NOT** persist external entity definitions, revisions, content hashes, dependencies, lifecycle state, Registry Reference mappings, query indexes, caches, or tombstones. The owning plugin **MUST** provide those capabilities live through the Types Registry federation contract.

- **Rationale**: Vendor products may already have authoritative type registries, but platform gears still need one Types Registry contract for resolving, discovery, and platform governance.
- **Actors**: `cpt-cf-types-registry-actor-xaas-vendor-architect`, `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-registry-source-plugin`

#### Registry Source Routing

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-registry-source-routing`

Each Registry Source Plugin instance **MUST** declare one or more validated Source Claims, the entity kinds it serves, and a deterministic selection priority. For every claimed entity kind, an active P1 plugin **MUST** support batch forward and reverse resolution, complete bounded candidate queries with opaque pagination, outgoing and reverse dependency-impact lookup, lifecycle and ownership/visibility assertions, tenant state, revision/hash and conditional-read semantics, retained reverse resolution after deletion, and structured source-failure outcomes.

Query and dependency results **MUST NOT** have false negatives. A plugin **MAY** return a broader candidate set for Types Registry to filter under normalized platform semantics. A plugin configuration **MUST NOT** become active for a Source Claim and entity kind when an applicable mandatory capability is absent; inability to establish a complete result at runtime **MUST** fail closed.

P1 Source Claims **MUST NOT** overlap each other or the identifier space of existing Managed Entities. Managed storage **MUST** be consulted before plugins, and plugins **MUST** be consulted in deterministic priority order.

All P1 registry entity list and search operations **MUST** fail closed if any selected Registry Source is unavailable or returns an invalid or incomplete response. P1 **MUST NOT** return a partial result page or treat a source failure as source exhaustion or authoritative absence.

- **Rationale**: Live federation requires deterministic ownership and routing without a per-external-entity index or identifier shadowing.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`, `cpt-cf-types-registry-actor-registry-source-plugin`

#### Externally Managed Entities

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-externally-managed-entities`

The system **MUST** distinguish Managed Entities from Externally Managed Entities. The External Registry Source **MUST** remain the sole authority for whether an Externally Managed Entity is valid under source-owned rules; Types Registry **MUST NOT** require, interpret, or reproduce source-owned entity validation results.

Before exposing a live external result, Types Registry **MUST** validate only federation response conformance and platform-owned invariants: identifier integrity, Registry Reference mapping, Source Claim conformance, entity kind, authorization, visibility, lifecycle mapping, availability, and cache/freshness metadata. Each external result **MUST** carry an External Revision and canonical content hash. Types Registry **MUST NOT** persist those values as registry state.

- **Rationale**: External source ownership must not bypass platform contract governance, while source-owned entity validation policies and results remain outside the Types Registry responsibility boundary.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`, `cpt-cf-types-registry-actor-domain-gear`, `cpt-cf-types-registry-actor-registry-source-plugin`

#### Owning-Gear Semantic Validation

- [ ] `p2` - **ID**: `cpt-cf-types-registry-fr-validation-hooks`

In P2, the system **MUST** invoke every matching required owning-gear Validation Hook before initial admission or admission of a new content revision for a Managed Type Schema or managed registered Instance. Admission of a higher-major Version Successor is covered as initial admission; the hook context **MUST** expose the resulting predecessor deprecation, but that deprecation **MUST NOT** trigger a separate hook.

Validation Hooks **MUST NOT** apply to Externally Managed Entities, P2 Aliases, deletion, automatic deprecation, or tenant enablement changes. Those operations remain governed by their registry, dependency, lifecycle, source, and authorization rules.

- **Rationale**: Some gear-specific type requirements cannot be validated by GTS schema rules alone; the owning gear may need to enforce domain semantics while Types Registry remains the central control-plane authority.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`, `cpt-cf-types-registry-actor-domain-gear`, `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-developer`

### 5.2 References, Aliases, And Queries

#### Alias Management

- [ ] `p2` - **ID**: `cpt-cf-types-registry-fr-aliasing`

The system **MUST** allow multiple Aliases per Managed GTS Type Schema and per Managed registered GTS Instance, and **MUST** provide management and resolving behavior for Aliases. Every Alias **MUST** be a Managed Entity for which Types Registry is the source of truth. An External Registry Source **MUST NOT** supply an Externally Managed Alias, and an Externally Managed Entity **MUST NOT** be an Alias target. Each Alias has its own globally unique GTS Identifier; no Type Schema, registered Instance, or Alias may use the same canonical identifier. Tenant ownership affects Alias visibility and management only: tenant-local Alias shadowing and resolution fallback are not supported.

- **Rationale**: Users and gears need stable alternate names without duplicating registry entities. Restricting Alias ownership and targets to Managed Entities keeps Alias identity, lifecycle, uniqueness, and target validity under one authoritative consistency boundary.
- **Actors**: `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-developer`, `cpt-cf-types-registry-actor-tenant-admin`, `cpt-cf-types-registry-actor-domain-gear`

#### Reference And Identifier Resolution

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-id-resolution`

The system **MUST** resolve between user-facing GTS Identifiers, machine-readable Registry References, entity kind, ownership scope, and lifecycle status for both single and batch lookups. For domain-owned data, the Types Registry SDK **MUST** return an opaque Registry Reference UUID for the exact client-supplied GTS Identifier. Domain gears **MUST** persist that Registry Reference rather than deriving it or persisting the GTS Identifier as the type reference. Types Registry **MUST** resolve Managed Entities locally, then delegate unresolved external references to Registry Source Plugins in deterministic priority order. A plugin-returned GTS Identifier **MUST** derive to the requested Registry Reference and match the plugin's Source Claim. When P2 Alias support is introduced, reverse resolution **MUST** preserve an exact client-supplied Alias GTS Identifier while exposing Alias target metadata separately, and Managed Aliases **MUST** resolve locally.

- **Rationale**: Domain gears need stable references for stored data and human-readable identifiers for APIs, logs, and operator workflows.
- **Actors**: `cpt-cf-types-registry-actor-domain-gear`, `cpt-cf-types-registry-actor-platform-gear`

#### Type Query Assistance

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-type-query-assistance`

The system **MUST** translate user-facing type filters, including exact GTS Identifiers, compatible versions, derivation hierarchy constraints, and GTS wildcard patterns, into a complete, deduplicated Concrete Reference Set suitable for querying gear-owned data by Registry Reference UUID. Query assistance **MUST NOT** return a normalized database predicate or opaque executable query plan. The result **MUST** be complete within a documented maximum reference count; Types Registry **MUST NOT** silently truncate or paginate it. If expansion exceeds that limit, Types Registry **MUST** return a structured `QUERY_EXPANSION_LIMIT_EXCEEDED` failure. If any source required to establish the complete set is unavailable or invalid, query assistance **MUST** fail rather than return a partial constraint.

Federated expansion **MUST** internally use source-major traversal: managed results first, followed by matching Registry Source Plugins in deterministic priority order. Internal continuation tokens **MUST** bind the query, plugin configuration revision, current source, and source cursor. Global ordering by entity fields across Registry Sources is irrelevant to the resulting set and remains outside P1.

- **Rationale**: Domain gears persist Registry Reference UUIDs and need a portable constraint that can be applied consistently across SQLite, PostgreSQL, and MySQL without executing Registry-owned predicates or query plans inside gear-owned storage.
- **Actors**: `cpt-cf-types-registry-actor-domain-gear`

### 5.3 Ownership, Lifecycle, And Caching

#### Tenant And Global Ownership

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-tenant-ownership`

The system **MUST** support platform-global registry entries and tenant-owned registry entries with explicit visibility, management, and conflict rules. Platform-global entries **MUST** be visible to every tenant, subject to lifecycle, availability, and authorization rules. A tenant-owned entry **MUST** be visible only within the Tenant Subtree rooted at its owning tenant, including the owning tenant itself, and **MUST NOT** be visible to ancestor, sibling, or unrelated tenants. Discovery, search, exact resolution, batch resolution, and query assistance **MUST** enforce the same ownership-visibility boundary and **MUST NOT** disclose the existence or metadata of an entry outside its visible scope. Visibility does not grant management authority; management remains subject to ownership and platform authorization rules.

- **Rationale**: Platform types and tenant customizations must coexist without cross-tenant leakage or accidental global mutation, while descendants can reuse contracts governed by an ancestor tenant.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`, `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-architect`, `cpt-cf-types-registry-actor-xaas-vendor-developer`, `cpt-cf-types-registry-actor-tenant-admin`

#### Lifecycle Management

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-lifecycle`

The system **MUST** manage the Lifecycle Status of admitted Managed Type Schemas and registered Instances as `ACTIVE`, `DEPRECATED`, or `DELETED`. `PENDING` **MUST** be an Admission Status of a candidate or admission operation and **MUST NOT** be exposed as the Lifecycle Status of a logical entity. Initial admission **MUST** atomically create the logical entity in `ACTIVE` with revision `1`; a failed or cancelled initial candidate **MUST NOT** create a logical entity. While an update candidate is `PENDING`, the existing entity **MUST** retain its current Lifecycle Status and current admitted revision.

`ACTIVE -> DEPRECATED` **MUST** occur only when a higher-major Version Successor in the same version family becomes `ACTIVE`. Activating that successor and deprecating the previously `ACTIVE` entity **MUST** be one atomic registry-state transition, and at most one entity in a managed version family **MUST** be `ACTIVE`. Admitting a compatible internal content revision under the same GTS Identifier **MUST NOT** deprecate the logical entity. P1 **MUST NOT** expose an independent manual deprecation or reactivation transition.

A `DEPRECATED` entity **MUST** remain ordinarily resolvable, discoverable, tenant-available under the same enablement rules as an `ACTIVE` entity, and valid for both existing and newly admitted references. Resolution and discovery **MUST** expose its Lifecycle Status so consumers can prefer the active successor without treating the deprecated entity as invalid. P2 Aliases **MUST**, when introduced, use the same logical-entity lifecycle model unless the P2 Alias decision explicitly supersedes it.

An authorized deletion operation **MUST** be permitted to transition either an `ACTIVE` or a `DEPRECATED` entity directly to terminal `DELETED`. Deletion **MUST NOT** require prior deprecation or a Version Successor, but **MUST** be rejected while a live registered dependent exists or complete dependency impact cannot be established. `DELETED` **MUST** be terminal in P1, P1 **MUST NOT** support restore, and a deleted GTS Identifier **MUST NOT** be reused for a new logical entity. Deletion **MUST** preserve identity-resolution guarantees for previously issued Registry References.

For Externally Managed Entities, Types Registry **MUST** obtain source lifecycle assertions live from the owning Registry Source Plugin and map exposed entities to the platform `ACTIVE`, `DEPRECATED`, or `DELETED` lifecycle semantics. An external `DEPRECATED` assertion **MUST** identify the Version Successor whose activation caused the transition. An external source **MAY** transition either `ACTIVE` or `DEPRECATED` directly to `DELETED`. Source-side pending candidates **MUST NOT** be exposed as logical registry entities. Resolution, reference validation, and search behavior **MUST** respect the resulting platform status.

- **Rationale**: Type evolution needs controlled activation, deprecation, and removal.
- **Actors**: `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-architect`, `cpt-cf-types-registry-actor-xaas-vendor-developer`, `cpt-cf-types-registry-actor-tenant-admin`

#### Tenant Availability Evaluation

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-tenant-availability`

The system **MUST** evaluate and expose a Tenant Availability State for a concrete registry entity and tenant. The result **MUST** be derived from Lifecycle Status, required target and dependency states, and, when applicable, authoritative tenant state and freshness from the External Registry Source. P1 has no managed tenant enablement override: visible `ACTIVE` and `DEPRECATED` managed entities are `AVAILABLE`; `DELETED` entities are unavailable for ordinary resolution. Admission Candidates are not logical entities and **MUST NOT** participate in availability evaluation. When the External Registry Source cannot confirm required tenant state, enabled-only operations **MUST** fail closed. Types Registry determines and exposes the availability result, but the handling of an existing runtime domain object whose referenced entity is unavailable is owned by that object's owning Gear. Each owning Gear defines whether its operations filter, reject, or return such an object with an explicit unavailable status.

- **Rationale**: Consumers need one authoritative usability result instead of independently combining lifecycle, tenancy, dependency, and external-source rules.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`, `cpt-cf-types-registry-actor-domain-gear`, `cpt-cf-types-registry-actor-tenant-admin`, `cpt-cf-types-registry-actor-registry-source-plugin`

#### Tenant Enablement Management

- [ ] `p2` - **ID**: `cpt-cf-types-registry-fr-tenant-enablement`

The system **MUST**, after P1, support a stored Tenant Enablement State for an entity: `NOT_INITIALIZED`, `ENABLED`, `TEMPORARILY_DISABLED`, or `DISABLED`. This state is a policy input to Tenant Availability State, not the consumer-facing result. Types Registry **MUST** allow authorized actors to manage this state for Managed Entities. For Externally Managed Entities, the External Registry Source remains authoritative for tenant enablement state.

- **Rationale**: Tenant policy must be independently controllable without conflating it with platform lifecycle or computed availability.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`, `cpt-cf-types-registry-actor-domain-gear`, `cpt-cf-types-registry-actor-tenant-admin`, `cpt-cf-types-registry-actor-registry-source-plugin`

#### Casting

- [ ] `p2` - **ID**: `cpt-cf-types-registry-fr-casting`

The system **MUST** support casting supplied instance content between compatible GTS Type Schema versions and report incompatible casts as structured failures.

- **Rationale**: Consumers need a central, consistent way to migrate or interpret versioned typed content.
- **Actors**: `cpt-cf-types-registry-actor-domain-gear`, `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-developer`

#### Client-Side Cache Correctness

- [ ] `p2` - **ID**: `cpt-cf-types-registry-fr-client-cache`

The system **MUST** define cache metadata and invalidation semantics that allow SDK clients to cache registry lookup and resolution results correctly across registry mutations. External cache validation **MUST** use the opaque revision and content hash returned by the owning Registry Source Plugin; Types Registry does not persist external cache state.

- **Rationale**: Registry lookups are common on startup and hot paths; caching must not return stale type authority.
- **Actors**: `cpt-cf-types-registry-actor-domain-gear`, `cpt-cf-types-registry-actor-platform-gear`

#### Initialization Registration

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-two-phase-init`

The system **MUST** support platform gear startup registration before the registry is fully ready by staging Admission Candidates without publishing logical entities. Types Registry **MUST** validate the complete staged startup set, including references between candidates, before atomically admitting successful initial candidates as `ACTIVE` logical entities with revision `1` and publishing ready state.

- **Rationale**: Platform gears can have interdependent type definitions that must be registered before full validation.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`

## 6. Non-Functional Requirements

> **Global baselines**: Project-wide architectural and quality baselines are defined in [docs/ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md), [guidelines/README.md](../../../../guidelines/README.md), and [ToolKit Unified System](../../../../docs/toolkit_unified_system/README.md). This section defines only Types Registry-specific NFRs.
>
> **Testing strategy**: NFRs are verified through automated benchmarks, integration tests, security checks, and monitoring as appropriate to the requirement.

### 6.1 Gear-Specific NFRs

#### Lookup Latency

- [ ] `p1` - **ID**: `cpt-cf-types-registry-nfr-lookup-latency`

The system **MUST** resolve an exact Managed Entity registry reference or GTS Identifier lookup within 10ms at p95 under normal production load. For an Externally Managed Entity, Types Registry federation and policy-processing overhead **MUST** remain within the same budget, excluding Registry Source Plugin and External Registry Source execution time; the end-to-end target is governed by the source capability contract.

- **Threshold**: p95 < 10ms for a managed exact lookup and p95 < 10ms Types Registry overhead for an external exact lookup, measured separately from source execution.
- **Rationale**: Registry resolving is used by gear startup and runtime paths.

#### Query Latency

- [ ] `p2` - **ID**: `cpt-cf-types-registry-nfr-query-latency`

The system **MUST** return common filtered Managed Entity searches within 100ms at p95 under normal production load. For a federated search, Types Registry processing overhead **MUST** remain within the same budget, excluding Registry Source Plugin and External Registry Source execution time; the end-to-end target is governed by the participating source capability contracts.

- **Threshold**: p95 < 100ms for bounded managed search results and p95 < 100ms Types Registry overhead for a bounded federated search, measured separately from source execution.
- **Rationale**: Discovery and management views must remain responsive.

#### Multi-Pod Correctness

- [ ] `p1` - **ID**: `cpt-cf-types-registry-nfr-multi-pod-correctness`

The system **MUST** make every committed Managed Entity or Registry Source Plugin configuration mutation visible to every Types Registry pod after transaction commit. External entity consistency across plugin instances, pods, and data centers is governed by the Registry Source Plugin capability contract.

- **Threshold**: 100% of committed mutations are visible on every pod's first post-commit read.
- **Rationale**: Production deployments are horizontally scaled.

#### Cache Correctness

- [ ] `p2` - **ID**: `cpt-cf-types-registry-nfr-cache-correctness`

The system **MUST** prevent SDK clients from treating invalidated registry lookup results as current after a relevant registry mutation is observed.

- **Threshold**: Zero stale registry results are accepted as current after the relevant mutation is observed by the client.
- **Rationale**: Client-side caching is required but cannot weaken type authority.
- **Verification Method**: Integration tests cover mutation, cache validation, and stale-entry rejection.

### 6.2 NFR Exclusions

- None identified.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### SDK Contract

- [ ] `p1` - **ID**: `cpt-cf-types-registry-interface-sdk`

- **Type**: Rust SDK trait and models.
- **Stability**: unstable until first platform-stable release.
- **Description**: In-process and remote-client contract for gear-to-gear registration, resolving, discovery, compatibility, and externally managed entity access.
- **Breaking Change Policy**: Breaking changes allowed before first stable release; afterwards require versioned contract.

#### REST API

- [ ] `p1` - **ID**: `cpt-cf-types-registry-interface-rest`

- **Type**: Authenticated REST API.
- **Stability**: unstable until first platform-stable release.
- **Description**: External and tenant-facing contract for management, discovery, resolving, validation, and externally managed entity visibility.
- **Breaking Change Policy**: Breaking changes allowed before first stable release; afterwards require versioned API.

### 7.2 External Integration Contracts

#### GTS Implementation

- [ ] `p1` - **ID**: `cpt-cf-types-registry-contract-gts-rust`

- **Direction**: required by Types Registry.
- **Protocol/Format**: Rust library API.
- **Compatibility**: Types Registry relies on the approved GTS implementation for parsing, normalization, reference derivation, wildcard matching, validation, compatibility, and casting semantics.

#### Platform AuthN/AuthZ

- [ ] `p1` - **ID**: `cpt-cf-types-registry-contract-platform-auth`

- **Direction**: required by Types Registry.
- **Protocol/Format**: ToolKit SecurityContext, PolicyEnforcer, and platform authentication/authorization contracts.
- **Compatibility**: Tenant/global ownership checks must follow platform-level AuthN/AuthZ rules.

#### ToolKit Plugin Architecture

- [ ] `p1` - **ID**: `cpt-cf-types-registry-contract-toolkit-plugins`

- **Direction**: required by Types Registry for external registry source integration.
- **Protocol/Format**: ToolKit plugin and scoped ClientHub contracts.
- **Compatibility**: External Registry Sources must be integrated behind Types Registry rather than consumed directly by regular gears. For each claimed entity kind, Registry Source Plugins must satisfy the mandatory P1 capability and completeness profile defined by Registry Source Routing; concrete plugin traits and transport models are versioned SDK design.

## 8. Use Cases

#### Register A GTS Type Schema

- [ ] `p1` - **ID**: `cpt-cf-types-registry-usecase-register-type-schema`

**Actor**: `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-architect`, or `cpt-cf-types-registry-actor-xaas-vendor-developer`

**Preconditions**:
- A GTS Type Schema is available for registration.

**Main Flow**:
1. Actor registers the GTS Type Schema.
2. Types Registry creates an Admission Candidate and validates identity, ownership, compatibility, lifecycle, and conflicts.
3. On successful admission, Types Registry atomically creates the logical Type Schema in `ACTIVE` with revision `1`.
4. Owning gears can discover the Type Schema, resolve it for their tenant, and use its registry reference in their own data.

**Postconditions**:
- The Type Schema is discoverable and governed by Types Registry.

#### Resolve A User-Facing Type Filter For Gear-Owned Data

- [ ] `p1` - **ID**: `cpt-cf-types-registry-usecase-resolve-type-filter`

**Actor**: `cpt-cf-types-registry-actor-domain-gear`

**Preconditions**:
- The gear owns runtime objects that reference registry entities.
- A caller supplies a GTS Identifier, compatible-version expression, or wildcard pattern.

**Main Flow**:
1. Gear asks Types Registry to resolve the user-facing type filter.
2. Types Registry applies ownership, lifecycle, version, and wildcard rules.
3. Gear receives a complete, bounded Concrete Reference Set and applies it to its own storage using backend-safe UUID-set filtering.

**Postconditions**:
- The gear returns domain objects by matching their stored Registry Reference UUIDs against the complete set selected by Types Registry.

#### Use An Externally Managed Entity

- [ ] `p1` - **ID**: `cpt-cf-types-registry-usecase-use-externally-managed-entity`

**Actor**: `cpt-cf-types-registry-actor-domain-gear`

**Preconditions**:
- An External Registry Source is available through a governed Registry Source Plugin.
- The external source provides a registry entity that is visible to the platform.

**Main Flow**:
1. Types Registry checks managed storage and selects the owning Registry Source Plugin using the ordered Source Claim model.
2. The plugin resolves or queries the externally managed entity live and returns canonical content, opaque revision, content hash, source lifecycle and ownership/visibility assertions, and authoritative tenant state when required.
3. Types Registry validates federation response conformance, the Registry Reference, and the Source Claim, then applies platform-owned authorization, visibility, lifecycle mapping, availability, and cache/freshness rules.
4. The domain gear resolves or discovers the entity through the normal Types Registry SDK or REST contract.

**Postconditions**:
- The domain gear uses the entity through Types Registry without directly depending on the External Registry Source.

#### Validate A Type Evolution Before Deployment

- [ ] `p2` - **ID**: `cpt-cf-types-registry-usecase-validate-type-evolution`

**Actor**: `cpt-cf-types-registry-actor-ci-pipeline`

**Preconditions**:
- A Type Schema change is proposed.

**Main Flow**:
1. CI checks the proposed Type Schema against existing registered state.
2. Types Registry reports compatibility, dependency impact, and lifecycle conflicts.
3. CI accepts or blocks the deployment based on registry results.

**Postconditions**:
- Incompatible or unsafe type changes are detected before rollout.

## 9. Acceptance Criteria

- [ ] Platform gears can stage GTS Type Schema and registered Instance Admission Candidates during startup; no logical entity becomes ordinarily visible until validation succeeds and the candidate is admitted as `ACTIVE`.
- [ ] A pending, rejected, or cancelled initial candidate is never returned as a logical entity and consumes no admitted revision number; a pending or failed update leaves the existing entity Lifecycle Status and current revision unchanged.
- [ ] A new platform GTS Type Schema can be introduced through Types Registry without each owning gear maintaining its own type registry.
- [ ] In P2, a matching required owning-gear Validation Hook can reject initial admission or a content revision of a Managed Type Schema or registered Instance, while aliases, external entities, deletion, automatic deprecation, and tenant enablement do not invoke hooks.
- [ ] An externally managed entity can be discovered and resolved through Types Registry without direct dependency on its External Registry Source.
- [ ] Types Registry persists no Externally Managed Entity content or metadata projection; the owning plugin supplies forward/reverse resolution, querying, revisions, hashes, dependencies, tombstones, lifecycle assertions, caches, and tenant state.
- [ ] Managed storage is resolved first, non-overlapping Source Claims select external plugins, and unresolved Registry References are delegated in deterministic priority order.
- [ ] A Registry Source Plugin cannot activate a Source Claim for an entity kind unless it implements the complete P1 resolution, query, dependency, state, freshness, retention, and failure contract; query and dependency results contain no false negatives.
- [ ] Federated wildcard pages use deterministic source-major ordering and opaque cursors that become stale when plugin routing configuration changes.
- [ ] A P1 registry entity list or search operation returns a source failure and no result page when any selected Registry Source is unavailable or returns an invalid or incomplete response.
- [ ] Type query assistance returns a complete, deduplicated Concrete Reference Set; it never returns a partial or paginated constraint and reports a structured limit error when expansion is too broad.
- [ ] Tenant-specific availability decisions respect lifecycle status, managed enablement policy when introduced, and authoritative external tenant state.
- [ ] Activating a managed higher-major Version Successor atomically marks the previously active member of that version family `DEPRECATED`; an internal content revision does not, and no independent P1 deprecation operation exists.
- [ ] A visible and tenant-available `DEPRECATED` entity remains resolvable, discoverable, and valid for both existing and newly admitted references.
- [ ] Both `ACTIVE` and `DEPRECATED` entities can transition directly to terminal `DELETED` only when no live registered dependent exists and complete dependency impact is known; P1 has no restore and never reuses the GTS Identifier for a new logical entity.
- [ ] Domain gears can use stable registry references and resolve user-facing GTS Identifiers, compatible-version filters, and wildcard patterns through Types Registry; P2 adds Alias-aware resolving without changing the P1 reference contract.
- [ ] A tenant-owned entry can be discovered, resolved, and used by its owning tenant and descendant tenants, is not disclosed to tenants outside that Tenant Subtree, and can reference visible global entries.
- [ ] Type Schema Evolution Compatibility, Type Derivation Compatibility, dependency, lifecycle, and cache invalidation behavior is testable through SDK and REST contracts.

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| [GTS specification](https://github.com/globaltypesystem/gts-spec) | Defines canonical GTS identity, type/instance terminology, validation, derivation, compatibility, and reference semantics | `p1` |
| gts-rust | Platform-approved implementation of GTS parsing, validation, compatibility, reference derivation, wildcard, casting, and schema generation/conversion behavior | `p1` |
| ToolKit SDK/ClientHub | Gear-to-gear contract and client registration mechanism | `p1` |
| ToolKit plugin architecture | Plugin isolation and scoped client pattern for Registry Source Plugins | `p1` |
| Platform AuthN/AuthZ | Tenant/global access control and SecurityContext propagation | `p1` |
| Persistent platform database | Authoritative Managed Entity and Registry Source Plugin configuration state for multi-pod deployments | `p1` |

## 11. Assumptions

- GTS remains the canonical platform type identity model.
- Runtime domain objects remain owned by their domain gears, not by Types Registry.
- Gears use Types Registry for resolving and query assistance. Domain gears persist the opaque Registry Reference UUID returned by the Types Registry SDK for the exact client-supplied GTS Identifier; they do not derive the reference or persist the GTS Identifier as the type reference, as defined by ADR-0001.
- External Registry Sources remain authoritative for externally managed entities. Their plugins own external definitions, Registry Reference mappings, revisions, queries, dependencies, caches, tombstones, lifecycle assertions, and tenant state, while regular gears access them only through Types Registry.
- Industry analogues are used as design inputs by pattern, not as direct product copies.

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Types Registry scope expands into a universal object store | Ownership confusion and excessive complexity | Keep runtime object storage and business behavior explicitly out of scope |
| P2 Alias and wildcard expansion semantics are underspecified | Inconsistent query and cache behavior across gears | Define literal-versus-target Alias matching and compatibility/hierarchy expansion rules before P2 implementation |
| Cache protocol is too weak for multi-pod deployments | Stale type resolution in long-running clients | Make cache correctness a first-class requirement and integration-test mutation scenarios |
| Gear-specific semantic validation is underspecified | Types unsuitable for a gear's domain can be activated | Define hook binding, execution, AuthN, timeout, and failure policy before implementation |
| Semantic validation hooks become an execution framework | Security, latency, and ownership complexity | Keep hooks as governed validation contracts owned by gears; define execution, AuthN, timeout, and failure policy before implementation |
| External sources bypass platform governance | Inconsistent contracts, resolving, or visibility across gears | Require every external result to pass platform-owned federation boundary checks before use by gears |
| A Registry Source Plugin serves stale tenant state from its internal cache | Tenants may see entities as available after the source changes lifecycle or tenant enablement | Require live plugin lookup at decision time and make any plugin-internal cache subject to explicit source invalidation and conformance guarantees |
| A Registry Source Plugin is unavailable or returns incomplete data | Exact resolution or list/search results may be mistaken for authoritative absence | Distinguish `NOT_FOUND` from source failure and fail closed for all P1 registry operations that require the source |
| Plugin Source Claims overlap | Priority silently becomes identifier shadowing and results vary by source order | Reject overlapping Source Claims and Managed Entity conflicts in P1 |
| Federated pagination is unstable across plugin changes | Clients see duplicates, gaps, or inconsistent source ordering | Use source-major ordering and bind opaque cursors to a plugin configuration revision |

## 13. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/)

## 14. References

- **GTS spec**: [Global Type System](https://github.com/globaltypesystem/gts-spec)
- **ToolKit**: [docs/toolkit_unified_system/README.md](../../../../docs/toolkit_unified_system/README.md)
- **ToolKit plugins**: [docs/TOOLKIT_PLUGINS.md](../../../../docs/TOOLKIT_PLUGINS.md)
