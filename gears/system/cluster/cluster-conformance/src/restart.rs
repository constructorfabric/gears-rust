// Created: 2026-06-30 by Constructor Tech
//! Watch auto-restart combinator scenarios (`SC-REST-*`).
//!
//! See the [scenario catalog](../docs/scenarios/restart.md). The combinator is
//! SDK-level (no backend required): it wraps a watch and transparently reconnects
//! on retryable closes.
//!
//! ## Scope
//!
//! The reconnect path (SC-REST-001, 003, 004) requires a watch with a
//! resubscribe seam installed by the facade (`LeaderElectionV1`, etc.), which
//! needs a `ClientHub`. Those scenarios are deferred to the L3 wiring-crate
//! integration tests. The scenarios implemented here cover:
//!
//! - SC-REST-002: non-retryable closes propagate verbatim to the consumer.
//! - Smoke test: `Lagged`/`Reset` events pass through the combinator unchanged.
//! - Smoke test: `RetryPolicy::default()` produces a sensible configuration.
//!
//! The full SC-REST-001/003/004 assertions (retryable close → backoff →
//! reconnect → synthesized `Reset`) are added once the facade resubscribe seam
//! is accessible without a running `ClientHub`.

use cluster_sdk::cache::{CacheWatch, CacheWatchEvent};
use cluster_sdk::error::{ClusterError, ProviderErrorKind};
use cluster_sdk::leader::{LeaderStatus, LeaderWatch, LeaderWatchEvent};
use cluster_sdk::{RestartingWatch, RetryPolicy, ServiceWatch, ServiceWatchEvent};

/// Runs every implemented SC-REST-* scenario. No backend factory is needed —
/// all scenarios drive the combinator via the watch test-harness channels.
pub async fn run_restart_conformance() {
    scenario_rest_002_cache().await;
    scenario_rest_002_leader().await;
    scenario_rest_002_service().await;
    scenario_rest_002_capability_and_other().await;
    scenario_rest_pass_through().await;
    scenario_rest_retry_policy_smoke().await;
}

/// SC-REST-002 (cache): a non-retryable close (`AuthFailure`) reaches the consumer
/// verbatim; the combinator does not swallow or retry it.
pub async fn scenario_rest_002_cache() {
    let (tx, watch) = CacheWatch::channel(8);
    let mut restarting: RestartingWatch<CacheWatch> = watch.auto_restart(RetryPolicy::default());

    // Non-retryable close: AuthFailure.
    tx.send(CacheWatchEvent::Closed(ClusterError::Provider {
        kind: ProviderErrorKind::AuthFailure,
        message: "SC-REST-002".into(),
    }))
    .await
    .ok();

    let event = restarting
        .recv()
        .await
        .expect("SC-REST-002(cache): must receive the Closed event");
    assert!(
        matches!(
            event,
            CacheWatchEvent::Closed(ClusterError::Provider {
                kind: ProviderErrorKind::AuthFailure,
                ..
            })
        ),
        "SC-REST-002(cache): non-retryable AuthFailure must propagate verbatim, got {event:?}"
    );
    assert!(
        restarting.recv().await.is_none(),
        "SC-REST-002(cache): no further events after a terminal non-retryable close"
    );
}

/// SC-REST-002 (leader): a non-retryable `Shutdown` close propagates verbatim.
pub async fn scenario_rest_002_leader() {
    let (tx, _resign_rx, watch) = LeaderWatch::channel(8, LeaderStatus::Follower);
    let mut restarting: RestartingWatch<LeaderWatch> = watch.auto_restart(RetryPolicy::default());

    tx.send(LeaderWatchEvent::Closed(ClusterError::Shutdown))
        .await
        .ok();

    let event = restarting
        .recv()
        .await
        .expect("SC-REST-002(leader): must receive Closed");
    assert!(
        matches!(event, LeaderWatchEvent::Closed(ClusterError::Shutdown)),
        "SC-REST-002(leader): Shutdown must propagate verbatim, got {event:?}"
    );
}

/// SC-REST-002 (service): a non-retryable `Shutdown` close propagates verbatim.
pub async fn scenario_rest_002_service() {
    let (tx, watch) = ServiceWatch::channel(8);
    let mut restarting: RestartingWatch<ServiceWatch> = watch.auto_restart(RetryPolicy::default());

    tx.send(ServiceWatchEvent::Closed(ClusterError::Shutdown))
        .await
        .ok();

    let event = restarting
        .recv()
        .await
        .expect("SC-REST-002(service): must receive Closed");
    assert!(
        matches!(event, ServiceWatchEvent::Closed(ClusterError::Shutdown)),
        "SC-REST-002(service): Shutdown must propagate verbatim, got {event:?}"
    );
}

/// SC-REST-002 (cache, cont'd): `CapabilityNotMet` and `Provider { kind: Other }`
/// also propagate verbatim — the combinator treats every non-`Provider` error,
/// and every non-retryable `ProviderErrorKind`, as terminal.
pub async fn scenario_rest_002_capability_and_other() {
    let (tx, watch) = CacheWatch::channel(8);
    let mut restarting: RestartingWatch<CacheWatch> = watch.auto_restart(RetryPolicy::default());

    tx.send(CacheWatchEvent::Closed(ClusterError::CapabilityNotMet {
        primitive: "ClusterCacheV1",
        capability: "prefix_watch",
        provider: "SC-REST-002",
    }))
    .await
    .ok();

    let event = restarting
        .recv()
        .await
        .expect("SC-REST-002(capability): must receive the Closed event");
    assert!(
        matches!(
            event,
            CacheWatchEvent::Closed(ClusterError::CapabilityNotMet { .. })
        ),
        "SC-REST-002(capability): CapabilityNotMet must propagate verbatim, got {event:?}"
    );

    let (tx2, watch2) = CacheWatch::channel(8);
    let mut restarting2: RestartingWatch<CacheWatch> = watch2.auto_restart(RetryPolicy::default());

    tx2.send(CacheWatchEvent::Closed(ClusterError::Provider {
        kind: ProviderErrorKind::Other,
        message: "SC-REST-002".into(),
    }))
    .await
    .ok();

    let event2 = restarting2
        .recv()
        .await
        .expect("SC-REST-002(other): must receive the Closed event");
    assert!(
        matches!(
            event2,
            CacheWatchEvent::Closed(ClusterError::Provider {
                kind: ProviderErrorKind::Other,
                ..
            })
        ),
        "SC-REST-002(other): Provider{{kind: Other}} must propagate verbatim, got {event2:?}"
    );
}

/// Smoke test: `Lagged` and `Reset` events pass through the combinator unchanged
/// and do not trigger a reconnect attempt.
pub async fn scenario_rest_pass_through() {
    let (tx, watch) = CacheWatch::channel(8);
    let mut restarting: RestartingWatch<CacheWatch> = watch.auto_restart(RetryPolicy::default());

    tx.send(CacheWatchEvent::Lagged { dropped: 7 }).await.ok();
    tx.send(CacheWatchEvent::Reset).await.ok();

    let e1 = restarting.recv().await.expect("must receive Lagged");
    assert!(
        matches!(e1, CacheWatchEvent::Lagged { dropped: 7 }),
        "restart pass-through: Lagged must arrive unchanged, got {e1:?}"
    );
    let e2 = restarting.recv().await.expect("must receive Reset");
    assert!(
        matches!(e2, CacheWatchEvent::Reset),
        "restart pass-through: Reset must arrive unchanged, got {e2:?}"
    );
}

/// Smoke: `RetryPolicy::default()` produces a sensible configuration — the
/// fields satisfy the documented invariants (initial ≤ max, jitter ∈ [0,1]).
/// Not tagged SC-REST-003 (that scenario requires the facade resubscribe seam).
#[allow(clippy::unused_async, reason = "uniform async signature")]
pub async fn scenario_rest_retry_policy_smoke() {
    let policy = RetryPolicy::default();
    assert!(
        !policy.initial_backoff.is_zero(),
        "retry-policy smoke: default initial_backoff must be positive"
    );
    assert!(
        policy.initial_backoff <= policy.max_backoff,
        "retry-policy smoke: initial_backoff must be <= max_backoff"
    );
    assert!(
        (0.0..=1.0).contains(&policy.jitter_factor),
        "retry-policy smoke: jitter_factor must be in [0, 1]"
    );
}

// TODO(SC-REST-001) [deferred]: retryable close → backoff → resubscribe → Reset
//   requires a facade-installed resubscribe seam (needs ClientHub). Defer to L3.
// TODO(SC-REST-003) [deferred]: backoff schedule and retry-cap enforcement
//   require the resubscribe path — same reason.
// TODO(SC-REST-004) [deferred]: uniform across CacheWatch/LeaderWatch/ServiceWatch
//   for the reconnect path — same reason.
