// Updated: 2026-10-06 by Constructor Tech
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderValue, Request, StatusCode, header};
use time::OffsetDateTime;
use toolkit::api::OpenApiRegistryImpl;
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

use crate::domain::authz::actions;
use crate::domain::ports::metrics::CredStoreMetricsPort;
use crate::domain::ports::plugin::PluginSelector;
use crate::domain::resolver::TenantDirectory;
use crate::domain::secret::model::PutPrecondition;
use crate::domain::secret::repo::SecretRepo;
use crate::domain::secret::service::{ListSettings, Service};
use crate::domain::secret::test_support::{
    FakeDir, FakeMetrics, FakePlugin, FakePluginSelector, FakeSecretRepo, action_deny_enforcer,
    catalog_type_resolver, deny_enforcer, make_ctx, mock_enforcer,
};
use credstore_sdk::{
    CredentialWrite, Fallback, SecretRef, SecretType, SecretValue, SecretWrite, SharingMode,
};

use super::register_routes;
use crate::api::rest::dto::weak_etag;

const MERGE_PATCH: &str = "application/merge-patch+json";

// ── Harness helpers ──────────────────────────────────────────────────────────

fn test_subject() -> Uuid {
    Uuid::from_u128(0xAAAA)
}

fn test_tenant() -> Uuid {
    Uuid::from_u128(0xBBBB)
}

fn test_ctx() -> SecurityContext {
    make_ctx(test_subject(), test_tenant())
}

struct TestHarness {
    router: Router,
    svc: Arc<Service>,
    repo: Arc<FakeSecretRepo>,
}

fn build_harness() -> TestHarness {
    build_harness_with_enforcer(mock_enforcer())
}

fn build_harness_with_enforcer(enforcer: authz_resolver_sdk::PolicyEnforcer) -> TestHarness {
    build_harness_with(FakeDir::single(test_tenant()), enforcer)
}

/// The parent of [`test_tenant`] in the two-tenant chain harness.
fn parent_tenant() -> Uuid {
    Uuid::from_u128(0xCCCC)
}

/// The subject that owns records written in [`parent_tenant`]; distinct from
/// [`test_subject`] so a leaked owner id could not be mistaken for the caller's.
fn parent_subject() -> Uuid {
    Uuid::from_u128(0xDDDD)
}

/// `test_ctx()`'s tenant is a child of [`parent_tenant`]: a `shared` record
/// the parent writes is inherited by the caller.
fn build_harness_with_parent() -> TestHarness {
    build_harness_with(
        FakeDir::new(vec![test_tenant(), parent_tenant()]),
        mock_enforcer(),
    )
}

fn build_harness_with(dir: FakeDir, enforcer: authz_resolver_sdk::PolicyEnforcer) -> TestHarness {
    let repo = Arc::new(FakeSecretRepo::new());
    let plugin = FakePlugin::new();
    let selector = Arc::new(FakePluginSelector::new(plugin));
    let dir = Arc::new(dir);
    let metrics = FakeMetrics::new();
    let svc = Arc::new(Service::new(
        Arc::clone(&repo) as Arc<dyn SecretRepo>,
        dir as Arc<dyn TenantDirectory>,
        enforcer,
        selector as Arc<dyn PluginSelector>,
        catalog_type_resolver(),
        metrics as Arc<dyn CredStoreMetricsPort>,
        ListSettings { max_limit: 200 },
    ));
    let openapi = OpenApiRegistryImpl::new();
    let router = register_routes(Router::new(), &openapi, Arc::clone(&svc));
    TestHarness { router, svc, repo }
}

/// Build a JSON request (`Content-Type: application/json`) with the
/// `SecurityContext` injected as an extension.
fn json_request(
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
    ctx: SecurityContext,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let body_bytes = match body {
        Some(json) => Body::from(serde_json::to_vec(&json).unwrap()),
        None => Body::empty(),
    };
    let mut req = builder.body(body_bytes).unwrap();
    req.extensions_mut().insert(ctx);
    req
}

/// Build a merge-patch request (`Content-Type: application/merge-patch+json`).
fn merge_patch_request(
    uri: &str,
    body: &serde_json::Value,
    if_match: &str,
    ctx: SecurityContext,
) -> Request<Body> {
    let mut req = Request::builder()
        .method("PATCH")
        .uri(uri)
        .header("content-type", MERGE_PATCH)
        .header(axum::http::header::IF_MATCH, if_match)
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap();
    req.extensions_mut().insert(ctx);
    req
}

/// Build a request with an `If-Match` and/or `If-None-Match` header set.
fn json_request_preconditioned(
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
    if_none_match: Option<&str>,
    if_match: Option<&str>,
    ctx: SecurityContext,
) -> Request<Body> {
    let mut req = json_request(method, uri, body, ctx);
    if let Some(v) = if_none_match {
        req.headers_mut().insert(
            axum::http::header::IF_NONE_MATCH,
            axum::http::HeaderValue::from_str(v).expect("ascii"),
        );
    }
    if let Some(v) = if_match {
        req.headers_mut().insert(
            axum::http::header::IF_MATCH,
            axum::http::HeaderValue::from_str(v).expect("ascii"),
        );
    }
    req
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), 1024 * 64).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

/// The machine-readable reason a problem body carries, wherever the canonical
/// category puts it: `context.field_violations[0].reason` (invalid argument),
/// `context.reason` (aborted, permission denied) or
/// `context.violations[0].type` (failed precondition). Categories without a
/// reason (not found, already exists) yield `None`.
fn problem_reason(body: &serde_json::Value) -> Option<&str> {
    let context = &body["context"];
    context["field_violations"][0]["reason"]
        .as_str()
        .or_else(|| context["reason"].as_str())
        .or_else(|| context["violations"][0]["type"].as_str())
}

/// Whether `value` carries an object key named `key` at any depth.
fn has_key_anywhere(value: &serde_json::Value, key: &str) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            map.contains_key(key) || map.values().any(|v| has_key_anywhere(v, key))
        }
        serde_json::Value::Array(items) => items.iter().any(|v| has_key_anywhere(v, key)),
        _ => false,
    }
}

/// The full problem-shape assertion every negative REST test makes (RFC 9457,
/// `docs/toolkit_unified_system/12_unit_testing.md`): the status; `Content-Type`
/// `application/problem+json`; a body with `status` (equal to the code),
/// `title` and `detail`; no `stack`/`trace`/`backtrace` leak; and, when given,
/// the machine-readable reason a client dispatches on. Returns the parsed body
/// for any assertion specific to the test.
async fn assert_problem(
    resp: axum::response::Response,
    expected_status: StatusCode,
    expected_reason: Option<&str>,
) -> serde_json::Value {
    assert_eq!(resp.status(), expected_status, "status");
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(
        content_type.contains("application/problem+json"),
        "Content-Type must be application/problem+json, got {content_type:?}"
    );
    let bytes = to_bytes(resp.into_body(), 1024 * 64).await.unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&bytes).expect("a problem body is a JSON document");
    assert_eq!(
        body["status"],
        serde_json::json!(expected_status.as_u16()),
        "body status: {body}"
    );
    for member in ["title", "detail"] {
        assert!(
            body[member].as_str().is_some_and(|s| !s.is_empty()),
            "`{member}` must be a non-empty string: {body}"
        );
    }
    for leak in ["stack", "trace", "backtrace"] {
        assert!(
            !has_key_anywhere(&body, leak),
            "no `{leak}` may leak into a problem body: {body}"
        );
    }
    if let Some(reason) = expected_reason {
        assert_eq!(problem_reason(&body), Some(reason), "body: {body}");
    }
    body
}

/// The strong validator `GET`/`PUT`/`PATCH` hand out for `(id, version)`.
fn strong_etag(id: Uuid, version: i64) -> String {
    format!("\"{id}.{version}\"")
}

/// A header value from raw bytes, for values `HeaderValue::from_str` refuses
/// to build but a client can still send (obs-text).
fn raw_header(bytes: &[u8]) -> HeaderValue {
    HeaderValue::from_bytes(bytes).expect("a legal header byte string")
}

/// Parse an RFC 3339 timestamp (as the REST layer does).
fn rfc3339(raw: &str) -> OffsetDateTime {
    OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339)
        .expect("valid RFC 3339")
}

/// An instant far enough ahead to stay valid, in its wire spelling.
const FAR_FUTURE: &str = "2999-12-31T23:59:59Z";

/// A `PATCH` with the merge-patch content type and an arbitrary raw body;
/// `if_match` is set only when given.
fn raw_patch_request(uri: &str, if_match: Option<&str>, body: &[u8]) -> Request<Body> {
    let mut builder = Request::builder()
        .method("PATCH")
        .uri(uri)
        .header("content-type", MERGE_PATCH);
    if let Some(v) = if_match {
        builder = builder.header(header::IF_MATCH, v);
    }
    let mut req = builder.body(Body::from(body.to_vec())).unwrap();
    req.extensions_mut().insert(test_ctx());
    req
}

/// A `DELETE` carrying `if_match` when given.
fn delete_request(uri: &str, if_match: Option<HeaderValue>) -> Request<Body> {
    let mut req = Request::builder()
        .method("DELETE")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    if let Some(v) = if_match {
        req.headers_mut().insert(header::IF_MATCH, v);
    }
    req.extensions_mut().insert(test_ctx());
    req
}

/// A `PUT` of a `tenant`-shared generic credential holding `secret`, with
/// `If-Match` set from raw header values (appended, so repeated lines work).
fn put_replace_request(uri: &str, secret: &str, if_match: Vec<HeaderValue>) -> Request<Body> {
    let mut req = json_request(
        "PUT",
        uri,
        Some(serde_json::json!({"sharing": "tenant", "secret": secret})),
        test_ctx(),
    );
    for v in if_match {
        req.headers_mut().append(header::IF_MATCH, v);
    }
    req
}

// ── Seed helpers ─────────────────────────────────────────────────────────────

/// A `generic` write of `value` with the given sharing mode.
fn generic_write(sharing: SharingMode, value: &str) -> CredentialWrite {
    CredentialWrite {
        secret_type: Some(SecretType::generic().into()),
        sharing,
        fallback: Fallback::Inherit,
        expires_at: None,
        secret: SecretWrite::Set(SecretValue::from(value)),
    }
}

/// Create a record as `ctx` through the real write protocol (`Service::put`) -
/// seeding through the same path the router uses (rather than fabricating a
/// row/plugin entry by hand) keeps the row's `value_version` pointing at a real
/// stored version. Returns the strong validator (`id`, `version`) the
/// `If-Match` tests build against.
async fn seed_with(
    harness: &TestHarness,
    ctx: &SecurityContext,
    reference: &str,
    write: CredentialWrite,
) -> (Uuid, i64) {
    let key = SecretRef::new(reference).expect("valid ref");
    let outcome = harness
        .svc
        .put(ctx, &key, write, PutPrecondition::CreateOnly)
        .await
        .expect("seed via the real write protocol");
    (outcome.validator.id, outcome.validator.version)
}

/// Create an active `Tenant`-shared `generic` credential owned by the test
/// caller.
async fn seed_credential(harness: &TestHarness, reference: &str, value: &str) -> (Uuid, i64) {
    seed_with(
        harness,
        &test_ctx(),
        reference,
        generic_write(SharingMode::Tenant, value),
    )
    .await
}

/// Read `reference`'s secret back through the router (`$select=secret`).
async fn read_secret(harness: &TestHarness, reference: &str) -> String {
    let req = json_request(
        "GET",
        &format!("/credstore/v1/credentials/{reference}?%24select=secret"),
        None,
        test_ctx(),
    );
    let resp = harness.router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    body_json(resp).await["secret"]
        .as_str()
        .expect("the secret is selected")
        .to_owned()
}

// ── GET /credentials/{ref} ───────────────────────────────────────────────────

#[tokio::test]
async fn get_credential_existing_returns_200_with_body_and_strong_etag() {
    let h = build_harness();
    let (id, version) = seed_credential(&h, "getkey", "hello-world").await;

    let req = json_request("GET", "/credstore/v1/credentials/getkey", None, test_ctx());
    let resp = h.router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let etag = resp
        .headers()
        .get(axum::http::header::ETAG)
        .expect("ETag")
        .to_str()
        .expect("ascii")
        .to_owned();
    assert_eq!(etag, format!("\"{id}.{version}\""));
    let cc = resp
        .headers()
        .get(axum::http::header::CACHE_CONTROL)
        .expect("Cache-Control")
        .to_str()
        .unwrap();
    assert!(cc.contains("no-store"));

    let body = body_json(resp).await;
    assert_eq!(body["reference"], "getkey");
    assert_eq!(body["sharing"], "tenant");
    assert_eq!(body["status"], "active");
    assert_eq!(body["inheritance"], "own");
    assert!(
        body.get("secret").is_none(),
        "credential must never carry a value"
    );
}

#[tokio::test]
async fn get_credential_missing_returns_404() {
    let h = build_harness();
    let req = json_request("GET", "/credstore/v1/credentials/nokey", None, test_ctx());
    let resp = h.router.oneshot(req).await.expect("router");
    let body = assert_problem(resp, StatusCode::NOT_FOUND, None).await;
    assert!(
        body["type"].as_str().unwrap().contains("not_found"),
        "body: {body}"
    );
}

/// Write a `shared` generic record as the parent tenant's subject: the record
/// the caller's tenant (a child) inherits.
async fn seed_shared_in_parent(h: &TestHarness, reference: &str, value: &str) -> (Uuid, i64) {
    seed_with(
        h,
        &make_ctx(parent_subject(), parent_tenant()),
        reference,
        generic_write(SharingMode::Shared, value),
    )
    .await
}

#[tokio::test]
async fn get_credential_weak_etag_when_only_inherited() {
    let h = build_harness_with_parent();
    let (parent_row_id, parent_row_version) = seed_shared_in_parent(&h, "shared-ref", "v1").await;

    let req = json_request(
        "GET",
        "/credstore/v1/credentials/shared-ref",
        None,
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let etag = resp
        .headers()
        .get(axum::http::header::ETAG)
        .expect("ETag")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        etag.starts_with("W/\""),
        "a caller with no own row gets a weak ETag: {etag}"
    );
    assert_eq!(
        etag,
        weak_etag(
            test_tenant(),
            "shared-ref",
            parent_row_id,
            parent_row_version
        ),
        "the weak ETag is derived from the inheriting tenant and the winning ancestor row"
    );
    assert!(
        !etag.contains(&parent_row_id.to_string()),
        "the weak ETag is opaque, it must not expose the ancestor's record id: {etag}"
    );

    let body = body_json(resp).await;
    assert_eq!(body["reference"], "shared-ref");
    assert_eq!(body["inheritance"], "inherited");
    assert_eq!(body["status"], "none", "the caller holds no row of its own");
    assert_eq!(body["sharing"], "shared");
    for own_row_only in ["version", "updated_at", "owner_id", "fallback", "secret"] {
        assert!(
            body.get(own_row_only).is_none(),
            "`{own_row_only}` is not disclosed for an inherited record: {body}"
        );
    }
    let text = body.to_string();
    assert!(
        !text.contains(&parent_tenant().to_string())
            && !text.contains(&parent_subject().to_string()),
        "no identifier of the ancestor tenant may leak: {body}"
    );
}

#[tokio::test]
async fn get_credential_overriding_an_inherited_record_keeps_the_strong_etag() {
    let h = build_harness_with_parent();
    seed_shared_in_parent(&h, "both", "from-parent").await;
    let (id, version) = seed_credential(&h, "both", "from-child").await;

    let req = json_request("GET", "/credstore/v1/credentials/both", None, test_ctx());
    let resp = h.router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let etag = resp
        .headers()
        .get(axum::http::header::ETAG)
        .expect("ETag")
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        etag,
        strong_etag(id, version),
        "an own row is addressed by its strong validator, whatever an ancestor shares"
    );
    let body = body_json(resp).await;
    assert_eq!(body["inheritance"], "overridden");
    assert_eq!(body["status"], "active");
    assert_eq!(body["version"], version);
}

// ── PUT /credentials/{ref} ───────────────────────────────────────────────────

#[tokio::test]
async fn put_create_only_returns_201_with_location_and_etag() {
    let h = build_harness();
    let req = json_request_preconditioned(
        "PUT",
        "/credstore/v1/credentials/mykey",
        Some(serde_json::json!({
            "type": SecretType::generic().gts_id(),
            "sharing": "tenant",
            "secret": "mysecret"
        })),
        Some("*"),
        None,
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert!(resp.headers().get(axum::http::header::LOCATION).is_some());
    assert!(resp.headers().get(axum::http::header::ETAG).is_some());
}

#[tokio::test]
async fn put_type_is_full_gts_id_only() {
    let api_key = SecretType::from_name("api-key").expect("known");

    let h = build_harness();
    let req = json_request_preconditioned(
        "PUT",
        "/credstore/v1/credentials/byid",
        Some(serde_json::json!({
            "type": api_key.gts_id(),
            "sharing": "tenant",
            "secret": "v"
        })),
        Some("*"),
        None,
        test_ctx(),
    );
    let resp = h.router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::CREATED);

    let get = json_request("GET", "/credstore/v1/credentials/byid", None, test_ctx());
    let resp = h.router.clone().oneshot(get).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["type"], api_key.gts_id());

    // Short name and raw UUID are not GTS type ids → rejected at the transport.
    for bad in ["api-key".to_owned(), api_key.uuid().to_string()] {
        let req = json_request_preconditioned(
            "PUT",
            "/credstore/v1/credentials/badtype",
            Some(serde_json::json!({"type": bad, "sharing": "tenant", "secret": "v"})),
            Some("*"),
            None,
            test_ctx(),
        );
        let resp = h.router.clone().oneshot(req).await.expect("router");
        assert_problem(resp, StatusCode::BAD_REQUEST, Some("UNKNOWN_SECRET_TYPE")).await;
        assert_eq!(h.repo.rows().len(), 1, "{bad}: only `byid` exists");
    }
}

#[tokio::test]
async fn put_unknown_custom_type_returns_400_unknown_secret_type() {
    let h = build_harness();
    let req = json_request_preconditioned(
        "PUT",
        "/credstore/v1/credentials/customkey",
        Some(serde_json::json!({
            "type": gts_id!("cf.core.credstore.credential.v1~acme.connectors.creds.db_password.v1~"),
            "sharing": "tenant",
            "secret": "v"
        })),
        Some("*"),
        None,
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::BAD_REQUEST, Some("UNKNOWN_SECRET_TYPE")).await;
}

#[tokio::test]
async fn put_duplicate_create_only_returns_409() {
    let h = build_harness();
    seed_credential(&h, "dupkey", "v1").await;

    let req = json_request_preconditioned(
        "PUT",
        "/credstore/v1/credentials/dupkey",
        Some(serde_json::json!({
            "type": SecretType::generic().gts_id(),
            "sharing": "tenant",
            "secret": "v2"
        })),
        Some("*"),
        None,
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    let body = assert_problem(resp, StatusCode::CONFLICT, None).await;
    assert!(
        body["type"].as_str().unwrap().contains("already_exists"),
        "body: {body}"
    );
}

#[tokio::test]
async fn put_without_secret_returns_400_secret_required() {
    let h = build_harness();
    let req = json_request_preconditioned(
        "PUT",
        "/credstore/v1/credentials/novalue",
        Some(serde_json::json!({
            "type": SecretType::generic().gts_id(),
            "sharing": "tenant"
        })),
        Some("*"),
        None,
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::BAD_REQUEST, Some("SECRET_REQUIRED")).await;
}

#[tokio::test]
async fn put_neither_precondition_returns_400_precondition_required() {
    let h = build_harness();
    let req = json_request_preconditioned(
        "PUT",
        "/credstore/v1/credentials/nocond",
        Some(serde_json::json!({
            "type": SecretType::generic().gts_id(),
            "sharing": "tenant",
            "secret": "v"
        })),
        None,
        None,
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::BAD_REQUEST, Some("PRECONDITION_REQUIRED")).await;
}

#[tokio::test]
async fn put_both_preconditions_returns_400() {
    let h = build_harness();
    let req = json_request_preconditioned(
        "PUT",
        "/credstore/v1/credentials/bothcond",
        Some(serde_json::json!({
            "type": SecretType::generic().gts_id(),
            "sharing": "tenant",
            "secret": "v"
        })),
        Some("*"),
        Some("*"),
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::BAD_REQUEST, Some("PRECONDITION_REQUIRED")).await;
}

#[tokio::test]
async fn put_if_match_matching_version_replaces_returns_204() {
    let h = build_harness();
    let (id, version) = seed_credential(&h, "ocp", "old").await;

    let req = json_request_preconditioned(
        "PUT",
        "/credstore/v1/credentials/ocp",
        Some(serde_json::json!({"sharing": "tenant", "secret": "new"})),
        None,
        Some(&format!("\"{id}.{version}\"")),
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(resp.headers().get(axum::http::header::ETAG).is_some());
}

#[tokio::test]
async fn put_if_match_stale_version_returns_409() {
    let h = build_harness();
    let (id, _version) = seed_credential(&h, "ocp", "old").await;

    let req = json_request_preconditioned(
        "PUT",
        "/credstore/v1/credentials/ocp",
        Some(serde_json::json!({"sharing": "tenant", "secret": "new"})),
        None,
        Some(&format!("\"{id}.999\"")),
        test_ctx(),
    );
    let resp = h.router.clone().oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::CONFLICT, Some("OPTIMISTIC_LOCK_FAILURE")).await;
    assert_eq!(
        read_secret(&h, "ocp").await,
        "old",
        "the stale write must not land"
    );
}

#[tokio::test]
async fn put_if_match_star_replaces_returns_204() {
    let h = build_harness();
    seed_credential(&h, "putkey", "old-value").await;

    let req = json_request_preconditioned(
        "PUT",
        "/credstore/v1/credentials/putkey",
        Some(serde_json::json!({"sharing": "tenant", "secret": "new-value"})),
        None,
        Some("*"),
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn put_missing_target_with_if_match_never_creates_returns_409() {
    let h = build_harness();
    let req = json_request_preconditioned(
        "PUT",
        "/credstore/v1/credentials/absent",
        Some(serde_json::json!({"sharing": "tenant", "secret": "v"})),
        None,
        Some("*"),
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::CONFLICT, Some("OPTIMISTIC_LOCK_FAILURE")).await;
    assert!(h.repo.rows().is_empty(), "If-Match must never create");
}

#[tokio::test]
async fn put_create_with_explicit_null_secret_returns_201_declared() {
    let h = build_harness();
    let req = json_request_preconditioned(
        "PUT",
        "/credstore/v1/credentials/nullcreate",
        Some(serde_json::json!({
            "type": SecretType::generic().gts_id(),
            "sharing": "tenant",
            "secret": null
        })),
        Some("*"),
        None,
        test_ctx(),
    );
    let resp = h.router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert!(resp.headers().get(axum::http::header::LOCATION).is_some());
    assert!(resp.headers().get(axum::http::header::ETAG).is_some());

    let get = json_request(
        "GET",
        "/credstore/v1/credentials/nullcreate",
        None,
        test_ctx(),
    );
    let resp = h.router.oneshot(get).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["status"], "declared");
    assert!(body.get("secret").is_none());
}

#[tokio::test]
async fn put_if_none_match_not_star_is_400_invalid_if_match() {
    let h = build_harness();
    let strong = strong_etag(Uuid::from_u128(1), 1);
    let cases: Vec<(&str, &str)> = vec![
        (strong.as_str(), "a strong ETag"),
        ("W/\"abc\"", "a weak ETag"),
        ("garbage", "a bare token"),
        ("*, *", "a list"),
    ];
    for (value, what) in cases {
        let req = json_request_preconditioned(
            "PUT",
            "/credstore/v1/credentials/notstar",
            Some(serde_json::json!({"sharing": "tenant", "secret": "v"})),
            Some(value),
            None,
            test_ctx(),
        );
        let resp = h.router.clone().oneshot(req).await.expect("router");
        assert_problem(resp, StatusCode::BAD_REQUEST, Some("INVALID_IF_MATCH")).await;
        assert!(h.repo.rows().is_empty(), "{what}: nothing may be created");
    }
}

#[tokio::test]
async fn put_if_none_match_non_ascii_is_400_invalid_if_match() {
    let h = build_harness();
    let mut req = json_request(
        "PUT",
        "/credstore/v1/credentials/nonascii",
        Some(serde_json::json!({"sharing": "tenant", "secret": "v"})),
        test_ctx(),
    );
    // `*` followed by two obs-text bytes (UTF-8 for an accented letter): a
    // legal header value that is not ASCII, so it can never be `*`.
    req.headers_mut()
        .insert(header::IF_NONE_MATCH, raw_header(b"*\xc3\xa9"));
    let resp = h.router.oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::BAD_REQUEST, Some("INVALID_IF_MATCH")).await;
    assert!(h.repo.rows().is_empty(), "nothing may be created");
}

#[tokio::test]
async fn put_if_match_several_validators_matches_any() {
    let other = strong_etag(Uuid::from_u128(1), 1);
    // The current validator in every position of the list.
    for position in 0..3 {
        let h = build_harness();
        let (id, version) = seed_credential(&h, "multi", "old").await;
        let mut validators = vec![other.clone(), strong_etag(Uuid::from_u128(2), 7)];
        validators.insert(position, strong_etag(id, version));

        let req = put_replace_request(
            "/credstore/v1/credentials/multi",
            "new",
            vec![raw_header(validators.join(", ").as_bytes())],
        );
        let resp = h.router.clone().oneshot(req).await.expect("router");
        assert_eq!(resp.status(), StatusCode::NO_CONTENT, "position {position}");
        assert_eq!(
            resp.headers().get(header::ETAG).unwrap().to_str().unwrap(),
            strong_etag(id, version + 1),
            "position {position}: the replace bumps the version of the same record"
        );
        assert_eq!(read_secret(&h, "multi").await, "new", "position {position}");
        let rows = h.repo.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].version, version + 1);
    }
}

#[tokio::test]
async fn put_if_match_several_validators_none_current_returns_409() {
    let h = build_harness();
    let (id, version) = seed_credential(&h, "multi", "old").await;
    // Right id with a stale version, and a foreign id with the right version.
    let validators = [
        strong_etag(id, version + 1),
        strong_etag(Uuid::from_u128(1), version),
    ];

    let req = put_replace_request(
        "/credstore/v1/credentials/multi",
        "new",
        vec![raw_header(validators.join(", ").as_bytes())],
    );
    let resp = h.router.clone().oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::CONFLICT, Some("OPTIMISTIC_LOCK_FAILURE")).await;
    assert_eq!(
        read_secret(&h, "multi").await,
        "old",
        "the write must not land"
    );
    assert_eq!(h.repo.rows()[0].version, version);
}

#[tokio::test]
async fn put_expires_at_invalid_is_400_invalid_expires_at() {
    let h = build_harness();
    let cases = vec![
        ("tomorrow", "not a timestamp"),
        ("2999-12-31", "a date without a time"),
        ("2999-12-31T23:59:59", "no UTC offset"),
        ("2999-13-40T00:00:00Z", "an impossible calendar date"),
        ("", "empty"),
    ];
    for (value, what) in cases {
        let req = json_request_preconditioned(
            "PUT",
            "/credstore/v1/credentials/badexpiry",
            Some(serde_json::json!({
                "type": SecretType::generic().gts_id(),
                "sharing": "tenant",
                "expires_at": value,
                "secret": "v"
            })),
            Some("*"),
            None,
            test_ctx(),
        );
        let resp = h.router.clone().oneshot(req).await.expect("router");
        assert_problem(resp, StatusCode::BAD_REQUEST, Some("INVALID_EXPIRES_AT")).await;
        assert!(h.repo.rows().is_empty(), "{what}: nothing may be created");
    }
}

#[tokio::test]
async fn put_expires_at_is_stored_and_returned() {
    let bearer = SecretType::from_name("bearer-token").expect("catalog type");
    let h = build_harness();
    let req = json_request_preconditioned(
        "PUT",
        "/credstore/v1/credentials/expiring",
        Some(serde_json::json!({
            "type": bearer.gts_id(),
            "sharing": "tenant",
            "expires_at": FAR_FUTURE,
            "secret": "tok"
        })),
        Some("*"),
        None,
        test_ctx(),
    );
    let resp = h.router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::CREATED);

    let rows = h.repo.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].expires_at, Some(rfc3339(FAR_FUTURE)), "stored");

    let get = json_request(
        "GET",
        "/credstore/v1/credentials/expiring",
        None,
        test_ctx(),
    );
    let resp = h.router.oneshot(get).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["expires_at"], FAR_FUTURE, "returned: {body}");
    assert_eq!(body["type"], bearer.gts_id());
    assert_eq!(
        body["status"], "active",
        "a future expiry is not yet expired"
    );
}

// ── PATCH /credstore/v1/credentials/{ref} ───────────────────────────────────

#[tokio::test]
async fn patch_rotates_secret_returns_204_with_new_etag() {
    let h = build_harness();
    let (id, version) = seed_credential(&h, "rot", "old").await;

    let req = merge_patch_request(
        "/credstore/v1/credentials/rot",
        &serde_json::json!({"secret": "new"}),
        &format!("\"{id}.{version}\""),
        test_ctx(),
    );
    let resp = h.router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let etag = resp
        .headers()
        .get(axum::http::header::ETAG)
        .expect("ETag")
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(etag, format!("\"{id}.{}\"", version + 1));

    let get = json_request(
        "GET",
        "/credstore/v1/credentials/rot?%24select=reference%2Ctype%2Cexpires_at%2Csecret",
        None,
        test_ctx(),
    );
    let resp = h.router.oneshot(get).await.expect("router");
    let body = body_json(resp).await;
    assert_eq!(body["secret"], "new");
}

#[tokio::test]
async fn patch_wrong_content_type_returns_415() {
    let h = build_harness();
    let (id, version) = seed_credential(&h, "ct", "v").await;

    let mut req = Request::builder()
        .method("PATCH")
        .uri("/credstore/v1/credentials/ct")
        .header("content-type", "application/json")
        .header(axum::http::header::IF_MATCH, format!("\"{id}.{version}\""))
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({"secret": "x"})).unwrap(),
        ))
        .unwrap();
    req.extensions_mut().insert(test_ctx());
    let resp = h.router.clone().oneshot(req).await.expect("router");
    assert_problem(
        resp,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        Some("UNSUPPORTED_MEDIA_TYPE"),
    )
    .await;
    assert_eq!(
        read_secret(&h, "ct").await,
        "v",
        "the body must not be applied"
    );
}

#[tokio::test]
async fn patch_missing_content_type_returns_415() {
    let h = build_harness();
    let (id, version) = seed_credential(&h, "ct2", "v").await;

    let mut req = Request::builder()
        .method("PATCH")
        .uri("/credstore/v1/credentials/ct2")
        .header(axum::http::header::IF_MATCH, format!("\"{id}.{version}\""))
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({"secret": "x"})).unwrap(),
        ))
        .unwrap();
    req.extensions_mut().insert(test_ctx());
    let resp = h.router.clone().oneshot(req).await.expect("router");
    assert_problem(
        resp,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        Some("UNSUPPORTED_MEDIA_TYPE"),
    )
    .await;
    assert_eq!(
        read_secret(&h, "ct2").await,
        "v",
        "the body must not be applied"
    );
}

#[tokio::test]
async fn patch_if_none_match_present_returns_400() {
    let h = build_harness();
    let (id, version) = seed_credential(&h, "inm", "v").await;

    let mut req = Request::builder()
        .method("PATCH")
        .uri("/credstore/v1/credentials/inm")
        .header("content-type", MERGE_PATCH)
        .header(axum::http::header::IF_MATCH, format!("\"{id}.{version}\""))
        .header(axum::http::header::IF_NONE_MATCH, "*")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({"secret": "x"})).unwrap(),
        ))
        .unwrap();
    req.extensions_mut().insert(test_ctx());
    let resp = h.router.clone().oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::BAD_REQUEST, Some("PRECONDITION_REQUIRED")).await;
    assert_eq!(
        read_secret(&h, "inm").await,
        "v",
        "the body must not be applied"
    );
}

#[tokio::test]
async fn patch_without_if_match_returns_400() {
    let h = build_harness();
    seed_credential(&h, "noif", "v").await;

    let mut req = Request::builder()
        .method("PATCH")
        .uri("/credstore/v1/credentials/noif")
        .header("content-type", MERGE_PATCH)
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({"secret": "x"})).unwrap(),
        ))
        .unwrap();
    req.extensions_mut().insert(test_ctx());
    let resp = h.router.clone().oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::BAD_REQUEST, Some("IF_MATCH_REQUIRED")).await;
    assert_eq!(
        read_secret(&h, "noif").await,
        "v",
        "the body must not be applied"
    );
}

#[tokio::test]
async fn patch_empty_body_returns_400_empty_patch() {
    let h = build_harness();
    let (id, version) = seed_credential(&h, "empty", "v").await;

    let req = merge_patch_request(
        "/credstore/v1/credentials/empty",
        &serde_json::json!({}),
        &format!("\"{id}.{version}\""),
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::BAD_REQUEST, Some("EMPTY_PATCH")).await;
}

#[tokio::test]
async fn patch_null_sharing_returns_400_null_not_allowed() {
    let h = build_harness();
    let (id, version) = seed_credential(&h, "nullshare", "v").await;

    let req = merge_patch_request(
        "/credstore/v1/credentials/nullshare",
        &serde_json::json!({"sharing": null}),
        &format!("\"{id}.{version}\""),
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::BAD_REQUEST, Some("NULL_NOT_ALLOWED")).await;
}

#[tokio::test]
async fn patch_null_fallback_returns_400_null_not_allowed() {
    let h = build_harness();
    let (id, version) = seed_credential(&h, "nullfb", "v").await;

    let req = merge_patch_request(
        "/credstore/v1/credentials/nullfb",
        &serde_json::json!({"fallback": null}),
        &format!("\"{id}.{version}\""),
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::BAD_REQUEST, Some("NULL_NOT_ALLOWED")).await;
}

#[tokio::test]
async fn patch_null_type_returns_400_null_not_allowed() {
    let h = build_harness();
    let (id, version) = seed_credential(&h, "nulltype", "v").await;

    let req = merge_patch_request(
        "/credstore/v1/credentials/nulltype",
        &serde_json::json!({"type": null}),
        &format!("\"{id}.{version}\""),
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::BAD_REQUEST, Some("NULL_NOT_ALLOWED")).await;
}

#[tokio::test]
async fn patch_secret_null_suppresses_and_secret_read_becomes_404() {
    let h = build_harness();
    let (id, version) = seed_credential(&h, "suppress", "v").await;

    let req = merge_patch_request(
        "/credstore/v1/credentials/suppress",
        &serde_json::json!({"fallback": "none", "secret": null}),
        &format!("\"{id}.{version}\""),
        test_ctx(),
    );
    let resp = h.router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let get_secret = json_request(
        "GET",
        "/credstore/v1/credentials/suppress?%24select=secret",
        None,
        test_ctx(),
    );
    let resp = h.router.clone().oneshot(get_secret).await.expect("router");
    assert_problem(resp, StatusCode::NOT_FOUND, None).await;

    let get_cred = json_request(
        "GET",
        "/credstore/v1/credentials/suppress",
        None,
        test_ctx(),
    );
    let resp = h.router.oneshot(get_cred).await.expect("router");
    let body = body_json(resp).await;
    assert_eq!(body["status"], "declared");
    assert_eq!(body["inheritance"], "suppressed");
}

#[tokio::test]
async fn patch_if_match_several_validators_matches_any() {
    let other = strong_etag(Uuid::from_u128(1), 1);
    // The current validator in every position of the list.
    for position in 0..3 {
        let h = build_harness();
        let (id, version) = seed_credential(&h, "multi", "old").await;
        let mut validators = vec![other.clone(), strong_etag(Uuid::from_u128(2), 7)];
        validators.insert(position, strong_etag(id, version));

        let req = merge_patch_request(
            "/credstore/v1/credentials/multi",
            &serde_json::json!({"secret": "new"}),
            &validators.join(", "),
            test_ctx(),
        );
        let resp = h.router.clone().oneshot(req).await.expect("router");
        assert_eq!(resp.status(), StatusCode::NO_CONTENT, "position {position}");
        assert_eq!(
            resp.headers().get(header::ETAG).unwrap().to_str().unwrap(),
            strong_etag(id, version + 1),
            "position {position}"
        );
        assert_eq!(read_secret(&h, "multi").await, "new", "position {position}");
        assert_eq!(h.repo.rows()[0].version, version + 1);
    }
}

#[tokio::test]
async fn patch_if_match_repeated_header_lines_match_any() {
    let h = build_harness();
    let (id, version) = seed_credential(&h, "lines", "old").await;

    // Two `If-Match` header lines: the first names a foreign record, the
    // second the current one (RFC 9110: the lines are one list).
    let mut req = raw_patch_request(
        "/credstore/v1/credentials/lines",
        None,
        br#"{"secret": "new"}"#,
    );
    req.headers_mut().append(
        header::IF_MATCH,
        raw_header(strong_etag(Uuid::from_u128(1), 1).as_bytes()),
    );
    req.headers_mut().append(
        header::IF_MATCH,
        raw_header(strong_etag(id, version).as_bytes()),
    );
    let resp = h.router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        resp.headers().get(header::ETAG).unwrap().to_str().unwrap(),
        strong_etag(id, version + 1)
    );
    assert_eq!(read_secret(&h, "lines").await, "new");
}

#[tokio::test]
async fn patch_if_match_several_validators_none_current_returns_409() {
    let h = build_harness();
    let (id, version) = seed_credential(&h, "multi", "old").await;
    // Right id with a stale version, and a foreign id with the right version.
    let validators = [
        strong_etag(id, version + 1),
        strong_etag(Uuid::from_u128(1), version),
    ];

    let req = merge_patch_request(
        "/credstore/v1/credentials/multi",
        &serde_json::json!({"secret": "new"}),
        &validators.join(", "),
        test_ctx(),
    );
    let resp = h.router.clone().oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::CONFLICT, Some("OPTIMISTIC_LOCK_FAILURE")).await;
    assert_eq!(
        read_secret(&h, "multi").await,
        "old",
        "the patch must not land"
    );
    assert_eq!(h.repo.rows()[0].version, version);
}

#[tokio::test]
async fn patch_body_not_json_object_is_400_invalid_merge_patch_body() {
    let h = build_harness();
    let (id, version) = seed_credential(&h, "badbody", "v").await;
    let if_match = strong_etag(id, version);

    let cases = vec![
        ("[]", "a JSON array"),
        (r#"[{"secret": "x"}]"#, "an array holding a patch"),
        (r#""just a string""#, "a JSON string"),
        ("42", "a JSON number"),
        ("null", "JSON null"),
        ("{not json", "malformed JSON"),
        ("", "an empty body"),
        (r#"{"bogus": 1}"#, "an unknown member"),
    ];
    for (body, what) in cases {
        let req = raw_patch_request(
            "/credstore/v1/credentials/badbody",
            Some(&if_match),
            body.as_bytes(),
        );
        let resp = h.router.clone().oneshot(req).await.expect("router");
        assert_problem(
            resp,
            StatusCode::BAD_REQUEST,
            Some("INVALID_MERGE_PATCH_BODY"),
        )
        .await;
        assert_eq!(
            h.repo.rows()[0].version,
            version,
            "{what}: nothing may be written"
        );
    }
    assert_eq!(read_secret(&h, "badbody").await, "v");
}

#[tokio::test]
async fn patch_expires_at_invalid_is_400_invalid_expires_at() {
    let h = build_harness();
    let (id, version) = seed_credential(&h, "badexpiry", "v").await;

    let cases = vec![
        ("tomorrow", "not a timestamp"),
        ("2999-12-31", "a date without a time"),
        ("", "empty"),
    ];
    for (value, what) in cases {
        let req = merge_patch_request(
            "/credstore/v1/credentials/badexpiry",
            &serde_json::json!({"expires_at": value}),
            &strong_etag(id, version),
            test_ctx(),
        );
        let resp = h.router.clone().oneshot(req).await.expect("router");
        assert_problem(resp, StatusCode::BAD_REQUEST, Some("INVALID_EXPIRES_AT")).await;
        assert_eq!(h.repo.rows()[0].version, version, "{what}: nothing written");
    }
}

#[tokio::test]
async fn patch_expires_at_set_and_clear() {
    let bearer: credstore_sdk::GtsId = SecretType::from_name("bearer-token")
        .expect("catalog type")
        .into();
    let h = build_harness();
    let (id, version) = seed_with(
        &h,
        &test_ctx(),
        "toggle",
        CredentialWrite {
            secret_type: Some(bearer),
            ..generic_write(SharingMode::Tenant, "tok")
        },
    )
    .await;
    assert_eq!(
        h.repo.rows()[0].expires_at,
        None,
        "seeded without an expiry"
    );

    // Set.
    let req = merge_patch_request(
        "/credstore/v1/credentials/toggle",
        &serde_json::json!({"expires_at": FAR_FUTURE}),
        &strong_etag(id, version),
        test_ctx(),
    );
    let resp = h.router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        resp.headers().get(header::ETAG).unwrap().to_str().unwrap(),
        strong_etag(id, version + 1)
    );
    assert_eq!(
        h.repo.rows()[0].expires_at,
        Some(rfc3339(FAR_FUTURE)),
        "stored"
    );
    let get = json_request("GET", "/credstore/v1/credentials/toggle", None, test_ctx());
    let body = body_json(h.router.clone().oneshot(get).await.expect("router")).await;
    assert_eq!(body["expires_at"], FAR_FUTURE, "returned: {body}");
    assert_eq!(body["version"], version + 1);

    // Clear with an explicit null.
    let req = merge_patch_request(
        "/credstore/v1/credentials/toggle",
        &serde_json::json!({"expires_at": null}),
        &strong_etag(id, version + 1),
        test_ctx(),
    );
    let resp = h.router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        resp.headers().get(header::ETAG).unwrap().to_str().unwrap(),
        strong_etag(id, version + 2)
    );
    assert_eq!(h.repo.rows()[0].expires_at, None, "cleared in storage");
    let get = json_request("GET", "/credstore/v1/credentials/toggle", None, test_ctx());
    let body = body_json(h.router.clone().oneshot(get).await.expect("router")).await;
    assert!(
        body.get("expires_at").is_none(),
        "a cleared expiry is absent from the body: {body}"
    );
    assert_eq!(body["version"], version + 2);
    assert_eq!(
        read_secret(&h, "toggle").await,
        "tok",
        "a metadata patch leaves the value alone"
    );
}

#[tokio::test]
async fn patch_sets_type_is_rejected_type_immutable() {
    let api_key = SecretType::from_name("api-key").expect("catalog type");
    let h = build_harness();
    let (id, version) = seed_credential(&h, "typed", "v").await;
    let before = h.repo.rows()[0].clone();

    let req = merge_patch_request(
        "/credstore/v1/credentials/typed",
        &serde_json::json!({"type": api_key.gts_id()}),
        &strong_etag(id, version),
        test_ctx(),
    );
    let resp = h.router.clone().oneshot(req).await.expect("router");
    let body = assert_problem(resp, StatusCode::CONFLICT, Some("TYPE_IMMUTABLE")).await;
    assert!(
        body["type"].as_str().unwrap().contains("aborted"),
        "a type conflict is canonical ABORTED, like a version conflict: {body}"
    );

    let after = h.repo.rows()[0].clone();
    assert_eq!(after.secret_type_uuid, before.secret_type_uuid);
    assert_eq!(after.version, version, "nothing was written");
}

#[tokio::test]
async fn patch_sets_sharing_updates_the_record() {
    let h = build_harness();
    let (id, version) = seed_credential(&h, "share", "v").await;
    let before = h.repo.rows()[0].clone();
    assert_eq!(before.sharing, SharingMode::Tenant);

    let req = merge_patch_request(
        "/credstore/v1/credentials/share",
        &serde_json::json!({"sharing": "shared"}),
        &strong_etag(id, version),
        test_ctx(),
    );
    let resp = h.router.clone().oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        resp.headers().get(header::ETAG).unwrap().to_str().unwrap(),
        strong_etag(id, version + 1)
    );

    let after = h.repo.rows()[0].clone();
    assert_eq!(after.sharing, SharingMode::Shared, "stored");
    assert_eq!(after.version, version + 1);
    assert_eq!(after.id, id, "the same record");
    assert_eq!(after.value_version, before.value_version, "value untouched");
    assert_eq!(after.fallback, before.fallback);
    assert_eq!(after.secret_type_uuid, before.secret_type_uuid);
    assert_eq!(after.expires_at, before.expires_at);

    let get = json_request("GET", "/credstore/v1/credentials/share", None, test_ctx());
    let body = body_json(h.router.clone().oneshot(get).await.expect("router")).await;
    assert_eq!(body["sharing"], "shared", "returned: {body}");
    assert_eq!(body["version"], version + 1);
    assert_eq!(read_secret(&h, "share").await, "v");
}

#[tokio::test]
async fn patch_sharing_across_the_private_boundary_is_400_unsupported_transition() {
    // `private` records live in a different key class than `tenant`/`shared`
    // ones, so a patch cannot move a record across that line in either
    // direction.
    let cases = vec![
        (SharingMode::Tenant, "private"),
        (SharingMode::Shared, "private"),
        (SharingMode::Private, "tenant"),
        (SharingMode::Private, "shared"),
    ];
    for (from, to) in cases {
        let h = build_harness();
        let (id, version) = seed_with(&h, &test_ctx(), "boundary", generic_write(from, "v")).await;

        let req = merge_patch_request(
            "/credstore/v1/credentials/boundary",
            &serde_json::json!({"sharing": to}),
            &strong_etag(id, version),
            test_ctx(),
        );
        let resp = h.router.clone().oneshot(req).await.expect("router");
        let body = assert_problem(
            resp,
            StatusCode::BAD_REQUEST,
            Some("UNSUPPORTED_TRANSITION"),
        )
        .await;
        assert!(
            body["type"]
                .as_str()
                .unwrap()
                .contains("failed_precondition"),
            "{from:?} -> {to}: {body}"
        );
        let after = h.repo.rows()[0].clone();
        assert_eq!(after.sharing, from, "{from:?} -> {to}: unchanged");
        assert_eq!(after.version, version, "{from:?} -> {to}: nothing written");
    }
}

// ── DELETE /credentials/{ref} ────────────────────────────────────────────────

#[tokio::test]
async fn delete_with_if_match_star_returns_204() {
    let h = build_harness();
    seed_credential(&h, "delkey", "bye").await;

    let mut req = Request::builder()
        .method("DELETE")
        .uri("/credstore/v1/credentials/delkey")
        .header(axum::http::header::IF_MATCH, "*")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(test_ctx());
    let resp = h.router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn delete_without_if_match_returns_400() {
    let h = build_harness();
    seed_credential(&h, "delkey2", "bye").await;

    let req = delete_request("/credstore/v1/credentials/delkey2", None);
    let resp = h.router.clone().oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::BAD_REQUEST, Some("IF_MATCH_REQUIRED")).await;
    assert_eq!(h.repo.rows().len(), 1, "the record must survive");
}

#[tokio::test]
async fn delete_missing_returns_404() {
    let h = build_harness();
    let mut req = Request::builder()
        .method("DELETE")
        .uri("/credstore/v1/credentials/ghost")
        .header(axum::http::header::IF_MATCH, "*")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(test_ctx());
    let resp = h.router.oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::NOT_FOUND, None).await;
}

// ── If-Match parsing shared by PATCH and DELETE (and PUT's replace) ─────────

#[tokio::test]
async fn if_match_malformed_is_400_invalid_if_match() {
    let h = build_harness();
    let (id, version) = seed_credential(&h, "malformed", "v").await;
    let current = strong_etag(id, version);
    let uri = "/credstore/v1/credentials/malformed";

    let list_with_garbage = format!("{current}, garbage");
    let weak_current = format!("W/{current}");
    let cases: Vec<(&str, &str)> = vec![
        (
            weak_current.as_str(),
            "a weak validator naming the current record",
        ),
        (r#"W/"x""#, "a weak validator"),
        ("garbage", "a bare token"),
        (r#""not-a-uuid.1""#, "a quoted id that is not a UUID"),
        (
            r#""00000000-0000-0000-0000-000000000001""#,
            "a quoted id without a version",
        ),
        (
            r#""00000000-0000-0000-0000-000000000001.x""#,
            "a non-numeric version",
        ),
        (r#""""#, "an empty quoted tag"),
        ("", "an empty value"),
        (",", "an empty list"),
        (
            list_with_garbage.as_str(),
            "one bad member beside the current validator",
        ),
    ];
    for (value, what) in cases {
        let patch = raw_patch_request(uri, Some(value), br#"{"secret": "x"}"#);
        let resp = h.router.clone().oneshot(patch).await.expect("router");
        assert_problem(resp, StatusCode::BAD_REQUEST, Some("INVALID_IF_MATCH")).await;

        let delete = delete_request(uri, Some(raw_header(value.as_bytes())));
        let resp = h.router.clone().oneshot(delete).await.expect("router");
        assert_problem(resp, StatusCode::BAD_REQUEST, Some("INVALID_IF_MATCH")).await;

        let rows = h.repo.rows();
        assert_eq!(rows.len(), 1, "{what}: the record must survive");
        assert_eq!(rows[0].version, version, "{what}: nothing may be written");
    }
    assert_eq!(read_secret(&h, "malformed").await, "v");
}

#[tokio::test]
async fn if_match_non_ascii_is_400_invalid_if_match() {
    let h = build_harness();
    let (id, version) = seed_credential(&h, "nonascii", "v").await;
    let uri = "/credstore/v1/credentials/nonascii";
    // Obs-text bytes: a legal header value that is not ASCII. Even beside the
    // current validator the whole header is refused.
    let mut value = strong_etag(id, version).into_bytes();
    value.extend_from_slice(b", \"\xc3\xa9\"");

    let mut patch = raw_patch_request(uri, None, br#"{"secret": "x"}"#);
    patch
        .headers_mut()
        .insert(header::IF_MATCH, raw_header(&value));
    let resp = h.router.clone().oneshot(patch).await.expect("router");
    let body = assert_problem(resp, StatusCode::BAD_REQUEST, Some("INVALID_IF_MATCH")).await;
    assert!(
        body.to_string().contains("ASCII"),
        "the problem names the non-ASCII header, not a malformed tag: {body}"
    );

    let delete = delete_request(uri, Some(raw_header(&value)));
    let resp = h.router.clone().oneshot(delete).await.expect("router");
    let body = assert_problem(resp, StatusCode::BAD_REQUEST, Some("INVALID_IF_MATCH")).await;
    assert!(body.to_string().contains("ASCII"), "{body}");

    let put = put_replace_request(uri, "x", vec![raw_header(&value)]);
    let resp = h.router.clone().oneshot(put).await.expect("router");
    let body = assert_problem(resp, StatusCode::BAD_REQUEST, Some("INVALID_IF_MATCH")).await;
    assert!(body.to_string().contains("ASCII"), "{body}");

    let rows = h.repo.rows();
    assert_eq!(rows.len(), 1, "the record must survive");
    assert_eq!(rows[0].version, version, "nothing may be written");
    assert_eq!(read_secret(&h, "nonascii").await, "v");
}

// ── GET /credentials/{ref}?$select=... (ADR-0004, "Read actions follow the projection") ───────────────
// The withdrawn `GET /credentials/{ref}/secret` is superseded by
// `$select=reference,type,expires_at,secret` on the point read.

#[tokio::test]
async fn get_credential_select_secret_returns_the_secret() {
    let h = build_harness();
    seed_credential(&h, "sec", "topsecret").await;

    let req = json_request(
        "GET",
        "/credstore/v1/credentials/sec?%24select=reference%2Ctype%2Cexpires_at%2Csecret",
        None,
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let cc = resp
        .headers()
        .get(axum::http::header::CACHE_CONTROL)
        .expect("Cache-Control")
        .to_str()
        .unwrap();
    assert!(cc.contains("no-store"));
    let body = body_json(resp).await;
    assert_eq!(body["secret"], "topsecret");
    assert_eq!(body["reference"], "sec");
    assert!(
        body.get("sharing").is_none(),
        "an administrative field must not ride along with a secret-only projection: {body}"
    );
}

#[tokio::test]
async fn get_credential_select_secret_missing_returns_404() {
    let h = build_harness();
    let req = json_request(
        "GET",
        "/credstore/v1/credentials/nosec?%24select=secret",
        None,
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::NOT_FOUND, None).await;
}

#[tokio::test]
async fn get_credential_select_projects_only_the_requested_fields() {
    let h = build_harness();
    seed_credential(&h, "proj", "v").await;

    let req = json_request(
        "GET",
        "/credstore/v1/credentials/proj?%24select=reference%2Ctype",
        None,
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let mut keys: Vec<&str> = body
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["reference", "type"]);
}

#[tokio::test]
async fn get_credential_select_unknown_field_returns_400() {
    let h = build_harness();
    seed_credential(&h, "badselect", "v").await;

    let req = json_request(
        "GET",
        "/credstore/v1/credentials/badselect?%24select=bogus",
        None,
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::BAD_REQUEST, Some("INVALID_SELECT")).await;
}

#[tokio::test]
async fn get_credential_fully_denied_by_the_pdp_returns_403_even_for_a_missing_reference() {
    // Reads authorize first: a flat denial is 403 whether or not the
    // reference exists, for the record and for the value alike.
    let h = build_harness_with_enforcer(deny_enforcer());
    for uri in [
        "/credstore/v1/credentials/nothing",
        "/credstore/v1/credentials/nothing?%24select=secret",
    ] {
        let req = json_request("GET", uri, None, test_ctx());
        let resp = h.router.clone().oneshot(req).await.expect("router");
        assert_problem(resp, StatusCode::FORBIDDEN, None).await;
    }
}

#[tokio::test]
async fn get_credential_select_secret_without_read_secret_grant_returns_404() {
    // A caller holding `read` but not `read_secret`: the unselected point
    // read (metadata only) still succeeds, but naming `secret` in `$select`
    // is the canonical 404 -- not a 403, and not a 200 missing the field.
    let denied_type = SecretType::generic().gts_id().to_owned();
    let (enforcer, _resolver) = action_deny_enforcer(denied_type, actions::READ_SECRET);
    let h = build_harness_with_enforcer(enforcer);
    seed_credential(&h, "noreadsecret", "v").await;

    let plain = json_request(
        "GET",
        "/credstore/v1/credentials/noreadsecret",
        None,
        test_ctx(),
    );
    let resp = h.router.clone().oneshot(plain).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK, "read alone must still work");

    let with_secret = json_request(
        "GET",
        "/credstore/v1/credentials/noreadsecret?%24select=secret",
        None,
        test_ctx(),
    );
    let resp = h.router.oneshot(with_secret).await.expect("router");
    assert_problem(resp, StatusCode::NOT_FOUND, None).await;
}

#[tokio::test]
async fn secret_route_is_withdrawn_returns_404_from_the_router() {
    let h = build_harness();
    seed_credential(&h, "goneroute", "v").await;

    let req = json_request(
        "GET",
        "/credstore/v1/credentials/goneroute/secret",
        None,
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    // The router's own fallback, not a handler's error: no `Problem` body to
    // assert on (the gateway layer renders those).
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "no route is registered for .../secret any more"
    );
}

// ── misc ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn invalid_ref_returns_400() {
    let h = build_harness();
    let req = json_request(
        "GET",
        "/credstore/v1/credentials/has%3Acolon",
        None,
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::BAD_REQUEST, Some("INVALID_SECRET_REF")).await;
}

#[tokio::test]
async fn invalid_ref_on_delete_returns_400() {
    let h = build_harness();
    let mut req = Request::builder()
        .method("DELETE")
        .uri("/credstore/v1/credentials/has%3Acolon")
        .header(axum::http::header::IF_MATCH, "*")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(test_ctx());
    let resp = h.router.oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::BAD_REQUEST, Some("INVALID_SECRET_REF")).await;
}

// ── GET /credentials (collection read, ADR-0005/ADR-0004) ───────────────────

fn list_uri(query: &str) -> String {
    if query.is_empty() {
        "/credstore/v1/credentials".to_owned()
    } else {
        format!("/credstore/v1/credentials?{query}")
    }
}

#[tokio::test]
async fn list_credentials_smoke_returns_200_json() {
    let h = build_harness();
    let req = json_request("GET", &list_uri(""), None, test_ctx());
    let resp = h.router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let content_type = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(content_type.starts_with("application/json"));
    assert_eq!(
        resp.headers().get(axum::http::header::CACHE_CONTROL),
        Some(&axum::http::HeaderValue::from_static("no-store"))
    );
    let body = body_json(resp).await;
    assert!(body["items"].as_array().is_some());
    assert!(body["page_info"].is_object());
}

#[tokio::test]
async fn list_credentials_without_select_returns_the_full_credential_shape() {
    let h = build_harness();
    seed_credential(&h, "list-full", "value").await;

    let req = json_request("GET", &list_uri(""), None, test_ctx());
    let resp = h.router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let items = body["items"].as_array().expect("items array");
    let item = items
        .iter()
        .find(|i| i["reference"] == "list-full")
        .expect("seeded item present");

    let mut keys: Vec<&str> = item
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "fallback",
            "inheritance",
            "owner_id",
            "reference",
            "sharing",
            "status",
            "type",
            "updated_at",
            "version",
        ],
        "no `secret` key unless selected (no expiry set on this fixture, so `expires_at` is \
         skipped too)"
    );
    assert_eq!(item["status"], "active");
    assert_eq!(item["inheritance"], "own");
}

#[tokio::test]
async fn list_credentials_select_projects_only_the_requested_fields() {
    let h = build_harness();
    seed_credential(&h, "list-projected", "value").await;

    let req = json_request(
        "GET",
        &list_uri("%24select=reference%2Ctype"),
        None,
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let items = body["items"].as_array().expect("items array");
    let item = items
        .iter()
        .find(|i| i["reference"] == "list-projected")
        .expect("seeded item present");
    let mut keys: Vec<&str> = item
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["reference", "type"]);
}

#[tokio::test]
async fn list_credentials_selecting_secret_returns_the_secret() {
    let h = build_harness();
    seed_credential(&h, "list-value", "top-secret").await;

    let req = json_request(
        "GET",
        &list_uri("%24select=reference%2Csecret&%24filter=reference%20eq%20%27list-value%27"),
        None,
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["reference"], "list-value");
    assert_eq!(items[0]["secret"], "top-secret");
    assert!(body["page_info"]["next_cursor"].is_null());
}

#[tokio::test]
async fn list_credentials_selecting_secret_paginates_and_is_not_cached() {
    let h = build_harness();
    for name in ["page-a", "page-b", "page-c"] {
        seed_credential(&h, name, &format!("value-{name}")).await;
    }

    let first = h
        .router
        .clone()
        .oneshot(json_request(
            "GET",
            &list_uri("%24select=reference%2Csecret&limit=2"),
            None,
            test_ctx(),
        ))
        .await
        .expect("router");
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        first.headers().get(axum::http::header::CACHE_CONTROL),
        Some(&axum::http::HeaderValue::from_static("no-store"))
    );
    let body = body_json(first).await;
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["secret"], "value-page-a");
    assert_eq!(items[1]["secret"], "value-page-b");
    let cursor = body["page_info"]["next_cursor"]
        .as_str()
        .expect("next cursor")
        .to_owned();

    let second = h
        .router
        .oneshot(json_request(
            "GET",
            &list_uri(&format!(
                "%24select=reference%2Csecret&limit=2&cursor={cursor}"
            )),
            None,
            test_ctx(),
        ))
        .await
        .expect("router");
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        second.headers().get(axum::http::header::CACHE_CONTROL),
        Some(&axum::http::HeaderValue::from_static("no-store"))
    );
    let body = body_json(second).await;
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["secret"], "value-page-c");
    assert!(body["page_info"]["next_cursor"].is_null());
}

#[tokio::test]
async fn list_credentials_unsupported_orderby_field_returns_400() {
    let h = build_harness();
    let req = json_request("GET", &list_uri("%24orderby=updated_at"), None, test_ctx());
    let resp = h.router.oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::BAD_REQUEST, None).await;
}

// ── expiry applies to the secret, not to the record ─────────────────────────

/// Seed `reference` and force the stored row past its expiry.
async fn seed_expired_credential(harness: &TestHarness, reference: &str) -> (Uuid, i64) {
    let (id, version) = seed_credential(harness, reference, "stale").await;
    harness.repo.force_expire(id);
    (id, version)
}

#[tokio::test]
async fn get_with_the_secret_of_an_expired_credential_returns_409_secret_expired() {
    let h = build_harness();
    seed_expired_credential(&h, "exp").await;

    for select in [
        "%24select=reference%2Ctype%2Cexpires_at%2Csecret",
        "%24select=status%2Csecret",
    ] {
        let req = json_request(
            "GET",
            &format!("/credstore/v1/credentials/exp?{select}"),
            None,
            test_ctx(),
        );
        let resp = h.router.clone().oneshot(req).await.expect("router");
        let body = assert_problem(resp, StatusCode::CONFLICT, Some("SECRET_EXPIRED")).await;
        assert!(
            body["type"]
                .as_str()
                .unwrap()
                .contains("failed_precondition"),
            "canonical category stays FAILED_PRECONDITION: {body}"
        );
        assert!(!body.to_string().contains("stale"), "no secret in the body");
    }
}

#[tokio::test]
async fn get_without_the_secret_of_an_expired_credential_shows_status_expired() {
    let h = build_harness();
    let (id, version) = seed_expired_credential(&h, "exp").await;

    let req = json_request("GET", "/credstore/v1/credentials/exp", None, test_ctx());
    let resp = h.router.oneshot(req).await.expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let etag = resp
        .headers()
        .get(axum::http::header::ETAG)
        .expect("ETag")
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(etag, format!("\"{id}.{version}\""), "the normal validator");
    let body = body_json(resp).await;
    assert_eq!(body["status"], "expired", "body: {body}");
    assert!(body.get("secret").is_none());
}

#[tokio::test]
async fn list_shows_status_expired_and_selecting_secret_omits_only_the_secret() {
    let h = build_harness();
    seed_credential(&h, "live", "live-value").await;
    seed_expired_credential(&h, "old").await;

    let resp = h
        .router
        .clone()
        .oneshot(json_request(
            "GET",
            "/credstore/v1/credentials",
            None,
            test_ctx(),
        ))
        .await
        .expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let statuses: Vec<(String, String)> = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| {
            (
                i["reference"].as_str().unwrap().to_owned(),
                i["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        statuses,
        vec![
            ("live".to_owned(), "active".to_owned()),
            ("old".to_owned(), "expired".to_owned())
        ],
        "body: {body}"
    );

    let resp = h
        .router
        .clone()
        .oneshot(json_request(
            "GET",
            "/credstore/v1/credentials?%24select=reference%2Cexpires_at%2Csecret&%24filter=reference%20in%20(%27live%27%2C%27old%27)",
            None,
            test_ctx(),
        ))
        .await
        .expect("router");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 2, "body: {body}");
    assert_eq!(items[0]["secret"], "live-value");
    assert_eq!(items[1]["reference"], "old");
    assert!(items[1].get("secret").is_none(), "body: {body}");

    // On the collection `secret` may not ride with an administrative field.
    let resp = h
        .router
        .oneshot(json_request(
            "GET",
            "/credstore/v1/credentials?%24select=reference%2Cstatus%2Csecret",
            None,
            test_ctx(),
        ))
        .await
        .expect("router");
    assert_problem(resp, StatusCode::BAD_REQUEST, Some("SECRET_SELECT_FIELDS")).await;
}

#[tokio::test]
async fn put_create_only_over_an_expired_credential_returns_409_already_exists() {
    let h = build_harness();
    let (id, _) = seed_expired_credential(&h, "exp").await;

    let req = json_request_preconditioned(
        "PUT",
        "/credstore/v1/credentials/exp",
        Some(serde_json::json!({
            "type": SecretType::generic().gts_id(),
            "sharing": "tenant",
            "secret": "fresh"
        })),
        Some("*"),
        None,
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    let body = assert_problem(resp, StatusCode::CONFLICT, None).await;
    assert!(
        body["type"].as_str().unwrap().contains("already_exists"),
        "body: {body}"
    );
    assert_eq!(h.repo.rows()[0].id, id, "the expired record is untouched");
}

#[tokio::test]
async fn expired_credential_without_read_secret_is_the_usual_404() {
    let h = build_harness_with_enforcer(
        action_deny_enforcer(
            SecretType::generic().gts_id().to_owned(),
            actions::READ_SECRET,
        )
        .0,
    );
    seed_expired_credential(&h, "exp").await;

    let req = json_request(
        "GET",
        "/credstore/v1/credentials/exp?%24select=reference%2Csecret",
        None,
        test_ctx(),
    );
    let resp = h.router.oneshot(req).await.expect("router");
    assert_problem(resp, StatusCode::NOT_FOUND, None).await;
}
