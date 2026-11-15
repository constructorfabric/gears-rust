//! `GET /v1/topics`, `GET /v1/topics/segments` (`DESIGN.md` "Module
//! Structure").

use axum::Extension;
use axum::extract::Query;
use toolkit::api::canonical_prelude::*;
use toolkit_odata::ast::Value as ODataValue;
use toolkit_security::SecurityContext;

use super::dto::{TopicDto, TopicSegmentsQuery, TopicSegmentsResponse};
use crate::api::rest::pagination::{eval_filter, paginate_by_key};
use crate::api::rest::state::HandlerState;
use crate::domain::ingest::Selection;
use crate::domain::model::Topic;

/// # Errors
/// Returns `400` for an invalid `$filter` expression or pagination cursor,
/// `503` if the topics or a topic's event types cannot be read, or `500` if
/// the policy cannot be evaluated.
pub async fn list_topics(
    Extension(ctx): Extension<SecurityContext>,
    Extension(state): Extension<HandlerState>,
    OData(query): OData,
) -> ApiResult<JsonPage<TopicDto>> {
    // `$filter` selects before the broker authorizes each topic, so a topic
    // the filter drops costs no PDP call.
    let filter = query.filter();
    let keep = |t: &Topic| {
        filter.is_none_or(|filter| {
            eval_filter(filter, &|field| match field {
                "id" => Some(ODataValue::String(t.id.to_string())),
                _ => None,
            })
        })
    };
    let topics = state
        .ingest
        .list_topics(&ctx, &Selection::only(&keep))
        .await?;
    let page = paginate_by_key(topics, &query, "id", |t| t.id.to_string())?;
    Ok(Json(page.map_items(TopicDto::from)))
}

/// # Errors
/// Returns the mapped `CanonicalError` for `DomainError::Forbidden` if the
/// caller may not read the topic (before the topic is looked up), for
/// `DomainError::NotFound` if the topic isn't registered, a `400` if
/// `topic` isn't a well-formed GTS instance id or the partition is outside
/// the topic's, `503` when the database or the topic's storage backend is
/// unavailable, or `500` if the policy cannot be evaluated.
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
