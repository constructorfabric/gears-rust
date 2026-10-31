//! A cluster for the change log's tests: the standalone backend wired into
//! a hub under the `oagw` profile, optionally behind [`ScriptedCache`].

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use cluster::{ClusterHandle, ClusterWiring, ProfileBackends};
use cluster_sdk::cache::{PutRequest, Ttl};
use cluster_sdk::{
    CacheConsistency, CacheEntry, CacheEvent, CacheFeatures, CacheResolverBuilder, CacheWatch,
    CacheWatchEvent, CacheWatchSender, ClusterCacheBackend, ClusterCacheV1, ClusterError,
    ClusterProfile, ProviderErrorKind,
};
use parking_lot::Mutex;
use standalone_cluster_plugin::{StandaloneClusterHandle, StandaloneClusterPlugin};
use tokio::sync::{Barrier, Notify};
use tokio_util::sync::CancellationToken;
use toolkit::client_hub::ClientHub;
use uuid::Uuid;

use super::super::change_log::{ChangeLog, LOG_KEY};
use super::super::publisher::Publisher;
use super::super::{SCOPE, WatchStatus};
use crate::domain::gts_helpers::HTTP_PROTOCOL_ID;
use crate::domain::model::{Endpoint, ManagedBy, Scheme, Server, Upstream};
use crate::domain::ports::{NoopMetrics, OagwMetricsPort};
use crate::gear::OagwProfile;

/// The key of the change log as the backend sees it.
pub(crate) const BACKEND_LOG_KEY: &str = "oagw/proxy-config-changes";

/// A cluster wired into a hub. Stop it at the end of the test: the cluster
/// handle panics in debug builds when dropped without `stop()`.
pub(crate) struct ClusterFixture {
    cluster: ClusterHandle,
    standalone: StandaloneClusterHandle,
}

impl ClusterFixture {
    /// The standalone backend under the `oagw` profile in `hub`.
    pub(crate) fn standalone(hub: &Arc<ClientHub>) -> Self {
        Self::with(hub, OagwProfile, |cache| cache)
    }

    /// The standalone backend behind `wrap` under `profile` in `hub`.
    pub(crate) fn with<P: ClusterProfile>(
        hub: &Arc<ClientHub>,
        profile: P,
        wrap: impl FnOnce(Arc<dyn ClusterCacheBackend>) -> Arc<dyn ClusterCacheBackend>,
    ) -> Self {
        let standalone = StandaloneClusterPlugin::builder()
            .build_and_start()
            .expect("standalone cluster backend");
        let cluster = ClusterWiring::builder(Arc::clone(hub))
            .profile(profile, ProfileBackends::new(wrap(standalone.cache())))
            .build_and_start()
            .expect("cluster wiring");
        Self {
            cluster,
            standalone,
        }
    }

    /// The standalone backend behind a [`ScriptedCache`] `script` sets up,
    /// under the `oagw` profile in `hub`.
    pub(crate) fn scripted(
        hub: &Arc<ClientHub>,
        script: impl FnOnce(ScriptedCache) -> ScriptedCache,
    ) -> (Self, Arc<ScriptedCache>) {
        let mut scripted = None;
        let fixture = Self::with(hub, OagwProfile, |cache| {
            let cache = Arc::new(script(ScriptedCache::new(cache)));
            scripted = Some(Arc::clone(&cache));
            cache
        });
        (fixture, scripted.expect("wired"))
    }

    /// Deregisters the backends and stops the standalone sweeper.
    pub(crate) async fn stop(self) {
        self.cluster.stop().await;
        self.standalone.stop().await;
    }
}

/// The resolver of the gear's cluster profile in `hub`, as the gear hands
/// it to `ChangeFeed::start`.
pub(crate) fn resolver(hub: &ClientHub) -> CacheResolverBuilder<'_> {
    ClusterCacheV1::resolver(hub).profile(OagwProfile)
}

/// The `oagw` cache facade in `hub`, scoped as the gear scopes it.
pub(crate) async fn facade(hub: &ClientHub) -> ClusterCacheV1 {
    resolver(hub)
        .resolve()
        .await
        .expect("the oagw profile")
        .scoped(SCOPE)
        .expect("scope")
}

/// The change log in `cluster`.
pub(crate) async fn read_log(cluster: &ClusterCacheV1) -> Option<ChangeLog> {
    let entry = cluster.get(LOG_KEY).await.expect("read the log")?;
    Some(ChangeLog::decode(&entry.value).expect("a readable log"))
}

/// Store `value` as the change log in `cluster`.
pub(crate) async fn put_log(cluster: &ClusterCacheV1, value: &[u8]) {
    cluster
        .put(PutRequest {
            key: LOG_KEY,
            value,
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("write the log");
}

/// A publisher appending to the log in `hub`'s `oagw` cache as `origin`,
/// until the token is cancelled.
pub(crate) async fn publisher(hub: &ClientHub, origin: Uuid) -> (Publisher, CancellationToken) {
    publisher_recording(hub, origin, Arc::new(NoopMetrics)).await
}

/// [`publisher`], counting failed appends in `metrics`.
pub(crate) async fn publisher_recording(
    hub: &ClientHub,
    origin: Uuid,
    metrics: Arc<dyn OagwMetricsPort>,
) -> (Publisher, CancellationToken) {
    publisher_reporting(hub, origin, metrics, WatchStatus::default()).await
}

/// [`publisher_recording`], reporting a newer log it skips to `status`.
pub(crate) async fn publisher_reporting(
    hub: &ClientHub,
    origin: Uuid,
    metrics: Arc<dyn OagwMetricsPort>,
    status: WatchStatus,
) -> (Publisher, CancellationToken) {
    let cancel = CancellationToken::new();
    let (publisher, _task) =
        Publisher::spawn(facade(hub).await, origin, metrics, status, cancel.clone());
    (publisher, cancel)
}

/// An enabled HTTPS upstream in `tenant_id` with `alias`.
pub(crate) fn upstream(tenant_id: Uuid, alias: &str) -> Upstream {
    Upstream {
        id: Uuid::new_v4(),
        tenant_id,
        alias: alias.into(),
        server: Server {
            endpoints: vec![Endpoint {
                scheme: Scheme::Https,
                host: "api.example.com".into(),
                port: 443,
            }],
        },
        protocol: HTTP_PROTOCOL_ID.into(),
        enabled: true,
        auth: None,
        headers: None,
        plugins: None,
        rate_limit: None,
        cors: None,
        tags: vec![],
        managed_by: ManagedBy::Api,
    }
}

/// A transient cluster error.
pub(crate) fn connection_lost() -> ClusterError {
    ClusterError::Provider {
        kind: ProviderErrorKind::ConnectionLost,
        message: "scripted: connection lost".to_owned(),
    }
}

/// Wait until `done` holds; a 10 s budget on the (possibly paused) clock.
pub(crate) async fn until(what: &str, mut done: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting until {what}"));
}

/// A backend over another that counts reads and writes, and does what the
/// test scripts.
pub(crate) struct ScriptedCache {
    inner: Arc<dyn ClusterCacheBackend>,
    /// Watches are channels the test drives instead of the inner backend's.
    manual_watch: bool,
    /// Declares no watch support.
    without_watch: bool,
    watches: Mutex<Vec<CacheWatchSender>>,
    watch_calls: AtomicUsize,
    failing_watches: AtomicUsize,
    gets: AtomicUsize,
    writes: AtomicUsize,
    conflicts: AtomicUsize,
    injected_conflicts: AtomicUsize,
    write_error: Mutex<Option<ClusterError>>,
    /// The next reads meet here after they read.
    meeting: Mutex<Option<(usize, Arc<Barrier>)>>,
    /// Reads wait for [`release`](Self::release) while held.
    held: Mutex<bool>,
    /// The next reads never answer.
    hanging: AtomicUsize,
    /// Reads waiting for [`release`](Self::release).
    held_reads: AtomicUsize,
    released: Notify,
}

impl ScriptedCache {
    /// Over `inner`, scripting nothing yet.
    pub(crate) fn new(inner: Arc<dyn ClusterCacheBackend>) -> Self {
        Self {
            inner,
            manual_watch: false,
            without_watch: false,
            watches: Mutex::new(Vec::new()),
            watch_calls: AtomicUsize::new(0),
            failing_watches: AtomicUsize::new(0),
            gets: AtomicUsize::new(0),
            writes: AtomicUsize::new(0),
            conflicts: AtomicUsize::new(0),
            injected_conflicts: AtomicUsize::new(0),
            write_error: Mutex::new(None),
            meeting: Mutex::new(None),
            held: Mutex::new(false),
            hanging: AtomicUsize::new(0),
            held_reads: AtomicUsize::new(0),
            released: Notify::new(),
        }
    }

    /// Hand out watches the test drives with [`send`](Self::send).
    pub(crate) fn with_manual_watch(mut self) -> Self {
        self.manual_watch = true;
        self
    }

    /// Declare no watch support.
    pub(crate) fn without_watch(mut self) -> Self {
        self.without_watch = true;
        self
    }

    /// Reads so far.
    pub(crate) fn gets(&self) -> usize {
        self.gets.load(Ordering::SeqCst)
    }

    /// Writes that landed so far.
    pub(crate) fn writes(&self) -> usize {
        self.writes.load(Ordering::SeqCst)
    }

    /// Compare-and-swaps that conflicted so far.
    pub(crate) fn conflicts(&self) -> usize {
        self.conflicts.load(Ordering::SeqCst)
    }

    /// Watches asked for so far, including failed ones.
    pub(crate) fn watch_calls(&self) -> usize {
        self.watch_calls.load(Ordering::SeqCst)
    }

    /// The next `n` compare-and-swaps conflict.
    pub(crate) fn conflict_next(&self, n: usize) {
        self.injected_conflicts.store(n, Ordering::SeqCst);
    }

    /// Every write fails with `error`, or works again with `None`.
    pub(crate) fn fail_writes(&self, error: Option<ClusterError>) {
        *self.write_error.lock() = error;
    }

    /// The next `n` watches fail with a transient error.
    pub(crate) fn fail_watches(&self, n: usize) {
        self.failing_watches.store(n, Ordering::SeqCst);
    }

    /// The next `n` reads wait for each other after reading, so they all
    /// read the same entry before any of them goes on.
    pub(crate) fn meet(&self, n: usize) {
        *self.meeting.lock() = Some((n, Arc::new(Barrier::new(n))));
    }

    /// Reads wait from now on until [`release`](Self::release).
    pub(crate) fn hold(&self) {
        *self.held.lock() = true;
    }

    /// How many reads wait for [`release`](Self::release) now.
    pub(crate) fn held_reads(&self) -> usize {
        self.held_reads.load(Ordering::SeqCst)
    }

    /// The next `n` reads never answer; the ones after them do.
    pub(crate) fn hang_next(&self, n: usize) {
        self.hanging.store(n, Ordering::SeqCst);
    }

    /// Let held and later reads go.
    pub(crate) fn release(&self) {
        *self.held.lock() = false;
        self.released.notify_waiters();
    }

    /// Deliver `event` on the latest manual watch.
    pub(crate) async fn send(&self, event: CacheWatchEvent) {
        let sender = self.watches.lock().last().cloned().expect("a manual watch");
        sender.send(event).await.expect("the watch is open");
    }

    /// Deliver a change of the log on the latest manual watch.
    pub(crate) async fn send_changed(&self) {
        self.send(CacheWatchEvent::Event(CacheEvent::Changed {
            key: BACKEND_LOG_KEY.to_owned(),
        }))
        .await;
    }

    /// Whether the watcher has dropped the latest manual watch.
    pub(crate) fn watch_closed(&self) -> bool {
        self.watches
            .lock()
            .last()
            .expect("a manual watch")
            .is_closed()
    }

    /// End the latest manual watch, as a backend dropping it does.
    pub(crate) fn end_watch(&self) {
        self.watches.lock().pop().expect("a manual watch");
    }

    async fn wait_released(&self) {
        let hangs = self
            .hanging
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        if hangs {
            std::future::pending::<()>().await;
        }
        loop {
            let released = self.released.notified();
            if !*self.held.lock() {
                break;
            }
            self.held_reads.fetch_add(1, Ordering::SeqCst);
            released.await;
            self.held_reads.fetch_sub(1, Ordering::SeqCst);
        }
    }

    async fn meet_others(&self) {
        let barrier = {
            let mut meeting = self.meeting.lock();
            match meeting.take() {
                Some((n, barrier)) => {
                    if n > 1 {
                        *meeting = Some((n - 1, Arc::clone(&barrier)));
                    }
                    Some(barrier)
                }
                None => None,
            }
        };
        if let Some(barrier) = barrier {
            barrier.wait().await;
        }
    }

    fn write_error(&self) -> Result<(), ClusterError> {
        self.write_error.lock().clone().map_or(Ok(()), Err)
    }
}

#[async_trait]
impl ClusterCacheBackend for ScriptedCache {
    fn consistency(&self) -> CacheConsistency {
        self.inner.consistency()
    }

    fn features(&self) -> CacheFeatures {
        if self.without_watch {
            CacheFeatures::without_watch()
        } else {
            self.inner.features()
        }
    }

    async fn get(&self, key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        self.wait_released().await;
        let entry = self.inner.get(key).await;
        self.meet_others().await;
        self.gets.fetch_add(1, Ordering::SeqCst);
        entry
    }

    async fn put(&self, req: PutRequest<'_>) -> Result<(), ClusterError> {
        self.inner.put(req).await
    }

    async fn delete(&self, key: &str) -> Result<bool, ClusterError> {
        self.inner.delete(key).await
    }

    async fn contains(&self, key: &str) -> Result<bool, ClusterError> {
        self.inner.contains(key).await
    }

    async fn put_if_absent(&self, req: PutRequest<'_>) -> Result<Option<CacheEntry>, ClusterError> {
        self.write_error()?;
        let created = self.inner.put_if_absent(req).await?;
        if created.is_some() {
            self.writes.fetch_add(1, Ordering::SeqCst);
        }
        Ok(created)
    }

    async fn compare_and_swap(
        &self,
        key: &str,
        expected_version: u64,
        new_value: &[u8],
        ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        self.write_error()?;
        let injected = self
            .injected_conflicts
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        let swapped = if injected {
            Err(ClusterError::CasConflict {
                key: key.to_owned(),
                current: None,
            })
        } else {
            self.inner
                .compare_and_swap(key, expected_version, new_value, ttl)
                .await
        };
        match &swapped {
            Ok(_) => self.writes.fetch_add(1, Ordering::SeqCst),
            Err(ClusterError::CasConflict { .. }) => self.conflicts.fetch_add(1, Ordering::SeqCst),
            Err(_) => 0,
        };
        swapped
    }

    async fn watch(&self, key: &str) -> Result<CacheWatch, ClusterError> {
        self.watch_calls.fetch_add(1, Ordering::SeqCst);
        if self
            .failing_watches
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(connection_lost());
        }
        if !self.manual_watch {
            return self.inner.watch(key).await;
        }
        let (sender, watch) = CacheWatch::channel(16);
        self.watches.lock().push(sender);
        Ok(watch)
    }

    async fn watch_prefix(&self, prefix: &str) -> Result<CacheWatch, ClusterError> {
        self.inner.watch_prefix(prefix).await
    }
}
