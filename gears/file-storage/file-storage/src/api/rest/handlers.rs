//! Axum handlers for the control-plane REST API. Handlers stay thin: extract,
//! call the service, map to a DTO. All error mapping flows through
//! `From<DomainError> for CanonicalError` (see `error.rs`).

use std::sync::Arc;

use axum::Extension;
use axum::extract::{Path, Query};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::IntoResponse;
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use file_storage_sdk::{CustomMetadataPatch, NewFile, OwnerFilter, OwnerKind};

use super::dto::{
    BindReq, CreateFileReq, CreateRetentionRuleReq, DownloadTicketDto, EffectivePolicyDto, FileDto,
    FileDtoList, InitiateMultipartReq, MigrateBackendReq, MissingPartDto, MultipartCompleteDto,
    MultipartPartPlanDto, MultipartPlanDto, MultipartStatusDto, PolicyDto, ReceivedPartDto,
    RetentionRuleDto, RetentionRuleDtoList, SetPolicyReq, StorageDto, StorageDtoList,
    TransferOwnershipReq, UpdateMetadataReq, UploadTicketDto, VersionDto, VersionDtoList,
};
use crate::domain::error::DomainError;
use crate::domain::etag;
use crate::domain::multipart::{MultipartPlan, MultipartUploadStatus};
use crate::domain::multipart_service::MultipartService;
use crate::domain::policy::{PolicyScope, RetentionScope};
use crate::domain::policy_service::PolicyService;
use crate::domain::service::FileService;
use crate::infra::signed_url::{Op, Verifier};

type Svc = Extension<Arc<FileService>>;
type MultiSvc = Extension<Arc<MultipartService>>;
type PolicySvc = Extension<Arc<PolicyService>>;
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

/// Query params for `GET /files/{id}/versions`.
#[derive(Debug, Deserialize)]
pub struct ListVersionsQuery {
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

fn header_str(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// Shared-secret credential for the s2s finalize/report-part callbacks (ADR-0003);
/// interim until `toolkit-security::internal_auth` can replace it.
///
/// The secret (`FileStorageConfig::finalize_internal_secret`) is mandatory: `verify`
/// always requires a matching `x-fs-internal-token` on top of the signed upload token.
pub struct FinalizeAuth {
    secret: String,
}

impl FinalizeAuth {
    #[must_use]
    pub fn new(secret: String) -> Self {
        Self { secret }
    }

    /// Verify the `x-fs-internal-token` header; the comparison is constant-time.
    pub fn verify(&self, headers: &HeaderMap) -> Result<(), DomainError> {
        let expected = self.secret.as_str();
        let provided = headers
            .get("x-fs-internal-token")
            .and_then(|v| v.to_str().ok());
        let matches = provided.is_some_and(|provided| {
            // Constant-time comparator; only available via a deprecated re-export in
            // ring 0.17, suppressed here rather than adding a crate for one call.
            #[allow(deprecated)]
            ring::constant_time::verify_slices_are_equal(expected.as_bytes(), provided.as_bytes())
                .is_ok()
        });
        if matches {
            Ok(())
        } else {
            Err(DomainError::token_invalid(
                "finalize requires internal credential",
            ))
        }
    }
}

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
    // Re-read so the response carries the full custom metadata.
    let (file, meta) = svc.get_file_with_metadata(&ctx, file_id).await?;
    Ok(Json(FileDto::from_parts(file, meta)))
}

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
) -> ApiResult<JsonBody<FileDtoList>> {
    let owner_kind = OwnerKind::parse(&q.owner_kind)
        .ok_or_else(|| DomainError::validation("owner_kind", "must be 'user' or 'app'"))?;
    let owner = OwnerFilter {
        owner_kind,
        owner_id: q.owner_id,
    };
    let files = svc
        .list_files(&ctx, owner, q.limit, q.offset.unwrap_or(0))
        .await?;
    let items = files
        .into_iter()
        .map(|f| FileDto::from_parts(f, vec![]))
        .collect();
    Ok(Json(FileDtoList(items)))
}

pub async fn list_versions(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Path(file_id): Path<Uuid>,
    Query(q): Query<ListVersionsQuery>,
) -> ApiResult<JsonBody<VersionDtoList>> {
    let versions = svc
        .list_versions(&ctx, file_id, q.limit, q.offset.unwrap_or(0))
        .await?;
    Ok(Json(VersionDtoList(
        versions.into_iter().map(VersionDto::from).collect(),
    )))
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

pub async fn update_metadata(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Path(file_id): Path<Uuid>,
    headers: HeaderMap,
    Json(req): Json<UpdateMetadataReq>,
) -> ApiResult<JsonBody<FileDto>> {
    let expected_meta_version = match header_str(&headers, "if-match-metadata") {
        Some(s) => Some(s.trim().trim_matches('"').parse::<i64>().map_err(|_| {
            DomainError::validation("if-match-metadata", "must be an integer version")
        })?),
        None => None,
    };
    let patch = CustomMetadataPatch {
        entries: req.custom_metadata.into_iter().collect(),
    };
    svc.update_metadata(&ctx, file_id, patch, expected_meta_version)
        .await?;
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

pub async fn list_storages(Extension(svc): Svc) -> ApiResult<JsonBody<StorageDtoList>> {
    let items = svc
        .list_backends()
        .into_iter()
        .map(|(id, caps)| StorageDto::new(id, caps))
        .collect();
    Ok(Json(StorageDtoList(items)))
}

pub async fn get_storage(
    Extension(svc): Svc,
    Path(storage_id): Path<String>,
) -> ApiResult<JsonBody<StorageDto>> {
    let (id, caps) = svc.get_backend(&storage_id)?;
    Ok(Json(StorageDto::new(id, caps)))
}

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
pub async fn get_policy(
    Extension(ctx): Ctx,
    Extension(svc): PolicySvc,
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
pub async fn set_policy(
    Extension(ctx): Ctx,
    Extension(svc): PolicySvc,
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
pub async fn get_effective_policy(
    Extension(ctx): Ctx,
    Extension(svc): PolicySvc,
    Query(q): Query<EffectivePolicyQuery>,
) -> ApiResult<JsonBody<EffectivePolicyDto>> {
    let ep = svc.get_effective_policy(&ctx, q.user_owner_id).await?;
    Ok(Json(EffectivePolicyDto::from(ep)))
}

/// `GET /retention-rules` — list all retention rules for the caller's tenant.
pub async fn list_retention_rules(
    Extension(ctx): Ctx,
    Extension(svc): PolicySvc,
) -> ApiResult<JsonBody<RetentionRuleDtoList>> {
    let rules = svc.list_retention_rules(&ctx).await?;
    Ok(Json(RetentionRuleDtoList(
        rules.into_iter().map(RetentionRuleDto::from).collect(),
    )))
}

/// `POST /retention-rules` — create a new retention rule.
pub async fn create_retention_rule(
    uri: Uri,
    Extension(ctx): Ctx,
    Extension(svc): PolicySvc,
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
pub async fn delete_retention_rule(
    Extension(ctx): Ctx,
    Extension(svc): PolicySvc,
    Path(rule_id): Path<Uuid>,
) -> ApiResult<impl axum::response::IntoResponse> {
    let removed = svc.delete_retention_rule(&ctx, rule_id).await?;
    if removed {
        Ok(no_content().into_response())
    } else {
        Err(DomainError::retention_rule_not_found(rule_id).into())
    }
}

fn plan_to_dto(p: MultipartPlan) -> MultipartPlanDto {
    MultipartPlanDto {
        upload_id: p.upload_id,
        version_id: p.version_id,
        part_hash_algorithm: p.part_hash_algorithm,
        part_size: p.part_size,
        parts: p
            .parts
            .into_iter()
            .map(|pp| MultipartPartPlanDto {
                part_number: pp.part_number,
                offset: pp.offset,
                size: pp.size,
                upload_url: pp.upload_url,
            })
            .collect(),
        expires_at: p.expires_at,
    }
}

/// `POST /files/{id}/multipart` — initiate a multipart upload; returns the parts plan
/// with per-part signed sidecar URLs.
pub async fn initiate_multipart(
    Extension(ctx): Ctx,
    Extension(svc): MultiSvc,
    Path(file_id): Path<Uuid>,
    Json(req): Json<InitiateMultipartReq>,
) -> ApiResult<JsonBody<MultipartPlanDto>> {
    let plan = svc
        .initiate_multipart_upload(
            &ctx,
            file_id,
            &req.declared_mime,
            req.declared_size,
            req.preferred_part_size,
            req.concurrency,
        )
        .await?;
    Ok(Json(plan_to_dto(plan)))
}

/// `POST /files/{id}/multipart/{upload_id}/complete` — finalize all parts.
///
/// Returns the bound version's id, size, and ADR-0006 composite hash. `If-Match` is
/// optional: a concrete value is checked against the current content `ETag`; `*` or
/// omission is unconditional.
pub async fn complete_multipart(
    Extension(ctx): Ctx,
    Extension(svc): MultiSvc,
    Path((file_id, upload_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
) -> ApiResult<JsonBody<MultipartCompleteDto>> {
    let if_match = header_str(&headers, "if-match");
    let completed = svc
        .complete_multipart_upload(&ctx, file_id, upload_id, if_match.as_deref())
        .await?;
    Ok(Json(MultipartCompleteDto {
        version_id: completed.version_id,
        size: completed.size,
        hash_algorithm: completed.hash_algorithm.to_owned(),
        content_hash: hex::encode(&completed.content_hash),
        hash_mode: completed.hash_mode.as_str().to_owned(),
        part_count: completed.part_count,
        manifest: completed.manifest,
    }))
}

fn status_to_dto(s: MultipartUploadStatus) -> MultipartStatusDto {
    MultipartStatusDto {
        upload_id: s.upload_id,
        version_id: s.version_id,
        state: s.state.as_str().to_owned(),
        declared_mime: s.declared_mime,
        declared_size: s.declared_size,
        part_size: s.part_size,
        created_at: s.created_at,
        expires_at: s.expires_at,
        received: s
            .received
            .into_iter()
            .map(|p| ReceivedPartDto {
                part_number: p.part_number,
                size: p.size,
                uploaded_at: p.uploaded_at,
            })
            .collect(),
        missing: s
            .missing
            .into_iter()
            .map(|p| MissingPartDto {
                part_number: p.part_number,
                offset: p.offset,
                size: p.size,
                upload_url: p.upload_url,
            })
            .collect(),
    }
}

/// `GET /files/{id}/multipart/{upload_id}` — state, received parts, and (while
/// resumable) fresh URLs for the missing parts.
pub async fn introspect_multipart(
    Extension(ctx): Ctx,
    Extension(svc): MultiSvc,
    Path((file_id, upload_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<JsonBody<MultipartStatusDto>> {
    let status = svc
        .introspect_multipart_upload(&ctx, file_id, upload_id)
        .await?;
    Ok(Json(status_to_dto(status)))
}

/// `DELETE /files/{id}/multipart/{upload_id}` — abort a multipart upload.
pub async fn abort_multipart(
    Extension(ctx): Ctx,
    Extension(svc): MultiSvc,
    Path((file_id, upload_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    svc.abort_multipart_upload(&ctx, file_id, upload_id).await?;
    Ok(no_content().into_response())
}

/// `POST /files/{id}/migrate` — migrate a file's content to a different backend.
///
/// Non-versioned files only.
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

/// `POST /files/{id}/transfer` — transfer ownership of a file to a new owner.
pub async fn transfer_ownership(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Path(file_id): Path<Uuid>,
    Json(req): Json<TransferOwnershipReq>,
) -> ApiResult<JsonBody<FileDto>> {
    let new_owner_kind = file_storage_sdk::OwnerKind::parse(&req.new_owner_kind)
        .ok_or_else(|| DomainError::validation("new_owner_kind", "must be 'user' or 'app'"))?;
    // Read metadata before the transfer: afterwards the caller may lose read access
    // under the new owner, and a failed re-read must not yield empty metadata.
    let (_, meta) = svc.get_file_with_metadata(&ctx, file_id).await?;
    let file = svc
        .transfer_ownership(&ctx, file_id, new_owner_kind, req.new_owner_id)
        .await?;
    Ok(Json(FileDto::from_parts(file, meta)))
}

/// Request body for the data-plane finalize endpoint.
/// The sidecar posts the measured size and SHA-256 hash after a successful PUT.
#[derive(Debug, serde::Deserialize)]
pub struct FinalizeUploadReq {
    /// Byte length of the uploaded content.
    pub size: i64,
    /// SHA-256 hash of the uploaded content, hex-encoded.
    pub hash_hex: String,
}

/// `POST /files/{file_id}/versions/{version_id}/finalize`
///
/// Authenticated by the signed upload token in `x-fs-token` (no user JWT). Called by the
/// sidecar after a successful `PUT` to report size + hash and make the version `available`.
pub async fn finalize_version(
    Extension(svc): Svc,
    Extension(verifier): Extension<Arc<Verifier>>,
    Extension(finalize_auth): Extension<Arc<FinalizeAuth>>,
    Path((file_id, version_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    Json(req): Json<FinalizeUploadReq>,
) -> ApiResult<impl IntoResponse> {
    let token = headers
        .get("x-fs-token")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(|| DomainError::token_invalid("missing x-fs-token header"))?;

    let claims = verifier
        .verify(&token, OffsetDateTime::now_utc())
        .map_err(|e| DomainError::token_invalid(e.to_string()))?;

    // The token must authorize a PUT to exactly this (file_id, version_id).
    if claims.op != Op::Put || claims.file_id != file_id || claims.version_id != version_id {
        return Err(DomainError::token_invalid(
            "token does not authorize finalization of this version",
        )
        .into());
    }

    // The reported size and hash are trusted on the strength of this credential.
    finalize_auth.verify(&headers)?;

    // Log the sidecar-propagated `x-request-id` to join with the sidecar's own logs.
    let request_id = headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    tracing::info!(
        request_id,
        %file_id,
        %version_id,
        "finalize_version: sidecar callback received"
    );

    let hash_value = hex::decode(&req.hash_hex)
        .map_err(|_| DomainError::validation("hash_hex", "must be valid hex-encoded SHA-256"))?;
    if hash_value.len() != 32 {
        return Err(DomainError::validation(
            "hash_hex",
            "must decode to exactly 32 bytes (SHA-256)",
        )
        .into());
    }

    svc.finalize_upload_by_token(&claims, req.size, hash_value)
        .await?;

    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Request body for the data-plane report-part endpoint.
///
/// The sidecar posts this after writing a part's bytes to the backend.
#[derive(Debug, serde::Deserialize)]
pub struct ReportPartReq {
    /// Backend-assigned `ETag` for this part (opaque, backend-specific).
    pub backend_etag: String,
    /// SHA-256 hash of the part's bytes, hex-encoded.
    pub hash_hex: String,
    /// Byte length of the part.
    pub size: i64,
}

/// `POST /files/{file_id}/versions/{version_id}/multipart/{upload_id}/parts/{part_number}/report`
///
/// Authenticated like `finalize_version`, with a signed `multipart_part` token. Records
/// the part row that `complete_multipart_upload` assembles from.
pub async fn report_multipart_part(
    Extension(msvc): MultiSvc,
    Extension(verifier): Extension<Arc<Verifier>>,
    Extension(finalize_auth): Extension<Arc<FinalizeAuth>>,
    Path((file_id, version_id, upload_id, part_number)): Path<(Uuid, Uuid, Uuid, u32)>,
    headers: HeaderMap,
    Json(req): Json<ReportPartReq>,
) -> ApiResult<impl IntoResponse> {
    let token = headers
        .get("x-fs-token")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(|| DomainError::token_invalid("missing x-fs-token header"))?;

    let claims = verifier
        .verify(&token, OffsetDateTime::now_utc())
        .map_err(|e| DomainError::token_invalid(e.to_string()))?;

    // The token must authorize exactly this (file, version, upload, part).
    if claims.op != Op::MultipartPart
        || claims.file_id != file_id
        || claims.version_id != version_id
        || claims.multipart.upload_id != upload_id
        || claims.multipart.part_number != part_number
    {
        return Err(
            DomainError::token_invalid("token does not authorize reporting this part").into(),
        );
    }

    // Same shared-secret gate as `finalize_version`.
    finalize_auth.verify(&headers)?;

    let request_id = headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    tracing::info!(
        request_id,
        %file_id,
        %version_id,
        %upload_id,
        part_number,
        "report_multipart_part: sidecar callback received"
    );

    let hash_value = hex::decode(&req.hash_hex)
        .map_err(|_| DomainError::validation("hash_hex", "must be valid hex-encoded SHA-256"))?;

    msvc.report_part(&claims, req.backend_etag, hash_value, req.size)
        .await?;

    Ok(StatusCode::NO_CONTENT.into_response())
}
