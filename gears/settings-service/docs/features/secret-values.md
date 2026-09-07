<!-- Created: 2026-09-07 by Constructor Tech -->
<!-- Updated: 2026-09-07 by Constructor Tech -->

# Feature: Secret Values

- [ ] `p1` - **ID**: `cpt-cf-settings-service-featstatus-secret-values`

- [ ] `p1` - `cpt-cf-settings-service-feature-secret-values`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Set a Secret Value](#set-a-secret-value)
  - [Resolve a Secret on the Machine Path](#resolve-a-secret-on-the-machine-path)
  - [Remove a Secret Value](#remove-a-secret-value)
  - [Read a Secret Administratively](#read-a-secret-administratively)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Secret Reference and Store Principal](#secret-reference-and-store-principal)
  - [Secret Handle Encoding](#secret-handle-encoding)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Secret Manager over the Credential Store](#secret-manager-over-the-credential-store)
  - [Reference Only, Never Plaintext](#reference-only-never-plaintext)
  - [The Machine Path Is the Only Plaintext Path](#the-machine-path-is-the-only-plaintext-path)
  - [No Human Reveal](#no-human-reveal)
  - [Removal Releases the Entry](#removal-releases-the-entry)
  - [The Placeholder Is Not a Credential](#the-placeholder-is-not-a-credential)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Holds `secret`-trait values in the platform Credential Store and nowhere else. The settings row carries an opaque `secret_ref`, every administrative surface shows a mask, and the one path to plaintext is the SDK reader's `resolve_secret`, taken by a consuming gear, authorized against that specific setting and audited on every resolution. A lost secret is set again, never revealed.

### 1.2 Purpose

An administrator who can set a credential must not be able to read it back. Reading it back is what turns every settings administrator into a holder of every upstream credential, and what turns a settings database dump into a credential dump. So the plaintext takes one path in and one path out: in through the Value Writer, which hands it to the Secret Manager and keeps only the reference the store returns; out through the reader that gears already use for configuration, where the caller is a service resolving the setting it needs, checked per setting and recorded per resolution.

The Credential Store is the `credstore` gear. Each entry lives in the tenant the value belongs to, under a reference that names the setting and the tenant and is unique to the write that created it, and is owned by a principal that exists only for this gear. That ownership is what makes "nowhere else" true beyond this service's own tables: a tenant's users hold no path to the entry through the store's own API either. The store cannot join the row's transaction, so a write stores first, under its own reference, and commits the reference with the row; a write refused after that releases the entry it created, and a write that lands releases the entry the row held before. The live entry is never written over.

What the reader hands out for a secret is a handle, not a reference. The handle names the setting and the requested scope and nothing more, so a consumer cannot bypass the reader by taking the reference to the store itself, and the value is resolved again when the handle is used, so a credential changed after the handle was issued is the one that resolves.

A `secret` declaration's default is a placeholder, an empty value of the type, enforced where declarations are made. Reverting a scope returns it to the placeholder, that is to *not configured*: the reader then answers that no credential is configured, and never hands a placeholder to a backend as if it were one.

**Requirements**: `cpt-cf-settings-service-fr-typed-value-validation`, `cpt-cf-settings-service-fr-audit-mutations`

**Principles**: `cpt-cf-settings-service-principle-machine-only-secrets`, `cpt-cf-settings-service-principle-fail-closed`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-settings-service-actor-platform-admin` | Sets and removes secret values at any scope; sees every one of them masked |
| `cpt-cf-settings-service-actor-tenant-admin` | Sets and removes secret values within its subtree under the same write gate as any value; sees them masked |
| `cpt-cf-settings-service-actor-internal-caller` | The consuming gear that resolves a secret to plaintext through the SDK reader, per setting, and is recorded doing so |
| `cpt-cf-settings-service-actor-authz-resolver` | Decides `read` on the value resource naming the declaration for the machine caller, and `read`/`write`/`delete` on the store's secret type for this gear's principal |
| `cpt-cf-settings-service-actor-compliance-reviewer` | Reads `secret_use` records in a setting's history, with the value masked |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — §5.7 Security, Secrets & Audit; *GTS Type Validation, Trait Discovery & Secret Protection*
- **Design**: [DESIGN.md](../DESIGN.md) — §1.3 (*Secrets Never Take a Human Path*), §4.1 (`secret_ref` on `SettingValue`, `SecretHandle`, `DataClassification`), §4.2 (Component: Secret Manager; Component: Value Writer, *Set atomicity model*), §4.5 (`SettingsReaderClient::resolve_secret`), §4.8 (*No `reveal` action*), §4.7 (Table `setting_values`), §6 (*Verified caller identity for the SDK traits*)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.9
- **Dependencies**: entry 2.8, whose commit hands a secret's plaintext to the Secret Manager port and persists the reference; entry 2.5, whose resolver finds the winning row and whose cache holds references only; entry 2.6, which records the write and the resolution; entry 2.3 and entry 2.10, which refuse a non-placeholder default on a secret declaration; the `credstore` gear as the Credential Store.
- **Not applicable**: Verified machine caller identity is R2 (DESIGN §6): in process the caller is whoever holds the hub, so the per-setting check and the `secret_use` attribution use the `SecurityContext` the caller presents. Envelope encryption in the persistence layer is an open alternative, not built. No reveal endpoint, permission, event or metric exists, by design. Rotation and expiry of entries are the store's concerns, not modelled here.

## 2. Actor Flows (CDSL)

### Set a Secret Value

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-secret-values-set`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- The plaintext is in the Credential Store, the settings row carries only the reference, the audit record carries a mask, and the response shows a mask

**Error Scenarios**:
- Any refusal of the value write gate: authorization, tenant access, scope class, step-up, `If-Match`, type validation
- The Credential Store cannot answer, and nothing is written locally

**Steps**:
1. [x] - `p1` - Actor sends PUT /settings-service/v1/settings/{key}/value?tenant={tenant_id} with the plaintext as the value, through the Value Writer gate of entry 2.8 unchanged - `inst-sv-set-1`
2. [x] - `p1` - Validate the plaintext against the declared type as any value is validated; a secret is still a typed value - `inst-sv-set-2`
3. [x] - `p1` - Derive a reference unique to this write and the store principal for `(key, tenant)` through the reference process, before the row's transaction opens, since the store cannot join it - `inst-sv-set-3`
4. [x] - `p1` - Credential Store: create the entry under that reference, `private` to that principal in that tenant; create only, so nothing this write does touches the entry the row currently holds - `inst-sv-set-4`
5. [x] - `p1` - **IF** the store refuses or cannot answer → **RETURN** rejected `503`; no row and no record are written, and the plaintext is dropped - `inst-sv-set-5`
6. [x] - `p1` - DB: in the one transaction entry 2.8 commits, write the row with the new `secret_ref` and `value` NULL and its audit record with both images masked; **IF** the row held a reference before → carry it out as superseded - `inst-sv-set-6`
7. [x] - `p1` - **IF** the transaction does not commit, the tag being stale or the database refusing → Credential Store: release the entry created in step 4, so a refused write leaves nothing behind and the live entry is untouched; **IF** the release fails → log the orphan - `inst-sv-set-7`
8. [x] - `p1` - After the commit: Credential Store: release the superseded entry, once nothing can point at it any more; **IF** the release fails → log the orphan, the committed row being the truth - `inst-sv-set-8`
9. [x] - `p1` - **RETURN** `200` with `old_value` and `new_value` carrying the mask token and `masked` true, so the response never echoes what was sent - `inst-sv-set-9`

### Resolve a Secret on the Machine Path

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-secret-values-resolve`

**Actor**: `cpt-cf-settings-service-actor-internal-caller`

**Success Scenarios**:
- The plaintext reaches an authorized consuming gear and one `secret_use` record is stored with the value masked

**Error Scenarios**:
- The handle is malformed, reported as an invalid argument
- No declaration at the handle's key, or the declaration is retired
- The declaration is not `secret`-classified, so there is nothing to resolve
- The caller is not authorized to read that setting
- No credential is configured at any scope, reported as the value not found, which the SDK projects to `SecretNotConfigured`
- The Credential Store or the audit store cannot answer

**Steps**:
1. [x] - `p1` - Consuming gear calls `SettingsReaderClient::resolve_secret` with the handle it took from `get_effective`'s value - `inst-sv-resolve-1`
2. [x] - `p1` - Decode the handle through the handle process; **IF** malformed → **RETURN** invalid argument - `inst-sv-resolve-2`
3. [x] - `p1` - Resolve the effective value at the handle's key and scope afresh; **IF** no declaration → **RETURN** `NotFound` on the declaration; **IF** retired → **RETURN** `Retired`; **IF** the declaration is not `secret`-classified → **RETURN** invalid argument - `inst-sv-resolve-3`
4. [x] - `p1` - Authorize the caller for `read` on the value resource naming that declaration through the `PolicyEnforcer` PEP; **IF** denied or undecidable → **RETURN** `Unauthorized`, before the store is asked anything and whether or not a credential exists - `inst-sv-resolve-4`
5. [x] - `p1` - **IF** the effective value is the placeholder default, no row with a reference having won → **RETURN** `NotFound` on the value, not `Unauthorized` and never the placeholder - `inst-sv-resolve-5`
6. [x] - `p1` - Credential Store: fetch the plaintext for the winning row's reference in the winning row's tenant; **IF** the store has no entry → **RETURN** `NotFound` on the value; **IF** it cannot answer → **RETURN** `Unavailable` - `inst-sv-resolve-6`
7. [x] - `p1` - Append a `secret_use` record for the key at the requested scope's tenant, actor the caller's subject, post-image masked, in its own transaction; **IF** it cannot be written → **RETURN** `Unavailable` and hand out no plaintext - `inst-sv-resolve-7`
8. [x] - `p1` - **RETURN** the plaintext; it is neither cached nor logged, and the cache keeps holding the reference only - `inst-sv-resolve-8`

### Remove a Secret Value

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-secret-values-remove`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- The row is gone, the scope resolves to the placeholder, and the store entry it held is released

**Error Scenarios**:
- Any refusal of the value write gate
- The store cannot release the entry after the commit, which is logged and leaves an orphan, never a live row without a record

**Steps**:
1. [x] - `p1` - Actor sends DELETE /settings-service/v1/settings/{key}/value?tenant={tenant_id} or POST /settings-service/v1/settings/{key}/value/revert?tenant={tenant_id}, through the write gate and commit of entry 2.8 - `inst-sv-remove-1`
2. [x] - `p1` - In the commit, **IF** the removed row carried a reference → carry it out of the transaction as released, so the store is touched only once the row is durably gone - `inst-sv-remove-2`
3. [x] - `p1` - After the commit: Credential Store: delete the released entry; **IF** it is already absent → nothing to do; **IF** the store cannot answer → log the orphaned reference and continue, since the local state is the truth and a later set creates the entry again - `inst-sv-remove-3`

### Read a Secret Administratively

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-secret-values-admin-read`

**Actor**: `cpt-cf-settings-service-actor-platform-admin`

**Success Scenarios**:
- Every read, browse, write response and history entry shows the mask token for a `secret`-classified value, and the SDK reader shows an opaque handle

**Error Scenarios**:
- None of its own; the read surfaces' own refusals apply

**Steps**:
1. [x] - `p1` - Actor reads a setting, browses a category, receives a write response or lists history through the REST surfaces of entries 2.5, 2.6 and 2.8 - `inst-sv-aread-1`
2. [x] - `p1` - Replace a `secret`-classified payload with the mask token whatever the caller's entitlements; mask a `pii` payload unless `read_unmasked` is granted; pass a `public` payload through - `inst-sv-aread-2`
3. [x] - `p1` - In the SDK reader, hand a `secret`-classified value out as the opaque handle for the requested scope, whether a credential is configured or only the placeholder resolves, so the shape does not disclose which - `inst-sv-aread-3`

## 3. Processes / Business Logic (CDSL)

### Secret Reference and Store Principal

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-secret-values-reference`

**Input**: A setting key and the tenant the value belongs to

**Output**: The reference the entry of one write is stored under and the security context this gear presents to the store

**Steps**:
1. [x] - `p1` - Derive a name-based UUID of the key under this gear's namespace, and with the tenant the prefix `cf-settings-{key uuid}-{tenant uuid}` every entry of the pair shares, carrying the key's identity without the characters the store's alphabet forbids - `inst-sv-ref-1`
2. [x] - `p1` - Compose the reference as that prefix and a nonce unique to this write, within the store's `[a-zA-Z0-9_-]` alphabet and length, so a write refused after the store leg cannot have altered the live entry and a superseded or removed entry is released by name - `inst-sv-ref-2`
3. [x] - `p1` - Build the store context with a fixed settings-service principal as subject, no subject type, and the target tenant as subject tenant, so the entry lives in the tenant the value belongs to and only this gear's principal reads it back - `inst-sv-ref-3`
4. [x] - `p1` - **RETURN** the reference and the context - `inst-sv-ref-4`

### Secret Handle Encoding

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-secret-values-handle`

**Input**: A setting key and the scope a consumer asked for, or a handle to decode

**Output**: An opaque `SecretHandle`, or the key and scope it was issued for

**Steps**:
1. [x] - `p1` - Encode the key and scope as JSON, then base64url without padding under the version prefix `sh1.`; nothing else rides in it: no reference, no winning tenant, no credential coordinates - `inst-sv-handle-1`
2. [x] - `p1` - Decode by requiring the prefix, reversing the encoding and parsing the two fields; **IF** any step fails → **RETURN** invalid argument without echoing the token - `inst-sv-handle-2`
3. [x] - `p1` - Treat the scope as a request, not a fact: the value is resolved again when the handle is used, so a credential set or removed after the handle was issued is what resolves - `inst-sv-handle-3`

## 4. States (CDSL)

Not applicable. A secret setting has a row with a reference or has none; the entry in the store follows the row, created by the set that the row records and released when that row is removed or points elsewhere, and nothing is transitioned between.

## 5. Definitions of Done

### Secret Manager over the Credential Store

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-secret-values-manager`

The system **MUST** bind the Secret Manager port to the `credstore` gear's client, implementing `store_secret`, `resolve_plaintext` and `delete_secret`, **MUST** present a fixed settings-service principal in the target tenant with `private` sharing so that no administrative principal can read an entry back through the store, **MUST** store each write create-only under the reference of the reference process, unique to that write, and **MUST** report a store that cannot answer as unavailable rather than storing plaintext locally.

**Implements**:
- `cpt-cf-settings-service-flow-secret-values-set`
- `cpt-cf-settings-service-flow-secret-values-resolve`
- `cpt-cf-settings-service-flow-secret-values-remove`
- `cpt-cf-settings-service-algo-secret-values-reference`

**Constraints**: `cpt-cf-settings-service-constraint-secrets-by-reference`

**Touches**:
- Entities: Secret Manager port, `SecretRef`
- External: `credstore` gear, `CredStoreClientV1`

### Reference Only, Never Plaintext

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-secret-values-reference-only`

A write to a `secret`-trait declaration **MUST** hand the plaintext to the store before the row's transaction opens and persist only the reference the Secret Manager returned, in the same transaction as its audit record, **MUST** leave `value` NULL, and **MUST** be refused as unavailable when the store cannot answer. Plaintext **MUST NOT** enter `setting_values`, the effective cache, any audit record, or a write response.

**Implements**:
- `cpt-cf-settings-service-flow-secret-values-set`

**Constraints**: `cpt-cf-settings-service-constraint-secrets-by-reference`

**Touches**:
- DB Table: `setting_values`
- Entities: `SettingValue`, `secret_ref`
- API: `PUT /settings-service/v1/settings/{key}/value`

### The Machine Path Is the Only Plaintext Path

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-secret-values-machine-path`

`SettingsReaderClient::resolve_secret` **MUST** be the only operation yielding plaintext, **MUST** authorize the caller for `read` on the value resource naming that declaration before the store is asked anything, **MUST** append one `secret_use` record per resolution with the value masked and refuse the plaintext when the record cannot be written, **MUST** never cache plaintext, and **MUST** answer an unconfigured secret as `NotFound` on the value, which the SDK projects to `SecretNotConfigured`, rather than `Unauthorized` or the placeholder.

**Implements**:
- `cpt-cf-settings-service-flow-secret-values-resolve`
- `cpt-cf-settings-service-algo-secret-values-handle`

**Constraints**: `cpt-cf-settings-service-constraint-secrets-by-reference`, `cpt-cf-settings-service-constraint-rbac-policy-enforcer`

**Touches**:
- SDK: `SettingsReaderClient::resolve_secret`, `SecretHandle`
- Entities: `SecretHandle`, `AuditRecord` (`secret_use`)

### No Human Reveal

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-secret-values-no-reveal`

No REST operation **MAY** return a secret's plaintext: every administrative read, browse, write response and history entry **MUST** carry the mask token for a `secret`-classified value, `pii` **MUST** be masked unless the caller holds `read_unmasked`, and the SDK reader **MUST** hand a `secret`-classified value out as the opaque handle whether or not a credential is configured.

**Implements**:
- `cpt-cf-settings-service-flow-secret-values-admin-read`

**Constraints**: `cpt-cf-settings-service-constraint-secrets-by-reference`

**Touches**:
- API: `GET /settings-service/v1/settings/{key}`
- API: `GET /settings-service/v1/settings`
- API: `GET /settings-service/v1/settings/{key}/history`
- SDK: `SettingsReaderClient::get_effective`

### Removal Releases the Entry

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-secret-values-cleanup`

Removing, reverting or superseding a row that carries a reference **MUST** delete that store entry after the row's transaction has committed, a write whose transaction does not commit **MUST** delete the entry it created, both **MUST** treat an already absent entry as done, and both **MUST** log rather than fail when the store cannot answer, since the committed local state is the truth and the live entry is never the one released.

**Implements**:
- `cpt-cf-settings-service-flow-secret-values-remove`
- `cpt-cf-settings-service-flow-secret-values-set`

**Constraints**: `cpt-cf-settings-service-constraint-secrets-by-reference`

**Touches**:
- API: `DELETE /settings-service/v1/settings/{key}/value`
- API: `POST /settings-service/v1/settings/{key}/value/revert`
- Entities: Secret Manager port

### The Placeholder Is Not a Credential

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-secret-values-placeholder`

A `secret`-trait declaration **MUST** carry an empty placeholder default, refused otherwise where declarations are registered, and a scope with no row **MUST** resolve to that placeholder on every administrative surface while the machine path **MUST** answer that no credential is configured instead of handing the placeholder out as plaintext.

**Implements**:
- `cpt-cf-settings-service-flow-secret-values-resolve`

**Constraints**: `cpt-cf-settings-service-constraint-secrets-by-reference`

**Touches**:
- Entities: `SettingDeclaration` (`default_value`), `SecretHandle`

## 6. Acceptance Criteria

- [x] Setting a value on a `secret`-trait declaration stores an entry in the Credential Store, `private` to the gear's principal in the target tenant, and a row whose `value` is NULL and whose `secret_ref` names that entry under the key-and-tenant prefix; the plaintext appears nowhere in `setting_values`
- [x] The write response, the setting read, the category browse and the history all carry the mask token for that setting, and the audit record's images are masked
- [x] Setting the same secret again creates a new entry, points the row at it and releases the previous entry after the commit, leaving one row and one live entry; a set refused on `If-Match` releases the entry it created and leaves the live one untouched
- [x] With the Credential Store unavailable, a secret write is refused `503` and no row or record is written
- [x] The SDK reader returns the opaque handle as the value of a `secret`-classified setting whether or not a credential is configured, and the handle contains neither the reference nor the winning tenant
- [x] `resolve_secret` returns the plaintext to an authorized caller and stores one `secret_use` record with the value masked, whose actor is the caller's subject
- [x] `resolve_secret` for a caller denied `read` on that declaration returns `Unauthorized` and touches neither the store nor the audit store
- [x] `resolve_secret` on a secret with no credential at any scope returns `NotFound` on the value, which the SDK projects to `SecretNotConfigured`, never the placeholder
- [x] A malformed handle is refused as an invalid argument without echoing it
- [x] Removing or reverting a secret row deletes the store entry after the commit, and the scope resolves to the placeholder afterwards
- [x] No plaintext is ever present in the effective cache; the cached entry carries the reference only
- [x] A `secret`-trait declaration with a non-empty default is refused at registration
