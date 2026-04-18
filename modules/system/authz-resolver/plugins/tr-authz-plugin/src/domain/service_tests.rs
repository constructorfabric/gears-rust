use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::models::{Action, Resource, Subject};
use authz_resolver_sdk::{EvaluationRequest, EvaluationRequestContext, Predicate, TenantContext};
use modkit_security::SecurityContext;
use tenant_resolver_sdk::{
    GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions, GetDescendantsResponse,
    GetTenantsOptions, IsAncestorOptions, TenantId, TenantInfo, TenantRef, TenantResolverClient,
    TenantResolverError, TenantStatus,
};
use uuid::Uuid;

use crate::domain::service::Service;

// -- Mock Tenant Resolver --

struct MockTenantResolver {
    descendants: Vec<TenantRef>,
    root: Option<TenantRef>,
}

impl MockTenantResolver {
    fn with_tenants(root_id: Uuid, descendant_ids: Vec<Uuid>) -> Self {
        let root = TenantRef {
            id: TenantId(root_id),
            status: TenantStatus::Active,
            tenant_type: None,
            parent_id: None,
            self_managed: false,
        };
        let descendants = descendant_ids
            .into_iter()
            .map(|id| TenantRef {
                id: TenantId(id),
                status: TenantStatus::Active,
                tenant_type: None,
                parent_id: Some(TenantId(root_id)),
                self_managed: false,
            })
            .collect();
        Self {
            descendants,
            root: Some(root),
        }
    }

    fn empty() -> Self {
        Self {
            descendants: vec![],
            root: None,
        }
    }
}

#[async_trait]
impl TenantResolverClient for MockTenantResolver {
    async fn get_tenant(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        if self.root.as_ref().is_some_and(|r| r.id == id) {
            Ok(TenantInfo {
                id,
                name: format!("T-{}", id.0),
                status: TenantStatus::Active,
                tenant_type: None,
                parent_id: None,
                self_managed: false,
            })
        } else {
            Err(TenantResolverError::TenantNotFound { tenant_id: id })
        }
    }

    async fn get_tenants(
        &self,
        _ctx: &SecurityContext,
        _ids: &[TenantId],
        _options: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        Ok(vec![])
    }

    async fn get_ancestors(
        &self,
        _ctx: &SecurityContext,
        _id: TenantId,
        _options: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        unimplemented!("not used by tr-authz-plugin")
    }

    async fn get_descendants(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
        _options: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        match &self.root {
            Some(root) if root.id == id => Ok(GetDescendantsResponse {
                tenant: root.clone(),
                descendants: self.descendants.clone(),
            }),
            _ => Err(TenantResolverError::TenantNotFound { tenant_id: id }),
        }
    }

    async fn is_ancestor(
        &self,
        _ctx: &SecurityContext,
        _ancestor_id: TenantId,
        _descendant_id: TenantId,
        _options: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        unimplemented!("not used by tr-authz-plugin")
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
    let mock = MockTenantResolver::with_tenants(t1, vec![t2]);
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
    // Should have both t1 (root) and t2 (descendant)
    if let Predicate::In(p) = &preds[0] {
        assert_eq!(p.values.len(), 2, "root + 1 descendant");
    }
}

#[tokio::test]
async fn barrier_handled_by_tr_only_visible_returned() {
    // TR returns only visible tenants (barrier filtering done internally).
    // If TR returns root + 1 normal child, authz sees exactly those.
    let t1 = Uuid::now_v7();
    let t_normal = Uuid::now_v7();
    // Mock: TR already filtered out barrier + behind-barrier
    let mock = MockTenantResolver::with_tenants(t1, vec![t_normal]);
    let svc = Service::new(Arc::new(mock));
    let resp = svc.evaluate(&make_request(t1)).await;

    assert!(resp.decision);
    let preds = &resp.context.constraints[0].predicates;
    if let Predicate::In(p) = &preds[0] {
        assert_eq!(
            p.values.len(),
            2,
            "only t1 and t_normal visible (barrier excluded by TR)"
        );
    } else {
        panic!("expected In predicate");
    }
}

#[tokio::test]
async fn no_tenant_in_request_denies() {
    let mock = MockTenantResolver::empty();
    let svc = Service::new(Arc::new(mock));
    let resp = svc.evaluate(&make_request_no_tenant()).await;
    assert!(!resp.decision, "no tenant -> deny");
}

#[tokio::test]
async fn nil_tenant_denies() {
    let mock = MockTenantResolver::empty();
    let svc = Service::new(Arc::new(mock));
    let resp = svc.evaluate(&make_request(Uuid::default())).await;
    assert!(!resp.decision, "nil tenant -> deny");
}

#[tokio::test]
async fn tenant_not_found_denies() {
    let mock = MockTenantResolver::empty();
    let svc = Service::new(Arc::new(mock));
    let resp = svc.evaluate(&make_request(Uuid::now_v7())).await;
    assert!(!resp.decision, "tenant not found -> deny (fail-closed)");
}

#[tokio::test]
async fn tr_error_denies() {
    struct FailingTr;

    #[async_trait]
    impl TenantResolverClient for FailingTr {
        async fn get_tenant(
            &self,
            _ctx: &SecurityContext,
            id: TenantId,
        ) -> Result<TenantInfo, TenantResolverError> {
            Err(TenantResolverError::Internal(format!("fail {id}")))
        }

        async fn get_tenants(
            &self,
            _ctx: &SecurityContext,
            _ids: &[TenantId],
            _options: &GetTenantsOptions,
        ) -> Result<Vec<TenantInfo>, TenantResolverError> {
            Err(TenantResolverError::Internal("fail".to_owned()))
        }

        async fn get_ancestors(
            &self,
            _ctx: &SecurityContext,
            _id: TenantId,
            _options: &GetAncestorsOptions,
        ) -> Result<GetAncestorsResponse, TenantResolverError> {
            Err(TenantResolverError::Internal("fail".to_owned()))
        }

        async fn get_descendants(
            &self,
            _ctx: &SecurityContext,
            _id: TenantId,
            _options: &GetDescendantsOptions,
        ) -> Result<GetDescendantsResponse, TenantResolverError> {
            Err(TenantResolverError::Internal("fail".to_owned()))
        }

        async fn is_ancestor(
            &self,
            _ctx: &SecurityContext,
            _a: TenantId,
            _d: TenantId,
            _options: &IsAncestorOptions,
        ) -> Result<bool, TenantResolverError> {
            Err(TenantResolverError::Internal("fail".to_owned()))
        }
    }

    let svc = Service::new(Arc::new(FailingTr));
    let resp = svc.evaluate(&make_request(Uuid::now_v7())).await;
    assert!(!resp.decision, "TR error -> deny (fail-closed)");
}

#[tokio::test]
async fn root_tenant_provisioning_allows_without_hierarchy() {
    let mock = MockTenantResolver::empty();
    let svc = Service::new(Arc::new(mock));

    let tid = Uuid::now_v7();
    let mut req = make_request(tid);
    req.action.name = "create".to_owned();
    req.resource
        .properties
        .insert("is_tenant".to_owned(), serde_json::Value::Bool(true));
    // No parent_id -> root tenant provisioning

    let resp = svc.evaluate(&req).await;
    assert!(resp.decision, "root tenant provisioning -> allow");
    let preds = &resp.context.constraints[0].predicates;
    if let Predicate::In(p) = &preds[0] {
        assert_eq!(p.values.len(), 1, "single tenant scope");
        assert_eq!(p.values[0], serde_json::Value::String(tid.to_string()));
    }
}

#[tokio::test]
async fn group_predicates_from_request_properties() {
    let t1 = Uuid::now_v7();
    let mock = MockTenantResolver::with_tenants(t1, vec![]);
    let svc = Service::new(Arc::new(mock));

    let g1 = Uuid::now_v7();
    let g2 = Uuid::now_v7();
    let mut req = make_request(t1);
    req.resource.properties.insert(
        "group_ids".to_owned(),
        serde_json::json!([g1.to_string(), g2.to_string()]),
    );
    req.resource.properties.insert(
        "ancestor_group_ids".to_owned(),
        serde_json::json!([g1.to_string()]),
    );

    let resp = svc.evaluate(&req).await;
    assert!(resp.decision);

    let preds = &resp.context.constraints[0].predicates;
    assert_eq!(preds.len(), 3, "In + InGroup + InGroupSubtree");
    assert!(matches!(&preds[0], Predicate::In(_)));
    assert!(matches!(&preds[1], Predicate::InGroup(_)));
    assert!(matches!(&preds[2], Predicate::InGroupSubtree(_)));
}
