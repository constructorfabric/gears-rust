//! The deadline-armed TTL sweeper (DESIGN.md §6.2).
//!
//! Read-path expiry is authoritative — an entry past its `expiresAt` reads as absent
//! whether or not it still exists (§6.2) — so the sweeper is only reclamation: it
//! deletes expired objects so they do not accumulate. It is a min-heap keyed by
//! deadline, fed by the shared watcher (every write carries an `expiresAt`) and by
//! the startup scan; when the earliest deadline passes it issues a **guarded**
//! delete, and a `404`/`409` (already gone, or revived) is dropped, never retried.
//!
//! The heap's ordering and its dedupe — a key re-armed with a new deadline
//! supersedes the old entry rather than deleting twice — are pure and carry the L1
//! coverage ([`SweepHeap`]); the delete I/O is Phase 6.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::ClusterError;
use k8s_openapi::jiff::{SignedDuration, Timestamp};
use kube::api::ListParams;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::scan::LIST_PAGE;
use super::{CacheRuntime, SweepCmd, is_expired};

/// The idle sleep when nothing is armed — a bounded wake so a lost command cannot
/// wedge the loop; real work is driven by commands and armed deadlines.
const IDLE_TICK: Duration = Duration::from_hours(1);

/// How long to defer a key whose reclaim read failed transiently, before retrying.
const SWEEP_RETRY_BACKOFF_SECS: i64 = 5;

/// How many transient read failures to tolerate for one key before giving up on it
/// (it stays reclaimable via read-path expiry, and a later watcher re-arm re-drives
/// it). Bounds an unreadable key — RBAC revoked mid-flight, a wedged object — from
/// re-arming forever and turning a storage leak into a perpetual request loop (§12).
const MAX_SWEEP_RETRIES: u32 = 3;

/// The outcome of one [`sweep_key`] attempt: reclaimed / nothing to do, or a
/// transient read failure the caller should re-arm for a bounded retry.
enum SweepOutcome {
    /// The key was reclaimed, already gone, or no longer expired — nothing owed.
    Done,
    /// A transient read error: the caller re-arms the key for a short retry.
    Retry,
}

/// One armed deadline: delete `key` at `at`, unless superseded first.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Armed {
    at: Timestamp,
    key: String,
}

// Order by deadline only (the heap is wrapped in `Reverse` for min-first); the key
// breaks ties so equal-deadline entries have a total order.
impl Ord for Armed {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.at
            .cmp(&other.at)
            .then_with(|| self.key.cmp(&other.key))
    }
}
impl PartialOrd for Armed {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A min-heap of expiry deadlines with per-key dedupe (§6.2).
///
/// Re-arming a key records its *latest* deadline; earlier heap entries for that key
/// are recognised as stale on pop (their deadline no longer matches the latest) and
/// skipped, so a key written N times is deleted once, at its final deadline.
#[derive(Default)]
pub struct SweepHeap {
    heap: BinaryHeap<Reverse<Armed>>,
    /// The current (latest) deadline armed for each key.
    latest: HashMap<String, Timestamp>,
}

impl SweepHeap {
    /// A fresh, empty heap.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Arms (or re-arms) `key` to be swept at `at`. A later call for the same key
    /// supersedes the earlier deadline.
    pub fn arm(&mut self, key: &str, at: Timestamp) {
        self.latest.insert(key.to_owned(), at);
        self.heap.push(Reverse(Armed {
            at,
            key: key.to_owned(),
        }));
        self.compact_if_bloated();
    }

    /// Cancels any armed deadline for `key` (it was deleted or overwritten to
    /// indefinite). The stale heap entry is dropped on pop.
    pub fn disarm(&mut self, key: &str) {
        self.latest.remove(key);
    }

    /// Rebuilds the heap from the live `latest` set once it has grown well past the
    /// live key count.
    ///
    /// Each [`arm`](Self::arm) pushes a `Reverse(Armed)` without removing the
    /// superseded entry, and [`drop_stale_front`](Self::drop_stale_front) only
    /// reclaims stale entries at the *front* — so a key re-armed while an
    /// earlier-deadline key stays live leaves stale entries buried in the middle. Left
    /// unchecked the heap grows toward O(writes-in-TTL-window); this compaction bounds
    /// it to O(distinct live keys) amortized without changing any observable ordering.
    fn compact_if_bloated(&mut self) {
        /// Don't churn tiny heaps; only compact once there is real bloat to reclaim.
        const MIN_HEAP: usize = 16;
        if self.heap.len() > MIN_HEAP && self.heap.len() > self.latest.len() * 2 {
            self.heap = self
                .latest
                .iter()
                .map(|(key, at)| {
                    Reverse(Armed {
                        at: *at,
                        key: key.clone(),
                    })
                })
                .collect();
        }
    }

    /// The earliest live deadline, for arming a timer. Skips stale heap entries at
    /// the front without consuming live ones.
    pub fn peek_deadline(&mut self) -> Option<Timestamp> {
        self.drop_stale_front();
        self.heap.peek().map(|Reverse(armed)| armed.at)
    }

    /// Pops the next key whose deadline is at or before `now`, or `None` when none
    /// is due. Stale (superseded) entries are discarded, never returned.
    pub fn pop_due(&mut self, now: Timestamp) -> Option<String> {
        loop {
            self.drop_stale_front();
            let Reverse(front) = self.heap.peek()?;
            if front.at > now {
                return None;
            }
            let Reverse(armed) = self.heap.pop()?;
            // Live iff its deadline is still the latest armed for the key.
            if self.latest.get(&armed.key) == Some(&armed.at) {
                self.latest.remove(&armed.key);
                return Some(armed.key);
            }
            // Otherwise superseded/disarmed — discard and continue.
        }
    }

    /// Discards stale entries at the front of the heap so `peek` reflects a live
    /// deadline.
    fn drop_stale_front(&mut self) {
        while let Some(Reverse(front)) = self.heap.peek() {
            if self.latest.get(&front.key) == Some(&front.at) {
                break;
            }
            self.heap.pop();
        }
    }

    /// The number of live armed keys. Retained for the heap's own unit tests.
    #[allow(dead_code)]
    #[must_use]
    pub fn len(&self) -> usize {
        self.latest.len()
    }

    /// Whether nothing is armed. Retained for the heap's own unit tests.
    #[allow(dead_code)]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.latest.is_empty()
    }
}

/// The std [`Duration`] until `deadline`, clamped to zero if it is already past.
fn wait_until(deadline: Timestamp) -> Duration {
    let signed = deadline.duration_since(Timestamp::now());
    Duration::from_secs(signed.as_secs().max(0).unsigned_abs())
        + Duration::from_nanos(u64::from(signed.subsec_nanos().max(0).unsigned_abs()))
}

/// The sweeper background task (§6.2): maintain the deadline heap from the watcher's
/// arm/disarm commands and reclaim each entry when its deadline passes.
pub(super) async fn run_sweeper(
    runtime: Arc<CacheRuntime>,
    mut commands: mpsc::UnboundedReceiver<SweepCmd>,
    shutdown: CancellationToken,
) {
    let mut heap = SweepHeap::new();
    // Per-key transient-read-failure counts, so a re-armed retry is bounded.
    let mut retries: HashMap<String, u32> = HashMap::new();
    loop {
        let wait = heap.peek_deadline().map_or(IDLE_TICK, wait_until);
        tokio::select! {
            () = shutdown.cancelled() => return,
            command = commands.recv() => match command {
                Some(SweepCmd::Arm { key, at }) => heap.arm(&key, at),
                Some(SweepCmd::Disarm { key }) => heap.disarm(&key),
                // The backend was dropped; nothing more will be armed.
                None => return,
            },
            () = tokio::time::sleep(wait) => {
                let now = Timestamp::now();
                while let Some(key) = heap.pop_due(now) {
                    // A batch of keys expiring together must not hold `K8sCache::stop`
                    // open for the whole batch of read+delete round trips: bail on
                    // cancel before each one. Dropping the un-swept keys is harmless —
                    // read-path expiry already hides them and a later instance re-arms.
                    if shutdown.is_cancelled() {
                        return;
                    }
                    match sweep_key(&runtime, &key).await {
                        SweepOutcome::Done => {
                            retries.remove(&key);
                        }
                        SweepOutcome::Retry => rearm_for_retry(&mut heap, &mut retries, &key),
                    }
                }
            }
        }
    }
}

/// Re-arms `key` for a bounded retry after a transient reclaim-read failure, or gives
/// up with a single ERROR once [`MAX_SWEEP_RETRIES`] is exceeded (§12). Shared by the
/// deadline sweeper and the fixed-interval fallback.
fn rearm_for_retry(heap: &mut SweepHeap, retries: &mut HashMap<String, u32>, key: &str) {
    let attempts = retries.entry(key.to_owned()).or_insert(0);
    *attempts += 1;
    if *attempts > MAX_SWEEP_RETRIES {
        tracing::error!(
            key = %key, attempts = *attempts,
            "cluster.provider.cache_sweep_giving_up: could not read an expired entry to reclaim \
             it after repeated retries; it stays reclaimable via read-path expiry and a later \
             watcher re-arm re-drives it"
        );
        retries.remove(key);
    } else if let Ok(at) =
        Timestamp::now().checked_add(SignedDuration::from_secs(SWEEP_RETRY_BACKOFF_SECS))
    {
        heap.arm(key, at);
    }
}

/// The fixed-interval TTL sweeper used when `cache_watch: false` (§6.2).
///
/// With the shared watcher off there is nothing to arm the deadline heap, so this is
/// the documented fallback that keeps expired `ClusterCacheEntry` objects from
/// accumulating in etcd without limit. It **sleeps first, then works** (a `select!`
/// over `sleep(interval)`, mirroring the lock reaper — *not* `tokio::time::interval`,
/// whose first tick fires immediately): every `interval` it lists the cache label
/// selector and guarded-deletes each entry past its `expiresAt`. `interval` is
/// rejected at zero in `build_and_start`, so this can never become an unthrottled
/// LIST loop.
pub(super) async fn run_interval_sweeper(
    runtime: Arc<CacheRuntime>,
    selector: String,
    interval: Duration,
    shutdown: CancellationToken,
) {
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(interval) => {
                if let Err(err) = interval_sweep_pass(&runtime, &selector, &shutdown).await {
                    tracing::warn!(
                        error = %err,
                        "cluster.provider.cache_interval_sweep_failed: a fixed-interval sweep \
                         pass could not complete; retrying next interval"
                    );
                }
            }
        }
    }
}

/// One fixed-interval sweep pass: paginate the cache selector and guarded-delete every
/// entry past its `expiresAt`. Checks `shutdown` before each page and each delete, so a
/// pass spanning many pages cannot hold `K8sCache::stop` open for its whole duration.
async fn interval_sweep_pass(
    runtime: &CacheRuntime,
    selector: &str,
    shutdown: &CancellationToken,
) -> Result<(), ClusterError> {
    let api = runtime.api();
    let mut continue_token: Option<String> = None;
    loop {
        if shutdown.is_cancelled() {
            return Ok(());
        }
        let mut params = ListParams::default().labels(selector).limit(LIST_PAGE);
        if let Some(token) = &continue_token {
            params = params.continue_token(token);
        }
        let list = runtime
            .timed("interval sweep list", async {
                api.list(&params)
                    .await
                    .map_err(|e| crate::k8s_error::map_kube_error(&e))
            })
            .await?;
        let now = Timestamp::now();
        for entry in &list.items {
            if shutdown.is_cancelled() {
                return Ok(());
            }
            if !is_expired(entry.spec.expires_at.as_deref(), now) {
                continue;
            }
            let Some(name) = entry.metadata.name.as_deref() else {
                continue;
            };
            // Guarded on `resourceVersion` + `uid`, so it can never remove an entry
            // revived under the same name; a 404/409 (already gone, or revived) is the
            // expected race outcome, `Ok(false)`, not an error.
            let _swept = runtime
                .timed(
                    "interval sweep entry",
                    crate::guarded::delete(&api, name, &entry.metadata),
                )
                .await;
        }
        continue_token = list.metadata.continue_.filter(|t| !t.is_empty());
        if continue_token.is_none() {
            break;
        }
    }
    Ok(())
}

/// Reclaims one expired key with a guarded delete (§6.2), re-reading first so the
/// delete carries `resourceVersion` + `uid` and cannot land on a revived object.
///
/// The three read outcomes are genuinely distinct and must not be collapsed:
/// `Ok(Some)` past its deadline is reclaimed; `Ok(Some)` re-armed to the future or
/// `Ok(None)` (already gone) is [`SweepOutcome::Done`]; a transient `Err` returns
/// [`SweepOutcome::Retry`] with a WARN, because `pop_due` has already removed this key
/// from the heap — so a swallowed read error would leak the object until that key
/// happens to be written again, which for a write-once-with-TTL key never happens.
async fn sweep_key(runtime: &CacheRuntime, key: &str) -> SweepOutcome {
    match runtime.read_raw(key).await {
        Ok(Some(entry)) => {
            if !is_expired(entry.spec.expires_at.as_deref(), Timestamp::now()) {
                return SweepOutcome::Done; // overwritten to a later deadline
            }
            let api = runtime.api();
            let name = runtime.object_name(key);
            // A 404/409 (already gone, or revived) comes back as Ok(false); a real
            // delete fault is dropped — the sweeper is reclamation, not correctness
            // (§6.2). Bounded by `request_timeout` so a stalled delete can't hold
            // `K8sCache::stop` open forever.
            let _swept = runtime
                .timed(
                    "sweep cache entry",
                    crate::guarded::delete(&api, &name, &entry.metadata),
                )
                .await;
            SweepOutcome::Done
        }
        Ok(None) => SweepOutcome::Done, // already gone: nothing to reclaim
        Err(err) => {
            tracing::warn!(
                error = %err, key = %key,
                "cluster.provider.cache_sweep_read_failed: could not read an expired entry to \
                 reclaim it; re-arming for a short retry"
            );
            SweepOutcome::Retry
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_SWEEP_RETRIES, SweepHeap, rearm_for_retry};
    use k8s_openapi::jiff::Timestamp;
    use std::collections::HashMap;

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_second(secs).unwrap()
    }

    #[test]
    fn pops_in_deadline_order() {
        let mut heap = SweepHeap::new();
        heap.arm("late", ts(300));
        heap.arm("early", ts(100));
        heap.arm("mid", ts(200));
        assert_eq!(heap.peek_deadline(), Some(ts(100)));
        assert_eq!(heap.pop_due(ts(1_000)), Some("early".to_owned()));
        assert_eq!(heap.pop_due(ts(1_000)), Some("mid".to_owned()));
        assert_eq!(heap.pop_due(ts(1_000)), Some("late".to_owned()));
        assert_eq!(heap.pop_due(ts(1_000)), None);
    }

    #[test]
    fn nothing_is_due_before_its_deadline() {
        let mut heap = SweepHeap::new();
        heap.arm("k", ts(500));
        assert_eq!(heap.pop_due(ts(499)), None, "not yet due");
        assert_eq!(
            heap.pop_due(ts(500)),
            Some("k".to_owned()),
            "due at the deadline"
        );
    }

    #[test]
    fn rearming_supersedes_the_earlier_deadline() {
        let mut heap = SweepHeap::new();
        heap.arm("k", ts(100));
        heap.arm("k", ts(400)); // pushed the deadline out
        // The stale 100 entry must not fire the delete.
        assert_eq!(
            heap.pop_due(ts(200)),
            None,
            "the superseded deadline is dead"
        );
        assert_eq!(heap.peek_deadline(), Some(ts(400)));
        assert_eq!(heap.pop_due(ts(400)), Some("k".to_owned()));
        // And the key is swept exactly once.
        assert_eq!(heap.pop_due(ts(1_000)), None);
        assert!(heap.is_empty());
    }

    #[test]
    fn disarm_cancels_a_pending_sweep() {
        let mut heap = SweepHeap::new();
        heap.arm("k", ts(100));
        heap.disarm("k"); // e.g. the key was deleted or made indefinite
        assert_eq!(heap.pop_due(ts(1_000)), None);
        assert!(heap.is_empty());
    }

    #[test]
    fn len_tracks_distinct_live_keys() {
        let mut heap = SweepHeap::new();
        heap.arm("a", ts(100));
        heap.arm("b", ts(200));
        heap.arm("a", ts(300)); // re-arm same key, not a new one
        assert_eq!(heap.len(), 2);
    }

    #[test]
    fn repeated_rearming_compacts_the_heap_and_preserves_ordering() {
        let mut heap = SweepHeap::new();
        // A live earlier-deadline key that keeps the re-armed key's stale entries
        // buried (so `drop_stale_front` cannot reclaim them), then re-arm one key many
        // times to later, ever-growing deadlines.
        heap.arm("early", ts(1));
        for i in 0..500 {
            heap.arm("hot", ts(1000 + i));
        }
        // Two live keys, so the backing heap must have compacted rather than retained
        // ~500 buried stale entries.
        assert_eq!(heap.len(), 2, "only two distinct live keys");
        assert!(
            heap.heap.len() <= 8,
            "the heap compacts toward the live-key count, got {}",
            heap.heap.len()
        );
        // Ordering is unchanged: the earlier-deadline key still pops first, then the
        // hot key at its *latest* armed deadline, and nothing stale resurfaces.
        assert_eq!(heap.pop_due(ts(10_000)), Some("early".to_owned()));
        assert_eq!(heap.pop_due(ts(10_000)), Some("hot".to_owned()));
        assert_eq!(heap.pop_due(ts(10_000)), None);
    }

    #[test]
    fn rearm_for_retry_re_arms_up_to_the_bound_then_gives_up() {
        let mut heap = SweepHeap::new();
        let mut retries: HashMap<String, u32> = HashMap::new();
        let key = "k";

        // Attempts 1..=MAX re-arm the key and count up.
        for expected in 1..=MAX_SWEEP_RETRIES {
            rearm_for_retry(&mut heap, &mut retries, key);
            assert_eq!(
                retries.get(key),
                Some(&expected),
                "attempt {expected} is counted"
            );
            assert_eq!(heap.len(), 1, "the key stays armed while under the bound");
        }
        let arms_before = heap.heap.len();

        // One more exceeds the bound: the retry state is cleared and NO new arm is
        // pushed (the object stays reclaimable via read-path expiry / a later watcher
        // re-arm, but this loop must not re-arm forever — §12).
        rearm_for_retry(&mut heap, &mut retries, key);
        assert_eq!(retries.get(key), None, "giving up clears the retry state");
        assert_eq!(
            heap.heap.len(),
            arms_before,
            "giving up must not push another arm"
        );
    }
}
