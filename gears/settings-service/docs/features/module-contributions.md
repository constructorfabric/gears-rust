<!-- Created: 2026-09-06 by Constructor Tech -->
<!-- Updated: 2026-09-06 by Constructor Tech -->

# Feature: Module-Contributed Declarations

- [ ] `p1` - **ID**: `cpt-cf-settings-service-featstatus-module-contributions`

- [ ] `p1` - `cpt-cf-settings-service-feature-module-contributions`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Register Declarations on Boot](#register-declarations-on-boot)
  - [Retire Declarations](#retire-declarations)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Contributed Key Admission](#contributed-key-admission)
  - [Reconcile One Declaration](#reconcile-one-declaration)
  - [Upgrade Migration to a New Major](#upgrade-migration-to-a-new-major)
  - [Setting Type Registration](#setting-type-registration)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Contribution Contract in the SDK](#contribution-contract-in-the-sdk)
  - [Reconciler Operations](#reconciler-operations)
  - [Namespaced Keys and Auto-Vivified Categories](#namespaced-keys-and-auto-vivified-categories)
  - [Reconcile Cases](#reconcile-cases)
  - [Upgrade Migration](#upgrade-migration)
  - [Type Registered Before the Row](#type-registered-before-the-row)
  - [Contributed Declarations Are Immutable to Administrators](#contributed-declarations-are-immutable-to-administrators)
  - [In-Process Only](#in-process-only)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Lets a gear register its own setting declarations from its init on every boot, idempotently, and retire them when it stops shipping them — through the `SettingsContributionClient` trait resolved from `ClientHub`, never through REST. The module supplies the derived half of each key and the value type it validates against; the reconciler extracts the category from the key, creates it if absent, registers the setting's type before inserting the row, and carries stored values across an upgrade to a new major with re-validation rather than silent coercion.

### 1.2 Purpose

Declaration ownership and value ownership are split on purpose: the gear that owns configuration declares it, and administrators change its values without being able to alter the declaration. That split is what lets the configuration surface grow with every installed gear and no core change, and it is why this path — not the administrative `POST` — is how most declarations come to exist.

The reconcile is idempotent and runs on every boot of the owning gear. A repeated call is safe, a version bump is picked up on the next start, and no separate install or upgrade hook is needed. What the owning gear owes is the ordering — it calls once the Settings Service is reachable, which it guarantees by naming `settings-service` in its `deps` — and its own failure posture when the call fails. What this service owes is that the reconcile is idempotent and returns a typed error.

Three rules keep an unattended reconcile from doing damage. A changed value type at the same major is not a metadata edit and is refused: accepting it would leave stored values under a type they were never validated against, and the contribution is telling us a new major rather than an edit. A higher major is an upgrade migration: the predecessor and its values are retained, every value is copied to the successor and re-validated, and what does not validate is flagged for review rather than coerced. And a module may not retype on revive, because the reconcile runs with nobody watching.

The key is the module's own: `gts.cf.core.settings.setting_type.v1~<vendor>.<package>.<category>.<name>.vN~`, derived from the abstract base this gear registers at init ([ADR-002](../ADR/ADR-002-setting-key-gts-type-id.md)). Its third segment is the category, which is therefore the gear's property and changes only with a new id.

**Requirements**: `cpt-cf-settings-service-fr-module-contributed-declarations`, `cpt-cf-settings-service-fr-contributed-lifecycle`

**Principles**: `cpt-cf-settings-service-principle-declaration-value-split`, `cpt-cf-settings-service-principle-consume-gts`, `cpt-cf-settings-service-principle-fail-closed`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-settings-service-actor-contributing-module` | Registers and retires its own declarations from its init, supplying the derived half of each key, the value type, the Schema Default and the metadata |
| `cpt-cf-settings-service-actor-types-registry` | Holds the abstract `setting_type` base and receives each contributed setting's composed type before its row is inserted; owns the curated value types the declarations name |
| `cpt-cf-settings-service-actor-platform-admin` | Changes the values of contributed settings and may not edit or retire their declarations |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — §5.8 Module-Contributed Settings; §5.2 (type-versioning policy)
- **Design**: [DESIGN.md](../DESIGN.md) — §4.2 (Component: Module Contribution Reconciler, including *Upgrade migration* and the worked example; Component: Declaration Management — *Declaration mutation classes*), §4.4 (Events Emitted), §4.5 (`SettingsContributionClient` trait), §4.7 (GTS Type & Schema Identifiers — *When the type is registered*, *Retiring a declaration does not unregister its type*; Compatibility mode for value types), §4.8 (Trusted-Caller Boundary), §4.9 (Gear init)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.10
- **Dependencies**: entry 2.4 for the Type Validator that checks every default and every copied value, and for the `setting_values` table an upgrade copies rows into; entry 2.3 for the declaration entity, its classification derivation and the administrative immutability of contributed rows; entry 2.2 for the category the reconciler reuses or creates; entry 2.1 for the abstract `setting_type` base registered at init and the Audit Emitter every changed row is recorded through.
- **Not applicable**: The administrative `POST /settings-service/v1/declarations` and its evolve-by-re-declaring are entry 2.3. Values of contributed settings are set through the value write path of entry 2.8, and a gear that must write a value does so as a service principal there, never through this contract. Verified module identity is R2: `owner_module` is caller-supplied and never an authorization input, and the trait is bound in-process only. Disposition of retained values on full gear removal is an open question in the design. Dependency Groups over contributed settings are R3.

## 2. Actor Flows (CDSL)

### Register Declarations on Boot

- [ ] `p1` - **ID**: `cpt-cf-settings-service-flow-module-contributions-register`

**Actor**: `cpt-cf-settings-service-actor-contributing-module`

**Success Scenarios**:
- Every declaration in the set reconciled — inserted, updated in place, upgraded to a new major, or reactivated — with a count of each outcome; a repeated call changes nothing
- A category named by a key's third segment created on first use and reused thereafter

**Error Scenarios**:
- A key that is not a well-formed derived half under the settings base, or whose category segment is missing
- A value type not present in the catalogue, a Schema Default that fails it, or a non-empty default on a secret-trait type
- A changed value type at the same major
- The types registry, the validator, or the database unavailable

**Steps**:
1. [ ] - `p1` - Actor resolves `SettingsContributionClient` from `ClientHub` in its own init — having named `settings-service` in its `deps` so this gear initialized first — and calls `register_declarations` with its `owner_module` and the full set of declarations it ships from its `post_init` hook, since the types registry admits the schemas registered during the init phase, the value-type catalogue among them, into its readable store only when it switches to ready mode after every gear's `init`; a call made from `init` finds its value types unknown - `inst-mc-reg-1`
2. [x] - `p1` - **FOR EACH** contributed declaration → invoke contributed key admission; **IF** it refuses → record the item's error and continue with the rest, since one malformed setting must not take the gear's whole set down - `inst-mc-reg-2`
3. [x] - `p1` - **FOR EACH** admitted declaration → reuse the category whose slug is the key's category segment, or create it with that slug as both key and display name, in one transaction with the declaration it is created for - `inst-mc-reg-3`
4. [x] - `p1` - **FOR EACH** admitted declaration → invoke reconcile one declaration, matched by its version-stripped path - `inst-mc-reg-4`
5. [x] - `p1` - Publish `event_declaration_registered` for every inserted or upgraded declaration, `event_declaration_reactivated` for every revived one, and `event_declaration_updated` for every in-place metadata change, and write one audit record per **changed** row — carrying no tenant, since a declaration sits at no scope, so the write asks the Tenant Resolver for nothing; a boot that converges writes neither - `inst-mc-reg-5`
6. [x] - `p1` - **RETURN** the `ReconcileResult` with the counts of registered, updated, retired and reactivated declarations and the per-item errors; a set that changed nothing returns all zeros and no error - `inst-mc-reg-6`

### Retire Declarations

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-module-contributions-retire`

**Actor**: `cpt-cf-settings-service-actor-contributing-module`

**Success Scenarios**:
- The named declarations marked retired, their values retained but excluded from resolution, their types left registered

**Error Scenarios**:
- A key the module does not own, or that does not exist
- A key that is already retired, which is a no-op rather than an error

**Steps**:
1. [x] - `p1` - Actor calls `retire_declarations` with its `owner_module` and the keys it no longer ships - `inst-mc-ret-1`
2. [x] - `p1` - **FOR EACH** key → DB: SELECT the declaration; **IF** none, **OR** its `owner_module` differs → record the item's error and continue - `inst-mc-ret-2`
3. [x] - `p1` - **IF** already retired → count nothing and continue - `inst-mc-ret-3`
4. [x] - `p1` - DB: UPDATE setting_declarations SET status = 'retired' in one transaction with its audit record; retain every row in `setting_values`; do not unregister the type, so a later re-registration is a lookup rather than a re-mint - `inst-mc-ret-4`
5. [x] - `p1` - Evict the local cache for the key at every scope and publish `event_declaration_retired` - `inst-mc-ret-5`
6. [x] - `p1` - **RETURN** the `ReconcileResult` with the retired count and the per-item errors; a read of a retired key now resolves as the distinct retired outcome - `inst-mc-ret-6`

## 3. Processes / Business Logic (CDSL)

### Contributed Key Admission

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-module-contributions-key`

**Input**: A contributed declaration's key

**Output**: The key's category slug, leaf name, major and version-stripped path, or the refusal

**Steps**:
1. [x] - `p1` - Parse the key through the shared setting-key parser, which requires the fixed base and one derived type; **IF** it fails → **RETURN** refused with the parser's problem - `inst-mc-key-1`
2. [x] - `p1` - **IF** the derived half's category segment is absent or not a well-formed slug → **RETURN** refused as `KeyNotNamespaced`; a contributed key is namespaced to its gear by construction and files under the category its third segment names - `inst-mc-key-2`
3. [x] - `p1` - Take the leaf name from the type token and the major from the version suffix, and form the version-stripped path — the derived half without its `.vN` — which is what "the same setting across versions" means - `inst-mc-key-3`
4. [x] - `p1` - **RETURN** the category slug, leaf name, major and stripped path - `inst-mc-key-4`

### Reconcile One Declaration

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-module-contributions-reconcile`

**Input**: An admitted contributed declaration, its category, and the declarations already stored on its version-stripped path

**Output**: The row inserted, updated, upgraded or reactivated, or the refusal

**Steps**:
1. [x] - `p1` - Resolve the value type's trait set; derive `has_secret_trait` and the classification — `secret` from the trait and never from the caller, otherwise the caller's `pii` or `public`; **IF** the caller supplied `secret` on a non-secret type, or a non-empty default on a secret-trait type → **RETURN** refused - `inst-mc-rec-1`
2. [x] - `p1` - Validate the Schema Default against `value_type_id` through the Type Validator; **IF** it fails → **RETURN** refused with field-level detail, since a declaration must have a valid default - `inst-mc-rec-2`
3. [x] - `p1` - **IF** no declaration exists on the stripped path → invoke setting type registration, then DB: INSERT the row with `source = module_contributed`, the `owner_module`, `status = active`, the classification, `requires_step_up` as supplied or `true`, and `anonymous_exposable` as supplied or `false`, refusing the latter on `secret` or `pii`; **RETURN** registered - `inst-mc-rec-3`
4. [x] - `p1` - **IF** a declaration exists at the same major **AND** its `value_type_id` differs → **RETURN** refused as `ValueTypeChanged`, whatever its status: a retype is a new major, and this path runs with nobody watching - `inst-mc-rec-4`
5. [x] - `p1` - **IF** a declaration exists at the same major **AND** is active → DB: UPDATE its descriptive metadata and classification in place, preserving every administrator-set value; **IF** the classification changed → re-sync the denormalized copy on the setting's value rows in the same transaction; **RETURN** updated - `inst-mc-rec-5`
6. [x] - `p1` - **IF** a declaration exists at the same major **AND** is retired → DB: UPDATE status = 'active' and its metadata, re-validate every retained value against the type and flag what fails with `needs_review` and its detail, evict the cache, and **RETURN** reactivated - `inst-mc-rec-6`
7. [x] - `p1` - **IF** the contributed major is higher than the highest stored → invoke the upgrade migration and **RETURN** registered - `inst-mc-rec-7`
8. [x] - `p1` - **IF** the contributed major is lower than the active one → **RETURN** refused; a gear does not roll a setting back by re-registering an older major - `inst-mc-rec-8`

### Upgrade Migration to a New Major

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-module-contributions-upgrade`

**Input**: The active predecessor on the stripped path and the contributed successor at a higher major

**Output**: The successor active with the predecessor's values carried over, the predecessor retired

**Steps**:
1. [x] - `p1` - DB: UPDATE the predecessor SET status = 'retired', its values retained, so exactly one major on the path is active — and first, because the two majors share a leaf name in one category and `uq_declaration_category_slug` admits one active row for that pair; the whole migration is one transaction, so nothing outside it observes the moment the path has no active major - `inst-mc-up-1`
2. [x] - `p1` - Invoke setting type registration for the successor's key, then DB: INSERT the successor row, its default validated against its own value type - `inst-mc-up-2`
3. [x] - `p1` - **FOR EACH** value row of the predecessor → copy it to the successor at the same scope; validate the copy against the successor's value type; **IF** it fails → insert it flagged `needs_review` with the validator's detail, excluded from resolution until an administrator corrects it, never coerced - `inst-mc-up-3`
4. [x] - `p1` - Commit the successor, the copies and the retirement in one transaction with the audit records, so a failure leaves the predecessor active and untouched - `inst-mc-up-4`
5. [x] - `p1` - Evict the cache for both keys at every scope; succession stays derivable from the keys alone — the same stripped path, the highest major below — and no pointer is stored - `inst-mc-up-5`
6. [x] - `p1` - **RETURN** with the successor active; readers of the old key receive the distinct retired outcome and drop the dependency - `inst-mc-up-6`

### Setting Type Registration

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-module-contributions-type`

**Input**: A setting key and the `value_type_id` its values validate against

**Output**: The setting's own type registered in the types registry, derived from the base

**Steps**:
1. [x] - `p1` - Compose the setting's type schema: identified by the key, deriving from the abstract `gts.cf.core.settings.setting_type.v1~` base this gear registered at init, and composed with the value type the declaration names — carrying **no** `default`, since the Schema Default lives in `default_value` alone - `inst-mc-type-1`
2. [x] - `p1` - Call the types registry's type-schema registration before the row is inserted; **IF** the registry reports the base absent → **RETURN** unavailable, since the base is registered at this gear's init and its absence means the gear is not the one that started - `inst-mc-type-2`
3. [x] - `p1` - **IF** the type is already registered → treat it as success: registration is idempotent, so a retry after a failed insert reuses the type rather than minting a second one, and a type with no declaration resolves but names nothing - `inst-mc-type-3`
4. [x] - `p1` - **RETURN** registered; a later retirement of the declaration leaves the type in place - `inst-mc-type-4`

## 4. States (CDSL)

Not applicable. The declaration lifecycle — active, retired, reactivated — is the state machine of entry 2.3, which this feature drives through its own paths; a contributed declaration has no additional state.

## 5. Definitions of Done

### Contribution Contract in the SDK

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-module-contributions-sdk`

The SDK **MUST** carry a `ContributedDeclaration` that names the full setting key, the `value_type_id`, the Schema Default, the scope class, and the optional `mode`, `description`, `domain_affinity`, `licence_feature`, `data_classification` (`public` or `pii` only), `requires_step_up` and `anonymous_exposable`; a `SettingKey` constructor composing a contributed key from vendor, package, category, name and major; and a `ReconcileResult` counting registered, updated, retired and reactivated declarations and carrying per-item errors. `SettingsContributionClient` **MUST** expose `register_declarations` and `retire_declarations`.

**Implements**:
- `cpt-cf-settings-service-flow-module-contributions-register`
- `cpt-cf-settings-service-flow-module-contributions-retire`

**Constraints**: `cpt-cf-settings-service-constraint-key-is-gts-type-id`

**Touches**:
- Entities: `ContributedDeclaration`, `ReconcileResult`, `SettingKey`

### Reconciler Operations

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-module-contributions-operations`

The system **MUST** implement `SettingsContributionClient` in process and register it into `ClientHub` at init, **MUST** make `register_declarations` idempotent so a repeated call converges the gear's set and changes nothing, **MUST** continue past a refused item and report it per item, **MUST** publish the registered, updated, retired and reactivated events, and **MUST** write one audit record per changed row — with **no tenant**, a declaration having no scope to be at, so that the reconcile resolves nothing through the Tenant Resolver and cannot be stopped by its absence while the contributing gear is still starting. It **MUST NOT** write an audit record for a contribution, and therefore **MUST NOT** resolve the root tenant on this path: a declaration is platform-wide and has no scope, so a record could only be written against a borrowed one, and borrowing it means asking the Tenant Resolver while the contributing gear is still starting.

**Implements**:
- `cpt-cf-settings-service-flow-module-contributions-register`
- `cpt-cf-settings-service-flow-module-contributions-retire`

**Constraints**: `cpt-cf-settings-service-constraint-audit-and-events`

**Touches**:
- Entities: `SettingsContributionClient`, `ReconcileResult`

### Namespaced Keys and Auto-Vivified Categories

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-module-contributions-key`

A contributed key **MUST** parse through the shared setting-key parser as the fixed base followed by one derived type, **MUST** be refused as `KeyNotNamespaced` when its category segment is absent or malformed, and **MUST** file under the category its third segment names, which the reconciler **MUST** reuse by slug or create with that slug as both key and name.

**Implements**:
- `cpt-cf-settings-service-algo-module-contributions-key`

**Constraints**: `cpt-cf-settings-service-constraint-key-is-gts-type-id`

**Touches**:
- DB Table: `categories`, `setting_declarations`
- Entities: `SettingKey`, `Category`

### Reconcile Cases

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-module-contributions-reconcile`

Matched by version-stripped path, the reconciler **MUST** insert a new setting with `source = module_contributed`, **MUST** update descriptive metadata and classification in place at the same major while preserving administrator-set values and re-syncing the denormalized classification, **MUST** refuse a changed `value_type_id` at the same major as `ValueTypeChanged` whatever the declaration's status, **MUST** reactivate a retired declaration at the same major with its retained values re-validated, and **MUST** refuse a lower major than the active one. `secret` **MUST** be derived from the value type's trait and never accepted from the caller, and every default **MUST** be validated before a row is written.

**Implements**:
- `cpt-cf-settings-service-algo-module-contributions-reconcile`

**Constraints**: `cpt-cf-settings-service-constraint-gts-value-validation`

**Touches**:
- DB Table: `setting_declarations`, `setting_values`
- Entities: `SettingDeclaration`

### Upgrade Migration

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-module-contributions-upgrade`

A higher major on the same path **MUST** insert the successor, copy every predecessor value to it and re-validate each copy against the successor's value type, flag a failing copy `needs_review` with its detail rather than coerce it, retire the predecessor so exactly one major is active, and commit all of it in one transaction. Succession **MUST** be derived from the keys and never stored.

**Implements**:
- `cpt-cf-settings-service-algo-module-contributions-upgrade`

**Touches**:
- DB Table: `setting_declarations`, `setting_values`
- Entities: `SettingDeclaration`, `SettingValue`

### Type Registered Before the Row

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-module-contributions-type`

Before a contributed declaration's row is inserted, its own type **MUST** be registered in the types registry, derived from the abstract `setting_type` base and composed with its value type, carrying no `default`. Registration **MUST** be idempotent, an absent base **MUST** surface as unavailable, and retiring the declaration **MUST NOT** unregister the type.

**Implements**:
- `cpt-cf-settings-service-algo-module-contributions-type`

**Constraints**: `cpt-cf-settings-service-constraint-key-is-gts-type-id`

**Touches**:
- Entities: types registry client

### Contributed Declarations Are Immutable to Administrators

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-module-contributions-immutable`

A `module_contributed` declaration **MUST** be refused `409 ContributedDeclarationImmutable` on the administrative update and retire paths, while its values **MUST** remain settable through the value write path under the ordinary rules.

**Implements**:
- `cpt-cf-settings-service-flow-module-contributions-register`

**Touches**:
- Entities: `SettingDeclaration`

### In-Process Only

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-module-contributions-trust`

While the release is Embedded-only, the contribution trait **MUST** be bound in process, the gear **MUST** publish no REST contract for it and **MUST** fail startup when configuration asks for a remote binding, and `owner_module` **MUST** be recorded as an attribute and never used as an authorization input.

**Implements**:
- `cpt-cf-settings-service-flow-module-contributions-register`

**Constraints**: `cpt-cf-settings-service-constraint-supplied-as-gear`

**Touches**:
- Entities: `SettingsContributionClient`

## 6. Acceptance Criteria

- [x] A gear registering three declarations on a fresh database gets `registered = 3`, three active rows with `source = module_contributed`, three registered types, and its category created by slug; the same call again returns all zeros and changes nothing
- [x] A key whose derived half lacks a category segment is refused as `KeyNotNamespaced`, and the other declarations in the same call are still reconciled
- [x] A Schema Default failing its value type is refused with field-level detail and inserts no row and no type
- [x] A `secret` classification supplied by the caller on a non-secret type is refused; a secret-trait type derives `secret` and refuses a non-empty default
- [x] Re-registering a setting at the same major with a new description updates the row in place and leaves every stored value intact
- [x] Re-registering a setting at the same major with a different `value_type_id` is refused as `ValueTypeChanged`, and the stored declaration and its values are untouched
- [x] Re-registering a retired setting at the same major reactivates it, and a retained value that no longer validates is flagged `needs_review` with a detail and falls through on read
- [x] Registering `…sett1.v2~` with a different value type while `…sett1.v1~` is active with two values creates `v2` active, copies both values with the failing one flagged, retires `v1`, and leaves all of it or none of it when the transaction fails
- [x] Registering a major lower than the active one is refused
- [x] The setting's type exists in the types registry before its row, a retry after a failed insert reuses it, and retiring the declaration leaves it registered
- [x] Retiring a key the module does not own is refused per item; retiring an already retired key changes nothing
- [x] After a retire, a read of the key resolves as the distinct retired outcome and every value row remains
- [x] Every registration, upgrade, reactivation, retirement and in-place metadata update publishes its event and writes one audit record whose tenant is absent; a boot that changes nothing writes neither, and no path resolves the root tenant, so a contribution never depends on the Tenant Resolver being serviceable at gear start
- [x] `PATCH` and `DELETE` on a contributed declaration return `409 ContributedDeclarationImmutable`, while a value write to it succeeds under the ordinary rules
- [x] `SettingsContributionClient` is resolvable from `ClientHub` after init, and a configuration naming a remote binding for it fails startup
