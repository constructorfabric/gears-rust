//! Client traits of the gear (DESIGN section 3.3, "SDK Rust Traits").
//!
//! Every method is async, takes the caller's [`SecurityContext`] first, and
//! returns [`QuotaEnforcementError`], the platform canonical error the REST
//! surface renders as a `Problem`. The gear registers an in-process
//! implementation in `ClientHub`; a REST client implements the same trait over
//! the public API. Both enter the gear at the same admission step, so the two
//! transports share one authorization boundary.
//!
//! The consumer and operator traits land with their features.

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

use crate::models::{
    DeactivateOutcome, PageRequest, PageResult, QuotaFilter, QuotaId, QuotaPatch, QuotaSpec,
    QuotaView,
};

/// Error of every client method: the platform canonical error.
pub type QuotaEnforcementError = CanonicalError;

/// Quota Manager surface: the Quota lifecycle within the caller's PDP scope
/// (quota-lifecycle feature). Platform operators use the same methods under
/// their own grants; the difference is the PDP decision, not the trait.
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
