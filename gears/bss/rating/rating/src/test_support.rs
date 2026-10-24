//! Fixtures of the crate's unit tests.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use time::{Duration, OffsetDateTime};
use toolkit::contracts::DatabaseCapability;
use toolkit_db::{ConnectOpts, Db};

use crate::BssRatingGear;
use crate::domain::ports::Clock;

/// An in-memory SQLite database with every Rating migration applied.
pub async fn migrated_db() -> Db {
    let db = toolkit_db::connect_db(
        "sqlite::memory:",
        ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..ConnectOpts::default()
        },
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        BssRatingGear::default().migrations(),
    )
    .await
    .unwrap();
    db
}

/// A clock the test moves.
pub struct TestClock(Mutex<OffsetDateTime>);

impl TestClock {
    /// A clock at 2026-10-19T12:00:00Z.
    pub fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(
            OffsetDateTime::from_unix_timestamp(1_792_411_200).unwrap(),
        )))
    }

    /// Moves the clock forward.
    pub fn advance(&self, by: Duration) {
        let mut now = self.0.lock().unwrap();
        *now += by;
    }
}

impl Clock for TestClock {
    fn now(&self) -> OffsetDateTime {
        *self.0.lock().unwrap()
    }
}
