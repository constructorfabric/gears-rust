# PRD — File Storage


<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Success Metrics](#14-success-metrics)
  - [1.5 Glossary](#15-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
  - [3.1 Module-Specific Environment Constraints](#31-module-specific-environment-constraints)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 Core File Operations](#51-core-file-operations)
  - [5.2 Ownership & Access Control](#52-ownership--access-control)
  - [5.3 Sharing](#53-sharing)
  - [5.4 Policies (Phase 2)](#54-policies-phase-2)
  - [5.5 Metadata](#55-metadata)
  - [5.6 File Retention & Lifecycle](#56-file-retention--lifecycle)
  - [5.7 Audit](#57-audit)
  - [5.8 Pluggable Storage Backends](#58-pluggable-storage-backends)
  - [5.9 Access Interfaces](#59-access-interfaces)
  - [5.10 Cache & Idempotency](#510-cache--idempotency)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Module-Specific NFRs](#61-module-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
  - [6.3 Applicability Notes](#63-applicability-notes)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
  - [Upload and Make Public](#upload-and-make-public)
  - [Fetch File for Module Processing](#fetch-file-for-module-processing)
  - [Validate File Metadata Before Processing](#validate-file-metadata-before-processing)
  - [Delete a File](#delete-a-file)
  - [Manage Public Access](#manage-public-access)
  - [Multi-Backend Deployment](#multi-backend-deployment)
  - [Configure Policy](#configure-policy)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

FileStorage is a universal file storage and management service for the Cyber Ware middleware. It provides upload,
download, metadata management, access control, and sharing capabilities for any module or user within the platform.

The service supports pluggable storage backends, multiple access protocols (REST, S3-compatible, WebDAV), tenant-scoped
access control with an ownership model, and policy-driven governance for file types, sizes, and sharing.

### 1.2 Background / Problem Statement

Cyber Ware modules and platform users require file storage for various purposes: modules handle multimodal AI content
(images, audio, video, documents), documents and artifacts, reporting outputs, and platform users need direct file
access through standard protocols.

Without a dedicated storage service, each module implements ad-hoc file handling, media gets inlined as base64 in API
payloads (bloating requests and hitting size limits), provider-generated URLs expire leaving consumers with broken
links, and there is no unified access control or policy enforcement across the platform.

FileStorage solves this by providing a centralized, tenant-aware storage service with persistent URLs, pluggable
backends, and standardized access interfaces — functioning as a superset of S3 and WebDAV capabilities within the
Cyber Ware security and governance model.

### 1.3 Goals (Business Outcomes)

- Unified file storage accessible by all Cyber Ware modules and platform users
- Tenant-scoped and origin-module-scoped access control with tenant, user and module ownership model
- Anonymous public sharing via a per-file flag and a dedicated unauthenticated URL namespace
- Policy-driven governance over file types, sizes, events, and sharing models
- Audit trail for all write operations
- Pluggable storage backends without service rebuild

### 1.4 Success Metrics

| Metric                                   | Baseline                                 | Target                                                           | Timeframe                      |
|------------------------------------------|------------------------------------------|------------------------------------------------------------------|--------------------------------|
| Module adoption rate                     | 0% (ad-hoc file handling)                | 90%+ of file-dependent modules use FileStorage SDK               | 6 months after GA              |
| Base64-inlined media payloads            | Present in LLM Gateway and other modules | 0 base64 file payloads in modules that adopted FileStorage       | 3 months after module adoption |
| Broken/expired provider URLs             | Recurring in downstream workflows        | 0 broken URLs for files within retention period                  | Ongoing after GA               |
| Audit coverage for file write operations | No centralized audit                     | 100% of write operations audited                                 | Phase 2                        |
| Multi-backend deployment                 | Single ad-hoc storage per module         | At least 2 backend types validated (e.g., S3 + local filesystem) | At GA                          |

### 1.5 Glossary

| Term                | Definition                                                                                                                                                                                                                                                                              |
|---------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| File                | Binary content stored in FileStorage with associated metadata                                                                                                                                                                                                                           |
| File URL            | Persistent URL pointing to content stored in FileStorage                                                                                                                                                                                                                                |
| Metadata            | File properties: system-managed (name, size, mime_type, GTS file type, dates, owner, availability) and user-defined custom key-value pairs                                                                                                                                              |
| Custom Metadata     | User-defined key-value pairs attached to a file, analogous to S3 object metadata                                                                                                                                                                                                        |
| Owner               | The principal that owns a file: `owner_kind ∈ {user, app}` plus `owner_id`. Every file also has a separate immutable `tenant_id`                                                                                                                                                       |
| Shareable Link      | A unique URL served by FileStorage that grants access to a file with a specific scope and expiration; FileStorage validates the link and enforces access control on every request                                                                                                       |
| Signed URL          | A presigned backend URL (download-only) for sharing with external or unauthenticated consumers; generated by FileStorage using its own backend credentials, time-limited, non-revocable — the storage backend validates the signature and enforces expiration                           |
| Direct Transfer URL | A presigned backend URL (upload-only) for authenticated clients to upload file content directly to the backend without routing traffic through FileStorage; generated by FileStorage after authorization, time-limited                                                                  |
| Storage Backend     | An underlying storage system (S3, GCS, Azure Blob, NFS, FTP, SMB, WebDAV) used for persisting file content                                                                                                                                                                              |
| Policy              | A set of rules (allowed file types, size limits, events, sharing models) that constrain file operations; applicable at the tenant level and the user level independently — when both apply, the most restrictive value per aspect wins                                                  |
| File Version        | An immutable snapshot of file content created on each upload to the same logical path when versioning is enabled; identified by an opaque version identifier assigned by the storage backend                                                                                            |
| Version Identifier  | An opaque string assigned by the storage backend that uniquely identifies a specific version of a file; format varies by backend and must not be parsed or assumed                                                                                                                      |
| File Type (GTS)     | A GTS type identifier assigned to every file at upload time that classifies the file by domain, actor, and purpose (e.g., `gts.cf.fstorage.file.type.v1~x.genai.llm.autogenerated.v1~`); used by the Authorization Service to enforce per-type access control between actors and modules |
| Backend Capability  | An optional feature that a storage backend may or may not support (e.g., presigned URLs, versioning, multipart upload); FileStorage discovers available capabilities per backend and adapts its behavior accordingly                                                                    |

## 2. Actors

### 2.1 Human Actors

#### Platform User

**ID**: `cpt-cf-file-storage-actor-platform-user`

**Role**: Authenticated user who uploads, downloads, and manages files through the platform UI or API.
**Needs**: Direct file access, sharing capabilities, metadata management, and self-service link management.

### 2.2 System Actors

#### Cyber Ware Modules

**ID**: `cpt-cf-file-storage-actor-cf-modules`

**Role**: Any Cyber Ware module requiring file upload, download, metadata retrieval, or link management (e.g., LLM
Gateway for multimodal media, document management modules, reporting modules).

## 3. Operational Concept & Environment

### 3.1 Module-Specific Environment Constraints

FileStorage operates within the standard Cyber Ware runtime environment. Authentication and identity management are
fully delegated to the platform — FileStorage does not implement its own authentication layer. All incoming requests are
pre-authenticated by the platform infrastructure, and FileStorage receives the caller's identity context (user, tenant,
roles) from the platform authentication middleware.

## 4. Scope

### 4.1 In Scope

- Upload, download, delete, and list files
- Rich file metadata storage, retrieval, and update
- File ownership by user or app (Cyber Ware module) within a tenant
- GTS file type classification for per-actor access control
- Authorization checks via Authorization Service
- Anonymous public sharing via a per-file flag served on a dedicated unauthenticated URL namespace
- Audit trail for all write operations and optional read audit logging
- Policies (file types, size limits, events) at tenant and user levels
- Pluggable storage backend abstraction
- Multipart (chunked) upload for large files
- Content-type validation against actual file content
- File retention and lifecycle management
- REST API access interface
- Random read access via HTTP Range requests
- Static (P1) and runtime (P3) storage backend configuration
- Storage quota enforcement via Quota Enforcement service
- Ownership transfer
- Custom metadata limits
- File versioning
- Conditional requests (ETags) for cache validation and concurrent update protection
- Upload idempotency
- Owner deletion handling via EventBroker and Serverless Runtime workflows
- File encryption (server-side, per backend capability and configuration)

### 4.2 Out of Scope

- Content transformation or transcoding
- CDN distribution
- Full-text search within file content
- Scope-based shareable links (`public`, `tenant`, `tenant-hierarchy` link tokens) — replaced by the per-file
  `public_access` flag (`cpt-cf-file-storage-fr-public-access`)
- Per-principal grant-based sharing — owned by the **FileShare** module (P3, `modules/file-share/`)
- S3-compatible and WebDAV protocol facades — if needed they will be implemented as separate modules consuming
  FileStorage's SDK

## 5. Functional Requirements

### 5.1 Core File Operations

#### Upload File

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-upload-file`

The system **MUST** accept file content with metadata and persist it, returning a persistent, accessible URL. Content
is mutable through dedicated content-replacement operations on the same file; when the backing storage backend
declares the versioning capability, each replacement creates a new immutable backend version.

**Rationale**: All platform modules and users need to store files — modules store generated content, documents, and
artifacts, users upload files directly. Coupling content replacement to backend versioning preserves recoverability
where the backend supports it without forcing consumers to rotate file identifiers.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Download File

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-download-file`

The system **MUST** retrieve file content and metadata by URL for consumption by requesting actors.

**Rationale**: All platform modules and users need to retrieve stored files — modules fetch media and documents, users
download files directly.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Delete File

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-delete-file`

The system **MUST** allow the file owner to delete a file. For non-versioned files, deletion is permanent — content,
metadata, and ownership records are removed. When versioning is enabled
(`cpt-cf-file-storage-fr-file-versioning`), deletion without a version identifier places a soft-delete marker.
Permanent removal of a specific version requires passing its version identifier explicitly.

**Rationale**: Owners need to remove files that are no longer needed. Versioned files default to soft-delete to
enable recovery from accidental deletions. Permanent removal is an explicit, version-targeted operation.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Get File Metadata

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-get-metadata`

The system **MUST** return file metadata (name, size, mime_type, GTS file type, created date, modified date, owner,
download availability, and custom metadata) without transferring file content.

**Rationale**: Consumers validate file properties (size limits, type compatibility) and read custom metadata before
initiating downloads, avoiding wasted bandwidth on incompatible files.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### List Files

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-list-files`

The system **MUST** support listing files with their metadata (no content transfer). The caller **MUST** specify the
owner type as a mandatory filter:

- **User-owned** — files owned by a specific user (`owner_kind = user`)
- **App-owned** — files owned by a Cyber Ware module (`owner_kind = app`)

The response **MUST** be paginated following the platform API guidelines (cursor-based or offset-based pagination with
configurable page size). The system **MUST** support optional additional filters (mime_type, date range, custom metadata
keys).

**Rationale**: Users and modules need to discover and browse files they own or have access to. Mandatory owner type
filtering prevents unbounded queries across all files and aligns with the ownership model.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Multipart Upload

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-multipart-upload`

The system **MUST** support multipart (chunked) upload for large files. Multipart upload requires the multipart
upload backend capability (`cpt-cf-file-storage-fr-backend-capabilities`). A multipart upload **MUST**:

- Allow the client to split a file into multiple parts and upload them independently
- Support resumable uploads — if a part fails, only that part needs re-uploading
- Assemble parts into a complete file upon finalization
- Apply the same authorization, metadata, and audit requirements as single-part uploads

For backends that do not declare the multipart upload capability, the system **MUST** reject multipart upload requests
with a clear error indicating the capability is unavailable. There is no FileStorage-level fallback for multipart —
clients must use single-part upload for backends without native multipart support.

**Rationale**: Single-request uploads are impractical for large files (video, datasets, backups) due to timeouts,
memory constraints, and network reliability. Multipart upload enables reliable transfer of arbitrarily large files.
Implementing multipart at the FileStorage layer without backend support would require full content buffering, negating
the scalability benefits. Rejecting with a clear error lets clients adapt their upload strategy per backend.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Content-Type Validation

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-content-type-validation`

The system **MUST** validate the declared mime_type against the actual file content (magic bytes / file signature) on
every upload (all upload traffic transits FileStorage). If the declared type does not match the detected type, the
system **MUST** reject the upload with an error indicating the mismatch.

For multipart uploads (`cpt-cf-file-storage-fr-multipart-upload`), the system **MUST** validate the declared mime_type
against the content of the **first uploaded part**, which contains the file's magic bytes / file signature. Validation
**MUST** occur when the first part is received — before subsequent parts are accepted. If the detected type does not
match the declared mime_type, the system **MUST** abort the multipart upload and reject all subsequent parts.

**Rationale**: Without content inspection, a client can declare `image/png` but upload an executable, trivially
bypassing file type policies. Content-type validation ensures declared types are trustworthy for downstream consumers
and policy enforcement. First-part validation for multipart uploads provides the same level of guarantee as single-part
validation — magic bytes reside at the start of the file and are always contained in the first part because backends
that support multipart upload (`cpt-cf-file-storage-fr-backend-capabilities`) enforce a minimum part size (e.g., 5 MB
for S3) that far exceeds the longest magic-byte sequence (~12 bytes). Backends without native multipart support reject
multipart uploads entirely, so no fallback is needed.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

### 5.2 Ownership & Access Control

#### File Ownership

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-file-ownership`

The system **MUST** associate every file with `tenant_id` (mandatory, immutable) plus `owner_kind ∈ {user, app}` and
`owner_id`. `user` is a platform user; `app` is a Cyber Ware module (e.g., LLM Gateway owning its generated media).
The owner principal is immutable after creation except through explicit ownership transfer
(`cpt-cf-file-storage-fr-ownership-transfer`) or owner deletion workflows (`cpt-cf-file-storage-fr-owner-deletion`).
`tenant_id` is never mutable.

**Rationale**: Ownership determines who can manage (delete, update metadata) a file and establishes the basis for
access control decisions. Separating `tenant_id` from `(owner_kind, owner_id)` reflects how Cyber Ware scopes data:
tenant is the hard boundary for isolation, while the owner identifies a specific principal within the tenant.
Modules own platform-generated content (LLM outputs, reports) via `owner_kind = app` without requiring an artificial
human user.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Authorization Checks

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-authorization`

The system **MUST** verify authorization for every file operation by requesting an access decision from the
Authorization Service. Read, write, and delete operations **MUST** be checked against `gts.cf.fstorage.file.type.v1~` resources in
the context of the requesting user. Authorization requests **MUST** include the file's GTS type
(`cpt-cf-file-storage-fr-file-type-classification`) in the resource context to enable per-type access decisions.

**Rationale**: All file access must be governed by the platform's centralized authorization model to enforce role-based,
tenant-scoped, and type-scoped permissions.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Tenant Boundary Enforcement

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-tenant-boundary`

The system **MUST** enforce tenant isolation: all file operations through the auth-required namespace **MUST NOT**
cross tenant boundaries. A principal in one tenant **MUST NOT** access files owned by another tenant. The only
exception is anonymous access via the public namespace (`cpt-cf-file-storage-fr-public-access`), which is not
tenant-aware.

**Rationale**: Multi-tenant platforms require strict data isolation to prevent unauthorized cross-tenant access.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Data Classification

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-data-classification`

FileStorage treats all stored files as opaque binary blobs and does **NOT** inspect, classify, or label file content by
sensitivity level. Data classification (public, internal, confidential, restricted) is the responsibility of consuming
modules and policies. FileStorage enforces access control through its authorization model and tenant boundaries
regardless of data sensitivity.

**Rationale**: FileStorage is a general-purpose storage service that serves modules with diverse data sensitivity
requirements. Embedding classification logic in the storage layer would couple it to domain-specific semantics. Instead,
consuming modules classify their own data and rely on FileStorage's authorization and tenant isolation to enforce access
boundaries appropriate to the sensitivity level.
**Actors**: `cpt-cf-file-storage-actor-cf-modules`

#### File Type Classification

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-file-type-classification`

The system **MUST** require a GTS file type identifier on every file at upload time. The file type classifies the file
by domain and purpose following the GTS type format (e.g. `gts.cf.fstorage.file.type.v1~x.genai.llm.autogenerated.v1~`
for LLM-generated files). The file type **MUST** be:

- Mandatory — uploads without a file type **MUST** be rejected
- Immutable — the file type **MUST NOT** be changeable after creation
- Stored as system-managed metadata — returned in all metadata queries alongside other system fields
- Validated — the system **MUST** verify that the provided type follows the GTS type format

The system **MUST** be able to use the file type to make per-type access decisions, enabling isolation
between actors and modules — a module **MUST** only be able to access files of types it is authorized for. File type
authorization is enforced through the existing authorization model (`cpt-cf-file-storage-fr-authorization`).

**Rationale**: Without file type classification, any module with general file access can read files created by any other
module, breaking isolation between platform components. GTS types enable fine-grained, per-actor access control — e.g.,
the LLM Gateway can only access LLM-generated files, the Feedback module can only access feedback-related files —
without requiring separate storage namespaces or custom authorization logic per module.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Ownership Transfer

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-ownership-transfer`

The system **MUST** allow the current file owner to transfer ownership of a file to another principal (user or app)
within the **same tenant**. Cross-tenant transfer is **NOT** supported. Ownership transfer **MUST** be an audited
operation and **MUST** require authorization of both the current owner and the receiving principal.

**Rationale**: As teams and modules evolve, files may need to change hands. Restricting transfers to within the
file's tenant preserves the tenant-isolation invariant.
**Actors**: `cpt-cf-file-storage-actor-platform-user`

### 5.3 Sharing

#### Public Access Flag

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-public-access`

The system **MUST** expose a per-file boolean property `public_access` that the owner can toggle via standard metadata
updates (`cpt-cf-file-storage-fr-update-metadata`). When `public_access = true`, the file's content is readable
through a dedicated **public namespace** (`/api/file-storage-public/v1/files/{file_id}`, `GET` and `HEAD` only)
without authentication. The platform API Gateway **MUST** route requests under this prefix without enforcing JWT
authentication. When `public_access = false`, requests to the public namespace **MUST** respond with
`404 Not Found` regardless of the file's actual existence (never `401`/`403`), to avoid leaking the existence of
private files. The flag is not time-bounded — revoking access requires setting the flag back to `false`.

The public namespace **MUST**:

- Support `GET` and `HEAD` only — all write methods return `405`
- Support `Range` and conditional-request semantics identical to the auth-required path
  (`cpt-cf-file-storage-fr-range-requests`, `cpt-cf-file-storage-fr-conditional-requests`)
- Omit tenant-internal classifiers (GTS file type, custom metadata) from response headers
- Honor the `download_availability` kill-switch — when `download_availability = false`, public-namespace requests
  **MUST** respond with `404` regardless of `public_access`

The file's UUID is the access secret on the public namespace; random 128-bit UUIDs make enumeration computationally
infeasible.

Per-principal grant-based sharing (one user explicitly granting access to specific other users) is **not** provided
by FileStorage; it is delivered by the separate **FileShare** module (P3, `modules/file-share/`), which proxies
content from FileStorage on the grantee's behalf.

**Rationale**: A boolean flag controlled by standard metadata updates is the simplest mechanism for "the internet
can read this" — no link tables, no token expiry. Routing through a distinct URL prefix lets the API Gateway bypass
JWT validation cleanly without per-request authentication-mode introspection. Returning `404` (rather than `401`/`403`)
preserves the privacy of files whose UUIDs leak through logs.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

### 5.4 Policies (Phase 2)

#### Allowed File Types Policy

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-allowed-types-policy`

The system **MUST** allow owners to define policies specifying which file types (by mime_type) are permitted for
upload. Uploads of disallowed types **MUST** be rejected.

**Rationale**: Tenants need to restrict uploads to approved file types for security and compliance (e.g., blocking
executable files).
**Actors**: `cpt-cf-file-storage-actor-platform-user`

#### File Size Limits Policy

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-size-limits-policy`

The system **MUST** enforce file size limits from two sources:

- **Backend limit** — each storage backend declares its maximum supported file size in configuration. This is a hard
  ceiling that no policy can override.
- **Policy limits** — tenants and users define a global maximum size and optional per-mime-type overrides (e.g., 100 MB
  general, 1 GB for `video/*`). When both tenant and user policies apply, the most restrictive value wins.

Uploads exceeding any applicable limit **MUST** be rejected with an error identifying which limit was violated.

**Rationale**: Backend limits reflect physical constraints of the storage system. Policy limits give tenants and users
granular control over storage consumption. The most-restrictive-wins model ensures no level can override another's
constraints.
**Actors**: `cpt-cf-file-storage-actor-platform-user`

#### File Events

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-file-events`

The system **MUST** emit events to the EventBroker module on file write operations (upload, update, delete). Owner
policy **MUST** define which event types are enabled.

**Rationale**: Enables integration with downstream consumers for workflows such as antivirus scanning, content
moderation, indexing, or backup triggers — without coupling FileStorage to specific consumers.
**Actors**: `cpt-cf-file-storage-actor-platform-user`

#### Storage Usage Reporting

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-usage-reporting`

The system **MUST** report storage usage data to the Usage Collector service. Usage reports **MUST** include per-owner
storage consumption (total bytes, file count) and **MUST** be emitted on every write operation that changes storage
consumption (upload, delete, version creation, version deletion) and on ownership transfer
(`cpt-cf-file-storage-fr-ownership-transfer`). For ownership transfers, the system **MUST** emit a usage report for both
the previous owner (storage decrease) and the new owner (storage increase). The reporting mechanism **MUST** be
asynchronous and **MUST NOT** block file operations if the Usage Collector is temporarily unavailable.

**Rationale**: Centralized usage data is required for metering, billing, capacity planning, and analytics. Ownership
transfers shift per-owner storage consumption without changing total platform storage — without debit/credit reporting,
billing and quota data become stale after transfers. Asynchronous reporting ensures file operations are not degraded by
usage collection availability.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Storage Quota Enforcement

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-storage-quota`

The system **MUST** check with the Quota Enforcement service before accepting any operation that increases storage
consumption (including uploads and version creation). Operations that would exceed the owner's storage quota **MUST** be
rejected.

**Rationale**: Without storage quotas, tenants can consume unbounded storage, increasing costs and risking resource
exhaustion for the platform. Quota checks must cover all storage-consuming operations, not only initial uploads, to
prevent quota bypass through versioned overwrites.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

### 5.5 Metadata

#### Rich Metadata Storage

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-metadata-storage`

The system **MUST** store and return the following system-managed metadata for every file:

- File name (original upload name)
- File size (bytes)
- File type (mime_type)
- GTS file type (`cpt-cf-file-storage-fr-file-type-classification`)
- Creation date
- Last modified date
- Owner (`owner_kind ∈ {user, app}` + `owner_id`) and `tenant_id`
- Download availability (whether the file is currently accessible for download; controlled by the file owner)

In addition, the system **MUST** support user-defined custom metadata as arbitrary key-value string pairs. Custom
metadata **MUST** be specifiable at upload time and updatable after upload. The system **MUST** return custom metadata
alongside system-managed metadata in metadata queries.

**Rationale**: Rich metadata enables file browsing, search, validation, and governance across the platform. Custom
metadata enables consumers to attach domain-specific context (tags, categories, processing status, source identifiers)
without schema changes — following the established pattern used by S3 object metadata, GCS custom metadata, and Azure
Blob metadata.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Update Custom Metadata

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-update-metadata`

The file owner **MUST** be able to update custom metadata (user-defined key-value pairs) and download availability on
an existing file. All other system-managed metadata (name, size, mime_type, GTS file type, creation date, last modified
date, owner) is **NOT** updatable by users — it is maintained by the system. Updating custom metadata or download
availability **MUST** update the file's last modified date.

**Rationale**: Custom metadata evolves as files are processed, categorized, or annotated by consuming modules. System
metadata reflects the immutable physical properties of the file and must remain authoritative.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Custom Metadata Limits

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-metadata-limits`

The system **MUST** enforce configurable limits on custom metadata: maximum number of key-value pairs per file, maximum
key name length, maximum value length, and maximum total custom metadata size per file. Metadata operations exceeding
limits **MUST** be rejected.

**Rationale**: Without limits, custom metadata can be abused for general-purpose data storage, inflating metadata
storage costs and degrading query performance.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

### 5.6 File Retention & Lifecycle

#### Indefinite Retention

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-retention-indefinite`

In phase 1, files **MUST** be retained indefinitely until explicitly deleted by the file owner. The system **MUST NOT**
automatically delete or expire file content based on age or inactivity.

**Rationale**: In the absence of tenant-level retention policies (phase 2), indefinite retention is the safest default —
it prevents accidental data loss and gives consuming modules predictable storage semantics.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Retention Policies

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-retention-policies`

The system **MUST** allow owners to define retention policies specifying automatic file expiration based on age,
inactivity, or custom metadata criteria. The system **MUST** also support per-file retention overrides set by the file
owner. When a file's retention period expires, the system **MUST** delete the file content, metadata, and all associated
links, and emit an audit record.

**Rationale**: Regulated environments and cost-conscious tenants need automated lifecycle management to enforce data
retention compliance and control storage growth.
**Actors**: `cpt-cf-file-storage-actor-platform-user`

#### Owner Deletion Handling

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-owner-deletion`

The system **MUST** handle file owner removal (user or tenant deletion) by consuming owner deletion events from the
EventBroker. Upon receiving an owner deletion event, the system **MUST** execute a configurable workflow via the
Serverless Runtime to determine the disposition of all files owned by the deleted entity. The workflow **MUST** be able
to:

- Delete all files owned by the removed owner
- Archive files (mark as archived and disable further modifications while preserving content)
- Transfer ownership to another user or app within the same tenant
- Apply any combination of the above based on file metadata or custom criteria

The specific disposition logic **MUST** be defined as a Serverless Runtime workflow or function, configurable per
deployment. If no workflow is configured, the system **MUST** retain files indefinitely (no automatic deletion) and
mark them as orphaned for manual resolution.

**Rationale**: When users leave an organization or tenants are decommissioned, their files require deliberate handling —
blind deletion risks data loss, while indefinite retention risks compliance violations. Delegating disposition to
Serverless Runtime workflows enables deployment-specific logic (legal holds, data migration, cascading cleanup) without
embedding policy decisions in FileStorage.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### File Versioning

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-file-versioning`

File versioning requires the versioning backend capability (`cpt-cf-file-storage-fr-backend-capabilities`). When the
versioning capability is available for a backend, the system **MUST**:

- Create a new version with an opaque version identifier on every file upload to the same logical path
- Retrieve a specific file version by its version identifier
- Retrieve metadata of a specific file version by its version identifier
- List all versions (current and non-current) of a file, including each version's identifier, size, last modified
  timestamp, and whether it is the current version
- Soft-delete a file (without specifying a version) by placing a logical delete marker on the current version. The
  delete marker makes the current version inaccessible through normal file access (download, metadata queries) while
  all non-current versions remain retrievable by their version identifiers. Soft-deleted content is **not** physically
  removed from the storage backend — it continues to exist and **MUST** count against the owner's storage quota
  (`cpt-cf-file-storage-fr-storage-quota`)
- Restore a soft-deleted file by removing the delete marker, making the most recent non-current version the current
  version again. Restore **MUST** require the same authorization as upload
- Permanently delete a specific file version by its version identifier
- Treat version identifiers as opaque strings — the system **MUST NOT** assume any specific format, ordering, or
  parseable structure of version identifiers across storage backends

Automatic garbage collection does **NOT** apply to soft-deleted versions — soft-delete is an intentional owner
action, not an orphaned state. Cleanup of soft-deleted versions is governed by retention policies
(`cpt-cf-file-storage-fr-retention-policies`).

The system **MUST** apply the same authorization, tenant boundary enforcement, and audit requirements to all versioned
operations as to non-versioned file operations.

**Rationale**: File versioning enables recovery from accidental overwrites and deletions, supports audit and compliance
workflows that require historical access to file content, and aligns with capabilities universally available across
major storage backends (S3, GCS, Azure Blob, MinIO, Ceph, Backblaze B2). Logical delete markers (rather than physical
removal) enable restoration and follow the established pattern of S3 versioned deletes, GCS soft-delete, and Azure Blob
soft-delete. Counting soft-deleted content against quota prevents quota bypass through repeated soft-delete cycles.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### File Encryption

- [ ] `p3` - **ID**: `cpt-cf-file-storage-fr-file-encryption`

File encryption requires the server-side encryption backend capability (`cpt-cf-file-storage-fr-backend-capabilities`).
When the encryption capability is available for a backend, the system **MUST** support server-side encryption of file
content at rest, configurable per backend and per policy.

**Rationale**: Regulated environments and security-sensitive deployments require encryption at rest to meet compliance
requirements (GDPR, HIPAA, SOC 2) and protect stored data against unauthorized physical or logical access to the
storage backend.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

### 5.7 Audit

#### Audit Trail

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-audit-trail`

The system **MUST** produce an audit record for every write operation (upload, content replacement, delete, metadata
update). Audit records **MUST** include the operation type, actor identity, file identifier, timestamp, and outcome
(success or failure).

**Rationale**: Audit trails are required for security forensics, compliance reporting, and operational troubleshooting.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Read Audit Logging

- [ ] `p3` - **ID**: `cpt-cf-file-storage-fr-read-audit`

The system **MUST** support optional audit logging for read operations (downloads and metadata queries), configurable
per policy. When enabled by policy, the system **MUST** produce an audit record for every read operation. Because all
content traffic transits FileStorage, read audit applies uniformly to every download (auth-required and public
namespace alike) — there are no per-flow carve-outs.

**Rationale**: Regulated environments and security-sensitive owners require visibility into who accessed their files and
when. Making read audit optional per policy avoids the performance and storage overhead of logging every read
across the platform, while enabling it where compliance demands it.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

### 5.8 Pluggable Storage Backends

#### Backend Abstraction

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-backend-abstraction`

The system **MUST** abstract the storage layer behind a common interface, enabling support for multiple backend types (
S3, GCS, Azure Blob, NFS, FTP, SMB, WebDAV, local filesystem).

**Rationale**: Different deployments and tenants have different storage infrastructure; a common interface allows
backend selection without changing the module's core logic.
**Actors**: `cpt-cf-file-storage-actor-cf-modules`

#### Backend Capabilities

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-backend-capabilities`

The system **MUST** define a capability model for storage backends. Each backend **MUST** declare which optional
capabilities it supports. The system **MUST** support at least the following client-facing capabilities:

- **Versioning** — the backend can maintain multiple versions of a file, identified by opaque version identifiers
- **Multipart Upload** — the backend natively supports chunked upload with independent part transfers and server-side
  assembly
- **Server-Side Encryption** — the backend can encrypt file content at rest using backend-managed or customer-provided
  keys

Backends **MAY** additionally support internal-only capabilities (e.g., presigned URL generation for
backend-to-backend replication, migration, or backup tooling). Internal-only capabilities are used by FileStorage
itself and are **NOT** exposed on the public capability discovery surface — no backend-addressable URL is ever
returned to a client.

Each declared client-facing capability **MUST** be independently configurable as enabled or disabled per backend. A
capability that is supported by the backend but disabled by configuration **MUST** behave identically to an
unsupported capability — the system **MUST NOT** expose or use it. Only capabilities that are both declared by the
backend and enabled in configuration are considered available.

The system **MUST** expose the set of available (declared and enabled) client-facing capabilities per backend so that
consumers can discover them at runtime. When a consumer requests an operation that depends on an unavailable
capability, the system **MUST** return a clear error indicating the capability is unavailable. Capability declarations
**MUST** be part of the backend configuration — not inferred at runtime from probing.

**Rationale**: Storage backends vary widely in feature support. A formal capability model enables FileStorage to adapt
behavior per backend, allows consumers to discover and handle feature availability, and replaces ad-hoc fallback logic
with a consistent, extensible pattern. Separating client-facing capabilities from internal-only ones preserves backend
opacity while keeping internal optimizations available to FileStorage itself.
**Actors**: `cpt-cf-file-storage-actor-cf-modules`

#### Backend Configuration Source

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-backend-config-source`

In P1, storage backend configurations (`type`, `endpoint`, `credentials`, `capabilities`, `hash_policy`) **MUST** be
loaded from a static TOML configuration file at module startup. Adding, removing, or re-configuring a backend
requires a module restart. The configured set is exposed for read-only runtime introspection.

**Rationale**: A static configuration file is the simplest viable mechanism for P1 — no DB or admin-UI dependency.
Read-only HTTP introspection is sufficient for clients to discover available backends and their capabilities without
granting any runtime mutation surface.
**Actors**: `cpt-cf-file-storage-actor-cf-modules`

#### Runtime Backend Configuration

- [ ] `p3` - **ID**: `cpt-cf-file-storage-fr-runtime-backends`

The system **MUST** allow tenants to connect and configure storage backends at runtime without requiring service
rebuild or redeployment. Runtime backend configurations **MUST** be persisted in the metadata database (replacing the
P1 TOML source) and propagated to running module instances.

**Rationale**: Enterprise tenants need to bring their own storage (BYOS) and switch backends based on cost, compliance,
or geographic requirements.
**Actors**: `cpt-cf-file-storage-actor-platform-user`

### 5.9 Access Interfaces

#### REST API

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-rest-api`

The system **MUST** expose a REST API for all file operations (upload, download, delete, metadata management, backend
discovery). The REST surface is split into the auth-required namespace (`/api/file-storage/v1`) and the public
namespace (`/api/file-storage-public/v1`, `GET`/`HEAD` only) — see `cpt-cf-file-storage-fr-public-access`.

**Rationale**: REST is the standard access interface for Cyber Ware modules and platform UI.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Random Read Access

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-range-requests`

Download endpoints **MUST** support random (non-sequential) read access to arbitrary byte ranges of stored content so
that consumers can seek through large files efficiently — most importantly, so that media players can scrub through
videos and audio without re-downloading the file. This applies uniformly to the auth-required download path and the
public namespace (`cpt-cf-file-storage-fr-public-access`); no download channel may opt out.

**Rationale**: Without random read access, every seek in a video forces a full re-download from byte 0, which is
unusable for any clip longer than a few seconds. The protocol-level mechanics (HTTP `Range`/`Content-Range` semantics,
`Accept-Ranges` advertisement, backend-level range translation) are documented in DESIGN.md.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

### 5.10 Cache & Idempotency

#### Conditional Requests

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-conditional-requests`

The system **MUST** support conditional HTTP requests (RFC 7232) for all operations served by FileStorage (downloads,
metadata requests, content-replacement and metadata-update operations, deletes). The system **MUST**:

- Return an `ETag` header with every download and metadata response. ETag is opaque, derived from `(file_id,
  content_revision)`, and **MUST NOT** equal the content hash (which is exposed separately)
- Support `If-None-Match` on `GET`/`HEAD` requests — return `304 Not Modified` when ETag matches
- Support `If-Match` on `GET`/`HEAD` — return `412 Precondition Failed` when ETag does not match
- Require `If-Match` on every write (`PATCH`, `DELETE`) and on multipart-control operations — `412 Precondition Failed`
  on mismatch

**ETag is content-only.** Metadata-only updates bump an internal `metadata_revision` and `last_modified_at` but
**MUST NOT** change the ETag or content hash — both remain tied to the content. Consequently `If-Match` on a
metadata-only update protects against concurrent **content** writes but does **not** detect concurrent metadata
writes; metadata updates are last-write-wins (S3-style).

**Rationale**: Conditional downloads eliminate redundant bandwidth for unchanged files and enable downstream caching by
browsers and reverse proxies. Conditional updates prevent silent data loss when multiple clients modify file metadata
concurrently. Both follow standard HTTP semantics (RFC 7232) understood by all HTTP clients. Since FileStorage manages
file metadata for all backends, ETags are a FileStorage-level feature independent of backend capabilities.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Upload Idempotency

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-upload-idempotency`

The system **MUST** support idempotent uploads. A client **MUST** be able to provide a unique idempotency key with an
upload request. If a subsequent upload request arrives with the same idempotency key, the system **MUST** return the
result of the original upload instead of creating a duplicate file. Idempotency keys **MUST** expire after a
configurable window.

Idempotency keys **MUST** be scoped to the file owner specified in the upload request — the same entity that will own
the resulting file (`cpt-cf-file-storage-fr-file-ownership`). When the owner is a tenant, the key is unique within that
tenant's namespace. When the owner is a user, the key is unique within that user's namespace. The same key value used by
different owners **MUST** be treated as distinct keys. The system **MUST NOT** allow idempotency key lookups to cross
owner boundaries — a request **MUST NOT** be able to detect whether a different owner has used a given key.

**Rationale**: Upload requests can fail ambiguously — the connection drops but the upload succeeds server-side. Without
idempotency, client retries create duplicate files. Idempotency keys enable safe retries for single-part and multipart
uploads across unreliable networks. Owner-scoped key namespacing prevents cross-tenant information leaks and aligns with
the platform's tenant boundary enforcement (`cpt-cf-file-storage-fr-tenant-boundary`).
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

## 6. Non-Functional Requirements

### 6.1 Module-Specific NFRs

#### Metadata Query Latency

- [ ] `p1` - **ID**: `cpt-cf-file-storage-nfr-metadata-latency`

File metadata queries **MUST** complete within 25ms at p95.

**Threshold**: <25ms p95
**Rationale**: Metadata queries are used for pre-fetch validation in latency-sensitive paths (e.g., a module checks file
size before processing).
**Architecture Allocation**: See DESIGN.md § NFR Allocation for how this is realized

#### Content Transfer Latency

- [ ] `p1` - **ID**: `cpt-cf-file-storage-nfr-transfer-latency`

Content download latency **MUST** have no fixed overhead exceeding 50ms at p95; total transfer time is proportional to
file size.

**Threshold**: <50ms + transfer time p95
**Rationale**: FileStorage is called synchronously in request paths of consuming modules; excessive overhead compounds
across requests with multiple files.
**Architecture Allocation**: See DESIGN.md § NFR Allocation for how this is realized

#### URL Availability

- [ ] `p1` - **ID**: `cpt-cf-file-storage-nfr-url-availability`

Stored file URLs (auth-required and public namespaces alike) **MUST** remain accessible for the duration of the
file's retention with availability matching the platform SLA.

**Threshold**: URL availability matches platform SLA for the duration of the retention period
**Rationale**: Consumers depend on URL stability — broken URLs disrupt downstream workflows and user experience.
**Architecture Allocation**: See DESIGN.md § NFR Allocation for how this is realized

#### Audit Completeness

- [ ] `p2` - **ID**: `cpt-cf-file-storage-nfr-audit-completeness`

Audit records **MUST** be emitted for 100% of write operations with no silent drops under normal operating conditions.

**Threshold**: 100% audit coverage for write operations
**Rationale**: Incomplete audit trails undermine compliance and forensic investigations.
**Architecture Allocation**: See DESIGN.md § NFR Allocation for how this is realized

#### Data Durability and Recovery

- [ ] `p1` - **ID**: `cpt-cf-file-storage-nfr-durability`

File content and metadata **MUST** achieve a Recovery Point Objective (RPO) of zero for committed writes — no
acknowledged upload may be silently lost. The Recovery Time Objective (RTO) for service restoration after an outage
**MUST NOT** exceed 15 minutes. These targets apply to the FileStorage service layer; underlying storage backend
durability (e.g., S3 99.999999999% durability) is inherited from the backend and not controlled by FileStorage.

**Threshold**: RPO = 0 (no data loss for committed writes); RTO ≤ 15 minutes
**Rationale**: File loss after a successful upload acknowledgment breaks consumer trust and disrupts downstream
workflows. The RPO=0 target ensures write-ahead semantics where acknowledgment implies durability. The 15-minute RTO
balances recovery speed with operational complexity for a non-user-facing backend service.
**Architecture Allocation**: See DESIGN.md § NFR Allocation for how this is realized

#### Scalability & Capacity

- [ ] `p1` - **ID**: `cpt-cf-file-storage-nfr-scalability`

FileStorage **MUST** support horizontal scaling to handle concurrent file operations without degradation. The system
**MUST** support at least 1,000 concurrent file operations (uploads + downloads + metadata queries combined) per
deployment instance. The system **MUST** scale linearly — adding instances **MUST** proportionally increase throughput
without introducing coordination bottlenecks between instances.

**Threshold**: ≥1,000 concurrent operations per instance; linear horizontal scaling
**Rationale**: As platform adoption grows, file operation volume grows proportionally. Without explicit scalability
requirements, the architecture may adopt patterns (global locks, shared mutable state) that prevent horizontal scaling.
**Architecture Allocation**: See DESIGN.md § NFR Allocation for how this is realized

### 6.2 NFR Exclusions

None — all project-default NFRs apply to this module.

### 6.3 Applicability Notes

The following NFR categories from the platform checklist are **not applicable** to this module:

| Category                 | Rationale                                                                                                                                                                                                                                                                                               |
|--------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| **Safety**               | FileStorage is a data storage service with no physical actuators, safety-critical control loops, or human safety implications.                                                                                                                                                                          |
| **UX**                   | FileStorage is a backend service consumed via SDK and APIs. It has no user-facing UI; UX concerns are the responsibility of consuming modules and platform UI.                                                                                                                                          |
| **Internationalization** | FileStorage stores and returns opaque binary content and metadata strings. It does not render, translate, or localize content. File names and metadata values are preserved as-is.                                                                                                                      |
| **Privacy by Design**    | FileStorage treats all files as opaque blobs and does not inspect, index, or process file content. Privacy controls (data minimization, consent, right to erasure) are enforced at the platform and consuming-module level. Tenant isolation and access control are covered by functional requirements. |
| **Compliance**           | FileStorage does not implement domain-specific compliance logic (GDPR, HIPAA, SOX). It provides the building blocks (audit trail, tenant isolation, retention policies, encryption) that enable consuming modules and platform operators to achieve compliance.                                         |
| **Operations**           | Operational concerns (deployment, monitoring, alerting, runbooks) follow platform-wide standards and are not module-specific.                                                                                                                                                                           |
| **Maintainability**      | Maintainability follows platform-wide coding standards, testing requirements, and CI/CD practices. No module-specific maintainability NFRs beyond the platform baseline.                                                                                                                                |

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### FileStorage SDK Trait

- [ ] `p1` - **ID**: `cpt-cf-file-storage-interface-sdk-trait`

**Type**: Rust trait (SDK crate)
**Stability**: unstable
**Description**: Async trait providing upload, download (with Range), delete, metadata read/update, listing, and
backend-capability discovery.
**Breaking Change Policy**: Major version bump required for trait signature changes.

#### Auth-Required REST API

- [ ] `p1` - **ID**: `cpt-cf-file-storage-interface-rest-api`

**Type**: REST API (OpenAPI 3.0)
**URL Prefix**: `/api/file-storage/v1`
**Stability**: unstable
**Description**: HTTP REST API for owner-authenticated file operations and metadata management.
**Breaking Change Policy**: Major version bump required for endpoint removal or incompatible schema changes.

#### Public REST API

- [ ] `p1` - **ID**: `cpt-cf-file-storage-interface-public-rest-api`

**Type**: REST API (no authentication)
**URL Prefix**: `/api/file-storage-public/v1`
**Stability**: unstable
**Description**: `GET` and `HEAD` only, gated by per-file `public_access` flag
(`cpt-cf-file-storage-fr-public-access`). The platform API Gateway routes this prefix without enforcing JWT
authentication.
**Breaking Change Policy**: Major version bump required for endpoint removal or incompatible schema changes.

### 7.2 External Integration Contracts

#### Cyber Ware Module Contract

- [ ] `p1` - **ID**: `cpt-cf-file-storage-contract-cf-modules`

**Direction**: provided by library (consumed by Cyber Ware modules)
**Protocol/Format**: In-process Rust SDK trait via ClientHub
**Compatibility**: Trait versioned with SDK crate; breaking changes require coordinated release with consuming modules.

#### Authorization Service Contract

- [ ] `p1` - **ID**: `cpt-cf-file-storage-contract-authz`

**Direction**: required from external service (Authorization Service)
**Protocol/Format**: Access decision requests for `gts.cf.fstorage.file.type.v1~` resources
**Compatibility**: Contract follows platform authorization protocol; changes require coordinated release.

#### Usage Collector Contract

- [ ] `p2` - **ID**: `cpt-cf-file-storage-contract-usage-collector`

**Direction**: required from external service (Usage Collector)
**Protocol/Format**: Asynchronous per-owner usage reports (storage consumption per owner, including ownership-transfer
debits/credits per `cpt-cf-file-storage-fr-usage-reporting`)
**Compatibility**: Contract follows platform usage reporting protocol; changes require coordinated release.

#### Quota Enforcement Contract

- [ ] `p2` - **ID**: `cpt-cf-file-storage-contract-quota-enforcement`

**Direction**: required from external service (Quota Enforcement)
**Protocol/Format**: Synchronous per-owner quota check requests before storage-consuming operations
(per `cpt-cf-file-storage-fr-storage-quota`)
**Compatibility**: Contract follows platform quota enforcement protocol; changes require coordinated release.

#### EventBroker Contract

- [ ] `p2` - **ID**: `cpt-cf-file-storage-contract-eventbroker`

**Direction**: bidirectional (publishes file events; consumes platform events such as owner deletion)
**Protocol/Format**: Asynchronous event publishing and consumption via EventBroker module
**Compatibility**: Contract follows platform event protocol; event schema changes require coordinated release.

#### Serverless Runtime Contract

- [ ] `p2` - **ID**: `cpt-cf-file-storage-contract-serverless-runtime`

**Direction**: required from external service (Serverless Runtime)
**Protocol/Format**: Workflow invocation for configurable lifecycle operations (e.g., owner deletion disposition)
**Compatibility**: Contract follows platform Serverless Runtime invocation protocol; changes require coordinated release.

## 8. Use Cases

### Upload and Make Public

- [ ] `p1` - **ID**: `cpt-cf-file-storage-usecase-upload-public`

**Actor**: `cpt-cf-file-storage-actor-platform-user`

**Preconditions**:

- User is authenticated
- Authorization Service grants write access

**Main Flow**:

1. User uploads file content with metadata (name, mime_type, GTS file type)
2. FileStorage validates the GTS file type format
3. FileStorage checks authorization for write on `gts.cf.fstorage.file.type.v1~` with the file type in resource context
4. *(Phase 2)* FileStorage validates file against policies (type, size); in phase 1 all uploads are accepted
5. FileStorage persists content, assigns ownership, stores metadata
6. *(Phase 2)* FileStorage emits audit record for the upload
7. FileStorage returns persistent URL and file identifier
8. User toggles `public_access = true` via metadata update
9. The file becomes readable via the public namespace (`/api/file-storage-public/v1/files/{id}`)

**Postconditions**:

- File stored with metadata and ownership
- `public_access = true`; file is anonymously readable via the public namespace
- *(Phase 2)* Audit records emitted for creation and public-access toggle

**Alternative Flows**:

- **Missing or invalid GTS file type**: FileStorage rejects the upload with a validation error
- **Authorization denied**: FileStorage returns access-denied error
- *(Phase 2)* **Policy violation**: FileStorage returns error indicating which policy was violated (type or size)

### Fetch File for Module Processing

- [ ] `p1` - **ID**: `cpt-cf-file-storage-usecase-fetch-media`

**Actor**: `cpt-cf-file-storage-actor-cf-modules`

**Preconditions**:

- File exists at the specified URL

**Main Flow**:

1. Module calls download with a file URL
2. FileStorage checks authorization for read on `gts.cf.fstorage.file.type.v1~` with the file's GTS type in resource context
3. FileStorage retrieves file content from the storage backend
4. FileStorage returns content with metadata (mime_type, size, GTS file type)

**Postconditions**:

- Content and metadata returned to the requesting module

**Alternative Flows**:

- **File not found**: FileStorage returns file_not_found error
- **Authorization denied**: FileStorage returns access-denied error

### Validate File Metadata Before Processing

- [ ] `p1` - **ID**: `cpt-cf-file-storage-usecase-get-metadata`

**Actor**: `cpt-cf-file-storage-actor-cf-modules`

**Preconditions**:

- File exists at the specified URL

**Main Flow**:

1. Module calls get_metadata with a file URL
2. FileStorage checks authorization for read on `gts.cf.fstorage.file.type.v1~` with the file's GTS type in resource context
3. FileStorage returns metadata (name, size, mime_type, GTS file type, owner, availability) without transferring content

**Postconditions**:

- Metadata returned; no content transferred

**Alternative Flows**:

- **File not found**: FileStorage returns file_not_found error
- **Authorization denied**: FileStorage returns access-denied error

### Delete a File

- [ ] `p1` - **ID**: `cpt-cf-file-storage-usecase-delete-file`

**Actor**: `cpt-cf-file-storage-actor-platform-user`

**Preconditions**:

- User is authenticated
- User owns the file

**Main Flow** (non-versioned file):

1. Owner requests deletion of a file by its identifier
2. FileStorage checks authorization for delete on `gts.cf.fstorage.file.type.v1~`
3. FileStorage deletes the file content from the storage backend
4. FileStorage removes file metadata and ownership records
5. *(Phase 2)* FileStorage emits audit record for the deletion

**Postconditions**:

- File content removed from storage backend
- Metadata and ownership records removed; subsequent public-namespace requests for the file return `404`
- *(Phase 2)* Audit record emitted

**Alternative Flow — versioned file, no version identifier** (`cpt-cf-file-storage-fr-file-versioning`):

1. Owner requests deletion of a file by its identifier (no version identifier supplied)
2. FileStorage checks authorization for delete on `gts.cf.fstorage.file.type.v1~`
3. FileStorage places a soft-delete marker on the current version
4. *(Phase 2)* FileStorage emits audit record for the soft-delete

**Postconditions**:

- Current version inaccessible through normal file access; non-current versions remain retrievable by version ID
- Shareable links remain associated but resolve to the soft-deleted state
- Content is **not** physically removed and continues to count against storage quota
  (`cpt-cf-file-storage-fr-storage-quota`)
- *(Phase 2)* Audit record emitted

**Alternative Flow — versioned file, with version identifier**:

1. Owner requests deletion of a specific version by file identifier and version identifier
2. FileStorage checks authorization for delete on `gts.cf.fstorage.file.type.v1~`
3. FileStorage permanently removes the specified version from the storage backend
4. *(Phase 2)* FileStorage emits audit record for the permanent version deletion

**Postconditions**:

- Specified version permanently removed; remaining versions unaffected
- If the deleted version was the last remaining version, the file is fully removed (equivalent to non-versioned
  deletion postconditions)
- *(Phase 2)* Audit record emitted

**Alternative Flows — error cases**:

- **Authorization denied**: FileStorage returns access-denied error
- **File not found**: FileStorage returns file_not_found error
- **Version not found**: FileStorage returns version_not_found error
- **Cross-tenant attempt**: FileStorage returns access-denied error (tenant boundary enforcement)

### Manage Public Access

- [ ] `p1` - **ID**: `cpt-cf-file-storage-usecase-manage-public-access`

**Actor**: `cpt-cf-file-storage-actor-platform-user`

**Preconditions**:

- User is authenticated
- User owns the file

**Main Flow** (revoke public access):

1. Owner reads current file metadata
2. Owner sends a metadata update to toggle `public_access = false`
3. FileStorage checks authorization for the owner on `gts.cf.fstorage.file.type.v1~`
4. FileStorage updates the flag; the public namespace returns `404` for subsequent requests
5. *(Phase 2)* FileStorage emits an audit record for the change

**Postconditions**:

- `public_access = false`; public namespace responds with `404`
- Auth-required access for the owner is unaffected
- *(Phase 2)* Audit record emitted

**Alternative Flows**:

- **Enable public access**: same flow with `public_access = true`
- **Toggle `download_availability`**: same flow with `download_availability = false`; owner can still download, public
  namespace returns `404`
- **Authorization denied**: FileStorage returns access-denied error

### Multi-Backend Deployment

- [ ] `p1` - **ID**: `cpt-cf-file-storage-usecase-backend-config`

**Actor**: `cpt-cf-file-storage-actor-cf-modules`

**Preconditions**:

- FileStorage is deployed with a configured storage backend

**Main Flow**:

1. Deployment A configures FileStorage with an S3-compatible backend (e.g., AWS S3)
2. Deployment B configures FileStorage with a different backend (e.g., Azure Blob Storage)
3. Both deployments expose identical FileStorage SDK and REST APIs
4. Cyber Ware modules interact with FileStorage through the SDK trait without awareness of the underlying backend
5. Upload, download, delete, metadata, and link operations behave identically regardless of backend

**Postconditions**:

- All functional requirements are met identically across different backend configurations
- Consuming modules require zero code changes when the backend changes

**Alternative Flows**:

- **Backend-specific feature unavailable**: FileStorage returns a clear error indicating the capability is unavailable
  (e.g., multipart upload or versioning request rejected when backend does not declare the capability)

### Configure Policy

- [ ] `p2` - **ID**: `cpt-cf-file-storage-usecase-configure-policy`

**Actor**: `cpt-cf-file-storage-actor-platform-user`

**Preconditions**:

- User has tenant administration privileges (for tenant-level policy) or is an authenticated user (for user-level
  policy)

**Main Flow**:

1. Tenant admin or user defines policies: allowed file types, size limits (global and per-type), enabled event types,
   and permitted sharing models
2. FileStorage validates and stores the policy configuration
3. Subsequent file operations are enforced against the effective policy (most restrictive per aspect across tenant and
   user levels)

**Postconditions**:

- Policy active and enforced on all file operations

**Alternative Flows**:

- **Invalid policy**: FileStorage returns validation error with details

## 9. Acceptance Criteria

- [ ] File upload returns persistent URL and stores metadata (name, size, type, dates, owner)
- [ ] File download returns content with correct metadata
- [ ] File deletion of a non-versioned file permanently removes content
- [ ] File deletion of a versioned file without version identifier places a soft-delete marker (no physical removal)
- [ ] Authorization checked for every file operation via Authorization Service
- [ ] Tenant boundary enforced — cross-tenant access through the auth-required namespace rejected
- [ ] Public access flag (`public_access`) gates anonymous reads via the public namespace; `false` returns `404`
- [ ] Audit record emitted for every write operation
- [ ] Policies enforce file type and size restrictions on upload (most restrictive wins across tenant and user levels)
- [ ] All content traffic flows through FileStorage; no backend-addressable URL is returned to any client
- [ ] file_not_found error returned for non-existent files
- [ ] access_denied error returned for unauthorized operations
- [ ] Metadata-only queries complete without transferring file content
- [ ] Content is mutable through dedicated content-replacement operations; ETag (content-derived) changes on every
  content write; metadata-only updates do not change ETag or content hash
- [ ] Custom metadata is updatable by the file owner; system-managed metadata is not user-updatable
- [ ] Custom metadata update changes the file's last modified date
- [ ] File ownership (`owner_kind`, `owner_id`) is immutable after creation except through explicit ownership transfer
  or owner deletion workflows; `tenant_id` is never mutable
- [ ] Every file has a mandatory GTS file type assigned at upload time; uploads without a file type are rejected
- [ ] GTS file type is immutable after creation
- [ ] Authorization requests include the file's GTS type, enabling per-type access decisions
- [ ] A module authorized only for type A cannot access files of type B
- [ ] FileStorage SDK and REST API behave identically regardless of configured storage backend
- [ ] File listing returns metadata only, is paginated, and requires a mandatory owner-kind filter (`user` or `app`)
- [ ] Multipart upload assembles parts into a complete file with correct metadata
- [ ] Upload rejected when declared mime_type does not match actual file content
- [ ] File owner can toggle download availability via metadata update; owner can always download regardless of the
  flag; public namespace returns `404` when `download_availability=false`
- [ ] Each backend declares its supported client-facing capabilities (versioning, multipart upload, server-side
  encryption); internal-only capabilities are not surfaced on public discovery
- [ ] Consumers can discover backend capabilities at runtime
- [ ] Operations requiring an unsupported capability return a clear error
- [ ] File versioning creates a new version on each content-replacement when backend versioning capability is enabled;
  metadata-only updates do not create a new backend version
- [ ] All versions of a file are listable with version ID, size, timestamp, and current-version flag
- [ ] Soft-delete places a logical delete marker; current version becomes inaccessible but content is not physically
  removed
- [ ] Soft-deleted content counts against storage quota
- [ ] Restore of a soft-deleted file removes the delete marker and reinstates the previous current version
- [ ] Garbage collection does not clean up soft-deleted versions
- [ ] Permanent delete of a specific version removes only that version
- [ ] Declared capabilities are independently configurable (enable/disable) per backend
- [ ] A capability disabled by configuration behaves identically to an unsupported capability
- [ ] Download and metadata responses include `ETag` header derived from `(file_id, content_revision)` and not equal
  to the content hash
- [ ] Conditional download with `If-None-Match` returns `304 Not Modified` when file is unchanged
- [ ] `If-Match` is required on writes (`PATCH`/`DELETE`); missing or mismatching `If-Match` returns `412`
- [ ] Retried upload with the same idempotency key returns the original result without creating a duplicate file
- [ ] Retried upload with the same idempotency key by a different owner does not return or create the original owner's
  file
- [ ] Owner deletion event from EventBroker triggers a configurable Serverless Runtime workflow for file disposition
- [ ] Files of a deleted owner are retained as orphaned when no workflow is configured
- [ ] Server-side encryption is applied when the encryption capability is available and enabled for the backend
- [ ] Upload rejected when storage quota would be exceeded (Quota Enforcement service check)
- [ ] Usage report emitted asynchronously on every storage-consuming write operation; file operations not blocked if
  Usage Collector is unavailable
- [ ] Ownership transfer emits usage reports for both previous and new owner
- [ ] File events emitted to EventBroker on write operations (upload, update, delete) when enabled by owner policy
- [ ] HTTP Range requests return partial content for downloads (auth-required and public namespaces); seeking and
  resumable downloads supported; `Accept-Ranges: bytes` set on every download response
- [ ] Retention policies automatically expire and delete files based on configured age, inactivity, or custom metadata
  criteria; per-file retention overrides are honored
- [ ] Storage backends in P1 are loaded from a static TOML configuration file at module startup; in P3, backends can
  be connected and configured at runtime via admin API without service rebuild
- [ ] File ownership transferable by current owner to another user or app within the same tenant; transfer requires
  authorization of both parties and emits an audit record
- [ ] Custom metadata operations rejected when exceeding configurable limits (max pairs, key length, value length, total
  size)
- [ ] Read audit records emitted for every download (auth-required and public namespaces) when enabled by policy

## 10. Dependencies

| Dependency            | Description                                                        | Criticality |
|-----------------------|--------------------------------------------------------------------|-------------|
| ModKit Framework      | Module lifecycle, ClientHub for service registration               | p1          |
| Authorization Service | Access decisions for `gts.cf.fstorage.file.type.v1~` resources     | p1          |
| Audit Infrastructure  | Platform audit event sink                                          | p2          |
| Usage Collector       | Receives storage usage reports for metering and billing            | p2          |
| Quota Enforcement     | Per-owner storage quota enforcement                                | p2          |
| EventBroker           | Publishes and consumes file/platform events                        | p2          |
| Serverless Runtime    | Executes configurable workflows for lifecycle operations           | p2          |

## 11. Assumptions

- Authorization Service is available and supports `gts.cf.fstorage.file.type.v1~` resource type
- All file access respects tenant boundaries at the platform level
- Initial storage backend is configured at deployment time; runtime backend switching is phase 2
- Auth-required file URLs are internal to Cyber Ware; external anonymous access is via the public namespace
  (`cpt-cf-file-storage-fr-public-access`); per-principal external sharing is delivered by the FileShare module (P3)
- Policy configuration is available to tenant administrators and users through the platform

## 12. Risks

| Risk                                                                | Impact                                                         | Mitigation                                                                                                                                              |
|---------------------------------------------------------------------|----------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------|
| Storage service unavailability blocks all file-dependent operations | High — multimodal AI, document workflows disrupted             | Design for graceful degradation; clear error propagation to consumers                                                                                   |
| Large file sizes increase request latency for consuming modules     | Medium — slow responses for multimodal and document operations | Metadata pre-fetch enables size validation; streaming support for large files                                                                           |
| Backend credential compromise enables unauthorized backend access  | High — data exposure                                           | Backend credentials held only by FileStorage and never exposed to clients (proxy model — see DESIGN.md); standard credential rotation procedures apply at the FileStorage layer |
| Policy misconfiguration blocks legitimate uploads                   | Medium — user frustration                                      | Policy validation on save; clear error messages identifying which policy was violated                                                                   |

## 13. Open Questions

None.

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/)
- **Features**: [features/](./features/)
