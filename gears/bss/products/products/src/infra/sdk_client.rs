//! Authorized SKU head reads for in-process consumers.
use crate::{
    api::rest::{authz_error_to_canonical, repo_error_to_canonical},
    authz::{access_scope, actions, resource_types},
    infra::storage::repo,
};
use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use bss_products_sdk::{ProductsClient, models::Sku};
use std::sync::Arc;
use toolkit_canonical_errors::{CanonicalError, resource_error};
use toolkit_db::Db;
use toolkit_security::SecurityContext;
use uuid::Uuid;

#[resource_error(gts_id!("cf.bss.products.sku.v1~"))]
struct SkuResource;

/// Reads any SKU lifecycle, with PDP constraints applied by the repository.
pub struct LocalProductsClient {
    db: Db,
    enforcer: Arc<PolicyEnforcer>,
}
impl LocalProductsClient {
    /// Use the same database and enforcer as the REST doors.
    pub fn new(db: Db, enforcer: Arc<PolicyEnforcer>) -> Self {
        Self { db, enforcer }
    }
}
#[async_trait]
impl ProductsClient for LocalProductsClient {
    async fn get_sku(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        sku_id: Uuid,
    ) -> Result<Sku, CanonicalError> {
        let scope = access_scope(
            &self.enforcer,
            ctx,
            &resource_types::SKU,
            actions::READ,
            None,
        )
        .await
        .map_err(|e| {
            authz_error_to_canonical(e, |reason| {
                SkuResource::permission_denied()
                    .with_reason(reason)
                    .create()
            })
        })?;
        let conn = self
            .db
            .conn()
            .map_err(|e| CanonicalError::internal(e.to_string()).create())?;
        repo::find_sku(&conn, &scope, tenant_id, sku_id)
            .await
            .map_err(|e| repo_error_to_canonical(&e))?
            .ok_or_else(|| {
                SkuResource::not_found("SKU not found")
                    .with_resource(sku_id.to_string())
                    .create()
            })
    }
}
