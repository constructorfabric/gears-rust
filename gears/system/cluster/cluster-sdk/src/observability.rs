// Created: 2026-06-10 by Constructor Tech
//! Versioned observability naming contract for the cluster SDK (ADR-004).
//!
//! Observability signals are part of the cluster contract, on par with Rust
//! trait signatures. Every follow-up plugin emits spans, metrics, and log
//! events against the stable names defined here so that operator dashboards
//! and alerts are portable across providers and survive plugin minor versions.
//! The concrete catalog — with attribute keys, label sets, and severities — is
//! maintained in [`docs/OBSERVABILITY.md`]. Renaming, removing, or relabeling a
//! signal is a breaking change requiring a major SDK version bump; additions
//! are non-breaking.
//!
//! # Naming conventions
//!
//! - **Spans** — OpenTelemetry style, dotted lowercase: `cluster.<primitive>.<op>`.
//! - **Metrics** — Prometheus style, underscored lowercase: `cluster_<primitive>_<subject>_<unit>`.
//! - **Log events** — dotted lowercase event names emitted via `tracing` at the
//!   severity documented in the catalog.
//!
//! # Cardinality rule (hard contract)
//!
//! Operation keys, lock names, election names, and service instance IDs — the
//! values in [`fields::attr`] — **never** appear as metric labels. They are
//! unbounded and would explode Prometheus cardinality. They may appear only as
//! span attributes (traces are sampled) and log-event fields (log volume is
//! filter-controlled). Metric labels are restricted to the bounded, enum-like
//! dimensions in [`fields::label`], enumerated by [`METRIC_LABEL_ALLOWLIST`].
//!
//! [`docs/OBSERVABILITY.md`]: ../../docs/OBSERVABILITY.md

/// OpenTelemetry span names (dotted lowercase). One per public facade
/// operation. Drop unsubscribes a watch — no span is defined for it.
pub mod spans {
    // Cache primitive.
    /// Span covering `ClusterCacheV1::get`.
    pub const CACHE_GET: &str = "cluster.cache.get";
    /// Span covering `ClusterCacheV1::put`.
    pub const CACHE_PUT: &str = "cluster.cache.put";
    /// Span covering `ClusterCacheV1::delete`.
    pub const CACHE_DELETE: &str = "cluster.cache.delete";
    /// Span covering `ClusterCacheV1::contains`.
    pub const CACHE_CONTAINS: &str = "cluster.cache.contains";
    /// Span covering `ClusterCacheV1::put_if_absent`.
    pub const CACHE_PUT_IF_ABSENT: &str = "cluster.cache.put_if_absent";
    /// Span covering `ClusterCacheV1::compare_and_swap`.
    pub const CACHE_COMPARE_AND_SWAP: &str = "cluster.cache.compare_and_swap";
    /// Span covering `ClusterCacheV1::watch`.
    pub const CACHE_WATCH: &str = "cluster.cache.watch";
    /// Span covering `ClusterCacheV1::watch_prefix`.
    pub const CACHE_WATCH_PREFIX: &str = "cluster.cache.watch_prefix";

    // Leader-election primitive.
    /// Span covering `LeaderElectionV1::elect` / `elect_with_config`.
    pub const LEADER_ELECT: &str = "cluster.leader.elect";
    /// Span covering a single background claim renewal.
    pub const LEADER_RENEW: &str = "cluster.leader.renew";
    /// Span covering `LeaderWatch::resign`.
    pub const LEADER_RESIGN: &str = "cluster.leader.resign";

    // Distributed-lock primitive.
    /// Span covering `DistributedLockV1::try_lock`.
    pub const LOCK_TRY_LOCK: &str = "cluster.lock.try_lock";
    /// Span covering `DistributedLockV1::lock`.
    pub const LOCK_LOCK: &str = "cluster.lock.lock";
    /// Span covering `LockGuard::renew`.
    pub const LOCK_RENEW: &str = "cluster.lock.renew";
    /// Span covering `LockGuard::release`.
    pub const LOCK_RELEASE: &str = "cluster.lock.release";

    // Service-discovery primitive.
    /// Span covering `ServiceDiscoveryV1::register`.
    pub const DISCOVERY_REGISTER: &str = "cluster.discovery.register";
    /// Span covering `ServiceDiscoveryV1::discover`.
    pub const DISCOVERY_DISCOVER: &str = "cluster.discovery.discover";
    /// Span covering `ServiceDiscoveryV1::watch`.
    pub const DISCOVERY_WATCH: &str = "cluster.discovery.watch";
    /// Span covering `ServiceHandle::deregister`.
    pub const DISCOVERY_DEREGISTER: &str = "cluster.discovery.deregister";
}

/// Prometheus metric names (underscored lowercase). Labels are restricted to
/// [`METRIC_LABEL_ALLOWLIST`].
pub mod metrics {
    /// Counter of cache operations. Labels: `provider`, `op`, `result`.
    pub const CACHE_OPS_TOTAL: &str = "cluster_cache_ops_total";
    /// Histogram of cache operation latency in seconds. Labels: `provider`, `op`.
    pub const CACHE_OP_DURATION_SECONDS: &str = "cluster_cache_op_duration_seconds";
    /// Counter of lock operations. Labels: `provider`, `op`, `result`.
    pub const LOCK_OPS_TOTAL: &str = "cluster_lock_ops_total";
    /// Histogram of lock operation latency in seconds. Labels: `provider`, `op`.
    pub const LOCK_OP_DURATION_SECONDS: &str = "cluster_lock_op_duration_seconds";
    /// Counter of leadership transitions. Labels: `provider`, `transition`.
    pub const LEADER_TRANSITIONS_TOTAL: &str = "cluster_leader_transitions_total";
    /// Counter of service-discovery operations. Labels: `provider`, `op`, `result`.
    pub const DISCOVERY_OPS_TOTAL: &str = "cluster_discovery_ops_total";
    /// Counter of watch resets/resubscriptions. Labels: `provider`, `primitive`.
    pub const WATCH_RESETS_TOTAL: &str = "cluster_watch_resets_total";
    /// Counter of provider/backend errors. Labels: `provider`, `kind`.
    pub const PROVIDER_ERRORS_TOTAL: &str = "cluster_provider_errors_total";
}

/// Structured log-event names (dotted lowercase), emitted via `tracing` at the
/// severity documented in the catalog (leader transitions at INFO, watch resets
/// at WARN, provider errors at ERROR).
pub mod logs {
    /// A leadership transition occurred (INFO).
    pub const LEADER_TRANSITION: &str = "cluster.leader.transition";
    /// A watch terminally closed and was resubscribed (WARN).
    pub const WATCH_RESET: &str = "cluster.watch.reset";
    /// A backend/provider operation failed (ERROR).
    pub const PROVIDER_ERROR: &str = "cluster.provider.error";
}

/// Field/attribute key names, split by cardinality class.
pub mod fields {
    /// Low-cardinality, enum-like keys permitted as metric labels (and also
    /// usable as span attributes / log fields).
    pub mod label {
        /// The concrete backend/provider name.
        pub const PROVIDER: &str = "provider";
        /// The facade operation (e.g. `get`, `try_lock`).
        pub const OP: &str = "op";
        /// The bounded operation outcome (e.g. `ok`, `conflict`, `timeout`).
        pub const RESULT: &str = "result";
        /// A leadership transition kind (e.g. `acquired`, `lost`).
        pub const TRANSITION: &str = "transition";
        /// The provider-error retryability class.
        pub const KIND: &str = "kind";
        /// The primitive (e.g. `cache`, `lock`, `leader`, `discovery`).
        pub const PRIMITIVE: &str = "primitive";
    }

    /// High-cardinality keys that carry user-supplied or unbounded values.
    /// Permitted only as span attributes or log fields — never as metric
    /// labels (see the crate-level cardinality rule).
    pub mod attr {
        /// A cache key.
        pub const KEY: &str = "key";
        /// A coordination name (service name, generic name).
        pub const NAME: &str = "name";
        /// A lock name.
        pub const LOCK: &str = "lock";
        /// An election name.
        pub const ELECTION: &str = "election";
        /// A service instance ID.
        pub const INSTANCE_ID: &str = "instance_id";
        /// The cluster profile.
        pub const PROFILE: &str = "profile";
    }
}

/// The exhaustive set of label keys permitted on cluster metrics. Any field not
/// in this list — in particular every key in [`fields::attr`] — must not be
/// attached as a metric label.
pub const METRIC_LABEL_ALLOWLIST: &[&str] = &[
    fields::label::PROVIDER,
    fields::label::OP,
    fields::label::RESULT,
    fields::label::TRANSITION,
    fields::label::KIND,
    fields::label::PRIMITIVE,
];

/// Bounded, enum-like `result` label values (see [`fields::label::RESULT`]).
///
/// These are the only values that may be attached as the `result` metric label;
/// they are derived from a [`ClusterError`] by the `label` / `from_error`
/// functions below, never from a free-form string. Keeping them bounded preserves
/// the cardinality rule.
pub mod result {
    /// The operation succeeded.
    pub const OK: &str = "ok";
    /// A compare-and-swap / version conflict ([`ClusterError::CasConflict`]).
    pub const CONFLICT: &str = "conflict";
    /// A non-blocking lock acquisition found the lock held
    /// ([`ClusterError::LockContended`]).
    pub const CONTENDED: &str = "contended";
    /// A blocking acquisition timed out, or a backend op timed out
    /// ([`ClusterError::LockTimeout`] / a [`ProviderErrorKind::Timeout`] provider
    /// error).
    pub const TIMEOUT: &str = "timeout";
    /// A lease lapsed before the operation completed
    /// ([`ClusterError::LockExpired`]).
    pub const EXPIRED: &str = "expired";
    /// The subsystem was shutting down ([`ClusterError::Shutdown`]).
    pub const SHUTDOWN: &str = "shutdown";
    /// A required feature was unsupported by the bound backend
    /// ([`ClusterError::Unsupported`]).
    pub const UNSUPPORTED: &str = "unsupported";
    /// Any other failure (including non-timeout provider errors).
    pub const ERROR: &str = "error";

    use super::ClusterError;

    /// Maps an operation outcome to its bounded [`result`](self) label.
    ///
    /// This is the single mapping every instrumentation site uses, so the
    /// `result` label vocabulary stays bounded and consistent across providers.
    pub fn label<T>(outcome: &Result<T, ClusterError>) -> &'static str {
        match outcome {
            Ok(_) => OK,
            Err(err) => from_error(err),
        }
    }

    /// Maps a failure to its bounded [`result`](self) label.
    #[must_use]
    pub fn from_error(err: &ClusterError) -> &'static str {
        match err {
            ClusterError::CasConflict { .. } => CONFLICT,
            ClusterError::LockContended { .. } => CONTENDED,
            ClusterError::LockExpired { .. } => EXPIRED,
            ClusterError::Shutdown => SHUTDOWN,
            ClusterError::Unsupported { .. } => UNSUPPORTED,
            // A blocking-acquire timeout and a backend timeout share the bounded
            // `timeout` outcome.
            ClusterError::LockTimeout { .. }
            | ClusterError::Provider {
                kind: super::ProviderErrorKind::Timeout,
                ..
            } => TIMEOUT,
            _ => ERROR,
        }
    }
}

/// Bounded `transition` label values for leadership transitions (see
/// [`fields::label::TRANSITION`]).
pub mod transition {
    /// Leadership was acquired (a follower/candidate became leader).
    pub const ACQUIRED: &str = "acquired";
    /// Leadership was lost involuntarily (claim expired / overtaken).
    pub const LOST: &str = "lost";
    /// Leadership was voluntarily resigned.
    pub const RESIGNED: &str = "resigned";
}

/// Bounded `kind` label values for provider errors, mirroring
/// [`ProviderErrorKind`] (see [`fields::label::KIND`]).
pub mod kind {
    use super::ProviderErrorKind;

    /// The connection to the backend was lost.
    pub const CONNECTION_LOST: &str = "connection_lost";
    /// A backend operation timed out.
    pub const TIMEOUT: &str = "timeout";
    /// Authentication against the backend failed.
    pub const AUTH_FAILURE: &str = "auth_failure";
    /// The backend rejected the operation due to resource exhaustion.
    pub const RESOURCE_EXHAUSTED: &str = "resource_exhausted";
    /// Any other backend error.
    pub const OTHER: &str = "other";

    /// Maps a [`ProviderErrorKind`] to its bounded [`kind`](self) label.
    #[must_use]
    pub fn label(kind: ProviderErrorKind) -> &'static str {
        match kind {
            ProviderErrorKind::ConnectionLost => CONNECTION_LOST,
            ProviderErrorKind::Timeout => TIMEOUT,
            ProviderErrorKind::AuthFailure => AUTH_FAILURE,
            ProviderErrorKind::ResourceExhausted => RESOURCE_EXHAUSTED,
            ProviderErrorKind::Other => OTHER,
        }
    }
}

/// Bounded `primitive` label values (see [`fields::label::PRIMITIVE`]).
pub mod primitive {
    /// The cache primitive.
    pub const CACHE: &str = "cache";
    /// The distributed-lock primitive.
    pub const LOCK: &str = "lock";
    /// The leader-election primitive.
    pub const LEADER: &str = "leader";
    /// The service-discovery primitive.
    pub const DISCOVERY: &str = "discovery";
}

/// The metrics sink the SDK instrumentation emits through.
///
/// An OTel-agnostic output port so the SDK core carries no OpenTelemetry
/// dependency and the emission sites (the [`InstrumentedCache`](crate::InstrumentedCache)
/// decorator and the SDK default backends) stay testable with a recording
/// double. The concrete OpenTelemetry adapter
/// [`OtelClusterMetrics`](crate::observability::otel::OtelClusterMetrics) lives
/// behind the `otel` crate feature.
///
/// The `provider` label is fixed when a concrete sink is constructed (one sink
/// per provider in a deployment), so it is not a per-call argument. Every method
/// takes only the bounded, allowlisted label values from this module — never a
/// high-cardinality key, lock name, or instance id (the cardinality rule).
///
/// Spans and log events are emitted directly via `tracing` at the instrumentation
/// sites (they need no sink); only metrics flow through this port.
pub trait ClusterMetrics: Send + Sync {
    /// Records one cache operation. `op` is a facade op (e.g. `get`); `result` is
    /// a [`result`](self::result) value. Backs [`metrics::CACHE_OPS_TOTAL`].
    fn cache_op(&self, op: &str, result: &str);
    /// Records a cache operation's latency. Backs
    /// [`metrics::CACHE_OP_DURATION_SECONDS`].
    fn cache_op_duration(&self, op: &str, seconds: f64);
    /// Records one lock operation. Backs [`metrics::LOCK_OPS_TOTAL`].
    fn lock_op(&self, op: &str, result: &str);
    /// Records a lock operation's latency. Backs
    /// [`metrics::LOCK_OP_DURATION_SECONDS`].
    fn lock_op_duration(&self, op: &str, seconds: f64);
    /// Records a leadership transition. `transition` is a
    /// [`transition`](self::transition) value. Backs
    /// [`metrics::LEADER_TRANSITIONS_TOTAL`].
    fn leader_transition(&self, transition: &str);
    /// Records one service-discovery operation. Backs
    /// [`metrics::DISCOVERY_OPS_TOTAL`].
    fn discovery_op(&self, op: &str, result: &str);
    /// Records a watch reset/resubscription for `primitive` (a
    /// [`primitive`](self::primitive) value). Backs
    /// [`metrics::WATCH_RESETS_TOTAL`].
    fn watch_reset(&self, primitive: &str);
    /// Records a backend/provider error of bounded `kind` (a [`kind`](self::kind)
    /// value). Backs [`metrics::PROVIDER_ERRORS_TOTAL`].
    fn provider_error(&self, kind: &str);
}

/// A [`ClusterMetrics`] that discards every signal.
///
/// The default sink when no telemetry is wired (tests, the zero-infrastructure
/// standalone dev path).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopMetrics;

impl ClusterMetrics for NoopMetrics {
    fn cache_op(&self, _op: &str, _result: &str) {}
    fn cache_op_duration(&self, _op: &str, _seconds: f64) {}
    fn lock_op(&self, _op: &str, _result: &str) {}
    fn lock_op_duration(&self, _op: &str, _seconds: f64) {}
    fn leader_transition(&self, _transition: &str) {}
    fn discovery_op(&self, _op: &str, _result: &str) {}
    fn watch_reset(&self, _primitive: &str) {}
    fn provider_error(&self, _kind: &str) {}
}

use crate::error::{ClusterError, ProviderErrorKind};

/// The high-cardinality resource identifier a provider error pertains to. It is
/// emitted as a **log field** on `cluster.provider.error` (never a metric label,
/// per the cardinality rule); the variant selects the contracted field name from
/// OBSERVABILITY.md §6 (`key` / `lock` / `election` / `name`). `tracing` field
/// names must be static, so the field name is chosen by matching the variant
/// rather than passed dynamically.
#[doc(hidden)]
#[derive(Clone, Copy)]
pub enum ResourceId<'a> {
    /// A cache key — emitted as `key`.
    Key(&'a str),
    /// A lock name — emitted as `lock`.
    Lock(&'a str),
    /// An election name — emitted as `election`.
    Election(&'a str),
    /// A coordination / service name — emitted as `name`.
    Name(&'a str),
}

/// Emits the shared provider-error signals when `err` is a genuine backend
/// [`ClusterError::Provider`] error: increments `cluster_provider_errors_total`
/// (by bounded [`kind`](self::kind)) and logs the `cluster.provider.error` event
/// at ERROR with the `provider`, `op`, `kind`, the `resource` identifier, and
/// `message` fields. Normal outcomes — a CAS conflict, lock contention, a timeout
/// — are not provider errors and are skipped.
///
/// Shared by every instrumentation site (the [`InstrumentedCache`] decorator and
/// the SDK default backends) so the `kind` vocabulary and the ERROR severity
/// stay consistent across primitives and providers.
#[doc(hidden)]
#[allow(
    clippy::cognitive_complexity,
    reason = "four near-identical `tracing::event!` arms — one per contracted \
              resource field name (`key`/`lock`/`election`/`name`), which must be \
              a static identifier; the apparent complexity is macro expansion"
)]
pub fn emit_provider_error(
    metrics: &dyn ClusterMetrics,
    provider: &str,
    op: &str,
    resource: ResourceId<'_>,
    err: &ClusterError,
) {
    let ClusterError::Provider {
        kind: error_kind,
        message,
    } = err
    else {
        return;
    };
    let kind_label = kind::label(*error_kind);
    metrics.provider_error(kind_label);
    // One arm per contracted identifier field — `tracing` field names are static.
    match resource {
        ResourceId::Key(value) => tracing::event!(
            name: logs::PROVIDER_ERROR, tracing::Level::ERROR,
            provider = %provider, op, kind = kind_label, key = %value, message = %message,
            "cluster provider error"
        ),
        ResourceId::Lock(value) => tracing::event!(
            name: logs::PROVIDER_ERROR, tracing::Level::ERROR,
            provider = %provider, op, kind = kind_label, lock = %value, message = %message,
            "cluster provider error"
        ),
        ResourceId::Election(value) => tracing::event!(
            name: logs::PROVIDER_ERROR, tracing::Level::ERROR,
            provider = %provider, op, kind = kind_label, election = %value, message = %message,
            "cluster provider error"
        ),
        ResourceId::Name(value) => tracing::event!(
            name: logs::PROVIDER_ERROR, tracing::Level::ERROR,
            provider = %provider, op, kind = kind_label, name = %value, message = %message,
            "cluster provider error"
        ),
    }
}

mod instrumented_cache;
pub use instrumented_cache::InstrumentedCache;

#[cfg(feature = "otel")]
pub mod otel;

#[cfg(test)]
#[path = "observability_tests.rs"]
mod observability_tests;
