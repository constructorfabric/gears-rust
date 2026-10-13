// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-testing-rest-api:p2
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]
//! API-level tests using `Router::oneshot` pattern.
//!
//! Verifies HTTP-level behavior: status codes, response shapes,
//! `OData` query parsing, and RFC 9457 error format.

mod common;

use std::sync::Arc;
use toolkit_gts::{gts_id, gts_uri};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

use authz_resolver_sdk::{
    AuthZResolverApi, EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
    PolicyEnforcer,
    constraints::{Constraint, InPredicate, InTenantSubtreePredicate, Predicate},
};
use sea_orm_migration::MigratorTrait;
use toolkit::api::OpenApiRegistry;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit::api::operation_builder::OperationSpec;
use toolkit_db::{
    ConnectOpts, DBProvider, DbError, connect_db, migration_runner::run_migrations_for_testing,
};
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};

use resource_group::domain::group_service::{GroupService, QueryProfile};
use resource_group::domain::membership_service::MembershipService;
use resource_group::infra::storage::group_repo::GroupRepository;
use resource_group::infra::storage::membership_repo::MembershipRepository;
use resource_group::infra::storage::migrations::Migrator;

macro_rules! rg_type_id {
    ($($arg:tt)*) => {
        format!("{}{}", gts_id!("cf.core.rg.type.v1~"), format_args!($($arg)*))
    };
}

const ALREADY_EXISTS_TYPE: &str = gts_uri!("cf.core.errors.err.v1~cf.core.err.already_exists.v1~");

// ── Noop OpenAPI Registry for tests ─────────────────────────────────────

struct NoopOpenApiRegistry;

impl OpenApiRegistry for NoopOpenApiRegistry {
    fn register_operation(&self, _spec: &OperationSpec) {}

    fn ensure_schema_raw(
        &self,
        name: &str,
        _schemas: Vec<(
            String,
            utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
        )>,
    ) -> String {
        name.to_owned()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ── Mock AuthZ: allow-all with tenant scoping ───────────────────────────

struct AllowAllAuthZ;

#[async_trait]
impl AuthZResolverApi for AllowAllAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let tenant_id = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or(Uuid::nil());

        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [tenant_id],
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
}

// ── Test setup ──────────────────────────────────────────────────────────

async fn test_db() -> Arc<DBProvider<DbError>> {
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .expect("connect to in-memory SQLite");

    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run migrations");

    Arc::new(DBProvider::new(db))
}

fn make_ctx(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant_id)
        .build()
        .expect("valid SecurityContext")
}

fn make_enforcer() -> PolicyEnforcer {
    let authz: Arc<dyn AuthZResolverApi> = Arc::new(AllowAllAuthZ);
    PolicyEnforcer::new(authz)
}

/// Nonconforming PDP: permits, but the sole constraint is an
/// `InTenantSubtree` predicate — a capability this gear never advertises.
/// The PEP rejects it fail-closed at compile time; see
/// `create_group_target_tenant_test.rs` for the domain-level pin. Here it
/// drives the HTTP-level wire-shape assertion.
struct InTenantSubtreeOnlyAuthZ;

#[async_trait]
impl AuthZResolverApi for InTenantSubtreeOnlyAuthZ {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let tenant_id = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or(Uuid::nil());

        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::InTenantSubtree(InTenantSubtreePredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        tenant_id,
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
}

async fn build_test_router() -> (Router, common::TestTypes) {
    build_test_router_with_enforcer(make_enforcer()).await
}

async fn build_test_router_with_enforcer(enforcer: PolicyEnforcer) -> (Router, common::TestTypes) {
    let db = test_db().await;
    let (router, types, _, _) = build_shared_router_with_parts(db, enforcer);
    (router, types)
}

fn json_request(
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
    tenant_id: Uuid,
) -> Request<Body> {
    let ctx = make_ctx(tenant_id);
    let mut builder = Request::builder().method(method).uri(uri);

    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }

    let body = match body {
        Some(json) => Body::from(serde_json::to_vec(&json).unwrap()),
        None => Body::empty(),
    };

    let mut req = builder.body(body).unwrap();
    req.extensions_mut().insert(ctx);
    req
}

async fn response_body(resp: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or_default()
}

// ── Type CRUD Tests ─────────────────────────────────────────────────────

// ── Group CRUD Tests ────────────────────────────────────────────────────

#[tokio::test]
async fn create_group_returns_201() {
    let (router, types) = build_test_router().await;
    let tenant_id = Uuid::now_v7();
    let type_code = common::unique_rg_code("grp");

    types.add_rg_type(&type_code, true, &[], &[], None);

    let req = json_request(
        "POST",
        "/resource-group/v1/groups",
        Some(serde_json::json!({
            "type": type_code,
            "name": "Test Group"
        })),
        tenant_id,
    );

    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = response_body(resp).await;
    assert_eq!(body["name"], "Test Group");
    assert!(body["id"].is_string());
    assert_eq!(body["hierarchy"]["tenant_id"], tenant_id.to_string());
}

#[tokio::test]
async fn list_groups_returns_200() {
    let (router, _) = build_test_router().await;
    let tenant_id = Uuid::now_v7();

    let req = json_request("GET", "/resource-group/v1/groups", None, tenant_id);
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = response_body(resp).await;
    assert!(body["items"].is_array());
}

#[tokio::test]
async fn get_group_not_found_returns_404() {
    let (router, _) = build_test_router().await;
    let tenant_id = Uuid::now_v7();
    let fake_id = Uuid::now_v7();

    let req = json_request(
        "GET",
        &format!("/resource-group/v1/groups/{fake_id}"),
        None,
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// POST group with an already-taken id returns 409 with a problem body.
#[tokio::test]
async fn create_group_duplicate_id_returns_409() {
    let (router, types) = build_test_router().await;
    let tenant_id = Uuid::now_v7();
    let type_code = common::unique_rg_code("dupid");

    types.add_rg_type(&type_code, true, &[], &[], None);

    let id = Uuid::now_v7();
    let payload = |name: &str| {
        serde_json::json!({
            "id": id,
            "type": type_code,
            "name": name
        })
    };

    let req = json_request(
        "POST",
        "/resource-group/v1/groups",
        Some(payload("First")),
        tenant_id,
    );
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let req = json_request(
        "POST",
        "/resource-group/v1/groups",
        Some(payload("Second")),
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    let body = response_body(resp).await;
    assert_eq!(body["type"], ALREADY_EXISTS_TYPE, "problem type: {body}");
    assert_eq!(body["status"], 409, "problem status: {body}");
    assert_eq!(
        body["context"]["resource_name"],
        id.to_string(),
        "problem resource_name: {body}"
    );
}

// ── Error format tests (RFC 9457 Problem Details) ───────────────────────

#[tokio::test]
async fn error_response_has_problem_fields() {
    let (router, _) = build_test_router().await;
    let tenant_id = Uuid::now_v7();

    // Trigger a validation error
    let req = json_request(
        "POST",
        "/resource-group/v1/groups",
        Some(serde_json::json!({
            "type": "invalid",
            "name": "Invalid Type"
        })),
        tenant_id,
    );

    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = response_body(resp).await;
    // RFC 9457 requires these fields
    assert!(
        body["title"].is_string(),
        "Problem must have 'title': {body}"
    );
    assert!(
        body["status"].is_number(),
        "Problem must have 'status': {body}"
    );
    assert!(
        body["detail"].is_string(),
        "Problem must have 'detail': {body}"
    );
}

// ── Helper: assert no surrogate IDs in JSON ──────────────────────────────

fn assert_no_surrogate_ids(json: &serde_json::Value) {
    let text = json.to_string();
    assert!(
        !text.contains("\"gts_type_id\""),
        "Response should not contain gts_type_id: {text}"
    );
    assert!(
        !text.contains("\"type_id\""),
        "Response should not contain type_id: {text}"
    );
    assert!(
        !text.contains("\"parent_type_id\""),
        "Response should not contain parent_type_id: {text}"
    );
    assert!(
        !text.contains("\"schema_id\""),
        "Response should not contain schema_id: {text}"
    );
}

// =========================================================================
// Section A: REST API endpoint tests (TC-REST-01..08)
// =========================================================================

/// TC-REST-03: POST membership returns 201.
#[tokio::test]
async fn rest_post_membership_returns_201() {
    let (router, types, group_svc, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);

    let mt_code = common::unique_rg_code("mt2");
    types.add_rg_type(&mt_code, true, &[], &[], None);

    let gt_code = common::unique_rg_code("gt2");
    types.add_rg_type(&gt_code, true, &[], &[&mt_code], None);

    let group = group_svc
        .create_group(
            &ctx,
            resource_group_sdk::CreateGroupRequest::new(gt_code, "G1".to_owned()),
            tenant_id,
        )
        .await
        .unwrap();

    let mt_encoded = mt_code.replace('~', "%7E");
    let req = json_request(
        "POST",
        &format!(
            "/resource-group/v1/memberships/{}/{}/res-001",
            group.id, mt_encoded
        ),
        None,
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = response_body(resp).await;
    assert_eq!(body["resource_type"], mt_code);
    assert!(
        body.get("tenant_id").is_none(),
        "No tenant_id in membership response"
    );
    assert_no_surrogate_ids(&body);
}

/// Helper: register a root type that allows itself as a parent.
fn create_self_ref_type(types: &common::TestTypes, suffix: &str) -> String {
    let code = common::unique_rg_code(suffix);
    types.add_rg_type(&code, true, &[&code], &[], None);
    code
}

/// Router and services sharing one database for HTTP fixtures.
type SharedRouter = (
    Router,
    common::TestTypes,
    Arc<GroupService<GroupRepository>>,
    Arc<MembershipService<GroupRepository, MembershipRepository>>,
);

/// Helper: build a fully-wired router with shared services for multi-request tests.
async fn build_shared_router() -> SharedRouter {
    build_shared_router_with_db(test_db().await)
}

/// Build services around an explicit database.
fn build_shared_router_with_db(db: Arc<DBProvider<DbError>>) -> SharedRouter {
    build_shared_router_with_parts(db, make_enforcer())
}

/// Build the services and the router around one database, one enforcer and
/// one Types Registry fixture (returned so tests can register types).
fn build_shared_router_with_parts(
    db: Arc<DBProvider<DbError>>,
    enforcer: PolicyEnforcer,
) -> SharedRouter {
    let types = common::TestTypes::new();
    let group_svc = Arc::new(GroupService::new(
        db.clone(),
        QueryProfile::default(),
        enforcer.clone(),
        Arc::new(GroupRepository),
        types.catalog(),
    ));
    let membership_svc = Arc::new(MembershipService::new(
        db,
        enforcer,
        Arc::new(GroupRepository),
        types.catalog(),
        Arc::new(MembershipRepository),
    ));
    let router = resource_group::api::rest::routes::register_routes(
        Router::new(),
        &NoopOpenApiRegistry,
        group_svc.clone(),
        membership_svc.clone(),
    );
    (router, types, group_svc, membership_svc)
}

/// TC-REST-04: DELETE membership returns 204.
#[tokio::test]
async fn rest_delete_membership_returns_204() {
    let (router, types, group_svc, membership_svc) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);

    let mt = common::unique_rg_code("mtr");
    types.add_rg_type(&mt, true, &[], &[], None);

    let gt = common::unique_rg_code("gtr");
    types.add_rg_type(&gt, true, &[], &[&mt], None);

    let group = group_svc
        .create_group(
            &ctx,
            resource_group_sdk::CreateGroupRequest::new(gt, "GDel".to_owned()),
            tenant_id,
        )
        .await
        .unwrap();

    membership_svc
        .add_membership(&ctx, group.id, &mt, "res-del")
        .await
        .unwrap();

    let mt_encoded = mt.replace('~', "%7E");
    let req = json_request(
        "DELETE",
        &format!(
            "/resource-group/v1/memberships/{}/{}/res-del",
            group.id, mt_encoded
        ),
        None,
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

/// TC-REST-05: GET memberships returns 200 with list.
#[tokio::test]
async fn rest_get_memberships_returns_200() {
    let (router, _, _, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();

    let req = json_request("GET", "/resource-group/v1/memberships", None, tenant_id);
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = response_body(resp).await;
    assert!(body["items"].is_array());
}

/// TC-REST-06: POST group with parent_id returns 201 with hierarchy.
#[tokio::test]
async fn rest_post_group_with_parent_returns_201() {
    let (router, types, group_svc, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);

    let root_type = common::unique_rg_code("rtp");
    // Create type first without self-reference, then update to allow self as parent
    types.add_rg_type(&root_type, true, &[&root_type], &[], None);

    let parent = group_svc
        .create_group(
            &ctx,
            resource_group_sdk::CreateGroupRequest::new(root_type.clone(), "Parent".to_owned()),
            tenant_id,
        )
        .await
        .unwrap();

    let req = json_request(
        "POST",
        "/resource-group/v1/groups",
        Some(serde_json::json!({
            "type": root_type,
            "name": "Child",
            "parent_id": parent.id.to_string()
        })),
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = response_body(resp).await;
    assert_eq!(body["hierarchy"]["parent_id"], parent.id.to_string());
    assert_no_surrogate_ids(&body);
}

/// TC-REST-07: DELETE group with force=true returns 204.
#[tokio::test]
async fn rest_delete_group_force_returns_204() {
    let (router, types, group_svc, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);

    let rt = create_self_ref_type(&types, "fdel");

    let parent = group_svc
        .create_group(
            &ctx,
            resource_group_sdk::CreateGroupRequest::new(rt.clone(), "FParent".to_owned()),
            tenant_id,
        )
        .await
        .unwrap();

    // Create child so normal delete would fail
    group_svc
        .create_group(
            &ctx,
            resource_group_sdk::CreateGroupRequest::new(rt, "FChild".to_owned())
                .with_parent_id(Some(parent.id)),
            tenant_id,
        )
        .await
        .unwrap();

    let req = json_request(
        "DELETE",
        &format!("/resource-group/v1/groups/{}?force=true", parent.id),
        None,
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

/// TC-REST-08: GET group hierarchy returns 200 with depth fields.
#[tokio::test]
async fn rest_get_group_hierarchy_returns_200() {
    let (router, types, group_svc, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);

    let rt = create_self_ref_type(&types, "hier");

    let root = group_svc
        .create_group(
            &ctx,
            resource_group_sdk::CreateGroupRequest::new(rt.clone(), "HRoot".to_owned()),
            tenant_id,
        )
        .await
        .unwrap();

    let child = group_svc
        .create_group(
            &ctx,
            resource_group_sdk::CreateGroupRequest::new(rt, "HChild".to_owned())
                .with_parent_id(Some(root.id)),
            tenant_id,
        )
        .await
        .unwrap();

    let req = json_request(
        "GET",
        &format!("/resource-group/v1/groups/{}/descendants", child.id),
        None,
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = response_body(resp).await;
    assert!(body["items"].is_array());
    let items = body["items"].as_array().unwrap();
    assert!(!items.is_empty());
    // Each item should have hierarchy.depth
    for item in items {
        assert!(
            item["hierarchy"]["depth"].is_number(),
            "hierarchy item should have depth: {item}"
        );
        assert!(
            item["type"].is_string(),
            "hierarchy item type should be string: {item}"
        );
        assert_no_surrogate_ids(item);
    }
}

// =========================================================================
// Section B: REST Metadata Tests (TC-META-19..22)
// =========================================================================

/// TC-META-20: POST group with metadata returns 201 with metadata.
#[tokio::test]
async fn rest_create_group_with_metadata() {
    let (router, types, _, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();

    let code = common::unique_rg_code("gm");
    types.add_rg_type(&code, true, &[], &[], None);

    let req = json_request(
        "POST",
        "/resource-group/v1/groups",
        Some(serde_json::json!({
            "type": code,
            "name": "MetaGroup",
            "metadata": {"self_managed": true}
        })),
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = response_body(resp).await;
    assert_eq!(body["metadata"]["self_managed"], true);
    assert_no_surrogate_ids(&body);
}

/// TC-META-21: Response omits metadata when null.
#[tokio::test]
async fn rest_group_response_omits_null_metadata() {
    let (router, types, _, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();

    let code = common::unique_rg_code("nm");
    types.add_rg_type(&code, true, &[], &[], None);

    let req = json_request(
        "POST",
        "/resource-group/v1/groups",
        Some(serde_json::json!({
            "type": code,
            "name": "NoMeta"
        })),
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = response_body(resp).await;
    assert!(
        body.get("metadata").is_none(),
        "Response should omit metadata when null: {body}"
    );
}

// =========================================================================
// Section C: Invalid/Non-GTS Input (TC-NOGTS + TC-DESER)
// =========================================================================

/// TC-NOGTS-04: Create group with non-RG type_path returns 400.
#[tokio::test]
async fn input_create_group_non_rg_type_returns_400() {
    let (router, _, _, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();

    let req = json_request(
        "POST",
        "/resource-group/v1/groups",
        Some(serde_json::json!({
            "type": gts_id!("cf.other.prefix.type.v1~test.rg.other.type.v1~"),
            "name": "BadGroup"
        })),
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    // Should be 400 (validation) or 404 (type not found)
    let status = resp.status();
    assert!(
        status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND,
        "Expected 400 or 404, got {status}"
    );
}

/// TC-NOGTS-05: Create group with empty type_path returns 400.
#[tokio::test]
async fn input_create_group_empty_type_returns_400() {
    let (router, _, _, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();

    let req = json_request(
        "POST",
        "/resource-group/v1/groups",
        Some(serde_json::json!({
            "type": "",
            "name": "EmptyType"
        })),
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND,
        "Expected 400 or 404, got {status}"
    );
}

/// TC-NOGTS-06: Membership with non-GTS resource_type returns 400 or 404.
#[tokio::test]
async fn input_membership_non_gts_resource_type() {
    let (router, types, group_svc, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);

    let rt = common::unique_rg_code("ngts");
    types.add_rg_type(&rt, true, &[], &[], None);

    let group = group_svc
        .create_group(
            &ctx,
            resource_group_sdk::CreateGroupRequest::new(rt, "NGGroup".to_owned()),
            tenant_id,
        )
        .await
        .unwrap();

    let req = json_request(
        "POST",
        &format!(
            "/resource-group/v1/memberships/{}/not-a-gts-path/res-001",
            group.id
        ),
        None,
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND,
        "Expected 400 or 404 for non-GTS resource_type, got {status}"
    );
}

/// TC-NOGTS-07: Membership with empty resource_type returns 404 or 400.
#[tokio::test]
async fn input_membership_empty_resource_type() {
    let (router, _, _, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();
    let fake_id = Uuid::now_v7();

    // Empty resource_type in the URL path -- axum routing may not match
    let req = json_request(
        "POST",
        &format!("/resource-group/v1/memberships/{fake_id}//res-001"),
        None,
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    // Empty path segment may cause 404 (no route match) or 400
    assert!(
        status == StatusCode::BAD_REQUEST
            || status == StatusCode::NOT_FOUND
            || status == StatusCode::METHOD_NOT_ALLOWED,
        "Expected 400/404/405 for empty resource_type, got {status}"
    );
}

/// TC-DESER-04: Create group missing `type` field returns 400/422.
#[tokio::test]
async fn input_deser_group_missing_type_returns_error() {
    let (router, _, _, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();

    let req = json_request(
        "POST",
        "/resource-group/v1/groups",
        Some(serde_json::json!({
            "name": "NoType"
        })),
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
        "Expected 400 or 422 for missing type, got {status}"
    );
}

/// TC-DESER-05: Create group with `parent_id: "not-a-uuid"` returns 400/422.
#[tokio::test]
async fn input_deser_group_invalid_parent_uuid_returns_error() {
    let (router, _, _, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();

    let req = json_request(
        "POST",
        "/resource-group/v1/groups",
        Some(serde_json::json!({
            "type": gts_id!("cf.core.rg.type.v1~test.rg.simple.type.v1~"),
            "name": "BadParent",
            "parent_id": "not-a-uuid"
        })),
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
        "Expected 400 or 422 for invalid parent_id, got {status}"
    );
}

/// TC-DESER-06: Malformed JSON body returns 400.
#[tokio::test]
async fn input_deser_malformed_json_returns_400() {
    let (router, _) = build_test_router().await;
    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);

    let mut req = Request::builder()
        .method("POST")
        .uri("/resource-group/v1/groups")
        .header("content-type", "application/json")
        .body(Body::from("{not valid json"))
        .unwrap();
    req.extensions_mut().insert(ctx);

    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// TC-DESER-07: Empty body when expected returns 400/422.
#[tokio::test]
async fn input_deser_empty_body_returns_error() {
    let (router, _) = build_test_router().await;
    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);

    let mut req = Request::builder()
        .method("POST")
        .uri("/resource-group/v1/groups")
        .header("content-type", "application/json")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(ctx);

    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
        "Expected 400 or 422 for empty body, got {status}"
    );
}

/// TC-DESER-08: Create group with empty name returns 400.
#[tokio::test]
async fn input_deser_group_empty_name_returns_400() {
    let (router, types, _, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();

    let code = common::unique_rg_code("en");
    types.add_rg_type(&code, true, &[], &[], None);

    let req = json_request(
        "POST",
        "/resource-group/v1/groups",
        Some(serde_json::json!({
            "type": code,
            "name": ""
        })),
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// TC-DESER-09: Group path with non-UUID group_id returns 400.
#[tokio::test]
async fn input_deser_group_path_non_uuid_returns_400() {
    let (router, _, _, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();

    let req = json_request(
        "GET",
        "/resource-group/v1/groups/not-a-uuid",
        None,
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// TC-DESER-10: Membership path with non-UUID group_id returns 400.
#[tokio::test]
async fn input_deser_membership_path_non_uuid_returns_400() {
    let (router, _, _, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();

    let req = json_request(
        "POST",
        &format!(
            "/resource-group/v1/memberships/not-a-uuid/{}/res-001",
            gts_id!("cf.core.rg.type.v1~test.rg.simple.type.v1~")
        ),
        None,
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// TC-DESER-11: Extra unknown fields in body are tolerated (verify behavior).
#[tokio::test]
async fn input_deser_extra_fields_behavior() {
    let (router, types) = build_test_router().await;
    let tenant_id = Uuid::now_v7();
    let code = common::unique_rg_code("extra");
    types.add_rg_type(&code, true, &[], &[], None);

    let req = json_request(
        "POST",
        "/resource-group/v1/groups",
        Some(serde_json::json!({
            "type": code,
            "name": "Extra Fields",
            "unknown_field": "should be ignored"
        })),
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    // Most Rust frameworks ignore extra fields by default (deny_unknown_fields not set)
    // or reject them. Either is valid.
    assert!(
        status == StatusCode::CREATED
            || status == StatusCode::BAD_REQUEST
            || status == StatusCode::UNPROCESSABLE_ENTITY,
        "Expected 201 (ignored) or 400/422 (denied), got {status}"
    );
}

// =========================================================================
// Section D: GTS URL Tilde + SMALLINT (TC-GTS-16..20, TC-ADR-17..20)
// =========================================================================

/// TC-GTS-16: Membership POST with %7E tilde encoding succeeds.
#[tokio::test]
async fn gts_membership_post_tilde_encoded() {
    let (router, types, group_svc, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);

    let mt = common::unique_rg_code("tmt");
    types.add_rg_type(&mt, true, &[], &[], None);

    let gt = common::unique_rg_code("tgt");
    types.add_rg_type(&gt, true, &[], &[&mt], None);

    let group = group_svc
        .create_group(
            &ctx,
            resource_group_sdk::CreateGroupRequest::new(gt, "TildeGroup".to_owned()),
            tenant_id,
        )
        .await
        .unwrap();

    let mt_encoded = mt.replace('~', "%7E");
    let req = json_request(
        "POST",
        &format!(
            "/resource-group/v1/memberships/{}/{}/res-tilde",
            group.id, mt_encoded
        ),
        None,
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
}

/// TC-GTS-17: Membership DELETE with %7E tilde encoding succeeds.
#[tokio::test]
async fn gts_membership_delete_tilde_encoded() {
    let (router, types, group_svc, membership_svc) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);

    let mt = common::unique_rg_code("tmd");
    types.add_rg_type(&mt, true, &[], &[], None);

    let gt = common::unique_rg_code("tgd");
    types.add_rg_type(&gt, true, &[], &[&mt], None);

    let group = group_svc
        .create_group(
            &ctx,
            resource_group_sdk::CreateGroupRequest::new(gt, "TildeDelGrp".to_owned()),
            tenant_id,
        )
        .await
        .unwrap();

    membership_svc
        .add_membership(&ctx, group.id, &mt, "res-tdel")
        .await
        .unwrap();

    let mt_encoded = mt.replace('~', "%7E");
    let req = json_request(
        "DELETE",
        &format!(
            "/resource-group/v1/memberships/{}/{}/res-tdel",
            group.id, mt_encoded
        ),
        None,
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

/// TC-ADR-18: Group response has no SMALLINT IDs -- `type` is string.
#[tokio::test]
async fn smallint_group_response_has_no_surrogate_ids() {
    let (router, types, _, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();

    let code = common::unique_rg_code("gsid");
    types.add_rg_type(&code, true, &[], &[], None);

    let req = json_request(
        "POST",
        "/resource-group/v1/groups",
        Some(serde_json::json!({
            "type": code,
            "name": "SIDGroup"
        })),
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = response_body(resp).await;
    assert_no_surrogate_ids(&body);
    assert!(body["type"].is_string());
    assert!(body["id"].is_string());
}

/// TC-ADR-19: Membership response has no SMALLINT IDs -- `resource_type` is string.
#[tokio::test]
async fn smallint_membership_response_has_no_surrogate_ids() {
    let (router, types, group_svc, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);

    let mt = common::unique_rg_code("msid");
    types.add_rg_type(&mt, true, &[], &[], None);

    let gt = common::unique_rg_code("gsidm");
    types.add_rg_type(&gt, true, &[], &[&mt], None);

    let group = group_svc
        .create_group(
            &ctx,
            resource_group_sdk::CreateGroupRequest::new(gt, "MSIDGrp".to_owned()),
            tenant_id,
        )
        .await
        .unwrap();

    let mt_encoded = mt.replace('~', "%7E");
    let req = json_request(
        "POST",
        &format!(
            "/resource-group/v1/memberships/{}/{}/res-sid",
            group.id, mt_encoded
        ),
        None,
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = response_body(resp).await;
    assert_no_surrogate_ids(&body);
    assert!(body["resource_type"].is_string());
    assert!(body.get("tenant_id").is_none());
}

/// TC-ADR-20: Hierarchy response has no SMALLINT IDs -- each `type` is string.
#[tokio::test]
async fn smallint_hierarchy_response_has_no_surrogate_ids() {
    let (router, types, group_svc, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);

    let rt = create_self_ref_type(&types, "hsid");

    let root = group_svc
        .create_group(
            &ctx,
            resource_group_sdk::CreateGroupRequest::new(rt.clone(), "HSIDRoot".to_owned()),
            tenant_id,
        )
        .await
        .unwrap();

    let child = group_svc
        .create_group(
            &ctx,
            resource_group_sdk::CreateGroupRequest::new(rt, "HSIDChild".to_owned())
                .with_parent_id(Some(root.id)),
            tenant_id,
        )
        .await
        .unwrap();

    let req = json_request(
        "GET",
        &format!("/resource-group/v1/groups/{}/descendants", child.id),
        None,
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = response_body(resp).await;
    for item in body["items"].as_array().unwrap() {
        assert!(item["type"].is_string());
        assert_no_surrogate_ids(item);
    }
}

// =========================================================================
// Section G: Error response HTTP mapping (TC-REST-10)
// =========================================================================

/// TC-REST-10: Error responses have correct HTTP status and Content-Type.
#[tokio::test]
async fn rest_error_responses_have_problem_content_type_and_status() {
    let (router, types, _, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();

    // --- 404 Not Found: GET nonexistent group ---
    let fake_id = Uuid::now_v7();
    let req = json_request(
        "GET",
        &format!("/resource-group/v1/groups/{fake_id}"),
        None,
        tenant_id,
    );
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "expected 404");
    let ct = resp
        .headers()
        .get("content-type")
        .expect("Content-Type header must be present");
    assert!(
        ct.to_str().unwrap().contains("application/problem+json"),
        "expected application/problem+json, got: {ct:?}"
    );
    let body = response_body(resp).await;
    assert_eq!(body["status"], 404);
    assert!(body["title"].is_string());
    assert!(body["detail"].is_string());
    assert!(body.get("stack").is_none(), "no stack trace leaked");
    assert!(body.get("trace").is_none(), "no trace leaked");
    assert!(body.get("backtrace").is_none(), "no backtrace leaked");

    // --- 409 Conflict: duplicate group id ---
    let code = common::unique_rg_code("errdup");
    types.add_rg_type(&code, true, &[], &[], None);
    let dup_id = Uuid::now_v7();
    let dup_payload = serde_json::json!({"id": dup_id, "type": code, "name": "Dup"});

    let req = json_request(
        "POST",
        "/resource-group/v1/groups",
        Some(dup_payload.clone()),
        tenant_id,
    );
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "first create");
    let req = json_request(
        "POST",
        "/resource-group/v1/groups",
        Some(dup_payload),
        tenant_id,
    );
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT, "expected 409");
    let ct = resp
        .headers()
        .get("content-type")
        .expect("Content-Type header must be present");
    assert!(
        ct.to_str().unwrap().contains("application/problem+json"),
        "expected application/problem+json for 409, got: {ct:?}"
    );
    let body = response_body(resp).await;
    assert_eq!(body["status"], 409);

    // --- 400 Bad Request: invalid type code ---
    let req = json_request(
        "POST",
        "/resource-group/v1/groups",
        Some(serde_json::json!({
            "type": "invalid",
            "name": "Invalid Type"
        })),
        tenant_id,
    );
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "expected 400");
    let ct = resp
        .headers()
        .get("content-type")
        .expect("Content-Type header must be present");
    assert!(
        ct.to_str().unwrap().contains("application/problem+json"),
        "expected application/problem+json for 400, got: {ct:?}"
    );
    let body = response_body(resp).await;
    assert_eq!(body["status"], 400);

    // --- 404: TypeNotFound when creating group with nonexistent type ---
    let req = json_request(
        "POST",
        "/resource-group/v1/groups",
        Some(serde_json::json!({
            "type": gts_id!("cf.core.rg.type.v1~test.rg.nonexistent.type.v1~"),
            "name": "Ghost"
        })),
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "expected 404 for TypeNotFound"
    );
    let ct = resp
        .headers()
        .get("content-type")
        .expect("Content-Type header must be present");
    assert!(
        ct.to_str().unwrap().contains("application/problem+json"),
        "expected application/problem+json for TypeNotFound, got: {ct:?}"
    );
    let body = response_body(resp).await;
    assert_eq!(body["status"], 404);
}

// =========================================================================
// Section H: Route registration smoke test (RG7)
// =========================================================================

/// RG7: All endpoints are registered and respond with non-405.
/// Uses HEAD/POST/DELETE with minimal bodies to exercise route matching
/// without needing valid data setup.
#[tokio::test]
async fn rest_route_smoke_all_endpoints_registered() {
    let (router, _, _, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();
    let fake_id = Uuid::now_v7();
    let fake_code = rg_type_id!("smoke.v1%7E");

    // (method, path, has_body?, description)
    let endpoints: Vec<(&str, String, bool, &str)> = vec![
        // Groups: 7 endpoints
        (
            "GET",
            "/resource-group/v1/groups".to_owned(),
            false,
            "list groups",
        ),
        (
            "POST",
            "/resource-group/v1/groups".to_owned(),
            true,
            "create group",
        ),
        (
            "GET",
            format!("/resource-group/v1/groups/{fake_id}"),
            false,
            "get group",
        ),
        (
            "PUT",
            format!("/resource-group/v1/groups/{fake_id}"),
            true,
            "update group",
        ),
        (
            "DELETE",
            format!("/resource-group/v1/groups/{fake_id}"),
            false,
            "delete group",
        ),
        (
            "GET",
            format!("/resource-group/v1/groups/{fake_id}/descendants"),
            false,
            "hierarchy",
        ),
        (
            "GET",
            format!("/resource-group/v1/groups/{fake_id}/ancestors"),
            false,
            "ancestors",
        ),
        // Memberships: 3 endpoints
        (
            "GET",
            "/resource-group/v1/memberships".to_owned(),
            false,
            "list memberships",
        ),
        (
            "POST",
            format!("/resource-group/v1/memberships/{fake_id}/{fake_code}/res-1"),
            false,
            "add membership",
        ),
        (
            "DELETE",
            format!("/resource-group/v1/memberships/{fake_id}/{fake_code}/res-1"),
            false,
            "remove membership",
        ),
    ];

    for (method, path, has_body, desc) in &endpoints {
        let body = if *has_body {
            Some(serde_json::json!({}))
        } else {
            None
        };
        let req = json_request(method, path, body, tenant_id);
        let resp = router.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        assert_ne!(
            status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {path} ({desc}) returned 405 -- route not registered"
        );
    }
}
// =========================================================================
// re-parent cycle detection (REST-level regression coverage)
// =========================================================================
//
// `move_group_internal_impl` (src/domain/group_service.rs) detects cycles via
// `is_descendant`; the resulting `DomainError::CycleDetected` is mapped to
// `RgError::failed_precondition()` with the `hierarchy` precondition subject
// (src/api/rest/error.rs). Service-level coverage already exists in
// `group_service_test.rs` (TC-GRP-06/07, via `move_group_unscoped`) and
// error-mapping coverage exists in `domain_unit_test.rs`
// (`domain_to_problem_cycle_detected_is_400`), but neither one drives the
// actual HTTP path. These two tests close that gap.
//
// There is no separate move endpoint: re-parenting stays part of the
// `UpdateGroupDto` payload (`parent_id`, required, `null` for root -- see
// its doc comment in `api/rest/dto.rs`), and `update_group_inner` delegates
// to the same move logic when `parent_id` changes (src/domain/group_service.rs,
// "Delegate to move logic"). So `PUT /resource-group/v1/groups/{id}` with a
// changed `parent_id` is how these tests reach cycle detection.

/// moving a group under its own descendant returns 400 with a
/// `hierarchy` precondition violation.
#[tokio::test]
async fn rest_move_group_under_descendant_returns_400_cycle() {
    let (router, types, group_svc, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);

    let rt = create_self_ref_type(&types, "cyc1");

    let root = group_svc
        .create_group(
            &ctx,
            resource_group_sdk::CreateGroupRequest::new(rt.clone(), "CycRoot".to_owned()),
            tenant_id,
        )
        .await
        .expect("create root group");

    let child = group_svc
        .create_group(
            &ctx,
            resource_group_sdk::CreateGroupRequest::new(rt, "CycChild".to_owned())
                .with_parent_id(Some(root.id)),
            tenant_id,
        )
        .await
        .expect("create child group");

    // Move root under its own child -- would create a cycle.
    let req = json_request(
        "PUT",
        &format!("/resource-group/v1/groups/{}", root.id),
        Some(serde_json::json!({
            "name": "CycRoot",
            "parent_id": child.id,
            "metadata": null,
        })),
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    let body = response_body(resp).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "expected 400, got {status}: {body}"
    );

    let violations = body["context"]["violations"]
        .as_array()
        .expect("context.violations must be present");
    assert!(
        !violations.is_empty(),
        "expected at least one precondition violation: {body}"
    );
    assert_eq!(violations[0]["subject"], "hierarchy");
    assert_eq!(violations[0]["type"], "STATE");
    assert!(
        violations[0]["description"]
            .as_str()
            .is_some_and(|d| !d.is_empty()),
        "expected non-empty violation description: {body}"
    );
}

/// moving a group under itself returns 400 with a `hierarchy`
/// precondition violation.
#[tokio::test]
async fn rest_move_group_self_parent_returns_400_cycle() {
    let (router, types, group_svc, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();
    let ctx = make_ctx(tenant_id);

    let rt = create_self_ref_type(&types, "cyc2");

    let root = group_svc
        .create_group(
            &ctx,
            resource_group_sdk::CreateGroupRequest::new(rt, "SelfRoot".to_owned()),
            tenant_id,
        )
        .await
        .expect("create root group");

    // Move root under itself.
    let req = json_request(
        "PUT",
        &format!("/resource-group/v1/groups/{}", root.id),
        Some(serde_json::json!({
            "name": "SelfRoot",
            "parent_id": root.id,
            "metadata": null,
        })),
        tenant_id,
    );
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    let body = response_body(resp).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "expected 400, got {status}: {body}"
    );

    let violations = body["context"]["violations"]
        .as_array()
        .expect("context.violations must be present");
    assert!(
        !violations.is_empty(),
        "expected at least one precondition violation: {body}"
    );
    assert_eq!(violations[0]["subject"], "hierarchy");
    assert_eq!(violations[0]["type"], "STATE");
    assert!(
        violations[0]["description"]
            .as_str()
            .is_some_and(|d| !d.is_empty()),
        "expected non-empty violation description: {body}"
    );
}

/// `DomainError::TenantNotFound` -> 404, with the foreign tenant id as the
/// problem's resource (`src/api/rest/error.rs` 56-59, currently uncovered).
///
/// Reached through `create_group`'s target-tenant path: `CreateGroupDto`
/// carries an optional `tenant_id` letting an authorized caller create a
/// group in a different tenant (see `tests/create_group_target_tenant_test.rs`
/// for the domain-level coverage of that branch and of `TenantNotFound`
/// itself). This router's `AllowAllAuthZ` mock compiles a scope constrained
/// to the caller's own tenant (an `In` predicate over the subject's
/// `tenant_id`), so a foreign target tenant is outside the compiled
/// `AccessScope` and `create_group` maps that to `TenantNotFound` -- the
/// same "we don't know this tenant" answer a real unknown tenant would get.
#[tokio::test]
async fn create_group_with_foreign_tenant_id_returns_404() {
    let (router, types) = build_test_router().await;
    let tenant_id = Uuid::now_v7();
    let foreign_tenant_id = Uuid::now_v7();
    let type_code = common::unique_rg_code("foreigntenant");

    types.add_rg_type(&type_code, true, &[], &[], None);

    let req = json_request(
        "POST",
        "/resource-group/v1/groups",
        Some(serde_json::json!({
            "type": type_code,
            "name": "Cross-tenant Group",
            "tenant_id": foreign_tenant_id,
        })),
        tenant_id,
    );

    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    let body = response_body(resp).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "expected 404, got {status}: {body}"
    );
    assert_eq!(
        body["context"]["resource_name"],
        foreign_tenant_id.to_string(),
        "problem resource_name should name the foreign tenant: {body}"
    );
}

/// A nonconforming PDP that returns an unadvertised `InTenantSubtree`
/// predicate must surface as 403 `permission_denied` on the wire — never a
/// 500 — and the problem body must not leak the predicate or capability
/// names (they live only in a debug log; see `api/rest/error.rs`).
#[tokio::test]
async fn create_group_with_unadvertised_predicate_returns_403_without_leak() {
    let (router, types) =
        build_test_router_with_enforcer(PolicyEnforcer::new(Arc::new(InTenantSubtreeOnlyAuthZ)))
            .await;
    let tenant_id = Uuid::now_v7();
    let type_code = common::unique_rg_code("unadvertised");

    types.add_rg_type(&type_code, true, &[], &[], None);

    let req = json_request(
        "POST",
        "/resource-group/v1/groups",
        Some(serde_json::json!({
            "type": type_code,
            "name": "Unadvertised Predicate Group",
        })),
        tenant_id,
    );

    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    let body = response_body(resp).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "expected 403, got {status}: {body}"
    );
    let rendered = body.to_string();
    assert!(
        rendered.contains("ACCESS_DENIED"),
        "problem body should carry the stable ACCESS_DENIED reason: {body}"
    );
    for leak in ["InTenantSubtree", "tenant_hierarchy", "unadvertised"] {
        assert!(
            !rendered.contains(leak),
            "problem body must not leak PDP internals ({leak}): {body}"
        );
    }
}

/// Fixture with two resource types, two groups, and UUID-looking resource IDs.
async fn membership_filter_fixture() -> (Router, Uuid, Uuid, String, String, String) {
    let (router, types, groups, memberships) = build_shared_router().await;
    let tenant = Uuid::now_v7();
    let ctx = make_ctx(tenant);
    let first_type = rg_type_id!("test.filter._.first.v1~");
    let second_type = rg_type_id!("test.filter._.second.v1~");
    let group_type = rg_type_id!("test.filter._.group.v1~");
    types.add_rg_type(&first_type, true, &[], &[], None);
    types.add_rg_type(&second_type, true, &[], &[], None);
    types.add_rg_type(&group_type, true, &[], &[&first_type, &second_type], None);
    let resource_id = Uuid::now_v7().to_string();
    let mut group_id = Uuid::nil();
    for name in ["matching", "other"] {
        let group = groups
            .create_group(
                &ctx,
                resource_group_sdk::CreateGroupRequest::new(group_type.clone(), name.to_owned()),
                tenant,
            )
            .await
            .unwrap();
        if name == "matching" {
            group_id = group.id;
            memberships
                .add_membership(&ctx, group.id, &first_type, &resource_id)
                .await
                .unwrap();
            memberships
                .add_membership(&ctx, group.id, &first_type, "second")
                .await
                .unwrap();
            memberships
                .add_membership(&ctx, group.id, &second_type, "third")
                .await
                .unwrap();
        } else {
            memberships
                .add_membership(&ctx, group.id, &second_type, "other-group")
                .await
                .unwrap();
        }
    }
    (
        router,
        tenant,
        group_id,
        first_type,
        second_type,
        resource_id,
    )
}

fn encode_query_value(value: &str) -> String {
    use std::fmt::Write;
    value.bytes().fold(String::new(), |mut encoded, byte| {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            write!(encoded, "%{byte:02X}").unwrap();
        }
        encoded
    })
}

async fn filtered_memberships(
    router: &Router,
    tenant: Uuid,
    filter: &str,
    extra: &str,
) -> serde_json::Value {
    let uri = format!(
        "/resource-group/v1/memberships?$filter={}&{extra}",
        encode_query_value(filter)
    );
    let response = router
        .clone()
        .oneshot(json_request("GET", &uri, None, tenant))
        .await
        .unwrap();
    let status = response.status();
    let body = response_body(response).await;
    assert_eq!(status, StatusCode::OK, "filter {filter}: {body}");
    body
}

#[tokio::test]
async fn membership_filters_accept_quoted_uuids_and_resolve_gts_paths() {
    let (router, tenant, group, first, second, resource) = membership_filter_fixture().await;
    for (filter, mut expected) in [
        (
            format!("group_id eq '{group}'"),
            vec![resource.as_str(), "second", "third"],
        ),
        (
            format!("group_id eq {group}"),
            vec![resource.as_str(), "second", "third"],
        ),
        (format!("group_id ne '{group}'"), vec!["other-group"]),
        (
            format!("group_id in ('{group}')"),
            vec![resource.as_str(), "second", "third"],
        ),
        (
            format!("resource_type eq '{first}'"),
            vec![resource.as_str(), "second"],
        ),
        (
            format!("resource_type ne '{first}'"),
            vec!["third", "other-group"],
        ),
        (
            format!("resource_type in ('{first}', '{second}') and group_id eq '{group}'"),
            vec![resource.as_str(), "second", "third"],
        ),
        (
            format!(
                "not (resource_type eq '{second}') and (group_id eq '{group}' or resource_id eq 'absent')"
            ),
            vec![resource.as_str(), "second"],
        ),
        (
            format!("resource_id eq '{resource}'"),
            vec![resource.as_str()],
        ),
    ] {
        let body = filtered_memberships(&router, tenant, &filter, "limit=200").await;
        let mut actual: Vec<_> = body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["resource_id"].as_str().unwrap())
            .collect();
        actual.sort_unstable();
        expected.sort_unstable();
        assert_eq!(actual, expected, "filter: {filter}");
        assert_no_surrogate_ids(&body);
    }
}

#[tokio::test]
async fn membership_filters_reject_invalid_input_with_400() {
    let (router, _, _, _) = build_shared_router().await;
    let tenant = Uuid::now_v7();
    for filter in [
        "group_id eq 'invalid'",
        "group_id eq 123",
        "group_id in ('invalid')",
        "resource_type eq 'invalid'",
        "resource_type eq 'gts.test.filter._.missing.v1~'",
        "resource_type eq 123",
        "resource_type gt 'gts.test.filter._.member.v1~'",
        "contains(resource_type, 'gts.test.filter._.member.v1~')",
        "missing_field eq 'value'",
    ] {
        let uri = format!(
            "/resource-group/v1/memberships?$filter={}",
            encode_query_value(filter)
        );
        let response = router
            .clone()
            .oneshot(json_request("GET", &uri, None, tenant))
            .await
            .unwrap();
        let status = response.status();
        let body = response_body(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "filter {filter}: {body}");
        assert_eq!(
            body["type"],
            gts_uri!("cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~")
        );
    }
}

#[tokio::test]
async fn membership_filter_cursor_preserves_the_original_filter() {
    let (router, tenant, group, first, _, resource) = membership_filter_fixture().await;
    let filter = format!("group_id eq '{group}' and resource_type eq '{first}'");
    let first_page =
        filtered_memberships(&router, tenant, &filter, "$orderby=resource_id&limit=1").await;
    assert_eq!(first_page["items"][0]["resource_id"], resource);
    let cursor = first_page["page_info"]["next_cursor"]
        .as_str()
        .expect("next page");
    let extra = format!("cursor={}&limit=1", encode_query_value(cursor));
    let second_page = filtered_memberships(&router, tenant, &filter, &extra).await;
    assert_eq!(second_page["items"].as_array().unwrap().len(), 1);
    assert_eq!(second_page["items"][0]["resource_id"], "second");

    let changed_filter = format!("group_id eq '{group}'");
    let uri = format!(
        "/resource-group/v1/memberships?$filter={}&{extra}",
        encode_query_value(&changed_filter)
    );
    let response = router
        .oneshot(json_request("GET", &uri, None, tenant))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn group_filters_share_uuid_normalization_and_gts_resolution() {
    let (router, tenant, group, _, _, _) = membership_filter_fixture().await;
    let group_type = rg_type_id!("test.filter._.group.v1~");
    let filter =
        format!("id eq '{group}' and tenant_id eq '{tenant}' and type in ('{group_type}')");
    let uri = format!(
        "/resource-group/v1/groups?$filter={}",
        encode_query_value(&filter)
    );
    let response = router
        .oneshot(json_request("GET", &uri, None, tenant))
        .await
        .unwrap();
    let status = response.status();
    let body = response_body(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["items"][0]["id"], group.to_string());
    assert_eq!(body["items"][0]["type"], group_type);
}

/// Resolved group filters preserve cursor binding to the original public filter.
#[tokio::test]
async fn group_filter_cursor_preserves_the_original_filter() {
    let (router, tenant, _, _, _, _) = membership_filter_fixture().await;
    let group_type = rg_type_id!("test.filter._.group.v1~");
    let filter = format!("type eq '{group_type}' and tenant_id eq '{tenant}'");
    let uri = format!(
        "/resource-group/v1/groups?$filter={}&$orderby=id&limit=1",
        encode_query_value(&filter)
    );
    let response = router
        .clone()
        .oneshot(json_request("GET", &uri, None, tenant))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let first_page = response_body(response).await;
    assert_eq!(first_page["items"].as_array().unwrap().len(), 1);
    let cursor = first_page["page_info"]["next_cursor"]
        .as_str()
        .expect("next page");
    let extra = format!("cursor={}&limit=1", encode_query_value(cursor));
    let uri = format!(
        "/resource-group/v1/groups?$filter={}&{extra}",
        encode_query_value(&filter)
    );
    let response = router
        .clone()
        .oneshot(json_request("GET", &uri, None, tenant))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let second_page = response_body(response).await;
    assert_eq!(second_page["items"].as_array().unwrap().len(), 1);
    assert_ne!(first_page["items"][0]["id"], second_page["items"][0]["id"]);

    let changed_filter = format!("type eq '{group_type}'");
    let uri = format!(
        "/resource-group/v1/groups?$filter={}&{extra}",
        encode_query_value(&changed_filter)
    );
    let response = router
        .oneshot(json_request("GET", &uri, None, tenant))
        .await
        .unwrap();
    let status = response.status();
    let body = response_body(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["type"],
        gts_uri!("cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~")
    );
}

/// Unsupported group operators and unknown membership types are client errors.
#[tokio::test]
async fn typed_filters_reject_unsupported_operators_unknown_types_and_null() {
    let (router, tenant, _, known, _, _) = membership_filter_fixture().await;
    let unknown = rg_type_id!("test.filter._.unknown.v1~");
    let group_type = rg_type_id!("test.filter._.group.v1~");
    for (endpoint, filter) in [
        ("groups", format!("type gt '{group_type}'")),
        ("groups", format!("contains(type, '{group_type}')")),
        ("memberships", format!("resource_type ne '{unknown}'")),
        (
            "memberships",
            format!("resource_type in ('{known}', '{unknown}')"),
        ),
        ("memberships", format!("not (resource_type eq '{unknown}')")),
        ("memberships", "resource_type eq null".to_owned()),
        ("memberships", "group_id eq null".to_owned()),
    ] {
        let uri = format!(
            "/resource-group/v1/{endpoint}?$filter={}",
            encode_query_value(&filter)
        );
        let response = router
            .clone()
            .oneshot(json_request("GET", &uri, None, tenant))
            .await
            .unwrap();
        let status = response.status();
        let body = response_body(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{filter}: {body}");
        assert_eq!(
            body["type"],
            gts_uri!("cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~")
        );
    }
}

/// `$orderby=type` on groups is rejected: the type is a registry reference,
/// not a sortable column of the group table.
#[tokio::test]
async fn groups_orderby_type_rejected_with_400() {
    let (router, tenant, _, _, _, _) = membership_filter_fixture().await;
    for field in ["type", "resource_type"] {
        let response = router
            .clone()
            .oneshot(json_request(
                "GET",
                &format!("/resource-group/v1/groups?$orderby={field}"),
                None,
                tenant,
            ))
            .await
            .unwrap();
        let status = response.status();
        let body = response_body(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "$orderby={field}: {body}");
        assert_eq!(
            body["type"],
            gts_uri!("cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~")
        );
    }
}

/// Types Registry unavailable on create: the canonical mapping answers 503 with
/// a retry hint, and no registry detail reaches the wire.
#[tokio::test]
async fn create_group_types_registry_unavailable_returns_503_with_retry_after() {
    let (router, types, _, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();
    let code = common::unique_rg_code("tr503");
    types.add_rg_type(&code, true, &[], &[], None);
    types.fail_reads();

    let req = json_request(
        "POST",
        "/resource-group/v1/groups",
        Some(serde_json::json!({"type": code, "name": "Unavailable"})),
        tenant_id,
    );
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let retry_header = resp
        .headers()
        .get("retry-after")
        .map(|v| v.to_str().unwrap().to_owned());
    let body = response_body(resp).await;
    assert_eq!(body["status"], 503, "{body}");
    assert_eq!(
        body["type"],
        gts_uri!("cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~"),
        "{body}"
    );
    // The canonical mapping carries the hint either as the `Retry-After`
    // header or as the problem's `retry_after_seconds` field.
    let retry_field = body["context"]["retry_after_seconds"].as_u64();
    assert!(
        retry_header.as_deref() == Some("5") || retry_field == Some(5),
        "expected a 5s retry hint (header {retry_header:?}): {body}"
    );
}

/// A malicious type code on group creation is an inert string: it is either
/// rejected as invalid input or reported as an unknown type, and nothing is
/// persisted.
#[tokio::test]
async fn input_create_group_sql_injection_type_code_persists_nothing() {
    let (router, _, group_svc, _) = build_shared_router().await;
    let tenant_id = Uuid::now_v7();

    let req = json_request(
        "POST",
        "/resource-group/v1/groups",
        Some(serde_json::json!({
            "type": "'; DROP TABLE resource_group; --",
            "name": "Injection"
        })),
        tenant_id,
    );
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND,
        "Expected 400 or 404, got {status}"
    );

    let page = group_svc
        .list_groups(&make_ctx(tenant_id), &toolkit_odata::ODataQuery::default())
        .await
        .expect("the groups table must still exist and be readable");
    assert!(page.items.is_empty(), "nothing must be persisted");
}
