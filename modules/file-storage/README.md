# FileStorage

Universal file storage and management service for the CyberFabric platform.

## Overview

FileStorage provides upload, download, metadata management, access control, and sharing capabilities for all platform
modules and users. It replaces ad-hoc per-module file handling with a centralized, tenant-aware storage service.

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
| CyberFabric Modules | Any module requiring file operations (e.g., LLM Gateway, document management) |

### Dependencies

| Dependency            | Criticality |
|-----------------------|-------------|
| ModKit Framework      | p1          |
| Authorization Service | p1          |
| Audit Infrastructure  | p2          |
| Usage Collector       | p2          |
| Quota Enforcement     | p2          |
| EventBroker           | p2          |
| Serverless Runtime    | p2          |

## Documentation

- [PRD.md](docs/PRD.md) — Product requirements document
- [DESIGN.md](docs/DESIGN.md) — Architecture and design
- [ADR/](docs/ADR/) — Architecture decision records
- [features/](docs/features/) — Feature specifications
