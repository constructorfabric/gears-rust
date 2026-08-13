<!-- Created: 2026-08-10 by Constructor Tech -->
<!-- Updated: 2026-08-10 by Constructor Tech -->

# Feature: Typed Value Validation

- [ ] `p1` - **ID**: `cpt-cf-settings-service-featstatus-typed-value-validation`

- [ ] `p1` - `cpt-cf-settings-service-feature-typed-value-validation`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Value Validation Against GTS Type](#value-validation-against-gts-type)
  - [Value Size and Canonicality Guards](#value-size-and-canonicality-guards)
  - [Trait Resolution](#trait-resolution)
  - [Classification Denormalization Sync](#classification-denormalization-sync)
- [4. States (CDSL)](#4-states-cdsl)
  - [SettingValue Review State](#settingvalue-review-state)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Type Validator Component and Registry Client](#type-validator-component-and-registry-client)
  - [Structural and Trait Validation](#structural-and-trait-validation)
  - [Value Size Cap](#value-size-cap)
  - [Numeric Canonicality](#numeric-canonicality)
  - [Trait Resolution and Rendering Metadata](#trait-resolution-and-rendering-metadata)
  - [SettingValue Entity and Schema](#settingvalue-entity-and-schema)
  - [Value Scope and Uniqueness Invariants](#value-scope-and-uniqueness-invariants)
  - [Needs-Review Flag](#needs-review-flag)
  - [Classification Denormalization](#classification-denormalization)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Delivers the Type Validator: structural validation of a setting value against the GTS value type named by the left half of its key, trait-driven rules enforced as hard checks rather than advisories, the size and numeric-canonicality guards that bound what a value may be, and the resolved trait set consumers use for rendering. Also establishes the `SettingValue` entity and the `setting_values` table that effective-value resolution reads.

### 1.2 Purpose

The service consumes GTS types and never authors them; the types registry owns that. What this feature owns is the decision to treat trait rules as **hard** checks. A cron expression that does not parse, a regex that does not compile, an entity reference that does not resolve, or an enum member outside its dynamic source are all rejected at validation time rather than passed through with a warning, because a setting value that fails at consumption time fails inside whichever gear read it, far from the administrator who set it.

Two guards exist for reasons that are easy to miss and expensive to discover later. The 64 KiB cap on a serialized value keeps the hot read cache, audit pre-images and post-images, and apply-preview payloads bounded — a settings value is a configuration datum, not a blob. The IEEE-754 round-trip check rejects integers beyond the double-precision integer range and decimals finer than a double resolves, because activation compares values through a canonical encoding that cannot carry them; a setting needing more range or precision declares a string type instead.

The `setting_values` schema carries two invariants worth reading carefully before writing migrations. Exactly one of `value` and `secret_ref` is set, so a row can be neither doubly-valued nor valueless. And SQL `NULL` in the `value` column means *no inline value here*, which is not the JSON value `null` — a setting whose type admits `null` stores a non-`NULL` column holding JSON `null`, so the exactly-one check reads it as a value like any other.

**Requirements**: `cpt-cf-settings-service-fr-typed-value-validation`

**Principles**: `cpt-cf-settings-service-principle-consume-gts`, `cpt-cf-settings-service-principle-fail-closed`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-settings-service-actor-types-registry` | Owns the GTS schemas and trait sets this feature resolves and validates against; never written to from here |
| `cpt-cf-settings-service-actor-platform-admin` | Receives the field-level validation errors when a Schema Default or an override fails its type |
| `cpt-cf-settings-service-actor-internal-caller` | Consumes the resolved trait set returned alongside an effective value |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — §5.2 Typed Values and Validation
- **Design**: [DESIGN.md](../DESIGN.md) — §4.1 (Entity `SettingValue`), §4.2 (Component: Type Validator), §4.7 (Table `setting_values`)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.4
- **Dependencies**: entry 2.3 setting declarations, since the value type is the left half of a declaration key and there is nothing to validate against without one; entry 2.1 gear foundation for persistence, the `TypeValidator` trait declaration, and Problem mapping. Declaration creation in 2.3 calls this validator for its Schema Default, so the two meet at that trait.
- **Not applicable**: GTS type authoring and the schema registry are owned by the `types-registry` gear. Secret value storage and masking are owned by the Secret Manager in a later wave; this feature only records that a value is held by reference. No administrative write path for values exists in this wave, because values are set through staged changes, so `setting_values` is populated only by seeded rows and tests until staging lands. Apply-time re-validation is deliberately absent by design: a staged change carries the already-validated value.

## 2. Actor Flows (CDSL)

Not applicable. Validation is an internal service invoked by other features rather than a user-facing interaction: declaration creation in entry 2.3 calls it for a Schema Default, and staging calls it for an override in a later wave. The administrator-visible outcome is the field-level error array returned on the calling feature's own endpoint.

## 3. Processes / Business Logic (CDSL)

### Value Validation Against GTS Type

- [ ] `p1` - **ID**: `cpt-cf-settings-service-algo-typed-value-validation-validate`

**Input**: A GTS type id and a candidate value

**Output**: A validation result that is either accepted, or a list of field-level errors

**Steps**:
1. [ ] - `p1` - Resolve the type's JSON Schema and its trait annotations through the types registry client - `inst-tvv-val-1`
2. [ ] - `p1` - **IF** the type cannot be resolved → **RETURN** a validation failure rather than accepting the value, so an unresolvable type fails closed - `inst-tvv-val-2`
3. [ ] - `p1` - Invoke the value size and canonicality guards on the candidate - `inst-tvv-val-3`
4. [ ] - `p1` - **IF** a guard rejects the value → **RETURN** its error without attempting schema validation - `inst-tvv-val-4`
5. [ ] - `p1` - Validate the value structurally against the JSON Schema dialect the registry publishes - `inst-tvv-val-5`
6. [ ] - `p1` - Assert every `format` keyword the schema declares, such as URI and IP address forms, as a hard check rather than an annotation - `inst-tvv-val-6`
7. [ ] - `p1` - **FOR EACH** trait-driven rule on the resolved trait set - `inst-tvv-val-7`
   1. [ ] - `p1` - Assert a cron-dialect value parses under its declared dialect - `inst-tvv-val-8`
   2. [ ] - `p1` - Assert a regex-bearing value compiles - `inst-tvv-val-9`
   3. [ ] - `p1` - Assert a dynamic-enum value is a member of its declared source - `inst-tvv-val-10`
   4. [ ] - `p1` - Assert an entity reference resolves - `inst-tvv-val-11`
8. [ ] - `p1` - Collect every failure as a field-level error carrying the field path, a stable code, and a message, rather than stopping at the first - `inst-tvv-val-12`
9. [ ] - `p1` - **RETURN** accepted when no error was collected, otherwise the collected errors - `inst-tvv-val-13`

### Value Size and Canonicality Guards

- [ ] `p1` - **ID**: `cpt-cf-settings-service-algo-typed-value-validation-guards`

**Input**: A candidate value

**Output**: Accepted, or the guard error that rejected it

**Steps**:
1. [ ] - `p1` - Serialize the candidate to its JSON representation - `inst-tvv-guard-1`
2. [ ] - `p1` - **IF** the serialized form exceeds 64 KiB → **RETURN** a value-too-large error, because the cap bounds the hot cache, audit images, and apply-preview payloads - `inst-tvv-guard-2`
3. [ ] - `p1` - **FOR EACH** number anywhere in the value, including nested positions - `inst-tvv-guard-3`
   1. [ ] - `p1` - Round-trip the number through IEEE-754 binary64 - `inst-tvv-guard-4`
   2. [ ] - `p1` - **IF** the round trip does not return the number unchanged in value → **RETURN** a not-canonical error naming the position - `inst-tvv-guard-5`
4. [ ] - `p1` - **RETURN** accepted, noting that a setting needing wider range or finer precision than a double carries declares a string type instead - `inst-tvv-guard-6`

### Trait Resolution

- [ ] `p1` - **ID**: `cpt-cf-settings-service-algo-typed-value-validation-resolve-traits`

**Input**: A GTS type id

**Output**: The resolved trait set, or a resolution failure

**Steps**:
1. [ ] - `p1` - Resolve the type through the types registry client - `inst-tvv-traits-1`
2. [ ] - `p1` - **IF** the type cannot be resolved → **RETURN** a resolution failure; callers treat this as fail-closed rather than as an empty trait set - `inst-tvv-traits-2`
3. [ ] - `p1` - Collect the trait set, including the secret marker, multiline rendering, cron dialect, dynamic-enum source, and entity-reference target - `inst-tvv-traits-3`
4. [ ] - `p1` - **RETURN** the trait set, which serves two distinct callers: client rendering metadata, and create-time classification in entry 2.3 where the secret marker decides whether values route through the Secret Manager - `inst-tvv-traits-4`

### Classification Denormalization Sync

- [ ] `p1` - **ID**: `cpt-cf-settings-service-algo-typed-value-validation-classification-sync`

**Input**: A declaration whose `data_classification` is being written or changed

**Output**: Value rows whose denormalized classification matches their declaration

**Steps**:
1. [ ] - `p1` - Copy the declaration's `data_classification` onto every `setting_values` row written for that declaration - `inst-tvv-sync-1`
2. [ ] - `p1` - **WHEN** a declaration's classification changes → re-sync the denormalized column on every existing value row for that declaration - `inst-tvv-sync-2`
3. [ ] - `p1` - Perform the re-sync in the same transaction as the declaration change, so no window exists in which the two disagree - `inst-tvv-sync-3`
4. [ ] - `p1` - **RETURN** having preserved the table check tying a `secret` classification to the presence of `secret_ref` - `inst-tvv-sync-4`

## 4. States (CDSL)

### SettingValue Review State

- [ ] `p2` - **ID**: `cpt-cf-settings-service-state-typed-value-validation-review`

**States**: `valid`, `needs_review`

**Initial State**: `valid`

**Transitions**:
1. [ ] - `p2` - **FROM** `valid` **TO** `needs_review` **WHEN** an invalidating value-type upgrade means the stored value no longer validates against the current type - `inst-tvv-state-1`
2. [ ] - `p2` - **FROM** `needs_review` **TO** `valid` **WHEN** a valid value is re-staged and applied at that scope, or the override is reverted - `inst-tvv-state-2`

This wave delivers the `needs_review` and `needs_review_detail` columns, the partial index supporting the administrator's needs-review listing, and the guarantee that a flagged value is excluded from resolution. Both transitions are driven by later features: the flagging side by the Contribution Reconciler on a type upgrade, the clearing side by staging and Apply.

## 5. Definitions of Done

### Type Validator Component and Registry Client

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-typed-value-validation-component`

The system **MUST** provide a Type Validator resolving GTS types through the types registry client obtained in process, exposing validation of a value against a type id and resolution of a type's trait set. The validator **MUST** be generic over any GTS type id rather than coupled to settings, and a type that cannot be resolved **MUST** fail closed rather than validate vacuously.

**Implements**:
- `cpt-cf-settings-service-algo-typed-value-validation-validate`
- `cpt-cf-settings-service-algo-typed-value-validation-resolve-traits`

**Constraints**: `cpt-cf-settings-service-constraint-gts-value-validation`

**Touches**:
- Entities: Type Validator, `ValidationResult`, `TraitSet`

### Structural and Trait Validation

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-typed-value-validation-rules`

The system **MUST** validate a value structurally against the type's JSON Schema, **MUST** assert declared `format` keywords, and **MUST** enforce trait-driven rules — cron dialect parsing, regex compilation, dynamic-enum membership, and entity-reference resolution — as hard checks that reject the value, never as advisory annotations. Failures **MUST** be reported as a field-level array rather than a single first error.

**Implements**:
- `cpt-cf-settings-service-algo-typed-value-validation-validate`

**Touches**:
- Entities: `ValidationResult`

### Value Size Cap

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-typed-value-validation-size-cap`

The system **MUST** reject any value whose serialized JSON exceeds 64 KiB, and **MUST** apply the cap at validation time so the bound holds for the read cache, audit pre-images and post-images, and apply-preview payloads alike.

**Implements**:
- `cpt-cf-settings-service-algo-typed-value-validation-guards`

**Touches**:
- Entities: `SettingValue`

### Numeric Canonicality

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-typed-value-validation-canonicality`

The system **MUST** reject any number, at any nesting depth, that a round trip through IEEE-754 binary64 does not return unchanged in value, because the canonical encoding used downstream cannot carry integers beyond the double-precision integer range or decimals finer than a double resolves.

**Implements**:
- `cpt-cf-settings-service-algo-typed-value-validation-guards`

**Touches**:
- Entities: `SettingValue`

### Trait Resolution and Rendering Metadata

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-typed-value-validation-traits`

The system **MUST** resolve and expose a type's trait set covering the secret marker, multiline rendering, cron dialect, dynamic-enum source, and entity-reference target, and **MUST** make it available both as rendering metadata on reads and as the create-time input that decides whether a declaration is secret-backed.

**Implements**:
- `cpt-cf-settings-service-algo-typed-value-validation-resolve-traits`

**Touches**:
- Entities: `TraitSet`

### SettingValue Entity and Schema

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-typed-value-validation-value-schema`

The system **MUST** persist applied overrides in a `setting_values` table with a `declaration_id` foreign key declared `ON DELETE CASCADE`, a nullable `tenant_id` where `NULL` denotes platform scope, nullable `value` and `secret_ref`, a denormalized `data_classification`, the `needs_review` pair, and audit columns. A check **MUST** enforce that exactly one of `value` and `secret_ref` is set, and a second check **MUST** tie which one is set to the `secret` classification.

**Implements**:
- `cpt-cf-settings-service-algo-typed-value-validation-classification-sync`

**Constraints**: `cpt-cf-settings-service-constraint-postgres-primary-storage`

**Touches**:
- DB Table: `setting_values`
- Entities: `SettingValue`

### Value Scope and Uniqueness Invariants

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-typed-value-validation-scope-invariants`

Uniqueness **MUST** be expressed as two partial unique indexes rather than one plain index, because `NULL` marks platform scope and Postgres treats `NULL`s as distinct: at most one override per tenant per declaration, and at most one platform row per declaration. `tenant_id` **MUST** hold an id and never a path, so ancestry is never derived from this column, and it **MUST NOT** be a database foreign key because tenants live outside this schema.

**Touches**:
- DB Table: `setting_values`
- Entities: `SettingValue`

### Needs-Review Flag

- [ ] `p2` - **ID**: `cpt-cf-settings-service-dod-typed-value-validation-needs-review`

The system **MUST** carry a non-null `needs_review` boolean with an optional human-readable detail, **MUST** provide the partial index supporting an administrator listing of flagged values, and **MUST** guarantee that a flagged value is excluded from resolution and from apply until corrected.

**Implements**:
- `cpt-cf-settings-service-state-typed-value-validation-review`

**Touches**:
- DB Table: `setting_values`
- Entities: `SettingValue`

### Classification Denormalization

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-typed-value-validation-classification`

The system **MUST** copy the owning declaration's `data_classification` onto each value row on write and **MUST** re-sync it in the same transaction when the declaration's classification changes, because a Postgres partial-index predicate can only reference columns of the table being indexed and the search corpus split depends on that predicate.

**Implements**:
- `cpt-cf-settings-service-algo-typed-value-validation-classification-sync`

**Touches**:
- DB Table: `setting_values`, `setting_declarations`
- Entities: `SettingValue`

## 6. Acceptance Criteria

- [ ] A value conforming to its type's schema validates successfully
- [ ] A value violating its type's schema returns every field-level error, not only the first
- [ ] A declared `format` keyword such as a URI or IP address form is enforced, and a malformed instance is rejected
- [ ] A cron-dialect value that does not parse under its declared dialect is rejected
- [ ] A regex-bearing value that does not compile is rejected
- [ ] A dynamic-enum value outside its declared source is rejected
- [ ] An entity reference that does not resolve is rejected
- [ ] A value whose serialized JSON is just under 64 KiB is accepted, and one just over is rejected as too large
- [ ] An integer beyond the double-precision integer range is rejected as not canonical
- [ ] A decimal finer than a double resolves is rejected as not canonical
- [ ] A number nested inside an object or array is subject to the same canonicality check as a top-level one
- [ ] A GTS type that cannot be resolved causes validation to fail rather than pass vacuously
- [ ] Trait resolution returns the secret marker, and a type carrying it is reported as secret-backed
- [ ] Trait resolution failure is reported as a failure rather than as an empty trait set
- [ ] A `setting_values` row with both `value` and `secret_ref` set is rejected by the exactly-one check
- [ ] A `setting_values` row with neither `value` nor `secret_ref` set is rejected by the same check
- [ ] A setting whose type admits `null` stores JSON `null` in a non-`NULL` column and satisfies the exactly-one check
- [ ] A row whose `data_classification` is `secret` but whose `secret_ref` is absent is rejected
- [ ] Two platform-scope rows for one declaration are rejected by the partial unique index on the platform scope
- [ ] Two rows for the same declaration and tenant are rejected by the partial unique index on tenant scope
- [ ] A platform row and a tenant row for the same declaration coexist
- [ ] Deleting a declaration cascades to its value rows
- [ ] Changing a declaration's classification re-syncs every value row in the same transaction, leaving no window where the two disagree
- [ ] A value flagged `needs_review` carries a detail string, and clearing the flag clears the detail
