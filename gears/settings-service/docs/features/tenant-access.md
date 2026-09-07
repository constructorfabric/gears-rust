<!-- Created: 2026-09-06 by Constructor Tech -->
<!-- Updated: 2026-09-06 by Constructor Tech -->

# Feature: Tenant Access Restrictions

- [ ] `p1` - **ID**: `cpt-cf-settings-service-featstatus-tenant-access`

- [ ] `p1` - `cpt-cf-settings-service-feature-tenant-access`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Set a Restriction](#set-a-restriction)
  - [Clear a Restriction](#clear-a-restriction)
  - [Read a Tenant's Access](#read-a-tenants-access)
  - [List Restrictions in the Subtree](#list-restrictions-in-the-subtree)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Effective Access Resolution](#effective-access-resolution)
  - [Strict-Descendant Target Check](#strict-descendant-target-check)
  - [Restriction State Tag](#restriction-state-tag)
  - [Eviction on an Access Change](#eviction-on-an-access-change)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Tenant Permissions Table](#tenant-permissions-table)
  - [Restriction Operations](#restriction-operations)
  - [Effective Access Rule](#effective-access-rule)
  - [Access Gates the Caller, Not the Value](#access-gates-the-caller-not-the-value)
  - [Standalone Tenants Are Opaque Upward](#standalone-tenants-are-opaque-upward)
  - [Optimistic Concurrency on Restrictions](#optimistic-concurrency-on-restrictions)
  - [Eviction of the Restricted Subtree](#eviction-of-the-restricted-subtree)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Lets an ancestor's administrator narrow what one descendant tenant may do with one setting — `read_only` or `hidden`, recorded sparsely, absence meaning `overridable` — and makes the resulting effective access the single answer every administrative read and write consults.

### 1.2 Purpose

What a tenant may do with a setting is not a property of the declaration. The earlier model carried a pair of booleans on it; this one carries a sparse decision per `(setting, tenant)` pair, recorded by someone above the tenant and never by the tenant itself. Two consequences follow and are easy to get backwards. Access gates the **caller**, not the value: a restricted tenant's existing override still resolves and is still inherited, and an authorized ancestor may still write at a restricted descendant. And the in-process reader is not gated at all — runtime configuration resolution is not administrative access.

Effective access is the strictest value on the root-to-self chain, `overridable < read_only < hidden`, so a descendant can never widen an ancestor's restriction. A row is stored even when a stricter ancestor already dominates it, which lets an administrator prepare a narrower exception before lifting a broader restriction without briefly opening access.

Scope class remains stronger than any row: a `global` setting has no tenant-scoped value for anyone to write. And the platform has no row of its own, because nobody is above the root to record one.

The standalone seam lives here too, because it is the same boundary drawn from the other side: inheritance flows into a standalone tenant unchanged, while nothing of the tenant's own state is visible or writable from above.

**Requirements**: `cpt-cf-settings-service-fr-tenant-scope-enforcement`, `cpt-cf-settings-service-fr-per-setting-access`, `cpt-cf-settings-service-fr-barrier-default-seam`, `cpt-cf-settings-service-nfr-scope-isolation`

**Principles**: `cpt-cf-settings-service-principle-fail-closed`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-settings-service-actor-platform-admin` | Restricts any tenant, since every tenant is a strict descendant of the root |
| `cpt-cf-settings-service-actor-tenant-admin` | Restricts strict descendants within its own subtree, and is itself gated by the effective access recorded above it |
| `cpt-cf-settings-service-actor-tenant-resolver` | Supplies the root-to-self chain effective access is computed over, the subtree the target check runs against, and the standalone marking |
| `cpt-cf-settings-service-actor-authz-resolver` | Decides `delegate` on the setting's key, distinct from `write`, because changing a value and restricting its administrator are different powers |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — §5.6 Multi-Tenant Overrides & Cascading Inheritance (Tenant Access Enforcement, Hierarchy Barriers), §5.7 (Per-Setting Access)
- **Design**: [DESIGN.md](../DESIGN.md) — §4.1 (Entity `TenantAccessRestriction`, Enum `TenantAccess`), §4.2 (Component: Tenant Access; Component: Value Resolver — *Tenant access uses the same ancestor lookup*, *Standalone tenants*), §4.3 (REST API — Setting Values, Permission Rules), §4.7 (Table `tenant_permissions`), §4.8 (Authorization Model)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.7
- **Dependencies**: entry 2.5, whose read path consumes effective access and whose cache is evicted on an access change; entry 2.3 for the declaration a restriction is about; entry 2.6, through which restriction changes are audited; entry 2.1 for the PEP, the precondition helper and persistence.
- **Not applicable**: Per-setting authorization grants themselves — the platform's policy manager holds them; this feature passes the setting key as the resource and enforces the decision. The write path's use of the caller's access is specified with the Value Writer in entry 2.8. Licence gating is R2. Cleanup of a deleted tenant's rows waits on the tenant-deleted signal (R2).

## 2. Actor Flows (CDSL)

### Set a Restriction

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-tenant-access-set`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- A `read_only` or `hidden` row stored for a strict descendant, effective at once unless a stricter ancestor already dominates it, in which case it is stored and waits

**Error Scenarios**:
- The caller lacks `delegate` on the setting
- The target is the caller itself, an ancestor, a sibling, outside the subtree, or a standalone descendant
- The requested access is `overridable`, which is expressed by clearing, not by storing
- The setting does not exist or is hidden from the caller, reported as absent
- `If-Match` absent or stale

**Steps**:
1. [x] - `p1` - Actor sends PUT /settings-service/v1/settings/{key}/permissions?tenant={tenant_id} with `If-Match` and a body carrying `access` - `inst-ta-set-1`
2. [x] - `p1` - Authorize `delegate` on the setting's key through the `PolicyEnforcer` PEP - `inst-ta-set-2`
3. [x] - `p1` - **IF** the decision is deny or cannot be obtained → **RETURN** `403` - `inst-ta-set-3`
4. [x] - `p1` - Invoke the strict-descendant target check for the caller and `tenant`; **IF** it fails → **RETURN** `403`, since a caller that could restrict itself could lift the restriction again - `inst-ta-set-4`
5. [x] - `p1` - **IF** `access` is not `read_only` or `hidden` → **RETURN** `400`, because `overridable` is the absence of a row and is expressed by DELETE - `inst-ta-set-5`
6. [x] - `p1` - DB: SELECT the declaration by key; **IF** none, **OR** the caller's own effective access for it is `hidden` → **RETURN** `404`, so a caller cannot restrict what it cannot see and cannot learn that it exists - `inst-ta-set-6`
7. [x] - `p1` - DB: SELECT the row for `(declaration_id, tenant_id)`, and compute the restriction state tag for the stored row or for the absent state - `inst-ta-set-7`
8. [x] - `p1` - Evaluate the `If-Match` precondition against that tag; **IF** absent → **RETURN** `428`; **IF** stale → **RETURN** `412` - `inst-ta-set-8`
9. [x] - `p1` - DB: UPSERT tenant_permissions on `uq_tenant_permission` with `access`, `set_by` from the authenticated principal, and `updated_at` now, in one transaction with the audit record and with the comparison above, so concurrent delegates cannot silently overwrite each other - `inst-ta-set-9`
10. [x] - `p1` - Record the row even when an ancestor already imposes a stricter access; it takes effect when that restriction is lifted - `inst-ta-set-10`
11. [x] - `p1` - Emit an audit record for the restriction change with pre-image and post-image - `inst-ta-set-11`
12. [x] - `p1` - Invoke eviction for the target tenant and every descendant, independent of the setting's scope class - `inst-ta-set-12`
13. [x] - `p1` - **RETURN** `200` with the stored restriction, the tenant's resulting effective access and the tenant that supplies it, and the refreshed ETag - `inst-ta-set-13`

### Clear a Restriction

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-tenant-access-clear`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- The pair's own row removed, making it `overridable` at this level while an ancestor restriction may still determine the effective result

**Error Scenarios**:
- The caller lacks `delegate`, or the target is not a strict descendant
- `If-Match` absent or stale

**Steps**:
1. [x] - `p1` - Actor sends DELETE /settings-service/v1/settings/{key}/permissions?tenant={tenant_id} with `If-Match` - `inst-ta-clear-1`
2. [x] - `p1` - Authorize `delegate` on the setting's key; **IF** deny or cannot be obtained → **RETURN** `403` - `inst-ta-clear-2`
3. [x] - `p1` - Invoke the strict-descendant target check; **IF** it fails → **RETURN** `403` - `inst-ta-clear-3`
4. [x] - `p1` - DB: SELECT the declaration by key; **IF** none, **OR** hidden from the caller → **RETURN** `404` - `inst-ta-clear-4`
5. [x] - `p1` - DB: SELECT the row for the pair and compute the restriction state tag; evaluate `If-Match`; **IF** absent → **RETURN** `428`; **IF** stale → **RETURN** `412` - `inst-ta-clear-5`
6. [x] - `p1` - DB: DELETE the row in one transaction with the audit record; clearing an already absent row is a no-op that still requires the absent-state tag - `inst-ta-clear-6`
7. [x] - `p1` - Emit an audit record carrying the removed row as pre-image - `inst-ta-clear-7`
8. [x] - `p1` - Invoke eviction for the target tenant and every descendant - `inst-ta-clear-8`
9. [x] - `p1` - **RETURN** `200` with the tenant's resulting effective access, which an ancestor row may still narrow, and the absent-state ETag - `inst-ta-clear-9`

### Read a Tenant's Access

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-tenant-access-read`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- The pair's stored row if any, the effective access, the tenant whose row supplied it, and the ETag a mutation must present — an absent-state ETag when no row exists

**Error Scenarios**:
- The target is outside the caller's subtree
- The setting does not exist or is hidden from the caller

**Steps**:
1. [x] - `p1` - Actor sends GET /settings-service/v1/settings/{key}/permissions?tenant={tenant_id} - `inst-ta-read-1`
2. [x] - `p1` - Authorize `read` on the setting's key; **IF** deny or cannot be obtained → **RETURN** `403` - `inst-ta-read-2`
3. [x] - `p1` - Confirm the target is the caller's own tenant or a descendant that is not standalone; **IF** not → **RETURN** `403` - `inst-ta-read-3`
4. [x] - `p1` - DB: SELECT the declaration by key; **IF** none, **OR** hidden from the caller → **RETURN** `404` - `inst-ta-read-4`
5. [x] - `p1` - Invoke effective access resolution for the setting over the target's root-to-self chain - `inst-ta-read-5`
6. [x] - `p1` - Compute the restriction state tag for the pair's stored row or for the absent state - `inst-ta-read-6`
7. [x] - `p1` - **RETURN** `200` with the stored row or `overridable` for absence, the effective access, the supplying tenant, and the tag in `ETag` - `inst-ta-read-7`

### List Restrictions in the Subtree

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-tenant-access-list`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- Every stored restriction for the setting inside the caller's subtree, each with its ETag

**Error Scenarios**:
- The setting does not exist or is hidden from the caller

**Steps**:
1. [x] - `p1` - Actor sends GET /settings-service/v1/settings/{key}/permissions with a pagination cursor - `inst-ta-list-1`
2. [x] - `p1` - Authorize `read` on the setting's key; **IF** deny or cannot be obtained → **RETURN** `403` - `inst-ta-list-2`
3. [x] - `p1` - DB: SELECT the declaration by key; **IF** none, **OR** hidden from the caller → **RETURN** `404` - `inst-ta-list-3`
4. [x] - `p1` - DB: SELECT tenant_permissions for the declaration where the tenant lies inside the caller's subtree, as the `AccessScope` constrains it, excluding standalone descendants, through `idx_tenant_permission_tenant` - `inst-ta-list-4`
5. [x] - `p1` - **RETURN** `200` with the page of rows, each carrying its tag - `inst-ta-list-5`

## 3. Processes / Business Logic (CDSL)

### Effective Access Resolution

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-tenant-access-resolve`

**Input**: A declaration and a tenant

**Output**: The tenant's effective access for the setting, and the tenant whose row supplies it

**Steps**:
1. [x] - `p1` - Obtain the tenant's ancestor ids from the tenant resolver, root to self — the same lookup the cascading walk uses, and the same one for `local` and `global` settings, which do not walk ancestors for their value but do for their access - `inst-ta-resolve-1`
2. [x] - `p1` - DB: SELECT tenant_permissions WHERE declaration_id = {declaration} AND tenant_id IN ({chain}) as one exact-match set query - `inst-ta-resolve-2`
3. [x] - `p1` - **IF** no row → **RETURN** `overridable` with no supplying tenant, and create nothing: absence is the default, not a state to materialize - `inst-ta-resolve-3`
4. [x] - `p1` - Take the strictest access on the chain, `hidden` over `read_only`, and the tenant of the row that supplied it - `inst-ta-resolve-4`
5. [x] - `p1` - **RETURN** the effective access and its source; resolution never uses access to choose a value row - `inst-ta-resolve-5`

### Strict-Descendant Target Check

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-tenant-access-target`

**Input**: The caller's tenant and a target tenant

**Output**: Whether the caller may restrict the target

**Steps**:
1. [x] - `p1` - **IF** the target is the caller's own tenant → **RETURN** refused; a tenant cannot change its own row - `inst-ta-target-1`
2. [x] - `p1` - Ask the tenant resolver whether the caller is an ancestor of the target; **IF** not → **RETURN** refused, covering ancestors, siblings and tenants outside the subtree alike - `inst-ta-target-2`
3. [x] - `p1` - **IF** the target is a standalone tenant, or lies below one, as the tenant resolver marks it → **RETURN** refused; nothing traverses downward into a standalone tenant from an administrator above it - `inst-ta-target-3`
4. [x] - `p1` - **RETURN** permitted - `inst-ta-target-4`

### Restriction State Tag

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-tenant-access-etag`

**Input**: The stored row for a `(setting, tenant)` pair, or its absence

**Output**: The ETag a mutation of that pair must present

**Steps**:
1. [x] - `p1` - **IF** a row exists → derive the tag from its normalized UTC `updated_at`, as every other ETag in this service is derived - `inst-ta-etag-1`
2. [x] - `p1` - **ELSE** derive an absent-state tag that is stable for the pair and distinct from every stored-row tag, so a PUT may create a row only against the caller's knowledge that none existed - `inst-ta-etag-2`
3. [x] - `p1` - **RETURN** the tag; comparison and mutation happen in one transaction, so a row changed in between fails the comparison rather than being overwritten - `inst-ta-etag-3`

### Eviction on an Access Change

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-tenant-access-evict`

**Input**: A declaration and the tenant whose row changed

**Output**: Evicted cache entries

**Steps**:
1. [x] - `p1` - Obtain the tenant's descendants from the tenant resolver - `inst-ta-evict-1`
2. [x] - `p1` - Evict the cached entries of the setting for the tenant and every descendant on this instance, regardless of scope class, because their effective access may have changed even where their effective value has not - `inst-ta-evict-2`
3. [x] - `p1` - **RETURN** having evicted locally only; peer replicas converge through the R2 `cache_invalidate` broadcast - `inst-ta-evict-3`

## 4. States (CDSL)

Not applicable. A restriction is a row that exists with one of two values or does not exist; it is upserted and deleted rather than transitioned, and effective access is computed on every read rather than stored.

## 5. Definitions of Done

### Tenant Permissions Table

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-tenant-access-table`

The system **MUST** persist restrictions in a `tenant_permissions` table carrying `declaration_id` as a foreign key to `setting_declarations` declared `ON DELETE CASCADE`, a non-null `tenant_id` that is the restricted tenant, `access` checked to `read_only` or `hidden`, `set_by`, and timestamps, with `uq_tenant_permission` on `(declaration_id, tenant_id)` and `idx_tenant_permission_tenant`. `overridable` **MUST** be represented by no row, and the root tenant **MUST** never have one. Rows **MUST** survive a declaration's soft-retire and be deleted only by a hard delete of the declaration or a tenant's deletion.

**Implements**:
- `cpt-cf-settings-service-flow-tenant-access-set`
- `cpt-cf-settings-service-flow-tenant-access-clear`

**Constraints**: `cpt-cf-settings-service-constraint-postgres-primary-storage`, `cpt-cf-settings-service-constraint-scope-hierarchy-paths`

**Touches**:
- DB Table: `tenant_permissions`
- Entities: `TenantAccessRestriction`, `TenantAccess`

### Restriction Operations

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-tenant-access-operations`

The system **MUST** expose set, clear, read and list over restrictions, authorized by `delegate` for mutations and `read` for reads on the setting's key, **MUST** accept a mutation only for a strict descendant of the caller that is not standalone, **MUST** refuse `overridable` as a stored value, and **MUST** report a setting the caller cannot see as absent. The read **MUST** return the effective access and the tenant that supplies it.

**Implements**:
- `cpt-cf-settings-service-flow-tenant-access-set`
- `cpt-cf-settings-service-flow-tenant-access-clear`
- `cpt-cf-settings-service-flow-tenant-access-read`
- `cpt-cf-settings-service-flow-tenant-access-list`
- `cpt-cf-settings-service-algo-tenant-access-target`

**Constraints**: `cpt-cf-settings-service-constraint-rbac-policy-enforcer`

**Touches**:
- API: `PUT /settings-service/v1/settings/{key}/permissions`
- API: `DELETE /settings-service/v1/settings/{key}/permissions`
- API: `GET /settings-service/v1/settings/{key}/permissions`
- Entities: `TenantAccessRestriction`

### Effective Access Rule

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-tenant-access-effective`

Effective access **MUST** be the strictest row on the tenant's root-to-self chain, `overridable < read_only < hidden`, obtained from the tenant resolver's ancestry and one exact-match set query, with absence meaning `overridable` and creating no row. A row **MUST** be stored even when a stricter ancestor already dominates it.

**Implements**:
- `cpt-cf-settings-service-algo-tenant-access-resolve`

**Touches**:
- DB Table: `tenant_permissions`
- Entities: `TenantAccess`

### Access Gates the Caller, Not the Value

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-tenant-access-consumption`

Every administrative read path **MUST** report a `hidden` setting as absent, and a `read_only` or `hidden` tenant **MUST** be refused as a writer, while its existing override **MUST** keep resolving and being inherited. The check on a write **MUST** use the **caller's** effective access, never the target's, so an `overridable` ancestor may manage a restricted descendant. The in-process reader **MUST NOT** be gated by tenant access.

**Implements**:
- `cpt-cf-settings-service-algo-tenant-access-resolve`

**Touches**:
- Entities: `TenantAccess`, `EffectiveValue`

### Standalone Tenants Are Opaque Upward

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-tenant-access-standalone`

A tenant the tenant resolver marks standalone **MUST** remain an ordinary inheritance target, and every administrative surface above it — single and bulk read, history, listing, restriction operations, and the impact report's list and count — **MUST** treat it and its descendants as outside the caller's subtree, so nothing of its own state is read or written from above.

**Implements**:
- `cpt-cf-settings-service-algo-tenant-access-target`

**Touches**:
- Entities: `TenantAccess`

### Optimistic Concurrency on Restrictions

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-tenant-access-concurrency`

Set and clear **MUST** require `If-Match` against the stored row's tag or the absent-state tag the read returns — `428` when missing, `412` when stale — and the comparison and the mutation **MUST** happen in one transaction.

**Implements**:
- `cpt-cf-settings-service-algo-tenant-access-etag`

**Constraints**: `cpt-cf-settings-service-constraint-optimistic-concurrency`

**Touches**:
- API: `PUT /settings-service/v1/settings/{key}/permissions`
- API: `DELETE /settings-service/v1/settings/{key}/permissions`

### Eviction of the Restricted Subtree

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-tenant-access-eviction`

An access change **MUST** evict the setting's cached entries for the target tenant and every descendant on the local instance, independent of scope class.

**Implements**:
- `cpt-cf-settings-service-algo-tenant-access-evict`

**Touches**:
- Entities: cache entry

## 6. Acceptance Criteria

- [x] A freshly declared setting has `overridable` effective access for every tenant, and reading it creates no row
- [x] An ancestor with `hidden` and a descendant with `read_only` yield `hidden` for the descendant, and siblings outside the restricted branch are unaffected
- [x] A tenant administrator targeting its own tenant receives `403` and no row is written
- [x] Targeting an ancestor, a sibling, a tenant outside the subtree, or a standalone descendant receives `403`
- [ ] A row set below an already `hidden` tenant is stored and becomes effective when the ancestor restriction is cleared
- [x] A value set before its tenant became `read_only` still resolves and is inherited, and a write by that tenant is refused
- [x] An `overridable` ancestor writes at a `read_only` descendant successfully, while the descendant's own write remains refused
- [x] A gear reading through `SettingsReaderClient` receives the effective value of a `hidden` tenant unchanged
- [x] `PUT` with `access=overridable` returns `400`
- [x] `PUT` or `DELETE` without `If-Match` returns `428`; with a tag made stale by another delegate returns `412`, and the newer restriction remains stored
- [x] `GET` for a pair with no row returns `overridable` and an absent-state ETag that a subsequent `PUT` can present
- [ ] `DELETE` clears one row, and the effective access afterwards reflects any ancestor row that remains
- [x] Setting or clearing a restriction evicts the cached entries of the target tenant and its descendants
- [x] Restriction rows survive a retire and revive of their declaration unchanged
- [x] Every restriction change leaves an audit record with the previous and the new row
- [x] The root tenant never holds a restriction row
