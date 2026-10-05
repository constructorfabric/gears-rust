//! Receipt capability, separate from catalog preview and money reads.
use crate::infra::commercial_terms::CommercialTermsService;
use bss_pricing_sdk::acceptance::{
    AcceptanceQuery, AcceptanceReceipt, CommandMeta, FulfilmentQuery, HeldBindings,
    PricingAcceptanceV1,
};
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
/// Authorized receipt reads and durable frozen holds.
pub struct PricingAcceptanceProvider {
    service: Arc<CommercialTermsService>,
}
impl PricingAcceptanceProvider {
    /// Share the commercial service with sellability.
    #[must_use]
    pub fn new(service: Arc<CommercialTermsService>) -> Self {
        Self { service }
    }
}
#[async_trait::async_trait]
impl PricingAcceptanceV1 for PricingAcceptanceProvider {
    async fn acceptance(
        &self,
        ctx: &SecurityContext,
        query: AcceptanceQuery,
    ) -> Result<AcceptanceReceipt, CanonicalError> {
        self.service.acceptance(ctx, query).await
    }
    async fn hold(
        &self,
        ctx: &SecurityContext,
        query: FulfilmentQuery,
        meta: CommandMeta,
    ) -> Result<HeldBindings, CanonicalError> {
        self.service.hold(ctx, query, meta).await
    }
}
