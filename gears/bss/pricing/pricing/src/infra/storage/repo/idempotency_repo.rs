//! Scoped idempotency repo; follows the Products implementation.
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-idempotency-key-store:p1
use super::driver_failure;
use crate::infra::storage::{RepoError, entity::idempotency};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ColumnTrait, Condition, DbErr, EntityTrait, Set};
use serde_json::Value as JsonValue;
use time::OffsetDateTime;
use toolkit_db::secure::{
    AccessScope, DBRunner, ScopeError, SecureDeleteExt, SecureEntityExt, SecureInsertExt,
    SecureUpdateExt,
};
use uuid::Uuid;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdempotencyClaim {
    /// No live row existed for the key, or the held row was expired and this
    /// call took it over. The caller now holds the key and proceeds with the
    /// guarded mutation.
    Claimed,
    /// A row exists in `answered` state and is live. `payload_hash` is the
    /// hash the answer was recorded against — the caller compares it with
    /// its own to tell an identical replay from `IDEMPOTENCY_CONFLICT`
    /// (`inst-fd-idem-conflict`), a comparison this repository does not make
    /// because it was never handed the incoming request to compare.
    /// `response_status`/`response_body` are the replay itself, and it is
    /// self-contained (D-429): nothing else needs to be read to serve
    /// it.
    Answered {
        /// The digest the stored answer was recorded against.
        payload_hash: Vec<u8>,
        /// The status the original caller was told.
        response_status: i32,
        /// The body the original caller was told.
        response_body: JsonValue,
    },
    /// A live, unexpired `claimed` row already holds the key. The caller
    /// refuses `IDEMPOTENCY_KEY_IN_FLIGHT` **when the payloads agree** and
    /// `IDEMPOTENCY_CONFLICT` when they do not: a payload mismatch "stays
    /// `IDEMPOTENCY_CONFLICT` in either state"
    /// (`design/01-foundation.md` §3.2 `inst-fd-idem-claim-inflight`,
    /// `inst-fd-idem-conflict`), so the in-flight refusal is reserved for
    /// the duplicate that is genuinely the same request. This repository
    /// writes nothing to the row on either reading — the comparison is the
    /// caller's, for [`Answered`](Self::Answered)'s reason: this layer was
    /// never handed the incoming request.
    InFlight {
        /// The digest the live `claimed` row was recorded against.
        payload_hash: Vec<u8>,
    },
    /// This call lost the expired-key takeover race (D-429): another
    /// caller's compare-and-swap moved the row off the stamp this one read.
    ///
    /// Distinct from [`InFlight`](Self::InFlight) because **no digest
    /// comparison is owed here and none is possible**. The loser "may even
    /// carry a different payload from the winner, and is still refused
    /// in-flight rather than for the mismatch, since its transaction never
    /// compared the two" (D-429): the
    /// row this call read was the *expired* holder's, and the payload now
    /// under the key is the winner's, which this transaction never saw.
    /// Answering `IDEMPOTENCY_CONFLICT` from a hash this call never read
    /// would be a fabricated verdict. The caller refuses
    /// `IDEMPOTENCY_KEY_IN_FLIGHT`, having executed nothing.
    TakeoverRaceLost,
}

fn idempotency_key_of(tenant_id: Uuid, endpoint: &str, client_key: &str) -> Condition {
    Condition::all()
        .add(idempotency::Column::TenantId.eq(tenant_id))
        .add(idempotency::Column::Endpoint.eq(endpoint))
        .add(idempotency::Column::ClientKey.eq(client_key))
}

#[expect(
    clippy::too_many_arguments,
    reason = "the composite key is three columns and the claim needs the digest, the \
              instant and the fresh expiry beside them, matching the sibling pricing \
              gear's identically-shaped `IdempotencyGate::claim`; bundling them into a \
              parameter struct would hide the key's own shape behind a type nothing \
              else uses"
)]
/// Claim idempotency key.
/// # Errors
/// Returns scoped storage failures, preserving database errors for retry.
pub async fn claim_idempotency_key(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    endpoint: &str,
    client_key: &str,
    payload_hash: &[u8],
    now: OffsetDateTime,
    expires_at: OffsetDateTime,
) -> Result<IdempotencyClaim, RepoError> {
    let model = idempotency::ActiveModel {
        tenant_id: Set(tenant_id),
        endpoint: Set(endpoint.to_owned()),
        client_key: Set(client_key.to_owned()),
        state: Set("claimed".to_owned()),
        payload_hash: Set(payload_hash.to_vec()),
        response_status: Set(None),
        response_body: Set(None),
        expires_at: Set(expires_at),
        // A fresh claim is bound to no op; a door that begins a durable op
        // binds it afterwards, in this same transaction ([`bind_op`]).
        entity_ref: Set(None),
    };

    let on_conflict = OnConflict::columns([
        idempotency::Column::TenantId,
        idempotency::Column::Endpoint,
        idempotency::Column::ClientKey,
    ])
    .do_nothing()
    .to_owned();

    match idempotency::Entity::insert(model.clone())
        .secure()
        .scope_with_model(scope, &model)
        .map_err(|e| {
            driver_failure(
                format!("idempotency claim {tenant_id}/{endpoint}/{client_key} scope"),
                e,
            )
        })?
        .on_conflict_raw(on_conflict)
        .exec(runner)
        .await
    {
        Ok(_) => return Ok(IdempotencyClaim::Claimed),
        // The key is already held; the conflict swallowed the insert.
        Err(ScopeError::Db(DbErr::RecordNotInserted)) => {}
        Err(e) => {
            return Err(driver_failure(
                format!("idempotency claim {tenant_id}/{endpoint}/{client_key}"),
                e,
            ));
        }
    }

    let held = idempotency::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(idempotency_key_of(tenant_id, endpoint, client_key))
        .one(runner)
        .await
        .map_err(|e| {
            driver_failure(
                format!("read held idempotency claim {tenant_id}/{endpoint}/{client_key}"),
                e,
            )
        })?
        .ok_or_else(|| {
            RepoError::Db(format!(
                "idempotency claim {tenant_id}/{endpoint}/{client_key} conflicted but is \
                 not readable in the same transaction"
            ))
        })?;

    // A claim bound to its durable op is never taken over, however old: the op may still be retrying,
    // and a takeover would mint a second op under the same key (D-401, D-429).
    if now > held.expires_at && !(held.state == "claimed" && held.entity_ref.is_some()) {
        return take_over_expired_idempotency_claim(runner, scope, &held, payload_hash, expires_at)
            .await;
    }

    held_claim(held)
}

/// Lookup idempotency key.
/// # Errors
/// Returns scoped storage failures, preserving database errors for retry.
pub async fn lookup_idempotency_key(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    endpoint: &str,
    client_key: &str,
    now: OffsetDateTime,
) -> Result<Option<IdempotencyClaim>, RepoError> {
    let held = idempotency::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(idempotency_key_of(tenant_id, endpoint, client_key))
        .filter(idempotency::Column::ExpiresAt.gte(now).into())
        .one(runner)
        .await
        .map_err(|e| driver_failure("lookup idempotency".into(), e))?;
    held.map(held_claim).transpose()
}

fn held_claim(held: idempotency::Model) -> Result<IdempotencyClaim, RepoError> {
    match held.state.as_str() {
        "answered" => {
            let (Some(response_status), Some(response_body)) =
                (held.response_status, held.response_body)
            else {
                return Err(RepoError::CorruptRow(
                    "pricing_idempotency stored key answered with an incomplete response".into(),
                ));
            };
            Ok(IdempotencyClaim::Answered {
                payload_hash: held.payload_hash,
                response_status,
                response_body,
            })
        }
        "claimed" => Ok(IdempotencyClaim::InFlight {
            payload_hash: held.payload_hash,
        }),
        other => Err(RepoError::CorruptRow(format!(
            "pricing_idempotency.state `{other}` on stored key"
        ))),
    }
}

async fn take_over_expired_idempotency_claim(
    runner: &impl DBRunner,
    scope: &AccessScope,
    held: &idempotency::Model,
    payload_hash: &[u8],
    new_expires_at: OffsetDateTime,
) -> Result<IdempotencyClaim, RepoError> {
    let result = idempotency::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(
            idempotency::Column::State,
            Expr::value("claimed".to_owned()),
        )
        .col_expr(
            idempotency::Column::PayloadHash,
            Expr::value(payload_hash.to_vec()),
        )
        .col_expr(
            idempotency::Column::ResponseStatus,
            Expr::value(None::<i32>),
        )
        .col_expr(
            idempotency::Column::ResponseBody,
            Expr::value(None::<JsonValue>),
        )
        .col_expr(idempotency::Column::ExpiresAt, Expr::value(new_expires_at))
        // A taken-over claim is a fresh act's: the expired holder's op binding
        // (an answered row may still carry one) does not carry over.
        .col_expr(idempotency::Column::EntityRef, Expr::value(None::<Uuid>))
        .filter(
            idempotency_key_of(held.tenant_id, &held.endpoint, &held.client_key)
                .add(idempotency::Column::ExpiresAt.eq(held.expires_at)),
        )
        .exec(runner)
        .await
        .map_err(|e| {
            driver_failure(
                format!(
                    "take over expired idempotency claim {}/{}/{}",
                    held.tenant_id, held.endpoint, held.client_key
                ),
                e,
            )
        })?;

    // Zero rows means the takeover race above was lost: another caller's
    // `UPDATE` already moved `expires_at` off the stamp this call read, so
    // the `WHERE` clause matches nothing left. Reporting that as `Claimed`
    // is exactly the defect this compare-and-swap exists to prevent — it
    // would tell two callers they both hold a key only one of them does.
    // It is not `InFlight` either: that outcome carries the held digest for
    // the caller to compare, and the digest now under the key is the
    // winner's, which this transaction never read (D-429).
    if result.rows_affected == 0 {
        return Ok(IdempotencyClaim::TakeoverRaceLost);
    }
    Ok(IdempotencyClaim::Claimed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdempotencyAnswer {
    /// The row was `claimed` and is now `answered`, carrying both response
    /// columns.
    Recorded,
    /// No `claimed` row matched the key: it was never claimed, it was
    /// already answered, or it was taken over by another caller. **Nothing
    /// was written**, and the caller decides what that means for it.
    NotHeld,
}

/// Answer idempotency key.
/// # Errors
/// Returns scoped storage failures, preserving database errors for retry.
#[expect(
    clippy::too_many_arguments,
    reason = "the composite key is three columns and the answer needs the status, the body and \
              the late answer's retention beside them, like the claim above"
)]
pub async fn answer_idempotency_key(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    endpoint: &str,
    client_key: &str,
    response_status: i32,
    response_body: JsonValue,
    retain_until: Option<OffsetDateTime>,
) -> Result<IdempotencyAnswer, RepoError> {
    let mut update = idempotency::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(
            idempotency::Column::State,
            Expr::value("answered".to_owned()),
        );
    // A durable op may answer long after its claim; its answer is kept a full retention from the
    // answer, or the next same-key retry would find the row expired and take the key over (D-429).
    if let Some(retain_until) = retain_until {
        update = update.col_expr(idempotency::Column::ExpiresAt, Expr::value(retain_until));
    }
    let result = update
        .col_expr(
            idempotency::Column::ResponseStatus,
            Expr::value(Some(response_status)),
        )
        .col_expr(
            idempotency::Column::ResponseBody,
            Expr::value(Some(response_body)),
        )
        .filter(
            idempotency_key_of(tenant_id, endpoint, client_key)
                .add(idempotency::Column::State.eq("claimed")),
        )
        .exec(runner)
        .await
        .map_err(|e| {
            driver_failure(
                format!("answer idempotency claim {tenant_id}/{endpoint}/{client_key}"),
                e,
            )
        })?;

    // Zero rows is a real answer, not a no-op to shrug at: no `claimed` row
    // matched, so the response this call was handed was never recorded and
    // the caller must not proceed as though it had been.
    if result.rows_affected == 0 {
        return Ok(IdempotencyAnswer::NotHeld);
    }
    Ok(IdempotencyAnswer::Recorded)
}

/// Release idempotency claim.
/// # Errors
/// Returns scoped storage failures, preserving database errors for retry.
pub async fn release_idempotency_claim(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    endpoint: &str,
    client_key: &str,
) -> Result<u64, RepoError> {
    let result = idempotency::Entity::delete_many()
        .secure()
        .scope_with(scope)
        .filter(
            idempotency_key_of(tenant_id, endpoint, client_key)
                .add(idempotency::Column::State.eq("claimed")),
        )
        .exec(runner)
        .await
        .map_err(|e| {
            driver_failure(
                format!("release idempotency claim {tenant_id}/{endpoint}/{client_key}"),
                e,
            )
        })?;
    Ok(result.rows_affected)
}

#[cfg(test)]
#[path = "idempotency_repo_tests.rs"]
mod tests;

/// Bind an unanswered claim to its durable op so expiry cannot strand in-flight work.
/// # Errors
/// Returns a storage error or a lost claim conflict.
pub async fn bind_op(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    endpoint: &str,
    key: &str,
    op_id: Uuid,
) -> Result<(), RepoError> {
    let result = idempotency::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(idempotency::Column::EntityRef, Expr::value(op_id))
        .filter(
            idempotency_key_of(tenant, endpoint, key).add(idempotency::Column::State.eq("claimed")),
        )
        .exec(runner)
        .await
        .map_err(|e| driver_failure("bind reference op claim".into(), e))?;
    super::matched(result.rows_affected, "IDEMPOTENCY_KEY_IN_FLIGHT")
}
