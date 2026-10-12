//! The list, the counts, the card and the three vote doors.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Extension, Path, Query};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bss_approvals_sdk::{VoteAction, VoteRequest};
use bss_rest::conditional_get::{PRIVATE_REVALIDATE, respond};
use serde::Deserialize;
use toolkit_canonical_errors::{CanonicalError, ForeignPassthrough};
use toolkit_odata::errors::OdataError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::dto::{InboxCountsDto, InboxUnitDto, InboxUnitListDto};
use crate::api::ApiState;
use crate::domain::error;
use crate::domain::query::{self, ListParams};
use crate::domain::read;

const IDEMPOTENCY_KEY: &str = "Idempotency-Key";

#[derive(Debug, Deserialize)]
pub(super) struct ListQuery {
    state: Option<String>,
    kind: Option<String>,
    ref_id: Option<Uuid>,
    book_id: Option<Uuid>,
    limit: Option<u64>,
    cursor: Option<String>,
    impact: Option<bool>,
    /// Keys other than the named fields. `$orderby` is taken here so the query struct
    /// does not rename a field to a non-snake-case wire name (DE0803). Any other key is refused.
    #[serde(flatten)]
    rest: BTreeMap<String, String>,
}

impl ListQuery {
    fn take_order(mut self) -> Result<(Self, Option<String>), CanonicalError> {
        let orderby = self.rest.remove("$orderby");
        if let Some(key) = self.rest.keys().next() {
            return Err(OdataError::invalid_argument()
                .with_field_violation(
                    "query",
                    format!("unknown query key {key}"),
                    "INVALID_QUERY_PARAMS",
                )
                .create());
        }
        Ok((self, orderby))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CountsQuery {
    state: Option<String>,
    kind: Option<String>,
    ref_id: Option<Uuid>,
    book_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CardQuery {
    impact: Option<bool>,
}

pub(super) async fn list_units(
    Extension(state): Extension<Arc<ApiState>>,
    ctx: Option<Extension<SecurityContext>>,
    headers: HeaderMap,
    query: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Response, CanonicalError> {
    let ctx = caller(ctx)?;
    let Query(query) = bad_query(query)?;
    let (query, orderby) = query.take_order()?;
    let prepared = query::prepare_list(&ListParams {
        state: query.state,
        kind: query.kind,
        ref_id: query.ref_id,
        book_id: query.book_id,
        limit: query.limit,
        cursor: query.cursor,
        orderby,
        impact: query.impact,
    })?;
    let listed = read::list_page(&state.hub, &state.sources, &ctx, &prepared).await?;
    let mut page = InboxUnitListDto {
        items: listed.units.into_iter().map(Into::into).collect(),
        next_cursor: listed.next_cursor,
        sources: listed.sources.into_iter().map(Into::into).collect(),
    };
    // AP-D-11: the merged page's submitters, voters and live subjects' actors in one lookup; the
    // sources' declared system actors read "System".
    state
        .actor_names
        .with_system_ids(read::system_actors(&state.hub, &state.sources))
        .fill(&ctx, &mut page)
        .await;
    // AP-D-10: a weak tag of the merged page, sources and names included; a match is 304.
    Ok(respond(&headers, &page, PRIVATE_REVALIDATE))
}

pub(super) async fn count_units(
    Extension(state): Extension<Arc<ApiState>>,
    ctx: Option<Extension<SecurityContext>>,
    headers: HeaderMap,
    query: Result<Query<CountsQuery>, QueryRejection>,
) -> Result<Response, CanonicalError> {
    let ctx = caller(ctx)?;
    let Query(query) = bad_query(query)?;
    let narrowing = bss_approvals_sdk::SourceNarrowing {
        state: query.state,
        kind: query.kind,
        ref_id: query.ref_id,
        book_id: query.book_id,
    };
    let counted = read::count_all(&state.hub, &state.sources, &ctx, &narrowing).await?;
    let body = InboxCountsDto::from_counts(counted.counts, counted.sources);
    // AP-D-10: a weak tag of the summed counts, sources included; a match is 304.
    Ok(respond(&headers, &body, PRIVATE_REVALIDATE))
}

pub(super) async fn get_unit(
    Extension(state): Extension<Arc<ApiState>>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    query: Result<Query<CardQuery>, QueryRejection>,
) -> Result<Json<InboxUnitDto>, CanonicalError> {
    let ctx = caller(ctx)?;
    let Query(query) = bad_query(query)?;
    let unit = read::get_unit(
        &state.hub,
        &state.sources,
        &ctx,
        id,
        query.impact.unwrap_or(true),
    )
    .await?;
    let mut card = InboxUnitDto::from(unit);
    // AP-D-11: the submitter, the voters and the live subject's actors in one lookup, the only
    // one this card makes: its source answers it unnamed.
    state
        .actor_names
        .with_system_ids(read::system_actors(&state.hub, &state.sources))
        .fill(&ctx, &mut card)
        .await;
    Ok(Json(card))
}

pub(super) async fn approve(
    state: Extension<Arc<ApiState>>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    vote(state, ctx, id, VoteAction::Approve, headers, body).await
}

pub(super) async fn reject(
    state: Extension<Arc<ApiState>>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    vote(state, ctx, id, VoteAction::Reject, headers, body).await
}

pub(super) async fn withdraw(
    state: Extension<Arc<ApiState>>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    vote(state, ctx, id, VoteAction::Withdraw, headers, body).await
}

async fn vote(
    Extension(state): Extension<Arc<ApiState>>,
    ctx: Option<Extension<SecurityContext>>,
    id: Uuid,
    action: VoteAction,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let ctx = match caller(ctx) {
        Ok(ctx) => ctx,
        Err(err) => return err.into_response(),
    };
    let request = match vote_request(&headers, body) {
        Ok(request) => request,
        Err(err) => return err.into_response(),
    };
    match read::vote_unit(&state.hub, &state.sources, &ctx, id, action, request).await {
        Ok(answered) => pass_through(answered),
        Err(err) => err.into_response(),
    }
}

fn vote_request(headers: &HeaderMap, body: Bytes) -> Result<VoteRequest, CanonicalError> {
    let Some(value) = headers.get(IDEMPOTENCY_KEY) else {
        return Err(key_invalid(
            "Idempotency-Key is required",
            "IDEMPOTENCY_KEY_REQUIRED",
        ));
    };
    let key = value.to_str().map_err(|_| {
        key_invalid(
            "Idempotency-Key is not valid text",
            "IDEMPOTENCY_KEY_INVALID",
        )
    })?;
    if key.is_empty() {
        return Err(key_invalid(
            "Idempotency-Key is required",
            "IDEMPOTENCY_KEY_REQUIRED",
        ));
    }
    Ok(VoteRequest {
        body: Vec::from(body),
        idempotency_key: Some(key.to_owned()),
    })
}

fn key_invalid(description: &str, reason: &str) -> CanonicalError {
    OdataError::invalid_argument()
        .with_field_violation(IDEMPOTENCY_KEY, description, reason)
        .create()
}

/// The owning door's answer as it left that door: status, headers and body. It is marked as a
/// passthrough, so the platform's error layer does not rewrite a refusal the door already shaped
/// (its `instance` names the door, AP-D-4).
fn pass_through(answered: bss_approvals_sdk::VoteResponse) -> Response {
    let Ok(status) = StatusCode::from_u16(answered.status) else {
        return CanonicalError::internal(
            "bss-approvals: a source vote returned a status that is not an HTTP status",
        )
        .create()
        .into_response();
    };
    let mut response = Response::new(axum::body::Body::from(answered.body));
    *response.status_mut() = status;
    for (name, value) in answered.headers {
        let Ok(name) = HeaderName::try_from(name) else {
            return CanonicalError::internal(
                "bss-approvals: a source vote returned a header name that is not valid",
            )
            .create()
            .into_response();
        };
        let Ok(value) = HeaderValue::try_from(value) else {
            return CanonicalError::internal(
                "bss-approvals: a source vote returned a header value that is not valid",
            )
            .create()
            .into_response();
        };
        response.headers_mut().append(name, value);
    }
    response.extensions_mut().insert(ForeignPassthrough);
    response
}

fn caller(ctx: Option<Extension<SecurityContext>>) -> Result<SecurityContext, CanonicalError> {
    let Some(Extension(ctx)) = ctx else {
        return Err(error::unauthenticated());
    };
    if ctx.subject_id().is_nil() || ctx.subject_tenant_id().is_nil() || ctx.subject_type().is_none()
    {
        return Err(error::unauthenticated());
    }
    Ok(ctx)
}

fn bad_query<T>(query: Result<Query<T>, QueryRejection>) -> Result<Query<T>, CanonicalError> {
    query.map_err(|e| {
        OdataError::invalid_argument()
            .with_field_violation("query", e.body_text(), "INVALID_QUERY_PARAMS")
            .create()
    })
}
