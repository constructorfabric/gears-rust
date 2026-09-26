//! Public model types for the file-storage gear.
//!
//! Contract-layer types only — no `serde`, no HTTP, no `utoipa`. REST DTOs live
//! in the impl crate under `api/rest/`. These are the transport-agnostic domain
//! types other gears and the impl layers share.

use time::OffsetDateTime;
use uuid::Uuid;

/// Immutable identity of a logical file (PRD: `File ID`).
pub type FileId = Uuid;

/// Identity of one immutable content blob of a file (PRD: `Version ID`).
pub type VersionId = Uuid;

/// The principal that owns a file (PRD `cpt-cf-file-storage-fr-file-ownership`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerKind {
    /// A platform user.
    User,
    /// A Gear (app), e.g. the LLM Gateway owning its generated media.
    App,
}

impl OwnerKind {
    /// The wire/DB spelling (`"user"` / `"app"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::App => "app",
        }
    }

    /// Parse from the DB/wire spelling; `None` for anything else.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Self::User),
            "app" => Some(Self::App),
            _ => None,
        }
    }
}

/// Lifecycle of a content version (PRD: `pending` → `available`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionStatus {
    /// Pre-registered, bytes may be uploading; not yet bindable as current.
    Pending,
    /// Bytes durably written and verified; bindable as the file's content.
    Available,
}

impl VersionStatus {
    /// The wire/DB spelling (`"pending"` / `"available"`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Available => "available",
        }
    }

    /// Parse from the DB/wire spelling; `None` for anything else.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "available" => Some(Self::Available),
            _ => None,
        }
    }
}

/// A logical file: stable identity plus the current content pointer. Holds no
/// bytes — content lives in [`FileVersion`] objects on a storage backend.
// `file_id` mirrors the DB column name and the domain vocabulary.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub file_id: FileId,
    pub tenant_id: Uuid,
    pub owner_kind: OwnerKind,
    pub owner_id: Uuid,
    pub name: String,
    pub gts_file_type: String,
    /// `version_id` currently bound as live content; `None` until first bind.
    pub content_id: Option<VersionId>,
    /// Monotonic counter bumped on metadata-only writes (`If-Match-Metadata`).
    pub meta_version: i64,
    pub created_at: OffsetDateTime,
    pub last_modified_at: OffsetDateTime,
}

/// An immutable content version of a [`File`]. The backend object lives at
/// `/{file_id}/{version_id}` and is never mutated in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileVersion {
    pub file_id: FileId,
    pub version_id: VersionId,
    pub mime_type: String,
    pub size: i64,
    pub hash_algorithm: String,
    pub hash_value: Vec<u8>,
    /// ADR-0006 content-hash mode discriminator: `"whole-sha256"` (the
    /// default for every version written before ADR-0006, and for every
    /// non-multipart upload) or `"multipart-composite-sha256"`. Together
    /// with `hash_value` this is the sole ground truth for "how do I verify
    /// this version": for `whole-sha256`, `hash_value` is `sha256(bytes)`;
    /// for `multipart-composite-sha256`, `hash_value` is `sha256(manifest)`.
    pub hash_mode: String,
    /// Number of parts for a `multipart-composite-sha256` version; `None`
    /// for `whole-sha256`.
    pub part_count: Option<i32>,
    pub status: VersionStatus,
    pub is_current: bool,
    pub backend_id: String,
    pub backend_path: String,
    pub created_at: OffsetDateTime,
    /// `true` once this version's own finalize won its auto-bind CAS (set in
    /// that transaction, never cleared), so a retried finalize replays
    /// `Bound` even if the file has since been rebound. A later explicit
    /// `bind` does not set it.
    pub bound_on_finalize: bool,
}

/// One user-defined custom-metadata key/value pair attached to a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomMetadataEntry {
    pub key: String,
    pub value: String,
}

/// Data to create a new file (control-plane `POST /files`). The tenant is taken
/// from the authenticated caller, never from the request. The first content
/// version is pre-registered and bound after upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFile {
    pub owner_kind: OwnerKind,
    pub owner_id: Uuid,
    pub name: String,
    pub gts_file_type: String,
    /// Declared content type of the first version (validated against bytes).
    pub mime_type: String,
    pub custom_metadata: Vec<CustomMetadataEntry>,
}

/// Mandatory owner filter for listing (PRD `cpt-cf-file-storage-fr-list-files`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnerFilter {
    pub owner_kind: OwnerKind,
    pub owner_id: Uuid,
}

/// A parsed HTTP `Range` request over a content blob of known length
/// (PRD `cpt-cf-file-storage-fr-range-requests`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRange {
    /// `bytes=start-end` (both inclusive).
    Inclusive { start: u64, end: u64 },
    /// `bytes=start-` (start to end of content).
    OpenEnded { start: u64 },
    /// `bytes=-length` (the final `length` bytes).
    Suffix { length: u64 },
}

impl ByteRange {
    /// Resolve to a concrete inclusive `[start, end]` against `total` bytes.
    /// Returns `None` if the range is unsatisfiable for that length.
    #[must_use]
    pub fn resolve(self, total: u64) -> Option<(u64, u64)> {
        if total == 0 {
            return None;
        }
        match self {
            Self::Inclusive { start, end } => {
                if start >= total || start > end {
                    return None;
                }
                Some((start, end.min(total - 1)))
            }
            Self::OpenEnded { start } => {
                if start >= total {
                    return None;
                }
                Some((start, total - 1))
            }
            Self::Suffix { length } => {
                if length == 0 {
                    return None;
                }
                let len = length.min(total);
                Some((total - len, total - 1))
            }
        }
    }
}

/// JSON-Merge-Patch semantics for custom metadata: `Some(value)` upserts a key,
/// `None` deletes it; absent keys are unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CustomMetadataPatch {
    pub entries: Vec<(String, Option<String>)>,
}

/// A file plus its custom metadata — what most read/write operations on
/// [`crate::FileStorageClientV1`] return (mirrors the control API's `FileDto`,
/// which always round-trips both together).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    pub file: File,
    pub custom_metadata: Vec<CustomMetadataEntry>,
}

/// Result of a conditional [`crate::FileStorageClientV1::get_file`]: either the
/// current state, or an explicit "unchanged" when the caller's `if_none_match`
/// matches the file's current content `ETag` (mirrors the control API's `304`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileFetch {
    Modified {
        // Boxed: `FileRecord` is far larger than the unit `NotModified`
        // variant (clippy::large_enum_variant), and this is the read path's
        // return value, not a hot allocation.
        record: Box<FileRecord>,
        /// The file's current content `ETag` (`None` until the first bind).
        etag: Option<String>,
    },
    NotModified,
}

/// One content version plus its ADR-0006 offset-manifest text, when it has
/// one (`multipart-composite-sha256` versions only — `None` for
/// `whole-sha256`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionRecord {
    pub version: FileVersion,
    pub manifest: Option<String>,
}

/// A page of an offset-paginated listing.
///
/// Not `toolkit_odata::Page` (cursor-based `OData` paging): every listing
/// operation on this trait mirrors a control-plane endpoint that already
/// uses plain `limit`/`offset` (no cursor, no total count — see
/// `docs/api.md`), so this stays a simple offset-continuation marker instead
/// of introducing a cursor format nothing else in this gear speaks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// Offset to resume at for the next page. `Some` only when this page was
    /// full against the caller's own requested `limit` — a hint that more
    /// results may exist, not a guarantee (no `COUNT` query backs it). `None`
    /// on a short page (the common "last page" signal) or when the caller
    /// left `limit` unspecified (the server's own default page size is not
    /// visible to the client, so no continuation offset can be computed).
    pub next_offset: Option<u64>,
}

impl<T> Page<T> {
    /// Build a page from `items` plus the `(limit, offset)` the caller
    /// requested, applying the [`Self::next_offset`] heuristic described on
    /// the field doc.
    #[must_use]
    pub fn new(items: Vec<T>, requested_limit: Option<u64>, offset: u64) -> Self {
        let next_offset = requested_limit.and_then(|limit| {
            (items.len() as u64 >= limit).then(|| offset.saturating_add(items.len() as u64))
        });
        Self { items, next_offset }
    }
}

// ── Upload / download tickets ───────────────────────────────────────────────

/// Identity plus the signed URL a caller `PUT`s a single part's bytes to
/// (`create_file`'s single-part path, and `presign_version`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadTicket {
    pub file_id: FileId,
    pub version_id: VersionId,
    pub upload_url: String,
}

/// The signed URL plus the content `ETag` it pins (`download_url`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadTicket {
    pub download_url: String,
    pub etag: String,
    pub version_id: VersionId,
}

// ── Multipart upload ─────────────────────────────────────────────────────────

/// Multipart intent for `create_file`'s merged create+plan path (mirrors the
/// control API's `multipart` request block).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultipartIntent {
    pub declared_size: u64,
    pub preferred_part_size: Option<u64>,
}

/// Result of [`crate::FileStorageClientV1::create_file`]: either the ordinary
/// single-part ticket, or — when a [`MultipartIntent`] was given and the
/// server-computed plan has two or more parts — the file's identity plus the
/// full parts plan (no single-part version was pre-registered).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateFileOutcome {
    SinglePart(UploadTicket),
    Multipart {
        file_id: FileId,
        plan: MultipartPlan,
    },
}

/// One planned part, with its own signed sidecar upload URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultipartPartPlan {
    /// 1-based part number (S3 convention).
    pub part_number: u32,
    /// Byte offset of this part within the final assembled object.
    pub offset: u64,
    /// Exact byte length of this part.
    pub size: u64,
    /// Sidecar signed URL; the caller must `PUT` exactly `size` bytes here.
    pub upload_url: String,
}

/// The server-authoritative parts plan returned by `initiate_multipart` (and
/// by `create_file`'s merged path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultipartPlan {
    pub upload_id: Uuid,
    pub version_id: VersionId,
    /// Hash algorithm used for per-part hashes (`"SHA-256"`).
    pub part_hash_algorithm: String,
    /// Uniform part size (bytes); the final part may be smaller.
    pub part_size: u64,
    /// One entry per part, in ascending `part_number` order.
    pub parts: Vec<MultipartPartPlan>,
    /// Expiry shared by every per-part URL above.
    pub expires_at: OffsetDateTime,
}

/// Lifecycle state of a multipart upload session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MultipartUploadState {
    InProgress,
    /// A `complete` call is assembling the object on the backend.
    Completing,
    Completed,
    Aborted,
}

impl MultipartUploadState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InProgress => "in_progress",
            Self::Completing => "completing",
            Self::Completed => "completed",
            Self::Aborted => "aborted",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "in_progress" => Some(Self::InProgress),
            "completing" => Some(Self::Completing),
            "completed" => Some(Self::Completed),
            "aborted" => Some(Self::Aborted),
            _ => None,
        }
    }
}

/// One already-uploaded part, as reported by the sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedPart {
    pub part_number: u32,
    pub size: i64,
    pub uploaded_at: OffsetDateTime,
}

/// One part not yet uploaded, with a fresh resume URL when the session can
/// still be resumed (`in_progress` and unexpired); `None` for a terminal or
/// expired session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingPart {
    pub part_number: u32,
    pub offset: u64,
    pub size: u64,
    pub upload_url: Option<String>,
}

/// Result of `introspect_multipart`: session state plus received/missing parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultipartStatus {
    pub upload_id: Uuid,
    pub version_id: VersionId,
    pub state: MultipartUploadState,
    pub declared_mime: String,
    pub declared_size: u64,
    pub part_size: u64,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
    pub received: Vec<ReceivedPart>,
    pub missing: Vec<MissingPart>,
}

/// Bind outcome shared by the single-part and multipart upload paths.
///
/// * `Bound` — this upload's version became the file's current content.
/// * `Conflict` — an auto-bind was requested but the CAS lost (content moved
///   concurrently); the upload itself still succeeded — a manual `bind` with
///   `current_etag` as `If-Match` makes it live without re-uploading.
/// * `Manual` — no bind was requested; the caller binds explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindState {
    Bound,
    Conflict,
    Manual,
}

impl BindState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bound => "bound",
            Self::Conflict => "conflict",
            Self::Manual => "manual",
        }
    }
}

/// Result of a successful `complete_multipart`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedMultipartUpload {
    pub version_id: VersionId,
    pub size: i64,
    /// Always `"SHA-256"`.
    pub hash_algorithm: String,
    pub content_hash: Vec<u8>,
    /// `"whole-sha256"` or `"multipart-composite-sha256"` (ADR-0006).
    pub hash_mode: String,
    pub part_count: i32,
    /// Wire-format offset-manifest text; `None` for a one-part plan.
    pub manifest: Option<String>,
    pub bind_state: BindState,
    /// New content `ETag` after a successful bind (`bind_state == Bound` only).
    pub etag: Option<String>,
    /// Current content `ETag` when the bind CAS was lost (`Conflict` only).
    pub current_etag: Option<String>,
}

/// Outcome of `complete_multipart`: either the finished result, or "someone
/// else currently holds the completion lease" — poll by re-issuing the same
/// (idempotent) call after `retry_after_secs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MultipartCompleteOutcome {
    Completed(CompletedMultipartUpload),
    Completing { retry_after_secs: u64 },
}

// ── Storage backends ─────────────────────────────────────────────────────────

/// Capabilities of a configured storage backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct StorageCapabilities {
    pub multipart_native: bool,
    pub encryption_native: bool,
    pub range_native: bool,
}

/// A configured storage backend (`list_storages`/`get_storage`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Storage {
    pub id: String,
    pub capabilities: StorageCapabilities,
}

// ── Policy ───────────────────────────────────────────────────────────────────

/// Whether a policy row applies to the whole tenant or a single user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyScope {
    Tenant,
    User,
}

impl PolicyScope {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tenant => "tenant",
            Self::User => "user",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "tenant" => Some(Self::Tenant),
            "user" => Some(Self::User),
            _ => None,
        }
    }
}

/// Per-mime-type size limit override.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MimeSizeOverride {
    pub mime: String,
    pub max_bytes: u64,
}

/// Size limits portion of a policy body.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SizeLimits {
    /// Global maximum file size in bytes (`None` = unlimited at this level).
    pub max_bytes: Option<u64>,
    /// Per-mime overrides; the most specific matching entry is used.
    pub per_mime: Vec<MimeSizeOverride>,
}

/// Metadata limits portion of a policy body.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MetadataLimits {
    pub max_pairs: Option<u32>,
    pub max_key_len: Option<u32>,
    pub max_value_len: Option<u32>,
    pub max_total_bytes: Option<u32>,
}

/// A policy body: allowed mime types, size limits, metadata limits, and
/// enabled event types for a single scope (tenant or user).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PolicyBody {
    /// Empty means "all types allowed". Entries may use a `*` subtype
    /// wildcard (e.g. `"image/*"`).
    pub allowed_mime_types: Vec<String>,
    pub size_limits: SizeLimits,
    pub metadata_limits: MetadataLimits,
    /// Empty means no `EventBroker` events enabled at this level.
    pub enabled_event_types: Vec<String>,
}

/// A stored policy row (`get_policy`/`put_policy`).
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub policy_id: Uuid,
    pub tenant_id: Uuid,
    pub scope: PolicyScope,
    /// `None` for `scope = Tenant`; the user's `owner_id` for `scope = User`.
    pub scope_owner_id: Option<Uuid>,
    pub body: PolicyBody,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// The fully resolved effective policy (`get_effective_policy`): the
/// most-restrictive combination of tenant + user levels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectivePolicy {
    /// `None` means "all types allowed"; an empty `Vec` means "none allowed".
    pub allowed_mime_types: Option<Vec<String>>,
    pub max_bytes: Option<u64>,
    pub per_mime_max_bytes: Vec<MimeSizeOverride>,
    pub metadata_limits: MetadataLimits,
}

// ── Retention rules ──────────────────────────────────────────────────────────

/// Whether a retention rule applies to the tenant, a user, or a single file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionScope {
    Tenant,
    User,
    File,
}

impl RetentionScope {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tenant => "tenant",
            Self::User => "user",
            Self::File => "file",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "tenant" => Some(Self::Tenant),
            "user" => Some(Self::User),
            "file" => Some(Self::File),
            _ => None,
        }
    }
}

/// Age-based retention criterion: delete files older than `max_age_days`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgeRetention {
    pub max_age_days: u32,
}

/// Inactivity-based retention criterion: delete files not modified in
/// `inactivity_days`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InactivityRetention {
    pub inactivity_days: u32,
}

/// Metadata-based retention criterion: delete when `key` equals `value`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MetadataRetention {
    pub key: String,
    pub value: String,
}

/// A retention rule body. May specify one or more criteria; any match
/// triggers expiry (OR semantics).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetentionRuleBody {
    pub age: Option<AgeRetention>,
    pub inactivity: Option<InactivityRetention>,
    pub metadata: Option<MetadataRetention>,
}

/// A stored retention rule (`list_retention_rules`/`create_retention_rule`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionRule {
    pub rule_id: Uuid,
    pub tenant_id: Uuid,
    pub scope: RetentionScope,
    /// `None` for tenant scope; `user_id` for user scope; `file_id` for file
    /// scope.
    pub scope_target_id: Option<Uuid>,
    pub body: RetentionRuleBody,
    pub created_at: OffsetDateTime,
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod models_tests;
