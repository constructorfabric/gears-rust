Updated:  2026-07-07 by Virtuozzo International GmbH

# PRD — CredStore


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
  - [5.1 P1 — Core Operations](#51-p1--core-operations)
  - [5.2 P1 — Hierarchical Sharing](#52-p1--hierarchical-sharing)
  - [5.3 P1 — Authorization](#53-p1--authorization)
  - [5.4 P1 — Reliability & Concurrency](#54-p1--reliability--concurrency)
  - [5.5 P1 — Secret Types](#55-p1--secret-types)
  - [5.6 P1 — Deprovisioning Lifecycle](#56-p1--deprovisioning-lifecycle)
  - [5.7 P2](#57-p2)
  - [5.8 P1 — Credential Records and Secrets](#58-p1--credential-records-and-secrets)
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

<!--
=============================================================================
PRODUCT REQUIREMENTS DOCUMENT (PRD)
=============================================================================
PURPOSE: Define WHAT the system must do and WHY — business requirements,
functional capabilities, and quality attributes.

SCOPE:
  ✓ Business goals and success criteria
  ✓ Actors (users, systems) that interact with this gear
  ✓ Functional requirements (WHAT, not HOW)
  ✓ Non-functional requirements (quality attributes, SLOs)
  ✓ Scope boundaries (in/out of scope)
  ✓ Assumptions, dependencies, risks

NOT IN THIS DOCUMENT (see other templates):
  ✗ Technical architecture, design decisions → DESIGN.md
  ✗ Why a specific technical approach was chosen → ADR/
  ✗ Detailed implementation flows, algorithms → features/

REQUIREMENT LANGUAGE:
  - Use "MUST" or "SHALL" for mandatory requirements (implicit default)
  - Do not use "SHOULD" or "MAY" — use priority p2/p3 instead
  - Be specific and clear; no fluff, bloat, duplication, or emoji
  - Keep transport/mechanism detail (endpoints, status codes, headers) out of
    this doc — it lives in DESIGN.md; the PRD states capabilities and outcomes.
=============================================================================
-->

## 1. Overview

### 1.1 Purpose

CredStore provides per-tenant secret storage and retrieval for the platform. It owns all secret metadata (identity, sharing, ownership, lifecycle status, version) and enforces policy; pluggable backends store only the secrets. This abstracts backend differences behind a unified API, enabling platform gears to store and access credentials without coupling to a specific storage technology.

### 1.2 Background / Problem Statement

Platform gears — most notably the Outbound API Gateway (OAGW) — need access to secrets (API keys, tokens, credentials) for making upstream API calls on behalf of tenants. These secrets must be stored securely, scoped per tenant, and accessible only to authorized consumers.

Standard credential stores provide per-tenant isolation but do not support hierarchical multi-tenant sharing. In the platform's business model, parent tenants (partners) share API credentials with child tenants (customers). For example, a partner with an OpenAI API key and quota allows their customers to make requests through OAGW using the partner's key — without the customer ever seeing the actual secret. This requires a hierarchical resolution model: when a customer requests a secret, the system walks up the tenant tree to find a shared secret from an ancestor.

Keeping secret metadata in the gear's own database (rather than in the backend) makes hierarchical resolution and authorization a single transactional query, removes any backend schema prerequisite, and allows any simple key-value store to serve as a backend plugin.

### 1.3 Goals (Business Outcomes)

- Enable OAGW to retrieve tenant credentials for upstream API calls without exposing secrets to end users
- Support hierarchical credential sharing so partners can share API access with customers
- Decouple platform gears from specific credential storage backends
- Enforce least-privilege access through the platform policy plane (PDP), with tenant isolation guaranteed at the data layer
- Make secret writes and deletes crash-safe: no partial failure may leak a readable half-written secret or permanently block a secret name

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Credential | A tenant-scoped named record that may hold a secret; the unit of storage, sharing, inheritance and authorization |
| Record | A credential's identity, sharing, type and lifecycle status, independent of whether it currently holds a secret |
| Secret | The sensitive payload held by a credential (API key, token, password) |
| Reference | A human-readable key identifying a credential within a tenant's namespace (e.g., `partner-openai-key`). **Format**: `[a-zA-Z0-9_-]+`, 1–255 characters. |
| Sharing mode | Controls credential access scope: `private` (owner only), `tenant` (all users in tenant, default), or `shared` (tenant + descendants) |
| Owner | The specific actor (identified by `subject_id` from SecurityContext) that created the record |
| Inheritance | Resolution that walks a reference up the requesting tenant's ancestor chain, returning the closest accessible credential |
| Override | A tenant's own record for a reference that would otherwise resolve to an ancestor's shared credential; the tenant's own record takes precedence |
| Suppression | A tenant's own record that deliberately makes a reference resolve as absent rather than falling back to an ancestor's shared credential |
| Secret mode | A bounded bulk read of several credentials' secrets in one request, scoped by an explicit set of references or by type, never paginated |
| Credential type | A GTS-registered classification of a credential (e.g., `api-key`, `personal-token`) carrying enforceable traits such as `allow_sharing`; `generic` by default, immutable per credential |
| Version | Monotonic per-record counter used for optimistic concurrency (lost-update detection) |
| SecurityContext | Request security context carrying the authenticated tenant ID, subject ID, and claims |
| PDP | The platform policy decision point (`authz-resolver`) that evaluates access scopes |

## 2. Actors

### 2.1 Human Actors

#### Tenant Admin

**ID**: `cpt-cf-credstore-actor-tenant-admin`

<!-- cpt-cf-id-content -->
**Role**: Authenticated user managing secrets for their tenant. Creates, updates, and deletes secrets. Configures sharing mode to control descendant access. **Needs**: CRUD operations on secrets within their own tenant namespace. Ability to share secrets with descendants or keep them private.
<!-- cpt-cf-id-content -->

#### Integration Administrator

**ID**: `cpt-cf-credstore-actor-integrations-admin`

<!-- cpt-cf-id-content -->
**Role**: Configures a tenant's integrations (SMTP, provider keys, webhooks): creates and rotates credentials, retargets and disables them, and reads the catalogue. **Needs**: Read access to the credential catalogue and to a record's metadata; ability to create and replace credential records and to rotate their secrets under precondition control. Does not need the plaintext of the credentials being managed.
<!-- cpt-cf-id-content -->

#### Catalogue Auditor

**ID**: `cpt-cf-credstore-actor-catalogue-auditor`

<!-- cpt-cf-id-content -->
**Role**: Reviews what a tenant has configured — which credentials exist, their types, whether each is the tenant's own or inherited, when each expires — for compliance, support or migration planning. Changes nothing and reads no secret. **Needs**: The credential catalogue and each record's metadata; nothing else.
<!-- cpt-cf-id-content -->

### 2.2 System Actors

#### Outbound API Gateway (OAGW)

**ID**: `cpt-cf-credstore-actor-oagw`

<!-- cpt-cf-id-content -->
**Role**: Service that proxies outbound API calls to external services. Retrieves secrets on behalf of tenants by constructing a SecurityContext for the target tenant. Primary consumer of hierarchical secret resolution.
<!-- cpt-cf-id-content -->

#### Integration Application

**ID**: `cpt-cf-credstore-actor-integration-app`

<!-- cpt-cf-id-content -->
**Role**: A platform service (mail sender, billing connector) that reads the secrets of the credentials assigned to it, one by one or as its whole set, in the tenant it acts for. Never enumerates the catalogue.
<!-- cpt-cf-id-content -->

#### Self-Rotating Application

**ID**: `cpt-cf-credstore-actor-self-rotating-app`

<!-- cpt-cf-id-content -->
**Role**: A service that both consumes and renews its own credential — refreshing an OAuth token, rotating an API key with its provider — and stores the new secret back. **Needs**: To read the secret of its credential and to rotate it under the validator that arrives with the secret; no catalogue and no other record's metadata.
<!-- cpt-cf-id-content -->

#### Provisioning Injector

**ID**: `cpt-cf-credstore-actor-provisioner`

<!-- cpt-cf-id-content -->
**Role**: A pipeline or synchronization job (CI/CD, a sync from an external vault) that places secrets into records someone else declared, and rotates them on schedule. Sees no secret it did not itself supply and no catalogue. Repairs a corrupted secret by writing a fresh version. **Needs**: To write a secret under a guarded or last-writer-wins precondition without holding any read action; optionally to create or edit records too, when it owns their definition.
<!-- cpt-cf-id-content -->

#### Platform Gear

**ID**: `cpt-cf-credstore-actor-platform-gear`

<!-- cpt-cf-id-content -->
**Role**: Any internal gear consuming secrets via the ClientHub in-process API. Reads or writes secrets using the calling tenant's SecurityContext.
<!-- cpt-cf-id-content -->

#### Value-Store Backend (Plugin)

**ID**: `cpt-cf-credstore-actor-backend`

<!-- cpt-cf-id-content -->
**Role**: Pluggable per-tenant store that persists **secrets only** (no metadata, no policy). Current implementation: `static-credstore-plugin` (in-memory, for development/testing). Production vault-backed plugins are planned. Accessed exclusively through the gear.
<!-- cpt-cf-id-content -->

#### Platform Policy & Directory Services

**ID**: `cpt-cf-credstore-actor-platform-services`

<!-- cpt-cf-id-content -->
**Role**: `authz-resolver` (PDP) evaluates per-operation access scopes; `tenant-resolver` supplies tenant ancestor chains; `types-registry` provides GTS-based plugin discovery and receives the credential-type registrations.
<!-- cpt-cf-id-content -->

## 3. Operational Concept & Environment

> **Note**: Project-wide runtime, OS, architecture, lifecycle policy, and integration patterns defined in root PRD. Document only gear-specific deviations here.

### 3.1 Gear-Specific Environment Constraints

- The gear is a **stateful** gear: it requires a database (PostgreSQL or SQLite; MySQL is rejected at migration time)
- Exactly one value-store plugin is active per deployment (selected by GTS `vendor` configuration)
- The gear depends on `authz-resolver`, `tenant-resolver`, and `types-registry`, and initializes at system priority (its consumers, e.g. OAGW, resolve the client during their own init)
- No resident background task runs inside the gear; a periodic maintenance job, run on an operator-chosen schedule outside the gear's own lifecycle, reclaims garbage and expired records, and no correctness property depends on it

## 4. Scope

### 4.1 In Scope

- Store, retrieve, and delete per-tenant secrets (ClientHub + REST)
- Sharing modes: private (owner-only), tenant (tenant-wide, default), shared (hierarchical)
- Owner-based access control for private secrets (`subject_id` from SecurityContext)
- Hierarchical secret resolution across tenant ancestry
- Secret shadowing (child overrides parent)
- Service-to-service retrieval on behalf of arbitrary tenants (OAGW pattern)
- PDP-based authorization with tenant-scope enforcement at the data layer
- Crash-safe write and delete lifecycles: immutable secret versions reconciled by a periodic maintenance job, no resident reaper
- Optimistic concurrency: per-secret version with mandatory update/delete preconditions (creation is the only preconditionless write)
- Gear + plugin architecture with runtime backend selection; in-memory static plugin for development/testing
- GTS-based credential types with enforceable traits (allowed sharing modes, secret schema validation, size/format limits, expiry)
- Operational metrics for resolution and periodic maintenance-job health; no inventory count is offered, consistent with the platform's rule against counting queries

### 4.2 Out of Scope

- Secret history or rollback (the version counter serves optimistic locking only)
- Automatic secret rotation (type-level rotation traits are advisory only)
- Cross-tenant secret transfer (secrets cannot change ownership)
- Unauthenticated or untrusted client access (all access requires platform authentication via SecurityContext)
- Full-text search over secrets or references (retrieval remains by known reference, or by the allowlisted metadata filter of a bounded bulk secret read)
- Secrets returned through the credential listing (the listing surface is metadata-only by construction, regardless of the caller's grants)
- Granular per-secret ACLs naming specific tenants (e.g., "share with tenants A, B, C only") or sharing outside the tenant hierarchy
- Hierarchical or policy logic in backend plugins (plugins are pure value stores)
- MySQL as a metadata database

## 5. Functional Requirements

### 5.1 P1 — Core Operations

#### Store Secret

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-put-secret`

<!-- cpt-cf-id-content -->
The system **MUST** allow a tenant to store a secret with a reference (key), a value, and a sharing mode, as part of writing the credential record that carries it (`cpt-cf-credstore-fr-write-credential-record`, `cpt-cf-credstore-fr-write-secret`). A create-only write fails with a conflict when a secret of the same sharing class already exists; a precondition-guarded update (see the optimistic-concurrency requirement) fails with a conflict when the target does not exist or the precondition is stale — an update never creates. Each write of a secret **MUST** create a fresh, immutable version and switch the record to it atomically, rather than overwriting a stored secret in place (`cpt-cf-credstore-fr-immutable-value-versions`). For `tenant` and `shared` modes a write updates the single non-private secret for `(tenant, reference)`; for `private` mode each owner has an independent secret under `(tenant, reference, owner)`. A private secret and a tenant/shared secret with the same reference coexist; a write of one sharing class **MUST NOT** affect the other. Changing a secret between `private` and `tenant`/`shared` is rejected as an unsupported transition.

**Rationale**: Core capability — tenants manage their own credentials; the coexistence rule makes private and team secrets independent under common names. **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### Retrieve Secret

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-get-secret`

<!-- cpt-cf-id-content -->
The system **MUST** allow a caller to retrieve the decrypted value of an accessible secret by reference. The secret is a selectable part of the credential record (`cpt-cf-credstore-fr-read-secret`), and the record's metadata is independently readable on its own (`cpt-cf-credstore-fr-get-credential`), so a caller may obtain either half without the other; the record does not name the owning tenant, and its inheritance status (`cpt-cf-credstore-fr-inheritance-status`) states whether the resolved row is the caller's own, inherited, or an override. Only rows currently holding a secret are visible to a secret read. Not-found and inaccessible are indistinguishable in the response (a single not-found surface).

**Rationale**: Consumers need the value plus enough metadata to understand inheritance and support concurrency control. **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-platform-gear`, `cpt-cf-credstore-actor-oagw`
<!-- cpt-cf-id-content -->

#### Delete Secret

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-delete-secret`

<!-- cpt-cf-id-content -->
The system **MUST** allow a tenant to delete their own secret by reference (own-tenant only; the private class targets the caller's own private secret), by deleting the credential **record** that carries it. Descendants using a shared secret lose access immediately upon deletion. Deleting a missing backend value is not an error (idempotent delete). Deletion is an immediate, single-step removal: the reference is free to reuse the instant the delete completes, with no intermediate status and no reference-retention window. A tenant that wants to disable an inherited credential without deleting anything of its own uses suppression instead (`cpt-cf-credstore-fr-suppression`).

**Rationale**: Tenants must be able to revoke credentials reliably. **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### Tenant Scoping

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-tenant-scoping`

<!-- cpt-cf-id-content -->
The system **MUST** derive the operating tenant from the request SecurityContext (`subject_tenant_id`) and the owner from `subject_id` for all operations. Tenants **MUST NOT** create, update, or delete secrets belonging to other tenants. If the caller's authorized scope does not include their own tenant, the operation is denied before any side effect and the denial is recorded (cross-tenant metric).

**Rationale**: Prevents cross-tenant data manipulation; fail-closed before side effects. **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### Secret Reference Validation

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-secretref-validation`

<!-- cpt-cf-id-content -->
The system **MUST** validate the secret reference format: `[a-zA-Z0-9_-]+`, 1–255 characters. Invalid references are rejected with a validation error, and the same constraint is enforced redundantly at the storage layer.

**Rationale**: A restricted, portable key alphabet keeps references safe for every backend key namespace and URL path segment. **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-platform-gear`
<!-- cpt-cf-id-content -->

### 5.2 P1 — Hierarchical Sharing

#### Sharing Modes

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-sharing-modes`

<!-- cpt-cf-id-content -->
Each secret **MUST** have a sharing mode: `private`, `tenant` (default), or `shared`.
- `private`: accessible only to the owner (the actor identified by `subject_id` that created the secret)
- `tenant`: accessible to all users and services within the owning tenant
- `shared`: accessible to all users in the owning tenant and all descendant tenants in the hierarchy

**Rationale**: Partners need flexible credential sharing. Personal API keys should be owner-only (`private`), team credentials tenant-wide (`tenant`), platform-level credentials for customer access hierarchical (`shared`). **Actors**: `cpt-cf-credstore-actor-tenant-admin`
<!-- cpt-cf-id-content -->

#### Hierarchical Secret Resolution

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-hierarchical-resolve`

<!-- cpt-cf-id-content -->
The system **MUST** resolve a secret reference against the requesting tenant and its ancestor chain (parent, grandparent, … root), returning the closest accessible secret; at the same tenant level the caller's private secret takes precedence over a tenant/shared one. If no accessible secret exists, the system returns not-found.

**Hierarchical direction**: resolution is **upward-only** (child → parent → root). A tenant can access ancestor secrets marked `shared`, but parents **cannot** access child secrets.

**Isolation barriers**: a `shared` secret **MUST** be inherited by all descendant tenants, including across `self_managed` (isolation-barrier) boundaries — publishing as `shared` is the owner's explicit sharing decision; read authorization remains the PDP's.

**Rationale**: Enables the core business use case — OAGW retrieves a partner's shared API key when acting for a customer — including for customers that manage their own sub-hierarchy. **Actors**: `cpt-cf-credstore-actor-oagw`
<!-- cpt-cf-id-content -->

#### Secret Shadowing

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-secret-shadowing`

<!-- cpt-cf-id-content -->
When a tenant owns a secret with the same reference as an ancestor's shared secret, and that secret is **accessible** to the requester, the tenant's own secret **MUST** take precedence during hierarchical resolution. If the tenant's same-reference secret is **inaccessible** to the requester (e.g., another owner's `private` secret), resolution **MUST** continue to ancestors.

**Rationale**: Customers can override partner defaults with their own credentials while keeping hierarchical fallback when the local secret is not theirs. **Actors**: `cpt-cf-credstore-actor-oagw`
<!-- cpt-cf-id-content -->

#### Service-to-Service Retrieval

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-service-retrieve`

<!-- cpt-cf-id-content -->
The system **MUST** support retrieval on behalf of an arbitrary tenant by an authorized service account: the service constructs a SecurityContext for the target tenant and performs the standard read operation; the PDP decides whether that subject may read in that tenant's scope. The response includes the decrypted value. There is no separate service-to-service operation.

**Rationale**: OAGW operates as a service account and needs hierarchical retrieval for arbitrary tenants through the same audited, policy-checked path. **Actors**: `cpt-cf-credstore-actor-oagw`
<!-- cpt-cf-id-content -->

### 5.3 P1 — Authorization

#### PDP-Based Authorization

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-authz-pdp`

<!-- cpt-cf-id-content -->
Every operation **MUST** be authorized through the platform PDP: the gear evaluates the actions the operation requires — `list`/`read`/`write`/`delete` on the credential record, `read_secret`/`write_secret` on the secret (`cpt-cf-credstore-fr-authz-action-split`) — against the secret's resolved concrete GTS type (including `generic`), and **MUST** enforce the returned scope on every metadata query at the data layer, enabling per-type policies (e.g., a role that reads `api-key` but not `certificate` secrets). Enforcement is fail-closed: a PDP denial denies the operation; a PDP evaluation failure surfaces as unavailable; out-of-scope or type-denied secrets are indistinguishable from non-existent ones on read.

**Rationale**: Real tenant isolation enforced at the data layer, consistent with the platform policy plane; least privilege per action. **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-oagw`, `cpt-cf-credstore-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### Gear-Level Enforcement

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-authz-gear`

<!-- cpt-cf-id-content -->
Authorization, sharing-mode enforcement, and hierarchy logic **MUST** live exclusively in the gear. Plugins are pure value stores and **MUST NOT** implement authorization or policy decisions.

**Rationale**: Prevents inconsistent authorization behavior across backends; keeps backends trivially simple. **Actors**: `cpt-cf-credstore-actor-platform-gear`, `cpt-cf-credstore-actor-backend`
<!-- cpt-cf-id-content -->

#### Authorization Action Split

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-authz-action-split`

<!-- cpt-cf-id-content -->
Authorization **MUST** distinguish six actions on the credential resource type: `list`, `read`, `write` and `delete` on the record, and `read_secret` and `write_secret` on the secret. The resource type **MUST** be the credential (`gts.cf.core.credstore.credential.v1~` and its derived types); no permission on any other type matches an operation on this surface. A write **MUST** require `write` when it changes any record field and `write_secret` when it changes the secret, both when it changes both. A read **MUST** require `read`, or `list` when reading several records at once, when it returns any record field, and `read_secret` when it returns the secret, both when it returns both — so a caller reading only the secret needs `read_secret` alone. The purpose an application reads for is expressed by the credential **type** alone: a `read_secret` grant names a concrete type or a GTS wildcard of one, and a service that needs "its own" credentials **MUST** declare its own derived type rather than filing records under a separate label — because the type is immutable once set, a metadata edit can never change who may read a secret.

**Rationale**: Enumerating entries, reading a record's metadata, and reading a secret have different blast radius and must be separately grantable; an ambiguous grant would defeat that separation. **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-integrations-admin`, `cpt-cf-credstore-actor-integration-app`
<!-- cpt-cf-id-content -->

### 5.4 P1 — Reliability & Concurrency

#### Crash-Safe Write Lifecycle

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-write-lifecycle`

<!-- cpt-cf-id-content -->
A secret write that spans metadata and backend **MUST** be crash-safe: a write only ever produces a fully readable record or none at all — there is no in-flight status and no half-written record a reader could observe. On an overwrite of an existing secret, the old secret **MUST** keep serving until the new one is fully in place; an interrupted write **MUST** cost at most an orphaned version that a periodic maintenance process reclaims, never a wrong secret, a closed read, or a permanently blocked reference (`cpt-cf-credstore-fr-immutable-value-versions`).

**Rationale**: Readers must never observe half-written secrets; writers must never permanently wedge a secret name. **Actors**: `cpt-cf-credstore-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### Optimistic Concurrency

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-optimistic-concurrency`

<!-- cpt-cf-id-content -->
Each secret **MUST** carry a monotonic version, exposed on retrieval only on the caller's own record; an inherited record carries an opaque validator that changes when the ancestor writes but cannot be used in a write precondition. Update and delete **MUST** require a caller-supplied precondition ("must exist", or "the specified generation must still be current"), enforced atomically with the metadata commit — every write states its concurrency stance, there are no unconditional overwrites; creation is the only preconditionless write. A record write that changes nothing does not advance the version; a secret write always does. A failed precondition surfaces as a conflict (lost-update detection); a malformed precondition is a validation error; a missing precondition is a validation error with its own distinct reason; all failed preconditions surface identically.

**Rationale**: Lost-update detection for concurrent secret management. **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-platform-gear`
<!-- cpt-cf-id-content -->

### 5.5 P1 — Secret Types

#### GTS-Based Secret Types

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-secret-types`

<!-- cpt-cf-id-content -->
Each secret **MUST** have a *secret type* chosen at creation (default: `generic`) and immutable thereafter. Secret types are GTS types derived from the credstore base type (`gts.cf.core.credstore.credential.v1~`, [ADR-0004](./ADR/0004-cpt-cf-credstore-adr-secret-value-exposure.md)) and registered in the types-registry; the type is also the PDP resource type. Each type **MUST** declare enforceable traits that the gear applies uniformly, at minimum: which sharing modes the type permits, rejecting a write that requests a disallowed one (e.g., `personal-token` secrets are private-only and can never be shared); optional structural validation of the secret on write; whether the type is expirable, in which case an expired secret resolves as not-found; and bounds on the secret's size and encoding.

The initial type catalog covers `generic`, `api-key`, `personal-token`, `oauth2-client`, `basic-auth`, `bearer-token`, `certificate`, `ssh-key`, `webhook-hmac`, and `connection-string` (see DESIGN §5.3). Untyped existing secrets behave as `generic` with unchanged semantics. Expired secrets of expirable types resolve as not-found and are cleaned up the same way as an ordinary delete, by the periodic maintenance job (`cpt-cf-credstore-fr-immutable-value-versions`).

**Rationale**: Different kinds of secrets have different safe-handling rules; encoding them as GTS type traits gives one enforcement point in the gear, platform-native discoverability/versioning, and per-type policy targeting (PDP) without per-secret ACLs. **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-platform-gear`
<!-- cpt-cf-id-content -->

### 5.6 P1 — Deprovisioning Lifecycle

#### Crash-Safe Delete

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-deprovisioning`

<!-- cpt-cf-id-content -->
Secret deletion **MUST** be crash-safe: deleting a credential removes the backend secret and the metadata record together, and it is free to recreate the reference the instant the delete completes, because a successor write always gets its own secret version and can never collide with a lagging backend cleanup of the old one. A failure to clean up the backend secret **MUST NOT** be visible to the caller or block the delete — the record and the reference are already gone, and cleanup completes through the periodic maintenance process (`cpt-cf-credstore-fr-immutable-value-versions`).

**Rationale**: Deletion must be reliable and observable without leaving metadata/backend divergence on partial failure, and without a name-retention window that a successor write would need to wait out. **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-platform-gear`
<!-- cpt-cf-id-content -->

### 5.7 P2

#### Production Value-Store Backend

- [ ] `p2` - **ID**: `cpt-cf-credstore-fr-production-backend`

<!-- cpt-cf-id-content -->
The system **MUST** provide at least one production-grade value-store plugin (external secret vault, KMS-backed store, or OS-protected storage for desktop/VM environments) implementing the same plugin contract as the development in-memory plugin. Backend selection remains a deployment-time configuration with no consumer-visible change.

**Rationale**: The in-memory static plugin is suitable for development and testing only (secrets do not survive process restart). **Actors**: `cpt-cf-credstore-actor-backend`
<!-- cpt-cf-id-content -->

### 5.8 P1 — Credential Records and Secrets

> Every requirement in this section is specified by [ADR-0004](./ADR/0004-cpt-cf-credstore-adr-secret-value-exposure.md) and [ADR-0005](./ADR/0005-cpt-cf-credstore-adr-upward-collection-read.md).

#### Credential Record and Secret Split

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-credential-record`

<!-- cpt-cf-id-content -->
The system **MUST** treat a credential's record and its secret as one entity, but **MUST** default a read of that credential to metadata only — reference, sharing mode, type, fallback policy, the lifecycle status of the caller's own record (none, declared, or active), expiry, inheritance status, and, for the caller's own record only, version, last-update time and the creating subject — and **MUST NOT** return the secret by default. The metadata **MUST NOT** name the owning tenant, nor the creating subject of an inherited record: for an inherited credential that would disclose an ancestor's identifier the caller cannot obtain by any authorized route, and the inheritance status already answers whether the record is the caller's own. The secret **MUST** be obtainable only when the caller explicitly asks for it and holds `read_secret`, as part of the same credential — never as a separately addressable resource.

**Rationale**: A metadata surface cannot leak a secret it structurally does not contain; separating the two makes a secret-blind administrator role possible. **Actors**: `cpt-cf-credstore-actor-integrations-admin`, `cpt-cf-credstore-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### List Credential Records

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-list-credentials`

<!-- cpt-cf-id-content -->
The system **MUST** allow an authorized caller to list the credential records effectively visible to its tenant, paginated, ordered deterministically, and **never carrying a secret regardless of the caller's grants**. The listing **MUST** follow the platform's cursor-pagination contract: a bounded page reached by an opaque cursor, filterable and orderable only on an allowlisted set of indexed fields, with no total count. The caller **MUST** be able to select a subset of fields to return, restricted to the credential's field allowlist; an unrecognized field **MUST** be rejected as a validation error; with no selection the item **MUST** be the full record. Of the filterable fields, only reference and type **MUST** be applied before hierarchical resolution, since they are invariant across a reference's chain; sharing, expiry and fallback **MUST** be filtered only after resolution, since they vary along the chain and filtering them earlier could change which record wins. Listing **MUST** require its own authorization action, distinct from reading a single record.

**Rationale**: Integration administrators need a catalogue view without turning it into a secret-disclosure or enumeration primitive. **Actors**: `cpt-cf-credstore-actor-integrations-admin`
<!-- cpt-cf-id-content -->

#### Get Credential Record

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-get-credential`

<!-- cpt-cf-id-content -->
The system **MUST** allow an authorized caller to read one credential by reference, applying hierarchical resolution, in the same representation the credential listing uses. The caller **MUST** be able to select a subset of fields to return, from the same allowlist the listing uses, plus the secret; with no selection the response **MUST** be the full record and **MUST NOT** carry the secret — the secret is returned only when explicitly asked for, and only under the `read_secret` action. The response **MUST** carry the optimistic-concurrency validator for the caller's own record regardless of what was asked for, so that a caller entitled to write but not to read secrets can still perform a guarded write. A record that does not resolve or is inaccessible **MUST** be indistinguishable in the response.

**Rationale**: A secret-blind writer still needs a concurrency validator to rotate or replace a record safely; one representation for reading a single credential and for listing them means field selection behaves identically on both. **Actors**: `cpt-cf-credstore-actor-integrations-admin`, `cpt-cf-credstore-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### Write Credential Record

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-write-credential-record`

<!-- cpt-cf-id-content -->
The system **MUST** accept, at a credential's address, a full replace that creates or replaces the record and its secret together in one request, guarded by a create-only or a replace precondition, and **MUST** separately accept a partial update at the same address that changes only the fields supplied, leaves every other field untouched, and never creates a record. On a full replace, the caller **MUST** either supply a secret or explicitly state that the record has none; omitting the secret entirely **MUST** be rejected as a validation error. Explicitly stating no secret **MUST** be accepted and **MUST** produce a record without one: creating such a record needs no secret to be written at all; replacing a record that currently holds a secret **MUST** remove it as part of the same request; replacing a record that already holds no secret **MUST** leave that state unchanged. A partial update **MUST** likewise be able to state that a record has no secret, which **MUST** remove any secret the record held while leaving the rest of the record in place. The credential's type **MUST** remain immutable under both forms.

**Rationale**: A secret-blind administrator edits metadata (sharing, expiry, fallback) through a partial update that never carries a secret; the same address creates a record — with a secret or, by explicitly stating it has none, without one — in one request, which is what lets a tenant suppress an inherited credential without ever holding a secret of its own. **Actors**: `cpt-cf-credstore-actor-integrations-admin`
<!-- cpt-cf-id-content -->

#### Read a Secret

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-read-secret`

<!-- cpt-cf-id-content -->
The system **MUST** allow an authorized caller to read the secret of a credential resolved through the tenant hierarchy, whether reading one credential or several, with caching disabled and one audit record per secret returned; there **MUST NOT** be a dedicated address for the secret alone. A caller **MUST** be able to ask for the secret alongside only what is needed to use it — the reference, the type and the expiry — and obtain it with none of the record's administrative metadata (sharing mode, inheritance status, lifecycle status); the SDK **MUST** offer a `get_secret` convenience that does exactly this. Reading the secret **MUST** require the `read_secret` action independently of whatever record fields are requested alongside it, so that reading a secret and reading a record remain distinct privileges in fact.

**Rationale**: Secret disclosure is its own privilege with its own auditable, throttleable path, separate from reading or listing metadata; folding it into the credential itself removes a second address and a second response shape for the same entity. **Actors**: `cpt-cf-credstore-actor-integration-app`, `cpt-cf-credstore-actor-oagw`, `cpt-cf-credstore-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### Write a Secret

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-write-secret`

<!-- cpt-cf-id-content -->
The system **MUST** write a credential's secret only as part of writing its record, and **MUST NOT** grant the ability to read that secret as a side effect of granting the ability to write it. The write-secret action **MUST** be required whenever a request writes or removes a secret, and the record-write action **MUST** also be required when the same request carries metadata fields. The write-secret action **MUST NOT** be required when a request states that a record has no secret and it already had none: creating a record with no secret, or replacing an already secret-less record with the same absence, needs only the record-write action, because no secret is being read or changed. A partial update that removes a secret **MUST** return the record to holding no secret while leaving the rest of its metadata intact, and the record's fallback policy then decides whether the reference inherits or resolves as absent. A partial update **MUST NOT** create a record — a reference with no record of the caller's own is a not-found. The write **MUST NOT** be permitted for a record owned by an ancestor: a tenant that wants its own secret creates its own record first. A write that changes or removes a secret **MUST** always write and always advance the version, even when a submitted secret equals the stored one, under the record's one validator.

**Rationale**: This is the requirement that makes the secret-blind configurator possible — the persona who provisions and rotates an integration's credentials without ever being able to read one. **Actors**: `cpt-cf-credstore-actor-integrations-admin`, `cpt-cf-credstore-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### Immutable Value Versions

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-immutable-value-versions`

<!-- cpt-cf-id-content -->
Every write of a secret **MUST** create a new immutable value version rather than overwriting a stored one in place, and a record **MUST NOT** ever come to reference a version that was not completed on its behalf. An interrupted write **MUST** cost at most an orphaned version that the system reclaims — never a wrong secret, a closed read, or a permanently reserved reference. A record holding no secret **MUST** be indistinguishable, in what it references, from one that never held one. Garbage and expired records **MUST** be reclaimed by a periodic maintenance process; no correctness property may depend on that process running promptly.

**Rationale**: Overwriting a stored secret in place makes every partial-write failure a race between two states of the same bytes; giving each write its own version removes that race entirely — a failure can only leave garbage beside the truth, never inside it. This is the versioning model every managed secret store uses, and it is why the in-process reaper is withdrawn: with no intermediate state to repair, what remains is periodic garbage collection rather than a resident loop. **Actors**: `cpt-cf-credstore-actor-platform-gear`, `cpt-cf-credstore-actor-integrations-admin`, `cpt-cf-credstore-actor-backend`
<!-- cpt-cf-id-content -->

#### Bulk Read Secrets (Secret Mode)

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-bulk-read-secrets`

<!-- cpt-cf-id-content -->
The system **MUST** allow an authorized caller to read the secrets of several credentials in one request through the same listing used for metadata — this is secret mode. Reading a secret **MUST** require `read_secret`; any record field requested alongside it **MUST** require `list`; both when both are requested. Secret mode **MUST** be scoped by exactly one of: an explicit set of references, or a scope over the credential type; no broader selector, and no ordering, **MUST** be offered — an unbounded scan over references is exactly the enumeration this surface exists to withhold. The set read this way **MUST NOT** be able to exceed what the caller may read one by one: each item **MUST** be authorized individually. Secret mode **MUST NOT** be paginated or continuable. The number of items it returns **MUST** be bounded by a fixed cap; exceeding it **MUST** fail the request rather than truncate the result. A refused item **MUST** be omitted entirely rather than reported by name, because the filter found it, not the caller. One audit record **MUST** be produced per secret returned.

**Rationale**: Applications commonly need their whole credential set in one round-trip; disclosure must stay bounded by the caller's own grant, a hard cap, and the absence of pagination, rather than becoming a walkable catalogue dump. Serving this through the same listing as metadata means one authorization path covers both. **Actors**: `cpt-cf-credstore-actor-integration-app`
<!-- cpt-cf-id-content -->

#### Inheritance Status

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-inheritance-status`

<!-- cpt-cf-id-content -->
Every record representation **MUST** state its relationship to the ancestor chain: owned by the requesting tenant, inherited from an ancestor, owning a record that overrides an ancestor's shared credential, or resolving to nothing because the nearest record holds no secret and is set to suppress. The status is metadata, not secret material, and **MUST** be available to callers holding only metadata actions.

**Rationale**: An integration administrator must be able to tell "mine" from "inherited" from the catalogue alone, without ever reading a secret. **Actors**: `cpt-cf-credstore-actor-integrations-admin`
<!-- cpt-cf-id-content -->

#### Tenant-Level Suppression

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-suppression`

<!-- cpt-cf-id-content -->
Each credential record **MUST** carry a fallback policy, to inherit or to suppress, governing resolution while the record holds no secret; the policy **MUST** be settable with the record-write action alone. A record set to suppress and holding no secret **MUST** make the reference resolve as absent in its tenant and, per its sharing mode, its descendants, leaving the ancestor's credential untouched. Writing a secret **MUST** make the record serve it regardless of the policy. The policy **MUST** persist while a secret is present, so that removing the secret later applies it. A tenant **MUST** be able to suppress an inherited credential in one request, whether or not it already holds a record, without supplying a secret and using only the record-write action.

**Rationale**: Descendants need a way to opt out of an inherited credential without deleting or shadowing it at the ancestor's expense; the same policy lets a tenant fail closed while it is still setting its own, and suppressing without an existing record needs no separate placeholder write. **Actors**: `cpt-cf-credstore-actor-integrations-admin`
<!-- cpt-cf-id-content -->

#### Override Type Consistency

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-override-type-consistency`

<!-- cpt-cf-id-content -->
When a tenant creates a credential record for a reference that currently resolves to an ancestor's `shared` credential, the new record **MUST** carry the same secret type as the credential it overrides; a differing type **MUST** be rejected as a conflict. Consuming applications address a credential by reference and rely on its type to know the shape of the secret, so a local override of a different type would break them without any change on their side. The rule applies only at creation: the type is immutable afterwards, and a reference that resolves to nothing may be created with any registered type.

**Rationale**: The secret type is the contract between the credential and the application that reads it; a tenant must not be able to break that contract unilaterally by shadowing a credential with an incompatible one. **Actors**: `cpt-cf-credstore-actor-integrations-admin`, `cpt-cf-credstore-actor-integration-app`
<!-- cpt-cf-id-content -->

## 6. Non-Functional Requirements

### 6.1 Gear-Specific NFRs

#### Secret Confidentiality

- [ ] `p1` - **ID**: `cpt-cf-credstore-nfr-confidentiality`

<!-- cpt-cf-id-content -->
Secrets **MUST NOT** appear in logs, error messages, or debug output at any level (gear, plugin, transport), **MUST NOT** be cacheable by intermediaries, and **MUST NOT** be silently corrupted (a non-UTF-8 secret is rejected rather than lossily decoded). Secret memory is zeroized on drop. Metadata surfaces (the credential record, its listing, and any catalogue view) **MUST NOT** be able to carry a secret; the restriction is a structural property of the resource, not a convention enforced by review. Every secret returned to a caller **MUST** be attributable to a subject and an operation in the audit trail.

**Threshold**: Zero plaintext secrets in any log output **Rationale**: Secrets are the most sensitive data in the platform. **Architecture Allocation**: See DESIGN.md §3.2 for the implementation approach
<!-- cpt-cf-id-content -->

#### Tenant Isolation

- [ ] `p1` - **ID**: `cpt-cf-credstore-nfr-tenant-isolation`

<!-- cpt-cf-id-content -->
No operation may read or modify secret metadata outside the caller's PDP-authorized tenant scope; enforcement happens at the data layer on every query. Inaccessible secrets are indistinguishable from non-existent ones (anti-enumeration). This equivalence **MUST** hold per item inside a bulk response, not only for point reads.

**Threshold**: Zero cross-tenant reads/writes outside the authorized scope **Rationale**: Multi-tenant platform guarantee. **Architecture Allocation**: PDP scope + data-layer clamps; see DESIGN.md §3.1
<!-- cpt-cf-id-content -->

#### Observability

- [ ] `p1` - **ID**: `cpt-cf-credstore-nfr-observability`

<!-- cpt-cf-id-content -->
The gear **MUST** emit operational metrics sufficient to detect resolution anomalies and lifecycle divergence: walk-up depth, read outcome (own/inherited/miss), per-dependency latency and outcome (PDP, tenant-resolver, plugin), cross-tenant denials, and periodic-maintenance-job counters (garbage collected, expired records reclaimed); no inventory count is offered, consistent with the platform's rule against counting queries. Metric labels **MUST NOT** contain secret references or secrets.

**Rationale**: Crash-safe writes/deletes, hierarchical resolution, and periodic maintenance fail in partial, quiet ways; operators need signals, not log archaeology. **Architecture Allocation**: See DESIGN.md §10 Observability
<!-- cpt-cf-id-content -->

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### CredStoreClientV1

- [ ] `p1` - **ID**: `cpt-cf-credstore-interface-client`

<!-- cpt-cf-id-content -->
**Type**: Rust trait (async) **Stability**: stable **Description**: Public API for platform gears. Registered in ClientHub without scope. Operations, as reshaped by ADR-0004: a read (the credential's metadata, plus its secret only when asked for, never the owning tenant), a secret-read convenience returning just the secret and what is needed to use it, a precondition-guarded create-or-replace of the record together with its secret in one call (the secret may be supplied, or the record explicitly left without one), a precondition-guarded partial update (metadata edit, secret rotate, or secret removal; never creates), a filtered/paginated listing of records that also serves secret mode when secrets are requested for a bounded set, and a precondition-guarded delete. The guarded create-or-replace is the only way to create a credential. Hierarchical resolution is internal to the gear. **Breaking Change Policy**: Major version bump required
<!-- cpt-cf-id-content -->

#### CredStorePluginClientV1

- [ ] `p1` - **ID**: `cpt-cf-credstore-interface-plugin-client`

<!-- cpt-cf-id-content -->
**Type**: Rust trait (async) **Stability**: unstable **Description**: Plugin SPI for backend secret stores. Registered in ClientHub with GTS instance scope. Operations: read, write and delete an immutable secret version keyed by tenant and an opaque version id — a write never overwrites an existing id. Returns the secret only — no metadata, no policy, no reference or owner. **Breaking Change Policy**: Minor version bump (unstable API)
<!-- cpt-cf-id-content -->

### 7.2 External Integration Contracts

#### REST API

- [ ] `p1` - **ID**: `cpt-cf-credstore-contract-rest-api`

<!-- cpt-cf-id-content -->
**Direction**: provided **Protocol/Format**: HTTP/REST, JSON, canonical `Problem` error envelope, under a versioned path served beneath the platform API prefix. Exposes a credential record and its optional secret as one addressable item: a filtered/paginated listing and a point read, both metadata-only by default and disclosing the secret only when explicitly selected; a full-replace write and a merge-patch partial update, each guarded by a mandatory precondition (create-only, guarded-replace, or last-writer-wins); and a delete. See DESIGN.md for the concrete endpoints, methods, status codes, and headers.

**Compatibility**: the reference is the caller-chosen name and never becomes an internal row id; the `Problem` envelope and the canonical status mapping are platform-standard; the optimistic-concurrency validator is generation-bound for an own record; secret-confidentiality controls (no intermediary caching, per-secret audit) apply to every address that discloses a secret.
<!-- cpt-cf-id-content -->

#### GTS Registration

- [ ] `p1` - **ID**: `cpt-cf-credstore-contract-gts`

<!-- cpt-cf-id-content -->
**Direction**: provided to types-registry **Protocol/Format**: GTS link-time inventory. Registered types: the plugin spec, the secret resource type used by the PDP (carrying the secret-type traits schema), and the derived secret-type family (traits mirrored as `x-gts-traits`). See DESIGN §5 for the concrete type ids. **Compatibility**: Type ids are stable identifiers; new versions are new ids
<!-- cpt-cf-id-content -->

## 8. Use Cases

#### UC-001: Partner Creates Shared Secret

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-create-shared`

<!-- cpt-cf-id-content -->
**Actor**: `cpt-cf-credstore-actor-tenant-admin`

**Preconditions**:
- Tenant is authenticated; PDP authorizes `write` on secrets in the tenant's scope

**Main Flow**:
1. Partner tenant stores `partner-openai-key` with a secret and sharing `shared`
2. Gear evaluates the PDP write scope and the own-tenant gate
3. The write completes only once the secret is durably stored as a fresh, immutable secret version
4. Secret is immediately resolvable by the partner and all descendant tenants

**Postconditions**:
- Secret is stored and accessible to partner and descendants

**Alternative Flows**:
- **Secret already exists (same class)**: secret and sharing updated, version bumped
- **Create-only write**: fails with a conflict if the reference is taken in that sharing class
- **Backend write fails**: leaves nothing readable and never permanently blocks the reference — no record is created at all, only an orphaned secret version the periodic maintenance job reclaims
<!-- cpt-cf-id-content -->

#### UC-002: OAGW Retrieves Secret for Customer (Hierarchical Resolution)

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-hierarchical-resolve`

<!-- cpt-cf-id-content -->
**Actor**: `cpt-cf-credstore-actor-oagw`

**Preconditions**:
- OAGW holds a service identity authorized to read secrets in the customer's scope
- Partner has a `shared` secret `partner-openai-key`; customer is a descendant of partner

**Main Flow**:
1. OAGW constructs a SecurityContext for the customer tenant and reads the reference `partner-openai-key`
2. Gear evaluates the PDP read scope for that context
3. Gear obtains the customer's full ancestor chain (cached)
4. Gear resolves the reference against the whole ancestor chain → the partner's `shared` secret wins (the customer holds none)
5. Gear reads the secret only for the winning credential
6. OAGW receives the secret plus the credential record (inheritance status = inherited, version; the owning tenant is not named — ADR-0004)

**Postconditions**:
- OAGW has the decrypted secret; the customer never sees it
- Resolution depth and inherited-read outcome are recorded as metrics

**Alternative Flows**:
- **Customer has own accessible secret**: it wins (shadowing); the parent's credential is not considered
- **No accessible secret in the chain**: not-found
<!-- cpt-cf-id-content -->

#### UC-003: Customer Overrides Parent Secret (Shadowing)

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-shadowing`

<!-- cpt-cf-id-content -->
**Actor**: `cpt-cf-credstore-actor-tenant-admin`

**Preconditions**:
- Partner has shared secret `partner-openai-key`; customer is a descendant

**Main Flow**:
1. Customer creates own secret with the same reference (sharing `tenant`)
2. OAGW resolves `partner-openai-key` for the customer
3. The customer's record is closer in the chain → the customer's secret is returned
4. Partner's secret remains available to other descendants

**Postconditions**:
- Customer uses its own key; partner's shared secret is unaffected

**Alternative Flows**:
- **Customer uses `private` mode**: the override applies only to the creating owner; other subjects in the customer tenant still resolve the partner's shared secret
<!-- cpt-cf-id-content -->

#### UC-004: Private Secret Access & Fallback

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-private-denied`

<!-- cpt-cf-id-content -->
**Actor**: `cpt-cf-credstore-actor-oagw`

**Scenario A: Parent's private secret (no leak)**

**Preconditions**:
- Partner has `internal-admin-key` with sharing `private` (owned by PartnerAdmin); customer has no secret with this reference

**Main Flow**:
1. OAGW resolves `internal-admin-key` for the customer
2. Resolution only matches a private secret owned by the requesting subject; PartnerAdmin's secret is invisible to OAGW
3. Nothing matches → not-found

**Postconditions**:
- A parent's private secret is never disclosed to descendants or other subjects

**Scenario B: Another user's private secret with fallback to parent's shared**

**Preconditions**:
- Customer has `api-key` (sharing `private`, owner User A); partner has `api-key` (sharing `shared`); User B in the customer tenant requests `api-key`

**Main Flow**:
1. User B requests `api-key`
2. User A's private secret is invisible to User B; the customer tenant holds no tenant or shared secret of its own
3. The partner's `shared` secret is the closest accessible match → returned

**Postconditions**:
- User B falls back to the partner's shared secret; User A's private secret stays invisible

**Rationale**: Private secrets are per-owner; inaccessible private secrets never block fallback to ancestor shared secrets.
<!-- cpt-cf-id-content -->

#### UC-005: Tenant CRUD Own Secrets (with Concurrency Control)

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-crud`

<!-- cpt-cf-id-content -->
**Actor**: `cpt-cf-credstore-actor-tenant-admin`

**Preconditions**:
- Tenant is authenticated with PDP-authorized read/write/delete scope

**Main Flow**:
1. Create a secret by reference (create-only)
2. Read the secret → its payload, metadata, and current version
3. Guarded update with a version precondition (or an explicit must-exist overwrite) → success, or conflict on a stale version
4. Guarded delete with a version precondition (or an explicit must-exist form) → success

**Postconditions**:
- Secret lifecycle managed; descendants of shared secrets lose access on delete

**Alternative Flows**:
- **Read or delete a non-existent secret**: not-found
- **Read another owner's private secret**: not-found (anti-enumeration)
- **Stale version precondition**: conflict, no changes applied
- **Missing precondition on update/delete**: validation error (distinct reason), no changes applied
- **Malformed version precondition**: validation error
<!-- cpt-cf-id-content -->

#### UC-006: Owner-Only Private Secret Access Control

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-private-owner-only`

<!-- cpt-cf-id-content -->
**Actor**: `cpt-cf-credstore-actor-tenant-admin`

**Preconditions**:
- User A and User B are authenticated users in the same tenant with write access

**Main Flow**:
1. User A stores `my-personal-api-key` with sharing `private`, creating an independent secret scoped to that owner
2. User B stores the same reference with sharing `private`, creating a second independent secret scoped to their own ownership; no conflict with User A's
3. Each user's read resolves their own private secret

**Postconditions**:
- Independent per-owner private secrets under one reference; no cross-owner visibility

**Alternative Flows**:
- **User C (no private secret) reads the reference**: falls back to the tenant/shared secret or not-found
- **User B attempts to delete User A's private secret**: a delete only ever targets the caller's own private secret → User A's secret is untouched (User B gets not-found if they hold none)
<!-- cpt-cf-id-content -->

#### UC-007: Type-Restricted Sharing

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-type-restricted-sharing`

<!-- cpt-cf-id-content -->
**Actor**: `cpt-cf-credstore-actor-tenant-admin`

**Preconditions**:
- The `personal-token` secret type is registered as private-only

**Main Flow**:
1. User stores a secret with type `personal-token` and sharing `private` → accepted
2. User (or a later update) attempts sharing `tenant` or `shared` for the same type → rejected as a violation of the type's sharing restriction
3. Retrieval reports the type as `personal-token` in metadata

**Postconditions**:
- Personal tokens can never be widened beyond their owner, regardless of caller permissions

**Alternative Flows**:
- **Type omitted**: defaults to `generic` (all sharing modes allowed — current behavior)
- **Attempt to change the type of an existing secret**: rejected as unsupported transition
<!-- cpt-cf-id-content -->

#### UC-008: Reliable Revocation via Deprovisioning

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-deprovisioning`

<!-- cpt-cf-id-content -->
**Actor**: `cpt-cf-credstore-actor-tenant-admin`

**Preconditions**:
- Tenant owns an `active` secret consumed by descendants

**Main Flow**:
1. Tenant deletes the secret by reference
2. The record and reference are removed immediately, with no intermediate status and no name-retention window, and the secret's version is queued for garbage collection
3. The gear best-effort deletes the backend secret; a failure leaves it queued for the periodic maintenance job, never blocking or re-exposing the reference

**Postconditions**:
- Secret fully revoked; the reference is immediately reusable, and a reuse before backend cleanup finishes cannot collide with or be clobbered by that cleanup — the new secret gets its own version identifier

**Alternative Flows**:
- **Backend delete fails**: invisible to the caller — the record and the reference are already gone; the periodic maintenance job reconciles the orphaned backend secret
- **Retry the delete**: idempotent — the record no longer exists, so a retried delete is an ordinary not-found, not a resumed lifecycle
- **Crash mid-delete**: the delete either completed in full (record gone, cleanup queued) or not at all (record and reference untouched); there is no partial state to recover
<!-- cpt-cf-id-content -->

#### UC-009: Integration Administrator Configures a Tenant's SMTP Without Seeing Credentials

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-admin-configure-without-value`

<!-- cpt-cf-id-content -->
**Actor**: `cpt-cf-credstore-actor-integrations-admin`

**Preconditions**:
- Administrator holds the `list`, `read` and `write` actions in their tenant, but not `read_secret`
- A partner ancestor publishes a `shared` SMTP credential

**Main Flow**:
1. Administrator lists the tenant's credential catalogue and sees the SMTP entry marked as inherited from the partner
2. Administrator creates their own record with its secret in one create-only request (never reading the secret)
3. Administrator reads the record for its validator, then rotates the secret with a guarded partial update carrying only the secret
4. At no point does the administrator ask for the secret, whether reading one credential or listing them

**Postconditions**:
- The tenant has its own SMTP credential, rotated under concurrency control, without the administrator ever seeing a plaintext secret

**Alternative Flows**:
- **Administrator attempts to read the secret**: refused; the response is identical to requesting a reference that does not exist
- **Administrator suppresses the partner's SMTP for this tenant, having already created an own record with a secret**: suppresses it and removes the secret in one partial update; secret reads in the tenant and, with `shared`, its descendants then resolve as absent while the partner's credential is untouched
- **Administrator suppresses without ever having created an own record**: a single create-only write, without supplying a secret, creates the suppressing record directly, needing only the record-write action
<!-- cpt-cf-id-content -->

#### UC-010: Mail Service Fetches Its Whole Credential Set in One Call

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-bulk-fetch-own-set`

<!-- cpt-cf-id-content -->
**Actor**: `cpt-cf-credstore-actor-integration-app`

**Preconditions**:
- The application is authorized to read secrets of type `gts.cf.core.credstore.credential.v1~cf.smtp_sender.creds.smtp.v1~` only, in the tenant it acts for

**Main Flow**:
1. Application issues one secret-mode read scoped to its credential type, asking for the reference, type, expiry and secret
2. The system resolves and authorizes the matching type with a single `read_secret` decision, then returns every matching credential in one response, each item carrying the secret with its type and expiry and nothing administrative
3. Credentials of other types are not evaluated and do not appear in the result

**Postconditions**:
- The application has its full SMTP credential set from a single round-trip, scoped exactly to its own grant

**Alternative Flows**:
- **Selection would match more items than the cap allows**: the request fails outright; the result is never truncated
<!-- cpt-cf-id-content -->

#### UC-011: Tenant Overrides, Suppresses, and Returns to an Inherited Credential

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-override-suppress-return`

<!-- cpt-cf-id-content -->
**Actor**: `cpt-cf-credstore-actor-integrations-admin`

**Preconditions**:
- T1 (a partner tenant) publishes `smtp-default` as a `shared` credential with secret V1
- T2 is T1's child tenant; T3 is T2's child tenant; neither holds a record for `smtp-default` at the start
- The administrator, acting in T2, holds `write`, `write_secret` and `delete` on the reference

**Main Flow**:
1. In T2, the administrator creates a record for `smtp-default` with secret V2 in one create-only request; T2's record becomes its own, active credential, and T2 and T3 now resolve V2, overriding T1's V1
2. In T2, the administrator rotates the secret to V3 with a guarded partial update; T2 and T3 resolve V3
3. In T2, the administrator sets the record to suppress and removes the secret in one partial update; the reference now resolves to nothing in T2 and, because the record is `shared`, in T3 as well; T1's record and secret, and T1's other descendants, are unaffected
4. In T2, the administrator deletes the record entirely; T2 and T3 fall back to resolving T1's secret V1 again

**Postconditions**:
- The tenant hierarchy passes through override, suppression and a return to inheritance without ever exposing an intermediate state in which the wrong tenant's secret is served, and without the partner's credential being touched at any step

**Alternative Flows**:
- **Soft return instead of step 4**: the administrator sets the record's fallback back to `inherit` without deleting the record; T2 and T3 resolve T1's secret V1 again, and T2 keeps its own record (still holding no secret, still reserving the reference) for a future override
- **A tenant without an own record that wants to block T1's credential**: a single create-only write, without supplying a secret, creates the suppressing record directly — no placeholder secret, no second request
<!-- cpt-cf-id-content -->

## 9. Acceptance Criteria

- [ ] Tenant can store, retrieve, and delete secrets via both ClientHub and REST API
- [ ] Create-only writes conflict on a same-class duplicate; updates require a precondition and never create
- [ ] Private secrets are accessible only to the owner; multiple owners can hold private secrets under one reference; a private and a tenant/shared secret coexist under one reference
- [ ] Tenant secrets are accessible to all subjects within the owning tenant and never inherited; shared secrets are inherited by all descendants
- [ ] Shadowing: the closest accessible secret wins; inaccessible private secrets do not block fallback
- [ ] OAGW can retrieve secrets on behalf of any tenant it is authorized for, through the standard API
- [ ] Every operation is PDP-authorized and scope-clamped at the data layer; inaccessible reads are not-found; operation-level denial is refused; a PDP outage fails closed
- [ ] Half-written secrets are never readable: no in-flight record can exist, because a write only ever produces a fully readable record or none at all; **no background timer runs inside the gear** — maintenance runs as a periodic job invoked on an operator-chosen schedule, outside the gear's own lifecycle
- [ ] `p1` **Immutable value versions**: every write of a secret creates a new version rather than overwriting one in place, and a record only ever comes to reference a version once it is complete; an interrupted write leaves the old secret still served and the new, unreferenced version collected by the periodic maintenance job — never a wrong secret, a closed read, or a reserved reference; no record can be left in an in-flight status
- [ ] Retrieval exposes the current version and secret-confidentiality controls; update/delete require a precondition, with a conflict on stale versions and a distinct validation error when it is missing
- [ ] Secrets never appear in log output or metric labels; a non-UTF-8 secret is rejected, not corrupted
- [ ] Secret types: a write violating the type's sharing, schema, size/format, or expiry rules is rejected with a stable reason; the type is immutable, defaults to `generic`, and is returned in metadata; expired secrets resolve as not-found and are removed by the periodic maintenance job, same as an ordinary delete
- [ ] Deletion is immediate and atomic: the secret stops resolving and the reference is free to reuse the instant the delete completes, with no intermediate status, followed by best-effort backend cleanup the periodic maintenance job guarantees; there is no conflict window, because a successor write never shares the deleted secret's version
- [ ] A credential's record and its optional secret share one representation; the default read omits the secret, which appears only when explicitly asked for, gated by `read_secret`, whether reading one credential or listing them
- [ ] Listing returns credential records only, paginated per the platform cursor contract, filterable and orderable only on allowlisted indexed fields, with no total count, and requires its own authorization action
- [ ] A single credential can be read by reference, returning the same representation the listing uses; by default the item never carries the secret; explicitly asking for the secret discloses it under `read_secret`; the response always carries the current version validator for the caller's own record; an inaccessible or non-resolving record is indistinguishable in the response
- [ ] A credential can be created or fully replaced in one request — record and secret together — under a create-only, guarded-replace, or last-writer-wins precondition; the secret type never changes; omitting the secret from the request is a validation error; supplying one writes it; explicitly stating the record has none is accepted and creates or leaves the record without a secret, with no backend call on create, removing an existing secret in the same request when replacing a record that holds one, and leaving an already secret-less record unchanged on replace; a partial update applies only the fields supplied, leaves the rest untouched, never creates, and can likewise state that a record has no secret to remove one without deleting the record; a record without a secret neither resolves for secret reads nor hides an inherited secret
- [ ] A credential's secret can be read, hierarchically resolved, whether reading one credential or several, with caching disabled and one audit record per secret returned; there is no separate address for the secret alone
- [ ] Several credentials' secrets can be read in one request (secret mode), scoped by an explicit set of references or by credential type; each item is authorized individually, the response is never paginated or continuable, and exceeding the cap fails the request rather than truncating it; a refused item is omitted rather than reported
- [ ] Authorization distinguishes six actions — list records, read one record, write a record, read a secret, write a secret, delete — and no policy grants secret access as a side effect of a metadata grant
- [ ] Every credential record states whether it is owned, inherited, or an override of an ancestor's shared credential, and this status is readable with metadata-only access
- [ ] `p2` A tenant can suppress an inherited credential so it resolves as absent locally and for its descendants, without altering the ancestor's credential
- [ ] A secret-less record set to suppress makes its reference resolve as absent for its tenant and, when `shared`, its descendants; the ancestor's credential is untouched; a single partial update can apply the suppression and remove the secret together
- [ ] A partial update that removes a secret leaves a secret-less record whose fallback policy decides resolution; a partial update that carries only metadata, by a caller holding only the record-write action, succeeds, and one that also carries a secret, by that same caller, is refused

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| `authz-resolver` | PDP: per-operation access-scope evaluation (fail-closed) | `p1` |
| `tenant-resolver` | Tenant ancestor chains for hierarchical resolution | `p1` |
| `types-registry` | GTS plugin discovery; secret resource type + secret-type registrations | `p1` |
| Database (PostgreSQL / SQLite) | Gear-owned secret metadata | `p1` |
| Value-store plugin | Per-tenant secret persistence (`static-credstore-plugin` for dev/test; production vault plugin planned) | `p1` |
| OAGW | Primary consumer of hierarchical secret retrieval (uses the SDK client) | `p1` |
| Database indexes on secret type and reference | The bulk filtered secret read depends on indexes over the allowlisted metadata fields; without them the read is unindexed and slow | `p1` |

## 11. Assumptions

- The gear owns all secret metadata; backends store secrets only and provide per-tenant CRUD without hierarchical or policy logic
- Exactly one value-store plugin is active per deployment (GTS vendor match)
- Tenant hierarchy is managed externally and served by `tenant-resolver`; short-TTL caching of ancestor chains is acceptable
- The PDP is the sole authorization authority; there is no local policy cache (policy freshness over availability)
- Consumers provisioning infrastructure from secrets at startup (e.g., mini-chat → OAGW upstreams) tolerate missing secrets by degrading per-provider rather than failing boot
- OAGW is a ToolKit gear that uses the standard CredStore SDK client (all access flows through Gear → Plugin)

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Secrets leaked through logs/caches | Critical security incident | NFR enforcement (redaction, zeroize, non-cacheable responses), code review |
| Garbage until the next maintenance-job run | A secret version outlives the switch or delete that superseded it, until the periodic maintenance job's next run | No resident reaper — a scheduled job (daily by default, weekly acceptable) drains the backlog each run in bounded batches; the version is never referenced by any record, so it is a storage cost only, never a correctness risk; a tighter destruction requirement is met by running the job more often |
| Pending write never completes | A write's secret version was stored but the record was never switched to it (crash, or a losing concurrent write whose own cleanup failed) | The maintenance job reclaims an unreferenced version once it exceeds an age threshold; a defensive check refuses to delete a version any live record still references |
| Expired record lingers in the catalogue and holds its reference until the job | An expired credential stops resolving at read time but its record is not removed until the periodic maintenance job's next run or an owner action | A create-only write under that reference conflicts until then; the owner can update or delete the record directly to reclaim the reference immediately rather than waiting for the job |
| PDP or tenant-resolver outage | Operations fail closed (unavailable) | Ancestor-chain cache absorbs blips; dependency metrics for fast diagnosis |
| Ancestor-chain cache staleness | A re-parented tenant keeps inheriting its former parent's `shared` credentials for up to the cache TTL | Short TTL + LRU. The own-tenant gate is **not** a control here: it validates the caller's tenant, not the ancestors the cached chain names, which is precisely what lets an inherited read work. Closing the window needs a hierarchy version or change signal from the tenant resolver, which is work outside this gear; until then the TTL is the settling time for credential inheritance after a move |
| In-memory static plugin in non-dev use | Secrets lost on restart | Production vault plugin (`cpt-cf-credstore-fr-production-backend`); deployment policy |
| Type-trait misconfiguration | Overly permissive or broken writes for a type | Compiled-in catalog pinned to registered GTS schemas by unit tests; catalog changes are code-reviewed SDK releases; `generic` keeps legacy behavior |

## 13. Open Questions

- ~~**Batch retrieval**: should a read support multiple references per call?~~ **Answered** by [ADR-0004](./ADR/0004-cpt-cf-credstore-adr-secret-value-exposure.md): the credential listing gains a secret mode, scoped by an explicit reference list or by a type filter, non-paginated, authorized per item and capped by cardinality (`cpt-cf-credstore-fr-bulk-read-secrets`).
- ~~**P2/Future — Human vs service access**: should human users be restricted to metadata-only?~~ **Answered** by [ADR-0004](./ADR/0004-cpt-cf-credstore-adr-secret-value-exposure.md) and `cpt-cf-credstore-fr-authz-action-split`: the restriction is expressed by granting metadata actions without the read-secret action, and applies to any principal kind rather than being derived from whether the subject is human.
- **P2/Future — Audit trails**: structured audit events (actor, tenant, outcome — never secrets) to a tamper-evident platform sink.
- ~~**P2/Future — Metadata list endpoint**~~ **Answered** by [ADR-0004](./ADR/0004-cpt-cf-credstore-adr-secret-value-exposure.md), which fixes that a listing exists (`cpt-cf-credstore-fr-list-credentials`) and never carries a secret unless the caller explicitly asks for one — in which case the same listing also serves the bulk secret read (`cpt-cf-credstore-fr-bulk-read-secrets`), under its own cap and without pagination — and by [ADR-0005](./ADR/0005-cpt-cf-credstore-adr-upward-collection-read.md), which is the dedicated ADR this entry asked for: the listing is rooted at the caller's tenant and resolves upward through the hierarchy, checking the requesting tenant rather than filtering records individually, which is what lets an inherited entry appear at all.
- ~~**One item per reference**~~ **Answered** by [ADR-0005](./ADR/0005-cpt-cf-credstore-adr-upward-collection-read.md) "Pagination over a reduced result": because the canonical order leads with reference, every record for one reference is contiguous, so a cursor always sits on a reference boundary and no reference can be split across a page. **Still open**: this is the platform's first pagination that reduces records before paging, so its page-boundary behaviour needs its own dedicated test suite.

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/) — [ADR-0001 stateful gear](./ADR/0001-cpt-cf-credstore-adr-stateful-gear.md), [ADR-0002 deprovisioning saga](./ADR/0002-cpt-cf-credstore-adr-deprovisioning-saga.md) (superseded by ADR-0006), [ADR-0003 value-fingerprint fence](./ADR/0003-cpt-cf-credstore-adr-value-fingerprint-fence.md) (amended by ADR-0006), [ADR-0004 credential: metadata with a selectable secret](./ADR/0004-cpt-cf-credstore-adr-secret-value-exposure.md), [ADR-0005 upward-rooted collection read](./ADR/0005-cpt-cf-credstore-adr-upward-collection-read.md), [ADR-0006 immutable value versions](./ADR/0006-cpt-cf-credstore-adr-immutable-value-versions.md), [ADR-0007 two write verbs on one address: PUT replaces, PATCH merges](./ADR/0007-cpt-cf-credstore-adr-record-write-verbs.md), [ADR-0008 suppression: fallback on the tenant's own record](./ADR/0008-cpt-cf-credstore-adr-suppression-fallback.md), [ADR-0009 an inherited entry discloses nothing about the ancestor](./ADR/0009-cpt-cf-credstore-adr-no-ancestor-disclosure.md), [ADR-0010 six actions on the credential type; the type is the only scope axis](./ADR/0010-cpt-cf-credstore-adr-type-scoped-authorization.md)
- **Features**: features/ (planned)
