use std::sync::Arc;

use async_trait::async_trait;
use modkit_odata::{ODataQuery, Page, PageInfo};
use modkit_security::SecurityContext;
use resource_group_sdk::TENANT_RG_TYPE_PATH;
use resource_group_sdk::api::ResourceGroupReadHierarchy;
use resource_group_sdk::error::ResourceGroupError;
use resource_group_sdk::models::{GroupHierarchyWithDepth, ResourceGroupWithDepth};
use tenant_resolver_sdk::{
    BarrierMode, GetAncestorsOptions, GetDescendantsOptions, GetTenantsOptions, IsAncestorOptions,
    TenantId, TenantResolverError, TenantResolverPluginClient, TenantStatus,
};
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
    ) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError> {
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
    ) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError> {
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
    let t1 = Uuid::now_v7();
    let mock = MockRgHierarchy::ancestors_only(vec![make_group(
        t1,
        "Active",
        None,
        0,
        Some(serde_json::json!({"status": "active"})),
    )]);
    let svc = service_with(mock);

    // Duplicate IDs should be deduplicated
    let result = svc
        .get_tenants(
            &ctx(),
            &[TenantId(t1), TenantId(t1)],
            &GetTenantsOptions {
                status: vec![TenantStatus::Active],
            },
        )
        .await
        .unwrap();
    assert_eq!(result.len(), 1);
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
        ) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError> {
            Err(ResourceGroupError::internal())
        }

        async fn get_group_ancestors(
            &self,
            _ctx: &SecurityContext,
            _group_id: Uuid,
            _query: &ODataQuery,
        ) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError> {
            Err(ResourceGroupError::internal())
        }
    }

    let svc = Service::new(Arc::new(FailingRg));
    let err = svc.get_tenant(&ctx(), TenantId(Uuid::now_v7())).await;
    assert!(
        matches!(err, Err(TenantResolverError::Internal(_))),
        "RG error should map to Internal"
    );
}
