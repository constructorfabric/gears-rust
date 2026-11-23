#[path = "../src/cluster_memory/mod.rs"]
mod cluster_memory;

use std::collections::BTreeMap;
use std::future::{Future, poll_fn};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::Poll;
use std::thread::{self, ThreadId};
use std::time::Duration;

use cluster_conformance::{
    ScenarioBackend, TimeControl, run_cache_conformance, run_lock_conformance,
};
use cluster_sdk::cache::{
    CacheConsistency, CacheEntry, CacheEvent, CacheFeatures, CacheWatch, CacheWatchEvent,
    CacheWatchSender, ClusterCacheBackend, PutRequest, Ttl, reserved_lease_cache,
};
use cluster_sdk::error::ClusterError;
use cluster_sdk::lease::{LeaseRecord, LeaseToken};
use cluster_sdk::lock::{DistributedLockBackend, LockFeatures};
use cluster_sdk::observability::NoopMetrics;
use event_broker::domain::EventBrokerCluster;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::{Barrier, Notify};
use tokio::task::JoinSet;
use tokio::time::{Instant, advance, timeout};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

use cluster_memory::{MemoryCache, MemoryLock};

const SWEEP_INTERVAL: Duration = Duration::from_millis(10);
const VIRTUAL_BUDGET: Duration = Duration::from_millis(250);
const TEST_DEADLINE: Duration = Duration::from_secs(5);
const LEASE_TTL: Duration = Duration::from_millis(50);
const PERIODIC_TTL: Duration = Duration::from_millis(30);
const UNSTARTED_BUDGET: Duration = Duration::from_millis(20);

fn put<'a>(key: &'a str, value: &'a [u8]) -> PutRequest<'a> {
    PutRequest {
        key,
        value,
        ttl: Ttl::Indefinite,
    }
}

#[tokio::test]
async fn the_memory_cache_satisfies_the_cluster_cache_contract() {
    let builds = AtomicUsize::new(0);
    let joined = Arc::new(AtomicUsize::new(0));
    run_cache_conformance(
        || async {
            builds.fetch_add(1, Ordering::Relaxed);
            let cache = MemoryCache::new();
            assert_eq!(cache.consistency(), CacheConsistency::Linearizable);
            assert_eq!(cache.features(), CacheFeatures::new(true));
            let worker = cache.prepare_sweep(SWEEP_INTERVAL).start();
            let joined = Arc::clone(&joined);
            ScenarioBackend::<dyn ClusterCacheBackend>::with_teardown(cache.clone(), async move {
                cache.shutdown();
                worker.shutdown().await;
                joined.fetch_add(1, Ordering::Relaxed);
            })
        },
        TimeControl::Virtual,
    )
    .await;
    assert_eq!(builds.into_inner(), 15);
    assert_eq!(joined.load(Ordering::Relaxed), 15);
}

#[tokio::test]
async fn value_guards_preserve_the_successor_after_a_key_is_deleted_and_recreated() {
    let cache = MemoryCache::new();
    let mut watch = cache.watch("k").await.unwrap();
    assert_eq!(
        cache.put_if_absent(put("k", b"old")).await.unwrap(),
        Some(CacheEntry {
            value: b"old".to_vec(),
            version: 1,
        })
    );
    assert!(cache.delete("k").await.unwrap());
    assert_eq!(
        cache.put_if_absent(put("k", b"new")).await.unwrap(),
        Some(CacheEntry {
            value: b"new".to_vec(),
            version: 1,
        })
    );
    let Err(ClusterError::CasConflict { key, current }) = cache
        .compare_and_swap_value("k", b"old", b"bad", Ttl::Indefinite)
        .await
    else {
        panic!("stale bytes must produce CasConflict");
    };
    assert_eq!(key, "k");
    assert_eq!(
        current,
        Some(CacheEntry {
            value: b"new".to_vec(),
            version: 1,
        })
    );
    assert!(!cache.compare_and_delete("k", b"old").await.unwrap());
    assert_eq!(cache.put_if_absent(put("k", b"bad")).await.unwrap(), None);
    assert_eq!(
        cache.get("k").await.unwrap(),
        Some(CacheEntry {
            value: b"new".to_vec(),
            version: 1,
        })
    );
    assert_eq!(
        cache
            .compare_and_swap_value("k", b"new", b"next", Ttl::Indefinite)
            .await
            .unwrap(),
        CacheEntry {
            value: b"next".to_vec(),
            version: 2,
        }
    );
    let Err(ClusterError::CasConflict { key, current }) = cache
        .compare_and_swap("k", 1, b"bad", Ttl::Indefinite)
        .await
    else {
        panic!("stale version must produce CasConflict");
    };
    assert_eq!(key, "k");
    assert_eq!(
        current,
        Some(CacheEntry {
            value: b"next".to_vec(),
            version: 2,
        })
    );
    assert!(cache.compare_and_delete("k", b"next").await.unwrap());
    assert_eq!(cache.get("k").await.unwrap(), None);
    let Err(ClusterError::CasConflict { key, current }) = cache
        .compare_and_swap("k", 1, b"bad", Ttl::Indefinite)
        .await
    else {
        panic!("version CAS on a missing key must produce CasConflict");
    };
    assert_eq!(key, "k");
    assert_eq!(current, None);
    let Err(ClusterError::CasConflict { key, current }) = cache
        .compare_and_swap_value("k", b"next", b"bad", Ttl::Indefinite)
        .await
    else {
        panic!("value CAS on a missing key must produce CasConflict");
    };
    assert_eq!(key, "k");
    assert_eq!(current, None);
    assert!(!cache.compare_and_delete("k", b"next").await.unwrap());
    assert!(!cache.delete("k").await.unwrap());
    assert_eq!(cache.get("k").await.unwrap(), None);

    cache.put(put("k", b"plain")).await.unwrap();
    assert_eq!(
        cache
            .compare_and_swap("k", 1, b"versioned", Ttl::Indefinite)
            .await
            .unwrap(),
        CacheEntry {
            value: b"versioned".to_vec(),
            version: 2,
        }
    );

    cache.shutdown();
    timeout(TEST_DEADLINE, async {
        let mut events = Vec::new();
        loop {
            match watch.recv().await {
                Some(CacheWatchEvent::Event(event)) => events.push(event),
                Some(CacheWatchEvent::Closed(ClusterError::Shutdown)) => break,
                other => panic!("expected ordered data through shutdown, got {other:?}"),
            }
        }
        assert_eq!(
            events,
            [
                CacheEvent::Changed { key: "k".into() },
                CacheEvent::Deleted { key: "k".into() },
                CacheEvent::Changed { key: "k".into() },
                CacheEvent::Changed { key: "k".into() },
                CacheEvent::Deleted { key: "k".into() },
                CacheEvent::Changed { key: "k".into() },
                CacheEvent::Changed { key: "k".into() },
            ]
        );
        assert!(watch.recv().await.is_none());
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn expiry_is_visible_before_the_sweep_and_the_sweeper_announces_removal() {
    let cache = MemoryCache::new();
    let prepared = cache.prepare_sweep(SWEEP_INTERVAL);
    let mut watch = cache.watch("lease/live").await.unwrap();
    cache
        .put(PutRequest {
            key: "lease/live",
            value: b"lease",
            ttl: Ttl::Of(LEASE_TTL),
        })
        .await
        .unwrap();
    cache.put(put("lease/keep", b"keep")).await.unwrap();
    cache.put(put("unrelated", b"exclude")).await.unwrap();

    advance(Duration::from_millis(49)).await;
    assert_eq!(
        cache.get("lease/live").await.unwrap(),
        Some(CacheEntry {
            value: b"lease".to_vec(),
            version: 1,
        })
    );
    assert!(cache.contains("lease/live").await.unwrap());
    let mut keys = cache.scan_prefix("lease/").await.unwrap();
    keys.sort_unstable();
    assert_eq!(keys, ["lease/keep", "lease/live"]);

    advance(Duration::from_millis(1)).await;
    assert_eq!(cache.get("lease/live").await.unwrap(), None);
    assert!(!cache.contains("lease/live").await.unwrap());
    assert_eq!(cache.scan_prefix("lease/").await.unwrap(), ["lease/keep"]);
    assert_eq!(
        cache.get("lease/keep").await.unwrap(),
        Some(CacheEntry {
            value: b"keep".to_vec(),
            version: 1,
        })
    );

    let Some(CacheWatchEvent::Event(event)) = timeout(VIRTUAL_BUDGET, watch.recv()).await.unwrap()
    else {
        panic!("watch must contain the committed lease");
    };
    assert_eq!(
        event,
        CacheEvent::Changed {
            key: "lease/live".into(),
        }
    );
    assert!(timeout(UNSTARTED_BUDGET, watch.recv()).await.is_err());

    for key in ["version", "value", "cad", "delete", "create", "overwrite"] {
        cache
            .put(PutRequest {
                key,
                value: b"previous",
                ttl: Ttl::Of(LEASE_TTL),
            })
            .await
            .unwrap();
        advance(LEASE_TTL).await;
    }
    let Err(ClusterError::CasConflict { key, current }) = cache
        .compare_and_swap("version", 1, b"bad", Ttl::Indefinite)
        .await
    else {
        panic!("version CAS on an expired key must produce CasConflict");
    };
    assert_eq!(key, "version");
    assert_eq!(current, None);
    let Err(ClusterError::CasConflict { key, current }) = cache
        .compare_and_swap_value("value", b"previous", b"bad", Ttl::Indefinite)
        .await
    else {
        panic!("value CAS on an expired key must produce CasConflict");
    };
    assert_eq!(key, "value");
    assert_eq!(current, None);
    assert!(!cache.compare_and_delete("cad", b"previous").await.unwrap());
    assert!(!cache.delete("delete").await.unwrap());
    assert_eq!(
        cache.put_if_absent(put("create", b"fresh")).await.unwrap(),
        Some(CacheEntry {
            value: b"fresh".to_vec(),
            version: 1,
        })
    );
    cache.put(put("overwrite", b"replacement")).await.unwrap();
    assert_eq!(
        cache.get("overwrite").await.unwrap(),
        Some(CacheEntry {
            value: b"replacement".to_vec(),
            version: 1,
        })
    );

    let finite = Ttl::Of(LEASE_TTL);
    assert_eq!(
        cache
            .put_if_absent(PutRequest {
                key: "finite-create",
                value: b"fresh",
                ttl: finite
            })
            .await
            .unwrap(),
        Some(CacheEntry {
            value: b"fresh".to_vec(),
            version: 1
        })
    );
    assert_eq!(
        cache
            .compare_and_swap("overwrite", 1, b"finite", finite)
            .await
            .unwrap(),
        CacheEntry {
            value: b"finite".to_vec(),
            version: 2
        }
    );
    assert_eq!(
        cache
            .compare_and_swap_value("create", b"fresh", b"finite", finite)
            .await
            .unwrap(),
        CacheEntry {
            value: b"finite".to_vec(),
            version: 2
        }
    );
    advance(LEASE_TTL).await;
    for key in ["finite-create", "overwrite", "create"] {
        assert_eq!(cache.get(key).await.unwrap(), None, "expiry of {key}");
    }

    let mut cad = cache.watch("cad").await.unwrap();
    let mut deleted = cache.watch("delete").await.unwrap();
    timeout(VIRTUAL_BUDGET, async {
        let worker = prepared.start();
        let Some(CacheWatchEvent::Event(event)) = watch.recv().await else {
            panic!("the initial sweep must announce physical lease removal");
        };
        assert_eq!(
            event,
            CacheEvent::Expired {
                key: "lease/live".into(),
            }
        );
        let Some(CacheWatchEvent::Event(event)) = cad.recv().await else {
            panic!("an expired CAD must leave physical removal to the sweeper");
        };
        assert_eq!(event, CacheEvent::Expired { key: "cad".into() });

        let written = Instant::now();
        cache
            .put(PutRequest {
                key: "lease/live",
                value: b"periodic",
                ttl: Ttl::Of(PERIODIC_TTL),
            })
            .await
            .unwrap();
        let Some(CacheWatchEvent::Event(event)) = watch.recv().await else {
            panic!("the later lease must be announced");
        };
        assert_eq!(
            event,
            CacheEvent::Changed {
                key: "lease/live".into(),
            }
        );
        let Some(CacheWatchEvent::Event(event)) = watch.recv().await else {
            panic!("a later sweep must announce removal after its TTL");
        };
        assert_eq!(
            event,
            CacheEvent::Expired {
                key: "lease/live".into(),
            }
        );
        assert!(written.elapsed() >= PERIODIC_TTL);
        assert_eq!(
            cache.get("lease/keep").await.unwrap(),
            Some(CacheEntry {
                value: b"keep".to_vec(),
                version: 1,
            })
        );
        assert_eq!(Arc::strong_count(&cache), 2);
        cache.shutdown();
        assert!(matches!(
            deleted.recv().await,
            Some(CacheWatchEvent::Closed(ClusterError::Shutdown))
        ));
        assert!(deleted.recv().await.is_none());
        assert!(matches!(
            cad.recv().await,
            Some(CacheWatchEvent::Closed(ClusterError::Shutdown))
        ));
        assert!(cad.recv().await.is_none());
        worker.shutdown().await;
        assert_eq!(Arc::strong_count(&cache), 1);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_slow_watch_reports_the_dropped_count_and_closes_even_when_full() {
    timeout(TEST_DEADLINE, async {
        let cache = MemoryCache::new();
        let mut exact = cache.watch("k").await.unwrap();
        let mut prefix = cache.watch_prefix("other/").await.unwrap();

        cache.put(put("other/x", b"value")).await.unwrap();
        let Some(CacheWatchEvent::Event(event)) = prefix.recv().await else {
            panic!("prefix watch must see the matching key");
        };
        assert_eq!(
            event,
            CacheEvent::Changed {
                key: "other/x".into(),
            }
        );

        cache.put(put("k/nested", b"exclude")).await.unwrap();
        let changed = CacheEvent::Changed { key: "k".into() };
        for _ in 0..306 {
            cache.put(put("k", b"value")).await.unwrap();
        }
        cache.put(put("other/after", b"value")).await.unwrap();
        let Some(CacheWatchEvent::Event(event)) = prefix.recv().await else {
            panic!("a full exact watch must leave other publishers and watches responsive");
        };
        assert_eq!(
            event,
            CacheEvent::Changed {
                key: "other/after".into(),
            }
        );
        for _ in 0..256 {
            let Some(CacheWatchEvent::Event(event)) = exact.recv().await else {
                panic!("all 256 ordinary slots must be used");
            };
            assert_eq!(event, changed);
        }
        cache.put(put("k", b"value")).await.unwrap();
        let Some(CacheWatchEvent::Lagged { dropped }) = exact.recv().await else {
            panic!("the next matching update must send a lag count first");
        };
        assert_eq!(dropped, 50);
        let Some(CacheWatchEvent::Event(event)) = exact.recv().await else {
            panic!("the update sending the lag count must also be delivered");
        };
        assert_eq!(event, changed);

        for _ in 0..258 {
            cache.put(put("k", b"value")).await.unwrap();
        }
        let Some(CacheWatchEvent::Event(event)) = exact.recv().await else {
            panic!("draining one event must free one ordinary slot");
        };
        assert_eq!(event, changed);
        cache.put(put("k", b"value")).await.unwrap();
        for _ in 0..255 {
            let Some(CacheWatchEvent::Event(event)) = exact.recv().await else {
                panic!("the original events must precede the next lag count");
            };
            assert_eq!(event, changed);
        }
        let Some(CacheWatchEvent::Lagged { dropped }) = exact.recv().await else {
            panic!("two lost writes must fill the last slot with a lag count");
        };
        assert_eq!(dropped, 2);
        cache.put(put("k", b"value")).await.unwrap();
        let Some(CacheWatchEvent::Lagged { dropped }) = exact.recv().await else {
            panic!("the write displaced by the prior lag count must be reported");
        };
        assert_eq!(dropped, 1);
        let Some(CacheWatchEvent::Event(event)) = exact.recv().await else {
            panic!("the ordinary change must follow the lag count");
        };
        assert_eq!(event, changed);

        for _ in 0..256 {
            cache.put(put("k", b"value")).await.unwrap();
        }
        cache.shutdown();
        for _ in 0..256 {
            let Some(CacheWatchEvent::Event(event)) = exact.recv().await else {
                panic!("shutdown must preserve all 256 buffered data events");
            };
            assert_eq!(event, changed);
        }
        assert!(matches!(
            exact.recv().await,
            Some(CacheWatchEvent::Closed(ClusterError::Shutdown))
        ));
        assert!(exact.recv().await.is_none());
        assert!(matches!(
            prefix.recv().await,
            Some(CacheWatchEvent::Closed(ClusterError::Shutdown))
        ));
        assert!(prefix.recv().await.is_none());
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn closing_the_cache_rejects_every_mutation_and_new_watch_while_reads_remain_live() {
    let cache = MemoryCache::new();
    cache.put(put("k/live", b"x")).await.unwrap();
    cache.shutdown();
    cache.shutdown();
    assert!(matches!(
        cache.put(put("k/live", b"replacement")).await,
        Err(ClusterError::Shutdown)
    ));
    assert!(matches!(
        cache.put_if_absent(put("k/free", b"new")).await,
        Err(ClusterError::Shutdown)
    ));
    assert!(matches!(
        cache
            .compare_and_swap("k/live", 1, b"replacement", Ttl::Indefinite)
            .await,
        Err(ClusterError::Shutdown)
    ));
    assert!(matches!(
        cache
            .compare_and_swap_value("k/live", b"x", b"replacement", Ttl::Indefinite)
            .await,
        Err(ClusterError::Shutdown)
    ));
    assert!(matches!(
        cache.compare_and_delete("k/live", b"x").await,
        Err(ClusterError::Shutdown)
    ));
    assert!(matches!(
        cache.delete("k/live").await,
        Err(ClusterError::Shutdown)
    ));
    assert!(matches!(
        cache.watch("k/live").await,
        Err(ClusterError::Shutdown)
    ));
    assert!(matches!(
        cache.watch_prefix("k/").await,
        Err(ClusterError::Shutdown)
    ));
    assert_eq!(
        cache.get("k/live").await.unwrap(),
        Some(CacheEntry {
            value: b"x".to_vec(),
            version: 1,
        })
    );
    assert!(cache.contains("k/live").await.unwrap());
    assert_eq!(cache.scan_prefix("k/").await.unwrap(), ["k/live"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_create_and_delete_publish_changes_in_the_order_that_took_effect() {
    timeout(TEST_DEADLINE, async {
        let cache = MemoryCache::new();
        let mut watch = cache.watch_prefix("p/").await.unwrap();
        let start = Arc::new(Barrier::new(3));
        let writer = tokio::spawn({
            let cache = Arc::clone(&cache);
            let start = Arc::clone(&start);
            async move {
                start.wait().await;
                cache.put_if_absent(put("p/k", b"one")).await.unwrap()
            }
        });
        let delete_task = tokio::spawn({
            let cache = Arc::clone(&cache);
            let start = Arc::clone(&start);
            async move {
                start.wait().await;
                cache.delete("p/k").await.unwrap()
            }
        });
        start.wait().await;
        let (created, removed) = tokio::join!(writer, delete_task);
        assert_eq!(
            created.unwrap(),
            Some(CacheEntry {
                value: b"one".to_vec(),
                version: 1,
            })
        );
        let removed = removed.unwrap();
        let final_entry = cache.get("p/k").await.unwrap();
        cache.put(put("p/end", b"sentinel")).await.unwrap();

        let mut events = Vec::new();
        loop {
            let Some(CacheWatchEvent::Event(event)) = watch.recv().await else {
                panic!("the concurrent changes must precede the sentinel");
            };
            let is_sentinel = event
                == (CacheEvent::Changed {
                    key: "p/end".into(),
                });
            events.push(event);
            if is_sentinel {
                break;
            }
        }
        if removed {
            assert_eq!(final_entry, None);
            assert_eq!(
                events,
                [
                    CacheEvent::Changed { key: "p/k".into() },
                    CacheEvent::Deleted { key: "p/k".into() },
                    CacheEvent::Changed {
                        key: "p/end".into()
                    },
                ]
            );
        } else {
            assert_eq!(
                final_entry,
                Some(CacheEntry {
                    value: b"one".to_vec(),
                    version: 1,
                })
            );
            assert_eq!(
                events,
                [
                    CacheEvent::Changed { key: "p/k".into() },
                    CacheEvent::Changed {
                        key: "p/end".into()
                    },
                ]
            );
        }

        assert_eq!(
            cache
                .put_if_absent(put("p/sequential", b"one"))
                .await
                .unwrap(),
            Some(CacheEntry {
                value: b"one".to_vec(),
                version: 1,
            })
        );
        assert!(cache.delete("p/sequential").await.unwrap());
        cache.shutdown();
        let mut sequential = Vec::new();
        loop {
            match watch.recv().await {
                Some(CacheWatchEvent::Event(event)) => sequential.push(event),
                Some(CacheWatchEvent::Closed(ClusterError::Shutdown)) => break,
                other => panic!("expected ordered data through shutdown, got {other:?}"),
            }
        }
        assert_eq!(
            sequential,
            [
                CacheEvent::Changed {
                    key: "p/sequential".into()
                },
                CacheEvent::Deleted {
                    key: "p/sequential".into()
                },
            ]
        );
        assert!(watch.recv().await.is_none());
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn put_preserves_a_value_for_the_maximum_finite_ttl() {
    let cache = MemoryCache::new();
    cache
        .put(PutRequest {
            key: "zero",
            value: b"instant",
            ttl: Ttl::Of(Duration::ZERO),
        })
        .await
        .unwrap();
    assert_eq!(cache.get("zero").await.unwrap(), None);

    cache
        .put(PutRequest {
            key: "put",
            value: b"new",
            ttl: Ttl::Of(Duration::MAX),
        })
        .await
        .unwrap();
    advance(Duration::from_millis(51)).await;
    assert_eq!(
        cache.get("put").await.unwrap(),
        Some(CacheEntry {
            value: b"new".to_vec(),
            version: 1
        })
    );
}

#[tokio::test(start_paused = true)]
async fn put_if_absent_creates_a_value_with_the_maximum_finite_ttl() {
    let cache = MemoryCache::new();
    assert_eq!(
        cache
            .put_if_absent(PutRequest {
                key: "create",
                value: b"fresh",
                ttl: Ttl::Of(Duration::MAX)
            })
            .await
            .unwrap(),
        Some(CacheEntry {
            value: b"fresh".to_vec(),
            version: 1
        })
    );
    advance(Duration::from_millis(51)).await;
    assert_eq!(
        cache.get("create").await.unwrap(),
        Some(CacheEntry {
            value: b"fresh".to_vec(),
            version: 1
        })
    );
}

#[tokio::test(start_paused = true)]
async fn version_cas_replaces_a_short_ttl_with_the_maximum_finite_ttl() {
    let cache = MemoryCache::new();
    cache
        .put(PutRequest {
            key: "version",
            value: b"old",
            ttl: Ttl::Of(LEASE_TTL),
        })
        .await
        .unwrap();
    assert_eq!(
        cache.get("version").await.unwrap(),
        Some(CacheEntry {
            value: b"old".to_vec(),
            version: 1
        })
    );
    advance(Duration::from_millis(10)).await;
    assert_eq!(
        cache
            .compare_and_swap("version", 1, b"updated", Ttl::Of(Duration::MAX))
            .await
            .unwrap(),
        CacheEntry {
            value: b"updated".to_vec(),
            version: 2
        }
    );
    advance(Duration::from_millis(41)).await;
    assert_eq!(
        cache.get("version").await.unwrap(),
        Some(CacheEntry {
            value: b"updated".to_vec(),
            version: 2
        })
    );
}

#[tokio::test(start_paused = true)]
async fn value_cas_replaces_a_short_ttl_with_the_maximum_finite_ttl() {
    let cache = MemoryCache::new();
    cache
        .put(PutRequest {
            key: "value",
            value: b"old",
            ttl: Ttl::Of(LEASE_TTL),
        })
        .await
        .unwrap();
    assert_eq!(
        cache.get("value").await.unwrap(),
        Some(CacheEntry {
            value: b"old".to_vec(),
            version: 1
        })
    );
    advance(Duration::from_millis(10)).await;
    assert_eq!(
        cache
            .compare_and_swap_value("value", b"old", b"next", Ttl::Of(Duration::MAX))
            .await
            .unwrap(),
        CacheEntry {
            value: b"next".to_vec(),
            version: 2
        }
    );
    advance(Duration::from_millis(41)).await;
    assert_eq!(
        cache.get("value").await.unwrap(),
        Some(CacheEntry {
            value: b"next".to_vec(),
            version: 2
        })
    );
}

const LOCK_TTL: Duration = Duration::from_millis(20);
const RETENTION: Duration = Duration::from_millis(100);
const LOCK_KEY: &str = "$lease/lock/job";

fn memory_lock(cache: Arc<dyn ClusterCacheBackend>) -> MemoryLock {
    let Ok(lock) = MemoryLock::new(reserved_lease_cache(cache), Arc::new(NoopMetrics)) else {
        panic!("the fixture must provide a linearizable cache");
    };
    lock.with_test_clock(RETENTION, PreparedMemory::test_nonce_seed())
}

#[tokio::test]
async fn the_memory_lock_satisfies_the_cluster_lock_contract() {
    let builds = AtomicUsize::new(0);
    let joined = Arc::new(AtomicUsize::new(0));
    run_lock_conformance(
        || async {
            builds.fetch_add(1, Ordering::Relaxed);
            let cache = MemoryCache::new();
            let lock = Arc::new(memory_lock(cache.clone()));
            assert_eq!(lock.features(), LockFeatures::new(true));
            let sweep = cache.prepare_sweep(SWEEP_INTERVAL).start();
            let joined = Arc::clone(&joined);
            ScenarioBackend::<dyn DistributedLockBackend>::with_teardown(lock.clone(), async move {
                lock.revoke().await;
                assert_eq!(lock.registered_guards(), 0);
                cache.shutdown();
                sweep.shutdown().await;
                joined.fetch_add(1, Ordering::Relaxed);
            })
        },
        TimeControl::Virtual,
    )
    .await;
    assert_eq!(builds.into_inner(), 6);
    assert_eq!(joined.load(Ordering::Relaxed), 6);
}

#[tokio::test(start_paused = true)]
async fn ordinary_cache_keys_are_independent_of_the_reserved_lock_record() {
    let cache = MemoryCache::new();
    let lock = memory_lock(cache.clone());
    let ordinary = Some(CacheEntry {
        value: b"ordinary".to_vec(),
        version: 1,
    });
    assert_eq!(
        cache
            .put_if_absent(put("lock/job", b"ordinary"))
            .await
            .unwrap(),
        ordinary
    );
    let before = lock.lease_now();
    let token = lock.acquire("job", "worker-a", LOCK_TTL).await.unwrap();
    assert_eq!(token, LeaseToken::new("job", "worker-a", 1));
    assert_eq!(cache.get("lock/job").await.unwrap(), ordinary);
    assert_eq!(
        cache.scan_prefix("$lease/").await.unwrap(),
        vec![LOCK_KEY.to_owned()]
    );
    let entry = cache.get(LOCK_KEY).await.unwrap().unwrap();
    let record = LeaseRecord::decode(&entry.value).unwrap();
    assert!((before + 20..=lock.lease_now() + 20).contains(&record.deadline_ms));
    assert_eq!(
        entry,
        CacheEntry {
            value: LeaseRecord {
                owner: "worker-a".to_owned(),
                deadline_ms: record.deadline_ms,
                fence: 1,
                nonce: PreparedMemory::test_nonce_seed(),
            }
            .encode(),
            version: 1,
        }
    );
    lock.release(&token).await.unwrap();
    assert_eq!(cache.get(LOCK_KEY).await.unwrap(), None);
    assert_eq!(cache.get("lock/job").await.unwrap(), ordinary);

    let weak = Arc::new(RecordingCache {
        consistency: CacheConsistency::EventuallyConsistent,
        ..RecordingCache::new(cache.clone())
    });
    assert!(matches!(
        MemoryLock::new(reserved_lease_cache(weak.clone()), Arc::new(NoopMetrics)),
        Err(ClusterError::InvalidConfig { reason })
            if reason == "event-broker memory lock requires a linearizable cache. Use the linearizable profile cache."
    ));
    assert_eq!(weak.operations.load(Ordering::Relaxed), 0);
    lock.revoke().await;
}

#[tokio::test(start_paused = true)]
async fn stored_lease_fences_survive_logical_lapse_inside_the_retention_window() {
    let cache = MemoryCache::new();
    let lock = memory_lock(cache.clone());
    let first = lock.acquire("job", "worker-a", LOCK_TTL).await.unwrap();
    assert_eq!(first, LeaseToken::new("job", "worker-a", 1));
    let initial = cache.get(LOCK_KEY).await.unwrap();
    for owner in ["worker-a", "worker-b"] {
        assert!(matches!(lock.acquire("job", owner, LOCK_TTL).await,
            Err(ClusterError::LockContended { name }) if name == "job"));
        assert_eq!(cache.get(LOCK_KEY).await.unwrap(), initial);
    }

    let sweep = cache.prepare_sweep(SWEEP_INTERVAL).start();
    advance(Duration::from_millis(25)).await;
    assert_eq!(cache.get(LOCK_KEY).await.unwrap(), initial);
    let second = lock.acquire("job", "worker-b", LOCK_TTL).await.unwrap();
    assert_eq!(second, LeaseToken::new("job", "worker-b", 2));
    let retained = cache.get(LOCK_KEY).await.unwrap();
    for stale in [
        LeaseToken::new("job", "worker-a", 2),
        LeaseToken::new("job", "worker-b", 1),
    ] {
        assert!(matches!(lock.renew(&stale, LOCK_TTL).await,
            Err(ClusterError::LockExpired { name }) if name == "job"));
        lock.release(&stale).await.unwrap();
        assert_eq!(cache.get(LOCK_KEY).await.unwrap(), retained);
    }

    advance(Duration::from_millis(10)).await;
    let before = lock.lease_now();
    lock.renew(&second, Duration::from_millis(40))
        .await
        .unwrap();
    let entry = cache.get(LOCK_KEY).await.unwrap().unwrap();
    let renewed = LeaseRecord::decode(&entry.value).unwrap();
    assert!((before + 40..=lock.lease_now() + 40).contains(&renewed.deadline_ms));
    assert_eq!(
        entry,
        CacheEntry {
            value: LeaseRecord {
                owner: "worker-b".to_owned(),
                deadline_ms: renewed.deadline_ms,
                fence: 2,
                nonce: PreparedMemory::test_nonce_seed().wrapping_add(5),
            }
            .encode(),
            version: 3,
        }
    );
    advance(Duration::from_millis(39)).await;
    assert!(matches!(lock.acquire("job", "worker-c", LOCK_TTL).await,
        Err(ClusterError::LockContended { name }) if name == "job"));
    advance(Duration::from_millis(1)).await;
    assert!(matches!(lock.renew(&second, LOCK_TTL).await,
        Err(ClusterError::LockExpired { name }) if name == "job"));
    advance(Duration::from_millis(99)).await;
    assert_eq!(cache.get(LOCK_KEY).await.unwrap(), Some(entry));
    advance(Duration::from_millis(1)).await;
    assert_eq!(cache.get(LOCK_KEY).await.unwrap(), None);
    assert_eq!(
        lock.acquire("job", "worker-c", LOCK_TTL).await.unwrap(),
        LeaseToken::new("job", "worker-c", 1)
    );

    let lapsed = lock.acquire("lapsed", "worker-d", LOCK_TTL).await.unwrap();
    advance(LOCK_TTL).await;
    lock.release(&lapsed).await.unwrap();
    lock.release(&lapsed).await.unwrap();
    assert_eq!(cache.get("$lease/lock/lapsed").await.unwrap(), None);

    cache
        .put(put("$lease/lock/opaque", b"foreign"))
        .await
        .unwrap();
    assert!(matches!(lock.acquire("opaque", "worker-e", LOCK_TTL).await,
        Err(ClusterError::LockContended { name }) if name == "opaque"));
    let foreign = LeaseToken::new("opaque", "worker-e", 1);
    assert!(matches!(lock.renew(&foreign, LOCK_TTL).await,
        Err(ClusterError::LockExpired { name }) if name == "opaque"));
    lock.release(&foreign).await.unwrap();
    assert_eq!(
        cache.get("$lease/lock/opaque").await.unwrap(),
        Some(CacheEntry {
            value: b"foreign".to_vec(),
            version: 1,
        })
    );
    lock.revoke().await;
    cache.shutdown();
    sweep.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn renewal_cannot_replace_a_recreated_lease() {
    let cache = MemoryCache::new();
    let delegate = Arc::new(RecordingCache {
        record_renewal: true,
        ..RecordingCache::new(cache.clone())
    });
    let lock = memory_lock(delegate.clone());
    let observed = LeaseRecord {
        owner: "worker-a".to_owned(),
        deadline_ms: lock.lease_now() + 1000,
        fence: 1,
        nonce: 10,
    }
    .encode();
    cache.put(put(LOCK_KEY, &observed)).await.unwrap();
    let before = lock.lease_now();
    assert!(matches!(
        lock.renew(&LeaseToken::new("job", "worker-a", 1), LOCK_TTL).await,
        Err(ClusterError::LockExpired { name }) if name == "job"
    ));
    {
        let writes = delegate.writes.lock().unwrap();
        let [write] = writes.as_slice() else {
            panic!("expected one value-conditional write, got {writes:?}");
        };
        let requested = LeaseRecord::decode(&write.new_value).unwrap();
        assert!((before + 20..=lock.lease_now() + 20).contains(&requested.deadline_ms));
        assert_eq!(
            *write,
            ConditionalWrite {
                key: LOCK_KEY.to_owned(),
                expected_value: observed.clone(),
                new_value: LeaseRecord {
                    owner: "worker-a".to_owned(),
                    deadline_ms: requested.deadline_ms,
                    fence: 1,
                    nonce: PreparedMemory::test_nonce_seed(),
                }
                .encode(),
                ttl: Ttl::Of(LOCK_TTL + RETENTION),
            }
        );
    }
    assert_eq!(
        cache.get(LOCK_KEY).await.unwrap(),
        Some(CacheEntry {
            value: observed,
            version: 1
        })
    );
    lock.revoke().await;

    let cache = MemoryCache::new();
    let lock = memory_lock(cache.clone());
    let previous = lock.acquire("job", "worker-a", LOCK_TTL).await.unwrap();
    assert_eq!(previous, LeaseToken::new("job", "worker-a", 1));
    advance(Duration::from_millis(25)).await;
    let successor = lock.acquire("job", "worker-b", LOCK_TTL).await.unwrap();
    assert_eq!(successor, LeaseToken::new("job", "worker-b", 2));
    let held = cache.get(LOCK_KEY).await.unwrap().unwrap();
    assert!(matches!(lock.renew(&previous, LOCK_TTL).await,
        Err(ClusterError::LockExpired { name }) if name == "job"));
    assert_eq!(cache.get(LOCK_KEY).await.unwrap(), Some(held.clone()));
    lock.renew(&successor, LOCK_TTL).await.unwrap();
    assert_eq!(
        cache.get(LOCK_KEY).await.unwrap(),
        Some(CacheEntry {
            value: LeaseRecord {
                owner: "worker-b".to_owned(),
                deadline_ms: 45,
                fence: 2,
                nonce: PreparedMemory::test_nonce_seed().wrapping_add(3),
            }
            .encode(),
            version: 3,
        })
    );
    lock.revoke().await;
}

async fn poll_pending<F: Future>(mut future: std::pin::Pin<&mut F>) {
    poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn cancelling_a_waiter_does_not_acquire_the_lock_after_release() {
    let cache = MemoryCache::new();
    let delegate = Arc::new(RecordingCache {
        watch_mode: WatchMode::Recorded,
        ..RecordingCache::new(cache.clone())
    });
    let lock = memory_lock(delegate.clone());
    let first = lock.acquire("job", "worker-a", LOCK_TTL).await.unwrap();
    assert_eq!(first, LeaseToken::new("job", "worker-a", 1));
    let mut waiter = Box::pin(lock.acquire_waiting("job", "worker-b", LOCK_TTL, TEST_DEADLINE));
    poll_pending(waiter.as_mut()).await;
    let sender = {
        let watch = delegate.watch.lock().unwrap();
        let Some((key, sender)) = &*watch else {
            panic!("no exact watch was opened");
        };
        assert_eq!(key, LOCK_KEY);
        assert!(!sender.is_closed());
        sender.clone()
    };
    drop(waiter);
    // Only the SDK watch forwarding task needs a turn to drop its upstream receiver.
    tokio::task::yield_now().await;
    assert!(sender.is_closed());
    lock.release(&first).await.unwrap();
    let mut held = lock.acquire("job", "worker-c", LOCK_TTL).await.unwrap();
    assert_eq!(held, LeaseToken::new("job", "worker-c", 1));

    let start = Instant::now();
    for (event, owner) in [
        (CacheWatchEvent::Lagged { dropped: 2 }, "after-lag"),
        (CacheWatchEvent::Reset, "after-reset"),
    ] {
        let mut waiter = Box::pin(lock.acquire_waiting("job", owner, LOCK_TTL, TEST_DEADLINE));
        poll_pending(waiter.as_mut()).await;
        lock.release(&held).await.unwrap();
        {
            let watch = delegate.watch.lock().unwrap();
            let Some((key, sender)) = &*watch else {
                panic!("no exact watch was opened");
            };
            assert_eq!(key, LOCK_KEY);
            sender.try_send(event).unwrap();
        }
        held = timeout(TEST_DEADLINE, waiter).await.unwrap().unwrap();
        assert_eq!(held, LeaseToken::new("job", owner, 1));
        assert_eq!(Instant::now(), start);
    }
    lock.revoke().await;

    let cache = MemoryCache::new();
    let lock = memory_lock(cache.clone());
    let start = Instant::now();
    let first = lock
        .acquire_waiting("job", "worker-a", LOCK_TTL, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(first, LeaseToken::new("job", "worker-a", 1));
    assert!(
        matches!(lock.acquire_waiting("job", "other", LOCK_TTL, Duration::ZERO).await,
        Err(ClusterError::LockTimeout { name, waited: Duration::ZERO }) if name == "job")
    );
    let mut waiter = Box::pin(lock.acquire_waiting("job", "worker-b", LOCK_TTL, TEST_DEADLINE));
    poll_pending(waiter.as_mut()).await;
    lock.release(&first).await.unwrap();
    let next = timeout(TEST_DEADLINE, waiter).await.unwrap().unwrap();
    assert_eq!(next, LeaseToken::new("job", "worker-b", 1));
    assert_eq!(Instant::now(), start);

    let mut waiter = Box::pin(lock.acquire_waiting("job", "worker-c", LOCK_TTL, TEST_DEADLINE));
    poll_pending(waiter.as_mut()).await;
    let stored = cache.get(LOCK_KEY).await.unwrap();
    advance(Duration::from_millis(25)).await;
    assert_eq!(cache.get(LOCK_KEY).await.unwrap(), stored);
    let next = timeout(TEST_DEADLINE, waiter).await.unwrap().unwrap();
    assert_eq!(next, LeaseToken::new("job", "worker-c", 2));

    let mut waiter = Box::pin(lock.acquire_waiting("job", "worker-d", LOCK_TTL, TEST_DEADLINE));
    poll_pending(waiter.as_mut()).await;
    let retained = cache.get(LOCK_KEY).await.unwrap();
    cache.shutdown();
    assert!(matches!(
        timeout(TEST_DEADLINE, waiter).await.unwrap(),
        Err(ClusterError::Shutdown)
    ));
    assert_eq!(cache.get(LOCK_KEY).await.unwrap(), retained);
    lock.revoke().await;

    let cache = MemoryCache::new();
    let delegate = Arc::new(RecordingCache {
        watch_mode: WatchMode::Ended,
        ..RecordingCache::new(cache.clone())
    });
    let lock = memory_lock(delegate.clone());
    cache.put(put(LOCK_KEY, b"opaque")).await.unwrap();
    let budget = Duration::from_secs(2);
    assert!(matches!(
        timeout(TEST_DEADLINE, lock.acquire_waiting("job", "worker", LOCK_TTL, budget))
            .await
            .unwrap(),
        Err(ClusterError::Provider { kind: cluster_sdk::error::ProviderErrorKind::Other, message })
            if message == "memory lock watch ended eight consecutive times"
    ));
    assert_eq!(delegate.watch_calls.load(Ordering::Relaxed), 8);
    assert_eq!(
        cache.get(LOCK_KEY).await.unwrap(),
        Some(CacheEntry {
            value: b"opaque".to_vec(),
            version: 1,
        })
    );
    lock.revoke().await;
}

#[tokio::test(start_paused = true)]
async fn guard_shutdown_joins_workers_and_rejects_new_claims() {
    let cache = MemoryCache::new();
    let lock = memory_lock(cache.clone());
    let first = lock.try_lock("job", LOCK_TTL).await.unwrap();
    assert_eq!(first.name(), "job");
    assert_eq!(lock.registered_guards(), 1);
    let entry = cache.get(LOCK_KEY).await.unwrap().unwrap();
    let initial = LeaseRecord::decode(&entry.value).unwrap();
    assert_eq!(
        uuid::Uuid::parse_str(&initial.owner)
            .unwrap()
            .get_version_num(),
        4
    );
    assert!(matches!(
        timeout(TEST_DEADLINE, first.release()).await,
        Ok(Ok(()))
    ));
    assert_eq!(cache.get(LOCK_KEY).await.unwrap(), None);
    let dropped = lock.lock("job", LOCK_TTL, Duration::ZERO).await.unwrap();
    let entry = cache.get(LOCK_KEY).await.unwrap().unwrap();
    let other = LeaseRecord::decode(&entry.value).unwrap();
    assert_eq!(
        uuid::Uuid::parse_str(&other.owner)
            .unwrap()
            .get_version_num(),
        4
    );
    assert_ne!(initial.owner, other.owner);
    assert_eq!(lock.registered_guards(), 1);
    drop(dropped);
    tokio::task::yield_now().await;
    assert_eq!(cache.get(LOCK_KEY).await.unwrap(), Some(entry));

    let guard = lock.try_lock("held", LOCK_TTL).await.unwrap();
    assert_eq!(lock.registered_guards(), 1);
    let mut waiter = Box::pin(lock.acquire_waiting("held", "waiter", LOCK_TTL, TEST_DEADLINE));
    poll_pending(waiter.as_mut()).await;
    let before = cache.get("$lease/lock/held").await.unwrap();
    timeout(TEST_DEADLINE, async {
        tokio::join!(lock.revoke(), lock.revoke());
    })
    .await
    .unwrap();
    assert_eq!(lock.registered_guards(), 0);
    assert!(matches!(
        timeout(TEST_DEADLINE, waiter).await.unwrap(),
        Err(ClusterError::Shutdown)
    ));
    assert!(matches!(guard.renew(LOCK_TTL).await,
        Err(ClusterError::LockExpired { name }) if name == "held"));
    guard.release().await.unwrap();
    assert_eq!(cache.get("$lease/lock/held").await.unwrap(), before);
    assert!(matches!(
        lock.try_lock("closed", LOCK_TTL).await,
        Err(ClusterError::Shutdown)
    ));
    assert!(matches!(
        lock.lock("closed", LOCK_TTL, Duration::ZERO).await,
        Err(ClusterError::Shutdown)
    ));
    assert!(matches!(
        lock.acquire("closed", "worker", LOCK_TTL).await,
        Err(ClusterError::Shutdown)
    ));
    assert!(matches!(
        lock.acquire_waiting("closed", "worker", LOCK_TTL, Duration::ZERO)
            .await,
        Err(ClusterError::Shutdown)
    ));
    assert_eq!(cache.get("$lease/lock/closed").await.unwrap(), None);

    let replacement = memory_lock(cache.clone());
    assert!(
        matches!(replacement.acquire("held", "successor", LOCK_TTL).await,
        Err(ClusterError::LockContended { name }) if name == "held")
    );
    advance(Duration::from_millis(25)).await;
    assert_eq!(
        replacement
            .acquire("held", "successor", LOCK_TTL)
            .await
            .unwrap(),
        LeaseToken::new("held", "successor", 2)
    );
    replacement.revoke().await;

    let cache = MemoryCache::new();
    let gate = Arc::new(ClaimGate::default());
    let delegate = Arc::new(RecordingCache {
        claim_gate: Some(Arc::clone(&gate)),
        ..RecordingCache::new(cache.clone())
    });
    let lock = memory_lock(delegate);
    let mut claiming = Box::pin(lock.try_lock("job", LOCK_TTL));
    poll_pending(claiming.as_mut()).await;
    timeout(TEST_DEADLINE, gate.written.notified())
        .await
        .unwrap();
    assert_eq!(lock.registered_guards(), 0);
    assert!(cache.contains(LOCK_KEY).await.unwrap());
    timeout(TEST_DEADLINE, lock.revoke()).await.unwrap();
    gate.resume.notify_one();
    assert!(matches!(
        timeout(TEST_DEADLINE, claiming).await.unwrap(),
        Err(ClusterError::Shutdown)
    ));
    assert_eq!(lock.registered_guards(), 0);
    assert_eq!(cache.get(LOCK_KEY).await.unwrap(), None);
}

#[tokio::test(start_paused = true)]
async fn the_waiter_subscribes_before_its_first_claim() {
    let delegate = Arc::new(RecordingCache::new(MemoryCache::new()));
    let lock = memory_lock(delegate.clone());
    let token = lock
        .acquire_waiting("job", "worker-a", LOCK_TTL, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(token, LeaseToken::new("job", "worker-a", 1));
    assert_eq!(
        *delegate.calls.lock().unwrap(),
        [
            ClaimCall::WatchRegistered(LOCK_KEY.to_owned()),
            ClaimCall::PutIfAbsent(LOCK_KEY.to_owned()),
        ]
    );
    lock.revoke().await;
}

#[tokio::test(start_paused = true)]
async fn reset_wakes_a_waiter_without_advancing_its_clock() {
    let delegate = Arc::new(RecordingCache {
        watch_mode: WatchMode::Recorded,
        ..RecordingCache::new(MemoryCache::new())
    });
    let lock = memory_lock(delegate.clone());
    let held = lock.acquire("job", "worker-a", LOCK_TTL).await.unwrap();
    assert_eq!(held, LeaseToken::new("job", "worker-a", 1));
    let mut waiter = Box::pin(lock.acquire_waiting("job", "worker-b", LOCK_TTL, TEST_DEADLINE));
    poll_pending(waiter.as_mut()).await;
    lock.release(&held).await.unwrap();
    let start = Instant::now();
    {
        let watch = delegate.watch.lock().unwrap();
        let Some((key, sender)) = &*watch else {
            panic!("the pending waiter must have registered an exact watch");
        };
        assert_eq!(key, LOCK_KEY);
        sender.try_send(CacheWatchEvent::Reset).unwrap();
    }
    let next = timeout(TEST_DEADLINE, waiter).await.unwrap().unwrap();
    assert_eq!(next, LeaseToken::new("job", "worker-b", 1));
    assert_eq!(Instant::now(), start);
    lock.revoke().await;
}

#[tokio::test(start_paused = true)]
async fn dropping_a_guard_ends_its_worker_before_the_next_guard_is_registered() {
    let cache = MemoryCache::new();
    let lock = memory_lock(cache.clone());
    let guard = lock.try_lock("job", LOCK_TTL).await.unwrap();
    assert_eq!(guard.name(), "job");
    assert!(matches!(
        timeout(TEST_DEADLINE, guard.renew(LOCK_TTL)).await,
        Ok(Ok(()))
    ));
    let entry = cache.get(LOCK_KEY).await.unwrap().unwrap();
    drop(guard);
    // Paused time advances to this timer only after the runnable receiver has finished.
    tokio::time::sleep(Duration::from_millis(1)).await;
    let next = lock.try_lock("next", LOCK_TTL).await.unwrap();
    assert_eq!(next.name(), "next");
    assert_eq!(lock.registered_guards(), 1);
    assert!(matches!(
        timeout(TEST_DEADLINE, next.release()).await,
        Ok(Ok(()))
    ));
    assert_eq!(cache.get("$lease/lock/next").await.unwrap(), None);
    advance(Duration::from_millis(18)).await;
    assert_eq!(cache.get(LOCK_KEY).await.unwrap(), Some(entry));
    assert!(matches!(lock.acquire("job", "successor", LOCK_TTL).await,
        Err(ClusterError::LockContended { name }) if name == "job"));
    advance(Duration::from_millis(1)).await;
    assert_eq!(
        lock.acquire("job", "successor", LOCK_TTL).await.unwrap(),
        LeaseToken::new("job", "successor", 2)
    );
    timeout(TEST_DEADLINE, lock.revoke()).await.unwrap();
    assert_eq!(lock.registered_guards(), 0);
}

#[tokio::test(start_paused = true)]
async fn vacant_reads_bound_acquisition_to_three_create_attempts() {
    use ClaimCall::{PutIfAbsent, Read};

    let cache = MemoryCache::new();
    let delegate = Arc::new(RecordingCache {
        vacant_creates: AtomicUsize::new(2),
        ..RecordingCache::new(cache.clone())
    });
    let lock = memory_lock(delegate.clone());
    let third = lock.acquire("job", "worker-a", LOCK_TTL).await;
    assert_eq!(
        *delegate.calls.lock().unwrap(),
        [
            PutIfAbsent(LOCK_KEY.to_owned()),
            Read(LOCK_KEY.to_owned()),
            PutIfAbsent(LOCK_KEY.to_owned()),
            Read(LOCK_KEY.to_owned()),
            PutIfAbsent(LOCK_KEY.to_owned()),
        ]
    );
    assert_eq!(third.unwrap(), LeaseToken::new("job", "worker-a", 1));
    assert_eq!(
        cache.get(LOCK_KEY).await.unwrap(),
        Some(CacheEntry {
            value: LeaseRecord {
                owner: "worker-a".to_owned(),
                fence: 1,
                deadline_ms: 20,
                nonce: PreparedMemory::test_nonce_seed().wrapping_add(2),
            }
            .encode(),
            version: 1,
        })
    );
    lock.revoke().await;

    let cache = MemoryCache::new();
    let delegate = Arc::new(RecordingCache {
        vacant_creates: AtomicUsize::new(usize::MAX),
        ..RecordingCache::new(cache.clone())
    });
    let lock = memory_lock(delegate.clone());
    assert!(matches!(lock.acquire("job", "worker-a", LOCK_TTL).await,
        Err(ClusterError::LockContended { name }) if name == "job"));
    assert_eq!(
        *delegate.calls.lock().unwrap(),
        [
            PutIfAbsent(LOCK_KEY.to_owned()),
            Read(LOCK_KEY.to_owned()),
            PutIfAbsent(LOCK_KEY.to_owned()),
            Read(LOCK_KEY.to_owned()),
            PutIfAbsent(LOCK_KEY.to_owned()),
            Read(LOCK_KEY.to_owned()),
        ]
    );
    assert_eq!(cache.get(LOCK_KEY).await.unwrap(), None);
    lock.revoke().await;
}

#[derive(Debug, PartialEq, Eq)]
enum ClaimCall {
    WatchRegistered(String),
    PutIfAbsent(String),
    Read(String),
}

#[derive(Debug, PartialEq, Eq)]
struct ConditionalWrite {
    key: String,
    expected_value: Vec<u8>,
    new_value: Vec<u8>,
    ttl: Ttl,
}

#[derive(Default)]
struct ClaimGate {
    written: Notify,
    resume: Notify,
}

#[derive(Clone, Copy)]
enum WatchMode {
    Native,
    NativeThroughLag,
    Recorded,
    Ended,
}

struct RecordingCache {
    inner: Arc<MemoryCache>,
    consistency: CacheConsistency,
    record_renewal: bool,
    watch_mode: WatchMode,
    claim_gate: Option<Arc<ClaimGate>>,
    renew_attempts: AtomicUsize,
    renew_errors: Mutex<std::collections::VecDeque<Option<ClusterError>>>,
    read_error: Mutex<Option<ClusterError>>,
    watch_error: Mutex<Option<ClusterError>>,
    watch_calls: AtomicUsize,
    operations: AtomicUsize,
    vacant_creates: AtomicUsize,
    calls: Mutex<Vec<ClaimCall>>,
    writes: Mutex<Vec<ConditionalWrite>>,
    watch: Mutex<Option<(String, CacheWatchSender)>>,
}

impl RecordingCache {
    fn new(inner: Arc<MemoryCache>) -> Self {
        Self {
            inner,
            consistency: CacheConsistency::Linearizable,
            record_renewal: false,
            watch_mode: WatchMode::Native,
            claim_gate: None,
            renew_attempts: AtomicUsize::new(0),
            renew_errors: Mutex::default(),
            read_error: Mutex::default(),
            watch_error: Mutex::default(),
            watch_calls: AtomicUsize::new(0),
            operations: AtomicUsize::new(0),
            vacant_creates: AtomicUsize::new(0),
            calls: Mutex::new(Vec::new()),
            writes: Mutex::new(Vec::new()),
            watch: Mutex::new(None),
        }
    }

    fn record(&self, call: ClaimCall) {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(call);
    }

    fn cache(&self) -> &MemoryCache {
        self.operations.fetch_add(1, Ordering::Relaxed);
        &self.inner
    }
}

#[toolkit::async_trait]
impl ClusterCacheBackend for RecordingCache {
    fn consistency(&self) -> CacheConsistency {
        self.consistency
    }

    fn features(&self) -> CacheFeatures {
        self.inner.features()
    }

    async fn get(&self, key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        self.record(ClaimCall::Read(key.to_owned()));
        if let Some(error) = self
            .read_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            return Err(error);
        }
        self.cache().get(key).await
    }

    async fn put(&self, req: PutRequest<'_>) -> Result<(), ClusterError> {
        self.cache().put(req).await
    }

    async fn delete(&self, key: &str) -> Result<bool, ClusterError> {
        self.cache().delete(key).await
    }

    async fn contains(&self, key: &str) -> Result<bool, ClusterError> {
        self.cache().contains(key).await
    }

    async fn put_if_absent(&self, req: PutRequest<'_>) -> Result<Option<CacheEntry>, ClusterError> {
        self.record(ClaimCall::PutIfAbsent(req.key.to_owned()));
        if self
            .vacant_creates
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                left.checked_sub(1)
            })
            .is_ok()
        {
            return Ok(None);
        }
        let outcome = self.cache().put_if_absent(req).await?;
        if let Some(gate) = &self.claim_gate {
            gate.written.notify_one();
            timeout(TEST_DEADLINE, gate.resume.notified())
                .await
                .unwrap_or_else(|_| panic!("the claim confirmation was never resumed"));
        }
        Ok(outcome)
    }

    async fn compare_and_swap(
        &self,
        key: &str,
        version: u64,
        value: &[u8],
        ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        self.cache()
            .compare_and_swap(key, version, value, ttl)
            .await
    }

    async fn compare_and_swap_value(
        &self,
        key: &str,
        expected: &[u8],
        value: &[u8],
        ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        self.renew_attempts.fetch_add(1, Ordering::Relaxed);
        if let Some(Some(error)) = self
            .renew_errors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
        {
            return Err(error);
        }
        if self.record_renewal {
            self.writes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(ConditionalWrite {
                    key: key.to_owned(),
                    expected_value: expected.to_vec(),
                    new_value: value.to_vec(),
                    ttl,
                });
            return Err(ClusterError::CasConflict {
                key: key.to_owned(),
                current: None,
            });
        }
        self.cache()
            .compare_and_swap_value(key, expected, value, ttl)
            .await
    }

    async fn compare_and_delete(&self, key: &str, expected: &[u8]) -> Result<bool, ClusterError> {
        self.cache().compare_and_delete(key, expected).await
    }

    async fn watch(&self, key: &str) -> Result<CacheWatch, ClusterError> {
        self.watch_calls.fetch_add(1, Ordering::Relaxed);
        if let Some(error) = self
            .watch_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            return Err(error);
        }
        let watch = match self.watch_mode {
            WatchMode::Native => self.cache().watch(key).await,
            WatchMode::NativeThroughLag => {
                let mut native = self.cache().watch(key).await?;
                let (sender, feed) = CacheWatch::channel(4);
                // End at the native lag notice so a subsequent change cannot cause the observed read.
                tokio::spawn(async move {
                    while let Some(event) = native.recv().await {
                        let last = matches!(event, CacheWatchEvent::Lagged { .. });
                        if sender.send(event).await.is_err() || last {
                            break;
                        }
                    }
                });
                Ok(feed)
            }
            WatchMode::Recorded => {
                let (sender, watch) = CacheWatch::channel(4);
                *self.watch.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some((key.to_owned(), sender));
                Ok(watch)
            }
            WatchMode::Ended => {
                let (_, watch) = CacheWatch::channel(4);
                Ok(watch)
            }
        }?;
        self.record(ClaimCall::WatchRegistered(key.to_owned()));
        Ok(watch)
    }

    async fn watch_prefix(&self, prefix: &str) -> Result<CacheWatch, ClusterError> {
        self.cache().watch_prefix(prefix).await
    }
}

use cluster_conformance::run_leader_conformance;
use cluster_sdk::cache::ClusterCacheV1;
use cluster_sdk::client::ClusterClient;
use cluster_sdk::dto::{
    CacheDescriptor, LeaderElectionDescriptor, LockDescriptor, ProfileDescriptor, ProfileHealth,
    WireCacheConsistency, WireCacheFeatures, WireLeaderElectionFeatures, WireLockFeatures,
};
use cluster_sdk::intern::intern_existing;
use cluster_sdk::leader::{
    ElectionConfig, LeaderElectionBackend, LeaderStatus, LeaderWatch, LeaderWatchEvent,
};
use cluster_sdk::observability::ClusterMetrics;
use cluster_sdk::{ClusterProfile, validate_cache_key};
use toolkit::bootstrap::OopCompositionError;
use toolkit::client_hub::ClientHub;
use toolkit::discovery::{
    ConsumerRegistration, EndpointResolver, ResolveError, ResolveTarget, WireOutcome,
};

use cluster_memory::{
    EbMemoryOptions, MemoryLeader, PreparedMemory, RunningMemory, TerminalGate, prepare_memory,
};

const ELECTION_KEY: &str = "$lease/election/job";

#[derive(Clone, Copy)]
struct AlphaProfile;

impl ClusterProfile for AlphaProfile {
    const NAME: &'static str = "alpha";
}

fn memory_options(names: &[&str]) -> EbMemoryOptions {
    EbMemoryOptions {
        profiles: names.iter().map(|name| (*name).to_owned()).collect(),
        sweep_interval: SWEEP_INTERVAL,
        metrics: Arc::new(NoopMetrics),
    }
}

fn prepared_memory(names: &[&str]) -> PreparedMemory {
    PreparedMemory::prepare_for_test(memory_options(names), |_, cache| cache)
        .unwrap_or_else(|error| panic!("prepare real profiles: {error}"))
}

async fn election_event(watch: &mut LeaderWatch) -> LeaderWatchEvent {
    timeout(TEST_DEADLINE, watch.changed())
        .await
        .unwrap_or_else(|_| panic!("election event arrives"))
}

#[tokio::test]
async fn the_memory_leader_satisfies_the_cluster_election_contract() {
    let built = AtomicUsize::new(0);
    let joined = Arc::new(AtomicUsize::new(0));
    run_leader_conformance(
        || async {
            let prepared = prepared_memory(&["alpha"]);
            let sweep = prepared.test_profile("alpha").cache;
            let RunningMemory { client, cleanup } = prepared.start();
            let leader = client.leader_election_backend("alpha").unwrap();
            assert_eq!(
                leader.features(),
                cluster_sdk::leader::LeaderElectionFeatures::new(true)
            );
            built.fetch_add(1, Ordering::Relaxed);
            let joined = Arc::clone(&joined);
            ScenarioBackend::<dyn LeaderElectionBackend>::with_teardown(leader, async move {
                timeout(TEST_DEADLINE, cleanup.finish())
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(sweep.test_sweep_counts(), (1, 0));
                joined.fetch_add(1, Ordering::Relaxed);
            })
        },
        TimeControl::Virtual,
    )
    .await;
    assert_eq!(built.into_inner(), 7);
    assert_eq!(joined.load(Ordering::Relaxed), 7);
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Metric {
    Cache(String, String),
    Lock(String, String),
    Transition(String),
    Reset(String),
}

#[derive(Default)]
struct MemoryMetrics(Mutex<Vec<Metric>>);

impl MemoryMetrics {
    fn record(&self, metric: Metric) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(metric);
    }
}

impl ClusterMetrics for MemoryMetrics {
    fn cache_op(&self, op: &str, result: &str) {
        self.record(Metric::Cache(op.to_owned(), result.to_owned()));
    }

    fn cache_op_duration(&self, _op: &str, _seconds: f64) {}

    fn lock_op(&self, op: &str, result: &str) {
        self.record(Metric::Lock(op.to_owned(), result.to_owned()));
    }

    fn lock_op_duration(&self, _op: &str, _seconds: f64) {}

    fn leader_transition(&self, transition: &str) {
        self.record(Metric::Transition(transition.to_owned()));
    }

    fn watch_reset(&self, primitive: &str) {
        self.record(Metric::Reset(primitive.to_owned()));
    }

    fn provider_error(&self, _kind: &str) {}
}

#[tokio::test(start_paused = true)]
async fn the_memory_client_returns_stable_backends_and_separate_profile_state() {
    let nonce_seed = PreparedMemory::test_nonce_seed();
    let metrics = Arc::new(MemoryMetrics::default());
    let prepared = PreparedMemory::prepare_for_test(
        EbMemoryOptions {
            metrics: metrics.clone(),
            ..memory_options(&["alpha", "beta"])
        },
        |_, cache| cache,
    )
    .unwrap();
    let RunningMemory { client, cleanup } = prepared.start();
    let a_cache = client.cache_backend("alpha").unwrap();
    let b_cache = client.cache_backend("beta").unwrap();
    assert!(Arc::ptr_eq(
        &a_cache,
        &client.cache_backend("alpha").unwrap()
    ));
    assert!(!Arc::ptr_eq(&a_cache, &b_cache));
    let a_lock = client.lock_backend("alpha").unwrap();
    let b_lock = client.lock_backend("beta").unwrap();
    assert!(Arc::ptr_eq(&a_lock, &client.lock_backend("alpha").unwrap()));
    assert!(!Arc::ptr_eq(&a_lock, &b_lock));
    let a_leader = client.leader_election_backend("alpha").unwrap();
    let b_leader = client.leader_election_backend("beta").unwrap();
    assert!(Arc::ptr_eq(
        &a_leader,
        &client.leader_election_backend("alpha").unwrap()
    ));
    assert!(!Arc::ptr_eq(&a_leader, &b_leader));

    let hub = ClientHub::new();
    hub.register::<dyn ClusterClient>(client.clone());
    let facade = ClusterCacheV1::resolver(&hub)
        .profile(AlphaProfile)
        .resolve()
        .await
        .unwrap();
    facade.put(put("job", b"a")).await.unwrap();
    b_cache.put(put("job", b"b")).await.unwrap();
    for key in [LOCK_KEY, ELECTION_KEY] {
        let Err(ClusterError::InvalidName {
            reason: expected, ..
        }) = validate_cache_key(key)
        else {
            panic!("SDK rejects reserved keys");
        };
        assert!(
            matches!(facade.get(key).await, Err(ClusterError::InvalidName { name, reason }) if name == key && reason == expected)
        );
    }

    let alpha = LeaseToken::new("job", "alpha-owner", 1);
    let beta = LeaseToken::new("job", "beta-owner", 1);
    assert_eq!(
        a_lock
            .acquire("job", "alpha-owner", LOCK_TTL)
            .await
            .unwrap(),
        alpha.clone()
    );
    assert_eq!(
        b_lock.acquire("job", "beta-owner", LOCK_TTL).await.unwrap(),
        beta.clone()
    );
    let timing = ElectionConfig::new(LOCK_TTL, 1).unwrap();
    assert_eq!(
        a_leader.join("job", "alpha-owner", timing).await.unwrap(),
        Some(alpha.clone())
    );
    assert_eq!(
        b_leader.join("job", "beta-owner", timing).await.unwrap(),
        Some(beta.clone())
    );

    for cache in [&a_cache, &b_cache] {
        let owner = if Arc::ptr_eq(cache, &a_cache) {
            "alpha-owner"
        } else {
            "beta-owner"
        };
        for key in [LOCK_KEY, ELECTION_KEY] {
            assert_eq!(
                cache.get(key).await.unwrap(),
                Some(CacheEntry {
                    value: LeaseRecord {
                        owner: owner.to_owned(),
                        fence: 1,
                        deadline_ms: 20,
                        nonce: nonce_seed
                    }
                    .encode(),
                    version: 1,
                }),
            );
        }
    }
    assert!(a_lock.release(&alpha).await.is_ok());
    assert!(b_leader.resign(&beta).await.is_ok());
    assert_eq!(a_cache.get(LOCK_KEY).await.unwrap(), None);
    assert_eq!(b_cache.get(ELECTION_KEY).await.unwrap(), None);
    assert_eq!(
        a_cache.get(ELECTION_KEY).await.unwrap(),
        Some(CacheEntry {
            value: LeaseRecord {
                owner: "alpha-owner".to_owned(),
                fence: 1,
                deadline_ms: 20,
                nonce: nonce_seed
            }
            .encode(),
            version: 1,
        })
    );
    assert_eq!(
        b_cache.get(LOCK_KEY).await.unwrap(),
        Some(CacheEntry {
            value: LeaseRecord {
                owner: "beta-owner".to_owned(),
                fence: 1,
                deadline_ms: 20,
                nonce: nonce_seed
            }
            .encode(),
            version: 1,
        })
    );
    assert_eq!(
        a_cache.get("job").await.unwrap(),
        Some(CacheEntry {
            value: b"a".to_vec(),
            version: 1
        })
    );
    assert_eq!(
        b_cache.get("job").await.unwrap(),
        Some(CacheEntry {
            value: b"b".to_vec(),
            version: 1
        })
    );
    let mut watched = a_leader.elect("metrics").await.unwrap();
    assert!(matches!(
        election_event(&mut watched).await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    assert!(watched.resign().await.is_ok());
    let recorded = metrics
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert!(recorded.contains(&Metric::Cache("put".to_owned(), "ok".to_owned())));
    assert!(recorded.contains(&Metric::Lock("try_lock".to_owned(), "ok".to_owned())));
    assert!(recorded.contains(&Metric::Transition("acquired".to_owned())));
    assert!(recorded.contains(&Metric::Transition("resigned".to_owned())));
    assert_eq!(
        timeout(TEST_DEADLINE, cleanup.finish()).await.unwrap(),
        Ok(())
    );
}

#[tokio::test]
async fn the_memory_client_reports_each_actual_provider_until_cleanup_finishes() {
    let RunningMemory { client, cleanup } = prepare_memory(memory_options(&["alpha", "beta"]))
        .unwrap()
        .start();
    let cache_provider = std::any::type_name::<MemoryCache>();
    let lock_provider = std::any::type_name::<MemoryLock>();
    let leader_provider = std::any::type_name::<MemoryLeader>();
    assert!(client.refresh_descriptors().await.is_ok());
    for name in ["alpha", "beta"] {
        assert_eq!(
            client.cache_backend(name).unwrap().provider_name(),
            cache_provider
        );
        assert_eq!(
            client.lock_backend(name).unwrap().provider_name(),
            lock_provider
        );
        assert_eq!(
            client
                .leader_election_backend(name)
                .unwrap()
                .provider_name(),
            leader_provider
        );
        assert_eq!(
            client.descriptor(name).await.unwrap(),
            ProfileDescriptor {
                name: name.to_owned(),
                cache: CacheDescriptor {
                    consistency: WireCacheConsistency::Linearizable,
                    features: WireCacheFeatures::new(Some(true), true),
                    provider: cache_provider.to_owned(),
                },
                lock: LockDescriptor {
                    features: WireLockFeatures { linearizable: true },
                    provider: lock_provider.to_owned(),
                },
                leader_election: LeaderElectionDescriptor {
                    features: WireLeaderElectionFeatures { linearizable: true },
                    provider: leader_provider.to_owned(),
                },
                health: ProfileHealth::Serving,
            },
        );
    }
    assert_eq!(
        timeout(TEST_DEADLINE, cleanup.finish()).await.unwrap(),
        Ok(())
    );
    assert!(client.refresh_descriptors().await.is_ok());
    for name in ["alpha", "beta"] {
        assert_eq!(
            client.descriptor(name).await.unwrap(),
            ProfileDescriptor {
                name: name.to_owned(),
                cache: CacheDescriptor {
                    consistency: WireCacheConsistency::Linearizable,
                    features: WireCacheFeatures::new(Some(true), true),
                    provider: cache_provider.to_owned(),
                },
                lock: LockDescriptor {
                    features: WireLockFeatures { linearizable: true },
                    provider: lock_provider.to_owned(),
                },
                leader_election: LeaderElectionDescriptor {
                    features: WireLeaderElectionFeatures { linearizable: true },
                    provider: leader_provider.to_owned(),
                },
                health: ProfileHealth::Degraded,
            },
        );
    }
}

#[tokio::test]
async fn a_missing_profile_does_not_intern_caller_names() {
    let RunningMemory { client, cleanup } = prepared_memory(&["alpha"]).start();
    let [cache, lock, election, descriptor] = [
        "test-missing-9c26-cache",
        "test-missing-9c26-lock",
        "test-missing-9c26-election",
        "test-missing-9c26-descriptor",
    ];
    for name in [cache, lock, election, descriptor] {
        assert_eq!(intern_existing(name), None);
    }
    assert!(matches!(
        client.cache_backend(cache),
        Err(ClusterError::ProfileNotBound {
            profile: "<unknown>"
        })
    ));
    assert!(matches!(
        client.lock_backend(lock),
        Err(ClusterError::ProfileNotBound {
            profile: "<unknown>"
        })
    ));
    assert!(matches!(
        client.leader_election_backend(election),
        Err(ClusterError::ProfileNotBound {
            profile: "<unknown>"
        })
    ));
    assert!(matches!(
        client.descriptor(descriptor).await,
        Err(ClusterError::ProfileNotBound {
            profile: "<unknown>"
        })
    ));
    for name in [cache, lock, election, descriptor] {
        assert_eq!(intern_existing(name), None);
    }
    let before = client.descriptor("alpha").await.unwrap();
    assert!(client.refresh_descriptors().await.is_ok());
    assert_eq!(client.descriptor("alpha").await.unwrap(), before);
    assert_eq!(
        timeout(TEST_DEADLINE, cleanup.finish()).await.unwrap(),
        Ok(())
    );
}

#[tokio::test(start_paused = true)]
async fn a_token_and_watch_contend_for_one_election_record() {
    let nonce_seed = PreparedMemory::test_nonce_seed();
    let mut tracked = Vec::new();
    let prepared = PreparedMemory::prepare_for_test(memory_options(&["alpha"]), |_, cache| {
        let recorded = Arc::new(RecordingCache::new(cache));
        tracked.push(recorded.clone());
        recorded
    })
    .unwrap();
    let [recorded] = tracked.as_slice() else {
        panic!("one prepared profile")
    };
    let RunningMemory { client, cleanup } = prepared.start();
    let election = client.leader_election_backend("alpha").unwrap();
    let config = ElectionConfig::new(LOCK_TTL, 1).unwrap();
    let token = LeaseToken::new("job", "worker-a", 1);
    assert_eq!(
        election.join("job", "worker-a", config).await.unwrap(),
        Some(token.clone())
    );
    assert_eq!(
        election.join("job", "worker-b", config).await.unwrap(),
        None
    );
    recorded
        .calls
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
    let mut follow = election.elect_with_config("job", config).await.unwrap();
    assert!(matches!(
        election_event(&mut follow).await,
        LeaderWatchEvent::Status(LeaderStatus::Follower)
    ));
    assert_eq!(
        *recorded
            .calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
        [
            ClaimCall::WatchRegistered(ELECTION_KEY.to_owned()),
            ClaimCall::PutIfAbsent(ELECTION_KEY.to_owned()),
            ClaimCall::Read(ELECTION_KEY.to_owned()),
        ]
    );

    advance(Duration::from_millis(5)).await;
    let long_renewal = Duration::from_millis(60);
    assert!(election.renew(&token, long_renewal).await.is_ok());
    assert!(matches!(
        recorded.inner.get(ELECTION_KEY).await,
        Ok(Some(CacheEntry { value, version: 2 }))
            if LeaseRecord::decode(&value) == Some(LeaseRecord {
                owner: "worker-a".to_owned(), fence: 1, deadline_ms: 65, nonce: nonce_seed.wrapping_add(3),
            })
    ));
    for stale in [
        LeaseToken::new("job", "worker-b", 1),
        LeaseToken::new("job", "worker-a", 2),
    ] {
        assert!(
            matches!(election.renew(&stale, LOCK_TTL).await, Err(ClusterError::LockExpired { name }) if name == "job")
        );
        assert!(election.resign(&stale).await.is_ok());
        assert_eq!(
            election.join("job", "worker-b", config).await.unwrap(),
            None
        );
    }
    assert!(election.resign(&token).await.is_ok());
    assert!(matches!(
        election_event(&mut follow).await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    let mut next = election.elect_with_config("job", config).await.unwrap();
    assert!(matches!(
        election_event(&mut next).await,
        LeaderWatchEvent::Status(LeaderStatus::Follower)
    ));
    drop(follow);
    assert!(matches!(
        election_event(&mut next).await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    let after_drop = recorded.inner.get(ELECTION_KEY).await.unwrap().unwrap();
    let owner = LeaseRecord::decode(&after_drop.value).unwrap().owner;
    assert_eq!(
        after_drop,
        CacheEntry {
            value: LeaseRecord {
                owner,
                fence: 1,
                deadline_ms: 25,
                nonce: nonce_seed.wrapping_add(8)
            }
            .encode(),
            version: 1,
        }
    );
    assert!(next.resign().await.is_ok());
    assert_eq!(
        election.join("job", "worker-a", config).await.unwrap(),
        Some(token.clone())
    );
    advance(LOCK_TTL).await;
    assert!(
        matches!(election.renew(&token, LOCK_TTL).await, Err(ClusterError::LockExpired { name }) if name == "job")
    );
    assert_eq!(
        election.join("job", "worker-a", config).await.unwrap(),
        Some(LeaseToken::new("job", "worker-a", 2))
    );
    assert!(election.resign(&token).await.is_ok());
    assert_eq!(
        election.join("job", "worker-b", config).await.unwrap(),
        None
    );

    let maximum = ElectionConfig::new(Duration::MAX, 1).unwrap();
    let mut large = election
        .elect_with_config("maximum", maximum)
        .await
        .unwrap();
    assert!(matches!(
        election_event(&mut large).await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    let mut large_follower = election
        .elect_with_config("maximum", maximum)
        .await
        .unwrap();
    assert!(matches!(
        election_event(&mut large_follower).await,
        LeaderWatchEvent::Status(LeaderStatus::Follower)
    ));
    assert_eq!(
        timeout(TEST_DEADLINE, cleanup.finish()).await.unwrap(),
        Ok(())
    );
    election_renews_through_watch_traffic_and_feed_loss().await;
}

#[expect(
    clippy::unwrap_used,
    clippy::cognitive_complexity,
    reason = "the election integration case checks stored state across watch traffic and feed loss"
)]
async fn election_renews_through_watch_traffic_and_feed_loss() {
    let nonce_seed = PreparedMemory::test_nonce_seed();
    let mut delegates = Vec::new();
    let prepared = PreparedMemory::prepare_for_test(memory_options(&["alpha"]), |_, cache| {
        let recorded = Arc::new(RecordingCache {
            watch_mode: WatchMode::Recorded,
            ..RecordingCache::new(cache)
        });
        delegates.push(recorded.clone());
        recorded
    })
    .unwrap();
    let [recorded] = delegates.as_slice() else {
        panic!("one profile")
    };
    let RunningMemory { client, cleanup } = prepared.start();
    let election = client.leader_election_backend("alpha").unwrap();
    let mut watch = election
        .elect_with_config("job", ElectionConfig::new(LOCK_TTL, 1).unwrap())
        .await
        .unwrap();
    assert!(matches!(
        election_event(&mut watch).await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    let initial = recorded.inner.get(ELECTION_KEY).await.unwrap().unwrap();
    let owner = LeaseRecord::decode(&initial.value).unwrap().owner;
    let sender = recorded.election_feed();
    for _ in 0..2 {
        advance(Duration::from_millis(4)).await;
        timeout(TEST_DEADLINE, sender.send(CacheWatchEvent::Reset))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            election_event(&mut watch).await,
            LeaderWatchEvent::Reset
        ));
    }
    assert_eq!(
        recorded.inner.get(ELECTION_KEY).await.unwrap(),
        Some(CacheEntry {
            value: LeaseRecord {
                owner: owner.clone(),
                fence: 1,
                deadline_ms: 20,
                nonce: nonce_seed
            }
            .encode(),
            version: 1,
        })
    );

    // At 11 ms the original 10 ms tick is ready. Resetting it on the feed's
    // last event at 8 ms would move it to 18 ms.
    advance(Duration::from_millis(3)).await;
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        recorded.inner.get(ELECTION_KEY).await.unwrap(),
        Some(CacheEntry {
            value: LeaseRecord {
                owner: owner.clone(),
                fence: 1,
                deadline_ms: 31,
                nonce: nonce_seed.wrapping_add(1)
            }
            .encode(),
            version: 2,
        })
    );

    let feed_error = ClusterError::Provider {
        kind: cluster_sdk::error::ProviderErrorKind::ConnectionLost,
        message: "fixture closed the election feed".to_owned(),
    };
    for expected in 2..=4 {
        let feed = recorded.election_feed();
        timeout(
            TEST_DEADLINE,
            feed.send(CacheWatchEvent::Closed(feed_error.clone())),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(
            election_event(&mut watch).await,
            LeaderWatchEvent::Reset
        ));
        assert_eq!(recorded.watch_calls.load(Ordering::Relaxed), expected);
    }
    let feed = recorded.election_feed();
    timeout(
        TEST_DEADLINE,
        feed.send(CacheWatchEvent::Closed(feed_error)),
    )
    .await
    .unwrap()
    .unwrap();
    wait_for_ready_turns(|| feed.is_closed()).await;
    assert_eq!(recorded.watch_calls.load(Ordering::Relaxed), 4);
    advance(Duration::from_millis(11)).await;
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        recorded.inner.get(ELECTION_KEY).await.unwrap(),
        Some(CacheEntry {
            value: LeaseRecord {
                owner,
                fence: 1,
                deadline_ms: 42,
                nonce: nonce_seed.wrapping_add(2)
            }
            .encode(),
            version: 3,
        })
    );
    assert_eq!(watch.status(), LeaderStatus::Leader);
    assert_eq!(
        timeout(TEST_DEADLINE, cleanup.finish()).await.unwrap(),
        Ok(())
    );
}

impl RecordingCache {
    fn election_feed(&self) -> CacheWatchSender {
        let recorded = self.watch.lock().unwrap_or_else(PoisonError::into_inner);
        let Some((key, sender)) = recorded.as_ref() else {
            panic!("election feed subscribed");
        };
        assert_eq!(key, ELECTION_KEY);
        sender.clone()
    }
}

#[test]
fn a_failed_last_profile_leaves_no_started_workers() {
    let mut built = Vec::new();
    let result = PreparedMemory::prepare_for_test(
        memory_options(&["alpha", "beta", "alpha"]),
        |name, cache| {
            built.push((name.to_owned(), cache.clone()));
            cache
        },
    );
    assert!(matches!(
        result,
        Err(OopCompositionError::InvalidConfig {
            field: "event-broker.process.cluster.profiles"
        })
    ));
    assert_eq!(
        built
            .iter()
            .map(|(name, cache)| (name.as_str(), cache.test_sweep_counts()))
            .collect::<Vec<_>>(),
        [("alpha", (0, 0)), ("beta", (0, 0))]
    );
    let accepted = prepare_memory(memory_options(&["alpha", "beta"])).unwrap();
    let alpha = accepted.test_profile("alpha").cache;
    let beta = accepted.test_profile("beta").cache;
    assert_eq!(alpha.test_sweep_counts(), (0, 0));
    assert_eq!(beta.test_sweep_counts(), (0, 0));
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(async {
            let RunningMemory { client, cleanup } = accepted.start();
            drop(client);
            assert_eq!(alpha.test_sweep_counts(), (1, 1));
            assert_eq!(beta.test_sweep_counts(), (1, 1));
            assert_eq!(
                timeout(TEST_DEADLINE, cleanup.finish()).await.unwrap(),
                Ok(())
            );
            assert_eq!(alpha.test_sweep_counts(), (1, 0));
            assert_eq!(beta.test_sweep_counts(), (1, 0));
        });
}

#[tokio::test(start_paused = true)]
async fn orderly_memory_cleanup_revokes_all_profiles_and_joins_every_started_worker() {
    let mut delegates = Vec::new();
    let prepared =
        PreparedMemory::prepare_for_test(memory_options(&["alpha", "beta"]), |name, cache| {
            let recorded = Arc::new(RecordingCache {
                watch_mode: if name == "beta" {
                    WatchMode::Recorded
                } else {
                    WatchMode::Native
                },
                ..RecordingCache::new(cache)
            });
            delegates.push(recorded.clone());
            recorded
        })
        .unwrap();
    let [alpha_cache, beta_cache] = delegates.as_slice() else {
        panic!("two prepared profiles")
    };
    let counts = prepared.test_profile("alpha");
    let beta_counts = prepared.test_profile("beta");
    let stages = prepared.test_cleanup_stages();
    let terminal = Arc::new(TerminalGate::default());
    beta_counts.leader.set_test_terminal_gate(terminal.clone());
    let RunningMemory { client, cleanup } = prepared.start();
    let alpha = client.leader_election_backend("alpha").unwrap();
    let beta = client.leader_election_backend("beta").unwrap();
    let mut alpha_watch = alpha.elect("job").await.unwrap();
    let mut follower = alpha.elect("job").await.unwrap();
    let mut beta_watch = beta.elect("job").await.unwrap();
    assert!(matches!(
        election_event(&mut alpha_watch).await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    assert!(matches!(
        election_event(&mut follower).await,
        LeaderWatchEvent::Status(LeaderStatus::Follower)
    ));
    assert!(beta_watch.is_leader());
    let mut a_cache_watch = client
        .cache_backend("alpha")
        .unwrap()
        .watch("ordinary")
        .await
        .unwrap();
    let mut b_cache_watch = client
        .cache_backend("beta")
        .unwrap()
        .watch_prefix("ordinary")
        .await
        .unwrap();
    let lock = client.lock_backend("alpha").unwrap();
    let guard = lock.try_lock("held", LEASE_TTL).await.unwrap();
    assert_eq!(counts.lock.registered_guards(), 1);
    let mut waiter = Box::pin(lock.lock("held", LEASE_TTL, VIRTUAL_BUDGET));
    poll_pending(waiter.as_mut()).await;
    assert_eq!(counts.leader.test_worker_counts(), (2, 2, 0));
    assert_eq!(beta_counts.leader.test_worker_counts(), (1, 1, 0));
    assert_eq!(counts.cache.test_sweep_counts(), (1, 1));
    assert_eq!(beta_counts.cache.test_sweep_counts(), (1, 1));

    let sender = beta_cache.election_feed();
    let reads_before = beta_cache.operations.load(Ordering::Relaxed);
    for _ in 0..17 {
        timeout(TEST_DEADLINE, sender.send(CacheWatchEvent::Reset))
            .await
            .unwrap()
            .unwrap();
    }
    wait_for_ready_turns(|| beta_cache.operations.load(Ordering::Relaxed) >= reads_before + 17)
        .await;
    assert_eq!(
        beta_cache.operations.load(Ordering::Relaxed),
        reads_before + 17
    );
    let alpha_row = alpha_cache.inner.get(ELECTION_KEY).await.unwrap().unwrap();
    let beta_row = beta_cache.inner.get(ELECTION_KEY).await.unwrap().unwrap();
    let mut finish = Box::pin(cleanup.finish());
    poll_pending(finish.as_mut()).await;
    timeout(TEST_DEADLINE, terminal.sent.notified())
        .await
        .unwrap();
    assert_eq!(beta_counts.leader.test_worker_counts(), (1, 1, 0));
    assert_eq!(counts.lock.registered_guards(), 1);
    assert_eq!(beta_counts.cache.test_sweep_counts(), (1, 1));
    assert!(matches!(
        election_event(&mut beta_watch).await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    for _ in 0..15 {
        assert!(matches!(
            election_event(&mut beta_watch).await,
            LeaderWatchEvent::Reset
        ));
    }
    assert!(matches!(
        election_event(&mut beta_watch).await,
        LeaderWatchEvent::Status(LeaderStatus::Lost)
    ));
    assert!(matches!(
        election_event(&mut beta_watch).await,
        LeaderWatchEvent::Closed(ClusterError::Shutdown)
    ));
    terminal.resume.notify_one();
    assert_eq!(timeout(TEST_DEADLINE, finish).await.unwrap(), Ok(()));
    assert!(matches!(
        election_event(&mut alpha_watch).await,
        LeaderWatchEvent::Status(LeaderStatus::Lost)
    ));
    assert!(matches!(
        election_event(&mut alpha_watch).await,
        LeaderWatchEvent::Closed(ClusterError::Shutdown)
    ));
    assert!(matches!(
        election_event(&mut follower).await,
        LeaderWatchEvent::Closed(ClusterError::Shutdown)
    ));
    assert!(matches!(
        timeout(TEST_DEADLINE, waiter).await.unwrap(),
        Err(ClusterError::Shutdown)
    ));
    assert!(matches!(
        guard.renew(LEASE_TTL).await,
        Err(ClusterError::LockExpired { name }) if name == "held"
    ));
    for watch in [&mut a_cache_watch, &mut b_cache_watch] {
        assert!(matches!(
            timeout(TEST_DEADLINE, watch.recv()).await.unwrap(),
            Some(CacheWatchEvent::Closed(ClusterError::Shutdown))
        ));
    }
    assert_eq!(
        alpha_cache.inner.get(ELECTION_KEY).await.unwrap(),
        Some(alpha_row)
    );
    assert_eq!(
        beta_cache.inner.get(ELECTION_KEY).await.unwrap(),
        Some(beta_row)
    );
    assert_eq!(counts.leader.test_worker_counts(), (2, 0, 2));
    assert_eq!(beta_counts.leader.test_worker_counts(), (1, 0, 1));
    assert_eq!(counts.lock.registered_guards(), 0);
    assert_eq!(counts.cache.test_sweep_counts(), (1, 0));
    assert_eq!(beta_counts.cache.test_sweep_counts(), (1, 0));
    assert_eq!(
        *stages.lock().unwrap_or_else(PoisonError::into_inner),
        [
            ("alpha".to_owned(), "leaders"),
            ("beta".to_owned(), "leaders"),
            ("alpha".to_owned(), "locks"),
            ("beta".to_owned(), "locks"),
            ("alpha".to_owned(), "cache"),
            ("beta".to_owned(), "cache"),
            ("alpha".to_owned(), "sweep"),
            ("beta".to_owned(), "sweep"),
        ]
    );
    assert!(matches!(
        alpha.elect("late").await,
        Err(ClusterError::Shutdown)
    ));
    assert!(matches!(
        beta.join("late", "owner", ElectionConfig::default()).await,
        Err(ClusterError::Shutdown)
    ));
    assert!(matches!(
        lock.try_lock("late", LOCK_TTL).await,
        Err(ClusterError::Shutdown)
    ));
    assert!(client.refresh_descriptors().await.is_ok());
    assert_eq!(
        client.descriptor("alpha").await.unwrap().health,
        ProfileHealth::Degraded
    );

    a_late_lock_claim_observes_cache_closure().await;
}

#[expect(
    clippy::cognitive_complexity,
    clippy::unwrap_used,
    reason = "one named integration case checks the complete late-claim lifecycle and cache visibility"
)]
async fn a_late_lock_claim_observes_cache_closure() {
    let gate = Arc::new(ClaimGate::default());
    let prepared = PreparedMemory::prepare_for_test(memory_options(&["alpha"]), |_, cache| {
        Arc::new(RecordingCache {
            claim_gate: Some(gate.clone()),
            ..RecordingCache::new(cache)
        })
    })
    .unwrap();
    let observer = prepared.test_profile("alpha");
    let RunningMemory { client, cleanup } = prepared.start();
    let lock = client.lock_backend("alpha").unwrap();
    let mut late = Box::pin(lock.try_lock("job", LOCK_TTL));
    poll_pending(late.as_mut()).await;
    timeout(TEST_DEADLINE, gate.written.notified())
        .await
        .unwrap();
    let entry = observer.cache.get(LOCK_KEY).await.unwrap().unwrap();
    let owner = LeaseRecord::decode(&entry.value).unwrap().owner;
    assert_eq!(
        entry,
        CacheEntry {
            value: LeaseRecord {
                owner,
                fence: 1,
                deadline_ms: 20,
                nonce: PreparedMemory::test_nonce_seed()
            }
            .encode(),
            version: 1,
        }
    );
    assert_eq!(observer.lock.registered_guards(), 0);
    assert_eq!(
        timeout(TEST_DEADLINE, cleanup.finish()).await.unwrap(),
        Ok(())
    );
    assert!(matches!(
        lock.try_lock("later", LOCK_TTL).await,
        Err(ClusterError::Shutdown)
    ));
    assert!(matches!(
        observer.cache.put(put("after-close", b"closed")).await,
        Err(ClusterError::Shutdown)
    ));
    gate.resume.notify_one();
    assert!(matches!(
        timeout(TEST_DEADLINE, late).await.unwrap(),
        Err(ClusterError::Shutdown)
    ));
    assert_eq!(observer.lock.registered_guards(), 0);
    assert_eq!(observer.cache.test_sweep_counts(), (1, 0));
    assert!(client.refresh_descriptors().await.is_ok());
    assert_eq!(
        client.descriptor("alpha").await.unwrap().health,
        ProfileHealth::Degraded
    );
    advance(LOCK_TTL).await;
    assert_eq!(
        observer.cache.get(LOCK_KEY).await.unwrap(),
        Some(entry.clone())
    );
    advance(RETENTION.checked_sub(Duration::from_millis(1)).unwrap()).await;
    assert_eq!(observer.cache.get(LOCK_KEY).await.unwrap(), Some(entry));
    advance(Duration::from_millis(1)).await;
    assert_eq!(observer.cache.get(LOCK_KEY).await.unwrap(), None);
}

#[tokio::test(start_paused = true)]
async fn dropping_memory_cleanup_cancels_workers_before_or_during_finish() {
    let prepared = prepared_memory(&["alpha"]);
    let counters = prepared.test_profile("alpha");
    let abort = prepared.test_abort();
    let RunningMemory { client, cleanup } = prepared.start();
    let election = client.leader_election_backend("alpha").unwrap();
    let lock = client.lock_backend("alpha").unwrap();
    let guard = lock.try_lock("job", LOCK_TTL).await.unwrap();
    let mut watch = election.elect("job").await.unwrap();
    assert!(matches!(
        election_event(&mut watch).await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    assert!(!abort.is_cancelled());
    drop(cleanup);
    assert!(abort.is_cancelled());
    assert!(matches!(
        election_event(&mut watch).await,
        LeaderWatchEvent::Status(LeaderStatus::Lost)
    ));
    assert!(matches!(
        election_event(&mut watch).await,
        LeaderWatchEvent::Closed(ClusterError::Shutdown)
    ));
    let renew = timeout(TEST_DEADLINE, guard.renew(LOCK_TTL)).await.unwrap();
    match renew {
        Err(ClusterError::LockExpired { name }) => assert_eq!(name, "job"),
        Err(ClusterError::Provider {
            kind: cluster_sdk::error::ProviderErrorKind::ConnectionLost,
            message,
        }) => {
            assert_eq!(
                message,
                "distributed-lock backend dropped the renew acknowledgement without responding; the TTL renewal was not confirmed"
            );
            assert!(matches!(
                timeout(TEST_DEADLINE, guard.renew(LOCK_TTL)).await.unwrap(),
                Err(ClusterError::LockExpired { name }) if name == "job"
            ));
        }
        other => panic!("lock guard must terminate after abort: {other:?}"),
    }
    assert!(matches!(
        election.elect("late").await,
        Err(ClusterError::Shutdown)
    ));
    assert!(matches!(
        lock.try_lock("late", LOCK_TTL).await,
        Err(ClusterError::Shutdown)
    ));
    wait_for_ready_turns(|| {
        counters.cache.test_sweep_counts().1 == 0 && counters.leader.test_worker_counts().1 == 0
    })
    .await;
    assert_eq!(counters.leader.test_worker_counts(), (1, 0, 0));
    assert_eq!(
        client.descriptor("alpha").await.unwrap().health,
        ProfileHealth::Degraded
    );

    let gate = Arc::new(ClaimGate::default());
    let prepared = PreparedMemory::prepare_for_test(memory_options(&["alpha"]), |_, cache| {
        Arc::new(RecordingCache {
            claim_gate: Some(gate.clone()),
            ..RecordingCache::new(cache)
        })
    })
    .unwrap();
    let counters = prepared.test_profile("alpha");
    let abort = prepared.test_abort();
    let RunningMemory { client, cleanup } = prepared.start();
    let election = client.leader_election_backend("alpha").unwrap();
    let mut enrolling =
        Box::pin(election.elect_with_config("job", ElectionConfig::new(LOCK_TTL, 1).unwrap()));
    poll_pending(enrolling.as_mut()).await;
    timeout(TEST_DEADLINE, gate.written.notified())
        .await
        .unwrap();
    assert_eq!(counters.leader.test_worker_counts(), (1, 1, 0));
    let mut finish = Box::pin(cleanup.finish());
    poll_pending(finish.as_mut()).await;
    assert!(!abort.is_cancelled());
    assert!(matches!(
        election.elect("late").await,
        Err(ClusterError::Shutdown)
    ));
    drop(finish);
    assert!(abort.is_cancelled());
    assert!(matches!(
        timeout(TEST_DEADLINE, enrolling).await.unwrap(),
        Err(ClusterError::Shutdown)
    ));
    wait_for_ready_turns(|| {
        counters.cache.test_sweep_counts().1 == 0 && counters.leader.test_worker_counts().1 == 0
    })
    .await;
    assert_eq!(counters.leader.test_worker_counts(), (1, 0, 0));
    let written = counters.cache.get(ELECTION_KEY).await.unwrap();
    assert!(written.is_some());
    advance(LOCK_TTL).await;
    assert_eq!(counters.cache.get(ELECTION_KEY).await.unwrap(), written);

    a_ready_election_refuses_publication_after_close().await;
    abort_before_the_election_worker_first_runs().await;
    an_in_flight_election_finishes_before_orderly_cleanup().await;

    let gate = Arc::new(ClaimGate::default());
    let prepared = PreparedMemory::prepare_for_test(memory_options(&["alpha"]), |_, cache| {
        Arc::new(RecordingCache {
            claim_gate: Some(gate.clone()),
            ..RecordingCache::new(cache)
        })
    })
    .unwrap();
    let counters = prepared.test_profile("alpha");
    let RunningMemory { client, cleanup } = prepared.start();
    let election = client.leader_election_backend("alpha").unwrap();
    let mut abandoned = Box::pin(election.elect("job"));
    poll_pending(abandoned.as_mut()).await;
    timeout(TEST_DEADLINE, gate.written.notified())
        .await
        .unwrap();
    assert_eq!(counters.leader.test_worker_counts(), (1, 1, 0));
    drop(abandoned);
    gate.resume.notify_one();
    wait_for_ready_turns(|| counters.leader.test_worker_counts().1 == 0).await;
    assert_eq!(counters.cache.get(ELECTION_KEY).await.unwrap(), None);
    assert_eq!(
        timeout(TEST_DEADLINE, cleanup.finish()).await.unwrap(),
        Ok(())
    );
    assert_eq!(counters.leader.test_worker_counts(), (1, 0, 1));
}

#[expect(
    clippy::unwrap_used,
    reason = "the cleanup integration case expects an admitted worker to terminate"
)]
async fn abort_before_the_election_worker_first_runs() {
    let prepared = prepared_memory(&["alpha"]);
    let counters = prepared.test_profile("alpha");
    let RunningMemory { client, cleanup } = prepared.start();
    let election = client.leader_election_backend("alpha").unwrap();
    let mut enrolment = Box::pin(election.elect("job"));
    poll_pending(enrolment.as_mut()).await;
    assert_eq!(counters.leader.test_worker_counts(), (1, 1, 0));
    drop(cleanup);
    assert!(matches!(
        timeout(TEST_DEADLINE, enrolment).await.unwrap(),
        Err(ClusterError::Shutdown)
    ));
    assert_eq!(counters.leader.test_worker_counts(), (1, 0, 0));
    assert_eq!(counters.cache.get(ELECTION_KEY).await.unwrap(), None);
}

#[expect(
    clippy::unwrap_used,
    reason = "the cleanup integration case drives the real admission barrier"
)]
async fn an_in_flight_election_finishes_before_orderly_cleanup() {
    let gate = Arc::new(ClaimGate::default());
    let metrics = Arc::new(MemoryMetrics::default());
    let options = EbMemoryOptions {
        metrics: metrics.clone(),
        ..memory_options(&["alpha"])
    };
    let prepared = PreparedMemory::prepare_for_test(options, |_, cache| {
        Arc::new(RecordingCache {
            claim_gate: Some(gate.clone()),
            ..RecordingCache::new(cache)
        })
    })
    .unwrap();
    let counters = prepared.test_profile("alpha");
    let RunningMemory { client, cleanup } = prepared.start();
    let election = client.leader_election_backend("alpha").unwrap();
    let mut enrolling =
        Box::pin(election.elect_with_config("job", ElectionConfig::new(LOCK_TTL, 1).unwrap()));
    poll_pending(enrolling.as_mut()).await;
    timeout(TEST_DEADLINE, gate.written.notified())
        .await
        .unwrap();
    let before = counters.cache.get(ELECTION_KEY).await.unwrap().unwrap();
    let mut finish = Box::pin(cleanup.finish());
    poll_pending(finish.as_mut()).await;
    assert!(matches!(
        election.elect("late").await,
        Err(ClusterError::Shutdown)
    ));
    assert_eq!(counters.leader.test_worker_counts(), (1, 1, 0));
    gate.resume.notify_one();
    assert!(matches!(
        timeout(TEST_DEADLINE, enrolling).await.unwrap(),
        Err(ClusterError::Shutdown)
    ));
    assert_eq!(timeout(TEST_DEADLINE, finish).await.unwrap(), Ok(()));
    assert_eq!(counters.leader.test_worker_counts(), (1, 0, 1));
    assert_eq!(
        counters.cache.get(ELECTION_KEY).await.unwrap(),
        Some(before)
    );
    let metrics = metrics.0.lock().unwrap_or_else(PoisonError::into_inner);
    assert!(!metrics.contains(&Metric::Transition("acquired".to_owned())));
}

#[expect(
    clippy::unwrap_used,
    reason = "the cleanup integration case expects the real worker and completion to start"
)]
async fn a_ready_election_refuses_publication_after_close() {
    let metrics = Arc::new(MemoryMetrics::default());
    let prepared = PreparedMemory::prepare_for_test(
        EbMemoryOptions {
            metrics: metrics.clone(),
            ..memory_options(&["alpha"])
        },
        |_, cache| cache,
    )
    .unwrap();
    let counters = prepared.test_profile("alpha");
    let RunningMemory { client, cleanup } = prepared.start();
    let election = client.leader_election_backend("alpha").unwrap();
    let mut enrolling = Box::pin(election.elect("job"));
    poll_pending(enrolling.as_mut()).await;
    wait_for_ready_turns(|| {
        metrics
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&Metric::Transition("acquired".to_owned()))
    })
    .await;
    assert_eq!(
        timeout(TEST_DEADLINE, cleanup.finish()).await.unwrap(),
        Ok(())
    );
    assert!(matches!(
        timeout(TEST_DEADLINE, enrolling).await.unwrap(),
        Err(ClusterError::Shutdown)
    ));
    assert_eq!(counters.leader.test_worker_counts(), (1, 0, 1));
}

async fn wait_for_ready_turns(ready: impl Fn() -> bool) {
    for _ in 0..256 {
        if ready() {
            return;
        }
        tokio::task::yield_now().await;
    }
    assert!(ready(), "condition still pending after 256 ready turns");
}

#[tokio::test(start_paused = true)]
async fn a_watch_loses_leadership_at_the_monotonic_deadline_even_if_wall_time_lags() {
    let ttl = Duration::from_secs(60);
    let config = ElectionConfig::new(ttl, 1).unwrap();
    for renew_first in [false, true] {
        let RunningMemory { client, cleanup } =
            prepare_memory(memory_options(&["alpha"])).unwrap().start();
        let cache = client.cache_backend("alpha").unwrap();
        let election = client.leader_election_backend("alpha").unwrap();
        let mut watch = election.elect_with_config("job", config).await.unwrap();
        assert!(matches!(
            election_event(&mut watch).await,
            LeaderWatchEvent::Status(LeaderStatus::Leader)
        ));
        let first = cache.get(ELECTION_KEY).await.unwrap().unwrap();
        assert_eq!(first.version, 1);
        if renew_first {
            advance(ttl.saturating_sub(Duration::from_millis(1))).await;
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            assert_eq!(watch.status(), LeaderStatus::Leader);
            advance(config.renewal_interval()).await;
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
        }
        let last = cache.get(ELECTION_KEY).await.unwrap().unwrap();
        assert_eq!(last.version, if renew_first { 3 } else { 1 });
        advance(ttl).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(matches!(
            timeout(Duration::ZERO, watch.changed()).await,
            Ok(LeaderWatchEvent::Status(LeaderStatus::Lost))
        ));
        assert!(matches!(
            election_event(&mut watch).await,
            LeaderWatchEvent::Status(LeaderStatus::Follower)
        ));
        assert_eq!(cache.get(ELECTION_KEY).await.unwrap(), Some(last));
        assert_eq!(cleanup.finish().await, Ok(()));
    }
}

#[tokio::test(start_paused = true)]
async fn a_positive_fractional_election_ttl_expires_at_its_requested_instant() {
    assert!(
        matches!(ElectionConfig::new(Duration::ZERO,1), Err(ClusterError::InvalidConfig {reason}) if reason == "ttl and max_missed_renewals must both be > 0 (got ttl=0ns, max_missed_renewals=1)")
    );
    for (nanos, interval) in [(999_999, 499_999), (1_000_000, 500_000)] {
        let ttl = Duration::from_nanos(nanos);
        let config = ElectionConfig::new(ttl, 1).unwrap();
        assert_eq!(config.renewal_interval(), Duration::from_nanos(interval));
        for offset in [0, 100] {
            for renew in [false, true] {
                let RunningMemory { client, cleanup } = prepared_memory(&["alpha"]).start();
                let election = client.leader_election_backend("alpha").unwrap();
                advance(Duration::from_nanos(offset)).await;
                let token = LeaseToken::new("job", "first", 1);
                let grant = election
                    .join("job", "first", config)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(grant.deadline, None);
                assert_eq!(grant, token);
                assert_eq!(election.join("job", "next", config).await.unwrap(), None);
                if renew {
                    advance(Duration::from_nanos(201)).await;
                    election.renew(&token, ttl).await.unwrap();
                }
                advance(ttl.saturating_sub(Duration::from_nanos(1))).await;
                assert_eq!(election.join("job", "next", config).await.unwrap(), None);
                advance(Duration::from_nanos(1)).await;
                assert_eq!(
                    election.join("job", "next", config).await.unwrap(),
                    Some(LeaseToken::new("job", "next", 2))
                );
                assert_eq!(cleanup.finish().await, Ok(()));
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_direct_election_join_closed_before_acknowledgment_returns_shutdown() {
    let gate = Arc::new(ClaimGate::default());
    let prepared = PreparedMemory::prepare_for_test(memory_options(&["alpha"]), |_, cache| {
        Arc::new(RecordingCache {
            claim_gate: Some(gate.clone()),
            ..RecordingCache::new(cache)
        })
    })
    .unwrap();
    let abort = prepared.test_abort();
    let raw = prepared.test_profile("alpha").cache;
    let RunningMemory { client, cleanup } = prepared.start();
    let election = client.leader_election_backend("alpha").unwrap();
    let mut joining = Box::pin(election.join("job", "first", ElectionConfig::default()));
    poll_pending(joining.as_mut()).await;
    timeout(TEST_DEADLINE, gate.written.notified())
        .await
        .unwrap();
    assert!(raw.contains(ELECTION_KEY).await.unwrap());
    abort.cancel();
    gate.resume.notify_one();
    assert!(matches!(
        timeout(TEST_DEADLINE, joining).await.unwrap(),
        Err(ClusterError::Shutdown)
    ));
    assert_eq!(raw.get(ELECTION_KEY).await.unwrap(), None);
    assert_eq!(cleanup.finish().await, Ok(()));
}

#[test]
fn memory_preparation_enforces_profile_count_name_interval_and_consistency() {
    let weak = Arc::new(RecordingCache {
        consistency: CacheConsistency::EventuallyConsistent,
        ..RecordingCache::new(MemoryCache::new())
    });
    assert!(
        matches!(MemoryLeader::new(reserved_lease_cache(weak),Arc::new(NoopMetrics),tokio_util::sync::CancellationToken::new()),Err(ClusterError::InvalidConfig{reason}) if reason=="event-broker memory election requires a linearizable cache")
    );
    for count in [0, 1, 64, 65] {
        let names: Vec<_> = (0..count).map(|n| format!("profile-{n}")).collect();
        let result = prepare_memory(EbMemoryOptions {
            profiles: names,
            ..memory_options(&[])
        });
        if matches!(count, 1 | 64) {
            assert!(result.is_ok());
        } else {
            assert!(matches!(
                result,
                Err(OopCompositionError::InvalidConfig {
                    field: "event-broker.process.cluster.profiles"
                })
            ));
        }
    }
    assert!(matches!(
        prepare_memory(memory_options(&["bad/name"])),
        Err(OopCompositionError::InvalidConfig {
            field: "event-broker.process.cluster.profiles"
        })
    ));
    assert!(matches!(
        prepare_memory(EbMemoryOptions {
            sweep_interval: Duration::ZERO,
            ..memory_options(&["alpha"])
        }),
        Err(OopCompositionError::InvalidConfig {
            field: "event-broker.process.cluster.sweep_interval"
        })
    ));
    assert!(matches!(
        PreparedMemory::prepare_for_test(memory_options(&["alpha"]), |_, cache| Arc::new(
            RecordingCache {
                consistency: CacheConsistency::EventuallyConsistent,
                ..RecordingCache::new(cache)
            }
        )),
        Err(OopCompositionError::ClientConstruction {
            component: "event-broker memory lock"
        })
    ));
}

fn recorded_election(mode: WatchMode) -> (RunningMemory, Arc<RecordingCache>) {
    let mut backend = None;
    let prepared = PreparedMemory::prepare_for_test(memory_options(&["alpha"]), |_, cache| {
        let recorded = Arc::new(RecordingCache {
            watch_mode: mode,
            ..RecordingCache::new(cache)
        });
        backend = Some(recorded.clone());
        recorded
    })
    .unwrap_or_else(|error| panic!("profile preparation failed: {error}"));
    (
        prepared.start(),
        backend.unwrap_or_else(|| panic!("one prepared profile")),
    )
}

#[tokio::test(start_paused = true)]
async fn retryable_election_renewals_lose_only_after_the_miss_budget_is_exceeded() {
    let (RunningMemory { client, cleanup }, recorded) = recorded_election(WatchMode::Recorded);
    let election = client.leader_election_backend("alpha").unwrap();
    // The two 5 ms ticks both land before the 10,000,001 ns deadline.
    let config = ElectionConfig::new(Duration::from_nanos(10_000_001), 1).unwrap();
    let error = ClusterError::Provider {
        kind: cluster_sdk::error::ProviderErrorKind::Timeout,
        message: "renewal timed out".to_owned(),
    };
    *recorded.renew_errors.lock().unwrap() =
        [Some(error.clone()), None, Some(error.clone()), Some(error)].into();
    let mut watch = election.elect_with_config("job", config).await.unwrap();
    assert!(matches!(
        election_event(&mut watch).await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    for attempt in 1..=4 {
        advance(config.renewal_interval()).await;
        wait_for_ready_turns(|| recorded.renew_attempts.load(Ordering::Relaxed) == attempt).await;
        if attempt < 4 {
            assert_eq!(watch.status(), LeaderStatus::Leader);
        }
    }
    assert!(matches!(
        election_event(&mut watch).await,
        LeaderWatchEvent::Status(LeaderStatus::Lost)
    ));
    assert!(matches!(
        election_event(&mut watch).await,
        LeaderWatchEvent::Status(LeaderStatus::Follower)
    ));
    assert_eq!(
        recorded
            .inner
            .get(ELECTION_KEY)
            .await
            .unwrap()
            .unwrap()
            .version,
        2
    );
    assert_eq!(cleanup.finish().await, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn native_cache_lag_reaches_the_election_watch_and_reconciles() {
    let (RunningMemory { client, cleanup }, recorded) =
        recorded_election(WatchMode::NativeThroughLag);
    let election = client.leader_election_backend("alpha").unwrap();
    let mut watch = election.elect("job").await.unwrap();
    assert!(matches!(
        election_event(&mut watch).await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    let initial = recorded.inner.get(ELECTION_KEY).await.unwrap().unwrap();
    let read_count = || {
        recorded
            .calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|call| matches!(call,ClaimCall::Read(key) if key == ELECTION_KEY))
            .count()
    };
    wait_for_ready_turns(|| read_count() == 1).await;
    let mut native = recorded.inner.watch(ELECTION_KEY).await.unwrap();
    for _ in 0..259 {
        recorded
            .inner
            .put(put(ELECTION_KEY, &initial.value))
            .await
            .unwrap();
    }
    timeout(TEST_DEADLINE, async {
        for _ in 0..256 {
            assert!(
                matches!(native.recv().await, Some(CacheWatchEvent::Event(CacheEvent::Changed { key })) if key == ELECTION_KEY)
            );
        }
    })
    .await
    .expect("all 256 native frames arrive within one deadline");
    wait_for_ready_turns(|| read_count() == 257).await;
    *recorded.read_error.lock().unwrap() = Some(ClusterError::Shutdown);
    recorded
        .inner
        .put(put(ELECTION_KEY, &initial.value))
        .await
        .unwrap();
    assert!(matches!(
        timeout(TEST_DEADLINE, native.recv()).await.unwrap(),
        Some(CacheWatchEvent::Lagged { dropped: 3 })
    ));
    assert!(matches!(
        election_event(&mut watch).await,
        LeaderWatchEvent::Lagged { dropped: 3 }
    ));
    assert!(matches!(
        election_event(&mut watch).await,
        LeaderWatchEvent::Closed(ClusterError::Shutdown)
    ));
    assert_eq!(cleanup.finish().await, Ok(()));
}

#[derive(Debug, PartialEq, Eq)]
struct OperationSpan {
    name: &'static str,
    fields: BTreeMap<&'static str, String>,
}

impl Visit for OperationSpan {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields.insert(field.name(), value.to_owned());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.fields.insert(field.name(), format!("{value:?}"));
    }
}

struct OperationSpans {
    caller: ThreadId,
    next_id: AtomicU64,
    spans: Arc<Mutex<Vec<OperationSpan>>>,
}

impl Subscriber for OperationSpans {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.is_span()
            && matches!(
                metadata.name(),
                "cluster.cache.get" | "cluster.lock.try_lock" | "cluster.leader.elect"
            )
    }

    fn new_span(&self, attributes: &Attributes<'_>) -> Id {
        if thread::current().id() == self.caller {
            let mut span = OperationSpan {
                name: attributes.metadata().name(),
                fields: BTreeMap::new(),
            };
            attributes.record(&mut span);
            self.spans
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(span);
        }
        Id::from_u64(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, _: &Event<'_>) {}
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

#[tokio::test]
async fn a_memory_operation_emits_the_bounded_provider_in_each_primitive_span() {
    const CHILD_DEADLINE: Duration = Duration::from_secs(15);
    const CHILD_MODE: &str = "EB_TEST_MEMORY_PROVIDER_SPAN_CHILD";
    const TEST_NAME: &str = "a_memory_operation_emits_the_bounded_provider_in_each_primitive_span";
    const COMPLETE: &str = "EB memory provider span assertions and cleanup complete";

    if std::env::var_os(CHILD_MODE).as_deref() == Some(std::ffi::OsStr::new("1")) {
        let spans = Arc::new(Mutex::new(Vec::new()));
        tracing::subscriber::set_global_default(OperationSpans {
            caller: thread::current().id(),
            next_id: AtomicU64::new(1),
            spans: spans.clone(),
        })
        .expect("the child owns its tracing subscriber");
        timeout(TEST_DEADLINE, async {
            let RunningMemory { client, cleanup } =
                prepare_memory(memory_options(&["alpha"])).unwrap().start();
            let cache = client.cache_backend("alpha").unwrap();
            let lock = client.lock_backend("alpha").unwrap();
            let leader = client.leader_election_backend("alpha").unwrap();

            assert_eq!(cache.get("provider-key").await.unwrap(), None);
            let guard = lock
                .try_lock("provider-lock", Duration::from_secs(30))
                .await
                .unwrap();
            let mut watch = leader.elect("provider-election").await.unwrap();
            assert!(matches!(
                election_event(&mut watch).await,
                LeaderWatchEvent::Status(LeaderStatus::Leader)
            ));
            guard.release().await.unwrap();
            watch.resign().await.unwrap();
            assert_eq!(cleanup.finish().await, Ok(()));
        })
        .await
        .expect("memory operations and cleanup complete");

        let mut spans = spans.lock().unwrap();
        spans.retain(|span| {
            !span
                .fields
                .get("key")
                .is_some_and(|key| key.starts_with("$lease/"))
        });
        assert_eq!(
            *spans,
            [
                OperationSpan {
                    name: "cluster.cache.get",
                    fields: BTreeMap::from([
                        ("key", "provider-key".to_owned()),
                        ("provider", "eb-memory".to_owned()),
                    ]),
                },
                OperationSpan {
                    name: "cluster.lock.try_lock",
                    fields: BTreeMap::from([
                        ("lock", "provider-lock".to_owned()),
                        ("provider", "eb-memory".to_owned()),
                    ]),
                },
                OperationSpan {
                    name: "cluster.leader.elect",
                    fields: BTreeMap::from([
                        ("election", "provider-election".to_owned()),
                        ("provider", "eb-memory".to_owned()),
                    ]),
                },
            ]
        );
        println!("{COMPLETE}");
        return;
    }

    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(CHILD_MODE, "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("start tracing test child");
    let mut stdout_pipe = child.stdout.take().unwrap();
    let mut stderr_pipe = child.stderr.take().unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let outcome = timeout(CHILD_DEADLINE, async {
        tokio::try_join!(
            child.wait(),
            stdout_pipe.read_to_end(&mut stdout),
            stderr_pipe.read_to_end(&mut stderr),
        )
    })
    .await;
    let (status, _, _) = match outcome {
        Ok(Ok(result)) => result,
        failed => {
            let reaped = timeout(TEST_DEADLINE, child.kill()).await;
            let drained = timeout(TEST_DEADLINE, async {
                tokio::join!(
                    stdout_pipe.read_to_end(&mut stdout),
                    stderr_pipe.read_to_end(&mut stderr),
                )
            })
            .await;
            panic!(
                "tracing child wait failed: {failed:?}, kill/reap: {reaped:?}, output: {drained:?}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            );
        }
    };
    let stdout = String::from_utf8_lossy(&stdout);
    assert!(
        status.success(),
        "tracing child exited with {status}\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&stderr)
    );
    assert!(
        stdout.contains(&format!("test {TEST_NAME} ..."))
            && stdout.lines().any(|line| line.ends_with(COMPLETE))
            && stdout.contains("test result: ok. 1 passed; 0 failed; 0 ignored;"),
        "tracing child did not run the expected assertions:\n{stdout}"
    );
}

#[derive(Default)]
struct RecordingResolver(AtomicUsize);

#[toolkit::async_trait]
impl EndpointResolver for RecordingResolver {
    async fn resolve_endpoint(&self, _: &str) -> Result<Option<String>, ResolveError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(None)
    }

    async fn resolve_grpc_endpoint(&self, _: &str) -> Result<Option<String>, ResolveError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(None)
    }

    async fn resolve_target(&self, _: &ResolveTarget) -> Result<Option<String>, ResolveError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(None)
    }

    fn invalidate(&self, _: &ResolveTarget) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[tokio::test]
async fn the_grpc_enabled_wiring_keeps_the_prepared_memory_client_local() {
    let RunningMemory { client, cleanup } = prepare_memory(memory_options(&["event-broker"]))
        .unwrap()
        .start();
    let hub = ClientHub::new();
    hub.register::<dyn ClusterClient>(client.clone());
    let view = hub.for_gear("event-broker");
    let resolver = Arc::new(RecordingResolver::default());
    let matching: Vec<_> = toolkit::inventory::iter::<ConsumerRegistration>
        .into_iter()
        .filter(|entry| entry.owner_gear == "cluster-sdk" && entry.dep_gear == "cluster")
        .collect();
    let [registration] = matching.as_slice() else {
        panic!(
            "expected one cluster SDK registration, got {}",
            matching.len()
        );
    };

    assert_eq!(
        (registration.wire)(
            &view,
            resolver.clone(),
            toolkit::contract_support::wiring::ClientTuning::default(),
        )
        .unwrap(),
        WireOutcome::Local
    );
    assert!(Arc::ptr_eq(
        &client,
        &view.try_get_local::<dyn ClusterClient>().unwrap()
    ));
    assert!(Arc::ptr_eq(
        &client,
        &view.try_get::<dyn ClusterClient>().unwrap()
    ));
    assert!(!hub.has_remote_proxy::<dyn ClusterClient>());
    assert!(!view.has_remote_proxy::<dyn ClusterClient>());
    assert_eq!(resolver.0.load(Ordering::Relaxed), 0);

    timeout(TEST_DEADLINE, async {
        let facade = EventBrokerCluster::resolve(&view).await.unwrap();
        facade
            .cache
            .put(put("local-cache", b"native"))
            .await
            .unwrap();
        let cache = client.cache_backend("event-broker").unwrap();
        assert_eq!(
            cache.get("evbk/local-cache").await.unwrap(),
            Some(CacheEntry {
                value: b"native".to_vec(),
                version: 1,
            })
        );
        assert_eq!(cache.get("local-cache").await.unwrap(), None);

        let ttl = Duration::from_secs(30);
        let guard = facade.lock.try_lock("local-lock", ttl).await.unwrap();
        let lock = client.lock_backend("event-broker").unwrap();
        assert!(matches!(
            lock.acquire("evbk/local-lock", "contender", ttl).await,
            Err(ClusterError::LockContended { name }) if name == "evbk/local-lock"
        ));

        let mut watch = facade
            .leader_election
            .elect("local-election")
            .await
            .unwrap();
        assert!(matches!(
            election_event(&mut watch).await,
            LeaderWatchEvent::Status(LeaderStatus::Leader)
        ));
        let leader = client.leader_election_backend("event-broker").unwrap();
        assert_eq!(
            leader
                .join(
                    "evbk/local-election",
                    "contender",
                    ElectionConfig::new(ttl, 1).unwrap(),
                )
                .await
                .unwrap(),
            None
        );
        guard.release().await.unwrap();
        watch.resign().await.unwrap();
        assert!(facade.cache.delete("local-cache").await.unwrap());
        assert_eq!(cleanup.finish().await, Ok(()));
    })
    .await
    .expect("local facades and cleanup complete");
    assert!(!hub.has_remote_proxy::<dyn ClusterClient>());
    assert!(!view.has_remote_proxy::<dyn ClusterClient>());
    assert_eq!(resolver.0.load(Ordering::Relaxed), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_hundred_fifty_six_actors_contend_for_independent_cache_lock_and_election_state() {
    const ACTORS: usize = 256;
    const RESOURCE: &str = "actor-contention";
    const LOCK_ROW: &str = "$lease/lock/actor-contention";
    const ELECTION_ROW: &str = "$lease/election/actor-contention";
    const TTL: Duration = Duration::from_secs(60);

    let RunningMemory { client, cleanup } =
        prepare_memory(memory_options(&["alpha"])).unwrap().start();
    let cache = client.cache_backend("alpha").unwrap();
    let lock = client.lock_backend("alpha").unwrap();
    let leader = client.leader_election_backend("alpha").unwrap();
    let config = ElectionConfig::new(TTL, 1).unwrap();
    let start = Arc::new(Barrier::new(ACTORS + 1));
    let mut actors = JoinSet::new();
    let exercise = timeout(TEST_DEADLINE, async {
        for id in 0..ACTORS {
            let start = start.clone();
            let cache = cache.clone();
            let lock = lock.clone();
            let leader = leader.clone();
            actors.spawn(async move {
                let owner = format!("worker-{id:03}");
                start.wait().await;
                let cached = cache
                    .put_if_absent(PutRequest {
                        key: RESOURCE,
                        value: owner.as_bytes(),
                        ttl: Ttl::Of(TTL),
                    })
                    .await;
                let locked = lock.acquire(RESOURCE, &owner, TTL).await;
                let elected = leader.join(RESOURCE, &owner, config).await;
                (owner, cached, locked, elected)
            });
        }

        start.wait().await;
        let mut cache_winners = Vec::new();
        let mut lock_winners = Vec::new();
        let mut election_winners = Vec::new();
        let mut misses = [0; 3];
        while let Some(attempt) = actors.join_next().await {
            let (owner, cached, locked, elected) = attempt.unwrap();
            if let Some(row) = cached.unwrap() {
                assert_eq!(
                    row,
                    CacheEntry {
                        value: owner.as_bytes().to_vec(),
                        version: 1,
                    }
                );
                cache_winners.push(row);
            } else {
                misses[0] += 1;
            }
            match locked {
                Ok(token) => {
                    assert_eq!(token, LeaseToken::new(RESOURCE, &owner, 1));
                    lock_winners.push(token);
                }
                Err(ClusterError::LockContended { name }) => {
                    assert_eq!(name, RESOURCE);
                    misses[1] += 1;
                }
                Err(error) => panic!("unexpected lock result for {owner}: {error}"),
            }
            if let Some(token) = elected.unwrap() {
                assert_eq!(token, LeaseToken::new(RESOURCE, &owner, 1));
                election_winners.push(token);
            } else {
                misses[2] += 1;
            }
        }
        assert_eq!(
            [
                cache_winners.len(),
                lock_winners.len(),
                election_winners.len()
            ],
            [1, 1, 1],
            "cache, lock and election each return one winner"
        );
        assert_eq!(misses, [255, 255, 255]);
        let ([cache_winner], [lock_winner], [election_winner]) = (
            cache_winners.as_slice(),
            lock_winners.as_slice(),
            election_winners.as_slice(),
        ) else {
            unreachable!("asserted one winner per primitive");
        };

        assert_eq!(
            cache.get(RESOURCE).await.unwrap().as_ref(),
            Some(cache_winner)
        );
        let lock_row = cache.get(LOCK_ROW).await.unwrap().unwrap();
        let lock_record = LeaseRecord::decode(&lock_row.value).unwrap();
        assert_eq!(
            (
                lock_row.version,
                LeaseToken::new(RESOURCE, lock_record.owner, lock_record.fence),
            ),
            (1, lock_winner.clone())
        );
        let election_row = cache.get(ELECTION_ROW).await.unwrap().unwrap();
        let election_bytes = election_row
            .value
            .strip_prefix(b"EBEL\x01")
            .map_or(election_row.value.as_slice(), |tagged| {
                tagged.split_first_chunk::<4>().unwrap().1
            });
        let election_record = LeaseRecord::decode(election_bytes).unwrap();
        assert_eq!(
            (
                election_row.version,
                LeaseToken::new(RESOURCE, election_record.owner, election_record.fence),
            ),
            (1, election_winner.clone())
        );
        lock.release(lock_winner).await.unwrap();
        leader.resign(election_winner).await.unwrap();
        assert!(cache.delete(RESOURCE).await.unwrap());
        assert_eq!(cache.get(LOCK_ROW).await.unwrap(), None);
        assert_eq!(cache.get(ELECTION_ROW).await.unwrap(), None);
        assert_eq!(cache.get(RESOURCE).await.unwrap(), None);
    })
    .await;

    let stopped = timeout(TEST_DEADLINE, async {
        actors.shutdown().await;
        cleanup.finish().await
    })
    .await;
    assert_eq!(stopped.unwrap(), Ok(()));
    exercise.expect("all 256 actors and their retained state complete before the watchdog");
}

#[tokio::test(start_paused = true)]
async fn a_fractional_lock_keeps_the_existing_millisecond_record() {
    for (ttl, ms) in [
        (Duration::ZERO, 0),
        (Duration::from_nanos(999_999), 0),
        (Duration::from_millis(1), 1),
    ] {
        let cache = MemoryCache::new();
        let lock = memory_lock(cache.clone());
        advance(Duration::from_nanos(100)).await;
        assert_eq!(
            lock.acquire("job", "first", ttl).await.unwrap(),
            LeaseToken::new("job", "first", 1)
        );
        assert_eq!(
            cache.get(LOCK_KEY).await.unwrap(),
            Some(CacheEntry {
                value: LeaseRecord {
                    owner: "first".to_owned(),
                    fence: 1,
                    deadline_ms: ms,
                    nonce: PreparedMemory::test_nonce_seed()
                }
                .encode(),
                version: 1
            })
        );
        if ms > 0 {
            assert!(
                matches!(lock.acquire("job","second",ttl).await,Err(ClusterError::LockContended{name}) if name=="job")
            );
            advance(Duration::from_millis(1)).await;
        }
        assert_eq!(
            lock.acquire("job", "second", ttl).await.unwrap(),
            LeaseToken::new("job", "second", 2)
        );
        lock.revoke().await;
    }
}

#[tokio::test(start_paused = true)]
async fn election_records_preserve_their_encoding_through_resign_and_zero_renewal() {
    let RunningMemory { client, cleanup } = prepared_memory(&["alpha"]).start();
    let raw = client.cache_backend("alpha").unwrap();
    let election = client.leader_election_backend("alpha").unwrap();
    let config = ElectionConfig::new(Duration::from_millis(3), 1).unwrap();
    let ordinary = LeaseRecord {
        owner: "holder".to_owned(),
        fence: 9,
        deadline_ms: 3,
        nonce: 500,
    }
    .encode();
    let token = LeaseToken::new("job", "holder", 9);
    let mut unknown = ordinary.clone();
    unknown[4] = 99;
    let foreign = [
        unknown,
        b"EBEL\x01\x00".to_vec(),
        [b"EBEL\x02".as_slice(), &1u32.to_be_bytes(), &ordinary].concat(),
        [
            b"EBEL\x01".as_slice(),
            &1_000_000u32.to_be_bytes(),
            &ordinary,
        ]
        .concat(),
        [b"EBEL\x01".as_slice(), &0u32.to_be_bytes(), &ordinary].concat(),
        [b"EBEL\x01".as_slice(), &1u32.to_be_bytes(), b"unknown"].concat(),
    ];
    for bytes in foreign {
        raw.put(put(ELECTION_KEY, &bytes)).await.unwrap();
        assert_eq!(election.join("job", "new", config).await.unwrap(), None);
        assert!(
            matches!(election.renew(&token,config.ttl()).await,Err(ClusterError::LockExpired{name}) if name=="job")
        );
        election.resign(&token).await.unwrap();
        assert_eq!(
            raw.get(ELECTION_KEY).await.unwrap(),
            Some(CacheEntry {
                value: bytes,
                version: 1
            })
        );
        assert!(raw.delete(ELECTION_KEY).await.unwrap());
    }
    for bytes in [
        ordinary.clone(),
        [b"EBEL\x01".as_slice(), &100u32.to_be_bytes(), &ordinary].concat(),
    ] {
        raw.put(put(ELECTION_KEY, &bytes)).await.unwrap();
        assert_eq!(election.join("job", "new", config).await.unwrap(), None);
        election.resign(&token).await.unwrap();
        assert_eq!(raw.get(ELECTION_KEY).await.unwrap(), None);
    }
    let token = election.join("job", "one", config).await.unwrap().unwrap();
    election.renew(&token, Duration::ZERO).await.unwrap();
    assert_eq!(
        election.join("job", "two", config).await.unwrap(),
        Some(LeaseToken::new("job", "two", 2))
    );
    assert_eq!(cleanup.finish().await, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn watched_election_claims_and_renewals_preserve_the_fractional_deadline() {
    let nonce_seed = PreparedMemory::test_nonce_seed();
    let (RunningMemory { client, cleanup }, recorded) = recorded_election(WatchMode::Recorded);
    let election = client.leader_election_backend("alpha").unwrap();
    advance(Duration::from_nanos(100)).await;
    let config = ElectionConfig::new(Duration::from_nanos(3_000_001), 1).unwrap();
    let mut watch = election.elect_with_config("job", config).await.unwrap();
    assert!(matches!(
        election_event(&mut watch).await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    for (version, ms, nonce) in [(1, 3, nonce_seed), (2, 5, nonce_seed.wrapping_add(1))] {
        if version == 2 {
            advance(Duration::from_millis(2)).await;
            wait_for_ready_turns(|| recorded.renew_attempts.load(Ordering::Relaxed) == 1).await;
        }
        let row = recorded.inner.get(ELECTION_KEY).await.unwrap().unwrap();
        let prefix = [b"EBEL\x01".as_slice(), &101u32.to_be_bytes()].concat();
        let owner = LeaseRecord::decode(row.value.strip_prefix(prefix.as_slice()).unwrap())
            .unwrap()
            .owner;
        assert_eq!(
            row,
            CacheEntry {
                value: [
                    prefix,
                    LeaseRecord {
                        owner,
                        fence: 1,
                        deadline_ms: ms,
                        nonce
                    }
                    .encode()
                ]
                .concat(),
                version
            }
        );
    }
    let competing = ElectionConfig::new(Duration::from_nanos(999_999), 1).unwrap();
    assert_eq!(
        election
            .join("other", "incumbent", competing)
            .await
            .unwrap(),
        Some(LeaseToken::new("other", "incumbent", 1))
    );
    advance(Duration::from_nanos(999_998)).await;
    let mut follower = election
        .elect_with_config("other", competing)
        .await
        .unwrap();
    assert!(matches!(
        election_event(&mut follower).await,
        LeaderWatchEvent::Status(LeaderStatus::Follower)
    ));
    advance(Duration::from_nanos(1)).await;
    let sender = {
        let recorded = recorded.watch.lock().unwrap();
        let Some((key, sender)) = recorded.as_ref() else {
            panic!("follower feed registered");
        };
        assert_eq!(key, "$lease/election/other");
        sender.clone()
    };
    sender.send(CacheWatchEvent::Reset).await.unwrap();
    assert!(matches!(
        election_event(&mut follower).await,
        LeaderWatchEvent::Reset
    ));
    assert!(matches!(
        election_event(&mut follower).await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    assert_eq!(cleanup.finish().await, Ok(()));
}

#[tokio::test(start_paused = true)]
async fn election_feed_failures_preserve_their_typed_outcome_and_timer_fallback() {
    for cause in ["eof", "resubscribe", "fatal", "read"] {
        let (RunningMemory { client, cleanup }, recorded) = recorded_election(if cause == "eof" {
            WatchMode::Ended
        } else {
            WatchMode::Recorded
        });
        let election = client.leader_election_backend("alpha").unwrap();
        let config = ElectionConfig::new(Duration::from_secs(1), 1).unwrap();
        let mut watch = election.elect_with_config("job", config).await.unwrap();
        assert!(matches!(
            election_event(&mut watch).await,
            LeaderWatchEvent::Status(LeaderStatus::Leader)
        ));
        let retry = ClusterError::Provider {
            kind: cluster_sdk::error::ProviderErrorKind::Timeout,
            message: "watch timed out".to_owned(),
        };
        let fatal = ClusterError::Provider {
            kind: cluster_sdk::error::ProviderErrorKind::AuthFailure,
            message: "read failed".to_owned(),
        };
        match cause {
            "resubscribe" => {
                *recorded.watch_error.lock().unwrap() = Some(fatal.clone());
                let sender = recorded.election_feed();
                sender.send(CacheWatchEvent::Closed(retry)).await.unwrap();
                wait_for_ready_turns(|| recorded.watch_calls.load(Ordering::Relaxed) == 2).await;
            }
            "fatal" => {
                recorded
                    .election_feed()
                    .send(CacheWatchEvent::Closed(fatal.clone()))
                    .await
                    .unwrap();
                assert!(
                    matches!(election_event(&mut watch).await,LeaderWatchEvent::Closed(ClusterError::Provider{kind:cluster_sdk::error::ProviderErrorKind::AuthFailure,message}) if message=="read failed")
                );
            }
            "read" => {
                for err in [retry, fatal.clone()] {
                    *recorded.read_error.lock().unwrap() = Some(err);
                    recorded
                        .election_feed()
                        .send(CacheWatchEvent::Reset)
                        .await
                        .unwrap();
                    assert!(matches!(
                        election_event(&mut watch).await,
                        LeaderWatchEvent::Reset
                    ));
                }
                assert!(
                    matches!(election_event(&mut watch).await,LeaderWatchEvent::Closed(ClusterError::Provider{kind:cluster_sdk::error::ProviderErrorKind::AuthFailure,message}) if message=="read failed")
                );
            }
            _ => {}
        }
        if matches!(cause, "eof" | "resubscribe") {
            assert!(timeout(Duration::ZERO, watch.changed()).await.is_err());
            advance(config.renewal_interval()).await;
            wait_for_ready_turns(|| recorded.renew_attempts.load(Ordering::Relaxed) == 1).await;
            assert_eq!(
                recorded
                    .inner
                    .get(ELECTION_KEY)
                    .await
                    .unwrap()
                    .unwrap()
                    .version,
                2
            );
            assert_eq!(
                recorded.watch_calls.load(Ordering::Relaxed),
                if cause == "eof" { 1 } else { 2 }
            );
        } else {
            assert_eq!(recorded.inner.get(ELECTION_KEY).await.unwrap(), None);
        }
        assert_eq!(cleanup.finish().await, Ok(()));
    }
}

#[tokio::test(start_paused = true)]
async fn an_election_reset_checks_the_incumbent_owner_fence_and_liveness() {
    for variant in ["matching", "owner", "fence", "lapsed"] {
        let (RunningMemory { client, cleanup }, recorded) = recorded_election(WatchMode::Recorded);
        let election = client.leader_election_backend("alpha").unwrap();
        let mut watch = election.elect("job").await.unwrap();
        assert!(matches!(
            election_event(&mut watch).await,
            LeaderWatchEvent::Status(LeaderStatus::Leader)
        ));
        let initial = recorded.inner.get(ELECTION_KEY).await.unwrap().unwrap();
        let mut lease = LeaseRecord::decode(&initial.value).unwrap();
        match variant {
            "owner" => lease.owner = "successor".to_owned(),
            "fence" => lease.fence = 2,
            "lapsed" => lease.deadline_ms = 0,
            _ => {}
        }
        recorded
            .inner
            .put(put(ELECTION_KEY, &lease.encode()))
            .await
            .unwrap();
        recorded
            .election_feed()
            .send(CacheWatchEvent::Reset)
            .await
            .unwrap();
        assert!(matches!(
            election_event(&mut watch).await,
            LeaderWatchEvent::Reset
        ));
        if variant == "matching" {
            assert_eq!(watch.status(), LeaderStatus::Leader);
            assert_eq!(
                recorded.inner.get(ELECTION_KEY).await.unwrap(),
                Some(CacheEntry {
                    value: initial.value,
                    version: 2
                })
            );
        } else {
            assert!(matches!(
                election_event(&mut watch).await,
                LeaderWatchEvent::Status(LeaderStatus::Lost)
            ));
            let expected = if variant == "lapsed" {
                LeaderStatus::Leader
            } else {
                LeaderStatus::Follower
            };
            assert!(
                matches!(election_event(&mut watch).await,LeaderWatchEvent::Status(state) if state==expected)
            );
        }
        assert_eq!(cleanup.finish().await, Ok(()));
    }
}

#[tokio::test(start_paused = true)]
#[should_panic(expected = "condition still pending after 256 ready turns")]
async fn an_unmet_observation_fails_within_the_ready_turn_bound() {
    wait_for_ready_turns(|| false).await;
}

#[tokio::test(start_paused = true)]
async fn a_fractional_election_record_outlives_its_lease_by_the_retention_window() {
    let RunningMemory { client, cleanup } = prepared_memory(&["alpha"]).start();
    let election = client.leader_election_backend("alpha").unwrap();
    let raw = client.cache_backend("alpha").unwrap();
    let ttl = Duration::from_nanos(999_999);
    let config = ElectionConfig::new(ttl, 1).unwrap();
    assert_eq!(
        election.join("job", "holder", config).await.unwrap(),
        Some(LeaseToken::new("job", "holder", 1))
    );
    let initial = raw.get(ELECTION_KEY).await.unwrap().unwrap();
    advance(Duration::from_millis(100) + Duration::from_nanos(999_998)).await;
    assert_eq!(raw.get(ELECTION_KEY).await.unwrap(), Some(initial));
    advance(Duration::from_nanos(1)).await;
    assert_eq!(raw.get(ELECTION_KEY).await.unwrap(), None);
    assert_eq!(cleanup.finish().await, Ok(()));
}
