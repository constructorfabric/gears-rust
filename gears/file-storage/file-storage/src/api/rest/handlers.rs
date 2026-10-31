//! Axum handlers for the control-plane REST API. Handlers stay thin: extract,
//! call the service, map to a DTO. All error mapping flows through
//! `From<DomainError> for CanonicalError` (see `error.rs`).

use std::sync::Arc;

use axum::Extension;
use axum::extract::Query;
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::IntoResponse;
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use toolkit::api::canonical_prelude::*;
use toolkit::api::rest::extract::Path;
use toolkit_security::SecurityContext;

use file_storage_sdk::{CustomMetadataPatch, NewFile, OwnerFilter, OwnerKind};

use super::dto::{
    BindModeDto, BindReq, CreateFileReq, CreateRetentionRuleReq, DownloadTicketDto,
    EffectivePolicyDto, FileDto, InitiateMultipartReq, MigrateBackendReq, MissingPartDto,
    MultipartCompleteDto, MultipartCompletingDto, MultipartCompletingStateDto,
    MultipartPartPlanDto, MultipartPlanDto, MultipartStatusDto, PolicyDto, ReceivedPartDto,
    RetentionRuleDto, SetPolicyReq, StorageDto, TransferOwnershipReq, UpdateMetadataReq,
    UploadTicketDto, VersionDto,
};
use super::listing_query::{FilesKeys, ListingQuery, PageKeys};
use crate::domain::create_flow::{self, CreateFileOutcome, MultipartIntent};
use crate::domain::error::DomainError;
use crate::domain::etag;
use crate::domain::multipart::{
    BindState, MultipartCompleteOutcome, MultipartPlan, MultipartUploadStatus,
};
use crate::domain::multipart_service::MultipartService;
use crate::domain::policy::{PolicyScope, RetentionScope};
use crate::domain::policy_service::PolicyService;
use crate::domain::service::FileService;
use crate::infra::signed_url::{Op, Verifier};

type Svc = Extension<Arc<FileService>>;
type MultiSvc = Extension<Arc<MultipartService>>;
type PolicySvc = Extension<Arc<PolicyService>>;
type Ctx = Extension<SecurityContext>;

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

/// Policies for the s2s finalize/report-part callbacks, on top of the signed upload token:
///
/// * A mandatory shared secret (`finalize_internal_secret`, ADR-0003): `verify` always requires
///   a matching `x-fs-internal-token`. Interim until `toolkit-security::internal_auth`.
/// * A grace window on the token's `exp` (`verify_with_grace`): the sidecar checks the token only
///   at upload start, so a slow upload can reach finalize after `exp`. `0` restores strict `exp`.
pub struct FinalizeAuth {
    secret: String,
    token_grace: time::Duration,
}

impl FinalizeAuth {
    #[must_use]
    pub fn new(secret: String, token_grace: time::Duration) -> Self {
        Self {
            secret,
            token_grace,
        }
    }

    /// Grace window on the upload token's `exp` on the callbacks.
    #[must_use]
    pub fn token_grace(&self) -> time::Duration {
        self.token_grace
    }

    /// Verify the `x-fs-internal-token` header; the comparison is constant-time.
    pub fn verify(&self, headers: &HeaderMap) -> Result<(), DomainError> {
        let expected = self.secret.as_str();
        let provided = headers
            .get("x-fs-internal-token")
            .and_then(|v| v.to_str().ok());
        let matches = provided.is_some_and(|provided| {
            toolkit_security::crypto::constant_time_eq(expected.as_bytes(), provided.as_bytes())
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

/// `POST /files`: create a file and presign its first content upload.
///
/// * No `multipart` block (or a one-part plan): a single-part `upload_url`; with `bind: "auto"`
///   (default) the sidecar's finalize binds the first content itself.
/// * `multipart` with at least 2 parts: the response carries the parts plan and no single-part
///   version is pre-registered.
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
    let auto_bind = req.bind == BindModeDto::Auto;
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

    let multipart_intent = req.multipart.as_ref().map(|mp| MultipartIntent {
        declared_size: mp.declared_size,
        preferred_part_size: mp.preferred_part_size,
    });

    let outcome = create_flow::create_file(
        &svc,
        &msvc,
        &ctx,
        new,
        req.idempotency_key,
        auto_bind,
        multipart_intent,
    )
    .await?;

    let dto = match outcome {
        CreateFileOutcome::SinglePart(ticket) => UploadTicketDto {
            file_id: ticket.file_id,
            version_id: ticket.version_id,
            upload_url: Some(ticket.upload_url),
            multipart: None,
        },
        CreateFileOutcome::Multipart { file_id, plan } => UploadTicketDto {
            file_id,
            version_id: plan.version_id,
            upload_url: None,
            multipart: Some(plan_to_dto(plan)),
        },
    };
    let id = dto.file_id.to_string();
    Ok(created_json(dto, &uri, &id).into_response())
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
    listing: ListingQuery<FilesKeys>,
) -> ApiResult<JsonPage<FileDto>> {
    let owner_kind = listing
        .value("owner_kind")
        .ok_or_else(|| DomainError::validation("owner_kind", "is required"))?;
    let owner_kind = OwnerKind::parse(owner_kind)
        .ok_or_else(|| DomainError::validation("owner_kind", "must be 'user' or 'app'"))?;
    let owner_id = listing
        .value("owner_id")
        .ok_or_else(|| DomainError::validation("owner_id", "is required"))?;
    let owner_id = Uuid::parse_str(owner_id)
        .map_err(|_| DomainError::validation("owner_id", "must be a UUID"))?;
    let owner = OwnerFilter {
        owner_kind,
        owner_id,
    };
    let query = listing.query;
    let page = svc
        .list_files(&ctx, owner, query.limit, query.cursor)
        .await?;
    Ok(Json(page.map_items(|f| FileDto::from_parts(f, vec![]))))
}

pub async fn list_versions(
    Extension(ctx): Ctx,
    Extension(svc): Svc,
    Path(file_id): Path<Uuid>,
    ListingQuery { query, .. }: ListingQuery<PageKeys>,
) -> ApiResult<JsonPage<VersionDto>> {
    let page = svc
        .list_versions_with_manifests(&ctx, file_id, query.limit, query.cursor)
        .await?;
    Ok(Json(page.map_items(|(v, manifest)| {
        VersionDto::from_parts(v, manifest)
    })))
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

pub async fn list_storages(
    Extension(svc): Svc,
    ListingQuery { query, .. }: ListingQuery<PageKeys>,
) -> ApiResult<JsonPage<StorageDto>> {
    let page = svc.list_backends(query.limit, query.cursor)?;
    Ok(Json(page.map_items(|(id, caps)| StorageDto::new(id, caps))))
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

/// `GET /retention-rules` — list retention rules for the caller's tenant, cursor-paginated.
pub async fn list_retention_rules(
    Extension(ctx): Ctx,
    Extension(svc): PolicySvc,
    ListingQuery { query, .. }: ListingQuery<PageKeys>,
) -> ApiResult<JsonPage<RetentionRuleDto>> {
    let page = svc
        .list_retention_rules(&ctx, query.limit, query.cursor)
        .await?;
    Ok(Json(page.map_items(RetentionRuleDto::from)))
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
            // Standalone initiate: the client binds explicitly.
            false,
        )
        .await?;
    Ok(Json(plan_to_dto(plan)))
}

/// `POST /files/{id}/multipart/{upload_id}/complete` — finalize all parts.
///
/// Returns the version's id, size, hash and bind outcome (`200`), or `202` + `Retry-After` while
/// another caller holds the lease. `If-Match` is optional: a concrete value is checked against
/// the content `ETag`; `*` or omission is unconditional.
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
    Ok(match outcome {
        MultipartCompleteOutcome::Completed(completed) => Json(MultipartCompleteDto {
            version_id: completed.version_id,
            size: completed.size,
            hash_algorithm: completed.hash_algorithm.to_owned(),
            content_hash: hex::encode(&completed.content_hash),
            hash_mode: completed.hash_mode.as_str().to_owned(),
            part_count: completed.part_count,
            manifest: completed.manifest,
            bind_state: completed.bind_state.into(),
            etag: completed.etag,
            current_etag: completed.current_etag,
        })
        .into_response(),
        // Lease held elsewhere: poll by re-issuing complete.
        MultipartCompleteOutcome::Completing { retry_after_secs } => {
            let mut resp = (
                StatusCode::ACCEPTED,
                Json(MultipartCompletingDto {
                    state: MultipartCompletingStateDto::Completing,
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
        .verify_with_grace(
            &token,
            OffsetDateTime::now_utc(),
            finalize_auth.token_grace(),
        )
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

    let outcome = svc
        .finalize_upload_by_token(&claims, req.size, hash_value)
        .await?;

    // Surface the auto-bind outcome; the sidecar forwards it on its `PUT` response.
    let mut resp = StatusCode::NO_CONTENT.into_response();
    // No bind attempted (a `bind: "manual"` upload) is reported as `manual`.
    let state = outcome.bind_state.unwrap_or(BindState::Manual);
    let (name, value) = match state {
        BindState::Bound => ("etag", outcome.etag),
        BindState::Conflict => ("x-fs-current-etag", outcome.current_etag),
        BindState::Manual => ("etag", None),
    };
    resp.headers_mut()
        .insert("x-fs-bound", HeaderValue::from_static(state.as_str()));
    if let Some(v) = value
        && let Ok(v) = HeaderValue::from_str(&v)
    {
        resp.headers_mut().insert(name, v);
    }
    Ok(resp)
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
        .verify_with_grace(
            &token,
            OffsetDateTime::now_utc(),
            finalize_auth.token_grace(),
        )
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
