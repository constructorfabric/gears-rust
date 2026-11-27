//! REST round-trip test for the `AuthZResolverApi` contract projection.
//!
//! Hosts the generated server routes
//! (`register_auth_z_resolver_api_rest_routes`) on an ephemeral axum server and
//! drives them with the generated `AuthZResolverApiRestClient`, asserting the
//! allow/deny decision survives the HTTP boundary. This locks in the wire
//! surface introduced by the REST-contract migration.

#![cfg(all(feature = "rest-client", feature = "rest-server"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use uuid::Uuid;

use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    Action, DenyReason, EvaluationRequestContext, EvaluationResponseContext, Resource, Subject,
};
use authz_resolver_sdk::rest::register_auth_z_resolver_api_rest_routes_with;
use authz_resolver_sdk::{
    AuthZResolverApi, AuthZResolverApiRestClient, EvaluationBatchItem, EvaluationBatchRequest,
    EvaluationRequest, EvaluationResponse, EvaluationsSemantic,
};

use toolkit::api::{OpenApiRegistryImpl, RestRouteOptions};
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::runtime::config::ClientConfig;
use toolkit_security::PlatformSecurityContext;

/// Mock PDP: allows `get`, denies everything else. Mirrors the historical
/// gRPC round-trip fixture.
struct AllowGetResolver;

#[async_trait]
impl AuthZResolverApi for AllowGetResolver {
    async fn evaluate(
        &self,
        _ctx: toolkit_security::PlatformSecurityContext,
        req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: req.action.name == "get",
            context: EvaluationResponseContext::default(),
        })
    }

    async fn evaluate_batch(
        &self,
        ctx: toolkit_security::PlatformSecurityContext,
        req: authz_resolver_sdk::EvaluationBatchRequest,
    ) -> Result<authz_resolver_sdk::EvaluationBatchResponse, CanonicalError> {
        authz_resolver_sdk::evaluate_batch_sequentially(self, ctx, req).await
    }
}

/// Mock PDP that always fails the RPC — models a degraded resolver. Used to
/// pin the error-mapping path across the REST boundary (`Err(CanonicalError)`
/// on the hosted impl → typed `CanonicalError` on the generated client).
struct UnavailableResolver;

#[async_trait]
impl AuthZResolverApi for UnavailableResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Err(CanonicalError::service_unavailable()
            .with_detail("PDP backend is down")
            .create())
    }

    async fn evaluate_batch(
        &self,
        ctx: PlatformSecurityContext,
        req: authz_resolver_sdk::EvaluationBatchRequest,
    ) -> Result<authz_resolver_sdk::EvaluationBatchResponse, CanonicalError> {
        authz_resolver_sdk::evaluate_batch_sequentially(self, ctx, req).await
    }
}

fn sample_request(action: &str) -> EvaluationRequest {
    EvaluationRequest {
        subject: Subject {
            id: Uuid::new_v4(),
            subject_type: Some("user".to_owned()),
            properties: HashMap::new(),
        },
        action: Action {
            name: action.to_owned(),
        },
        resource: Resource {
            resource_type: "gts.cf.core.users.user.v1~".to_owned(),
            id: None,
            properties: HashMap::new(),
        },
        context: EvaluationRequestContext {
            tenant_context: None,
            token_scopes: Vec::new(),
            require_constraints: false,
            capabilities: Vec::new(),
            supported_properties: Vec::new(),
            bearer_token: None,
        },
    }
}

async fn start_test_server(service: Arc<dyn AuthZResolverApi>) -> String {
    start_counting_server(service, &RestRouteOptions::default())
        .await
        .0
}

/// `(path, status)` of every request the test server answered.
type Exchanges = Arc<Mutex<Vec<(String, u16)>>>;

async fn start_counting_server(
    service: Arc<dyn AuthZResolverApi>,
    options: &RestRouteOptions,
) -> (String, Exchanges) {
    // `evaluate` is a platform-plane route: `internal_auth_middleware` normally
    // validates `X-ToolKit-Internal-Token` into a `PlatformSecurityContext`. In
    // tests a per-request layer materializes a marker one for every request so
    // the generated handler's `Extension<PlatformSecurityContext>` extractor
    // succeeds.
    let secctx_layer = axum::middleware::from_fn(
        |mut req: axum::http::Request<axum::body::Body>, next: axum::middleware::Next| async move {
            req.extensions_mut()
                .insert(PlatformSecurityContext::outbound_marker());
            next.run(req).await
        },
    );
    let exchanges = Exchanges::default();
    let recorded = Arc::clone(&exchanges);
    let counting_layer = axum::middleware::from_fn(
        move |req: axum::http::Request<axum::body::Body>, next: axum::middleware::Next| {
            let recorded = Arc::clone(&recorded);
            async move {
                let path = req.uri().path().to_owned();
                let response = next.run(req).await;
                recorded
                    .lock()
                    .unwrap()
                    .push((path, response.status().as_u16()));
                response
            }
        },
    );

    let openapi = OpenApiRegistryImpl::new();
    let app: Router =
        register_auth_z_resolver_api_rest_routes_with(Router::new(), &openapi, service, options)
            .expect("the options name only operations of this contract")
            .layer(secctx_layer)
            .layer(counting_layer);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), exchanges)
}

fn http_client(base_url: &str) -> AuthZResolverApiRestClient {
    AuthZResolverApiRestClient::new(ClientConfig::new(base_url.to_owned()))
        .expect("default toolkit-http client build is infallible in tests")
}

#[tokio::test]
async fn rest_evaluate_allows_get() {
    let base_url = start_test_server(Arc::new(AllowGetResolver)).await;
    let client = http_client(&base_url);

    let resp = AuthZResolverApi::evaluate(
        &client,
        PlatformSecurityContext::outbound_marker(),
        sample_request("get"),
    )
    .await
    .unwrap();
    assert!(resp.decision, "get should be allowed over REST");
}

#[tokio::test]
async fn rest_evaluate_denies_delete() {
    let base_url = start_test_server(Arc::new(AllowGetResolver)).await;
    let client = http_client(&base_url);

    let resp = AuthZResolverApi::evaluate(
        &client,
        PlatformSecurityContext::outbound_marker(),
        sample_request("delete"),
    )
    .await
    .unwrap();
    assert!(!resp.decision, "delete should be denied over REST");
}

#[tokio::test]
async fn rest_evaluate_surfaces_error_variant() {
    // The hosted impl returns `Err(CanonicalError::ServiceUnavailable)`; the
    // generated server encodes it as an RFC 9457 Problem and the generated
    // client must reconstruct the same typed variant after the round trip.
    let base_url = start_test_server(Arc::new(UnavailableResolver)).await;
    let client = http_client(&base_url);

    let err = AuthZResolverApi::evaluate(
        &client,
        PlatformSecurityContext::outbound_marker(),
        sample_request("get"),
    )
    .await
    .expect_err("a failing PDP must surface as an error over REST");

    assert!(
        matches!(err, CanonicalError::ServiceUnavailable { .. }),
        "expected ServiceUnavailable to survive the REST boundary, got: {err:?}"
    );
}

// ── Batch evaluation ─────────────────────────────────────────────────────────

/// A batch of `actions` with the subject and context as defaults.
fn sample_batch(actions: &[&str]) -> EvaluationBatchRequest {
    let base = sample_request("get");
    EvaluationBatchRequest {
        subject: Some(base.subject),
        action: None,
        resource: None,
        context: Some(base.context),
        evaluations: actions
            .iter()
            .map(|action| EvaluationBatchItem {
                action: Some(Action {
                    name: (*action).to_owned(),
                }),
                resource: Some(base.resource.clone()),
                ..Default::default()
            })
            .collect(),
        options: authz_resolver_sdk::EvaluationBatchOptions::default(),
    }
}

fn batch_requests(exchanges: &Exchanges) -> Vec<(String, u16)> {
    exchanges
        .lock()
        .unwrap()
        .iter()
        .filter(|(path, _)| path == "/authz-resolver/v1/evaluations")
        .cloned()
        .collect()
}

#[tokio::test]
async fn rest_evaluate_batch_is_one_request_with_per_item_decisions() {
    let (base_url, exchanges) =
        start_counting_server(Arc::new(AllowGetResolver), &RestRouteOptions::default()).await;
    let client = http_client(&base_url);

    let resp = AuthZResolverApi::evaluate_batch(
        &client,
        PlatformSecurityContext::outbound_marker(),
        sample_batch(&["get", "delete", "get"]),
    )
    .await
    .unwrap();

    let decisions: Vec<bool> = resp.evaluations.iter().map(|r| r.decision).collect();
    assert_eq!(decisions, [true, false, true]);
    assert_eq!(
        batch_requests(&exchanges),
        [("/authz-resolver/v1/evaluations".to_owned(), 200)]
    );
    let requests = { exchanges.lock().unwrap().len() };
    assert_eq!(requests, 1, "no single-evaluation fallback");
}

#[tokio::test]
async fn rest_evaluate_batch_error_is_one_request() {
    let (base_url, exchanges) =
        start_counting_server(Arc::new(UnavailableResolver), &RestRouteOptions::default()).await;
    let client = http_client(&base_url);

    let err = AuthZResolverApi::evaluate_batch(
        &client,
        PlatformSecurityContext::outbound_marker(),
        sample_batch(&["get", "get"]),
    )
    .await
    .expect_err("a failing PDP fails the whole batch");

    assert!(
        matches!(err, CanonicalError::ServiceUnavailable { .. }),
        "expected ServiceUnavailable to survive the REST boundary, got: {err:?}"
    );
    let requests = { exchanges.lock().unwrap().len() };
    assert_eq!(requests, 1, "not retried, no fallback");
}

#[tokio::test]
async fn rest_evaluate_batch_body_limit_is_413_and_leaves_evaluate_alone() {
    let options = RestRouteOptions::default().body_limit("evaluate_batch", 512);
    let (base_url, exchanges) = start_counting_server(Arc::new(AllowGetResolver), &options).await;
    let client = http_client(&base_url);

    AuthZResolverApi::evaluate_batch(
        &client,
        PlatformSecurityContext::outbound_marker(),
        sample_batch(&["get"; 50]),
    )
    .await
    .expect_err("an over-limit batch fails");
    assert_eq!(
        batch_requests(&exchanges),
        [("/authz-resolver/v1/evaluations".to_owned(), 413)]
    );

    // A body of the same size on `/evaluate` is not limited by the batch option.
    let mut big = sample_request("get");
    big.resource.properties.insert(
        "padding".to_owned(),
        serde_json::Value::String("x".repeat(1024)),
    );
    let resp = AuthZResolverApi::evaluate(&client, PlatformSecurityContext::outbound_marker(), big)
        .await
        .unwrap();
    assert!(resp.decision);
}

#[tokio::test]
async fn rest_evaluate_batch_round_trips_options() {
    let (base_url, _) =
        start_counting_server(Arc::new(AllowGetResolver), &RestRouteOptions::default()).await;
    let client = http_client(&base_url);

    let mut batch = sample_batch(&["get", "delete", "get"]);
    batch.options = authz_resolver_sdk::EvaluationBatchOptions::default()
        .with_semantic(EvaluationsSemantic::DenyOnFirstDeny);
    let resp = AuthZResolverApi::evaluate_batch(
        &client,
        PlatformSecurityContext::outbound_marker(),
        batch,
    )
    .await
    .unwrap();
    let decisions: Vec<bool> = resp.evaluations.iter().map(|r| r.decision).collect();
    assert_eq!(
        decisions,
        [true, false],
        "the server saw deny_on_first_deny"
    );
}

/// Permits `get` with a constraint naming the subject's tenant, and denies
/// anything else with an error code: a well-behaved PDP.
struct CodedResolver;

#[async_trait]
impl AuthZResolverApi for CodedResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        req: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        if req.action.name != "get" {
            return Ok(EvaluationResponse::deny(DenyReason::new(
                DenyReason::INSUFFICIENT_PERMISSIONS,
            )));
        }
        let tenant = req.subject.properties["tenant_id"].clone();
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        "owner_tenant_id",
                        [Uuid::parse_str(tenant.as_str().unwrap()).unwrap()],
                    ))],
                }],
                deny_reason: None,
            },
        })
    }

    async fn evaluate_batch(
        &self,
        ctx: PlatformSecurityContext,
        req: EvaluationBatchRequest,
    ) -> Result<authz_resolver_sdk::EvaluationBatchResponse, CanonicalError> {
        authz_resolver_sdk::evaluate_batch_sequentially(self, ctx, req).await
    }
}

fn subject_in(tenant: Uuid) -> Subject {
    Subject {
        id: Uuid::new_v4(),
        subject_type: Some("user".to_owned()),
        properties: HashMap::from([("tenant_id".to_owned(), serde_json::json!(tenant))]),
    }
}

#[tokio::test]
async fn rest_batch_entries_equal_single_rest_evaluations() {
    let (base_url, _) =
        start_counting_server(Arc::new(CodedResolver), &RestRouteOptions::default()).await;
    let client = http_client(&base_url);
    let base = sample_request("get");
    let (tenant_a, tenant_b) = (Uuid::new_v4(), Uuid::new_v4());
    let item = |action: &str| EvaluationBatchItem {
        action: Some(Action {
            name: action.to_owned(),
        }),
        ..Default::default()
    };
    let batch = EvaluationBatchRequest {
        subject: Some(subject_in(tenant_a)),
        action: None,
        resource: Some(base.resource.clone()),
        context: Some(base.context.clone()),
        evaluations: vec![
            item("get"),
            // Another subject: its own tenant reaches the constraint.
            EvaluationBatchItem {
                subject: Some(subject_in(tenant_b)),
                ..item("get")
            },
            item("delete"),
            // A duplicate of the first item.
            item("get"),
        ],
        options: authz_resolver_sdk::EvaluationBatchOptions::default(),
    };
    let (resolved, semantic) = batch.clone().resolve(None).unwrap();

    let response = AuthZResolverApi::evaluate_batch(
        &client,
        PlatformSecurityContext::outbound_marker(),
        batch,
    )
    .await
    .unwrap();
    let results = response.into_results(resolved.len(), semantic).unwrap();

    for (entry, item) in results.iter().zip(resolved) {
        let single =
            AuthZResolverApi::evaluate(&client, PlatformSecurityContext::outbound_marker(), item)
                .await
                .unwrap();
        assert_eq!(
            serde_json::to_value(entry).unwrap(),
            serde_json::to_value(&single).unwrap()
        );
    }
    let decisions: Vec<bool> = results.iter().map(|r| r.decision).collect();
    assert_eq!(decisions, [true, true, false, true]);
}

/// POSTs `body` to `path` over a raw HTTP/1.1 connection and returns the
/// status code, so malformed bodies reach the server unchanged.
async fn raw_post_status(base_url: &str, path: &str, body: &str) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = base_url.trim_start_matches("http://");
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let head = String::from_utf8_lossy(&response);
    head.split_whitespace().nth(1).unwrap().parse().unwrap()
}

#[tokio::test]
async fn rest_batch_rejects_undecodable_and_empty_bodies() {
    let (base_url, _) =
        start_counting_server(Arc::new(CodedResolver), &RestRouteOptions::default()).await;
    let path = "/authz-resolver/v1/evaluations";

    assert_eq!(raw_post_status(&base_url, path, "{not json").await, 400);
    assert_eq!(
        raw_post_status(&base_url, path, "{}").await,
        422,
        "missing evaluations"
    );
    assert_eq!(
        raw_post_status(
            &base_url,
            path,
            r#"{"evaluations":[{}],"options":{"evaluations_semantic":"permit_on_first_permit"}}"#,
        )
        .await,
        422,
        "permit_on_first_permit is deferred"
    );

    // An empty batch decodes and is rejected by the resolution step.
    let client = http_client(&base_url);
    let err = AuthZResolverApi::evaluate_batch(
        &client,
        PlatformSecurityContext::outbound_marker(),
        sample_batch(&[]),
    )
    .await
    .expect_err("an empty batch is invalid");
    assert!(
        matches!(err, CanonicalError::InvalidArgument { .. }),
        "{err:?}"
    );
}
