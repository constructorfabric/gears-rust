//! Shared commercial durability contracts on `PostgreSQL`. Never records goldens.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod acceptance_support;
mod pg_support;
mod plan_support;
mod seam_support;

#[tokio::test]
#[ignore = "requires the PostgreSQL contract harness"]
async fn concurrent_acceptance_has_one_durable_winner() {
    let pg = pg_support::Pg::applied().await;
    let db = toolkit_db::DBProvider::<toolkit_db::DbError>::new(pg.db().await);
    let result = seam_support::run_acceptance_race(db).await;
    assert_eq!(result.stored_acceptances, 1);
    assert_eq!(result.stored_commands, 1);
    assert_eq!(result.receipt_ids[0], result.receipt_ids[1]);
}
mod seam_parity_support;

#[tokio::test]
#[ignore = "requires the PostgreSQL contract harness"]
async fn replay_and_conflicts() {
    let pg = pg_support::Pg::applied().await;
    let db = toolkit_db::DBProvider::<toolkit_db::DbError>::new(pg.db().await);
    let dsn = plan_support::entry_support::TestDsn::of(pg.url(true));
    seam_parity_support::replay_and_conflicts(seam_parity_support::fixture(db, dsn).await).await;
}

#[tokio::test]
#[ignore = "requires the PostgreSQL contract harness"]
async fn authorization_denial() {
    let pg = pg_support::Pg::applied().await;
    let db = toolkit_db::DBProvider::<toolkit_db::DbError>::new(pg.db().await);
    let dsn = plan_support::entry_support::TestDsn::of(pg.url(true));
    seam_parity_support::authorization_denial(seam_parity_support::fixture(db, dsn).await).await;
}

#[tokio::test]
#[ignore = "requires the PostgreSQL contract harness"]
async fn explicit_price_close_race() {
    let pg = pg_support::Pg::applied().await;
    let db = toolkit_db::DBProvider::<toolkit_db::DbError>::new(pg.db().await);
    let dsn = plan_support::entry_support::TestDsn::of(pg.url(true));
    seam_parity_support::explicit_price_close_race(seam_parity_support::fixture(db, dsn).await)
        .await;
}

#[tokio::test]
#[ignore = "requires the PostgreSQL contract harness"]
async fn money_digest_stability() {
    let pg = pg_support::Pg::applied().await;
    let db = toolkit_db::DBProvider::<toolkit_db::DbError>::new(pg.db().await);
    let dsn = plan_support::entry_support::TestDsn::of(pg.url(true));
    seam_parity_support::money_digest_stability(seam_parity_support::fixture(db, dsn).await).await;
}

#[tokio::test]
#[ignore = "requires the PostgreSQL contract harness"]
async fn scheduled_promotion_and_held_policy() {
    let pg = pg_support::Pg::applied().await;
    let db = toolkit_db::DBProvider::<toolkit_db::DbError>::new(pg.db().await);
    let dsn = plan_support::entry_support::TestDsn::of(pg.url(true));
    seam_parity_support::scheduled_promotion_and_held_policy(
        seam_parity_support::fixture(db, dsn).await,
    )
    .await;
}

#[test]
#[ignore = "requires the PostgreSQL contract harness"]
fn crash_and_response_loss_reopen_database() {
    let pg = seam_parity_support::runtime().block_on(pg_support::Pg::applied());
    let dsn = plan_support::entry_support::TestDsn::of(pg.url(true));
    seam_parity_support::restart(&dsn);
}

mod schema_dump;

#[tokio::test]
#[ignore = "requires the PostgreSQL contract harness"]
async fn concurrent_entry_policies() {
    let pg = pg_support::Pg::applied().await;
    let db = toolkit_db::DBProvider::<toolkit_db::DbError>::new(pg.db().await);
    let dsn = plan_support::entry_support::TestDsn::of(pg.url(true));
    seam_parity_support::concurrent_entry_policies(db, dsn).await;
}

#[tokio::test]
#[ignore = "requires the PostgreSQL contract harness"]
async fn entry_write_contention_still_confirms() {
    let pg = pg_support::Pg::applied().await;
    let db = toolkit_db::DBProvider::<toolkit_db::DbError>::new(pg.db().await);
    let dsn = plan_support::entry_support::TestDsn::of(pg.url(true));
    seam_parity_support::entry_write_contention_still_confirms(db, dsn).await;
}

#[tokio::test]
#[ignore = "requires the PostgreSQL contract harness"]
async fn confirm_contention_still_confirms() {
    let pg = pg_support::Pg::applied().await;
    let db = toolkit_db::DBProvider::<toolkit_db::DbError>::new(pg.db().await);
    let dsn = plan_support::entry_support::TestDsn::of(pg.url(true));
    seam_parity_support::confirm_contention_still_confirms(db, dsn).await;
}

#[tokio::test]
#[ignore = "requires the PostgreSQL contract harness"]
async fn phase9_upgrade_and_fresh_install() {
    let pg = pg_support::Pg::empty().await;
    let db = toolkit_db::DBProvider::<toolkit_db::DbError>::new(pg.db().await);
    let dsn = plan_support::entry_support::TestDsn::of(pg.url(true));
    let fresh = pg_support::Pg::applied().await;
    let fresh = plan_support::entry_support::TestDsn::of(fresh.url(true));
    seam_parity_support::migration::upgrade(db, dsn, fresh).await;
}
