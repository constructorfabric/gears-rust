// Created: 2026-04-16 by Constructor Tech
// Updated: 2026-04-29 by Constructor Tech

use std::sync::Arc;

use async_trait::async_trait;
use resource_group_sdk::TENANT_RG_TYPE_PATH;
use resource_group_sdk::api::ResourceGroupReadHierarchy;
use resource_group_sdk::models::{
    GroupHierarchy, GroupHierarchyWithDepth, ResourceGroup, ResourceGroupMembership,
    ResourceGroupWithDepth,
};
use tenant_resolver_sdk::{
    BarrierMode, GetAncestorsOptions, GetDescendantsOptions, GetTenantsOptions, IsAncestorOptions,
    TenantId, TenantResolverError, TenantResolverPluginClient, TenantStatus,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::ast::{CompareOperator, Expr, Value};
use toolkit_odata::{ODataQuery, Page, PageInfo};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::service::Service;

// -- Mock RG hierarchy --

struct MockRgHierarchy {
    /// Default items returned for any `get_group_ancestors` call (legacy).
    ancestors: Vec<ResourceGroupWithDepth>,
    /// Default items returned for any `get_group_descendants` call (legacy).
    descendants: Vec<ResourceGroupWithDepth>,
    /// Optional per-`group_id` dispatch for `get_group_ancestors`.
    ancestors_by_id: std::collections::HashMap<Uuid, Vec<ResourceGroupWithDepth>>,
    /// Optional per-`group_id` dispatch for `get_group_descendants`.
    descendants_by_id: std::collections::HashMap<Uuid, Vec<ResourceGroupWithDepth>>,
}

impl MockRgHierarchy {
    fn descendants_only(descendants: Vec<ResourceGroupWithDepth>) -> Self {
        Self {
            ancestors: vec![],
            descendants,
            ancestors_by_id: std::collections::HashMap::new(),
            descendants_by_id: std::collections::HashMap::new(),
        }
    }

    fn ancestors_only(ancestors: Vec<ResourceGroupWithDepth>) -> Self {
        Self {
            ancestors,
            descendants: vec![],
            ancestors_by_id: std::collections::HashMap::new(),
            descendants_by_id: std::collections::HashMap::new(),
        }
    }

    /// Build a mock whose `get_group_ancestors` dispatches by `group_id`.
    /// Use this when a test exercises multiple distinct queries that need
    /// to return different ancestor chains — prevents existence checks from
    /// passing by accident when the mock would otherwise return the same
    /// list regardless of which `group_id` was asked for.
    fn ancestors_by_id(map: std::collections::HashMap<Uuid, Vec<ResourceGroupWithDepth>>) -> Self {
        Self {
            ancestors: vec![],
            descendants: vec![],
            ancestors_by_id: map,
            descendants_by_id: std::collections::HashMap::new(),
        }
    }
}

#[async_trait]
impl ResourceGroupReadHierarchy for MockRgHierarchy {
    async fn get_group_descendants(
        &self,
        _ctx: &SecurityContext,
        group_id: Uuid,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, CanonicalError> {
        // Precedence: if per-id dispatch is configured, honor it strictly —
        // unknown ids return an empty page (simulates "group not found").
        // Otherwise fall back to the flat list. Real RG returns the subtree
        // anchored at `group_id`, so depth=0 is the group itself.
        let items = if self.descendants_by_id.is_empty() {
            self.descendants.clone()
        } else {
            self.descendants_by_id
                .get(&group_id)
                .cloned()
                .unwrap_or_default()
        };
        Ok(Page {
            items,
            page_info: PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: 100,
            },
        })
    }

    async fn get_group_ancestors(
        &self,
        _ctx: &SecurityContext,
        group_id: Uuid,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, CanonicalError> {
        // See `get_group_descendants` for precedence rules.
        let items = if self.ancestors_by_id.is_empty() {
            self.ancestors.clone()
        } else {
            self.ancestors_by_id
                .get(&group_id)
                .cloned()
                .unwrap_or_default()
        };
        Ok(Page {
            items,
            page_info: PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: 100,
            },
        })
    }

    async fn list_groups(
        &self,
        _ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroup>, CanonicalError> {
        // Flatten every `ResourceGroupWithDepth` known to the mock (ancestors
        // + descendants + per-id dispatches) into `ResourceGroup` entries,
        // deduplicated by id, then apply the `OData $filter` predicate so
        // tests cannot pass merely because the mock returned everything.
        // See `group_matches_filter` for the predicate subset honoured.
        let mut seen = std::collections::HashSet::new();
        let mut items: Vec<ResourceGroup> = Vec::new();
        let filter_expr = query.filter();
        let flatten = |src: &[ResourceGroupWithDepth],
                       seen: &mut std::collections::HashSet<Uuid>,
                       items: &mut Vec<ResourceGroup>| {
            for g in src {
                if filter_expr.is_some_and(|e| !group_matches_filter(g, e)) {
                    continue;
                }
                if seen.insert(g.id) {
                    items.push(ResourceGroup {
                        id: g.id,
                        code: g.code.clone(),
                        name: g.name.clone(),
                        hierarchy: GroupHierarchy {
                            parent_id: g.hierarchy.parent_id,
                            tenant_id: g.hierarchy.tenant_id,
                        },
                        metadata: g.metadata.clone(),
                    });
                }
            }
        };
        flatten(&self.ancestors, &mut seen, &mut items);
        flatten(&self.descendants, &mut seen, &mut items);
        for v in self.ancestors_by_id.values() {
            flatten(v, &mut seen, &mut items);
        }
        for v in self.descendants_by_id.values() {
            flatten(v, &mut seen, &mut items);
        }
        Ok(Page {
            items,
            page_info: PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: 100,
            },
        })
    }

    async fn get_group(
        &self,
        _ctx: &SecurityContext,
        _id: Uuid,
    ) -> Result<ResourceGroup, CanonicalError> {
        unimplemented!("MockRgHierarchy models hierarchy reads only")
    }

    async fn list_memberships(
        &self,
        _ctx: &SecurityContext,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupMembership>, CanonicalError> {
        unimplemented!("MockRgHierarchy models hierarchy reads only")
    }
}

/// Lightweight `OData $filter` evaluator for the mock `list_groups`.
///
/// Honours the predicate subset the rg-tr-plugin service actually emits *or*
/// that downstream callers (e.g. account-management's soft-delete precondition
/// from PR #1746) may emit against `list_groups`:
///
///   * `id eq <uuid>` / `id ne <uuid>` / `id in (<uuid>, …)`
///   * `hierarchy/parent_id eq <uuid>` / `... ne <uuid>` / `... in (…)`
///   * `tenant_id eq <uuid>` / `... ne <uuid>` / `... in (…)`
///   * `And` / `Or` / `Not` over the above
///
/// `tenant_id` is a `Uuid` field (never `None`) on every group, so its
/// `eq`/`ne`/`in` semantics use the value directly rather than the
/// `Option`-aware path used for `hierarchy/parent_id`.
///
/// Predicates on identifiers the mock does not understand (e.g. `type eq …`,
/// `name eq …`) are treated as `true` — the mock has only one type-fixture
/// per test, so type filtering would be a no-op anyway. This keeps the
/// evaluator small while still catching regressions where the service stops
/// passing the `id`-style predicates that batch reads depend on.
fn group_matches_filter(g: &ResourceGroupWithDepth, expr: &Expr) -> bool {
    match expr {
        Expr::And(l, r) => group_matches_filter(g, l) && group_matches_filter(g, r),
        Expr::Or(l, r) => group_matches_filter(g, l) || group_matches_filter(g, r),
        Expr::Not(inner) => !group_matches_filter(g, inner),
        Expr::Compare(lhs, op, rhs) => match (lhs.as_ref(), rhs.as_ref()) {
            (Expr::Identifier(name), Expr::Value(Value::Uuid(u))) => {
                let actual: Option<Uuid> = match name.as_str() {
                    "id" => Some(g.id),
                    "hierarchy/parent_id" => g.hierarchy.parent_id,
                    "tenant_id" => Some(g.hierarchy.tenant_id),
                    _ => return true, // unknown identifier — treat as no-op
                };
                match op {
                    CompareOperator::Eq => actual == Some(*u),
                    CompareOperator::Ne => actual != Some(*u),
                    _ => true, // ordering ops are not used on UUIDs
                }
            }
            _ => true,
        },
        Expr::In(lhs, values) => {
            let Expr::Identifier(name) = lhs.as_ref() else {
                return true;
            };
            let actual: Option<Uuid> = match name.as_str() {
                "id" => Some(g.id),
                "hierarchy/parent_id" => g.hierarchy.parent_id,
                "tenant_id" => Some(g.hierarchy.tenant_id),
                _ => return true,
            };
            let Some(actual) = actual else { return false };
            values
                .iter()
                .any(|v| matches!(v, Expr::Value(Value::Uuid(u)) if *u == actual))
        }
        // Other AST shapes (Function, bare Identifier/Value) are not produced
        // by the rg-tr-plugin service today; treat them as pass-through to
        // keep the mock minimal but forward-compatible.
        _ => true,
    }
}

fn make_group(
    id: Uuid,
    name: &str,
    parent_id: Option<Uuid>,
    depth: i32,
    metadata: Option<serde_json::Value>,
) -> ResourceGroupWithDepth {
    ResourceGroupWithDepth {
        id,
        code: TENANT_RG_TYPE_PATH.to_owned(),
        name: name.to_owned(),
        hierarchy: GroupHierarchyWithDepth {
            parent_id,
            tenant_id: id,
            depth,
        },
        metadata,
    }
}

fn service_with(mock: MockRgHierarchy) -> Service {
    Service::new(Arc::new(mock))
}

fn ctx() -> SecurityContext {
    SecurityContext::anonymous()
}

// -- get_tenant tests --

#[tokio::test]
async fn get_tenant_returns_info() {
    let t1 = Uuid::now_v7();
    let mock = MockRgHierarchy::ancestors_only(vec![make_group(t1, "Root", None, 0, None)]);
    let svc = service_with(mock);

    let tenant = svc.get_tenant(&ctx(), TenantId(t1)).await.unwrap();
    assert_eq!(tenant.id, TenantId(t1));
    assert_eq!(tenant.name, "Root");
    assert_eq!(tenant.status, TenantStatus::Active);
    assert!(!tenant.self_managed);
    assert_eq!(tenant.tenant_type, Some(TENANT_RG_TYPE_PATH.to_owned()));
}

#[tokio::test]
async fn get_tenant_with_metadata() {
    let t1 = Uuid::now_v7();
    let mock = MockRgHierarchy::ancestors_only(vec![make_group(
        t1,
        "Suspended",
        None,
        0,
        Some(serde_json::json!({"status": "suspended", "self_managed": true})),
    )]);
    let svc = service_with(mock);

    let tenant = svc.get_tenant(&ctx(), TenantId(t1)).await.unwrap();
    assert_eq!(tenant.status, TenantStatus::Suspended);
    assert!(tenant.self_managed);
}

#[tokio::test]
async fn get_tenant_not_found() {
    let mock = MockRgHierarchy::ancestors_only(vec![]);
    let svc = service_with(mock);

    let err = svc.get_tenant(&ctx(), TenantId(Uuid::now_v7())).await;
    assert!(matches!(
        err,
        Err(TenantResolverError::TenantNotFound { .. })
    ));
}

// -- get_tenants tests --

#[tokio::test]
async fn get_tenants_deduplicates_and_filters_status() {
    // Three tenants in storage:
    //   - `t_active`    — requested, Active    → must be in the result
    //   - `t_suspended` — requested, Suspended → must be filtered by status
    //   - `t_other`     — NOT requested, Active → must be filtered by ID
    // The input list contains `t_active` twice to verify input-dedup.
    //
    // The mock's `list_groups` honours the OData `id in (...)` filter (see
    // `group_matches_filter`), so `t_other` is excluded by the mock unless
    // the service stops emitting that filter, and `t_suspended` is excluded
    // by the service-side status filter on top.
    let t_active = Uuid::now_v7();
    let t_suspended = Uuid::now_v7();
    let t_other = Uuid::now_v7();
    let mock = MockRgHierarchy::ancestors_only(vec![
        make_group(
            t_active,
            "Active",
            None,
            0,
            Some(serde_json::json!({"status": "active"})),
        ),
        make_group(
            t_suspended,
            "Suspended",
            None,
            0,
            Some(serde_json::json!({"status": "suspended"})),
        ),
        make_group(
            t_other,
            "Other",
            None,
            0,
            Some(serde_json::json!({"status": "active"})),
        ),
    ]);
    let svc = service_with(mock);

    let result = svc
        .get_tenants(
            &ctx(),
            &[
                TenantId(t_active),
                TenantId(t_active),
                TenantId(t_suspended),
            ],
            &GetTenantsOptions {
                status: vec![TenantStatus::Active],
            },
        )
        .await
        .unwrap();

    assert_eq!(
        result.len(),
        1,
        "expected exactly one tenant after status filter + dedup"
    );
    assert_eq!(result[0].id, TenantId(t_active));
    assert_eq!(result[0].status, TenantStatus::Active);
    assert!(
        !result.iter().any(|t| t.id == TenantId(t_suspended)),
        "Suspended tenant must be excluded by status filter"
    );
    assert!(
        !result.iter().any(|t| t.id == TenantId(t_other)),
        "Tenant not present in the request must not leak through, even when stored"
    );
}

#[tokio::test]
async fn get_tenants_skips_not_found() {
    let mock = MockRgHierarchy::ancestors_only(vec![]);
    let svc = service_with(mock);

    let result = svc
        .get_tenants(
            &ctx(),
            &[TenantId(Uuid::now_v7())],
            &GetTenantsOptions::default(),
        )
        .await
        .unwrap();
    assert!(result.is_empty());
}

// -- get_ancestors tests --

#[tokio::test]
async fn get_ancestors_returns_parent_chain() {
    let root = Uuid::now_v7();
    let child = Uuid::now_v7();
    let mock = MockRgHierarchy::ancestors_only(vec![
        make_group(child, "Child", Some(root), 0, None),
        make_group(root, "Root", None, -1, None),
    ]);
    let svc = service_with(mock);

    let resp = svc
        .get_ancestors(&ctx(), TenantId(child), &GetAncestorsOptions::default())
        .await
        .unwrap();

    assert_eq!(resp.tenant.id, TenantId(child));
    assert_eq!(resp.ancestors.len(), 1);
    assert_eq!(resp.ancestors[0].id, TenantId(root));
}

#[tokio::test]
async fn get_ancestors_barrier_on_self_returns_empty() {
    let root = Uuid::now_v7();
    let barrier_child = Uuid::now_v7();
    let mock = MockRgHierarchy::ancestors_only(vec![
        make_group(
            barrier_child,
            "Barrier",
            Some(root),
            0,
            Some(serde_json::json!({"self_managed": true})),
        ),
        make_group(root, "Root", None, -1, None),
    ]);
    let svc = service_with(mock);

    let resp = svc
        .get_ancestors(
            &ctx(),
            TenantId(barrier_child),
            &GetAncestorsOptions {
                barrier_mode: BarrierMode::Respect,
            },
        )
        .await
        .unwrap();

    assert_eq!(resp.tenant.id, TenantId(barrier_child));
    assert!(
        resp.ancestors.is_empty(),
        "self_managed tenant should have no visible ancestors"
    );
}

#[tokio::test]
async fn get_ancestors_barrier_on_ancestor_stops_traversal() {
    let root = Uuid::now_v7();
    let barrier = Uuid::now_v7();
    let child = Uuid::now_v7();
    let mock = MockRgHierarchy::ancestors_only(vec![
        make_group(child, "Child", Some(barrier), 0, None),
        make_group(
            barrier,
            "Barrier",
            Some(root),
            -1,
            Some(serde_json::json!({"self_managed": true})),
        ),
        make_group(root, "Root", None, -2, None),
    ]);
    let svc = service_with(mock);

    let resp = svc
        .get_ancestors(
            &ctx(),
            TenantId(child),
            &GetAncestorsOptions {
                barrier_mode: BarrierMode::Respect,
            },
        )
        .await
        .unwrap();

    // Should include barrier but not root (stopped at barrier)
    assert_eq!(resp.ancestors.len(), 1);
    assert_eq!(resp.ancestors[0].id, TenantId(barrier));
}

#[tokio::test]
async fn get_ancestors_ignore_barrier_returns_all() {
    let root = Uuid::now_v7();
    let barrier = Uuid::now_v7();
    let child = Uuid::now_v7();
    let mock = MockRgHierarchy::ancestors_only(vec![
        make_group(child, "Child", Some(barrier), 0, None),
        make_group(
            barrier,
            "Barrier",
            Some(root),
            -1,
            Some(serde_json::json!({"self_managed": true})),
        ),
        make_group(root, "Root", None, -2, None),
    ]);
    let svc = service_with(mock);

    let resp = svc
        .get_ancestors(
            &ctx(),
            TenantId(child),
            &GetAncestorsOptions {
                barrier_mode: BarrierMode::Ignore,
            },
        )
        .await
        .unwrap();

    assert_eq!(resp.ancestors.len(), 2);
}

// -- get_descendants tests --

#[tokio::test]
async fn get_descendants_returns_subtree() {
    let root = Uuid::now_v7();
    let c1 = Uuid::now_v7();
    let c2 = Uuid::now_v7();
    let mock = MockRgHierarchy::descendants_only(vec![
        make_group(root, "Root", None, 0, None),
        make_group(c1, "C1", Some(root), 1, None),
        make_group(c2, "C2", Some(root), 1, None),
    ]);
    let svc = service_with(mock);

    let resp = svc
        .get_descendants(&ctx(), TenantId(root), &GetDescendantsOptions::default())
        .await
        .unwrap();

    assert_eq!(resp.tenant.id, TenantId(root));
    assert_eq!(resp.descendants.len(), 2);
}

#[tokio::test]
async fn get_descendants_barrier_excludes_subtree() {
    let root = Uuid::now_v7();
    let normal = Uuid::now_v7();
    let barrier = Uuid::now_v7();
    let behind = Uuid::now_v7();
    let mock = MockRgHierarchy::descendants_only(vec![
        make_group(root, "Root", None, 0, None),
        make_group(normal, "Normal", Some(root), 1, None),
        make_group(
            barrier,
            "Barrier",
            Some(root),
            1,
            Some(serde_json::json!({"self_managed": true})),
        ),
        make_group(behind, "Behind", Some(barrier), 2, None),
    ]);
    let svc = service_with(mock);

    let resp = svc
        .get_descendants(
            &ctx(),
            TenantId(root),
            &GetDescendantsOptions {
                barrier_mode: BarrierMode::Respect,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // Only normal should be visible; barrier + behind excluded
    assert_eq!(resp.descendants.len(), 1);
    assert_eq!(resp.descendants[0].id, TenantId(normal));
}

#[tokio::test]
async fn get_descendants_status_filter() {
    let root = Uuid::now_v7();
    let active = Uuid::now_v7();
    let suspended = Uuid::now_v7();
    let mock = MockRgHierarchy::descendants_only(vec![
        make_group(root, "Root", None, 0, None),
        make_group(
            active,
            "Active",
            Some(root),
            1,
            Some(serde_json::json!({"status": "active"})),
        ),
        make_group(
            suspended,
            "Suspended",
            Some(root),
            1,
            Some(serde_json::json!({"status": "suspended"})),
        ),
    ]);
    let svc = service_with(mock);

    let resp = svc
        .get_descendants(
            &ctx(),
            TenantId(root),
            &GetDescendantsOptions {
                status: vec![TenantStatus::Active],
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(resp.descendants.len(), 1);
    assert_eq!(resp.descendants[0].id, TenantId(active));
}

#[tokio::test]
async fn get_descendants_max_depth() {
    let root = Uuid::now_v7();
    let child = Uuid::now_v7();
    let grandchild = Uuid::now_v7();
    let mock = MockRgHierarchy::descendants_only(vec![
        make_group(root, "Root", None, 0, None),
        make_group(child, "Child", Some(root), 1, None),
        make_group(grandchild, "Grandchild", Some(child), 2, None),
    ]);
    let svc = service_with(mock);

    let resp = svc
        .get_descendants(
            &ctx(),
            TenantId(root),
            &GetDescendantsOptions {
                max_depth: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // Only direct children (depth=1), not grandchild (depth=2)
    assert_eq!(resp.descendants.len(), 1);
    assert_eq!(resp.descendants[0].id, TenantId(child));
}

// -- is_ancestor tests --

#[tokio::test]
async fn is_ancestor_self_returns_false() {
    let t1 = Uuid::now_v7();
    let mock = MockRgHierarchy::ancestors_only(vec![make_group(t1, "T1", None, 0, None)]);
    let svc = service_with(mock);

    let result = svc
        .is_ancestor(
            &ctx(),
            TenantId(t1),
            TenantId(t1),
            &IsAncestorOptions::default(),
        )
        .await
        .unwrap();
    assert!(!result, "self is not an ancestor of self");
}

#[tokio::test]
async fn is_ancestor_true_for_parent() {
    let root = Uuid::now_v7();
    let child = Uuid::now_v7();
    // `is_ancestor` calls `resolve_tenant(root)` first, then
    // `resolve_ancestors(child)`. Each query must see its own distinct
    // ancestor chain — otherwise the first call would pass by returning
    // child's depth=0 row as if it were root's. Dispatch by group_id.
    let mock = MockRgHierarchy::ancestors_by_id(std::collections::HashMap::from([
        (root, vec![make_group(root, "Root", None, 0, None)]),
        (
            child,
            vec![
                make_group(child, "Child", Some(root), 0, None),
                make_group(root, "Root", None, -1, None),
            ],
        ),
    ]));
    let svc = service_with(mock);

    let result = svc
        .is_ancestor(
            &ctx(),
            TenantId(root),
            TenantId(child),
            &IsAncestorOptions::default(),
        )
        .await
        .unwrap();
    assert!(result);
}

#[tokio::test]
async fn is_ancestor_barrier_descendant_returns_false() {
    let root = Uuid::now_v7();
    let barrier_child = Uuid::now_v7();
    // Same dispatch requirement as `is_ancestor_true_for_parent`: the
    // existence check for `root` must resolve against root's own ancestor
    // chain, not the barrier child's.
    let mock = MockRgHierarchy::ancestors_by_id(std::collections::HashMap::from([
        (root, vec![make_group(root, "Root", None, 0, None)]),
        (
            barrier_child,
            vec![
                make_group(
                    barrier_child,
                    "Barrier",
                    Some(root),
                    0,
                    Some(serde_json::json!({"self_managed": true})),
                ),
                make_group(root, "Root", None, -1, None),
            ],
        ),
    ]));
    let svc = service_with(mock);

    let result = svc
        .is_ancestor(
            &ctx(),
            TenantId(root),
            TenantId(barrier_child),
            &IsAncestorOptions {
                barrier_mode: BarrierMode::Respect,
            },
        )
        .await
        .unwrap();
    assert!(!result, "barrier descendant blocks ancestor claim");
}

// -- RG error handling --

#[tokio::test]
async fn rg_error_propagates() {
    struct FailingRg;

    #[async_trait]
    impl ResourceGroupReadHierarchy for FailingRg {
        async fn get_group_descendants(
            &self,
            _ctx: &SecurityContext,
            _group_id: Uuid,
            _query: &ODataQuery,
        ) -> Result<Page<ResourceGroupWithDepth>, CanonicalError> {
            Err(CanonicalError::internal("rg backend error").create())
        }

        async fn get_group_ancestors(
            &self,
            _ctx: &SecurityContext,
            _group_id: Uuid,
            _query: &ODataQuery,
        ) -> Result<Page<ResourceGroupWithDepth>, CanonicalError> {
            Err(CanonicalError::internal("rg backend error").create())
        }

        async fn list_groups(
            &self,
            _ctx: &SecurityContext,
            _query: &ODataQuery,
        ) -> Result<Page<ResourceGroup>, CanonicalError> {
            Err(CanonicalError::internal("rg backend error").create())
        }

        async fn get_group(
            &self,
            _ctx: &SecurityContext,
            _id: Uuid,
        ) -> Result<ResourceGroup, CanonicalError> {
            Err(CanonicalError::internal("rg backend error").create())
        }

        async fn list_memberships(
            &self,
            _ctx: &SecurityContext,
            _query: &ODataQuery,
        ) -> Result<Page<ResourceGroupMembership>, CanonicalError> {
            Err(CanonicalError::internal("rg backend error").create())
        }
    }

    let svc = Service::new(Arc::new(FailingRg));
    let err = svc.get_tenant(&ctx(), TenantId(Uuid::now_v7())).await;
    assert!(
        matches!(err, Err(TenantResolverError::Internal(_))),
        "RG error should map to Internal"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Unit tests for `group_matches_filter` — the predicate evaluator the
// mock `list_groups` uses to honour OData filters. Pure logic, sync.
// ─────────────────────────────────────────────────────────────────────

mod group_matches_filter_tests {
    use super::*;

    /// Convenience: build the canonical group with explicit `id`, `parent_id`,
    /// `tenant_id` so each test case can assert which axis matched.
    fn g(id: Uuid, parent_id: Option<Uuid>, tenant_id: Uuid) -> ResourceGroupWithDepth {
        ResourceGroupWithDepth {
            id,
            code: TENANT_RG_TYPE_PATH.to_owned(),
            name: "g".to_owned(),
            hierarchy: GroupHierarchyWithDepth {
                parent_id,
                tenant_id,
                depth: 0,
            },
            metadata: None,
        }
    }

    fn id_expr(name: &str) -> Expr {
        Expr::Identifier(name.to_owned())
    }

    fn uuid_value(u: Uuid) -> Expr {
        Expr::Value(Value::Uuid(u))
    }

    fn cmp(lhs: Expr, op: CompareOperator, rhs: Expr) -> Expr {
        Expr::Compare(Box::new(lhs), op, Box::new(rhs))
    }

    fn in_(lhs: Expr, vs: Vec<Expr>) -> Expr {
        Expr::In(Box::new(lhs), vs)
    }

    // ── tenant_id ─────────────────────────────────────────────────────

    #[test]
    fn tenant_id_eq_matches_owning_tenant() {
        let t = Uuid::now_v7();
        let other = Uuid::now_v7();
        let group = g(Uuid::now_v7(), None, t);
        assert!(group_matches_filter(
            &group,
            &cmp(id_expr("tenant_id"), CompareOperator::Eq, uuid_value(t))
        ));
        assert!(!group_matches_filter(
            &group,
            &cmp(id_expr("tenant_id"), CompareOperator::Eq, uuid_value(other))
        ));
    }

    #[test]
    fn tenant_id_ne_excludes_owning_tenant() {
        let t = Uuid::now_v7();
        let other = Uuid::now_v7();
        let group = g(Uuid::now_v7(), None, t);
        assert!(!group_matches_filter(
            &group,
            &cmp(id_expr("tenant_id"), CompareOperator::Ne, uuid_value(t))
        ));
        assert!(group_matches_filter(
            &group,
            &cmp(id_expr("tenant_id"), CompareOperator::Ne, uuid_value(other))
        ));
    }

    #[test]
    fn tenant_id_in_matches_when_owning_tenant_is_listed() {
        let t = Uuid::now_v7();
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let group = g(Uuid::now_v7(), None, t);
        assert!(group_matches_filter(
            &group,
            &in_(id_expr("tenant_id"), vec![uuid_value(t), uuid_value(a)])
        ));
        assert!(!group_matches_filter(
            &group,
            &in_(id_expr("tenant_id"), vec![uuid_value(a), uuid_value(b)])
        ));
    }

    // ── id ────────────────────────────────────────────────────────────

    #[test]
    fn id_eq_matches_self_only() {
        let id = Uuid::now_v7();
        let other = Uuid::now_v7();
        let group = g(id, None, Uuid::now_v7());
        assert!(group_matches_filter(
            &group,
            &cmp(id_expr("id"), CompareOperator::Eq, uuid_value(id))
        ));
        assert!(!group_matches_filter(
            &group,
            &cmp(id_expr("id"), CompareOperator::Eq, uuid_value(other))
        ));
    }

    #[test]
    fn id_in_matches_when_self_is_listed() {
        let id = Uuid::now_v7();
        let other = Uuid::now_v7();
        let group = g(id, None, Uuid::now_v7());
        assert!(group_matches_filter(
            &group,
            &in_(id_expr("id"), vec![uuid_value(id), uuid_value(other)])
        ));
        assert!(!group_matches_filter(
            &group,
            &in_(id_expr("id"), vec![uuid_value(other)])
        ));
    }

    // ── hierarchy/parent_id ───────────────────────────────────────────

    #[test]
    fn parent_id_eq_matches_set_parent() {
        let p = Uuid::now_v7();
        let other = Uuid::now_v7();
        let group = g(Uuid::now_v7(), Some(p), Uuid::now_v7());
        assert!(group_matches_filter(
            &group,
            &cmp(
                id_expr("hierarchy/parent_id"),
                CompareOperator::Eq,
                uuid_value(p)
            )
        ));
        assert!(!group_matches_filter(
            &group,
            &cmp(
                id_expr("hierarchy/parent_id"),
                CompareOperator::Eq,
                uuid_value(other)
            )
        ));
    }

    #[test]
    fn parent_id_in_excludes_root_group_with_none_parent() {
        // Root groups have parent_id = None; any non-empty `parent_id in (...)`
        // should not match — the `in` arm short-circuits to `false` when the
        // resolved Option is `None`.
        let p = Uuid::now_v7();
        let root = g(Uuid::now_v7(), None, Uuid::now_v7());
        assert!(!group_matches_filter(
            &root,
            &in_(id_expr("hierarchy/parent_id"), vec![uuid_value(p)])
        ));
    }

    // ── unknown identifier (pass-through) ─────────────────────────────

    #[test]
    fn unknown_identifier_eq_passes_through() {
        // The mock has only one type fixture per test, so `type eq …` and
        // `name eq …` are intentionally treated as no-ops (return true).
        let group = g(Uuid::now_v7(), None, Uuid::now_v7());
        assert!(group_matches_filter(
            &group,
            &cmp(
                id_expr("name"),
                CompareOperator::Eq,
                Expr::Value(Value::String("any".to_owned()))
            )
        ));
        assert!(group_matches_filter(
            &group,
            &cmp(
                id_expr("type"),
                CompareOperator::Eq,
                Expr::Value(Value::String("anything".to_owned()))
            )
        ));
    }

    // ── combinators: And / Or / Not ───────────────────────────────────

    #[test]
    fn and_combines_tenant_id_and_id() {
        let t = Uuid::now_v7();
        let id = Uuid::now_v7();
        let other_t = Uuid::now_v7();
        let group = g(id, None, t);

        // (tenant_id eq t) AND (id eq id) → match
        let both_match = Expr::And(
            Box::new(cmp(
                id_expr("tenant_id"),
                CompareOperator::Eq,
                uuid_value(t),
            )),
            Box::new(cmp(id_expr("id"), CompareOperator::Eq, uuid_value(id))),
        );
        assert!(group_matches_filter(&group, &both_match));

        // (tenant_id eq other_t) AND (id eq id) → no match (tenant_id fails)
        let one_fails = Expr::And(
            Box::new(cmp(
                id_expr("tenant_id"),
                CompareOperator::Eq,
                uuid_value(other_t),
            )),
            Box::new(cmp(id_expr("id"), CompareOperator::Eq, uuid_value(id))),
        );
        assert!(!group_matches_filter(&group, &one_fails));
    }

    #[test]
    fn or_matches_when_either_branch_matches() {
        let t = Uuid::now_v7();
        let id = Uuid::now_v7();
        let unrelated = Uuid::now_v7();
        let group = g(id, None, t);

        // (tenant_id eq unrelated) OR (id eq id) → match (id arm)
        let or_expr = Expr::Or(
            Box::new(cmp(
                id_expr("tenant_id"),
                CompareOperator::Eq,
                uuid_value(unrelated),
            )),
            Box::new(cmp(id_expr("id"), CompareOperator::Eq, uuid_value(id))),
        );
        assert!(group_matches_filter(&group, &or_expr));

        // (tenant_id eq unrelated) OR (id eq unrelated) → no match
        let neither = Expr::Or(
            Box::new(cmp(
                id_expr("tenant_id"),
                CompareOperator::Eq,
                uuid_value(unrelated),
            )),
            Box::new(cmp(
                id_expr("id"),
                CompareOperator::Eq,
                uuid_value(unrelated),
            )),
        );
        assert!(!group_matches_filter(&group, &neither));
    }

    #[test]
    fn not_inverts_the_inner_predicate() {
        let t = Uuid::now_v7();
        let group = g(Uuid::now_v7(), None, t);

        let not_match = Expr::Not(Box::new(cmp(
            id_expr("tenant_id"),
            CompareOperator::Eq,
            uuid_value(t),
        )));
        assert!(!group_matches_filter(&group, &not_match));

        let not_other = Expr::Not(Box::new(cmp(
            id_expr("tenant_id"),
            CompareOperator::Eq,
            uuid_value(Uuid::now_v7()),
        )));
        assert!(group_matches_filter(&group, &not_other));
    }
}

// ─────────────────────────────────────────────────────────────────────
// Integration test for tenant_id filter through the trait surface.
// Verifies the mock `list_groups` actually scopes results when callers
// (e.g. account-management's soft-delete precondition from PR #1746)
// build `ODataQuery::with_filter(tenant_id eq <uuid>)`.
// ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn list_groups_honours_tenant_id_eq_filter() {
    use resource_group_sdk::api::ResourceGroupReadHierarchy;
    use toolkit_security::SecurityContext;

    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let group_a = ResourceGroupWithDepth {
        hierarchy: GroupHierarchyWithDepth {
            parent_id: None,
            tenant_id: tenant_a,
            depth: 0,
        },
        ..make_group(Uuid::now_v7(), "InA", None, 0, None)
    };
    let group_b = ResourceGroupWithDepth {
        hierarchy: GroupHierarchyWithDepth {
            parent_id: None,
            tenant_id: tenant_b,
            depth: 0,
        },
        ..make_group(Uuid::now_v7(), "InB", None, 0, None)
    };
    let mock = MockRgHierarchy::ancestors_only(vec![group_a.clone(), group_b.clone()]);

    // tenant_id eq tenant_a → only group_a is returned.
    let query = ODataQuery::default().with_filter(Expr::Compare(
        Box::new(Expr::Identifier("tenant_id".to_owned())),
        CompareOperator::Eq,
        Box::new(Expr::Value(Value::Uuid(tenant_a))),
    ));
    let page = mock
        .list_groups(&SecurityContext::anonymous(), &query)
        .await
        .unwrap();
    assert_eq!(
        page.items.len(),
        1,
        "only one group should match tenant_id eq tenant_a"
    );
    assert_eq!(page.items[0].id, group_a.id);

    // tenant_id in (tenant_b) → only group_b.
    let query_in = ODataQuery::default().with_filter(Expr::In(
        Box::new(Expr::Identifier("tenant_id".to_owned())),
        vec![Expr::Value(Value::Uuid(tenant_b))],
    ));
    let page = mock
        .list_groups(&SecurityContext::anonymous(), &query_in)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, group_b.id);

    // No filter → both groups (sanity check, ensures the new arm is the
    // only thing gating results).
    let page = mock
        .list_groups(&SecurityContext::anonymous(), &ODataQuery::default())
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
}
