#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Router tests: oneshot requests against [`register_routes`] wired to a
//! fake [`PolicyManagementClientV1`] that returns canned entities and records
//! what it was asked.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use policy_engine_sdk::management::{
    Assignment, AssignmentId, AssignmentSpec, Bundle, BundleId, BundlePatch, BundleVersion,
    Document, DocumentId, DocumentSpec, ManagementError, ManagementResource, NewBundle,
    PolicyManagementClientV1, PublishOperation, ValidationFinding, VersionContent, VersionDetail,
    VersionId, VersionState,
};
use time::OffsetDateTime;
use toolkit::api::OpenApiRegistryImpl;
use toolkit_odata::{ODataQuery, Page, PageInfo};
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

use super::register_routes;
use crate::domain::management::test_support::ctx as make_ctx;

const BUNDLE: Uuid = Uuid::from_u128(0xB1);
const VERSION: Uuid = Uuid::from_u128(0xF1);
const ASSIGNMENT: Uuid = Uuid::from_u128(0xA5);
const TENANT: Uuid = Uuid::from_u128(0x7E_4A47);

fn at() -> OffsetDateTime {
    OffsetDateTime::UNIX_EPOCH
}

fn bundle() -> Bundle {
    Bundle {
        id: BundleId(BUNDLE),
        owner_tenant_id: TENANT,
        name: "b".to_owned(),
        description: String::new(),
        active_version_id: None,
        created_at: at(),
        created_by: TENANT,
        updated_at: at(),
    }
}

fn version(state: VersionState) -> BundleVersion {
    BundleVersion {
        id: VersionId(VERSION),
        bundle_id: BundleId(BUNDLE),
        owner_tenant_id: TENANT,
        ordinal: 1,
        state,
        created_at: at(),
        activated_at: None,
        activated_by: None,
    }
}

fn assignment(enforce: bool) -> Assignment {
    Assignment {
        id: AssignmentId(ASSIGNMENT),
        bundle_id: BundleId(BUNDLE),
        tenant_id: TENANT,
        owner_tenant_id: TENANT,
        enforce,
        created_at: at(),
        created_by: Uuid::from_u128(0x5B),
        updated_at: at(),
        updated_by: Uuid::from_u128(0x5B),
        dormant: false,
    }
}

fn document() -> Document {
    Document {
        id: DocumentId(Uuid::from_u128(0xD0)),
        spec: DocumentSpec {
            name: "main".to_owned(),
            content: "package policy\ndeny := false".to_owned(),
            resource_types: vec!["gts.cf.core.widget.v1~".to_owned()],
            actions: vec!["delete".to_owned()],
        },
    }
}

fn detail() -> VersionDetail {
    VersionDetail {
        version: version(VersionState::Draft),
        documents: vec![document()],
    }
}

/// Canned-answer double recording the last call it received.
#[derive(Default)]
struct FakeClient {
    calls: Mutex<Vec<String>>,
    /// The operations `check_publish` was asked about.
    checks: Mutex<Vec<PublishOperation>>,
    missing: bool,
}

impl FakeClient {
    fn record(&self, call: impl Into<String>) {
        self.calls.lock().unwrap().push(call.into());
    }

    fn last(&self) -> String {
        self.calls
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default()
    }

    fn refuse(&self) -> Result<(), ManagementError> {
        if self.missing {
            return Err(ManagementError::NotFound {
                resource: ManagementResource::Bundle,
                id: BUNDLE,
            });
        }
        Ok(())
    }
}

#[async_trait]
impl PolicyManagementClientV1 for FakeClient {
    async fn create_bundle(
        &self,
        _ctx: &SecurityContext,
        bundle: NewBundle,
    ) -> Result<Bundle, ManagementError> {
        self.record(format!(
            "create_bundle {} {}",
            bundle.name, bundle.description
        ));
        Ok(self::bundle())
    }

    async fn get_bundle(
        &self,
        _ctx: &SecurityContext,
        _id: BundleId,
    ) -> Result<Bundle, ManagementError> {
        self.refuse()?;
        Ok(bundle())
    }

    async fn list_bundles(
        &self,
        _ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<Bundle>, ManagementError> {
        self.record(format!("list_bundles filtered={}", query.has_filter()));
        Ok(Page::new(
            vec![bundle()],
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: 25,
            },
        ))
    }

    async fn update_bundle(
        &self,
        _ctx: &SecurityContext,
        _id: BundleId,
        patch: BundlePatch,
    ) -> Result<Bundle, ManagementError> {
        self.record(format!(
            "update_bundle {:?} {:?}",
            patch.name, patch.description
        ));
        Ok(bundle())
    }

    async fn create_draft_version(
        &self,
        _ctx: &SecurityContext,
        _bundle: BundleId,
        seed_from: Option<VersionId>,
    ) -> Result<BundleVersion, ManagementError> {
        self.record(format!("create_draft_version {seed_from:?}"));
        Ok(version(VersionState::Draft))
    }

    async fn get_version(
        &self,
        _ctx: &SecurityContext,
        _bundle: BundleId,
        _version: VersionId,
    ) -> Result<VersionDetail, ManagementError> {
        Ok(detail())
    }

    async fn list_versions(
        &self,
        _ctx: &SecurityContext,
        _bundle: BundleId,
        _query: &ODataQuery,
    ) -> Result<Page<BundleVersion>, ManagementError> {
        Ok(Page::new(
            vec![version(VersionState::Active)],
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: 25,
            },
        ))
    }

    async fn replace_draft_content(
        &self,
        _ctx: &SecurityContext,
        _bundle: BundleId,
        _version: VersionId,
        content: VersionContent,
    ) -> Result<VersionDetail, ManagementError> {
        self.record(format!("replace_draft_content {:?}", content.documents));
        Ok(detail())
    }

    async fn delete_draft_version(
        &self,
        _ctx: &SecurityContext,
        _bundle: BundleId,
        _version: VersionId,
    ) -> Result<(), ManagementError> {
        self.record("delete_draft_version");
        Ok(())
    }

    async fn validate_version(
        &self,
        _ctx: &SecurityContext,
        _bundle: BundleId,
        _version: VersionId,
    ) -> Result<(), ManagementError> {
        self.record("validate_version");
        Err(ManagementError::ValidationFailed {
            findings: vec![ValidationFinding {
                document_name: Some("main".to_owned()),
                code: "SYNTAX_ERROR".to_owned(),
                message: "bad".to_owned(),
            }],
        })
    }

    async fn check_publish(
        &self,
        _ctx: &SecurityContext,
        operation: PublishOperation,
    ) -> Result<(), ManagementError> {
        self.record("check_publish");
        self.checks.lock().unwrap().push(operation);
        self.refuse()
    }

    async fn activate_version(
        &self,
        _ctx: &SecurityContext,
        _bundle: BundleId,
        _version: VersionId,
    ) -> Result<BundleVersion, ManagementError> {
        self.record("activate_version");
        Ok(version(VersionState::Active))
    }

    async fn assign(
        &self,
        _ctx: &SecurityContext,
        spec: AssignmentSpec,
    ) -> Result<Assignment, ManagementError> {
        self.record(format!("assign enforce={}", spec.enforce));
        Ok(assignment(spec.enforce))
    }

    async fn get_assignment(
        &self,
        _ctx: &SecurityContext,
        _id: AssignmentId,
    ) -> Result<Assignment, ManagementError> {
        Ok(assignment(true))
    }

    async fn list_assignments(
        &self,
        _ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<Assignment>, ManagementError> {
        self.record(format!("list_assignments filtered={}", query.has_filter()));
        Ok(Page::new(
            vec![Assignment {
                dormant: true,
                ..assignment(true)
            }],
            PageInfo {
                next_cursor: None,
                prev_cursor: None,
                limit: 25,
            },
        ))
    }

    async fn update_assignment(
        &self,
        _ctx: &SecurityContext,
        _id: AssignmentId,
        enforce: bool,
    ) -> Result<Assignment, ManagementError> {
        self.record(format!("update_assignment enforce={enforce}"));
        Ok(assignment(enforce))
    }

    async fn unassign(
        &self,
        _ctx: &SecurityContext,
        _id: AssignmentId,
    ) -> Result<(), ManagementError> {
        self.record("unassign");
        Ok(())
    }
}

// -- Harness ------------------------------------------------------------------

fn build_router_with(client: FakeClient) -> (Router, Arc<FakeClient>) {
    let client = Arc::new(client);
    let openapi = OpenApiRegistryImpl::new();
    let router = register_routes(
        Router::new(),
        &openapi,
        Arc::clone(&client) as Arc<dyn PolicyManagementClientV1>,
    );
    (router, client)
}

fn build_router() -> (Router, Arc<FakeClient>) {
    build_router_with(FakeClient::default())
}

fn request(method: &str, uri: &str, body: Option<serde_json::Value>) -> Request<Body> {
    request_ctx(
        method,
        uri,
        body,
        Some(make_ctx(Uuid::from_u128(0xA11CE), TENANT)),
    )
}

fn request_ctx(
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
    ctx: Option<SecurityContext>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let bytes = match body {
        Some(json) => Body::from(serde_json::to_vec(&json).unwrap()),
        None => Body::empty(),
    };
    let mut req = builder.body(bytes).unwrap();
    if let Some(ctx) = ctx {
        req.extensions_mut().insert(ctx);
    }
    req
}

async fn send(
    router: &Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    let resp = router
        .clone()
        .oneshot(request(method, uri, body))
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = to_bytes(resp.into_body(), 1024 * 64).await.unwrap();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

fn bundle_uri() -> String {
    format!("/policy-engine/v1/bundles/{BUNDLE}")
}

fn version_uri() -> String {
    format!("{}/versions/{VERSION}", bundle_uri())
}

fn assignment_uri() -> String {
    format!("/policy-engine/v1/assignments/{ASSIGNMENT}")
}

// -- Bundles ------------------------------------------------------------------

#[tokio::test]
async fn bundles_create_get_list_update() {
    let (router, client) = build_router();

    let (status, headers, body) = send(
        &router,
        "POST",
        "/policy-engine/v1/bundles",
        Some(serde_json::json!({ "name": "b", "description": "d" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        headers.get(header::LOCATION).unwrap().to_str().unwrap(),
        format!("/policy-engine/v1/bundles/{BUNDLE}")
    );
    assert!(headers.get(header::ETAG).is_none(), "no entity tags");
    assert!(body.get("etag").is_none());
    assert_eq!(client.last(), "create_bundle b d");

    let (status, headers, body) = send(&router, "GET", &bundle_uri(), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get(header::ETAG).is_none());
    assert_eq!(body["name"], "b");

    let (status, _, body) = send(&router, "GET", "/policy-engine/v1/bundles", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert!(body["page_info"]["limit"].is_number());

    // A PATCH needs no precondition header.
    let (status, _, _) = send(
        &router,
        "PATCH",
        &bundle_uri(),
        Some(serde_json::json!({ "name": "n", "description": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(client.last(), "update_bundle Some(\"n\") Some(\"x\")");
}

#[tokio::test]
async fn a_missing_bundle_is_404() {
    let (router, _) = build_router_with(FakeClient {
        missing: true,
        ..FakeClient::default()
    });
    let (status, _, body) = send(&router, "GET", &bundle_uri(), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // The handler returns the client's typed error, not a re-mapped one.
    assert_eq!(body["error_code"], "NOT_FOUND");
}

// -- Versions -----------------------------------------------------------------

#[tokio::test]
async fn versions_create_read_list_and_carry_content_only_where_documented() {
    let (router, client) = build_router();

    let (status, headers, body) = send(
        &router,
        "POST",
        &format!("{}/versions", bundle_uri()),
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(headers.get(header::LOCATION).is_some());
    assert_eq!(body["state"], "draft");
    assert_eq!(client.last(), "create_draft_version None");

    // No body at all is an empty draft too; a body is still read as JSON.
    let versions = format!("{}/versions", bundle_uri());
    let (status, _, _) = send(&router, "POST", &versions, None).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(client.last(), "create_draft_version None");
    let untyped = Request::builder()
        .method("POST")
        .uri(&versions)
        .extension(make_ctx(Uuid::from_u128(0xA11CE), TENANT))
        .body(Body::from("{}"))
        .unwrap();
    let resp = router.clone().oneshot(untyped).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

    let (status, _, body) = send(&router, "GET", &version_uri(), None).await;
    assert_eq!(status, StatusCode::OK);
    let doc = &body["documents"][0];
    assert_eq!(doc["content"], "package policy\ndeny := false");
    assert_eq!(doc["resource_types"][0], "gts.cf.core.widget.v1~");
    assert_eq!(doc["actions"][0], "delete");
    assert!(doc.get("backend").is_none() && doc.get("targets").is_none());

    let (status, _, body) = send(&router, "GET", &format!("{}/versions", bundle_uri()), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"][0]["state"], "active");
    assert_eq!(body["page_info"]["limit"], 25);
    assert!(!body.to_string().contains("content"));
    assert!(!body.to_string().contains("digest"));
}

#[tokio::test]
async fn replace_validate_activate_and_delete_a_draft() {
    let (router, client) = build_router();

    let (status, _, body) = send(
        &router,
        "PUT",
        &version_uri(),
        Some(serde_json::json!({ "documents": [
            { "name": "main", "content": "package p\ndeny := true", "resource_types": ["gts.cf.core.*"] }
        ]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["documents"].as_array().unwrap().len(), 1);
    assert!(
        client
            .last()
            .contains("resource_types: [\"gts.cf.core.*\"], actions: []")
    );

    let (status, _, body) = send(
        &router,
        "POST",
        &format!("{}/validate", version_uri()),
        None,
    )
    .await;
    // Invalid content is the problem activation would return.
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error_code"], "VALIDATION_FAILED");
    assert!(body.to_string().contains("SYNTAX_ERROR"), "{body}");
    assert_eq!(client.last(), "validate_version");

    let (status, _, body) = send(
        &router,
        "POST",
        &format!("{}/activate", version_uri()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "active");
    assert_eq!(client.last(), "activate_version");

    let (status, headers, _) = send(&router, "DELETE", &version_uri(), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(headers.get(header::ETAG).is_none());
    assert_eq!(client.last(), "delete_draft_version");
}

#[tokio::test]
async fn there_is_no_deprecate_route() {
    let (router, _) = build_router();
    let (status, _, _) = send(
        &router,
        "POST",
        &format!("{}/deprecate", version_uri()),
        None,
    )
    .await;
    assert!(
        status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED,
        "{status}"
    );
}

// -- Assignments --------------------------------------------------------------

#[tokio::test]
async fn assignments_assign_get_update_unassign() {
    let (router, client) = build_router();

    let (status, headers, body) = send(
        &router,
        "POST",
        "/policy-engine/v1/assignments",
        Some(serde_json::json!({ "bundle_id": BUNDLE, "tenant_id": TENANT })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(headers.get(header::LOCATION).is_some());
    assert_eq!(body["enforce"], true, "enforcing by default");
    assert!(body.get("priority").is_none() && body.get("reaches_barriers").is_none());
    assert_eq!(client.last(), "assign enforce=true");

    let (_, _, body) = send(
        &router,
        "POST",
        "/policy-engine/v1/assignments",
        Some(serde_json::json!({ "bundle_id": BUNDLE, "tenant_id": TENANT, "enforce": false })),
    )
    .await;
    assert_eq!(body["enforce"], false);

    let (status, _, body) = send(&router, "GET", &assignment_uri(), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], ASSIGNMENT.to_string());

    let (status, _, body) = send(
        &router,
        "PATCH",
        &assignment_uri(),
        Some(serde_json::json!({ "enforce": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enforce"], false);
    assert_eq!(client.last(), "update_assignment enforce=false");

    let (status, _, _) = send(&router, "DELETE", &assignment_uri(), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(client.last(), "unassign");
}

#[tokio::test]
async fn assignments_are_listed_a_page_at_a_time_with_their_dormant_flag() {
    let (router, client) = build_router();

    let (status, _, body) = send(&router, "GET", "/policy-engine/v1/assignments", None).await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], ASSIGNMENT.to_string());
    assert_eq!(items[0]["bundle_id"], BUNDLE.to_string());
    assert_eq!(items[0]["tenant_id"], TENANT.to_string());
    assert_eq!(items[0]["dormant"], true);
    assert_eq!(body["page_info"]["limit"], 25);
    assert_eq!(client.last(), "list_assignments filtered=false");
}

#[tokio::test]
async fn bundles_are_filtered_by_name() {
    let (router, client) = build_router();

    let (status, _, body) = send(
        &router,
        "GET",
        "/policy-engine/v1/bundles?$filter=name%20eq%20%27baseline%27",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(client.last(), "list_bundles filtered=true");
}

#[tokio::test]
async fn assignments_are_filtered_by_bundle_and_target_tenant() {
    let (router, client) = build_router();
    let uri = format!(
        "/policy-engine/v1/assignments?$filter=bundle_id%20eq%20{BUNDLE}%20and%20tenant_id%20eq%20{TENANT}"
    );

    let (status, _, _) = send(&router, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(client.last(), "list_assignments filtered=true");
}

#[tokio::test]
async fn an_unsupported_assignment_query_option_is_refused() {
    let (router, client) = build_router();

    let (status, _, body) = send(
        &router,
        "GET",
        "/policy-engine/v1/assignments?$expand=bundle",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["status"], 400, "a problem body: {body}");
    assert_eq!(
        client.last(),
        "",
        "a refused query never reaches the client"
    );
}

// -- Authentication -----------------------------------------------------------

#[tokio::test]
async fn unauthenticated_and_anonymous_requests_are_refused() {
    let (router, _) = build_router();
    let resp = router
        .clone()
        .oneshot(request_ctx("GET", "/policy-engine/v1/bundles", None, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let anonymous = SecurityContext::builder()
        .subject_id(Uuid::nil())
        .subject_tenant_id(Uuid::nil())
        .build()
        .unwrap();
    let resp = router
        .oneshot(request_ctx(
            "GET",
            "/policy-engine/v1/bundles",
            None,
            Some(anonymous),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// -- Dry runs -----------------------------------------------------------------

#[tokio::test]
async fn dry_runs_check_and_perform_nothing() {
    let cases = [
        (
            "POST",
            format!("{}/activate?dry_run=true", version_uri()),
            None,
            PublishOperation::Activate {
                bundle_id: BundleId(BUNDLE),
                version_id: VersionId(VERSION),
            },
        ),
        (
            "POST",
            "/policy-engine/v1/assignments?dry_run=true".to_owned(),
            Some(serde_json::json!({ "bundle_id": BUNDLE, "tenant_id": TENANT, "enforce": false })),
            PublishOperation::Assign(AssignmentSpec::new(BundleId(BUNDLE), TENANT).shadow()),
        ),
        (
            "PATCH",
            format!("{}?dry_run=true", assignment_uri()),
            Some(serde_json::json!({ "enforce": false })),
            PublishOperation::UpdateAssignment {
                assignment_id: AssignmentId(ASSIGNMENT),
            },
        ),
        (
            "DELETE",
            format!("{}?dry_run=true", assignment_uri()),
            None,
            PublishOperation::Unassign {
                assignment_id: AssignmentId(ASSIGNMENT),
            },
        ),
    ];
    for (method, uri, body, operation) in cases {
        let (router, client) = build_router();
        let (status, headers, response) = send(&router, method, &uri, body).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{method} {uri}: {response}");
        assert_eq!(response, serde_json::Value::Null, "{method} {uri}: no body");
        assert!(headers.get(header::LOCATION).is_none(), "{method} {uri}");
        assert_eq!(
            *client.checks.lock().unwrap(),
            [operation],
            "{method} {uri}"
        );
        assert_eq!(
            client.calls.lock().unwrap().len(),
            1,
            "{method} {uri}: only the check ran"
        );
    }
}

#[tokio::test]
async fn without_dry_run_the_operation_runs() {
    let (router, client) = build_router();
    let (status, _, body) = send(
        &router,
        "POST",
        &format!("{}/activate?dry_run=false", version_uri()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "active");
    assert_eq!(client.last(), "activate_version");
    assert!(client.checks.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_failing_dry_run_answers_the_operations_problem() {
    let (router, _) = build_router_with(FakeClient {
        missing: true,
        ..FakeClient::default()
    });
    let (status, headers, body) = send(
        &router,
        "POST",
        &format!("{}/activate?dry_run=true", version_uri()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "application/problem+json"
    );
    assert_eq!(body["error_code"], "NOT_FOUND");
}

#[tokio::test]
async fn a_malformed_dry_run_flag_is_a_problem() {
    let (router, client) = build_router();
    let (status, headers, body) = send(
        &router,
        "POST",
        &format!("{}/activate?dry_run=maybe", version_uri()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "application/problem+json"
    );
    assert_eq!(client.last(), "", "nothing ran");
}
