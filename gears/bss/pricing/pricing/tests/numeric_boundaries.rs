//! Pricing's numeric contract at its boundaries, through the production router on `SQLite`. The
//! walks live in `numeric_boundary_support`; `postgres_numeric_boundaries.rs` runs the same walks
//! on `PostgreSQL`. Each row of the boundary table and the walk that proves it:
//!
//! | Input or operation | Expected behavior | Walk |
//! |---|---|---|
//! | EUR rate `0.047`, scale 2 | Accepted unchanged | `accepted_values_round_trip_exactly` |
//! | EUR rate `0.10` | Accepted, stored as `0.1` | `accepted_values_round_trip_exactly` |
//! | EUR minimum fee `12.340`, scale 2 | Stored and returned as `12.34` | `accepted_values_round_trip_exactly` |
//! | EUR minimum fee `12.341`, scale 2 | `MIN_FEE_INVALID`, no rounding | `refused_inputs_write_nothing` |
//! | JSON number in a price or quantity field | `AMOUNT_INVALID`, string required | `refused_inputs_write_nothing` |
//! | Rate one place past the rate bound | `AMOUNT_OUT_OF_RANGE`, no write | `refused_inputs_write_nothing` |
//! | 29-digit coefficient | `AMOUNT_OUT_OF_RANGE`, no write | `refused_inputs_write_nothing` |
//! | Exponent, sign, whitespace, separator, leading zeros | `AMOUNT_INVALID` | `refused_inputs_write_nothing` |
//! | Largest and smallest accepted values | Exact round trip | `accepted_values_round_trip_exactly` |
//! | Unknown currency at book create | `BOOK_CURRENCY_INVALID` | `refused_inputs_write_nothing` |
//! | Pending price after a table change | `CURRENCY_SCALE_CHANGED`, draft unchanged | `a_currency_table_change_keeps_history` |
//! | Approved price after a table change | Stored scale and value returned | `a_currency_table_change_keeps_history` |
//! | Resolve binding | Carries `currency` and integer `currency_scale` | `a_receipt_keeps_its_snapshot` |
//! | Same value with trailing zeros | Same stored text and digest | `a_currency_table_change_keeps_history` |
//! | Same value, other stored scale | Different money digest | `a_currency_table_change_keeps_history` |
//! | Accepted receipt after a table change | Same snapshot and digests | `a_receipt_keeps_its_snapshot` |
//! | Corrupt stored decimal or metadata | Integrity error, no silent repair | `a_corrupt_row_is_refused_and_never_repaired` |
//!
//! A refused input writes no price, approval unit, outbox event or completed idempotency result
//! (`refused_inputs_write_nothing`).
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod acceptance_support;
mod numeric_boundary_support;
mod plan_support;
mod seam_support;

use numeric_boundary_support as walk;

#[tokio::test]
async fn refused_inputs_write_nothing() {
    let (db, _, _, dsn) = plan_support::entry_support::test_db().await;
    walk::refused_inputs_write_nothing(db, dsn).await;
}

#[tokio::test]
async fn accepted_values_round_trip_exactly() {
    let (db, _, _, dsn) = plan_support::entry_support::test_db().await;
    walk::accepted_values_round_trip_exactly(db, dsn).await;
}

#[tokio::test]
async fn a_currency_table_change_keeps_history() {
    let (db, _, _, dsn) = plan_support::entry_support::test_db().await;
    walk::a_currency_table_change_keeps_history(db, dsn).await;
}

#[tokio::test]
async fn a_receipt_keeps_its_snapshot() {
    let (db, _, _, dsn) = plan_support::entry_support::test_db().await;
    walk::a_receipt_keeps_its_snapshot(db, dsn).await;
}

#[tokio::test]
async fn a_corrupt_row_is_refused_and_never_repaired() {
    let (db, _, _, dsn) = plan_support::entry_support::test_db().await;
    walk::a_corrupt_row_is_refused_and_never_repaired(db, dsn).await;
}
