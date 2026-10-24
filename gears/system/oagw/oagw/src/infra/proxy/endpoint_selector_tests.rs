use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::*;
use crate::domain::model::{Endpoint, Scheme};

fn ep(host: &str, port: u16, scheme: Scheme) -> Endpoint {
    Endpoint {
        scheme,
        host: host.to_string(),
        port,
    }
}

fn ssrf_off() -> Arc<SsrfGuard> {
    Arc::new(SsrfGuard::disabled())
}

fn ssrf_on() -> Arc<SsrfGuard> {
    Arc::new(SsrfGuard::from_config(&Default::default()).unwrap())
}

#[tokio::test]
async fn select_round_robin_distribution() {
    let selector = PoolEndpointSelector::new(ssrf_off());
    let id = Uuid::new_v4();
    let endpoints = vec![
        ep("127.0.0.1", 10001, Scheme::Https),
        ep("127.0.0.1", 10002, Scheme::Https),
    ];
    let (mut a, mut b) = (0, 0);
    for _ in 0..4 {
        match selector.select(id, &endpoints).await.unwrap().endpoint.port {
            10001 => a += 1,
            10002 => b += 1,
            other => panic!("unexpected port: {other}"),
        }
    }
    assert!(a > 0 && b > 0);
}

#[tokio::test]
async fn invalidate_causes_rebuild() {
    let selector = PoolEndpointSelector::new(ssrf_off());
    let id = Uuid::new_v4();
    let v1 = vec![ep("127.0.0.1", 20001, Scheme::Https)];
    assert_eq!(selector.select(id, &v1).await.unwrap().endpoint.port, 20001);
    selector.invalidate(id);
    let v2 = vec![ep("127.0.0.1", 20002, Scheme::Https)];
    assert_eq!(selector.select(id, &v2).await.unwrap().endpoint.port, 20002);
}

/// ADR-0018: PERS-PROXY-002.
#[tokio::test]
async fn changed_endpoints_rebuild_without_invalidate() {
    let selector = PoolEndpointSelector::new(ssrf_off());
    let id = Uuid::new_v4();
    let cached_pool = || Arc::clone(&selector.cache.get(&id).unwrap().pool);

    let v1 = vec![ep("127.0.0.1", 20011, Scheme::Https)];
    assert_eq!(selector.select(id, &v1).await.unwrap().endpoint.port, 20011);
    let pool_v1 = cached_pool();

    // Another replica changed the endpoints; no local invalidate() call.
    let v2 = vec![ep("127.0.0.1", 20012, Scheme::Https)];
    assert_eq!(selector.select(id, &v2).await.unwrap().endpoint.port, 20012);
    let pool_v2 = cached_pool();
    assert!(!Arc::ptr_eq(&pool_v1, &pool_v2), "a changed list rebuilds");

    assert_eq!(selector.select(id, &v2).await.unwrap().endpoint.port, 20012);
    assert!(
        Arc::ptr_eq(&pool_v2, &cached_pool()),
        "the same list reuses"
    );
}

#[tokio::test]
async fn select_preserves_endpoint_and_returns_resolved_addr() {
    let selector = PoolEndpointSelector::new(ssrf_off());
    let id = Uuid::new_v4();
    let endpoints = vec![ep("127.0.0.1", 30001, Scheme::Http)];
    let selected = selector.select(id, &endpoints).await.unwrap();
    assert_eq!(selected.endpoint, endpoints[0]);
    assert_eq!(
        selected.resolved_addr,
        Some("127.0.0.1:30001".parse().unwrap())
    );
}

#[tokio::test]
async fn select_resolves_hostname_but_keeps_original_endpoint() {
    let selector = PoolEndpointSelector::new(ssrf_off());
    let id = Uuid::new_v4();
    let endpoints = vec![ep("localhost", 50001, Scheme::Https)];
    let selected = selector.select(id, &endpoints).await.unwrap();
    assert_eq!(selected.endpoint.host, "localhost");
    assert_eq!(selected.endpoint.port, 50001);
    assert_eq!(selected.endpoint.scheme, Scheme::Https);
    assert!(selected.resolved_addr.unwrap().ip().is_loopback());
}

#[tokio::test]
async fn select_empty_endpoints_returns_err() {
    let selector = PoolEndpointSelector::new(ssrf_off());
    let id = Uuid::new_v4();
    assert!(selector.select(id, &[]).await.is_err());
    assert!(!selector.cache.contains_key(&id), "no cache entry");
}

#[tokio::test]
async fn select_unresolvable_endpoints_returns_no_backends() {
    let selector = PoolEndpointSelector::new(ssrf_off());
    let endpoints = vec![ep("this.host.does.not.exist.invalid", 443, Scheme::Https)];
    let err = selector
        .select(Uuid::new_v4(), &endpoints)
        .await
        .unwrap_err();
    assert!(matches!(err, SelectionError::NoBackendsResolved));
}

/// Dropping a pool ends its background task.
#[tokio::test(start_paused = true)]
async fn invalidate_stops_the_background_task() {
    let selector = PoolEndpointSelector::new(ssrf_off());
    let id = Uuid::new_v4();
    selector
        .select(id, &[ep("127.0.0.1", 61001, Scheme::Https)])
        .await
        .unwrap();
    let pool = Arc::downgrade(&selector.cache.get(&id).unwrap().pool);
    selector.invalidate(id);
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert_eq!(pool.strong_count(), 0, "the pool and its task are gone");
}

#[tokio::test(start_paused = true)]
async fn idle_sweep_drops_only_unselected_pools() {
    let selector = PoolEndpointSelector::new(ssrf_off());
    let (idle, used) = (Uuid::new_v4(), Uuid::new_v4());
    let idle_eps = [ep("127.0.0.1", 61011, Scheme::Https)];
    let used_eps = [ep("127.0.0.1", 61012, Scheme::Https)];
    selector.select(idle, &idle_eps).await.unwrap();
    selector.select(used, &used_eps).await.unwrap();
    let idle_pool = Arc::downgrade(&selector.cache.get(&idle).unwrap().pool);
    let used_pool = Arc::clone(&selector.cache.get(&used).unwrap().pool);

    tokio::time::sleep(Duration::from_secs(4)).await;
    let _ = selector.select(used, &used_eps).await;
    tokio::time::sleep(Duration::from_secs(4)).await;

    assert_eq!(selector.evict_idle(Duration::from_secs(5)), 1);
    assert!(selector.cache.get(&idle).is_none(), "the idle pool went");
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert_eq!(idle_pool.strong_count(), 0, "its task ended");
    assert!(
        Arc::ptr_eq(&used_pool, &selector.cache.get(&used).unwrap().pool),
        "the used pool stays"
    );
}

#[tokio::test(start_paused = true)]
async fn idle_sweeper_drops_idle_pools_and_stops_on_cancel() {
    let selector = Arc::new(PoolEndpointSelector::new(ssrf_off()));
    let id = Uuid::new_v4();
    selector
        .select(id, &[ep("127.0.0.1", 61021, Scheme::Https)])
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    let sweep = tokio::spawn(sweep_idle_pools(
        Arc::downgrade(&selector),
        Duration::from_secs(2),
        Duration::from_secs(5),
        cancel.clone(),
    ));

    // Ticks at 0, 2 and 4 keep the pool; the one at 6 drops it.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(selector.cache.get(&id).is_some(), "not idle yet");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(selector.cache.get(&id).is_none(), "dropped once idle");

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), sweep)
        .await
        .expect("the sweep ends")
        .unwrap();
}

#[tokio::test]
async fn selector_ssrf_blocks_loopback() {
    let selector = PoolEndpointSelector::new(ssrf_on());
    let endpoints = vec![ep("127.0.0.1", 30001, Scheme::Http)];
    let err = selector
        .select(Uuid::new_v4(), &endpoints)
        .await
        .unwrap_err();
    assert!(matches!(err, SelectionError::NoBackendsResolved));
}

#[tokio::test]
async fn selector_ssrf_blocks_localhost_hostname() {
    let selector = PoolEndpointSelector::new(ssrf_on());
    let endpoints = vec![ep("localhost", 9999, Scheme::Https)];
    let err = selector
        .select(Uuid::new_v4(), &endpoints)
        .await
        .unwrap_err();
    assert!(matches!(err, SelectionError::NoBackendsResolved));
}

#[tokio::test]
async fn selector_ssrf_off_allows_loopback() {
    let selector = PoolEndpointSelector::new(ssrf_off());
    let endpoints = vec![ep("127.0.0.1", 30001, Scheme::Http)];
    assert!(selector.select(Uuid::new_v4(), &endpoints).await.is_ok());
}
