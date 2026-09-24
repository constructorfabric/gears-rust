//! The in-process `QuotaManagerClientV1`: the SDK client trait over the domain
//! service, registered in `ClientHub` at `init`.
//!
//! It enters the domain exactly where REST does (`Service::quotas`), so both
//! transports share one admission boundary. Before bootstrap binds the
//! dependencies every call fails `NotReady`, the same 503 REST returns.

use std::sync::Arc;

use async_trait::async_trait;
use quota_enforcement_sdk::{
    DeactivateOutcome, PageRequest, PageResult, QuotaEnforcementError, QuotaFilter, QuotaId,
    QuotaManagerClientV1, QuotaPatch, QuotaSpec, QuotaView,
};
use toolkit_security::SecurityContext;

use crate::domain::Service;
use crate::domain::quotas::{CreateQuotaRequest, ListQuotasRequest, UpdateQuotaRequest};

/// The gear's own implementation of the manager client.
pub struct InProcessQuotaManager {
    service: Arc<Service>,
}

impl InProcessQuotaManager {
    /// Wrap the domain service.
    #[must_use]
    pub fn new(service: Arc<Service>) -> Self {
        Self { service }
    }
}

// @cpt-dod:cpt-cf-quota-enforcement-dod-quota-crud:p1
#[async_trait]
impl QuotaManagerClientV1 for InProcessQuotaManager {
    async fn create_quota(
        &self,
        ctx: &SecurityContext,
        spec: QuotaSpec,
    ) -> Result<QuotaId, QuotaEnforcementError> {
        let quotas = self.service.quotas()?;
        let request = CreateQuotaRequest::try_from(spec)?;
        Ok(quotas.create(ctx, request).await?.quota.id)
    }

    async fn update_quota(
        &self,
        ctx: &SecurityContext,
        quota_id: QuotaId,
        patch: QuotaPatch,
    ) -> Result<(), QuotaEnforcementError> {
        let quotas = self.service.quotas()?;
        let request = UpdateQuotaRequest::try_from(patch)?;
        quotas.update(ctx, quota_id, request).await?;
        Ok(())
    }

    async fn deactivate_quota(
        &self,
        ctx: &SecurityContext,
        quota_id: QuotaId,
    ) -> Result<DeactivateOutcome, QuotaEnforcementError> {
        let quotas = self.service.quotas()?;
        Ok(quotas.deactivate(ctx, quota_id).await?)
    }

    async fn read_quotas(
        &self,
        ctx: &SecurityContext,
        filter: QuotaFilter,
        page: PageRequest,
    ) -> Result<PageResult<QuotaView>, QuotaEnforcementError> {
        let quotas = self.service.quotas()?;
        Ok(quotas
            .list(ctx, ListQuotasRequest::from((filter, page)))
            .await?)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "in_process_tests.rs"]
mod in_process_tests;

/// Platform operator client; business Quota management remains a separate trait.
pub struct InProcessQuotaOperator {
    service: Arc<Service>,
}
impl InProcessQuotaOperator {
    /// Share the same service as REST.
    #[must_use]
    pub fn new(service: Arc<Service>) -> Self {
        Self { service }
    }
}
#[async_trait]
impl quota_enforcement_sdk::QuotaOperatorClientV1 for InProcessQuotaOperator {
    async fn create_policy(
        &self,
        ctx: &SecurityContext,
        spec: quota_enforcement_sdk::PolicySpec,
    ) -> Result<quota_enforcement_sdk::PolicyVersion, QuotaEnforcementError> {
        Ok(self.service.policies()?.create(ctx, spec).await?)
    }
    async fn update_policy(
        &self,
        ctx: &SecurityContext,
        id: quota_enforcement_sdk::PolicyId,
        patch: quota_enforcement_sdk::PolicyPatch,
    ) -> Result<quota_enforcement_sdk::PolicyVersion, QuotaEnforcementError> {
        Ok(self.service.policies()?.update(ctx, id, patch).await?)
    }
    async fn rollback_policy(
        &self,
        ctx: &SecurityContext,
        id: quota_enforcement_sdk::PolicyId,
        target_version: u32,
        comment: Option<String>,
    ) -> Result<quota_enforcement_sdk::PolicyVersion, QuotaEnforcementError> {
        Ok(self
            .service
            .policies()?
            .rollback(ctx, id, target_version, comment)
            .await?)
    }
    async fn delete_policy(
        &self,
        ctx: &SecurityContext,
        id: quota_enforcement_sdk::PolicyId,
        comment: Option<String>,
    ) -> Result<(), QuotaEnforcementError> {
        Ok(self.service.policies()?.delete(ctx, id, comment).await?)
    }
    async fn read_policy(
        &self,
        ctx: &SecurityContext,
        id: quota_enforcement_sdk::PolicyId,
        version: Option<u32>,
    ) -> Result<quota_enforcement_sdk::PolicyVersion, QuotaEnforcementError> {
        Ok(self.service.policies()?.read(ctx, &id, version).await?)
    }
    async fn list_policy_versions(
        &self,
        ctx: &SecurityContext,
        id: quota_enforcement_sdk::PolicyId,
        page: PageRequest,
    ) -> Result<PageResult<quota_enforcement_sdk::PolicyVersionMeta>, QuotaEnforcementError> {
        Ok(self.service.policies()?.list(ctx, &id, page).await?)
    }
}

/// The gear's own implementation of the consumer client.
///
/// It enters the domain at the same admission step the REST surface does, so
/// an in-process caller and an HTTP caller share one authorization boundary.
pub struct InProcessQuotaEnforcement {
    service: Arc<Service>,
}

impl InProcessQuotaEnforcement {
    /// Wrap the domain service.
    #[must_use]
    pub fn new(service: Arc<Service>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl quota_enforcement_sdk::QuotaEnforcementClientV1 for InProcessQuotaEnforcement {
    async fn debit(
        &self,
        ctx: &SecurityContext,
        request: quota_enforcement_sdk::DebitRequest,
    ) -> Result<quota_enforcement_sdk::Decision, QuotaEnforcementError> {
        Ok(self.service.operations()?.debit(ctx, request).await?)
    }

    async fn credit(
        &self,
        ctx: &SecurityContext,
        request: quota_enforcement_sdk::CreditRequest,
    ) -> Result<quota_enforcement_sdk::Decision, QuotaEnforcementError> {
        Ok(self.service.operations()?.credit(ctx, request).await?)
    }

    async fn rollback(
        &self,
        ctx: &SecurityContext,
        request: quota_enforcement_sdk::RollbackRequest,
    ) -> Result<quota_enforcement_sdk::Decision, QuotaEnforcementError> {
        Ok(self.service.operations()?.rollback(ctx, request).await?)
    }

    async fn evaluate_preview(
        &self,
        ctx: &SecurityContext,
        request: quota_enforcement_sdk::PreviewRequest,
    ) -> Result<quota_enforcement_sdk::DecisionPreview, QuotaEnforcementError> {
        Ok(self.service.operations()?.preview(ctx, request).await?)
    }

    async fn acquire_lease(
        &self,
        ctx: &SecurityContext,
        request: quota_enforcement_sdk::AcquireLeaseRequest,
    ) -> Result<quota_enforcement_sdk::AcquireLeaseOutcome, QuotaEnforcementError> {
        Ok(self
            .service
            .operations()?
            .acquire_lease(ctx, request)
            .await?)
    }

    async fn commit_lease(
        &self,
        ctx: &SecurityContext,
        request: quota_enforcement_sdk::CommitLeaseRequest,
    ) -> Result<quota_enforcement_sdk::Decision, QuotaEnforcementError> {
        Ok(self
            .service
            .operations()?
            .commit_lease(ctx, request)
            .await?)
    }

    async fn release_lease(
        &self,
        ctx: &SecurityContext,
        request: quota_enforcement_sdk::ReleaseLeaseRequest,
    ) -> Result<quota_enforcement_sdk::Decision, QuotaEnforcementError> {
        Ok(self
            .service
            .operations()?
            .release_lease(ctx, request)
            .await?)
    }
}
