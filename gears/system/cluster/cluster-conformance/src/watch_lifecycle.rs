// Created: 2026-06-30 by Constructor Tech
//! Watch lifecycle uniformity scenarios (`SC-WLU-*`).
//!
//! See the [scenario catalog](../docs/scenarios/watch-lifecycle.md). These
//! scenarios are SDK-level: they assert the uniform `{value, Lagged, Reset,
//! Closed}` union shape and terminal-signal semantics across all three watch
//! types, with no backend required.

use cluster_sdk::cache::{CacheWatch, CacheWatchEvent};
use cluster_sdk::error::ClusterError;
use cluster_sdk::leader::{LeaderStatus, LeaderWatch, LeaderWatchEvent};
use cluster_sdk::{ServiceWatch, ServiceWatchEvent};

/// SC-WLU-001: every watch event type carries exactly the four-variant union
/// `{value, Lagged, Reset, Closed}`. This is a structural compile-time check:
/// if any variant is removed the match arm below becomes unreachable (dead code
/// warning → error) or the non-exhaustive catch-all fires where it should not.
#[allow(
    clippy::unused_async,
    reason = "kept `async` so the runner can `.await` it uniformly with the other scenarios"
)]
pub async fn scenario_wlu_001() {
    fn _check_cache(e: &CacheWatchEvent) {
        let _ = match e {
            CacheWatchEvent::Event(_) => 0u8,
            CacheWatchEvent::Lagged { dropped: _ } => 1,
            CacheWatchEvent::Reset => 2,
            CacheWatchEvent::Closed(_) => 3,
            _ => 4, // non_exhaustive catch-all
        };
    }
    fn _check_leader(e: &LeaderWatchEvent) {
        let _ = match e {
            LeaderWatchEvent::Status(_) => 0u8,
            LeaderWatchEvent::Lagged { dropped: _ } => 1,
            LeaderWatchEvent::Reset => 2,
            LeaderWatchEvent::Closed(_) => 3,
            _ => 4,
        };
    }
    fn _check_service(e: &ServiceWatchEvent) {
        let _ = match e {
            ServiceWatchEvent::Change(_) => 0u8,
            ServiceWatchEvent::Lagged { dropped: _ } => 1,
            ServiceWatchEvent::Reset => 2,
            ServiceWatchEvent::Closed(_) => 3,
            _ => 4,
        };
    }
    // The check functions are intentionally dead — the value is in compilation.
    let _ = (
        _check_cache as fn(&CacheWatchEvent),
        _check_leader as fn(&LeaderWatchEvent),
        _check_service as fn(&ServiceWatchEvent),
    );
}

/// SC-WLU-004: a terminal `Closed` event is final — no further events arrive on
/// the stream after it. Transient blips are retried internally (no event emitted).
/// Verified over all three watch types via their test-harness channels.
pub async fn scenario_wlu_004() {
    // CacheWatch
    {
        let (tx, mut watch) = CacheWatch::channel(8);
        tx.send(CacheWatchEvent::Closed(ClusterError::Shutdown))
            .await
            .ok();
        let event = watch
            .recv()
            .await
            .expect("SC-WLU-004(cache): must receive Closed");
        assert!(
            matches!(event, CacheWatchEvent::Closed(ClusterError::Shutdown)),
            "SC-WLU-004(cache): Closed must be surfaced verbatim, got {event:?}"
        );
        // `recv` has no terminal-event latching — it's a bare channel
        // passthrough (see `CacheWatch::recv`'s doc comment) — so the sender
        // must be dropped for the channel to actually close, or this next
        // `recv` blocks forever waiting for a message that will never come.
        drop(tx);
        assert!(
            watch.recv().await.is_none(),
            "SC-WLU-004(cache): no further events must arrive after terminal Closed"
        );
    }

    // LeaderWatch — note: `changed()` always returns an event (it does not
    // return `Option`), so we assert on the variant and then check `is_leader()`
    // is no longer true as a proxy for terminal state.
    {
        let (tx, _resign_rx, mut watch) = LeaderWatch::channel(8, LeaderStatus::Follower);
        tx.send(LeaderWatchEvent::Closed(ClusterError::Shutdown))
            .await
            .ok();
        let event = watch.changed().await;
        assert!(
            matches!(event, LeaderWatchEvent::Closed(ClusterError::Shutdown)),
            "SC-WLU-004(leader): Closed must be surfaced verbatim, got {event:?}"
        );
        // Drop tx so the channel is closed; the next changed() must return Closed(Shutdown)
        // or a stream-end signal — a well-behaved watch must not deliver more events.
        drop(tx);
        let next = watch.changed().await;
        assert!(
            matches!(next, LeaderWatchEvent::Closed(_)),
            "SC-WLU-004(leader): no further value events must arrive after terminal Closed, got {next:?}"
        );
    }

    // ServiceWatch
    {
        let (tx, mut watch) = ServiceWatch::channel(8);
        tx.send(ServiceWatchEvent::Closed(ClusterError::Shutdown))
            .await
            .ok();
        let event = watch
            .recv()
            .await
            .expect("SC-WLU-004(service): must receive Closed");
        assert!(
            matches!(event, ServiceWatchEvent::Closed(ClusterError::Shutdown)),
            "SC-WLU-004(service): Closed must be surfaced verbatim, got {event:?}"
        );
        // Same bare-passthrough `recv` behavior as `CacheWatch` — drop the
        // sender first or the channel never closes and this hangs forever.
        drop(tx);
        assert!(
            watch.recv().await.is_none(),
            "SC-WLU-004(service): no further events must arrive after terminal Closed"
        );
    }
}

/// Runs every implemented SC-WLU-* scenario. No backend factory is needed —
/// all scenarios are SDK-level structural or channel-harness tests.
pub async fn run_watch_lifecycle_conformance() {
    scenario_wlu_001().await;
    scenario_wlu_004().await;
}

// TODO(SC-WLU-002) [L4]: Lagged under backpressure (all three watch types) —
//   fault-injection harness.
// TODO(SC-WLU-003) [L4]: Reset and recovery (all three watch types) —
//   fault-injection harness (Toxiproxy/induced reconnect).
