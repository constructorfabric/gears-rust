<!-- Created: 2026-08-10 by Virtuozzo International GmbH -->
<!-- Updated: 2026-10-28 by Virtuozzo International GmbH -->

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
  - [Report a Revival](#report-a-revival)
  - [Purge Declaration](#purge-declaration)
  - [Evolve Declaration](#evolve-declaration)
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
  - [Revival Report](#revival-report)
  - [Purge of a Retired Declaration](#purge-of-a-retired-declaration)
  - [Contributed Declaration Protection](#contributed-declaration-protection)
  - [Dependency Group Declaration](#dependency-group-declaration)
  - [Declaration Read Surface](#declaration-read-surface)
  - [Declaration Mutation Audit](#declaration-mutation-audit)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Introduces the setting declaration as an entity distinct from its value: an admin-authored record keyed by a GTS **type** identifier derived from the Settings gear's abstract `setting_type` base ([ADR-002](../ADR/ADR-002-setting-key-gts-type-id.md)), filed under a category, carrying a mandatory Schema Default and a first-class scope class from which cascade and override behaviour is derived rather than configured. Includes the retire, reactivate and purge lifecycle, the mutation-class discipline that governs which fields may change and how, and Dependency Group declaration.

### 1.2 Purpose

The declaration-value split is the design's central principle, and this feature is where it becomes real. A declaration says what a setting *is* — its type, its default, its cascade behaviour, its visibility, its classification. A value says what it currently *holds* at some scope. Keeping them apart is what lets a Schema Default survive an override being set and later reverted, and what lets a setting be retired without destroying the values already stored against it.

Scope class replaces the older pattern of hand-set override and inheritance booleans. Because behaviour is derived from a single mandatory attribute, a setting cannot end up tenant-overridable because someone forgot to clear a flag: an infrastructure setting is `global` by declaration, and no tenant holds a value for it. What a tenant may *do* with a setting is a separate, sparse decision — a `tenant_permissions` row of `read_only` or `hidden` recorded by an ancestor's administrator, absence meaning `overridable` — never a pair of booleans on the declaration. The two flags the declaration does carry gate people, not scopes: `requires_step_up`, whose default is the protective `true`, and `anonymous_exposable`, which the schema refuses on a `secret` or `pii` classification.

The hardest constraint here is not any single field but the rule connecting them: **no declaration edit may silently change a live setting's effective resolution.** That is enforced by partitioning every field and action into mutation classes — descriptive metadata is immediate, resolution-affecting fields are immutable, and the two actions that change whether a setting resolves at all require credential step-up. Purge is a further step-up-gated action: it does not change resolution — a retired declaration is already out of it — but it is irreversible and frees the key and its category. Getting this partition wrong is how an ungated `PATCH` would quietly re-point production configuration.

**Requirements**: `cpt-cf-settings-service-fr-setting-scope-class`, `cpt-cf-settings-service-fr-dependency-group-declaration`, `cpt-cf-settings-service-nfr-versatility-gts-scope-class`, `cpt-cf-settings-service-fr-declaration-purge`

**Principles**: `cpt-cf-settings-service-principle-declaration-value-split`, `cpt-cf-settings-service-principle-scope-class-derivation`, `cpt-cf-settings-service-principle-fail-closed`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-settings-service-actor-platform-admin` | Authors, updates, retires, reactivates and purges admin declarations, and declares Dependency Groups over them |
| `cpt-cf-settings-service-actor-tenant-admin` | Reads declarations exposed by the visibility, domain, and licence gates |
| `cpt-cf-settings-service-actor-contributing-module` | Owns `module_contributed` declarations, which this feature reads and protects from admin edit but does not itself write |
| `cpt-cf-settings-service-actor-authz-resolver` | Supplies the authorization decision and the `AccessScope` constraints applied to reads |
| `cpt-cf-settings-service-actor-types-registry` | Owns the value types a declaration names by `value_type_id` — this gear's catalogue and any a module registered in its own namespace — and holds the setting's own type, registered under the Settings base when the declaration is created; this service consumes the catalogue and never authors a value shape |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — §5.1 Settings and Category Model and Purge of a Retired Setting (`cpt-cf-settings-service-fr-declaration-purge`), §5.5 Validate and Set Values, §5.6 Multi-Tenant Overrides
- **Design**: [DESIGN.md](../DESIGN.md) — §4.1 (Entity `SettingDeclaration`, `ScopeClass`, `DeclarationSource` / `DeclarationStatus` / `DomainAffinity`), §4.2 (Component: Declaration Management, including the Declaration Mutation Classes table and `purge_declaration`), §4.3 (REST API — Setting Declarations, the purge route and its Rules), §4.6 (`cpt-cf-settings-service-seq-declaration-purge`), §4.7 (Tables `setting_declarations` and `tenant_permissions`, the cascades, GTS Type & Schema Identifiers), §4.8 (the `delete` action)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.3
- **Dependencies**: entry 2.2 category management, since a declaration carries a non-null `category_id` and embeds the category slug in its key; and entry 2.1 gear foundation for persistence, Problem mapping, the `If-Match` precondition helper, the `PolicyEnforcer` PEP with credential step-up, and the Audit Emitter.
- **Forward seam**: DESIGN §4.2 lists `TypeValidator` among this component's dependencies, because creating a declaration validates its Schema Default against the value type and resolves that type's traits. The validator itself is delivered in entry 2.4. Build this feature against the validator's trait rather than its implementation, so the two can land in either order; the trait belongs with the SDK contracts from 2.1. This is the one place in the first wave where a feature reaches forward rather than back. The purge reaches forward further than that, and is the one part of this entry that waits for later ones: it collects from `setting_values` (entry 2.4), its cascade removes rows of `tenant_permissions` (2.7) and `pending_secrets` (2.9), and it releases Credential Store entries through the Secret Manager port (`SecretManager::delete_secret`), which 2.9 delivers. It is built after 2.9 has landed, and its acceptance criteria run from then on.
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
- `display_name` empty or whitespace-only after trimming, longer than 256 Unicode characters, or containing a control character
- Schema Default fails validation against the value type
- A non-empty Schema Default supplied on a secret-trait value type
- `secret` classification supplied by the author on a non-secret value type
- Duplicate `key`, or a leaf slug already held by an active declaration in the category

**Steps**:
1. [x] - `p1` - Actor sends POST /settings-service/v1/declarations with `value_type_id`, `vendor`, leaf `name`, `category_id`, `default_value`, `scope_class`, and optional `display_name`, `description`, `mode`, `requires_step_up`, `anonymous_exposable`, `domain_affinity`, `licence_feature`, `data_classification` - `inst-decl-create-1`
2. [x] - `p1` - Authorize `create` on `gts.cf.core.settings.declaration.v1~` through the `PolicyEnforcer` PEP - `inst-decl-create-2`
3. [x] - `p1` - **IF** the decision is deny or cannot be obtained → **RETURN** `403` - `inst-decl-create-3`
4. [x] - `p1` - DB: SELECT the target category WHERE id = {category_id} - `inst-decl-create-4`
5. [x] - `p1` - **IF** the category does not exist → **RETURN** `404` - `inst-decl-create-5`
6. [x] - `p1` - Invoke setting instance key construction using `value_type_id`, `vendor`, the category's slug, and the leaf `name` - `inst-decl-create-6`
7. [x] - `p1` - **IF** any segment violates the GTS grammar → **RETURN** `400` naming the offending segment; **IF** `display_name` is present and not `null` **AND**, after trimming, it is empty, longer than 256 Unicode characters or contains a control character → **RETURN** `400` naming the field; an explicit `null` counts as unsupplied, and the trimmed value is what the rest of the flow stores - `inst-decl-create-7`
8. [x] - `p1` - Resolve the value type's trait set through the Type Validator trait - `inst-decl-create-8`
9. [x] - `p1` - Invoke classification and secret-trait derivation using the resolved traits and any author-supplied `data_classification` - `inst-decl-create-9`
10. [x] - `p1` - **IF** derivation rejects the combination → **RETURN** `400` - `inst-decl-create-10`
11. [x] - `p1` - **IF** `has_secret_trait` is true **AND** `default_value` is non-empty → **RETURN** `400`, because a secret setting has no secret default; an empty value still has to be a value of the type, which the next step decides - `inst-decl-create-11`
12. [x] - `p1` - Judge `default_value` on the request's raw text first, as a value write is judged — the size cap and numeric canonicality, so a decimal a double cannot hold exactly is refused `400 value_not_canonical` rather than rounded, which only the text can tell — then validate it against `value_type_id` through the Type Validator trait — a secret's placeholder included, so `""` passes for a string-shaped secret type and `null`, `[]` or `{}` do not, exactly as on the contribution path - `inst-decl-create-12`
13. [x] - `p1` - **IF** Schema Default validation fails → **RETURN** `400` with the validator's field-level errors - `inst-decl-create-13`
14. [x] - `p1` - **IF** `anonymous_exposable` is requested **AND** the derived `data_classification` is `secret` or `pii` → **RETURN** `400`, because the two classifications that must never leave are excluded from the anonymous surface in the schema as well as here - `inst-decl-create-14`
15. [x] - `p1` - Set `source` to `admin_authored`, `status` to `active`, `mode` to its default when unsupplied, `requires_step_up` to `true` when unsupplied, and `created_by` from the authenticated principal - `inst-decl-create-15`
16. [x] - `p1` - Register the composed setting type in the types registry — derived from the Settings base, composed with the `value_type_id`, and carrying no `default` — before the row is inserted; registration is idempotent, so a retry after a failed create reuses the type rather than minting a second one - `inst-decl-create-16`
17. [x] - `p1` - DB: INSERT INTO setting_declarations with the constructed `key`, `leaf_slug`, `value_type_id`, `category_id`, `display_name` (trimmed, `NULL` when unsupplied), and derived columns - `inst-decl-create-17`
18. [x] - `p1` - **IF** unique violation on `uq_declaration_key` → **RETURN** `409` - `inst-decl-create-18`
19. [x] - `p1` - **IF** unique violation on `uq_declaration_category_slug` → **RETURN** `409` stating the leaf name is held by an active declaration in this category - `inst-decl-create-19`
20. [x] - `p1` - Emit a declaration-created audit record - `inst-decl-create-20`
21. [x] - `p1` - **RETURN** `201` with the declaration, its `key`, its `value_type_id`, its `display_name` when set (omitted otherwise, never derived from `leaf_slug`), and its resolved traits - `inst-decl-create-21`

### Update Declaration Metadata

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-setting-declarations-update`

**Actor**: `cpt-cf-settings-service-actor-platform-admin`

**Success Scenarios**:
- Descriptive metadata updated immediately, since none of it changes an effective value

**Error Scenarios**:
- Declaration is `module_contributed` and therefore not admin-editable
- Request carries a behavior-affecting field
- Request loosens `data_classification` without a step-up assertion
- `display_name` empty or whitespace-only after trimming, longer than 256 Unicode characters, or containing a control character
- `If-Match` absent or stale

**Steps**:
1. [x] - `p1` - Actor sends PATCH /settings-service/v1/declarations/{id} with `If-Match` and any of `display_name`, `description`, `mode`, `domain_affinity`, `licence_feature`, `requires_step_up`, `anonymous_exposable`, `data_classification` - `inst-decl-update-1`
2. [x] - `p1` - Authorize `update` on `gts.cf.core.settings.declaration.v1~` - `inst-decl-update-2`
3. [x] - `p1` - **IF** the decision is deny or cannot be obtained → **RETURN** `403` - `inst-decl-update-3`
4. [x] - `p1` - DB: SELECT the declaration WHERE id = {id} - `inst-decl-update-4`
5. [x] - `p1` - **IF** not found → **RETURN** `404` - `inst-decl-update-5`
6. [x] - `p1` - **IF** `source` is `module_contributed` → **RETURN** `409 ContributedDeclarationImmutable`, because gear declarations change only through their owning module - `inst-decl-update-6`
7. [x] - `p1` - Evaluate the `If-Match` precondition; **IF** absent → **RETURN** `428`; **IF** stale → **RETURN** `412` - `inst-decl-update-7`
8. [x] - `p1` - Invoke declaration mutation class resolution over every field present in the request; a `400` the resolution returns for a field's shape or length is the response - `inst-decl-update-8`
9. [x] - `p1` - **IF** any field resolves to the immutable class → **RETURN** `400` naming the field and stating that the change is expressible only as a replacement declaration - `inst-decl-update-9`
10. [x] - `p1` - **IF** any field resolves to the step-up class → require a valid credential step-up assertion - `inst-decl-update-10`
11. [x] - `p1` - **IF** step-up is required and absent or invalid → **RETURN** `403` - `inst-decl-update-11`
12. [x] - `p1` - **IF** the request enables `anonymous_exposable` on a declaration whose `data_classification` is `secret` or `pii` → **RETURN** `400`, preserving the database check as the backstop - `inst-decl-update-12`
13. [x] - `p1` - DB: UPDATE setting_declarations SET {supplied metadata} (an explicit `null` clears a nullable field — `display_name`, `description`, `domain_affinity`, `licence_feature` — and an absent field is unchanged, and a supplied `display_name` is stored trimmed), `updated_at` = now(), and `last_change_at` = now() only when a field that changes what a reader is served changed — classification, step-up, anonymous exposure, licence feature, domain affinity; a description, display name or mode edit leaves the definition recency alone — WHERE id = {id} AND `updated_at` = {the version the tag was compared against}; **IF** no row matched → **RETURN** `412`; invalidate the local cache for the key — inside the transaction and again once it commits, as retire does — since the cached effective value carries the declaration's classification and domain affinity - `inst-decl-update-13`
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
10. [x] - `p1` - DB: UPDATE setting_declarations SET `status` = 'retired' WHERE id = {id} AND `updated_at` = {the version the tag was compared against}, in one transaction with the invalidation below; **IF** no row matched → **RETURN** `412` - `inst-decl-retire-10`
11. [x] - `p1` - Retain every row in `setting_values` for this declaration; retire never deletes values - `inst-decl-retire-11`
12. [x] - `p1` - Invalidate the local cache for the affected scopes and publish the cache-invalidation and declaration-retired signals - `inst-decl-retire-12`
13. [x] - `p1` - Emit a declaration-retired audit record carrying pre-images - `inst-decl-retire-13`
14. [x] - `p1` - **RETURN** `200` with the retired declaration; this is a soft delete, not a `204` removal - `inst-decl-retire-14`

### Reactivate Declaration

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-setting-declarations-reactivate`

**Actor**: `cpt-cf-settings-service-actor-platform-admin`

**Success Scenarios**:
- A retired declaration is revived by re-declaring its key with the value type it was declared with, and its retained values, re-validated against that type, resume participating in resolution

**Error Scenarios**:
- The retired declaration is `module_contributed`: refused with `409`, because a gear's declaration changes only through its owning module
- Credential step-up absent or invalid
- The re-declaration names a value type on the other side of the secret boundary, or a different scope class: refused with `409`, because either would move stored values rather than re-interpret them
- The re-declared key does not match an existing retired row, in which case the request is an ordinary create

**Steps**:
1. [x] - `p1` - Actor sends POST /settings-service/v1/declarations at a key that matches an existing `retired` declaration - `inst-decl-react-1`
2. [x] - `p1` - Authorize `create` on `gts.cf.core.settings.declaration.v1~` - `inst-decl-react-2`
3. [x] - `p1` - **IF** the decision is deny or cannot be obtained → **RETURN** `403` - `inst-decl-react-3`
4. [x] - `p1` - Construct the key and look up an existing declaration at that key - `inst-decl-react-4`
5. [x] - `p1` - **IF** no row exists → continue as an ordinary create - `inst-decl-react-5`
6. [x] - `p1` - **IF** a row exists with `status` = 'active' → **RETURN** `409` for the duplicate key; an active declaration anywhere on the setting's version-stripped path is found first and the request is an evolution or a `409` (Evolve Declaration), so a revive considers only a path with no active major - `inst-decl-react-6`
7. [x] - `p1` - **IF** the retired declaration is `module_contributed` → **RETURN** `409 ContributedDeclarationImmutable`, nothing written: a gear's declaration changes only through its owning module, a revive would replace its Schema Default and metadata, and the `settings` package an admin key is composed with is not reserved from modules — judged right after the key is looked up, before the request's value type, classification or default is resolved and before step-up, so no refusal about them stands in front of it - `inst-decl-react-15`
8. [x] - `p1` - **IF** the re-declaration flips the secret trait, changes the Scope Class or names a different `value_type_id` → **RETURN** `409` naming which, nothing written — judged before the Schema Default is validated, so a revive that cannot happen is refused as such rather than for a default that does not fit a type it could never adopt: the first two would move stored values rather than re-interpret them, and the setting's own GTS type is registered with its value type, which the Types Registry does not replace - `inst-decl-react-13`
9. [x] - `p1` - Validate the Schema Default and the remaining body fields against the stored value type before step-up is asked, as the create does - `inst-decl-react-16`
10. [x] - `p1` - Require a valid credential step-up assertion, because reactivation changes whether a live setting resolves; then take the retired row for update and read it again — a row gone, or outside the caller's administrative domain, answers `404`, and one no longer retired answers `409` - `inst-decl-react-7`
11. [x] - `p1` - **IF** step-up is absent or invalid → **RETURN** `403` - `inst-decl-react-8`
12. [x] - `p1` - Confirm the setting's own type in the Types Registry under the stored value type before anything is written, as a create registers before it inserts - `inst-decl-react-14`
13. [x] - `p1` - DB: UPDATE setting_declarations SET `status` = 'active', the re-declared Schema Default and metadata WHERE key = {key}; re-validate every retained value against the value type, which may have gained a compatible revision while the setting sat retired, flagging what no longer validates `needs_review` with its detail and clearing the flag on what validates again - `inst-decl-react-9`
14. [x] - `p1` - Invalidate the local cache for the affected scopes, since retained values re-enter resolution - `inst-decl-react-10`
15. [x] - `p1` - Emit a declaration-reactivated audit record - `inst-decl-react-11`
16. [x] - `p1` - **RETURN** `200` with the revived declaration - `inst-decl-react-12`

### Report a Revival

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-setting-declarations-revival-report`

**Actor**: `cpt-cf-settings-service-actor-platform-admin`

**Success Scenarios**:
- For a create body that would revive a retired declaration, the retained values that revive would bring back within the caller's administration, each masked by the stricter of its stored classification and the one the body derives and marked when the revive's re-validation would flag it, with the counts — nothing written, no step-up asked, and nothing the revive later requires or checks
- More retained values than `limit`, or a subtree past the node budget: the first `limit` in traversal order, and `complete` false with lower-bound counts when the budget cut the walk
- Values at standalone descendants and below them: neither listed nor counted
- No retained value within the caller's administration: an empty list with `total` 0

**Error Scenarios**:
- The body is refused as the create would refuse it — the create's own `400`, `404` or `409`, in the create's order — or an active declaration off the body's path — another vendor or package — holds the leaf name in the category (`409 LeafNameTaken`)
- The body would create, evolve or duplicate rather than revive (`404 nothing_to_revive`)
- The caller may not create declarations or may not read values (`403`, whatever exists); a retired declaration the caller could not read answers as no retired declaration does (`404 nothing_to_revive`)
- The Type Validator cannot answer (`503`)
- A `limit` below 1 (`400`)

**Steps**:
1. [x] - `p1` - Actor sends POST /settings-service/v1/declarations/revival-preview with the create body and an optional `limit` (default 100; above 500 clamped to it; below 1 refused `400`) - `inst-decl-revival-1`
2. [x] - `p1` - Authorize `create` on `gts.cf.core.settings.declaration.v1~` and `read` on the settings value base type, as browse does; **IF** either decision is deny or cannot be obtained → **RETURN** `403`, before the body is looked at, so the answer does not depend on what exists - `inst-decl-revival-2`
3. [x] - `p1` - Run the create's own checks on the body in the create's order up to the lookup at the composed key and on its version-stripped path — body shape (the create's JSON answers: `400`, `415`, `422`), category (`404`), key segments, display name, Scope Class present and valid (`400`) — and an active `module_contributed` declaration on the path (`409 ContributedDeclarationImmutable`); **IF** one refuses → **RETURN** the create's answer - `inst-decl-revival-3`
4. [x] - `p1` - **IF** the body would not revive a retired declaration — no retired declaration at the key, or an active declaration on the path, which makes the create an evolution — **OR** the retired declaration fails the single read's gates for the caller — its grant on the key, the administrative domain, `hidden`, licence → **RETURN** `404 nothing_to_revive`, the same answer either way, so beyond the create's own refusals in step 3 the answer reveals nothing of a declaration the caller could not read; the licence predicate joins in R2, as for the single read - `inst-decl-revival-4`
5. [x] - `p1` - Run the rest of the create's checks on the revive in the create's order, stopping where the create would ask for step-up — the retired declaration `module_contributed` (`409 ContributedDeclarationImmutable`), the value type's traits and the derived classification, the secret boundary (`409 SecretnessChanged`), the Scope Class unchanged (`409 ScopeClassChanged`), the value type unchanged (`409 ValueTypeChanged`), the Schema Default and the remaining body fields (`400`); **IF** one refuses → **RETURN** the create's answer - `inst-decl-revival-5`
6. [x] - `p1` - **IF** an active declaration off the body's path — another vendor or package — holds the retired declaration's leaf name in the category → **RETURN** `409 LeafNameTaken` - `inst-decl-revival-6`
7. [x] - `p1` - Walk the caller's own tenant and its descendants breadth-first under the subtree node budget, standalone descendants and their subtrees left out, noting whether the budget or the depth ceiling cut the walk - `inst-decl-revival-7`
8. [x] - `p1` - DB: under the caller's value-read scope, COUNT the retired declaration's subject-less rows at the walked tenants, SELECT its distinct values there grouped with their multiplicity, and SELECT the listed rows in the walk's order, a batch of tenants at a time, until `limit` rows are in hand - `inst-decl-revival-8`
9. [x] - `p1` - Re-validate each distinct value, once, as the revive would re-validate it, so every row holding a value that would be flagged is counted and marked, one that validates again is reported clear and a secret is never flagged; **IF** the Type Validator cannot answer → **RETURN** `503` - `inst-decl-revival-9`
10. [x] - `p1` - Mask each listed value by the stricter of its stored classification and the one the body derives, and withhold the re-validation detail whenever the value is masked - `inst-decl-revival-10`
11. [x] - `p1` - **RETURN** `200` with the declaration's `key` and id, the first `limit` values in traversal order, `total`, `total_needs_review`, `scanned` and `complete`; nothing stored, no step-up, no audit, no cache touched - `inst-decl-revival-11`

### Purge Declaration

- [x] `p2` - **ID**: `cpt-cf-settings-service-flow-setting-declarations-purge`

**Actor**: `cpt-cf-settings-service-actor-platform-admin`

**Success Scenarios**:
- A retired admin-authored declaration is removed for good in one transaction, with its values, tenant-access restrictions and staged secrets; its Credential Store entries are released after the commit; `204`

**Error Scenarios**:
- The actor is not authorized
- Credential step-up absent or invalid
- The declaration is not found or is outside the caller's administrative domain
- The declaration is `module_contributed`
- The declaration is not `retired`
- `If-Match` absent or stale
- A Credential Store release is refused — the purge stands

**Steps**:
1. [x] - `p2` - Actor sends POST /settings-service/v1/declarations/{id}/purge with `If-Match` and a step-up assertion - `inst-decl-purge-1`
2. [x] - `p2` - Authorize `delete` on `gts.cf.core.settings.declaration.v1~` - `inst-decl-purge-2`
3. [x] - `p2` - **IF** the decision is deny or cannot be obtained → **RETURN** `403` - `inst-decl-purge-3`
4. [x] - `p2` - Require a valid credential step-up assertion, because purge is irreversible and frees the key and its category - `inst-decl-purge-4`
5. [x] - `p2` - Count the step-up outcome in `settings_step_up_total` under the operation `purge` - `inst-decl-purge-5`
6. [x] - `p2` - **IF** step-up is absent or invalid → **RETURN** the step-up challenge (`401`/`403`); nothing is read - `inst-decl-purge-6`
7. [x] - `p2` - DB: open one transaction and take the declaration row for update, so a value write in flight and a concurrent revive or retire are serialized against the purge - `inst-decl-purge-7`
8. [x] - `p2` - **IF** the row is absent or outside the caller's administrative domain → **RETURN** `404` - `inst-decl-purge-8`
9. [x] - `p2` - **IF** `source` is `module_contributed` → **RETURN** `409 ContributedDeclarationImmutable`, judged before the status - `inst-decl-purge-9`
10. [x] - `p2` - **IF** `status` is not `retired` → **RETURN** `409 DeclarationNotRetired`; purge never retires - `inst-decl-purge-10`
11. [x] - `p2` - Evaluate the `If-Match` precondition; **IF** absent → **RETURN** `428`; **IF** stale → **RETURN** `412` - `inst-decl-purge-11`
12. [x] - `p2` - DB: SELECT the `(tenant_id, secret_ref)` pairs of every `setting_values` row of the declaration that holds a `secret_ref` — at any scope and with or without a subject, whatever the caller's scope, because the cascade removes them all — and of every `pending_secrets` row of the declaration, keeping out every reference a row of another declaration still holds (an evolution copies its predecessor's references into the successor) - `inst-decl-purge-12`
13. [x] - `p2` - DB: DELETE FROM setting_declarations WHERE id = {id} AND `updated_at` = {the version the tag was compared against}; `setting_values`, `tenant_permissions` and `pending_secrets` go by `ON DELETE CASCADE`; **IF** no row matched → **RETURN** `412` - `inst-decl-purge-13`
14. [x] - `p2` - Emit the declaration-purged audit record, attributed to the authenticated caller: operation `remove` on the definition resource, the definition as pre-image, no post-image and no stored value - `inst-decl-purge-14`
15. [x] - `p2` - Invalidate the local cache for the key inside the transaction - `inst-decl-purge-15`
16. [x] - `p2` - Commit the transaction - `inst-decl-purge-16`
17. [x] - `p2` - Evict the key from the effective cache again, and check the references kept for another declaration again: one no declaration holds any more joins the release - `inst-decl-purge-17`
18. [x] - `p2` - Start the release task, detached from the request, with the key and the collected pairs; log one `info` line with the key, the number of entries, the number kept for another declaration and the request id - `inst-decl-purge-18`
19. [x] - `p2` - **FOR EACH** collected pair, at most 4 at a time → ask the Secret Manager to release the entry - `inst-decl-purge-19`
20. [x] - `p2` - **IF** the entry is released, or is already absent → count `ok` in `settings_declaration_purge_secret_releases_total` - `inst-decl-purge-20`
21. [x] - `p2` - **ELSE** → log at `error` with the key, the tenant and the error's log-safe text — never the secret — count `failed`, and keep the purge - `inst-decl-purge-21`
22. [x] - `p2` - When every pair is done, log one `info` line with the released and failed counts and the request id - `inst-decl-purge-22`
23. [x] - `p2` - Wait for the release task and **RETURN** `204`; a dropped request does not stop the task - `inst-decl-purge-23`

A start line without an end line shows a release cut short by a process stop: nothing retries it, the purge stands, and the entries it did not reach are orphans an operator finds in the Credential Store under the reference prefix the gear derives from the key and the tenant the log names.

A purge's duration grows with the number of Credential Store entries it releases (at most 4 at a time), so it is excluded from the REST latency target `cpt-cf-settings-service-nfr-performance-read-cache` (DESIGN §7).

### Evolve Declaration

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-setting-declarations-evolve`

**Actor**: `cpt-cf-settings-service-actor-platform-admin`

**Success Scenarios**:
- An active admin-authored setting re-declared with a different value type, Schema Default or scope class evolves to the next free major on its version-stripped path: a new declaration under a new key, holding every value of the previous major re-validated, while the previous major is retired
- A later shape change evolves whichever major is active, never a retired one

**Error Scenarios**:
- The re-declaration matches the active declaration, or changes only metadata: `409`, so a retry after a lost response never mints a major and metadata goes through PATCH
- Credential step-up absent or invalid
- The active declaration is contributed by a gear, or the new value type is on the other side of the secret boundary: `409`, nothing written

**Steps**:
1. [x] - `p1` - Look up every declaration on the request's version-stripped path — `(vendor, category, leaf name)`, whatever the major or status; **IF** one is active, the request is an evolution rather than a create or a revive - `inst-decl-evolve-1`
2. [x] - `p1` - **IF** the active declaration is contributed by a gear → **RETURN** `409 ContributedDeclarationImmutable`, nothing written — judged before anything about the request is resolved and before step-up, since it decides who may change the row at all; a gear's declaration evolves when that gear registers a new major - `inst-decl-evolve-8`
3. [x] - `p1` - **IF** the request changes none of the value type, the Schema Default and the scope class of the active declaration → **RETURN** `409` for the duplicate key - `inst-decl-evolve-2`
4. [x] - `p1` - Require a valid credential step-up assertion, because evolution retires a live setting's current major; **IF** absent or invalid → **RETURN** `401`/`403`; then take the active row for update and read it again — a row gone, or outside the caller's administrative domain, answers `404`, and one no longer active answers `409` - `inst-decl-evolve-3`
5. [x] - `p1` - **IF** the new value type flips the secret trait → **RETURN** `409`, since stored values would be re-interpreted rather than moved - `inst-decl-evolve-4`
6. [x] - `p1` - Mint the next free major: one above the highest major the path has ever used, retired ones included, composed as `<vendor>.settings.<category>.<name>.vN~` - `inst-decl-evolve-5`
7. [x] - `p1` - In one transaction: retire the active predecessor and record it; insert the successor at the new key — its own GTS setting type registered first, its create recorded; copy every predecessor value to it at the same scope under an update lock, carrying `set_by` and `set_by_tenant` unchanged, re-validating each against the new value type and flagging what fails `needs_review` with its detail, never coercing or dropping it - `inst-decl-evolve-6`
8. [x] - `p1` - Invalidate the local cache for both keys, and **RETURN** `200` with the successor and `evolved: true` - `inst-decl-evolve-7`

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
- A single declaration, or a filtered page of declarations, returned with resolved traits and `display_name` when set, for client rendering

**Error Scenarios**:
- Declaration outside the caller's visibility, domain, or licence gate
- Unsupported OData filter or ordering expression

**Steps**:
1. [x] - `p1` - Actor sends GET /settings-service/v1/declarations/{id} or GET /settings-service/v1/declarations with optional OData `$filter`, `$orderby` over `key`, `category_id`, `mode` or `status`, and a pagination cursor; `$orderby` over `domain_affinity` or `owner_module` is refused `400 odata_unsortable_field` because either may be empty and a page cursor cannot carry an empty value, and `$select` is not taken and is refused with `400` rather than ignored - `inst-decl-read-1`
2. [x] - `p1` - Authorize `read` on `gts.cf.core.settings.declaration.v1~` and obtain the `AccessScope` constraints - `inst-decl-read-2`
3. [x] - `p1` - **IF** the decision is deny or cannot be obtained → **RETURN** `403` - `inst-decl-read-3`
4. [ ] - `p1` - Derive the combined visibility and domain-affinity predicate from the `AccessScope` constraints; the licence predicate joins it in R2, when the License Resolver exists to answer it - `inst-decl-read-4`
5. [x] - `p1` - **IF** an OData expression references an unmapped field or an unsupported operator → **RETURN** `400` - `inst-decl-read-5`
6. [x] - `p1` - DB: SELECT declarations with the combined predicate applied inside the query - `inst-decl-read-6`
7. [x] - `p1` - **IF** a single-declaration read is filtered out → **RETURN** `404` rather than `403`, so a gated declaration's existence is not disclosed - `inst-decl-read-7`
8. [x] - `p1` - Resolve each returned declaration's trait set for client rendering - `inst-decl-read-8`
9. [x] - `p1` - **RETURN** `200` with the declaration or page, each carrying `key`, `value_type_id`, resolved traits, and `display_name` when set (omitted otherwise, never derived from `leaf_slug`); a page carries `page_info.total_count`, the declarations the whole listing holds under its filter and visibility, counted without the cursor - `inst-decl-read-9`

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

**Output**: A per-field class of immediate, immutable, or step-up-gated, or a `400` naming a field whose shape or length is wrong

**Steps**:
1. [x] - `p1` - **FOR EACH** field present in the request - `inst-decl-mutcls-1`
   1. [x] - `p1` - **IF** the field is `display_name`, `description`, `mode`, `domain_affinity`, or `licence_feature`, or it tightens a gate — sets `requires_step_up` true or `anonymous_exposable` false → classify as immediate; a `display_name` must be `null` or a string of 1..256 Unicode characters after trimming with no control character, and any other shape, length or content → **RETURN** `400` naming the field - `inst-decl-mutcls-2`
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
5. [x] - `p1` - **FROM** `active` **TO** `retired` **WHEN** the setting evolves to a new major with a valid credential step-up assertion; the successor enters `active` as a new declaration in the same transaction - `inst-decl-state-5`

A `retired` declaration leaves this machine when it is purged (`cpt-cf-settings-service-flow-setting-declarations-purge`): the row is deleted, so there is no terminal state to record; an `active` declaration cannot be purged.

## 5. Definitions of Done

### Declaration Entity and Schema

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-setting-declarations-entity-schema`

The system **MUST** persist declarations in a `setting_declarations` table carrying `key` unique via `uq_declaration_key`, `leaf_slug` unique per category **among active declarations** via the partial `uq_declaration_category_slug`, a non-null `category_id` foreign key to `categories` declared `ON DELETE RESTRICT`, a nullable `display_name` `varchar(256)` whose 1..256-Unicode-characters-after-trimming bound is enforced in the domain layer, added by an additive migration with no backfill so that pre-existing rows read as absent, with its trigram index `idx_declarations_display_name_trgm` on PostgreSQL only, a **non-null** `default_value`, `requires_step_up` defaulting to `true`, `anonymous_exposable` defaulting to `false`, and check constraints enforcing the `scope_class`, `mode`, `status`, `source`, and `data_classification` vocabularies. Two cross-field checks **MUST** hold in the database, not only in application code: an `anonymous_exposable` declaration may not be classified `secret` or `pii`, and `data_classification` being `secret` must be equivalent to `has_secret_trait`.

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

The system **MUST** partition declaration changes into descriptive metadata (`display_name`, `description`, `mode`, `domain_affinity`, `licence_feature`) applied immediately under `update` permission plus `If-Match` — the write itself conditional on the version the tag was compared against — behavior-affecting fields rejected as immutable, and behavior-affecting actions gated by credential step-up. `data_classification` tightening **MUST** be immediate while loosening **MUST** require step-up. No declaration edit may change a live setting's effective resolution without a gate, and an unrecognized field **MUST** be treated as immutable rather than immediate. A metadata change **MUST** evict the key's cached effective values, so a reclassification masks on the next read rather than when the cache TTL runs out.

**Implements**:
- `cpt-cf-settings-service-algo-setting-declarations-mutation-class`
- `cpt-cf-settings-service-flow-setting-declarations-update`

**Constraints**: `cpt-cf-settings-service-constraint-optimistic-concurrency`, `cpt-cf-settings-service-constraint-step-up-at-idp`

**Touches**:
- API: `PATCH /settings-service/v1/declarations/{id}`
- Entities: `SettingDeclaration`

### Retire and Reactivate Lifecycle

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-setting-declarations-lifecycle`

Retire **MUST** be an immediate soft delete setting `status` to `retired` in one transaction with cache invalidation and signal publication, **MUST** require credential step-up, and **MUST** retain every stored value while excluding the declaration from resolution. Reactivation **MUST** be expressed as re-declaring the key, also step-up gated; every retained value **MUST** be re-validated against the value type before it goes live, and the re-declaration **MUST** be refused when it flips the secret trait, changes the Scope Class or names a different value type — the setting's own GTS type is registered with its value type, and the Types Registry does not replace a registered type. Evolution **MUST** be expressed as re-declaring an active setting with a different value type, Schema Default or scope class, step-up gated: it **MUST** insert the next free major under a new key and type, copy and re-validate every value of the active major, and retire that major, all in one transaction, and **MUST** refuse a request matching the active declaration, a contributed declaration, and a new value type across the secret boundary. None of these actions goes through the value write path, and none deletes values. The copy of every value on evolution **MUST** carry its `set_by` and `set_by_tenant` unchanged.

**Implements**:
- `cpt-cf-settings-service-flow-setting-declarations-retire`
- `cpt-cf-settings-service-flow-setting-declarations-reactivate`
- `cpt-cf-settings-service-flow-setting-declarations-evolve`
- `cpt-cf-settings-service-state-setting-declarations-lifecycle`

**Constraints**: `cpt-cf-settings-service-constraint-step-up-at-idp`

**Touches**:
- API: `DELETE /settings-service/v1/declarations/{id}`
- API: `POST /settings-service/v1/declarations`
- DB Table: `setting_declarations`
- Entities: `DeclarationStatus`

### Revival Report

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-setting-declarations-revival-report`

The system **MUST** serve `POST /settings-service/v1/declarations/revival-preview`, taking the body of the create that would revive a retired declaration. It **MUST** authorize `create` on the declaration type and `read` on the settings value base type before looking at the body, refusing `403` whatever exists; **MUST** answer the create's own refusals in the create's order, then `409 LeafNameTaken` for an active declaration off the body's path — another vendor or package — holding the leaf name in the category; and **MUST** answer `404 nothing_to_revive` both when the body would not revive and when the retired declaration fails the single read's gates for the caller. It **MUST** report the declaration's retained subject-less values within the caller's administration — the caller's tenant and its descendants under the subtree node budget, standalone descendants left out of the list and the counts — each re-validated as the revive would (a value that validates again reported clear, a secret never flagged), masked by the stricter classification with its detail withheld when masked; the first `limit` (default 100, above 500 clamped, below 1 refused `400`) in traversal order, `total`, `total_needs_review`, `scanned` and `complete`. It **MUST** store nothing, ask no step-up, emit no audit and touch no cache, **MUST** answer `503` when the Type Validator cannot answer, and is advisory: reactivation neither requires it nor checks it. Beyond the create's own refusals, which a caller allowed to create gets from the create as well, it **MUST** disclose nothing of a retired declaration the caller could not read.

**Implements**:
- `cpt-cf-settings-service-flow-setting-declarations-revival-report`

**Constraints**: `cpt-cf-settings-service-constraint-rbac-policy-enforcer`

**Touches**:
- API: `POST /settings-service/v1/declarations/revival-preview`
- DB Table: `setting_values`

### Purge of a Retired Declaration

- [x] `p2` - **ID**: `cpt-cf-settings-service-dod-setting-declarations-purge`

Purge **MUST** refuse a declaration that is `module_contributed` (`409 ContributedDeclarationImmutable`, judged first) or not `retired` (`409 DeclarationNotRetired`), and one that is absent or outside the caller's administrative domain (`404`); it **MUST** require `If-Match`. `delete` authorization and credential step-up **MUST** be judged before any row is read. Every declaration action that requires credential step-up — retire, reactivate, evolve, a metadata update that loosens the classification or clears a step-up flag, and purge — **MUST** count its step-up outcome in `settings_step_up_total` under its own operation (`retire`, `reactivate`, `evolve`, `update`, `purge`).

Purge **MUST** run as one transaction that first takes the declaration row for update, judges the refusals on the locked row, collects the `(tenant, secret_ref)` pairs of every value row of the declaration — whatever its scope or subject — and of every staged secret, except a reference another declaration still holds, hard-deletes the row guarded on the compared `updated_at` (cascading its values, tenant permissions and staged secrets) and writes one audit record, attributed to the authenticated caller (`remove` on the definition resource, the definition as pre-image, no post-image, no stored value).

After the commit it **MUST** check the references it kept for another declaration again and add every one no declaration holds any more, then release every collected entry through the Secret Manager in a task detached from the request, at most 4 at a time, counting each outcome in `settings_declaration_purge_secret_releases_total{result}`, logging a refusal at `error` with the key, the tenant and the error's log-safe text, never the secret, and one line when the task starts and one when it ends, both carrying the request id. A refusal **MUST NOT** undo the purge; the request **MUST** wait for the task before answering `204`, and a dropped request **MUST NOT** stop the task.

Purge **MUST** publish no domain event and **MUST NOT** remove or alter existing audit records. A later declaration at the key **MUST** be accepted under the same value type only — the Types Registry keeps the key's type after the row is gone, and the create flow's type registration answers a different value type with `409` (DESIGN §4.7), as for any existing key. A category whose last declaration, active or retired, is purged **MUST** become deletable (`cpt-cf-settings-service-fr-settings-category-model`). The design is `cpt-cf-settings-service-component-declaration-management` and `cpt-cf-settings-service-seq-declaration-purge`.

**Implements**:
- `cpt-cf-settings-service-flow-setting-declarations-purge`

**Constraints**: `cpt-cf-settings-service-constraint-step-up-at-idp`, `cpt-cf-settings-service-constraint-optimistic-concurrency`, `cpt-cf-settings-service-constraint-audit-and-events`

**Touches**:
- API: `POST /settings-service/v1/declarations/{id}/purge`
- DB Table: `setting_declarations`
- DB Table: `setting_values`
- DB Table: `tenant_permissions`
- DB Table: `pending_secrets`
- DB Table: `audit_records`
- Entities: `SettingDeclaration`, `DeclarationStatus`
- Port: the Credential Store through the Secret Manager (`delete_secret`)

### Contributed Declaration Protection

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-setting-declarations-contributed-protection`

A declaration whose `source` is `module_contributed` **MUST** be rejected for administrative update, administrative retire and an administrative re-declaration that would revive it, with a contributed-immutable conflict, and the database **MUST** enforce that `owner_module` is present exactly when `source` is `module_contributed`. This feature reads contributed declarations but never writes them.

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

Declaration reads **MUST** be visibility- and domain-gated with the predicate applied inside the query, **MUST** leave the seam through which the licence gate joins that predicate in R2, **MUST** return the setting `key`, its `value_type_id`, and the resolved trait set for client rendering, **MUST** return `display_name` when set and omit it otherwise, never deriving one from `leaf_slug`, and **MUST** report a gated single-declaration read as absent rather than forbidden.

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
- [x] A `PATCH` changing `data_classification` evicts the key from the effective-value cache, and the next read masks by the new class without waiting for the TTL
- [x] A `PATCH` loosening `data_classification` from `pii` to `public` without step-up returns `403`, and succeeds with a valid step-up assertion
- [x] A `PATCH` clearing `requires_step_up` or enabling `anonymous_exposable` without step-up returns `403` and leaves the flag unchanged; the opposite edits apply immediately
- [x] A `PATCH` on a `module_contributed` declaration returns a contributed-immutable conflict
- [x] Re-declaring a retired `module_contributed` declaration at its key returns a contributed-immutable conflict whatever the request names — the same shape, a retype, an unknown value type, a contradicted classification — and leaves it retired with its Schema Default and owner
- [x] Re-declaring an active `module_contributed` declaration returns a contributed-immutable conflict whatever the request names, without asking for step-up, and mints no major
- [x] A `PATCH` without `If-Match` returns `428`, and with a stale `If-Match` returns `412`
- [x] Retiring a declaration without step-up returns `403`
- [x] Retiring a declaration sets `status` to `retired`, leaves every row in `setting_values` intact, and does not go through the value write path
- [x] Retiring a declaration invalidates the cache for the affected scopes in the same transaction that flips the status
- [x] A retired declaration still blocks deletion of its category
- [x] Re-declaring a retired key with step-up revives the row to `active` and its retained values participate in resolution again
- [x] Re-declaring a retired key with a different value type is refused `409 value_type_changed` with nothing written; a revive under the same type re-validates every retained value, one that validates goes live and one that does not is flagged `needs_review` with its detail
- [x] Re-declaring a retired key with a type that flips the secret trait, or with a different scope class, returns `409` and leaves the row retired and its values untouched
- [x] The revival report for a create body that would revive a retired declaration lists its retained values at the caller's tenant and descendants in traversal order with `total`, `total_needs_review`, `scanned` and `complete`, a value the revive would flag marked with its detail, a previously flagged one that validates again reported clear; nothing is written, no audit is recorded and no step-up is asked
- [x] A retained value at a standalone descendant, or below one, is neither listed nor counted by the revival report
- [x] A `pii` retained value is masked, with its detail withheld, for a caller without the PII entitlement; a secret one is the mask token for every caller and never flagged; a stored `public` value under a body deriving `pii` is masked
- [x] A retired declaration with no retained value in the caller's administration gives `200` with an empty list, `total` 0 and `complete` true
- [x] The revival report answers `404 nothing_to_revive` for a body that would create, evolve or duplicate, distinct from a missing category's `404`
- [x] The revival report answers the create's own refusals in the create's order: a missing category `404`, a malformed segment `400`, an active module-contributed declaration on the path `409 ContributedDeclarationImmutable`, an invalid Schema Default `400`
- [x] The revival report answers the revive's refusals `409 SecretnessChanged`, `409 ScopeClassChanged` and `409 ValueTypeChanged`, and `409 LeafNameTaken` for a leaf name held by an active declaration off the body's path — another vendor or package — in the category, judged after the create's checks so an invalid Schema Default answers first
- [x] A body that changes both the Scope Class and the value type gets `409 ScopeClassChanged`
- [x] The revival report answers `403` without `create` or without `read` on the settings value base type, whatever exists, and `404 nothing_to_revive` for a retired declaration `hidden` from the caller — the same answer as when nothing is retired
- [ ] The revival report answers `404 nothing_to_revive` for a retired declaration outside the caller's grant on its key or its administrative domain, or behind a licence its tenant lacks — the same answer as when nothing is retired
- [x] An active module-contributed declaration on the body's path answers `409 ContributedDeclarationImmutable` exactly as the create with the same body does, whether or not the caller could read it
- [x] More retained values than `limit` give the first `limit` and exact counts; a subtree past the node budget gives `complete` false and lower-bound counts; a `limit` above 500 is clamped and one below 1 refused `400`
- [x] A Type Validator that cannot answer makes the revival report `503`
- [x] A revive with step-up succeeds whether or not the revival report was requested first, and nothing the report answered is consulted by it
- [x] Re-declaring an active setting with a different value type evolves it: `200` with `evolved: true`, the next major created and active, every value copied and re-validated with failures flagged `needs_review`, the previous major retired
- [x] A later shape change evolves the active major, not a retired one, and a repeat of a request that matches the active declaration returns `409` without minting a major
- [x] An evolution without step-up, of a contributed declaration, or across the secret boundary is refused and leaves the active declaration and its values untouched
- [x] Re-declaring a key that is already `active` returns `409`
- [ ] A Dependency Group naming a key that resolves to no active declaration returns `400`
- [ ] An attempt to edit an existing Dependency Group or its constraint in place is rejected
- [ ] A declaration read outside the caller's visibility, domain, or licence gate returns `404` rather than `403`
- [ ] Every declaration read returns the `key`, the `value_type_id`, and the resolved trait set
- [x] Every create, update, retire, and reactivate produces exactly one audit record, and update and retire records carry pre-images
- [x] Creating a declaration with a `display_name` returns it in the `201` body and on every later declaration read, creating one without it omits the field, and nothing derives a `display_name` from `leaf_slug`
- [x] A `display_name` empty or whitespace-only after trimming, longer than 256 Unicode characters, or containing a control character, is refused `400` naming the field on create and on `PATCH`, while 1 and 256 Unicode characters (multi-byte included) are accepted, and a value exceeding 256 only by surrounding whitespace is accepted and stored trimmed
- [x] A `PATCH` with `display_name` sets it, with `display_name: null` clears it, and without the field leaves it unchanged, and the edit refreshes `updated_at` and the ETag without moving `last_change_at`
- [x] A `PATCH` of `display_name` on a `module_contributed` declaration returns `409 ContributedDeclarationImmutable`
- [x] A declaration row that predates the `display_name` column reads as having no display name
- [x] An evolution copies every value with its original `set_by` and `set_by_tenant` unchanged
- [x] A tenant administrator's own value stays shown to that tenant after evolve
- [x] Purge of an `active` declaration returns `409 DeclarationNotRetired` and the row stays active
- [x] Purge of a retired `module_contributed` declaration returns `409 ContributedDeclarationImmutable`, judged before the status, and removes nothing
- [x] Purge without a fresh step-up assertion returns `401`/`403` and removes nothing
- [x] Purge without `If-Match` returns `428`, and with a stale `If-Match` returns `412`; neither removes anything
- [x] Purge of a declaration outside the caller's administrative domain returns `404`
- [x] Purge of a retired admin-authored declaration returns `204`, a following GET returns `404`, and its `setting_values`, `tenant_permissions` and `pending_secrets` rows are gone
- [x] After a purge, its category can be deleted when no other declaration references it
- [x] Every `secret_ref` of the purged declaration's values — at any scope, with or without a subject — and of its staged secrets is released through the Secret Manager after the commit, each release counting `result=ok`
- [x] After an evolution of a secret setting, a purge of its retired predecessor releases none of the entries the successor still holds, and the successor still resolves its secret values
- [x] Two purges of declarations that share an entry, committed together, still release it: the later re-check after its commit finds no declaration holding it
- [x] A release the Credential Store refuses leaves the purge done, logs one `error` line with the key, the tenant and the error's log-safe text and never the secret, and counts `result=failed`
- [x] The release task runs at most 4 releases at a time, logs one `info` line when it starts and one when it ends, both carrying the request id, and finishes even when the request that started it is dropped
- [x] Purge writes exactly one audit record, attributed to the authenticated caller — `remove` on the definition resource with the definition as pre-image, no post-image and no stored value — and leaves every earlier record of the key in place
- [x] Purge publishes no domain event
- [x] The `204` is answered only after the release task has ended
- [x] After a purge commits, the next effective read of the key finds no declaration, because the key is evicted from the effective cache
- [x] Purge without `delete` authorization returns `403` and removes nothing
- [x] The gates are judged before any row is read: purge of a non-existent id or of an `active` declaration answers `403` without `delete` authorization and `401`/`403` without a fresh step-up — never `404` or `409`
- [x] After a purge, a declaration at the key is accepted under the same value type and refused `409` under a different one
- [x] A purge racing a revive of the same declaration lets exactly one win and never deletes an active declaration
- [x] A revive that loses the race to a purge answers `404`, never `500`
- [x] A purge racing a value write that stores a secret leaves no Credential Store entry unreleased
- [x] Retire, reactivate, evolve, a step-up-gated metadata update and purge each count their step-up outcome in `settings_step_up_total` under their own operation
