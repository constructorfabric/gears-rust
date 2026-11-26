//! The operations over trusted admission, for the quota resolver plugin only
//! (ADR-0009). They are the operations of QE's API: the same implementation,
//! storage, engines and caches, admitted without a PDP call and confined to
//! the tenant each call names.
//!
//! The handle is crate-private: no REST route, in-process client, or other
//! crate reaches it. Policy management and Quota management are not on it.

use super::operations::ReturnRequest;
use std::sync::Arc;

use quota_enforcement_sdk::{
    AcquireLeaseOutcome, AcquireLeaseRequest, CommitLeaseRequest, DebitRequest, Decision,
    PageResult, QuotaSnapshot, ReleaseLeaseRequest, SnapshotRequest, TenantId,
};
use toolkit_macros::domain_model;
use toolkit_security::PlatformIdentity;

use super::error::DomainError;
use super::service::Service;

/// The trusted operations. Each call takes the tenant it acts on, the
/// platform identity of the resolver's caller (recorded as the actor, never
/// authorized by, `None` for a caller in the resolver's process), and the
/// request; the tenant replaces the one the request names.
#[domain_model]
pub struct TrustedOperations {
    service: Arc<Service>,
}

impl TrustedOperations {
    /// The trusted operations over the gear's bound service.
    pub fn new(service: Arc<Service>) -> Self {
        Self { service }
    }

    /// Debit, as [`super::operations::Operations::debit`].
    pub async fn debit(
        &self,
        tenant_id: TenantId,
        caller: Option<PlatformIdentity>,
        mut request: DebitRequest,
    ) -> Result<Decision, DomainError> {
        request.attribution.tenant_id = tenant_id;
        self.service
            .trusted_operations(caller)?
            .debit(request)
            .await
    }

    /// Lease acquisition, as [`super::operations::Operations::acquire_lease`].
    pub async fn acquire_lease(
        &self,
        tenant_id: TenantId,
        caller: Option<PlatformIdentity>,
        mut request: AcquireLeaseRequest,
    ) -> Result<AcquireLeaseOutcome, DomainError> {
        request.attribution.tenant_id = tenant_id;
        self.service
            .trusted_operations(caller)?
            .acquire_lease(request)
            .await
    }

    /// Lease commit, as [`super::operations::Operations::commit_lease`].
    pub async fn commit_lease(
        &self,
        tenant_id: TenantId,
        caller: Option<PlatformIdentity>,
        mut request: CommitLeaseRequest,
    ) -> Result<Decision, DomainError> {
        request.tenant_id = tenant_id;
        self.service
            .trusted_operations(caller)?
            .commit_lease(request)
            .await
    }

    /// Lease release, as [`super::operations::Operations::release_lease`].
    pub async fn release_lease(
        &self,
        tenant_id: TenantId,
        caller: Option<PlatformIdentity>,
        mut request: ReleaseLeaseRequest,
    ) -> Result<Decision, DomainError> {
        request.tenant_id = tenant_id;
        self.service
            .trusted_operations(caller)?
            .release_lease(request)
            .await
    }

    /// Return of part of a charge, as
    /// [`super::operations::Operations::return_amount`]. The resolver plugin
    /// is its only caller; QE's API has no return.
    pub async fn return_amount(
        &self,
        tenant_id: TenantId,
        caller: Option<PlatformIdentity>,
        mut request: ReturnRequest,
    ) -> Result<(), DomainError> {
        request.tenant_id = tenant_id;
        self.service
            .trusted_operations(caller)?
            .return_amount(request)
            .await
    }

    /// The snapshot read, as [`super::operations::Operations::snapshot`],
    /// except that a subject type the metric does not admit narrows the read
    /// to the tenant's Quotas.
    pub async fn snapshot(
        &self,
        tenant_id: TenantId,
        caller: Option<PlatformIdentity>,
        mut request: SnapshotRequest,
    ) -> Result<PageResult<QuotaSnapshot>, DomainError> {
        request.tenant_id = tenant_id;
        self.service
            .trusted_operations(caller)?
            .snapshot(request)
            .await
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "trusted_tests.rs"]
mod trusted_tests;
