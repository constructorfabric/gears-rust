//! Bounded single-flight discovery and its retryable availability categories.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::future::pending;
use std::sync::Arc;
use std::time::Duration;

use toolkit_canonical_errors::CanonicalError;

use super::{GtsPluginSelector, is_discovery_outage};

const BUDGET: Duration = Duration::from_secs(5);

#[tokio::test(start_paused = true)]
async fn expired_resolution_leaves_no_cached_selection_and_can_retry() {
    let selector = GtsPluginSelector::new();
    let started = tokio::time::Instant::now();
    let expired = selector
        .get_or_init_with_timeout(
            BUDGET,
            pending::<Result<String, String>>,
            std::convert::identity,
        )
        .await;
    assert_eq!(
        expired,
        Err("plugin discovery did not finish within 5s".to_owned())
    );
    assert_eq!(started.elapsed(), BUDGET);

    let retried = selector
        .get_or_init_with_timeout(
            BUDGET,
            || async { Ok("recovered".to_owned()) },
            |_| "expired",
        )
        .await
        .unwrap();
    assert_eq!(&*retried, "recovered");
}

#[tokio::test(start_paused = true)]
async fn cancelling_resolution_releases_single_flight_without_caching() {
    let selector = GtsPluginSelector::new();
    let entered = tokio::sync::Notify::new();
    let mut attempt = Box::pin(selector.get_or_init_with_timeout(
        BUDGET,
        || async {
            entered.notify_one();
            pending::<Result<String, &str>>().await
        },
        |_| "expired",
    ));
    tokio::select! {
        result = &mut attempt => panic!("resolution must still be pending: {result:?}"),
        () = entered.notified() => {},
    }
    drop(attempt);

    let retried = selector
        .get_or_init_with_timeout(BUDGET, || async { Ok("retried".to_owned()) }, |_| "expired")
        .await
        .unwrap();
    assert_eq!(&*retried, "retried");
}

#[tokio::test(start_paused = true)]
async fn single_flight_lock_wait_counts_against_the_callers_budget() {
    let selector = Arc::new(GtsPluginSelector::new());
    let entered = Arc::new(tokio::sync::Notify::new());
    let first = tokio::spawn({
        let selector = Arc::clone(&selector);
        let entered = Arc::clone(&entered);
        async move {
            selector
                .get_or_init(|| async {
                    entered.notify_one();
                    pending::<Result<String, &str>>().await
                })
                .await
        }
    });
    entered.notified().await;

    let started = tokio::time::Instant::now();
    let queued = selector
        .get_or_init_with_timeout(
            BUDGET,
            || async { panic!("queued resolver must not acquire the held lock") },
            |_| "queued timeout",
        )
        .await;
    assert_eq!(queued, Err("queued timeout"));
    assert_eq!(started.elapsed(), BUDGET);
    assert!(
        !first.is_finished(),
        "a queued timeout does not cancel its owner"
    );
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());

    let retried = selector
        .get_or_init_with_timeout(
            BUDGET,
            || async { Ok("after owner cancelled".to_owned()) },
            |_| "expired",
        )
        .await
        .unwrap();
    assert_eq!(&*retried, "after owner cancelled");
}

#[tokio::test(start_paused = true)]
async fn cached_selection_uses_neither_resolver_nor_timeout_error_factory() {
    let selector = GtsPluginSelector::pre_cached("cached".to_owned());
    let cached = selector
        .get_or_init_with_timeout(
            Duration::ZERO,
            || async { panic!("cached selection must not resolve again") },
            |_| -> &'static str { panic!("ready cached selection must not expire") },
        )
        .await
        .unwrap();
    assert_eq!(&*cached, "cached");
}

#[toolkit_canonical_errors::resource_error(toolkit_gts::gts_id!("cf.toolkit.plugins.plugin.v1~"))]
struct Catalogue;

#[test]
fn discovery_outages_exclude_credential_refusals_and_corrupt_pages() {
    let outages = [
        CanonicalError::service_unavailable().create(),
        Catalogue::deadline_exceeded("listing timed out").create(),
        Catalogue::resource_exhausted("listing throttled")
            .with_quota_violation("listing", "rate limit")
            .create(),
        Catalogue::aborted("listing aborted")
            .with_reason("LISTING_ABORTED")
            .create(),
    ];
    for error in outages {
        assert!(is_discovery_outage(&error), "{error:?}");
    }
    let faults = [
        CanonicalError::unauthenticated()
            .with_reason("PLATFORM_CREDENTIAL_MISSING")
            .create(),
        Catalogue::permission_denied()
            .with_reason("PLATFORM_CREDENTIAL_REFUSED")
            .create(),
        CanonicalError::internal("malformed registry page").create(),
    ];
    for error in faults {
        assert!(!is_discovery_outage(&error), "{error:?}");
    }
}
