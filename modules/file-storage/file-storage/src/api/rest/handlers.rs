//! REST handlers — the unified P1 surface (5 endpoints).

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Extension, Path, Query};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use axum::Json;
use file_storage_sdk::{
    FileMeta, FileMetaUpdate, FileStatus, FileStorageError, ListFilesQuery, PresignDownloadItem,
    UrlParams,
};
use modkit::api::prelude::*;
use modkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::rest::routes::ConcreteService;

use super::dto::{
    BackendDto, FileInfoDto, FileListDto, FileUpdateRequest, ListBackendsResponse,
    ListFilesQueryDto, PresignBatchRequest, PresignBatchResponse, PresignItemDto,
    PresignOutcomeDto, PresignedUploadHandleDto,
};
use super::error::file_storage_error_to_problem;

// ── Helpers ─────────────────────────────────────────────────────────────────

fn etag_from_if_match(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim_matches('"').to_owned())
}

fn require_if_match(headers: &HeaderMap) -> Result<String, Problem> {
    etag_from_if_match(headers).ok_or_else(|| {
        file_storage_error_to_problem(
            &FileStorageError::BadRequest("If-Match header is required".to_owned()),
            "/",
        )
    })
}

fn etag_from_if_none_match(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim_matches('"').to_owned())
}

fn map_err(e: FileStorageError) -> Problem {
    file_storage_error_to_problem(&e, "/")
}

fn header_etag_value(etag: &str) -> HeaderValue {
    HeaderValue::from_str(&format!(r#""{etag}""#)).unwrap_or(HeaderValue::from_static("\"\""))
}

// ── GET /storages ───────────────────────────────────────────────────────────

pub async fn list_backends(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
) -> ApiResult<JsonBody<ListBackendsResponse>> {
    let backends = svc
        .list_backends(&ctx)
        .await
        .map_err(|e| file_storage_error_to_problem(&e.into(), "/storages"))?;
    let items: Vec<BackendDto> = backends.into_iter().map(Into::into).collect();
    Ok(Json(ListBackendsResponse { items }))
}

// ── GET /files/{file_id} ────────────────────────────────────────────────────

pub async fn get_file(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path(file_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<axum::response::Response> {
    let if_none_match = etag_from_if_none_match(&headers);
    let info = svc
        .get_file_info(&ctx, file_id, None)
        .await
        .map_err(|e| map_err(e.into()))?;

    if let Some(etag) = if_none_match
        && etag == info.etag
    {
        let mut h = HeaderMap::new();
        h.insert(header::ETAG, header_etag_value(&info.etag));
        return Ok((StatusCode::NOT_MODIFIED, h, Body::empty()).into_response());
    }

    let etag = info.etag.clone();
    let dto: FileInfoDto = info.into();
    let mut h = HeaderMap::new();
    h.insert(header::ETAG, header_etag_value(&etag));
    Ok((StatusCode::OK, h, Json(dto)).into_response())
}

// ── PUT /files/{file_id} (unified status / metadata branch) ─────────────────

pub async fn update_file(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path(file_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<FileUpdateRequest>,
) -> ApiResult<axum::response::Response> {
    let etag = require_if_match(&headers)?;

    let has_status = body.has_status_branch();
    let has_meta = body.has_metadata_branch();

    if has_status && has_meta {
        return Err(file_storage_error_to_problem(
            &FileStorageError::BadRequest(
                "body must not mix status-commit and metadata-replace fields".to_owned(),
            ),
            "/",
        ));
    }
    if !has_status && !has_meta {
        return Err(file_storage_error_to_problem(
            &FileStorageError::BadRequest(
                "body must set either status+new_etag or at least one metadata field".to_owned(),
            ),
            "/",
        ));
    }

    if has_status {
        // Status-commit branch: target must be Uploaded, new_etag mandatory.
        let status_dto = body.status.ok_or_else(|| {
            file_storage_error_to_problem(
                &FileStorageError::BadRequest("status is required".to_owned()),
                "/",
            )
        })?;
        let new_etag = body.new_etag.ok_or_else(|| {
            file_storage_error_to_problem(
                &FileStorageError::BadRequest("new_etag is required when status is set".to_owned()),
                "/",
            )
        })?;
        let target: FileStatus = status_dto.into();
        let info = svc
            .change_status(&ctx, file_id, target, etag, new_etag)
            .await
            .map_err(|e| map_err(e.into()))?;
        let etag_out = info.etag.clone();
        let dto: FileInfoDto = info.into();
        let mut h = HeaderMap::new();
        h.insert(header::ETAG, header_etag_value(&etag_out));
        Ok((StatusCode::OK, h, Json(dto)).into_response())
    } else {
        // Metadata-replace branch.
        let update: FileMetaUpdate = body.into_metadata_update();
        let info = svc
            .put_file_info(&ctx, file_id, update, etag)
            .await
            .map_err(|e| map_err(e.into()))?;
        let etag_out = info.etag.clone();
        let dto: FileInfoDto = info.into();
        let mut h = HeaderMap::new();
        h.insert(header::ETAG, header_etag_value(&etag_out));
        Ok((StatusCode::OK, h, Json(dto)).into_response())
    }
}

// ── DELETE /files/{file_id} ─────────────────────────────────────────────────

pub async fn delete_file(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path(file_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let etag = require_if_match(&headers)?;
    svc.delete_file(&ctx, file_id, etag)
        .await
        .map_err(|e| map_err(e.into()))?;
    Ok(StatusCode::NO_CONTENT)
}

// ── GET /files ──────────────────────────────────────────────────────────────

pub async fn list_files(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Query(q): Query<ListFilesQueryDto>,
) -> ApiResult<JsonBody<FileListDto>> {
    let query = ListFilesQuery {
        owner_id: q.owner_id,
        backend_id: q.backend_id,
        mime_type: q.mime_type,
        gts_file_type: q.gts_file_type,
        created_after: q.created_after,
        created_before: q.created_before,
        cursor: q.cursor,
        limit: q.limit,
    };
    let list = svc
        .list_files(&ctx, query)
        .await
        .map_err(|e| map_err(e.into()))?;
    let dto = FileListDto {
        items: list.items.into_iter().map(Into::into).collect(),
        next_cursor: list.next_cursor,
    };
    Ok(Json(dto))
}

// ── POST /presign-batch (unified upload + download) ─────────────────────────

pub async fn presign_batch(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Json(req): Json<PresignBatchRequest>,
) -> ApiResult<JsonBody<PresignBatchResponse>> {
    let mut outcomes: Vec<PresignOutcomeDto> = Vec::with_capacity(req.items.len());

    for item in req.items {
        match item {
            PresignItemDto::Upload(u) => {
                let owner = u.owner.into();
                let meta: FileMeta = u.meta.into();
                let params: UrlParams = u.params.into();
                let res = svc
                    .create_presigned_url(&ctx, u.backend_id, owner, &u.file_path, meta, params)
                    .await;
                match res {
                    Ok(handle) => outcomes.push(PresignOutcomeDto::upload_ok(handle.into())),
                    Err(e) => {
                        let public: FileStorageError = e.into();
                        outcomes.push(PresignOutcomeDto::upload_err(public.to_string()));
                    }
                }
            }
            PresignItemDto::Download(d) => {
                let item = PresignDownloadItem {
                    file_id: d.file_id,
                    params: d.params.into(),
                    etag: d.etag,
                };
                // Single-item batch through the service; reuse the
                // per-item outcome shape.
                let result = svc.presign_urls(&ctx, vec![item]).await;
                match result {
                    Ok(mut v) => {
                        if let Some(o) = v.pop() {
                            outcomes.push(o.into());
                        } else {
                            outcomes.push(PresignOutcomeDto::download_err(
                                "internal: empty outcome".to_owned(),
                            ));
                        }
                    }
                    Err(e) => {
                        let public: FileStorageError = e.into();
                        outcomes.push(PresignOutcomeDto::download_err(public.to_string()));
                    }
                }
            }
        }
    }

    Ok(Json(PresignBatchResponse { items: outcomes }))
}

// `Json` is already imported via `axum::Json`.
#[allow(dead_code)]
fn _unused_uploads_marker() -> PresignedUploadHandleDto {
    PresignedUploadHandleDto {
        file_id: Uuid::nil(),
        upload_url: String::new(),
        etag_pinned: String::new(),
        expires_at: time::OffsetDateTime::UNIX_EPOCH,
    }
}
