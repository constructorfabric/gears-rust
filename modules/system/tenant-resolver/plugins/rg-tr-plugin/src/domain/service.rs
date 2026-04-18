//! Domain service for the RG tenant resolver plugin.
//!
//! Maps Resource Group hierarchy data to tenant resolver models.
//! Groups with the configured tenant type path are treated as tenants;
//! all other groups are invisible to this plugin.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use modkit_macros::domain_model;
use modkit_odata::ODataQuery;
use modkit_security::SecurityContext;
use resource_group_sdk::api::ResourceGroupReadHierarchy;
use resource_group_sdk::error::ResourceGroupError;
use resource_group_sdk::models::ResourceGroupWithDepth;
use tenant_resolver_sdk::{
    BarrierMode, TenantId, TenantInfo, TenantRef, TenantResolverError, TenantStatus,
};

/// RG-based tenant resolver service.
///
/// Resolves tenant data from Resource Group hierarchy. Only groups
/// whose `type_path` matches `tenant_type_path` are treated as tenants.
#[domain_model]
pub struct Service {
    rg: Arc<dyn ResourceGroupReadHierarchy>,
    tenant_type_path: String,
}

impl Service {
    pub fn new(rg: Arc<dyn ResourceGroupReadHierarchy>, tenant_type_path: String) -> Self {
        Self {
            rg,
            tenant_type_path,
        }
    }

    // -- Public query methods (called from client.rs) --

    /// Get a single tenant by ID.
    ///
    /// Calls `get_group_ancestors` with type filter to retrieve the depth=0 item.
    /// Returns `TenantNotFound` if the group doesn't exist or isn't a tenant type.
    pub(super) async fn resolve_tenant(
        &self,
        ctx: &SecurityContext,
        id: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        let query = self.build_type_filter_query();
        let page = self
            .rg
            .get_group_ancestors(ctx, id.0, &query)
            .await
            .map_err(|e| match e {
                ResourceGroupError::NotFound { .. } => {
                    TenantResolverError::TenantNotFound { tenant_id: id }
                }
                other => TenantResolverError::Internal(other.to_string()),
            })?;

        // depth=0 is the group itself
        page.items
            .iter()
            .find(|g| g.hierarchy.depth == 0)
            .map(map_to_tenant_info)
            .ok_or(TenantResolverError::TenantNotFound { tenant_id: id })
    }

    /// Resolve ancestors of a tenant.
    ///
    /// Returns the tenant itself (depth=0) and ancestors (depth < 0) ordered
    /// from direct parent to root. Applies barrier filtering in memory.
    pub(super) async fn resolve_ancestors(
        &self,
        ctx: &SecurityContext,
        id: TenantId,
        barrier_mode: BarrierMode,
    ) -> Result<(TenantRef, Vec<TenantRef>), TenantResolverError> {
        let query = self.build_type_filter_query();
        let items = self.drain_hierarchy_pages(ctx, id.0, &query, Direction::Ancestors).await?;

        // Split: depth=0 is the tenant, depth<0 are ancestors
        let tenant_group = items
            .iter()
            .find(|g| g.hierarchy.depth == 0)
            .ok_or(TenantResolverError::TenantNotFound { tenant_id: id })?;

        let tenant_ref = map_to_tenant_ref(tenant_group);

        // Ancestors ordered by depth ascending (direct parent first = depth -1, then -2, etc.)
        let mut ancestors: Vec<&ResourceGroupWithDepth> = items
            .iter()
            .filter(|g| g.hierarchy.depth < 0)
            .collect();
        ancestors.sort_by_key(|g| std::cmp::Reverse(g.hierarchy.depth)); // -1, -2, -3...

        let ancestor_refs: Vec<TenantRef> = ancestors
            .iter()
            .map(|g| map_to_tenant_ref(g))
            .collect();

        // Apply barrier filtering
        let filtered = filter_ancestors_by_barrier(&tenant_ref, ancestor_refs, barrier_mode);

        Ok((tenant_ref, filtered))
    }

    /// Resolve descendants of a tenant.
    ///
    /// Returns the tenant itself (depth=0) and descendants (depth > 0).
    /// Applies barrier, status, and `max_depth` filtering in memory.
    pub(super) async fn resolve_descendants(
        &self,
        ctx: &SecurityContext,
        id: TenantId,
        statuses: &[TenantStatus],
        barrier_mode: BarrierMode,
        max_depth: Option<u32>,
    ) -> Result<(TenantRef, Vec<TenantRef>), TenantResolverError> {
        let query = self.build_type_filter_query();
        let items =
            self.drain_hierarchy_pages(ctx, id.0, &query, Direction::Descendants).await?;

        // Split: depth=0 is the tenant, depth>0 are descendants
        let tenant_group = items
            .iter()
            .find(|g| g.hierarchy.depth == 0)
            .ok_or(TenantResolverError::TenantNotFound { tenant_id: id })?;

        let tenant_ref = map_to_tenant_ref(tenant_group);

        let descendants: Vec<&ResourceGroupWithDepth> = items
            .iter()
            .filter(|g| g.hierarchy.depth > 0)
            .collect();

        let filtered =
            filter_descendants_by_barrier(&descendants, statuses, barrier_mode, max_depth);

        Ok((tenant_ref, filtered))
    }

    /// Check if `ancestor_id` is an ancestor of `descendant_id`.
    pub(super) async fn check_is_ancestor(
        &self,
        ctx: &SecurityContext,
        ancestor_id: TenantId,
        descendant_id: TenantId,
        barrier_mode: BarrierMode,
    ) -> Result<bool, TenantResolverError> {
        // Self is NOT an ancestor of self
        if ancestor_id == descendant_id {
            // Verify the tenant exists
            self.resolve_tenant(ctx, ancestor_id).await?;
            return Ok(false);
        }

        // Verify ancestor exists
        self.resolve_tenant(ctx, ancestor_id).await?;

        // Get ancestors of descendant with barrier filtering
        let (descendant_ref, ancestors) =
            self.resolve_ancestors(ctx, descendant_id, barrier_mode).await?;

        // If descendant is self_managed and we respect barriers, no ancestor can claim parentage
        if barrier_mode == BarrierMode::Respect && descendant_ref.self_managed {
            return Ok(false);
        }

        Ok(ancestors.iter().any(|a| a.id == ancestor_id))
    }

    // -- Internal helpers --

    fn build_type_filter_query(&self) -> ODataQuery {
        let filter_str = format!("type eq '{}'", self.tenant_type_path);
        match modkit_odata::parse_filter_string(&filter_str) {
            Ok(parsed) => ODataQuery::default().with_filter(parsed.into_expr()),
            Err(_) => ODataQuery::default(),
        }
    }

    /// Drain all pages from a hierarchy query.
    async fn drain_hierarchy_pages(
        &self,
        ctx: &SecurityContext,
        group_id: uuid::Uuid,
        base_query: &ODataQuery,
        direction: Direction,
    ) -> Result<Vec<ResourceGroupWithDepth>, TenantResolverError> {
        let mut all_items = Vec::new();
        let mut query = base_query.clone();

        loop {
            let page = match direction {
                Direction::Ancestors => self.rg.get_group_ancestors(ctx, group_id, &query).await,
                Direction::Descendants => {
                    self.rg.get_group_descendants(ctx, group_id, &query).await
                }
            }
            .map_err(|e| match e {
                ResourceGroupError::NotFound { .. } => TenantResolverError::TenantNotFound {
                    tenant_id: TenantId(group_id),
                },
                other => TenantResolverError::Internal(other.to_string()),
            })?;

            all_items.extend(page.items);

            match page.page_info.next_cursor {
                Some(cursor_str) => {
                    let cursor = modkit_odata::CursorV1::decode(&cursor_str).map_err(|e| {
                        TenantResolverError::Internal(format!("Invalid cursor: {e}"))
                    })?;
                    query = query.with_cursor(cursor);
                }
                None => break,
            }
        }

        Ok(all_items)
    }

}

#[derive(Clone, Copy)]
enum Direction {
    Ancestors,
    Descendants,
}

// -- Mapping helpers --

fn map_to_tenant_info(group: &ResourceGroupWithDepth) -> TenantInfo {
    TenantInfo {
        id: TenantId(group.id),
        name: group.name.clone(),
        status: parse_status_from_metadata(group.metadata.as_ref()),
        tenant_type: Some(group.type_path.clone()),
        parent_id: group.hierarchy.parent_id.map(TenantId),
        self_managed: parse_self_managed_from_metadata(group.metadata.as_ref()),
    }
}

fn map_to_tenant_ref(group: &ResourceGroupWithDepth) -> TenantRef {
    TenantRef {
        id: TenantId(group.id),
        status: parse_status_from_metadata(group.metadata.as_ref()),
        tenant_type: Some(group.type_path.clone()),
        parent_id: group.hierarchy.parent_id.map(TenantId),
        self_managed: parse_self_managed_from_metadata(group.metadata.as_ref()),
    }
}

// -- Metadata parsing helpers --

fn parse_status_from_metadata(metadata: Option<&serde_json::Value>) -> TenantStatus {
    metadata
        .and_then(|m| m.get("status"))
        .and_then(serde_json::Value::as_str)
        .map_or(TenantStatus::Active, |s| match s {
            "suspended" => TenantStatus::Suspended,
            "deleted" => TenantStatus::Deleted,
            _ => TenantStatus::Active,
        })
}

fn parse_self_managed_from_metadata(metadata: Option<&serde_json::Value>) -> bool {
    metadata
        .and_then(|m| m.get("self_managed"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

// -- Barrier filtering --

/// Filter ancestors by barrier semantics.
///
/// If the starting tenant is `self_managed`, return empty ancestors (cannot see parent chain).
/// Otherwise, walk the parent chain; include each ancestor, stop after a `self_managed` one.
fn filter_ancestors_by_barrier(
    tenant: &TenantRef,
    ancestors: Vec<TenantRef>,
    barrier_mode: BarrierMode,
) -> Vec<TenantRef> {
    if barrier_mode == BarrierMode::Ignore {
        return ancestors;
    }

    // If the starting tenant is self_managed, it cannot see its parent chain
    if tenant.self_managed {
        return Vec::new();
    }

    let mut filtered = Vec::new();
    for ancestor in ancestors {
        let is_barrier = ancestor.self_managed;
        filtered.push(ancestor);
        if is_barrier {
            break; // Include barrier, stop traversal
        }
    }
    filtered
}

/// Filter descendants by barrier, status, and `max_depth`.
///
/// Uses pre-order DFS traversal. Barrier children (and their subtrees)
/// are excluded when `barrier_mode` is `Respect`.
fn filter_descendants_by_barrier(
    descendants: &[&ResourceGroupWithDepth],
    statuses: &[TenantStatus],
    barrier_mode: BarrierMode,
    max_depth: Option<u32>,
) -> Vec<TenantRef> {
    // Build parent_id → children index
    let mut children_map: HashMap<uuid::Uuid, Vec<&ResourceGroupWithDepth>> = HashMap::new();
    for g in descendants {
        if let Some(pid) = g.hierarchy.parent_id {
            children_map.entry(pid).or_default().push(g);
        }
    }

    // Also need the root group's ID (depth=0 parent) to find direct children
    // Direct children of the tenant have the tenant as parent.
    // But we receive descendants only (depth > 0). We need to find depth=1 items.
    // depth=1 items are direct children of the queried tenant.

    // Collect all IDs of groups in descendants set
    let descendant_ids: HashSet<uuid::Uuid> = descendants.iter().map(|g| g.id).collect();

    // Find root children: groups whose parent_id is NOT in the descendant set
    // (meaning their parent is the queried tenant at depth=0)
    let mut roots: Vec<&ResourceGroupWithDepth> = descendants
        .iter()
        .filter(|g| {
            g.hierarchy
                .parent_id
                .is_none_or(|pid| !descendant_ids.contains(&pid))
        })
        .copied()
        .collect();
    // Sort roots by depth for stable ordering
    roots.sort_by_key(|g| g.hierarchy.depth);

    let mut result = Vec::new();
    let mut stack: Vec<(&ResourceGroupWithDepth, u32)> =
        roots.into_iter().rev().map(|g| (g, 1)).collect();

    while let Some((group, depth)) = stack.pop() {
        // Check max_depth
        if max_depth.is_some_and(|d| depth > d) {
            continue;
        }

        let tenant_ref = map_to_tenant_ref(group);

        // Skip barrier children (+ their subtrees) when respecting barriers
        if barrier_mode == BarrierMode::Respect && tenant_ref.self_managed {
            continue;
        }

        // Skip non-matching status (+ their subtrees)
        if !statuses.is_empty() && !statuses.contains(&tenant_ref.status) {
            continue;
        }

        result.push(tenant_ref);

        // Push children in reverse order for pre-order traversal
        if let Some(children) = children_map.get(&group.id) {
            let mut sorted_children: Vec<&ResourceGroupWithDepth> = children.clone();
            sorted_children.sort_by_key(|g| g.hierarchy.depth);
            for child in sorted_children.into_iter().rev() {
                stack.push((child, depth + 1));
            }
        }
    }

    result
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;
