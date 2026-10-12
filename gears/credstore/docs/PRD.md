Updated:  2026-10-04 by Constructor Tech

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
  - [5.7 P2 — Planned](#57-p2--planned)
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
  - Requirements marked **Planned** are specified but not yet implemented;
    everything else is implemented.
  - Be specific and clear; no fluff, bloat, duplication, or emoji
  - Keep transport/mechanism detail (endpoints, status codes, headers) out of
    this doc — it lives in DESIGN.md; the PRD states capabilities and outcomes.
=============================================================================
-->

## 1. Overview

### 1.1 Purpose

CredStore provides per-tenant secret storage and retrieval for the platform. It owns all secret metadata (identity, sharing, ownership, lifecycle status, version) and enforces policy; pluggable backends store only immutable secret value versions. This abstracts backend differences behind a unified API, enabling platform gears to store and access credentials without coupling to a specific storage technology.

### 1.2 Background / Problem Statement

Platform gears — most notably the Outbound API Gateway (OAGW) — need access to secrets (API keys, tokens, credentials) for making upstream API calls on behalf of tenants. These secrets must be stored securely, scoped per tenant, and accessible only to authorized consumers.

Standard credential stores provide per-tenant isolation but do not support hierarchical multi-tenant sharing. In the platform's business model, parent tenants (partners) share API credentials with child tenants (customers). For example, a partner with an OpenAI API key and quota allows their customers to make requests through OAGW using the partner's key — without the customer ever seeing the actual secret. This requires a hierarchical resolution model: when a customer requests a secret, the system walks up the tenant tree to find a shared secret from an ancestor.

Keeping secret metadata in the gear's own database (rather than in the backend) makes hierarchical resolution and authorization a single transactional query, removes any backend schema prerequisite, and allows any versioned key-value store whose provider returns a version for each stored value to serve as a backend plugin. The gear's database is the system of record for metadata (and is backed up together with the store); the metadata index is not rebuildable from the store.

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
| Credential type | A GTS-registered classification of a credential (e.g., `api-key`, `personal-token`) carrying enforceable traits such as `allow_sharing`; named explicitly at creation (no default), immutable per credential |
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
**Role**: A pipeline or synchronization job (CI/CD, a sync from an external vault) that places secrets into records someone else declared, and rotates them on schedule. Sees no secret it did not itself supply and no catalogue. Under [ADR-0006](./ADR/0006-cpt-cf-credstore-adr-immutable-value-versions.md) there is no corrupted state to repair: a re-supply is an ordinary new write that mints a fresh version. **Needs**: To write a secret under a guarded or last-writer-wins precondition without holding any read action; optionally to create or edit records too, when it owns their definition.
<!-- cpt-cf-id-content -->

#### Platform Gear

**ID**: `cpt-cf-credstore-actor-platform-gear`

<!-- cpt-cf-id-content -->
**Role**: Any internal gear consuming secrets via the ClientHub in-process API. Reads or writes secrets using the calling tenant's SecurityContext.
<!-- cpt-cf-id-content -->

#### Value-Store Backend (Plugin)

**ID**: `cpt-cf-credstore-actor-backend`

<!-- cpt-cf-id-content -->
**Role**: Pluggable per-tenant versioned store that persists **secret value versions only** (no metadata, no policy), keyed by tenant and record. It provides provider-assigned value versions (ordered where destroy is supported), durable writes, exact-bytes reads and idempotent key deletion, with an optional idempotent destroy of superseded versions (`cpt-cf-credstore-interface-plugin-client`). Current implementations: `static-credstore-plugin` (in-memory, for development/testing) and `vault-credstore-plugin` (HashiCorp Vault / OpenBao KV v2, the production-grade reference implementation). Other backends are future plugins. Accessed exclusively through the gear.
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
- No background task runs inside the gear: there is no reaper and no maintenance job ([ADR-0006](./ADR/0006-cpt-cf-credstore-adr-immutable-value-versions.md)). The process runs no background work at all: no background workers or tasks, no timers, no database polling, no deferred tasks and no work at startup. Store cleanup (removal of superseded, removed and orphaned secret versions and the purge of a deleted record's store key) is recorded as a debt in the gear's database in the transaction that makes the content dead, executed by the very request that caused it after a confirmed commit, and, when left over for a live record or a failed create, healed by a later request that touches it; a failed purge of a deleted record's key stays recorded until a possible external job; no correctness property depends on its promptness

## 4. Scope

### 4.1 In Scope

- Store, retrieve, and delete per-tenant secrets (ClientHub + REST)
- Sharing modes: private (owner-only), tenant (tenant-wide, default), shared (hierarchical)
- Owner-based access control for private secrets (`subject_id` from SecurityContext)
- Hierarchical secret resolution across tenant ancestry
- Secret shadowing (child overrides parent)
- Service-to-service retrieval on behalf of arbitrary tenants (OAGW pattern)
- PDP-based authorization with tenant-scope enforcement at the data layer
- Crash-safe write and delete lifecycles — immutable secret versions announced by a write intent before they are stored, a pointer switched in one database transaction that also records the destruction of superseded versions as a cleanup debt where the backend supports it, and a recorded purge debt on delete, executed by the request itself and, for a live record, healed on the next access to it ([ADR-0006](./ADR/0006-cpt-cf-credstore-adr-immutable-value-versions.md)); no reaper, no maintenance job
- Optimistic concurrency: per-secret version with mandatory update/delete preconditions (creation is the only preconditionless write)
- Gear + plugin architecture with runtime backend selection; in-memory static plugin for development/testing
- GTS-based credential types with enforceable traits (allowed sharing modes, secret schema validation, size/format limits, expiry)
- Operational metrics for resolution and lifecycle health — dependency health plus counters for recorded and failed store cleanup, healed write intents, verified ambiguous commits and read retries; no equivalent inventory count is offered, consistent with the platform's rule against counting queries

### 4.2 Out of Scope

- Secret history or rollback (the version counter serves optimistic locking only)
- Automatic secret rotation (type-level rotation traits are advisory only)
- Cross-tenant secret transfer (secrets cannot change ownership)
- Unauthenticated or untrusted client access (all access requires platform authentication via SecurityContext)
- Full-text search over secrets or references (retrieval remains by known reference, or by the allowlisted metadata filter of the collection read)
- Secrets in the plain credential listing (the listing is metadata-only; a secret is returned only when the caller explicitly selects `secret` on the collection, under `read_secret`)
- Granular per-secret ACLs naming specific tenants (e.g., "share with tenants A, B, C only") or sharing outside the tenant hierarchy
- Hierarchical or policy logic in backend plugins (plugins are versioned value stores)
- MySQL as a metadata database

## 5. Functional Requirements

### 5.1 P1 — Core Operations

#### Store Secret

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-put-secret`

<!-- cpt-cf-id-content -->
The system **MUST** allow a tenant to store a secret with a reference (key), a value, and a sharing mode. Two write operations exist: a create-only operation that fails with a conflict when a secret of the same sharing class already exists, and a precondition-guarded update of an existing secret (see the optimistic-concurrency requirement) that fails with a conflict when the target does not exist — an update never creates. For `tenant` and `shared` modes a write updates the single non-private secret for `(tenant, reference)`; for `private` mode each owner has an independent secret under `(tenant, reference, owner)`. A private secret and a tenant/shared secret with the same reference coexist; a write of one sharing class **MUST NOT** affect the other. Changing a secret between `private` and `tenant`/`shared` is rejected as an unsupported transition.

**Superseded in part** by [ADR-0004](./ADR/0004-cpt-cf-credstore-adr-secret-value-exposure.md): the capability holds, but a credential's record and secret are now written together as one addressable item, with a full replace and a guarded partial update covering what a create and an update covered before (`cpt-cf-credstore-fr-write-credential-record`, `cpt-cf-credstore-fr-write-secret`).

**Superseded** by [ADR-0006](./ADR/0006-cpt-cf-credstore-adr-immutable-value-versions.md) (`cpt-cf-credstore-fr-immutable-value-versions`): a write MUST create a fresh, immutable secret version and switch the record to it atomically, rather than overwriting a stored secret in place.

**Rationale**: Core capability — tenants manage their own credentials; the coexistence rule makes private and team secrets independent under common names. **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### Retrieve Secret

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-get-secret`

<!-- cpt-cf-id-content -->
The system **MUST** allow a caller to retrieve the decrypted value of an accessible secret by reference, together with access metadata: owning tenant, sharing mode, whether the secret was inherited from an ancestor, and its version. Only fully provisioned (`active`) secrets are served; the secret of an expired record is refused with `SECRET_EXPIRED` to a caller authorized to read it. Not-found and inaccessible are indistinguishable in the response (a single not-found surface).

**Superseded in part** by [ADR-0004](./ADR/0004-cpt-cf-credstore-adr-secret-value-exposure.md): the secret becomes a selectable part of the same record (`cpt-cf-credstore-fr-read-secret`), and the metadata becomes independently readable on its own (`cpt-cf-credstore-fr-get-credential`), so a caller may obtain either half without the other; the metadata no longer names the owning tenant, and a richer inheritance status (`cpt-cf-credstore-fr-inheritance-status`) replaces the inherited flag.

**Rationale**: Consumers need the value plus enough metadata to understand inheritance and support concurrency control. **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-platform-gear`, `cpt-cf-credstore-actor-oagw`
<!-- cpt-cf-id-content -->

#### Delete Secret

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-delete-secret`

<!-- cpt-cf-id-content -->
The system **MUST** allow a tenant to delete their own secret by reference (own-tenant only; the private class targets the caller's own private secret). Descendants using a shared secret lose access immediately upon deletion. Deleting a missing backend value is not an error (idempotent delete).

**Superseded in part** by [ADR-0004](./ADR/0004-cpt-cf-credstore-adr-secret-value-exposure.md): revocation semantics are unchanged, but deletion addresses the credential **record** (removing its secret with it), and a tenant that wants to disable an inherited credential without deleting anything of its own uses suppression instead (`cpt-cf-credstore-fr-suppression`).

**Superseded** by [ADR-0006](./ADR/0006-cpt-cf-credstore-adr-immutable-value-versions.md): deletion is an immediate, single-transaction removal of the record — the deprovisioning status and its reference-retention window are withdrawn, the reference is free to reuse the instant the delete completes, and the secret's stored versions are purged afterwards by the same request, from a purge debt recorded in the delete transaction, a failed purge stays recorded (no later access retries it) until a possible external job.

**Rationale**: Tenants must be able to revoke credentials reliably. **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### Tenant Scoping

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-tenant-scoping`

<!-- cpt-cf-id-content -->
The system **MUST** derive the operating tenant from the request SecurityContext (`subject_tenant_id`) and the owner from `subject_id` for all operations. Tenants **MUST NOT** create, update, or delete secrets belonging to other tenants. If the caller's authorized scope does not include their own tenant, the operation is denied before any side effect and the denial is recorded (cross-tenant metric). That denial **MUST NOT** depend on whether the targeted record exists.

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
Every operation **MUST** be authorized through the platform PDP: the gear evaluates an access scope for the operation's action — `read` for a read, `write` for a create or an update, `delete` for a delete — through one PDP evaluation per needed action, however many credential types exist — against the base credential type for an operation on an existing credential (the PDP answers with constraints on the credential type and/or the reference) and against the requested concrete GTS type (including `generic`) for a create — and **MUST** enforce the returned scope on every metadata query at the data layer, including the credential-type and reference constraints in the row lookup itself, enabling per-type policies (e.g., a role that reads `api-key` but not `certificate` secrets) and per-instance policies (e.g., an application that reads only the credential whose reference is `smtp-password`). A create **MUST** be refused (403) when the PDP's reference constraint does not admit the requested reference, and a row the constraints do not admit **MUST** be treated as non-existent, never replaced by an ancestor's value. The gear **MUST NOT** evaluate the PDP once per credential type or derive candidate types from stored rows. Enforcement is fail-closed: a PDP denial denies the operation; a PDP evaluation failure surfaces as unavailable; out-of-scope or type-denied secrets are indistinguishable from non-existent ones on read.

**Superseded in part** by `cpt-cf-credstore-fr-authz-action-split`: the mechanism — one PDP evaluation per needed action, enforced at the data layer, fail-closed — holds unchanged, but the action set does not: the six actions replace `read`/`write`/`delete`, with no synonym for the old pair, so every policy granting them is re-issued. The use cases below describe the shipped combined flow, not the split one.

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
Authorization **MUST** distinguish six actions on the credential resource type: `list`, `read`, `write` and `delete` on the record, and `read_secret` and `write_secret` on the secret. The resource type **MUST** be the credential (`gts.cf.core.credstore.credential.v1~` and its derived types; an operation on an existing credential is evaluated on the base type and the credential type and reference are returned as PDP constraints), renamed from the former `secret.v1~`, so that no pre-rename permission matches an operation on the new surface; policies granting the former actions **MUST** be re-issued against the new type rather than honoured as synonyms. A write **MUST** require `write` when it changes any record field and `write_secret` when it changes the secret, both when it changes both. A read **MUST** require `read`, or `list` when reading several records at once, when it returns any record field, and `read_secret` when it returns the secret, both when it returns both — so a caller reading only the secret needs `read_secret` alone. The purpose an application reads for is expressed by the credential **type** alone: a `read_secret` grant names a concrete type or a GTS wildcard of one, and a service that needs "its own" credentials **MUST** declare its own derived type rather than filing records under a separate label — because the type is immutable once set, a metadata edit can never change who may read a secret. A single credential is addressed in a grant by its reference, never by its record id, which changes when the credential is re-created.

**Rationale**: Enumerating entries, reading a record's metadata, and reading a secret have different blast radius and must be separately grantable; an ambiguous grant would defeat that separation. **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-integrations-admin`, `cpt-cf-credstore-actor-integration-app`
<!-- cpt-cf-id-content -->

### 5.4 P1 — Reliability & Concurrency

#### Crash-Safe Write Lifecycle

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-write-lifecycle`

<!-- cpt-cf-id-content -->
A secret write that spans metadata and backend **MUST** be crash-safe: the secret becomes readable only after its value is durably stored in the backend, and the record switches to it in a single database transaction; a failed or interrupted write leaves the previously served value in place (or no record, on create), at most an unreachable stored version that a durable cleanup obligation covers until it is removed (see the immutable-versions requirement). No failure mode may serve a readable secret without a matching value, serve a value other than the one a write committed, or permanently block the reference.

**Superseded in part** by [ADR-0006](./ADR/0006-cpt-cf-credstore-adr-immutable-value-versions.md) (`cpt-cf-credstore-fr-immutable-value-versions`): the shipped mechanism — a `provisioning` → `active` status, rollback on create, fail-closed overwrite and a periodic reaper — is withdrawn. There is no in-flight status, no half-written record and no reaper: a write only ever produces a fully readable record or none at all, and on an overwrite the old secret keeps serving until the new one is fully in place.

**Rationale**: Readers must never observe half-written secrets; writers must never permanently wedge a secret name. **Actors**: `cpt-cf-credstore-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### Optimistic Concurrency

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-optimistic-concurrency`

<!-- cpt-cf-id-content -->
Each secret **MUST** carry a monotonic version, exposed on retrieval. Update and delete **MUST** require a caller-supplied precondition ("must exist", or "the specified generation must still be current"), enforced atomically with the metadata commit — every write states its concurrency stance, there are no unconditional overwrites; creation is the only preconditionless write. A failed precondition surfaces as a conflict (lost-update detection); a malformed precondition is a validation error; a missing precondition is a validation error with its own distinct reason.

**Superseded in part** by [ADR-0004](./ADR/0004-cpt-cf-credstore-adr-secret-value-exposure.md): the version is exposed only on the caller's own record; an inherited record carries an opaque validator that changes when the ancestor writes but cannot be used in a write precondition. A record write that changes nothing does not advance the version; a secret write always does. All failed preconditions surface identically.

**Rationale**: Lost-update detection for concurrent secret management. **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-platform-gear`
<!-- cpt-cf-id-content -->

### 5.5 P1 — Secret Types

#### GTS-Based Secret Types

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-secret-types`

<!-- cpt-cf-id-content -->
Each secret **MUST** have a *secret type* named explicitly at creation (a create without a type is rejected, `TYPE_REQUIRED`; there is no default) and immutable thereafter. Secret types are GTS types derived from the credstore base type and registered in the types-registry. **Amended by** [ADR-0004](./ADR/0004-cpt-cf-credstore-adr-secret-value-exposure.md): the base type is renamed from `gts.cf.core.credstore.secret.v1~` to `gts.cf.core.credstore.credential.v1~`, every derived type follows, and the type is also the PDP resource type of the new surface. Each type **MUST** declare enforceable traits that the gear applies uniformly, at minimum: which sharing modes the type permits, rejecting a write that requests a disallowed one (e.g., `personal-token` secrets are private-only and can never be shared); optional structural validation of the secret on write; whether the type is expirable, in which case the secret of an expired record is never served (expiry applies to the secret, not to the record); and bounds on the secret's size and encoding.

The initial type catalog covers `generic`, `api-key`, `personal-token`, `oauth2-client`, `basic-auth`, `bearer-token`, `certificate`, `ssh-key`, `webhook-hmac`, and `connection-string` (see DESIGN §5.3). `generic` is an ordinary type that must be named like any other. Expiry applies to the secret, not to the record. A record is *expired* when it is `active` and its expiry has passed (a `declared` record never expires); an expired record stays visible, its metadata is readable by point read and listing with the lifecycle status `expired` (derived from the expiry at read time, never stored) and its normal validator, but its secret **MUST NOT** be served. Any read that would return the secret of an expired decisive record — a point read with the secret selected, the SDK `get_secret` — **MUST** fail with the stable reason `SECRET_EXPIRED` (HTTP 409, canonical category failed-precondition), and resolution **MUST NOT** continue up the tenant chain past an expired decisive record: if the caller's own override is expired, or the decisive ancestor `shared` record is expired, the answer is `SECRET_EXPIRED`, never an ancestor's value; the suppression `fallback` does not apply to expired records (it governs `declared` records only). In the bulk secret read an expired item **MUST** be returned with status `expired` and without a secret, and the request **MUST NOT** fail because of it. The reason `SECRET_EXPIRED` **MUST** be disclosed only to a caller authorized to read the secret of that type; every other caller gets the ordinary not-found answer. A replace or a partial update addressed to the caller's own expired record finds it and updates it in place (the record keeps its id), which is how it is renewed — a partial update of the expiry restores the secret; a create-only write over an expired own record is refused as already existing (409 `ALREADY_EXISTS`) because the record is visible: it is renewed or deleted instead. Nothing sweeps expired records: an expired record stays until it is renewed in place by a replace or a partial update, or removed by its owner's delete.

**Rationale**: Different kinds of secrets have different safe-handling rules; encoding them as GTS type traits gives one enforcement point in the gear, platform-native discoverability/versioning, and per-type policy targeting (PDP) without per-secret ACLs. **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-platform-gear`
<!-- cpt-cf-id-content -->

### 5.6 P1 — Deprovisioning Lifecycle

#### Crash-Safe Delete (Deprovisioning Saga)

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-deprovisioning`

<!-- cpt-cf-id-content -->
Secret deletion **MUST** be a crash-safe lifecycle symmetric to provisioning: the secret first enters a `deprovisioning` status — at which instant it atomically stops resolving — then the backend secret is deleted, then the metadata record is removed. A failure or crash at any step leaves a non-readable `deprovisioning` record that (a) a client retry of the delete resumes idempotently, and (b) the reaper completes within a configurable timeout. While a reference is deprovisioning, re-creating it **MUST** fail with a retryable conflict (the name is released only after backend cleanup completes).

**Superseded** by [ADR-0006](./ADR/0006-cpt-cf-credstore-adr-immutable-value-versions.md): deletion is an immediate, single-transaction removal with no intermediate status and no name-retention window — the reference is free to reuse the instant the delete completes, because a re-created record always gets a new identity and therefore a new store key that a lagging purge of the old one cannot touch; crash-safety is unchanged in outcome, and the deleted record's stored versions are purged by the deleting request from a purge debt recorded in the delete transaction (a failed purge stays recorded, with no later access to retry it), rather than by a reaper.

**Rationale**: A plain backend-first delete leaves metadata/backend divergence on partial failure with no self-healing owner; the status-driven lifecycle plus reaper makes revocation reliable and observable, and closes the orphaned-backend-secret debt of the write lifecycle (the reaper reconciles backend secrets for all reaped records). **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-platform-gear`
<!-- cpt-cf-id-content -->

### 5.7 P2 — Planned

#### Production Value-Store Backend

- [ ] `p2` - **ID**: `cpt-cf-credstore-fr-production-backend`

<!-- cpt-cf-id-content -->
The system **MUST** provide at least one production-grade value-store plugin (external secret vault, KMS-backed store, or OS-protected storage for desktop/VM environments) implementing the same plugin contract as the development in-memory plugin, including its value versions, durable writes and exact-bytes reads (`cpt-cf-credstore-interface-plugin-client`). `vault-credstore-plugin` (HashiCorp Vault / OpenBao KV v2) is the production-grade implementation of this requirement for Vault-backed deployments; its own requirements and design are in the plugin's PRD and DESIGN. The provider-specific settings that guarantee this are in DESIGN (`cpt-cf-credstore-fr-backend-compatibility`). The value store **MUST** be a plain versioned byte store that knows nothing about tenants, hierarchy, references, types or sharing; the metadata store alone decides which version is current. The value store **MUST NOT** delete, on its own, a version the gear references: the version a record points at disappears only through the gear's destroy or delete-key operation; removing versions no record references any more is harmless. A backend that limits retention **MUST** be configured so that it never deletes a referenced version on its own (for Vault: `max_versions` above the number of versions a key accumulates, and `delete_version_after` disabled). The value store **MUST** encrypt values at rest and in transit, and **MUST** be accessible only by the gear, with permissions limited to the operations the gear uses and to its own key space. Concurrent writes to one key are allowed; each creates a new version and the metadata store's compare-and-swap decides the winner. Backend selection remains a deployment-time configuration with no consumer-visible change.

**Rationale**: The in-memory static plugin is suitable for development and testing only (secrets do not survive process restart). **Actors**: `cpt-cf-credstore-actor-backend`
<!-- cpt-cf-id-content -->

#### Backend Compatibility

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-backend-compatibility`

<!-- cpt-cf-id-content -->
Base compatibility, the three required plugin operations (write returning a version, read by version, delete the key), **MUST** be provided for HashiCorp Vault KV v2, OpenBao KV v2, Google Cloud Secret Manager, AWS Secrets Manager and Azure Key Vault. Full compatibility, which adds destroy with ordered versions, **MUST** be provided for Vault, OpenBao and Google Cloud Secret Manager. AWS Secrets Manager and Azure Key Vault run without destroy, with the accepted limitation that a rotated or removed secret stays in the backend until the record is deleted. The per-provider mapping and obligations are in the plugin SPI section of DESIGN.

**Rationale**: Customers run the platform on different secret stores; the narrow plugin contract keeps each backend's adapter thin. **Actors**: `cpt-cf-credstore-actor-backend`
<!-- cpt-cf-id-content -->

### 5.8 P1 — Credential Records and Secrets

> **Implemented.** Every requirement in this section is specified by [ADR-0004](./ADR/0004-cpt-cf-credstore-adr-secret-value-exposure.md) and [ADR-0005](./ADR/0005-cpt-cf-credstore-adr-upward-collection-read.md) and implemented together with ADR-0006 through ADR-0010. It supersedes in part the earlier combined credential-and-secret contract of §5.1 through §5.6 (the superseded passages say so). The acceptance criteria in §9 that reference these IDs are implemented on the same terms.

#### Credential Record and Secret Split

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-credential-record`

<!-- cpt-cf-id-content -->
The system **MUST** treat a credential's record and its secret as one entity, but **MUST** default a read of that credential to metadata only — reference, sharing mode, type, fallback policy, the lifecycle status of the caller's own record (none, declared, active, or expired), expiry, inheritance status, and, for the caller's own record only, version, last-update time and the creating subject — and **MUST NOT** return the secret by default. The metadata **MUST NOT** name the owning tenant, nor the creating subject of an inherited record: for an inherited credential that would disclose an ancestor's identifier the caller cannot obtain by any authorized route, and the inheritance status already answers whether the record is the caller's own. The secret **MUST** be obtainable only when the caller explicitly asks for it and holds `read_secret`, as part of the same credential — never as a separately addressable resource.

**Rationale**: A metadata surface cannot leak a secret it structurally does not contain; separating the two makes a secret-blind administrator role possible. **Actors**: `cpt-cf-credstore-actor-integrations-admin`, `cpt-cf-credstore-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### List Credential Records

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-list-credentials`

<!-- cpt-cf-id-content -->
The system **MUST** allow an authorized caller to list the credential records effectively visible to its tenant, paginated, ordered deterministically, and **never carrying a secret regardless of the caller's grants — unless the caller explicitly selects `secret`**, in which case `cpt-cf-credstore-fr-bulk-read-secrets` below governs what is disclosed and the same pagination, ordering and filters apply. The listing **MUST** follow the platform's cursor-pagination contract: a bounded page reached by an opaque cursor, filterable and orderable only on an allowlisted set of indexed fields, with no total count. The caller **MUST** be able to select a subset of fields to return, restricted to the credential's field allowlist; an unrecognized field **MUST** be rejected as a validation error; with no selection the item **MUST** be the full record. Of the filterable fields, only reference and type **MUST** be applied before hierarchical resolution, since they are invariant across a reference's chain; sharing, expiry and fallback **MUST** be filtered only after resolution, since they vary along the chain and filtering them earlier could change which record wins. Listing **MUST** require its own authorization action, distinct from reading a single record.

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
The system **MUST** write a credential's secret only as part of writing its record, and **MUST NOT** grant the ability to read that secret as a side effect of granting the ability to write it. The write-secret action **MUST** be required whenever a request writes or removes a secret, and the record-write action **MUST** also be required when the same request carries metadata fields. The write-secret action **MUST NOT** be required when a `PUT` states that a record has no secret and it already had none — creating a record with no secret, or replacing an already secret-less record with the same absence, needs only the record-write action, because no secret is being read or changed. This exemption is `PUT`-only: a `PATCH` carrying an explicit `secret` key, string or `null`, **MUST** require the write-secret action regardless of the record's prior state, because — unlike `PUT`'s full-record view — a merge-patch has no other signal that the caller intended to touch the secret at all; a caller who only means to leave the secret untouched omits the key rather than sending `null`. A partial update that removes a secret **MUST** return the record to holding no secret while leaving the rest of its metadata intact, and the record's fallback policy then decides whether the reference inherits or resolves as absent. A partial update **MUST NOT** create a record — a reference with no record of the caller's own is a not-found. The write **MUST NOT** be permitted for a record owned by an ancestor: a tenant that wants its own secret creates its own record first. A write that changes or removes a secret **MUST** always write and always advance the version, even when a submitted secret equals the stored one, under the record's one validator.

**Rationale**: This is the requirement that makes the secret-blind configurator possible — the persona who provisions and rotates an integration's credentials without ever being able to read one. **Actors**: `cpt-cf-credstore-actor-integrations-admin`, `cpt-cf-credstore-actor-platform-gear`
<!-- cpt-cf-id-content -->

#### Immutable Value Versions

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-immutable-value-versions`

<!-- cpt-cf-id-content -->
Every write of a secret **MUST** create a new immutable value version, whose version identifier the backend provider assigns, stored under one key per record, rather than overwriting a stored one in place; the record **MUST** switch to the new version in a single database transaction (a compare-and-swap on the row's own version counter, distinct from the value version), and **MUST NOT** ever come to reference a version that was not completed on its behalf. Before a secret value is stored, the system **MUST** record the write in its own database as an announcement bound to the record key (a write intent) and **MUST** retire that announcement in the transaction that switches the record; a write that finds its announcement already healed away **MUST NOT** switch the record. When the backend supports destroy, the switching transaction **MUST** also record the destruction of the record's older versions as a cleanup debt in the gear's database; after a confirmed commit the same request **MUST** execute it and delete the debt on success, and a debt left over **MUST** be executed by the next request that reads or writes the record; a failed destruction **MUST NOT** fail the write or change its answer, and the system **MUST NOT** rely on any best-effort backend call for cleanup that is not backed by a recorded debt. On a backend without destroy, nothing is destroyed: rotated, removed and orphaned versions stay in the backend, unreachable through the gear, until the record is deleted. An interrupted write **MUST** cost at most unreachable versions under that record's key (within the store's version limit, see accepted behavior 5) — never a wrong secret, a closed read, or a permanently reserved reference — and **MUST NOT** delete a version the record may reference when the outcome of the switch is unknown. Removing a record's secret **MUST**, when the backend supports destroy, destroy the old version and everything below it, recording that destruction as a cleanup debt in the same transaction that removes the pointer, and **MUST NOT** delete the record's key. Deleting a record **MUST** remove the row and record the purge of its key as a cleanup debt in the same transaction; the deleting request **MUST** execute the purge after a confirmed commit, a failed purge stays recorded, **MUST NOT** fail the delete or block or re-expose the reference, and is not retried by any later access (no row remains and the record identifier is never reused; only a possible external job would retry it), and a re-created record **MUST** get a new identity and key that a lagging purge cannot touch. A record holding no secret **MUST** be indistinguishable, in what it references, from one that never held one. Metadata-only reads, listings and metadata-only updates **MUST NOT** touch the backend. A secret read **MUST** retry once, after re-reading the record, when its version has been destroyed by a concurrent write, and **MUST** report unavailability rather than a stale or empty value on a second miss. No resident loop and no maintenance job exist: the process **MUST NOT** run any background worker or task, timer, database polling, deferred or delayed task, or work at startup, and no correctness property depends on any background process; every store side effect is executed by the request that caused it, and leftovers of an interrupted request are healed by a later request that touches the same record.

**Store hygiene.** A superseded, removed or orphaned secret version **MUST** be covered by a durable cleanup obligation (a recorded cleanup debt or an open write announcement) recorded in the same transaction that makes it unreachable — for a version that may be stored without ever being switched to, before it is stored — except for the residuals listed under accepted behavior. A write announcement whose writer has gone **MUST** be healed only on access once its lease has expired: it is deleted in the transaction of the next successful secret write to the same record, or, for a create that failed, by a later create or read of the same reference that finds no record row, which records and executes the purge of its key; reads **MUST NOT** touch the announcements of a live record. The lease **MUST** comfortably exceed the time a backend may still apply a request it has received (a deployment requirement, for example the backend's maximum request duration); no in-process check enforces it. Safety — a record never references a missing or different version — **MUST NOT** depend on the lease or on any cleanup having run.

**Backend contract.** Every backend **MUST** provide three operations: write a value under the record key and return the version the provider assigned to the new immutable value (durable before returning); read a version, returning exactly the bytes of the write that returned it, or nothing, and a read **MAY** report that the version exists but can never be read (for example a lost decryption key); and delete the record's key with all its versions (idempotent). Every operation names the record key explicitly (tenant and record identifier) and the gear alone chooses it. A backend **MUST NOT** be required to provide compare-and-swap, listing, cross-key transactions or server-side logic. A fourth operation, destroy, is optional and idempotent: it permanently deletes either every version older than a given version or exactly one version. The system **MUST** distinguish a permanently unreadable version from an outage and **MUST NOT** answer it as retryable: a secret read whose version the record points at but the backend cannot return, with the pointer unchanged, fails as an internal error (HTTP 500) and is logged, and the record is rewritten or deleted instead; in the bulk secret read such an item fails the request. A miss after the pointer moved (a concurrent rotation) is re-read once and, if it misses again, stays a 503. The plugin declares whether it supports destroy. A backend that supports destroy **MUST** return ordered versions (a write that starts after another write on the same key has returned gets a greater version), because destroying everything below a committed version is safe only when every older version belongs to a writer with a stale base; a backend without destroy does not need ordered versions. Without destroy, rotated, removed and orphaned versions stay in the backend until the record is deleted; this is the accepted behaviour of such backends.

**Accepted behavior.** Residuals (1) to (4) are garbage only and never affect consistency; (5) makes a record unreadable; they are kept short here and catalogued, together with the failure scenarios they come from, in `CORNER-CASES.md` (added with the code in #4741): (1) an unreachable version above the committed version of a live record (a crash, a failed switch or a lost store answer after the store of a replacement) is never served and stays, tracked by its announcement, until the record's next secret write or its delete; (2) a late landing of a stored write after the lease was healed, or after the key was purged, leaves an untracked version (on a dead key, permanent), and safety never depends on the lease; closing it needs a conditional write in the backend contract, which is not part of the contract; (3) a backend without destroy keeps rotated and removed versions, and orphans, until the record is deleted; (4) a cleanup debt whose execution failed stays recorded with its bytes in the store: for a live record until it is accessed again, for a deleted record's purge (and an expired announcement of a failed create nobody retries) until a possible external job, which is not part of this release; (5) more versions above the pointer than the store keeps, from failed writes in a row or a burst of concurrent writers, evicts the pointed-at version, and the record answers an internal error (500) until rewritten (operator setting). Also accepted: (6) the system does not detect out-of-band modification of a stored version — a backend that returns different bytes violates its contract; (7) audit publication is best-effort: a failed publication is logged and counted and never blocks, fails or rolls back an operation (`cpt-cf-credstore-nfr-audit`); (8) the gear's database is the system of record for metadata and **MUST** be backed up together with the store, and the metadata index is not rebuildable from the store; index rebuild is out of scope; (9) an expired record stays visible with its metadata and status `expired` but its secret is never served; it is renewed in place by its owner or removed only by its owner's delete.

**Rationale**: Overwriting a stored secret in place makes every partial-write failure a race between two states of the same bytes; giving each write its own version removes that race entirely — a failure can only leave garbage beside the truth, never inside it. This is the versioning model every managed secret store uses, and it is why the in-process reaper, the garbage-collection table and the maintenance job are all withdrawn: with no intermediate state to repair, the leftover garbage is found by position — below the committed version — every version that can become garbage is covered by a durable obligation (a write announcement before it is stored, a recorded cleanup debt when it becomes unreachable), and it is that obligation, executed by the request that caused it or healed by a later request that touches the record, that removes it. **Actors**: `cpt-cf-credstore-actor-platform-gear`, `cpt-cf-credstore-actor-integrations-admin`, `cpt-cf-credstore-actor-backend`
<!-- cpt-cf-id-content -->

#### Read Secrets Through the Collection

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-bulk-read-secrets`

<!-- cpt-cf-id-content -->
The system **MUST** allow an authorized caller to read the secrets of several credentials through the same paginated collection used for metadata, by selecting the secret. Reading a secret **MUST** require `read_secret`; besides it the collection **MUST** accept only `reference`, `type` and `expires_at`, any other field being rejected as invalid. Pagination, ordering and filters **MUST** be those of the metadata listing; selecting the secret changes only what disclosure requires. The set read this way **MUST NOT** be able to exceed what the caller may read one by one: each item **MUST** be authorized individually, and a record of a type or reference the caller may not `read_secret` **MUST** be omitted entirely rather than reported by name, because the filter found it, not the caller. Each secret **MUST** be read at the version stored on its record; an expired item **MUST** be returned with its metadata and without a secret; an item whose stored version the store cannot return **MUST** fail the whole request. A response that carries secrets **MUST NOT** be cached. One audit record **MUST** be produced per secret returned.

**Rationale**: Applications commonly need their credential set in few round-trips; a small set fits one page. Disclosure is bounded by the caller's own grants (`read_secret` per type or reference) and audited per secret, not by a cap or by withholding pagination: a caller holding `list` and `read_secret` could page the metadata and fetch the secrets one by one anyway. A page of secrets is bounded by the page `limit`. Serving this through the same listing as metadata means one authorization path covers both. **Actors**: `cpt-cf-credstore-actor-integration-app`
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
A new non-private (`tenant` or `shared`) credential record for a reference that currently resolves, for its creator, to an existing non-private credential — in practice an ancestor's `shared` one — **MUST** carry that credential's secret type; a differing type **MUST** be rejected as a conflict. A new non-private credential record **MUST** also be rejected as a conflict when any descendant tenant of its creator — in the tenant hierarchy as inheritance sees it, isolation barriers included — holds a non-private record of the same reference, in any status, whose secret type differs; that rejection **MUST NOT** disclose which tenant holds the record or its type. A `private` record (visible to its owner only) **MAY** carry any type: private records are neither checked nor counted by this rule. Consuming applications address a credential by reference and rely on its type to know the shape of the secret, so a local override of a different type would break them without any change on their side. The rule applies only at creation: the type is immutable afterwards, and a reference with no record above or below its creator may be created with any registered type. It is a check at creation, not an invariant the system maintains: two creates of one reference in an ancestor and a descendant that overlap in time, or a tenant moved under a new parent, can still leave different types on one chain.

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
No operation may read or modify secret metadata outside the caller's PDP-authorized tenant scope; enforcement happens at the data layer on every query. Inaccessible secrets are indistinguishable from non-existent ones (anti-enumeration). This equivalence **MUST** hold per item inside a bulk response, not only for point reads. The same equivalence **MUST** hold for writes and deletes: a caller without permission on a record **MUST NOT** be able to tell, from any response to a create, replace, patch, remove-secret or delete, whether that record exists — the authorization decision **MUST** be taken before the record is looked up, a record the caller may not act on **MUST** be answered exactly as a missing one, and a precondition (`If-Match`, `If-None-Match`) **MUST** be evaluated only after authorization and only against a record the caller may act on. A create over a name already taken **MUST** answer the same conflict whatever the occupant is, and **MUST NOT** disclose its type or identifier.

**Threshold**: Zero cross-tenant reads/writes outside the authorized scope **Rationale**: Multi-tenant platform guarantee. **Architecture Allocation**: PDP scope + data-layer clamps; see DESIGN.md §3.1
<!-- cpt-cf-id-content -->

#### Audit

- [ ] `p1` - **ID**: `cpt-cf-credstore-nfr-audit`

<!-- cpt-cf-id-content -->
Every read of a secret (point read, `get_secret`, a collection read with `secret` selected) and every write of a secret (create or replace carrying a secret, a patch that sets or removes the secret, deleting a record that holds a secret) **MUST** publish an audit event through the platform event-broker gear (`event-broker`). Events name the subject, the tenant acted in, the reference, the credential type, the operation and its outcome, and **MUST NOT** contain the secret. Publishing is best-effort: if the event broker is unavailable or rejects the event, the operation **MUST** continue without interruption, the gear **MUST** log an error (without the secret) and count the failure in a metric; the audit never blocks, fails or rolls back a read or a write. PDP-denied attempts and reads that resolve nothing are not audited.

**Threshold**: One audit event per secret read or written; zero secrets in any audit event; zero reads or writes failed, delayed beyond the publish attempt, or rolled back by an audit failure **Rationale**: Secret disclosure and secret changes must be attributable to a subject, but an audit sink outage must not turn into a credential outage for every consumer. **Actors**: `cpt-cf-credstore-actor-integration-app`, `cpt-cf-credstore-actor-oagw`, `cpt-cf-credstore-actor-platform-gear` **Architecture Allocation**: See DESIGN.md §6.5 and §10 Observability
<!-- cpt-cf-id-content -->

#### Observability

- [ ] `p1` - **ID**: `cpt-cf-credstore-nfr-observability`

<!-- cpt-cf-id-content -->
The gear **MUST** emit operational metrics sufficient to detect resolution anomalies and lifecycle divergence: walk-up depth, read outcome (own/inherited/miss), per-dependency latency and outcome (PDP, tenant-resolver, plugin), cross-tenant denials, and counters for recorded and failed store cleanup, healed write intents, verified ambiguous commits and read retries ([ADR-0006](./ADR/0006-cpt-cf-credstore-adr-immutable-value-versions.md)); the withdrawn reaper's rollback/reap counters and inventory count are not replaced, consistent with the platform's rule against counting queries. Metric labels **MUST NOT** contain secret references or secrets.

**Rationale**: Crash-safe writes/deletes and hierarchical resolution — under ADR-0006, the write/delete lifecycle and its store cleanup — fail in partial, quiet ways; operators need signals, not log archaeology. **Architecture Allocation**: See DESIGN.md §10 Observability
<!-- cpt-cf-id-content -->

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### CredStoreClientV1

- [ ] `p1` - **ID**: `cpt-cf-credstore-interface-client`

<!-- cpt-cf-id-content -->
**Type**: Rust trait (async) **Stability**: stable **Description**: Public API for platform gears. Registered in ClientHub without scope. Operations, as reshaped by ADR-0004: a read (the credential's metadata, plus its secret only when asked for, never the owning tenant), a secret-read convenience returning just the secret and what is needed to use it, a precondition-guarded create-or-replace of the record together with its secret in one call (the secret may be supplied, or the record explicitly left without one), a precondition-guarded partial update (metadata edit, secret rotate, or secret removal; never creates), a filtered/paginated listing of records that also returns the secrets of its page when `secret` is selected, and a precondition-guarded delete. The guarded create-or-replace is the only way to create a credential. Hierarchical resolution is internal to the gear. **Breaking Change Policy**: Major version bump required
<!-- cpt-cf-id-content -->

#### CredStorePluginClientV2

- [ ] `p1` - **ID**: `cpt-cf-credstore-interface-plugin-client`

<!-- cpt-cf-id-content -->
**Type**: Rust trait (async) **Stability**: unstable **Description**: Plugin SPI for backend secret stores (`CredStorePluginClientV2`). Registered in ClientHub with GTS instance scope. Every operation names the record key explicitly (tenant and record identifier; the gear chooses it) and carries the request context for correlation only, never for authorization. Required: write a value under a key and receive the version the provider assigned; read a version by key and version (exactly the bytes written, or nothing); delete the record's key with all its versions (idempotent). Optional, declared by a capability flag: destroy, idempotent, selecting every version older than a reference or exactly one version; a plugin that supports it must return ordered versions per key. Required of the backend: durable writes, exact-bytes reads; not required: compare-and-swap, listing, cross-key transactions. Returns the secret only — no metadata, no policy. **Breaking Change Policy**: Minor version bump (unstable API)
<!-- cpt-cf-id-content -->

### 7.2 External Integration Contracts

#### REST API

- [ ] `p1` - **ID**: `cpt-cf-credstore-contract-rest-api`

<!-- cpt-cf-id-content -->
**Direction**: provided **Protocol/Format**: HTTP/REST, JSON, canonical `Problem` error envelope, under a versioned path served beneath the platform API prefix. Exposes create-only and precondition-guarded update writes, retrieval, and delete over the credential reference, with optional credential-type and expiry inputs on writes, a mandatory precondition on update and delete, and secret-confidentiality response controls. See DESIGN.md for the concrete endpoints, methods, status codes, and headers.

**Compatibility**: **not** backward-compatible under [ADR-0004](./ADR/0004-cpt-cf-credstore-adr-secret-value-exposure.md), which is the point of that decision rather than a side effect. The credential address stops returning the secret by default and starts returning the record; the listing is renamed; the secret becomes a selectable part of the same record and listing addresses rather than moving to a dedicated one; the create path becomes a guarded create-or-replace that stays atomic — record and secret in one request; a merge-style partial update is added (metadata edit, secret rotate, secret remove); and the PDP action set is replaced with no synonym for the old read/write pair, so every policy granting them is re-issued. What **does** hold across the change: the reference stays the caller-chosen name and never becomes an internal row id, the `Problem` envelope and the canonical status mapping are unchanged, the optimistic-concurrency validator keeps its shape for an own record, and secret-confidentiality controls (no intermediary caching, per-secret audit) apply to every address that discloses a secret. The replacement surface is implemented: the `/credstore/v1/secrets` routes of the first release no longer exist and `/credstore/v1/credentials` (DESIGN §4.3.1) is the only REST surface.
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
3. The write completes only once the secret is durably stored as a fresh, immutable secret version and the record has switched to it in one transaction
4. Secret is immediately resolvable by the partner and all descendant tenants

**Postconditions**:
- Secret is stored and accessible to partner and descendants

**Alternative Flows**:
- **Secret already exists (same class)**: secret and sharing updated, version bumped
- **Create-only write**: fails with a conflict if the reference is taken in that sharing class
- **Backend write fails**: leaves nothing readable and never permanently blocks the reference — on create no record exists at all, on replace the previously served value keeps serving; at most an unreachable secret version remains, covered by the write's open announcement: after its lease expires a later create or read of the same reference heals the announcement and purges the key of a failed create (there is no record to write to again), while on a replace the orphan above the committed version is removed by the record's next secret write or its delete (accepted residual)
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
3. Gear obtains the customer's full ancestor chain
4. Gear resolves the reference against the whole ancestor chain → the partner's `shared` secret wins (the customer holds none)
5. Gear reads the secret only for the winning credential
6. OAGW receives the secret plus the credential record (inheritance status = inherited, version; the owning tenant is not named — ADR-0004)

**Postconditions**:
- OAGW has the decrypted secret; the customer never sees it
- Resolution depth and inherited-read outcome are recorded as metrics

**Alternative Flows**:
- **Customer has own accessible secret**: it wins (shadowing); the parent's credential is not considered
- **No accessible secret in the chain**: not-found
- **The decisive record is expired**: the read fails with `SECRET_EXPIRED` (409); the secret of an ancestor is never substituted
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
- **Delete, replace or patch a record the caller has no permission on**: the same answer as for a record that does not exist, whether or not it exists (anti-enumeration)
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
- **Type omitted on create**: rejected with 400 `TYPE_REQUIRED`; there is no default type (`generic` must be named explicitly)
- **Attempt to change the type of an existing secret**: rejected as unsupported transition
<!-- cpt-cf-id-content -->

#### UC-008: Reliable Revocation via Deprovisioning

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-deprovisioning`

<!-- cpt-cf-id-content -->
**Actor**: `cpt-cf-credstore-actor-tenant-admin`

**Preconditions**:
- Tenant owns an `active` secret consumed by descendants

**Main Flow** ([ADR-0006](./ADR/0006-cpt-cf-credstore-adr-immutable-value-versions.md)):
1. Tenant deletes the secret by reference
2. The record and reference are removed immediately in one transaction, with no intermediate status and no name-retention window, and the purge of the record's stored versions is recorded as a debt in the same transaction
3. The deleting request purges the stored versions after the confirmed commit and deletes the debt on success; a failure leaves the debt recorded and never blocks or re-exposes the reference

**Postconditions**:
- Secret fully revoked; the reference is immediately reusable, and a reuse before the purge finishes cannot collide with or be clobbered by it — the re-created record gets a new identity and its own store key

**Alternative Flows**:
- **Purge fails**: invisible to the caller — the record and the reference are already gone; the debt stays recorded and no later access retries it (no row, record identifier never reused), so the stored versions stay until a possible external job
- **Retry the delete**: idempotent — the record no longer exists, so a retried delete is an ordinary not-found, not a resumed lifecycle
- **Crash mid-delete**: the delete either completed in full (record gone, purge debt recorded) or not at all (record and reference untouched); there is no partial state to recover
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
1. Application issues one collection read filtered to its credential type, selecting `secret`, asking for the reference, type, expiry and secret
2. The system resolves and authorizes the matching type with a single `read_secret` decision, then returns the matching credentials page by page (a small set fits one page), each item carrying the secret with its type and expiry and nothing administrative
3. Credentials of other types are not evaluated and do not appear in the result

**Postconditions**:
- The application has its full SMTP credential set from a paginated read, scoped exactly to its own grant

**Alternative Flows**:
- **More matches than one page**: the response carries a cursor and the application continues with it; nothing is truncated
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
- [ ] Half-written secrets are never readable; failed writes leave the previously served value in place (or no record, on create); no in-flight record can exist, because a write only ever produces a fully readable record or none at all; **no background timer, worker or maintenance job runs inside or beside the gear** — store cleanup (purges of deleted records' keys and destruction of superseded or removed versions) is executed by the request that caused it and, for a live record, healed on a later access; expired write announcements are healed on access
- [ ] `p1` **Immutable value versions** (ADR-0006): every write of a secret creates a new immutable version, assigned by the backend provider, rather than overwriting one in place, and a record only ever comes to point at a version once it is complete and the record has switched to it in one transaction; every write is announced in the gear's database before its value is stored and the announcement is retired in the switching transaction; an interrupted write leaves the old secret still served and at most unreachable versions, each covered by an open announcement or a recorded cleanup debt until removed (except the documented residuals; on a backend without destroy rotated and removed versions stay until record deletion) — never a wrong secret, a closed read, or a reserved reference (within the store's version limit); no record can be left in an in-flight status; a read that races a write retries once and otherwise reports unavailability; deleting a record records the purge of its key and the request executes it, and a failed purge stays recorded; metadata-only operations make no backend call
- [ ] Backend compatibility (`cpt-cf-credstore-fr-backend-compatibility`): each of Vault KV v2, OpenBao KV v2, Google Cloud Secret Manager, AWS Secrets Manager and Azure Key Vault passes the plugin conformance suite for its compatibility level: base (write, read by version, delete key) for AWS and Azure; full (base plus destroy with ordered versions) for Vault, OpenBao and GCP
- [ ] Retrieval exposes the current version and secret-confidentiality controls; update/delete require a precondition, with a conflict on stale versions and a distinct validation error when it is missing
- [ ] Secrets never appear in log output or metric labels; a non-UTF-8 secret is rejected, not corrupted
- [ ] Every secret read and every secret write publishes an audit event through `event-broker` naming subject, tenant, reference, type, operation and outcome and never the secret (`cpt-cf-credstore-nfr-audit`); with the event broker unavailable or rejecting events, reads and writes still succeed, an error is logged without the secret, and the failure is counted in a metric
- [ ] Secret types: a write violating the type's sharing, schema, size/format, or expiry rules is rejected with a stable reason; the type is required on create (400 `TYPE_REQUIRED`), immutable, and returned in metadata; an expired record stays visible with status `expired` and its normal validator, while any read of its secret fails with `SECRET_EXPIRED` (409) without falling through to an ancestor's value (and is not-found for a caller who may not read the secret); the bulk secret read returns the item with status `expired` and no secret; an expired record is renewed in place (same id) by a replace or partial update addressed to the owner's own record, a create-only write over it is refused with 409 `ALREADY_EXISTS`, and it is otherwise removed only by its owner's delete (no sweep)
- [ ] Deprovisioning: a deleted secret stops resolving and the reference is free to reuse the instant the delete transaction commits, with no intermediate status; a failed purge of the stored versions stays recorded and never blocks or re-exposes the reference; there is no conflict window, because a re-created record never shares the deleted record's store key
- [ ] (ADR-0004) A credential's record and its optional secret share one representation; the default read omits the secret, which appears only when explicitly asked for, gated by `read_secret`, whether reading one credential or listing them
- [ ] (ADR-0004/ADR-0005) Listing returns credential records only, paginated per the platform cursor contract, filterable and orderable only on allowlisted indexed fields, with no total count, and requires its own authorization action
- [ ] (ADR-0004) A single credential can be read by reference, returning the same representation the listing uses; by default the item never carries the secret; explicitly asking for the secret discloses it under `read_secret`; the response always carries the current version validator for the caller's own record; an inaccessible or non-resolving record is indistinguishable in the response
- [ ] (ADR-0004/ADR-0007) A credential can be created or fully replaced in one request — record and secret together — under a create-only, guarded-replace, or last-writer-wins precondition; the secret type never changes; omitting the secret from the request is a validation error; supplying one writes it; explicitly stating the record has none is accepted and creates or leaves the record without a secret, with no backend call on create, removing an existing secret in the same request when replacing a record that holds one, and leaving an already secret-less record unchanged on replace; a partial update applies only the fields supplied, leaves the rest untouched, never creates, and can likewise state that a record has no secret to remove one without deleting the record; a record without a secret neither resolves for secret reads nor hides an inherited secret
- [ ] (ADR-0004) A credential's secret can be read, hierarchically resolved, whether reading one credential or several, with caching disabled and one audit record per secret returned; there is no separate address for the secret alone
- [ ] (ADR-0004/ADR-0005) Several credentials' secrets can be read through the paginated collection by selecting `secret`; `limit`, `cursor`, `$orderby` and `$filter` behave as in the metadata listing; each item is authorized individually and a record the caller may not `read_secret` is omitted rather than reported; the response is not cached; one audit record is produced per secret returned
- [ ] (ADR-0010) Authorization distinguishes six actions — list records, read one record, write a record, read a secret, write a secret, delete — and no policy grants secret access as a side effect of a metadata grant
- [ ] (ADR-0004) Every credential record states whether it is owned, inherited, or an override of an ancestor's shared credential, and this status is readable with metadata-only access
- [ ] `p2` (ADR-0008) A tenant can suppress an inherited credential so it resolves as absent locally and for its descendants, without altering the ancestor's credential
- [ ] (ADR-0008) A secret-less record set to suppress makes its reference resolve as absent for its tenant and, when `shared`, its descendants; the ancestor's credential is untouched; a single partial update can apply the suppression and remove the secret together
- [ ] (ADR-0004/ADR-0008) A partial update that removes a secret leaves a secret-less record whose fallback policy decides resolution; a partial update that carries only metadata, by a caller holding only the record-write action, succeeds, and one that also carries a secret, by that same caller, is refused

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| `event-broker` | Platform event broker: receives audit events for secret reads and writes; non-blocking, best-effort (an outage never fails an operation) | `p1` |
| `authz-resolver` | PDP: per-operation access-scope evaluation (fail-closed) | `p1` |
| `tenant-resolver` | Tenant ancestor chains for hierarchical resolution | `p1` |
| `types-registry` | GTS plugin discovery; secret resource type + secret-type registrations | `p1` |
| Database (PostgreSQL / SQLite) | Gear-owned secret metadata | `p1` |
| Value-store plugin | Per-tenant versioned secret persistence (`static-credstore-plugin` for dev/test; `vault-credstore-plugin` over Vault / OpenBao KV v2 for production) | `p1` |
| OAGW | Primary consumer of hierarchical secret retrieval (uses the SDK client) | `p1` |
| PDP policy re-issuance | Existing `read` grants must be re-issued under the six-action split (list records, read record, write record, read secret, write secret, delete) before the split is deployed | `p1` |
| Database indexes on secret type and reference | The filtered collection read depends on indexes over the allowlisted metadata fields; without them the read is unindexed and slow | `p1` |

## 11. Assumptions

- The gear owns all secret metadata; backends store secret value versions only and provide durable writes, exact-bytes reads and idempotent key deletion (and, optionally, destroy with ordered versions) without hierarchical or policy logic
- The gear's database is the system of record for metadata and is backed up together with the store; the metadata index is not rebuildable from the store
- Exactly one value-store plugin is active per deployment (GTS vendor match)
- Tenant hierarchy is managed externally and served by `tenant-resolver`; the gear reads ancestor chains from it on every request (no local cache; a shared cache, if needed, belongs in tenant-resolver)
- The PDP is the sole authorization authority; there is no local policy cache (policy freshness over availability)
- Consumers provisioning infrastructure from secrets at startup (e.g., mini-chat → OAGW upstreams) tolerate missing secrets by degrading per-provider rather than failing boot
- OAGW is a ToolKit gear that uses the standard CredStore SDK client (all access flows through Gear → Plugin)

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Secrets leaked through logs/caches | Critical security incident | NFR enforcement (redaction, zeroize, non-cacheable responses), code review |
| Metadata/backend divergence on partial write or delete failure | Unreachable stored versions (storage cost only; no record can reference them) | Every unreachable version is covered by an open write announcement or a recorded cleanup debt (destruction of older and removed versions on a backend with destroy, purge of dead keys), executed by the causing request and, for a live record, healed on a later access to it (a failed purge of a deleted record's key stays recorded); expired announcements are healed on access; without destroy, rotated, removed and orphaned versions stay in the backend until record deletion (accepted); counters for recorded, failed and healed cleanup |
| Unreachable versions above the committed version of a live record that is never written again | A replacement whose switch failed ambiguously, or a writer that crashed after storing its value, leaves a version nobody references; it stays until the record's next secret write or its delete | Accepted residual: a storage cost only, never a correctness risk; at most one version per failed or crashed write; no maintenance job is provided |
| A writer paused beyond its write-announcement lease | Its stored value can land after the key was purged and leak if the writer also stops before cleaning up | Accepted residual (see Accepted behavior), kept small by the deployment requirement that the lease comfortably exceed the backend's maximum request duration; safety does not depend on the lease; closing it needs a conditional write in the backend contract, which is not added |
| Writer outlives a record delete | A writer that read a record before its deletion and stored its value after the purge leaves a version under the deleted key if it crashes before its commit | Tracked by its open announcement and healed like a failed create by the next create or read of the reference; if nobody asks for the reference again it stays (accepted) |
| Expired record lingers in the catalogue | An expired credential keeps its record visible (status `expired`) while its secret is no longer served, and no sweep removes it; a consumer that does not handle `SECRET_EXPIRED` fails closed on it | The owner renews it in place by a replace or partial update (same id; the validator the metadata read returns, or `If-Match: *`); a create-only write over it is refused (409 `ALREADY_EXISTS`); the owner can also delete it directly; consumers treat `SECRET_EXPIRED` as an unusable secret and never fall back to another credential |
| PDP or tenant-resolver outage | Operations fail closed (unavailable) | Fail closed (503); dependency metrics for fast diagnosis |
| In-memory static plugin in non-dev use | Secrets lost on restart | Production backend (`vault-credstore-plugin`, `cpt-cf-credstore-fr-production-backend`); deployment policy |
| Metadata database loss | The metadata index is not rebuildable from the store (the backend offers no listing), so stored versions would be unreachable | The database is the system of record for metadata and is backed up together with the store; index rebuild is out of scope |
| Backend without ordered versions | Destroying older versions would be unsafe | Ordered versions are required of any backend that supports destroy; a backend without them (AWS, Azure) runs without destroy and keeps superseded versions until the record is deleted |
| Type-trait misconfiguration | Overly permissive or broken writes for a type | Compiled-in catalog pinned to registered GTS schemas by unit tests; catalog changes are code-reviewed SDK releases; `generic` is the unrestricted type, never an implicit default |

## 13. Open Questions

- ~~**Batch retrieval**: should a read support multiple references per call?~~ **Answered** by [ADR-0004](./ADR/0004-cpt-cf-credstore-adr-secret-value-exposure.md): the paginated credential listing returns secrets when `secret` is selected, authorized per item (`cpt-cf-credstore-fr-bulk-read-secrets`).
- ~~**P2/Future — Human vs service access**: should human users be restricted to metadata-only?~~ **Answered** by [ADR-0004](./ADR/0004-cpt-cf-credstore-adr-secret-value-exposure.md) and `cpt-cf-credstore-fr-authz-action-split`: the restriction is expressed by granting metadata actions without the read-secret action, and applies to any principal kind rather than being derived from whether the subject is human.
- ~~**Audit trails**~~ **Answered** by `cpt-cf-credstore-nfr-audit`: every secret read and write publishes an audit event through the `event-broker` gear, best-effort and never blocking the operation.
- ~~**P2/Future — Metadata list endpoint**~~ **Answered** by [ADR-0004](./ADR/0004-cpt-cf-credstore-adr-secret-value-exposure.md), which fixes that a listing exists (`cpt-cf-credstore-fr-list-credentials`) and never carries a secret unless the caller explicitly asks for one — in which case the same paginated listing returns the secrets of its page (`cpt-cf-credstore-fr-bulk-read-secrets`) — and by [ADR-0005](./ADR/0005-cpt-cf-credstore-adr-upward-collection-read.md), which is the dedicated ADR this entry asked for: the listing is rooted at the caller's tenant and resolves upward through the hierarchy, checking the requesting tenant rather than filtering records individually, which is what lets an inherited entry appear at all. Both are `accepted`; the answer stands, pending their implementation.
- ~~**One item per reference**~~ **Answered** by [ADR-0005](./ADR/0005-cpt-cf-credstore-adr-upward-collection-read.md) "Pagination over a reduced result": because the canonical order leads with reference, every record for one reference is contiguous, so a cursor always sits on a reference boundary and no reference can be split across a page. **Still open**: this is the platform's first pagination that reduces records before paging, so its page-boundary behaviour needs its own test suite before the endpoint ships.

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/) — [ADR-0001 stateful gear](./ADR/0001-cpt-cf-credstore-adr-stateful-gear.md), [ADR-0002 deprovisioning saga](./ADR/0002-cpt-cf-credstore-adr-deprovisioning-saga.md) (superseded by ADR-0006), [ADR-0003 value-fingerprint fence](./ADR/0003-cpt-cf-credstore-adr-value-fingerprint-fence.md) (superseded by ADR-0006), [ADR-0004 credential: metadata with a selectable secret](./ADR/0004-cpt-cf-credstore-adr-secret-value-exposure.md), [ADR-0005 upward-rooted collection read](./ADR/0005-cpt-cf-credstore-adr-upward-collection-read.md), [ADR-0006 immutable value versions](./ADR/0006-cpt-cf-credstore-adr-immutable-value-versions.md), [ADR-0007 two write verbs on one address: PUT replaces, PATCH merges](./ADR/0007-cpt-cf-credstore-adr-record-write-verbs.md), [ADR-0008 suppression: fallback on the tenant's own record](./ADR/0008-cpt-cf-credstore-adr-suppression-fallback.md), [ADR-0009 an inherited entry discloses nothing about the ancestor](./ADR/0009-cpt-cf-credstore-adr-no-ancestor-disclosure.md), [ADR-0010 six actions on the credential type; type and reference are the scope axes](./ADR/0010-cpt-cf-credstore-adr-type-scoped-authorization.md)
- **Features**: features/ (planned)
