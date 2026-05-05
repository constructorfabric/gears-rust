<!-- Created: 2026-04-20 by Constructor Tech -->

# Rust SDK Contracts — File Storage

> Reference document for planned Rust trait contracts and SDK types.
> Canonical source after implementation: `file-storage-sdk/src/`.

Related specs: [DESIGN.md](./DESIGN.md) · [openapi.yaml](./openapi.yaml) · [migration.sql](./migration.sql)

## Overview

The FileStorage SDK exposes one consumer-facing trait — `FileStorageClient` — registered in the ModKit `ClientHub`. **In P1 this trait is the sole access interface to FileStorage** (`cpt-cf-file-storage-fr-rust-sdk`); every consumer (chat backend, llm-gateway, antivirus, file-parser, …) lives in the same monolith process and calls it in-process. **A FileStorage-owned REST surface is deferred to P2** (`cpt-cf-file-storage-fr-rest-api`) and will mirror this trait 1:1 — see [openapi.yaml](./openapi.yaml). Throughout this document, references to `POST /…` / `PUT /…` / etc. describe the P2 REST mapping that the P1 SDK shape is designed to feed into.

The shape of the trait follows the in-process SDK convention used by `simple-user-settings-sdk::SimpleUserSettingsClientV1`: every method takes a `&SecurityContext` first, every method is `async`, every method returns `Result<_, FileStorageError>`.

The trait is built around the **opaque `FileId` (UUID)** as the canonical handle, per [ADR-0002](./ADR/0002-cpt-cf-file-storage-adr-opaque-file-ids.md). All operations on an existing file address it by `file_id`. There is one upload entry-point — `create_presigned_upload` — which accepts an optional `file_id` (when present, it overwrites an existing row in place; when absent, it mints a fresh `file_id` and registers a new `PendingUpload` row).

**Identifiers are immutable; files cannot be renamed.** Both `file_id` and `file_path` (the logical path / file key inside the backend's tenant scope) are captured at `create_presigned_upload` and remain fixed for the file's lifetime. There is no SDK or future-REST surface that mutates either — `FileMetaUpdate` does not declare them, and no other operation accepts an `update_path` / `move` / `rename` argument. To change the logical address, callers upload a new file at the desired `file_path` and delete the old one. The display label `meta.name` (used in `Content-Disposition` on downloads) IS mutable through `put_file_info` — it is a label, not an identifier.

`read_file` and `put_file` both use an idiomatic async byte-chunk stream (`Stream<Item = Result<Bytes, _>>`) — the same shape that axum, reqwest, and tonic use for HTTP request/response bodies, so adapters can pipe bytes between FileStorage and the backend without intermediate buffering. `put_file` is the in-process SDK upload entry-point: it drives the same `S3Backend` adapter as the presigned path (`create_multipart_and_presign_parts` → per-part `UploadPart` → `complete_multipart`) and commits the row in one async call without the presign roundtrip. **There is no single-shot `PutObject` path** anywhere in the design — even small in-process uploads use a 1-part multipart session (one part of arbitrary size, last-part rule). External frontends always use the `create_presigned_upload` → client-direct PUT (per part) → `complete_upload` triad — in P1 the application backend invokes those SDK methods in-process and ships bytes straight to the backend over the presigned URLs; in P2, the same triad is also reachable over the REST API. There is no bytes-through-FileStorage proxy upload path in any phase.

**Upload is always multipart.** Even one-byte files go through the multipart lifecycle (single part, last-part rule lets `part_size = 0..` work). This is intentional: a single canonical write path means a single set of state transitions to reason about, no `<hex>` vs `<hex>-N` ETag-format split, and no second protocol to test/maintain.

**No `reconcile` primitive.** Atomicity of every DB↔S3 mutation is guaranteed by **multi-phase commits**: `complete_upload` flips `PendingUpload → Completing → Uploaded`, `put_file_info` flips `Uploaded → MetaUpdating → Uploaded`, `delete_file` flips `Uploaded → Deleting → purged`. A row stuck in any transient state is recovered in-band on the next SDK call against it (HEAD the backend, finalize the row, then serve the original request); the GC sweep (P2) is the safety net for crashed clients. **Recovery happens only on the SDK path** — direct-to-S3 reads via presigned URLs do not trigger recovery and read the bytes that are at the backend object regardless of the DB row's transient status.

The companion [ADR-0001](./ADR/0001-cpt-cf-file-storage-adr-s3-no-metadata-db.md), [ADR-0003](./ADR/0003-cpt-cf-file-storage-adr-presigned-put-sigv4.md), and [ADR-0005](./ADR/0005-cpt-cf-file-storage-adr-versioning-and-aba.md) explain the metadata-database decision, the choice of presigned PUT (SigV4) for the direct-transfer path, and the bucket-versioning + ABA-CAS strategy.

## SDK Models

Defined in `file-storage-sdk/src/models.rs`. Aligned with the future-P2 REST API schemas ([openapi.yaml](./openapi.yaml)) — the REST shape mirrors these models 1:1 — and follow the platform DDD pattern observed in `simple-user-settings-sdk/src/models.rs`.

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

/// Raw S3 VersionId (opaque, up to 1024 bytes) for the file's current
/// generation. `Some` only when S3 returned a `x-amz-version-id`
/// header on the relevant write (per ADR-0005). FileStorage treats
/// the value as opaque — no parsing, sorting, or monotonicity
/// assumptions.
///
/// Two roles on SDK methods:
///
/// - **Historical selector** on `read_file` and `presign_urls`
///   (download): the bytes returned correspond to that exact S3
///   generation (`GetObject?versionId=<vid>`).
/// - **CAS pin** on `get_file_info`, `put_file_info`, `delete_file`,
///   `put_file`, `create_presigned_upload` (variant-B re-upload): the
///   call asserts the row's current `version_id` matches before
///   committing. Mismatch surfaces as `EtagMismatch` (the same error
///   variant covers etag-mismatch and version_id-mismatch — they are
///   both content-pin failures from the caller's perspective).
///
/// Combined with `etag`, the two pins are independent: `etag` checks
/// content fingerprint, `version_id` checks generation identity.
/// Passing both gives ABA-safe CAS even on bit-identical re-uploads.
/// When S3 versioning is not enabled `version_id` is always `None` on
/// the row; passing `Some(v)` to a CAS path on such a backend
/// surfaces `EtagMismatch` immediately (`None` ≠ `Some(v)` under
/// null-safe comparison).
pub type VersionId = String;

// ── Backend descriptors ─────────────────────────────────────────────────

/// One backend declared in the FileStorage roster (`GET /storages`).
/// The descriptor declares the role flags and the supported optional
/// capabilities. Versioning support is expressed through the
/// `*.versioned.*` capability tags rather than a separate flag;
/// FileStorage does NOT probe `GetBucketVersioning` at boot
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
    /// **public-read** files. Implies the `download.s3.public.*`
    /// capability — every file in this backend gets an eternal
    /// bare-HTTPS URL.
    pub default_public: bool,
    /// Versioned capability tags declared by the backend in the
    /// TOML roster (see `CapabilityTag`). Each tag has the shape
    /// `<operation>.<descriptor>+.v<n>` — for example
    /// `upload.s3.multipart.sigv4.v1`, `download.s3.sigv4.v1`,
    /// `download.s3.public.v1`. The list is validated against the
    /// SDK's `KNOWN_CAPABILITIES` whitelist at module init; unknown
    /// tags fail the boot.
    pub capabilities: Vec<CapabilityTag>,
    /// Per-backend hard ceiling on object size in bytes. Optional in
    /// the TOML roster — when omitted, FileStorage falls back to the
    /// S3 single-object maximum of **5 TiB** (`5 * 1024^4` =
    /// `5_497_558_138_880`), per
    /// <https://docs.aws.amazon.com/AmazonS3/latest/userguide/qfacts.html>.
    /// Operators can lower this for tenancy / quota reasons; raising
    /// it above the S3 hard cap has no effect because S3 itself
    /// rejects oversize PUTs.
    pub max_file_size_bytes: Option<u64>,
    /// Per-backend hard ceiling on aggregate user-metadata size in
    /// bytes. Optional — when omitted, FileStorage falls back to the
    /// S3 user-metadata budget of **2 KiB** (`2048`), per
    /// <https://docs.aws.amazon.com/AmazonS3/latest/userguide/UsingMetadata.html>
    /// ("Within the PUT request header, the user-defined metadata is
    /// limited to 2 KB in size."). The cap covers `Content-Type`,
    /// `Content-Disposition` (derived from `meta.name`), and every
    /// `x-amz-meta-<k>=<v>` mirrored entry; `gts_file_type` is DB-only
    /// and does NOT count toward this budget. Enforced at presign
    /// time and at `put_file_info`.
    pub max_metadata_bytes: Option<u64>,
    /// Per-backend hard ceiling on presigned-URL TTL in seconds.
    /// Optional — when omitted, FileStorage falls back to the AWS
    /// SigV4 maximum of **7 days** (`604_800` seconds), per
    /// <https://docs.aws.amazon.com/AmazonS3/latest/API/sigv4-query-string-auth.html>
    /// ("X-Amz-Expires: A maximum of 604800 (seven days)."). Operators
    /// can lower this for tenancy / security reasons; raising it above
    /// the AWS hard cap has no effect because S3 itself rejects
    /// signatures requesting longer expiries. Applied to every
    /// signing path (`upload.s3.multipart.sigv4.v1` part URLs,
    /// `download.s3.sigv4.v1`) — `download.s3.public.v1` is
    /// unaffected because public URLs carry no expiry. Caller's
    /// `UrlParams.expires_in_seconds` is capped by this value;
    /// exceeding it is a `BadRequest`.
    pub max_presign_ttl_seconds: Option<u64>,
    // Versioning support is declared per-backend through the
    // `*.versioned.*` capability tags (e.g.
    // `download.s3.sigv4.versioned.v1`,
    // `download.s3.public.versioned.v1`) — there is no separate
    // `Backend.versioning` flag. The DB `version_id` column is populated
    // from S3 response headers as-is; the null-safe race-detection
    // predicate works in both modes (see ADR-0005).
}

/// Versioned capability identifier — a flat string of the form
/// `<operation>.<protocol>.<algorithm>.<variant>?.v<n>`. Each tag
/// describes one (operation, protocol, signing-strategy, optional
/// modifier, version) tuple that a backend can serve. Examples:
/// `upload.s3.multipart.sigv4.v1`, `download.s3.sigv4.v1`,
/// `download.s3.sigv4.versioned.v1`, `download.s3.public.v1`,
/// `download.s3.public.versioned.v1`.
///
/// **Grammar.** Regex `^[a-z][a-z0-9_]+(\.[a-z][a-z0-9_]+){2,4}\.v\d+$`.
/// The leading segment is the SDK operation (`upload` / `download`).
/// The next segment is the protocol family (`s3`, `gcs`, `azure`, …).
/// The next is the signing/auth flavor (`sigv4`, `sigv4a`,
/// `sas_user`, `multipart`, `public` — the last denotes anonymous
/// bare-HTTPS, no signature). The optional fourth descriptor is a
/// variant modifier (currently only `versioned`, marking that the
/// tag accepts an optional `version_id` selector). The trailing
/// segment is the contract version (`v1`, `v2`, …).
///
/// **Why flat strings instead of an enum.** Each tag is constant data
/// from the SDK's perspective: when a new signing strategy or wire
/// protocol is added, it lands as a new string in
/// `KNOWN_CAPABILITIES` and a new branch in the adapter's `match` —
/// no enum migration, no breaking schema change. Operators write
/// tags as-is in TOML.
///
/// **Boot-time validation (constitutive).** The SDK ships with
/// `const KNOWN_CAPABILITIES: &[&str] = &[ ... ]` listing every tag
/// it knows how to sign. At module init, every tag in every
/// `Backend.capabilities` is checked against the whitelist; unknown
/// tags → fail-fast initialization with `unknown capability "{tag}"
/// on backend {id}`. Because of this, runtime code never has to
/// handle "unknown capability" — the impossibility is enforced at
/// boot. Adapters use `match tag.as_str()` with `_ => unreachable!()`
/// as a defensive default.
///
/// **P1 whitelist (5 tags).** These are the only tags shipped in P1
/// and cover every supported backend, including non-S3 endpoints
/// reached through S3-compat gateways (WebDAV, FTP, custom non-S3
/// clients).
/// - `upload.s3.multipart.sigv4.v1` — server-mediated chunked S3
///   multipart upload (`CreateMultipartUpload` + `UploadPart` +
///   `CompleteMultipartUpload`) signed with SigV4. The only upload
///   path; even single-byte files go through it as a one-part
///   session. Handler presigns N `UploadPart` URLs in one round trip
///   and exposes the companion REST endpoints
///   `POST /files/{file_id}/upload/{upload_id}` (commit) and
///   `DELETE /files/{file_id}/upload/{upload_id}` (abort one
///   session). Refers specifically to the chunked S3 multipart-upload
///   protocol, **not** to `multipart/form-data` POST Policy uploads.
///   There is no single-shot SigV4 PUT path.
/// - `download.s3.sigv4.v1` — SigV4-signed GET for time-limited
///   private downloads (TTL + optional CIDR allowlist). Rejects
///   `version_id` parameter (use the `versioned` variant for that).
/// - `download.s3.sigv4.versioned.v1` — version-aware SigV4-signed
///   GET. Identical to `download.s3.sigv4.v1` but accepts an optional
///   `version_id` selector embedded as `versionId=<vid>` in the
///   signed URL. Declared on backends fronting versioning-enabled
///   buckets that wish to expose history to consumers; declaring it
///   implies bucket S3 versioning is enabled.
/// - `download.s3.public.v1` — bare-HTTPS public-read URL with no
///   signature and no expiry (`public` in the algorithm slot denotes
///   the deliberate absence of a signing algorithm). For buckets with
///   public-read ACL or an origin behind a CDN; pairs with
///   `Backend.default_public`. Rejects `version_id` parameter.
/// - `download.s3.public.versioned.v1` — version-aware bare-HTTPS
///   public-read URL (`https://<host>/<key>?versionId=<vid>`, no
///   signature). Requires bucket policy to grant
///   `s3:GetObjectVersion` to the anonymous principal in addition to
///   `s3:GetObject` — without it, anonymous requests with
///   `?versionId=` return 403. Declaring it implies the operator has
///   confirmed both.
///
/// **Forward-looking examples (NOT planned for P1, P2, or P3).** The
/// following tags illustrate how the whitelist would extend if a
/// non-S3 cloud-native adapter were ever added; they are listed only
/// to demonstrate the naming scheme and may be appended at any time.
/// - `upload.gcs.resumable.v1` — GCS native resumable-upload session
///   (single session URL, sequential `Content-Range` PUTs).
/// - `upload.azure.blocks.sas_user.v1` — Azure Block Blob
///   (`PutBlock` + `PutBlockList`) with a User-Delegation SAS.
pub type CapabilityTag = String;

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
    /// `PUT /files/{id}` cannot change it.
    pub gts_file_type: String,
    /// Application-defined key/value tags
    /// (`cpt-cf-file-storage-fr-metadata-storage`). Mirrored as
    /// `x-amz-meta-<k>=<v>` on the S3 object. The aggregate 2 KB
    /// user-metadata budget is enforced by FileStorage.
    pub custom_metadata: CustomMetadata,
}

// `size_bytes` is system-managed and lives only on `FileInfo`
// (top-level). The caller never sets it; it is captured from S3's
// `Content-Length` at `complete_upload` (Phase 3). Keeping it off
// `FileMeta` removes the duplication / nullability mismatch the API
// shape used to carry.

/// Body for `put_file_info` — every field is optional. `Some(v)`
/// replaces the row's current value; `None` keeps it unchanged.
///
/// **Structurally immutable fields are NOT declared here**:
/// `gts_file_type`, `file_id`, `file_path` (the logical path /
/// file key inside the backend's tenant scope), `backend_id`,
/// `owner`, `size_bytes`, `etag`, `version_id`, timestamps.
/// **FileStorage does NOT support renaming files** — there is no
/// REST or SDK path that mutates `file_id` or `file_path`. Both
/// are captured at `create_presigned_upload` and remain fixed for
/// the file's lifetime. To "rename", the caller uploads a new
/// file at the desired `file_path` and deletes the old one — there
/// is no atomic move/rename surface. `name` here is the display
/// label (used for `Content-Disposition` on downloads), not an
/// identifier — it is mutable.
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
    /// Opaque file handle (UUID, per ADR-0002). **Immutable** —
    /// minted once at `create_presigned_upload` time and fixed for
    /// the file's lifetime. There is no REST or SDK path that
    /// changes `file_id`.
    pub file_id: FileId,
    pub backend_id: BackendId,
    /// Logical path inside the backend's tenant scope, captured
    /// at upload time. Used for filtering / listing; not part of
    /// the URL. **Immutable — files cannot be renamed.** There is
    /// no REST or SDK path that mutates `file_path`. To change
    /// the logical address, callers upload a new file at the
    /// desired `file_path` and delete the old one — FileStorage
    /// has no atomic move/rename surface.
    pub file_path: String,
    pub owner: OwnerRef,
    pub meta: FileMeta,
    pub status: FileStatus,
    /// Raw S3 ETag for the current bytes (sans surrounding quotes).
    /// Content fingerprint only.
    pub etag: Etag,
    /// Raw S3 VersionId for the current generation. `Some` when the
    /// hosting backend has S3 versioning enabled; `None` otherwise.
    pub version_id: Option<VersionId>,
    pub size_bytes: u64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    /// `Some` while `status == PendingUpload` — the presigned URL
    /// TTL. `MAX`-merged across multiple variant-B re-upload presigns.
    pub upload_expires_at: Option<OffsetDateTime>,
}

/// Lifecycle states for a file row in the FileStorage database.
///
/// ```text
///   ┌──────────────────┐  complete       ┌──────────────────┐
///   │  PendingUpload   │── (Phase 1) ───▶│    Completing    │
///   └────────┬─────────┘                 └────────┬─────────┘
///            │ GC sweep                           │ S3 CompleteMultipartUpload
///            │ (upload_expires_at < NOW)          │ + final UPDATE (Phase 3)
///            │                                    ▼
///            │                           ┌──────────────────┐
///            │                           │     Uploaded     │ ◀── re-upload
///            │                           └────────┬─────────┘     completes
///            │                                    │
///            │           ┌─────── PUT /meta ──────┼────── delete_file ───┐
///            │           │ (Phase 1)              │ (Phase 1)            │
///            │           ▼                        │                      ▼
///            │  ┌──────────────────┐              │             ┌──────────────────┐
///            │  │   MetaUpdating   │              │             │     Deleting     │
///            │  └────────┬─────────┘              │             └────────┬─────────┘
///            │           │ S3 CopyObject self-copy│                      │ S3 DeleteObject
///            │           │ + final UPDATE         │                      │ + row purge
///            │           │ (Phase 3)              │                      │ (Phase 3)
///            │           ▼                        │                      │
///            │       (back to Uploaded) ──────────┘                      │
///            ▼                                                           ▼
///                                ⟨ row purged ⟩
/// ```
///
/// **Three transient states** correspond to the three multi-phase
/// commit paths over (DB row, S3 object): `Completing` for upload
/// finalization, `MetaUpdating` for `PUT /meta` atomic DB+S3 sync,
/// `Deleting` for the 2-phase delete. None of them are soft-delete
/// tombstones (`cpt-cf-file-storage-constraint-no-soft-delete`).
///
/// **Recovery from a transient state** — when an SDK call (e.g.
/// `read_file`, `complete_upload`, `put_file_info`) encounters a row
/// stuck in `Completing` / `MetaUpdating` / `Deleting`, it triggers
/// in-band recovery: HEAD the backend, pull the authoritative state,
/// finalize the row (or re-execute the pending phase) before serving
/// the original request. External direct-to-S3 reads via presigned
/// URLs do NOT trigger recovery — they read whatever bytes are at
/// the backend object, which is consistent regardless of the DB
/// row's transient status. The GC sweep (P2) is the safety net for
/// rows whose owning client crashed mid-flow and never returned.
///
/// The `PendingUpload → purged` arrow models the P2 GC sweep
/// harvesting upload sessions whose presigned URL TTL expired before
/// the client called `complete_upload` (DESIGN §3.6 GC and orphans).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    /// Row inserted by the initial `presign-batch` upload item
    /// without `file_id`; bytes have not been confirmed yet.
    PendingUpload,
    /// Phase 1 of `complete_upload` flipped the row. Phase 2
    /// invokes `CompleteMultipartUpload` on the backend; Phase 3
    /// flips the row to `Uploaded` with the etag / version_id from
    /// the backend's response. The same state covers re-upload —
    /// `Uploaded → Completing → Uploaded` when the caller passes
    /// an existing `file_id` to `create_presigned_upload`. A row
    /// that survives in `Completing` (handler crash between Phase 2
    /// and Phase 3) is recovered via HEAD-and-pull on the next
    /// SDK call against it.
    Completing,
    /// Phase 1 of `put_file_info` flipped the row's STATUS only —
    /// `name` / `mime_type` / `custom_metadata` columns still hold
    /// the OLD values. Phase 2 issues `CopyObject` self-copy with
    /// `MetadataDirective: REPLACE` against the backend, carrying
    /// the merged new metadata. Phase 3 flips the row back to
    /// `Uploaded` AND writes the new
    /// `(name, mime_type, custom_metadata, etag, version_id)` in
    /// one conditional UPDATE — atomically. A row that survives in
    /// `MetaUpdating` (handler crash between Phase 2 and Phase 3)
    /// is recovered in-band by the next SDK call: HEAD the
    /// backend, pull whatever metadata `CopyObject` already wrote,
    /// run Phase 3 with that. The DB never holds new metadata
    /// under a "still updating" status — the row is either fully
    /// old (status `MetaUpdating`, columns unchanged) or fully new
    /// (status `Uploaded` with new metadata).
    MetaUpdating,
    /// Bytes finalized via `complete_upload`. The only state in
    /// which `read_file` / `presign_urls` (download) succeed.
    Uploaded,
    /// Phase 1 of `delete_file` flipped the row. Subsequent
    /// `complete_upload` / `put_file_info` / `delete_file` against
    /// the row return `DeleteInProgress`; reads return `NotFound`.
    /// Phase 3 hard-deletes the row after Phase 2 reaps the backend
    /// object.
    Deleting,
}

// ── Presigned URLs ──────────────────────────────────────────────────────

/// Knobs the caller wants applied to a presigned URL.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlParams {
    /// Requested TTL. Capped server-side by the backend's configured
    /// maximum; exceeding the cap is a `BadRequest`. Ignored for
    /// `download.s3.public.*` outcomes — public URLs have no expiry.
    /// On `upload.s3.multipart.sigv4.v1` the same TTL applies uniformly to
    /// every part URL in the issued batch.
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

/// Result of `create_presigned_upload` — the upload presign path
/// implemented in P1. Every P1 upload uses S3 multipart and is
/// multipart-shaped (single-part for small files), so the handle
/// always carries a backend-supplied `upload_id` and a list of part
/// URLs. Alternative upload tags (e.g. `upload.gcs.resumable.v1`,
/// `upload.azure.blocks.sas_user.v1`) are reserved by the naming
/// scheme but not implemented in any phase.
///
/// The server has already executed `CreateMultipartUpload` against
/// the backend and presigned every part URL — the caller does not
/// negotiate the multipart session with the backend directly. After
/// the client uploads each part it commits via
/// `POST /files/{file_id}/upload/{upload_id}` (or aborts
/// via `DELETE /files/{file_id}/upload/{upload_id}`).
///
/// **`upload_id` is NOT persisted by FileStorage.** The caller MUST
/// keep it for `complete` / `abort`. If lost, the row times out at
/// `upload_expires_at` and the bucket's
/// `AbortIncompleteMultipartUpload` lifecycle rule reaps the orphan
/// multipart session on the backend side.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresignedUploadHandle {
    pub file_id: FileId,
    /// Backend-supplied multipart session id (S3 `UploadId`).
    pub upload_id: String,
    /// Presigned PUT URLs, one per part, in `part_number` order
    /// (1-indexed). Length equals the `part_count` requested by the
    /// caller in the presign item.
    pub part_urls: Vec<UploadPartUrl>,
    /// Common expiry across every part URL (uniform TTL from the
    /// caller's `UrlParams.expires_in_seconds`).
    pub expires_at: OffsetDateTime,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadPartUrl {
    pub part_number: u32,
    pub url: String,
}

/// Caller-supplied entry of a `complete_upload` body. `etag` is
/// whatever the backend returned in the `ETag` header of the
/// corresponding `UploadPart` response — opaque to FileStorage,
/// passed straight to `CompleteMultipartUpload`.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadPartCompletion {
    pub part_number: u32,
    pub etag: String,
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
/// hosting backend has S3 versioning enabled. When set, the server
/// includes `versionId=<vid>` in the signed URL so the caller fetches
/// a historical generation. When unset, the URL resolves to the
/// current bytes.
///
/// `capability` is the versioned tag the caller wants (e.g.
/// `download.s3.sigv4.v1` or `download.s3.public.v1`). The server
/// rejects with `capability_unavailable` if the resolved backend has
/// not declared that tag.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresignDownloadItem {
    pub file_id: FileId,
    pub capability: CapabilityTag,
    pub params: UrlParams,
    /// Optional CAS pin: when `Some`, server verifies the row's
    /// current `etag` matches (DB-only check) before signing —
    /// mismatch returns per-item `EtagMismatch`.
    pub etag: Option<Etag>,
    /// Two roles, depending on what's pinned:
    ///
    /// - **Historical selector.** When S3 versioning is enabled,
    ///   the signed URL embeds `versionId=<vid>` so the caller
    ///   fetches that exact generation. Ignored on
    ///   backends without S3 versioning.
    /// - **CAS pin** (when paired with `etag`). When both are
    ///   `Some`, the server verifies the row's current
    ///   `(etag, version_id)` matches — closes the ABA window on
    ///   bit-identical re-uploads.
    pub version_id: Option<VersionId>,
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
    /// `download.s3.public.v1` outcomes (`is_public == true`) this is
    /// set to a far-future sentinel ("never expires").
    pub expires_at: OffsetDateTime,
    /// `true` when the URL is a bare-HTTPS public-read URL issued
    /// under the `download.s3.public.v1` capability tag (no presigning,
    /// no expiry). `false` for SigV4 GET URLs from
    /// `download.s3.sigv4.v1`.
    pub is_public: bool,
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

/// Byte-range selector for partial reads on `read_file`. Maps 1:1
/// onto the HTTP `Range: bytes=...` header (RFC 7233), which every
/// S3-class backend honours on `GetObject` (constitutive S3 feature
/// — does not require a capability tag). Range is constructed at
/// the SDK boundary; the adapter forwards it as-is to the backend.
///
/// The bytes streamed back through `FileReadHandle.bytes` cover
/// **only** the requested range; `FileReadHandle.info` still
/// reflects the FULL object metadata (`size_bytes`, `etag`,
/// `version_id`). The actual resolved range (what the backend
/// returned, including the full object size) is exposed on
/// `FileReadHandle.range` as `ResolvedByteRange`.
///
/// **Semantics on bit-identical re-reads.** The backend's `ETag`
/// header on a range response is the FULL object ETag, not a
/// per-range hash; this is what lets in-band reconciliation
/// (ADR-0004) keep working unchanged on range reads.
///
/// **Out-of-range** (e.g. `Inclusive { start: u, end: v }` with
/// `u > total_size`) surfaces as `FileStorageError::BadRequest` —
/// the backend returns `416 Range Not Satisfiable` and the SDK
/// maps it onto `BadRequest` with a message that includes the
/// actual object size.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRange {
    /// `Range: bytes=START-END` — both bounds inclusive. Validated
    /// at the SDK boundary: `start > end` is a `BadRequest` before
    /// the backend is contacted.
    Inclusive { start: u64, end: u64 },
    /// `Range: bytes=START-` — from offset to end of object.
    From(u64),
    /// `Range: bytes=-N` — last N bytes of the object (suffix
    /// range). Useful for footer reads (Parquet, ZIP central
    /// directory, video moov-atom). `N == 0` is `BadRequest`.
    Suffix(u64),
}

/// Resolved byte range that the backend actually served, captured
/// from the `Content-Range: bytes START-END/TOTAL` response header.
/// Returned on `FileReadHandle.range` whenever a range was
/// requested; lets the caller know the exact served diapason and
/// the full object size in one shot.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedByteRange {
    /// First byte of the served range (inclusive).
    pub start: u64,
    /// Last byte of the served range (inclusive).
    pub end_inclusive: u64,
    /// Full object size in bytes (the `TOTAL` part of
    /// `Content-Range`). Equal to `FileInfo.size_bytes`, repeated
    /// here so callers do not have to thread the snapshot to
    /// downstream consumers.
    pub total: u64,
}

/// Reader-side handle returned by `read_file`. The caller polls
/// `bytes` to receive chunks and treats the stream end as EOF; the
/// `info` snapshot is cheap to clone and lets callers inspect MIME,
/// size, etc. without an extra round-trip.
///
/// When the call requested a partial read (`range = Some(_)`), the
/// streamed bytes cover only the requested diapason and `range` is
/// populated with the resolved bounds and total object size; `info`
/// still reflects the FULL object (size, etag, version_id).
#[derive(Debug)]
pub struct FileReadHandle {
    pub info: FileInfo,
    pub bytes: FileByteStream,
    /// `Some` iff the caller passed `range = Some(_)`. Mirrors the
    /// HTTP `Content-Range` header from the backend (`start`,
    /// `end_inclusive`, `total`). On full reads (`range = None` on
    /// the call) this is `None`.
    pub range: Option<ResolvedByteRange>,
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
    /// fields, invalid GTS file type, `part_count` out of range,
    /// `meta` semantics violation on a re-upload presign item, etc.).
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
    /// flow). Surfaced by `complete_upload`, `put_file_info`,
    /// `delete_file` when they encounter such a row; reads return
    /// `NotFound`.
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
    /// `complete_upload` finalized the row.
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
/// 2. It calls `create_presigned_upload(... file_id = None ...)` and
///    receives a `PresignedUploadHandle { file_id, upload_id,
///    part_urls, expires_at }`. FileStorage has persisted a
///    `PendingUpload` row and called `CreateMultipartUpload` on the
///    backend; the multipart session id (`upload_id`) is opaque
///    to FileStorage and travels back through the client.
/// 3. The end-client `PUT`s each part directly to its corresponding
///    URL (SigV4 PUT against the S3-compatible backend). For small
///    files the caller requests `part_count = 1` and uploads the
///    whole payload as a single last-part.
/// 4. The application's backend confirms the upload by calling
///    `complete_upload(file_id, upload_id, parts)`. FileStorage
///    flips the row to the transient `Completing` state (Phase 1),
///    issues `CompleteMultipartUpload` against the backend (Phase 2),
///    captures the finalized etag / version_id from the backend's
///    response, and conditionally UPDATEs the row to `Uploaded`
///    (Phase 3) — all in a single SDK call. There is **no separate
///    reconcile step**.
/// 5. From here on every consumer references the file by `file_id`
///    (and an optional `etag` for optimistic concurrency).
///
/// **Re-uploading the same file** — the application's backend calls
/// `create_presigned_upload(... file_id = Some(id) ...)`. The
/// server reads the row's current metadata (`name`, `mime_type`,
/// `custom_metadata`), starts a fresh multipart session against
/// the same backend object key, and presigns part URLs with the
/// row's pinned headers. After the client uploads parts and calls
/// `complete_upload`, the row transitions `Uploaded → Completing
/// → Uploaded` and its etag / version_id rotate to the new bytes;
/// metadata is unchanged.
///
/// **Changing metadata** — `put_file_info(file_id, update, etag?)`.
/// Two-phase commit: the server flips the row to `MetaUpdating`
/// (Phase 1), issues `CopyObject` self-copy with `MetadataDirective:
/// REPLACE` to synchronize S3 user-metadata and `Content-Type` /
/// `Content-Disposition` to the new values (Phase 2), then UPDATEs
/// the DB to `Uploaded` with the new etag / version_id from the
/// `CopyObject` response (Phase 3). Optional `If-Match` becomes a
/// strong CAS over both DB and S3.
///
/// All methods that take an `etag` argument honour it as
/// "proceed only if the row's current etag matches" — the SDK
/// returns `EtagMismatch` otherwise. Methods that take
/// `Option<&Etag>` make the check optional; passing `None` means
/// "I trust whatever I get".
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

    // ── Upload lifecycle ────────────────────────────────────────────

    /// **The single upload entry-point.** Server-mediated multipart
    /// initiation: validates input, registers (or reuses) a row,
    /// calls `CreateMultipartUpload` on the backend, presigns one
    /// PUT URL per part, and returns the bundle in a single round
    /// trip.
    ///
    /// **Two modes (discriminated by `file_id`).**
    ///
    /// - `file_id = None` — **initial upload.** Server mints a
    ///   fresh `file_id`, derives the backend object key
    ///   (`file_path`) deterministically from it, INSERTs a row
    ///   with `status = PendingUpload`, and starts a multipart
    ///   session. `owner` and `meta` are required. `backend_id`
    ///   is optional and falls back to the caller's tenant
    ///   `default_private` backend. Logical-address uniqueness
    ///   is structural — the PRIMARY KEY on `id` is sufficient,
    ///   no separate path-level index is required.
    /// - `file_id = Some(id)` — **re-upload (overwrite in place).**
    ///   The row MUST exist and be in `Uploaded` (else `NotFound`
    ///   for `Deleting` / `PendingUpload`, or appropriate transient
    ///   status). Server flips the row to `Completing` (Phase 1 of
    ///   the upload commit, reused for re-uploads), reads the row's
    ///   CURRENT metadata and pins those exact values into the
    ///   presigned part URLs. **`owner`, `meta`, `backend_id` are
    ///   ignored** when `file_id` is supplied — to change metadata,
    ///   call `put_file_info` either before `create_presigned_upload`
    ///   or after `complete_upload`. The optional `etag` and
    ///   `version_id` arguments act as variant-B CAS pins:
    ///   - `etag = Some(e)` → mismatch fails the call with
    ///     `EtagMismatch` *before* `CreateMultipartUpload` is
    ///     issued (the row is not flipped to `Completing`).
    ///   - `version_id = Some(v)` (when S3 versioning is enabled) →
    ///     mismatch likewise fails with `EtagMismatch` (closes the
    ///     ABA window even when bytes are bit-identical and the
    ///     etag did not rotate). When S3 versioning is not enabled
    ///     `version_id = Some(_)` is a null-safe mismatch against
    ///     the row's `None` and returns `EtagMismatch` immediately.
    ///   - Both pins compose. Both default to `None` (last-write-
    ///     wins re-upload).
    ///
    /// **Pinned headers (always, every part URL).** SigV4
    /// SignedHeaders pin:
    /// - `Content-Type` = `meta.mime_type`
    /// - `Content-Disposition` = `attachment; filename="<URL-encoded meta.name>"`
    /// - `x-amz-meta-<k>` = `<v>` for each entry in `meta.custom_metadata`
    ///
    /// **`gts_file_type` is NOT pinned to the backend object** — it
    /// is DB-only (specific exception to the meta-mirror rule).
    ///
    /// **`capability`** MUST be `"upload.s3.multipart.sigv4.v1"`. Validated
    /// against the resolved backend's `capabilities` list — unknown
    /// / undeclared tag → `CapabilityUnavailable`.
    ///
    /// **`part_count`** (1..=10000 per S3's hard cap on
    /// multipart-parts-per-object). Caller is responsible for
    /// choosing a part size consistent with the backend's minimum
    /// (5 MiB on AWS S3 except the final part — single-part
    /// uploads use one part as the final part, which has no minimum
    /// size). FileStorage does not pick `part_size` and does not
    /// retroactively extend a session: if the file turns out
    /// larger than the initial `part_count` covers, the caller
    /// aborts and re-inits.
    ///
    /// **`params.expires_in_seconds`** is a uniform TTL across every
    /// part URL and the deadline for `complete_upload` /
    /// `abort_upload`.
    ///
    /// **`upload_id` is NOT persisted by FileStorage.** Returned in
    /// `PresignedUploadHandle.upload_id`; the caller MUST keep it
    /// for `complete_upload` / `abort_upload`. If lost, the row
    /// times out at `upload_expires_at` and the bucket-level
    /// `AbortIncompleteMultipartUpload` lifecycle rule reaps the
    /// orphan multipart session on the backend.
    ///
    /// The owner's `tenant_id` MUST match the security context's
    /// tenant; otherwise the call returns `AccessDenied`.
    async fn create_presigned_upload(
        &self,
        ctx: &SecurityContext,
        backend_id: Option<BackendId>,
        file_id: Option<FileId>,
        owner: OwnerRef,
        meta: FileMeta,
        capability: &CapabilityTag,
        part_count: u32,
        params: UrlParams,
        etag: Option<&Etag>,
        version_id: Option<&VersionId>,
    ) -> Result<PresignedUploadHandle, FileStorageError>;

    /// **Complete an upload (initial or re-upload).** In P1 this is
    /// invoked in-process; in P2 it also backs
    /// `POST /files/{file_id}/upload/{upload_id}`.
    /// Multi-phase commit:
    ///
    /// 1. **Phase 1.** Conditional UPDATE flips the row from
    ///    `PendingUpload` (or `Uploaded`, for re-upload) to the
    ///    transient `Completing` state, capturing the pre-existing
    ///    `(etag, updated_at, version_id[, xmin])` for race detection (`version_id` participates always — null-safe via `IS NOT DISTINCT FROM`; `xmin` adds Postgres-only transaction-id race detection).
    /// 2. **Phase 2.** Calls `CompleteMultipartUpload` on the
    ///    backend with the supplied `(part_number, etag)` list,
    ///    captures the finalized object's etag and `version_id`.
    /// 3. **Phase 3.** Conditional UPDATE flips `Completing →
    ///    Uploaded` and writes the new `(etag, version_id,
    ///    size_bytes)` along with `upload_expires_at = NULL`. `0`
    ///    rows → row was concurrently abort/deleted → return the
    ///    appropriate error.
    ///
    /// `upload_id` is the value previously returned by
    /// `create_presigned_upload`; FileStorage forwards it to the
    /// backend and never persists it.
    ///
    /// **Recovery from a stuck `Completing` row.** If a previous
    /// call crashed between Phase 2 and Phase 3, the row sits in
    /// `Completing`. The next `complete_upload` call against the
    /// same `(file_id, upload_id)` HEADs the backend; if the object
    /// already finalized successfully, the SDK pulls the etag and
    /// runs Phase 3 alone. If the multipart session is still live,
    /// the SDK re-runs Phase 2 (`CompleteMultipartUpload` is
    /// idempotent on `(key, upload_id, parts)` until the session
    /// is closed by another path).
    ///
    /// **Errors.** A missing row returns `NotFound`; a row in
    /// `Deleting` returns `DeleteInProgress`; a row in
    /// `MetaUpdating` returns `Conflict` (concurrent metadata
    /// mutation in progress).
    async fn complete_upload(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        upload_id: &str,
        parts: Vec<UploadPartCompletion>,
    ) -> Result<FileInfo, FileStorageError>;

    /// **Abort an upload session.** In P1 invoked in-process; in P2
    /// also backs `DELETE /files/{file_id}/upload/{upload_id}`. The
    /// server calls `AbortMultipartUpload` on the backend for the
    /// specified `upload_id` only.
    ///
    /// **Does NOT remove the file row.** Multiple upload sessions
    /// may run in parallel against the same `file_id` (retry of a
    /// stalled session, concurrent client attempts), and aborting
    /// one of them must not affect the others. To drop the file
    /// entirely after aborting, the caller MUST follow up with
    /// `delete_file(file_id, …)` (the canonical "remove this file"
    /// surface); alternatively the row times out at
    /// `upload_expires_at` and the GC sweep removes it.
    ///
    /// **`Completing → PendingUpload` rollback.** When `abort_upload`
    /// runs against a row that the caller previously flipped to
    /// `Completing` via `complete_upload` (and that call failed
    /// before reaching Phase 3), this aborts that pending finalize
    /// and rolls the row back to `PendingUpload`. A row already in
    /// `Uploaded` is rejected (`Conflict`) — abort is not a "delete
    /// already-committed file" surface; use `delete_file` for that.
    async fn abort_upload(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        upload_id: &str,
    ) -> Result<(), FileStorageError>;

    // ── File lookups ────────────────────────────────────────────────

    /// `GET /api/file-storage/v1/files/{file_id}` — returns the
    /// authoritative metadata view from the FileStorage database
    /// without touching the storage backend.
    ///
    /// `etag` and `version_id` are optional CAS pins:
    /// - `etag = Some(e)` → returns `EtagMismatch` if the row's etag
    ///   has rotated.
    /// - `version_id = Some(v)` → when S3 versioning is enabled, returns
    ///   `EtagMismatch` if the row's `version_id` has rotated
    ///   (null-safe — `Some(v)` ≠ `None` if the backend has
    ///   S3 versioning not enabled).
    /// Both pins compose: passing both gives ABA-safe CAS over
    /// content + generation. The REST equivalent supports
    /// `If-None-Match` → `304 Not Modified` on the etag side.
    async fn get_file_info(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Option<&Etag>,
        version_id: Option<&VersionId>,
    ) -> Result<FileInfo, FileStorageError>;

    /// `PUT /api/file-storage/v1/files/{file_id}` — atomic
    /// DB+S3 metadata sync via a 2-phase commit. Body fields are
    /// optional — `Some(v)` replaces, `None` keeps the existing
    /// value. The update body declares only `name`, `mime_type`,
    /// and `custom_metadata`; **`gts_file_type` is structurally not
    /// declared** — it is immutable.
    ///
    /// **Multi-phase commit.**
    ///
    /// 1. **Phase 1.** Conditional UPDATE flips the row's STATUS
    ///    from `Uploaded` to the transient `MetaUpdating` state,
    ///    capturing `(etag, updated_at, version_id[, xmin])` for race detection (`version_id` participates always — null-safe via `IS NOT DISTINCT FROM`; `xmin` adds Postgres-only transaction-id race detection). The
    ///    `name` / `mime_type` / `custom_metadata` columns still
    ///    hold the OLD values — Phase 1 does NOT write the new
    ///    metadata into the row. `0` rows → the row moved
    ///    underneath us; retry up to 3 times before surfacing
    ///    `Conflict`. A row in `Deleting` / `PendingUpload` /
    ///    `Completing` returns the appropriate error without
    ///    entering the phase.
    /// 2. **Phase 2.** Server merges the update against the row's
    ///    current metadata, validates the backend's
    ///    `max_metadata_bytes` budget (default 2 KiB), then issues a
    ///    `CopyObject` with `CopySource = derive(file_id)`,
    ///    `MetadataDirective = REPLACE`, and the new `Content-Type`,
    ///    `Content-Disposition`, and `x-amz-meta-<k>` headers. The
    ///    backend's response carries the new `ETag` and (if
    ///    S3 versioning is enabled) the new `VersionId`.
    /// 3. **Phase 3.** Conditional UPDATE flips `MetaUpdating →
    ///    Uploaded` AND writes the new
    ///    `(name, mime_type, custom_metadata, etag, version_id)`
    ///    in one statement — the metadata columns and the status
    ///    transition land atomically. The row never holds new
    ///    metadata under a "still updating" status.
    ///
    /// **Optional `etag` / `version_id` = strong CAS on DB + S3.**
    ///
    /// - `etag = Some(E1)`:
    ///   - Phase 1's UPDATE includes `etag = E1` (412 on mismatch).
    ///   - Phase 2 HEADs S3 to verify the live `s3_etag` matches the
    ///     row (412 on mismatch).
    ///   - The `CopyObject` is issued with
    ///     `x-amz-copy-source-if-match: E1` for backend-side
    ///     precondition enforcement.
    /// - `version_id = Some(V1)` (when S3 versioning is enabled):
    ///   - Phase 1's UPDATE includes `version_id IS NOT DISTINCT FROM V1`
    ///     (412 on mismatch).
    ///   - Phase 2's HEAD additionally verifies `s3_version_id == V1`
    ///     (412 on mismatch — closes the ABA race per ADR-0005, even
    ///     when bytes are bit-identical and `etag` did not rotate).
    ///   - When S3 versioning is not enabled `version_id = Some(_)`
    ///     unconditionally returns 412 (the row's `version_id` is
    ///     `None`, the pin is `Some(v)` — null-safe mismatch).
    /// - Both pins compose; the strongest CAS uses both.
    ///
    /// When BOTH are `None` the call is best-effort
    /// last-write-wins on metadata
    /// (`cpt-cf-file-storage-constraint-no-meta-cas`).
    ///
    /// **Recovery from a stuck `MetaUpdating` row.** If a previous
    /// call crashed between Phase 2 and Phase 3, the row sits in
    /// `MetaUpdating` with the OLD metadata in its columns. The
    /// next SDK call against the row HEADs the backend; whatever
    /// metadata `CopyObject` already wrote (the new values, if
    /// Phase 2 landed; the old values, if it did not) is pulled
    /// into the row and Phase 3 flips the row back to `Uploaded`
    /// with that metadata. The DB always converges to the
    /// backend's truth.
    async fn put_file_info(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        update: FileMetaUpdate,
        etag: Option<&Etag>,
        version_id: Option<&VersionId>,
    ) -> Result<FileInfo, FileStorageError>;

    /// `DELETE /api/file-storage/v1/files/{file_id}` — 2-phase hard
    /// delete in P1.
    ///
    /// `etag` and `version_id` are optional CAS pins. When supplied,
    /// Phase 1's conditional UPDATE includes them — protects against
    /// deleting a file that has been overwritten between read and
    /// delete. When both are omitted, the delete is best-effort
    /// last-write-wins (still gated by `status = 'uploaded'`).
    ///
    /// **Phases**:
    ///
    /// 1. Conditional UPDATE: `SET status = 'deleting' WHERE id =
    ///    $1 AND status = 'uploaded' [AND etag = $2]
    ///    [AND version_id IS NOT DISTINCT FROM $3]`. `0` rows →
    ///    `EtagMismatch` (when either pin supplied) or `NotFound`
    ///    (neither). Row already in `Deleting` → `DeleteInProgress`.
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
    /// `complete_upload` / `put_file_info` / `delete_file` against
    /// a `Deleting` row return `DeleteInProgress`.
    async fn delete_file(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Option<&Etag>,
        version_id: Option<&VersionId>,
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
    /// proxies content over its REST surface. `read_file` exists
    /// for in-process consumers that share the FileStorage runtime
    /// (antivirus / llm-gateway / file-parser) and benefit from
    /// the in-memory adapter handle (no extra hop, snapshot-isolated
    /// stream, in-band recovery for rows stuck in transient states).
    ///
    /// **`etag` (CAS pin)** is optional: when supplied, the stream
    /// is opened only if the row's `etag` currently matches;
    /// otherwise `EtagMismatch`. This is the equivalent of `If-Match`
    /// and is intended for in-process modules that pinned an etag
    /// earlier and want to fail fast on drift.
    ///
    /// **`version_id` (historical selector + optional CAS)** is
    /// optional and has two roles, depending on what's pinned:
    ///
    /// - **Historical selector.** When S3 versioning is enabled,
    ///   `version_id = Some(v)` opens the stream against that exact
    ///   S3 generation (`GetObject?versionId=v`); the bytes
    ///   correspond to the historical version, while the returned
    ///   `FileInfo` still reflects the row's CURRENT metadata (the
    ///   DB tracks current generation only — see ADR-0005). On
    ///   backends without S3 versioning `version_id = Some(_)`
    ///   returns `EtagMismatch` (the row's `version_id` is `None`,
    ///   the pin is `Some(v)` — null-safe mismatch).
    /// - **CAS pin (when paired with `etag`).** Passing both
    ///   `etag` and `version_id` checks both axes against the row
    ///   before opening the stream — closes the ABA window on
    ///   bit-identical re-uploads.
    ///
    /// **In-band recovery for transient states.** The SDK is the
    /// only surface that can recover a row stuck in `Completing` /
    /// `MetaUpdating` — direct-to-S3 reads via presigned URLs do
    /// not have access to the row state and read whatever bytes
    /// are at the backend object. When `read_file` encounters a
    /// row in:
    ///
    ///   - **`Completing`** — the SDK HEADs the backend; if the
    ///     object exists with a final etag, the row is rolled
    ///     forward (Phase 3 of the upload commit), and the open
    ///     proceeds against the row's now-final etag. If the
    ///     backend HEAD reports `404`, the row is rolled back to
    ///     `PendingUpload` and `read_file` returns `NotFound`.
    ///   - **`MetaUpdating`** — the SDK HEADs the backend, pulls
    ///     the authoritative `etag` / `version_id` / metadata,
    ///     and runs the Phase-3 UPDATE that flips the row back to
    ///     `Uploaded`. The original `read_file` request is then
    ///     served against the now-consistent row.
    ///   - **`Deleting`** — `DeleteInProgress` is returned
    ///     immediately; no recovery is attempted (delete is
    ///     irreversible).
    ///
    /// Recovery UPDATEs run in system-context (no `SecurityContext`
    /// consulted) per
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
    /// **`range` (partial read)** is optional and maps directly to
    /// the HTTP `Range: bytes=...` header on the backend
    /// `GetObject`. Range is a constitutive S3 feature — no
    /// capability tag is required, every supported backend honours
    /// it. When `Some`, `FileReadHandle.bytes` carries only the
    /// requested diapason and `FileReadHandle.range` mirrors the
    /// `Content-Range` header from the backend (`start`,
    /// `end_inclusive`, `total`). `FileReadHandle.info` still
    /// reflects the FULL object metadata (`size_bytes`, `etag`,
    /// `version_id`) — the row is not split per range; CAS pins on
    /// `etag` / `version_id` apply to the full object regardless of
    /// range. Out-of-range requests surface as `BadRequest`. Common
    /// uses: video / audio streaming with seek, parallel multi-range
    /// download, footer reads (Parquet, ZIP, MP4 moov-atom).
    ///
    /// Typical pattern in an in-process consumer:
    ///
    /// ```ignore
    /// use futures::StreamExt;
    /// use sha2::{Digest, Sha256};
    /// use tokio::io::AsyncWriteExt;
    ///
    /// let mut handle = files.read_file(&ctx, file_id, Some(&etag), None, None).await?;
    /// let mut hasher = Sha256::new();
    /// let mut sink = tokio::fs::File::create("/tmp/scan.bin").await?;
    /// while let Some(chunk) = handle.bytes.next().await {
    ///     let chunk = chunk?;
    ///     hasher.update(&chunk);
    ///     sink.write_all(&chunk).await?;
    /// }
    ///
    /// // Footer-only partial read (last 64 KiB):
    /// let footer = files.read_file(
    ///     &ctx, file_id, None, None,
    ///     Some(ByteRange::Suffix(65_536)),
    /// ).await?;
    /// assert!(footer.range.is_some());
    /// ```
    async fn read_file(
        &self,
        ctx: &SecurityContext,
        file_id: FileId,
        etag: Option<&Etag>,
        version_id: Option<&VersionId>,
        range: Option<ByteRange>,
    ) -> Result<FileReadHandle, FileStorageError>;

    /// In-process SDK only — **no REST surface in P1**. Single-call
    /// upload for in-process callers (test fixtures, modules that
    /// generate content, antivirus storing a cleaned copy, etc.).
    ///
    /// **Fully implemented in P1.** Does NOT mint presigned URLs:
    /// in-process callers share the FileStorage runtime, so the SDK
    /// drives the adapter directly. **No single-shot `PutObject`
    /// path** — every in-process upload uses a multipart session
    /// (one part of arbitrary size for small payloads, last-part
    /// rule; N parts for larger payloads), the same shape the
    /// presigned path uses externally. The row goes through the same
    /// `PendingUpload → Completing → Uploaded` transitions as the
    /// REST path. There is no bytes-through-FileStorage REST proxy
    /// in any phase; external frontends always use the
    /// `create_presigned_upload` → client-direct PUT (per part) →
    /// `complete_upload` triad and ship bytes straight to the
    /// backend over the presigned URLs. `put_file` is the
    /// in-process equivalent that compresses the same lifecycle
    /// into one async call without the presign roundtrip.
    ///
    /// **Lifecycle.**
    /// 1. Mint `file_id`, derive the backend object key
    ///    (`file_path`) deterministically from it, INSERT row with
    ///    `status = PendingUpload`, sentinel `etag`,
    ///    `upload_expires_at = NOW + TTL`, supplied `owner`/`meta`.
    ///    For variant-B re-upload (`file_id = Some(id)`, `etag = Some(e)`),
    ///    SELECT the existing row by `id`, verify `etag = e` (412 on
    ///    mismatch), reuse its `file_path` and pin its current meta.
    /// 2. Start a multipart session against `derive(file_id)` via
    ///    `S3Backend::create_multipart_and_presign_parts` (the SDK
    ///    just consumes the resulting per-part URLs in-process —
    ///    no presign hop to the caller). Pin `Content-Type`,
    ///    `Content-Disposition`, and `x-amz-meta-<k>` (NOT
    ///    `x-amz-meta-gts-file-type`). Pull bytes off the input
    ///    stream and `UploadPart` them one part at a time; collect
    ///    `(part_number, etag)` pairs.
    /// 3. Run the same 3-phase commit as `complete_upload`:
    ///    Phase 1 conditional UPDATE flips `pending_upload →
    ///    completing` (capturing `(etag, updated_at, version_id[, xmin])`
    ///    for race detection); Phase 2 invokes
    ///    `S3Backend::complete_multipart` and captures the finalized
    ///    `(s3_etag, s3_version_id)`; Phase 3 conditional UPDATE
    ///    flips `completing → uploaded` and writes the new
    ///    `(etag, version_id, size_bytes)` along with
    ///    `upload_expires_at = NULL`. Logical-address uniqueness is
    ///    structural (PRIMARY KEY on `id`); no path-level index
    ///    arbitration is required.
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
    /// **Two modes (discriminated by `file_id`).**
    /// - `file_id = None`: initial upload. A fresh `file_id` is minted
    ///   server-side; the backend object key is derived from it. The
    ///   `etag` and `version_id` arguments are ignored on this path
    ///   (no row exists yet). `owner` and `meta` are required;
    ///   `backend_id` is optional and falls back to
    ///   `default_private`.
    /// - `file_id = Some(id)`: variant-B re-upload — same `file_id`,
    ///   same backend object key. The row's CURRENT metadata is
    ///   preserved (the `meta` argument is ignored on this path).
    ///   `etag` and `version_id` act as optional CAS pins:
    ///   - `etag = Some(e)` → mismatch returns `EtagMismatch`.
    ///   - `version_id = Some(v)` (when S3 versioning is enabled) →
    ///     mismatch returns `EtagMismatch` (closes the ABA window
    ///     on bit-identical re-uploads). On backends without S3 versioning
    ///     backends `version_id = Some(_)` is a null-safe mismatch
    ///     against the row's `None` and returns `EtagMismatch`
    ///     immediately.
    ///   - Both pins compose; the strongest CAS uses both.
    async fn put_file(
        &self,
        ctx: &SecurityContext,
        file_id: Option<FileId>,
        backend_id: Option<BackendId>,
        owner: OwnerRef,
        meta: FileMeta,
        bytes: FileByteStream,
        etag: Option<&Etag>,
        version_id: Option<&VersionId>,
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
    /// **Public-read backends** — when the per-item `capability` is
    /// `download.s3.public.v1` and the resolved backend declares that
    /// tag (typically paired with `default_public = true`), the
    /// outcome carries `is_public = true` and a bare HTTPS URL with
    /// no expiry. SigV4 GET URLs (`download.s3.sigv4.v1`) are
    /// returned for
    /// every other backend.
    ///
    /// **Historical version GET** — when the hosting backend has
    /// S3 versioning enabled and the caller passes
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

## Internal S3 Adapter — `pub(crate) struct S3Backend`

Lives inside the implementation crate (`file-storage/src/infra/s3_backend.rs`), not in the public SDK. **Concrete struct, no trait** — `S3Backend` is the single backend kind on the architectural roadmap (S3-only invariant, DESIGN §1.1) so the trait abstraction served only as a unit-test seam, and that role is covered by integration tests against `s3s-fs` running as a child process (DESIGN §4 Testing strategy). The earlier `StorageBackend: Send + Sync` trait has been removed; `Backend Router` holds `Arc<S3Backend>` directly.

Documented here so implementers can match the SQL-side contract (transient-state recovery, strong-CAS, presigned URLs).

The adapter is the seam between the FileStorage core (which owns the `file_storage` SQL schema, the lifecycle write paths in the SDK facade and the in-process `put_file` SDK, and authz integration) and the S3-compatible byte store. It deliberately does **not** know about `file_id`, `OwnerRef`, or authz — those are resolved one layer up. The adapter only ever sees a `BackendObjectKey` (an opaque `String` minted by the core, derived deterministically from `file_id` per ADR-0002) and bytes. It also does NOT know about `gts_file_type` — that field is DB-only and never crosses the adapter boundary.

```rust
use bytes::Bytes;

/// Opaque per-backend object key, minted by the FileStorage core,
/// derived deterministically from the file's `file_id` (per ADR-0002).
/// The adapter treats it as an opaque string.
pub type BackendObjectKey = String;

/// Backend-side metadata returned by `head_object` and by the
/// `open_read` helper. `s3_etag` and `s3_version_id` are the raw
/// values (without surrounding quotes for the etag). The metadata
/// bag (`content_type`, `content_disposition`, `user_metadata`) is
/// what the SDK pulls into the row when recovering a stuck
/// `Completing` / `MetaUpdating` row.
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

/// The single S3-compatible adapter. Holds the `aws-sdk-s3` client,
/// the configured `Backend` descriptor, and signing secrets. Wired
/// once at module init, kept behind `Arc<S3Backend>` for cheap clone.
pub(crate) struct S3Backend {
    client: aws_sdk_s3::Client,
    descriptor: Backend,
    // signing secrets / endpoint / region — held internally
}

impl S3Backend {
    pub(crate) fn descriptor(&self) -> &Backend { &self.descriptor }

    /// Stream the object's bytes, optionally constrained to a byte
    /// range. Returned stream MUST yield chunks in arrival order
    /// from the backend; the FileStorage layer adds the `FileInfo`
    /// snapshot from its own database. The metadata struct is
    /// captured from the GET response headers so the SDK facade can
    /// run in-band recovery for rows stuck in transient states
    /// without an extra HEAD round-trip.
    ///
    /// `range = Some(_)` adds an HTTP `Range: bytes=...` header to
    /// the backend `GetObject` and returns `Some(ResolvedByteRange)`
    /// alongside the metadata, mirroring the backend's
    /// `Content-Range` response. `range = None` is a full-object
    /// read with `range_resolved = None`.
    pub(crate) async fn open_read(
        &self,
        key: &BackendObjectKey,
        range: Option<ByteRange>,
    ) -> Result<(FileByteStream, BackendObjectMetadata, Option<ResolvedByteRange>), FileStorageError>;

    /// Read backend-side metadata without streaming the body. Used
    /// by transient-state recovery and by the strong-CAS variant of
    /// `put_file_info` to verify S3's live etag/version_id.
    pub(crate) async fn head_object(
        &self,
        key: &BackendObjectKey,
    ) -> Result<BackendObjectMetadata, FileStorageError>;

    pub(crate) async fn delete_object(
        &self,
        key: &BackendObjectKey,
    ) -> Result<(), FileStorageError>;

    /// Initiate a multipart upload session and presign N `UploadPart`
    /// PUT URLs (per ADR-0003 — SigV4 PUT). Pins `Content-Type`,
    /// `Content-Disposition`, and every `x-amz-meta-<k>` header from
    /// the supplied `PinnedObjectHeaders` into the SigV4
    /// SignedHeaders set on every part URL. FileStorage NEVER pins
    /// `x-amz-meta-gts-file-type` here — it is DB-only.
    pub(crate) async fn create_multipart_and_presign_parts(
        &self,
        key: &BackendObjectKey,
        pinned: &PinnedObjectHeaders,
        part_count: u32,
        params: &UrlParams,
    ) -> Result<(String /* upload_id */, Vec<UploadPartUrl>, OffsetDateTime), FileStorageError>;

    /// Finalize a multipart upload session — calls
    /// `CompleteMultipartUpload` on the backend with the supplied
    /// `(part_number, etag)` list and returns the finalized
    /// `(s3_etag, s3_version_id)`.
    pub(crate) async fn complete_multipart(
        &self,
        key: &BackendObjectKey,
        upload_id: &str,
        parts: &[UploadPartCompletion],
    ) -> Result<(String, Option<String>), FileStorageError>;

    /// Abort a multipart upload session.
    pub(crate) async fn abort_multipart(
        &self,
        key: &BackendObjectKey,
        upload_id: &str,
    ) -> Result<(), FileStorageError>;

    /// `CopyObject self-copy` with `MetadataDirective: REPLACE` —
    /// rotates the object's user-metadata in place. When `if_match`
    /// is `Some(e)`, the request carries
    /// `x-amz-copy-source-if-match: e` and the backend rejects
    /// stale sources with `412`. Returns the new `s3_etag` and the
    /// new `s3_version_id` (if S3 versioning is enabled).
    pub(crate) async fn copy_object_self(
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
    /// For per-item capability `download.s3.public.v1` (declared by the
    /// backend), the adapter returns a bare-HTTPS URL with no signing
    /// (`is_public = true`).
    pub(crate) async fn issue_presigned_gets(
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
    /// When `Some` and the chosen capability is a `*.versioned.*`
    /// variant (i.e. the bucket has S3 versioning enabled), the
    /// signed URL embeds `versionId=<vid>` so the caller fetches
    /// that historical generation.
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
    PresignDownloadItem, UrlParams, UploadPartCompletion,
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
    custom_metadata: BTreeMap::new(),
};
let handle = files
    .create_presigned_upload(
        &ctx,
        Some(s3_prod_backend_id),  // or None to land on the tenant's default_private backend
        None,                      // None = initial upload; Some(file_id) = re-upload
        owner.clone(),
        meta,
        &"upload.s3.multipart.sigv4.v1".to_string(),
        1,                         // part_count — 1 for small files (single last-part upload)
        UrlParams {
            expires_in_seconds: 600,
            content_disposition: None,
            content_type_override: None,
            allowed_client_cidrs: vec![],
        },
        None,                      // etag pin — initial upload, no row to pin
        None,                      // version_id pin — initial upload, no row to pin
    )
    .await?;
// Frontend PUTs each part to handle.part_urls[i].url; collects (part_number, etag).

// 2. After every part is uploaded, the application backend commits
//    the upload. complete_upload runs the 3-phase commit
//    (PendingUpload → Completing → Uploaded) and returns the
//    finalized FileInfo.
let info = files
    .complete_upload(
        &ctx,
        handle.file_id,
        &handle.upload_id,
        vec![UploadPartCompletion { part_number: 1, etag: part1_etag }],
    )
    .await?;
// `info.etag` is the raw S3 ETag for the finalized object; `info.version_id`
// matches whatever S3 returned (None on backends without S3 versioning).

// 3. Later, another module asks for downloadable URLs. The URL is
//    SigV4-signed (or a bare HTTPS URL if the file lives in a
//    download.s3.public.v1-capable backend) and time-limited per
//    `params.expires_in_seconds`.
let outcomes = files
    .presign_urls(
        &ctx,
        vec![PresignDownloadItem {
            file_id: info.file_id,
            capability: "download.s3.sigv4.v1".to_string(),
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

## Usage Example — re-upload (overwrite in place)

```rust
// Application backend re-asks the user to upload a fresh version of
// an existing file (same file_id). The row stays at the same
// (tenant_id, backend_id, file_path) slot; the multipart session
// replaces the bytes at the same backend object key.
let handle = files
    .create_presigned_upload(
        &ctx,
        None,                          // backend_id ignored on re-upload
        Some(existing_file_id),        // re-upload mode
        owner.clone(),                 // ignored on re-upload
        FileMeta::default(),           // ignored on re-upload — server pins the row's CURRENT meta
        &"upload.s3.multipart.sigv4.v1".to_string(),
        4,                             // 4-part upload
        UrlParams {
            expires_in_seconds: 600,
            content_disposition: None,
            content_type_override: None,
            allowed_client_cidrs: vec![],
        },
        Some(&pinned_etag),            // CAS pin: only proceed if row's etag still matches
        pinned_version_id.as_ref(),    // ABA-safe pin (backends with S3 versioning enabled): both must match
    )
    .await?;
// Frontend PUTs each part to handle.part_urls[i].url with the headers
// FileStorage pinned from the row's CURRENT metadata. After all parts
// uploaded:
let info = files
    .complete_upload(&ctx, existing_file_id, &handle.upload_id, parts).await?;
// info.etag now reflects the new bytes; metadata is unchanged.
```

## Usage Example — in-process streaming read

```rust
use futures::StreamExt;

let mut handle = files.read_file(&ctx, file_id, Some(&etag), None, None).await?;
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
| `presignBatch` (upload item) | `create_presigned_upload` | `Option<BackendId> + Option<FileId> + OwnerRef + FileMeta + capability + part_count + UrlParams + Option<Etag> + Option<VersionId>` | `PresignedUploadHandle` |
| `presignBatch` (download item) | `presign_urls` | `Vec<PresignDownloadItem>` | `Vec<PresignDownloadOutcome>` |
| `completeUpload` | `complete_upload` | `FileId + upload_id + Vec<UploadPartCompletion>` | `FileInfo` |
| `abortUpload` | `abort_upload` | `FileId + upload_id` | `()` |
| `getFile` | `get_file_info` | `Option<Etag> + Option<VersionId>` | `FileInfo` |
| `updateFile` | `put_file_info` | `FileMetaUpdate + Option<Etag> + Option<VersionId>` | `FileInfo` |
| `deleteFile` | `delete_file` | `Option<Etag> + Option<VersionId>` | `()` |
| `listFiles` | `list_files` | `ListFilesQuery` | `FileList` |
| — (in-process SDK only, no REST) | `read_file` | `Option<Etag> + Option<VersionId>` | `FileReadHandle` (streaming) |
| — (in-process SDK only, no REST) | `put_file` | `Option<FileId> + Option<BackendId> + OwnerRef + FileMeta + FileByteStream + Option<Etag> + Option<VersionId>` | `FileInfo` |

## Trait Hierarchy Summary

| Trait | Methods | Consumers | ClientHub key |
|-------|---------|-----------|---------------|
| `FileStorageClient` | 11 (full P1 surface: backends, single canonical upload via `create_presigned_upload` + `complete_upload` / `abort_upload`, batched presigned downloads, file lifecycle including `put_file_info` (2-phase) and `delete_file` (2-phase), streaming I/O including the in-process direct-upload `put_file`) | CyberFabric modules via ClientHub, REST adapter | `dyn FileStorageClient` |
| `S3Backend` (internal `pub(crate) struct`, no trait) | 8 methods (adapter boundary; `head_object` is required by transient-state recovery (`Completing` / `MetaUpdating`) and the strong-CAS variant of `put_file_info`; `copy_object_self` is required by `put_file_info`'s DB+S3 sync; multipart upload via `create_multipart_and_presign_parts` / `complete_multipart` / `abort_multipart`; presigned downloads batch-first via `issue_presigned_gets`) | `BackendRouter` and the SDK facade methods (`create_presigned_upload`, `complete_upload`, `abort_upload`, `delete_file`, `put_file_info`, `read_file`, `put_file`) | — (not exposed in SDK) |

The 11 SDK methods are: `list_backends`, `create_presigned_upload`, `complete_upload`, `abort_upload`, `get_file_info`, `put_file_info`, `delete_file`, `list_files`, `read_file`, `put_file`, `presign_urls` (in P2 also exposed over REST: `presign-batch` carries upload and download items together; `complete_upload` / `abort_upload` are dedicated REST endpoints under `/files/{file_id}/upload/{upload_id}`). The internal `S3Backend` struct declares `open_read`, `head_object`, `delete_object`, `create_multipart_and_presign_parts`, `complete_multipart`, `abort_multipart`, `copy_object_self`, and `issue_presigned_gets`.
