use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::FutureExt;
use parking_lot::Mutex;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::{Evict, LOAD_TIMEOUT, LoadMany, LoadOne, Section, SectionKey, SectionValue};
use crate::domain::ports::metric_labels::CacheMap;
use crate::domain::ports::{NoopMetrics, OagwMetricsPort};
use crate::domain::repo::RepositoryError;
use crate::infra::proxy_cache::testing::CacheMetrics;

const TTL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    alias: &'static str,
    n: u8,
}

impl SectionKey for Key {
    fn alias(&self) -> Option<&str> {
        Some(self.alias)
    }
}

fn key(alias: &'static str, n: u8) -> Key {
    Key { alias, n }
}

#[derive(Debug, Clone, PartialEq)]
struct Row(Uuid);

impl SectionValue for Row {
    fn id(&self) -> Uuid {
        self.0
    }
}

fn rows(n: usize) -> Vec<Row> {
    (0..n).map(|_| Row(Uuid::new_v4())).collect()
}

type Rows = Result<Arc<[Row]>, RepositoryError>;
type TestSection = Arc<Section<Key, Row>>;

fn section(max_entries: usize) -> TestSection {
    section_recording(max_entries, Arc::new(NoopMetrics))
}

/// A section recording its metrics to `metrics` under the `map` label `m`.
fn section_recording(max_entries: usize, metrics: Arc<dyn OagwMetricsPort>) -> TestSection {
    Arc::new(Section::new(CacheMap::Alias, TTL, max_entries, metrics))
}

/// A scripted database. A load reads the rows when it starts; a gated
/// load answers only once released, one permit per load.
#[derive(Clone)]
struct Script(Arc<ScriptState>);

struct ScriptState {
    rows: Mutex<HashMap<Key, Vec<Row>>>,
    calls: Mutex<Vec<Vec<Key>>>,
    fail: AtomicBool,
    panic: AtomicBool,
    /// Panic with a formatted message (a `String` payload).
    panic_formatted: AtomicBool,
    gate: Semaphore,
}

impl Script {
    fn open() -> Self {
        Self::with_permits(Semaphore::MAX_PERMITS)
    }

    fn gated() -> Self {
        Self::with_permits(0)
    }

    fn with_permits(permits: usize) -> Self {
        Self(Arc::new(ScriptState {
            rows: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
            fail: AtomicBool::new(false),
            panic: AtomicBool::new(false),
            panic_formatted: AtomicBool::new(false),
            gate: Semaphore::new(permits),
        }))
    }

    fn set(&self, key: &Key, rows: &[Row]) {
        self.0.rows.lock().insert(key.clone(), rows.to_vec());
    }

    fn fail(&self, fail: bool) {
        self.0.fail.store(fail, Ordering::SeqCst);
    }

    fn panic(&self, panic: bool) {
        self.0.panic.store(panic, Ordering::SeqCst);
    }

    fn panic_formatted(&self, panic: bool) {
        self.0.panic_formatted.store(panic, Ordering::SeqCst);
    }

    fn release(&self, loads: usize) {
        self.0.gate.add_permits(loads);
    }

    fn calls(&self) -> Vec<Vec<Key>> {
        self.0.calls.lock().clone()
    }

    /// Yield until `n` loads have started.
    async fn started(&self, n: usize) {
        for _ in 0..1000 {
            if self.0.calls.lock().len() >= n {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("{n} loads never started: {:?}", self.calls());
    }

    fn load_many(&self) -> impl FnOnce(Vec<Key>) -> LoadMany<Key, Row> + use<> {
        let script = self.clone();
        move |keys| {
            let state = &script.0;
            state.calls.lock().push(keys.clone());
            let n = keys.len();
            let answer = if state.fail.load(Ordering::SeqCst) {
                Err(RepositoryError::Internal("database down".into()))
            } else {
                let rows = state.rows.lock();
                Ok(keys
                    .into_iter()
                    .map(|key| {
                        let found = rows.get(&key).cloned().unwrap_or_default();
                        (key, found)
                    })
                    .collect())
            };
            async move {
                script.0.gate.acquire().await.unwrap().forget();
                assert!(
                    !script.0.panic.load(Ordering::SeqCst),
                    "scripted load panic"
                );
                assert!(
                    !script.0.panic_formatted.load(Ordering::SeqCst),
                    "scripted formatted panic over {n} keys"
                );
                answer
            }
            .boxed()
        }
    }

    fn load(&self) -> impl FnOnce(Key) -> LoadOne<Row> + use<> {
        let many = self.load_many();
        move |key| {
            let rows = many(vec![key]);
            async move { rows.await.map(|mut rows| rows.remove(0).1) }.boxed()
        }
    }
}

async fn get(section: &TestSection, script: &Script, key: &Key) -> Rows {
    section.get_or_load(key.clone(), script.load()).await
}

fn spawn_get(section: &TestSection, script: &Script, key: &Key) -> JoinHandle<Rows> {
    let (section, script, key) = (Arc::clone(section), script.clone(), key.clone());
    tokio::spawn(async move { get(&section, &script, &key).await })
}

async fn settle() {
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
}

fn assert_rows(value: &Rows, expected: &[Row]) {
    assert_eq!(&**value.as_ref().expect("lookup succeeds"), expected);
}

#[tokio::test]
async fn a_miss_loads_and_a_fresh_entry_is_served_without_a_load() {
    let (section, script) = (section(10), Script::open());
    let (a, rows_a) = (key("a", 1), rows(2));
    script.set(&a, &rows_a);

    assert_rows(&get(&section, &script, &a).await, &rows_a);
    script.set(&a, &rows(1));
    assert_rows(&get(&section, &script, &a).await, &rows_a);

    assert_eq!(script.calls(), vec![vec![a]]);
    section.assert_consistent();
}

/// ADR-0020: CACHE-RACE-002.
#[tokio::test]
async fn concurrent_misses_on_one_key_run_one_load() {
    let (section, script) = (section(10), Script::gated());
    let (a, rows_a) = (key("a", 1), rows(1));
    script.set(&a, &rows_a);

    let first = spawn_get(&section, &script, &a);
    script.started(1).await;
    let joined: Vec<_> = (0..3).map(|_| spawn_get(&section, &script, &a)).collect();
    settle().await;
    assert_eq!(script.calls().len(), 1, "the later misses join the load");
    assert_eq!(section.loads(), 1);

    script.release(1);
    assert_rows(&first.await.unwrap(), &rows_a);
    for request in joined {
        assert_rows(&request.await.unwrap(), &rows_a);
    }
    assert_rows(&get(&section, &script, &a).await, &rows_a);
    assert_eq!(script.calls().len(), 1);
    assert_eq!(section.loads(), 0);
}

#[tokio::test]
async fn a_batched_lookup_serves_hits_joins_loads_and_loads_the_rest_once() {
    let (section, script) = (section(10), Script::gated());
    let keys = [key("a", 1), key("b", 1), key("c", 1), key("d", 1)];
    let values: Vec<_> = keys.iter().map(|_| rows(2)).collect();
    for (key, rows) in keys.iter().zip(&values) {
        script.set(key, rows);
    }
    let [a, b, c, d] = keys;

    script.release(1);
    assert_rows(&get(&section, &script, &a).await, &values[0]);
    let in_flight = spawn_get(&section, &script, &b);
    script.started(2).await;

    let lookup = {
        let (section, script) = (Arc::clone(&section), script.clone());
        let wanted = vec![c.clone(), a.clone(), b.clone(), d.clone(), c.clone()];
        tokio::spawn(async move { section.get_or_load_many(&wanted, script.load_many()).await })
    };
    script.started(3).await;
    assert_eq!(
        script.calls()[2],
        vec![c.clone(), d.clone()],
        "one load for the keys neither stored nor loading"
    );

    script.release(2);
    let got = lookup.await.unwrap().unwrap();
    let expected = [&values[2], &values[0], &values[1], &values[3], &values[2]];
    assert_eq!(got.len(), expected.len());
    for (got, expected) in got.iter().zip(expected) {
        assert_eq!(&**got, &expected[..]);
    }
    assert_rows(&in_flight.await.unwrap(), &values[1]);
    assert_eq!(script.calls().len(), 3);
    for key in [&a, &b, &c, &d] {
        assert!(section.contains(key), "{key:?} stored");
    }
    section.assert_consistent();
}

#[tokio::test]
async fn a_lookup_of_no_keys_loads_nothing() {
    let (section, script) = (section(10), Script::open());
    let got = section.get_or_load_many(&[], script.load_many()).await;
    assert!(got.unwrap().is_empty());
    assert!(script.calls().is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_entry_expires_after_the_ttl() {
    let (section, script) = (section(10), Script::open());
    let (a, old, new) = (key("a", 1), rows(1), rows(1));
    script.set(&a, &old);
    assert_rows(&get(&section, &script, &a).await, &old);
    script.set(&a, &new);

    tokio::time::advance(TTL - Duration::from_millis(1)).await;
    assert_rows(&get(&section, &script, &a).await, &old);
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_rows(&get(&section, &script, &a).await, &new);

    assert_eq!(script.calls().len(), 2);
    section.assert_consistent();
}

#[tokio::test(start_paused = true)]
async fn a_store_drops_the_expired_entries_with_their_index_links() {
    let (section, script) = (section(10), Script::open());
    let (a, b, c) = (key("a", 1), key("a", 2), key("c", 1));
    script.set(&a, &rows(2));
    script.set(&b, &rows(1));
    get(&section, &script, &a).await.unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    get(&section, &script, &b).await.unwrap();

    tokio::time::advance(TTL - Duration::from_millis(500)).await;
    assert_eq!(section.len(), 2, "expired entries stay until a store");
    get(&section, &script, &c).await.unwrap();

    assert!(!section.contains(&a), "expired: dropped");
    assert!(section.contains(&b), "still fresh: kept");
    assert!(section.contains(&c));
    section.assert_consistent();
}

/// ADR-0020: CACHE-HIT-004.
#[tokio::test(start_paused = true)]
async fn a_full_section_evicts_the_least_recently_used_entry() {
    let (section, script) = (section(2), Script::open());
    let keys = [key("a", 1), key("b", 1), key("c", 1)];
    let values: Vec<_> = keys.iter().map(|_| rows(1)).collect();
    for (key, rows) in keys.iter().zip(&values) {
        script.set(key, rows);
    }
    let [a, b, c] = keys;

    get(&section, &script, &a).await.unwrap();
    get(&section, &script, &b).await.unwrap();
    assert_rows(&get(&section, &script, &a).await, &values[0]);
    get(&section, &script, &c).await.unwrap();

    assert_eq!(section.len(), 2);
    assert!(!section.contains(&b), "used least recently: goes");
    assert!(section.contains(&a), "stored first, used since: kept");
    assert!(section.contains(&c));
    assert_rows(&get(&section, &script, &a).await, &values[0]);
    assert_eq!(script.calls().len(), 3, "a never reloaded");
    section.assert_consistent();
}

/// A batched lookup uses each key it serves from the cache.
#[tokio::test(start_paused = true)]
async fn a_batched_lookup_uses_every_key_it_hits() {
    let (section, script) = (section(3), Script::open());
    let [a, b, c, d] = [key("a", 1), key("b", 1), key("c", 1), key("d", 1)];
    for key in [&a, &b, &c] {
        get(&section, &script, key).await.unwrap();
    }

    section
        .get_or_load_many(&[a.clone(), b.clone()], script.load_many())
        .await
        .unwrap();
    get(&section, &script, &d).await.unwrap();

    assert!(!section.contains(&c), "the only key not used since: goes");
    assert!(section.contains(&a) && section.contains(&b) && section.contains(&d));
    section.assert_consistent();
}

/// ADR-0020: CACHE-HIT-004.
#[tokio::test(start_paused = true)]
async fn an_entry_stored_again_is_the_most_recently_used() {
    let (section, script) = (section(2), Script::open());
    let (a, b, c) = (key("a", 1), key("b", 1), key("c", 1));
    get(&section, &script, &a).await.unwrap();
    get(&section, &script, &b).await.unwrap();
    section.evict(|_| Evict::Keys(vec![a.clone()]));
    get(&section, &script, &a).await.unwrap();

    get(&section, &script, &c).await.unwrap();

    assert!(!section.contains(&b), "b is the least recently used");
    assert!(section.contains(&a), "a was stored again after b");
    assert!(section.contains(&c));
    section.assert_consistent();
}

/// An expired entry used since its store is not at the least recently used
/// end, so no store's expiry pass reaches it: its next lookup removes it,
/// even when the load that follows fails.
#[tokio::test(start_paused = true)]
async fn a_lookup_removes_the_expired_entry_it_finds() {
    let (section, metrics) = recorded(10);
    let script = Script::open();
    let (a, b) = (key("a", 1), key("b", 1));
    get(&section, &script, &a).await.unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    get(&section, &script, &b).await.unwrap();
    get(&section, &script, &a).await.unwrap();
    tokio::time::advance(TTL - Duration::from_millis(500)).await;

    script.fail(true);
    get(&section, &script, &a).await.unwrap_err();

    assert!(!section.contains(&a), "expired: removed by its lookup");
    assert!(section.contains(&b), "still fresh: kept");
    assert_eq!(metrics.count("evictions alias ttl"), 1);
    assert_eq!(metrics.entries("alias"), Some(1));
    section.assert_consistent();
}

#[tokio::test(start_paused = true)]
async fn the_indexes_follow_store_replace_evict_and_expire() {
    let (section, script) = (section(10), Script::open());
    let (a, b) = (key("x", 1), key("x", 2));
    let (shared, old, new) = (rows(1), rows(2), rows(1));
    script.set(&a, &[shared.clone(), old.clone()].concat());
    script.set(&b, &shared);
    get(&section, &script, &a).await.unwrap();
    get(&section, &script, &b).await.unwrap();
    section.assert_consistent();
    let holding = |id| -> Vec<Key> {
        let state = section.inner.state.lock();
        let mut keys: Vec<_> = state.index.holding(id).cloned().collect();
        keys.sort_by_key(|key| key.n);
        keys
    };
    assert_eq!(holding(shared[0].0), vec![a.clone(), b.clone()]);
    assert_eq!(holding(old[0].0), vec![a.clone()]);

    // Replace a's value once it expired: its old rows no longer lead to it.
    tokio::time::advance(TTL).await;
    script.set(&a, &new);
    get(&section, &script, &a).await.unwrap();
    section.assert_consistent();
    assert!(holding(old[0].0).is_empty());
    assert_eq!(holding(new[0].0), vec![a.clone()]);
    assert!(!section.contains(&b), "b expired with a's old value");
    assert!(holding(shared[0].0).is_empty());

    section.evict(|_| Evict::Keys(vec![a.clone()]));
    section.assert_consistent();
    assert!(holding(new[0].0).is_empty());
    let state = section.inner.state.lock();
    assert_eq!(state.index.with_alias("x").count(), 0);
}

/// ADR-0020: CACHE-FAIL-001.
#[tokio::test]
async fn a_failed_load_reaches_every_waiter_and_is_not_stored() {
    let (section, script) = (section(10), Script::gated());
    let (a, rows_a) = (key("a", 1), rows(1));
    script.set(&a, &rows_a);
    script.fail(true);

    let first = spawn_get(&section, &script, &a);
    script.started(1).await;
    let joined = spawn_get(&section, &script, &a);
    settle().await;
    script.release(1);
    for request in [first, joined] {
        assert!(matches!(
            request.await.unwrap(),
            Err(RepositoryError::Internal(detail)) if detail == "database down"
        ));
    }
    assert_eq!(script.calls().len(), 1, "both got the one load's error");
    assert_eq!((section.len(), section.loads()), (0, 0));

    script.fail(false);
    script.release(1);
    assert_rows(&get(&section, &script, &a).await, &rows_a);
    assert_eq!(script.calls().len(), 2, "the next miss loads again");
}

#[tokio::test]
async fn a_load_whose_every_requester_was_cancelled_still_finishes_and_is_stored() {
    let (section, script) = (section(10), Script::gated());
    let (a, rows_a) = (key("a", 1), rows(1));
    script.set(&a, &rows_a);

    let requests = [
        spawn_get(&section, &script, &a),
        spawn_get(&section, &script, &a),
    ];
    script.started(1).await;
    settle().await;
    for request in requests {
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
    }

    script.release(1);
    for _ in 0..1000 {
        if section.contains(&a) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        section.contains(&a),
        "the load ran on without its requesters"
    );
    assert_eq!(section.loads(), 0);
    assert_rows(&get(&section, &script, &a).await, &rows_a);
    assert_eq!(script.calls().len(), 1);
}

#[tokio::test]
async fn a_panicking_load_answers_an_error_and_the_next_miss_loads_again() {
    let (section, script) = (section(10), Script::gated());
    let (a, rows_a) = (key("a", 1), rows(1));
    script.set(&a, &rows_a);
    script.panic(true);

    let request = spawn_get(&section, &script, &a);
    script.started(1).await;
    script.release(1);
    assert!(matches!(
        request.await.unwrap(),
        Err(RepositoryError::Internal(detail)) if detail == "proxy cache load panicked (keys: 1)"
    ));
    assert_eq!((section.len(), section.loads()), (0, 0));

    script.panic(false);
    script.release(1);
    assert_rows(&get(&section, &script, &a).await, &rows_a);
    assert_eq!(script.calls().len(), 2);
}

#[tokio::test]
async fn a_load_whose_first_requester_was_cancelled_is_still_stored() {
    let (section, script) = (section(10), Script::gated());
    let (a, rows_a) = (key("a", 1), rows(1));
    script.set(&a, &rows_a);

    let first = spawn_get(&section, &script, &a);
    script.started(1).await;
    let joined = spawn_get(&section, &script, &a);
    settle().await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());

    script.release(1);
    assert_rows(&joined.await.unwrap(), &rows_a);
    assert!(section.contains(&a));
    assert_rows(&get(&section, &script, &a).await, &rows_a);
    assert_eq!(script.calls().len(), 1);
}

/// ADR-0020: CACHE-EVICT-006, CACHE-RACE-001.
#[tokio::test]
async fn a_change_detaches_the_in_flight_loads() {
    let (section, script) = (section(10), Script::gated());
    let (a, old, new) = (key("a", 1), rows(1), rows(1));
    script.set(&a, &old);

    let first = spawn_get(&section, &script, &a);
    script.started(1).await;
    let joined = spawn_get(&section, &script, &a);
    settle().await;

    // The write commits, then its change reaches the section.
    script.set(&a, &new);
    section.evict(|_| Evict::Keys(vec![]));
    assert_eq!(section.loads(), 0);
    let later = spawn_get(&section, &script, &a);
    script.started(2).await;

    script.release(1);
    assert_rows(&first.await.unwrap(), &old);
    assert_rows(&joined.await.unwrap(), &old);
    assert!(!section.contains(&a), "the detached load is not stored");

    script.release(1);
    assert_rows(&later.await.unwrap(), &new);
    assert_rows(&get(&section, &script, &a).await, &new);
    assert_eq!(script.calls().len(), 2);
}

#[tokio::test]
async fn evict_removes_only_the_picked_entries() {
    let (section, script) = (section(10), Script::open());
    let (a, b, absent) = (key("a", 1), key("b", 1), key("z", 1));
    script.set(&a, &rows(1));
    script.set(&b, &rows(1));
    get(&section, &script, &a).await.unwrap();
    get(&section, &script, &b).await.unwrap();

    section.evict(|_| Evict::Keys(vec![a.clone(), absent]));

    assert!(!section.contains(&a));
    assert!(section.contains(&b));
    section.assert_consistent();
}

#[tokio::test]
async fn clear_evicts_everything_and_detaches_the_loads() {
    let (section, script) = (section(10), Script::gated());
    let (a, b) = (key("a", 1), key("b", 1));
    script.set(&a, &rows(1));
    script.set(&b, &rows(1));
    script.release(1);
    get(&section, &script, &a).await.unwrap();
    let in_flight = spawn_get(&section, &script, &b);
    script.started(2).await;

    section.clear();
    assert_eq!((section.len(), section.loads()), (0, 0));
    section.assert_consistent();

    script.release(1);
    in_flight.await.unwrap().unwrap();
    assert!(!section.contains(&b), "the detached load is not stored");
}

// ── Metrics ─────────────────────────────────────────────────────────────

/// A section of at most `max_entries` and the metrics it records.
fn recorded(max_entries: usize) -> (TestSection, Arc<CacheMetrics>) {
    let metrics = Arc::new(CacheMetrics::default());
    (section_recording(max_entries, metrics.clone()), metrics)
}

/// Each key looked up counts once: a fresh entry as a hit (whether the
/// whole lookup hits or not), a key a load is already running for as
/// joined, and a key the lookup loads as a miss.
#[tokio::test]
async fn lookups_count_each_key_as_a_hit_a_miss_or_joined() {
    let (section, metrics) = recorded(10);
    let script = Script::gated();
    let [a, b, c, d] = [key("a", 1), key("b", 1), key("c", 1), key("d", 1)];

    script.release(1);
    get(&section, &script, &a).await.unwrap();
    get(&section, &script, &a).await.unwrap();
    assert_eq!(
        (
            metrics.count("lookups alias miss"),
            metrics.count("lookups alias hit")
        ),
        (1, 1)
    );

    let in_flight = spawn_get(&section, &script, &b);
    script.started(2).await;
    let lookup = {
        let (section, script) = (Arc::clone(&section), script.clone());
        let wanted = vec![a.clone(), b.clone(), c.clone(), d.clone()];
        tokio::spawn(async move { section.get_or_load_many(&wanted, script.load_many()).await })
    };
    script.started(3).await;
    script.release(2);
    lookup.await.unwrap().unwrap();
    in_flight.await.unwrap().unwrap();

    assert_eq!(metrics.count("lookups alias hit"), 2, "a, twice");
    assert_eq!(metrics.count("lookups alias miss"), 4, "a, b, c, d");
    assert_eq!(metrics.count("lookups alias joined"), 1, "b");
}

/// A store into a full section evicts the least recently used entry for
/// capacity, and the entries gauge stays at the limit.
#[tokio::test(start_paused = true)]
async fn a_full_section_counts_capacity_evictions() {
    let (section, metrics) = recorded(2);
    let script = Script::open();
    for n in 1..=3 {
        get(&section, &script, &key("a", n)).await.unwrap();
        assert_eq!(metrics.entries("alias"), Some(u64::from(n.min(2))));
        tokio::time::advance(Duration::from_secs(1)).await;
    }
    assert_eq!(metrics.count("evictions alias capacity"), 1);
    assert_eq!(metrics.count("evictions alias ttl"), 0);
}

/// A store drops the expired entries for the TTL, and the entries gauge
/// follows.
#[tokio::test(start_paused = true)]
async fn a_store_counts_the_expired_entries_it_drops() {
    let (section, metrics) = recorded(10);
    let script = Script::open();
    get(&section, &script, &key("a", 1)).await.unwrap();
    get(&section, &script, &key("a", 2)).await.unwrap();
    assert_eq!(metrics.entries("alias"), Some(2));

    tokio::time::advance(TTL).await;
    get(&section, &script, &key("a", 3)).await.unwrap();
    assert_eq!(metrics.count("evictions alias ttl"), 2);
    assert_eq!(metrics.entries("alias"), Some(1));
    assert_eq!(metrics.count("evictions alias capacity"), 0);
}

/// A change counts the stored entries it evicts (not the keys it names),
/// emptying counts every entry, and the entries gauge follows.
#[tokio::test]
async fn a_change_counts_the_entries_it_evicts() {
    let (section, metrics) = recorded(10);
    let script = Script::open();
    let [a, b, c] = [key("a", 1), key("a", 2), key("a", 3)];
    for key in [&a, &b, &c] {
        get(&section, &script, key).await.unwrap();
    }

    section.evict(|_| Evict::Keys(vec![a.clone(), key("absent", 1)]));
    assert_eq!(metrics.count("evictions alias change"), 1);
    assert_eq!(metrics.entries("alias"), Some(2));

    section.evict(|_| Evict::Nothing);
    section.evict(|_| Evict::Everything);
    assert_eq!(metrics.count("evictions alias change"), 3);
    assert_eq!(metrics.entries("alias"), Some(0));
}

/// A load a change detached counts its keys as discarded when it answers
/// rows; a failed one has nothing to discard.
#[tokio::test]
async fn a_detached_load_counts_its_rows_as_discarded() {
    let (section, metrics) = recorded(10);
    let script = Script::gated();
    let a = key("a", 1);

    let request = spawn_get(&section, &script, &a);
    script.started(1).await;
    section.evict(|_| Evict::Keys(vec![]));
    script.release(1);
    request.await.unwrap().unwrap();
    assert_eq!(metrics.count("discarded alias"), 1);
    assert!(!section.contains(&a));

    script.fail(true);
    let request = spawn_get(&section, &script, &a);
    script.started(2).await;
    section.evict(|_| Evict::Keys(vec![]));
    script.release(1);
    request.await.unwrap().unwrap_err();
    assert_eq!(metrics.count("discarded alias"), 1, "no rows to discard");

    script.fail(false);
    script.release(1);
    get(&section, &script, &a).await.unwrap();
    assert_eq!(
        metrics.count("discarded alias"),
        1,
        "a stored load is not discarded"
    );
}

// ── Bounds ──────────────────────────────────────────────────────────────

/// A load that does not answer within [`LOAD_TIMEOUT`] answers its waiters
/// an error and leaves the in-flight loads, so the next miss loads again.
/// Its query is dropped: it neither stores a late answer nor runs on. The
/// warning names how many keys it loaded and the first.
#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn a_hung_load_times_out_and_the_next_miss_loads_again() {
    let (section, script) = (section(10), Script::gated());
    let (a, rows_a) = (key("a", 1), rows(1));
    script.set(&a, &rows_a);

    let first = spawn_get(&section, &script, &a);
    script.started(1).await;
    let joined = spawn_get(&section, &script, &a);
    settle().await;
    for request in [first, joined] {
        let answer = tokio::time::timeout(LOAD_TIMEOUT + Duration::from_secs(1), request)
            .await
            .expect("the hung load times out")
            .unwrap();
        assert!(
            matches!(&answer, Err(RepositoryError::Internal(detail)) if detail == "proxy cache load timed out (keys: 1)"),
            "{answer:?}"
        );
    }
    assert_eq!((section.len(), section.loads()), (0, 0));
    assert!(logs_contain("A proxy cache load timed out"));
    assert!(logs_contain("keys=1"));
    assert!(logs_contain(r#"first=Some(Key { alias: "a", n: 1 })"#));

    // The timed-out load's query was dropped: the permit goes to the next
    // miss, which loads again and is stored.
    script.release(1);
    assert_rows(&get(&section, &script, &a).await, &rows_a);
    assert_eq!(script.calls().len(), 2);
    assert!(section.contains(&a));
    section.assert_consistent();
}

/// A load that answers just inside [`LOAD_TIMEOUT`] is stored.
#[tokio::test(start_paused = true)]
async fn a_slow_load_inside_the_timeout_is_stored() {
    let (section, script) = (section(10), Script::gated());
    let (a, rows_a) = (key("a", 1), rows(1));
    script.set(&a, &rows_a);

    let request = spawn_get(&section, &script, &a);
    script.started(1).await;
    tokio::time::advance(LOAD_TIMEOUT - Duration::from_millis(1)).await;
    script.release(1);
    assert_rows(&request.await.unwrap(), &rows_a);
    assert!(section.contains(&a));
}

/// A panicking load logs its payload with the section's label, how many
/// keys it loaded and the first.
#[tokio::test]
#[tracing_test::traced_test]
async fn a_panicking_load_logs_its_payload() {
    let (section, script) = (section(10), Script::gated());
    let a = key("a", 1);
    script.panic(true);

    let request = spawn_get(&section, &script, &a);
    script.started(1).await;
    script.release(1);
    request.await.unwrap().unwrap_err();
    assert!(logs_contain("A proxy cache load panicked"));
    assert!(logs_contain("scripted load panic"));
    assert!(logs_contain("map=\"alias\""));
    assert!(logs_contain("keys=1"));
    assert!(logs_contain(r#"first=Some(Key { alias: "a", n: 1 })"#));
}

/// A load that panics with a formatted message (a `String` payload) logs
/// that message too.
#[tokio::test]
#[tracing_test::traced_test]
async fn a_load_panicking_with_a_formatted_message_logs_it() {
    let (section, script) = (section(10), Script::gated());
    let a = key("a", 1);
    script.panic_formatted(true);

    let request = spawn_get(&section, &script, &a);
    script.started(1).await;
    script.release(1);
    request.await.unwrap().unwrap_err();
    assert!(logs_contain("scripted formatted panic over 1 keys"));
}
