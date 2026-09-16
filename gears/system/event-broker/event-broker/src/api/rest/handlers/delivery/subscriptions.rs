//! JOIN, list, read, leave, seek (`DESIGN.md:591`,
//! `docs/schemas/gts.cf.core.events.subscription.v1~.schema.json`).

use std::collections::BTreeMap;

use axum::Extension;
use axum::extract::Path;
use axum::http::Uri;
use toolkit::api::canonical_prelude::*;
use toolkit_gts::GtsInstanceId;
use toolkit_odata::ast::Value as ODataValue;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::dto::{
    JoinSubscriptionRequest, ResolvedPositionEntryDto, SeekResponse, SeekSubscriptionRequest,
    SubscriptionDto,
};
use crate::api::rest::handlers::action_suffix::parse_action_suffixed_id;
use crate::api::rest::pagination::{eval_filter, paginate_by_key};
use crate::api::rest::state::HandlerState;
use crate::domain::delivery::{JoinRequest, SeekTarget};
use crate::domain::error::DomainError;
use crate::domain::model::Interest;

const DEFAULT_SESSION_TIMEOUT_SECS: u64 = 30;

/// Parses `session_timeout` (`docs/schemas/gts.cf.core.events.subscription.v1~.schema.json`'s
/// `"format": "duration"`, i.e. ISO 8601) via
/// `toolkit_utils::iso8601_duration::Iso8601Duration`. `None` (field absent)
/// defaults to `PT30S` - documented, correct behavior. A value that is
/// present but fails to parse, or parses to a zero duration, is rejected
/// with `400 Validation` instead of silently substituting the default (the
/// previous hand-rolled parser here only understood `PT<n>S`/`PT<n>M` and
/// silently defaulted on anything else - `PT1H`, `P1D`, combined
/// `PT1H30M`, or outright garbage - and had a distinct overflow bug where a
/// sufficiently large minutes value silently produced a *1-second* timeout
/// instead of the 30s default).
fn parse_session_timeout(raw: Option<&str>) -> Result<std::time::Duration, DomainError> {
    let Some(raw) = raw else {
        return Ok(std::time::Duration::from_secs(DEFAULT_SESSION_TIMEOUT_SECS));
    };
    let duration = raw
        .parse::<toolkit_utils::iso8601_duration::Iso8601Duration>()
        .map_err(|err| DomainError::Validation {
            code: "InvalidSessionTimeout",
            message: format!("'{raw}' is not a valid ISO 8601 duration: {err}"),
        })?
        .as_duration();
    if duration.is_zero() {
        return Err(DomainError::Validation {
            code: "InvalidSessionTimeout",
            message: format!("session_timeout must be a positive duration, got '{raw}'"),
        });
    }
    Ok(duration)
}

/// # Errors
/// Returns the mapped `CanonicalError` for any `DomainError`
/// `DeliveryService::join` produces (consumer group/topic not found, bad
/// type pattern).
/// Named consumer groups are recognized but not implemented yet; only anonymous
/// groups (`<type>~<uuid>`) are. A named id (a valid consumer-group instance id
/// whose suffix is not a uuid) is rejected as `Unimplemented` (501) rather than
/// the misleading `404 NotFound` it would otherwise get, so a caller can tell
/// "not supported yet" from "does not exist". Anonymous detection reuses the
/// SDK's `ConsumerGroupId::try_from_gts` (Some for anonymous, None for named).
pub(super) fn reject_named_group(id: &GtsInstanceId) -> Result<(), DomainError> {
    if event_broker_sdk::ConsumerGroupId::try_from_gts(id.as_ref()).is_none() {
        return Err(DomainError::Unimplemented(
            "named consumer groups are not implemented".to_owned(),
        ));
    }
    Ok(())
}

/// # Errors
/// Returns `400` for an invalid `client_agent`, `session_timeout`,
/// `consumer_group` id, or interest entry; `501` for a named consumer group,
/// which is not implemented yet; and the mapped `CanonicalError` for any
/// `DomainError` `DeliveryService::join` produces.
pub async fn join_subscription(
    Extension(ctx): Extension<SecurityContext>,
    Extension(state): Extension<HandlerState>,
    uri: Uri,
    Json(req): Json<JoinSubscriptionRequest>,
) -> ApiResult<impl IntoResponse> {
    event_broker_sdk::validate::client_agent(&req.client_agent).map_err(DomainError::from)?;
    let session_timeout = parse_session_timeout(req.session_timeout.as_deref())?;
    let consumer_group =
        GtsInstanceId::try_new(&req.consumer_group).map_err(|err| DomainError::Validation {
            code: "InvalidBody",
            message: format!(
                "'{}' is not a valid GTS instance id: {err}",
                req.consumer_group
            ),
        })?;
    reject_named_group(&consumer_group)?;
    let request = JoinRequest {
        consumer_group,
        client_agent: req.client_agent,
        interests: req
            .interests
            .into_iter()
            .map(Interest::try_from)
            .collect::<Result<Vec<_>, _>>()?,
        session_timeout,
    };
    let subscription = state.delivery.join(&ctx, request).await?;
    let id = subscription.id.to_string();
    Ok(created_json(SubscriptionDto::from(subscription), &uri, &id))
}

/// # Errors
/// Returns `400` for an invalid `$filter` expression or pagination cursor.
pub async fn list_subscriptions(
    Extension(ctx): Extension<SecurityContext>,
    Extension(state): Extension<HandlerState>,
    OData(query): OData,
) -> ApiResult<JsonPage<SubscriptionDto>> {
    let mut subscriptions = state.delivery.list_subscriptions(&ctx).await?;
    if let Some(filter) = query.filter() {
        subscriptions.retain(|s| {
            eval_filter(filter, &|field| match field {
                "id" => Some(ODataValue::String(s.id.to_string())),
                "consumer_group" => Some(ODataValue::String(s.consumer_group.to_string())),
                _ => None,
            })
        });
    }
    // Newest first (most-recently created), then id as a stable tiebreaker, so
    // a freshly created subscription is on the first page rather than wherever
    // its random id happened to fall. `paginate_by_key` sorts its key string
    // ascending, so the timestamp is inverted (`MAX - nanos`) to make recent
    // creations sort first; the nanos are zero-padded so the string key sorts
    // in the same order as the numbers.
    let page = paginate_by_key(subscriptions, &query, "created_at", |s| {
        format!(
            "{:020}-{}",
            i64::MAX - s.created_at.timestamp_nanos_opt().unwrap_or(0),
            s.id
        )
    })?;
    Ok(Json(page.map_items(SubscriptionDto::from)))
}

/// # Errors
/// Returns the mapped `CanonicalError` for `DomainError::NotFound` if the
/// subscription doesn't exist.
pub async fn get_subscription(
    Extension(ctx): Extension<SecurityContext>,
    Extension(state): Extension<HandlerState>,
    Path(id): Path<Uuid>,
) -> ApiResult<JsonBody<SubscriptionDto>> {
    let subscription = state.delivery.get_subscription(&ctx, id).await?;
    Ok(Json(subscription.into()))
}

/// # Errors
/// Returns the mapped `CanonicalError` for `DomainError::NotFound` if the
/// subscription doesn't exist.
pub async fn leave_subscription(
    Extension(ctx): Extension<SecurityContext>,
    Extension(state): Extension<HandlerState>,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    state.delivery.leave(&ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The `:seek` suffix can't be a normal axum route template - a bare `{id}`
/// is registered instead, split via `action_suffix::parse_action_suffixed_id`
/// (shared with `producers::reset_producer`).
///
/// # Errors
/// Returns the mapped `CanonicalError` for any `DomainError`
/// `DeliveryService::seek` produces, or a `400` if the path segment isn't
/// `<uuid>:seek`, the body isn't valid JSON, or a `value` sentinel is
/// malformed.
pub async fn seek_subscription(
    Extension(ctx): Extension<SecurityContext>,
    Extension(state): Extension<HandlerState>,
    Path(raw_id): Path<String>,
    Json(req): Json<SeekSubscriptionRequest>,
) -> ApiResult<JsonBody<SeekResponse>> {
    let subscription_id = parse_action_suffixed_id(&raw_id, "seek", "subscription")?;

    let mut targets = Vec::new();
    for (raw_topic, entries) in req.positions {
        let topic = GtsInstanceId::try_new(&raw_topic).map_err(|err| DomainError::Validation {
            code: "InvalidBody",
            message: format!("'{raw_topic}' is not a valid GTS instance id: {err}"),
        })?;
        for entry in entries {
            targets.push(SeekTarget {
                topic: topic.clone(),
                partition: entry.partition,
                value: entry.value.into_domain()?,
            });
        }
    }

    let resolved = state
        .delivery
        .seek(&ctx, subscription_id, req.topology_version, targets)
        .await?;
    let mut positions: BTreeMap<String, Vec<ResolvedPositionEntryDto>> = BTreeMap::new();
    for p in resolved {
        positions
            .entry(p.topic.into_string())
            .or_default()
            .push(ResolvedPositionEntryDto {
                partition: p.partition,
                value: p.offset,
            });
    }
    Ok(Json(SeekResponse { positions }))
}
