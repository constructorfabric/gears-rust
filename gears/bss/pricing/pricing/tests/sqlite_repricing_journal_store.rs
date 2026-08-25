//! What the **schema** refuses of a repricing journal row — Slice 12's
//! idempotency spine (`design/12-operator-efficiency.md` §6, `inst-mr-journal`,
//! `inst-mp-journal`, O3), driven as raw SQL because what is under test is the
//! store's own guarantee and not any repository's use of it.
//!
//! The journal is what makes a crashed run safe to re-drive, so every rule here
//! is about one of the two ways a re-drive can go wrong: applying a row twice,
//! or skipping a row that was never applied. The second is the quiet one — a
//! skipped row leaves a completed run, an untouched price, and no state anywhere
//! that disagrees.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use sea_orm::DatabaseConnection;

mod common;

use common::{exec, migrated_db, must_succeed};

const RUN: &str = "11111111-1111-1111-1111-111111111111";
const TENANT: &str = "22222222-2222-2222-2222-222222222222";
const ACTOR: &str = "33333333-3333-3333-3333-333333333333";
const PRICE: &str = "44444444-4444-4444-4444-444444444444";
const SUCCESSOR: &str = "55555555-5555-5555-5555-555555555555";
const PLAN: &str = "77777777-7777-7777-7777-777777777777";
const PHASE_A: &str = "88888888-8888-8888-8888-888888888888";
const PHASE_B: &str = "99999999-9999-9999-9999-999999999999";

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

/// The run the journal rows belong to, in its own initial state.
fn seed_run() -> String {
    seed_run_of_kind("repricing")
}

fn seed_run_of_kind(kind: &str) -> String {
    format!(
        "INSERT INTO pricing_bulk_operation \
         (operation_id, tenant_id, kind, state, client_key, report, submitted_by, submitted_at) \
         VALUES ('{RUN}', '{TENANT}', '{kind}', 'validating', 'ck-1', '{{}}', '{ACTOR}', \
         '2026-08-08T00:00:00Z')"
    )
}

/// The selected row and the successor an apply would produce.
///
/// Both are real `pricing_price` rows because the journal keys them, exactly as
/// `pricing_price_tier_band` and `pricing_price_window` key theirs.
fn seed_price(id: &str) -> String {
    // The phase is derived from the row id so the two rows this suite seeds do
    // not collide on `uq_pricing_price_scope_key_current`, which admits one
    // published row per canonical scope key.
    let phase = if id == SUCCESSOR { PHASE_B } else { PHASE_A };
    format!(
        "INSERT INTO pricing_price ( \
             price_id, tenant_id, plan_id, currency, region, phase, \
             charge_kind, amount_minor, model_kind, lifecycle_state, \
             created_by, created_at_utc) \
         VALUES ('{id}', '{TENANT}', '{PLAN}', 'EUR', 'eu', '{phase}', \
             'recurring', 1000, 'flat', 'published', '{ACTOR}', \
             '2026-08-08T00:00:00Z')"
    )
}

/// One selected row, `pending` — the only state a journal row may be born in.
fn seed_row() -> String {
    format!(
        "INSERT INTO pricing_repricing_journal (run_id, price_id, tenant_id, state) \
         VALUES ('{RUN}', '{PRICE}', '{TENANT}', 'pending')"
    )
}

/// A journalled outcome, with the columns that state owes.
fn decide(state: &str) -> String {
    let columns = match state {
        "applied" => {
            format!("applied_price_id = '{SUCCESSOR}', applied_at = '2026-08-08T01:00:00Z'")
        }
        "failed" => "failure_reason = 'the plan-level pass refused this plan'".to_owned(),
        _ => "applied_price_id = NULL, applied_at = NULL, failure_reason = NULL".to_owned(),
    };
    format!(
        "UPDATE pricing_repricing_journal SET state = '{state}', {columns} \
         WHERE run_id = '{RUN}' AND price_id = '{PRICE}'"
    )
}

async fn journalled(conn: &DatabaseConnection) {
    seeded(conn).await;
    must_succeed(conn, &seed_row()).await;
}

/// The world a journal row needs before it can exist: its run and both price
/// rows its two keys name.
async fn seeded(conn: &DatabaseConnection) {
    must_succeed(conn, &seed_price(PRICE)).await;
    must_succeed(conn, &seed_price(SUCCESSOR)).await;
    must_succeed(conn, &seed_run()).await;
}

/// Both edges out of `pending` are walkable — the whitelist half, and the case
/// that would catch a guard written too tight. A journal that refused `failed`
/// would leave the run's own state machine unable to reach either terminal state.
#[tokio::test]
async fn both_sanctioned_outcomes_are_reachable() {
    for state in ["applied", "failed"] {
        let conn = migrated_db().await;
        journalled(&conn).await;
        must_succeed(&conn, &decide(state)).await;
    }
}

/// **A journal row is born `pending` and in no other state.**
///
/// This is the arm whose absence is silent. `pricing_bulk_operation` shipped without
/// its `INSERT` arm for one commit and a run could be born past its own machine;
/// here the same hole means a row born `applied` is a row the re-drive **skips** —
/// the operator sees a completed run, the price was never repriced, and no state
/// anywhere disagrees. Every other case in this file seeds `pending`, so without
/// this one the whole green suite could not see it.
#[tokio::test]
async fn a_journal_row_cannot_be_born_decided() {
    for born in [
        format!(
            "INSERT INTO pricing_repricing_journal \
             (run_id, price_id, tenant_id, state, applied_price_id, applied_at) \
             VALUES ('{RUN}', '{PRICE}', '{TENANT}', 'applied', '{SUCCESSOR}', \
             '2026-08-08T01:00:00Z')"
        ),
        format!(
            "INSERT INTO pricing_repricing_journal \
             (run_id, price_id, tenant_id, state, failure_reason) \
             VALUES ('{RUN}', '{PRICE}', '{TENANT}', 'failed', 'never actually tried')"
        ),
    ] {
        let conn = migrated_db().await;
        seeded(&conn).await;
        must_be_rejected(&conn, &born, "born pending").await;
    }
}

/// A decided row never moves again — and that one guard also freezes the outcome
/// columns, so an `applied` row cannot later be made to name a different
/// successor than the one that actually committed.
#[tokio::test]
async fn a_decided_row_never_moves_again() {
    for (first, second) in [("applied", "failed"), ("failed", "applied")] {
        let conn = migrated_db().await;
        journalled(&conn).await;
        must_succeed(&conn, &decide(first)).await;
        must_be_rejected(&conn, &decide(second), "never moves again").await;
    }

    // Back to `pending` is the re-drive's own nightmare: a second apply against a
    // row that already has a successor.
    let conn = migrated_db().await;
    journalled(&conn).await;
    must_succeed(&conn, &decide("applied")).await;
    must_be_rejected(&conn, &decide("pending"), "never moves again").await;

    // And the successor cannot be rewritten in place while the state stands.
    let conn = migrated_db().await;
    journalled(&conn).await;
    must_succeed(&conn, &decide("applied")).await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_repricing_journal SET applied_price_id = '{TENANT}' \
             WHERE run_id = '{RUN}' AND price_id = '{PRICE}'"
        ),
        "never moves again",
    )
    .await;
}

/// The key and its tenant are what the row is *about*, so they are frozen.
#[tokio::test]
async fn the_journal_key_is_frozen() {
    let conn = migrated_db().await;
    journalled(&conn).await;
    for column in ["run_id", "price_id", "tenant_id"] {
        must_be_rejected(
            &conn,
            &format!(
                "UPDATE pricing_repricing_journal SET {column} = '{SUCCESSOR}' \
                 WHERE run_id = '{RUN}' AND price_id = '{PRICE}'"
            ),
            "key is frozen",
        )
        .await;
    }
}

/// `DELETE` is refused: a missing journal row is a row the re-drive re-applies,
/// which is the one thing O3 promises cannot happen.
#[tokio::test]
async fn a_journal_row_is_never_deleted() {
    let conn = migrated_db().await;
    journalled(&conn).await;
    must_be_rejected(
        &conn,
        &format!(
            "DELETE FROM pricing_repricing_journal WHERE run_id = '{RUN}' AND price_id = '{PRICE}'"
        ),
        "not permitted",
    )
    .await;
}

/// The outcome columns and the state agree in both directions — a `pending` row
/// carrying a successor would be re-driven into a second one, and an `applied`
/// row without one records an apply nothing can point at.
#[tokio::test]
async fn the_outcome_columns_agree_with_the_state() {
    let conn = migrated_db().await;
    seeded(&conn).await;

    for (born, constraint) in [
        // Pending, but already naming a successor.
        (
            format!(
                "INSERT INTO pricing_repricing_journal \
             (run_id, price_id, tenant_id, state, applied_price_id, applied_at) \
             VALUES ('{RUN}', '{PRICE}', '{TENANT}', 'pending', '{SUCCESSOR}', \
             '2026-08-08T01:00:00Z')"
            ),
            "chk_pricing_repricing_journal_applied",
        ),
        // Pending, naming a successor, with **no instant** — the half-set shape
        // the pair-wise spelling of this `CHECK` would have admitted, and the
        // one the re-drive would apply a second time.
        (
            format!(
                "INSERT INTO pricing_repricing_journal \
             (run_id, price_id, tenant_id, state, applied_price_id) \
             VALUES ('{RUN}', '{PRICE}', '{TENANT}', 'pending', '{SUCCESSOR}')"
            ),
            "chk_pricing_repricing_journal_applied",
        ),
        // Pending, but already carrying a reason it failed.
        (
            format!(
                "INSERT INTO pricing_repricing_journal \
             (run_id, price_id, tenant_id, state, failure_reason) \
             VALUES ('{RUN}', '{PRICE}', '{TENANT}', 'pending', 'refused before it began')"
            ),
            "chk_pricing_repricing_journal_failed",
        ),
        // A decided row carrying the *other* state's column.
        (
            format!(
                "INSERT INTO pricing_repricing_journal \
             (run_id, price_id, tenant_id, state, applied_price_id, applied_at, failure_reason) \
             VALUES ('{RUN}', '{PRICE}', '{TENANT}', 'pending', '{SUCCESSOR}', \
             '2026-08-08T01:00:00Z', 'both at once')"
            ),
            "chk_pricing_repricing_journal_applied",
        ),
        // The mirror half-set: an instant with no successor to point at.
        (
            format!(
                "INSERT INTO pricing_repricing_journal \
             (run_id, price_id, tenant_id, state, applied_at) \
             VALUES ('{RUN}', '{PRICE}', '{TENANT}', 'pending', '2026-08-08T01:00:00Z')"
            ),
            "chk_pricing_repricing_journal_applied",
        ),
    ] {
        must_be_rejected(&conn, &born, constraint).await;
    }

    // The same disagreements arriving as an update of a legitimately pending row.
    let conn = migrated_db().await;
    journalled(&conn).await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_repricing_journal SET state = 'applied' \
             WHERE run_id = '{RUN}' AND price_id = '{PRICE}'"
        ),
        "chk_pricing_repricing_journal_applied",
    )
    .await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_repricing_journal SET state = 'failed' \
             WHERE run_id = '{RUN}' AND price_id = '{PRICE}'"
        ),
        "chk_pricing_repricing_journal_failed",
    )
    .await;
    // Applied, with a successor but no instant — the other half-set shape.
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_repricing_journal \
             SET state = 'applied', applied_price_id = '{SUCCESSOR}' \
             WHERE run_id = '{RUN}' AND price_id = '{PRICE}'"
        ),
        "chk_pricing_repricing_journal_applied",
    )
    .await;
    // And the vocabulary is closed. This one is asserted **only** as an update:
    // at `INSERT` the born-pending trigger is strictly narrower than the `CHECK`
    // and answers first, so the constraint is shadowed there and a case
    // expecting it would be reading the trigger's refusal instead — the same
    // ordering `postgres_schema_composite_meter` records for its parent key.
    // `not-attempted` is the word `inst-bs-abort` uses for an aborted run's
    // uncommitted rows, and it is deliberately not a state this table holds.
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_repricing_journal SET state = 'not-attempted' \
             WHERE run_id = '{RUN}' AND price_id = '{PRICE}'"
        ),
        "chk_pricing_repricing_journal_state",
    )
    .await;
}

/// `inst-mp-standard`: an applied row is a **new** immutable row through the
/// Foundation path, because bulk never mutates in place. A successor wearing the
/// selected row's own id would be that mutation, journalled as a supersession.
#[tokio::test]
async fn a_successor_may_not_wear_the_selected_rows_own_id() {
    let conn = migrated_db().await;
    journalled(&conn).await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE pricing_repricing_journal \
             SET state = 'applied', applied_price_id = '{PRICE}', \
                 applied_at = '2026-08-08T01:00:00Z' \
             WHERE run_id = '{RUN}' AND price_id = '{PRICE}'"
        ),
        "chk_pricing_repricing_journal_successor_is_new",
    )
    .await;
}

/// All three keys, each asserted by making its referent absent.
///
/// **No pragma, and the earlier one was decoration resting on a false premise.**
/// `sqlx` puts `foreign_keys = ON` in its default pragma set, so every connection
/// this suite opens already enforces them, and `toolkit_db`'s own pragma parser
/// handles `journal_mode` and `synchronous` only — it never touches this one.
/// `sqlite_window_guards` states the same fact in this directory and asserts its
/// key with no pragma at all. Where `sqlite_bundle_store` *does* set it, the
/// reason is the opposite of the one written here before: it asserts the
/// **absence** of a key with `must_succeed`, and pragma-off would make that
/// vacuous. A `must_be_rejected` fails loudly either way.
///
/// `SQLite` names neither the constraint nor the table in a key violation — the
/// message is the bare `FOREIGN KEY constraint failed` — so the three cases are
/// told apart by which referent each one removes, not by the text.
#[tokio::test]
async fn every_key_names_a_row_that_exists() {
    // The run.
    let conn = migrated_db().await;
    must_succeed(&conn, &seed_price(PRICE)).await;
    must_be_rejected(&conn, &seed_row(), "FOREIGN KEY").await;

    // The selected price row.
    let conn = migrated_db().await;
    must_succeed(&conn, &seed_run()).await;
    must_be_rejected(&conn, &seed_row(), "FOREIGN KEY").await;

    // The successor. Its key is only reachable at the apply, since the column is
    // null in every other state.
    let conn = migrated_db().await;
    must_succeed(&conn, &seed_price(PRICE)).await;
    must_succeed(&conn, &seed_run()).await;
    must_succeed(&conn, &seed_row()).await;
    must_be_rejected(&conn, &decide("applied"), "FOREIGN KEY").await;

    // And with every referent present the same statements land, so each refusal
    // above is its key rather than something else about the statement.
    let conn = migrated_db().await;
    journalled(&conn).await;
    must_succeed(&conn, &decide("applied")).await;
}

/// **One journal row per `(run_id, price_id)`** — the uniqueness that makes this
/// table an idempotency spine rather than a log. A second row on one pair would
/// let a re-drive read `pending` for a row already `applied` and apply it twice,
/// which is the single thing O3 promises cannot happen.
///
/// Asserted behaviourally because `EXPECTED_PRIMARY_KEYS` proves the key was
/// *declared*, on both engines, and nothing proved it refuses.
#[tokio::test]
async fn one_journal_row_per_run_and_price() {
    let conn = migrated_db().await;
    journalled(&conn).await;
    must_be_rejected(&conn, &seed_row(), "UNIQUE").await;
}

/// **Only a repricing run journals here.** A bulk import's per-row outcomes live
/// in the operation's own `report` (`inst-bi-commit`), and nothing drives an
/// import through this table — so a journal row under an import is a record no
/// code will ever complete, and the entity said so in prose while the schema
/// admitted it.
#[tokio::test]
async fn an_import_does_not_journal() {
    let conn = migrated_db().await;
    must_succeed(&conn, &seed_price(PRICE)).await;
    must_succeed(&conn, &seed_price(SUCCESSOR)).await;
    must_succeed(&conn, &seed_run_of_kind("import")).await;
    must_be_rejected(&conn, &seed_row(), "only a repricing run").await;
}
