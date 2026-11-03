//! Repository decorators that evict what each configuration write can
//! affect (ADR-0020).
//!
//! Every write call maps to one [`Change`], built from the call's own
//! arguments before the call. The change is applied when the call ends,
//! whether it succeeded, failed or its future was dropped mid-call: a write
//! can commit before its caller goes away, or fail after it committed.
//! Reads pass through. Sitting at the repository boundary, the hook covers
//! every write path: the Management API, the SDK and the registry reconcile.
//!
//! Once the cluster is up, each change also goes to the [`Publisher`], so
//! other replicas evict it too: a completed write waits for the append (see
//! [`PUBLISH_TIMEOUT`]); a dropped one hands its change over without waiting
//! and re-applies it after [`LATE_COMMIT_WINDOW`]. The hook exists only
//! with the cache enabled; publishing needs the `cluster` feature, without
//! which OAGW caches nothing and no write reaches this hook.

use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;
#[cfg(feature = "cluster")]
use std::time::Duration;

#[cfg(feature = "cluster")]
use arc_swap::ArcSwap;
use async_trait::async_trait;
#[cfg(feature = "cluster")]
use futures_util::FutureExt;
#[cfg(feature = "cluster")]
use tokio::sync::oneshot;
#[cfg(feature = "cluster")]
use tracing::{debug, warn};
use uuid::Uuid;

#[cfg(feature = "cluster")]
use super::publisher::{Ack, PublishError, Publisher};
use super::{Change, ProxyConfigCache};
use crate::domain::model::{ListQuery, Route, Upstream};
use crate::domain::ports::OagwMetricsPort;
use crate::domain::ports::metric_labels::cache_change_source;
#[cfg(feature = "cluster")]
use crate::domain::ports::metric_labels::cache_publish_failure;
#[cfg(feature = "cluster")]
use crate::domain::repo::defer_wait;
use crate::domain::repo::{
    DeleteOutcome, RegistryRowRepository, RepositoryError, RouteRepository, RowKey, Tags,
    UpstreamRepository,
};

/// How long a write waits for its change to reach the change log.
#[cfg(feature = "cluster")]
pub(crate) const PUBLISH_TIMEOUT: Duration = Duration::from_secs(2);

/// How long after a write's future is dropped its change is applied and
/// published once more. A statement already sent (an autocommit `DELETE`
/// waiting on a row lock, a `COMMIT`) can still complete after some replica
/// has loaded the old row; a commit later than this is left to the TTL.
/// Sized for a registry reconcile call cut at its 20 s timeout under
/// `MySQL`'s default 50 s lock wait.
#[cfg(feature = "cluster")]
pub(crate) const LATE_COMMIT_WINDOW: Duration = Duration::from_secs(30);

/// Where the hook's changes go.
#[cfg(feature = "cluster")]
enum Publishing {
    /// The change feed has not started: there is no cluster to publish to
    /// yet. Changes are not published, and not counted.
    NotStarted,
    /// Through this publisher.
    Running(Publisher),
    /// The change feed has stopped. Changes are not published, and are
    /// counted and logged.
    Stopped,
}

/// What the decorators share: the cache each write evicts from, and the
/// publisher, while the cluster is up.
#[derive(Clone)]
pub(crate) struct WriteHook {
    cache: Arc<ProxyConfigCache>,
    metrics: Arc<dyn OagwMetricsPort>,
    #[cfg(feature = "cluster")]
    publishing: Arc<ArcSwap<Publishing>>,
}

impl WriteHook {
    /// A hook evicting from `cache`, publishing nothing yet.
    pub(crate) fn new(cache: Arc<ProxyConfigCache>) -> Self {
        Self {
            metrics: Arc::clone(cache.metrics()),
            cache,
            #[cfg(feature = "cluster")]
            publishing: Arc::new(ArcSwap::from_pointee(Publishing::NotStarted)),
        }
    }

    /// The cache writes evict from.
    #[cfg(feature = "cluster")]
    pub(super) fn cache(&self) -> &Arc<ProxyConfigCache> {
        &self.cache
    }

    /// Where the hook and its change feed record.
    #[cfg(feature = "cluster")]
    pub(super) fn metrics(&self) -> &Arc<dyn OagwMetricsPort> {
        &self.metrics
    }

    /// Publish each later change through `publisher` (the change feed
    /// started). Every decorator sharing this hook follows.
    #[cfg(feature = "cluster")]
    pub(crate) fn start_publishing(&self, publisher: Publisher) {
        self.publishing
            .store(Arc::new(Publishing::Running(publisher)));
    }

    /// Publish nothing more, and count each later change as not published
    /// (the change feed stopped). Every decorator sharing this hook follows.
    #[cfg(feature = "cluster")]
    pub(crate) fn stop_publishing(&self) {
        self.publishing.store(Arc::new(Publishing::Stopped));
    }

    /// Run `write`, then apply `change` and publish it. The guard applies it
    /// also when the call fails or this future is dropped.
    async fn write<T, F, Fut>(&self, change: Change, write: F) -> Result<T, RepositoryError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, RepositoryError>>,
    {
        let guard = ChangeGuard {
            hook: self,
            change,
            completed: false,
        };
        let result = write().await;
        let change = guard.complete();
        #[cfg(feature = "cluster")]
        self.publish(change).await;
        #[cfg(not(feature = "cluster"))]
        drop(change);
        result
    }

    /// Apply `change` to the cache as a local change.
    fn apply(&self, change: &Change) {
        self.cache.apply(change);
        self.metrics
            .record_proxy_cache_change(cache_change_source::LOCAL);
    }

    /// Hand `change` to the publisher, if one runs; `false` when it is not
    /// published. A change the stopped feed does not take is counted and
    /// logged here.
    #[cfg(feature = "cluster")]
    fn submit(&self, change: Change, ack: Option<Ack>) -> bool {
        let refused = match &**self.publishing.load() {
            Publishing::Running(publisher) => match publisher.submit(change, ack) {
                Ok(()) => return true,
                Err(refused) => refused,
            },
            Publishing::Stopped => change,
            Publishing::NotStarted => {
                debug!(change = %change, "No change feed yet; the proxy configuration change is not published");
                return false;
            }
        };
        self.metrics
            .record_proxy_cache_publish_failures(cache_publish_failure::PUBLISHER_STOPPED, 1);
        warn!(
            change = %refused,
            "The proxy configuration change publisher has stopped; the change is not published, \
             and other replicas see it when their cache entries expire"
        );
        false
    }

    /// Hand `change` to the publisher, if there is one, and wait until it is
    /// in the change log or [`PUBLISH_TIMEOUT`] passes; in
    /// [`one_wait_at_the_end`](crate::domain::repo::one_wait_at_the_end),
    /// leave that wait to its end. The write's own result stands either way.
    #[cfg(feature = "cluster")]
    async fn publish(&self, change: Change) {
        let (ack, appended) = oneshot::channel();
        if !self.submit(change.clone(), Some(ack)) {
            return;
        }
        if let Err(wait) = defer_wait(wait_for_append(change, appended).boxed()) {
            wait.await;
        }
    }

    /// Apply and publish `change` again after [`LATE_COMMIT_WINDOW`], on a
    /// task of its own, for a write dropped mid-call. Only while the change
    /// feed runs; the task ends when the feed stops, leaving a late commit
    /// to the TTL.
    ///
    /// A refusal by a stopped publisher is not counted again: the dropped
    /// write was counted already.
    #[cfg(feature = "cluster")]
    fn reapply_later(&self, change: Change) {
        let Publishing::Running(publisher) = &**self.publishing.load() else {
            debug!(
                change = %change,
                "No change feed runs; a dropped write's late commit is seen when the cache \
                 entries expire"
            );
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            warn!(
                change = %change,
                "No runtime to apply a dropped write's change again; \
                 a late commit is seen when the cache entries expire"
            );
            return;
        };
        let publisher = publisher.clone();
        let stopping = publisher.stopping();
        let hook = self.clone();
        runtime.spawn(async move {
            tokio::select! {
                biased;
                () = stopping => {}
                () = tokio::time::sleep(LATE_COMMIT_WINDOW) => {
                    hook.apply(&change);
                    if let Err(refused) = publisher.submit(change, None) {
                        debug!(
                            change = %refused,
                            "The change feed has stopped; a dropped write's change is not \
                             published again"
                        );
                    }
                }
            }
        });
    }
}

/// Wait until the append carrying `change` is acked through `appended`, or
/// [`PUBLISH_TIMEOUT`] passes. Counts nothing: the publisher counts a change
/// whose append failed or that it dropped, and a change still on its way
/// may yet land.
#[cfg(feature = "cluster")]
async fn wait_for_append(change: Change, appended: oneshot::Receiver<Result<(), PublishError>>) {
    match tokio::time::timeout(PUBLISH_TIMEOUT, appended).await {
        // A failed append is logged by the publisher.
        Ok(Ok(_)) => {}
        Ok(Err(_)) => warn!(
            change = %change,
            "The proxy configuration change publisher stopped before the change was published"
        ),
        Err(_) => warn!(
            change = %change,
            timeout = ?PUBLISH_TIMEOUT,
            "A proxy configuration change was not published in time; \
             the write answers without waiting further"
        ),
    }
}

/// Applies its change when dropped; dropped before the write completed, it
/// also submits the change and schedules the late re-apply.
struct ChangeGuard<'a> {
    hook: &'a WriteHook,
    change: Change,
    completed: bool,
}

impl ChangeGuard<'_> {
    /// The write completed: return the change for the caller to publish.
    fn complete(mut self) -> Change {
        self.completed = true;
        self.change.clone()
    }
}

impl Drop for ChangeGuard<'_> {
    fn drop(&mut self) {
        self.hook.apply(&self.change);
        #[cfg(feature = "cluster")]
        if !self.completed {
            // Counted and logged inside when not published.
            self.hook.submit(self.change.clone(), None);
            self.hook.reapply_later(self.change.clone());
        }
    }
}

/// The change of an upstream create or update.
fn upstream_written(upstream: &Upstream) -> Change {
    Change::Upstream {
        tenant_id: upstream.tenant_id,
        id: upstream.id,
        alias: upstream.alias.clone(),
    }
}

/// The change of a route create or update.
fn route_written(route: &Route) -> Change {
    Change::Route {
        tenant_id: route.tenant_id,
        id: route.id,
        upstream_id: route.upstream_id,
    }
}

/// [`UpstreamRepository`] that evicts what each write can affect.
pub(crate) struct FlushingUpstreamRepo {
    inner: Arc<dyn UpstreamRepository>,
    hook: WriteHook,
}

impl FlushingUpstreamRepo {
    /// `inner`, evicting through `hook`.
    pub(crate) fn new(inner: Arc<dyn UpstreamRepository>, hook: WriteHook) -> Self {
        Self { inner, hook }
    }
}

#[async_trait]
impl UpstreamRepository for FlushingUpstreamRepo {
    async fn create(&self, upstream: Upstream) -> Result<Upstream, RepositoryError> {
        self.hook
            .write(upstream_written(&upstream), || self.inner.create(upstream))
            .await
    }

    async fn get_by_id(&self, tenant_id: Uuid, id: Uuid) -> Result<Upstream, RepositoryError> {
        self.inner.get_by_id(tenant_id, id).await
    }

    async fn list(
        &self,
        tenant_id: Uuid,
        query: &ListQuery,
    ) -> Result<Vec<Upstream>, RepositoryError> {
        self.inner.list(tenant_id, query).await
    }

    async fn update(&self, upstream: Upstream) -> Result<Upstream, RepositoryError> {
        self.hook
            .write(upstream_written(&upstream), || self.inner.update(upstream))
            .await
    }

    async fn delete(&self, tenant_id: Uuid, id: Uuid) -> Result<(), RepositoryError> {
        let change = Change::UpstreamDeleted { tenant_id, id };
        self.hook
            .write(change, || self.inner.delete(tenant_id, id))
            .await
    }

    async fn list_by_alias_for_tenants(
        &self,
        alias: &str,
        tenant_ids: &HashSet<Uuid>,
        tags: Tags,
    ) -> Result<Vec<Upstream>, RepositoryError> {
        self.inner
            .list_by_alias_for_tenants(alias, tenant_ids, tags)
            .await
    }
}

/// [`RouteRepository`] that evicts what each write can affect.
pub(crate) struct FlushingRouteRepo {
    inner: Arc<dyn RouteRepository>,
    hook: WriteHook,
}

impl FlushingRouteRepo {
    /// `inner`, evicting through `hook`.
    pub(crate) fn new(inner: Arc<dyn RouteRepository>, hook: WriteHook) -> Self {
        Self { inner, hook }
    }
}

#[async_trait]
impl RouteRepository for FlushingRouteRepo {
    async fn create(&self, route: Route) -> Result<Route, RepositoryError> {
        self.hook
            .write(route_written(&route), || self.inner.create(route))
            .await
    }

    async fn get_by_id(&self, tenant_id: Uuid, id: Uuid) -> Result<Route, RepositoryError> {
        self.inner.get_by_id(tenant_id, id).await
    }

    async fn list(
        &self,
        tenant_id: Uuid,
        upstream_id: Option<Uuid>,
        query: &ListQuery,
    ) -> Result<Vec<Route>, RepositoryError> {
        self.inner.list(tenant_id, upstream_id, query).await
    }

    async fn find_matching_in_tenants(
        &self,
        tenant_chain: &[Uuid],
        upstream_ids: &[Uuid],
        method: &str,
        path: &str,
        tags: Tags,
    ) -> Result<Route, RepositoryError> {
        self.inner
            .find_matching_in_tenants(tenant_chain, upstream_ids, method, path, tags)
            .await
    }

    async fn list_proxy_routes(
        &self,
        upstreams: &[RowKey],
    ) -> Result<Vec<(RowKey, Vec<Route>)>, RepositoryError> {
        self.inner.list_proxy_routes(upstreams).await
    }

    async fn update(&self, route: Route) -> Result<Route, RepositoryError> {
        self.hook
            .write(route_written(&route), || self.inner.update(route))
            .await
    }

    async fn delete(&self, tenant_id: Uuid, id: Uuid) -> Result<(), RepositoryError> {
        let change = Change::RouteDeleted { tenant_id, id };
        self.hook
            .write(change, || self.inner.delete(tenant_id, id))
            .await
    }

    async fn delete_by_upstream(
        &self,
        tenant_id: Uuid,
        upstream_id: Uuid,
    ) -> Result<(), RepositoryError> {
        let change = Change::RoutesOfUpstream {
            tenant_id,
            upstream_id,
        };
        self.hook
            .write(change, || {
                self.inner.delete_by_upstream(tenant_id, upstream_id)
            })
            .await
    }
}

/// [`RegistryRowRepository`] that evicts what its delete can affect.
pub(crate) struct FlushingRegistryRowRepo {
    inner: Arc<dyn RegistryRowRepository>,
    hook: WriteHook,
}

impl FlushingRegistryRowRepo {
    /// `inner`, evicting through `hook`.
    pub(crate) fn new(inner: Arc<dyn RegistryRowRepository>, hook: WriteHook) -> Self {
        Self { inner, hook }
    }
}

#[async_trait]
impl RegistryRowRepository for FlushingRegistryRowRepo {
    async fn list_upstreams(&self) -> Result<Vec<Upstream>, RepositoryError> {
        self.inner.list_upstreams().await
    }

    async fn list_routes(&self) -> Result<Vec<Route>, RepositoryError> {
        self.inner.list_routes().await
    }

    async fn delete_upstream_if_unused(
        &self,
        tenant_id: Uuid,
        id: Uuid,
    ) -> Result<DeleteOutcome, RepositoryError> {
        // Evicts also when the upstream was kept (`InUse`): one rule for
        // every outcome, as for a failed write.
        let change = Change::UpstreamDeleted { tenant_id, id };
        self.hook
            .write(change, || {
                self.inner.delete_upstream_if_unused(tenant_id, id)
            })
            .await
    }
}

#[cfg(test)]
#[path = "write_hook_tests.rs"]
mod tests;
