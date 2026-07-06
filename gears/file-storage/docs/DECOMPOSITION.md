Created:  2026-07-02 by Constructor Tech
Updated:  2026-07-02 by Constructor Tech

# Decomposition: File Storage

**Overall implementation status:**
- [ ] `p1` - **ID**: `cpt-cf-file-storage-status-overall`



<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 Multipart Upload Coordinator - HIGH](#21-multipart-upload-coordinator---high)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

## 1. Overview

This document originally decomposed the P2 release cycle into a single feature — the server-authoritative multipart
upload path — and that is still the only entry with its own FEATURE artifact
([features/multipart-coordinator.md](features/multipart-coordinator.md)). **It understates what actually shipped in
P2.** Beyond multipart, the P2 branch also delivered: the **policy engine** (allowed-types / size / custom-metadata
limits at tenant and user scope), **retention rules + a background cleanup sweep** (whole-file retention pruning and
orphan reconciliation), an **audit outbox** (transactional write-operation audit trail), an **events outbox** (file
lifecycle events, not yet drained to the platform EventBroker — Tier 4 item 4.1 in the P2 remediation plan),
**ownership transfer**, and **backend migration**. None of these have their own FEATURE artifact under
`docs/features/` today; their behavior is documented only in code comments, `docs/api.md`, and
[README.md](../README.md)'s "Implementation status" section.

**Decomposition Strategy**:

- Only the multipart upload lifecycle (initiate, upload-part via sidecar, complete, abort, and introspect/resume) has
  a dedicated FEATURE decomposition entry (§2.1) and artifact, control-plane/sidecar split per ADR-0003 and ADR-0004.
- The policy engine, retention-cleanup, audit-trail, ownership-transfer, and backend-migration subsystems are real,
  shipped P2 scope that this document does **not** yet decompose into their own entries or FEATURE artifacts. Given
  the compliance weight of at least the audit-trail and ownership-transfer requirements
  (`cpt-cf-file-storage-fr-audit-trail`, `cpt-cf-file-storage-fr-ownership-transfer`), authoring proper FEATURE docs
  for all five — matching `features/multipart-coordinator.md`'s structure (flows, acceptance criteria, `p1`/`p2`
  tags) — is a **recommended follow-up**, tracked as P2 remediation plan item 3.6. This document takes the smaller,
  immediate fix instead: acknowledging the full P2 scope here rather than leaving the "one feature" framing
  uncorrected.
- The multipart feature depends on the P1 upload and versioning foundation (single-shot upload, file_versions table,
  signed-URL infrastructure) already shipped in P1; those P1 capabilities are not re-decomposed here.
- No shared components or DB tables are introduced by the multipart feature beyond the multipart_uploads and
  multipart_upload_parts tables it owns; the other P2 subsystems listed above have their own tables (see
  `docs/DESIGN.md` §3.7 and the gear's migrations) not enumerated in this single-feature-scoped document.


## 2. Entries

### 2.1 [Multipart Upload Coordinator](features/multipart-coordinator.md) - HIGH

- [ ] `p2` - **ID**: `cpt-cf-file-storage-feature-multipart-coordinator`

- **Type**: Core
- **Phases**: Single-phase implementation

- **Purpose**: Provide a safe, resumable, server-controlled multipart upload path. The client declares total size and a preferred part size; the control plane computes the exact parts plan and returns one signed sidecar URL per part. The sidecar enforces the per-part size claim before writing. The control plane assembles and hashes the parts at complete and finalizes the new file version; binding it as the file's current content remains a separate, client-issued request.

- **Depends On**: P1 file upload and versioning foundation (single-shot upload, file_versions table, signed-URL infrastructure -- codec-equivalent Ed25519 today, not literal PASETO, see ADR-0004's Implementation note -- not a formal DECOMPOSITION feature)

- **Scope**:
  - `POST /api/file-storage/v1/files/{id}/multipart` -- initiate: validate MIME/size/quota, compute parts plan, mint signed sidecar URLs, pre-register pending version
  - Sidecar part-upload handler: verify the signed token, enforce per-part size claim (HTTP 413), write part bytes, compute the per-part hash, and report it to the control plane over a token-authenticated callback (the sidecar has no DB connection of its own)
  - `POST .../multipart/{upload_id}/complete` -- verify assembled size against declared_size, assemble + hash the parts, **finalize** the version (`pending -> available`); does **not** bind (`content_id` untouched) and does **not** accept `If-Match` today -- see [features/multipart-coordinator.md](features/multipart-coordinator.md) for the tracked gap between this and the richer `If-Match`/`200`-body/missing-parts contract originally specified
  - `DELETE .../multipart/{upload_id}` -- abort: mark session aborted, delete part rows and pending version, abort backend handle for multipart_native backends
  - `GET .../multipart/{upload_id}` -- introspect/resume (p2): return plan recomputed from persisted columns, re-issue fresh signed URLs for missing parts
  - DB migration: add version_id, declared_size, part_size columns to multipart_uploads table

- **Out of scope**:
  - Single-shot upload path (owned by P1 foundation)
  - File download, listing, metadata update, or delete (owned by P1 foundation)
  - Storage quota ledger management (quota is read and enforced here; ledger updates owned by P1 foundation)

- **Requirements Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-fr-multipart-upload`
  - [ ] `p2` - `cpt-cf-file-storage-fr-size-limits-policy`
  - [ ] `p2` - `cpt-cf-file-storage-fr-storage-quota`

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


---

## 3. Feature Dependencies

```text
(P1 file upload / versioning foundation)
    |
    +-- cpt-cf-file-storage-feature-multipart-coordinator
```

**Dependency Rationale**:

- `cpt-cf-file-storage-feature-multipart-coordinator` depends on the P1 upload and versioning foundation: the initiate endpoint pre-registers a pending version in the file_versions table (owned by P1); the complete endpoint finalizes that version (a later, separate `bind` call activates it via the CAS mechanism established in P1); the signed-URL infrastructure (minting and verification -- a codec-equivalent Ed25519 token today, not literal PASETO, per ADR-0004's Implementation note) is a P1 capability.
- No inter-feature dependencies exist within P2 because this is the sole P2 DECOMPOSITION entry.
