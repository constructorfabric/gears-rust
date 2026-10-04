use std::sync::Arc;

use axum::{Json, extract::Extension};
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use crate::api::rest::routes::ConcreteService;

use super::dto::{CreateFoundationNoteRequest, FoundationNoteDto};

pub async fn create_note(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Json(req): Json<CreateFoundationNoteRequest>,
) -> ApiResult<impl IntoResponse> {
    let note = svc.create_note(&ctx, req.into()).await?;
    let dto: FoundationNoteDto = note.into();
    Ok((StatusCode::CREATED, Json(dto)))
}
