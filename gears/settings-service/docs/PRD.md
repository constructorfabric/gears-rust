# PRD — Settings Service


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
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 Settings & Category Model](#51-settings--category-model)
  - [5.2 Typed Values & Validation](#52-typed-values--validation)
  - [5.3 Standard/Advanced Mode & Discoverability](#53-standardadvanced-mode--discoverability)
  - [5.4 Defaults & Revert](#54-defaults--revert)
  - [5.5 Staged Change → Apply](#55-staged-change--apply)
  - [5.6 Multi-Tenant Overrides & Cascading Inheritance](#56-multi-tenant-overrides--cascading-inheritance)
  - [5.7 Security, Secrets & Audit](#57-security-secrets--audit)
  - [5.8 Module-Contributed Settings](#58-module-contributed-settings)
  - [5.9 Additional Scope Items](#59-additional-scope-items)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Gear-Specific NFRs](#61-gear-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
  - [10.1 Launch Prerequisites (blocking)](#101-launch-prerequisites-blocking)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

Settings Service is the platform's typed configuration gear. It provides a single surface for platform and tenant administrators to manage settings (categories, typed keys, defaults), apply staged changes safely, and resolve per-tenant overrides through the tenant scope hierarchy — including feature flags and other platform/tenant customization consumed by other gears.

### 1.2 Background / Problem Statement

The platform has no configuration service yet: each gear owns its own configuration through config files, environment variables, or ad-hoc endpoints. Without a central service, every new gear adds another configuration surface with its own change model, no central audit trail, no staged-preview-then-apply safety net, and no per-tenant override behavior. Administrators cannot discover all configuration from one place or preview the blast radius of a change before it goes live, and tenants have no cascading-override model for configuring their own scope. Comparable platforms that grew this way (see survey below) show how costly it is to retrofit a unified configuration model later.

**Key problems solved**:

- **Configuration discoverability**: administrators cannot find or reason about all platform configuration from one place.
- **Unsafe changes**: without a staged-then-apply model, configuration changes take effect immediately and can destabilize a live platform.
- **Multi-tenant configuration**: configuration models in comparable platforms (see survey below) are single-tenant, retrofitted with folders rather than purpose-built cascading inheritance; this platform needs cascading inheritance from the start.
- **Type safety**: configuration values are not validated against a declared type before being stored or applied.
- **Compliance**: mutations to platform configuration are not consistently audited or gated behind credential verification.

> **Note**: Boundary between "a setting" and "a managed entity" (e.g. quotas/limits) — a value is a *setting* when it configures platform/service behaviour and has no independent lifecycle. Entities with their own lifecycle, quota/policy semantics, and API remain owned by their respective gears; settings can reference them via entity-reference traits but MUST NOT duplicate them.

**Neutral survey of comparable configuration/administration systems** (informal internal product research — observations from public product behaviour, not independently sourced or vendor-verified; patterns worth adopting, pitfalls worth avoiding): VMware vCloud Director offers per-tenant override as a first-class concept but a heavy, modal-driven UX with no global search or pre-apply preview. OpenNebula cleanly splits bootstrap/system config from runtime-adjustable settings, but keeps file-based and UI-based config as two sources of truth. Nutanix Prism aggregates configuration across clusters with good basic/advanced disclosure, but still requires CLI for some configuration. VMware vSphere makes inheritance visible via folder/group cascading, but retrofits a single-tenant model onto folders. Cloud org models (AWS Organizations, SCPs) offer effective-policy views analogous to effective-value-with-trace, but scatter configuration across many services.

**Differentiators** (cross-cutting principles applied throughout this PRD):

1. **Discoverability** — global cross-field search with match context; never force users to learn the category tree.
2. **Inheritance transparency** — always show effective source with a drill-into walk.
3. **Staged apply with preview** — show what changes, which services it touches, and which descendants it affects, before committing.
4. **Multi-tenancy by design** — model the tenant scope hierarchy natively; do not retrofit a single-tenant abstraction.
5. **One hub, not many** — all configuration in one place; embedded feature affordances read/write the same service.
6. **Inform, don't block** — surface consequences (cascading impact) as non-blocking warnings; reserve hard blocks for the irreversible.
7. **Type-safe & extensible** — GTS types + traits grow the configuration surface without code changes.

### 1.3 Goals (Business Outcomes)

- **One configuration hub, not many**: prevent configuration from fragmenting into per-gear surfaces as the platform grows. All platform configuration is discoverable, searchable, and governed in one place, reducing administrator error and onboarding time.
- **Safe change management**: a staged change → explicit apply model (for **value** changes) with a pre-apply preview and credential step-up prevents accidental, unreviewed, or partially-applied configuration changes to a live platform. Defining a setting is a separate authoring action: descriptive-metadata edits apply immediately, resolution-affecting fields (Schema Default, type, Scope Class) are immutable (changed only via a replacement declaration or new major type version), and retire/reactivate require credential step-up.
- **Multi-tenant by design**: tenants configure their own scope; values cascade down the tenant scope hierarchy, with full transparency about where an effective value came from. This is a differentiator versus single-tenant configuration models retrofitted onto folders.
- **Type-safe configuration**: every setting value is validated against a GTS type before it is stored or applied, catching invalid configuration at write time rather than in production.
- **Compliance-ready**: every mutation is audited (secrets masked), every apply is credential-verified, and visibility/override permissions are governed per setting.

**Success Metrics** (measured at GA unless noted):


| Goal                            | Measurable Success Criterion                                                                                                                | Target                                                                                                                                               |
| ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| One configuration hub, not many | Gears exposing their in-scope platform configuration through the Settings Service, vs. through gear-specific configuration surfaces         | 100% of in-scope platform configuration registered in the Settings Service at GA; zero standalone per-gear configuration surfaces introduced post-GA |
| Safe change management          | Value mutations that go through the staged-change → credential-verified Apply path, vs. any direct-write path that bypasses staging (declaration authoring is immediate by design and outside this metric) | 100% of value mutations staged-then-applied; 0 direct-write paths for values at GA                                                                                    |
| Multi-tenant by design          | Cascading settings resolvable via the inheritance walk with an exposed effective source                                                     | 100% of settings declared `cascading` support cascading inheritance with source trace at GA                                                          |
| Type-safe configuration         | Setting values rejected at write time when they fail their declared GTS type, vs. accepted and only failing later                           | 100% of writes validated against the declared GTS type before persistence; 0 untyped writes at GA                                                    |
| Compliance-ready                | Mutating operations that produce a complete audit record (actor, target, pre/post, timestamp, outcome, request id)                          | 100% of mutating operations audited; 0 audit-trail gaps found in the first post-GA compliance review                                                 |


### 1.4 Glossary


| Term                                    | Definition                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| --------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Setting (Key)**                       | A single configurable item with a unique key within its category, a typed value, an independent default, and metadata (mode, description, permissions, Scope Class, last-change timestamp).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| **Category**                            | A named, flat (single-level) grouping of settings — categories do not nest. Removable only when empty.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| **Effective Value**                     | The value in force for a setting at a given scope. For a `cascading` setting it is the override resolved via the inheritance walk if present, otherwise the Schema Default; `global` and `local` resolve differently — see the Scope Class resolution table ([§5.6](#56-multi-tenant-overrides--cascading-inheritance)).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| **Override**                            | A value explicitly set at a specific scope, taking precedence over inherited/default values for that scope and its non-overriding descendants.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| **Remove-Value**                        | A value operation that clears an explicit override at the target scope after Apply. Effective-value resolution then follows the setting's normal Scope Class rules. It does not retire the Setting Declaration.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| **Clone**                               | A value operation that stages the source scope's effective value as an explicit override at a target scope. The caller must be authorized to read and use the source effective value and to mutate the target scope. Clone does not copy the Setting Declaration or establish a continuing link to the source.                                                                                                                                                                                                                                                                                                                                                                                     |
| **GTS Type**                            | The schema-based type (JSON Schema core + `x-gts-traits`) a setting value must conform to. Owned by the platform's Types Registry.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| **Trait**                               | A semantic annotation on a GTS type that attaches behaviour/metadata beyond structural validation (e.g. `secret`, `multiline`, cron dialect, dynamic-enum source, entity-reference).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| **Mode**                                | Visibility/complexity level of a setting: **Standard** or **Advanced**. Orthogonal to administrative domain affinity.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| **Activation**                          | How an applied value reaches running consumers. The platform mechanism is **live-read (on next read / pull)** via the settings SDK, with cache invalidation on apply — no service disruption. A consumer that must do more than re-read (rebuild a pool, re-render its config, restart) performs that reaction **itself** on the change signal; the Settings Service never centrally reloads or restarts a consumer, and settings declare no per-setting "effect." Mechanics owned by DESIGN.                                                                                                                                                                                                    |
| **Staged / Pending Change**             | A **value** change (set / revert / remove-value / clone) marked as not-yet-applied; it does not affect running services until Apply. Descriptive-metadata declaration edits apply immediately (they change no effective value); behavior-affecting declaration fields are immutable (see Declaration Mutation Classes), so no declaration edit silently changes a live value.                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| **Apply**                               | The explicit, credential-verified operation that activates pending **value** changes in a scope; each change becomes effective by live-read (pull) with cache invalidation. A consumer needing a heavier reaction self-reacts (see Activation).                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| **Dependency Group**                    | An explicitly declared set of interdependent settings, with an associated cross-setting constraint, whose values **MUST** apply together atomically (all-or-nothing) so that partial application cannot leave the applied scope in an invalid combination. Declared per `cpt-cf-settings-service-fr-dependency-group-declaration` ([§5.5](#55-staged-change--apply)). A change not in any dependency group applies as an independent per-change unit.                                                                                                                                                                                                                                                                                                                                                                                                       |
| **Apply Revision**                      | An opaque token identifying the exact set of pending value changes an Apply acts on at a scope. It makes Apply idempotent (a retried Apply re-acts only on still-pending changes, never double-applying) and lets the service detect a stale/concurrent Apply against a superseded pending set ([§6.1 Reliability](#61-gear-specific-nfrs)). Token format and generation are owned by DESIGN.                                                                                                                                                                                                                                                                                                        |
| **Scope**                               | The point in the tenant hierarchy a setting value applies to: platform (root) or a specific tenant.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| **Scope Class**                         | A first-class, mandatory per-setting attribute that **deterministically derives** override and cascade behaviour (replacing manually-set booleans). One of: **global** (value lives only at platform scope; never tenant-overridable or inherited; tenant access governed solely by visibility), **cascading** (value inherits down the tenant hierarchy; overridable at any scope where permitted), **local** (value applies only at the scope where set; never inherited by descendants). Orthogonal to visibility (`tenant-visible`) and Mode. Secure-by-default: a setting must declare its class; infrastructure settings are `global` by declaration, not by remembering to disable a flag. |
| **Cascading Setting**                   | A setting of the **cascading** Scope Class (see Scope Class); descendants without their own override inherit the nearest ancestor's value.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| **Effective Source**                    | A computed indicator of where an effective value resolved from: own override, an ancestor's override, or the platform default.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| **Tenant-Visible / Tenant-Overridable** | Per-setting flags controlling whether a tenant may see / change a setting. Managed only by platform administrators.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| **Schema Default**                      | The type-declared default value of a setting, used when no override exists at any scope in the chain; independent of the current override and never destroyed by setting one. The canonical meaning of "default" in this service.                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| **Inherited Value**                     | An effective value resolved from an ancestor scope's override (distinct from an own override or the Schema Default).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| **Domain Affinity**                     | Optional per-setting attribute binding a setting to an administrative domain (e.g. infrastructure vs. commercial) so the hub shows only domain-relevant settings. Orthogonal to Standard/Advanced mode.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| **Last Change Timestamp**               | A per-setting attribute recording when the setting (its definition or its value at a scope) was last modified. Distinct from audit records; exposed on the admin single-setting read (not the consumer effective-value read) so clients can show recency.                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| **Setting Declaration**                 | The definition of a setting — key, GTS type, default, metadata, and Scope Class — distinct from its runtime value(s). A declaration is authored either by a platform administrator (admin-authored settings) or contributed by a gear (see Contributed Setting). Its fields split into **behavior-affecting fields** (Schema Default, GTS type, Scope Class), which change effective runtime resolution and are immutable after creation (see Declaration Mutation Classes), and **descriptive metadata** (description, Mode, Domain Affinity), which change no effective value.                                                                                                                                                                                                                                                                                                                                                                                                            |
| **Contributed Setting**                 | A setting whose Declaration is registered by an owning gear on install/upgrade (under a gear-namespaced key) rather than authored at runtime. Administrators may change its **value** (subject to permissions and Scope Class) but MUST NOT alter its Declaration. The declaration follows a register/retire lifecycle tied to gear install/upgrade/removal.                                                                                                                                                                                                                                                                                                                                      |
| **Declaration Mutation Classes**        | Two disjoint classes of declaration change. A **descriptive-metadata edit** (description, Mode, Domain Affinity) applies immediately and changes no effective value. A **behavior-affecting change** (Schema Default, GTS type, or Scope Class) is never applied in place: those fields are immutable on an existing declaration, and changing them requires a replacement declaration (new key) or, for type, a new major GTS type version — so a live setting's resolution can never change through an ungated declaration edit. **Retirement** (soft-delete) and **reactivation** are behavior-affecting authoring actions requiring credential step-up ([§5.7](#57-security-secrets--audit)). |
| **Namespaced Key**                      | A setting key prefixed by its owning gear's namespace so gear-contributed declarations never collide across gears.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |


## 2. Actors

### 2.1 Human Actors

#### Platform Administrator

**ID**: `cpt-cf-settings-service-actor-platform-admin`

- **Role**: Configures platform-scoped settings, manages categories and Setting Declarations' visibility/override permissions, governs Domain Affinity and Standard/Advanced views, applies staged changes, and manages per-setting tenant permissions.
- **Needs**: A single discoverable hub, a credential-verified apply preview, and full visibility into cascading impact before committing a change.

#### Tenant Administrator

**ID**: `cpt-cf-settings-service-actor-tenant-admin`

- **Role**: Configures settings within their own tenant scope and any descendant tenant in their subtree — cascading overrides where permitted, viewing inherited values and their source, and staging/applying changes within their delegated scope.
- **Needs**: A reduced, subtree-appropriate view that never exposes settings outside their visibility, with clear inheritance trace before overriding a value.

#### Compliance Reviewer

**ID**: `cpt-cf-settings-service-actor-compliance-reviewer`

- **Role**: Reviews the audit trail of settings mutations and machine secret-use events to answer "who changed what, when, and which service used which secret."
- **Needs**: A queryable, masked audit history per setting/scope and a global audit view; secret values remain masked in every audit record, with no human/administrative reveal path.

### 2.2 System Actors

#### Contributing Module

**ID**: `cpt-cf-settings-service-actor-contributing-module`

- **Role**: A gear that owns configuration and registers its Setting Declarations (namespaced key, GTS type, default, metadata, Scope Class) on install/upgrade, retiring them on removal; it does not change setting values at runtime.

#### Internal Service Caller

**ID**: `cpt-cf-settings-service-actor-internal-caller`

- **Role**: A restricted, token-authenticated internal caller invoking Internal Activation Endpoints (integrity-verified apply, tenant cache invalidation) as part of the Apply pipeline.

#### Tenant Resolver

**ID**: `cpt-cf-settings-service-actor-tenant-resolver`

- **Role**: Platform service that resolves the tenant scope hierarchy (ancestor/descendant relationships) that cascading-inheritance walks and server-side scope enforcement depend on.
- **Integration Direction**: Outbound — `tenant-resolver` does not call into Settings Service. The **hot cached read path does NOT call `tenant-resolver` per read**: Settings resolves cascading effective values against a **locally cached tenant-hierarchy snapshot** ([§6.1 Performance: Read-Path Caching](#61-gear-specific-nfrs)). Settings queries `tenant-resolver` **synchronously** only for (a) mutation/apply scope-enforcement, and (b) refreshing the hierarchy snapshot (bounded-TTL refresh or on a `tenant-resolver` change signal / cache miss) — not on every warm read.
- **Availability Expectation**: Required at request time for **hierarchy-changing and mutation/apply** operations and for a **cold** tenant read with no cached hierarchy: if `tenant-resolver` is unavailable, those **MUST** fail closed (rejected, never silently defaulted to platform scope). **Warm cached reads MUST continue to be served from the local hierarchy snapshot during a `tenant-resolver` blip**, so the read-path availability budget ([§6.1 Availability](#61-gear-specific-nfrs)) is **not** gated on `tenant-resolver`'s synchronous availability; platform-scoped operations are unaffected. Maximum tolerated snapshot staleness is owned by DESIGN within the freshness bound stated in [§6.1](#61-gear-specific-nfrs).

#### AuthN Resolver

**ID**: `cpt-cf-settings-service-actor-authn-resolver`

- **Role**: Platform authentication service that converts a bearer token into a validated identity for every Settings operation.
- **Integration Direction**: Outbound — Settings Service calls `authn-resolver`'s `authenticate()` synchronously on every operation; `authn-resolver` does not call into Settings Service.
- **Availability Expectation**: Required at request time. If `authn-resolver` is unavailable, every Settings operation **MUST** fail closed (rejected as unauthenticated), never processed unauthenticated.

#### AuthZ Resolver

**ID**: `cpt-cf-settings-service-actor-authz-resolver`

- **Role**: Platform authorization PDP that evaluates Subject+Action+Resource requests for every read/mutate/apply operation.
- **Integration Direction**: Outbound — Settings Service calls `authz-resolver`'s `evaluate()` synchronously per operation; `authz-resolver` does not call into Settings Service.
- **Availability Expectation**: Required at request time. If `authz-resolver` is unavailable, every Settings operation **MUST** fail closed (denied), per the PEP compiler's fail-closed guarantee.

#### License Resolver

**ID**: `cpt-cf-settings-service-actor-license-resolver`

- **Role**: Platform feature/licence entitlement service gating visibility of licence-restricted settings and categories. Currently aspirational (see [§10 Dependencies](#10-dependencies)).
- **Integration Direction**: Outbound — Settings Service queries `license-resolver` on read/search/list operations for gated settings; `license-resolver` does not call into Settings Service.
- **Availability Expectation**: Required for entitlement-gated reads only. If unavailable, gated settings **MUST** be excluded by default (fail closed to "not entitled"), not exposed.

#### Types Registry

**ID**: `cpt-cf-settings-service-actor-types-registry`

- **Role**: The platform `types-registry` gear. Owns the GTS type/trait definitions that Setting Declarations reference for validation and rendering.
- **Integration Direction**: Outbound — Settings Service resolves GTS type schemas from `types-registry` at declaration-authoring and value-validation time; `types-registry` does not call into Settings Service.
- **Availability Expectation**: Required at declaration-authoring and value-write time for type resolution/validation. If briefly unavailable, already-validated effective values continue to be servable from cache per [§6.1 Performance: Read-Path Caching](#61-gear-specific-nfrs).

## 3. Operational Concept & Environment

This gear depends on the platform's Types Registry (GTS types + traits), `tenant-resolver` (tenant scope hierarchy), `authn-resolver`/`authz-resolver` (authentication and authorization decisions), and the platform's persistence/audit facilities being available — tracked in [§10 Dependencies](#10-dependencies), not as an environment constraint. See `[docs/ARCHITECTURE_MANIFEST.md](../../../docs/ARCHITECTURE_MANIFEST.md)` and `[docs/GEARS.md](../../../docs/GEARS.md)` for the foundational runtime, lifecycle, and integration patterns shared by all gears.

No constraints beyond project defaults apply to this gear (no GPU/async-runtime/external-library deviation), so no Gear-Specific Environment Constraints subsection is included.

## 4. Scope

### 4.1 In Scope


| **Feature**                                    | **Priority** | **Notes**                                                                                                                                                                                                                                                |
| ---------------------------------------------- | ------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Settings & Category Model**                  | `p1`         | Declarative settings and categories; unique keys; create/remove a setting (descriptive-metadata edits apply **immediately**; behavior-affecting fields — Schema Default, GTS type, Scope Class — are **immutable**, changed only via a replacement declaration or new major type version; removal is a step-up-gated **soft-delete/retire** — the setting drops out of resolution but its values are **retained** and recoverable); no-orphan category deletion; effective-value semantics.                              |
| **Module-Contributed Settings**                | `p1`         | Gears contribute Setting Declarations (namespaced key, type, default, metadata, Scope Class) on install/upgrade; admins change values only; register/retire lifecycle; declaration/value separation.                                                     |
| **Setting Scope Class**                        | `p1`         | First-class global / cascading / local class; override & cascade behaviour derived deterministically from it (not manual booleans); visibility orthogonal. Secure-by-default for infrastructure settings.                                                |
| **Typed Values & Validation (GTS + traits)**   | `p1`         | GTS schema-based types with traits (via the platform's Types Registry); assert formats; structured values; type/traits discoverable for rendering.                                                                                                       |
| **Staged Change → Apply**                      | `p1`         | Applies to **value** changes: pending state, pending-changes view, apply preview, credential step-up; applying activates values by live-read (pull) with cache invalidation — a consumer needing more self-reacts; per-change reporting; interdependent settings apply atomically as a declared Dependency Group. Declaration changes are **not** value-staged: descriptive-metadata edits apply **immediately**, behavior-affecting fields are **immutable** (replacement declaration / new major type version), and removal is a **step-up-gated** soft-delete. |
| **Multi-Tenant Overrides & Scope Enforcement** | `p1`         | Cascading vs. global values (per Scope Class); set/clone/remove tenant overrides at any tenant within the caller's subtree (own tenant or a descendant); server-side subtree enforcement (via `tenant-resolver`); visibility-gated reads.                |
| **Cascading Inheritance**                      | `p1`         | Inheritance walk through the tenant hierarchy (for cascading settings, resolved via `tenant-resolver`); effective source/trace; override-at-any-level; local/global opt-outs via Scope Class; non-blocking downstream-impact warning.                    |
| **Per-Setting Tenant Permissions**             | `p1`         | `tenant-visible` / `tenant-overridable` flags; platform-admin managed; no leakage through any read path.                                                                                                                                                 |
| **Security, Secrets & Audit**                  | `p1`         | AuthN + access-level gating (via `authn-resolver`/`authz-resolver`); secret encrypt-at-rest + masking on all administrative paths; plaintext resolvable only via the authenticated machine-only runtime path; step-up before apply; audit of all mutations and of machine secret-use; feature/licence gating (via `license-resolver`).                              |
| **Standard/Advanced Mode**                     | `p2`         | Per-user persisted mode; mode-filtered reads; legible truncation of hidden advanced settings.                                                                                                                                                            |
| **Search & Discoverability**                   | `p2`         | Cross-field search (key/description/value/category) respecting scope, mode, and visibility; secrets never indexed/matched; PII authorization applied before matching.                                                                                                                                       |
| **Defaults & Revert**                          | `p2`         | Independent Schema Defaults; revert at tenant scope (to nearest ancestor) and platform scope (to Schema Default), with fallback preview.                                                                                                                 |
| **Domain Affinity Filtering**                  | `p3`         | Optional per-setting domain affinity; hub filters categories by the admin's current domain; cross-domain hidden by default with an "All domains" platform-admin view.                                                                                    |
| **Internal Activation Endpoints**              | `p3`         | Restricted, token-only inter-service operations (integrity-verified apply, tenant cache invalidation).                                                                                                                                                   |


*Sorting order: priority (`p1` → `p2` → `p3`).*

### 4.2 Out of Scope

- **Cross-region settings replication**: multi-region propagation of configuration is deferred; v1 targets the single control-plane scope.
- **Ancestor-level batch apply across descendant tenants**: v1 supports per-scope/per-tenant apply only.
- **Edition Defaults (per-edition baseline value sets)**: bulk-setting curated per-edition default values and then applying them is deferred to a later iteration; v1 ships Schema Defaults only. Candidate follow-up.
- **Export/import of settings manifests**: backup/migration/onboarding via settings export-import is deferred to a later iteration covering backup/DR and tenant onboarding.
- **Settings visual design**: UI/visual specification is not addressed by this PRD; interface expectations are captured only at the Use Case level.
- **GTS type authoring and the Types Registry itself**: owned by the platform's Types Registry (`gears/system/types-registry`); this PRD consumes GTS types, it does not define the registry.
- **Boot-critical infrastructure configuration**: the Settings Service manages runtime configuration only. Anything a gear needs to start *before* the Settings Service is reachable (database and broker endpoints, service identity, platform TLS, ports, domain) is owned by deployment tooling, not managed as a setting; registering it here would create a dependency cycle.
- **Managed-resource desired-state**: per-resource spec/status reconciliation is owned by whichever gear manages that resource's independent lifecycle; this service governs platform configuration, not managed resources.
- **Central reload/restart orchestration**: the Settings Service never reloads or restarts a consumer. A consumer that needs more than a live re-read self-reacts on the change signal — including restarting **itself** (it exits; its supervisor restarts it, and it reads the current value on start). Coordinated **rolling** restart across a gear's replicas (to avoid simultaneous downtime) is a deployment/rollout concern, out of scope.
- **Settings-owned config templating**: a consumer whose runtime config is a rendered file re-renders it **itself** as part of self-react; the Settings Service ships no template/rendering engine and owns no consumer's config artifacts.
- **API schemas, data models, and error taxonomies**: owned by the downstream DESIGN document; this PRD defines WHAT/WHY only.
- **End-user (per-user) preferences**: `docs/GEARS.md`'s "Settings Service" entry describes "typed configuration and preferences at **tenant/user scope**" with per-user CRUD as a p1 scenario. This PRD deliberately narrows that definition to **platform and tenant scope only** — no actor, glossary term, or FR in this document covers a per-user preference slice. That slice is already implemented and owned by `gears/simple-user-settings`, a separate gear under a different name. Downstream readers of `docs/GEARS.md`'s Settings Service entry should treat `gears/simple-user-settings` as the owner of its user-scope portion and this gear as the owner of its platform/tenant-scope portion.

## 5. Functional Requirements

> **Testing strategy**: All requirements verified via automated tests (unit, integration, e2e) targeting 90%+ code coverage unless otherwise specified.

### 5.1 Settings & Category Model

#### Category and Setting Lifecycle

- [ ] `p1` - **ID**: `cpt-cf-settings-service-fr-settings-category-model`

The system **MUST** let a platform administrator create a named, flat, single-level category with a unique name and description; creating a category with a duplicate name **MUST** be rejected with a clear error. The system **MUST** reject removal of a category that still contains one or more settings; removal **MUST** succeed only when the category is empty. The system **MUST** let an administrator create a setting under an existing category with a key (unique within its category), type, default value, mode, and description; creating a setting under a non-existent category, or with a duplicate key, **MUST** be rejected.

Once a setting is created, its **descriptive metadata** (description, Mode, Domain Affinity, and — for admin-authored settings — visibility/override flags) **MUST** be editable in place with immediate effect, because such edits change no effective value. Its **behavior-affecting fields** (Schema Default, GTS type, Scope Class) **MUST** be immutable: the system **MUST** reject an in-place edit of any of them and **MUST** require the change to be expressed as a **new declaration** (a distinct key) or, for the GTS type, a **new major type version** ([§5.2](#52-typed-values--validation)), so that no ungated edit can alter a live setting's effective resolution. Removing a setting is a **soft-delete (retire)** and, together with reactivation, is a behavior-affecting authoring action that **MUST** require credential step-up ([§5.7](#57-security-secrets--audit)); the setting drops out of resolution at once, but its values are **retained** and recoverable by reactivation.

- **Rationale**: A predictable category/setting lifecycle with no-orphan deletion and uniqueness guarantees is the structural foundation the rest of the service (typed values, staged apply, cascading overrides) depends on. Making the resolution-affecting fields immutable — rather than editable in place — keeps the staged-apply safety model honest: a change that would alter live behaviour cannot slip in through a declaration edit that bypasses Apply.
- **Actors**: `cpt-cf-settings-service-actor-platform-admin`

### 5.2 Typed Values & Validation

#### GTS Type Validation, Trait Discovery & Secret Protection

- [ ] `p1` - **ID**: `cpt-cf-settings-service-fr-typed-value-validation`

The system **MUST** validate every setting value (override or default) against its declared GTS type at creation or change time and **MUST** reject invalid values with a clear, field-level error; `format` keywords (e.g. `uri`, `ipv4`/`ipv6`) and trait-driven rules (e.g. cron dialect, regex-compiles) **MUST** be asserted, not treated as advisory. The system **MUST** include, or let a client resolve, a setting's type and resolved trait set when read, so the client can render the appropriate input and pre-validate; structured (object/array) values **MUST** be supported, not only scalar values. A setting whose type carries the `secret` trait **MUST** be stored encrypted at rest and **MUST** be masked in every **administrative** read, search, or audit response; its plaintext **MUST NOT** be retrievable through any administrative or human-facing path (single get, bulk get, search, list-by-category, audit). Plaintext secret resolution **MUST** be available only through one explicit, authenticated **machine-only runtime path** — the Settings Read SDK / internal in-process reader ([§7.1](#71-public-api-surface)) — and only for a consuming service authorized to that specific setting; every such plaintext resolution **MUST** be recorded as a secret-use audit event ([§5.7](#57-security-secrets--audit)) and its value **MUST NOT** be cached in plaintext ([§6.1](#61-gear-specific-nfrs)).

> **Note**: Minor/compatible GTS type changes auto-resolve. A type change that invalidates an existing override flags that override at the affected scope as `needs-review`, excludes it from Apply until corrected, and surfaces a migration prompt — no silent auto-migration. Breaking changes require a new major type version and explicit re-validation (mechanics owned by DESIGN together with the platform Types Registry's compatibility rules).

> **Note on data classification**: `secret` is not the only sensitivity classification this service must recognize. Module-Contributed Settings ([§5.8](#58-module-contributed-settings)) let any gear register a GTS-typed value that carries PII (e.g., an alerting-contact email) without it being a `secret`. Setting values **MUST** use at least these classifications: **public** (no special handling), **PII** (visible unmasked only to callers authorized for unmasked PII; masked in all other administrative reads and audit/report outputs; governed by the platform's retention/anonymization policy), and **secret** (encrypted at rest, masked on all administrative/human paths with no human reveal path, plaintext only via the machine-only runtime path, as above). These outputs do not include settings-manifest export/import, which is out of scope ([§4.2](#42-out-of-scope)). The concrete classification mechanism is owned by DESIGN; this PRD requires the classification to exist and be enforced.

- **Rationale**: Catching invalid configuration at write time, giving clients enough type/trait information to render safely, and guaranteeing secrets are never exposed in plaintext are the baseline trust guarantees for a shared configuration surface.
- **Actors**: `cpt-cf-settings-service-actor-platform-admin`, `cpt-cf-settings-service-actor-tenant-admin`

### 5.3 Standard/Advanced Mode & Discoverability

#### Mode-Filtered Reads

- [ ] `p2` - **ID**: `cpt-cf-settings-service-fr-standard-advanced-mode`

The system **MUST** tag every setting and category as Standard or Advanced. When a client reads categories or settings for Standard mode, Advanced-only settings and categories **MUST** be excluded; the user's mode preference **MUST** persist per user, not per session. When a category has hidden Advanced-only settings, reads **MUST** expose the count of hidden settings so clients can indicate them rather than silently omitting them.

- **Rationale**: Progressive disclosure keeps the hub usable for everyday administrators while still exposing full depth to advanced operators, without ever hiding the fact that more settings exist.
- **Actors**: `cpt-cf-settings-service-actor-platform-admin`, `cpt-cf-settings-service-actor-tenant-admin`

#### Cross-Field Search

- [ ] `p2` - **ID**: `cpt-cf-settings-service-fr-search-discoverability`

The system **MUST** let an administrator search settings by key, description, value, or category name and **MUST** return matches as a flat list with category breadcrumbs and an indication of which field matched. Search **MUST** respect the same scope, mode, and tenant-visibility filters as browsing.

Value search **MUST** obey these privacy rules, which go beyond omitting values from output — because match existence, result counts, snippets, and timing can themselves leak content:

- **Search corpus**: value matching covers **Schema Defaults and overrides the caller is authorized to read in the requested scope** (per visibility, Scope Class, and licence gating). It **MUST NOT** match against any value the caller cannot read, and it does not index effective values that the caller could not otherwise retrieve.
- **Secrets**: `secret`-trait values **MUST NOT** be indexed or matched on at all. A secret **MUST NOT** be discoverable through match existence, counts, snippets, or timing — searching secret content is unsupported, not merely masked in output.
- **PII**: PII value content **MUST** have PII authorization applied **before matching**. A caller not authorized for unmasked PII **MUST NOT** match on PII value content, and such content **MUST NOT** appear in any snippet, matched-field indicator, or count returned to that caller.
- **Structured values**: search over structured (object/array) values matches leaf values subject to the same classification rules (secret leaves never matched; PII leaves matched only under PII authorization).

- **Rationale**: A single, global search is the primary discoverability mechanism that lets administrators reach any setting without learning the category tree — but because searching over values can leak secret or PII content indirectly (through whether a match exists, how many, or a returned snippet), the corpus and matching itself must be classification- and authorization-aware, not just the response payload.
- **Actors**: `cpt-cf-settings-service-actor-platform-admin`, `cpt-cf-settings-service-actor-tenant-admin`

### 5.4 Defaults & Revert

#### Revert to Ancestor or Schema Default

- [ ] `p2` - **ID**: `cpt-cf-settings-service-fr-defaults-revert`

The system **MUST** maintain an independent Schema Default per setting that is never destroyed by setting an override. When an administrator reverts a tenant-scope override of a cascading setting, the system **MUST** clear the local override, fall the effective value back to the nearest ancestor's override (or the platform default if none), and **MUST** communicate the resulting fallback value before the revert is committed. When a platform administrator reverts a platform-level override, the system **MUST** clear it and fall the effective value back to the schema-declared default, which **MUST** remain intact and independent of the override throughout.

> **Note**: Settings do not carry per-setting version history or one-action rollback to a previous applied value in v1. Operational safety before activation is provided by the staged-change → credential-verified Apply model; after apply, the supported corrections are revert-to-Schema-Default (platform scope) and revert-to-ancestor (tenant scope), with the prior value recoverable from the audit trail. Per-setting version history + rollback is a candidate follow-up.

- **Rationale**: A clear, independent default and predictable revert-to-ancestor/default behavior let administrators undo a bad override without guessing where the value will land.
- **Actors**: `cpt-cf-settings-service-actor-platform-admin`, `cpt-cf-settings-service-actor-tenant-admin`

### 5.5 Staged Change → Apply

#### Staged Changes with Pending Indicator

- [ ] `p1` - **ID**: `cpt-cf-settings-service-fr-staged-change-pending`

A **value** operation on a setting — set, revert, remove-value, or clone (at platform or any tenant scope) — **MUST** mark it pending and **MUST NOT** affect running services until an explicit Apply. Clone **MUST** require authorization to read and use the source effective value and to mutate the target scope. Clone and remove-value **MUST** otherwise satisfy the same target-scope, validation, and Apply requirements as set and revert. A persistent pending-change indicator **MUST** reflect the count of pending value changes in the user's visible scope. The pending-changes view **MUST** show, per pending change, category, key, old value (or "default"), new value, and who staged it, and **MUST** let the administrator discard individual or bulk pending changes without applying them.

Operations on a **setting Declaration itself** are **NOT** value-staged, but they **MUST** be split by their effect on live behaviour (see Declaration Mutation Classes, [§1.4](#14-glossary)). A **descriptive-metadata edit** (description, Mode, Domain Affinity) takes effect immediately and needs no gate because it changes no effective value. A **behavior-affecting change** (Schema Default, GTS type, Scope Class) **MUST NOT** be applied in place: those fields are immutable, and the change **MUST** be expressed as a new declaration or a new major GTS type version ([§5.1](#51-settings--category-model), [§5.2](#52-typed-values--validation)), so a live setting's resolution can never change through an ungated edit. Removing a setting is a **soft-delete (retire)**, and — with reactivation — is a behavior-affecting action that **MUST** require credential step-up ([§5.7](#57-security-secrets--audit)); the retired setting drops out of resolution at once, but its values are **retained** and recoverable by reactivation.

- **Rationale**: Decoupling "change" from "effect" is the core safety guarantee against accidental or unreviewed configuration changes to a live platform. A declaration edit that only touches descriptive metadata has no in-effect value to gate, so it applies immediately; a declaration change that would alter resolution (default, type, Scope Class, or retirement) is gated — by immutability plus a replacement-declaration path, or by credential step-up for retire/reactivate — instead of silently bypassing Apply.
- **Actors**: `cpt-cf-settings-service-actor-platform-admin`, `cpt-cf-settings-service-actor-tenant-admin`

#### Apply Preview with Credential Step-Up

- [ ] `p1` - **ID**: `cpt-cf-settings-service-fr-apply-preview-stepup`

When an administrator initiates Apply on pending changes, the system **MUST** present a preview listing the pending changes (category, key, old → new value, scope, who staged), making clear that applying activates values by live-read (pull) with no service disruption by default. The preview **MUST** require credential re-verification (step-up) before proceeding.

- **Rationale**: A credential-verified preview is the last checkpoint before a change reaches a live platform; it must be impossible to apply by surprise.
- **Actors**: `cpt-cf-settings-service-actor-platform-admin`, `cpt-cf-settings-service-actor-tenant-admin`

#### Apply Activates Values via Live-Read; Consumers Self-React

- [ ] `p1` - **ID**: `cpt-cf-settings-service-fr-apply-effect-resolution`

Activating a change **MUST NOT** require the Settings Service to reload or restart any consumer: the new value becomes effective and each consumer reads it **on next read (live-read / pull)** via the settings SDK, with cache invalidation. The Settings Service **MUST NOT** declare or resolve a per-setting "effect"; live-read is the platform's activation mechanism. A consumer that must do more than re-read a changed value (rebuild a pool, re-render its config file, or restart) **MUST** perform that reaction **itself**, triggered by the change signal, reacting only to the settings it consumes (per setting, not a blanket per-category restart); the Settings Service **MUST NOT** reload or restart a consumer on its behalf, and a consumer that cannot apply a change in place restarts **itself** and reads the current value on start. Coordinated rolling restart across a gear's replicas is a deployment/rollout concern, out of scope. The Apply result **MUST** report per change: old → new value, scope, and success/failure; pending flags **MUST** be cleared only for successfully applied changes (a live-read change clears once its cache invalidation is issued); a failed or partially-failed Apply **MUST** leave the unapplied changes pending for retry.

**Atomicity, success, and ordering.** Apply is **deliberately non-atomic across independent changes**: each independent change succeeds or fails on its own and failed items remain pending. However, interdependent settings **MUST** be groupable into a **Dependency Group** (declared per `cpt-cf-settings-service-fr-dependency-group-declaration`) that applies **atomically** (all-or-nothing), and an Apply **MUST** validate the **resulting configuration** of the applied scope before committing a group; an Apply that would leave the scope in an invalid combination (violating a declared cross-setting constraint) **MUST** be rejected for that group and leave it pending, rather than committing a partial, invalid combination. A change is **"successfully applied"** only when its new value is **durably persisted** *and* the applied scope's cache invalidation has been issued (with descendant cache-invalidation emitted for cascading settings); the service **MUST** persist durably **before** invalidating cache or signaling consumers, so no consumer can observe an invalidation for a value that is not yet stored. Apply **MUST** be **idempotent**, keyed by an **Apply Revision** ([§1.4](#14-glossary)): a retried Apply re-acts only on still-pending changes and **MUST NOT** double-apply an already-applied change, and an Apply against a superseded pending set **MUST** be detected and reported rather than silently overwriting ([§6.1 Reliability](#61-gear-specific-nfrs)). Revision-token format, persistence/invalidation transaction mechanics, and retry backoff are owned by DESIGN.

> **Note**: Apply activates **the applied scope only** — the scope being applied to (the acting admin's own tenant, or a targeted descendant within its subtree per `cpt-cf-settings-service-fr-tenant-scope-enforcement`); that scope's consumers self-react (live-read, or their own heavier reaction). Scopes *below* the applied scope are **not** reloaded or restarted — an apply of a cascading setting **MUST** emit cache-invalidation for the affected descendant scopes (via the Internal Activation Endpoints), so those descendants re-resolve the new effective value lazily on next read rather than reading stale values. Cross-scope reaction fan-out is out of scope for v1; invalidation mechanics → DESIGN.

- **Rationale**: In a supervised platform, reload / regenerate / restart all collapse into self-react: a consumer re-renders its own config or restarts itself (exit → supervisor restarts → reads the current value on boot), so a central per-setting effect and platform-driven reload/restart are redundant, not primitive-blocked.
- **Actors**: `cpt-cf-settings-service-actor-platform-admin`, `cpt-cf-settings-service-actor-tenant-admin`, `cpt-cf-settings-service-actor-internal-caller`

#### Dependency Group and Cross-Setting Constraint Declaration

- [ ] `p1` - **ID**: `cpt-cf-settings-service-fr-dependency-group-declaration`

The system **MUST** let a declaration author declare a **Dependency Group** — a named set of interdependent settings with an associated **cross-setting constraint** over their combined values — so that the atomic, all-or-nothing Apply and resulting-configuration validation required by `cpt-cf-settings-service-fr-apply-effect-resolution` have an explicit definition to enforce. A Dependency Group over **admin-authored** settings **MUST** be declarable by a platform administrator; a Dependency Group over a gear's **contributed** settings **MUST** be declarable by that owning gear on install/upgrade ([§5.8](#58-module-contributed-settings)). A Dependency Group definition and its cross-setting constraint are **behavior-affecting**: they **MUST** follow the same immutability rule as other behavior-affecting declaration fields ([§5.1](#51-settings--category-model)) — changed only via a replacement declaration, never edited in place — so a live scope's validity rules cannot change through an ungated edit. A setting not in any Dependency Group applies as an independent per-change unit.

- **Rationale**: The atomic-apply and invalid-combination-rejection guarantees in `cpt-cf-settings-service-fr-apply-effect-resolution` are only enforceable if the interdependencies and constraints they check are explicitly declared and owned; leaving the authoring side undefined would make the atomicity requirement unenforceable.
- **Actors**: `cpt-cf-settings-service-actor-platform-admin`, `cpt-cf-settings-service-actor-contributing-module`

### 5.6 Multi-Tenant Overrides & Cascading Inheritance

#### Setting Scope Class Governs Cascade/Override Behaviour

- [ ] `p1` - **ID**: `cpt-cf-settings-service-fr-setting-scope-class`

Every setting **MUST** declare a Scope Class (global | cascading | local), and cascade/override behaviour **MUST** be derived deterministically from it: **global** **MUST NOT** be tenant-overridable nor inherited by tenants; **cascading** **MUST** inherit down the tenant scope hierarchy with overrides at permitted scopes; **local** **MUST** apply only at the scope where set and **MUST NOT** be inherited by descendants. Cascade/override behaviour **MUST NOT** depend on independently-set booleans that can be forgotten.

- **Rationale**: Deriving behaviour from one mandatory, declared attribute (rather than several independently-toggleable booleans) makes infrastructure settings secure-by-default and removes an entire class of misconfiguration risk.
- **Actors**: `cpt-cf-settings-service-actor-platform-admin`

The effective value and tenant read behaviour for each Scope Class **MUST** follow this resolution table (visibility gating from `tenant-visible` applies on top; a setting not visible to a tenant is never exposed):

| Scope Class | Platform scope, no platform override | Platform scope, with platform override | Tenant scope, no own override | Tenant scope, with own override | Tenant read (when `tenant-visible`) |
| ----------- | ------------------------------------ | -------------------------------------- | ----------------------------- | ------------------------------- | ----------------------------------- |
| **global** | Schema Default | Platform override value | — (never resolved through the tenant's ancestor chain) | — (override **rejected**: global is never tenant-overridable, regardless of any flag) | The **platform-scope effective value**, **read-only** (this is the value a tenant "receives"); never inherited, never overridable |
| **cascading** | Schema Default | Platform override value | Nearest ancestor override walking up; else the platform value; else Schema Default (exposed as Inherited Value + effective source) | The tenant's own override (applies to it and its non-overriding descendants) | The resolved value above; changeable only when `tenant-overridable` |
| **local** | Schema Default | Platform-scope local value (**applies only at platform scope; NOT inherited by tenants**) | **Schema Default** (local never inherits an ancestor/platform value) | The tenant's own local value (applies **only at that tenant**, not its descendants) | Its own local value if set, else Schema Default; changeable only when `tenant-overridable` |

Read the table with these clarifications: (1) a `global` setting a tenant can see surfaces the **platform value read-only** — "not inherited" means it is not resolved through the tenant's ancestor chain and cannot be overridden, not that the tenant sees the Schema Default; (2) a `local` setting **can** carry a platform value, but that value stays at platform scope and every tenant without its own local override resolves to the **Schema Default**; (3) `tenant-visible` governs whether a tenant sees a setting at all and `tenant-overridable` whether it may set its own value — for `global`, `tenant-overridable` has no effect (override is always rejected).

#### Tenant Override of a Cascading Value

- [ ] `p1` - **ID**: `cpt-cf-settings-service-fr-tenant-overrides`

Given a `cascading` setting with a platform-level value, when a value is set for that setting at a target tenant (the caller's own tenant or a descendant within its subtree), the value **MUST** override the inherited value **at that target tenant** and for that tenant's non-overriding descendants. Setting, cloning, and removing tenant overrides **MUST** follow the same authorization, target-scope, validation, and staged-then-apply requirements as platform-scoped value changes. These operations **MUST** be permitted only for an authorized target tenant within the caller's subtree and only when the setting is `tenant-overridable`.

- **Rationale**: Per-tenant override is the mechanism that lets tenants configure their own scope without platform administrator involvement for every change.
- **Actors**: `cpt-cf-settings-service-actor-tenant-admin`

#### Cascading Inheritance with Source Trace and Impact Warning

- [ ] `p1` - **ID**: `cpt-cf-settings-service-fr-cascading-inheritance`

For a cascading setting at a tenant scope, the system **MUST** resolve the effective value by walking up the tenant scope hierarchy — against the locally cached hierarchy snapshot on the warm read path, refreshed from `tenant-resolver` within a bounded freshness window ([§6.1](#61-gear-specific-nfrs)) — to the first override, else the platform default, and the read API **MUST** expose the effective source / inheritance trail so clients can show where the value came from. When an administrator is about to change a cascading setting at scope X and descendants of X would have their effective value changed, the system **MUST** report a non-blocking warning listing affected descendants with current vs. new effective values; the administrator **MUST** be able to proceed — the service informs, it does not block.

- **Rationale**: Inheritance transparency and non-blocking impact warnings are a stated differentiator versus competing configuration systems where the origin of a value is unclear.
- **Actors**: `cpt-cf-settings-service-actor-platform-admin`, `cpt-cf-settings-service-actor-tenant-admin`

#### Tenant Permission Enforcement and Subtree Scope Isolation

- [ ] `p1` - **ID**: `cpt-cf-settings-service-fr-tenant-scope-enforcement`

Given the per-setting `tenant-visible` and `tenant-overridable` flags, when a tenant administrator reads or changes settings in their scope, only tenant-visible settings **MUST** be returned through any read path, and only tenant-overridable settings **MUST** be changeable; these flags **MUST** be managed only by platform administrators, and tenants **MUST NOT** change their own visibility/override permissions. Visibility is orthogonal to Scope Class: a **global** setting **MUST NOT** be tenant-overridable regardless of any flag, but it can be `tenant-visible` (read-only) to tenants. Given a caller who is a tenant administrator, when any operation is performed against a target tenant, the target tenant **MUST** be within the caller's own **subtree** (the caller's own tenant or any descendant), enforced server-side regardless of client-supplied filters; a target outside the subtree **MUST** be rejected. Setting a value at a descendant tenant **MUST** create the override **at that descendant** (not at the caller's own tenant); the operation follows the same staged-then-apply model. The caller **MUST NOT** modify platform-scoped values, any ancestor's values, or any tenant outside its subtree, and **MUST NOT** read any setting not visible to its scope. Reads are gated by **visibility, not by Scope Class**: a **global** setting marked `tenant-visible` **MUST** be readable **read-only** by the tenant, while any setting not visible to the tenant **MUST NOT** be exposed through any path (single get, bulk get, search, list-by-category).

- **Rationale**: Per-setting visibility/override flags plus server-side subtree enforcement (via `tenant-resolver`) are the two guarantees that make multi-tenant delegation safe without relying on client-side filtering.
- **Actors**: `cpt-cf-settings-service-actor-platform-admin`, `cpt-cf-settings-service-actor-tenant-admin`

### 5.7 Security, Secrets & Audit

#### Authentication and Access-Level Gating

- [ ] `p1` - **ID**: `cpt-cf-settings-service-fr-authn-role-gating`

Any Settings operation **MUST** require a valid authenticated session/token (via `authn-resolver`) and **MUST** be gated by an authorization decision via `authz-resolver`: view-level access permits read, owner/admin-level access permits mutate/apply. Apply operations, and behavior-affecting declaration actions (retire/reactivate, [§5.1](#51-settings--category-model)), **MUST** additionally require credential step-up re-verification. Credential step-up **MUST** be a **pluggable behaviour** behind a stable step-up contract: the Settings Service **MUST** depend only on that contract and **MUST NOT** hard-code any single second-authentication implementation. A default second-authentication gear is provided by the platform to satisfy the contract; an integrator can replace it with their own step-up mechanism (e.g. a different MFA/second-factor provider) behind the same contract without changing the Settings Service.

> **Note**: Neither dependency currently exposes the primitive this FR assumes. `authn-resolver`'s entire public API today is a single `authenticate(bearer_token)` call (ADR-0003's deliberately minimalist interface) with no re-authentication/step-up method. `authz-resolver`'s entire public API today is an AuthZEN Subject+Action+Resource decision (`list`/`get`/`create`/`update`/`delete`) with no built-in role storage or hierarchy — role/permission binding is explicitly deferred to a future AuthZ Management Gear (`docs/arch/authorization/PERMISSION_GTS_TYPE.md`). Both the credential step-up mechanism and the view/owner/admin access-level model are **new platform capability that does not exist yet** and are **blocking launch prerequisites** ([§10.1](#101-launch-prerequisites-blocking)); see [§13 Open Questions](#13-open-questions) for ownership and timing.

- **Rationale**: Settings govern platform behaviour; an unauthenticated or under-privileged caller must never be able to read or change them.
- **Actors**: `cpt-cf-settings-service-actor-platform-admin`, `cpt-cf-settings-service-actor-tenant-admin`

#### Audited Mutations and Secret Reveals

- [ ] `p1` - **ID**: `cpt-cf-settings-service-fr-audit-mutations`

Any mutating operation (create/change/revert/remove/apply/clone) **MUST** write an audit record with actor, target (category + key + scope), pre/post values (secrets masked), timestamp, outcome, and a request identifier; audit history **MUST** be queryable globally and per (setting, scope), with secrets masked and no reveal path.

> **Note**: A **machine secret-use** — a plaintext secret resolution through the machine-only runtime path ([§5.2](#52-typed-values--validation), [§7.1](#71-public-api-surface)) — **MUST** be recorded as a security audit event (resolving service identity, target setting + scope, timestamp, request id) with the value still masked in the record. This is the only path that yields plaintext; there is no human/administrative reveal path. Routine (non-secret) effective-value reads are not audited, to bound log volume. Retention is configurable (see [§6.1 Scale & Growth](#61-gear-specific-nfrs)).

Audit-record actor identities **MUST** be classified as public or PII. Actor-identity PII **MUST** be visible unmasked only to callers authorized for unmasked PII, **MUST** be masked in all other administrative audit reads and audit/report outputs, and **MUST** remain governed by the platform's retention/anonymization policy. These outputs do not include settings-manifest export/import, which is out of scope ([§4.2](#42-out-of-scope)); the classification mechanism is owned by DESIGN.

- **Rationale**: A complete, tamper-evident audit trail — including secret reveals — is required for compliance review of who changed or accessed configuration and when.
- **Actors**: `cpt-cf-settings-service-actor-platform-admin`, `cpt-cf-settings-service-actor-tenant-admin`, `cpt-cf-settings-service-actor-compliance-reviewer`

#### Feature/Licence Gating

- [ ] `p2` - **ID**: `cpt-cf-settings-service-fr-feature-license-gating`

Given settings or categories whose visibility is restricted by a licence/feature flag (via `license-resolver`), when read, search, or list operations run, gated settings **MUST** be excluded for callers without the entitlement, enforced server-side consistently across all **administrative read paths** (UI/API browse, search, list-by-category).

> **Note**: The **internal in-process settings reader** used by platform gears to resolve effective configuration is NOT licence-gated: gears **MUST** receive effective values regardless of a tenant's UI entitlement, because licence gating governs administrative visibility, not runtime configuration resolution.

- **Rationale**: Feature/licence-gated settings must never leak to callers without the corresponding entitlement, regardless of which read path they use; but a gear's own configuration resolution must not silently fail closed just because a tenant lacks a UI entitlement.
- **Actors**: `cpt-cf-settings-service-actor-platform-admin`, `cpt-cf-settings-service-actor-tenant-admin`

### 5.8 Module-Contributed Settings

#### Gears Contribute Declarations; Admins Change Values

- [ ] `p1` - **ID**: `cpt-cf-settings-service-fr-module-contributed-declarations`

When a gear that owns configuration is installed or upgraded, it **MUST** be able to contribute its Setting Declarations (namespaced key, GTS type, default, metadata, Scope Class) to the Settings Service, and administrators **MUST** be able to change the contributed settings' **values** (subject to permissions and Scope Class) but **MUST NOT** alter the Declarations. Contributed keys **MUST** be namespaced to their owning gear to prevent collisions.

- **Rationale**: Separating declaration ownership (gear) from value ownership (administrator) lets the configuration surface grow without core changes while keeping administrators in control of runtime behaviour.
- **Actors**: `cpt-cf-settings-service-actor-contributing-module`, `cpt-cf-settings-service-actor-platform-admin`

#### Contributed-Declaration Register/Retire Lifecycle

- [ ] `p1` - **ID**: `cpt-cf-settings-service-fr-contributed-lifecycle`

When a gear registers, upgrades, or retires a declaration, the Settings Service **MUST** reconcile the declaration set (add / update descriptive metadata / mark-retired), preserving administrator-set values across compatible upgrades. A gear upgrade **MUST NOT** change a contributed declaration's behavior-affecting fields (Schema Default, GTS type, Scope Class) in place; such a change **MUST** be carried as a new major GTS type version or a replacement (re-namespaced) declaration, following the same immutability rule as admin-authored declarations ([§5.1](#51-settings--category-model)). Declarations that would invalidate existing values **MUST** follow the GTS type-versioning policy ([§5.2](#52-typed-values--validation)); the lifecycle of values on full gear removal is an open question ([§13 Open Questions](#13-open-questions)).

- **Rationale**: Gears install, upgrade, and are removed independently of administrator activity; the declaration lifecycle must reconcile safely without silently discarding administrator-set values.
- **Actors**: `cpt-cf-settings-service-actor-contributing-module`

### 5.9 Additional Scope Items

#### Domain Affinity Filtering

- [ ] `p3` - **ID**: `cpt-cf-settings-service-fr-domain-affinity-filtering`

The system **MUST** support an optional Domain Affinity per setting (not every setting carries one); the hub **MUST** filter categories by the administrator's current domain. Cross-domain settings **MUST** be hidden by default (not read-only); a platform administrator **MUST** be able to switch to an "All domains" view. Domain Affinity **MUST** be orthogonal to Standard/Advanced mode.

- **Rationale**: Domain-affinity filtering keeps administrators focused on settings relevant to their administrative domain without permanently hiding cross-domain configuration from those who need it.
- **Actors**: `cpt-cf-settings-service-actor-platform-admin`

#### Internal Activation Endpoints

- [ ] `p3` - **ID**: `cpt-cf-settings-service-fr-internal-activation-endpoints`

The system **MUST** expose restricted, token-only inter-service operations for integrity-verified apply and tenant cache invalidation, callable only by authenticated internal service callers.

- **Rationale**: Descendant cache invalidation on ancestor apply and other internal reconciliation flows need a narrow, non-administrator-facing activation surface.
- **Actors**: `cpt-cf-settings-service-actor-internal-caller`

## 6. Non-Functional Requirements

> **Global baselines**: Project-wide NFRs (performance, security, reliability, scalability) are defined once at the project/foundational level — see `[docs/ARCHITECTURE_MANIFEST.md](../../../docs/ARCHITECTURE_MANIFEST.md)`. Only gear-specific NFRs (exclusions from defaults or standalone requirements) are documented here.

### 6.1 Gear-Specific NFRs

#### Efficiency: Live-Read, No Central Reload/Restart

- [ ] `p1` - **ID**: `cpt-cf-settings-service-nfr-efficiency-live-read`

The system **MUST** activate every applied value via live-read (pull) with no service disruption; the Settings Service **MUST NOT** reload or restart consumers. A consumer that needs more than a live re-read self-reacts, reacting only to the settings it consumes (per setting, not a blanket per-category restart).

- **Threshold**: Zero platform-initiated reload/restart operations against any consumer.
- **Rationale**: Over-reloading and scattered configuration surfaces are the main operational cost and error source; live-read avoids service restarts entirely.

#### Reliability: Fail-Safe Staged Model

- [ ] `p1` - **ID**: `cpt-cf-settings-service-nfr-reliability-fail-safe-staged`

Given the platform is live and serving traffic, when setting **values** are changed but not yet applied, running services **MUST** be unaffected until an explicit, credential-verified Apply. A failed or partially-failed Apply **MUST** leave unapplied value changes in pending state for retry, **MUST** be idempotently retryable keyed by its Apply Revision (never double-applying an already-applied change), and **MUST** surface failure detail through a durable notification channel. Same-scope concurrent Apply attempts **MUST NOT** silently overwrite or reorder pending revisions; an Apply against a superseded pending set (a stale Apply Revision) **MUST** be detected and reported. Conflict-detection and revision-token mechanics are owned by DESIGN.

- **Threshold**: 100% of unapplied changes leave running services behaviorally unchanged; 100% of failed Apply items remain pending (not silently dropped); zero silently overwritten or reordered pending revisions under same-scope concurrent Apply.
- **Rationale**: Unreviewed or partially-applied configuration changes to a live control plane risk outages; the staged model is the primary safety guarantee.

#### Operational Visibility: Apply Failure Monitoring

- [ ] `p2` - **ID**: `cpt-cf-settings-service-nfr-ops-apply-monitoring`

Beyond the per-change durable notification to the acting administrator, platform operations **MUST** have an aggregate view of Apply health: an apply-failure-rate metric integrated into shared platform dashboards, with alert routing for platform-wide Apply failure conditions (e.g., a bad GTS type rollout causing failures across many settings/services).

- **Threshold**: Apply-failure-rate metric present in platform dashboards; an alert-routing rule exists for platform-wide Apply failure conditions.
- **Rationale**: A per-administrator notification does not give platform operations a way to detect a systemic Apply failure (e.g., failures clustered across many unrelated administrators/scopes); an aggregate operator-facing signal is needed to catch that class of failure.

#### Performance: Read-Path Caching

- [ ] `p1` - **ID**: `cpt-cf-settings-service-nfr-performance-read-cache`

Given services and UIs resolving effective values frequently, cache-served effective-value reads **MUST** complete within 50ms at p95, measured at the Settings read SDK boundary excluding caller-side compute, while sustaining at least 1,000 reads per second for 15 minutes against the full declared dataset: 5,000 settings, 100,000 tenants, and hierarchy depth up to 10. Requests **MUST** be distributed across settings and tenant depths rather than concentrated on a single hot key or scope. Hardware, tooling, deployment topology, and exact distribution mechanics are owned by DESIGN. The cached read path **MUST** resolve cascading effective values against a **locally cached tenant-hierarchy snapshot** — it **MUST NOT** call `tenant-resolver` synchronously per warm read — so that read latency and the read-path availability budget ([§6.1 Availability](#61-gear-specific-nfrs)) do not depend on `tenant-resolver`'s per-request availability. The hierarchy snapshot **MUST** be refreshed within a bounded freshness window (on a `tenant-resolver` change signal or a bounded TTL); the exact freshness bound and refresh mechanics are owned by DESIGN. Value-cache invalidation **MUST** occur on apply, and a hierarchy change that alters resolution **MUST** invalidate affected cached effective values. Secret values **MUST NOT** be cached in plaintext and **MUST NOT** be returned in plaintext on any administrative/human path; plaintext is served only via the authenticated machine-only runtime path ([§5.2](#52-typed-values--validation)).

- **Threshold**: 50ms p95 cache-served read latency at the SDK boundary while sustaining at least 1,000 reads/second for 15 minutes across the full declared dataset, distributed across settings and tenant depths; cache invalidated within the apply transaction.
- **Rationale**: The 1,000 reads/second target represents simultaneous demand from 1% of the declared 100,000 tenants, with each of those 1,000 tenants issuing one effective-value read per second. It is a minimum GA regression floor, not peak-capacity sizing; DESIGN and deployment sizing can require higher targets.

#### Security: Authentication, Secrets, and Step-Up

- [ ] `p1` - **ID**: `cpt-cf-settings-service-nfr-security-baseline`

The system **MUST** enforce authentication and access-level gating on every operation, encrypt secrets at rest and mask them on every administrative/human read (no human reveal path, including audit) — plaintext resolvable only via the authenticated machine-only runtime path, with each resolution audited — require credential step-up before apply, audit all mutations, and enforce scope isolation server-side.

- **Threshold**: Zero unauthenticated operations; zero plaintext secret exposure through any administrative/human path; 100% of machine-path plaintext secret resolutions audited.
- **Rationale**: Settings govern platform behaviour; leakage, unauthorized change, or secret exposure are high-impact security failures.

#### Versatility: Extensible Types and Scope Model

- [ ] `p2` - **ID**: `cpt-cf-settings-service-nfr-versatility-gts-scope-class`

The system **MUST** support scalar and structured values via GTS schema-based types + traits (including domain-specific entity references), and **MUST** govern multi-level overrides across the tenant hierarchy via a first-class Scope Class (global / cascading / local); gear-contributed declarations **MUST** extend the surface without core changes.

- **Threshold**: New setting types and gear-contributed declarations require no core service changes.
- **Rationale**: The configuration surface must grow without code changes and serve platform and tenant scopes from one model.

#### Scope Isolation Integrity

- [ ] `p1` - **ID**: `cpt-cf-settings-service-nfr-scope-isolation`

Given a multi-tenant hierarchy, when any read or search is performed, settings not visible to the caller's scope **MUST NOT** leak through any path (single get, bulk get, search, list-by-category); isolation **MUST** be enforced server-side, never relying on client-side filtering.

- **Threshold**: Zero cross-scope data leakage across all read paths, verified by automated isolation tests per read path.
- **Rationale**: Scope isolation is the multi-tenancy safety guarantee; any leak undermines the entire cascading-override model.

#### Availability

- [ ] `p1` - **ID**: `cpt-cf-settings-service-nfr-availability`

The system **MUST** maintain 99.9% monthly availability for the read path (effective-value resolution via the Settings read SDK). Because warm cached reads resolve against the local tenant-hierarchy snapshot ([§6.1 Performance: Read-Path Caching](#61-gear-specific-nfrs)), the read-path budget **MUST NOT** be charged for a transient `tenant-resolver` outage; warm reads continue during such a blip, while only cold reads and hierarchy-changing/mutation operations fail closed ([§2.2](#22-system-actors)). Administrative write/apply-path availability follows the platform's default maintenance-window expectations and is not held to a stricter bound in v1.

- **Threshold**: 99.9% monthly uptime for effective-value reads, measured on warm cached reads and independent of transient `tenant-resolver` availability.
- **Rationale**: Effective-value reads sit on other services' hot paths ([§6.1 Performance: Read-Path Caching](#61-gear-specific-nfrs)); a dependency consumed on every request needs an explicit uptime bound.

#### Scale & Growth

- [ ] `p2` - **ID**: `cpt-cf-settings-service-nfr-scale-growth`

The system **MUST** support at least 100,000 tenants in the tenant scope hierarchy, a cascading-inheritance hierarchy depth of at least 10 levels, and at least 5,000 distinct settings across all categories per platform instance (admin-authored and gear-contributed combined). Audit volume is stated as a **per-platform-instance aggregate** across all tenants (not per tenant): the system **MUST** sustain at least **50,000,000 audit events per platform instance per year** (mutations, applies, and machine secret-use combined — averaging under ~2 events/second with headroom for peaks; ~500/tenant/year averaged across 100,000 tenants), and **MUST** retain audit records for a **configurable online window of at least 12 months** aligned to the platform retention/anonymization policy, with older records archived or purged per that policy. A scoped audit query (per setting/scope, or a bounded time range) **MUST** return within **2 seconds at p95** over the online window. None of these **MUST** degrade the read-path latency threshold ([§6.1 Performance: Read-Path Caching](#61-gear-specific-nfrs)) or Apply behavior. Storage layout, partitioning, and archival mechanics are owned by DESIGN.

- **Threshold**: 100,000 tenants; 10-level cascade depth; 5,000 settings per platform instance; ≥ 50,000,000 audit events per **platform instance** per year (aggregate); ≥ 12-month configurable online retention; scoped audit query ≤ 2s p95 — none of which degrades the stated read-latency or Apply thresholds.
- **Rationale**: Cascading-inheritance walk cost, gear-contributed declaration volume, and audit-record accumulation are the dimensions most likely to grow unbounded as more gears contribute settings and more tenants onboard. The audit figure is expressed per platform instance because settings mutations are infrequent administrative actions; a per-tenant figure of 1,000,000/year (≈100 billion/year platform-wide) was not a realistic workload and is corrected here.

### 6.2 NFR Exclusions

- **Accessibility** (UX-PRD-002): Not applicable to this PRD — the Settings Hub UI is owned by a future frontend DESIGN document; accessibility requirements belong there.
- **Internationalization** (UX-PRD-003): Not applicable to this PRD for the same reason — UI-level i18n is a frontend DESIGN concern.
- **Safety** (SAFE-PRD-001/002): Not applicable — this service is a pure information/configuration system with no physical, medical, or industrial interaction; "safety" appears elsewhere in this document only in the operational-safety-net sense (staged-change → Apply), not the physical-harm sense these checklist items address.
- **Device/Platform** (UX-PRD-004): Not applicable to this PRD for the same reason as Accessibility/Internationalization — supported platforms, browsers, and responsive/offline behavior are UI-level concerns owned by the future frontend DESIGN document, not this service's backend contract.
- **Inclusivity** (UX-PRD-005): Not applicable to this PRD for the same reason — this is a backend administrative service with no direct end-user-facing surface of its own; inclusivity requirements for the Settings Hub UI belong to the frontend DESIGN document.
- **Regulatory Compliance beyond stated audit requirements**: Not applicable as additional scope — audit coverage is fully defined by `cpt-cf-settings-service-nfr-security-baseline` and `cpt-cf-settings-service-fr-audit-mutations`, and PII-shaped values are governed by the data-classification model in [§5.2](#52-typed-values--validation). Audit-record actor identities (administrator user IDs, retention configurable per [§5.7](#57-security-secrets--audit)) are processed for accountability of privileged administrative actions; the lawful basis and retention terms for this processing are determined by the **platform's approved privacy policy / legal decision**, which this service defers to rather than asserting a specific basis here. If a gear-contributed setting's value is independently PII, its handling is governed by the [§5.2](#52-typed-values--validation) classification model. No further regulatory scope is introduced by this service.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### Settings Read SDK

- [ ] `p2` - **ID**: `cpt-cf-settings-service-interface-settings-read-sdk`

- **Type**: Stable library interface consumed by platform services and UIs (implementation language/runtime owned by DESIGN)
- **Stability**: stable (business-level contract for this PRD; technical contract owned by DESIGN)
- **Description**: Provides live-read access to a setting's effective value with cache invalidation on apply, and exposes the effective source / inheritance trail. It is the sole machine-only path that resolves a `secret`-trait value to plaintext, and only for a consuming service authorized to that specific setting; each such resolution is recorded as a machine secret-use audit event ([§5.7](#57-security-secrets--audit)) and plaintext is never cached. Administrative/human read, search, and audit paths never receive secret plaintext.
- **Breaking Change Policy**: A major version bump is required for any change that alters an existing method's observable contract (return shape, error semantics, or effective-value resolution behavior) in an incompatible way; additive changes (new methods, new optional fields) do not require a bump. Endpoint-level detail is owned by the downstream DESIGN document.

### 7.2 External Integration Contracts

#### Internal Activation Endpoints (Integration Contract)

- [ ] `p3` - **ID**: `cpt-cf-settings-service-contract-internal-activation-endpoints`

- **Direction**: provided by the Settings Service, consumed by internal platform services only
- **Protocol/Format**: Owned by the downstream DESIGN document.
- **Compatibility**: Token-only, restricted to authenticated internal service callers; not a tenant- or administrator-facing contract. A major version bump is required for any incompatible change to request/response shape or authorization requirements; protocol-level detail is owned by the downstream DESIGN document.

## 8. Use Cases

#### Stage, Review & Apply Changes

- [ ] `p1` - **ID**: `cpt-cf-settings-service-usecase-stage-review-apply`

**Actor**: `cpt-cf-settings-service-actor-platform-admin`

**Preconditions**: One or more settings have pending (staged) changes in the administrator's scope.

**Main Flow**:

1. Administrator opens "Apply N pending" and reviews the changes list (old → new, scope, who staged) — applying activates values by live-read with no service disruption by default
2. Administrator re-authenticates (step-up) and confirms
3. System activates each change by live-read (pull) with cache invalidation and shows per-change progress (pending → running → success/failure); a consumer needing more than a live re-read self-reacts on the change signal

**Postconditions**: Successfully applied changes are cleared from pending; the effective value is active via live-read (or the consumer's own self-react, where needed).

**Alternative Flows**:

- **Partial failure**: On partial failure, the system shows a summary of succeeded vs. failed change counts with per-item failure detail and a retry action, plus a durable notification; failed items remain pending for retry.
- **Credential step-up fails**: Apply does not start; all changes remain pending and no value is activated.
- **Concurrent Apply**: Same-scope concurrent Apply attempts do not silently overwrite or reorder pending revisions; conflicts are detected and reported per `cpt-cf-settings-service-nfr-reliability-fail-safe-staged`. Conflict-handling mechanics are owned by DESIGN.
- **Required dependency unavailable**: Tenant-resolution, authentication, authorization, and type dependencies fail closed per their availability expectations ([§2.2 System Actors](#22-system-actors)); no affected pending change is activated.

#### Resolve Effective Value via Inheritance

- [ ] `p2` - **ID**: `cpt-cf-settings-service-usecase-resolve-effective-value`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Preconditions**: The tenant administrator is viewing a cascading setting in their scope.

**Main Flow**:

1. Administrator reads the setting's source indicator: own override / inherited from a named ancestor scope / Platform default
2. Administrator opens the indicator to reveal the full inheritance walk (scopes inspected, which provided the value, when, by whom)
3. Administrator optionally pins the inherited value as an own override to insulate from future ancestor changes

**Postconditions**: The administrator understands where the effective value originates before deciding whether to override it.

**Alternative Flows**:

- **Ancestor changes the value**: If an ancestor later changes the setting, the tenant's own override (if pinned) is unaffected; an unpinned inherited value is invalidated via the descendant cache-invalidation trigger and re-resolved lazily on next read.

#### Read Setting Detail & Stage a Type-Validated Value

- [ ] `p2` - **ID**: `cpt-cf-settings-service-usecase-configure-setting-type-aware`

**Actor**: `cpt-cf-settings-service-actor-platform-admin`

**Preconditions**: The setting exists and is readable in the administrator's scope.

**Main Flow**:

1. The service returns the setting's full detail for the requested scope — key, category breadcrumb, description, declared GTS type + resolved trait set, effective value + source, and Schema Default — so any UI client can render an appropriate type-aware editor; secret-trait values are returned masked
2. The administrator submits a candidate value; the service validates it against the setting's declared GTS type and traits (including `format` keywords and secret handling) before accepting it — the service owns validation and value exposure, not editor rendering or control-to-trait mapping
3. For a cascading setting, the response includes the affected-descendants set (current vs. new effective value); the validated value is staged, not applied

**Postconditions**: The change is staged, not applied, and has been validated against the setting's GTS type.

**Alternative Flows**:

- **Validation fails**: An invalid candidate value is rejected with a field-level error and nothing is staged.
- **Type change invalidates an override**: The affected override becomes `needs-review` and is excluded from Apply until corrected and revalidated, per the type-versioning policy in `cpt-cf-settings-service-fr-typed-value-validation` ([§5.2](#52-typed-values--validation)).

#### Review Audit Trail for Compliance

- [ ] `p2` - **ID**: `cpt-cf-settings-service-usecase-review-audit-trail`

**Actor**: `cpt-cf-settings-service-actor-compliance-reviewer`

**Preconditions**: One or more mutating operations (create/change/revert/remove/apply/clone, or a secret reveal) have occurred within the reviewer's audit scope.

**Main Flow**:

1. Reviewer opens the global audit view and filters by scope, setting, actor, and/or time range
2. Reviewer inspects matching audit records — actor, target (category + key + scope), pre/post values, timestamp, outcome, and request identifier — with all secret values shown masked and no reveal path
3. Reviewer drills into a single setting/scope to see its full mutation history, including any recorded secret-reveal events (actor, timestamp, request id; value still masked)

**Postconditions**: The reviewer can answer "who changed what, when" for any setting/scope in their audit visibility, without ever observing a plaintext secret.

**Alternative Flows**:

- **No matching records**: An empty, clearly-labeled result set is returned rather than an error, distinguishing "nothing happened" from a query failure.

## 9. Acceptance Criteria

**As a platform or tenant administrator, I want** a single, type-safe, auditable Settings Service with staged-then-applied changes and cascading per-tenant overrides **so that** I can configure the platform confidently from one place without risking unreviewed or unsafe changes to a live system.

Each criterion validates the referenced FR/NFR; the full normative statement lives with that requirement.

- [ ] Category with a unique name and description can be created; duplicate name is rejected with a clear error (`cpt-cf-settings-service-fr-settings-category-model`)
- [ ] Category removal is rejected while settings remain; succeeds only when empty (`cpt-cf-settings-service-fr-settings-category-model`)
- [ ] Setting created under an existing category with key/type/default/mode/description, key unique within its category; non-existent category or duplicate key rejected (`cpt-cf-settings-service-fr-settings-category-model`)
- [ ] Values (override or default) validated against the declared GTS type at create/change; invalid values rejected with a field-level error; `format` keywords and trait-driven rules asserted, not advisory (`cpt-cf-settings-service-fr-typed-value-validation`)
- [ ] Reads expose (or let clients resolve) a setting's type and resolved trait set for rendering and pre-validation; structured (object/array) values supported (`cpt-cf-settings-service-fr-typed-value-validation`)
- [ ] `secret`-trait values stored encrypted at rest and masked on every administrative/human read, search, and audit path; plaintext not retrievable through any administrative or human-facing path (`cpt-cf-settings-service-fr-typed-value-validation`)
- [ ] Secret plaintext is resolvable only via the authenticated machine-only runtime path, only for a consumer authorized to that specific setting, and every such resolution is recorded as a masked machine secret-use audit event (`cpt-cf-settings-service-fr-typed-value-validation`, `cpt-cf-settings-service-fr-audit-mutations`)
- [ ] Setting values are classified as public, PII, or secret; unauthorized administrative reads and audit/report outputs mask PII, and only callers authorized for unmasked PII can observe it (`cpt-cf-settings-service-fr-typed-value-validation`)
- [ ] PII in setting values remains governed by the platform retention/anonymization policy (`cpt-cf-settings-service-fr-typed-value-validation`)
- [ ] A GTS type change that invalidates an existing override marks it `needs-review` and excludes it from Apply until corrected and revalidated (`cpt-cf-settings-service-fr-typed-value-validation`)
- [ ] Standard-mode reads exclude Advanced-only settings and categories; mode preference persists per user, not per session (`cpt-cf-settings-service-fr-standard-advanced-mode`)
- [ ] Reads expose the count of hidden Advanced-only settings per category rather than silently omitting them (`cpt-cf-settings-service-fr-standard-advanced-mode`)
- [ ] Cross-field search (key/description/value/category) returns a flat list with category breadcrumbs and matched-field indication; respects scope/mode/visibility filters (`cpt-cf-settings-service-fr-search-discoverability`)
- [ ] Value search covers only defaults/overrides the caller may read in scope; `secret` values are never indexed or matched (no leakage via match existence, count, snippet, or timing); PII authorization is applied before matching so unauthorized callers cannot match PII content; structured-value search matches leaf values under the same rules (`cpt-cf-settings-service-fr-search-discoverability`)
- [ ] Tenant-scope revert clears the local override and falls back to the nearest ancestor's override or the platform default; the resulting fallback is communicated before commit (`cpt-cf-settings-service-fr-defaults-revert`)
- [ ] Platform-scope revert clears the override and falls back to the Schema Default, which remains intact and independent throughout (`cpt-cf-settings-service-fr-defaults-revert`)
- [ ] A value operation (set/revert/remove-value/clone) marks the setting pending with no effect on running services until Apply; a persistent indicator reflects the pending value-change count in the user's visible scope (`cpt-cf-settings-service-fr-staged-change-pending`)
- [ ] Remove-value clears only the explicit override at the target scope and does so only after Apply (`cpt-cf-settings-service-fr-staged-change-pending`)
- [ ] Descriptive-metadata declaration edits (description, Mode, Domain Affinity) apply immediately and change no effective value (`cpt-cf-settings-service-fr-staged-change-pending`)
- [ ] Behavior-affecting declaration fields (Schema Default, GTS type, Scope Class) are immutable: an in-place edit is rejected, and the change is expressible only via a replacement declaration or a new major GTS type version (`cpt-cf-settings-service-fr-settings-category-model`, `cpt-cf-settings-service-fr-staged-change-pending`)
- [ ] Setting removal is a step-up-gated soft-delete (retire) that drops the setting from resolution at once; values are retained and recoverable by reactivation, which is likewise step-up-gated (`cpt-cf-settings-service-fr-settings-category-model`, `cpt-cf-settings-service-fr-staged-change-pending`)
- [ ] Pending-changes view shows category, key, old value (or "default"), new value, and who staged each change; individual and bulk discard supported (`cpt-cf-settings-service-fr-staged-change-pending`)
- [ ] Apply preview lists each pending value change (category, key, old → new value, scope, who staged) and requires credential step-up before proceeding (`cpt-cf-settings-service-fr-apply-preview-stepup`)
- [ ] Applying activates values via live-read (pull) through the settings SDK with cache invalidation; the Settings Service never reloads or restarts a consumer (`cpt-cf-settings-service-fr-apply-effect-resolution`)
- [ ] A consumer needing more than a live re-read self-reacts on the change signal (rebuild, re-render, or restart itself), reacting only to the settings it consumes, not a blanket per-category restart (`cpt-cf-settings-service-fr-apply-effect-resolution`)
- [ ] Apply result reports per-change old → new value, scope, and success/failure; pending flags cleared only for successfully applied changes; failed/partial Apply leaves items pending for retry (`cpt-cf-settings-service-fr-apply-effect-resolution`)
- [ ] Interdependent settings apply atomically as a Dependency Group; an Apply that would leave the scope in an invalid combination is rejected for that group and left pending, never partially committed (`cpt-cf-settings-service-fr-apply-effect-resolution`)
- [ ] A Dependency Group and its cross-setting constraint can be declared over admin-authored settings by a platform administrator and over contributed settings by the owning gear; the definition is behavior-affecting and immutable in place, changed only via a replacement declaration (`cpt-cf-settings-service-fr-dependency-group-declaration`)
- [ ] A change is "successfully applied" only when durably persisted and its applied-scope cache-invalidation issued (descendant invalidation emitted for cascading settings); persistence commits before cache-invalidation/consumer signaling (`cpt-cf-settings-service-fr-apply-effect-resolution`)
- [ ] Apply is idempotent keyed by its Apply Revision: retry re-acts only on still-pending changes and never double-applies; an Apply against a superseded pending set is detected and reported (`cpt-cf-settings-service-fr-apply-effect-resolution`, `cpt-cf-settings-service-nfr-reliability-fail-safe-staged`)
- [ ] Same-scope concurrent Apply attempts cannot silently overwrite or reorder pending revisions; conflicts are detected and reported, with conflict-handling mechanics owned by DESIGN (`cpt-cf-settings-service-nfr-reliability-fail-safe-staged`)
- [ ] Tenant-scoped value of a `cascading` setting overrides the inherited value at the target tenant (caller's own tenant or a descendant within its subtree) and its non-overriding descendants; set/apply/clone/remove follow the staged-then-apply model (`cpt-cf-settings-service-fr-tenant-overrides`)
- [ ] Clone requires authorization to read and use the source effective value and to mutate the target scope; it creates an independent explicit override at that authorized target scope without copying the Setting Declaration or linking future source changes to the target override (`cpt-cf-settings-service-fr-staged-change-pending`, `cpt-cf-settings-service-fr-tenant-overrides`)
- [ ] Effective value resolved by walking up the hierarchy to the first override, else the platform default; read API exposes the effective source / inheritance trail (`cpt-cf-settings-service-fr-cascading-inheritance`)
- [ ] Changing a cascading setting reports a non-blocking warning listing affected descendants with current vs. new effective values; the administrator can proceed (`cpt-cf-settings-service-fr-cascading-inheritance`)
- [ ] Every setting declares a Scope Class; cascade/override behaviour derives deterministically from it (global / cascading / local semantics), never from independently-set booleans (`cpt-cf-settings-service-fr-setting-scope-class`)
- [ ] Effective value resolves per the Scope Class resolution table: a tenant-visible `global` surfaces the platform value read-only (override rejected); a `local` resolves to the Schema Default at any tenant without its own local override, and a platform-scope `local` value is not inherited by tenants (`cpt-cf-settings-service-fr-setting-scope-class`)
- [ ] Only tenant-visible settings are returned and only tenant-overridable settings are changeable; flags are platform-admin-managed; visibility is orthogonal to Scope Class (global may be read-only tenant-visible, never overridable) (`cpt-cf-settings-service-fr-tenant-scope-enforcement`)
- [ ] Tenant-administrator operations against a target tenant are constrained server-side to the caller's subtree (own tenant or any descendant); a target outside the subtree is rejected; setting at a descendant creates the override there; no modification of platform-scoped or ancestor values; non-visible settings not exposed through any read path (`cpt-cf-settings-service-fr-tenant-scope-enforcement`)
- [ ] Every operation requires an authenticated session/token and an authorization decision (view = read; owner/admin = mutate/apply); Apply and behavior-affecting declaration actions (retire/reactivate) additionally require credential step-up (`cpt-cf-settings-service-fr-authn-role-gating`)
- [ ] Credential step-up is a pluggable behaviour behind a stable contract: the Settings Service hard-codes no single second-authentication implementation, works against a platform-provided default second-authentication gear, and accepts an integrator-supplied replacement behind the same contract (`cpt-cf-settings-service-fr-authn-role-gating`)
- [ ] Every mutation writes an audit record (actor, target, pre/post values with secrets masked, timestamp, outcome, request id); audit history queryable globally and per (setting, scope) with no reveal path (`cpt-cf-settings-service-fr-audit-mutations`)
- [ ] Audit-record actor identities are classified as public or PII; unauthorized administrative audit reads and audit/report outputs mask actor-identity PII, only callers authorized for unmasked PII can observe it, and retention/anonymization policy remains enforced (`cpt-cf-settings-service-fr-audit-mutations`)
- [ ] Licence/feature-gated settings are excluded server-side for callers without the entitlement, consistently across all administrative read paths; the internal in-process settings reader is not licence-gated (`cpt-cf-settings-service-fr-feature-license-gating`)
- [ ] Unapplied changes leave running services behaviorally unchanged; failed or partially-failed Apply leaves items pending for retry with durable failure notification (`cpt-cf-settings-service-nfr-reliability-fail-safe-staged`)
- [ ] Cache-served effective-value reads complete within 50ms at p95 at the SDK boundary while sustaining at least 1,000 reads/second for 15 minutes against 5,000 settings, 100,000 tenants, and hierarchy depth up to 10, distributed across settings and tenant depths rather than one hot key or scope; cache invalidated on apply; secrets never cached or returned in plaintext (`cpt-cf-settings-service-nfr-performance-read-cache`)
- [ ] No cross-scope leakage through any read path (single get, bulk get, search, list-by-category); isolation enforced server-side (`cpt-cf-settings-service-nfr-scope-isolation`)
- [ ] Gears contribute namespaced Setting Declarations on install/upgrade; administrators change contributed values only (subject to permissions and Scope Class), never the Declarations (`cpt-cf-settings-service-fr-module-contributed-declarations`)
- [ ] Declaration set reconciled (add / update / mark-retired) on register/upgrade/retire, preserving administrator-set values across compatible upgrades; invalidating declarations follow the GTS type-versioning policy (`cpt-cf-settings-service-fr-contributed-lifecycle`)
- [ ] Values activate via live-read with no platform-initiated reload/restart of any consumer; a consumer needing more self-reacts, scoped per setting it consumes — zero platform-initiated reload/restart operations (`cpt-cf-settings-service-nfr-efficiency-live-read`)
- [ ] Security baseline enforced on every operation: authentication + access gating, secrets encrypted and masked on all administrative/human paths with no human reveal path (including audit; plaintext only via the audited machine-only runtime path), step-up before apply, all mutations audited, server-side scope isolation (`cpt-cf-settings-service-nfr-security-baseline`)
- [ ] Scalar and structured values supported via GTS types + traits; multi-level overrides governed by Scope Class; new types and gear-contributed declarations require no core service changes (`cpt-cf-settings-service-nfr-versatility-gts-scope-class`)
- [ ] 99.9% monthly availability maintained for effective-value reads via the Settings read SDK (`cpt-cf-settings-service-nfr-availability`)
- [ ] Apply-failure-rate metric present in shared platform dashboards with an alert-routing rule for platform-wide Apply failure conditions (`cpt-cf-settings-service-nfr-ops-apply-monitoring`)
- [ ] 100,000 tenants, 10-level cascade depth, 5,000 settings per platform instance, and ≥ 50,000,000 audit events per platform instance per year (aggregate) with ≥ 12-month configurable online retention and scoped audit query ≤ 2s p95, sustained without degrading the read-latency or Apply thresholds (`cpt-cf-settings-service-nfr-scale-growth`)
- [ ] Hub categories filtered by the administrator's current Domain Affinity; cross-domain settings hidden by default; platform administrators can switch to "All domains"; orthogonal to Standard/Advanced mode (`cpt-cf-settings-service-fr-domain-affinity-filtering`)
- [ ] Internal Activation Endpoints (integrity-verified apply, tenant cache invalidation) are restricted, token-only, and callable only by authenticated internal service callers (`cpt-cf-settings-service-fr-internal-activation-endpoints`)

## 10. Dependencies


| Dependency                                                                                                                                                                                                                                                                            | Description                                                                                                                                                                                                                                                                          | Criticality |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------- |
| Types Registry (`gears/system/types-registry`)                                                                                                                                                                                                                                        | Type system + traits used to validate setting values                                                                                                                                                                                                                                 | `p1`        |
| Tenant Resolver (`gears/system/tenant-resolver`)                                                                                                                                                                                                                                      | Tenant scope hierarchy that settings cascade through; scope enforcement                                                                                                                                                                                                              | `p1`        |
| AuthZ Resolver (`gears/system/authz-resolver`)                                                                                                                                                                                                                                        | Authorization decisions (AuthZEN Subject+Action+Resource PDP) for read/mutate/apply gating — **the view/owner/admin access-level model this PRD assumes is new work**; `authz-resolver` has no built-in role storage or hierarchy today (deferred to a future AuthZ Management Gear) | `p1`        |
| AuthN Resolver (`gears/system/authn-resolver`)                                                                                                                                                                                                                                        | Bearer-token authentication only (single `authenticate()` call per ADR-0003) — **credential step-up re-verification is new work**; no re-authentication primitive exists in `authn-resolver` today                                                                                   | `p1`        |
| `toolkit-db` (library)                                                                                                                                                                                                                                                                | Storage of settings and encrypted secrets                                                                                                                                                                                                                                            | `p1`        |
| Audit (Core Functionality) — **aspirational, not yet implemented per `docs/GEARS.md`**                                                                                                                                                                                                | Audit trail for mutations and apply, including secret-reveal events                                                                                                                                                                                                                  | `p1`        |
| License Resolver (`gears/system/license-resolver`) — **aspirational, not yet implemented** (zero `.rs` files; absent from the workspace `Cargo.toml` members; every `docs/GEARS.md` scenario unchecked) + broader Policy Manager concept — **also aspirational, not yet implemented** | Admission gating and feature/licence gating                                                                                                                                                                                                                                          | `p2`        |
| Events & Notifications (Core Functionality) — **aspirational, not yet implemented**                                                                                                                                                                                                   | Apply progress/failure notifications                                                                                                                                                                                                                                                 | `p2`        |


### 10.1 Launch Prerequisites (blocking)

Several mandatory acceptance criteria depend on platform capabilities that **do not exist yet**. Each is a **blocking launch prerequisite**: the PRD is **not implementation-ready** until, for every row below, either the named owner has delivered the integration contract or an **approved interim mechanism** has been agreed with that owner and recorded in DESIGN. These are tracked as milestone dependencies, not optional enhancements.

| Prerequisite | Current state | Owner | Required before | Approved interim mechanism |
| ------------ | ------------- | ----- | --------------- | -------------------------- |
| **Credential step-up (re-authentication) primitive** — gates every Apply and every behavior-affecting declaration action (retire/reactivate); consumed as a **pluggable behaviour** behind a stable step-up contract | `authn-resolver` exposes only `authenticate(bearer_token)` (ADR-0003); no re-authentication method | Platform Security / `authn-resolver` owners | DESIGN defines the step-up contract; the default second-authentication gear delivered; **GA** | A default second-authentication gear provided by the platform, or an integrator-supplied implementation behind the same contract |
| **View / owner-admin access-level model** — backs the view-vs-mutate/apply distinction in `cpt-cf-settings-service-fr-authn-role-gating` | `authz-resolver` is an AuthZEN PDP with no built-in role storage/hierarchy; binding deferred to a future AuthZ Management Gear | Platform AuthZ owners | DESIGN can specify access gating; **GA** | An approved interim role→permission mapping consumed via the existing PDP |
| **Platform audit subsystem** — records mutations and machine secret-use events (`cpt-cf-settings-service-fr-audit-mutations`) | Aspirational per `docs/GEARS.md`; not yet implemented | Platform Core (Audit) | Audit FRs/NFRs can be realized; **GA** | A DESIGN-owned interim audit sink meeting the same masking/retention guarantees |
| **Events & Notifications** — durable Apply progress/failure notifications (`cpt-cf-settings-service-nfr-reliability-fail-safe-staged`, `cpt-cf-settings-service-nfr-ops-apply-monitoring`) | Aspirational; not yet implemented | Platform Core (Events/Notifications) | Durable failure-notification behavior can be delivered; **GA** | An interim notification channel/stub, if approved |

## 11. Assumptions

- The platform's Types Registry is available and stable enough to define value types before this gear's v1 ships.
- The tenant scope hierarchy (resolved via `tenant-resolver`) is in place for the cascading-inheritance walk to operate against.
- A view/owner/admin access-level model is built — by `authz-resolver`'s future AuthZ Management Gear or an interim mechanism — to back the view-vs-mutate/apply distinction `cpt-cf-settings-service-fr-authn-role-gating` depends on; `authz-resolver`'s current PDP API has no built-in role storage or hierarchy (see [§13 Open Questions](#13-open-questions)).
- A credential step-up (re-authentication) primitive is built behind a stable, pluggable step-up contract before Apply can enforce it — satisfied by a default second-authentication gear provided by the platform, or by an integrator-supplied implementation behind the same contract; `authn-resolver`'s current API has no re-authentication method (see [§13 Open Questions](#13-open-questions)).
- A platform audit subsystem becomes available to record mutation and secret-reveal audit events (currently aspirational — see [§10 Dependencies](#10-dependencies)).

## 12. Risks


| Risk                                                                   | Impact                                                                                                                                    | Mitigation                                                                                                                                                           |
| ---------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Retired-value lifecycle (module retire or admin soft-delete) undefined | Administrator-set values for a retired setting — via gear removal or an admin's soft-delete of the setting — may be orphaned indefinitely | Resolve retention/purge/archive policy in DESIGN before GA                                                                                                           |
| Audit, Policy Manager, and Events/Notifications gears do not exist yet | This service's audit-trail and notification FRs/NFRs cannot be fully realized until those platform capabilities ship                      | Track as aspirational dependencies ([§10](#10-dependencies)); DESIGN may need an interim audit sink/notification stub until the platform-wide gear ships             |
| Tenant Resolver outage | Cold tenant reads and hierarchy-changing/mutation operations cannot establish the target hierarchy or enforce scope isolation; warm cached reads are unaffected | Warm reads continue from the local hierarchy snapshot; only cold reads and hierarchy-changing/mutation operations fail closed per the Tenant Resolver availability expectation ([§2.2](#22-system-actors)); snapshot freshness bound, outage detection, and surfacing to platform operations are owned by DESIGN |
| Concurrent Apply attempts against the same scope | Pending changes may be lost, activated out of order, or applied against a stale pending revision | Pending-revision integrity under same-scope concurrency is required by `cpt-cf-settings-service-nfr-reliability-fail-safe-staged`; conflict-handling mechanics are owned by DESIGN |
| Incomplete descendant invalidation after applying a cascading change | Descendants may continue receiving stale inherited values | Descendant cache-invalidation on apply is required by `cpt-cf-settings-service-fr-apply-effect-resolution`; bounding stale-value exposure and verifying invalidation coverage are owned by DESIGN |
| Audit volume exceeds retention or storage capacity | Required audit evidence may become unavailable, or storage growth may disrupt service operation | Capacity for the declared audit volume is required by `cpt-cf-settings-service-nfr-scale-growth`, with retention/anonymization governed by the [§5.2](#52-typed-values--validation) classification model; storage and retention mechanics are owned by DESIGN |


## 13. Open Questions

- Retired-setting value lifecycle: when a setting becomes retired — a gear removal **or an administrator's (soft) removal of a setting** — what happens to its administrator-set values — purge, archive, or retain as orphaned? Applies to both the gear-retire and admin-remove paths. — owner: Settings Service DESIGN owner — target resolution: before GA.
- Step-up re-authentication primitive: `authn-resolver`'s public API has no re-authentication/step-up method today (ADR-0003's minimalist interface). Step-up is consumed as a pluggable behaviour behind a stable contract; the open items are the exact contract shape, who builds the default second-authentication gear that satisfies it, and by when (an integrator can substitute their own implementation behind the same contract). — owner: Platform Security / `authn-resolver` owners — target resolution: before DESIGN depends on it.
- View/owner/admin access-level model: `authz-resolver` has no built-in role storage or hierarchy today (deferred to a future AuthZ Management Gear per `docs/arch/authorization/PERMISSION_GTS_TYPE.md`). What concrete mechanism backs the view-vs-owner/admin distinction this PRD assumes, and who builds it? — owner: Platform AuthZ owners — target resolution: before DESIGN depends on it.

## 14. Traceability

Links to related specification artifacts.

- **Design**: [DESIGN.md](./DESIGN.md) — TBD, not yet authored for this gear
- **ADRs**: [ADR/](./ADR/) — TBD, not yet authored for this gear
- **Features**: [features/](./features/) — TBD, not yet authored for this gear

