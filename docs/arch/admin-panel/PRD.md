# PRD — Integrated Admin Panel

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
  - [5.1 Admin Shell & Modes](#51-admin-shell--modes)
  - [5.2 Admin Resource Model & Extensibility](#52-admin-resource-model--extensibility)
  - [5.3 API-Driven Admin](#53-api-driven-admin)
  - [5.4 Resource Coverage (v0)](#54-resource-coverage-v0)
  - [5.5 Session, Capability, and Context](#55-session-capability-and-context)
  - [5.6 Tenant Isolation](#56-tenant-isolation)
  - [5.7 User Management](#57-user-management)
  - [5.8 Raw Database Fallback](#58-raw-database-fallback)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Gear-Specific NFRs](#61-gear-specific-nfrs)
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

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

The Integrated Admin Panel is a Django-admin-like management UI for `gears-rust`. It gives operators and tenant administrators a generated, metadata-driven web console to manage gear-owned resources (tenants, resource groups, types, gateway routes, gear status, and example domain objects) through existing Gears APIs as the authority boundary. It ships in two flavours: a **platform/operator** console with full cross-tenant data management, and a **tenant-scoped** console for customer/tenant administrators that exposes only the current tenant's data, governed by backend authorization and tenant isolation.

The panel is generated from resource metadata and the aggregated OpenAPI specification rather than hand-built per resource, so new admin objects, fields, views, filters, and actions can be added by registering descriptors instead of editing the core admin app.

### 1.2 Background / Problem Statement

`gears-rust` exposes a rich set of REST APIs (account management, resource groups, types registry/GTS, API gateway, gear orchestrator, nodes registry, file parser, example gears), aggregated behind the API Gateway as a single OpenAPI document with vendor extensions for OData filtering, ordering, and rate limits. Today there is no first-party UI to manage these resources: operators rely on raw API calls, Swagger UI, or direct database access. There is no tenant-scoped self-service admin for customers, and no consistent, safe path for cross-tenant operator management.

A vendor composing gears into a product needs an out-of-the-box admin surface that respects the platform's defense-in-depth model — authentication, authorization, tenant isolation, scoped DB access — without re-implementing it in the UI. The admin must prefer the safe API boundary, fall back to raw database access only for operator-only resources that have no API yet, and remain additive so each gear can contribute its own admin resources as the platform grows.

The platform's authorization model currently distinguishes principals only by identity and home tenant; there is no built-in "platform admin vs tenant admin" mode, and no endpoint that returns the caller's admin context (mode, capabilities, enabled gears). The admin panel must introduce a minimal, clearly non-production admin-role model in the static auth plugins for the first version, and an admin-context endpoint, while keeping the backend the final authority.

### 1.3 Goals (Business Outcomes)

- Operators manage all authorized tenants and gear-owned resources from a single console, with cross-tenant and destructive operations gated by confirmation and audit logging.
- Tenant administrators manage only their own tenant (or authorized subtree) with no access to global lists or platform internals.
- New admin resources and fields are added by registering metadata, without editing the core admin application.
- The admin reuses Gears APIs and the platform security model as the authority boundary; raw database access is an explicit, audited, operator-only fallback.
- A working v0 ships integrated with the example server (or as a `make admin` sidecar) with e2e coverage, web-docs, and README updates.

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Admin resource | A manageable object exposed in the admin panel (e.g. tenant, resource group, type, gateway route), described by a resource descriptor. |
| Resource descriptor | Metadata defining a resource's key, owning gear, API/DB source, fields, views, filters, sort/relation fields, tenant-scope strategy, actions, required capabilities, and safety level. |
| Platform admin | An admin mode with cross-tenant authority over gear-owned resources, able to switch tenant context and perform operator-only/destructive actions. |
| Tenant admin | An admin mode scoped to the current tenant or authorized subtree, with no access to global tenant lists or platform internals. |
| Admin context | The caller's current principal, tenant, admin mode, enabled gears, and capabilities, fetched at startup. |
| Safety level | A resource/action classification: `normal`, `destructive`, `operator-only`, or `read-only`. |
| Data provider | The frontend adapter that translates admin list/read/create/update/delete/custom-action calls into Gears API requests. |
| Raw database fallback | Operator-only, allowlisted, default-read-only access to database tables for resources that have no safe API yet. |
| OpenAPI discovery | Deriving default resource fields, operations, filters, and ordering from the aggregated OpenAPI spec and its `x-odata-*` extensions. |

## 2. Actors

> **Note**: Stakeholder needs are managed at project/task level by steering committee. Document **actors** (users, systems) that interact with this gear.

### 2.1 Human Actors

#### Platform Operator

**ID**: `cpt-admin-panel-actor-platform-operator`

- **Role**: A first-party operator administering the whole deployment across all authorized tenants and gear-owned resources.
- **Needs**: Cross-tenant visibility and management, tenant-context switching, operator-only and destructive operations with confirmation and audit, an operational summary of enabled gears and resources.

#### Tenant Administrator

**ID**: `cpt-admin-panel-actor-tenant-admin`

- **Role**: A customer/tenant administrator managing data within their own tenant or authorized tenant subtree.
- **Needs**: Tenant-scoped lists, details, relations, and actions; no exposure to global tenant lists or platform internals; reliance on backend isolation rather than hidden UI controls.

#### Gear Developer

**ID**: `cpt-admin-panel-actor-gear-developer`

- **Role**: Authors gears and contributes admin resource descriptors and admin metadata alongside the gear's API routes.
- **Needs**: An additive registration mechanism to expose new objects, fields, filters, and actions without modifying the core admin app.

### 2.2 System Actors

#### API Gateway

**ID**: `cpt-admin-panel-actor-api-gateway`

- **Role**: Serves the aggregated OpenAPI specification and proxies all gear REST routes under the configured prefix; the admin panel's primary integration surface.

#### AuthN / AuthZ / Tenant Resolvers

**ID**: `cpt-admin-panel-actor-security-resolvers`

- **Role**: Authenticate the principal, evaluate authorization decisions, and resolve tenant scope server-side; the final authority for every admin action.

#### Gears REST Services

**ID**: `cpt-admin-panel-actor-gear-services`

- **Role**: Account management, resource group, types registry, gear orchestrator, nodes registry, file parser, and example gears that own the resources the admin panel manages.

## 3. Operational Concept & Environment

> Foundational constraints (runtime, lifecycle, transport, security model) are defined at repository level. See [`docs/ARCHITECTURE_MANIFEST.md`](../../ARCHITECTURE_MANIFEST.md), the foundational [`guidelines/`](../../../guidelines), and the authorization design [`docs/arch/authorization/DESIGN.md`](../authorization/DESIGN.md). Only admin-panel-specific constraints are documented here.

### 3.1 Gear-Specific Environment Constraints

- The admin panel frontend is a browser SPA. The monorepo is otherwise pure Rust with no existing JavaScript/TypeScript build tooling; introducing a frontend toolchain (or a separate repository) is an explicit project decision (see Open Questions).
- The aggregated OpenAPI document and all gear routes are served by the API Gateway under a configurable prefix (default `/cf`); the admin panel depends on this discovery surface being reachable.
- The first version targets the static (demo) auth, authz, and tenant-resolver plugins; these are explicitly non-production. Platform-admin vs tenant-admin roles are stubbed in the static auth plugins for v0.
- Tenant isolation is enforced server-side (Secure ORM tenant-subtree predicates and authorization decisions); the UI must not implement isolation logic.

## 4. Scope

### 4.1 In Scope

- A metadata-driven admin shell with platform and tenant admin modes.
- A current admin-context/session view (principal, tenant, admin mode, enabled gears, capabilities).
- An enabled-gears operational summary.
- A resource registry and capability-driven generated navigation.
- Tenant management: list/tree/detail, create/update/suspend/unsuspend/soft-delete, metadata read/write/delete, conversion requests list/detail/action handling.
- Resource Group management: list/tree/detail/create/update/delete and memberships, where the API is stable.
- Types/GTS management: list/detail/create/update/delete, where the API is stable.
- API Gateway upstreams and routes management, where the API is stable.
- Gear/instance status summaries from the gear orchestrator.
- A user-management placeholder shown when no IdP plugin supports user operations.
- An API-driven data provider with OpenAPI discovery, OData filtering/ordering where advertised, cursor pagination, custom actions (suspend, unsuspend, approve, reject, cancel, retry, resolve, deprovision), and normalized error/list-metadata handling.
- A minimal platform-admin vs tenant-admin role stub in the static auth plugins, and an admin-context endpoint.
- Integration with `apps/cf-gears-example-server` or a `make admin` sidecar, e2e tests, web-docs updates, and README updates.

### 4.2 Out of Scope

- Full policy editor and role editor.
- Credential store editor and deep plugin configuration editor.
- Billing management and audit log browsing UI.
- Password reset and IdP-native group management.
- Tenant-facing raw database admin.
- A production-grade identity provider; v0 uses the non-production static auth plugins.
- The gear-contributed admin-metadata registration mechanism beyond a hardcoded v0 resource registry (deferred; see Open Questions and FR priorities).
- gRPC service registry management and deployment-mode mutation in the gear orchestrator.

## 5. Functional Requirements

> **Testing strategy**: All requirements verified via automated tests (unit, integration, e2e) targeting 90%+ code coverage unless otherwise specified. Document verification method only for non-test approaches. Priority `p1` marks the v0 scope; `p2`/`p3` mark later increments.

### 5.1 Admin Shell & Modes

#### Two Admin Modes

- [ ] `p1` - **ID**: `cpt-admin-panel-fr-admin-modes`

The admin panel **MUST** support two admin modes — platform admin and tenant admin — and select the active mode from the backend-provided admin context, not from a user-toggleable UI control.

- **Rationale**: The two flavours are the central requirement; mode must derive from backend authority to prevent privilege escalation via UI.
- **Actors**: `cpt-admin-panel-actor-platform-operator`, `cpt-admin-panel-actor-tenant-admin`

#### Platform Admin Capabilities

- [ ] `p1` - **ID**: `cpt-admin-panel-fr-platform-mode`

In platform admin mode the panel **MUST** allow managing all authorized tenants and gear-owned resources, switching tenant context when managing tenant-owned data, and viewing enabled gears, API resources, feature status, and operational summaries. Cross-tenant and destructive operations **MUST** require confirmation and **MUST** be audit logged.

- **Rationale**: Operator console must cover full data management with safety controls on dangerous actions.
- **Actors**: `cpt-admin-panel-actor-platform-operator`

#### Tenant Admin Scoping

- [ ] `p1` - **ID**: `cpt-admin-panel-fr-tenant-mode`

In tenant admin mode the panel **MUST** restrict management to the current tenant or authorized tenant subtree, **MUST** show tenant-owned objects only, and **MUST NOT** expose global tenant lists or platform internals by default. Scoping **MUST** rely on backend tenant isolation, not on hidden UI controls.

- **Rationale**: Tenant self-service must never leak cross-tenant data; enforcement is the backend's responsibility.
- **Actors**: `cpt-admin-panel-actor-tenant-admin`

#### Admin Shell & Generated Navigation

- [ ] `p1` - **ID**: `cpt-admin-panel-fr-admin-shell`

The panel **MUST** provide an admin shell that renders navigation generated from the registered admin resources and the caller's capabilities, showing or hiding resources based on backend capabilities.

- **Rationale**: Capability-driven, generated navigation is core to the metadata-driven design.
- **Actors**: `cpt-admin-panel-actor-platform-operator`, `cpt-admin-panel-actor-tenant-admin`

### 5.2 Admin Resource Model & Extensibility

#### Resource Descriptor Model

- [ ] `p1` - **ID**: `cpt-admin-panel-fr-resource-descriptor`

The panel **MUST** treat each manageable object as an admin resource described by a descriptor that defines: resource key, owning gear, API route or database source, list/detail/create/update fields, search/filter fields, sort fields, relation fields, tenant-scope strategy, supported actions, required capabilities, and safety level (`normal`, `destructive`, `operator-only`, `read-only`).

- **Rationale**: The descriptor is the contract that drives generated screens and access decisions.
- **Actors**: `cpt-admin-panel-actor-gear-developer`

#### Generated Screens from Metadata

- [ ] `p1` - **ID**: `cpt-admin-panel-fr-generated-screens`

The panel **MUST** generate list, create, edit, and detail screens from resource metadata where possible, and **MUST** allow per-resource overrides for custom workflows.

- **Rationale**: Generation keeps the admin additive and low-maintenance while permitting bespoke flows.
- **Actors**: `cpt-admin-panel-actor-gear-developer`

#### Field Descriptors

- [ ] `p2` - **ID**: `cpt-admin-panel-fr-field-descriptors`

A field descriptor **MUST** be able to define label, type, visibility, read-only state, validation, relation, widget, and permission.

- **Rationale**: Rich field metadata enables faithful generated forms and per-field access control.
- **Actors**: `cpt-admin-panel-actor-gear-developer`

#### Gear-Contributed Admin Metadata

- [ ] `p2` - **ID**: `cpt-admin-panel-fr-gear-contributed-metadata`

Each gear **MUST** be able to contribute its own admin resource descriptors alongside its API routes, and admin registration **MUST** be additive so new objects and fields can be added without editing the core admin app. For v0, a hardcoded resource registry **MAY** substitute for the gear-contributed mechanism.

- **Rationale**: Long-term extensibility goal; v0 uses a hardcoded registry to avoid blocking on a new backend mechanism.
- **Actors**: `cpt-admin-panel-actor-gear-developer`

### 5.3 API-Driven Admin

#### OpenAPI Resource Discovery

- [ ] `p1` - **ID**: `cpt-admin-panel-fr-openapi-discovery`

The panel **MUST** discover resources from the aggregated OpenAPI specification plus gear-provided admin metadata, using OpenAPI schemas for default fields and form generation and OpenAPI operations for list, read, create, update, delete, and custom actions.

- **Rationale**: OpenAPI-first generation minimizes per-resource hand-coding.
- **Actors**: `cpt-admin-panel-actor-api-gateway`, `cpt-admin-panel-actor-gear-developer`

#### Partial CRUD Support

- [ ] `p1` - **ID**: `cpt-admin-panel-fr-partial-crud`

The panel **MUST** support resources without full CRUD, including resources missing `getOne`, `update`, or `delete` operations, and **MUST** clearly expose unsupported operations (especially IdP-backed operations) rather than presenting broken controls.

- **Rationale**: Many gear resources are read-only or action-only; the UI must degrade gracefully.
- **Actors**: `cpt-admin-panel-actor-platform-operator`, `cpt-admin-panel-actor-tenant-admin`

#### Custom Actions

- [ ] `p1` - **ID**: `cpt-admin-panel-fr-custom-actions`

The panel **MUST** support custom actions beyond CRUD — including suspend, unsuspend, retry, approve, reject, cancel, resolve, and deprovision — with confirmation flows for destructive or cross-tenant actions.

- **Rationale**: Tenant lifecycle and conversion workflows are action-driven, not plain CRUD.
- **Actors**: `cpt-admin-panel-actor-platform-operator`

#### Pagination, Filtering, and Ordering

- [ ] `p1` - **ID**: `cpt-admin-panel-fr-pagination-filtering`

The panel **MUST** support cursor pagination and **MUST** support OData filtering and ordering where advertised by the OpenAPI `x-odata-*` extensions, and **MUST** normalize list response metadata for the frontend.

- **Rationale**: Gear list endpoints use cursor pagination and advertise OData capabilities; the data provider must consume them.
- **Actors**: `cpt-admin-panel-actor-platform-operator`, `cpt-admin-panel-actor-tenant-admin`

#### Error Normalization

- [ ] `p1` - **ID**: `cpt-admin-panel-fr-error-normalization`

The panel **MUST** normalize Gears RFC-9457 problem/error responses into user-friendly admin messages.

- **Rationale**: Consistent, readable error handling is required across all gear responses.
- **Actors**: `cpt-admin-panel-actor-platform-operator`, `cpt-admin-panel-actor-tenant-admin`

### 5.4 Resource Coverage (v0)

#### Tenant Management

- [ ] `p1` - **ID**: `cpt-admin-panel-fr-tenants`

The panel **MUST** support tenant list/tree/detail, create, update, suspend, unsuspend, and soft-delete, plus tenant metadata list/read/write/delete and tenant conversion requests list/detail/action handling, using account-management APIs.

- **Rationale**: Tenants are the primary v0 resource and exercise lifecycle, metadata, and action workflows.
- **Actors**: `cpt-admin-panel-actor-platform-operator`, `cpt-admin-panel-actor-tenant-admin`

#### Resource Group Management

- [ ] `p1` - **ID**: `cpt-admin-panel-fr-resource-groups`

The panel **MUST** support resource group list/tree/detail/create/update/delete and memberships where the resource-group API is stable.

- **Rationale**: Resource groups are a stable, hierarchical v0 resource.
- **Actors**: `cpt-admin-panel-actor-platform-operator`, `cpt-admin-panel-actor-tenant-admin`

#### Types / GTS Management

- [ ] `p1` - **ID**: `cpt-admin-panel-fr-types-gts`

The panel **MUST** support type/GTS list/detail/create/update/delete where the types-registry API is stable.

- **Rationale**: Type definitions underpin metadata across gears and are a v0 target.
- **Actors**: `cpt-admin-panel-actor-platform-operator`

#### API Gateway Upstreams and Routes

- [ ] `p2` - **ID**: `cpt-admin-panel-fr-gateway-routes`

The panel **SHOULD** support API Gateway (OAGW) upstreams and routes management where the API is stable; CORS, auth, header, plugin, and rate-limit rule editing is later work.

- **Rationale**: Gateway management is valuable but its write APIs are feature-gated and lack OData; defer rich rule editing.
- **Actors**: `cpt-admin-panel-actor-platform-operator`

#### Gear and Instance Status

- [ ] `p1` - **ID**: `cpt-admin-panel-fr-gear-status`

The panel **MUST** show enabled gears and gear/instance status summaries from the gear orchestrator.

- **Rationale**: Operators need an at-a-glance operational summary of enabled gears.
- **Actors**: `cpt-admin-panel-actor-platform-operator`

#### Nodes, File Parser, and Example Resources

- [ ] `p3` - **ID**: `cpt-admin-panel-fr-other-resources`

The panel **SHOULD** expose nodes registry information, file-parser capabilities, and example domain resources (users-info, mini-chat) when those gears are enabled and expose them safely.

- **Rationale**: Broader coverage is desirable but secondary to core platform resources.
- **Actors**: `cpt-admin-panel-actor-platform-operator`

### 5.5 Session, Capability, and Context

#### Admin Context at Startup

- [ ] `p1` - **ID**: `cpt-admin-panel-fr-admin-context`

The panel **MUST** fetch the current admin context at startup — current principal, tenant, admin mode, enabled gears, and capabilities — and **MUST** show or hide resources based on backend capabilities, keeping backend authorization as the final authority.

- **Rationale**: All navigation, mode selection, and capability gating depend on a context fetch; this endpoint does not exist today and must be built.
- **Actors**: `cpt-admin-panel-actor-platform-operator`, `cpt-admin-panel-actor-tenant-admin`, `cpt-admin-panel-actor-security-resolvers`

#### Admin Role Stub

- [ ] `p1` - **ID**: `cpt-admin-panel-fr-role-stub`

The platform **MUST** provide a minimal platform-admin vs tenant-admin role distinction for v0 via the static (non-production) auth plugins, sufficient to drive admin mode and capabilities, and **MUST** clearly mark this as demo/static and non-production.

- **Rationale**: The security model has no admin-mode concept today; a clearly-marked static stub unblocks v0 without committing the production authorization model.
- **Actors**: `cpt-admin-panel-actor-security-resolvers`

#### Session State Handling

- [ ] `p1` - **ID**: `cpt-admin-panel-fr-session-states`

The panel **MUST** handle unauthenticated, unauthorized, expired-session, and feature-unavailable states, and **MUST** support feature-gated resources and optional gears.

- **Rationale**: Robust handling of auth and feature states is required for a usable, safe console.
- **Actors**: `cpt-admin-panel-actor-platform-operator`, `cpt-admin-panel-actor-tenant-admin`

### 5.6 Tenant Isolation

#### Server-Side Tenant Scope

- [ ] `p1` - **ID**: `cpt-admin-panel-fr-server-side-scope`

Tenant scope **MUST** be resolved server-side, derived from the authenticated security context where possible, and the panel **MUST NOT** allow arbitrary tenant ID escalation. Unauthorized access **MUST** collapse without leaking object existence.

- **Rationale**: Isolation must be structural, enforced by backend policy and Secure ORM, never by UI logic.
- **Actors**: `cpt-admin-panel-actor-security-resolvers`, `cpt-admin-panel-actor-tenant-admin`

#### Scoped Lists, Relations, and Lookups

- [ ] `p1` - **ID**: `cpt-admin-panel-fr-scoped-views`

Tenant admin lists, details, relations, actions, and lookups **MUST** be scoped by backend policy, and cross-tenant platform actions **MUST** be clearly marked in the UI.

- **Rationale**: Every view must respect tenant scope; operators must see clearly when an action crosses tenant boundaries.
- **Actors**: `cpt-admin-panel-actor-platform-operator`, `cpt-admin-panel-actor-tenant-admin`

### 5.7 User Management

#### IdP-Backed User Management

- [ ] `p2` - **ID**: `cpt-admin-panel-fr-user-management`

The panel **MUST** treat users as IdP-backed resources, **MUST** support tenant user list/create/delete when the IdP plugin supports those operations, and **MUST** show user management as unavailable when no IdP provider supports it. The panel **MUST NOT** assume a local users table exists.

- **Rationale**: User management depends on an IdP plugin; absent one, the panel must show a placeholder rather than break.
- **Actors**: `cpt-admin-panel-actor-platform-operator`, `cpt-admin-panel-actor-tenant-admin`

### 5.8 Raw Database Fallback

#### Operator-Only Raw DB Access

- [ ] `p3` - **ID**: `cpt-admin-panel-fr-raw-db`

Raw database admin **MUST** be available only to platform admins and disabled for tenant admins. Tables **MUST** be explicitly allowlisted and default to read-only; create/update/delete **MUST** require explicit opt-in. Secrets, credentials, tokens, plugin internals, and security-sensitive columns **MUST** be hidden by default, and every raw write **MUST** be audit logged. Resources **MUST** migrate to API-backed access once a safe gear API exists.

- **Rationale**: Raw DB access is the last-resort operator fallback and carries the highest risk; it requires the strictest guardrails. Deferred beyond v0.
- **Actors**: `cpt-admin-panel-actor-platform-operator`

## 6. Non-Functional Requirements

> **Global baselines**: Project-wide security, reliability, and performance NFRs are defined at repository level — see [`docs/ARCHITECTURE_MANIFEST.md`](../../ARCHITECTURE_MANIFEST.md) and the foundational [`guidelines/`](../../../guidelines). Only admin-panel-specific NFRs are documented here.

### 6.1 Gear-Specific NFRs

#### Authentication on All Admin Routes

- [ ] `p1` - **ID**: `cpt-admin-panel-nfr-auth-required`

All admin routes **MUST** require authentication, and every write action **MUST** require authorization; destructive, raw-database, and cross-tenant actions **MUST** be audit logged.

- **Threshold**: Zero admin routes reachable without an authenticated principal; 100% of write/destructive/cross-tenant/raw-DB actions produce an audit record.
- **Rationale**: Admin is a privileged surface; security must be structural, not optional.

#### No Secret Exposure by Default

- [ ] `p1` - **ID**: `cpt-admin-panel-nfr-no-secret-exposure`

The panel **MUST NOT** expose secrets, credentials, tokens, or security internals by default in any view, including raw database views.

- **Threshold**: No secret/security-sensitive column or field rendered without explicit, audited opt-in.
- **Rationale**: Defense-in-depth requires secrets to remain hidden across all admin surfaces.

#### Backend-Authoritative Isolation

- [ ] `p1` - **ID**: `cpt-admin-panel-nfr-backend-authority`

Tenant isolation **MUST** be enforced in Gears services or database policy, not in frontend logic; the frontend **MUST** treat backend authorization as the final authority.

- **Threshold**: No isolation or authorization decision implemented solely in the frontend.
- **Rationale**: UI-side isolation is bypassable; the backend must remain authoritative.

#### Demo/Static Auth Clearly Marked

- [ ] `p2` - **ID**: `cpt-admin-panel-nfr-demo-marking`

Demo/static auth and IdP plugins, and the v0 admin role stub, **MUST** be clearly marked as non-production in the UI and documentation.

- **Rationale**: Prevents accidental production use of demo credentials and stubbed roles.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### Admin Context Endpoint

- [ ] `p1` - **ID**: `cpt-admin-panel-interface-admin-context`

- **Type**: REST API
- **Stability**: unstable
- **Description**: An endpoint returning the caller's admin context — principal, tenant, admin mode, enabled gears, and capabilities — consumed by the panel at startup. Final design (host gear, path, shape) is determined in DESIGN/ADR.
- **Breaking Change Policy**: Unstable until v1; shape may change without major version bump.

#### Admin Data Provider Contract

- [ ] `p2` - **ID**: `cpt-admin-panel-interface-data-provider`

- **Type**: Frontend adapter (data provider)
- **Stability**: unstable
- **Description**: The frontend data-provider contract mapping admin list/read/create/update/delete/custom-action operations onto Gears API requests, including OData filtering/ordering, cursor pagination, and RFC-9457 error normalization.
- **Breaking Change Policy**: Unstable until the frontend stack is finalized.

### 7.2 External Integration Contracts

#### Aggregated OpenAPI Specification

- [ ] `p1` - **ID**: `cpt-admin-panel-contract-openapi`

- **Direction**: required from platform (served by API Gateway)
- **Protocol/Format**: OpenAPI 3.x JSON with `x-odata-filter`, `x-odata-orderby`, `x-rate-limit-rps` vendor extensions
- **Compatibility**: The panel consumes advertised operations, schemas, and extensions; it must tolerate resources without full CRUD.

## 8. Use Cases

#### Operator Suspends a Tenant

- [ ] `p2` - **ID**: `cpt-admin-panel-usecase-suspend-tenant`

**Actor**: `cpt-admin-panel-actor-platform-operator`

**Preconditions**:
- Operator is authenticated in platform admin mode with capability to manage the target tenant.

**Main Flow**:
1. Operator opens the tenant list and selects a tenant in `active` status.
2. Operator triggers the `suspend` action; the panel shows a confirmation dialog marking it as a state-changing action.
3. On confirmation, the panel calls the account-management suspend endpoint.
4. The backend authorizes, performs the transition, and the panel refreshes the tenant detail showing `suspended` status.
5. The action is audit logged.

**Postconditions**:
- The tenant status is `suspended`; an audit record exists.

**Alternative Flows**:
- **Unauthorized**: The backend denies; the panel shows a normalized authorization error and makes no state change.

#### Tenant Admin Edits Tenant Metadata

- [ ] `p2` - **ID**: `cpt-admin-panel-usecase-tenant-metadata`

**Actor**: `cpt-admin-panel-actor-tenant-admin`

**Preconditions**:
- Tenant admin is authenticated; admin context resolves tenant scope to their own tenant.

**Main Flow**:
1. Tenant admin opens their tenant's metadata view (scoped server-side).
2. Tenant admin edits a metadata entry and saves.
3. The panel calls the metadata upsert endpoint for the in-scope tenant.
4. The backend validates against the GTS type and persists; the panel shows the updated entry.

**Postconditions**:
- The metadata entry is updated for the tenant only; no other tenant's data is reachable.

## 9. Acceptance Criteria

- [ ] An operator can authenticate, see platform admin mode, view enabled gears, and manage tenants (list/tree/detail, create/update/suspend/unsuspend/soft-delete) end-to-end.
- [ ] A tenant admin can authenticate, see only their tenant's data, and is unable to access global tenant lists or other tenants' objects.
- [ ] The panel renders list/detail/create/edit screens generated from resource metadata and OpenAPI discovery for at least tenants, resource groups, and types.
- [ ] Custom actions (suspend/unsuspend and conversion approve/reject/cancel) work with confirmation flows.
- [ ] RFC-9457 errors are shown as user-friendly messages; resources lacking CRUD operations degrade gracefully.
- [ ] e2e tests cover the v0 flows; web-docs and README are updated; the panel runs via the example server or `make admin`.

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| API Gateway | Serves aggregated OpenAPI and proxies gear routes under the configured prefix | p1 |
| Account Management | Tenant, metadata, conversion, and user (IdP) APIs | p1 |
| AuthN / AuthZ / Tenant resolvers | Authentication, authorization decisions, server-side tenant scoping | p1 |
| Resource Group | Group hierarchy and membership APIs | p1 |
| Types Registry / GTS | Type definition and schema APIs | p1 |
| Gear Orchestrator | Enabled-gears and status summaries | p1 |
| Static auth/authz/tenant plugins | Non-production demo identities and the v0 admin role stub | p1 |
| Frontend toolchain (Refine or alternative) | SPA framework and build tooling for the admin console | p1 |

## 11. Assumptions

- The aggregated OpenAPI document and gear routes are reachable under the configured API Gateway prefix.
- Cursor pagination and `x-odata-*` extensions are advertised by list endpoints that support them.
- Tenant isolation is enforced server-side via Secure ORM tenant-subtree predicates and authorization decisions.
- v0 runs against the static (non-production) auth, authz, and tenant-resolver plugins.
- A browser SPA stack (preferring Refine) is acceptable, introduced either embedded in the example server or as a separate repository/sidecar.

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| No admin-mode/capabilities concept in the current security model | Cannot drive platform vs tenant mode or capability gating | Build an admin-context endpoint and a clearly-marked static role stub for v0 |
| Introducing a JS/TS frontend into a pure-Rust monorepo | Build/CI complexity, maintenance burden | Decide embedded vs separate repo/sidecar in an ADR; isolate the toolchain |
| Some gear APIs lack OData or full CRUD (types registry, OAGW) | Inconsistent generated screens | Support partial CRUD and limit-offset; defer rich filtering/rule editing |
| Raw DB fallback exposing secrets or enabling unsafe writes | Security incident | Operator-only, allowlist, read-only default, secret masking, mandatory audit; defer beyond v0 |
| Demo/static auth mistaken for production | Insecure deployment | Mark demo auth and role stub as non-production in UI and docs |

## 13. Open Questions

- Where does the implementation live — embedded SPA in `apps/cf-gears-example-server` or a separate `constructorfabric/` repository plus a `make admin` sidecar? (Blocks DESIGN topology.)
- Which gear owns the admin-context endpoint (account-management, API Gateway, or a new admin gear), and what is its exact shape?
- How is admin mode represented beyond the v0 static stub — authorization action, token scope claim, or computed identity field?
- Should the UI be driven purely by OpenAPI discovery, by explicit gear-contributed descriptors, or a hybrid, for v0 versus later?
- Is a global `GET /tenants` list needed for platform admin, or is starting from the root tenant via children sufficient for v0?
- Is the frontend stack confirmed as Refine, and is there a licensing constraint (must remain MIT/OSS)?
- How does the SPA authenticate against a non-`auth_disabled` deployment — reuse the authn-resolver bearer flow or a dedicated admin login?

## 14. Traceability

- **PRD** (this document): [`./PRD.md`](./PRD.md)
- **Design**: [`./DESIGN.md`](./DESIGN.md)
- **ADRs**: [`./ADR/`](./ADR/)
- **Issue**: constructorfabric/gears-rust#4144
