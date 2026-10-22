use std::sync::Arc;

use axum::extract::Extension;
use axum::http::header;
use toolkit::api::canonical_prelude::*;
use toolkit::api::rest::extract::{Json, Path, Query};
use toolkit_security::SecurityContext;
use tracing::Instrument as _;
use uuid::Uuid;

use crate::api::rest::error::{for_rest, for_settings};
use crate::api::rest::types::{ConcreteIntake, ConcreteSettings};
use crate::domain::record_intake::IntakeOutcome;

use super::dto::{
    PutSubjectSettingsRequest, RecordOutcomeDto, RecordQuery, RecordRequest, SubjectSettingsDto,
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
    Ok(not_stored(SubjectSettingsDto::new(subject_id, settings)))
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
    Ok(not_stored(SubjectSettingsDto::new(subject_id, settings)))
    // @cpt-end:cpt-cf-construct-flow-subject-settings-change:p1:inst-change-return
}

/// One subject's settings, marked `Cache-Control: no-store`, so no proxy or
/// browser cache keeps one subject's privacy state.
fn not_stored(dto: SubjectSettingsDto) -> impl IntoResponse {
    ([(header::CACHE_CONTROL, "no-store")], Json(dto))
}
