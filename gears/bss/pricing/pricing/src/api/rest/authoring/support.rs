//! Canonical failures, transaction retries, and audit plumbing shared by the doors.
use crate::{
    authz,
    infra::{
        events::{self, EventSink, TxOutbox},
        storage::{RepoError, repo},
    },
};
use axum::{
    Extension, Json,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use toolkit::api::canonical_prelude::{CanonicalError, resource_error};
use toolkit_db::{
    Db, DbTx,
    secure::{AccessScope, DBRunner},
};
use toolkit_security::SecurityContext;
use uuid::Uuid;
#[resource_error(gts_id!("cf.bss.pricing.price_book.v1~"))]
struct PricingResource;
#[cfg(test)]
#[path = "support_tests.rs"]
mod tests;
/// The refusal of a REST caller that asserts pricing's own system actor (D-424).
pub const SYSTEM_ACTOR_RESERVED: &str = "SYSTEM_ACTOR_RESERVED";
/// The caller of a REST door: 401 `AUTHENTICATION_REQUIRED` without a subject, a tenant or a
/// subject type. Pricing's own system actor, in either half, is 403 `SYSTEM_ACTOR_RESERVED`
/// (D-424, products P-D-222): Products' registry trusts it in-process, and a door hands the
/// registry its caller's context, so no REST caller may act as it, whatever its token asserts.
/// Another system subject (Rating's, Subscriptions') passes. Every door calls this first.
/// A caller with a subject, a tenant and a subject type.
#[must_use]
pub fn authenticated(ctx: &SecurityContext) -> bool {
    !ctx.subject_id().is_nil() && !ctx.subject_tenant_id().is_nil() && ctx.subject_type().is_some()
}
pub fn require_authenticated(
    ctx: Option<Extension<SecurityContext>>,
) -> Result<SecurityContext, CanonicalError> {
    let ctx = ctx
        .map(|Extension(c)| c)
        .filter(authenticated)
        .ok_or_else(|| {
            CanonicalError::unauthenticated()
                .with_reason("AUTHENTICATION_REQUIRED")
                .create()
        })?;
    if bss_products_sdk::is_pricing_system_actor(&ctx) {
        tracing::warn!(
            target: "pricing.authz.deny",
            subject_id = %ctx.subject_id(),
            subject_tenant_id = %ctx.subject_tenant_id(),
            subject_type = ctx.subject_type().unwrap_or_default(),
            reason = SYSTEM_ACTOR_RESERVED,
            "bss-pricing: a REST caller asserted pricing's system actor"
        );
        return Err(forbidden(SYSTEM_ACTOR_RESERVED));
    }
    Ok(ctx)
}
/// A denial is 403 with the PDP's reason (logged where [`authz::access_scope`] made it); an
/// unreachable PDP is 503.
pub fn authz_failure(error: authz::AuthzError) -> CanonicalError {
    match error {
        authz::AuthzError::Denied(d) => PricingResource::permission_denied()
            .with_reason(d.reason)
            .create(),
        authz::AuthzError::Unavailable(detail) => {
            tracing::error!(detail, "pricing authorization unavailable");
            CanonicalError::service_unavailable().create()
        }
    }
}
pub fn invalid(field: &str, code: &str) -> CanonicalError {
    invalid_because(field, code, code)
}
/// [`invalid`] whose violation tells the client what to send instead.
pub fn invalid_because(field: &str, code: &str, description: &str) -> CanonicalError {
    PricingResource::invalid_argument()
        .with_field_violation(field, description, code)
        .create()
}
/// Each plain key in `allowed` at most once. `skip` drops a key the extractor owns. An unknown
/// key is 400 `QUERY_INVALID` with `unknown`'s detail. The keys that were judged come back in order.
pub fn plain_keys<'a>(
    pairs: &'a [(String, String)],
    allowed: &[&str],
    skip: impl Fn(&str) -> bool,
    unknown: impl Fn(&str) -> String,
) -> Result<Vec<&'a str>, CanonicalError> {
    let mut seen = Vec::new();
    for (key, _) in pairs {
        let key = key.as_str();
        if skip(key) {
            continue;
        }
        if !allowed.contains(&key) {
            return Err(invalid_because(key, "QUERY_INVALID", &unknown(key)));
        }
        if seen.contains(&key) {
            return Err(invalid_because(
                key,
                "QUERY_INVALID",
                &format!("`{key}` is given more than once"),
            ));
        }
        seen.push(key);
    }
    Ok(seen)
}
/// The first 8 bytes of the SHA-256 of `payload`, as hex: a list cursor's narrowing hash.
pub fn page_hash(payload: &serde_json::Value) -> Result<String, CanonicalError> {
    let digest =
        crate::api::rest::preconditions::request_digest(payload).map_err(CanonicalError::from)?;
    Ok(digest
        .iter()
        .take(8)
        .fold(String::with_capacity(16), |mut hex, byte| {
            const DIGITS: &[u8; 16] = b"0123456789abcdef";
            hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
            hex.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
            hex
        }))
}
/// A 403 with its own code: the caller may act on the resource type, not on this one.
pub fn forbidden(code: &str) -> CanonicalError {
    PricingResource::permission_denied()
        .with_reason(code)
        .create()
}
/// [`forbidden`] whose detail names what forbids the act (for example the other author's
/// draft). The toolkit fixes a permission problem's detail text, so it is set on the rendered
/// problem and read back.
pub fn forbidden_because(code: &str, detail: impl Into<String>) -> CanonicalError {
    let mut problem = toolkit::api::canonical_prelude::Problem::from(forbidden(code));
    problem.detail = detail.into();
    CanonicalError::try_from(problem).unwrap_or_else(|_| forbidden(code))
}
pub fn conflict(code: &str) -> CanonicalError {
    PricingResource::aborted(code).with_reason(code).create()
}
/// [`conflict`] whose detail names what is in conflict (for example the dimension value a price
/// uses, D-436); the detail leads with the code.
pub fn conflict_because(code: &str, detail: impl Into<String>) -> CanonicalError {
    PricingResource::aborted(format!("{code}: {}", detail.into()))
        .with_reason(code)
        .create()
}
/// D-522: an entry whose reference was released is read-only. While its book is archived the
/// refusal is 409 `BOOK_ARCHIVED`; an entry an unarchive could not re-reserve (its SKU refused) is
/// 409 `ENTRY_REFERENCE_RELEASED`. Any other entry passes, with no read.
/// # Errors
/// The two refusals above; storage failures.
pub async fn writable_entry(
    tx: &impl DBRunner,
    entry: &crate::infra::storage::entity::price_book_entry::Model,
) -> Result<(), DoorError> {
    use crate::domain::price_book_entry::ReferenceState;
    if entry.reference_state != ReferenceState::Released.as_str() {
        return Ok(());
    }
    let archived = repo::book_repo::find(
        tx,
        &AccessScope::for_tenant(entry.tenant_id),
        entry.tenant_id,
        entry.book_id,
    )
    .await?
    .is_some_and(|book| book.archived_at.is_some());
    Err(conflict(if archived {
        "BOOK_ARCHIVED"
    } else {
        "ENTRY_REFERENCE_RELEASED"
    })
    .into())
}
pub fn missing() -> CanonicalError {
    PricingResource::not_found("Price book not found")
        .with_resource("price_book")
        .create()
}
/// A price book entry the caller's tenant does not hold: 404 on resource `price_book_entry`.
/// A not-found problem carries no reason, so its detail leads with the code `ENTRY_NOT_FOUND`.
pub fn missing_entry() -> CanonicalError {
    PricingResource::not_found("ENTRY_NOT_FOUND: price book entry not found")
        .with_resource("price_book_entry")
        .create()
}
/// A named pricing resource that the caller's tenant does not hold.
pub fn missing_what(what: &str) -> CanonicalError {
    PricingResource::not_found(format!("{what} not found"))
        .with_resource(what)
        .create()
}
/// What a claimed key answers, the one mapping of every door (whole-branch review PS-43):
/// `Ok(None)` when this call holds the key, the stored status and body of an answered key the
/// same payload claims, and the refusal otherwise. The match is exhaustive, so a new claim
/// outcome does not compile until it is answered here.
/// # Errors
/// A different payload under the key is `IDEMPOTENCY_CONFLICT`, in either state; a live claim of
/// the same payload, or a lost takeover race, is `IDEMPOTENCY_KEY_IN_FLIGHT`.
pub fn held(
    claim: repo::idempotency_repo::IdempotencyClaim,
    digest: &[u8],
) -> Result<Option<(i32, serde_json::Value)>, CanonicalError> {
    use repo::idempotency_repo::IdempotencyClaim;
    match claim {
        IdempotencyClaim::Claimed => Ok(None),
        IdempotencyClaim::Answered {
            payload_hash,
            response_status,
            response_body,
        } => {
            if payload_hash == digest {
                Ok(Some((response_status, response_body)))
            } else {
                Err(conflict("IDEMPOTENCY_CONFLICT"))
            }
        }
        IdempotencyClaim::InFlight { payload_hash, .. } => {
            Err(conflict(if payload_hash == digest {
                "IDEMPOTENCY_KEY_IN_FLIGHT"
            } else {
                "IDEMPOTENCY_CONFLICT"
            }))
        }
        IdempotencyClaim::TakeoverRaceLost => Err(conflict("IDEMPOTENCY_KEY_IN_FLIGHT")),
    }
}
/// A stored status, back as the one the caller was told.
/// # Errors
/// A stored status that is not an HTTP status is an internal failure.
pub fn stored_status(status: i32) -> Result<StatusCode, CanonicalError> {
    u16::try_from(status)
        .ok()
        .and_then(|s| StatusCode::from_u16(s).ok())
        .ok_or_else(|| CanonicalError::internal("invalid stored status").create())
}
/// Replay a completed command before detached dependency observations.
/// # Errors
/// A conflicting payload or live claim retains its usual canonical refusal.
pub async fn replay(
    db: &Db,
    tenant: Uuid,
    endpoint: &str,
    key: &str,
    digest: &[u8],
) -> Result<Option<Response>, DoorError> {
    let conn = db.conn()?;
    let Some(claim) = repo::idempotency_repo::lookup_idempotency_key(
        &conn,
        &AccessScope::for_tenant(tenant),
        tenant,
        endpoint,
        key,
        crate::infra::storage::stored_now(),
    )
    .await?
    else {
        return Ok(None);
    };
    let Some((status, body)) = held(claim, digest)? else {
        return Ok(None);
    };
    Ok(Some(response(
        stored_status(status)?,
        &body["body"],
        body["etag"].as_u64(),
    )?))
}
/// Claim a POST's key inside the mutation transaction, or replay its stored answer.
/// # Errors
/// A different payload under the key is `IDEMPOTENCY_CONFLICT`; a live claim is in flight.
pub async fn claim(
    tx: &impl DBRunner,
    tenant: Uuid,
    endpoint: &str,
    key: &str,
    digest: &[u8],
) -> Result<Option<Response>, DoorError> {
    let now = crate::infra::storage::stored_now();
    let scope = AccessScope::for_tenant(tenant);
    let claim = repo::idempotency_repo::claim_idempotency_key(
        tx,
        &scope,
        tenant,
        endpoint,
        key,
        digest,
        now,
        now + time::Duration::hours(24),
    )
    .await?;
    let Some((status, body)) = held(claim, digest)? else {
        return Ok(None);
    };
    Ok(Some(response(
        stored_status(status)?,
        &body["body"],
        body["etag"].as_u64(),
    )?))
}
/// Record the answer of a claimed key in the same transaction and render it.
/// # Errors
/// A lost claim is an internal failure; the transaction rolls back.
pub async fn answer<T: serde::Serialize>(
    tx: &impl DBRunner,
    tenant: Uuid,
    endpoint: &str,
    key: &str,
    status: StatusCode,
    body: &T,
    etag: Option<u64>,
) -> Result<Response, DoorError> {
    let body = value(body)?;
    if repo::idempotency_repo::answer_idempotency_key(
        tx,
        &AccessScope::for_tenant(tenant),
        tenant,
        endpoint,
        key,
        i32::from(status.as_u16()),
        serde_json::json!({ "etag": etag, "body": body }),
        None,
    )
    .await?
        != repo::idempotency_repo::IdempotencyAnswer::Recorded
    {
        return Err(CanonicalError::internal("idempotency claim lost")
            .create()
            .into());
    }
    Ok(response(status, &body, etag)?)
}
#[derive(Debug, thiserror::Error)]
pub enum DoorError {
    /// Detached observations no longer describe the local selection; recapture after rollback.
    #[error("the detached selection moved")]
    SelectionMoved,
    #[error(transparent)]
    Repo(#[from] RepoError),
    #[error(transparent)]
    Api(#[from] CanonicalError),
    /// A vote named another generation; the answer carries the current one.
    #[error("the vote names another generation; the unit is at {current}")]
    Generation { current: i32 },
}
/// Refusals of the shared approval engine and the `prices` subject, with their codes.
///
/// A pure-rule refusal is 400 with its code (D-403); a conflict is 409; separation of duties
/// and the submitter-only withdraw are 403. Database errors stay typed for the retry loop.
#[must_use]
pub fn approval_failure(error: bss_approval::ApprovalError) -> DoorError {
    use bss_approval::ApprovalError as A;
    match error {
        A::Db(source) => DoorError::Repo(RepoError::Driver {
            context: "approval".into(),
            source,
        }),
        A::InvalidSubmit { code, field, .. } => match code {
            "PRICE_NOT_DRAFT"
            | "ENTRY_REFERENCE_LOST"
            // D-522: a released entry's prices, as its doors answer them.
            | "BOOK_ARCHIVED"
            | "ENTRY_REFERENCE_RELEASED"
            | "REVISION_NOT_DRAFT"
            | "PRICE_NOT_SCHEDULED"
            | "PRICE_CHANGE_PENDING"
            | "PRICE_BOUND"
            | "PRICE_ALREADY_ENDED" => conflict(code).into(),
            "REGISTRY_UNAVAILABLE" => unavailable().into(),
            "PRICE_NOT_FOUND" => missing_what("price").into(),
            "REVISION_NOT_FOUND" => missing_what("plan_revision").into(),
            "ENTRY_NOT_FOUND" => missing_entry().into(),
            _ => invalid(&field, code).into(),
        },
        A::ApplyRefused { code, detail } => match code {
            "REGISTRY_UNAVAILABLE" => unavailable().into(),
            // D-520: the cancel's own race, a price that started between submit and apply, keeps
            // its code; every other apply refusal is APPLY_REFUSED naming its cause.
            "PRICE_ALREADY_STARTED" => conflict_because(code, detail).into(),
            _ => PricingResource::aborted(format!("{code}: {detail}"))
                .with_reason("APPLY_REFUSED")
                .create()
                .into(),
        },
        A::SodViolation | A::NotSubmitter => PricingResource::permission_denied()
            .with_reason(error.code())
            .create()
            .into(),
        A::NoteRequired => invalid("note", "NOTE_REQUIRED").into(),
        A::NoteTooLong => invalid_because(
            "note",
            "NOTE_TOO_LONG",
            &format!(
                "a note is at most {} characters",
                bss_approval::NOTE_MAX_CHARS
            ),
        )
        .into(),
        A::UnitNotFound { .. } => missing_what("approval_unit").into(),
        A::Empty => invalid("price_ids", "NO_DRAFT_PRICES").into(),
        A::GenerationMismatch { current, .. } => DoorError::Generation { current },
        // The shared engine names its lock conflict for every gear (`ROW_LOCKED_PENDING`);
        // where a pending unit holds a Price, the door says so.
        A::Locked { item_type, .. } => conflict(if item_type == "price" {
            "PRICE_LOCKED_PENDING"
        } else {
            "ROW_LOCKED_PENDING"
        })
        .into(),
        A::AlreadyDecided | A::DuplicateVote | A::Contended => conflict(error.code()).into(),
        A::Store(detail) => {
            tracing::error!(detail, "pricing approval store failure");
            CanonicalError::internal("pricing approval store failure")
                .create()
                .into()
        }
    }
}
/// A revision whose checks are red cannot be submitted (400 `REVISION_CHECKS_RED`, D-403): the
/// problem's detail is the red checks as the checks door renders them (code, label, detail,
/// `blocked_by`), and each red check is also a field violation under `checks.<code>`.
#[must_use]
pub fn checks_red(red: &[crate::domain::plan::Check]) -> CanonicalError {
    let red: Vec<super::dto::PricingPlanCheckDto> = red.iter().cloned().map(Into::into).collect();
    let mut builder = PricingResource::invalid_argument().with_field_violation(
        "checks",
        format!("{} check(s) are red", red.len()),
        "REVISION_CHECKS_RED",
    );
    for check in &red {
        let mut description = format!("{}: {}", check.label, check.detail);
        if !check.blocked_by.is_empty() {
            let units: Vec<String> = check.blocked_by.iter().map(Uuid::to_string).collect();
            description = format!("{description} (blocked by {})", units.join(", "));
        }
        builder = builder.with_field_violation(
            format!("checks.{}", check.code),
            description,
            check.code.clone(),
        );
    }
    let refusal = builder.create();
    let Ok(detail) = serde_json::to_string(&red) else {
        return refusal;
    };
    let mut problem = toolkit::api::canonical_prelude::Problem::from(refusal.clone());
    problem.detail = detail;
    CanonicalError::try_from(problem).unwrap_or(refusal)
}
/// A Products call that did not answer (a 5xx, a timeout, a rate limit): its own status and
/// diagnostic reach the log, since the caller sees only 503 `REGISTRY_UNAVAILABLE` (PS-32).
pub fn registry_unavailable(error: &CanonicalError) -> CanonicalError {
    tracing::warn!(
        status = error.status_code(),
        error = %error,
        diagnostic = error.diagnostic().unwrap_or_default(),
        "pricing: a Products registry call did not answer"
    );
    unavailable()
}
/// The Products registry is not reachable from this process.
pub fn unavailable() -> CanonicalError {
    CanonicalError::service_unavailable()
        .with_detail("REGISTRY_UNAVAILABLE: Products reference registry is unavailable")
        .create()
}
/// A 400 problem that names the unit's current generation for the reviewer's client.
#[must_use]
pub fn generation_problem(code: &str, generation: i32) -> toolkit::api::canonical_prelude::Problem {
    let mut problem = toolkit::api::canonical_prelude::Problem::from(invalid("generation", code));
    problem.context["generation"] = serde_json::json!(generation);
    problem
}
impl From<toolkit_db::DbError> for DoorError {
    fn from(e: toolkit_db::DbError) -> Self {
        Self::Repo(e.into())
    }
}
impl From<DoorError> for CanonicalError {
    fn from(e: DoorError) -> Self {
        match e {
            DoorError::Api(e) => e,
            DoorError::SelectionMoved => conflict(UNIT_CONTENDED),
            DoorError::Generation { .. } => invalid("generation", "GENERATION_MISMATCH"),
            DoorError::Repo(RepoError::Conflict { code }) => conflict(code),
            DoorError::Repo(e) => {
                tracing::error!(error=%e,"pricing storage failure");
                Self::internal("pricing storage failure").create()
            }
        }
    }
}
/// A mutation door's refusal when its transaction still meets retryable contention after
/// the toolkit's retries: a lost race the client may retry, never a 500.
pub const CONTENDED: &str = "CONTENDED";
/// The same refusal at an approval-unit door (submit, publish-changes, approve, reject,
/// withdraw), where the design names it `UNIT_CONTENDED` (D-403).
pub const UNIT_CONTENDED: &str = "UNIT_CONTENDED";
pub async fn transaction<T: Send + 'static>(
    db: &Db,
    work: impl for<'a> FnMut(
        &'a DbTx<'a>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, DoorError>> + Send + 'a>,
    > + Send,
) -> Result<T, CanonicalError> {
    transaction_door(db, work).await.map_err(Into::into)
}
/// The serializable retrying transaction, keeping the door's typed refusal.
/// # Errors
/// Returns the last attempt's refusal or storage failure; contention the retries could not
/// clear is `CONTENDED`.
pub async fn transaction_door<T: Send + 'static>(
    db: &Db,
    work: impl for<'a> FnMut(
        &'a DbTx<'a>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, DoorError>> + Send + 'a>,
    > + Send,
) -> Result<T, DoorError> {
    transaction_coded(db, CONTENDED, work).await
}
/// Run `work` serializably with the toolkit's contention retries. A driver error the retry
/// classifier still calls contention after the last attempt becomes 409 `code`.
async fn transaction_coded<T: Send + 'static>(
    db: &Db,
    code: &'static str,
    work: impl for<'a> FnMut(
        &'a DbTx<'a>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, DoorError>> + Send + 'a>,
    > + Send,
) -> Result<T, DoorError> {
    db.transaction_with_retry(
        toolkit_db::secure::TxConfig::serializable(),
        driver_source,
        work,
    )
    .await
    .map_err(|error| exhausted_contention(db.backend(), code, error))
}
/// The driver error the retry classifier judges, if the door's error carries one.
fn driver_source(error: &DoorError) -> Option<&sea_orm::DbErr> {
    match error {
        DoorError::Repo(RepoError::Driver { source, .. }) => Some(source),
        _ => None,
    }
}
/// [`transaction`] for work that enqueues events: `work` takes the attempt's [`TxOutbox`] over
/// `sink`, and the outbox's sequencers wake only once the transaction has committed (D-455).
/// # Errors
/// Returns the last attempt's refusal or storage failure; exhausted contention is `CONTENDED`.
pub async fn transaction_with_events<T: Send + 'static>(
    db: &Db,
    sink: &EventSink,
    work: impl for<'a> FnMut(
        &'a DbTx<'a>,
        TxOutbox,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, DoorError>> + Send + 'a>,
    > + Send,
) -> Result<T, CanonicalError> {
    events_coded(db, sink, CONTENDED, work)
        .await
        .map_err(Into::into)
}
/// [`transaction_with_events`] keeping the typed error, for a caller that classifies it before it
/// is rendered (the reference work's commit, PS-42).
/// # Errors
/// Returns the last attempt's refusal or storage failure; exhausted contention is `CONTENDED`.
pub async fn transaction_door_with_events<T: Send + 'static>(
    db: &Db,
    sink: &EventSink,
    work: impl for<'a> FnMut(
        &'a DbTx<'a>,
        TxOutbox,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, DoorError>> + Send + 'a>,
    > + Send,
) -> Result<T, DoorError> {
    events_coded(db, sink, CONTENDED, work).await
}
/// Retry the entire detached capture and single transaction against the existing attempt budget.
/// Only local selection drift and driver contention are retryable; provider refusals are not.
/// # Errors
/// The original refusal, or `UNIT_CONTENDED` when the shared attempt budget is exhausted.
pub async fn retry_unit_capture<T, F, Fut>(db: &Db, mut attempt: F) -> Result<T, DoorError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, DoorError>>,
{
    let mut last = None;
    for n in 1..=toolkit_db::DEFAULT_TX_RETRY_ATTEMPTS {
        match attempt().await {
            Err(error @ DoorError::SelectionMoved) => last = Some(error),
            Err(error)
                if driver_source(&error).is_some_and(|source| {
                    toolkit_db::contention::is_retryable_contention(db.backend(), source)
                }) =>
            {
                last = Some(error);
            }
            other => return other,
        }
        if n < toolkit_db::DEFAULT_TX_RETRY_ATTEMPTS {
            tokio::time::sleep(retry_backoff_delay(n + 1)).await;
        }
    }
    if let Some(error) = last.as_ref() {
        tracing::warn!(
            error = %error,
            attempts = toolkit_db::DEFAULT_TX_RETRY_ATTEMPTS,
            "detached capture retry budget exhausted"
        );
    }
    Err(conflict(UNIT_CONTENDED).into())
}
/// The same stagger toolkit-db uses before a contended transaction retry: a few milliseconds,
/// grown per attempt and jittered, so two capturers do not restart in lockstep.
fn retry_backoff_delay(next_attempt: u32) -> std::time::Duration {
    use std::time::Duration;
    use tokio_retry::strategy::{ExponentialBackoff, jitter};
    let index = usize::try_from(next_attempt.saturating_sub(2)).unwrap_or(0);
    let base = ExponentialBackoff::from_millis(2)
        .factor(5)
        .max_delay(Duration::from_millis(100))
        .nth(index)
        .unwrap_or(Duration::from_millis(100));
    jitter(base)
}

/// One event-bearing transaction within [`retry_unit_capture`]. The outer loop owns the entire
/// budget so database retries cannot reuse a stale capture or multiply the attempt limit.
/// # Errors
/// The original typed refusal or driver error, after rollback and discarding event wakes.
pub async fn unit_transaction_observed_with_events<T: Send + 'static>(
    db: &Db,
    sink: &EventSink,
    work: impl for<'a> FnMut(
        &'a DbTx<'a>,
        TxOutbox,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, DoorError>> + Send + 'a>,
    > + Send,
) -> Result<T, DoorError> {
    events::transaction_with_attempts(
        db,
        sink,
        toolkit_db::secure::TxConfig::serializable(),
        1,
        driver_source,
        work,
    )
    .await
}

/// [`transaction_coded`] through [`events::transaction`]: the same isolation, retries and
/// exhausted-contention code, with the attempt's [`TxOutbox`] fired after the commit.
async fn events_coded<T: Send + 'static>(
    db: &Db,
    sink: &EventSink,
    code: &'static str,
    work: impl for<'a> FnMut(
        &'a DbTx<'a>,
        TxOutbox,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, DoorError>> + Send + 'a>,
    > + Send,
) -> Result<T, DoorError> {
    events::transaction(
        db,
        sink,
        toolkit_db::secure::TxConfig::serializable(),
        driver_source,
        work,
    )
    .await
    .map_err(|error| exhausted_contention(db.backend(), code, error))
}
/// Classify a finished transaction's error: retryable contention is the door's 409 `code`, kept
/// as a typed conflict so each door names its own resource (phase 4 second review B-1): an
/// authoring door renders it through `From<DoorError>` as [`conflict`], a read door as its own
/// resource's conflict (`read_contract::read_failure`).
#[must_use]
pub fn exhausted_contention(
    backend: sea_orm::DbBackend,
    code: &'static str,
    error: DoorError,
) -> DoorError {
    match &error {
        DoorError::Repo(RepoError::Driver { source, .. })
            if toolkit_db::contention::is_retryable_contention(backend, source) =>
        {
            tracing::warn!(error=%error, code, "pricing transaction contention outlasted its retries");
            DoorError::Repo(RepoError::Conflict { code })
        }
        _ => error,
    }
}
/// A door's answer with its status and `ETag`. An error status is an RFC 9457 problem, so its
/// body is served `application/problem+json` and the canonical error middleware completes and
/// logs it (PS-07): the committed `UNIT_STALE` and its replay are the doors' only such answers.
/// # Errors
/// An `ETag` that is not a header value.
pub fn response<T: serde::Serialize>(
    status: StatusCode,
    body: &T,
    version: Option<u64>,
) -> Result<Response, CanonicalError> {
    let mut headers = HeaderMap::new();
    if let Some(version) = version {
        headers.insert(
            "etag",
            format!("\"{version}\"")
                .parse()
                .map_err(|_| CanonicalError::internal("invalid ETag").create())?,
        );
    }
    let mut response = (status, headers, Json(body)).into_response();
    if status.is_client_error() || status.is_server_error() {
        response.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/problem+json"),
        );
    }
    Ok(response)
}
pub fn value<T: serde::Serialize>(body: &T) -> Result<serde_json::Value, CanonicalError> {
    serde_json::to_value(body)
        .map_err(|_| CanonicalError::internal("pricing serialization failed").create())
}
pub fn date(raw: Option<String>, field: &str) -> Result<Option<time::Date>, CanonicalError> {
    raw.map(|s| {
        time::Date::parse(
            &s,
            &time::macros::format_description!("[year]-[month]-[day]"),
        )
        .map_err(|_| invalid(field, "DATE_INVALID"))
    })
    .transpose()
}
/// A command POST that carries no fields: an empty body or `{}`.
/// # Errors
/// Any other body is refused with `BODY_UNEXPECTED`.
pub fn empty_body(body: &[u8]) -> Result<serde_json::Value, CanonicalError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(serde_json::json!({}));
    }
    let value: serde_json::Value =
        crate::api::rest::preconditions::parse_body(body).map_err(CanonicalError::from)?;
    if value.as_object().is_some_and(serde_json::Map::is_empty) {
        Ok(value)
    } else {
        Err(invalid("body", "BODY_UNEXPECTED"))
    }
}
/// The body of a plan revision's submit, which takes only the submitter's note (D-464, the rule
/// of products P-D-219): no body, `{}` and `{"note": null}` carry no note, `{"note": "…"}` carries
/// it; any other key is 400 `BODY_UNEXPECTED`, the rule of [`empty_body`] it replaces, and a note
/// that is neither text nor null is 400 as an unreadable body. Once its keys are judged, the body
/// is read into the served request DTO, `PricingPlanRevisionSubmitRequest`, so the schema and the
/// parser are one definition (the phase 9 review's R18). Answers the payload the key's digest
/// covers (the body as sent, `{}` for none) and the note. Its cap is the door's to judge.
/// # Errors
/// The refusals above; an unreadable body.
pub fn note_body(body: &[u8]) -> Result<(serde_json::Value, Option<String>), CanonicalError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok((serde_json::json!({}), None));
    }
    let value: serde_json::Value =
        crate::api::rest::preconditions::parse_body(body).map_err(CanonicalError::from)?;
    if !value
        .as_object()
        .is_some_and(|fields| fields.keys().all(|key| key == "note"))
    {
        return Err(invalid("body", "BODY_UNEXPECTED"));
    }
    let request: super::dto::PricingPlanRevisionSubmitRequest =
        serde_json::from_value(value.clone()).map_err(|_| {
            crate::infra::error_mapping::DomainError::InvalidRequest(
                "the request body is not readable: note is text or null".to_owned(),
            )
        })?;
    Ok((value, request.note))
}
/// The If-Match token must name the stored version: a stale one is 409 `STALE_REVISION`
/// before anything is written (the code products answers for the same refusal).
///
/// @cpt-dod:cpt-cf-bss-pricing-dod-if-match-version:p1
pub fn check_version(seen: u64, actual: i64) -> Result<(), CanonicalError> {
    if u64::try_from(actual).ok() == Some(seen) {
        Ok(())
    } else {
        Err(conflict("STALE_REVISION"))
    }
}
pub async fn audit(
    tx: &impl DBRunner,
    ctx: &SecurityContext,
    correlation: Uuid,
    action: &str,
    id: Uuid,
    version: i64,
) -> Result<(), DoorError> {
    repo::audit_repo::write_eventless_act_audit(
        tx,
        &AccessScope::for_tenant(ctx.subject_tenant_id()),
        repo::audit_repo::AuditCommon {
            audit_id: Uuid::now_v7(),
            tenant_id: ctx.subject_tenant_id(),
            actor_ref: ctx.subject_id(),
            action: action.into(),
            subject_kind: "pricing".into(),
            reason: None,
            correlation_id: Some(correlation.to_string()),
            written_at: crate::infra::storage::stored_now(),
        },
        id,
        Some(version),
    )
    .await?;
    Ok(())
}
/// The `ETag` a read answers: the version, or the content tag, a following write sends back as
/// If-Match. Declared on each read that sets one.
#[must_use]
pub fn etag() -> toolkit::api::operation_builder::ResponseHeaderSpec {
    use toolkit::api::operation_builder::{ResponseHeaderSpec, ResponseHeaderType};
    ResponseHeaderSpec::new(
        "ETag",
        "The version to send back as If-Match",
        ResponseHeaderType::String,
    )
}
pub fn header(name: &str) -> toolkit::api::operation_builder::ParamSpec {
    toolkit::api::operation_builder::ParamSpec::header(name)
        .required(true)
        .description("Required authoring precondition")
}
/// `If-None-Match` on a list read (D-518). A match is 304; the header is optional.
#[must_use]
pub fn if_none_match() -> toolkit::api::operation_builder::ParamSpec {
    toolkit::api::operation_builder::ParamSpec::header("If-None-Match")
        .required(false)
        .description("A weak ETag from an earlier read of this answer, or *. A match is 304.")
}
/// The weak `ETag` of the JSON body (D-518). It is not the version a write sends as `If-Match`.
#[must_use]
pub fn weak_etag_header() -> toolkit::api::operation_builder::ResponseHeaderSpec {
    use toolkit::api::operation_builder::{ResponseHeaderSpec, ResponseHeaderType};
    ResponseHeaderSpec::new(
        "ETag",
        "Weak tag of this JSON body",
        ResponseHeaderType::String,
    )
}
/// `Cache-Control: private, no-cache` (D-518): the browser stores the answer and must revalidate it.
#[must_use]
pub fn revalidate_header() -> toolkit::api::operation_builder::ResponseHeaderSpec {
    use toolkit::api::operation_builder::{ResponseHeaderSpec, ResponseHeaderType};
    ResponseHeaderSpec::new(
        "Cache-Control",
        "private, no-cache",
        ResponseHeaderType::String,
    )
}
/// `If-None-Match` on a single document that keeps its strong version tag (D-518): the tag a
/// `PUT` sends back as `If-Match`, compared here by weak comparison.
#[must_use]
pub fn if_none_match_version() -> toolkit::api::operation_builder::ParamSpec {
    toolkit::api::operation_builder::ParamSpec::header("If-None-Match")
        .required(false)
        .description("The ETag of an earlier read, or *. A match is 304.")
}
/// D-518: a single document revalidates on its strong version tag. `answer` is the read's 200
/// with that `ETag`. It gains `Cache-Control: private, no-cache`, and it becomes a 304 with an
/// empty body, the same `ETag` and that `Cache-Control` when an `If-None-Match` of `request`
/// matches the tag by weak comparison. Any other answer passes through untouched.
#[must_use]
pub fn revalidate_version(request: &HeaderMap, mut answer: Response) -> Response {
    use axum::http::{HeaderValue, header};
    use bss_rest::conditional_get::{PRIVATE_REVALIDATE, matches_if_none_match};
    if answer.status() != StatusCode::OK {
        return answer;
    }
    let cache = HeaderValue::from_static(PRIVATE_REVALIDATE.cache_control);
    answer
        .headers_mut()
        .insert(header::CACHE_CONTROL, cache.clone());
    let Some(tag) = answer.headers().get(header::ETAG).cloned() else {
        return answer;
    };
    let matched = request
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .any(|candidate| matches_if_none_match(Some(candidate), &tag));
    if !matched {
        return answer;
    }
    let mut not_modified = Response::new(axum::body::Body::empty());
    *not_modified.status_mut() = StatusCode::NOT_MODIFIED;
    let headers = not_modified.headers_mut();
    headers.insert(header::ETAG, tag);
    headers.insert(header::CACHE_CONTROL, cache);
    not_modified
}
