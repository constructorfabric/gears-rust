# FileStorage

Universal file storage and management service for the Gears middleware.

## Overview

FileStorage provides upload, download, metadata management, access control, and sharing capabilities for all platform
gears and users. It replaces ad-hoc per-gear file handling with a centralized, tenant-aware storage service.

### Key Capabilities

- **File operations** — upload, download, delete, list with rich metadata
- **Pluggable backends** — S3, GCS, Azure Blob, NFS, FTP, SMB, WebDAV, local filesystem
- **Access control** — tenant-scoped ownership, GTS file type classification, Authorization Service integration
- **Sharing** — shareable links (public/tenant/hierarchy scopes), signed URLs, direct transfer URLs
- **Access interfaces** — REST API, S3-compatible API, WebDAV API
- **Policies** — file type restrictions, size limits, sharing model restrictions, storage quotas
- **Lifecycle** — file versioning, retention policies, multipart upload, conditional requests (ETags)
- **Audit** — write operation audit trail, optional read audit logging

### Actors

| Actor               | Description                                                                   |
|---------------------|-------------------------------------------------------------------------------|
| Platform User       | Authenticated user managing files via UI or API                               |
| CF/Gears | Any gear requiring file operations (e.g., LLM Gateway, document management) |

### Dependencies

| Dependency            | Criticality |
|-----------------------|-------------|
| ToolKit Framework      | p1          |
| Authorization Service | p1          |
| Audit Infrastructure  | p2          |
| Usage Collector       | p2          |
| Quota Enforcement     | p2          |
| EventBroker           | p2          |
| Serverless Runtime    | p2          |

## Documentation

- [PRD.md](docs/PRD.md) — Product requirements document
- [DESIGN.md](docs/DESIGN.md) — Architecture and design
- [DECOMPOSITION.md](docs/DECOMPOSITION.md) — Feature decomposition strategy
- [api.md](docs/api.md) — HTTP API reference
- [ADR/](docs/ADR/) — Architecture decision records
- [features/](docs/features/) — Per-feature specs (multipart coordinator, …)

## Implementation status

### P1 (foundation)

The **P1 control plane** is implemented and tested. Highlights:

- Two crates: `cf-gears-file-storage-sdk` (public API) + `cf-gears-file-storage` (gear lib + `sidecar` binary).
- Control-plane REST under `/api/file-storage/v1` (create/presign/bind, download-URL, metadata CRUD, list,
  versions, storages) — JSON only; content never transits the control plane.
- Immutable-blob + content-pointer model with optimistic-CAS bind, FileStorage-level versioning, tenant isolation,
  Authorization-Service per-type checks, conditional requests (ETag / `If-Match` / `If-None-Match`).
- Pluggable backends (trait + `local-fs` + `in-memory`); Ed25519 signed URLs (codec-equivalent to, but not literal,
  PASETO `v4.public` — see [ADR-0004](docs/ADR/0004-cpt-cf-file-storage-adr-signed-url-transport.md)'s
  Implementation note); SHA-256 + magic-byte content-type validation; HTTP `Range`. Data-plane **sidecar** binary
  verifies tokens and streams bytes, then calls a token-authenticated `finalize` callback back to the control plane
  (`pending → available`); binding a version as the file's live content (`content_id`) is always a separate,
  client-issued request (see [DESIGN.md](docs/DESIGN.md) §3.6 and
  [ADR-0003](docs/ADR/0003-cpt-cf-file-storage-adr-sidecar-data-plane.md)).

### P2 (this branch)

Built on top of P1, the following shipped:

- **Policy engine** — allowed-types / size / custom-metadata-limit policies, resolved at tenant and user scope
  (`GET`/`PUT /policy`, `GET /policy/effective`).
- **Retention rules + background cleanup sweep** — per-tenant retention rules (`/retention-rules`) plus a background
  process that prunes expired files and reconciles orphaned backend objects.
- **Idempotent create** — `POST /files` is safe to retry.
- **Audit outbox** — a transactional outbox recording write operations (create, finalize, bind, metadata update,
  delete, ownership transfer, backend migration, …) for downstream audit consumption.
- **Events outbox** — file lifecycle events (`file.content_updated`, `file.owner_transferred`, …) are written
  transactionally alongside the mutation. **Not yet drained** to the platform EventBroker — see Tier 4 item 4.1 in
  the P2 remediation plan.
- **Ownership transfer** — `POST /files/{id}/transfer`, atomic owner swap with audit + event + usage-delta reporting.
- **Backend migration** — `POST /files/{id}/migrate`, relocates a non-versioned file's content to a different backend
  with a verified content-hash check before committing.
- **Multipart upload** — server-authoritative parts plan, per-part signed URLs, and the sidecar's report-part
  callback are wired end-to-end. **Functional only against a `multipart_native` backend** (today: the non-durable
  in-memory backend, dev/test only) — the default `local-fs` backend does not declare `multipart_native`, so
  `POST /files/{id}/multipart` is rejected against the real default topology. See
  [docs/features/multipart-coordinator.md](docs/features/multipart-coordinator.md) for the tracked gap and current
  vs. intended `complete` contract.

**Not yet implemented**: sharing (shareable links), WebDAV, quota enforcement wiring (Tier 1 item 1.4), and the S3
backend (Tier 1 item 1.7) — all still declared in the PRD/DESIGN but absent from the code as of this branch.

### Run

```bash
cargo build -p cf-gears-file-storage                 # control-plane gear (lib)
cargo build -p cf-gears-file-storage --bin sidecar   # data-plane sidecar
cargo test  -p cf-gears-file-storage -p cf-gears-file-storage-sdk

# Sidecar env (P1 static): FS_SIDECAR_ADDR, FS_SIDECAR_PUBLIC_KEY (base64url Ed25519), FS_SIDECAR_BACKEND_ROOT
```
