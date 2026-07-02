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
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)
- [15. References](#15-references)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

Types Registry is the central platform registry for type contracts used by gears to communicate, exchange typed data, discover capabilities, and extend platform functionality. It gives gears one shared authority for type identity, schema validation, derivation compatibility, type casting/conversion, aliases, lifecycle, discovery, and resolving between user-facing type identifiers and machine-readable registry references.

Types Registry governs contract registration and activation metadata, while owning gears remain responsible for runtime object storage and business behavior.

### 1.2 Background / Problem Statement

The platform currently needs shared type contracts for gear contracts, configuration, plugin discovery, and typed references between domain objects. Without a central registry, each gear would need to duplicate schema management, version compatibility, type derivation compatibility checks, type casting/conversion, alias resolution, tenant/global ownership, lifecycle rules, and cache invalidation.

Some vendors may already have an existing type registry or contract catalog that remains the source of truth for their contracts. Types Registry must still provide one platform-facing control plane for gears, while allowing selected registry entities to be externally managed by vendor registry sources.

Industry systems solve adjacent parts of this problem separately. Kubernetes CRDs, Azure Resource Providers, and AWS CloudFormation Registry cover controlled resource-type registration. Confluent Schema Registry, AWS Glue Schema Registry, Azure Event Hubs Schema Registry, and Google Pub/Sub Schemas cover schema compatibility and client lookup. Dataverse metadata covers tenant-facing metadata customization. Types Registry combines these patterns for the platform's type-contract control plane.

The canonical representation of registry contracts is based on [Global Type System](https://github.com/globaltypesystem/gts-spec) (GTS) Types, GTS Type Schemas, and registered GTS Instances.

### 1.3 Goals (Business Outcomes)

- Provide one governed registry for platform type contracts instead of bespoke per-gear type-registration mechanisms.
- Allow gears to use stable machine-readable type references while preserving user-facing GTS Identifiers and aliases.
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
| Registry Reference | Opaque UUID returned by the Types Registry SDK for one exact client-supplied GTS Identifier, including an Alias GTS Identifier, and persisted by a domain gear as its type reference. Domain gears do not derive Registry References and do not persist GTS Identifiers as type references. |
| Alias | Registry-managed alternate GTS identifier that resolves only to a Managed GTS Type Schema or Managed registered GTS Instance. Every Alias is a Managed Entity; Externally Managed Aliases and Aliases targeting Externally Managed Entities are not supported. |
| Owning Gear | Gear that owns runtime storage and behavior for objects that use a registered type. |
| Validation Hook | Registry-governed declaration that allows an owning gear to semantically validate a registry entity before it becomes active. |
| Registry Federation | Types Registry capability to expose one platform-facing registry contract over multiple registry sources. |
| Registry Source | Authoritative provider of registry definitions: either Types Registry's managed storage or a configured External Registry Source integrated through a Registry Source Plugin. |
| External Registry Source | Vendor or platform-integrated registry source outside Types Registry's own authoritative storage. |
| Managed Entity | Registry entity for which Types Registry is the source of truth. |
| Externally Managed Entity | Registry entity whose definition and source-owned state are authoritative in an External Registry Source, while Types Registry governs platform visibility and usage semantics. |
| Tenant Subtree | A tenant and all of its descendants in the platform tenant hierarchy. |
| Lifecycle Status | Platform-level state of a core registry entity: `PENDING`, `ACTIVE`, `DEPRECATED`, or `DELETED`. It determines whether the entity is admitted for use at all. |
| Tenant Enablement State | Stored, post-P1 tenant-level policy input for an entity: `NOT_INITIALIZED`, `ENABLED`, `TEMPORARILY_DISABLED`, or `DISABLED`. It is not the consumer-facing availability result. |
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

- **Role**: Manages tenant-owned custom types and aliases exposed through authenticated platform APIs.
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

- **Role**: Provides access to an External Registry Source through a platform-governed plugin contract.

#### CI Pipeline

**ID**: `cpt-cf-types-registry-actor-ci-pipeline`

- **Role**: Validates type compatibility, dependency impact, and registry changes before deployment.

## 3. Operational Concept & Environment

Runtime, gear architecture, and project-wide quality baselines follow the repository foundations:

- [docs/ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md)
- [guidelines/README.md](../../../../guidelines/README.md)
- [docs/toolkit_unified_system/README.md](../../../../docs/toolkit_unified_system/README.md)

### 3.1 Gear-Specific Environment Constraints

Types Registry has one gear-specific operational constraint: registry state must be persistent and consistent across multi-pod deployments. Process-local state and client caches are allowed only as derived cache state.

## 4. Scope

### 4.1 In Scope

- GTS Type Schema registration, retrieval, search, lifecycle, Type Schema Evolution Compatibility checks, and Type Derivation Compatibility checks.
- GTS Instance registration, retrieval, search, lifecycle, validation, and casting.
- Owning-gear semantic validation hooks for registration and lifecycle activation.
- Registry federation and support for externally managed entities through External Registry Sources, including platform admission, resolving, and source-owned tenant state.
- Alias management and alias-aware resolving.
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

## 5. Functional Requirements

> **Testing strategy**: Functional requirements are verified through automated unit, integration, and end-to-end tests in accordance with the repository testing architecture, targeting 90%+ code coverage unless a requirement specifies another verification method.

Functional requirements define what Types Registry must provide. Design details such as DB tables, route paths, cache transport, and query planner representation are intentionally outside this PRD and will be specified in the Types Registry DESIGN document and, where appropriate, ADRs.

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

The system **MUST** validate GTS Identifiers, Type Schemas, Instances, references, wildcard patterns, and version semantics using the platform-approved GTS implementation.

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

The system **MUST** track dependencies between registered entities and expose impact information before deletion, incompatible changes, or lifecycle transitions that can affect dependents.

- **Rationale**: Platform teams need predictable blast-radius analysis for type changes.
- **Actors**: `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-architect`, `cpt-cf-types-registry-actor-xaas-vendor-developer`, `cpt-cf-types-registry-actor-ci-pipeline`

#### Registry Federation

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-registry-federation`

The system **MUST** support multiple Registry Sources, including Types Registry's own managed storage and External Registry Sources integrated through governed platform contracts.

- **Rationale**: Vendor products may already have authoritative type registries, but platform gears still need one Types Registry contract for resolving, discovery, validation, and platform governance.
- **Actors**: `cpt-cf-types-registry-actor-xaas-vendor-architect`, `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-registry-source-plugin`

#### Externally Managed Entities

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-externally-managed-entities`

The system **MUST** distinguish Managed Entities from Externally Managed Entities, **MUST** require source-owned validation from External Registry Sources, and **MUST** apply platform admission rules for visibility, lifecycle exposure, resolving, and cache/freshness before externally managed entities become usable by platform gears.

- **Rationale**: External source ownership must not bypass platform contract governance, but Types Registry should not pretend to be the source of truth for externally managed definitions or source-owned validation.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`, `cpt-cf-types-registry-actor-domain-gear`, `cpt-cf-types-registry-actor-registry-source-plugin`

#### Owning-Gear Semantic Validation

- [ ] `p2` - **ID**: `cpt-cf-types-registry-fr-validation-hooks`

The system **MUST** support registry-governed validation hooks that allow owning gears to accept or reject registration and lifecycle activation of registry entities based on domain-specific semantic rules before those entities become active.

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

The system **MUST** resolve between user-facing GTS Identifiers, machine-readable Registry References, entity kind, ownership scope, and lifecycle status for both single and batch lookups. For domain-owned data, the Types Registry SDK **MUST** return an opaque Registry Reference UUID for the exact client-supplied GTS Identifier. Domain gears **MUST** persist that Registry Reference rather than deriving it or persisting the GTS Identifier as the type reference. Reverse resolution **MUST** preserve the exact client-supplied identifier, including an Alias GTS Identifier, while exposing Alias target metadata separately when applicable.

- **Rationale**: Domain gears need stable references for stored data and human-readable identifiers for APIs, logs, and operator workflows.
- **Actors**: `cpt-cf-types-registry-actor-domain-gear`, `cpt-cf-types-registry-actor-platform-gear`

#### Type Query Assistance

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-type-query-assistance`

The system **MUST** help domain gears translate user-facing type filters, including compatible versions and GTS wildcard patterns, into registry-aware query constraints suitable for gear-owned data.

- **Rationale**: Query behavior must be consistent even though runtime objects are stored outside Types Registry.
- **Actors**: `cpt-cf-types-registry-actor-domain-gear`

### 5.3 Ownership, Lifecycle, And Caching

#### Tenant And Global Ownership

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-tenant-ownership`

The system **MUST** support platform-global registry entries and tenant-owned registry entries with explicit visibility, management, and conflict rules. Platform-global entries **MUST** be visible to every tenant, subject to lifecycle, availability, and authorization rules. A tenant-owned entry **MUST** be visible only within the Tenant Subtree rooted at its owning tenant, including the owning tenant itself, and **MUST NOT** be visible to ancestor, sibling, or unrelated tenants. Discovery, search, exact resolution, batch resolution, and query assistance **MUST** enforce the same ownership-visibility boundary and **MUST NOT** disclose the existence or metadata of an entry outside its visible scope. Visibility does not grant management authority; management remains subject to ownership and platform authorization rules.

- **Rationale**: Platform types and tenant customizations must coexist without cross-tenant leakage or accidental global mutation, while descendants can reuse contracts governed by an ancestor tenant.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`, `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-architect`, `cpt-cf-types-registry-actor-xaas-vendor-developer`, `cpt-cf-types-registry-actor-tenant-admin`

#### Lifecycle Management

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-lifecycle`

The system **MUST** manage and expose the Lifecycle Status of Type Schemas, registered Instances, and Aliases: `PENDING`, `ACTIVE`, `DEPRECATED`, or `DELETED`. Resolution and search behavior **MUST** respect that status.

- **Rationale**: Type evolution needs controlled activation, deprecation, and removal.
- **Actors**: `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-architect`, `cpt-cf-types-registry-actor-xaas-vendor-developer`, `cpt-cf-types-registry-actor-tenant-admin`

#### Tenant Availability Evaluation

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-tenant-availability`

The system **MUST** evaluate and expose a Tenant Availability State for a concrete registry entity and tenant. The result **MUST** be derived from Lifecycle Status, required target and dependency states, and, when applicable, authoritative tenant state and freshness from the External Registry Source. P1 has no managed tenant enablement override: visible `ACTIVE` and `DEPRECATED` managed entities are `AVAILABLE`; `PENDING` and `DELETED` entities are unavailable for ordinary resolution. When the External Registry Source cannot confirm required tenant state, enabled-only operations **MUST** fail closed. Types Registry determines and exposes the availability result, but the handling of an existing runtime domain object whose referenced entity is unavailable is owned by that object's owning Gear. Each owning Gear defines whether its operations filter, reject, or return such an object with an explicit unavailable status.

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

The system **MUST** define cache metadata and invalidation semantics that allow SDK clients to cache registry lookup and resolution results correctly across registry mutations.

- **Rationale**: Registry lookups are common on startup and hot paths; caching must not return stale type authority.
- **Actors**: `cpt-cf-types-registry-actor-domain-gear`, `cpt-cf-types-registry-actor-platform-gear`

#### Initialization Registration

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-two-phase-init`

The system **MUST** support platform gear startup registration before the registry is fully ready, and **MUST** validate the complete startup registry state before publishing ready state.

- **Rationale**: Platform gears can have interdependent type definitions that must be registered before full validation.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`

## 6. Non-Functional Requirements

> **Global baselines**: Project-wide architectural and quality baselines are defined in [docs/ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md), [guidelines/README.md](../../../../guidelines/README.md), and [ToolKit Unified System](../../../../docs/toolkit_unified_system/README.md). This section defines only Types Registry-specific NFRs.
>
> **Testing strategy**: NFRs are verified through automated benchmarks, integration tests, security checks, and monitoring as appropriate to the requirement.

### 6.1 Gear-Specific NFRs

#### Lookup Latency

- [ ] `p1` - **ID**: `cpt-cf-types-registry-nfr-lookup-latency`

The system **MUST** resolve an exact registry reference or GTS Identifier lookup within 10ms at p95 under normal production load.

- **Threshold**: p95 < 10ms for exact lookup.
- **Rationale**: Registry resolving is used by gear startup and runtime paths.

#### Query Latency

- [ ] `p2` - **ID**: `cpt-cf-types-registry-nfr-query-latency`

The system **MUST** return common filtered registry searches within 100ms at p95 under normal production load.

- **Threshold**: p95 < 100ms for bounded registry search results.
- **Rationale**: Discovery and management views must remain responsive.

#### Multi-Pod Correctness

- [ ] `p1` - **ID**: `cpt-cf-types-registry-nfr-multi-pod-correctness`

The system **MUST** make every committed registry mutation visible to every Types Registry pod after transaction commit.

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
- **Compatibility**: External Registry Sources must be integrated behind Types Registry rather than consumed directly by regular gears.

## 8. Use Cases

#### Register A GTS Type Schema

- [ ] `p1` - **ID**: `cpt-cf-types-registry-usecase-register-type-schema`

**Actor**: `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-architect`, or `cpt-cf-types-registry-actor-xaas-vendor-developer`

**Preconditions**:
- A GTS Type Schema is available for registration.

**Main Flow**:
1. Actor registers the GTS Type Schema.
2. Types Registry validates identity, ownership, compatibility, lifecycle, and conflicts.
3. Owning gears can discover the Type Schema, resolve it for their tenant, and use its registry reference in their own data.

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
3. Gear receives registry-aware query constraints and applies them to its own storage.

**Postconditions**:
- The gear returns domain objects using consistent registry semantics without depending on one specific registry-reference storage representation.

#### Use An Externally Managed Entity

- [ ] `p1` - **ID**: `cpt-cf-types-registry-usecase-use-externally-managed-entity`

**Actor**: `cpt-cf-types-registry-actor-domain-gear`

**Preconditions**:
- An External Registry Source is available through a governed Registry Source Plugin.
- The external source provides a registry entity that is visible to the platform.

**Main Flow**:
1. Types Registry obtains the externally managed entity definition from the External Registry Source.
2. Types Registry requires source-owned validation from the External Registry Source and applies platform admission rules for visibility, lifecycle exposure, and cache/freshness.
3. Types Registry obtains the authoritative tenant state from the External Registry Source when the caller needs tenant-specific availability.
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

- [ ] Platform gears can register GTS Type Schemas and registered Instances during startup and reach ready state only after validation succeeds.
- [ ] A new platform GTS Type Schema can be introduced through Types Registry without each owning gear maintaining its own type registry.
- [ ] An owning gear can block activation of a Type Schema that violates its domain semantics through a registry-governed semantic validation hook.
- [ ] An externally managed entity can be discovered and resolved through Types Registry without direct dependency on its External Registry Source.
- [ ] Tenant-specific availability decisions respect lifecycle status, managed enablement policy when introduced, and authoritative external tenant state.
- [ ] Domain gears can use stable registry references and resolve user-facing GTS Identifiers, aliases, compatible-version filters, and wildcard patterns through Types Registry.
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
| Persistent platform database | Authoritative registry state for multi-pod deployments | `p1` |

## 11. Assumptions

- GTS remains the canonical platform type identity model.
- Runtime domain objects remain owned by their domain gears, not by Types Registry.
- Gears use Types Registry for resolving and query assistance. Domain gears persist the opaque Registry Reference UUID returned by the Types Registry SDK for the exact client-supplied GTS Identifier; they do not derive the reference or persist the GTS Identifier as the type reference, as defined by ADR-0001.
- External Registry Sources may remain authoritative for externally managed entities, but platform gears access those entities through Types Registry.
- Industry analogues are used as design inputs by pattern, not as direct product copies.

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Types Registry scope expands into a universal object store | Ownership confusion and excessive complexity | Keep runtime object storage and business behavior explicitly out of scope |
| Alias and wildcard semantics are underspecified | Inconsistent query and cache behavior across gears | Capture alias and query-planning rules before implementation |
| Cache protocol is too weak for multi-pod deployments | Stale type resolution in long-running clients | Make cache correctness a first-class requirement and integration-test mutation scenarios |
| Gear-specific semantic validation is underspecified | Types unsuitable for a gear's domain can be activated | Define hook binding, execution, AuthN, timeout, and failure policy before implementation |
| Semantic validation hooks become an execution framework | Security, latency, and ownership complexity | Keep hooks as governed validation contracts owned by gears; define execution, AuthN, timeout, and failure policy before implementation |
| External sources bypass platform governance | Inconsistent contracts, resolving, or visibility across gears | Require externally managed entities to pass platform admission before use by gears |
| Externally managed tenant state is cached without strong invalidation | Tenants may see entities as available after the source changes lifecycle or tenant enablement | Keep external tenant state source-owned and define live lookup or explicit invalidation guarantees before implementation |

## 13. Open Questions

- What form should type query assistance return to domain gears: concrete reference set, normalized predicate, or opaque query plan?
- What lifecycle transition, deprecation, and deletion policies are required for the first release?
- Which registry entity kinds and lifecycle transitions require owning-gear semantic validation in the first release?
- Which external registry source capabilities are required in the first release: read-only discovery, synchronization, validation, or delegated writes?

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/)

## 15. References

- **GTS spec**: [Global Type System](https://github.com/globaltypesystem/gts-spec)
- **ToolKit**: [docs/toolkit_unified_system/README.md](../../../../docs/toolkit_unified_system/README.md)
- **ToolKit plugins**: [docs/TOOLKIT_PLUGINS.md](../../../../docs/TOOLKIT_PLUGINS.md)
