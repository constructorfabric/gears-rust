//! The refresher publishes what the catalog says, and publishes nothing when it
//! cannot read.
//!
//! The second half is the one worth the fixture. D-246 removed a `[C]` whose whole
//! shape was *a caller that did not hold the catalog's answer writing to this
//! series anyway* — a tax-exclusive plan clearing §7's alarm over five markets that
//! stayed gated. A refresher that published `0` on a failed read would be the same
//! defect in a different costume, and nothing about the gauge's type prevents it.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};

use super::{GatedMarketsJob, GatedMarketsReport};
use crate::config::JobsConfig;
use crate::domain::ports::metrics::{
    CurrencyBindingCase, PreviewFailClosed, PricingAlarm, PricingMetricsPort,
};
use crate::infra::storage::migrations::Migrator;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;

/// A sink that records the last value published to the gauge, and how many times.
///
/// The call count is asserted as well as the value, because "published `0`" and
/// "published nothing" are the two states this job must keep apart and they are
/// indistinguishable from the value alone.
#[derive(Default)]
struct RecordingMetrics {
    last: AtomicI64,
    calls: AtomicI64,
}

impl PricingMetricsPort for RecordingMetrics {
    fn preview_failclosed(&self, _reason: PreviewFailClosed) {}
    fn currency_binding_block(&self, _case: CurrencyBindingCase) {}
    fn tax_not_sellable_ga(&self, count: i64) {
        self.last.store(count, Ordering::Relaxed);
        self.calls.fetch_add(1, Ordering::Relaxed);
    }
    fn alarm(&self, _alarm: PricingAlarm) {}
}

/// A migrated in-memory database with an **empty** catalog.
///
/// No rows are seeded by hand here on purpose. Whether a given row counts as a
/// gated market is `price_repo::gated_markets`' question and is settled through the
/// gear in `tests/sqlite_price_repo.rs`, against rows written by `create_draft`; a
/// hand-built `ActiveModel` would be a fixture that could not reach its state
/// through the gear, which is the defect class this program keeps paying for. What
/// these cases own is the job's plumbing: what it publishes, and when it does not.
async fn empty_catalog() -> DBProvider<DbError> {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .expect("connect in-memory sqlite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run migrator");
    DBProvider::<DbError>::new(db)
}

/// A provider over a database the migrator never ran on: `pricing_price` does not
/// exist, so the pass's read fails. The cheapest reachable read failure, and the
/// class of failure does not matter — what the case is about is that **no** value
/// is published when one cannot be read.
async fn catalog_with_no_schema() -> DBProvider<DbError> {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .expect("connect in-memory sqlite");
    DBProvider::<DbError>::new(db)
}

fn job(db: DBProvider<DbError>, metrics: Arc<RecordingMetrics>) -> GatedMarketsJob {
    GatedMarketsJob::new(db, metrics, JobsConfig::default())
}

#[tokio::test]
async fn an_empty_catalog_publishes_zero_rather_than_skipping() {
    // The distinction the call count exists for: nothing gated is a real answer and
    // must reach the gauge, or the series stays at whatever the last non-empty pass
    // left and §7's alarm never clears.
    let metrics = Arc::new(RecordingMetrics::default());
    let job = job(empty_catalog().await, Arc::clone(&metrics));

    assert_eq!(
        job.run_once().await.expect("the pass reads"),
        GatedMarketsReport { gated_markets: 0 }
    );
    assert_eq!(metrics.last.load(Ordering::Relaxed), 0);
    assert_eq!(metrics.calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn a_failed_read_publishes_nothing_and_leaves_the_previous_value() {
    // The failure is an unmigrated store, which is the cheapest reachable one. What
    // matters is not which failure it is but that **no** value is published:
    // publishing `0` here
    // would clear the alarm over markets that are still gated, which is the `[C]`
    // D-246 removed, arriving from the other side.
    let metrics = Arc::new(RecordingMetrics::default());
    // A first pass over a readable catalog leaves a value on the gauge...
    job(empty_catalog().await, Arc::clone(&metrics))
        .run_once()
        .await
        .expect("the first pass reads");
    assert_eq!(metrics.calls.load(Ordering::Relaxed), 1);

    // ...and a second pass that cannot read must not move it.
    let broken = job(catalog_with_no_schema().await, Arc::clone(&metrics));

    assert!(
        broken.run_once().await.is_err(),
        "the pass reports the failure"
    );
    assert_eq!(
        metrics.calls.load(Ordering::Relaxed),
        1,
        "nothing was published by the failed pass: the previous value stands, because a \
         stale gauge is a flat series an operator can see while a zero here is an alarm \
         cleared over markets that are still gated"
    );
}

#[tokio::test]
async fn the_tick_is_the_configured_one() {
    // D-250's value, read back rather than assumed: the job carries the cadence so a
    // scheduler does not have to know which knob applies to it.
    let metrics = Arc::new(RecordingMetrics::default());
    let job = job(empty_catalog().await, metrics);

    assert_eq!(job.tick_secs(), 60);
}
