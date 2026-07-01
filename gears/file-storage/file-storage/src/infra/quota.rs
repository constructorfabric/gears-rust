//! Quota Enforcement client abstraction.
//!
//! @cpt-cf-file-storage-fr-storage-quota

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
