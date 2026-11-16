//! `GET /v1/event-types` (`DESIGN.md` "Module Structure").

use axum::Extension;
use toolkit::api::canonical_prelude::*;
use toolkit_odata::ast::Value as ODataValue;
use toolkit_security::SecurityContext;

use super::dto::EventTypeDto;
use crate::api::rest::pagination::{eval_filter, paginate_by_key};
use crate::api::rest::state::HandlerState;
use crate::domain::ingest::Selection;
use event_broker_sdk::models::EventType;

/// # Errors
///
/// Returns `400` for an invalid `$filter` expression or pagination cursor,
/// `503` if the event types cannot be read, or `500` if the policy cannot be
/// evaluated.
pub async fn list_event_types(
    Extension(ctx): Extension<SecurityContext>,
    Extension(state): Extension<HandlerState>,
    OData(query): OData,
) -> ApiResult<JsonPage<EventTypeDto>> {
    // `$filter` selects before the broker authorizes each type, so a type
    // the filter drops costs no PDP call.
    let filter = query.filter();
    let keep = |t: &EventType| {
        filter.is_none_or(|filter| {
            eval_filter(filter, &|field| match field {
                "id" => Some(ODataValue::String(t.id.to_string())),
                "topic" => Some(ODataValue::String(t.topic.to_string())),
                _ => None,
            })
        })
    };
    let event_types = state
        .ingest
        .list_event_types(&ctx, &Selection::only(&keep))
        .await?;
    let page = paginate_by_key(event_types, &query, "id", |t| t.id.to_string())?;
    Ok(Json(page.map_items(EventTypeDto::from)))
}
