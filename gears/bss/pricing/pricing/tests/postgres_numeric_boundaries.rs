//! Pricing's numeric contract at its boundaries, through the production router on `PostgreSQL`.
//! The `SQLite` twin, `numeric_boundaries.rs`, maps each row of the boundary table to its walk.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod acceptance_support;
mod numeric_boundary_support;
mod pg_support;
mod plan_support;
mod seam_support;

use numeric_boundary_support as walk;

/// One database of the shared server with the chain applied, and its DSN.
async fn database() -> (
    toolkit_db::DBProvider<toolkit_db::DbError>,
    plan_support::entry_support::TestDsn,
    pg_support::Pg,
) {
    let pg = pg_support::Pg::applied().await;
    let db = toolkit_db::DBProvider::<toolkit_db::DbError>::new(pg.db().await);
    let dsn = plan_support::entry_support::TestDsn::of(pg.url(true));
    (db, dsn, pg)
}

#[tokio::test]
#[ignore = "requires the PostgreSQL contract harness"]
async fn refused_inputs_write_nothing() {
    let (db, dsn, _pg) = database().await;
    walk::refused_inputs_write_nothing(db, dsn).await;
}

#[tokio::test]
#[ignore = "requires the PostgreSQL contract harness"]
async fn accepted_values_round_trip_exactly() {
    let (db, dsn, _pg) = database().await;
    walk::accepted_values_round_trip_exactly(db, dsn).await;
}

#[tokio::test]
#[ignore = "requires the PostgreSQL contract harness"]
async fn a_currency_table_change_keeps_history() {
    let (db, dsn, _pg) = database().await;
    walk::a_currency_table_change_keeps_history(db, dsn).await;
}

#[tokio::test]
#[ignore = "requires the PostgreSQL contract harness"]
async fn a_receipt_keeps_its_snapshot() {
    let (db, dsn, _pg) = database().await;
    walk::a_receipt_keeps_its_snapshot(db, dsn).await;
}

#[tokio::test]
#[ignore = "requires the PostgreSQL contract harness"]
async fn a_corrupt_row_is_refused_and_never_repaired() {
    let (db, dsn, _pg) = database().await;
    walk::a_corrupt_row_is_refused_and_never_repaired(db, dsn).await;
}
