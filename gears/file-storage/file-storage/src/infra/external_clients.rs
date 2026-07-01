//! Optional external-service client abstractions (quota enforcement + usage reporting).
//!
//! Both clients are optional — `None` disables the feature (permissive quota, no
//! usage deltas). Grouping them here keeps the two tiny trait modules from adding
//! separate fan-out edges to every service that holds them.
//!
//! @cpt-cf-file-storage-fr-storage-quota
//! @cpt-cf-file-storage-fr-usage-reporting

use async_trait::async_trait;
use uuid::Uuid;

use crate::domain::error::DomainError;

// ── Quota Enforcement ─────────────────────────────────────────────────────────

/// The result of a quota preflight check.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotaDecision {
    /// The operation is within quota limits.
    Allowed,
    /// The operation would exceed quota.
    Denied { reason: String },
}

/// Quota Enforcement client — checks whether a storage-increasing operation is
/// permitted for an owner.
///
/// @cpt-cf-file-storage-fr-storage-quota
#[async_trait]
pub trait QuotaClient: Send + Sync {
    /// Check whether `owner_id` (of `owner_kind`) in `tenant_id` may store
    /// `additional_bytes` more. Returns `Allowed` or `Denied`.
    ///
    /// `metric_name` is the metric identifier used in the quota system
    /// (e.g. `"gts.cf.qe.metric.type.v1~cf.qe.metric.file_storage_bytes.v1"`).
    async fn check_storage_quota(
        &self,
        tenant_id: Uuid,
        owner_id: Uuid,
        additional_bytes: u64,
        metric_name: &str,
    ) -> Result<QuotaDecision, DomainError>;
}

// ── Usage Reporting ───────────────────────────────────────────────────────────

/// A usage delta to report to the Usage Collector.
///
/// Positive `bytes_delta` = storage gain (upload/create).
/// Negative `bytes_delta` = storage freed (delete).
/// `file_count_delta`: +1 when a file is created, -1 when deleted, 0 otherwise.
///
/// @cpt-cf-file-storage-fr-usage-reporting
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone)]
pub struct UsageDelta {
    pub tenant_id: Uuid,
    pub owner_id: Uuid,
    pub bytes_delta: i64,
    pub file_count_delta: i64,
}

/// Usage reporting adapter — fire-and-forget; failures must NOT propagate to callers.
///
/// @cpt-cf-file-storage-fr-usage-reporting
#[async_trait]
pub trait UsageReporter: Send + Sync {
    /// Report a storage-delta event. Must be infallible from the caller's perspective —
    /// implementations MUST log and swallow errors internally.
    async fn report(&self, delta: UsageDelta);
}
