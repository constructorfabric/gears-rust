//! Keeps the specification cache following what `types-registry` holds.
//!
//! `types-registry` commits configuration-seeded entities once, during the
//! platform's `post_init`, and offers no change feed, so the broker re-reads on
//! a cadence instead: a topic or an event type registered after startup becomes
//! resolvable at the next pass, without a restart. Watching for change events,
//! and later a registration-time hook, are the two stages after this one.
//!
//! Driven rather than self-scheduling: the caller decides the cadence and one
//! pass does one load, so a test forces passes instead of sleeping and hoping a
//! background task ran.
//!
//! Only the ingest role runs this. Exactly one role writes the shared state and
//! the others re-read it, which is what lets a delivery instance serve a topic
//! whose settings appear in no configuration that instance was given.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use types_registry_sdk::TypesRegistryClient;

use crate::config::EventBrokerConfig;
use crate::domain::backend::BackendResolver;
use crate::domain::specification::{ProvisioningLimits, SpecificationManager};

/// First retry delay after a provisioning pass leaves topics unprovisioned in
/// `run` (doubled up to the tick); the cap on `settle_provisioning`'s backoff.
const PROVISIONING_RETRY_FIRST: Duration = Duration::from_secs(1);

pub struct SpecificationRefreshWorker {
    client: Arc<dyn TypesRegistryClient>,
    db: Arc<toolkit_db::DBProvider<toolkit_db::DbError>>,
    config: EventBrokerConfig,
    tick: Duration,
    provisioning: Option<Provisioning>,
}

struct Provisioning {
    specs: Arc<dyn SpecificationManager>,
    backends: Arc<dyn BackendResolver>,
    limits: ProvisioningLimits,
}

impl SpecificationRefreshWorker {
    /// Four arguments, none of them interchangeable: a registry client, a
    /// database handle, the configuration resolution folds in, and a cadence.
    #[must_use]
    pub fn new(
        client: Arc<dyn TypesRegistryClient>,
        db: Arc<toolkit_db::DBProvider<toolkit_db::DbError>>,
        config: EventBrokerConfig,
        tick: Duration,
    ) -> Self {
        Self {
            client,
            db,
            config,
            tick,
            provisioning: None,
        }
    }

    /// Also provisions every loaded topic on its backend after each load
    /// (feature 0006 §4.1): at start, and whenever a pass registers a topic.
    #[must_use]
    pub fn provisioning(
        mut self,
        specs: Arc<dyn SpecificationManager>,
        backends: Arc<dyn BackendResolver>,
        limits: ProvisioningLimits,
    ) -> Self {
        self.provisioning = Some(Provisioning {
            specs,
            backends,
            limits,
        });
        self
    }

    /// One pass: re-read, re-project, re-resolve, upsert, then provision.
    ///
    /// A failure is logged and left for the next pass. The cache still holds
    /// what the previous pass found, which is a better answer than none.
    ///
    /// Returns how many registered topics are left unprovisioned (always 0
    /// without provisioning).
    pub async fn run_once(&self) -> usize {
        if let Err(err) =
            crate::infra::specification::bulk_load(&self.client, &self.db, &self.config).await
        {
            tracing::warn!(%err, "specification refresh failed; the cache still holds the last load");
        }
        self.provision_once().await
    }

    /// Retries provisioning on the short backoff until every registered topic
    /// is provisioned or `within` has passed; the number left unprovisioned,
    /// or `None` when `stop` fired first.
    ///
    /// For the start phase: a backend that is still starting gets its topics
    /// provisioned before the instance takes traffic, while one that is down
    /// delays the start by `within` at most and never fails it. A stop ends the
    /// wait at once, so a process stopped right after launch exits promptly;
    /// abandoning a pass midway is safe, since a provisioning attempt left
    /// unfinished is taken over once its attempt timeout passes.
    pub async fn settle_provisioning(
        &self,
        within: Duration,
        stop: &CancellationToken,
    ) -> Option<usize> {
        let settle = async {
            let deadline = tokio::time::Instant::now() + within;
            let mut backoff = PROVISIONING_RETRY_FIRST.min(Duration::from_millis(250));
            let mut pending = self.provision_once().await;
            while pending > 0 && tokio::time::Instant::now() + backoff < deadline {
                tokio::time::sleep(backoff).await;
                pending = self.provision_once().await;
                backoff = (backoff * 2).min(PROVISIONING_RETRY_FIRST);
            }
            pending
        };
        tokio::select! {
            pending = settle => Some(pending),
            () = stop.cancelled() => None,
        }
    }

    /// A provisioning pass alone, without re-reading the registry.
    async fn provision_once(&self) -> usize {
        match &self.provisioning {
            Some(provisioning) => {
                provisioning
                    .specs
                    .provision_topics(provisioning.backends.as_ref(), provisioning.limits)
                    .await
            }
            None => 0,
        }
    }

    /// Runs until cancelled, re-reading the registry once per tick, waiting a
    /// tick first.
    ///
    /// The initial load is the caller's: nothing may serve a request against an
    /// empty cache, so `serve()` awaits one pass before anything is wired and
    /// this loop picks up from the next tick. Passing straight away here would
    /// re-read what was just read.
    pub async fn run(self, shutdown: CancellationToken) {
        // A topic not provisioned by a pass - its backend not started yet, or
        // answering `Pending` - is retried on a short backoff rather than at
        // the next refresh (feature 0006 §4.1); the registry is still re-read
        // once per tick.
        let mut pending = self.provision_once().await;
        let mut backoff = PROVISIONING_RETRY_FIRST;
        let mut next_refresh = tokio::time::Instant::now() + self.tick;
        loop {
            let wake = if pending > 0 {
                (tokio::time::Instant::now() + backoff).min(next_refresh)
            } else {
                next_refresh
            };
            tokio::select! {
                () = tokio::time::sleep_until(wake) => {}
                () = shutdown.cancelled() => return,
            }
            if shutdown.is_cancelled() {
                return;
            }
            if tokio::time::Instant::now() >= next_refresh {
                pending = self.run_once().await;
                next_refresh = tokio::time::Instant::now() + self.tick;
                backoff = PROVISIONING_RETRY_FIRST;
            } else {
                pending = self.provision_once().await;
                backoff = (backoff * 2).min(self.tick);
            }
        }
    }

    /// Spawns [`Self::run`]. The handle is returned so a shutdown can join it
    /// rather than leaving a load in flight against a closing pool.
    #[must_use]
    pub fn spawn(self, shutdown: CancellationToken) -> JoinHandle<()> {
        tokio::spawn(self.run(shutdown))
    }
}

#[cfg(test)]
#[path = "specification_refresh_tests.rs"]
mod specification_refresh_tests;
