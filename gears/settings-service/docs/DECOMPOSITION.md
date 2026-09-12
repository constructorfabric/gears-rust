<!-- Created: 2026-08-10 by Constructor Tech -->
<!-- Updated: 2026-09-05 by Constructor Tech -->

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
  - [2.6 Audit Store and History &mdash; HIGH](#26-audit-store-and-history-mdash-high)
  - [2.7 Tenant Access Restrictions &mdash; HIGH](#27-tenant-access-restrictions-mdash-high)
  - [2.8 Validate and Set Values &mdash; HIGH](#28-validate-and-set-values-mdash-high)
  - [2.9 Secret Values &mdash; HIGH](#29-secret-values-mdash-high)
  - [2.10 Module-Contributed Declarations &mdash; HIGH](#210-module-contributed-declarations-mdash-high)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

## 1. Overview

This is a **partial decomposition, extended in waves**. It covers **R1** of DESIGN.md §2.3 — the release whose defining property is that it depends on no gear that does not exist — in two waves of entries. It is not a complete split of the DESIGN, and it does not claim full requirement coverage; what it leaves out is listed at the end of this section with the release each item belongs to, so nothing is dropped silently.

**Selection rule**: a feature qualifies for a wave only if its dependency closure lies entirely within the waves decomposed so far, and only if every platform primitive it needs exists today. The R2 and R3 items wait on things this gear does not own — verified machine caller identity, the platform-wide elevated session, the platform Audit Subsystem, a tenant-deleted signal, per-feature licence entitlement, and the activation machinery — and are therefore not decomposed here.

**Wave 1 — the read path (2.1–2.5).** Foundation, then the category taxonomy, then declarations, then value validation, then effective-value resolution: a chain with no edge pointing outside the set. After 2.5 the service is functionally alive on its read path. An administrator can create a category and declare a typed setting against a curated GTS value type with a Schema Default, and any consumer can resolve that setting's effective value for a scope, with the inheritance trail and the hot-path cache behind it.

**Wave 2 — the write path and the rest of R1 (2.6–2.10).** The gear-local audit store, tenant access restrictions, validate-then-set value writes with the step-up gate built in this gear, secret values through the Credential Store, and module-contributed declarations. After 2.10 the service is R1-complete: an administrator sets, reverts, removes and clones values at any scope within their subtree, each write validated, step-up-verified where the declaration demands it, refused when stale, and audited in the same transaction; an ancestor restricts a descendant's access to a setting; a gear contributes declarations on every boot; and a consuming service resolves a secret's plaintext machine-only, with a secret-use record for each resolution.

**Coverage** — stated precisely because a partial decomposition must not imply completeness, and because `cfs validate` does not enforce requirement coverage in a DECOMPOSITION (`fr.references.DESIGN` carries `coverage = true`; the DECOMPOSITION reference does not):

| Element | Covered here | Total | Remaining for later waves |
|---|---|---|---|
| PRD functional requirements | 19 | 28 | 9 |
| PRD non-functional requirements | 9 | 9 | 0 |
| DESIGN components | 10 | 11 | 1 |
| DESIGN principles | 8 | 8 | 0 |
| DESIGN constraints | 11 | 13 | 2 |
| DESIGN sequences | 2 | 3 | 1 |

One requirement is counted as covered while being split across releases: `cpt-cf-settings-service-fr-subject-scoped-values` asks for the subject identity model from v1 and lets resolution over subjects phase; the identity model — columns, partial unique indexes, subject-aware API shape — is in 2.4, and the resolution half is R2.

**Alignment with the rebased PRD and DESIGN.** The PRD and DESIGN moved while wave 1 was being implemented, and the wave-1 entries and their FEATUREs were reconciled to them rather than to the drafts they were carved from. The changes that reach this document:

- **A setting key is a GTS type identifier**, `gts.cf.core.settings.setting_type.v1~<vendor>.<package>.<category>.<name>.v1~`, derived from an abstract base the Settings gear owns and registers at init ([ADR-002](./ADR/ADR-002-setting-key-gts-type-id.md)). The value type is a separate fact of the declaration (`value_type_id`), no longer a half of the key, which is what lets an authorization policy name a setting or a wildcarded subtree of settings as a resource. ADR-001, which made the key an instance identifier, is retired.
- **Validate, then set** replaces the staged-change-and-Apply model. A value operation takes effect when the caller sets it, after inline validation and an `If-Match` check; a read-only validate call reports what a value would do; there is no pending state and no separate activation step (`cpt-cf-settings-service-fr-set-value`, `cpt-cf-settings-service-fr-validate-before-set`).
- **Platform scope is the root tenant's id**, never `NULL` and never a sentinel: every platform-scoped row and every audit resource id carries it, so a scoped read's `AccessScope` predicate can see it and the cascade collapses to one `IN` over the ancestor ids. The `@platform` audit sentinel is gone.
- **Audit is gear-local in R1.** The `AuditSink` port takes the mutation's transaction; its R1 binding appends to the gear's own `audit_records` table, and R2 adds shipping through the transactional outbox behind the same port.
- **Step-up is built here, not awaited.** Earlier revisions of this section deferred step-up to a contract owned by `authn-resolver`. The design now specifies the default binding as this gear's own: a `StepUpVerifier` port whose OIDC/JWKS implementation checks the presented token's signature, `sub`, `auth_time` within a freshness window of at most five minutes, and `acr`/`amr` locally (DESIGN §4.2 *Value Writer*). It gates interactive writes to declarations that require elevated confirmation and the behavior-affecting declaration actions (`cpt-cf-settings-service-fr-authn-role-gating`, `cpt-cf-settings-service-fr-validate-before-set`). The only sanctioned non-verifying binding is `MockStepUpVerifier` inside the test harness.
- **Tenant access is a sparse decision, not a pair of flags.** `tenant_visible` and `tenant_overridable` are gone from the declaration; a `tenant_permissions` row of `read_only` or `hidden`, recorded by an ancestor's administrator, restricts one tenant for one setting, and absence means `overridable`. The declaration keeps two flags that gate people rather than scopes: `requires_step_up` (default `true`) and `anonymous_exposable` (default `false`, refused on `secret` or `pii`).
- **Licence gating is R2.** The `license-resolver` gear is documentation only, so the licence predicate the read surfaces will apply is left as a seam.

**Accepted narrowing — notification filtering is by subscription, not by entitlement.** `cpt-cf-settings-service-fr-consumer-activation` requires that a notification carry *"never a setting it is not entitled to read"*. [DESIGN-activation](./DESIGN-activation.md) §4.1 filters each notification to the subscriber's **own subscribed keys** and states that, under the platform trusted-caller model, subscriber identity is taken on trust — making this least-privilege by blast radius rather than an identity-enforced entitlement check. **This narrowing is intended and accepted.** It is recorded here rather than left in passing prose because it qualifies a PRD `MUST`: a consumer that subscribes to a key it could not read on the read path is still notified of that key's change. Revisit if consumers ever cease to be trusted.

**Things to settle with the DESIGN owners**, none of which blocks a wave:

- `audit_records.declaration_key` is `NOT NULL`, and the canonical resource id is built from a setting key. Category mutations have no setting key, yet 2.2 audits them through the same emitter. Either the column becomes nullable or category audit gets its own resource form; until that is decided, 2.6 keeps the emitter's interface value-centric and records category mutations with the category key in the resource field.
- DESIGN §4.3 still carries `422` in several rule tables outside the Error Response Format section, which itself states that a validation rejection is the canonical invalid-argument category rendering as `400` with no `422` category. The FEATUREs and code follow the canonical-error ADRs and use `400`; the reason codes (`DefaultRequired`, `ValueTooLarge`, `SecretNotCloneable`, …) are kept.
- DESIGN §4.8 *Bootstrap* has the gear seed "a minimal category set" at startup, but no section names the set. The init step is specified without its contents until it does.
- DESIGN §4.9 has every consumed client declared with `#[toolkit::consumes]`, which requires the provider SDK's REST projection to exist. `tenant-resolver-sdk` has none, so the tenant resolver is fetched from the hub at first use instead — the same thing in the Embedded profile R1 is limited to, but the declaration has to land in that SDK before the out-of-process profiles of R2.
- The default for an omitted `tenant` is stated twice in §4.3 and differently: "platform scope" on the read and write path notes, "the caller's own tenant" in the set rules. The FEATUREs assume the caller's own tenant, which for a platform administrator is the root tenant and therefore platform scope — the one reading that satisfies both sentences.
- DESIGN names no authorization action for the PII entitlement it gates unmasked reads on (§1.3 `DataClassification`, §4.2 *Secret Manager* `mask`). The read surface asks the `PolicyEnforcer` for `read_unmasked` on the value resource and unmasks `pii` only when it is granted; the action name is this decomposition's, to be confirmed by the authorization owners. An omitted `tenant` resolves to the caller's own tenant, which for the root is platform scope, so both §4.3 wordings hold.
- DESIGN leaves how the step-up assertion travels to the implementation (§4.2 *Value Writer*: the parameter MAY be folded into the bearer token). The write surface reads an `X-Step-Up-Token` header and, absent, checks the bearer token itself, which a session re-authenticated just now satisfies through its own `auth_time`; the header name is this decomposition's, to be confirmed against `authn-resolver`'s step-up contract when it lands. With no `step_up` configuration section nothing is bound and every write to a declaration that requires step-up refuses; the demo catalogue opts its settings out of step-up except `api_token`, so the example server can exercise writes without an identity provider.
- §4.3 says the read's `last_change_at` is the ETag a write submits, but that field is the leak-safe maximum over the declaration and the resolved row, which may be an ancestor's. The FEATUREs guard a write on the target scope's **own** row — its `last_change_at`, or an absent-state tag when no row exists — and return that tag in `ETag`, distinct from the recency in the body.
- DESIGN §4.2 *Secret Manager* names no principal for the gear's own calls to the Credential Store. The Secret Manager presents a fixed settings-service principal, a name-based UUID, in the target tenant with `private` sharing, so that only this gear reads an entry back; a real deployment has to grant that principal `read`, `write` and `delete` on the generic secret type in the authorization policy, which the static authorizer of the e2e profile grants implicitly. And §4.8 defines no action for the machine path's per-setting check, so `resolve_secret` asks the `PolicyEnforcer` for `read` on the value resource with the declaration id as the resource id.
- DESIGN §4.2 *Secret Manager* has `store_secret` write "under a deterministic path". The implementation derives a deterministic prefix from the key and the tenant but stores each write under its own reference, create-only, because the Credential Store cannot join the row's transaction (§4.2 *Set atomicity model*) and toolkit-db refuses a fresh connection while one is open on the task: a write refused after the store leg must not have altered the live entry, so it releases the entry it created, and a write that lands releases the superseded one, the "applied-away" case `delete_secret` names.
- `audit_records.operation` is checked to the value operations only; restriction changes (2.7) and declaration and category mutations need entries too, or a second vocabulary. The §4.3 history row still spells its parameter `scope={path}` although scopes are tenant ids everywhere else.
- The administrative read carries no effective tenant access for the caller, so a console cannot tell from the response whether to offer an editor; the FEATUREs keep the design's shape and leave the field to the DESIGN owners.

**Not decomposed yet — R2 and R3, by owning component:**

| Requirement or element | Release | Where it will live |
|---|---|---|
| `cpt-cf-settings-service-fr-service-writes` | R2 | Value Writer (2.8) — the gate table already refuses a service principal on a `requires_step_up` declaration; the authorized service-write SDK operation `set_value` is R2 |
| `cpt-cf-settings-service-fr-anonymous-exposable` | R2 | the anonymous read route `GET /settings-service/v1/public/settings`; the declaration flag and its schema check ship in 2.3 |
| `cpt-cf-settings-service-fr-feature-license-gating`, `cpt-cf-settings-service-constraint-licence-entitlement-fail-closed` | R2 | the licence predicate seam in 2.3 and 2.5 reads; waits on the License Resolver gear |
| `cpt-cf-settings-service-fr-search-discoverability`, `cpt-cf-settings-service-component-search` | R2 | a search feature over the trigram indexes 2.2–2.4 already create |
| `cpt-cf-settings-service-fr-standard-advanced-mode` | R2 | mode-filtered reads; the preference is a field addition on the `simple-user-settings` contract |
| `cpt-cf-settings-service-fr-file-valued-settings`, `cpt-cf-settings-service-constraint-files-by-reference`, `cpt-cf-settings-service-seq-file-valued-setting` | R2 | a file-reference value type over the platform file store |
| `cpt-cf-settings-service-fr-subject-scoped-values` — resolution half | R2 | Value Resolver, over the identity model 2.4 delivers |
| `cpt-cf-settings-service-fr-replica-coherence` | R2 | the `cache_invalidate` broadcast from the Value Writer; `cache_ttl_seconds` in 2.5 is the backstop |
| tenant-deleted cleanup (DESIGN §4.4) | R2 | consumed once Account Management publishes the signal |
| `cpt-cf-settings-service-fr-consumer-activation` | R3 | [DESIGN-activation](./DESIGN-activation.md) — subscription, filtered notification, delivery until confirmed |
| `cpt-cf-settings-service-fr-dependency-group-declaration` — enforcement | R3 | the all-or-nothing set of a group's members; the group **declaration** is in 2.3 |
| `cpt-cf-settings-service-fr-domain-affinity-filtering` | R3 | reads and search filtered by the caller's administrative domain |

## 2. Entries

### 2.1 Gear Foundation, SDK Contracts and Cross-Cutting Infrastructure &mdash; HIGH

- [ ] `p1` - **ID**: `cpt-cf-settings-service-feature-gear-foundation`

- **Purpose**: Establish the `settings-service-sdk` crate and the gear scaffold every later feature builds on: domain models and the `SettingKey` value object, the Settings Reader and Contribution client traits, the error taxonomy and its RFC-9457 Problem mapping, PostgreSQL persistence with migration tooling, REST and OData infrastructure, the `PolicyEnforcer` PEP with the `StepUpVerifier` port, and the Audit Emitter that all mutating features publish through.

- **Depends On**: None

- **Scope**:
  - SDK crate (`settings-service-sdk`): domain models, the `SettingKey` value object — a GTS type id under the Settings gear's abstract base `gts.cf.core.settings.setting_type.v1~` ([ADR-002](./ADR/ADR-002-setting-key-gts-type-id.md)), parsed once and never re-normalized — the `SettingsReaderClient` trait (DESIGN §4.5), the Contribution client trait,  the change-notification and outcome types the Settings Activation consumes, and the error taxonomy
  - Gear scaffold: `#[toolkit::gear]` annotated gear with `deps = [types_registry]` — the one client it calls during its own init — and every consumed client (authorization resolver, tenant resolver, later the credential store and event broker) fetched at first use and declared with `#[toolkit::consumes]` where its SDK carries a REST projection (DESIGN §4.9; the tenant resolver's does not yet, see §1); ClientHub registration for the SDK client traits; registration of the gear's GTS control-plane schemas and the abstract `setting_type` base, and the idempotent seed of the minimal category set, at init (DESIGN §4.8 *Bootstrap*). The root tenant's id — platform scope (DESIGN §4.1, §4.7) — is learned from the tenant resolver on first use and kept
  - Persistence: SeaORM entity scaffolding, `SecureConn` and `DBRunner` wiring, migration harness
  - Error mapping: `DomainError` to Problem (RFC-9457) across the canonical error categories of DESIGN §4.3 — a validation rejection is invalid-argument and renders as `400`; `428`/`412` are explicit transport overrides on the `If-Match` preconditions
  - REST infrastructure: `OperationBuilder` wiring, OData `$filter`, `$select`, and `$orderby` parsing, pagination helpers, and `If-Match`/ETag plumbing
  - AuthN and AuthZ: the `PolicyEnforcer` PEP pattern, `AccessScope` derived from PDP constraints, and the `StepUpVerifier` port — declared in the domain, adapted in infra — whose default OIDC/JWKS binding verifies a presented step-up token locally; it is bound in 2.8, the first path that refuses without it, and picked up there by the behavior-affecting declaration actions of 2.3
  - Audit Emitter: the `AuditSink` port taking the mutation's own transaction and `AccessScope`, and event publication; the gear-local binding is entry 2.6, and the tracing stand-in until then exercises the emitter's callers without satisfying the audit DoD
  - Deployment-owned bootstrap configuration delivered through ToolKit config at gear init (DESIGN §4.9), never as a managed setting

- **Out of scope**:
  - Every domain service and its REST handlers, which are features 2.2 through 2.10
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
  - `SettingKey` value object
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

- **Purpose**: Provide the flat category taxonomy every setting declaration is filed under, with globally unique `key` and `name`, optional administrative-domain binding, display ordering, and no-orphan deletion. The category `key` is load-bearing beyond grouping: it becomes the `<category>` token of every setting key declared under it — the third segment of the key's derived half, for admin-authored and module-contributed settings alike — which is why it is validated against the path separator rather than treated as free text, and why it is immutable after creation.

- **Depends On**: `cpt-cf-settings-service-feature-gear-foundation`

- **Scope**:
  - `Category` entity and the `categories` table with `uq_category_key` and `uq_category_name` (DESIGN §4.1, §4.7)
  - `create_category`, `update_category`, `delete_category`, `get_category`, and `list_categories` (DESIGN §4.2)
  - Five REST endpoints under `/settings-service/v1/categories` (DESIGN §4.3)
  - Per-resource-type CRUD authorization on `gts.cf.core.settings.category.v1~` through the `PolicyEnforcer`; no step-up on any category operation, since an empty category holds no setting and its removal changes no effective value
  - `key` validation rejecting `/` and enforcing the 1..128 bound; `key` refused on update rather than merely discouraged, because an in-place change would re-key every declaration filed under it with nothing left pointing at the old name
  - No-orphan deletion returning `409 CategoryNotEmpty` while any declaration references the category, including `retired` declarations
  - `If-Match` and ETag optimistic concurrency on `PATCH` and `DELETE`
  - Domain-filtered, visibility-gated, paginated list ordered by `sort_order` then `name`
  - Category mutations audited at platform scope — the root tenant's id — through the emitter of 2.1
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
  - POST /settings-service/v1/categories
  - GET /settings-service/v1/categories
  - GET /settings-service/v1/categories/{id}
  - PATCH /settings-service/v1/categories/{id}
  - DELETE /settings-service/v1/categories/{id}

- **Data**:
  - `categories` table with `uq_category_key`, `uq_category_name`, and `idx_categories_name_trgm`

### 2.3 Setting Declarations and Scope Class &mdash; HIGH

- [ ] `p1` - **ID**: `cpt-cf-settings-service-feature-setting-declarations`

- **Purpose**: Introduce the setting declaration as an entity distinct from its value, keyed by a GTS type identifier `gts.cf.core.settings.setting_type.v1~<vendor>.settings.<category>.<name>.v1~` registered under the Settings gear's abstract base ([ADR-002](./ADR/ADR-002-setting-key-gts-type-id.md)), and give each declaration a first-class scope class from which cascade and override behaviour is derived deterministically rather than configured per setting. Establishes the mutation-class discipline that keeps declaration edits from silently changing a live setting's resolution.

- **Depends On**: `cpt-cf-settings-service-feature-category-management`

- **Scope**:
  - `SettingDeclaration` entity and the `setting_declarations` table, including the `category_id` foreign key declared `ON DELETE RESTRICT`, which is the authoritative guard behind 2.2's no-orphan rule
  - Key construction: the admin supplies a `vendor`, a leaf `name`, and separately the `value_type_id` of a curated toolkit value type; the service composes the derived type `<vendor>.settings.<category>.<name>.v1~` under the fixed base, validates the whole through the same parser a supplied key goes through, rejects grammar violations with `400`, and registers the composed type in the types registry — derived from the base, composed with the value type, carrying no `default` — before inserting the row
  - Uniqueness: `uq_declaration_key` globally, and the partial `uq_declaration_category_slug` on `(category_id, leaf_slug) WHERE status = 'active'`, so a retired predecessor does not hold its name against its successor
  - `ScopeClass` (`global`, `cascading`, `local`) and the scope-class engine deriving cascade and override behaviour; a `global` setting has no tenant-scoped value at all
  - `DeclarationSource`, `DeclarationStatus`, and `DomainAffinity` enums
  - Schema Default: `default_value` is non-null so the resolution chain always terminates, is validated against `value_type_id`, and supports structured object and array defaults
  - Trait-derived classification: `has_secret_trait` resolved from the value type, `data_classification` with `secret` derived from the trait and never author-supplied, rejecting an author-supplied `secret` on a non-secret type
  - Rejecting a non-empty `default_value` on a secret-trait type
  - The two people-gating flags: `requires_step_up`, defaulting to the protective `true`, and `anonymous_exposable`, defaulting to `false` and refused — in the handler and by a schema check — on a `secret` or `pii` classification
  - The mutation-class discipline: descriptive metadata immediate under `update` plus `If-Match`; behavior-affecting fields (`default_value`, value type, `scope_class`) immutable and rejected with `400`; retire and reactivate immediate but step-up gated through the port delivered in 2.1 and bound in 2.8; classification tightening immediate, loosening step-up gated; clearing `requires_step_up` or enabling `anonymous_exposable` step-up gated whatever the flag currently says
  - Retire as soft-delete setting `status = retired`, retaining values in `setting_values` while excluding them from resolution, with re-declare-to-revive recovery and evolve-by-re-declaring to the next free major on the same version-stripped path
  - Dependency Group and cross-setting constraint **declaration**
  - Declaration REST surface, visibility- and domain-gated with the predicate applied inside the query, returning the `key`, its `value_type_id`, and resolved traits for client rendering, and leaving the seam through which the licence predicate joins in R2

- **Out of scope**:
  - Module-contributed declarations and the Contribution Reconciler, which own the contributed write path (2.10)
  - Tenant access restrictions, which are a separate decision recorded in `tenant_permissions` (2.7)
  - Value writes, validation internals, and secret value storage
  - Setting a Dependency Group's members all-or-nothing, which is the value write path's and R3; only the declaration of the group is here
  - The anonymous read route, which is R2; only the flag and its schema check are here

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-fr-setting-scope-class`
  - [ ] `p3` - `cpt-cf-settings-service-fr-dependency-group-declaration`
  - [ ] `p2` - `cpt-cf-settings-service-nfr-versatility-gts-scope-class`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-principle-declaration-value-split`
  - [ ] `p1` - `cpt-cf-settings-service-principle-scope-class-derivation`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-constraint-key-is-gts-type-id`

- **Domain Model Entities**:
  - SettingDeclaration
  - ScopeClass
  - DeclarationSource, DeclarationStatus, DomainAffinity

- **Design Components**:

  - [ ] `p1` - `cpt-cf-settings-service-component-declaration-management`

- **API**:
  - POST /settings-service/v1/declarations
  - GET /settings-service/v1/declarations
  - GET /settings-service/v1/declarations/{id}
  - PATCH /settings-service/v1/declarations/{id}
  - DELETE /settings-service/v1/declarations/{id}

- **Data**:
  - `setting_declarations` table with `uq_declaration_key`, the partial `uq_declaration_category_slug`, the `categories` foreign key `ON DELETE RESTRICT`, the `ck_declaration_exposable_not_sensitive` and secret-equivalence checks, and the partial active-status index

### 2.4 Typed Value Validation &mdash; HIGH

- [ ] `p1` - **ID**: `cpt-cf-settings-service-feature-typed-value-validation`

- **Purpose**: Validate every setting value against the GTS value type its declaration names by `value_type_id`, as hard checks rather than advisory ones, and expose the resolved trait set that both drives client rendering and determines whether a setting is secret-backed. The service consumes GTS types; it never authors them.

- **Depends On**: `cpt-cf-settings-service-feature-setting-declarations`

- **Scope**:
  - The `TypeValidator` port, declared in the gear's domain layer — not in the SDK — and bound over `TypesRegistryClient` resolved in-process through ClientHub
  - `validate_value`: structural validation against JSON Schema 2020-12, plus `format` keyword assertions and trait-driven rules (cron dialect parses, regex compiles, dynamic-enum membership, entity-reference resolves) enforced as hard checks
  - The 64 KiB serialized-JSON size cap, rejected as `ValueTooLarge`, bounding the hot cache, audit pre and post images, and validate-before-set report payloads
  - IEEE-754 binary64 round-trip canonicality, rejecting values that do not survive the round trip unchanged as `ValueNotCanonical`
  - `resolve_traits` returning the trait set (`secret`, `multiline`, cron dialect, dynamic-enum source, entity-reference) for rendering metadata and create-time classification
  - Field-level error reporting on validation failure
  - `SettingValue` entity and the `setting_values` table, giving resolution in 2.5 something to read: a non-null `tenant_id` that is the root tenant's id for platform scope, the nullable `subject_type`/`subject_id` pair named by both halves or neither, and the two partial unique indexes `uq_value_scope` and `uq_value_scope_subject` — the subject identity model `cpt-cf-settings-service-fr-subject-scoped-values` requires from v1

- **Out of scope**:
  - GTS type authoring and the schema registry itself, owned by the `types-registry` gear
  - Secret value storage and masking, which route through the Secret Manager (2.9)
  - Any administrative write path for values: the Value Writer (2.8) is what sets, reverts, removes and clones values, so within this wave `setting_values` has no user-facing writer and override behaviour is exercisable only through seeded rows and tests
  - Resolution over subject scopes, which is R2; only the identity model is here

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-fr-typed-value-validation`
  - [ ] `p2` - `cpt-cf-settings-service-fr-subject-scoped-values`

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
  - `setting_values` table with `uq_value_scope`, `uq_value_scope_subject`, the exactly-one and secret-equivalence checks, the both-or-neither subject check, and the partial `idx_values_needs_review`

### 2.5 Effective Value Resolution, Defaults and Cache &mdash; HIGH

- [ ] `p1` - **ID**: `cpt-cf-settings-service-feature-value-resolution`

- **Purpose**: Resolve the effective value of a setting by dispatching on its scope class, returning a source trace alongside the value, and serve that hot path from a local in-process cache. This is the feature that makes the service useful to consumers, and it is the dependency almost every later feature waits on.

- **Depends On**: `cpt-cf-settings-service-feature-typed-value-validation`

- **Scope**:
  - Value Resolver operations `resolve`, `resolve_bulk`, and `effective_source`
  - Scope-class dispatch: `global` reads the root-tenant row or the Schema Default and is served read-only to a tenant whose effective access for the setting is not `hidden`; `cascading` obtains ancestor ids from `TenantResolverClient` — a chain that begins at the root tenant, so platform scope needs no special case — and resolves nearest-first preferring the deepest match; `local` reads only the requested tenant's row with no ancestor walk
  - `EffectiveValue` computed entity and `EffectiveSource` with the inheritance trail recording which scopes were inspected and which supplied the value
  - Batched resolution sharing one ancestry walk per scope, with independent per-key outcomes so a mixed batch never fails wholesale — the bulk read by category or by key set, a key the caller may not see or that does not exist reported in its own entry
  - Needs-review fallthrough: a flagged override is skipped rather than served, resolution continues to the nearest valid ancestor or the Schema Default, and the flagged override stays visible to administrators
  - Distinct not-found outcomes: a stale key after a category rename resolves as `NotFound` with no tombstone or alias, while a retired declaration resolves as the distinct `Retired`
  - The administrative read surface over the resolver: `GET …/settings/{key}` with source, trail, leak-safe recency, the scope's own review flag and the value state tag in `ETag`; `GET …/settings` by category or key set with per-key outcomes, and the needs-review listing over the caller's subtree; hidden settings absent, standalone descendants outside the subtree, values masked by classification
  - The in-process `SettingsReaderClient` over the resolver, registered into `ClientHub` at init, with no REST contract published and startup refused on a remote binding while the release is Embedded-only
  - Revert-to-default resolution semantics, with the Schema Default independent of any override and never destroyed by setting or clearing one
  - Cache `get`, `populate`, and `invalidate` keyed by `(key, scope)`, with key-wide eviction for cascading declarations so descendants re-resolve lazily
  - `cache_ttl_seconds` backstop, default 30 s, owned by this cache
  - Hierarchy-change eviction for cascading declarations, noting that the Tenant Resolver does not publish that signal today so the TTL is currently the only backstop after a re-parent

- **Out of scope**:
  - The cross-replica `cache_invalidate` broadcast and its bounded-staleness guarantee, which ship in R2 when several replicas become the normal case (`cpt-cf-settings-service-fr-replica-coherence`)
  - Tenant override writes, the cascading-impact report, and the validate-before-set check (2.8)
  - The revert **action** as an administrative operation, which is a value write like any other (2.8); only the resolution semantics of defaults are here

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-fr-cascading-inheritance`
  - [ ] `p2` - `cpt-cf-settings-service-fr-defaults-revert`
  - [ ] `p1` - `cpt-cf-settings-service-fr-bulk-effective-read`
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
  - GET /settings-service/v1/settings/{key}?tenant={tenant_id}
  - GET /settings-service/v1/settings?tenant={tenant_id} (bulk by category or key set, browse)
  - SDK `SettingsReaderClient` in-process read path (`get_effective`, `get_effective_bulk`)

- **Sequences**:

  - `cpt-cf-settings-service-seq-effective-value-read`

- **Data**:
  - Reads `setting_declarations`, `setting_values`, and — for the visibility rule — `tenant_permissions`; adds no table of its own

### 2.6 Audit Store and History &mdash; HIGH

- [ ] `p1` - **ID**: `cpt-cf-settings-service-feature-audit-store`

- **Purpose**: Replace the tracing stand-in behind the Audit Emitter with the R1 sink the design specifies: an append-only `audit_records` table written inside the mutation's own transaction through `AuditSink::append(txn, scope, record)`, and the per-(setting, scope) history read served from it. Audit is a show-stopper here — a mutation that cannot record itself must not take effect — and a local sink fails only when the database does, which is when the mutation could not have committed anyway.

- **Depends On**: `cpt-cf-settings-service-feature-setting-declarations`

- **Scope**:
  - The `audit_records` table (DESIGN §4.7): `resource`, `declaration_key`, `tenant_id` (set for a record about a scope, `NULL` for one about a definition), `operation` in `create`, `change`, `revert`, `remove`, `clone`, `secret_use`, `actor` with its own `actor_classification`, masked `pre_value`/`post_value`, `outcome`, `request_id`, nullable `change_set_id`, `occurred_at`, `retain_until`; append-only, no `UPDATE` and no `DELETE` outside retention pruning
  - The canonical resource id `cf.settings:{key}@{tenant_id}` — the root tenant's id for platform scope — produced by one formatter shared by the write and the history read, so the two cannot drift
  - Writing the record as the last step before commit of the mutation's transaction, through the same `AccessScope`-scoped path as every other write; a failed write rolls the mutation back and rejects it `503`
  - Masking before the record is built: a `secret`-classified value is never written in plaintext, and the actor identity carries its `public`/`pii` classification for the read side to honour
  - `GET /settings-service/v1/settings/{key}/history` querying `(declaration_key, tenant_id)` on `idx_audit_scoped`, paginated, newest first, with no second masking implementation and no reveal path
  - `idx_audit_retention` on `retain_until` for pruning; `retain_until` carried on every record from the start, the store's configured default applying when absent
  - Retiring the `TracingAuditEmitter` and closing the audit DoDs 2.1 and 2.2 left open

- **Out of scope**:
  - Shipping records to the platform Audit Subsystem through the transactional outbox, which is R2 and an addition behind the same port
  - Cross-gear audit queries and retention beyond the online window, which have no destination until that subsystem exists
  - A pruning schedule: the design fixes that `retain_until` is carried and that deletes outside pruning are forbidden, not when pruning runs
  - The mutations themselves, which remain the business of the features that perform them

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-fr-audit-mutations`
  - [ ] `p2` - `cpt-cf-settings-service-nfr-scale-growth`

- **Design Constraints Covered**:
  - Completes `cpt-cf-settings-service-constraint-audit-and-events`, listed under 2.1 where the port is declared

- **Domain Model Entities**:
  - AuditRecord
  - AuditSink port and its gear-local binding
  - Canonical audit resource id

- **Design Components**:
  - Completes `cpt-cf-settings-service-component-audit-emitter`, listed under 2.1 where the emitter is declared

- **API**:
  - GET /settings-service/v1/settings/{key}/history?tenant={tenant_id}

- **Data**:
  - `audit_records` table with `idx_audit_scoped` and the partial `idx_audit_retention`

### 2.7 Tenant Access Restrictions &mdash; HIGH

- [ ] `p1` - **ID**: `cpt-cf-settings-service-feature-tenant-access`

- **Purpose**: Let an ancestor's administrator narrow what one descendant may do with one setting — `read_only` or `hidden`, recorded sparsely, absence meaning `overridable` — and make the resulting effective access the single answer every read and write consults. Access is narrowing-only along the root-to-self chain, the platform has no row of its own because nobody is above the root to record one, and a standalone tenant is sealed from above without losing what flows down into it.

- **Depends On**: `cpt-cf-settings-service-feature-value-resolution`

- **Scope**:
  - `TenantAccessRestriction` entity and the `tenant_permissions` table with `uq_tenant_permission` on `(declaration_id, tenant_id)` and `idx_tenant_permission_tenant`; `access IN ('read_only', 'hidden')`; rows survive a soft-retire and cascade only on a hard delete
  - `set_restriction`, `clear_restriction`, `resolve_access`, and `list_restrictions` (DESIGN §4.2 *Tenant Access*): the `delegate` permission on the setting's key, a strict-descendant target — a caller restricting itself could lift the restriction again — and a row stored even when a stricter ancestor already dominates it
  - Effective access as the strictest row on the chain, `overridable < read_only < hidden`, with the read reporting which tenant supplied it
  - `If-Match` on `PUT` and `DELETE` against the stored row's ETag or the absent-state ETag the `GET` returns; comparison and mutation atomic
  - Eviction of the target and all its descendants on any access change, independent of scope class, because their effective access may change
  - Consumption by the read path: a setting whose effective access is `hidden` is reported as absent, never as forbidden, to the tenant and on the permission endpoints alike; and by the write path in 2.8, where the **caller's own** access must be `overridable`
  - The standalone seam: inheritance into a tenant `tenant-resolver` marks standalone is unchanged, while every administrative surface above it — single and bulk read, history, listing, and the impact report's `changed[]` and `total_changed` — omits it, and an ancestor that cannot read it cannot write to it
  - Restrictions audited through 2.6

- **Out of scope**:
  - Value writes and the write-side use of the caller's access, which is 2.8
  - Any per-setting flag on the declaration: `tenant_visible` and `tenant_overridable` no longer exist
  - Search and its filters, which are R2

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-fr-tenant-scope-enforcement`
  - [ ] `p2` - `cpt-cf-settings-service-fr-per-setting-access`
  - [ ] `p2` - `cpt-cf-settings-service-fr-barrier-default-seam`
  - [ ] `p1` - `cpt-cf-settings-service-nfr-scope-isolation`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-constraint-scope-hierarchy-paths`

- **Domain Model Entities**:
  - TenantAccessRestriction
  - TenantAccess (`overridable`, `read_only`, `hidden`)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-settings-service-component-tenant-permission`

- **API**:
  - GET /settings-service/v1/settings/{key}/permissions?tenant={tenant_id}
  - PUT /settings-service/v1/settings/{key}/permissions?tenant={tenant_id}
  - DELETE /settings-service/v1/settings/{key}/permissions?tenant={tenant_id}
  - GET /settings-service/v1/settings/{key}/permissions

- **Data**:
  - `tenant_permissions` table with `uq_tenant_permission` and `idx_tenant_permission_tenant`

### 2.8 Validate and Set Values &mdash; HIGH

- [ ] `p1` - **ID**: `cpt-cf-settings-service-feature-value-writes`

- **Purpose**: Deliver the write path: a read-only check of what a value would do, and set, revert, remove and clone operations that validate inline, refuse a stale write, take effect when the caller sets them, and are audited in the same transaction. This is where the `StepUpVerifier` port acquires its default OIDC/JWKS binding, and where the two gates — authorization, asked of every caller, and elevated confirmation, which only a human can answer — are kept apart.

- **Depends On**: `cpt-cf-settings-service-feature-audit-store`, `cpt-cf-settings-service-feature-tenant-access`

- **Scope**:
  - Value Writer operations `validate`, `set`, `revert`, `remove_value`, `clone`, and `cascading_impact` (DESIGN §4.2 *Value Writer*)
  - `validate`: authorize `read` at scope; report validity with field-level detail, the current effective value and its source, and for a `cascading` setting the affected descendants; stores nothing, needs no step-up, returns the same answer for the same inputs, and is never required before a write
  - `set`: authorize `write` **first**; then, for an interactive caller, verify step-up once per request when any target declaration requires it, and refuse a service principal for such a declaration before validation; require the target to be the caller's own tenant or a descendant, reject a tenant-scoped write to a `global` setting, and require the caller's own effective access to be `overridable`; validate the value and the `If-Match` ETag; commit the value and its audit record in one transaction **per change**; then evict the local cache, then publish — commit, evict, publish, in that order
  - Per-item results with no atomicity across items: at most 500 changes per request, each reporting old value, new value, scope, and success or the error that rejected it
  - `revert` clearing the scope's override and reporting the resulting fallback — nearest ancestor for a tenant scope, Schema Default at the root — with `validate` reporting the same fallback beforehand
  - `clone` authorizing `read` at the source and `write` at the target, both within the caller's subtree, copying the effective value with no continuing link, and refusing a `secret` setting with `SecretNotCloneable`
  - `cascading_impact` as a bounded, non-blocking preview: breadth-first over `get_descendants`, the first `limit` changed descendants (default 100, at most 500) in traversal order, `total_changed`, `scanned`, and `truncated` under a node budget of 5,000; standalone descendants omitted from the list and the count
  - The default `StepUpVerifier` binding: signature against the identity provider's JWKS, `sub` matching the session, `auth_time` within the freshness window of at most five minutes, `acr`/`amr` as required; the `401` refusal carrying the RFC 9470 challenge `insufficient_user_authentication` with `max_age`; no development or sandbox bypass, since a binding that cannot fail is the always-satisfied binding the contract rejects
  - A `change_set_id` minted per `set` request and carried on the audit records it produces
  - Publication of `event_value_changed` and `event_value_change_failed` through the Change Publisher port; in R1 no consumer notification and no cross-replica broadcast are bound, so the local eviction is the write's only cache effect and a second host would be stale for at most `cache_ttl_seconds`
  - The `settings_step_up_total` counter by operation and result, and the set-path metrics `cpt-cf-settings-service-nfr-ops-set-monitoring` requires

- **Out of scope**:
  - Writes by an authorized service principal through the SDK `set_value`, which is R2 (`cpt-cf-settings-service-fr-service-writes`); the gate table's machine column — refusal on a `requires_step_up` declaration — is enforced here
  - Dependency Group all-or-nothing sets, which are R3
  - The `cache_invalidate` broadcast and consumer `change_notification` delivery, owned by the Settings Activation and R2/R3 respectively
  - Secret plaintext storage, which `set` routes through the Secret Manager of 2.9

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-fr-set-value`
  - [ ] `p1` - `cpt-cf-settings-service-fr-validate-before-set`
  - [ ] `p1` - `cpt-cf-settings-service-fr-live-read-activation`
  - [ ] `p1` - `cpt-cf-settings-service-fr-tenant-overrides`
  - [ ] `p1` - `cpt-cf-settings-service-nfr-reliability-validated-set`
  - [ ] `p2` - `cpt-cf-settings-service-nfr-ops-set-monitoring`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-principle-write-scope`
  - [ ] `p1` - `cpt-cf-settings-service-principle-inform-not-block`

- **Design Constraints Covered**:
  - Binds `cpt-cf-settings-service-constraint-step-up-at-idp`, listed under 2.1 where the port is declared

- **Domain Model Entities**:
  - SetResult, ValidationReport, ImpactReport
  - StepUpVerifier and its OIDC/JWKS binding
  - Change set identifier

- **Design Components**:

  - [ ] `p1` - `cpt-cf-settings-service-component-value-writer`

- **API**:
  - POST /settings-service/v1/settings/{key}/validate?tenant={tenant_id}
  - PUT /settings-service/v1/settings/{key}/value?tenant={tenant_id}
  - POST /settings-service/v1/settings/batch
  - POST /settings-service/v1/settings/{key}/value/revert?tenant={tenant_id}
  - POST /settings-service/v1/settings/{key}/value/clone?tenant={tenant_id}
  - DELETE /settings-service/v1/settings/{key}/value?tenant={tenant_id}
  - GET /settings-service/v1/settings/{key}/impact?tenant={tenant_id}&limit={n}

- **Sequences**:

  - `cpt-cf-settings-service-seq-validate-and-set`

- **Data**:
  - Writes `setting_values` and `audit_records`; adds no table of its own

### 2.9 Secret Values &mdash; HIGH

- [ ] `p1` - **ID**: `cpt-cf-settings-service-feature-secret-values`

- **Purpose**: Hold `secret`-trait values in the platform Credential Store and nowhere else: the settings row carries only an opaque `secret_ref`, every administrative read shows a mask, and the sole plaintext path is machine-only through the Settings Reader, authorized per setting and audited per resolution. A lost secret is re-set, never revealed.

- **Depends On**: `cpt-cf-settings-service-feature-value-writes`

- **Scope**:
  - Secret Manager operations `store_secret`, `mask`, `resolve_plaintext`, and `delete_secret` (DESIGN §4.2 *Secret Manager*) over the `credstore` gear
  - `set` routing a secret-trait value's plaintext through `store_secret` and persisting only the returned `secret_ref`, so plaintext never enters the settings database, the cache, the search index, or the audit trail
  - Classification-aware masking in every administrative read, list, search, and audit output: `secret` always masked, `pii` masked unless the caller is authorized for unmasked PII, `public` passed through
  - `SettingsReaderClient::resolve_secret` over an opaque `SecretHandle`: per-setting authorization of the calling service, plaintext fetched from the store, one `event_secret_used` record with the value masked, plaintext never cached and never returned to an administrative caller
  - `delete_secret` when an override is removed or superseded
  - The placeholder-default rule, already enforced by 2.3 on the declaration side: a secret setting resolves to its empty placeholder, never to a credential

- **Out of scope**:
  - Verified machine caller identity, without which the machine path enforces only the deployment trust boundary and attributes secret use to the caller's declared module (DESIGN §6); R2
  - Envelope encryption inside the persistence layer as an alternative to the Credential Store, an open question with persistence and security owners
  - Any reveal endpoint, permission, event, or metric — none exists by design

- **Requirements Covered**:
  - Secret handling is a clause of `cpt-cf-settings-service-fr-typed-value-validation` (2.4) and of `cpt-cf-settings-service-fr-audit-mutations` (2.6); this entry adds no requirement of its own

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-principle-machine-only-secrets`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-constraint-secrets-by-reference`

- **Domain Model Entities**:
  - SecretHandle
  - `secret_ref` on SettingValue

- **Design Components**:

  - [ ] `p1` - `cpt-cf-settings-service-component-secret-manager`

- **API**:
  - SDK `SettingsReaderClient::resolve_secret`; no REST endpoint, since no human reveal path exists

- **Data**:
  - `setting_values.secret_ref`, tied to the `secret` classification by the schema check of 2.4; entries in the Credential Store

### 2.10 Module-Contributed Declarations &mdash; HIGH

- [ ] `p1` - **ID**: `cpt-cf-settings-service-feature-module-contributions`

- **Purpose**: Let a gear register its own setting declarations from its init on every boot, idempotently, and retire them when it stops shipping them — the module supplying the derived half of each key and its `value_type_id`, the reconciler extracting the category from the derived half's third segment and creating it if absent, and upgrades to a new setting major carrying every stored value across with re-validation rather than silent coercion.

- **Depends On**: `cpt-cf-settings-service-feature-typed-value-validation`

- **Scope**:
  - Reconciler operations `register_declarations` and `retire_declarations` behind the SDK Contribution client trait, implemented in process and registered into `ClientHub` at init (DESIGN §4.2 *Module Contribution Reconciler*, §4.5); the SDK's `ContributedDeclaration` completed with `value_type_id`, scope class, classification and the metadata a declaration carries, plus a `SettingKey` constructor for contributed keys
  - Key composition for a contributed setting: the module's derived half `<vendor>.<package>.<category>.<name>.vN` under the Settings base, `KeyNotNamespaced` when the category segment is missing or the half is malformed, the category auto-vivified by slug, and the composed type registered before the row is inserted
  - Reconcile by version-stripped path: a new path inserted with `source = module_contributed`; a same-major compatible change updating metadata in place and preserving administrator-set values; a changed `value_type_id` at the same major refused as `ValueTypeChanged`; a higher major running the upgrade migration
  - The upgrade migration: the predecessor and all its values retained; the successor inserted at the new key; every value copied and re-validated against the new value type, failures inserted flagged `needs_review` with detail; the default re-validated, a failing default blocking the new declaration; the predecessor retired so exactly one major is active; succession derived from the keys with no stored pointer
  - Reactivation of a retired matched declaration on reconcile, retained values re-validated before they go live; a module may not retype on revive
  - Contributed classification: `pii` declared by the gear, `secret` derived from the trait and never accepted, a class correction re-syncing the denormalized copy on the setting's value rows
  - Admin immutability of contributed declarations, `409 ContributedDeclarationImmutable` on `PATCH` and `DELETE`, already enforced by 2.3
  - Publication of `event_declaration_registered`, `event_declaration_updated`, `event_declaration_retired`, and `event_declaration_reactivated`; one audit record per **changed** row through the Audit Emitter of 2.1, moved onto the transactional sink when 2.6 lands. The record carries **no tenant**: a declaration is platform-wide and sits at no scope, so the reconcile borrows none and resolves nothing — the borrowed root tenant was a Tenant Resolver lookup from inside this transaction, and it is what stopped contributing gears from booting. A boot that converges records nothing, so the trail is installs and upgrades rather than one entry per restart
  - The contributing gear calls from its `post_init` hook, not its `init`: the types registry admits the schemas registered during the init phase — the value-type catalogue among them — into its readable store only when it switches to ready mode after every gear's `init`, so a value type is resolvable, and a derived setting type registrable with validation, only from that phase on. `settings-demo` under `plugins/` is the worked example and the example server's seed data

- **Out of scope**:
  - Disposition of retained values on gear removal — purge, archive, or keep — which is open (DESIGN §6)
  - Verified module identity: `owner_module` is caller-supplied and never an authorization input; the contribution trust model is R2
  - Dependency Groups over contributed settings, which are R3
  - The owner gear's own ordering and failure posture around the call, which the design assigns to the owner

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-settings-service-fr-module-contributed-declarations`
  - [ ] `p1` - `cpt-cf-settings-service-fr-contributed-lifecycle`

- **Domain Model Entities**:
  - ContributedDeclaration, ReconcileResult
  - Version-stripped path

- **Design Components**:

  - [ ] `p1` - `cpt-cf-settings-service-component-module-contribution-reconciler`

- **API**:
  - SDK Contribution client: `register_declarations`, `retire_declarations`, `list_contributed`; no REST endpoint, since the caller is a gear's init

- **Data**:
  - Writes `setting_declarations` with `source = module_contributed` and `owner_module`, copies `setting_values` rows on upgrade, and reads `categories`; adds no table of its own

---

## 3. Feature Dependencies

```text
cpt-cf-settings-service-feature-gear-foundation
    ↓
cpt-cf-settings-service-feature-category-management
    ↓
cpt-cf-settings-service-feature-setting-declarations
    ↓                                   ↓
cpt-cf-settings-service-feature-typed-value-validation
    ↓                                   ↓
cpt-cf-settings-service-feature-value-resolution      cpt-cf-settings-service-feature-audit-store
    ↓                                                     ↓            ↓
cpt-cf-settings-service-feature-tenant-access ────────────┘            ↓
    ↓                                                                  ↓
cpt-cf-settings-service-feature-value-writes                           ↓
    ↓                                                                  ↓
cpt-cf-settings-service-feature-secret-values                          ↓
                                                                       ↓
cpt-cf-settings-service-feature-typed-value-validation ─→ cpt-cf-settings-service-feature-module-contributions
```

**Dependency Rationale**:

- `cpt-cf-settings-service-feature-gear-foundation` has no dependency and is the only feature that could start against an empty crate tree.
- `cpt-cf-settings-service-feature-category-management` requires the foundation: it is the first domain entity and needs persistence, REST and OData infrastructure, the `PolicyEnforcer` PEP, error mapping, and the root tenant's id for its audit scope before it can expose an endpoint.
- `cpt-cf-settings-service-feature-setting-declarations` requires categories: a declaration carries a non-null `category_id`, and its key embeds the category slug as the third token of the derived half. The dependency runs the other way too in one respect worth planning around — the foreign key that makes 2.2's no-orphan rule enforceable at the database level is created here, so the no-orphan behaviour cannot be end-to-end tested until 2.3 lands.
- `cpt-cf-settings-service-feature-typed-value-validation` requires declarations: the value type is named by the declaration's `value_type_id`, so there is nothing to validate against until declarations exist. Declaration creation in turn calls the validator for its Schema Default, which is why the two are adjacent rather than independent.
- `cpt-cf-settings-service-feature-value-resolution` requires typed value validation: the resolution chain terminates in the Schema Default, and every value it walks must already be a validated typed value stored in `setting_values`.
- `cpt-cf-settings-service-feature-audit-store` requires declarations: the history read and the `declaration_key` column are keyed by a setting key, and the store must exist before any path that fails closed on it is built. It does not wait on resolution, so it can proceed alongside 2.4 and 2.5.
- `cpt-cf-settings-service-feature-tenant-access` requires resolution: effective access is consulted by the read path, and an access change evicts the target's and its descendants' cached values, so the cache and the visibility rule it feeds must exist first.
- `cpt-cf-settings-service-feature-value-writes` requires the audit store, because every write commits its record in the same transaction and refuses without it, and tenant access, because a write requires the caller's own effective access to be `overridable`. Through those it also requires resolution, which supplies the current effective value the validate report and the impact preview compare against.
- `cpt-cf-settings-service-feature-secret-values` requires value writes: a secret is set as a value at a scope, and `set` is what routes the plaintext through the Secret Manager and persists only the reference.
- `cpt-cf-settings-service-feature-module-contributions` requires typed value validation, for the defaults and the copied values an upgrade re-validates and for the `setting_values` table the copies land in. It writes **no** audit records at all — a declaration has no scope to write one against — so the audit store never enters its ordering; and it does not require value writes, since the upgrade migration copies rows directly rather than setting them.

**Parallelism**: wave 1 is strictly sequential. In wave 2, `audit-store` can start as soon as 2.3 lands and run alongside 2.4 and 2.5; `module-contributions` needs only 2.4 and can run alongside 2.5 through 2.9; `tenant-access`, `value-writes`, and `secret-values` form a chain. Work also parallelizes inside 2.1, where the SDK crate, persistence harness, REST and OData infrastructure, error mapping, and the Audit Emitter are largely independent of one another.

**Order of delivery within the waves.** Declarations reach the platform through two doors, and only one of them is on the critical path: gears contribute theirs through the SDK (2.10), administrators author theirs through `POST /settings-service/v1/declarations` (the write flows of 2.3). Set and read of values need declarations to exist, not an administrative way to author them, so the sequence that reaches a working value path soonest is: the `setting_type` base registered at init (2.1, step 9 of gear init), the Type Validator and the `setting_values` table (2.4), the reconciler (2.10), the read surface and the in-process reader (2.5), the audit store (2.6), the write path (2.8) — with tenant access (2.7) entering as its absent-row semantics first, which is `overridable` for everyone, and its endpoints after. The administrative declaration writes of 2.3 follow; their read surface is already in place.

**What unblocks next**: 2.10 completes R1. R2 then waits on the platform — verified machine caller identity for the out-of-process profiles and the service-write SDK path, the platform-wide elevated session behind the existing `StepUpVerifier` port, the platform Audit Subsystem behind the existing `AuditSink` port, the tenant-deleted signal, and per-feature licence entitlement — while search, standard/advanced mode, file-valued settings, the anonymous read route, and the replica broadcast need nothing outside this gear and the broker it already publishes to.
