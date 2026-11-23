//! @cpt-dod:cpt-cf-bss-products-dod-quota-catalog-reads:p1
//! The quota catalog's in-process client (P-D-272): `ProductsQuotaCatalogClientV1` over the same
//! code as the two REST reads, authorized as its caller. Pricing runs in the same process, as it
//! does for `ProductsClient`, so no REST client is published.
use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use bss_products_sdk::ProductsQuotaCatalogClientV1;
use bss_products_sdk::quota::catalog::{
    QuotaDefinition, QuotaDefinitionListRequest, QuotaDefinitionPage, QuotaVersion,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::rest::quota::QuotaState;
use crate::api::rest::quota::definitions::{list_page, read_one};

/// Reads the approved catalog for in-process consumers, as the caller.
pub struct LocalQuotaCatalogClient {
    state: Arc<QuotaState>,
    enforcer: Arc<PolicyEnforcer>,
}

impl LocalQuotaCatalogClient {
    /// Over the same state and enforcer as the REST reads.
    #[must_use]
    pub fn new(state: Arc<QuotaState>, enforcer: Arc<PolicyEnforcer>) -> Self {
        Self { state, enforcer }
    }
}

// @cpt-begin:cpt-cf-bss-products-flow-quota-catalog-browse:p1:inst-quota-sdk
#[async_trait]
impl ProductsQuotaCatalogClientV1 for LocalQuotaCatalogClient {
    async fn list_quota_definitions(
        &self,
        ctx: &SecurityContext,
        request: QuotaDefinitionListRequest,
    ) -> Result<QuotaDefinitionPage, CanonicalError> {
        list_page(&self.state, &self.enforcer, ctx, &request).await
    }

    async fn get_quota_definition(
        &self,
        ctx: &SecurityContext,
        definition_id: Uuid,
        version: QuotaVersion,
        locale: Option<&str>,
    ) -> Result<QuotaDefinition, CanonicalError> {
        read_one(
            &self.state,
            &self.enforcer,
            ctx,
            definition_id,
            version,
            locale,
        )
        .await
    }
}
// @cpt-end:cpt-cf-bss-products-flow-quota-catalog-browse:p1:inst-quota-sdk

#[cfg(test)]
#[path = "catalog_client_tests.rs"]
mod catalog_client_tests;
