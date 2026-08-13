<!-- Created: 2026-08-10 by Constructor Tech -->
<!-- Updated: 2026-08-10 by Constructor Tech -->

# Decomposition: Settings Service

**Overall implementation status:**
- [ ] `p1` - **ID**: `cpt-cf-settings-service-status-overall`

<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 Gear Foundation, SDK Contracts and Cross-Cutting Infrastructure &mdash; HIGH](#21-gear-foundation-sdk-contracts-and-cross-cutting-infrastructure-mdash-high)
  - [2.2 Category Management &mdash; HIGH](#22-category-management-mdash-high)
  - [2.3 Setting Declarations and Scope Class &mdash; HIGH](#23-setting-declarations-and-scope-class-mdash-high)
  - [2.4 Typed Value Validation &mdash; HIGH](#24-typed-value-validation-mdash-high)
  - [2.5 Effective Value Resolution, Defaults and Cache &mdash; HIGH](#25-effective-value-resolution-defaults-and-cache-mdash-high)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

## 1. Overview

This is a **partial decomposition**. It covers the first implementable wave only: the five features that can be started immediately because every dependency they have is satisfied either by nothing at all or by another feature inside this wave. It is not a complete split of the DESIGN, and it does not claim full requirement coverage. Later waves extend this document rather than replace it.

**Selection rule**: a feature qualifies for this wave only if its dependency closure lies entirely within the wave. The five below form a chain — foundation, then the category taxonomy, then declarations, then value validation, then effective-value resolution — with no edge pointing outside the set.

**What the wave delivers end to end**: after 2.5 the service is functionally alive on its read path. An administrator can create a category, declare a typed setting against a curated GTS value type with a Schema Default, and any consumer can resolve that setting's effective value for a scope, with the inheritance trail and the hot-path cache behind it. That is a working settings service for reads.

**What the wave deliberately excludes**: staged changes and Apply, tenant override writes, secret values, module-contributed declarations, search and browse modes, and audit coverage assertions. Those become startable once 2.5 lands, and most of them depend only on it.

**Coverage in this wave** — stated precisely because a partial decomposition must not imply completeness, and because `cfs validate` does not enforce requirement coverage in a DECOMPOSITION (`fr.references.DESIGN` carries `coverage = true`; the DECOMPOSITION reference does not):

| Element | Covered here | Total | Remaining for later waves |
|---|---|---|---|
| PRD functional requirements | 7 | 21 | 14 |
| PRD non-functional requirements | 5 | 9 | 4 |
| DESIGN components | 6 | 11 | 5 |
| DESIGN principles | 5 | 8 | 3 |
| DESIGN constraints | 9 | 12 | 3 |
| DESIGN sequences | 1 | 2 | 1 |

**Known PRD-to-DESIGN divergence, outside this wave.** The PRD defines two requirements that no DESIGN document references by ID, and DESIGN narrows one of them deliberately.

- `cpt-cf-settings-service-fr-replica-coherence` — a **traceability** gap, not a design gap. DESIGN §4.2 *Cache & Invalidation* already specifies the mechanism the requirement asks for: the `cache_invalidate` broadcast plus a `cache_ttl_seconds` backstop (default 30 s) that self-heals a missed broadcast, which is exactly the bounded staleness window that must hold whether or not the signal arrives. DESIGN needs to cite the requirement, not acquire new content.
- `cpt-cf-settings-service-fr-consumer-activation` — also a **traceability** gap. An earlier revision of this section called it a design gap, listing consumer registration of interest, identifier-only notification payloads, delivery-until-confirmed, and the per-Apply account of confirmations as absent. All four are specified in [DESIGN-activation](./DESIGN-activation.md): per-setting opt-in subscription (§4.2 *Subscription Manager*), the identifier-only `apply_notification` payload, redelivery from awaiting records until acked with **no** automatic timeout, and one await-record per `(apply, subscriber, key)` resolved by the Apply Outcome Tracker. Obligations the earlier text did not mention — tolerate-repeats rather than exactly-once, late outcomes always accepted, outcomes attributed to the reporting subscriber, and subscription retirement resolving still-open records — are specified there too. As with its sibling, DESIGN needs to cite the requirement.

**Accepted narrowing — notification filtering is by subscription, not by entitlement.** `cpt-cf-settings-service-fr-consumer-activation` requires that a notification carry *"never a setting it is not entitled to read"*. DESIGN-activation §4.1 filters each notification to the subscriber's **own subscribed keys** and states that, under the platform trusted-caller model, subscriber identity is taken on trust — making this least-privilege by blast radius rather than an identity-enforced entitlement check. **This narrowing is intended and accepted.** It is recorded here rather than left in passing prose because it qualifies a PRD `MUST`: a consumer that subscribes to a key it could not read on the read path is still notified of that key's change. Revisit if consumers ever cease to be trusted.

Neither item blocks this wave, and — contrary to an earlier revision of this note — **neither blocks the Apply-side wave from being decomposed**. What remains is for DESIGN to cite two requirement IDs, which is an editorial change to DESIGN rather than new design content.

**Deferred for the Apply-side wave — step-up verification.** `cpt-cf-settings-service-fr-apply-preview-stepup` requires credential re-verification before every Apply, with no carve-out. DESIGN.md §4.2 assigns the **step-up contract to the `authn-resolver` gear**, which this service resolves through `ClientHub` rather than defines; that contract does not exist yet. Apply is therefore to be built **without** a verifier binding, while taking the `step_up_assertion` parameter on its signature from the outset — so landing verification later is a change inside Apply rather than to its contract. Shipping Apply without a verifier is **not** conformant: DESIGN records that an always-satisfied binding removes the requirement rather than implementing it, and the only sanctioned non-verifying binding is `MockStepUpVerifier` inside the test harness.

## 2. Entries

### 2.1 Gear Foundation, SDK Contracts and Cross-Cutting Infrastructure &mdash; HIGH

- [ ] `p1` - **ID**: `cpt-cf-settings-service-feature-gear-foundation`

- **Purpose**: Establish the `settings-service-sdk` crate and the gear scaffold every later feature builds on: domain models, the Settings Reader and Contribution client traits, the error taxonomy and its RFC-9457 Problem mapping, PostgreSQL persistence with migration tooling, REST and OData infrastructure, the `PolicyEnforcer` PEP with credential step-up, and the Audit Emitter that all mutating features publish through. The gear directory is currently docs-only, so this feature is what makes any code exist at all.

- **Depends On**: None

- **Scope**:
  - SDK crate (`settings-service-sdk`): domain models, the `SettingsReaderClient` trait (DESIGN §4.5), the Contribution client trait, the `TypeValidator` trait whose implementation arrives in 2.4, and the error taxonomy
  - Gear scaffold: `#[toolkit::gear]` annotated gear with ClientHub registration for the SDK client traits
  - Persistence: SeaORM entity scaffolding, `SecureConn` and `DBRunner` wiring, migration harness
  - Error mapping: `DomainError` to Problem (RFC-9457) across the deterministic error categories of DESIGN §4.3
  - REST infrastructure: `OperationBuilder` wiring, OData `$filter`, `$select`, and `$orderby` parsing, pagination helpers, and `If-Match`/ETag plumbing
  - AuthN and AuthZ: the `PolicyEnforcer` PEP pattern, `AccessScope` derived from PDP constraints, and the credential step-up interaction at the IdP that behavior-affecting actions in later features invoke
  - Audit Emitter: the shared mutation-audit and event publication path
  - Deployment-owned bootstrap configuration delivered through ToolKit config at gear init (DESIGN §4.9), never as a managed setting

- **Out of scope**:
  - Every domain service and its REST handlers, which are features 2.2 through 2.5 and later waves
  - Authorization decisions themselves, owned by the RBAC Engine and the AuthZ Resolver Plugin
  - Asserting audit coverage across all mutating paths, which can only be closed once those paths exist

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-fr-authn-role-gating`
  - [ ] `p1` - `cpt-cf-settings-service-nfr-security-baseline`
  - [ ] `p1` - `cpt-cf-settings-service-nfr-availability`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-principle-fail-closed`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-constraint-supplied-as-gear`
  - [ ] `p1` - `cpt-cf-settings-service-constraint-postgres-primary-storage`
  - [ ] `p1` - `cpt-cf-settings-service-constraint-rbac-policy-enforcer`
  - [ ] `p1` - `cpt-cf-settings-service-constraint-audit-and-events`
  - [ ] `p1` - `cpt-cf-settings-service-constraint-step-up-at-idp`

- **Domain Model Entities**:
  - SDK client traits (`SettingsReaderClient`, Contribution client)
  - Error taxonomy and Problem mapping types
  - Pagination and OData query value objects

- **Design Components**:

  - [ ] `p1` - `cpt-cf-settings-service-component-audit-emitter`

- **API**:
  - Gear initialization and ClientHub registration; no domain REST endpoints in this feature

- **Data**:
  - Migration harness and shared schema conventions; no domain tables in this feature

### 2.2 Category Management &mdash; HIGH

- [ ] `p1` - **ID**: `cpt-cf-settings-service-feature-category-management`

- **Purpose**: Provide the flat category taxonomy every setting declaration is filed under, with globally unique `key` and `name`, optional administrative-domain binding, display ordering, and no-orphan deletion. The category `key` is load-bearing beyond grouping: it becomes the `<category>` segment of an admin-authored setting's instance id, which is why it is validated against the path separator rather than treated as free text.

- **Depends On**: `cpt-cf-settings-service-feature-gear-foundation`

- **Scope**:
  - `Category` entity and the `categories` table with `uq_category_key` and `uq_category_name` (DESIGN §4.1, §4.7)
  - `create_category`, `update_category`, `delete_category`, `get_category`, and `list_categories` (DESIGN §4.2)
  - Five REST endpoints under `/v1/categories` (DESIGN §4.3)
  - Per-resource-type CRUD authorization on `gts.cf.toolkit.settings.category.v1~` through the `PolicyEnforcer`
  - `key` validation rejecting `/` and enforcing the 1..128 bound
  - No-orphan deletion returning `409 CategoryNotEmpty` while any declaration references the category, including `retired` declarations
  - `If-Match` and ETag optimistic concurrency on `PATCH` and `DELETE`
  - Domain-filtered, visibility-gated, paginated list ordered by `sort_order` then `name`
  - `idx_categories_name_trgm` GIN trigram index on `name`, supporting search in a later wave

- **Out of scope**:
  - Setting declarations and the `setting_declarations` table, which arrive in 2.3 along with the foreign key that makes no-orphan deletion enforceable at the database level
  - Category-scoped search ranking, which belongs to the search wave
  - Category nesting, which the PRD excludes: categories are flat

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-fr-settings-category-model`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-constraint-optimistic-concurrency`

- **Domain Model Entities**:
  - Category
  - DomainAffinity, as applied to a category

- **Design Components**:

  - [ ] `p1` - `cpt-cf-settings-service-component-category-management`

- **API**:
  - POST /v1/categories
  - GET /v1/categories
  - GET /v1/categories/{id}
  - PATCH /v1/categories/{id}
  - DELETE /v1/categories/{id}

- **Data**:
  - `categories` table with `uq_category_key`, `uq_category_name`, and `idx_categories_name_trgm`

### 2.3 Setting Declarations and Scope Class &mdash; HIGH

- [ ] `p1` - **ID**: `cpt-cf-settings-service-feature-setting-declarations`

- **Purpose**: Introduce the setting declaration as an entity distinct from its value, keyed by a GTS instance identifier of the form `<value-type>~<instance-id>`, and give each declaration a first-class scope class from which cascade and override behaviour is derived deterministically rather than configured per setting. Establishes the mutation-class discipline that keeps declaration edits from silently changing a live setting's resolution.

- **Depends On**: `cpt-cf-settings-service-feature-category-management`

- **Scope**:
  - `SettingDeclaration` entity and the `setting_declarations` table, including the `category_id` foreign key declared `ON DELETE RESTRICT`, which is the authoritative guard behind 2.2's no-orphan rule
  - Key construction: the admin supplies `value_type_id`, a `vendor`, and a leaf `name`; the service builds the instance id `<vendor>.settings.<category>.<name>.v1` and the full `key` as `<value_type_id>~<instance-id>`, validating each segment against the GTS grammar and rejecting violations with `422`
  - Uniqueness: `uq_declaration_key` globally, and `UNIQUE(category_id, leaf_slug)` within a category
  - `ScopeClass` (`global`, `cascading`, `local`) and the scope-class engine deriving cascade and override behaviour
  - `DeclarationSource`, `DeclarationStatus`, and `DomainAffinity` enums
  - Schema Default: `default_value` is non-null so the resolution chain always terminates, is validated against the value type, and supports structured object and array defaults
  - Trait-derived classification: `has_secret_trait` resolved from the value type, `data_classification` with `secret` derived from the trait and never author-supplied, rejecting an author-supplied `secret` on a non-secret type
  - Rejecting a non-empty `default_value` on a secret-trait type
  - Forcing `tenant_overridable = false` when `scope_class = global`
  - The mutation-class discipline: descriptive metadata immediate under `update` plus `If-Match`; behavior-affecting fields (`default_value`, value type, `scope_class`) immutable and rejected with `422`; retire and reactivate immediate but credential step-up gated using the primitive delivered in 2.1; classification tightening immediate, loosening step-up gated
  - Retire as soft-delete setting `status = retired`, retaining values in `setting_values` while excluding them from resolution, with re-declare-to-revive recovery
  - Dependency Group and cross-setting constraint **declaration**
  - Declaration REST surface, visibility-, domain-, and licence-gated, returning the `key`, its `value_type_id`, and resolved traits for client rendering

- **Out of scope**:
  - Module-contributed declarations and the Contribution Reconciler, which own the contributed write path
  - Value writes, validation internals, and secret value storage
  - Atomic application of a Dependency Group, which is Apply-side; only the declaration of the group is here

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-fr-setting-scope-class`
  - [ ] `p1` - `cpt-cf-settings-service-fr-dependency-group-declaration`
  - [ ] `p1` - `cpt-cf-settings-service-nfr-versatility-gts-scope-class`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-principle-declaration-value-split`
  - [ ] `p1` - `cpt-cf-settings-service-principle-scope-class-derivation`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-constraint-key-is-gts-instance-id`

- **Domain Model Entities**:
  - SettingDeclaration
  - ScopeClass
  - DeclarationSource, DeclarationStatus, DomainAffinity

- **Design Components**:

  - [ ] `p1` - `cpt-cf-settings-service-component-declaration-management`

- **API**:
  - POST /v1/declarations
  - GET /v1/declarations
  - GET /v1/declarations/{id}
  - PATCH /v1/declarations/{id}
  - DELETE /v1/declarations/{id}

- **Data**:
  - `setting_declarations` table with `uq_declaration_key`, `uq_declaration_category_slug`, the `categories` foreign key `ON DELETE RESTRICT`, and the partial active-status index

### 2.4 Typed Value Validation &mdash; HIGH

- [ ] `p1` - **ID**: `cpt-cf-settings-service-feature-typed-value-validation`

- **Purpose**: Validate every setting value against the GTS value type that forms the left half of the setting key, as hard checks rather than advisory ones, and expose the resolved trait set that both drives client rendering and determines whether a setting is secret-backed. The service consumes GTS types; it never authors them.

- **Depends On**: `cpt-cf-settings-service-feature-setting-declarations`

- **Scope**:
  - Type Validator component with `TypesRegistryClient` resolved in-process through ClientHub
  - `validate_value`: structural validation against JSON Schema 2020-12, plus `format` keyword assertions and trait-driven rules (cron dialect parses, regex compiles, dynamic-enum membership, entity-reference resolves) enforced as hard checks
  - The 64 KiB serialized-JSON size cap, rejected as `ValueTooLarge`, bounding the hot cache, audit pre and post images, and apply-preview payloads
  - IEEE-754 binary64 round-trip canonicality, rejecting values that do not survive the round trip unchanged as `ValueNotCanonical`
  - `resolve_traits` returning the trait set (`secret`, `multiline`, cron dialect, dynamic-enum source, entity-reference) for rendering metadata and create-time classification
  - Field-level error reporting on validation failure
  - `SettingValue` entity and the `setting_values` table, giving resolution in 2.5 something to read

- **Out of scope**:
  - GTS type authoring and the schema registry itself, owned by the `types-registry` gear
  - Secret value storage and masking, which route through the Secret Manager
  - Re-validation at Apply time, which the design explicitly does not perform because the staged change carries the already-validated value
  - Any administrative write path for values: values are set through staged changes, so within this wave `setting_values` has no user-facing writer and override behaviour is exercisable only through seeded rows and tests

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-fr-typed-value-validation`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-principle-consume-gts`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-constraint-gts-value-validation`

- **Domain Model Entities**:
  - SettingValue
  - TraitSet, ValidationResult

- **Design Components**:

  - [ ] `p1` - `cpt-cf-settings-service-component-type-validator`

- **API**:
  - Internal validation surface consumed by declaration and value paths; no dedicated public endpoint

- **Data**:
  - `setting_values` table

### 2.5 Effective Value Resolution, Defaults and Cache &mdash; HIGH

- [ ] `p1` - **ID**: `cpt-cf-settings-service-feature-value-resolution`

- **Purpose**: Resolve the effective value of a setting by dispatching on its scope class, returning a source trace alongside the value, and serve that hot path from a local in-process cache. This is the feature that makes the service useful to consumers, and it is the dependency almost every later feature waits on.

- **Depends On**: `cpt-cf-settings-service-feature-typed-value-validation`

- **Scope**:
  - Value Resolver operations `resolve`, `resolve_bulk`, and `effective_source`
  - Scope-class dispatch: `global` reads the platform row or the Schema Default and is read-only for tenants when `tenant_visible`; `cascading` obtains ancestor ids from `TenantResolverClient` and resolves nearest-first preferring the deepest match; `local` reads only the requested tenant's row with no ancestor walk
  - `EffectiveValue` computed entity and `EffectiveSource` with the inheritance trail recording which scopes were inspected and which supplied the value
  - Batched resolution sharing one ancestry walk per scope, with independent per-key outcomes so a mixed batch never fails wholesale
  - Needs-review fallthrough: a flagged override is skipped rather than served, resolution continues to the nearest valid ancestor or the Schema Default, and the flagged override stays visible to administrators
  - Distinct not-found outcomes: a stale key after a category rename resolves as `NotFound` with no tombstone or alias, while a retired declaration resolves as the distinct `Retired`
  - Revert-to-default resolution semantics, with the Schema Default independent of any override and never destroyed by setting or clearing one
  - Cache `get`, `populate`, and `invalidate` keyed by `(key, scope)`, with key-wide eviction for cascading declarations so descendants re-resolve lazily
  - `cache_ttl_seconds` backstop, default 30 s, owned by this cache
  - Hierarchy-change eviction for cascading declarations, noting that the Tenant Resolver does not publish that signal today so the TTL is currently the only backstop after a re-parent

- **Out of scope**:
  - The cross-replica `cache_invalidate` broadcast and its bounded-staleness guarantee, which is Apply-side and carries the `cpt-cf-settings-service-fr-replica-coherence` traceability gap noted in §1
  - Tenant override writes and the cascading-impact warning
  - The revert **action** as an administrative operation, which is staged like any other value change; only the resolution semantics of defaults are here

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-fr-cascading-inheritance`
  - [ ] `p1` - `cpt-cf-settings-service-fr-defaults-revert`
  - [ ] `p1` - `cpt-cf-settings-service-nfr-performance-read-cache`
  - [ ] `p1` - `cpt-cf-settings-service-nfr-efficiency-live-read`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-principle-single-ancestry-source`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-constraint-effective-on-next-read`

- **Domain Model Entities**:
  - EffectiveValue (computed, not persisted)
  - EffectiveSource

- **Design Components**:

  - [ ] `p1` - `cpt-cf-settings-service-component-value-resolver`
  - [ ] `p1` - `cpt-cf-settings-service-component-cache-and-invalidation`

- **API**:
  - GET /v1/settings/{key}/effective
  - GET /v1/settings/effective (bulk)
  - SDK `SettingsReaderClient` in-process read path

- **Sequences**:

  - `cpt-cf-settings-service-seq-effective-value-read`

- **Data**:
  - Reads `setting_declarations` and `setting_values`; adds no table of its own

---

## 3. Feature Dependencies

```text
cpt-cf-settings-service-feature-gear-foundation
    ↓
cpt-cf-settings-service-feature-category-management
    ↓
cpt-cf-settings-service-feature-setting-declarations
    ↓
cpt-cf-settings-service-feature-typed-value-validation
    ↓
cpt-cf-settings-service-feature-value-resolution
```

**Dependency Rationale**:

- `cpt-cf-settings-service-feature-gear-foundation` has no dependency and is the only feature that can start against an empty crate tree: `gears/settings-service/` currently contains documentation and no Rust source at all.
- `cpt-cf-settings-service-feature-category-management` requires the foundation: it is the first domain entity and needs persistence, REST and OData infrastructure, the `PolicyEnforcer` PEP, and error mapping before it can expose an endpoint.
- `cpt-cf-settings-service-feature-setting-declarations` requires categories: a declaration carries a non-null `category_id`, and its key embeds the category slug. The dependency runs the other way too in one respect worth planning around — the foreign key that makes 2.2's no-orphan rule enforceable at the database level is created here, so the no-orphan behaviour cannot be end-to-end tested until 2.3 lands.
- `cpt-cf-settings-service-feature-typed-value-validation` requires declarations: the value type is the left half of the declaration key, so there is nothing to validate against until declarations exist. Declaration creation in turn calls the validator for its Schema Default, which is why the two are adjacent rather than independent.
- `cpt-cf-settings-service-feature-value-resolution` requires typed value validation: the resolution chain terminates in the Schema Default, and every value it walks must already be a validated typed value stored in `setting_values`.

**Parallelism within the wave**: the chain is strictly sequential, so the wave does not parallelize across features. Work does parallelize inside 2.1, where the SDK crate, persistence harness, REST and OData infrastructure, error mapping, and the Audit Emitter are largely independent of one another.

**What unblocks next**: 2.3 unblocks module-contributed declarations and their reconciler. 2.4 unblocks secret values. 2.5 unblocks tenant overrides, staged changes, and search, which are mutually independent and can then run in parallel; Apply follows staged changes, and audit-coverage assertion follows Apply.
