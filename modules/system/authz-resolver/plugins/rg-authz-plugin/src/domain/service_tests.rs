use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::models::{Action, Resource, Subject};
use authz_resolver_sdk::{EvaluationRequest, EvaluationRequestContext, Predicate, TenantContext};
use modkit_odata::{ODataQuery, Page, PageInfo};
use modkit_security::SecurityContext;
use resource_group_sdk::api::ResourceGroupReadHierarchy;
use resource_group_sdk::error::ResourceGroupError;
use resource_group_sdk::models::{GroupHierarchyWithDepth, ResourceGroupWithDepth};
use uuid::Uuid;

use crate::domain::service::Service;

// -- Mock RG hierarchy --

struct MockRgHierarchy {
    descendants: Vec<ResourceGroupWithDepth>,
}

#[async_trait]
impl ResourceGroupReadHierarchy for MockRgHierarchy {
    async fn get_group_descendants(
        &self,
        _ctx: &SecurityContext,
        _group_id: Uuid,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError> {
        Ok(Page {
            items: self.descendants.clone(),
            page_info: PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: 25,
            },
        })
    }

    async fn get_group_ancestors(
        &self,
        _ctx: &SecurityContext,
        _group_id: Uuid,
        _query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError> {
        Ok(Page {
            items: vec![],
            page_info: PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: 25,
            },
        })
    }
}

fn make_group(
    id: Uuid,
    parent_id: Option<Uuid>,
    depth: i32,
    metadata: Option<serde_json::Value>,
) -> ResourceGroupWithDepth {
    ResourceGroupWithDepth {
        id,
        type_path: "gts.cf.core.rg.type.v1~y.core.tn.tenant.v1~".to_owned(),
        name: format!("T-{id}"),
        hierarchy: GroupHierarchyWithDepth {
            parent_id,
            tenant_id: id,
            depth,
        },
        metadata,
    }
}

fn make_request(tenant_id: Uuid) -> EvaluationRequest {
    EvaluationRequest {
        subject: Subject {
            id: Uuid::now_v7(),
            subject_type: None,
            properties: std::collections::HashMap::default(),
        },
        action: Action {
            name: "list".to_owned(),
        },
        resource: Resource {
            resource_type: "gts.cf.test.v1~".to_owned(),
            id: None,
            properties: std::collections::HashMap::default(),
        },
        context: EvaluationRequestContext {
            tenant_context: Some(TenantContext {
                root_id: Some(tenant_id),
                ..Default::default()
            }),
            token_scopes: vec![],
            require_constraints: true,
            capabilities: vec![],
            supported_properties: vec![],
            bearer_token: None,
        },
    }
}

fn make_request_no_tenant() -> EvaluationRequest {
    EvaluationRequest {
        subject: Subject {
            id: Uuid::now_v7(),
            subject_type: None,
            properties: std::collections::HashMap::default(),
        },
        action: Action {
            name: "list".to_owned(),
        },
        resource: Resource {
            resource_type: "gts.cf.test.v1~".to_owned(),
            id: None,
            properties: std::collections::HashMap::default(),
        },
        context: EvaluationRequestContext {
            tenant_context: None,
            token_scopes: vec![],
            require_constraints: false,
            capabilities: vec![],
            supported_properties: vec![],
            bearer_token: None,
        },
    }
}

// -- Tests --

#[tokio::test]
async fn tenant_subtree_resolved_to_in_predicate() {
    let t1 = Uuid::now_v7();
    let t2 = Uuid::now_v7();
    let mock = MockRgHierarchy {
        descendants: vec![
            make_group(t1, None, 0, None),
            make_group(t2, Some(t1), 1, None),
        ],
    };
    let svc = Service::new(Arc::new(mock));
    let resp = svc.evaluate(&make_request(t1)).await;

    assert!(resp.decision);
    assert_eq!(resp.context.constraints.len(), 1);
    let preds = &resp.context.constraints[0].predicates;
    assert_eq!(preds.len(), 1);
    assert!(
        matches!(&preds[0], Predicate::In(p) if p.property == "owner_tenant_id"),
        "expected In(owner_tenant_id), got: {preds:?}"
    );
}

#[tokio::test]
async fn barrier_tenant_and_descendants_excluded() {
    // T1 (root)
    // ├── T_normal
    // └── T_barrier (barrier=true)
    //     └── T_behind (child of barrier -- also excluded)
    let t1 = Uuid::now_v7();
    let t_normal = Uuid::now_v7();
    let t_barrier = Uuid::now_v7();
    let t_behind = Uuid::now_v7();
    let mock = MockRgHierarchy {
        descendants: vec![
            make_group(t1, None, 0, None),
            make_group(t_normal, Some(t1), 1, None),
            make_group(
                t_barrier,
                Some(t1),
                1,
                Some(serde_json::json!({"self_managed": true})),
            ),
            make_group(t_behind, Some(t_barrier), 2, None),
        ],
    };
    let svc = Service::new(Arc::new(mock));
    let resp = svc.evaluate(&make_request(t1)).await;

    assert!(resp.decision);
    let preds = &resp.context.constraints[0].predicates;
    if let Predicate::In(p) = &preds[0] {
        // Only T1 and T_normal should be visible.
        // T_barrier (barrier) and T_behind (behind barrier) excluded.
        assert_eq!(
            p.values.len(),
            2,
            "only t1 and t_normal should be visible, got: {:?}",
            p.values
        );
    } else {
        panic!("expected In predicate, got: {preds:?}");
    }
}

#[tokio::test]
async fn no_tenant_in_request_denies() {
    let mock = MockRgHierarchy {
        descendants: vec![],
    };
    let svc = Service::new(Arc::new(mock));
    let resp = svc.evaluate(&make_request_no_tenant()).await;
    assert!(!resp.decision, "no tenant -> deny");
}

#[tokio::test]
async fn nil_tenant_denies() {
    let mock = MockRgHierarchy {
        descendants: vec![],
    };
    let svc = Service::new(Arc::new(mock));
    let resp = svc.evaluate(&make_request(Uuid::default())).await;
    assert!(!resp.decision, "nil tenant -> deny");
}

#[tokio::test]
async fn empty_hierarchy_denies() {
    let tid = Uuid::now_v7();
    let mock = MockRgHierarchy {
        descendants: vec![],
    };
    let svc = Service::new(Arc::new(mock));
    let resp = svc.evaluate(&make_request(tid)).await;
    assert!(!resp.decision, "empty hierarchy -> deny");
}

#[tokio::test]
async fn rg_error_denies() {
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
    let resp = svc.evaluate(&make_request(Uuid::now_v7())).await;
    assert!(!resp.decision, "RG error -> deny (fail-closed)");
}
