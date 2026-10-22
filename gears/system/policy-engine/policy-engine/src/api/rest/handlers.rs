//! One async handler per operation of the Policy Administration REST API.
//! Every handler is a thin projection over [`PolicyManagementClientV1`]: it
//! authenticates the caller, calls exactly one trait method, and maps the
//! result onto [`dto`](super::dto) types. No domain logic lives here.

use std::sync::Arc;

use axum::extract::Extension;
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use policy_engine_sdk::management::{AssignmentId, BundleId, PolicyManagementClientV1, VersionId};
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::dto::{
    PolicyEngineAssignmentDto, PolicyEngineBundleDto, PolicyEngineCreateAssignmentRequestDto,
    PolicyEngineCreateBundleRequestDto, PolicyEngineCreateDraftVersionRequestDto,
    PolicyEngineReplaceDraftContentRequestDto, PolicyEngineUpdateAssignmentRequestDto,
    PolicyEngineUpdateBundleRequestDto, PolicyEngineValidationReportDto,
    PolicyEngineVersionDetailDto, PolicyEngineVersionDto,
};
use super::error::{self, ApiResult, OptionalJson};

/// Concrete client alias for handler signatures.
type Client = Arc<dyn PolicyManagementClientV1>;

// ---------------------------------------------------------------------------
// Bundles
// ---------------------------------------------------------------------------

/// `POST /policy-engine/v1/bundles`.
#[tracing::instrument(skip_all)]
pub async fn create_bundle(
    uri: Uri,
    ctx: Option<Extension<SecurityContext>>,
    Extension(client): Extension<Client>,
    extract::Json(body): extract::Json<PolicyEngineCreateBundleRequestDto>,
) -> ApiResult<Response> {
    let ctx = error::require_context(ctx)?;
    let bundle = client.create_bundle(&ctx, body.into()).await?;
    let id = bundle.id.to_string();
    Ok(created_json(PolicyEngineBundleDto::from(bundle), &uri, &id).into_response())
}

/// `GET /policy-engine/v1/bundles/{id}`.
#[tracing::instrument(skip_all)]
pub async fn get_bundle(
    ctx: Option<Extension<SecurityContext>>,
    Extension(client): Extension<Client>,
    extract::Path(bundle_id): extract::Path<Uuid>,
) -> ApiResult<Json<PolicyEngineBundleDto>> {
    let ctx = error::require_context(ctx)?;
    let bundle_id = BundleId(bundle_id);
    Ok(Json(client.get_bundle(&ctx, bundle_id).await?.into()))
}

/// `GET /policy-engine/v1/bundles`.
#[tracing::instrument(skip_all)]
pub async fn list_bundles(
    ctx: Option<Extension<SecurityContext>>,
    Extension(client): Extension<Client>,
    OData(query): OData,
) -> ApiResult<Json<toolkit_odata::Page<PolicyEngineBundleDto>>> {
    let ctx = error::require_context(ctx)?;
    let page = client.list_bundles(&ctx, &query).await?;
    Ok(Json(page.map_items(PolicyEngineBundleDto::from)))
}

/// `PATCH /policy-engine/v1/bundles/{id}`.
#[tracing::instrument(skip_all)]
pub async fn update_bundle(
    ctx: Option<Extension<SecurityContext>>,
    Extension(client): Extension<Client>,
    extract::Path(bundle_id): extract::Path<Uuid>,
    extract::Json(body): extract::Json<PolicyEngineUpdateBundleRequestDto>,
) -> ApiResult<Json<PolicyEngineBundleDto>> {
    let ctx = error::require_context(ctx)?;
    let bundle_id = BundleId(bundle_id);
    let bundle = client.update_bundle(&ctx, bundle_id, body.into()).await?;
    Ok(Json(bundle.into()))
}

// ---------------------------------------------------------------------------
// Versions
// ---------------------------------------------------------------------------

/// `POST /policy-engine/v1/bundles/{id}/versions`.
#[tracing::instrument(skip_all)]
pub async fn create_draft_version(
    uri: Uri,
    ctx: Option<Extension<SecurityContext>>,
    Extension(client): Extension<Client>,
    extract::Path(bundle_id): extract::Path<Uuid>,
    OptionalJson(body): OptionalJson<PolicyEngineCreateDraftVersionRequestDto>,
) -> ApiResult<Response> {
    let ctx = error::require_context(ctx)?;
    // No body is an empty draft, like a body without `seed_from`.
    let body = body.unwrap_or_default();
    let bundle_id = BundleId(bundle_id);
    let version = client
        .create_draft_version(&ctx, bundle_id, body.seed_from.map(VersionId))
        .await?;
    let id = version.id.to_string();
    Ok(created_json(PolicyEngineVersionDto::from(version), &uri, &id).into_response())
}

/// `GET /policy-engine/v1/bundles/{id}/versions/{version}`.
#[tracing::instrument(skip_all)]
pub async fn get_version(
    ctx: Option<Extension<SecurityContext>>,
    Extension(client): Extension<Client>,
    extract::Path((bundle_id, version_id)): extract::Path<(Uuid, Uuid)>,
) -> ApiResult<Json<PolicyEngineVersionDetailDto>> {
    let ctx = error::require_context(ctx)?;
    let bundle_id = BundleId(bundle_id);
    let version_id = VersionId(version_id);
    let detail = client.get_version(&ctx, bundle_id, version_id).await?;
    Ok(Json(detail.into()))
}

/// `GET /policy-engine/v1/bundles/{id}/versions`.
#[tracing::instrument(skip_all)]
pub async fn list_versions(
    ctx: Option<Extension<SecurityContext>>,
    Extension(client): Extension<Client>,
    extract::Path(bundle_id): extract::Path<Uuid>,
    OData(query): OData,
) -> ApiResult<Json<toolkit_odata::Page<PolicyEngineVersionDto>>> {
    let ctx = error::require_context(ctx)?;
    let bundle_id = BundleId(bundle_id);
    let page = client.list_versions(&ctx, bundle_id, &query).await?;
    Ok(Json(page.map_items(PolicyEngineVersionDto::from)))
}

/// `PUT /policy-engine/v1/bundles/{id}/versions/{version}`. **Carries
/// content**, both ways.
#[tracing::instrument(skip_all)]
pub async fn replace_draft_content(
    ctx: Option<Extension<SecurityContext>>,
    Extension(client): Extension<Client>,
    extract::Path((bundle_id, version_id)): extract::Path<(Uuid, Uuid)>,
    extract::Json(body): extract::Json<PolicyEngineReplaceDraftContentRequestDto>,
) -> ApiResult<Json<PolicyEngineVersionDetailDto>> {
    let ctx = error::require_context(ctx)?;
    let bundle_id = BundleId(bundle_id);
    let version_id = VersionId(version_id);
    let detail = client
        .replace_draft_content(&ctx, bundle_id, version_id, body.into())
        .await?;
    Ok(Json(detail.into()))
}

/// `POST /policy-engine/v1/bundles/{id}/versions/{version}/validate`.
#[tracing::instrument(skip_all)]
pub async fn validate_version(
    ctx: Option<Extension<SecurityContext>>,
    Extension(client): Extension<Client>,
    extract::Path((bundle_id, version_id)): extract::Path<(Uuid, Uuid)>,
) -> ApiResult<Json<PolicyEngineValidationReportDto>> {
    let ctx = error::require_context(ctx)?;
    let bundle_id = BundleId(bundle_id);
    let version_id = VersionId(version_id);
    let report = client.validate_version(&ctx, bundle_id, version_id).await?;
    Ok(Json(report.into()))
}

/// `POST /policy-engine/v1/bundles/{id}/versions/{version}/activate`.
#[tracing::instrument(skip_all)]
pub async fn activate_version(
    ctx: Option<Extension<SecurityContext>>,
    Extension(client): Extension<Client>,
    extract::Path((bundle_id, version_id)): extract::Path<(Uuid, Uuid)>,
) -> ApiResult<Json<PolicyEngineVersionDto>> {
    let ctx = error::require_context(ctx)?;
    let bundle_id = BundleId(bundle_id);
    let version_id = VersionId(version_id);
    let version = client.activate_version(&ctx, bundle_id, version_id).await?;
    Ok(Json(version.into()))
}

/// `DELETE /policy-engine/v1/bundles/{id}/versions/{version}`: the only
/// deletion the lifecycle permits.
#[tracing::instrument(skip_all)]
pub async fn delete_draft_version(
    ctx: Option<Extension<SecurityContext>>,
    Extension(client): Extension<Client>,
    extract::Path((bundle_id, version_id)): extract::Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let ctx = error::require_context(ctx)?;
    let bundle_id = BundleId(bundle_id);
    let version_id = VersionId(version_id);
    client
        .delete_draft_version(&ctx, bundle_id, version_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Assignments
// ---------------------------------------------------------------------------

/// `POST /policy-engine/v1/assignments`.
#[tracing::instrument(skip_all)]
pub async fn assign(
    uri: Uri,
    ctx: Option<Extension<SecurityContext>>,
    Extension(client): Extension<Client>,
    extract::Json(body): extract::Json<PolicyEngineCreateAssignmentRequestDto>,
) -> ApiResult<Response> {
    let ctx = error::require_context(ctx)?;
    let assignment = client.assign(&ctx, body.into()).await?;
    let id = assignment.id.to_string();
    Ok(created_json(PolicyEngineAssignmentDto::from(assignment), &uri, &id).into_response())
}

/// `GET /policy-engine/v1/assignments/{id}`.
#[tracing::instrument(skip_all)]
pub async fn get_assignment(
    ctx: Option<Extension<SecurityContext>>,
    Extension(client): Extension<Client>,
    extract::Path(assignment_id): extract::Path<Uuid>,
) -> ApiResult<Json<PolicyEngineAssignmentDto>> {
    let ctx = error::require_context(ctx)?;
    let assignment_id = AssignmentId(assignment_id);
    Ok(Json(
        client.get_assignment(&ctx, assignment_id).await?.into(),
    ))
}

/// `PATCH /policy-engine/v1/assignments/{id}`.
#[tracing::instrument(skip_all)]
pub async fn update_assignment(
    ctx: Option<Extension<SecurityContext>>,
    Extension(client): Extension<Client>,
    extract::Path(assignment_id): extract::Path<Uuid>,
    extract::Json(body): extract::Json<PolicyEngineUpdateAssignmentRequestDto>,
) -> ApiResult<Json<PolicyEngineAssignmentDto>> {
    let ctx = error::require_context(ctx)?;
    let assignment_id = AssignmentId(assignment_id);
    let assignment = client
        .update_assignment(&ctx, assignment_id, body.enforce)
        .await?;
    Ok(Json(assignment.into()))
}

/// `DELETE /policy-engine/v1/assignments/{id}`.
#[tracing::instrument(skip_all)]
pub async fn unassign(
    ctx: Option<Extension<SecurityContext>>,
    Extension(client): Extension<Client>,
    extract::Path(assignment_id): extract::Path<Uuid>,
) -> ApiResult<StatusCode> {
    let ctx = error::require_context(ctx)?;
    let assignment_id = AssignmentId(assignment_id);
    client.unassign(&ctx, assignment_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
