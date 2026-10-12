//! Optional POST receipts; claim, mutation and answer share the caller transaction.
use super::{ApiState, TxError, idempotency_key, replay_response};
use crate::infra::idempotency::{
    ClaimVerdict, IdempotencyClaimInput, claim_idempotency, lookup_idempotency,
    record_idempotency_answer,
};
use axum::{
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde_json::Value;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_db::secure::{AccessScope, DBRunner};
use uuid::Uuid;

pub(super) fn input(
    state: &ApiState,
    headers: &HeaderMap,
    endpoint: String,
    body: &Value,
) -> Result<Option<IdempotencyClaimInput>, CanonicalError> {
    Ok(idempotency_key(headers)?.map(|key| {
        IdempotencyClaimInput::new(
            endpoint,
            key,
            crate::domain::idempotency::payload_digest(body).to_vec(),
            crate::infra::storage::stored_now(),
            state.idempotency_retention_hours,
        )
    }))
}
fn response(verdict: ClaimVerdict) -> Result<Option<Response>, TxError> {
    match verdict {
        ClaimVerdict::Proceed => Ok(None),
        ClaimVerdict::Replay { status, body } => Ok(Some(reply(status, body))),
        ClaimVerdict::Refused(error) => Err(TxError::Refused(error)),
    }
}
/// Call after resource authorization and before external resolution.
pub(super) async fn lookup(
    runner: &impl DBRunner,
    tenant: Uuid,
    input: Option<&IdempotencyClaimInput>,
) -> Result<Option<Response>, TxError> {
    let Some(input) = input else {
        return Ok(None);
    };
    response(
        lookup_idempotency(runner, &AccessScope::for_tenant(tenant), tenant, input)
            .await
            .map_err(TxError::Repo)?,
    )
}
/// Acquire the claim only on the mutation's transaction.
pub(super) async fn begin(
    tx: &impl DBRunner,
    tenant: Uuid,
    input: Option<&IdempotencyClaimInput>,
) -> Result<Option<Response>, TxError> {
    let Some(input) = input else {
        return Ok(None);
    };
    response(
        claim_idempotency(tx, &AccessScope::for_tenant(tenant), tenant, input)
            .await
            .map_err(TxError::Repo)?,
    )
}
pub(super) async fn finish<T: serde::Serialize>(
    tx: &impl DBRunner,
    tenant: Uuid,
    input: Option<&IdempotencyClaimInput>,
    status: StatusCode,
    receipt: &T,
) -> Result<Response, TxError> {
    let body = serde_json::to_value(receipt)
        .map_err(|e| TxError::Repo(crate::infra::storage::RepoError::Db(e.to_string())))?;
    if let Some(input) = input {
        record_idempotency_answer(
            tx,
            &AccessScope::for_tenant(tenant),
            tenant,
            input,
            status,
            &body,
        )
        .await
        .map_err(TxError::Repo)?;
    }
    Ok(reply(i32::from(status.as_u16()), body))
}
fn reply(status: i32, body: Value) -> Response {
    let revision = body
        .get("revision")
        .or_else(|| body.get("version"))
        .and_then(Value::as_i64);
    let mut response = replay_response(status, body);
    if status >= 400 {
        response.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/problem+json"),
        );
    }
    if let Some(revision) = revision {
        let etag = crate::domain::concurrency::InternalRevision::new(revision);
        if let Ok(value) = super::preconditions::etag(etag).parse() {
            response
                .headers_mut()
                .insert(axum::http::header::ETAG, value);
        }
    }
    response
}
pub(super) fn param() -> toolkit::api::operation_builder::ParamSpec {
    toolkit::api::operation_builder::ParamSpec::header("Idempotency-Key")
        .description("Replay this endpoint's original receipt within the retention period")
}
