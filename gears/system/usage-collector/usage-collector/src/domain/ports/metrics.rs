//! Output port for recording usage-collector operational metrics.
//!
//! Implementations live in [`crate::infra::metrics`] (OpenTelemetry
//! instruments declared on a scoped `Meter` from `ToolKit`'s global
//! `SdkMeterProvider`). Domain code depends only on this trait and the
//! label enums below — it has no knowledge of `OTel`, honoring the DDD-light
//! layer boundary (domain must not depend on infra / transport types).
//!
//! ## Naming contract (DESIGN §3.11.5)
//!
//! Unlike some sibling gears, usage-collector bakes the **full literal**
//! Prometheus name into the instrument (counters carry `_total`, duration
//! histograms carry `_seconds`) and sets **no** `.with_unit(...)` hint —
//! the account-management convention. The rendered Prometheus name is then
//! identical whether the downstream `OTel` collector runs with
//! `add_metric_suffixes` on or off. The concrete builder lives in the infra
//! impl; this port names each family in its method docs.
//!
//! ## Label cardinality (DESIGN §3.11.5 "Label cardinality")
//!
//! Every label is a closed, enumerated value set — modeled here as `enum`s
//! with `const fn as_str()`. Unbounded identifiers (`tenant_id`,
//! `resource_id`, `gts_id`, `trace_id`, idempotency keys) MUST NOT appear
//! as metric labels; they belong in structured logs and traces.

use toolkit_macros::domain_model;

/// Label key constants shared by the instrument families below.
pub mod key {
    /// `operation` — the domain operation (PDP) or SPI method (plugin host).
    pub const OPERATION: &str = "operation";
    /// `cause` — PDP-failure cause discriminator.
    pub const CAUSE: &str = "cause";
    /// `decision` — PDP permit/deny decision.
    pub const DECISION: &str = "decision";
    /// `error_category` — plugin-host backend-error classification.
    pub const ERROR_CATEGORY: &str = "error_category";
    /// `outcome` — request/record completion outcome.
    pub const OUTCOME: &str = "outcome";
    /// `record_kind` — usage vs compensation record.
    pub const RECORD_KIND: &str = "record_kind";
    /// `query_kind` — aggregated vs raw query.
    pub const QUERY_KIND: &str = "query_kind";
}

/// `operation` label for the PDP-helper instruments (`uc_pdp_*`,
/// `uc_authz_decisions_total`) — the nine-value gateway set from DESIGN
/// §3.11.5.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdpOp {
    /// Usage-record ingestion (single + batch emit).
    Ingest,
    /// Raw (non-aggregated) usage-record listing.
    QueryRaw,
    /// Aggregated usage-record query.
    QueryAggregated,
    /// Read a single usage record by id.
    GetRecord,
    /// Deactivate a usage record.
    Deactivate,
    /// Register a usage type.
    UsageTypeCreate,
    /// Read a single usage type.
    UsageTypeGet,
    /// List usage types.
    UsageTypeList,
    /// Delete a usage type.
    UsageTypeDelete,
}

impl PdpOp {
    /// The bounded `operation` label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ingest => "ingest",
            Self::QueryRaw => "query_raw",
            Self::QueryAggregated => "query_aggregated",
            Self::GetRecord => "get_record",
            Self::Deactivate => "deactivate",
            Self::UsageTypeCreate => "usage_type_create",
            Self::UsageTypeGet => "usage_type_get",
            Self::UsageTypeList => "usage_type_list",
            Self::UsageTypeDelete => "usage_type_delete",
        }
    }
}

/// `operation` label for the plugin-host instruments
/// (`uc_plugin_call_duration_seconds`, `uc_plugin_accept_errors_total`) —
/// the ten Plugin SPI method names from DESIGN §3.11.5.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginOp {
    /// SPI Method 1.
    CreateUsageRecord,
    /// SPI Method 2.
    CreateUsageRecords,
    /// SPI Method 3.
    QueryAggregatedUsageRecords,
    /// SPI Method 4.
    ListUsageRecords,
    /// SPI Method 10.
    GetUsageRecord,
    /// SPI Method 5.
    DeactivateUsageRecord,
    /// SPI Method 6.
    CreateUsageType,
    /// SPI Method 7.
    GetUsageType,
    /// SPI Method 8.
    ListUsageTypes,
    /// SPI Method 9.
    DeleteUsageType,
}

impl PluginOp {
    /// The bounded `operation` label value (verbatim SPI method name).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CreateUsageRecord => "create_usage_record",
            Self::CreateUsageRecords => "create_usage_records",
            Self::QueryAggregatedUsageRecords => "query_aggregated_usage_records",
            Self::ListUsageRecords => "list_usage_records",
            Self::GetUsageRecord => "get_usage_record",
            Self::DeactivateUsageRecord => "deactivate_usage_record",
            Self::CreateUsageType => "create_usage_type",
            Self::GetUsageType => "get_usage_type",
            Self::ListUsageTypes => "list_usage_types",
            Self::DeleteUsageType => "delete_usage_type",
        }
    }
}

/// `cause` label for `uc_pdp_failures_total`.
///
/// **v1 mapping:** the bootstrap-bound `PolicyEnforcer` surfaces
/// `AuthZResolverError` (via `EnforcerError::EvaluationFailed`) which carries
/// no timeout discriminator, so every PDP failure maps to
/// [`PdpFailureCause::Unreachable`]. [`PdpFailureCause::Timeout`] is reserved
/// for a future host-side PDP-dispatch deadline (none exists in v1).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdpFailureCause {
    /// PDP unreachable / evaluation failed.
    Unreachable,
    /// Reserved for a future host-side dispatch deadline (not emitted in v1).
    Timeout,
    /// The enforcer returned an outcome class this gear does not know: an
    /// internal failure, neither a decision nor an unreachable PDP.
    Internal,
}

impl PdpFailureCause {
    /// The bounded `cause` label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unreachable => "unreachable",
            Self::Timeout => "timeout",
            Self::Internal => "internal",
        }
    }
}

/// `decision` label for `uc_authz_decisions_total`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthzDecision {
    /// The gear permitted the request: the PDP permitted AND the post-permit
    /// gate (per-record attribution / query scope projection) admitted it.
    Permit,
    /// The gear denied the request: a PDP deny (`EnforcerError::AccessDenied`), a
    /// permitted scope that matches nothing (`AccessDenial::ScopeMatchesNothing`),
    /// a fail-closed compile failure (`EnforcerError::CompileFailed`), or a
    /// permit-with-constraints the post-permit gate rejected (e.g. cross-tenant
    /// attribution outside the granted scope).
    Deny,
}

impl AuthzDecision {
    /// The bounded `decision` label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Permit => "permit",
            Self::Deny => "deny",
        }
    }
}

/// `error_category` label for `uc_plugin_accept_errors_total`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginErrorCategory {
    /// Structural-unready short-circuit (no plugin handle resolved).
    Unready,
    /// Plugin returned a backend-classified fault (`Transient` / `Internal`).
    BackendError,
    /// Host-side dispatch deadline expiry (reserved; no deadline exists in v1).
    Timeout,
}

impl PluginErrorCategory {
    /// The bounded `error_category` label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unready => "unready",
            Self::BackendError => "backend_error",
            Self::Timeout => "timeout",
        }
    }
}

// ── Phase 2: per-component gateway label vocabularies (DESIGN §3.11.5) ──

/// `outcome` label shared by the query, deactivation, and usage-type request
/// counters (their §3.11.5 vocabularies are identical: `success` on a
/// successful return, `denied` on a completed PDP deny, `error` otherwise).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestOutcome {
    /// Successful completion.
    Success,
    /// A completed PDP deny decision (the request was authorized against and denied).
    Denied,
    /// Any non-deny failure completion.
    Error,
}

impl RequestOutcome {
    /// The bounded `outcome` label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Denied => "denied",
            Self::Error => "error",
        }
    }
}

/// `outcome` label for `uc_ingestion_requests_total` — maps to the HTTP
/// `200` / `207` / request-wide-`Problem` tri-state.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestRequestOutcome {
    /// All records accepted (HTTP 200).
    Accepted,
    /// At least one per-record rejection (HTTP 207).
    Partial,
    /// Request-wide rejection (a `Problem` envelope).
    Rejected,
}

impl IngestRequestOutcome {
    /// The bounded `outcome` label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Partial => "partial",
            Self::Rejected => "rejected",
        }
    }
}

/// `error_category` label for `uc_ingestion_requests_total` (request-wide).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestRequestErrorCategory {
    /// `outcome` was `accepted` / `partial` (no request-wide reason).
    None,
    /// Reserved/defensive — unauthenticated calls are rejected upstream.
    MissingSecurityContext,
    /// Whole-request plugin transport / readiness / persistence failure.
    PluginError,
}

impl IngestRequestErrorCategory {
    /// The bounded `error_category` label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::MissingSecurityContext => "missing_security_context",
            Self::PluginError => "plugin_error",
        }
    }
}

/// `outcome` label for `uc_ingestion_records_total` (per-record). `Duplicate`
/// is reserved — the Method 1/2 SPI returns `Ok` indistinguishably for a
/// fresh persist and an exact-equality replay, so it is never emitted in v1.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOutcome {
    /// Record accepted (fresh persist or silent-absorb idempotent replay).
    Accepted,
    /// Reserved — not emitted in v1 (no SPI dedup signal).
    Duplicate,
    /// Per-record rejection.
    Rejected,
}

impl RecordOutcome {
    /// The bounded `outcome` label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Duplicate => "duplicate",
            Self::Rejected => "rejected",
        }
    }
}

/// `record_kind` label for `uc_ingestion_records_total`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    /// Ordinary usage record (`corrects_id` unset).
    Usage,
    /// Compensation record (`corrects_id` set).
    Compensation,
}

impl RecordKind {
    /// The bounded `record_kind` label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::Compensation => "compensation",
        }
    }
}

/// `error_category` label for `uc_ingestion_records_total` (per-record).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordErrorCategory {
    /// `outcome` was `accepted` (no per-record reason).
    None,
    /// PDP deny for this record's attribution tuple.
    Authz,
    /// Catalog-absent `UsageType` (`NotFound` with the usage-type resource).
    UnknownUsageType,
    /// Counter/gauge semantics violation or an L1 `corrects_id` referential fault.
    SemanticsViolation,
    /// Metadata size-cap or closed-shape rejection (the sole metadata category).
    MetadataSize,
    /// Same-key canonical-field mismatch.
    IdempotencyConflict,
    /// Per-record plugin transport / readiness / persistence fault.
    PluginError,
}

impl RecordErrorCategory {
    /// The bounded `error_category` label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Authz => "authz",
            Self::UnknownUsageType => "unknown_usage_type",
            Self::SemanticsViolation => "semantics_violation",
            Self::MetadataSize => "metadata_size",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::PluginError => "plugin_error",
        }
    }
}

/// `query_kind` label for the query-gateway instruments.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryKind {
    /// Aggregated query (`query_aggregated_usage_records`).
    Aggregated,
    /// Raw listing (`list_usage_records`).
    Raw,
}

impl QueryKind {
    /// The bounded `query_kind` label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Aggregated => "aggregated",
            Self::Raw => "raw",
        }
    }
}

/// `error_category` label for `uc_query_requests_total`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryErrorCategory {
    /// `outcome` was `success`.
    None,
    /// Reserved/defensive — rejected upstream / unreachable on SDK.
    MissingSecurityContext,
    /// PDP deny (or empty-constraint fail-closed) / substrate-unreachable authz exit.
    Authz,
    /// Unregistered `UsageType` surfaced from the plugin as `NotFound`.
    UnknownUsageType,
    /// Cursor decode failure (REST-handler boundary; reserved at this seam).
    CursorDecode,
    /// Cursor `$orderby` mismatch (REST-handler boundary; reserved at this seam).
    OrderMismatch,
    /// Cursor `$filter` mismatch (REST-handler boundary; reserved at this seam).
    FilterMismatch,
    /// Missing / one-sided mandatory bounded time window (scan-scope budget guard).
    QueryBudget,
    /// Plugin transport / readiness / backend failure.
    PluginError,
}

impl QueryErrorCategory {
    /// The bounded `error_category` label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::MissingSecurityContext => "missing_security_context",
            Self::Authz => "authz",
            Self::UnknownUsageType => "unknown_usage_type",
            Self::CursorDecode => "cursor_decode",
            Self::OrderMismatch => "order_mismatch",
            Self::FilterMismatch => "filter_mismatch",
            Self::QueryBudget => "query_budget",
            Self::PluginError => "plugin_error",
        }
    }
}

/// `error_category` label for `uc_deactivation_requests_total`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeactivationErrorCategory {
    /// `outcome` was `success`.
    None,
    /// Reserved/defensive — rejected upstream / unreachable on SDK.
    MissingSecurityContext,
    /// PDP deny (metric records the true denial) or PDP fail-closed.
    Authz,
    /// Prefetch or Method-5 `UsageRecordNotFound`.
    NotFound,
    /// Target record was already `inactive`.
    AlreadyInactive,
    /// Plugin transport / readiness / persistence fault.
    PluginError,
}

impl DeactivationErrorCategory {
    /// The bounded `error_category` label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::MissingSecurityContext => "missing_security_context",
            Self::Authz => "authz",
            Self::NotFound => "not_found",
            Self::AlreadyInactive => "already_inactive",
            Self::PluginError => "plugin_error",
        }
    }
}

/// `operation` label for `uc_usage_type_requests_total`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageTypeOp {
    /// Register a usage type.
    Create,
    /// Read a single usage type.
    Get,
    /// List usage types.
    List,
    /// Delete a usage type.
    Delete,
}

impl UsageTypeOp {
    /// The bounded `operation` label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Get => "get",
            Self::List => "list",
            Self::Delete => "delete",
        }
    }
}

/// `error_category` label for `uc_usage_type_requests_total`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageTypeErrorCategory {
    /// `outcome` was `success`.
    None,
    /// Reserved/defensive — rejected upstream / unreachable on SDK.
    MissingSecurityContext,
    /// PDP deny.
    Authz,
    /// Request-shape / kind / shape-validation rejection.
    Validation,
    /// Duplicate registration on create (HTTP 409).
    Conflict,
    /// `UsageTypeNotFound` on get / delete.
    NotFound,
    /// Referentially-unsafe delete rejection (HTTP 409).
    Referenced,
    /// Plugin transport / availability / persistence failure.
    PluginError,
}

impl UsageTypeErrorCategory {
    /// The bounded `error_category` label value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::MissingSecurityContext => "missing_security_context",
            Self::Authz => "authz",
            Self::Validation => "validation",
            Self::Conflict => "conflict",
            Self::NotFound => "not_found",
            Self::Referenced => "referenced",
            Self::PluginError => "plugin_error",
        }
    }
}

/// Output port for recording usage-collector operational metrics.
///
/// Phase 1 declares the foundation-owned instruments — the plugin-host set
/// (`uc_plugin_ready`, `uc_plugin_accept_errors_total`,
/// `uc_plugin_call_duration_seconds`) and the PDP-helper set
/// (`uc_pdp_ready`, `uc_pdp_failures_total`, `uc_pdp_duration_seconds`,
/// `uc_authz_decisions_total`). Phase 2 adds the per-component gateway
/// instruments (ingestion, query, deactivation, usage-type).
pub trait UsageCollectorMetrics: Send + Sync {
    /// `uc_pdp_ready` gauge — set to `1` while the `authz-resolver` client is
    /// bound in the bootstrap-constructed `PolicyEnforcer`, `0` otherwise.
    fn set_pdp_ready(&self, ready: bool);

    /// Observe a completed PDP authorization: `uc_pdp_duration_seconds{operation}`
    /// plus `uc_authz_decisions_total{operation, decision}`.
    ///
    /// `decision` is the **effective** gear decision, not the raw
    /// `access_scope_with` return: a permit-with-constraints that the domain's
    /// post-permit gate rejects (cross-tenant attribution, an un-projectable row
    /// scope) is recorded as [`AuthzDecision::Deny`]. Exactly one of this method
    /// or [`Self::record_pdp_failure`] fires per authorization.
    fn record_pdp_decision(&self, op: PdpOp, decision: AuthzDecision, seconds: f64);

    /// Observe a PDP failure (transport / evaluation):
    /// `uc_pdp_duration_seconds{operation}` plus
    /// `uc_pdp_failures_total{operation, cause}`. A failure completion is
    /// still a completion, so the duration is observed.
    fn record_pdp_failure(&self, op: PdpOp, cause: PdpFailureCause, seconds: f64);

    /// `uc_plugin_ready` gauge — set to `1` iff the active plugin binding is
    /// resolved structurally (selector cached AND scoped client registered).
    fn set_plugin_ready(&self, ready: bool);

    /// Observe a completed Plugin SPI dispatch (success or error):
    /// `uc_plugin_call_duration_seconds{operation}`.
    fn record_plugin_call(&self, op: PluginOp, seconds: f64);

    /// Increment `uc_plugin_accept_errors_total{operation, error_category}` —
    /// only for structural-unready short-circuits and backend-classified
    /// faults, never for deterministic domain-typed plugin variants.
    fn record_plugin_accept_error(&self, op: PluginOp, category: PluginErrorCategory);

    // ── Ingestion gateway (usage-emission) ──

    /// Observe `uc_ingestion_batch_size` — one observation per received batch
    /// submission, before per-record processing.
    fn observe_ingestion_batch_size(&self, size: u64);

    /// Observe `uc_ingestion_duration_seconds` (label-free) — one per
    /// completed ingestion request (single-emit call or batch submission).
    fn observe_ingestion_duration(&self, seconds: f64);

    /// Observe `uc_record_metadata_bytes` — one per submitted record that
    /// carries metadata (recorded before the size-cap comparison).
    fn observe_record_metadata_bytes(&self, bytes: u64);

    /// Increment `uc_ingestion_records_total{outcome, record_kind, error_category}`
    /// once per record in a batch acknowledgement (and once for a single emit).
    fn record_ingestion_record(
        &self,
        outcome: RecordOutcome,
        kind: RecordKind,
        error_category: RecordErrorCategory,
    );

    /// Increment `uc_ingestion_requests_total{outcome, error_category}` — once
    /// per completed batch-submission request (not on the single-emit path).
    fn record_ingestion_request(
        &self,
        outcome: IngestRequestOutcome,
        error_category: IngestRequestErrorCategory,
    );

    // ── Query gateway (usage-query) ──

    /// Increment `uc_query_inflight{query_kind}` on query-gateway entry once
    /// authorization composes.
    fn query_inflight_inc(&self, kind: QueryKind);

    /// Decrement `uc_query_inflight{query_kind}` — only on exits that followed
    /// a matching [`Self::query_inflight_inc`].
    fn query_inflight_dec(&self, kind: QueryKind);

    /// Observe `uc_query_result_rows{query_kind}` with the page size / group
    /// count — recorded only on a successful query completion.
    fn observe_query_result_rows(&self, kind: QueryKind, rows: u64);

    /// Observe `uc_query_duration_seconds{query_kind}` plus increment
    /// `uc_query_requests_total{query_kind, outcome, error_category}` — once
    /// per completed query attempt.
    fn record_query_request(
        &self,
        kind: QueryKind,
        outcome: RequestOutcome,
        error_category: QueryErrorCategory,
        seconds: f64,
    );

    // ── Deactivation handler (event-deactivation) ──

    /// Observe `uc_deactivation_duration_seconds` plus increment
    /// `uc_deactivation_requests_total{outcome, error_category}` — once per
    /// completed deactivation attempt.
    fn record_deactivation_request(
        &self,
        outcome: RequestOutcome,
        error_category: DeactivationErrorCategory,
        seconds: f64,
    );

    // ── UsageType catalog (usage-type-lifecycle) ──

    /// Increment `uc_usage_type_requests_total{operation, outcome, error_category}`
    /// once per completed UsageType-lifecycle attempt.
    fn record_usage_type_request(
        &self,
        op: UsageTypeOp,
        outcome: RequestOutcome,
        error_category: UsageTypeErrorCategory,
    );

    /// Set `uc_usage_types` (no labels) to the current catalog entry count.
    fn set_usage_types(&self, count: u64);
}

/// No-op implementation for tests and pre-bootstrap contexts.
///
/// A [`crate::infra::metrics::UcMetricsMeter`] bound to the process-global
/// `NoopMeterProvider` is already effectively inert, but this ZST avoids
/// constructing any meter at all where metrics are irrelevant.
#[domain_model]
#[allow(dead_code)] // constructed only by test / pre-init builds
pub struct NoopMetrics;

impl UsageCollectorMetrics for NoopMetrics {
    fn set_pdp_ready(&self, _: bool) {}
    fn record_pdp_decision(&self, _: PdpOp, _: AuthzDecision, _: f64) {}
    fn record_pdp_failure(&self, _: PdpOp, _: PdpFailureCause, _: f64) {}
    fn set_plugin_ready(&self, _: bool) {}
    fn record_plugin_call(&self, _: PluginOp, _: f64) {}
    fn record_plugin_accept_error(&self, _: PluginOp, _: PluginErrorCategory) {}
    fn observe_ingestion_batch_size(&self, _: u64) {}
    fn observe_ingestion_duration(&self, _: f64) {}
    fn observe_record_metadata_bytes(&self, _: u64) {}
    fn record_ingestion_record(&self, _: RecordOutcome, _: RecordKind, _: RecordErrorCategory) {}
    fn record_ingestion_request(&self, _: IngestRequestOutcome, _: IngestRequestErrorCategory) {}
    fn query_inflight_inc(&self, _: QueryKind) {}
    fn query_inflight_dec(&self, _: QueryKind) {}
    fn observe_query_result_rows(&self, _: QueryKind, _: u64) {}
    fn record_query_request(&self, _: QueryKind, _: RequestOutcome, _: QueryErrorCategory, _: f64) {
    }
    fn record_deactivation_request(&self, _: RequestOutcome, _: DeactivationErrorCategory, _: f64) {
    }
    fn record_usage_type_request(
        &self,
        _: UsageTypeOp,
        _: RequestOutcome,
        _: UsageTypeErrorCategory,
    ) {
    }
    fn set_usage_types(&self, _: u64) {}
}
