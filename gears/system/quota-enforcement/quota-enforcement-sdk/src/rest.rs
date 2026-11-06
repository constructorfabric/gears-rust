//! REST projections of the three client contracts.
//!
//! Feature `rest-client` emits the `*RestClient` types and their
//! directory-resolving wrappers; `rest-server` emits the
//! `register_*_rest_routes` functions the gear hosts. The routes are internal
//! (not exposed at the edge) and authenticated: the generated client forwards
//! the caller's bearer token, and the server rebuilds the `SecurityContext`
//! from it, so the PDP judges the original caller.

use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

use crate::bulk::{
    BulkCreateQuotasRequest, BulkCreated, BulkDeactivateQuotasRequest, BulkDeactivated,
    BulkUpdateQuotasRequest, BulkUpdated,
};
use crate::client::{
    DeletePolicyRequest, ListPolicyVersionsRequest, QuotaEnforcementApi, QuotaManagerApi,
    QuotaOperatorApi, ReadPolicyRequest, ReadQuotasRequest, RollbackPolicyRequest,
    UpdatePolicyRequest, UpdateQuotaRequest,
};
use crate::models::{
    AcquireLeaseOutcome, AcquireLeaseRequest, BatchDebitRequest, BatchDecision, CommitLeaseRequest,
    CreditRequest, DeactivateOutcome, DebitRequest, Decision, DecisionPreview, PageResult,
    PolicySpec, PolicyVersion, PolicyVersionMeta, PreviewRequest, QuotaId, QuotaSnapshot,
    QuotaSpec, QuotaView, ReleaseLeaseRequest, RollbackRequest, SnapshotRequest,
};

/// HTTP projection of [`QuotaManagerApi`].
#[toolkit::rest_contract(base_path = "/quota-enforcement/v1")]
pub trait QuotaManagerApiRest: QuotaManagerApi {
    /// `POST /quota-enforcement/v1/quotas/create`.
    #[post("/quotas/create")]
    async fn create_quota(
        &self,
        ctx: &SecurityContext,
        spec: QuotaSpec,
    ) -> Result<QuotaId, CanonicalError>;

    /// `POST /quota-enforcement/v1/quotas/update`.
    #[post("/quotas/update")]
    async fn update_quota(
        &self,
        ctx: &SecurityContext,
        request: UpdateQuotaRequest,
    ) -> Result<(), CanonicalError>;

    /// `POST /quota-enforcement/v1/quotas/deactivate`.
    #[post("/quotas/deactivate")]
    async fn deactivate_quota(
        &self,
        ctx: &SecurityContext,
        quota_id: QuotaId,
    ) -> Result<DeactivateOutcome, CanonicalError>;

    /// `POST /quota-enforcement/v1/quotas/read`.
    #[post("/quotas/read")]
    #[retryable]
    async fn read_quotas(
        &self,
        ctx: &SecurityContext,
        request: ReadQuotasRequest,
    ) -> Result<PageResult<QuotaView>, CanonicalError>;

    /// `POST /quota-enforcement/v1/quotas/credit`.
    #[post("/quotas/credit")]
    #[retryable]
    async fn credit(
        &self,
        ctx: &SecurityContext,
        request: CreditRequest,
    ) -> Result<Decision, CanonicalError>;

    /// `POST /quota-enforcement/v1/quotas/bulk-create`.
    #[post("/quotas/bulk-create")]
    #[retryable]
    async fn bulk_create_quotas(
        &self,
        ctx: &SecurityContext,
        request: BulkCreateQuotasRequest,
    ) -> Result<BulkCreated, CanonicalError>;

    /// `POST /quota-enforcement/v1/quotas/bulk-update`.
    #[post("/quotas/bulk-update")]
    #[retryable]
    async fn bulk_update_quotas(
        &self,
        ctx: &SecurityContext,
        request: BulkUpdateQuotasRequest,
    ) -> Result<BulkUpdated, CanonicalError>;

    /// `POST /quota-enforcement/v1/quotas/bulk-deactivate`.
    #[post("/quotas/bulk-deactivate")]
    #[retryable]
    async fn bulk_deactivate_quotas(
        &self,
        ctx: &SecurityContext,
        request: BulkDeactivateQuotasRequest,
    ) -> Result<BulkDeactivated, CanonicalError>;
}

/// HTTP projection of [`QuotaEnforcementApi`].
#[toolkit::rest_contract(base_path = "/quota-enforcement/v1")]
pub trait QuotaEnforcementApiRest: QuotaEnforcementApi {
    /// `POST /quota-enforcement/v1/operations/debit`.
    #[post("/operations/debit")]
    #[retryable]
    async fn debit(
        &self,
        ctx: &SecurityContext,
        request: DebitRequest,
    ) -> Result<Decision, CanonicalError>;

    /// `POST /quota-enforcement/v1/operations/rollback`.
    #[post("/operations/rollback")]
    #[retryable]
    async fn rollback(
        &self,
        ctx: &SecurityContext,
        request: RollbackRequest,
    ) -> Result<Decision, CanonicalError>;

    /// `POST /quota-enforcement/v1/operations/preview`.
    #[post("/operations/preview")]
    #[retryable]
    async fn evaluate_preview(
        &self,
        ctx: &SecurityContext,
        request: PreviewRequest,
    ) -> Result<DecisionPreview, CanonicalError>;

    /// `POST /quota-enforcement/v1/leases/acquire`.
    #[post("/leases/acquire")]
    #[retryable]
    async fn acquire_lease(
        &self,
        ctx: &SecurityContext,
        request: AcquireLeaseRequest,
    ) -> Result<AcquireLeaseOutcome, CanonicalError>;

    /// `POST /quota-enforcement/v1/leases/commit`.
    #[post("/leases/commit")]
    #[retryable]
    async fn commit_lease(
        &self,
        ctx: &SecurityContext,
        request: CommitLeaseRequest,
    ) -> Result<Decision, CanonicalError>;

    /// `POST /quota-enforcement/v1/leases/release`.
    #[post("/leases/release")]
    #[retryable]
    async fn release_lease(
        &self,
        ctx: &SecurityContext,
        request: ReleaseLeaseRequest,
    ) -> Result<Decision, CanonicalError>;

    /// `POST /quota-enforcement/v1/operations/batch-debit`.
    #[post("/operations/batch-debit")]
    #[retryable]
    async fn batch_debit(
        &self,
        ctx: &SecurityContext,
        request: BatchDebitRequest,
    ) -> Result<BatchDecision, CanonicalError>;

    /// `POST /quota-enforcement/v1/snapshot`.
    #[post("/snapshot")]
    #[retryable]
    async fn snapshot(
        &self,
        ctx: &SecurityContext,
        request: SnapshotRequest,
    ) -> Result<PageResult<QuotaSnapshot>, CanonicalError>;
}

/// HTTP projection of [`QuotaOperatorApi`].
#[toolkit::rest_contract(base_path = "/quota-enforcement/v1")]
pub trait QuotaOperatorApiRest: QuotaOperatorApi {
    /// `POST /quota-enforcement/v1/policies/create`.
    #[post("/policies/create")]
    async fn create_policy(
        &self,
        ctx: &SecurityContext,
        spec: PolicySpec,
    ) -> Result<PolicyVersion, CanonicalError>;

    /// `POST /quota-enforcement/v1/policies/update`.
    #[post("/policies/update")]
    async fn update_policy(
        &self,
        ctx: &SecurityContext,
        request: UpdatePolicyRequest,
    ) -> Result<PolicyVersion, CanonicalError>;

    /// `POST /quota-enforcement/v1/policies/rollback`.
    #[post("/policies/rollback")]
    async fn rollback_policy(
        &self,
        ctx: &SecurityContext,
        request: RollbackPolicyRequest,
    ) -> Result<PolicyVersion, CanonicalError>;

    /// `POST /quota-enforcement/v1/policies/delete`.
    #[post("/policies/delete")]
    async fn delete_policy(
        &self,
        ctx: &SecurityContext,
        request: DeletePolicyRequest,
    ) -> Result<(), CanonicalError>;

    /// `POST /quota-enforcement/v1/policies/read`.
    #[post("/policies/read")]
    #[retryable]
    async fn read_policy(
        &self,
        ctx: &SecurityContext,
        request: ReadPolicyRequest,
    ) -> Result<PolicyVersion, CanonicalError>;

    /// `POST /quota-enforcement/v1/policies/versions`.
    #[post("/policies/versions")]
    #[retryable]
    async fn list_policy_versions(
        &self,
        ctx: &SecurityContext,
        request: ListPolicyVersionsRequest,
    ) -> Result<PageResult<PolicyVersionMeta>, CanonicalError>;
}
