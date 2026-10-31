//! The publisher's appends to the change log (ADR-0020).

use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::ClusterError;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use toolkit::client_hub::ClientHub;
use tracing_test::traced_test;
use uuid::Uuid;

use toolkit::{Healthcheck, HealthcheckStatus};

use super::{ATTEMPT_TIMEOUT, FIRST_BACKOFF, MAX_ATTEMPTS, PublishError, Publisher};
use crate::infra::proxy_cache::change_log::LOG_KEY;
use crate::infra::proxy_cache::testing::{
    CacheMetrics, ClusterFixture, ScriptedCache, connection_lost, facade, publisher,
    publisher_recording, publisher_reporting, put_log, read_log,
};
use crate::infra::proxy_cache::write_hook::PUBLISH_TIMEOUT;
use crate::infra::proxy_cache::{Change, WatchHealth, WatchState, WatchStatus};

/// A change no entry depends on, numbered `n` by its alias.
fn change(n: usize) -> Change {
    Change::Upstream {
        tenant_id: Uuid::new_v4(),
        id: Uuid::new_v4(),
        alias: format!("u{n}"),
    }
}

/// Submit `change` and wait for the append that carries it.
async fn publish(publisher: &Publisher, change: Change) -> Result<(), PublishError> {
    let (ack, appended) = oneshot::channel();
    assert!(publisher.submit(change, Some(ack)).is_ok(), "the task runs");
    appended.await.expect("the task acks")
}

/// The origins of the changes in the log, oldest first.
async fn origins(hub: &ClientHub) -> Vec<Uuid> {
    let log = read_log(&facade(hub).await).await.expect("a log");
    log.changes().map(|logged| logged.origin).collect()
}

fn scripted(hub: &Arc<ClientHub>) -> (ClusterFixture, Arc<ScriptedCache>) {
    ClusterFixture::scripted(hub, |cache| cache)
}

/// A publisher on the cluster in `hub` counting into `metrics`, with its
/// task and the token that cancels it.
async fn spawned(
    hub: &ClientHub,
    metrics: &Arc<CacheMetrics>,
) -> (Publisher, JoinHandle<()>, CancellationToken) {
    let cancel = CancellationToken::new();
    let (publisher, task) = Publisher::spawn(
        facade(hub).await,
        Uuid::new_v4(),
        metrics.clone(),
        WatchStatus::default(),
        cancel.clone(),
    );
    (publisher, task, cancel)
}

/// Two replicas that read the same log both append: the second
/// compare-and-swap conflicts and is retried, on a new log as on an
/// existing one.
///
/// ADR-0020: CACHE-LOG-003.
#[tokio::test]
async fn concurrent_appends_from_two_replicas_both_land() {
    let hub = Arc::new(ClientHub::new());
    let (cluster, cache) = scripted(&hub);
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let (publisher_a, stop_a) = publisher(&hub, a).await;
    let (publisher_b, stop_b) = publisher(&hub, b).await;

    for round in 0..2 {
        cache.meet(2);
        let conflicts = cache.conflicts();
        let (first, second) = tokio::join!(
            publish(&publisher_a, change(2 * round)),
            publish(&publisher_b, change(2 * round + 1)),
        );
        first.expect("a lands");
        second.expect("b lands");
        if round == 1 {
            assert_eq!(cache.conflicts(), conflicts + 1, "one swap conflicted");
        }
    }
    let mut landed = origins(&hub).await;
    landed.sort();
    let mut expected = vec![a, a, b, b];
    expected.sort();
    assert_eq!(landed, expected);
    assert_eq!(cache.writes(), 4);

    stop_a.cancel();
    stop_b.cancel();
    cluster.stop().await;
}

/// Changes queued while the task is busy go out in one append, which keeps
/// the newest 128.
///
/// ADR-0020: CACHE-LOG-003.
#[tokio::test]
async fn a_burst_is_one_append() {
    let hub = Arc::new(ClientHub::new());
    let (cluster, cache) = scripted(&hub);
    let me = Uuid::new_v4();
    let (publisher, stop) = publisher(&hub, me).await;

    // Queued before the task first runs, as during a reconcile pass.
    let acks: Vec<_> = (0..200)
        .map(|n| {
            let (ack, appended) = oneshot::channel();
            assert!(publisher.submit(change(n), Some(ack)).is_ok());
            appended
        })
        .collect();
    for appended in acks {
        appended.await.unwrap().expect("appended");
    }
    assert_eq!(cache.writes(), 1, "one append");
    let log = read_log(&facade(&hub).await).await.unwrap();
    assert_eq!(log.seq(), 200);
    assert_eq!(log.oldest(), Some(73));

    stop.cancel();
    cluster.stop().await;
}

/// A value this build cannot read that is no newer log (empty, not JSON,
/// an older version, a version 1 log that breaks its rules) is replaced by
/// a new log with a new ID, so readers know to empty their caches. The
/// warning names the new log and the replica that replaced it.
///
/// ADR-0020: CACHE-LOG-002.
#[tokio::test]
#[traced_test]
async fn an_unreadable_log_is_replaced_by_a_new_one() {
    let hub = Arc::new(ClientHub::new());
    let cluster = ClusterFixture::standalone(&hub);
    let cache = facade(&hub).await;
    let me = Uuid::new_v4();
    let (publisher, stop) = publisher(&hub, me).await;
    publish(&publisher, change(0)).await.unwrap();
    let mut old = read_log(&cache).await.unwrap();

    let json: serde_json::Value = serde_json::from_slice(&old.encode()).unwrap();
    let mut older = json.clone();
    older["v"] = 0.into();
    let mut broken = json;
    broken["seq"] = 7.into();
    let unreadable = [
        Vec::new(),
        b"not json".to_vec(),
        serde_json::to_vec(&older).unwrap(),
        serde_json::to_vec(&broken).unwrap(),
    ];
    for (n, value) in unreadable.into_iter().enumerate() {
        put_log(&cache, &value).await;
        let next = change(n + 1);
        publish(&publisher, next.clone()).await.expect("appended");

        let new = read_log(&cache).await.expect("readable again");
        assert_ne!(new.id(), old.id());
        assert!(
            logs_contain(&format!("log={}", new.id())),
            "names the new log"
        );
        assert!(logs_contain(&format!("origin={me}")), "names the replica");
        assert_eq!(new.seq(), 1);
        let logged: Vec<Change> = new.changes().map(|logged| logged.change).collect();
        assert_eq!(logged, [next]);
        old = new;
    }

    stop.cancel();
    cluster.stop().await;
}

/// A log of a newer version, as a newer build writes during a rolling
/// upgrade, is left as it is: the append is not tried again, its changes
/// are acked with the error and counted once each as `newer_log`, and the
/// healthcheck reports Degraded `proxy_cache_newer_log` naming the key to
/// delete. Once it is deleted, the next append lands and the healthcheck is
/// healthy again.
///
/// ADR-0020: CACHE-LOG-002.
#[tokio::test]
#[traced_test]
async fn a_newer_log_is_left_alone() {
    let hub = Arc::new(ClientHub::new());
    let (cluster, scripted) = scripted(&hub);
    let cache = facade(&hub).await;
    let metrics = Arc::new(CacheMetrics::default());
    let status = WatchStatus::default();
    status.set(WatchState::Up);
    let health = WatchHealth(status.clone());
    let (publisher, stop) =
        publisher_reporting(&hub, Uuid::new_v4(), metrics.clone(), status).await;
    publish(&publisher, change(0)).await.unwrap();
    assert_eq!(health.check().await.status, HealthcheckStatus::Healthy);
    let mut newer: serde_json::Value =
        serde_json::from_slice(&read_log(&cache).await.unwrap().encode()).unwrap();
    newer["v"] = 2.into();
    let newer = serde_json::to_vec(&newer).unwrap();
    put_log(&cache, &newer).await;

    let gets = scripted.gets();
    let skipped = change(1);
    let left = publish(&publisher, skipped.clone()).await;
    assert!(matches!(left, Err(PublishError::NewerLog(2))), "{left:?}");
    assert_eq!(scripted.gets(), gets + 1, "not retried");
    let stored = cache.get(LOG_KEY).await.unwrap().expect("still there");
    assert_eq!(stored.value, newer, "left as it was");
    assert_eq!(metrics.count("publish_failures newer_log"), 1);
    assert_eq!(metrics.publish_failures(), 1);
    assert!(logs_contain(&skipped.to_string()), "names the change");
    let degraded = health.check().await;
    assert_eq!(degraded.status, HealthcheckStatus::Degraded);
    assert_eq!(degraded.code.as_deref(), Some("proxy_cache_newer_log"));
    let message = degraded.message.unwrap_or_default();
    assert!(message.contains("version 2"), "{message}");
    assert!(message.contains("oagw/proxy-config-changes"), "{message}");

    cache.delete(LOG_KEY).await.unwrap();
    publish(&publisher, change(2)).await.expect("lands");
    assert_eq!(read_log(&cache).await.unwrap().seq(), 1);
    assert_eq!(health.check().await.status, HealthcheckStatus::Healthy);

    stop.cancel();
    cluster.stop().await;
}

/// An append other replicas keep beating gives up after five attempts and
/// acks the conflict; the next one lands.
///
/// ADR-0020: CACHE-LOG-003.
#[tokio::test(start_paused = true)]
async fn a_contended_append_gives_up_after_five_attempts() {
    let hub = Arc::new(ClientHub::new());
    let (cluster, cache) = scripted(&hub);
    let (publisher, stop) = publisher(&hub, Uuid::new_v4()).await;
    publish(&publisher, change(0)).await.unwrap();

    cache.conflict_next(usize::MAX);
    let contended = publish(&publisher, change(1)).await;
    assert!(
        matches!(contended, Err(PublishError::Contended)),
        "{contended:?}"
    );
    assert_eq!(cache.conflicts(), MAX_ATTEMPTS as usize);

    cache.conflict_next(0);
    publish(&publisher, change(2)).await.expect("lands");
    assert_eq!(read_log(&facade(&hub).await).await.unwrap().seq(), 2);

    stop.cancel();
    cluster.stop().await;
}

/// The attempts are spaced by a jittered backoff that starts at 20 ms and
/// doubles: four waits of 20, 40, 80 and 160 ms, each within ±20 %.
///
/// ADR-0020: CACHE-LOG-003.
#[tokio::test(start_paused = true)]
async fn retries_back_off_from_20_ms_doubling() {
    let hub = Arc::new(ClientHub::new());
    let (cluster, cache) = scripted(&hub);
    let (publisher, stop) = publisher(&hub, Uuid::new_v4()).await;
    publish(&publisher, change(0)).await.unwrap();

    cache.conflict_next(usize::MAX);
    let started = tokio::time::Instant::now();
    let contended = publish(&publisher, change(1)).await;
    let waited = started.elapsed();
    assert!(matches!(contended, Err(PublishError::Contended)));
    let waits: Duration = (0..MAX_ATTEMPTS - 1)
        .map(|n| FIRST_BACKOFF * 2_u32.pow(n))
        .sum();
    assert_eq!(waits, Duration::from_millis(300));
    assert!(
        waited >= waits.mul_f64(0.8) && waited <= waits.mul_f64(1.2),
        "waited {waited:?} between attempts"
    );

    stop.cancel();
    cluster.stop().await;
}

/// A cluster call that hangs fails its attempt after one second and the
/// append is retried: a hung cluster fails the append with the timeout
/// instead of stalling the task; one hung attempt alone does not.
///
/// ADR-0020: CACHE-FAIL-002.
#[tokio::test(start_paused = true)]
async fn a_hung_cluster_call_fails_its_attempt_and_is_retried() {
    let hub = Arc::new(ClientHub::new());
    let (cluster, cache) = scripted(&hub);
    let metrics = Arc::new(CacheMetrics::default());
    let (publisher, stop) = publisher_recording(&hub, Uuid::new_v4(), metrics.clone()).await;

    cache.hold();
    let started = tokio::time::Instant::now();
    let hung = publish(&publisher, change(0)).await;
    assert!(matches!(hung, Err(PublishError::TimedOut)), "{hung:?}");
    assert!(started.elapsed() >= ATTEMPT_TIMEOUT * MAX_ATTEMPTS);
    assert_eq!(metrics.count("publish_failures append_failed"), 1);

    // Released while the second attempt waits: that one lands.
    let (ack, appended) = oneshot::channel();
    assert!(publisher.submit(change(1), Some(ack)).is_ok());
    tokio::time::sleep(ATTEMPT_TIMEOUT + Duration::from_millis(100)).await;
    cache.release();
    appended.await.unwrap().expect("the retry lands");
    assert_eq!(read_log(&facade(&hub).await).await.unwrap().seq(), 1);
    assert_eq!(metrics.publish_failures(), 1);

    stop.cancel();
    cluster.stop().await;
}

/// One hung attempt leaves time for a second within the 2 s a write waits
/// for its append: the append lands, and the write hears so, before
/// [`PUBLISH_TIMEOUT`].
///
/// ADR-0020: CACHE-FAIL-002.
#[tokio::test(start_paused = true)]
async fn one_hung_attempt_leaves_time_for_a_second_within_the_publish_timeout() {
    let hub = Arc::new(ClientHub::new());
    let (cluster, cache) = scripted(&hub);
    let (publisher, stop) = publisher(&hub, Uuid::new_v4()).await;

    cache.hang_next(1);
    let started = tokio::time::Instant::now();
    publish(&publisher, change(0))
        .await
        .expect("the second attempt lands");
    let took = started.elapsed();
    assert!(took >= ATTEMPT_TIMEOUT, "the first attempt hung: {took:?}");
    assert!(took < PUBLISH_TIMEOUT, "within the write's wait: {took:?}");

    stop.cancel();
    cluster.stop().await;
}

/// Each failed attempt that is retried is logged with its number and
/// error.
///
/// ADR-0020: CACHE-FAIL-002.
#[tokio::test(start_paused = true)]
#[traced_test]
async fn each_retried_attempt_is_logged() {
    let hub = Arc::new(ClientHub::new());
    let (cluster, cache) = scripted(&hub);
    let (publisher, stop) = publisher(&hub, Uuid::new_v4()).await;

    cache.hang_next(2);
    publish(&publisher, change(0))
        .await
        .expect("the third attempt lands");
    logs_assert(|lines: &[&str]| {
        let retried: Vec<&&str> = lines
            .iter()
            .filter(|line| line.contains("WARN") && line.contains("failed; retrying"))
            .collect();
        match retried.as_slice() {
            [first, second] if first.contains("attempt=1") && second.contains("attempt=2") => {
                Ok(())
            }
            other => Err(format!("expected attempts 1 and 2 logged, got {other:?}")),
        }
    });

    stop.cancel();
    cluster.stop().await;
}

/// Appends are acked in the order their changes were handed over, so the
/// ack of a later change implies the earlier one's append ended: a registry
/// reconcile waits for its last write only
/// ([`one_wait_at_the_end`](crate::domain::repo::one_wait_at_the_end)).
///
/// ADR-0020: CACHE-LOG-003.
#[tokio::test]
async fn acks_follow_the_order_changes_were_handed_over() {
    let hub = Arc::new(ClientHub::new());
    let (cluster, cache) = scripted(&hub);
    let (publisher, stop) = publisher(&hub, Uuid::new_v4()).await;

    // The first change's append is under way when the second is queued:
    // two batches.
    cache.hold();
    let (first_ack, mut first) = oneshot::channel();
    assert!(publisher.submit(change(0), Some(first_ack)).is_ok());
    while cache.held_reads() == 0 {
        tokio::task::yield_now().await;
    }
    let (second_ack, second) = oneshot::channel();
    assert!(publisher.submit(change(1), Some(second_ack)).is_ok());
    cache.release();

    second.await.unwrap().expect("the second append lands");
    let first = first.try_recv().expect("acked before the second");
    assert!(first.is_ok(), "the first append landed: {first:?}");
    assert_eq!(read_log(&facade(&hub).await).await.unwrap().seq(), 2);

    stop.cancel();
    cluster.stop().await;
}

/// A transient cluster failure is retried five times, then acked with the
/// error, logged with the changes, and counted once per change; a permanent
/// one is not retried. The next append lands.
///
/// ADR-0020: CACHE-FAIL-002.
#[tokio::test(start_paused = true)]
#[traced_test]
async fn a_failing_append_is_logged_counted_and_acked_with_the_error() {
    let hub = Arc::new(ClientHub::new());
    let (cluster, cache) = scripted(&hub);
    let metrics = Arc::new(CacheMetrics::default());
    let (publisher, stop) = publisher_recording(&hub, Uuid::new_v4(), metrics.clone()).await;

    cache.fail_writes(Some(connection_lost()));
    let lost = change(0);
    let failed = publish(&publisher, lost.clone()).await;
    assert!(
        matches!(failed, Err(PublishError::Cluster(ref e)) if e.is_retryable()),
        "{failed:?}"
    );
    assert_eq!(cache.gets(), MAX_ATTEMPTS as usize, "retried");
    assert!(logs_contain(
        "Could not append proxy configuration changes to the cluster change log"
    ));
    assert!(logs_contain(&lost.to_string()), "names the change");
    assert_eq!(
        metrics.count("publish_failures append_failed"),
        1,
        "once, not per attempt"
    );

    cache.fail_writes(Some(ClusterError::Shutdown));
    let failed = publish(&publisher, change(1)).await;
    assert!(
        matches!(failed, Err(PublishError::Cluster(ClusterError::Shutdown))),
        "{failed:?}"
    );
    assert_eq!(cache.gets(), MAX_ATTEMPTS as usize + 1, "not retried");
    assert_eq!(metrics.count("publish_failures append_failed"), 2);

    // A batch counts each of its changes.
    let acks: Vec<_> = (2..5)
        .map(|n| {
            let (ack, appended) = oneshot::channel();
            assert!(publisher.submit(change(n), Some(ack)).is_ok());
            appended
        })
        .collect();
    for appended in acks {
        assert!(appended.await.unwrap().is_err());
    }
    assert_eq!(metrics.count("publish_failures append_failed"), 5);

    cache.fail_writes(None);
    publish(&publisher, change(5)).await.expect("lands");
    assert_eq!(metrics.publish_failures(), 5);

    stop.cancel();
    cluster.stop().await;
}

// ---------------------------------------------------------------------------
// Shutdown
// ---------------------------------------------------------------------------

/// Cancelled, the task finishes the append under way, then takes no more
/// changes, appends those queued by then in one append, and ends.
///
/// ADR-0020: CACHE-FAIL-002.
#[tokio::test(start_paused = true)]
async fn on_cancel_queued_changes_are_still_appended() {
    let hub = Arc::new(ClientHub::new());
    let (cluster, cache) = scripted(&hub);
    let metrics = Arc::new(CacheMetrics::default());
    let (publisher, task, cancel) = spawned(&hub, &metrics).await;

    cache.hold();
    let (ack, appended) = oneshot::channel();
    assert!(publisher.submit(change(0), Some(ack)).is_ok());
    tokio::time::sleep(Duration::from_millis(10)).await;
    let queued = [change(1), change(2)];
    for change in &queued {
        assert!(publisher.submit(change.clone(), None).is_ok());
    }
    cancel.cancel();
    cache.release();
    task.await.expect("ends");

    appended
        .await
        .unwrap()
        .expect("the append under way finished");
    let log = read_log(&facade(&hub).await).await.unwrap();
    assert_eq!(log.seq(), 3);
    let logged: Vec<Change> = log.changes().skip(1).map(|logged| logged.change).collect();
    assert_eq!(logged, queued, "the queue drained into one append");
    assert_eq!(cache.writes(), 2);
    assert_eq!(metrics.publish_failures(), 0);
    let late = change(3);
    assert_eq!(
        publisher.submit(late.clone(), None),
        Err(late),
        "takes no more"
    );
    cluster.stop().await;
}

/// A task aborted at the shutdown deadline counts every change it still
/// held, in hand or queued, as not published, and logs them with their IDs.
///
/// ADR-0020: CACHE-FAIL-002.
#[tokio::test(start_paused = true)]
#[traced_test]
async fn an_aborted_task_counts_and_logs_the_changes_it_held() {
    let hub = Arc::new(ClientHub::new());
    let (cluster, cache) = scripted(&hub);
    let metrics = Arc::new(CacheMetrics::default());
    let (publisher, task, cancel) = spawned(&hub, &metrics).await;

    cache.hold();
    let (in_hand, queued) = (change(0), change(1));
    let (ack, appended) = oneshot::channel();
    assert!(publisher.submit(in_hand.clone(), Some(ack)).is_ok());
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(publisher.submit(queued.clone(), None).is_ok());
    cancel.cancel();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());

    assert!(
        appended.await.is_err(),
        "the waiting writer hears it stopped"
    );
    assert_eq!(metrics.count("publish_failures publisher_stopped"), 2);
    assert_eq!(metrics.publish_failures(), 2);
    assert!(logs_contain("stopped before appending these changes"));
    assert!(logs_contain(&in_hand.to_string()));
    assert!(logs_contain(&queued.to_string()));
    let late = change(2);
    assert_eq!(publisher.submit(late.clone(), None), Err(late));
    cache.release();
    cluster.stop().await;
}
