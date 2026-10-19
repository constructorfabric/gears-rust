//! Fixtures of the crate's unit tests.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use cluster::{ClusterHandle, ClusterWiring, ProfileBackends};
use cluster_sdk::LeaderElectionBackend;
use standalone_cluster_plugin::{StandaloneClusterHandle, StandaloneClusterPlugin};
use time::{Duration, OffsetDateTime};
use toolkit::client_hub::ClientHub;
use toolkit::contracts::DatabaseCapability;
use toolkit_db::{ConnectOpts, Db};

use crate::BssRatingGear;
use crate::domain::ports::Clock;
use crate::infra::cluster::RatingClusterProfile;
use crate::test_providers::FakeClusterLeaders;

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

/// The cluster wiring that binds a [`FakeClusterLeaders`] as the leader election of Rating's
/// cluster profile, over the standalone in-memory cache. Stop it at the end of the test: the
/// cluster handle panics in debug builds when it is dropped running.
pub struct LeadersWiring {
    cluster: ClusterHandle,
    standalone: StandaloneClusterHandle,
}

impl LeadersWiring {
    /// Binds `leaders` in `hub`.
    pub fn bind(leaders: &FakeClusterLeaders, hub: &Arc<ClientHub>) -> Self {
        let standalone = StandaloneClusterPlugin::builder()
            .build_and_start()
            .unwrap();
        let backends = ProfileBackends::new(standalone.cache())
            .with_leader_election(Arc::new(leaders.clone()) as Arc<dyn LeaderElectionBackend>);
        let cluster = ClusterWiring::builder(Arc::clone(hub))
            .profile(RatingClusterProfile, backends)
            .build_and_start()
            .unwrap();
        Self {
            cluster,
            standalone,
        }
    }

    /// Deregisters the backends and stops the standalone cache.
    pub async fn stop(self) {
        self.cluster.stop().await;
        self.standalone.stop().await;
    }
}
