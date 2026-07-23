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
    MultipartCompletingDto, MultipartPartPlanDto, MultipartPlanDto, MultipartStatusDto, PolicyDto,
    ReceivedPartDto, RetentionRuleDto, RetentionRuleDtoList, SetPolicyReq, StorageDto,
    StorageDtoList, TransferOwnershipReq, UpdateMetadataReq, UploadTicketDto, VersionDto,
    VersionDtoList,
};
use crate::domain::error::DomainError;
use crate::domain::etag;
use crate::domain::multipart::{MultipartCompleteOutcome, MultipartPlan, MultipartUploadStatus};
use crate::domain::multipart_service::MultipartService;
use crate::domain::policy::{PolicyScope, RetentionScope};
use crate::domain::policy_service::PolicyService;
use crate::domain::service::FileService;
use crate::infra::content::hash_mode::HashMode;
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

/// Interim gear-local shared-secret credential for the s2s finalize/
/// report-part callback routes (P2 0.1 remaining — see
/// `docs/ADR/0003-…-sidecar-data-plane.md`'s trust-model section). This is a
/// stop-gap until the platform's `toolkit-security::internal_auth` profiles
/// are deployable in this gear; swap the comparator below for
/// `InternalAuthenticator` when that lands.
///
/// When `secret` is `None` (the default — `FileStorageConfig::finalize_internal_secret`
/// unset), [`FinalizeAuth::verify`] is a no-op: the signed upload token
/// (already verified by the caller) remains the sole authorization,
/// preserving pre-0.1 behavior. When `Some`, callers must additionally
/// present a matching `x-fs-internal-token` header.
pub struct FinalizeAuth {
    secret: Option<String>,
}

impl FinalizeAuth {
    #[must_use]
    pub fn new(secret: Option<String>) -> Self {
        Self { secret }
    }

    /// Verify the `x-fs-internal-token` header against the configured
    /// secret. No-op `Ok(())` when no secret is configured. Comparison is
    /// constant-time to avoid leaking the secret through response-timing
    /// side channels.
    pub fn verify(&self, headers: &HeaderMap) -> Result<(), DomainError> {
        let Some(expected) = self.secret.as_deref() else {
            return Ok(());
        };
        let provided = headers
            .get("x-fs-internal-token")
            .and_then(|v| v.to_str().ok());
        let matches = provided.is_some_and(|provided| {
            // `ring::constant_time::verify_slices_are_equal` is ring 0.17's
            // constant-time byte-slice comparator (this crate already
            // depends on `ring` via `infra::signed_url`). It lives under a
            // `#[deprecated]` re-export with no non-deprecated replacement
            // for a bare secret comparison, so the deprecation warning is
            // suppressed locally rather than adding a new crate (e.g.
            // `subtle`) for this one call.
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

// ── create + presign ─────────────────────────────────────────────────────────

/// `POST /files` — create a file and presign its first content upload
/// (upload-flow redesign).
///
/// * No `multipart` block (or a plan that collapses to one part): the
///   response carries a single-part `upload_url` (as before). With
///   `bind: "auto"` (the default) the token instructs the sidecar's finalize
///   to bind the first content itself under a `content_id IS NULL` CAS — the
///   whole upload is 2 requests (`POST /files` + `PUT`); the `PUT` response
///   echoes the outcome as `X-FS-Bound`/`ETag` headers.
/// * `multipart` block with a ≥2-part plan: the response carries the full
///   parts plan (`multipart` field — same shape as
///   `POST /files/{id}/multipart`), no single-part version is
///   pre-registered, and `complete` binds (for `bind: "auto"`) — N+2
///   requests total. Resume stays `GET /files/{id}/multipart/{upload_id}`.
pub async fn create_file(
    uri: Uri,
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Extension(msvc): MultiSvc,
    Json(req): Json<CreateFileReq>,
) -> ApiResult<impl IntoResponse> {
    let owner_kind = req
        .parse_owner_kind()
        .ok_or_else(|| DomainError::validation("owner_kind", "must be 'user' or 'app'"))?;
    let auto_bind = match req.bind.as_deref() {
        None | Some("auto") => true,
        Some("manual") => false,
        Some(_) => {
            return Err(
                DomainError::validation("bind", "must be 'auto' or 'manual'").into(),
            );
        }
    };
    let new = NewFile {
        owner_kind,
        owner_id: req.owner_id,
        name: req.name,
        gts_file_type: req.gts_file_type,
        mime_type: req.mime_type.clone(),
        custom_metadata: req
            .custom_metadata
            .into_iter()
            .map(|e| file_storage_sdk::CustomMetadataEntry {
                key: e.key,
                value: e.value,
            })
            .collect(),
    };

    if let Some(mp) = &req.multipart {
        // The idempotency record stores a single-part replay ticket; a
        // multipart plan does not fit that contract — reject rather than
        // silently ignoring one of the two.
        if req.idempotency_key.is_some() {
            return Err(DomainError::validation(
                "idempotency_key",
                "not supported together with the multipart intent block",
            )
            .into());
        }
        // Same plan computation the initiate path runs — decides up front
        // whether this is a real (≥2 parts) multipart upload. One-part plans
        // fall through to the ordinary single-part path below.
        let (_, planned_parts) = crate::domain::multipart::compute_plan(
            mp.declared_size,
            mp.preferred_part_size,
            None,
        )?;
        if planned_parts.len() >= 2 {
            // Create the file row only (no single-part pending version — the
            // multipart initiate registers its own; the old flow's abandoned
            // presign orphan disappears).
            let file_id = svc.create_file_bare(&ctx, new).await?;
            let plan = msvc
                .initiate_multipart_upload(
                    &ctx,
                    file_id,
                    &req.mime_type,
                    mp.declared_size,
                    mp.preferred_part_size,
                    mp.concurrency,
                    auto_bind,
                )
                .await?;
            let id = file_id.to_string();
            let version_id = plan.version_id;
            return Ok(created_json(
                UploadTicketDto {
                    file_id,
                    version_id,
                    upload_url: None,
                    multipart: Some(plan_to_dto(plan)),
                },
                &uri,
                &id,
            )
            .into_response());
        }
    }

    let ticket = svc
        .create_file(&ctx, new, req.idempotency_key, auto_bind)
        .await?;
    let id = ticket.file_id.to_string();
    Ok(created_json(
        UploadTicketDto {
            file_id: ticket.file_id,
            version_id: ticket.version_id,
            upload_url: Some(ticket.upload_url),
            multipart: None,
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
        upload_url: Some(ticket.upload_url),
        multipart: None,
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
    // Batched (one `IN (...)` query for the whole page) rather than one
    // `list_metadata` call per file — see `FileService::list_files_with_metadata`.
    let file_ids: Vec<Uuid> = files.iter().map(|f| f.file_id).collect();
    let mut metadata_by_file = svc.list_metadata_for_files(&file_ids).await?;
    let items = files
        .into_iter()
        .map(|f| {
            let meta = metadata_by_file.remove(&f.file_id).unwrap_or_default();
            FileDto::from_parts(f, meta)
        })
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
    // Attach the stored ADR-0006 offset-manifest to every
    // multipart-composite version on the page, fetched in one batched query
    // (mirrors list_files's list_metadata_for_files N+1 avoidance).
    let composite_ids: Vec<Uuid> = versions
        .iter()
        .filter(|v| v.hash_mode == HashMode::MULTIPART_COMPOSITE_SHA256)
        .map(|v| v.version_id)
        .collect();
    let mut manifests = svc.manifests_for_versions(&composite_ids).await?;
    Ok(Json(VersionDtoList(
        versions
            .into_iter()
            .map(|v| {
                let manifest = manifests.remove(&v.version_id);
                VersionDto::from_parts(v, manifest)
            })
            .collect(),
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

// ── mutations ──────────────────────────────────────────────────────────────────

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
/// @cpt-dod:cpt-cf-file-storage-dod-policy-get-put-endpoints:p1
// @cpt-begin:cpt-cf-file-storage-flow-policy-get-own:p1:inst-policy-get-request
pub async fn get_policy(
    Extension(ctx): Ctx,
    Extension(svc): PolicySvc,
    Query(q): Query<GetPolicyQuery>,
) -> ApiResult<impl axum::response::IntoResponse> {
    // @cpt-end:cpt-cf-file-storage-flow-policy-get-own:p1:inst-policy-get-request
    // @cpt-begin:cpt-cf-file-storage-flow-policy-get-own:p1:inst-policy-get-parse-scope
    let policy_scope = PolicyScope::parse(&q.scope)
        .ok_or_else(|| DomainError::validation("scope", "must be 'tenant' or 'user'"))?;
    // @cpt-end:cpt-cf-file-storage-flow-policy-get-own:p1:inst-policy-get-parse-scope
    let stored = svc
        .get_own_policy(&ctx, policy_scope, q.scope_owner_id)
        .await?;
    // @cpt-begin:cpt-cf-file-storage-flow-policy-get-own:p1:inst-policy-get-return
    match stored {
        Some(p) => Ok((StatusCode::OK, Json(PolicyDto::from(p))).into_response()),
        None => Ok(StatusCode::NO_CONTENT.into_response()),
    }
    // @cpt-end:cpt-cf-file-storage-flow-policy-get-own:p1:inst-policy-get-return
}

/// `PUT /policy` — upsert the policy for a scope.
///
/// @cpt-cf-file-storage-usecase-configure-policy
// @cpt-begin:cpt-cf-file-storage-flow-policy-set:p1:inst-policy-set-request
pub async fn set_policy(
    Extension(ctx): Ctx,
    Extension(svc): PolicySvc,
    Json(req): Json<SetPolicyReq>,
) -> ApiResult<JsonBody<PolicyDto>> {
    // @cpt-end:cpt-cf-file-storage-flow-policy-set:p1:inst-policy-set-request
    // @cpt-begin:cpt-cf-file-storage-flow-policy-set:p1:inst-policy-set-parse-scope
    let policy_scope = PolicyScope::parse(&req.scope)
        .ok_or_else(|| DomainError::validation("scope", "must be 'tenant' or 'user'"))?;
    // @cpt-end:cpt-cf-file-storage-flow-policy-set:p1:inst-policy-set-parse-scope
    let body = req.body.into();
    let stored = svc
        .set_policy(&ctx, policy_scope, req.scope_owner_id, body)
        .await?;
    // @cpt-begin:cpt-cf-file-storage-flow-policy-set:p1:inst-policy-set-return
    Ok(Json(PolicyDto::from(stored)))
    // @cpt-end:cpt-cf-file-storage-flow-policy-set:p1:inst-policy-set-return
}

/// `GET /policy/effective` — compute the effective (most-restrictive) policy.
///
/// @cpt-cf-file-storage-usecase-configure-policy
/// @cpt-dod:cpt-cf-file-storage-dod-policy-effective-endpoint:p1
// @cpt-begin:cpt-cf-file-storage-flow-policy-get-effective:p1:inst-policy-eff-request
pub async fn get_effective_policy(
    Extension(ctx): Ctx,
    Extension(svc): PolicySvc,
    Query(q): Query<EffectivePolicyQuery>,
) -> ApiResult<JsonBody<EffectivePolicyDto>> {
    // @cpt-end:cpt-cf-file-storage-flow-policy-get-effective:p1:inst-policy-eff-request
    let ep = svc.get_effective_policy(&ctx, q.user_owner_id).await?;
    // @cpt-begin:cpt-cf-file-storage-flow-policy-get-effective:p1:inst-policy-eff-return
    Ok(Json(EffectivePolicyDto::from(ep)))
    // @cpt-end:cpt-cf-file-storage-flow-policy-get-effective:p1:inst-policy-eff-return
}

// ── retention rules (P2-M1) ────────────────────────────────────────────────────

/// `GET /retention-rules` — list all retention rules for the caller's tenant.
///
/// @cpt-cf-file-storage-fr-retention-policies
/// @cpt-dod:cpt-cf-file-storage-dod-retention-rule-endpoints:p1
// @cpt-begin:cpt-cf-file-storage-flow-retention-list:p1:inst-retention-list-request
pub async fn list_retention_rules(
    Extension(ctx): Ctx,
    Extension(svc): PolicySvc,
) -> ApiResult<JsonBody<RetentionRuleDtoList>> {
    // @cpt-end:cpt-cf-file-storage-flow-retention-list:p1:inst-retention-list-request
    let rules = svc.list_retention_rules(&ctx).await?;
    // @cpt-begin:cpt-cf-file-storage-flow-retention-list:p1:inst-retention-list-return
    Ok(Json(RetentionRuleDtoList(
        rules.into_iter().map(RetentionRuleDto::from).collect(),
    )))
    // @cpt-end:cpt-cf-file-storage-flow-retention-list:p1:inst-retention-list-return
}

/// `POST /retention-rules` — create a new retention rule.
///
/// @cpt-cf-file-storage-fr-retention-policies
// @cpt-begin:cpt-cf-file-storage-flow-retention-create:p1:inst-retention-create-request
pub async fn create_retention_rule(
    uri: Uri,
    Extension(ctx): Ctx,
    Extension(svc): PolicySvc,
    Json(req): Json<CreateRetentionRuleReq>,
) -> ApiResult<impl axum::response::IntoResponse> {
    // @cpt-end:cpt-cf-file-storage-flow-retention-create:p1:inst-retention-create-request
    let retention_scope = RetentionScope::parse(&req.scope)
        .ok_or_else(|| DomainError::validation("scope", "must be 'tenant', 'user', or 'file'"))?;
    let body = req.body.into();
    let rule = svc
        .create_retention_rule(&ctx, retention_scope, req.scope_target_id, body)
        .await?;
    let id = rule.rule_id.to_string();
    // @cpt-begin:cpt-cf-file-storage-flow-retention-create:p1:inst-retention-create-return
    Ok(created_json(RetentionRuleDto::from(rule), &uri, &id).into_response())
    // @cpt-end:cpt-cf-file-storage-flow-retention-create:p1:inst-retention-create-return
}

/// `DELETE /retention-rules/{rule_id}` — delete a retention rule.
///
/// @cpt-cf-file-storage-fr-retention-policies
// @cpt-begin:cpt-cf-file-storage-flow-retention-delete:p1:inst-retention-delete-request
pub async fn delete_retention_rule(
    Extension(ctx): Ctx,
    Extension(svc): PolicySvc,
    Path(rule_id): Path<Uuid>,
) -> ApiResult<impl axum::response::IntoResponse> {
    // @cpt-end:cpt-cf-file-storage-flow-retention-delete:p1:inst-retention-delete-request
    let removed = svc.delete_retention_rule(&ctx, rule_id).await?;
    // @cpt-begin:cpt-cf-file-storage-flow-retention-delete:p1:inst-retention-delete-return
    if removed {
        Ok(no_content().into_response())
    } else {
        Err(DomainError::retention_rule_not_found(rule_id).into())
    }
    // @cpt-end:cpt-cf-file-storage-flow-retention-delete:p1:inst-retention-delete-return
}

// ── multipart upload (multipart-coordinator feature) ──────────────────────────

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

/// `POST /files/{id}/multipart` — initiate a server-authoritative multipart
/// upload session and return the parts plan with per-part signed sidecar URLs.
///
/// @cpt-cf-file-storage-fr-multipart-upload
/// @cpt-cf-file-storage-fr-size-limits-policy
/// @cpt-cf-file-storage-fr-storage-quota
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
            // Standalone initiate keeps the staged pre-redesign behaviour:
            // complete never binds; the client binds manually (this is the
            // "new version of an existing file" path, where the CAS target
            // is caller-controlled via bind's If-Match).
            false,
        )
        .await?;
    Ok(Json(plan_to_dto(plan)))
}

/// `POST /files/{id}/multipart/{upload_id}/complete` — finalize all parts.
///
/// Returns the bound version's id, size, and ADR-0006 composite hash (item
/// 3.3) instead of the previous bare `204`. `If-Match` is optional: a
/// concrete value is checked against the file's current content `ETag`; `*`
/// (or omission) is unconditional.
///
/// @cpt-cf-file-storage-fr-multipart-upload
pub async fn complete_multipart(
    Extension(ctx): Ctx,
    Extension(svc): MultiSvc,
    Path((file_id, upload_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
) -> ApiResult<axum::response::Response> {
    let if_match = header_str(&headers, "if-match");
    let outcome = svc
        .complete_multipart_upload(&ctx, file_id, upload_id, if_match.as_deref())
        .await?;
    // @cpt-begin:cpt-cf-file-storage-flow-multipart-complete:p1:inst-complete-return
    Ok(match outcome {
        MultipartCompleteOutcome::Completed(completed) => Json(MultipartCompleteDto {
            version_id: completed.version_id,
            size: completed.size,
            hash_algorithm: completed.hash_algorithm.to_owned(),
            content_hash: hex::encode(&completed.content_hash),
            hash_mode: completed.hash_mode.as_str().to_owned(),
            part_count: completed.part_count,
            manifest: completed.manifest,
            bind_state: completed.bind_state.as_str().to_owned(),
            etag: completed.etag,
            current_etag: completed.current_etag,
        })
        .into_response(),
        // Another caller holds the completion lease — poll by re-issuing
        // the same idempotent complete (Retry-After mirrors the body hint).
        MultipartCompleteOutcome::Completing { retry_after_secs } => {
            let mut resp = (
                StatusCode::ACCEPTED,
                Json(MultipartCompletingDto {
                    state: "completing".to_owned(),
                    retry_after_secs,
                }),
            )
                .into_response();
            if let Ok(v) = HeaderValue::from_str(&retry_after_secs.to_string()) {
                resp.headers_mut().insert(header::RETRY_AFTER, v);
            }
            resp
        }
    })
    // @cpt-end:cpt-cf-file-storage-flow-multipart-complete:p1:inst-complete-return
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

/// `GET /files/{id}/multipart/{upload_id}` — introspect a multipart upload
/// (item 3.4): current state, received parts, and (while resumable) fresh
/// resume URLs for the missing parts.
///
/// @cpt-cf-file-storage-fr-multipart-upload
/// @cpt-dod:cpt-cf-file-storage-dod-multipart-introspect:p2
// @cpt-begin:cpt-cf-file-storage-flow-multipart-introspect:p1:inst-introspect-request
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
// @cpt-end:cpt-cf-file-storage-flow-multipart-introspect:p1:inst-introspect-request

/// `DELETE /files/{id}/multipart/{upload_id}` — abort a multipart upload.
///
/// @cpt-cf-file-storage-fr-multipart-upload
pub async fn abort_multipart(
    Extension(ctx): Ctx,
    Extension(svc): MultiSvc,
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
// @cpt-begin:cpt-cf-file-storage-flow-ownership-transfer:p1:inst-transfer-request
pub async fn transfer_ownership(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Path(file_id): Path<Uuid>,
    Json(req): Json<TransferOwnershipReq>,
) -> ApiResult<JsonBody<FileDto>> {
    // @cpt-end:cpt-cf-file-storage-flow-ownership-transfer:p1:inst-transfer-request
    // @cpt-begin:cpt-cf-file-storage-flow-ownership-transfer:p1:inst-transfer-kind-parse
    let new_owner_kind = file_storage_sdk::OwnerKind::parse(&req.new_owner_kind)
        .ok_or_else(|| DomainError::validation("new_owner_kind", "must be 'user' or 'app'"))?;
    // @cpt-end:cpt-cf-file-storage-flow-ownership-transfer:p1:inst-transfer-kind-parse
    // Capture metadata BEFORE the transfer. A transfer does not change custom
    // metadata, but afterwards the caller may no longer have read access under
    // the new owner — re-reading then and defaulting on failure would return a
    // 200 with empty `custom_metadata` for a file that actually has some.
    // @cpt-begin:cpt-cf-file-storage-flow-ownership-transfer:p1:inst-transfer-capture-meta
    let (_, meta) = svc.get_file_with_metadata(&ctx, file_id).await?;
    // @cpt-end:cpt-cf-file-storage-flow-ownership-transfer:p1:inst-transfer-capture-meta
    let file = svc
        .transfer_ownership(&ctx, file_id, new_owner_kind, req.new_owner_id)
        .await?;
    // @cpt-begin:cpt-cf-file-storage-flow-ownership-transfer:p1:inst-transfer-return
    Ok(Json(FileDto::from_parts(file, meta)))
    // @cpt-end:cpt-cf-file-storage-flow-ownership-transfer:p1:inst-transfer-return
}

// ── data-plane finalize (s2s, token-authenticated) ────────────────────────────

/// Request body for the data-plane finalize endpoint.
///
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
/// Token-authenticated: the request must carry the signed upload token in the
/// `x-fs-token` request header. No user JWT is required — the token proves the
/// upload was pre-authorized by the control plane.
///
/// Called by the sidecar immediately after a successful `PUT` to report the
/// measured size + hash and transition the version from `pending` to `available`.
///
/// @cpt-cf-file-storage-fr-audit-trail
pub async fn finalize_version(
    Extension(svc): Svc,
    Extension(verifier): Extension<Arc<Verifier>>,
    Extension(finalize_auth): Extension<Arc<FinalizeAuth>>,
    Path((file_id, version_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    Json(req): Json<FinalizeUploadReq>,
) -> ApiResult<impl IntoResponse> {
    // Extract the token from the x-fs-token header.
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

    // P2 0.1 remaining: interim gear-local shared-secret credential gate —
    // AFTER token verification, additionally require a matching
    // `x-fs-internal-token` header when a secret is configured. `None`
    // (the default) preserves the token-only trust model above.
    finalize_auth.verify(&headers)?;

    // P2 1.8 remediation: log the sidecar-propagated `x-request-id` (echoed
    // from `claims.request_id`, minted at signed-URL issuance) so this
    // control-plane log line can be joined with the sidecar's own log lines
    // for the same upload.
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

    let outcome = svc
        .finalize_upload_by_token(&claims, req.size, hash_value)
        .await?;

    // Upload-flow redesign: surface the auto-bind outcome to the sidecar,
    // which forwards it TRANSPARENTLY to the uploading client on its `PUT`
    // response (fixed contract, the single-part half of the shared
    // bind-state model): `X-FS-Bound: true` + `ETag: <new>` on a won CAS;
    // `X-FS-Bound: conflict` + `X-FS-Current-ETag: <current>` on a lost one.
    // No headers at all when the token requested no bind (manual mode).
    let mut resp = StatusCode::NO_CONTENT.into_response();
    match outcome.bind_state {
        Some(crate::domain::multipart::BindState::Bound) => {
            resp.headers_mut()
                .insert("x-fs-bound", HeaderValue::from_static("true"));
            if let Some(etag) = outcome.etag
                && let Ok(v) = HeaderValue::from_str(&etag)
            {
                resp.headers_mut().insert(header::ETAG, v);
            }
        }
        Some(crate::domain::multipart::BindState::Conflict) => {
            resp.headers_mut()
                .insert("x-fs-bound", HeaderValue::from_static("conflict"));
            if let Some(cur) = outcome.current_etag
                && let Ok(v) = HeaderValue::from_str(&cur)
            {
                resp.headers_mut().insert("x-fs-current-etag", v);
            }
        }
        _ => {}
    }
    Ok(resp)
}

/// Request body for the data-plane report-part endpoint.
///
/// The sidecar posts this after successfully writing a part's bytes to the
/// backend (P2 0.2 group B — the "report part" callback).
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
/// Token-authenticated (mirrors `finalize_version`): the request must carry
/// the signed `multipart_part` upload token in the `x-fs-token` request
/// header. Called by the sidecar immediately after a successful part write to
/// record the part row that `complete_multipart_upload` assembles from.
///
/// @cpt-cf-file-storage-fr-multipart-upload
pub async fn report_multipart_part(
    Extension(msvc): MultiSvc,
    Extension(verifier): Extension<Arc<Verifier>>,
    Extension(finalize_auth): Extension<Arc<FinalizeAuth>>,
    Path((file_id, version_id, upload_id, part_number)): Path<(Uuid, Uuid, Uuid, u32)>,
    headers: HeaderMap,
    Json(req): Json<ReportPartReq>,
) -> ApiResult<impl IntoResponse> {
    // Extract the token from the x-fs-token header.
    let token = headers
        .get("x-fs-token")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(|| DomainError::token_invalid("missing x-fs-token header"))?;

    let claims = verifier
        .verify(&token, OffsetDateTime::now_utc())
        .map_err(|e| DomainError::token_invalid(e.to_string()))?;

    // The token must authorize a report for exactly this
    // (file_id, version_id, upload_id, part_number).
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

    // P2 0.1 remaining: same interim shared-secret gate as `finalize_version`.
    finalize_auth.verify(&headers)?;

    // P2 1.8 remediation: same correlation-id logging as `finalize_version`.
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
    // Bug fix (integrity remediation): `finalize_version` above already
    // rejects a hash that does not decode to exactly 32 bytes; this route
    // must mirror that check — otherwise a wrong-length hash is accepted
    // here with a `204`, gets persisted unchecked by
    // `MultipartService::report_part`, and only surfaces later as an opaque
    // `400` at `complete` (against the wrong actor: whoever happens to call
    // `complete`, not the caller that actually reported the bad hash).
    if hash_value.len() != 32 {
        return Err(DomainError::validation(
            "hash_hex",
            "must decode to exactly 32 bytes (SHA-256)",
        )
        .into());
    }

    msvc.report_part(&claims, req.backend_etag, hash_value, req.size)
        .await?;

    Ok(StatusCode::NO_CONTENT.into_response())
}
