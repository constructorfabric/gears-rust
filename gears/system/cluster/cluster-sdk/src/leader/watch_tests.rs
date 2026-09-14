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

/// M13: an owed `Lagged` must be payable *proactively*, so a consumer that fell
/// behind is told even when the election then goes quiet and no further event is
/// offered.
///
/// [`LeaderWatchSender::offer`] pays the debt on the *next* event, which is the
/// right ordering while events keep coming. But a stable leader's renewal emits
/// none, so once the election falls quiet a `changed()`-only consumer would
/// drain a backlog whose last status is a stale `Status(Leader)` and never learn
/// it dropped anything — the silent-staleness failure ADR-003 exists to
/// eliminate for a leadership gate. Both profiles' renewal ticks call
/// `flush_lagged` for exactly this; here it is asserted at the shared mechanism.
///
/// The notice is delivered by the flush and by nothing else: no event is offered
/// between the drained backlog and the flush, so a `changed()` that only returns
/// after the flush proves the flush paid it.
#[tokio::test]
async fn flush_lagged_pays_the_owed_notice_when_the_election_goes_quiet() {
    // Two usable slots. Fill them, then drop one so a `Lagged` is owed.
    let (mut tx, _resign, mut watch) = LeaderWatch::channel(2, LeaderStatus::Follower);
    assert!(tx.try_send_status(LeaderStatus::Leader), "slot 1");
    assert!(
        tx.try_send(LeaderWatchEvent::Reset),
        "slot 2 - buffer now full"
    );
    assert!(
        tx.try_send(LeaderWatchEvent::Reset),
        "a full buffer still returns true (the subscription is alive); this one is dropped and \
         a Lagged is owed"
    );

    // The consumer catches up on the backlog, freeing the room the notice needs.
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    assert!(matches!(watch.changed().await, LeaderWatchEvent::Reset));

    // The election now goes quiet: no status change, no further event. The tick
    // flush is the only thing that can pay the debt — and it must.
    tx.flush_lagged();

    assert!(
        matches!(
            watch.changed().await,
            LeaderWatchEvent::Lagged { dropped: 1 }
        ),
        "the owed Lagged must be delivered by the tick flush, not by an unrelated later event"
    );

    // And the debt is cleared: a second flush with no new drop adds nothing.
    tx.flush_lagged();
    assert!(tx.try_send(LeaderWatchEvent::Reset), "still live");
    assert!(
        matches!(watch.changed().await, LeaderWatchEvent::Reset),
        "no phantom second Lagged once the debt is paid"
    );
}
