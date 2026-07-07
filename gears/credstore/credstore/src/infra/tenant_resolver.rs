//! Infra adapter: `TenantDirectory` backed by `TenantResolverClient` with a TTL cache.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tenant_resolver_sdk::{BarrierMode, GetAncestorsOptions, TenantResolverClient};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::ports::metrics::{CredStoreMetricsPort, Dep, DepOp, Outcome};
use crate::domain::resolver::TenantDirectory;
use credstore_sdk::TenantId;

/// Maximum cache entries before a full eviction.
const CACHE_CAP: usize = 4096;

/// Entry: `(chain, inserted_at)`.
type CacheEntry = (Vec<Uuid>, Instant);

/// Infra implementation of [`TenantDirectory`] backed by [`TenantResolverClient`].
pub struct TenantResolverDir {
    client: Arc<dyn TenantResolverClient>,
    metrics: Arc<dyn CredStoreMetricsPort>,
    /// Ancestor chains keyed by tenant id. Safe to share across security
    /// contexts: the chain is a pure function of the tenant topology and the
    /// fixed barrier-respecting mode — it carries no caller-specific data — and
    /// all authorization is enforced downstream (`scope_includes_tenant` +
    /// `resolve_for_get`). So a chain populated under one caller's `ctx` is
    /// valid for any caller resolving the same tenant.
    cache: Mutex<HashMap<Uuid, CacheEntry>>,
    ttl: Duration,
}

impl TenantResolverDir {
    /// Construct a new adapter.
    #[must_use]
    pub fn new(
        client: Arc<dyn TenantResolverClient>,
        metrics: Arc<dyn CredStoreMetricsPort>,
        ttl_secs: u64,
    ) -> Self {
        Self {
            client,
            metrics,
            cache: Mutex::new(HashMap::new()),
            ttl: Duration::from_secs(ttl_secs),
        }
    }
}

#[async_trait]
impl TenantDirectory for TenantResolverDir {
    async fn ancestor_chain(
        &self,
        ctx: &SecurityContext,
        req: TenantId,
    ) -> Result<Vec<Uuid>, DomainError> {
        // Cache lookup — no metric on hit.
        {
            let guard = self
                .cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some((chain, inserted)) = guard.get(&req.0)
                && inserted.elapsed() < self.ttl
            {
                return Ok(chain.clone());
            }
        }

        let t0 = Instant::now();
        // Respect self-managed tenant boundaries: a secret must not be inherited
        // across an isolation barrier, so the walk-up stops at the boundary.
        let opts = GetAncestorsOptions {
            barrier_mode: BarrierMode::Respect,
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

        {
            let mut guard = self
                .cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.len() >= CACHE_CAP {
                // Drop expired entries first; only if still at cap evict the
                // single oldest. A blunt clear() would periodically blow away
                // every still-fresh chain and cause a thundering herd of misses.
                let ttl = self.ttl;
                guard.retain(|_, (_, inserted)| inserted.elapsed() < ttl);
                if guard.len() >= CACHE_CAP
                    && let Some(oldest) = guard
                        .iter()
                        .min_by_key(|(_, (_, inserted))| *inserted)
                        .map(|(k, _)| *k)
                {
                    guard.remove(&oldest);
                }
            }
            guard.insert(req.0, (chain.clone(), Instant::now()));
        }

        Ok(chain)
    }
}

#[cfg(test)]
#[path = "tenant_resolver_tests.rs"]
mod tests;
