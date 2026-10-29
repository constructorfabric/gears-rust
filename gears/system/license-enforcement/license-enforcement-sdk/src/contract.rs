//! The management contract of license-enforcement (DESIGN §3.3, Management).
//!
//! In process the gear registers its implementation in `ClientHub`; over REST
//! the [`rest`](crate::rest) projection carries the same methods. The PDP
//! authorizes every method with the tenant of `ctx`.

use toolkit::Page;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

use crate::dto::{
    BindRequest, CheckExplainRequest, CheckExplanation, DelegateRequest, Delegation,
    DelegationVerbRequest, ImportEvidenceRequest, IssueGrantRequest, IssuePackRequest, IssueResult,
    ListQuery, RenewRequest, ReplaceRequest, VerbRequest,
};
use crate::ids::{DelegationId, GrantId, PackId};
use crate::models::{Grant, Pack};

/// Management of grants, packs, and delegations.
///
/// Errors are `CanonicalError`; [`crate::error`] lists the discriminators.
#[toolkit::contract(gear = "license-enforcement", version = "v1")]
pub trait LicenseEnforcementApi: Send + Sync {
    /// Issue a pack through the `issue` intake.
    #[idempotency(IdempotentWrite)]
    async fn issue_pack(
        &self,
        ctx: SecurityContext,
        req: IssuePackRequest,
    ) -> Result<IssueResult, CanonicalError>;

    /// Issue one root grant through the `issue` intake.
    #[idempotency(IdempotentWrite)]
    async fn issue_grant(
        &self,
        ctx: SecurityContext,
        req: IssueGrantRequest,
    ) -> Result<IssueResult, CanonicalError>;

    /// Submit evidence through the `import` intake.
    #[idempotency(IdempotentWrite)]
    async fn import_evidence(
        &self,
        ctx: SecurityContext,
        req: ImportEvidenceRequest,
    ) -> Result<IssueResult, CanonicalError>;

    /// Read a pack.
    #[idempotency(SafeRead)]
    async fn get_pack(&self, ctx: SecurityContext, id: PackId) -> Result<Pack, CanonicalError>;

    /// List packs of the caller's tenant.
    #[idempotency(SafeRead)]
    async fn list_packs(
        &self,
        ctx: SecurityContext,
        query: ListQuery,
    ) -> Result<Page<Pack>, CanonicalError>;

    /// Read a grant.
    #[idempotency(SafeRead)]
    async fn get_grant(&self, ctx: SecurityContext, id: GrantId) -> Result<Grant, CanonicalError>;

    /// List grants in the scope that the PDP gives the caller. `$filter` only
    /// narrows the list.
    #[idempotency(SafeRead)]
    async fn list_grants(
        &self,
        ctx: SecurityContext,
        query: ListQuery,
    ) -> Result<Page<Grant>, CanonicalError>;

    /// Read a delegation; the grantor and the recipient can read it.
    #[idempotency(SafeRead)]
    async fn get_delegation(
        &self,
        ctx: SecurityContext,
        id: DelegationId,
    ) -> Result<Delegation, CanonicalError>;

    /// Holder accepts a grant that awaits acceptance.
    #[idempotency(IdempotentWrite)]
    async fn accept_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError>;

    /// Holder rejects a grant that awaits acceptance.
    #[idempotency(IdempotentWrite)]
    async fn reject_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError>;

    /// Vendor suspends a root.
    #[idempotency(IdempotentWrite)]
    async fn suspend_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError>;

    /// Vendor reinstates a suspended root.
    #[idempotency(IdempotentWrite)]
    async fn reinstate_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError>;

    /// Vendor revokes a root and every descendant.
    #[idempotency(IdempotentWrite)]
    async fn revoke_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError>;

    /// Extend the validity in place.
    #[idempotency(IdempotentWrite)]
    async fn renew_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: RenewRequest,
    ) -> Result<Grant, CanonicalError>;

    /// Holder binds a subject.
    #[idempotency(IdempotentWrite)]
    async fn bind_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: BindRequest,
    ) -> Result<Grant, CanonicalError>;

    /// Holder removes the subject binding.
    #[idempotency(IdempotentWrite)]
    async fn unbind_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError>;

    /// Holder returns a sub-grant.
    #[idempotency(IdempotentWrite)]
    async fn return_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError>;

    /// Grantor recalls a sub-grant and every descendant.
    #[idempotency(IdempotentWrite)]
    async fn recall_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError>;

    /// Vendor replaces the grant through the `issue` intake.
    #[idempotency(IdempotentWrite)]
    async fn replace_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: ReplaceRequest,
    ) -> Result<IssueResult, CanonicalError>;

    /// Suspend every member of a pack in one transaction.
    #[idempotency(IdempotentWrite)]
    async fn suspend_pack(
        &self,
        ctx: SecurityContext,
        id: PackId,
        req: VerbRequest,
    ) -> Result<Pack, CanonicalError>;

    /// Reinstate every member of a pack in one transaction.
    #[idempotency(IdempotentWrite)]
    async fn reinstate_pack(
        &self,
        ctx: SecurityContext,
        id: PackId,
        req: VerbRequest,
    ) -> Result<Pack, CanonicalError>;

    /// Revoke every member of a pack in one transaction.
    #[idempotency(IdempotentWrite)]
    async fn revoke_pack(
        &self,
        ctx: SecurityContext,
        id: PackId,
        req: VerbRequest,
    ) -> Result<Pack, CanonicalError>;

    /// Extend the validity of every member of a pack in one transaction.
    #[idempotency(IdempotentWrite)]
    async fn renew_pack(
        &self,
        ctx: SecurityContext,
        id: PackId,
        req: RenewRequest,
    ) -> Result<Pack, CanonicalError>;

    /// Replace the pack through the `issue` intake.
    #[idempotency(IdempotentWrite)]
    async fn replace_pack(
        &self,
        ctx: SecurityContext,
        id: PackId,
        req: ReplaceRequest,
    ) -> Result<IssueResult, CanonicalError>;

    /// Delegate unbound grants to a descendant tenant in one transaction.
    #[idempotency(IdempotentWrite)]
    async fn delegate(
        &self,
        ctx: SecurityContext,
        req: DelegateRequest,
    ) -> Result<Delegation, CanonicalError>;

    /// Recipient accepts a delegation.
    #[idempotency(IdempotentWrite)]
    async fn accept_delegation(
        &self,
        ctx: SecurityContext,
        id: DelegationId,
        req: DelegationVerbRequest,
    ) -> Result<Delegation, CanonicalError>;

    /// Recipient rejects a delegation.
    #[idempotency(IdempotentWrite)]
    async fn reject_delegation(
        &self,
        ctx: SecurityContext,
        id: DelegationId,
        req: DelegationVerbRequest,
    ) -> Result<Delegation, CanonicalError>;

    /// Recipient returns a delegation.
    #[idempotency(IdempotentWrite)]
    async fn return_delegation(
        &self,
        ctx: SecurityContext,
        id: DelegationId,
        req: DelegationVerbRequest,
    ) -> Result<Delegation, CanonicalError>;

    /// Grantor recalls a delegation and every descendant.
    #[idempotency(IdempotentWrite)]
    async fn recall_delegation(
        &self,
        ctx: SecurityContext,
        id: DelegationId,
        req: DelegationVerbRequest,
    ) -> Result<Delegation, CanonicalError>;

    /// The decision, reason, and matched grant that a check would give.
    #[idempotency(SafeRead)]
    async fn check_explain(
        &self,
        ctx: SecurityContext,
        req: CheckExplainRequest,
    ) -> Result<CheckExplanation, CanonicalError>;
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "contract_tests.rs"]
mod contract_tests;
