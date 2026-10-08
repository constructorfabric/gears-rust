use std::sync::Arc;

use axum::Extension;
use toolkit::api::canonical_prelude::*;
use toolkit::api::rest::extract::Path;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{ReactionDto, SetReactionReq};
use crate::gear::AppServices;

/// PUT /mini-chat/v1/chats/{id}/messages/{msg_id}/reaction
#[tracing::instrument(skip(svc, ctx, req_body), fields(chat_id = %chat_id, msg_id = %msg_id))]
pub(crate) async fn put_reaction(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<AppServices>>,
    Path((chat_id, msg_id)): Path<(uuid::Uuid, uuid::Uuid)>,
    extract::Json(req_body): extract::Json<SetReactionReq>,
) -> ApiResult<JsonBody<ReactionDto>> {
    let result = svc
        .reactions
        .set_reaction(&ctx, chat_id, msg_id, &req_body.reaction)
        .await?;
    Ok(Json(ReactionDto::from(result)))
}

/// DELETE /mini-chat/v1/chats/{id}/messages/{msg_id}/reaction
#[tracing::instrument(skip(svc, ctx), fields(chat_id = %chat_id, msg_id = %msg_id))]
pub(crate) async fn delete_reaction(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<AppServices>>,
    Path((chat_id, msg_id)): Path<(uuid::Uuid, uuid::Uuid)>,
) -> ApiResult<impl IntoResponse> {
    svc.reactions.delete_reaction(&ctx, chat_id, msg_id).await?;
    Ok(no_content().into_response())
}
