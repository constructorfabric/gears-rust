//! `pricing_approval`'s and `pricing_approval_key`'s append-only triggers on the
//! executed `SQLite` mirror.
//!
//! Postgres carries the rule as one PL/pgSQL function with five branches;
//! `SQLite` has no procedural language, so the migration mirrors it as five
//! `RAISE(ABORT, ...)` triggers with literal messages. Until this file existed
//! **all of them could be deleted with the whole crate green** — nothing
//! asserted their presence and nothing executed them — while
//! `tests/sqlite_approval_repo.rs` opened its module doc by claiming that "the
//! refusals are the ones the trigger and the CHECKs produce". The CHECK half was
//! true; the trigger half was not. `tests/sqlite_append_only.rs` is the
//! precedent: `pricing_price`'s mirror triggers are executed there for the same
//! reason, and the mirror is the backend e2e and dev boot on.
//!
//! One test per trigger, each written so that trigger is the **only** object
//! that can answer:
//!
//! | trigger | test |
//! |---|---|
//! | `trg_pricing_approval_born_submitted` | `a_record_is_born_submitted_or_it_is_not_born` |
//! | `trg_pricing_approval_no_delete` | `an_approval_row_cannot_be_deleted` |
//! | `trg_pricing_approval_immutable_once_decided` | `a_decided_record_is_frozen` |
//! | `trg_pricing_approval_pinned_columns` | `a_decision_may_not_re_pin_what_it_decides` |
//! | `trg_pricing_approval_flip_whitelist` | `a_pending_record_cannot_stay_pending` |
//! | `trg_pricing_approval_key_no_delete` | `a_register_row_cannot_be_deleted` |
//! | `trg_pricing_approval_key_born_submitted` | `a_register_row_is_born_submitted` |
//! | `trg_pricing_approval_key_born_under_a_pending_unit` | `a_register_row_is_born_under_a_pending_unit` |
//! | `trg_pricing_approval_key_pinned_columns` | `a_register_row_cannot_be_re_pointed` |
//! | `trg_pricing_approval_key_follows_its_unit` | `an_ad_hoc_update_cannot_free_a_held_key` |
//! | `trg_pricing_approval_key_follow_state` | `a_decided_unit_frees_every_key_it_held` (`sqlite_window_service.rs`) |
//!
//! **The register's half was untested and its own doc claimed otherwise.** The
//! migration said the guards were "the same whitelist shape the parent carries, so an
//! ad-hoc `UPDATE` cannot free a key either", and that was false: the parent whitelists
//! the *destination* of a flip, the register only pinned its other columns — so
//! `UPDATE pricing_approval_key SET state = 'voided' WHERE scope_key = '<key>'` landed,
//! the parent stayed `submitted`, and `uq_pricing_approval_key_pending` then admitted a
//! second holder. [`an_ad_hoc_update_cannot_free_a_held_key`] is that statement,
//! executed. `trg_pricing_approval_key_follows_once` has no case of its own here for
//! the reason the pinned-columns note below gives — it and the direction whitelist
//! answer the same `UPDATE`, and `SQLite` fixes no order between two triggers on one
//! verb, so the case that separates them is the one where only one *can* fire.
//!
//! The pinned-columns case in particular moves a pinned column **as part of a
//! sanctioned flip**. Moving one on its own would leave `NEW.state` still
//! `submitted`, which trips the flip whitelist as well — and `SQLite` fixes no
//! order between two triggers on one verb, so that test would have been a coin
//! toss between two objects rather than evidence about either.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use sea_orm::DatabaseConnection;

mod common;

use common::{exec, migrated_db, must_succeed, scalar};

const TENANT: &str = "11111111-1111-1111-1111-111111111111";
const SUBJECT: &str = "22222222-2222-2222-2222-222222222222/3";
const SUBMITTER: &str = "44444444-4444-4444-4444-444444444444";
const APPROVER: &str = "55555555-5555-5555-5555-555555555555";
const PENDING: &str = "66666666-6666-6666-6666-666666666666";

const SUBMITTED_AT: &str = "2026-08-03 09:00:00 +00:00";
const DECIDED_AT: &str = "2026-08-03 10:00:00 +00:00";

/// Reject, **and** for the stated reason.
///
/// The fragment is not decoration: this table carries six `CHECK` constraints
/// and five triggers, and every one of their messages names
/// `pricing_approval`. A test that accepted any error naming the table would
/// pass with the trigger it means to prove deleted, refused instead by a
/// neighbour it never intended to trip.
async fn must_be_rejected(conn: &DatabaseConnection, sql: &str, because: &str) {
    let err = exec(conn, sql)
        .await
        .err()
        .unwrap_or_else(|| panic!("the append-only guard must reject: {sql}"));
    let message = err.to_string();
    assert!(
        message.contains("pricing_approval"),
        "the rejection must name the guard it came from, got: {message}"
    );
    assert!(
        message.contains(because),
        "the rejection must be the one under test (`{because}`), got: {message}"
    );
}

/// An `INSERT` of one record in a named state, valid against all six CHECKs.
fn insert(id: &str, state: &str, approver: &str, reason: &str, decided_at: &str) -> String {
    format!(
        "INSERT INTO pricing_approval (
            approval_id, tenant_id, subject_ref, subject_kind, content_hash,
            state, submitter_principal, approver_principal, reason, materiality,
            submitted_at, decided_at)
         VALUES ('{id}', '{TENANT}', '{SUBJECT}', 'plan_revision', X'deadbeef',
            '{state}', '{SUBMITTER}', {approver}, {reason}, '{{}}',
            '{SUBMITTED_AT}', {decided_at})"
    )
}

/// One pending record — the world every case below starts in.
async fn seed_pending(conn: &DatabaseConnection) {
    must_succeed(conn, &insert(PENDING, "submitted", "NULL", "NULL", "NULL")).await;
}

/// The one flip the machine sanctions, spelled as SQL.
fn approve(id: &str) -> String {
    format!(
        "UPDATE pricing_approval
            SET state = 'approved', approver_principal = '{APPROVER}',
                decided_at = '{DECIDED_AT}'
          WHERE approval_id = '{id}'"
    )
}

async fn state_of(conn: &DatabaseConnection, id: &str) -> String {
    scalar(
        conn,
        &format!("SELECT state AS v FROM pricing_approval WHERE approval_id = '{id}'"),
    )
    .await
}

/// The move that is *supposed* to work.
///
/// First, and load-bearing: without it every refusal below would pass against a
/// table five triggers had made read-only, which is not the rule the design set
/// states.
#[tokio::test]
async fn a_pending_record_takes_its_one_decision() {
    let conn = migrated_db().await;
    seed_pending(&conn).await;

    must_succeed(&conn, &approve(PENDING)).await;

    assert_eq!(state_of(&conn, PENDING).await, "approved");
}

/// A record is born `submitted`; every other birth is refused.
///
/// The mirror of the hole the Postgres branch closes. Each row below satisfies
/// all six CHECKs — approver where the state needs one, reason where it needs
/// one, `decided_at` throughout — so the trigger is the only object left that
/// can refuse it. Without it a row arrives `approved` having bypassed the whole
/// decision plane, because there was no `UPDATE` on which to bypass it.
#[tokio::test]
async fn a_record_is_born_submitted_or_it_is_not_born() {
    let conn = migrated_db().await;

    for (n, (state, approver, reason)) in [
        ("approved", format!("'{APPROVER}'"), "NULL".to_owned()),
        (
            "rejected",
            format!("'{APPROVER}'"),
            "'margin below floor'".to_owned(),
        ),
        ("voided", "NULL".to_owned(), "NULL".to_owned()),
    ]
    .into_iter()
    .enumerate()
    {
        must_be_rejected(
            &conn,
            &insert(
                &format!("00000000-0000-0000-0000-0000000000{n:02}"),
                state,
                &approver,
                &reason,
                &format!("'{DECIDED_AT}'"),
            ),
            "a record is born submitted",
        )
        .await;
    }

    // And nothing was born at all.
    assert_eq!(
        scalar(
            &conn,
            "SELECT CAST(count(*) AS TEXT) AS v FROM pricing_approval"
        )
        .await,
        "0"
    );
}

/// `inst-as-immutable`: a decided record is frozen, in every column.
#[tokio::test]
async fn a_decided_record_is_frozen() {
    let conn = migrated_db().await;
    seed_pending(&conn).await;
    must_succeed(&conn, &approve(PENDING)).await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_approval SET reason = 'on reflection'
              WHERE approval_id = '{PENDING}'"
        ),
        "a decided record is immutable",
    )
    .await;
    // Not only the decision columns: a second decision is the same refusal.
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_approval SET state = 'rejected', reason = 'changed my mind'
              WHERE approval_id = '{PENDING}'"
        ),
        "a decided record is immutable",
    )
    .await;

    assert_eq!(state_of(&conn, PENDING).await, "approved");
}

/// The pin. `content_hash` **is** the TOCTOU guard (`inst-ap-pin`), so a
/// decision that could re-pin it in place would launder the very mutation the
/// guard exists to catch.
///
/// Every statement here is an otherwise-sanctioned flip that also moves one
/// pinned column, which is what keeps the flip whitelist out of the answer.
#[tokio::test]
async fn a_decision_may_not_re_pin_what_it_decides() {
    let conn = migrated_db().await;
    seed_pending(&conn).await;

    for column in [
        "content_hash = X'cafebabe'",
        "subject_ref = 'other/1'",
        "subject_kind = 'price_unit'",
        "submitter_principal = '77777777-7777-7777-7777-777777777777'",
        "materiality = '{\"a\":1}'",
        "submitted_at = '2026-08-03 08:00:00 +00:00'",
        "tenant_id = '88888888-8888-8888-8888-888888888888'",
        "approval_id = '99999999-9999-9999-9999-999999999999'",
    ] {
        must_be_rejected(
            &conn,
            &format!(
                "UPDATE pricing_approval
                    SET state = 'approved', approver_principal = '{APPROVER}',
                        decided_at = '{DECIDED_AT}', {column}
                  WHERE approval_id = '{PENDING}'"
            ),
            "only the decision columns may move",
        )
        .await;
    }

    assert_eq!(
        state_of(&conn, PENDING).await,
        "submitted",
        "no refused statement may have landed"
    );
}

/// The flip whitelist: `submitted -> submitted` is not a move the machine has.
///
/// No `CHECK` can catch this — the resulting row is a perfectly valid pending
/// record — so the trigger is the only thing between a re-submit and one
/// approval unit being re-used in place, `content_hash` and all.
#[tokio::test]
async fn a_pending_record_cannot_stay_pending() {
    let conn = migrated_db().await;
    seed_pending(&conn).await;

    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_approval SET state = 'submitted'
              WHERE approval_id = '{PENDING}'"
        ),
        "is not a sanctioned flip",
    )
    .await;
    // An edit that names no state at all is the same move: `NEW.state` is still
    // `submitted`, and a pending record that could be edited in place is a
    // re-submit wearing the first submission's timestamp.
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_approval SET reason = 'second thoughts'
              WHERE approval_id = '{PENDING}'"
        ),
        "is not a sanctioned flip",
    )
    .await;
}

/// No `REVOKE` anywhere in this chain, deliberately — it names a deployment role
/// the migration does not own and `SQLite` has no `GRANT` at all. The trigger is
/// the portable half of the discipline, and it is the half that has to work.
#[tokio::test]
async fn an_approval_row_cannot_be_deleted() {
    let conn = migrated_db().await;
    seed_pending(&conn).await;

    // A pending record is what `PENDING_CHANGE_UNIT_EXISTS` reads.
    must_be_rejected(
        &conn,
        &format!("DELETE FROM pricing_approval WHERE approval_id = '{PENDING}'"),
        "DELETE of an approval is not permitted",
    )
    .await;

    // And a decided one is the evidence itself.
    must_succeed(&conn, &approve(PENDING)).await;
    must_be_rejected(
        &conn,
        &format!("DELETE FROM pricing_approval WHERE approval_id = '{PENDING}'"),
        "DELETE of an approval is not permitted",
    )
    .await;

    assert_eq!(
        scalar(
            &conn,
            "SELECT CAST(count(*) AS TEXT) AS v FROM pricing_approval"
        )
        .await,
        "1"
    );
}

// ---------------------------------------------------------------------------
// `pricing_approval_key` — the register's own guards
// ---------------------------------------------------------------------------

/// One canonical scope key, as the register renders it: the ten axes, piped.
const KEY: &str = "3f2a|EUR|eu|base|9c1|all_subscriptions|recurring|none|none|none";
/// A second key on the same plan, for the case that must still be admitted.
const OTHER_KEY: &str = "3f2a|USD|us|base|9c1|all_subscriptions|recurring|none|none|none";
/// A second pending unit, for the one re-point case its own guard cannot answer
/// without it — see [`a_register_row_cannot_be_re_pointed`].
const SECOND_UNIT: &str = "00000000-0000-0000-0000-0000000000dd";

/// Reject, **and** for the stated reason, on the register.
///
/// [`must_be_rejected`]'s sibling with a different table in the fragment: five of the
/// register's six guards name `pricing_approval_key` and a test that accepted any
/// error mentioning the parent would pass with the guard under test deleted.
async fn register_must_be_rejected(conn: &DatabaseConnection, sql: &str, because: &str) {
    let err = exec(conn, sql)
        .await
        .err()
        .unwrap_or_else(|| panic!("the register guard must reject: {sql}"));
    let message = err.to_string();
    assert!(
        message.contains("pricing_approval_key"),
        "the rejection must name the guard it came from, got: {message}"
    );
    assert!(
        message.contains(because),
        "the rejection must be the one under test (`{because}`), got: {message}"
    );
}

/// Insert one register row holding `key` for `unit`, in `state`.
fn hold(unit: &str, key: &str, state: &str) -> String {
    format!(
        "INSERT INTO pricing_approval_key (approval_id, tenant_id, scope_key, state)
         VALUES ('{unit}', '{TENANT}', '{key}', '{state}')"
    )
}

/// A pending unit holding [`KEY`] — the world every case below starts in.
async fn seed_held_key(conn: &DatabaseConnection) {
    seed_pending(conn).await;
    must_succeed(conn, &hold(PENDING, KEY, "submitted")).await;
}

/// The move that is *supposed* to work: the parent's own transition carries the
/// register with it.
///
/// First and load-bearing, for [`a_pending_record_takes_its_one_decision`]'s reason:
/// without it every refusal below would pass against a register six triggers had made
/// read-only, which is not the rule the design set states — a decided unit **frees**
/// its keys.
#[tokio::test]
async fn the_parents_decision_carries_the_register_with_it() {
    let conn = migrated_db().await;
    seed_held_key(&conn).await;

    must_succeed(&conn, &approve(PENDING)).await;

    assert_eq!(
        scalar(
            &conn,
            &format!("SELECT state AS v FROM pricing_approval_key WHERE scope_key = '{KEY}'")
        )
        .await,
        "approved",
        "the register followed its unit out of `submitted`"
    );
    // And the key is really free: a second unit takes it.
    must_succeed(
        &conn,
        &insert(
            "00000000-0000-0000-0000-0000000000aa",
            "submitted",
            "NULL",
            "NULL",
            "NULL",
        ),
    )
    .await;
    must_succeed(
        &conn,
        &hold("00000000-0000-0000-0000-0000000000aa", KEY, "submitted"),
    )
    .await;
}

/// **The statement the migration's own doc said was impossible.**
///
/// `UPDATE pricing_approval_key SET state = 'voided' WHERE scope_key = '<key>'` leaves
/// the three pinned columns untouched and satisfies `OLD.state = 'submitted'`, so
/// neither the pinned-columns guard nor the follows-once guard sees anything wrong with
/// it. Before `trg_pricing_approval_key_follows_its_unit` it **landed**: the child read
/// `voided`, the parent still read `submitted`, and the partial unique index then
/// admitted a second holder — two units holding one key while the first was still
/// approvable, which is exactly the state `inst-co-single-pending` exists to make
/// impossible.
///
/// **No neighbour can refuse this input**, which is why it is the statement only this
/// guard can answer: the columns it moves are the one column the register permits
/// moving, and it moves it *from* `submitted`.
#[tokio::test]
async fn an_ad_hoc_update_cannot_free_a_held_key() {
    let conn = migrated_db().await;
    seed_held_key(&conn).await;

    register_must_be_rejected(
        &conn,
        &format!("UPDATE pricing_approval_key SET state = 'voided' WHERE scope_key = '{KEY}'"),
        "a register row follows its unit",
    )
    .await;

    assert_eq!(
        scalar(
            &conn,
            &format!("SELECT state AS v FROM pricing_approval_key WHERE scope_key = '{KEY}'")
        )
        .await,
        "submitted",
        "the key is still held"
    );
    // And the hold is still enforced, which is the half that made the defect a defect.
    must_succeed(
        &conn,
        &insert(
            "00000000-0000-0000-0000-0000000000bb",
            "submitted",
            "NULL",
            "NULL",
            "NULL",
        ),
    )
    .await;
    let second = exec(
        &conn,
        &hold("00000000-0000-0000-0000-0000000000bb", KEY, "submitted"),
    )
    .await;
    assert!(
        second.is_err(),
        "a second holder must still be refused: {second:?}"
    );
}

/// A register row is born `submitted`, whatever its unit says.
#[tokio::test]
async fn a_register_row_is_born_submitted() {
    let conn = migrated_db().await;
    seed_pending(&conn).await;

    for state in ["approved", "rejected", "voided"] {
        register_must_be_rejected(
            &conn,
            &hold(PENDING, KEY, state),
            "a register row is born submitted with its unit",
        )
        .await;
    }
}

/// A register row is born **under a pending unit** — the missing foreign key and the
/// parent-state check, which are one rule because they have one failure mode.
///
/// A row born under a unit that is missing or already decided holds its key **for
/// ever**: `trg_pricing_approval_key_follow_state` fires only `AFTER UPDATE`, the
/// parent refuses every `UPDATE` once decided, and `find_pending_key_holder` then
/// answers `CorruptRow` — a 500, not a refusal an operator can act on.
///
/// Both births are staged, because they are two different mistakes with one remedy:
/// a unit that does not exist at all, and one that exists and has been decided.
#[tokio::test]
async fn a_register_row_is_born_under_a_pending_unit() {
    let conn = migrated_db().await;

    // No such unit.
    register_must_be_rejected(
        &conn,
        &hold("00000000-0000-0000-0000-0000000000cc", KEY, "submitted"),
        "a register row is born with a pending unit",
    )
    .await;

    // A unit that exists and is decided.
    seed_pending(&conn).await;
    must_succeed(&conn, &approve(PENDING)).await;
    register_must_be_rejected(
        &conn,
        &hold(PENDING, KEY, "submitted"),
        "a register row is born with a pending unit",
    )
    .await;

    assert_eq!(
        scalar(
            &conn,
            "SELECT CAST(count(*) AS TEXT) AS v FROM pricing_approval_key"
        )
        .await,
        "0",
        "no phantom hold landed"
    );
}

/// A register row cannot be re-pointed at another unit, tenant or key.
///
/// The row **is** the record of what the unit held, so every column but `state` is
/// pinned. Each statement here also sets `state` to the value its unit is already in,
/// which is what keeps the two neighbouring guards out of the answer: a statement that
/// moved `state` would be refused by the direction whitelist or by the follows-once
/// arm, and `SQLite` fixes no order between two triggers on one verb.
///
/// **The `approval_id` case needs a second *existing, pending* unit to point at, and
/// that is not staging convenience.** Pointing it at an id nobody holds is refused by
/// the direction whitelist instead — the subquery finds no unit, so `NEW.state` matches
/// nothing — which was measured: the case passed for the wrong guard's reason until the
/// second unit was seeded. With a real pending unit at the other end, the whitelist is
/// satisfied and the pinned-columns guard is the only object that can refuse.
#[tokio::test]
async fn a_register_row_cannot_be_re_pointed() {
    let conn = migrated_db().await;
    seed_held_key(&conn).await;
    must_succeed(
        &conn,
        &insert(SECOND_UNIT, "submitted", "NULL", "NULL", "NULL"),
    )
    .await;

    for column in [
        &format!("approval_id = '{SECOND_UNIT}'"),
        "tenant_id = '88888888-8888-8888-8888-888888888888'",
        &format!("scope_key = '{OTHER_KEY}'"),
    ] {
        register_must_be_rejected(
            &conn,
            &format!(
                "UPDATE pricing_approval_key SET state = 'submitted', {column}
                  WHERE scope_key = '{KEY}'"
            ),
            "the register row is pinned",
        )
        .await;
    }

    assert_eq!(
        scalar(
            &conn,
            &format!(
                "SELECT CAST(count(*) AS TEXT) AS v FROM pricing_approval_key
                  WHERE approval_id = '{PENDING}' AND scope_key = '{KEY}'"
            )
        )
        .await,
        "1",
        "the row still records what the unit held"
    );
}

/// A register row cannot be deleted, pending or decided.
///
/// `DELETE` is refused here for the reason it is refused on the parent: the register is
/// the record of what a unit held, and this store's discipline is that a decided record
/// is evidence. It is also the alternative the migration's doc rejects for maintaining
/// `state` — deleting rows on a decision — so a `DELETE` that worked would be that
/// design arriving by the back door.
#[tokio::test]
async fn a_register_row_cannot_be_deleted() {
    let conn = migrated_db().await;
    seed_held_key(&conn).await;

    register_must_be_rejected(
        &conn,
        &format!("DELETE FROM pricing_approval_key WHERE scope_key = '{KEY}'"),
        "DELETE of a held key is not permitted",
    )
    .await;

    must_succeed(&conn, &approve(PENDING)).await;
    register_must_be_rejected(
        &conn,
        &format!("DELETE FROM pricing_approval_key WHERE scope_key = '{KEY}'"),
        "DELETE of a held key is not permitted",
    )
    .await;

    assert_eq!(
        scalar(
            &conn,
            "SELECT CAST(count(*) AS TEXT) AS v FROM pricing_approval_key"
        )
        .await,
        "1"
    );
}
