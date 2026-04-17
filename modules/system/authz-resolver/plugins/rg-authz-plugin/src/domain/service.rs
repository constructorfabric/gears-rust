//! Service implementation for the RG `AuthZ` resolver plugin.
//!
//! Resolves tenant hierarchy via `ResourceGroupReadHierarchy` and produces
//! `InGroup` / `InGroupSubtree` predicates based on the caller's tenant scope.

use std::sync::Arc;

use authz_resolver_sdk::{
    Constraint, EvaluationRequest, EvaluationResponse, EvaluationResponseContext, InGroupPredicate,
    InGroupSubtreePredicate, InPredicate, Predicate,
};
use modkit_odata::ODataQuery;
use modkit_security::pep_properties;
use resource_group_sdk::api::ResourceGroupReadHierarchy;
use tracing::{debug, warn};
use uuid::Uuid;

/// RG-based `AuthZ` resolver service.
///
/// Resolves tenant hierarchy via `ResourceGroupReadHierarchy`:
/// 1. Extracts tenant root from request context
/// 2. Calls `get_group_descendants` to resolve the tenant subtree
/// 3. Filters barrier tenants (metadata.barrier = true) from scope
/// 4. Returns `In(owner_tenant_id, visible_tenants)` + optional `InGroup`/`InGroupSubtree`
pub struct Service {
    rg: Arc<dyn ResourceGroupReadHierarchy>,
}

impl Service {
    pub fn new(rg: Arc<dyn ResourceGroupReadHierarchy>) -> Self {
        Self { rg }
    }

    /// Evaluate an authorization request with RG hierarchy resolution.
    #[allow(clippy::cognitive_complexity)]
    pub async fn evaluate(&self, request: &EvaluationRequest) -> EvaluationResponse {
        let tenant_id = request
            .context
            .tenant_context
            .as_ref()
            .and_then(|t| t.root_id)
            .or_else(|| {
                request
                    .subject
                    .properties
                    .get("tenant_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
            });

        let Some(tid) = tenant_id else {
            debug!("No tenant resolvable -- deny");
            return Self::deny();
        };

        if tid == Uuid::default() {
            debug!("Nil UUID tenant -- deny");
            return Self::deny();
        }

        // Resolve tenant subtree via RG hierarchy
        let ctx = modkit_security::SecurityContext::anonymous();
        let visible_tenants = match self.resolve_tenant_subtree(&ctx, tid).await {
            Ok(tenants) => tenants,
            Err(e) => {
                warn!(error = %e, tenant_id = %tid, "Failed to resolve tenant hierarchy -- deny");
                return Self::deny();
            }
        };

        if visible_tenants.is_empty() {
            debug!(tenant_id = %tid, "Empty tenant subtree -- deny");
            return Self::deny();
        }

        // Build predicates
        let mut predicates = vec![Predicate::In(InPredicate::new(
            pep_properties::OWNER_TENANT_ID,
            visible_tenants,
        ))];

        // If request includes group context, add group predicates
        let props = &request.resource.properties;
        if let Some(group_ids) = props.get("group_ids")
            && let Some(ids) = Self::parse_uuid_array(group_ids)
            && !ids.is_empty()
        {
            predicates.push(Predicate::InGroup(InGroupPredicate::new("id", ids)));
        }
        if let Some(ancestor_ids) = props.get("ancestor_group_ids")
            && let Some(ids) = Self::parse_uuid_array(ancestor_ids)
            && !ids.is_empty()
        {
            predicates.push(Predicate::InGroupSubtree(InGroupSubtreePredicate::new(
                "id", ids,
            )));
        }

        EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint { predicates }],
                ..Default::default()
            },
        }
    }

    /// Resolve the tenant subtree: call `get_group_descendants` and filter barriers.
    ///
    /// A barrier group and ALL its descendants are excluded from the visible scope.
    /// This matches `tenant_closure` semantics: `barrier = 1` for any row where a
    /// barrier group is on the path between ancestor and descendant.
    async fn resolve_tenant_subtree(
        &self,
        ctx: &modkit_security::SecurityContext,
        tenant_id: Uuid,
    ) -> Result<Vec<Uuid>, String> {
        let query = ODataQuery::default();
        let page = self
            .rg
            .get_group_descendants(ctx, tenant_id, &query)
            .await
            .map_err(|e| format!("RG hierarchy error: {e}"))?;

        // Build lookup: id -> (parent_id, is_barrier)
        let groups: std::collections::HashMap<Uuid, (Option<Uuid>, bool)> = page
            .items
            .iter()
            .map(|g| (g.id, (g.hierarchy.parent_id, Self::is_barrier(g))))
            .collect();

        // Collect barrier IDs
        let barrier_ids: std::collections::HashSet<Uuid> = groups
            .iter()
            .filter(|(_, (_, is_b))| *is_b)
            .map(|(id, _)| *id)
            .collect();

        // For each group, walk up parent chain to check if any ancestor is a barrier.
        // If so, this group is "behind" the barrier and excluded.
        let visible: Vec<Uuid> = page
            .items
            .iter()
            .filter(|g| {
                if barrier_ids.contains(&g.id) {
                    return false; // barrier itself excluded
                }
                // Walk up parent chain within the result set
                let mut current = g.hierarchy.parent_id;
                while let Some(pid) = current {
                    if barrier_ids.contains(&pid) {
                        return false; // behind a barrier
                    }
                    current = groups.get(&pid).and_then(|(p, _)| *p);
                }
                true
            })
            .map(|g| g.id)
            .collect();

        debug!(
            tenant_id = %tenant_id,
            total = page.items.len(),
            barriers = barrier_ids.len(),
            visible = visible.len(),
            "Resolved tenant subtree"
        );

        Ok(visible)
    }

    /// Check if a group has barrier metadata.
    fn is_barrier(group: &resource_group_sdk::models::ResourceGroupWithDepth) -> bool {
        group
            .metadata
            .as_ref()
            .and_then(|m| m.get("barrier"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    fn deny() -> EvaluationResponse {
        EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext::default(),
        }
    }

    fn parse_uuid_array(value: &serde_json::Value) -> Option<Vec<Uuid>> {
        value.as_array().map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().and_then(|s| Uuid::parse_str(s).ok()))
                .collect()
        })
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod service_tests;
