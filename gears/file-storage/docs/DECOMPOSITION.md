Created:  2026-07-02 by Constructor Tech
Updated:  2026-07-02 by Constructor Tech

# Decomposition: File Storage

**Overall implementation status:**
- [ ] `p1` - **ID**: `cpt-cf-file-storage-status-overall`



<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 Multipart Upload Coordinator - HIGH](#21-multipart-upload-coordinator---high)
  - [2.2 Content-Hash Modes - MEDIUM](#22-content-hash-modes---medium)
  - [2.3 Policy Engine - HIGH](#23-policy-engine---high)
  - [2.4 Retention Rules & Cleanup Sweep - MEDIUM](#24-retention-rules--cleanup-sweep---medium)
  - [2.5 Audit Trail - HIGH](#25-audit-trail---high)
  - [2.6 Ownership Transfer - MEDIUM](#26-ownership-transfer---medium)
  - [2.7 Backend Migration - MEDIUM](#27-backend-migration---medium)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

## 1. Overview

This document originally decomposed the P2 release cycle into a single feature — the server-authoritative multipart
upload path — understating what actually shipped in P2. Beyond multipart, the P2 branch also delivered: the **policy
engine** (allowed-types / size / custom-metadata limits at tenant and user scope, §2.3), **retention rules + a
background cleanup sweep** (whole-file retention pruning and orphan reconciliation, §2.4), an **audit outbox**
(transactional write-operation audit trail, §2.5), an **events outbox** (file lifecycle events, not yet drained to
the platform EventBroker — Tier 4 item 4.1 in the P2 remediation plan), **ownership transfer** (§2.6), and **backend
migration** (§2.7). As of this revision (P2 remediation item 3.6), all five now have their own DECOMPOSITION entry
and FEATURE artifact under `docs/features/`, matching `features/multipart-coordinator.md`'s structure. A further
entry, §2.2, decomposes a P2+ design — content-hash modes — which, unlike the other entries, was **proposed** in
ADR-0006 ahead of its own implementation and has since shipped alongside the rest of P2 (see
[features/content-hash-modes.md](features/content-hash-modes.md)'s "implemented" status).

**Decomposition Strategy**:

- Only the multipart upload lifecycle (initiate, upload-part via sidecar, complete, abort, and introspect/resume) has
  a dedicated, **shipped** FEATURE decomposition entry (§2.1) and artifact, control-plane/sidecar split per ADR-0003
  and ADR-0004.
- The content-hash-modes decision (§2.2) was decomposed as a **proposed** FEATURE ahead of implementation —
  formalized in ADR-0006 (now `status: accepted`) — covering the two-mode SHA-256 hashing design (whole-object for
  non-multipart, offset-manifest composite for multipart); it has since shipped.
- The policy engine (§2.3), retention-cleanup (§2.4), audit-trail (§2.5), ownership-transfer (§2.6), and
  backend-migration (§2.7) subsystems are real, shipped P2 scope, now decomposed into their own entries and FEATURE
  artifacts (P2 remediation plan item 3.6), given in particular the compliance weight of the audit-trail and
  ownership-transfer requirements (`cpt-cf-file-storage-fr-audit-trail`, `cpt-cf-file-storage-fr-ownership-transfer`).
  Two of the five (audit-trail, ownership-transfer) carry explicit, tracked partial-implementation caveats — see
  their own FEATURE docs' status lines — rather than being presented as fully closed.
- The multipart feature depends on the P1 upload and versioning foundation (single-shot upload, file_versions table,
  signed-URL infrastructure) already shipped in P1; those P1 capabilities are not re-decomposed here. The
  content-hash-modes feature additionally depends on the multipart feature's part-hash/offset plumbing (§3). The
  policy engine is a dependency of both the multipart-initiate flow (MIME/size checks) and the P1 single-shot upload
  path; backend-migration depends on content-hash-modes for its mode-aware pre-commit verification; audit-trail is a
  cross-cutting dependency of every other entry's write path (§3).
- No shared components or DB tables are introduced by the multipart feature beyond the multipart_uploads and
  multipart_upload_parts tables it owns. The content-hash-modes feature added one table, `version_hash_manifest`.
  The other P2 subsystems own their own tables (`policies`, `retention_rules`, `audit_outbox`, `events_outbox`; see
  `docs/DESIGN.md` §3.7 and the gear's migrations) — ownership-transfer and backend-migration introduce no new tables
  of their own, only new mutation paths over `files`/`file_versions`.

**Deferred items** (carried over from the now-deleted temporary planning docs; re-audited against current code —
everything else those docs once tracked as open has since shipped):

- **`files.content_id → file_versions(version_id)` FK (`ON DELETE RESTRICT`)** — deliberately not implemented.
  `files.content_id` has no DB-level foreign key today; the active protection is the predicate guard in
  `VersionRepo::delete` (refuses to delete a row with `is_current = true`) plus `delete_version`'s
  zero-rows-affected → conflict handling. A real FK is Postgres-only (SQLite cannot `ALTER TABLE … ADD CONSTRAINT`),
  this gear's test suite runs entirely on SQLite (no Postgres-backed test harness in CI), and the FK would form a
  circular reference with `file_versions.file_id → files ON DELETE CASCADE` that needs verification against a real
  Postgres flow before shipping. The prerequisite unique index (`file_versions_version_id_unique_idx`, needed for a
  single-column FK target) already exists (shipped with the ADR-0006 migration). Re-open once a Postgres-backed test
  harness exists.

## 2. Entries

### 2.1 [Multipart Upload Coordinator](features/multipart-coordinator.md) - HIGH

- [ ] `p2` - **ID**: `cpt-cf-file-storage-feature-multipart-coordinator`

- **Type**: Core
- **Phases**: Single-phase implementation

- **Purpose**: Provide a safe, resumable, server-controlled multipart upload path. The client declares total size and a preferred part size; the control plane computes the exact parts plan and returns one signed sidecar URL per part. The sidecar enforces the per-part size claim before writing. The control plane assembles and hashes the parts at complete and finalizes the new file version; binding it as the file's current content remains a separate, client-issued request.

- **Depends On**: P1 file upload and versioning foundation (single-shot upload, file_versions table, signed-URL infrastructure -- codec-equivalent Ed25519, not literal PASETO, see ADR-0004's Implementation note -- not a formal DECOMPOSITION feature)

- **Scope**:
  - `POST /api/file-storage/v1/files/{id}/multipart` -- initiate: validate MIME/size/quota, compute parts plan, mint signed sidecar URLs, pre-register pending version
  - Sidecar part-upload handler: verify the signed token, enforce per-part size claim (HTTP 413), write part bytes, compute the per-part hash, and report it to the control plane over a token-authenticated callback (the sidecar has no DB connection of its own)
  - `POST .../multipart/{upload_id}/complete` -- verify assembled size against declared_size, assemble + hash the parts, **finalize** the version (`pending -> available`); does **not** bind (`content_id` untouched) and does **not** accept `If-Match` -- see [features/multipart-coordinator.md](features/multipart-coordinator.md) for the tracked gap between this and the richer `If-Match`/`200`-body/missing-parts contract originally specified
  - `DELETE .../multipart/{upload_id}` -- abort: mark session aborted, delete part rows and pending version, abort backend handle for multipart_native backends
  - `GET .../multipart/{upload_id}` -- introspect/resume (p2): return plan recomputed from persisted columns, re-issue fresh signed URLs for missing parts
  - DB migration: add version_id, declared_size, part_size columns to multipart_uploads table

- **Out of scope**:
  - Single-shot upload path (owned by P1 foundation)
  - File download, listing, metadata update, or delete (owned by P1 foundation)
  - Storage quota ledger management (quota is read and *checked* here via the `QuotaClient` port — see
    implementation-status note below; the check itself is a no-op because no client is wired; ledger
    updates owned by P1 foundation)

- **Requirements Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-fr-multipart-upload`
  - [ ] `p2` - `cpt-cf-file-storage-fr-size-limits-policy`
  - [ ] `p2` - `cpt-cf-file-storage-fr-storage-quota` — **implementation status (P2)**: the
    `check_quota_bytes` call site exists in `multipart_service.rs`, but `gear.rs` wires `quota_client: None`
    (Tier 1 item 1.4), so no quota is actually enforced on multipart initiate — permissive/fail-open, blocked
    on a Quota Enforcement SDK crate (`gears/system/quota-enforcement/` is docs-only)

- **Design Principles Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-principle-control-no-content`
  - [ ] `p2` - `cpt-cf-file-storage-principle-signed-urls`

- **Design Constraints Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-constraint-sidecar`
  - [ ] `p2` - `cpt-cf-file-storage-constraint-postgres`

- **Domain Model Entities**:
  - MultipartUpload (session)
  - MultipartUploadPart

- **API**:
  - `POST /api/file-storage/v1/files/{id}/multipart` -- initiate multipart upload
  - `PUT <sidecar signed part URL>` -- upload a single part (sidecar, not control plane)
  - `POST /api/file-storage/v1/files/{id}/multipart/{upload_id}/complete` -- complete upload
  - `DELETE /api/file-storage/v1/files/{id}/multipart/{upload_id}` -- abort upload
  - `GET /api/file-storage/v1/files/{id}/multipart/{upload_id}` -- introspect/resume (p2)

- **Sequences**:

  - None (flow documented inline in `cpt-cf-file-storage-flow-multipart-initiate`, `cpt-cf-file-storage-flow-multipart-upload-part`, `cpt-cf-file-storage-flow-multipart-complete`, `cpt-cf-file-storage-flow-multipart-abort`)

- **Data**:

  - None (tables multipart_uploads and multipart_upload_parts are created by the P1 foundation migration; this feature extends multipart_uploads via migration m20260701_000002_multipart_plan_columns)


### 2.2 [Content-Hash Modes](features/content-hash-modes.md) - MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-file-storage-feature-content-hash-modes`

- **Type**: Core
- **Phases**: Staged implementation (see [features/content-hash-modes.md](features/content-hash-modes.md) §5/§7 -- groundwork, schema migration, multipart-composite-sha256 implementation, docs) -- all stages shipped

- **Status**: **Implemented.** Formalized in [ADR-0006](ADR/0006-cpt-cf-file-storage-adr-content-hash-modes.md) (`status: accepted`). This entry was originally decomposed here as a proposal ahead of implementation; it has since shipped alongside the rest of P2 -- `complete_multipart` builds the offset-manifest composite from already-collected per-part hashes with no re-read of the assembled object, and `migrate_backend` (§2.7) verifies mode-awarely.

- **Purpose**: Replace the single implicit whole-object-SHA-256 hashing shape with exactly two explicit, mode-tagged content-hash modes -- non-multipart whole-object SHA-256 (unchanged) and multipart SHA-256 offset-manifest composite (new) -- computed on-the-fly during upload with no re-read of the stored object, and independently client-verifiable from the object bytes plus a small, durable manifest.

- **Depends On**: `cpt-cf-file-storage-feature-multipart-coordinator` (this feature consumes the multipart plan's per-part offsets and the already-persisted `multipart_upload_parts.part_hash` values; it does not change that feature's endpoints or session lifecycle)

- **Scope**:
  - `HashMode`/`ManifestEntry`/`Manifest` types and the manifest wire-format codec (`to_wire_string`/`from_wire_string`)
  - Schema migration: `file_versions.hash_mode`/`part_count` columns, new `version_hash_manifest` table
  - `StorageBackend::upload_part`/`complete_multipart` trait signature changes so multipart completion builds the manifest/root from already-collected per-part hashes and offsets instead of re-reading the assembled object
  - Mode-aware `Store::verify_content_hash` and `migrate_backend` re-verification
  - Additive `hash_mode`/`part_count`/`manifest` fields in metadata and multipart-complete API responses

- **Out of scope**:
  - Any second hash algorithm, per-request hash-mode preference, or capability-discovery endpoint (ADR-0002's P2 `hash_policy`/`selection_rules` vision -- dropped entirely, not deferred, since SHA-256 is the only algorithm for both modes)
  - Changes to the multipart session state machine or any multipart endpoint's method/path/request shape (owned by `cpt-cf-file-storage-feature-multipart-coordinator`)
  - Cross-mode or cross-split-choice content deduplication (an accepted, documented trade-off -- see [features/content-hash-modes.md](features/content-hash-modes.md) §7 "12. Risks & open decisions")

- **Requirements Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-fr-multipart-upload`
  - [ ] `p2` - `cpt-cf-file-storage-fr-metadata-storage`
  - [ ] `p1` - `cpt-cf-file-storage-fr-get-metadata`

- **Design Principles Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-principle-streaming`
  - [ ] `p2` - `cpt-cf-file-storage-principle-control-no-content`

- **Design Constraints Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-constraint-postgres`

- **Domain Model Entities**:
  - HashMode (enum)
  - Manifest / ManifestEntry

- **API**:
  - `POST /api/file-storage/v1/files/{id}/multipart/{upload_id}/complete` -- response fields only (`hash_mode`, `part_count`, `manifest`); method/path unchanged

- **Sequences**:

  - None (flow documented inline in `cpt-cf-file-storage-flow-content-hash-modes-client-reverify`)

- **Data**:

  - New table `version_hash_manifest` (`version_id` PK/FK into `file_versions`, `manifest text`, `created_at`); `file_versions` gains `hash_mode`/`part_count` columns -- both shipped via migration


### 2.3 [Policy Engine](features/policy-engine.md) - HIGH

- [ ] `p2` - **ID**: `cpt-cf-file-storage-feature-policy-engine`

- **Type**: Core
- **Phases**: Single-phase implementation

- **Purpose**: Let tenants and individual users define allowed-MIME-type, size-limit, and custom-metadata-limit
  policies at two scopes (tenant, user); resolve the two levels into one effective policy per request with a
  most-restrictive-wins rule per aspect; enforce the resolved policy on every storage-increasing write (single-shot
  upload, multipart initiate/complete, metadata update).

- **Depends On**: none beyond the P1 upload/versioning foundation, whose write paths (`create_file`,
  `presign_version`, `update_metadata`) call into `PolicyResolver`'s enforcement helpers

- **Scope**:
  - `PolicyBody`/`SizeLimits`/`MimeSizeOverride`/`MetadataLimits` domain types and the `PolicyResolver`
    most-restrictive-wins merge algorithm (`src/domain/policy.rs`)
  - `GET`/`PUT /policy` (tenant or user scope) and `GET /policy/effective` (the resolved effective policy for the
    caller's context)
  - Enforcement call sites: allowed-MIME check, effective size-limit check, metadata-limit check, wired into
    `domain/service/create.rs` and the multipart-initiate path

- **Out of scope**:
  - Storage quota enforcement (a related but separate control -- `cpt-cf-file-storage-fr-storage-quota`, not
    enforced in any deployment today, see [multipart-coordinator.md](features/multipart-coordinator.md)'s quota
    caveat)
  - Retention policies (a distinct policy *type*, owned by §2.4 despite living in the same `policy.rs` module and
    sharing the tenant/user/file scope model)

- **Requirements Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-fr-allowed-types-policy`
  - [ ] `p2` - `cpt-cf-file-storage-fr-size-limits-policy`
  - [ ] `p2` - `cpt-cf-file-storage-fr-metadata-limits`

- **Design Principles Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-principle-control-no-content`

- **Design Constraints Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-constraint-postgres`

- **Domain Model Entities**:
  - StoredPolicy
  - EffectivePolicy

- **API**:
  - `GET /api/file-storage/v1/policy` -- read a policy (tenant or user scope)
  - `PUT /api/file-storage/v1/policy` -- upsert a policy
  - `GET /api/file-storage/v1/policy/effective` -- resolved effective policy for the caller

- **Sequences**:

  - None (resolution documented inline in `src/domain/policy.rs::PolicyResolver::resolve`)

- **Data**:

  - Table `policies` (`tenant_id`, `scope`, `scope_owner_id`, `body` json), with partial unique indexes enforcing at
    most one row per `(tenant_id, 'tenant')` and per `(tenant_id, 'user', scope_owner_id)` (P2 remediation item 2.4)


### 2.4 [Retention Rules & Cleanup Sweep](features/retention-cleanup.md) - MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-file-storage-feature-retention-cleanup`

- **Type**: Core
- **Phases**: Single-phase implementation

- **Purpose**: Let tenants define retention rules (age-, inactivity-, or metadata-based expiry, OR semantics across
  criteria) at tenant/user/file scope, and run a background sweep that (a) prunes files matching an expired
  retention rule, (b) reclaims abandoned pending versions and expired multipart sessions past a grace window, and
  (c) deletes expired idempotency-key rows -- each step best-effort and independently idempotent across concurrent
  sweep instances.

- **Depends On**: [Audit Trail](features/audit-trail.md) (every sweep-triggered deletion writes a `RetentionDelete`
  or `OrphanReconcile` audit row through the same transactional-outbox mechanism)

- **Scope**:
  - `RetentionRuleBody`/`AgeRetention`/`InactivityRetention`/`MetadataRetention` domain types (`src/domain/policy.rs`)
  - `GET`/`POST /retention-rules`, `DELETE /retention-rules/{rule_id}`
  - `CleanupEngine::run_sweep` (`src/domain/cleanup.rs`): abandoned-pending-version reclamation (skips versions still
    backing a live in-progress multipart session, P2 remediation 2.8), expired-multipart-session abort,
    retention-policy expiry (keyset-paginated file scan), expired idempotency-key purge
  - Per-instance sweep scheduling; no cross-instance coordination in P2 (deferred to P3)

- **Out of scope**:
  - Draining `audit_outbox`/`events_outbox` -- the sweep deliberately does **not** purge these tables, since
    `published_at` can never become non-null until the Tier-4 EventBroker relay exists (see
    [audit-trail.md](features/audit-trail.md))
  - Cross-instance leader election / distributed locking (deferred to P3)

- **Requirements Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-fr-retention-policies`
  - [ ] `p2` - `cpt-cf-file-storage-fr-orphan-reconciliation`

- **Design Principles Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-principle-control-no-content`

- **Design Constraints Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-constraint-postgres`

- **Domain Model Entities**:
  - StoredRetentionRule
  - SweepResult (tally, not persisted)

- **API**:
  - `GET /api/file-storage/v1/retention-rules` -- list retention rules
  - `POST /api/file-storage/v1/retention-rules` -- create a retention rule
  - `DELETE /api/file-storage/v1/retention-rules/{rule_id}` -- delete a retention rule

- **Sequences**:

  - None (sweep order documented inline in `src/domain/cleanup.rs::CleanupEngine::run_sweep`)

- **Data**:

  - Table `retention_rules` (`tenant_id`, `scope`, `scope_target_id`, `body` json)


### 2.5 [Audit Trail](features/audit-trail.md) - HIGH

- [ ] `p2` - **ID**: `cpt-cf-file-storage-feature-audit-trail`

- **Type**: Core
- **Phases**: Single-phase implementation (write side); drain/relay phase not started

- **Status**: **PARTIAL.** The write side -- one `audit_outbox` row per write mutation, in the same DB transaction
  as the mutation -- is fully shipped and tested. The drain/relay side (delivering those rows to a downstream audit
  sink) is **not implemented** -- `published_at` is written `NULL` on every insert and never updated by any code
  path in this repository (Tier-4 item 4.1, no target date). See [features/audit-trail.md](features/audit-trail.md).

- **Purpose**: Give the platform a transactionally-guaranteed, tamper-evident-by-construction record of every write
  this gear performs, for compliance and forensic purposes -- a mutation and its audit row always commit or roll
  back together.

- **Depends On**: none -- this is a cross-cutting concern threaded through every other feature's write path rather
  than depending on any one of them

- **Scope**:
  - `AuditEntry`/`AuditOperation`/`AuditOutcome` domain types (`src/domain/audit.rs`)
  - `AuditRepo::insert`, called inside the same transaction as every audited mutation across
    `domain/service/{write,create,read_ops,backend}.rs`, `domain/multipart_service.rs`, and `domain/cleanup.rs`

- **Out of scope**:
  - Draining/relaying `audit_outbox` rows to any downstream sink -- **not implemented** (Tier-4 item 4.1)
  - Any REST endpoint for reading the audit trail back -- none exists; rows are only readable via direct SQL or the
    test-only `Store::list_audit`/`AuditRepo::list_for_file`

- **Requirements Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-fr-audit-trail` -- write-side shipped; see the PARTIAL status above for the
    drain/relay gap
  - [ ] `p2` - `cpt-cf-file-storage-nfr-audit-completeness`

- **Design Principles Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-principle-control-no-content`

- **Design Constraints Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-constraint-postgres`

- **Domain Model Entities**:
  - AuditEntry
  - AuditOperation (enum, 12 variants)

- **API**:
  - None -- no dedicated audit-trail endpoint; this feature is a side effect of every other feature's write API

- **Sequences**:

  - None (flow documented inline in `cpt-cf-file-storage-flow-audit-trail-record-write`)

- **Data**:

  - Table `audit_outbox` (`event_id`, `tenant_id`, `actor_kind`, `actor_id`, `file_id`, `operation`, `outcome`,
    `detail` json, `occurred_at`, `published_at` nullable), plus its `audit_outbox_unpublished_idx` index


### 2.6 [Ownership Transfer](features/ownership-transfer.md) - MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-file-storage-feature-ownership-transfer`

- **Type**: Core
- **Phases**: Single-phase implementation

- **Status**: **PARTIAL.** The endpoint, atomic owner swap, audit row, file event, and usage-delta reporting are
  fully shipped and tested. Target-owner validation is **PARTIAL** -- only the nil-UUID sentinel is rejected; full
  existence/tenant-membership validation is blocked on an account-management SDK that does not exist yet (P2
  remediation item 2.12). See [features/ownership-transfer.md](features/ownership-transfer.md).

- **Purpose**: Let a file's owner change without recreating the file or losing its `file_id`/version
  history/metadata, atomically alongside an audit row and a `file.owner_transferred` event.

- **Depends On**: [Audit Trail](features/audit-trail.md) (the `TransferOwnership` audit row); the file-events
  outbox (`cpt-cf-file-storage-fr-file-events`); usage reporting (`cpt-cf-file-storage-fr-usage-reporting`)

- **Scope**:
  - `POST /files/{id}/transfer`: nil-UUID rejection, atomic `owner_kind`/`owner_id` swap, audit row, file event,
    post-commit usage-delta debit/credit (`src/domain/service/write.rs::transfer_ownership`)

- **Out of scope**:
  - Full target-owner existence/tenant-membership validation -- **PARTIAL / NOT IMPLEMENTED**, blocked on an
    account-management SDK (item 2.12)
  - A distinct privileged-transfer authorization grant (currently reuses the file's ordinary `WRITE` grant) -- open,
    tied to item 0.7's admin-scope work

- **Requirements Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-fr-ownership-transfer` -- PARTIAL, see the status note above

- **Design Principles Covered**:

  - None specific to this feature beyond the general audit/event guarantees it composes

- **Design Constraints Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-constraint-postgres`

- **Domain Model Entities**:
  - None new -- mutates the existing `File` entity's `owner_kind`/`owner_id` fields

- **API**:
  - `POST /api/file-storage/v1/files/{id}/transfer` -- transfer ownership

- **Sequences**:

  - None (flow documented inline in `cpt-cf-file-storage-flow-ownership-transfer`)

- **Data**:

  - None new (mutates the existing `files` table; writes to the existing `audit_outbox`/`events_outbox` tables)


### 2.7 [Backend Migration](features/backend-migration.md) - MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-file-storage-feature-backend-migration`

- **Type**: Core
- **Phases**: Single-phase implementation

- **Purpose**: Relocate a non-versioned file's content from one storage backend to another without changing its
  identity, verifying the content hash (mode-aware per ADR-0006) before committing the new backend binding, with a
  CAS-guarded pointer swap that safely resolves concurrent-migration races.

- **Depends On**: [Content-Hash Modes](features/content-hash-modes.md) (mode-aware pre-commit verification, §2.2);
  [Audit Trail](features/audit-trail.md) (the `BackendMigrate` audit row)

- **Scope**:
  - `POST /files/{id}/migrate`: single-version-only guard, non-durable-target admin gate, source read + mode-aware
    hash verify + destination write, CAS-guarded version-row rebind, concurrent-migration race resolution,
    best-effort source cleanup (`src/domain/service/backend.rs::migrate_backend`)

- **Out of scope**:
  - Versioned files (more than 1 version) -- migration is restricted to non-versioned files by design, a permanent
    scope boundary, not a tracked gap
  - Bulk/background migration tooling (this is a single-file, synchronous, caller-initiated operation only)

- **Requirements Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-fr-backend-migration`

- **Design Principles Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-principle-control-no-content` -- with the caveat that this feature is an
    operator/admin path that reads content through the control plane's own process, not a signed sidecar URL
    (ADR-0003's sidecar-only rule targets regular user upload/download, not this admin-initiated relocation)

- **Design Constraints Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-constraint-postgres`

- **Domain Model Entities**:
  - None new -- mutates the existing `FileVersion` entity's `backend_id`/`backend_path` fields

- **API**:
  - `POST /api/file-storage/v1/files/{id}/migrate` -- migrate a file's content to a different backend

- **Sequences**:

  - None (flow and race-resolution documented inline in `cpt-cf-file-storage-flow-backend-migration` and
    `cpt-cf-file-storage-algo-backend-migration-race-resolve`)

- **Data**:

  - None new (mutates the existing `file_versions` table; reads the existing `version_hash_manifest` table for
    multipart-composite versions; writes to the existing `audit_outbox` table)


---

## 3. Feature Dependencies

```text
(P1 file upload / versioning foundation)
    |
    +-- cpt-cf-file-storage-feature-multipart-coordinator
    |       |
    |       +-- cpt-cf-file-storage-feature-content-hash-modes
    |               |
    |               +-- cpt-cf-file-storage-feature-backend-migration
    |
    +-- cpt-cf-file-storage-feature-policy-engine

cpt-cf-file-storage-feature-audit-trail (cross-cutting; every write path above and below depends on it)
    |
    +-- cpt-cf-file-storage-feature-retention-cleanup
    +-- cpt-cf-file-storage-feature-ownership-transfer
    +-- cpt-cf-file-storage-feature-backend-migration
```

**Dependency Rationale**:

- `cpt-cf-file-storage-feature-multipart-coordinator` depends on the P1 upload and versioning foundation: the initiate endpoint pre-registers a pending version in the file_versions table (owned by P1); the complete endpoint finalizes that version (a later, separate `bind` call activates it via the CAS mechanism established in P1); the signed-URL infrastructure (minting and verification -- a codec-equivalent Ed25519 token, not literal PASETO, per ADR-0004's Implementation note) is a P1 capability.
- `cpt-cf-file-storage-feature-content-hash-modes` depends on `cpt-cf-file-storage-feature-multipart-coordinator`: it consumes the multipart plan's per-part byte offsets (`compute_plan`) and the per-part SHA-256 digests multipart-coordinator already persists into `multipart_upload_parts.part_hash`, combining them into the offset-manifest composite at `complete` instead of multipart-coordinator's current re-read-and-rehash. It introduces no new inter-feature dependency beyond this one.
- `cpt-cf-file-storage-feature-policy-engine` depends on the P1 upload foundation the same way multipart-coordinator does (its enforcement hooks live in `create_file`/`presign_version`), but is otherwise independent of multipart-coordinator/content-hash-modes -- it is also consumed by multipart-coordinator's own initiate flow (the MIME/size checks in `cpt-cf-file-storage-flow-multipart-initiate`), making it a dependency of that feature too, not shown as a second arrow above to keep the diagram acyclic-readable.
- `cpt-cf-file-storage-feature-backend-migration` depends on `cpt-cf-file-storage-feature-content-hash-modes`: its pre-commit hash check dispatches through that feature's shared `cpt-cf-file-storage-algo-content-hash-modes-verify` algorithm rather than hard-coding a whole-object-only comparison, so a `multipart-composite-sha256` version can be migrated correctly.
- `cpt-cf-file-storage-feature-audit-trail` is a cross-cutting dependency of every feature with a write path in this document (multipart-coordinator, ownership-transfer, backend-migration, retention-cleanup, and the P1 foundation's own create/finalize/bind/patch/delete operations) -- it does not itself depend on any of them, since its transactional-outbox mechanism is generic over the caller's `AuditEntry`.
- `cpt-cf-file-storage-feature-ownership-transfer` and `cpt-cf-file-storage-feature-retention-cleanup` both depend on `cpt-cf-file-storage-feature-audit-trail` for their respective `TransferOwnership`/`RetentionDelete`/`OrphanReconcile` audit rows, and are otherwise independent of each other and of multipart-coordinator/content-hash-modes/backend-migration.
