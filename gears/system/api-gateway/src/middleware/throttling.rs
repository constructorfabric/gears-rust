//! Zone-based throttling middleware.
//!
//! Two spec-driven maps are built from the registered operations:
//!
//! - [`ThrottlingMapNoAuth`] — operations whose `ThrottlingSpec` has
//!   `require_security_context = false`. Enforced *before* authentication and
//!   restricted to IP-keyed zones (identity keying is unavailable pre-auth).
//! - [`ThrottlingMap`] — operations with `require_security_context = true`.
//!   Enforced *after* authentication; identity-keyed zones use the subject id
//!   (or a code-supplied [`toolkit::api::IdentityExtractor`]).
//!
//! Each `(method, path)` lands in exactly one map (decided by the per-operation
//! flag).
//!
//! On a served request, the rate-limit zone's `RateLimit-*` (and legacy
//! `X-RateLimit-*`) metadata headers are attached to the response.
//!
//! When an operation's `ThrottlingSpec` sets `dry_run = true`, limits are
//! observed but not enforced: a request that would have been rejected is served
//! instead, and a `warn` event is logged.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;
use governor::clock::{Clock, DefaultClock};
use governor::middleware::StateInformationMiddleware;
use governor::state::keyed::DefaultKeyedStateStore;
use governor::{Quota, RateLimiter};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use toolkit::api::{OperationSpec, ThrottlingSpec};
use toolkit_security::SecurityContext;

use crate::config::{ApiGatewayConfig, InFlightLimitZone, KeyType, RateLimitZone, RetryAfter};
use crate::middleware::common;
use crate::middleware::errors::ApiGatewayGatewayError;

type ThrottleKey = (Method, String);

/// Floor for the `Retry-After` hint on in-flight rejections (seconds).
const DEFAULT_IN_FLIGHT_RETRY_AFTER_SECS: u64 = 5;

/// Keyed token-bucket limiter (one entry per identity/IP key).
type KeyedRateLimiter =
    RateLimiter<String, DefaultKeyedStateStore<String>, DefaultClock, StateInformationMiddleware>;

/// A resolved rate-limit zone: config + shared keyed limiter state.
struct RateZone {
    cfg: RateLimitZone,
    limiter: KeyedRateLimiter,
    policy: HeaderValue,
}

/// Per-key concurrency gate for an in-flight zone.
struct KeyGate {
    inflight: Arc<Semaphore>,
    backlog: Arc<Semaphore>,
}

impl KeyGate {
    /// Acquire an in-flight permit, optionally waiting in the backlog.
    ///
    /// Returns `None` when the request should be rejected (no in-flight slot and
    /// either no backlog capacity or the backlog wait timed out).
    async fn acquire(&self, backlog_timeout: Duration) -> Option<OwnedSemaphorePermit> {
        if let Ok(permit) = Arc::clone(&self.inflight).try_acquire_owned() {
            return Some(permit);
        }
        // No free slot: take a backlog slot and wait for one to free up.
        let _backlog_slot = Arc::clone(&self.backlog).try_acquire_owned().ok()?;
        if let Ok(Ok(permit)) =
            tokio::time::timeout(backlog_timeout, Arc::clone(&self.inflight).acquire_owned()).await
        {
            Some(permit)
        } else {
            None
        }
        // `_backlog_slot` is released here, before the in-flight permit is held.
    }
}

/// A resolved in-flight (concurrency) zone with per-key gates.
struct InFlightZone {
    cfg: InFlightLimitZone,
    keys: DashMap<String, Arc<KeyGate>>,
    excluded: HashSet<String>,
}

impl InFlightZone {
    fn gate(&self, key: &str) -> Arc<KeyGate> {
        if let Some(existing) = self.keys.get(key) {
            return Arc::clone(&existing);
        }
        // Soft `max_keys` cap: drop gates no longer referenced by in-flight requests.
        if self.keys.len() as u64 >= self.cfg.max_keys {
            self.keys.retain(|_, v| Arc::strong_count(v) > 1);
        }
        Arc::clone(&self.keys.entry(key.to_owned()).or_insert_with(|| {
            Arc::new(KeyGate {
                inflight: Arc::new(Semaphore::new(self.cfg.in_flight_limit as usize)),
                backlog: Arc::new(Semaphore::new(self.cfg.backlog_limit as usize)),
            })
        }))
    }
}

/// A per-operation throttling entry.
struct ThrottlingEntry {
    spec: ThrottlingSpec,
    rate_zone: Option<Arc<RateZone>>,
    inflight_zone: Option<Arc<InFlightZone>>,
}

/// Shared inner state for both throttling maps.
///
/// Each [`ThrottlingEntry`] holds `Arc` handles to its resolved zones, so the
/// zone runtimes stay alive for as long as the routing table; no separate
/// zone registry is needed at request time.
#[derive(Default)]
struct ThrottlingInner {
    routes: HashMap<ThrottleKey, ThrottlingEntry>,
}

/// Post-auth throttling map (identity-keyed zones allowed).
#[derive(Clone, Default)]
pub struct ThrottlingMap {
    inner: Arc<ThrottlingInner>,
}

/// Pre-auth throttling map (IP-keyed zones only).
#[derive(Clone, Default)]
pub struct ThrottlingMapNoAuth {
    inner: Arc<ThrottlingInner>,
}

impl ThrottlingMap {
    /// Build the post-auth (`require_security_context = true`) throttling map.
    ///
    /// # Errors
    /// Returns an error if an entry references an undefined zone or an invalid
    /// (e.g. zero-limit) zone.
    pub fn from_specs(specs: &[OperationSpec], cfg: &ApiGatewayConfig) -> Result<Self> {
        Ok(Self {
            inner: Arc::new(build(specs, cfg, true)?),
        })
    }
}

impl ThrottlingMapNoAuth {
    /// Build the pre-auth (`require_security_context = false`) throttling map.
    ///
    /// # Errors
    /// Returns an error if an entry references an undefined zone, an invalid
    /// zone, or an identity-keyed zone (forbidden before authentication).
    pub fn from_specs(specs: &[OperationSpec], cfg: &ApiGatewayConfig) -> Result<Self> {
        Ok(Self {
            inner: Arc::new(build(specs, cfg, false)?),
        })
    }
}

/// Shared builder used by both maps, selecting specs by `require_ctx`.
fn build(
    specs: &[OperationSpec],
    cfg: &ApiGatewayConfig,
    require_ctx: bool,
) -> Result<ThrottlingInner> {
    let mut rate_zones: HashMap<String, Arc<RateZone>> = HashMap::new();
    let mut inflight_zones: HashMap<String, Arc<InFlightZone>> = HashMap::new();
    let mut routes: HashMap<ThrottleKey, ThrottlingEntry> = HashMap::new();

    for spec in specs {
        let Some(thr) = spec.throttling.as_ref() else {
            continue;
        };
        if thr.require_security_context != require_ctx {
            continue;
        }

        let rate_zone = if thr.rate_limit_zone.is_empty() {
            None
        } else {
            let zcfg = cfg
                .rate_limit_zones
                .get(&thr.rate_limit_zone)
                .ok_or_else(|| {
                    anyhow!(
                        "throttling: operation {} {} references undefined rate_limit zone '{}'",
                        spec.method,
                        spec.path,
                        thr.rate_limit_zone
                    )
                })?;
            check_key_type(require_ctx, &thr.rate_limit_zone, zcfg.key.key_type)?;
            Some(get_or_build_rate_zone(
                &mut rate_zones,
                &thr.rate_limit_zone,
                zcfg,
            )?)
        };

        let inflight_zone = if thr.in_flight_limit_zone.is_empty() {
            None
        } else {
            let zcfg = cfg
                .in_flight_limit_zones
                .get(&thr.in_flight_limit_zone)
                .ok_or_else(|| {
                    anyhow!(
                        "throttling: operation {} {} references undefined in_flight_limit zone '{}'",
                        spec.method,
                        spec.path,
                        thr.in_flight_limit_zone
                    )
                })?;
            check_key_type(require_ctx, &thr.in_flight_limit_zone, zcfg.key.key_type)?;
            Some(get_or_build_inflight_zone(
                &mut inflight_zones,
                &thr.in_flight_limit_zone,
                zcfg,
            ))
        };

        let key = (spec.method.clone(), spec.path.clone());
        routes.insert(
            key,
            ThrottlingEntry {
                spec: thr.clone(),
                rate_zone,
                inflight_zone,
            },
        );
    }

    Ok(ThrottlingInner { routes })
}

/// Identity keying is only valid after authentication.
fn check_key_type(require_ctx: bool, zone: &str, kt: KeyType) -> Result<()> {
    if !require_ctx && matches!(kt, KeyType::Identity) {
        bail!(
            "throttling: zone '{zone}' is identity-keyed but is referenced by a pre-auth \
             (require_security_context=false) operation; identity keying requires authentication"
        );
    }
    Ok(())
}

fn get_or_build_rate_zone(
    zones: &mut HashMap<String, Arc<RateZone>>,
    name: &str,
    cfg: &RateLimitZone,
) -> Result<Arc<RateZone>> {
    if let Some(existing) = zones.get(name) {
        return Ok(Arc::clone(existing));
    }
    let rps = NonZeroU32::new(cfg.rate_limit.rps)
        .ok_or_else(|| anyhow!("throttling: rate_limit zone '{name}' has rps = 0"))?;
    let burst = NonZeroU32::new(cfg.burst_limit)
        .ok_or_else(|| anyhow!("throttling: rate_limit zone '{name}' has burst_limit = 0"))?;
    let limiter = RateLimiter::keyed(Quota::per_second(rps).allow_burst(burst))
        .with_middleware::<StateInformationMiddleware>();
    let policy = HeaderValue::from_str(&format!(
        "\"burst\";q={};w={}",
        cfg.burst_limit, cfg.rate_limit.rps
    ))
    .context("throttling: failed to build RateLimit-Policy header")?;
    let zone = Arc::new(RateZone {
        cfg: cfg.clone(),
        limiter,
        policy,
    });
    zones.insert(name.to_owned(), Arc::clone(&zone));
    Ok(zone)
}

fn get_or_build_inflight_zone(
    zones: &mut HashMap<String, Arc<InFlightZone>>,
    name: &str,
    cfg: &InFlightLimitZone,
) -> Arc<InFlightZone> {
    if let Some(existing) = zones.get(name) {
        return Arc::clone(existing);
    }
    let zone = Arc::new(InFlightZone {
        cfg: cfg.clone(),
        keys: DashMap::new(),
        excluded: cfg.excluded_keys.iter().cloned().collect(),
    });
    zones.insert(name.to_owned(), Arc::clone(&zone));
    zone
}

/// Post-auth throttling middleware (uses [`ThrottlingMap`]).
pub async fn throttling_middleware(map: ThrottlingMap, req: Request, next: Next) -> Response {
    enforce(&map.inner, req, next).await
}

/// Pre-auth throttling middleware (uses [`ThrottlingMapNoAuth`]).
pub async fn throttling_no_auth_middleware(
    map: ThrottlingMapNoAuth,
    req: Request,
    next: Next,
) -> Response {
    enforce(&map.inner, req, next).await
}

async fn enforce(inner: &ThrottlingInner, req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map_or_else(|| req.uri().path().to_owned(), |p| p.as_str().to_owned());
    let path = common::resolve_path(&req, path.as_str());
    let key = (method, path);

    let Some(entry) = inner.routes.get(&key) else {
        return next.run(req).await;
    };

    // Rate-limit metadata headers to attach to the *response* once we have one.
    let mut rate_headers: Option<RateHeaders> = None;

    // Rate limiting.
    if let Some(zone) = entry.rate_zone.as_ref() {
        let id = compute_key(zone.cfg.key.key_type, entry, &req);
        match zone.limiter.check_key(&id) {
            Ok(snapshot) => {
                rate_headers = Some(RateHeaders {
                    policy: zone.policy.clone(),
                    burst: HeaderValue::from(zone.cfg.burst_limit),
                    remaining: HeaderValue::from(snapshot.remaining_burst_capacity()),
                });
            }
            Err(not_until) => {
                if entry.spec.dry_run {
                    // Dry-run: observe but don't enforce. Log and fall through.
                    log_dry_run_rate(&id);
                } else {
                    let wait = not_until
                        .wait_time_from(zone.limiter.clock().now())
                        .as_secs();
                    let retry_after = match zone.cfg.response_retry_after {
                        RetryAfter::Auto => Some(wait),
                        RetryAfter::Seconds(n) => Some(n),
                    };
                    log_throttled(&key, "rate_limit", &id);
                    return throttle_response(
                        zone.cfg.response_status_code,
                        retry_after,
                        Some((&zone.policy, zone.cfg.burst_limit)),
                    );
                }
            }
        }
    }

    // In-flight concurrency limiting.
    if let Some(zone) = entry.inflight_zone.as_ref() {
        let id = compute_key(zone.cfg.key.key_type, entry, &req);
        if !zone.excluded.contains(&id) {
            let gate = zone.gate(&id);
            let Some(permit) = gate.acquire(zone.cfg.backlog_timeout).await else {
                if entry.spec.dry_run {
                    // Dry-run: observe but don't enforce. Log and serve the
                    // request without holding an in-flight permit.
                    log_dry_run_in_flight(&id);
                    let mut response = next.run(req).await;
                    apply_rate_headers(&mut response, rate_headers.as_ref());
                    return response;
                }
                log_throttled(&key, "in_flight", &id);
                // Suggest a retry after roughly the backlog wait window, with a
                // sensible floor so clients always get a usable hint.
                let retry_after = zone
                    .cfg
                    .backlog_timeout
                    .as_secs()
                    .max(DEFAULT_IN_FLIGHT_RETRY_AFTER_SECS);
                return throttle_response(zone.cfg.response_status_code, Some(retry_after), None);
            };
            let mut response = next.run(req).await;
            drop(permit);
            apply_rate_headers(&mut response, rate_headers.as_ref());
            return response;
        }
    }

    let mut response = next.run(req).await;
    apply_rate_headers(&mut response, rate_headers.as_ref());
    response
}

/// Rate-limit metadata headers echoed on successful (served) responses.
struct RateHeaders {
    policy: HeaderValue,
    burst: HeaderValue,
    remaining: HeaderValue,
}

/// Attach `RateLimit-*` (and legacy `X-RateLimit-*`) headers to a response.
fn apply_rate_headers(response: &mut Response, rate_headers: Option<&RateHeaders>) {
    let Some(h) = rate_headers else {
        return;
    };
    let headers = response.headers_mut();
    headers.insert("RateLimit-Policy", h.policy.clone());
    headers.insert("RateLimit-Limit", h.burst.clone());
    headers.insert("RateLimit-Remaining", h.remaining.clone());
    // Legacy `X-RateLimit-*` headers for compatibility with the pre-zone limiter.
    headers.insert("X-RateLimit-Limit", h.burst.clone());
    headers.insert("X-RateLimit-Remaining", h.remaining.clone());
}

/// Compute the throttling key for a request according to the zone key type.
fn compute_key(kind: KeyType, entry: &ThrottlingEntry, req: &Request) -> String {
    match kind {
        KeyType::Ip => client_ip(req),
        KeyType::Identity => entry.spec.identity_extractor.as_ref().map_or_else(
            || {
                req.extensions()
                    .get::<SecurityContext>()
                    .map_or_else(|| "anonymous".to_owned(), |sc| sc.subject_id().to_string())
            },
            |ext| ext.extract(req),
        ),
    }
}

/// Resolve the client IP: `X-Forwarded-For` (first hop) / `X-Real-IP`, then the
/// peer address from `ConnectInfo`, else `"unknown"`.
fn client_ip(req: &Request) -> String {
    let headers = req.headers();
    if let Some(ip) = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return ip.to_owned();
    }
    if let Some(ip) = headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return ip.to_owned();
    }
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or_else(|| "unknown".to_owned(), |ci| ci.0.ip().to_string())
}

/// Build a throttling rejection response.
///
/// `rate_headers` carries `(policy, burst_limit)` for rate-limit rejections so
/// the `RateLimit-*` / legacy `X-RateLimit-Limit` headers are echoed on the
/// error (matching the legacy rate limiter); it is `None` for in-flight
/// rejections, which have no token-bucket policy.
fn throttle_response(
    status: u16,
    retry_after_seconds: Option<u64>,
    rate_headers: Option<(&HeaderValue, u32)>,
) -> Response {
    let err = ApiGatewayGatewayError::resource_exhausted("throttling limit exceeded")
        .with_quota_violation("throttling", "limit exceeded")
        .create();
    let mut response = err.into_response();
    if let Ok(code) = StatusCode::from_u16(status) {
        *response.status_mut() = code;
    }
    let headers = response.headers_mut();
    if let Some((policy, burst_limit)) = rate_headers {
        let burst = HeaderValue::from(burst_limit);
        headers.insert("RateLimit-Policy", policy.clone());
        headers.insert("RateLimit-Limit", burst.clone());
        headers.insert("X-RateLimit-Limit", burst);
    }
    if let Some(secs) = retry_after_seconds
        && let Ok(value) = HeaderValue::from_str(&secs.to_string())
    {
        headers.insert(header::RETRY_AFTER, value);
    }
    response
}

fn log_throttled(key: &ThrottleKey, kind: &str, id: &str) {
    tracing::debug!(
        method = %key.0,
        path = %key.1,
        kind,
        key = %id,
        "throttling limit exceeded"
    );
}

/// Dry-run rate-limit event: the request would have been rate-limited but is
/// served because the operation is in dry-run mode.
fn log_dry_run_rate(id: &str) {
    tracing::warn!(
        rate_limit_key = %id,
        "too many requests, serving will be continued because of dry run mode"
    );
}

/// Dry-run in-flight event: the request would have been rejected by the
/// in-flight limit but is served because the operation is in dry-run mode.
fn log_dry_run_in_flight(id: &str) {
    tracing::warn!(
        in_flight_limit_key = %id,
        "too many in-flight requests, serving will be continued because of dry run mode"
    );
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::config::{KeyConfig, RateSpec};
    use axum::Router;
    use axum::body::Body;
    use axum::routing::get;
    use std::time::Duration;
    use tower::ServiceExt;

    use toolkit::api::IdentityExtractor;
    use toolkit::api::operation_builder::VendorExtensions;

    #[derive(Debug)]
    struct StaticIdentity(&'static str);
    impl IdentityExtractor for StaticIdentity {
        fn extract(&self, _req: &Request) -> String {
            self.0.to_owned()
        }
    }

    fn op(method: Method, path: &str, throttling: Option<ThrottlingSpec>) -> OperationSpec {
        OperationSpec {
            method,
            path: path.to_owned(),
            operation_id: None,
            summary: None,
            description: None,
            tags: vec![],
            params: vec![],
            request_body: None,
            responses: vec![],
            handler_id: "test".to_owned(),
            authenticated: false,
            is_public: true,
            throttling,
            allowed_request_content_types: None,
            vendor_extensions: VendorExtensions::default(),
            license_requirement: None,
        }
    }

    fn thr(
        rate_zone: &str,
        inflight_zone: &str,
        require_ctx: bool,
        extractor: Option<Arc<dyn IdentityExtractor>>,
    ) -> ThrottlingSpec {
        ThrottlingSpec {
            rate_limit_zone: rate_zone.to_owned(),
            in_flight_limit_zone: inflight_zone.to_owned(),
            identity_extractor: extractor,
            require_security_context: require_ctx,
            dry_run: false,
        }
    }

    fn thr_dry(rate_zone: &str, inflight_zone: &str) -> ThrottlingSpec {
        ThrottlingSpec {
            rate_limit_zone: rate_zone.to_owned(),
            in_flight_limit_zone: inflight_zone.to_owned(),
            identity_extractor: None,
            require_security_context: false,
            dry_run: true,
        }
    }

    fn rate_zone_cfg(rps: u32, burst: u32, key: KeyType) -> RateLimitZone {
        RateLimitZone {
            rate_limit: RateSpec { rps },
            burst_limit: burst,
            response_status_code: 429,
            response_retry_after: RetryAfter::Auto,
            key: KeyConfig { key_type: key },
            max_keys: 1000,
        }
    }

    fn inflight_zone_cfg(in_flight: u32, key: KeyType, excluded: Vec<String>) -> InFlightLimitZone {
        InFlightLimitZone {
            in_flight_limit: in_flight,
            backlog_limit: 0,
            backlog_timeout: Duration::from_millis(50),
            response_status_code: 429,
            key: KeyConfig { key_type: key },
            max_keys: 1000,
            excluded_keys: excluded,
        }
    }

    fn cfg_with_rate(name: &str, zone: RateLimitZone) -> ApiGatewayConfig {
        let mut cfg = ApiGatewayConfig::default();
        cfg.rate_limit_zones.insert(name.to_owned(), zone);
        cfg
    }

    #[test]
    fn partitions_specs_by_require_security_context() {
        let mut cfg = ApiGatewayConfig::default();
        cfg.rate_limit_zones
            .insert("ip".to_owned(), rate_zone_cfg(10, 10, KeyType::Ip));
        cfg.rate_limit_zones
            .insert("id".to_owned(), rate_zone_cfg(10, 10, KeyType::Identity));

        let specs = vec![
            op(Method::GET, "/pre", Some(thr("ip", "", false, None))),
            op(Method::GET, "/post", Some(thr("id", "", true, None))),
            op(Method::GET, "/none", None),
        ];

        let pre = ThrottlingMapNoAuth::from_specs(&specs, &cfg).unwrap();
        let post = ThrottlingMap::from_specs(&specs, &cfg).unwrap();

        assert_eq!(pre.inner.routes.len(), 1);
        assert!(
            pre.inner
                .routes
                .contains_key(&(Method::GET, "/pre".to_owned()))
        );
        assert_eq!(post.inner.routes.len(), 1);
        assert!(
            post.inner
                .routes
                .contains_key(&(Method::GET, "/post".to_owned()))
        );
    }

    #[test]
    fn pre_auth_identity_zone_is_rejected() {
        let cfg = cfg_with_rate("id", rate_zone_cfg(10, 10, KeyType::Identity));
        let specs = vec![op(Method::GET, "/x", Some(thr("id", "", false, None)))];
        let err = ThrottlingMapNoAuth::from_specs(&specs, &cfg)
            .err()
            .expect("should error")
            .to_string();
        assert!(
            err.contains("identity keying requires authentication"),
            "{err}"
        );
    }

    #[test]
    fn undefined_zone_is_rejected() {
        let cfg = ApiGatewayConfig::default();
        let specs = vec![op(Method::GET, "/x", Some(thr("missing", "", false, None)))];
        let err = ThrottlingMapNoAuth::from_specs(&specs, &cfg)
            .err()
            .expect("should error")
            .to_string();
        assert!(err.contains("undefined rate_limit zone"), "{err}");
    }

    #[test]
    fn shared_zone_arc_within_map() {
        let cfg = cfg_with_rate("ip", rate_zone_cfg(10, 10, KeyType::Ip));
        let specs = vec![
            op(Method::GET, "/a", Some(thr("ip", "", false, None))),
            op(Method::GET, "/b", Some(thr("ip", "", false, None))),
        ];
        let map = ThrottlingMapNoAuth::from_specs(&specs, &cfg).unwrap();
        let a = map.inner.routes[&(Method::GET, "/a".to_owned())]
            .rate_zone
            .clone()
            .unwrap();
        let b = map.inner.routes[&(Method::GET, "/b".to_owned())]
            .rate_zone
            .clone()
            .unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn client_ip_precedence() {
        let req = Request::builder()
            .header("x-forwarded-for", "203.0.113.7, 10.0.0.1")
            .header("x-real-ip", "198.51.100.9")
            .body(Body::empty())
            .unwrap();
        assert_eq!(client_ip(&req), "203.0.113.7");

        let req = Request::builder()
            .header("x-real-ip", "198.51.100.9")
            .body(Body::empty())
            .unwrap();
        assert_eq!(client_ip(&req), "198.51.100.9");

        let mut req = Request::builder().body(Body::empty()).unwrap();
        req.extensions_mut().insert(ConnectInfo(
            "192.168.1.5:1234".parse::<SocketAddr>().unwrap(),
        ));
        assert_eq!(client_ip(&req), "192.168.1.5");

        let req = Request::builder().body(Body::empty()).unwrap();
        assert_eq!(client_ip(&req), "unknown");
    }

    #[test]
    fn compute_key_identity_uses_extractor_then_subject() {
        let entry_with_ext = ThrottlingEntry {
            spec: thr(
                "",
                "",
                true,
                Some(Arc::new(StaticIdentity("from-extractor"))),
            ),
            rate_zone: None,
            inflight_zone: None,
        };
        let req = Request::builder().body(Body::empty()).unwrap();
        assert_eq!(
            compute_key(KeyType::Identity, &entry_with_ext, &req),
            "from-extractor"
        );

        let entry_no_ext = ThrottlingEntry {
            spec: thr("", "", true, None),
            rate_zone: None,
            inflight_zone: None,
        };
        // No SecurityContext present → anonymous.
        assert_eq!(
            compute_key(KeyType::Identity, &entry_no_ext, &req),
            "anonymous"
        );
    }

    #[tokio::test]
    async fn rate_limit_denies_after_burst() {
        let cfg = cfg_with_rate("ip", rate_zone_cfg(1, 1, KeyType::Ip));
        let specs = vec![op(Method::GET, "/x", Some(thr("ip", "", false, None)))];
        let map = ThrottlingMapNoAuth::from_specs(&specs, &cfg).unwrap();

        let app =
            Router::new()
                .route("/x", get(|| async { "ok" }))
                .layer(axum::middleware::from_fn(
                    move |req: Request, next: Next| {
                        let map = map.clone();
                        async move { throttling_no_auth_middleware(map, req, next).await }
                    },
                ));

        let first = app
            .clone()
            .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);

        let second = app
            .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(second.headers().contains_key(header::RETRY_AFTER));
        // Rate-limit rejections echo the policy/limit headers (legacy parity).
        assert!(second.headers().contains_key("RateLimit-Policy"));
        assert!(second.headers().contains_key("RateLimit-Limit"));
        assert!(second.headers().contains_key("X-RateLimit-Limit"));
    }

    #[tokio::test]
    async fn inflight_rejection_sets_retry_after() {
        let mut cfg = ApiGatewayConfig::default();
        // in_flight_limit = 0 with no backlog => first request is rejected.
        cfg.in_flight_limit_zones
            .insert("ifl".to_owned(), inflight_zone_cfg(0, KeyType::Ip, vec![]));
        let specs = vec![op(Method::GET, "/x", Some(thr("", "ifl", false, None)))];
        let map = ThrottlingMapNoAuth::from_specs(&specs, &cfg).unwrap();

        let app =
            Router::new()
                .route("/x", get(|| async { "ok" }))
                .layer(axum::middleware::from_fn(
                    move |req: Request, next: Next| {
                        let map = map.clone();
                        async move { throttling_no_auth_middleware(map, req, next).await }
                    },
                ));

        let resp = app
            .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        let retry = resp
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .expect("retry-after present");
        assert_eq!(retry, DEFAULT_IN_FLIGHT_RETRY_AFTER_SECS);
    }

    #[tokio::test]
    async fn inflight_excluded_key_bypasses_limit() {
        let mut cfg = ApiGatewayConfig::default();
        cfg.in_flight_limit_zones.insert(
            "ifl".to_owned(),
            inflight_zone_cfg(1, KeyType::Ip, vec!["unknown".to_owned()]),
        );
        let specs = vec![op(Method::GET, "/x", Some(thr("", "ifl", false, None)))];
        let map = ThrottlingMapNoAuth::from_specs(&specs, &cfg).unwrap();

        let app =
            Router::new()
                .route("/x", get(|| async { "ok" }))
                .layer(axum::middleware::from_fn(
                    move |req: Request, next: Next| {
                        let map = map.clone();
                        async move { throttling_no_auth_middleware(map, req, next).await }
                    },
                ));

        // Client IP resolves to "unknown" (no ConnectInfo/headers), which is excluded.
        let resp = app
            .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn rate_limit_headers_on_success_response() {
        let cfg = cfg_with_rate("ip", rate_zone_cfg(10, 10, KeyType::Ip));
        let specs = vec![op(Method::GET, "/x", Some(thr("ip", "", false, None)))];
        let map = ThrottlingMapNoAuth::from_specs(&specs, &cfg).unwrap();

        let app = Router::new()
            .route("/x", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(
                move |req: Request, next: Next| {
                    let map = map.clone();
                    async move { throttling_no_auth_middleware(map, req, next).await }
                },
            ));

        let resp = app
            .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // Metadata headers are exposed on the served response, not the request.
        let headers = resp.headers();
        assert!(headers.contains_key("RateLimit-Policy"));
        assert!(headers.contains_key("RateLimit-Limit"));
        assert!(headers.contains_key("RateLimit-Remaining"));
        assert!(headers.contains_key("X-RateLimit-Limit"));
        assert!(headers.contains_key("X-RateLimit-Remaining"));
    }

    #[tokio::test]
    async fn dry_run_rate_limit_serves_over_burst() {
        // rps 1 / burst 1: the second request would normally be rejected (429),
        // but dry-run serves it and logs instead.
        let cfg = cfg_with_rate("ip", rate_zone_cfg(1, 1, KeyType::Ip));
        let specs = vec![op(Method::GET, "/x", Some(thr_dry("ip", "")))];
        let map = ThrottlingMapNoAuth::from_specs(&specs, &cfg).unwrap();

        let app =
            Router::new()
                .route("/x", get(|| async { "ok" }))
                .layer(axum::middleware::from_fn(
                    move |req: Request, next: Next| {
                        let map = map.clone();
                        async move { throttling_no_auth_middleware(map, req, next).await }
                    },
                ));

        let first = app
            .clone()
            .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);

        // Would-be-throttled request is served instead of rejected.
        let second = app
            .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        // Bypassed requests carry no rejection hint.
        assert!(!second.headers().contains_key(header::RETRY_AFTER));
    }

    #[tokio::test]
    async fn dry_run_in_flight_serves_over_limit() {
        // in_flight_limit = 0 with no backlog => the request would normally be
        // rejected (429), but dry-run serves it and logs instead.
        let mut cfg = ApiGatewayConfig::default();
        cfg.in_flight_limit_zones
            .insert("ifl".to_owned(), inflight_zone_cfg(0, KeyType::Ip, vec![]));
        let specs = vec![op(Method::GET, "/x", Some(thr_dry("", "ifl")))];
        let map = ThrottlingMapNoAuth::from_specs(&specs, &cfg).unwrap();

        let app =
            Router::new()
                .route("/x", get(|| async { "ok" }))
                .layer(axum::middleware::from_fn(
                    move |req: Request, next: Next| {
                        let map = map.clone();
                        async move { throttling_no_auth_middleware(map, req, next).await }
                    },
                ));

        let resp = app
            .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(!resp.headers().contains_key(header::RETRY_AFTER));
    }
}
