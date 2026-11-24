//! Local (in-process) client for the quota resolver gear.

use std::sync::Arc;

use async_trait::async_trait;
use quota_resolver_sdk::models::{
    LeaseOutcome, PlatformAcquireLeaseRequest, PlatformCommitLeaseRequest, PlatformDebitRequest,
    PlatformQuotaStateRequest, PlatformReleaseLeaseRequest, PlatformReturnRequest, QuotaDecision,
    QuotaState, QuotaStateRequest,
};
use quota_resolver_sdk::{PlatformQuotaResolverApi, QuotaResolverApi};
use toolkit::Page;
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_security::{PlatformIdentity, PlatformSecurityContext, SecurityContext};

use super::Service;

/// Local client wrapping the quota resolver service; implements both contracts.
#[domain_model]
pub struct QuotaResolverLocalClient {
    svc: Arc<Service>,
}

impl QuotaResolverLocalClient {
    #[must_use]
    pub fn new(svc: Arc<Service>) -> Self {
        Self { svc }
    }
}

/// The caller's identity for the plugin record; none for the in-process marker.
fn caller_of(ctx: PlatformSecurityContext) -> Option<PlatformIdentity> {
    if ctx.is_outbound_marker() {
        None
    } else {
        Some(ctx.into_identity())
    }
}

#[async_trait]
impl PlatformQuotaResolverApi for QuotaResolverLocalClient {
    async fn debit(
        &self,
        ctx: PlatformSecurityContext,
        request: PlatformDebitRequest,
    ) -> Result<QuotaDecision, CanonicalError> {
        self.svc.debit(request, caller_of(ctx)).await
    }

    async fn acquire_lease(
        &self,
        ctx: PlatformSecurityContext,
        request: PlatformAcquireLeaseRequest,
    ) -> Result<LeaseOutcome, CanonicalError> {
        self.svc.acquire_lease(request, caller_of(ctx)).await
    }

    async fn commit_lease(
        &self,
        ctx: PlatformSecurityContext,
        request: PlatformCommitLeaseRequest,
    ) -> Result<(), CanonicalError> {
        self.svc.commit_lease(request, caller_of(ctx)).await
    }

    async fn release_lease(
        &self,
        ctx: PlatformSecurityContext,
        request: PlatformReleaseLeaseRequest,
    ) -> Result<(), CanonicalError> {
        self.svc.release_lease(request, caller_of(ctx)).await
    }

    async fn return_amount(
        &self,
        ctx: PlatformSecurityContext,
        request: PlatformReturnRequest,
    ) -> Result<(), CanonicalError> {
        self.svc.return_amount(request, caller_of(ctx)).await
    }

    async fn quota_state(
        &self,
        ctx: PlatformSecurityContext,
        request: PlatformQuotaStateRequest,
    ) -> Result<Page<QuotaState>, CanonicalError> {
        self.svc.quota_state(request, caller_of(ctx)).await
    }
}

#[async_trait]
impl QuotaResolverApi for QuotaResolverLocalClient {
    async fn quota_state(
        &self,
        ctx: SecurityContext,
        request: QuotaStateRequest,
    ) -> Result<Page<QuotaState>, CanonicalError> {
        self.svc.tenant_quota_state(&ctx, request).await
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "local_client_tests.rs"]
mod local_client_tests;
