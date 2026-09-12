<!-- Created: 2026-08-10 by Constructor Tech -->
<!-- Updated: 2026-08-10 by Constructor Tech -->

# Feature: Setting Declarations and Scope Class

- [ ] `p1` - **ID**: `cpt-cf-settings-service-featstatus-setting-declarations`

- [ ] `p1` - `cpt-cf-settings-service-feature-setting-declarations`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Create Declaration](#create-declaration)
  - [Update Declaration Metadata](#update-declaration-metadata)
  - [Retire Declaration](#retire-declaration)
  - [Reactivate Declaration](#reactivate-declaration)
  - [Declare Dependency Group](#declare-dependency-group)
  - [Read Declarations](#read-declarations)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Setting Key Construction](#setting-key-construction)
  - [Declaration Mutation Class Resolution](#declaration-mutation-class-resolution)
  - [Classification and Secret-Trait Derivation](#classification-and-secret-trait-derivation)
- [4. States (CDSL)](#4-states-cdsl)
  - [SettingDeclaration State Machine](#settingdeclaration-state-machine)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Declaration Entity and Schema](#declaration-entity-and-schema)
  - [Setting Key Construction and Uniqueness](#setting-key-construction-and-uniqueness)
  - [Schema Default Semantics](#schema-default-semantics)
  - [Scope Class Derivation](#scope-class-derivation)
  - [Derived Data Classification](#derived-data-classification)
  - [Mutation Class Discipline](#mutation-class-discipline)
  - [Retire and Reactivate Lifecycle](#retire-and-reactivate-lifecycle)
  - [Contributed Declaration Protection](#contributed-declaration-protection)
  - [Dependency Group Declaration](#dependency-group-declaration)
  - [Declaration Read Surface](#declaration-read-surface)
  - [Declaration Mutation Audit](#declaration-mutation-audit)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Introduces the setting declaration as an entity distinct from its value: an admin-authored record keyed by a GTS **type** identifier derived from the Settings gear's abstract `setting_type` base ([ADR-002](../ADR/ADR-002-setting-key-gts-type-id.md)), filed under a category, carrying a mandatory Schema Default and a first-class scope class from which cascade and override behaviour is derived rather than configured. Includes the retire and reactivate lifecycle, the mutation-class discipline that governs which fields may change and how, and Dependency Group declaration.

### 1.2 Purpose

The declaration-value split is the design's central principle, and this feature is where it becomes real. A declaration says what a setting *is* — its type, its default, its cascade behaviour, its visibility, its classification. A value says what it currently *holds* at some scope. Keeping them apart is what lets a Schema Default survive an override being set and later reverted, and what lets a setting be retired without destroying the values already stored against it.

Scope class replaces the older pattern of hand-set override and inheritance booleans. Because behaviour is derived from a single mandatory attribute, a setting cannot end up tenant-overridable because someone forgot to clear a flag: an infrastructure setting is `global` by declaration, and no tenant holds a value for it. What a tenant may *do* with a setting is a separate, sparse decision — a `tenant_permissions` row of `read_only` or `hidden` recorded by an ancestor's administrator, absence meaning `overridable` — never a pair of booleans on the declaration. The two flags the declaration does carry gate people, not scopes: `requires_step_up`, whose default is the protective `true`, and `anonymous_exposable`, which the schema refuses on a `secret` or `pii` classification.

The hardest constraint here is not any single field but the rule connecting them: **no declaration edit may silently change a live setting's effective resolution.** That is enforced by partitioning every field and action into mutation classes — descriptive metadata is immediate, resolution-affecting fields are immutable, and the two actions that change whether a setting resolves at all require credential step-up. Getting this partition wrong is how an ungated `PATCH` would quietly re-point production configuration.

**Requirements**: `cpt-cf-settings-service-fr-setting-scope-class`, `cpt-cf-settings-service-fr-dependency-group-declaration`, `cpt-cf-settings-service-nfr-versatility-gts-scope-class`

**Principles**: `cpt-cf-settings-service-principle-declaration-value-split`, `cpt-cf-settings-service-principle-scope-class-derivation`, `cpt-cf-settings-service-principle-fail-closed`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-settings-service-actor-platform-admin` | Authors, updates, retires, and reactivates admin declarations, and declares Dependency Groups over them |
| `cpt-cf-settings-service-actor-tenant-admin` | Reads declarations exposed by the visibility, domain, and licence gates |
| `cpt-cf-settings-service-actor-contributing-module` | Owns `module_contributed` declarations, which this feature reads and protects from admin edit but does not itself write |
| `cpt-cf-settings-service-actor-authz-resolver` | Supplies the authorization decision and the `AccessScope` constraints applied to reads |
| `cpt-cf-settings-service-actor-types-registry` | Owns the curated value types a declaration names by `value_type_id`, and holds the setting's own type, registered under the Settings base when the declaration is created; this service consumes the catalogue and never authors a value shape |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — §5.1 Settings and Category Model, §5.5 Validate and Set Values, §5.6 Multi-Tenant Overrides
- **Design**: [DESIGN.md](../DESIGN.md) — §4.1 (Entity `SettingDeclaration`, `ScopeClass`, `DeclarationSource` / `DeclarationStatus` / `DomainAffinity`), §4.2 (Component: Declaration Management, including the Declaration Mutation Classes table), §4.3 (REST API — Setting Declarations), §4.7 (Tables `setting_declarations` and `tenant_permissions`, GTS Type & Schema Identifiers)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.3
- **Dependencies**: entry 2.2 category management, since a declaration carries a non-null `category_id` and embeds the category slug in its key; and entry 2.1 gear foundation for persistence, Problem mapping, the `If-Match` precondition helper, the `PolicyEnforcer` PEP with credential step-up, and the Audit Emitter.
- **Forward seam**: DESIGN §4.2 lists `TypeValidator` among this component's dependencies, because creating a declaration validates its Schema Default against the value type and resolves that type's traits. The validator itself is delivered in entry 2.4. Build this feature against the validator's trait rather than its implementation, so the two can land in either order; the trait belongs with the SDK contracts from 2.1. This is the one place in the first wave where a feature reaches forward rather than back.
- **Not applicable**: Module-contributed declaration authoring is owned by the Contribution Reconciler in a later wave; this feature only reads such declarations and refuses admin edits to them. Value writes belong to the Value Writer of entry 2.8 and are out of scope. Frontend presentation is owned by a future frontend DESIGN.

## 2. Actor Flows (CDSL)

### Create Declaration

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-setting-declarations-create`

**Actor**: `cpt-cf-settings-service-actor-platform-admin`

**Success Scenarios**:
- Declaration created with a server-composed GTS type key registered under the Settings base, a validated Schema Default, and derived classification

**Error Scenarios**:
- Actor not authorized, or the decision cannot be obtained
- Target category does not exist
- A key segment violates the GTS grammar
- Schema Default fails validation against the value type
- A non-empty Schema Default supplied on a secret-trait value type
- `secret` classification supplied by the author on a non-secret value type
- Duplicate `key`, or a leaf slug already held by an active declaration in the category

**Steps**:
1. [x] - `p1` - Actor sends POST /settings-service/v1/declarations with `value_type_id`, `vendor`, leaf `name`, `category_id`, `default_value`, `scope_class`, and optional `description`, `mode`, `requires_step_up`, `anonymous_exposable`, `domain_affinity`, `licence_feature`, `data_classification` - `inst-decl-create-1`
2. [x] - `p1` - Authorize `create` on `gts.cf.core.settings.declaration.v1~` through the `PolicyEnforcer` PEP - `inst-decl-create-2`
3. [x] - `p1` - **IF** the decision is deny or cannot be obtained → **RETURN** `403` - `inst-decl-create-3`
4. [x] - `p1` - DB: SELECT the target category WHERE id = {category_id} - `inst-decl-create-4`
5. [x] - `p1` - **IF** the category does not exist → **RETURN** `404` - `inst-decl-create-5`
6. [x] - `p1` - Invoke setting instance key construction using `value_type_id`, `vendor`, the category's slug, and the leaf `name` - `inst-decl-create-6`
7. [x] - `p1` - **IF** any segment violates the GTS grammar → **RETURN** `400` naming the offending segment - `inst-decl-create-7`
8. [x] - `p1` - Resolve the value type's trait set through the Type Validator trait - `inst-decl-create-8`
9. [x] - `p1` - Invoke classification and secret-trait derivation using the resolved traits and any author-supplied `data_classification` - `inst-decl-create-9`
10. [x] - `p1` - **IF** derivation rejects the combination → **RETURN** `400` - `inst-decl-create-10`
11. [x] - `p1` - **IF** `has_secret_trait` is true **AND** `default_value` is non-empty → **RETURN** `400`, because a secret setting has no secret default - `inst-decl-create-11`
12. [x] - `p1` - **IF** `has_secret_trait` is false → validate `default_value` against `value_type_id` through the Type Validator trait - `inst-decl-create-12`
13. [x] - `p1` - **IF** Schema Default validation fails → **RETURN** `400` with the validator's field-level errors - `inst-decl-create-13`
14. [x] - `p1` - **IF** `anonymous_exposable` is requested **AND** the derived `data_classification` is `secret` or `pii` → **RETURN** `400`, because the two classifications that must never leave are excluded from the anonymous surface in the schema as well as here - `inst-decl-create-14`
15. [x] - `p1` - Set `source` to `admin_authored`, `status` to `active`, `mode` to its default when unsupplied, `requires_step_up` to `true` when unsupplied, and `created_by` from the authenticated principal - `inst-decl-create-15`
16. [x] - `p1` - Register the composed setting type in the types registry — derived from the Settings base, composed with the `value_type_id`, and carrying no `default` — before the row is inserted; registration is idempotent, so a retry after a failed create reuses the type rather than minting a second one - `inst-decl-create-16`
17. [x] - `p1` - DB: INSERT INTO setting_declarations with the constructed `key`, `leaf_slug`, `value_type_id`, `category_id`, and derived columns - `inst-decl-create-17`
18. [x] - `p1` - **IF** unique violation on `uq_declaration_key` → **RETURN** `409` - `inst-decl-create-18`
19. [x] - `p1` - **IF** unique violation on `uq_declaration_category_slug` → **RETURN** `409` stating the leaf name is held by an active declaration in this category - `inst-decl-create-19`
20. [x] - `p1` - Emit a declaration-created audit record - `inst-decl-create-20`
21. [x] - `p1` - **RETURN** `201` with the declaration, its `key`, its `value_type_id`, and its resolved traits - `inst-decl-create-21`

### Update Declaration Metadata

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-setting-declarations-update`

**Actor**: `cpt-cf-settings-service-actor-platform-admin`

**Success Scenarios**:
- Descriptive metadata updated immediately, since none of it changes an effective value

**Error Scenarios**:
- Declaration is `module_contributed` and therefore not admin-editable
- Request carries a behavior-affecting field
- Request loosens `data_classification` without a step-up assertion
- `If-Match` absent or stale

**Steps**:
1. [x] - `p1` - Actor sends PATCH /settings-service/v1/declarations/{id} with `If-Match` and any of `description`, `mode`, `domain_affinity`, `licence_feature`, `requires_step_up`, `anonymous_exposable`, `data_classification` - `inst-decl-update-1`
2. [x] - `p1` - Authorize `update` on `gts.cf.core.settings.declaration.v1~` - `inst-decl-update-2`
3. [x] - `p1` - **IF** the decision is deny or cannot be obtained → **RETURN** `403` - `inst-decl-update-3`
4. [x] - `p1` - DB: SELECT the declaration WHERE id = {id} - `inst-decl-update-4`
5. [x] - `p1` - **IF** not found → **RETURN** `404` - `inst-decl-update-5`
6. [x] - `p1` - **IF** `source` is `module_contributed` → **RETURN** `409 ContributedDeclarationImmutable`, because gear declarations change only through their owning module - `inst-decl-update-6`
7. [x] - `p1` - Evaluate the `If-Match` precondition; **IF** absent → **RETURN** `428`; **IF** stale → **RETURN** `412` - `inst-decl-update-7`
8. [x] - `p1` - Invoke declaration mutation class resolution over every field present in the request - `inst-decl-update-8`
9. [x] - `p1` - **IF** any field resolves to the immutable class → **RETURN** `400` naming the field and stating that the change is expressible only as a replacement declaration - `inst-decl-update-9`
10. [x] - `p1` - **IF** any field resolves to the step-up class → require a valid credential step-up assertion - `inst-decl-update-10`
11. [x] - `p1` - **IF** step-up is required and absent or invalid → **RETURN** `403` - `inst-decl-update-11`
12. [x] - `p1` - **IF** the request enables `anonymous_exposable` on a declaration whose `data_classification` is `secret` or `pii` → **RETURN** `400`, preserving the database check as the backstop - `inst-decl-update-12`
13. [x] - `p1` - DB: UPDATE setting_declarations SET {supplied metadata}, `last_change_at` = now(), `updated_at` = now() WHERE id = {id} - `inst-decl-update-13`
14. [x] - `p1` - Emit a declaration-updated audit record with pre-image and post-image - `inst-decl-update-14`
15. [x] - `p1` - **RETURN** `200` with the updated declaration and a refreshed ETag - `inst-decl-update-15`

### Retire Declaration

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-setting-declarations-retire`

**Actor**: `cpt-cf-settings-service-actor-platform-admin`

**Success Scenarios**:
- Declaration soft-deleted to `retired` in one transaction, its values retained but excluded from resolution

**Error Scenarios**:
- Declaration is `module_contributed`
- Credential step-up absent or invalid
- `If-Match` absent or stale

**Steps**:
1. [x] - `p1` - Actor sends DELETE /settings-service/v1/declarations/{id} with `If-Match` - `inst-decl-retire-1`
2. [x] - `p1` - Authorize `delete` on `gts.cf.core.settings.declaration.v1~` - `inst-decl-retire-2`
3. [x] - `p1` - **IF** the decision is deny or cannot be obtained → **RETURN** `403` - `inst-decl-retire-3`
4. [x] - `p1` - Require a valid credential step-up assertion, because retire drops a live setting out of resolution at once - `inst-decl-retire-4`
5. [x] - `p1` - **IF** step-up is absent or invalid → **RETURN** `403` - `inst-decl-retire-5`
6. [x] - `p1` - DB: SELECT the declaration WHERE id = {id} - `inst-decl-retire-6`
7. [x] - `p1` - **IF** not found → **RETURN** `404` - `inst-decl-retire-7`
8. [x] - `p1` - **IF** `source` is `module_contributed` → **RETURN** `409 ContributedDeclarationImmutable` - `inst-decl-retire-8`
9. [x] - `p1` - Evaluate the `If-Match` precondition; **IF** absent → **RETURN** `428`; **IF** stale → **RETURN** `412` - `inst-decl-retire-9`
10. [x] - `p1` - DB: UPDATE setting_declarations SET `status` = 'retired' WHERE id = {id}, in one transaction with the invalidation below - `inst-decl-retire-10`
11. [x] - `p1` - Retain every row in `setting_values` for this declaration; retire never deletes values - `inst-decl-retire-11`
12. [x] - `p1` - Invalidate the local cache for the affected scopes and publish the cache-invalidation and declaration-retired signals - `inst-decl-retire-12`
13. [x] - `p1` - Emit a declaration-retired audit record carrying pre-images - `inst-decl-retire-13`
14. [x] - `p1` - **RETURN** `200` with the retired declaration; this is a soft delete, not a `204` removal - `inst-decl-retire-14`

### Reactivate Declaration

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-setting-declarations-reactivate`

**Actor**: `cpt-cf-settings-service-actor-platform-admin`

**Success Scenarios**:
- A retired declaration is revived by re-declaring its key, and its retained values resume participating in resolution

**Error Scenarios**:
- Credential step-up absent or invalid
- The re-declared key does not match an existing retired row, in which case the request is an ordinary create

**Steps**:
1. [x] - `p1` - Actor sends POST /settings-service/v1/declarations at a key that matches an existing `retired` declaration - `inst-decl-react-1`
2. [x] - `p1` - Authorize `create` on `gts.cf.core.settings.declaration.v1~` - `inst-decl-react-2`
3. [x] - `p1` - **IF** the decision is deny or cannot be obtained → **RETURN** `403` - `inst-decl-react-3`
4. [x] - `p1` - Construct the key and look up an existing declaration at that key - `inst-decl-react-4`
5. [x] - `p1` - **IF** no row exists → continue as an ordinary create - `inst-decl-react-5`
6. [x] - `p1` - **IF** a row exists with `status` = 'active' → **RETURN** `409` for the duplicate key - `inst-decl-react-6`
7. [x] - `p1` - Require a valid credential step-up assertion, because reactivation changes whether a live setting resolves - `inst-decl-react-7`
8. [x] - `p1` - **IF** step-up is absent or invalid → **RETURN** `403` - `inst-decl-react-8`
9. [x] - `p1` - DB: UPDATE setting_declarations SET `status` = 'active' and the re-declared metadata WHERE key = {key} - `inst-decl-react-9`
10. [x] - `p1` - Invalidate the local cache for the affected scopes, since retained values re-enter resolution - `inst-decl-react-10`
11. [x] - `p1` - Emit a declaration-reactivated audit record - `inst-decl-react-11`
12. [x] - `p1` - **RETURN** `200` with the revived declaration - `inst-decl-react-12`

### Declare Dependency Group

- [ ] `p2` - **ID**: `cpt-cf-settings-service-flow-setting-declarations-dependency-group`

**Actor**: `cpt-cf-settings-service-actor-platform-admin`

**Success Scenarios**:
- A named set of interdependent settings is declared with a cross-setting constraint over their combined values

**Error Scenarios**:
- A member key does not resolve to an active declaration
- An attempt to edit an existing group or its constraint in place

**Steps**:
1. [ ] - `p2` - Actor sends a Dependency Group declaration naming the group and its member setting keys, with the cross-setting constraint over their combined values - `inst-decl-depgrp-1`
2. [ ] - `p2` - Authorize the declaration action through the `PolicyEnforcer` PEP - `inst-decl-depgrp-2`
3. [ ] - `p2` - **IF** the decision is deny or cannot be obtained → **RETURN** `403` - `inst-decl-depgrp-3`
4. [ ] - `p2` - Resolve every member key to an active declaration - `inst-decl-depgrp-4`
5. [ ] - `p2` - **IF** any member key does not resolve → **RETURN** `400` naming the unresolved key - `inst-decl-depgrp-5`
6. [ ] - `p2` - **IF** a group already exists under this name → **RETURN** `400`, because a group definition and its constraint are behavior-affecting and change only through a replacement declaration - `inst-decl-depgrp-6`
7. [ ] - `p2` - Restrict membership to declarations of a single `source`, so an admin group cannot capture a gear's contributed settings - `inst-decl-depgrp-7`
8. [ ] - `p2` - Persist the group and its constraint - `inst-decl-depgrp-8`
9. [ ] - `p2` - Emit a dependency-group-declared audit record - `inst-decl-depgrp-9`
10. [ ] - `p2` - **RETURN** the declared group; setting its members all-or-nothing belongs to the value write path and is out of scope here - `inst-decl-depgrp-10`

### Read Declarations

- [ ] `p1` - **ID**: `cpt-cf-settings-service-flow-setting-declarations-read`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- A single declaration, or a filtered page of declarations, returned with resolved traits for client rendering

**Error Scenarios**:
- Declaration outside the caller's visibility, domain, or licence gate
- Unsupported OData filter or ordering expression

**Steps**:
1. [x] - `p1` - Actor sends GET /settings-service/v1/declarations/{id} or GET /settings-service/v1/declarations with optional OData `$filter`, `$orderby`, `$select`, and a pagination cursor - `inst-decl-read-1`
2. [x] - `p1` - Authorize `read` on `gts.cf.core.settings.declaration.v1~` and obtain the `AccessScope` constraints - `inst-decl-read-2`
3. [x] - `p1` - **IF** the decision is deny or cannot be obtained → **RETURN** `403` - `inst-decl-read-3`
4. [ ] - `p1` - Derive the combined visibility and domain-affinity predicate from the `AccessScope` constraints; the licence predicate joins it in R2, when the License Resolver exists to answer it - `inst-decl-read-4`
5. [x] - `p1` - **IF** an OData expression references an unmapped field or an unsupported operator → **RETURN** `400` - `inst-decl-read-5`
6. [x] - `p1` - DB: SELECT declarations with the combined predicate applied inside the query - `inst-decl-read-6`
7. [x] - `p1` - **IF** a single-declaration read is filtered out → **RETURN** `404` rather than `403`, so a gated declaration's existence is not disclosed - `inst-decl-read-7`
8. [x] - `p1` - Resolve each returned declaration's trait set for client rendering - `inst-decl-read-8`
9. [x] - `p1` - **RETURN** `200` with the declaration or page, each carrying `key`, `value_type_id`, and resolved traits - `inst-decl-read-9`

## 3. Processes / Business Logic (CDSL)

### Setting Key Construction

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-setting-declarations-key-construction`

**Input**: `vendor`, the owning category's slug, and the leaf `name`; the `value_type_id` is a separate fact of the declaration and takes no part in the key

**Output**: The full setting key and its leaf slug, or a validation problem naming the offending segment

**Steps**:
1. [x] - `p1` - Validate `vendor`, the category slug, and the leaf `name` against the GTS grammar: lowercase, the permitted character set only, and no `/` - `inst-decl-key-1`
2. [x] - `p1` - **IF** any segment violates the grammar → **RETURN** validation problem naming that segment - `inst-decl-key-2`
3. [x] - `p1` - Build the derived half by composing the vendor, the fixed `settings` package, the category slug, and the leaf name with the version suffix and the trailing type terminator, so the result is itself a GTS type - `inst-decl-key-3`
4. [x] - `p1` - Compose the full key as the Settings gear's abstract base `gts.cf.core.settings.setting_type.v1~` followed by that derived type, and run the whole through the same parser a supplied key goes through, so a composed key and a parsed one can never disagree - `inst-decl-key-4`
5. [x] - `p1` - Set the leaf slug to the leaf `name`, which is what uniqueness within the category is enforced on - `inst-decl-key-5`
6. [x] - `p1` - **RETURN** the key and leaf slug, recording that the embedded category slug makes the key a function of its category, so moving or renaming the category re-keys the setting with no alias retained - `inst-decl-key-6`

### Declaration Mutation Class Resolution

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-setting-declarations-mutation-class`

**Input**: The set of fields present in an update request, and the declaration's current `data_classification`

**Output**: A per-field class of immediate, immutable, or step-up-gated

**Steps**:
1. [x] - `p1` - **FOR EACH** field present in the request - `inst-decl-mutcls-1`
   1. [x] - `p1` - **IF** the field is `description`, `mode`, `domain_affinity`, or `licence_feature`, or it tightens a gate — sets `requires_step_up` true or `anonymous_exposable` false → classify as immediate - `inst-decl-mutcls-2`
   2. [x] - `p1` - **IF** the field is `default_value`, the value type, or `scope_class` → classify as immutable, because each would change a live setting's resolution without any gate - `inst-decl-mutcls-3`
   3. [x] - `p1` - **IF** the field is `data_classification` **AND** the change tightens from `public` toward `pii` → classify as immediate - `inst-decl-mutcls-4`
   4. [x] - `p1` - **IF** the field is `data_classification` **AND** the change loosens from `pii` toward `public` → classify as step-up-gated, because it un-masks content previously withheld - `inst-decl-mutcls-5`
   5. [x] - `p1` - **IF** the field is `data_classification` **AND** the request sets `secret` → classify as immutable, because `secret` is derived from the value type's trait and is never author-supplied - `inst-decl-mutcls-6`
   6. [x] - `p1` - **IF** the field loosens a gate — clears `requires_step_up` or enables `anonymous_exposable` → classify as step-up-gated whatever the flag currently says, because a caller holding a live session could otherwise clear the gate and then write with no re-verification anywhere in the sequence - `inst-decl-mutcls-8`
2. [x] - `p1` - **RETURN** the per-field classes, treating any unrecognized field as immutable so an unknown field can never take the immediate path - `inst-decl-mutcls-7`

### Classification and Secret-Trait Derivation

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-setting-declarations-classification`

**Input**: The value type's resolved trait set, and any author-supplied `data_classification`

**Output**: The derived `has_secret_trait` and `data_classification`, or a validation problem

**Steps**:
1. [x] - `p1` - Set `has_secret_trait` from the presence of the secret trait in the resolved trait set, never from author input - `inst-decl-class-1`
2. [x] - `p1` - **IF** `has_secret_trait` is true - `inst-decl-class-2`
   1. [x] - `p1` - Set `data_classification` to `secret` - `inst-decl-class-3`
   2. [x] - `p1` - **IF** the author supplied a `data_classification` other than `secret` → **RETURN** validation problem, since the class is derived and cannot be overridden - `inst-decl-class-4`
3. [x] - `p1` - **IF** `has_secret_trait` is false - `inst-decl-class-5`
   1. [x] - `p1` - **IF** the author supplied `secret` → **RETURN** validation problem, because a non-secret value type cannot carry a secret classification - `inst-decl-class-6`
   2. [x] - `p1` - Set `data_classification` to the author's `pii` or `public`, defaulting to `public` when unsupplied - `inst-decl-class-7`
4. [x] - `p1` - **RETURN** the derived pair, which the database re-checks through its equality constraint between `data_classification` being `secret` and `has_secret_trait` - `inst-decl-class-8`

## 4. States (CDSL)

### SettingDeclaration State Machine

- [x] `p1` - **ID**: `cpt-cf-settings-service-state-setting-declarations-lifecycle`

**States**: `active`, `retired`

**Initial State**: `active`

**Transitions**:
1. [x] - `p1` - **FROM** none **TO** `active` **WHEN** a declaration is created at a key that holds no existing row - `inst-decl-state-1`
2. [x] - `p1` - **FROM** `active` **TO** `retired` **WHEN** an authorized administrator retires it with a valid credential step-up assertion - `inst-decl-state-2`
3. [x] - `p1` - **FROM** `retired` **TO** `active` **WHEN** the key is re-declared with a valid credential step-up assertion - `inst-decl-state-3`
4. [x] - `p1` - **FROM** `retired` **TO** `retired` **WHEN** values remain stored against the declaration, since retire never deletes values and a retired declaration continues to occupy its category - `inst-decl-state-4`

## 5. Definitions of Done

### Declaration Entity and Schema

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-setting-declarations-entity-schema`

The system **MUST** persist declarations in a `setting_declarations` table carrying `key` unique via `uq_declaration_key`, `leaf_slug` unique per category **among active declarations** via the partial `uq_declaration_category_slug`, a non-null `category_id` foreign key to `categories` declared `ON DELETE RESTRICT`, a **non-null** `default_value`, `requires_step_up` defaulting to `true`, `anonymous_exposable` defaulting to `false`, and check constraints enforcing the `scope_class`, `mode`, `status`, `source`, and `data_classification` vocabularies. Two cross-field checks **MUST** hold in the database, not only in application code: an `anonymous_exposable` declaration may not be classified `secret` or `pii`, and `data_classification` being `secret` must be equivalent to `has_secret_trait`.

**Implements**:
- `cpt-cf-settings-service-flow-setting-declarations-create`

**Constraints**: `cpt-cf-settings-service-constraint-key-is-gts-type-id`

**Touches**:
- DB Table: `setting_declarations`
- Entities: `SettingDeclaration`, `ScopeClass`, `DeclarationSource`, `DeclarationStatus`, `DomainAffinity`

### Setting Key Construction and Uniqueness

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-setting-declarations-key`

The system **MUST** construct the setting key as the Settings gear's abstract base `gts.cf.core.settings.setting_type.v1~` followed by one derived type `<vendor>.settings.<category>.<name>.v1~`, validating every segment against the GTS grammar, and **MUST** enforce global key uniqueness alongside leaf-slug uniqueness among a category's active declarations. The setting is itself a GTS **type**: the composed type **MUST** be registered in the types registry — derived from the base, composed with the value type, carrying no `default` — before its row is inserted, and retiring the declaration **MUST NOT** unregister it ([ADR-002](../ADR/ADR-002-setting-key-gts-type-id.md)).

**Implements**:
- `cpt-cf-settings-service-algo-setting-declarations-key-construction`

**Constraints**: `cpt-cf-settings-service-constraint-key-is-gts-type-id`

**Touches**:
- API: `POST /settings-service/v1/declarations`
- Entities: `SettingDeclaration`

### Schema Default Semantics

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-setting-declarations-schema-default`

The Schema Default **MUST** live solely in the `default_value` column, be non-null so every resolution chain terminates, support structured object and array values, and be validated against the declaration's value type at create time. It **MUST** remain independent of any override and **MUST NOT** be editable through the declaration update path; changing the effective baseline is done with a platform-scope override instead. A secret-trait declaration **MUST** reject a non-empty Schema Default.

**Implements**:
- `cpt-cf-settings-service-flow-setting-declarations-create`

**Touches**:
- API: `POST /settings-service/v1/declarations`
- DB Table: `setting_declarations`
- Entities: `SettingDeclaration`

### Scope Class Derivation

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-setting-declarations-scope-class`

Every declaration **MUST** carry a mandatory `scope_class` of `global`, `cascading`, or `local`, and override and inheritance behaviour **MUST** be derived from it rather than from independently settable flags. A `global` declaration **MUST** hold no tenant-scoped value at all — a tenant caller resolves its platform value read-only, subject only to visibility — and what a tenant may do with any setting **MUST** come from `tenant_permissions` rather than from flags on the declaration.

**Implements**:
- `cpt-cf-settings-service-flow-setting-declarations-create`

**Touches**:
- DB Table: `setting_declarations`
- Entities: `ScopeClass`

### Derived Data Classification

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-setting-declarations-classification`

The system **MUST** derive `has_secret_trait` from the value type's resolved traits and **MUST** derive the `secret` classification from that trait alone, never from author input. An author-supplied `secret` on a non-secret value type **MUST** be rejected, and a non-`secret` classification supplied on a secret-trait value type **MUST** be rejected.

**Implements**:
- `cpt-cf-settings-service-algo-setting-declarations-classification`

**Touches**:
- API: `POST /settings-service/v1/declarations`
- Entities: `SettingDeclaration`

### Mutation Class Discipline

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-setting-declarations-mutation-classes`

The system **MUST** partition declaration changes into descriptive metadata applied immediately under `update` permission plus `If-Match`, behavior-affecting fields rejected as immutable, and behavior-affecting actions gated by credential step-up. `data_classification` tightening **MUST** be immediate while loosening **MUST** require step-up. No declaration edit may change a live setting's effective resolution without a gate, and an unrecognized field **MUST** be treated as immutable rather than immediate.

**Implements**:
- `cpt-cf-settings-service-algo-setting-declarations-mutation-class`
- `cpt-cf-settings-service-flow-setting-declarations-update`

**Constraints**: `cpt-cf-settings-service-constraint-optimistic-concurrency`, `cpt-cf-settings-service-constraint-step-up-at-idp`

**Touches**:
- API: `PATCH /settings-service/v1/declarations/{id}`
- Entities: `SettingDeclaration`

### Retire and Reactivate Lifecycle

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-setting-declarations-lifecycle`

Retire **MUST** be an immediate soft delete setting `status` to `retired` in one transaction with cache invalidation and signal publication, **MUST** require credential step-up, and **MUST** retain every stored value while excluding the declaration from resolution. Reactivation **MUST** be expressed as re-declaring the key, also step-up gated. Neither action goes through the value write path, and neither deletes values.

**Implements**:
- `cpt-cf-settings-service-flow-setting-declarations-retire`
- `cpt-cf-settings-service-flow-setting-declarations-reactivate`
- `cpt-cf-settings-service-state-setting-declarations-lifecycle`

**Constraints**: `cpt-cf-settings-service-constraint-step-up-at-idp`

**Touches**:
- API: `DELETE /settings-service/v1/declarations/{id}`
- API: `POST /settings-service/v1/declarations`
- DB Table: `setting_declarations`
- Entities: `DeclarationStatus`

### Contributed Declaration Protection

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-setting-declarations-contributed-protection`

A declaration whose `source` is `module_contributed` **MUST** be rejected for administrative update and administrative retire with a contributed-immutable conflict, and the database **MUST** enforce that `owner_module` is present exactly when `source` is `module_contributed`. This feature reads contributed declarations but never writes them.

**Implements**:
- `cpt-cf-settings-service-flow-setting-declarations-update`
- `cpt-cf-settings-service-flow-setting-declarations-retire`

**Touches**:
- API: `PATCH /settings-service/v1/declarations/{id}`
- API: `DELETE /settings-service/v1/declarations/{id}`
- Entities: `DeclarationSource`

### Dependency Group Declaration

- [ ] `p2` - **ID**: `cpt-cf-settings-service-dod-setting-declarations-dependency-group`

The system **MUST** let an authorized author declare a named Dependency Group over a set of interdependent settings with a cross-setting constraint over their combined values, **MUST** treat the group definition and its constraint as behavior-affecting and therefore immutable in place, and **MUST** resolve every member key to an active declaration at declaration time. Setting a group's members all-or-nothing belongs to the value write path and is not delivered here.

**Implements**:
- `cpt-cf-settings-service-flow-setting-declarations-dependency-group`

**Touches**:
- Entities: Dependency Group, cross-setting constraint

### Declaration Read Surface

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-setting-declarations-read-surface`

Declaration reads **MUST** be visibility- and domain-gated with the predicate applied inside the query, **MUST** leave the seam through which the licence gate joins that predicate in R2, **MUST** return the setting `key`, its `value_type_id`, and the resolved trait set for client rendering, and **MUST** report a gated single-declaration read as absent rather than forbidden.

**Implements**:
- `cpt-cf-settings-service-flow-setting-declarations-read`

**Constraints**: `cpt-cf-settings-service-constraint-rbac-policy-enforcer`

**Touches**:
- API: `GET /settings-service/v1/declarations`
- API: `GET /settings-service/v1/declarations/{id}`
- Entities: `SettingDeclaration`

### Declaration Mutation Audit

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-setting-declarations-audit`

The system **MUST** emit an audit record through the Audit Emitter for every declaration create, metadata update, retire, reactivate, and Dependency Group declaration, with the update and retire records carrying pre-images so a category rename or a retirement stays reconstructable from the trail.

**Implements**:
- `cpt-cf-settings-service-flow-setting-declarations-create`
- `cpt-cf-settings-service-flow-setting-declarations-update`
- `cpt-cf-settings-service-flow-setting-declarations-retire`

**Constraints**: `cpt-cf-settings-service-constraint-audit-and-events`

**Touches**:
- API: `POST /settings-service/v1/declarations`
- API: `PATCH /settings-service/v1/declarations/{id}`
- API: `DELETE /settings-service/v1/declarations/{id}`
- Entities: `SettingDeclaration`

## 6. Acceptance Criteria

- [x] Creating a declaration against an existing category returns `201` with a key composed of the Settings base type and a derived type `<vendor>.settings.<category>.<name>.v1~`, the derived half carrying the trailing type terminator, and the composed type registered in the types registry
- [x] Creating a declaration against a missing category returns `404`
- [x] A `vendor`, category slug, or leaf name containing uppercase, `/`, or a character outside the permitted set returns `400` naming the offending segment
- [x] Two active declarations with the same leaf name in the same category conflict on `uq_declaration_category_slug`, while a retired predecessor does not hold its name against a successor
- [x] Two declarations with the same leaf name in different categories both succeed, and their keys differ by the category segment
- [x] A declaration created without `requires_step_up` is stored with it `true`, and a direct database insert with `anonymous_exposable` true on a `pii` or `secret` classification is rejected by the check constraint
- [x] A Schema Default that fails validation against the value type returns `400` with field-level errors and inserts no row
- [x] A structured object or array Schema Default is accepted
- [x] A declaration on a secret-trait value type with a non-empty Schema Default returns `400`
- [x] A declaration on a secret-trait value type is stored with `data_classification` of `secret` even though the author supplied nothing
- [x] An author-supplied `secret` classification on a non-secret value type returns `400`
- [x] A direct database insert with `data_classification` of `secret` and `has_secret_trait` false is rejected by the equivalence check
- [x] A `PATCH` carrying `default_value`, the value type, or `scope_class` returns `400` and modifies no row
- [x] A `PATCH` carrying an unrecognized field is rejected rather than silently applied
- [x] A `PATCH` tightening `data_classification` from `public` to `pii` succeeds without step-up
- [x] A `PATCH` loosening `data_classification` from `pii` to `public` without step-up returns `403`, and succeeds with a valid step-up assertion
- [x] A `PATCH` clearing `requires_step_up` or enabling `anonymous_exposable` without step-up returns `403` and leaves the flag unchanged; the opposite edits apply immediately
- [x] A `PATCH` on a `module_contributed` declaration returns a contributed-immutable conflict
- [x] A `PATCH` without `If-Match` returns `428`, and with a stale `If-Match` returns `412`
- [x] Retiring a declaration without step-up returns `403`
- [x] Retiring a declaration sets `status` to `retired`, leaves every row in `setting_values` intact, and does not go through the value write path
- [x] Retiring a declaration invalidates the cache for the affected scopes in the same transaction that flips the status
- [x] A retired declaration still blocks deletion of its category
- [x] Re-declaring a retired key with step-up revives the row to `active` and its retained values participate in resolution again
- [x] Re-declaring a key that is already `active` returns `409`
- [ ] A Dependency Group naming a key that resolves to no active declaration returns `400`
- [ ] An attempt to edit an existing Dependency Group or its constraint in place is rejected
- [ ] A declaration read outside the caller's visibility, domain, or licence gate returns `404` rather than `403`
- [ ] Every declaration read returns the `key`, the `value_type_id`, and the resolved trait set
- [x] Every create, update, retire, and reactivate produces exactly one audit record, and update and retire records carry pre-images
