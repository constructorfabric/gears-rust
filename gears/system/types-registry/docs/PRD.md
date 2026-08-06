# PRD - Types Registry

## Table of Contents

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

Types Registry is the central platform registry for type contracts used by gears to communicate, exchange typed data, discover capabilities, and extend platform functionality. It gives gears one shared authority for type identity, schema validation, derivation compatibility, lifecycle, discovery, resolving between user-facing type identifiers and machine-readable registry references, and — from P2 — type casting/conversion and Aliases.

Types Registry governs contract registration and activation metadata, while owning gears remain responsible for runtime object storage and business behavior.

### 1.2 Background / Problem Statement

The platform currently needs shared type contracts for gear contracts, configuration, plugin discovery, and typed references between domain objects. Without a central registry, each gear would need to duplicate schema management, version compatibility, type derivation compatibility checks, type casting/conversion, future Alias resolution, tenant/global ownership, lifecycle rules, and cache invalidation.

Some vendors may already have an existing type registry or contract catalog that remains the source of truth for their contracts. Types Registry must still provide one platform-facing control plane for gears, while allowing selected registry entities to be resolved and queried live through vendor Registry Source Plugins without replicating those entities into Types Registry storage.

Industry systems solve adjacent parts of this problem separately. Kubernetes CRDs, Azure Resource Providers, and AWS CloudFormation Registry cover controlled resource-type registration. Confluent Schema Registry, AWS Glue Schema Registry, Azure Event Hubs Schema Registry, and Google Pub/Sub Schemas cover schema compatibility and client lookup. Dataverse metadata covers tenant-facing metadata customization. Types Registry combines these patterns for the platform's type-contract control plane.

The canonical representation of registry contracts is based on [Global Type System](https://github.com/globaltypesystem/gts-spec) (GTS) Types, GTS Type Schemas, and registered GTS Instances.

### 1.3 Goals (Business Outcomes)

- Provide one governed registry for platform type contracts instead of bespoke per-gear type-registration mechanisms.
- Allow gears to use stable machine-readable type references while preserving user-facing GTS Identifiers and, in P2, Aliases.
- Enable safe type evolution through compatibility checks, lifecycle state, dependency awareness, and P2 casting.
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
| JSON Schema Dialect | The JSON Schema draft a Type Schema declares through its top-level `$schema` URI. GTS is dialect-agnostic, and every compatibility relation is defined relative to the declaring document's dialect. |
| Resolution Closure | The set of documents inlined to produce a Type Schema's effective form: every base in its `$id` chain and every `$ref` target reachable from its content, including targets referenced from inside `x-gts-traits-schema`. It is distinct from the availability-blocking dependency closure, which additionally contains `x-gts-ref` targets that are never inlined. |
| GTS Instance | A concrete object, value, or document that conforms to a GTS Type. |
| GTS Instance Identifier | GTS identifier without the trailing `~`, used to identify a well-known instance. |
| GTS Identifier | Canonical user-facing identifier for a GTS Type or GTS Instance. |
| Type Schema Evolution Compatibility | Compatibility between successive revisions of the same logical GTS Type Schema identity, defined by the GTS specification as inclusion of accepted-instance sets. It determines whether a schema may evolve in place under the platform-enforced mode. It is distinct from Type Derivation Compatibility and is never qualified as "fully compatible" with a base type. |
| Type Derivation Compatibility | Compatibility between a derived GTS Type Schema and its base-type chain. It requires every instance valid against the derived Type Schema to remain valid against every base Type Schema in that chain. |
| Version Family | The set of logical entities that are Version Successors of one another. It is named by the canonical GTS Identifier with the major version of its **last** segment removed and the trailing `~` of a Type Identifier normalized away, every preceding segment held exactly as written. Succession therefore never crosses a derivation chain — adopting a new base major produces an entity in a different family — and one family name covers exactly one entity kind. |
| Version Successor | A distinct logical GTS entity in the same version family whose concrete GTS version is higher than the entity it succeeds. It is not an internal content revision of the same logical entity. For Managed Entities, ADR-0004's major-only policy means that a Version Successor has a higher major version. |
| Unstable Type Schema | A Managed Type Schema whose own last identifier segment carries major version 0. It evolves without the enforced Type Schema Evolution Compatibility check, and no entity outside the profile may reference or derive from one. Defined by ADR-0015. The marker means nothing on a registered Instance identifier and is refused there. |
| Registry Reference | Opaque UUID returned by the Types Registry SDK for one exact client-supplied GTS Identifier and persisted by a domain gear as its type reference. Domain gears do not derive Registry References and do not persist GTS Identifiers as type references. When P2 Aliases are introduced, an Alias GTS Identifier has its own Registry Reference. The value is named `gts_uuid` in registry storage, in the SDK, and in the REST contract; *Registry Reference* is what this document calls the concept, not a second field name. Naming it after what it is does not relax the rule against deriving it locally: that rule protects a long-lived compatibility invariant and is enforced by SDK documentation and review rather than by a name that concealed the value's shape. |
| Concrete Reference Set | Complete, deduplicated, bounded set of Registry Reference UUIDs selected by a type filter for use as a domain-storage query constraint. It is not a paginated result, normalized predicate, or opaque executable query plan. |
| Alias | Strictly P2 Registry-managed alternate GTS identifier that resolves only to a Managed GTS Type Schema or Managed registered GTS Instance. Every Alias is a Managed Entity; Externally Managed Aliases and Aliases targeting Externally Managed Entities are not supported. |
| Owning Gear | Gear that owns runtime storage and behavior for objects that use a registered type. |
| Validation Hook | P2 registry-governed declaration that allows an owning gear to semantically validate admission or deletion of a Managed Type Schema or registered Instance. |
| Admission Candidate | Proposed initial definition or content update undergoing validation. It is not a logical registry entity or an admitted immutable revision and is never returned by ordinary resolving or discovery. |
| Admission Status | Internal state of an Admission Candidate: `PENDING`, `ADMITTED`, or `REJECTED`. Under ADR-0012 it is not part of the SDK or REST contract; the public carrier of progress is the operation resource, whose **single** status is `pending`, `running`, `succeeded`, `unchanged`, `partially_succeeded`, or `failed` — progress and outcome are one field rather than two, so an outcome cannot be stated under a progress value that never carried it. There is no cancellation and no expiry: a stalled operation is failed. The operation also carries the scoped request key, so an accepted mutation has one durable record rather than a receipt and an operation. |
| Dry Run | Mode of a mutating operation that performs its complete check sequence and commits nothing. It is a mode rather than a separate operation, so the checks cannot drift from the ones admission applies. A Dry Run is not a guarantee of admission: its verdict is relative to the state observed during the run. |
| Registry Federation | Types Registry capability to expose one platform-facing registry contract over multiple registry sources. |
| Registry Source | Authoritative provider of registry definitions: either Types Registry's managed storage or a configured External Registry Source integrated through a Registry Source Plugin. |
| External Registry Source | Vendor or platform-integrated registry source outside Types Registry's own authoritative storage. |
| Registry Source Plugin | Governed ToolKit plugin through which Types Registry resolves and queries an External Registry Source. The plugin owns external definitions, identifiers, Registry Reference mappings, revisions, caches, indexes, tombstones, and tenant state, and has no write path into Types Registry state. |
| Source Claim | Rooted single-segment GTS wildcard pattern declared by a Registry Source Plugin instance to identify the non-overlapping identifier space served by that source. It covers every identifier chained beneath what it matches. |
| External Revision | Opaque, source-owned freshness token for one exact Externally Managed Entity. Equal revisions identify equal canonical content and content hash. |
| Managed Entity | Registry entity for which Types Registry is the source of truth. |
| Externally Managed Entity | Registry entity whose definition, Registry Reference mapping, revisions, caches, history, and source-owned state are authoritative in an External Registry Source and obtained live through its Registry Source Plugin, while Types Registry governs platform visibility and usage semantics. |
| Tenant Subtree | A tenant and all of its descendants in the platform tenant hierarchy. |
| Lifecycle Status | Platform-level state of an admitted logical registry entity. In P1 the vocabulary is `ACTIVE` or `DELETED` for every entity, managed or externally managed. `DEPRECATED` is deferred past P1 in both halves by ADR-0008: managed deprecation is not built, and a source assertion of deprecation is not surfaced. `PENDING` is an Admission Status, not a Lifecycle Status. |
| Tenant Enablement State | Tenant-level policy input for an entity: `NOT_INITIALIZED`, `ENABLED`, or `DISABLED`, the last with an optional reason and expiry. In P1 it may be source-owned for an Externally Managed Entity; post-P1 Types Registry also stores and manages it for Managed Entities. It is not the consumer-facing availability result. |
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

- **Role**: Provides live forward/reverse resolution, querying, caching, revision metadata, lifecycle assertions, and tenant state for an External Registry Source through a platform-governed plugin contract. The contract is read-only with respect to Types Registry state.

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

A second constraint governs content rather than deployment. Admitted revisions are retained without a time limit, and the one operation that physically removes them also releases the GTS Identifier and is therefore disabled by default in production, so admitted content in a production deployment is effectively unremovable. ADR-0013 records why no narrower erasure path is offered in its place, including why the payload of a deleted entity cannot be dropped while its identity is kept.

Those retention terms are unconditional, and deciding what may be registered under them is not a Types Registry responsibility. Data classification, and any resulting limit on what content may be placed in a registered Type Schema or Instance value, belongs to platform-wide policy and to the authors bound by it; Types Registry stores what it admits and applies no content policy of its own.

## 4. Scope

### 4.1 In Scope

- GTS Type Schema registration, retrieval, search, lifecycle, Type Schema Evolution Compatibility checks, and Type Derivation Compatibility checks.
- GTS Instance registration, retrieval, search, lifecycle, and validation, plus P2 casting.
- P2 owning-gear semantic validation hooks for initial admission, content revisions, and deletion of Managed Type Schemas and registered Instances.
- Registry federation and live support for externally managed entities through ordered Registry Source Plugins, including platform-owned federation boundary enforcement, forward/reverse resolving, querying, source-owned caching, revision metadata, lifecycle assertions, and tenant state.
- P2 Alias management and alias-aware resolving.
- Stable registry reference support for domain gears.
- Tenant/global ownership, visibility, and management boundaries.
- Lifecycle status, post-P1 tenant enablement state, and computed tenant availability state for registry entities.
- Dependency tracking for GTS and JSON Schema references.
- `gts-rust` integration for GTS parsing, validation, reference derivation, wildcard matching, compatibility, casting, and schema generation/conversion capabilities required by registry workflows.
- SDK and REST contracts for registry management, resolving, validation, discovery, and P2 casting.
- Client-side cache correctness protocol.

### 4.2 Out of Scope

- Runtime domain-object storage and business behavior owned by other Gears, except explicitly registered well-known GTS Instances.
- Read and query policy for existing runtime domain objects whose referenced registry entity becomes unavailable; this policy is owned by the respective Domain Gear.
- Authoritative management of external registry sources that remain outside the platform's ownership boundary.
- GTS namespace governance outside registration-time validation and conflict detection.
- A general-purpose business audit product. Types Registry retains admitted content revisions and emits operation/audit records for registry mutations as required by its revision and lifecycle model; it does not provide platform-wide audit query, retention, or export capabilities.
- Local projection, synchronization, indexing, revision history, or caching of Externally Managed Entity content inside Types Registry.

## 5. Functional Requirements

> **Testing strategy**: Functional requirements are verified through automated unit, integration, and end-to-end tests in accordance with the repository testing architecture, targeting 90%+ code coverage unless a requirement specifies another verification method.

Functional requirements define what Types Registry must provide. Design details such as DB tables, route paths, cache transport, and exact SDK or REST DTOs are intentionally outside this PRD and will be specified in the Types Registry DESIGN document and, where appropriate, ADRs.

### 5.1 Registry Core

#### Type Schema Management

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-register-schemas`

The system **MUST** allow authorized actors to register, retrieve, search, update lifecycle state for, and delete GTS Type Schemas, subject to validation, content-profile, ownership, dependency, and compatibility rules. The content profile of a Managed Type Schema includes its JSON Schema Dialect, restricted by `cpt-cf-types-registry-fr-gts-validation`.

- **Rationale**: Gears need one authoritative registry for type contracts.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`, `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-architect`, `cpt-cf-types-registry-actor-xaas-vendor-developer`, `cpt-cf-types-registry-actor-tenant-admin`

#### Instance Management

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-register-instances`

The system **MUST** allow authorized actors to register, retrieve, search, update lifecycle state for, and delete named GTS Instances that conform to registered Type Schemas.

A registered Instance **MUST NOT** conform to an unstable Type Schema. ADR-0006 forbids a schema revision from becoming current while an affected registered Instance would cease to be valid; applied to an unstable schema that rule would restore exactly the block the profile exists to remove, and waived it would leave admitted Instances failing validation against their own current schema while the registry records a revalidation that no longer holds. Refusing the combination is what keeps both records truthful, and its cost — a control-plane type and its Instances cannot be developed together under the profile — is accepted rather than worked around.

- **Rationale**: Platform gears need registered well-known instances for configuration and discovery metadata.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`, `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-developer`, `cpt-cf-types-registry-actor-tenant-admin`

#### GTS Validation

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-gts-validation`

For Managed Entities and explicit platform validation operations, the system **MUST** validate GTS Identifiers, Type Schemas, Instances, references, wildcard patterns, and version semantics using the platform-approved GTS implementation. For Externally Managed Entities, this requirement applies only to the identifier and response-envelope conformance needed to enforce the federation contract; Types Registry **MUST NOT** interpret or reproduce source-owned entity validation.

The managed identifier profile is narrower than the GTS grammar in three ways, adopted for the same reason as the dialect restriction below: each keeps a platform guarantee decidable. A managed GTS Identifier **MUST NOT** carry a minor version, so it names one mutable logical entity within one major rather than an immutable snapshot (ADR-0004). A managed GTS Identifier **MUST NOT** carry an explicit UUID tail, because the Registry Reference derivation passes such a tail through unchanged and two different identifiers embedding the same tail would resolve to one reference; the invariant that different identifiers resolve to different references is therefore imposed on what may be admitted rather than guaranteed by the derivation (ADR-0001). A managed **registered Instance** identifier **MUST NOT** carry major version 0 in its last segment, because ADR-0015 gives that value a meaning — unenforced Type Schema evolution — that is vacuous for an Instance, whose successive values have no compatibility relation at all; admitting it would leave one marker meaning two things and a reader unable to conclude anything from seeing it. None of the three restrictions reaches an Externally Managed Entity, whose identifiers its source owns.

Major version 0 in the last segment of a managed **Type Schema** identifier is admissible and is not merely tolerated: under ADR-0015 it marks the unstable profile, in which the entity evolves without the enforced compatibility check of `cpt-cf-types-registry-fr-validate-schema-compat`. Every other admission check applies unchanged, and the quarantine rule stated in that requirement bounds what may depend on such an entity.

A managed Type Schema **MUST** declare a top-level `$schema`, and in P1 that dialect **MUST** be JSON Schema Draft-07; a `$schema` below the document root **MUST** be absent or equal to the root's. The declared dialect is pinned at initial admission and **MUST NOT** change across a logical entity's content revisions. Types Registry **MUST NOT** rely on a validator's default-dialect fallback for an absent value, and **MUST NOT** persist the declared dialect as registry state, since it is recoverable from the retained document.

ADR-0014 decides this and records why: a compatibility relation is defined only relative to a dialect, the platform GTS implementation resolves a mixed Resolution Closure by discarding every non-leaf `$schema`, and JSON Schema ignores unrecognized keywords, so mixing removes constraints silently in both directions rather than failing. When the admissible set widens past P1 it **MUST** be governed by dialect uniformity across the Resolution Closure, of which P1 is the degenerate case. `x-gts-ref` targets are excluded from that rule because they are instance-value constraints and are never inlined.

None of this applies to an Externally Managed Entity. The source owns its evolution and derivation rules, ADR-0011's closed boundary keeps external documents out of every managed Resolution Closure, and Types Registry **MUST NOT** inspect `$schema` in returned external content.

- **Rationale**: Registry behavior must match the GTS specification and avoid divergent local interpretations. Where the specification leaves a question open — which dialect governs a mixed closure, and whether a successive definition may change dialect — the platform narrows its own managed profile instead of inventing an answer.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`, `cpt-cf-types-registry-actor-ci-pipeline`

#### Type Schema Evolution Compatibility Checks

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-validate-schema-compat`

The system **MUST** check a proposed GTS Type Schema revision against the current admitted revision of the same logical Type Schema identity under the platform-enforced Type Schema Evolution Compatibility mode, and **MUST** reject a revision that violates it. The enforced mode is backward compatibility as decided by ADR-0003; the guarantee extends over the whole revision history without comparing against every retained revision.

One profile is exempt. Under ADR-0015 a managed Type Schema whose own last identifier segment carries major version 0 is **unstable**: the system **MUST** admit any content revision of it regardless of its compatibility relation to the current revision, including one the implementation cannot decide. The exemption covers Type Schema Evolution Compatibility and nothing else — Type Derivation Compatibility, dependent revalidation, the dialect profile, reference resolvability, deletion safety, ownership, and registration authority all apply unchanged. The freeze state machine does not reach an unstable entity, because it protects a whole-history guarantee that this profile does not offer; a revalidation pass following a semantic change of the compatibility relation **MUST** skip such entities rather than freeze or fail them.

The exemption **MUST** be bounded so that it cannot reach an entity outside it. A managed entity whose own last identifier segment carries a major version of 1 or higher **MUST NOT** reference or derive from an unstable entity, whether through `$ref`, `x-gts-ref`, or its immediate derivation base, and admission **MUST** reject such a candidate. Without this rule a floating reference would let an unstable entity redefine the accepted-instance set of a stable one — with no revision of the stable entity and therefore no verdict to report — so an owner would lose a guarantee it made to its own consumers through an act of a different owner. The relation is one-way by design: an unstable entity **MAY** reference and derive from a stable one.

The system **MUST** expose the enforced mode, the verdict, and whether the Type Schema is evolvable in place. For an unstable entity the exposed mode **MUST** state that no mode is enforced, and the chain state **MUST** state that none is established, rather than reporting a bare verdict that a reader could mistake for a guarantee. Forward-direction results **MAY** be reported as advisory metadata. Operational claims about producer conventions, reader tolerance, casting, or default materialization **MUST NOT** be presented as schema compatibility results.

- **Rationale**: In-place Type Schema evolution must not silently break producers, consumers, or historical payload processing. A contract that is still being designed is the exception: its author knows the change is breaking and accepts it, and forcing a new major on every reshape would churn the identifier and the Registry Reference that consumers have already persisted. Marking that state in the identifier makes the risk legible to every consumer without a lookup, and the quarantine rule keeps the accepted risk with the owners who accepted it.
- **Actors**: `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-architect`, `cpt-cf-types-registry-actor-xaas-vendor-developer`, `cpt-cf-types-registry-actor-ci-pipeline`

#### Type Derivation Compatibility Checks

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-validate-type-derivation`

The system **MUST** check every derived GTS Type Schema against its immediate base Type Schema and the complete transitive base-type chain. Every instance valid against the derived Type Schema **MUST** remain valid against every base Type Schema in that chain. Registration and activation **MUST** reject derivations that violate base constraints or applicable GTS derivation, finality, and inherited-trait rules.

- **Rationale**: A derived GTS Type must remain safely substitutable for every base Type declared by its GTS identifier chain, independently of compatibility between revisions of any one Type Schema.
- **Actors**: `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-architect`, `cpt-cf-types-registry-actor-xaas-vendor-developer`, `cpt-cf-types-registry-actor-ci-pipeline`

#### Dependency Awareness

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-ref-tracking`

The system **MUST** track dependencies between Managed Entities. Under ADR-0011 every tracked dependency has a Managed Entity at both ends, so the tracked set is authoritative for deletion safety and that decision is reached from local state without plugin availability, plugin cooperation, or plugin-supplied data of any kind. No plugin capability contributes to that set, and none is asked to: the closed boundary leaves no cross-boundary dependency either to register or to report. Types Registry **MUST NOT** expose a client-facing operation for enumerating dependents; what a caller needs — whether a deletion or a revision would be refused, and by what — is answered by the Dry Run of that same mutation. Any visible and tenant-available entity **MUST** remain a valid target for both existing and newly admitted GTS and JSON Schema references; deletion removes a target from that set, and so does the quarantine rule of `cpt-cf-types-registry-fr-validate-schema-compat`, under which an unstable entity is a valid target only for another unstable entity. In P1 there is no lifecycle status between `ACTIVE` and `DELETED`, so no additional exclusion applies.

- **Rationale**: Platform teams need predictable blast-radius analysis for type changes.
- **Actors**: `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-architect`, `cpt-cf-types-registry-actor-xaas-vendor-developer`, `cpt-cf-types-registry-actor-ci-pipeline`

#### Registry Federation

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-registry-federation`

The system **MUST** support multiple Registry Sources, including Types Registry's own managed storage and External Registry Sources integrated through governed Registry Source Plugins. Types Registry **MUST NOT** persist external entity definitions, identifiers, revisions, content hashes, lifecycle state, Registry Reference mappings, query indexes, caches, or tombstones, and the owning plugin **MUST** provide those capabilities live through the Types Registry federation contract. Under ADR-0011 this prohibition has no exception, and Registry Source Plugins **MUST NOT** have any write path into Types Registry state.

- **Rationale**: Vendor products may already have authoritative type registries, but platform gears still need one Types Registry contract for resolving, discovery, and platform governance.
- **Actors**: `cpt-cf-types-registry-actor-xaas-vendor-architect`, `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-registry-source-plugin`

#### Registry Source Routing

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-registry-source-routing`

Each Registry Source Plugin instance **MUST** declare one or more validated Source Claims, the entity kinds it serves, and a deterministic selection priority. Every Source Claim pattern **MUST** be a rooted single-segment wildcard pattern: exactly one GTS segment, carrying the wildcard at a token boundary within it, from `gts.<vendor>.*` through `gts.<vendor>.<package>.<namespace>.<type>.*`. A multi-segment pattern **MUST** be rejected at activation.

The owning claim of an identifier is therefore selected from its **first segment alone**, and because a wildcard segment accepts every remaining segment including the chain separator, an externally managed entity's whole derivation chain lies inside one claim. That is what keeps the managed and externally managed identifier spaces disjoint, and it is why a multi-segment claim is refused: such a claim would slice into a chain whose base segment may be managed.

For every claimed entity kind, an active P1 plugin **MUST** support batch forward and reverse resolution, complete bounded candidate queries with opaque pagination, lifecycle and ownership/visibility assertions, tenant state, revision/hash and conditional-read semantics, retained reverse resolution after deletion, and structured source-failure outcomes. For a claimed Type Schema kind it **MUST** additionally produce the resolved effective schema and the effective trait artifacts, since Types Registry never resolves source-owned content and a consumer therefore has no other way to obtain them. Every capability in the profile is mandatory and authoritative; there is no optional or advisory tier, so no plugin output may degrade in place of failing closed. Neither dependency registration nor reverse dependency-impact lookup is part of the profile at all: the closed boundary leaves no cross-boundary dependency to register, and a report confined to external dependents of an externally managed entity has no consumer, since no operation on either plane enumerates dependents.

Candidate query results **MUST NOT** have false negatives. A plugin **MAY** return a broader candidate set for Types Registry to filter under normalized platform semantics. A plugin configuration **MUST NOT** become active for a Source Claim and entity kind when an applicable mandatory capability is absent; inability to establish a complete result at runtime **MUST** fail closed.

P1 Source Claims **MUST NOT** overlap each other or the identifier space of existing Managed Entities. Because a claim covers every identifier chained beneath it, an external claim and managed identifiers **MUST NOT** nest: a vendor integrating an External Registry Source partitions its identifier prefixes between served-externally and registered-as-managed rather than placing the latter beneath the former. Managed storage **MUST** be consulted before plugins, and plugins **MUST** be consulted in deterministic priority order.

All P1 registry entity list and search operations **MUST** fail closed if any selected Registry Source is unavailable or returns an invalid or incomplete response. P1 **MUST NOT** return a partial result page or treat a source failure as source exhaustion or authoritative absence.

- **Rationale**: Live federation requires deterministic ownership and routing without a per-external-entity index or identifier shadowing.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`, `cpt-cf-types-registry-actor-registry-source-plugin`

#### Externally Managed Entities

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-externally-managed-entities`

The system **MUST** distinguish Managed Entities from Externally Managed Entities. Types Registry **MUST NOT** persist state whose authority belongs to a source, and under ADR-0011 that prohibition has no exception.

The managed and externally managed identifier spaces **MUST** be disjoint, and no reference or derivation may cross between them in either direction. A Managed Entity **MUST NOT** reference or derive from an Externally Managed Entity, and an Externally Managed Entity **MUST NOT** reference or derive from a Managed Entity. A vendor that needs a type derived from a platform contract **MUST** register it as a Managed Entity, where every platform guarantee applies to it; an External Registry Source serves a type universe that is self-contained.

Enforcement of that rule is asymmetric, and the asymmetry is part of the requirement rather than an implementation detail. Admission rejects a Managed Entity that crosses the boundary. On the external side, derivation across the boundary is impossible by construction, because a Source Claim is a rooted single-segment pattern and the owning source of a chained identifier follows from its first segment. A reference from inside an external schema document is a different case: an External Registry Source is outside the platform's control, its implementation **MAY** permit a `$ref` or `x-gts-ref` to a managed identifier, and Types Registry **MUST NOT** interpret source-owned content, so the platform can neither prevent nor detect it.

Types Registry therefore **MUST NOT** be understood to offer any guarantee for such a reference, and **MUST** document that it does not. Validation at admission applies to Managed Entities only. For a cross-boundary content reference the platform provides no deletion safety for the managed target, no availability propagation to the external entity, no revalidation of the external schema when the managed target admits a new revision, no notification of managed lifecycle transitions, and no protection against a purge releasing the identifier and rebinding the reference. The backward-compatibility guarantee on the managed entity's own revision chain is unaffected, because it is unconditional and independent of who consumes the entity; what is absent is the dependent-specific revalidation, not the compatibility mode.

Types Registry **MUST NOT** parse returned external content in order to detect such a reference. Doing so would place content parsing on the live read path, make the platform read source-owned content to enforce a platform rule, and turn a documented limitation into a barrier that makes an otherwise integrable vendor registry unintegrable.

The External Registry Source **MUST** remain the sole authority for whether an Externally Managed Entity is valid under source-owned rules; Types Registry **MUST NOT** require, interpret, or reproduce source-owned entity validation results.

Before exposing a live external result, Types Registry **MUST** validate only federation response conformance and platform-owned invariants: identifier integrity, Registry Reference mapping, Source Claim conformance, entity kind, authorization, visibility, lifecycle mapping, availability, and cache/freshness metadata. Each external result **MUST** carry an External Revision and canonical content hash. Types Registry **MUST NOT** persist those values as registry state.

- **Rationale**: External source ownership must not bypass platform contract governance, while source-owned entity validation policies and results remain outside the Types Registry responsibility boundary.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`, `cpt-cf-types-registry-actor-domain-gear`, `cpt-cf-types-registry-actor-registry-source-plugin`

#### Owning-Gear Semantic Validation

- [ ] `p2` - **ID**: `cpt-cf-types-registry-fr-validation-hooks`

In P2, the system **MUST** invoke every matching required owning-gear Validation Hook before initial admission, before admission of a new content revision, and before deletion of a Managed Type Schema or managed registered Instance. Admission of a higher-major Version Successor is covered as initial admission; it changes no other member of its version family and therefore triggers no additional hook.

Deletion is included because an owning gear is the only component that can see its own runtime objects, and P1 deletion cannot: a type may be deleted while live domain data still conforms to it. Until hooks exist, that exposure is a stated P1 limitation of `cpt-cf-types-registry-fr-lifecycle` rather than a gap the registry can close.

Validation Hooks **MUST NOT** apply to Externally Managed Entities, P2 Aliases, or tenant enablement changes. Those operations remain governed by their registry, dependency, lifecycle, source, and authorization rules.

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

The system **MUST** resolve between user-facing GTS Identifiers, machine-readable Registry References, entity kind, ownership scope, and lifecycle status for both single and batch lookups. For domain-owned data, the Types Registry SDK **MUST** return an opaque Registry Reference UUID for the exact client-supplied GTS Identifier. Domain gears **MUST** persist that Registry Reference rather than deriving it or persisting the GTS Identifier as the type reference. Types Registry **MUST** resolve Managed Entities locally, then delegate unresolved external references to Registry Source Plugins in deterministic priority order. A plugin-returned GTS Identifier **MUST** derive to the requested Registry Reference and match the plugin's Source Claim. Where Types Registry observes two distinct GTS Identifiers resolving to one Registry Reference, it **MUST** fail with a structured identity-collision error rather than select a winner, since silently choosing one corrupts persisted domain references. A collision between two External Registry Sources that is never co-observed cannot be detected and is an accepted, documented residual of deterministic derivation. When P2 Alias support is introduced, reverse resolution **MUST** preserve an exact client-supplied Alias GTS Identifier while exposing Alias target metadata separately, and Managed Aliases **MUST** resolve locally.

- **Rationale**: Domain gears need stable references for stored data and human-readable identifiers for APIs, logs, and operator workflows.
- **Actors**: `cpt-cf-types-registry-actor-domain-gear`, `cpt-cf-types-registry-actor-platform-gear`

#### Type Query Assistance

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-type-query-assistance`

The system **MUST** translate user-facing type filters, including exact GTS Identifiers, compatible versions, derivation hierarchy constraints, and GTS wildcard patterns, into a complete, deduplicated Concrete Reference Set suitable for querying gear-owned data by Registry Reference UUID. One operation need not accept every filter kind: exact identifiers are translated by the batch read, which takes an arbitrary list and does not paginate, while the remaining kinds are translated by the paged expansion described below. Query assistance **MUST NOT** return a normalized database predicate or opaque executable query plan. If any source required to establish the set is unavailable or invalid, query assistance **MUST** fail rather than yield a partial constraint.

The set is assembled by a paged traversal rather than produced by one operation, and the contract states what that does and does not give. It **MUST** be exhaustive for the filter — every matching reference appears, and the caller-facing contract **MUST NOT** hand back a partially accumulated set as if it were whole. It is **not** a snapshot: entities may be registered or deleted between the first page and the last, so the set is complete with respect to the traversal rather than to an instant. Pagination is what bounds the memory a deduplicated set would otherwise occupy, and the loss of atomicity is accepted deliberately in exchange.

The result **MUST** stay within a documented maximum reference count, and enforcing that maximum **MUST** remain the registry's obligation rather than a client convention: the pagination cursor **MUST** carry the count already served, and the page that would take the total past the maximum **MUST** return a structured `QUERY_EXPANSION_LIMIT_EXCEEDED` failure. Types Registry **MUST NOT** silently truncate. Accumulation itself **MAY** be provided by the SDK, and a caller that bypasses it receives pages and assembles them itself.

Query assistance is a tenant-plane operation carrying the requesting tenant's `SecurityContext`, propagated by the calling gear from the request it is serving. The set **MUST** contain only references visible **and available** to that tenant, so it is tenant-specific and one filter yields different sets for different tenants. Narrowing to available leaves the unavailable-entity policy of `cpt-cf-types-registry-fr-tenant-availability` with the owning gear, which exercises it when reading a known object rather than when filtering a list.

Federated expansion **MUST** internally use source-major traversal: managed results first, followed by matching Registry Source Plugins in deterministic priority order. Internal continuation tokens **MUST** bind the query, the requesting subject's visibility context and the Context Tenant the page was narrowed for, the authorization scope, the plugin configuration revision, the current source, and the source cursor. A token presented under a different tenant or authorization scope **MUST** be rejected with a structured stale-cursor failure rather than continued. Binding those is a completeness property rather than a disclosure one — every page is filtered for the subject presenting it, so no result crosses a boundary — but continuing a traversal across a change of context would assemble one set out of two different visible sets, and would advance a source cursor pointing into a scan the source performed for another tenant. Global ordering by entity fields across Registry Sources is irrelevant to the resulting set and remains outside P1.

- **Rationale**: Domain gears persist Registry Reference UUIDs and need a portable constraint that can be applied consistently across SQLite, PostgreSQL, and MySQL without executing Registry-owned predicates or query plans inside gear-owned storage.
- **Actors**: `cpt-cf-types-registry-actor-domain-gear`

### 5.3 Ownership, Lifecycle, And Caching

#### Tenant And Global Ownership

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-tenant-ownership`

The system **MUST** support platform-global registry entries and tenant-owned registry entries with explicit visibility, management, and conflict rules. Platform-global entries **MUST** be visible to every tenant, subject to lifecycle, availability, and authorization rules. A tenant-owned entry **MUST** be visible only within the Tenant Subtree rooted at its owning tenant, including the owning tenant itself, and **MUST NOT** be visible to ancestor, sibling, or unrelated tenants. Discovery, search, exact resolution, batch resolution, and query assistance **MUST** enforce the same ownership-visibility boundary and **MUST NOT** disclose the existence or metadata of an entry outside its visible scope. Visibility does not grant management authority; management remains subject to ownership and platform authorization rules.

These disclosure rules govern the tenant plane. A platform-plane read **MUST** span every tenant without visibility filtering — there is no requesting tenant, so the Tenant Subtree relation has no left-hand side — and **MUST NOT** disclose which tenant owns what: the one operation that must name owners, the purge report of `cpt-cf-types-registry-fr-lifecycle`, carries them itself. Authorization still applies. A platform-plane request **MUST NOT** create a tenant-owned entity: ownership is derived from the requesting context and this plane has none, so there is nothing to derive an owner from.

Ownership is evaluated but **MUST NOT** be disclosed as an identity on the tenant plane. A read result **MUST** carry only whether the requesting tenant owns the entry, and **MUST NOT** carry an owning tenant identifier: the identifier is not actionable, since no operation lets one tenant address another through Types Registry, and disclosing it would let a caller map the tenant hierarchy above itself by browsing the contracts it can see. Discovery **MUST** select by ownership scope rather than by a supplied tenant identifier, for the same reason ownership is not request data on the write path — accepting one would let a caller probe for its ancestors by observing whether a filtered result is empty.

An Externally Managed Entity **MUST** carry an ownership scope asserted by its owning Registry Source Plugin, and Types Registry **MUST** derive visibility from it using the same Tenant Subtree relation. The plugin states only the flat fact — platform-wide, or one owning tenant — while the hierarchy relation, the authorization decision, and the availability verdict remain platform-computed. The assertion is mandatory in a plugin response; an absent one, or one naming a tenant the platform does not know, **MUST** be rejected as an invalid source response rather than exposed. It confers no management authority, because no write path to an Externally Managed Entity exists.

The ownership scope of an admitted entry is fixed at admission and **MUST NOT** change afterwards; the system offers no ownership-correction operation. A mis-assigned owner is repaired by deleting the entry and re-registering it under the correct owner, which first requires the platform purge of ADR-0013 to release the identifier. Changing an owner changes which tenants can see a contract, so a correction would have to establish which registered dependents lose sight of the entry and which entries the new owner loses sight of, and then reject or migrate them — a migration of the visible audience under a name suggesting a repair.

- **Rationale**: Platform types and tenant customizations must coexist without cross-tenant leakage or accidental global mutation, while descendants can reuse contracts governed by an ancestor tenant.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`, `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-architect`, `cpt-cf-types-registry-actor-xaas-vendor-developer`, `cpt-cf-types-registry-actor-tenant-admin`

#### Registration Authority

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-registration-authority`

The system **MUST** authorize every initial admission, content revision, and deletion against the GTS Identifier being registered, and **MUST** perform that authorization before it evaluates whether the identifier is available.

Registration, revision, and deletion of a platform-global entity **MUST** be a platform-plane operation carrying `PlatformSecurityContext`. A tenant-plane request **MUST NOT** create, revise, or delete a global entity under any grant, and the platform plane **MUST NOT** be reachable from the tenant-facing REST surface.

The owning tenant of a tenant-plane registration **MUST** be derived from the request's `SecurityContext`. Ownership **MUST NOT** be accepted as request data: no payload field may name an owning tenant or select the global scope, and a request carrying one **MUST** be rejected rather than honoured. Ownership is consequently a property of who asked, not of what was asked for, which is what makes the authorization below a decision about one subject and one candidate identifier rather than about a claimed owner as well.

Platform-plane operations **MUST NOT** be authorized through the tenant policy path. Under `cpt-cf-adr-two-plane-auth` a `PlatformSecurityContext` is never evaluated by the tenant `PolicyEnforcer`, so Types Registry **MUST NOT** issue a PDP decision request for a platform-plane call, and **MUST NOT** define a permission whose only evaluation point would be that plane. Authorization there is the validated platform workload identity of `cpt-cf-adr-platform-plane-auth`; any narrowing beyond it is workload policy over that identity and is outside this gear's scope. It follows, and **MUST** be documented for operators rather than left implicit, that any authenticated platform workload may author, revise, or delete any global entity — `owning_gear` is attribution and **MUST NOT** be treated as authority. Purge is additionally gated by the deployment policy of ADR-0013, which is a separate and stronger control than any grant.

Registration, revision, and deletion of a tenant-owned entity **MUST** be authorized by the platform PDP for the requesting subject, the requested action, and the candidate's canonical GTS Identifier. Types Registry **MUST** supply the candidate identifier to the authorization request as a resource property, and **MUST** fail closed when the decision is negative or absent, when the PDP is unreachable, or when a returned constraint references a property Types Registry cannot enforce.

Authority over a region of the GTS identifier namespace is therefore a **grant, not a consequence of registering first**. A subject holding a permission whose resource expression covers `gts.<vendor>.<package>.*` may register within that region; a subject with no covering grant **MUST** be refused whether or not the identifier is free. Without this, the global namespace would be first-come-first-served and any tenant could occupy another vendor's prefix.

**The platform's own namespace is reserved, and in P1 the reservation is absolute.** Every candidate whose canonical GTS Identifier matches `gts.cf.toolkit.*` — which under GTS §3.6 covers the whole derivation chain beneath it, not only the base segment — **MUST** be refused on the tenant plane and **MUST NOT** be admitted as tenant-owned under any grant. Such an entity is admissible only on the platform plane, where it is global by construction. The refusal is a property of the candidate's identifier evaluated during envelope validation, not a consequence of who happens to hold which grant, so a misconfigured grant covering that region cannot produce a tenant-owned platform contract. This bounds no grant model and requires none: it is checked before the PDP is consulted at all.

The rule is deliberately broader than the entities that need it, and the asymmetry of the alternatives is why. Ownership is write-once per version family, so a wrongly admitted tenant-owned platform contract is not repairable by revision — only by a purge that releases its identifier, an operation of a different destructive class. Relaxing the reservation later, for a derived contract that a tenant should be able to own, is additive and needs no migration; discovering that it should have applied is not. In P1 nothing is known to need the relaxation: platform base types are registry-owned, plugin registrations and permission declarations are platform-plane acts, and a vendor extending a platform base type may still do so globally on the platform plane.

Ordering is normative rather than incidental. Because `cpt-cf-types-registry-fr-tenant-ownership` deliberately discloses name availability on the registration surface, evaluating availability before authority would let an unauthorized caller enumerate the namespace by attempting registrations. An unauthorized caller **MUST** receive the same response whether the candidate identifier is free, held by a visible entity, held by an invisible one, or held by a tombstone or Source Claim reservation.

Authorization of a batch **MUST** hold for every member. This composes with the single-authorization-scope rule of `cpt-cf-types-registry-fr-two-phase-init`: a batch is bounded by one scope, and every member must additionally be covered by a grant within it.

- **Rationale**: GTS Identifiers are globally unique in a vendor-structured namespace, so the right to name something is a governed right. Neither platform authority nor prefix ownership can be inferred from the order in which registrations arrive.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`, `cpt-cf-types-registry-actor-tenant-admin`, `cpt-cf-types-registry-actor-xaas-vendor-architect`, `cpt-cf-types-registry-actor-xaas-vendor-developer`

#### Lifecycle Management

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-lifecycle`

The system **MUST** manage the Lifecycle Status of admitted Managed Type Schemas and registered Instances as `ACTIVE` or `DELETED`. `PENDING` **MUST** be an Admission Status of a candidate or admission operation and **MUST NOT** be exposed as the Lifecycle Status of a logical entity. `DEPRECATED` is not part of the P1 vocabulary at all: no Managed Entity may carry it, and no Externally Managed Entity is exposed with it. Deprecation is deferred past P1 in both halves by ADR-0008. Initial admission **MUST** atomically create the logical entity in `ACTIVE` with revision `1`; a failed initial candidate **MUST NOT** create a logical entity. While an update candidate is `PENDING`, the existing entity **MUST** retain its current Lifecycle Status and current admitted revision.

Each successfully admitted content update **MUST** create the next monotonically increasing revision scoped to the Managed logical entity. A pending, rejected, failed, or idempotent no-op candidate **MUST NOT** create a content revision or change the current revision. Lifecycle-only transitions, including deletion, **MUST NOT** create content revisions. The lifecycle change and the corresponding cache freshness metadata **MUST** become visible atomically.

Admitting a higher-major Version Successor **MUST NOT** change the Lifecycle Status of any other member of its version family. Several members of one managed version family **MAY** be `ACTIVE` simultaneously, and members **MAY** be admitted in any order. P1 **MUST NOT** expose a deprecation, undeprecation, or reactivation transition for a Managed Entity.

The system **MUST NOT** compute or expose which member of a version family is newest. Version ordering is already encoded in the members' GTS Identifiers, so a caller that can enumerate a family can read it directly; discovery **MUST** therefore support enumerating the members of a version family. P2 Aliases **MUST**, when introduced, use the same logical-entity lifecycle model unless the P2 Alias decision explicitly supersedes it.

An authorized deletion operation **MUST** be permitted to transition an `ACTIVE` entity directly to terminal `DELETED`. Deletion **MUST NOT** require a Version Successor and **MUST NOT** be constrained by the status of other members of the same version family, but **MUST** be rejected while a live registered dependent exists. Under ADR-0011 every dependent is a Managed Entity, so complete dependency impact is always establishable locally and deletion depends on neither plugin availability nor plugin-supplied data.

P1 deletion validates only what Types Registry can establish from its own state: derived types, schemas holding a `$ref` or `x-gts-ref` to the target, and registered Instances conforming to it. There is no fourth category. It has no visibility into runtime objects held by domain gears, so a Type Schema **MAY** be deleted while live domain data still conforms to it. Owning-gear validation of deletion arrives with `cpt-cf-types-registry-fr-validation-hooks` in P2; until then this is a stated limitation and not a registry guarantee. `DELETED` **MUST** be terminal in P1, P1 **MUST NOT** support restore, and a deleted GTS Identifier **MUST NOT** be reused for a new logical entity. Admitted content revisions **MUST NOT** be physically removed by any retention period, time-to-live, or background policy; the only mechanism that physically removes admitted content or identity is the explicit platform-level purge operation decided by ADR-0013. Operation records are not admitted content: a terminal operation that no revision points at **MAY** be removed on a retention policy, which releases no identifier and leaves no entity, revision, or tombstone changed. Deletion **MUST** preserve identity-resolution guarantees for previously issued Registry References.

For Externally Managed Entities, Types Registry **MUST** obtain source lifecycle assertions live from the owning Registry Source Plugin and map exposed entities to the platform `ACTIVE` or `DELETED` semantics. A source **MAY** assert that an entity is deprecated, and P1 **MUST** expose such an entity as `ACTIVE`: deprecation discourages new adoption without changing what the entity is or how it behaves, so `ACTIVE` states the P1 truth rather than approximating it. Types Registry **MUST NOT** invent a third status to carry the assertion, and the federation contract **MUST** state plainly that P1 does not relay it, so that a vendor learns this before integrating rather than after. An external source **MAY** transition an entity directly to `DELETED` whether or not it previously deprecated it. Source-side pending candidates **MUST NOT** be exposed as logical registry entities. Resolution, reference validation, and search behavior **MUST** respect the resulting platform status.

- **Rationale**: Type evolution needs controlled activation and removal. The registry neither invents owner intent nor restates version ordering that the identifiers already carry.
- **Actors**: `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-architect`, `cpt-cf-types-registry-actor-xaas-vendor-developer`, `cpt-cf-types-registry-actor-tenant-admin`

#### Tenant Availability Evaluation

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-tenant-availability`

The system **MUST** evaluate and expose a Tenant Availability State for a concrete registry entity and tenant. The result **MUST** be derived from Lifecycle Status, visibility, the state of availability-blocking relationships, and, when applicable, authoritative tenant state and freshness from the External Registry Source. Under ADR-0010 a relationship is availability-blocking when its target contributes to the semantic contract required to use the subject. Materialization does not sever that relationship, and unavailability propagates transitively along outgoing blocking edges only.

P1 has no managed tenant enablement override. A visible `ACTIVE` Managed Entity is eligible for `AVAILABLE`, but **MUST** be `UNAVAILABLE` with a reason when an availability-blocking relationship is not available for the requesting tenant. A `DELETED` entity **MUST** be `UNAVAILABLE`. It is still returned by an exact read, marked deleted, so that a gear holding a stored Registry Reference can distinguish a retired contract from an identifier that never existed; discovery, search, and query assistance exclude it. Admission Candidates are not logical entities and **MUST NOT** participate in availability evaluation.

Tenant Availability State is evaluated for a **Context Tenant** — the tenant scope root of the operation, which may differ from the requesting subject's own tenant. A caller **MAY** name one; on the tenant plane it defaults to the subject's tenant, and on the platform plane it has no default, so the verdict **MUST** be absent when none is named and the system **MUST NOT** invent a not-evaluated value to fill the gap. Naming a descendant is the supported way to ask why that tenant cannot use a given entity, and the platform PDP **MUST** authorize it — the subject's tenant must be an ancestor of the one named.

Two tenants therefore act on one read and **MUST NOT** be conflated: visibility **MUST** be evaluated for the subject and availability for the Context Tenant. Their visible sets are not nested, since an entity owned by a descendant is invisible to its ancestor, so evaluating visibility for the Context Tenant would disclose a descendant's contracts to whoever names it.

When the External Registry Source cannot confirm state required for availability evaluation, the operation **MUST** fail closed. Types Registry determines and exposes the availability result, but the handling of an existing runtime domain object whose referenced registry entity is unavailable remains the responsibility of that object's owning Gear. Each owning Gear defines whether its operations filter, reject, or return such an object with an explicit unavailable status.

- **Rationale**: Consumers need one authoritative usability result instead of independently combining lifecycle, tenancy, dependency, and external-source rules.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`, `cpt-cf-types-registry-actor-domain-gear`, `cpt-cf-types-registry-actor-tenant-admin`, `cpt-cf-types-registry-actor-registry-source-plugin`

#### Tenant Enablement Management

- [ ] `p2` - **ID**: `cpt-cf-types-registry-fr-tenant-enablement`

The system **MUST**, after P1, support a stored Tenant Enablement State for an entity: `NOT_INITIALIZED`, `ENABLED`, or `DISABLED`, where `DISABLED` carries an optional reason and optional expiry. This state is a policy input to Tenant Availability State, not the consumer-facing result. Types Registry **MUST** allow authorized actors to manage this state for Managed Entities. For Externally Managed Entities, the External Registry Source remains authoritative for tenant enablement state.

- **Rationale**: Tenant policy must be independently controllable without conflating it with platform lifecycle or computed availability.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`, `cpt-cf-types-registry-actor-domain-gear`, `cpt-cf-types-registry-actor-tenant-admin`, `cpt-cf-types-registry-actor-registry-source-plugin`

#### Casting

- [ ] `p2` - **ID**: `cpt-cf-types-registry-fr-casting`

The system **MUST** support casting supplied instance content between two registered GTS Type Schemas that Types Registry can relate, and **MUST** report incompatible casts as structured failures.

GTS OP#9 defines casting only between compatible minor versions. Under ADR-0004 a managed entity carries no minor version, so the transitions this requirement covers — between major identities in one version family, and between content revisions of one logical entity — lie outside OP#9. Types Registry **MUST** present such a result as a platform capability and **MUST NOT** present it as an OP#9 conformance result. The exact admissible transition set is an open question.

- **Rationale**: Consumers need a central, consistent way to migrate or interpret versioned typed content.
- **Actors**: `cpt-cf-types-registry-actor-domain-gear`, `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-developer`

#### Cache Freshness Metadata And Conditional Reads

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-cache-freshness-metadata`

The system **MUST** return, with every resolution and discovery result, the metadata required to determine later whether that result is still current, and **MUST** publish updated metadata atomically with the mutation that invalidates it.

This is a P1 obligation of the registry regardless of whether any client caches. Under ADR-0004 a managed GTS Identifier is no longer content-immutable, so a result carries no implicit validity; ADR-0005 and ADR-0006 make the current revision and content hash part of every ordinary resolution result. A resolution result **MUST NOT** be validated by an entity resource version alone, because ADR-0010 establishes that a tenant availability verdict can change with no mutation to the resolved entity. For an Externally Managed Entity the validator **MUST** be the opaque revision and content hash returned by the owning Registry Source Plugin; Types Registry does not persist external cache state. That token **MUST** be scoped to the entity and the tenant the read concerns, and **MUST** change whenever anything the platform exposes for that pair changes rather than only when canonical content does. Tenant enablement is the case this exists for: it is source-owned, it moves the availability verdict, and a content revision would not move with it, so a token covering content alone would let a conditional read report unchanged for an entity the source has disabled for that tenant.

The system **MUST** also accept a caller-supplied validator on read operations and report the result unchanged instead of returning it, and this is P1 rather than P2. The obligation to emit a validator is only half a mechanism: a consumer that can detect staleness but must transfer the whole result to do so will not poll often enough to be current. Conditional reads **MUST** be available on batch reads per requested item, not only on single reads, because the load-bearing case is a consumer re-checking every definition it holds; making it re-ask for each one individually is the shape that does not scale and the one that will therefore be skipped.

Three properties bound the mechanism. A validator **MUST** be scoped to the field projection it was issued for, so that a caller supplying one obtained under a different projection observes a mismatch and receives the full result rather than a false unchanged. Types Registry **MUST NOT** report unchanged when it cannot establish that the result is still current — an unconfirmed unchanged is the one failure direction that silently hands the caller stale authority, and `cpt-cf-types-registry-principle-fail-closed` applies. For an Externally Managed Entity the check **MUST** be delegated to the owning Registry Source Plugin through the conditional-read semantics its capability profile already requires, so a caller polls managed and externally managed entities through one contract without branching.

These two facilities are the server-side half of the mechanism. The SDK half — storing, validating, and evicting on the caller's behalf — is `cpt-cf-types-registry-fr-client-cache`, which is also P1. A caller that declines the SDK cache and holds resolved content in process memory can still keep it correct by hand with the validator and the conditional read alone.

- **Rationale**: Once a managed identifier is mutable, a consumer cannot tell a current result from a stale one without the registry saying so. This is a correctness property of the registry, not of its clients. Emitting the validator without honouring it leaves the correct behaviour available in principle and unaffordable in practice, which is how consumers end up holding a definition for the lifetime of a process. A later event-based invalidation transport does not retire this: events say when to invalidate, a validator says whether what is held now is current, and only the second answers for a process that just started or missed a message.
- **Actors**: `cpt-cf-types-registry-actor-domain-gear`, `cpt-cf-types-registry-actor-platform-gear`

#### Client-Side Cache Correctness

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-client-cache`

The system **MUST** define SDK client caching behaviour — storage, validation against the freshness metadata above, and eviction — such that a client cannot treat an invalidated result as current across registry mutations.

A client that never caches and always resolves is still correct, so this requirement is not what makes resolution correct — `cpt-cf-types-registry-fr-cache-freshness-metadata` is, and it supplies the two server-side facilities this one is built on: the emitted validator and the conditional read that honours it. What this requirement adds is that the **SDK** does the caching rather than each consumer separately: where entries live, when they are evicted, how a cold start behaves, and how a batch poll is scheduled.

It is P1 for the same reason the conditional read is. Registry resolution sits on gear startup and on hot paths, so consumers will cache whether or not the SDK does; leaving it to them yields one cache per gear over a protocol whose failure mode is stale type authority, and an invalidation defect in any one of them is indistinguishable from a registry defect. A caller that declines the SDK cache may still use both server-side facilities directly.

- **Rationale**: Registry lookups are common on startup and hot paths; caching must not return stale type authority.
- **Actors**: `cpt-cf-types-registry-actor-domain-gear`, `cpt-cf-types-registry-actor-platform-gear`

#### Batch Admission And Startup Registration

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-two-phase-init`

The system **MUST** support batch admission: a caller submits a set of Admission Candidates that are validated and admitted as one operation, so that a reference from one member to another resolves against the submitted candidate rather than against that identifier's previously committed state.

A batch is **not** one all-or-nothing transaction. Under ADR-0012 the P1 batch mode is **dependency-aware partial admission**: the candidate graph is condensed into strongly connected components and processed in dependency order, one acyclic candidate is one admission unit, one cyclic component is one atomic admission unit, independent units that pass every check are admitted even when another branch of the same batch fails, and a candidate whose selected in-batch dependency failed **MUST NOT** be admitted, and **MUST** be reported as failed under a reason that distinguishes it from a candidate that was evaluated and rejected — the first may pass unchanged once the other is fixed, and a caller cannot act correctly without knowing which it holds. Every member **MUST** carry an independent outcome keyed by its exact GTS Identifier, and a failure **MUST** identify the offending members with sufficient diagnostics for correction and retry.

A batch **MAY** mix initial admissions with content revisions of existing entities, each member carrying its own precondition. An admitted initial candidate creates the logical entity as `ACTIVE` with revision `1`; a failed initial candidate creates no logical entity and leaves previously committed registry state unchanged, whether it was rejected on its own merits or never evaluated because an in-batch dependency failed.

Types Registry **MUST NOT** operate a global startup barrier. It **MUST** publish ready state once its own storage is ready, **MUST NOT** wait for any registrant, and has no notion of an expected startup set. A gear that registers definitions **MUST** retry failed registrations and **MUST NOT** publish its own ready state until its own registrations have succeeded; admission that fails because a base or referenced definition is not yet registered **MUST** be retryable and **MUST** succeed once that definition exists.

A reference cycle spanning two owners cannot be admitted, because neither owner can submit both members in one batch. This is intentional.

- **Rationale**: A gear can have interdependent definitions, including reference cycles, that cannot be admitted one at a time, while an unrelated invalid candidate should not prevent valid independent registrations. Separately, the registry cannot know the membership of a platform-wide startup set, and making its readiness depend on every registrant would put the slowest gear on the platform boot path.
- **Actors**: `cpt-cf-types-registry-actor-platform-gear`

#### Dry Run

- [ ] `p1` - **ID**: `cpt-cf-types-registry-fr-dry-run`

Every mutating operation — registration, deletion, and the purge of `cpt-cf-types-registry-fr-lifecycle` — **MUST** accept a Dry Run request. A Dry Run performs the complete check sequence the corresponding real operation performs and **MUST NOT** create a logical entity, allocate a content revision, move a current-revision pointer, advance a `resource_version`, change a Lifecycle Status, or remove anything. It **MUST** report, per candidate GTS Identifier, the outcome the real operation would have produced under the state observed during the run, with the same diagnostics.

Dry Run is a mode of the operation rather than a separate operation, and the reason is a correctness one rather than an economy of endpoints. A separate validation operation would be a second implementation of the ordered check sequence, and the two would drift; a drifted check that passes in CI and fails at admission is the one failure a pre-deployment gate exists to prevent. The mode is consequently orthogonal to the mutation kind rather than a member of it.

A Dry Run **MUST** use the same acceptance shape and authorization as the real operation it rehearses, whatever that shape is. For registration and deletion that is the asynchronous operation of ADR-0012, and there the Dry Run's request identity **MUST** additionally be distinguishable from the real operation's: the mode participates in the request fingerprint, so a Dry Run and a real submission carrying the same request key are different requests and the second **MUST NOT** be answered with the first's result. For the purge of ADR-0013 the shape is synchronous and stores no request identity, so that rule has nothing to apply to and re-running either form is harmless by construction. Authorization **MUST** be evaluated before identifier availability, exactly as `cpt-cf-types-registry-fr-registration-authority` requires of the real operation, so that a Dry Run cannot become an unauthorized probe of the GTS namespace. A Dry Run **MUST NOT** disclose anything about an entity outside the caller's visible scope that the real operation would not disclose.

When P2 owning-gear Validation Hooks exist, a Dry Run **MUST** invoke every hook the real operation would invoke, because a mode that skips them stops predicting admission and becomes misleading precisely where the stakes are highest. This is why a Dry Run of a registration or a deletion cannot be given a synchronous contract that the real operation lacks: hook duration is unbounded, and a response shape that held in P1 and changed in P2 would break the contract that `cpt-cf-types-registry-fr-two-phase-init` and ADR-0012 exist to keep stable. No hook applies to a purge, which is why its synchronous shape is stable rather than provisional — there is nothing that could later make its duration unbounded.

A successful Dry Run **MUST NOT** be presented as a guarantee of admission, and the contract **MUST** say so. Its verdict is computed against the state observed during the run: a target's `resource_version` may advance, a dependency may admit a new revision, or the entity may be deleted before the real submission, and each changes the outcome. A Dry Run also establishes only whether the operation would be accepted, and names what refused it. The wider set of dependents a change would affect without refusing it is deliberately not reported anywhere: `cpt-cf-types-registry-fr-ref-tracking` tracks it for admission decisions, and no requirement, actor, or use case asks to read it.

- **Rationale**: The checks a caller wants before deploying are exactly the checks admission performs, but admission commits when they pass. Under `cpt-cf-types-registry-fr-lifecycle` an admitted revision cannot be withdrawn — P1 has no rollback and the purge of ADR-0013 is disabled in production — so using a real registration as a test publishes the contract as a side effect of testing it. There is also a state in which the check is wanted and admission is impossible by construction: ADR-0003 freezes a logical entity whose revision chain spans a semantic change of the compatibility relation, and a compatibility check against it is still required to be answerable. Separately, because a registrant gates its own readiness on its registrations succeeding, an incompatible change discovered at admission is a failed rollout rather than a failed build.
- **Actors**: `cpt-cf-types-registry-actor-ci-pipeline`, `cpt-cf-types-registry-actor-gears-developer`, `cpt-cf-types-registry-actor-xaas-vendor-architect`, `cpt-cf-types-registry-actor-xaas-vendor-developer`, `cpt-cf-types-registry-actor-tenant-admin`

## 6. Non-Functional Requirements

> **Global baselines**: Project-wide architectural and quality baselines are defined in [docs/ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md), [guidelines/README.md](../../../../guidelines/README.md), and [ToolKit Unified System](../../../../docs/toolkit_unified_system/README.md). This section defines only Types Registry-specific NFRs.
>
> **Testing strategy**: NFRs are verified through automated benchmarks, integration tests, security checks, and monitoring as appropriate to the requirement.

### 6.1 Gear-Specific NFRs

#### Lookup Latency

- [ ] `p1` - **ID**: `cpt-cf-types-registry-nfr-lookup-latency`

The system **MUST** resolve an exact Managed Entity Registry Reference or GTS Identifier lookup within 10 ms at p95 under the supported production benchmark profile defined in DESIGN. For an Externally Managed Entity, the same threshold applies only to Types Registry federation and policy-processing overhead; Registry Source Plugin and External Registry Source execution time are governed by the source capability contract.

- **Threshold**: p95 < 10 ms for a managed exact lookup and p95 < 10 ms for Types Registry external-resolution overhead.
- **Rationale**: Registry resolving is used by gear startup and runtime paths.
- **Verification Method**: Automated benchmark against the versioned production benchmark profile defined in DESIGN.

#### Query Latency

- [ ] `p2` - **ID**: `cpt-cf-types-registry-nfr-query-latency`

The system **MUST** return bounded Managed Entity searches within 100 ms at p95 under the supported production benchmark profile defined in DESIGN. For federated searches, the same threshold applies only to Types Registry processing overhead; participating source execution time is governed by the source capability contracts.

- **Threshold**: p95 < 100 ms for a bounded managed search and p95 < 100 ms for Types Registry federated-search overhead.
- **Rationale**: Discovery and management views must remain responsive.
- **Verification Method**: Automated benchmark against the versioned production benchmark profile defined in DESIGN.

#### Multi-Pod Correctness

- [ ] `p1` - **ID**: `cpt-cf-types-registry-nfr-multi-pod-correctness`

The system **MUST** make every committed Managed Entity or Registry Source Plugin configuration mutation visible to every Types Registry pod after transaction commit. External entity consistency across plugin instances, pods, and data centers is governed by the Registry Source Plugin capability contract.

- **Threshold**: 100% of committed mutations are visible on every pod's first post-commit read.
- **Rationale**: Production deployments are horizontally scaled.

#### Cache Correctness

- [ ] `p1` - **ID**: `cpt-cf-types-registry-nfr-cache-correctness`

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

#### Registry Source Plugin SPI

- [ ] `p1` - **ID**: `cpt-cf-types-registry-interface-source-plugin`

- **Type**: Rust plugin trait and models, resolved through the ToolKit scoped ClientHub.
- **Stability**: unstable until first platform-stable release.
- **Description**: The contract Types Registry defines and a Registry Source Plugin implements: batch forward and reverse resolution, bounded candidate queries, tenant state, freshness and conditional reads, ownership assertions, and the effective artifacts of a claimed Type Schema kind. It is shaped for a remote counterparty although P1 plugins are in-process.
- **Breaking Change Policy**: Breaking changes allowed before first stable release; afterwards require a versioned contract, because a plugin is built and shipped separately from the registry.

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
- **Compatibility**: Tenant/global ownership checks must follow platform-level AuthN/AuthZ rules, and the two planes use different mechanisms rather than one mechanism with different inputs. Tenant-scoped registration authority is a PDP decision over the candidate GTS Identifier, expressed through the canonical permission GTS Type of `docs/arch/authorization/PERMISSION_GTS_TYPE.md`, whose `resource_type` field already accepts a GTS wildcard pattern. Global registration is a platform-plane operation under the two-plane model and `cpt-cf-adr-platform-plane-auth`, where the `PolicyEnforcer` is not on the path at all: the validated `PlatformIdentity` is the authorization, and per-workload narrowing, if a deployment wants it, is workload policy over that identity.

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

- [ ] `p1` - **ID**: `cpt-cf-types-registry-usecase-validate-type-evolution`

**Actor**: `cpt-cf-types-registry-actor-ci-pipeline`

**Preconditions**:
- A Type Schema change is proposed.

**Main Flow**:
1. CI submits the proposed Type Schemas as a Dry Run of the ordinary registration operation.
2. Types Registry performs the complete admission check sequence and commits nothing.
3. CI polls the operation and reads the per-GTS-ID outcome: the compatibility verdict against the current revision, the enforced mode, per-level evolvability, derivation results against every base in the chain, and any lifecycle or dependency conflict, together with the unproven-chain state when the entity is frozen.
4. CI reads the per-candidate diagnostics, which name the dependents a change would break; the Dry Run performs the same dependent revalidation admission does, so nothing further needs asking.
5. CI accepts or blocks the deployment based on those results.

**Postconditions**:
- Incompatible or unsafe type changes are detected before rollout, and nothing was published in the course of detecting them.

**Notes**:
- A passing Dry Run is not a guarantee of admission. The verdict is relative to the state observed during the run, and a target's `resource_version`, a dependency's current revision, or the entity's existence may change before the real submission.
- The comparison baseline is the current revision held by the installation the Dry Run ran against. Under `cpt-cf-types-registry-constraint-single-installation` two installations need not hold the same entities, so a green result against one environment does not establish acceptance in another.

## 9. Acceptance Criteria

- [ ] A caller-submitted batch is validated as one operation, a reference from one member to another resolves against the submitted candidate rather than against that identifier's previously committed revision, and every member carries an independent outcome keyed by its exact GTS Identifier.
- [ ] Independent candidates that pass every check are admitted even when another branch of the same batch fails; a candidate whose selected in-batch dependency failed creates nothing and is reported as failed under a reason distinct from that of a candidate rejected on its own merits; a cyclic dependency component is admitted atomically or not at all; and every failure identifies the offending members with diagnostics sufficient for correction and retry.
- [ ] Types Registry reaches ready state without waiting for any registrant, and a gear whose base definition is not yet registered fails admission, retries, and succeeds once that definition exists.
- [ ] Initial admission creates revision `1`, and each successfully admitted content update creates the next entity-scoped revision; pending, rejected, failed, and idempotent no-op candidates consume no revision and do not change the current revision.
- [ ] Lifecycle-only transitions do not create content revisions and become visible atomically with the corresponding cache freshness metadata.
- [ ] A Dry Run of a registration, a deletion, and a purge reports the same per-GTS-ID outcomes and diagnostics as the corresponding real operation, while leaving every logical entity, revision, current-revision pointer, `resource_version`, and Lifecycle Status untouched; a Dry Run against a frozen logical entity still returns the compatibility verdict together with the unproven-chain state.
- [ ] A Dry Run and a real submission carrying the same request key are treated as different requests, so the real submission executes rather than replaying the Dry Run's result; an unauthorized Dry Run is refused identically whether the candidate identifier is free or taken.
- [ ] A new platform GTS Type Schema can be introduced through Types Registry without each owning gear maintaining its own type registry.
- [ ] In P2, a matching required owning-gear Validation Hook can reject initial admission, a content revision, or deletion of a Managed Type Schema or registered Instance, while aliases, external entities, and tenant enablement do not invoke hooks.
- [ ] An externally managed entity can be discovered and resolved through Types Registry without direct dependency on its External Registry Source.
- [ ] Types Registry persists no Externally Managed Entity content or metadata projection, and no external identifier appears in any column of its storage; the owning plugin supplies forward/reverse resolution, querying, revisions, hashes, tombstones, lifecycle assertions, caches, and tenant state. There is no exception.
- [ ] A Managed Entity cannot reference or derive from an Externally Managed Entity, an externally managed entity cannot be served as derived from a managed base, and Types Registry exposes no plugin-callable operation that creates, modifies, or withdraws registry state.
- [ ] A managed entity referenced from inside an external schema document remains deletable, purgeable, and revisable with no block, no availability effect, and no revalidation, and no federation response validation parses returned content to detect the reference — the documented gap is exercised rather than assumed.
- [ ] A Source Claim is rejected at activation unless its pattern is a rooted single segment with the wildcard at a token boundary; a multi-segment pattern is refused; a claim also covers every identifier chained beneath what it covers, and registering a Managed Entity anywhere inside a claim is rejected as overlapping it.
- [ ] Managed storage is resolved first, non-overlapping Source Claims select external plugins, and unresolved Registry References are delegated in deterministic priority order.
- [ ] A Registry Source Plugin cannot activate a Source Claim for an entity kind unless it implements the complete P1 resolution, query, state, freshness, retention, and failure contract, and candidate query results contain no false negatives.
- [ ] No plugin capability is optional: every operation the P1 contract defines is required for each claimed entity kind, and no plugin output degrades with a warning in place of failing closed.
- [ ] Federated wildcard pages use deterministic source-major ordering and opaque cursors that become stale when plugin routing configuration changes.
- [ ] A P1 registry entity list or search operation returns a source failure and no result page when any selected Registry Source is unavailable or returns an invalid or incomplete response.
- [ ] Type query assistance returns a complete, deduplicated Concrete Reference Set; it never returns a partial or paginated constraint and reports a structured limit error when expansion is too broad.
- [ ] Tenant Availability State respects Lifecycle Status, availability-blocking registry relationships, and authoritative external tenant and source state. Managed tenant enablement policy becomes an additional input only when the post-P1 capability is introduced and is not part of this criterion.
- [ ] Admitting a managed higher-major Version Successor leaves every other member of its version family `ACTIVE`; several majors of one family can be active at once, members can be admitted in any order, and no P1 deprecation operation exists for a Managed Entity.
- [ ] Discovery can enumerate the members of a version family, and no operation reports which member is newest.
- [ ] No entity, managed or externally managed, is ever returned with Lifecycle Status `DEPRECATED` in P1; an externally managed entity whose source asserts deprecation is exposed as `ACTIVE` and remains resolvable, discoverable, and valid for both existing and newly admitted references.
- [ ] An entity transitions directly to terminal `DELETED` only when no live registered dependent exists, and the decision is reached from local state with every plugin unreachable; P1 has no restore and never reuses the GTS Identifier for a new logical entity outside the purge of ADR-0013.
- [ ] An exact read returns a deleted entity marked deleted and unavailable whether it was addressed by GTS Identifier or by Registry Reference, while discovery, search, and query assistance omit it; an identifier that never existed and one outside the caller's visible scope are both reported not found, indistinguishably from each other.
- [ ] A batch read reports source unavailability against the affected key as a failure distinct from not found, and answers the remaining keys normally; a list or search over the same unavailable source returns no page at all.
- [ ] Domain gears can use stable registry references and resolve user-facing GTS Identifiers, compatible-version filters, and wildcard patterns through Types Registry; P2 adds Alias-aware resolving without changing the P1 reference contract.
- [ ] A tenant-owned entry can be discovered, resolved, and used by its owning tenant and descendant tenants, is not disclosed to tenants outside that Tenant Subtree, and can reference visible global entries.
- [ ] No tenant-plane read result carries an owning tenant identifier; a tenant caller can determine only whether an entry is its own, and discovery rejects a supplied tenant identifier while accepting an ownership-scope selector.
- [ ] A platform-plane read returns entries owned by tenants outside any single subtree without disclosing which tenant owns any of them; it returns a Tenant Availability verdict only when a Context Tenant is named and omits the field otherwise; and it cannot create a tenant-owned entry under any grant.
- [ ] Naming a descendant as Context Tenant returns the availability verdict for that descendant while the visible set stays the subject's own, so an entry owned by the named descendant is still not disclosed to its ancestor; naming a tenant that is not a descendant is refused by the PDP.
- [ ] An Externally Managed Entity is visible to the Tenant Subtree of the tenant its source names; a plugin response omitting the ownership scope or naming an unknown tenant is rejected rather than exposed; and a plugin-side check can hide an entity from a caller but cannot reveal one that platform policy refused.
- [ ] A read result distinguishes a Managed from an Externally Managed entity, so a consumer can tell which guarantees stand behind a returned resolved schema; discovery can filter on that distinction, and a query restricted to Managed Entities succeeds while every Registry Source Plugin is unreachable.
- [ ] A tenant-plane request cannot register, revise, or delete a global entity under any grant, and global registration succeeds only on the platform plane with `PlatformSecurityContext`.
- [ ] A tenant-plane candidate matching `gts.cf.toolkit.*` is refused whether or not a covering grant exists, whether or not the identifier is free, and whether the candidate is a base type, a type derived beneath one, or an Instance of either — the refusal is exercised with a grant deliberately configured to cover the region, and it occurs before the PDP is consulted.
- [ ] The owner of an entity admitted on the tenant plane equals the requesting tenant of its `SecurityContext`, and a request body naming an owning tenant or selecting the global scope is rejected rather than honoured.
- [ ] A tenant-scoped registration whose candidate GTS Identifier is not covered by a grant held by the requesting subject is refused, and is refused identically whether the identifier is free, held by a visible entity, held by an invisible one, or held by a tombstone or Source Claim reservation.
- [ ] A subject granted a GTS pattern covering one vendor prefix can register inside it and cannot register outside it; being first to attempt an identifier confers no authority.
- [ ] Authorization is evaluated before identifier availability, proven by an unauthorized caller being unable to distinguish a free identifier from a taken one across repeated attempts.
- [ ] A batch is refused unless every member is covered by a grant within the single authorization scope that bounds the batch.
- [ ] A Type Schema revision is checked against the current admitted revision only; admission cost does not grow with the number of retained revisions, and a revision that drops a property cannot be followed by one that reintroduces it under a different schema.
- [ ] Compatibility results expose the enforced mode, the verdict, and whether the Type Schema is evolvable in place; no operational claim about producers, readers, casting, or default materialization is presented as a schema-compatibility result.
- [ ] A content revision of an unstable Type Schema is admitted whether it narrows, widens, is incomparable to the current revision, or cannot be decided at all, while the same revision of a stable Type Schema is rejected; an unstable candidate that violates its base chain or declares a dialect other than Draft-07 is still rejected.
- [ ] A stable Type Schema carrying a `$ref` or `x-gts-ref` to an unstable target is rejected, as is a stable identifier deriving from an unstable base, with diagnostics naming the offending member of the closure; the reverse direction is admitted, so an unstable Type Schema may build on stable ones.
- [ ] A registered Instance whose own last identifier segment carries major version 0 is rejected, and so is one conforming to an unstable Type Schema.
- [ ] Registering the first stable member of a family whose unstable member is `ACTIVE` succeeds, leaves the unstable member `ACTIVE`, and is refused for any owner other than the family's; a read of an unstable entity reports no enforced mode and no established chain state rather than a bare compatibility verdict.
- [ ] A managed Type Schema candidate declaring a dialect other than Draft-07, carrying no top-level `$schema`, or carrying a divergent `$schema` below its root is rejected at admission; a candidate pair differing only in declared dialect is rejected rather than compared for compatibility; and no column of registry storage holds a declared dialect.
- [ ] An externally managed entity declaring a non-Draft-07 dialect resolves and is returned without objection, and no federation response validation reads `$schema` from returned content.
- [ ] A read supplying a validator obtained from an earlier read of the same entity under the same projection reports the result unchanged and transfers no payload, and reports it changed after any mutation that invalidates it — including one that advances no `resource_version`, such as a recomputed effective schema or a tenant availability verdict that moved on its own.
- [ ] A batch read carries a validator per requested item and returns payloads only for the items that changed; a validator issued under a different projection produces a full result rather than a false unchanged; and an entity whose current state cannot be established returns a failure rather than unchanged.
- [ ] A conditional read of an Externally Managed Entity is answered through the owning Registry Source Plugin's conditional-read capability, so one caller loop covers managed and externally managed entities without branching.
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
- External Registry Sources remain authoritative for externally managed entities. Their plugins own external definitions, identifiers, Registry Reference mappings, revisions, queries, source-side dependency data, caches, tombstones, lifecycle assertions, and tenant state, while regular gears access them only through Types Registry. There is no exception and no plugin write path: an External Registry Source serves a self-contained type universe that neither depends on nor is depended upon by Managed Entities.
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
| An External Registry Source references a managed contract from inside its own schema, which the platform can neither prevent nor detect | The managed contract can be deleted, purged, or revised without any block, availability signal, or revalidation, and the external entity breaks with no registry event | Accepted and documented rather than mitigated: the integration contract states that no dependency guarantee crosses the boundary, and a vendor that needs to build on a platform contract registers the derived type as a Managed Entity instead. Detection by parsing external content is rejected by `cpt-cf-types-registry-fr-externally-managed-entities` |
| Federated pagination is unstable across plugin changes | Clients see duplicates, gaps, or inconsistent source ordering | Use source-major ordering and bind opaque cursors to a plugin configuration revision |
| A production consumer depends on an unstable Type Schema, whose owner then reshapes it | Stored domain data stops conforming to its own type, with no registry event and no compatibility verdict to have warned anyone | Partly mitigated and partly accepted. The quarantine rule keeps the risk out of every stable contract, and the identifier makes it legible to anyone who reads a reference. What P1 cannot do is refuse the dependency on the read path: managed tenant enablement is a P2 capability, and the grant model governs writing rather than reading. The residual exposure is no larger than the one `cpt-cf-types-registry-fr-lifecycle` already accepts, since deletion of a stable type under live conforming data is permitted and is strictly more destructive |

## 13. Open Questions

Decisions that are deliberately not settled by this PRD. Each is recorded in a design note or ADR where one already discusses it; this table exists so that a reader of the PRD alone does not mistake the surrounding requirements for a closed set. A question leaves this table once its answer has a home — in a requirement above, in DESIGN, or in an ADR.

Entry numbers are stable and are never reused, so a gap marks a closed question rather than a missing one.

This table holds unresolved **requirements** — scope, policy, and what the product owes. Unmade **design** decisions live in [DESIGN §4, Open questions](./DESIGN.md#open-questions), numbered `D1`, `D2`, … Everything there is P2 by rule: a design question P1 depends on is a blocker rather than a note. A question moves from here to there once what remains of it is a construction decision.

| # | Question | Affects | Recorded in |
|---|----------|---------|-------------|
| 1 | **Who marks an entity deprecated, at what moment, and what the mark affects.** ADR-0008 settled that deprecation is authored rather than derived from publishing a successor, and deferred it for want of a named consumer. What remains unsettled is the requirement itself: which actor may deprecate, whether the act is tied to any other event or is purely discretionary, what a consumer is expected to do on seeing the mark given that a deprecated entity stays fully usable, and whether, **once the concept exists**, a deprecation asserted by an External Registry Source is relayed or stays a managed-only concept. The P1 half of that last point is closed and is not reopened here: `cpt-cf-types-registry-fr-lifecycle` requires such an entity to be exposed as `ACTIVE` and the assertion not to be relayed | `cpt-cf-types-registry-fr-lifecycle` | ADR-0008 |
| 2 | **What the platform must expose about federation failures and about its own mutations.** Federation fails closed, so a source outage surfaces as a failed operation rather than a degraded result — and nothing yet requires the platform to say which source failed, to which actor, or how a chronically unhealthy source becomes visible to an operator rather than only to the caller whose request it broke. Naming the source to a tenant caller is itself a disclosure decision. Separately, §4.2 keeps a general-purpose audit product out of scope while still requiring operation and audit records for registry mutations, without stating what such a record must contain, who may read it, or how long it is kept | all federation requirements, `cpt-cf-types-registry-fr-registry-source-routing` | [registry-federation-external-sources.md](./design-notes/registry-federation-external-sources.md) (draft) |
| 3 | **Which transitions casting must support.** GTS OP#9 defines casting only between compatible minor versions, and ADR-0004 leaves a managed identifier with no minor version at all, so the one transition the specification defines does not exist in the managed profile. The candidates that remain are each doubtful for a different reason: two majors of one version family are incompatible by construction, since publishing a new major is how an incompatible change is expressed; two content revisions of one logical entity are not addressable by a consumer, because ADR-0005 keeps revision numbers out of the contract; and a derived type and its base need no transformation, because Type Derivation Compatibility already makes every derived instance valid against the base. Until this is settled, `cpt-cf-types-registry-fr-casting` states an obligation with no defined reach | `cpt-cf-types-registry-fr-casting` | this PRD, section 5.3 |
| 4 | **Whether it is acceptable for a tenant's move within the hierarchy to change what it can see and who can see what it owns.** Relocation changes the visible audience in both directions, so contracts a tenant was using may stop being available to it, and contracts it published may reach a different set of descendants — in both cases with no registry mutation behind the change and no event a consumer could have observed. The requirement question is whether that is an acceptable outcome of an account-management operation, and, if it is not, what the registry owes: refusing the move, reporting the affected entities, or migrating something. Out of scope for P1 per ADR-0009 | `cpt-cf-types-registry-fr-tenant-ownership`, `cpt-cf-types-registry-fr-tenant-availability` | ADR-0009; the design consequence of bringing it into scope is recorded in ADR-0010 |
| 5 | **How the entities of an offboarded tenant are retired.** The answer turns on account-management semantics the registry does not own — what offboarding means, whether the tenant record survives it, and whether any other tenant inherits the departing tenant's authority — so it cannot be settled here alone. What the registry contributes is the constraint: ordinary deletion belongs to the owning tenant, ADR-0013 requires an entity to be `DELETED` before it can be purged, and the platform plane cannot author in a tenant's place, so once the owner is gone nobody can retire its entries. The answer must also decide what happens to dependents in other tenants that reference the departing tenant's contracts | `cpt-cf-types-registry-fr-tenant-ownership`, `cpt-cf-types-registry-fr-lifecycle` | not yet recorded; adjacent to entry 4 |
| 6 | **Whether an Alias may target another Alias, and whether an admitted Alias may be retargeted.** Both are P2 obligations the product either owes or does not, and neither follows from the Alias identity model: ADR-0001 already gives an Alias its own Registry Reference, which settles identity and settles neither of these. Chaining makes the Alias-to-target relation transitive, and ADR-0010 classifies that relation as availability-blocking, so a chain propagates unavailability along its whole length. Retargeting is the sharper one, because it changes what an already issued Registry Reference resolves to — the effect `cpt-cf-types-registry-principle-permanent-identity` otherwise permits only under the purge of ADR-0013 — so allowing it means deciding deliberately that an Alias reference carries a weaker permanence guarantee than an entity reference, and saying so in the contract. What an Alias resolution returns once these are answered is a construction decision and is DESIGN D2 | `cpt-cf-types-registry-fr-aliasing`, `cpt-cf-types-registry-fr-id-resolution` | ADR-0001; DESIGN D2 holds the remaining construction half |

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/)

## 15. References

- **GTS spec**: [Global Type System](https://github.com/globaltypesystem/gts-spec)
- **ToolKit**: [docs/toolkit_unified_system/README.md](../../../../docs/toolkit_unified_system/README.md)
- **ToolKit plugins**: [docs/TOOLKIT_PLUGINS.md](../../../../docs/TOOLKIT_PLUGINS.md)
