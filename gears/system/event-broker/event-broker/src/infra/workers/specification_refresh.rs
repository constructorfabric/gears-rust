//! Keeps the specification cache following what `types-registry` holds.
//!
//! `types-registry` offers no change feed, so the broker re-reads on a cadence
//! instead: a topic or an event type admitted into the registry's database
//! after startup becomes resolvable at the next pass, without a restart.
//! Watching for change events, and later a registration-time hook, are the two
//! stages after this one.
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
use types_registry_sdk::PlatformTypesRegistryApi;

use crate::config::EventBrokerConfig;
use crate::domain::backend::BackendResolver;
use crate::domain::specification::{ProvisioningLimits, SpecificationManager};

/// First retry delay after a provisioning pass leaves topics unprovisioned in
/// `run` (doubled up to the tick); the cap on `settle_provisioning`'s backoff.
const PROVISIONING_RETRY_FIRST: Duration = Duration::from_secs(1);

/// First retry delay of [`SpecificationRefreshWorker::settle_provisioning`]:
/// short, so a backend that is still starting is picked up soon after it is up.
const STARTUP_RETRY_FIRST: Duration = Duration::from_millis(250);

/// How [`SpecificationRefreshWorker::settle_provisioning`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisioningWait {
    /// The retries ended with no pass in flight: every registered topic is
    /// provisioned (`unprovisioned` is 0), or the bound left no room for
    /// another retry with `unprovisioned` topics still not provisioned.
    Settled { unprovisioned: usize },
    /// The bound passed while a pass was in flight; that pass was abandoned,
    /// so how many topics are left unprovisioned is not known.
    Cut,
    /// The stop fired first; the wait was abandoned.
    Stopped,
}

pub struct SpecificationRefreshWorker {
    client: Arc<dyn PlatformTypesRegistryApi>,
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
        client: Arc<dyn PlatformTypesRegistryApi>,
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

    /// One pass: [`Self::load_once`], then a provisioning pass.
    ///
    /// Returns how many registered topics are left unprovisioned (always 0
    /// without provisioning).
    pub async fn run_once(&self) -> usize {
        self.load_once().await;
        self.provision_once().await
    }

    /// Re-reads the registry, re-projects, re-resolves and upserts, without
    /// provisioning.
    ///
    /// A failure is logged and left for the next pass. The cache still holds
    /// what the previous pass found, which is a better answer than none.
    pub async fn load_once(&self) {
        if let Err(err) =
            crate::infra::specification::bulk_load(&self.client, &self.db, &self.config).await
        {
            tracing::warn!(%err, "specification refresh failed; the cache still holds the last load");
        }
    }

    /// Retries provisioning on the short backoff until every registered topic
    /// is provisioned, `within` has passed or `stop` fires, whichever comes
    /// first.
    ///
    /// For the start phase: a backend that is still starting gets its topics
    /// provisioned before the instance takes traffic, while one that is down
    /// delays the start by `within` at most and never fails it. `within` is a
    /// hard bound: a pass still in flight when it passes, the first one
    /// included, is abandoned rather than awaited, so a backend that never
    /// answers costs `within` and not a call timeout per topic. A stop ends the
    /// wait at once, so a process stopped right after launch exits promptly.
    /// Abandoning a pass midway is safe: a provisioning attempt left
    /// unfinished is taken over once its attempt timeout passes, and a
    /// re-provision abandoned midway leaves its record unconfirmed, so the next
    /// pass calls again (feature 0006 §4.1).
    pub async fn settle_provisioning(
        &self,
        within: Duration,
        stop: &CancellationToken,
    ) -> ProvisioningWait {
        let settle = async {
            let deadline = tokio::time::Instant::now() + within;
            let mut backoff = STARTUP_RETRY_FIRST;
            let mut pending = self.provision_once().await;
            while pending > 0 && tokio::time::Instant::now() + backoff < deadline {
                tokio::time::sleep(backoff).await;
                pending = self.provision_once().await;
                backoff = (backoff * 2).min(PROVISIONING_RETRY_FIRST);
            }
            pending
        };
        tokio::select! {
            biased;
            () = stop.cancelled() => ProvisioningWait::Stopped,
            unprovisioned = settle => ProvisioningWait::Settled { unprovisioned },
            () = tokio::time::sleep(within) => ProvisioningWait::Cut,
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
    /// empty cache, so `serve()` awaits one load before anything is wired and
    /// this loop picks up from the next tick. Passing straight away here would
    /// re-read what was just read.
    ///
    /// Every pass is raced against `shutdown`, so a stop never waits out a
    /// pass against an unreachable registry or backend. A provisioning attempt
    /// abandoned midway is taken over once its attempt timeout passes, and a
    /// re-provision abandoned midway leaves its record unconfirmed, so the next
    /// pass calls again (feature 0006 §4.1).
    pub async fn run(self, shutdown: CancellationToken) {
        // A topic not provisioned by a pass - its backend not started yet, or
        // answering `Pending` - is retried on a short backoff rather than at
        // the next refresh (feature 0006 §4.1); the registry is still re-read
        // once per tick.
        let Some(mut pending) = race_shutdown(&shutdown, self.provision_once()).await else {
            return;
        };
        let mut backoff = PROVISIONING_RETRY_FIRST;
        let mut next_refresh = tokio::time::Instant::now() + self.tick;
        loop {
            let wake = if pending > 0 {
                (tokio::time::Instant::now() + backoff).min(next_refresh)
            } else {
                next_refresh
            };
            if race_shutdown(&shutdown, tokio::time::sleep_until(wake))
                .await
                .is_none()
            {
                return;
            }
            if tokio::time::Instant::now() >= next_refresh {
                let Some(left) = race_shutdown(&shutdown, self.run_once()).await else {
                    return;
                };
                pending = left;
                next_refresh = tokio::time::Instant::now() + self.tick;
                backoff = PROVISIONING_RETRY_FIRST;
            } else {
                let Some(left) = race_shutdown(&shutdown, self.provision_once()).await else {
                    return;
                };
                pending = left;
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

/// Runs `pass` to its end, or abandons it when `shutdown` fires first; `None`
/// when abandoned. A shutdown that has already fired wins over a pass that is
/// ready too, so nothing new completes after a stop.
async fn race_shutdown<T>(
    shutdown: &CancellationToken,
    pass: impl Future<Output = T>,
) -> Option<T> {
    tokio::select! {
        biased;
        () = shutdown.cancelled() => None,
        done = pass => Some(done),
    }
}

#[cfg(test)]
#[path = "specification_refresh_tests.rs"]
mod specification_refresh_tests;
