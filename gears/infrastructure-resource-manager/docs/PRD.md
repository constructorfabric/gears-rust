---
refs: []
---

# PRD — Infrastructure Resource Manager (IRM)


<!-- toc -->

- [Document Information](#document-information)
  - [Change Log](#change-log)
- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Architecture Alignment](#2-architecture-alignment)
- [3. Actors](#3-actors)
  - [3.1 Human Actors](#31-human-actors)
  - [3.2 System Actors](#32-system-actors)
- [4. Operational Concept & Environment](#4-operational-concept--environment)
  - [4.1 Module-Specific Environment Constraints](#41-module-specific-environment-constraints)
- [5. Scope](#5-scope)
  - [5.1 In Scope](#51-in-scope)
  - [5.2 Out of Scope](#52-out-of-scope)
- [6. Functional Requirements](#6-functional-requirements)
  - [6.1 Type System and Adapters](#61-type-system-and-adapters)
  - [6.2 Resource Lifecycle](#62-resource-lifecycle)
  - [6.3 Declarative Deployments and Reconciliation](#63-declarative-deployments-and-reconciliation)
  - [6.4 Lifecycle Actions](#64-lifecycle-actions)
  - [6.5 Relationships and Topology](#65-relationships-and-topology)
  - [6.6 Discovery and Inventory](#66-discovery-and-inventory)
  - [6.7 Resource Groups and Organization](#67-resource-groups-and-organization)
  - [6.8 Governance and Security](#68-governance-and-security)
  - [6.9 API Contract and Platform Hardening](#69-api-contract-and-platform-hardening)
- [7. Non-Functional Requirements](#7-non-functional-requirements)
  - [7.1 NFR Inclusions](#71-nfr-inclusions)
  - [7.2 NFR Exclusions](#72-nfr-exclusions)
- [8. Five Quality Vectors Analysis](#8-five-quality-vectors-analysis)
- [9. Public Library Interfaces](#9-public-library-interfaces)
  - [9.1 Public API Surface](#91-public-api-surface)
  - [9.2 External Integration Contracts](#92-external-integration-contracts)
- [10. Use Cases](#10-use-cases)
- [11. User Interaction and Design](#11-user-interaction-and-design)
- [12. Acceptance Criteria](#12-acceptance-criteria)
  - [Governance Cross-Cut](#governance-cross-cut)
  - [Type System and Adapters](#type-system-and-adapters)
  - [Declarative Change Management](#declarative-change-management)
  - [Day-2 and Topology](#day-2-and-topology)
  - [Discovery](#discovery)
  - [Placement and Groups](#placement-and-groups)
  - [Authorization Granularity](#authorization-granularity)
  - [Destructive Operations](#destructive-operations)
  - [Type System and Adapter Onboarding](#type-system-and-adapter-onboarding)
  - [Resource and Deployment Lifecycle](#resource-and-deployment-lifecycle)
  - [Access Control](#access-control)
  - [Secret Handling](#secret-handling)
  - [Adapter Trust Boundary](#adapter-trust-boundary)
  - [Placement Propagation](#placement-propagation)
  - [Destructive Operation Disclosure](#destructive-operation-disclosure)
  - [Discovery and Inventory](#discovery-and-inventory)
  - [Capabilities and Tags](#capabilities-and-tags)
  - [Retention and Accounting](#retention-and-accounting)
  - [History and Migration](#history-and-migration)
  - [Dependency Unavailability](#dependency-unavailability)
  - [Non-Functional Requirements (Show-Stoppers)](#non-functional-requirements-show-stoppers)
- [13. Dependencies](#13-dependencies)
- [14. Assumptions](#14-assumptions)
- [15. Risks](#15-risks)
- [16. Open Questions](#16-open-questions)
- [17. Reference Materials](#17-reference-materials)
- [18. Traceability](#18-traceability)

<!-- /toc -->

## Document Information

| **Field** | **Value** |
|-----------|----------|
| **Version** | 1.0.0 |
| **Last review** | 2026-08-05 |
| **Target release** | To be set — project planning tracks delivery sequencing |
| **Document status** | DRAFT — open for contributor review |
| **Lifecycle position** | First artifact for the component — no implementation exists yet. A technical design follows from this PRD. Implementation follows the design. |
| **Self-containment** | This document states every requirement, term, actor, limit, and acceptance criterion in full. Reviewers need no other document. |

### Change Log

| **Date** | **Version** | **Change** |
|----------|-------------|------------|
| 2026-07-31 | 1.0.0 | Initial component PRD, opened for contributor review. It consolidates the platform's earlier IRM requirement material into one self-contained specification. |

## 1. Overview

### 1.1 Purpose

The Infrastructure Resource Manager (IRM) is the central orchestration layer for all infrastructure and application resources on the platform. It provides one consistent management surface and type system for every resource class. It provides a declarative deployment model with safe reconciliation (diff, preview, apply, rollback). It provides day-2 lifecycle actions and automated discovery of existing estates. It provides a virtual resource graph for topology and dependency analysis, and a scope hierarchy for governance and multi-tenancy.

This document is the single requirements source for IRM. It describes what IRM must do, not how to build it. The technical design that follows will settle the how.

### 1.2 Background / Problem Statement

Resource management on the platform was historically fragmented. Multiple interfaces had inconsistent behavior per resource class. Provisioning was manual and error-prone. There was no desired-state tracking (environments diverge silently). Policy enforcement was inconsistent, and there were audit gaps. Every integration required custom work.

IRM addresses this with a single governed management surface. A registered type describes every resource class. IRM classifies each change and makes it previewable before it happens. IRM records every successful change as an immutable revision that can be rolled back. Every operation is tenant-scoped, policy-gated, and audited.

### 1.3 Goals (Business Outcomes)

- Single pane of glass: one API surface and one graph model across virtualization, multi-cloud, and on-prem resources.
- Zero-surprise changes: every apply is previewable, deterministic, reversible, and duplicate-safe.
- Governance built in: tenant isolation, role-based access, policy and quota gating, and complete audit on every operation.
- Ecosystem and revenue: a versioned resource type registry and adapter model let third parties integrate without core changes. Per-resource attribution enables usage-based billing.
- Less manual work: automated discovery, standardized day-2 actions, and desired-state reconciliation replace manual provisioning.

Each goal is tracked by a metric with a defined data source. Baselines and targets are open (§16).

| **Metric** | **Data Source** |
|------------|------------------|
| Single pane: % of resource classes managed through the IRM API | Type registry |
| Zero-surprise: % of applies preceded by a preview | Audit events |
| Governance: % of operations with complete audit correlation; audit-gap count | Audit sink |
| Ecosystem: third-party adapters onboarded without core changes | Adapter registry |
| Less manual: share of the estate adopted through discovery versus manual re-creation | Discovery jobs |

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Resource | A managed infrastructure or application entity (VM, volume, network, container, service) with a type, properties, outputs, and lifecycle state. |
| Resource Type | Versioned schema defining a class of resources (properties, actions, capabilities), registered in the type registry under a GTS identifier. |
| GTS | Global Type System — platform-wide identifier and versioning scheme for types and events. |
| Adapter | Deployable component that manages resources for a specific provider or backend. It registers resource types and executes provisioning, reads, and deletes. |
| Deployment | Declarative definition of a set of resources with parameters, variables, dependencies, and outputs. It is the unit of apply, history, and rollback. |
| Anonymous Deployment | Implicit single-resource deployment that IRM auto-creates when a caller manages a resource directly. |
| Diff Engine | Deterministic reconciler that classifies each resource change as one of five operations (no change, create, update, replace, delete) and determines the execution order that the workflow executor carries out. |
| Plan | In-memory result of diff classification, bound to its inputs by a canonical fingerprint. A dry-run previews it, and IRM re-validates it at apply. |
| Revision | Immutable record of a successful apply. It is the authoritative target for history and rollback. |
| Lineage | Identity thread that survives resource replacement, keeping history and rollback reachable across re-provisioning. |
| Management Policy | Per-resource protection level: **full**, **no-delete** or **no-touch**. |
| Capability | Optional feature (backup, monitoring, encryption) that a resource type offers. It is enabled and configured per resource instance. |
| Virtual Resource Graph | Typed nodes (resources) and edges — ownership (parent-child), dependency, attachment — that support traversal, impact analysis, and visualization. |
| Discovery | Automated detection and synchronization of existing provider resources into IRM inventory. |
| Resource Group | Lifecycle container for related resources. Every resource belongs to exactly one group within the scope hierarchy. |
| Tenant | Isolated customer or organizational boundary. All IRM operations carry tenant context. |
| Deployment Address | The tuple (tenant, resource group, deployment name) that uniquely identifies a deployment. |
| Default Group | Per-tenant resource group that IRM uses as the implicit placement when the caller gives none. |
| Membership Convergence | Asynchronous reconciliation that propagates placement decisions to the resource-group service. |
| Adapter Package | Declarative package that describes an adapter, its resource types, data-plane operations, delegation scopes, and policy bundles. It is the single-call onboarding input. Requirement names use "manifest" as a synonym for this package. |
| Data-Plane Operation Catalog | Per-type registry of provider operations that IRM publishes so that capability grants can be issued and discovered. |
| Capability Token | Short-lived, single-purpose credential minted per outbound adapter call. |
| Owned Subtree | A parent resource together with the resources it owns. When the parent is deleted, IRM tears the subtree down to completion. |
| Trusted System Actor | The internal identity that IRM uses for its own maintenance work, clamped to the tenant being served. |
| SRE | Site Reliability Engineer — the operator persona that runs day-2 actions, rollbacks, and cleanup. |
| RBAC | Role-Based Access Control — the access model that the platform RBAC engine resolves. |
| IdP | Identity Provider — the platform component that authenticates callers and supplies subject identity. |
| AM | Account Management — the platform component that owns tenants and accounts. |
| Policy Decision Service | Abstract decision-service role that answers admission, policy, quota, and license-entitlement questions fail-closed. A capability, not a component; which engine implements it is a design decision. |
| IRM | The short name for the Infrastructure Resource Manager gear, used throughout this document. |

## 2. Architecture Alignment

| **Field** | **Value** |
|-----------|----------|
| **Applicable Manifest(s)** | Not referenced — this PRD is self-contained. The rows below state the architecture context in its own terms. |
| **Platform position** | IRM is the resource-management component of the platform's operations layer. It owns resource types and adapters, resource lifecycle, declarative deployments and reconciliation, day-2 actions, resource relationships, discovery, and the scope hierarchy. |
| **Adjacent components** | Separate platform components own policy decisions, role definitions, identity, durable execution, durable storage, resource-group membership, and event and audit delivery. §3.2 declares each as a system actor. §13 records the criticality of each. |
| **Deliberate scope decision** | Continuous drift detection and reconciliation loops are **not** an IRM responsibility in this scope. Adapters own continuous reconciliation. IRM provides on-demand refresh and preview (§5.2). §16 records the question of revisiting this. |
| **Deliberate scope decision (multi-region)** | Multi-region management is out of scope for this release (§7.2), but it is a known platform direction: the technical design **MUST NOT** preclude a later placement dimension (such as a region) in deployment addressing, identifiers, or group semantics. §16 records the question. |
| **Safety applicability** | Safety (ISO/IEC 25010 §4.2.9) is not applicable: IRM is a control plane for IT resources operated through API and CLI; it does not actuate physical equipment. Destructive-operation risk to managed infrastructure is governed by `cpt-cf-infrastructure-resource-manager-fr-guardrails`, `cpt-cf-infrastructure-resource-manager-fr-cascade-admission`, `cpt-cf-infrastructure-resource-manager-fr-cascade-disclosure`, and `cpt-cf-infrastructure-resource-manager-fr-operation-cancel`. |
| **Recorded platform conventions** | CloudEvents (event-broker ADR-0003) for the event envelope. RFC 9457 (ToolKit `05_errors_rfc9457.md`) for error responses. The Idempotency-Key header (toolkit-http) for duplicate-safe mutations. CEL (quota-enforcement / serverless-runtime precedents) for declarative expressions. AuthZEN-based authorization resolution (authz-resolver, `docs/arch/authorization/`) for access decisions. |
| **IRM-level recorded choices** | UUID v7 (RFC 9562) for time-sortable identifiers. Salted per-tenant digests for secret-field change detection without cleartext exposure. A canonical plan fingerprint that binds an apply to the exact inputs it was previewed against. |

## 3. Actors

All human actors are technical professionals who work through the API, CLI, and operator consoles. The interfaces optimize for expert efficiency and scriptability rather than first-use guidance. Business sponsors are stakeholders, not actors: their needs are expressed through the §1.3 goals and metric table, not through a dedicated actor entry here.

### 3.1 Human Actors

#### Platform Engineer

**ID**: `cpt-cf-infrastructure-resource-manager-actor-platform-engineer`

**Role**: Defines and maintains resource types and platform standards. Operates the management surface for automation and CI/CD integration.
**Needs**: One consistent, versioned interface for every resource class. Deterministic previews before changes reach production.

#### Automation Engineer

**ID**: `cpt-cf-infrastructure-resource-manager-actor-automation-engineer`

**Role**: Builds integrations and self-service automation on top of IRM. Registers custom resource types for internal or partner use.
**Needs**: Stable contracts, idempotent operations, and machine-readable previews and results.

#### SRE / Operator

**ID**: `cpt-cf-infrastructure-resource-manager-actor-sre-operator`

**Role**: Troubleshoots resources, executes day-2 actions, performs rollbacks and orphan cleanup during incident response.
**Needs**: Reliable history of what changed and when. Safe rollback. Visibility into detached and degraded resources.

#### System Administrator

**ID**: `cpt-cf-infrastructure-resource-manager-actor-system-administrator`

**Role**: Plans maintenance and capacity using the resource topology. Administers discovery across providers.
**Needs**: Accurate dependency graph. Discovery health status. Control over discovery scheduling and failure handling.

#### Tenant Administrator

**ID**: `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`

**Role**: Manages the tenant's resource estate: hierarchy, resource groups, tags, quotas, and access for tenant users.
**Needs**: Strict isolation of the tenant's data. Scope-level access control. Clear quota and policy feedback.

#### Adapter Developer

**ID**: `cpt-cf-infrastructure-resource-manager-actor-adapter-developer`

**Role**: Authors adapters for providers (virtualization platforms, public clouds, custom backends). Registers adapters and their resource types. Onboarding requests enter IRM inbound through the management API.
**Needs**: A provider-agnostic contract that requires no IRM core changes. A controlled onboarding lifecycle.

### 3.2 System Actors

#### Infrastructure Adapter

**ID**: `cpt-cf-infrastructure-resource-manager-actor-infrastructure-adapter`

**Role**: Executes provisioning, updates, reads, and deletes against the concrete provider. Supplies discovery inventory and health signals. Semi-trusted: IRM validates responses and bounds their size.
**Direction**: Outbound — IRM invokes the adapter.

#### Policy Decision Service

**ID**: `cpt-cf-infrastructure-resource-manager-actor-policy-engine`

**Role**: Evaluates admission, policy, quota, and license-entitlement decisions for IRM operations. Decisions are fail-closed. This is an abstract decision-service role: IRM depends on the capability, not on a concrete component. Which engine implements it — including the runtime that evaluates adapter-registered policy bundles — is a design decision (§16).
**Direction**: Outbound — IRM requests decisions.

#### AM and IdP

**ID**: `cpt-cf-infrastructure-resource-manager-actor-identity-provider`

**Role**: Account Management (AM) and the Identity Provider (IdP) authenticate callers and supply subject identity and tenant context.
**Direction**: Inbound — identity and tenant context arrive with each request.

#### Workflow Executor

**ID**: `cpt-cf-infrastructure-resource-manager-actor-workflow-executor`

**Role**: Durable execution substrate for long-running operations (apply, actions, discovery). It survives process crashes and resumes work.
**Direction**: Bidirectional — IRM dispatches work outbound. Status callbacks arrive inbound.

#### Event & Audit Consumers

**ID**: `cpt-cf-infrastructure-resource-manager-actor-event-consumer`

**Role**: Downstream systems (audit sink, billing, notification, graph projections) that consume IRM domain and audit events.
**Direction**: Outbound — IRM publishes.

#### Resource Group Service

**ID**: `cpt-cf-infrastructure-resource-manager-actor-resource-group-service`

**Role**: Owns groups and their membership. IRM validates group references against it, propagates membership asynchronously, and treats it as the authorization truth that the policy decision service reads.
**Direction**: Outbound — IRM validates references and writes membership.

#### Trusted System Actor

**ID**: `cpt-cf-infrastructure-resource-manager-actor-system-trusted`

**Role**: The internal identity under which IRM performs its own maintenance work. This work includes creating a tenant's default group, converging membership, repairing drift, and registering its types at start-up. Every elevation is clamped to the tenant being served and is individually attributable.
**Direction**: Internal — not an external integration.

#### Grant Issuance Service

**ID**: `cpt-cf-infrastructure-resource-manager-actor-grant-service`

**Role**: Consumes the data-plane operation catalog and resource resolution that IRM publishes, to issue and scope capability grants for direct data-plane access.
**Direction**: Inbound — the service calls the IRM catalog and resolution APIs.

#### RBAC Engine

**ID**: `cpt-cf-infrastructure-resource-manager-actor-rbac-engine`

**Role**: Resolves role definitions and scope-based access. The caller's effective authority is compiled from it.
**Direction**: Outbound — IRM consults it through the platform authorization resolution path.

#### Type Identifier Service

**ID**: `cpt-cf-infrastructure-resource-manager-actor-type-identifier-service`

**Role**: Allocates and resolves platform-wide type identifiers. IRM registers its schemas and per-type authorization identities with it.
**Direction**: Outbound — IRM registers and resolves identifiers.

#### Token Issuer

**ID**: `cpt-cf-infrastructure-resource-manager-actor-token-issuer`

**Role**: Mints the per-call capability tokens that IRM attaches to outbound adapter traffic.
**Direction**: Outbound — IRM requests a token before each adapter call.

#### Persistence Layer

**ID**: `cpt-cf-infrastructure-resource-manager-actor-persistence`

**Role**: Durable storage substrate providing atomic reservations, consistency guards, and cursor pagination.
**Direction**: Outbound — IRM reads and writes through the platform data layer.

## 4. Operational Concept & Environment

### 4.1 Module-Specific Environment Constraints

- IRM is pre-GA: one-time breaking migrations (event namespace normalization to the platform vendor token, migration to the deployment-scoped resource model) MAY be executed without a dual-publish compatibility window.
- Deployment and release mechanics (build, packaging, rollout) are owned by the platform ToolKit lifecycle, not by this PRD.
- This PRD imposes no further runtime, OS, or lifecycle constraints. The technical design settles anything beyond the NFRs in §7.

## 5. Scope

### 5.1 In Scope

Priority below is the strongest requirement priority within the row, on the same `p1`–`p4` scale that §6 and §7 use. `p1` must ship — the capability already exists in a reference implementation. `p2` should ship — planned. `p3` is deferred — blocked on a platform dependency that is not yet available. `p4` is unused.

| **#** | **Feature** | **Priority** | **Requirements** | **Notes** |
|-------|-------------|--------------|------------------|-----------|
| 1 | Unified resource lifecycle management | p1 | `cpt-cf-infrastructure-resource-manager-fr-resource-crud`, `cpt-cf-infrastructure-resource-manager-fr-lifecycle-states` | One consistent interface for create, read, update, delete across all resource types. Scoped listing with filtering and pagination. |
| 2 | Resource type registry (GTS) | p1 | `cpt-cf-infrastructure-resource-manager-fr-type-registry`, `cpt-cf-infrastructure-resource-manager-fr-type-evolution` | Register, version, query, and retire resource types with schemas, actions, and capabilities. IRM rejects invalid definitions. |
| 3 | Adapter onboarding and registry | p1 | `cpt-cf-infrastructure-resource-manager-fr-adapter-onboarding`, `cpt-cf-infrastructure-resource-manager-fr-adapter-health`, `cpt-cf-infrastructure-resource-manager-fr-adapter-retirement` | Adapter registration lifecycle (pending → active), type contribution, and health visibility. Activation requires at least one registered type. |
| 4 | Resource capabilities | p2 | `cpt-cf-infrastructure-resource-manager-fr-capabilities` | Optional per-resource features (backup, monitoring, encryption): discover, enable, configure, disable. These operations are fully audited. |
| 5 | Declarative deployments | p1 | `cpt-cf-infrastructure-resource-manager-fr-declarative-definitions`, `cpt-cf-infrastructure-resource-manager-fr-conditional-resources`, `cpt-cf-infrastructure-resource-manager-fr-parameters` | Multi-resource definitions with parameters, variables, dependencies, outputs, dynamic expressions, and conditional inclusion. Validation returns actionable errors. |
| 6 | Change classification (diff engine) | p1 | `cpt-cf-infrastructure-resource-manager-fr-change-classification` | Five-operation classification per resource (no change / create / update / replace / delete) driven by type metadata (immutable, computed, secret fields). |
| 7 | Preview (dry-run) | p1 | `cpt-cf-infrastructure-resource-manager-fr-preview` | Human-readable and machine-readable preview of every planned change with zero side effects. Secrets are always redacted. |
| 8 | Plan binding and concurrency safety | p1 | `cpt-cf-infrastructure-resource-manager-fr-plan-binding` | Apply executes exactly the previewed change, or IRM rejects the apply when definition, state, or type metadata drifted since preview. |
| 9 | Ordered durable execution | p1 | `cpt-cf-infrastructure-resource-manager-fr-ordered-execution`, `cpt-cf-infrastructure-resource-manager-fr-deployment-status` | Dependency-ordered execution, parallel only where no dependency relates, durable and resumable, with compensation on failure. |
| 10 | Replacement strategies | p1 | `cpt-cf-infrastructure-resource-manager-fr-replace-strategies` | Delete-before-create (default) and create-before-destroy per type with per-resource override. IRM re-wires dependent resources safely. |
| 11 | Management policy | p1 | `cpt-cf-infrastructure-resource-manager-fr-guardrails` | One per-resource protection mechanism with three levels (full / no-delete / no-touch). no-delete detaches the provider object instead of destroying it. |
| 12 | Duplicate-safe writes (idempotency) | p1 | `cpt-cf-infrastructure-resource-manager-fr-idempotent-writes` | Mandatory caller-supplied idempotency keys make resource and deployment mutations safely retryable with verbatim replay. `cpt-cf-infrastructure-resource-manager-fr-idempotent-writes` lists the operations exempt by construction. |
| 13 | Revisions, history, rollback | p1 | `cpt-cf-infrastructure-resource-manager-fr-revisions-history`, `cpt-cf-infrastructure-resource-manager-fr-rollback` | Immutable revision per successful apply. Unified chronological history. Multi-selector rollback as a fresh reconciliation. Lineage survives replacement. |
| 14 | Refresh of actual state | p1 | `cpt-cf-infrastructure-resource-manager-fr-refresh` | On-demand re-read of provider state to surface out-of-band changes before the next apply. |
| 15 | Soft-delete, retention, orphans | p1 | `cpt-cf-infrastructure-resource-manager-fr-soft-delete-retention`, `cpt-cf-infrastructure-resource-manager-fr-delete-uncertainty` | Tombstones with configurable retention and purge. Orphaned provider objects are first-class: visible, capped per tenant, operator-cleanable. |
| 16 | Secret hygiene | p1 | `cpt-cf-infrastructure-resource-manager-fr-secret-hygiene` | Secret values are never persisted or emitted in cleartext anywhere (state, history, previews, logs, events). Comparison detects a changed secret value without storing or exposing cleartext. |
| 17 | Lifecycle actions (day-2) | p2 | `cpt-cf-infrastructure-resource-manager-fr-action-framework`, `cpt-cf-infrastructure-resource-manager-fr-action-execution` | Provider-defined actions (start, stop, snapshot, resize, migrate) with state validation, asynchronous tracking, and full audit. |
| 18 | Virtual resource graph | p1 | `cpt-cf-infrastructure-resource-manager-fr-relationship-model`, `cpt-cf-infrastructure-resource-manager-fr-graph-query`, `cpt-cf-infrastructure-resource-manager-fr-relationship-backfill` | Typed relationships derived from resource data. Traversal and impact queries. Consistency maintenance (cascade, orphan-edge cleanup, migration archiving). |
| 19 | Topology data surface | p2 | `cpt-cf-infrastructure-resource-manager-fr-visualization` | Machine-readable topology surface for the frontend visualization: scoped queries, path computation, export. |
| 20 | Discovery and inventory | p2 | `cpt-cf-infrastructure-resource-manager-fr-discovery-jobs`, `cpt-cf-infrastructure-resource-manager-fr-discovery-sync`, `cpt-cf-infrastructure-resource-manager-fr-tenant-assignment`, `cpt-cf-infrastructure-resource-manager-fr-discovery-compliance` | Manual, scheduled, and event-driven discovery. Idempotent bulk sync. Stale handling. Tenant assignment with discovery pool. Circuit breaker. Non-blocking compliance flagging. |
| 21 | Resource groups | p1 | `cpt-cf-infrastructure-resource-manager-fr-resource-groups` | Scope model is tenant → resource group → resource. Every managed resource is in exactly one group. Group placement and move. IRM rejects deletion of non-empty groups. |
| 22 | Tags | p2 | `cpt-cf-infrastructure-resource-manager-fr-tags` | Key-value tags on groups and resources with downward inheritance. Filtering and policy targeting. |
| 23 | Governance cross-cut | p1 | `cpt-cf-infrastructure-resource-manager-fr-tenant-isolation`, `cpt-cf-infrastructure-resource-manager-fr-rbac`, `cpt-cf-infrastructure-resource-manager-fr-policy-gating`, `cpt-cf-infrastructure-resource-manager-fr-audit-events`, `cpt-cf-infrastructure-resource-manager-fr-admission-pipeline`, `cpt-cf-infrastructure-resource-manager-fr-data-classification` | Tenant isolation, role-based access, policy and quota gating, audit events with correlation on every operation. |
| 24 | Migration to the deployment-scoped model | p2 | `cpt-cf-infrastructure-resource-manager-fr-model-migration` | Per-tenant migration (expand → backfill → verify → contract) with abort and resume. Legacy data stays readable until completion. |
| 25 | Group placement and deployment addressing | p1 | `cpt-cf-infrastructure-resource-manager-fr-group-addressing`, `cpt-cf-infrastructure-resource-manager-fr-default-group`, `cpt-cf-infrastructure-resource-manager-fr-group-validation`, `cpt-cf-infrastructure-resource-manager-fr-deployment-scoped` | Deployment identity is (tenant, group, name). Submitting a definition creates-or-updates at that address. Default group is implicit when the caller gives none. |
| 26 | Explicit group move | p1 | `cpt-cf-infrastructure-resource-manager-fr-group-move`, `cpt-cf-infrastructure-resource-manager-fr-group-move-concurrency` | Relocating a deployment between groups is a separate, synchronous, optimistically-concurrent operation. Apply never moves anything. |
| 27 | Membership convergence and drift repair | p1 | `cpt-cf-infrastructure-resource-manager-fr-membership-convergence`, `cpt-cf-infrastructure-resource-manager-fr-membership-ordering`, `cpt-cf-infrastructure-resource-manager-fr-membership-durability`, `cpt-cf-infrastructure-resource-manager-fr-membership-failure-handling`, `cpt-cf-infrastructure-resource-manager-fr-placement-drift` | IRM commits placement locally and propagates it asynchronously with bounded staleness. A periodic sweep reconciles out-of-band changes in both directions. |
| 28 | Manifest-based adapter onboarding | p1 | `cpt-cf-infrastructure-resource-manager-fr-manifest-onboarding`, `cpt-cf-infrastructure-resource-manager-fr-manifest-policy`, `cpt-cf-infrastructure-resource-manager-fr-adapter-delegation` | One call ingests an adapter package and atomically registers the adapter, its types, data-plane operations, delegation scopes, and policy bundles, then activates it. |
| 29 | Data-plane operation catalog | p1 | `cpt-cf-infrastructure-resource-manager-fr-data-plane-catalog`, `cpt-cf-infrastructure-resource-manager-fr-grantable-types` | Per-type catalog of provider operations published for capability-grant issuance, discovery, and per-instance availability. |
| 30 | Per-resource-type authorization with response masking | p1 | `cpt-cf-infrastructure-resource-manager-fr-per-type-authz`, `cpt-cf-infrastructure-resource-manager-fr-write-admission`, `cpt-cf-infrastructure-resource-manager-fr-authz-list-union`, `cpt-cf-infrastructure-resource-manager-fr-authz-payload-masking`, `cpt-cf-infrastructure-resource-manager-fr-authz-topology-narrowing` | IRM decides access per resource type. Unreadable members stay listed, but IRM withholds and marks their payloads. IRM silently narrows topology neighbors. |
| 31 | Mid-flight re-authorization | p2 | `cpt-cf-infrastructure-resource-manager-fr-midflight-reauth` | A running deployment re-validates the caller's live authority before each side-effecting stage and cancels when authority was revoked. |
| 32 | Cascade delete of owned subtrees | p1 | `cpt-cf-infrastructure-resource-manager-fr-cascade-delete`, `cpt-cf-infrastructure-resource-manager-fr-cascade-admission`, `cpt-cf-infrastructure-resource-manager-fr-cascade-disclosure` | Deleting an owning parent commits first. The owned subtree converges to deleted asynchronously, behind admission gates and a blast-radius cap. |
| 33 | Operation cancellation | p1 | `cpt-cf-infrastructure-resource-manager-fr-operation-cancel` | A single idempotent cancel surface addressed by operation, distinguishing "cancellation requested" from "already finished". |
| 34 | Concurrency control and conditional reads | p2 | `cpt-cf-infrastructure-resource-manager-fr-conditional-reads` | Validators on reads with not-modified responses, and optional precondition validation on mutating operations. |
| 35 | Platform hardening and licensing | p1 | `cpt-cf-infrastructure-resource-manager-fr-adapter-credential`, `cpt-cf-infrastructure-resource-manager-fr-adapter-egress`, `cpt-cf-infrastructure-resource-manager-fr-adapter-response-validation`, `cpt-cf-infrastructure-resource-manager-fr-adapter-async-protocol`, `cpt-cf-infrastructure-resource-manager-fr-request-limits`, `cpt-cf-infrastructure-resource-manager-fr-license-gating`, `cpt-cf-infrastructure-resource-manager-fr-dependency-unavailability` | Per-call adapter credentials, egress protection, adapter-response validation, request size limits distinct from field validation, and license gating of the whole API. |

### 5.2 Out of Scope

- Infrastructure adapter implementations — each provider adapter is built and released separately. IRM specifies the contract that it holds adapters to, not the adapters themselves.
- Policy Decision Service and RBAC Engine internals — IRM integrates through their published contracts.
- Authentication mechanics (SSO/federation, MFA, session and credential policy) — AM and IdP own them. IRM performs no authentication itself: it trusts the per-request identity and tenant context they supply, and refuses requests that arrive without it.
- Billing, rating, and metering — IRM exposes resource and scope data for attribution only.
- Capacity pools — allocatable-capacity containers are out of scope. Resource groups are lifecycle and authorization containers only and carry no capacity or allocation semantics; a future pool concept, if introduced, is a separate abstraction, not a kind of group.
- Lightweight typed object storage — the `simple-resource-registry` gear owns schema-validated CRUD of simple typed objects; IRM owns provider-backed orchestration. Provider projections belong to IRM inventory, not the registry.
- Continuous drift detection and reconciliation loops — infrastructure adapters own these. IRM provides on-demand refresh and preview.
- Interface schemas, wire contracts, data models, and error-code taxonomies — this PRD states required behavior and the distinctions that callers must be able to make. The component's technical design and its published interface description settle the concrete schemas and codes.
- Multi-region execution and cross-region coordination — future enhancement.
- Phase-2 secret hardening (envelope encryption, reference sentinels, key rotation) — deferred to a Phase-2 specification. §15 records the residual risk and its mitigation.
- Graph analytics and machine-learning insights — the analytics platform owns these.
- End-user UI implementation — separate frontend design scope.
- Documentation and support tiers — the machine-readable interface description ships with the management API (§9.1). User guides, training material, and product-specific support tiers are owned by the platform's shared documentation and support processes, not by this component. Operational escalation for IRM incidents follows the platform's standard SRE on-call process.
- Per-dependency unavailability classes and recovery mechanics — the technical design settles them.
- Operational dashboards and log retention — owned by the platform's operations tooling, not by this component.

## 6. Functional Requirements

> **Testing strategy**: Automated tests (unit, integration, e2e) cover all requirements. The target is 90%+ code coverage, unless specified otherwise. Record the verification method only for non-test approaches (analysis, inspection, demonstration).

### 6.1 Type System and Adapters

#### Resource Type Registry

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-type-registry`

The system **MUST** allow registration, versioning, querying, and retirement of resource type definitions under platform-wide (GTS) identifiers. A definition includes property schemas, day-2 actions, and optional capabilities. The system **MUST** reject invalid definitions with actionable errors. Invalid definitions include identifiers inside the platform-reserved namespace.

**Rationale**: The type registry is the foundation for extensibility. Every other IRM domain (deployments, actions, discovery, graph) consumes registered types.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-platform-engineer`, `cpt-cf-infrastructure-resource-manager-actor-adapter-developer`, `cpt-cf-infrastructure-resource-manager-actor-type-identifier-service`

#### Adapter Onboarding Lifecycle

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-adapter-onboarding`

The system **MUST** provide a controlled adapter lifecycle: register (inactive), contribute resource types, then activate. The system **MUST NOT** activate an adapter that contributed no resource types. Activation **MUST** publish the per-type authorization identity of every contributed type before the adapter serves traffic. A publication failure **MUST** leave the adapter in its previous state. Adapter health **MUST** be observable.

**Rationale**: This lifecycle makes sure that a half-configured provider does not serve traffic. It gives operators a clear onboarding path.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-adapter-developer`, `cpt-cf-infrastructure-resource-manager-actor-infrastructure-adapter`

#### Adapter Inventory and Retirement

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-adapter-retirement`

The adapter inventory **MUST** be listable with filtering, ordering, and pagination. An adapter **MUST** be removable. The system **MUST** refuse removal while any resource provisioned through the adapter's types exists. Removal **MUST** remove the type definitions that the adapter contributed. The system **MUST** refuse resource creation against a type whose adapter is not active.

**Rationale**: Without an offboarding path, dead providers accumulate. Removal of an adapter that still has live resources leaves those resources unmanageable.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-adapter-developer`, `cpt-cf-infrastructure-resource-manager-actor-system-administrator`

#### Manifest-Based Onboarding

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-manifest-onboarding`

The system **MUST** accept a complete adapter package in a single operation. As one unit, this operation **MUST** do all of the following:

- Validate the package.
- Register or update the adapter.
- Register every resource type that the package declares.
- Materialize the data-plane operation catalog of the package.
- Record the delegation scopes that the package requests.
- Publish the policy bundles of the package.
- Activate the adapter.

A re-submitted package **MUST** update the existing adapter, not duplicate it. The system **MUST** verify the integrity and authenticity of an adapter package before any registration begins; a package that fails verification **MUST** be rejected with nothing registered. The system **MUST** record a trust level for each onboarded adapter — at minimum distinguishing platform-verified packages from third-party ones — and **MUST** expose that trust level wherever the adapter and its contributed types are listed.

**Rationale**: Onboarding a provider through the granular lifecycle takes many ordered calls. A single declarative package makes adapter delivery reproducible. It also removes half-configured intermediate states.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-adapter-developer`, `cpt-cf-infrastructure-resource-manager-actor-type-identifier-service`

#### Manifest-Declared Authorization Policy

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-manifest-policy`

An adapter package **MAY** declare authorization policy for its own resource types. The system **MUST** publish those policies to the platform policy service as part of onboarding. The system **MUST** activate the policies only after they are complete. Registration of an adapter therefore changes platform authorization. The change **MUST** be attributable to that adapter.

**Rationale**: Providers know which operations on their types must be permitted or denied. Delivery of that policy with the adapter avoids a separate manual policy rollout per provider.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-adapter-developer`, `cpt-cf-infrastructure-resource-manager-actor-policy-engine`

#### Operator-Granted Adapter Delegation

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-adapter-delegation`

Where an adapter needs to call back into the platform on a user's behalf, the package **MUST** only *declare* the scopes it wants. An operator **MUST** separately grant a subset of those scopes. The operator **MUST** be able to disable callbacks entirely. The system **MUST** reject a grant that names an undeclared scope. Re-submission of a package with fewer declared scopes **MUST** narrow the existing grant automatically.

**Rationale**: Declaration by the vendor plus grant by the operator keeps delegated authority a deliberate and revocable decision. It is not a side effect of adapter installation.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-adapter-developer`, `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`

#### Type Evolution Safety

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-type-evolution`

The system **MUST** version resource type updates so that existing resources are unaffected. The system **MUST** refuse to remove a type while active resources of that type exist. A registered type **MUST** be modifiable only through the adapter that owns it. Re-registration of an existing type **MUST** update the type in place. The response **MUST** state which submitted types were newly registered and which were updated.

**Rationale**: Type changes must never silently break running estates.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-platform-engineer`

### 6.2 Resource Lifecycle

#### Unified Resource Management

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-resource-crud`

The system **MUST** provide one consistent interface to create, read, update, and delete resources of every registered type. This interface **MUST** include scoped listing, filters by type, status, group, and tags, and pagination for large result sets. Listing **MUST** offer caller-selected ordering over a published, bounded field set with opaque cursor pagination. The system **MUST** reject a malformed cursor as a distinct client error. Updates operate on full desired state. The system does not offer partial updates. Direct creation **MUST** validate properties against the type's schema (as enriched by admission) before the system persists any record. A violation **MUST** name the offending property on every path that validates it. Deletion of a resource that belongs to a deployment **MUST** execute as a classified change to that deployment: the deployment's definition minus the resource. The system updates the deployment's recorded definition accordingly. Re-submission of the previous definition re-creates the resource.

**Rationale**: A single surface across all resource classes is the core product promise. Fragmentation is the problem that this system solves.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-platform-engineer`, `cpt-cf-infrastructure-resource-manager-actor-automation-engineer`, `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`

#### Deployment-Scoped Resources

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-deployment-scoped`

Every managed resource **MUST** belong to a deployment. The system **MUST** wrap directly managed resources in an automatically created single-resource deployment (an anonymous deployment). This wrapper makes sure that history, rollback, and guardrails apply uniformly. When its sole resource is deleted, the anonymous deployment persists with its revision history. Its structural address remains occupied and is dedicated to that resource alone. A direct re-creation of a resource with the same identity attaches to the persisted anonymous deployment as a new revision, continuing its lineage. The dedication holds until the retention purge of the deleted resource completes (`cpt-cf-infrastructure-resource-manager-fr-soft-delete-retention`); the purge removes the anonymous deployment together with its history, and the address becomes reusable.

**Rationale**: One reconciliation and history model applies to everything. There are no second-class "loose" resources.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-platform-engineer`

#### Lifecycle States and Durable Acceptance

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-lifecycle-states`

Resources and operations **MUST** move through published state models. A resource is pending, provisioning, active, updating, executing an action, deleting, or failed. An operation is pending, accepted, running, succeeded, failed, or cancelled. The system **MUST** refuse illegal transitions. A completed deletion ends in a tombstone (removal from the live set under `cpt-cf-infrastructure-resource-manager-fr-soft-delete-retention`), not in a further resource state. The system **MUST** accept a mutation asynchronously. The system **MUST** commit the resource record and its tracking operation durably before provisioning work starts. This commit makes the request trackable even if the process dies immediately after acceptance. Every operation **MUST** reach a terminal state. A running operation has a bounded maximum lifetime, declared in `cpt-cf-infrastructure-resource-manager-nfr-limits`. After this lifetime, the system records the operation as failed. A delete that the provider permanently refuses **MUST** restore the resource to the state it held immediately before the delete, never a blanket failed. The system **MUST** record the refusal reason on the resource. The reason **MUST** be readable in the resource's representation, size-capped, and cleared on the next successful transition. Detachment and degradation are observable **conditions** on a resource — flags that any lifecycle state can carry — not lifecycle states themselves; the lifecycle state set is the closed list this requirement defines.

**Rationale**: Status filtering, polling, and automation are only possible against a defined state vocabulary. Acceptance without durability loses work invisibly.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-automation-engineer`, `cpt-cf-infrastructure-resource-manager-actor-workflow-executor`

#### Deletion Under Provisioning Uncertainty

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-delete-uncertainty`

A synchronous provider refusal of a creation is an answer from the provider's own validation, for a resource that never became addressable. The system **MUST** record this refusal on the resource at the moment the refusal is known. The system **MUST** clear the record when the resource becomes active or when new properties are committed for it. Deletion **MUST** complete without contact with the provider only for such resources. A recorded provider identifier **MUST** always take precedence and be used. The system **MUST** refuse a delete of a resource that carries neither a provider identifier nor a refusal record. The system **MUST** restore that resource and never report it deleted, because a provider object can exist. The system **MUST NOT** infer existence from the resource's status. An update whose result carries no provider identifier **MUST NOT** clear the recorded one.

**Rationale**: Both failure modes that this requirement prevents are invisible until too late. A delete reported as done while a provider object survives orphans real infrastructure. A permanent refusal to delete a resource that was never provisioned strands the inventory. The refusal record is the only item that makes the two cases distinguishable from the resource itself.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-sre-operator`, `cpt-cf-infrastructure-resource-manager-actor-infrastructure-adapter`

#### Resource Capabilities

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-capabilities`

The system **MUST** let callers discover which optional capabilities a resource type offers. The system **MUST** let callers enable, configure, or disable those capabilities per resource instance. The system **MUST** audit every capability change.

**Rationale**: Monetizable optional features (backup, monitoring, encryption) need first-class, governable enablement.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`, `cpt-cf-infrastructure-resource-manager-actor-sre-operator`

#### Data-Plane Operation Catalog

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-data-plane-catalog`

For every resource type, the system **MUST** publish the provider operations available on it. The published entry for an operation **MUST** include the required resource state, input and output shapes, maximum credential lifetime, credential class, and deprecation status. With this catalog, other platform services can issue scoped grants for direct data-plane access. They can discover which operations exist. They can also determine which operations are available on a specific resource instance.

**Rationale**: Direct data-plane access must be grantable per operation, not all-or-nothing. The grant issuer needs an authoritative machine-readable catalog to scope against.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-grant-service`, `cpt-cf-infrastructure-resource-manager-actor-adapter-developer`

#### Grantable Resource Type Discovery

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-grantable-types`

The system **MUST** expose the catalog of active resource types together with the authorization identity by which each type is addressed. With this catalog, a role author can grant access for a specific resource type. A read of the catalog **MUST** itself require authority to read type definitions. Each entry **MUST** carry the type's display name and owning adapter, so a role author can tell what they grant.

**Rationale**: Per-type authorization is unusable without a discoverable list of the types that can be named in a role.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`

### 6.3 Declarative Deployments and Reconciliation

#### Declarative Definitions

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-declarative-definitions`

The system **MUST** accept declarative multi-resource definitions with parameters, variables, inter-resource dependencies, outputs, and dynamic expressions. The system **MUST** validate the definitions, including expression and reference correctness, before it attempts any change. Validation **MUST** report the exact location of each error. Validation **MUST** collect every fault across all validation stages in one response, not stop at the first. The system **MUST** reject a definition that carries an unrecognized field. The rejection **MUST** name the field. Resource names **MUST** be unique within their deployment. A type that declares no usable property schema accepts no properties at all. An absent schema tightens validation. It does not disable validation.

**Rationale**: Repeatable, reviewable infrastructure requires a declarative source of truth with early validation.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-platform-engineer`, `cpt-cf-infrastructure-resource-manager-actor-automation-engineer`

#### Conditional Resource Inclusion

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-conditional-resources`

A definition **MAY** attach a boolean condition expression to any resource it declares. The system **MUST** evaluate every condition before change classification. The system **MUST** exclude resources whose condition is false from the planned set. A condition that cannot be evaluated, or that yields a non-boolean value, **MUST** fail validation before the system attempts any change. Deployment status **MUST** report excluded members as skipped, with the reason. If the condition of a previously provisioned resource becomes false, the system classifies that resource for deletion under `cpt-cf-infrastructure-resource-manager-fr-change-classification`.

**Rationale**: One definition that serves several environments needs per-resource inclusion. Fail-closed evaluation makes sure that a broken condition does not silently provision or silently drop a resource. Classification stays five-valued: exclusion changes the planned set, not the outcome vocabulary.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-platform-engineer`, `cpt-cf-infrastructure-resource-manager-actor-automation-engineer`

#### Parameter Contract

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-parameters`

Definition parameters **MUST** support a declared constraint vocabulary: value type, numeric bounds, length bounds, enumerated allowed values, a required flag, and a sensitivity flag. The system **MUST** enforce these constraints before execution. The system **MUST** name every violated constraint and **MUST** collect all parameter faults in one response. An omitted optional parameter **MUST** resolve to its declared default. The system **MUST** refuse a required parameter with neither a default nor a supplied value before anything executes.

**Rationale**: Defaults and constraints make one definition reusable across environments. They do not move validation to apply time.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-platform-engineer`, `cpt-cf-infrastructure-resource-manager-actor-automation-engineer`

#### Five-Operation Change Classification

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-change-classification`

For every resource in a definition, the system **MUST** classify the pending change as exactly one of: no change, create, update, replace, or delete. Type metadata (immutable, computed, and secret fields) drives the classification. As a result, immutable-field changes classify as replace, and computed fields never produce spurious changes. When the caller re-applies an unchanged definition, the system **MUST** classify everything as no change.

**Rationale**: Correct classification is the contract for previews, guardrails, and safe execution.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-platform-engineer`

#### Preview Without Side Effects

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-preview`

The system **MUST** produce a preview of every planned change, in both human-readable and machine-readable form. The preview **MUST** persist nothing and touch no provider. The system **MUST** redact secret values in every preview form. Preview **MUST** work before the first apply with no caller-supplied parameters. In that case, preview **MUST** resolve declared defaults. The preview output **MUST** be deterministic and carry totals per operation class. The system **MUST** present values that only exist after provisioning (cross-resource references) as unresolved, not guessed.

**Rationale**: Zero-surprise change management: reviewers approve exactly what will happen.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-platform-engineer`, `cpt-cf-infrastructure-resource-manager-actor-automation-engineer`

#### Plan Binding (Zero-Surprise Apply)

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-plan-binding`

An apply **MUST** execute exactly the change that was previewed, or refuse when the definition, current state, or type metadata drifted since preview. To do this, the system **MUST** bind the plan to its inputs (definition, current state, type metadata, tenant, options). As a result, definitions that differ only in ordering, or that explicitly state a declared default, bind to the same plan. When any input drifted since preview, the system **MUST** reject the apply with a distinct, actionable reason. Concurrent submissions against the same deployment **MUST** admit against its current revision under a consistency guard. The system **MUST** refuse a submission that lost the race as a conflict.

**Rationale**: If the executed change can differ from the reviewed one, approval is meaningless.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-platform-engineer`

#### Ordered, Durable Execution

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-ordered-execution`

The system **MUST** execute changes in dependency order. The system **MAY** execute in parallel those changes that no dependency relates. The system **MUST** survive process failure and resume with no double application of any resource. The system **MUST** compensate on failure. The system **MUST** schedule for removal each resource that it created during the failed change. A removal that finds the resource already gone **MUST** count as success. The system **MUST NOT** revert a resource that was updated rather than created.

**Rationale**: Long-running multi-resource changes must be crash-proof and leave a consistent state on failure.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-workflow-executor`, `cpt-cf-infrastructure-resource-manager-actor-infrastructure-adapter`

#### Deployment Status, Outputs, and Teardown

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-deployment-status`

A deployment **MUST** expose its execution state while it runs and after completion, including a distinct cancelled outcome, with per-member state. A failure **MUST** be attributable to the specific members that failed, each with a machine-readable reason. The system **MUST** compute declared outputs from provisioned state, persist them with the deployment, and serve them without recomputation. The outputs are empty before the first apply. Each successful resolution refreshes them. After a failed apply, the outputs retain the previously recorded values. The system omits unresolvable entries, and no error occurs. Deployments **MUST** be listable with filtering, ordering, and pagination. Deletion of a deployment **MUST** tear down its members through the standard classified delete path. The system **MUST** record this teardown like any apply.

**Rationale**: Polling automation and troubleshooting both read this surface. Without per-member attribution, a failed apply is undiagnosable. Without durable outputs, a pipeline cannot recover from a half-failed apply.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-automation-engineer`, `cpt-cf-infrastructure-resource-manager-actor-sre-operator`

#### Replacement Strategies

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-replace-strategies`

Where a change requires re-provisioning, the system **MUST** support both delete-before-create (default) and create-before-destroy strategies. The strategy is selected per type, with a per-resource override. The system **MUST** re-wire dependent resources to the replacement safely. Create-before-destroy **MUST** bring the replacement into service and re-point dependents before the system tears down the replaced instance.

**Rationale**: Different resource classes have different availability and uniqueness constraints during replacement.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-platform-engineer`

#### Guardrails and Management Policy

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-guardrails`

Every resource **MUST** carry exactly one management policy. The system evaluates the policy before execution. The policy comes from a single mechanism with three levels:

- **full** — every operation is permitted.
- **no-delete** — the system refuses deletion as a destructive act. Instead, the system detaches the provider object intact and preserves its provider identity. The object remains queryable, not destroyed. Creation and update proceed normally.
- **no-touch** — the system refuses both modification and deletion. Reading and preview proceed normally.

A refusal **MUST** identify which policy level caused it. The refusal **MUST** occur before the system makes any change. A resource type **MAY** declare a default management policy. A policy stated in a definition **MAY** only tighten the type's default, never loosen it.

**Rationale**: Production estates need protection against accidental destruction, the most expensive class of operator error. One mechanism with three levels is a deliberate choice over several overlapping protection layers. A single place to look answers "why was this refused" unambiguously. Layered guards did not. Known downstream extension axes — finer observation-only modes, deployment-level deny settings, and configurable unmanage behavior — **MAY** be carried by a conforming implementation as extensions of this mechanism; adopting any of them as platform defaults requires a change request, per the §16 decision that one mechanism with three levels stands.

> **Scope note.** Two further protection layers were considered and are **not** part of this scope. These layers are deployment-level deny settings with a configurable unmanage behavior, and separate per-resource hard guards against destruction or update. They are deferred, not rejected. Their reintroduction requires a change request informed by production experience.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`, `cpt-cf-infrastructure-resource-manager-actor-sre-operator`

#### Duplicate-Safe Mutations

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-idempotent-writes`

Every mutating operation on resources and deployments **MUST** require a caller-supplied idempotency key. The system **MUST** refuse a request without a key before any work begins. A retried request **MUST NOT** execute twice and **MUST** receive the original outcome verbatim. The system **MUST** detect concurrent duplicates. Keys **MUST** be scoped per caller. Duplicate detection **MUST** distinguish two windows. The first window is a short in-flight reservation, during which the system refuses a duplicate as in progress. The second window is a longer replay window, during which the system returns the recorded outcome. After the replay window, a repeated key executes as a fresh request. The system **MUST** refuse a key reused with a different request body as a conflict distinct from a concurrent duplicate. The system **MUST** mark a replayed response as a replay to the caller. Only successful outcomes replay. A failed outcome **MUST** be immediately re-executable. Some operations are exempt from the key requirement. Operations that are safe to repeat by construction — cancellation, and placement moves — are exempt. Placement moves offer a conditional-update precondition instead. Administrative writes to the adapter and type registries are also exempt.

**Rationale**: Automation and CI/CD retry on timeout. Retries must never double-provision or double-delete.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-automation-engineer`, `cpt-cf-infrastructure-resource-manager-actor-persistence`

#### Cascade Delete of Owned Subtrees

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-cascade-delete`

Deletion of a resource that owns others **MUST** tear down its whole owned subtree. The parent's deletion **MUST** be committed first. The owned subtree **MUST** then converge to deleted asynchronously until no owned descendant remains. The teardown **MUST** run in bounded batches and resume after process restart from persisted state alone. Reads during that window **MUST** reflect reality: a descendant not yet removed remains visible until it is actually removed. Descendants **MUST NOT** require authorization beyond the admission-time evaluation of `cpt-cf-infrastructure-resource-manager-fr-cascade-admission`. No per-descendant re-authorization occurs during teardown.

**Rationale**: Owned resources are meaningless without their parent. A separate teardown of those resources leaves unusable remnants. A per-descendant authorization requirement makes a legitimate teardown fail halfway. A subtree cannot be destroyed in one provider call. A commit of the parent's deletion first makes the intent durable. Bounded, restart-safe batches make the teardown convergent under any failure.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-sre-operator`, `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`

#### Cascade Admission Conditions

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-cascade-admission`

The system **MUST** refuse a cascade before any resource is changed when any of the following conditions holds:

- A descendant's management policy protects it.
- The caller lacks delete authority over a descendant's type.
- A descendant lies outside the caller's visibility.
- The subtree exceeds the blast-radius limit in `cpt-cf-infrastructure-resource-manager-nfr-limits`.

The refusal **MUST** identify which condition fired. For the blast-radius condition, the refusal **MUST** report the observed subtree size against the limit. The owning parent's own policy is part of admission. The system **MUST** refuse outright a delete of a no-delete or no-touch parent that owns live descendants. No teardown starts and no detach occurs. Detachment instead of deletion is available only to a resource that owns nothing. The system **MUST** re-validate the admission verdict under the change lock immediately before commit. A subtree that gained a descendant or a protection since admission **MUST** be refused, not deleted on the stale verdict.

**Rationale**: A cascade that fails partway leaves an estate in a state that no one designed. Refusal before the first change is what makes the cascade all-or-nothing.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-sre-operator`

#### Cascade Disclosure and Confirmation

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-cascade-disclosure`

Before it executes an admissible cascade, the system **MUST** disclose to the caller the extent of what will be destroyed. At minimum, the disclosure **MUST** state the number of resources and the identity of those within the caller's visibility. The system **MUST** require explicit confirmation of that disclosed extent. An unconfirmed cascade request **MUST** change nothing.

**Rationale**: This is the most destructive single operation in the system. Descendants are deliberately exempt from separate authorization. As a result, nothing else in the flow tells the caller how much is about to be destroyed. Detachment of an orphaned provider object already requires confirmation. Destruction of a whole subtree must not require less.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-sre-operator`, `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`

#### Operation Cancellation

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-operation-cancel`

The system **MUST** let an authorized caller request cancellation of a running operation. The system **MUST** authorize the request before it contacts the execution engine. The system **MUST** distinguish "cancellation requested" from "already finished". In the latter case, the system reports the final outcome. A repeat of the request **MUST** be safe. Cancellation **MUST** take effect at a change boundary: work already in flight completes, and the system skips the remaining work. The operation settles in a distinct cancelled terminal state. The deployment reports a cancelled outcome.

**Rationale**: Long-running provisioning must be interruptible. An operator needs to know whether cancellation still means anything.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-sre-operator`

#### Relationship Backfill for Pre-Existing Deployments

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-relationship-backfill`

The system **SHOULD** provide an idempotent administrative pass that derives relationships for deployments created before relationship tracking existed. The pass **MUST** report the deployments that it cannot cover. Operators then know where ownership-dependent behavior can be incomplete.

**Rationale**: Cascade and protection semantics depend on derived relationships. Without this pass, deployments that predate relationship tracking behave incorrectly and silently.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-sre-operator`

#### Revisions and Unified History

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-revisions-history`

The system **MUST** record every admitted apply as an immutable revision. The revision captures what was applied and under which type metadata and policies. An empty (no-change) apply **MUST** complete synchronously and still record a revision. It starts no execution and no provider call. The system **MUST** provide a unified chronological history of applies, rollbacks, and refreshes at both deployment and single-resource scope. Resource history **MUST** remain reachable across replacement (lineage).

**Rationale**: "What changed, when, by whom" is the audit and recovery backbone.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-sre-operator`, `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`, `cpt-cf-infrastructure-resource-manager-actor-persistence`

#### Revision Rollback

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-rollback`

The system **MUST** support rollback to any retained revision selected by identifier, timestamp, or relative position. The selectors include a previous-meaningful selector that skips no-change revisions. Rollback **MUST** execute as a fresh reconciliation against current actual state, never a replay of a stored plan. Rollback **MUST** use the revision's frozen type metadata and policies. The system **MUST** reject targets outside the resource's lineage. Type-evolution compatibility **MUST** be a graded verdict. Identical and additively-compatible targets proceed, the latter with a warning. The system **MUST** refuse an incompatible target and name the offending types. A rollback that revives a deleted resource **MUST** re-derive its relationships and ownership ancestry. This re-derivation restores topology, not only the record.

**Rationale**: Reversibility is a first-class promise. Rollback must be as safe and previewable as forward change.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-sre-operator`

#### Actual-State Refresh

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-refresh`

The system **MUST** let operators refresh the recorded actual state of a deployment or single resource from the provider on demand. Out-of-band changes then become visible in the next preview. Refresh **MUST NOT** run concurrently with an apply on the same scope. The refresh outcome **MUST** report how many resources were refreshed, drifted, unchanged, and failed. Refresh **MUST** re-derive the relationships extracted from instance data. Observed placement changes then update topology.

**Rationale**: This gives point-in-time drift visibility without a continuous reconciliation loop.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-sre-operator`

#### Soft-Delete, Retention, and Orphans

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-soft-delete-retention`

Deletion **MUST** tombstone resources with configurable retention before permanent removal. Every tombstone **MUST** record why it was created. At minimum, the record **MUST** distinguish removal from the definition from removal through a cascade. For a cascade, the record **MUST** name the originating parent. Provider objects detached by policy (orphans) **MUST** remain queryable with their provider identity preserved. Orphans **MUST** count against a per-tenant orphan capacity. Orphans **MUST** be cleanable only through an explicit operator action with confirmation.

**Rationale**: This gives recoverability after deletion and controlled handling of intentionally preserved provider objects.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-sre-operator`, `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`

#### Secret Hygiene

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-secret-hygiene`

The system **MUST NOT** persist or emit secret values in cleartext in any artifact. Artifacts include state, revisions, previews, history, logs, metrics, events, and error messages. The system **MUST** detect a change to a secret field without storing, exposing, or reconstructing its cleartext. The comparison artifacts the system derives from a secret value **MUST NOT** enable cross-tenant correlation of equal values or offline recovery of the value. When a field becomes secret through type re-registration, the system **MUST** re-protect existing persisted values before further changes on affected types proceed.

**Rationale**: A single cleartext leak in any persisted artifact defeats all other secret handling.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-platform-engineer`

#### Model Migration with Abort Path

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-model-migration`

Migration of existing data to the deployment-scoped model **MUST** proceed per tenant in resumable phases, with verification before any destructive step. The migration **MUST** keep legacy data readable until completion. The migration **MUST** support administrative abort and resume. Each migration step **MUST** be reversible.

**Rationale**: A platform-wide data model change must never strand a tenant in a broken intermediate state.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-platform-engineer`

### 6.4 Lifecycle Actions

#### Action Framework

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-action-framework`

The system **MUST** let providers define day-2 actions per resource type, with parameters, allowed source states, and an execution contract. The system **MUST** make defined actions discoverable per type.

**Rationale**: Standardized day-2 operations across heterogeneous providers are a core platform differentiator.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-adapter-developer`

#### Validated Asynchronous Action Execution

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-action-execution`

Action invocation **MUST** validate the resource's current state against the action's allowed states and reject invalid transitions. Action invocation **MUST** validate supplied parameters against the action's declared parameter schema before dispatch. Action execution is a modification for management-policy purposes. The system **MUST** refuse an action invocation on a no-touch resource before dispatch. The no-delete level does not restrict actions. Execution **MUST** be asynchronous and trackable to completion or failure. Execution **MUST** be authorized and tenant-scoped. The system **MUST** fully audit execution, including rejected attempts.

**Rationale**: Day-2 actions mutate live workloads. State validation and traceability are non-negotiable.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-sre-operator`, `cpt-cf-infrastructure-resource-manager-actor-infrastructure-adapter`

### 6.5 Relationships and Topology

#### Relationship Model and Consistency

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-relationship-model`

The system **MUST** maintain typed relationships between resources: ownership (parent-child), dependency, and attachment. The system **MUST** keep these relationships consistent with the resource lifecycle. Relationship removal cascades with resource removal. The system removes orphaned relationships. A relationship change from migration preserves the prior relationship as archived, not erased. A resource **MUST** have at most one live owning parent. The ownership graph **MUST** stay acyclic. A relationship **MUST NOT** relate resources of different tenants. Relationships **MUST** be derivable from two sources in the same committed change. The sources are declarations in the deployment definition, and per-type declarations that extract references from the instance data of a resource. Delete behavior is attachable to owning relationships only.

**Rationale**: Trustworthy topology requires that the graph never contradicts resource reality.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-system-administrator`

#### Graph Queries

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-graph-query`

The system **MUST** answer topology queries with pagination, cycle safety, and bounded staleness relative to resource changes. These queries include direct relationships of a resource, multi-hop dependency traversal with configurable depth, and filtered listing by type and scope. A traversal deeper than one hop **MUST** name exactly one relationship kind. A pagination cursor is bound to its original query (direction, kind, and depth). The field projection can change between pages.

**Rationale**: Impact analysis ("what depends on this host") is the primary consumer value of the graph.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-system-administrator`, `cpt-cf-infrastructure-resource-manager-actor-automation-engineer`

#### Topology Visualization

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-visualization`

The system **SHOULD** provide the machine-readable topology surface that a visualization frontend consumes: scoped graph queries with tenant, type, and tag filtering, dependency-path computation, and export in a documented format. The interactive view itself is frontend scope (§5.2, §11).

**Rationale**: Visual topology shortens troubleshooting and maintenance planning.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-system-administrator`

### 6.6 Discovery and Inventory

#### Discovery Jobs and Controls

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-discovery-jobs`

The system **MUST** support manual, scheduled, and event-driven discovery per adapter, over provider environments and directory services alike. Discovery **MUST** have operational controls: maintenance mode and disable (each blocks new runs), and an error-threshold circuit breaker. The circuit breaker suspends a repeatedly failing adapter and alerts operators.

**Rationale**: Discovery against live providers needs operator brakes as much as automation.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-system-administrator`, `cpt-cf-infrastructure-resource-manager-actor-infrastructure-adapter`

#### Idempotent Synchronization

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-discovery-sync`

Discovery synchronization **MUST** idempotently create and update IRM inventory from provider output at the volume stated in `cpt-cf-infrastructure-resource-manager-nfr-discovery-throughput`. Synchronization **MUST** support full and incremental modes where the provider allows them. Synchronization **MUST** handle resources missing from the source per a configurable policy (default: flag only). Synchronized inventory **MUST** carry the reported configuration and operational state of each resource, not only its existence. Discovered resources **MUST** take their place in the resource graph like any managed resource.

**Rationale**: This gives inventory accuracy without destructive surprises. A repeated sync run must always be safe.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-infrastructure-adapter`

#### Tenant Assignment and Discovery Pool

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-tenant-assignment`

Discovered resources **MUST** be assignable to tenants automatically (by adapter ownership or policy) or manually through a pool of unassigned resources. The pool **MUST** support bulk assignment. A pooled discovered resource is an inventory candidate, not yet a managed resource; the group and deployment invariants apply from the moment of tenant assignment onward.

**Rationale**: Multi-tenant adoption of an existing estate requires controlled ownership assignment.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-system-administrator`, `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`

#### Non-Blocking Compliance Flagging

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-discovery-compliance`

Discovery **MUST** record all found resources, even when they violate quota, license, or policy constraints. Discovery **MUST** flag each violating resource with its violation condition. Discovery **MUST** notify affected stakeholders with actionable remediation. Discovery **MUST** never block synchronization on violations.

**Rationale**: An accurate inventory that includes violations is worth more than a blocked sync. The system enforces compliance on change, not on observation.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`, `cpt-cf-infrastructure-resource-manager-actor-policy-engine`

### 6.7 Resource Groups and Organization

#### Resource Group Containment

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-resource-groups`

The scope model is tenant → resource group → resource. Every managed resource **MUST** belong to exactly one resource group. The system **MUST** reject the deletion of a non-empty group. `cpt-cf-infrastructure-resource-manager-fr-group-addressing`, `cpt-cf-infrastructure-resource-manager-fr-group-move`, and `cpt-cf-infrastructure-resource-manager-fr-default-group` govern placement, relocation, and the tenant default group.

**Rationale**: Lifecycle containers with strict membership are the unit of organization, access, and cleanup.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`

#### Deployment Addressing by Group

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-group-addressing`

The tuple (tenant, resource group, name) **MUST** identify a deployment. When a caller submits a definition, the system **MUST** create-or-update the deployment at that address. The same definition submitted against a different group **MUST** produce a separate, independent deployment, and not relocate the existing one. Placement **MUST NOT** be part of the definition document, so that one definition stays portable across groups. If the caller gives no group, the system **MUST** use the default group of the tenant. An address freed by teardown or relocation **MUST** become reusable, subject to the occupied-address refusal that `cpt-cf-infrastructure-resource-manager-fr-group-move-concurrency` states for a concurrent claim on that same address. An anonymous deployment's address is the exception: per `cpt-cf-infrastructure-resource-manager-fr-deployment-scoped` it stays dedicated to its resource after that resource is deleted, so resource deletion alone does not free the address; the address becomes reusable after the retention purge of that resource completes.

**Rationale**: Addressing by group makes a definition reusable across environments and keeps the deployment of each environment separate. The document stays portable because placement is not part of the document.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-platform-engineer`, `cpt-cf-infrastructure-resource-manager-actor-automation-engineer`

#### Explicit Group Relocation

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-group-move`

Relocation of a deployment to another group **MUST** be a distinct, explicitly requested operation. An apply of a definition **MUST NOT** move anything. Relocation **MUST** complete synchronously and **MUST** carry every live resource of the deployment with it. The system **MUST** state to callers that the vacated address becomes free. As a result, a pipeline that still addresses the old location creates a new deployment there and does not find the relocated one.

**Rationale**: Placement changes are consequential and must never occur as a side effect of a routine apply. Explicit relocation allows apply to be idempotent with respect to placement.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`, `cpt-cf-infrastructure-resource-manager-actor-sre-operator`

#### Relocation Concurrency Safety

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-group-move-concurrency`

Relocation **MUST** accept an optional precondition. With this precondition, a caller can refuse to act on a stale view of the deployment. When the target is already the current group, relocation **MUST** be a no-op. When the destination address is already occupied, the system **MUST** refuse the relocation. A refusal for a stale view **MUST** be distinguishable from a refusal for an occupied address.

**Rationale**: Without the precondition, two operators who relocate the same deployment concurrently silently overwrite one another. An occupied destination is a different problem from a stale view. If the two are conflated, the caller cannot choose a remedy.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`

#### Tenant Default Group

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-default-group`

Each tenant **MUST** have a default group with a deterministic identity derived from the tenant. The system **MUST** create this group on first need, not in advance. Concurrent creation of this group **MUST** be safe. If the group is deleted out of band, the system **MUST** recreate it with the same identity. As a result, access grants that refer to the group continue to work. If the group was renamed or replaced out of band, the system **MUST** fail closed and surface the discrepancy, and not repair it silently. The system **MUST NOT** create any group other than this default.

**Rationale**: Placement must work before a tenant organizes anything. A stable identity lets group-scoped access survive accidental deletion. Silent repair of a deliberate operator change is worse than a refusal.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`, `cpt-cf-infrastructure-resource-manager-actor-resource-group-service`

#### Group Reference Validation

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-group-validation`

The system **MUST** validate a group reference before placement takes effect. Validation **MUST** distinguish at least these cases:

- The group does not exist or is not visible.
- The name matches more than one group.
- The target is not a group that can hold resources.
- The group belongs to another tenant.

The system **MUST** report each case with a distinct machine-readable reason, so that clients can react programmatically. The system **MUST NOT** reveal whether an invisible group exists. When the system cannot reach the group service, address-resolving requests **MUST** fail closed rather than guess a placement.

**Rationale**: Placement decides who can see a resource. A guess on an uncertain answer silently mis-scopes access.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-resource-group-service`, `cpt-cf-infrastructure-resource-manager-actor-automation-engineer`

#### Membership Convergence

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-membership-convergence`

The system **MUST** commit placement locally in the same transaction as the resources that the placement describes. The system **MUST** propagate placement to the group service asynchronously. The local decision **MUST NOT** depend on immediate success of that propagation. Local placement **MUST** be strongly consistent at commit. Group membership **MUST** become consistent within the bound stated in `cpt-cf-infrastructure-resource-manager-nfr-placement-convergence`.

**Rationale**: A required remote call inside the write path makes provisioning fail whenever the group service is briefly unavailable. A deferred call obliges us to state the staleness bound explicitly instead.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-resource-group-service`, `cpt-cf-infrastructure-resource-manager-actor-system-trusted`

#### Membership Propagation Ordering

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-membership-ordering`

Propagation **MUST** establish the new membership before it removes the old one. As a result, a resource is never observably ungrouped at any point during a change of placement. This ordering governs group-placement propagation. The system commits a change of owning parent within the resource graph atomically, in a single transaction, with no observable intermediate state.

**Rationale**: A momentarily ungrouped resource is invisible to group-scoped access. The order of the two writes makes sure that a relocation does not briefly revoke legitimate access.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-resource-group-service`

#### Membership Propagation Durability

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-membership-durability`

Propagation **MUST** survive a process restart with no loss of pending work. Propagation **MUST** produce the same end state when several instances process the same pending placement concurrently.

**Rationale**: The component runs multiple instances and restarts routinely. Propagation that loses work on restart or double-applies under concurrency corrupts the authorization view, and does not only delay it.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-system-trusted`

#### Membership Propagation Failure Handling

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-membership-failure-handling`

Propagation **MUST** distinguish transient failure from permanent failure. The system **MUST** retry transient failures. The system **MUST** park permanent failures for operator attention, and not retry them indefinitely. Parked work **MUST** be observable and **MUST** have a stated operator action that resumes it.

**Rationale**: Silent infinite retry hides a broken placement. A parked placement without an observable count and a documented resume path is stranded.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-sre-operator`, `cpt-cf-infrastructure-resource-manager-actor-system-trusted`

#### Placement Drift Reconciliation

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-placement-drift`

The system **MUST** periodically reconcile placement in both directions. The system **MUST** remove membership records for resource types that IRM manages that no longer correspond to a managed resource; membership records of other platform components **MUST NOT** be touched. The system **MUST** re-propagate managed resources whose membership is missing or wrong. Reconciliation **MUST** be bounded per pass. If reconciliation stops early, it **MUST** report this rather than appear complete. Every resource of a deployment **MUST** eventually be in the group of the deployment. The system **MUST** repair a divergence, not tolerate it.

**Rationale**: Groups can be edited out of band. Without a reconciler, the authorization view drifts from reality permanently and invisibly.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-system-trusted`, `cpt-cf-infrastructure-resource-manager-actor-sre-operator`

#### Tags

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-tags`

The system **SHOULD** support key-value tags on resource groups and resources with downward inheritance (explicit child tags override). Tags are usable for filtering, cost attribution, and policy targeting. The system **SHOULD** audit tag changes.

**Rationale**: Tags give cross-cutting grouping that the strict hierarchy cannot express.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`

### 6.8 Governance and Security

#### Tenant Isolation

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-tenant-isolation`

Every IRM operation (queries, mutations, actions, deployments, discovery, graph) **MUST** carry tenant context and **MUST** be scoped to the tenant hierarchy of the caller. Resources outside that hierarchy **MUST NOT** be readable, writable, or inferable.

**Rationale**: Cross-tenant leakage is an existential compliance failure for a multi-tenant platform.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`, `cpt-cf-infrastructure-resource-manager-actor-identity-provider`

#### Role-Based Access

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-rbac`

The system **MUST** enforce the scope-based access decisions resolved through the platform authorization path, including the inheritance and deny semantics that path defines. Orphan cleanup and forced detach **MUST** require a dedicated operator permission distinct from ordinary delete.

**Rationale**: Exact per-actor/operation permissions prevent both privilege creep and accidental destruction. A typical deployment distinguishes roles such as Owner (all operations at scope, including access management), Contributor (create, update, delete, deploy, act — no access management), Reader (read-only over resources, history, and topology), and Adapter Developer (adapters and type definitions only, no tenant resources); the platform authorization path owns the actual role definitions.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`, `cpt-cf-infrastructure-resource-manager-actor-sre-operator`, `cpt-cf-infrastructure-resource-manager-actor-adapter-developer`, `cpt-cf-infrastructure-resource-manager-actor-rbac-engine`

#### Per-Resource-Type Authorization

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-per-type-authz`

Access **MUST** be decidable per resource type, not only per resource collection. As a result, a role can grant rights over one class of resources and not over all classes.

**Rationale**: A tenant estate mixes sensitive and routine resource classes. Without per-type decisions, any useful role becomes over-broad.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`, `cpt-cf-infrastructure-resource-manager-actor-policy-engine`

#### Write Admission by Member Type

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-write-admission`

Before an admitted change dispatches any work, the system **MUST** evaluate write authority for every resource type that the plan touches, as one decision. A denial refuses the whole change atomically and names every denied type. Preview **MUST** apply the identical admission. As a result, a change that previews cleanly is one that the caller can apply. Rollback **MUST** be admitted against the types that its reverse plan touches. The automatically created single-resource deployment path **MUST** pass the same gate as the declarative path.

**Rationale**: A missing grant discovered halfway through an apply leaves a half-changed estate. Parity between preview and apply makes an approved preview trustworthy.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-policy-engine`, `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`

#### Listing Under Partial Authority

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-authz-list-union`

A listing **MUST** return the union of what the caller can see across all resource types that the caller holds rights over. When the authority of the caller covers only some of the types present, the listing **MUST** return that union rather than fail or return nothing.

**Rationale**: An all-or-nothing listing makes partial authority useless in practice. This limitation pushes operators toward over-broad roles.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`

#### Withheld Payloads Are Marked

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-authz-payload-masking`

If a caller can see that a resource exists but cannot read its type, the resource **MUST** stay visible as an entry. The payload of the resource **MUST** be withheld. The response **MUST** state explicitly that the payload was withheld. A payload that cannot be partially withheld **MUST** be withheld in full rather than partially disclosed.

**Rationale**: An empty payload returned silently is indistinguishable from a resource that has none. The explicit mark of a withheld payload keeps a partial answer honest.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`

#### Topology Narrowing

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-authz-topology-narrowing`

Topology results **MUST** omit neighbors whose type the caller cannot read, and not disclose that an omission occurred. A caller who cannot read the anchor resource itself **MUST** be refused with no disclosure of whether the anchor exists. Narrowing applies to neighbors, never to the anchor.

**Rationale**: Disclosure of the existence of an unreadable neighbor leaks the shape of an estate that the caller has no rights over.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`

#### Authority Revocation During Execution

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-midflight-reauth`

Before each side-effecting stage of a running deployment, the system **MUST** re-evaluate the authority of the initiating caller against their current rights, as compiled from the platform authorization resolution path (the policy decision service and the RBAC engine). The system **MUST** make sure that this authority still covers the resources in that stage. A definitive loss of authority **MUST** cancel the operation and record the reason. An inability to reach the decision service **MUST** be treated as transient and retried, never as a denial.

**Rationale**: Long deployments outlive access decisions. Revocation of a role must stop work in flight. But a decision-service blip must not kill a legitimate deployment.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-policy-engine`, `cpt-cf-infrastructure-resource-manager-actor-rbac-engine`, `cpt-cf-infrastructure-resource-manager-actor-workflow-executor`

#### Pre-Create Admission Pipeline

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-admission-pipeline`

Resource creation **MUST** pass through an ordered, extensible admission pipeline before anything is persisted. The first rejecting check aborts the creation, with nothing persisted and the remaining checks skipped. An admission extension **MAY** enrich the request (defaults for name, labels, or properties). The enriched values are what the system validates and persists. The type of the resource **MUST** be resolved before any extension runs and **MUST NOT** be changeable by an extension.

**Rationale**: Policy enforcement and platform defaults need one sanctioned interception point. An extension that redirects a request to a different type bypasses every type-scoped decision made upstream.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-policy-engine`

#### Policy and Quota Gating

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-policy-gating`

Policy and quota evaluation **MUST** gate provisioning, modification, and lifecycle actions before any change executes. The system evaluates quota before policy. Denials **MUST** carry an actionable reason. When the decision service is unavailable, the system **MUST** fail closed. Replacement strategies that temporarily double capacity **MUST** be validated against quota at their peak. A decision **MAY** be advisory: an allow verdict **MAY** carry obligations or warnings from the decision service, and the system **MUST** deliver them to the caller unaltered alongside the operation result.

**Rationale**: Governance that runs after the change is not governance.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-policy-engine`

#### Audit and Domain Events

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-audit-events`

Every mutation, action execution, deployment lifecycle transition, and discovery outcome **MUST** emit an audit record. The audit record **MUST** carry the full correlation context (tenant, actor, affected entities, operation, outcome) with zero secret content. Domain events **MUST** be published at least once per committed change. Each domain event **MUST** carry an ordering key that is monotonic per affected entity. Each domain event **MUST** be deduplicable by event identity and make loss detectable by the consumer. Idempotent replays **MUST** be distinguishable from fresh mutations in the audit trail. Rejected operations **MUST** be audited as well as committed ones. Events that an attribution or rating pipeline consumes **MUST** carry the resource's tenant and group identity and its lineage-stable identifier, which survives replacement.

**Rationale**: Compliance, billing, and cross-system integration all depend on a trustworthy event record. Integrity and tamper-evidence of the persisted audit trail are owned by the platform audit sink, not by this component.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-event-consumer`, `cpt-cf-infrastructure-resource-manager-actor-sre-operator`

#### Data Classification

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-data-classification`

IRM defines no data field intended to carry end-user personal data, and its own generated artifacts (state, revisions, audit records, events) **MUST NOT** introduce end-user personal data beyond what the caller supplied. A resource type's definition **MAY** embed secrets only under the handling that `cpt-cf-infrastructure-resource-manager-fr-secret-hygiene` requires. Audit records **MUST** carry operator identity and tenant context, subject to the retention and purge primitives of `cpt-cf-infrastructure-resource-manager-fr-soft-delete-retention`.

**Rationale**: The tenant owns the data of its resources; the platform owns the audit trail. Audit records carry the minimum identity attributes needed for attribution — subject, tenant, and operation context — and nothing more. Data-protection obligations for caller-supplied content rest with the deployment operator: the regime-layering answer recorded in §16 (2026-08-03) lets an operator apply regime-specific obligations on top of the primitives this requirement states, without IRM encoding any regime itself.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`, `cpt-cf-infrastructure-resource-manager-actor-sre-operator`

### 6.9 API Contract and Platform Hardening

#### Per-Call Adapter Credentials

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-adapter-credential`

Every outbound call to an adapter **MUST** carry a capability token. This token is a credential usable only for that adapter and that operation. The credential expires well within the duration of the work that it authorizes. Long-running work **MUST** obtain a fresh credential rather than extend or reuse an expiring one.

**Rationale**: A credential can outlive its call, or can be replayable against a different adapter or operation. Such a credential turns one leaked value into general access to the provider estate.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-infrastructure-adapter`, `cpt-cf-infrastructure-resource-manager-actor-token-issuer`

#### Outbound Traffic Confinement

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-adapter-egress`

The component **MUST NOT** be usable as a path to platform-internal endpoints. The system **MUST** validate the destination of an outbound adapter call on every attempt. As a result, a destination that resolves differently after admission cannot bypass the validation. A redirect **MUST NOT** be followed. A destination that the system cannot validate **MUST** fail closed.

**Rationale**: Adapters are registered by operators and addressed by URL. This makes adapter registration an egress attack surface. Validation done only once at registration is trivially bypassable.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-infrastructure-adapter`

#### Adapter Response Validation

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-adapter-response-validation`

Adapter responses **MUST** be treated as untrusted input. Responses **MUST** be size-bounded before parsing. A malformed response **MUST** be rejected. For a creation, a response without the identity of the newly provisioned resource **MUST** be rejected. Responses **MUST NOT** be able to impersonate internal protocol markers. Responses **MUST** be validated against the declared output shape of the type. Provider error text surfaced to users **MUST** be truncated. Ambiguous provider state **MUST** be treated as not-yet-ready rather than ready.

**Rationale**: A hostile or broken adapter must not be able to corrupt platform state, exhaust memory, or trick the engine into a different protocol path.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-infrastructure-adapter`

#### Asynchronous Adapter Protocol

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-adapter-async-protocol`

An adapter **MUST** be able to answer synchronously, or to accept the work and report a location to poll. For accepted work:

- An accepted answer without a pollable location **MUST** fail the operation immediately as non-retryable. The polling location **MUST** belong to the same adapter.
- The system **MUST** poll with backoff up to a stated maximum duration (one hour unless overridden per operation). After this duration, the operation **MUST** be recorded as failed rather than left pending.
- The system **MUST** continue to poll after a transient provider error. Authorization and absence errors **MUST** be treated as terminal.
- Retried outbound calls **MUST** carry the same duplicate-safety key. As a result, a retry or a process restart resumes the provider-side operation and does not start a second one.
- When the operation is canceled, the system **MUST** attempt to cancel the provider-side work and record whether that attempt succeeded.

**Rationale**: Real provisioning takes minutes to hours. A requirement for adapters to hold a connection makes them fragile and prevents cancellation.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-infrastructure-adapter`

#### Adapter Health Reporting

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-adapter-health`

The system **MUST** report the health of an adapter on demand. The system **MUST** classify an unreachable or invalid response as unhealthy, not as an error of the health request itself. The system **MUST** distinguish "cannot be determined" from "unhealthy".

**Rationale**: Operators who triage failed provisioning need a definite answer about the provider. A health check that itself fails gives no answer.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-sre-operator`, `cpt-cf-infrastructure-resource-manager-actor-infrastructure-adapter`

#### Concurrency Control and Conditional Reads

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-conditional-reads`

Reads of a single resource, its topology, and a revision **MUST** carry a validator that changes whenever the represented state changes. A caller that presents an unchanged validator **MUST** receive a not-modified response. A malformed validator **MUST** be treated as absent rather than rejected. Mutating operations that accept a precondition **MUST** refuse to act on a stale view, and **MUST** report a precondition failure distinctly from other conflicts.

**Rationale**: Polling clients and UIs need cheap change detection. Optimistic concurrency makes concurrent operators safe without global locks.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-automation-engineer`

#### Request Limits Distinct from Validation

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-request-limits`

The system **MUST** bound request size at the transport boundary, with a higher bound only on the operations that legitimately carry large payloads (definitions, resource properties, adapter packages). The system **MUST** keep those transport limits distinct from field-level validation. As a result, a marginally oversized payload receives a structured validation error rather than an opaque size rejection.

**Rationale**: A single global limit either blocks legitimate definitions or leaves the service exposed. Size conflated with validation makes errors unactionable.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-automation-engineer`

#### License Gating

- [ ] `p3` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-license-gating`

Every IRM operation **MUST** be gated on the platform license feature that entitles resource management. As a result, an unlicensed deployment exposes no IRM functions. License entitlement is resolved through the platform's policy and license resolution path, not by IRM itself.

**Rationale**: Resource management is a licensed platform capability. A per-operation gate, rather than an install-time gate, prevents partial entitlement bypass.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-policy-engine`

#### Dependency Unavailability

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-fr-dependency-unavailability`

On unavailability of any dependency IRM calls (the outbound and bidirectional integrations that §13 lists), the system **MUST** behave deterministically and observably: no half-states, no hangs. The system **MUST NOT** guess the truth about access, identity, entitlement, or placement; an operation whose correctness depends on an unavailable dependency **MUST** refuse, generalizing the fail-closed behavior that policy and group-reference resolution already state for their own dependencies. Unavailability of downstream event-delivery infrastructure **MUST NOT** block a committed mutation; delivery resumes without loss once the infrastructure recovers. Unavailability of an inbound consumer (the Grant Issuance Service; inbound onboarding requests from adapter developers) has no bearing on the correctness of IRM operations and triggers no refusal. Every dependency outage **MUST** be observable, attributable to the failing dependency, and alertable. `cpt-cf-infrastructure-resource-manager-fr-midflight-reauth` states the one exception: an unreachable authorization decision service during mid-flight re-authorization is treated as transient and retried, never as a denial.

**Rationale**: IRM integrates with the many external systems that §13 lists. A dependency outage must degrade the same way everywhere: predictably, visibly, and without ever fabricating an answer the system cannot stand behind.

**Actors**: `cpt-cf-infrastructure-resource-manager-actor-sre-operator`, `cpt-cf-infrastructure-resource-manager-actor-policy-engine`, `cpt-cf-infrastructure-resource-manager-actor-event-consumer`

## 7. Non-Functional Requirements

### 7.1 NFR Inclusions

#### Interactive Latency

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-nfr-latency`

Core read and mutation acknowledgment operations **MUST** respond within 500 ms at p95. Single-resource topology lookups **MUST** respond within 200 ms at p95.

**Threshold**: p95, measured under sustained production load at declared scale. This threshold is validated against the reference load profile that the §16 open question defines; until that profile is defined, the threshold is not validated. Data-scale NFRs (`cpt-cf-infrastructure-resource-manager-nfr-scale`) are validated independently of the load profile.

**Rationale**: CI/CD and self-service portals depend on predictable interactive latency.

#### Preview Latency

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-nfr-preview-latency`

Change preview **MUST** complete within 2 s at p95 for definitions of up to 100 resources. Change preview **MUST** complete within 10 s at p95 for definitions of up to 1000 resources. Single-resource preview **MUST** complete within 200 ms at p95. These resource counts are measurement bands, not limits. The enforced bound on definition size is the request-body limit in Declared Limits.

**Threshold**: p95 per definition size band.

**Rationale**: Preview sits in every review loop. Slow previews push users to skip them.

#### Availability and Durability

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-nfr-availability`

The management surface **MUST** meet ≥ 99.9 % availability and ≥ 99.999 % data durability, with recovery point ≤ 1 hour and recovery time ≤ 4 hours.

**Threshold**: Measured monthly over continuous (24/7) operation. Planned maintenance is excluded from the measurement only when it is announced in advance, and is capped at 4 hours per month.

**Rationale**: IRM is the control plane of the platform. An IRM outage blocks all resource operations. Backup cadence, retention, and restore-verification mechanics that achieve the stated recovery point and recovery time are settled by the technical design and the platform backup policy, not by this PRD.

#### Scale

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-nfr-scale`

The system **MUST** operate at 100 000+ resources and 1000+ resource groups per tenant, and 1 000 000+ topology nodes with 5 000 000+ relationships platform-wide. Scoped list operations **MUST** complete within 2 s at p95 at that scale.

**Threshold**: Validated by scale tests before GA.

**Rationale**: Enterprise estates reach this scale. Degradation at this scale voids the single-pane promise.

#### Bounded Staleness

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-nfr-staleness`

Topology **MUST** converge within 10 s at p95 of a resource change. Unified history views **MUST** lag live changes by no more than 60 s at p99.

**Threshold**: p95 / p99 as stated.

**Rationale**: Operators act on topology and history. Stale views cause wrong decisions.

#### Discovery Throughput

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-nfr-discovery-throughput`

Discovery throughput is dominated by the adapter and its provider: enumeration speed is per-adapter, and this PRD does not bound it. IRM **MUST NOT** be the bottleneck: ingestion of a sync batch **MUST NOT** dominate end-to-end sync time, and IRM **MAY** parallelize sync runs — across adapters and within a single adapter's estate — where the adapter and provider allow it.

**Threshold**: The numeric ingestion-throughput target is set by the reference load profile (§16).

**Rationale**: The platform commits to the part it controls; the concrete target follows the measured profile rather than an arbitrary figure.

#### Duplicate Safety

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-nfr-idempotency`

A retried mutation that carries a previously used key **MUST NOT** produce a second set of side effects. This also applies when the retry arrives during the original execution, or after a crash in the middle of the original execution.

**Threshold**: Zero duplicate side effects across the retry and crash-recovery test matrix, including concurrent duplicate submission.

**Rationale**: A single double-provision breaks billing and trust.

#### Placement Convergence

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-nfr-placement-convergence`

Group membership **MUST** reflect a committed placement decision within 5 s at p95, measured from commit to converged. Group reference validation **MUST** complete within 50 ms at p95. Default-group provisioning **MUST** complete within 100 ms at p95. Rows parked for operator attention **MUST** be zero in steady state, and any nonzero count **MUST** be observable.

**Threshold**: p95 as stated. Parked rows and unrepaired drift are alertable at any nonzero value.

**Rationale**: Group-scoped access is only as current as membership. This bound makes "moved out of the group" mean "lost access" in a predictable time.

#### Background Process Resilience

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-nfr-background-resilience`

Background reconciliation **MUST** survive a failure of any single pass and continue. Reconciliation **MUST** begin a pass on start-up rather than wait a full interval. Reconciliation **MUST** be safe to run concurrently on multiple instances with no duplicated effect. For a clean shutdown, reconciliation **MUST** drain work in flight. Invalid configuration **MUST** be rejected at start-up rather than discovered at runtime. An individually corrupt stored record encountered during start-up recovery **MUST** be skipped with an operational signal, while the remaining records are served. One bad record **MUST NOT** prevent the start of the component.

**Threshold**: No single-pass failure stops the loop. There is no duplicated effect across concurrent instances.

**Rationale**: These processes are the only backstop for placement drift and stuck operations. If one dies quietly, the failure is invisible until the data diverges.

#### Declared Limits

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-nfr-limits`

The system **MUST** enforce and publish the limits that follow. A violation **MUST** be rejected with a message that names the limit and the observed value.

| Limit | Value |
|---|---|
| Request body, general operations | 64 KiB |
| Request body, operations carrying large payloads (definitions, resource properties, adapter packages) | 1 MiB + 64 KiB |
| Resource properties per resource | 1 MiB |
| Labels per resource | 64 KiB |
| Resource name length | 64 characters |
| Display name length | 256 characters |
| Adapter-supplied type-identifier length | 227 bytes |
| Cascade blast radius (descendants torn down in one owned subtree) | 256 (default; operator-configurable per deployment — the effective cap is part of the disclosed extent that `cpt-cf-infrastructure-resource-manager-fr-cascade-disclosure` requires the caller to confirm) |
| Relationship traversal depth | 1–16 |
| Traversal results per page | 100 |
| Owned parent-child chain depth | 16 |
| Completed-operation retention | 24 hours |
| Idempotency in-flight reservation window | 5 minutes |
| Idempotency replay window (recorded outcomes) | 24 hours |
| Running-operation maximum lifetime | 2 hours |

**Threshold**: As tabulated. Deployment size and definition size are deliberately expressed as a request-body bound, not as an independent resource count. As a result, one enforceable check covers both. The type-identifier bound is derived, not arbitrary. The per-type authorization identity of the identifier (the platform prefix plus the identifier) must stay grantable in the authorization system of the platform. Otherwise an accepted type can never receive a narrow per-type grant.

**Rationale**: A published but unenforced limit is worse than none. Callers plan against it and discover the real bound in production. Every value here is one that the system rejects on.

> **Scope note.** Earlier requirement drafts declared three limits. These limits are deliberately **not** introduced: a maximum resource count per deployment, a maximum dependency depth, and a cap on retained deployment history per tenant. The request-body bound, cycle detection, and retention provide the operative bounds. A change request backed by production data is required to introduce these limits anew.

### 7.2 NFR Exclusions

- Multi-region active-active availability: out of scope for this release. The platform roadmap covers it.
- Real-time push updates to user interfaces: the initial release uses polling. Streaming is a future enhancement; the conditional-read validators (`cpt-cf-infrastructure-resource-manager-fr-conditional-reads`) are reserved as the cursor mechanism for that future watch surface.
- Accessibility (WCAG 2.2): IRM ships no end-user UI (§5.2). The API and CLI target technical professionals working through terminals and automation. Accessibility requirements for operator-facing consoles (§11) belong to the separate frontend design scope.
- Internationalization and localization: the initial release is English-only across errors, identifiers, and payloads. Localization is revisited together with the multi-region roadmap.
- Inclusivity beyond accessibility: IRM actors are a narrow population of technical professionals working through API and CLI (§3). Broad-population inclusivity considerations do not apply at this scope.
- Co-existence isolation: resource-share protection between IRM and other platform components (shared execution substrate, event bus) is provided by platform-level quotas and namespacing, outside this component's scope.
- Report and export operations: none are in scope. Scoped-listing performance is covered by `cpt-cf-infrastructure-resource-manager-nfr-scale`.

## 8. Five Quality Vectors Analysis

| **Quality Vector** | **Show-Stopper Requirements** | **Rationale** |
|--------------------|-------------------------------|---------------|
| **Efficiency** | The single management surface MUST reduce integration points. Previews and empty-change applies MUST avoid provider calls and workflow starts entirely. | Consolidation is the reason the product exists. Wasted provider calls at scale are unaffordable. |
| **Reliability** | Duplicate-safe writes, crash-resumable execution with compensation, idempotent discovery sync, and rollback available for every retained revision. | The control plane must never leave estates in an unrecoverable half-state. |
| **Performance** | Interactive p95 targets (§7.1) MUST hold at declared scale (100 k+ resources/tenant, 1 M+ topology nodes). | Latency regressions at scale silently kill automation and UX. |
| **Security** | Mandatory tenant context everywhere. Fail-closed policy gating. Zero cleartext secrets in any persisted or emitted artifact. Complete correlated audit. | One isolation or secret leak is an existential compliance failure. |
| **Versatility** | The type registry + adapter contract MUST allow new providers and resource classes with zero core changes. Replacement strategies, management policies, and capabilities are configurable per type and per resource. | Ecosystem growth across heterogeneous infrastructure is the strategic bet. |

## 9. Public Library Interfaces

### 9.1 Public API Surface

#### Unified Management API

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-interface-management-api`

**Type**: REST API

**Stability**: stable

**Description**: The single management surface for all IRM domains: types and adapters, resources and capabilities, deployments (preview, apply, rollback, refresh, history), lifecycle actions, operations tracking, discovery, and topology queries. Absent optional data MUST be distinguishable from empty or zero values in every response. A machine-readable interface description MUST be published and kept in sync with the surface. The platform edge provides request-rate limiting. IRM itself enforces only the size limits declared in §7.

**Breaking Change Policy**: A major version bump is required. Evolution within a major version is additive.

#### Command-Line Interface

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-interface-cli`

**Type**: CLI

**Stability**: stable

**Description**: Operator and developer workflows over the management API: validate and apply definitions, inspect resources and history, trigger actions and discovery.

**Breaking Change Policy**: A deprecation window with warnings applies before removal of commands or flags.

#### In-Process Service Client

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-interface-service-client`

**Type**: In-process client contract

**Stability**: stable

**Description**: The in-process client contract for platform services that consume IRM (grants, tooling) without the network edge.

**Breaking Change Policy**: Versioned contract. New majors ship alongside old ones until consumers migrate.

### 9.2 External Integration Contracts

#### Adapter Contract

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-contract-adapter`

**Direction**: required from client (adapter implementations)

**Protocol/Format**: HTTP/REST (provider-agnostic, any implementation stack)

**Compatibility**: Versioned contract. IRM validates and size-bounds all adapter responses. Long-running provider operations are trackable to completion. Data-handling obligations for transmitted resource properties and secret material are part of this contract; the contract specification settles them. The contract includes an optional drift-report channel through which an adapter **MAY** surface detected out-of-band divergence to IRM; drift-detection service levels are per-adapter and outside this PRD's scope.

#### Workflow Executor Contract

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-contract-workflow-executor`

**Direction**: required from client (executor plugin)

**Protocol/Format**: Platform plugin interface with instance discovery

**Compatibility**: IRM core has no compile-time dependency on a concrete executor. A default no-op implementation MUST allow IRM to start without one.

#### Domain and Audit Event Stream

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-contract-events`

**Direction**: provided by library

**Protocol/Format**: CloudEvents envelope, versioned event names under the platform vendor namespace

**Compatibility**: Additive schema evolution within a major version. Consumers deduplicate by event identity. A breaking rename of the event namespace is permitted at most once before general availability, and MUST be announced to consumers in advance. After that, a breaking rename requires a major version.

## 10. Use Cases

#### Provision an Application Stack Declaratively

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-usecase-provision-stack`

**Actor**: `cpt-cf-infrastructure-resource-manager-actor-platform-engineer`

**Preconditions**:
- An active adapter registers the required resource types. The caller has Contributor access in the target scope.

**Main Flow**:
1. The engineer submits a declarative definition (network, VMs, dependencies, outputs) with parameters for the target environment.
2. The system validates the definition. It returns a preview that classifies every resource change.
3. The engineer approves. The apply executes in dependency order, gated by policy and quota.
4. The system records a revision, emits audit events, and exposes outputs for downstream automation.

**Postconditions**:
- Resources exist in the desired state. The apply is visible in history and reversible by rollback.

**Alternative Flows**:
- **Validation or policy failure**: The system reports the exact error and location. The system provisions nothing.
- **Mid-apply failure**: The system schedules every resource created during the failed change for removal. Resources that the change updated, not created, stay as they are. The system fully audits the failure.

#### Review a Change Before It Happens

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-usecase-preview-change`

**Actor**: `cpt-cf-infrastructure-resource-manager-actor-automation-engineer`

**Preconditions**:
- A deployment exists. A modified definition is in a change pipeline.

**Main Flow**:
1. The pipeline requests a preview of the modified definition.
2. The system redacts secrets and returns the classified change set (create/update/replace/delete per resource).
3. The reviewer approves. The pipeline applies the change.
4. The system validates that no drift occurred since the preview. Then it executes exactly the reviewed change.

**Postconditions**:
- The applied change equals the reviewed change. A revision records it.

**Alternative Flows**:
- **Drift since preview**: The system rejects the apply with the drift reason. The pipeline requests a new preview.

#### Roll Back a Regression

- [ ] `p1` - **ID**: `cpt-cf-infrastructure-resource-manager-usecase-rollback`

**Actor**: `cpt-cf-infrastructure-resource-manager-actor-sre-operator`

**Preconditions**:
- A deployment has retained revisions. A bad change was applied recently.

**Main Flow**:
1. The operator inspects the unified history and selects the last known-good revision.
2. The operator previews the rollback. The system synthesizes a fresh reconciliation to that revision.
3. The operator applies the rollback. Resources revert. If necessary, the system re-creates resources deleted since the target revision.

**Postconditions**:
- The estate matches the selected revision. The rollback itself is a new audited revision.

**Alternative Flows**:
- **Breaking type evolution since the target revision**: The system rejects the rollback with the incompatibility reason. The operator selects a newer target.

#### Onboard a New Provider Adapter

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-usecase-onboard-adapter`

**Actor**: `cpt-cf-infrastructure-resource-manager-actor-adapter-developer`

**Preconditions**:
- The adapter implementation is deployed and reachable. The developer has the adapter management permission.

**Main Flow**:
1. The developer submits one adapter package. The package declares the adapter, its resource types, its data-plane operations, the delegation scopes it requests, and its authorization policy.
2. The system registers all of it as a unit and activates the adapter. Its types become deployable.
3. If the adapter needs to call back on behalf of a user, the operator separately grants a subset of the declared delegation scopes.
4. The platform reports adapter health on demand.

**Postconditions**:
- The resource classes of the provider are manageable through the unified surface. The operations that its resources expose are grantable.

**Alternative Flows**:
- **Package invalid**: The system rejects the package as a whole. No partial registration remains.

#### Execute a Day-2 Action

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-usecase-day2-action`

**Actor**: `cpt-cf-infrastructure-resource-manager-actor-sre-operator`

**Preconditions**:
- A resource exists whose type declares day-2 actions. The operator holds write authority over that resource type.

**Main Flow**:
1. The operator inspects which actions the type of the resource declares, and which its current state permits.
2. The operator invokes an action.
3. The system validates the current state of the resource against the allowed states of the action. Then the system executes the action asynchronously.
4. The operator tracks the operation to a terminal outcome and reviews the audit record.

**Postconditions**:
- The resource reflects the effect of the action. The invocation is auditable.

**Alternative Flows**:
- **Resource in a disallowed state**: The system rejects the invocation and reports the current state. The system still audits the rejected attempt.

#### Assess the Impact of a Change

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-usecase-impact-analysis`

**Actor**: `cpt-cf-infrastructure-resource-manager-actor-system-administrator`

**Preconditions**:
- Resources with derived relationships exist. The administrator can read the relevant resource types.

**Main Flow**:
1. The administrator selects a resource that is a candidate for maintenance or decommissioning.
2. The administrator queries which resources depend on it, to a chosen traversal depth.
3. The system returns the dependency set and omits neighbors whose type the administrator cannot read.
4. The administrator plans the maintenance window against that set.

**Postconditions**:
- The blast radius of the planned change is known before the change is made.

**Alternative Flows**:
- **Traversal beyond the depth limit**: The system refuses the traversal and states the limit.

#### Place and Relocate a Deployment

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-usecase-placement`

**Actor**: `cpt-cf-infrastructure-resource-manager-actor-tenant-administrator`

**Preconditions**:
- A resource group that the administrator can write to exists, or none exists. If none exists, the tenant default group applies.

**Main Flow**:
1. The administrator submits a definition that names a target group. The system creates the deployment at that address.
2. The administrator later submits the same definition again, without a group name. Placement is unchanged.
3. The administrator relocates the deployment to another group as an explicit operation. The operation can carry a precondition against a stale view.
4. All live resources of the deployment move with it. Group membership converges within the stated bound.

**Postconditions**:
- The deployment is in the intended group. The vacated address is free for reuse.

**Alternative Flows**:
- **Group unknown, ambiguous by name, of the wrong kind, or another tenant's**: The system refuses each case with a distinct reason.
- **Group service unreachable**: Requests that must resolve an address fail closed. Requests addressed by deployment identifier are unaffected.

#### Delete an Owning Resource with Its Subtree

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-usecase-cascade-delete`

**Actor**: `cpt-cf-infrastructure-resource-manager-actor-sre-operator`

**Preconditions**:
- A resource owns a subtree. The operator holds delete authority over the parent.

**Main Flow**:
1. The operator requests deletion of the owning parent.
2. The system evaluates admission (protections, visibility, type authority, blast radius). Then it discloses what the deletion will destroy.
3. The operator confirms the disclosed extent.
4. The deletion of the parent commits. The owned subtree converges to deleted asynchronously, until no descendant remains. This convergence survives restarts.

**Postconditions**:
- The parent and every owned descendant are gone. The teardown is visible in history.

**Alternative Flows**:
- **A protected or unauthorized descendant, or an oversized subtree**: The system refuses the request before any change and names the condition that fired.
- **No confirmation**: Nothing changes.

#### Adopt an Existing Estate via Discovery

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-usecase-discover-estate`

**Actor**: `cpt-cf-infrastructure-resource-manager-actor-system-administrator`

**Preconditions**:
- An active adapter exists for the provider. Discovery is enabled.

**Main Flow**:
1. The administrator triggers (or schedules) discovery for the adapter.
2. The system synchronizes provider inventory into IRM idempotently. It flags quota/license/policy violations and does not block.
3. The administrator assigns unowned resources to tenants from the discovery pool (bulk where needed).
4. Stakeholders receive a consolidated violation notification with remediation steps.

**Postconditions**:
- The existing estate is inventoried, owned, and governable in IRM.

**Alternative Flows**:
- **Repeated provider errors**: The circuit breaker suspends discovery for the adapter and alerts operators.

#### Migrate a Tenant to the Deployment-Scoped Model

- [ ] `p2` - **ID**: `cpt-cf-infrastructure-resource-manager-usecase-migrate-tenant`

**Actor**: `cpt-cf-infrastructure-resource-manager-actor-platform-engineer`

**Preconditions**:
- The tenant still holds data in the legacy model. The operator holds the dedicated migration permission, following the same precedent as the orphan-cleanup permission that `cpt-cf-infrastructure-resource-manager-fr-rbac` requires.

**Main Flow**:
1. The operator initiates migration for the tenant. The system expands the schema so both models are served concurrently.
2. The system backfills existing tenant data into the deployment-scoped model.
3. The operator triggers verification. The system confirms the backfilled data matches the legacy source.
4. The operator triggers the contract step. The system retires the legacy representation.

**Postconditions**:
- The tenant's data is fully represented in the deployment-scoped model. Legacy data stayed readable throughout, up to the contract step.

**Alternative Flows**:
- **Interruption at any phase**: The operator resumes migration from the interrupted phase rather than restarting it.
- **Administrative abort before the contract step**: The tenant remains on the legacy model with no data loss.

## 11. User Interaction and Design

The consoles below are frontend-scope deliverables (§5.2); their platform-side targets are settled there, not in this table. API and CLI usability is covered by the §7.1 latency NFRs and the §1.3 preview-adoption metric. This PRD sets no separate learnability target for the expert tooling these interfaces represent — that is a deliberate omission, not a gap.

| **Interface Name** | **Role** | **Steps** | **Mockup Screen** |
|--------------------|----------|-----------|-------------------|
| Graph Explorer | As a System Administrator, I want to view infrastructure topology so that I can plan maintenance and troubleshoot | 1. Open the resource graph for a scope<br>2. Filter by tenant, type, or tag<br>3. Highlight dependency paths. Export the view | — |
| Discovery Console | As a System Administrator, I want discovery health and pool management so that I can adopt estates safely | 1. Review adapter discovery status and errors<br>2. Configure schedules, maintenance mode, thresholds<br>3. Assign pooled resources to tenants | — |
| Day-2 Operations | As an SRE / Operator, I want to execute lifecycle actions so that I can operate workloads | 1. Select resources and view available actions<br>2. Trigger the action. Track it to completion (§12 #67)<br>3. Review the audit trail | — |
| Command-Line Interface | As an SRE / Operator, I want to preview, apply and roll back from a terminal so that I can work from automation and during incidents | 1. Validate a definition, then preview the classified change set (§12 #9)<br>2. Confirm and apply. The applied change equals the reviewed change (§12 #10)<br>3. Inspect history and roll back to a selected revision (§12 #15)<br>4. Before any destructive operation, confirm explicitly | — |
| Deployment Timeline | As an SRE, I want deployment progress and history so that I can diagnose and revert failures | 1. Open the timeline of a deployment<br>2. Inspect per-resource status of the apply in progress<br>3. Trigger rollback to a selected revision | — |

## 12. Acceptance Criteria

Requirements without a named criterion here are validated through the testing strategy stated in §6.

### Governance Cross-Cut

**1. Tenant isolation**
- **Given** an authenticated caller with tenant context
- **When** any IRM operation executes (query, mutation, action, deployment, discovery, graph)
- **Then** the operation MUST be scoped to the caller's tenant hierarchy
- **And** resources outside that hierarchy MUST NOT be readable, writable, or inferable

**2. Policy and quota gate**
- **Given** policy and quota constraints configured for a scope
- **When** provisioning, modification, or a lifecycle action is requested
- **Then** the request MUST be evaluated against quota first and policy second before any change executes
- **And** a denial MUST carry an actionable reason. An unavailable decision service MUST fail closed

**3. Audit completeness**
- **Given** IRM is operational
- **When** any mutation, action, deployment transition, or discovery outcome occurs
- **Then** an audit record MUST be emitted with full correlation context and zero secret content
- **And** idempotent replays MUST be distinguishable from fresh mutations

**4. Admission pipeline is fail-fast and type-stable**
- **Given** resource creation that passes through ordered admission checks, one of which rejects
- **When** the creation is processed
- **Then** the first rejection MUST abort with nothing persisted and the remaining checks skipped
- **And** enriched values MUST be what is validated and persisted, and no check can change the resource's type

**5. License gating covers the whole surface**
- **Given** a deployment without the platform license feature that entitles resource management
- **When** any IRM operation is attempted
- **Then** the operation MUST be refused by license gating

### Type System and Adapters

**6. Type registration**
- **Given** a valid resource type definition with schemas, actions, and capabilities
- **When** it is registered under a GTS identifier
- **Then** the type MUST become discoverable and deployable
- **And** an invalid definition MUST be rejected with the exact reason

**7. Adapter activation gate**
- **Given** a registered adapter without resource types
- **When** activation is requested
- **Then** activation MUST be refused until at least one type is contributed

**8. Type evolution never breaks running estates**
- **Given** a registered resource type with active resources
- **When** a new version of the type is registered, and separately its removal is requested
- **Then** existing resources MUST be unaffected by the version update. The removal MUST be refused while active resources of the type exist
- **And** a re-registration of an existing type MUST update it in place, and the response states which types were newly registered and which were updated

### Declarative Change Management

**9. Preview fidelity**
- **Given** a definition change against current state
- **When** a preview is requested
- **Then** every resource MUST be classified as exactly one of no-change, create, update, replace, delete
- **And** the preview MUST cause zero side effects and redact all secret values

**10. Zero-surprise apply**
- **Given** an approved preview
- **When** apply executes after the definition, state, or type metadata changed
- **Then** the apply MUST be rejected with the specific drift reason
- **And** an apply of an unchanged plan MUST execute exactly the previewed operations

**11. Idempotent re-apply**
- **Given** a definition already fully applied
- **When** the same definition is applied again
- **Then** every resource MUST classify as no-change, and the system MUST NOT issue any provider call or start any execution

**12. Duplicate-safe retry**
- **Given** a mutation request with an idempotency key that already completed
- **When** the identical request is retried
- **Then** the original outcome MUST be returned verbatim without re-execution
- **And** a concurrent duplicate MUST be rejected as in progress

**13. Idempotency windows and conflicts**
- **Given** a key with a recorded outcome, a key still in flight, and a key reused with a different body
- **When** each request arrives
- **Then** the recorded outcome MUST replay, marked as a replay. The in-flight duplicate MUST be refused as in progress. The different-body reuse MUST be refused as a distinct conflict
- **And** after the replay window elapses, the same key MUST execute as a fresh request

**14. Guardrail enforcement**
- **Given** a resource whose management policy is no-delete or no-touch
- **When** a change that modifies or destroys it is requested
- **Then** the change MUST be rejected (or, for no-delete, the provider object MUST be detached intact and remain queryable as an orphan)
- **And** the protection layer that fired MUST be identifiable from the rejection
- **And** an invocation of a day-2 action on a no-touch resource MUST be refused as a modification

**15. Rollback to revision**
- **Given** a deployment with retained revisions
- **When** rollback targets a revision in the resource's lineage
- **Then** the system MUST reconcile current actual state to that revision as a fresh plan, with the revision's frozen type metadata
- **And** a target outside the lineage or behind breaking type evolution MUST be rejected with the reason

### Day-2 and Topology

**16. Action state validation**
- **Given** a resource in a state not allowed by an action's definition
- **When** the action is invoked
- **Then** the invocation MUST be rejected with the current state
- **And** the rejected attempt MUST appear in the audit trail

**17. Topology convergence**
- **Given** resources that change (create, update, migrate, delete)
- **When** the graph is queried after the staleness bound
- **Then** nodes and typed relationships MUST reflect the changes
- **And** deletions MUST cascade to relationships, with no orphaned edges left after cleanup

**18. Traversal is bounded and single-kind beyond one hop**
- **Given** topology queries of increasing depth
- **When** a traversal deeper than one hop names no single relationship kind, or a traversal exceeds the depth limit
- **Then** each MUST be refused with the violated constraint stated
- **And** an in-limit traversal MUST paginate with cursors bound to the query they were issued for

### Discovery

**19. Non-blocking compliance**
- **Given** discovered resources exceeding quota or violating license or policy
- **When** synchronization runs
- **Then** all resources MUST be recorded, with resources in violation flagged by condition
- **And** stakeholders MUST receive a consolidated notification with remediation steps

### Placement and Groups

**20. Address-based create-or-update**
- **Given** a deployment definition and a target resource group
- **When** the definition is submitted against that group
- **Then** if the address is free, the system MUST create the deployment there. Otherwise, the system MUST update the deployment already at that address
- **And** a submission of the same definition against a different group MUST produce a separate independent deployment, not a relocation

**21. Apply never relocates**
- **Given** an existing deployment placed in a group
- **When** its definition is applied again
- **Then** placement MUST remain unchanged, and the system MUST NOT write group membership
- **And** relocation MUST only occur through the explicit move operation

**22. Group reference preconditions**
- **Given** a placement reference that is unknown, ambiguous by name, of the wrong kind, or owned by another tenant
- **When** placement is attempted
- **Then** the request MUST be refused with a distinct machine-readable reason for each case
- **And** the response MUST NOT reveal whether an invisible group exists

**23. Fail-closed on group-service outage**
- **Given** the resource-group service is unreachable
- **When** a request that must resolve a group address is made
- **Then** the request MUST fail closed as temporarily unavailable
- **And** requests addressed by deployment identifier rather than group MUST remain unaffected

**24. Default group self-healing**
- **Given** a tenant's default group is deleted out of band
- **When** placement next needs it
- **Then** it MUST be recreated with the same identity so that existing group-scoped access continues to function
- **And** if it was instead renamed out of band, the system MUST fail closed and surface the discrepancy rather than repair it silently

**25. Membership convergence within bound**
- **Given** a committed placement decision
- **When** the convergence bound elapses
- **Then** group membership MUST reflect that decision
- **And** the system MUST NOT leave a resource ungrouped at any observable point during propagation

### Authorization Granularity

**26. Revocation stops work in flight**
- **Given** a deployment executes on behalf of a caller
- **When** that caller's authority over the affected resources is definitively revoked
- **Then** the operation MUST be cancelled with the reason recorded
- **And** an unreachable decision service MUST be retried as transient rather than treated as a denial

### Destructive Operations

**27. Cascade refuses before it starts**
- **Given** a resource that owns a subtree. A descendant in the subtree is protected, out of the caller's visibility, or beyond the caller's type authority, or the subtree exceeds the blast-radius limit
- **When** deletion of the owning parent is requested
- **Then** the request MUST be refused before any resource is changed
- **And** an admissible cascade MUST converge until no owned descendant remains and MUST survive a process restart mid-teardown

### Type System and Adapter Onboarding

**28. Single-call adapter onboarding**
- **Given** a complete adapter package that declares an adapter, its resource types, its data-plane operations, the delegation scopes it requests, and its authorization policy
- **When** the package is submitted in one operation
- **Then** all of those MUST be registered as one unit and the adapter MUST become active
- **And** a repeated submission of the package MUST update the existing adapter rather than create a second one. It MUST narrow any operator-granted delegation to the scopes that the new package still declares

**29. Delegation requires a separate operator grant**
- **Given** an adapter package that declares the delegation scopes the adapter requests
- **When** the package is registered
- **Then** that registration alone MUST NOT grant delegated authority
- **And** a subsequent operator grant that names a scope the package did not declare MUST be refused. The operator MUST be able to disable delegated callbacks entirely

**30. Data-plane operation catalog is published**
- **Given** a registered resource type whose adapter declares provider operations on it
- **When** another platform service queries what can be granted for that type
- **Then** the system MUST return each operation with its required resource state, its input and output shape, and its maximum credential lifetime. It MUST also return the credential class and the deprecation status of each operation
- **And** the system MUST be able to state whether a given operation is available on a specific resource instance

**31. Adapter health is a definite answer**
- **Given** an adapter that is unreachable or returns an invalid health response
- **When** its health is requested
- **Then** the adapter MUST be reported unhealthy — not as an error of the health request itself
- **And** "cannot be determined" MUST be distinguishable from "unhealthy"

**32. Async provider work is bounded and duplicate-safe**
- **Given** an adapter that accepts work for asynchronous completion
- **When** polling exhausts the stated maximum duration, or the accepted response carries no pollable location, or an outbound call is retried after a process restart
- **Then** at budget exhaustion, the operation MUST be recorded as failed. Without a pollable location, the operation MUST fail immediately. A retried call MUST carry the same duplicate-safety key so that no second provider-side operation starts
- **And** on transient provider errors, polling MUST continue, while authorization and absence errors terminate the operation. A cancellation MUST record whether the provider-side cancel attempt succeeded

**33. Adapter retirement refuses while resources live**
- **Given** an adapter whose types back live resources
- **When** its removal is requested
- **Then** the removal MUST be refused
- **And** when no such resource remains, removal MUST succeed and MUST remove the type definitions that the adapter contributed

**34. Grantable catalog requires authority and names its entries**
- **Given** a caller without authority to read type definitions
- **When** the grantable-type catalog is read
- **Then** the read MUST be refused
- **And** an authorized read MUST return each type with its authorization identity, display name, and owning adapter

### Resource and Deployment Lifecycle

**35. Every managed resource is deployment-scoped**
- **Given** a caller creates a resource directly rather than through a declarative definition
- **When** the resource is created
- **Then** the system MUST wrap it in an automatically created single-resource deployment in the same transaction
- **And** that resource MUST thereafter have the same history, rollback and protection behavior as one declared in a multi-resource definition

**36. Acceptance is durable and lifecycle bounded**
- **Given** a mutation accepted for asynchronous execution
- **When** the process fails immediately after acceptance
- **Then** the resource and its tracking operation MUST already be durably recorded in their published states
- **And** the operation MUST reach a terminal state within its bounded lifetime

**37. Deletion never orphans and never strands**
- **Given** one resource whose creation the provider refused synchronously before it became addressable, and another whose creation outcome was never learned, both without provider identifiers
- **When** each is deleted
- **Then** the refused resource MUST delete without any provider call. The unknown-outcome resource MUST be refused and restored rather than reported deleted
- **And** for a resource that carries a provider identifier, the provider's answer MUST always decide its delete. A permanently refused delete MUST restore the pre-delete state, with the refusal reason readable on the resource

**38. Listing and pagination behave predictably**
- **Given** resources and deployments listed with filters, ordering, and a page cursor
- **When** a caller pages through results
- **Then** ordering MUST follow the requested published field, pages MUST carry opaque cursors, and a malformed cursor MUST be refused as a distinct client error
- **And** an update MUST operate on full desired state. A partial update is refused

**39. Deployment status is attributable and outputs are durable**
- **Given** a deployment whose apply partially failed
- **When** its status and outputs are read
- **Then** status MUST identify the failed members, each with a machine-readable reason. Outputs MUST retain the previously recorded values, with unresolvable entries omitted
- **And** before the first apply, outputs MUST read as empty rather than as an error

**40. Cancellation stops at a change boundary**
- **Given** a multi-resource apply in progress
- **When** cancellation is requested
- **Then** work already in flight MUST complete, remaining work MUST be skipped, and the operation MUST settle as cancelled — distinct from failed
- **And** a repeated request MUST be safe. Cancellation of a finished operation MUST report its final outcome

**41. Refresh reports drift without applying anything**
- **Given** a deployment whose provider state changed out of band
- **When** an operator refreshes it
- **Then** the outcome MUST report refreshed, drifted, unchanged, and failed counts and MUST NOT change any desired state
- **And** while an apply runs on the same scope, a refresh MUST be refused

**42. Conditional reads and preconditions are honored**
- **Given** a caller that holds a validator from a previous read, and separately a mutation that carries a stale precondition
- **When** each request is made against unchanged and changed state respectively
- **Then** the unchanged read MUST answer not-modified, and the stale mutation MUST be refused distinctly from other conflicts
- **And** a malformed validator MUST be treated as absent rather than rejected

**43. Definition validation reports the exact fault location**
- **Given** a declarative definition containing an invalid expression or a reference to a resource it does not declare
- **When** the definition is submitted for validation
- **Then** the system MUST reject it and MUST report the location of each fault within the definition
- **And** the system MUST NOT create or change any resource

**44. Conditional inclusion is fail-closed and visible**
- **Given** a definition attaching condition expressions to its resources
- **When** the definition is validated and applied
- **Then** a resource whose condition is false MUST be excluded from the planned set and reported as skipped in deployment status
- **And** a condition that cannot be evaluated, or yields a non-boolean value, MUST fail validation before any change is attempted

**45. Parameters are constrained and defaulted**
- **Given** a definition that declares constrained parameters, some with defaults and one required without a default
- **When** it is submitted with the optional parameters omitted, with the required one omitted, and with one constraint violated
- **Then** the omitted optionals MUST resolve to their defaults. Before anything executes, the missing required parameter and the violated constraint MUST each be refused and named individually
- **And** all parameter faults MUST arrive in the same response

**46. Replacement strategy is honored**
- **Given** a change to a field the resource type declares immutable
- **When** the change is applied
- **Then** the system MUST replace the resource rather than update it, with the strategy configured for that type or with the per-resource override
- **And** resources that depend on the replaced one MUST be re-pointed at the replacement

### Access Control

**47. Access boundaries hold**
- **Given** the access decisions the platform authorization path resolves for a caller
- **When** the caller attempts an operation those decisions forbid — managing access, mutating a resource from a read-only grant, touching a tenant resource from an adapter-scoped grant, or performing orphan cleanup without the dedicated permission
- **Then** each attempt MUST be refused
- **And** each operation the decisions permit MUST succeed

**48. Partial type authority narrows rather than fails**
- **Given** a caller authorized to read some but not all resource types present in a result
- **When** the caller lists resources, reads a deployment's members, or queries topology
- **Then** the listing MUST return the union of what the caller can read. The deployment's members MUST all remain visible, with unreadable payloads withheld and explicitly marked as withheld. Topology MUST omit unreadable neighbors and not disclose the omission

**49. Write admission is atomic with preview parity**
- **Given** a plan that touches several resource types, one of which the caller cannot write
- **When** the caller previews and then applies it
- **Then** each MUST be refused as one atomic decision that names every denied type
- **And** a grant of the missing type authority MUST make both succeed

### Secret Handling

**50. Secrets never appear in cleartext**
- **Given** a resource type that declares a field as secret, and a change that supplies a value for it
- **When** the change is applied and afterwards inspected through every surface the system offers. These surfaces are stored state, revision history, previews, unified history, logs, metrics, published events, and error messages
- **Then** the supplied value MUST NOT appear in cleartext in any of them
- **And** change detection on that field MUST still correctly distinguish a changed value from an unchanged one
- **And** equal secret values in different tenants MUST produce unrelated comparison artifacts

**51. A field becoming secret re-protects existing data**
- **Given** existing resources that hold values in a field that a re-registered type now declares secret
- **When** the type is re-registered
- **Then** the already-persisted values MUST be re-protected before further changes on affected types proceed

### Adapter Trust Boundary

**52. Outbound calls are individually credentialed and confined**
- **Given** an adapter registered with an endpoint that resolves to a platform-internal address, or that redirects, or whose address changes after registration
- **When** the system calls it
- **Then** the call MUST fail closed in each case, with the destination validated again on that attempt rather than trusted from registration
- **And** a call to a legitimate adapter MUST carry a credential usable only for that adapter and that operation. When the work outlives the credential, the credential MUST be refreshed rather than reused

**53. Hostile or broken adapter responses are contained**
- **Given** an adapter response body that is oversized, malformed, omits the provisioned resource's identity, or imitates the system's own internal markers
- **When** the response is processed
- **Then** each MUST be rejected with no change to recorded state
- **And** provider error text surfaced to a caller MUST be truncated. An ambiguous provider state MUST be treated as not-yet-ready rather than ready

### Placement Propagation

**54. Propagation is ordered, durable and honest about failure**
- **Given** a committed placement decision
- **When** propagation runs, including across a process restart and with several instances that process concurrently
- **Then** the resource MUST NOT be observably ungrouped at any point. The end state MUST be the same as a single uninterrupted run. The system MUST NOT apply any membership twice
- **And** a propagation that fails permanently MUST be parked with an observable count and a stated operator action that resumes it, rather than retried indefinitely
- **And** verification covers the write order — the new membership is written before the old one is removed — including a crash between the two writes

**55. Placement drift is repaired in both directions**
- **Given** membership recorded with no matching managed resource, and a managed resource with missing or wrong membership
- **When** the periodic reconciliation runs
- **Then** the stale membership MUST be removed, and the missing membership MUST be propagated again
- **And** the sweep MUST touch only membership records for resource types that IRM manages; records of other platform components stay intact
- **And** a pass that stopped early MUST report that it did

**56. Relocation refuses stale and occupied cases distinctly**
- **Given** a relocation request that carries a precondition that no longer matches, and separately one whose destination address is already occupied
- **When** each is submitted
- **Then** each MUST be refused, and the two refusals MUST be distinguishable
- **And** a relocation whose target is already the current group MUST succeed as a no-op, with nothing changed

### Destructive Operation Disclosure

**57. A cascade discloses its extent and requires confirmation**
- **Given** an admissible cascade over a subtree of owned resources
- **When** deletion of the owning parent is requested
- **Then** before execution, the system MUST disclose the extent of what will be destroyed. The system MUST require explicit confirmation of that extent
- **And** an unconfirmed request MUST change nothing

### Discovery and Inventory

**58. Discovery is controllable and idempotent**
- **Given** an adapter for which discovery is configured
- **When** discovery runs repeatedly against an unchanged provider estate
- **Then** the second and subsequent runs MUST leave IRM inventory unchanged
- **And** an operator MUST be able to suspend discovery for that adapter. Repeated provider failures MUST suspend it automatically and alert operators

**59. Discovered resources reach an owner**
- **Given** provider resources discovered with no determinable tenant
- **When** synchronization completes
- **Then** they MUST be recorded and MUST be visible as unassigned
- **And** an administrator MUST be able to assign them, individually or in bulk, after which they behave as any other managed resource

**60. Resources missing from the source are handled predictably**
- **Given** a resource previously discovered that the provider no longer reports
- **When** discovery runs
- **Then** the configured policy for missing resources MUST be applied. The default is to flag, not to delete
- **And** absence from a single run MUST NOT destroy the resource as a side effect

### Capabilities and Tags

**61. Capabilities are enabled per resource instance**
- **Given** a resource whose type declares an optional capability
- **When** an administrator enables, reconfigures, then disables it on one resource
- **Then** each change MUST take effect on that resource alone and MUST be audited
- **And** the capabilities available for a resource MUST be discoverable from its type

**62. Tags are inherited and usable for selection**
- **Given** tags set on a resource group and on individual resources within it
- **When** resources are listed with a tag filter
- **Then** resources MUST be selectable by tag, including by a tag inherited from their group
- **And** a tag set explicitly on a resource MUST take precedence over the inherited value

### Retention and Accounting

**63. Retained data is purged when its window elapses**
- **Given** deleted resources, deleted deployments, completed operations, spent duplicate-detection keys and expired revisions, each past its retention window
- **When** the purge process runs
- **Then** each MUST be removed
- **And** data still inside its window MUST be retained and MUST remain reachable through the surfaces that expose it

**64. Detached provider objects are bounded and reclaimable**
- **Given** a tenant at its orphan capacity
- **When** an operation that detaches a further provider object is requested
- **Then** the operation MUST be refused, and the refusal reports the current count against the capacity
- **And** an operator MUST be able to list detached objects and remove them through the explicit confirmed action

### History and Migration

**65. Unified history reconstructs the change timeline**
- **Given** a deployment that was applied several times, rolled back, and refreshed
- **When** its history is requested
- **Then** the response MUST present those events in chronological order. The same MUST hold for a single resource across a replacement that changed its identity
- **And** history MUST NOT lag the underlying change beyond the stated staleness bound

**66. Migration is resumable and never strands a tenant**
- **Given** a tenant part-way through migration to the deployment-scoped model
- **When** the migration is interrupted, then resumed
- **Then** the migration MUST continue from where it stopped rather than restart or skip
- **And** the tenant's data MUST remain readable throughout. An administrator MUST be able to abort and not leave the tenant between models

**67. Day-2 action execution is tracked to a terminal outcome**
- **Given** a validated action invocation on a resource
- **When** the action executes
- **Then** the system MUST record an operation that the caller can track to success or failure. It MUST reflect the provider's resulting state on the resource
- **And** a failure MUST be reported with the provider's reason rather than recorded as success

### Dependency Unavailability

**68. Dependency unavailability is deterministic and never blocks committed delivery**
- **Given** any IRM dependency is unavailable
- **When** an operation whose correctness depends on that dependency is attempted, and separately when downstream event-delivery infrastructure is unavailable after a mutation has committed
- **Then** the dependency-dependent operation MUST refuse deterministically, naming the unavailable dependency, rather than guess or hang
- **And** the committed mutation MUST NOT be blocked by the event-delivery outage. Delivery MUST resume without loss once the infrastructure recovers, with no partial state left behind
- **And** the outage MUST be visible through an operational signal that names the failing dependency and is alertable, for the entire duration of the outage

### Non-Functional Requirements (Show-Stoppers)

**69. Interactive latency at scale**
- **Given** the declared scale (100 k+ resources per tenant, 1 M+ topology nodes)
- **When** core operations and single-node topology queries execute under sustained load
- **Then** p95 latency MUST be within 500 ms and 200 ms respectively
- **And** scoped lists MUST complete within 2 s at p95

**70. Availability**
- **Given** production operation measured monthly
- **When** availability and durability are evaluated
- **Then** the management surface MUST meet ≥ 99.9 % availability and ≥ 99.999 % durability
- **And** policy and authorization failures MUST fail closed

**71. Crash-safe execution**
- **Given** a process failure during a multi-resource apply
- **When** execution resumes
- **Then** the system MUST NOT apply any resource twice
- **And** the operation MUST reach a terminal state visible in history

## 13. Dependencies

"Direction" states which side initiates the call: outbound — IRM calls the system; inbound — the system calls IRM; bidirectional — both sides initiate. (Contract entries in §9.2 use the template vocabulary instead: who provides or requires the contract.) Criticality is the highest §5.1 scope priority that requires the integration.

| Dependency | Description | Direction | Criticality |
|------------|-------------|-----------|-------------|
| Policy Decision Service | Admission, policy, quota, and license-entitlement decisions (fail-closed) | outbound | p1 |
| RBAC Engine | Role definitions and scope-based access resolution | outbound | p1 |
| AM and IdP | Authentication, subject identity, tenant context | inbound | p1 |
| Persistence layer | Durable storage with atomic reservations, consistency guards, and cursor pagination for idempotency, history, and stable pagination (database-agnostic) | outbound | p1 |
| Infrastructure Adapters | Provider adapters that implement the adapter contract | outbound | p1 |
| Workflow Executor | Long-running operation substrate, reached through a plugin contract | bidirectional | p1 |
| Type Identifier Service | Platform type-identifier allocation and resolution (IRM owns the resource-type registry itself) | outbound | p1 |
| Event & Audit Consumers | Delivery of domain and audit events to consumers | outbound | p1 |
| Resource Group Service | Group existence, membership, and default-group semantics. The decision point compiles group-scoped access from the membership it holds | outbound | p1 |
| Token Issuer | Mints the per-call credentials used for outbound adapter traffic | outbound | p1 |
| Grant Issuance Service | Consumes the data-plane operation catalog and resource resolution that IRM publishes | inbound | p1 |

## 14. Assumptions

- The Policy Decision Service, RBAC Engine, AM, and IdP are operational and expose stable contracts.
- Infrastructure adapters are semi-trusted external components. Their responses are validated and bounded.
- A durable workflow engine is available through the executor plugin contract.
- Persistence supports atomic reservations, consistency guards, and cursor pagination as required by idempotency and history.
- Tenants receive a provisioned secret salt at creation for digest-based secret comparison, per the mechanism recorded in the §2 "IRM-level recorded choices" row.
- IRM is pre-GA: one-shot breaking migrations are acceptable (no dual-publish windows).
- All new entities use time-sortable unique identifiers to support stable pagination.
- The platform event broker provides the delivery substrate for domain and audit events, meeting the at-least-once, ordered, deduplicable, loss-detectable delivery that `cpt-cf-infrastructure-resource-manager-fr-audit-events` requires.

## 15. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Performance at 1 M+ topology nodes unvalidated | Latency SLOs are missed at enterprise scale. The graph value proposition is void | Scale testing occurs before GA. The storage strategy decision is gated on benchmarks |
| Event delivery substrate integration is pending | Downstream consumers do not receive domain and audit events until the binding lands | The platform event broker is the assumed transport (its ADR defines the envelope convention recorded in §2); emitter contracts stay stable so the binding lands without contract changes |
| Deferred secret hardening (Phase 2) | Workflow payloads and retention windows carry residual exposure | Restrict execution-substrate access. Shorten retention. Schedule the Phase-2 spec |
| Specification-code drift once implementation begins | The build diverges from this PRD, and the divergence is not noticed | Every requirement is verifiable through a named criterion in §12 or the §6 testing strategy. Validate at each milestone. Route scope changes through change requests, not through code |
| Migration to deployment-scoped model fails mid-tenant | A tenant is stranded between data models | Use phased migration with verification, abort, and resume. Legacy data stays readable until the contract step |
| Visualization complexity at 10 k+ nodes | Unusable topology UI | Progressive loading and aggregation in the frontend design |
| Group membership lag widens the window in which revoked access still resolves | A user removed from a group can briefly still reach its resources | The convergence bound is a stated NFR and is alertable. Drift reconciliation is the backstop |
| Placement rows parked after permanent failure stay parked on a quiet tenant | Group membership silently diverges from placement for that deployment | The parked count is an alertable metric with a documented operator recovery path. The resume trigger is settled in the technical design |
| The decision point trusts the trusted system actor, and the actor does not bypass it | A defect in the clamp widens internal authority | Elevation is confined to named call sites and is individually attributable. A pre-GA security review gates release |
| Adapter onboarding mutates platform authorization policy | A malicious or careless adapter package can widen access | Onboarding is restricted to tenant-wide administrative authority. Policy changes are attributable to the adapter. Packages are integrity-verified and carry an exposed trust level (`cpt-cf-infrastructure-resource-manager-fr-manifest-onboarding`) |

## 16. Open Questions

These decisions must be made before or during design, each with an owner and a date. A question is listed here only if its answer changes what is built. Anything already settled is stated as a requirement in §6 or §7, not carried here.

| **Question** | **Owner** | **Target Date** | **Answer** | **Date Answered** |
|--------------|-----------|-----------------|------------|-------------------|
| Regulatory and privacy applicability: which regimes apply to a control plane that persists operator identity but no end-user personal data? For each regime that does not apply, what is the recorded reason? The answer determines whether data-minimization, residency, and erasure requirements are needed. | Product + Legal | 2026-09-30 | IRM ships no regime-specific behavior: the deployment operator determines applicability. The component owes the enabling primitives (configurable retention and purge, secret hygiene, attributable operator identity), which §6 already requires. Regime-imposed obligations are layered on them per deployment. | 2026-08-03 |
| Reference load profile behind the p95 targets: concurrent callers, sustained request rate, read/write mix and peak multiplier, including growth projections, burst patterns, and the discovery ingestion-throughput target. The profile is required before the latency NFRs can be validated. | Head of Platform Architecture | 2026-09-30 | — | — |
| Region as a scope dimension: multi-region management is a known platform direction. How does a region or placement dimension enter the deployment address, identifiers, and group semantics without breaking existing addresses? | Head of Platform Architecture | 2026-10-31 | — | — |
| Adapter backend-instance model: can one adapter serve several configured backend integrations, each with its own capability governance and placement-scope binding? The current model neither supports nor precludes this; it is a known extension point the adapter-contract design must not hard-code away with a 1:1 adapter-to-backend assumption. | Head of Platform Architecture | 2026-10-31 | — | — |
| Availability coverage: is the stated availability target continuous or business-hours, and what maintenance allowance can be excluded from its measurement? | Head of Platform Architecture | 2026-09-30 | Continuous (24/7). Planned maintenance is excludable from the measurement only when announced in advance, capped at 4 hours per month. The cap is stated in the availability NFR threshold. | 2026-08-03 |
| Per-tenant admission control: does a tenant need a ceiling on concurrent deployments and on request rate, beyond the per-endpoint limiting that the platform edge provides? | Head of Platform Architecture | 2026-10-31 | Not in this release: per-endpoint limiting at the platform edge is the stated protection (§9.1). If production data shows tenant-level interference, a per-tenant ceiling returns as a change request. | 2026-08-03 |
| Are two further protection layers needed for the single management-policy mechanism — deployment-level deny settings with a configurable unmanage behavior, and separate per-resource hard guards? | Head of Platform Architecture | 2026-10-31 | Neither layer is added: one mechanism with three levels stands, per the fr-guardrails rationale. Reintroduction requires a change request informed by production experience. | 2026-08-03 |
| Does management policy gate day-2 actions? "No-touch" refuses modification, but whether an action (stop, resize, snapshot) counts as modification is unstated. A no-touch resource can still be mutated through an action. | Head of Platform Architecture + Security | 2026-09-15 | Yes: action execution is a modification. An invocation of an action on a no-touch resource is refused before dispatch. No-delete does not restrict actions. This is stated in fr-action-execution. Enforcement is an implementation ticket. | 2026-08-03 |
| A no-delete parent that owns a subtree: deletion detaches the parent intact. But whether its owned descendants are then removed or preserved, or whether the request is refused outright, is undefined. The interaction between detach-instead-of-delete and cascade needs a rule. | Head of Platform Architecture | 2026-10-31 | Refused outright: a protected parent that owns live descendants can be neither deleted nor detached. The request fails admission. Detach-instead-of-delete applies only to a resource that owns nothing (fr-cascade-admission). | 2026-08-03 |
| Are three further limits needed, beyond the request-body bound that constrains deployment size today? The candidates are maximum resources per deployment, maximum dependency depth, and retained history per tenant. | Head of Platform Architecture | 2026-10-31 | Not introduced: the request-body bound constrains definition size, cycle detection bounds the dependency graph, and retention bounds history. Reintroduction requires a change request backed by production data. | 2026-08-03 |
| Business success metrics for the five goals in §1.3: baseline and target for each metric in the §1.3 metric table (metric and data source are already defined there). | Product | 2026-09-30 | — | — |
| Does continuous drift detection belong to IRM or to infrastructure adapters? This scope assigns it to adapters (§2). | Head of Platform Architecture | 2026-09-30 | Confirmed: infrastructure adapters own continuous reconciliation. IRM provides on-demand refresh and preview (§2, §5.2). A revisit requires a change request. | 2026-08-03 |
| Policy-execution engine: adapter packages register policy bundles (`cpt-cf-infrastructure-resource-manager-fr-manifest-policy`); their evaluation requires an execution engine outside IRM, consumed through the fail-closed policy-gating contract (`cpt-cf-infrastructure-resource-manager-fr-policy-gating`). Which engine evaluates the bundles is a design decision. The selected engine MUST satisfy the admission NFRs: fit the p95 mutation budget (§7.1), incur no cold start on the hot path, and degrade fail-closed (§6.8). | Head of Platform Architecture | 2026-10-31 | — | — |
| Workflow Executor evolution: `cpt-cf-infrastructure-resource-manager-contract-workflow-executor` (§9.2) defines a plugin contract with a no-op default, and today one plugin implementation exists. Whether and how that implementation is replaced is a design decision. Related platform documentation gaps to close before that design: the Grant Issuance Service has no gear or design documentation, the types-registry has a PRD but no design documentation, and the authz-resolver has platform-level authorization documentation (`docs/arch/authorization/` — design and ADRs, AuthZEN evaluation model) but no gear-level PRD or DESIGN. | Head of Platform Architecture | 2026-10-31 | — | — |

## 17. Reference Materials

The materials below are external standards that the requirements refer to by name. None is required to read or validate this PRD.

| **Material** | **Link** | **Comments** |
|--------------|----------|--------------|
| RFC 2119 — Requirement keyword levels | https://datatracker.ietf.org/doc/html/rfc2119 | Meaning of MUST / MUST NOT / SHOULD / MAY in this document |
| RFC 9457 — Problem Details | https://datatracker.ietf.org/doc/html/rfc9457 | Convention for actionable machine-readable error reasons |
| RFC 6902 — JSON Patch | https://datatracker.ietf.org/doc/html/rfc6902 | Informative analogy for the five-operation classification model; not a normative contract |
| RFC 9562 — UUID versions | https://datatracker.ietf.org/doc/html/rfc9562 | Time-sortable identifiers behind stable pagination |
| IETF Idempotency-Key header draft | https://datatracker.ietf.org/doc/draft-ietf-httpapi-idempotency-key-header/ | Duplicate-safe mutation convention |
| CloudEvents | https://cloudevents.io/ | Envelope convention for the published event stream |
| Common Expression Language (CEL) | https://github.com/google/cel-spec | Expression language used in declarative definitions |
| ISO/IEC 25010:2023 | https://www.iso.org/standard/78176.html | Quality characteristics the Five Quality Vectors analysis maps to |
| WCAG 2.2 | https://www.w3.org/WAI/standards-guidelines/wcag/ | Accessibility reference for the operator interface owners |
| OWASP ASVS | https://owasp.org/www-project-application-security-verification-standard/ | Baseline the security requirements (§6.8, §6.9) are verifiable against |

## 18. Traceability

- **Upstream**: No UPSTREAM_REQS document exists — this is a PRD-first consolidation of the platform's earlier IRM requirement material (see the Change Log).
- **Downstream**: The technical design is pending; §16 lists the decisions it owes.
- **Code**: No implementation is bound to this document yet.
- Requirement-level traceability is carried by the stable `cpt-cf-infrastructure-resource-manager-*` identifiers on every actor, requirement, interface, contract, use case, and acceptance criterion.
