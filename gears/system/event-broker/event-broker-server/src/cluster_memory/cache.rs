use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use cluster_sdk::cache::{
    CacheConsistency, CacheEntry, CacheEvent, CacheFeatures, CacheTerminalPermit, CacheWatch,
    CacheWatchEvent, CacheWatchSender, CacheWatchTrySendError, ClusterCacheBackend, PutRequest,
    Ttl,
};
use cluster_sdk::error::ClusterError;
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit::async_trait;
use toolkit_taskward::{Directive, TaskSet, WorkerAction, WorkerBuilder, WorkerTask};

const WATCH_CAPACITY: usize = 256;
const SWEEPER_NAME: &str = "eb-memory-cache-expiry";

struct Record {
    entry: CacheEntry,
    written_at: Instant,
    ttl: Option<Duration>,
}

impl Record {
    fn new(value: &[u8], version: u64, ttl: Ttl, now: Instant) -> Self {
        Self {
            entry: CacheEntry {
                value: value.to_vec(),
                version,
            },
            written_at: now,
            ttl: ttl.as_duration(),
        }
    }

    fn is_expired(&self, now: Instant) -> bool {
        self.ttl
            .is_some_and(|ttl| now.saturating_duration_since(self.written_at) >= ttl)
    }
}

enum Selector {
    Key(String),
    Prefix(String),
}

impl Selector {
    fn matches(&self, key: &str) -> bool {
        match self {
            Self::Key(exact) => key == exact,
            Self::Prefix(prefix) => key.starts_with(prefix),
        }
    }
}

struct Subscription {
    selector: Selector,
    sender: CacheWatchSender,
    terminal: CacheTerminalPermit,
    dropped: u64,
}

impl Subscription {
    fn publish(&mut self, event: &CacheEvent) -> bool {
        if !self.selector.matches(event.key()) {
            return true;
        }

        // A lag notice must arrive before the next ordinary event so the consumer can re-read.
        if self.dropped != 0 {
            match self.sender.try_send(CacheWatchEvent::Lagged {
                dropped: self.dropped,
            }) {
                Ok(()) => self.dropped = 0,
                Err(CacheWatchTrySendError::Full) => {
                    self.dropped += 1;
                    return true;
                }
                Err(CacheWatchTrySendError::Closed) => return false,
            }
        }
        match self.sender.try_send(CacheWatchEvent::Event(event.clone())) {
            Ok(()) => true,
            Err(CacheWatchTrySendError::Full) => {
                self.dropped += 1;
                true
            }
            Err(CacheWatchTrySendError::Closed) => false,
        }
    }
}

#[derive(Default)]
struct CacheState {
    entries: HashMap<String, Record>,
    watchers: Vec<Subscription>,
    closed: bool,
}

impl CacheState {
    fn require_open(&self) -> Result<(), ClusterError> {
        if self.closed {
            Err(ClusterError::Shutdown)
        } else {
            Ok(())
        }
    }

    fn live(&self, key: &str, now: Instant) -> Option<&CacheEntry> {
        self.entries
            .get(key)
            .filter(|record| !record.is_expired(now))
            .map(|record| &record.entry)
    }

    fn publish(watchers: &mut Vec<Subscription>, event: &CacheEvent) {
        watchers.retain_mut(|watch| !watch.sender.is_closed() && watch.publish(event));
    }

    fn insert(&mut self, key: &str, record: Record) {
        self.entries.insert(key.to_owned(), record);
        Self::publish(
            &mut self.watchers,
            &CacheEvent::Changed {
                key: key.to_owned(),
            },
        );
    }

    fn swap(
        &mut self,
        key: &str,
        matches: impl FnOnce(&CacheEntry) -> bool,
        new_value: &[u8],
        ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        let now = Instant::now();
        let current = self.live(key, now);
        if let Some(entry) = current.filter(|entry| matches(entry)) {
            let record = Record::new(new_value, entry.version + 1, ttl, now);
            let updated = record.entry.clone();
            self.insert(key, record);
            Ok(updated)
        } else {
            Err(ClusterError::CasConflict {
                key: key.to_owned(),
                current: current.cloned(),
            })
        }
    }
}

/// The linearizable cache backend for a single in-process profile.
pub struct MemoryCache {
    state: Mutex<CacheState>,
    #[cfg(test)]
    sweep_counts: Arc<super::client::WorkerCounts>,
}

impl MemoryCache {
    /// Creates an empty cache without starting a worker.
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(CacheState::default()),
            #[cfg(test)]
            sweep_counts: Arc::default(),
        })
    }

    /// Builds a dormant expiry worker for the profile's validated positive interval.
    pub(crate) fn prepare_sweep(self: &Arc<Self>, interval: Duration) -> PreparedSweep {
        let cancel = CancellationToken::new();
        let wake = Arc::new(Notify::new());
        // Taskward starts idle, so the expiry action needs a permit to schedule its first pass.
        wake.notify_one();
        let action = ExpirySweep {
            cache: Arc::clone(self),
            interval,
        };
        let worker = WorkerBuilder::new(SWEEPER_NAME, cancel.clone())
            .notifier(wake)
            .build(action);
        PreparedSweep {
            cancel,
            worker,
            #[cfg(test)]
            counts: self.sweep_counts.clone(),
        }
    }

    /// Closes existing watches and rejects subsequent writes and subscriptions.
    pub(crate) fn shutdown(&self) {
        let mut state = self.lock();
        state.closed = true;
        for watch in std::mem::take(&mut state.watchers) {
            watch.terminal.close(ClusterError::Shutdown);
        }
    }

    pub(super) fn is_closed(&self) -> bool {
        self.lock().closed
    }

    #[cfg(test)]
    pub(crate) fn test_sweep_counts(&self) -> (usize, usize) {
        let (started, live, _) = self.sweep_counts.snapshot();
        (started, live)
    }

    fn lock(&self) -> MutexGuard<'_, CacheState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn register(&self, selector: Selector) -> Result<CacheWatch, ClusterError> {
        let mut state = self.lock();
        state.require_open()?;
        state.watchers.retain(|watch| !watch.sender.is_closed());
        let (sender, terminal, watch) = CacheWatch::channel_with_terminal_headroom(WATCH_CAPACITY);
        state.watchers.push(Subscription {
            selector,
            sender,
            terminal,
            dropped: 0,
        });
        Ok(watch)
    }

    fn sweep_expired(&self) {
        let mut state = self.lock();
        let CacheState {
            entries, watchers, ..
        } = &mut *state;
        let now = Instant::now();
        for (key, _) in entries.extract_if(|_, record| record.is_expired(now)) {
            CacheState::publish(watchers, &CacheEvent::Expired { key });
        }
    }
}

/// Owns an expiry worker until it can be started.
pub struct PreparedSweep {
    cancel: CancellationToken,
    worker: WorkerTask<ExpirySweep>,
    #[cfg(test)]
    counts: Arc<super::client::WorkerCounts>,
}

impl PreparedSweep {
    /// Starts the expiry worker and returns its cancellation and join owner.
    pub(crate) fn start(self) -> TaskSet {
        let mut tasks = TaskSet::new(self.cancel);
        #[cfg(test)]
        let tracking = self.counts.track();
        tasks.spawn(SWEEPER_NAME, async move {
            #[cfg(test)]
            let _tracking = tracking;
            self.worker.run().await;
        });
        tasks
    }
}

struct ExpirySweep {
    cache: Arc<MemoryCache>,
    interval: Duration,
}

impl WorkerAction for ExpirySweep {
    type Payload = ();
    type Error = Infallible;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, Self::Error> {
        self.cache.sweep_expired();
        Ok(Directive::sleep(self.interval))
    }
}

#[async_trait]
impl ClusterCacheBackend for MemoryCache {
    fn consistency(&self) -> CacheConsistency {
        CacheConsistency::Linearizable
    }

    fn features(&self) -> CacheFeatures {
        CacheFeatures::new(true)
    }

    async fn get(&self, key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        let state = self.lock();
        Ok(state.live(key, Instant::now()).cloned())
    }

    async fn put(&self, req: PutRequest<'_>) -> Result<(), ClusterError> {
        let mut state = self.lock();
        state.require_open()?;
        let now = Instant::now();
        let version = state
            .live(req.key, now)
            .map_or(1, |entry| entry.version + 1);
        state.insert(req.key, Record::new(req.value, version, req.ttl, now));
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<bool, ClusterError> {
        let mut state = self.lock();
        state.require_open()?;
        let now = Instant::now();
        let live = state
            .entries
            .remove(key)
            .is_some_and(|record| !record.is_expired(now));
        if live {
            CacheState::publish(
                &mut state.watchers,
                &CacheEvent::Deleted {
                    key: key.to_owned(),
                },
            );
        }
        Ok(live)
    }

    async fn contains(&self, key: &str) -> Result<bool, ClusterError> {
        let state = self.lock();
        Ok(state.live(key, Instant::now()).is_some())
    }

    async fn put_if_absent(&self, req: PutRequest<'_>) -> Result<Option<CacheEntry>, ClusterError> {
        let mut state = self.lock();
        state.require_open()?;
        let now = Instant::now();
        if state.live(req.key, now).is_some() {
            return Ok(None);
        }
        let record = Record::new(req.value, 1, req.ttl, now);
        let entry = record.entry.clone();
        state.insert(req.key, record);
        Ok(Some(entry))
    }

    async fn compare_and_swap(
        &self,
        key: &str,
        expected_version: u64,
        new_value: &[u8],
        ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        let mut state = self.lock();
        state.require_open()?;
        state.swap(
            key,
            |entry| entry.version == expected_version,
            new_value,
            ttl,
        )
    }

    async fn compare_and_swap_value(
        &self,
        key: &str,
        expected_value: &[u8],
        new_value: &[u8],
        ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        let mut state = self.lock();
        state.require_open()?;
        state.swap(key, |entry| entry.value == expected_value, new_value, ttl)
    }

    async fn compare_and_delete(
        &self,
        key: &str,
        expected_value: &[u8],
    ) -> Result<bool, ClusterError> {
        let mut state = self.lock();
        state.require_open()?;
        let now = Instant::now();
        let matches = state
            .live(key, now)
            .is_some_and(|entry| entry.value == expected_value);
        if matches {
            state.entries.remove(key);
            CacheState::publish(
                &mut state.watchers,
                &CacheEvent::Deleted {
                    key: key.to_owned(),
                },
            );
        }
        Ok(matches)
    }

    async fn watch(&self, key: &str) -> Result<CacheWatch, ClusterError> {
        self.register(Selector::Key(key.to_owned()))
    }

    async fn watch_prefix(&self, prefix: &str) -> Result<CacheWatch, ClusterError> {
        self.register(Selector::Prefix(prefix.to_owned()))
    }

    async fn scan_prefix(&self, prefix: &str) -> Result<Vec<String>, ClusterError> {
        let state = self.lock();
        let now = Instant::now();
        Ok(state
            .entries
            .iter()
            .filter(|(key, record)| key.starts_with(prefix) && !record.is_expired(now))
            .map(|(key, _)| key.clone())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use cluster_sdk::cache::{ClusterCacheBackend, PutRequest, Ttl};

    use super::MemoryCache;

    #[tokio::test]
    async fn watch_activity_reclaims_dropped_subscribers_with_unrelated_keys() {
        let cache = MemoryCache::new();
        let live = cache.watch("retained").await.unwrap();

        for index in 0..16 {
            let abandoned = cache.watch(&format!("exact/{index}")).await.unwrap();
            assert_eq!(cache.lock().watchers.len(), 2);
            drop(abandoned);
        }

        let abandoned = cache.watch_prefix("obsolete/").await.unwrap();
        assert_eq!(cache.lock().watchers.len(), 2);
        drop(abandoned);
        cache
            .put(PutRequest {
                key: "unrelated",
                value: b"event",
                ttl: Ttl::Indefinite,
            })
            .await
            .unwrap();
        assert_eq!(cache.lock().watchers.len(), 1);
        drop(live);
    }
}
