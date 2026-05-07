//! REST DTOs aligned with `modules/file-storage/docs/openapi.yaml`.

use std::collections::BTreeMap;

use file_storage_sdk::{
    Backend, BackendCapability, BackendKind, BackendTransport, FileInfo, FileMeta, FileMetaUpdate,
    FileStatus, OwnerRef, PresignDownloadOutcome, PresignedDownload, PresignedUploadHandle,
    UrlParams,
};
use modkit_macros::api_dto;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone)]
#[api_dto(request, response)]
pub struct BackendDto {
    pub id: Uuid,
    pub kind: String,
    pub default_public: bool,
    pub default_private: bool,
    pub transport: String,
    pub capabilities: Vec<String>,
    pub max_file_size_bytes: Option<u64>,
}

impl From<Backend> for BackendDto {
    fn from(b: Backend) -> Self {
        Self {
            id: b.id,
            kind: match b.kind {
                BackendKind::S3Compatible => "s3-compatible".to_owned(),
            },
            default_public: b.default_public,
            default_private: b.default_private,
            transport: match b.transport {
                BackendTransport::Redirect => "redirect".to_owned(),
            },
            capabilities: b
                .capabilities
                .into_iter()
                .map(|c| match c {
                    BackendCapability::PresignedUrls => "presigned_urls".to_owned(),
                    BackendCapability::PublicReadUrls => "public_read_urls".to_owned(),
                    BackendCapability::PresignedConditionalPut => {
                        "presigned_conditional_put".to_owned()
                    }
                })
                .collect(),
            max_file_size_bytes: b.max_file_size_bytes,
        }
    }
}

#[derive(Debug, Clone)]
#[api_dto(response)]
pub struct ListBackendsResponse {
    pub items: Vec<BackendDto>,
}

#[derive(Debug, Clone)]
#[api_dto(request, response)]
pub struct OwnerRefDto {
    pub tenant_id: Uuid,
    pub owner_id: Uuid,
}

impl From<OwnerRefDto> for OwnerRef {
    fn from(dto: OwnerRefDto) -> Self {
        Self {
            tenant_id: dto.tenant_id,
            owner_id: dto.owner_id,
        }
    }
}

impl From<OwnerRef> for OwnerRefDto {
    fn from(o: OwnerRef) -> Self {
        Self {
            tenant_id: o.tenant_id,
            owner_id: o.owner_id,
        }
    }
}

#[derive(Debug, Clone)]
#[api_dto(request, response)]
pub struct FileMetaDto {
    pub name: String,
    pub mime_type: String,
    pub gts_file_type: String,
    pub size_bytes: Option<u64>,
    pub custom_metadata: BTreeMap<String, String>,
}

impl From<FileMetaDto> for FileMeta {
    fn from(d: FileMetaDto) -> Self {
        Self {
            name: d.name,
            mime_type: d.mime_type,
            gts_file_type: d.gts_file_type,
            size_bytes: d.size_bytes,
            custom_metadata: d.custom_metadata,
        }
    }
}

impl From<FileMeta> for FileMetaDto {
    fn from(m: FileMeta) -> Self {
        Self {
            name: m.name,
            mime_type: m.mime_type,
            gts_file_type: m.gts_file_type,
            size_bytes: m.size_bytes,
            custom_metadata: m.custom_metadata,
        }
    }
}

#[derive(Debug, Clone)]
#[api_dto(request, response)]
pub struct UrlParamsDto {
    pub expires_in_seconds: u64,
    pub content_disposition: Option<String>,
    pub content_type_override: Option<String>,
    pub allowed_client_cidrs: Vec<String>,
    pub refresh_etag: bool,
}

impl From<UrlParamsDto> for UrlParams {
    fn from(d: UrlParamsDto) -> Self {
        Self {
            expires_in_seconds: d.expires_in_seconds,
            content_disposition: d.content_disposition,
            content_type_override: d.content_type_override,
            allowed_client_cidrs: d.allowed_client_cidrs,
            refresh_etag: d.refresh_etag,
        }
    }
}

#[derive(Debug, Clone)]
#[api_dto(response)]
pub struct PresignedUploadHandleDto {
    pub file_id: Uuid,
    pub upload_url: String,
    pub etag_pinned: String,
    pub expires_at: OffsetDateTime,
}

impl From<PresignedUploadHandle> for PresignedUploadHandleDto {
    fn from(h: PresignedUploadHandle) -> Self {
        Self {
            file_id: h.file_id,
            upload_url: h.upload_url,
            etag_pinned: h.etag_pinned,
            expires_at: h.expires_at,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FileStatusDto {
    PendingUpload,
    Uploaded,
}

impl From<FileStatus> for FileStatusDto {
    fn from(s: FileStatus) -> Self {
        match s {
            FileStatus::PendingUpload => Self::PendingUpload,
            FileStatus::Uploaded => Self::Uploaded,
        }
    }
}

impl From<FileStatusDto> for FileStatus {
    fn from(d: FileStatusDto) -> Self {
        match d {
            FileStatusDto::PendingUpload => Self::PendingUpload,
            FileStatusDto::Uploaded => Self::Uploaded,
        }
    }
}

#[derive(Debug, Clone)]
#[api_dto(response)]
pub struct FileInfoDto {
    pub file_id: Uuid,
    pub backend_id: Uuid,
    pub file_path: String,
    pub owner: OwnerRefDto,
    pub meta: FileMetaDto,
    pub status: FileStatusDto,
    pub etag: String,
    pub size_bytes: u64,
    pub created_at: OffsetDateTime,
    pub modified_at: OffsetDateTime,
    pub upload_expires_at: Option<OffsetDateTime>,
}

impl From<FileInfo> for FileInfoDto {
    fn from(f: FileInfo) -> Self {
        Self {
            file_id: f.file_id,
            backend_id: f.backend_id,
            file_path: f.file_path,
            owner: f.owner.into(),
            meta: f.meta.into(),
            status: f.status.into(),
            etag: f.etag,
            size_bytes: f.size_bytes,
            created_at: f.created_at,
            modified_at: f.modified_at,
            upload_expires_at: f.upload_expires_at,
        }
    }
}

#[derive(Debug, Clone)]
#[api_dto(response)]
pub struct FileListDto {
    pub items: Vec<FileInfoDto>,
    pub next_cursor: Option<String>,
}

/// Unified `PUT /files/{file_id}` body.
///
/// EITHER a status commit (`status` + `new_etag`) OR a metadata replace
/// (any subset of `name`, `mime_type`, `custom_metadata`). The server
/// rejects bodies that mix the two.
#[derive(Debug, Clone, Default)]
#[api_dto(request)]
pub struct FileUpdateRequest {
    pub status: Option<FileStatusDto>,
    pub new_etag: Option<String>,
    pub name: Option<String>,
    pub mime_type: Option<String>,
    pub custom_metadata: Option<BTreeMap<String, String>>,
}

impl FileUpdateRequest {
    /// `true` if any status-branch field is set.
    pub fn has_status_branch(&self) -> bool {
        self.status.is_some() || self.new_etag.is_some()
    }
    /// `true` if any metadata-branch field is set.
    pub fn has_metadata_branch(&self) -> bool {
        self.name.is_some() || self.mime_type.is_some() || self.custom_metadata.is_some()
    }
    /// Project the metadata branch into a `FileMetaUpdate` (consumes
    /// metadata fields).
    pub fn into_metadata_update(self) -> FileMetaUpdate {
        FileMetaUpdate {
            name: self.name,
            mime_type: self.mime_type,
            custom_metadata: self.custom_metadata,
        }
    }
}

// ── Presign batch (upload + download) ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PresignItemDto {
    Upload(PresignUploadItemDto),
    Download(PresignDownloadItemDto),
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PresignUploadItemDto {
    pub backend_id: Option<Uuid>,
    pub owner: OwnerRefDto,
    pub file_path: String,
    pub meta: FileMetaDto,
    pub params: UrlParamsDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PresignDownloadItemDto {
    pub file_id: Uuid,
    pub params: UrlParamsDto,
    pub etag: Option<String>,
}

#[derive(Debug, Clone)]
#[api_dto(request)]
pub struct PresignBatchRequest {
    pub items: Vec<PresignItemDto>,
}

#[derive(Debug, Clone)]
#[api_dto(response)]
pub struct PresignedDownloadDto {
    pub url: String,
    pub expires_at: OffsetDateTime,
    pub is_public: bool,
}

impl From<PresignedDownload> for PresignedDownloadDto {
    fn from(d: PresignedDownload) -> Self {
        Self {
            url: d.url,
            expires_at: d.expires_at,
            is_public: d.is_public,
        }
    }
}

/// Per-item outcome inside the unified presign batch response. Either
/// `ok_upload`/`ok_download` is set, or `error` is set.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PresignOutcomeDto {
    pub kind: String,
    pub ok_upload: Option<PresignedUploadHandleDto>,
    pub ok_download: Option<PresignedDownloadDto>,
    pub error: Option<String>,
}

impl PresignOutcomeDto {
    pub fn upload_ok(handle: PresignedUploadHandleDto) -> Self {
        Self {
            kind: "upload".to_owned(),
            ok_upload: Some(handle),
            ok_download: None,
            error: None,
        }
    }
    pub fn upload_err(msg: String) -> Self {
        Self {
            kind: "upload".to_owned(),
            ok_upload: None,
            ok_download: None,
            error: Some(msg),
        }
    }
    pub fn download_ok(d: PresignedDownloadDto) -> Self {
        Self {
            kind: "download".to_owned(),
            ok_upload: None,
            ok_download: Some(d),
            error: None,
        }
    }
    pub fn download_err(msg: String) -> Self {
        Self {
            kind: "download".to_owned(),
            ok_upload: None,
            ok_download: None,
            error: Some(msg),
        }
    }
}

impl From<PresignDownloadOutcome> for PresignOutcomeDto {
    fn from(o: PresignDownloadOutcome) -> Self {
        match o.result {
            Ok(d) => Self::download_ok(d.into()),
            Err(e) => Self::download_err(e.to_string()),
        }
    }
}

#[derive(Debug, Clone)]
#[api_dto(response)]
pub struct PresignBatchResponse {
    pub items: Vec<PresignOutcomeDto>,
}

#[derive(Debug, Clone, Default)]
#[api_dto(request)]
pub struct ListFilesQueryDto {
    pub owner_id: Option<Uuid>,
    pub backend_id: Option<Uuid>,
    pub mime_type: Option<String>,
    pub gts_file_type: Option<String>,
    pub created_after: Option<OffsetDateTime>,
    pub created_before: Option<OffsetDateTime>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}
