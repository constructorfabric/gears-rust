//! Commercial parity contracts on persisted `SQLite`.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod acceptance_support;
mod plan_support;
mod seam_parity_support;
mod seam_support;

#[tokio::test]
async fn replay_and_conflicts() {
    let (db, _, _, dsn) = plan_support::entry_support::test_db().await;
    seam_parity_support::replay_and_conflicts(seam_parity_support::fixture(db, dsn).await).await;
}

#[tokio::test]
async fn authorization_denial() {
    let (db, _, _, dsn) = plan_support::entry_support::test_db().await;
    seam_parity_support::authorization_denial(seam_parity_support::fixture(db, dsn).await).await;
}

#[tokio::test]
async fn explicit_price_close_race() {
    let (db, _, _, dsn) = plan_support::entry_support::test_db().await;
    seam_parity_support::explicit_price_close_race(seam_parity_support::fixture(db, dsn).await)
        .await;
}

#[tokio::test]
async fn money_digest_stability() {
    let (db, _, _, dsn) = plan_support::entry_support::test_db().await;
    seam_parity_support::money_digest_stability(seam_parity_support::fixture(db, dsn).await).await;
}

#[tokio::test]
async fn scheduled_promotion_and_held_policy() {
    let (db, _, _, dsn) = plan_support::entry_support::test_db().await;
    seam_parity_support::scheduled_promotion_and_held_policy(
        seam_parity_support::fixture(db, dsn).await,
    )
    .await;
}

#[tokio::test]
async fn concurrent_acceptance_has_one_durable_winner() {
    let (db, _, _, _dsn) = plan_support::entry_support::test_db().await;
    let result = seam_support::run_acceptance_race(db).await;
    assert_eq!(result.stored_acceptances, 1);
    assert_eq!(result.stored_commands, 1);
    assert_eq!(result.receipt_ids[0], result.receipt_ids[1]);
}

#[test]
fn crash_and_response_loss_reopen_database() {
    let (_, _, _, dsn) =
        seam_parity_support::runtime().block_on(plan_support::entry_support::test_db());
    seam_parity_support::restart(&dsn);
}

mod schema_dump;

#[tokio::test]
async fn concurrent_entry_policies() {
    let (db, _, _, dsn) = plan_support::entry_support::test_db().await;
    seam_parity_support::concurrent_entry_policies(db, dsn).await;
}

#[tokio::test]
async fn entry_write_contention_still_confirms() {
    let (db, _, _, dsn) = plan_support::entry_support::test_db().await;
    seam_parity_support::entry_write_contention_still_confirms(db, dsn).await;
}

#[tokio::test]
async fn confirm_contention_still_confirms() {
    let (db, _, _, dsn) = plan_support::entry_support::test_db().await;
    seam_parity_support::confirm_contention_still_confirms(db, dsn).await;
}

#[tokio::test]
async fn phase9_upgrade_and_fresh_install() {
    let dsn = plan_support::entry_support::TestDsn::new("pricing-phase9-");
    let db = seam_parity_support::open(&dsn).await;
    let (_, _, _, fresh) = plan_support::entry_support::test_db().await;
    seam_parity_support::migration::upgrade(db, dsn, fresh).await;
}

#[path = "review_fix/sqlite_schema.rs"]
mod review_schema;
