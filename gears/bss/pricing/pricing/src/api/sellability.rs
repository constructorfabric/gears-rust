//! Authorized acceptance command and live fulfilment checks.
use crate::infra::commercial_terms::CommercialTermsService;
use bss_pricing_sdk::acceptance::{
    AcceptanceReceipt, CommandMeta, FulfilmentEligibility, FulfilmentQuery, NewSaleQuery,
    SellabilityV1,
};
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
/// Acceptance command and live fulfilment capability.
pub struct SellabilityProvider {
    service: Arc<CommercialTermsService>,
}
impl SellabilityProvider {
    /// Share the commercial service with receipt reads/holds.
    #[must_use]
    pub fn new(service: Arc<CommercialTermsService>) -> Self {
        Self { service }
    }
}
#[async_trait::async_trait]
impl SellabilityV1 for SellabilityProvider {
    async fn check(
        &self,
        ctx: &SecurityContext,
        query: NewSaleQuery,
        meta: CommandMeta,
    ) -> Result<AcceptanceReceipt, CanonicalError> {
        self.service.check(ctx, query, meta).await
    }
    async fn check_fulfilment(
        &self,
        ctx: &SecurityContext,
        query: FulfilmentQuery,
    ) -> Result<FulfilmentEligibility, CanonicalError> {
        self.service.check_fulfilment(ctx, query).await
    }
}
