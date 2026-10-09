//! Optional external-service client abstractions (quota enforcement + usage reporting).
//!
//! Both clients are optional: `None` disables the feature (permissive quota, no usage deltas).

use async_trait::async_trait;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// The result of a quota preflight check.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotaDecision {
    /// The operation is within quota limits.
    Allowed,
    /// The operation would exceed quota.
    Denied { reason: String },
}

/// Quota Enforcement client: checks whether a storage-increasing operation is permitted.
#[async_trait]
pub trait QuotaClient: Send + Sync {
    /// Check whether `owner_id` in `tenant_id` may store `additional_bytes` more.
    ///
    /// `metric_name` is the quota-system metric id
    /// (e.g. `gts_id!("cf.qe.metric.type.v1~cf.qe.metric.file_storage_bytes.v1")`).
    async fn check_storage_quota(
        &self,
        tenant_id: Uuid,
        owner_id: Uuid,
        additional_bytes: u64,
        metric_name: &str,
    ) -> Result<QuotaDecision, DomainError>;
}

/// A usage delta to report to the Usage Collector.
///
/// `bytes_delta` is positive for storage gained and negative for storage freed;
/// `file_count_delta` is +1 on file creation, -1 on deletion, 0 otherwise.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Debug, Clone)]
pub struct UsageDelta {
    pub tenant_id: Uuid,
    pub owner_id: Uuid,
    pub bytes_delta: i64,
    pub file_count_delta: i64,
}

/// Usage reporting adapter — fire-and-forget; failures must NOT propagate to callers.
#[async_trait]
pub trait UsageReporter: Send + Sync {
    /// Report a storage-delta event; implementations MUST log and swallow errors internally.
    async fn report(&self, delta: UsageDelta);
}
