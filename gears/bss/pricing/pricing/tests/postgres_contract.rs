//! The golden consumer contracts on Postgres (run 4.4): the same body as `contract.rs` runs on
//! `SQLite`, against the same files under `tests/contract/`. This tier only compares — it never
//! re-records — so a contract that holds on one backend and drifts on the other is red here.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#[macro_use]
mod contract_support;
mod pg_support;
mod plan_support;
use plan_support::{
    Catalog, Fixture,
    entry_support::{app_for, state_on, user_of},
};
use std::sync::Arc;
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

async fn check(golden: &str) {
    let pg = pg_support::Pg::applied().await;
    let catalog = Arc::new(Catalog::default());
    let ctx = user_of(Uuid::new_v4());
    let db = DBProvider::<DbError>::new(pg.db().await);
    let state = state_on(db.clone(), catalog.clone()).await;
    let app = app_for(state.clone(), ctx.subject_tenant_id());
    let f = Fixture {
        dsn: plan_support::entry_support::TestDsn::of(pg.url(true)),
        state,
        app: app.clone(),
        denied: app,
        ctx,
        db,
    };
    let world = contract_support::world(f, &catalog).await;
    contract_support::verify(&world, golden, false).await;
}

with_goldens!(contract_tests! [#[ignore = "needs the Postgres harness"]]);
