//! What the **schema** refuses of a bulk operation — Slice 12 §4's state
//! machine, driven as raw SQL because what is under test is the store's own
//! guarantee and not any repository's use of it.
//!
//! §4 gives seven states and six edges — `rejected` and
//! `awaiting_approval → rejected` are D-267's, and the rest are as built. A
//! caller that walked a seventh edge would re-drive a commit whose rows are
//! already applied, or strand a record in a state nothing can leave, and neither
//! is a mistake a caller should be trusted not to make: the run's report is an
//! operator-facing record of money that moved.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use sea_orm::DatabaseConnection;

mod common;

use common::{exec, migrated_db, must_succeed};

const OP: &str = "11111111-1111-1111-1111-111111111111";
const TENANT: &str = "22222222-2222-2222-2222-222222222222";
const ACTOR: &str = "33333333-3333-3333-3333-333333333333";

async fn must_be_rejected(conn: &DatabaseConnection, sql: &str, because: &str) {
    let err = exec(conn, sql)
        .await
        .expect_err(&format!("this statement must be rejected: {sql}"));
    let message = err.to_string();
    assert!(
        message.contains(because),
        "the rejection must be the one under test (`{because}`), got: {message}"
    );
}

/// A run of `kind` parked in `validating`, the initial state.
fn seed(kind: &str) -> String {
    format!(
        "INSERT INTO pricing_bulk_operation \
         (operation_id, tenant_id, kind, state, client_key, report, submitted_by, submitted_at) \
         VALUES ('{OP}', '{TENANT}', '{kind}', 'validating', 'ck-1', '{{}}', '{ACTOR}', \
         '2026-08-08T00:00:00Z')"
    )
}

fn move_to(state: &str) -> String {
    move_to_with(state, terminal(state).then_some("'2026-08-08T01:00:00Z'"))
}

/// The states that end a run, which is also the set
/// `chk_pricing_bulk_operation_completed_at` names. `rejected` joined it with
/// D-267: a refused batch approval is an outcome, not a pause.
fn terminal(state: &str) -> bool {
    matches!(
        state,
        "validation_failed" | "completed" | "completed_with_conflicts" | "rejected"
    )
}

/// The same move with the end instant chosen by the caller, so a case can ask
/// what the `completed_at` agreement refuses rather than only what it admits.
fn move_to_with(state: &str, completed: Option<&str>) -> String {
    let completed = completed.unwrap_or("NULL");
    format!(
        "UPDATE pricing_bulk_operation SET state = '{state}', completed_at = {completed} \
         WHERE operation_id = '{OP}'"
    )
}

/// Every edge §4 draws is walkable, and this is the case that would catch a
/// guard written too tight — a state machine that refuses a legal move is as
/// broken as one that admits an illegal one, and only one of the two is loud.
#[tokio::test]
async fn every_sanctioned_edge_is_walkable() {
    for (kind, path) in [
        ("import", vec!["committing", "completed"]),
        (
            "repricing",
            vec![
                "awaiting_approval",
                "committing",
                "completed_with_conflicts",
            ],
        ),
        ("import", vec!["validation_failed"]),
        // D-267's edge: the approval was refused, and the run ends there.
        ("repricing", vec!["awaiting_approval", "rejected"]),
    ] {
        let conn = migrated_db().await;
        must_succeed(&conn, &seed(kind)).await;
        for state in path {
            must_succeed(&conn, &move_to(state)).await;
        }
    }
}

/// **A rejected run is over, so it carries an end instant** — D-267 put
/// `rejected` in `chk_pricing_bulk_operation_completed_at`'s terminal set, and
/// this is the case that gives that half of the migration an operand. Reachable
/// at `UPDATE` and *not* at `INSERT`: the born-validating trigger is strictly
/// narrower and answers first, so a case expecting the constraint at insert
/// would be reading the trigger's refusal instead (D-261's shadowing, met again).
#[tokio::test]
async fn a_rejected_run_carries_the_instant_it_ended() {
    let conn = migrated_db().await;
    must_succeed(&conn, &seed("repricing")).await;
    must_succeed(&conn, &move_to("awaiting_approval")).await;
    must_be_rejected(
        &conn,
        &move_to_with("rejected", None),
        "chk_pricing_bulk_operation_completed_at",
    )
    .await;
    must_succeed(&conn, &move_to("rejected")).await;
}

/// **`rejected` is terminal**, and nothing takes a run back out of it: a
/// re-drive of an approval that was refused would commit rows the approver
/// declined, which is the one outcome a rejection exists to prevent.
#[tokio::test]
async fn a_rejected_run_never_moves_again() {
    for onward in ["committing", "completed", "awaiting_approval", "validating"] {
        let conn = migrated_db().await;
        must_succeed(&conn, &seed("repricing")).await;
        must_succeed(&conn, &move_to("awaiting_approval")).await;
        must_succeed(&conn, &move_to("rejected")).await;
        must_be_rejected(&conn, &move_to(onward), "not an edge").await;
    }
}

/// **`awaiting_approval` is the only state that reaches `rejected`.** The edge
/// is an approval decision, so a run that never asked for one cannot be refused
/// one — and a run already committing has rows applied that a rejection could
/// not take back.
#[tokio::test]
async fn nothing_but_a_pending_approval_can_be_rejected() {
    // From the initial state, with no approval outstanding.
    let conn = migrated_db().await;
    must_succeed(&conn, &seed("repricing")).await;
    must_be_rejected(&conn, &move_to("rejected"), "not an edge").await;

    // From `committing`, where the decision has already been taken.
    let conn = migrated_db().await;
    must_succeed(&conn, &seed("repricing")).await;
    must_succeed(&conn, &move_to("committing")).await;
    must_be_rejected(&conn, &move_to("rejected"), "not an edge").await;

    // From a terminal state, which `a_terminal_run_never_moves_again` covers for
    // `committing` and this covers for the state D-267 added.
    let conn = migrated_db().await;
    must_succeed(&conn, &seed("repricing")).await;
    must_succeed(&conn, &move_to("committing")).await;
    must_succeed(&conn, &move_to("completed")).await;
    must_be_rejected(&conn, &move_to("rejected"), "not an edge").await;

    // The remaining way in would be birth, and
    // `a_run_cannot_be_born_past_the_state_machine` carries `rejected` in its
    // list rather than this case restating it.
}

/// **An import can never be rejected, and D-267 adds no clause saying so.**
/// `rejected` is reachable only from `awaiting_approval`, which
/// `chk_pricing_bulk_operation_import_never_awaits` already forbids an import —
/// so the new edge inherits D-137 rather than restating it. Verified rather than
/// argued: a second constraint repeating a rule that already holds is a rule
/// that can never fail, and this suite would not be able to tell it from one
/// that does.
#[tokio::test]
async fn an_import_can_never_be_rejected() {
    // The direct move, refused by the edge list rather than by the import
    // `CHECK` — the import never gets far enough for that `CHECK` to be asked.
    let conn = migrated_db().await;
    must_succeed(&conn, &seed("import")).await;
    must_be_rejected(&conn, &move_to("rejected"), "not an edge").await;

    // And the only state that could reach `rejected` is the one an import is
    // refused, so the path is closed at both ends. Named rather than asserted as
    // a bare `CHECK`: four constraints sit on this table and only one of them is
    // D-137's, so a rewrite that let a different one answer would stay green.
    must_be_rejected(
        &conn,
        &move_to("awaiting_approval"),
        "chk_pricing_bulk_operation_import_never_awaits",
    )
    .await;

    let conn = migrated_db().await;
    let born = seed("import").replace("'validating'", "'rejected'");
    must_be_rejected(&conn, &born, "born validating").await;
}

/// **An import can never park awaiting approval** (D-137): draft-plane authoring
/// is never material, so the record would be waiting for a decision nothing can
/// produce. The one edge of §4 whose violation strands a run forever, and the
/// reason it is a `CHECK` rather than a caller's convention.
#[tokio::test]
async fn an_import_cannot_await_an_approval_that_can_never_come() {
    let conn = migrated_db().await;
    must_succeed(&conn, &seed("import")).await;
    // By name, for the sibling case's reason twenty lines up: four constraints sit
    // on this table and only one of them is D-137's, so a bare `CHECK` is a
    // discriminator every one of them carries and a rewrite that let a different
    // one answer would stay green here.
    must_be_rejected(
        &conn,
        &move_to("awaiting_approval"),
        "chk_pricing_bulk_operation_import_never_awaits",
    )
    .await;

    // And the same state is legal for the kind that can actually be material.
    let conn = migrated_db().await;
    must_succeed(&conn, &seed("repricing")).await;
    must_succeed(&conn, &move_to("awaiting_approval")).await;
}

/// The four terminal states are terminal: a record that left one would re-drive
/// a commit whose rows are already applied.
#[tokio::test]
async fn a_terminal_run_never_moves_again() {
    for terminal in ["completed", "completed_with_conflicts"] {
        let conn = migrated_db().await;
        must_succeed(&conn, &seed("repricing")).await;
        must_succeed(&conn, &move_to("committing")).await;
        must_succeed(&conn, &move_to(terminal)).await;
        must_be_rejected(&conn, &move_to("committing"), "not an edge").await;
    }

    let conn = migrated_db().await;
    must_succeed(&conn, &seed("import")).await;
    must_succeed(&conn, &move_to("validation_failed")).await;
    must_be_rejected(&conn, &move_to("committing"), "not an edge").await;
}

/// Skipping the machine is refused, not merely discouraged: `validating`
/// straight to `completed` is a run that reports outcomes for rows it never
/// committed.
#[tokio::test]
async fn a_state_may_not_be_skipped() {
    let conn = migrated_db().await;
    must_succeed(&conn, &seed("repricing")).await;
    must_be_rejected(&conn, &move_to("completed"), "not an edge").await;
}

/// Identity and provenance are frozen; only the run's progress moves.
///
/// Every disjunct of `trg_pricing_bulk_operation_frozen_columns` is executed here;
/// the trigger is the roster and this loop is measured against it, so no count is
/// restated in prose beside it.
///
/// The behavioural half matters most for `request_hash`: the replay refuses a body
/// that does not match this column, so a writer able to move it could relicense a
/// spent client key onto a batch the operator never submitted — and the whitelist
/// census two files over is a **text** assertion, which cannot tell a column named
/// in the trigger from one that refuses.
#[tokio::test]
async fn a_runs_identity_and_provenance_are_frozen() {
    let conn = migrated_db().await;
    must_succeed(&conn, &seed("import")).await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_bulk_operation SET kind = 'repricing' WHERE operation_id = '{OP}'"
        ),
        "frozen",
    )
    .await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_bulk_operation SET client_key = 'ck-2' WHERE operation_id = '{OP}'"
        ),
        "frozen",
    )
    .await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_bulk_operation SET request_hash = X'00' WHERE operation_id = '{OP}'"
        ),
        "frozen",
    )
    .await;
    // The rest of the trigger's disjuncts, each executed. Four of them had no
    // refusal on either engine, so a `WHEN` clause that lost one left a submitted
    // run's tenancy or its submitter rewritable with this suite green -- and the
    // whitelist census two files over is a text assertion, which cannot tell a
    // column named in the trigger from one that refuses.
    for (column, value) in [
        ("operation_id", "'00000000-0000-0000-0000-0000000000ff'"),
        ("tenant_id", "'00000000-0000-0000-0000-0000000000fe'"),
        ("submitted_by", "'00000000-0000-0000-0000-0000000000fd'"),
        ("submitted_at", "'2099-01-01T00:00:00.000Z'"),
    ] {
        must_be_rejected(
            &conn,
            &format!(
                "UPDATE pricing_bulk_operation SET {column} = {value} WHERE operation_id = '{OP}'"
            ),
            "frozen",
        )
        .await;
    }
    // The positive control every refusal above rests on: the one column of this
    // group that a caller may still write is `report`, and the same shape of
    // statement lands on it. Without this a trigger refusing **every** UPDATE
    // would satisfy all of them.
    must_succeed(
        &conn,
        &format!("UPDATE pricing_bulk_operation SET report = '{{\"rows\":[]}}' WHERE operation_id = '{OP}'"),
    )
    .await;
}

/// The column reads back **empty** for a row that did not
/// name it, which is the whole of the backfill.
///
/// `NOT NULL DEFAULT X''` rather than a nullable column and an `UPDATE`: the
/// digest of a request nobody stored cannot be derived, so every run that predates
/// the column has to read back the one value the replay treats as "unverifiable",
/// and it has to be a value rather than `NULL` — a reader that refuses `NULL`
/// wedged a frontier in this gear permanently once. The `seed` above names eight
/// columns and not this one, so this case is that path exactly.
#[tokio::test]
async fn a_run_that_names_no_digest_reads_back_the_unverifiable_one() {
    let conn = migrated_db().await;
    must_succeed(&conn, &seed("import")).await;
    let length = common::scalar(
        &conn,
        &format!(
            "SELECT CAST(length(request_hash) AS text) AS v FROM pricing_bulk_operation \
             WHERE operation_id = '{OP}'"
        ),
    )
    .await;
    assert_eq!(
        length, "0",
        "a run opened without a digest holds the empty one, not NULL, and not a \
         fabricated value a replay would compare against"
    );
}

/// A run is a record, not a draft: `DELETE` is refused in every state.
#[tokio::test]
async fn a_run_is_never_deleted() {
    let conn = migrated_db().await;
    must_succeed(&conn, &seed("import")).await;
    must_be_rejected(
        &conn,
        &format!("DELETE FROM pricing_bulk_operation WHERE operation_id = '{OP}'"),
        "not permitted",
    )
    .await;
}

/// **`kind` is §4's two flows and nothing else.**
///
/// The rule fails **open**: nothing in this suite writes a third token, so a
/// `CHECK` that stopped refusing would break no path here at all. What it would
/// break is the read — `bulk_repo` maps a row whose `kind` it cannot parse to
/// `CorruptRow`, so the run answers a 500 instead of a report, and the report is
/// the operator's record of money that moved.
///
/// Driven at `INSERT` with the state left at `validating`: the born-validating
/// trigger is strictly narrower and answers ahead of every `CHECK` on this
/// table, so a case that varied the state as well would be reading the trigger's
/// refusal (D-261's shadowing). `UPDATE` is closed to it in any case — `kind` is
/// one of the six frozen columns.
#[tokio::test]
async fn a_kind_outside_the_two_flows_is_refused() {
    let conn = migrated_db().await;
    must_be_rejected(&conn, &seed("migration"), "chk_pricing_bulk_operation_kind").await;
    // The near miss, so the constraint is shown to be about the vocabulary and
    // not merely about the column being non-empty.
    must_be_rejected(&conn, &seed("Import"), "chk_pricing_bulk_operation_kind").await;
    // The same statement with only `kind` changed lands, which is what makes the
    // two refusals above facts about this constraint rather than about the
    // fixture: with it dropped, both would have landed too.
    must_succeed(&conn, &seed("import")).await;
}

/// O4: one operation per client key per tenant.
#[tokio::test]
async fn a_client_key_is_idempotent_within_a_tenant() {
    let conn = migrated_db().await;
    must_succeed(&conn, &seed("import")).await;
    let second = seed("import").replace(OP, "44444444-4444-4444-4444-444444444444");
    must_be_rejected(&conn, &second, "UNIQUE").await;
}

/// **A run is born `validating` and in no other state** — §4's initial state,
/// enforced at `INSERT` rather than assumed of the caller.
///
/// Without this arm the whole machine is a rule about `UPDATE` with a row free to
/// be born past it: born `committing` skips the approval gate transitions 2 and 3
/// exist to impose, and born `completed` reports outcomes for rows it never
/// committed — the very move `a_terminal_run_never_moves_again` proves is refused
/// as an update. `pricing_approval` carries the same arm for the same reason.
///
/// Found by review; every case above seeded `validating` and so could not see it.
#[tokio::test]
async fn a_run_cannot_be_born_past_the_state_machine() {
    for state in [
        "committing",
        "awaiting_approval",
        "completed",
        "completed_with_conflicts",
        "validation_failed",
        "rejected",
    ] {
        let conn = migrated_db().await;
        let born = seed("repricing").replace("'validating'", &format!("'{state}'"));
        must_be_rejected(&conn, &born, "born validating").await;
    }
}
