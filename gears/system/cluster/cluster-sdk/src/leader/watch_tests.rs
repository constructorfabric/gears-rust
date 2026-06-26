// Created: 2026-06-11 by Constructor Tech
use super::{LeaderStatus, LeaderWatch, LeaderWatchEvent};
use crate::error::{ClusterError, ProviderErrorKind};

#[test]
fn initial_status_is_reported_before_any_event() {
    let (_tx, _resign, watch) = LeaderWatch::channel(8, LeaderStatus::Follower);
    assert_eq!(watch.status(), LeaderStatus::Follower);
    assert!(!watch.is_leader());
}

#[tokio::test]
async fn send_status_updates_snapshot_and_emits_event() {
    let (tx, _resign, mut watch) = LeaderWatch::channel(8, LeaderStatus::Follower);
    assert!(tx.send_status(LeaderStatus::Leader).await.is_ok());

    // Snapshot reflects the transition synchronously.
    assert_eq!(watch.status(), LeaderStatus::Leader);
    assert!(watch.is_leader());
    // ...and the matching event is delivered on the stream.
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
}

#[tokio::test]
async fn delivers_events_in_order_then_closes_on_sender_drop() {
    let (tx, _resign, mut watch) = LeaderWatch::channel(8, LeaderStatus::Follower);
    assert!(tx.send_status(LeaderStatus::Leader).await.is_ok());
    assert!(tx.send(LeaderWatchEvent::Reset).await.is_ok());
    drop(tx);

    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    assert!(matches!(watch.changed().await, LeaderWatchEvent::Reset));
    // End of stream without an explicit Closed → synthesized Shutdown,
    // and it stays terminal on repeated calls.
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Closed(ClusterError::Shutdown)
    ));
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Closed(ClusterError::Shutdown)
    ));
}

#[tokio::test]
async fn revoke_delivers_both_terminal_events_even_when_the_buffer_is_full() {
    // Two usable slots; fill them so a plain `try_send` of the terminal events
    // would be dropped. The headroom reserved at construction must still deliver
    // the distinct `Status(Lost)` → `Closed(Shutdown)` two-step that a pure
    // event-stream consumer relies on (ADR-003).
    let (mut tx, _resign, mut watch) = LeaderWatch::channel(2, LeaderStatus::Follower);
    assert!(tx.send_status(LeaderStatus::Leader).await.is_ok());
    assert!(tx.send(LeaderWatchEvent::Reset).await.is_ok());

    // Usable buffer is now full; revoke must not block and must not drop.
    tx.revoke_for_shutdown(true);

    // The pre-filled events drain first, in order...
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    assert!(matches!(watch.changed().await, LeaderWatchEvent::Reset));
    // ...then both terminal events arrive, distinct and ordered.
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Status(LeaderStatus::Lost)
    ));
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Closed(ClusterError::Shutdown)
    ));
    // The snapshot guard still latches Lost for gate-pattern consumers.
    assert_eq!(watch.status(), LeaderStatus::Lost);
}

#[tokio::test]
async fn explicit_closed_event_is_delivered_verbatim() {
    let (tx, _resign, mut watch) = LeaderWatch::channel(8, LeaderStatus::Follower);
    let err = ClusterError::Provider {
        kind: ProviderErrorKind::AuthFailure,
        message: "bad credentials".to_owned(),
    };
    assert!(tx.send(LeaderWatchEvent::Closed(err)).await.is_ok());
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Closed(ClusterError::Provider {
            kind: ProviderErrorKind::AuthFailure,
            ..
        })
    ));
}

#[tokio::test]
async fn resign_round_trips_backend_result() {
    let (_tx, mut resign, watch) = LeaderWatch::channel(8, LeaderStatus::Leader);

    // Backend: receive the request and reply with success.
    let backend = tokio::spawn(async move {
        let Some(responder) = resign.recv().await else {
            panic!("a resign request must arrive");
        };
        responder.respond(Ok(()));
    });

    assert!(watch.resign().await.is_ok());
    assert!(backend.await.is_ok());
}

#[tokio::test]
async fn resign_propagates_backend_error() {
    let (_tx, mut resign, watch) = LeaderWatch::channel(8, LeaderStatus::Leader);
    let backend = tokio::spawn(async move {
        let Some(responder) = resign.recv().await else {
            panic!("a resign request must arrive");
        };
        responder.respond(Err(ClusterError::Provider {
            kind: ProviderErrorKind::ConnectionLost,
            message: "lost mid-release".to_owned(),
        }));
    });

    assert!(matches!(
        watch.resign().await,
        Err(ClusterError::Provider {
            kind: ProviderErrorKind::ConnectionLost,
            ..
        })
    ));
    assert!(backend.await.is_ok());
}

#[tokio::test]
async fn resign_after_backend_gone_is_best_effort_ok() {
    let (_tx, resign, watch) = LeaderWatch::channel(8, LeaderStatus::Leader);
    // Backend torn down (e.g. cluster shutdown) before the consumer resigns.
    drop(resign);
    assert!(watch.resign().await.is_ok());
}

#[tokio::test]
async fn resign_errors_when_backend_drops_responder_without_reply() {
    let (_tx, mut resign, watch) = LeaderWatch::channel(8, LeaderStatus::Leader);
    // Backend accepts the request, then drops the responder without
    // replying — a crash / connection loss mid-release. Per DESIGN §3.7 this
    // must propagate, not be masked as success.
    let backend = tokio::spawn(async move {
        let Some(responder) = resign.recv().await else {
            panic!("a resign request must arrive");
        };
        drop(responder);
    });

    assert!(matches!(
        watch.resign().await,
        Err(ClusterError::Provider {
            kind: ProviderErrorKind::ConnectionLost,
            ..
        })
    ));
    assert!(backend.await.is_ok());
}

#[test]
fn status_reports_lost_after_abrupt_sender_drop() {
    let (tx, _resign, watch) = LeaderWatch::channel(8, LeaderStatus::Leader);
    assert!(watch.is_leader());
    // Backend torn down abruptly — sender dropped without the graceful
    // terminal Status(Lost). The snapshot must not latch stale leadership.
    drop(tx);
    assert_eq!(watch.status(), LeaderStatus::Lost);
    assert!(!watch.is_leader());
}

#[tokio::test]
async fn dropping_watch_performs_no_io_and_does_not_resign() {
    let (_tx, mut resign, watch) = LeaderWatch::channel(8, LeaderStatus::Leader);
    // Dropping the watch must not send a resign request and must not block.
    drop(watch);
    assert!(resign.recv().await.is_none());
}

async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn run_while_leader_runs_on_leader_and_cancels_on_loss() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    let (tx, _resign, watch) = LeaderWatch::channel(8, LeaderStatus::Follower);
    let runs = Arc::new(AtomicUsize::new(0));
    let cancelled = Arc::new(AtomicBool::new(false));
    let r = Arc::clone(&runs);
    let c = Arc::clone(&cancelled);
    let driver = tokio::spawn(async move {
        watch
            .run_while_leader(Duration::from_secs(1), move |token| {
                let r = Arc::clone(&r);
                let c = Arc::clone(&c);
                async move {
                    r.fetch_add(1, Ordering::SeqCst);
                    token.cancelled().await;
                    c.store(true, Ordering::SeqCst);
                }
            })
            .await;
    });

    // Becoming leader starts the work exactly once.
    assert!(tx.send_status(LeaderStatus::Leader).await.is_ok());
    settle().await;
    assert_eq!(runs.load(Ordering::SeqCst), 1, "work starts on leadership");
    assert!(!cancelled.load(Ordering::SeqCst));

    // Losing leadership cancels the work's token.
    assert!(tx.send_status(LeaderStatus::Lost).await.is_ok());
    settle().await;
    assert!(
        cancelled.load(Ordering::SeqCst),
        "work is cancelled on leadership loss"
    );

    // Closing the watch terminally returns from the loop.
    drop(tx);
    assert!(driver.await.is_ok());
}

#[tokio::test]
async fn run_while_leader_tears_down_work_when_the_loop_future_is_dropped() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    // Set on the worker future's drop, proving the spawned task was torn down
    // rather than detached.
    struct DropFlag(Arc<AtomicBool>);
    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    let (tx, _resign, watch) = LeaderWatch::channel(8, LeaderStatus::Follower);
    let started = Arc::new(AtomicBool::new(false));
    let torn_down = Arc::new(AtomicBool::new(false));
    let s = Arc::clone(&started);
    let t = Arc::clone(&torn_down);
    let driver = tokio::spawn(async move {
        watch
            .run_while_leader(Duration::from_mins(1), move |_token| {
                let s = Arc::clone(&s);
                let t = Arc::clone(&t);
                async move {
                    let _guard = DropFlag(t);
                    s.store(true, Ordering::SeqCst);
                    // Unresponsive: ignores the token and never completes on its own.
                    std::future::pending::<()>().await;
                }
            })
            .await;
    });

    assert!(tx.send_status(LeaderStatus::Leader).await.is_ok());
    settle().await;
    assert!(started.load(Ordering::SeqCst), "the worker must start");
    assert!(
        !torn_down.load(Ordering::SeqCst),
        "the worker must keep running while leadership holds"
    );

    // Drop the `run_while_leader` future by aborting the task driving it.
    driver.abort();
    let _aborted = driver.await;
    settle().await;

    assert!(
        torn_down.load(Ordering::SeqCst),
        "dropping the loop future must tear down in-flight work, not detach it"
    );
}

#[tokio::test(start_paused = true)]
async fn run_while_leader_aborts_unresponsive_work_after_timeout() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    let (tx, _resign, watch) = LeaderWatch::channel(8, LeaderStatus::Follower);
    let starts = Arc::new(AtomicUsize::new(0));
    let s = Arc::clone(&starts);
    let driver = tokio::spawn(async move {
        watch
            .run_while_leader(Duration::from_millis(50), move |token| {
                let s = Arc::clone(&s);
                async move {
                    s.fetch_add(1, Ordering::SeqCst);
                    // Deliberately ignores its cancel token — unresponsive work.
                    let _ignored = token;
                    tokio::time::sleep(Duration::from_hours(1)).await;
                }
            })
            .await;
    });

    assert!(tx.send_status(LeaderStatus::Leader).await.is_ok());
    settle().await;
    assert_eq!(starts.load(Ordering::SeqCst), 1);

    // Loss cancels the worker, which ignores the token; after the stop timeout
    // the loop aborts it rather than wedging. Settle first so the loop enters
    // `stop_work` and arms its timeout, then advance past it to fire the abort.
    assert!(tx.send_status(LeaderStatus::Lost).await.is_ok());
    settle().await;
    tokio::time::advance(Duration::from_millis(60)).await;
    settle().await;

    // Re-election proves the loop survived the abort and spawns a fresh worker.
    assert!(tx.send_status(LeaderStatus::Leader).await.is_ok());
    settle().await;
    assert_eq!(
        starts.load(Ordering::SeqCst),
        2,
        "the loop must survive aborting unresponsive work"
    );

    drop(tx);
    assert!(driver.await.is_ok());
}
