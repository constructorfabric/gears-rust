use std::sync::Arc;

use crate::domain::models::{ChatPatch, NewChat};
use axum::Extension;
use axum::extract::Path;
use modkit::api::odata::OData;
use modkit::api::prelude::*;
use modkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::rest::dto::{ChatDetailDto, CreateChatReq, UpdateChatReq};
use crate::module::AppServices;

/// POST /mini-chat/v1/chats
#[tracing::instrument(skip(svc, ctx, uri, req_body))]
pub(crate) async fn create_chat(
    uri: axum::http::Uri,
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<AppServices>>,
    Json(req_body): Json<CreateChatReq>,
) -> ApiResult<impl IntoResponse> {
    let new = NewChat {
        model: req_body.model,
        title: req_body.title,
        is_temporary: false,
    };

    let detail = svc.chats.create_chat(&ctx, new).await?;
    let id_str = detail.id.to_string();
    Ok(created_json(ChatDetailDto::from(detail), &uri, &id_str).into_response())
}

/// GET /mini-chat/v1/chats
#[tracing::instrument(skip(svc, ctx, query))]
pub(crate) async fn list_chats(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<AppServices>>,
    OData(query): OData,
) -> ApiResult<JsonPage<ChatDetailDto>> {
    let page = svc.chats.list_chats(&ctx, &query).await?;
    let page = page.map_items(ChatDetailDto::from);
    Ok(Json(page))
}

/// GET /mini-chat/v1/chats/{id}
#[tracing::instrument(skip(svc, ctx), fields(chat_id = %id))]
pub(crate) async fn get_chat(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<AppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<JsonBody<ChatDetailDto>> {
    let detail = svc.chats.get_chat(&ctx, id).await?;
    Ok(Json(ChatDetailDto::from(detail)))
}

/// PATCH /mini-chat/v1/chats/{id}
#[tracing::instrument(skip(svc, ctx, req_body), fields(chat_id = %id))]
pub(crate) async fn update_chat(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<AppServices>>,
    Path(id): Path<Uuid>,
    Json(req_body): Json<UpdateChatReq>,
) -> ApiResult<JsonBody<ChatDetailDto>> {
    let patch = ChatPatch {
        title: Some(Some(req_body.title)),
    };
    let detail = svc.chats.update_chat(&ctx, id, patch).await?;
    Ok(Json(ChatDetailDto::from(detail)))
}

/// DELETE /mini-chat/v1/chats/{id}
#[tracing::instrument(skip(svc, ctx), fields(chat_id = %id))]
pub(crate) async fn delete_chat(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<AppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    svc.chats.delete_chat(&ctx, id).await?;
    Ok(no_content().into_response())
}
