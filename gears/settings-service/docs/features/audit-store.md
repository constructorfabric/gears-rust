<!-- Created: 2026-09-06 by Virtuozzo International GmbH -->
<!-- Updated: 2026-10-15 by Virtuozzo International GmbH -->

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
  - [Resolve Actor Names](#resolve-actor-names)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Audit Records Table](#audit-records-table)
  - [Transactional Audit Sink](#transactional-audit-sink)
  - [One Resource Id for Write and Read](#one-resource-id-for-write-and-read)
  - [Masking and Actor Classification](#masking-and-actor-classification)
  - [History Read](#history-read)
  - [Retention](#retention)
  - [Actor Names on History](#actor-names-on-history)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Replaces the tracing stand-in behind the Audit Emitter with the R1 audit sink the design specifies: an append-only `audit_records` table written inside the mutation's own transaction through the `AuditSink` port, and the per-(setting, scope) history read served from that table on the canonical audit resource id. Each history record also carries a display name of its actor, `actor_name`, resolved server-side at read time and never stored.

### 1.2 Purpose

Audit is a show-stopper here, not a deferrable: a mutation that cannot record itself must not take effect. The earlier shape — a synchronous call to an external Audit Subsystem inside an open transaction — bought three limitations that all followed from the one choice: a record that commits while the mutation rolls back, an ambiguous timeout where neither side knows whether the record landed, and every mutation in this gear blocked whenever the audit endpoint is down. Writing to a local table in the same transaction removes all three by construction. The record and the change are one commit, so they cannot diverge; there is nothing to time out; and fail-closed now means the local write must succeed, which fails only when the database itself is unavailable — in which case the mutation could not have committed anyway.

The sink is a port with two bindings. R1 binds this table, the system of record for the online retention window. R2 adds shipping through the transactional outbox to the platform Audit Subsystem behind the same port — an addition, not a replacement, and one that can never affect a mutation, because by then the record is already committed.

Two things about the record itself are fixed before it is written. Masking happens before the record is built, so a `secret`-classified value is never written in plaintext anywhere. And the actor identity is itself classified, because an administrator identity is PII: the record carries the classification, and the read side honours it. A compliance reviewer reading who changed what needs that actor as a display name rather than a bare identifier, which is why each history record also carries a server-resolved `actor_name` that is never stored.

**Requirements**: `cpt-cf-settings-service-fr-audit-mutations`, `cpt-cf-settings-service-nfr-scale-growth`

**Principles**: `cpt-cf-settings-service-principle-fail-closed`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-settings-service-actor-platform-admin` | Reads the history of any setting at any scope, and is the actor most records name |
| `cpt-cf-settings-service-actor-tenant-admin` | Reads the history of settings at scopes within its own subtree |
| `cpt-cf-settings-service-actor-compliance-reviewer` | Reviews who changed what and when, with values masked by classification and no reveal path |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) — §5.7 Security, Secrets & Audit; §6.1 Scale & Growth (`cpt-cf-settings-service-nfr-scale-growth`). The display name on the history read and the optional Account Management dependency are required there too: §5.7 (`cpt-cf-settings-service-fr-audit-mutations`) and §2.2 (`cpt-cf-settings-service-actor-account-management`).
- **Design**: [DESIGN.md](../DESIGN.md) — §4.2 (Component: Audit Emitter — *Canonical audit resource id*, *Fail-closed audit*, *The sink is a port with two bindings*), §4.3 (REST API — Search, History & Preferences; *History reads the gear's own audit store*), §4.7 (Table `audit_records`), §4.8 (The Data Path); §3.2 constraint `cpt-cf-settings-service-constraint-account-management-optional`; §4.2 Component: Actor Name Resolver, `cpt-cf-settings-service-component-actor-name-resolver`; §4.6 sequence `cpt-cf-settings-service-seq-history-actor-names`
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) entry 2.6
- **Soft dependency**: Account Management (AM), reached only to put a display name on a history record's actor, through `AccountManagementClient` (`account-management-sdk`) looked up in `ClientHub` at each lookup; a deployment without AM reads every `actor_name` as `null`.
- **Dependencies**: entry 2.1 for the Audit Emitter and the `AuditSink` port it declares; entry 2.3, since history is keyed by a setting key and the table denormalizes `declaration_key`; entry 2.7 for the tenant access rule that hides a setting's history along with the setting — until it lands, absence of restriction rows means every setting is visible.
- **Not applicable**: Shipping records to the platform Audit Subsystem through the transactional outbox is R2. The global audit view across all settings is the platform audit surface's, not an endpoint here. Secret-use records are written by the Secret Manager of entry 2.9 through the same sink. The mutations themselves stay the business of the features that perform them; this feature is the sink they all write to and the one read over it. A pruning schedule is not specified: the design fixes that `retain_until` is carried and that deletes outside pruning are forbidden, not when pruning runs.

## 2. Actor Flows (CDSL)

**Use cases**: `cpt-cf-settings-service-usecase-review-audit-trail`

### Read Setting History

- [x] `p1` - **ID**: `cpt-cf-settings-service-flow-audit-store-history`

**Actor**: `cpt-cf-settings-service-actor-tenant-admin`

**Success Scenarios**:
- The mutation history of one setting at one scope, newest first, paginated, with values as recorded and the actor masked when the caller may not see PII
- An empty page when nothing has happened, distinguishable from a failure
- Every unmasked administrator actor the caller may see in AM carries a display name; a name that cannot be resolved reads `null` and never fails the page

**Error Scenarios**:
- The caller is not authorized to read the setting
- The target tenant lies outside the caller's subtree, or is a standalone descendant
- The setting is hidden from the target tenant, reported as absent rather than forbidden

**Steps**:
1. [x] - `p1` - Actor sends GET /settings-service/v1/settings/{key}/history with optional `tenant`, `cursor` and `limit`; `tenant` omitted means the caller's own tenant, which for a platform administrator is the root tenant and therefore platform scope - `inst-as-hist-1`
2. [x] - `p1` - Authorize `read` on the setting's key through the `PolicyEnforcer` PEP and obtain the `AccessScope` constraints - `inst-as-hist-2`
3. [x] - `p1` - **IF** the decision is deny or cannot be obtained → **RETURN** `403` - `inst-as-hist-3`
4. [x] - `p1` - Confirm through the tenant resolver that the target tenant is the caller's own or a descendant, and not a standalone descendant; **IF** it is neither → **RETURN** `403`, since a caller that cannot read a tenant's values cannot read their history either - `inst-as-hist-4`
5. [x] - `p1` - DB: SELECT the declaration by key; **IF** none → **RETURN** `404`; a retired declaration keeps its history and is read like an active one - `inst-as-hist-5`
6. [x] - `p1` - Evaluate the caller's effective tenant access for the setting; **IF** `hidden` → **RETURN** `404` rather than `403`, so a hidden setting's existence is not disclosed through its history - `inst-as-hist-6`
7. [x] - `p1` - Compose the canonical audit resource id for the key and the target tenant with the shared formatter, and DB: SELECT audit_records WHERE declaration_key = {key} AND (tenant_id = {tenant} OR tenant_id IS NULL) ORDER BY occurred_at DESC through `idx_audit_scoped`, cursor-paginated and bound to the pair, on the caller's `AccessScope` — the scope's own records and the setting's definition records, which belong to no tenant and explain changes no scope made - `inst-as-hist-7`
8. [x] - `p1` - **FOR EACH** record → **IF** its actor classification is `pii` **AND** the caller is not authorized for unmasked PII → mask the actor; **IF** a recorded value carries the `pii` class, or the setting is `pii` now, under the same condition → mask it — a value recorded while its setting was `pii` stays masked after the setting is declassified, and one recorded before it became `pii` is masked once it is; a `secret` value needs no decision here, since it was never recorded in plaintext - `inst-as-hist-8`
9. [x] - `p1` - Obtain the target's chain from the reader tenant — the caller's own tenant, the root for a platform administrator — and apply the identity rule to each record's actor: show it only when its recorded `actor_tenant_id` lies on the path from the reader tenant down to the record's tenant, as the hierarchy stands at the read, else withhold it — the mask token with `actor_withheld: true`, whatever the PII entitlement; a definition record has no tenant, so its actor is on no path and a machine act's actor carries no actor tenant, both withheld from every tenant reader; a platform administrator sees every actor - `inst-as-hist-10`
10. [x] - `p1` - **RETURN** `200` with the page of records — `tenant_id` (`null` for a definition record), `operation`, `actor`, `actor_withheld`, `pre_value`, `post_value`, `outcome`, `request_id`, `change_set_id`, `occurred_at` — and its pagination cursors, and `page_info.total_count`, the records the pair holds in all; an empty page is `200` with no items and a total of zero, never an error - `inst-as-hist-9`

## 3. Processes / Business Logic (CDSL)

### Transactional Append

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-audit-store-append`

**Input**: The mutation's open transaction, the caller's `AccessScope`, and the audit record draft — resource key and tenant, operation, actor, pre-image, post-image, outcome, request id, optional change set id

**Output**: The record committed together with the mutation, or the mutation rejected

**Steps**:
1. [x] - `p1` - Mask the pre-image and post-image by the declaration's classification before the record exists: a `secret` value is replaced by the mask token and never enters the record; a `pii` value is recorded with its class inside the image, so the class the value had when it was set outlives a later reclassification of the setting, and a `public` value is recorded as is; the caller names the class of every image — a restriction or a category is `public`, a declaration's own image is `pii` when its setting is, since its Schema Default is a value of it - `inst-as-append-1`
2. [x] - `p1` - Stamp the actor and the actor's own `public` or `pii` classification onto the record, so the read side can honour it without re-deriving who the actor was - `inst-as-append-2`
3. [x] - `p1` - Stamp `actor_tenant_id` from the request's security context — the actor's subject tenant — and none for a machine act with no security context, such as a gear's reconcile or upgrade; it is set once at insert, as the store is append-only - `inst-as-append-8`
4. [x] - `p1` - Compose the `resource` field with the shared formatter, and set `declaration_key` and `tenant_id` from the same two inputs, so the scoped query is an index lookup that can never disagree with the resource id - `inst-as-append-3`
5. [x] - `p1` - Set `occurred_at` from the gear's shared clock, aligned to the microsecond Postgres keeps as every other timestamp column is, so both backends hold the same instant, `retain_until` to the value supplied or `NULL` for the configured default, and `change_set_id` when the mutation was produced under one - `inst-as-append-4`
6. [x] - `p1` - DB: INSERT INTO audit_records inside the caller's transaction as its last step before commit, on the same `AccessScope`-scoped path as every other write, so a record is never visible outside its tenant - `inst-as-append-5`
7. [x] - `p1` - **IF** the insert fails → the transaction is rolled back and the mutation is rejected as unavailable, `503`; the platform never applies a change it could not record - `inst-as-append-6`
8. [x] - `p1` - **RETURN** with nothing further to do: the record commits with the change or rolls back with it, and no delivery state is tracked here, since shipping belongs to the R2 outbox - `inst-as-append-7`

### Canonical Audit Resource Id

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-audit-store-resource-id`

**Input**: A setting key and the tenant id that is the record's scope

**Output**: The resource id string both the write and the history read use

**Steps**:
1. [x] - `p1` - Format `cf.settings:{key}@{tenant_id}` where the record has a scope, and `cf.settings:{key}` where it has none — the key verbatim, since it is immutable for the life of the declaration and so keeps a setting's history continuous through every metadata edit - `inst-as-rid-1`
2. [x] - `p1` - Use the flat tenant UUID for every scope, the root tenant's id being platform scope, and never a tenant path, which is derived state that a re-parent or rename would invalidate under every historical record; omit the scope entirely — separator and all — for a record about a **definition**, which sits at no scope and would otherwise have to borrow one - `inst-as-rid-2`
3. [x] - `p1` - **RETURN** the id, which maps one `(setting, scope)` pair to exactly one string, so per-scope history is a single exact-match query and never a prefix or wildcard search - `inst-as-rid-3`

### Retention Horizon

- [x] `p1` - **ID**: `cpt-cf-settings-service-algo-audit-store-retention`

**Input**: A record's `retain_until`, and the store's configured default retention

**Output**: Whether the record is inside its online window

**Steps**:
1. [x] - `p1` - **IF** the record carries `retain_until` → its horizon is that instant - `inst-as-ret-1`
2. [x] - `p1` - **ELSE** its horizon is `occurred_at` plus the configured default, which **MUST NOT** be shorter than twelve months - `inst-as-ret-2`
3. [x] - `p1` - Pruning, run by the gear's managed lifecycle at start and once a day, deletes only records past their horizon, located through the partial `idx_audit_retention` for an explicit hold and the partial `idx_audit_default_horizon` for the default; it works in bounded batches — each selects its ids through those indexes, then deletes them by primary key, two short statements whose cost does not grow with the table — each batch its own commit, until a batch comes back short, a per-tick cap is reached or the lifecycle stops, leaving the rest to the next tick; no other `DELETE` and no `UPDATE` is ever issued against the table - `inst-as-ret-3`
4. [x] - `p1` - **RETURN** the horizon; R2 shipping copies a record onward but changes nothing about its online window here - `inst-as-ret-4`

### Resolve Actor Names

- [x] `p2` - **ID**: `cpt-cf-settings-service-algo-audit-store-actor-names`

**Input**: The page of history records after masking and the caller's `SecurityContext`

**Output**: `actor_name` on every record of the page, and one lookup result, `ok`, `degraded` or `skipped`

**Steps**:
1. [x] - `p2` - Collect the user-id actors of the page whose actor is unmasked, each with the tenant its record says the actor acted from (`actor_tenant_id`), and deduplicate them; a masked actor, a module-name actor, the nil id and an actor whose record carries no actor tenant are never looked up, and the nil id reads `"System"` when unmasked - `inst-as-names-1`
2. [x] - `p2` - **IF** no user-id actor remains → no lookup is made, the pass counts as `skipped`, and continue with the fill step - `inst-as-names-17`
3. [x] - `p2` - **IF** the context has no real subject → no lookup is made and nothing is cached: every user-id actor reads `null`, the pass counts as `skipped`, and continue with the fill step - `inst-as-names-2`
4. [x] - `p2` - **IF** no `AccountManagementClient` is found in `ClientHub` → every user-id actor reads `null`, the pass counts as `skipped`, and continue with the fill step - `inst-as-names-3`
5. [x] - `p2` - Take cache hits first: the key is the caller's tenant, the caller's subject and the actor, a name lives 5 minutes, and the cache holds at most 10,000 names; a full cache drops its expired names, then all - `inst-as-names-4`
6. [x] - `p2` - **IF** at least one actor was collected and every one was a cache hit → the pass counts as `ok` with no AM call, and continue with the fill step - `inst-as-names-5`
7. [x] - `p2` - Start one budget of 1 s for the page's lookups, counted by the resolver from here - `inst-as-names-6`
8. [x] - `p2` - Group the collected ids the cache did not answer by the actor's tenant and run the lookup - `inst-as-names-7`
   1. [x] - `p2` - Split each tenant group into chunks of 200 (`IdpUserPagination::MAX_TOP`) and run at most 4 chunks at once - `inst-as-names-9`
      1. [x] - `p2` - API: `AccountManagementClient::list_users` with an id-set filter for the chunk in the group's tenant, always with the caller's own `SecurityContext`; AM decides visibility, and no privileged context is used - `inst-as-names-10`
      2. [x] - `p2` - **IF** the chunk is answered → fill the names it returned and keep each in the cache for this caller; the name is the display name, then first and last name, then the username, blank values skipped - `inst-as-names-11`
      3. [x] - `p2` - **IF** an actor is refused by the answer or not in it → it stays unresolved, is not cached, and nothing is logged; a refused answer, including a whole chunk AM refuses, is AM's own answer: it counts as `ok` for the pass and logs nothing - `inst-as-names-12`
      4. [x] - `p2` - **IF** the chunk fails, times out, hits a paging or query failure or the budget → its actors stay unresolved, the pass is `degraded`, and one warn line is logged with the reason kind and the id count only, with no id, no profile and no error text - `inst-as-names-13`
9. [x] - `p2` - Every actor the lookup did not answer reads `null`, final; no other tenant is asked - `inst-as-names-19`
10. [x] - `p2` - **FOR EACH** record → set `actor_name` to `null` when `actor_masked` is true, `null` for a module actor, `"System"` for the nil id when unmasked, and otherwise the resolved name or `null` - `inst-as-names-16`
11. [x] - `p2` - Count the pass once in `settings_actor_name_lookups_total` and record its duration in `settings_actor_name_lookup_duration_seconds`, both by `result` on every path: `ok` when every chunk answered — a refused or not-found answer is an answer — an all-cache-hit pass included, `degraded` when any chunk failed, timed out or hit the budget, `skipped` when no lookup was made - `inst-as-names-14`
12. [x] - `p2` - **RETURN** the page with `actor_name` on every record and the pass result - `inst-as-names-15`

The visibility boundary holds in both directions. An ancestor reading a descendant scope gets the name of each actor AM lets the caller list in the actor's own tenant — the descendant's administrators in the descendant's tenant, an ancestor administrator who wrote there in the ancestor's — and `null` otherwise. A descendant reading a record an ancestor administrator wrote reads `null`, because the actor is withheld by the upward disclosure boundary (`inst-as-hist-10`), masked and never looked up, so an ancestor's display name is never shown to a subordinate tenant. A record written before the actor tenant was recorded reads `null`: the resolver asks no tenant the record does not name. This relies on AM allowing an ancestor administrator to list a descendant tenant's users; where it does not, names read `null` and nothing leaks.

## 4. States (CDSL)

Not applicable. An audit record is appended once and never transitions; its only end is deletion by retention pruning, which is captured as a process above.

## 5. Definitions of Done

### Audit Records Table

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-audit-store-table`

The system **MUST** persist audit records in an `audit_records` table carrying `resource`, `declaration_key`, a `tenant_id` that is set for a record about a scope and null for one about a definition, `operation`, `actor`, `actor_classification`, a nullable `actor_tenant_id` set at insert, masked `pre_value` and `post_value`, `outcome`, `request_id`, a nullable `change_set_id`, `occurred_at` and a nullable `retain_until`, with check constraints on the `operation` and `outcome` vocabularies, `idx_audit_scoped` on `(declaration_key, tenant_id, occurred_at DESC)`, the partial `idx_audit_retention`, and the partial `idx_audit_default_horizon` on `occurred_at` where `retain_until` is null. The table **MUST** be append-only, and the store **MUST** say so itself: no code path issues an `UPDATE`, the only `DELETE` is retention pruning, and a trigger carried by the gear's own migration refuses every `UPDATE` at the database — on PostgreSQL also a `DELETE` while an explicit `retain_until` hold is in force or while the record is younger than the greater of the platform minimum of twelve months and the configured retention, which the gear writes into the one-row `settings_audit_policy` table before every retention pass — a row a migration seeds at the minimum, so the pass only ever updates it and two replicas have nothing to race for (a check keeps it from going below the minimum), and one that migration's rollback leaves in place, since it is configuration a live writer depends on, not schema. SQLite, the test backend, carries no delete guard, since its retention tests move the clock. The trigger guards against a mistake in code, not a privileged database writer, who can drop it: that is for database roles, which stay with provisioning, and for the off-box copy R2 ships.

The vocabularies of `operation`, `outcome` and `actor_classification` carry a compatibility contract. Their spellings are at once the stored form and the wire form of the history response, and are **permanent once shipped**. Adding a value is compatible: a migration widens the check constraint, and a reader **MUST** treat the field as an open set of strings, showing a value it does not know as it is rather than refusing the record. Renaming or removing a value is breaking. The service itself **MUST NOT** guess: a stored value it does not know is an integrity error on the read, never a record with a default.

**Implements**:
- `cpt-cf-settings-service-algo-audit-store-append`

**Constraints**: `cpt-cf-settings-service-constraint-postgres-primary-storage`, `cpt-cf-settings-service-constraint-audit-and-events`

**Touches**:
- DB Table: `audit_records`
- Entities: `AuditRecord`

### Transactional Audit Sink

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-audit-store-transactional-sink`

The system **MUST** bind the `AuditSink` port — `append(txn, scope, record)` — to the `audit_records` table so that every mutation commits its record in its own transaction, as the last step before commit, and is rejected as unavailable when the record cannot be written. Category and declaration mutations **MUST** be moved onto this sink and the tracing stand-in retired. The port **MUST** stay the only way a record is written, so the R2 outbox binding can be added behind it without touching a call site. Each record's `actor_tenant_id` **MUST** be stamped once at insert from the security context's subject tenant — none for a machine act with no security context, such as a gear's reconcile or upgrade — and never rewritten.

**Implements**:
- `cpt-cf-settings-service-algo-audit-store-append`

**Constraints**: `cpt-cf-settings-service-constraint-audit-and-events`

**Touches**:
- Entities: `AuditSink`, `AuditRecord`
- DB Table: `audit_records`

### One Resource Id for Write and Read

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-audit-store-resource-id`

The system **MUST** form every record's `resource` as `cf.settings:{key}@{tenant_id}`, or as `cf.settings:{key}` when the record is about a definition and has no scope, through one formatter shared by the write side and the history read, keyed by the flat tenant UUID with the root tenant's id as platform scope and never by a tenant path, so that a `(setting, scope)` pair maps to exactly one id and its history is a single exact-match query.

**Implements**:
- `cpt-cf-settings-service-algo-audit-store-resource-id`

**Touches**:
- Entities: `AuditRecord`

### Masking and Actor Classification

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-audit-store-masking-classification`

A `secret`-classified value **MUST** be masked before the record is built and **MUST NOT** appear in plaintext in any record. Every record **MUST** carry the actor's `public` or `pii` classification, and the history read **MUST** mask a `pii` actor, and any `pii`-classified recorded value, for a caller not authorized for unmasked PII. A value's class **MUST** be recorded with it, so a value recorded while its setting was `pii` stays masked after the setting is declassified; while a setting is `pii`, every value recorded for it is masked, including those recorded before it was classified so. The read **MUST NOT** apply a second masking implementation to secrets, since none were recorded.

**Implements**:
- `cpt-cf-settings-service-algo-audit-store-append`
- `cpt-cf-settings-service-flow-audit-store-history`

**Constraints**: `cpt-cf-settings-service-constraint-secrets-by-reference`

**Touches**:
- Entities: `AuditRecord`
- API: `GET /settings-service/v1/settings/{key}/history`

### History Read

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-audit-store-history-read`

The system **MUST** serve `GET /settings-service/v1/settings/{key}/history` from the gear's own `audit_records` table, newest first and cursor-paginated, authorized by `read` on the setting's key, confined to the caller's own tenant or a descendant that is not standalone, and reporting a hidden setting as absent rather than forbidden. The query **MUST** be an index lookup on `(declaration_key, tenant_id)`, and an empty history **MUST** be an empty page rather than an error. An actor **MUST** be shown only when its recorded `actor_tenant_id` lies on the path from the reader tenant down to the record's tenant, else the mask token with `actor_withheld: true`; a definition record's actor, a machine act's actor and a record written before the column existed are withheld from every tenant reader, and a platform administrator sees every actor. The `actor_name` field of each record is owned by `cpt-cf-settings-service-dod-audit-store-actor-names`.

**Implements**:
- `cpt-cf-settings-service-flow-audit-store-history`

**Constraints**: `cpt-cf-settings-service-constraint-rbac-policy-enforcer`

**Touches**:
- API: `GET /settings-service/v1/settings/{key}/history`
- DB Table: `audit_records`

### Retention

- [x] `p1` - **ID**: `cpt-cf-settings-service-dod-audit-store-retention`

Every record **MUST** carry `retain_until` or fall under the store's configured default, which **MUST** be configurable and **MUST NOT** default below twelve months. Pruning **MUST** locate expired records through an index — `idx_audit_retention` for an explicit hold, `idx_audit_default_horizon` for the default — **MUST** delete them in bounded batches so no statement grows with a backlog, **MUST** report every pass — `settings_audit_retention_passes_total` by `result` (`ok`, `failed`) and `settings_audit_records_pruned_total` — so a pass that fails is told apart from one with nothing to prune, and **MUST** be the only path that deletes from the table. A failed pass is retried on the next daily tick rather than sooner: the retention is a minimum, so a record kept a day past its horizon breaks nothing.

**Implements**:
- `cpt-cf-settings-service-algo-audit-store-retention`

**Touches**:
- DB Table: `audit_records`

### Actor Names on History

- [x] `p2` - **ID**: `cpt-cf-settings-service-dod-audit-store-actor-names`

The system **MUST** return `actor_name: string | null` beside `actor` on every history record: `null` when the actor is masked or a module name, `"System"` for the nil id when unmasked, and otherwise the display name resolved from Account Management or `null` when it cannot be had. It **MUST** resolve names once per page, inside a budget of 1 s the resolver keeps itself, so AM never fails the page or delays it by more than that. It **MUST** keep AM optional: with no `AccountManagementClient` in `ClientHub` every name reads `null`. It **MUST** call AM only with the caller's own `SecurityContext`, **MUST** cache names per caller only, and **MUST NOT** persist a name or log an id, a profile or an error text. The field is additive and nullable: removing the resolver returns `null` everywhere and leaves the record and the table unchanged.

**Security**: a masked actor is never looked up; AM, not this gear, decides what the caller may see, with no privileged context and no added permission; the cache key includes the caller's tenant and subject, and a name is never persisted. Names are transient PII and fall under the platform retention and privacy policy of the actor identity. A cached name may outlive a visibility narrowing or an erasure in AM by up to 5 minutes, the accepted staleness bound (DESIGN §5.2 "Account Management for actor names").

**Reliability**: a failed, refused, late or absent AM yields `null` names on a `200`; there is deliberately no circuit breaker and no negative cache, so a recovered AM is used again on the next read. The AM call is single-shot with no retry, and the cap of 4 chunks at once is per page, deliberately with no process-wide bound.

**Observability**: `settings_actor_name_lookups_total` and `settings_actor_name_lookup_duration_seconds` by `result` (`ok`, `degraded`, `skipped`), carrying no actor identifier, and one warn line per degraded chunk with the reason kind and the id count only.

**Test layering**: unit tests cover the name fallback order, the cache bound and the per-caller key; API and integration tests use a mock `AccountManagementClient` that answers per tenant and records its queries, covering the per-tenant queries, the chunking, the budget and the absent client; a load test that times the history page with a healthy and a slow AM is planned after the first release; until it runs, the p95 criterion in §6 stays unchecked.

**Implements**:
- `cpt-cf-settings-service-flow-audit-store-history`
- `cpt-cf-settings-service-algo-audit-store-actor-names`

**Constraints**: `cpt-cf-settings-service-constraint-account-management-optional`, `cpt-cf-settings-service-constraint-rbac-policy-enforcer`

**Touches**:
- API: `GET /settings-service/v1/settings/{key}/history` (field `actor_name`)
- Entities: Actor Name Resolver (`cpt-cf-settings-service-component-actor-name-resolver`)
- Sequence: `cpt-cf-settings-service-seq-history-actor-names`
- ClientHub: `AccountManagementClient`
- Metrics: `settings_actor_name_lookups_total`, `settings_actor_name_lookup_duration_seconds`

## 6. Acceptance Criteria

- [x] A category mutation, a declaration mutation, and a value write each leave exactly one audit record, committed in the same transaction as the change
- [x] A fault injected between the mutation's write and the record's insert leaves neither behind: no changed row, no record
- [x] When the record cannot be inserted, the mutation is rejected as unavailable and the caller sees no change
- [x] A `secret`-classified value appears in no record; its pre-image and post-image carry the mask token
- [x] A stored pre-image or post-image that does not decode fails the history read as an integrity error, the same way an unknown operation or outcome does; it is never read as an absent image
- [x] The spellings of `operation`, `outcome` and `actor_classification` are the stored and the wire form and never change once shipped; a reader treats each as an open set of strings, and the service fails the read on a stored value it does not know rather than defaulting it
- [x] A record's `resource` equals the shared formatter's output for the same key and tenant, and the history read finds it by that pair
- [x] A platform-level **value** record carries the root tenant's id, never a sentinel; a record about a **definition** — a category or a declaration — carries no tenant at all, so the write borrows no scope and asks the Tenant Resolver for nothing
- [x] History for one setting at one scope returns that pair's records and the setting's definition records, each item's `tenant_id` telling them apart (`null` for a definition record), newest first, and a second page follows the cursor without duplicates; another scope's records never appear
- [x] A `pii`-classified actor is masked for a caller without the PII entitlement and unmasked for one with it
- [x] A value recorded while its setting was `pii` stays masked in the history for a caller without the entitlement after the setting is declassified to `public`, and is shown to one with it
- [x] History of a hidden setting returns `404`, and history of a setting for a tenant outside the caller's subtree, or for a standalone descendant, returns `403`
- [x] History of a retired declaration is readable
- [x] A setting with no history returns `200` with an empty page
- [x] A record inserted with no `retain_until` is pruned only after the configured default horizon, and one with an explicit `retain_until` only after that instant
- [x] No code path issues an `UPDATE` against `audit_records`, and the only `DELETE` is pruning
- [x] A record produced under a change set carries its `change_set_id`, and records of one change set are retrievable together
- [x] A history record whose actor's tenant lies on the path from the reader tenant to the record's tenant shows that actor
- [x] A record whose actor sits above the reader tenant, a definition record's actor and a machine act's actor are the mask token with `actor_withheld: true` for a tenant reader
- [x] A platform administrator sees every actor in the history
- [x] A record written before `actor_tenant_id` existed carries `NULL` and its actor is withheld from a tenant reader
- [x] An actor re-parented off the path from the reader tenant to the record's tenant is withheld
- [ ] A history page of records by several users, one of them repeated, makes one lookup pass: one id-set query per (tenant, chunk) over the deduplicated ids, and each record carries the user's name in `actor_name`
- [ ] A masked actor reads `actor_masked` true and `actor_name` `null`, and no lookup is made for it
- [ ] A record written by a contributing module reads `actor_name` `null`, and no lookup is made
- [ ] A record whose actor is the nil id reads `actor_name` `"System"` for an unmasked caller, with no lookup
- [ ] With no `AccountManagementClient` in `ClientHub`, with a client that errors, or with one that outlasts the budget, the history read answers `200` with every user-id actor's `actor_name` `null`, within the budget; a nil-id record still reads `"System"` when unmasked
- [ ] An ancestor reading a descendant scope's history gets the descendant administrators' names, asked in the descendant's tenant, and for a record it wrote in that scope its own name, asked in its own tenant; a lookup that asks the record's tenant for that record fails the scenario
- [ ] A descendant administrator reading a record an ancestor administrator wrote reads `actor_name` `null`: the actor is withheld (`actor_withheld: true`, `actor_masked` true), so no lookup is made for it
- [ ] More than 200 distinct actors on one page are read in chunks of 200, at most 4 at once
- [ ] An all-cache-hit pass records `result=ok` and one histogram sample, with no AM call
- [ ] A pass with a failed chunk records `result=degraded`
- [ ] An actor AM does not list in the tenant it acted from reads `null` and is asked in no other tenant, and a chunk that failed is not retried
- [ ] A page whose actors are all masked or module names records `result=skipped` with no AM call
- [ ] A client slower than the 1 s budget leaves the resolved names set and the rest `null`
- [ ] The name cache holds at most 10,000 names, and a full cache drops its expired names, then all
- [ ] A name is cached per caller: a second read by the same caller makes no lookup, and another subject reads AM itself
- [ ] A refused or absent actor is not cached, so the next read looks up again and shows the name once it becomes visible
- [ ] A context without a real subject makes no AM call, caches nothing, reads every name `null` and counts `result=skipped`
- [ ] The name is the display name, then first and last name, then the username, blank values skipped, and `null` when all are blank
- [ ] A record without a recorded actor tenant — written before the column existed — reads `actor_name` `null`, with no lookup made for it
- [ ] A degraded chunk logs one warn line with the reason kind and the id count only, with no id and no profile text
- [ ] An actor AM refuses or does not list, including a whole chunk AM refuses, reads `null`, records `result=ok` and writes no warn line
- [ ] History p95 is at most 2 s with a healthy AM and with a slow AM whose mock client outlasts the budget; names degrade to `null` and the lookup stays within its budget (`cpt-cf-settings-service-nfr-scale-growth`)
