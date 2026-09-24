// Created: 2026-09-06 by Virtuozzo International GmbH
//! `TenantHierarchy` over the tenant resolver, resolved from `ClientHub` at
//! first use.
//!
//! The tenant resolver SDK publishes no REST projection, so its client cannot
//! be a `#[toolkit::consumes]` field; it is looked up in the hub when first
//! asked for, and an unwired resolver is unavailability, not a guess.

use std::sync::Arc;

use async_trait::async_trait;
use tenant_resolver_sdk::{
    BarrierMode, GetAncestorsOptions, GetDescendantsOptions, IsAncestorOptions, TenantId,
    TenantResolverClient, TenantResolverError,
};
use toolkit::ClientHub;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::resolution::TenantHierarchy;

/// The adapter.
pub struct HubTenantHierarchy {
    hub: Arc<ClientHub>,
}

impl HubTenantHierarchy {
    /// Resolve the tenant resolver through this hub.
    #[must_use]
    pub fn new(hub: Arc<ClientHub>) -> Self {
        Self { hub }
    }

    fn client(&self) -> Result<Arc<dyn TenantResolverClient>, DomainError> {
        self.hub
            .get::<dyn TenantResolverClient>()
            .map_err(|e| DomainError::Unavailable {
                detail: format!("tenant resolver: {e}"),
            })
    }
}

/// The deepest level a descendants request asks the resolver for: well above
/// the design's ten-level anchor, and the one bound the resolver's SDK offers
/// on the request itself. A node found at this depth may have children below
/// it, so a walk that reaches it reports itself cut.
pub const SUBTREE_DEPTH_CEILING: u32 = 32;

fn map(err: TenantResolverError) -> DomainError {
    match err {
        TenantResolverError::TenantNotFound { .. } => DomainError::NotFound { resource: "tenant" },
        other => DomainError::Unavailable {
            detail: format!("tenant resolver: {other}"),
        },
    }
}

#[async_trait]
impl TenantHierarchy for HubTenantHierarchy {
    async fn chain(&self, tenant: Uuid) -> Result<Vec<Uuid>, DomainError> {
        let client = self.client()?;
        // Barriers ignored: runtime resolution walks the whole chain, because
        // a standalone tenant still needs the platform's defaults.
        let response = client
            .get_ancestors(
                &SecurityContext::anonymous(),
                TenantId(tenant),
                &GetAncestorsOptions {
                    barrier_mode: BarrierMode::Ignore,
                },
            )
            .await
            .map_err(map)?;
        // The resolver answers parent-first; the walk wants root-first with the
        // requested tenant as the last element.
        let mut chain: Vec<Uuid> = response.ancestors.iter().rev().map(|r| r.id.0).collect();
        chain.push(tenant);
        Ok(chain)
    }

    async fn is_within_subtree(&self, caller: Uuid, target: Uuid) -> Result<bool, DomainError> {
        if caller == target {
            return Ok(true);
        }
        let client = self.client()?;
        client
            .is_ancestor(
                &SecurityContext::anonymous(),
                TenantId(caller),
                TenantId(target),
                &IsAncestorOptions {
                    barrier_mode: BarrierMode::Respect,
                },
            )
            .await
            .map_err(map)
    }

    async fn is_standalone(&self, tenant: Uuid) -> Result<bool, DomainError> {
        let client = self.client()?;
        client
            .get_tenant(&SecurityContext::anonymous(), TenantId(tenant))
            .await
            .map(|info| info.self_managed)
            .map_err(map)
    }

    async fn descendants_bfs(
        &self,
        tenant: Uuid,
        budget: usize,
    ) -> Result<(Vec<Uuid>, bool), DomainError> {
        let client = self.client()?;
        // Bounded on the request as far as the SDK allows — by depth. There is
        // no count and no cursor on `get_descendants`, so a wide tree still
        // comes back whole, and the budget is applied to what arrived.
        let response = client
            .get_descendants(
                &SecurityContext::anonymous(),
                TenantId(tenant),
                &GetDescendantsOptions {
                    barrier_mode: BarrierMode::Respect,
                    max_depth: Some(SUBTREE_DEPTH_CEILING),
                    ..GetDescendantsOptions::default()
                },
            )
            .await
            .map_err(map)?;
        // The resolver answers a flat set; breadth-first order is rebuilt from
        // the parent links it carries. The links are data, not a proof of a
        // tree: a child named under two parents, or a pair naming each other,
        // is walked once — the order holds distinct tenants and the budget
        // counts distinct tenants, so a cycle does not read as truncation.
        let mut children: std::collections::HashMap<Uuid, Vec<Uuid>> =
            std::collections::HashMap::new();
        for r in &response.descendants {
            if let Some(parent) = r.parent_id {
                children.entry(parent.0).or_default().push(r.id.0);
            }
        }
        let mut order = Vec::new();
        let mut seen: std::collections::HashSet<Uuid> = std::collections::HashSet::from([tenant]);
        let mut queue = std::collections::VecDeque::from([(tenant, 0_u32)]);
        let mut truncated = false;
        'walk: while let Some((next, depth)) = queue.pop_front() {
            for child in children.get(&next).into_iter().flatten() {
                if !seen.insert(*child) {
                    continue;
                }
                if order.len() >= budget {
                    truncated = true;
                    break 'walk;
                }
                order.push(*child);
                // A node at the ceiling was answered without its children: what
                // lies below is unknown, and the walk says so.
                if depth + 1 >= SUBTREE_DEPTH_CEILING {
                    truncated = true;
                } else {
                    queue.push_back((*child, depth + 1));
                }
            }
        }
        Ok((order, truncated))
    }
}

#[cfg(test)]
#[path = "tenant_hierarchy_tests.rs"]
mod tenant_hierarchy_tests;
