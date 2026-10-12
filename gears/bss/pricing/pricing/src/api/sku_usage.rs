//! Products' SKU usage port, as pricing fills it (D-428, P-D-197).
//!
//! Pricing registers [`PricingSkuUsage`] in the `ClientHub` at init as `dyn SkuUsageV1`, and
//! Products resolves it at each SKU read. The port is a read door into pricing like the REST
//! ones: the caller must hold `price_book_entry:read` (else 403), the SKUs' entries are read under
//! the scope that grant gives, in the tenant asked for, and the counts are read in one
//! transaction with a fixed number of statements. `usage_sets` answers the SKU list's `priced`
//! and `in_plan` filters (P-D-212) under the same rule, in two set-based statements. Pricing reads only the SKU ids it is given;
//! nothing Products holds flows back into pricing. `sku_ids_in` answers the pickers' scopes
//! (P-D-246) in one statement each: a book's SKUs under the same rule, and a revision's SKUs,
//! which are plan content, under `plan:read` as well.
use crate::api::rest::authoring::{AuthoringState, support};
use crate::authz::{self, actions, resource_types};
use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use bss_products_sdk::sku_usage::{
    SkuUsage, SkuUsageSets, SkuUsageV1, UsageScope, sku_usage_denied, sku_usage_unavailable,
};
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::secure::AccessScope;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Pricing's implementation of Products' `SkuUsageV1`.
pub struct PricingSkuUsage {
    state: Arc<AuthoringState>,
    enforcer: PolicyEnforcer,
}
impl PricingSkuUsage {
    /// The port over the gear's database and policy.
    #[must_use]
    pub fn new(state: Arc<AuthoringState>, enforcer: PolicyEnforcer) -> Self {
        Self { state, enforcer }
    }
    /// The scope `price_book_entry:read` gives the caller; a denial is the port's 403, a PDP
    /// outage its 503.
    async fn entry_scope(&self, ctx: &SecurityContext) -> Result<AccessScope, CanonicalError> {
        self.read_scope(ctx, &resource_types::PRICE_BOOK_ENTRY)
            .await
    }
    /// The scope `read` on `resource` gives the caller; a denial is the port's 403, a PDP outage
    /// its 503.
    async fn read_scope(
        &self,
        ctx: &SecurityContext,
        resource: &authz_resolver_sdk::pep::ResourceType,
    ) -> Result<AccessScope, CanonicalError> {
        authz::access_scope(&self.enforcer, ctx, resource, actions::READ, None, None)
            .await
            .map_err(|error| match error {
                authz::AuthzError::Denied(denial) => {
                    tracing::debug!(reason = %denial.reason, "bss-pricing: SKU usage refused");
                    sku_usage_denied()
                }
                authz::AuthzError::Unavailable(detail) => {
                    tracing::warn!(detail, "bss-pricing: SKU usage authorization unavailable");
                    sku_usage_unavailable("pricing authorization is unavailable")
                }
            })
    }
}
#[async_trait]
impl SkuUsageV1 for PricingSkuUsage {
    async fn usage(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        sku_ids: &[Uuid],
    ) -> Result<Vec<SkuUsage>, CanonicalError> {
        let scope = self.entry_scope(ctx).await?;
        let skus = sku_ids.to_vec();
        support::transaction(&self.state.db.db(), move |tx| {
            let (scope, skus) = (scope.clone(), skus.clone());
            Box::pin(
                async move { Ok(crate::infra::usage::sku_usage(tx, &scope, tenant, &skus).await?) },
            )
        })
        .await
        .map_err(|error| {
            tracing::warn!(error = %error, diagnostic = error.diagnostic().unwrap_or_default(), "bss-pricing: SKU usage could not be read");
            sku_usage_unavailable("pricing could not read the SKU usage")
        })
    }
    /// P-D-212: the same rule and scope as `usage`, two set-based reads in one transaction.
    async fn usage_sets(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
    ) -> Result<SkuUsageSets, CanonicalError> {
        let scope = self.entry_scope(ctx).await?;
        support::transaction(&self.state.db.db(), move |tx| {
            let scope = scope.clone();
            Box::pin(
                async move { Ok(crate::infra::usage::sku_usage_sets(tx, &scope, tenant).await?) },
            )
        })
        .await
        .map_err(|error| {
            tracing::warn!(error = %error, diagnostic = error.diagnostic().unwrap_or_default(), "bss-pricing: SKU usage sets could not be read");
            sku_usage_unavailable("pricing could not read the SKU usage sets")
        })
    }
    /// P-D-246: a book's SKUs under `price_book_entry:read`, read under the scope it gives; a
    /// revision's SKUs under `price_book_entry:read` and `plan:read`, the revision read under the
    /// plan scope. One statement either way; a book or a revision the tenant does not hold is the
    /// empty set.
    async fn sku_ids_in(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        scope: UsageScope,
    ) -> Result<Vec<Uuid>, CanonicalError> {
        use crate::infra::storage::repo::{plan_item_repo, price_book_entry_repo};
        let entries = self.entry_scope(ctx).await?;
        // The scope each read runs under: the entries' for a book, the plans' for a revision.
        let under = match scope {
            UsageScope::Book(_) => entries,
            // A revision's SKUs are plan content (P-D-246).
            UsageScope::Revision(_) => self.read_scope(ctx, &resource_types::PLAN).await?,
        };
        support::transaction(&self.state.db.db(), move |tx| {
            let under = under.clone();
            Box::pin(async move {
                Ok(match scope {
                    UsageScope::Book(book) => {
                        price_book_entry_repo::skus_in_book(tx, &under, tenant, book).await?
                    }
                    UsageScope::Revision(revision) => {
                        plan_item_repo::skus_of_revision(tx, &under, tenant, revision).await?
                    }
                })
            })
        })
        .await
        .map_err(|error| {
            tracing::warn!(error = %error, diagnostic = error.diagnostic().unwrap_or_default(), ?scope, "bss-pricing: a SKU usage scope could not be read");
            sku_usage_unavailable("pricing could not read the SKUs of the scope")
        })
    }
}
