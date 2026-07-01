//! Control-plane route registration via `OperationBuilder`
//! (`cpt-cf-file-storage-fr-rest-api`). This surface is JSON-only and never
//! carries file content — bytes move over signed URLs against the sidecar.

use std::sync::Arc;

use axum::Router;
use http::StatusCode;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::{LicenseFeature, OperationBuilder};

use super::dto;
use super::handlers;
use crate::domain::multipart_service::MultipartService;
use crate::domain::policy_service::PolicyService;
use crate::domain::service::FileService;

const API_TAG: &str = "File Storage";
const BASE: &str = "/api/file-storage/v1";

pub(crate) struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        "gts.cf.core.lic.feat.v1~cf.core.global.base.v1"
    }
}

impl LicenseFeature for License {}

/// Register all file-storage control-plane routes and attach the services.
#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
pub(crate) fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<FileService>,
    multipart_service: Arc<MultipartService>,
    policy_service: Arc<PolicyService>,
) -> Router {
    // POST /files — create + presign upload
    router = OperationBuilder::post(format!("{BASE}/files"))
        .operation_id("file_storage.create_file")
        .authenticated()
        .require_license_features::<License>([])
        .summary("Create a file and presign its first content upload")
        .tag(API_TAG)
        .json_request::<dto::CreateFileReq>(openapi, "File metadata")
        .handler(handlers::create_file)
        .json_response_with_schema::<dto::UploadTicketDto>(openapi, StatusCode::CREATED, "Created")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /files/{id}/versions — presign a new version
    router = OperationBuilder::post(format!("{BASE}/files/{{id}}/versions"))
        .operation_id("file_storage.presign_version")
        .authenticated()
        .require_license_features::<License>([])
        .summary("Presign a new content version upload")
        .tag(API_TAG)
        .path_param("id", "File UUID")
        .handler(handlers::presign_version)
        .json_response_with_schema::<dto::UploadTicketDto>(openapi, StatusCode::OK, "Presigned")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /files/{id}/bind — bind/rebind content pointer (If-Match)
    router = OperationBuilder::post(format!("{BASE}/files/{{id}}/bind"))
        .operation_id("file_storage.bind")
        .authenticated()
        .require_license_features::<License>([])
        .summary("Bind/rebind the content pointer under optimistic CAS")
        .description("If-Match carries the current content ETag; 412 on conflict.")
        .tag(API_TAG)
        .path_param("id", "File UUID")
        .json_request::<dto::BindReq>(openapi, "Version to bind")
        .handler(handlers::bind)
        .json_response_with_schema::<dto::FileDto>(openapi, StatusCode::OK, "Bound")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        // 409: the target version's upload is not finalized yet.
        .error_409(openapi)
        // 412: the If-Match / CAS precondition failed (or is required and absent).
        .problem_response(
            openapi,
            StatusCode::PRECONDITION_FAILED,
            "Precondition Failed",
        )
        .error_500(openapi)
        .register(router, openapi);

    // GET /files/{id}/download-url — issue a signed download URL
    router = OperationBuilder::get(format!("{BASE}/files/{{id}}/download-url"))
        .operation_id("file_storage.download_url")
        .authenticated()
        .require_license_features::<License>([])
        .summary("Issue a signed download URL (pins current content or ?version_id)")
        .tag(API_TAG)
        .path_param("id", "File UUID")
        .query_param("version_id", false, "Pin a specific version")
        .handler(handlers::download_url)
        .json_response_with_schema::<dto::DownloadTicketDto>(openapi, StatusCode::OK, "Signed URL")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /files/{id}/versions — list versions
    router = OperationBuilder::get(format!("{BASE}/files/{{id}}/versions"))
        .operation_id("file_storage.list_versions")
        .authenticated()
        .require_license_features::<License>([])
        .summary("List all versions of a file")
        .tag(API_TAG)
        .path_param("id", "File UUID")
        .handler(handlers::list_versions)
        .json_response_with_schema::<Vec<dto::VersionDto>>(openapi, StatusCode::OK, "Versions")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // DELETE /files/{id}/versions/{version_id} — delete a version
    router = OperationBuilder::delete(format!("{BASE}/files/{{id}}/versions/{{version_id}}"))
        .operation_id("file_storage.delete_version")
        .authenticated()
        .require_license_features::<License>([])
        .summary("Delete a single version")
        .tag(API_TAG)
        .path_param("id", "File UUID")
        .path_param("version_id", "Version UUID")
        .handler(handlers::delete_version)
        .json_response(StatusCode::NO_CONTENT, "Deleted")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /files/{id} — metadata (conditional)
    router = OperationBuilder::get(format!("{BASE}/files/{{id}}"))
        .operation_id("file_storage.get_file")
        .authenticated()
        .require_license_features::<License>([])
        .summary("Get file metadata (supports If-None-Match -> 304)")
        .tag(API_TAG)
        .path_param("id", "File UUID")
        .handler(handlers::get_file)
        .json_response_with_schema::<dto::FileDto>(openapi, StatusCode::OK, "File metadata")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // PATCH /files/{id} — update custom metadata (If-Match-Metadata)
    router = OperationBuilder::patch(format!("{BASE}/files/{{id}}"))
        .operation_id("file_storage.update_metadata")
        .authenticated()
        .require_license_features::<License>([])
        .summary("Update custom metadata (JSON merge patch)")
        .tag(API_TAG)
        .path_param("id", "File UUID")
        .json_request::<dto::UpdateMetadataReq>(openapi, "Metadata merge patch")
        .handler(handlers::update_metadata)
        .json_response_with_schema::<dto::FileDto>(openapi, StatusCode::OK, "Updated")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // DELETE /files/{id} — delete file + all versions (If-Match required)
    router = OperationBuilder::delete(format!("{BASE}/files/{{id}}"))
        .operation_id("file_storage.delete_file")
        .authenticated()
        .require_license_features::<License>([])
        .summary("Delete a file and all its versions")
        .description(
            "If-Match (content ETag or \"*\") is required; 412 on mismatch or when absent.",
        )
        .tag(API_TAG)
        .path_param("id", "File UUID")
        .handler(handlers::delete_file)
        .json_response(StatusCode::NO_CONTENT, "Deleted")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        // 412: If-Match absent or does not match the current content ETag.
        .problem_response(
            openapi,
            StatusCode::PRECONDITION_FAILED,
            "Precondition Failed",
        )
        .error_500(openapi)
        .register(router, openapi);

    // GET /files — list (mandatory owner filter, offset pagination)
    router = OperationBuilder::get(format!("{BASE}/files"))
        .operation_id("file_storage.list_files")
        .authenticated()
        .require_license_features::<License>([])
        .summary("List files for an owner (owner_kind + owner_id required)")
        .tag(API_TAG)
        .query_param("owner_kind", true, "'user' or 'app'")
        .query_param("owner_id", true, "Owner UUID")
        .query_param_typed("limit", false, "Page size", "integer")
        .query_param_typed("offset", false, "Offset", "integer")
        .handler(handlers::list_files)
        .json_response_with_schema::<Vec<dto::FileDto>>(openapi, StatusCode::OK, "Files")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /storages — backend discovery
    router = OperationBuilder::get(format!("{BASE}/storages"))
        .operation_id("file_storage.list_storages")
        .authenticated()
        .require_license_features::<License>([])
        .summary("List configured storage backends and capabilities")
        .tag(API_TAG)
        .handler(handlers::list_storages)
        .json_response_with_schema::<Vec<dto::StorageDto>>(openapi, StatusCode::OK, "Backends")
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /storages/{id} — one backend
    router = OperationBuilder::get(format!("{BASE}/storages/{{id}}"))
        .operation_id("file_storage.get_storage")
        .authenticated()
        .require_license_features::<License>([])
        .summary("Get one storage backend")
        .tag(API_TAG)
        .path_param("id", "Backend id")
        .handler(handlers::get_storage)
        .json_response_with_schema::<dto::StorageDto>(openapi, StatusCode::OK, "Backend")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // ── Policy endpoints (P2-M1) — cpt-cf-file-storage-usecase-configure-policy

    // GET /policy — fetch own policy for a scope
    router = OperationBuilder::get(format!("{BASE}/policy"))
        .operation_id("file_storage.get_policy")
        .authenticated()
        .require_license_features::<License>([])
        .summary("Get the stored policy for a scope (tenant or user)")
        .tag(API_TAG)
        .query_param("scope", true, "'tenant' or 'user'")
        .query_param(
            "scope_owner_id",
            false,
            "User UUID (required for user scope)",
        )
        .handler(handlers::get_policy)
        .json_response_with_schema::<dto::PolicyDto>(openapi, StatusCode::OK, "Policy")
        .json_response(
            StatusCode::NO_CONTENT,
            "No policy configured for this scope",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // PUT /policy — upsert policy for a scope
    router = OperationBuilder::put(format!("{BASE}/policy"))
        .operation_id("file_storage.set_policy")
        .authenticated()
        .require_license_features::<License>([])
        .summary("Set (upsert) the policy for a scope")
        .tag(API_TAG)
        .json_request::<dto::SetPolicyReq>(openapi, "Policy configuration")
        .handler(handlers::set_policy)
        .json_response_with_schema::<dto::PolicyDto>(openapi, StatusCode::OK, "Stored policy")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /policy/effective — compute effective policy
    router = OperationBuilder::get(format!("{BASE}/policy/effective"))
        .operation_id("file_storage.get_effective_policy")
        .authenticated()
        .require_license_features::<License>([])
        .summary("Get effective policy (most-restrictive across tenant + user levels)")
        .tag(API_TAG)
        .query_param(
            "user_owner_id",
            false,
            "User UUID to include in effective resolution",
        )
        .handler(handlers::get_effective_policy)
        .json_response_with_schema::<dto::EffectivePolicyDto>(
            openapi,
            StatusCode::OK,
            "Effective policy",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /retention-rules — list retention rules
    router = OperationBuilder::get(format!("{BASE}/retention-rules"))
        .operation_id("file_storage.list_retention_rules")
        .authenticated()
        .require_license_features::<License>([])
        .summary("List all retention rules for the caller's tenant")
        .tag(API_TAG)
        .handler(handlers::list_retention_rules)
        .json_response_with_schema::<Vec<dto::RetentionRuleDto>>(
            openapi,
            StatusCode::OK,
            "Retention rules",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /retention-rules — create a retention rule
    router = OperationBuilder::post(format!("{BASE}/retention-rules"))
        .operation_id("file_storage.create_retention_rule")
        .authenticated()
        .require_license_features::<License>([])
        .summary("Create a retention rule")
        .tag(API_TAG)
        .json_request::<dto::CreateRetentionRuleReq>(openapi, "Retention rule")
        .handler(handlers::create_retention_rule)
        .json_response_with_schema::<dto::RetentionRuleDto>(
            openapi,
            StatusCode::CREATED,
            "Created retention rule",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // DELETE /retention-rules/{rule_id} — delete a retention rule
    router = OperationBuilder::delete(format!("{BASE}/retention-rules/{{rule_id}}"))
        .operation_id("file_storage.delete_retention_rule")
        .authenticated()
        .require_license_features::<License>([])
        .summary("Delete a retention rule")
        .tag(API_TAG)
        .path_param("rule_id", "Retention rule UUID")
        .handler(handlers::delete_retention_rule)
        .json_response(StatusCode::NO_CONTENT, "Deleted")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // ── Multipart upload (P2-M3) ─────────────────────────────────────────────────

    // POST /files/{id}/multipart — initiate multipart session (server-authoritative plan)
    router = OperationBuilder::post(format!("{BASE}/files/{{id}}/multipart"))
        .operation_id("file_storage.initiate_multipart")
        .authenticated()
        .require_license_features::<License>([])
        .summary("Initiate a server-authoritative multipart upload and get the parts plan")
        .description(
            "Returns the exact parts plan (sizes, offsets) plus one signed sidecar URL per \
             part. The client PUTs bytes directly to the sidecar; the control plane never \
             carries content bytes (ADR-0003, DESIGN \u{a7}4.6).",
        )
        .tag(API_TAG)
        .path_param("id", "File UUID")
        .json_request::<dto::InitiateMultipartReq>(openapi, "Multipart initiation")
        .handler(handlers::initiate_multipart)
        .json_response_with_schema::<dto::MultipartPlanDto>(
            openapi,
            StatusCode::OK,
            "Server-authoritative parts plan",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_422(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // NOTE: The control-plane PUT .../parts/{part_number} byte route is intentionally
    // absent — bytes flow exclusively to the sidecar via the per-part signed URLs
    // returned by the initiate response (ADR-0003 "no bytes through the control
    // plane"; multipart-coordinator FEATURE §8 migration).

    // POST /files/{id}/multipart/{upload_id}/complete — finalize
    router = OperationBuilder::post(format!(
        "{BASE}/files/{{id}}/multipart/{{upload_id}}/complete"
    ))
    .operation_id("file_storage.complete_multipart")
    .authenticated()
    .require_license_features::<License>([])
    .summary("Finalize a multipart upload (assemble all parts)")
    .tag(API_TAG)
    .path_param("id", "File UUID")
    .path_param("upload_id", "Upload session UUID")
    .handler(handlers::complete_multipart)
    .json_response(StatusCode::NO_CONTENT, "Completed")
    .error_401(openapi)
    .error_403(openapi)
    .error_404(openapi)
    .error_409(openapi)
    .error_500(openapi)
    .register(router, openapi);

    // DELETE /files/{id}/multipart/{upload_id} — abort
    router = OperationBuilder::delete(format!("{BASE}/files/{{id}}/multipart/{{upload_id}}"))
        .operation_id("file_storage.abort_multipart")
        .authenticated()
        .require_license_features::<License>([])
        .summary("Abort a multipart upload and discard all parts")
        .tag(API_TAG)
        .path_param("id", "File UUID")
        .path_param("upload_id", "Upload session UUID")
        .handler(handlers::abort_multipart)
        .json_response(StatusCode::NO_CONTENT, "Aborted")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // ── Backend migration (P2-M4) ─────────────────────────────────────────────

    // POST /files/{id}/migrate — backend migration
    router = OperationBuilder::post(format!("{BASE}/files/{{id}}/migrate"))
        .operation_id("file_storage.migrate_backend")
        .authenticated()
        .require_license_features::<License>([])
        .summary("Migrate file content to a different storage backend")
        .description(
            "Non-versioned files only. Preserves identity and verifies content hash before \
             committing the new backend binding.",
        )
        .tag(API_TAG)
        .path_param("id", "File UUID")
        .json_request::<dto::MigrateBackendReq>(openapi, "Migration request")
        .handler(handlers::migrate_backend)
        .json_response(StatusCode::NO_CONTENT, "Migrated")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // ── Ownership transfer (P2-M5) ─────────────────────────────────────────────

    // POST /files/{id}/transfer — transfer ownership
    router = OperationBuilder::post(format!("{BASE}/files/{{id}}/transfer"))
        .operation_id("file_storage.transfer_ownership")
        .authenticated()
        .require_license_features::<License>([])
        .summary("Transfer ownership of a file to a new owner")
        .description(
            "Atomically replaces owner_kind + owner_id and records an audit row. \
             A file.owner_transferred event is enqueued in the same transaction.",
        )
        .tag(API_TAG)
        .path_param("id", "File UUID")
        .json_request::<dto::TransferOwnershipReq>(openapi, "Transfer ownership request")
        .handler(handlers::transfer_ownership)
        .json_response_with_schema::<dto::FileDto>(openapi, StatusCode::OK, "Updated file")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
        .layer(axum::Extension(policy_service))
        .layer(axum::Extension(multipart_service))
        .layer(axum::Extension(service))
}
