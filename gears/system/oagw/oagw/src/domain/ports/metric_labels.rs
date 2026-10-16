//! Centralized constants for OAGW metric label keys and values.
//!
//! Keeps magic strings out of service code and ensures consistent naming
//! across recording sites and dashboards.
//!
//! Label keys follow OpenTelemetry HTTP semantic conventions where an
//! analog exists in the inbound API Gateway (`http.request.method`,
//! `http.route`, `http.response.status_code`) so the two gears share
//! dashboard and alert templates. OAGW-specific concerns (upstream alias,
//! pipeline phase, error variant) keep short gear-local names.

/// Metric label (attribute) key names shared by all OAGW instruments.
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
    /// instrument labels so the two gears share dashboards and alerts.
    pub const STATUS_CODE: &str = "http.response.status_code";
    /// `DomainError` variant name.
    pub const ERROR_TYPE: &str = "error_type";
    /// Pipeline phase for duration histogram (`total`, future: `auth`, `guard`, `upstream`, …).
    pub const PHASE: &str = "phase";
    /// Registry instance kind (`upstream`, `route`).
    pub const KIND: &str = "kind";
    /// Why the registry reconcile skipped or kept an instance.
    pub const REASON: &str = "reason";
    /// How a registry reconcile run ended.
    pub const OUTCOME: &str = "outcome";
}

/// Registry instance kinds (`kind` label).
pub mod provisioning_kind {
    /// An upstream instance.
    pub const UPSTREAM: &str = "upstream";
    /// A route instance.
    pub const ROUTE: &str = "route";
}

/// Why the registry reconcile skipped an instance or kept a dropped upstream
/// (`reason` label of `registry_provisioning_failures_total`).
pub mod provisioning_reason {
    /// The instance has no ID.
    pub const INVALID_INSTANCE: &str = "invalid_instance";
    /// The instance fails the request checks.
    pub const INVALID_REQUEST: &str = "invalid_request";
    /// Several upstream instances of one tenant derive the same alias.
    pub const DUPLICATE_ALIAS: &str = "duplicate_alias";
    /// The route names no upstream instance of its tenant.
    pub const UNKNOWN_UPSTREAM: &str = "unknown_upstream";
    /// Registry routes overlap each other.
    pub const OVERLAPPING_REGISTRY_ROUTES: &str = "overlapping_registry_routes";
    /// A Management API row holds the instance's ID.
    pub const ID_HELD_BY_API: &str = "id_held_by_api";
    /// The write conflicts with another row (alias, ID, route overlap).
    pub const CONFLICT: &str = "conflict";
    /// The write failed validation (hierarchy, budget, upstream missing, …).
    pub const VALIDATION: &str = "validation";
    /// The tenant or the row is gone.
    pub const NOT_FOUND: &str = "not_found";
    /// The policy denied the write.
    pub const FORBIDDEN: &str = "forbidden";
    /// The `secret_ref` is not accessible to the tenant.
    pub const SECRET_NOT_ACCESSIBLE: &str = "secret_not_accessible";
    /// Writer and owner differ.
    pub const REGISTRY_MANAGED: &str = "registry_managed";
    /// The alias is held by another registry upstream whose write this run
    /// also rejected: a swap, a longer cycle (split across two releases), or
    /// a hand-over from an upstream rejected for another reason.
    pub const ALIAS_CYCLE: &str = "alias_cycle";
    /// The path is held by another registry route whose write this run also
    /// rejected: a swap, a longer cycle (split across two releases), or a
    /// hand-over from a route rejected for another reason.
    pub const PATH_CYCLE: &str = "path_cycle";
    /// A dropped upstream kept because routes still use it (Management API
    /// routes, or skipped registry routes).
    pub const KEPT_IN_USE: &str = "kept_in_use";
    /// A plugin the instance names is missing or in use.
    pub const PLUGIN: &str = "plugin";
    /// The write returned an error a Control Plane write never returns (a
    /// proxy-time error); the log names its variant.
    pub const UNEXPECTED: &str = "unexpected";
}

/// How a registry reconcile run ended (`outcome` label of
/// `registry_provisioning_runs_total`).
pub mod provisioning_outcome {
    /// The stored content hash matched: nothing to do.
    pub const UP_TO_DATE: &str = "up_to_date";
    /// A newer deployment generation owns the registry: nothing written.
    pub const STALE: &str = "stale";
    /// Reconciled with nothing skipped or kept.
    pub const CLEAN: &str = "clean";
    /// Reconciled with skips or kept upstreams, or stopped early.
    pub const NOT_CLEAN: &str = "not_clean";
    /// Aborted by an infrastructure error.
    pub const FAILED: &str = "failed";
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
