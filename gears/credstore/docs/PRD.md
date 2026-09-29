Updated:  2026-09-29 by Virtuozzo International GmbH

# PRD — CredStore

> **Status: target contract, not implemented.** This PRD specifies the Vault-backed CredStore. The gear shipped on main still follows the design of ADR-0001..0003 until the cutover (§14). Priority `p1` is the first delivery (P0 scope); `p2` follows later.


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
  - [5.1 P1 — Credential Records and Secrets](#51-p1--credential-records-and-secrets)
  - [5.2 P1 — Hierarchical Resolution](#52-p1--hierarchical-resolution)
  - [5.3 P1 — Authorization](#53-p1--authorization)
  - [5.4 P1 — Storage, Consistency and Concurrency](#54-p1--storage-consistency-and-concurrency)
  - [5.5 P1 — Credential Types](#55-p1--credential-types)
  - [5.6 P1 — Consumer Surface](#56-p1--consumer-surface)
  - [5.7 P2 — Later](#57-p2--later)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Gear-Specific NFRs](#61-gear-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
  - [8.1 Publishing and Consuming](#81-publishing-and-consuming)
  - [8.2 Administration Without Plaintext](#82-administration-without-plaintext)
  - [8.3 Inheritance Control](#83-inheritance-control)
  - [8.4 Operations](#84-operations)
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
  - Keep transport/mechanism detail (endpoints, status codes, headers,
    reason codes, storage layout, schemas) out of this doc — it lives in
    DESIGN.md.
=============================================================================
-->

## 1. Overview

### 1.1 Purpose

CredStore is the platform's hierarchical, tenant-scoped credential store. Platform gears and tenant administrators use it to keep API keys, tokens, passwords and certificates, to share them down the tenant hierarchy, and to read them back under the platform's authorization model.

CredStore is a thin policy layer in front of a Vault-compatible secret store (HashiCorp Vault or OpenBao, KV version 2). The secret store is the only system of record: it holds every credential — its secret together with every field that decides who can see it — and it provides durability, versioning, atomic conditional writes and encryption at rest. CredStore adds only what the secret store does not have: tenant hierarchy semantics (inheritance, override, suppression, descendant blocking), per-type authorization through the platform PDP, credential-type traits, anti-enumeration, per-secret audit, and a paginated catalogue of what a tenant can see. The gear reaches the secret store through a narrow internal adapter that a pluggable backend can replace later without changing consumers.

CredStore also keeps a small database, used only as a derived index that makes listing and hierarchical resolution cheap. The index holds no secret, never decides on its own what is served, and can be dropped and rebuilt from the secret store without losing any credential.

### 1.2 Background / Problem Statement

Platform gears — most notably the Outbound API Gateway (OAGW) — need secrets to call upstream services on behalf of tenants. In the platform's business model a parent tenant (a partner) shares credentials with its descendants (customers): the partner's OpenAI key is used by OAGW for the partner's customers, and no customer ever sees it. Resolving a credential therefore walks up the tenant tree, and every tenant on the way can override the inherited credential, suppress it, or keep it from its own descendants.

The shipped CredStore stores all credential metadata in its own database and treats the backend as a pure value store. That split forces the gear to reimplement what a secret store already provides: write and delete sagas, a resident reaper, a value fingerprint to detect divergence between metadata and value, and a garbage-collection job for orphaned values. It still leaves failure windows in which the two halves disagree, and it makes the gear's database a second copy of security-relevant state that must be backed up consistently with the secret store.

PRs #4737 (documentation) and #4741 (implementation) reshaped the consumer surface — a credential record with a selectable secret, a catalogue listing, six authorization actions, suppression and an explicit inheritance status — but kept the split storage. This PRD keeps that consumer surface and delegates storage, versioning and transactionality to the secret store, reducing the gear to authorization, hierarchy and translation.

**Why one store family.** Every production deployment of the platform runs a Vault-compatible store, and HashiCorp Vault and OpenBao share one API, so supporting that family costs one adapter. A provider-neutral abstraction over cloud secret managers would force the gear to reimplement versioning and conditional writes for stores that lack them — the very machinery this PRD removes. The adapter boundary keeps a later pluggable backend possible.

**Why one store identity.** The gear must read any tenant's credentials to resolve inheritance, so per-tenant store policies would all be held by the same process and would not contain a compromise of the gear. They would only guard against a malformed key path, which reference validation already excludes, at the cost of a store role per tenant. Tenant isolation therefore lives in the gear, and the store identity is scoped to the installation prefix.

### 1.3 Goals (Business Outcomes)

- **Same consumer surface as #4737/#4741**: every consumer written against it works unchanged apart from the differences enumerated in `cpt-cf-credstore-fr-consumer-compat`.
- **One system of record**: zero credentials exist only in the gear's database; dropping and rebuilding the database changes no listing and no resolution result.
- **No correctness-critical background work**: no reaper, saga recovery or maintenance job is required for any guarantee in this PRD.
- **Least privilege end to end**: a caller holding only metadata actions never receives a secret; the gear's own access to the secret store is one identity limited to the gear's installation prefix.
- **Hierarchy control**: a tenant can use, override, suppress, or withhold from its descendants any credential it inherits, without touching the ancestor's credential.
- **Attributable disclosure**: 100% of secrets returned produce an audit event naming the platform subject.

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Credential | A tenant-scoped, named record that may hold a secret; the unit of storage, sharing, inheritance and authorization |
| Record | A credential's reference, type, sharing mode, fallback policy, descendant block, expiry and lifecycle status, independent of whether it holds a secret |
| Secret | The sensitive payload a credential holds; text, or bytes when the credential type allows binary |
| Reference | The caller-chosen name of a credential within a tenant, e.g. `partner-openai-key`; format `[a-zA-Z0-9_-]+`, 1–255 characters |
| Owner | The subject (`subject_id` of the SecurityContext) that created a private record |
| Sharing mode | Who can see a record: `private` (its owner in its tenant), `tenant` (every subject of its tenant, the default), `shared` (its tenant and all descendants) |
| Sharing class | The private class (one record per owner and reference) or the non-private class (one `tenant` or `shared` record per tenant and reference); the two classes coexist under one reference |
| Lifecycle status | State of the caller's own record: `none` (no own record), `declared` (a record without a secret), `active` (a record with a secret) |
| Fallback policy | What a record without a secret means for resolution: `inherit` (continue up the chain, the default) or `none` (resolve as absent) |
| Resolution | Finding, for a reference and a requesting subject, the closest decisive record on the requesting tenant's ancestor chain |
| Decisive record | A record that ends resolution: one holding a secret, a declared record with fallback `none`, or a descendant block |
| Override | A tenant's own record holding a secret for a reference that would otherwise resolve to an ancestor's shared credential |
| Suppression | A declared record with fallback `none` that makes the reference resolve as absent instead of inheriting |
| Descendant block | A mark on a tenant's own non-private record that makes the reference resolve as absent for the tenant's descendants, independently of what the tenant itself resolves |
| Inheritance status | Relationship of the resolved result to the chain: `own`, `inherited`, `overridden`, `suppressed` |
| Expired record | A record of an expirable type whose expiry has passed; treated as absent by every read and replaced or removed by the next write or delete that addresses it |
| Field selection | The caller's choice of which record fields, and whether the secret, a read returns |
| Secret mode | A bounded bulk read of several credentials' secrets in one request, scoped by an explicit set of references or by type, never paginated |
| Credential type | A GTS type derived from `gts.cf.core.credstore.credential.v1~` that classifies a credential and carries enforceable traits; `generic` by default; immutable per credential |
| Version | The store-assigned, monotonic revision number of an own record; never reused for a reference, including after delete and re-create |
| Validator | The opaque value a caller echoes in a write precondition; for an inherited result it is opaque and not usable as a precondition |
| Secret store | The Vault-compatible KV version 2 service that is the system of record for credentials |
| Store adapter | The gear's narrow internal boundary to the secret store; replaceable by a pluggable backend later |
| Credential index | The gear's derived, rebuildable database of record fields used for listing and resolution; holds no secret |
| Installation prefix | The part of the secret store's key space reserved for one CredStore installation |
| PDP | The platform policy decision point, `authz-resolver` |
| SecurityContext | The request security context carrying the authenticated tenant, subject and claims |

## 2. Actors

### 2.1 Human Actors

#### Tenant Admin

**ID**: `cpt-cf-credstore-actor-tenant-admin`

- **Role**: Authenticated user with full control over the credentials of their tenant: creates, replaces, rotates and deletes them, sets their sharing mode, and decides what the tenant inherits and what its descendants receive.
- **Needs**: The complete lifecycle of the tenant's own credentials under precondition control; control over inheritance in both directions.

#### Integration Administrator

**ID**: `cpt-cf-credstore-actor-integrations-admin`

- **Role**: Configures a tenant's integrations (SMTP, provider keys, webhooks): creates and rotates credentials, retargets and disables them, and reads the catalogue.
- **Needs**: The catalogue and record metadata; creating and replacing records; editing metadata and rotating secrets under precondition control; suppressing inherited credentials. Never needs the plaintext of the credentials being managed.

#### Catalogue Auditor

**ID**: `cpt-cf-credstore-actor-catalogue-auditor`

- **Role**: Reviews what a tenant has configured — which credentials exist, their types, whether each is own or inherited, when each expires — for compliance, support or migration planning. Changes nothing and reads no secret.
- **Needs**: The catalogue and each record's metadata; nothing else.

#### Platform Operator

**ID**: `cpt-cf-credstore-actor-platform-operator`

- **Role**: Operates the installation: provisions the secret store mount and the gear's store identity, monitors the gear, and rebuilds the credential index after a database loss.
- **Needs**: An index rebuild that is safe to run and re-run while the gear serves traffic; metrics that show store-side failures.

### 2.2 System Actors

#### Outbound API Gateway (OAGW)

**ID**: `cpt-cf-credstore-actor-oagw`

- **Role**: Proxies outbound API calls to external services and retrieves the secret for each call on behalf of the target tenant by constructing a SecurityContext for it. Primary consumer of hierarchical resolution; sits on the request hot path.

#### Integration Application

**ID**: `cpt-cf-credstore-actor-integration-app`

- **Role**: A platform service (mail sender, billing connector) that reads the secrets of the credentials of its own credential type, one by one or as its whole set, in the tenant it acts for. Never enumerates the catalogue.

#### Self-Rotating Application

**ID**: `cpt-cf-credstore-actor-self-rotating-app`

- **Role**: A service that consumes and renews its own credential — refreshing an OAuth token, rotating an API key with its provider — and stores the new secret back under the validator returned together with the secret, holding only `read_secret` and `write_secret`.

#### Provisioning Injector

**ID**: `cpt-cf-credstore-actor-provisioner`

- **Role**: A pipeline or synchronization job (CI/CD, a sync from an external vault) that places secrets into records someone else declared and rotates them on schedule, under a guarded or last-writer-wins precondition, holding only `write_secret`. Sees no secret it did not supply; repairs a suspect secret by writing it again.

#### Platform Gear

**ID**: `cpt-cf-credstore-actor-platform-gear`

- **Role**: Any internal gear using the in-process client — today OAGW, settings-service and the Keycloak IdP plugin of account-management. Acts under the calling tenant's SecurityContext or under a system SecurityContext it constructs.

#### Vault-Compatible Secret Store

**ID**: `cpt-cf-credstore-actor-backend`

- **Role**: HashiCorp Vault or OpenBao with a KV version 2 mount, operated by the platform. System of record for every credential. Authorizes only the gear's own identity, never tenants or end users, and holds no platform policy. Reached exclusively through the gear.

#### Platform Policy & Directory Services

**ID**: `cpt-cf-credstore-actor-platform-services`

- **Role**: `authz-resolver` evaluates per-operation access scopes; `tenant-resolver` supplies ancestor chains; `types-registry` holds the credential base type, the derived credential types and their traits; account-management owns the tenant lifecycle.

## 3. Operational Concept & Environment

> **Note**: Project-wide runtime, architecture and integration patterns are defined in [ARCHITECTURE_MANIFEST.md](../../../docs/ARCHITECTURE_MANIFEST.md) and the foundational [guidelines](../../../guidelines/README.md), in particular [SECURITY.md](../../../guidelines/SECURITY.md), [GTS.md](../../../guidelines/GTS.md) and the REST conventions in [QUERYING.md](../../../guidelines/DNA/REST/QUERYING.md). The tenant model is [TENANT_MODEL.md](../../../docs/arch/authorization/TENANT_MODEL.md). Only gear-specific constraints are listed here.

### 3.1 Gear-Specific Environment Constraints

- Every deployment that stores real credentials **MUST** provide a Vault-compatible secret store with a KV version 2 mount usable by the gear; there is no other production backend in this PRD.
- The gear authenticates to the secret store as **one platform identity** for the whole installation, obtained from the runtime (a Kubernetes service-account role or an AppRole). Tenants and end users never hold store credentials, and the store holds no per-tenant policy.
- Several installations **MUST** be able to share one secret store, each confined to its own installation prefix.
- The gear requires a database (PostgreSQL or SQLite) for the credential index only.
- The gear depends on `authz-resolver`, `tenant-resolver` and `types-registry` and initializes at system priority; its consumers resolve the client during their own initialization.
- No resident background task and no scheduled job is required for correctness; the only background work is index repair and the operator-triggered index rebuild.
- Development and automated tests run against an OpenBao instance or an in-process store adapter with the same observable behaviour; preventing the in-process adapter from being selected in production is `cpt-cf-credstore-fr-store-config-check` (p2).

## 4. Scope

### 4.1 In Scope

- The consumer surface of #4737/#4741 (in-process client and REST): record with a selectable secret, field selection, catalogue listing with filtering and ordering, secret mode, full replace and merge-patch partial update at one address, guarded delete, `get_secret`
- Sharing modes `private`, `tenant`, `shared`; private and non-private records coexisting under one reference
- Hierarchical resolution up the tenant ancestry, across isolation barriers, with override, suppression, descendant blocking and inheritance status, without disclosing ancestors
- Six PDP actions on the concrete credential type `gts.cf.core.credstore.credential.v1~` and its derived types
- Credential types with enforceable traits (allowed sharing modes, secret schema, size and encoding bounds, expiry), including binary secrets through the in-process client
- Expired records filtered on every read and healed by the next write or delete
- Optimistic concurrency with mandatory preconditions on every write and delete, and versions that never repeat
- A Vault-compatible KV version 2 secret store as the only system of record, behind a narrow internal store adapter; the database as a derived, rebuildable index
- Service-to-service retrieval on behalf of arbitrary tenants (OAGW pattern)
- Per-secret audit; operational metrics; secret confidentiality
- Later (p2): startup check of the store configuration, no retention of previous secret versions in the store, audit correlation with the store, tenant offboarding

### 4.2 Out of Scope

- Secret history, rollback or reading a previous version; the version serves concurrency only
- Automatic rotation; type-level rotation traits are advisory only
- Transfer or re-ownership of credentials between tenants or owners
- Unauthenticated access; every operation requires a platform SecurityContext
- Full-text search over references or secrets
- Secrets in the catalogue listing unless the caller explicitly selects secret mode
- Per-credential ACLs naming specific tenants, and sharing outside the tenant hierarchy
- Downward listing: a parent reads a descendant's catalogue only by acting in the descendant's context
- Vault namespaces, per-tenant store policies or per-tenant store identities
- Pluggable backends, backend selection by vendor, and a public backend SPI (the store adapter keeps this possible later)
- Any maintenance job or host-invoked maintenance operation
- A separate create-only address on the collection; creation is a guarded full replace
- Direct use of the secret store by any component other than this gear; edits made directly in the store are unsupported
- Migration of values stored by the shipped in-memory backend (it keeps nothing across restarts)
- MySQL as the index database

## 5. Functional Requirements

> **Testing strategy**: every requirement is verified by automated tests — unit tests for resolution and authorization rules, and integration tests against an OpenBao instance for storage, concurrency and failure behaviour — unless a requirement states otherwise.

### 5.1 P1 — Credential Records and Secrets

#### Credential Record and Secret

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-credential-record`

The system **MUST** treat a credential's record and its secret as one entity and **MUST** default every read of it to the record alone: reference, type, sharing mode, expiry, lifecycle status of the caller's own record, inheritance status, and — only when the caller's tenant holds its own record — that record's fallback policy, descendant block, version, last-update time and creating subject. The secret **MUST** be returned only when the caller explicitly asks for it and holds `read_secret`, as part of the same credential; it **MUST NOT** be addressable as a separate resource. The record **MUST NOT** name the owning tenant (see `cpt-cf-credstore-fr-no-ancestor-disclosure`).

- **Rationale**: A metadata surface cannot leak a secret it structurally does not contain; this is what makes a secret-blind administrator possible.
- **Actors**: `cpt-cf-credstore-actor-integrations-admin`, `cpt-cf-credstore-actor-catalogue-auditor`, `cpt-cf-credstore-actor-platform-gear`

#### Get Credential

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-get-credential`

The system **MUST** allow an authorized caller to read one credential by reference, resolved through the hierarchy, in the same representation the listing uses. The caller **MUST** be able to select fields from the credential field allowlist plus the secret; an unknown field **MUST** be a validation error; with no selection the result **MUST** be the full record without the secret. The result **MUST** carry the validator of the caller's own record whenever the caller's tenant holds one — declared or active, even while the effective secret is inherited — regardless of the selection. A reference that does not resolve and one the caller may not read **MUST** produce the same not-found result.

- **Rationale**: A secret-blind writer still needs a validator to edit safely; one representation for the point read and the listing makes field selection behave identically on both.
- **Actors**: `cpt-cf-credstore-actor-integrations-admin`, `cpt-cf-credstore-actor-platform-gear`

#### List Credentials

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-list-credentials`

The system **MUST** allow an authorized caller to list the credentials visible to its tenant — exactly one resolved item per reference, the same item a point read returns — and **MUST NOT** include a secret unless the caller selects secret mode (`cpt-cf-credstore-fr-bulk-read-secrets`). The listing **MUST** follow the platform cursor-pagination contract: bounded pages, an opaque cursor, field selection from the credential field allowlist, filtering and ordering only on an allowlisted set of fields, and no total count. A filter on reference or type **MUST** only narrow which references are considered: each candidate reference **MUST** still be resolved over its whole chain, and the resolved item **MUST** itself match the filter to be listed, so a filtered listing never reports an item a point read would not return. Filters on sharing mode, expiry and fallback policy **MUST** apply to the resolved item. Declared records **MUST** appear with their lifecycle status. A page boundary **MUST NOT** split the records of one reference. Listing **MUST** require the `list` action.

- **Rationale**: Administrators and auditors need a catalogue that matches what point reads return, without it becoming a secret-disclosure or enumeration primitive.
- **Actors**: `cpt-cf-credstore-actor-integrations-admin`, `cpt-cf-credstore-actor-catalogue-auditor`

#### Write Credential Record

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-write-credential-record`

The system **MUST** accept two write forms at a credential's address:

- a **full replace** that creates or replaces the record and its secret together, atomically, under a create-only, guarded-replace or last-writer-wins precondition. The caller **MUST** either supply a secret or explicitly state that there is none; omitting the secret **MUST** be a validation error. Stating none **MUST** create a declared record, remove the secret of an active record being replaced, or leave an already declared record without one. Optional fields absent from a full replace **MUST** reset to their defaults (an omitted expiry clears a stored one). The full replace is the only way to create a record;
- a **partial update** with JSON merge-patch semantics (RFC 7396): supplied fields replace, absent fields stay untouched, an explicit null secret removes the secret and keeps the rest of the record. A request not declared as a merge patch **MUST** be rejected. A patch that supplies nothing **MUST** be a validation error, and so **MUST** a null for type, sharing mode or fallback policy. A patch that carries no secret and whose supplied fields equal the stored record **MUST** succeed without changing the version. A partial update requires a guarded or last-writer-wins precondition and **MUST NOT** create a record — a reference with no own record of the caller's class is not-found.

The credential type **MUST** be required on create and **MUST** be immutable under both forms.

- **Rationale**: A secret-blind administrator edits metadata through a partial update that never carries a secret, and an injector rotates a secret through a partial update that carries nothing else; the same address creates a record with or without a secret in one request, which is what lets a tenant suppress an inherited credential without ever holding a secret.
- **Actors**: `cpt-cf-credstore-actor-integrations-admin`, `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-provisioner`

#### Sharing Classes on Write

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-sharing-classes`

A write of a `tenant` or `shared` record **MUST** address the tenant's single non-private record of the reference; a write of a `private` record **MUST** address the caller's own private record of the reference. Several owners **MUST** be able to hold private records under one reference next to one non-private record, and a write of one class **MUST NOT** affect the other. Changing a record between `tenant` and `shared` **MUST** be an in-place update; changing it between `private` and a non-private mode **MUST** be rejected as an unsupported transition.

- **Rationale**: Personal and team credentials under a common name stay independent; a class change has no atomic meaning because the two classes coexist.
- **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-platform-gear`

#### Write a Secret

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-write-secret`

The system **MUST** write a secret only as part of writing its record and **MUST NOT** grant reading a secret as a side effect of granting writing it. `write_secret` **MUST** be required when a request writes a secret or removes an existing one; it **MUST NOT** be required when a full replace creates a declared record or restates the absence of a secret on an already declared record. A partial update carrying an explicit secret — a value or null — **MUST** require `write_secret` regardless of the record's prior state. A request that also carries record fields **MUST** additionally require `write`; a partial update carrying only a secret **MUST** need `write_secret` alone. Every write that supplies or removes a secret **MUST** produce a new version, even when the supplied secret equals the stored one. A tenant **MUST NOT** write the secret of an ancestor's record; it creates its own record instead.

- **Rationale**: This is what makes the secret-blind configurator and the provisioning injector possible, and it avoids turning an equality check into an oracle for callers who may write but not read.
- **Actors**: `cpt-cf-credstore-actor-integrations-admin`, `cpt-cf-credstore-actor-provisioner`, `cpt-cf-credstore-actor-self-rotating-app`

#### Read a Secret

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-read-secret`

The system **MUST** allow an authorized caller to read the secret of a credential resolved through the hierarchy, for one credential or in secret mode. Reading a secret **MUST** require `read_secret` independently of any record fields selected alongside it; a caller selecting only the secret and its usage envelope (reference, type, expiry) **MUST** need `read_secret` alone. Every credential response **MUST** be marked non-cacheable — not only those carrying a secret, because metadata also varies by tenant and subject. Every secret returned **MUST** produce an audit event (`cpt-cf-credstore-nfr-audit`).

- **Rationale**: Secret disclosure is its own privilege with its own auditable path, separate from reading or listing metadata.
- **Actors**: `cpt-cf-credstore-actor-integration-app`, `cpt-cf-credstore-actor-oagw`, `cpt-cf-credstore-actor-platform-gear`

#### Secret for Use

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-get-secret`

The in-process client **MUST** offer a `get_secret` operation that returns the secret with its usage envelope — reference, type, expiry — and the validator of the resolved record, and none of the administrative metadata (sharing mode, lifecycle status, inheritance status). It **MUST** need `read_secret` alone. A reference that does not resolve to a secret, or that the caller may not read, **MUST** return an empty result rather than an error. The validator of an inherited result is opaque and not usable as a precondition (`cpt-cf-credstore-fr-no-ancestor-disclosure`).

- **Rationale**: Runtime consumers such as OAGW need exactly the secret and what is needed to use it, through the smallest grant; a self-rotating application needs the validator with the secret so it can write back without also holding `read`.
- **Actors**: `cpt-cf-credstore-actor-oagw`, `cpt-cf-credstore-actor-integration-app`, `cpt-cf-credstore-actor-self-rotating-app`

#### Bulk Read Secrets (Secret Mode)

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-bulk-read-secrets`

The system **MUST** allow an authorized caller to read the secrets of several credentials in one request through the listing. Secret mode **MUST** be scoped by exactly one selector: an explicit set of references, or one or more concrete credential types. It **MUST NOT** accept ordering, pagination, a cursor, or any other selector. Each item **MUST** be resolved and authorized as a point read would be, so the result never exceeds what the caller could read one by one. A refused or non-resolving item **MUST** be omitted, never reported. The result **MUST** be bounded by a configured cap; a selector matching more **MUST** fail the whole request rather than truncate it. Record fields selected alongside the secret **MUST** require `list`.

- **Rationale**: Applications need their whole credential set in one round-trip; disclosure stays bounded by the caller's own grant, a hard cap and the absence of pagination.
- **Actors**: `cpt-cf-credstore-actor-integration-app`

#### Delete Credential

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-delete-secret`

The system **MUST** allow a tenant to delete its own record by reference under a guarded or last-writer-wins precondition; for the private class the delete targets the caller's own private record only. The credential **MUST** stop resolving at once for the tenant and for every descendant that inherited it, which then resolve the next decisive record up the chain. The reference **MUST** be reusable immediately after the delete returns. Deleting a reference with no own record **MUST** be not-found. A tenant that wants to stop using an inherited credential without deleting anything uses suppression (`cpt-cf-credstore-fr-suppression`).

- **Rationale**: Revocation must be immediate and must not leave a name-retention window.
- **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-integrations-admin`

#### Tenant Scoping

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-tenant-scoping`

The system **MUST** derive the operating tenant from the SecurityContext (`subject_tenant_id`) and the owner from `subject_id` for every operation. A caller **MUST NOT** create, change or delete a record of another tenant, including an ancestor's record it inherits. If the caller's authorized scope does not include its own tenant, the operation **MUST** be denied before any side effect and the denial counted.

- **Rationale**: Prevents cross-tenant manipulation; fail-closed before any side effect.
- **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-platform-gear`

#### Reference Validation

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-secretref-validation`

The system **MUST** accept only references matching `[a-zA-Z0-9_-]+`, 1–255 characters, and reject any other value as a validation error before touching the secret store or the index.

- **Rationale**: A restricted alphabet keeps references safe as URL path segments and store keys, and makes addressing another tenant's record through a crafted reference impossible.
- **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-platform-gear`

### 5.2 P1 — Hierarchical Resolution

#### Sharing Modes

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-sharing-modes`

Each record **MUST** have one sharing mode:

- `private` — visible only to its owner, in its own tenant; never inherited; never hides another record from any other subject;
- `tenant` (default) — visible to every subject of its tenant; never inherited — descendants resolve past it as if it did not exist;
- `shared` — visible to its tenant and to all descendants.

- **Rationale**: Personal keys are owner-only, team credentials tenant-wide, partner credentials hierarchical.
- **Actors**: `cpt-cf-credstore-actor-tenant-admin`

#### Hierarchical Resolution

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-hierarchical-resolve`

The system **MUST** resolve a reference against the requesting tenant's ancestor chain, from the requesting tenant to the root, and stop at the first decisive record according to the table below. If no record is decisive, the reference is not found. Resolution **MUST** be upward-only: a tenant never sees a descendant's record. A `shared` record **MUST** be inherited across `self_managed` isolation barriers — publishing as `shared` is the owner's explicit decision, and whether a caller may read at all remains the PDP's decision. An expired record **MUST** be treated as if it did not exist (`cpt-cf-credstore-fr-expired-records`).

| Where the record is | Record | Outcome |
|---|---|---|
| Requesting tenant, caller's own private record | holds a secret | serve it; checked before the tenant's non-private record |
| Requesting tenant, caller's own private record | declared, fallback `none` | not found |
| Requesting tenant, caller's own private record | declared, fallback `inherit` | continue with the tenant's non-private record |
| Requesting tenant, another owner's private record | any | ignored |
| Requesting tenant, non-private record | holds a secret | serve it |
| Requesting tenant, non-private record | declared, fallback `none` | not found |
| Requesting tenant, non-private record | declared, fallback `inherit` | continue to the parent |
| Ancestor, non-private record with a descendant block | any | not found for the whole subtree |
| Ancestor, `shared` record | holds a secret | serve it |
| Ancestor, `shared` record | declared, fallback `none` | not found for the whole subtree |
| Ancestor, `shared` record | declared, fallback `inherit` | continue to the next ancestor |
| Ancestor, `tenant` or `private` record without a descendant block | any | ignored |

A descendant block **MUST NOT** affect resolution at the tenant that holds it.

- **Rationale**: The core business case — OAGW uses a partner's key for a customer — including for customers that manage their own sub-hierarchy; one table governs the point read, the listing and secret mode alike.
- **Actors**: `cpt-cf-credstore-actor-oagw`, `cpt-cf-credstore-actor-integration-app`

#### Override

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-secret-shadowing`

A tenant's own record holding a secret **MUST** take precedence over every ancestor's record of the same reference — for the tenant itself and, when the record is `shared`, for its descendants. A record the requester cannot see (another owner's private record, an ancestor's `tenant` record) **MUST NOT** hide an ancestor's record. Deleting the override, or removing its secret while its fallback is `inherit`, **MUST** restore inheritance immediately.

- **Rationale**: Customers override partner defaults with their own credentials while keeping fallback when the local record is not theirs.
- **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-oagw`

#### Suppression

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-suppression`

Each record **MUST** carry a fallback policy, `inherit` (default) or `none`, settable with `write` alone. A declared record with fallback `none` **MUST** make the reference resolve as absent in its tenant and, when `shared`, in all descendants, leaving the ancestor's credential untouched. A record holding a secret **MUST** serve it regardless of the policy, and the policy **MUST** persist so that removing the secret later applies it. A tenant **MUST** be able to suppress an inherited credential in one request: with an active own record, by one partial update that sets fallback `none` and removes the secret, with no moment at which the ancestor's secret is served; with no own record, by one create-only full replace of a declared record with fallback `none`, needing only `write`.

- **Rationale**: A descendant opts out of an inherited credential without touching the ancestor's; the same policy lets a tenant fail closed while it is still setting up its own.
- **Actors**: `cpt-cf-credstore-actor-integrations-admin`

#### Descendant Block

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-descendant-block`

A tenant **MUST** be able to mark its own non-private record so that the reference resolves as absent for all its descendants, independently of what the tenant itself resolves. This **MUST** cover the two combinations sharing mode and fallback policy cannot express:

- **use the ancestor's credential, pass nothing down** — a declared record with fallback `inherit` and a block: the tenant resolves the ancestor's secret; its descendants resolve nothing;
- **own secret for me, nothing for my descendants** — a record holding a secret with a block: the tenant resolves its own secret; its descendants resolve neither that secret nor the ancestor's.

A descendant **MUST** still be able to hold its own record under a block: its own record wins for itself and, per its own sharing mode, for its subtree; deleting it returns the descendant to the block, not to the ancestor's credential. The block **MUST** apply across isolation barriers, **MUST** be settable and removable with `write` alone, and **MUST** be reported to descendants as `suppressed` without naming the blocking tenant. The block **MUST** be an addition to the record: sharing mode and fallback policy keep their meaning, and a record without a block resolves exactly as it would without this requirement. Preventing a descendant from creating its own record is a PDP grant decision, not a record field.

- **Rationale**: Partners reselling to sub-partners need to consume a credential without passing it on, or to replace it for themselves without exposing either key to their customers.
- **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-integrations-admin`

#### Inheritance Status

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-inheritance-status`

Every credential representation **MUST** carry an inheritance status describing the resolved result:

- `own` — the decisive record is the caller's tenant's (or the caller's private) record and no ancestor record of the reference is visible;
- `inherited` — the decisive record is an ancestor's;
- `overridden` — the decisive record is the caller's own and takes precedence over an ancestor's `shared` record of the reference (with or without a secret) or over a descendant block;
- `suppressed` — the decisive record makes the reference resolve as absent, whether it is the caller's own or an ancestor's.

The status **MUST** be readable with metadata actions alone and **MUST NOT** be stored.

- **Rationale**: An administrator tells "mine" from "inherited" from "blocked" from the catalogue alone, without reading a secret.
- **Actors**: `cpt-cf-credstore-actor-integrations-admin`, `cpt-cf-credstore-actor-catalogue-auditor`

#### No Ancestor Disclosure

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-no-ancestor-disclosure`

An inherited or suppressed result **MUST NOT** reveal which ancestor supplied the credential or the suppression, nor that ancestor's tenant identifier, creating subject, fallback policy, descendant block, version or last-update time. Its validator **MUST** be opaque, **MUST** change when the decisive ancestor record changes, and **MUST** be rejected as a write precondition. Responses and errors **MUST NOT** carry secret-store paths, identifiers of other tenants, or secret-store error text.

- **Rationale**: A descendant must not learn the shape of its ancestry or its partner's internal identifiers through the credential surface; store errors embed exactly those identifiers.
- **Actors**: `cpt-cf-credstore-actor-integrations-admin`, `cpt-cf-credstore-actor-oagw`

#### Override Type Consistency

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-override-type-consistency`

When a tenant creates a record for a reference that currently resolves to an ancestor's `shared` credential holding a secret, the new record **MUST** carry the same credential type, and a different type **MUST** be rejected as a conflict. The rule applies at creation only; a reference that resolves to nothing — including one resolving to nothing because of an ancestor's suppression or descendant block — accepts any registered type.

- **Rationale**: The type is the contract between a credential and the application that reads it by reference; a tenant must not break that contract by shadowing with an incompatible type.
- **Actors**: `cpt-cf-credstore-actor-integrations-admin`, `cpt-cf-credstore-actor-integration-app`

#### Service-to-Service Retrieval

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-service-retrieve`

The system **MUST** serve retrieval on behalf of an arbitrary tenant through the ordinary read: an authorized service constructs a SecurityContext for the target tenant, and the PDP decides whether that subject may read in that scope. There **MUST NOT** be a separate service-to-service operation.

- **Rationale**: OAGW and other gears need hierarchical retrieval for arbitrary tenants through the same audited, policy-checked path.
- **Actors**: `cpt-cf-credstore-actor-oagw`, `cpt-cf-credstore-actor-platform-gear`

### 5.3 P1 — Authorization

#### PDP-Based Authorization

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-authz-pdp`

Every operation **MUST** be authorized by the PDP against the credential's full concrete type, including `generic`, before any side effect and before any secret is read from the store. The type **MUST** be known before the decision: from the request on create, and from the resolved or existing record otherwise. Enforcement **MUST** be fail-closed: a denial on a read **MUST** be indistinguishable from not-found, a denial on a write or delete **MUST** refuse the operation, and a PDP failure **MUST** surface as unavailable.

- **Rationale**: Per-type policies (read `api-key` but not `certificate`; an SMTP application reads only its own SMTP type) consistent with the platform policy plane, with no disclosure through the difference between "denied" and "absent".
- **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-oagw`, `cpt-cf-credstore-actor-integration-app`, `cpt-cf-credstore-actor-platform-gear`

#### Six Actions on the Credential Type

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-authz-action-split`

Authorization **MUST** distinguish six actions on the credential resource type `gts.cf.core.credstore.credential.v1~` and its derived types: `list`, `read`, `write`, `delete` on the record and `read_secret`, `write_secret` on the secret. The actions an operation needs **MUST** follow from what it touches, not from its address:

| The request… | Requires |
|---|---|
| returns record fields of one credential (including a read with no selection) | `read` |
| returns record fields of several credentials | `list` |
| returns a secret | `read_secret` |
| changes record fields | `write` |
| writes or removes a secret | `write_secret` |
| deletes a record | `delete` |

When several rows apply, all **MUST** be granted. The usage envelope returned with a secret (reference, type, expiry, validator) **MUST NOT** require `read`. The type **MUST** be the only scope axis: a service that needs "its own" credentials declares its own derived type, and because the type is immutable, a metadata edit can never change who may read a secret. Grants issued for the shipped actions on the shipped `secret.v1~` type **MUST NOT** be honoured as synonyms.

- **Rationale**: Enumerating, reading metadata and reading a secret have different blast radius and must be separately grantable.
- **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-integrations-admin`, `cpt-cf-credstore-actor-integration-app`, `cpt-cf-credstore-actor-provisioner`

#### Gear as the Only Enforcement Point

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-authz-gear`

Authorization, sharing, hierarchy and type-trait enforcement **MUST** live exclusively in the gear. The secret store **MUST** authorize only the gear's own identity and **MUST NOT** hold per-tenant or per-user policy; the store adapter **MUST NOT** make any policy decision.

- **Rationale**: One enforcement point keeps behaviour consistent and auditable; the store's single identity is a boundary guard, not a policy.
- **Actors**: `cpt-cf-credstore-actor-platform-gear`, `cpt-cf-credstore-actor-backend`

### 5.4 P1 — Storage, Consistency and Concurrency

#### Vault-Compatible Secret Store as the System of Record

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-production-backend`

Every credential — its secret and every record field that decides its visibility and use (type, sharing mode, fallback policy, descendant block, expiry, lifecycle status) — **MUST** be kept in the secret store as one record that changes atomically. An operation **MUST NOT** serve a secret under visibility rules other than those stored with it. A write **MUST** be reported as successful only once the secret store has durably accepted it, and a write the store rejected **MUST** leave nothing visible. The gear **MUST** reach the secret store only through the store adapter (`cpt-cf-credstore-interface-plugin-client`).

- **Rationale**: Keeping the secret and its visibility together removes every metadata/value divergence window that the shipped design contained with sagas, a reaper and a fingerprint.
- **Actors**: `cpt-cf-credstore-actor-backend`, `cpt-cf-credstore-actor-platform-gear`

#### Crash-Safe Writes

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-write-lifecycle`

A write interrupted at any point — process crash, lost connection, client disconnect, database failure — **MUST** leave the credential either exactly as before the write or exactly as the write specified, never in between. On a rotation the old secret **MUST** keep serving until the new one is in place. A failure **MUST NOT** leave a record that is unreadable, half-written, or permanently reserving its reference, and reaching a consistent state **MUST NOT** require any background process.

- **Rationale**: Readers must never observe half-written credentials; writers must never wedge a name.
- **Actors**: `cpt-cf-credstore-actor-platform-gear`

#### Crash-Safe Delete

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-deprovisioning`

A delete **MUST** be a single step: once it returns, the credential no longer resolves anywhere, its secret is no longer retrievable through the gear, and the reference is free. An interrupted delete **MUST** leave the credential either fully present or fully deleted. Callers **MUST NOT** observe any intermediate status, retention window or deferred cleanup.

- **Rationale**: Revocation must be reliable without a saga, a reaper or a garbage-collection backlog.
- **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-platform-gear`

#### Versions Never Repeat

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-immutable-value-versions`

Every change of an own record **MUST** produce a new store-assigned version, monotonic per reference and sharing class and never reused — including after the record is deleted and the reference re-created. A validator obtained before a delete **MUST** therefore never match the re-created record.

- **Rationale**: A reused version would let a delayed writer holding a stale validator overwrite a credential it never saw (ABA).
- **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-self-rotating-app`, `cpt-cf-credstore-actor-backend`

#### Optimistic Concurrency

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-optimistic-concurrency`

Every write and delete **MUST** state a precondition: create-only (full replace only), "the stated validator is still current", or explicit last-writer-wins. A missing precondition, or a full replace stating both create-only and a validator, **MUST** be a validation error. The precondition **MUST** be enforced atomically by the secret store, so that of two concurrent writes under the same validator exactly one succeeds. Every failed precondition **MUST** surface as the same conflict. A create-only write **MUST** conflict only with a live own record of the same class — an inherited record or an expired own record does not count.

- **Rationale**: Lost-update detection for concurrent administrators and self-rotating replicas; there are no implicit unconditional overwrites.
- **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-self-rotating-app`, `cpt-cf-credstore-actor-platform-gear`

#### Expired Records

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-expired-records`

Expiry **MUST** be handled without any background job:

- **filter on read** — every read (point read, `get_secret`, listing, secret mode) and every resolution **MUST** treat an expired record as if it did not exist, so resolution continues past it;
- **heal on write** — a write or delete addressing an expired own record **MUST** treat it as absent: a create-only full replace succeeds and replaces it; a guarded or last-writer-wins write fails as not-found; a delete returns not-found and removes the expired record from the secret store in the same operation.

- **Rationale**: Expiry needs no resident reaper or scheduled job when every read ignores expired records and every write reclaims them.
- **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-integrations-admin`

#### Derived Credential Index

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-derived-index`

The gear's database **MUST** serve only as a derived index of record fields. It **MUST NOT** hold any secret, **MUST** be fully rebuildable from the secret store alone, and **MUST NOT** by itself decide what is served: every secret and record returned **MUST** come from the secret store and be re-checked against the visibility rules stored with it. The index **MUST** guarantee:

- **read-your-writes** — a write acknowledged to a caller is reflected in that caller's subsequent reads and listings;
- **no false absence** — a credential present in the secret store is never reported as not-found because the index missed it; while the index is incomplete, the gear either resolves correctly without it or reports unavailable;
- **self-healing** — an index entry left stale by a crash, a lost update or a reordered update is corrected on the next access to its reference or by a rebuild, and an older state never overwrites a newer one;
- **online rebuild** — an operator-triggered rebuild runs while the gear serves traffic, is safe to interrupt and re-run, and converges to exactly the secret store's content.

Loss of the database **MUST NOT** lose any credential.

- **Rationale**: The index exists only to make listing and hierarchical resolution cheap; making it disposable removes backup coupling between two stores and the class of bugs where the two disagree about what may be served.
- **Actors**: `cpt-cf-credstore-actor-platform-gear`, `cpt-cf-credstore-actor-platform-operator`

### 5.5 P1 — Credential Types

#### GTS Credential Types

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-secret-types`

Each credential **MUST** have a credential type — a GTS type derived from `gts.cf.core.credstore.credential.v1~` and registered in `types-registry` — chosen at creation and immutable afterwards; the type is also the PDP resource type. Registering a new derived type **MUST NOT** require a CredStore release. Each type **MUST** declare traits the gear enforces on every write, at minimum:

- the sharing modes it permits (e.g., `personal-token` is private-only);
- optional structural validation of the secret;
- bounds on the secret's size and whether it must be valid UTF-8;
- whether it is expirable.

A write violating a trait **MUST** be rejected with a stable reason. A platform maximum secret size **MUST** apply to every type. Traits **MUST** be enforced against the type's current registered revision at write time; a record stored under an earlier revision **MUST** stay readable, and its next write **MUST** satisfy the current traits. A type that allows binary secrets **MUST** round-trip them byte-exact through the in-process client; the REST transport carries text only, and a secret that is not valid UTF-8 **MUST** be rejected on it, never lossily decoded. An unknown type named by a caller **MUST** be a validation error; a stored type that is no longer registered **MUST** surface as unavailable. The initial catalogue **MUST** cover `generic`, `api-key`, `personal-token`, `oauth2-client`, `basic-auth`, `bearer-token`, `certificate`, `ssh-key`, `webhook-hmac` and `connection-string`.

- **Rationale**: Different kinds of secrets have different safe-handling rules; type traits give one enforcement point, platform-native versioning and per-type policy targeting.
- **Actors**: `cpt-cf-credstore-actor-tenant-admin`, `cpt-cf-credstore-actor-platform-gear`

### 5.6 P1 — Consumer Surface

#### Consumer Surface of #4737/#4741

- [ ] `p1` - **ID**: `cpt-cf-credstore-fr-consumer-compat`

The credential surface — in-process client operations, REST addresses and verbs, preconditions, field names and their values, error categories and reason codes — **MUST** follow the credential surface specified by PR #4737 (head `01b012908`) and implemented by PR #4741 (head `71b6177b1`). The only permitted differences are:

- **Validator content** — the validator stays opaque and keeps its role, but its content is not part of the contract; consumers that parse it are unsupported.
- **Version numbering** — the version of a re-created record continues from the deleted record's version instead of restarting.
- **Expired records** — filtered on read and healed on write (`cpt-cf-credstore-fr-expired-records`) instead of being removed by a maintenance job; a create-only write therefore succeeds over an expired own record.
- **Maintenance operation** — the host-invoked maintenance operation is withdrawn, because nothing requires periodic maintenance.
- **Backend** — the value-store plugin SPI and vendor selection are replaced by the internal store adapter; this affects backend implementers and operators, not consumers.
- **Addition** — the descendant block is new; a record without it behaves exactly as under #4741.

The move from the shipped surface on main is a deliberate break, permitted before mass production use: a plain read no longer returns the secret; the owning tenant and the inherited flag are replaced by the inheritance status; the three shipped actions are replaced by six; the create-only address and the shipped write operations are replaced by the guarded full replace and the partial update.

- **Rationale**: #4737/#4741 is the surface its consumers are being migrated to; changing how credentials are stored must not reopen that migration.
- **Actors**: `cpt-cf-credstore-actor-platform-gear`, `cpt-cf-credstore-actor-oagw`, `cpt-cf-credstore-actor-integration-app`
- **Verification Method**: the #4741 REST and client contract test suites run against the new gear, except for tests pinned to the listed differences.

### 5.7 P2 — Later

#### No Retained Previous Secrets

- [ ] `p2` - **ID**: `cpt-cf-credstore-fr-no-secret-history`

After a secret is rotated, removed or deleted, the secret store **MUST NOT** keep the previous secret as an older version.

- **Rationale**: In `p1` a previous secret is unreachable through the gear but may remain in the store; revoking a leaked key should remove it from the store as well.
- **Actors**: `cpt-cf-credstore-actor-platform-operator`, `cpt-cf-credstore-actor-backend`

#### Secret Store Configuration Check

- [ ] `p2` - **ID**: `cpt-cf-credstore-fr-store-config-check`

At startup the gear **MUST** verify that the secret store mount is KV version 2, requires conditional writes, and retains no older versions, that the gear's identity can reach its installation prefix, and that the in-process store adapter is not selected outside an explicitly declared test configuration. On any failed check the gear **MUST** refuse to become ready and name the failed check.

- **Rationale**: A mis-configured mount silently weakens concurrency control or retains rotated secrets, and a test adapter in production loses every credential on restart; failing at boot is the safe outcome.
- **Actors**: `cpt-cf-credstore-actor-platform-operator`, `cpt-cf-credstore-actor-backend`

#### Tenant Offboarding

- [ ] `p2` - **ID**: `cpt-cf-credstore-fr-tenant-offboarding`

When account-management deletes a tenant, the system **MUST** remove every record of that tenant — including the private records of all its owners and the records kept for deleted references — from the secret store and the index, **MUST** be idempotent under retry, and **MUST** report completion to account-management. Once deletion has started, writes for that tenant **MUST** be rejected. Account-management deletes only tenants without non-deleted children, so offboarding never affects a live descendant.

- **Rationale**: Credentials of a removed tenant are a liability, and they are the only data the gear keeps in the secret store indefinitely.
- **Actors**: `cpt-cf-credstore-actor-platform-services`, `cpt-cf-credstore-actor-backend`

## 6. Non-Functional Requirements

> **Global baselines**: project-wide NFRs are defined in [ARCHITECTURE_MANIFEST.md](../../../docs/ARCHITECTURE_MANIFEST.md) and [SECURITY.md](../../../guidelines/SECURITY.md). Only gear-specific NFRs are listed here.

### 6.1 Gear-Specific NFRs

#### Secret Confidentiality

- [ ] `p1` - **ID**: `cpt-cf-credstore-nfr-confidentiality`

Secrets **MUST NOT** appear in logs, traces, metrics, error messages or debug output of the gear, the store adapter or the transport; secret-store responses and error bodies **MUST NOT** be logged verbatim. Secret memory **MUST** be zeroized on drop. Metadata surfaces **MUST NOT** be able to carry a secret by construction. Traffic to the secret store **MUST** be encrypted in transit with the store's identity verified.

- **Threshold**: zero plaintext secrets in any log, trace or metric, verified by automated log-capture tests over every operation and failure path.
- **Rationale**: Secrets are the most sensitive data on the platform.
- **Architecture Allocation**: see DESIGN.md security section.

#### Tenant Isolation

- [ ] `p1` - **ID**: `cpt-cf-credstore-nfr-tenant-isolation`

An operation **MUST NOT** read or change a credential outside the caller's PDP-authorized scope and the visibility rules of `cpt-cf-credstore-fr-hierarchical-resolve`. Inaccessible credentials **MUST** be indistinguishable from non-existent ones — per item inside a bulk response as well as for point reads. A record of one tenant **MUST NOT** be addressable through a request made in another tenant.

- **Threshold**: zero cross-tenant reads or writes outside the authorized scope in the isolation test suite.
- **Rationale**: The platform's multi-tenant guarantee.
- **Architecture Allocation**: see DESIGN.md security section.

#### Audit

- [ ] `p1` - **ID**: `cpt-cf-credstore-nfr-audit`

Every secret returned — by a point read, `get_secret` or secret mode — **MUST** produce a structured audit event naming the subject, the tenant acted in, the reference, the credential type and the operation — never the secret.

- **Threshold**: one audit event per secret returned, 100% coverage in tests.
- **Rationale**: The secret store sees only the gear's identity; attribution to a platform subject exists only in the gear.
- **Architecture Allocation**: see DESIGN.md observability section.

#### Store Audit Correlation

- [ ] `p2` - **ID**: `cpt-cf-credstore-nfr-store-audit-correlation`

Every secret-store request **MUST** carry the platform request identifier so that the store's own audit log can be correlated with the gear's audit events.

- **Threshold**: 100% of store requests carry the identifier, verified against the store's audit log in an integration test.
- **Rationale**: Incident reconstruction needs to join the gear's subject-level events with the store's access log.
- **Architecture Allocation**: see DESIGN.md observability section.

#### Store Access Hygiene

- [ ] `p1` - **ID**: `cpt-cf-credstore-nfr-store-access`

In production the gear **MUST** authenticate to the secret store with a short-lived, renewable identity obtained from the runtime — never a long-lived static token — renew it before expiry, and re-authenticate after a failed renewal without operator action. The identity's store permissions **MUST** be limited to the operations the gear uses, within its installation prefix.

- **Threshold**: zero requests failing solely because the gear's store token expired, over a soak test spanning at least three token lifetimes.
- **Rationale**: One identity serves the whole installation, so its lifetime and scope bound the blast radius of a compromise.
- **Architecture Allocation**: see DESIGN.md security section.

#### Resolution Cost

- [ ] `p1` - **ID**: `cpt-cf-credstore-nfr-resolution-cost`

In the steady state a single-credential read **MUST** cost at most one index query and one secret-store read, independent of the depth of the ancestor chain; a secret-mode read **MUST** cost at most one index query plus one secret-store read per item returned.

- **Threshold**: 1 index query + 1 store read per resolved credential, measured by dependency metrics in integration tests at chain depths 1, 5 and 10.
- **Rationale**: OAGW resolves credentials on the request hot path; cost growing with hierarchy depth would scale with the partner tree.
- **Architecture Allocation**: see DESIGN.md resolution flow.

#### Availability and Fail-Closed Behaviour

- [ ] `p1` - **ID**: `cpt-cf-credstore-nfr-availability`

Every call to a dependency — secret store, index database, PDP, `tenant-resolver`, `types-registry` — **MUST** be bounded by a timeout. When the secret store is unreachable, sealed or throttling, or when any other dependency fails or times out, every affected operation **MUST** fail as unavailable, with a retry hint when one is known. The gear **MUST NOT** serve a secret whose record it has not read from the secret store for that request, and **MUST NOT** return a result decided by index data it could not confirm against the secret store. The gear **MUST NOT** add any availability dependency beyond those listed.

- **Threshold**: zero secrets served under an unconfirmed visibility decision, and no request exceeding the sum of its dependency timeouts, in fault-injection tests.
- **Rationale**: Credential access favours correctness over availability.
- **Architecture Allocation**: see DESIGN.md error handling.

#### Recovery

- [ ] `p1` - **ID**: `cpt-cf-credstore-nfr-recovery`

Credential durability **MUST** equal the secret store's: the recovery point of any credential is the store's recovery point, and nothing in the gear's database is needed to restore it. After a database loss, an index rebuild **MUST** restore listing and resolution to exactly the store's content without any manual data repair.

- **Threshold**: rebuild of an index holding 100 000 records completes without data loss and with identical listing results before and after, in a recovery test.
- **Rationale**: The platform backs up one system of record, not two.
- **Architecture Allocation**: see DESIGN.md operations section.

#### Observability

- [ ] `p1` - **ID**: `cpt-cf-credstore-nfr-observability`

The gear **MUST** emit metrics sufficient to detect resolution anomalies and store-side failures: resolution depth and outcome (own, inherited, overridden, suppressed, miss); latency and outcome per dependency (PDP, `tenant-resolver`, `types-registry`, secret store, index) with store failures classified as not-found, conflict, forbidden, throttled, sealed or other; index repairs and rebuild progress; cross-tenant denials. Metrics **MUST NOT** require counting queries, and metric labels **MUST NOT** contain references or secrets.

- **Threshold**: every listed signal present and exercised in integration tests.
- **Rationale**: Index drift and store-side failures are quiet; operators need signals, not log archaeology.
- **Architecture Allocation**: see DESIGN.md observability section.

### 6.2 NFR Exclusions

- **Encryption at rest**: provided by the secret store's barrier encryption and seal; the gear adds no encryption layer of its own.
- **Backup of the index**: not required; the index is rebuilt from the secret store (`cpt-cf-credstore-nfr-recovery`).
- **Absolute latency SLO**: not stated; end-to-end latency is dominated by the secret store and depends on its deployment. `cpt-cf-credstore-nfr-resolution-cost` bounds the gear's own contribution.
- **Index-rebuild duration**: no time bound is set; reads stay correct or unavailable while a rebuild runs (`cpt-cf-credstore-fr-derived-index`), and the duration depends on the secret store's listing throughput, which DESIGN measures before setting an operational target.
- **Throughput and capacity targets**: inherited from the secret store's capacity; the gear adds no per-tenant quota.
- **Audit retention and personal data**: audit events follow the platform's audit retention; the gear treats every secret as opaque and takes no responsibility for personal data a caller chooses to store in one.
- **Usability, accessibility, internationalization**: not applicable — the gear has no user interface; human actors reach it through platform tooling.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### CredStoreClientV1

- [ ] `p1` - **ID**: `cpt-cf-credstore-interface-client`

- **Type**: Rust trait (async), registered in ClientHub without scope; SecurityContext is the first argument of every operation.
- **Stability**: stable
- **Description**: The consumer API of #4741: `get` (one credential with field selection; the secret only when selected), `get_secret` (secret with its usage envelope and validator), `put` (precondition-guarded create or full replace of record and secret; the only way to create), `patch` (precondition-guarded merge-patch partial update; never creates), `list` (paginated listing with filtering, ordering and field selection, which also serves secret mode) and `delete` (precondition-guarded). Secrets are bytes on this interface. Hierarchical resolution is internal.
- **Breaking Change Policy**: breaking changes to v1 are allowed only before mass production use; afterwards a major version bump is required.

#### Store Adapter

- [ ] `p1` - **ID**: `cpt-cf-credstore-interface-plugin-client`

- **Type**: Rust trait (async), internal to the gear.
- **Stability**: unstable
- **Description**: The gear's only path to the secret store, with no store-specific types in its operations: read a record with its version, conditionally write a record (only if absent, or only if the stated version is current), delete a record, and list keys under a prefix. Keys are opaque to it; failures map to not-found, conflict, unavailable (with a retry hint when known) or other. It carries no policy and makes no visibility decision; authentication, token renewal, TLS and the installation prefix are internal to each implementation. Implemented for Vault-compatible KV version 2 and, for tests, in process. Shaped so that it can later become a pluggable backend SPI without changing consumers. Replaces the shipped value-store plugin SPI.
- **Breaking Change Policy**: minor version bump (unstable API).

### 7.2 External Integration Contracts

#### REST API

- [ ] `p1` - **ID**: `cpt-cf-credstore-contract-rest-api`

- **Direction**: provided
- **Protocol/Format**: HTTP/REST, JSON, the canonical `Problem` error envelope, under a versioned path beneath the platform API prefix; the credential surface of #4737/#4741 — a credential collection for the listing and secret mode, and one address per reference for the point read, full replace, merge-patch partial update and delete. Concrete endpoints, status codes, headers and reason codes are in DESIGN.md.
- **Compatibility**: as #4737/#4741 except for `cpt-cf-credstore-fr-consumer-compat`; breaks the shipped surface on main by design; backward-compatible within the major version once v1 is declared final.

#### GTS Registration

- [ ] `p1` - **ID**: `cpt-cf-credstore-contract-gts`

- **Direction**: provided to `types-registry`
- **Protocol/Format**: GTS link-time inventory: the credential base type `gts.cf.core.credstore.credential.v1~` (also the PDP resource type, carrying the traits schema) and the derived credential-type catalogue with traits. Stored type references are UUIDs.
- **Compatibility**: type ids are stable; new versions are new ids. The rename from the shipped `secret.v1~` requires re-issuing every policy that granted the shipped actions.

#### Secret Store

- [ ] `p1` - **ID**: `cpt-cf-credstore-contract-secret-store`

- **Direction**: required from the platform
- **Protocol/Format**: Vault HTTP API, KV version 2 secrets engine, over TLS. Required capabilities: versioned JSON records, conditional writes on version, metadata reads, key listing, deletion of a key with all its versions, and a machine-identity auth method (Kubernetes or AppRole) with renewable tokens. For `p2`: per-mount version retention and mandatory conditional writes, and audit of requests with a caller-supplied request identifier. Not required: namespaces, soft delete, custom metadata.
- **Compatibility**: any HashiCorp Vault or OpenBao release providing these capabilities.

## 8. Use Cases

### 8.1 Publishing and Consuming

#### UC-001: Partner Publishes a Shared Credential

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-create-shared`

**Actor**: `cpt-cf-credstore-actor-tenant-admin`

**Preconditions**:
- The partner admin holds `write` and `write_secret` on the credential type in the partner tenant

**Main Flow**:
1. The admin creates `partner-openai-key` with a secret, sharing `shared`, with a create-only full replace
2. The gear authorizes the request, validates the type traits and writes the record to the secret store
3. The gear acknowledges once the secret store has accepted the write

**Postconditions**:
- The credential resolves for the partner and all descendants and is listed in the partner's catalogue as `own`

**Alternative Flows**:
- **Reference already taken by a live own non-private record**: conflict; nothing changes
- **Type forbids `shared`**: validation error; nothing is written
- **Secret store unavailable**: unavailable; nothing is visible and the reference is not reserved
- **Index update fails after the store accepted the write**: the caller still gets success and sees the credential in its next read and listing; the index is corrected on the next access or by a rebuild

#### UC-002: OAGW Retrieves a Credential for a Customer

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-hierarchical-resolve`

**Actor**: `cpt-cf-credstore-actor-oagw`

**Preconditions**:
- OAGW is authorized to `read_secret` the credential type in the customer's scope
- The partner holds a `shared` `partner-openai-key`; the customer is a descendant and holds no record of it

**Main Flow**:
1. OAGW constructs a SecurityContext for the customer and calls `get_secret("partner-openai-key")`
2. The gear obtains the customer's ancestor chain, selects the decisive record and authorizes the read against its type
3. The gear reads that one record from the secret store, re-checks its visibility and returns the secret with its type, expiry and an opaque validator

**Postconditions**:
- OAGW has the secret; the customer never sees it; the read is audited; nothing names the partner

**Alternative Flows**:
- **The customer holds its own record with a secret**: that secret is returned
- **Nothing decisive in the chain, or the read is not permitted**: empty result
- **Secret store unavailable**: unavailable; OAGW fails the upstream call rather than using a cached secret

#### UC-003: Customer Overrides a Partner Credential

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-shadowing`

**Actor**: `cpt-cf-credstore-actor-tenant-admin`

**Preconditions**:
- The partner holds a `shared` `partner-openai-key` of type `api-key`

**Main Flow**:
1. The customer creates its own `partner-openai-key` of type `api-key`, sharing `tenant`, with its own secret
2. OAGW resolves the reference for the customer and receives the customer's secret
3. The customer's catalogue shows the entry as `overridden`

**Postconditions**:
- The customer uses its own key; the partner's other descendants still receive the partner's key

**Alternative Flows**:
- **The customer names a different type**: conflict; nothing is written
- **The customer uses `private`**: only that owner is overridden; other subjects in the customer tenant still receive the partner's key
- **The customer deletes its record**: the customer resolves the partner's key again at once

#### UC-004: Private Credentials Never Leak or Block

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-private-denied`

**Actor**: `cpt-cf-credstore-actor-oagw`

**Preconditions**:
- In the partner tenant, PartnerAdmin holds a `private` `internal-admin-key`
- In the customer tenant, User A holds a `private` `api-key`; the partner holds a `shared` `api-key`

**Main Flow**:
1. OAGW resolves `internal-admin-key` for the customer → not found
2. User B of the customer tenant resolves `api-key` → the partner's secret
3. User A resolves `api-key` → User A's own secret

**Postconditions**:
- A private record is never disclosed to other subjects or descendants and never hides an ancestor's record from them

#### UC-005: Mail Service Fetches Its Whole Credential Set

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-bulk-fetch-own-set`

**Actor**: `cpt-cf-credstore-actor-integration-app`

**Preconditions**:
- The application holds `read_secret` only on its own derived SMTP credential type, in the tenant it acts for

**Main Flow**:
1. The application issues one secret-mode read scoped to its type, selecting reference, type, expiry and secret
2. The gear resolves every matching reference, authorizes each item and reads each winner from the secret store
3. The application receives every matching credential in one response, each with its secret, type and expiry

**Postconditions**:
- One audit event per secret returned; credentials of other types were neither evaluated nor returned

**Alternative Flows**:
- **More matches than the cap**: the request fails; nothing is returned
- **An item resolves to a suppression or has expired**: it is omitted
- **The application reads a credential of another type by reference**: not found

### 8.2 Administration Without Plaintext

#### UC-006: Guarded Lifecycle of an Own Credential

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-crud`

**Actor**: `cpt-cf-credstore-actor-tenant-admin`

**Preconditions**:
- The admin holds all six actions on the type

**Main Flow**:
1. Create a credential with a create-only full replace
2. Read it; receive the record and its validator
3. Rotate the secret with a partial update under that validator; a new validator is returned
4. Delete under the new validator; the credential stops resolving and the reference is free

**Postconditions**:
- Every secret read is audited; the rotated and deleted secrets are no longer retrievable through the gear

**Alternative Flows**:
- **Stale validator**: conflict; nothing changes
- **Missing precondition**: validation error
- **Delete, re-create, then write with a validator from before the delete**: conflict — versions never repeat

#### UC-007: Independent Private Credentials per Owner

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-private-owner-only`

**Actor**: `cpt-cf-credstore-actor-tenant-admin`

**Preconditions**:
- Users A, B and C are subjects of the same tenant; the tenant holds a `tenant` record `my-token`

**Main Flow**:
1. User A creates a `private` `my-token`; User B creates a `private` `my-token`; neither conflicts
2. Users A and B each resolve their own private secret; User C resolves the tenant record

**Postconditions**:
- Three independent records share one reference

**Alternative Flows**:
- **User B deletes `my-token` in the private class**: only User B's private record is removed

#### UC-008: Type-Restricted Sharing

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-type-restricted-sharing`

**Actor**: `cpt-cf-credstore-actor-tenant-admin`

**Preconditions**:
- `personal-token` permits `private` only

**Main Flow**:
1. A user creates a `personal-token` credential as `private` → accepted
2. A later partial update to `tenant` or `shared` → rejected with the type's sharing reason

**Postconditions**:
- A personal token can never be widened beyond its owner, whatever the caller's grants

**Alternative Flows**:
- **An attempt to change the type**: rejected; the type is immutable

#### UC-009: Administrator Configures SMTP Without Seeing Credentials

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-admin-configure-without-value`

**Actor**: `cpt-cf-credstore-actor-integrations-admin`

**Preconditions**:
- The administrator holds `list`, `read`, `write`, `write_secret` and `delete`, but not `read_secret`
- A partner ancestor publishes a `shared` SMTP credential

**Main Flow**:
1. The administrator lists the catalogue and sees the SMTP entry as `inherited`
2. The administrator creates the tenant's own SMTP record with its secret, create-only
3. The administrator reads the record for its validator, changes its expiry with a partial update carrying no secret, then rotates the secret with a partial update carrying only the secret

**Postconditions**:
- The tenant has its own SMTP credential; the administrator never saw a secret

**Alternative Flows**:
- **The administrator selects the secret**: refused exactly as a missing reference would be
- **Suppress instead of override**: see UC-011

#### UC-010: Provisioning Injector Fills a Declared Record

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-provisioner-fills-declared`

**Actor**: `cpt-cf-credstore-actor-provisioner`

**Preconditions**:
- An administrator created a declared `stripe-key` record
- The injector holds `write_secret` only

**Main Flow**:
1. The injector writes the secret with a partial update carrying only the secret, under last-writer-wins
2. The record becomes active and resolves

**Postconditions**:
- The injector placed a secret it cannot read back

**Alternative Flows**:
- **The request also changes sharing**: refused — it would need `write`
- **No own record exists**: not-found; a partial update never creates

### 8.3 Inheritance Control

#### UC-011: Override, Suppress and Return to Inheritance

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-override-suppress-return`

**Actor**: `cpt-cf-credstore-actor-integrations-admin`

**Preconditions**:
- T1 publishes `smtp-default` as `shared` with secret V1; T2 is T1's child and T3 is T2's child; neither holds a record
- The administrator, acting in T2, holds `write`, `write_secret` and `delete`

**Main Flow**:
1. T2 creates its own `shared` record with V2 → T2 and T3 resolve V2; T2 sees `overridden`
2. T2 rotates to V3 → T2 and T3 resolve V3; V2 is no longer retrievable through the gear
3. T2 sets fallback `none` and removes the secret in one partial update → T2 and T3 resolve nothing and see `suppressed`; T1 and its other descendants are unaffected
4. T2 deletes its record → T2 and T3 resolve V1 again and see `inherited`

**Postconditions**:
- No step served a wrong tenant's secret, and T1's credential was never touched

**Alternative Flows**:
- **Soft return instead of step 4**: T2 sets fallback `inherit`; T2 and T3 resolve V1, and T2 keeps its declared record for a future override
- **T2 suppresses without ever holding a record**: one create-only full replace of a declared record with fallback `none`, needing only `write`
- **T2's record is `tenant` in step 3**: only T2 resolves nothing; T3 resolves V1

#### UC-012: Partner Withholds a Credential from Its Customers

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-descendant-block`

**Actor**: `cpt-cf-credstore-actor-tenant-admin`

**Preconditions**:
- T1 publishes `llm-key` as `shared` with secret V1; T2 is T1's child; T3 is T2's child

**Main Flow**:
1. T2 creates a declared record with fallback `inherit` and a descendant block
2. T2 resolves V1 and sees `inherited`; T3 resolves nothing and sees `suppressed`, without learning who blocked it

**Postconditions**:
- T2 uses T1's credential; T3 and its subtree receive nothing

**Alternative Flows**:
- **Own key for T2, nothing below**: T2's record holds V2 and a block → T2 resolves V2; T3 resolves neither V2 nor V1
- **T3 creates its own record with V3 under the block**: T3 resolves V3 and sees `overridden`; T3's descendants follow T3's own sharing mode
- **T3 later deletes its record**: T3 resolves nothing again, not V1
- **T2 lifts the block**: T3 resolves per the ordinary rules at once

### 8.4 Operations

#### UC-013: Reliable Revocation

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-deprovisioning`

**Actor**: `cpt-cf-credstore-actor-tenant-admin`

**Preconditions**:
- The partner holds an active `shared` credential used by its descendants

**Main Flow**:
1. The partner deletes the credential under its validator
2. The credential stops resolving for the partner and every descendant that used it; descendants resolve the next decisive record up the chain
3. The deleted secret is no longer retrievable through the gear

**Postconditions**:
- The reference is immediately reusable

**Alternative Flows**:
- **Secret store unavailable**: unavailable; the credential is exactly as before
- **Crash during the delete**: the credential is either fully present or fully deleted
- **Retry after success**: not-found

#### UC-014: Concurrent Rotation by Two Replicas

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-concurrent-rotation`

**Actor**: `cpt-cf-credstore-actor-self-rotating-app`

**Preconditions**:
- Two replicas of the application called `get_secret` on the same credential and obtained the same validator

**Main Flow**:
1. Both replicas refresh the token with the provider and write it back with a partial update under that validator
2. Exactly one write succeeds; the other gets a conflict
3. The losing replica calls `get_secret` again and uses the winner's secret

**Postconditions**:
- The credential holds exactly one of the two secrets; no update was lost silently

#### UC-015: Index Rebuilt After Database Loss

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-index-rebuild`

**Actor**: `cpt-cf-credstore-actor-platform-operator`

**Preconditions**:
- The gear's database was lost or restored from an old backup; the secret store is intact

**Main Flow**:
1. The operator triggers the index rebuild
2. The gear enumerates the secret store under its installation prefix and restores every index entry, keeping the newer state where both exist and removing entries the store no longer has
3. The gear reports completion

**Postconditions**:
- Listing and resolution return exactly what the secret store holds; no credential was lost

**Alternative Flows**:
- **Reads during the rebuild**: correct results or unavailable, never a false not-found
- **Writes during the rebuild**: never overwritten by the rebuild
- **Rebuild interrupted**: re-running it converges to the same result

#### UC-016: Renewing an Expired Credential

- [ ] `p1` - **ID**: `cpt-cf-credstore-usecase-expired-renewal`

**Actor**: `cpt-cf-credstore-actor-integrations-admin`

**Preconditions**:
- The tenant's own `bearer-token` record has expired; an ancestor holds a `shared` record of the same reference and type

**Main Flow**:
1. A read in the tenant resolves past the expired record and returns the ancestor's credential as `inherited`
2. The administrator creates the record again with a create-only full replace and a new expiry
3. The new record replaces the expired one and resolves as `overridden`

**Postconditions**:
- No job ran; the expired secret was replaced by the write

**Alternative Flows**:
- **The administrator deletes instead**: not-found, and the expired record is removed from the secret store
- **A guarded partial update addresses the expired record**: not-found

#### UC-017: Mis-Configured Secret Store at Startup

- [ ] `p2` - **ID**: `cpt-cf-credstore-usecase-store-misconfigured`

**Actor**: `cpt-cf-credstore-actor-platform-operator`

**Preconditions**:
- The mount retains older versions or does not require conditional writes, or the in-process adapter is selected in a production configuration

**Main Flow**:
1. The gear starts and checks its store configuration
2. The gear refuses to become ready and reports the failed check

**Postconditions**:
- No request is served against a store that would weaken concurrency control, retain rotated secrets, or lose credentials on restart

## 9. Acceptance Criteria

- [ ] The #4741 credential contract test suites pass against the new gear, except for tests pinned to the differences in `cpt-cf-credstore-fr-consumer-compat`
- [ ] The resolution table of `cpt-cf-credstore-fr-hierarchical-resolve` holds row by row for the point read, `get_secret`, the listing and secret mode alike, including across an isolation barrier
- [ ] Use cases UC-001 through UC-016 behave exactly as described, including every alternative flow
- [ ] A caller holding only metadata actions never receives a secret on any surface; a caller holding only `read_secret` reads a secret through `get_secret`; a caller holding only `write_secret` rotates a secret through a partial update; a type-denied read is indistinguishable from not-found; a PDP failure fails closed
- [ ] A filtered listing never reports an item that a point read of the same reference would not return, including when an ancestor's record of that reference has a different type
- [ ] Every credential lives in the secret store as one record with all its visibility fields; dropping the index database and rebuilding it changes no listing and no resolution result, and loses no credential
- [ ] Fault injection at every step of a write and a delete leaves the credential either as before or as specified; no reaper, saga or maintenance job runs
- [ ] Versions never repeat across delete and re-create; of two concurrent writes under one validator exactly one succeeds
- [ ] Expired records are invisible to every read and are replaced or removed by the next write or delete that addresses them, with no job running
- [ ] A binary secret written through the in-process client round-trips byte-exact for a type that allows binary; a non-UTF-8 secret is rejected on the REST transport
- [ ] A single read costs at most one index query and one store read at chain depths 1, 5 and 10
- [ ] No secret, store response body, store path or other tenant's identifier appears in any log, trace, metric, response or error
- [ ] Every secret returned produces an audit event
- [ ] The gear's store identity is short-lived and renewed without operator action
- [ ] `p2` The gear refuses to start against a mount that retains older versions, does not require conditional writes, or is unreachable under the installation prefix, and when the in-process adapter is selected in production
- [ ] `p2` Deleting a tenant removes all of its records from the secret store and the index, idempotently, and rejects later writes for it

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| Vault-compatible secret store (Vault / OpenBao, KV v2) | System of record for every credential | `p1` |
| Machine identity for the store (Kubernetes auth or AppRole) | The gear's single store identity and its policy limited to the installation prefix | `p1` |
| `authz-resolver` | Six actions per concrete credential type, fail-closed | `p1` |
| `tenant-resolver` | Ancestor chains, including across isolation barriers | `p1` |
| `types-registry` | Credential base type, derived types and traits | `p1` |
| Database (PostgreSQL / SQLite) | Derived credential index only | `p1` |
| PDP policy re-issuance | Policies granting the shipped `read`/`write`/`delete` on `secret.v1~` are re-issued under the six actions on `credential.v1~` before cutover | `p1` |
| Consumer migration | OAGW, settings-service and the Keycloak IdP plugin move to the #4741 client operations | `p1` |
| Account-management tenant deletion | Trigger for tenant offboarding | `p2` |

## 11. Assumptions

- Every production deployment runs a Vault-compatible secret store operated by the platform; its availability, sealing, snapshots and encryption at rest are the platform's responsibility
- One store identity per installation is acceptable; tenant isolation is the gear's responsibility, not the store's
- No component other than the gear writes under the installation prefix
- The shipped gear holds no persistent credentials to migrate: main ships only the in-memory development backend. This must be confirmed for every environment before cutover
- Tenant hierarchy is managed externally and served by `tenant-resolver`; short-TTL caching of ancestor chains is acceptable
- The PDP is the sole authorization authority; there is no local policy cache
- Consumers that provision infrastructure from credentials at startup tolerate a missing credential by degrading rather than failing boot
- Breaking changes to v1 are acceptable before mass production use

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Secrets leaked through logs, traces or store error bodies | Critical security incident | Confidentiality NFR; store responses redacted before logging; zeroize; non-cacheable responses; log-capture tests |
| Compromise of the gear's single store identity | Every credential of the installation readable | Short-lived renewable identity; store policy limited to the installation prefix and the operations used; store audit correlation (p2) |
| Secret store outage, seal or throttling | All credential operations unavailable, including the OAGW hot path | Fail closed with retry hints; bounded timeouts; dependency metrics; store high availability is a platform concern |
| Store latency on the OAGW hot path | Slower upstream calls than with a local value store | One store read per call (`cpt-cf-credstore-nfr-resolution-cost`); value caching is an open question |
| Index drift after partial failures or out-of-band store edits | Listing or resolution temporarily wrong for a reference | Every served result re-checked in the store; index updates ordered by version; repair on access; rebuild; drift metrics |
| Database loss | Listing and resolution degraded until rebuild | Online rebuild (UC-015); reads correct or unavailable meanwhile |
| Records kept for deleted references so versions never repeat | Store key space grows with deletions | Storage cost only; bounded by tenant offboarding (p2) and nothing else |
| Previous and expired secrets remain in the store | A rotated secret stays as an older store version, and an expired secret stays until its reference is written or deleted; neither is reachable through the gear | No retained previous secrets (p2); expired-secret retention is an open question |
| Ancestor-chain cache staleness | A re-parented tenant keeps its former inheritance for up to the cache TTL | Short TTL; closing the window needs a hierarchy change signal from `tenant-resolver` |
| Consumer migration breaks OAGW, settings-service or the IdP plugin | Integrations fail after cutover | Surface of #4741; migrate all three in the cutover release; contract tests against OpenBao |
| Type-trait misconfiguration | Overly permissive or broken writes for a type | Catalogue pinned to registered GTS schemas by tests; `generic` stays permissive |

## 13. Open Questions

- **Delivery path**: land PR #4741 on its current storage first and swap storage behind the same surface later, or supersede #4741 and ship the Vault-backed gear directly?
- **Audit failure behaviour**: must a secret be withheld when its audit event cannot be recorded (fail-closed), or is best-effort emission with a stated loss bound acceptable, and which platform sink receives the events?
- **Value caching for OAGW**: is a short-lived in-process cache of resolved secrets acceptable on the hot path? Adopting one would amend `cpt-cf-credstore-nfr-availability` and add a bounded revocation delay, so it needs its own decision.
- **Expired secrets**: must an expired secret be removed from the secret store at expiry for compliance, or is removal by the next write or delete (`cpt-cf-credstore-fr-expired-records`) sufficient?
- **Descendant block field**: the name and representation of the field on the record (a boolean next to sharing mode, or a descendant-policy enumeration defaulting to the current behaviour).
- **Platform maximum secret size**: which limit applies to every type, given the secret store's own request-size limits?

## 14. Traceability

- **Consumer surface**: PR #4737 (head `01b012908`) — its PRD, DESIGN §4.3.2 and ADR-0004, ADR-0005, ADR-0007, ADR-0008, ADR-0009, ADR-0010; PR #4741 (head `71b6177b1`) — the implementation of that surface
- **Design**: [DESIGN.md](./DESIGN.md) — still describes the shipped design; to be rewritten against this PRD
- **ADRs**: [ADR-0001 stateful gear](./ADR/0001-cpt-cf-credstore-adr-stateful-gear.md), [ADR-0002 deprovisioning saga](./ADR/0002-cpt-cf-credstore-adr-deprovisioning-saga.md) and [ADR-0003 value-fingerprint fence](./ADR/0003-cpt-cf-credstore-adr-value-fingerprint-fence.md) describe the shipped design and are superseded by this PRD's system-of-record model; the ADRs for the Vault-backed design are written with the new DESIGN
- **Retired requirement IDs**: `cpt-cf-credstore-fr-put-secret` (shipped create/update of a secret) is retired; its successors are `cpt-cf-credstore-fr-write-credential-record`, `cpt-cf-credstore-fr-write-secret` and `cpt-cf-credstore-fr-sharing-classes`. Every other requirement ID of the shipped PRD is kept with the same subject.
- **Features**: features/ (planned)
