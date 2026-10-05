#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use crate::test_support::at;
use serde_json::json;
use toolkit_db::{DBProvider, DbError};
const TENANT: Uuid = Uuid::from_u128(0x7e_11);
const OTHER_TENANT: Uuid = Uuid::from_u128(0x7e_22);
/// A migrated database and its DSN, which the test holds for its life (the directory goes with it).
async fn harness() -> (DBProvider<DbError>, crate::test_support::TestDsn) {
    let (db, _, _, dsn) = crate::test_support::test_db().await;
    (db, dsn)
}
/// Read one `products_idempotency` row by its composite key, for the tests
/// below.
async fn find_idempotency_row(
    runner: &impl toolkit_db::secure::DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    endpoint: &str,
    client_key: &str,
) -> Option<idempotency::Model> {
    idempotency::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(idempotency::Column::TenantId.eq(tenant_id))
                .add(idempotency::Column::Endpoint.eq(endpoint))
                .add(idempotency::Column::ClientKey.eq(client_key)),
        )
        .one(runner)
        .await
        .expect("read idempotency row")
}

/// Transition a claimed idempotency row to `answered` through the
/// repository's own [`answer_idempotency_key`], asserting it reports the row
/// as held.
///
/// An earlier version of this helper wrote the transition by hand, because
/// the repository had no answer-writer at all; it does now, so the setup of
/// every `answered`-state case below runs the same statement production
/// runs. A hand-written setup would let the writer regress while the cases
/// that depend on an `answered` row stayed green.
async fn answer_idempotency_row(
    runner: &impl toolkit_db::secure::DBRunner,
    scope: &AccessScope,
    tenant_id: Uuid,
    endpoint: &str,
    client_key: &str,
    response_status: i32,
    response_body: serde_json::Value,
) {
    let outcome = answer_idempotency_key(
        runner,
        scope,
        tenant_id,
        endpoint,
        client_key,
        response_status,
        response_body,
    )
    .await
    .expect("answer the claimed row");
    assert_eq!(
        outcome,
        IdempotencyAnswer::Recorded,
        "this helper's own premise: the key was claimed and is now answered"
    );
}

/// A first claim on a fresh key succeeds and persists a `claimed` row with
/// both response columns `NULL`.
///
/// This is the ordinary path `dod-idempotency-store` exists for: nothing held
/// the key before, so the claim `INSERT` itself is the gate (P-D-198) and
/// there is nothing left to conflict with.
#[tokio::test]
async fn a_first_claim_on_a_fresh_key_succeeds_and_persists_an_unanswered_claimed_row() {
    let (provider, _dsn) = harness().await;
    let conn = provider.conn().expect("scoped connection");
    let scope = AccessScope::for_tenant(TENANT);

    let outcome = claim_idempotency_key(
        &conn,
        &scope,
        TENANT,
        "products/create",
        "key-1",
        b"hash-1",
        at(9),
        at(11),
    )
    .await
    .expect("claim a fresh key");

    assert_eq!(outcome, IdempotencyClaim::Claimed);

    let row = find_idempotency_row(&conn, &scope, TENANT, "products/create", "key-1")
        .await
        .expect("the row exists");
    assert_eq!(row.state, "claimed");
    assert_eq!(row.payload_hash, b"hash-1".to_vec());
    assert_eq!(row.response_status, None);
    assert_eq!(row.response_body, None);
    assert_eq!(row.expires_at, at(11));
}

/// A second claim on a live, unexpired key **carrying the same payload** is
/// refused in flight and writes nothing to the row.
///
/// If this returned `Claimed` a second time, the guarded mutation would run
/// twice under one key — the exact failure the claim `INSERT` being the gate
/// exists to prevent (P-D-198).
///
/// The duplicate deliberately carries the **same** digest as the held claim,
/// because that is what `inst-fd-idem-claim-inflight` reserves the in-flight
/// refusal for: "a duplicate **whose payload hash matches the claimed key's**".
/// An earlier version of this case claimed `hash-1` and retried with
/// `hash-2`, and so proved in-flight for a request the design answers
/// `IDEMPOTENCY_CONFLICT` — the sibling case below is that one, retargeted.
#[tokio::test]
async fn a_second_claim_on_a_live_unexpired_key_answers_in_flight_and_writes_nothing() {
    let (provider, _dsn) = harness().await;
    let conn = provider.conn().expect("scoped connection");
    let scope = AccessScope::for_tenant(TENANT);

    claim_idempotency_key(
        &conn,
        &scope,
        TENANT,
        "products/create",
        "key-2",
        b"hash-1",
        at(9),
        at(11),
    )
    .await
    .expect("claim the key first");

    let outcome = claim_idempotency_key(
        &conn,
        &scope,
        TENANT,
        "products/create",
        "key-2",
        b"hash-1",
        at(10),
        at(15),
    )
    .await
    .expect("a duplicate against a live claim does not error");

    assert_eq!(
        outcome,
        IdempotencyClaim::InFlight {
            payload_hash: b"hash-1".to_vec(),
        },
        "the outcome carries the held digest, which is what lets the door tell this \
         duplicate from one that merely reuses the key"
    );

    let row = find_idempotency_row(&conn, &scope, TENANT, "products/create", "key-2")
        .await
        .expect("the row exists");
    assert_eq!(
        row.payload_hash,
        b"hash-1".to_vec(),
        "an in-flight refusal must not touch the row it refused against"
    );
    assert_eq!(
        row.expires_at,
        at(11),
        "an in-flight refusal must not touch the row it refused against"
    );
}

/// A second claim on a live, unexpired key **carrying a different payload**
/// reports the **held** digest, not the arriving one, and still writes
/// nothing.
///
/// This is the operand the door's `IDEMPOTENCY_CONFLICT` is computed from,
/// and it has exactly one correct value: the digest already under the key.
/// Returning the caller's own digest would make every comparison agree, and
/// the conflict `inst-fd-idem-conflict` names — "a different payload under a
/// live key fails `IDEMPOTENCY_CONFLICT`, never a silent no-op" — would be
/// structurally unreachable while looking implemented.
///
/// The repository itself still raises nothing here: it was never handed the
/// incoming request to compare, so it reports and the door judges, exactly
/// as for an `answered` row.
#[tokio::test]
async fn a_second_claim_under_a_different_payload_reports_the_held_digest_unchanged() {
    let (provider, _dsn) = harness().await;
    let conn = provider.conn().expect("scoped connection");
    let scope = AccessScope::for_tenant(TENANT);

    claim_idempotency_key(
        &conn,
        &scope,
        TENANT,
        "products/create",
        "key-2b",
        b"hash-1",
        at(9),
        at(11),
    )
    .await
    .expect("claim the key first");

    let outcome = claim_idempotency_key(
        &conn,
        &scope,
        TENANT,
        "products/create",
        "key-2b",
        b"hash-2",
        at(10),
        at(15),
    )
    .await
    .expect("a mismatching duplicate is a refusal for the door to classify, not an error");

    assert_eq!(
        outcome,
        IdempotencyClaim::InFlight {
            payload_hash: b"hash-1".to_vec(),
        },
        "the digest reported is the one the row holds, never the one that just arrived"
    );

    let row = find_idempotency_row(&conn, &scope, TENANT, "products/create", "key-2b")
        .await
        .expect("the row exists");
    assert_eq!(
        row.payload_hash,
        b"hash-1".to_vec(),
        "a mismatching duplicate owns nothing and overwrites nothing"
    );
    assert_eq!(
        row.expires_at,
        at(11),
        "a mismatching duplicate owns nothing and overwrites nothing"
    );
}

/// A claim against an `answered` row returns the stored response for replay
/// and does not overwrite it.
///
/// The replay is self-contained (P-D-198): the caller can serve
/// `response_status`/`response_body` back without re-executing the guarded
/// mutation or reading anything else, and the stored answer must survive
/// being read this way.
#[tokio::test]
async fn a_claim_against_an_answered_row_returns_the_stored_response_and_does_not_overwrite_it() {
    let (provider, _dsn) = harness().await;
    let conn = provider.conn().expect("scoped connection");
    let scope = AccessScope::for_tenant(TENANT);

    claim_idempotency_key(
        &conn,
        &scope,
        TENANT,
        "products/create",
        "key-3",
        b"hash-1",
        at(9),
        at(11),
    )
    .await
    .expect("claim the key first");
    answer_idempotency_row(
        &conn,
        &scope,
        TENANT,
        "products/create",
        "key-3",
        201,
        json!({"productId": "p-1"}),
    )
    .await;

    let outcome = claim_idempotency_key(
        &conn,
        &scope,
        TENANT,
        "products/create",
        "key-3",
        b"hash-2",
        at(10),
        at(15),
    )
    .await
    .expect("a claim against an answered row does not error");

    assert_eq!(
        outcome,
        IdempotencyClaim::Answered {
            payload_hash: b"hash-1".to_vec(),
            response_status: 201,
            response_body: json!({"productId": "p-1"}),
        }
    );

    let row = find_idempotency_row(&conn, &scope, TENANT, "products/create", "key-3")
        .await
        .expect("the row exists");
    assert_eq!(
        row.state, "answered",
        "the replay must not overwrite the stored row"
    );
    assert_eq!(row.response_status, Some(201));
    assert_eq!(row.response_body, Some(json!({"productId": "p-1"})));
}

/// A claim against an expired row takes it over: the row's `expires_at`
/// moves to the new deadline and the caller is told it claimed the key.
///
/// Expiry is evaluated at claim time, not by a reaper
/// (`design/01-foundation.md` §3.2, item 3): the very first request past the
/// deadline is what reclaims the key, with no sweep having run. The
/// *compare-and-swap* that makes the takeover safe under contention (P-D-198) is
/// the sibling case below, not this one.
#[tokio::test]
async fn a_claim_against_an_expired_row_takes_it_over_and_reports_claimed() {
    let (provider, _dsn) = harness().await;
    let conn = provider.conn().expect("scoped connection");
    let scope = AccessScope::for_tenant(TENANT);

    claim_idempotency_key(
        &conn,
        &scope,
        TENANT,
        "products/create",
        "key-4",
        b"hash-1",
        at(9),
        at(9),
    )
    .await
    .expect("claim with an already-passed expiry");

    let outcome = claim_idempotency_key(
        &conn,
        &scope,
        TENANT,
        "products/create",
        "key-4",
        b"hash-2",
        at(10),
        at(20),
    )
    .await
    .expect("the takeover does not error");

    assert_eq!(outcome, IdempotencyClaim::Claimed);

    let row = find_idempotency_row(&conn, &scope, TENANT, "products/create", "key-4")
        .await
        .expect("the row exists");
    assert_eq!(row.state, "claimed");
    assert_eq!(row.payload_hash, b"hash-2".to_vec());
    assert_eq!(row.response_status, None);
    assert_eq!(row.response_body, None);
    assert_eq!(row.expires_at, at(20));
}

/// Two claims that both read the same expired row: exactly one wins the
/// takeover and the other is told in flight, having executed nothing.
///
/// This is the case P-D-198's compare-and-swap exists for. Nothing holds an expired row between
/// one caller's conflict check and its takeover `UPDATE`, so two duplicates
/// racing on the same expired key both clear the check and both read the
/// same expired row; without the compare-and-swap on `expires_at`, both would
/// be told they claimed it and the guarded mutation would run twice under one
/// key. The interleaving is simulated directly, matching the task's own
/// prescription: both racers' takeover runs against the very same stamp the
/// one read saw, and the loser's `UPDATE` must affect zero rows rather than
/// silently succeed a second time.
///
/// **The loser here carries `hash-b` while the winner wrote `hash-a`, and it
/// is still not a conflict.** That is the one exception to "a payload
/// mismatch stays `IDEMPOTENCY_CONFLICT` in either state": the loser "may
/// even carry a different payload from the winner, and is still refused
/// in-flight rather than for the mismatch, since its transaction never
/// compared the two" (P-D-198). It read the expired holder's row, not the
/// winner's, so the outcome is `TakeoverRaceLost` — the variant that carries
/// no digest, precisely so no caller can compute a verdict from a hash this
/// transaction never saw.
#[tokio::test]
async fn the_expired_key_takeover_race_admits_exactly_one_winner() {
    let (provider, _dsn) = harness().await;
    let conn = provider.conn().expect("scoped connection");
    let scope = AccessScope::for_tenant(TENANT);

    claim_idempotency_key(
        &conn,
        &scope,
        TENANT,
        "products/create",
        "key-5",
        b"hash-0",
        at(9),
        at(9),
    )
    .await
    .expect("claim with an already-passed expiry");

    let seen = find_idempotency_row(&conn, &scope, TENANT, "products/create", "key-5")
        .await
        .expect("both racers read this same row");

    let winner = take_over_expired_idempotency_claim(&conn, &scope, &seen, b"hash-a", at(20))
        .await
        .expect("the winner's takeover does not error");
    assert_eq!(winner, IdempotencyClaim::Claimed);

    let loser = take_over_expired_idempotency_claim(&conn, &scope, &seen, b"hash-b", at(21))
        .await
        .expect("the loser's takeover does not error either: it is a refusal, not a fault");
    assert_eq!(
        loser,
        IdempotencyClaim::TakeoverRaceLost,
        "the loser's UPDATE must find nothing left matching the stamp it read, and be \
         told in flight rather than told it claimed the key a second time"
    );

    let row = find_idempotency_row(&conn, &scope, TENANT, "products/create", "key-5")
        .await
        .expect("the row exists");
    assert_eq!(
        row.payload_hash,
        b"hash-a".to_vec(),
        "only the winner's write may be visible"
    );
    assert_eq!(
        row.expires_at,
        at(20),
        "only the winner's write may be visible"
    );
}

/// The key is tenant-scoped: the same `(endpoint, client_key)` in two
/// different tenants both claim successfully.
///
/// If `tenant_id` were not part of the primary key, the second tenant's claim
/// would collide with the first's insert and be refused in flight for a key
/// it never held.
#[tokio::test]
async fn the_same_endpoint_and_client_key_in_two_tenants_both_claim() {
    let (provider, _dsn) = harness().await;
    let conn = provider.conn().expect("scoped connection");
    let scope = AccessScope::for_tenant(TENANT);
    let other_scope = AccessScope::for_tenant(OTHER_TENANT);

    let first = claim_idempotency_key(
        &conn,
        &scope,
        TENANT,
        "products/create",
        "key-shared",
        b"hash-1",
        at(9),
        at(11),
    )
    .await
    .expect("the first tenant claims");
    let second = claim_idempotency_key(
        &conn,
        &other_scope,
        OTHER_TENANT,
        "products/create",
        "key-shared",
        b"hash-1",
        at(9),
        at(11),
    )
    .await
    .expect("the second tenant claims independently");

    assert_eq!(first, IdempotencyClaim::Claimed);
    assert_eq!(second, IdempotencyClaim::Claimed);
}

/// A claim under a foreign `AccessScope` does not see another tenant's row —
/// the idempotency twin of
/// `resolution_under_a_foreign_scope_does_not_see_another_tenants_ref`.
///
/// A repository that let a foreign scope's insert attempt fall through to
/// another tenant's key would let a caller outside `TENANT` learn, from the
/// refusal it gets back, that the key is already claimed under a tenant it
/// has no access to.
#[tokio::test]
async fn a_claim_under_a_foreign_scope_does_not_see_another_tenants_row() {
    let (provider, _dsn) = harness().await;
    let conn = provider.conn().expect("scoped connection");
    let owner_scope = AccessScope::for_tenant(TENANT);
    let foreign_scope = AccessScope::for_tenant(OTHER_TENANT);

    claim_idempotency_key(
        &conn,
        &owner_scope,
        TENANT,
        "products/create",
        "key-7",
        b"hash-1",
        at(9),
        at(11),
    )
    .await
    .expect("the owner claims normally");

    let err = claim_idempotency_key(
        &conn,
        &foreign_scope,
        TENANT,
        "products/create",
        "key-7",
        b"hash-2",
        at(9),
        at(11),
    )
    .await
    .expect_err("a foreign scope must neither see nor claim under TENANT's row");
    assert!(matches!(err, RepoError::Db(_)));

    // The owner's own claim is untouched by the foreign attempt.
    let row = find_idempotency_row(&conn, &owner_scope, TENANT, "products/create", "key-7")
        .await
        .expect("the row exists");
    assert_eq!(row.payload_hash, b"hash-1".to_vec());
}

/// The answer write moves a claimed row to `answered` and fills **both**
/// response columns in one statement.
///
/// `chk_products_idempotency_response_group` admits `answered` only with the
/// status and the body together, so a writer that set one column, or that
/// moved the state without either, could not commit at all — this case is
/// what proves the single statement carries all three, rather than that the
/// function merely returned `Ok`.
#[tokio::test]
async fn the_answer_write_moves_the_row_to_answered_and_fills_both_response_columns() {
    let (provider, _dsn) = harness().await;
    let conn = provider.conn().expect("scoped connection");
    let scope = AccessScope::for_tenant(TENANT);

    claim_idempotency_key(
        &conn,
        &scope,
        TENANT,
        "products/create",
        "key-answer-1",
        b"hash-1",
        at(9),
        at(11),
    )
    .await
    .expect("claim the key first");

    let outcome = answer_idempotency_key(
        &conn,
        &scope,
        TENANT,
        "products/create",
        "key-answer-1",
        201,
        json!({"productId": "p-1"}),
    )
    .await
    .expect("answer the held claim");
    assert_eq!(outcome, IdempotencyAnswer::Recorded);

    let row = find_idempotency_row(&conn, &scope, TENANT, "products/create", "key-answer-1")
        .await
        .expect("the row exists");
    assert_eq!(row.state, "answered");
    assert_eq!(row.response_status, Some(201));
    assert_eq!(row.response_body, Some(json!({"productId": "p-1"})));
    assert_eq!(
        row.payload_hash,
        b"hash-1".to_vec(),
        "the answer records a response; it must not restamp the digest the claim was made against"
    );
    assert_eq!(
        row.expires_at,
        at(11),
        "the answer records a response; the retention deadline is the claim's own"
    );
}

/// A claim arriving after the answer write reads the stored response back —
/// the two halves of the store joined end to end.
///
/// Each half alone is satisfiable by a broken pairing: a writer that stored
/// the status under the wrong column, or a reader that reported an
/// `answered` row as in flight, is caught only by driving the write and then
/// the read. This is the retry a client actually makes, one layer below the
/// door.
#[tokio::test]
async fn a_claim_after_the_answer_write_replays_the_recorded_response() {
    let (provider, _dsn) = harness().await;
    let conn = provider.conn().expect("scoped connection");
    let scope = AccessScope::for_tenant(TENANT);

    claim_idempotency_key(
        &conn,
        &scope,
        TENANT,
        "products/create",
        "key-answer-2",
        b"hash-1",
        at(9),
        at(11),
    )
    .await
    .expect("claim the key first");
    answer_idempotency_row(
        &conn,
        &scope,
        TENANT,
        "products/create",
        "key-answer-2",
        201,
        json!({"productId": "p-2"}),
    )
    .await;

    let replay = claim_idempotency_key(
        &conn,
        &scope,
        TENANT,
        "products/create",
        "key-answer-2",
        b"hash-1",
        at(10),
        at(15),
    )
    .await
    .expect("the retry's claim does not error");

    assert_eq!(
        replay,
        IdempotencyClaim::Answered {
            payload_hash: b"hash-1".to_vec(),
            response_status: 201,
            response_body: json!({"productId": "p-2"}),
        },
        "the retry is answered from the row the answer write left behind, not refused in flight"
    );
}

/// An answer write against a row that is not `claimed` reports
/// [`IdempotencyAnswer::NotHeld`] and writes nothing — both when no row
/// exists at all and when one exists already `answered`.
///
/// This is the branch that must never be a silent success. A writer without
/// the `state = 'claimed'` predicate would overwrite the answer already
/// recorded under the second key, replacing one act's outcome with another's
/// — the substitution the store exists to prevent — and a writer that
/// reported zero rows as `Recorded` would let its caller commit an act whose
/// answer was never stored.
#[tokio::test]
async fn an_answer_write_on_a_row_that_is_not_claimed_reports_not_held_and_writes_nothing() {
    let (provider, _dsn) = harness().await;
    let conn = provider.conn().expect("scoped connection");
    let scope = AccessScope::for_tenant(TENANT);

    let unclaimed = answer_idempotency_key(
        &conn,
        &scope,
        TENANT,
        "products/create",
        "key-answer-3",
        201,
        json!({"productId": "p-3"}),
    )
    .await
    .expect("answering an unclaimed key is an outcome, not a fault");
    assert_eq!(
        unclaimed,
        IdempotencyAnswer::NotHeld,
        "no row was ever claimed under this key, so there was nothing to answer"
    );
    assert!(
        find_idempotency_row(&conn, &scope, TENANT, "products/create", "key-answer-3")
            .await
            .is_none(),
        "a missed answer must not conjure the row it failed to find"
    );

    claim_idempotency_key(
        &conn,
        &scope,
        TENANT,
        "products/create",
        "key-answer-4",
        b"hash-1",
        at(9),
        at(11),
    )
    .await
    .expect("claim the key first");
    answer_idempotency_row(
        &conn,
        &scope,
        TENANT,
        "products/create",
        "key-answer-4",
        201,
        json!({"productId": "p-4"}),
    )
    .await;

    let second = answer_idempotency_key(
        &conn,
        &scope,
        TENANT,
        "products/create",
        "key-answer-4",
        500,
        json!({"productId": "an-act-that-never-ran"}),
    )
    .await
    .expect("a second answer is an outcome, not a fault");
    assert_eq!(
        second,
        IdempotencyAnswer::NotHeld,
        "the row is already answered, so it is not held `claimed` by anyone"
    );

    let row = find_idempotency_row(&conn, &scope, TENANT, "products/create", "key-answer-4")
        .await
        .expect("the row exists");
    assert_eq!(row.response_status, Some(201));
    assert_eq!(
        row.response_body,
        Some(json!({"productId": "p-4"})),
        "the first answer stands: a second write must not replace one act's outcome with another's"
    );
}

/// The answer write commits **inside** the caller's transaction: a rollback
/// takes it, and the key is left free rather than answered.
///
/// `inst-fd-idem-claim-write` requires claim, mutation and answer to commit
/// together or not at all, and only running the write inside a transaction
/// that then fails can tell that wiring from one that answers on a runner of
/// its own. Without this case, a writer that opened its own connection would
/// keep every other case here green while recording, in production, a `201`
/// for an act whose transaction rolled back.
#[tokio::test]
async fn the_answer_write_rolls_back_with_the_transaction_it_rides_in() {
    let (provider, _dsn) = harness().await;
    let scope = AccessScope::for_tenant(TENANT);
    let scope_for_mutation = scope.clone();

    let mutation = provider
        .transaction(move |tx| {
            Box::pin(async move {
                claim_idempotency_key(
                    tx,
                    &scope_for_mutation,
                    TENANT,
                    "products/create",
                    "key-answer-5",
                    b"hash-1",
                    at(9),
                    at(11),
                )
                .await
                .map_err(|e| DbError::Other(anyhow::Error::msg(e.to_string())))?;

                answer_idempotency_key(
                    tx,
                    &scope_for_mutation,
                    TENANT,
                    "products/create",
                    "key-answer-5",
                    201,
                    json!({"productId": "p-5"}),
                )
                .await
                .map_err(|e| DbError::Other(anyhow::Error::msg(e.to_string())))?;

                Err::<(), DbError>(DbError::Other(anyhow::Error::msg(
                    "the act fails after its claim was answered",
                )))
            })
        })
        .await;
    assert!(mutation.is_err(), "the mutation must roll back");

    let conn = provider.conn().expect("scoped connection");
    assert!(
        find_idempotency_row(&conn, &scope, TENANT, "products/create", "key-answer-5")
            .await
            .is_none(),
        "claim and answer commit with the mutation, so a rollback leaves no answered row, and \
         no claimed one either"
    );
}
