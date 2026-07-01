//! Axum handlers for the control-plane REST API. Handlers stay thin: extract,
//! call the service, map to a DTO. All error mapping flows through
//! `From<DomainError> for CanonicalError` (see `error.rs`).

use std::sync::Arc;

use axum::Extension;
use axum::extract::{Path, Query};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::IntoResponse;
use serde::Deserialize;
use uuid::Uuid;

use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use file_storage_sdk::{CustomMetadataPatch, NewFile, OwnerFilter, OwnerKind};

use super::dto::{
    BindReq, CreateFileReq, CreateRetentionRuleReq, DownloadTicketDto, EffectivePolicyDto, FileDto,
    InitiateMultipartReq, MigrateBackendReq, MultipartSessionDto, PolicyDto, RetentionRuleDto,
    SetPolicyReq, StorageDto, TransferOwnershipReq, UpdateMetadataReq, UploadPartDto,
    UploadTicketDto, VersionDto,
};
use crate::domain::error::DomainError;
use crate::domain::etag;
use crate::domain::multipart::MultipartUploadSession;
use crate::domain::policy::{PolicyScope, RetentionScope};
use crate::domain::service::FileService;

type Svc = Extension<Arc<FileService>>;
type Ctx = Extension<SecurityContext>;

/// Query params for `GET /files`.
#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub owner_kind: String,
    pub owner_id: Uuid,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

/// Query params for `GET /files/{id}/download-url`.
#[derive(Debug, Deserialize)]
pub struct DownloadQuery {
    pub version_id: Option<Uuid>,
}

fn header_str(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

// ── create + presign ─────────────────────────────────────────────────────────

pub async fn create_file(
    uri: Uri,
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Json(req): Json<CreateFileReq>,
) -> ApiResult<impl IntoResponse> {
    let owner_kind = req
        .parse_owner_kind()
        .ok_or_else(|| DomainError::validation("owner_kind", "must be 'user' or 'app'"))?;
    let new = NewFile {
        owner_kind,
        owner_id: req.owner_id,
        name: req.name,
        gts_file_type: req.gts_file_type,
        mime_type: req.mime_type,
        custom_metadata: req
            .custom_metadata
            .into_iter()
            .map(|e| file_storage_sdk::CustomMetadataEntry {
                key: e.key,
                value: e.value,
            })
            .collect(),
    };
    let ticket = svc.create_file(&ctx, new, req.idempotency_key).await?;
    let id = ticket.file_id.to_string();
    Ok(created_json(
        UploadTicketDto {
            file_id: ticket.file_id,
            version_id: ticket.version_id,
            upload_url: ticket.upload_url,
        },
        &uri,
        &id,
    )
    .into_response())
}

pub async fn presign_version(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Path(file_id): Path<Uuid>,
) -> ApiResult<JsonBody<UploadTicketDto>> {
    let ticket = svc.presign_version(&ctx, file_id).await?;
    Ok(Json(UploadTicketDto {
        file_id: ticket.file_id,
        version_id: ticket.version_id,
        upload_url: ticket.upload_url,
    }))
}

pub async fn bind(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Path(file_id): Path<Uuid>,
    headers: HeaderMap,
    Json(req): Json<BindReq>,
) -> ApiResult<JsonBody<FileDto>> {
    let if_match = header_str(&headers, "if-match");
    svc.bind(&ctx, file_id, req.version_id, if_match.as_deref())
        .await?;
    // Re-read with metadata so the mutation response round-trips the full file
    // state instead of reporting empty custom metadata.
    let (file, meta) = svc.get_file_with_metadata(&ctx, file_id).await?;
    Ok(Json(FileDto::from_parts(file, meta)))
}

// ── reads ─────────────────────────────────────────────────────────────────────

pub async fn get_file(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Path(file_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let (file, meta) = svc.get_file_with_metadata(&ctx, file_id).await?;
    let etag = etag::etag_for(&file);
    let etag_header = etag
        .as_deref()
        .and_then(|tag| HeaderValue::from_str(tag).ok());

    // Conditional GET: If-None-Match → 304 (still carrying the ETag header).
    if let (Some(inm), Some(tag)) = (header_str(&headers, "if-none-match"), etag.as_deref()) {
        let inm = inm.trim();
        if inm == "*" || inm == tag {
            let mut resp = StatusCode::NOT_MODIFIED.into_response();
            if let Some(v) = etag_header {
                resp.headers_mut().insert(header::ETAG, v);
            }
            return Ok(resp);
        }
    }

    let dto = FileDto::from_parts(file, meta);
    let mut resp = Json(dto).into_response();
    if let Some(v) = etag_header {
        resp.headers_mut().insert(header::ETAG, v);
    }
    Ok(resp)
}

pub async fn list_files(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Query(q): Query<ListQuery>,
) -> ApiResult<JsonBody<Vec<FileDto>>> {
    let owner_kind = OwnerKind::parse(&q.owner_kind)
        .ok_or_else(|| DomainError::validation("owner_kind", "must be 'user' or 'app'"))?;
    let owner = OwnerFilter {
        owner_kind,
        owner_id: q.owner_id,
    };
    let files = svc
        .list_files(&ctx, owner, q.limit, q.offset.unwrap_or(0))
        .await?;
    let dtos = files
        .into_iter()
        .map(|f| FileDto::from_parts(f, vec![]))
        .collect();
    Ok(Json(dtos))
}

pub async fn list_versions(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Path(file_id): Path<Uuid>,
) -> ApiResult<JsonBody<Vec<VersionDto>>> {
    let versions = svc.list_versions(&ctx, file_id).await?;
    Ok(Json(versions.into_iter().map(VersionDto::from).collect()))
}

pub async fn download_url(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Path(file_id): Path<Uuid>,
    Query(q): Query<DownloadQuery>,
) -> ApiResult<JsonBody<DownloadTicketDto>> {
    let ticket = svc.download_url(&ctx, file_id, q.version_id).await?;
    Ok(Json(DownloadTicketDto {
        download_url: ticket.download_url,
        etag: ticket.etag,
        version_id: ticket.version_id,
    }))
}

// ── mutations ──────────────────────────────────────────────────────────────────

pub async fn update_metadata(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Path(file_id): Path<Uuid>,
    headers: HeaderMap,
    Json(req): Json<UpdateMetadataReq>,
) -> ApiResult<JsonBody<FileDto>> {
    let expected_meta_version = header_str(&headers, "if-match-metadata")
        .and_then(|s| s.trim().trim_matches('"').parse::<i64>().ok());
    let patch = CustomMetadataPatch {
        entries: req.custom_metadata.into_iter().collect(),
    };
    svc.update_metadata(&ctx, file_id, patch, expected_meta_version)
        .await?;
    // Re-read with metadata so the response reflects the patched state.
    let (file, meta) = svc.get_file_with_metadata(&ctx, file_id).await?;
    Ok(Json(FileDto::from_parts(file, meta)))
}

pub async fn delete_file(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Path(file_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let if_match = header_str(&headers, "if-match");
    svc.delete_file(&ctx, file_id, if_match.as_deref()).await?;
    Ok(no_content().into_response())
}

pub async fn delete_version(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Path((file_id, version_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    svc.delete_version(&ctx, file_id, version_id).await?;
    Ok(no_content().into_response())
}

// ── storages ────────────────────────────────────────────────────────────────────

pub async fn list_storages(Extension(svc): Svc) -> ApiResult<JsonBody<Vec<StorageDto>>> {
    let storages = svc
        .list_backends()
        .into_iter()
        .map(|(id, caps)| StorageDto::new(id, caps))
        .collect();
    Ok(Json(storages))
}

pub async fn get_storage(
    Extension(svc): Svc,
    Path(storage_id): Path<String>,
) -> ApiResult<JsonBody<StorageDto>> {
    let (id, caps) = svc.get_backend(&storage_id)?;
    Ok(Json(StorageDto::new(id, caps)))
}

// ── policy (P2-M1) ──────────────────────────────────────────────────────────────

/// Query params for `GET /policy` (own policy for a given scope).
#[derive(Debug, Deserialize)]
pub struct GetPolicyQuery {
    /// `"tenant"` or `"user"`.
    pub scope: String,
    /// Required when `scope = "user"`.
    pub scope_owner_id: Option<Uuid>,
}

/// Query params for `GET /policy/effective`.
#[derive(Debug, Deserialize)]
pub struct EffectivePolicyQuery {
    /// The user owner id to include in the effective resolution (optional).
    pub user_owner_id: Option<Uuid>,
}

/// `GET /policy` — return the raw own policy for a scope.
///
/// @cpt-cf-file-storage-usecase-configure-policy
pub async fn get_policy(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Query(q): Query<GetPolicyQuery>,
) -> ApiResult<impl axum::response::IntoResponse> {
    let policy_scope = PolicyScope::parse(&q.scope)
        .ok_or_else(|| DomainError::validation("scope", "must be 'tenant' or 'user'"))?;
    let stored = svc
        .get_own_policy(&ctx, policy_scope, q.scope_owner_id)
        .await?;
    match stored {
        Some(p) => Ok((StatusCode::OK, Json(PolicyDto::from(p))).into_response()),
        None => Ok(StatusCode::NO_CONTENT.into_response()),
    }
}

/// `PUT /policy` — upsert the policy for a scope.
///
/// @cpt-cf-file-storage-usecase-configure-policy
pub async fn set_policy(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Json(req): Json<SetPolicyReq>,
) -> ApiResult<JsonBody<PolicyDto>> {
    let policy_scope = PolicyScope::parse(&req.scope)
        .ok_or_else(|| DomainError::validation("scope", "must be 'tenant' or 'user'"))?;
    let body = req.body.into();
    let stored = svc
        .set_policy(&ctx, policy_scope, req.scope_owner_id, body)
        .await?;
    Ok(Json(PolicyDto::from(stored)))
}

/// `GET /policy/effective` — compute the effective (most-restrictive) policy.
///
/// @cpt-cf-file-storage-usecase-configure-policy
pub async fn get_effective_policy(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Query(q): Query<EffectivePolicyQuery>,
) -> ApiResult<JsonBody<EffectivePolicyDto>> {
    let ep = svc.get_effective_policy(&ctx, q.user_owner_id).await?;
    Ok(Json(EffectivePolicyDto::from(ep)))
}

// ── retention rules (P2-M1) ────────────────────────────────────────────────────

/// `GET /retention-rules` — list all retention rules for the caller's tenant.
///
/// @cpt-cf-file-storage-fr-retention-policies
pub async fn list_retention_rules(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
) -> ApiResult<JsonBody<Vec<RetentionRuleDto>>> {
    let rules = svc.list_retention_rules(&ctx).await?;
    Ok(Json(
        rules.into_iter().map(RetentionRuleDto::from).collect(),
    ))
}

/// `POST /retention-rules` — create a new retention rule.
///
/// @cpt-cf-file-storage-fr-retention-policies
pub async fn create_retention_rule(
    uri: Uri,
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Json(req): Json<CreateRetentionRuleReq>,
) -> ApiResult<impl axum::response::IntoResponse> {
    let retention_scope = RetentionScope::parse(&req.scope)
        .ok_or_else(|| DomainError::validation("scope", "must be 'tenant', 'user', or 'file'"))?;
    let body = req.body.into();
    let rule = svc
        .create_retention_rule(&ctx, retention_scope, req.scope_target_id, body)
        .await?;
    let id = rule.rule_id.to_string();
    Ok(created_json(RetentionRuleDto::from(rule), &uri, &id).into_response())
}

/// `DELETE /retention-rules/{rule_id}` — delete a retention rule.
///
/// @cpt-cf-file-storage-fr-retention-policies
pub async fn delete_retention_rule(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Path(rule_id): Path<Uuid>,
) -> ApiResult<impl axum::response::IntoResponse> {
    let removed = svc.delete_retention_rule(&ctx, rule_id).await?;
    if removed {
        Ok(no_content().into_response())
    } else {
        Err(DomainError::file_not_found(rule_id).into())
    }
}

// ── multipart upload (P2-M3) ────────────────────────────────────────────────────

fn session_to_dto(s: MultipartUploadSession) -> MultipartSessionDto {
    MultipartSessionDto {
        upload_id: s.upload_id,
        file_id: s.file_id,
        version_id: s.version_id,
        state: s.state.as_str().to_owned(),
        declared_mime: s.declared_mime,
        expires_at: s.expires_at,
    }
}

/// `POST /files/{id}/multipart` — initiate a multipart upload session.
///
/// @cpt-cf-file-storage-fr-multipart-upload
pub async fn initiate_multipart(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Path(file_id): Path<Uuid>,
    Json(req): Json<InitiateMultipartReq>,
) -> ApiResult<JsonBody<MultipartSessionDto>> {
    let session = svc
        .initiate_multipart_upload(&ctx, file_id, &req.declared_mime)
        .await?;
    Ok(Json(session_to_dto(session)))
}

/// `PUT /files/{id}/multipart/{upload_id}/parts/{part_number}` — upload one part.
///
/// @cpt-cf-file-storage-fr-multipart-upload
pub async fn upload_multipart_part(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Path((file_id, upload_id, part_number)): Path<(Uuid, Uuid, u32)>,
    body: axum::body::Bytes,
) -> ApiResult<JsonBody<UploadPartDto>> {
    let part = svc
        .upload_multipart_part(&ctx, file_id, upload_id, part_number, body)
        .await?;
    Ok(Json(UploadPartDto {
        part_number: part.part_number,
        backend_etag: part.backend_etag,
        size: part.size,
    }))
}

/// `POST /files/{id}/multipart/{upload_id}/complete` — finalize all parts.
///
/// @cpt-cf-file-storage-fr-multipart-upload
pub async fn complete_multipart(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Path((file_id, upload_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    svc.complete_multipart_upload(&ctx, file_id, upload_id)
        .await?;
    Ok(no_content().into_response())
}

/// `DELETE /files/{id}/multipart/{upload_id}` — abort a multipart upload.
///
/// @cpt-cf-file-storage-fr-multipart-upload
pub async fn abort_multipart(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Path((file_id, upload_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    svc.abort_multipart_upload(&ctx, file_id, upload_id).await?;
    Ok(no_content().into_response())
}

// ── backend migration (P2-M4) ─────────────────────────────────────────────────

/// `POST /files/{id}/migrate` — migrate a file's content to a different backend.
///
/// Non-versioned files only. Preserves identity and verifies content hash.
///
/// @cpt-cf-file-storage-fr-backend-migration
pub async fn migrate_backend(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Path(file_id): Path<Uuid>,
    Json(req): Json<MigrateBackendReq>,
) -> ApiResult<impl IntoResponse> {
    svc.migrate_backend(&ctx, file_id, &req.target_backend_id)
        .await?;
    Ok(no_content().into_response())
}

// ── ownership transfer (P2-M5) ────────────────────────────────────────────────

/// `POST /files/{id}/transfer` — transfer ownership of a file to a new owner.
///
/// @cpt-cf-file-storage-fr-ownership-transfer
pub async fn transfer_ownership(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Path(file_id): Path<Uuid>,
    Json(req): Json<TransferOwnershipReq>,
) -> ApiResult<JsonBody<FileDto>> {
    let new_owner_kind = file_storage_sdk::OwnerKind::parse(&req.new_owner_kind)
        .ok_or_else(|| DomainError::validation("new_owner_kind", "must be 'user' or 'app'"))?;
    // Capture metadata BEFORE the transfer. A transfer does not change custom
    // metadata, but afterwards the caller may no longer have read access under
    // the new owner — re-reading then and defaulting on failure would return a
    // 200 with empty `custom_metadata` for a file that actually has some.
    let (_, meta) = svc.get_file_with_metadata(&ctx, file_id).await?;
    let file = svc
        .transfer_ownership(&ctx, file_id, new_owner_kind, req.new_owner_id)
        .await?;
    Ok(Json(FileDto::from_parts(file, meta)))
}
