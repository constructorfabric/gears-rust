//! `GET /v1/topics`, `GET /v1/topics/segments` (`DESIGN.md:586`).

use axum::Extension;
use axum::extract::Query;
use toolkit::api::canonical_prelude::*;
use toolkit_odata::ast::Value as ODataValue;
use toolkit_security::SecurityContext;

use super::dto::{TopicDto, TopicSegmentsQuery, TopicSegmentsResponse};
use crate::api::rest::pagination::{eval_filter, paginate_by_key};
use crate::api::rest::state::HandlerState;

/// # Errors
/// Returns `400` for an invalid `$filter` expression or pagination cursor.
pub async fn list_topics(
    Extension(ctx): Extension<SecurityContext>,
    Extension(state): Extension<HandlerState>,
    OData(query): OData,
) -> ApiResult<JsonPage<TopicDto>> {
    let mut topics = state.ingest.list_topics(&ctx).await?;
    if let Some(filter) = query.filter() {
        topics.retain(|t| {
            eval_filter(filter, &|field| match field {
                "id" => Some(ODataValue::String(t.id.to_string())),
                _ => None,
            })
        });
    }
    let page = paginate_by_key(topics, &query, "id", |t| t.id.to_string())?;
    Ok(Json(page.map_items(TopicDto::from)))
}

/// # Errors
/// Returns the mapped `CanonicalError` for `DomainError::NotFound` if the
/// topic isn't registered, or a `400` if `topic` isn't a well-formed GTS
/// instance id.
pub async fn list_topic_segments(
    Extension(ctx): Extension<SecurityContext>,
    Extension(state): Extension<HandlerState>,
    Query(query): Query<TopicSegmentsQuery>,
) -> ApiResult<JsonBody<TopicSegmentsResponse>> {
    let topic = crate::domain::id_parse::parse_topic_id(&query.topic)?;
    let manifest = state
        .ingest
        .list_topic_segments(&ctx, &topic, query.partition)
        .await?;
    Ok(Json(manifest.into()))
}
