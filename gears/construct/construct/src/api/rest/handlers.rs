use std::sync::Arc;

use axum::extract::Extension;
use axum::http::header;
use toolkit::api::canonical_prelude::*;
use toolkit::api::rest::extract::{Json, Path};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::rest::error::for_settings;
use crate::api::rest::types::{ConcreteService, ConcreteSettings};

use super::dto::{
    CreateFoundationNoteRequest, FoundationNoteDto, PutSubjectSettingsRequest, SubjectSettingsDto,
};

/// @cpt-dod:cpt-cf-construct-dod-gear-foundation-create-route:p1
pub async fn create_note(
    // @cpt-begin:cpt-cf-construct-flow-gear-foundation-create-note:p1:inst-create-parse
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Json(req): Json<CreateFoundationNoteRequest>,
    // @cpt-end:cpt-cf-construct-flow-gear-foundation-create-note:p1:inst-create-parse
) -> ApiResult<impl IntoResponse> {
    let note = svc.create_note(&ctx, req.into()).await?;
    // @cpt-begin:cpt-cf-construct-flow-gear-foundation-create-note:p1:inst-create-return
    let dto: FoundationNoteDto = note.into();
    Ok((StatusCode::CREATED, Json(dto)))
    // @cpt-end:cpt-cf-construct-flow-gear-foundation-create-note:p1:inst-create-return
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
