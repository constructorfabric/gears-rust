<!-- Created: 2026-04-20 by Constructor Tech -->

# Rust SDK Contracts — File Storage

> Reference document for planned Rust trait contracts and SDK types.
> Canonical source after implementation: `file-storage-sdk/src/`.

Related specs: [DESIGN.md](./DESIGN.md) · [openapi.yaml](./openapi.yaml) · [migration.sql](./migration.sql)

## Overview

The FileStorage SDK exposes one consumer-facing trait — `FileStorageClient` — registered in the ModKit `ClientHub`. The shape of the trait follows the in-process SDK convention used by `simple-user-settings-sdk::SimpleUserSettingsClientV1`: every method takes a `&SecurityContext` first, every method is `async`, every method returns `Result<_, FileStorageError>`.

The trait is built around the **opaque `FileId` (UUID)** as the canonical handle, per [ADR-0002](./ADR/0002-cpt-cf-file-storage-adr-opaque-file-ids.md). All operations on an existing file address it by `file_id`; only `create_presigned_url` accepts a logical `file_path` (and turns it into a fresh `file_id`). Re-uploads against an existing file go through `create_presigned_overwrite_url`, which keeps the same `file_id`.

`read_file` and `put_file` both use an idiomatic async byte-chunk stream (`Stream<Item = Result<Bytes, _>>`) — the same shape that axum, reqwest, and tonic use for HTTP request/response bodies, so adapters can pipe bytes between FileStorage and the backend without intermediate buffering. `put_file` is the in-process SDK upload entry-point: it drives the adapter directly (e.g. `aws-sdk-s3 PutObject`) and commits the row in one async call, with no presigned URL roundtrip. External frontends always use the `create_presigned_url` → client-direct PUT → `reconcile` triad over REST and ship bytes straight to the backend over the presigned URL — there is no bytes-through-FileStorage proxy upload path in any phase.

`reconcile` is the explicit reconciliation primitive — it HEADs the backend, pulls the authoritative `s3_etag`, `s3_version_id`, and the entire user-visible metadata mirror (Content-Type, Content-Disposition, every `x-amz-meta-<k>` header), and writes the row in a single conditional UPDATE. Concurrent reconciles converge by construction (HEAD-first, conditional-UPDATE second). See [ADR-0004](./ADR/0004-cpt-cf-file-storage-adr-self-healing-reconciliation.md). The companion REST endpoint is `POST /files/{id}/meta/reconcile` and rejects `If-Match` with `400`.

The companion [ADR-0001](./ADR/0001-cpt-cf-file-storage-adr-s3-no-metadata-db.md), [ADR-0003](./ADR/0003-cpt-cf-file-storage-adr-presigned-put-sigv4.md), and [ADR-0005](./ADR/0005-cpt-cf-file-storage-adr-versioning-and-aba.md) explain the metadata-database decision, the choice of presigned PUT (SigV4) for the direct-transfer path, and the bucket-versioning + ABA-CAS strategy.

## SDK Models

Defined in `file-storage-sdk/src/models.rs`. Aligned with REST API schemas ([openapi.yaml](./openapi.yaml)) and follow the platform DDD pattern observed in `simple-user-settings-sdk/src/models.rs`.

```rust
use bytes::Bytes;
use modkit_macros::domain_model;
use std::collections::BTreeMap;
use std::pin::Pin;
use time::OffsetDateTime;
use uuid::Uuid;
use futures::Stream;

// ── Identifiers ─────────────────────────────────────────────────────────

/// Canonical, opaque file handle (per ADR-0002).
/// External URLs and cross-module references all key off `FileId`.
pub type FileId = Uuid;

/// Stable identity of a backend instance, assigned once in the static
/// TOML roster. Persisted in `FileInfo.backend_id` and used as the
/// optional `backend_id` field on upload requests.
pub type BackendId = Uuid;

/// Raw S3 ETag (sans surrounding quotes) for the file's current bytes.
/// CONTENT FINGERPRINT ONLY — does not track metadata changes (see
/// `cpt-cf-file-storage-constraint-etag-content-only`). Used for
/// conditional updates (HTTP `If-Match`-style optimistic concurrency)
/// on routes that opt in.
pub type Etag = String;

// ── Backend descriptors ─────────────────────────────────────────────────

/// One backend declared in the FileStorage roster (`GET /storages`).
/// The descriptor declares the role flags, the supported optional
/// capabilities, and whether the underlying S3 bucket has versioning
/// turned on. Operators are responsible for declaring `versioning`
/// correctly; FileStorage trusts the TOML and does NOT probe
/// `GetBucketVersioning` at boot
/// (`cpt-cf-file-storage-constraint-no-bootstrap-connectivity-check`).
///
/// **Backend uniformity (architectural invariant).** Every backend
/// speaks S3 protocol over HTTP and respects presigned URLs. There is
/// no `kind` or `transport` discriminator — those would only ever
/// hold one value, so they are not represented. Native non-S3
/// transports (POSIX, WebDAV, FTP, …) are out-of-scope at the
/// architecture level, not deferred. Gateway clients for non-S3
/// protocols, if needed, live as independent modules consuming
/// FileStorage's REST API and presigned URLs as ordinary clients.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backend {
    /// Stable backend identity, assigned once in the static TOML
    /// roster.
    pub id: BackendId,
    /// `true` when this backend is the tenant's default for new
    /// **private** files (presigned downloads only). At least one
    /// backend per tenant view MUST hold one default role.
    pub default_private: bool,
    /// `true` when this backend is the tenant's default for new
    /// **public-read** files. Implies the `PublicReadUrls` capability
    /// — every file in this backend gets an eternal bare-HTTPS URL.
    pub default_public: bool,
    pub capabilities: Vec<BackendCapability>,
    /// Per-backend hard ceiling, configured statically in P1.
    pub max_file_size_bytes: Option<u64>,
    /// Mirrors the underlying bucket's versioning configuration.
    /// `true` enables ABA-safe content CAS via `version_id` and lets
    /// callers request historical versions on `presign_urls`.
    /// Operator-declared in TOML — FileStorage trusts the value, no
    /// runtime probe (see ADR-0005).
    pub versioning: bool,
}

/// Optional capabilities a backend may advertise. These are
/// **variations** between already-S3-protocol-compatible backends, not
/// gates on whether something qualifies as a backend in the first
/// place. Presigned URL support is constitutive — a backend that
/// cannot sign `PUT/GET` URLs is not a FileStorage backend by
/// definition — so it is not represented as a capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendCapability {
    /// Backend serves objects through bare-HTTPS URLs without
    /// presigning (e.g. an S3 bucket with public-read ACL or an
    /// origin behind a CDN). Pairs with `Backend.default_public`.
    /// When this capability is present and a download is issued for
    /// a file in such a backend, `PresignedDownload.is_public` is
    /// `true` and the URL has no expiry — it is eternal for the
    /// file's lifetime.
    PublicReadUrls,
    /// Backend honours S3-style multipart upload
    /// (`CreateMultipartUpload` / `UploadPart` /
    /// `CompleteMultipartUpload` / `AbortMultipartUpload`). Optional
    /// — clients fall back to single-shot presigned PUT when absent.
    /// (P2 — see DESIGN §4 Future deltas for the server-mediated
    /// design.)
    Multipart,
}

// ── Owner ───────────────────────────────────────────────────────────────

/// Owner principal of a file. `owner_id` is the principal's UUID — a
/// user or an app — FileStorage does not distinguish; the kind is
/// tracked in the identity / authz subsystem.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerRef {
    pub tenant_id: Uuid,
    pub owner_id: Uuid,
}

// ── File metadata ───────────────────────────────────────────────────────

/// User-supplied key/value pairs attached to a file. The same shape on
/// the SDK and REST surfaces. Aggregated size (`mime_type`,
/// `Content-Disposition` derived from `name`, plus every
/// `x-amz-meta-<k>=<v>` mirrored entry) is capped at **2 KB** by AWS
/// S3 and enforced by FileStorage at presign and at `put_file_info`;
/// `gts_file_type` does NOT count toward this budget because it is
/// never mirrored to S3.
pub type CustomMetadata = BTreeMap<String, String>;

/// Caller-provided file metadata at upload time.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMeta {
    /// Display name (original upload name). Stored, returned, and
    /// pinned into both `Content-Disposition` SigV4 SignedHeader on
    /// the presigned PUT (initial-upload only) and the
    /// `response-content-disposition` query param on every presigned
    /// download. Never used to address the file (per ADR-0002).
    pub name: String,
    /// Declared MIME type. Pinned into both `Content-Type` SigV4
    /// SignedHeader on the presigned PUT (initial upload + variant-B
    /// re-upload) and the `response-content-type` query param on
    /// every presigned download.
    pub mime_type: String,
    /// Mandatory GTS file type (`gts.cf.fstorage.file.type.v1~…`).
    /// Immutable after creation. Injected into every authz request
    /// as the resource type. Stored in DB only — NOT mirrored to S3
    /// (specific exception to the meta-mirror rule). Structurally
    /// immutable: `FileMetaUpdate` does not declare this field, so
    /// `PUT /files/{id}/meta` cannot change it.
    pub gts_file_type: String,
    /// Caller's expected size in bytes when known up-front. The row's
    /// committed `size_bytes` is read from S3 Content-Length on every
    /// reconcile.
    pub size_bytes: Option<u64>,
    /// Application-defined key/value tags
    /// (`cpt-cf-file-storage-fr-metadata-storage`). Mirrored as
    /// `x-amz-meta-<k>=<v>` on the S3 object. The aggregate 2 KB
    /// user-metadata budget is enforced by FileStorage.
    pub custom_metadata: CustomMetadata,
}

/// Body for `put_file_info` — every field is optional. `Some(v)`
/// replaces the row's current value; `None` keeps it unchanged.
/// `gts_file_type` is **not** declared here — it is structurally
/// immutable. System-managed fields (`size_bytes`, `etag`,
/// `version_id`, timestamps, owner) are likewise out of scope.
#[domain_model]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileMetaUpdate {
    pub name: Option<String>,
    pub mime_type: Option<String>,
    pub custom_metadata: Option<CustomMetadata>,
}

/// Authoritative view of a file that FileStorage hands back to callers.
/// Serves both `get_file_info` and the response of every mutation
/// method.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    pub file_id: FileId,
    pub backend_id: BackendId,
    /// Logical path inside the backend's tenant scope, captured at
    /// upload time. Used for filtering / listing; not part of the
    /// URL.
    pub file_path: String,
    pub owner: OwnerRef,
    pub meta: FileMeta,
    pub status: FileStatus,
    /// Raw S3 ETag for the current bytes (sans surrounding quotes).
    /// Content fingerprint only.
    pub etag: Etag,
    /// Raw S3 VersionId for the current generation. `Some` when the
    /// hosting backend has `versioning = true`; `None` otherwise.
    pub version_id: Option<String>,
    pub size_bytes: u64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    /// `Some` while `status == PendingUpload` — the presigned URL
    /// TTL. `MAX`-merged across multiple variant-B re-upload presigns.
    pub upload_expires_at: Option<OffsetDateTime>,
}

/// Lifecycle states for a file row in the FileStorage database (P1).
///
/// ```text
///   ┌──────────────────┐                          ┌──────────────────┐
///   │  PendingUpload   │── reconcile (commit) ───▶│     Uploaded     │ ⟲ reconcile (drift
///   └────────┬─────────┘                          └────────┬─────────┘   resync, same row)
///            │                                             │
///            │ GC sweep                                    │ delete_file
///            │ (P2, upload_expires_at < NOW)               │ Phase 1
///            │                                             ▼
///            │                                    ┌──────────────────┐
///            │                                    │     Deleting     │
///            │                                    └────────┬─────────┘
///            │                                             │ delete_file
///            │                                             │ Phase 3
///            ▼                                             ▼
///                            ⟨ row purged ⟩
/// ```
///
/// `Deleting` is a transient operational state, never a soft-delete
/// tombstone (`cpt-cf-file-storage-constraint-no-soft-delete`).
/// `reconcile` on an already-`Uploaded` row is a self-loop — same row,
/// same state, refreshed `(etag, version_id, mirrored meta)` from a
/// HEAD-and-pull. The `PendingUpload → purged` arrow models the P2 GC
/// sweep harvesting upload sessions whose presigned URL TTL expired
/// before `reconcile` arrived (DESIGN §3.6 GC and orphans).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    /// Row inserted by the initial `presign-batch` upload item
    /// without `file_id`; bytes have not been confirmed yet.
    PendingUpload,
    /// Bytes acknowledged via `reconcile`. The only state in which
    /// `read_file` / `presign_urls` succeed.
    Uploaded,
    /// Phase 1 of `delete_file` flipped the row. Subsequent
    /// `reconcile` / `put_file_info` / `delete_file` against the row
    /// return `DeleteInProgress`; reads return `NotFound`. Phase 3
    /// hard-deletes the row after Phase 2 reaps the backend object.
    Deleting,
}

// ── Presigned URLs ──────────────────────────────────────────────────────

/// Knobs the caller wants applied to a presigned URL.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlParams {
    /// Requested TTL. Capped server-side by the backend's configured
    /// maximum; exceeding the cap is a `BadRequest`. Ignored for
    /// `PublicReadUrls` outcomes — public URLs have no expiry.
    pub expires_in_seconds: u64,
    /// Optional override for the presigned download's
    /// `Content-Disposition`. When `None`, FileStorage builds it
    /// from the row's `meta.name`
    /// (`attachment; filename="<row.name>"`). On presigned PUT, the
    /// adapter pins `Content-Disposition` from the row's `meta.name`
    /// and ignores this field (see
    /// `cpt-cf-file-storage-constraint-presigned-download-headers-from-db`).
    pub content_disposition: Option<String>,
    /// Optional override for the presigned download's
    /// `Content-Type`. When `None`, FileStorage uses the row's
    /// `meta.mime_type`. On presigned PUT, the adapter pins
    /// `Content-Type` from the row's `meta.mime_type` and ignores
    /// this field.
    pub content_type_override: Option<String>,
    /// Optional client IP allowlist enforced by the backend (S3
    /// bucket policy). Empty = no restriction.
    pub allowed_client_cidrs: Vec<String>,
}

/// Result of `create_presigned_url` and `create_presigned_overwrite_url`.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresignedUploadHandle {
    pub file_id: FileId,
    /// Pre-signed PUT URL the caller hands to the end-client.
    pub upload_url: String,
    /// Etag pinned by FileStorage at presign time. For an initial
    /// upload this is a sentinel; for a variant-B re-upload it is
    /// the row's current `etag` (so callers may use it as a CAS
    /// token elsewhere). The row's authoritative etag is rotated
    /// later by `reconcile` after HEAD against the backend.
    pub etag_pinned: Etag,
    pub expires_at: OffsetDateTime,
}

/// One entry of a batched download request.
///
/// `etag` is optional but **strongly recommended**. When present, the
/// implementation verifies `row.etag == etag` against the **DB row
/// only** (no HEAD against S3) and returns `EtagMismatch` for that
/// item if the row has rotated since the caller observed it. Issued
/// URLs are signed against the bytes that are at the backend object
/// at the moment they resolve.
///
/// `version_id` is optional and only meaningful when the file's
/// hosting backend has `versioning = true`. When set, the server
/// includes `versionId=<vid>` in the signed URL so the caller fetches
/// a historical generation. When unset, the URL resolves to the
/// current bytes.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresignDownloadItem {
    pub file_id: FileId,
    pub params: UrlParams,
    pub etag: Option<Etag>,
    pub version_id: Option<String>,
}

/// Per-item outcome inside a batched presigned-download response. The
/// outer `Result` only surfaces whole-batch transport / authz errors;
/// per-item failures (file deleted, etag mismatch, capability missing)
/// are reported inside the vector.
#[derive(Debug, Clone)]
pub struct PresignDownloadOutcome {
    pub file_id: FileId,
    pub result: Result<PresignedDownload, FileStorageError>,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresignedDownload {
    pub url: String,
    /// Time at which the URL stops resolving. For
    /// `PublicReadUrls` outcomes (`is_public == true`) this is set
    /// to a far-future sentinel ("never expires").
    pub expires_at: OffsetDateTime,
    /// `true` when the URL is a bare-HTTPS public-read URL backed by
    /// a `PublicReadUrls` backend (no presigning, no expiry). `false`
    /// for SigV4 GET URLs.
    pub is_public: bool,
}

// ── Reconcile result ────────────────────────────────────────────────────

/// Returned by `reconcile()`. `s3_etag` and `s3_version_id` are the
/// raw values pulled from the HEAD response; both are also present on
/// `info.etag` / `info.version_id`. Exposed in their raw form so
/// callers can correlate against what S3 returned on their own PUT
/// (race-loser detection).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileResult {
    pub info: FileInfo,
    pub s3_etag: String,
    pub s3_version_id: Option<String>,
}

// ── Streaming bodies ────────────────────────────────────────────────────
//
// `read_file` returns a byte-chunk stream (`Stream<Item = Result<Bytes, _>>`).
// This is the convention every modern Rust HTTP / RPC stack already
// speaks — axum's `Body::into_data_stream`, reqwest's
// `Response::bytes_stream`, tonic's `Streaming<Bytes>` — so adapters can
// pipe bytes between FileStorage and the wire without re-buffering. The
// trait alias keeps the type signatures readable while staying compatible
// with the `Pin<Box<…>>` erasure that the trait object requires.

/// Outbound body for `read_file`. Items are pushed in arrival order;
/// the producer is responsible for back-pressure.
pub type FileByteStream =
    Pin<Box<dyn Stream<Item = Result<Bytes, FileStorageError>> + Send + 'static>>;

/// Reader-side handle returned by `read_file`. The caller polls
/// `bytes` to receive chunks and treats the stream end as EOF; the
/// `info` snapshot is cheap to clone and lets callers inspect MIME,
/// size, etc. without an extra round-trip.
#[derive(Debug)]
pub struct FileReadHandle {
    pub info: FileInfo,
    pub bytes: FileByteStream,
}

// ── Listing / filtering ─────────────────────────────────────────────────

/// Optional filters for `list_files`. P1 exposes only owner-scoped
/// listing with cursor pagination. When `owner_id` is `None`, the
/// implementation defaults to the caller's `subject_id` so callers
/// see only their own files unless the AuthZ layer admits a broader
/// scope. Sort order is fixed to `created_at DESC, id ASC`. Other
/// filters (`mime_type`, `gts_file_type`, date range, `backend_id`)
/// are deferred to P2 — see DESIGN §4 Future deltas.
#[domain_model]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListFilesQuery {
    pub owner_id: Option<Uuid>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileList {
    pub items: Vec<FileInfo>,
    pub next_cursor: Option<String>,
}
```

## SDK Errors

Defined in `file-storage-sdk/src/errors.rs`. Each variant maps 1:1 to a `ProblemDetails.code` value (see [openapi.yaml](./openapi.yaml) `components.schemas.ProblemDetails`).

```rust
use thiserror::Error;

/// Errors returned by the FileStorage SDK. Variants map 1:1 to the
/// `ProblemDetails.code` values exposed by the REST API.
#[derive(Debug, Clone, Error)]
pub enum FileStorageError {
    /// `code = not_found` — file or backend missing. Returned
    /// identically for "absent" and "not visible to this tenant" so
    /// the API does not act as an enumeration oracle (per ADR-0002).
    #[error("not found")]
    NotFound,

    /// `code = access_denied` — authz rejected the request.
    #[error("access denied")]
    AccessDenied,

    /// `code = bad_request` — request validation failed (missing
    /// fields, invalid GTS file type, `If-Match` on `/reconcile`,
    /// `meta` field on a re-upload presign item, etc.).
    #[error("bad request: {0}")]
    BadRequest(String),

    /// `code = etag_mismatch` — the caller's `etag` does not match
    /// the current value on the row (or, for the strong-CAS variant
    /// of `put_file_info`, S3's reported etag/version_id has moved).
    /// The caller should re-read with `get_file_info` and retry.
    #[error("etag mismatch")]
    EtagMismatch,

    /// `code = delete_in_progress` — operation targeted a row in the
    /// transient `Deleting` status (Phase 1 of the 2-phase delete
    /// flow). Surfaced by `reconcile`, `put_file_info`, `delete_file`
    /// when they encounter such a row; reads return `NotFound`.
    #[error("delete in progress")]
    DeleteInProgress,

    /// `code = capability_unavailable` — the backend does not declare
    /// or has disabled the capability needed for this operation.
    #[error("capability unavailable: {0}")]
    CapabilityUnavailable(String),

    /// `code = payload_too_large` — bytes exceed `max_file_size_bytes`,
    /// or aggregate user-metadata exceeds the 2 KB AWS S3 budget.
    #[error("payload too large (max {max_bytes} bytes)")]
    PayloadTooLarge { max_bytes: u64 },

    /// `code = upload_expired` — the presigned URL TTL elapsed before
    /// `reconcile` confirmed the upload.
    #[error("upload expired")]
    UploadExpired,

    /// `code = backend_failure` — wrapped error from the storage
    /// backend (S3 5xx, etc.).
    #[error("backend failure: {0}")]
    BackendFailure(String),

    /// `code = conflict` (HTTP 409) — optimistic-concurrency retries
    /// exhausted on a write path. The handler captures `(etag,
    /// updated_at[, xmin])` at SELECT time and includes them in the
    /// conditional UPDATE; if `0` rows are affected the merge is
    /// retried up to 3 times before this variant is surfaced. See
    /// `put_file_info` for the canonical retry contract.
    #[error("conflict")]
    Conflict,

    /// `code = internal` — unexpected server error.
    #[error("internal error")]
    Internal,
}
```

## SDK Traits

Defined in `file-storage-sdk/src/api.rs`.

```rust
use async_trait::async_trait;
use modkit_security::SecurityContext;

/// Consumer-facing client trait registered in `ClientHub` by the
/// `file-storage` module. Follows the in-process SDK convention used
/// by `simple-user-settings-sdk::SimpleUserSettingsClientV1`.
///
/// Consumers obtain the client from `ClientHub`:
/// ```ignore
/// let files = hub.get::<dyn FileStorageClient>()?;
/// let backends = files.list_backends(&ctx).await?;
/// ```
///
/// Lifecycle of a file (the canonical "presign-first" flow):
///
/// 1. The application's own backend (e.g. chat) validates the request
///    against its domain rules.
/// 2. It calls `create_presigned_url` and receives a
///    `(file_id, upload_url, etag_pinned, expires_at)` tuple —
///    FileStorage has persisted a `PendingUpload` row.
/// 3. The end-client `PUT`s bytes directly to `upload_url` (SigV4 PUT
///    against the S3-compatible backend).
/// 4. The application's backend confirms the upload by calling
///    `reconcile(file_id)`. FileStorage HEADs the backend, pulls the
///    authoritative `s3_etag`, `s3_version_id`, and metadata mirror,
///    writes them into the row, and returns the post-commit
///    `FileInfo`.
/// 5. From here on every consumer references the file by `file_id`
///    (and an optional `etag` for optimistic concurrency).
///
/// **Re-uploading the same file** — the application's backend calls
/// `create_presigned_overwrite_url(file_id, params)` (no `meta`
/// argument). The server pins the row's current metadata into the
/// presigned PUT, the end-client re-PUTs to the same backend object
/// key, the application calls `reconcile(file_id)`. The row's
/// `etag` rotates to the new bytes; metadata is unchanged.
///
/// **Changing metadata** — `put_file_info(file_id, update, etag?)`.
/// The server merges the update into the row, then issues
/// `CopyObject` self-copy with `MetadataDirective: REPLACE` to
/// synchronize S3 user-metadata and `Content-Type` /
/// `Content-Disposition` to the new values, then UPDATEs the DB.
/// Optional `If-Match` becomes a strong CAS over both DB and S3.
///
/// All methods that take an `etag` argument honour it as
/// "proceed only if the row's current etag matches" — the SDK
/// returns `EtagMismatch` otherwise. Methods that take
/// `Option<&Etag>` make the check optional; passing `None` means
/// "I trust whatever I get". `reconcile` is the sole exception — it
/// takes no `etag`, because it is the explicit reconciliation
/// command. The REST endpoint `POST /files/{id}/meta/reconcile`
/// rejects `If-Match` with `400`.
#[async_trait]
pub trait FileStorageClient: Send + Sync {
    // ── Backends ────────────────────────────────────────────────────

    /// `GET /api/file-storage/v1/storages` — list backends visible
    /// to the caller's tenant. The response includes one entry per
    /// backend the tenant is allowed to see (per the per-backend
    /// access list configured in TOML).
    async fn list_backends(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<Backend>, FileStorageError>;

    // ── Upload coordination ─────────────────────────────────────────

    /// **Initial upload.** Validates input, registers a row with
    /// `status = PendingUpload`, and returns a presigned PUT URL the
    /// caller hands to the end-client. The presigned URL pins, via
    /// SigV4 SignedHeaders:
    /// - `Content-Type` = `meta.mime_type`
    /// - `Content-Disposition` = `attachment; filename="<URL-encoded meta.name>"`
    /// - `x-amz-meta-<k>` = `<v>` for each entry in `meta.custom_metadata`
    ///
    /// **`gts_file_type` is NOT pinned to the S3 object** — it is a
    /// DB-only field (specific exception to the meta-mirror rule).
    ///
    /// The owner's `tenant_id` MUST match the security context's
    /// tenant; otherwise the call returns `AccessDenied`.
    ///
    /// **Backend selection** — `backend_id` is optional. When `None`,
    /// FileStorage falls back to the caller's tenant `default_private`
    /// backend. When `Some`, the UUID is resolved through the
    /// per-tenant access list (`NotFound` if the caller's tenant
    /// cannot see it).
    async fn create_presigned_url(
        &self,
        ctx: &SecurityContext,
        backend_id: Option<BackendId>,
        owner: OwnerRef,
        file_path: &str,
        meta: FileMeta,
        params: UrlParams,
    ) -> Result<PresignedUploadHandle, FileStorageError>;

    /// **Variant B — re-upload to an existing `file_id`.** The
    /// server reads the row's current metadata (`name`, `mime_type`,
    /// `custom_metadata`) and pins those exact values into a fresh
    /// presigned PUT URL. **No `meta` argument is accepted** — to
    /// change metadata, call `put_file_info` first (or after the
    /// re-upload completes; the new bytes inherit whatever the row's
    /// metadata is at presign time).
    ///
    /// The row's `upload_expires_at` is updated to
    /// `MAX(coalesce(current, ε), NOW + TTL)` so multiple outstanding
    /// re-upload URLs never shorten an already-valid window.
    /// Returns `etag_pinned = row.etag` (the pre-overwrite etag) so
    /// the caller can correlate against `reconcile`'s post-overwrite
    /// etag for race-loser detection.
    ///
    /// A row in `PendingUpload` (no committed bytes yet) is rejected
    /// with `NotFound`; a row in `Deleting` returns `DeleteInProgress`.
    async fn create_presigned_overwrite_url(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        params: UrlParams,
    ) -> Result<PresignedUploadHandle, FileStorageError>;

    /// **Explicit reconciliation primitive.** HEADs the backend to
    /// learn the authoritative `s3_etag`, `s3_version_id`,
    /// `Content-Type`, `Content-Disposition`, and every
    /// `x-amz-meta-<k>` header, then writes the row.
    ///
    /// **When to call** — after a successful client PUT (to commit
    /// `pending_upload → uploaded`); after a re-upload to refresh
    /// the row's etag and metadata; whenever the caller suspects the
    /// row has drifted from S3. `reconcile` is safe to call at any
    /// time — it never produces an inconsistent intermediate state.
    ///
    /// **What it does** — single atomic HEAD-then-reconcile flow:
    ///
    /// 1. SELECT row by file_id with tenant scope. `Deleting` →
    ///    `DeleteInProgress`.
    /// 2. HEAD `derive(file_id)` on backend → `(s3_etag,
    ///    s3_version_id, content_type, content_disposition,
    ///    content_length, x-amz-meta-*)`. 404 → `BackendFailure`.
    /// 3. Build `new_meta_from_s3`:
    ///    - `name` parsed from Content-Disposition (URL-decoded)
    ///    - `mime_type` = Content-Type
    ///    - `custom_metadata` = every `x-amz-meta-<k>` header
    ///      (case-normalized, `x-amz-meta-` prefix stripped)
    ///    - `gts_file_type` is **kept from DB** — never pulled from
    ///      S3 even if `x-amz-meta-gts-file-type` is somehow present
    ///      (FileStorage never writes it there)
    ///    - `size_bytes` = Content-Length
    /// 4. Conditional UPDATE flipping `status` to `uploaded` (on
    ///    `pending_upload`) or rotating only the drifted columns
    ///    (on `uploaded`):
    ///       SET status='uploaded', etag=$s3_etag,
    ///           version_id=$s3_version_id,
    ///           name=$new.name, mime_type=$new.mime_type,
    ///           custom_metadata=$new.custom_metadata,
    ///           size_bytes=$content_length,
    ///           upload_expires_at=NULL,
    ///           updated_at=NOW()
    ///         WHERE id=$file_id
    ///           AND etag=$etag_db
    ///           AND updated_at=$updated_at_db
    ///           [AND xmin=$xmin_db]
    /// 5. `0` rows affected → race detected; retry up to 3 times
    ///    from step 1. After 3 unsuccessful attempts, return
    ///    `Conflict` (mapped to HTTP 409).
    /// 6. Return `ReconcileResult { info, s3_etag, s3_version_id }`.
    ///
    /// **State-machine semantics** — `pending_upload → uploaded` is
    /// the commit; `uploaded → uploaded` is drift correction.
    /// Callers may call `reconcile` repeatedly under suspected drift
    /// without side effects — it is idempotent by construction.
    ///
    /// **Idempotency** — multiple concurrent `reconcile` calls
    /// converge. Whichever caller wins the conditional UPDATE writes
    /// the row; the others re-SELECT, observe the row is already at
    /// the post-reconcile state, and return the same `Ok`.
    ///
    /// **No `If-Match` parameter** — the REST endpoint
    /// `POST /files/{id}/meta/reconcile` rejects `If-Match`
    /// with `400`. Adding an `etag` argument would make `reconcile`
    /// itself fail under exactly the conditions where it is most
    /// useful.
    ///
    /// **Error: `DeleteInProgress`** — when the row is in
    /// `Deleting`. Callers should treat the file as gone; do not
    /// retry.
    async fn reconcile(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
    ) -> Result<ReconcileResult, FileStorageError>;

    // ── File lookups ────────────────────────────────────────────────

    /// `GET /api/file-storage/v1/files/{file_id}/meta` — returns the
    /// authoritative metadata view from the FileStorage database
    /// without touching the storage backend.
    ///
    /// `etag` is optional: when supplied, the call returns
    /// `EtagMismatch` if the row's etag has rotated. The REST
    /// equivalent supports `If-None-Match` → `304 Not Modified`.
    async fn get_file_info(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Option<&Etag>,
    ) -> Result<FileInfo, FileStorageError>;

    /// `PUT /api/file-storage/v1/files/{file_id}/meta` — atomic
    /// DB+S3 metadata sync. Body fields are optional —
    /// `Some(v)` replaces, `None` keeps the existing value. The
    /// update body declares only `name`, `mime_type`, and
    /// `custom_metadata`; **`gts_file_type` is structurally not
    /// declared** — it is immutable.
    ///
    /// **DB+S3 sync via `CopyObject` self-copy.** The server merges
    /// the update against the row's current metadata, validates the
    /// 2 KB user-metadata budget, then issues a `CopyObject` with
    /// `CopySource = derive(file_id)`,
    /// `MetadataDirective = REPLACE`, and the new `Content-Type`,
    /// `Content-Disposition`, and `x-amz-meta-<k>` headers. The S3
    /// response carries the new `ETag` and (if versioning is on) the
    /// new `VersionId`; FileStorage writes those alongside the new
    /// metadata into the DB row in a single conditional UPDATE.
    ///
    /// **Optional `If-Match` = strong CAS on DB + S3.** When the
    /// caller passes `etag = Some(E1)`:
    /// - The server first checks DB.etag == E1 (412 on mismatch).
    /// - It then HEADs S3 to verify the live `s3_etag` and
    ///   `s3_version_id` (versioning-on backends only) match the
    ///   row (412 on mismatch — closes the ABA race per ADR-0005).
    /// - The `CopyObject` is issued with
    ///   `x-amz-copy-source-if-match: E1` for backend-side
    ///   precondition enforcement.
    ///
    /// When `etag = None` the call is best-effort
    /// last-write-wins on metadata
    /// (`cpt-cf-file-storage-constraint-no-meta-cas`). Race
    /// detection on the DB UPDATE still fires via the
    /// `(etag, updated_at[, xmin])` filter; on `0` rows the
    /// handler retries the merge up to 3 times before surfacing
    /// `Conflict`.
    ///
    /// **Concurrency** — single conditional UPDATE with status
    /// guard `WHERE status = 'uploaded'`. A row in `Deleting`
    /// returns `DeleteInProgress`.
    async fn put_file_info(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        update: FileMetaUpdate,
        etag: Option<&Etag>,
    ) -> Result<FileInfo, FileStorageError>;

    /// `DELETE /api/file-storage/v1/files/{file_id}` — 2-phase hard
    /// delete in P1.
    ///
    /// `etag` is optional. When supplied, Phase 1's conditional
    /// UPDATE includes it — protects against deleting a file that
    /// has been overwritten between read and delete. When omitted,
    /// the delete is best-effort last-write-wins (still gated by
    /// `status = 'uploaded'`).
    ///
    /// **Phases**:
    ///
    /// 1. Conditional UPDATE: `SET status = 'deleting' WHERE id =
    ///    $1 AND status = 'uploaded' [AND etag = $2]`. `0` rows →
    ///    `EtagMismatch` (when If-Match supplied) or `NotFound` (no
    ///    If-Match). Row already in `Deleting` →
    ///    `DeleteInProgress`.
    /// 2. Backend DELETE on `derive(file_id)` (S3 idempotent). On
    ///    transient failure: inline retry up to 3 attempts with
    ///    exponential backoff (e.g. 100 ms, 500 ms, 2 s). On
    ///    persistent failure: leave the row in `Deleting`, return
    ///    `BackendFailure` (HTTP 502). Subsequent reads return
    ///    `NotFound`. P2 GC sweep will retry.
    /// 3. `DELETE FROM files WHERE id = $file_id AND status =
    ///    'deleting'` — no etag check (we own the row in `Deleting`).
    ///
    /// **Concurrency** — A concurrent `read_file` whose stream is
    /// already in flight continues to receive bytes from its open
    /// backend handle (S3 GET-in-flight snapshot semantics). New
    /// readers after the Phase 1 commit see `NotFound`. Concurrent
    /// `reconcile` / `put_file_info` / `delete_file` against a
    /// `Deleting` row return `DeleteInProgress`.
    async fn delete_file(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Option<&Etag>,
    ) -> Result<(), FileStorageError>;

    /// `GET /api/file-storage/v1/files` — paginated owner-scoped
    /// listing across the backends the tenant can see. When
    /// `query.owner_id` is `None`, the implementation defaults to
    /// the caller's `subject_id` so callers see only their own
    /// files unless the AuthZ layer admits a broader scope. Sort
    /// order is fixed to `created_at DESC, id ASC`.
    async fn list_files(
        &self,
        ctx: &SecurityContext,
        query: ListFilesQuery,
    ) -> Result<FileList, FileStorageError>;

    // ── Streaming I/O ───────────────────────────────────────────────

    /// In-process SDK only — **no REST surface in P1**. Opens a
    /// streaming reader over the file content directly through the
    /// adapter; returns the authoritative `FileInfo` snapshot
    /// together with a byte stream of `bytes::Bytes` chunks.
    ///
    /// External / out-of-process callers (browsers, services in
    /// different processes) MUST use `presign_urls` and fetch bytes
    /// directly from the storage backend — FileStorage never
    /// proxies content over its REST surface in P1. `read_file`
    /// exists for in-process consumers that share the FileStorage
    /// runtime (antivirus / llm-gateway / file-parser) and benefit
    /// from the in-memory adapter handle (no extra hop,
    /// snapshot-isolated stream, on-the-fly self-healing).
    ///
    /// `etag` is optional: when supplied, the stream is opened only
    /// if the row currently matches; otherwise `EtagMismatch`. This
    /// is the equivalent of `If-Match` and is intended for
    /// in-process modules that pinned an etag earlier and want to
    /// fail fast on drift.
    ///
    /// **Self-healing** (ADR-0004) — `read_file` is the lazy
    /// in-process trigger for repairing presigned-first overwrite
    /// desync. When opening the backend GET, the SDK reads the
    /// backend's `ETag` response header. If it differs from the
    /// row's `etag`, the SDK repairs the row with a single
    /// conditional UPDATE **before** returning to the caller:
    ///
    ///   - **`etag = Some(e_pinned)`**: if the backend's etag does
    ///     not equal `e_pinned`, return `Err(EtagMismatch)` AFTER
    ///     repairing the row. Caller's retry sees the consistent
    ///     state.
    ///   - **`etag = None`**: repair the row, then return the
    ///     consistent `(FileInfo, bytes)` pair transparently — the
    ///     caller does not learn that a repair happened.
    ///
    /// External callers that need the eager equivalent of this
    /// reconciliation call `reconcile(file_id)` over REST — same
    /// write outcome, no byte transfer.
    ///
    /// The repair UPDATE runs in system-context (no
    /// `SecurityContext` consulted) per
    /// `cpt-cf-file-storage-constraint-system-context-maintenance`.
    ///
    /// **Yielded chunk size is transport-dependent.** The SDK forwards
    /// `bytes::Bytes` items as they arrive from the backend's HTTP
    /// response body (hyper/TCP framing) without re-aggregation, so
    /// individual chunks may be anywhere from a few KiB to ~1 MiB and
    /// the distribution is not stable across runs. Consumers that
    /// require fixed-size windows (e.g. an antivirus that scans in
    /// 64 KiB blocks, or a parser that expects 1 MiB pages) MUST adapt
    /// the stream themselves — typically via
    /// `futures::StreamExt::ready_chunks(n)` (coalesce up to `n`
    /// items) or a manual `BytesMut` accumulator that flushes at the
    /// desired boundary. The trait deliberately does not expose a
    /// `chunk_size` knob: forcing re-aggregation in the SDK costs an
    /// extra copy and is the opposite of what most callers want
    /// (zero-copy passthrough into another `Stream<Bytes>` sink).
    ///
    /// Typical pattern in an in-process consumer:
    ///
    /// ```ignore
    /// use futures::StreamExt;
    /// use sha2::{Digest, Sha256};
    /// use tokio::io::AsyncWriteExt;
    ///
    /// let mut handle = files.read_file(&ctx, file_id, Some(&etag)).await?;
    /// let mut hasher = Sha256::new();
    /// let mut sink = tokio::fs::File::create("/tmp/scan.bin").await?;
    /// while let Some(chunk) = handle.bytes.next().await {
    ///     let chunk = chunk?;
    ///     hasher.update(&chunk);
    ///     sink.write_all(&chunk).await?;
    /// }
    /// ```
    async fn read_file(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Option<&Etag>,
    ) -> Result<FileReadHandle, FileStorageError>;

    /// In-process SDK only — **no REST surface in P1**. Single-call
    /// upload for in-process callers (test fixtures, modules that
    /// generate content, antivirus storing a cleaned copy, etc.).
    ///
    /// **Fully implemented in P1.** Does NOT mint a presigned URL:
    /// in-process callers share the FileStorage runtime, so the SDK
    /// drives the adapter directly (`aws-sdk-s3 PutObject`) and
    /// commits the row in the same call. There is no
    /// bytes-through-FileStorage REST proxy in any phase; external
    /// frontends always use the `create_presigned_url` → client-direct
    /// PUT → `reconcile` triad and ship bytes straight to the backend
    /// over the presigned URL. `put_file` is the in-process equivalent
    /// that compresses the same lifecycle into one async call without
    /// the presign roundtrip.
    ///
    /// **Lifecycle.**
    /// 1. Mint `file_id`, INSERT row with `status = PendingUpload`,
    ///    sentinel `etag`, `upload_expires_at = NOW + TTL`, supplied
    ///    `owner`/`file_path`/`meta`. For variant-B re-upload
    ///    (`etag = Some(e)`), SELECT the existing row by
    ///    `(owner.tenant_id, backend_id, file_path)`, verify `etag = e`
    ///    (412 on mismatch), pin its `file_id` and current meta.
    /// 2. Stream `bytes` to the adapter's `PutObject` against
    ///    `derive(file_id)`, pinning `Content-Type`,
    ///    `Content-Disposition`, and `x-amz-meta-<k>` (NOT
    ///    `x-amz-meta-gts-file-type`) on the request.
    /// 3. HEAD the backend for authoritative
    ///    `(s3_etag, s3_version_id, content_length, mirrored meta)`.
    /// 4. Conditional UPDATE flips `pending_upload → uploaded` and
    ///    pulls the HEAD-derived fields in one transaction. The
    ///    partial unique index on
    ///    `(tenant_id, backend_id, file_path) WHERE status = 'uploaded'`
    ///    enforces last-write-wins; a concurrent racing finalize that
    ///    lands first leaves this call's row to be superseded.
    ///
    /// **Failure handling (in-process inline cleanup).** On error
    /// between (1) and (4) the SDK best-effort runs
    /// `DELETE FROM files WHERE id = $file_id AND status = 'pending_upload'`
    /// before surfacing the error, so a failed `put_file` does not
    /// leave a `PendingUpload` row behind. If the failure is between
    /// (2) and (4) — bytes already in the backend — the row is still
    /// removed; the orphan backend object is reclaimed by the P2 GC
    /// inverse sweep (DESIGN §3.6). A process crash mid-call falls
    /// back to the same P2 sweep via `upload_expires_at`.
    ///
    /// **Streaming.** `bytes` is a `Stream<Item = Result<Bytes, _>>`;
    /// the adapter consumes it without materialising the full payload
    /// in RAM. Per-backend size limits and the optional magic-byte
    /// MIME sniff (which buffers only the first chunk) are the only
    /// buffering points.
    ///
    /// **`etag` parameter.**
    /// - `None`: initial upload. A fresh `file_id` is minted; if the
    ///   `(tenant_id, backend_id, file_path)` slot already holds an
    ///   `Uploaded` row, the call supersedes it on commit (the loser
    ///   becomes a backend orphan reclaimed by P2 GC).
    /// - `Some(e)`: variant-B re-upload — same `file_id`, same backend
    ///   object key. Overwrite-in-place against pinned `etag`.
    ///   Mismatch returns `EtagMismatch`.
    async fn put_file(
        &self,
        ctx: &SecurityContext,
        backend_id: Option<BackendId>,
        owner: OwnerRef,
        file_path: &str,
        meta: FileMeta,
        bytes: FileByteStream,
        etag: Option<&Etag>,
    ) -> Result<FileInfo, FileStorageError>;

    // ── Presigned download URLs (batch-first) ──────────────────────

    /// `POST /api/file-storage/v1/presign-batch` (with `kind:
    /// "download"` items) — issue presigned download URLs for a
    /// batch of files.
    ///
    /// The method is batch-first by design — see DESIGN §2.1
    /// (`cpt-cf-file-storage-principle-batch-presigned-urls`):
    ///
    /// - **P1 embedded** — the SDK runs in-process alongside the
    ///   adapter and signs each item in memory. The batch collapses
    ///   to N cheap local operations with zero network round-trips.
    ///   A one-element `Vec` is indistinguishable in cost from a
    ///   former singleton API.
    /// - **P3 remote** — the SDK is an RPC stub with no signing
    ///   secrets. The whole batch travels in one RPC; one RTT
    ///   amortises every URL.
    ///
    /// **URL header overrides** — every issued URL pins
    /// `response-content-type=row.mime_type` and
    /// `response-content-disposition=attachment;
    /// filename="<row.name>"` from the DB row. This decouples the
    /// user-visible download serving from whatever S3 user-metadata
    /// happens to be on the object
    /// (`cpt-cf-file-storage-constraint-presigned-download-headers-from-db`).
    ///
    /// **Public-read backends** — when the file's hosting backend
    /// has the `PublicReadUrls` capability and `default_public =
    /// true`, the outcome carries `is_public = true` and a bare
    /// HTTPS URL with no expiry. SigV4 GET URLs are returned for
    /// every other backend.
    ///
    /// **Historical version GET** — when the hosting backend has
    /// `versioning = true` and the caller passes
    /// `PresignDownloadItem.version_id`, the signed URL embeds
    /// `versionId=<vid>` and resolves to the requested historical
    /// generation.
    ///
    /// Per-item authorization, NotFound, EtagMismatch, and
    /// CapabilityUnavailable errors surface inside
    /// `PresignDownloadOutcome.result`; the outer `Result` only
    /// fails for whole-batch transport errors.
    ///
    /// **Concurrency** — each item may include `etag` for
    /// fail-fast drift detection at presign time
    /// (DB-row-only check, no HEAD against S3 — mismatch ⇒ per-item
    /// `EtagMismatch`, no URL signed).
    async fn presign_urls(
        &self,
        ctx: &SecurityContext,
        items: Vec<PresignDownloadItem>,
    ) -> Result<Vec<PresignDownloadOutcome>, FileStorageError>;
}
```

## Internal Adapter Trait — `StorageBackend`

Lives inside the implementation crate (`file-storage/src/infra/backend.rs`), not in the public SDK. Captured here so implementers can plan the split described in DESIGN §3.2.

The adapter trait is the seam between the FileStorage core (which owns the `file_storage` SQL schema, the lifecycle write paths in REST handlers and the in-process `put_file` SDK, and authz integration) and a concrete byte store. It deliberately does **not** know about `file_id`, `OwnerRef`, or authz — those are resolved one layer up. The adapter only ever sees a `BackendObjectKey` (an opaque `String` minted by the core) and bytes. It also does NOT know about `gts_file_type` — that field is DB-only and never crosses the adapter boundary.

```rust
use async_trait::async_trait;
use bytes::Bytes;

/// Opaque per-backend object key, minted by the FileStorage core. For
/// `s3-compatible` backends it is derived deterministically from the
/// file's `file_id` (per ADR-0002). The adapter treats it as an opaque
/// string.
pub type BackendObjectKey = String;

/// Backend-side metadata returned by `head_object` and by the
/// `open_read` helper. `s3_etag` and `s3_version_id` are the raw
/// values (without surrounding quotes for the etag). The metadata
/// bag (`content_type`, `content_disposition`, `user_metadata`) is
/// what `reconcile` pulls into the row.
#[derive(Debug, Clone)]
pub struct BackendObjectMetadata {
    pub s3_etag: String,
    pub s3_version_id: Option<String>,
    pub size_bytes: u64,
    pub content_type: Option<String>,
    pub content_disposition: Option<String>,
    pub user_metadata: BTreeMap<String, String>,
}

/// Headers FileStorage wants pinned on the resulting S3 object. Used
/// by `issue_presigned_put` (initial upload + variant-B re-upload)
/// and by `copy_object_self` (PUT /meta sync).
#[derive(Debug, Clone)]
pub struct PinnedObjectHeaders {
    pub content_type: String,
    pub content_disposition: String,
    pub user_metadata: BTreeMap<String, String>,
}

#[async_trait]
pub trait StorageBackend: Send + Sync {
    fn descriptor(&self) -> &Backend;

    /// Stream the object's bytes. Returned stream MUST yield chunks
    /// in arrival order from the backend; the FileStorage layer
    /// adds the `FileInfo` snapshot from its own database. The
    /// metadata struct is captured from the GET response headers so
    /// the SDK facade can run self-healing reconciliation without
    /// an extra HEAD round-trip.
    async fn open_read(
        &self,
        key: &BackendObjectKey,
    ) -> Result<(FileByteStream, BackendObjectMetadata), FileStorageError>;

    /// Read backend-side metadata without streaming the body. Used
    /// by `reconcile` and by the strong-CAS variant of
    /// `put_file_info` to verify S3's live etag/version_id.
    async fn head_object(
        &self,
        key: &BackendObjectKey,
    ) -> Result<BackendObjectMetadata, FileStorageError>;

    async fn delete_object(
        &self,
        key: &BackendObjectKey,
    ) -> Result<(), FileStorageError>;

    /// Issue a presigned PUT URL (per ADR-0003 — SigV4 PUT). Pins
    /// `Content-Type`, `Content-Disposition`, and every
    /// `x-amz-meta-<k>` header from the supplied
    /// `PinnedObjectHeaders` into the SigV4 SignedHeaders set.
    /// FileStorage NEVER pins `x-amz-meta-gts-file-type` here — it
    /// is DB-only.
    async fn issue_presigned_put(
        &self,
        key: &BackendObjectKey,
        pinned: &PinnedObjectHeaders,
        params: &UrlParams,
    ) -> Result<(String /* upload_url */, OffsetDateTime /* expires_at */), FileStorageError>;

    /// `CopyObject self-copy` with `MetadataDirective: REPLACE` —
    /// rotates the object's user-metadata in place. When `if_match`
    /// is `Some(e)`, the request carries
    /// `x-amz-copy-source-if-match: e` and the backend rejects
    /// stale sources with `412`. Returns the new `s3_etag` and the
    /// new `s3_version_id` (if versioning is on).
    async fn copy_object_self(
        &self,
        key: &BackendObjectKey,
        new_pinned: &PinnedObjectHeaders,
        if_match: Option<&Etag>,
    ) -> Result<(String, Option<String>), FileStorageError>;

    /// Batched presigned-GET URL issuance. Mirrors
    /// `FileStorageClient::presign_urls`; per-key failures surface
    /// inside the outcome vector. The adapter sets
    /// `response-content-type` and `response-content-disposition`
    /// query params from the per-item hints (sourced from DB).
    /// For backends with the `PublicReadUrls` capability, the
    /// adapter MAY return a bare-HTTPS URL with no signing instead
    /// (`is_public = true`).
    async fn issue_presigned_gets(
        &self,
        items: Vec<PresignedGetItem>,
    ) -> Result<Vec<PresignedGetOutcome>, FileStorageError>;
}

#[derive(Debug, Clone)]
pub struct PresignedGetItem {
    pub key: BackendObjectKey,
    pub params: UrlParams,
    pub mime_type_hint: String,
    pub display_name_hint: String,
    /// When `Some` and the adapter's backend has `versioning =
    /// true`, the signed URL embeds `versionId=<vid>` so the
    /// caller fetches that historical generation.
    pub version_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PresignedGetOutcome {
    pub key: BackendObjectKey,
    pub result: Result<PresignedDownload, FileStorageError>,
}
```

## ClientHub Registration

Single implementation, single registration:

```rust
let svc: Arc<FileStorageService> = Arc::new(FileStorageService::new(/* … */));
ctx.client_hub().register::<dyn FileStorageClient>(svc.clone());
```

## Usage Example — full lifecycle

```rust
use file_storage_sdk::{
    FileStorageClient, FileMeta, OwnerRef,
    PresignDownloadItem, UrlParams, ReconcileResult,
};
use modkit_security::SecurityContext;
use std::collections::BTreeMap;
use uuid::Uuid;

let files = hub.get::<dyn FileStorageClient>()?;

// 1. Application backend creates a presigned upload URL after running
//    its own validations.
let owner = OwnerRef {
    tenant_id: ctx.tenant_id(),
    owner_id: ctx.subject_id(),
};
let meta = FileMeta {
    name: "plan.pdf".into(),
    mime_type: "application/pdf".into(),
    gts_file_type: "gts.cf.fstorage.file.type.v1~vendor.docmgmt.reports.file.v1~".into(),
    size_bytes: Some(18_274),
    custom_metadata: BTreeMap::new(),
};
let handle = files
    .create_presigned_url(
        &ctx,
        Some(s3_prod_backend_id),  // or None to land on the tenant's default_private backend
        owner.clone(),
        "chat/threads/abc/plan.pdf",
        meta,
        UrlParams {
            expires_in_seconds: 600,
            content_disposition: None,
            content_type_override: None,
            allowed_client_cidrs: vec![],
        },
    )
    .await?;
// hand handle.upload_url + handle.etag_pinned to the frontend …

// 2. After the frontend reports the upload completed, the application
//    backend reconciles the row.
let ReconcileResult { info, s3_etag, s3_version_id } =
    files.reconcile(&ctx, handle.file_id).await?;
// `info.etag` is the raw S3 ETag for the new bytes; `info.version_id`
// matches whatever S3 returned (None on non-versioning backends).

// 3. Later, another module asks for downloadable URLs. The URL is
//    SigV4-signed (or a bare HTTPS URL if the file lives in a
//    public-read backend) and time-limited per
//    `params.expires_in_seconds`.
let outcomes = files
    .presign_urls(
        &ctx,
        vec![PresignDownloadItem {
            file_id: info.file_id,
            params: UrlParams {
                expires_in_seconds: 300,
                content_disposition: None,
                content_type_override: None,
                allowed_client_cidrs: vec![],
            },
            etag: Some(info.etag.clone()),
            version_id: None, // current bytes
        }],
    )
    .await?;
```

## Usage Example — variant B re-upload

```rust
// Application backend re-asks the user to upload a fresh version of
// an existing file (same file_id).
let handle = files
    .create_presigned_overwrite_url(
        &ctx,
        existing_file_id,
        UrlParams {
            expires_in_seconds: 600,
            content_disposition: None,
            content_type_override: None,
            allowed_client_cidrs: vec![],
        },
    )
    .await?;
// Frontend PUTs to handle.upload_url with the headers FileStorage
// pinned from the row's CURRENT metadata. After PUT acknowledged:
let ReconcileResult { info, .. } = files.reconcile(&ctx, existing_file_id).await?;
// info.etag now reflects the new bytes; metadata is unchanged.
```

## Usage Example — in-process streaming read

```rust
use futures::StreamExt;

let mut handle = files.read_file(&ctx, file_id, Some(&etag)).await?;
println!("downloading {} ({} bytes)", handle.info.meta.name, handle.info.size_bytes);
while let Some(chunk) = handle.bytes.next().await {
    let chunk = chunk?;
    sink.write_all(&chunk).await?; // tokio AsyncWrite sink
}
```

## Alignment Table

| OpenAPI operationId | Trait method | Request type | Response type |
|---------------------|--------------|--------------|---------------|
| `listBackends` | `list_backends` | — | `Vec<Backend>` |
| `presignBatch` (`kind: "upload"`, no `file_id`) | `create_presigned_url` | `OwnerRef + file_path + FileMeta + UrlParams` | `PresignedUploadHandle` |
| `presignBatch` (`kind: "upload"`, with `file_id`) | `create_presigned_overwrite_url` | `FileId + UrlParams` | `PresignedUploadHandle` |
| `presignBatch` (`kind: "download"`) | `presign_urls` | `Vec<PresignDownloadItem>` | `Vec<PresignDownloadOutcome>` |
| `reconcileFileMeta` | `reconcile` | `FileId` | `ReconcileResult { info, s3_etag, s3_version_id }` |
| `getFileMeta` | `get_file_info` | `Option<Etag>` | `FileInfo` |
| `updateFileMeta` | `put_file_info` | `FileMetaUpdate + Option<Etag>` | `FileInfo` |
| `deleteFile` | `delete_file` | `Option<Etag>` | `()` |
| `listFiles` | `list_files` | `ListFilesQuery` | `FileList` |
| — (in-process SDK only, no REST in P1) | `read_file` | `Option<Etag>` | `FileReadHandle` (streaming) |
| — (in-process SDK only, no REST in P1) | `put_file` | `OwnerRef + file_path + FileMeta + FileByteStream + Option<Etag>` | `FileInfo` |

## Trait Hierarchy Summary

| Trait | Methods | Consumers | ClientHub key |
|-------|---------|-----------|---------------|
| `FileStorageClient` | 11 (full P1 surface: backends, file lifecycle including `reconcile`, both initial and variant-B presign, streaming I/O including the in-process direct-upload `put_file`, batched presign) | CyberFabric modules via ClientHub, REST adapter | `dyn FileStorageClient` |
| `StorageBackend` (internal) | 6 (adapter boundary; `head_object` is required by `reconcile` and the strong-CAS variant of `put_file_info`; `copy_object_self` is required by `put_file_info`'s DB+S3 sync; presigned URLs are batch-first on the GET side, single-shot on the PUT side per ADR-0003) | `BackendRouter`, REST endpoint handlers (`presign-batch`, `reconcile`, `delete`, `put_file_info`) and the in-process `put_file` SDK path | — (not registered in ClientHub) |

The 11 SDK methods are: `list_backends`, `create_presigned_url`, `create_presigned_overwrite_url`, `reconcile`, `get_file_info`, `put_file_info`, `delete_file`, `list_files`, `read_file`, `put_file`, `presign_urls` (REST counterpart `presign-batch` aggregates the two upload variants and the download variant). The internal adapter declares `open_read`, `head_object`, `delete_object`, `issue_presigned_put`, `copy_object_self`, and `issue_presigned_gets`.
