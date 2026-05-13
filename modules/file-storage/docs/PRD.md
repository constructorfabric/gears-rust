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
  - [Inter-User Sharing (delegated to FileShare)](#inter-user-sharing-delegated-to-fileshare)
  - [Multi-Backend Deployment](#multi-backend-deployment)
  - [Configure Policy](#configure-policy)
- [9. Acceptance Criteria](#9-acceptance-criteria)
  - [Core (P1)](#core-p1)
  - [Public access (P1)](#public-access-p1)
  - [Phase 2](#phase-2)
  - [Phase 3](#phase-3)
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
| Owner               | The principal that owns a file: `owner_kind ∈ {user, app}` plus `owner_id`. `user` is a platform user; `app` is a Cyber Ware module. Every file also has a separate immutable `tenant_id` for tenant-boundary enforcement                                                                |
| Public Access       | A per-file boolean flag (`public_access`) that, when true, allows anonymous read access via the dedicated public namespace `/api/file-storage-public/v1/files/{id}`. Not time-bounded; toggled by the owner                                                                              |
| FileShare           | Separate Cyber Ware module (P3, `modules/file-share/`) that delivers per-principal grant-based sharing on top of FileStorage. FileShare proxies content from FileStorage; FileStorage has no dependency on FileShare                                                                     |
| Storage Backend     | An underlying storage system (S3, GCS, Azure Blob, NFS, FTP, SMB, WebDAV, local filesystem) used for persisting file content                                                                                                                                                            |
| Policy              | A set of rules (allowed file types, size limits, events, public-access restriction) that constrain file operations; applicable at the tenant level and the user level independently — when both apply, the most restrictive value per aspect wins                                       |
| File Version        | An immutable snapshot of file content created on each content replacement when backend versioning is enabled; identified by an opaque version identifier assigned by the storage backend                                                                                                |
| Content Revision    | Monotonic per-file counter, incremented **only** when content changes. Backs the `ETag` derivation                                                                                                                                                                                       |
| Metadata Revision   | Monotonic per-file counter, incremented on every successful metadata or content write. Independent of `ETag`                                                                                                                                                                            |
| ETag                | Opaque cache validator derived from `(file_id, content_revision)`, returned on every download and `HEAD` response. **Not equal** to the content hash. Tied to content, not metadata — metadata-only updates do not change ETag                                                          |
| Version Identifier  | An opaque string assigned by the storage backend that uniquely identifies a specific version of a file; format varies by backend and must not be parsed or assumed                                                                                                                      |
| File Type (GTS)     | A GTS type identifier assigned to every file at upload time that classifies the file by domain, actor, and purpose (e.g., `gts.cf.fstorage.file.type.v1~x.genai.llm.autogenerated.v1~`); used by the Authorization Service to enforce per-type access control between actors and modules |
| Backend Capability  | An optional feature that a storage backend may or may not support (e.g., versioning, multipart upload); FileStorage discovers available client-facing capabilities per backend and adapts its behavior accordingly                                                                      |

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
- Public access per file via boolean flag served on a dedicated unauthenticated namespace
- Tenant-level public-access restriction policy
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
- Ownership transfer within the same tenant
- Custom metadata limits
- File versioning (content-only; backend-driven)
- Conditional requests (ETags) for cache validation and content-write concurrency protection
- Upload idempotency
- Owner deletion handling via EventBroker and Serverless Runtime workflows
- File encryption (server-side, per backend capability and configuration)

### 4.2 Out of Scope

- Content transformation or transcoding
- CDN distribution
- Full-text search within file content
- Scope-based shareable links (`public`, `tenant`, `tenant-hierarchy` link tokens) — replaced by the per-file
  `public_access` flag (`cpt-cf-file-storage-fr-public-access`) for anonymous sharing
- Per-principal grant-based sharing (one user granting access to specific other users/groups) — owned by the
  **FileShare** module (P3, `modules/file-share/`)
- S3-compatible and WebDAV protocol facades on top of FileStorage — out of scope for this module; if needed in a
  future phase they will be implemented as separate modules consuming FileStorage's SDK

## 5. Functional Requirements

### 5.1 Core File Operations

#### Upload File

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-upload-file`

The system **MUST** accept file content with metadata and persist it, returning a persistent, accessible URL. File
content is mutable after creation through dedicated content-replacement operations (e.g., `PATCH /files/{id}` with a
`content` part). When the backing storage backend declares `versioning_native = true`
(`cpt-cf-file-storage-fr-backend-capabilities`), each content replacement creates a new immutable version retrievable by
its opaque version identifier (`cpt-cf-file-storage-fr-file-versioning`); when versioning is unavailable or disabled,
the prior content is permanently overwritten. Metadata-only updates **MUST NOT** modify content and **MUST NOT** create
a new backend version.

**Rationale**: All platform modules and users need to store files — modules store generated content, documents, and
artifacts, users upload files directly. Allowing content replacement aligns with S3-style object semantics and avoids
forcing consumers to rotate file identifiers (and update every downstream reference) just to change a file's contents.
Coupling content replacement to backend versioning preserves recoverability where the backend supports it, without
imposing version-management overhead where it does not.
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
(`cpt-cf-file-storage-fr-file-versioning`), deletion without a version identifier places a soft-delete marker; the
current version becomes inaccessible through normal access, while non-current versions remain retrievable by their
version identifiers. Permanent removal of a specific version requires passing its version identifier explicitly.

**Rationale**: Owners need to remove files that are no longer needed. Versioned files default to soft-delete to enable
recovery from accidental deletions. Permanent removal is an explicit, version-targeted operation.
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

The list **MUST** be implicitly scoped to the requesting caller's tenant — cross-tenant listing is not supported on
this endpoint (`cpt-cf-file-storage-fr-tenant-boundary`). The response **MUST** be paginated following the platform API
guidelines (cursor-based or offset-based pagination with configurable page size). The system **MUST** support optional
additional filters (mime_type, GTS file type, `public_access` flag, date range, custom metadata keys).

**Rationale**: Users and modules need to discover and browse files they own or have access to. Mandatory owner type
filtering prevents unbounded queries across all files and aligns with the ownership model
(`cpt-cf-file-storage-fr-file-ownership`).
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
every upload. All upload traffic flows through FileStorage's proxy (see DESIGN.md), so content inspection always
applies. If the declared type does not match the detected type, the system **MUST** reject the upload with an error
indicating the mismatch.

For multipart uploads (`cpt-cf-file-storage-fr-multipart-upload`), the system **MUST** validate the declared mime_type
against the content of the **first uploaded part**, which contains the file's magic bytes / file signature. Validation
**MUST** occur when the first part is received — before subsequent parts are accepted. If the detected type does not
match the declared mime_type, the system **MUST** abort the multipart upload (`abortMultipartUpload`) and reject all
subsequent parts.

**Rationale**: Without content inspection, a client can declare `image/png` but upload an executable, trivially
bypassing file type policies. Content-type validation ensures declared types are trustworthy for downstream consumers
and policy enforcement. First-part validation for multipart uploads provides the same level of guarantee as single-part
validation — magic bytes reside at the start of the file and are always contained in the first part because backends
that support multipart upload (`cpt-cf-file-storage-fr-backend-capabilities`) enforce a minimum part size (e.g., 5 MB
for S3) that far exceeds the longest magic-byte sequence (~12 bytes). Backends without native multipart support reject
multipart uploads entirely, so no fallback is needed. Post-assembly re-validation would require downloading the
assembled file from the backend, negating the efficiency benefits of multipart upload.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

### 5.2 Ownership & Access Control

#### File Ownership

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-file-ownership`

The system **MUST** associate every file with:

- **`tenant_id`** — the tenant the file belongs to. Mandatory; used for tenant-boundary enforcement
  (`cpt-cf-file-storage-fr-tenant-boundary`). Set at creation time; **immutable** thereafter.
- **`owner_kind`** ∈ `{user, app}` — whether the owner principal is a platform user or a Cyber Ware module
  (application).
- **`owner_id`** — the UUID of the owning principal: a user identifier when `owner_kind = user`, a module identifier
  when `owner_kind = app` (e.g., LLM Gateway storing its generated media has `owner_kind = app` with `owner_id` set to
  the LLM Gateway module identifier).

`(owner_kind, owner_id)` is mutable only via explicit ownership transfer (`cpt-cf-file-storage-fr-ownership-transfer`)
or owner deletion workflows (`cpt-cf-file-storage-fr-owner-deletion`). `tenant_id` is **never** mutable — moving a file
between tenants requires re-upload by the receiving tenant.

**Rationale**: Ownership determines who can manage (delete, update metadata) a file and establishes the basis for
access control decisions. Separating `tenant_id` from `(owner_kind, owner_id)` reflects how Cyber Ware actually scopes
data: tenant is the hard boundary for isolation and billing, while ownership identifies the specific principal within
the tenant. The `user|app` distinction lets modules own platform-generated content (LLM outputs, reports, generated
media) without having to attribute it to an artificial human user, while still keeping file ownership a first-class
principal-typed concept.
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

The system **MUST** enforce tenant isolation: all file operations (read, write, delete, metadata update) **MUST NOT**
cross tenant boundaries. A principal in one tenant **MUST NOT** access files owned by another tenant through the
auth-required namespace. The only cross-tenant or unauthenticated access paths are:

- The public namespace (`/api/file-storage-public/v1`) — gated by the file's `public_access` flag
  (`cpt-cf-file-storage-fr-public-access`); access is anonymous, not cross-tenant in a tenant-aware sense
- Per-principal grant-based sharing through the **FileShare** module (P3); FileShare runs its own access checks and
  proxies content from FileStorage on the grantee's behalf

**Rationale**: Multi-tenant platforms require strict data isolation. Cross-tenant sharing is an explicit, audited
operation owned by FileShare (which controls who-grants-what-to-whom), not an implicit property of FileStorage's tenant
scoping.
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

The system **MUST** allow the current file owner to transfer ownership of a file to another principal — another user
(`owner_kind = user`) or another module (`owner_kind = app`) within the **same tenant**. Cross-tenant transfer is
**NOT** supported (see `cpt-cf-file-storage-fr-tenant-boundary`); moving a file between tenants requires re-upload.
Ownership transfer **MUST** be an audited operation and **MUST** require authorization of both the current owner and
the receiving principal.

**Rationale**: As teams and modules evolve, files may need to change hands — e.g., when a user leaves the organization
or when files generated by an application module need to be reassigned to a successor module. Restricting transfers to
within the file's tenant preserves the tenant-isolation invariant.
**Actors**: `cpt-cf-file-storage-actor-platform-user`

### 5.3 Sharing

FileStorage supports exactly two sharing models:

- **Public access** — a per-file boolean flag toggled by the owner. When set, the file is readable through a dedicated
  unauthenticated namespace via the file's UUID. Described below as `cpt-cf-file-storage-fr-public-access`.
- **Per-principal grant-based sharing** — one user explicitly granting access to specific other users (and groups in
  later phases). This model is **NOT** owned by FileStorage; it is provided by the **FileShare** module (P3, separate
  module under `modules/file-share/`). FileShare proxies content from FileStorage on the grantee's behalf and is
  responsible for enforcing its own grant model.

There are **no** scope-based shareable links (public, tenant, tenant-hierarchy) and **no** link tokens served by
FileStorage. Tenant-internal access is governed entirely by `cpt-cf-file-storage-fr-authorization` and
`cpt-cf-file-storage-fr-tenant-boundary`.

#### Public Access Flag

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-public-access`

The system **MUST** expose a per-file boolean property `public_access` that the file owner can toggle through standard
metadata updates (`cpt-cf-file-storage-fr-update-metadata`). When `public_access = true`, the file's content is readable
through a dedicated **public namespace** (`/api/file-storage-public/v1/files/{file_id}`, `GET` and `HEAD` only) without
authentication. The platform API Gateway **MUST** route requests under this namespace prefix without enforcing JWT
authentication. When `public_access = false`, requests to the public namespace **MUST** respond with `404 Not Found`
regardless of the file's actual existence — never `401`/`403` — to avoid leaking the existence of private files. The
`public_access` flag has no expiration and is **NOT** time-bounded; revoking public access requires the owner to set
the flag back to `false`.

The public namespace **MUST**:

- Support `GET` and `HEAD` only — all write methods return `405 Method Not Allowed`
- Support `Range` and `If-None-Match` semantics identical to the auth-required download path
  (`cpt-cf-file-storage-fr-range-requests`, `cpt-cf-file-storage-fr-conditional-requests`)
- Omit `X-FS-GTS-File-Type`, `X-FS-Public-Access`, custom `X-FS-Meta-*` headers, and any tenant-internal classifier from
  responses, to avoid leaking owner-private context
- Honor the `download_availability` kill-switch — when `download_availability = false`, public-namespace requests
  **MUST** respond with `404` regardless of `public_access`

The file's UUID acts as the access secret on the public namespace — random 128-bit UUIDs make enumeration
computationally infeasible. Owners who need to revoke access without re-uploading the file **MUST** toggle
`public_access` to `false`.

**Rationale**: A boolean flag controlled by standard metadata updates is the simplest possible mechanism for "the
internet can read this" — no link tables, no token expiry, no scope enums. Routing through a distinct URL prefix lets
the API Gateway bypass JWT validation cleanly without per-request authentication-mode introspection inside FileStorage.
Returning `404` (rather than `401`/`403`) when the flag is off preserves the privacy of files whose UUIDs are guessed
or leaked through logs.
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

#### Public Access Restriction Policy

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-public-access-policy`

The system **MUST** allow tenant administrators to disable public access (`cpt-cf-file-storage-fr-public-access`) at the
tenant level. When public access is disabled by tenant policy, owner attempts to set `public_access = true` on any file
within the tenant **MUST** be rejected, and previously-public files **MUST** behave as if `public_access = false` (i.e.,
return `404` on the public namespace).

**Rationale**: Tenants in regulated environments may need to prohibit anonymous public access to enforce data
governance policies. Centralizing the kill-switch at the policy level keeps per-file flags free of policy logic.
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

- `file_id` (UUID, immutable)
- `tenant_id` (UUID, immutable)
- `owner_kind` ∈ `{user, app}` and `owner_id` (UUID; mutable only via ownership transfer)
- `name` (original upload name)
- `size` (bytes, content-only)
- `mime_type` (declared, validated against magic bytes per `cpt-cf-file-storage-fr-content-type-validation`)
- `gts_file_type` (`cpt-cf-file-storage-fr-file-type-classification`)
- `created_at` (timestamp; immutable)
- `last_modified_at` (timestamp; bumped on any successful write — content or metadata)
- `content_revision` (monotonic counter; incremented **only** when content changes)
- `metadata_revision` (monotonic counter; incremented on every successful write — including content writes, which
  bump both counters)
- `hash_algorithm` and `hash_value` (P1: `SHA-256` only — see ADR-0002 for the algorithm-selection rationale; both
  reflect the **content** and change only when content changes)
- `etag` (opaque cache validator, derived from `(file_id, content_revision)`; explicitly **not** equal to
  `hash_value`; tied to content, not to metadata — see `cpt-cf-file-storage-fr-conditional-requests`)
- `public_access` (boolean; gates the public namespace per `cpt-cf-file-storage-fr-public-access`)
- `download_availability` (boolean; kill-switch for non-owner access — see `cpt-cf-file-storage-fr-update-metadata`)
- `content_state` ∈ `{pending, available}` (pending only exists for files created via P2 multipart initiate without an
  uploaded content body; in P1 every successfully created file is `available`)

In addition, the system **MUST** support user-defined custom metadata as arbitrary key-value string pairs. Custom
metadata **MUST** be specifiable at upload time and updatable after upload (via JSON Merge Patch semantics on
`cpt-cf-file-storage-fr-update-metadata`). The system **MUST** return custom metadata alongside system-managed metadata
in metadata queries.

**HTTP header exposure** on every download (`GET`/`HEAD`) response served by FileStorage:

- System metadata is exposed via `X-FS-*` headers (e.g., `X-FS-File-Id`, `X-FS-Owner-Kind`, `X-FS-Owner-Id`,
  `X-FS-GTS-File-Type`, `X-FS-Hash-Algorithm`, `X-FS-Hash-Value`, `X-FS-Content-Revision`, `X-FS-Metadata-Revision`,
  `X-FS-Public-Access`, `X-FS-Download-Availability`, `X-FS-Created-At`).
- Custom metadata is exposed as one `X-FS-Meta-<key>` header per entry. Values that contain non-ASCII characters
  **MUST** be transmitted using the RFC 8187 `*=UTF-8''<percent-encoded>` extended-value format
  (e.g., `X-FS-Meta-Name*=UTF-8''%D0%9F%D1%80%D0%B8%D0%B2%D0%B5%D1%82`). Pure-ASCII values **MAY** be sent without the
  extended form.
- On the public namespace (`/api/file-storage-public/v1`), `X-FS-GTS-File-Type`, `X-FS-Public-Access`, and all
  `X-FS-Meta-*` headers **MUST** be omitted to avoid leaking owner-private classifiers.

**Rationale**: Rich metadata enables file browsing, search, validation, and governance across the platform. Custom
metadata enables consumers to attach domain-specific context (tags, categories, processing status, source identifiers)
without schema changes — following the established pattern used by S3 object metadata, GCS custom metadata, and Azure
Blob metadata. Splitting revisions into `content_revision` and `metadata_revision` makes ETag tied to content (a
stable cache-validation key) while still letting clients detect metadata-only changes when they need to. Header
exposure via `X-FS-*` mirrors the S3 `x-amz-meta-*` convention and lets HEAD requests carry the full system+custom
metadata picture without a separate JSON endpoint. RFC 8187 is the standard mechanism for non-ASCII header values and
is supported by all modern HTTP clients.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Update Custom Metadata

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-update-metadata`

The file owner **MUST** be able to update the following file properties via JSON Merge Patch (RFC 7396) semantics on
the metadata-update operation:

- `custom_metadata` (user-defined key-value pairs)
- `public_access` (`cpt-cf-file-storage-fr-public-access`)
- `download_availability`

All other system-managed metadata (`file_id`, `tenant_id`, `owner_kind`, `owner_id`, `name`, `size`, `mime_type`,
`gts_file_type`, `created_at`, `content_revision`, `hash_*`, `etag`) is **NOT** user-updatable — it is maintained by
the system. A successful metadata update **MUST** bump `metadata_revision` and `last_modified_at`, but **MUST NOT**
change `content_revision`, `etag`, or `hash_value` (which all reflect the content). Content replacement is a separate
operation that does change those (`cpt-cf-file-storage-fr-upload-file`).

**`download_availability` semantics.** The flag is a kill-switch for **non-owner** download access. When
`download_availability = false`:

- The owner **MUST** still be able to download the file through the auth-required namespace.
- The public namespace (`/api/file-storage-public/v1`) **MUST** respond with `404` regardless of `public_access`.
- FileShare-mediated grantee access (P3) **MUST** respond with `403` (mediated by the FileShare module, not by
  FileStorage directly).

When `download_availability = true`, all access paths apply their normal authorization rules.

**Rationale**: Custom metadata evolves as files are processed, categorized, or annotated by consuming modules. System
metadata reflects the immutable physical properties of the file and must remain authoritative. Separating
`download_availability` as an owner-controlled kill-switch lets owners temporarily revoke external visibility (e.g.,
during incident response) without losing the file or its share configuration. Owner exemption ensures the owner can
always recover the file's content even after disabling external access.
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
automatically delete or expire file content based on age or inactivity. Public access (`public_access`) does not
expire — it is revoked only by an explicit owner action.

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
- Transfer ownership to another user or tenant
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
versioning capability is available **and enabled** for a backend, the system **MUST**:

- Create a new version with an opaque version identifier **on every content-replacement operation** (single-shot
  `PATCH /files/{id}` with a `content` part, or completion of a multipart upload per
  `cpt-cf-file-storage-fr-multipart-upload`). Metadata-only updates **MUST NOT** create a new backend version; metadata
  history (if needed) is maintained separately in FileStorage's own metadata store
- Retrieve a specific file version (content + version-pinned headers) by its version identifier
- Retrieve metadata headers of a specific file version by its version identifier (`HEAD` semantics)
- List all versions (current and non-current) of a file, including each version's identifier, size, last modified
  timestamp, content hash, and whether it is the current version
- Soft-delete a file (without specifying a version) by placing a logical delete marker on the current version. The
  delete marker makes the current version inaccessible through normal file access (download, metadata queries) while
  all non-current versions remain retrievable by their version identifiers. Soft-deleted content is **not** physically
  removed from the storage backend — it continues to exist and **MUST** count against the owner's storage quota
  (`cpt-cf-file-storage-fr-storage-quota`)
- Restore a soft-deleted file by removing the delete marker, making the most recent non-current version the current
  version again. Restore **MUST** require the same authorization as a content write
- Permanently delete a specific file version by its version identifier
- Treat version identifiers as opaque strings — the system **MUST NOT** assume any specific format, ordering, or
  parseable structure of version identifiers across storage backends

When versioning is unavailable or disabled, content-replacement operations permanently overwrite the prior content and
no version history is retained. Soft-deleted versions are **not** subject to automatic garbage collection — soft-delete
is an intentional owner action, not an orphaned state. Cleanup of soft-deleted versions is governed by retention
policies (`cpt-cf-file-storage-fr-retention-policies`).

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

The system **MUST** produce an audit record for every write operation (upload, delete, metadata update, link creation,
link revocation). Audit records **MUST** include the operation type, actor identity, file identifier, timestamp, and
outcome (success or failure).

**Rationale**: Audit trails are required for security forensics, compliance reporting, and operational troubleshooting.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Read Audit Logging

- [ ] `p3` - **ID**: `cpt-cf-file-storage-fr-read-audit`

The system **MUST** support optional audit logging for read operations (downloads and metadata queries), configurable
per policy. When enabled by policy, the system **MUST** produce an audit record for every read operation. Because all
content traffic flows through FileStorage's proxy (see DESIGN.md), read audit applies uniformly to every download —
auth-required and public-namespace alike — there are no per-flow carve-outs.

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

Backends **MAY** additionally support internal-only capabilities (e.g., presigned URL generation for backend-to-backend
replication, migration, or backup tooling). Internal-only capabilities are used by FileStorage itself and are **NOT**
exposed on the public capability discovery surface; no backend-addressable URL is ever returned to a client (see
DESIGN.md for the proxy model).

Each declared client-facing capability **MUST** be independently configurable as enabled or disabled per backend. A
capability that is supported by the backend but disabled by configuration **MUST** behave identically to an unsupported
capability — the system **MUST NOT** expose or use it. Only capabilities that are both declared by the backend and
enabled in configuration are considered available.

The system **MUST** expose the set of available (declared and enabled) client-facing capabilities per backend so that
consumers can discover them at runtime. When a consumer requests an operation that depends on an unavailable
capability, the system **MUST** return a clear error indicating the capability is unavailable. Capability declarations
**MUST** be part of the backend configuration — not inferred at runtime from probing.

**Rationale**: Storage backends vary widely in feature support. A formal capability model enables FileStorage to adapt
behavior per backend, allows consumers to discover and handle feature availability, and replaces ad-hoc fallback logic
with a consistent, extensible pattern. Separating client-facing capabilities from internal-only ones preserves backend
opacity while keeping internal optimizations (e.g., backend-to-backend signed URLs for replication) available to
FileStorage itself.
**Actors**: `cpt-cf-file-storage-actor-cf-modules`

#### Backend Configuration Source

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-backend-config-source`

In P1, storage backend configurations (`type`, `endpoint`, `credentials`, `capabilities`, `hash_policy`) **MUST** be
loaded from a static TOML configuration file at module startup. The file path follows the platform configuration
convention (`config/file-storage/backends.toml` or equivalent). Adding, removing, or re-configuring a backend requires
a module restart. The configured set is exposed for runtime introspection via `GET /storages` and
`GET /storages/{storage_id}`.

**Rationale**: A static configuration file is the simplest viable mechanism for P1, has no dependency on the database
or admin UI, and matches how platform infrastructure modules are routinely configured. Read-only HTTP introspection is
sufficient for clients to discover available backends and their capabilities without granting any runtime mutation
surface.
**Actors**: `cpt-cf-file-storage-actor-cf-modules`

#### Runtime Backend Configuration

- [ ] `p3` - **ID**: `cpt-cf-file-storage-fr-runtime-backends`

The system **MUST** allow tenant administrators to connect, configure, and remove storage backends at runtime through
an authenticated admin API without requiring service rebuild or redeployment. Backend configurations **MUST** be
persisted in the metadata database (replacing the P1 TOML source) and propagated to running module instances. Live
backends **MUST** remain serviceable through configuration updates that do not change credentials or endpoints.

**Rationale**: Enterprise tenants need to bring their own storage (BYOS) and switch backends based on cost, compliance,
or geographic requirements without a maintenance window. Moving the configuration source from TOML to DB is a P3
concern because it requires admin tooling, credential rotation flows, and per-tenant overrides that are not justified
for the initial release.
**Actors**: `cpt-cf-file-storage-actor-platform-user`

### 5.9 Access Interfaces

#### REST API

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-rest-api`

The system **MUST** expose a REST API for all file operations (upload, download, delete, metadata, link management).

**Rationale**: REST is the standard access interface for Cyber Ware modules and platform UI.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Random Read Access

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-range-requests`

Download endpoints **MUST** support random (non-sequential) read access to arbitrary byte ranges of stored content so
that consumers can seek through large files efficiently — most importantly, so that media players can scrub through
videos and audio without re-downloading the file. This applies uniformly to the auth-required download path, the
public namespace (`cpt-cf-file-storage-fr-public-access`), and any future facade or proxy in front of FileStorage; no
download channel may opt out.

**Rationale**: Without random read access, every seek in a video forces a full re-download from byte 0, which is
unusable for any clip longer than a few seconds. P1 already targets multimedia-producing consumers (LLM Gateway
outputs, generated video/audio), so shipping P1 without random read access would leave those consumers unable to
play back what they store.

The protocol-level mechanics (HTTP `Range`/`Content-Range` semantics, response codes, `Accept-Ranges` advertisement,
backend-level range translation, fallback when the backend does not support range reads natively) are documented in
DESIGN.md (`cpt-cf-file-storage-design-random-read-access`).
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

### 5.10 Cache & Idempotency

#### Conditional Requests

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-conditional-requests`

The system **MUST** support conditional HTTP requests (RFC 7232) for all operations served by FileStorage (proxied
downloads, metadata requests, content-replacement and metadata-update operations, deletes, and — in P2 — multipart
control endpoints). The system **MUST**:

- Return an `ETag` header with every download and `HEAD` response. The `ETag` is opaque, deterministic per
  `(file_id, content_revision)`, and **MUST NOT** equal the content hash (`X-FS-Hash-Value` is the separate channel for
  hash exposure).
- Support `If-None-Match` on `GET` and `HEAD` (on both the auth-required and public namespaces): when the supplied
  ETag matches the file's current ETag, respond with `304 Not Modified` and an empty body.
- Support `If-Match` on `GET` and `HEAD`: when the supplied ETag does not match the file's current ETag, respond with
  `412 Precondition Failed`. Useful for "give me this content only if it is still the version I expect".
- **Require** `If-Match` on every write (`PATCH`, `DELETE`, and P2 multipart-control endpoints under `/files/{id}/multipart/...`):
  when the supplied ETag does not match, respond with `412 Precondition Failed`.

**ETag is content-only.** Metadata-only updates (a `PATCH` request with no `content` part) bump `metadata_revision` and
`last_modified_at` but **MUST NOT** change `ETag` or `hash_value` — both remain tied to the content
(`cpt-cf-file-storage-fr-metadata-storage`). The practical consequence is that `If-Match` on a metadata-only `PATCH`
protects against concurrent **content** writes (which would invalidate the client's read-modify-write loop's
assumptions about the file's content state) but **does not detect concurrent metadata-only writes** — metadata updates
are last-write-wins on overlapping keys. This is the S3 model: ETag reflects the bytes, not the surrounding metadata.

Because all downloads pass through FileStorage's proxy (see DESIGN.md), conditional requests apply uniformly to every
download (including public-namespace and FileShare-mediated paths) — there are no per-flow carve-outs.

**Rationale**: Conditional downloads eliminate redundant bandwidth for unchanged files and enable downstream caching
by browsers and reverse proxies. Required `If-Match` on writes is a deliberate cost: every client must read before
write, which catches lost updates against content races. Accepting last-write-wins for metadata-only updates is an
explicit S3-style trade-off: keeping ETag tied to content makes it a stable cache-validation key for downloads (the
high-volume operation), at the cost of losing optimistic-concurrency protection for metadata edits (the low-volume
operation, where the conflict surface is also narrower because JSON Merge Patch generally touches disjoint keys).
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

Stored file URLs (both the auth-required namespace and the public namespace per
`cpt-cf-file-storage-fr-public-access`) **MUST** remain accessible for the duration of the file's retention with
availability matching the platform SLA.

**Threshold**: URL availability matches platform SLA for the duration of the file's retention period
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
backend-capability discovery. Consumed in-process by Cyber Ware modules through ClientHub.
**Breaking Change Policy**: Major version bump required for trait signature changes.

#### Auth-Required REST API

- [ ] `p1` - **ID**: `cpt-cf-file-storage-interface-rest-api`

**Type**: REST API (OpenAPI 3.0)
**URL Prefix**: `/api/file-storage/v1`
**Stability**: unstable
**Description**: HTTP REST API for all owner-authenticated file operations and metadata management.
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

1. User uploads file content with metadata (name, mime_type, GTS file type) via `POST /files` (multipart/form-data:
   `metadata` + `content`)
2. FileStorage validates the GTS file type format
3. FileStorage checks authorization for write on `gts.cf.fstorage.file.type.v1~` with the file type in resource context
4. FileStorage validates the declared mime_type against magic bytes during streaming
   (`cpt-cf-file-storage-fr-content-type-validation`)
5. *(Phase 2)* FileStorage validates the upload against tenant and user policies (type, size); in P1 all uploads pass
6. FileStorage persists content via the configured storage backend, assigns ownership
   (`owner_kind`, `owner_id`, `tenant_id`), records `hash_value` (SHA-256), and stores metadata
7. *(Phase 2)* FileStorage emits an audit record for the creation
8. FileStorage returns `201 Created` with full file metadata including `file_id`, `etag`,
   `hash_value`, `content_revision`, and `metadata_revision`
9. User toggles `public_access = true` via `PATCH /files/{id}` (metadata-only, JSON Merge Patch) with `If-Match` equal
   to the current ETag
10. The file is now readable via `GET /api/file-storage-public/v1/files/{id}` without authentication

**Postconditions**:

- File stored with metadata and ownership
- `public_access = true` — file is anonymously readable via the public namespace
- *(Phase 2)* Audit records emitted for both creation and the public-access toggle

**Alternative Flows**:

- **Missing or invalid GTS file type**: FileStorage rejects with `422 Unprocessable Entity`
- **Authorization denied**: FileStorage returns `403`
- **Mime mismatch**: FileStorage rejects with `415 Unsupported Media Type`
- *(Phase 2)* **Tenant disables public access via policy**
  (`cpt-cf-file-storage-fr-public-access-policy`): the `PATCH` to set `public_access = true` is rejected with `422`

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

1. Owner sends `DELETE /files/{id}` with `If-Match` equal to the current ETag
2. FileStorage checks authorization for delete on `gts.cf.fstorage.file.type.v1~`
3. FileStorage deletes the file content from the storage backend
4. FileStorage removes file metadata and ownership records (cascading to custom metadata)
5. *(Phase 2)* FileStorage emits audit record for the deletion
6. Public-namespace and any FileShare-mediated requests for this file thereafter return `404`

**Postconditions**:

- File content removed from storage backend
- Metadata and ownership records removed
- *(Phase 2)* Audit record emitted

**Alternative Flow — versioned file, no version identifier** (`cpt-cf-file-storage-fr-file-versioning`):

1. Owner sends `DELETE /files/{id}` with `If-Match` (no version identifier supplied)
2. FileStorage checks authorization for delete on `gts.cf.fstorage.file.type.v1~`
3. FileStorage places a soft-delete marker on the current version
4. *(Phase 2)* FileStorage emits audit record for the soft-delete

**Postconditions**:

- Current version inaccessible through normal file access; non-current versions remain retrievable by version ID
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

1. Owner reads current file state via `HEAD /files/{id}` and records `ETag` and `X-FS-Public-Access`
2. Owner sends `PATCH /files/{id}` with `metadata` = `{"public_access": false}` and `If-Match: <etag>`
3. FileStorage checks authorization, applies JSON Merge Patch, bumps `metadata_revision` and `last_modified_at`
4. Subsequent `GET /api/file-storage-public/v1/files/{id}` requests return `404`
5. *(Phase 2)* FileStorage emits an audit record for the `public_access` change

**Postconditions**:

- `public_access = false` on the file row
- Public namespace returns `404`; auth-required access for the owner is unaffected

**Alternative Flows**:

- **Enable public access**: same flow with `"public_access": true`. Subsequent public-namespace requests succeed
- **Toggle download_availability**: same flow with `"download_availability": false`. Owner can still download; public
  namespace and FileShare grantees are blocked
- **Authorization denied**: `403`
- **ETag mismatch**: `412 Precondition Failed`
- *(Phase 2)* **Tenant disables public access via policy** (`cpt-cf-file-storage-fr-public-access-policy`): setting
  `public_access = true` is rejected with `422`; previously-public files behave as if `public_access = false`

### Inter-User Sharing (delegated to FileShare)

- [ ] `p3` - **ID**: `cpt-cf-file-storage-usecase-fileshare-grant`

**Actor**: `cpt-cf-file-storage-actor-platform-user`

**Preconditions**:

- FileShare module (P3) is deployed
- User owns the file (or has delegated share-on rights, per FileShare's own model)

**Main Flow**:

1. Owner creates a grant in the **FileShare** module specifying file_id, grantee principals, and permissions; FileShare
   stores the grant in its own data store
2. Grantee requests the file from FileShare (e.g., `GET /api/file-share/v1/files/{file_id}`)
3. FileShare checks its grant model, enforces permissions, and on success calls FileStorage SDK with FileShare's
   service identity to fetch metadata and content
4. FileStorage applies its normal authorization (FileShare's service identity has read permissions on the relevant GTS
   types) and streams content back to FileShare
5. FileShare streams the content to the grantee with a response shape identical to FileStorage's auth-required GET

**Postconditions**:

- Grantee receives the file content; FileStorage logs the read as performed by FileShare's service identity
- *(Phase 3)* FileShare emits its own audit record describing the grantee-mediated access

**Alternative Flows**:

- **`download_availability = false` on the file**: FileShare returns `403`
- **Grant revoked / expired / out of scope**: FileShare returns `403` or `404` per its own model

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
5. Upload, download, delete, metadata, and public-namespace operations behave identically regardless of backend

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

### Core (P1)

- [ ] File creation (`POST /files` with `multipart/form-data`: `metadata` + `content`) persists the file with full
  system metadata (`file_id`, `tenant_id`, `owner_kind`, `owner_id`, `name`, `size`, `mime_type`, `gts_file_type`,
  `created_at`, `last_modified_at`, `content_revision`, `metadata_revision`, `hash_algorithm`, `hash_value`, `etag`,
  `public_access=false`, `download_availability=true`) and returns `201` with that metadata
- [ ] File download (`GET /files/{id}`) returns content with all `X-FS-*` headers populated
- [ ] `HEAD /files/{id}` returns the same headers as `GET` without a body and ignores `Range`
- [ ] `PATCH /files/{id}` with a `content` part replaces content, bumps `content_revision`, `metadata_revision`,
  `last_modified_at`, `hash_value`, and `ETag`; without a `content` part, only `metadata_revision` and
  `last_modified_at` are bumped, and `ETag`/`hash_value` are unchanged
- [ ] `DELETE /files/{id}` removes content and metadata for non-versioned files; for versioned files without
  `version_id`, places a soft-delete marker
- [ ] Authorization checked for every file operation via Authorization Service, with the file's GTS type in resource
  context
- [ ] Tenant boundary enforced — cross-tenant access through the auth-required namespace returns access-denied
- [ ] All file content traffic flows through FileStorage; no backend-addressable URL is returned to any client
  (proxy model — see DESIGN.md)
- [ ] `404` returned for non-existent files; `403` for unauthorized operations on existing files in the auth-required
  namespace
- [ ] Metadata-only queries (`HEAD /files/{id}`) complete without transferring content
- [ ] Custom metadata is updatable by the owner via JSON Merge Patch on `PATCH /files/{id}`; system-managed metadata is
  not user-updatable
- [ ] `(owner_kind, owner_id)` is immutable after creation except via ownership transfer or owner deletion workflows;
  `tenant_id` is **never** mutable
- [ ] Every file has a mandatory GTS file type assigned at creation; missing or malformed `gts_file_type` is rejected
  at `POST /files` with `422`
- [ ] GTS file type is immutable after creation
- [ ] Authorization requests include the file's GTS type, enabling per-type access decisions
- [ ] A module authorized only for type A cannot access files of type B
- [ ] FileStorage SDK and REST API behave identically regardless of configured storage backend
- [ ] File listing (`GET /files`) returns metadata only, is paginated, and requires a mandatory `owner_kind` filter
  (`user` or `app`)
- [ ] Upload rejected with `415` when declared `mime_type` does not match the magic-bytes detection on the streaming
  upload path
- [ ] File owner can toggle `public_access` via `PATCH /files/{id}` metadata-only update
- [ ] File owner can toggle `download_availability` via `PATCH /files/{id}` metadata-only update; owner can always
  download regardless of `download_availability`; public namespace returns `404` when `download_availability=false`
- [ ] Each backend declares its supported client-facing capabilities (`versioning_native`, `multipart_native`,
  `encryption_native`); internal-only capabilities are not surfaced on public discovery
- [ ] `GET /storages` and `GET /storages/{storage_id}` expose configured backends with their resolved capabilities
- [ ] Declared capabilities are independently configurable (enable/disable) per backend; a capability disabled by
  configuration behaves identically to an unsupported capability
- [ ] Operations requiring an unavailable capability return a clear error
- [ ] Storage backend configuration in P1 is read from a static TOML file at module startup
- [ ] Download and metadata responses include an opaque `ETag` header derived from `(file_id, content_revision)` that
  is **not** equal to `hash_value`
- [ ] `If-None-Match` on `GET`/`HEAD` returns `304 Not Modified` when ETag matches
- [ ] `If-Match` on `GET`/`HEAD` returns `412 Precondition Failed` when ETag does not match
- [ ] `If-Match` is required on `PATCH`/`DELETE` and on multipart-control endpoints; missing or mismatching `If-Match`
  returns `412`
- [ ] Metadata-only `PATCH` with `If-Match` against the current content-derived ETag succeeds even when concurrent
  metadata writes have happened (S3-style last-write-wins on metadata is the documented behavior)
- [ ] Random read access via HTTP `Range` requests works on every download (auth-required and public namespaces);
  media seeking, resumable downloads, and parallel-segment downloads are supported; `Accept-Ranges: bytes` is set on
  every download response (including `HEAD`); unsatisfiable ranges return `416` with `Content-Range: bytes */<size>`;
  backends without native range support still serve ranges through FileStorage without full-file memory buffering

### Public access (P1)

- [ ] When `public_access=true`, `GET /api/file-storage-public/v1/files/{id}` returns content; `HEAD` returns metadata
  headers; the same Range/`If-None-Match` semantics as the auth-required path apply
- [ ] When `public_access=false`, the public namespace returns `404` regardless of whether the file exists, never
  `401`/`403`
- [ ] All non-`GET`/`HEAD` methods on the public namespace return `405 Method Not Allowed`
- [ ] Public namespace responses **omit** `X-FS-GTS-File-Type`, `X-FS-Public-Access`, and `X-FS-Meta-*` headers
- [ ] When `download_availability=false`, the public namespace returns `404` regardless of `public_access`

### Phase 2

- [ ] Multipart upload (initiate / upload parts / complete / abort) assembles parts into a complete file with correct
  metadata; abandoned multipart uploads are reclaimable per a documented TTL
- [ ] Upload idempotency: retried `POST /files` with the same `Idempotency-Key` returns the original result without
  creating a duplicate file; the same key from a different owner is treated as distinct
- [ ] Custom metadata operations rejected when exceeding configurable limits (max pairs, key length, value length,
  total size)
- [ ] Policies enforce file-type and size restrictions on upload (most restrictive wins across tenant and user levels)
- [ ] Tenant-level public-access restriction policy disables `public_access=true` on new and existing files
- [ ] File events emitted to EventBroker on write operations (upload, update, delete) when enabled by owner policy
- [ ] Storage quota enforcement: upload rejected when quota would be exceeded
- [ ] Usage report emitted asynchronously on every storage-consuming write; file operations not blocked when Usage
  Collector is unavailable; ownership transfer emits usage reports for both prior and new owner
- [ ] Audit record emitted for every write operation
- [ ] File ownership transferable by the current owner to another user or app within the same tenant; transfer requires
  authorization of both parties and emits an audit record
- [ ] Owner deletion event from EventBroker triggers a configurable Serverless Runtime workflow for file disposition;
  files of a deleted owner are retained as orphaned when no workflow is configured
- [ ] Retention policies automatically expire and delete files based on configured age, inactivity, or custom metadata
  criteria; per-file retention overrides are honored
- [ ] File versioning: when `versioning_native=true`, each content-replacement `PATCH` (and each multipart `complete`)
  creates a new version; previous versions remain accessible by opaque version identifier; metadata-only updates do
  **not** create a new backend version. Soft-delete places a marker, counts against quota, and is reversible via
  restore; permanent version delete removes only the targeted version

### Phase 3

- [ ] Server-side encryption is applied when the encryption capability is available and enabled for the backend
- [ ] Storage backends can be added, removed, and reconfigured at runtime through the admin API without service
  redeployment; runtime configuration replaces the P1 TOML source
- [ ] Read audit records emitted for every download (including public-namespace requests) when enabled by policy

## 10. Dependencies

| Dependency            | Description                                                                                        | Criticality |
|-----------------------|----------------------------------------------------------------------------------------------------|-------------|
| ModKit Framework      | Module lifecycle, ClientHub for service registration                                               | p1          |
| Authorization Service | Access decisions for `gts.cf.fstorage.file.type.v1~` resources                                     | p1          |
| API Gateway           | Routes `/api/file-storage-public/v1` without JWT enforcement (`cpt-cf-file-storage-fr-public-access`) | p1       |
| PostgreSQL            | File metadata, custom metadata, ownership records (P3+: backend configurations)                    | p1          |
| Audit Infrastructure  | Platform audit event sink                                                                          | p2          |
| Usage Collector       | Receives storage usage reports for metering and billing                                            | p2          |
| Quota Enforcement     | Per-owner storage quota enforcement                                                                | p2          |
| EventBroker           | Publishes and consumes file/platform events                                                        | p2          |
| Serverless Runtime    | Executes configurable workflows for lifecycle operations                                           | p2          |
| FileShare (consumer)  | Inverse dependency: FileShare (P3) consumes FileStorage SDK to deliver per-principal grant-based   | p3          |
|                       | sharing. FileStorage itself has no dependency on FileShare.                                        |             |

## 11. Assumptions

- Authorization Service is available and supports `gts.cf.fstorage.file.type.v1~` resource type
- All file access respects tenant boundaries at the platform level
- Initial storage backend is configured at deployment time; runtime backend switching is phase 2
- Auth-required file URLs are internal to Cyber Ware; external anonymous access is via the public namespace
  per `cpt-cf-file-storage-fr-public-access`; per-principal external sharing is delivered by the FileShare module (P3)
- Policy configuration is available to tenant administrators and users through the platform

## 12. Risks

| Risk                                                                       | Impact                                                         | Mitigation                                                                                                                                              |
|----------------------------------------------------------------------------|----------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------|
| Storage service unavailability blocks all file-dependent operations        | High — multimodal AI, document workflows disrupted             | Design for graceful degradation; clear error propagation to consumers                                                                                   |
| Large file sizes increase request latency for consuming modules            | Medium — slow responses for multimodal and document operations | Metadata pre-fetch enables size validation; streaming support for large files                                                                           |
| Backend credential compromise enables unauthorized backend access          | High — data exposure                                           | Backend credentials held only by FileStorage and never exposed to clients (proxy model — see DESIGN.md); standard credential rotation procedures apply at the FileStorage layer  |
| FileStorage becomes a bandwidth bottleneck under high load                 | High — degraded transfer latency and timeouts                  | Horizontal scaling per `cpt-cf-file-storage-nfr-scalability`; streaming I/O without full-file buffering; HTTP Range support (`cpt-cf-file-storage-fr-range-requests`) for partial reads and resumable downloads |
| Policy misconfiguration blocks legitimate uploads                          | Medium — user frustration                                      | Policy validation on save; clear error messages identifying which policy was violated                                                                   |
| `public_access` flag misconfigured by owner exposes private file via UUID  | High — unintended data leak                                    | Flag is owner-controlled and revocable by metadata `PATCH`; tenant-level `public_access` restriction policy (`cpt-cf-file-storage-fr-public-access-policy`) lets administrators disable public access categorically; explicit audit event on every toggle (P2)                       |
| File UUID enumeration / leakage from logs exposes `public_access=true` file| Medium — anonymous read of intended-public file                | UUIDs are 128-bit random — enumeration is computationally infeasible. UUIDs **MUST NOT** be logged on the public namespace in client-accessible logs; in logs they appear truncated or hashed                                                                                       |

## 13. Open Questions

None.

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**:
  - [ADR-0001: Proxy All File Content Traffic Through FileStorage](./ADR/0001-cpt-cf-file-storage-adr-proxy-content-traffic.md)
  - [ADR-0002: Content Integrity Hash — SHA-256 in P1, Configurable in P2](./ADR/0002-cpt-cf-file-storage-adr-content-hash-selection.md)
- **Features**: [features/](./features/)
