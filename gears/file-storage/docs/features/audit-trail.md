Created:  2026-07-08 by Constructor Tech
Updated:  2026-07-08 by Constructor Tech
# Feature: Audit Trail

- [ ] `p2` - **ID**: `cpt-cf-file-storage-featstatus-audit-trail-implemented`

> Every in-scope durable file/version mutation this gear performs inserts an
> audit row transactionally into `audit_outbox` — create/finalize, content and
> metadata patch, delete, multipart complete/abort (including the DELETE of the
> session's pending version), retention delete, backend migrate, ownership
> transfer, and orphan reconcile (including the sweep of abandoned pending
> versions). Policy and retention-rule writes (the `policy_service` config
> surface) are not audited. There is nothing downstream of the insert: no
> consumer, exporter, or relay ever reads a row back out and marks it
> `published_at`. See
> [§5 "Outbox Drain to a Downstream Sink (NOT IMPLEMENTED)"](#outbox-drain-to-a-downstream-sink-not-implemented)
> below.



<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Write Operation Emits an Audit Row](#write-operation-emits-an-audit-row)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Build and Persist an Audit Entry Transactionally](#build-and-persist-an-audit-entry-transactionally)
- [4. States (CDSL)](#4-states-cdsl)
  - [Audit Outbox Row Lifecycle](#audit-outbox-row-lifecycle)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Transactional Audit-Entry Insertion on Every Write](#transactional-audit-entry-insertion-on-every-write)
  - [Schema: audit_outbox Table](#schema-audit_outbox-table)
  - [Outbox Drain to a Downstream Sink (NOT IMPLEMENTED)](#outbox-drain-to-a-downstream-sink-not-implemented)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

- [ ] `p2` - `cpt-cf-file-storage-feature-audit-trail`

### 1.1 Overview

A transactional-outbox audit trail: every write mutation that changes a
**file's or version's durable, user-visible state** — create, finalize, bind,
metadata patch, delete (file/version), multipart complete/abort, ownership
transfer, backend migration, and the background cleanup engine's
retention-delete/orphan-reconcile actions — inserts one `AuditEntry` row into
the `audit_outbox` table **in the same DB transaction** as the mutation it
describes. There is no separate "log after the fact" step for any of those
operations, so a rolled-back mutation among them leaves zero audit rows (the
same transaction covers both).

This coverage is not, however, "every mutation in the gear" — a few mutating
code paths are deliberately unaudited today:

- **Transient coordination rows**, not yet a durable file/version state
  change a consumer would care about: a version pre-register's pending-version
  insert, and multipart initiate's session + pending-version insert and
  per-part upserts, write no audit row. These rows are
  provisional — they either get promoted into an already-audited mutation
  (finalize/bind, multipart complete) or are cleaned up
  by the orphan-reconciliation sweep (which *is* audited, as
  `OrphanReconcile`) — so the durable outcome is always covered even though
  the intermediate coordination state is not.
- **Policy and retention-rule writes.** Upserting a policy or inserting/deleting
  a retention rule mutates tenant/user policy and retention-rule rows with no
  audit row. The PRD's
  audit-trail requirement (`cpt-cf-file-storage-fr-audit-trail`) scopes
  audit records to file write operations (upload, content replacement,
  delete, metadata update); policy and retention-rule administration is a
  separate, unaudited surface. These are still durable configuration changes
  with real compliance relevance (they alter what a *future* write is allowed
  to do), so the absence of an audit row here is a real limitation worth
  knowing about even though it sits outside this requirement's stated scope.

This feature has **no REST endpoint of its own** — it is a pure side effect of
other features' write paths. The only way to read `audit_outbox` rows today is
a direct SQL query or a test-only internal accessor; there is no
`GET /files/{id}/audit` route or equivalent exposed by this gear.

**Traces to**: `cpt-cf-file-storage-fr-audit-trail`, `cpt-cf-file-storage-nfr-audit-completeness`

### 1.2 Purpose

Give the platform a complete, tamper-evident-by-construction record of every
write this gear performs, for compliance and forensic purposes, with a
correctness guarantee stronger than "best-effort logging": because the audit
row is written in the *same* database transaction as the mutation, there is no
window in which a mutation commits without its audit row, or an audit row
exists for a mutation that was rolled back. This is the transactional-outbox
pattern applied to compliance logging rather than to event delivery (the
sibling `events_outbox` table applies the identical pattern to file lifecycle
events — see `cpt-cf-file-storage-fr-file-events`).

**Requirements**: `cpt-cf-file-storage-fr-audit-trail`, `cpt-cf-file-storage-nfr-audit-completeness`

**Principles**: `cpt-cf-file-storage-principle-control-no-content` (audit rows carry
only metadata/identifiers in `detail`, never content bytes)

> **Caveat: outbox drain/relay is not implemented.** `audit_outbox.published_at`
> is written as `NULL` on every insert, and nothing in
> this codebase ever sets it. No relay drains `audit_outbox` (or its sibling
> `events_outbox`) to a downstream platform sink. Concretely this means: (a)
> rows accumulate in `audit_outbox` indefinitely with no retention or archival
> process; (b) the background cleanup sweep's idempotency-key-expiry step
> *deliberately* does **not** touch
> `audit_outbox`/`events_outbox` — a row-age-based purge would silently drop rows that were never delivered, since
> `published_at` can never become non-`NULL` today; (c) there is no way for any
> downstream consumer to actually receive these audit events short of a direct
> database read. The write-side guarantee (100% coverage, same-transaction
> atomicity) is real and tested; nothing reads the outbox back out and
> delivers it anywhere.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-file-storage-actor-platform-user` | Performs a write operation (create/finalize/bind/patch/delete/transfer/…); the audit row is an automatic, non-optional side effect of their request, not something they explicitly request |
| `cpt-cf-file-storage-actor-cf-gears` | Peer gear / service acting as `actor_kind = "app"`; subject to the identical audit coverage as a human user |

The background cleanup engine (`cpt-cf-file-storage-fr-orphan-reconciliation`,
`cpt-cf-file-storage-fr-retention-policies`) also writes audit rows for its own
sweep-triggered deletions, using a synthetic `actor_kind = "system"`,
nil-UUID actor identity rather than either actor above — there is no
human or peer-gear caller to attribute those rows to. Both the `OrphanReconcile`
and `RetentionDelete` rows carry the
real file's `tenant_id`, resolved from the orphan candidate's or expiring file's
own row at delete time (falling back to a nil tenant id only when the file row
is already gone by the time the sweep reaches it — i.e. only when there is
genuinely no real tenant left to attribute the row to).

A fourth synthetic identity, `actor_kind = "sidecar"` with a nil actor id, is
used for the token-authenticated single-part finalize callback's
`FinalizeVersion` audit row — that callback carries no platform security
context to derive `"app"`/`"user"` from. The sidecar's sibling report-part
callback writes no audit row at all: it only records a provisional per-part
upload upsert, one of the transient coordination writes described in §1.1
above, folded into an audited mutation only once multipart `complete` runs.

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md)
- **Dependencies**: none — the audit trail is a cross-cutting concern threaded
  through every other feature's write path (multipart coordinator, ownership
  transfer, backend migration, retention/cleanup, and the P1 single-shot
  upload/bind/metadata/delete foundation), rather than depending on any one of
  them
- **Related**: `cpt-cf-file-storage-fr-file-events` (the sibling `events_outbox`
  table, same transactional-outbox pattern, same undrained-relay caveat)

## 2. Actor Flows (CDSL)

This feature introduces no new endpoint and no actor-initiated journey of its
own (`ARCH-FDESIGN-NO-002`-style: it rides along inside other features'
flows). The one flow below documents the side effect common to all of them,
from the audited operation's point of view.

### Write Operation Emits an Audit Row

- [x] `p1` - **ID**: `cpt-cf-file-storage-flow-audit-trail-record-write`

**Actor**: `cpt-cf-file-storage-actor-platform-user` (or `cpt-cf-file-storage-actor-cf-gears`)

**Success Scenarios**:
- The actor's write request (create, finalize, bind, metadata patch, delete,
  multipart complete/abort, ownership transfer, backend migration) succeeds;
  exactly one audit row describing it is committed in the same transaction,
  with `outcome = success`

**Error Scenarios**:
- The mutation's own precondition fails (e.g. a stale `If-Match`/CAS version) —
  the entire transaction, including the would-be audit row, rolls back; **no**
  audit row is left behind for the failed attempt
- The mutation's CAS predicate finds no matching row at all (e.g. ownership
  transfer racing a concurrent delete) — again no audit row, no event

**Steps**:
1. [x] - `p1` - Actor: issues a write request through any audited operation's normal API path - `inst-audit-actor-request`
2. [x] - `p1` - Service layer: builds an `AuditEntry` (or, for the cleanup engine, a synthetic `system`-actor entry built inline) using `cpt-cf-file-storage-algo-audit-trail-build-entry` - `inst-audit-build`
3. [x] - `p1` - Service layer: passes the `AuditEntry` into the same store/repo call that performs the mutation (e.g. ownership transfer, backend rebind, finalize, bind, version delete, file delete, metadata update) - `inst-audit-pass-through`
4. [x] - `p1` - Store: opens (or reuses) one DB transaction; performs the mutation's own writes, then inserts the audit row inside that **same** transaction - `inst-audit-insert-same-tx`
5. [x] - `p1` - DB: commits both the mutation and the audit row together, or rolls back both together on any failure - `inst-audit-commit-or-rollback`
6. [x] - `p1` - **RETURN** the mutation's normal response to the actor; the audit row is invisible in that response (no audit-related fields in any success payload) - `inst-audit-return`

## 3. Processes / Business Logic (CDSL)

### Build and Persist an Audit Entry Transactionally

- [x] `p1` - **ID**: `cpt-cf-file-storage-algo-audit-trail-build-entry`

**Input**: `SecurityContext` (or a synthetic `system` identity for background
sweeps), an optional `file_id`, an `AuditOperation` variant, a JSON `detail`
payload

**Output**: an `AuditEntry` value, later persisted as one row in
`audit_outbox`

**Steps**:
1. [x] - `p1` - Extract `tenant_id`/`actor_id` from the caller's security context, or use a nil actor id for a background-sweep-originated entry - `inst-buildentry-identity`
2. [x] - `p1` - Compute `actor_kind`: `"app"` for an app-principal caller, else `"user"`; `"system"` for the cleanup engine's own entries; `"sidecar"` (with a nil actor id) for the single-part token-authenticated finalize callback, which has no security context to derive an actor from - `inst-buildentry-actor-kind`
3. [x] - `p1` - Select the `AuditOperation` variant matching the mutation (`Create`, `PatchContent`, `PatchMetadata`, `DeleteFile`, `DeleteVersion`, `MultipartComplete`, `MultipartAbort`, `FinalizeVersion`, `RetentionDelete`, `BackendMigrate`, `OrphanReconcile`, `TransferOwnership`) - `inst-buildentry-operation`
4. [x] - `p1` - Build a `detail` JSON object with operation-specific identifiers (e.g. `version_id`, `from_backend`/`to_backend`, `from_owner_id`/`to_owner_id`) — never content bytes - `inst-buildentry-detail`
5. [x] - `p1` - Construct the `AuditEntry` as a success record (the equivalent failure-record constructor is defined but never used by any call site — every call site only ever records successes, since a failed mutation's transaction rolls back before an audit row would matter) with the current timestamp - `inst-buildentry-construct`
6. [x] - `p1` - Persist the entry as a new row (a fresh event id, `published_at = NULL`); the table has no tenant-scoping enforced at the storage layer, so `tenant_id` is a plain data column the application populates from the caller's context - `inst-buildentry-insert`
7. [x] - `p1` - **RETURN** control to the caller once the surrounding transaction commits - `inst-buildentry-return`

## 4. States (CDSL)

### Audit Outbox Row Lifecycle

- [ ] `p2` - **ID**: `cpt-cf-file-storage-state-audit-outbox-row`

**States**: unpublished, published

**Initial State**: unpublished (`published_at IS NULL`)

**Transitions**:
1. [ ] - `p2` - **FROM** unpublished **TO** published **WHEN** a downstream drain/relay process reads the row and marks `published_at` — **this transition never fires; no drain process exists** - `inst-st-audit-never-published`

Every `audit_outbox` row today is permanently `unpublished`. The
`published_at` column and its supporting index
(`audit_outbox_unpublished_idx ... WHERE published_at IS NULL`) were added in
anticipation of a drain process that does not exist yet; they are inert schema
today, not dead weight to be removed, since a future consumer would implement
against this exact shape.

## 5. Definitions of Done

### Transactional Audit-Entry Insertion on Every Write

- [x] `p1` - **ID**: `cpt-cf-file-storage-dod-audit-trail-transactional-write`

The system **MUST** insert exactly one `audit_outbox` row, in the same DB
transaction as the mutation, for every write operation: `create_file`,
`finalize_upload`/`finalize_upload_by_token`, `bind`, `update_metadata`,
`delete_file`, `delete_version`, `complete_multipart_upload`,
`abort_multipart_upload`, `transfer_ownership`, `migrate_backend`, and the
cleanup engine's `RetentionDelete`/`OrphanReconcile`/expired-multipart-session
`MultipartAbort` deletions. A rolled-back mutation (failed CAS/`If-Match`,
CAS predicate matching zero rows) **MUST** leave zero new audit rows.

**Implements**:
- `cpt-cf-file-storage-flow-audit-trail-record-write`
- `cpt-cf-file-storage-algo-audit-trail-build-entry`

**Touches**:
- Gears: every domain write-path service module (single-shot upload/bind/metadata/delete, multipart, backend
  migration, ownership transfer, the cleanup sweep)
- DB Table: `audit_outbox`

### Schema: audit_outbox Table

- [x] `p2` - **ID**: `cpt-cf-file-storage-dod-audit-trail-schema`

The system **MUST** provide an `audit_outbox` table
(`event_id` uuid PK, `tenant_id`, `actor_kind`, `actor_id`, `file_id`
nullable, `operation`, `outcome`, `detail` json, `occurred_at`,
`published_at` nullable), plus an
index `audit_outbox_unpublished_idx` scoped to `published_at IS NULL` for the
(not-yet-implemented) drain process's eventual query pattern. The table
carries **no tenant-scoping enforcement at the storage layer** — application
code is responsible for populating `tenant_id` correctly.

**Implements**:
- `cpt-cf-file-storage-algo-audit-trail-build-entry`

**Touches**:
- DB Table: `audit_outbox`

### Outbox Drain to a Downstream Sink (NOT IMPLEMENTED)

- [ ] `p2` - **ID**: `cpt-cf-file-storage-dod-audit-trail-relay`

**NOT IMPLEMENTED.**

The system **SHOULD** eventually drain unpublished `audit_outbox` rows to a
platform audit sink (the same relay that would also drain `events_outbox`),
marking `published_at` on successful delivery. **None of this exists today.**
Nothing in this gear reads `audit_outbox` back out except test-only internal
accessors and ad hoc SQL. This
DoD line stays unchecked so the gap remains an explicit, acknowledged
limitation rather than silently assumed done because the write side is fully
tested.

**Implements**: (nothing yet — this is the open item)

**Touches**:
- DB Table: `audit_outbox` (read side, not yet built)
- Gears: a future relay/drain component (not yet designed)

## 6. Acceptance Criteria

- [x] Creating a file leaves exactly one `create` audit row
- [x] Finalizing an upload leaves exactly one `finalize_version` audit row
- [x] Binding a version as current leaves exactly one `patch_content` audit row
- [x] Updating metadata leaves exactly one `patch_metadata` audit row
- [x] Deleting a file leaves exactly one `delete_file` audit row
- [x] Deleting a version leaves exactly one `delete_version` audit row
- [x] Completing a multipart upload leaves exactly one `multipart_complete` row and exactly one `finalize_version` row
- [x] A failed metadata CAS (stale metadata revision) leaves **no** new audit row — proves the same-transaction atomicity guarantee
- [x] A failed bind (stale `If-Match`) leaves **no** new audit row
- [x] Transferring ownership leaves exactly one `transfer_ownership` audit row; a CAS-losing transfer (target row not found) leaves **no** audit row and **no** file event
- [x] Migrating a file's backend leaves at least one `backend_migrate` audit row on a real migration, and **zero** when the migration is a same-backend no-op
- [x] The cleanup engine's retention-expiry sweep leaves a `retention_delete` audit row per expired file, and its abandoned-pending-version reclamation leaves an `orphan_reconcile` row
- [ ] `audit_outbox` rows are drained/relayed to a downstream platform audit sink — **NOT IMPLEMENTED**; `published_at` is written `NULL` on every insert and never updated by any code path in this repository (see the caveat in §1.2 and the DoD in §5)
- [ ] The audit trail is queryable through this gear's own REST API — **NOT IMPLEMENTED**; there is no `GET`-style audit endpoint, only direct SQL / test-only repo methods
