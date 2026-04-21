//! Domain service for the RG tenant resolver plugin.
//!
//! Maps Resource Group hierarchy data to tenant resolver models.
//! Groups whose type code equals `TENANT_RG_TYPE_PATH` are treated as tenants;
//! all other groups are invisible to this plugin.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use modkit_macros::domain_model;
use modkit_odata::ODataQuery;
use modkit_odata::ast::{CompareOperator, Expr, Value};
use modkit_odata::filter::FilterField;
use modkit_security::SecurityContext;
use resource_group_sdk::TENANT_RG_TYPE_PATH;
use resource_group_sdk::api::ResourceGroupReadHierarchy;
use resource_group_sdk::error::ResourceGroupError;
use resource_group_sdk::models::{ResourceGroup, ResourceGroupWithDepth};
use resource_group_sdk::odata::{GroupFilterField, HierarchyFilterField};
use tenant_resolver_sdk::{
    BarrierMode, TenantId, TenantInfo, TenantRef, TenantResolverError, TenantStatus,
};

/// Build an `eq` comparison predicate for a typed hierarchy filter field.
///
/// Goes through `HierarchyFilterField::name()` so field names are resolved
/// from the typed enum (single source of truth in `resource-group-sdk`)
/// rather than from stringly-typed literals that could drift from the
/// hierarchy schema and silently match nothing.
fn hierarchy_eq(field: HierarchyFilterField, value: Value) -> Expr {
    Expr::Compare(
        Box::new(Expr::Identifier(field.name().to_owned())),
        CompareOperator::Eq,
        Box::new(Expr::Value(value)),
    )
}

/// `type eq '<TENANT_RG_TYPE_PATH>'` — restrict the hierarchy walk to
/// tenant-typed groups only.
fn tenant_type_filter() -> Expr {
    hierarchy_eq(
        HierarchyFilterField::Type,
        Value::String(TENANT_RG_TYPE_PATH.to_owned()),
    )
}

/// `type eq '<TENANT_RG_TYPE_PATH>'` expressed via the groups-filter field
/// enum. Both `HierarchyFilterField::Type` and `GroupFilterField::Type`
/// resolve to the public name `"type"`; this variant is used with
/// `list_groups` (the flat listing endpoint) where `GroupFilterField` is
/// the correct type-safe source of identifiers.
fn tenant_type_filter_for_groups() -> Expr {
    Expr::Compare(
        Box::new(Expr::Identifier(GroupFilterField::Type.name().to_owned())),
        CompareOperator::Eq,
        Box::new(Expr::Value(Value::String(TENANT_RG_TYPE_PATH.to_owned()))),
    )
}

/// Precomputed `type eq '<TENANT_RG_TYPE_PATH>'` query used for multi-row
/// ancestor / descendant walks.
///
/// Built once via the AST constructors instead of re-parsing the constant
/// string on every lookup. The `Box<Expr>` clone that eventually lands in
/// `ODataQuery` happens once per `drain_hierarchy_pages` invocation, not
/// per page.
static TENANT_TYPE_FILTER_QUERY: LazyLock<ODataQuery> =
    LazyLock::new(|| ODataQuery::default().with_filter(tenant_type_filter()));

/// Precomputed `type eq '<TENANT_RG_TYPE_PATH>' and hierarchy/depth eq 0`
/// query used by `resolve_tenant` for single-node lookup.
///
/// The hierarchy API treats the reference group as `depth = 0`; adding the
/// depth predicate bounds the result to that single row so we don't drag
/// the entire ancestor chain back from RG just to read the node's metadata.
static TENANT_SELF_QUERY: LazyLock<ODataQuery> = LazyLock::new(|| {
    let depth_eq_zero = hierarchy_eq(
        HierarchyFilterField::HierarchyDepth,
        Value::Number(0i64.into()),
    );
    ODataQuery::default().with_filter(tenant_type_filter().and(depth_eq_zero))
});

/// RG-based tenant resolver service.
///
/// Resolves tenant data from Resource Group hierarchy. Only groups whose
/// type code equals [`TENANT_RG_TYPE_PATH`] are treated as tenants — the
/// prefix is the single source of truth and lives in `resource-group-sdk`.
#[domain_model]
pub struct Service {
    rg: Arc<dyn ResourceGroupReadHierarchy>,
}

impl Service {
    pub fn new(rg: Arc<dyn ResourceGroupReadHierarchy>) -> Self {
        Self { rg }
    }

    // -- Public query methods (called from client.rs) --

    /// Get a single tenant by ID.
    ///
    /// Queries the RG hierarchy API with a `hierarchy/depth eq 0` filter so
    /// the result is bounded to the requested group itself — no need to drag
    /// back its entire ancestor chain just to read one node's metadata.
    /// Returns `TenantNotFound` if the group doesn't exist or isn't a tenant type.
    pub(super) async fn resolve_tenant(
        &self,
        ctx: &SecurityContext,
        id: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        let items = self
            .drain_hierarchy_pages(ctx, id.0, &TENANT_SELF_QUERY, Direction::Ancestors)
            .await?;

        items
            .iter()
            .find(|g| g.hierarchy.depth == 0)
            .map(map_to_tenant_info)
            .ok_or(TenantResolverError::TenantNotFound { tenant_id: id })
    }

    /// Get multiple tenants by IDs in a single RG round-trip.
    ///
    /// Issues one `list_groups` call with an `OData` filter
    /// `type eq '<TENANT_RG_TYPE_PATH>' and id in (id1, id2, …)`, then drains
    /// pagination if the result spans multiple pages. This replaces the
    /// previous per-id sequential hierarchy lookups used by
    /// `TenantResolverPluginClient::get_tenants`.
    ///
    /// Contract (from `TenantResolverPluginClient::get_tenants`):
    /// - IDs not present in RG are silently skipped — `list_groups` simply
    ///   does not include them in the response.
    /// - Duplicate IDs in `ids` are de-duplicated for efficiency; RG would
    ///   only return one row per id anyway, but we avoid sending duplicate
    ///   entries in the `OData` filter.
    /// - Output order is not guaranteed (the contract on the SDK trait
    ///   explicitly leaves ordering unspecified).
    /// - Status filtering is the caller's responsibility — applied in
    ///   `client.rs::get_tenants` against the returned `TenantInfo` list.
    pub(super) async fn resolve_tenants_batch(
        &self,
        ctx: &SecurityContext,
        ids: &[TenantId],
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        // Deduplicate input IDs. `HashSet` preserves uniqueness; order of the
        // resulting Vec is irrelevant for `id in (...)`.
        let unique_ids: Vec<uuid::Uuid> = ids
            .iter()
            .map(|id| id.0)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        let id_values: Vec<Expr> = unique_ids
            .iter()
            .map(|id| Expr::Value(Value::Uuid(*id)))
            .collect();

        let id_in_filter = Expr::In(
            Box::new(Expr::Identifier(GroupFilterField::Id.name().to_owned())),
            id_values,
        );

        let filter = tenant_type_filter_for_groups().and(id_in_filter);
        let base_query = ODataQuery::default().with_filter(filter);

        let items = self.drain_list_groups_pages(ctx, base_query).await?;

        Ok(items.iter().map(map_group_to_tenant_info).collect())
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
        let items = self
            .drain_hierarchy_pages(ctx, id.0, &TENANT_TYPE_FILTER_QUERY, Direction::Ancestors)
            .await?;

        // Split: depth=0 is the tenant, depth<0 are ancestors
        let tenant_group = items
            .iter()
            .find(|g| g.hierarchy.depth == 0)
            .ok_or(TenantResolverError::TenantNotFound { tenant_id: id })?;

        let tenant_ref = map_to_tenant_ref(tenant_group);

        // Ancestors ordered by depth ascending (direct parent first = depth -1, then -2, etc.)
        let mut ancestors: Vec<&ResourceGroupWithDepth> =
            items.iter().filter(|g| g.hierarchy.depth < 0).collect();
        ancestors.sort_by_key(|g| std::cmp::Reverse(g.hierarchy.depth)); // -1, -2, -3...

        let ancestor_refs: Vec<TenantRef> =
            ancestors.iter().map(|g| map_to_tenant_ref(g)).collect();

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
        let items = self
            .drain_hierarchy_pages(ctx, id.0, &TENANT_TYPE_FILTER_QUERY, Direction::Descendants)
            .await?;

        // Split: depth=0 is the tenant, depth>0 are descendants
        let tenant_group = items
            .iter()
            .find(|g| g.hierarchy.depth == 0)
            .ok_or(TenantResolverError::TenantNotFound { tenant_id: id })?;

        let tenant_ref = map_to_tenant_ref(tenant_group);

        let descendants: Vec<&ResourceGroupWithDepth> =
            items.iter().filter(|g| g.hierarchy.depth > 0).collect();

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
        // `is_ancestor` is non-reflexive; still surface TenantNotFound for a
        // bogus ID so the caller can distinguish "no such tenant" from
        // "exists but not an ancestor of itself".
        if ancestor_id == descendant_id {
            self.resolve_tenant(ctx, ancestor_id).await?;
            return Ok(false);
        }

        // `resolve_ancestors` already performs the barrier walk in
        // `filter_ancestors_by_barrier`: in `Respect` mode it returns an
        // empty chain when the descendant is self_managed and truncates at
        // (inclusive of) the first self_managed ancestor otherwise, so the
        // returned `ancestors` list is authoritative for the barrier mode.
        // A non-existent `ancestor_id` simply won't appear in the chain, so
        // the final `any` returns false without a dedicated existence check.
        // It also errors if `descendant_id` doesn't exist, surfacing
        // `TenantNotFound` for the question's subject.
        let (_, ancestors) = self
            .resolve_ancestors(ctx, descendant_id, barrier_mode)
            .await?;
        Ok(ancestors.iter().any(|a| a.id == ancestor_id))
    }

    // -- Internal helpers --

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

    /// Drain all pages from `list_groups` for the given query. Mirrors
    /// `drain_hierarchy_pages` but for the flat listing endpoint — no
    /// per-anchor `NotFound` mapping, since `list_groups` returns an empty
    /// page (not `NotFound`) when the filter matches no rows.
    async fn drain_list_groups_pages(
        &self,
        ctx: &SecurityContext,
        base_query: ODataQuery,
    ) -> Result<Vec<ResourceGroup>, TenantResolverError> {
        let mut all_items = Vec::new();
        let mut query = base_query;

        loop {
            let page = self
                .rg
                .list_groups(ctx, &query)
                .await
                .map_err(|e| TenantResolverError::Internal(e.to_string()))?;

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
#[allow(unknown_lints, de0309_must_have_domain_model)]
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
        tenant_type: Some(group.code.clone()),
        parent_id: group.hierarchy.parent_id.map(TenantId),
        self_managed: parse_self_managed_from_metadata(group.metadata.as_ref()),
    }
}

/// Same as `map_to_tenant_info` but sourced from a plain `ResourceGroup`
/// (no depth context) returned by `list_groups`. Used by
/// `resolve_tenants_batch`.
fn map_group_to_tenant_info(group: &ResourceGroup) -> TenantInfo {
    TenantInfo {
        id: TenantId(group.id),
        name: group.name.clone(),
        status: parse_status_from_metadata(group.metadata.as_ref()),
        tenant_type: Some(group.code.clone()),
        parent_id: group.hierarchy.parent_id.map(TenantId),
        self_managed: parse_self_managed_from_metadata(group.metadata.as_ref()),
    }
}

fn map_to_tenant_ref(group: &ResourceGroupWithDepth) -> TenantRef {
    TenantRef {
        id: TenantId(group.id),
        status: parse_status_from_metadata(group.metadata.as_ref()),
        tenant_type: Some(group.code.clone()),
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
    mut ancestors: Vec<TenantRef>,
    barrier_mode: BarrierMode,
) -> Vec<TenantRef> {
    if barrier_mode == BarrierMode::Ignore {
        return ancestors;
    }

    // If the starting tenant is self_managed, it cannot see its parent chain
    if tenant.self_managed {
        return Vec::new();
    }

    // Walk the parent chain; include each ancestor, stop after (inclusive) the
    // first self_managed one. Reuse the caller's Vec — truncate in place to
    // avoid allocating a parallel result buffer.
    if let Some(idx) = ancestors.iter().position(|a| a.self_managed) {
        ancestors.truncate(idx + 1);
    }
    ancestors
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
