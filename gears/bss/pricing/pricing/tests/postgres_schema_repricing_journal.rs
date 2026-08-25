//! Slice 12's repricing journal **on the engine that runs in production**
//! (`design/12-operator-efficiency.md` §6, `inst-mr-journal`, O3).
//!
//! `sqlite_repricing_journal_store` proves the same rules against the mirror,
//! and that is not the same thing: the two arms are written separately — one
//! PL/pgSQL function against four `RAISE(ABORT, …)` triggers — and only the
//! `SQLite` side carries a trigger-**body** digest census. D-260 paid this debt
//! for `pricing_bulk_operation` and `pricing_composite_meter` after finding that
//! a lost disjunct on the shipping engine was invisible to every gate; a new
//! table owes a behavioural Postgres suite from the start, not four roster lines.
//!
//! Run with:
//! `cargo test -p cf-gears-bss-pricing --test postgres_schema_repricing_journal -- --ignored`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod pg_support;

use pg_support::Pg;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

const RUN: &str = "11111111-1111-1111-1111-111111111111";
const TENANT: &str = "22222222-2222-2222-2222-222222222222";
const ACTOR: &str = "33333333-3333-3333-3333-333333333333";
const PRICE: &str = "44444444-4444-4444-4444-444444444444";
const SUCCESSOR: &str = "55555555-5555-5555-5555-555555555555";
const PLAN: &str = "77777777-7777-7777-7777-777777777777";
const PHASE_A: &str = "88888888-8888-8888-8888-888888888888";
const PHASE_B: &str = "99999999-9999-9999-9999-999999999999";

async fn applied() -> DatabaseConnection {
    Pg::applied().await.raw().await
}

async fn exec(conn: &DatabaseConnection, sql: &str) -> Result<(), sea_orm::DbErr> {
    conn.execute_raw(Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        sql.to_owned(),
    ))
    .await
    .map(|_| ())
}

async fn must_succeed(conn: &DatabaseConnection, sql: &str) {
    exec(conn, sql)
        .await
        .unwrap_or_else(|e| panic!("statement must succeed: {sql}\n{e}"));
}

async fn must_be_rejected(conn: &DatabaseConnection, sql: &str, by: &str) {
    let Err(err) = exec(conn, sql).await else {
        panic!("this statement must be rejected: {sql}");
    };
    let message = err.to_string();
    assert!(
        message.contains(by),
        "the rejection must be the one under test (`{by}`), got: {message}"
    );
}

fn seed_run() -> String {
    seed_run_of_kind("repricing")
}

fn seed_run_of_kind(kind: &str) -> String {
    format!(
        "INSERT INTO bss.pricing_bulk_operation \
         (operation_id, tenant_id, kind, state, client_key, report, submitted_by, submitted_at) \
         VALUES ('{RUN}', '{TENANT}', '{kind}', 'validating', 'ck-1', '{{}}'::jsonb, \
         '{ACTOR}', now())"
    )
}

/// The selected row and the successor an apply would produce — real
/// `pricing_price` rows, because the journal keys both.
fn seed_price(id: &str) -> String {
    // Distinct phases so the two do not collide on the published-plane scope-key
    // index, which admits one current row per canonical key.
    let phase = if id == SUCCESSOR { PHASE_B } else { PHASE_A };
    format!(
        "INSERT INTO bss.pricing_price ( \
             price_id, tenant_id, plan_id, currency, region, phase, \
             charge_kind, amount_minor, model_kind, lifecycle_state, \
             created_by, created_at_utc) \
         VALUES ('{id}', '{TENANT}', '{PLAN}', 'EUR', 'eu', '{phase}', \
             'recurring', 1000, 'flat', 'published', '{ACTOR}', now())"
    )
}

/// Everything a journal row's three keys name.
async fn seeded() -> DatabaseConnection {
    let conn = applied().await;
    must_succeed(&conn, &seed_price(PRICE)).await;
    must_succeed(&conn, &seed_price(SUCCESSOR)).await;
    must_succeed(&conn, &seed_run()).await;
    conn
}

fn seed_row() -> String {
    format!(
        "INSERT INTO bss.pricing_repricing_journal (run_id, price_id, tenant_id, state) \
         VALUES ('{RUN}', '{PRICE}', '{TENANT}', 'pending')"
    )
}

fn decide(state: &str) -> String {
    let columns = match state {
        "applied" => format!("applied_price_id = '{SUCCESSOR}', applied_at = now()"),
        "failed" => "failure_reason = 'the plan-level pass refused this plan'".to_owned(),
        _ => "applied_price_id = NULL, applied_at = NULL, failure_reason = NULL".to_owned(),
    };
    format!(
        "UPDATE bss.pricing_repricing_journal SET state = '{state}', {columns} \
         WHERE run_id = '{RUN}' AND price_id = '{PRICE}'"
    )
}

async fn journalled() -> DatabaseConnection {
    let conn = seeded().await;
    must_succeed(&conn, &seed_row()).await;
    conn
}

/// Both edges out of `pending` are walkable on Postgres too. A guard written too
/// tight here would leave a run unable to journal one of its two outcomes, and —
/// the journal being what the run's completion predicate reads — unable to finish.
#[tokio::test]
#[ignore = "requires Docker"]
async fn both_sanctioned_outcomes_are_reachable() {
    for state in ["applied", "failed"] {
        // A fresh database per outcome: the row is undeletable and final once
        // decided, so the two cannot share one.
        let conn = journalled().await;
        must_succeed(&conn, &decide(state)).await;
    }
}

/// A journal row is born `pending` and in no other state — the arm whose absence
/// is silent, here on the engine where that silence would ship. A row born
/// `applied` is a row the re-drive skips: a completed run, an untouched price,
/// and nothing anywhere that disagrees.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_journal_row_cannot_be_born_decided() {
    let conn = seeded().await;
    for born in [
        format!(
            "INSERT INTO bss.pricing_repricing_journal \
             (run_id, price_id, tenant_id, state, applied_price_id, applied_at) \
             VALUES ('{RUN}', '{PRICE}', '{TENANT}', 'applied', '{SUCCESSOR}', now())"
        ),
        format!(
            "INSERT INTO bss.pricing_repricing_journal \
             (run_id, price_id, tenant_id, state, failure_reason) \
             VALUES ('{RUN}', '{PRICE}', '{TENANT}', 'failed', 'never actually tried')"
        ),
    ] {
        must_be_rejected(&conn, &born, "born pending").await;
    }
}

/// A decided row never moves again, outcome columns included — so an `applied`
/// row cannot be made to name a successor other than the one that committed.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_decided_row_never_moves_again() {
    let conn = journalled().await;
    must_succeed(&conn, &decide("applied")).await;
    must_be_rejected(&conn, &decide("failed"), "never moves again").await;
    must_be_rejected(&conn, &decide("pending"), "never moves again").await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_repricing_journal SET applied_price_id = '{TENANT}' \
             WHERE run_id = '{RUN}' AND price_id = '{PRICE}'"
        ),
        "never moves again",
    )
    .await;

    let conn = journalled().await;
    must_succeed(&conn, &decide("failed")).await;
    must_be_rejected(&conn, &decide("applied"), "never moves again").await;
}

/// The key and its tenant are frozen, and `DELETE` is refused: a missing journal
/// row is a row the re-drive re-applies.
#[tokio::test]
#[ignore = "requires Docker"]
async fn a_journal_row_is_keyed_frozen_and_undeletable() {
    let conn = journalled().await;
    for column in ["run_id", "price_id", "tenant_id"] {
        must_be_rejected(
            &conn,
            &format!(
                "UPDATE bss.pricing_repricing_journal SET {column} = '{SUCCESSOR}' \
                 WHERE run_id = '{RUN}' AND price_id = '{PRICE}'"
            ),
            "key is frozen",
        )
        .await;
    }
    must_be_rejected(
        &conn,
        &format!(
            "DELETE FROM bss.pricing_repricing_journal \
             WHERE run_id = '{RUN}' AND price_id = '{PRICE}'"
        ),
        "not permitted",
    )
    .await;
}

/// The outcome columns agree with the state in both directions, the vocabulary
/// is closed, and a successor may not wear the selected row's own id
/// (`inst-mp-standard`: bulk never mutates in place). Asserted by constraint
/// **name**, which is what a `CHECK` census cannot do and what tells a lost
/// constraint from a differently-worded one.
#[tokio::test]
#[ignore = "requires Docker"]
async fn the_outcome_columns_agree_with_the_state() {
    let conn = seeded().await;

    for (born, constraint) in [
        (
            format!(
                "INSERT INTO bss.pricing_repricing_journal \
                 (run_id, price_id, tenant_id, state, applied_price_id, applied_at) \
                 VALUES ('{RUN}', '{PRICE}', '{TENANT}', 'pending', '{SUCCESSOR}', now())"
            ),
            "chk_pricing_repricing_journal_applied",
        ),
        (
            // Naming a successor with **no instant** — the half-set shape the
            // pair-wise spelling of this `CHECK` would have admitted, and the one
            // the re-drive would apply a second time.
            format!(
                "INSERT INTO bss.pricing_repricing_journal \
                 (run_id, price_id, tenant_id, state, applied_price_id) \
                 VALUES ('{RUN}', '{PRICE}', '{TENANT}', 'pending', '{SUCCESSOR}')"
            ),
            "chk_pricing_repricing_journal_applied",
        ),
        (
            format!(
                "INSERT INTO bss.pricing_repricing_journal \
                 (run_id, price_id, tenant_id, state, failure_reason) \
                 VALUES ('{RUN}', '{PRICE}', '{TENANT}', 'pending', 'refused before it began')"
            ),
            "chk_pricing_repricing_journal_failed",
        ),
        (
            // The mirror half-set: an instant with no successor to point at.
            format!(
                "INSERT INTO bss.pricing_repricing_journal \
                 (run_id, price_id, tenant_id, state, applied_at) \
                 VALUES ('{RUN}', '{PRICE}', '{TENANT}', 'pending', now())"
            ),
            "chk_pricing_repricing_journal_applied",
        ),
        (
            // A row carrying both states' columns at once.
            format!(
                "INSERT INTO bss.pricing_repricing_journal \
                 (run_id, price_id, tenant_id, state, applied_price_id, applied_at, failure_reason) \
                 VALUES ('{RUN}', '{PRICE}', '{TENANT}', 'pending', '{SUCCESSOR}', now(), 'both')"
            ),
            "chk_pricing_repricing_journal_applied",
        ),
    ] {
        must_be_rejected(&conn, &born, constraint).await;
    }

    must_succeed(&conn, &seed_row()).await;
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_repricing_journal \
             SET state = 'applied', applied_price_id = '{PRICE}', applied_at = now() \
             WHERE run_id = '{RUN}' AND price_id = '{PRICE}'"
        ),
        "chk_pricing_repricing_journal_successor_is_new",
    )
    .await;
    // The same two agreements arriving as an **update** out of `pending`. Both
    // were reachable on this engine through the insert path alone until now, so a
    // constraint that stopped holding for the verb the apply loop actually uses
    // would have been invisible here.
    for (update, constraint) in [
        (
            "SET state = 'applied'",
            "chk_pricing_repricing_journal_applied",
        ),
        (
            "SET state = 'failed'",
            "chk_pricing_repricing_journal_failed",
        ),
        (
            "SET state = 'applied', applied_at = now()",
            "chk_pricing_repricing_journal_applied",
        ),
    ] {
        must_be_rejected(
            &conn,
            &format!(
                "UPDATE bss.pricing_repricing_journal {update} \
                 WHERE run_id = '{RUN}' AND price_id = '{PRICE}'"
            ),
            constraint,
        )
        .await;
    }
    // The vocabulary is closed — asserted **only** as an update, because at
    // `INSERT` the born-pending arm is strictly narrower than this `CHECK` and
    // raises first, so a case expecting the constraint there would be reading the
    // trigger's refusal instead. `not-attempted` is the word `inst-bs-abort` uses
    // for an aborted run's uncommitted rows and is deliberately not a state this
    // table holds.
    must_be_rejected(
        &conn,
        &format!(
            "UPDATE bss.pricing_repricing_journal SET state = 'not-attempted' \
             WHERE run_id = '{RUN}' AND price_id = '{PRICE}'"
        ),
        "chk_pricing_repricing_journal_state",
    )
    .await;
}

/// All three keys, each asserted by **name** — which is what Postgres gives and
/// the `SQLite` mirror cannot, its key violation being the bare string
/// `FOREIGN KEY constraint failed`.
///
/// The run key is observable, unlike `pricing_composite_meter`'s parent key,
/// because both arms that read the parent defer when the run is absent: a `BEFORE`
/// trigger answers ahead of the key on this engine, so an arm that raised there
/// would shadow it and report a fault the caller does not have.
#[tokio::test]
#[ignore = "requires Docker"]
async fn every_key_names_a_row_that_exists() {
    let conn = applied().await;
    must_succeed(&conn, &seed_price(PRICE)).await;
    must_be_rejected(&conn, &seed_row(), "fk_pricing_repricing_journal_run").await;

    let conn = applied().await;
    must_succeed(&conn, &seed_run()).await;
    must_be_rejected(&conn, &seed_row(), "fk_pricing_repricing_journal_price").await;

    // The successor's key is reachable only at the apply: the column is null in
    // every other state.
    let conn = applied().await;
    must_succeed(&conn, &seed_price(PRICE)).await;
    must_succeed(&conn, &seed_run()).await;
    must_succeed(&conn, &seed_row()).await;
    must_be_rejected(
        &conn,
        &decide("applied"),
        "fk_pricing_repricing_journal_applied_price",
    )
    .await;

    // With every referent present the same statements land, so each refusal above
    // is its own key rather than something else about the statement.
    let conn = journalled().await;
    must_succeed(&conn, &decide("applied")).await;
}

/// **One journal row per `(run_id, price_id)`** — the uniqueness that makes this
/// table an idempotency spine rather than a log. A second row on one pair would
/// let a re-drive read `pending` for a row already `applied` and apply it twice.
///
/// Asserted behaviourally because `EXPECTED_PRIMARY_KEYS` proves the key was
/// *declared* on both engines and nothing proved it refuses.
#[tokio::test]
#[ignore = "requires Docker"]
async fn one_journal_row_per_run_and_price() {
    let conn = journalled().await;
    must_be_rejected(&conn, &seed_row(), "pricing_repricing_journal_pkey").await;
}

/// **Only a repricing run journals here.** A bulk import's per-row outcomes live
/// in the operation's own `report` (`inst-bi-commit`), and nothing drives an
/// import through this table — so a journal row under an import is a record no
/// code will ever complete.
#[tokio::test]
#[ignore = "requires Docker"]
async fn an_import_does_not_journal() {
    let conn = applied().await;
    must_succeed(&conn, &seed_price(PRICE)).await;
    must_succeed(&conn, &seed_run_of_kind("import")).await;
    must_be_rejected(&conn, &seed_row(), "only a repricing run").await;
}
