use futures_util::FutureExt;

use super::*;

#[test]
fn parse_notification_round_trips_changed_deleted_expired() {
    assert_eq!(
        parse_notification("C:orders/lock"),
        ParsedNotification::Changed {
            key: "orders/lock".to_owned()
        }
    );
    assert_eq!(
        parse_notification("D:orders/lock"),
        ParsedNotification::Deleted {
            key: "orders/lock".to_owned()
        }
    );
    assert_eq!(
        parse_notification("E:orders/lock"),
        ParsedNotification::Expired {
            key: "orders/lock".to_owned()
        }
    );
}

#[test]
fn parse_notification_maps_empty_payload_to_reset() {
    assert_eq!(parse_notification(""), ParsedNotification::Reset);
}

#[test]
fn parse_notification_maps_malformed_payload_to_reset() {
    // No `:` separator at all.
    assert_eq!(parse_notification("garbage"), ParsedNotification::Reset);
    // Unrecognized event-type code.
    assert_eq!(parse_notification("X:key"), ParsedNotification::Reset);
}

#[test]
fn parse_notification_handles_a_key_containing_colons() {
    // `split_once` splits on the *first* `:` only, so a key that itself
    // contains `:` round-trips intact.
    assert_eq!(
        parse_notification("C:tenant:orders:lock"),
        ParsedNotification::Changed {
            key: "tenant:orders:lock".to_owned()
        }
    );
}

#[test]
fn validate_key_len_accepts_boundary_and_rejects_over_limit() {
    let at_limit = "k".repeat(MAX_KEY_BYTES);
    assert!(validate_key_len(&at_limit).is_ok());

    let over_limit = "k".repeat(MAX_KEY_BYTES + 1);
    assert!(matches!(
        validate_key_len(&over_limit),
        Err(ClusterError::InvalidName { .. })
    ));
}

#[tokio::test]
async fn dispatch_delivers_changed_only_to_the_matching_key() {
    let registry = WatchRegistry::new();
    let mut watch_a = registry.register("a");
    let mut watch_b = registry.register("b");

    registry
        .dispatch(&ParsedNotification::Changed {
            key: "a".to_owned(),
        })
        .await;

    assert!(matches!(
        watch_a.recv().await,
        Some(CacheWatchEvent::Event(CacheEvent::Changed { key })) if key == "a"
    ));
    // `b`'s watch received nothing — draining it would hang, so just assert
    // there is no event already buffered.
    assert!(watch_b.recv().now_or_never().is_none());
}

#[tokio::test]
async fn dispatch_reset_broadcasts_to_every_key_and_clears_subscriptions() {
    let registry = WatchRegistry::new();
    let mut watch_a = registry.register("a");
    let mut watch_b = registry.register("b");

    registry.dispatch(&ParsedNotification::Reset).await;

    assert!(matches!(watch_a.recv().await, Some(CacheWatchEvent::Reset)));
    assert!(matches!(watch_b.recv().await, Some(CacheWatchEvent::Reset)));

    // Subscriptions were cleared: the registry dropped every sender, so
    // this watch's stream has ended (`CacheWatch::recv`'s documented `None`
    // contract — "once the backend has dropped the sender without sending
    // a terminal `Closed`"). DESIGN.md §4.3's "consumers must resubscribe"
    // means calling `watch()` again for a fresh watch, not continuing to
    // poll this one.
    assert!(watch_a.recv().await.is_none());

    // A later Changed on `a` reaches no one — the registry has forgotten
    // this key entirely.
    registry
        .dispatch(&ParsedNotification::Changed {
            key: "a".to_owned(),
        })
        .await;
}

#[tokio::test]
async fn close_all_sends_terminal_shutdown_to_every_watcher() {
    let registry = WatchRegistry::new();
    let mut watch = registry.register("k");

    registry.close_all().await;

    assert!(matches!(
        watch.recv().await,
        Some(CacheWatchEvent::Closed(ClusterError::Shutdown))
    ));
}

#[tokio::test]
async fn dispatch_prunes_a_watcher_whose_consumer_dropped_it() {
    let registry = WatchRegistry::new();
    let watch = registry.register("k");
    drop(watch);

    // Must not panic despite the sole watcher on "k" being gone.
    registry
        .dispatch(&ParsedNotification::Changed {
            key: "k".to_owned(),
        })
        .await;

    assert!(
        registry.watchers.get("k").is_none(),
        "dispatch must prune the now-empty entry for a key whose sole watcher was dropped"
    );
}

/// Fills a watcher's entire 64-slot buffer, then dispatches `Reset` — the typed
/// terminal event must still be delivered rather than dropped once the buffer
/// drains (PGR-C4/C3). The broadcast awaits buffer space, so it is driven
/// concurrently with the drain.
#[tokio::test]
async fn reset_is_delivered_even_when_the_watcher_buffer_is_full() {
    let registry = WatchRegistry::new();
    let mut watch = registry.register("k");

    // Saturate the 64-slot channel (see `CacheWatch::channel(64)`).
    for _ in 0..64 {
        registry
            .dispatch(&ParsedNotification::Changed {
                key: "k".to_owned(),
            })
            .await;
    }

    let reg = Arc::clone(&registry);
    let broadcast = tokio::spawn(async move { reg.dispatch(&ParsedNotification::Reset).await });

    // Drain the buffered `Changed` events; the typed `Reset` must follow.
    let mut saw_reset = false;
    while let Some(event) = watch.recv().await {
        if matches!(event, CacheWatchEvent::Reset) {
            saw_reset = true;
            break;
        }
    }
    assert!(
        saw_reset,
        "Reset must reach a full-buffer watcher as the typed terminal event"
    );
    broadcast.await.expect("broadcast task completes");
}

/// Same as above for `close_all`: a full-buffer watcher must still observe the
/// typed `Closed(Shutdown)`, not merely a bare channel close (PGR-C4/C3).
#[tokio::test]
async fn shutdown_is_delivered_even_when_the_watcher_buffer_is_full() {
    let registry = WatchRegistry::new();
    let mut watch = registry.register("k");

    for _ in 0..64 {
        registry
            .dispatch(&ParsedNotification::Changed {
                key: "k".to_owned(),
            })
            .await;
    }

    let reg = Arc::clone(&registry);
    let closing = tokio::spawn(async move { reg.close_all().await });

    let mut saw_shutdown = false;
    while let Some(event) = watch.recv().await {
        if matches!(event, CacheWatchEvent::Closed(ClusterError::Shutdown)) {
            saw_shutdown = true;
            break;
        }
    }
    assert!(
        saw_shutdown,
        "Closed(Shutdown) must reach a full-buffer watcher as the typed terminal event"
    );
    closing.await.expect("close_all task completes");
}

// ---------------------------------------------------------------------------
// The production dispatch path (`dispatch_from_listener`).
//
// Everything above drives `dispatch`, which is `#[cfg(test)]` and *awaits* the
// `Reset` broadcast. Production cannot: the LISTEN reader loop must never stop
// draining (see `dispatch_from_listener`), so it spawns instead. That difference
// is exactly where the terminal-ordering and lost-watcher defects lived, so it
// needs its own coverage rather than being inferred from the awaited form.
// ---------------------------------------------------------------------------

/// Counts `watch_reset` calls so the tests can assert the DESIGN.md §8 signal
/// fires when — and only when — a `Reset` is actually delivered.
#[derive(Default)]
struct ResetCounter(AtomicU64);

impl cluster_sdk::ClusterMetrics for ResetCounter {
    fn cache_op(&self, _op: &str, _result: &str) {}
    fn cache_op_duration(&self, _op: &str, _seconds: f64) {}
    fn lock_op(&self, _op: &str, _result: &str) {}
    fn lock_op_duration(&self, _op: &str, _seconds: f64) {}
    fn leader_transition(&self, _transition: &str) {}
    fn discovery_op(&self, _op: &str, _result: &str) {}
    fn watch_reset(&self, _primitive: &str) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn provider_error(&self, _kind: &str) {}
}

fn reset_signal() -> (WatchResetSignal, Arc<ResetCounter>) {
    let counter = Arc::new(ResetCounter::default());
    let signal = WatchResetSignal {
        metrics: Arc::clone(&counter) as Arc<dyn cluster_sdk::ClusterMetrics>,
        provider: "postgres",
    };
    (signal, counter)
}

/// Drains a watch to end-of-stream. Terminates because every terminal broadcast
/// takes the senders out of the registry and drops them once delivery finishes.
async fn drain(watch: &mut CacheWatch) -> Vec<CacheWatchEvent> {
    let mut events = Vec::new();
    while let Some(event) = watch.recv().await {
        events.push(event);
    }
    events
}

/// The production path must actually deliver `Reset`, and must count the
/// DESIGN.md §8 watch-reset signal once it has.
#[tokio::test]
async fn dispatch_from_listener_delivers_reset_and_counts_it() {
    let registry = WatchRegistry::new();
    let mut watch_a = registry.register("a");
    let mut watch_b = registry.register("b");
    let (signal, resets) = reset_signal();

    registry.dispatch_from_listener(&ParsedNotification::Reset, &signal);

    assert!(matches!(watch_a.recv().await, Some(CacheWatchEvent::Reset)));
    assert!(matches!(watch_b.recv().await, Some(CacheWatchEvent::Reset)));
    // Both `recv`s complete as soon as the sends land, which is strictly before
    // the spawned task returns from `broadcast_and_clear` and emits the signal.
    // Sampling the counter here would be a race on scheduler ordering, so wait
    // for the emission — then hold it to exactly one.
    //
    // Bounded by a deadline rather than a fixed yield count: `yield_now` only
    // reschedules this task, it does not promise the spawned fan-out has reached
    // `emit()`, so any fixed number of yields is a guess that a correct
    // `dispatch_from_listener` can still lose. The deadline makes the failure mode
    // "this took too long" instead of "this was polled too few times".
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while resets.0.load(Ordering::Relaxed) == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the delivered Reset to emit \
             cluster_watch_resets_total"
        );
        // A sleep rather than `yield_now`, so a failing run waits out the deadline
        // idle instead of spinning a core on a single-threaded test runtime.
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(
        resets.0.load(Ordering::Relaxed),
        1,
        "one delivered Reset must emit exactly one cluster_watch_resets_total"
    );
}

/// The per-key fan-out stays inline on the production path — it is all
/// `try_send`, so nothing about it may become a scheduling boundary.
#[tokio::test]
async fn dispatch_from_listener_delivers_per_key_events_inline() {
    let registry = WatchRegistry::new();
    let mut watch = registry.register("k");
    let (signal, resets) = reset_signal();

    registry.dispatch_from_listener(
        &ParsedNotification::Changed {
            key: "k".to_owned(),
        },
        &signal,
    );

    assert!(
        matches!(
            watch.recv().now_or_never().flatten(),
            Some(CacheWatchEvent::Event(CacheEvent::Changed { key })) if key == "k"
        ),
        "a per-key event must be delivered without awaiting a spawned task"
    );
    assert_eq!(resets.0.load(Ordering::Relaxed), 0);
}

/// The SDK's `CacheWatch` contract: nothing may follow a terminal `Closed`.
///
/// A `Reset` fan-out spawned just before `stop()` cancels used to be able to land
/// after `close_all`'s `Closed(Shutdown)` on the very same channel. Which of the
/// two wins is a genuine schedule race and is *not* asserted here — only that
/// whichever wins, no `Reset` ever appears after a `Closed`.
#[tokio::test]
async fn a_reset_broadcast_never_lands_after_a_terminal_close() {
    let registry = WatchRegistry::new();
    let mut watch = registry.register("k");
    let (signal, _resets) = reset_signal();

    registry.dispatch_from_listener(&ParsedNotification::Reset, &signal);
    registry.close_all().await;

    let events = drain(&mut watch).await;
    let closed_at = events
        .iter()
        .position(|event| matches!(event, CacheWatchEvent::Closed(_)));
    if let Some(closed_at) = closed_at {
        assert!(
            !events[closed_at + 1..]
                .iter()
                .any(|event| matches!(event, CacheWatchEvent::Reset)),
            "Reset must never follow the terminal Closed, got: {events:?}"
        );
    }
}

/// `close_all` must always deliver a terminal event (DESIGN.md §10 step 2 /
/// `PG-LIFE-004`) — a concurrent `Reset` emptying the registry first must not
/// leave it with nothing to close.
///
/// The `Reset` here is *awaited* rather than spawned, which pins the interleaving
/// that used to break this: the registry is provably empty by the time
/// `close_all` runs, and the watcher registered afterwards is the one that must
/// still be closed.
#[tokio::test]
async fn close_all_still_closes_a_watch_registered_after_a_reset_emptied_the_registry() {
    let registry = WatchRegistry::new();
    registry.dispatch(&ParsedNotification::Reset).await;
    let mut watch = registry.register("k");

    registry.close_all().await;

    assert!(
        matches!(
            watch.recv().await,
            Some(CacheWatchEvent::Closed(ClusterError::Shutdown))
        ),
        "a watch registered after a Reset must still receive the terminal Closed"
    );
}

/// A `watch()` landing *after* `close_all` must not produce a watcher that
/// silently receives nothing forever: the registry is latched closed, so
/// registration answers with the terminal event immediately.
#[tokio::test]
async fn a_watch_registered_after_close_all_is_terminated_immediately() {
    let registry = WatchRegistry::new();
    registry.close_all().await;

    let mut watch = registry.register("late");

    assert!(
        matches!(
            watch.recv().now_or_never().flatten(),
            Some(CacheWatchEvent::Closed(ClusterError::Shutdown))
        ),
        "registering into a closed registry must yield the terminal event, not silence"
    );
}

/// The terminal event a late registration is handed must be the error the
/// registry actually closed with, not a fixed `Shutdown`.
///
/// The LISTEN task closes the registry with `Provider { ConnectionLost }` when its
/// reconnect budget is exhausted, and that error is retryable where `Shutdown` is
/// not (`cluster-sdk/src/error.rs::ClusterError::is_retryable`). Answering a late
/// `watch()` with `Shutdown` would report the wrong cause and take the consumer's
/// own retry policy — `RestartingWatch` — out of the decision entirely.
#[tokio::test]
async fn a_watch_registered_after_a_connection_loss_close_replays_that_error() {
    let registry = WatchRegistry::new();
    let lost = ClusterError::Provider {
        kind: ProviderErrorKind::ConnectionLost,
        message: "LISTEN connection reconnect retry budget exhausted".to_owned(),
    };
    assert!(registry.broadcast_and_clear(Some(lost.clone())).await);

    let mut watch = registry.register("late");

    let event = watch.recv().now_or_never().flatten();
    assert!(
        matches!(
            &event,
            Some(CacheWatchEvent::Closed(err)) if err.is_retryable()
        ),
        "a late registration must replay the retryable ConnectionLost close, got: {event:?}"
    );
}

/// Once closed, the registry stays closed: a later `Reset` is suppressed rather
/// than delivered, and it must not be counted either.
#[tokio::test]
async fn a_reset_after_close_all_is_suppressed_and_uncounted() {
    let registry = WatchRegistry::new();
    let mut watch = registry.register("k");
    let (signal, resets) = reset_signal();

    registry.close_all().await;
    registry.dispatch_from_listener(&ParsedNotification::Reset, &signal);
    // Let the spawned broadcast run; it must find the registry closed and stop.
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    let events = drain(&mut watch).await;
    assert!(
        matches!(events.as_slice(), [CacheWatchEvent::Closed(_)]),
        "a closed registry must deliver only its terminal event, got: {events:?}"
    );
    assert_eq!(
        resets.0.load(Ordering::Relaxed),
        0,
        "a suppressed Reset must not be counted as one that happened"
    );
}

/// Every registered watcher, across every key, must be collected by one terminal
/// broadcast — the property the per-key `remove` in `drain_senders` preserves and
/// that `iter()`-then-`clear()` did not.
///
/// Note this asserts the *invariant*, not the race. Reproducing the race itself
/// requires registering a watcher between the collection and the clear, which has
/// no deterministic seam without an injected pause inside `drain_senders`; it is
/// not simulated here rather than being papered over with a sleep.
#[tokio::test]
async fn a_terminal_broadcast_collects_every_watcher_across_every_key() {
    let registry = WatchRegistry::new();
    let mut watches: Vec<CacheWatch> = (0..32)
        .map(|i| registry.register(&format!("key-{i}")))
        .collect();
    // Two watchers on one key, so the per-key `Vec` is exercised too.
    let mut shared = registry.register("key-0");

    registry.close_all().await;

    for (i, watch) in watches.iter_mut().enumerate() {
        assert!(
            matches!(
                watch.recv().await,
                Some(CacheWatchEvent::Closed(ClusterError::Shutdown))
            ),
            "watcher {i} was dropped by the broadcast without receiving anything"
        );
    }
    assert!(matches!(
        shared.recv().await,
        Some(CacheWatchEvent::Closed(ClusterError::Shutdown))
    ));
    assert!(
        registry.watchers.is_empty(),
        "the broadcast must leave no subscription behind"
    );
}
