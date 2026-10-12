//! REST DTOs for the control-plane API. These are the only types that carry
//! serde/utoipa; the contract types in the SDK stay transport-agnostic.

use std::collections::BTreeMap;

use time::OffsetDateTime;
use uuid::Uuid;

use file_storage_sdk::{CustomMetadataEntry, File, FileVersion, OwnerKind};

use crate::domain::etag;
use crate::domain::policy::{
    AgeRetention, EffectivePolicy, InactivityRetention, MetadataLimits, MetadataRetention,
    MimeSizeOverride, PolicyBody, RetentionRuleBody, SizeLimits, StoredPolicy, StoredRetentionRule,
};
use crate::infra::backend::BackendCapabilities;

/// One custom-metadata key/value pair.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct MetadataEntryDto {
    pub key: String,
    pub value: String,
}

/// File metadata response (`GET /files/{id}`, and the body of mutations).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct FileDto {
    pub file_id: Uuid,
    pub tenant_id: Uuid,
    pub owner_kind: String,
    pub owner_id: Uuid,
    pub name: String,
    pub gts_file_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_id: Option<Uuid>,
    pub meta_version: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub last_modified_at: OffsetDateTime,
    pub custom_metadata: Vec<MetadataEntryDto>,
}

impl FileDto {
    #[must_use]
    pub fn from_parts(file: File, meta: Vec<CustomMetadataEntry>) -> Self {
        let etag = etag::etag_for(&file);
        Self {
            file_id: file.file_id,
            tenant_id: file.tenant_id,
            owner_kind: file.owner_kind.as_str().to_owned(),
            owner_id: file.owner_id,
            name: file.name,
            gts_file_type: file.gts_file_type,
            content_id: file.content_id,
            meta_version: file.meta_version,
            etag,
            created_at: file.created_at,
            last_modified_at: file.last_modified_at,
            custom_metadata: meta
                .into_iter()
                .map(|e| MetadataEntryDto {
                    key: e.key,
                    value: e.value,
                })
                .collect(),
        }
    }
}

/// Request to create a file (`POST /files`).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct CreateFileReq {
    /// `"user"` or `"app"`.
    pub owner_kind: String,
    pub owner_id: Uuid,
    pub name: String,
    pub gts_file_type: String,
    pub mime_type: String,
    #[serde(default)]
    pub custom_metadata: Vec<MetadataEntryDto>,
    /// Optional idempotency key: within the same `(owner_kind, owner_id)`, a retry with
    /// the same key returns the original response without creating a new file.
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

impl CreateFileReq {
    /// Parse the owner kind, rejecting unknown spellings.
    #[must_use]
    pub fn parse_owner_kind(&self) -> Option<OwnerKind> {
        OwnerKind::parse(&self.owner_kind)
    }
}

/// Result of create / presign: identity + the signed upload URL.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct UploadTicketDto {
    pub file_id: Uuid,
    pub version_id: Uuid,
    pub upload_url: String,
}

/// Result of `GET /files/{id}/download-url`.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct DownloadTicketDto {
    pub download_url: String,
    pub etag: String,
    pub version_id: Uuid,
}

/// Request body for `POST /files/{id}/bind`.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct BindReq {
    pub version_id: Uuid,
}

/// Request body for `PATCH /files/{id}` (JSON merge patch over custom metadata).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct UpdateMetadataReq {
    /// Keys mapped to a value (upsert) or `null` (delete). Absent keys unchanged.
    #[serde(default)]
    pub custom_metadata: BTreeMap<String, Option<String>>,
}

/// One content version (`GET /files/{id}/versions`).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct VersionDto {
    pub version_id: Uuid,
    pub mime_type: String,
    pub size: i64,
    pub hash_algorithm: String,
    pub hash: String,
    /// Hash mode: `"whole-sha256"` (`hash` is `sha256` of the object bytes) or
    /// `"multipart-composite-sha256"` (`hash` is `sha256` of the part manifest).
    pub hash_mode: String,
    /// Number of parts for `multipart-composite-sha256`; omitted for `whole-sha256`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part_count: Option<i32>,
    pub status: String,
    pub is_current: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl From<FileVersion> for VersionDto {
    fn from(v: FileVersion) -> Self {
        Self {
            version_id: v.version_id,
            mime_type: v.mime_type,
            size: v.size,
            hash_algorithm: v.hash_algorithm,
            hash: hex::encode(&v.hash_value),
            hash_mode: v.hash_mode,
            part_count: v.part_count,
            status: v.status.as_str().to_owned(),
            is_current: v.is_current,
            created_at: v.created_at,
        }
    }
}

/// List of file versions (`GET /files/{id}/versions`).
// Transparent newtype: a bare JSON array with a unique OpenAPI schema name (applies to
// every `*List` below).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(transparent)]
pub struct VersionDtoList(pub Vec<VersionDto>);

impl toolkit::api::api_dto::ResponseApiDto for VersionDtoList {}

/// List of files (`GET /files`).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(transparent)]
pub struct FileDtoList(pub Vec<FileDto>);

impl toolkit::api::api_dto::ResponseApiDto for FileDtoList {}

/// Backend capabilities surface for `GET /storages`.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct CapabilitiesDto {
    pub multipart_native: bool,
    pub encryption_native: bool,
    pub range_native: bool,
}

impl From<BackendCapabilities> for CapabilitiesDto {
    fn from(c: BackendCapabilities) -> Self {
        Self {
            multipart_native: c.multipart_native,
            encryption_native: c.encryption_native,
            range_native: c.range_native,
        }
    }
}

/// A configured storage backend (`GET /storages`).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct StorageDto {
    pub id: String,
    pub capabilities: CapabilitiesDto,
}

impl StorageDto {
    #[must_use]
    pub fn new(id: String, capabilities: BackendCapabilities) -> Self {
        Self {
            id,
            capabilities: capabilities.into(),
        }
    }
}

/// List of configured storage backends (`GET /storages`).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(transparent)]
pub struct StorageDtoList(pub Vec<StorageDto>);

impl toolkit::api::api_dto::ResponseApiDto for StorageDtoList {}

/// Per-mime-type size limit override in a policy request/response.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct MimeSizeOverrideDto {
    /// MIME type or pattern (e.g. `"image/*"`, `"video/mp4"`).
    pub mime: String,
    /// Maximum file size in bytes for this mime pattern.
    pub max_bytes: u64,
}

impl From<MimeSizeOverride> for MimeSizeOverrideDto {
    fn from(v: MimeSizeOverride) -> Self {
        Self {
            mime: v.mime,
            max_bytes: v.max_bytes,
        }
    }
}

impl From<MimeSizeOverrideDto> for MimeSizeOverride {
    fn from(v: MimeSizeOverrideDto) -> Self {
        Self {
            mime: v.mime,
            max_bytes: v.max_bytes,
        }
    }
}

/// Size limits in a policy body.
#[derive(Debug, Clone, Default)]
#[toolkit_macros::api_dto(request, response)]
pub struct SizeLimitsDto {
    /// Global maximum file size in bytes (`null` = unlimited at this level).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    /// Per-mime-type overrides.
    #[serde(default)]
    pub per_mime: Vec<MimeSizeOverrideDto>,
}

impl From<SizeLimits> for SizeLimitsDto {
    fn from(v: SizeLimits) -> Self {
        Self {
            max_bytes: v.max_bytes,
            per_mime: v.per_mime.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<SizeLimitsDto> for SizeLimits {
    fn from(v: SizeLimitsDto) -> Self {
        Self {
            max_bytes: v.max_bytes,
            per_mime: v.per_mime.into_iter().map(Into::into).collect(),
        }
    }
}

/// Metadata limits in a policy body.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, Default)]
#[toolkit_macros::api_dto(request, response)]
pub struct MetadataLimitsDto {
    /// Maximum number of key-value pairs per file (`null` = unlimited).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_pairs: Option<u32>,
    /// Maximum key length in bytes (`null` = unlimited).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_key_len: Option<u32>,
    /// Maximum value length in bytes (`null` = unlimited).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_value_len: Option<u32>,
    /// Maximum total metadata byte size (`null` = unlimited).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_total_bytes: Option<u32>,
}

impl From<MetadataLimits> for MetadataLimitsDto {
    fn from(v: MetadataLimits) -> Self {
        Self {
            max_pairs: v.max_pairs,
            max_key_len: v.max_key_len,
            max_value_len: v.max_value_len,
            max_total_bytes: v.max_total_bytes,
        }
    }
}

impl From<MetadataLimitsDto> for MetadataLimits {
    fn from(v: MetadataLimitsDto) -> Self {
        Self {
            max_pairs: v.max_pairs,
            max_key_len: v.max_key_len,
            max_value_len: v.max_value_len,
            max_total_bytes: v.max_total_bytes,
        }
    }
}

/// Policy body in requests and responses.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct PolicyBodyDto {
    /// Allowed MIME types for upload (empty = all types permitted at this level).
    #[serde(default)]
    pub allowed_mime_types: Vec<String>,
    /// Size limits (global + per-mime overrides).
    #[serde(default)]
    pub size_limits: SizeLimitsDto,
    /// Metadata limits.
    #[serde(default)]
    pub metadata_limits: MetadataLimitsDto,
    /// Enabled `EventBroker` event types (empty = none at this level).
    #[serde(default)]
    pub enabled_event_types: Vec<String>,
}

impl From<PolicyBody> for PolicyBodyDto {
    fn from(v: PolicyBody) -> Self {
        Self {
            allowed_mime_types: v.allowed_mime_types,
            size_limits: v.size_limits.into(),
            metadata_limits: v.metadata_limits.into(),
            enabled_event_types: v.enabled_event_types,
        }
    }
}

impl From<PolicyBodyDto> for PolicyBody {
    fn from(v: PolicyBodyDto) -> Self {
        Self {
            allowed_mime_types: v.allowed_mime_types,
            size_limits: v.size_limits.into(),
            metadata_limits: v.metadata_limits.into(),
            enabled_event_types: v.enabled_event_types,
        }
    }
}

/// A stored policy row as returned by `GET /policy`.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct PolicyDto {
    pub policy_id: Uuid,
    pub tenant_id: Uuid,
    /// `"tenant"` or `"user"`.
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_owner_id: Option<Uuid>,
    pub body: PolicyBodyDto,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl From<StoredPolicy> for PolicyDto {
    fn from(p: StoredPolicy) -> Self {
        Self {
            policy_id: p.policy_id,
            tenant_id: p.tenant_id,
            scope: p.scope.as_str().to_owned(),
            scope_owner_id: p.scope_owner_id,
            body: p.body.into(),
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }
}

/// Effective policy response: the most-restrictive combination of tenant ⊕ user.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct EffectivePolicyDto {
    /// Intersection of allowed mime types from all levels (`null` = all types permitted).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_mime_types: Option<Vec<String>>,
    /// Effective global size limit in bytes (`null` = unlimited).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    /// Per-mime size overrides (union from all levels, most restrictive per pattern).
    pub per_mime_max_bytes: Vec<MimeSizeOverrideDto>,
    /// Effective metadata limits (most restrictive per field).
    pub metadata_limits: MetadataLimitsDto,
}

impl From<EffectivePolicy> for EffectivePolicyDto {
    fn from(ep: EffectivePolicy) -> Self {
        Self {
            allowed_mime_types: ep.allowed_mime_types,
            max_bytes: ep.max_bytes,
            per_mime_max_bytes: ep.per_mime_max_bytes.into_iter().map(Into::into).collect(),
            metadata_limits: ep.metadata_limits.into(),
        }
    }
}

/// Request body for `PUT /policy`.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct SetPolicyReq {
    /// `"tenant"` or `"user"`.
    pub scope: String,
    /// Target owner id (required when `scope = "user"`; omit for tenant scope).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_owner_id: Option<Uuid>,
    /// The policy to store.
    pub body: PolicyBodyDto,
}

/// Age-based retention criterion.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct AgeRetentionDto {
    pub max_age_days: u32,
}

impl From<AgeRetention> for AgeRetentionDto {
    fn from(v: AgeRetention) -> Self {
        Self {
            max_age_days: v.max_age_days,
        }
    }
}

impl From<AgeRetentionDto> for AgeRetention {
    fn from(v: AgeRetentionDto) -> Self {
        Self {
            max_age_days: v.max_age_days,
        }
    }
}

/// Inactivity-based retention criterion.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct InactivityRetentionDto {
    pub inactivity_days: u32,
}

impl From<InactivityRetention> for InactivityRetentionDto {
    fn from(v: InactivityRetention) -> Self {
        Self {
            inactivity_days: v.inactivity_days,
        }
    }
}

impl From<InactivityRetentionDto> for InactivityRetention {
    fn from(v: InactivityRetentionDto) -> Self {
        Self {
            inactivity_days: v.inactivity_days,
        }
    }
}

/// Metadata-value-based retention criterion.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct MetadataRetentionDto {
    pub key: String,
    pub value: String,
}

impl From<MetadataRetention> for MetadataRetentionDto {
    fn from(v: MetadataRetention) -> Self {
        Self {
            key: v.key,
            value: v.value,
        }
    }
}

impl From<MetadataRetentionDto> for MetadataRetention {
    fn from(v: MetadataRetentionDto) -> Self {
        Self {
            key: v.key,
            value: v.value,
        }
    }
}

/// Retention rule body in requests and responses.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
pub struct RetentionRuleBodyDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age: Option<AgeRetentionDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inactivity: Option<InactivityRetentionDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<MetadataRetentionDto>,
}

impl From<RetentionRuleBody> for RetentionRuleBodyDto {
    fn from(v: RetentionRuleBody) -> Self {
        Self {
            age: v.age.map(Into::into),
            inactivity: v.inactivity.map(Into::into),
            metadata: v.metadata.map(Into::into),
        }
    }
}

impl From<RetentionRuleBodyDto> for RetentionRuleBody {
    fn from(v: RetentionRuleBodyDto) -> Self {
        Self {
            age: v.age.map(Into::into),
            inactivity: v.inactivity.map(Into::into),
            metadata: v.metadata.map(Into::into),
        }
    }
}

/// A stored retention rule row.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct RetentionRuleDto {
    pub rule_id: Uuid,
    pub tenant_id: Uuid,
    /// `"tenant"`, `"user"`, or `"file"`.
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_target_id: Option<Uuid>,
    pub body: RetentionRuleBodyDto,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl From<StoredRetentionRule> for RetentionRuleDto {
    fn from(r: StoredRetentionRule) -> Self {
        Self {
            rule_id: r.rule_id,
            tenant_id: r.tenant_id,
            scope: r.scope.as_str().to_owned(),
            scope_target_id: r.scope_target_id,
            body: r.body.into(),
            created_at: r.created_at,
        }
    }
}

/// List of retention rules (`GET /retention-rules`).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(transparent)]
pub struct RetentionRuleDtoList(pub Vec<RetentionRuleDto>);

impl toolkit::api::api_dto::ResponseApiDto for RetentionRuleDtoList {}

/// Request body to create a retention rule (`POST /retention-rules`).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct CreateRetentionRuleReq {
    /// `"tenant"`, `"user"`, or `"file"`.
    pub scope: String,
    /// Target id (`user_id` for user scope, `file_id` for file scope; omit for tenant scope).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_target_id: Option<Uuid>,
    pub body: RetentionRuleBodyDto,
}

/// Request to initiate a multipart upload (`POST /files/{id}/multipart`).
///
/// The server answers with a parts plan containing one signed upload URL per part.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct InitiateMultipartReq {
    /// Declared MIME type for the file content.
    pub declared_mime: String,
    /// Declared total file size in bytes. Validated against the effective policy size
    /// limit and the storage quota up front, so an oversized upload is rejected before
    /// any bytes are transferred.
    pub declared_size: u64,
    /// Preferred part size in bytes (hint); the server may override it to meet the
    /// backend's minimum part size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_part_size: Option<u64>,
    /// Advisory upload concurrency; does not change the parts plan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<u32>,
}

/// One part in the parts plan.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct MultipartPartPlanDto {
    /// 1-based part number (S3 convention).
    pub part_number: u32,
    /// Byte offset of this part within the final assembled object.
    pub offset: u64,
    /// Exact byte length this part's body must be.
    pub size: u64,
    /// Sidecar signed URL for `PUT`-ing this part's bytes.
    /// The URL embeds the exact `size` claim the sidecar enforces.
    pub upload_url: String,
}

/// Parts plan returned by `POST /files/{id}/multipart`.
///
/// The client `PUT`s each part to its `upload_url`; the size is enforced before any
/// bytes are written.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct MultipartPlanDto {
    pub upload_id: Uuid,
    pub version_id: Uuid,
    /// Hash algorithm used for per-part hashes (`"SHA-256"`).
    pub part_hash_algorithm: String,
    /// Uniform part size in bytes (the final part may be smaller).
    pub part_size: u64,
    /// Parts in ascending `part_number` order, each with its signed upload URL.
    pub parts: Vec<MultipartPartPlanDto>,
    /// Expiry of all per-part URLs (RFC 3339).
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: time::OffsetDateTime,
}

/// Response of `POST /files/{id}/multipart/{upload_id}/complete`: the bound version,
/// its size, and the composite content hash. `manifest` lets a client re-verify the hash.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct MultipartCompleteDto {
    pub version_id: Uuid,
    pub size: i64,
    /// Always `"SHA-256"`.
    pub hash_algorithm: String,
    /// Hex-encoded composite hash (`sha256` of the manifest).
    pub content_hash: String,
    /// Always `"multipart-composite-sha256"` for this endpoint.
    pub hash_mode: String,
    pub part_count: i32,
    /// Part manifest text the composite hash is computed over.
    pub manifest: String,
}

/// One already-uploaded part (`GET /files/{id}/multipart/{upload_id}`).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct ReceivedPartDto {
    pub part_number: u32,
    pub size: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub uploaded_at: OffsetDateTime,
}

/// One part not yet uploaded.
///
/// `upload_url` is present only while the session is `in_progress` and unexpired.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct MissingPartDto {
    pub part_number: u32,
    pub offset: u64,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upload_url: Option<String>,
}

/// Response of `GET /files/{id}/multipart/{upload_id}`.
///
/// `received` lists uploaded parts; `missing` the rest, each with a fresh resume
/// `upload_url` only while the session is `in_progress` and unexpired.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct MultipartStatusDto {
    pub upload_id: Uuid,
    pub version_id: Uuid,
    /// `"in_progress"`, `"completed"`, or `"aborted"`.
    pub state: String,
    pub declared_mime: String,
    pub declared_size: u64,
    pub part_size: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    pub received: Vec<ReceivedPartDto>,
    pub missing: Vec<MissingPartDto>,
}

/// Request to migrate a file's content to a different storage backend.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct MigrateBackendReq {
    /// The id of the target backend to migrate the file's content to.
    pub target_backend_id: String,
}

/// Request to transfer ownership of a file (`POST /files/{id}/transfer`).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct TransferOwnershipReq {
    /// New owner kind: `"user"` or `"app"`.
    pub new_owner_kind: String,
    /// New owner UUID.
    pub new_owner_id: Uuid,
}
