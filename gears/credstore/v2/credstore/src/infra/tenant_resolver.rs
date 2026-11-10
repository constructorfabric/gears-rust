//! Infra adapter: `TenantDirectory` backed by `TenantResolverClient` without a cache.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use tenant_resolver_sdk::{
    BarrierMode, GetAncestorsOptions, IsAncestorOptions, TenantResolverClient, TenantResolverError,
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::metrics::{CredStoreMetricsPort, Dep, DepOp, Outcome};
use crate::domain::resolver::TenantDirectory;
use credstore_sdk::TenantId;

/// Infra implementation of [`TenantDirectory`] backed by [`TenantResolverClient`].
///
/// Keeps no cache: every call reads the ancestor chain from tenant-resolver, so
/// a tenant change is followed at once. If this ever becomes slow, a single
/// shared cache belongs in tenant-resolver (for every gear, with its own
/// invalidation), not here.
pub struct TenantResolverDir {
    client: Arc<dyn TenantResolverClient>,
    metrics: Arc<dyn CredStoreMetricsPort>,
}

impl TenantResolverDir {
    /// Construct a new adapter.
    #[must_use]
    pub fn new(
        client: Arc<dyn TenantResolverClient>,
        metrics: Arc<dyn CredStoreMetricsPort>,
    ) -> Self {
        Self { client, metrics }
    }
}

#[async_trait]
impl TenantDirectory for TenantResolverDir {
    async fn ancestor_chain(
        &self,
        ctx: &SecurityContext,
        req: TenantId,
    ) -> Result<Vec<Uuid>, DomainError> {
        let t0 = Instant::now();
        // Walk the full ancestry: `shared` secrets inherit through self-managed
        // (isolation-barrier) boundaries by design — a partner key stays
        // resolvable for customers that manage their own sub-tree. Whether the
        // caller may read at all is the PDP's decision, not the chain's.
        let opts = GetAncestorsOptions {
            barrier_mode: BarrierMode::Ignore,
        };
        let resp = match self.client.get_ancestors(ctx, req, &opts).await {
            Ok(r) => {
                self.metrics.dependency(
                    Dep::TenantResolver,
                    DepOp::GetAncestors,
                    Outcome::Success,
                    t0.elapsed().as_secs_f64(),
                );
                r
            }
            Err(e) => {
                self.metrics.dependency(
                    Dep::TenantResolver,
                    DepOp::GetAncestors,
                    Outcome::Error,
                    t0.elapsed().as_secs_f64(),
                );
                // Wire-visible detail stays curated (`with_detail` contract);
                // the raw dependency error goes to the log + cause chain only.
                tracing::warn!(err = %e, "tenant_resolver get_ancestors failed");
                return Err(DomainError::ServiceUnavailable {
                    detail: "tenant resolver unavailable".to_owned(),
                    retry_after: None,
                    cause: Some(Box::new(e)),
                });
            }
        };

        let mut chain = Vec::with_capacity(1 + resp.ancestors.len());
        chain.push(req.0);
        chain.extend(resp.ancestors.iter().map(|a| a.id.0));

        Ok(chain)
    }

    async fn is_ancestor(
        &self,
        ctx: &SecurityContext,
        ancestor: TenantId,
        descendant: TenantId,
    ) -> Result<bool, DomainError> {
        let t0 = Instant::now();
        // Barriers ignored, like the ancestor walk: a row behind an isolation
        // barrier still holds its reference under the creator's hierarchy
        // (`fr-override-type-consistency`).
        let opts = IsAncestorOptions {
            barrier_mode: BarrierMode::Ignore,
        };
        let res = self
            .client
            .is_ancestor(ctx, ancestor, descendant, &opts)
            .await;
        match res {
            Ok(is) => {
                self.metrics.dependency(
                    Dep::TenantResolver,
                    DepOp::IsAncestor,
                    Outcome::Success,
                    t0.elapsed().as_secs_f64(),
                );
                Ok(is)
            }
            // A tenant deleted in Account Management while its rows remain:
            // not a descendant of anything.
            Err(TenantResolverError::TenantNotFound { .. }) => {
                self.metrics.dependency(
                    Dep::TenantResolver,
                    DepOp::IsAncestor,
                    Outcome::Success,
                    t0.elapsed().as_secs_f64(),
                );
                Ok(false)
            }
            Err(e) => {
                self.metrics.dependency(
                    Dep::TenantResolver,
                    DepOp::IsAncestor,
                    Outcome::Error,
                    t0.elapsed().as_secs_f64(),
                );
                tracing::warn!(err = %e, "tenant_resolver is_ancestor failed");
                Err(DomainError::ServiceUnavailable {
                    detail: "tenant resolver unavailable".to_owned(),
                    retry_after: None,
                    cause: Some(Box::new(e)),
                })
            }
        }
    }
}

#[cfg(test)]
#[path = "tenant_resolver_tests.rs"]
mod tests;
