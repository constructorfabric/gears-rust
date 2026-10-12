//! Lazy resolution permits pricing to boot before Products registers its owner-bound client.
//!
//! The registry's reads (`states`, `sku_for_write`, `skus_for_write`, `sku_version_as_of`) run on a task of
//! their own. The approval subjects read SKUs inside the unit's transaction (D-408; D-402's dated
//! metering), and Products' in-process registry opens a connection to its own database, which
//! toolkit-db refuses on a task that is inside a transaction (`ConnRequestedInsideTx`): on the
//! caller's task every such read failed as an unavailable registry. The reads carry no side
//! effect, so a caller that is dropped only discards the answer, as with a registry out of
//! process. The writes (`reserve`, `confirm`, `release`) are made between transactions (D-401)
//! and stay on the caller's task.
use bss_products_sdk::{
    PricingReferenceRegistry, ReferenceRegistryV1,
    models::{ReferenceKind, ReferenceState, ReservationReceipt, Sku, SkuVersion},
};
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;
/// Resolve on each use; absence never gets cached as a permanent startup failure.
/// # Errors
/// Returns 503 `REGISTRY_UNAVAILABLE` when Products has not registered pricing's key.
pub fn resolve(hub: &toolkit::ClientHub) -> Result<Arc<dyn ReferenceRegistryV1>, CanonicalError> {
    hub.get::<PricingReferenceRegistry>()
        .map(|key| Arc::new(Detached(key.0.clone())) as Arc<dyn ReferenceRegistryV1>)
        .map_err(|_| {
            CanonicalError::service_unavailable()
                .with_detail("REGISTRY_UNAVAILABLE: Products reference registry is unavailable")
                .create()
        })
}
/// The registry with its reads on their own task (see the module docs).
struct Detached(Arc<dyn ReferenceRegistryV1>);
/// A read task that did not finish (it panicked or was cancelled) is an unavailable registry.
fn unfinished(error: &tokio::task::JoinError) -> CanonicalError {
    tracing::warn!(%error, "bss-pricing: a Products registry read did not finish");
    CanonicalError::service_unavailable()
        .with_detail("REGISTRY_UNAVAILABLE: Products reference registry is unavailable")
        .create()
}
#[async_trait::async_trait]
impl ReferenceRegistryV1 for Detached {
    async fn reserve(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        sku: Uuid,
        kind: ReferenceKind,
        ref_id: Uuid,
    ) -> Result<ReservationReceipt, CanonicalError> {
        self.0.reserve(ctx, tenant, sku, kind, ref_id).await
    }
    async fn confirm(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<(), CanonicalError> {
        self.0.confirm(ctx, tenant, id).await
    }
    async fn release(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<(), CanonicalError> {
        self.0.release(ctx, tenant, id).await
    }
    async fn states(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        ids: &[Uuid],
    ) -> Result<Vec<(Uuid, ReferenceState)>, CanonicalError> {
        let (inner, ctx, ids) = (self.0.clone(), ctx.clone(), ids.to_vec());
        tokio::spawn(async move { inner.states(&ctx, tenant, &ids).await })
            .await
            .map_err(|e| unfinished(&e))?
    }
    async fn sku_for_write(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<Sku, CanonicalError> {
        let (inner, ctx) = (self.0.clone(), ctx.clone());
        tokio::spawn(async move { inner.sku_for_write(&ctx, tenant, id).await })
            .await
            .map_err(|e| unfinished(&e))?
    }
    async fn skus_for_write(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        ids: &[Uuid],
    ) -> Result<Vec<Sku>, CanonicalError> {
        let (inner, ctx, ids) = (self.0.clone(), ctx.clone(), ids.to_vec());
        tokio::spawn(async move { inner.skus_for_write(&ctx, tenant, &ids).await })
            .await
            .map_err(|e| unfinished(&e))?
    }
    async fn sku_version_as_of(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
        date: time::Date,
    ) -> Result<Option<SkuVersion>, CanonicalError> {
        let (inner, ctx) = (self.0.clone(), ctx.clone());
        tokio::spawn(async move { inner.sku_version_as_of(&ctx, tenant, id, date).await })
            .await
            .map_err(|e| unfinished(&e))?
    }
}
