use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use super::ScopedCacheBackend;
use crate::cache::backend::ClusterCacheBackend;
use crate::cache::types::{
    CacheConsistency, CacheEntry, CacheEvent, CacheFeatures, PutRequest, Ttl,
};
use crate::cache::watch::{CacheWatch, CacheWatchEvent, CacheWatchSender};
use crate::error::ClusterError;
use crate::scope;

/// A stub cache that records the keys it is asked about, seeds a fixed
/// keyspace for `scan_prefix`, and emits one `Changed` event (carrying the
/// backend-facing key) on `watch`/`watch_prefix`.
struct RecordingCache {
    seen: Mutex<Vec<String>>,
    keys: Vec<String>,
}

impl RecordingCache {
    fn new() -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            keys: vec![
                "event-broker/a".to_owned(),
                "event-broker/b".to_owned(),
                "other/c".to_owned(),
            ],
        }
    }
}

#[async_trait]
impl ClusterCacheBackend for RecordingCache {
    fn consistency(&self) -> CacheConsistency {
        CacheConsistency::Linearizable
    }

    fn features(&self) -> CacheFeatures {
        CacheFeatures::new(true)
    }

    async fn get(&self, key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        self.seen.lock().expect("lock").push(key.to_owned());
        Ok(None)
    }

    async fn put(&self, req: PutRequest<'_>) -> Result<(), ClusterError> {
        self.seen.lock().expect("lock").push(req.key.to_owned());
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<bool, ClusterError> {
        self.seen.lock().expect("lock").push(key.to_owned());
        Ok(true)
    }

    async fn contains(&self, key: &str) -> Result<bool, ClusterError> {
        self.seen.lock().expect("lock").push(key.to_owned());
        Ok(false)
    }

    async fn put_if_absent(&self, req: PutRequest<'_>) -> Result<Option<CacheEntry>, ClusterError> {
        self.seen.lock().expect("lock").push(req.key.to_owned());
        Ok(Some(CacheEntry {
            value: Vec::new(),
            version: 1,
        }))
    }

    async fn compare_and_swap(
        &self,
        key: &str,
        _expected_version: u64,
        _new_value: &[u8],
        _ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        self.seen.lock().expect("lock").push(key.to_owned());
        Ok(CacheEntry {
            value: Vec::new(),
            version: 2,
        })
    }

    async fn watch(&self, key: &str) -> Result<CacheWatch, ClusterError> {
        self.seen.lock().expect("lock").push(key.to_owned());
        let (tx, watch) = CacheWatch::channel(8);
        // Emit one event carrying the backend-facing (prefixed) key, then end.
        tx.send(CacheWatchEvent::Event(CacheEvent::Changed {
            key: key.to_owned(),
        }))
        .await
        .ok();
        Ok(watch)
    }

    async fn watch_prefix(&self, prefix: &str) -> Result<CacheWatch, ClusterError> {
        self.seen.lock().expect("lock").push(prefix.to_owned());
        let (tx, watch) = CacheWatch::channel(8);
        let event_key = format!("{prefix}item");
        tx.send(CacheWatchEvent::Event(CacheEvent::Changed {
            key: event_key,
        }))
        .await
        .ok();
        Ok(watch)
    }

    async fn scan_prefix(&self, prefix: &str) -> Result<Vec<String>, ClusterError> {
        self.seen.lock().expect("lock").push(prefix.to_owned());
        Ok(self
            .keys
            .iter()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect())
    }

    async fn probe(&self) -> Result<(), ClusterError> {
        // Records the call under a name no key could take, so the forwarding test
        // can tell a forwarded probe from the trait's `Ok(())` default.
        self.seen.lock().expect("lock").push("<probe>".to_owned());
        Ok(())
    }
}

fn scoped<B: ClusterCacheBackend + 'static>(inner: Arc<B>, prefix: &str) -> ScopedCacheBackend {
    ScopedCacheBackend::new(
        inner,
        scope::validated_prefix(prefix).expect("valid prefix"),
    )
}

#[tokio::test]
async fn write_path_prepends_the_prefix() {
    let cache = Arc::new(RecordingCache::new());
    let wrapper = scoped(Arc::clone(&cache), "event-broker");
    assert!(
        wrapper
            .put(PutRequest {
                key: "shard-assignments",
                value: b"v",
                ttl: Ttl::Indefinite,
            })
            .await
            .is_ok()
    );
    assert!(wrapper.get("shard-assignments").await.is_ok());
    assert_eq!(
        cache.seen.lock().expect("lock").as_slice(),
        [
            "event-broker/shard-assignments",
            "event-broker/shard-assignments"
        ]
    );
}

#[tokio::test]
async fn watch_strips_the_prefix_from_event_keys() {
    let cache = Arc::new(RecordingCache::new());
    let wrapper = scoped(Arc::clone(&cache), "event-broker");
    let mut watch = wrapper.watch("shard-assignments").await.expect("watch");
    // The backend saw the prefixed key...
    assert_eq!(
        cache.seen.lock().expect("lock").as_slice(),
        ["event-broker/shard-assignments"]
    );
    // ...but the consumer sees the name relative to its scope.
    match watch.recv().await {
        Some(CacheWatchEvent::Event(CacheEvent::Changed { key })) => {
            assert_eq!(key, "shard-assignments");
        }
        other => panic!("expected a stripped Changed event, got {other:?}"),
    }
}

#[tokio::test]
async fn watch_prefix_strips_the_prefix_from_event_keys() {
    let cache = Arc::new(RecordingCache::new());
    let wrapper = scoped(Arc::clone(&cache), "event-broker");
    // Watch the whole scope (relative prefix "").
    let mut watch = wrapper.watch_prefix("").await.expect("watch_prefix");
    assert_eq!(
        cache.seen.lock().expect("lock").as_slice(),
        ["event-broker/"]
    );
    match watch.recv().await {
        Some(CacheWatchEvent::Event(CacheEvent::Changed { key })) => {
            assert_eq!(key, "item");
        }
        other => panic!("expected a stripped Changed event, got {other:?}"),
    }
}

#[tokio::test]
async fn scan_prefix_strips_the_prefix_from_returned_keys() {
    let cache = Arc::new(RecordingCache::new());
    let wrapper = scoped(Arc::clone(&cache), "event-broker");
    let mut keys = wrapper.scan_prefix("").await.expect("scan");
    keys.sort();
    assert_eq!(keys, ["a", "b"]);
}

#[tokio::test]
async fn scoping_composes_when_nested() {
    let cache = Arc::new(RecordingCache::new());
    let inner = scoped(Arc::clone(&cache), "event-broker");
    let outer = ScopedCacheBackend::new(
        Arc::new(inner),
        scope::validated_prefix("shard-0").expect("valid"),
    );
    assert!(
        outer
            .put(PutRequest {
                key: "k",
                value: b"v",
                ttl: Ttl::Indefinite,
            })
            .await
            .is_ok()
    );
    assert_eq!(
        cache.seen.lock().expect("lock").as_slice(),
        ["event-broker/shard-0/k"]
    );
}

/// A probe carries no key, so scoping has nothing to apply — but it must still be
/// forwarded, including through nesting, or a scoped view answers the trait's
/// `Ok(())` default over an unreachable backend.
#[tokio::test]
async fn probe_is_forwarded_through_every_scoping_layer() {
    let cache = Arc::new(RecordingCache::new());
    let inner = scoped(Arc::clone(&cache), "event-broker");
    let outer = ScopedCacheBackend::new(
        Arc::new(inner),
        scope::validated_prefix("shard-0").expect("valid"),
    );

    assert!(outer.probe().await.is_ok());
    assert_eq!(
        cache.seen.lock().expect("lock").as_slice(),
        ["<probe>"],
        "the probe reached the real backend, unprefixed"
    );
}

// Scoping combined with `watch`, against a keyspace two handles share
//
// `RecordingCache` above fabricates one event per subscription, which is enough
// to see the strip but cannot show whether two handles onto the same backend
// alias each other. The name-space isolation the default lease backends will
// rely on (a scoped cache handed to leader election, whose `watch` must not see
// an unscoped writer at the same logical key) is only observable against a
// backend that fans real mutations out to whichever watchers are live.

/// The subscription kind a fixture watcher matches event keys against.
enum WatchKind {
    Exact(String),
    Prefix(String),
}

impl WatchKind {
    fn matches(&self, key: &str) -> bool {
        match self {
            Self::Exact(exact) => exact == key,
            Self::Prefix(prefix) => key.starts_with(prefix.as_str()),
        }
    }
}

/// A watch-only cache: `put`/`delete` store nothing and merely fan the mutation
/// out to the matching live watchers, which is all the read-path strip layer can
/// observe. The methods no watch test exercises answer `Unsupported` rather than
/// a plausible-looking lie.
struct FanoutCache {
    watchers: Mutex<Vec<(WatchKind, CacheWatchSender)>>,
}

impl FanoutCache {
    fn new() -> Self {
        Self {
            watchers: Mutex::new(Vec::new()),
        }
    }

    fn subscribe(&self, kind: WatchKind) -> CacheWatch {
        let (tx, watch) = CacheWatch::channel(16);
        self.watchers.lock().expect("lock").push((kind, tx));
        watch
    }

    /// Delivers `event` to every watcher whose subscription matches its key.
    /// Non-blocking, so the watcher list is never held across an await.
    fn fanout(&self, event: &CacheEvent) {
        for (kind, tx) in &*self.watchers.lock().expect("lock") {
            if kind.matches(event.key()) {
                tx.try_send(CacheWatchEvent::Event(event.clone()))
                    .expect("watcher buffer has room");
            }
        }
    }

    /// Emits the `Expired` a TTL sweep would. The fixture models no clock, and
    /// the strip layer cannot tell a swept expiry from an injected one.
    fn expire(&self, key: &str) {
        self.fanout(&CacheEvent::Expired {
            key: key.to_owned(),
        });
    }

    /// Delivers a key-less lifecycle signal to every watcher.
    fn signal(&self, event: &CacheWatchEvent) {
        for (_, tx) in &*self.watchers.lock().expect("lock") {
            tx.try_send(event.clone()).expect("watcher buffer has room");
        }
    }

    /// Ends every subscription by dropping the fixture's senders, as a backend
    /// tearing its watches down does.
    fn end_all_watches(&self) {
        self.watchers.lock().expect("lock").clear();
    }
}

#[async_trait]
impl ClusterCacheBackend for FanoutCache {
    fn consistency(&self) -> CacheConsistency {
        CacheConsistency::Linearizable
    }

    fn features(&self) -> CacheFeatures {
        CacheFeatures::new(true)
    }

    async fn get(&self, _key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        Err(ClusterError::Unsupported { feature: "get" })
    }

    async fn put(&self, req: PutRequest<'_>) -> Result<(), ClusterError> {
        self.fanout(&CacheEvent::Changed {
            key: req.key.to_owned(),
        });
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<bool, ClusterError> {
        self.fanout(&CacheEvent::Deleted {
            key: key.to_owned(),
        });
        Ok(true)
    }

    async fn contains(&self, _key: &str) -> Result<bool, ClusterError> {
        Err(ClusterError::Unsupported {
            feature: "contains",
        })
    }

    async fn put_if_absent(
        &self,
        _req: PutRequest<'_>,
    ) -> Result<Option<CacheEntry>, ClusterError> {
        Err(ClusterError::Unsupported {
            feature: "put_if_absent",
        })
    }

    async fn compare_and_swap(
        &self,
        _key: &str,
        _expected_version: u64,
        _new_value: &[u8],
        _ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        Err(ClusterError::Unsupported {
            feature: "compare_and_swap",
        })
    }

    async fn watch(&self, key: &str) -> Result<CacheWatch, ClusterError> {
        Ok(self.subscribe(WatchKind::Exact(key.to_owned())))
    }

    async fn watch_prefix(&self, prefix: &str) -> Result<CacheWatch, ClusterError> {
        Ok(self.subscribe(WatchKind::Prefix(prefix.to_owned())))
    }
}

/// How long a bounded `recv` waits before it is treated as "no event". Only ever
/// reached on a failing assertion, so it costs nothing when the tests pass.
const RECV_TIMEOUT: Duration = Duration::from_secs(5);

/// Bounds a `recv` so a wrapper that subscribed to the wrong physical key fails
/// the test instead of hanging it — the forwarding task delivers asynchronously,
/// so a missing event is otherwise indistinguishable from a slow one.
async fn next_event(watch: &mut CacheWatch) -> Option<CacheWatchEvent> {
    tokio::time::timeout(RECV_TIMEOUT, watch.recv())
        .await
        .expect("an event (or end of stream) within the timeout")
}

fn put(key: &str) -> PutRequest<'_> {
    PutRequest {
        key,
        value: b"v",
        ttl: Ttl::Indefinite,
    }
}

/// The isolation property the default lease backends will depend on once the
/// cache they are handed is scoped: a writer outside the scope touching the same
/// *logical* name must be invisible to a scoped watcher, or a foreign write
/// would look like a lease change.
///
/// The two writes deliberately use different event kinds: after the strip, a
/// leaked `Deleted { key: "leader" }` and the legitimate
/// `Changed { key: "leader" }` are otherwise identical. The fixture fans out in
/// call order, so a leak arrives first and is caught by asserting on the first
/// event rather than by waiting for an absence.
#[tokio::test]
async fn watch_does_not_observe_a_write_outside_the_scope() {
    let cache = Arc::new(FanoutCache::new());
    let wrapper = scoped(Arc::clone(&cache), "event-broker");
    let mut watch = wrapper.watch("leader").await.expect("watch");

    // Unscoped, same logical key: lands on "leader", not "event-broker/leader".
    assert!(cache.delete("leader").await.is_ok());
    // Scoped: lands on "event-broker/leader".
    assert!(wrapper.put(put("leader")).await.is_ok());

    match next_event(&mut watch).await {
        Some(CacheWatchEvent::Event(CacheEvent::Changed { key })) => assert_eq!(key, "leader"),
        other => panic!("an unscoped write at the same logical key leaked in: {other:?}"),
    }
}

/// The mirror image: a scoped write really does land on the prefixed physical
/// key, and a watcher at that physical name sees it unstripped. Together with
/// the isolation test this pins both halves of the translation, so neither can
/// be dropped without a failure.
#[tokio::test]
async fn a_scoped_write_reaches_an_unscoped_watcher_at_the_physical_key() {
    let cache = Arc::new(FanoutCache::new());
    let wrapper = scoped(Arc::clone(&cache), "event-broker");
    let mut watch = cache.watch("event-broker/leader").await.expect("watch");

    assert!(wrapper.put(put("leader")).await.is_ok());

    match next_event(&mut watch).await {
        Some(CacheWatchEvent::Event(CacheEvent::Changed { key })) => {
            assert_eq!(key, "event-broker/leader");
        }
        other => panic!("expected the physical key at the backend, got {other:?}"),
    }
}

/// Every event variant carries a key, so every variant must be stripped. A
/// consumer that sees a raw physical key on one variant only (an `Expired`,
/// say) would fail to match it against the name it asked about.
#[tokio::test]
async fn watch_strips_the_prefix_from_every_event_variant() {
    let cache = Arc::new(FanoutCache::new());
    let wrapper = scoped(Arc::clone(&cache), "event-broker");
    let mut watch = wrapper.watch("leader").await.expect("watch");

    assert!(wrapper.put(put("leader")).await.is_ok());
    assert!(wrapper.delete("leader").await.is_ok());
    cache.expire("event-broker/leader");

    assert!(matches!(
        next_event(&mut watch).await,
        Some(CacheWatchEvent::Event(CacheEvent::Changed { key })) if key == "leader"
    ));
    assert!(matches!(
        next_event(&mut watch).await,
        Some(CacheWatchEvent::Event(CacheEvent::Deleted { key })) if key == "leader"
    ));
    assert!(matches!(
        next_event(&mut watch).await,
        Some(CacheWatchEvent::Event(CacheEvent::Expired { key })) if key == "leader"
    ));
}

/// Two sibling scopes over one backend: a prefix watch must see its own scope
/// and nothing else. A leak is distinguishable by key here — `scope::strip`
/// passes a key it does not own through unchanged, so a leaked sibling event
/// would arrive as "event-broker-2/leader".
///
/// The sibling's name **string-prefix-overlaps** ours on purpose, and that is
/// the whole assertion: with two unrelated names ("other-gear") the test passes
/// even against a wrapper that subscribes without its trailing separator, so it
/// proves only that unrelated scopes do not alias — which nothing threatens.
/// `event-broker-2/` starts with `event-broker`, so a wrapper that trimmed the
/// separator would subscribe to `event-broker` and swallow the sibling's write.
#[tokio::test]
async fn watch_prefix_sees_only_its_own_scope() {
    let cache = Arc::new(FanoutCache::new());
    let ours = scoped(Arc::clone(&cache), "event-broker");
    let theirs = scoped(Arc::clone(&cache), "event-broker-2");
    // Watch the whole scope (relative prefix "").
    let mut watch = ours.watch_prefix("").await.expect("watch_prefix");

    assert!(theirs.put(put("leader")).await.is_ok());
    assert!(ours.put(put("leader")).await.is_ok());

    match next_event(&mut watch).await {
        Some(CacheWatchEvent::Event(CacheEvent::Changed { key })) => assert_eq!(key, "leader"),
        other => panic!("a sibling scope's write leaked in: {other:?}"),
    }
}

/// The lifecycle signals carry no key, so the strip layer must forward them
/// verbatim. Swallowing a `Lagged` is the dangerous case: the consumer would
/// believe its view is current when events were in fact lost.
#[tokio::test]
async fn lifecycle_signals_are_forwarded_unchanged() {
    let cache = Arc::new(FanoutCache::new());
    let wrapper = scoped(Arc::clone(&cache), "event-broker");
    let mut watch = wrapper.watch("leader").await.expect("watch");

    cache.signal(&CacheWatchEvent::Lagged { dropped: 7 });
    cache.signal(&CacheWatchEvent::Reset);
    cache.signal(&CacheWatchEvent::Closed(ClusterError::Unsupported {
        feature: "watch",
    }));

    assert!(matches!(
        next_event(&mut watch).await,
        Some(CacheWatchEvent::Lagged { dropped: 7 })
    ));
    assert!(matches!(
        next_event(&mut watch).await,
        Some(CacheWatchEvent::Reset)
    ));
    assert!(matches!(
        next_event(&mut watch).await,
        Some(CacheWatchEvent::Closed(ClusterError::Unsupported { feature })) if feature == "watch"
    ));
}

/// The forwarding task owns the consumer's sender, so it has to drop it when the
/// inner watch ends — otherwise the consumer's `recv()` never returns `None` and
/// a caller looping until end of stream hangs forever.
#[tokio::test]
async fn forwarding_ends_when_the_inner_watch_ends() {
    let cache = Arc::new(FanoutCache::new());
    let wrapper = scoped(Arc::clone(&cache), "event-broker");
    let mut watch = wrapper.watch("leader").await.expect("watch");

    cache.end_all_watches();

    assert!(next_event(&mut watch).await.is_none());
}

/// The claim that scoping composes, on the watch path: each layer subscribes
/// through the next and strips its own prefix, so the innermost backend sees the
/// fully composed key and the consumer sees only its own name.
///
/// The out-of-scope write goes through the *inner* layer alone — physically
/// "event-broker/leader", one level short of the composed key — so a wrapper
/// that applied only one layer's prefix would observe it and fail here.
#[tokio::test]
async fn nested_scoping_strips_every_layer_from_watch_events() {
    let cache = Arc::new(FanoutCache::new());
    let inner = Arc::new(scoped(Arc::clone(&cache), "event-broker"));
    let outer = ScopedCacheBackend::new(
        Arc::clone(&inner) as Arc<dyn ClusterCacheBackend>,
        scope::validated_prefix("shard-0").expect("valid"),
    );
    let mut watch = outer.watch("leader").await.expect("watch");

    assert!(inner.delete("leader").await.is_ok());
    assert!(outer.put(put("leader")).await.is_ok());

    match next_event(&mut watch).await {
        Some(CacheWatchEvent::Event(CacheEvent::Changed { key })) => assert_eq!(key, "leader"),
        other => panic!("expected a doubly stripped Changed event, got {other:?}"),
    }
}

// The two seams the strip layer must carry across
//
// `strip_watch` hands the consumer a watch from `CacheWatch::channel`, which is
// bare: no observability stamp, no resubscribe seam. Both plugins wrap their
// native cache in `InstrumentedCache` at the *bottom*, so a scoped view over a
// plugin backend is `Scoped(Instrumented(raw))` and the stamp is applied below
// this layer — the ordering the cluster gear cannot invert, because the plugin
// hands it an already-instrumented `Arc`. Carrying both seams across is what
// makes that ordering harmless.

/// A `ClusterMetrics` that counts only the signal these tests ask about.
#[derive(Default)]
struct CountingMetrics {
    resets: std::sync::atomic::AtomicUsize,
}

impl CountingMetrics {
    fn resets(&self) -> usize {
        self.resets.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl crate::observability::ClusterMetrics for CountingMetrics {
    fn cache_op(&self, _op: &str, _result: &str) {}
    fn cache_op_duration(&self, _op: &str, _seconds: f64) {}
    fn lock_op(&self, _op: &str, _result: &str) {}
    fn lock_op_duration(&self, _op: &str, _seconds: f64) {}
    fn leader_transition(&self, _transition: &str) {}
    fn watch_reset(&self, _primitive: &str) {
        self.resets
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    fn provider_error(&self, _kind: &str) {}
}

/// Reconnects immediately and at most once, so a reconnect test costs no wall
/// clock and an unexpected second attempt cannot mask a failure.
fn immediate_retry() -> crate::restart::RetryPolicy {
    crate::restart::RetryPolicy {
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(1),
        jitter_factor: 0.0,
        max_retries: Some(1),
    }
}

/// The stamp `InstrumentedCache::watch` applies below the strip layer must
/// survive it. Asserted through the real wrapper stack rather than on the field,
/// because the ordering is the thing under test: `Scoped(Instrumented(raw))` is
/// what the plugins force, and it is the ordering that used to lose the stamp.
#[tokio::test]
async fn a_scoped_watch_keeps_the_observability_stamp_applied_below_it() {
    let raw: Arc<dyn ClusterCacheBackend> = Arc::new(FanoutCache::new());
    let metrics = Arc::new(CountingMetrics::default());
    let instrumented: Arc<dyn ClusterCacheBackend> = Arc::new(
        crate::observability::InstrumentedCache::new(raw, "fanout", Arc::clone(&metrics) as _),
    );
    let wrapper = ScopedCacheBackend::new(
        instrumented,
        scope::validated_prefix("event-broker").expect("valid"),
    );

    let watch = wrapper.watch("leader").await.expect("watch");

    let (provider, _) = watch
        .observability_context()
        .expect("the stamp applied below the strip layer must reach the consumer");
    assert_eq!(provider, "fanout");
}

/// The consequence of the stamp, and the reason it is worth carrying: an
/// `auto_restart`ed scoped watch emits `cluster_watch_resets_total` on a
/// reconnect. Without the carry-across it reconnected silently — the pre-existing
/// gap (`M10`), which the same change closes.
///
/// The second assertion is the one that makes carrying the *resubscribe* seam
/// safe rather than merely possible: the reconnected subscription still speaks
/// the consumer's name space. A seam copied across instead of re-wrapped would
/// deliver "event-broker/leader" here — the strip layer undone by its own
/// recovery path.
#[tokio::test]
async fn a_reconnected_scoped_watch_reports_the_reset_and_still_strips() {
    let cache = Arc::new(FanoutCache::new());
    let metrics = Arc::new(CountingMetrics::default());

    // The inner watch as an instrumented backend with a resubscribe seam would
    // hand it over: stamped, and able to produce a fresh *backend-name-space*
    // subscription. Built directly so the test drives `strip_watch` itself.
    let mut inner = cache.subscribe(WatchKind::Exact("event-broker/leader".to_owned()));
    inner.set_observability("fanout", Arc::clone(&metrics) as _);
    let reconnect_to = Arc::clone(&cache);
    inner.set_resubscribe(move || {
        let cache = Arc::clone(&reconnect_to);
        Box::pin(
            async move { Ok(cache.subscribe(WatchKind::Exact("event-broker/leader".to_owned()))) },
        )
    });

    let outer = ScopedCacheBackend::strip_watch("event-broker/".to_owned(), inner);
    let mut restarting = outer.auto_restart(immediate_retry());

    // The backend drops its sender: the canonical reconnect trigger.
    cache.end_all_watches();

    let reset = tokio::time::timeout(RECV_TIMEOUT, restarting.recv())
        .await
        .expect("the reconnect completes within the timeout");
    assert!(
        matches!(reset, Some(CacheWatchEvent::Reset)),
        "a reconnect surfaces as a synthesized Reset, got {reset:?}"
    );
    assert_eq!(
        metrics.resets(),
        1,
        "the reconnect must emit the watch-reset signal the stamp carries"
    );

    // The fresh subscription is still scoped.
    cache.fanout(&CacheEvent::Changed {
        key: "event-broker/leader".to_owned(),
    });
    let event = tokio::time::timeout(RECV_TIMEOUT, restarting.recv())
        .await
        .expect("an event within the timeout");
    match event {
        Some(CacheWatchEvent::Event(CacheEvent::Changed { key })) => assert_eq!(key, "leader"),
        other => panic!("the reconnected watch stopped stripping: {other:?}"),
    }
}
