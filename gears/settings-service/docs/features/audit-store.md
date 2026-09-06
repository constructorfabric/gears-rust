<!-- Created: 2026-09-06 by Constructor Tech -->
<!-- Updated: 2026-09-06 by Constructor Tech -->

# Feature: Audit Store and History

- [ ] `p1` - **ID**: `cpt-cf-settings-service-featstatus-audit-store`

- [ ] `p1` - `cpt-cf-settings-service-feature-audit-store`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Read Setting History](#read-setting-history)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Transactional Append](#transactional-append)
  - [Canonical Audit Resource Id](#canonical-audit-resource-id)
  - [Retention Horizon](#retention-horizon)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Audit Records Table](#audit-records-table)
  - [Transactional Audit Sink](#transactional-audit-sink)
  - [One Resource Id for Write and Read](#one-resource-id-for-write-and-read)
  - [Masking and Actor Classification](#masking-and-actor-classification)
  - [History Read](#history-read)
  - [Retention](#retention)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Replaces the tracing stand-in behind the Audit Emitter with the R1 audit sink the design specifies: an append-only `audit_records` table written inside the mutation's own transaction through the `AuditSink` port, and the per-(setting, scope) history read served from that table on the canonical audit resource id.

### 1.2 Purpose

Audit is a show-stopper here, not a deferrable: a mutation that cannot record itself must not take effect. The earlier shape — a synchronous call to an external Audit Subsystem inside an open transaction — bought three limitations that all followed from the one choice: a record that commits while the mutation rolls back, an ambiguous timeout where neither side knows whether the record landed, and every mutation in this gear blocked whenever the audit endpoint is down. Writing to a local table in the same transaction removes all three by construction. The record and the change are one commit, so they cannot diverge; there is nothing to time out; and fail-closed now means the local write must succeed, which fails only when the database itself is unavailable — in which case the mutation could not have committed anyway.

The sink is a port with two bindings. R1 binds this table, the system of record for the online retention window. R2 adds shipping through the transactional outbox to the platform Audit Subsystem behind the same port — an addition, not a replacement, and one that can never affect a mutation, because by then the record is already committed.

Two things about the record itself are fixed before it is written. Masking happens before the record is built, so a `secret`-classified value is never written in plaintext anywhere. And the actor identity is itself classified, because an administrator identity is PII: the record carries the classification, and the read side honours it.

**Requirements**: `cpt-cf-settings-service-fr-audit-mutations`, `cpt-cf-settings-service-nfr-scale-growth`

**Principles**: `cpt-cf-settings-service-principle-fail-closed`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-settings-service-actor-platform-admin` | Reads the history of any setting at any scope, and is the actor most records name |
| `cpt-cf-settings-service-actor-tenant-admin` | Reads the history of settings at scopes within its own subtree |
| `cpt-cf-settings-service-actor-compliance-reviewer` | Reviews who changed what and when, with values masked by classification and no reveal path |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — §5.7 Security, Secrets & Audit; §6.1 Scale & Growth
- **Design**: [DESIGN.md](../DESIGN.md) — §4.2 (Component: Audit Emitter — *Canonical audit resource id*, *Fail-closed audit*, *The sink is a port with two bindings*), §4.3 (REST API — Search, History & Preferences; *History reads the gear's own audit store*), §4.7 (Table `audit_records`), §4.8 (The Data Path)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.6
- **Dependencies**: entry 2.1 for the Audit Emitter and the `AuditSink` port it declares; entry 2.3, since history is keyed by a setting key and the table denormalizes `declaration_key`; entry 2.7 for the tenant access rule that hides a setting's history along with the setting — until it lands, absence of restriction rows means every setting is visible.
- **Not applicable**: Shipping records to the platform Audit Subsystem through the transactional outbox is R2. The global audit view across all settings is the platform audit surface's, not an endpoint here. Secret-use records are written by the Secret Manager of entry 2.9 through the same sink. The mutations themselves stay the business of the features that perform them; this feature is the sink they all write to and the one read over it. A pruning schedule is not specified: the design fixes that `retain_until` is carried and that deletes outside pruning are forbidden, not when pruning runs.

## 2. Actor Flows (CDSL)

**Use cases**: `cpt-cf-settings-service-usecase-review-audit-trail`

### Read Setting History

- [ ] `p1` - **ID**: `cpt-cf-settings-service-flow-audit-store-history`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- The mutation history of one setting at one scope, newest first, paginated, with values as recorded and the actor masked when the caller may not see PII
- An empty page when nothing has happened, distinguishable from a failure

**Error Scenarios**:
- The caller is not authorized to read the setting
- The target tenant lies outside the caller's subtree, or is a standalone descendant
- The setting is hidden from the target tenant, reported as absent rather than forbidden

**Steps**:
1. [ ] - `p1` - Actor sends GET /settings-service/v1/settings/{key}/history with optional `tenant`, `cursor` and `limit`; `tenant` omitted means the caller's own tenant, which for a platform administrator is the root tenant and therefore platform scope - `inst-as-hist-1`
2. [ ] - `p1` - Authorize `read` on the setting's key through the `PolicyEnforcer` PEP and obtain the `AccessScope` constraints - `inst-as-hist-2`
3. [ ] - `p1` - **IF** the decision is deny or cannot be obtained → **RETURN** `403` - `inst-as-hist-3`
4. [ ] - `p1` - Confirm through the tenant resolver that the target tenant is the caller's own or a descendant, and not a standalone descendant; **IF** it is neither → **RETURN** `403`, since a caller that cannot read a tenant's values cannot read their history either - `inst-as-hist-4`
5. [ ] - `p1` - DB: SELECT the declaration by key; **IF** none → **RETURN** `404`; a retired declaration keeps its history and is read like an active one - `inst-as-hist-5`
6. [ ] - `p1` - Evaluate the caller's effective tenant access for the setting; **IF** `hidden` → **RETURN** `404` rather than `403`, so a hidden setting's existence is not disclosed through its history - `inst-as-hist-6`
7. [ ] - `p1` - Compose the canonical audit resource id for the key and the target tenant with the shared formatter, and DB: SELECT audit_records WHERE declaration_key = {key} AND tenant_id = {tenant} ORDER BY occurred_at DESC through `idx_audit_scoped`, cursor-paginated, on the caller's `AccessScope` - `inst-as-hist-7`
8. [ ] - `p1` - **FOR EACH** record → **IF** its actor classification is `pii` **AND** the caller is not authorized for unmasked PII → mask the actor; **IF** a recorded value is `pii`-classified under the same condition → mask it; a `secret` value needs no decision here, since it was never recorded in plaintext - `inst-as-hist-8`
9. [ ] - `p1` - **RETURN** `200` with the page of records — `operation`, `actor`, `pre_value`, `post_value`, `outcome`, `request_id`, `change_set_id`, `occurred_at` — and its pagination cursors; an empty page is `200` with no items, never an error - `inst-as-hist-9`

## 3. Processes / Business Logic (CDSL)

### Transactional Append

- [ ] `p1` - **ID**: `cpt-cf-settings-service-algo-audit-store-append`

**Input**: The mutation's open transaction, the caller's `AccessScope`, and the audit record draft — resource key and tenant, operation, actor, pre-image, post-image, outcome, request id, optional change set id

**Output**: The record committed together with the mutation, or the mutation rejected

**Steps**:
1. [ ] - `p1` - Mask the pre-image and post-image by the declaration's classification before the record exists: a `secret` value is replaced by the mask token and never enters the record; a `pii` or `public` value is recorded as is, the read side masking `pii` for callers without the entitlement - `inst-as-append-1`
2. [ ] - `p1` - Stamp the actor and the actor's own `public` or `pii` classification onto the record, so the read side can honour it without re-deriving who the actor was - `inst-as-append-2`
3. [ ] - `p1` - Compose the `resource` field with the shared formatter, and set `declaration_key` and `tenant_id` from the same two inputs, so the scoped query is an index lookup that can never disagree with the resource id - `inst-as-append-3`
4. [ ] - `p1` - Set `occurred_at` to the transaction's clock, `retain_until` to the value supplied or `NULL` for the configured default, and `change_set_id` when the mutation was produced under one - `inst-as-append-4`
5. [ ] - `p1` - DB: INSERT INTO audit_records inside the caller's transaction as its last step before commit, on the same `AccessScope`-scoped path as every other write, so a record is never visible outside its tenant - `inst-as-append-5`
6. [ ] - `p1` - **IF** the insert fails → the transaction is rolled back and the mutation is rejected as unavailable, `503`; the platform never applies a change it could not record - `inst-as-append-6`
7. [ ] - `p1` - **RETURN** with nothing further to do: the record commits with the change or rolls back with it, and no delivery state is tracked here, since shipping belongs to the R2 outbox - `inst-as-append-7`

### Canonical Audit Resource Id

- [ ] `p1` - **ID**: `cpt-cf-settings-service-algo-audit-store-resource-id`

**Input**: A setting key and the tenant id that is the record's scope

**Output**: The resource id string both the write and the history read use

**Steps**:
1. [ ] - `p1` - Format `cf.settings:{key}@{tenant_id}` — the key verbatim, since it is immutable for the life of the declaration and so keeps a setting's history continuous through every metadata edit - `inst-as-rid-1`
2. [ ] - `p1` - Use the flat tenant UUID for every scope, the root tenant's id being platform scope, and never a tenant path, which is derived state that a re-parent or rename would invalidate under every historical record - `inst-as-rid-2`
3. [ ] - `p1` - **RETURN** the id, which maps one `(setting, scope)` pair to exactly one string, so per-scope history is a single exact-match query and never a prefix or wildcard search - `inst-as-rid-3`

### Retention Horizon

- [ ] `p1` - **ID**: `cpt-cf-settings-service-algo-audit-store-retention`

**Input**: A record's `retain_until`, and the store's configured default retention

**Output**: Whether the record is inside its online window

**Steps**:
1. [ ] - `p1` - **IF** the record carries `retain_until` → its horizon is that instant - `inst-as-ret-1`
2. [ ] - `p1` - **ELSE** its horizon is `occurred_at` plus the configured default, which **MUST NOT** be shorter than twelve months - `inst-as-ret-2`
3. [ ] - `p1` - Pruning deletes only records past their horizon, located through the partial `idx_audit_retention`; no other `DELETE` and no `UPDATE` is ever issued against the table - `inst-as-ret-3`
4. [ ] - `p1` - **RETURN** the horizon; R2 shipping copies a record onward but changes nothing about its online window here - `inst-as-ret-4`

## 4. States (CDSL)

Not applicable. An audit record is appended once and never transitions; its only end is deletion by retention pruning, which is captured as a process above.

## 5. Definitions of Done

### Audit Records Table

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-audit-store-table`

The system **MUST** persist audit records in an `audit_records` table carrying `resource`, `declaration_key`, a non-null `tenant_id`, `operation`, `actor`, `actor_classification`, masked `pre_value` and `post_value`, `outcome`, `request_id`, a nullable `change_set_id`, `occurred_at` and a nullable `retain_until`, with check constraints on the `operation` and `outcome` vocabularies, `idx_audit_scoped` on `(declaration_key, tenant_id, occurred_at DESC)` and the partial `idx_audit_retention`. The table **MUST** be append-only: no code path issues an `UPDATE`, and the only `DELETE` is retention pruning.

**Implements**:
- `cpt-cf-settings-service-algo-audit-store-append`

**Constraints**: `cpt-cf-settings-service-constraint-postgres-primary-storage`, `cpt-cf-settings-service-constraint-audit-and-events`

**Touches**:
- DB Table: `audit_records`
- Entities: `AuditRecord`

### Transactional Audit Sink

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-audit-store-transactional-sink`

The system **MUST** bind the `AuditSink` port — `append(txn, scope, record)` — to the `audit_records` table so that every mutation commits its record in its own transaction, as the last step before commit, and is rejected as unavailable when the record cannot be written. Category and declaration mutations **MUST** be moved onto this sink and the tracing stand-in retired. The port **MUST** stay the only way a record is written, so the R2 outbox binding can be added behind it without touching a call site.

**Implements**:
- `cpt-cf-settings-service-algo-audit-store-append`

**Constraints**: `cpt-cf-settings-service-constraint-audit-and-events`

**Touches**:
- Entities: `AuditSink`, `AuditRecord`
- DB Table: `audit_records`

### One Resource Id for Write and Read

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-audit-store-resource-id`

The system **MUST** form every record's `resource` as `cf.settings:{key}@{tenant_id}` through one formatter shared by the write side and the history read, keyed by the flat tenant UUID with the root tenant's id as platform scope and never by a tenant path, so that a `(setting, scope)` pair maps to exactly one id and its history is a single exact-match query.

**Implements**:
- `cpt-cf-settings-service-algo-audit-store-resource-id`

**Touches**:
- Entities: `AuditRecord`

### Masking and Actor Classification

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-audit-store-masking-classification`

A `secret`-classified value **MUST** be masked before the record is built and **MUST NOT** appear in plaintext in any record. Every record **MUST** carry the actor's `public` or `pii` classification, and the history read **MUST** mask a `pii` actor, and any `pii`-classified recorded value, for a caller not authorized for unmasked PII. The read **MUST NOT** apply a second masking implementation to secrets, since none were recorded.

**Implements**:
- `cpt-cf-settings-service-algo-audit-store-append`
- `cpt-cf-settings-service-flow-audit-store-history`

**Constraints**: `cpt-cf-settings-service-constraint-secrets-by-reference`

**Touches**:
- Entities: `AuditRecord`
- API: `GET /settings-service/v1/settings/{key}/history`

### History Read

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-audit-store-history-read`

The system **MUST** serve `GET /settings-service/v1/settings/{key}/history` from the gear's own `audit_records` table, newest first and cursor-paginated, authorized by `read` on the setting's key, confined to the caller's own tenant or a descendant that is not standalone, and reporting a hidden setting as absent rather than forbidden. The query **MUST** be an index lookup on `(declaration_key, tenant_id)`, and an empty history **MUST** be an empty page rather than an error.

**Implements**:
- `cpt-cf-settings-service-flow-audit-store-history`

**Constraints**: `cpt-cf-settings-service-constraint-rbac-policy-enforcer`

**Touches**:
- API: `GET /settings-service/v1/settings/{key}/history`
- DB Table: `audit_records`

### Retention

- [ ] `p1` - **ID**: `cpt-cf-settings-service-dod-audit-store-retention`

Every record **MUST** carry `retain_until` or fall under the store's configured default, which **MUST** be configurable and **MUST NOT** default below twelve months. Pruning **MUST** locate expired records through `idx_audit_retention` and **MUST** be the only path that deletes from the table.

**Implements**:
- `cpt-cf-settings-service-algo-audit-store-retention`

**Touches**:
- DB Table: `audit_records`

## 6. Acceptance Criteria

- [ ] A category mutation, a declaration mutation, and a value write each leave exactly one audit record, committed in the same transaction as the change
- [ ] A fault injected between the mutation's write and the record's insert leaves neither behind: no changed row, no record
- [ ] When the record cannot be inserted, the mutation is rejected as unavailable and the caller sees no change
- [ ] A `secret`-classified value appears in no record; its pre-image and post-image carry the mask token
- [ ] A record's `resource` equals the shared formatter's output for the same key and tenant, and the history read finds it by that pair
- [ ] The platform-scope record of a category or a platform-level value carries the root tenant's id, never a sentinel
- [ ] History for one setting at one scope returns only that pair's records, newest first, and a second page follows the cursor without duplicates
- [ ] A `pii`-classified actor is masked for a caller without the PII entitlement and unmasked for one with it
- [ ] History of a hidden setting returns `404`, and history of a setting for a tenant outside the caller's subtree, or for a standalone descendant, returns `403`
- [ ] History of a retired declaration is readable
- [ ] A setting with no history returns `200` with an empty page
- [ ] A record inserted with no `retain_until` is pruned only after the configured default horizon, and one with an explicit `retain_until` only after that instant
- [ ] No code path issues an `UPDATE` against `audit_records`, and the only `DELETE` is pruning
- [ ] A record produced under a change set carries its `change_set_id`, and records of one change set are retrievable together
