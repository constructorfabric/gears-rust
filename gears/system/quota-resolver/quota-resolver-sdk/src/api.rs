//! Public contracts of the quota resolver, one per plane.

use toolkit::Page;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};

use crate::models::{
    LeaseOutcome, PlatformAcquireLeaseRequest, PlatformCommitLeaseRequest, PlatformDebitRequest,
    PlatformQuotaStateRequest, PlatformReleaseLeaseRequest, PlatformReturnRequest, QuotaDecision,
    QuotaState, QuotaStateRequest,
};

/// Platform plane: every operation, for trusted platform services that name the
/// tenant and the subjects. A denial is a successful call; only a failure to
/// decide is an error.
#[toolkit::contract(gear = "quota-resolver", version = "v1")]
pub trait PlatformQuotaResolverApi: Send + Sync {
    /// Charge the metric items against the subjects.
    #[idempotency(IdempotentWrite)]
    async fn debit(
        &self,
        ctx: PlatformSecurityContext,
        request: PlatformDebitRequest,
    ) -> Result<QuotaDecision, CanonicalError>;

    /// Hold capacity for a bounded time.
    #[idempotency(IdempotentWrite)]
    async fn acquire_lease(
        &self,
        ctx: PlatformSecurityContext,
        request: PlatformAcquireLeaseRequest,
    ) -> Result<LeaseOutcome, CanonicalError>;

    /// Settle a lease for the amounts actually used.
    #[idempotency(IdempotentWrite)]
    async fn commit_lease(
        &self,
        ctx: PlatformSecurityContext,
        request: PlatformCommitLeaseRequest,
    ) -> Result<(), CanonicalError>;

    /// Release a lease without charging.
    #[idempotency(IdempotentWrite)]
    async fn release_lease(
        &self,
        ctx: PlatformSecurityContext,
        request: PlatformReleaseLeaseRequest,
    ) -> Result<(), CanonicalError>;

    /// Give back amounts of an earlier charge.
    #[idempotency(IdempotentWrite)]
    async fn return_amount(
        &self,
        ctx: PlatformSecurityContext,
        request: PlatformReturnRequest,
    ) -> Result<(), CanonicalError>;

    /// Read the state of the quotas that apply to one subject.
    #[idempotency(SafeRead)]
    async fn quota_state(
        &self,
        ctx: PlatformSecurityContext,
        request: PlatformQuotaStateRequest,
    ) -> Result<Page<QuotaState>, CanonicalError>;
}

/// Tenant plane: read-only. The subject is the caller's principal and the tenant
/// is the caller's tenant, both taken from `ctx`.
#[toolkit::contract(gear = "quota-resolver", version = "v1")]
pub trait QuotaResolverApi: Send + Sync {
    /// Read the state of the caller's quotas.
    #[idempotency(SafeRead)]
    async fn quota_state(
        &self,
        ctx: SecurityContext,
        request: QuotaStateRequest,
    ) -> Result<Page<QuotaState>, CanonicalError>;
}
