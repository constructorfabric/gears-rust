//! The `ClientHub` read of a stored derived usage declaration (P-D-277).
//!
//! `gear.rs` registers [`LocalDerivedUsageTypeRead`] at init as the hub's one `dyn DerivedUsageTypeReadV1`, over the
//! same runtime as the REST endpoints, as it registers `UsageMeterSemanticsV1` (P-D-233). It answers the version read
//! `GET /derived-usage-types/{code}/versions/{n}` in a catalog tenant the caller names, under `sku:read` in that
//! tenant ([`crate::api::rest::derived_usage_types::declaration_in`]): the same refusals and the same stored digest.
use crate::api::rest::{ApiState, derived_usage_types};
use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use bss_products_sdk::derived::MeterId;
use bss_products_sdk::derived_read::{DerivedUsageTypeReadV1, StoredDerivedDeclaration};
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// The hub's `dyn DerivedUsageTypeReadV1`: the version read of this gear's store, in a named catalog tenant.
pub struct LocalDerivedUsageTypeRead {
    state: Arc<ApiState>,
    enforcer: Arc<PolicyEnforcer>,
}

impl LocalDerivedUsageTypeRead {
    /// Over the gear's runtime: the same database and PDP the REST endpoints use.
    #[must_use]
    pub fn new(state: Arc<ApiState>, enforcer: Arc<PolicyEnforcer>) -> Self {
        Self { state, enforcer }
    }
}

#[async_trait]
impl DerivedUsageTypeReadV1 for LocalDerivedUsageTypeRead {
    async fn declaration(
        &self,
        ctx: &SecurityContext,
        catalog_tenant_id: Uuid,
        meter: &MeterId,
    ) -> Result<StoredDerivedDeclaration, CanonicalError> {
        derived_usage_types::declaration_in(
            &self.state,
            &self.enforcer,
            ctx,
            catalog_tenant_id,
            meter,
        )
        .await
    }
}

#[cfg(test)]
#[path = "derived_read_tests.rs"]
mod derived_read_tests;
