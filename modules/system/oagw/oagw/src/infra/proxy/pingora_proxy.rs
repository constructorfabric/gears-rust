use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use bytes::Bytes;
use dashmap::DashMap;
use pingora_core::protocols::Digest;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_http::ResponseHeader;
use pingora_load_balancing::discovery::ServiceDiscovery;
use pingora_load_balancing::health_check::TcpHealthCheck;
use pingora_load_balancing::selection::RoundRobin;
use pingora_load_balancing::{Backend, Backends, LoadBalancer};
use pingora_proxy::{HttpProxy, ProxyHttp, Session, http_proxy};
use tokio::sync::watch;
use tracing::{info, warn};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::model::{Endpoint, Scheme};
use crate::domain::services::{EndpointSelector, SelectedEndpoint};
use modkit::api::Problem;

// ---------------------------------------------------------------------------
// Internal header names (D9)
// ---------------------------------------------------------------------------

const INTERNAL_PREFIX: &str = "x-oagw-internal-";

pub(crate) const H_UPSTREAM_ID: &str = "x-oagw-internal-upstream-id";
pub(crate) const H_ENDPOINT_HOST: &str = "x-oagw-internal-endpoint-host";
pub(crate) const H_ENDPOINT_PORT: &str = "x-oagw-internal-endpoint-port";
pub(crate) const H_ENDPOINT_SCHEME: &str = "x-oagw-internal-endpoint-scheme";
pub(crate) const H_INSTANCE_URI: &str = "x-oagw-internal-instance-uri";
pub(crate) const H_RESOLVED_ADDR: &str = "x-oagw-internal-resolved-addr";

/// Hop-by-hop headers that must not be forwarded in responses (mirrors headers.rs).
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

// ---------------------------------------------------------------------------
// PingoraProxy — ProxyHttp implementation (D3)
// ---------------------------------------------------------------------------

pub struct PingoraProxy {
    connect_timeout: Duration,
    read_timeout: Duration,
    /// When true, skip TLS certificate verification for upstream connections.
    /// **Test use only** — allows self-signed certs in integration tests.
    skip_upstream_tls_verify: bool,
}

impl PingoraProxy {
    pub fn new(connect_timeout: Duration, read_timeout: Duration) -> Self {
        Self {
            connect_timeout,
            read_timeout,
            skip_upstream_tls_verify: false,
        }
    }

    /// Skip upstream TLS certificate verification. **Test use only.**
    #[must_use]
    #[allow(dead_code)]
    pub fn with_skip_upstream_tls_verify(mut self, allow: bool) -> Self {
        self.skip_upstream_tls_verify = allow;
        self
    }
}

/// Construct an `HttpProxy` from a `ServerConf` and `PingoraProxy`.
pub fn new_http_proxy(
    conf: &Arc<pingora_core::server::configuration::ServerConf>,
    inner: PingoraProxy,
) -> HttpProxy<PingoraProxy> {
    http_proxy(conf, inner)
}

// ---------------------------------------------------------------------------
// DNS-aware ServiceDiscovery (D2)
// ---------------------------------------------------------------------------

/// Shared reverse-lookup map: resolved `"ip:port"` → original `Endpoint`.
///
/// Updated atomically by [`DnsDiscovery::discover`] each cycle so that
/// `select()` can map Pingora's resolved `Backend` address back to the
/// domain-level `Endpoint` (which carries scheme, original hostname, port).
type AddrMap = Arc<ArcSwap<HashMap<String, Endpoint>>>;

/// [`ServiceDiscovery`] implementation that re-resolves hostnames on every
/// `discover()` call. IP-only endpoints are passed through without DNS.
///
/// On each cycle the reverse-lookup [`AddrMap`] is rebuilt so that any DNS
/// changes (failover, blue-green) are immediately reflected.
struct DnsDiscovery {
    /// Original domain-level endpoints (hostname/IP + port + scheme).
    endpoints: Vec<Endpoint>,
    /// Shared map updated on each `discover()` cycle.
    addr_map: AddrMap,
}

impl DnsDiscovery {
    fn new(endpoints: Vec<Endpoint>, addr_map: AddrMap) -> Box<Self> {
        Box::new(Self {
            endpoints,
            addr_map,
        })
    }

    /// Resolve endpoints to `Backend`s and rebuild the reverse-lookup map.
    ///
    /// Uses async `tokio::net::lookup_host` to avoid blocking the Tokio
    /// worker thread during DNS resolution.
    async fn resolve(&self) -> (BTreeSet<Backend>, HashMap<String, Endpoint>) {
        let mut backends = BTreeSet::new();
        let mut map = HashMap::with_capacity(self.endpoints.len());

        for ep in &self.endpoints {
            let addr_str = format!("{}:{}", ep.host, ep.port);

            let resolved = tokio::net::lookup_host(addr_str.clone()).await;
            match resolved {
                Ok(addrs) => {
                    for sock in addrs {
                        let key = sock.to_string();
                        if let Ok(b) = Backend::new(&key) {
                            backends.insert(b);
                            // First endpoint wins if multiple resolve to the same IP.
                            map.entry(key).or_insert_with(|| ep.clone());
                        }
                    }
                }
                Err(e) => {
                    warn!(addr = %addr_str, error = %e, "DNS resolution failed, using original address");
                    if let Ok(b) = Backend::new(&addr_str) {
                        backends.insert(b);
                        map.entry(addr_str).or_insert_with(|| ep.clone());
                    }
                }
            }
        }

        (backends, map)
    }
}

#[async_trait]
impl ServiceDiscovery for DnsDiscovery {
    async fn discover(&self) -> pingora_core::Result<(BTreeSet<Backend>, HashMap<u64, bool>)> {
        let (backends, new_map) = self.resolve().await;

        // Atomically swap the reverse-lookup map so concurrent select() calls
        // see the latest DNS resolution.
        self.addr_map.store(Arc::new(new_map));

        Ok((backends, HashMap::new()))
    }
}

// ---------------------------------------------------------------------------
// PingoraEndpointSelector — default in-process BackendSelector (D2, D3)
// ---------------------------------------------------------------------------

/// Cache entry: load balancer + shared reverse-lookup map + shutdown handle.
struct LbEntry {
    lb: Arc<LoadBalancer<RoundRobin>>,
    /// Shared reverse-lookup map updated by [`DnsDiscovery::discover`].
    addr_map: AddrMap,
    /// Dropping this sender signals the background update task to stop.
    _shutdown_tx: watch::Sender<bool>,
}

/// Default in-process `EndpointSelector` backed by Pingora's `LoadBalancer<RoundRobin>`
/// with DNS-aware service discovery.
///
/// Lazily constructs a `LoadBalancer` per upstream on first `select()` call,
/// caches it in a `DashMap`, and attaches a `TcpHealthCheck` with 10s interval.
/// DNS re-resolution runs every 30s via the [`DnsDiscovery`] `ServiceDiscovery`
/// implementation. Dropping the cache entry (via `invalidate()`) stops the
/// background task.
pub struct PingoraEndpointSelector {
    cache: DashMap<Uuid, LbEntry>,
}

impl PingoraEndpointSelector {
    pub fn new() -> Self {
        Self {
            cache: DashMap::new(),
        }
    }

    /// Build a `LoadBalancer<RoundRobin>` from domain endpoints using
    /// [`DnsDiscovery`] for dynamic DNS re-resolution.
    ///
    /// DNS resolution uses async `tokio::net::lookup_host` to avoid blocking
    /// the Tokio worker thread.
    async fn build_entry(&self, endpoints: &[Endpoint]) -> Option<LbEntry> {
        let addr_map: AddrMap = Arc::new(ArcSwap::from_pointee(HashMap::new()));

        let mut backends = Backends::new(DnsDiscovery::new(endpoints.to_vec(), addr_map.clone()));
        backends.set_health_check(TcpHealthCheck::new());

        let mut lb = LoadBalancer::<RoundRobin>::from_backends(backends);
        lb.health_check_frequency = Some(Duration::from_secs(10));
        lb.update_frequency = Some(Duration::from_secs(30));

        // update() calls discover() which resolves DNS and populates both
        // the backend selector and the addr_map in a single pass.
        lb.update().await.ok()?;

        if addr_map.load().is_empty() {
            warn!("No backends resolved for endpoints, skipping LB creation");
            return None;
        }

        let lb = Arc::new(lb);

        // Delegate periodic discovery + health checks to Pingora's
        // BackgroundService implementation, which respects
        // update_frequency and health_check_frequency.
        // Dropping _shutdown_tx sets the watch to `true`, signaling stop.
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let lb_bg = lb.clone();
        tokio::spawn(async move {
            use pingora_core::services::background::BackgroundService;
            lb_bg.start(shutdown_rx).await;
        });

        Some(LbEntry {
            lb,
            addr_map,
            _shutdown_tx: shutdown_tx,
        })
    }
}

#[async_trait]
impl EndpointSelector for PingoraEndpointSelector {
    async fn select(&self, upstream_id: Uuid, endpoints: &[Endpoint]) -> Option<SelectedEndpoint> {
        // Fast path: LB already cached.
        if let Some(entry) = self.cache.get(&upstream_id) {
            let backend = entry.lb.select(b"", 256)?;
            let resolved_addr = backend.addr.as_inet();
            let addr_key = backend.addr.to_string();
            let map = entry.addr_map.load();
            let endpoint = map.get(&addr_key)?.clone();
            return Some(SelectedEndpoint {
                endpoint,
                resolved_addr: resolved_addr.copied(),
            });
        }

        // Slow path: build a new LB entry then atomically insert-if-absent.
        // Concurrent builders may race here; or_insert ensures only one wins
        // and losers are dropped (stopping their background task via _shutdown_tx).
        let entry = self.build_entry(endpoints).await?;
        let entry_ref = self.cache.entry(upstream_id).or_insert(entry);
        let backend = entry_ref.lb.select(b"", 256)?;
        let resolved_addr = backend.addr.as_inet();
        let addr_key = backend.addr.to_string();
        let map = entry_ref.addr_map.load();
        let endpoint = map.get(&addr_key)?.clone();
        Some(SelectedEndpoint {
            endpoint,
            resolved_addr: resolved_addr.copied(),
        })
    }

    fn invalidate(&self, upstream_id: Uuid) {
        // Removing the entry drops LbEntry, which drops _shutdown_tx,
        // which signals the background update task to stop.
        self.cache.remove(&upstream_id);
    }
}

// ---------------------------------------------------------------------------
// Per-request context (D3)
// ---------------------------------------------------------------------------

pub struct ProxyCtx {
    endpoint: Endpoint,
    instance_uri: String,
    /// Upstream that owns this endpoint (for diagnostic logs).
    upstream_id: Option<Uuid>,
    /// Pre-resolved socket address from the load balancer's DNS cache.
    /// When set, `upstream_peer` skips DNS and connects directly.
    resolved_addr: Option<std::net::SocketAddr>,
}

impl ProxyCtx {
    /// Populate context fields from internal headers.
    ///
    /// Extracted from `request_filter` so the parsing logic is unit-testable
    /// without constructing a full Pingora `Session`.
    fn populate_from_headers(&mut self, headers: &http::HeaderMap) {
        if let Some(v) = headers.get(H_ENDPOINT_HOST).and_then(|v| v.to_str().ok()) {
            self.endpoint.host = v.to_string();
        }
        if let Some(v) = headers.get(H_ENDPOINT_PORT).and_then(|v| v.to_str().ok())
            && let Ok(port) = v.parse()
        {
            self.endpoint.port = port;
        }
        if let Some(v) = headers.get(H_ENDPOINT_SCHEME).and_then(|v| v.to_str().ok()) {
            self.endpoint.scheme = match v {
                "http" => Scheme::Http,
                "https" => Scheme::Https,
                "wss" => Scheme::Wss,
                "wt" => Scheme::Wt,
                "grpc" => Scheme::Grpc,
                _ => Scheme::Https,
            };
        }
        if let Some(v) = headers.get(H_INSTANCE_URI).and_then(|v| v.to_str().ok()) {
            self.instance_uri = v.to_string();
        }
        if let Some(v) = headers.get(H_UPSTREAM_ID).and_then(|v| v.to_str().ok()) {
            self.upstream_id = v.parse().ok();
        }
        if let Some(v) = headers.get(H_RESOLVED_ADDR).and_then(|v| v.to_str().ok()) {
            self.resolved_addr = v.parse().ok();
        }
    }
}

impl Default for ProxyCtx {
    fn default() -> Self {
        Self {
            endpoint: Endpoint {
                scheme: Scheme::Https,
                host: String::new(),
                port: 443,
            },
            instance_uri: String::new(),
            upstream_id: None,
            resolved_addr: None,
        }
    }
}

// ---------------------------------------------------------------------------
// ProxyHttp trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl ProxyHttp for PingoraProxy {
    type CTX = ProxyCtx;

    fn new_ctx(&self) -> Self::CTX {
        ProxyCtx::default()
    }

    /// Extract internal context headers, populate `ProxyCtx`, strip them. (D9)
    async fn request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora_core::Result<bool> {
        ctx.populate_from_headers(&session.req_header().headers);

        // Strip all internal headers before forwarding.
        let to_remove: Vec<http::HeaderName> = session
            .req_header()
            .headers
            .keys()
            .filter(|k| k.as_str().starts_with(INTERNAL_PREFIX))
            .cloned()
            .collect();
        let req_mut = session.req_header_mut();
        for name in &to_remove {
            req_mut.remove_header(name);
        }

        Ok(false) // continue processing
    }

    /// Build `HttpPeer` from the resolved endpoint. (D3, D4, D7)
    ///
    /// Uses the pre-resolved `SocketAddr` from the load balancer's DNS cache
    /// when available, falling back to an explicit `lookup_host` otherwise.
    /// Both paths pass a `SocketAddr` to `HttpPeer::new`, avoiding the
    /// `unwrap()` panic on DNS failure in pingora-core 0.8.0. (See bug: https://github.com/cloudflare/pingora/issues/570)
    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora_core::Result<Box<HttpPeer>> {
        let ep = &ctx.endpoint;
        let tls = matches!(ep.scheme, Scheme::Https | Scheme::Wss | Scheme::Wt);

        let addr = match ctx.resolved_addr {
            Some(a) => a,
            None => {
                // Fallback: resolve DNS explicitly (single-endpoint bypass, target-host header).
                tokio::net::lookup_host((ep.host.as_str(), ep.port))
                    .await
                    .map_err(|e| {
                        warn!(upstream_id = ?ctx.upstream_id, host = %ep.host, port = ep.port, error = %e, "DNS resolution failed");
                        pingora_core::Error::because(
                            pingora_core::ErrorType::ConnectError,
                            "DNS resolution failed",
                            e,
                        )
                    })?
                    .next()
                    .ok_or_else(|| {
                        warn!(upstream_id = ?ctx.upstream_id, host = %ep.host, port = ep.port, "DNS returned no addresses");
                        pingora_core::Error::explain(
                            pingora_core::ErrorType::ConnectError,
                            format!("DNS returned no addresses for {}:{}", ep.host, ep.port),
                        )
                    })?
            }
        };

        // Pass SocketAddr directly — no DNS inside HttpPeer::new.
        let mut peer = HttpPeer::new(addr, tls, ep.host.clone());

        peer.options.connection_timeout = Some(self.connect_timeout);
        peer.options.read_timeout = Some(self.read_timeout);
        peer.options.idle_timeout = Some(Duration::from_secs(90));

        // ALPN: H2H1 for HTTPS, H1 for WebSocket and cleartext.
        peer.options.alpn = if tls && !matches!(ep.scheme, Scheme::Wss) {
            pingora_core::protocols::tls::ALPN::H2H1
        } else {
            pingora_core::protocols::tls::ALPN::H1
        };

        if self.skip_upstream_tls_verify {
            peer.options.verify_cert = false;
            peer.options.verify_hostname = false;
        }

        Ok(Box::new(peer))
    }

    /// No-op — headers are already prepared by proxy_request() steps 3–5. (D3)
    async fn upstream_request_filter(
        &self,
        _session: &mut Session,
        _upstream_request: &mut pingora_http::RequestHeader,
        _ctx: &mut Self::CTX,
    ) -> pingora_core::Result<()> {
        Ok(())
    }

    /// Sanitize response headers: strip hop-by-hop and x-oagw-* headers. (D3)
    async fn upstream_response_filter(
        &self,
        _session: &mut Session,
        upstream_response: &mut ResponseHeader,
        _ctx: &mut Self::CTX,
    ) -> pingora_core::Result<()> {
        let status = upstream_response.status;
        let content_type = upstream_response
            .headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("<none>");
        tracing::debug!(
            %status,
            content_type,
            "upstream response received"
        );

        // Strip Connection-nominated headers.
        if let Some(conn_value) = upstream_response
            .headers
            .get("connection")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
        {
            for token in conn_value.split(',') {
                let name = token.trim();
                if !name.is_empty() {
                    upstream_response.remove_header(name);
                }
            }
        }

        // Strip static hop-by-hop headers.
        for name in HOP_BY_HOP {
            upstream_response.remove_header(*name);
        }

        // Strip x-oagw-* internal headers.
        let to_remove: Vec<http::HeaderName> = upstream_response
            .headers
            .keys()
            .filter(|k| k.as_str().starts_with("x-oagw-"))
            .cloned()
            .collect();
        for name in &to_remove {
            upstream_response.remove_header(name);
        }

        Ok(())
    }

    // No fail_to_connect override: OAGW does not retry on connection failure.
    // Per DESIGN.md §311 and scenario 12.6, upstream sees exactly one request
    // attempt. Connection-establishment retries would violate this invariant.

    /// Reconnect on stale pooled connection errors for idempotent methods.
    ///
    /// When Pingora reuses a pooled connection that was closed server-side
    /// (e.g. `Connection: close`, idle timeout), the request *likely* has not
    /// been sent — but this is not guaranteed (partial header write before
    /// RST is possible). Reconnecting is therefore safe only for idempotent
    /// methods (RFC 9110 §9.2.2). Non-idempotent methods (POST, PATCH) are
    /// not retried, consistent with DESIGN.md and scenario 12.6.
    fn error_while_proxy(
        &self,
        _peer: &HttpPeer,
        session: &mut Session,
        mut e: Box<pingora_core::Error>,
        _ctx: &mut Self::CTX,
        client_reused: bool,
    ) -> Box<pingora_core::Error> {
        if client_reused {
            let idempotent = matches!(
                session.req_header().method,
                http::Method::GET
                    | http::Method::HEAD
                    | http::Method::PUT
                    | http::Method::DELETE
                    | http::Method::OPTIONS
            );
            e.retry.decide_reuse(idempotent);
        }
        e
    }

    /// Map Pingora error types to `DomainError`, then use the canonical
    /// `DomainError → Problem` pipeline to write an RFC 9457 response. (D6)
    async fn fail_to_proxy(
        &self,
        session: &mut Session,
        e: &pingora_core::Error,
        ctx: &mut Self::CTX,
    ) -> pingora_proxy::FailToProxy {
        let instance = ctx.instance_uri.clone();
        let domain_err = match &e.etype {
            pingora_core::ErrorType::ConnectTimedout => DomainError::ConnectionTimeout {
                detail: "upstream connection timed out".into(),
                instance,
            },
            pingora_core::ErrorType::ReadTimedout | pingora_core::ErrorType::WriteTimedout => {
                DomainError::RequestTimeout {
                    detail: format!(
                        "upstream {} timed out",
                        if matches!(e.etype, pingora_core::ErrorType::ReadTimedout) {
                            "read"
                        } else {
                            "write"
                        }
                    ),
                    instance,
                }
            }
            pingora_core::ErrorType::H2Error | pingora_core::ErrorType::H2Downgrade => {
                DomainError::ProtocolError {
                    detail: "upstream HTTP/2 error".into(),
                    instance,
                }
            }
            _ => DomainError::DownstreamError {
                detail: match &e.etype {
                    pingora_core::ErrorType::ConnectRefused => "upstream connection refused",
                    pingora_core::ErrorType::TLSHandshakeFailure
                    | pingora_core::ErrorType::TLSHandshakeTimedout => {
                        "upstream TLS handshake failed"
                    }
                    pingora_core::ErrorType::InvalidCert => "upstream certificate invalid",
                    pingora_core::ErrorType::ConnectionClosed => "upstream connection closed",
                    _ => "upstream error",
                }
                .into(),
                instance,
            },
        };

        let problem: Problem = domain_err.into();
        let status = problem.status.as_u16();
        let body_bytes = Bytes::from(serde_json::to_vec(&problem).unwrap_or_default());

        if let Ok(mut resp) = ResponseHeader::build(status, Some(body_bytes.len())) {
            let _ = resp.insert_header("content-type", "application/problem+json");
            let _ = resp.insert_header("x-oagw-error-source", "gateway");
            let _ = session.write_response_header(Box::new(resp), false).await;
            let _ = session.write_response_body(Some(body_bytes), true).await;
        } else {
            let _ = session.respond_error(status).await;
        }

        pingora_proxy::FailToProxy {
            error_code: 0,
            can_reuse_downstream: false,
        }
    }

    /// Log upstream connection info. (D3)
    async fn connected_to_upstream(
        &self,
        _session: &mut Session,
        reused: bool,
        peer: &HttpPeer,
        #[cfg(unix)] _fd: std::os::unix::io::RawFd,
        #[cfg(windows)] _sock: std::os::windows::io::RawSocket,
        _digest: Option<&Digest>,
        ctx: &mut Self::CTX,
    ) -> pingora_core::Result<()> {
        info!(
            reused,
            peer = %peer,
            instance = %ctx.instance_uri,
            "Connected to upstream"
        );
        Ok(())
    }

    /// Log request summary with timing. (D3)
    async fn logging(
        &self,
        session: &mut Session,
        e: Option<&pingora_core::Error>,
        _ctx: &mut Self::CTX,
    ) {
        let status = session
            .as_downstream()
            .response_written()
            .map(|r| r.status.as_u16())
            .unwrap_or(0);
        let method = session.req_header().method.as_str();
        let path = session.req_header().uri.path();

        if let Some(err) = e {
            warn!(method, path, status, error = %err, "Proxy request failed");
        } else {
            info!(method, path, status, "Proxy request completed");
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
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
    fn build_peer(scheme: Scheme, host: &str, port: u16) -> HttpPeer {
        let tls = matches!(scheme, Scheme::Https | Scheme::Wss | Scheme::Wt);
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let mut peer = HttpPeer::new(addr, tls, host.to_string());
        peer.options.alpn = if tls && !matches!(scheme, Scheme::Wss) {
            pingora_core::protocols::tls::ALPN::H2H1
        } else {
            pingora_core::protocols::tls::ALPN::H1
        };
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
        let proxy = PingoraProxy::new(Duration::from_secs(7), Duration::from_secs(15));
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
}
