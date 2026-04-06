//! Domain service for the static tenant resolver plugin.

use std::collections::{HashMap, HashSet};

use modkit_macros::domain_model;
use tenant_resolver_sdk::{
    BarrierMode, TenantId, TenantInfo, TenantRef, TenantResolverError, TenantStatus, matches_status,
};

use crate::config::StaticTrPluginConfig;

/// Static tenant resolver service.
///
/// Stores tenant data in memory, loaded from configuration.
/// Supports hierarchical tenant model with parent-child relationships.
#[domain_model]
pub struct Service {
    /// Tenant info by ID.
    pub(super) tenants: HashMap<TenantId, TenantInfo>,

    /// Children index: `parent_id` -> list of child tenant IDs.
    pub(super) children: HashMap<TenantId, Vec<TenantId>>,
}

impl Service {
    /// Creates a new service from configuration.
    #[must_use]
    pub fn from_config(cfg: &StaticTrPluginConfig) -> Self {
        let tenants: HashMap<TenantId, TenantInfo> = cfg
            .tenants
            .iter()
            .map(|t| {
                (
                    TenantId(t.id),
                    TenantInfo {
                        id: TenantId(t.id),
                        name: t.name.clone(),
                        status: t.status,
                        tenant_type: t.tenant_type.clone(),
                        parent_id: t.parent_id.map(TenantId),
                        self_managed: t.self_managed,
                    },
                )
            })
            .collect();

        // Build children index
        let mut children: HashMap<TenantId, Vec<TenantId>> = HashMap::new();
        for tenant in tenants.values() {
            if let Some(parent_id) = tenant.parent_id {
                children.entry(parent_id).or_default().push(tenant.id);
            }
        }

        Self { tenants, children }
    }

    /// Check if a tenant matches the status filter.
    pub(super) fn matches_status_filter(tenant: &TenantInfo, statuses: &[TenantStatus]) -> bool {
        matches_status(tenant, statuses)
    }

    /// Collect ancestors from a tenant to root.
    ///
    /// Returns ancestors ordered from direct parent to root.
    /// Stops at barriers unless `barrier_mode` is `Ignore`.
    /// If the starting tenant itself is a barrier, returns empty (consistent
    /// with `is_ancestor` returning `false` for barrier descendants).
    ///
    /// Note: The starting tenant is NOT included in the result.
    pub(super) fn collect_ancestors(
        &self,
        id: TenantId,
        barrier_mode: BarrierMode,
    ) -> Vec<TenantRef> {
        let mut ancestors = Vec::new();
        let mut visited = HashSet::new();
        visited.insert(id);

        // Start from the tenant's parent
        let Some(tenant) = self.tenants.get(&id) else {
            return ancestors;
        };

        // If the starting tenant is a barrier, it cannot see its parent chain
        if barrier_mode == BarrierMode::Respect && tenant.self_managed {
            return ancestors;
        }

        let mut current_parent_id = tenant.parent_id;

        while let Some(parent_id) = current_parent_id {
            if !visited.insert(parent_id) {
                break; // Cycle detected
            }

            let Some(parent) = self.tenants.get(&parent_id) else {
                break;
            };

            // Barrier semantics: include the barrier tenant, but stop traversal
            // at it (don't continue to its parent).
            ancestors.push(parent.into());

            if barrier_mode == BarrierMode::Respect && parent.self_managed {
                break;
            }

            current_parent_id = parent.parent_id;
        }

        ancestors
    }

    /// Collect descendants subtree using pre-order traversal.
    ///
    /// Returns descendants (not including the starting tenant) in pre-order:
    /// parent is visited before children.
    ///
    /// Traversal stops when:
    /// - `self_managed` barrier is encountered (unless `barrier_mode` is `Ignore`)
    /// - Node doesn't pass the filter (filtered nodes and their subtrees are excluded)
    /// - `max_depth` is reached
    pub(super) fn collect_descendants(
        &self,
        id: TenantId,
        statuses: &[TenantStatus],
        barrier_mode: BarrierMode,
        max_depth: Option<u32>,
    ) -> Vec<TenantRef> {
        let mut collector = DescendantCollector {
            tenants: &self.tenants,
            children: &self.children,
            statuses,
            barrier_mode,
            max_depth,
            result: Vec::new(),
            visited: HashSet::new(),
        };
        collector.visited.insert(id);
        collector.collect(id, 1);
        collector.result
    }

    /// Check if `ancestor_id` is an ancestor of `descendant_id`.
    ///
    /// Returns `true` if `ancestor_id` is in the parent chain of `descendant_id`.
    /// Returns `false` if `ancestor_id == descendant_id` (self is not an ancestor of self).
    ///
    /// Respects barriers: if there's a barrier between them, returns `false`.
    ///
    /// # Errors
    ///
    /// Returns `TenantNotFound` if either tenant does not exist.
    pub(super) fn is_ancestor_of(
        &self,
        ancestor_id: TenantId,
        descendant_id: TenantId,
        barrier_mode: BarrierMode,
    ) -> Result<bool, TenantResolverError> {
        // Self is NOT an ancestor of self
        if ancestor_id == descendant_id {
            if self.tenants.contains_key(&ancestor_id) {
                return Ok(false);
            }
            return Err(TenantResolverError::TenantNotFound {
                tenant_id: ancestor_id,
            });
        }

        // Check both tenants exist
        if !self.tenants.contains_key(&ancestor_id) {
            return Err(TenantResolverError::TenantNotFound {
                tenant_id: ancestor_id,
            });
        }

        let descendant =
            self.tenants
                .get(&descendant_id)
                .ok_or(TenantResolverError::TenantNotFound {
                    tenant_id: descendant_id,
                })?;

        // If the descendant itself is a barrier, the ancestor cannot claim
        // parentage — consistent with get_descendants excluding barriers.
        if barrier_mode == BarrierMode::Respect && descendant.self_managed {
            return Ok(false);
        }

        // Walk up the chain from descendant
        let mut visited = HashSet::new();
        visited.insert(descendant_id);
        let mut current_parent_id = descendant.parent_id;

        while let Some(parent_id) = current_parent_id {
            if !visited.insert(parent_id) {
                break; // Cycle detected
            }

            let Some(parent) = self.tenants.get(&parent_id) else {
                break;
            };

            // Found the ancestor
            if parent_id == ancestor_id {
                return Ok(true);
            }

            // Barrier semantics: if the parent is self_managed and not the target
            // ancestor, traversal is blocked beyond this point.
            if barrier_mode == BarrierMode::Respect && parent.self_managed {
                return Ok(false);
            }

            current_parent_id = parent.parent_id;
        }

        // Reached root without finding ancestor
        Ok(false)
    }
}

/// Encapsulates traversal state for collecting descendants.
///
/// Eliminates the need for passing many arguments through recursive calls.
#[domain_model]
struct DescendantCollector<'a> {
    tenants: &'a HashMap<TenantId, TenantInfo>,
    children: &'a HashMap<TenantId, Vec<TenantId>>,
    statuses: &'a [TenantStatus],
    barrier_mode: BarrierMode,
    max_depth: Option<u32>,
    result: Vec<TenantRef>,
    visited: HashSet<TenantId>,
}

impl DescendantCollector<'_> {
    fn collect(&mut self, parent_id: TenantId, current_depth: u32) {
        // Check depth limit (None = unlimited)
        if self.max_depth.is_some_and(|d| current_depth > d) {
            return;
        }

        let Some(child_ids) = self.children.get(&parent_id) else {
            return;
        };

        for child_id in child_ids {
            if !self.visited.insert(*child_id) {
                continue;
            }

            let Some(child) = self.tenants.get(child_id) else {
                continue;
            };

            // If respecting barriers and this child is self_managed, skip it and its subtree
            if self.barrier_mode == BarrierMode::Respect && child.self_managed {
                continue;
            }

            // If child doesn't pass status filter, skip it AND its subtree
            if !Service::matches_status_filter(child, self.statuses) {
                continue;
            }

            self.result.push(child.into());

            // Recurse into children
            self.collect(*child_id, current_depth + 1);
        }
    }
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod tests;
