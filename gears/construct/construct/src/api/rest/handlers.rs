use std::sync::Arc;

use axum::extract::Extension;
use toolkit::api::canonical_prelude::*;
use toolkit::api::rest::extract::Json;
use toolkit_security::SecurityContext;

use crate::api::rest::types::ConcreteService;

use super::dto::{CreateFoundationNoteRequest, FoundationNoteDto};

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
