//! Axum router wiring for the FileStorage REST surface.
//!
//! 5 P1 endpoints unified under `/api/file-storage/v1/`.

use std::sync::Arc;

use axum::http::StatusCode;
use axum::{Extension, Router};
use modkit::api::operation_builder::LicenseFeature;
use modkit::api::{OpenApiRegistry, OperationBuilder};

use crate::domain::service::Service;
use crate::infra::storage::sea_orm_repo::SeaOrmFilesRepository;

use super::dto::{
    FileInfoDto, FileListDto, FileUpdateRequest, ListBackendsResponse, PresignBatchRequest,
    PresignBatchResponse,
};
use super::handlers;

/// Concrete service type used by the REST handlers.
pub type ConcreteService = Service<SeaOrmFilesRepository>;

struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        "gts.x.core.lic.feat.v1~x.core.global.base.v1"
    }
}

impl LicenseFeature for License {}

pub fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<ConcreteService>,
) -> Router {
    // GET /storages
    router = OperationBuilder::get("/file-storage/v1/storages")
        .operation_id("file_storage.list_backends")
        .summary("List backends visible to the caller's tenant")
        .description("Returns the roster filtered by per-tenant access list.")
        .tag("Backends")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_backends)
        .json_response_with_schema::<ListBackendsResponse>(
            openapi,
            StatusCode::OK,
            "Backend roster",
        )
        .error_401(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /files
    router = OperationBuilder::get("/file-storage/v1/files")
        .operation_id("file_storage.list_files")
        .summary("Paginated owner-scoped file listing")
        .description("List files visible to the caller's tenant.")
        .tag("Files")
        .authenticated()
        .require_license_features::<License>([])
        .query_param("owner_id", false, "Owner principal UUID")
        .query_param("backend_id", false, "Backend UUID")
        .query_param("mime_type", false, "MIME filter")
        .query_param("gts_file_type", false, "GTS file type filter")
        .query_param("created_after", false, "RFC3339 timestamp lower bound")
        .query_param("created_before", false, "RFC3339 timestamp upper bound")
        .query_param("cursor", false, "Opaque pagination cursor")
        .query_param("limit", false, "Page size (max 200)")
        .handler(handlers::list_files)
        .json_response_with_schema::<FileListDto>(openapi, StatusCode::OK, "Paginated file list")
        .error_400(openapi)
        .error_401(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /files/{file_id}
    router = OperationBuilder::get("/file-storage/v1/files/{file_id}")
        .operation_id("file_storage.get_file")
        .summary("Get authoritative file metadata")
        .description("Returns the FileStorage SQL row as the authoritative metadata view.")
        .tag("Files")
        .authenticated()
        .require_license_features::<License>([])
        .path_param("file_id", "File identifier (UUID)")
        .handler(handlers::get_file)
        .json_response_with_schema::<FileInfoDto>(openapi, StatusCode::OK, "File metadata")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // PUT /files/{file_id}
    router = OperationBuilder::put("/file-storage/v1/files/{file_id}")
        .operation_id("file_storage.update_file")
        .summary("Commit upload OR replace mutable metadata")
        .description("Single mutation entry-point. EITHER status-commit OR metadata-replace.")
        .tag("Files")
        .authenticated()
        .require_license_features::<License>([])
        .path_param("file_id", "File identifier (UUID)")
        .json_request::<FileUpdateRequest>(openapi, "File update body")
        .handler(handlers::update_file)
        .json_response_with_schema::<FileInfoDto>(openapi, StatusCode::OK, "File updated")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // DELETE /files/{file_id}
    router = OperationBuilder::delete("/file-storage/v1/files/{file_id}")
        .operation_id("file_storage.delete_file")
        .summary("Hard-delete a file")
        .description("Removes the metadata row and queues the backend object for orphan-delete.")
        .tag("Files")
        .authenticated()
        .require_license_features::<License>([])
        .path_param("file_id", "File identifier (UUID)")
        .handler(handlers::delete_file)
        .json_response(StatusCode::NO_CONTENT, "File deleted")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /presign-batch
    router = OperationBuilder::post("/file-storage/v1/presign-batch")
        .operation_id("file_storage.presign_batch")
        .summary("Batch-issue presigned upload and/or download URLs")
        .description("Unified upload + download presign batch.")
        .tag("Presign")
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<PresignBatchRequest>(openapi, "Presign batch request")
        .handler(handlers::presign_batch)
        .json_response_with_schema::<PresignBatchResponse>(
            openapi,
            StatusCode::OK,
            "Per-item outcomes",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = router.layer(Extension(service));
    router
}
