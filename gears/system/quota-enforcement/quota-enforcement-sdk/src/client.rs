//! Client traits for quota enforcement.
//!
//! Every method is async, takes the caller's [`SecurityContext`] first, and
//! returns the same canonical error exposed by REST. In-process and REST
//! clients share the same authorization boundary.

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

use crate::models::{
    CreditRequest, DeactivateOutcome, DebitRequest, Decision, DecisionPreview, PageRequest,
    PageResult, PreviewRequest, QuotaFilter, QuotaId, QuotaPatch, QuotaSpec, QuotaView,
    RollbackRequest,
};

/// Error of every client method: the platform canonical error.
pub type QuotaEnforcementError = CanonicalError;

/// Manages Quotas within the caller's PDP scope.
#[async_trait]
pub trait QuotaManagerClientV1: Send + Sync + 'static {
    /// Create a Quota. The gear validates `spec`, resolves the metric owner's
    /// constraint contract, and returns the server-assigned identifier.
    ///
    /// # Errors
    ///
    /// The canonical error of the failed validation, authorization, or
    /// storage step; `Unimplemented` for the reserved `rate` type.
    async fn create_quota(
        &self,
        ctx: &SecurityContext,
        spec: QuotaSpec,
    ) -> Result<QuotaId, QuotaEnforcementError>;

    /// Apply a non-breaking patch. Metric, type, period, and subject are
    /// immutable; a breaking change is a deactivate followed by a create.
    ///
    /// # Errors
    ///
    /// `NotFound` for an unknown or out-of-scope Quota, `FailedPrecondition`
    /// for a deactivated Quota or a cap below the consumed amount, and the
    /// canonical error of any other failed step.
    async fn update_quota(
        &self,
        ctx: &SecurityContext,
        quota_id: QuotaId,
        patch: QuotaPatch,
    ) -> Result<(), QuotaEnforcementError>;

    /// Deactivate a Quota. The record stays readable; its active leases are
    /// resolved atomically and listed in the outcome.
    ///
    /// # Errors
    ///
    /// `NotFound` for an unknown or out-of-scope Quota, `FailedPrecondition`
    /// when it is already deactivated.
    async fn deactivate_quota(
        &self,
        ctx: &SecurityContext,
        quota_id: QuotaId,
    ) -> Result<DeactivateOutcome, QuotaEnforcementError>;

    /// Read Quotas within the caller's scope, one page at a time. Rows outside
    /// the scope are absent, not errors.
    ///
    /// # Errors
    ///
    /// The canonical error of a failed authorization or storage step.
    async fn read_quotas(
        &self,
        ctx: &SecurityContext,
        filter: QuotaFilter,
        page: PageRequest,
    ) -> Result<PageResult<QuotaView>, QuotaEnforcementError>;
}

/// Performs guarded consumption operations.
///
/// A denial is a successful call: [`Decision`] carries either verdict. Only a
/// failure to decide is an error.
///
/// Write keys are scoped by tenant, subjects, and operation. A replay returns
/// the stored decision without re-evaluation.
#[async_trait]
pub trait QuotaEnforcementClientV1: Send + Sync + 'static {
    /// Charge `amount` against every Quota applicable to the attribution, and
    /// commit the resulting plan atomically.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for a non-positive amount, a missing idempotency key,
    /// or a metric that is not quota-gated; `PermissionDenied` when the PDP
    /// refuses the attribution; `Aborted` (`IDEMPOTENCY_PAYLOAD_MISMATCH`)
    /// when the key was used for a different payload; the canonical error of
    /// any failed evaluation or storage step.
    async fn debit(
        &self,
        ctx: &SecurityContext,
        request: DebitRequest,
    ) -> Result<Decision, QuotaEnforcementError>;

    /// Return `amount` to one named Quota. Corrective, never evaluated against
    /// a policy, and floored at zero.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for a non-positive amount or a missing key;
    /// `NotFound` for an unknown Quota, `PermissionDenied` for one outside the
    /// caller's scope; `FailedPrecondition` for a deactivated Quota or a
    /// closed period; `Aborted` on a payload mismatch.
    async fn credit(
        &self,
        ctx: &SecurityContext,
        request: CreditRequest,
    ) -> Result<Decision, QuotaEnforcementError>;

    /// Reverse a committed debit, restoring the counters to what they would
    /// have been had it never happened.
    ///
    /// The request carries the reversed debit's own attribution, which the
    /// server re-authorizes: a caller can only reverse what it could have
    /// debited. Reversal happens at most once per original operation.
    ///
    /// # Errors
    ///
    /// `NotFound` (`UNKNOWN_OPERATION`) when no committed debit answers the
    /// original key under the recomputed scope and authorized attribution;
    /// `FailedPrecondition` (`PERIOD_CLOSED`) once the attribution period has
    /// settled; `Aborted` on a payload mismatch.
    async fn rollback(
        &self,
        ctx: &SecurityContext,
        request: RollbackRequest,
    ) -> Result<Decision, QuotaEnforcementError>;

    /// Evaluate without mutating anything and without occupying a key.
    ///
    /// # Errors
    ///
    /// The errors of [`Self::debit`], minus the idempotency ones: a preview
    /// carries no key.
    async fn evaluate_preview(
        &self,
        ctx: &SecurityContext,
        request: PreviewRequest,
    ) -> Result<DecisionPreview, QuotaEnforcementError>;
}

/// Platform operator policy surface. Every method requires explicit PDP admission.
/// Identity and trusted schema snapshots are supplied by the service, not callers.
#[async_trait]
pub trait QuotaOperatorClientV1: Send + Sync + 'static {
    /// Create and activate version one at an unoccupied scope.
    ///
    /// # Errors
    /// Authorization/configuration errors, or `AlreadyExists` for an occupied scope.
    async fn create_policy(
        &self,
        ctx: &SecurityContext,
        spec: crate::PolicySpec,
    ) -> Result<crate::PolicyVersion, QuotaEnforcementError>;

    /// Create a new immutable version conditional on the active version.
    ///
    /// # Errors
    /// Authorization/validation errors, unknown/deleted policy, or version conflict.
    async fn update_policy(
        &self,
        ctx: &SecurityContext,
        id: crate::PolicyId,
        patch: crate::PolicyPatch,
    ) -> Result<crate::PolicyVersion, QuotaEnforcementError>;

    /// Activate a permissible historical version; retrying the active target is a no-op.
    ///
    /// # Errors
    /// Authorization errors, unknown/terminal version, or incompatible saved schemas.
    async fn rollback_policy(
        &self,
        ctx: &SecurityContext,
        id: crate::PolicyId,
        target_version: u32,
        comment: Option<String>,
    ) -> Result<crate::PolicyVersion, QuotaEnforcementError>;

    /// Soft-delete a metric policy, preserving history. A repeated delete is a no-op.
    ///
    /// # Errors
    /// Authorization errors, unknown policy, or attempted deletion of the global policy.
    async fn delete_policy(
        &self,
        ctx: &SecurityContext,
        id: crate::PolicyId,
        comment: Option<String>,
    ) -> Result<(), QuotaEnforcementError>;

    /// Read the active version, or a specific retained historical version.
    ///
    /// # Errors
    /// Authorization errors or an unknown policy/version; a deleted policy has no active version.
    async fn read_policy(
        &self,
        ctx: &SecurityContext,
        id: crate::PolicyId,
        version: Option<u32>,
    ) -> Result<crate::PolicyVersion, QuotaEnforcementError>;

    /// Read one page of immutable policy history.
    ///
    /// # Errors
    /// Authorization errors, an unknown policy or invalid pagination input.
    async fn list_policy_versions(
        &self,
        ctx: &SecurityContext,
        id: crate::PolicyId,
        page: PageRequest,
    ) -> Result<PageResult<crate::PolicyVersionMeta>, QuotaEnforcementError>;
}
