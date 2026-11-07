use std::sync::Arc;

use axum::extract::Extension;
use toolkit::api::canonical_prelude::*;
use toolkit::api::odata::OData;
use toolkit::api::rest::extract::{Json, Path, Query};
use toolkit_security::SecurityContext;
use tracing::Instrument as _;
use uuid::Uuid;

use crate::api::rest::error::{
    for_export, for_facts, for_rest, for_settings, no_such_fact, too_long,
};
use crate::api::rest::types::{
    ConcreteIntake, ConcreteProfile, ConcreteSettings, ConcreteSubjectControl,
};
use crate::domain::plan::NodeKey;
use crate::domain::planner::manage::ManageFacts;
use crate::domain::record_intake::IntakeOutcome;

use super::dto::{
    ManageFactsRequest, ManageFactsResponse, ProfileDto, ProfileQuery, PutSubjectSettingsRequest,
    RecordOutcomeDto, RecordQuery, RecordRequest, SubjectExportDto, SubjectFactDto,
    SubjectFactsPageDto, SubjectSettingsDto,
};

/// The answer's status: 202 for a received record, which is processed later,
/// and 200 for a repeat, which changes nothing.
fn status_of(outcome: IntakeOutcome) -> StatusCode {
    match outcome {
        IntakeOutcome::Received => StatusCode::ACCEPTED,
        IntakeOutcome::Repeat => StatusCode::OK,
    }
}

/// @cpt-dod:cpt-cf-construct-dod-record-intake-route:p1
pub async fn submit_record(
    // @cpt-begin:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-parse
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteIntake>>,
    Query(query): Query<RecordQuery>,
    Json(RecordRequest(record)): Json<RecordRequest>,
    // @cpt-end:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-parse
) -> ApiResult<impl IntoResponse> {
    // The error is mapped, and so logged, inside this span, so the log names
    // the connector and the tenant it sent the record for.
    let span = tracing::info_span!(
        "record_intake",
        tenant_id = %query.tenant,
        connector = %ctx.subject_id()
    );
    let outcome = async {
        svc.submit(&ctx, query.tenant, record)
            .await
            .map_err(for_rest)
    }
    .instrument(span)
    .await?;
    // @cpt-begin:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-answer
    Ok((status_of(outcome), Json(RecordOutcomeDto::from(outcome))))
    // @cpt-end:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-answer
}

/// @cpt-dod:cpt-cf-construct-dod-subject-settings-routes:p1
pub async fn get_subject_settings(
    // @cpt-begin:cpt-cf-construct-flow-subject-settings-view:p1:inst-view-parse
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteSettings>>,
    Path(subject_id): Path<Uuid>,
    // @cpt-end:cpt-cf-construct-flow-subject-settings-view:p1:inst-view-parse
) -> ApiResult<impl IntoResponse> {
    let settings = svc
        .owner_settings(&ctx, subject_id)
        .await
        .map_err(for_settings)?;
    // @cpt-begin:cpt-cf-construct-flow-subject-settings-view:p1:inst-view-return
    Ok(Json(SubjectSettingsDto::new(subject_id, settings)))
    // @cpt-end:cpt-cf-construct-flow-subject-settings-view:p1:inst-view-return
}

pub async fn put_subject_settings(
    // @cpt-begin:cpt-cf-construct-flow-subject-settings-change:p1:inst-change-parse
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteSettings>>,
    Path(subject_id): Path<Uuid>,
    Json(req): Json<PutSubjectSettingsRequest>,
    // @cpt-end:cpt-cf-construct-flow-subject-settings-change:p1:inst-change-parse
) -> ApiResult<impl IntoResponse> {
    let settings = svc
        .set_personalization(&ctx, subject_id, req.personalization_enabled)
        .await
        .map_err(for_settings)?;
    // @cpt-begin:cpt-cf-construct-flow-subject-settings-change:p1:inst-change-return
    Ok(Json(SubjectSettingsDto::new(subject_id, settings)))
    // @cpt-end:cpt-cf-construct-flow-subject-settings-change:p1:inst-change-return
}

/// Plans what the subject told an agent, stores it and answers what changed.
/// A field past its limit answers 400 before any model call.
///
/// @cpt-dod:cpt-cf-construct-dod-planner-manage-route:p1
pub async fn manage_facts(
    Extension(ctx): Extension<SecurityContext>,
    Extension(manage): Extension<Arc<ManageFacts>>,
    Path(subject_id): Path<Uuid>,
    Json(req): Json<ManageFactsRequest>,
) -> ApiResult<impl IntoResponse> {
    if let Some((field, limit)) = req.field_past_its_limit() {
        return Err(too_long(field, limit));
    }
    let managed = manage
        .manage(&ctx, subject_id, req.message, req.conversation)
        .await
        .map_err(|error| for_facts(error, subject_id))?;
    Ok(Json(ManageFactsResponse::from(managed)))
}

/// @cpt-dod:cpt-cf-construct-dod-profile-reader-route:p1
pub async fn get_profile(
    // @cpt-begin:cpt-cf-construct-flow-profile-reader-read:p1:inst-profile-parse
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteProfile>>,
    Path(subject_id): Path<Uuid>,
    Query(query): Query<ProfileQuery>,
    // @cpt-end:cpt-cf-construct-flow-profile-reader-read:p1:inst-profile-parse
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-construct-flow-profile-reader-read:p1:inst-profile-serve
    // The error is mapped, and so logged, inside this span, so the log names
    // the caller, the subject and the tenant.
    let tenant = ConcreteProfile::tenant_of(&ctx, query.tenant);
    let span = tracing::info_span!(
        "profile_read",
        %subject_id,
        caller = %ctx.subject_id(),
        tenant_id = %tenant
    );
    let profile = async {
        svc.read(&ctx, query.tenant, subject_id)
            .await
            .map_err(CanonicalError::from)
    }
    .instrument(span)
    .await?;
    // @cpt-end:cpt-cf-construct-flow-profile-reader-read:p1:inst-profile-serve
    // @cpt-begin:cpt-cf-construct-flow-profile-reader-read:p1:inst-profile-return
    Ok(Json(ProfileDto::from(profile)))
    // @cpt-end:cpt-cf-construct-flow-profile-reader-read:p1:inst-profile-return
}

/// The longest fact id the routes take; a graph node key is far shorter.
const MAX_FACT_ID_CHARS: usize = 200;

/// The fact id in the path, when it has the shape of a graph node key:
/// letters, digits, `:`, `-`, `_` and `.`, at most [`MAX_FACT_ID_CHARS`].
/// Anything else is no fact of the subject's, and never reaches the log.
fn fact_key(subject_id: Uuid, raw: String) -> Result<NodeKey, CanonicalError> {
    let plausible = (1..=MAX_FACT_ID_CHARS).contains(&raw.len())
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'-' | b'_' | b'.'));
    if plausible {
        Ok(NodeKey(raw))
    } else {
        tracing::warn!(len = raw.len(), "construct subject fact id malformed");
        Err(no_such_fact(subject_id))
    }
}

/// A span that names who asked for which subject, for the error mapped and
/// logged in it.
fn subject_span(name: &'static str, ctx: &SecurityContext, subject_id: Uuid) -> tracing::Span {
    tracing::info_span!(
        "subject_control",
        operation = name,
        %subject_id,
        caller = %ctx.subject_id(),
        tenant_id = %ctx.subject_tenant_id()
    )
}

/// @cpt-dod:cpt-cf-construct-dod-subject-control-view:p1
pub async fn list_subject_facts(
    // @cpt-begin:cpt-cf-construct-flow-subject-control-view:p1:inst-view-parse
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteSubjectControl>>,
    Path(subject_id): Path<Uuid>,
    OData(query): OData,
    // @cpt-end:cpt-cf-construct-flow-subject-control-view:p1:inst-view-parse
) -> ApiResult<Json<SubjectFactsPageDto>> {
    let page = async {
        svc.view(&ctx, subject_id, &query)
            .await
            .map_err(|e| for_facts(e, subject_id))
    }
    .instrument(subject_span("view", &ctx, subject_id))
    .await?;
    // @cpt-begin:cpt-cf-construct-flow-subject-control-view:p1:inst-view-return
    Ok(Json(SubjectFactsPageDto::from(page)))
    // @cpt-end:cpt-cf-construct-flow-subject-control-view:p1:inst-view-return
}

pub async fn get_subject_fact(
    // @cpt-begin:cpt-cf-construct-flow-subject-control-fact:p1:inst-fact-parse
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteSubjectControl>>,
    Path((subject_id, fact_id)): Path<(Uuid, String)>,
    // @cpt-end:cpt-cf-construct-flow-subject-control-fact:p1:inst-fact-parse
) -> ApiResult<Json<SubjectFactDto>> {
    let fact = async {
        let fact_id = fact_key(subject_id, fact_id)?;
        svc.fact(&ctx, subject_id, &fact_id)
            .await
            .map_err(|e| for_facts(e, subject_id))
    }
    .instrument(subject_span("read_fact", &ctx, subject_id))
    .await?;
    // @cpt-begin:cpt-cf-construct-flow-subject-control-fact:p1:inst-fact-return
    Ok(Json(SubjectFactDto::from(fact)))
    // @cpt-end:cpt-cf-construct-flow-subject-control-fact:p1:inst-fact-return
}

/// @cpt-dod:cpt-cf-construct-dod-subject-control-delete:p1
pub async fn delete_subject_fact(
    // @cpt-begin:cpt-cf-construct-flow-subject-control-delete:p1:inst-delete-parse
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteSubjectControl>>,
    Path((subject_id, fact_id)): Path<(Uuid, String)>,
    // @cpt-end:cpt-cf-construct-flow-subject-control-delete:p1:inst-delete-parse
) -> ApiResult<StatusCode> {
    async {
        let fact_id = fact_key(subject_id, fact_id)?;
        svc.delete(&ctx, subject_id, &fact_id)
            .await
            .map_err(|e| for_facts(e, subject_id))
    }
    .instrument(subject_span("delete_fact", &ctx, subject_id))
    .await?;
    // @cpt-begin:cpt-cf-construct-flow-subject-control-delete:p1:inst-delete-return
    Ok(StatusCode::NO_CONTENT)
    // @cpt-end:cpt-cf-construct-flow-subject-control-delete:p1:inst-delete-return
}

/// @cpt-dod:cpt-cf-construct-dod-subject-control-export:p1
pub async fn export_subject(
    // @cpt-begin:cpt-cf-construct-flow-subject-control-export:p1:inst-export-parse
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteSubjectControl>>,
    Path(subject_id): Path<Uuid>,
    // @cpt-end:cpt-cf-construct-flow-subject-control-export:p1:inst-export-parse
) -> ApiResult<impl IntoResponse> {
    let export = async { svc.export(&ctx, subject_id).await.map_err(for_export) }
        .instrument(subject_span("export", &ctx, subject_id))
        .await?;
    // @cpt-begin:cpt-cf-construct-flow-subject-control-export:p1:inst-export-return
    let disposition = format!("attachment; filename=\"subject-{subject_id}.json\"");
    Ok((
        [(axum::http::header::CONTENT_DISPOSITION, disposition)],
        Json(SubjectExportDto::new(
            export,
            time::OffsetDateTime::now_utc(),
        )),
    ))
    // @cpt-end:cpt-cf-construct-flow-subject-control-export:p1:inst-export-return
}
