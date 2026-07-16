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
