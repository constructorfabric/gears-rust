//! Per-replica cache of the configuration the proxy reads (ADR-0020).
//!
//! Two sections: upstream candidates by alias and tenant chain, and the
//! enabled HTTP routes of each upstream. Both hold raw repository answers;
//! upstream selection, the SSRF check, route selection and the merge still
//! run per request. Each write evicts only the entries it can affect
//! ([`Change`]), on this replica and, through the cluster change log, on
//! every other; each entry also expires after the TTL.
//!
//! The change log, its publisher and watcher are built with the `cluster`
//! feature only, so an in-memory OAGW binary does not link the cluster SDK
//! (ADR-0020 *Cluster Setup*).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
#[cfg(feature = "cluster")]
use cluster_sdk::{CacheCapability, CacheResolverBuilder, ClusterError};
use futures_util::FutureExt;
#[cfg(feature = "cluster")]
use tokio::task::JoinHandle;
#[cfg(feature = "cluster")]
use tokio_util::sync::CancellationToken;
#[cfg(feature = "cluster")]
use tracing::{info, warn};
use uuid::Uuid;

use crate::domain::model::{Route, Upstream};
use crate::domain::ports::OagwMetricsPort;
use crate::domain::ports::metric_labels::CacheMap;
use crate::domain::repo::{
    ProxyConfigReader, RepositoryError, RouteRepository, RowKey, Tags, UpstreamRepository,
};

mod change;
#[cfg(feature = "cluster")]
mod change_log;
#[cfg(feature = "cluster")]
mod publisher;
mod section;
#[cfg(test)]
pub(crate) mod testing;
#[cfg(feature = "cluster")]
mod watcher;
mod write_hook;

pub(crate) use change::Change;
#[cfg(feature = "cluster")]
use publisher::Publisher;
use section::{Section, SectionKey, SectionValue};
#[cfg(feature = "cluster")]
pub(crate) use watcher::{WatchHealth, WatchState, WatchStatus};
pub(crate) use write_hook::{
    FlushingRegistryRowRepo, FlushingRouteRepo, FlushingUpstreamRepo, WriteHook,
};

/// Scope of the gear's cluster keys: the backend sees `oagw/<key>`.
#[cfg(feature = "cluster")]
const SCOPE: &str = "oagw";

/// An alias section key: a normalized alias and the tenant chain it is
/// looked up in, in order. A tenant whose chain changes uses a new key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AliasKey {
    alias: String,
    chain: Vec<Uuid>,
}

impl SectionKey for AliasKey {
    fn alias(&self) -> Option<&str> {
        Some(&self.alias)
    }
}

impl SectionKey for RowKey {}

impl SectionValue for Upstream {
    fn id(&self) -> Uuid {
        self.id
    }
}

impl SectionValue for Route {
    fn id(&self) -> Uuid {
        self.id
    }
}

/// The cache: every upstream with an alias in a tenant chain (including
/// disabled ones; empty when there is none), and the enabled HTTP routes of
/// each upstream, keyed by the upstream's tenant and ID.
pub(crate) struct ProxyConfigCache {
    aliases: Section<AliasKey, Upstream>,
    routes: Section<RowKey, Route>,
    metrics: Arc<dyn OagwMetricsPort>,
}

impl ProxyConfigCache {
    /// Entries live for `ttl`; each section holds at most `max_entries`.
    pub(crate) fn new(
        ttl: Duration,
        max_entries: usize,
        metrics: Arc<dyn OagwMetricsPort>,
    ) -> Self {
        Self {
            aliases: Section::new(CacheMap::Alias, ttl, max_entries, Arc::clone(&metrics)),
            routes: Section::new(CacheMap::Route, ttl, max_entries, Arc::clone(&metrics)),
            metrics,
        }
    }

    /// Shared by the cache, its write hook and its change feed.
    fn metrics(&self) -> &Arc<dyn OagwMetricsPort> {
        &self.metrics
    }

    /// Evict what `change` can affect, and detach the in-flight loads of
    /// every section it touches.
    pub(crate) fn apply(&self, change: &Change) {
        self.aliases.evict(|index| change.aliases(index));
        self.routes.evict(|index| change.routes(index));
    }
}

/// [`ProxyConfigReader`] over the cache, loading misses from the
/// repositories without tags.
pub(crate) struct CachedProxyConfigReader {
    cache: Arc<ProxyConfigCache>,
    upstreams: Arc<dyn UpstreamRepository>,
    routes: Arc<dyn RouteRepository>,
}

impl CachedProxyConfigReader {
    /// A reader serving from `cache`, loading misses from the repositories.
    pub(crate) fn new(
        cache: Arc<ProxyConfigCache>,
        upstreams: Arc<dyn UpstreamRepository>,
        routes: Arc<dyn RouteRepository>,
    ) -> Self {
        Self {
            cache,
            upstreams,
            routes,
        }
    }
}

#[async_trait]
impl ProxyConfigReader for CachedProxyConfigReader {
    async fn upstreams_by_alias(
        &self,
        alias: &str,
        chain: &[Uuid],
    ) -> Result<Arc<[Upstream]>, RepositoryError> {
        let key = AliasKey {
            alias: alias.to_owned(),
            chain: chain.to_vec(),
        };
        let repo = Arc::clone(&self.upstreams);
        self.cache
            .aliases
            .get_or_load(key, move |key| {
                async move {
                    let tenants: HashSet<Uuid> = key.chain.iter().copied().collect();
                    repo.list_by_alias_for_tenants(&key.alias, &tenants, Tags::Skip)
                        .await
                }
                .boxed()
            })
            .await
    }

    async fn routes_of(&self, upstreams: &[RowKey]) -> Result<Vec<Arc<[Route]>>, RepositoryError> {
        let repo = Arc::clone(&self.routes);
        self.cache
            .routes
            .get_or_load_many(upstreams, move |missing| {
                async move { repo.list_proxy_routes(&missing).await }.boxed()
            })
            .await
    }
}

/// The tasks that keep replicas' caches in step through the cluster change
/// log: the publisher appends this replica's changes, the watcher applies
/// the others'.
#[cfg(feature = "cluster")]
pub(crate) struct ChangeFeed {
    hook: WriteHook,
    watch: WatchStatus,
    cancel: CancellationToken,
    publisher: JoinHandle<()>,
    watcher: JoinHandle<()>,
}

#[cfg(feature = "cluster")]
impl ChangeFeed {
    /// Resolve the cluster cache through `cluster` (it must support watches;
    /// `Linearizable` is not required), start the publisher as `origin` and
    /// the watcher keeping `hook`'s cache fresh. The tasks end with `cancel`
    /// or [`stop`](Self::stop). The watch is retried in the watcher's task,
    /// so this does not wait for a remote cluster.
    ///
    /// # Errors
    ///
    /// The profile is not bound, or its cache cannot watch.
    pub(crate) async fn start(
        cluster: CacheResolverBuilder<'_>,
        hook: &WriteHook,
        origin: Uuid,
        watch: &WatchStatus,
        cancel: CancellationToken,
    ) -> Result<Self, ClusterError> {
        let cluster = cluster
            .require(CacheCapability::Watch)
            .resolve()
            .await?
            .scoped(SCOPE)?;
        info!("Resolved the cluster cache for the proxy configuration change log");
        let (publisher, publisher_task) = Publisher::spawn(
            cluster.clone(),
            origin,
            Arc::clone(hook.metrics()),
            watch.clone(),
            cancel.clone(),
        );
        watch.set(WatchState::Connecting);
        let watcher = watcher::spawn(
            cluster,
            Arc::clone(hook.cache()),
            origin,
            watch.clone(),
            cancel.clone(),
        );
        hook.start_publishing(publisher);
        Ok(Self {
            hook: hook.clone(),
            watch: watch.clone(),
            cancel,
            publisher: publisher_task,
            watcher,
        })
    }

    /// Whether every task has ended.
    #[cfg(test)]
    pub(crate) fn is_finished(&self) -> bool {
        self.publisher.is_finished() && self.watcher.is_finished()
    }

    /// The token the feed's tasks end with.
    #[cfg(test)]
    pub(crate) fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Stop publishing, end the tasks and wait for them, aborting what is
    /// still running when `deadline` fires. The watch is down afterwards.
    pub(crate) async fn stop(self, deadline: CancellationToken) {
        self.hook.stop_publishing();
        self.cancel.cancel();
        for (name, mut task) in [("publisher", self.publisher), ("watcher", self.watcher)] {
            tokio::select! {
                joined = &mut task => log_task_end(name, joined),
                () = deadline.cancelled() => {
                    task.abort();
                    warn!(task = name, "Proxy configuration change task cut off at the shutdown deadline");
                    log_task_end(name, task.await);
                }
            }
        }
        self.watch.set(WatchState::Down);
    }
}

/// Log a panic of a change feed task.
#[cfg(feature = "cluster")]
fn log_task_end(name: &str, joined: Result<(), tokio::task::JoinError>) {
    if let Err(e) = joined
        && e.is_panic()
    {
        tracing::error!(task = name, error = %e, "Proxy configuration change task panicked");
    }
}

#[cfg(test)]
#[path = "reader_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "db_tests.rs"]
mod db_tests;
