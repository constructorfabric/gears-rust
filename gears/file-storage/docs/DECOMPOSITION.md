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

The File Storage design is decomposed into one feature for the P2 release cycle, focusing on the server-authoritative multipart upload path. The decomposition follows the control-plane / sidecar split established in ADR-0003 and ADR-0004: the control plane owns plan computation, signed-URL minting, quota enforcement, and version activation, while the sidecar owns byte movement and per-part enforcement.

**Decomposition Strategy**:

- A single feature covers the full multipart upload lifecycle (initiate, upload-part via sidecar, complete, abort, and introspect/resume).
- The feature depends on the P1 upload and versioning foundation (single-shot upload, file_versions table, signed-URL infrastructure) already shipped in P1; those P1 capabilities are not re-decomposed here.
- No shared components or DB tables are introduced in this cycle beyond the multipart_uploads and multipart_upload_parts tables owned by this feature.


## 2. Entries

### 2.1 [Multipart Upload Coordinator](features/multipart-coordinator.md) - HIGH

- [ ] `p2` - **ID**: `cpt-cf-file-storage-feature-multipart-coordinator`

- **Type**: Core
- **Phases**: Single-phase implementation

- **Purpose**: Provide a safe, resumable, server-controlled multipart upload path. The client declares total size and a preferred part size; the control plane computes the exact parts plan and returns one signed sidecar URL per part. The sidecar enforces the per-part size claim before writing. The control plane combines per-part hashes into the root hash at complete and binds the new file version atomically.

- **Depends On**: P1 file upload and versioning foundation (single-shot upload, file_versions table, PASETO signed-URL infrastructure -- not a formal DECOMPOSITION feature)

- **Scope**:
  - `POST /api/file-storage/v1/files/{id}/multipart` -- initiate: validate MIME/size/quota, compute parts plan, mint signed sidecar URLs, pre-register pending version
  - Sidecar part-upload handler: verify PASETO token, enforce per-part size claim (HTTP 413 before any write), write part bytes, compute and persist per-part hash
  - `POST .../multipart/{upload_id}/complete` -- verify all parts present, combine part hashes into root hash, apply If-Match CAS, activate file version
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

- `cpt-cf-file-storage-feature-multipart-coordinator` depends on the P1 upload and versioning foundation: the initiate endpoint pre-registers a pending version in the file_versions table (owned by P1); the complete endpoint activates that version using the CAS mechanism established in P1; PASETO signed-URL infrastructure (minting and verification) is a P1 capability.
- No inter-feature dependencies exist within P2 because this is the sole P2 DECOMPOSITION entry.
