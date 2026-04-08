# PRD — File Storage

## 1. Overview

### 1.1 Purpose

FileStorage is a universal file storage and management service for the CyberFabric platform. It provides upload,
download, metadata management, access control, and sharing capabilities for any module or user within the platform.

The service supports pluggable storage backends, multiple access protocols (REST, S3-compatible, WebDAV), tenant-scoped
access control with an ownership model, and policy-driven governance for file types, sizes, and sharing.

### 1.2 Background / Problem Statement

CyberFabric modules and platform users require file storage for various purposes: modules handle multimodal AI content
(images, audio, video, documents), documents and artifacts, reporting outputs, and platform users need direct file
access through standard protocols.

Without a dedicated storage service, each module implements ad-hoc file handling, media gets inlined as base64 in API
payloads (bloating requests and hitting size limits), provider-generated URLs expire leaving consumers with broken
links, and there is no unified access control or policy enforcement across the platform.

FileStorage solves this by providing a centralized, tenant-aware storage service with persistent URLs, pluggable
backends, and standardized access interfaces — functioning as a superset of S3 and WebDAV capabilities within the
CyberFabric security and governance model.

### 1.3 Goals (Business Outcomes)

- Unified file storage accessible by all CyberFabric modules and platform users
- Tenant-scoped and origin-module-scoped access control with tenant, user and module ownership model
- Flexible sharing via public, tenant-scoped, and signed URLs
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
| Owner               | Entity (user or tenant) that owns a file and controls its sharing and lifecycle                                                                                                                                                                                                         |
| Shareable Link      | A unique URL served by FileStorage that grants access to a file with a specific scope and expiration; FileStorage validates the link and enforces access control on every request                                                                                                       |
| Signed URL          | A presigned backend URL (download-only) for sharing with external or unauthenticated consumers; generated by FileStorage using its own backend credentials, time-limited, non-revocable — the storage backend validates the signature and enforces expiration                           |
| Direct Transfer URL | A presigned backend URL (upload-only) for authenticated clients to upload file content directly to the backend without routing traffic through FileStorage; generated by FileStorage after authorization, time-limited                                                                  |
| Storage Backend     | An underlying storage system (S3, GCS, Azure Blob, NFS, FTP, SMB, WebDAV) used for persisting file content                                                                                                                                                                              |
| Policy              | A set of rules (allowed file types, size limits, events, sharing models) that constrain file operations; applicable at the tenant level and the user level independently — when both apply, the most restrictive value per aspect wins                                                  |
| File Version        | An immutable snapshot of file content created on each upload to the same logical path when versioning is enabled; identified by an opaque version identifier assigned by the storage backend                                                                                            |
| Version Identifier  | An opaque string assigned by the storage backend that uniquely identifies a specific version of a file; format varies by backend and must not be parsed or assumed                                                                                                                      |
| File Type (GTS)     | A GTS type identifier assigned to every file at upload time that classifies the file by domain, actor, and purpose (e.g., `gts.x.fstorage.file.type.v1~x.genai.llm.autogenerated.v1~`); used by the Authorization Service to enforce per-type access control between actors and modules |
| Backend Capability  | An optional feature that a storage backend may or may not support (e.g., presigned URLs, versioning, multipart upload); FileStorage discovers available capabilities per backend and adapts its behavior accordingly                                                                    |

## 2. Actors

### 2.1 Human Actors

#### Platform User

**ID**: `cpt-cf-file-storage-actor-platform-user`

**Role**: Authenticated user who uploads, downloads, and manages files through the platform UI or API.  
**Needs**: Direct file access, sharing capabilities, metadata management, and self-service link management.

### 2.2 System Actors

#### CyberFabric Modules

**ID**: `cpt-cf-file-storage-actor-cf-modules`

**Role**: Any CyberFabric module requiring file upload, download, metadata retrieval, or link management (e.g., LLM
Gateway for multimodal media, document management modules, reporting modules).

## 3. Operational Concept & Environment

### 3.1 Module-Specific Environment Constraints

FileStorage operates within the standard CyberFabric runtime environment. Authentication and identity management are
fully delegated to the platform — FileStorage does not implement its own authentication layer. All incoming requests are
pre-authenticated by the platform infrastructure, and FileStorage receives the caller's identity context (user, tenant,
roles) from the platform authentication middleware.

## 4. Scope

### 4.1 In Scope

- Upload, download, delete, and list files
- Rich file metadata storage, retrieval, and update
- File ownership by user or tenant
- GTS file type classification for per-actor access control
- Authorization checks via Authorization Service
- Shareable links with public, tenant, and tenant-hierarchy scopes
- Signed URLs for unauthenticated, time-limited downloads
- Link expiration and lifecycle management
- Audit trail for all write operations and optional read audit logging
- Policies (file types, size limits, events, sharing restrictions) at tenant and user levels
- Pluggable storage backend abstraction
- Multipart (chunked) upload for large files
- Content-type validation against actual file content
- Direct-to-backend upload via presigned URLs for compatible backends
- Garbage collection for unconfirmed direct uploads
- File retention and lifecycle management
- REST API access interface
- S3-compatible API
- WebDAV API
- Streaming and range requests
- Runtime tenant-configurable storage backends
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

## 5. Functional Requirements

### 5.1 Core File Operations

#### Upload File

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-upload-file`

The system **MUST** accept file content with metadata and persist it, returning a persistent, accessible URL. File
content is immutable after upload — to change content, a new file **MUST** be uploaded.

**Rationale**: All platform modules and users need to store files — modules store generated content, documents, and
artifacts, users upload files directly. Immutable content simplifies caching, integrity verification, and backend
replication.  
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
metadata, ownership records, and all associated shareable links are removed. When versioning is enabled
(`cpt-cf-file-storage-fr-file-versioning`), deletion without a version identifier places a soft-delete marker;
shareable links remain associated but resolve to the soft-deleted state. Permanent removal of a specific version
requires passing its version identifier explicitly.

**Rationale**: Owners need to remove files that are no longer needed; permanent deletion must cascade to all links to
prevent dangling references. Versioned files default to soft-delete to enable recovery from accidental deletions —
shareable links are preserved so they can be restored alongside the file. Permanent removal is an explicit,
version-targeted operation.
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

- **User-owned** — files owned by a specific user
- **Tenant-owned** — files owned by the tenant

The response **MUST** be paginated following the platform API guidelines (cursor-based or offset-based pagination with
configurable page size). The system **MUST** support optional additional filters (mime_type, date range, custom metadata
keys).

**Rationale**: Users and modules need to discover and browse files they own or have access to. Mandatory owner type
filtering prevents unbounded queries across all files and aligns with the ownership model.  
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Multipart Upload

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-multipart-upload`

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
proxied uploads (where file content passes through FileStorage). If the declared type does not match the detected type,
the system **MUST** reject the upload with an error indicating the mismatch.

For proxied multipart uploads (`cpt-cf-file-storage-fr-multipart-upload`), the system **MUST** validate the declared
mime_type against the content of the **first uploaded part**, which contains the file's magic bytes / file signature.
Validation **MUST** occur when the first part is received — before subsequent parts are accepted. If the detected type
does not match the declared mime_type, the system **MUST** abort the multipart upload (`abortMultipartUpload`) and
reject all subsequent parts.

Content-type validation does not apply to direct uploads (single-part or multipart) via presigned URLs because
FileStorage does not receive the file content in that flow.

**Rationale**: Without content inspection, a client can declare `image/png` but upload an executable, trivially
bypassing file type policies. Content-type validation ensures declared types are trustworthy for downstream consumers
and policy enforcement. First-part validation for multipart uploads provides the same level of guarantee as single-part
validation — magic bytes reside at the start of the file and are always contained in the first part because backends
that support multipart upload (`cpt-cf-file-storage-fr-backend-capabilities`) enforce a minimum part size (e.g., 5 MB
for S3) that far exceeds the longest magic-byte sequence (~12 bytes). Backends without native multipart support reject
multipart uploads entirely, so no fallback is needed. Post-assembly re-validation would require downloading the
assembled file from the backend, negating the efficiency benefits of multipart upload. Direct uploads trade server-side
content validation for transfer efficiency — consumers relying on strict type guarantees should use proxied uploads.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

### 5.2 Ownership & Access Control

#### File Ownership

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-file-ownership`

The system **MUST** associate every file with an owner. Ownership **MUST** be assignable to either a specific user or a
tenant at upload time. Ownership is immutable after creation except through explicit ownership transfer
(`cpt-cf-file-storage-fr-ownership-transfer`) or owner deletion workflows (`cpt-cf-file-storage-fr-owner-deletion`).

**Rationale**: Ownership determines who can manage (delete, share, update metadata) a file and establishes the basis for
access control decisions. Restricting ownership changes to explicit transfer operations simplifies the authorization
model and prevents accidental privilege escalation.  
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Authorization Checks

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-authorization`

The system **MUST** verify authorization for every file operation by requesting an access decision from the
Authorization Service. Read, write, and delete operations **MUST** be checked against `gts.x.fstorage.file.type.v1~` resources in
the context of the requesting user. Authorization requests **MUST** include the file's GTS type
(`cpt-cf-file-storage-fr-file-type-classification`) in the resource context to enable per-type access decisions.

**Rationale**: All file access must be governed by the platform's centralized authorization model to enforce role-based,
tenant-scoped, and type-scoped permissions.  
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Tenant Boundary Enforcement

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-tenant-boundary`

The system **MUST** enforce tenant isolation: file deletion and metadata update operations **MUST NOT** cross tenant
boundaries. A user in one tenant **MUST NOT** delete or update metadata of files owned by another tenant. Cross-tenant
read access is intentionally permitted via shareable links with tenant-hierarchy scope (see
`cpt-cf-file-storage-fr-shareable-links`).

**Rationale**: Multi-tenant platforms require strict data isolation for write operations to prevent unauthorized
cross-tenant modification, while supporting controlled read sharing across tenant hierarchies.  
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
by domain and purpose following the GTS type format (e.g. `gts.x.fstorage.file.type.v1~x.genai.llm.autogenerated.v1~`
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

The system **MUST** allow the current file owner to transfer ownership of a file to another user or to the tenant.
Ownership transfer **MUST** be an audited operation and **MUST** require authorization of both the current owner and the
receiving entity.

**Rationale**: As teams evolve, files may need to change hands — e.g., when a user leaves the organization or when
personal files should become shared tenant resources.  
**Actors**: `cpt-cf-file-storage-actor-platform-user`

### 5.3 Link Management & Sharing

#### Create Shareable Links

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-shareable-links`

The system **MUST** support creating unique shareable links for files with the following access scopes:

- **Public** — accessible to anyone, including unauthenticated users
- **Tenant** — accessible to any authenticated user within the file's tenant
- **Tenant hierarchy** — accessible to any authenticated user within the file's tenant and its child tenants

Shareable links are served by FileStorage — all requests pass through FileStorage, which validates the link, enforces
scope-based access control, and serves the file content from the storage backend. The desired sharing scope(s) **MUST**
be specifiable at file creation time and when creating additional links for existing files.

**Rationale**: Different use cases require different visibility: public links for external sharing, tenant links for
internal collaboration, hierarchy links for parent-child tenant structures. Routing through FileStorage enables
scope-based access control, revocation, and audit logging on every access.  
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Signed Download URLs

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-signed-urls`

The system **MUST** support generating presigned download URLs that point directly to the storage backend, granting
time-limited download access without requiring authentication. FileStorage generates these URLs using its own backend
credentials. A signed URL **MUST**:

- Be generated by FileStorage using its own credentials with the storage backend
- Point directly to the storage backend (bypassing FileStorage for content delivery)
- Contain a cryptographic signature that the storage backend validates against its own key material
- Include an expiration timestamp after which the URL becomes invalid
- Be scoped to a single file (download only)
- Not require the consumer to present authentication credentials

Signed URLs are **not revocable** — once issued, they remain valid until expiration because the storage backend
validates the signature independently of FileStorage. If revocable access is needed, use shareable links instead
(served through FileStorage with revocation support).

Signed URL expiration **MUST** be constrained from two sources:

- **Backend limit** — each storage backend declares its maximum supported signed URL expiration in configuration. This
  is a hard ceiling that no policy can override.
- **Policy limits** — tenants and users define maximum and default signed URL expiration. When both tenant and user
  policies apply, the most restrictive value wins. Requested expiration exceeding the effective limit **MUST** be
  rejected. When no expiration is specified, the policy default applies.

Signed URL generation requires the presigned URLs backend capability (`cpt-cf-file-storage-fr-backend-capabilities`).
For backends that do not declare this capability or have it disabled, FileStorage **MUST** reject the signed URL request
with a clear error indicating the capability is unavailable.

**Rationale**: Signed URLs enable secure file sharing with external systems and unauthenticated consumers (e.g.,
embedding in emails, third-party integrations) while maintaining time-bounded access control. Routing downloads through
the storage backend directly eliminates FileStorage as a bottleneck for shared content — following the pattern
established by S3 presigned URLs, GCS signed URLs, and Azure SAS tokens. The non-revocable nature follows the same
constraint inherent in S3 presigned URLs, GCS signed URLs, and Azure SAS tokens.  
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Link Expiration

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-link-expiration`

The system **MUST** support configurable expiration for any shareable link or signed URL. Expiration **MUST** be
specifiable at link creation time. For shareable links, FileStorage enforces expiration and **MUST** return an
access-denied response after the link expires. For signed URLs, the storage backend enforces expiration — the
backend validates the signature and rejects expired URLs independently of FileStorage.

**Rationale**: Time-limited access prevents stale links from remaining accessible indefinitely, reducing the attack
surface for shared files. Expiration enforcement follows the traffic path: FileStorage enforces for shareable links
(which it serves), and the storage backend enforces for signed URLs (which bypass FileStorage).  
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Manage Links

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-manage-links`

The file owner **MUST** be able to list all active shareable links and issued signed URLs for a file. The owner **MUST**
be able to revoke (delete) individual shareable links. Signed URLs cannot be revoked (they remain valid until
expiration) but the owner **MUST** be able to view their expiration status.

**Rationale**: Owners need visibility into how their files are shared and the ability to revoke shareable link access
when no longer needed. Signed URLs are non-revocable by design (backend validates independently), so short expiration
is the primary access control mechanism.  
**Actors**: `cpt-cf-file-storage-actor-platform-user`

### 5.4 Direct-to-Backend Transfer

#### Direct Transfer URLs

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-direct-transfer`

The system **MUST** support generating presigned direct transfer URLs that point to the storage backend, allowing
clients to upload file content directly to the backend without routing traffic through FileStorage. FileStorage
generates these URLs using its own backend credentials (e.g., AWS access keys, GCS service account). A direct transfer
URL **MUST**:

- Be generated by FileStorage using its own credentials with the storage backend
- Point directly to the storage backend (e.g., S3 bucket endpoint)
- Contain a cryptographic signature that the backend validates against its own key material
- Support upload (PUT) operations only — direct downloads are covered by signed URLs (
  `cpt-cf-file-storage-fr-signed-urls`)
- Be time-limited with a configurable expiration
- Be scoped to a single file

The client authenticates with FileStorage (using its API token), FileStorage verifies authorization, registers the file
metadata (including the target backend path), and then issues the presigned URL. The client uses the presigned URL
directly with the backend — no further authentication or callback is required because the backend trusts the signature
generated with FileStorage's credentials.

Direct transfer URL generation requires the presigned URLs backend capability
(`cpt-cf-file-storage-fr-backend-capabilities`). For backends that do not declare this capability or have it disabled,
FileStorage **MUST** reject the direct transfer request with a clear error indicating the capability is unavailable.
Clients must use standard (proxied) upload for backends without presigned URL support.

**Rationale**: For large files (video, datasets, backups), proxying upload traffic through FileStorage creates a
bottleneck and doubles bandwidth consumption. Direct-to-backend upload via presigned URLs eliminates this overhead for
backends that declare the presigned URLs capability, following the pattern established by S3 presigned URLs, GCS signed
URLs, and Azure SAS tokens — where the service with backend credentials signs on behalf of the client.  
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### Garbage Collection for Unconfirmed Uploads

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-gc-direct-uploads`

The system **MUST** automatically detect and remove orphaned records from direct uploads that were never completed.
An unconfirmed or incomplete upload **MUST** become eligible for garbage collection after the expiration of the
pre-signed URL plus a configurable grace period. After the eligibility window has passed, the system **MUST** reconcile
file metadata records against actual backend object existence — remove records with no corresponding backend object,
and confirm records whose corresponding backend object exists but was never acknowledged.

**Rationale**: Since metadata is registered before the presigned URL is issued, failed or abandoned uploads leave
metadata records pointing to non-existent backend objects. The presigned URL expiration bounds the upload window but
does not guarantee upload outcome, so garbage collection prevents stale metadata accumulation and
ensures consistency between FileStorage records and backend state.  
**Actors**: `cpt-cf-file-storage-actor-cf-modules`

### 5.5 Policies (Phase 2)

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

#### Sharing Model Restrictions

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-sharing-restrictions`

The system **MUST** allow owners to restrict which sharing models (public, tenant, tenant hierarchy, signed URLs) are
available within their tenant. Attempts to create links with restricted sharing models **MUST** be rejected.

**Rationale**: Tenants in regulated environments may need to prohibit public sharing or signed URLs to enforce data
governance policies.  
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

### 5.6 Metadata

#### Rich Metadata Storage

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-metadata-storage`

The system **MUST** store and return the following system-managed metadata for every file:

- File name (original upload name)
- File size (bytes)
- File type (mime_type)
- GTS file type (`cpt-cf-file-storage-fr-file-type-classification`)
- Creation date
- Last modified date
- Owner (user or tenant reference)
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

### 5.7 File Retention & Lifecycle

#### Indefinite Retention

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-retention-indefinite`

In phase 1, files **MUST** be retained indefinitely until explicitly deleted by the file owner. The system **MUST NOT**
automatically delete or expire file content based on age or inactivity. Shareable links and signed URLs expire per their
configured expiration, but the underlying file content remains available.

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

Garbage collection (`cpt-cf-file-storage-fr-gc-direct-uploads`) does **NOT** apply to soft-deleted versions —
soft-delete is an intentional owner action, not an orphaned state. Cleanup of soft-deleted versions is governed by
retention policies (`cpt-cf-file-storage-fr-retention-policies`).

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

### 5.8 Audit

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
per policy. When enabled by policy, the system **MUST** produce an audit record for every read operation
that passes through FileStorage — proxied downloads and shareable link access. Read audit logging does not apply to
presigned URL downloads, which bypass FileStorage and are served directly by the storage backend.

**Rationale**: Regulated environments and security-sensitive owners require visibility into who accessed their files and
when. Making read audit optional per policy avoids the performance and storage overhead of logging every read
across the platform, while enabling it where compliance demands it.  
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

### 5.9 Pluggable Storage Backends

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
capabilities it supports. The system **MUST** support at least the following capabilities:

- **Presigned URLs** — the backend can generate cryptographically signed, time-limited URLs for direct client-to-backend
  upload and download without proxying through FileStorage
- **Versioning** — the backend can maintain multiple versions of a file, identified by opaque version identifiers
- **Multipart Upload** — the backend natively supports chunked upload with independent part transfers and server-side
  assembly
- **Server-Side Encryption** — the backend can encrypt file content at rest using backend-managed or customer-provided
  keys

Each declared capability **MUST** be independently configurable as enabled or disabled per backend. A capability that is
supported by the backend but disabled by configuration **MUST** behave identically to an unsupported capability — the
system **MUST NOT** expose or use it. Only capabilities that are both declared by the backend and enabled in
configuration are considered available.

The system **MUST** expose the set of available (declared and enabled) capabilities per backend so that consumers can
discover them at runtime. When a consumer requests an operation that depends on an unavailable capability, the system
**MUST** return a clear error indicating the capability is unavailable. Capability declarations **MUST** be part of the
backend configuration — not inferred at runtime from probing.

**Rationale**: Storage backends vary widely in feature support. A formal capability model enables FileStorage to adapt
behavior per backend, allows consumers to discover and handle feature availability, and replaces ad-hoc fallback logic
with a consistent, extensible pattern. Per-backend capability toggling allows administrators to disable features for
security or operational reasons — e.g., disabling presigned URLs to force all traffic through FileStorage for audit
visibility, even when the backend supports them.  
**Actors**: `cpt-cf-file-storage-actor-cf-modules`

#### Runtime Backend Configuration

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-runtime-backends`

The system **MUST** allow tenants to connect and configure storage backends at runtime without requiring service rebuild
or redeployment.

**Rationale**: Enterprise tenants need to bring their own storage (BYOS) and switch backends based on cost, compliance,
or geographic requirements.  
**Actors**: `cpt-cf-file-storage-actor-platform-user`

### 5.10 Access Interfaces

#### REST API

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-rest-api`

The system **MUST** expose a REST API for all file operations (upload, download, delete, metadata, link management).

**Rationale**: REST is the standard access interface for CyberFabric modules and platform UI.  
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### S3-Compatible API

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-s3-api`

The system **MUST** expose an S3-compatible API for file upload and download operations, enabling integration with
existing S3 tooling and SDKs.

**Rationale**: S3 is the de facto standard for object storage APIs; compatibility enables direct integration with tools,
libraries, and workflows that already support S3.  
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

#### WebDAV API

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-webdav-api`

The system **MUST** expose a WebDAV API for file access, enabling native filesystem-like mounting on client operating
systems.

**Rationale**: WebDAV enables direct OS-level access to stored files without custom client software, supporting use
cases like document editing and file management through native file explorers.  
**Actors**: `cpt-cf-file-storage-actor-platform-user`

#### Streaming and Range Requests

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-range-requests`

The system **MUST** support HTTP Range requests (RFC 7233) for partial content download, enabling seeking within large
files, resumable downloads, and parallel download of file segments.

**Rationale**: For large files (video, datasets), clients need partial access for seeking, preview generation, and
resuming interrupted downloads without re-transferring the entire file.  
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-modules`

### 5.11 Cache & Idempotency

#### Conditional Requests

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-conditional-requests`

The system **MUST** support conditional HTTP requests (RFC 7232) for all operations served by FileStorage (proxied
downloads, metadata requests, metadata updates). The system **MUST**:

- Return an `ETag` header with every download and metadata response
- Support `If-None-Match` on download and metadata requests — return `304 Not Modified` when the file has not changed,
  avoiding content transfer
- Support `If-Match` on metadata update requests — reject the update with `412 Precondition Failed` when the file state
  has changed since the client last read it, preventing lost updates from concurrent modifications

Conditional requests do not apply to presigned URL downloads, which bypass FileStorage and are served directly by the
storage backend.

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

Stored file URLs and shareable links **MUST** remain accessible for the duration of their configured lifetime with
availability matching the platform SLA.

**Threshold**: URL availability matches platform SLA for the duration of the retention/expiration period  
**Rationale**: Consumers depend on URL stability — broken links disrupt downstream workflows and user experience.  
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
**Description**: Async trait providing upload, download, delete, metadata, and link management operations.  
**Breaking Change Policy**: Major version bump required for trait signature changes.

#### REST API

- [ ] `p1` - **ID**: `cpt-cf-file-storage-interface-rest-api`

**Type**: REST API (OpenAPI 3.0)  
**Stability**: unstable  
**Description**: HTTP REST API for all file operations, metadata management, and link management.  
**Breaking Change Policy**: Major version bump required for endpoint removal or request/response schema incompatible
changes.

### 7.2 External Integration Contracts

#### CyberFabric Module Contract

- [ ] `p1` - **ID**: `cpt-cf-file-storage-contract-cf-modules`

**Direction**: provided by library (consumed by CyberFabric modules)  
**Protocol/Format**: In-process Rust SDK trait via ClientHub  
**Compatibility**: Trait versioned with SDK crate; breaking changes require coordinated release with consuming modules.

#### Authorization Service Contract

- [ ] `p1` - **ID**: `cpt-cf-file-storage-contract-authz`

**Direction**: required from external service (Authorization Service)  
**Protocol/Format**: Access decision requests for `gts.x.fstorage.file.type.v1~` resources  
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

### Upload and Share a File

- [ ] `p2` - **ID**: `cpt-cf-file-storage-usecase-upload-share`

**Actor**: `cpt-cf-file-storage-actor-platform-user`

**Preconditions**:

- User is authenticated
- Authorization Service grants write access

**Main Flow**:

1. User uploads file content with metadata (name, mime_type, GTS file type)
2. FileStorage validates the GTS file type format
3. FileStorage checks authorization for write on `gts.x.fstorage.file.type.v1~` with the file type in resource context
4. *(Phase 2)* FileStorage validates file against policies (type, size); in phase 1 all uploads are accepted
5. FileStorage persists content, assigns ownership to the user, and stores metadata (including GTS file type)
6. *(Phase 2)* FileStorage emits audit record for the upload
7. FileStorage returns persistent URL and file identifier
8. User creates a shareable link with desired scope and expiration
9. FileStorage returns the shareable link URL

**Postconditions**:

- File stored with metadata and ownership
- Shareable link active with configured scope and expiration
- *(Phase 2)* Audit record emitted

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
2. FileStorage checks authorization for read on `gts.x.fstorage.file.type.v1~` with the file's GTS type in resource context
3. FileStorage retrieves file content from the storage backend
4. FileStorage returns content with metadata (mime_type, size, GTS file type)

**Postconditions**:

- Content and metadata returned to the requesting module

**Alternative Flows**:

- **File not found**: FileStorage returns file_not_found error
- **Authorization denied**: FileStorage returns access-denied error

### Generate and Access Signed URL

- [ ] `p1` - **ID**: `cpt-cf-file-storage-usecase-signed-url`

**Actor**: `cpt-cf-file-storage-actor-platform-user`

**Preconditions**:

- User owns the file or has write access
- Backend supports presigned URLs capability
- Policy permits signed URL sharing

**Main Flow**:

1. Owner requests a signed URL for a file with a specified expiration
2. FileStorage checks authorization for the owner on `gts.x.fstorage.file.type.v1~`
3. *(Phase 2)* FileStorage validates sharing policy allows signed URLs; in phase 1 all sharing models are
   permitted
4. FileStorage generates a presigned download URL using its own backend credentials, scoped to the file and time-limited
5. FileStorage records the signed URL metadata (file, expiration, owner) for visibility and audit
6. *(Phase 2)* FileStorage emits audit record for signed URL creation
7. Owner shares the signed URL with an external consumer
8. External consumer downloads the file directly from the storage backend using the signed URL — no authentication
   required, backend validates signature

**Postconditions**:

- File content delivered to external consumer directly from storage backend without authentication
- Signed URL metadata recorded for visibility and audit
- *(Phase 2)* Audit record emitted for signed URL creation

**Alternative Flows**:

- **Expired URL**: Storage backend rejects the request (signature expiration enforced by backend)
- **Invalid signature**: Storage backend rejects the request
- **Backend does not support presigned URLs or capability is disabled**: FileStorage returns an error indicating the
  capability is unavailable
- *(Phase 2)* **Sharing model restricted by policy**: FileStorage returns policy-violation error

### Validate File Metadata Before Processing

- [ ] `p1` - **ID**: `cpt-cf-file-storage-usecase-get-metadata`

**Actor**: `cpt-cf-file-storage-actor-cf-modules`

**Preconditions**:

- File exists at the specified URL

**Main Flow**:

1. Module calls get_metadata with a file URL
2. FileStorage checks authorization for read on `gts.x.fstorage.file.type.v1~` with the file's GTS type in resource context
3. FileStorage returns metadata (name, size, mime_type, GTS file type, owner, availability) without transferring content

**Postconditions**:

- Metadata returned; no content transferred

**Alternative Flows**:

- **File not found**: FileStorage returns file_not_found error
- **Authorization denied**: FileStorage returns access-denied error

### Direct Upload from External Client

- [ ] `p1` - **ID**: `cpt-cf-file-storage-usecase-direct-upload`

**Actor**: `cpt-cf-file-storage-actor-platform-user`

**Preconditions**:

- Client is authenticated with a valid API token
- Storage backend supports presigned URLs (e.g., S3, GCS, Azure Blob)

**Main Flow**:

1. Client requests a direct transfer URL for upload from FileStorage, providing file metadata (name, mime_type, size,
   GTS file type)
2. FileStorage validates the GTS file type format
3. FileStorage checks authorization for write on `gts.x.fstorage.file.type.v1~` with the file type in resource context
4. *(Phase 2)* FileStorage validates file against policies (type, size); in phase 1 all uploads are accepted
5. FileStorage registers the file metadata (including GTS file type) and ownership, assigns the target backend path
6. FileStorage generates a presigned upload URL using its own backend credentials (e.g., AWS access key), scoped to the
   assigned path and time-limited
7. *(Phase 2)* FileStorage emits audit record for the upload
8. FileStorage returns the presigned URL and file identifier to the client
9. Client uploads file content directly to the storage backend using the presigned URL
10. Storage backend validates the signature against its own key material and accepts the upload

**Postconditions**:

- File metadata and ownership registered in FileStorage before upload
- File content stored on backend via presigned URL — never transited through FileStorage
- *(Phase 2)* Audit record emitted

**Alternative Flows**:

- **Missing or invalid GTS file type at step 2**: FileStorage rejects with a validation error; no presigned URL issued
- **Authorization denied at step 3**: FileStorage returns access-denied error; no presigned URL issued
- *(Phase 2)* **Policy violation at step 4**: FileStorage returns error indicating which policy was violated
- **Presigned URL expired**: Backend rejects the upload; client must request a new presigned URL from FileStorage
- **Backend does not support presigned URLs or capability is disabled**: FileStorage returns an error indicating the
  capability is unavailable; client must use standard (proxied) upload instead
- **Upload never completed (abandoned or failed)**: Metadata registered at step 5 remains unconfirmed; garbage
  collection handles cleanup per `cpt-cf-file-storage-fr-gc-direct-uploads`

### Delete a File

- [ ] `p1` - **ID**: `cpt-cf-file-storage-usecase-delete-file`

**Actor**: `cpt-cf-file-storage-actor-platform-user`

**Preconditions**:

- User is authenticated
- User owns the file

**Main Flow** (non-versioned file):

1. Owner requests deletion of a file by its identifier
2. FileStorage checks authorization for delete on `gts.x.fstorage.file.type.v1~`
3. FileStorage revokes all active shareable links associated with the file
4. FileStorage deletes the file content from the storage backend
5. FileStorage removes file metadata and ownership records
6. *(Phase 2)* FileStorage emits audit record for the deletion

**Postconditions**:

- File content removed from storage backend
- All associated shareable links invalidated; issued signed URLs remain valid until their expiration
  (`cpt-cf-file-storage-fr-signed-urls`) — the underlying content is no longer available on the backend
- Metadata and ownership records removed
- *(Phase 2)* Audit record emitted

**Alternative Flow — versioned file, no version identifier** (`cpt-cf-file-storage-fr-file-versioning`):

1. Owner requests deletion of a file by its identifier (no version identifier supplied)
2. FileStorage checks authorization for delete on `gts.x.fstorage.file.type.v1~`
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
2. FileStorage checks authorization for delete on `gts.x.fstorage.file.type.v1~`
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

### Manage Shareable Links

- [ ] `p2` - **ID**: `cpt-cf-file-storage-usecase-manage-links`

**Actor**: `cpt-cf-file-storage-actor-platform-user`

**Preconditions**:

- User is authenticated
- User owns the file

**Main Flow**:

1. Owner requests the list of all active shareable links and signed URLs for a file
2. FileStorage checks authorization for the owner on `gts.x.fstorage.file.type.v1~`
3. FileStorage returns the list with each link's scope, expiration, and creation date
4. Owner identifies a link to revoke
5. Owner requests revocation of the link by its identifier
6. FileStorage checks authorization for the owner on `gts.x.fstorage.file.type.v1~`
7. FileStorage invalidates the link immediately
8. *(Phase 2)* FileStorage emits audit record for the link revocation

**Postconditions**:

- Revoked link returns access-denied on subsequent access
- Remaining links unaffected
- *(Phase 2)* Audit record emitted

**Alternative Flows**:

- **Authorization denied**: FileStorage returns access-denied error
- **No active links**: FileStorage returns an empty list
- **Link not found**: FileStorage returns link_not_found error
- **Owner creates a new link**: Owner requests a shareable link with desired scope and expiration; FileStorage creates
  and returns the link URL; *(Phase 2)* audit record emitted for link creation

### Multi-Backend Deployment

- [ ] `p1` - **ID**: `cpt-cf-file-storage-usecase-backend-config`

**Actor**: `cpt-cf-file-storage-actor-cf-modules`

**Preconditions**:

- FileStorage is deployed with a configured storage backend

**Main Flow**:

1. Deployment A configures FileStorage with an S3-compatible backend (e.g., AWS S3)
2. Deployment B configures FileStorage with a different backend (e.g., Azure Blob Storage)
3. Both deployments expose identical FileStorage SDK and REST APIs
4. CyberFabric modules interact with FileStorage through the SDK trait without awareness of the underlying backend
5. Upload, download, delete, metadata, and link operations behave identically regardless of backend

**Postconditions**:

- All functional requirements are met identically across different backend configurations
- Consuming modules require zero code changes when the backend changes

**Alternative Flows**:

- **Backend-specific feature unavailable**: FileStorage returns a clear error indicating the capability is unavailable
  (e.g., signed URL or direct transfer request rejected when backend does not support presigned URLs)

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
- [ ] File deletion of a non-versioned file permanently removes content and cascades to all associated shareable links
- [ ] File deletion of a versioned file without version identifier places a soft-delete marker (no physical removal)
- [ ] Authorization checked for every file operation via Authorization Service
- [ ] Tenant boundary enforced — cross-tenant modification/deletion rejected
- [ ] Shareable links work with public, tenant, and tenant-hierarchy scopes
- [ ] Signed URLs point directly to storage backend, grant download access without authentication, and storage backend
  rejects after expiration
- [ ] File owner can list active shareable links and signed URLs; can revoke shareable links (signed URLs are
  non-revocable)
- [ ] Audit record emitted for every write operation
- [ ] Policies enforce file type and size restrictions on upload (most restrictive wins across tenant and user levels)
- [ ] Direct transfer presigned URLs allow upload directly to storage backend without proxying through FileStorage
- [ ] Presigned URLs are generated using FileStorage's own backend credentials and validated by the backend via
  signature verification
- [ ] Signed URL and direct transfer requests rejected with clear error when backend does not support presigned URLs
- [ ] file_not_found error returned for non-existent files
- [ ] access_denied error returned for unauthorized operations
- [ ] Metadata-only queries complete without transferring file content
- [ ] File content is immutable — no in-place content update; changes require a new upload
- [ ] Custom metadata is updatable by the file owner; system-managed metadata is not user-updatable
- [ ] Custom metadata update changes the file's last modified date
- [ ] File ownership is immutable after creation except through explicit ownership transfer or owner deletion workflows
- [ ] Every file has a mandatory GTS file type assigned at upload time; uploads without a file type are rejected
- [ ] GTS file type is immutable after creation
- [ ] Authorization requests include the file's GTS type, enabling per-type access decisions
- [ ] A module authorized only for type A cannot access files of type B
- [ ] FileStorage SDK and REST API behave identically regardless of configured storage backend
- [ ] File listing returns metadata only, is paginated, and requires owner type filter
- [ ] Multipart upload assembles parts into a complete file with correct metadata
- [ ] Upload rejected when declared mime_type does not match actual file content
- [ ] Orphaned metadata records from unconfirmed direct uploads are detected, reconciled against backend state, and
  cleaned up automatically
- [ ] File owner can toggle download availability via metadata update
- [ ] Each backend declares its supported capabilities (presigned URLs, versioning, multipart upload)
- [ ] Consumers can discover backend capabilities at runtime
- [ ] Operations requiring an unsupported capability return a clear error
- [ ] File versioning creates a new version on each upload; previous versions remain accessible by opaque version ID
- [ ] All versions of a file are listable with version ID, size, timestamp, and current-version flag
- [ ] Soft-delete places a logical delete marker; current version becomes inaccessible but content is not physically
  removed
- [ ] Soft-deleted content counts against storage quota
- [ ] Restore of a soft-deleted file removes the delete marker and reinstates the previous current version
- [ ] Garbage collection does not clean up soft-deleted versions
- [ ] Permanent delete of a specific version removes only that version
- [ ] Declared capabilities are independently configurable (enable/disable) per backend
- [ ] A capability disabled by configuration behaves identically to an unsupported capability
- [ ] Download and metadata responses include ETag header
- [ ] Conditional download with If-None-Match returns 304 Not Modified when file is unchanged
- [ ] Metadata update with If-Match returns 412 Precondition Failed when file state has changed
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
- [ ] HTTP Range requests return partial content for large files; seeking and resumable downloads supported
- [ ] S3-compatible API exposes upload and download operations usable by standard S3 tooling and SDKs
- [ ] WebDAV API enables native filesystem-like mounting and file access on client operating systems
- [ ] Retention policies automatically expire and delete files based on configured age, inactivity, or custom metadata
  criteria; per-file retention overrides are honored
- [ ] Sharing model restrictions reject link creation for policy-disabled sharing models (public, tenant, hierarchy,
  signed URLs)
- [ ] Storage backends can be connected and configured at runtime without service rebuild or redeployment
- [ ] File ownership transferable by current owner to another user or tenant; transfer requires authorization of both
  parties and emits an audit record
- [ ] Custom metadata operations rejected when exceeding configurable limits (max pairs, key length, value length, total
  size)
- [ ] Read audit records emitted for proxied downloads and shareable link access when enabled by policy; not emitted for
  presigned URL downloads

## 10. Dependencies

| Dependency            | Description                                                        | Criticality |
|-----------------------|--------------------------------------------------------------------|-------------|
| ModKit Framework      | Module lifecycle, ClientHub for service registration               | p1          |
| Authorization Service | Access decisions for `gts.x.fstorage.file.type.v1~` resources     | p1          |
| Audit Infrastructure  | Platform audit event sink                                          | p2          |
| Usage Collector       | Receives storage usage reports for metering and billing            | p2          |
| Quota Enforcement     | Per-owner storage quota enforcement                                | p2          |
| EventBroker           | Publishes and consumes file/platform events                        | p2          |
| Serverless Runtime    | Executes configurable workflows for lifecycle operations           | p2          |

## 11. Assumptions

- Authorization Service is available and supports `gts.x.fstorage.file.type.v1~` resource type
- All file access respects tenant boundaries at the platform level
- Initial storage backend is configured at deployment time; runtime backend switching is phase 2
- File URLs are internal to CyberFabric; external access is via shareable links or signed URLs
- Policy configuration is available to tenant administrators and users through the platform

## 12. Risks

| Risk                                                                | Impact                                                         | Mitigation                                                                                                                                              |
|---------------------------------------------------------------------|----------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------|
| Storage service unavailability blocks all file-dependent operations | High — multimodal AI, document workflows disrupted             | Design for graceful degradation; clear error propagation to consumers                                                                                   |
| Large file sizes increase request latency for consuming modules     | Medium — slow responses for multimodal and document operations | Metadata pre-fetch enables size validation; streaming support for large files                                                                           |
| Signed URL key compromise enables unauthorized file access          | High — data exposure                                           | Key rotation is a backend configuration concern (credentials updated in backend config); short default expiration; shareable links for revocable access |
| Policy misconfiguration blocks legitimate uploads                   | Medium — user frustration                                      | Policy validation on save; clear error messages identifying which policy was violated                                                                   |

## 13. Open Questions

None.

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/)
- **Features**: [features/](./features/)
