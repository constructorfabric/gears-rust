//! Metric name constants.

// ─── Efficiency ──────────────────────────────────────────────────────

/// Counter: JWKS cache hits.
pub const AUTHN_JWKS_CACHE_HITS_TOTAL: &str = "authn.jwks.cache.hit.count";
/// Counter: JWKS cache misses.
pub const AUTHN_JWKS_CACHE_MISSES_TOTAL: &str = "authn.jwks.cache.miss.count";
/// Gauge: current JWKS cache entry count.
pub const AUTHN_JWKS_CACHE_ENTRIES: &str = "authn.jwks.cache.entry.count";

// ─── Performance ─────────────────────────────────────────────────────

/// Histogram: JWT local validation duration.
pub const AUTHN_JWT_VALIDATION_DURATION_SECONDS: &str = "authn.jwt.validation.duration";
/// Histogram: JWKS remote fetch duration.
pub const AUTHN_JWKS_FETCH_DURATION_SECONDS: &str = "authn.jwks.fetch.duration";
/// Histogram: successful authentication request duration.
pub const AUTHN_REQUEST_SUCCESS_DURATION_SECONDS: &str = "authn.request.success.duration";
/// Counter family: failed authentication requests by reason (`reason` label).
pub const AUTHN_REQUEST_FAILURES_TOTAL: &str = "authn.request.failure.count";

// ─── Reliability ─────────────────────────────────────────────────────

/// Counter family: resolver errors by variant (`type` label).
pub const AUTHN_ERRORS_TOTAL: &str = "authn.error.count";
/// Gauge family: circuit-breaker state by host (`0=closed,1=half-open,2=open`).
pub const AUTHN_CIRCUIT_BREAKER_STATE: &str = "authn.circuit_breaker.state";
/// Counter family: circuit-breaker close transitions by host.
pub const AUTHN_CIRCUIT_BREAKER_CLOSED_TOTAL: &str = "authn.circuit_breaker.closed.count";
/// Gauge family: identity-provider availability probe by host (`0` down, `1` up).
pub const AUTHN_IDP_UP: &str = "authn.idp.availability";
/// Counter: failed forced JWKS refresh attempts.
pub const AUTHN_JWKS_REFRESH_FAILURES_TOTAL: &str = "authn.jwks.refresh.failure.count";

// ─── Security ────────────────────────────────────────────────────────

/// Counter family: token rejections by reason (`reason` label).
pub const AUTHN_TOKEN_REJECTED_TOTAL: &str = "authn.token.rejection.count";

// ─── S2S Exchange ───────────────────────────────────────────────────

/// Counter: total S2S client credentials exchange attempts.
pub const AUTHN_S2S_EXCHANGE_TOTAL: &str = "authn.s2s.exchange.count";
/// Counter family: S2S exchange errors by error type (`type` label).
pub const AUTHN_S2S_EXCHANGE_ERRORS_TOTAL: &str = "authn.s2s.exchange.error.count";
/// Histogram: S2S client credentials exchange duration.
pub const AUTHN_S2S_EXCHANGE_DURATION_SECONDS: &str = "authn.s2s.exchange.duration";

// ─── Versatility ─────────────────────────────────────────────────────

/// Gauge: ratio of first-party auth outcomes (`0.0..=1.0`).
pub const AUTHN_FIRST_PARTY_RATIO: &str = "authn.first_party.ratio";

/// Token rejection reason: token is expired.
pub const TOKEN_REJECTION_REASON_EXPIRED: &str = "expired";
/// Token rejection reason: token audience is invalid.
pub const TOKEN_REJECTION_REASON_INVALID_AUDIENCE: &str = "invalid_audience";
/// Token rejection reason: token issued-at claim is invalid.
pub const TOKEN_REJECTION_REASON_INVALID_IAT: &str = "invalid_iat";
/// Token rejection reason: token signature is invalid.
pub const TOKEN_REJECTION_REASON_INVALID_SIG: &str = "invalid_sig";
/// Token rejection reason: tenant claim is malformed.
pub const TOKEN_REJECTION_REASON_INVALID_TENANT: &str = "invalid_tenant";
/// Token rejection reason: required audience is missing.
pub const TOKEN_REJECTION_REASON_MISSING_AUDIENCE: &str = "missing_audience";
/// Token rejection reason: tenant claim is missing.
pub const TOKEN_REJECTION_REASON_MISSING_TENANT: &str = "missing_tenant";
/// Token rejection reason: issuer is not trusted.
pub const TOKEN_REJECTION_REASON_UNTRUSTED_ISSUER: &str = "untrusted_issuer";
