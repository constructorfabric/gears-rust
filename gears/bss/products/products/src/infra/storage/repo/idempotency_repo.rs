//! Restored atomic claim, answer, expiry takeover and release.
use super::driver_failure;
use crate::infra::storage::{RepoError, entity::idempotency};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ColumnTrait, Condition, DbErr, EntityTrait, Set};
use serde_json::Value as JsonValue;
use time::OffsetDateTime;
use toolkit_db::secure::{
    AccessScope, DBRunner, ScopeError, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use uuid::Uuid;
/// What [`claim_idempotency_key`] found `(tenant_id, endpoint, client_key)`
/// in, shaped so the three outcomes the caller must tell apart cannot be
/// conflated into one error type (`design/01-foundation.md` §3.2,
/// `dod-idempotency-store`).
///
/// A returned enum rather than an error for the non-error cases: only
/// [`InFlight`](Self::InFlight) is a refusal the caller executes nothing
/// under, and even that refusal is the door's to raise as
/// `IDEMPOTENCY_KEY_IN_FLIGHT` — this repository raises no `DomainError` of
/// its own, since the existing taxonomy already carries the two idempotency
/// codes this slice's outcomes map to.
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
    /// self-contained (P-D-198): nothing else needs to be read to serve
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
    /// This call lost the expired-key takeover race (P-D-198): another
    /// caller's compare-and-swap moved the row off the stamp this one read.
    ///
    /// Distinct from [`InFlight`](Self::InFlight) because **no digest
    /// comparison is owed here and none is possible**. The loser "may even
    /// carry a different payload from the winner, and is still refused
    /// in-flight rather than for the mismatch, since its transaction never
    /// compared the two" (P-D-198): the
    /// row this call read was the *expired* holder's, and the payload now
    /// under the key is the winner's, which this transaction never saw.
    /// Answering `IDEMPOTENCY_CONFLICT` from a hash this call never read
    /// would be a fabricated verdict. The caller refuses
    /// `IDEMPOTENCY_KEY_IN_FLIGHT`, having executed nothing.
    TakeoverRaceLost,
}

/// The composite primary key of `products_idempotency`, as a filter. One
/// spelling, so no statement in this module can address a row by fewer axes
/// than the key actually has.
fn idempotency_key_of(tenant_id: Uuid, endpoint: &str, client_key: &str) -> Condition {
    Condition::all()
        .add(idempotency::Column::TenantId.eq(tenant_id))
        .add(idempotency::Column::Endpoint.eq(endpoint))
        .add(idempotency::Column::ClientKey.eq(client_key))
}

/// Claim `(tenant_id, endpoint, client_key)` for `payload_hash`, or report
/// why it could not be claimed (`dod-idempotency-store`; P-D-198).
///
/// # This function opens no transaction — `runner` MUST already be the
/// # guarded mutation's own
///
/// **The claim `INSERT` is the gate, not a lookup** (P-D-198): this
/// function writes the claim row with an `INSERT ... ON CONFLICT DO NOTHING`
/// and reads back only when the conflict actually fired, so between the
/// attempt and the read nothing is ever left free for a second caller to
/// also see as available. There is no separate reservation step and no
/// `in_flight_until` column — the primary key is the whole of the gate. For
/// that gate to do its job, `runner` MUST be the caller's own guarded
/// mutation's transaction: joining it is what makes a rollback free the key
/// automatically, with no release step of its own, and it is a stricter
/// obligation than `resolve_actor_ref`'s, which asks for a transaction of
/// its own instead. A `runner` that is not the mutation's transaction breaks
/// the one property this whole mechanism exists to provide.
///
/// `now` and `expires_at` are both caller-supplied, matching
/// `resolve_actor_ref`'s own discipline: this function reasons about no
/// clock but the one its caller hands it.
///
/// # The expired-key takeover is a compare-and-swap on `expires_at` itself
/// # (P-D-198)
///
/// Nothing holds an expired row between this function's own conflict check
/// and its takeover `UPDATE`, so two duplicates racing on one expired key can
/// both clear the check and both read the same expired row. Without a
/// predicate both would be told they claimed it and the guarded mutation
/// would run twice under one key — precisely the failure this mechanism
/// exists to prevent. The takeover `UPDATE` therefore carries `WHERE
/// expires_at = <the value this call read>`; exactly one racer's `UPDATE`
/// matches, and **a zero-row result is the lost race, not success** — it is
/// answered [`IdempotencyClaim::TakeoverRaceLost`], never
/// [`IdempotencyClaim::Claimed`], and the loser may even carry a different
/// payload from the winner and is still refused in-flight rather than for
/// the mismatch, since this transaction never compared the two — which is
/// why that outcome is a variant of its own and carries no digest for the
/// caller to compare.
///
/// # Errors
/// [`RepoError::Driver`] on a storage failure, or [`RepoError::Db`] on a
/// scope refusal that raised no driver error. The failures include a
/// conflicting row this call cannot read back inside its own transaction —
/// the store contradicting itself, not a foreign tenant's row, since the
/// insert was already validated against the same scope.
/// [`RepoError::CorruptRow`] when the held row's `state` is outside
/// `claimed`/`answered`, or when an `answered` row is missing one of its
/// paired response columns — both refused by
/// `chk_products_idempotency_response_group` at write time, so reaching
/// either here means a row was written around this gear.
#[expect(
    clippy::too_many_arguments,
    reason = "the composite key is three columns and the claim needs the digest, the \
              instant and the fresh expiry beside them, matching the sibling pricing \
              gear's identically-shaped `IdempotencyGate::claim`; bundling them into a \
              parameter struct would hide the key's own shape behind a type nothing \
              else uses"
)]
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
        // `entity_ref` is carried in the DDL (P-D-198) and always NULL: no
        // door stamps it.
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

    if now > held.expires_at {
        return take_over_expired_idempotency_claim(runner, scope, &held, payload_hash, expires_at)
            .await;
    }

    held_claim(held)
}

/// Read a live receipt without acquiring a claim.
/// # Errors
/// Returns scope, database or corrupt-row errors.
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
                    "products_idempotency stored key answered with an incomplete response".into(),
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
            "products_idempotency.state `{other}` on stored key"
        ))),
    }
}

/// Take an expired claim over: `payload_hash`, no response, and a fresh
/// `expires_at`, under a predicate matching the row's own claim stamp as
/// [`claim_idempotency_key`] read it (P-D-198).
///
/// That predicate is the whole of the race protection this function
/// provides: nothing holds an expired row between the caller's conflict
/// check and this `UPDATE`, so two duplicates can both find the row expired
/// and both arrive here carrying the same `held.expires_at`. Without the
/// predicate both `UPDATE` statements would match, both callers would be told they
/// claimed the key, and the guarded mutation would run twice under one key.
/// With it, exactly one `UPDATE` matches; **the other affects zero rows,
/// which this function treats as the lost race, never as success** —
/// [`IdempotencyClaim::TakeoverRaceLost`], the one outcome that carries no
/// digest, because the payload now under the key is the winner's and this
/// transaction never read it.
///
/// # Errors
/// [`RepoError::Driver`] on a storage failure, or [`RepoError::Db`] on a
/// scope refusal that raised no driver error.
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
        // `entity_ref` stays NULL on a taken-over claim, as on a fresh one.
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
    // winner's, which this transaction never read (P-D-198).
    if result.rows_affected == 0 {
        return Ok(IdempotencyClaim::TakeoverRaceLost);
    }
    Ok(IdempotencyClaim::Claimed)
}

/// What [`answer_idempotency_key`] found the key in when it tried to write
/// the answer — an outcome the caller acts on, not an error this repository
/// picks a class for.
///
/// A returned enum, for the reason [`IdempotencyClaim`] is one: this layer
/// cannot tell the two callers of a missed answer apart. A door answering a
/// claim it took on this very runner moments ago meets
/// [`NotHeld`](Self::NotHeld) only if the store contradicts itself, and must
/// fail its mutation over it; a future lane answering a claim taken
/// elsewhere may legitimately find the key expired out from under it and
/// taken over by another caller (P-D-198), which is not a fault at all.
/// Raising a [`RepoError`] here would force the second reading into the
/// first, and a `Result<(), _>` that swallowed the zero-row case would be
/// exactly the silent success the claim's own compare-and-swap exists to
/// prevent.
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

/// Answer a held claim: move `(tenant_id, endpoint, client_key)` from
/// `claimed` to `answered`, recording the status and body the caller is
/// about to return (`design/01-foundation.md` §3.2
/// `inst-fd-idem-claim-write`, P-D-198; `dod-idempotency-store`).
///
/// # This function opens no transaction — `runner` MUST be the same one the
/// # claim and the mutation ran on
///
/// `inst-fd-idem-claim-write` is explicit that claim, mutation and answer
/// **commit together or not at all**. This function therefore takes the
/// caller's runner like every other function in this module, and that runner
/// MUST be the very transaction [`claim_idempotency_key`] and the guarded
/// mutation already ran on. Answering on a runner of its own would reopen
/// the gap P-D-198 closed from the other side: an answer that committed while
/// the mutation rolled back would replay a `201` for an act that never
/// happened, and a mutation that committed while the answer failed would
/// leave the key `claimed` over a committed act — the very defect this
/// function exists to remove.
///
/// # Both response columns are written in one statement
///
/// `chk_products_idempotency_response_group` admits `answered` only with
/// `response_status` **and** `response_body` non-null, so the state and both
/// columns move in a single `UPDATE`; a two-statement version could not
/// exist, since either half alone violates the `CHECK` at write time. The
/// stored pair is the whole of a replay (P-D-198): a bare reference to
/// the created entity could not reproduce the original status, and a refusal
/// has no entity to reference at all.
///
/// # A zero-row result is reported, never swallowed
///
/// The `UPDATE` carries `WHERE state = 'claimed'` beside the key, so it
/// cannot overwrite an answer another caller already recorded, and it
/// answers [`IdempotencyAnswer::NotHeld`] rather than
/// [`IdempotencyAnswer::Recorded`] when nothing matched — see that enum's
/// own doc for why the outcome is returned rather than raised.
///
/// # Errors
/// [`RepoError::Driver`] on a storage failure, or [`RepoError::Db`] on a
/// scope refusal that raised no driver error. A key that is
/// simply not held is **not** an error; it is
/// [`IdempotencyAnswer::NotHeld`].
pub async fn answer_idempotency_key(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    endpoint: &str,
    client_key: &str,
    response_status: i32,
    response_body: JsonValue,
) -> Result<IdempotencyAnswer, RepoError> {
    let result = idempotency::Entity::update_many()
        .secure()
        .scope_with(scope)
        .col_expr(
            idempotency::Column::State,
            Expr::value("answered".to_owned()),
        )
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

#[cfg(test)]
#[path = "idempotency_repo_tests.rs"]
mod idempotency_repo_tests;
