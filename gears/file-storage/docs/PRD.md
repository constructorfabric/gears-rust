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
  - [3.1 Gear-Specific Environment Constraints](#31-gear-specific-environment-constraints)
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
  - [6.1 Gear-Specific NFRs](#61-gear-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
  - [6.3 Applicability Notes](#63-applicability-notes)
  - [6.4 Five Quality Vectors Analysis](#64-five-quality-vectors-analysis)
  - [6.5 Quality Framework Conformance](#65-quality-framework-conformance)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
  - [Upload a File](#upload-a-file)
  - [Fetch File for Gear Processing](#fetch-file-for-gear-processing)
  - [Validate File Metadata Before Processing](#validate-file-metadata-before-processing)
  - [Delete a File](#delete-a-file)
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

FileStorage is a universal file storage and management service for the Gears middleware. It provides upload,
download, metadata management, and tenant-scoped access control for any gear or user within the platform. All
access in P1 is authenticated — anonymous/external sharing is deferred to a separate concern (P3, see `§5.3`).

FileStorage is split into two cooperating planes (see
[ADR-0003](./ADR/0003-cpt-cf-file-storage-adr-sidecar-data-plane.md)):

- a **control plane** (the FileStorage API/SDK) that owns metadata, authorization, versioning, and
  conditional-request semantics, and whose REST surface **never carries file content** — it issues short-lived
  **signed URLs** instead;
- a **data-plane sidecar** that is the only component to move user bytes, is connected to the storage backends, and
  serves content exclusively through those signed URLs.

The sidecar is a deliberate **platform-level exception** to the standard "API Gateway owns REST hosting" model: it is
**not** an ordinary gear REST route fronted by the API Gateway, but a dedicated byte-moving data plane with its own
host/URL that clients reach directly via signed URLs. This exception is what lets the data plane scale independently
and keeps content off the control-plane (gateway) path; how it authenticates and is contracted is specified in
DESIGN.md §3.3 and §4.5.

Consequently every content operation is at least two requests: a control request to obtain a signed URL, plus one or
more data requests against the sidecar. Backends are never addressed by clients directly — the signed URL always
points at the sidecar — so backend opacity, centralized per-byte metering, and uniform audit/policy coverage are
preserved while the byte-moving data plane scales independently of the control plane.

The service supports pluggable storage backends, tenant-scoped access control with an ownership model, and
policy-driven governance for file types and sizes.

### 1.2 Background / Problem Statement

Gears and platform users require file storage for various purposes: gears handle multimodal AI content
(images, audio, video, documents), documents and artifacts, reporting outputs, and platform users need direct file
access through standard protocols.

Without a dedicated storage service, each gear implements ad-hoc file handling, media gets inlined as base64 in API
payloads (bloating requests and hitting size limits), provider-generated URLs expire leaving consumers with broken
links, and there is no unified access control or policy enforcement across the platform.

FileStorage solves this by providing a centralized, tenant-aware storage service with persistent URLs, pluggable
backends, and standardized access interfaces — functioning as a superset of S3 and WebDAV capabilities within the
Gears security and governance model.

### 1.3 Goals (Business Outcomes)

- Unified file storage accessible by all Gears and platform users
- Tenant-scoped and origin-gear-scoped access control with tenant, user and gear ownership model
- Policy-driven governance over file types, sizes, and events
- Audit trail for all write operations
- Pluggable storage backends without service rebuild

### 1.4 Success Metrics

| Metric                                   | Baseline                                 | Target                                                           | Timeframe                      |
|------------------------------------------|------------------------------------------|------------------------------------------------------------------|--------------------------------|
| Gear adoption rate                     | 0% (ad-hoc file handling)                | 90%+ of file-dependent gears use FileStorage SDK               | 6 months after GA              |
| Base64-inlined media payloads            | Present in LLM Gateway and other gears | 0 base64 file payloads in gears that adopted FileStorage       | 3 months after gear adoption |
| Broken/expired provider URLs             | Recurring in downstream workflows        | 0 broken URLs for files within retention period                  | Ongoing after GA               |
| Audit coverage for file write operations | No centralized audit                     | 100% of write operations audited                                 | Phase 2                        |
| Multi-backend deployment                 | Single ad-hoc storage per gear         | At least 2 backend types validated (e.g., S3 + local filesystem) | At GA                          |

Beyond these gear-specific metrics, file-storage is also evaluated against the five quality vectors of the
[Constructor Gears Quality Framework](https://github.com/constructorfabric/vision/blob/main/CONSTRUCTOR_GEARS_QUALITY_FRAMEWORK.md),
in priority order Efficiency → Reliability → Performance → Security → Versatility; see §6.4 for the show-stopper
requirements per vector.

### 1.5 Glossary

| Term                | Definition                                                                                                                                                                                                                                                                              |
|---------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| File                | Binary content stored in FileStorage with associated metadata                                                                                                                                                                                                                           |
| Control Plane       | The FileStorage API/SDK. Owns metadata, authorization, versioning, and conditional-request semantics; issues signed URLs. Its REST surface never carries file content                                                                                                                    |
| Sidecar (Data Plane)| The only component that moves user bytes. Has its own domain/URL, is connected to the storage backends, and verifies signed-URL signatures — it has no DB connection of its own and makes no platform-JWT call of any kind. Reports back to the control plane (finalize, per-part hash) over token-authenticated HTTP within a documented trusted network boundary, or over TLS/equivalent authenticated encryption when the callback crosses an untrusted or shared network, never an SDK call. Serves content only through signed URLs |
| Signed URL          | A short-lived, control-minted **codec-equivalent Ed25519-signed token** (bespoke `base64url(json).base64url(ed25519_signature)` in P1 -- opaque and codec-evolvable per ADR-0004's Implementation note, not a literal PASETO library) pointing at the sidecar that authorizes one content operation (`GET`/`PUT`/part) on a specific object, subject to AND-combined claims (`exp`, optional `ip`, optional token-claim predicates, upload size/hash). Carried in the query (`?fs-token=`) or a header; **opaque** to all but control+sidecar (`cpt-cf-file-storage-fr-signed-urls`) |
| File ID             | The immutable uuid identity of a logical file. The current content is reached by resolving the file's content pointer (`content_id`)                                                                                                                                                     |
| Version ID          | A uuid assigned by FileStorage (control plane) identifying one immutable content blob; the backend object lives at `/{file_id}/{version_id}` and is never mutated in place                                                                                                                |
| Content Pointer (`content_id`) | The `version_id` currently bound as a file's live content; changing content is a pointer swap, not an in-place mutation. The content-only ETag derives from `(file_id, content_id)`                                                                                            |
| Metadata Revision (`meta_version`) | A monotonic counter bumped on metadata-only writes; the validator for `If-Match-Metadata`                                                                                                                                                                          |
| Bind                | The control-plane operation that swaps a file's `content_id` to a (`pending` → `available`) version under optimistic CAS (`If-Match`). A conflict (`400 failed_precondition`) is retried by re-binding the already-uploaded `version_id` — never by re-uploading bytes                                       |
| Metadata            | File properties: system-managed (name, size, mime_type, GTS file type, dates, owner) and user-defined custom key-value pairs                                                                                                                                                            |
| Custom Metadata     | User-defined key-value pairs attached to a file, analogous to S3 object metadata                                                                                                                                                                                                        |
| Owner               | The principal that owns a file: `owner_kind ∈ {user, app}` plus `owner_id`. Every file also has a separate immutable `tenant_id`                                                                                                                                                       |
| FileShare           | Working name for the future (P3) sharing capability built on top of FileStorage. Covers anonymous/public URLs, per-recipient grants, expirations, download counters, etc. Whether it ships as a separate Gear or as an extension of FileStorage is deferred to a future ADR  |
| Sharable Link       | A FileShare-issued (P3) reference to a FileStorage file with optional content/version pinning and access rules (anonymity, expiration, recipients, maximum download count). Out of P1 scope                                                                                                |
| Storage Backend     | An underlying storage system (S3, GCS, Azure Blob, NFS, FTP, SMB, WebDAV) used for persisting file content                                                                                                                                                                              |
| Policy              | A set of rules (allowed file types, size limits, events, sharing models) that constrain file operations; applicable at the tenant level and the user level independently — when both apply, the most restrictive value per aspect wins                                                  |
| File Version        | An immutable content blob created on each content write, stored as a distinct backend object `/{file_id}/{version_id}`. FileStorage-level (not backend-native), so versioning works on any backend (`cpt-cf-file-storage-fr-file-versioning`)                                            |
| File Type (GTS)     | A GTS type identifier assigned to every file at upload time that classifies the file by domain, actor, and purpose (e.g., `gts.cf.fstorage.file.type.v1~x.genai.llm.autogenerated.v1~`); used by the Authorization Service to enforce per-type access control between actors and gears |
| Backend Capability  | An optional feature that a storage backend may or may not support (e.g., presigned URLs, versioning, multipart upload); FileStorage discovers available capabilities per backend and adapts its behavior accordingly                                                                    |

## 2. Actors

### 2.1 Human Actors

#### Platform User

**ID**: `cpt-cf-file-storage-actor-platform-user`

**Role**: Authenticated user who uploads, downloads, and manages files through the platform UI or API.
**Needs**: Direct file access, sharing capabilities, metadata management, and self-service link management.

### 2.2 System Actors

#### Gears

**ID**: `cpt-cf-file-storage-actor-cf-gears`

**Role**: Any Gear requiring file upload, download, metadata retrieval, or link management (e.g., LLM
Gateway for multimodal media, document management gears, reporting gears).

## 3. Operational Concept & Environment

### 3.1 Gear-Specific Environment Constraints

FileStorage operates within the standard Gears runtime environment. Authentication and identity management are
fully delegated to the platform — FileStorage does not implement its own authentication layer. All incoming requests are
pre-authenticated by the platform infrastructure, and FileStorage receives the caller's identity context (user, tenant,
roles) from the platform authentication middleware.

## 4. Scope

### 4.1 In Scope

- Two-plane architecture: a control plane (API/SDK, metadata + authorization) and a data-plane sidecar (byte
  transfer), per [ADR-0003](./ADR/0003-cpt-cf-file-storage-adr-sidecar-data-plane.md)
- Signed-URL content access: the control plane issues short-lived signed URLs that authorize a single content
  operation against the sidecar (constraints: expiry, optional ip, optional token-claim predicates)
- Immutable-blob + content-pointer model with FileStorage-level versioning (backend-agnostic)
- Upload, download, delete, and list files
- Rich file metadata storage, retrieval, and update
- File ownership by user or app (Gear) within a tenant
- GTS file type classification for per-actor access control
- Authorization checks via Authorization Service
- Audit trail for all write operations and optional read audit logging
- Policies (file types, size limits, events) at tenant and user levels
- Pluggable storage backend abstraction
- Backend migration — relocating a file's content between backends without rotating its URL (P2; non-versioned files)
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
- All external/anonymous access (anonymous URLs, scope-based shareable links, per-recipient grants, time-bounded
  or count-limited access) — deferred to P3 (see `§5.3`). FileStorage P1 exposes only the auth-required surface
- S3-compatible and WebDAV protocol facades

## 5. Functional Requirements

### 5.1 Core File Operations

#### Upload File

- [x] `p1` - **ID**: `cpt-cf-file-storage-fr-upload-file`

The system **MUST** accept file content with metadata and persist it. Upload is a two-step exchange: the client first
asks the control plane for a **signed upload URL** (`cpt-cf-file-storage-fr-signed-urls`), then transfers the bytes to
the **sidecar** at that URL; the control-plane REST surface never receives the content itself. Each upload writes a
new **immutable** content blob to the backend object `/{file_id}/{version_id}`; the file's live content is a pointer
(`content_id`) that is **bound** to that version (`cpt-cf-file-storage-fr-file-versioning`). Backend content is
**never mutated in place** — replacing content writes a new version and swaps the pointer; partial-byte mutation is
**not** supported.

Binding the pointer is an optimistic compare-and-swap guarded by `If-Match` (`cpt-cf-file-storage-fr-conditional-requests`):
if the file's content changed concurrently the bind returns `400 failed_precondition`, and the client **MUST** be able to retry the bind
against the already-uploaded `version_id` **without re-uploading the bytes**. Rebinding is a **control-plane** operation
independent of the signed upload URL — the upload URL's expiry does **not** affect the retry and the client does **not**
re-presign; the failed-precondition response returns the current content ETag for the fresh `If-Match`. (Re-presigning instead would upload a
new sibling version, later reconciled by `cpt-cf-file-storage-fr-orphan-reconciliation`.)

**Rationale**: All platform gears and users need to store files — gears store generated content, documents, and
artifacts, users upload files directly. Separating the signed control request from the sidecar byte transfer keeps the
control plane out of the data path; the immutable-blob + pointer model makes versioning backend-agnostic and makes a
concurrent-write conflict cheap to recover from (re-bind, never re-upload).
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

#### Download File

- [x] `p1` - **ID**: `cpt-cf-file-storage-fr-download-file`

The system **MUST** retrieve file content for consumption by requesting actors via a two-step exchange: the client
asks the control plane for a **signed download URL** (`cpt-cf-file-storage-fr-signed-urls`), then fetches the bytes
from the **sidecar** at that URL. The signed URL pins a specific `content_id` (an immutable version), so it is stable
and cacheable; the sidecar serves the bytes, honours `Range` (`cpt-cf-file-storage-fr-range-requests`) and conditional
requests, and emits the response headers carried in the signed URL. The control-plane REST surface never returns the
content itself. File **metadata** is retrieved separately and directly from the control plane
(`cpt-cf-file-storage-fr-get-metadata`).

**Rationale**: All platform gears and users need to retrieve stored files — gears fetch media and documents, users
download files directly. Issuing a signed URL keeps the control plane out of the byte path while preserving backend
opacity (the URL points at the sidecar, never the backend) and central metering/audit on the sidecar.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

#### Delete File

- [x] `p1` - **ID**: `cpt-cf-file-storage-fr-delete-file`

The system **MUST** allow any actor authorized for the **delete** action on the file's GTS type
(`cpt-cf-file-storage-fr-authorization`) to delete a file. Deleting a file removes its metadata, ownership records, and
**all** of its versions; the backend objects are deleted best-effort (a failed backend delete degrades to an orphan
reconciled by the P2 cleanup engine, `cpt-cf-file-storage-fr-orphan-reconciliation`). Deletion is **idempotent** —
re-deleting an already-deleted file returns `404`. A single version may instead be removed by `version_id`
(`cpt-cf-file-storage-fr-file-versioning`), deleting only that version; deleting the only remaining version is
equivalent to deleting the file.

**Rationale**: Authorized actors need to remove files that are no longer needed. Recovery from an accidental content
overwrite is provided by versioning/restore within the file's lifetime (`cpt-cf-file-storage-fr-file-versioning`); a
file-level delete is intentional and removes the whole file. The metadata row is removed before the best-effort
backend delete, so a deleted file never leaves a row pointing at missing bytes.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

#### Get File Metadata

- [x] `p1` - **ID**: `cpt-cf-file-storage-fr-get-metadata`

The system **MUST** return file metadata (name, size, mime_type, GTS file type, created date, modified date, owner,
and custom metadata) without transferring file content.

**Rationale**: Consumers validate file properties (size limits, type compatibility) and read custom metadata before
initiating downloads, avoiding wasted bandwidth on incompatible files.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

#### List Files

- [x] `p1` - **ID**: `cpt-cf-file-storage-fr-list-files`

The system **MUST** support listing files with their metadata (no content transfer). The caller **MUST** specify the
owner type as a mandatory filter:

- **User-owned** — files owned by a specific user (`owner_kind = user`)
- **App-owned** — files owned by a Gear (`owner_kind = app`)

The response **MUST** be paginated following the platform API guidelines (keyset cursor-based pagination, navigable
in both directions, with configurable page size; offset pagination **MUST NOT** be offered). The system **MUST** support optional additional filters (mime_type, date range, custom metadata
keys).

**Rationale**: Users and gears need to discover and browse files they own or have access to. Mandatory owner type
filtering prevents unbounded queries across all files and aligns with the ownership model.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

#### Multipart Upload

- [x] `p2` - **ID**: `cpt-cf-file-storage-fr-multipart-upload`

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
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

#### Auto-Bind on Upload

- [x] `p2` - **ID**: `cpt-cf-file-storage-fr-auto-bind`

At file-creation time, the client **MAY** select a `bind` mode — `"auto"` (the default) or `"manual"`. Under
`bind: "auto"`, finalizing the uploaded version **MUST**, in the same transaction, also bind it as the file's current
content — a single-part upload finalizes and binds together, and a multipart upload's `complete` binds together with
its finalize — so the common case needs no separate, later `bind` call. Under `bind: "manual"`, the system **MUST**
leave `content_id` untouched at finalize and require the client to make the existing, separate `bind` request to
activate the uploaded content. Either way the bind **MUST** remain the same optimistic-CAS operation
(`cpt-cf-file-storage-fr-conditional-requests`): auto-bind either wins the CAS or reports a conflict without failing
the upload, and the outcome **MUST** be reported back to the caller (`X-FS-Bound` header for single-part,
`bind_state` field for multipart complete).

**Rationale**: Auto-bind collapses the common "upload then immediately make it live" case from three round trips
(presign, upload, bind) down to two (presign, upload) without weakening the CAS guarantee a manual bind already
provides — a lost auto-bind CAS is reported, not silently dropped, and resolves through the same manual re-bind path.
`bind: "manual"` is retained for callers that need to stage content before making it current (e.g. content requiring
a later approval step).
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

#### Multipart Complete Under a Completion Lease

- [x] `p2` - **ID**: `cpt-cf-file-storage-fr-multipart-complete-lease`

The system **MUST** serialize concurrent `complete` calls against the same multipart session through a completion
lease: exactly one caller **MUST** win the lease and perform the assembly/finalize work; every other caller arriving
while the lease is held **MUST** receive `202 Accepted` (`state: "completing"`) instead of performing or waiting on
the assembly itself, and **MUST** be able to poll for the result by re-issuing the identical `complete` call. A lease
holder that fails or dies before finishing **MUST NOT** strand the session indefinitely — once the lease's own
expiry passes, the next `complete` call **MUST** be able to take the lease over and retry. A retry of an
already-completed session **MUST** be idempotent: it **MUST** return the original stored result rather than
re-running assembly, re-verifying parts, or performing any further state transition.

**Rationale**: Multipart `complete` performs non-trivial work (missing-part verification, backend assembly, hashing,
finalize, and — for an auto-bind session — the bind) that must not run twice concurrently for the same session, and
must not be lost if the instance that started it crashes mid-assembly. A time-bounded lease with a `202`/poll contract
gives callers a well-defined way to wait without either blocking the request thread or racing a second assembly
attempt, while a dead lease holder's session still converges instead of getting stuck.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

#### Sidecar S2S Callbacks

- [x] `p2` - **ID**: `cpt-cf-file-storage-fr-sidecar-callbacks`

The sidecar **MUST** report back to the control plane over token-authenticated HTTP callbacks within a documented
trusted network boundary — or over TLS/equivalent authenticated encryption when the callback path crosses an
untrusted or shared network — for two events:
finalizing a version after a successful single-part `PUT`, and reporting a successfully-written multipart part. Each
callback's sole authorization **MUST** be the same signed upload token (`cpt-cf-file-storage-fr-signed-urls`) that
authorized the original upload — no separate app-token or on-behalf-of delegation. The control plane **MUST** accept
the callback within a short grace period after the token's `exp` has passed, since the callback necessarily arrives
after the byte transfer the token authorized completes. On finalize the control plane **MUST** check the reported
size against the stored object's length and **MUST** reject a missing object; it **MUST NOT** re-read the object and
persists the content hash the authenticated sidecar measured while streaming. A part's reported size **MUST** be
checked against the claims embedded in that part's own token.

**Rationale**: The sidecar holds no metadata-DB connection and no delegated user identity of its own
(`cpt-cf-file-storage-fr-authorization`), so the signed token it already verified for the byte transfer is the only
authorization available for reporting the outcome back. A short post-`exp` grace period accommodates the callback's
inherent lag behind the transfer without extending the token's authority for any new operation. Re-reading every
object at finalize would double the read traffic of every upload; the sidecar is an authenticated internal component
(signed token plus the mandatory `x-fs-internal-token`), so its measured digest is trusted, while the size check
still catches a missing or truncated object.
**Actors**: `cpt-cf-file-storage-actor-cf-gears`

#### Internal Callback Token (Second Factor)

- [x] `p2` - **ID**: `cpt-cf-file-storage-fr-callback-internal-token`

The system **MUST** require a shared-secret second factor, carried as the `x-fs-internal-token` header, on top of
the signed upload token for the sidecar's finalize and report-part callbacks
(`cpt-cf-file-storage-fr-sidecar-callbacks`). The control plane **MUST** reject a callback with a missing or
mismatched header. A deployment without a configured secret **MUST** fail at startup on both the control plane and
the sidecar.

**Rationale**: The callbacks persist whatever the sidecar reports (`cpt-cf-file-storage-fr-sidecar-callbacks`), so
they must be reachable only by the deployment's own sidecar; a mandatory secret makes a misconfigured deployment fail
fast instead of silently running without the second factor.
**Actors**: `cpt-cf-file-storage-actor-cf-gears`

#### Content-Type Validation

- [x] `p1` - **ID**: `cpt-cf-file-storage-fr-content-type-validation`

The system **MUST** validate the declared mime_type against the actual file content (magic bytes / file signature) on
every upload (all upload traffic transits the sidecar). If the declared type does not match the detected type, the
system **MUST** reject the upload with an error indicating the mismatch.

For multipart uploads (`cpt-cf-file-storage-fr-multipart-upload`), the system **MUST** validate the declared mime_type
against the assembled object's leading bytes, which contain the file's magic bytes / file signature. Validation
**MUST** occur after all parts are assembled and before the upload is finalized (i.e. before the version is ever
marked available) — not deferred to a later read. If the detected type does not match the declared mime_type, the
system **MUST** reject the completion request and **MUST NOT** finalize the version; the assembled-but-unfinalized
content is orphaned content reclaimed by the same mechanism as any other orphan
(`cpt-cf-file-storage-fr-orphan-reconciliation`).

**Rationale**: Without content inspection, a client can declare `image/png` but upload an executable, trivially
bypassing file type policies. Content-type validation ensures declared types are trustworthy for downstream consumers
and policy enforcement. Validating the assembled object once, at completion, rather than the first part in isolation,
gives the same guarantee without requiring every backend's minimum part size to exceed the longest magic-byte
sequence, and it reuses the identical bounded-prefix sniff the single-part path already performs on a ranged read.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

### 5.2 Ownership & Access Control

#### File Ownership

- [x] `p1` - **ID**: `cpt-cf-file-storage-fr-file-ownership`

The system **MUST** associate every file with `tenant_id` (mandatory, immutable) plus `owner_kind ∈ {user, app}` and
`owner_id`. `user` is a platform user; `app` is a Gear (e.g., LLM Gateway owning its generated media).
The owner principal is immutable after creation except through explicit ownership transfer
(`cpt-cf-file-storage-fr-ownership-transfer`) or owner deletion workflows (`cpt-cf-file-storage-fr-owner-deletion`).
`tenant_id` is never mutable.

**Rationale**: Ownership determines who can manage (delete, update metadata) a file and establishes the basis for
access control decisions. Separating `tenant_id` from `(owner_kind, owner_id)` reflects how Gears scopes data:
tenant is the hard boundary for isolation, while the owner identifies a specific principal within the tenant.
Gears own platform-generated content (LLM outputs, reports) via `owner_kind = app` without requiring an artificial
human user.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

#### Authorization Checks

- [x] `p1` - **ID**: `cpt-cf-file-storage-fr-authorization`

The system **MUST** verify authorization for every file operation by requesting an access decision from the
Authorization Service. Read, write, and delete operations **MUST** be checked against `gts.cf.fstorage.file.type.v1~` resources in
the context of the requesting user. Authorization requests **MUST** include the file's GTS type
(`cpt-cf-file-storage-fr-file-type-classification`) in the resource context to enable per-type access decisions.

For content operations the read/write decision is made by the **control plane** when it issues the signed URL, and
the signed URL's constraints carry that authorization to the **sidecar**. When the sidecar reports back to the
control plane (finalizing an upload, reporting a multipart part), it does **not** call under an app-token or any
other delegated-user identity: the verified signed token itself — issued at the moment the original authorization
decision was made — is the sidecar's sole authorization for that one `(file_id, version_id)` operation, with no
fresh access-control check on the callback (see [ADR-0003](./ADR/0003-cpt-cf-file-storage-adr-sidecar-data-plane.md)).
The sidecar never performs the **bind** (swapping a file's live content pointer) on the user's behalf; binding is a
separate, later request the client issues to the control plane directly, under its own authorization, except for a
narrow first-content case where the control plane's own finalize handler performs the pointer swap inline under the
same signed token (see `cpt-cf-file-storage-fr-conditional-requests`'s bind-on-finalize note).

**Rationale**: All file access must be governed by the platform's centralized authorization model to enforce role-based,
tenant-scoped, and type-scoped permissions. Carrying the authorization decision inside the signed token lets the
sidecar act in the data path without becoming an authorization principal, or needing a delegated identity, in its own
right.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

#### Tenant Boundary Enforcement

- [x] `p1` - **ID**: `cpt-cf-file-storage-fr-tenant-boundary`

The system **MUST** enforce tenant isolation on every file operation: a principal in one tenant **MUST NOT**
access files owned by another tenant.

**Rationale**: Multi-tenant platforms require strict data isolation to prevent unauthorized cross-tenant access.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

#### Data Classification

- [x] `p1` - **ID**: `cpt-cf-file-storage-fr-data-classification`

FileStorage treats all stored files as opaque binary blobs and does **NOT** inspect, classify, or label file content by
sensitivity level. Data classification (public, internal, confidential, restricted) is the responsibility of consuming
gears and policies. FileStorage enforces access control through its authorization model and tenant boundaries
regardless of data sensitivity.

**Rationale**: FileStorage is a general-purpose storage service that serves gears with diverse data sensitivity
requirements. Embedding classification logic in the storage layer would couple it to domain-specific semantics. Instead,
consuming gears classify their own data and rely on FileStorage's authorization and tenant isolation to enforce access
boundaries appropriate to the sensitivity level.
**Actors**: `cpt-cf-file-storage-actor-cf-gears`

#### File Type Classification

- [x] `p1` - **ID**: `cpt-cf-file-storage-fr-file-type-classification`

The system **MUST** require a GTS file type identifier on every file at upload time. The file type classifies the file
by domain and purpose following the GTS type format (e.g. `gts.cf.fstorage.file.type.v1~x.genai.llm.autogenerated.v1~`
for LLM-generated files). The file type **MUST** be:

- Mandatory — uploads without a file type **MUST** be rejected
- Immutable — the file type **MUST NOT** be changeable after creation
- Stored as system-managed metadata — returned in all metadata queries alongside other system fields
- Validated — the system **MUST** verify that the provided type follows the GTS type format

The system **MUST** be able to use the file type to make per-type access decisions, enabling isolation
between actors and gears — a gear **MUST** only be able to access files of types it is authorized for. File type
authorization is enforced through the existing authorization model (`cpt-cf-file-storage-fr-authorization`).

**Rationale**: Without file type classification, any gear with general file access can read files created by any other
gear, breaking isolation between platform components. GTS types enable fine-grained, per-actor access control — e.g.,
the LLM Gateway can only access LLM-generated files, the Feedback gear can only access feedback-related files —
without requiring separate storage namespaces or custom authorization logic per gear.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

#### Ownership Transfer

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-ownership-transfer`

The system **MUST** allow the current file owner to transfer ownership of a file to another principal (user or app)
within the **same tenant**. Cross-tenant transfer is **NOT** supported. Ownership transfer **MUST** be an audited
operation and **MUST** require authorization of both the current owner and the receiving principal.

**Rationale**: As teams and gears evolve, files may need to change hands. Restricting transfers to within the
file's tenant preserves the tenant-isolation invariant.
**Actors**: `cpt-cf-file-storage-actor-platform-user`

**Partial:** the endpoint, atomic owner swap, audit row, file event, and usage-delta reporting are implemented;
authorization of the receiving principal is not — `new_owner_id` is only checked against the nil UUID, since this
gear has no account-management client to verify it names a real, existing, same-tenant principal.

### 5.3 Sharing

FileStorage P1 exposes **only an authenticated REST surface**. Anonymous/public access, per-recipient grants,
expirations, content/version pinning, download counters, and any other sharing primitives are **out of P1 scope
and deferred to P3**.

The working name for the deferred capability is "FileShare". Whether it ships as a separate Gear or
as an extension of FileStorage itself is an open architectural decision to be settled by a future ADR at the
time the functionality is implemented. FileStorage P1 stores no sharing-related state, exposes no anonymous URL
namespace, and has no JWT-bypass paths — its surface is identical for every consumer and always goes through
platform authentication and the Authorization Service.

**Rationale**: Public/anonymous access is a sharing concern, not a storage concern. Keeping FileStorage purely
internal in P1 lets sharing semantics evolve independently inside a single gear with the appropriate data model, and
eliminates JWT-bypass surfaces and owner-private-header redaction logic from FileStorage.

### 5.4 Policies (Phase 2)

#### Allowed File Types Policy

- [x] `p2` - **ID**: `cpt-cf-file-storage-fr-allowed-types-policy`

The system **MUST** allow owners to define policies specifying which file types (by mime_type) are permitted for
upload. Uploads of disallowed types **MUST** be rejected.

**Rationale**: Tenants need to restrict uploads to approved file types for security and compliance (e.g., blocking
executable files).
**Actors**: `cpt-cf-file-storage-actor-platform-user`

#### File Size Limits Policy

- [x] `p2` - **ID**: `cpt-cf-file-storage-fr-size-limits-policy`

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

The system **MUST** emit events to the EventBroker gear on file write operations (upload, update, delete). Owner
policy **MUST** define which event types are enabled.

**Rationale**: Enables integration with downstream consumers for workflows such as antivirus scanning, content
moderation, indexing, or backup triggers — without coupling FileStorage to specific consumers.
**Actors**: `cpt-cf-file-storage-actor-platform-user`

**Partial:** every write operation inserts a transactional-outbox row into `events_outbox`; there is no relay that
drains it to the EventBroker gear, so no downstream consumer receives these events yet — see
[features/audit-trail.md](./features/audit-trail.md), whose undrained-relay caveat covers this table's sibling
`audit_outbox` under the identical pattern.

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
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

**Current status**: Usage reporting is **not sent in any deployment** — the call sites that would build and emit a
usage report exist (`FileService`, `MultipartService`, and `CleanupEngine` all accept an optional usage-reporting
sink), but every one of them is always constructed with the sink unset (`usage_reporter: None`), so no usage delta is
ever reported. No Usage Collector client is wired in. Detail in [operations.md](./operations.md)'s "Storage quota
(not enforced)" section, which covers this sibling gap.

#### Storage Quota Enforcement

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-storage-quota`

The system **MUST** check with the Quota Enforcement service before accepting any operation that increases storage
consumption (including uploads and version creation). Operations that would exceed the owner's storage quota **MUST** be
rejected.

**Rationale**: Without storage quotas, tenants can consume unbounded storage, increasing costs and risking resource
exhaustion for the platform. Quota checks must cover all storage-consuming operations, not only initial uploads, to
prevent quota bypass through versioned overwrites.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

**Current status**: Storage quota is **not enforced in any deployment** — the Quota Enforcement service this
requirement depends on does not exist yet. `file-storage`'s consumer side (the `QuotaClient` port and its
fail-closed call sites) is implemented and ready to enforce the check once that service exists. Technical detail in
[DESIGN.md](./DESIGN.md) (`quota-adapter`) and [operations.md](./operations.md).

### 5.5 Metadata

#### Rich Metadata Storage

- [x] `p1` - **ID**: `cpt-cf-file-storage-fr-metadata-storage`

The system **MUST** store and return the following system-managed metadata for every file:

- File name (original upload name)
- File size (bytes)
- File type (mime_type)
- GTS file type (`cpt-cf-file-storage-fr-file-type-classification`)
- Creation date
- Last modified date
- Owner (`owner_kind ∈ {user, app}` + `owner_id`) and `tenant_id`

In addition, the system **MUST** support user-defined custom metadata as arbitrary key-value string pairs. Custom
metadata **MUST** be specifiable at upload time and updatable after upload. The system **MUST** return custom metadata
alongside system-managed metadata in metadata queries.

**Rationale**: Rich metadata enables file browsing, search, validation, and governance across the platform. Custom
metadata enables consumers to attach domain-specific context (tags, categories, processing status, source identifiers)
without schema changes — following the established pattern used by S3 object metadata, GCS custom metadata, and Azure
Blob metadata.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

#### Update Custom Metadata

- [x] `p1` - **ID**: `cpt-cf-file-storage-fr-update-metadata`

Any actor authorized for the **write** action on the file's GTS type
(`cpt-cf-file-storage-fr-authorization`) **MUST** be able to update the file's `custom_metadata` (user-defined
key-value pairs).

The set of principals admitted by the Authorization Service for this action **MAY** include the file's current owner,
other principals within the same tenant, or service identities — the model is policy-driven, not hard-coded to
"owner". All other system-managed metadata (`file_id`, `tenant_id`, `owner_kind`, `owner_id`, `name`, `size`,
`mime_type`, `gts_file_type`, `created_at`) is **NOT** user-updatable — it is maintained by the system. A successful
update **MUST** advance the file's last modified date.

**Rationale**: Custom metadata evolves as files are processed, categorized, or annotated by consuming gears. System
metadata reflects the immutable physical properties of the file and must remain authoritative. Routing the
authorization decision through `cpt-cf-file-storage-fr-authorization` (rather than hard-coding "only the owner can
update") keeps the access-control model centralized in the platform Authorization Service and lets tenants extend
write permission to additional principals (delegated maintainers, automation service identities, etc.) without
schema changes.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

#### Custom Metadata Limits

- [x] `p2` - **ID**: `cpt-cf-file-storage-fr-metadata-limits`

The system **MUST** enforce configurable limits on custom metadata: maximum number of key-value pairs per file, maximum
key name length, maximum value length, and maximum total custom metadata size per file. Metadata operations exceeding
limits **MUST** be rejected.

**Rationale**: Without limits, custom metadata can be abused for general-purpose data storage, inflating metadata
storage costs and degrading query performance.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

### 5.6 File Retention & Lifecycle

#### Indefinite Retention

- [x] `p1` - **ID**: `cpt-cf-file-storage-fr-retention-indefinite`

In phase 1, files **MUST** be retained indefinitely until explicitly deleted by an authorized actor
(`cpt-cf-file-storage-fr-authorization`). The system **MUST NOT** automatically delete or expire file content based on
age or inactivity.

**Rationale**: In the absence of tenant-level retention policies (phase 2), indefinite retention is the safest default —
it prevents accidental data loss and gives consuming gears predictable storage semantics.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

#### Retention Policies

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-retention-policies`

**Status — not enforced yet:** rules can be stored, but the gear runs no background worker, so expiry is not enforced until a separate cleanup job exists.

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

**Not started**: no implementation in this release. There is no EventBroker owner-deletion consumer and no
Serverless Runtime client anywhere in this gear's code; the requirement remains a planned P2 item (see DESIGN.md's
`serverless-adapter` component).
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

#### Orphan Reconciliation

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-orphan-reconciliation`

**Status — not enforced yet:** the gear runs no background worker, so abandoned `pending` versions, expired multipart sessions and expired idempotency keys are not reconciled until a separate cleanup job exists.

The system **MUST** automatically detect and reconcile orphan state between the metadata store and storage backends.
Because content is uploaded to the sidecar and the version is only later **bound** in the metadata DB (the two writes
are not atomic), several edge cases produce orphans:

- A version was **pre-registered** (`status = pending`) and the bytes uploaded to the sidecar, but the **bind** never
  happened — the client abandoned the upload, or dropped after a failed-precondition response without retrying. The `pending` version row
  and its backend object are left dangling
- A backend object was written (`/{file_id}/{version_id}`) but no version row exists for it (the pre-register itself
  was lost)
- A **non-current** version superseded by a later bind and past the retention rule (`cpt-cf-file-storage-fr-retention-policies`)
- *(P2)* A multipart upload session was initiated but neither `complete` nor `abort` was invoked, leaving a `pending`
  version row and uploaded parts hanging

After a configurable grace period, the system **MUST** reconcile version rows against actual backend object existence
and apply the following dispositions:

- `pending` version past the grace window (never bound) → delete the version row **and** its backend object
- `available` version with **no** matching backend object → flag for operator attention (do **NOT** auto-delete; most
  likely backend data loss requiring manual review)
- Backend object with no matching version row → delete at the backend (orphaned content; no metadata path resolves it)
- Non-current version beyond the retention rule → delete the version row **and** its backend object
  (`cpt-cf-file-storage-fr-retention-policies`)
- *(P2)* Multipart session past the grace window with no `complete` → aborted at the backend
  (`abortMultipartUpload`), uploaded parts discarded, the corresponding `pending` version row removed

Reconciliation **MUST** be a control-plane internal scheduled task — it **MUST NOT** be triggerable from any public
API surface, it issues backend deletes via the sidecar — and **MUST** emit audit records
(`cpt-cf-file-storage-fr-audit-trail`) for every disposition it performs. This engine is unified with version
retention (`cpt-cf-file-storage-fr-retention-policies`): both prune by deleting a version row plus its backend object.

In **P1 there is no cleanup engine** — `pending`/non-current versions and orphan blobs accumulate (the indefinite-retention
P1 default, `cpt-cf-file-storage-fr-retention-indefinite`); acceptable at initial-release volumes.

**Rationale**: Upload-then-bind across two stores inevitably produces divergence on failure, and it accumulates as
`pending` rows pointing at blobs no consumer reached, or blobs with no row. Reconciliation keeps the two stores
converged. Auto-deletion is safe for orphan content (no metadata points to it) and for stale `pending` versions (the
bind never finished, so no consumer depends on them). The diverged-`available` case is the only one needing manual
handling, because it implies backend data loss that auto-deletion would mask.
**Actors**: `cpt-cf-file-storage-actor-cf-gears`

#### File Versioning

- [x] `p1` - **ID**: `cpt-cf-file-storage-fr-file-versioning`

Versioning is a **FileStorage-level** feature and **MUST NOT** depend on a backend versioning capability: each version
is a **distinct immutable backend object** at `/{file_id}/{version_id}` and the file's live content is the
`content_id` pointer (`cpt-cf-file-storage-fr-upload-file`). It therefore works identically on every backend (S3, NAS,
FTP, …). The system **MUST**:

- Assign a FileStorage-owned `version_id` (a uuid) on every content write and create the corresponding immutable
  object; `version_id`s are treated as opaque uuids (never parsed for ordering — use creation time for order)
- Retrieve a specific version's content and metadata by `version_id`
- List all versions of a file (current and non-current) with each version's `version_id`, size, hash, creation
  timestamp, and whether it is the current version
- **Restore** a prior version by **re-binding** `content_id` to that version's `version_id` — a pointer swap, no
  re-upload (`cpt-cf-file-storage-fr-conditional-requests`). Restore **MUST** require the same authorization as a
  content write
- Permanently delete a specific version by `version_id` (removing its row and backend object); deleting the only
  remaining version is equivalent to deleting the file (`cpt-cf-file-storage-fr-delete-file`)

In P1 there is **no automatic version cleanup** — versions are retained indefinitely (the P1 default, see
`cpt-cf-file-storage-fr-retention-indefinite`). Automatic pruning by a version-retention policy (keep ≤ X versions
and/or younger than T) is a P2 concern, unified with orphan reconciliation in the P2 cleanup engine
(`cpt-cf-file-storage-fr-retention-policies`, `cpt-cf-file-storage-fr-orphan-reconciliation`).

The system **MUST** apply the same authorization, tenant boundary enforcement, and audit requirements to all versioned
operations as to current-version operations.

**Rationale**: Modelling each version as its own immutable object plus a pointer makes versioning a property of
FileStorage rather than of the backend, so it is uniform across heterogeneous backends, makes "replace content" and
"restore" cheap pointer swaps, and guarantees content is never mutated in place. Deferring automatic cleanup to P2
keeps P1 simple (indefinite retention is the safe default); accumulation is acceptable at initial-release volumes.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

#### Backend Migration

- [x] `p2` - **ID**: `cpt-cf-file-storage-fr-backend-migration`

The system **MUST** be able to relocate a file's content from one storage backend to another **without changing the
file's `/files/{id}` URL or its identity**. Migration **MUST**:

- preserve the `file_id`, ownership, custom metadata, content hash, and externally observable behaviour of the file;
- be authorized as an administrative/owner operation and emit audit records (`cpt-cf-file-storage-fr-audit-trail`) per
  migrated file;
- update the file's `backend_id`/`backend_path` only after the destination object is durably written and verified
  (hash match), then remove the source object best-effort (a failed source cleanup degrades to an orphan handled by
  `cpt-cf-file-storage-fr-orphan-reconciliation`).

In P1 a file's backend is immutable; this requirement lifts that restriction for **non-versioned** files in P2.
Migration of versioned files (which carry a backend-owned version chain) is constrained by the backend's versioning
semantics and is out of scope until a dedicated design addresses version-chain relocation.

**Rationale**: One of the reasons to keep content behind the FileStorage sidecar (ADR-0003) is the ability to move
bytes between backends without rotating URLs. Real drivers include cost-tier optimization (move cold data to a cheaper
tier), backend deprecation/decommissioning, tenant data residency (relocate a tenant's files to an in-region backend),
capacity rebalancing across buckets, and disaster recovery from a degraded backend. Enforcing `backend_id`
immutability at the service layer only (not as a DB constraint) keeps this a behavioural change in P2 with no schema
migration.
**Actors**: `cpt-cf-file-storage-actor-cf-gears`

A content-hash-modes design (multipart offset-manifest composite hashing, computed on-the-fly) is recorded in
[ADR-0006](./ADR/0006-cpt-cf-file-storage-adr-content-hash-modes.md).

#### File Encryption

- [ ] `p3` - **ID**: `cpt-cf-file-storage-fr-file-encryption`

File encryption requires the server-side encryption backend capability (`cpt-cf-file-storage-fr-backend-capabilities`).
When the encryption capability is available for a backend, the system **MUST** support server-side encryption of file
content at rest, configurable per backend and per policy.

**Rationale**: Regulated environments and security-sensitive deployments require encryption at rest to meet compliance
requirements (GDPR, HIPAA, SOC 2) and protect stored data against unauthorized physical or logical access to the
storage backend.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

### 5.7 Audit

#### Audit Trail

- [ ] `p2` - **ID**: `cpt-cf-file-storage-fr-audit-trail`

The system **MUST** produce an audit record for every write operation (upload, content replacement, delete, metadata
update). Audit records **MUST** include the operation type, actor identity, file identifier, timestamp, and outcome
(success or failure).

**Rationale**: Audit trails are required for security forensics, compliance reporting, and operational troubleshooting.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

**Partial:** every audited write inserts an `audit_outbox` row transactionally with the mutation (write side
implemented and tested); there is no drain/relay to a downstream audit sink and no query API on this gear's own REST
surface — see [features/audit-trail.md](./features/audit-trail.md).

#### Read Audit Logging

- [ ] `p3` - **ID**: `cpt-cf-file-storage-fr-read-audit`

The system **MUST** support optional audit logging for read operations (downloads and metadata queries), configurable
per policy. When enabled by policy, the system **MUST** produce an audit record for every read operation. Because all
content traffic transits the sidecar, read audit applies uniformly to every download — there are no per-flow
carve-outs.

**Rationale**: Regulated environments and security-sensitive owners require visibility into who accessed their files and
when. Making read audit optional per policy avoids the performance and storage overhead of logging every read
across the platform, while enabling it where compliance demands it.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

### 5.8 Pluggable Storage Backends

#### Backend Abstraction

- [x] `p1` - **ID**: `cpt-cf-file-storage-fr-backend-abstraction`

The system **MUST** abstract the storage layer behind a common interface, enabling support for multiple backend types (
S3, GCS, Azure Blob, NFS, FTP, SMB, WebDAV, local filesystem).

**Rationale**: Different deployments and tenants have different storage infrastructure; a common interface allows
backend selection without changing the gear's core logic.
**Actors**: `cpt-cf-file-storage-actor-cf-gears`

#### Backend Capabilities

- [x] `p1` - **ID**: `cpt-cf-file-storage-fr-backend-capabilities`

The system **MUST** define a capability model for storage backends. Each backend **MUST** declare which optional
capabilities it supports. The system **MUST** support at least the following client-facing capabilities:

- **Multipart Upload** — the backend natively supports chunked upload with independent part transfers and server-side
  assembly
- **Server-Side Encryption** — the backend can encrypt file content at rest using backend-managed or customer-provided
  keys

Note: versioning is **not** a backend capability — it is implemented at the FileStorage level (distinct objects per
version + a pointer) and works on every backend (`cpt-cf-file-storage-fr-file-versioning`).

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
**Actors**: `cpt-cf-file-storage-actor-cf-gears`

#### Backend Configuration Source

- [x] `p1` - **ID**: `cpt-cf-file-storage-fr-backend-config-source`

In P1, storage backend configurations (`type`, `endpoint`, `credentials`, `capabilities`, `hash_policy`) **MUST** be
loaded at gear startup from the gear's own section of the platform YAML configuration — there is no standalone
TOML/JSON configuration file of its own. Adding, removing, or re-configuring a backend
requires a gear restart. The configured set is exposed for read-only runtime introspection.

**Rationale**: Loading backend configuration from the gear's own platform-YAML section is the simplest viable
mechanism for P1 — no DB or admin-UI dependency, and no separate configuration file to keep in sync with the rest of
the gear's config. Read-only HTTP introspection is sufficient for clients to discover available backends and their
capabilities without granting any runtime mutation surface.
**Actors**: `cpt-cf-file-storage-actor-cf-gears`

#### Runtime Backend Configuration

- [ ] `p3` - **ID**: `cpt-cf-file-storage-fr-runtime-backends`

The system **MUST** allow tenants to connect and configure storage backends at runtime without requiring service
rebuild or redeployment. Runtime backend configurations **MUST** be persisted in the metadata database (replacing the
P1 platform-YAML source) and propagated to running gear instances.

**Rationale**: Enterprise tenants need to bring their own storage (BYOS) and switch backends based on cost, compliance,
or geographic requirements.
**Actors**: `cpt-cf-file-storage-actor-platform-user`

### 5.9 Access Interfaces

#### Control-Plane REST API

- [x] `p1` - **ID**: `cpt-cf-file-storage-fr-rest-api`

The system **MUST** expose a control-plane REST API under a single auth-required namespace (`/api/file-storage/v1`)
for metadata management, listing, backend discovery, version bind, and the issuance of signed content URLs
(`cpt-cf-file-storage-fr-signed-urls`). This surface **MUST NOT** accept or return file content — content is moved
exclusively by the sidecar via signed URLs. FileStorage P1 has no anonymous namespace — see `§5.3`.

**Rationale**: REST is the standard control interface for Gears and platform UI; keeping content off this surface is
what allows the data plane (sidecar) to scale independently (ADR-0003).
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

#### Signed Content URLs

- [x] `p1` - **ID**: `cpt-cf-file-storage-fr-signed-urls`

The control plane **MUST** issue short-lived **signed URLs** that authorize a single content operation
(`GET`/`PUT`/part) against the **sidecar** for a specific object. Signed URLs **MUST**:

- be a **stateless, opaque, asymmetric Ed25519-signed token** (the bespoke codec-equivalent format per ADR-0004's
  Implementation note — `base64url(json).base64url(ed25519_signature)`, not a literal PASETO library), verifiable by the
  sidecar without a database lookup, for which the control plane holds the private key (**sole minter**) and the sidecar
  holds only the public key;
- be carried either in the `fs-token` URL query parameter (`?fs-token=<token>`, for bare embeddable URLs) or in the
  `X-FS-Token` request header (for programmatic/batch) — the **same token**, chosen by access intent; it is **never**
  carried in `Authorization`, which always carries the standard platform JWT;
- have a format known **only** to the control plane and the sidecar; every other participant (browser, CDN, proxy,
  app, logs) **MUST** treat the token as opaque bytes and **MUST NOT** parse it — the claim-set and crypto may change;
- always point at the sidecar, **never** at a backend-addressable URL (`cpt-cf-file-storage-principle-backend-opacity`);
- bind the **operation** `op` ∈ {GET, PUT, part} into the token (also checked against the HTTP method), so it cannot be
  reused for a different operation;
- carry **AND-combined claims**, of which only the expiry is mandatory:
  - **expiry** (`exp`, required) — and it **MUST NOT** exceed a configured maximum lifetime `max_url_ttl` (recommended
    7 days), enforced by the control plane at signing; the sidecar rejects once `now > exp`;
  - optional client `ip`/CIDR;
  - optional predicates over the caller's auth-token claims (e.g. `typ=user`, `sub=<id>`, `tenant_id=<id>`); when one
    is present the sidecar **MUST** also validate a real platform token and match each claim;
  - on **upload** URLs, optional content constraints the sidecar enforces during the stream: a size bound — either
    `max_size` (≤) **or** `exact_size` (==), mutually exclusive — and an `expected_hash` (`<alg>:<hex>`, `<alg>` from
    the backend allow-list) the uploaded bytes must match;
  - *(P2)* a `max_rate` (bandwidth) and a `max_conns` (concurrent connections) scoped to a single `(file_id, op)`;
- be tamper-evident as a whole — a client **MUST NOT** be able to add, remove, or weaken a constraint without
  invalidating the signature;
- optionally carry a set of response headers the sidecar **MUST** echo verbatim on the served response (e.g.
  `Content-Disposition`, `Content-Type` override, `Cache-Control`), so the sidecar needs no control-plane round-trip.

The control plane signs with a single active keypair at a time (the bespoke format carries no `kid`, in P1 or later);
the sidecar verifies against a small ordered set of public keys — the active one plus, during a rotation window,
previously-active ones (`FS_SIDECAR_PREVIOUS_PUBLIC_KEYS`) — which lets a `signing_key_seed` rotation happen without
an outage or invalidating already-issued signed URLs, with no `kid` needed to select among them (see
`docs/operations.md`'s `signing_key_seed` → Rotation section for the procedure). There is no per-token revocation;
emergency access revocation is the platform auth module's token revocation. Enforcement
of the `max_rate` / `max_conns` constraints is deferred to P2 (it additionally requires coordinating the
multi-instance sidecar fleet on a shared backend).

**Rationale**: Signed URLs let the control plane delegate the byte transfer to the sidecar without exposing backends
and without a per-request control round-trip on the data path. AND-combined constraints give per-link access control
(time-boxed, ip-boxed, principal-boxed) reusing the platform's own token claims.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

#### Random Read Access

- [x] `p1` - **ID**: `cpt-cf-file-storage-fr-range-requests`

The **sidecar** download endpoint **MUST** support random (non-sequential) read access to arbitrary byte ranges of
stored content so that consumers can seek through large files efficiently — most importantly, so that media players
can scrub through videos and audio without re-downloading the file. Because the `Range` header is **not** part of the
signed-URL signature, a **single** signed download URL serves **many** range requests at different offsets until it
expires (random access without re-presigning).

**Rationale**: Without random read access, every seek in a video forces a full re-download from byte 0, which is
unusable for any clip longer than a few seconds. The protocol-level mechanics (HTTP `Range`/`Content-Range` semantics,
`Accept-Ranges` advertisement, backend-level range translation) are documented in DESIGN.md.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

### 5.10 Cache & Idempotency

#### Conditional Requests

- [ ] `p1` - **ID**: `cpt-cf-file-storage-fr-conditional-requests`

The system **MUST** support conditional HTTP requests (RFC 7232) across both planes — on the sidecar for downloads,
and on the control plane for metadata reads, version bind, content-replacement, metadata updates, and deletes. The
system **MUST**:

- Return an `ETag` header on every download (sidecar) and metadata (control) response. ETag is opaque, derived from
  `(file_id, content_id)`, and **MUST NOT** equal the content hash (which is exposed separately). Because `content_id`
  is the current version pointer, the ETag changes exactly when content is (re)bound
- Support `If-None-Match` on download/metadata reads — return `304 Not Modified` when the ETag matches
- Support `If-Match` on reads — return `400 failed_precondition` when the ETag does not match
- Require `If-Match` on `DELETE` and on every content **bind** that rebinds already-bound content (the optimistic CAS
  that swaps `content_id`; it may be omitted only on the first bind of a file that has no content yet) —
  `400 failed_precondition` on mismatch, or when a required `If-Match` is missing. The retry re-binds the
  already-uploaded `version_id` without re-upload.
  The bind may also execute **inside** the upload itself (`bind: "auto"`, the
  default on `POST /files`) — the CAS requirement is unchanged, only the transport differs: multipart `complete`
  reuses its own `If-Match` (absent → the first-content `content_id IS NULL` case) as the embedded bind's
  precondition, and the single-part finalize binds strictly under `content_id IS NULL` (first content of a new file
  only). A lost CAS never fails the upload: it is reported (`bind_state: "conflict"` / `X-FS-Bound: conflict` with
  the current ETag) and resolved by the same manual re-bind, still with no re-upload

**ETag is content-only.** Metadata-only updates bump `meta_version` and `last_modified_at` but **MUST NOT** change the
ETag or content hash — both remain tied to the content. Consequently `If-Match` on a metadata-only update protects
against concurrent **content** writes but does **not** detect concurrent metadata writes. To give callers lost-update
protection for metadata without coupling it to the content ETag, the system **MUST** support an optional
metadata-revision precondition on metadata-only updates (matched against `meta_version`, returning
`400 failed_precondition` on mismatch);
when the caller omits it, metadata updates remain last-write-wins (S3-style) for back-compatibility. See DESIGN
`cpt-cf-file-storage-principle-content-only-etag`.

**Rationale**: Conditional downloads eliminate redundant bandwidth for unchanged files and enable downstream caching by
browsers and reverse proxies. Conditional updates prevent silent data loss when multiple clients modify file metadata
concurrently. Both follow standard HTTP semantics (RFC 7232) understood by all HTTP clients. Since FileStorage manages
file metadata for all backends, ETags are a FileStorage-level feature independent of backend capabilities.
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

**Partial:** the control plane implements `If-None-Match`/`If-Match` on metadata reads, requires `If-Match` on `DELETE`
and on a bind that rebinds already-bound content (optional on a file's first bind), and supports the
`If-Match-Metadata` revision precondition. The **sidecar** implements `Range`
(`cpt-cf-file-storage-fr-range-requests`) but not `If-None-Match` → `304` on content download — a deliberate,
documented-but-not-yet-implemented gap, since every download token is already scoped to one
`(file_id, version_id)` and a short expiry, making the bandwidth win of a conditional download small.

#### Upload Idempotency

- [x] `p2` - **ID**: `cpt-cf-file-storage-fr-upload-idempotency`

The system **MUST** support idempotent uploads. A client **MUST** be able to provide a unique idempotency key with an
upload request. If a subsequent upload request arrives with the same idempotency key while the original upload's
target version is still `pending`, the system **MUST** return the original result — the same `file_id`/`version_id`,
with a freshly re-minted upload token authorizing that same still-open version — instead of creating a duplicate
file. Once that target version is no longer `pending` (the original upload already completed), the system **MUST**
reject the replay with `409 Conflict` instead of re-minting an upload token against content that has already been
written. The `409` carries no `file_id`; a client that lost the original response finds the file by listing its
own files (`GET /files`). Idempotency keys **MUST** expire after a
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
**Actors**: `cpt-cf-file-storage-actor-platform-user`, `cpt-cf-file-storage-actor-cf-gears`

## 6. Non-Functional Requirements

### 6.1 Gear-Specific NFRs

#### Metadata Query Latency

- [ ] `p1` - **ID**: `cpt-cf-file-storage-nfr-metadata-latency`

File metadata reads and listings **MUST** complete within 300 ms at p95, measured single-threaded on one CPU core with
2 million files stored in total; synchronous control-plane mutations (create, presign, finalize, bind, multipart
complete) **MUST** complete within 2 s at p95.

**Threshold**: reads and listings < 300 ms p95 (single thread, one core, 2 million files in total); synchronous
mutations < 2 s p95
**Rationale**: Metadata queries are used for pre-fetch validation in latency-sensitive paths (e.g., a gear checks file
size before processing).
**Architecture Allocation**: See DESIGN.md § NFR Allocation for how this is realized
**Verification Method**: Load benchmark against PostgreSQL with 2 million files stored, single-threaded on one CPU core; p95 of read and listing requests and of synchronous mutations, taken from the per-route request-latency signal.

#### Content Transfer Latency

- [ ] `p1` - **ID**: `cpt-cf-file-storage-nfr-transfer-latency`

Content download latency **MUST** have no fixed overhead exceeding 2 s at p95; total transfer time is proportional to
file size.

**Threshold**: < 2 s + transfer time p95
**Rationale**: The sidecar serves content synchronously in the request paths of consuming gears; excessive fixed
overhead compounds across requests with multiple files. (Allocated to the sidecar, per
ADR-0003.)
**Architecture Allocation**: See DESIGN.md § NFR Allocation for how this is realized
**Verification Method**: Sidecar download benchmark measuring time to first byte; streaming download tests over a real TCP connection.

#### URL Availability

- [ ] `p1` - **ID**: `cpt-cf-file-storage-nfr-url-availability`

Stored file URLs **MUST** remain accessible for the duration of the file's retention with availability matching
the platform SLA.

**Threshold**: URL availability matches platform SLA for the duration of the retention period
**Rationale**: Consumers depend on URL stability — broken URLs disrupt downstream workflows and user experience.
**Architecture Allocation**: See DESIGN.md § NFR Allocation for how this is realized
**Verification Method**: End-to-end lifecycle suite (upload, then download through a freshly issued signed URL) against local-filesystem and S3-compatible backends.

#### Audit Completeness

- [ ] `p2` - **ID**: `cpt-cf-file-storage-nfr-audit-completeness`

Audit records **MUST** be emitted for 100% of write operations with no silent drops under normal operating conditions.

**Threshold**: 100% audit coverage for write operations
**Rationale**: Incomplete audit trails undermine compliance and forensic investigations.
**Architecture Allocation**: See DESIGN.md § NFR Allocation for how this is realized
**Verification Method**: Integration tests asserting that every audited write inserts its audit row in the same transaction, including the rollback case.

#### Data Durability and Recovery

- [ ] `p1` - **ID**: `cpt-cf-file-storage-nfr-durability`

An acknowledged write **MUST NOT** be lost or left partially applied by the service itself — across restarts,
instance failures and client retries — as long as the metadata database and the storage backend retain their data.
The FileStorage service (control plane and sidecar) **MUST** be restorable within 15 minutes once its database and
storage backend are available.

Recovery from loss of, or damage to, the metadata database or the storage backend is outside this gear: the achievable
RPO and the recovery time of those stores are set by the platform's backup and replication policy for them (a single
data centre and region in P0), configured independently of FileStorage, and this gear adds no loss beyond them.
Backend durability (e.g. S3's) is likewise inherited from the backend.

**Threshold**: zero service-induced loss of acknowledged writes; service RTO ≤ 15 minutes once the database and storage
backend are available; database/storage RPO and RTO inherited from the platform backup policy
**Rationale**: File loss after a successful upload acknowledgment breaks consumer trust and disrupts downstream
workflows. The service guarantees acknowledgment implies durability in its own stores; how much can be lost when a
store itself is lost depends on that store's backups, which the platform — not this gear — configures.
**Architecture Allocation**: See DESIGN.md § NFR Allocation for how this is realized
**Verification Method**: PostgreSQL concurrency and failure-injection tests (lost finalize, idempotent replay, cleanup races, backend faults); service restart covered by the end-to-end suite; database and storage recovery verified by the platform backup/restore procedure.

#### Scalability & Capacity

- [ ] `p1` - **ID**: `cpt-cf-file-storage-nfr-scalability`

FileStorage **MUST** support horizontal scaling to handle concurrent file operations without degradation. The latency
targets of `cpt-cf-file-storage-nfr-metadata-latency` and `cpt-cf-file-storage-nfr-transfer-latency` **MUST** hold with
2 million files stored in total. The system **MUST** scale linearly — adding instances **MUST** proportionally increase
throughput without introducing coordination bottlenecks between instances.

**Threshold**: 2 million files stored in total; linear horizontal scaling
**Rationale**: As platform adoption grows, file operation volume grows proportionally. Without explicit scalability
requirements, the architecture may adopt patterns (global locks, shared mutable state) that prevent horizontal scaling.
**Architecture Allocation**: See DESIGN.md § NFR Allocation for how this is realized
**Verification Method**: The load benchmark of `cpt-cf-file-storage-nfr-metadata-latency` at 2 million files; horizontal scaling holds because both planes are stateless per request (no in-process shared state between instances).

#### Bandwidth & Egress

- [ ] `p1` - **ID**: `cpt-cf-file-storage-nfr-bandwidth`

Because every uploaded and downloaded byte transits the **sidecar** (per
ADR-0003 — backends are never addressed directly by clients),
**bandwidth, not CPU or memory, is the binding capacity constraint of the sidecar** (the control plane carries no
content and is not bandwidth-bound). Each sidecar instance **MUST** sustain a defined combined ingress+egress budget,
and aggregate transfer capacity **MUST** scale horizontally by adding stateless sidecar instances, independently of
the control plane. Repeat-read egress **MUST** be offloadable to an upstream caching layer (API-Gateway / CDN) using
the conditional-request headers the sidecar emits (`ETag`, `Cache-Control`, `Vary`), so that cache hits do not
re-transit the sidecar.

**Threshold**: ≥ 2.5 GiB/s combined ingress+egress per sidecar instance (≈ 25 GbE class); aggregate capacity =
`ceil(peak aggregate transfer rate / per-instance budget)` sidecar instances; conditional re-reads served from
CDN/proxy cache without sidecar egress
**Rationale**: ADR-0003 confines the terabyte-scale traffic to the sidecar. If the
NFR set only constrains CPU/memory (the scalability NFR), implementers may size and scale against the wrong dimension
and under-provision network capacity. Making the bandwidth budget explicit, allocating it to the sidecar, and making
download caching a first-class offload path keeps the data plane affordable at scale.
**Architecture Allocation**: See DESIGN.md § NFR Allocation for how this is realized
**Verification Method**: Per-instance sidecar throughput measurement (capacity test) when sizing a deployment.

### 6.2 NFR Exclusions

All project-default NFRs apply to this gear. Cost (total cost of ownership) is not modelled per gear in this
repository; §6.5 records how this gear bounds its own cost drivers instead.

### 6.3 Applicability Notes

The following NFR categories from the platform checklist are **not applicable** to this gear:

| Category                 | Rationale                                                                                                                                                                                                                                                                                               |
|--------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| **Safety**               | FileStorage is a data storage service with no physical actuators, safety-critical control loops, or human safety implications.                                                                                                                                                                          |
| **UX**                   | FileStorage is a backend service consumed via SDK and APIs. It has no user-facing UI; UX concerns are the responsibility of consuming gears and platform UI.                                                                                                                                          |
| **Internationalization** | FileStorage stores and returns opaque binary content and metadata strings. It does not render, translate, or localize content. File names and metadata values are preserved as-is.                                                                                                                      |
| **Privacy by Design**    | FileStorage treats all files as opaque blobs and does not inspect, index, or process file content. Privacy controls (data minimization, consent, right to erasure) are enforced at the platform and consuming-gear level. Tenant isolation and access control are covered by functional requirements. |
| **Compliance**           | FileStorage does not implement domain-specific compliance logic (GDPR, HIPAA, SOX). It provides the building blocks (audit trail, tenant isolation, retention policies, encryption) that enable consuming gears and platform operators to achieve compliance.                                         |
| **Operations**           | Operational concerns (deployment, monitoring, alerting, runbooks) follow platform-wide standards and are not gear-specific.                                                                                                                                                                           |
| **Maintainability**      | Maintainability follows platform-wide coding standards, testing requirements, and CI/CD practices. No gear-specific maintainability NFRs beyond the platform baseline.                                                                                                                                |

### 6.4 Five Quality Vectors Analysis

File Storage is assessed against the five vectors of the
[Constructor Gears Quality Framework](https://github.com/constructorfabric/vision/blob/main/CONSTRUCTOR_GEARS_QUALITY_FRAMEWORK.md),
in the framework's priority order. North Star metrics are tracked at the platform level; the show-stoppers below bind
this gear only through the requirements they reference. The priority order applies only when choosing between options
that already satisfy every **MUST** in this PRD; it **MUST NOT** be used to weaken one.

| **Quality Vector** | **Show-Stopper Requirements** | **Rationale** |
|--------------------|-------------------------------|---------------|
| **Efficiency** | Content bytes **MUST NOT** transit the control plane; uploads and downloads **MUST** stream without buffering a whole object (`cpt-cf-file-storage-nfr-bandwidth`). | Keeps the control plane small and lets byte-path capacity scale independently. |
| **Reliability** | An acknowledged write **MUST NOT** be lost or partially applied by the service while the database and storage backend are intact, and the service **MUST** be restorable within 15 minutes once they are available (`cpt-cf-file-storage-nfr-durability`); retried uploads and completions **MUST** be idempotent (`cpt-cf-file-storage-fr-upload-idempotency`, `cpt-cf-file-storage-fr-multipart-complete-lease`); every audited write **MUST** be recorded with it (`cpt-cf-file-storage-nfr-audit-completeness`). Database and storage RPO/RTO are inherited from the platform backup policy. | Consumers retry on transient failures; a retry must never duplicate, lose or silently diverge from a write. |
| **Performance** | Metadata reads and listings **MUST** meet p95 < 300 ms single-threaded on one CPU core, and synchronous mutations p95 < 2 s (`cpt-cf-file-storage-nfr-metadata-latency`); content-transfer fixed overhead **MUST** stay under p95 < 2 s (`cpt-cf-file-storage-nfr-transfer-latency`); both with 2 million files stored in total (`cpt-cf-file-storage-nfr-scalability`). | Gears place file access on their own request paths. |
| **Security** | Every operation **MUST** be authorized within the caller's tenant (`cpt-cf-file-storage-fr-authorization`, `cpt-cf-file-storage-fr-tenant-boundary`); content **MUST** be reachable only through control-plane-issued signed URLs (`cpt-cf-file-storage-fr-signed-urls`). | Multi-tenant storage: cross-tenant exposure is a critical finding. |
| **Versatility** | Storage backends **MUST** be selectable by configuration, without a rebuild (`cpt-cf-file-storage-fr-backend-abstraction`). | One service serves deployments with different storage infrastructure. |

### 6.5 Quality Framework Conformance

This section answers every element of the
[Constructor Gears Quality Framework](https://github.com/constructorfabric/vision/blob/main/CONSTRUCTOR_GEARS_QUALITY_FRAMEWORK.md)
for this gear: each vector's guiding question, its North Star metric, and each of its example metrics. Each metric
carries one position:

- **Committed** — a requirement of this PRD, with its ID.
- **Observed** — measured by a signal this gear emits (DESIGN.md §4.4), with no gear-specific target.
- **Inherited** — owned by the platform (delivery process, CI gates, SLA, backup policy); this gear adds no target.
- **Not applicable** — with the reason.

The priority order and the trade-off rule are those of §6.4.

#### Efficiency

- **Guiding question** — *How quickly and economically can software be built, deployed, and operated?* Content moves
  between clients, the sidecar and the backend without crossing the control plane; a default upload takes two client
  requests (multipart: N + 2); Gears call an in-process SDK; storage backends are chosen by configuration.
- **North Star — Total Cost of Ownership (TCO)**: Inherited — cost is modelled at the platform level (§6.2). The gear
  bounds its own cost drivers: bytes transit only the sidecar (`cpt-cf-file-storage-nfr-bandwidth`), memory per
  transfer is bounded by streaming, and cleanup (once a cleanup job exists) is bounded by a time budget.

| Framework metric | Position | Reference |
|---|---|---|
| Time from approved PRD to production | Inherited — platform delivery process | — |
| TCO to build and operate a feature, Gear, or product | Inherited — platform cost model; gear cost drivers bounded as above | `cpt-cf-file-storage-nfr-bandwidth` |
| Lead time for change | Inherited — platform CI/CD; schema changes ship as one additive migration per change with a documented upgrade and rollback path | `operations.md` |
| Cost per delivered feature | Inherited — platform delivery metrics | — |
| Infrastructure cost per transaction/workflow | Observed — bytes moved per workflow; control-plane work per upload is a fixed number of metadata calls | ingress/egress byte signals; `cpt-cf-file-storage-fr-auto-bind` |
| Infrastructure cost per tenant/service | Egress bytes — Observed (`record_egress_bytes`); storage usage per owner and tenant — Not observed yet (Usage Collector integration is not wired; the usage reporter is not configured in any deployment) | `cpt-cf-file-storage-fr-usage-reporting`, `cpt-cf-file-storage-contract-usage-collector` |

#### Reliability

- **Guiding question** — *How dependable is it?* An acknowledged write is never lost or partially applied by the
  service; retries are idempotent; transient backend failures are reported as retryable; cleanup reconciles orphans.
- **North Star — Service availability (SLA)**: Inherited — the platform SLA (`cpt-cf-file-storage-nfr-url-availability`).
  The gear commits zero service-induced loss of acknowledged writes and a service RTO of 15 minutes
  (`cpt-cf-file-storage-nfr-durability`).

| Framework metric | Position | Reference |
|---|---|---|
| MTTR | Committed for the service — restorable within 15 minutes once its database and storage are available; database/storage recovery inherited | `cpt-cf-file-storage-nfr-durability` |
| MTBF | Inherited — platform monitoring | — |
| Failed workflow rate | Observed — per-operation success/failure signal | `cpt-cf-file-storage-fr-upload-idempotency`, `cpt-cf-file-storage-fr-multipart-complete-lease` |
| Change failure rate | Inherited — platform CD; additive migration, mixed-version window and rollback documented | `operations.md` |
| Successful deployment rate | Inherited — platform CD; startup rejects invalid configuration before serving | `operations.md` |
| Error rate | Observed — per-route status and per-backend error signals; transient faults distinguished from permanent ones | `cpt-cf-file-storage-fr-rest-api` |
| Disaster recovery success rate | Inherited — platform backup and restore of the database and the storage backend | `cpt-cf-file-storage-nfr-durability` |

#### Performance

- **Guiding question** — *How fast does it execute?* Bytes stream end to end without buffering; metadata reads and
  listings use keyset pagination whose cost does not grow with page depth.
- **North Star — P99 workflow latency**: Observed — tracked at the platform level from the per-route latency signal;
  the gear commits p95 targets (`cpt-cf-file-storage-nfr-metadata-latency`, `cpt-cf-file-storage-nfr-transfer-latency`).

| Framework metric | Position | Reference |
|---|---|---|
| Transactions/workflows per second | Committed as scaling behaviour — linear horizontal scaling; per-sidecar bandwidth budget | `cpt-cf-file-storage-nfr-scalability`, `cpt-cf-file-storage-nfr-bandwidth` |
| Average response time | Observed — per-route latency signal; the commitment is p95 | `cpt-cf-file-storage-nfr-metadata-latency` |
| P99/P999 latency | Observed — per-route latency signal; the commitment is p95 | `cpt-cf-file-storage-nfr-metadata-latency`, `cpt-cf-file-storage-nfr-transfer-latency` |
| Resource utilization (CPU/Memory) per transaction | Committed for memory — a transfer never buffers a whole object; CPU inherited | §6.4 Efficiency, `cpt-cf-file-storage-nfr-bandwidth` |
| % of performance SLAs met | Committed — verified by the load benchmark of the latency NFRs | `cpt-cf-file-storage-nfr-metadata-latency` (Verification Method) |
| Cold start time | Not applicable — a long-running service; process start is not on any request path and is bounded by the service RTO | `cpt-cf-file-storage-nfr-durability` |

#### Security

- **Guiding question** — *How well is it protected?* Every operation is authorized within the caller's tenant;
  content is reachable only through control-plane-issued signed URLs; secrets never appear in logs.
- **North Star — Critical security findings in production (target 0)**: Inherited target of 0 — enforced through the
  platform security gates below and the gear's show-stoppers in §6.4.

| Framework metric | Position | Reference |
|---|---|---|
| Security policy compliance | Committed — tenant-scoped authorization on every operation; cross-owner actions require elevated scope | `cpt-cf-file-storage-fr-authorization`, `cpt-cf-file-storage-fr-tenant-boundary` |
| Secrets management coverage | Committed — signing keys and the internal callback token are held as secrets and never logged; signing keys rotate without downtime | `cpt-cf-file-storage-fr-signed-urls`, `cpt-cf-file-storage-fr-callback-internal-token` |
| Mean time to remediate vulnerabilities | Inherited — platform vulnerability process; dependency advisories gated in CI | — |
| Dependency compliance | Inherited — CI gates on dependency licences, advisories and bans (`cargo-deny`) and FIPS verification | — |
| Secure coding compliance | Inherited — CI gates: architecture lints, `clippy -D warnings`, CodeQL, fuzzing | — |
| Security incident rate | Inherited — platform incident process; the audit trail supports forensics | `cpt-cf-file-storage-fr-audit-trail` |

#### Versatility

- **Guiding question** — *How many real-world scenarios can it support without building a new platform?* One service
  serves every Gear and user that stores files, across storage backends, through REST, an in-process SDK and signed
  URLs.
- **North Star — % of target business scenarios supported out of the box**: Committed — all six target scenarios of
  §8 are supported (100%).

| Framework metric | Position | Reference |
|---|---|---|
| Supported business scenarios | Committed — Upload a File; Fetch File for Gear Processing; Validate File Metadata Before Processing; Delete a File; Multi-Backend Deployment; Configure Policy | §8 |
| Supported deployment models | Committed — S3-compatible object storage, local filesystem, in-memory (development and tests); single data centre and region in P0 | `cpt-cf-file-storage-fr-backend-abstraction`, `cpt-cf-file-storage-fr-backend-capabilities` |
| Supported integration types | Committed — REST control plane, sidecar data plane over signed URLs with `Range`, in-process SDK; authorization, usage, quota, event and serverless contracts | `cpt-cf-file-storage-interface-rest-api`, `cpt-cf-file-storage-interface-sidecar-api`, `cpt-cf-file-storage-interface-sdk-trait`, §7.2 |
| Supported business domains | Committed — domain-agnostic: files are opaque content with typed metadata, usable by any Gear | `cpt-cf-file-storage-fr-file-type-classification` |
| Configuration vs. customization ratio | Committed — backends, type and size policies and retention rules are configured, not coded; object placement is a plugin extension point (ADR-0007) | `cpt-cf-file-storage-fr-backend-config-source`, `cpt-cf-file-storage-fr-allowed-types-policy`, `cpt-cf-file-storage-fr-retention-policies` |
| Feature coverage across target scenarios | Committed — the target scenarios are specified as use cases with acceptance criteria, and requirements are traced to them | §8, §9, §14 |

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### FileStorage SDK Trait

- [ ] `p1` - **ID**: `cpt-cf-file-storage-interface-sdk-trait`

**Partial:** every control-plane operation is implemented in-process (create/get/list/update/delete files and
versions, bind, multipart upload, ownership transfer, backend migration/discovery, policy, retention rules) —
`FileStorageLocalClient` calls the exact same services the REST handlers call, under the caller's own
`SecurityContext`. Not implemented: the two-step (presign + sidecar transfer) proxied **inside the SDK** as a
seekable read/write — a consuming gear still `PUT`s/`GET`s bytes against the sidecar itself, over the signed URLs
this trait hands back (Level 2, see DESIGN's `sdk-facade`).

**Type**: Rust trait (SDK crate)
**Stability**: unstable
**Description**: Async trait providing create/presign, conditional get, list, metadata update, delete, download-URL
issuance, version listing/presign/bind/delete, multipart upload (initiate/introspect/complete/abort), ownership
transfer, backend migration/discovery, and policy/retention-rule administration — every operation returns domain
models and signed URLs, never file bytes. A future Level 2 addition would perform the two-step (presign + sidecar
transfer) **inside the consumer's process** so a consuming gear sees a normal seekable read/write
(`cpt-cf-file-storage-component-sdk-facade`).
**Breaking Change Policy**: Major version bump required for trait signature changes.

#### Control-Plane REST API

- [x] `p1` - **ID**: `cpt-cf-file-storage-interface-rest-api`

**Type**: REST API (OpenAPI 3.0)
**URL Prefix**: `/api/file-storage/v1`
**Stability**: unstable
**Description**: HTTP REST API for authenticated metadata operations, listing, backend discovery, version bind, and
signed-URL issuance. It does **not** carry file content — content moves over signed URLs against the sidecar
(`cpt-cf-file-storage-fr-signed-urls`). All endpoints require platform JWT — there is no anonymous surface in P1 (see
`§5.3`).
**Breaking Change Policy**: Major version bump required for endpoint removal or incompatible schema changes.

#### Sidecar Data-Plane API

- [x] `p1` - **ID**: `cpt-cf-file-storage-interface-sidecar-api`

**Type**: HTTP (signed-URL authorized)
**Stability**: unstable
**Description**: The sidecar's content surface (`GET`/`PUT`/part), addressed only via control-plane-issued signed
URLs and served from its own domain. Verifies the bespoke codec-equivalent Ed25519 token and its claims per ADR-0004's
Implementation note (base64url-encoded JSON plus a base64url-encoded signature; not a literal PASETO library), validates
the platform token
when a token-claim predicate is present, serves `Range` and conditional requests, and echoes the response headers
baked into the URL. It holds **no** backend/tenant/user policy or quota state — all such limits (storage quota,
allowed types, size policy, retention) live in the control plane and are applied at presign; the sidecar enforces only
the per-URL constraints the signature carries, plus the per-URL connection/rate caps (P2).
**Breaking Change Policy**: Signed-URL format changes are coordinated with the control plane (shared signing contract).

### 7.2 External Integration Contracts

#### Gear Contract

- [x] `p1` - **ID**: `cpt-cf-file-storage-contract-cf-gears`

**Direction**: provided by library (consumed by Gears)
**Protocol/Format**: In-process Rust SDK trait via ClientHub
**Compatibility**: Trait versioned with SDK crate; breaking changes require coordinated release with consuming gears.

#### Authorization Service Contract

- [x] `p1` - **ID**: `cpt-cf-file-storage-contract-authz`

**Direction**: required from external service (Authorization Service)
**Protocol/Format**: Access decision requests for `gts.cf.fstorage.file.type.v1~` resources
**Compatibility**: Contract follows platform authorization protocol; changes require coordinated release.

#### Usage Collector Contract

- [ ] `p2` - **ID**: `cpt-cf-file-storage-contract-usage-collector`

**Direction**: required from external service (Usage Collector)
**Protocol/Format**: Asynchronous per-owner usage reports (storage consumption per owner, including ownership-transfer
debits/credits per `cpt-cf-file-storage-fr-usage-reporting`)
**Compatibility**: Contract follows platform usage reporting protocol; changes require coordinated release.

**Current status**: Not exercised in any deployment — the usage-reporting sink this gear would call is always
unset, so no usage delta is ever sent. See [operations.md](./operations.md)'s "Storage quota (not enforced)"
section, which covers this sibling gap.

#### Quota Enforcement Contract

- [ ] `p2` - **ID**: `cpt-cf-file-storage-contract-quota-enforcement`

**Direction**: required from external service (Quota Enforcement)
**Protocol/Format**: Synchronous per-owner quota check requests before storage-consuming operations
(per `cpt-cf-file-storage-fr-storage-quota`)
**Compatibility**: Contract follows platform quota enforcement protocol; changes require coordinated release.

**Current status**: The Quota Enforcement counterparty does not exist, so this contract is not exercised in any
deployment. `file-storage`'s side is implemented and ready. See [DESIGN.md](./DESIGN.md).

#### EventBroker Contract

- [ ] `p2` - **ID**: `cpt-cf-file-storage-contract-eventbroker`

**Direction**: bidirectional (publishes file events; consumes platform events such as owner deletion)
**Protocol/Format**: Asynchronous event publishing and consumption via EventBroker gear
**Compatibility**: Contract follows platform event protocol; event schema changes require coordinated release.

**Partial:** the publish direction writes a transactional-outbox row (`events_outbox`) per write operation, but no
relay drains it to EventBroker. The consume direction (owner-deletion events) is **not started** — see
`cpt-cf-file-storage-fr-owner-deletion`.

#### Serverless Runtime Contract

- [ ] `p2` - **ID**: `cpt-cf-file-storage-contract-serverless-runtime`

**Direction**: required from external service (Serverless Runtime)
**Protocol/Format**: Workflow invocation for configurable lifecycle operations (e.g., owner deletion disposition)

**Not started**: no implementation in this release — no Serverless Runtime client of any kind exists in this gear's
code; see `cpt-cf-file-storage-fr-owner-deletion`.
**Compatibility**: Contract follows platform Serverless Runtime invocation protocol; changes require coordinated release.

## 8. Use Cases

### Upload a File

- [x] `p1` - **ID**: `cpt-cf-file-storage-usecase-upload`

**Actor**: `cpt-cf-file-storage-actor-platform-user`

**Preconditions**:

- User is authenticated
- Authorization Service grants write access

**Main Flow**:

1. User asks the control plane to upload, supplying metadata (name, mime_type, GTS file type)
2. Control plane validates the GTS file type format and checks authorization for write on
   `gts.cf.fstorage.file.type.v1~` with the file type in resource context
3. *(Phase 2)* Control plane validates against policies (type, size); in phase 1 all uploads are accepted
4. Control plane returns a **signed upload URL** to the sidecar (`cpt-cf-file-storage-fr-signed-urls`)
5. User transfers the bytes to the **sidecar** at that URL; the sidecar streams to the backend object
   `/{file_id}/{version_id}`, computes the hash, and calls the control plane's token-authenticated finalize
   callback, which checks the reported size against the stored object and flips the version to available; for the common
   auto-bind case that same finalize call also **binds** the new version as current under optimistic CAS, in the
   same transaction — the sidecar itself never binds and holds no delegated identity of its own
6. *(Phase 2)* Audit record emitted for the upload
7. The client holds the `file_id` and the bound `version_id`; on a bind conflict (`400 failed_precondition`) it re-binds without
   re-uploading

**Postconditions**:

- File stored with metadata and ownership
- File is readable only by principals authorized via `cpt-cf-file-storage-fr-authorization`
- *(Phase 2)* Audit record emitted for the upload

**Alternative Flows**:

- **Missing or invalid GTS file type**: FileStorage rejects the upload with a validation error
- **Authorization denied**: FileStorage returns access-denied error
- *(Phase 2)* **Policy violation**: FileStorage returns error indicating which policy was violated (type or size)

### Fetch File for Gear Processing

- [x] `p1` - **ID**: `cpt-cf-file-storage-usecase-fetch-media`

**Actor**: `cpt-cf-file-storage-actor-cf-gears`

**Preconditions**:

- File exists at the specified URL

**Main Flow**:

1. Gear asks the control plane for a download URL for the file
2. Control plane checks authorization for read on `gts.cf.fstorage.file.type.v1~` with the file's GTS type in resource
   context and returns a **signed download URL** to the sidecar (pinning the current `content_id`), plus metadata
3. Gear fetches the bytes from the **sidecar** at that URL (with `Range` for partial/seeking reads)
4. Sidecar streams the content from the backend; metadata (mime_type, size, GTS file type) came from step 2

**Postconditions**:

- Content and metadata returned to the requesting gear

**Alternative Flows**:

- **File not found**: FileStorage returns file_not_found error
- **Authorization denied**: FileStorage returns access-denied error

### Validate File Metadata Before Processing

- [x] `p1` - **ID**: `cpt-cf-file-storage-usecase-get-metadata`

**Actor**: `cpt-cf-file-storage-actor-cf-gears`

**Preconditions**:

- File exists at the specified URL

**Main Flow**:

1. Gear calls get_metadata with a file URL
2. FileStorage checks authorization for read on `gts.cf.fstorage.file.type.v1~` with the file's GTS type in resource context
3. FileStorage returns metadata (name, size, mime_type, GTS file type, owner, availability) without transferring content

**Postconditions**:

- Metadata returned; no content transferred

**Alternative Flows**:

- **File not found**: FileStorage returns file_not_found error
- **Authorization denied**: FileStorage returns access-denied error

### Delete a File

- [x] `p1` - **ID**: `cpt-cf-file-storage-usecase-delete-file`

**Actor**: `cpt-cf-file-storage-actor-platform-user`

**Preconditions**:

- User is authenticated
- User owns the file

**Main Flow** (delete the whole file):

1. Owner requests deletion of a file by its identifier
2. Control plane checks authorization for delete on `gts.cf.fstorage.file.type.v1~`
3. Control plane removes the file metadata, ownership records, and **all** version rows (metadata-row-first)
4. Control plane deletes the backend objects best-effort via the sidecar (a failed backend delete degrades to an
   orphan reconciled by the P2 cleanup engine)
5. *(Phase 2)* Control plane emits audit record for the deletion

**Postconditions**:

- Metadata, ownership, and all versions removed; subsequent requests for the file return `404` (idempotent re-delete)
- Backend objects removed (best-effort; residual orphans swept by `cpt-cf-file-storage-fr-orphan-reconciliation`)
- *(Phase 2)* Audit record emitted

**Alternative Flow — delete a single version** (`cpt-cf-file-storage-fr-file-versioning`):

1. Owner requests deletion of a specific version by `file_id` and `version_id`
2. Control plane checks authorization for delete on `gts.cf.fstorage.file.type.v1~`
3. Control plane removes that version's row and backend object
4. *(Phase 2)* Control plane emits audit record for the version deletion

**Postconditions**:

- The specified version is permanently removed; remaining versions unaffected
- Deleting the only remaining version is equivalent to deleting the file (Main Flow postconditions)
- Deleting the **current** version requires the file to have another version to fall back to, or the file is deleted
- *(Phase 2)* Audit record emitted

**Alternative Flows — error cases**:

- **Authorization denied**: FileStorage returns access-denied error
- **File not found**: FileStorage returns file_not_found error
- **Version not found**: FileStorage returns version_not_found error
- **Cross-tenant attempt**: FileStorage returns access-denied error (tenant boundary enforcement)

### Multi-Backend Deployment

- [x] `p1` - **ID**: `cpt-cf-file-storage-usecase-backend-config`

**Actor**: `cpt-cf-file-storage-actor-cf-gears`

**Preconditions**:

- FileStorage is deployed with a configured storage backend

**Main Flow**:

1. Deployment A configures FileStorage with an S3-compatible backend (e.g., AWS S3)
2. Deployment B configures FileStorage with a different backend (e.g., Azure Blob Storage)
3. Both deployments expose identical FileStorage SDK and REST APIs
4. Gears interact with FileStorage through the SDK trait without awareness of the underlying backend
5. Upload, download, delete, metadata, and link operations behave identically regardless of backend

**Postconditions**:

- All functional requirements are met identically across different backend configurations
- Consuming gears require zero code changes when the backend changes

**Alternative Flows**:

- **Backend-specific feature unavailable**: FileStorage returns a clear error indicating the capability is unavailable
  (e.g., multipart upload or versioning request rejected when backend does not declare the capability)

### Configure Policy

- [x] `p2` - **ID**: `cpt-cf-file-storage-usecase-configure-policy`

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

- [x] File upload returns persistent URL and stores metadata (name, size, type, dates, owner)
- [x] File download returns content with correct metadata
- [x] File deletion of a non-versioned file permanently removes content; the metadata row is removed before the
  best-effort backend delete, so a deleted file never leaves a row pointing at missing content, and re-deleting an
  already-deleted file is idempotent (`404`)
- [x] Deleting a file removes all of its versions (metadata-row-first, idempotent); a single version can be deleted by `version_id`
- [x] Authorization checked for every file operation via Authorization Service
- [x] Tenant boundary enforced — cross-tenant access rejected
- [x] Audit record emitted for every write operation
- [x] Policies enforce file type and size restrictions on upload (most restrictive wins across tenant and user levels)
- [x] All content traffic flows through the **sidecar** via signed URLs; no backend-addressable URL is returned to any client
- [x] Content upload and download are each a two-step exchange (control request → signed URL → byte transfer to/from the sidecar); the control REST surface never carries content
- [x] The credential is an opaque, asymmetric Ed25519-signed token (the bespoke codec-equivalent format per ADR-0004's Implementation note — not a literal PASETO library), carried in the query (`?fs-token=`) or a header, stateless, enforcing AND-combined claims (expiry, optional ip, optional token-claim predicates, upload size/hash); altering any claim invalidates the signature; only control+sidecar parse it
- [x] file_not_found error returned for non-existent files
- [x] access_denied error returned for unauthorized operations
- [x] Metadata-only queries complete without transferring file content
- [x] Content is mutable through dedicated content-replacement operations; ETag (content-derived) changes on every
  content write; metadata-only updates do not change ETag or content hash
- [x] Content replacement uploads a new immutable version and **binds** it as current under `If-Match` CAS; a
  conflicting bind returns `400 failed_precondition` and is retried by re-binding the already-uploaded `version_id` without re-uploading
  the bytes; backend content is never mutated in place
- [x] `custom_metadata` is updatable by any actor authorized for the **write** action on the file's GTS type;
  system-managed metadata is not user-updatable
- [x] Custom metadata update changes the file's last modified date
- [x] File ownership (`owner_kind`, `owner_id`) is immutable after creation except through explicit ownership transfer
  or owner deletion workflows; `tenant_id` is never mutable
- [x] Every file has a mandatory GTS file type assigned at upload time; uploads without a file type are rejected
- [x] GTS file type is immutable after creation
- [x] Authorization requests include the file's GTS type, enabling per-type access decisions
- [x] A gear authorized only for type A cannot access files of type B
- [x] FileStorage SDK and REST API behave identically regardless of configured storage backend
- [x] File listing returns metadata only, is paginated, and requires a mandatory owner-kind filter (`user` or `app`)
- [x] Multipart upload assembles parts into a complete file with correct metadata
- [x] Upload rejected when declared mime_type does not match actual file content
- [x] Each backend declares its supported client-facing capabilities (multipart upload, server-side encryption);
  internal-only capabilities are not surfaced on public discovery
- [x] Consumers can discover backend capabilities at runtime
- [x] Operations requiring an unsupported capability return a clear error
- [x] Versioning is FileStorage-level and backend-agnostic: each content write creates a new immutable version at
  `/{file_id}/{version_id}`; metadata-only updates do not create a new version
- [x] All versions of a file are listable with `version_id`, size, hash, timestamp, and current-version flag
- [x] Restore rebinds `content_id` to a prior version (pointer swap, no re-upload), under the same authorization as a
  content write
- [x] In P1 versions are retained indefinitely (no automatic cleanup); P2 prunes via the retention policy +
  reconciliation engine (**Not enforced yet:** no background worker runs it)
- [x] Permanent delete of a specific version removes only that version
- [x] Declared capabilities are independently configurable (enable/disable) per backend
- [x] A capability disabled by configuration behaves identically to an unsupported capability
- [x] Download and metadata responses include `ETag` header derived from `(file_id, content_id)` and not equal
  to the content hash
- [ ] Conditional download with `If-None-Match` returns `304 Not Modified` when file is unchanged
  (**Partial:** implemented on the control-plane metadata `GET`; not implemented on the sidecar's content download)
- [x] `If-Match` is required on `DELETE` and on content **bind** whenever it rebinds already-bound content (it may be omitted only on the first bind of a file that has no content yet); a missing or mismatching `If-Match` returns `400 failed_precondition`
- [x] An optional metadata-revision precondition on metadata-only updates returns `400 failed_precondition` on mismatch, giving
  lost-update protection for concurrent metadata writers; when omitted, metadata updates remain last-write-wins
- [x] An upload whose bind never completes leaves no current pointer to it; the orphan `pending` version and its blob
  are reconciled by the P2 cleanup engine (`cpt-cf-file-storage-fr-orphan-reconciliation`) (**Not enforced yet:** no background worker runs it)
- [x] Retried upload with the same idempotency key returns the original result (same `file_id`/`version_id`, fresh
  upload token) without creating a duplicate file, as long as the target version is still `pending`; once it is no
  longer `pending`, the retry is instead rejected with `409 Conflict` rather than re-minting a token against content
  that already exists
- [x] Retried upload with the same idempotency key by a different owner does not return or create the original owner's
  file
- [ ] Owner deletion event from EventBroker triggers a configurable Serverless Runtime workflow for file disposition
  (**Not started:** no implementation in this release)
- [ ] Files of a deleted owner are retained as orphaned when no workflow is configured (**Not started:** no
  implementation in this release)
- [ ] Server-side encryption is applied when the encryption capability is available and enabled for the backend
- [ ] Upload rejected when storage quota would be exceeded (Quota Enforcement service check) (not enforced in any
  deployment — see `cpt-cf-file-storage-fr-storage-quota`'s Current status)
- [ ] Usage report emitted asynchronously on every storage-consuming write operation; file operations not blocked if
  Usage Collector is unavailable (not sent in any deployment — see `cpt-cf-file-storage-fr-usage-reporting`'s
  Current status)
- [ ] Ownership transfer emits usage reports for both previous and new owner (the call site exists, but no usage
  report is ever sent — see `cpt-cf-file-storage-fr-usage-reporting`'s Current status)
- [ ] File events emitted to EventBroker on write operations (upload, update, delete) when enabled by owner policy
  (**Partial:** written to the `events_outbox`; no relay delivers them to EventBroker)
- [x] HTTP Range requests return partial content for downloads; seeking and resumable downloads supported;
  `Accept-Ranges: bytes` set on every download response
- [ ] Retention policies automatically expire and delete files based on configured age, inactivity, or custom metadata
  criteria; per-file retention overrides are honored (**Not enforced yet:** no background worker runs it)
- [ ] Storage backends in P1 are loaded from the gear's own section of the platform YAML configuration at gear
  startup (no standalone TOML/JSON file); in P3, backends can
  be connected and configured at runtime via admin API without service rebuild (**Partial:** the P1 platform-YAML
  loading half is implemented; the P3 runtime admin API half is not)
- [ ] File ownership transferable by current owner to another user or app within the same tenant; transfer requires
  authorization of both parties and emits an audit record (**Partial:** the current owner's authorization is checked
  and an audit record is emitted; the receiving principal's authorization/existence is not verified — see
  `cpt-cf-file-storage-fr-ownership-transfer`)
- [x] Custom metadata operations rejected when exceeding configurable limits (max pairs, key length, value length, total
  size)
- [ ] Read audit records emitted for every download when enabled by policy

## 10. Dependencies

| Dependency            | Description                                                        | Criticality |
|-----------------------|--------------------------------------------------------------------|-------------|
| ToolKit Framework      | Gear lifecycle, ClientHub for service registration               | p1          |
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
- The control-plane API requires platform JWT in P1; content is reached only via short-lived signed URLs against the
  sidecar, which carry their own AND-combined constraints. Any external/anonymous sharing is deferred to P3 (see `§5.3`)
- The sidecar has **no** metadata-DB connection of its own — it resolves everything it needs from the verified
  signed token's claims and reports back to the control plane over a token-authenticated HTTP callback, never a
  direct DB write. The two planes' only shared state is the signing keypair (private on the control plane, public
  on the sidecar) and the token format they agree on
- Policy configuration is available to tenant administrators and users through the platform

## 12. Risks

| Risk                                                                | Impact                                                         | Mitigation                                                                                                                                              |
|---------------------------------------------------------------------|----------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------|
| Storage service unavailability blocks all file-dependent operations | High — multimodal AI, document workflows disrupted             | Design for graceful degradation; clear error propagation to consumers                                                                                   |
| Large file sizes increase request latency for consuming gears     | Medium — slow responses for multimodal and document operations | Metadata pre-fetch enables size validation; streaming support for large files                                                                           |
| Backend credential compromise enables unauthorized backend access  | High — data exposure                                           | Backend credentials held only by FileStorage and never exposed to clients (proxy model — see DESIGN.md); standard credential rotation procedures apply at the FileStorage layer |
| Policy misconfiguration blocks legitimate uploads                   | Medium — user frustration                                      | Policy validation on save; clear error messages identifying which policy was violated                                                                   |

## 13. Open Questions

None.

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/)
- **Features**: [features/](./features/)
