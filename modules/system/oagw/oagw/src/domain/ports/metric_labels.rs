//! Centralized constants for OAGW metric label keys and values.
//!
//! Keeps magic strings out of service code and ensures consistent naming
//! across recording sites and dashboards.
//!
//! Label keys follow OpenTelemetry HTTP semantic conventions where an
//! analog exists in the inbound API Gateway (`http.request.method`,
//! `http.route`, `http.response.status_code`) so the two modules share
//! dashboard and alert templates. OAGW-specific concerns (upstream alias,
//! pipeline phase, error variant) keep short module-local names.

pub mod key {
    /// Upstream alias (stable, low-cardinality identifier of the destination service).
    pub const HOST: &str = "host";
    /// Route match pattern (NOT raw request path — cardinality control).
    /// OTel HTTP semconv attribute name.
    pub const PATH: &str = "http.route";
    /// HTTP request method, normalized via [`normalize_method`] to bound cardinality.
    /// OTel HTTP semconv attribute name.
    pub const METHOD: &str = "http.request.method";
    /// HTTP response status code (numeric, OTel HTTP semconv).
    /// Aligns with the inbound API Gateway's `http.server.request.duration`
    /// instrument labels so the two modules share dashboards and alerts.
    pub const STATUS_CODE: &str = "http.response.status_code";
    /// `DomainError` variant name.
    pub const ERROR_TYPE: &str = "error_type";
    /// Pipeline phase for duration histogram (`total`, future: `auth`, `guard`, `upstream`, …).
    pub const PHASE: &str = "phase";
}

// ── Label values ─────────────────────────────────────────────────────────

/// Pipeline phase labels (`phase` label).
pub mod phase {
    /// End-to-end proxy_request duration.
    pub const TOTAL: &str = "total";
    /// Time spent inside the auth plugin (`execute_auth_plugin`).
    pub const AUTH: &str = "auth";
}

/// Sentinel for sites that observe a request before host/path resolution.
pub const UNKNOWN: &str = "unknown";

/// Sentinel for non-standard HTTP methods (per OTel HTTP semconv).
///
/// Exposed here so the infra-layer `normalize_method` and any downstream
/// consumers (dashboards, alert filters) reference a single shared constant.
pub const METHOD_OTHER: &str = "_OTHER";
