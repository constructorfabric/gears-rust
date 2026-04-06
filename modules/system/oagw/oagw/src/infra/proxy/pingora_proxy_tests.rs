use super::*;
use crate::domain::model::{Endpoint, Scheme};

fn ep(host: &str, port: u16, scheme: Scheme) -> Endpoint {
    Endpoint {
        scheme,
        host: host.to_string(),
        port,
    }
}

// Note: PingoraBackendSelector uses Pingora's LoadBalancer which resolves
// addresses via ToSocketAddrs during construction. Tests must use real IP
// addresses (e.g. 127.0.0.1) with distinct ports to differentiate endpoints.

#[tokio::test]
async fn select_round_robin_distribution() {
    let selector = PingoraEndpointSelector::new();
    let id = Uuid::new_v4();
    let endpoints = vec![
        ep("127.0.0.1", 10001, Scheme::Https),
        ep("127.0.0.1", 10002, Scheme::Https),
    ];

    let mut port_a = 0u32;
    let mut port_b = 0u32;
    for _ in 0..4 {
        let selected = selector.select(id, &endpoints).await.unwrap();
        match selected.endpoint.port {
            10001 => port_a += 1,
            10002 => port_b += 1,
            other => panic!("unexpected port: {other}"),
        }
    }
    assert!(port_a > 0, "port 10001 should be selected at least once");
    assert!(port_b > 0, "port 10002 should be selected at least once");
}

#[tokio::test]
async fn invalidate_causes_rebuild() {
    let selector = PingoraEndpointSelector::new();
    let id = Uuid::new_v4();

    let v1 = vec![ep("127.0.0.1", 20001, Scheme::Https)];
    let selected = selector.select(id, &v1).await.unwrap();
    assert_eq!(selected.endpoint.port, 20001);

    selector.invalidate(id);

    let v2 = vec![ep("127.0.0.1", 20002, Scheme::Https)];
    let selected = selector.select(id, &v2).await.unwrap();
    assert_eq!(selected.endpoint.port, 20002);
}

#[tokio::test]
async fn select_single_endpoint() {
    let selector = PingoraEndpointSelector::new();
    let id = Uuid::new_v4();
    let endpoints = vec![ep("127.0.0.1", 30001, Scheme::Http)];

    let selected = selector.select(id, &endpoints).await.unwrap();
    assert_eq!(selected.endpoint.host, "127.0.0.1");
    assert_eq!(selected.endpoint.port, 30001);
    assert_eq!(selected.endpoint.scheme, Scheme::Http);
}

/// Endpoints in an upstream share scheme/port (by design).
/// Verify the scheme survives the Pingora Backend round-trip.
#[tokio::test]
async fn select_preserves_scheme() {
    let selector = PingoraEndpointSelector::new();
    let id = Uuid::new_v4();
    // All endpoints share the same scheme (upstream-level invariant).
    // Use different ports to distinguish endpoints.
    let endpoints = vec![
        ep("127.0.0.1", 40001, Scheme::Https),
        ep("127.0.0.1", 40002, Scheme::Https),
    ];

    let mut found_1 = false;
    let mut found_2 = false;
    for _ in 0..20 {
        let selected = selector.select(id, &endpoints).await.unwrap();
        assert_eq!(
            selected.endpoint.scheme,
            Scheme::Https,
            "scheme must be preserved"
        );
        assert_eq!(
            selected.endpoint.host, "127.0.0.1",
            "host must be preserved"
        );
        match selected.endpoint.port {
            40001 => found_1 = true,
            40002 => found_2 = true,
            other => panic!("unexpected port: {other}"),
        }
        if found_1 && found_2 {
            break;
        }
    }
    assert!(found_1, "should have selected port 40001");
    assert!(found_2, "should have selected port 40002");
}

/// P1 #5: Hostname-based endpoints are resolved via DNS so the reverse
/// lookup after select() works. "localhost" resolves to 127.0.0.1 which
/// must match the resolved key in endpoints_by_addr.
#[tokio::test]
async fn select_resolves_hostname_for_reverse_lookup() {
    let selector = PingoraEndpointSelector::new();
    let id = Uuid::new_v4();
    // Use "localhost" — a hostname that resolves to 127.0.0.1.
    let endpoints = vec![ep("localhost", 50001, Scheme::Https)];

    let selected = selector.select(id, &endpoints).await;
    assert!(
        selected.is_some(),
        "select should succeed for hostname-based endpoint"
    );
    let selected = selected.unwrap();
    // The returned endpoint must match the original — host stays "localhost".
    assert_eq!(selected.endpoint.host, "localhost");
    assert_eq!(selected.endpoint.port, 50001);
    assert_eq!(selected.endpoint.scheme, Scheme::Https);
}

// -- DnsDiscovery unit tests --

fn make_addr_map() -> AddrMap {
    Arc::new(ArcSwap::from_pointee(HashMap::new()))
}

/// resolve() with IP-only endpoints produces backends and a correct
/// reverse-lookup map without any DNS syscalls.
#[tokio::test]
async fn dns_discovery_resolve_ip_endpoints() {
    let addr_map = make_addr_map();
    let endpoints = vec![
        ep("127.0.0.1", 8001, Scheme::Https),
        ep("127.0.0.1", 8002, Scheme::Https),
    ];
    let discovery = DnsDiscovery::new(endpoints, addr_map);

    let (backends, map) = discovery.resolve().await;

    assert_eq!(backends.len(), 2, "should produce 2 backends");
    assert_eq!(map.len(), 2, "should produce 2 map entries");
    // Verify reverse lookup maps back to original endpoints.
    assert_eq!(map.get("127.0.0.1:8001").unwrap().port, 8001);
    assert_eq!(map.get("127.0.0.1:8002").unwrap().port, 8002);
}

/// resolve() with hostname endpoints resolves DNS and maps the resolved
/// IP back to the original hostname-bearing Endpoint.
#[tokio::test]
async fn dns_discovery_resolve_hostname_endpoints() {
    let addr_map = make_addr_map();
    let endpoints = vec![ep("localhost", 9001, Scheme::Https)];
    let discovery = DnsDiscovery::new(endpoints, addr_map);

    let (backends, map) = discovery.resolve().await;

    assert!(!backends.is_empty(), "localhost should resolve");
    // The map should contain the resolved IP, mapping to host="localhost".
    let first_ep = map.values().next().unwrap();
    assert_eq!(first_ep.host, "localhost");
    assert_eq!(first_ep.port, 9001);
}

/// discover() atomically updates the shared AddrMap.
#[tokio::test]
async fn dns_discovery_discover_updates_addr_map() {
    let addr_map = make_addr_map();
    assert!(addr_map.load().is_empty(), "addr_map should start empty");

    let endpoints = vec![
        ep("127.0.0.1", 7001, Scheme::Https),
        ep("127.0.0.1", 7002, Scheme::Https),
    ];
    let discovery = DnsDiscovery::new(endpoints, addr_map.clone());

    let (backends, _health) = discovery.discover().await.unwrap();

    assert_eq!(backends.len(), 2);
    let map = addr_map.load();
    assert_eq!(
        map.len(),
        2,
        "addr_map should be populated after discover()"
    );
    assert_eq!(map.get("127.0.0.1:7001").unwrap().port, 7001);
    assert_eq!(map.get("127.0.0.1:7002").unwrap().port, 7002);
}

/// Calling discover() again replaces the addr_map atomically.
/// Simulates what happens when DNS results change between cycles.
#[tokio::test]
async fn dns_discovery_discover_replaces_addr_map() {
    let addr_map = make_addr_map();
    let endpoints = vec![ep("127.0.0.1", 6001, Scheme::Http)];
    let discovery = DnsDiscovery::new(endpoints, addr_map.clone());

    // First discover.
    discovery.discover().await.unwrap();
    let map1 = Arc::clone(&addr_map.load());
    assert_eq!(map1.len(), 1);

    // Second discover — same endpoints, but a fresh map instance.
    discovery.discover().await.unwrap();
    let map2 = addr_map.load();

    // Both maps have the same content but are different allocations.
    assert_eq!(map2.len(), 1);
    assert_eq!(map2.get("127.0.0.1:6001").unwrap().port, 6001);
    assert!(
        !Arc::ptr_eq(&map1, &map2),
        "discover should swap in a new map"
    );
}

/// resolve() with an unresolvable hostname falls back to the raw address
/// string and logs a warning (does not panic).
#[tokio::test]
async fn dns_discovery_resolve_unresolvable_hostname() {
    let addr_map = make_addr_map();
    // Use a hostname that will fail DNS resolution.
    let endpoints = vec![ep(
        "this.host.definitely.does.not.exist.invalid",
        443,
        Scheme::Https,
    )];
    let discovery = DnsDiscovery::new(endpoints, addr_map);

    let (backends, map) = discovery.resolve().await;

    // Fallback path: Backend::new with the raw string will also fail
    // because it's not a valid SocketAddr, so both should be empty.
    // This is correct — no valid backend can be created.
    assert!(
        (backends.is_empty() && map.is_empty()) || (!backends.is_empty() && !map.is_empty()),
        "either both empty (raw parse fails) or both populated (fallback succeeded)"
    );
}

/// select() returns None when the endpoint list is empty.
#[tokio::test]
async fn select_empty_endpoints_returns_none() {
    let selector = PingoraEndpointSelector::new();
    let id = Uuid::new_v4();

    let result = selector.select(id, &[]).await;
    assert!(result.is_none(), "empty endpoints should return None");
    assert!(
        !selector.cache.contains_key(&id),
        "no cache entry should be created"
    );
}

/// select() returns None when all endpoints fail DNS resolution
/// (build_entry returns None because addr_map stays empty).
#[tokio::test]
async fn select_unresolvable_endpoints_returns_none() {
    let selector = PingoraEndpointSelector::new();
    let id = Uuid::new_v4();

    let endpoints = vec![ep("this.host.does.not.exist.invalid", 443, Scheme::Https)];
    let result = selector.select(id, &endpoints).await;
    assert!(
        result.is_none(),
        "unresolvable endpoints should return None"
    );
}

/// After invalidate + re-select with different endpoints, the new
/// addr_map reflects the updated endpoints (simulates config change).
#[tokio::test]
async fn invalidate_rebuilds_with_new_addr_map() {
    let selector = PingoraEndpointSelector::new();
    let id = Uuid::new_v4();

    // Initial endpoints.
    let v1 = vec![ep("127.0.0.1", 60001, Scheme::Https)];
    let selected = selector.select(id, &v1).await.unwrap();
    assert_eq!(selected.endpoint.port, 60001);

    // Access the addr_map to verify it's populated.
    let entry = selector.cache.get(&id).unwrap();
    let map = entry.addr_map.load();
    assert!(map.contains_key("127.0.0.1:60001"));
    drop(entry);

    // Invalidate and re-select with different endpoints.
    selector.invalidate(id);

    let v2 = vec![ep("127.0.0.1", 60002, Scheme::Https)];
    let selected = selector.select(id, &v2).await.unwrap();
    assert_eq!(selected.endpoint.port, 60002);

    // New addr_map should only contain the new endpoint.
    let entry = selector.cache.get(&id).unwrap();
    let map = entry.addr_map.load();
    assert!(
        !map.contains_key("127.0.0.1:60001"),
        "old endpoint should be gone"
    );
    assert!(
        map.contains_key("127.0.0.1:60002"),
        "new endpoint should be present"
    );
}

// -- upstream_peer ALPN / TLS tests --
//
// These tests mirror the `upstream_peer` logic to verify the peer
// configuration without constructing a full Pingora Session. The
// logic under test is:
//   tls = matches!(scheme, Https | Wss | Wt)
//   alpn = if tls && !Wss { H2H1 } else { H1 }

/// Build an HttpPeer using the same logic as `upstream_peer`.
/// Uses a dummy IP (production resolves via `lookup_host`); the `host`
/// string is passed as the SNI, matching `upstream_peer` behaviour.
/// Build an `HttpPeer` using `select_alpn` for ALPN selection, matching
/// the production code path. Uses a default (enabled) protocol cache.
fn build_peer(scheme: Scheme, host: &str, port: u16) -> HttpPeer {
    let proxy = PingoraProxy::new(
        Duration::from_secs(10),
        Duration::from_secs(30),
        Duration::from_secs(3600),
    );
    let ep = ep(host, port, scheme);
    let tls = matches!(ep.scheme, Scheme::Https | Scheme::Wss | Scheme::Wt);
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut peer = HttpPeer::new(addr, tls, host.to_string());
    peer.options.alpn = proxy.select_alpn(&ep);
    peer
}

#[test]
fn alpn_https_uses_h2h1() {
    let peer = build_peer(Scheme::Https, "example.com", 443);
    assert!(peer.is_tls(), "HTTPS peer should use TLS");
    assert_eq!(
        peer.options.alpn,
        pingora_core::protocols::tls::ALPN::H2H1,
        "HTTPS should negotiate H2 with H1 fallback"
    );
}

#[test]
fn alpn_http_uses_h1() {
    let peer = build_peer(Scheme::Http, "example.com", 80);
    assert!(!peer.is_tls(), "HTTP peer should not use TLS");
    assert_eq!(
        peer.options.alpn,
        pingora_core::protocols::tls::ALPN::H1,
        "cleartext HTTP should use H1 only"
    );
}

#[test]
fn alpn_wss_uses_h1() {
    let peer = build_peer(Scheme::Wss, "example.com", 443);
    assert!(peer.is_tls(), "WSS peer should use TLS");
    assert_eq!(
        peer.options.alpn,
        pingora_core::protocols::tls::ALPN::H1,
        "WSS must use H1 (WebSocket requires HTTP/1.1 upgrade)"
    );
}

#[test]
fn alpn_wt_uses_h2h1() {
    let peer = build_peer(Scheme::Wt, "example.com", 443);
    assert!(peer.is_tls(), "WT peer should use TLS");
    assert_eq!(
        peer.options.alpn,
        pingora_core::protocols::tls::ALPN::H2H1,
        "WebTransport should negotiate H2 with H1 fallback"
    );
}

#[test]
fn peer_timeouts_propagate() {
    let proxy = PingoraProxy::new(
        Duration::from_secs(7),
        Duration::from_secs(15),
        Duration::from_secs(3600),
    );
    // Verify timeouts are stored correctly on the proxy.
    assert_eq!(proxy.connect_timeout, Duration::from_secs(7));
    assert_eq!(proxy.read_timeout, Duration::from_secs(15));
}

#[test]
fn populate_from_headers_parses_resolved_addr() {
    let mut ctx = ProxyCtx::default();
    let mut headers = http::HeaderMap::new();
    let upstream_id = Uuid::new_v4();
    headers.insert(H_ENDPOINT_HOST, "api.example.com".parse().unwrap());
    headers.insert(H_ENDPOINT_PORT, "8443".parse().unwrap());
    headers.insert(H_ENDPOINT_SCHEME, "https".parse().unwrap());
    headers.insert(H_INSTANCE_URI, "/test/instance".parse().unwrap());
    headers.insert(H_UPSTREAM_ID, upstream_id.to_string().parse().unwrap());
    headers.insert(H_RESOLVED_ADDR, "93.184.216.34:8443".parse().unwrap());

    ctx.populate_from_headers(&headers);

    assert_eq!(ctx.endpoint.host, "api.example.com");
    assert_eq!(ctx.endpoint.port, 8443);
    assert_eq!(ctx.endpoint.scheme, Scheme::Https);
    assert_eq!(ctx.instance_uri, "/test/instance");
    assert_eq!(ctx.upstream_id, Some(upstream_id));
    let expected: std::net::SocketAddr = "93.184.216.34:8443".parse().unwrap();
    assert_eq!(ctx.resolved_addr, Some(expected));
}

#[test]
fn populate_from_headers_missing_resolved_addr_leaves_none() {
    let mut ctx = ProxyCtx::default();
    let mut headers = http::HeaderMap::new();
    headers.insert(H_ENDPOINT_HOST, "api.example.com".parse().unwrap());
    headers.insert(H_ENDPOINT_PORT, "443".parse().unwrap());
    // No H_RESOLVED_ADDR header.

    ctx.populate_from_headers(&headers);

    assert_eq!(ctx.endpoint.host, "api.example.com");
    assert!(ctx.resolved_addr.is_none());
}

#[test]
fn populate_from_headers_invalid_resolved_addr_leaves_none() {
    let mut ctx = ProxyCtx::default();
    let mut headers = http::HeaderMap::new();
    headers.insert(H_RESOLVED_ADDR, "not-an-addr".parse().unwrap());

    ctx.populate_from_headers(&headers);

    assert!(ctx.resolved_addr.is_none());
}

#[tokio::test]
async fn select_populates_resolved_addr() {
    let selector = PingoraEndpointSelector::new();
    let id = Uuid::new_v4();
    // IP-based endpoint — resolved_addr should be populated.
    let endpoints = vec![ep("127.0.0.1", 30001, Scheme::Http)];

    let selected = selector.select(id, &endpoints).await.unwrap();
    assert!(
        selected.resolved_addr.is_some(),
        "resolved_addr should be populated for IP endpoint"
    );
    assert_eq!(selected.resolved_addr.unwrap().port(), 30001);
}

// -----------------------------------------------------------------------
// ProtocolVersionCache tests
// -----------------------------------------------------------------------

#[test]
fn protocol_cache_miss_returns_none() {
    let cache = ProtocolVersionCache::new(Duration::from_secs(3600));
    let endpoint = ep("example.com", 443, Scheme::Https);
    assert_eq!(cache.get(&endpoint), None);
}

#[test]
fn protocol_cache_insert_h2_then_get() {
    let cache = ProtocolVersionCache::new(Duration::from_secs(3600));
    let endpoint = ep("example.com", 443, Scheme::Https);
    cache.insert(&endpoint, CachedProtocol::Http2);
    assert_eq!(cache.get(&endpoint), Some(CachedProtocol::Http2));
}

#[test]
fn protocol_cache_insert_h1_then_get() {
    let cache = ProtocolVersionCache::new(Duration::from_secs(3600));
    let endpoint = ep("example.com", 443, Scheme::Https);
    cache.insert(&endpoint, CachedProtocol::Http1Only);
    assert_eq!(cache.get(&endpoint), Some(CachedProtocol::Http1Only));
}

#[test]
fn protocol_cache_ttl_expiry() {
    let cache = ProtocolVersionCache::new(Duration::from_millis(1));
    let endpoint = ep("example.com", 443, Scheme::Https);
    cache.insert(&endpoint, CachedProtocol::Http2);
    // Let the entry expire.
    std::thread::sleep(Duration::from_millis(5));
    assert_eq!(
        cache.get(&endpoint),
        None,
        "expired entry should return None"
    );
}

#[test]
fn protocol_cache_evict() {
    let cache = ProtocolVersionCache::new(Duration::from_secs(3600));
    let endpoint = ep("example.com", 443, Scheme::Https);
    cache.insert(&endpoint, CachedProtocol::Http2);
    cache.evict(&endpoint);
    assert_eq!(cache.get(&endpoint), None);
}

#[test]
fn protocol_cache_key_format() {
    let endpoint = ep("example.com", 443, Scheme::Https);
    assert_eq!(
        ProtocolVersionCache::cache_key(&endpoint),
        "https://example.com:443"
    );

    let endpoint_wt = ep("api.test.io", 8443, Scheme::Wt);
    assert_eq!(
        ProtocolVersionCache::cache_key(&endpoint_wt),
        "wt://api.test.io:8443"
    );
}

#[test]
fn protocol_cache_different_ports_distinct() {
    let cache = ProtocolVersionCache::new(Duration::from_secs(3600));
    let ep_a = ep("example.com", 443, Scheme::Https);
    let ep_b = ep("example.com", 8443, Scheme::Https);
    cache.insert(&ep_a, CachedProtocol::Http2);
    cache.insert(&ep_b, CachedProtocol::Http1Only);
    assert_eq!(cache.get(&ep_a), Some(CachedProtocol::Http2));
    assert_eq!(cache.get(&ep_b), Some(CachedProtocol::Http1Only));
}

#[test]
fn protocol_cache_disabled_when_ttl_zero() {
    let cache = ProtocolVersionCache::new(Duration::ZERO);
    assert!(!cache.is_enabled());
    let endpoint = ep("example.com", 443, Scheme::Https);
    cache.insert(&endpoint, CachedProtocol::Http2);
    assert_eq!(
        cache.get(&endpoint),
        None,
        "disabled cache should always miss"
    );
}

// -----------------------------------------------------------------------
// select_alpn tests
// -----------------------------------------------------------------------

#[test]
fn select_alpn_uses_h2_for_cached_h2_host() {
    let proxy = PingoraProxy::new(
        Duration::from_secs(10),
        Duration::from_secs(30),
        Duration::from_secs(3600),
    );
    let endpoint = ep("example.com", 443, Scheme::Https);
    proxy
        .protocol_cache
        .insert(&endpoint, CachedProtocol::Http2);
    assert_eq!(
        proxy.select_alpn(&endpoint),
        pingora_core::protocols::tls::ALPN::H2
    );
}

#[test]
fn select_alpn_uses_h1_for_cached_h1_host() {
    let proxy = PingoraProxy::new(
        Duration::from_secs(10),
        Duration::from_secs(30),
        Duration::from_secs(3600),
    );
    let endpoint = ep("example.com", 443, Scheme::Https);
    proxy
        .protocol_cache
        .insert(&endpoint, CachedProtocol::Http1Only);
    assert_eq!(
        proxy.select_alpn(&endpoint),
        pingora_core::protocols::tls::ALPN::H1
    );
}

#[test]
fn select_alpn_default_h2h1_for_uncached() {
    let proxy = PingoraProxy::new(
        Duration::from_secs(10),
        Duration::from_secs(30),
        Duration::from_secs(3600),
    );
    let endpoint = ep("example.com", 443, Scheme::Https);
    assert_eq!(
        proxy.select_alpn(&endpoint),
        pingora_core::protocols::tls::ALPN::H2H1
    );
}

#[test]
fn select_alpn_wss_always_h1() {
    let proxy = PingoraProxy::new(
        Duration::from_secs(10),
        Duration::from_secs(30),
        Duration::from_secs(3600),
    );
    let endpoint = ep("example.com", 443, Scheme::Wss);
    // Even if someone were to insert an entry for this host, WSS must use H1.
    proxy
        .protocol_cache
        .insert(&endpoint, CachedProtocol::Http2);
    assert_eq!(
        proxy.select_alpn(&endpoint),
        pingora_core::protocols::tls::ALPN::H1
    );
}

#[test]
fn select_alpn_h2h1_when_cache_disabled() {
    let proxy = PingoraProxy::new(
        Duration::from_secs(10),
        Duration::from_secs(30),
        Duration::ZERO, // disabled
    );
    let endpoint = ep("example.com", 443, Scheme::Https);
    // Insert would be a no-op, but call it anyway to verify.
    proxy
        .protocol_cache
        .insert(&endpoint, CachedProtocol::Http2);
    assert_eq!(
        proxy.select_alpn(&endpoint),
        pingora_core::protocols::tls::ALPN::H2H1,
        "disabled cache should fall through to H2H1"
    );
}
