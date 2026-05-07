<!-- Created: 2026-04-20 by Constructor Tech -->

# Decomposition: File Storage

**Overall implementation status:**
- [ ] `p1` - **ID**: `cpt-cf-file-storage-status-overall`

<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 Module Foundation ⏳ HIGH](#21-module-foundation--high)
  - [2.2 Files Schema and Repo ⏳ HIGH](#22-files-schema-and-repo--high)
  - [2.3 Backend Router and Roster ⏳ HIGH](#23-backend-router-and-roster--high)
  - [2.4 S3-Compatible Backend Adapter ⏳ HIGH](#24-s3-compatible-backend-adapter--high)
  - [2.5 Upload Lifecycle ⏳ HIGH](#25-upload-lifecycle--high)
  - [2.6 Read and Update ⏳ HIGH](#26-read-and-update--high)
  - [2.7 Batch Presigned Downloads ⏳ MEDIUM](#27-batch-presigned-downloads--medium)
  - [2.8 REST API ⏳ HIGH](#28-rest-api--high)
  - [2.9 Testkit and Local-Storage Recipe ⏳ MEDIUM](#29-testkit-and-local-storage-recipe--medium)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

## 1. Overview

The File Storage P1 design (DESIGN.md, ADR-0001..0005) is decomposed into nine implementable features. The decomposition follows a layered dependency order: a foundational ModKit module shell, then the metadata repository and backend abstraction, then the single P1 backend adapter (`s3-compatible`), then the lifecycle orchestration on top, and finally the REST surface and the testkit.

**Decomposition Strategy**:

- **Single P1 backend kind** — `s3-compatible` is the only backend adapter in P1. Local-disk deployments are realised by running `s3s-fs` (a Rust S3-compatible filesystem-backed server) side-by-side with FileStorage and registering it as a regular `s3-compatible` backend in the static TOML roster. There is no native POSIX `local` adapter; everything uniformly speaks SigV4 / presigned URLs. This removes the entire proxy-mode write path (and its physical-key-separation machinery) from P1.
- **No proxy upload path in P1** — every P1 backend is presign-capable. Proxy `put_file` (for backends that cannot issue presigned URLs, e.g. a future WebDAV adapter) is deferred to a later phase; P1 ships the trait method as a stub.
- **Multipart upload deferred to P2** — explicit deviation from PRD's `cpt-cf-file-storage-fr-multipart-upload` `p1` priority. P1 ships single-shot presigned PUT only. The pre-decided P2 design (Variant 2 — server-mediated init/abort + multipart presign URLs from extended `presign-batch` + reconcile after Complete) is documented in DESIGN.md §4 Future deltas. See ADR-0005 for the bucket-versioning + ABA-safe CAS strategy that pairs with multipart-aware reconcile in P2.
- **Cohesion by architectural layer** — every feature owns one horizontal slice (foundation, persistence, abstraction, the one adapter, coordinator, API surface, tests + recipe). No feature spans more than one DESIGN.md component.
- **Dependency order matches build order** — foundation features have no upstream prerequisites; each later feature names its concrete predecessors. The S3 adapter and the testkit are mutually independent and can proceed in parallel once their shared prerequisites land.
- **100% coverage of P1 design elements** — every DESIGN.md component, sequence, principle, constraint, and `dbtable` that the P1 phase realises is assigned to exactly one feature. Elements that DESIGN.md marks as deferred (multipart against non-S3 backends, audit, policies, quota, versioning, FileShare, native WebDAV adapter) are intentionally out of scope here and will be added as separate features in later phases.
- **Mutual exclusivity** — design elements appear in exactly one feature's "Design Components" / "Sequences" / "Data" lists. Cross-cutting principles (e.g. `cpt-cf-file-storage-principle-optimistic-concurrency`) attach to the single feature that owns the primitive (`files-schema-and-repo`) and are referenced — but not re-declared — by downstream callers.

## 2. Entries

### 2.1 [Module Foundation](feature-module-foundation/) ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-cf-file-storage-feature-module-foundation`

- **Purpose**: Stand up the FileStorage ModKit module — module manifest, static TOML configuration loader for the backend roster (P1 constraint: no runtime config), `DatabaseCapability` declaration, `ClientHub` registration of the `FileStorageClient` trait, and the SDK crate scaffolding (trait stub, error type, models). This is the foundation every other feature attaches to.

- **Depends On**: None

- **Scope**:
  - ModKit module shell with `Module`/`ModuleCtx` lifecycle hooks
  - Static TOML config loader: `[[backends]]` blocks with `id` (UUID) / kind / endpoint / credentials / `default_private` / `tenant_access` / capability flags
  - DB connection wiring through ModKit's `DatabaseCapability` (single shared schema for all backends per ADR-0001)
  - `file-storage-sdk` crate skeleton: `FileStorageClient` async trait, `FileStorageError` enum (with `DeleteInProgress`), `FileId` / `BackendId` / `Etag` aliases, `OwnerRef`, `Backend` / `BackendKind` / `BackendTransport` / `BackendCapability` enums (`PresignedUrls`, `PublicReadUrls`), `FileMeta` / `FileMetaUpdate` / `FileInfo`, `FileStatus` (`PendingUpload | Uploaded | Deleting`), `UrlParams`, `PresignedUploadHandle`, `PresignedDownload` (with `is_public`), `ReconcileResult`, `FileReadHandle`, `PresignDownloadItem` (with optional `version_id`), `PresignDownloadOutcome`
  - `ClientHub` registration of `dyn FileStorageClient` so consumers can resolve the SDK at runtime
  - Single default-backend configuration handle: `default_private_storage_id` (resolved from the `default_private` flag on roster entries; `default_public` and `PublicReadUrls` are P2 candidates)

- **Out of scope**:
  - Any concrete backend logic (split into `s3-compatible-adapter`)
  - DB schema and Files Repo (split into `files-schema-and-repo`)
  - Backend selection / capability resolution at request time (split into `backend-router-and-roster`)
  - Runtime backend registration (P2)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-file-storage-fr-rest-api`
  - [ ] `p1` - `cpt-cf-file-storage-interface-sdk-trait`
  - [ ] `p1` - `cpt-cf-file-storage-interface-rest-api`
  - [ ] `p1` - `cpt-cf-file-storage-contract-cf-modules`

- **Design Principles Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-principle-tenant-owner`

- **Design Constraints Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-constraint-no-ambient-authn`
  - [ ] `p2` - `cpt-cf-file-storage-constraint-static-config-p1`
  - [ ] `p2` - `cpt-cf-file-storage-constraint-server-minted-file-id`
  - [ ] `p2` - `cpt-cf-file-storage-constraint-no-bootstrap-connectivity-check`

- **Domain Model Entities**:
  - FileId
  - Etag
  - Backend
  - BackendKind
  - BackendCapability
  - BackendTransport
  - OwnerRef
  - FileMeta
  - FileMetaUpdate
  - FileInfo
  - FileStatus
  - UrlParams
  - PresignedUploadHandle
  - PresignDownloadItem
  - PresignDownloadOutcome
  - FileReadHandle

- **Design Components**:

  - [ ] `p2` - `cpt-cf-file-storage-component-sdk-facade`

- **API**:
  - SDK: `FileStorageClient` trait registered in `ClientHub`
  - Config: `[[backends]]` blocks, top-level `default_private_storage_id`

- **Sequences**:

  - (none — foundation feature has no sequences of its own)

- **Data**:

  - (none — DB schema lives in `files-schema-and-repo`)

### 2.2 [Files Schema and Repo](feature-files-schema-and-repo/) ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-cf-file-storage-feature-files-schema-and-repo`

- **Purpose**: Own the persistent metadata layer — the `file_storage.files` table (single shared schema for every backend kind per ADR-0001) and the Files Repo component that wraps it. The repo enforces the optimistic-concurrency contract: every mutation is a single conditional UPDATE with `(etag, updated_at[, xmin])` race detection, and the partial unique index on `(tenant_id, backend_id, file_path) WHERE status = 'uploaded'` is the supersession primitive used by the upload coordinator.

- **Depends On**: `cpt-cf-file-storage-feature-module-foundation`

- **Scope**:
  - DDL for `file_storage.files` (id, tenant_id, backend_id, file_path, owner_id, name, gts_file_type, mime_type, size_bytes, etag, version_id, status, custom_metadata, upload_expires_at, created_at, updated_at)
  - Indexes (3 in P1): `files_tenant_backend_path_uploaded_uq` (partial), `files_owner_lookup_idx`, `files_created_idx` (with `id` tiebreaker for stable cursor pagination)
  - Files Repo trait + impl: typed CRUD, conditional UPDATE with `(etag, updated_at[, xmin])` race-detection, status state-machine guards, cursor-paginated `list_files`
  - System-context API (no `SecurityContext`) for self-healing repair UPDATEs and the future P2 GC sweep

- **Out of scope**:
  - Backend selection (lives in `backend-router-and-roster`)
  - Reconciliation HEAD against backend (lives in `read-and-update` self-healing path)
  - Future P2 tables: `audit_events`, `file_versions`, `file_uploads`, `file_upload_parts`
  - Proxy two-phase commit — deferred along with future proxy backend kinds

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-file-storage-fr-metadata-storage`
  - [ ] `p1` - `cpt-cf-file-storage-fr-conditional-requests`
  - [ ] `p1` - `cpt-cf-file-storage-nfr-metadata-latency`

- **Design Principles Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-principle-optimistic-concurrency`
  - [ ] `p2` - `cpt-cf-file-storage-principle-atomic-metadata`
  - [ ] `p2` - `cpt-cf-file-storage-principle-file-id-address`

- **Design Constraints Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-constraint-system-context-maintenance`

- **Domain Model Entities**:
  - FileInfo
  - FileMetaUpdate
  - FileStatus

- **Design Components**:

  - [ ] `p2` - `cpt-cf-file-storage-component-files-repo`

- **API**:
  - Repo trait: `insert_pending`, `get_by_id`, `update_etag_conditional`, `update_status_with_supersession`, `delete_etag_conditional`, `list_paginated`, `repair_etag_system_context`

- **Sequences**:

  - (race detection on `(etag, updated_at[, xmin])` is implicit in every UPDATE — no standalone sequence)

- **Data**:

  - [ ] `p3` - `cpt-cf-file-storage-db-file-storage`
  - [ ] `p3` - `cpt-cf-file-storage-dbtable-files`

### 2.3 [Backend Router and Roster](feature-backend-router-and-roster/) ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-cf-file-storage-feature-backend-router-and-roster`

- **Purpose**: Resolve a `backend_id` (or the tenant's `default_private` fallback when none is supplied) to a concrete `StorageBackend` adapter. Enforce the per-backend tenant access list, capability declarations, and the at-least-one-`default_private` invariant. P1 declares only `s3-compatible` backends with the single `PresignedUrls` capability.

- **Depends On**: `cpt-cf-file-storage-feature-module-foundation`

- **Scope**:
  - Immutable backend registry built from TOML at init
  - `resolve(ctx, Option<&str>) -> &dyn StorageBackend` with default-private fallback
  - `list_backends(ctx) -> Vec<Backend>` filtered by per-tenant access list
  - Tenant access list enforcement (returns `NotFound` for slugs the tenant cannot see — no enumeration oracle)
  - Capability discovery surface (`requires_capability(cap) -> Result<…>`)
  - (No boot-time backend probes in P1 — connectivity and capability checks are deferred to first-request lazy paths per `cpt-cf-file-storage-constraint-no-bootstrap-connectivity-check`)
  - Internal `StorageBackend` adapter trait declaration (the single P1 impl lives in 2.4)
  - Per-backend `max_file_size_bytes` enforcement on the router boundary
  - All P1 backend instances are `s3-compatible`; `BackendKind` exposes only that variant in P1 (additional variants like `WebDav` ship when the corresponding adapter does)

- **Out of scope**:
  - Adapter implementation
  - Runtime backend registration (P2)
  - Per-tenant default override (P2)
  - WebDAV adapter (P3 — only future kind on the roadmap)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-file-storage-fr-backend-abstraction`
  - [ ] `p1` - `cpt-cf-file-storage-fr-backend-capabilities`
  - [ ] `p1` - `cpt-cf-file-storage-fr-tenant-boundary`

- **Design Principles Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-principle-modular-backend-roster`

- **Design Constraints Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-constraint-no-cross-backend-migration`

- **Domain Model Entities**:
  - Backend
  - BackendKind
  - BackendCapability
  - BackendTransport

- **Design Components**:

  - [ ] `p2` - `cpt-cf-file-storage-component-backend-router`

- **API**:
  - SDK: `FileStorageClient::list_backends(&ctx) -> Vec<Backend>`
  - REST: `GET /api/file-storage/v1/storages` (wired in 2.8)

- **Sequences**:

  - (none — Backend Router is invoked transitively by every SDK method)

- **Data**:

  - (none — registry is in-memory)

### 2.4 [S3-Compatible Backend Adapter](feature-s3-compatible-adapter/) ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-cf-file-storage-feature-s3-compatible-adapter`

- **Purpose**: Implement the **only** P1 backend adapter — `s3-compatible`. This adapter handles every P1 deployment: AWS S3, MinIO, Ceph RGW, Wasabi, GCS S3-compat, and the local-disk recipe via `s3s-fs` running side-by-side. PUT/GET/HEAD/DELETE against the configured bucket using `aws-sdk-s3` (or compatible), and generate SigV4 PUT/GET presigned URLs (per ADR-0003). P1 ships PUT-SigV4 without any backend-side preconditions — correctness is upheld by FileStorage's own primitives plus self-healing (ADR-0004). The adapter mirrors a small subset of metadata into S3 user-metadata for DR reconstruction; the SQL row remains authoritative on every read.

- **Depends On**: `cpt-cf-file-storage-feature-backend-router-and-roster`

- **Scope**:
  - `S3Backend` impl of `StorageBackend`: `open_read`, `delete_object`, `issue_presigned_put`, `issue_presigned_gets` (batch), `head_object` (used by self-healing reconciliation in 2.6)
  - SigV4 PUT/GET URL generation with mandatory pinned headers (`Content-Type`, `x-amz-meta-file-id`, `x-amz-meta-tenant-id`, `x-amz-meta-owner`, `x-amz-meta-gts-file-type`)
  - (No conditional-PUT preconditions in P1; `PresignedConditionalPut` is a P2 candidate)
  - (No public-read URL short-circuit in P1; `PublicReadUrls` is a P2 candidate)
  - In-memory streaming `open_read` over `aws-sdk-s3 GetObject` response body — used by `read_file` for in-process consumers (antivirus, llm-gateway)
  - Backend-error → `FileStorageError` translation (`BackendFailure`, `NotFound`)
  - In-place key reuse on overwrite (per ADR-0004 — backend object key derived from `file_id`, no separate column)

- **Out of scope**:
  - The SQL coordination row — owned by `files-schema-and-repo`
  - The lifecycle decision of when to issue PUT vs read — lives in `upload-lifecycle` / `read-and-update`
  - GCS-native `x-goog-if-generation-match` — explicit non-goal, P2/P3 future `gcs-native` backend kind
  - Proxy `put_file` (no `put_stream`) — P1 has no proxy upload path; deferred to a later phase along with future proxy backends
  - Magic-byte content-type validation — P3 (only meaningful when FileStorage is on the data plane; presigned PUT pins `Content-Type` via SigV4 instead)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-file-storage-fr-direct-transfer`
  - [ ] `p1` - `cpt-cf-file-storage-fr-signed-urls`

- **Design Principles Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-principle-presign-first`
  - [ ] `p2` - `cpt-cf-file-storage-principle-stream-by-default`
  - [ ] `p2` - `cpt-cf-file-storage-principle-batch-presigned-urls`
  - [ ] `p2` - `cpt-cf-file-storage-principle-self-healing`

- **Design Constraints Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-constraint-opaque-content`

- **Domain Model Entities**:
  - PresignedUploadHandle
  - PresignedDownload
  - UrlParams
  - FileByteStream

- **Design Components**:

  - [ ] `p2` - `cpt-cf-file-storage-component-s3-backend`

- **API**:
  - Internal adapter trait `StorageBackend` (instantiated as `S3Backend`); not directly exposed to consumers

- **Sequences**:

  - [ ] `p2` - `cpt-cf-file-storage-seq-presign-upload-s3`
  - [ ] `p2` - `cpt-cf-file-storage-seq-presign-download-s3`

- **Data**:

  - (no own tables — uses the shared `file_storage` schema through Files Repo)

### 2.5 [Upload Lifecycle](feature-upload-lifecycle/) ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-cf-file-storage-feature-upload-lifecycle`

- **Purpose**: Orchestrate the presign-first upload flow end-to-end. `create_presigned_url` registers a `pending_upload` row with sentinel `etag_pinned`, picks the right adapter (default-private fallback when `backend_id` is omitted), and asks the adapter for a presigned PUT URL with `Content-Type` / `Content-Disposition` / `x-amz-meta-<k>` pinned (NOT `x-amz-meta-gts-file-type`). `create_presigned_overwrite_url` is the variant-B re-upload variant: server pins the row's CURRENT meta, MAX-merges `upload_expires_at`. `reconcile` is the explicit HEAD-and-pull primitive: HEADs the storage backend, pulls etag, version_id, and S3-mirrored metadata into the row in one conditional UPDATE with retry. Concurrent `reconcile` calls converge by construction.

- **Depends On**:
  - `cpt-cf-file-storage-feature-files-schema-and-repo`
  - `cpt-cf-file-storage-feature-s3-compatible-adapter`

- **Scope**:
  - Upload Coordinator component
  - `create_presigned_url(ctx, backend_id?, owner, file_path, meta, params) -> PresignedUploadHandle`
  - `reconcile(ctx, file_id) -> ReconcileResult { info, s3_etag, s3_version_id }`
  - `create_presigned_overwrite_url(ctx, file_id, params) -> PresignedUploadHandle` — variant-B re-upload
  - Atomic supersession transaction (DELETE prior `uploaded` rows for the same `(tenant, backend, file_path)` + UPDATE the new row to `uploaded` in one TX)
  - Late-arrival idempotency: detect `derived(new_etag) == row.etag` after `0` rows affected, return `Ok` silently
  - GTS file type validation on `create_presigned_url`
  - Authz call into `AuthZResolverClient` with `gts.x.fstorage.file.type.v1~{type}` resource on every upload entry-point
  - `upload_expires_at` population for the future P2 GC sweep

- **Out of scope**:
  - Self-healing reconciliation triggers — they live in `read-and-update`
  - REST handlers — wired in `rest-api`
  - GC sweep — P2 (external scheduler runs a console command)
  - Proxy upload — P3 (no consumer in P1)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-file-storage-fr-upload-file`
  - [ ] `p1` - `cpt-cf-file-storage-fr-file-ownership`
  - [ ] `p1` - `cpt-cf-file-storage-fr-authorization`
  - [ ] `p1` - `cpt-cf-file-storage-fr-file-type-classification`
  - [ ] `p1` - `cpt-cf-file-storage-fr-data-classification`
  - [ ] `p1` - `cpt-cf-file-storage-nfr-durability`
  - [ ] `p1` - `cpt-cf-file-storage-nfr-url-availability`

- **Design Principles Covered**:

  - (presign-first principle is owned by `s3-compatible-adapter`; this feature consumes it)

- **Design Constraints Covered**:

  - (no new constraint registrations beyond what foundation/repo provide)

- **Domain Model Entities**:
  - FileInfo
  - PresignedUploadHandle
  - FileStatus

- **Design Components**:

  - [ ] `p2` - `cpt-cf-file-storage-component-upload-coordinator`

- **API**:
  - SDK: `create_presigned_url`, `create_presigned_overwrite_url`, `reconcile`
  - REST: `POST /api/file-storage/v1/presign-batch` (kind=upload, both initial and variant-B re-upload), `POST /api/file-storage/v1/files/{file_id}/meta/reconcile` (wired in 2.8)

- **Sequences**:

  - (covered by `seq-presign-upload-s3` in 2.4 — no new sequences here)

- **Data**:

  - (no own tables — uses `files` through Files Repo)

### 2.6 [Read and Update](feature-read-and-update/) ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-cf-file-storage-feature-read-and-update`

- **Purpose**: Implement every read- and update-path SDK operation: `get_file_info`, `read_file` (lazy in-process self-healing trigger per ADR-0004 — both etag-pinned and unpinned variants), `put_file_info` (PUT-only metadata replace with `Deleting`-row rejection), `delete_file` (2-phase hard delete), and `list_files` (P1 query: `owner_id?`, `cursor?`, `limit?`).

- **Depends On**:
  - `cpt-cf-file-storage-feature-files-schema-and-repo`
  - `cpt-cf-file-storage-feature-s3-compatible-adapter`

- **Scope**:
  - `get_file_info(ctx, file_id, etag?) -> FileInfo`
  - `put_file_info(ctx, file_id, FileMetaUpdate, etag?) -> FileInfo` — null-keeps-existing semantics; atomic DB+S3 metadata sync via `CopyObject` self-copy with `MetadataDirective: REPLACE`. Optional `If-Match` becomes a strong CAS over both DB and S3 (HEAD verifies S3 etag and version_id; CopyObject carries `x-amz-copy-source-if-match`).
  - `delete_file(ctx, file_id, etag) -> ()`
  - `list_files(ctx, ListFilesQuery) -> FileList` — P1 query parameters: `owner_id?`, `cursor?`, `limit?`; sort fixed to `created_at DESC, id ASC`; defaults to caller's own files when `owner_id` omitted
  - `read_file(ctx, file_id, etag?) -> FileReadHandle` — opens backend GET via the S3 adapter, reads `s3_etag` from response header, runs self-healing UPDATE when `s3_etag != row.etag`
  - Authz check on every entry point (read, write, delete) with the file's GTS type as resource context
  - For `delete_file`: enqueue the row's `(backend_id, id)` for asynchronous orphan-delete with grace period ≥ max signed-URL TTL (per invariant I6)
  - In-flight `read_file` stream isolation: S3 in-flight `GetObject` continues to serve pre-overwrite bytes until completion

- **Out of scope**:
  - Presigned download URLs (split into `batch-presign-downloads`)
  - REST handlers — wired in `rest-api`
  - Proxy `put_file` content overwrite — P1 ships a stub (`unimplemented!()`); overwrites go through `create_presigned_overwrite_url` + external PUT + `reconcile`
  - Multipart upload — P2

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-file-storage-fr-download-file`
  - [ ] `p1` - `cpt-cf-file-storage-fr-delete-file`
  - [ ] `p1` - `cpt-cf-file-storage-fr-get-metadata`
  - [ ] `p1` - `cpt-cf-file-storage-fr-list-files`
  - [ ] `p1` - `cpt-cf-file-storage-fr-update-metadata`
  - [ ] `p1` - `cpt-cf-file-storage-fr-retention-indefinite`
  - [ ] `p1` - `cpt-cf-file-storage-nfr-transfer-latency`
  - [ ] `p1` - `cpt-cf-file-storage-nfr-scalability`

- **Design Principles Covered**:

  - [ ] `p2` - `cpt-cf-file-storage-principle-self-healing`

- **Design Constraints Covered**:

  - (system-context maintenance constraint covered by `files-schema-and-repo`)

- **Domain Model Entities**:
  - FileInfo
  - FileMetaUpdate
  - FileReadHandle
  - FileByteStream
  - ListFilesQuery
  - FileList

- **Design Components**:

  - (consumes Files Repo + S3 adapter; no new component declarations)

- **API**:
  - SDK: `get_file_info`, `put_file_info`, `delete_file`, `list_files`, `read_file`
  - REST (wired in 2.8): `GET /api/file-storage/v1/files/{file_id}/meta`, `PUT /api/file-storage/v1/files/{file_id}/meta`, `DELETE /api/file-storage/v1/files/{file_id}`, `GET /api/file-storage/v1/files`. `read_file` is an in-process SDK method only — no REST surface; external download flow uses `presign_urls`.

- **Sequences**:

  - [ ] `p2` - `cpt-cf-file-storage-seq-proxy-read`

- **Data**:

  - (no own tables — uses `files`)

### 2.7 [Batch Presigned Downloads](feature-batch-presign-downloads/) ⏳ MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-file-storage-feature-batch-presign-downloads`

- **Purpose**: Implement `presign_urls` — the batch entry-point for browser-bound download URLs. Per item, optionally fail-fast on stale `etag`, then issue a SigV4 GET URL. The batch shape is what keeps P3-remote topology at one RTT regardless of N (DESIGN §2.1, batch-first principle). P1 issues only SigV4-signed URLs.

- **Depends On**:
  - `cpt-cf-file-storage-feature-s3-compatible-adapter`
  - `cpt-cf-file-storage-feature-files-schema-and-repo`

- **Scope**:
  - `presign_urls(ctx, Vec<PresignDownloadItem>) -> Vec<PresignDownloadOutcome>`
  - Per-item authz check (per-file-id, per-GTS-type)
  - Per-item etag fail-fast — if `item.etag.is_some() && item.etag != row.etag`, the item's outcome is `Err(EtagMismatch{ current: row.etag })` with no URL signed
  - Capability gate: `PresignedUrls` requirement; `CapabilityUnavailable` for backends without it
  - Composes per-backend max signed-URL TTL into a server-side cap on `expires_in_seconds`
  - Orphan-delete grace invariant guard at config load (`orphan_delete_grace_seconds ≥ max_signed_url_ttl + safety_margin`)

- **Out of scope**:
  - Shareable links with revocation, view counters, IP restrictions — P3 FileShare module
  - Mass-presign over a logical search (presign N URLs by query) — out of P1

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-file-storage-fr-signed-urls`

- **Design Principles Covered**:

  - (batch-first principle covered by 2.4 / SDK Facade)

- **Design Constraints Covered**:

  - (no new constraint registrations)

- **Domain Model Entities**:
  - PresignDownloadItem
  - PresignDownloadOutcome
  - PresignedDownload
  - UrlParams

- **Design Components**:

  - (consumes S3 adapter + Files Repo)

- **API**:
  - SDK: `presign_urls`
  - REST (wired in 2.8): `POST /api/file-storage/v1/presign-batch`

- **Sequences**:

  - (covered by `seq-presign-download-s3` in 2.4)

- **Data**:

  - (no own tables)

### 2.8 [REST API](feature-rest-api/) ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-cf-file-storage-feature-rest-api`

- **Purpose**: Wire every SDK method to the 7 HTTP endpoints documented in `openapi.yaml`. All write operations are PUT-shaped or POST-shaped; there is no PATCH anywhere. **No proxied content endpoint exists in P1** — neither `GET /files/{file_id}/content` nor `PUT /files/{file_id}/content`: byte transfers always flow client ↔ storage backend directly via presigned URLs (or bare HTTPS for public-read backends). Conditional headers (`If-Match`, `If-None-Match`) are mapped 1:1 to the SDK's `etag` arguments — except for `POST /files/{id}/meta/reconcile`, which **rejects** `If-Match` with `400`. Errors are RFC 7807 `application/problem+json`.

- **Depends On**:
  - `cpt-cf-file-storage-feature-upload-lifecycle`
  - `cpt-cf-file-storage-feature-read-and-update`
  - `cpt-cf-file-storage-feature-batch-presign-downloads`

- **Scope**:
  - axum router under `/api/file-storage/v1/`
  - Backend roster: `GET /storages`
  - Upload flow: `POST /presign-batch` (kind=upload, both initial and variant-B re-upload), `POST /files/{file_id}/meta/reconcile`
  - File ops: `GET /files/{file_id}/meta`, `PUT /files/{file_id}/meta`, `DELETE /files/{file_id}`, `GET /files`
  - Batch presigned downloads: `POST /presign-batch` (kind=download)
  - `If-Match` / `If-None-Match` header → SDK `etag` argument mapping (rejected on `POST /reconcile`; optional on `PUT /meta` and `DELETE`)
  - `ETag` header on every response carrying a `FileInfo`
  - RFC 7807 `ProblemDetails` with stable `code` field for every error variant (including `delete_in_progress`, `conflict`, `payload_too_large` with `max_metadata_bytes` extension)
  - `SecurityContext` propagation from ModKit middleware

- **Out of scope**:
  - End-user self-service `GET /files/{file_id}/get-presigned-url` — P2
  - Ranged GETs (P2)
  - S3-compatible API façade and WebDAV façade — P2
  - Proxy content endpoints (`GET /files/{file_id}/content`, `PUT /files/{file_id}/content`) — deferred (no adapter consumes them in P1)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-file-storage-fr-rest-api`
  - [ ] `p1` - `cpt-cf-file-storage-interface-rest-api`

- **Design Principles Covered**:

  - (no new principle registrations)

- **Design Constraints Covered**:

  - (no new constraint registrations)

- **Domain Model Entities**:
  - (consumes the entire SDK domain model; no new entities)

- **Design Components**:

  - [ ] `p2` - `cpt-cf-file-storage-component-rest-api`

- **API**:
  - REST (7 routes): `GET /api/file-storage/v1/storages`, `GET /api/file-storage/v1/files`, `GET /api/file-storage/v1/files/{file_id}/meta`, `PUT /api/file-storage/v1/files/{file_id}/meta`, `POST /api/file-storage/v1/files/{file_id}/meta/reconcile`, `DELETE /api/file-storage/v1/files/{file_id}`, `POST /api/file-storage/v1/presign-batch`

- **Sequences**:

  - (every sequence in DESIGN §3.6 is realised end-to-end through this REST surface; no standalone sequence here)

- **Data**:

  - (no own tables)

### 2.9 [Testkit and Local-Storage Recipe](feature-testkit-and-local-storage-recipe/) ⏳ MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-file-storage-feature-testkit-and-local-storage-recipe`

- **Purpose**: Stand up `file-storage-testkit` — the in-process `s3s-fs` fixture (DESIGN §4) plus the lifecycle and self-healing regression tests — **and** document the same `s3s-fs` setup as the P1 recipe for local-disk deployments (operators run `s3s-fs` side-by-side with FileStorage and register it as a regular `s3-compatible` backend, instead of running a native POSIX adapter that does not exist in P1). Provides the test infrastructure every other feature reuses for integration tests.

- **Depends On**: `cpt-cf-file-storage-feature-module-foundation`

- **Scope**:
  - `file-storage-testkit` crate under `[dev-dependencies]`
  - `LocalS3Fixture::start()` — `tempfile::TempDir` + `s3s_fs::FileSystem` + `s3s::service::S3ServiceBuilder` + hyper bind on random port + RAII shutdown
  - `s3s = "=0.13.0"`, `s3s-fs = "=0.13.0"` exact pin (per DESIGN §4)
  - Lifecycle and self-healing regression tests (presign → external PUT → reconcile; out-of-band mutation → read_file / reconcile repair; 2-phase delete; variant-B re-upload) — green here proves the pinned `s3s-fs` version still produces ETag values that match what our reconcile flow expects to pull from HEAD
  - End-to-end lifecycle test (presign → external PUT → reconcile → presigned download → read_file with etag → put_file_info DB+S3 sync → delete_file) against the fixture
  - Self-healing reconciliation test (mutate the fixture's TempDir directly, then call `read_file` → expect repair)
  - Cross-tenant isolation test (file_id in tenant A → request from tenant B → 404 NotFound, no enumeration leak)
  - Operator recipe documentation: how to deploy `s3s-fs` as a side-process for local-disk use cases and register it in FileStorage's TOML as an `s3-compatible` backend (the only local-disk recipe — there is no native POSIX `local` adapter on the roadmap)

- **Out of scope**:
  - Binary-mode `s3s-fs` subprocess fixture (deferred non-goal per DESIGN §4)
  - Load tests against real AWS S3 / MinIO via testcontainers — separate integration tier

- **Requirements Covered**:

  - (testkit covers no PRD requirement directly — it is verification infrastructure plus deployment recipe)

- **Design Principles Covered**:

  - (no new principle registrations)

- **Design Constraints Covered**:

  - (no new constraint registrations)

- **Domain Model Entities**:
  - (re-exports SDK types; no new entities)

- **Design Components**:

  - [ ] `p3` - `cpt-cf-file-storage-design-testing`

- **API**:
  - `LocalS3Fixture::start() -> anyhow::Result<LocalS3Fixture>`
  - `LocalS3Fixture::addr() -> SocketAddr`
  - `LocalS3Fixture::credentials() -> &TestCredentials`

- **Sequences**:

  - (no own sequences — tests exercise sequences from 2.4 / 2.5 / 2.6)

- **Data**:

  - (no own tables — fixture uses `s3s-fs` per-test `TempDir`)

---

## 3. Feature Dependencies

```text
cpt-cf-file-storage-feature-module-foundation
    ↓
    ├─→ cpt-cf-file-storage-feature-files-schema-and-repo
    │       ↓
    │       ├─→ cpt-cf-file-storage-feature-upload-lifecycle
    │       ├─→ cpt-cf-file-storage-feature-read-and-update
    │       └─→ cpt-cf-file-storage-feature-batch-presign-downloads
    │
    ├─→ cpt-cf-file-storage-feature-backend-router-and-roster
    │       ↓
    │       └─→ cpt-cf-file-storage-feature-s3-compatible-adapter
    │               ↓
    │               ├─→ cpt-cf-file-storage-feature-upload-lifecycle
    │               ├─→ cpt-cf-file-storage-feature-read-and-update
    │               └─→ cpt-cf-file-storage-feature-batch-presign-downloads
    │
    └─→ cpt-cf-file-storage-feature-testkit-and-local-storage-recipe

cpt-cf-file-storage-feature-upload-lifecycle
cpt-cf-file-storage-feature-read-and-update
cpt-cf-file-storage-feature-batch-presign-downloads
    ↓
    └─→ cpt-cf-file-storage-feature-rest-api
```

**Dependency Rationale**:

- `cpt-cf-file-storage-feature-files-schema-and-repo` requires `cpt-cf-file-storage-feature-module-foundation`: the schema can only be created against the module's `DatabaseCapability`, and the Repo trait is registered through the `ClientHub` set up by the foundation feature.
- `cpt-cf-file-storage-feature-backend-router-and-roster` requires `cpt-cf-file-storage-feature-module-foundation`: the registry is built from the static TOML loaded by the foundation; capability flags and tenant access lists are declared there.
- `cpt-cf-file-storage-feature-s3-compatible-adapter` requires `cpt-cf-file-storage-feature-backend-router-and-roster`: the adapter must register with the router to be reachable.
- `cpt-cf-file-storage-feature-upload-lifecycle` requires `cpt-cf-file-storage-feature-files-schema-and-repo`: the supersession transaction and the `reconcile`-driven conditional UPDATE both run through the Repo. Also requires `cpt-cf-file-storage-feature-s3-compatible-adapter` for the presign-first PUT URL and the `head_object` call used inside `reconcile`.
- `cpt-cf-file-storage-feature-read-and-update` requires `cpt-cf-file-storage-feature-files-schema-and-repo`: every read and metadata mutation goes through the Repo. Also requires `cpt-cf-file-storage-feature-s3-compatible-adapter` because `read_file` opens an in-process `GetObject` stream through the adapter, and `delete_file` enqueues the adapter-level orphan-delete.
- `cpt-cf-file-storage-feature-batch-presign-downloads` requires `cpt-cf-file-storage-feature-s3-compatible-adapter` (for SigV4 GET signing) and `cpt-cf-file-storage-feature-files-schema-and-repo` (for per-item `etag` fail-fast).
- `cpt-cf-file-storage-feature-rest-api` requires all three lifecycle features (`upload-lifecycle`, `read-and-update`, `batch-presign-downloads`): every REST endpoint is a thin axum handler that calls one SDK method, so all of them must exist before the router compiles.
- `cpt-cf-file-storage-feature-testkit-and-local-storage-recipe` requires `cpt-cf-file-storage-feature-module-foundation` (re-exports SDK types from the foundation crate). It does NOT depend on any specific lifecycle feature — the fixture is a generic S3 mock and the lifecycle tests it provides are exercised by every later feature that wants integration coverage. Testkit can therefore be developed in parallel with everything except foundation.
- `cpt-cf-file-storage-feature-upload-lifecycle`, `cpt-cf-file-storage-feature-read-and-update`, and `cpt-cf-file-storage-feature-batch-presign-downloads` are mutually independent at the SDK level (each owns a disjoint subset of `FileStorageClient` methods) and can be developed in parallel once `files-schema-and-repo` plus `s3-compatible-adapter` are in place.
