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
- [api.md](docs/api.md) — HTTP API reference
- [ADR/](docs/ADR/) — Architecture decision records

## Implementation status (P1)

The **P1 control plane** is implemented and tested. Highlights:

- Two crates: `cf-gears-file-storage-sdk` (public API) + `cf-gears-file-storage` (gear lib + `sidecar` binary).
- Control-plane REST under `/api/file-storage/v1` (create/presign/bind, download-URL, metadata CRUD, list,
  versions, storages) — JSON only; content never transits the control plane.
- Immutable-blob + content-pointer model with optimistic-CAS bind, FileStorage-level versioning, tenant isolation,
  Authorization-Service per-type checks, conditional requests (ETag / `If-Match` / `If-None-Match`).
- Pluggable backends (trait + `local-fs` + `in-memory`); Ed25519 signed URLs; SHA-256 + magic-byte content-type
  validation; HTTP `Range`. Data-plane **sidecar** binary verifies tokens and streams bytes.

P2/P3 features above (sharing, S3/WebDAV facades, policies, audit, multipart, quotas, …) are declared in the
PRD/DESIGN but not implemented in P1.

### Run

```bash
cargo build -p cf-gears-file-storage                 # control-plane gear (lib)
cargo build -p cf-gears-file-storage --bin sidecar   # data-plane sidecar
cargo test  -p cf-gears-file-storage -p cf-gears-file-storage-sdk

# Sidecar env (P1 static): FS_SIDECAR_ADDR, FS_SIDECAR_PUBLIC_KEY (base64url Ed25519), FS_SIDECAR_BACKEND_ROOT
```
